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
mod reap;
mod session;
mod sidebar;
pub mod socket;

#[doc(hidden)]
pub use pane_pid::ZellijPaneResolver;
pub(crate) use presence::{
    PresencePluginCleanup, presence_plugin_config_hash, presence_plugin_configuration,
};
pub use presence::{ensure_presence_plugin_artifact, presence_plugin_build, presence_plugin_path};
pub use reap::{ReapOutcome, reap_lineage_clients};
pub use socket::{socket_headroom, socket_preflight};

use std::env;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{CommandSpec, MuxBackend, MuxErr, Result};
use crate::config::ZellijConfig;
use crate::ids::PaneId;

/// Minimum Zellij version RimZ supports overall and reports as the doctor
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
pub(crate) const TOPOLOGY_CACHE_POLL_STEP: Duration = Duration::from_millis(50);

/// Maximum reload wait for a newly loaded presence-plugin generation to prove
/// it is publishing before stale instances are retired.
const PRESENCE_RETIRE_PROOF_TIMEOUT: Duration = Duration::from_secs(5);

/// `list-tabs` can hit an action-client startup race during busy session ticks.
const LIST_TABS_ATTEMPTS: u32 = 5;
const LIST_TABS_RETRY_DELAY: Duration = Duration::from_millis(50);

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

/// List the RimZ presence-plugin pane ids loaded in a live Zellij session.
pub fn live_presence_plugin_ids(session_name: &str) -> Result<Vec<u32>> {
    ZellijBackend::new().live_presence_plugin_ids(session_name)
}

pub fn classify_log_line(line: &str) -> Option<super::logtail::LogSeverity> {
    match parse_log_line(line) {
        super::logtail::RecordLine::Start(start) => start.severity,
        super::logtail::RecordLine::Continuation => None,
    }
}

pub fn parse_log_line(line: &str) -> super::logtail::RecordLine {
    use super::logtail::{LogRecordStart, LogSeverity, RecordLine};

    if line.starts_with("Panic occured") || line.starts_with("Panic occurred") {
        return RecordLine::Start(LogRecordStart {
            severity: Some(LogSeverity::Panic),
            message: line.to_owned(),
            ..LogRecordStart::default()
        });
    }
    let Some((severity_name, rest)) = ["TRACE", "DEBUG", "INFO", "WARN", "ERROR"]
        .into_iter()
        .find_map(|severity| {
            line.strip_prefix(severity)
                .filter(|rest| rest.chars().next().is_none_or(char::is_whitespace))
                .map(|rest| (severity, rest))
        })
    else {
        return RecordLine::Continuation;
    };
    let mut severity = match severity_name {
        "WARN" => Some(LogSeverity::Warn),
        "ERROR" => Some(LogSeverity::Error),
        _ => None,
    };
    if let Some(header) = parse_zellij_structured_header(rest) {
        if header.message.starts_with("Panic occured")
            || header.message.starts_with("Panic occurred")
        {
            severity = Some(LogSeverity::Panic);
        }
        return RecordLine::Start(LogRecordStart {
            severity,
            at: parse_log_timestamp(&header.timestamp),
            target: Some(header.target),
            thread: Some(header.thread),
            source: Some(header.source),
            message: header.message,
        });
    }
    let message = rest.trim_start().to_owned();
    RecordLine::Start(LogRecordStart {
        severity,
        message,
        ..LogRecordStart::default()
    })
}

/// Zellij stamps each record with local wall-clock time and no offset
/// (`2026-07-19 13:37:49.089`), so the machine's own zone resolves it.
fn parse_log_timestamp(raw: &str) -> Option<jiff::Timestamp> {
    raw.replace(' ', "T")
        .parse::<jiff::civil::DateTime>()
        .ok()?
        .to_zoned(jiff::tz::TimeZone::system())
        .ok()
        .map(|zoned| zoned.timestamp())
}

struct ZellijLogHeader {
    target: String,
    timestamp: String,
    thread: String,
    source: String,
    message: String,
}

fn parse_zellij_structured_header(rest: &str) -> Option<ZellijLogHeader> {
    let rest = rest.trim_start().strip_prefix('|')?;
    let (target, rest) = rest.split_once('|')?;
    let (timestamp, rest) = rest.trim_start().split_once(" [")?;
    let (thread, rest) = rest.split_once(']')?;
    let (source, message) = rest.trim_start().split_once(": ")?;
    let target = target.trim();
    let timestamp = timestamp.trim();
    let thread = thread.trim();
    let source = source.trim();
    if target.is_empty() || timestamp.is_empty() || thread.is_empty() || source.is_empty() {
        return None;
    }
    Some(ZellijLogHeader {
        target: target.to_owned(),
        timestamp: timestamp.to_owned(),
        thread: thread.to_owned(),
        source: source.to_owned(),
        message: message.trim_end().to_owned(),
    })
}

/// The wrapper zellij prints above every recoverable failure; it names nothing
/// on its own, so the `Caused by:` chain underneath is the real subject.
const NON_FATAL_HEADER: &str = "a non-fatal error occured";

pub fn diagnose_log_record(
    previous: Option<&super::logtail::LogicalRecord>,
    record: &super::logtail::LogicalRecord,
    next: Option<&super::logtail::LogicalRecord>,
) -> Option<super::logtail::LogDiagnosis> {
    use super::logtail::{LogDiagnosis, LogImpact, LogSeverity, LogState, normalized_issue_key};

    let severity = record.start.severity?;
    // A disconnect writes two records; the second rides with the first.
    if previous.is_some_and(is_unknown_client_message) && is_client_send_failure(record) {
        return None;
    }
    let paired_send_failure =
        next.filter(|next| is_unknown_client_message(record) && is_client_send_failure(next));

    let target = record.start.target.as_deref().unwrap_or_default();
    let message = record.start.message.trim();
    let causes = record_causes(&record.text);
    let subject = match (message.starts_with(NON_FATAL_HEADER), causes.first()) {
        (true, Some(cause)) => cause.as_str(),
        _ => message,
    };

    if let Some(expected) = expected_lifecycle(record, subject, paired_send_failure) {
        return Some(expected);
    }

    // The sidebar reads panes through these plugin calls, so a timeout here is
    // the log's own account of pane discovery falling behind.
    if subject.contains("timed out") && subject.contains("for plugin") {
        return Some(LogDiagnosis {
            key: "plugin_pane_query_timeout".to_owned(),
            state: LogState::Investigate,
            impact: LogImpact::Warn,
            summary: "plugin pane queries timed out — pane discovery lags behind the room"
                .to_owned(),
            sample: None,
        });
    }
    // An unknown client message with no disconnect behind it, and the logout
    // zellij escalates to, are the same event stream: a client speaking a
    // protocol this server does not know.
    if is_unknown_client_message(record)
        || (message.starts_with("Client sent over") && message.contains("unknown messages"))
    {
        return Some(LogDiagnosis {
            key: "client_protocol_mismatch".to_owned(),
            state: LogState::Investigate,
            impact: LogImpact::Warn,
            summary: "a client sent messages zellij could not read — usually a client/server version mismatch"
                .to_owned(),
            sample: None,
        });
    }

    // Zellij keeps the pane and spawns it in the inherited directory, so the
    // pane lives and only its directory is wrong. Keying on the path keeps two
    // different stale directories in two groups, each naming its own fix.
    if let Some(cwd) = missing_pane_cwd(subject) {
        return Some(LogDiagnosis {
            key: normalized_issue_key(&format!("missing_pane_cwd:{cwd}")),
            state: LogState::Investigate,
            impact: LogImpact::Warn,
            summary: format!(
                "a pane's configured directory is missing ({cwd}) — zellij started it in the inherited directory"
            ),
            sample: None,
        });
    }

    let impact = match severity {
        LogSeverity::Warn => LogImpact::Warn,
        LogSeverity::Error | LogSeverity::Panic => LogImpact::Alarm,
    };
    // Naming the whole cause chain keeps unrelated failures in separate groups;
    // keyed on the wrapper alone they collapse into one meaningless bucket.
    let summary = if causes.is_empty() || !message.starts_with(NON_FATAL_HEADER) {
        message.to_owned()
    } else {
        causes.join(": ")
    };
    Some(LogDiagnosis {
        key: normalized_issue_key(&format!("{target}:{summary}")),
        state: LogState::Investigate,
        impact,
        summary,
        sample: None,
    })
}

/// Log traffic the room provokes by living its normal life: clients attaching
/// and leaving, panes closing, a busy server acknowledging late. Each one reads
/// as an ERROR in zellij's log and means nothing to the operator.
fn expected_lifecycle(
    record: &super::logtail::LogicalRecord,
    subject: &str,
    paired_send_failure: Option<&super::logtail::LogicalRecord>,
) -> Option<super::logtail::LogDiagnosis> {
    use super::logtail::{LogDiagnosis, LogImpact, LogState};

    let expected = |key: &str, summary: &str, sample: Option<String>| LogDiagnosis {
        key: key.to_owned(),
        state: LogState::Expected,
        impact: LogImpact::Info,
        summary: summary.to_owned(),
        sample,
    };

    // Only the proven pair reads as a departure: an unknown client message on
    // its own is evidence of something else, and gets to keep saying so.
    if paired_send_failure.is_some() || is_client_send_failure(record) {
        return Some(expected(
            "client_disconnect",
            "a client left the session",
            paired_send_failure.map(|next| format!("{}\n{}", record.text, next.text)),
        ));
    }
    if let Some(action) = action_ack_timeout(subject) {
        return Some(expected(
            &format!("action_ack_timeout:{action}"),
            &format!("zellij acknowledged {action} late (the action still ran)"),
            None,
        ));
    }
    // Zellij truncates the target column, so the untruncated source path is the
    // reliable way to place a record in the server's pty reader.
    let source = record.start.source.as_deref().unwrap_or_default();
    if source.contains("terminal_bytes.rs") && subject.contains("I/O error (os error 5)") {
        return Some(expected(
            "closed_pane_pty",
            "read from a closed pane's terminal",
            None,
        ));
    }
    if subject.starts_with("failed to disable mouse mode") {
        return Some(expected(
            "client_teardown_mouse_mode",
            "a client tore down mouse mode on a terminal already gone",
            None,
        ));
    }
    // Pane-targeting actions name a pane the room listed a moment earlier, so a
    // pane that closes inside that window resolves to nothing. The id varies per
    // occurrence and one key groups them, because the race is the single fact.
    if subject.starts_with("Pane with id") && subject.ends_with("not found") {
        return Some(expected(
            "closed_pane_action",
            "addressed a pane that had already closed",
            None,
        ));
    }
    let lower = record.text.to_ascii_lowercase();
    if lower.contains("closed terminal") && lower.contains("resize") && lower.contains("caused by")
    {
        return Some(expected(
            "closed_terminal_resize",
            "resized a pane whose terminal had closed",
            None,
        ));
    }
    None
}

/// The directory a pane asked for and zellij could not enter, from
/// `Failed to set CWD for new pane. '<path>' does not exist or is not a folder`.
/// Matching the whole wording keeps a reworded upstream message falling through
/// to the generic path rather than reporting a truncated directory.
fn missing_pane_cwd(subject: &str) -> Option<&str> {
    subject
        .strip_prefix("Failed to set CWD for new pane. '")?
        .strip_suffix("' does not exist or is not a folder")
}

/// The action zellij took too long to acknowledge, from
/// `Action CliPipe did not complete within 1s timeout`.
fn action_ack_timeout(subject: &str) -> Option<&str> {
    subject
        .strip_prefix("Action ")?
        .split_once(" did not complete within")
        .map(|(action, _)| action)
}

fn is_unknown_client_message(record: &super::logtail::LogicalRecord) -> bool {
    record.start.message == "Received unknown message from client."
}

fn is_client_send_failure(record: &super::logtail::LogicalRecord) -> bool {
    record.start.message.starts_with(NON_FATAL_HEADER)
        && record.text.contains("failed to send message to client")
        && record.text.contains("Broken pipe (os error 32)")
}

/// The `Caused by:` chain under an error record, outermost cause first. Anyhow
/// numbers the entries once there is more than one; a lone cause is bare.
fn record_causes(text: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| line.trim() != "Caused by:")
        .skip(1)
        .map(str::trim)
        .take_while(|line| !line.is_empty())
        .map(|line| strip_cause_index(line).trim().to_owned())
        .collect()
}

/// Drop anyhow's `0: ` ordinal, keeping the cause text itself.
fn strip_cause_index(line: &str) -> &str {
    line.split_once(' ')
        .filter(|(ordinal, _)| {
            ordinal.ends_with(':')
                && ordinal
                    .trim_end_matches(':')
                    .chars()
                    .all(|ch| ch.is_ascii_digit())
        })
        .map_or(line, |(_, rest)| rest)
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

/// Zellij `options` flags RimZ owns for its rooms.
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
            .map_err(|err| MuxErr::Output {
                program: "zellij".to_owned(),
                reason: err.to_string(),
            })?
            .action_target();
        self.zellij_action(session)
            .args(["close-pane".to_owned(), "--pane-id".to_owned(), target])
            .run()
            .map(|_| ())
    }
}

#[cfg(test)]
pub(crate) mod tests;
