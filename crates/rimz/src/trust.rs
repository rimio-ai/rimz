//! Project trust — the executable-surface hash and the grant record.
//!
//! Project config at `<project_root>/.rimz/config.toml` is inert until the
//! workspace is trusted. A trust grant pins a SHA-256 of every command-running
//! field; every later [`status`] call re-hashes the live config and demotes
//! the state to `stale` when the hash drifts. That re-hash is the
//! "auto-revoke" half of the contract — no separate sweep is required.
//!
//! Adding a new command-running field that isn't projected by
//! [`ExecutableSurface`] is a CI invariant violation per
//! [`docs/guide/security.md`](../../docs/guide/security.md).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::agents::PermissionMode;
use crate::config::{CheckOn, ConfigFileDiagnosis, Team};
use crate::ids::WorkspaceId;
use crate::store::atomic::{self, write_bytes_atomically};
use crate::store::paths::config_home;

const CONFIG_REL: &str = ".rimz/config.toml";
const PROJECTS_SUBDIR: [&str; 2] = ["rimz", "projects"];
const TRUST_FILE: &str = "trust.toml";
const BIRTH_PROMPT_FILE: &str = "birth-prompt.toml";
const HASH_PREFIX: &str = "sha256:";

#[derive(Debug, thiserror::Error)]
pub enum TrustErr {
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot load {path} — the file has a TOML error")]
    ConfigParse {
        path: PathBuf,
        #[source]
        diagnosis: Box<ConfigFileDiagnosis>,
    },
    #[error("cannot load {path} — the file has a TOML error")]
    RecordParse {
        path: PathBuf,
        #[source]
        diagnosis: Box<ConfigFileDiagnosis>,
    },
    #[error("cannot load {path} — the file has a TOML error")]
    BirthPromptParse {
        path: PathBuf,
        #[source]
        diagnosis: Box<ConfigFileDiagnosis>,
    },
    #[error("parsing stored surface json in trust record at {path}: {source}")]
    RecordSurfaceJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("removed project config table in {path}: {detail}")]
    RemovedProjectTable { path: PathBuf, detail: String },
    #[error("removed project config key in {path}: {detail}")]
    RemovedProjectKey { path: PathBuf, detail: String },
    #[error("serializing trust record: {0}")]
    RecordSerialize(#[from] toml::ser::Error),
    #[error("serializing birth prompt dismissal: {0}")]
    BirthPromptSerialize(toml::ser::Error),
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
}

impl TrustErr {
    /// The classified TOML failure for a project-trust file.
    pub fn diagnosis(&self) -> Option<&ConfigFileDiagnosis> {
        match self {
            Self::ConfigParse { diagnosis, .. }
            | Self::RecordParse { diagnosis, .. }
            | Self::BirthPromptParse { diagnosis, .. } => Some(diagnosis),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, TrustErr>;

/// Trust state for a project workspace. Derived from the executable-surface
/// hash and the on-disk grant record; never written as state directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustState {
    /// No `.rimz/config.toml` exists — the project has no executable surface.
    NoConfig,
    /// Config present, no grant record on this machine.
    Untrusted,
    /// Config present, grant record present, hashes match.
    Trusted,
    /// Config present, grant record present, surface hash drifted.
    Stale,
}

impl TrustState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NoConfig => "no_config",
            Self::Untrusted => "untrusted",
            Self::Trusted => "trusted",
            Self::Stale => "stale",
        }
    }
}

/// Human fix lines for a blocked trust state, shared by every trust-gated refusal.
pub fn blocked_fix(state: TrustState) -> &'static str {
    match state {
        TrustState::Stale => {
            "the executable surface changed since your last grant\nreview the change with `rimz trust`, then approve with `rimz trust grant`"
        }
        _ => "review the project config with `rimz trust`, then approve with `rimz trust grant`",
    }
}

#[derive(Clone, Debug)]
pub struct TrustReport {
    pub state: TrustState,
    pub workspace_id: WorkspaceId,
    pub project_root: PathBuf,
    pub config_path: PathBuf,
    pub record_path: PathBuf,
    pub current_hash: Option<String>,
    pub granted_hash: Option<String>,
    pub granted_at: Option<Timestamp>,
    pub surface_diff: Option<Vec<SurfaceDiffEntry>>,
}

/// Offer shown by a fresh interactive `rimz start` for a never-granted project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BirthPromptOffer {
    pub current_hash: String,
    pub summary: SurfaceSummary,
}

/// Human summary of the executable surface in project config.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SurfaceSummary {
    pub task_names: Vec<String>,
    pub profiles: Vec<String>,
    pub subagent_profiles: Vec<String>,
    pub teams: Vec<String>,
    pub env_agents: Vec<String>,
    pub hooks: usize,
}

impl SurfaceSummary {
    fn from_config(config: &ProjectConfig) -> Self {
        Self {
            task_names: config.tasks.keys().cloned().collect(),
            profiles: config.profiles.keys().cloned().collect(),
            subagent_profiles: config.subagents.profiles.keys().cloned().collect(),
            teams: config.teams().keys().cloned().collect(),
            env_agents: config
                .agent_entries()
                .iter()
                .filter(|agent| !agent.env.is_empty() && !agent.name.is_empty())
                .map(|agent| agent.name.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            hooks: config.hooks.len(),
        }
    }
}

/// Leaf-level change from the granted surface to the current surface.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SurfaceDiffEntry {
    pub kind: SurfaceDiffKind,
    pub path: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub granted: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceDiffKind {
    Added,
    Removed,
    Changed,
}

impl SurfaceDiffKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Changed => "changed",
        }
    }
}

/// Read the current trust state for `project_root`. The executable-surface
/// hash is recomputed every call; that's the auto-revoke contract.
pub fn status(project_root: &Path) -> Result<TrustReport> {
    status_with_roots(project_root, &config_home())
}

/// Pin the current executable-surface hash as trusted on this machine.
pub fn grant(project_root: &Path) -> Result<TrustReport> {
    grant_with_roots(project_root, &config_home())
}

/// Delete the trust record. The state reverts to `Untrusted`, or `NoConfig`
/// when `.rimz/config.toml` is absent.
pub fn revoke(project_root: &Path) -> Result<TrustReport> {
    revoke_with_roots(project_root, &config_home())
}

/// Return the one-time birth trust offer for an untrusted project, when due.
pub fn birth_prompt(project_root: &Path) -> Result<Option<BirthPromptOffer>> {
    birth_prompt_with_roots(project_root, &config_home())
}

/// Mark a shown birth prompt as declined using its already-computed surface hash.
pub fn dismiss_birth_prompt_offer(project_root: &Path, offer: &BirthPromptOffer) -> Result<()> {
    dismiss_birth_prompt_offer_with_roots(project_root, &config_home(), offer)
}

/// Project roots with a durable trust grant on this machine.
///
/// Callers still re-evaluate each root's current trust state before executing
/// its configuration; this is only the discovery index for roots that have
/// ever been granted.
pub fn granted_roots() -> Result<Vec<PathBuf>> {
    granted_roots_with_config(&config_home())
}

fn granted_roots_with_config(config_root: &Path) -> Result<Vec<PathBuf>> {
    let projects_root = PROJECTS_SUBDIR
        .iter()
        .fold(config_root.to_path_buf(), |root, part| root.join(part));
    let entries = match std::fs::read_dir(&projects_root) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(TrustErr::Io {
                path: projects_root,
                source,
            });
        }
    };
    let mut roots = BTreeSet::new();
    for entry in entries {
        let path = match entry {
            Ok(entry) => entry.path().join(TRUST_FILE),
            Err(err) => {
                tracing::debug!(error = %err, "skipping unreadable project trust entry");
                continue;
            }
        };
        match read_trust_record(&path) {
            Ok(Some(record)) => {
                roots.insert(record.project_root);
            }
            Ok(None) => {}
            Err(err) => {
                tracing::debug!(path = %path.display(), error = %err, "skipping unreadable project trust record");
            }
        }
    }
    Ok(roots.into_iter().collect())
}

pub fn status_with_roots(project_root: &Path, config_root: &Path) -> Result<TrustReport> {
    let workspace_id = WorkspaceId::from_project_root(project_root);
    let config_path = project_root.join(CONFIG_REL);
    let record_path = trust_record_path(config_root, &workspace_id);

    let current_surface =
        read_project_config(&config_path)?.map(|config| surface_snapshot(&config));
    let current_hash = current_surface.as_ref().map(|surface| surface.hash.clone());
    let record = read_trust_record(&record_path)?;

    let state = match (&current_hash, &record) {
        (None, _) => TrustState::NoConfig,
        (Some(_), None) => TrustState::Untrusted,
        (Some(now), Some(rec)) if &rec.surface_hash == now => TrustState::Trusted,
        (Some(_), Some(_)) => TrustState::Stale,
    };

    let surface_diff = match (&current_surface, &record, state) {
        (Some(current), Some(record), TrustState::Stale) => Some(surface_diff_for_record(
            &record_path,
            record,
            &current.value,
        )?),
        _ => None,
    };

    Ok(TrustReport {
        state,
        workspace_id,
        project_root: project_root.to_path_buf(),
        config_path,
        record_path,
        current_hash,
        granted_hash: record.as_ref().map(|r| r.surface_hash.clone()),
        granted_at: record.as_ref().map(|r| r.granted_at),
        surface_diff,
    })
}

pub fn grant_with_roots(project_root: &Path, config_root: &Path) -> Result<TrustReport> {
    let workspace_id = WorkspaceId::from_project_root(project_root);
    let config_path = project_root.join(CONFIG_REL);
    let record_path = trust_record_path(config_root, &workspace_id);

    let Some(config) = read_project_config(&config_path)? else {
        return Ok(TrustReport {
            state: TrustState::NoConfig,
            workspace_id,
            project_root: project_root.to_path_buf(),
            config_path,
            record_path,
            current_hash: None,
            granted_hash: None,
            granted_at: None,
            surface_diff: None,
        });
    };

    let current_surface = surface_snapshot(&config);
    let previous_record = read_trust_record(&record_path)?;
    let surface_diff = match previous_record.as_ref() {
        Some(record) if record.surface_hash != current_surface.hash => Some(
            surface_diff_for_record(&record_path, record, &current_surface.value)?,
        ),
        _ => None,
    };
    let granted_at = Timestamp::now();
    let record = TrustRecord {
        project_root: project_root.to_path_buf(),
        surface_hash: current_surface.hash.clone(),
        surface_json: current_surface.json.clone(),
        granted_at,
    };
    let text = toml::to_string_pretty(&record)?;
    write_bytes_atomically(&record_path, text.as_bytes())?;
    remove_birth_prompt_dismissal(&birth_prompt_path(config_root, &workspace_id));

    Ok(TrustReport {
        state: TrustState::Trusted,
        workspace_id,
        project_root: project_root.to_path_buf(),
        config_path,
        record_path,
        current_hash: Some(current_surface.hash.clone()),
        granted_hash: Some(current_surface.hash),
        granted_at: Some(granted_at),
        surface_diff,
    })
}

pub fn revoke_with_roots(project_root: &Path, config_root: &Path) -> Result<TrustReport> {
    let workspace_id = WorkspaceId::from_project_root(project_root);
    let record_path = trust_record_path(config_root, &workspace_id);
    match std::fs::remove_file(&record_path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(TrustErr::Io {
                path: record_path,
                source,
            });
        }
    }
    status_with_roots(project_root, config_root)
}

pub fn birth_prompt_with_roots(
    project_root: &Path,
    config_root: &Path,
) -> Result<Option<BirthPromptOffer>> {
    let workspace_id = WorkspaceId::from_project_root(project_root);
    let config_path = project_root.join(CONFIG_REL);
    let Some(config) = read_project_config(&config_path)? else {
        return Ok(None);
    };
    let current_surface = surface_snapshot(&config);
    if read_trust_record(&trust_record_path(config_root, &workspace_id))?.is_some() {
        return Ok(None);
    }
    if read_birth_prompt_dismissal(&birth_prompt_path(config_root, &workspace_id))?
        .is_some_and(|record| record.dismissed_hash == current_surface.hash)
    {
        return Ok(None);
    }
    Ok(Some(BirthPromptOffer {
        current_hash: current_surface.hash,
        summary: SurfaceSummary::from_config(&config),
    }))
}

fn dismiss_birth_prompt_with_roots(project_root: &Path, config_root: &Path) -> Result<()> {
    let config_path = project_root.join(CONFIG_REL);
    let Some(config) = read_project_config(&config_path)? else {
        return Ok(());
    };
    dismiss_birth_prompt_hash_with_roots(
        project_root,
        config_root,
        surface_snapshot(&config).hash.as_str(),
    )
}

pub fn dismiss_birth_prompt_offer_with_roots(
    project_root: &Path,
    config_root: &Path,
    offer: &BirthPromptOffer,
) -> Result<()> {
    dismiss_birth_prompt_hash_with_roots(project_root, config_root, &offer.current_hash)
}

fn dismiss_birth_prompt_hash_with_roots(
    project_root: &Path,
    config_root: &Path,
    current_hash: &str,
) -> Result<()> {
    let workspace_id = WorkspaceId::from_project_root(project_root);
    let record = BirthPromptDismissal {
        dismissed_hash: current_hash.to_owned(),
        dismissed_at: Timestamp::now(),
    };
    let text = toml::to_string_pretty(&record).map_err(TrustErr::BirthPromptSerialize)?;
    write_bytes_atomically(
        &birth_prompt_path(config_root, &workspace_id),
        text.as_bytes(),
    )?;
    Ok(())
}

/// Launch-time `[[agents]]` env for one agent kind, resolved under the trust
/// gate. [`AgentEnv::Apply`] carries the vars an agent launcher injects into
/// the agent process; [`AgentEnv::Blocked`] names the trust state so the
/// launcher refuses at the entry point with the fix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentEnv {
    /// No `[[agents]]` entry with env vars names this kind.
    Unconfigured,
    /// The workspace is trusted; inject these vars into the agent process.
    Apply(BTreeMap<String, String>),
    /// Env is configured but the trust gate is closed.
    Blocked(TrustState),
}

/// Resolve the `[[agents]]` env for `kind` under the trust gate. Entries
/// sharing a name merge in declaration order; later entries win on key
/// collisions. Values are injected literally — no shell expansion.
pub fn agent_env(project_root: &Path, kind: &str) -> Result<AgentEnv> {
    agent_env_with_roots(project_root, &config_home(), kind)
}

pub fn agent_env_with_roots(
    project_root: &Path,
    config_root: &Path,
    kind: &str,
) -> Result<AgentEnv> {
    let Some(config) = read_project_config(&project_root.join(CONFIG_REL))? else {
        return Ok(AgentEnv::Unconfigured);
    };
    let mut env = BTreeMap::new();
    for agent in config
        .agent_entries()
        .iter()
        .filter(|agent| agent.name == kind)
    {
        env.extend(
            agent
                .env
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
    if env.is_empty() {
        return Ok(AgentEnv::Unconfigured);
    }
    match status_with_roots(project_root, config_root)?.state {
        TrustState::Trusted => Ok(AgentEnv::Apply(env)),
        // The config vanished between the read above and the status re-read;
        // nothing remains to apply.
        TrustState::NoConfig => Ok(AgentEnv::Unconfigured),
        state => Ok(AgentEnv::Blocked(state)),
    }
}

fn trust_record_path(config_root: &Path, workspace_id: &WorkspaceId) -> PathBuf {
    project_record_path(config_root, workspace_id, TRUST_FILE)
}

fn birth_prompt_path(config_root: &Path, workspace_id: &WorkspaceId) -> PathBuf {
    project_record_path(config_root, workspace_id, BIRTH_PROMPT_FILE)
}

fn project_record_path(config_root: &Path, workspace_id: &WorkspaceId, file: &str) -> PathBuf {
    let mut path = config_root.to_path_buf();
    for segment in PROJECTS_SUBDIR {
        path.push(segment);
    }
    path.push(workspace_id.as_str());
    path.push(file);
    path
}

fn read_project_config(path: &Path) -> Result<Option<ProjectConfig>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            check_project_config_removed_tables(path, &text)?;
            let config =
                toml::from_str::<ProjectConfig>(&text).map_err(|source| TrustErr::ConfigParse {
                    path: path.to_path_buf(),
                    diagnosis: Box::new(ConfigFileDiagnosis::from_toml_de(path, &text, &source)),
                })?;
            Ok(Some(config))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(TrustErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn check_project_config_removed_tables(path: &Path, text: &str) -> Result<()> {
    let Ok(doc) = toml::from_str::<toml::Table>(text) else {
        return Ok(());
    };
    if doc.contains_key("layout") {
        return Err(TrustErr::RemovedProjectTable {
            path: path.to_path_buf(),
            detail: "`[layout]` and `[[layout.initial_panes]]` are per-machine room layout config; move them to `$XDG_CONFIG_HOME/rimz/config.toml`"
                .to_owned(),
        });
    }
    if let Some(detail) = crate::config::retired_agents_key(&doc) {
        return Err(TrustErr::RemovedProjectKey {
            path: path.to_path_buf(),
            detail,
        });
    }
    Ok(())
}

fn read_trust_record(path: &Path) -> Result<Option<TrustRecord>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let record =
                toml::from_str::<TrustRecord>(&text).map_err(|source| TrustErr::RecordParse {
                    path: path.to_path_buf(),
                    diagnosis: Box::new(ConfigFileDiagnosis::from_toml_de(path, &text, &source)),
                })?;
            Ok(Some(record))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(TrustErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn read_birth_prompt_dismissal(path: &Path) -> Result<Option<BirthPromptDismissal>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let record = toml::from_str::<BirthPromptDismissal>(&text).map_err(|source| {
                TrustErr::BirthPromptParse {
                    path: path.to_path_buf(),
                    diagnosis: Box::new(ConfigFileDiagnosis::from_toml_de(path, &text, &source)),
                }
            })?;
            Ok(Some(record))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(TrustErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn remove_birth_prompt_dismissal(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!(
            path = %path.display(),
            error = %err,
            "failed to remove birth prompt dismissal after trust grant"
        ),
    }
}

/// Hash the executable surface — every field that can cause a process to run.
///
/// The hash input is canonical JSON over [`ExecutableSurface`], so struct
/// field order is fixed, `BTreeMap` keys sort, and `Option::None` serializes
/// as `null`. Empty subagent profiles are omitted for compatibility with
/// grants made before that collection existed. The wire format is
/// `sha256:<hex>`. Changing this projection is a product-invariant change that must land alongside a doc update in
/// [`docs/internals/harness/trust.md`](../../docs/internals/harness/trust.md).
pub fn executable_surface_hash(config: &ProjectConfig) -> String {
    surface_snapshot(config).hash
}

struct SurfaceSnapshot {
    hash: String,
    json: String,
    value: Value,
}

fn surface_snapshot(config: &ProjectConfig) -> SurfaceSnapshot {
    let surface = ExecutableSurface::from(config);
    let json = serde_json::to_string(&surface).expect("ExecutableSurface serializes");
    let digest = Sha256::digest(json.as_bytes());
    let value = serde_json::from_str(&json).expect("ExecutableSurface parses as JSON value");
    SurfaceSnapshot {
        hash: format!("{HASH_PREFIX}{}", hex::encode(digest)),
        json,
        value,
    }
}

/// On-disk trust record at
/// `$XDG_CONFIG_HOME/rimz/projects/<workspace_id>/trust.toml`.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct TrustRecord {
    project_root: PathBuf,
    surface_hash: String,
    surface_json: String,
    granted_at: Timestamp,
}

/// One-time decline record for the fresh-room trust prompt.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct BirthPromptDismissal {
    dismissed_hash: String,
    dismissed_at: Timestamp,
}

/// Project config schema for the trust subsystem. Lenient on unknown keys
/// (non-command fields like `display_name` or `sidebar_width` deserialize
/// without affecting the hash) but exact on the command-running fields
/// documented in `docs/guide/security.md`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ProjectConfig {
    pub agents: ProjectAgents,
    pub profiles: BTreeMap<String, ProjectProfile>,
    pub subagents: ProjectSubagents,
    pub tasks: BTreeMap<String, ProjectTask>,
    pub hooks: Vec<HookConfig>,
    pub env: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ProjectSubagents {
    pub profiles: BTreeMap<String, ProjectProfile>,
}

impl ProjectConfig {
    fn agent_entries(&self) -> &[AgentConfig] {
        match &self.agents {
            ProjectAgents::Entries(entries) => entries,
            ProjectAgents::Table(_) | ProjectAgents::Empty => &[],
        }
    }

    fn teams(&self) -> &BTreeMap<String, Team> {
        match &self.agents {
            ProjectAgents::Entries(_) | ProjectAgents::Empty => {
                static EMPTY: std::sync::LazyLock<BTreeMap<String, Team>> =
                    std::sync::LazyLock::new(BTreeMap::new);
                &EMPTY
            }
            ProjectAgents::Table(table) => &table.teams,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(untagged)]
pub enum ProjectAgents {
    Entries(Vec<AgentConfig>),
    Table(ProjectAgentsTable),
    #[default]
    Empty,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ProjectAgentsTable {
    pub teams: BTreeMap<String, Team>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub name: String,
    pub launch_command: Option<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ProjectProfile {
    pub agent: String,
    pub mode: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    #[serde(rename = "system-prompt-file")]
    pub system_prompt_file: Option<String>,
    #[serde(rename = "append-system-prompt-files")]
    pub append_system_prompt_files: Option<Vec<String>>,
    pub args: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ProjectTask {
    pub agent: Option<String>,
    pub prompt: Option<String>,
    #[serde(rename = "prompt-file")]
    pub prompt_file: Option<PathBuf>,
    pub check: Option<String>,
    pub verify: Option<String>,
    #[serde(rename = "max-attempts")]
    pub max_attempts: Option<u32>,
    pub on: Option<CheckOn>,
    pub root: Option<PathBuf>,
    pub worktree: Option<String>,
    pub mode: Option<String>,
    pub effort: Option<String>,
    #[serde(rename = "system-prompt-file")]
    pub system_prompt_file: Option<PathBuf>,
    pub timeout: Option<String>,
    pub at: Option<String>,
    pub every: Option<String>,
    pub cron: Option<String>,
    pub deadline: Option<Timestamp>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct HookConfig {
    pub event: String,
    pub command: String,
}

/// Canonical projection of [`ProjectConfig`] into just the fields the trust
/// hash covers. Serialized as JSON so the byte order is stable across runs
/// (struct field order is fixed, `BTreeMap` keys sort, `Option::None`
/// serializes as `null`). The subagent profile collection is omitted when
/// empty to preserve the pre-collection wire.
#[derive(Serialize)]
struct ExecutableSurface<'a> {
    agents: Vec<ExecutableAgent<'a>>,
    profiles: Vec<ExecutableProfile<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    subagent_profiles: Vec<ExecutableProfile<'a>>,
    teams: Vec<ExecutableTeam<'a>>,
    tasks: Vec<ExecutableTask<'a>>,
    hooks: Vec<ExecutableHook<'a>>,
    env: &'a BTreeMap<String, String>,
}

#[derive(Serialize)]
struct ExecutableAgent<'a> {
    name: &'a str,
    launch_command: Option<&'a str>,
    env: &'a BTreeMap<String, String>,
}

#[derive(Serialize)]
struct ExecutableProfile<'a> {
    name: &'a str,
    agent: &'a str,
    mode: Option<&'a str>,
    model: Option<&'a str>,
    effort: Option<&'a str>,
    system_prompt_file: Option<&'a str>,
    // Keep the legacy singular projection key so configs without fragments
    // retain their pinned executable-surface hash.
    #[serde(rename = "append_system_prompt_file")]
    append_system_prompt_files: Option<&'a [String]>,
    args: Option<&'a str>,
}

#[derive(Serialize)]
struct ExecutableTeam<'a> {
    name: &'a str,
    layout: Option<&'a str>,
    roles: Vec<ExecutableRole<'a>>,
}

#[derive(Serialize)]
struct ExecutableRole<'a> {
    role: &'a str,
    profile: &'a str,
    mode: Option<&'static str>,
    model: Option<&'a str>,
    effort: Option<&'a str>,
    system_prompt_file: Option<String>,
    // Keep the legacy singular projection key so configs without fragments
    // retain their pinned executable-surface hash.
    #[serde(rename = "append_system_prompt_file")]
    append_system_prompt_files: Option<Vec<String>>,
    args: Option<&'a str>,
}

#[derive(Serialize)]
struct ExecutableHook<'a> {
    event: &'a str,
    command: &'a str,
}

#[derive(Serialize)]
struct ExecutableTask<'a> {
    name: &'a str,
    agent: Option<&'a str>,
    prompt: Option<&'a str>,
    prompt_file: Option<String>,
    check: Option<&'a str>,
    verify: Option<&'a str>,
    max_attempts: Option<u32>,
    on: Option<CheckOn>,
    worktree: Option<&'a str>,
    mode: Option<&'a str>,
    effort: Option<&'a str>,
    system_prompt_file: Option<String>,
    timeout: Option<&'a str>,
    at: Option<&'a str>,
    every: Option<&'a str>,
    cron: Option<&'a str>,
}

fn permission_mode_name(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Auto => "auto",
        PermissionMode::Ask => "ask",
        PermissionMode::Yolo => "yolo",
        PermissionMode::Plan => "plan",
    }
}

impl<'a> From<&'a ProjectConfig> for ExecutableSurface<'a> {
    fn from(config: &'a ProjectConfig) -> Self {
        Self {
            agents: config
                .agent_entries()
                .iter()
                .map(|a| ExecutableAgent {
                    name: a.name.as_str(),
                    launch_command: a.launch_command.as_deref(),
                    env: &a.env,
                })
                .collect(),
            profiles: config
                .profiles
                .iter()
                .map(|(name, p)| ExecutableProfile {
                    name: name.as_str(),
                    agent: p.agent.as_str(),
                    mode: p.mode.as_deref(),
                    model: p.model.as_deref(),
                    effort: p.effort.as_deref(),
                    system_prompt_file: p.system_prompt_file.as_deref(),
                    append_system_prompt_files: p.append_system_prompt_files.as_deref(),
                    args: p.args.as_deref(),
                })
                .collect(),
            subagent_profiles: config
                .subagents
                .profiles
                .iter()
                .map(|(name, p)| ExecutableProfile {
                    name: name.as_str(),
                    agent: p.agent.as_str(),
                    mode: p.mode.as_deref(),
                    model: p.model.as_deref(),
                    effort: p.effort.as_deref(),
                    system_prompt_file: p.system_prompt_file.as_deref(),
                    append_system_prompt_files: p.append_system_prompt_files.as_deref(),
                    args: p.args.as_deref(),
                })
                .collect(),
            teams: config
                .teams()
                .iter()
                .map(|(name, team)| ExecutableTeam {
                    name: name.as_str(),
                    layout: team.layout.as_deref(),
                    roles: team
                        .roles
                        .iter()
                        .map(|role| ExecutableRole {
                            role: role.role.as_str(),
                            profile: role.profile.as_str(),
                            mode: role.mode.map(permission_mode_name),
                            model: role.model.as_deref(),
                            effort: role.effort.as_deref(),
                            system_prompt_file: role
                                .system_prompt_file
                                .as_ref()
                                .map(|path| path.to_string_lossy().into_owned()),
                            append_system_prompt_files: (!role
                                .append_system_prompt_files
                                .is_empty())
                            .then(|| {
                                role.append_system_prompt_files
                                    .iter()
                                    .map(|path| path.to_string_lossy().into_owned())
                                    .collect()
                            }),
                            args: role.args.as_deref(),
                        })
                        .collect(),
                })
                .collect(),
            tasks: config
                .tasks
                .iter()
                .map(|(name, task)| ExecutableTask {
                    name: name.as_str(),
                    agent: task.agent.as_deref(),
                    prompt: task.prompt.as_deref(),
                    prompt_file: task
                        .prompt_file
                        .as_ref()
                        .map(|path| path.to_string_lossy().into_owned()),
                    check: task.check.as_deref(),
                    verify: task.verify.as_deref(),
                    max_attempts: task.max_attempts,
                    on: task.on,
                    worktree: task.worktree.as_deref(),
                    mode: task.mode.as_deref(),
                    effort: task.effort.as_deref(),
                    system_prompt_file: task
                        .system_prompt_file
                        .as_ref()
                        .map(|path| path.to_string_lossy().into_owned()),
                    timeout: task.timeout.as_deref(),
                    at: task.at.as_deref(),
                    every: task.every.as_deref(),
                    cron: task.cron.as_deref(),
                })
                .collect(),
            hooks: config
                .hooks
                .iter()
                .map(|h| ExecutableHook {
                    event: h.event.as_str(),
                    command: h.command.as_str(),
                })
                .collect(),
            env: &config.env,
        }
    }
}

fn surface_diff_for_record(
    record_path: &Path,
    record: &TrustRecord,
    current: &Value,
) -> Result<Vec<SurfaceDiffEntry>> {
    let granted = serde_json::from_str::<Value>(&record.surface_json).map_err(|source| {
        TrustErr::RecordSurfaceJson {
            path: record_path.to_path_buf(),
            source,
        }
    })?;
    Ok(executable_surface_diff(&granted, current))
}

pub fn executable_surface_diff(granted: &Value, current: &Value) -> Vec<SurfaceDiffEntry> {
    let mut entries = Vec::new();
    diff_value(&mut Vec::new(), Some(granted), Some(current), &mut entries);
    entries
}

fn diff_value(
    path: &mut Vec<String>,
    granted: Option<&Value>,
    current: Option<&Value>,
    entries: &mut Vec<SurfaceDiffEntry>,
) {
    match (granted, current) {
        (Some(Value::Object(left)), Some(Value::Object(right))) => {
            let keys: BTreeSet<_> = left.keys().chain(right.keys()).collect();
            for key in keys {
                path.push(key.to_owned());
                diff_value(path, left.get(key), right.get(key), entries);
                path.pop();
            }
        }
        (Some(Value::Array(left)), Some(Value::Array(right))) => {
            for index in 0..left.len().max(right.len()) {
                path.push(format!("[{index}]"));
                diff_value(path, left.get(index), right.get(index), entries);
                path.pop();
            }
        }
        (Some(left), Some(right)) if left == right => {}
        (Some(left), Some(right)) => entries.push(SurfaceDiffEntry {
            kind: SurfaceDiffKind::Changed,
            path: path.clone(),
            granted: Some(left.clone()),
            current: Some(right.clone()),
        }),
        (None, Some(right)) => push_missing(path, right, SurfaceDiffKind::Added, entries),
        (Some(left), None) => push_missing(path, left, SurfaceDiffKind::Removed, entries),
        (None, None) => {}
    }
}

fn push_missing(
    path: &mut Vec<String>,
    value: &Value,
    kind: SurfaceDiffKind,
    entries: &mut Vec<SurfaceDiffEntry>,
) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (key, child) in map {
                path.push(key.clone());
                push_missing(path, child, kind, entries);
                path.pop();
            }
        }
        Value::Array(values) if !values.is_empty() => {
            for (index, child) in values.iter().enumerate() {
                path.push(format!("[{index}]"));
                push_missing(path, child, kind, entries);
                path.pop();
            }
        }
        _ => entries.push(SurfaceDiffEntry {
            kind,
            path: path.clone(),
            granted: (kind != SurfaceDiffKind::Added).then(|| value.clone()),
            current: (kind == SurfaceDiffKind::Added).then(|| value.clone()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn project_with(text: &str) -> tempfile::TempDir {
        let dir = tempdir().expect("tempdir");
        let config_dir = dir.path().join(".rimz");
        std::fs::create_dir_all(&config_dir).expect("mkdir .rimz");
        std::fs::write(config_dir.join("config.toml"), text).expect("write config");
        dir
    }

    fn birth_prompt_due(project_root: &Path, config_root: &Path) -> bool {
        birth_prompt_with_roots(project_root, config_root)
            .expect("birth prompt")
            .is_some()
    }

    #[test]
    fn blocked_fix_distinguishes_stale_from_untrusted() {
        assert!(blocked_fix(TrustState::Stale).contains("since your last grant\n"));
        assert!(blocked_fix(TrustState::Untrusted).starts_with("review the project config"));
    }

    #[test]
    fn empty_project_reports_no_config() {
        let dir = tempdir().expect("tempdir");
        let config = tempdir().expect("config root");
        let report = status_with_roots(dir.path(), config.path()).expect("status");
        assert_eq!(report.state, TrustState::NoConfig);
        assert!(report.current_hash.is_none());
        assert!(report.granted_hash.is_none());
        assert!(!birth_prompt_due(dir.path(), config.path()));
    }

    #[test]
    fn fresh_config_reports_untrusted() {
        let dir =
            project_with("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n");
        let config = tempdir().expect("config root");
        let report = status_with_roots(dir.path(), config.path()).expect("status");
        assert_eq!(report.state, TrustState::Untrusted);
        assert!(report.current_hash.is_some());
        assert!(report.granted_hash.is_none());
    }

    #[test]
    fn birth_prompt_offers_current_hash_and_summary_for_untrusted_config() {
        let dir = project_with(
            "[tasks.sync]\nagent = \"codex\"\nprompt = \"sync the repo\"\n\n[profiles.planner]\nagent = \"claude\"\n\n[subagents.profiles.reviewer]\nagent = \"codex\"\n\n[agents.teams.review]\nlayout = \"planner\"\n\n[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"planner\"\n\n[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n",
        );
        let config = tempdir().expect("config root");
        let offer = birth_prompt_with_roots(dir.path(), config.path())
            .expect("birth prompt")
            .expect("offer");
        let project_config = read_project_config(&dir.path().join(CONFIG_REL))
            .expect("read config")
            .expect("config present");

        assert_eq!(offer.current_hash, executable_surface_hash(&project_config));
        assert_eq!(offer.summary.task_names, vec!["sync".to_owned()]);
        assert_eq!(offer.summary.profiles, vec!["planner".to_owned()]);
        assert_eq!(offer.summary.teams, vec!["review".to_owned()]);
        assert_eq!(offer.summary.hooks, 1);
    }

    #[test]
    fn birth_prompt_dismissal_suppresses_until_surface_changes_and_grant_cleans_it() {
        let dir =
            project_with("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n");
        let config = tempdir().expect("config root");
        let dismissal_path =
            birth_prompt_path(config.path(), &WorkspaceId::from_project_root(dir.path()));

        dismiss_birth_prompt_with_roots(dir.path(), config.path()).expect("dismiss prompt");
        assert!(dismissal_path.exists());
        assert!(!birth_prompt_due(dir.path(), config.path()));

        std::fs::write(
            dir.path().join(CONFIG_REL),
            "[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks codex\"\n",
        )
        .expect("rewrite");
        assert!(birth_prompt_due(dir.path(), config.path()));

        grant_with_roots(dir.path(), config.path()).expect("grant");
        assert!(!dismissal_path.exists());
    }

    #[test]
    fn grant_pins_hash_and_returns_trusted() {
        let dir =
            project_with("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n");
        let config = tempdir().expect("config root");
        let granted = grant_with_roots(dir.path(), config.path()).expect("grant");
        assert_eq!(granted.state, TrustState::Trusted);
        let now = status_with_roots(dir.path(), config.path()).expect("status");
        assert_eq!(now.state, TrustState::Trusted);
        assert_eq!(now.current_hash, granted.current_hash);
        assert_eq!(now.granted_hash, granted.current_hash);
        assert!(!birth_prompt_due(dir.path(), config.path()));
    }

    #[test]
    fn editing_command_field_demotes_to_stale() {
        let dir =
            project_with("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n");
        let config = tempdir().expect("config root");
        grant_with_roots(dir.path(), config.path()).expect("grant");

        std::fs::write(
            dir.path().join(".rimz/config.toml"),
            "[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks codex\"\n",
        )
        .expect("rewrite");

        let report = status_with_roots(dir.path(), config.path()).expect("status");
        assert_eq!(report.state, TrustState::Stale);
        assert_ne!(report.current_hash, report.granted_hash);
        assert!(!birth_prompt_due(dir.path(), config.path()));
        let entries = report
            .surface_diff
            .expect("stale status should explain changed fields");
        assert!(entries.iter().any(|entry| {
            entry.kind == SurfaceDiffKind::Changed
                && entry.path == vec!["hooks".to_owned(), "[0]".to_owned(), "command".to_owned()]
                && entry.granted == Some(serde_json::json!("rimz hooks claude"))
                && entry.current == Some(serde_json::json!("rimz hooks codex"))
        }));
    }

    #[test]
    fn editing_profile_field_demotes_to_stale() {
        let dir = project_with("[profiles.planner]\nagent = \"claude\"\nargs = \"--safe\"\n");
        let config = tempdir().expect("config root");
        grant_with_roots(dir.path(), config.path()).expect("grant");

        std::fs::write(
            dir.path().join(".rimz/config.toml"),
            "[profiles.planner]\nagent = \"codex\"\nargs = \"--safe\"\n",
        )
        .expect("rewrite");

        let report = status_with_roots(dir.path(), config.path()).expect("status");
        assert_eq!(report.state, TrustState::Stale);
        assert_ne!(report.current_hash, report.granted_hash);
    }

    #[test]
    fn unknown_non_command_field_does_not_change_hash() {
        let base = project_with(
            "display_name = \"Query Engine\"\n\n[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n",
        );
        let extra = project_with(
            "display_name = \"Query Engine dev\"\nsidebar = true\n\n[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n",
        );
        let a = read_project_config(&base.path().join(CONFIG_REL))
            .expect("read base")
            .expect("config present");
        let b = read_project_config(&extra.path().join(CONFIG_REL))
            .expect("read extra")
            .expect("config present");
        assert_eq!(executable_surface_hash(&a), executable_surface_hash(&b));
    }

    #[test]
    fn project_notifications_do_not_enter_trust_hash() {
        let base =
            project_with("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n");
        let extra = project_with(
            "[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n\n[notifications]\ntitle = \"{{task}}\"\n[[notifications.handler]]\ncommand = \"ntfy publish rimz {{body}}\"\n",
        );
        let a = read_project_config(&base.path().join(CONFIG_REL))
            .expect("read base")
            .expect("config present");
        let b = read_project_config(&extra.path().join(CONFIG_REL))
            .expect("read extra")
            .expect("config present");
        assert_eq!(executable_surface_hash(&a), executable_surface_hash(&b));
    }

    #[test]
    fn revoke_drops_record_and_returns_untrusted() {
        let dir = project_with("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks\"\n");
        let config = tempdir().expect("config root");
        grant_with_roots(dir.path(), config.path()).expect("grant");
        let revoked = revoke_with_roots(dir.path(), config.path()).expect("revoke");
        assert_eq!(revoked.state, TrustState::Untrusted);
        assert!(!revoked.record_path.exists());
    }

    #[test]
    fn revoke_with_no_record_is_noop() {
        let dir = project_with("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks\"\n");
        let config = tempdir().expect("config root");
        let report = revoke_with_roots(dir.path(), config.path()).expect("revoke");
        assert_eq!(report.state, TrustState::Untrusted);
    }

    #[test]
    fn granted_roots_reads_valid_records_and_skips_corrupt_ones() {
        let first = project_with("[tasks.first]\ncheck = \"true\"\nevery = \"5m\"\n");
        let second = project_with("[tasks.second]\ncheck = \"true\"\nevery = \"5m\"\n");
        let config = tempdir().expect("config root");
        grant_with_roots(first.path(), config.path()).expect("first grant");
        grant_with_roots(second.path(), config.path()).expect("second grant");
        let corrupt = trust_record_path(
            config.path(),
            &WorkspaceId::from_project_root(Path::new("/corrupt")),
        );
        write_bytes_atomically(&corrupt, b"not = [valid").expect("corrupt record");

        assert_eq!(
            granted_roots_with_config(config.path()).expect("granted roots"),
            vec![first.path().to_path_buf(), second.path().to_path_buf()]
                .into_iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn project_layout_table_fails_with_per_machine_fix() {
        let dir =
            project_with("[[layout.initial_panes]]\nname = \"shell\"\ncommand = \"$SHELL\"\n");
        let config = tempdir().expect("config root");
        let err = status_with_roots(dir.path(), config.path()).expect_err("layout must fail");
        let rendered = err.to_string();
        assert!(rendered.contains("[layout]"), "{rendered}");
        assert!(rendered.contains("per-machine"), "{rendered}");
    }

    #[test]
    fn grant_record_stores_canonical_surface_json() {
        let dir =
            project_with("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n");
        let config = tempdir().expect("config root");
        let granted = grant_with_roots(dir.path(), config.path()).expect("grant");
        let record = read_trust_record(&granted.record_path)
            .expect("read trust record")
            .expect("record present");
        let parsed: Value = serde_json::from_str(&record.surface_json).expect("surface json");
        assert_eq!(parsed["hooks"][0]["command"], "rimz hooks claude");
    }

    #[test]
    fn trust_record_without_surface_json_fails_to_parse() {
        let dir =
            project_with("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n");
        let config = tempdir().expect("config root");
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let record_path = trust_record_path(config.path(), &workspace_id);
        let original_config = read_project_config(&dir.path().join(CONFIG_REL))
            .expect("read config")
            .expect("config present");
        #[derive(Serialize)]
        struct LegacyTrustRecord<'a> {
            project_root: &'a Path,
            surface_hash: String,
            granted_at: Timestamp,
        }
        let record = LegacyTrustRecord {
            project_root: dir.path(),
            surface_hash: executable_surface_hash(&original_config),
            granted_at: Timestamp::now(),
        };
        let text = toml::to_string_pretty(&record).expect("serialize legacy record");
        write_bytes_atomically(&record_path, text.as_bytes()).expect("write record");

        let err = status_with_roots(dir.path(), config.path()).expect_err("status must fail");
        match err {
            TrustErr::RecordParse { path, diagnosis } => {
                assert_eq!(path, record_path);
                let rendered = diagnosis.to_string();
                assert!(
                    rendered.contains("missing field `surface_json`"),
                    "{rendered}"
                );
            }
            other => panic!("expected record parse error, got {other:?}"),
        }
    }

    #[test]
    fn corrupt_stored_surface_json_returns_structured_error() {
        let dir =
            project_with("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n");
        let config = tempdir().expect("config root");
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let record_path = trust_record_path(config.path(), &workspace_id);
        let original_config = read_project_config(&dir.path().join(CONFIG_REL))
            .expect("read config")
            .expect("config present");
        let record = TrustRecord {
            project_root: dir.path().to_path_buf(),
            surface_hash: executable_surface_hash(&original_config),
            surface_json: "not-json".to_owned(),
            granted_at: Timestamp::now(),
        };
        let text = toml::to_string_pretty(&record).expect("serialize record");
        write_bytes_atomically(&record_path, text.as_bytes()).expect("write record");

        std::fs::write(
            dir.path().join(CONFIG_REL),
            "[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks codex\"\n",
        )
        .expect("rewrite");

        let err = status_with_roots(dir.path(), config.path()).expect_err("status must fail");
        match err {
            TrustErr::RecordSurfaceJson { path, source } => {
                assert_eq!(path, record_path);
                assert!(source.is_syntax(), "{source}");
            }
            other => panic!("expected surface json error, got {other:?}"),
        }
    }

    #[test]
    fn grant_returns_diff_before_repinning_stale_surface() {
        let dir =
            project_with("[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n");
        let config = tempdir().expect("config root");
        grant_with_roots(dir.path(), config.path()).expect("grant");
        std::fs::write(
            dir.path().join(CONFIG_REL),
            "[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks codex\"\n",
        )
        .expect("rewrite");

        let granted = grant_with_roots(dir.path(), config.path()).expect("regrant");
        assert_eq!(granted.state, TrustState::Trusted);
        assert!(matches!(
            granted.surface_diff,
            Some(ref entries) if !entries.is_empty()
        ));
        let now = status_with_roots(dir.path(), config.path()).expect("status");
        assert_eq!(now.state, TrustState::Trusted);
        assert!(now.surface_diff.is_none());
    }

    #[test]
    fn agent_env_is_unconfigured_without_a_matching_entry() {
        let config = tempdir().expect("config root");

        let empty = tempdir().expect("tempdir");
        assert_eq!(
            agent_env_with_roots(empty.path(), config.path(), "claude").expect("agent env"),
            AgentEnv::Unconfigured,
        );

        let other_kind = project_with("[[agents]]\nname = \"claude\"\nenv = { FOO = \"1\" }\n");
        grant_with_roots(other_kind.path(), config.path()).expect("grant");
        assert_eq!(
            agent_env_with_roots(other_kind.path(), config.path(), "codex").expect("agent env"),
            AgentEnv::Unconfigured,
        );

        let no_env = project_with("[[agents]]\nname = \"claude\"\nlaunch_command = \"claude\"\n");
        grant_with_roots(no_env.path(), config.path()).expect("grant");
        assert_eq!(
            agent_env_with_roots(no_env.path(), config.path(), "claude").expect("agent env"),
            AgentEnv::Unconfigured,
        );
    }

    #[test]
    fn agent_env_applies_merged_entries_when_trusted() {
        let dir = project_with(
            "[[agents]]\nname = \"claude\"\nenv = { A = \"1\", B = \"1\" }\n\n[[agents]]\nname = \"claude\"\nenv = { B = \"2\" }\n",
        );
        let config = tempdir().expect("config root");
        grant_with_roots(dir.path(), config.path()).expect("grant");

        let env = match agent_env_with_roots(dir.path(), config.path(), "claude") {
            Ok(AgentEnv::Apply(env)) => env,
            other => panic!("expected Apply, got {other:?}"),
        };
        assert_eq!(
            env,
            BTreeMap::from([
                ("A".to_owned(), "1".to_owned()),
                ("B".to_owned(), "2".to_owned())
            ]),
        );
    }

    #[test]
    fn agent_env_blocks_untrusted_and_stale_workspaces() {
        let dir = project_with("[[agents]]\nname = \"claude\"\nenv = { FOO = \"1\" }\n");
        let config = tempdir().expect("config root");
        assert_eq!(
            agent_env_with_roots(dir.path(), config.path(), "claude").expect("agent env"),
            AgentEnv::Blocked(TrustState::Untrusted),
        );

        grant_with_roots(dir.path(), config.path()).expect("grant");
        std::fs::write(
            dir.path().join(CONFIG_REL),
            "[[agents]]\nname = \"claude\"\nenv = { FOO = \"2\" }\n",
        )
        .expect("rewrite");
        assert_eq!(
            agent_env_with_roots(dir.path(), config.path(), "claude").expect("agent env"),
            AgentEnv::Blocked(TrustState::Stale),
        );
    }

    #[test]
    fn hash_covers_every_documented_surface_field() {
        // One config per documented executable-surface field. Any two must
        // hash to distinct values; if a future refactor drops a field from
        // `ExecutableSurface`, two cases collide and this test fires.
        let cases = [
            "[[agents]]\nname = \"claude\"\nlaunch_command = \"claude code\"\n",
            "[[agents]]\nname = \"claude\"\nenv = { PATH = \"/opt/llms/bin\" }\n",
            "[profiles.x]\nagent = \"claude\"\n",
            "[profiles.x]\nagent = \"codex\"\n",
            "[profiles.x]\nagent = \"claude\"\nmode = \"ask\"\n",
            "[profiles.x]\nagent = \"claude\"\nmodel = \"opus\"\n",
            "[profiles.x]\nagent = \"claude\"\neffort = \"low\"\n",
            "[profiles.x]\nagent = \"claude\"\nsystem-prompt-file = \"prompts/x.md\"\n",
            "[profiles.x]\nagent = \"claude\"\nappend-system-prompt-files = [\"prompts/a.md\"]\n",
            "[profiles.x]\nagent = \"claude\"\nargs = \"--profile x\"\n",
            "[profiles.y]\nagent = \"claude\"\n",
            "[agents.teams.review]\nlayout = \"planner,coder\"\n\n[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"x\"\n[[agents.teams.review.roles]]\nrole = \"coder\"\nprofile = \"x\"\n",
            "[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"x\"\n",
            "[[agents.teams.review.roles]]\nrole = \"coder\"\nprofile = \"x\"\n",
            "[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"y\"\n",
            "[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"x\"\nmode = \"ask\"\n",
            "[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"x\"\nmodel = \"opus\"\n",
            "[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"x\"\neffort = \"low\"\n",
            "[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"x\"\nsystem-prompt-file = \"prompts/planner.md\"\n",
            "[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"x\"\nappend-system-prompt-files = [\"prompts/a.md\"]\n",
            "[[agents.teams.review.roles]]\nrole = \"planner\"\nprofile = \"x\"\nargs = \"--role planner\"\n",
            "[tasks.x]\nagent = \"codex\"\n",
            "[tasks.y]\nagent = \"codex\"\n",
            "[tasks.x]\nprompt = \"repair CI\"\n",
            "[tasks.x]\nprompt-file = \"prompts/ci.md\"\n",
            "[tasks.x]\ncheck = \"cargo test\"\n",
            "[tasks.x]\nverify = \"cargo test\"\n",
            "[tasks.x]\nmax-attempts = 4\n",
            "[tasks.x]\non = \"success\"\n",
            "[tasks.x]\nworktree = \"sync\"\n",
            "[tasks.x]\nmode = \"yolo\"\n",
            "[tasks.x]\neffort = \"low\"\n",
            "[tasks.x]\nsystem-prompt-file = \"prompts/system.md\"\n",
            "[tasks.x]\ntimeout = \"2h\"\n",
            "[tasks.x]\nat = \"08:00\"\n",
            "[tasks.x]\nevery = \"15m\"\n",
            "[tasks.x]\ncron = \"0 8 * * 1\"\n",
            "[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n",
            "[env]\nPATH_PREPEND = \"/opt/rimz/bin\"\n",
        ];
        let mut hashes = std::collections::HashSet::new();
        for text in cases {
            let config: ProjectConfig =
                toml::from_str(text).unwrap_or_else(|err| panic!("parse `{text}`: {err}"));
            assert!(
                hashes.insert(executable_surface_hash(&config)),
                "case `{text}` collided with another surface case",
            );
        }
    }

    #[test]
    fn surface_diff_reports_added_leaf() {
        let diff = executable_surface_diff(
            &serde_json::json!({"env": {}}),
            &serde_json::json!({"env": {"FOO": "1"}}),
        );
        assert_eq!(
            diff,
            vec![SurfaceDiffEntry {
                kind: SurfaceDiffKind::Added,
                path: vec!["env".to_owned(), "FOO".to_owned()],
                granted: None,
                current: Some(serde_json::json!("1")),
            }]
        );
    }

    #[test]
    fn surface_diff_reports_removed_leaf() {
        let diff = executable_surface_diff(
            &serde_json::json!({"env": {"FOO": "1"}}),
            &serde_json::json!({"env": {}}),
        );
        assert_eq!(
            diff,
            vec![SurfaceDiffEntry {
                kind: SurfaceDiffKind::Removed,
                path: vec!["env".to_owned(), "FOO".to_owned()],
                granted: Some(serde_json::json!("1")),
                current: None,
            }]
        );
    }

    #[test]
    fn surface_diff_reports_changed_leaf() {
        let diff = executable_surface_diff(
            &serde_json::json!({"hooks": [{"command": "a"}]}),
            &serde_json::json!({"hooks": [{"command": "b"}]}),
        );
        assert_eq!(
            diff,
            vec![SurfaceDiffEntry {
                kind: SurfaceDiffKind::Changed,
                path: vec!["hooks".to_owned(), "[0]".to_owned(), "command".to_owned()],
                granted: Some(serde_json::json!("a")),
                current: Some(serde_json::json!("b")),
            }]
        );
    }

    #[test]
    fn surface_diff_noops_on_equal_values() {
        let value = serde_json::json!({"hooks": [{"command": "a"}]});
        assert!(executable_surface_diff(&value, &value).is_empty());
    }
}
