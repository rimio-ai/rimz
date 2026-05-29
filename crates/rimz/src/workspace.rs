//! Project-root and worktree-root resolution.
//!
//! Workspace identity is keyed on the canonical *repo* (the parent of
//! `git rev-parse --git-common-dir`). Every worktree of the same repo shares
//! the same workspace; submodules get their own. See `docs/internals` and
//! `DESIGN.md` for the rules this implements.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::ids::{MuxName, WorkspaceId};

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
