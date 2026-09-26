//! Worktree and caller resolution shared by the board commands.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use rimz::address::TeamCohort;
use rimz::agents::AgentState;
use rimz::harness::ancestry::{resolve_caller, resolve_launch_caller};
use rimz::utils::path::normalize_path_lexical;

pub(super) fn worktree() -> Result<PathBuf> {
    let worktree = match std::env::var_os(rimz::workspace::ENV_WORKTREE_PATH) {
        Some(path) => normalize_path_lexical(Path::new(&path)),
        None => {
            let output = Command::new("git")
                .args(["rev-parse", "--show-toplevel"])
                .current_dir(std::env::current_dir()?)
                .output()
                .context("resolve the current worktree with git")?;
            if !output.status.success() {
                bail!("cannot resolve the current worktree; run from a git worktree");
            }
            let path =
                String::from_utf8(output.stdout).context("git worktree path is not UTF-8")?;
            normalize_path_lexical(Path::new(path.trim()))
        }
    };
    Ok(worktree)
}

pub(super) fn caller(agents: &[AgentState]) -> Result<Option<&AgentState>> {
    Ok(resolve_caller(agents)
        .map(|caller| resolve_launch_caller(agents, &caller))
        .transpose()?)
}

pub(super) fn in_worktree(cohort: &TeamCohort<'_>, worktree: &Path) -> bool {
    cohort.members.iter().all(|member| {
        member
            .worktree_path
            .as_deref()
            .is_some_and(|path| normalize_path_lexical(Path::new(path)) == worktree)
    })
}
