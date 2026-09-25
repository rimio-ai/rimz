//! Read-only, bounded git state enrichment for a launch.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

pub(super) struct GitState {
    pub status: String,
    pub head: String,
    pub log: String,
}

pub(super) fn read(cwd: &Path) -> Option<GitState> {
    Some(GitState {
        status: output(cwd, &["status", "--short"])?,
        head: output(cwd, &["rev-parse", "--short", "HEAD"])?,
        log: output(cwd, &["log", "-1", "--oneline"])?,
    })
}

fn output(cwd: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(cwd)
        .args(["-c", "color.ui=never"])
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null());
    let output = match crate::proc::run_bounded_output(&mut command, Duration::from_secs(5)) {
        Ok(output) => output,
        Err(error) => {
            tracing::debug!(?cwd, ?args, %error, "git launch reminder unavailable");
            return None;
        }
    };
    if output.timed_out || !output.status.success() {
        tracing::debug!(?cwd, ?args, status = %output.status, timed_out = output.timed_out, "git launch reminder unavailable");
        return None;
    }
    match String::from_utf8(output.stdout) {
        Ok(text) => Some(text.trim_end_matches('\n').to_owned()),
        Err(error) => {
            tracing::debug!(?cwd, ?args, %error, "git launch reminder is not UTF-8");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(cwd: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .current_dir(cwd)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        String::from_utf8(output.stdout)
            .unwrap()
            .trim_end_matches('\n')
            .to_owned()
    }

    #[test]
    fn git_state_reads_clean_and_dirty_without_refreshing_index() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q"]);
        std::fs::write(repo.path().join("tracked"), "base").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-qm", "base"]);
        let index = repo.path().join(".git/index");
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1);
        std::fs::File::options()
            .write(true)
            .open(&index)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert_eq!(read(repo.path()).expect("clean state").status, "");
        std::fs::write(repo.path().join("file"), "new").unwrap();
        let state = read(repo.path()).expect("dirty state");
        assert_eq!(state.status, "?? file");
        assert_eq!(
            state.head,
            git(repo.path(), &["rev-parse", "--short", "HEAD"])
        );
        assert_eq!(state.log, format!("{} base", state.head));
        assert_eq!(std::fs::metadata(index).unwrap().modified().unwrap(), old);
        assert!(!repo.path().join(".git/index.lock").exists());
    }

    #[test]
    fn git_state_omits_non_repo_missing_path_and_unborn_head() {
        let repo = tempfile::tempdir().unwrap();
        assert!(read(repo.path()).is_none());
        assert!(read(&repo.path().join("missing")).is_none());
        git(repo.path(), &["init", "-q"]);
        assert!(read(repo.path()).is_none());
    }
}
