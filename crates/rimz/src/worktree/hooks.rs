//! Machine-local lifecycle commands, captured and bounded before returning to the caller.

use std::process::{Command, Stdio};
use std::time::Duration;

use crate::config::WorktreeHooks;

use super::WorktreeMarker;

pub(super) const TIMEOUT: Duration = Duration::from_secs(10 * 60);
const ENV_HOOK_EVENT: &str = "RIMZ_HOOK_EVENT";
const ENV_WORKTREE_NAME: &str = "RIMZ_WORKTREE_NAME";
const ENV_WORKTREE_BRANCH: &str = "RIMZ_WORKTREE_BRANCH";
const ENV_WORKTREE_REPO_ROOT: &str = "RIMZ_WORKTREE_REPO_ROOT";

#[derive(Clone, Copy)]
pub(super) enum WorktreeHookEvent {
    Created,
    Removed,
}

impl WorktreeHookEvent {
    fn as_str(self) -> &'static str {
        match self {
            Self::Created => "worktree.created",
            Self::Removed => "worktree.removed",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum WorktreeHookErr {
    #[error("could not run hook: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("{reason}\nstdout:\n{stdout}\nstderr:\n{stderr}")]
    Failed {
        reason: String,
        stdout: String,
        stderr: String,
    },
}

pub(super) fn run_hook(
    hooks: &WorktreeHooks,
    event: WorktreeHookEvent,
    marker: &WorktreeMarker,
    timeout: Duration,
) -> Result<(), WorktreeHookErr> {
    let (command, cwd) = match event {
        WorktreeHookEvent::Created => (&hooks.created, &marker.worktree_path),
        WorktreeHookEvent::Removed => (&hooks.removed, &marker.repo_root),
    };
    let Some(command) = command else {
        return Ok(());
    };
    let mut child = Command::new("sh");
    child
        .args(["-c", command])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .env(ENV_HOOK_EVENT, event.as_str())
        .env(ENV_WORKTREE_NAME, &marker.name)
        .env(crate::workspace::ENV_WORKTREE_PATH, &marker.worktree_path)
        .env(ENV_WORKTREE_BRANCH, &marker.branch)
        .env(ENV_WORKTREE_REPO_ROOT, &marker.repo_root);
    let output = crate::proc::run_bounded_output(&mut child, timeout)?;
    if output.status.success() && !output.timed_out {
        return Ok(());
    }
    Err(WorktreeHookErr::Failed {
        reason: if output.timed_out {
            format!("timed out after {}s", timeout.as_secs_f64())
        } else {
            output.status.to_string()
        },
        stdout: output_tail(&output.stdout),
        stderr: output_tail(&output.stderr),
    })
}

fn output_tail(bytes: &[u8]) -> String {
    let bounded = crate::proc::tail_output(bytes, 4096);
    let mut lines = bounded.lines().rev().take(20).collect::<Vec<_>>();
    lines.reverse();
    lines.join("\n")
}
