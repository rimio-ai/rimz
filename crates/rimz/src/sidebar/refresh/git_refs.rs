//! In-process git ref reads for diff-stats ancestry probes and the trunk lane.
//!
//! This module reads only ref files and `packed-refs`. Any shape that would
//! need git's full revision parser returns `None` and the caller falls back to
//! the existing `git` subprocess chain. A reftable repository keeps its refs in
//! binary tables this module does not decode: [`trunk`] names the stack instead,
//! and [`trunk_from_for_each_ref`] runs the same trunk ladder over the caller's
//! `git for-each-ref` output.

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

/// The trunk as the ref store answers it, read without HEAD.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum TrunkRefs {
    /// Loose refs and `packed-refs` are git's own storage, so a miss is a real
    /// miss (no trunk ref, or a lock held mid `pack-refs`).
    Files(Option<(String, String)>),
    /// The reftable stack's `tables.list` contents, which every ref update
    /// rewrites and a no-op update leaves byte-identical.
    Reftable { stack: String },
}

/// Resolve the trunk ladder of [`resolve`] without reading HEAD, so an unborn
/// or unparseable HEAD still yields the trunk; `None` when `worktree` has no
/// git admin directory.
pub(super) fn trunk(worktree: &Path, configured_trunk: Option<&str>) -> Option<TrunkRefs> {
    let git_dir = crate::worktree::git_admin_dir_from_checkout_metadata(worktree)
        .ok()
        .flatten()?;
    let common_dir = common_dir(&git_dir)?;
    if let Ok(stack) = std::fs::read_to_string(common_dir.join("reftable/tables.list")) {
        return Some(TrunkRefs::Reftable { stack });
    }
    Some(TrunkRefs::Files(trunk_ref(
        &git_dir,
        &common_dir,
        configured_trunk,
    )))
}

/// Every full refname the trunk ladder may read, for one `git for-each-ref`.
pub(super) fn trunk_ref_patterns(configured_trunk: Option<&str>) -> Vec<String> {
    trunk_names(configured_trunk)
        .flat_map(ref_candidates)
        .chain([ORIGIN_HEAD.to_owned()])
        .collect()
}

/// Run the trunk ladder over `git for-each-ref
/// --format=%(refname)%00%(objectname)%00%(symref)` output, matching each
/// refname exactly since a literal pattern also matches the refs below it.
pub(super) fn trunk_from_for_each_ref(
    configured_trunk: Option<&str>,
    stdout: &[u8],
) -> Option<(String, String)> {
    let text = String::from_utf8_lossy(stdout);
    let rows: Vec<[&str; 3]> = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\0');
            let row = [fields.next()?, fields.next()?, fields.next()?];
            fields.next().is_none().then_some(row)
        })
        .collect();
    let row = |reference: &str| rows.iter().find(|row| row[0] == reference);
    trunk_ladder(
        configured_trunk,
        |reference| row(reference).map(|row| row[1].to_owned()),
        || {
            let [_, sha, target] = row(ORIGIN_HEAD)?;
            Some((target.to_string(), sha.to_string()))
        },
    )
    .filter(|(_, sha)| is_hex_oid(sha))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HeadRef {
    sha: String,
    branch: Option<String>,
}

pub(super) fn common_dir(git_dir: &Path) -> Option<PathBuf> {
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

pub(super) fn head_branch(git_dir: &Path, common_dir: &Path) -> Option<Option<String>> {
    read_head(git_dir, common_dir).map(|head| head.branch)
}

pub(super) fn head_sha(git_dir: &Path, common_dir: &Path) -> Option<String> {
    read_head(git_dir, common_dir).map(|head| head.sha)
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

const ORIGIN_HEAD: &str = "refs/remotes/origin/HEAD";

fn trunk_ref(
    git_dir: &Path,
    common_dir: &Path,
    configured: Option<&str>,
) -> Option<(String, String)> {
    trunk_ladder(
        configured,
        |reference| read_ref(git_dir, common_dir, reference),
        || {
            let raw = read_ref_text(common_dir.join(ORIGIN_HEAD))?;
            let reference = raw.strip_prefix("ref:").map(str::trim)?;
            let sha = read_ref(git_dir, common_dir, reference)?;
            Some((reference.to_owned(), sha))
        },
    )
}

/// The configured trunk, then `main`, `master`, and `origin/HEAD`'s target:
/// `sha_of` reads one full refname, `origin_head` returns the symref's target
/// refname and SHA.
fn trunk_ladder(
    configured: Option<&str>,
    sha_of: impl Fn(&str) -> Option<String>,
    origin_head: impl FnOnce() -> Option<(String, String)>,
) -> Option<(String, String)> {
    for name in trunk_names(configured) {
        if let Some(sha) = ref_candidates(name)
            .iter()
            .find_map(|reference| sha_of(reference))
        {
            return Some((name.to_owned(), sha));
        }
    }
    let (reference, sha) = origin_head()?;
    Some((short_ref(&reference)?, sha))
}

fn trunk_names(configured: Option<&str>) -> impl Iterator<Item = &str> {
    configured
        .filter(|name| !name.is_empty() && !name.starts_with('-'))
        .into_iter()
        .chain(["main", "master"])
}

fn ref_candidates(name: &str) -> Vec<String> {
    if name.starts_with("refs/") {
        vec![name.to_owned()]
    } else if name.contains('/') {
        vec![format!("refs/remotes/{name}"), format!("refs/heads/{name}")]
    } else {
        vec![format!("refs/heads/{name}"), format!("refs/remotes/{name}")]
    }
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
    fn for_each_ref_output_runs_the_trunk_ladder_by_exact_refname() {
        let row = |name: &str, sha: &str, symref: &str| format!("{name}\0{sha}\0{symref}\n");
        let below = row("refs/heads/main/old", HEAD, "");
        let origin = row(
            "refs/remotes/origin/HEAD",
            PACKED,
            "refs/remotes/origin/trunk",
        );
        let stdout = format!("{below}{origin}");
        assert_eq!(
            trunk_from_for_each_ref(None, stdout.as_bytes()),
            Some(("origin/trunk".into(), PACKED.into()))
        );
        let stdout = format!("{stdout}{}", row("refs/heads/main", MAIN, ""));
        assert_eq!(
            trunk_from_for_each_ref(None, stdout.as_bytes()),
            Some(("main".into(), MAIN.into()))
        );
        let stdout = format!("{stdout}{}", row("refs/remotes/develop", HEAD, ""));
        assert_eq!(
            trunk_from_for_each_ref(Some("develop"), stdout.as_bytes()),
            Some(("develop".into(), HEAD.into()))
        );
        assert_eq!(
            trunk_ref_patterns(Some("develop")),
            [
                "refs/heads/develop",
                "refs/remotes/develop",
                "refs/heads/main",
                "refs/remotes/main",
                "refs/heads/master",
                "refs/remotes/master",
                "refs/remotes/origin/HEAD",
            ]
        );
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
