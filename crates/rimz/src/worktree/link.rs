//! Symlink directories from the project checkout into a freshly created worktree.
//!
//! `.worktreelink` is the sharing twin of `.worktreeinclude`: one relative
//! directory path per line, with blank lines and `#` comments ignored. RimZ
//! links those directories from the main checkout into each new worktree, then
//! registers an anchored exclude pattern so the machine-local link never
//! dirties the checkout.

use std::path::{Component, Path};

const LINK_FILE: &str = ".worktreelink";

/// Symlink directories listed in `<repo_root>/.worktreelink` into `worktree`,
/// returning the number of links created. Best-effort: never errors and never
/// blocks worktree creation; problems surface as `tracing::warn!`.
pub(crate) fn link_dirs(repo_root: &Path, worktree: &Path) -> usize {
    let link_path = repo_root.join(LINK_FILE);
    let text = match std::fs::read_to_string(&link_path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return 0,
        Err(err) => {
            tracing::warn!(path = %link_path.display(), error = %err, "reading .worktreelink");
            return 0;
        }
    };
    let root = match repo_root.canonicalize() {
        Ok(root) => root,
        Err(err) => {
            tracing::warn!(path = %repo_root.display(), error = %err, "canonicalizing project root for .worktreelink");
            return 0;
        }
    };
    parse_patterns(&text)
        .map(|rel| link_dir(repo_root, &root, worktree, rel))
        .sum()
}

fn link_dir(repo_root: &Path, root: &Path, worktree: &Path, rel: &str) -> usize {
    let rel = rel.trim_end_matches('/');
    if rel.is_empty() {
        return 0;
    }
    if Path::new(rel).is_absolute() || has_parent_segment(rel) {
        tracing::warn!(
            pattern = rel,
            "skipping .worktreelink entry: must be a relative path inside the project"
        );
        return 0;
    }

    let src = repo_root.join(rel);
    let real = match src.canonicalize() {
        Ok(real) => real,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!(src = %src.display(), "skipping .worktreelink entry: source is missing");
            return 0;
        }
        Err(err) => {
            tracing::warn!(src = %src.display(), error = %err, "canonicalizing .worktreelink source");
            return 0;
        }
    };
    if !real.starts_with(root) {
        tracing::warn!(src = %src.display(), "skipping .worktreelink entry: resolves outside the project root");
        return 0;
    }
    if !real.is_dir() {
        tracing::warn!(src = %src.display(), "skipping .worktreelink entry: source is not a directory");
        return 0;
    }

    let dest = worktree.join(rel);
    match std::fs::symlink_metadata(&dest) {
        Ok(_) => {
            tracing::warn!(dest = %dest.display(), "skipping .worktreelink entry: destination already exists");
            return 0;
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            tracing::warn!(dest = %dest.display(), error = %err, "stat .worktreelink destination");
            return 0;
        }
    }
    if let Some(parent) = dest.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(dest = %dest.display(), error = %err, "creating .worktreelink destination parent");
        return 0;
    }
    let target = root.join(rel);
    if let Err(err) = symlink_dir(&target, &dest) {
        tracing::warn!(src = %target.display(), dest = %dest.display(), error = %err, "creating .worktreelink symlink");
        return 0;
    }
    super::exclude::ensure_excluded(worktree, &anchored_pattern(rel));
    1
}

fn anchored_pattern(rel: &str) -> String {
    format!("/{}", rel.trim_end_matches('/'))
}

/// Non-blank, non-comment lines, trimmed. `#` introduces a comment.
fn parse_patterns(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
}

fn has_parent_segment(pattern: &str) -> bool {
    Path::new(pattern)
        .components()
        .any(|component| matches!(component, Component::ParentDir))
}

#[cfg(unix)]
fn symlink_dir(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dest)
}

#[cfg(not(unix))]
fn symlink_dir(_src: &Path, _dest: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        ".worktreelink symlinks require a Unix platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, contents).expect("write file");
    }

    fn git(cwd: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[test]
    fn linked_dir_is_symlinked_excluded_and_idempotent() {
        let dir = tempfile::tempdir().expect("dir");
        let repo = dir.path().join("repo");
        let worktree = dir.path().join("wt");
        std::fs::create_dir_all(&repo).expect("repo");
        if !git(&repo, &["init", "-q", "-b", "main"]) {
            return;
        }
        let _ = git(&repo, &["config", "user.email", "t@example.com"]);
        let _ = git(&repo, &["config", "user.name", "t"]);
        write(&repo.join("README.md"), "base\n");
        let _ = git(&repo, &["add", "README.md"]);
        let _ = git(&repo, &["commit", "-q", "-m", "base"]);
        write(
            &repo.join("node_modules/pkg/index.js"),
            "module.exports = 1\n",
        );
        write(&repo.join(LINK_FILE), "node_modules/\n");
        if !git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                worktree.to_str().expect("utf8 worktree"),
                "-b",
                "demo",
            ],
        ) {
            return;
        }

        assert_eq!(link_dirs(&repo, &worktree), 1);
        let dest = worktree.join("node_modules");
        assert!(
            std::fs::symlink_metadata(&dest)
                .expect("linked dir metadata")
                .is_symlink()
        );
        assert_eq!(
            dest.canonicalize().expect("dest canonical"),
            repo.join("node_modules")
                .canonicalize()
                .expect("src canonical")
        );

        let exclude = super::super::exclude::exclude_path(&worktree).expect("exclude path");
        let text = std::fs::read_to_string(&exclude).expect("exclude text");
        assert_eq!(
            text.lines().filter(|line| *line == "/node_modules").count(),
            1
        );

        assert_eq!(link_dirs(&repo, &worktree), 0);
        let text = std::fs::read_to_string(&exclude).expect("exclude text");
        assert_eq!(
            text.lines().filter(|line| *line == "/node_modules").count(),
            1
        );
    }

    #[test]
    fn escaping_and_absolute_paths_are_skipped() {
        let repo = tempfile::tempdir().expect("repo");
        let worktree = tempfile::tempdir().expect("worktree");
        write(
            &repo.path().join(LINK_FILE),
            "../outside\n/tmp/rimz-worktreelink\n",
        );

        assert_eq!(link_dirs(repo.path(), worktree.path()), 0);
        assert!(!worktree.path().join("outside").exists());
        assert!(!worktree.path().join("tmp").exists());
    }

    #[test]
    fn existing_destination_is_not_clobbered() {
        let repo = tempfile::tempdir().expect("repo");
        let worktree = tempfile::tempdir().expect("worktree");
        std::fs::create_dir_all(repo.path().join("target")).expect("source dir");
        write(&repo.path().join(LINK_FILE), "target\n");
        write(&worktree.path().join("target"), "keep me\n");

        assert_eq!(link_dirs(repo.path(), worktree.path()), 0);
        assert_eq!(
            std::fs::read_to_string(worktree.path().join("target")).expect("dest"),
            "keep me\n"
        );
    }
}
