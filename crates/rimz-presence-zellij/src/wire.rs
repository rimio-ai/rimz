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

/// The pipe message name `rimz web open` sends so this plugin asks Zellij to
/// admit web clients to the current session.
pub const SHARE_SESSION_PIPE: &str = "rimz:share_session";

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

/// Runtime keybind KDL binding `chord` to a pipe that targets this plugin
/// instance by id, in normal and locked modes. Binding by `plugin_id`
/// (`MessagePluginId`) reaches the exact loaded instance; a url+configuration
/// bind would also have to match the instance's initial cwd, which a keypress
/// from another pane does not.
pub fn focus_keybind_kdl(chord: FocusChord, plugin_id: u32) -> String {
    let key = kdl_string(&focus_key_label(chord));
    let pipe = kdl_string(FOCUS_SIDEBAR_PIPE);
    let bind = format!(
        "bind {key} {{\n            MessagePluginId {plugin_id} {{\n                name {pipe}\n            }}\n        }}"
    );
    format!(
        "keybinds {{\n    normal {{\n        {bind}\n    }}\n    locked {{\n        {bind}\n    }}\n}}\n"
    )
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
    if let Some(chord) = config.focus_key.and_then(FocusChord::parse)
        && let Some(plugin_id) = config.plugin_id
    {
        kdl.push_str(&focus_keybind_kdl(chord, plugin_id));
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
    pub commands_failed: u64,
    pub stale_writer_rejections: u64,
    pub topology_failures: u64,
    pub other_failures: u64,
    pub zellij_version: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommandCounters {
    pub completed: u64,
    pub succeeded: u64,
    pub stale_writer_rejections: u64,
    pub topology_failures: u64,
    pub other_failures: u64,
}

impl CommandCounters {
    pub fn record(&mut self, exit_code: Option<i32>, published_topology: bool) {
        self.completed = self.completed.saturating_add(1);
        match exit_code {
            Some(0) => self.succeeded = self.succeeded.saturating_add(1),
            Some(STALE_WRITER_EXIT_CODE) if published_topology => {
                self.stale_writer_rejections = self.stale_writer_rejections.saturating_add(1);
            }
            _ if published_topology => {
                self.topology_failures = self.topology_failures.saturating_add(1);
            }
            _ => self.other_failures = self.other_failures.saturating_add(1),
        }
    }

    pub fn failed(self) -> u64 {
        self.completed.saturating_sub(self.succeeded)
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
