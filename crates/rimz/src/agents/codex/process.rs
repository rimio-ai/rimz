//! Codex process classifiers used by pane binding and daemon-session reaping.

use std::collections::BTreeSet;
use std::path::Path;

use crate::daemon_view::{APP_SERVER_MARKER, COMMAND_MARKER};
use crate::ids::AgentSessionId;

const CODEX_BINARY_MARKER: &str = "codex";

/// PIDs of the per-user Codex app-server daemon — the process a remote-control
/// Codex session records as its hook owner (`$PPID`). A daemon-mode session's
/// recorded pid is the shared daemon, which outlives any one conversation, so
/// matching a session's owner pid against this set is how the sidebar tells a
/// daemon-backed session (reapable only by the app-server's loaded-thread set)
/// from a standalone one whose pid is its own in-pane CLI (reapable by process
/// liveness). Best-effort: an unreadable `/proc` yields an empty set, which the
/// caller reads as "no daemon-mode sessions to reap".
///
/// Extra matches are inert. The set classifies a session only by an owner-pid
/// match, and no session records RimZ's own `rimz codex app-server …` broker or
/// proxy as its hook owner — so a stray codex-server pid that no session points
/// at simply never matches.
pub fn codex_daemon_pids() -> BTreeSet<u32> {
    crate::proc::list_processes()
        .into_iter()
        .filter(|process| is_codex_daemon_cmdline(&process.cmdline))
        .map(|process| process.pid)
        .collect()
}

/// Whether `pid` is the per-user Codex app-server daemon.
pub fn pid_is_codex_daemon(pid: u32) -> bool {
    crate::proc::cmdline(pid).is_some_and(|cmdline| is_codex_daemon_cmdline(&cmdline))
}

/// Whether a command line runs the Codex daemon: the `codex` binary on its
/// `app-server` or `remote-control` surface. Mirrors
/// [`crate::daemon_view::pane_is_host`]'s markers, narrowed to the `codex`
/// binary so an unrelated process that merely mentions a marker is not mistaken
/// for the daemon.
fn is_codex_daemon_cmdline(cmdline: &str) -> bool {
    let on_daemon_surface = cmdline.contains(APP_SERVER_MARKER) || cmdline.contains(COMMAND_MARKER);
    on_daemon_surface && cmdline.contains(CODEX_BINARY_MARKER)
}

/// Session id from a resumed Codex CLI command (`codex resume <session-id>`).
/// Exact rebirth binding reads this instead of guessing by cwd. The parser is
/// deliberately narrow: daemon/app-server surfaces are excluded by
/// [`is_codex_cli_cmdline`], and the session id is accepted only when it is the
/// token immediately after `resume`.
pub fn codex_resumed_session_id_from_cmdline(cmdline: &str) -> Option<AgentSessionId> {
    if !is_codex_cli_cmdline(cmdline) {
        return None;
    }
    let mut tokens = cmdline.split_whitespace().peekable();
    while let Some(token) = tokens.next() {
        let is_codex = Path::new(token)
            .file_name()
            .and_then(|file| file.to_str())
            .is_some_and(|file| crate::agents::program_names_kind(file, CODEX_BINARY_MARKER));
        if !is_codex {
            continue;
        }
        if tokens.next() != Some("resume") {
            return None;
        }
        return tokens
            .next()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(AgentSessionId::from);
    }
    None
}

/// Whether a command line runs the in-pane Codex CLI — the bare `codex` TUI a
/// user launches in a pane — rather than a process whose arguments merely name
/// Codex, the daemon, or the remote-control host. The effective program decides
/// the kind; arguments never classify an unrelated process as Codex.
pub(crate) fn is_codex_cli_cmdline(cmdline: &str) -> bool {
    crate::store::snapshot::command_agent_kind(cmdline) == Some(CODEX_BINARY_MARKER)
        && !is_codex_daemon_cmdline(cmdline)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_daemon_cmdline_matches_the_app_server_surface() {
        // The per-user daemon runs the codex binary on its daemon surface.
        assert!(is_codex_daemon_cmdline(
            "/home/u/.codex/packages/standalone/current/codex app-server"
        ));
        assert!(is_codex_daemon_cmdline("codex remote-control start"));
    }

    #[test]
    fn codex_daemon_cmdline_rejects_a_plain_session_or_other_server() {
        // A plain in-pane codex TUI is a standalone session, not the daemon —
        // process liveness reaps it, so it must not join the daemon set.
        assert!(!is_codex_daemon_cmdline("codex"));
        assert!(!is_codex_daemon_cmdline("codex --model gpt-5.5"));
        // A non-codex server that merely spells a marker is not the codex daemon.
        assert!(!is_codex_daemon_cmdline("some-other app-server"));
    }

    #[test]
    fn codex_cli_cmdline_matches_bare_cli_not_daemon() {
        // The in-pane TUI a user launches, including the npm `node` wrapper.
        assert!(is_codex_cli_cmdline("codex"));
        assert!(is_codex_cli_cmdline("codex --model gpt-5.5"));
        assert!(is_codex_cli_cmdline("node /usr/bin/codex"));
        assert!(is_codex_cli_cmdline("codex-aarch64-apple-darwin"));
        // The daemon, the remote-control host, and RimZ's broker all spell a
        // daemon surface, so none reads as the in-pane CLI.
        assert!(!is_codex_cli_cmdline("codex app-server"));
        assert!(!is_codex_cli_cmdline("codex remote-control start"));
        assert!(!is_codex_cli_cmdline(
            "rimz codex app-server serve --workspace-id w"
        ));
        // A non-codex process is never the codex CLI.
        assert!(!is_codex_cli_cmdline("zsh"));
        assert!(!is_codex_cli_cmdline(
            "rust-code-analysis-cli -m -l rust -O json -o /tmp/out -p crates/rimz/src/agents/codex/mod.rs"
        ));
        assert!(!is_codex_cli_cmdline("sudo npm install -g @openai/codex"));
    }

    #[test]
    fn codex_resume_cmdline_yields_session_id() {
        assert_eq!(
            codex_resumed_session_id_from_cmdline("codex resume 019ea276").as_deref(),
            Some("019ea276")
        );
        assert_eq!(
            codex_resumed_session_id_from_cmdline("codex-aarch64-apple-darwin resume 019ea276")
                .as_deref(),
            Some("019ea276")
        );
        assert_eq!(
            codex_resumed_session_id_from_cmdline("node /usr/bin/codex resume sess-2").as_deref(),
            Some("sess-2")
        );
        assert_eq!(
            codex_resumed_session_id_from_cmdline("codex --model gpt-5 resume sess"),
            None
        );
        assert_eq!(
            codex_resumed_session_id_from_cmdline("codex app-server resume sess"),
            None
        );
    }
}
