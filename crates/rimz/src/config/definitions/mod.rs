//! Machine-tier Markdown definitions, resolved into the existing profile schema.
//! Errors without a `failed` owner refuse every launch; pack-scoped diagnostics use
//! `DefinitionCause::PackProblem` to retain reporting without failing the definition set.
//!
//! A Markdown profile merges in `frontmatter.rs` (`AgentFrontmatter::inherit` and
//! `inherit_base`), then resolves and lowers in `agent.rs` (`finish_profile`).

mod agent;
mod catalog;
pub mod customization;
pub(crate) mod frontmatter;
pub mod inventory;
pub mod review;
mod team;
mod traits;

pub use agent::DefinitionSource;
pub(crate) use agent::keeps_inherited_model;
pub(super) use agent::{TierCandidates, build_tier_candidates};
pub use traits::ConsensusPieces;

#[cfg(test)]
mod tests;

#[cfg(test)]
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::{
    AgentSpecSources, CommandsConfig, DefinitionOrigin, Profile, ProfileSource, ProfilesConfig,
    TeamsConfig,
};
use crate::packs::{ClaimResolution, ItemFate, PackItem, PackName, Problem, ResolvedPacks};
use frontmatter::AgentFrontmatter;

/// An override base passed to [`Profile::build_under`]: a Markdown definition row, or a resolved
/// provider runtime (bare kind, kind base, or project TOML chain) with an optional written model.
#[derive(Clone, Copy)]
pub enum BuildBase<'a> {
    Profile(&'a Profile),
    Runtime(crate::agents::Runtime, Option<&'a str>),
}

impl std::fmt::Debug for BuildBase<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Profile(profile) => f.debug_tuple("Profile").field(profile).finish(),
            Self::Runtime(runtime, model) => f
                .debug_tuple("Runtime")
                .field(&runtime.name())
                .field(model)
                .finish(),
        }
    }
}

/// A user-shell launch that can reach a provider's hook-trust prompt.
pub fn hook_trust_fix(
    kind: &crate::ids::AgentKind,
    profile: Option<&str>,
    relaunch: Option<(&crate::agents::AgentState, &str)>,
) -> String {
    let team_member = |agent: &crate::agents::AgentState| {
        (agent.team.is_some() && agent.role.is_some())
            || profile.is_some_and(|profile| profile.contains('.'))
    };
    // `rimz agents resume @handle` refuses team members; restart and fork accept them.
    if let Some((agent, action)) = relaunch
        && !(action == "resume" && team_member(agent))
    {
        return format!(
            "rimz agents {action} @{}",
            agent.name.as_deref().unwrap_or(agent.agent_id.as_str())
        );
    }
    let profile = profile.and_then(standalone_profile);
    format!(
        "rimz agents {}",
        profile.as_deref().unwrap_or(kind.as_str())
    )
}

fn standalone_profile(profile: &str) -> Option<String> {
    let Some((team, role)) = profile.split_once('.') else {
        return Some(profile.to_owned());
    };
    let (_, sources) = super::MachineConfig::load_with_agent_spec_sources().ok()?;
    let path = sources.team(team)?;
    let text = std::fs::read_to_string(path).ok()?;
    let (yaml, _) = frontmatter::split(path, &text).ok()?;
    let team: frontmatter::TeamFrontmatter = frontmatter::parse(path, yaml).ok()?;
    team.roles
        .into_iter()
        .find(|binding| binding.role.as_deref().unwrap_or(&binding.agent) == role)
        .map(|binding| binding.agent)
}

/// Whether enforced skill lists resolve against the provider root under `env`, then the
/// RimZ skill chain derived from the loader's home. Enforcement uses inherited isolation,
/// then `machine_isolation`; off Linux every definition runs on the host.
/// The check reads host roots; callers inside a scoped view must skip it.
#[derive(Clone, Copy, Debug)]
pub enum SkillCheck<'a> {
    Skip,
    Check {
        env: &'a BTreeMap<String, String>,
        machine_isolation: super::Isolation,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum OwnedSkillCheck {
    Skip,
    Check {
        env: BTreeMap<String, String>,
        machine_isolation: super::Isolation,
    },
}

impl From<SkillCheck<'_>> for OwnedSkillCheck {
    fn from(check: SkillCheck<'_>) -> Self {
        match check {
            SkillCheck::Skip => Self::Skip,
            SkillCheck::Check {
                env,
                machine_isolation,
            } => Self::Check {
                env: env.clone(),
                machine_isolation,
            },
        }
    }
}

impl OwnedSkillCheck {
    fn as_ref(&self) -> SkillCheck<'_> {
        match self {
            Self::Skip => SkillCheck::Skip,
            Self::Check {
                env,
                machine_isolation,
            } => SkillCheck::Check {
                env,
                machine_isolation: *machine_isolation,
            },
        }
    }
}

type SkillInventory = Arc<crate::agents::skills::SkillInventory>;
type TextRead = Result<Arc<str>, Arc<std::io::Error>>;

#[cfg(test)]
thread_local! {
    static TEXT_READS: RefCell<BTreeMap<PathBuf, usize>> = const { RefCell::new(BTreeMap::new()) };
}

#[cfg(test)]
fn count_text_read(path: &Path) {
    TEXT_READS.with(|reads| *reads.borrow_mut().entry(path.to_path_buf()).or_default() += 1);
}

#[derive(Debug)]
struct SkillCatalog {
    check: OwnedSkillCheck,
    chain: crate::agents::skills::SkillChain,
    inventories: Mutex<BTreeMap<String, Result<SkillInventory, String>>>,
    texts: Mutex<BTreeMap<PathBuf, TextRead>>,
}

impl PartialEq for SkillCatalog {
    fn eq(&self, other: &Self) -> bool {
        self.check == other.check && self.chain.roots() == other.chain.roots()
    }
}

impl Eq for SkillCatalog {}

impl SkillCatalog {
    fn new(check: SkillCheck<'_>, home: &Path, packs: &ResolvedPacks) -> Self {
        Self {
            check: check.into(),
            chain: crate::agents::skills::SkillChain::from_resolved(home, packs),
            inventories: Mutex::default(),
            texts: Mutex::default(),
        }
    }

    fn read_text(&self, path: &Path) -> TextRead {
        let mut texts = self
            .texts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        texts
            .entry(path.to_path_buf())
            .or_insert_with(|| {
                #[cfg(test)]
                count_text_read(path);
                std::fs::read_to_string(path)
                    .map(Arc::from)
                    .map_err(Arc::new)
            })
            .clone()
    }

    fn enumerate(&self, kind: &str) -> Result<Option<SkillInventory>, String> {
        let OwnedSkillCheck::Check { env, .. } = &self.check else {
            return Ok(None);
        };
        let mut inventories = self
            .inventories
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inventories
            .entry(kind.to_owned())
            .or_insert_with(|| {
                crate::agents::skills::enumerate(
                    crate::agents::find_definition(kind)
                        .and_then(|definition| definition.skills_home(env))
                        .as_deref(),
                    Some(&self.chain),
                    |_| true,
                )
                .map(Arc::new)
                .map_err(|error| error.to_string())
            })
            .clone()
            .map(Some)
    }
}

#[derive(Debug, Default)]
pub struct LoadedDefinitions {
    pub agent_profiles: ProfilesConfig,
    pub subagent_profiles: ProfilesConfig,
    pub teams: TeamsConfig,
    pub sources: AgentSpecSources,
    pub errors: Vec<DefinitionErr>,
    pub failed: BTreeMap<String, BTreeSet<PathBuf>>,
    pub rows: Vec<DefinitionRow>,
    pub pack_items: Vec<PackItem>,
    pub warnings: Vec<crate::packs::Problem>,
    pack_problems: BTreeMap<PackName, Vec<Problem>>,
    pub(in crate::config) consensus_pieces: ConsensusPieces,
    pub(in crate::config) contexts: Option<LoadContexts>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{}: {}", crate::packs::one_line(&.path.display().to_string()), crate::packs::one_line(.message))]
pub struct DefinitionErr {
    pub path: PathBuf,
    pub message: String,
    pub cause: DefinitionCause,
}

/// Why a definition could not load, for surfaces that group failures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DefinitionCause {
    /// A listed skill is absent from the provider root and RimZ skill chain.
    MissingSkill {
        skill: super::SkillName,
        roots: Vec<PathBuf>,
    },
    /// A parent, kind base, allowed subagent, or team role's agent failed to load.
    DependsOnFailed { name: String },
    /// A pack-scoped diagnostic that does not refuse unrelated launches.
    PackProblem,
    /// The message describes a failure that has no more specific grouping.
    Invalid,
}

/// The tail of every refusal for a definition whose kind base is absent.
/// Nothing in the tree ships one, so naming the file is not enough: the
/// message has to say what a kind base is, where to write it, and that an
/// empty body is refused in turn.
pub(super) fn missing_kind_base(home: &Path, kind: &str) -> String {
    let shipped = std::iter::once(kind)
        .chain(
            crate::agents::find_definition(kind)
                .map(|definition| definition.spec().aliases)
                .unwrap_or_default()
                .iter()
                .copied(),
        )
        .map(|name| format!("agents/{name}.md"))
        .collect::<Vec<_>>()
        .join(" or ");
    format!(
        "whose kind base is missing — create {} with a nonempty body, the system prompt every \
         {kind} definition starts from, or install a provider pack that ships {shipped}",
        home.join("agents").join(format!("{kind}.md")).display()
    )
}

/// The refusal for a kind base whose prompt body is empty, raw or rendered.
fn empty_kind_base(stem: &str) -> String {
    format!(
        "kind base has no prompt body — write the system prompt every {stem} definition starts \
         from below the frontmatter"
    )
}

/// The tail of every refusal for a definition whose kind base exists but failed to load,
/// so the user fixes that file rather than creating it again.
fn failed_kind_base(path: &Path) -> String {
    format!(
        "whose kind base {} failed to load; fix that file",
        path.display()
    )
}

impl DefinitionErr {
    fn new(path: &Path, message: impl AsRef<str>) -> Self {
        Self {
            path: path.to_owned(),
            cause: DefinitionCause::Invalid,
            message: message
                .as_ref()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        }
    }

    fn with_cause(mut self, cause: DefinitionCause) -> Self {
        self.cause = cause;
        self
    }

    /// Whether this kind base failure is packs claiming `kind` at once, which a dependent
    /// repeats whole: no one file is the one to fix.
    fn is_conflict_over(&self, kind: &str) -> bool {
        matches!(&self.cause, DefinitionCause::DependsOnFailed { name } if name == kind)
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct DefinitionRow {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_warm: Option<super::KeepWarmFields>,
    pub name: String,
    pub namespace: String,
    pub kind: String,
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<super::tiers::ModelTier>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier_fallback: Option<bool>,
    pub effort: Option<String>,
    /// Whether the definition launches in the provider's fast mode.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fast: bool,
    pub source: PathBuf,
    #[serde(flatten)]
    pub origin: DefinitionOrigin,
    pub team: Option<String>,
    pub role: Option<String>,
    pub owns: Vec<String>,
    pub signals: Vec<super::TeamSignalBinding>,
}

/// Profile values carried only by project TOML rows, lowered beside frontmatter by the builder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct TomlExtras {
    pub system_prompt_file: Option<super::PromptSource>,
    pub append_system_prompt_files: Vec<super::PromptSource>,
    pub args: Option<String>,
}

#[derive(Clone)]
struct Definition {
    path: PathBuf,
    pack: Option<PackName>,
    body: String,
    frontmatter: AgentFrontmatter,
    extras: TomlExtras,
}

#[derive(Default)]
struct Namespace {
    definitions: BTreeMap<String, Definition>,
    failed: BTreeSet<String>,
    problems: BTreeMap<String, DefinitionErr>,
    packs: BTreeSet<String>,
}

impl Namespace {
    fn lookup(&self, name: &str, source: &Path) -> Result<Option<&Definition>, DefinitionErr> {
        if let Some(error) = self.problems.get(name) {
            return Err(DefinitionErr::new(source, &error.message).with_cause(error.cause.clone()));
        }
        if let Some(definition) = self.definitions.get(name) {
            return Ok(Some(definition));
        }
        if let Some((pack, item)) = name.split_once(':') {
            let message = if self.packs.contains(pack) {
                format!(
                    "pack '{pack}' does not ship '{item}' in this namespace; run `rimz packs show {pack}`"
                )
            } else {
                format!(
                    "unknown pack '{pack}'; installed packs: {}; run `rimz packs list`",
                    self.packs.iter().cloned().collect::<Vec<_>>().join(", ")
                )
            };
            return Err(DefinitionErr::new(source, message));
        }
        Ok(None)
    }
}

/// Markdown files under `home` that are not definitions: contributor notes in
/// definition trees, and the inspection copy of the built-in consensus RimZ
/// publishes into `teams/`. Home traits alone admit contributor-note names.
fn reserved(home: &Path, path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("AGENTS.md" | "CLAUDE.md" | "README.md")
    ) || path == crate::harness::team_prompt::consensus_copy_path(home)
}

fn files(home: &Path, namespace: &str) -> Result<Vec<PathBuf>, DefinitionErr> {
    let directory = home.join(namespace);
    let directory = directory.as_path();
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(DefinitionErr::new(directory, error.to_string())),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| DefinitionErr::new(directory, error.to_string()))?;
        let path = entry.path();
        if !path.extension().is_some_and(|extension| extension == "md")
            || (namespace != "traits" && reserved(home, &path))
        {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|error| DefinitionErr::new(&path, error.to_string()))?;
        if file_type.is_file() || file_type.is_symlink() {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

pub(super) fn source_paths(agents_home: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for name in ["agents", "subagents", "teams", "traits"] {
        paths.extend(files(agents_home, name).unwrap_or_default());
        paths.push(agents_home.join(name));
    }
    paths.sort();
    paths
}

/// Immutable state from one definitions load, retained by stored Markdown rows so on-demand
/// builds reuse kind bases, tiers, and the skill catalog without rereading disk.
#[derive(Debug, PartialEq, Eq)]
struct SharedLoadState {
    home: PathBuf,
    bases: BTreeMap<String, AgentFrontmatter>,
    failed_bases: BTreeMap<String, DefinitionErr>,
    skills: Arc<SkillCatalog>,
    tiers: super::tiers::TierConfig,
    auto_compact_default: Option<super::AutoCompactDefault>,
}

/// The subagent-namespace names and diagnostics needed to validate a direct definition's
/// `subagents:` list after the load returns.
#[derive(Debug, PartialEq, Eq)]
struct SubagentCheck {
    defined: BTreeSet<String>,
    failed: BTreeSet<String>,
    problems: BTreeMap<String, DefinitionErr>,
    packs: BTreeSet<String>,
    allowed: BTreeSet<String>,
}

impl SubagentCheck {
    fn from_namespace(subagents: &Namespace, allowed: &BTreeSet<String>) -> Self {
        Self {
            defined: subagents.definitions.keys().cloned().collect(),
            failed: subagents.failed.clone(),
            problems: subagents.problems.clone(),
            packs: subagents.packs.clone(),
            allowed: allowed.clone(),
        }
    }

    fn check_lookup(&self, name: &str, source: &Path) -> Result<(), DefinitionErr> {
        if let Some(error) = self.problems.get(name) {
            return Err(DefinitionErr::new(source, &error.message).with_cause(error.cause.clone()));
        }
        if self.defined.contains(name) {
            return Ok(());
        }
        if let Some((pack, item)) = name.split_once(':') {
            let message = if self.packs.contains(pack) {
                format!(
                    "pack '{pack}' does not ship '{item}' in this namespace; run `rimz packs show {pack}`"
                )
            } else {
                format!(
                    "unknown pack '{pack}'; installed packs: {}; run `rimz packs list`",
                    self.packs.iter().cloned().collect::<Vec<_>>().join(", ")
                )
            };
            return Err(DefinitionErr::new(source, message));
        }
        Ok(())
    }
}

/// Per-namespace load context shared across all retained definitions of one load.
#[derive(Debug, PartialEq, Eq)]
struct LoadContext {
    shared: Arc<SharedLoadState>,
    subagents: Option<Arc<SubagentCheck>>,
}

/// Both namespace load contexts retained on [`super::MachineConfig`] after a machine load, so
/// project rows and later bare-name sources build without rereading disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadContexts {
    agents: Arc<LoadContext>,
    subagents: Arc<LoadContext>,
}

/// The inputs every resolver and seat loader in one [`load`] shares.
#[derive(Clone, Copy)]
struct LoadScope<'a> {
    context: &'a Arc<LoadContext>,
    traits: &'a BTreeMap<String, ClaimResolution>,
}

/// A kind base that loaded, held back until its `subagents:` names check.
struct PendingBase<'a> {
    item: &'a catalog::Item,
    /// Its names, the canonical kind first.
    keys: Vec<String>,
    /// Its rows in the agents table and in the subagents table.
    rows: (Profile, Profile),
}

/// Where an item that failed to load is recorded.
struct Failed<'a> {
    tree: &'a mut Namespace,
    failed_bases: &'a mut BTreeMap<String, DefinitionErr>,
    loaded: &'a mut LoadedDefinitions,
}

impl Failed<'_> {
    fn record(
        self,
        item: &catalog::Item,
        keys: Vec<String>,
        kind_base: bool,
        error: DefinitionErr,
    ) {
        let (path, name) = (&item.path, &item.name);
        let error = if item.pack.is_some() && error.cause != DefinitionCause::PackProblem {
            let owned = item.error(error.message);
            // A launch refusal follows a typed cause to the file to fix.
            match error.cause {
                DefinitionCause::Invalid => owned,
                cause => owned.with_cause(cause),
            }
        } else {
            error
        };
        if kind_base {
            self.failed_bases.insert(name.clone(), error.clone());
        }
        for key in keys {
            self.tree.failed.insert(key.clone());
            if key.contains(':') {
                self.tree.problems.insert(
                    key.clone(),
                    DefinitionErr::new(
                        path,
                        format!("follows `{key}`, which failed to load: {error}"),
                    )
                    .with_cause(DefinitionCause::DependsOnFailed { name: key }),
                );
            }
        }
        if item.selected() {
            self.loaded
                .failed
                .entry(name.clone())
                .or_default()
                .insert(path.clone());
            self.loaded.errors.push(error);
        }
    }
}

/// Non-empty, and only ASCII alphanumerics, `_`, and `-`.
fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// `commands` are the `[agents.commands]` names a `subagents:` list may also allow.
pub fn load(
    agents_home: &Path,
    skills: SkillCheck<'_>,
    commands: &CommandsConfig,
    config_path: &Path,
    tiers: &super::tiers::TierConfig,
    auto_compact_default: Option<super::AutoCompactDefault>,
) -> LoadedDefinitions {
    load_resolved(
        agents_home,
        skills,
        commands,
        config_path,
        tiers,
        auto_compact_default,
        &crate::packs::resolve(agents_home),
    )
}

fn load_resolved(
    agents_home: &Path,
    skills: SkillCheck<'_>,
    commands: &CommandsConfig,
    config_path: &Path,
    tiers: &super::tiers::TierConfig,
    auto_compact_default: Option<super::AutoCompactDefault>,
    packs: &ResolvedPacks,
) -> LoadedDefinitions {
    let catalog = Arc::new(SkillCatalog::new(skills, agents_home, packs));
    let mut loaded = LoadedDefinitions::default();
    loaded.warnings.clone_from(&packs.warnings);
    loaded.errors.extend(packs.problems.iter().map(|problem| {
        DefinitionErr::new(&problem.path, &problem.message).with_cause(DefinitionCause::PackProblem)
    }));
    let sources = catalog::Catalog::new(
        agents_home,
        packs,
        commands,
        config_path,
        &catalog,
        &mut loaded,
    );
    loaded.consensus_pieces = ConsensusPieces::new(agents_home, &sources.claims["traits"]);
    if let Ok(inventory) = catalog.chain.inventory(|_| true) {
        for problem in inventory
            .conflicts
            .values()
            .chain(inventory.pack_problems.values().flatten())
        {
            loaded.errors.push(
                DefinitionErr::new(&problem.path, &problem.message)
                    .with_cause(DefinitionCause::PackProblem),
            );
        }
        for (pack, problems) in inventory.pack_problems {
            loaded
                .pack_problems
                .entry(pack)
                .or_default()
                .extend(problems);
        }
    }
    let installed: BTreeSet<_> = packs
        .record
        .sources
        .iter()
        .flat_map(|source| &source.packs)
        .map(|pack| pack.name.to_string())
        .collect();
    let mut agents = Namespace {
        packs: installed.clone(),
        ..Namespace::default()
    };
    let mut subagents = Namespace {
        packs: installed,
        ..Namespace::default()
    };
    for (name, claim) in &sources.claims["definitions"] {
        if let ClaimResolution::Conflict { claims, message } = claim {
            let error = DefinitionErr::new(&claims[0].path, message)
                .with_cause(DefinitionCause::DependsOnFailed { name: name.clone() });
            for tree in [&mut agents, &mut subagents] {
                tree.failed.insert(name.clone());
                tree.problems.insert(name.clone(), error.clone());
            }
        }
    }
    let mut bases = BTreeMap::new();
    let mut pending: Vec<PendingBase<'_>> = Vec::new();
    let mut failed_bases = BTreeMap::new();
    for (name, claim) in &sources.claims["bases"] {
        if let ClaimResolution::Conflict { claims, message } = claim {
            failed_bases.insert(
                name.clone(),
                DefinitionErr::new(&claims[0].path, message)
                    .with_cause(DefinitionCause::DependsOnFailed { name: name.clone() }),
            );
        }
    }
    let mut names: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut base_paths: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (namespace, tree) in [("agents", &mut agents), ("subagents", &mut subagents)] {
        for item in sources
            .items
            .iter()
            .filter(|item| item.kind.as_str() == namespace)
        {
            let path = &item.path;
            let name = &item.name;
            let kind_base = crate::agents::find_definition(name)
                .filter(|_| catalog::kind_base(item.kind, path, name));
            if kind_base.is_some() && !item.selected() {
                continue;
            }
            let qualified = item.pack.as_ref().map(|pack| format!("{pack}:{name}"));
            if item.excluded() {
                if let Some(key) = qualified {
                    let pack = item
                        .pack
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    let target = if matches!(item.fate, Some(ItemFate::ExcludedType)) {
                        format!("{pack} --type {namespace}")
                    } else {
                        format!("{pack}:{name} --type {namespace}")
                    };
                    tree.problems.insert(
                        key,
                        DefinitionErr::new(
                            path,
                            format!("pack item is excluded; run `rimz packs include {target}`"),
                        ),
                    );
                }
                continue;
            }
            let keys: Vec<_> = item
                .selected()
                .then(|| name.clone())
                .into_iter()
                .chain(qualified)
                .collect();
            let result = (|| {
                if kind_base.is_some()
                    && let Some(previous) = base_paths.insert(name.clone(), path.clone())
                {
                    bases.remove(name);
                    pending.retain(|base| base.keys[0] != *name);
                    loaded
                        .failed
                        .entry(name.clone())
                        .or_default()
                        .insert(previous.clone());
                    return Err(DefinitionErr::new(
                        path,
                        format!(
                            "kind base for {name} is declared by both {} and {}; keep one",
                            previous.display(),
                            path.display()
                        ),
                    ));
                }
                if let Some(definition) = kind_base
                    && !definition.spec().launch.takes_system_prompt()
                {
                    return Err(DefinitionErr::new(
                        path,
                        format!(
                            "{name} takes no system prompt, so this kind base can never reach a \
                             launch; delete {}",
                            path.display()
                        ),
                    ));
                }
                let text = catalog
                    .read_text(path)
                    .map_err(|error| DefinitionErr::new(path, error.to_string()))?;
                let (yaml, body) = frontmatter::split(path, &text)?;
                if let Some(definition) = kind_base {
                    let kind = definition.spec().kind;
                    let fm: AgentFrontmatter = frontmatter::parse(path, yaml)?;
                    fm.check_plus()
                        .map_err(|message| DefinitionErr::new(path, message))?;
                    if fm.tier.is_some() {
                        return Err(DefinitionErr::new(
                            path,
                            "sets `tier:`, but a kind base runs on its own kind; name a model in \
                             `model:`, or set the tier on a profile",
                        ));
                    }
                    if let Some(agent) = fm.agent.as_deref()
                        && crate::agents::find_definition(agent)
                            .is_none_or(|named| named.spec().kind != kind)
                    {
                        let own = std::iter::once(kind)
                            .chain(definition.spec().aliases.iter().copied())
                            .collect::<Vec<_>>()
                            .join(" or ");
                        return Err(DefinitionErr::new(
                            path,
                            format!(
                                "names `agent: {agent}`, but a kind base runs on its own kind; \
                                 `agent:` may name only {own}"
                            ),
                        ));
                    }
                    if let Some(tier) = fm
                        .model
                        .as_deref()
                        .and_then(super::tiers::ModelTier::from_model)
                    {
                        return Err(DefinitionErr::new(
                            path,
                            format!(
                                "model is a tier; name a model, or set `tier: {tier}` on a profile"
                            ),
                        ));
                    }
                    if let Some(effort) = fm
                        .effort
                        .as_deref()
                        .filter(|effort| super::tiers::relative_effort(effort))
                    {
                        return Err(DefinitionErr::new(
                            path,
                            format!(
                                "effort {effort} is relative; set an absolute effort or omit it \
                                 for the chosen model's default"
                            ),
                        ));
                    }
                    if body.trim().is_empty() {
                        return Err(DefinitionErr::new(path, empty_kind_base(name)));
                    }
                    let tree = Namespace {
                        definitions: BTreeMap::from([(
                            name.clone(),
                            Definition {
                                path: path.clone(),
                                pack: item.pack.clone(),
                                body: body.trim().to_owned(),
                                frontmatter: fm.clone(),
                                extras: TomlExtras::default(),
                            },
                        )]),
                        ..Namespace::default()
                    };
                    let own_base = BTreeMap::from([(name.clone(), fm.clone())]);
                    let base_ctx = Arc::new(LoadContext {
                        shared: Arc::new(SharedLoadState {
                            home: agents_home.to_owned(),
                            bases: own_base,
                            failed_bases: BTreeMap::new(),
                            skills: Arc::clone(&catalog),
                            tiers: tiers.clone(),
                            auto_compact_default,
                        }),
                        subagents: None,
                    });
                    let scope = LoadScope {
                        context: &base_ctx,
                        traits: &sources.claims["traits"],
                    };
                    let row = |namespace: &'static str| {
                        agent::Resolver::new(scope, namespace, [&tree, &tree]).kind_base(name)
                    };
                    let rows = (row("agents")?, row("subagents")?);
                    bases.insert(name.clone(), fm);
                    pending.push(PendingBase {
                        item,
                        keys: keys.clone(),
                        rows,
                    });
                    return Ok(());
                }
                if !is_safe_name(name) {
                    return Err(DefinitionErr::new(
                        path,
                        format!("has unsafe definition name {name:?}"),
                    ));
                }
                if crate::agents::find_runtime(name).is_some() {
                    return Err(DefinitionErr::new(
                        path,
                        "takes the name of an agent runtime; rename it",
                    ));
                }
                if item.pack.is_some() {
                    crate::harness::spec::validate_config(
                        &ProfilesConfig(BTreeMap::from([(name.clone(), agent::empty_profile(""))])),
                        &CommandsConfig::default(),
                        &super::AgentsConfig::default().teams,
                    )
                    .map_err(|error| item.error(error))?;
                }
                for key in &keys {
                    if let Some(previous) = names.get(key) {
                        tree.failed.insert(key.clone());
                        loaded
                            .failed
                            .entry(key.clone())
                            .or_default()
                            .insert(previous.clone());
                        return Err(DefinitionErr::new(
                            path,
                            format!(
                                "definition '{name}' is declared by both {} and {}",
                                previous.display(),
                                path.display()
                            ),
                        ));
                    }
                    names.insert(key.clone(), path.clone());
                }
                let fm: AgentFrontmatter = frontmatter::parse(path, yaml)?;
                fm.check_plus()
                    .map_err(|message| DefinitionErr::new(path, message))?;
                frontmatter::description(path, fm.description.as_deref())?;
                let definition = Definition {
                    path: path.clone(),
                    pack: item.pack.clone(),
                    body: body.trim().to_owned(),
                    frontmatter: fm,
                    extras: TomlExtras::default(),
                };
                for key in &keys {
                    tree.definitions.insert(key.clone(), definition.clone());
                }
                Ok(())
            })();
            if let Err(error) = result {
                let failed = Failed {
                    tree: &mut *tree,
                    failed_bases: &mut failed_bases,
                    loaded: &mut loaded,
                };
                failed.record(item, keys, kind_base.is_some(), error);
            }
        }
    }
    // A base's `subagents:` names a subagent that loads. A base that names a failed one fails,
    // which fails its kind's subagents in turn, so this settles before either tree resolves.
    // A subagent's first error here predates any refusal its own failure causes.
    let mut first_errors: Vec<DefinitionErr> = Vec::new();
    while pending.iter().any(|base| base.rows.0.subagents.is_some()) {
        let probe_ctx = Arc::new(LoadContext {
            shared: Arc::new(SharedLoadState {
                home: agents_home.to_owned(),
                bases: bases.clone(),
                failed_bases: failed_bases.clone(),
                skills: Arc::clone(&catalog),
                tiers: tiers.clone(),
                auto_compact_default,
            }),
            subagents: None,
        });
        let scope = LoadScope {
            context: &probe_ctx,
            traits: &sources.claims["traits"],
        };
        let (loadable, errors) = agent::loadable(agent::Resolver::new(
            scope,
            "subagents",
            [&subagents, &agents],
        ));
        for error in errors {
            if first_errors.iter().all(|first| first.path != error.path) {
                first_errors.push(error);
            }
        }
        let children: BTreeSet<String> = loadable
            .into_iter()
            .chain(pending.iter().map(|base| base.keys[0].clone()))
            .chain(commands.0.keys().cloned())
            .collect();
        let subagent_check = SubagentCheck::from_namespace(&subagents, &children);
        let mut refused = Vec::new();
        for (index, base) in pending.iter_mut().enumerate() {
            let Some(listed) = &base.rows.0.subagents else {
                continue;
            };
            match agent::subagent_names(&base.item.path, listed, &subagent_check) {
                Ok(names) => base.rows.0.subagents = Some(names),
                Err(error) => refused.push((index, error)),
            }
        }
        if refused.is_empty() {
            break;
        }
        for (index, error) in refused.into_iter().rev() {
            let base = pending.remove(index);
            bases.remove(&base.keys[0]);
            let failed = Failed {
                tree: &mut agents,
                failed_bases: &mut failed_bases,
                loaded: &mut loaded,
            };
            failed.record(base.item, base.keys, true, error);
        }
    }
    for base in pending {
        let origin = DefinitionOrigin::new(base.item.pack.as_ref(), None);
        let name = &base.keys[0];
        loaded.insert("agents", name, &base.item.path, &origin, base.rows.0);
        loaded.insert("subagents", name, &base.item.path, &origin, base.rows.1);
    }
    // Only kind bases have been materialized yet; each family keeps its own base prompt.
    let families: BTreeSet<_> = tiers
        .entries(super::tiers::ModelTier::Intern)
        .filter_map(|(_, kind, _)| kind)
        .chain(
            crate::agents::known_kinds()
                .filter(|kind| crate::agents::runs_other_providers_models(kind)),
        )
        .collect();
    let mut exclusions = BTreeMap::new();
    for kind in &families {
        if loaded.agent_profiles.0.contains_key(*kind) {
            continue;
        }
        let message = match failed_bases.get(*kind) {
            Some(error) if error.is_conflict_over(kind) => error.message.clone(),
            Some(error) => failed_kind_base(&error.path),
            None => missing_kind_base(agents_home, kind),
        };
        exclusions.insert((*kind).to_owned(), message);
    }
    // Each table's rows route to that table's copy of each base.
    for profiles in [&mut loaded.agent_profiles, &mut loaded.subagent_profiles] {
        let renders: BTreeMap<_, _> = families
            .iter()
            .filter_map(|kind| Some(((*kind).to_owned(), profiles.0.get(*kind)?.clone())))
            .collect();
        for profile in profiles.0.values_mut() {
            profile.definition_renders = Some(super::tiers::DefinitionRenders {
                preference: super::tiers::TierPreference {
                    model: None,
                    family: Some(profile.agent.clone()),
                },
                renders: renders.clone(),
                exclusions: exclusions.clone(),
                effort: None,
            });
        }
    }
    // A public name belongs to exactly one tree; neither copy is loadable.
    for name in agents.definitions.keys() {
        if subagents.failed.contains(name) || subagents.definitions.contains_key(name) {
            agents.failed.insert(name.clone());
            subagents.failed.insert(name.clone());
        }
    }
    let shared = Arc::new(SharedLoadState {
        home: agents_home.to_owned(),
        bases,
        failed_bases,
        skills: Arc::clone(&catalog),
        tiers: tiers.clone(),
        auto_compact_default,
    });
    let subagents_ctx = Arc::new(LoadContext {
        shared: Arc::clone(&shared),
        subagents: None,
    });
    let subagents_scope = LoadScope {
        context: &subagents_ctx,
        traits: &sources.claims["traits"],
    };
    let resolved_from = loaded.errors.len();
    // Children resolve first so an agent's `subagents:` checks against what loaded.
    agent::resolve_namespace(
        agent::Resolver::new(subagents_scope, "subagents", [&subagents, &agents]),
        &mut loaded,
    );
    let children: BTreeSet<String> = loaded
        .subagent_profiles
        .0
        .keys()
        .chain(commands.0.keys())
        .cloned()
        .collect();
    let agents_ctx = Arc::new(LoadContext {
        shared,
        subagents: Some(Arc::new(SubagentCheck::from_namespace(
            &subagents, &children,
        ))),
    });
    loaded.contexts = Some(LoadContexts {
        agents: Arc::clone(&agents_ctx),
        subagents: Arc::clone(&subagents_ctx),
    });
    let agents_scope = LoadScope {
        context: &agents_ctx,
        traits: &sources.claims["traits"],
    };
    agent::resolve_namespace(
        agent::Resolver::new(agents_scope, "agents", [&agents, &subagents]),
        &mut loaded,
    );
    // Restore any first error recorded during the kind-base probe across both trees,
    // including a `subagents/` child's failed `agents/` parent.
    for error in &mut loaded.errors[resolved_from..] {
        if let Some(first) = first_errors.iter().find(|first| first.path == error.path) {
            *error = first.clone();
        }
    }
    // The resolver knows paths, not pack items; team::load appends its own fixes.
    for error in &mut loaded.errors[resolved_from..] {
        if let Some(item) = sources
            .items
            .iter()
            .find(|item| item.pack.is_some() && item.path == error.path)
        {
            error.message.push_str(&item.exclusion_fix());
        }
    }
    team::load(
        agents_scope,
        [&agents, &subagents],
        &sources,
        commands,
        &mut loaded,
    );
    loaded.failed.retain(|name, paths| {
        if crate::agents::find_runtime(name).is_some()
            && paths.iter().all(|path| {
                sources.items.iter().any(|item| {
                    item.pack.is_some() && item.path == *path && item.namespace() != "bases"
                })
            })
        {
            for error in loaded
                .errors
                .iter_mut()
                .filter(|error| paths.contains(&error.path))
            {
                error.cause = DefinitionCause::PackProblem;
            }
            return false;
        }
        !loaded.agent_profiles.0.contains_key(name)
            && !loaded.subagent_profiles.0.contains_key(name)
            && !loaded.teams.0.contains_key(name)
    });
    let owned: BTreeSet<_> = loaded.failed.values().flatten().collect();
    for error in &mut loaded.errors {
        if !owned.contains(&error.path)
            && sources
                .items
                .iter()
                .any(|item| item.pack.is_some() && item.path == error.path)
        {
            error.cause = DefinitionCause::PackProblem;
        }
    }
    loaded.list_tuning();
    loaded.pack_items = sources.pack_items(&loaded, &catalog);
    loaded
}

impl LoadedDefinitions {
    fn list_tuning(&mut self) {
        for row in &mut self.rows {
            let profiles = if row.namespace == "subagents" {
                &self.subagent_profiles
            } else {
                &self.agent_profiles
            };
            (row.effort, row.fast) = crate::harness::spec::listed_tuning(&row.name, profiles);
        }
    }

    fn insert(
        &mut self,
        namespace: &str,
        name: &str,
        source: &Path,
        origin: &DefinitionOrigin,
        profile: Profile,
    ) {
        self.rows.push(DefinitionRow {
            keep_warm: profile
                .keep_warm
                .as_ref()
                .and_then(|policy| policy.fields().max)
                .filter(|max| *max != super::KeepWarm::Off)
                .map(|max| super::KeepWarmFields {
                    max: Some(max),
                    while_clause: None,
                }),
            name: name.to_owned(),
            namespace: namespace.to_owned(),
            kind: profile.agent.clone(),
            model: profile.model.clone(),
            tier: profile.model_tier.as_ref().map(|tier| tier.tier),
            tier_fallback: profile.model_tier.as_ref().map(|tier| tier.fell_back),
            // Set by `list_tuning` once every definition, its kind base included, has loaded.
            effort: None,
            fast: false,
            source: source.to_owned(),
            origin: origin.clone(),
            team: None,
            role: None,
            owns: Vec::new(),
            signals: Vec::new(),
        });
        let (profiles, sources) = if namespace == "agents" {
            (&mut self.agent_profiles, &mut self.sources.agent_profiles)
        } else {
            (
                &mut self.subagent_profiles,
                &mut self.sources.subagent_profiles,
            )
        };
        profiles.0.insert(name.to_owned(), profile);
        sources.insert(
            name.to_owned(),
            ProfileSource {
                path: source.to_owned(),
                origin: origin.clone(),
            },
        );
    }
}
