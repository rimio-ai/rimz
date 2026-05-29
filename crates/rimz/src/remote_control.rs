//! Remote-control auto-launch behaviour, shared by Claude and Codex.
//!
//! When a [`crate::config::RemoteControlConfig`] toggle is set and that agent is
//! on PATH, `rimz start` launches its remote-control host in a single dedicated,
//! named background view of the workspace session (a tmux window / Zellij tab):
//!
//! - **Claude** runs `claude remote-control --spawn worktree`, a long-lived
//!   foreground host. It runs from the project root so `--spawn=worktree` carves
//!   new on-demand sessions off the canonical repo, not the current worktree.
//! - **Codex** runs `codex remote-control start`, which brings up the Codex
//!   app-server daemon with remote control enabled and returns. That daemon is
//!   the one Codex enrichment re-uses (see [`crate::agents::codex_app_server`]).
//!
//! Both share the one [`VIEW_NAME`] view, in separate panes. Neither host is a
//! coding agent — they have no Rimz hooks and never stamp a pane — so the
//! sidebar must not render them as idle agents. [`pane_is_host`] identifies a
//! host pane and [`host_label`] names it, so the snapshot reducer can give it a
//! dedicated, pinned row instead.

use crate::feed::PaneRef;

/// View name for the managed remote-control hosts. Shared by the launcher (the
/// idempotency key for the tmux window / Zellij tab) and the sidebar classifier
/// ([`pane_is_host`]), so both speak the same name. Claude and Codex share it.
pub const VIEW_NAME: &str = "rimz-rc";

/// Substring marking a remote-control subcommand in a pane's command line. Both
/// agents spell it `remote-control`, so this one marker catches either host.
const COMMAND_MARKER: &str = "remote-control";

/// Substring that distinguishes a Codex host from a Claude one. Zellij reports
/// the full command line and tmux reports the `codex` basename, so either way a
/// Codex host's command contains this.
const CODEX_MARKER: &str = "codex";

/// The Claude Remote Control argv (program first). `--spawn worktree` isolates
/// each on-demand remote session in its own git worktree — the worktree mode.
pub fn claude_command() -> Vec<String> {
    vec![
        "claude".to_owned(),
        "remote-control".to_owned(),
        "--spawn".to_owned(),
        "worktree".to_owned(),
    ]
}

/// The Codex remote-control argv (program first). `start` brings up the
/// app-server daemon with remote control enabled, then returns — so the pane
/// that runs it is launched `keep_open` to keep its start receipt on screen.
pub fn codex_command() -> Vec<String> {
    vec![
        "codex".to_owned(),
        "remote-control".to_owned(),
        "start".to_owned(),
    ]
}

/// Whether `pane` hosts a remote-control server (Claude or Codex).
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

/// The sidebar label for a host pane: `codex remote` when the command names
/// Codex, otherwise the canonical `remote control` (Claude or unattributed).
/// Never the bare agent name, so the row never reads as an idle coding agent.
pub fn host_label(pane: &PaneRef) -> &'static str {
    if pane
        .command
        .as_deref()
        .is_some_and(|command| command.contains(CODEX_MARKER))
    {
        "codex remote"
    } else {
        "remote control"
    }
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
    fn claude_command_uses_worktree_spawn() {
        assert_eq!(
            claude_command(),
            vec!["claude", "remote-control", "--spawn", "worktree"],
        );
    }

    #[test]
    fn codex_command_starts_the_daemon() {
        assert_eq!(codex_command(), vec!["codex", "remote-control", "start"]);
    }

    #[test]
    fn detects_either_host_by_full_command_line() {
        // Zellij reports the full command line; both agents spell the
        // subcommand `remote-control`.
        assert!(pane_is_host(&pane(
            Some("claude remote-control --spawn worktree"),
            None,
        )));
        assert!(pane_is_host(&pane(
            Some("codex remote-control start"),
            None
        )));
    }

    #[test]
    fn detects_host_by_view_name_when_command_is_a_bare_basename() {
        // tmux reports only the basename, but the window carries the view name.
        assert!(pane_is_host(&pane(Some("claude"), Some(VIEW_NAME))));
        assert!(pane_is_host(&pane(Some("codex"), Some(VIEW_NAME))));
        assert!(pane_is_host(&pane(Some("node"), Some(VIEW_NAME))));
    }

    #[test]
    fn a_plain_agent_is_not_the_host() {
        // A real coding session: bare basename, no rimz-rc view.
        assert!(!pane_is_host(&pane(Some("claude"), Some("2"))));
        assert!(!pane_is_host(&pane(Some("codex"), Some("3"))));
        assert!(!pane_is_host(&pane(Some("zsh"), None)));
    }

    #[test]
    fn host_label_attributes_codex_but_not_claude() {
        // Full command line (Zellij) and bare basename (tmux) both resolve.
        assert_eq!(
            host_label(&pane(Some("codex remote-control start"), None)),
            "codex remote"
        );
        assert_eq!(
            host_label(&pane(Some("codex"), Some(VIEW_NAME))),
            "codex remote"
        );
        assert_eq!(
            host_label(&pane(Some("claude remote-control --spawn worktree"), None)),
            "remote control",
        );
        // A bare-`claude` (or `node`) host in the rc window stays unattributed.
        assert_eq!(
            host_label(&pane(Some("node"), Some(VIEW_NAME))),
            "remote control"
        );
    }
}
