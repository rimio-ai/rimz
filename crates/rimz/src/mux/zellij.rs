//! Zellij `MuxBackend` implementation.
//!
//! Every action runs as `zellij --session <name> action <verb> ...`, built by
//! `ZellijBackend::zellij_action`. A caller that holds no session name gets
//! the one its own pane sits in (`ZELLIJ_SESSION_NAME`), and outside a pane
//! the action fails with `MuxErr::NoSessionToAddress` before Zellij is
//! spawned: an unnamed action among several live sessions exits 0 and does
//! nothing.
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
mod reap;
mod session;
mod sidebar;
pub(in crate::mux) mod socket;
mod tab_owner;

#[doc(hidden)]
pub use pane_pid::ZellijPaneResolver;
pub(crate) use presence::PresenceUpgrade;
pub use presence::{ensure_presence_plugin_artifact, presence_plugin_build, presence_plugin_path};
pub(crate) use raw_pane::WidthMemo;
#[cfg(test)]
pub(crate) use raw_pane::take_width_derivations;
pub use reap::{ReapOutcome, reap_lineage_clients};
pub use socket::{ZellijSocketHeadroom, socket_headroom, socket_preflight};

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::command::RefusalRetry;
use super::{CommandSpec, MuxBackend, MuxErr, Result};
use crate::config::ZellijConfig;
use crate::ids::PaneId;

fn output_error(reason: impl std::fmt::Display) -> MuxErr {
    MuxErr::Output {
        program: "zellij".to_owned(),
        reason: reason.to_string(),
    }
}

/// Minimum Zellij version RimZ supports overall and reports as the doctor
/// floor. Focus jumps (`action focus-pane-id`) and tab-targeted sidebar adds
/// (`new-pane --tab-id`) first ship in 0.44.1, and the presence plugin's only
/// foreground-command source, the `CommandChanged` event, first ships in 0.44.2.
pub const MIN_ZELLIJ_VERSION: (u32, u32, u32) = (0, 44, 2);

/// Zellij 0.45's non-focusing pane spawn preserves a background target
/// without moving any attached client.
const MIN_NO_FOCUS_ZELLIJ_VERSION: (u32, u32, u32) = (0, 45, 0);

/// Total budget for the pre-attach responsiveness verdict.
const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// Bound decimal-byte argv expansion for each paste write.
const ZELLIJ_WRITE_CHUNK: usize = 8 * 1024;

/// Per-call bound for reload's best-effort convergence pane/layout reads.
pub(crate) const RECONCILE_LIST_TIMEOUT: Duration = Duration::from_secs(5);

/// Pause between failed native responsiveness probes.
const HEALTH_PROBE_RETRY_DELAY: Duration = Duration::from_millis(250);

/// Runtime pre-attach responsiveness budget. Tests may set
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

/// Maximum reload wait for a newly loaded presence-plugin generation to prove
/// it is publishing before stale instances are retired.
const PRESENCE_RETIRE_PROOF_TIMEOUT: Duration = Duration::from_secs(5);

/// A tab or pane listing can hit an action-client startup race during busy
/// session ticks and answer a successful empty stdout.
const TRANSIENT_EMPTY_ATTEMPTS: u32 = 5;
const TRANSIENT_EMPTY_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Reruns of a command Zellij's client refused before dispatch while the
/// session's socket is still on disk (`socket::refused_live_session`). The
/// busy window that causes the refusal passes in tens of milliseconds.
const PREDISPATCH_REFUSAL_RERUNS: u32 = 5;
const PREDISPATCH_REFUSAL_DELAY: Duration = Duration::from_millis(100);

/// Zellij has no per-pane env flag or layout property. An empty map leaves the command unchanged.
fn env_prefixed(env: &BTreeMap<String, String>, command: Vec<String>) -> Vec<String> {
    if env.is_empty() {
        return command;
    }
    let mut wrapped = Vec::with_capacity(command.len() + env.len() + 1);
    wrapped.push("env".to_owned());
    wrapped.extend(env.iter().map(|(key, value)| format!("{key}={value}")));
    wrapped.extend(command);
    wrapped
}

fn pane_short_name(argv: &[String]) -> Option<String> {
    Path::new(argv.first()?)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
}

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
/// A freshly opened tab can report materialized before its screen worker
/// accepts the return action. Retry command acceptance without treating the
/// unchanged client sample as an acknowledgement.
const FOCUS_RESTORE_ATTEMPTS: u32 = 5;
const FOCUS_RESTORE_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Pipe name the presence-plugin launch sends its boot message down.
const PRESENCE_BOOT_PIPE: &str = "rimz_presence_boot";

/// Pipe name that asks the presence plugin for an immediate topology cache
/// publish. Keep in sync with `crates/rimz-presence-zellij/src/wire.rs`.
const PRESENCE_TOPOLOGY_PIPE: &str = "rimz:dump_topology";

/// Pipe name carrying a host-selected pane id to the presence plugin's
/// mechanical fullscreen toggle. Keep in sync with
/// `crates/rimz-presence-zellij/src/wire.rs`.
const PRESENCE_TOGGLE_FULLSCREEN_PIPE: &str = "rimz:toggle_fullscreen";

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

/// List the RimZ presence-plugin pane ids loaded in a live Zellij session.
pub fn live_presence_plugin_ids(session_name: &str) -> Result<Vec<u32>> {
    ZellijBackend::default().live_presence_plugin_ids(session_name)
}

/// Parse `"zellij 0.41.2"` (and tolerant of leading/trailing whitespace).
/// Returns None when the shape is unexpected so `doctor` can render the raw
/// string verbatim.
fn parse_version(raw: &str) -> Option<(u32, u32, u32)> {
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

/// One absolute session-scoped Zellij option. Opaque outside the crate: callers
/// resolve a list through [`zellij_session_options`] and hand it back whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZellijSessionOption {
    key: &'static str,
    value: ZellijOptionValue,
}

/// Values supported by RimZ's resolved session options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ZellijOptionValue {
    Bool(bool),
    Int(u32),
    Word(&'static str),
}

impl ZellijOptionValue {
    fn kdl(self) -> String {
        match self {
            // Every word is a bare ASCII enum spelling (`quit`, `primary`), so
            // Rust's own string quoting is already the KDL spelling.
            Self::Word(value) => format!("{value:?}"),
            _ => self.plugin_configuration(),
        }
    }

    fn plugin_configuration(self) -> String {
        match self {
            Self::Bool(value) => value.to_string(),
            Self::Int(value) => value.to_string(),
            Self::Word(value) => value.to_owned(),
        }
    }
}

/// Resolve the option list shared by birth layouts and live reconfiguration.
///
/// The order is fixed, and is the order the birth layout tail and the plugin
/// identity hash carry. The reconfigure payload does not keep it: the plugin
/// reads its load configuration from a `BTreeMap`, so the keys reach Zellij in
/// key order there. Nothing depends on it, since the keys are distinct and the
/// merge is per key.
pub fn zellij_session_options(config: &ZellijConfig) -> Vec<ZellijSessionOption> {
    use ZellijOptionValue::{Bool, Int, Word};

    // mouse_mode is read in each client's process, so no session channel can set it.
    let mut options = vec![
        ZellijSessionOption {
            key: "auto_layout",
            value: Bool(false),
        },
        ZellijSessionOption {
            key: "stacked_resize",
            value: Bool(true),
        },
        ZellijSessionOption {
            key: "stacked_pane_list",
            value: Bool(false),
        },
        ZellijSessionOption {
            key: "mouse_click_through",
            value: Bool(config.mouse_click_through),
        },
        ZellijSessionOption {
            key: "focus_follows_mouse",
            value: Bool(config.focus_follows_mouse),
        },
        ZellijSessionOption {
            key: "session_serialization",
            value: Bool(config.session_serialization),
        },
        ZellijSessionOption {
            key: "disable_session_metadata",
            value: Bool(config.disable_session_metadata),
        },
    ];
    if let Some(value) = config.advanced_mouse_actions {
        options.push(ZellijSessionOption {
            key: "advanced_mouse_actions",
            value: Bool(value),
        });
    }
    if let Some(value) = config.mouse_hover_effects {
        options.push(ZellijSessionOption {
            key: "mouse_hover_effects",
            value: Bool(value),
        });
    }
    if let Some(value) = config.pane_frames {
        options.push(ZellijSessionOption {
            key: "pane_frames",
            value: Bool(value),
        });
    }
    if let Some(value) = config.on_force_close {
        options.push(ZellijSessionOption {
            key: "on_force_close",
            value: Word(value.as_str()),
        });
    }
    if let Some(value) = config.scroll_buffer_size {
        options.push(ZellijSessionOption {
            key: "scroll_buffer_size",
            value: Int(value),
        });
    }
    if let Some(value) = config.show_startup_tips {
        options.push(ZellijSessionOption {
            key: "show_startup_tips",
            value: Bool(value),
        });
    }
    if let Some(value) = config.show_release_notes {
        options.push(ZellijSessionOption {
            key: "show_release_notes",
            value: Bool(value),
        });
    }
    if let Some(value) = config.copy_clipboard {
        options.push(ZellijSessionOption {
            key: "copy_clipboard",
            value: Word(value.as_str()),
        });
    }
    if let Some(value) = config.copy_on_select {
        options.push(ZellijSessionOption {
            key: "copy_on_select",
            value: Bool(value),
        });
    }
    if let Some(value) = config.support_kitty_keyboard_protocol {
        options.push(ZellijSessionOption {
            key: "support_kitty_keyboard_protocol",
            value: Bool(value),
        });
    }
    if let Some(value) = config.osc8_hyperlinks {
        options.push(ZellijSessionOption {
            key: "osc8_hyperlinks",
            value: Bool(value),
        });
    }
    options
}

/// Options consumed by the attaching client, not the session.
fn zellij_client_options_args(config: &ZellijConfig) -> Vec<String> {
    let mut args = vec!["--default-mode".to_owned(), "locked".to_owned()];
    // Zellij 0.45.1 merges twice: plain merge in Setup::from_cli_args, then
    // Options::merge_from_cli in start_client. Any explicit XOR boolean
    // collapses to false, so false is expressible by flag and true is not.
    if config.mouse_mode == Some(false) {
        args.extend(["--mouse-mode".to_owned(), "false".to_owned()]);
    }
    if let Some(value) = config.support_kitty_keyboard_protocol {
        args.extend([
            "--support-kitty-keyboard-protocol".to_owned(),
            value.to_string(),
        ]);
    }
    args
}

#[derive(Debug, Default)]
pub struct ZellijBackend {
    /// Test-only root for Zellij's socket, state, config, cache, home, and log
    /// env pins. Production inherits the process environment.
    runtime_dir: Option<PathBuf>,
    /// Memoized `zellij --version` stdout ([`MuxBackend::version`]).
    version: std::sync::OnceLock<String>,
    /// Test-only command override that avoids process-global env mutation.
    #[cfg(test)]
    program: Option<PathBuf>,
    /// Test-only presence-plugin path override that avoids process-global env
    /// mutation while exercising topology dump pipes.
    #[cfg(test)]
    presence_plugin_path: Option<PathBuf>,
    /// Test-only responsiveness budget override.
    #[cfg(test)]
    health_probe_timeout: Option<Duration>,
    /// Test-only stand-in for the caller's `ZELLIJ_SESSION_NAME`, which a
    /// test never reads from the process environment.
    #[cfg(test)]
    ambient_session: Option<String>,
}

impl ZellijBackend {
    fn health_probe_timeout(&self) -> Duration {
        #[cfg(test)]
        if let Some(timeout) = self.health_probe_timeout {
            return timeout;
        }
        health_probe_timeout()
    }

    #[cfg(test)]
    fn with_health_probe_timeout_for_test(mut self, timeout: Duration) -> Self {
        self.health_probe_timeout = Some(timeout);
        self
    }

    /// Pin every Zellij command this backend runs to `dir` as the full XDG,
    /// HOME, and TMPDIR surface, so a test's server, sessions, sockets,
    /// permission grants, cache, and logs never touch the user's.
    #[cfg(any(test, feature = "testkit"))]
    pub fn with_runtime_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            runtime_dir: Some(dir.into()),
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
        Self {
            runtime_dir: Some(runtime_dir.into()),
            program: Some(program.into()),
            ..Self::default()
        }
    }

    #[cfg(test)]
    fn with_ambient_session_for_test(mut self, session: &str) -> Self {
        self.ambient_session = Some(session.to_owned());
        self
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

    /// Base `CommandSpec` for every Zellij invocation — the single chokepoint,
    /// with the user's `TMPDIR` restored and the pre-dispatch refusal rerun.
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
        let mut spec = CommandSpec::new(program)
            .restore_user_tmpdir(
                env::var(crate::child_process::USER_TMPDIR_ENV)
                    .ok()
                    .as_deref(),
                env::var(crate::child_process::TEMP_ROOT_KEYS_ENV)
                    .ok()
                    .as_deref(),
            )
            .retry_refusal(RefusalRetry {
                is_refusal: socket::refused_live_session,
                reruns: PREDISPATCH_REFUSAL_RERUNS,
                delay: PREDISPATCH_REFUSAL_DELAY,
            });
        if let Some(dir) = &self.runtime_dir {
            let dir = dir.to_string_lossy().into_owned();
            spec = spec
                .env("XDG_RUNTIME_DIR", dir.clone())
                .env("RIMZ_HOME", dir.clone())
                .env("XDG_STATE_HOME", dir.clone())
                .env("XDG_CONFIG_HOME", dir.clone())
                .env("XDG_CACHE_HOME", dir.clone())
                .env("HOME", dir.clone())
                .env("TMPDIR", dir);
        }
        spec
    }

    /// `zellij --session <name> action <verb> …`.
    pub(super) fn zellij_action(&self, session: &str) -> CommandSpec {
        self.cmd().args(["--session", session, "action"])
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

    /// Move client focus to the leading tab, when there is a client to move.
    ///
    /// Zellij resolves `go-to-tab` against a client's active tab, so the action
    /// needs an attached terminal client to land on. A session with none has no
    /// focus to place: zellij answers the request by logging `active tab not
    /// found` as a server ERROR, once per call. Probing first keeps that noise
    /// out of the log a reader is scanning for real faults, and the tab this
    /// call would have chosen is the one a fresh attach opens on anyway.
    ///
    /// The probe reads attachment the way the sidebar add path does, from
    /// clients focused on a terminal pane. A client parked on a zellij plugin
    /// UI therefore reads detached and keeps the focus it chose, which costs a
    /// courtesy the user is not watching for.
    pub(super) fn go_to_lead_tab(&self, session: &str) -> Result<()> {
        if self.focused_terminal_client_ids(session).is_empty() {
            return Ok(());
        }
        self.go_to_tab(session, 1)
    }

    pub(super) fn close_pane(&self, session: &str, pane: &PaneId) -> Result<()> {
        let target = pane_topology::ZellijPaneId::try_from(pane)
            .map_err(output_error)?
            .action_target();
        self.zellij_action(session)
            .args(["close-pane".to_owned(), "--pane-id".to_owned(), target])
            .run()
            .map(|_| ())
    }
}

#[cfg(test)]
pub(crate) mod tests;
