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
//! participating commands (hooks, `rimz event`/`feed`, statusline helpers)
//! resolve through [`WorkspaceResolver::resolve_participant`], which honors
//! that pin before the static ladder — so an agent in a nested repo inside a
//! directory workspace still writes to the room it lives in. Room-choosing
//! commands (`rimz start`/`attach`) resolve statically via
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
use crate::ledger::workspace_record::{self, WorkspaceRecord};

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceErr {
    #[error("could not resolve workspace from {path}: {reason}")]
    Resolve { path: PathBuf, reason: String },
    #[error(
        "refusing a directory workspace rooted at {root} ({why}); cd into a project, or force it with --root {root}"
    )]
    RefusedRoot { root: PathBuf, why: &'static str },
    #[error("git probe failed: {0}")]
    GitProbe(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, WorkspaceErr>;

/// Environment key carrying the session's pinned workspace id.
pub const ENV_WORKSPACE_ID: &str = "RIMZ_WORKSPACE_ID";
/// Environment key carrying the session's pinned project root.
pub const ENV_PROJECT_ROOT: &str = "RIMZ_PROJECT_ROOT";

/// The identity pin a Rimz session stamps into the mux environment at birth,
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
}

/// True when `inner` is `outer` itself or nested under it, compared by path
/// components so `/home/marvinX` is never read as under `/home/marvin`. A
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
    known_workspaces_under(&crate::ledger::paths::workspaces_dir())
}

/// [`known_workspaces`] over an explicit state root, for tests against a tempdir.
pub fn known_workspaces_under(workspaces_root: &Path) -> io::Result<Vec<KnownWorkspace>> {
    use crate::ledger::workspace_record::WorkspaceRecordErr;

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
        },
        updated_at: record.updated_at,
    })
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
    /// parent room. The directory tier refuses pathological roots here.
    Create,
    /// Room participation (hooks, `rimz event`/`feed`, statusline): the
    /// session's env pin wins over the static ladder, so a pane's writes land
    /// in the room it lives in. Never refuses — a hook on the agent's
    /// critical path degrades to the static ladder, never errors on identity.
    Participate,
}

/// Process-environment lookup, injected so the pin and `$HOME` reads unit-test
/// without mutating real env (which `forbid(unsafe_code)` rules out anyway).
type EnvReader<'a> = &'a dyn Fn(&str) -> Option<std::ffi::OsString>;

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
        )
    }

    fn resolve_with(
        mode: ResolveMode,
        start_in: &Path,
        root_override: Option<PathBuf>,
        env: EnvReader,
    ) -> Result<ResolvedWorkspace> {
        let start = start_in
            .canonicalize()
            .unwrap_or_else(|_| start_in.to_path_buf());

        let (project_root, worktree_root, root_class) = if let Some(root) = root_override {
            let root = root.canonicalize().unwrap_or(root);
            let class = classify_root(&root)?;
            (root.clone(), root, class)
        } else if let Some(pinned) = (mode == ResolveMode::Participate)
            .then(|| read_verified_pin(env))
            .flatten()
        {
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
            if mode == ResolveMode::Create {
                refuse_pathological_root(&start, env)?;
            }
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

/// Read and verify the session's identity pin. `None` — silently falling
/// through to the static ladder — unless the id parses, the root exists, and
/// the id is the hash of that root, so a stale or corrupt env never misroutes
/// a write into the wrong ledger.
fn read_verified_pin(env: EnvReader) -> Option<PathBuf> {
    let id = env(ENV_WORKSPACE_ID)?.to_string_lossy().into_owned();
    let root = env(ENV_PROJECT_ROOT)?;
    let Ok(id) = WorkspaceId::parse(&id) else {
        tracing::warn!(pin = %id, "ignoring unparseable workspace pin");
        return None;
    };
    let root = PathBuf::from(root);
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

/// The directory tier refuses the two roots that are almost certainly an
/// accident — `$HOME` and the filesystem root — with the fix in the error;
/// `--root` bypasses by selecting a different ladder tier.
fn refuse_pathological_root(start: &Path, env: EnvReader) -> Result<()> {
    if start == Path::new("/") {
        return Err(WorkspaceErr::RefusedRoot {
            root: start.to_path_buf(),
            why: "the filesystem root",
        });
    }
    if let Some(home) = env("HOME") {
        let home = PathBuf::from(home);
        let home = home.canonicalize().unwrap_or(home);
        if start == home {
            return Err(WorkspaceErr::RefusedRoot {
                root: start.to_path_buf(),
                why: "your home directory",
            });
        }
    }
    Ok(())
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

fn session_name_for(project_root: &Path) -> String {
    let slug: String = project_root
        .to_string_lossy()
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
    let slug = slug.trim_matches('-');
    format!("rimz-{slug}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_name_slugs_the_full_path() {
        assert_eq!(
            session_name_for(Path::new("/home/marvin/xxx")),
            "rimz-home-marvin-xxx",
        );
    }

    #[test]
    fn known_workspaces_reads_records_and_skips_recordless_dirs() {
        use crate::ledger::paths::{StatePaths, workspaces_dir_under};
        use crate::ledger::workspace_record::{self, WorkspaceRecord};

        let dir = tempfile::TempDir::new().expect("tempdir");
        let state_root = dir.path();
        let root = workspaces_dir_under(state_root);

        // Two workspaces with records, written through the canonical path.
        for project in ["/home/marvin/alpha", "/home/marvin/beta"] {
            let project_root = std::path::PathBuf::from(project);
            let workspace_id = WorkspaceId::from_project_root(&project_root);
            let paths = StatePaths::under(workspace_id.clone(), state_root).expect("state paths");
            std::fs::create_dir_all(&paths.root).expect("mkdir workspace");
            workspace_record::write(
                &paths,
                &WorkspaceRecord {
                    workspace_id,
                    project_root: project_root.clone(),
                    session_name: session_name_for(&project_root),
                    root_class: RootClass::Repo,
                    updated_at: jiff::Timestamp::UNIX_EPOCH,
                },
            )
            .expect("write record");
        }
        // A directory whose name isn't a workspace id, and a workspace dir with no
        // record, are both skipped silently.
        std::fs::create_dir_all(root.join("not-a-workspace-id")).expect("mkdir junk");

        let mut sessions: Vec<String> = known_workspaces_under(&root)
            .expect("enumerate")
            .into_iter()
            .map(|ws| ws.session_name)
            .collect();
        sessions.sort();
        assert_eq!(
            sessions,
            vec![
                "rimz-home-marvin-alpha".to_owned(),
                "rimz-home-marvin-beta".to_owned(),
            ],
        );
    }

    #[test]
    fn known_workspaces_repairs_record_fields_for_the_canonical_workspace_dir() {
        use crate::ledger::paths::{StatePaths, workspaces_dir_under};
        use crate::ledger::workspace_record::{self, WorkspaceRecord};

        let dir = tempfile::TempDir::new().expect("tempdir");
        let state_root = dir.path().join("state");
        let project_root = dir.path().join("project");
        std::fs::create_dir_all(&project_root).expect("mkdir project");

        let canonical_root = project_root.canonicalize().expect("canonical project");
        let noncanonical_root = project_root.join("..").join("project");
        let workspace_id = WorkspaceId::from_project_root(&canonical_root);
        let paths = StatePaths::under(workspace_id.clone(), &state_root).expect("state paths");
        std::fs::create_dir_all(&paths.root).expect("mkdir workspace");
        workspace_record::write(
            &paths,
            &WorkspaceRecord {
                workspace_id: workspace_id.clone(),
                project_root: noncanonical_root,
                session_name: "rimz-stale".to_owned(),
                root_class: RootClass::Repo,
                updated_at: jiff::Timestamp::UNIX_EPOCH,
            },
        )
        .expect("write stale record");

        let known = known_workspaces_under(&workspaces_dir_under(&state_root)).expect("enumerate");
        assert_eq!(known.len(), 1);
        assert_eq!(known[0].workspace_id, workspace_id);
        assert_eq!(known[0].project_root, canonical_root);
        assert_eq!(known[0].session_name, session_name_for(&project_root));

        let repaired = workspace_record::read(&paths.workspace_record).expect("read repaired");
        assert_eq!(repaired.workspace_id, workspace_id);
        assert_eq!(repaired.project_root, project_root.canonicalize().unwrap());
        assert_eq!(repaired.session_name, session_name_for(&project_root));
    }

    #[test]
    fn known_workspaces_skips_obsolete_noncanonical_duplicate_records() {
        use crate::ledger::paths::{StatePaths, workspaces_dir_under};
        use crate::ledger::workspace_record::{self, WorkspaceRecord};

        let dir = tempfile::TempDir::new().expect("tempdir");
        let state_root = dir.path().join("state");
        let project_root = dir.path().join("project");
        std::fs::create_dir_all(&project_root).expect("mkdir project");

        let canonical_root = project_root.canonicalize().expect("canonical project");
        let canonical_id = WorkspaceId::from_project_root(&canonical_root);
        let canonical_paths =
            StatePaths::under(canonical_id.clone(), &state_root).expect("canonical paths");
        std::fs::create_dir_all(&canonical_paths.root).expect("mkdir canonical");
        workspace_record::write(
            &canonical_paths,
            &WorkspaceRecord {
                workspace_id: canonical_id.clone(),
                project_root: canonical_root.clone(),
                session_name: session_name_for(&canonical_root),
                root_class: RootClass::Repo,
                updated_at: jiff::Timestamp::UNIX_EPOCH,
            },
        )
        .expect("write canonical record");

        let noncanonical_root = project_root.join("..").join("project");
        let stale_id = WorkspaceId::from_project_root(&noncanonical_root);
        assert_ne!(stale_id, canonical_id);
        let stale_paths = StatePaths::under(stale_id.clone(), &state_root).expect("stale paths");
        std::fs::create_dir_all(&stale_paths.root).expect("mkdir stale");
        workspace_record::write(
            &stale_paths,
            &WorkspaceRecord {
                workspace_id: stale_id,
                project_root: noncanonical_root,
                session_name: session_name_for(&canonical_root),
                root_class: RootClass::Repo,
                updated_at: jiff::Timestamp::now(),
            },
        )
        .expect("write stale duplicate");

        let known = known_workspaces_under(&workspaces_dir_under(&state_root)).expect("enumerate");
        assert_eq!(known.len(), 1);
        assert_eq!(known[0].workspace_id, canonical_id);
        assert_eq!(known[0].project_root, canonical_root);
        assert_eq!(known[0].session_name, session_name_for(&project_root));
    }

    #[test]
    fn known_workspaces_under_missing_root_is_empty() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let missing = dir.path().join("nope");
        assert!(known_workspaces_under(&missing).expect("ok").is_empty());
    }

    #[test]
    fn session_name_collapses_unsafe_runs() {
        // Spaces and `/` both fold to `-`, and runs collapse to a single `-`.
        assert_eq!(
            session_name_for(Path::new("/tmp/my repo")),
            "rimz-tmp-my-repo",
        );
    }

    #[test]
    fn session_name_is_stable_for_same_root() {
        let a = session_name_for(Path::new("/repo"));
        let b = session_name_for(Path::new("/repo"));
        assert_eq!(a, b);
    }

    #[test]
    fn resolve_marker_finds_cargo_toml_ancestor() {
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        let resolved = resolve_marker(here).expect("Cargo.toml above us");
        assert!(resolved.join("Cargo.toml").exists());
    }

    use std::ffi::OsString;

    /// An injected env carrying the identity pin, the test-side twin of the
    /// session environment a real pane inherits.
    fn pin_of(workspace_id: String, project_root: PathBuf) -> impl Fn(&str) -> Option<OsString> {
        move |key: &str| match key {
            ENV_WORKSPACE_ID => Some(workspace_id.clone().into()),
            ENV_PROJECT_ROOT => Some(project_root.clone().into_os_string()),
            _ => None,
        }
    }

    fn no_env(_key: &str) -> Option<OsString> {
        None
    }

    /// A bare directory and a marker directory, the fixture every pin test
    /// shares: the pin names the bare dir, the cwd sits in the marker dir.
    fn pin_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let pinned_root = dir.path().join("room");
        let marker_dir = dir.path().join("project");
        std::fs::create_dir_all(&pinned_root).expect("mkdir room");
        std::fs::create_dir_all(&marker_dir).expect("mkdir project");
        std::fs::write(marker_dir.join("Cargo.toml"), "[package]\n").expect("marker");
        (dir, pinned_root, marker_dir)
    }

    #[test]
    fn participant_pin_beats_the_static_ladder() {
        let (_dir, pinned_root, marker_dir) = pin_fixture();
        let pinned_root = pinned_root.canonicalize().expect("canonical room");
        let env = pin_of(
            WorkspaceId::from_project_root(&pinned_root).to_string(),
            pinned_root.clone(),
        );

        let resolved =
            WorkspaceResolver::resolve_with(ResolveMode::Participate, &marker_dir, None, &env)
                .expect("resolve");
        assert_eq!(resolved.project_root, pinned_root);
        assert_eq!(
            resolved.workspace_id,
            WorkspaceId::from_project_root(&pinned_root),
        );
        // The cwd still names the worktree the participant works in.
        assert_eq!(
            resolved.worktree_root,
            marker_dir.canonicalize().expect("canonical project"),
        );
        assert_eq!(resolved.root_class, RootClass::Directory);
    }

    #[test]
    fn create_mode_ignores_the_pin() {
        let (_dir, pinned_root, marker_dir) = pin_fixture();
        let pinned_root = pinned_root.canonicalize().expect("canonical room");
        let env = pin_of(
            WorkspaceId::from_project_root(&pinned_root).to_string(),
            pinned_root,
        );

        let resolved =
            WorkspaceResolver::resolve_with(ResolveMode::Create, &marker_dir, None, &env)
                .expect("resolve");
        assert_eq!(
            resolved.project_root,
            marker_dir.canonicalize().expect("canonical project"),
        );
        assert_eq!(resolved.root_class, RootClass::Marker);
    }

    #[test]
    fn root_override_beats_the_pin() {
        let (dir, pinned_root, marker_dir) = pin_fixture();
        let pinned_root = pinned_root.canonicalize().expect("canonical room");
        let env = pin_of(
            WorkspaceId::from_project_root(&pinned_root).to_string(),
            pinned_root,
        );
        let forced = dir.path().join("forced");
        std::fs::create_dir_all(&forced).expect("mkdir forced");

        let resolved = WorkspaceResolver::resolve_with(
            ResolveMode::Participate,
            &marker_dir,
            Some(forced.clone()),
            &env,
        )
        .expect("resolve");
        assert_eq!(
            resolved.project_root,
            forced.canonicalize().expect("canonical forced"),
        );
    }

    #[test]
    fn mismatched_pin_falls_back_to_the_static_ladder() {
        let (_dir, pinned_root, marker_dir) = pin_fixture();
        // An id that does not hash from the pinned root: stale or corrupt env.
        let env = pin_of(
            WorkspaceId::from_project_root(Path::new("/somewhere/else")).to_string(),
            pinned_root,
        );

        let resolved =
            WorkspaceResolver::resolve_with(ResolveMode::Participate, &marker_dir, None, &env)
                .expect("resolve");
        assert_eq!(
            resolved.project_root,
            marker_dir.canonicalize().expect("canonical project"),
        );
        assert_eq!(resolved.root_class, RootClass::Marker);
    }

    #[test]
    fn vanished_pin_root_falls_back_to_the_static_ladder() {
        let (dir, pinned_root, marker_dir) = pin_fixture();
        let gone = dir.path().join("gone");
        let env = pin_of(
            WorkspaceId::from_project_root(&pinned_root).to_string(),
            gone,
        );

        let resolved =
            WorkspaceResolver::resolve_with(ResolveMode::Participate, &marker_dir, None, &env)
                .expect("resolve");
        assert_eq!(resolved.root_class, RootClass::Marker);
    }

    #[test]
    fn unparseable_pin_falls_back_to_the_static_ladder() {
        let (_dir, pinned_root, marker_dir) = pin_fixture();
        let env = pin_of("not-a-workspace-id".to_owned(), pinned_root);

        let resolved =
            WorkspaceResolver::resolve_with(ResolveMode::Participate, &marker_dir, None, &env)
                .expect("resolve");
        assert_eq!(resolved.root_class, RootClass::Marker);
    }

    #[test]
    fn bare_directory_resolves_as_a_directory_workspace() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let scratch = dir.path().join("scratch");
        std::fs::create_dir_all(&scratch).expect("mkdir scratch");

        let resolved =
            WorkspaceResolver::resolve_with(ResolveMode::Create, &scratch, None, &no_env)
                .expect("resolve");
        assert_eq!(resolved.root_class, RootClass::Directory);
        assert_eq!(
            resolved.project_root,
            scratch.canonicalize().expect("canonical scratch"),
        );
        assert_eq!(resolved.project_root, resolved.worktree_root);
    }

    #[test]
    fn create_mode_refuses_home_as_a_directory_root() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("mkdir home");
        let home_env = |key: &str| (key == "HOME").then(|| home.clone().into_os_string());

        let err = WorkspaceResolver::resolve_with(ResolveMode::Create, &home, None, &home_env)
            .expect_err("refused");
        assert!(
            matches!(err, WorkspaceErr::RefusedRoot { .. }),
            "expected RefusedRoot, got: {err}",
        );
        assert!(err.to_string().contains("--root"), "error names the fix");

        // `--root` selects the override tier, which never refuses.
        let forced = WorkspaceResolver::resolve_with(
            ResolveMode::Create,
            &home,
            Some(home.clone()),
            &home_env,
        )
        .expect("forced via --root");
        assert_eq!(forced.root_class, RootClass::Directory);
    }

    #[test]
    fn participants_never_refuse_a_pathological_root() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("mkdir home");
        let home_env = |key: &str| (key == "HOME").then(|| home.clone().into_os_string());

        // A hook on the agent's critical path degrades, never errors: the
        // pinless fallback still resolves the directory tier at $HOME.
        let resolved =
            WorkspaceResolver::resolve_with(ResolveMode::Participate, &home, None, &home_env)
                .expect("resolve");
        assert_eq!(resolved.root_class, RootClass::Directory);
    }

    #[test]
    fn refuses_the_filesystem_root() {
        let err = refuse_pathological_root(Path::new("/"), &no_env).expect_err("refused");
        assert!(matches!(err, WorkspaceErr::RefusedRoot { .. }));
    }

    #[test]
    fn pin_env_carries_both_identity_keys() {
        let root = Path::new("/repo");
        let env = pin_env(&WorkspaceId::from_project_root(root), root);
        assert_eq!(
            env.get(ENV_WORKSPACE_ID),
            Some(&WorkspaceId::from_project_root(root).to_string()),
        );
        assert_eq!(env.get(ENV_PROJECT_ROOT), Some(&"/repo".to_owned()));
    }
}
