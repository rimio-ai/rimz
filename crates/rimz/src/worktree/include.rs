//! Seed a freshly created worktree from the project's `.worktreeinclude`.
//!
//! `git worktree add` checks out only tracked files at the base ref, so the
//! untracked files an agent needs to run — `.env`, local config, caches — never
//! follow it into a new worktree. A project lists those files as glob patterns
//! in `<repo_root>/.worktreeinclude`, one per line, and RimZ copies each
//! pattern's matches from the checkout into the new worktree, preserving the
//! path relative to the repo root.
//!
//! Seeding stays inside the project root. Absolute patterns and patterns
//! reaching out with `..` are skipped, top-level symlinks are not followed, and
//! every file is confined by its canonical path — so a symlinked directory a
//! glob descends into cannot pull host files into an agent-readable worktree.
//! Seeding is best-effort enrichment: a missing include file is a silent no-op,
//! and a pattern that matches nothing or a file that fails to copy emits a
//! warning and is skipped so the worktree still launches.

use std::ffi::OsStr;
use std::path::{Component, Path};

const INCLUDE_FILE: &str = ".worktreeinclude";

/// Copy files matching `<repo_root>/.worktreeinclude` into `worktree`, returning
/// the number of files copied. Best-effort: never errors and never blocks
/// worktree creation; problems surface as `tracing::warn!`.
pub(crate) fn copy_includes(repo_root: &Path, worktree: &Path) -> usize {
    let include_path = repo_root.join(INCLUDE_FILE);
    let text = match std::fs::read_to_string(&include_path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return 0,
        Err(err) => {
            tracing::warn!(path = %include_path.display(), error = %err, "reading .worktreeinclude");
            return 0;
        }
    };

    // Establish the boundary once: a file is seeded only if its real location is
    // inside the canonical project root. `glob`'s `**` descends through symlinked
    // directories, so confining by lexical path is not enough — a committed
    // symlink plus an include pattern could otherwise pull host files into an
    // agent-readable worktree, and `.worktreeinclude` is outside the trust hash.
    let root = match repo_root.canonicalize() {
        Ok(root) => root,
        Err(err) => {
            tracing::warn!(path = %repo_root.display(), error = %err, "canonicalizing project root for .worktreeinclude");
            return 0;
        }
    };

    parse_patterns(&text)
        .map(|pattern| copy_pattern(repo_root, &root, worktree, pattern))
        .sum()
}

/// Whether `path`'s real, symlink-resolved location is inside the canonical root.
fn within_root(root: &Path, path: &Path) -> bool {
    matches!(path.canonicalize(), Ok(real) if real.starts_with(root))
}

/// Non-blank, non-comment lines, trimmed. `#` introduces a comment.
fn parse_patterns(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
}

/// Conventional shell-glob semantics: `*` stays within a path component and
/// `**` crosses directories, so `.env*` matches only the root and `**/*.key`
/// recurses. Dotfiles match a leading `*` (the default), so `.env` need not be
/// spelled literally in a wider pattern.
const MATCH_OPTIONS: glob::MatchOptions = glob::MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

fn copy_pattern(repo_root: &Path, root: &Path, worktree: &Path, pattern: &str) -> usize {
    if Path::new(pattern).is_absolute() || has_parent_segment(pattern) {
        tracing::warn!(
            pattern,
            "skipping .worktreeinclude entry: must be a relative path inside the project"
        );
        return 0;
    }

    // Root the user pattern at the repo, escaping any glob metacharacters in the
    // repo path itself so only the user's pattern segment is interpreted.
    let rooted = format!(
        "{}/{}",
        glob::Pattern::escape(&repo_root.to_string_lossy()),
        pattern
    );
    let matches = match glob::glob_with(&rooted, MATCH_OPTIONS) {
        Ok(matches) => matches,
        Err(err) => {
            tracing::warn!(pattern, error = %err, "skipping invalid .worktreeinclude glob");
            return 0;
        }
    };

    let mut copied = 0;
    let mut matched_any = false;
    for entry in matches {
        match entry {
            Ok(src) => {
                matched_any = true;
                copied += copy_match(repo_root, root, worktree, &src);
            }
            Err(err) => tracing::warn!(pattern, error = %err, "reading .worktreeinclude match"),
        }
    }
    if !matched_any {
        tracing::warn!(pattern, "no files matched .worktreeinclude entry");
    }
    copied
}

fn has_parent_segment(pattern: &str) -> bool {
    Path::new(pattern)
        .components()
        .any(|component| matches!(component, Component::ParentDir))
}

/// Copy one matched path into the worktree at its repo-relative location.
fn copy_match(repo_root: &Path, root: &Path, worktree: &Path, src: &Path) -> usize {
    // Never recurse into the destination: skip the worktree itself and anything
    // inside it when a configured `dir` places the worktree under the repo root.
    if worktree.starts_with(src) || src.starts_with(worktree) {
        return 0;
    }
    let Ok(rel) = src.strip_prefix(repo_root) else {
        return 0;
    };
    if rel.components().next().map(Component::as_os_str) == Some(OsStr::new(".git")) {
        return 0;
    }

    let meta = match std::fs::symlink_metadata(src) {
        Ok(meta) => meta,
        Err(err) => {
            tracing::warn!(src = %src.display(), error = %err, "stat .worktreeinclude match");
            return 0;
        }
    };
    if meta.is_symlink() {
        tracing::warn!(src = %src.display(), "skipping symlink in .worktreeinclude match");
        return 0;
    }
    // The lexical path can sit under `repo_root` while a symlinked parent
    // component (traversed by `glob`'s `**`) resolves the real file outside it.
    if !within_root(root, src) {
        tracing::warn!(src = %src.display(), "skipping .worktreeinclude match: resolves outside the project root");
        return 0;
    }

    let dest = worktree.join(rel);
    if meta.is_dir() {
        copy_dir(root, src, &dest)
    } else {
        usize::from(copy_file(src, &dest))
    }
}

/// Recursively copy a directory's regular files, recreating its structure under
/// `dest`. Symlinks are never followed, so the walk stays within the real,
/// already-confined subtree; each file is reconfirmed inside the root before it
/// is copied.
fn copy_dir(root: &Path, src: &Path, dest: &Path) -> usize {
    let entries = match std::fs::read_dir(src) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(src = %src.display(), error = %err, "reading worktree seed directory");
            return 0;
        }
    };
    let mut copied = 0;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                tracing::warn!(dir = %src.display(), error = %err, "reading .worktreeinclude directory entry");
                continue;
            }
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "stat .worktreeinclude directory entry");
                continue;
            }
        };
        let child_dest = dest.join(entry.file_name());
        if file_type.is_symlink() {
            continue;
        } else if file_type.is_dir() {
            copied += copy_dir(root, &path, &child_dest);
        } else if within_root(root, &path) {
            copied += usize::from(copy_file(&path, &child_dest));
        } else {
            tracing::warn!(path = %path.display(), "skipping .worktreeinclude file: resolves outside the project root");
        }
    }
    copied
}

fn copy_file(src: &Path, dest: &Path) -> bool {
    if let Some(parent) = dest.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(dest = %dest.display(), error = %err, "creating worktree seed directory");
        return false;
    }
    match std::fs::copy(src, dest) {
        Ok(_) => true,
        Err(err) => {
            tracing::warn!(src = %src.display(), dest = %dest.display(), error = %err, "copying worktree seed file");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, contents).expect("write file");
    }

    #[test]
    fn parse_patterns_drops_comments_and_blanks() {
        let text = "\n  .env  \n# a comment\n\t# indented comment\nconfig/*.toml\n";
        let patterns: Vec<&str> = parse_patterns(text).collect();
        assert_eq!(patterns, vec![".env", "config/*.toml"]);
    }

    #[test]
    fn missing_include_file_is_a_silent_no_op() {
        let repo = tempfile::tempdir().expect("repo");
        let worktree = tempfile::tempdir().expect("worktree");
        assert_eq!(copy_includes(repo.path(), worktree.path()), 0);
    }

    #[test]
    fn copies_exact_path_and_glob_preserving_structure() {
        let repo = tempfile::tempdir().expect("repo");
        let worktree = tempfile::tempdir().expect("worktree");
        write(&repo.path().join(".env"), "SECRET=1");
        write(&repo.path().join("config/local.toml"), "a = 1");
        write(&repo.path().join("config/prod.toml"), "a = 2");
        write(&repo.path().join("config/notes.md"), "skip me");
        write(&repo.path().join(INCLUDE_FILE), ".env\nconfig/*.toml\n");

        let copied = copy_includes(repo.path(), worktree.path());

        assert_eq!(copied, 3);
        assert_eq!(
            std::fs::read_to_string(worktree.path().join(".env")).expect("env"),
            "SECRET=1"
        );
        assert!(worktree.path().join("config/local.toml").is_file());
        assert!(worktree.path().join("config/prod.toml").is_file());
        assert!(
            !worktree.path().join("config/notes.md").exists(),
            "glob copies only matches"
        );
    }

    #[test]
    fn copies_directory_recursively() {
        let repo = tempfile::tempdir().expect("repo");
        let worktree = tempfile::tempdir().expect("worktree");
        write(&repo.path().join("vendor/a.txt"), "a");
        write(&repo.path().join("vendor/nested/b.txt"), "b");
        write(&repo.path().join(INCLUDE_FILE), "vendor\n");

        assert_eq!(copy_includes(repo.path(), worktree.path()), 2);
        assert!(worktree.path().join("vendor/a.txt").is_file());
        assert!(worktree.path().join("vendor/nested/b.txt").is_file());
    }

    #[test]
    fn pattern_matching_nothing_copies_nothing() {
        let repo = tempfile::tempdir().expect("repo");
        let worktree = tempfile::tempdir().expect("worktree");
        write(&repo.path().join(INCLUDE_FILE), "does-not-exist.txt\n");
        assert_eq!(copy_includes(repo.path(), worktree.path()), 0);
    }

    #[test]
    fn absolute_and_parent_escaping_patterns_are_skipped() {
        let repo = tempfile::tempdir().expect("repo");
        let worktree = tempfile::tempdir().expect("worktree");
        write(&repo.path().join("inside.txt"), "ok");
        write(
            &repo.path().join(INCLUDE_FILE),
            "/etc/passwd\n../outside.txt\n",
        );
        assert_eq!(copy_includes(repo.path(), worktree.path()), 0);
        assert!(!worktree.path().join("inside.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        let repo = tempfile::tempdir().expect("repo");
        let worktree = tempfile::tempdir().expect("worktree");
        let outside = tempfile::tempdir().expect("outside");
        write(&outside.path().join("secret.txt"), "leak");
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            repo.path().join("link.txt"),
        )
        .expect("symlink");
        write(&repo.path().join(INCLUDE_FILE), "link.txt\n");

        assert_eq!(copy_includes(repo.path(), worktree.path()), 0);
        assert!(!worktree.path().join("link.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn glob_through_a_symlinked_directory_copies_nothing() {
        let repo = tempfile::tempdir().expect("repo");
        let worktree = tempfile::tempdir().expect("worktree");
        let outside = tempfile::tempdir().expect("outside");
        write(&outside.path().join("secret.txt"), "leak");
        // A committed directory symlink pointing out of the repo. `glob`'s `**`
        // descends it, so the matched file is a real file at a lexical path under
        // the repo — the canonical-containment guard must still reject it.
        std::os::unix::fs::symlink(outside.path(), repo.path().join("linkdir")).expect("symlink");
        write(
            &repo.path().join(INCLUDE_FILE),
            "linkdir/**/*.txt\nlinkdir\n",
        );

        assert_eq!(copy_includes(repo.path(), worktree.path()), 0);
        assert!(!worktree.path().join("linkdir").exists());
        assert!(!worktree.path().join("linkdir/secret.txt").exists());
    }
}
