//! Remote-control hosts and dashboard-pane classification.
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
//! skips that inert host so the room still starts, and `rimz doctor` surfaces
//! the install fix. Claude has version- and settings-gated preconditions: old
//! binaries lack remote control, `disableRemoteControl` blocks the surface,
//! newer agent-view settings can kill the host, and API-key auth disables
//! remote control on affected releases. Those installed-but-blocked cases stay
//! fail-fast at `rimz start`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::agents::claude::remote_control as claude_rc;
use crate::agents::codex::app_server::codex_home;
use crate::agents::version::CliVersion;
use crate::config::RemoteControlConfig;
use crate::pane::PaneRef;

/// View name for the managed daemon tab. Shared by the launcher (the idempotency
/// key for the tmux window / Zellij tab) and the sidebar classifier
/// ([`pane_is_host`]), so both speak the same name. The tab hosts configurable
/// content in the middle (live stats by default) and stacks the Claude
/// remote-control host and per-session Codex app-server broker on the right
/// when they apply.
pub const VIEW_NAME: &str = "rimzd";

/// Substring marking the Claude remote-control host in a pane's command line —
/// the subcommand it spells (`claude remote-control …`).
pub(crate) const COMMAND_MARKER: &str = "remote-control";

/// Substring marking the Codex app-server broker in a pane's command line
/// (`rimz codex app-server serve …`). The broker is a per-session host pane in
/// the same view, distinct from the per-user daemon [`ensure_codex_daemon`] runs.
pub(crate) const APP_SERVER_MARKER: &str = "app-server";

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

/// The daemon-host Claude argv. Pane launches deliberately set
/// [`claude_rc::DISABLE_AGENT_VIEW_ENV`] so a plain `claude` opens the classic
/// REPL, but the remote-control host needs the agent-view supervisor on newer
/// Claude Code builds. `env -u` unsets only that key while preserving the rest
/// of the inherited environment.
pub fn claude_host_argv() -> Vec<String> {
    let mut argv = vec![
        "env".to_owned(),
        "-u".to_owned(),
        claude_rc::DISABLE_AGENT_VIEW_ENV.to_owned(),
    ];
    argv.extend(claude_command());
    argv
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
/// with all stdio nulled, and hand it to the shared reaper. The command is
/// idempotent — it no-ops once the per-user daemon is up — and returns as soon
/// as the daemon is running, so this adds no latency and prints nothing to the
/// terminal. Best-effort: a spawn failure is logged and ignored, because the
/// app-server is enrichment, not correctness — the enrichment client cold-spawns
/// a server when the daemon is absent.
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
    if let Err(err) = crate::child_process::spawn_detached_reaped(&mut cmd, "codex-daemon") {
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

/// A configured remote-control host cannot start. [`preflight`] skips
/// uninstalled hosts so the room still launches, while installed agents with
/// fixable misconfigurations make `rimz start` refuse up front with the fix.
/// `rimz doctor` surfaces both categories.
#[derive(Debug, PartialEq, Eq)]
pub enum PreflightError {
    /// `[remote_control] codex = true` but the managed standalone install is
    /// absent. `rimz start` skips this host; the `Display` carries the
    /// user-facing install fix for `rimz doctor`.
    CodexStandaloneMissing,
    /// `[remote_control] claude = true` but the installed Claude Code version is
    /// older than remote-control support.
    ClaudeTooOld { found: CliVersion },
    /// Claude's own settings explicitly disable remote control.
    ClaudeRemoteControlDisabled { settings_path: PathBuf },
    /// Newer Claude Code hosts remote control through the agent-view surface,
    /// and that surface is disabled in settings.
    ClaudeAgentViewDisabled {
        settings_path: PathBuf,
        found: CliVersion,
    },
    /// Claude Code disables remote control when API-key auth is active on
    /// affected versions.
    ClaudeAuthConflict {
        sources: Vec<ClaudeAuthConflictSource>,
    },
}

impl PreflightError {
    /// Whether this refusal is an enabled host whose agent is not installed.
    /// `rimz start` skips these so the room still launches; `rimz doctor`
    /// reports them as advisories with the install fix.
    pub fn is_uninstalled_host(&self) -> bool {
        matches!(self, Self::CodexStandaloneMissing)
    }
}

/// A configured auth source that disables Claude remote control on affected
/// Claude Code versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaudeAuthConflictSource {
    ApiKeyEnv,
    AuthTokenEnv,
    ApiKeyHelperSetting,
    SettingsEnv,
}

impl std::fmt::Display for ClaudeAuthConflictSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ApiKeyEnv => write!(f, "ANTHROPIC_API_KEY in the launch environment"),
            Self::AuthTokenEnv => write!(f, "ANTHROPIC_AUTH_TOKEN in the launch environment"),
            Self::ApiKeyHelperSetting => write!(f, "apiKeyHelper in Claude settings"),
            Self::SettingsEnv => write!(
                f,
                "ANTHROPIC_API_KEY/ANTHROPIC_AUTH_TOKEN in Claude settings env"
            ),
        }
    }
}

impl std::fmt::Display for PreflightError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CodexStandaloneMissing => write!(
                f,
                "Codex remote-control is enabled (`[remote_control] codex = true`) but the \
                 managed standalone Codex install is missing, so `rimz start` brings the \
                 room up without the Codex remote-control host.\n\
                 `codex remote-control start` boots its app-server daemon from \
                 `$CODEX_HOME/packages/standalone/current/codex` (CODEX_HOME defaults to \
                 `~/.codex`); a `codex` on PATH is a different binary and does not satisfy it.\n\n\
                 Install it with:\n    {CODEX_INSTALL_COMMAND}\n\n\
                 then re-run to enable the host, or set `[remote_control] codex = false` to \
                 silence this."
            ),
            Self::ClaudeTooOld { found } => write!(
                f,
                "Claude remote-control is enabled (`[remote_control] claude = true`) but \
                 `claude --version` reports {found}; remote control requires Claude Code \
                 >= {}.\n\n\
                 Upgrade Claude Code, then re-run, or set `[remote_control] claude = false` \
                 to disable the Claude host.",
                claude_rc::MIN_REMOTE_CONTROL,
            ),
            Self::ClaudeRemoteControlDisabled { settings_path } => write!(
                f,
                "Claude remote-control is enabled (`[remote_control] claude = true`) but \
                 `disableRemoteControl: true` in {} blocks it.\n\n\
                 Remove that setting or set it to false, then re-run, or set \
                 `[remote_control] claude = false` to disable the Claude host.",
                settings_path.display(),
            ),
            Self::ClaudeAgentViewDisabled {
                settings_path,
                found,
            } => write!(
                f,
                "Claude remote-control is enabled (`[remote_control] claude = true`) but \
                 `disableAgentView: true` in {} blocks the remote-control host on Claude \
                 Code {found}.\n\n\
                 Remove that setting or set it to false, then re-run, or set \
                 `[remote_control] claude = false` to disable the Claude host.",
                settings_path.display(),
            ),
            Self::ClaudeAuthConflict { sources } => write!(
                f,
                "Claude remote-control is enabled (`[remote_control] claude = true`) but \
                 Claude Code disables remote control when API-key auth is active on this \
                 version. Conflicting source(s): {}.\n\n\
                 Remove those auth sources and use a claude.ai login for remote control, \
                 then re-run, or set `[remote_control] claude = false` to disable the \
                 Claude host.",
                sources
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        }
    }
}

impl std::error::Error for PreflightError {}

/// Gate `rimz start` for configured remote-control hosts. An enabled host whose
/// agent is not installed is skipped so the room still launches; an installed
/// agent with a fixable misconfiguration refuses the start with the fix. Codex's
/// `remote-control start` requires the managed standalone install
/// ([`codex_standalone_bin`]); Claude's host is version- and settings-gated
/// when the `claude` binary is present. `rimz doctor` reports both hard
/// refusals and skipped hosts.
pub fn preflight(config: &RemoteControlConfig) -> Result<(), PreflightError> {
    start_decision(preflight_codex(config), preflight_claude(config))
}

/// The pure start-gate decision over the two host preflights: abort on the first
/// fixable misconfiguration of an installed agent, and skip an enabled host
/// whose agent is not installed ([`PreflightError::is_uninstalled_host`]) so the
/// room still starts.
fn start_decision(
    codex: Result<(), PreflightError>,
    claude: Result<(), PreflightError>,
) -> Result<(), PreflightError> {
    for refusal in [codex, claude] {
        if let Err(err) = refusal
            && !err.is_uninstalled_host()
        {
            return Err(err);
        }
    }
    Ok(())
}

/// Check only the configured Codex remote-control daemon precondition.
/// `rimz doctor` uses this beside [`preflight_claude`] so it can report every
/// configured host failure instead of only the first one.
pub fn preflight_codex(config: &RemoteControlConfig) -> Result<(), PreflightError> {
    preflight_decision(config.codex, codex_standalone_bin().is_some())
}

/// Check only the configured Claude remote-control host preconditions. `rimz
/// doctor` uses this to report Claude readiness beside Codex readiness while
/// `preflight` keeps the single fail-fast entry point for `rimz start`.
pub fn preflight_claude(config: &RemoteControlConfig) -> Result<(), PreflightError> {
    if !config.claude {
        return Ok(());
    }
    if which::which("claude").is_err() {
        return Ok(());
    }
    let (settings_path, settings) = claude_rc::read_rc_settings();
    let version = (!settings.disable_remote_control)
        .then(claude_rc::probed_version)
        .flatten();
    claude_preflight_decision(
        config.claude,
        true,
        version,
        settings_path,
        settings,
        env_var_present("ANTHROPIC_API_KEY"),
        env_var_present("ANTHROPIC_AUTH_TOKEN"),
    )
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn claude_preflight_decision(
    claude_enabled: bool,
    claude_present: bool,
    version: Option<CliVersion>,
    settings_path: PathBuf,
    settings: claude_rc::ClaudeRcSettings,
    env_api_key: bool,
    env_auth_token: bool,
) -> Result<(), PreflightError> {
    if !claude_enabled || !claude_present {
        return Ok(());
    }
    if settings.disable_remote_control {
        return Err(PreflightError::ClaudeRemoteControlDisabled { settings_path });
    }

    let Some(found) = version else {
        tracing::warn!(
            "Claude remote-control preflight could not determine `claude --version`; applying version-independent gates only"
        );
        return Ok(());
    };
    if found < claude_rc::MIN_REMOTE_CONTROL {
        return Err(PreflightError::ClaudeTooOld { found });
    }
    if found >= claude_rc::AGENT_VIEW_HOSTS_RC_SINCE && settings.disable_agent_view {
        return Err(PreflightError::ClaudeAgentViewDisabled {
            settings_path,
            found,
        });
    }

    let mut sources = Vec::new();
    if env_api_key {
        sources.push(ClaudeAuthConflictSource::ApiKeyEnv);
    }
    if env_auth_token {
        sources.push(ClaudeAuthConflictSource::AuthTokenEnv);
    }
    if settings.api_key_helper {
        sources.push(ClaudeAuthConflictSource::ApiKeyHelperSetting);
    }
    if settings.env_auth_conflict {
        sources.push(ClaudeAuthConflictSource::SettingsEnv);
    }
    if found >= claude_rc::AUTH_ENV_BLOCKS_RC_SINCE && !sources.is_empty() {
        return Err(PreflightError::ClaudeAuthConflict { sources });
    }

    Ok(())
}

fn env_var_present(key: &str) -> bool {
    std::env::var_os(key).is_some_and(|value| !value.is_empty())
}

/// Whether a command line is one of Rimz's managed daemon hosts.
pub fn command_is_host(command: &str) -> bool {
    command.contains(COMMAND_MARKER) || command.contains(APP_SERVER_MARKER)
}

/// Whether `pane` belongs to the daemon dashboard. Command markers catch daemon
/// hosts wherever they are reported; the `rimzd` view name catches the full
/// dashboard, including content panes on backends that report only a foreground
/// binary basename.
pub fn pane_is_host(pane: &PaneRef) -> bool {
    pane.spawn_command.as_deref().is_some_and(command_is_host)
        || pane.command.as_deref().is_some_and(command_is_host)
        || pane.view_name.as_deref() == Some(VIEW_NAME)
}

#[cfg(test)]
mod tests;
