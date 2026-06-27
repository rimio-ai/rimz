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
use crate::ids::AgentKind;
use crate::pane::{ElevatedAgent, PaneRef};

/// View name for the managed daemon tab. Shared by the launcher (the idempotency
/// key for the tmux window / Zellij tab) and the sidebar classifier
/// ([`pane_is_host`]), so both speak the same name. The tab hosts configurable
/// content in the middle (live stats by default) and stacks the Claude
/// remote-control host and per-session Codex app-server broker on the right
/// when they apply.
pub const VIEW_NAME: &str = "rimzd";

/// Substring marking the Claude remote-control host in a pane's command line —
/// the subcommand it spells (`claude remote-control …`).
const COMMAND_MARKER: &str = "remote-control";

/// Substring marking the Codex app-server broker in a pane's command line
/// (`rimz codex app-server serve …`). The broker is a per-session host pane in
/// the same view, distinct from the per-user daemon [`ensure_codex_daemon`] runs.
const APP_SERVER_MARKER: &str = "app-server";

/// Maximum process-tree depth walked below a pane root when looking for an
/// elevated agent. `sudo su` + login shell + node launcher + agent is shallow;
/// this cap keeps a pathological pane from turning a sidebar tick into an
/// unbounded tree walk.
const ELEVATED_AGENT_DESCENT_DEPTH: usize = 8;

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

/// Whether a mux foreground command is an elevation entrypoint. The producer
/// uses this as the cheap gate before walking descendants of a pane root.
pub(crate) fn command_starts_with_elevation_wrapper(command: &str) -> bool {
    command
        .split_whitespace()
        .next()
        .map(basename)
        .is_some_and(is_elevation_wrapper)
}

/// A different-real-uid agent descendant under an elevation wrapper in this
/// pane, if one is visible through `/proc`. The marker is display-only; callers
/// must keep the pane's original command unchanged so the sidebar never binds a
/// foreign-user agent as a local ledger session.
pub fn elevated_in_pane_agent(pane_pid: u32) -> Option<ElevatedAgent> {
    elevated_in_pane_agent_with(
        pane_pid,
        crate::proc::own_uid()?,
        &|pid| crate::proc::children(pid),
        &|pid| crate::proc::cmdline(pid),
        &|pid| crate::proc::comm(pid),
        &|pid| crate::proc::real_uid(pid),
    )
}

fn elevated_in_pane_agent_with(
    pane_pid: u32,
    own_uid: u32,
    children: &dyn Fn(u32) -> Vec<u32>,
    cmdline: &dyn Fn(u32) -> Option<String>,
    comm: &dyn Fn(u32) -> Option<String>,
    real_uid: &dyn Fn(u32) -> Option<u32>,
) -> Option<ElevatedAgent> {
    let mut stack = vec![(pane_pid, 0, false)];
    let mut seen = std::collections::HashSet::new();
    while let Some((pid, depth, wrapper_seen)) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        let command = cmdline(pid).unwrap_or_default();
        let comm = comm(pid);
        let wrapper_seen = wrapper_seen || command_starts_with_elevation_wrapper(&command);
        if wrapper_seen
            && let Some(kind) =
                crate::ledger::snapshot::command_agent_kind_with_comm(&command, comm.as_deref())
            && let Some(uid) = real_uid(pid)
            && uid != own_uid
        {
            return Some(ElevatedAgent {
                kind: AgentKind::new_unchecked(kind),
                uid,
            });
        }
        if depth >= ELEVATED_AGENT_DESCENT_DEPTH {
            continue;
        }
        for child in children(pid) {
            stack.push((child, depth + 1, wrapper_seen));
        }
    }
    None
}

fn is_elevation_wrapper(program: &str) -> bool {
    matches!(program, "sudo" | "su" | "doas")
}

fn basename(token: &str) -> &str {
    Path::new(token)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(token)
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
/// working directory. This is the exact single-process case only: a cwd with no
/// match or multiple same-kind agent CLIs abstains so callers keep pane starts
/// unknown rather than duplicate one cwd-level timestamp across several panes.
pub fn in_pane_agent_start(kind: &str, pane_cwd: &str) -> Option<jiff::Timestamp> {
    let starts = in_pane_agent_starts(kind, pane_cwd);
    (starts.len() == 1).then_some(starts[0])
}

/// Start times for in-pane agent CLI processes whose `/proc` cwd equals
/// `pane_cwd`. Callers that know other panes' exact starts subtract those before
/// deciding whether one unaccounted process remains.
pub fn in_pane_agent_starts(kind: &str, pane_cwd: &str) -> Vec<jiff::Timestamp> {
    if !in_pane_agent_probe_supported(kind) {
        return Vec::new();
    }
    let pane_cwd = Path::new(pane_cwd);
    let mut starts = crate::proc::list_processes()
        .into_iter()
        .filter(|process| in_pane_agent_cmdline_matches(kind, &process.cmdline))
        .filter(|process| crate::proc::cwd(process.pid).as_deref() == Some(pane_cwd))
        .filter_map(|process| crate::proc::process_start(process.pid))
        .collect::<Vec<_>>();
    starts.sort();
    starts.dedup();
    starts
}

/// Start time of the in-pane lazy-agent CLI behind a pane's bound root process —
/// the per-pane exact signal the frame stamp prefers over the cwd scan above.
/// The root is the CLI itself when its cmdline reads as the agent TUI (a pane
/// running it directly); a shell-hosted CLI is the root's single child, since
/// the mux reports the *foreground* command while the root stays the shell. The
/// cmdline check is load-bearing twice over: a shell outlives the agents it
/// hosts, so stamping its older start would re-admit the very sessions
/// `pane_start_allows_bind` refuses, and a re-run CLI is a fresh child pid even
/// when the hosting shell survives, so re-tenancy stays visible. `None` for a
/// non-lazy kind or when neither process reads as the CLI, so the caller falls
/// back rather than guesses.
pub fn in_pane_agent_start_for_root(kind: &str, root_pid: u32) -> Option<jiff::Timestamp> {
    if !in_pane_agent_probe_supported(kind) {
        return None;
    }
    if crate::proc::cmdline(root_pid)
        .as_deref()
        .is_some_and(|cmdline| in_pane_agent_cmdline_matches(kind, cmdline))
    {
        return crate::proc::process_start(root_pid);
    }
    if let &[child] = crate::proc::children(root_pid).as_slice()
        && crate::proc::cmdline(child)
            .as_deref()
            .is_some_and(|cmdline| in_pane_agent_cmdline_matches(kind, cmdline))
    {
        return crate::proc::process_start(child);
    }
    None
}

fn in_pane_agent_cmdline_matches(kind: &str, cmdline: &str) -> bool {
    if kind == "codex" {
        return is_codex_cli_cmdline(cmdline);
    }
    crate::ledger::snapshot::command_agent_kind(cmdline) == Some(kind)
}

fn in_pane_agent_probe_supported(kind: &str) -> bool {
    crate::agents::descriptor_by_kind(kind)
        .is_some_and(|descriptor| descriptor.capabilities.registers_lazily)
}

/// Session id from the in-pane Codex CLI behind a pane's bound root process.
/// The root is the CLI itself when the pane runs it directly; a shell-hosted CLI
/// is the root's single foreground child. Multiple children abstain so a shell
/// doing other work cannot donate the wrong resumed session id.
pub fn codex_resumed_session_id_for_root(root_pid: u32) -> Option<crate::ids::AgentSessionId> {
    codex_resumed_session_id_for_root_with(root_pid, &crate::proc::cmdline, &crate::proc::children)
}

fn codex_resumed_session_id_for_root_with(
    root_pid: u32,
    cmdline: &dyn Fn(u32) -> Option<String>,
    children: &dyn Fn(u32) -> Vec<u32>,
) -> Option<crate::ids::AgentSessionId> {
    if let Some(resumed) = cmdline(root_pid)
        .as_deref()
        .and_then(codex_resumed_session_id_from_cmdline)
    {
        return Some(resumed);
    }
    if let &[child] = children(root_pid).as_slice() {
        return cmdline(child)
            .as_deref()
            .and_then(codex_resumed_session_id_from_cmdline);
    }
    None
}

/// Session id from a resumed Codex CLI command (`codex resume <session-id>`).
/// Exact rebirth binding reads this instead of guessing by cwd. The parser is
/// deliberately narrow: daemon/app-server surfaces are excluded by
/// [`is_codex_cli_cmdline`], and the session id is accepted only when it is the
/// token immediately after `resume`.
pub fn codex_resumed_session_id_from_cmdline(cmdline: &str) -> Option<crate::ids::AgentSessionId> {
    if !is_codex_cli_cmdline(cmdline) {
        return None;
    }
    let mut tokens = cmdline.split_whitespace().peekable();
    while let Some(token) = tokens.next() {
        let is_codex = Path::new(token)
            .file_name()
            .is_some_and(|file| file == "codex");
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
            .map(crate::ids::AgentSessionId::from);
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
mod tests;
