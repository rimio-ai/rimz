//! Host-boundary wire shapes for the Zellij presence plugin.
//!
//! This module owns every argv and KDL payload the wasm shell sends to the
//! host. It stays pure and host-tested so the shell only projects Zellij
//! events, gathers runtime telemetry, and executes these outputs.

use crate::policy::{self, PaneFields};

/// The pipe message name the focus-sidebar keybind sends to this plugin. The
/// chord (rimz-injected or a documented `config.kdl` bind) pipes this name, and
/// the plugin runs `rimz sidebar focus --toggle` — reaching the sidebar from any
/// pane, since a Zellij keybind cannot focus a pane by id on its own.
pub const FOCUS_SIDEBAR_PIPE: &str = "rimz:focus_sidebar";

/// Pipe message name for the sidebar-aware smart-zoom keybind.
pub const ZOOM_PANE_PIPE: &str = "rimz:zoom_pane";

/// Host-to-plugin pipe carrying the already-selected pane id for a mechanical
/// fullscreen toggle.
pub const TOGGLE_FULLSCREEN_PIPE: &str = "rimz:toggle_fullscreen";

/// Pipe message name the host backend sends when it needs a topology cache
/// newer than a local mutation. The plugin publishes one immediate `alive` wake
/// carrying the current topology payload; the host writes the cache and stamp
/// without broadcasting a sidebar event.
pub const DUMP_TOPOLOGY_PIPE: &str = "rimz:dump_topology";

/// Pipe message name the host broadcasts after proving the newest topology
/// writer. Older plugin instances retire by generation.
pub const RETIRE_PIPE: &str = "rimz:retire";

/// Private exit status from `rimz sidebar wake`: this plugin's topology writer
/// generation lost the durable CAS. Repeated rejections retire the instance.
pub const STALE_WRITER_EXIT_CODE: i32 = 73;

/// Run-command context key marking a topology-publishing wake. Zellij returns
/// this context with `RunCommandResult`, so unrelated command results cannot
/// reset the consecutive stale-writer rejection counter.
pub const TOPOLOGY_PUBLISH_CONTEXT: &str = "rimz_topology_publish";

/// Maximum bytes carried in one topology argv value. Linux limits each argv
/// entry independently, so topology travels as repeated bounded arguments.
pub const TOPOLOGY_ARG_CHUNK_BYTES: usize = 64 * 1024;

/// Topologies beyond this ceiling skip publication while the wake's stamp and
/// telemetry still reach the host.
pub const TOPOLOGY_MAX_BYTES: usize = 1024 * 1024;

pub fn publishes_topology(argv: &[String]) -> bool {
    argv.iter().any(|arg| arg == "--topology")
}

pub fn retire_generation(payload: Option<&str>) -> Option<policy::TopologyWriter> {
    serde_json::from_str(payload?).ok()
}

pub fn fullscreen_pane(payload: Option<&str>) -> Option<policy::ClientPaneId> {
    let raw = payload?.trim();
    if let Some(id) = raw.strip_prefix("terminal_") {
        return id.parse().ok().map(policy::ClientPaneId::Terminal);
    }
    raw.strip_prefix("plugin_")?
        .parse()
        .ok()
        .map(policy::ClientPaneId::Plugin)
}

/// The modifier half of a focus-key chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChordModifier {
    Alt,
    Ctrl,
}

/// A focus-key chord such as `Alt+p`, parsed from the rimz-injected
/// `focus_key` load configuration so the wasm shell can bind it. The grammar
/// matches the host's tmux binding (`rimz::mux::FocusChord`); it lives here too
/// because the plugin cannot depend on the rimz crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusChord {
    pub modifier: ChordModifier,
    pub key: char,
}

impl FocusChord {
    /// Parse a `Mod+key` (or `Mod-key`) chord. The modifier is case-insensitive
    /// (`alt`/`meta`/`m` or `ctrl`/`control`/`c`); the key is one printable
    /// ASCII character. Any other shape returns `None` so the caller skips the
    /// bind rather than register a broken one.
    pub fn parse(raw: &str) -> Option<Self> {
        let (modifier, key) = raw.trim().split_once(['+', '-'])?;
        let modifier = match modifier.trim().to_ascii_lowercase().as_str() {
            "alt" | "meta" | "m" => ChordModifier::Alt,
            "ctrl" | "control" | "c" => ChordModifier::Ctrl,
            _ => return None,
        };
        let mut chars = key.trim().chars();
        let key = chars.next()?;
        if chars.next().is_some() || !key.is_ascii_graphic() {
            return None;
        }
        Some(Self { modifier, key })
    }
}

/// Runtime keybind KDL targeting this plugin instance by id in normal and
/// locked modes. One fragment carries every configured room key so one
/// `Reconfigure` installs them atomically.
pub fn room_keybinds_kdl(
    focus: Option<FocusChord>,
    zoom: Option<FocusChord>,
    plugin_id: u32,
) -> Option<String> {
    let bindings = [(focus, FOCUS_SIDEBAR_PIPE), (zoom, ZOOM_PANE_PIPE)]
        .into_iter()
        .filter_map(|(chord, pipe)| chord.map(|chord| (chord, pipe)))
        .collect::<Vec<_>>();
    if bindings.is_empty() {
        return None;
    }
    let mut kdl = String::from("keybinds {\n");
    for mode in ["normal", "locked"] {
        kdl.push_str(&format!("    {mode} {{\n"));
        for (chord, pipe) in &bindings {
            let key = kdl_string(&focus_key_label(*chord));
            let pipe = kdl_string(pipe);
            kdl.push_str(&format!(
                "        bind {key} {{\n            MessagePluginId {plugin_id} {{\n                name {pipe}\n            }}\n        }}\n"
            ));
        }
        kdl.push_str("    }\n");
    }
    kdl.push_str("}\n");
    Some(kdl)
}

fn focus_key_label(chord: FocusChord) -> String {
    let modifier = match chord.modifier {
        ChordModifier::Alt => "Alt",
        ChordModifier::Ctrl => "Ctrl",
    };
    format!("{modifier} {}", chord.key)
}

fn kdl_string(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            _ => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

#[derive(Default)]
pub struct RuntimeReconfigure<'a> {
    pub plugin_id: Option<u32>,
    pub focus_key: Option<&'a str>,
    pub zoom_key: Option<&'a str>,
    pub focus_follows_mouse: Option<bool>,
    pub mouse_click_through: Option<bool>,
}

pub fn parse_configuration_bool(value: Option<&str>) -> Option<bool> {
    match value {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    }
}

pub fn runtime_reconfigure_kdl(config: &RuntimeReconfigure<'_>) -> Option<String> {
    let mut kdl = String::new();
    if let Some(value) = config.focus_follows_mouse {
        push_bool_option_kdl(&mut kdl, "focus_follows_mouse", value);
    }
    if let Some(value) = config.mouse_click_through {
        push_bool_option_kdl(&mut kdl, "mouse_click_through", value);
    }
    if let Some(plugin_id) = config.plugin_id
        && let Some(keybinds) = room_keybinds_kdl(
            config.focus_key.and_then(FocusChord::parse),
            config.zoom_key.and_then(FocusChord::parse),
            plugin_id,
        )
    {
        kdl.push_str(&keybinds);
    }
    (!kdl.is_empty()).then_some(kdl)
}

fn push_bool_option_kdl(kdl: &mut String, key: &str, value: bool) {
    kdl.push_str(key);
    kdl.push(' ');
    kdl.push_str(bool_kdl(value));
    kdl.push('\n');
}

fn bool_kdl(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PluginTelemetry {
    pub plugin_id: Option<u32>,
    pub plugin_build: Option<String>,
    pub loaded_at_ms: u64,
    pub mem_pages: u64,
    pub uptime_ms: u64,
    pub commands_completed: u64,
    pub commands_succeeded: u64,
    pub stale_writer_rejections: u64,
    pub topology_failures: u64,
    pub other_failures: u64,
    pub zellij_version: String,
    /// Why the most recent failing wake failed. Counters say how often the
    /// host refused; this says what it said while refusing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<CommandFailure>,
}

/// The evidence Zellij hands back with a failed `run_command`: what the host
/// exited with, the first thing it wrote to stderr on the way out, and when it
/// happened. The stamp lets a reader place the cause against the window the
/// counters describe, so an old cause stays recognizable as old.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct CommandFailure {
    pub exit_code: Option<i32>,
    pub detail: String,
    pub at_ms: u64,
}

/// Longest stderr excerpt carried back to the host. One line of `anyhow`
/// context names the cause; the rest is argv the host already knows.
const FAILURE_DETAIL_MAX_BYTES: usize = 200;

impl CommandFailure {
    pub fn new(exit_code: Option<i32>, stderr: &[u8], at_ms: u64) -> Self {
        Self {
            exit_code,
            detail: first_line(&String::from_utf8_lossy(stderr)),
            at_ms,
        }
    }
}

/// The first non-empty stderr line, bounded on a char boundary.
fn first_line(stderr: &str) -> String {
    let Some(line) = stderr.lines().map(str::trim).find(|line| !line.is_empty()) else {
        return String::new();
    };
    let mut end = line.len().min(FAILURE_DETAIL_MAX_BYTES);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end].to_owned()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommandCounters {
    pub completed: u64,
    pub succeeded: u64,
    pub stale_writer_rejections: u64,
    pub topology_failures: u64,
    pub other_failures: u64,
}

/// Which bucket a finished command landed in, so the caller knows whether the
/// failure is worth keeping evidence for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOutcome {
    Succeeded,
    StaleWriter,
    TopologyFailure,
    OtherFailure,
}

/// Fold one finished command into the retained failure evidence. A real failure
/// replaces it, and a stale-writer rejection leaves it alone — that exit is the
/// fence doing its job, and reporting it as the cause would bury the failure the
/// reader is actually chasing.
///
/// A success also leaves it alone, so the evidence outlives the recovery. Wakes
/// run far more often than telemetry is sampled, so clearing here dropped the
/// cause of an intermittent failure before any sample could carry it: the host
/// counted the failure in its window and had nothing to say about it. Retaining
/// the record keeps the plugin shipping observations — the last failure and its
/// time — and leaves the host to judge whether that cause still explains the
/// window it is describing.
pub fn fold_failure(
    previous: Option<CommandFailure>,
    outcome: CommandOutcome,
    exit_code: Option<i32>,
    stderr: &[u8],
    now_ms: u64,
) -> Option<CommandFailure> {
    match outcome {
        CommandOutcome::Succeeded | CommandOutcome::StaleWriter => previous,
        CommandOutcome::TopologyFailure | CommandOutcome::OtherFailure => {
            Some(CommandFailure::new(exit_code, stderr, now_ms))
        }
    }
}

impl CommandCounters {
    pub fn record(&mut self, exit_code: Option<i32>, published_topology: bool) -> CommandOutcome {
        self.completed = self.completed.saturating_add(1);
        match exit_code {
            Some(0) => {
                self.succeeded = self.succeeded.saturating_add(1);
                CommandOutcome::Succeeded
            }
            Some(STALE_WRITER_EXIT_CODE) if published_topology => {
                self.stale_writer_rejections = self.stale_writer_rejections.saturating_add(1);
                CommandOutcome::StaleWriter
            }
            _ if published_topology => {
                self.topology_failures = self.topology_failures.saturating_add(1);
                CommandOutcome::TopologyFailure
            }
            _ => {
                self.other_failures = self.other_failures.saturating_add(1);
                CommandOutcome::OtherFailure
            }
        }
    }
}

pub struct WakeContext<'a> {
    pub rimz_bin: Option<&'a str>,
    pub workspace_id: Option<&'a str>,
    pub session_name: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeRequest {
    Changed,
    Alive(PluginTelemetry),
    SwitchSettled {
        tab: u64,
        generation: u64,
        clients: Vec<policy::ClientViewEntry>,
    },
}

/// Build the `rimz sidebar wake` argv for a presence poke. `None` means a
/// session-scoped request cannot be expressed with the available context, so
/// the caller drops it.
pub fn wake_argv(
    ctx: &WakeContext<'_>,
    request: WakeRequest,
    topology_json: Option<&str>,
) -> Option<Vec<String>> {
    let reason = match &request {
        WakeRequest::Changed => "panes-changed",
        WakeRequest::Alive(_) => "alive",
        WakeRequest::SwitchSettled { .. } => "switch-settled",
    };
    let mut argv = vec![
        ctx.rimz_bin.unwrap_or("rimz").to_owned(),
        "sidebar".to_owned(),
        "wake".to_owned(),
        "--reason".to_owned(),
        reason.to_owned(),
    ];
    match request {
        WakeRequest::Changed => {
            push_session(ctx, &mut argv)?;
            push_workspace(ctx, &mut argv);
        }
        WakeRequest::Alive(telemetry) => {
            push_workspace(ctx, &mut argv);
            argv.push("--plugin-telemetry".to_owned());
            argv.push(serde_json::to_string(&telemetry).ok()?);
            if let Some(session_name) = ctx.session_name {
                argv.push("--session-name".to_owned());
                argv.push(session_name.to_owned());
            }
        }
        WakeRequest::SwitchSettled {
            tab,
            generation,
            clients,
        } => {
            push_session(ctx, &mut argv)?;
            argv.push("--active-tab".to_owned());
            argv.push(tab.to_string());
            argv.push("--focus-generation".to_owned());
            argv.push(generation.to_string());
            argv.push("--focus-clients".to_owned());
            argv.push(serde_json::to_string(&clients).ok()?);
            push_workspace(ctx, &mut argv);
        }
    }
    if let Some(topology) = topology_json.filter(|topology| topology.len() <= TOPOLOGY_MAX_BYTES) {
        let mut start = 0;
        while start < topology.len() {
            let mut end = (start + TOPOLOGY_ARG_CHUNK_BYTES).min(topology.len());
            while !topology.is_char_boundary(end) {
                end -= 1;
            }
            argv.push("--topology".to_owned());
            argv.push(topology[start..end].to_owned());
            start = end;
        }
    }
    Some(argv)
}

fn push_workspace(ctx: &WakeContext<'_>, argv: &mut Vec<String>) {
    if let Some(workspace_id) = ctx.workspace_id {
        argv.push("--workspace-id".to_owned());
        argv.push(workspace_id.to_owned());
    }
}

fn push_session(ctx: &WakeContext<'_>, argv: &mut Vec<String>) -> Option<()> {
    let session_name = ctx.session_name?;
    argv.push("--session-name".to_owned());
    argv.push(session_name.to_owned());
    Some(())
}

pub fn focus_sidebar_argv(ctx: &WakeContext<'_>) -> Vec<String> {
    let mut argv = vec![
        ctx.rimz_bin.unwrap_or("rimz").to_owned(),
        "sidebar".to_owned(),
        "focus".to_owned(),
        "--toggle".to_owned(),
    ];
    if let Some(session_name) = ctx.session_name {
        argv.push("--session-name".to_owned());
        argv.push(session_name.to_owned());
    }
    argv.push("--mux".to_owned());
    argv.push("zellij".to_owned());
    argv
}

pub fn zoom_pane_argv(ctx: &WakeContext<'_>) -> Vec<String> {
    let mut argv = vec![
        ctx.rimz_bin.unwrap_or("rimz").to_owned(),
        "pane".to_owned(),
        "zoom".to_owned(),
    ];
    if let Some(session_name) = ctx.session_name {
        argv.push("--session-name".to_owned());
        argv.push(session_name.to_owned());
    }
    argv.push("--mux".to_owned());
    argv.push("zellij".to_owned());
    argv
}

pub fn topology_json(
    session_name: Option<&str>,
    produced_at_ms: u64,
    writer: Option<policy::TopologyWriter>,
    clients: Option<&policy::ClientSample>,
    panes: &[PaneFields],
) -> Option<String> {
    let payload = policy::published_topology_payload(
        session_name?,
        produced_at_ms,
        writer,
        clients.cloned(),
        panes,
    )?;
    serde_json::to_string(&payload).ok()
}

#[cfg(test)]
mod tests;
