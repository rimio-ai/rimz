//! Claude Code Remote Control auto-launch behaviour.
//!
//! When [`crate::config::RemoteControlConfig::auto`] is set and `claude` is on
//! PATH, `rimz start` launches `claude remote-control` in a dedicated, named
//! background view of the workspace session (a tmux window / Zellij tab),
//! running from the project root so `--spawn=worktree` carves new on-demand
//! sessions off the canonical repo rather than the current worktree.
//!
//! The host process is not a coding agent — it has no Rimz hooks and never
//! stamps a pane — so the sidebar must not render it as an idle Claude agent.
//! [`pane_is_host`] identifies its pane so the snapshot reducer can give it a
//! dedicated, pinned row instead.

use crate::feed::PaneRef;

/// View name for the managed Remote Control instance. Shared by the launcher
/// (the idempotency key for the tmux window / Zellij tab) and the sidebar
/// classifier ([`pane_is_host`]), so both speak the same name.
pub const VIEW_NAME: &str = "rimz-rc";

/// Substring that marks the Remote Control subcommand in a pane's command line.
const COMMAND_MARKER: &str = "remote-control";

/// The Remote Control argv (program first). `--spawn worktree` isolates each
/// on-demand remote session in its own git worktree — the worktree spawn mode.
pub fn command() -> Vec<String> {
    vec![
        "claude".to_owned(),
        "remote-control".to_owned(),
        "--spawn".to_owned(),
        "worktree".to_owned(),
    ]
}

/// Whether `pane` hosts the Remote Control server.
///
/// Two signals, because the backends expose different metadata: Zellij reports
/// the full command line (so the `remote-control` subcommand is visible),
/// while tmux reports only the foreground binary basename but names the window
/// — which is the view name we launched it under.
pub fn pane_is_host(pane: &PaneRef) -> bool {
    pane.command
        .as_deref()
        .is_some_and(|command| command.contains(COMMAND_MARKER))
        || pane.view_name.as_deref() == Some(VIEW_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{MuxName, PaneId};

    fn pane(command: Option<&str>, view_name: Option<&str>) -> PaneRef {
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
            session_name: "rimz-demo".to_owned(),
            view_id: None,
            view_kind: None,
            view_name: view_name.map(ToOwned::to_owned),
            is_focused: false,
            command: command.map(ToOwned::to_owned),
            cwd: None,
            pane_pid: None,
            pane_process_start: None,
        }
    }

    #[test]
    fn command_uses_worktree_spawn() {
        assert_eq!(
            command(),
            vec!["claude", "remote-control", "--spawn", "worktree"],
        );
    }

    #[test]
    fn detects_host_by_full_command_line() {
        // Zellij reports the full command line.
        assert!(pane_is_host(&pane(
            Some("claude remote-control --spawn worktree"),
            None,
        )));
    }

    #[test]
    fn detects_host_by_view_name_when_command_is_a_bare_basename() {
        // tmux reports only `claude`, but the window carries the view name.
        assert!(pane_is_host(&pane(Some("claude"), Some(VIEW_NAME))));
        assert!(pane_is_host(&pane(Some("node"), Some(VIEW_NAME))));
    }

    #[test]
    fn a_plain_claude_agent_is_not_the_host() {
        // A real coding session: bare `claude`, no rimz-rc view.
        assert!(!pane_is_host(&pane(Some("claude"), Some("2"))));
        assert!(!pane_is_host(&pane(Some("zsh"), None)));
    }
}
