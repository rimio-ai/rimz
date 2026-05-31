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
//!   agent: [`pane_is_host`] identifies the host pane and [`host_label`] names
//!   it, so the snapshot reducer gives it a dedicated, pinned row instead.
//! - **Codex** runs `remote-control start` from the *managed standalone install*
//!   ([`codex_standalone_bin`]), which brings up the Codex app-server daemon
//!   with remote control enabled and returns. That daemon is a **per-user
//!   singleton** (one control socket), so it is *not* a per-workspace pane:
//!   [`ensure_codex_daemon`] spawns the (idempotent) start command detached with
//!   null stdio, and Codex enrichment reaches the daemon over the control socket
//!   (see [`crate::agents::codex_app_server`]).
//!
//! `remote-control start` boots and updates its daemon from the standalone's
//! fixed path, so a `codex` merely on PATH (a different binary) is not enough.
//! When the `codex` toggle is on but that install is absent, [`preflight`]
//! refuses the start with the fix — fail-fast, rather than ensuring a daemon
//! that only prints an install error.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::agents::codex_app_server::codex_home;
use crate::config::RemoteControlConfig;
use crate::feed::PaneRef;

/// View name for the managed remote-control hosts. Shared by the launcher (the
/// idempotency key for the tmux window / Zellij tab) and the sidebar classifier
/// ([`pane_is_host`]), so both speak the same name. Claude and Codex share it.
pub const VIEW_NAME: &str = "rimz-rc";

/// Substring marking a remote-control subcommand in a pane's command line. Only
/// Claude is a host pane now, and it spells the subcommand `remote-control`.
const COMMAND_MARKER: &str = "remote-control";

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

/// Whether `pane` hosts a remote-control server. Only Claude is a host pane now
/// (Codex is a per-user daemon, never a pane).
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

/// The sidebar label for a host pane. Only Claude is a host pane now (Codex is a
/// per-user daemon, never a pane), so the row reads the canonical `remote
/// control` — never a bare agent name, so it never reads as an idle coding agent.
pub fn host_label(_pane: &PaneRef) -> &'static str {
    "remote control"
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
            view_active: None,
            session_attached: None,
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
    fn detects_the_claude_host_by_full_command_line() {
        // Zellij reports the full command line; Claude spells the subcommand
        // `remote-control`.
        assert!(pane_is_host(&pane(
            Some("claude remote-control --spawn worktree"),
            None,
        )));
    }

    #[test]
    fn detects_host_by_view_name_when_command_is_a_bare_basename() {
        // tmux reports only the basename, but the window carries the view name,
        // so any pane in the rimz-rc view is a host regardless of its command.
        assert!(pane_is_host(&pane(Some("claude"), Some(VIEW_NAME))));
        assert!(pane_is_host(&pane(Some("node"), Some(VIEW_NAME))));
    }

    #[test]
    fn a_plain_agent_is_not_the_host() {
        // A real coding session: bare basename, no rimz-rc view. A plain `codex`
        // agent pane must never be classified as a host.
        assert!(!pane_is_host(&pane(Some("claude"), Some("2"))));
        assert!(!pane_is_host(&pane(Some("codex"), Some("3"))));
        assert!(!pane_is_host(&pane(Some("zsh"), None)));
    }

    #[test]
    fn host_label_is_the_canonical_remote_control() {
        // Only Claude is a host pane now; every host row reads `remote control`,
        // never a bare agent name.
        assert_eq!(
            host_label(&pane(Some("claude remote-control --spawn worktree"), None)),
            "remote control",
        );
        assert_eq!(
            host_label(&pane(Some("claude"), Some(VIEW_NAME))),
            "remote control",
        );
        assert_eq!(
            host_label(&pane(Some("node"), Some(VIEW_NAME))),
            "remote control",
        );
    }
}
