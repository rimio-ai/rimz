//! The one classifier of a directory into its checkout's own lane, read from
//! Git metadata and RimZ's worktree marker without spawning Git.

use std::path::{Path, PathBuf};

use crate::ids::{Lane, LaneHome};

/// The checkout's own lane for `dir`: the deepest of trunk and a managed
/// worktree containing it, else a foreign checkout homed at its repository
/// root, else at `dir` itself. A non-repository room's project root is its
/// trunk; a managed worktree whose directory is `main` is foreign. A repository
/// at or above the user's home, compared with links resolved, homes no foreign
/// lane, so every directory under a dotfiles home keeps its own.
pub fn own_lane(project_root: &Path, dir: &Path) -> Lane {
    let home = std::env::var_os("HOME").filter(|home| !home.is_empty());
    own_lane_at_home(project_root, dir, home.as_deref().map(Path::new))
}

fn own_lane_at_home(project_root: &Path, dir: &Path, home: Option<&Path>) -> Lane {
    let dir = crate::utils::path::normalize_path_lexical(dir);
    for ancestor in dir.ancestors() {
        if ancestor == project_root {
            return Lane::own(LaneHome::Trunk, path_text(project_root));
        }
        if is_managed_worktree(project_root, ancestor) {
            let home = if ancestor.file_name().is_some_and(|name| name == "main") {
                LaneHome::Foreign
            } else {
                LaneHome::Worktree
            };
            return Lane::own(home, path_text(ancestor));
        }
    }
    let root = dir
        .ancestors()
        .find(|ancestor| is_repository_root(ancestor))
        .filter(|root| !home.is_some_and(|home| resolved(home).starts_with(resolved(root))))
        .unwrap_or(&dir);
    Lane::own(LaneHome::Foreign, path_text(root))
}

fn is_managed_worktree(project_root: &Path, dir: &Path) -> bool {
    dir.join(".git").is_file()
        && matches!(
            super::classify_checkout(dir, project_root),
            super::CheckoutKind::Linked(_)
        )
        && super::read_marker_from_checkout_metadata(dir).is_ok_and(|marker| marker.is_some())
}

/// `path` with every link resolved, or as written when it cannot be resolved,
/// so a home and a root named through different links still compare equal.
fn resolved(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| crate::utils::path::normalize_path_lexical(path))
}

/// A `.git` directory holding `HEAD`, or a `.git` file naming its `gitdir:`;
/// read without Git, since a bare `.git` entry proves nothing.
fn is_repository_root(dir: &Path) -> bool {
    use std::io::Read;
    let git = dir.join(".git");
    if git.is_dir() {
        return git.join("HEAD").is_file();
    }
    let mut prefix = [0; 7];
    std::fs::File::open(&git)
        .and_then(|mut file| file.read_exact(&mut prefix))
        .is_ok_and(|()| &prefix == b"gitdir:")
}

fn path_text(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
#[path = "own_lane/tests.rs"]
mod tests;
