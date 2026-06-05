//! Remote-control auto-launch behaviour for Claude and Codex.
//!
//! When a [`crate::config::RemoteControlConfig`] toggle is set and that agent
//! can start, `rimz start` brings its host up — but the two have different
//! lifecycles, so they launch differently:
//!
//! - **Claude** runs `claude remote-control --spawn worktree`, a long-lived
//!   foreground host, in the workspace session's one named [`VIEW_NAME`]
//!   background view (a tmux window / Zellij tab). It runs from the project root
//!   so `--spawn=worktree` carves new on-demand sessions off the canonical repo,
//!   not the current worktree. It is a pane but not a coding agent — no Rimz
//!   hooks, never stamps a pane — so the sidebar must not render it as an idle
//!   agent: [`pane_is_host`] identifies the host pane and the snapshot reducer
//!   filters it out, surfacing remote control as a `⇅ rc` flag on the Claude
//!   provider dashboard block instead.
//! - **Codex** runs `remote-control start` from the *managed standalone install*
//!   ([`codex_standalone_bin`]), which brings up the Codex app-server daemon
//!   with remote control enabled and returns. That daemon is a **per-user
//!   singleton** (one control socket), so it is *not* a per-workspace pane:
//!   [`ensure_codex_daemon`] spawns the (idempotent) start command detached with
//!   null stdio, and Codex enrichment reaches the daemon over the control socket
//!   (see [`crate::agents::codex::app_server`]).
//!
//! `remote-control start` boots and updates its daemon from the standalone's
//! fixed path, so a `codex` merely on PATH (a different binary) is not enough.
//! When the `codex` toggle is on but that install is absent, [`preflight`]
//! refuses the start with the fix — fail-fast, rather than ensuring a daemon
//! that only prints an install error.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::agents::codex::app_server::codex_home;
use crate::config::RemoteControlConfig;
use crate::feed::PaneRef;

/// View name for the managed daemon tab. Shared by the launcher (the idempotency
/// key for the tmux window / Zellij tab) and the sidebar classifier
/// ([`pane_is_host`]), so both speak the same name. The tab hosts the Claude
/// remote-control host and the per-session Codex app-server broker side by side.
pub const VIEW_NAME: &str = "rimzd";

/// Substring marking the Claude remote-control host in a pane's command line —
/// the subcommand it spells (`claude remote-control …`).
const COMMAND_MARKER: &str = "remote-control";

/// Substring marking the Codex app-server broker in a pane's command line
/// (`rimz codex app-server serve …`). The broker is a per-session host pane in
/// the same view, distinct from the per-user daemon [`ensure_codex_daemon`] runs.
const APP_SERVER_MARKER: &str = "app-server";

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

/// The Codex remote-control argv (program first), invoked through `bin` — the
/// managed standalone install from [`codex_standalone_bin`]. `start` brings up
/// the app-server daemon with remote control enabled, then returns. Invoking the
/// standalone path directly means the launch never depends on a `codex` being on
/// PATH, and runs exactly the binary the daemon updates from.
pub fn codex_command(bin: &Path) -> Vec<String> {
    vec![
        bin.to_string_lossy().into_owned(),
        "remote-control".to_owned(),
        "start".to_owned(),
    ]
}

/// Ensure the per-user Codex app-server daemon is running when `[remote_control]
/// codex` is on and the managed standalone install resolves. The daemon is a
/// per-user singleton (one control socket), so it is ensured once here rather
/// than parked in a per-workspace pane; enrichment reaches it over the socket.
/// Best-effort, gated by [`should_ensure_codex_daemon`].
pub fn ensure_codex_daemon(config: &RemoteControlConfig) {
    let standalone = codex_standalone_bin();
    if !should_ensure_codex_daemon(config.codex, standalone.is_some()) {
        return;
    }
    // The gate above guarantees the standalone resolved.
    if let Some(bin) = standalone {
        spawn_codex_daemon(&bin);
    }
}

/// The pure ensure-daemon decision, split from [`ensure_codex_daemon`] so the
/// matrix is unit-testable without touching the filesystem: ensure iff the
/// toggle is on *and* the managed standalone install is present (a `codex` on
/// PATH does not satisfy `remote-control start` — see [`codex_standalone_bin`]).
fn should_ensure_codex_daemon(codex_enabled: bool, standalone_present: bool) -> bool {
    codex_enabled && standalone_present
}

/// Spawn `codex remote-control start` from the managed standalone `bin` detached,
/// with all stdio nulled, and drop the child without waiting. The command is
/// idempotent — it no-ops once the per-user daemon is up — and returns as soon as
/// the daemon is running, so this adds no latency and prints nothing to the
/// terminal. Best-effort: a spawn failure is logged and ignored, because the
/// app-server is enrichment, not correctness — the proxy client cold-spawns a
/// server when the daemon is absent.
fn spawn_codex_daemon(bin: &Path) {
    let argv = codex_command(bin);
    let mut parts = argv.iter();
    let Some(program) = parts.next() else {
        return;
    };
    let mut cmd = Command::new(program);
    cmd.args(parts)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Err(err) = cmd.spawn() {
        tracing::warn!(error = %err, "failed to spawn the codex app-server daemon");
    }
}

/// The managed standalone Codex install `codex remote-control start` boots its
/// daemon from: `$CODEX_HOME/packages/standalone/current/codex` (CODEX_HOME
/// defaults to `~/.codex`). Returns the path only when it exists, so callers can
/// gate on a host that can actually start. A `codex` on PATH is a different
/// binary and does not satisfy this — see [`preflight`].
pub fn codex_standalone_bin() -> Option<PathBuf> {
    standalone_bin_under(&codex_home()?)
}

/// [`codex_standalone_bin`] rooted at an explicit Codex home — split out pure so
/// tests can point at a tempdir without touching `CODEX_HOME` or `HOME`.
fn standalone_bin_under(codex_home: &Path) -> Option<PathBuf> {
    let bin = codex_home
        .join("packages")
        .join("standalone")
        .join("current")
        .join("codex");
    bin.is_file().then_some(bin)
}

/// The official one-liner that installs the managed standalone Codex. Surfaced
/// verbatim by [`PreflightError`] and `rimz doctor`, so the guidance never
/// drifts from one place to the other.
pub const CODEX_INSTALL_COMMAND: &str = "curl -fsSL https://chatgpt.com/codex/install.sh | sh";

/// A configured remote-control host cannot start. Returned by [`preflight`] so
/// `rimz start` refuses up front with the fix, instead of launching a doomed
/// host. Fail-fast precondition, not best-effort: sidebar wakeups and app-server
/// enrichment degrade silently, but a capability the user switched on does not.
#[derive(Debug, PartialEq, Eq)]
pub enum PreflightError {
    /// `[remote_control] codex = true` but the managed standalone install is
    /// absent. The `Display` carries the full, user-facing fix.
    CodexStandaloneMissing,
}

impl std::fmt::Display for PreflightError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CodexStandaloneMissing => write!(
                f,
                "Codex remote-control is enabled (`[remote_control] codex = true`) but the \
                 managed standalone Codex install is missing.\n\
                 `codex remote-control start` boots its app-server daemon from \
                 `$CODEX_HOME/packages/standalone/current/codex` (CODEX_HOME defaults to \
                 `~/.codex`); a `codex` on PATH is a different binary and does not satisfy it.\n\n\
                 Install it with:\n    {CODEX_INSTALL_COMMAND}\n\n\
                 then re-run, or set `[remote_control] codex = false` to disable the Codex host."
            ),
        }
    }
}

impl std::error::Error for PreflightError {}

/// Refuse `rimz start` when a configured remote-control host cannot possibly
/// start, so the user gets the fix instead of a workspace built around a host
/// that only errors. Codex's `remote-control start` requires the managed
/// standalone install ([`codex_standalone_bin`]); when `codex` is enabled that
/// install must exist. Claude has no such precondition — a missing `claude` is
/// skipped at launch (best-effort), so it never blocks a start.
pub fn preflight(config: &RemoteControlConfig) -> Result<(), PreflightError> {
    preflight_decision(config.codex, codex_standalone_bin().is_some())
}

/// The pure preflight decision, split from [`preflight`] so the full matrix is
/// unit-testable without touching the filesystem.
fn preflight_decision(
    codex_enabled: bool,
    codex_standalone_present: bool,
) -> Result<(), PreflightError> {
    if codex_enabled && !codex_standalone_present {
        return Err(PreflightError::CodexStandaloneMissing);
    }
    Ok(())
}

/// Whether `pane` hosts a managed daemon — the Claude remote-control host or the
/// Codex app-server broker. Both live in the [`VIEW_NAME`] view.
///
/// Two signals, because the backends expose different metadata: Zellij reports
/// the full command line (so the `remote-control` / `app-server` subcommand is
/// visible), while tmux reports only the foreground binary basename but names the
/// window — which is the view name we launched it under, catching either host.
pub fn command_is_host(command: &str) -> bool {
    command.contains(COMMAND_MARKER) || command.contains(APP_SERVER_MARKER)
}

pub fn pane_is_host(pane: &PaneRef) -> bool {
    pane.command.as_deref().is_some_and(command_is_host)
        || pane.view_name.as_deref() == Some(VIEW_NAME)
}

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
/// match, and no session records Rimz's own `rimz codex app-server …` broker or
/// proxy as its hook owner — so a stray codex-server pid that no session points at
/// simply never matches.
pub fn codex_daemon_pids() -> std::collections::BTreeSet<u32> {
    crate::proc::list_processes()
        .into_iter()
        .filter(|process| is_codex_daemon_cmdline(&process.cmdline))
        .map(|process| process.pid)
        .collect()
}

/// Whether a command line runs the Codex daemon: the `codex` binary on its
/// `app-server` or `remote-control` surface. Mirrors [`pane_is_host`]'s markers,
/// narrowed to the `codex` binary so an unrelated process that merely mentions a
/// marker is not mistaken for the daemon.
fn is_codex_daemon_cmdline(cmdline: &str) -> bool {
    let on_daemon_surface = cmdline.contains(APP_SERVER_MARKER) || cmdline.contains(COMMAND_MARKER);
    on_daemon_surface && cmdline.contains("codex")
}

/// Start time of the in-pane agent CLI process backing a live pane, found by
/// working directory — the signal a backend that reports no per-pane process
/// start (Zellij) needs so the cwd fallback can refuse a stale session. Codex is
/// the only lazy-registering agent today, so this resolves the bare `codex` TUI
/// ([`is_codex_cli_cmdline`]) whose `/proc` cwd equals `pane_cwd` and returns the
/// *earliest* such start: with that floor the sidebar's `pane_start_allows_bind`
/// guard only rejects a session predating every candidate, so a cwd hosting more
/// than one `codex` never hides a live one. `None` for a non-Codex kind, no
/// match, or an unreadable `/proc` (another user's process).
pub fn in_pane_agent_start(kind: &str, pane_cwd: &str) -> Option<jiff::Timestamp> {
    if kind != "codex" {
        return None;
    }
    let pane_cwd = Path::new(pane_cwd);
    crate::proc::list_processes()
        .into_iter()
        .filter(|process| is_codex_cli_cmdline(&process.cmdline))
        .filter(|process| crate::proc::cwd(process.pid).as_deref() == Some(pane_cwd))
        .filter_map(|process| crate::proc::process_start(process.pid))
        .min()
}

/// Start time of the in-pane agent CLI behind a pane's bound root process —
/// the per-pane exact signal the frame stamp prefers over the cwd scan above.
/// The root is the CLI itself when its cmdline reads as the bare `codex` TUI
/// (a pane running it directly); a shell-hosted CLI is the root's single
/// child, since the mux reports the *foreground* command while the root stays
/// the shell. The cmdline check is load-bearing twice over: a shell outlives
/// the agents it hosts, so stamping its older start would re-admit the very
/// sessions `pane_start_allows_bind` refuses, and a re-run CLI is a fresh
/// child pid even when the hosting shell survives, so re-tenancy stays
/// visible. `None` for a non-Codex kind or when neither process reads as the
/// CLI, so the caller falls back rather than guesses.
pub fn in_pane_agent_start_for_root(kind: &str, root_pid: u32) -> Option<jiff::Timestamp> {
    if kind != "codex" {
        return None;
    }
    if crate::proc::cmdline(root_pid).is_some_and(|cmdline| is_codex_cli_cmdline(&cmdline)) {
        return crate::proc::process_start(root_pid);
    }
    if let &[child] = crate::proc::children(root_pid).as_slice()
        && crate::proc::cmdline(child).is_some_and(|cmdline| is_codex_cli_cmdline(&cmdline))
    {
        return crate::proc::process_start(child);
    }
    None
}

/// Whether a command line runs the in-pane Codex CLI — the bare `codex` TUI a
/// user launches in a pane — rather than the daemon, the remote-control host, or
/// Rimz's own `rimz codex app-server serve` broker. The inverse of
/// [`is_codex_daemon_cmdline`] within the `codex` binary: those all spell
/// `app-server` or `remote-control`, so excluding them leaves the plain CLI.
fn is_codex_cli_cmdline(cmdline: &str) -> bool {
    cmdline.contains("codex") && !is_codex_daemon_cmdline(cmdline)
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
            rss_kb: None,
            cpu_pct: None,
            io_bps: None,
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
    fn codex_command_runs_the_standalone_bin() {
        let bin = Path::new("/home/u/.codex/packages/standalone/current/codex");
        assert_eq!(
            codex_command(bin),
            vec![
                "/home/u/.codex/packages/standalone/current/codex",
                "remote-control",
                "start",
            ],
        );
    }

    #[test]
    fn standalone_bin_resolves_only_when_the_install_exists() {
        let home = tempfile::tempdir().expect("tempdir");
        // Absent install → no host: `remote-control start` would only error.
        assert!(standalone_bin_under(home.path()).is_none());

        let bin = home
            .path()
            .join("packages")
            .join("standalone")
            .join("current")
            .join("codex");
        std::fs::create_dir_all(bin.parent().expect("parent")).expect("mkdir");
        std::fs::write(&bin, b"#!/bin/sh\n").expect("write");
        assert_eq!(standalone_bin_under(home.path()), Some(bin));
    }

    #[test]
    fn preflight_blocks_only_codex_without_its_standalone() {
        // codex off → never blocks, install present or not.
        assert!(preflight_decision(false, false).is_ok());
        assert!(preflight_decision(false, true).is_ok());
        // codex on → blocks iff the standalone install is absent.
        assert_eq!(
            preflight_decision(true, false),
            Err(PreflightError::CodexStandaloneMissing),
        );
        assert!(preflight_decision(true, true).is_ok());
    }

    #[test]
    fn preflight_error_carries_the_official_install_command() {
        let msg = PreflightError::CodexStandaloneMissing.to_string();
        assert!(
            msg.contains(CODEX_INSTALL_COMMAND),
            "guidance names the installer"
        );
        assert!(
            msg.contains("[remote_control] codex"),
            "guidance names the toggle"
        );
    }

    #[test]
    fn ensure_codex_daemon_requires_toggle_and_standalone() {
        // codex off → never ensure, install present or not.
        assert!(!should_ensure_codex_daemon(false, false));
        assert!(!should_ensure_codex_daemon(false, true));
        // codex on → ensure iff the managed standalone install is present.
        assert!(!should_ensure_codex_daemon(true, false));
        assert!(should_ensure_codex_daemon(true, true));
    }

    #[test]
    fn detects_both_hosts_by_full_command_line() {
        // Zellij reports the full command line. Claude spells the subcommand
        // `remote-control`; the broker spells `app-server`.
        assert!(pane_is_host(&pane(
            Some("claude remote-control --spawn worktree"),
            None,
        )));
        assert!(pane_is_host(&pane(
            Some("rimz codex app-server serve --workspace-id w"),
            None,
        )));
    }

    #[test]
    fn detects_host_by_view_name_when_command_is_a_bare_basename() {
        // tmux reports only the basename, but the window carries the view name,
        // so any pane in the rimzd view is a host regardless of its command.
        assert!(pane_is_host(&pane(Some("claude"), Some(VIEW_NAME))));
        assert!(pane_is_host(&pane(Some("rimz"), Some(VIEW_NAME))));
    }

    #[test]
    fn a_plain_agent_is_not_the_host() {
        // A real coding session: bare basename, no rimzd view. A plain `codex`
        // agent pane must never be classified as a host.
        assert!(!pane_is_host(&pane(Some("claude"), Some("2"))));
        assert!(!pane_is_host(&pane(Some("codex"), Some("3"))));
        assert!(!pane_is_host(&pane(Some("zsh"), None)));
    }

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
        // The daemon, the remote-control host, and Rimz's broker all spell a
        // daemon surface, so none reads as the in-pane CLI.
        assert!(!is_codex_cli_cmdline("codex app-server"));
        assert!(!is_codex_cli_cmdline("codex remote-control start"));
        assert!(!is_codex_cli_cmdline(
            "rimz codex app-server serve --workspace-id w"
        ));
        // A non-codex process is never the codex CLI.
        assert!(!is_codex_cli_cmdline("zsh"));
    }
}
