//! Best-effort registration of machine-local Git exclude patterns.
//!
//! Linked directories and team scratch files share Git's effective
//! `info/exclude`; Git remains the single source of truth for checkout status.

use std::path::{Path, PathBuf};

use super::git_stdout;

/// Register every team scratch pattern in the checkout's effective
/// `info/exclude`. Best-effort: failures warn and never block a launch.
pub fn exclude_team_scratch(checkout: &Path, patterns: &[String]) {
    if !patterns.is_empty() {
        ensure_excluded_patterns(checkout, patterns.iter().map(String::as_str));
    }
}

pub(super) fn ensure_excluded(checkout: &Path, pattern: &str) {
    ensure_excluded_patterns(checkout, [pattern]);
}

fn ensure_excluded_patterns<'a>(checkout: &Path, patterns: impl IntoIterator<Item = &'a str>) {
    let Some(exclude) = exclude_path(checkout) else {
        return;
    };
    let mut text = match std::fs::read_to_string(&exclude) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            tracing::warn!(path = %exclude.display(), error = %err, "reading git info/exclude");
            return;
        }
    };
    let mut changed = false;
    for pattern in patterns {
        if text.lines().any(|line| line == pattern) {
            continue;
        }
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(pattern);
        text.push('\n');
        changed = true;
    }
    if !changed {
        return;
    }
    if let Err(err) = crate::store::atomic::write_bytes_atomically(&exclude, text.as_bytes()) {
        tracing::warn!(path = %exclude.display(), error = %err, "writing git info/exclude");
    }
}

fn exclude_path(checkout: &Path) -> Option<PathBuf> {
    let raw = match git_stdout(checkout, ["rev-parse", "--git-path", "info/exclude"]) {
        Ok(raw) => raw,
        Err(err) => {
            if checkout.join(".git").exists() {
                tracing::warn!(path = %checkout.display(), error = %err, "resolving git info/exclude");
            }
            return None;
        }
    };
    if raw.is_empty() {
        return None;
    }
    let path = PathBuf::from(raw);
    Some(if path.is_absolute() {
        path
    } else {
        checkout.join(path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(cwd: &Path, args: &[&str]) -> Option<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }

    #[test]
    fn team_scratch_patterns_are_excluded_verbatim_and_idempotently() {
        let repo = tempfile::tempdir().expect("repo");
        if git(repo.path(), &["init", "-q", "-b", "main"]).is_none() {
            return;
        }
        std::fs::write(repo.path().join("plan.md"), "scratch\n").expect("scratch");
        std::fs::create_dir(repo.path().join("agent-notes")).expect("notes dir");
        std::fs::write(repo.path().join("agent-notes/coder.md"), "scratch\n")
            .expect("nested scratch");

        let patterns = vec!["/plan.md".to_owned(), "/agent-notes/*.md".to_owned()];
        exclude_team_scratch(repo.path(), &patterns);
        exclude_team_scratch(repo.path(), &patterns);

        let exclude = exclude_path(repo.path()).expect("exclude path");
        let text = std::fs::read_to_string(exclude).expect("exclude text");
        for pattern in &patterns {
            assert_eq!(
                text.lines().filter(|line| *line == pattern).count(),
                1,
                "{pattern}"
            );
        }
        assert_eq!(
            git(
                repo.path(),
                &["status", "--porcelain=v1", "--untracked-files=all"]
            )
            .expect("git status"),
            ""
        );
    }
}
