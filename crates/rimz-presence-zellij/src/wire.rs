//! Host-boundary wire shapes for the Zellij presence plugin.
//!
//! This module owns every argv and KDL payload the wasm shell sends to the
//! host. It stays pure and host-tested so the shell only projects Zellij
//! events, gathers runtime telemetry, and executes these outputs.

use std::collections::BTreeMap;

use crate::policy::{self, FocusPatch, PaneFields};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginTelemetry {
    pub mem_pages: u64,
    pub uptime_ms: u64,
    pub commands_completed: u64,
    pub commands_failed: u64,
    pub zellij_version: String,
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
    PaneOpened {
        pane_id: u32,
        command: Option<String>,
    },
    PaneClosed {
        pane_id: u32,
    },
    FocusStranded {
        pane_id: u32,
    },
    CommandChanged {
        pane_id: u32,
        args: Vec<String>,
    },
    FocusChanged {
        patch: Vec<FocusPatch>,
    },
}

/// Build the `rimz sidebar wake` argv for a presence poke. `None` means the
/// request cannot be expressed with the available context and the caller should
/// fall back to an identity-free change signal.
pub fn wake_argv(
    ctx: &WakeContext<'_>,
    request: WakeRequest,
    topology_json: Option<&str>,
) -> Option<Vec<String>> {
    let reason = match &request {
        WakeRequest::Changed => "panes-changed",
        WakeRequest::Alive(_) => "alive",
        WakeRequest::PaneOpened { .. } => "pane-opened",
        WakeRequest::PaneClosed { .. } => "pane-closed",
        WakeRequest::FocusStranded { .. } => "focus-stranded",
        WakeRequest::CommandChanged { .. } => "command-changed",
        WakeRequest::FocusChanged { .. } => "focus-changed",
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
            push_workspace(ctx, &mut argv);
        }
        WakeRequest::Alive(telemetry) => {
            push_workspace(ctx, &mut argv);
            argv.push("--plugin-mem-pages".to_owned());
            argv.push(telemetry.mem_pages.to_string());
            argv.push("--plugin-uptime-ms".to_owned());
            argv.push(telemetry.uptime_ms.to_string());
            argv.push("--plugin-commands".to_owned());
            argv.push(telemetry.commands_completed.to_string());
            argv.push("--plugin-commands-failed".to_owned());
            argv.push(telemetry.commands_failed.to_string());
            argv.push("--plugin-zellij-version".to_owned());
            argv.push(telemetry.zellij_version);
            if let Some(session_name) = ctx.session_name {
                argv.push("--session-name".to_owned());
                argv.push(session_name.to_owned());
            }
        }
        WakeRequest::PaneOpened { pane_id, command } => {
            push_session(ctx, &mut argv)?;
            push_pane_id(&mut argv, pane_id);
            push_workspace(ctx, &mut argv);
            if let Some(command) = command.filter(|command| !command.is_empty()) {
                argv.push("--command-arg".to_owned());
                argv.push(command);
            }
        }
        WakeRequest::PaneClosed { pane_id } => {
            push_session(ctx, &mut argv)?;
            push_pane_id(&mut argv, pane_id);
            push_workspace(ctx, &mut argv);
        }
        WakeRequest::FocusStranded { pane_id } => {
            push_session(ctx, &mut argv)?;
            push_pane_id(&mut argv, pane_id);
            push_workspace(ctx, &mut argv);
        }
        WakeRequest::CommandChanged { pane_id, args } => {
            push_session(ctx, &mut argv)?;
            push_pane_id(&mut argv, pane_id);
            push_workspace(ctx, &mut argv);
            let mut pushed = false;
            for arg in args.into_iter().filter(|arg| !arg.is_empty()) {
                argv.push("--command-arg".to_owned());
                argv.push(arg);
                pushed = true;
            }
            if !pushed {
                return None;
            }
        }
        WakeRequest::FocusChanged { patch } => {
            if patch.is_empty() {
                return None;
            }
            push_session(ctx, &mut argv)?;
            push_workspace(ctx, &mut argv);
            for pane in patch {
                argv.push(
                    if pane.is_focused {
                        "--focused-pane-id"
                    } else {
                        "--unfocused-pane-id"
                    }
                    .to_owned(),
                );
                argv.push(format!("terminal_{}", pane.id));
            }
        }
    }
    if let Some(topology) = topology_json {
        argv.push("--topology".to_owned());
        argv.push(topology.to_owned());
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

fn push_pane_id(argv: &mut Vec<String>, pane_id: u32) {
    argv.push("--pane-id".to_owned());
    argv.push(format!("terminal_{pane_id}"));
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
    focused_pane: Option<u32>,
    clients: Option<&policy::ClientSample>,
    tabs: &BTreeMap<usize, Vec<PaneFields>>,
) -> Option<String> {
    let payload = policy::published_topology_payload(
        session_name?,
        produced_at_ms,
        writer,
        focused_pane,
        clients.cloned(),
        tabs,
    )?;
    serde_json::to_string(&payload).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ctx() -> WakeContext<'static> {
        WakeContext {
            rimz_bin: Some("/bin/rimz"),
            workspace_id: Some("workspace-1"),
            session_name: Some("session-1"),
        }
    }
    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }
    fn pane(id: u32) -> PaneFields {
        PaneFields {
            id,
            is_plugin: false,
            is_focused: false,
            is_suppressed: false,
            is_floating: false,
            exited: false,
            is_held: false,
            tab_position: 0,
            tab_name: Some("main".to_owned()),
            pane_x: Some(0),
            pane_columns: Some(80),
            title: format!("pane-{id}"),
            pane_command: None,
            pane_cwd: None,
            terminal_command: Some("zsh".to_owned()),
        }
    }

    #[test]
    fn topology_json_carries_focused_pane() {
        let tabs = BTreeMap::from([(0, vec![pane(7)])]);
        let json = topology_json(
            Some("session-1"),
            42,
            Some(policy::TopologyWriter {
                plugin_id: 9,
                loaded_at_ms: 1_000,
            }),
            Some(7),
            None,
            &tabs,
        )
        .expect("topology serializes");
        let payload: serde_json::Value = serde_json::from_str(&json).expect("topology is JSON");

        assert_eq!(payload["focused_pane"], 7);
        assert_eq!(payload["writer"]["plugin_id"], 9);
        assert_eq!(payload["writer"]["loaded_at_ms"], 1000);
    }

    #[test]
    fn topology_json_carries_baseline_cwd() {
        let mut implicit = pane(7);
        implicit.terminal_command = None;
        let mut tabs = BTreeMap::from([(0, vec![implicit])]);
        let baseline = BTreeMap::from([(
            7,
            policy::PaneBaseline {
                command: "zsh".to_owned(),
                cwd: Some("/repo/main".to_owned()),
            },
        )]);
        policy::apply_foreground_commands(&mut tabs, &BTreeMap::new(), &baseline);
        let json = topology_json(Some("session-1"), 42, None, Some(7), None, &tabs)
            .expect("topology serializes");
        let payload: serde_json::Value = serde_json::from_str(&json).expect("topology is JSON");

        assert_eq!(payload["panes"][0]["pane_command"], "zsh");
        assert_eq!(payload["panes"][0]["pane_cwd"], "/repo/main");
    }

    #[test]
    fn topology_json_carries_clients_when_sampled() {
        let tabs = BTreeMap::from([(0, vec![pane(7)])]);
        let clients = policy::ClientSample {
            human_clients: 2,
            viewed_panes: vec![7, 9],
        };
        let json = topology_json(Some("session-1"), 42, None, Some(7), Some(&clients), &tabs)
            .expect("topology serializes");
        let payload: serde_json::Value = serde_json::from_str(&json).expect("topology is JSON");

        assert_eq!(payload["clients"]["human_clients"], 2);
        assert_eq!(
            payload["clients"]["viewed_panes"],
            serde_json::json!([7, 9])
        );

        let json = topology_json(Some("session-1"), 42, None, Some(7), None, &tabs)
            .expect("topology serializes");
        let payload: serde_json::Value = serde_json::from_str(&json).expect("topology is JSON");
        assert!(payload.get("clients").is_none());
    }

    #[test]
    fn changed_wake_argv_carries_workspace_and_topology() {
        assert_eq!(
            wake_argv(&ctx(), WakeRequest::Changed, Some("{\"topology\":true}")),
            Some(strings(&[
                "/bin/rimz",
                "sidebar",
                "wake",
                "--reason",
                "panes-changed",
                "--workspace-id",
                "workspace-1",
                "--topology",
                "{\"topology\":true}",
            ])),
        );
    }

    #[test]
    fn alive_wake_argv_carries_telemetry_before_session_name() {
        assert_eq!(
            wake_argv(
                &ctx(),
                WakeRequest::Alive(PluginTelemetry {
                    mem_pages: 12,
                    uptime_ms: 34,
                    commands_completed: 56,
                    commands_failed: 7,
                    zellij_version: "0.44.3".to_owned(),
                }),
                None,
            ),
            Some(strings(&[
                "/bin/rimz",
                "sidebar",
                "wake",
                "--reason",
                "alive",
                "--workspace-id",
                "workspace-1",
                "--plugin-mem-pages",
                "12",
                "--plugin-uptime-ms",
                "34",
                "--plugin-commands",
                "56",
                "--plugin-commands-failed",
                "7",
                "--plugin-zellij-version",
                "0.44.3",
                "--session-name",
                "session-1",
            ])),
        );
    }

    #[test]
    fn pane_opened_wake_argv_carries_optional_command() {
        assert_eq!(
            wake_argv(
                &ctx(),
                WakeRequest::PaneOpened {
                    pane_id: 7,
                    command: Some("codex".to_owned()),
                },
                None,
            ),
            Some(strings(&[
                "/bin/rimz",
                "sidebar",
                "wake",
                "--reason",
                "pane-opened",
                "--session-name",
                "session-1",
                "--pane-id",
                "terminal_7",
                "--workspace-id",
                "workspace-1",
                "--command-arg",
                "codex",
            ])),
        );
    }

    #[test]
    fn pane_closed_wake_argv_names_terminal_pane() {
        assert_eq!(
            wake_argv(&ctx(), WakeRequest::PaneClosed { pane_id: 8 }, None),
            Some(strings(&[
                "/bin/rimz",
                "sidebar",
                "wake",
                "--reason",
                "pane-closed",
                "--session-name",
                "session-1",
                "--pane-id",
                "terminal_8",
                "--workspace-id",
                "workspace-1",
            ])),
        );
    }

    #[test]
    fn focus_stranded_wake_argv_names_terminal_pane() {
        assert_eq!(
            wake_argv(&ctx(), WakeRequest::FocusStranded { pane_id: 9 }, None),
            Some(strings(&[
                "/bin/rimz",
                "sidebar",
                "wake",
                "--reason",
                "focus-stranded",
                "--session-name",
                "session-1",
                "--pane-id",
                "terminal_9",
                "--workspace-id",
                "workspace-1",
            ])),
        );
    }

    #[test]
    fn command_changed_wake_argv_drops_empty_args() {
        assert_eq!(
            wake_argv(
                &ctx(),
                WakeRequest::CommandChanged {
                    pane_id: 10,
                    args: strings(&["codex", "", "--model", "gpt"]),
                },
                None,
            ),
            Some(strings(&[
                "/bin/rimz",
                "sidebar",
                "wake",
                "--reason",
                "command-changed",
                "--session-name",
                "session-1",
                "--pane-id",
                "terminal_10",
                "--workspace-id",
                "workspace-1",
                "--command-arg",
                "codex",
                "--command-arg",
                "--model",
                "--command-arg",
                "gpt",
            ])),
        );
    }

    #[test]
    fn focus_changed_wake_argv_names_patch_entries() {
        assert_eq!(
            wake_argv(
                &ctx(),
                WakeRequest::FocusChanged {
                    patch: vec![
                        FocusPatch {
                            id: 1,
                            is_focused: false,
                        },
                        FocusPatch {
                            id: 2,
                            is_focused: true,
                        },
                    ],
                },
                None,
            ),
            Some(strings(&[
                "/bin/rimz",
                "sidebar",
                "wake",
                "--reason",
                "focus-changed",
                "--session-name",
                "session-1",
                "--workspace-id",
                "workspace-1",
                "--unfocused-pane-id",
                "terminal_1",
                "--focused-pane-id",
                "terminal_2",
            ])),
        );
    }

    #[test]
    fn wake_argv_none_gates_session_bound_and_empty_requests() {
        let no_session = WakeContext {
            rimz_bin: None,
            workspace_id: None,
            session_name: None,
        };
        assert_eq!(
            wake_argv(
                &no_session,
                WakeRequest::PaneOpened {
                    pane_id: 1,
                    command: None,
                },
                None,
            ),
            None,
        );
        assert_eq!(
            wake_argv(
                &ctx(),
                WakeRequest::CommandChanged {
                    pane_id: 1,
                    args: strings(&["", ""]),
                },
                None,
            ),
            None,
        );
        assert_eq!(
            wake_argv(
                &ctx(),
                WakeRequest::FocusChanged { patch: Vec::new() },
                None,
            ),
            None,
        );
    }

    #[test]
    fn focus_sidebar_argv_uses_zellij_mux_and_optional_session() {
        assert_eq!(
            focus_sidebar_argv(&ctx()),
            strings(&[
                "/bin/rimz",
                "sidebar",
                "focus",
                "--toggle",
                "--session-name",
                "session-1",
                "--mux",
                "zellij",
            ]),
        );
        assert_eq!(
            focus_sidebar_argv(&WakeContext {
                rimz_bin: None,
                workspace_id: None,
                session_name: None,
            }),
            strings(&["rimz", "sidebar", "focus", "--toggle", "--mux", "zellij"]),
        );
    }

    #[test]
    fn parses_boolean_load_configuration() {
        assert_eq!(parse_configuration_bool(Some("true")), Some(true));
        assert_eq!(parse_configuration_bool(Some("false")), Some(false));
        assert_eq!(parse_configuration_bool(Some("TRUE")), None);
        assert_eq!(parse_configuration_bool(None), None);
    }

    #[test]
    fn runtime_reconfigure_kdl_emits_mouse_options_without_focus_key() {
        let kdl = runtime_reconfigure_kdl(&RuntimeReconfigure {
            focus_follows_mouse: Some(false),
            mouse_click_through: Some(true),
            ..RuntimeReconfigure::default()
        })
        .expect("mouse options produce a reconfigure payload");

        assert_eq!(kdl, "focus_follows_mouse false\nmouse_click_through true\n");
    }

    #[test]
    fn runtime_reconfigure_kdl_combines_mouse_options_and_focus_keybind() {
        let kdl = runtime_reconfigure_kdl(&RuntimeReconfigure {
            plugin_id: Some(42),
            focus_key: Some("Alt+p"),
            focus_follows_mouse: Some(false),
            mouse_click_through: Some(true),
        })
        .expect("focus key and mouse options produce a reconfigure payload");

        assert!(
            kdl.starts_with("focus_follows_mouse false\nmouse_click_through true\nkeybinds {\n")
        );
        assert!(kdl.contains("bind \"Alt p\""));
        assert!(kdl.contains("name \"rimz:focus_sidebar\""));
        assert_eq!(kdl.matches("MessagePluginId 42").count(), 2);
        assert!(!kdl.contains("MessagePlugin \""));
        assert!(!kdl.contains("plugin_url"));
    }

    #[test]
    fn runtime_reconfigure_kdl_skips_empty_payload() {
        assert!(runtime_reconfigure_kdl(&RuntimeReconfigure::default()).is_none());
        assert!(
            runtime_reconfigure_kdl(&RuntimeReconfigure {
                plugin_id: Some(42),
                focus_key: Some("off"),
                ..RuntimeReconfigure::default()
            })
            .is_none()
        );
        assert!(
            runtime_reconfigure_kdl(&RuntimeReconfigure {
                focus_key: Some("Alt+p"),
                ..RuntimeReconfigure::default()
            })
            .is_none()
        );
    }

    #[test]
    fn focus_chord_parses_alt_and_ctrl_with_separators() {
        assert_eq!(
            FocusChord::parse("Alt+p"),
            Some(FocusChord {
                modifier: ChordModifier::Alt,
                key: 'p',
            }),
        );
        assert_eq!(
            FocusChord::parse("ctrl-S"),
            Some(FocusChord {
                modifier: ChordModifier::Ctrl,
                key: 'S',
            }),
        );
        assert_eq!(
            FocusChord::parse("  m+j  "),
            Some(FocusChord {
                modifier: ChordModifier::Alt,
                key: 'j',
            }),
        );
    }

    #[test]
    fn focus_chord_rejects_malformed_shapes() {
        assert_eq!(FocusChord::parse("p"), None);
        assert_eq!(FocusChord::parse("super+p"), None);
        assert_eq!(FocusChord::parse("alt+pp"), None);
        assert_eq!(FocusChord::parse("alt+ "), None);
        assert_eq!(FocusChord::parse(""), None);
    }

    #[test]
    fn focus_keybind_kdl_targets_plugin_id_in_normal_and_locked_modes() {
        let kdl = focus_keybind_kdl(
            FocusChord {
                modifier: ChordModifier::Alt,
                key: 'p',
            },
            42,
        );

        assert_eq!(kdl.matches("bind \"Alt p\"").count(), 2);
        assert_eq!(kdl.matches("MessagePluginId 42").count(), 2);
        assert_eq!(kdl.matches("name \"rimz:focus_sidebar\"").count(), 2);
        assert!(kdl.contains("normal {"));
        assert!(kdl.contains("locked {"));
        assert!(!kdl.contains("MessagePlugin \""));
        assert!(!kdl.contains("plugin_url"));
    }

    #[test]
    fn focus_keybind_kdl_formats_ctrl_chords() {
        let kdl = focus_keybind_kdl(
            FocusChord {
                modifier: ChordModifier::Ctrl,
                key: 's',
            },
            7,
        );

        assert_eq!(kdl.matches("bind \"Ctrl s\"").count(), 2);
        assert!(kdl.contains("MessagePluginId 7"));
    }

    #[test]
    fn retire_generation_parses_writer_payload() {
        let writer = retire_generation(Some(r#"{"plugin_id":9,"loaded_at_ms":1000}"#))
            .expect("writer parses");

        assert_eq!(
            writer,
            policy::TopologyWriter {
                plugin_id: 9,
                loaded_at_ms: 1000,
            }
        );
        assert_eq!(retire_generation(None), None);
        assert_eq!(retire_generation(Some("garbage")), None);
    }
}
