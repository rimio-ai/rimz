//! Zellij `MuxBackend` implementation.
//!
//! Interactive actions run `zellij action <verb> ...` against the session
//! inferred from the caller's `ZELLIJ_SESSION_NAME` env var. Operations that
//! may run before the user attaches, such as native sidebar launch and wakeup
//! fanout, carry the session name explicitly via `zellij --session <name>`.
//!
//! The backend covers session lifecycle, pane I/O, focus, sidebar and tab
//! layout, presence, and recovery. Backend caveats live in
//! `docs/internals/multiplexers.md` under "Zellij backend caveats".

mod backend;
mod layout;
mod pane_pid;
pub mod pane_topology;
mod parse;
mod presence;
mod raw_pane;
mod session;
mod sidebar;
pub mod socket;

#[doc(hidden)]
pub use pane_pid::ZellijPaneResolver;
pub use presence::{ensure_presence_plugin_artifact, presence_plugin_path};
pub use socket::{socket_headroom, socket_preflight};

use std::env;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{CommandSpec, MuxBackend, Result};
use crate::config::ZellijConfig;
use crate::ids::PaneId;

/// Minimum Zellij version Rimz supports overall and reports as the doctor
/// floor.
pub const MIN_ZELLIJ_VERSION: (u32, u32, u32) = (0, 44, 0);

/// Minimum Zellij version that ships the `mouse_click_through` option. Below
/// this the flag is unknown, so we omit it — a single click then focuses the
/// sidebar without reaching the renderer (degrade, never error).
const MIN_MOUSE_CLICK_THROUGH_VERSION: (u32, u32, u32) = (0, 44, 0);

/// Minimum Zellij version that ships `mouse_hover_effects`, the narrower
/// switch that suppresses hover chrome while leaving other mouse handling alone.
const MIN_MOUSE_HOVER_EFFECTS_VERSION: (u32, u32, u32) = (0, 44, 0);

/// Per-attempt bound for the pre-attach health probe.
const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// Runtime pre-attach health-probe bound. Tests may set
/// `RIMZ_TEST_ZELLIJ_HEALTH_PROBE_MS` to shorten fake-shim wait paths.
fn health_probe_timeout() -> Duration {
    let Some(value) =
        env::var_os("RIMZ_TEST_ZELLIJ_HEALTH_PROBE_MS").filter(|value| !value.is_empty())
    else {
        return HEALTH_PROBE_TIMEOUT;
    };
    value
        .to_str()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(HEALTH_PROBE_TIMEOUT)
}

/// Poll cadence while waiting for the presence plugin to publish a requested
/// topology payload.
const TOPOLOGY_CACHE_POLL_STEP: Duration = Duration::from_millis(50);

/// `query-tab-names` can hit an action-client startup race during busy session
/// ticks.
const TAB_NAMES_ATTEMPTS: u32 = 5;
const TAB_NAMES_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Zellij can accept a transient action client and still drop a `new-tab`
/// mutation under load. Confirm the tab name appears, then retry only while it
/// remains absent.
const NEW_TAB_ATTEMPTS: u32 = 3;
const NEW_TAB_CONFIRM_WINDOW: Duration = Duration::from_millis(750);
const NEW_TAB_CONFIRM_STEP: Duration = Duration::from_millis(50);
/// Zellij can publish a `new-tab --layout --name` name before its screen worker
/// has parsed the layout file and mounted panes. Keep the temp layout file
/// alive until the tab reports at least one selectable tiled pane.
const NEW_TAB_MATERIALIZE_WINDOW: Duration = Duration::from_secs(10);
const NEW_TAB_MATERIALIZE_STEP: Duration = Duration::from_millis(50);
/// A freshly opened background tab can report present before client focus has
/// accepted the return action. Confirm the attached client's pane before
/// handing control back to the caller.
const FOCUS_RESTORE_CONFIRM_WINDOW: Duration = Duration::from_secs(3);
const FOCUS_RESTORE_CONFIRM_STEP: Duration = Duration::from_millis(50);

/// Pipe name the presence-plugin launch sends its boot message down.
const PRESENCE_BOOT_PIPE: &str = "rimz_presence_boot";

/// Pipe name `rimz web open` sends to the presence plugin; keep in sync with
/// `crates/rimz-presence-zellij/src/wire.rs`.
const PRESENCE_SHARE_PIPE: &str = "rimz:share_session";

/// Pipe name that asks the presence plugin for an immediate topology cache
/// publish. Keep in sync with `crates/rimz-presence-zellij/src/wire.rs`.
const PRESENCE_TOPOLOGY_PIPE: &str = "rimz:dump_topology";

/// Pipe name that tells stale presence-plugin instances to close themselves.
/// Keep in sync with `crates/rimz-presence-zellij/src/wire.rs`.
const PRESENCE_RETIRE_PIPE: &str = "rimz:retire";

/// Deadline for the presence-plugin boot pipe.
const PRESENCE_PIPE_TIMEOUT: Duration = Duration::from_secs(2);

/// Ceiling on how long `create_session_with_sidebar` holds the temp layout file
/// on disk while waiting for Zellij to parse it.
const SIDEBAR_LAYOUT_TIMEOUT: Duration = Duration::from_secs(10);

/// Ceiling on how long an in-place sidebar add waits for its `new-pane` to
/// mount.
const MOUNT_POLL_TIMEOUT: Duration = Duration::from_secs(2);
const MOUNT_POLL_STEP: Duration = Duration::from_millis(50);

/// Bundle reported by `rimz doctor` when the active backend is Zellij.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ZellijCapabilities {
    pub binary_version: String,
    pub parsed_version: Option<(u32, u32, u32)>,
    pub meets_min_version: bool,
}

/// Probe the installed Zellij. Cheap: one `zellij --version` call.
pub fn capabilities() -> Result<ZellijCapabilities> {
    let raw = ZellijBackend::default().version()?;
    let parsed = parse_version(&raw);
    Ok(ZellijCapabilities {
        meets_min_version: parsed.is_some_and(|v| v >= MIN_ZELLIJ_VERSION),
        binary_version: raw,
        parsed_version: parsed,
    })
}

pub fn log_file() -> PathBuf {
    env::temp_dir()
        .join(format!("zellij-{}", nix::unistd::Uid::current().as_raw()))
        .join("zellij-log")
        .join("zellij.log")
}

pub fn classify_log_line(line: &str) -> Option<super::logtail::LogSeverity> {
    if line.starts_with("Panic occured") || line.starts_with("Panic occurred") {
        return Some(super::logtail::LogSeverity::Panic);
    }
    match line.split_whitespace().next() {
        Some("ERROR") => Some(super::logtail::LogSeverity::Error),
        Some("WARN") => Some(super::logtail::LogSeverity::Warn),
        _ => None,
    }
}

/// Parse `"zellij 0.41.2"` (and tolerant of leading/trailing whitespace).
/// Returns None when the shape is unexpected so `doctor` can render the raw
/// string verbatim.
pub(super) fn parse_version(raw: &str) -> Option<(u32, u32, u32)> {
    let trimmed = raw.trim();
    let after_prefix = trimmed.strip_prefix("zellij ").unwrap_or(trimmed);
    let mut parts = after_prefix
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .next()?
        .split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

/// `options` flags that forward a single click through the sidebar pane to the
/// renderer, gated on `parsed >= MIN_MOUSE_CLICK_THROUGH_VERSION`.
fn mouse_click_through_args(enabled: bool, parsed: Option<(u32, u32, u32)>) -> Vec<String> {
    if enabled {
        versioned_bool_arg(
            "--mouse-click-through",
            true,
            parsed,
            MIN_MOUSE_CLICK_THROUGH_VERSION,
        )
    } else {
        Vec::new()
    }
}

fn versioned_bool_arg(
    flag: &str,
    value: bool,
    parsed: Option<(u32, u32, u32)>,
    min_version: (u32, u32, u32),
) -> Vec<String> {
    if parsed.is_some_and(|v| v >= min_version) {
        vec![flag.to_owned(), bool_value(value)]
    } else {
        Vec::new()
    }
}

fn bool_value(value: bool) -> String {
    if value { "true" } else { "false" }.to_owned()
}

/// Zellij `options` flags Rimz owns for its rooms.
fn zellij_options_args(
    config: &ZellijConfig,
    parsed_version: Option<(u32, u32, u32)>,
) -> Vec<String> {
    let mut args = vec![
        "--default-mode".to_owned(),
        "locked".to_owned(),
        "--focus-follows-mouse".to_owned(),
        bool_value(config.focus_follows_mouse),
        "--session-serialization".to_owned(),
        bool_value(config.session_serialization),
        "--disable-session-metadata".to_owned(),
        bool_value(config.disable_session_metadata),
        "--auto-layout".to_owned(),
        bool_value(false),
    ];
    args.extend(["--stacked-resize".to_owned(), bool_value(true)]);
    args.extend(mouse_click_through_args(
        config.mouse_click_through,
        parsed_version,
    ));
    if let Some(value) = config.pane_frames {
        args.extend(["--pane-frames".to_owned(), bool_value(value)]);
    }
    if let Some(value) = config.mouse_mode {
        args.extend(["--mouse-mode".to_owned(), bool_value(value)]);
    }
    if let Some(value) = config.advanced_mouse_actions {
        args.extend(["--advanced-mouse-actions".to_owned(), bool_value(value)]);
    }
    if let Some(value) = config.mouse_hover_effects {
        args.extend(versioned_bool_arg(
            "--mouse-hover-effects",
            value,
            parsed_version,
            MIN_MOUSE_HOVER_EFFECTS_VERSION,
        ));
    }
    if let Some(value) = config.on_force_close {
        args.extend(["--on-force-close".to_owned(), value.as_str().to_owned()]);
    }
    if let Some(value) = config.scroll_buffer_size {
        args.extend(["--scroll-buffer-size".to_owned(), value.to_string()]);
    }
    if let Some(value) = config.show_startup_tips {
        args.extend(["--show-startup-tips".to_owned(), bool_value(value)]);
    }
    if let Some(value) = config.show_release_notes {
        args.extend(["--show-release-notes".to_owned(), bool_value(value)]);
    }
    if let Some(value) = config.copy_clipboard {
        args.extend(["--copy-clipboard".to_owned(), value.as_str().to_owned()]);
    }
    if let Some(value) = config.copy_on_select {
        args.extend(["--copy-on-select".to_owned(), bool_value(value)]);
    }
    if let Some(value) = config.support_kitty_keyboard_protocol {
        args.extend([
            "--support-kitty-keyboard-protocol".to_owned(),
            bool_value(value),
        ]);
    }
    if let Some(value) = config.osc8_hyperlinks {
        args.extend(["--osc8-hyperlinks".to_owned(), bool_value(value)]);
    }
    args
}

#[derive(Debug, Default)]
pub struct ZellijBackend {
    /// Test-only root for Zellij's socket, state, config, cache, home, and log
    /// env pins. Production inherits the process environment.
    runtime_dir: Option<PathBuf>,
    /// Test-scoped cache root paired with `runtime_dir`; production uses the
    /// process XDG cache root.
    cache_root: Option<PathBuf>,
    /// Memoized `zellij --version` stdout ([`MuxBackend::version`]).
    version: std::sync::OnceLock<String>,
    /// Test-only command override that avoids process-global env mutation.
    #[cfg(test)]
    program: Option<PathBuf>,
    /// Test-only presence-plugin path override that avoids process-global env
    /// mutation while exercising topology dump pipes.
    #[cfg(test)]
    presence_plugin_path: Option<PathBuf>,
}

impl ZellijBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pin every Zellij command this backend runs to `dir` as the full XDG,
    /// HOME, and TMPDIR surface, so a test's server, sessions, sockets,
    /// permission grants, cache, and logs never touch the user's.
    pub fn with_runtime_dir(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        Self {
            runtime_dir: Some(dir.clone()),
            cache_root: Some(dir),
            ..Self::default()
        }
    }

    #[cfg(test)]
    fn with_program_for_test(program: impl Into<PathBuf>) -> Self {
        Self {
            program: Some(program.into()),
            ..Self::default()
        }
    }

    #[cfg(test)]
    fn with_program_and_runtime_for_test(
        program: impl Into<PathBuf>,
        runtime_dir: impl Into<PathBuf>,
    ) -> Self {
        let runtime_dir = runtime_dir.into();
        Self {
            runtime_dir: Some(runtime_dir.clone()),
            cache_root: Some(runtime_dir),
            program: Some(program.into()),
            ..Self::default()
        }
    }

    #[cfg(test)]
    fn with_presence_plugin_for_test(mut self, path: impl Into<PathBuf>) -> Self {
        self.presence_plugin_path = Some(path.into());
        self
    }

    pub(super) fn presence_plugin_path(&self) -> Option<PathBuf> {
        #[cfg(test)]
        if let Some(path) = &self.presence_plugin_path {
            return Some(path.clone());
        }
        presence_plugin_path()
    }

    /// Base `CommandSpec` for every Zellij invocation — the single chokepoint.
    pub(super) fn cmd(&self) -> CommandSpec {
        #[cfg(test)]
        let program = self
            .program
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .or_else(|| env::var("RIMZ_ZELLIJ_BIN").ok())
            .unwrap_or_else(|| "zellij".to_owned());
        #[cfg(not(test))]
        let program = env::var("RIMZ_ZELLIJ_BIN").unwrap_or_else(|_| "zellij".to_owned());
        let mut spec = CommandSpec::new(program);
        if let Some(dir) = &self.runtime_dir {
            let dir = dir.to_string_lossy().into_owned();
            spec = spec
                .env("XDG_RUNTIME_DIR", dir.clone())
                .env("XDG_STATE_HOME", dir.clone())
                .env("XDG_CONFIG_HOME", dir.clone())
                .env("XDG_CACHE_HOME", dir.clone())
                .env("HOME", dir.clone())
                .env("TMPDIR", dir);
        }
        spec
    }

    /// Probe the installed Zellij and resolve the session `options` flags for it.
    pub(super) fn zellij_options_args_probed(&self, config: &ZellijConfig) -> Vec<String> {
        let parsed = self.version().ok().as_deref().and_then(parse_version);
        zellij_options_args(config, parsed)
    }

    /// `zellij --session <name> action <verb> …`.
    pub(super) fn zellij_action(&self, session: &str) -> CommandSpec {
        self.cmd().args([
            "--session".to_owned(),
            session.to_owned(),
            "action".to_owned(),
        ])
    }

    pub(super) fn focus_terminal(&self, session: &str, raw_id: u64) -> Result<()> {
        self.zellij_action(session)
            .args(["focus-pane-id".to_owned(), format!("terminal_{raw_id}")])
            .run()
            .map(|_| ())
    }

    pub(super) fn go_to_tab(&self, session: &str, index: u32) -> Result<()> {
        self.zellij_action(session)
            .args(["go-to-tab".to_owned(), index.to_string()])
            .run()
            .map(|_| ())
    }

    pub(super) fn go_to_tab_position(&self, session: &str, tab_position: u64) -> Result<()> {
        let index = u32::try_from(tab_position.saturating_add(1)).unwrap_or(u32::MAX);
        self.go_to_tab(session, index)
    }

    pub(super) fn close_pane(&self, session: &str, pane: &PaneId) -> Result<()> {
        self.zellij_action(session)
            .args([
                "close-pane".to_owned(),
                "--pane-id".to_owned(),
                pane.raw().to_owned(),
            ])
            .run()
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests;
