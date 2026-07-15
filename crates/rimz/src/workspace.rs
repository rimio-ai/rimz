//! Project-root and worktree-root resolution.
//!
//! Workspace identity is keyed on the canonical root directory. The resolver
//! ladder picks the richest root the starting path offers: the *repo* root for
//! a git checkout (the parent of `git rev-parse --git-common-dir`, so every
//! worktree of the same repo shares one workspace; submodules get their own),
//! a marker directory ([`PROJECT_MARKERS`]) for a non-git project, and the
//! directory itself as the last tier — a first-class directory workspace.
//!
//! Identity is then *pinned per session*: session birth stamps
//! [`ENV_WORKSPACE_ID`]/[`ENV_PROJECT_ROOT`] into the mux environment, and
//! participating commands (hooks, statusline helpers) resolve by ownership.
//! Pane-owned participants use `--root`, the verified env pin, a recovered
//! sibling pin, then the static ladder, so an agent in a nested repo inside a
//! directory workspace still writes to the room it lives in. Daemon-owned
//! hooks use `--root`, a recovered sibling pin, then the static ladder. Their
//! shared daemon's environment belongs to whichever context launched it and
//! may carry a valid pin for an unrelated room, so that ambient pin is
//! excluded. Room-choosing commands (`rimz start`/`attach`) resolve statically via
//! [`WorkspaceResolver::resolve`], keeping a deliberate per-repo room one
//! `rimz start` away from inside a parent room. See `docs/internals` and
//! `DESIGN.md` for the rules this implements.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::ids::{MuxName, WorkspaceId};
use crate::store::workspace_record::{self, WorkspaceRecord};

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceErr {
    #[error("could not resolve workspace from {path}: {reason}")]
    Resolve { path: PathBuf, reason: String },
    #[error("git probe failed: {0}")]
    GitProbe(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, WorkspaceErr>;

/// Environment key carrying the session's pinned workspace id.
pub const ENV_WORKSPACE_ID: &str = "RIMZ_WORKSPACE_ID";
/// Environment key carrying the session's pinned project root.
pub const ENV_PROJECT_ROOT: &str = "RIMZ_PROJECT_ROOT";

/// The identity pin a RimZ session stamps into the mux environment at birth,
/// inherited by every pane and so by every agent and its hook children.
pub fn pin_env(workspace_id: &WorkspaceId, project_root: &Path) -> BTreeMap<String, String> {
    BTreeMap::from([
        (ENV_WORKSPACE_ID.to_owned(), workspace_id.to_string()),
        (
            ENV_PROJECT_ROOT.to_owned(),
            project_root.display().to_string(),
        ),
    ])
}

/// Which ladder tier produced a workspace root. The class describes the root
/// itself, not how it was selected — an explicit `--root` into a git checkout
/// still classifies as [`RootClass::Repo`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootClass {
    /// A git repository; worktrees of the repo collapse to one workspace.
    Repo,
    /// A directory carrying a project marker ([`PROJECT_MARKERS`]).
    Marker,
    /// Any other directory — a directory workspace.
    Directory,
}

impl RootClass {
    /// The user-facing class label `rimz start` notices and `rimz doctor`
    /// speak — matching the serialized form.
    pub fn label(self) -> &'static str {
        match self {
            Self::Repo => "repo",
            Self::Marker => "marker",
            Self::Directory => "directory",
        }
    }
}

/// What resolution produces: the IDs the rest of the system uses.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolvedWorkspace {
    pub workspace_id: WorkspaceId,
    pub project_root: PathBuf,
    pub root_class: RootClass,
    pub worktree_root: PathBuf,
    pub worktree_branch: Option<String>,
    pub session_name: String,
    pub mux_hint: Option<MuxName>,
}

/// A workspace discovered by scanning the state dir, paired with the mux session
/// it was last started under. Read straight from `workspace.json`, so it needs
/// neither a cwd nor a running session — the cwd-independent basis shared by
/// `rimz list` and the user-wide `rimz reload`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownWorkspace {
    pub workspace_id: WorkspaceId,
    pub project_root: PathBuf,
    pub session_name: String,
    pub root_class: RootClass,
    pub rimz_bin: Option<PathBuf>,
}

/// True when `inner` is `outer` itself or nested under it, compared by path
/// components so `/home/userX` is never read as under `/home/user`. A
/// lexical test on recorded roots — no filesystem access — shared by the
/// `rimz start` overlap notice and the doctor room tree.
pub fn root_contains(outer: &Path, inner: &Path) -> bool {
    let mut outer_components = outer.components();
    let mut inner_components = inner.components();
    loop {
        match (outer_components.next(), inner_components.next()) {
            (Some(o), Some(i)) if o == i => continue,
            (Some(_), _) => return false,
            (None, _) => return true,
        }
    }
}

/// Every workspace with a readable, current `workspace.json` under the state dir,
/// deduplicated by session name with the newest record winning. A directory
/// missing its record is skipped quietly (half-removed or never finished); a
/// record that exists but won't parse is logged and skipped. A stale record whose
/// canonical project root now maps to another workspace id is skipped so
/// maintenance commands operate on the current workspace record only. Errors only
/// when the state root itself cannot be read.
pub fn known_workspaces() -> io::Result<Vec<KnownWorkspace>> {
    known_workspaces_under(&crate::store::paths::workspaces_dir())
}

/// [`known_workspaces`] over an explicit state root, for tests against a tempdir.
pub fn known_workspaces_under(workspaces_root: &Path) -> io::Result<Vec<KnownWorkspace>> {
    use crate::store::workspace_record::WorkspaceRecordErr;

    let entries = match std::fs::read_dir(workspaces_root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut by_session: BTreeMap<String, KnownWorkspaceCandidate> = BTreeMap::new();
    for entry in entries {
        let path = entry?.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        let Ok(workspace_id) = WorkspaceId::parse(name) else {
            continue;
        };
        let record_path = path.join("workspace.json");
        match workspace_record::read(&record_path) {
            Ok(record) => {
                let Some(candidate) =
                    normalize_known_workspace_record(workspace_id, &record_path, record)
                else {
                    continue;
                };
                by_session
                    .entry(candidate.workspace.session_name.clone())
                    .and_modify(|current| {
                        if candidate.updated_at > current.updated_at {
                            *current = candidate.clone();
                        }
                    })
                    .or_insert(candidate);
            }
            // A dir without a record isn't a usable workspace; `rimz gc`
            // reaps it. A record that won't parse is a real anomaly — surface it.
            Err(WorkspaceRecordErr::Io { source, .. })
                if source.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                tracing::warn!(workspace = %workspace_id, error = %err, "skipping workspace with unreadable record");
            }
        }
    }
    Ok(by_session
        .into_values()
        .map(|candidate| candidate.workspace)
        .collect())
}

#[derive(Clone)]
struct KnownWorkspaceCandidate {
    workspace: KnownWorkspace,
    updated_at: jiff::Timestamp,
}

fn normalize_known_workspace_record(
    workspace_id: WorkspaceId,
    record_path: &Path,
    mut record: WorkspaceRecord,
) -> Option<KnownWorkspaceCandidate> {
    match record.project_root.canonicalize() {
        Ok(project_root) => {
            let canonical_id = WorkspaceId::from_project_root(&project_root);
            let session_name = session_name_for(&project_root);
            if canonical_id != workspace_id {
                tracing::debug!(
                    workspace = %workspace_id,
                    canonical_workspace = %canonical_id,
                    path = %record_path.display(),
                    "skipping stale workspace record whose canonical root belongs to another workspace",
                );
                return None;
            }

            if record.workspace_id != workspace_id
                || record.project_root != project_root
                || record.session_name != session_name
            {
                record.workspace_id = workspace_id.clone();
                record.project_root = project_root;
                record.session_name = session_name;
                record.updated_at = jiff::Timestamp::now();
                if let Err(err) = workspace_record::write_path(record_path, &record) {
                    tracing::warn!(
                        path = %record_path.display(),
                        error = %err,
                        "repairing workspace record failed; using repaired value in memory",
                    );
                }
            }
        }
        Err(_) => {
            if record.workspace_id != workspace_id {
                tracing::warn!(
                    workspace = %workspace_id,
                    recorded_workspace = %record.workspace_id,
                    path = %record_path.display(),
                    "skipping workspace record whose id does not match its directory",
                );
                return None;
            }
        }
    }

    Some(KnownWorkspaceCandidate {
        workspace: KnownWorkspace {
            workspace_id,
            project_root: record.project_root,
            session_name: record.session_name,
            root_class: record.root_class,
            rimz_bin: record.rimz_bin,
        },
        updated_at: record.updated_at,
    })
}

/// Resolve the room-owning RimZ binary recorded for session-local helpers.
/// Missing or removed records fall back to the current executable so legacy
/// rooms and unowned test fixtures keep working.
pub fn resolve_recorded_rimz_bin(recorded: Option<&Path>) -> PathBuf {
    match recorded {
        Some(path) if path.is_file() => path.to_path_buf(),
        Some(path) => {
            tracing::debug!(
                rimz_bin = %path.display(),
                "recorded RimZ binary is unavailable; falling back to current executable",
            );
            crate::proc::rimz_exe()
        }
        None => crate::proc::rimz_exe(),
    }
}

/// Markers that signal "this directory is a project root" for non-git projects.
const PROJECT_MARKERS: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
    "flake.nix",
    "deno.json",
    "bun.lock",
    "pnpm-workspace.yaml",
    ".rimz/config.toml",
    ".hg",
    ".svn",
];

pub struct WorkspaceResolver;

/// Who is resolving: a command choosing a room, or one participating in the
/// room it already lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResolveMode {
    /// Room choice (`rimz start`/`attach`, maintenance): the static ladder
    /// only, so a deliberate per-repo room can be created from inside a
    /// parent room. The directory tier accepts any directory.
    Create,
    /// Room participation (hooks, statusline): the
    /// session's env pin wins over the static ladder, so a pane's writes land
    /// in the room it lives in. Never refuses — a hook on the agent's
    /// critical path degrades to the static ladder, never errors on identity.
    Participate,
    /// Daemon-owned hook participation: the shared daemon's environment may
    /// belong to an unrelated room, so only a recovered sibling pin precedes
    /// the static ladder. An explicit root still wins. Never refuses, like
    /// [`Self::Participate`].
    ParticipateDaemon,
}

/// Process-environment lookup, injected so the pin and `$HOME` reads unit-test
/// without mutating real env (which `forbid(unsafe_code)` rules out anyway).
type EnvReader<'a> = &'a dyn Fn(&str) -> Option<std::ffi::OsString>;

/// Verified pin roots harvested from sibling agent processes at a cwd,
/// injected like [`EnvReader`] so recovery unit-tests without `/proc`. The
/// caller owns process discovery and per-candidate verification
/// ([`verify_pin`]); the resolver owns the agreement rule.
pub type PinScan<'a> = &'a dyn Fn(&Path) -> Vec<PathBuf>;

/// The no-op scan every pin-recovery-free resolution passes.
const NO_SCAN: PinScan<'static> = &|_| Vec::new();

impl WorkspaceResolver {
    /// Resolve a room choice from a starting path. `root_override` corresponds
    /// to the `--root` CLI flag and `[workspace] root` in `.rimz/config.toml`.
    pub fn resolve(
        start: impl AsRef<Path>,
        root_override: Option<PathBuf>,
    ) -> Result<ResolvedWorkspace> {
        Self::resolve_with(
            ResolveMode::Create,
            start.as_ref(),
            root_override,
            &|key: &str| std::env::var_os(key),
            NO_SCAN,
        )
    }

    /// Resolve on behalf of a participant in a live room: the session's
    /// verified env pin ([`ENV_WORKSPACE_ID`]/[`ENV_PROJECT_ROOT`]) beats the
    /// static ladder; an explicit `root_override` beats both.
    pub fn resolve_participant(
        start: impl AsRef<Path>,
        root_override: Option<PathBuf>,
    ) -> Result<ResolvedWorkspace> {
        Self::resolve_with(
            ResolveMode::Participate,
            start.as_ref(),
            root_override,
            &|key: &str| std::env::var_os(key),
            NO_SCAN,
        )
    }

    /// [`Self::resolve_participant`] with sibling-pin recovery for a pane-owned
    /// hook. The verified env pin still wins; otherwise `scan` recovers the
    /// pin from sibling agent processes at the hook's cwd, adopted only when
    /// every verified candidate names one root. The static ladder remains the
    /// floor.
    pub fn resolve_participant_with_pin_recovery(
        start: impl AsRef<Path>,
        root_override: Option<PathBuf>,
        scan: PinScan,
    ) -> Result<ResolvedWorkspace> {
        Self::resolve_with(
            ResolveMode::Participate,
            start.as_ref(),
            root_override,
            &|key: &str| std::env::var_os(key),
            scan,
        )
    }

    /// Resolve a daemon-owned hook without consulting its ambient env pin.
    /// The shared daemon's environment belongs to its launcher and may carry
    /// a valid pin for an unrelated room. A unanimous recovered sibling pin
    /// wins over the static ladder; an explicit `root_override` wins over both.
    pub fn resolve_daemon_participant_with_pin_recovery(
        start: impl AsRef<Path>,
        root_override: Option<PathBuf>,
        scan: PinScan,
    ) -> Result<ResolvedWorkspace> {
        Self::resolve_with(
            ResolveMode::ParticipateDaemon,
            start.as_ref(),
            root_override,
            &|key: &str| std::env::var_os(key),
            scan,
        )
    }

    fn resolve_with(
        mode: ResolveMode,
        start_in: &Path,
        root_override: Option<PathBuf>,
        env: EnvReader,
        scan: PinScan,
    ) -> Result<ResolvedWorkspace> {
        let start = start_in
            .canonicalize()
            .unwrap_or_else(|_| start_in.to_path_buf());

        let (project_root, worktree_root, root_class) = if let Some(root) = root_override {
            let root = root.canonicalize().unwrap_or(root);
            let class = classify_root(&root)?;
            (root.clone(), root, class)
        } else if let Some(pinned) = match mode {
            ResolveMode::Create => None,
            ResolveMode::Participate => {
                read_verified_pin(env).or_else(|| recover_pinned_root(&start, scan))
            }
            ResolveMode::ParticipateDaemon => recover_pinned_root(&start, scan),
        } {
            // The pane lives in a session that already chose a root; the cwd
            // still names the worktree the participant works in, for grouping.
            let worktree_root = match resolve_git(&start)? {
                Some((_, worktree)) => worktree,
                None => resolve_marker(&start).unwrap_or_else(|| start.clone()),
            };
            let class = classify_root(&pinned)?;
            (pinned, worktree_root, class)
        } else if let Some((project_root, worktree_root)) = resolve_git(&start)? {
            (project_root, worktree_root, RootClass::Repo)
        } else if let Some(marker) = resolve_marker(&start) {
            (marker.clone(), marker, RootClass::Marker)
        } else {
            (start.clone(), start.clone(), RootClass::Directory)
        };

        let workspace_id = WorkspaceId::from_project_root(&project_root);
        let session_name = session_name_for(&project_root);
        let worktree_branch = current_branch(&worktree_root)?;

        Ok(ResolvedWorkspace {
            workspace_id,
            project_root,
            root_class,
            worktree_root,
            worktree_branch,
            session_name,
            mux_hint: None,
        })
    }
}

/// Read and verify the session's identity pin from the participant's own
/// environment. `None` silently falls through to recovery or the ladder.
fn read_verified_pin(env: EnvReader) -> Option<PathBuf> {
    let id = env(ENV_WORKSPACE_ID)?.to_string_lossy().into_owned();
    let root = PathBuf::from(env(ENV_PROJECT_ROOT)?);
    verify_pin(&id, &root)
}

/// Verify one identity pin: `None` unless the id parses, the root exists, and
/// the id is the hash of that root, so a stale or corrupt pin never misroutes
/// a write into the wrong store. The single validation path for the env pin
/// and every sibling-process candidate a [`PinScan`] yields.
pub fn verify_pin(id: &str, root: &Path) -> Option<PathBuf> {
    let Ok(id) = WorkspaceId::parse(id) else {
        tracing::warn!(pin = %id, "ignoring unparseable workspace pin");
        return None;
    };
    let Ok(root) = root.canonicalize() else {
        tracing::warn!(pin = %id, root = %root.display(), "ignoring workspace pin with a vanished root");
        return None;
    };
    if WorkspaceId::from_project_root(&root) != id {
        tracing::warn!(pin = %id, root = %root.display(), "ignoring workspace pin whose id does not hash from its root");
        return None;
    }
    Some(root)
}

/// Recover the pin from sibling agent processes when the participant cannot
/// use its own environment. Adopts the root iff every verified candidate names
/// the same one; an empty or split scan falls through to the static ladder.
fn recover_pinned_root(start: &Path, scan: PinScan) -> Option<PathBuf> {
    let mut roots = scan(start);
    roots.sort();
    roots.dedup();
    match roots.as_slice() {
        [] => None,
        [root] => Some(root.clone()),
        _ => {
            tracing::warn!(
                cwd = %start.display(),
                candidates = roots.len(),
                "ignoring sibling pins that disagree on the room"
            );
            None
        }
    }
}

/// Classify a root that resolution did not derive itself (an explicit
/// `--root`, the env pin): the richest tier the directory satisfies.
fn classify_root(root: &Path) -> Result<RootClass> {
    if git_output(root, ["rev-parse", "--show-toplevel"])?.is_some() {
        return Ok(RootClass::Repo);
    }
    if PROJECT_MARKERS.iter().any(|m| root.join(m).exists()) {
        return Ok(RootClass::Marker);
    }
    Ok(RootClass::Directory)
}

fn resolve_git(start: &Path) -> Result<Option<(PathBuf, PathBuf)>> {
    let Some(worktree) = git_output(start, ["rev-parse", "--show-toplevel"])? else {
        return Ok(None);
    };
    let worktree_root = PathBuf::from(worktree);

    let common_dir = git_output(start, ["rev-parse", "--git-common-dir"])?.ok_or_else(|| {
        WorkspaceErr::Resolve {
            path: start.to_path_buf(),
            reason: "git common dir not found".to_owned(),
        }
    })?;
    let common_dir_path = PathBuf::from(common_dir);
    let common_dir_abs = if common_dir_path.is_absolute() {
        common_dir_path
    } else {
        worktree_root.join(common_dir_path)
    };
    let common_dir_abs = common_dir_abs.canonicalize().unwrap_or(common_dir_abs);

    let project_root = if common_dir_abs.file_name() == Some(OsStr::new(".git")) {
        common_dir_abs
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| WorkspaceErr::Resolve {
                path: start.to_path_buf(),
                reason: "git common dir has no parent".to_owned(),
            })?
    } else {
        // Worktree common-dir lives at `<repo>/.git/worktrees/<name>` or similar;
        // walk up until we find the repo root (parent of `.git`).
        common_dir_abs
            .ancestors()
            .find_map(|p| {
                let candidate = p.file_name();
                if candidate == Some(OsStr::new(".git")) {
                    p.parent().map(Path::to_path_buf)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| worktree_root.clone())
    };

    Ok(Some((project_root, worktree_root)))
}

fn git_output<const N: usize>(cwd: &Path, args: [&str; N]) -> Result<Option<String>> {
    let output = match Command::new("git").args(args).current_dir(cwd).output() {
        Ok(output) => output,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    if !output.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if text.is_empty() {
        Ok(None)
    } else {
        Ok(Some(text))
    }
}

fn current_branch(worktree_root: &Path) -> Result<Option<String>> {
    let Some(branch) = git_output(worktree_root, ["rev-parse", "--abbrev-ref", "HEAD"])? else {
        return Ok(None);
    };
    if branch == "HEAD" || branch.is_empty() {
        Ok(None)
    } else {
        Ok(Some(branch))
    }
}

fn resolve_marker(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| PROJECT_MARKERS.iter().any(|m| dir.join(m).exists()))
        .map(Path::to_path_buf)
}

const SESSION_BASENAME_SLUG_MAX: usize = 8;

fn session_name_for(project_root: &Path) -> String {
    let basename = project_root
        .file_name()
        .map(|name| name.to_string_lossy())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "root".into());
    let slug: String = basename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '-'
            }
        })
        // Collapse runs of separators (leading slash, spaces, `/`) into one `-`.
        .fold(String::new(), |mut acc, c| {
            if c == '-' && acc.ends_with('-') {
                return acc;
            }
            acc.push(c);
            acc
        });
    let slug = slug
        .trim_matches('-')
        .chars()
        .take(SESSION_BASENAME_SLUG_MAX)
        .collect::<String>();
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "root" } else { slug };
    let workspace_id = WorkspaceId::from_project_root(project_root);
    let hash = workspace_id
        .as_str()
        .strip_prefix("ws_")
        .unwrap_or(workspace_id.as_str());
    format!("rimz-{slug}-{}", &hash[..6])
}

#[cfg(test)]
mod tests;
