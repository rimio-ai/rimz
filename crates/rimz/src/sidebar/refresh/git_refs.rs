//! In-process git ref reads for diff-stats ancestry probes.
//!
//! This module reads only ref files and `packed-refs`. Any shape that would
//! need git's full revision parser returns `None` and the caller falls back to
//! the existing `git` subprocess chain.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GitRefs {
    pub(super) head_sha: String,
    pub(super) head_branch: Option<String>,
    pub(super) trunk_name: String,
    pub(super) trunk_sha: String,
    pub(super) merge_in_progress: bool,
}

pub(super) fn resolve(worktree: &Path, configured_trunk: Option<&str>) -> Option<GitRefs> {
    let git_dir = crate::worktree::git_admin_dir_from_checkout_metadata(worktree)
        .ok()
        .flatten()?;
    let common_dir = common_dir(&git_dir)?;
    let head = read_head(&git_dir, &common_dir)?;
    let (trunk_name, trunk_sha) = trunk_ref(&git_dir, &common_dir, configured_trunk)?;
    Some(GitRefs {
        head_sha: head.sha,
        head_branch: head.branch,
        trunk_name,
        trunk_sha,
        merge_in_progress: merge_in_progress(&git_dir),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HeadRef {
    sha: String,
    branch: Option<String>,
}

fn common_dir(git_dir: &Path) -> Option<PathBuf> {
    let path = git_dir.join("commondir");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Some(git_dir.to_path_buf());
    };
    let raw = text.trim();
    if raw.is_empty() {
        return None;
    }
    let common = Path::new(raw);
    Some(if common.is_absolute() {
        common.to_path_buf()
    } else {
        git_dir.join(common)
    })
}

fn read_head(git_dir: &Path, common_dir: &Path) -> Option<HeadRef> {
    let raw = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let raw = raw.trim();
    if let Some(reference) = raw.strip_prefix("ref:").map(str::trim) {
        let sha = read_ref(git_dir, common_dir, reference)?;
        return Some(HeadRef {
            sha,
            branch: reference.strip_prefix("refs/heads/").map(ToOwned::to_owned),
        });
    }
    is_hex_oid(raw).then(|| HeadRef {
        sha: raw.to_owned(),
        branch: None,
    })
}

fn trunk_ref(
    git_dir: &Path,
    common_dir: &Path,
    configured: Option<&str>,
) -> Option<(String, String)> {
    let configured = configured.filter(|name| !name.is_empty() && !name.starts_with('-'));
    for name in configured.into_iter().chain(["main", "master"]) {
        if let Some((display, sha)) = named_ref(git_dir, common_dir, name) {
            return Some((display, sha));
        }
    }
    let raw = read_ref_text(common_dir.join("refs/remotes/origin/HEAD"))?;
    let reference = raw.strip_prefix("ref:").map(str::trim)?;
    let sha = read_ref(git_dir, common_dir, reference)?;
    Some((short_ref(reference)?, sha))
}

fn named_ref(git_dir: &Path, common_dir: &Path, name: &str) -> Option<(String, String)> {
    let candidates = if name.starts_with("refs/") {
        vec![(name.to_owned(), name.to_owned())]
    } else if name.contains('/') {
        vec![
            (name.to_owned(), format!("refs/remotes/{name}")),
            (name.to_owned(), format!("refs/heads/{name}")),
        ]
    } else {
        vec![
            (name.to_owned(), format!("refs/heads/{name}")),
            (name.to_owned(), format!("refs/remotes/{name}")),
        ]
    };
    candidates.into_iter().find_map(|(display, reference)| {
        read_ref(git_dir, common_dir, &reference).map(|sha| (display, sha))
    })
}

fn read_ref(git_dir: &Path, common_dir: &Path, reference: &str) -> Option<String> {
    if !reference.starts_with("refs/") || reference.contains("..") {
        return None;
    }
    let path = if reference.starts_with("refs/bisect/") {
        git_dir.join(reference)
    } else {
        common_dir.join(reference)
    };
    if let Some(sha) = read_ref_text(path).and_then(|raw| {
        let raw = raw.trim();
        is_hex_oid(raw).then(|| raw.to_owned())
    }) {
        return Some(sha);
    }
    read_packed_ref(common_dir, reference)
}

fn read_ref_text(path: PathBuf) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

fn read_packed_ref(common_dir: &Path, reference: &str) -> Option<String> {
    let text = std::fs::read_to_string(common_dir.join("packed-refs")).ok()?;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') || line.starts_with('^') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let sha = parts.next()?;
        let name = parts.next()?;
        if parts.next().is_none() && name == reference && is_hex_oid(sha) {
            return Some(sha.to_owned());
        }
    }
    None
}

fn short_ref(reference: &str) -> Option<String> {
    reference
        .strip_prefix("refs/heads/")
        .or_else(|| reference.strip_prefix("refs/remotes/"))
        .map(ToOwned::to_owned)
}

fn merge_in_progress(git_dir: &Path) -> bool {
    [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "rebase-merge",
        "rebase-apply",
    ]
    .into_iter()
    .any(|name| git_dir.join(name).exists())
}

fn is_hex_oid(raw: &str) -> bool {
    matches!(raw.len(), 40 | 64) && raw.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "1111111111111111111111111111111111111111";
    const MAIN: &str = "2222222222222222222222222222222222222222";
    const PACKED: &str = "3333333333333333333333333333333333333333";

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn resolves_loose_head_and_trunk_refs() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join(".git/HEAD"), "ref: refs/heads/feature\n");
        write(&dir.path().join(".git/refs/heads/feature"), HEAD);
        write(&dir.path().join(".git/refs/heads/main"), MAIN);

        let refs = resolve(dir.path(), None).unwrap();

        assert_eq!(refs.head_sha, HEAD);
        assert_eq!(refs.head_branch.as_deref(), Some("feature"));
        assert_eq!(refs.trunk_name, "main");
        assert_eq!(refs.trunk_sha, MAIN);
        assert!(!refs.merge_in_progress);
    }

    #[test]
    fn resolves_packed_and_detached_refs() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join(".git/HEAD"), HEAD);
        write(
            &dir.path().join(".git/packed-refs"),
            &format!("# pack\n{PACKED} refs/heads/main\n^{}\n", "4".repeat(40)),
        );

        let refs = resolve(dir.path(), None).unwrap();

        assert_eq!(refs.head_branch, None);
        assert_eq!(refs.trunk_sha, PACKED);
    }

    #[test]
    fn resolves_linked_worktree_gitdir_and_commondir() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo/.git");
        let worktree = dir.path().join("repo/wt");
        let linked = repo.join("worktrees/wt");
        std::fs::create_dir_all(&worktree).unwrap();
        write(&worktree.join(".git"), "gitdir: ../.git/worktrees/wt\n");
        write(&linked.join("commondir"), "../..\n");
        write(&linked.join("HEAD"), "ref: refs/heads/feature\n");
        write(&repo.join("refs/heads/feature"), HEAD);
        write(&repo.join("refs/heads/main"), MAIN);
        write(&linked.join("MERGE_HEAD"), HEAD);

        let refs = resolve(&worktree, None).unwrap();

        assert_eq!(refs.head_branch.as_deref(), Some("feature"));
        assert_eq!(refs.trunk_sha, MAIN);
        assert!(refs.merge_in_progress);
    }

    #[test]
    fn returns_none_for_unborn_or_unparseable_refs() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join(".git/HEAD"), "ref: refs/heads/feature\n");
        write(&dir.path().join(".git/refs/heads/main"), MAIN);
        assert!(resolve(dir.path(), None).is_none());

        write(
            &dir.path().join(".git/refs/heads/feature"),
            "ref: refs/heads/other\n",
        );
        assert!(resolve(dir.path(), None).is_none());
    }
}
