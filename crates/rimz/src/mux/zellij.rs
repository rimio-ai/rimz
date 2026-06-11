//! Zellij `MuxBackend` implementation.
//!
//! Interactive actions run `zellij action <verb> ...` against the session
//! inferred from the caller's `ZELLIJ_SESSION_NAME` env var. Operations that
//! may run before the user attaches, such as native sidebar launch and wakeup
//! fanout, carry the session name explicitly via `zellij --session <name>`.
//!
//! Caveats live in `docs/internals/sidebar/multiplexers.md` under
//! "Zellij backend caveats" — namely that raw Zellij pane IDs are
//! integers, scoped per-session, and that the spike does not yet expose
//! tab-level operations beyond what's needed to identify a pane.

mod backend;
mod layout;
mod parse;
mod presence;
mod raw_pane;
mod session;
mod sidebar;
pub mod socket;

pub use presence::presence_plugin_path;
pub use socket::{socket_headroom, socket_preflight};

use std::env;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{CommandSpec, MuxBackend, Result};
use crate::config::ZellijConfig;
use crate::ids::PaneId;

/// Minimum Zellij version that ships the pipe-broadcast semantics Rimz uses
/// as a best-effort wakeup optimization.
pub const MIN_ZELLIJ_VERSION: (u32, u32, u32) = (0, 41, 0);

/// Minimum Zellij version that ships `advanced_mouse_actions`. Below this the
/// flag is unknown, so Rimz omits it and accepts Zellij's older defaults.
const MIN_ADVANCED_MOUSE_ACTIONS_VERSION: (u32, u32, u32) = (0, 43, 0);

/// Minimum Zellij version that ships the `mouse_click_through` option. Below
/// this the flag is unknown, so we omit it — a single click then focuses the
/// sidebar without reaching the renderer (degrade, never error).
const MIN_MOUSE_CLICK_THROUGH_VERSION: (u32, u32, u32) = (0, 44, 0);

/// Minimum Zellij version that ships `mouse_hover_effects`, the narrower
/// switch that suppresses hover chrome while leaving other mouse handling alone.
const MIN_MOUSE_HOVER_EFFECTS_VERSION: (u32, u32, u32) = (0, 44, 0);

/// Pane name the sidebar layout assigns, and the title Zellij reports back for
/// it. The sole source of truth for both rendering the layout and detecting
/// whether a live session still carries its sidebar.
pub const SIDEBAR_PANE_NAME: &str = "rimz-sidebar";

/// Zellij's action client occasionally answers `list-panes` with an empty
/// stdout and a success status when the session server is mid-tick — a known
/// race that a short retry clears.
const LIST_PANES_ATTEMPTS: u32 = 3;
const LIST_PANES_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Per-attempt bound for the pre-attach health probe.
const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// `query-tab-names` can hit the same action-client startup race as
/// `list-panes`.
const TAB_NAMES_ATTEMPTS: u32 = 5;
const TAB_NAMES_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Minimum Zellij that loads the presence plugin.
pub const PRESENCE_PLUGIN_MIN_ZELLIJ: (u32, u32, u32) = (0, 44, 0);

/// Pipe name the presence-plugin launch sends its boot message down.
const PRESENCE_BOOT_PIPE: &str = "rimz_presence_boot";

/// Deadline for the presence-plugin boot pipe.
const PRESENCE_PIPE_TIMEOUT: Duration = Duration::from_secs(2);

/// Ceiling on how long `create_session_with_sidebar` holds the temp layout file
/// on disk while waiting for Zellij to parse it.
const SIDEBAR_LAYOUT_TIMEOUT: Duration = Duration::from_secs(10);

/// Ceiling on how long an in-place sidebar add waits for its `new-pane` to
/// mount.
const MOUNT_POLL_TIMEOUT: Duration = Duration::from_secs(2);
const MOUNT_POLL_STEP: Duration = Duration::from_millis(50);

/// An under-cap sidebar wider than this share of its tab's columns is resized
/// back toward the layout width.
const SIDEBAR_RESIZE_TRIGGER_PERCENT: u64 = 45;

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
    if enabled && parsed.is_some_and(|v| v >= MIN_MOUSE_CLICK_THROUGH_VERSION) {
        vec!["--mouse-click-through".to_owned(), "true".to_owned()]
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
        "--pane-frames".to_owned(),
        bool_value(config.pane_frames),
        "--on-force-close".to_owned(),
        config.on_force_close.as_str().to_owned(),
        "--scroll-buffer-size".to_owned(),
        config.scroll_buffer_size.to_string(),
        "--show-startup-tips".to_owned(),
        bool_value(config.show_startup_tips),
        "--show-release-notes".to_owned(),
        bool_value(config.show_release_notes),
        "--copy-clipboard".to_owned(),
        config.copy_clipboard.as_str().to_owned(),
        "--copy-on-select".to_owned(),
        bool_value(config.copy_on_select),
        "--support-kitty-keyboard-protocol".to_owned(),
        bool_value(config.support_kitty_keyboard_protocol),
        "--osc8-hyperlinks".to_owned(),
        bool_value(config.osc8_hyperlinks),
        "--auto-layout".to_owned(),
        bool_value(config.auto_layout),
        "--session-serialization".to_owned(),
        bool_value(config.session_serialization),
    ];
    if !config.mouse_mode {
        args.extend(["--mouse-mode".to_owned(), "false".to_owned()]);
    }
    args.extend(mouse_click_through_args(
        config.mouse_click_through,
        parsed_version,
    ));
    args.extend(versioned_bool_arg(
        "--advanced-mouse-actions",
        config.advanced_mouse_actions,
        parsed_version,
        MIN_ADVANCED_MOUSE_ACTIONS_VERSION,
    ));
    args.extend(versioned_bool_arg(
        "--mouse-hover-effects",
        config.mouse_hover_effects,
        parsed_version,
        MIN_MOUSE_HOVER_EFFECTS_VERSION,
    ));
    args
}

#[derive(Debug, Default)]
pub struct ZellijBackend {
    /// Override for `XDG_RUNTIME_DIR`, where Zellij locates its server socket.
    runtime_dir: Option<PathBuf>,
    /// Memoized `zellij --version` stdout ([`MuxBackend::version`]).
    version: std::sync::OnceLock<String>,
}

impl ZellijBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pin every Zellij command this backend runs to `dir` as `XDG_RUNTIME_DIR`,
    /// so a test's server, sessions, and sockets never touch the user's.
    pub fn with_runtime_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            runtime_dir: Some(dir.into()),
            ..Self::default()
        }
    }

    /// Base `CommandSpec` for every Zellij invocation — the single chokepoint.
    pub(super) fn cmd(&self) -> CommandSpec {
        let program = env::var("RIMZ_ZELLIJ_BIN").unwrap_or_else(|_| "zellij".to_owned());
        let mut spec = CommandSpec::new(program);
        if let Some(dir) = &self.runtime_dir {
            spec = spec.env("XDG_RUNTIME_DIR", dir.to_string_lossy().into_owned());
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
