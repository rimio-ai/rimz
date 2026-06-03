//! Project-root and worktree-root resolution.
//!
//! Workspace identity is keyed on the canonical *repo* (the parent of
//! `git rev-parse --git-common-dir`). Every worktree of the same repo shares
//! the same workspace; submodules get their own. See `docs/internals` and
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
    #[error("git probe failed: {0}")]
    GitProbe(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, WorkspaceErr>;

/// What resolution produces: the IDs the rest of the system uses.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolvedWorkspace {
    pub workspace_id: WorkspaceId,
    pub project_root: PathBuf,
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

impl WorkspaceResolver {
    /// Resolve from a starting path. `root_override` corresponds to the
    /// `--root` CLI flag and `[workspace] root` in `.rimz/config.toml`.
    pub fn resolve(
        start: impl AsRef<Path>,
        root_override: Option<PathBuf>,
    ) -> Result<ResolvedWorkspace> {
        let start_in = start.as_ref();
        let start = start_in
            .canonicalize()
            .unwrap_or_else(|_| start_in.to_path_buf());

        let (project_root, worktree_root) = if let Some(root) = root_override {
            let root = root.canonicalize().unwrap_or(root);
            (root.clone(), root)
        } else if let Some(git) = resolve_git(&start)? {
            git
        } else if let Some(marker) = resolve_marker(&start) {
            (marker.clone(), marker)
        } else {
            (start.clone(), start.clone())
        };

        let workspace_id = WorkspaceId::from_project_root(&project_root);
        let session_name = session_name_for(&project_root);
        let worktree_branch = current_branch(&worktree_root)?;

        Ok(ResolvedWorkspace {
            workspace_id,
            project_root,
            worktree_root,
            worktree_branch,
            session_name,
            mux_hint: None,
        })
    }
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
}
