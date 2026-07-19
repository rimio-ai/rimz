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
        pane_pid: None,
        terminal_command: Some("zsh".to_owned()),
    }
}
#[test]
fn topology_json_carries_focused_pane() {
    let panes = vec![pane(7)];
    let json = topology_json(
        Some("session-1"),
        42,
        Some(policy::TopologyWriter {
            plugin_id: 9,
            loaded_at_ms: 1_000,
            build: Some("wasm-build".to_owned()),
            config: Some("config-hash".to_owned()),
        }),
        Some(7),
        None,
        &panes,
    )
    .expect("topology serializes");
    let payload: serde_json::Value = serde_json::from_str(&json).expect("topology is JSON");

    assert_eq!(payload["focused_pane"], 7);
    assert_eq!(payload["writer"]["plugin_id"], 9);
    assert_eq!(payload["writer"]["loaded_at_ms"], 1000);
    assert_eq!(payload["writer"]["build"], "wasm-build");
    assert_eq!(payload["writer"]["config"], "config-hash");
}
#[test]
fn topology_json_carries_present_pid_and_omits_absent_pid() {
    let enriched = PaneFields {
        pane_command: Some("zsh".to_owned()),
        pane_cwd: Some("/repo/main".to_owned()),
        pane_pid: Some(707),
        ..pane(7)
    };
    let panes = vec![enriched, pane(8)];
    let json = topology_json(Some("session-1"), 42, None, Some(7), None, &panes)
        .expect("topology serializes");
    let payload: serde_json::Value = serde_json::from_str(&json).expect("topology is JSON");

    assert_eq!(payload["panes"][0]["pane_command"], "zsh");
    assert_eq!(payload["panes"][0]["pane_cwd"], "/repo/main");
    assert_eq!(payload["panes"][0]["pane_pid"], 707);
    assert!(payload["panes"][1].get("pane_pid").is_none());
}

#[test]
fn topology_json_carries_clients_when_sampled() {
    let panes = vec![pane(7)];
    let clients = policy::ClientSample {
        views: vec![policy::ClientViewEntry {
            client_id: 1,
            pane_id: policy::ClientPaneId::Terminal(7),
        }],
    };
    let json = topology_json(Some("session-1"), 42, None, Some(7), Some(&clients), &panes)
        .expect("topology serializes");
    let payload: serde_json::Value = serde_json::from_str(&json).expect("topology is JSON");

    assert!(payload["clients"].get("human_clients").is_none());
    assert!(payload["clients"].get("viewed_panes").is_none());
    assert_eq!(payload["clients"]["views"][0]["client_id"], 1);

    let json = topology_json(Some("session-1"), 42, None, Some(7), None, &panes)
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
            "--session-name",
            "session-1",
            "--workspace-id",
            "workspace-1",
            "--topology",
            "{\"topology\":true}",
        ])),
    );
}

#[test]
fn large_topology_is_chunked_and_reassembles_byte_identical() {
    let topology = format!(r#"{{"panes":"{}é"}}"#, "x".repeat(131_072));
    let argv = wake_argv(&ctx(), WakeRequest::Changed, Some(&topology)).expect("wake argv");
    let chunks = argv
        .windows(2)
        .filter(|pair| pair[0] == "--topology")
        .map(|pair| pair[1].as_str())
        .collect::<Vec<_>>();

    assert!(chunks.len() > 1);
    assert!(
        chunks
            .iter()
            .all(|chunk| chunk.len() <= TOPOLOGY_ARG_CHUNK_BYTES)
    );
    assert_eq!(chunks.concat(), topology);
}

#[test]
fn oversized_topology_keeps_wake_without_topology_arguments() {
    let topology = "x".repeat(TOPOLOGY_MAX_BYTES + 1);
    let argv = wake_argv(&ctx(), WakeRequest::Changed, Some(&topology)).expect("wake argv");

    assert!(!publishes_topology(&argv));
    assert_eq!(
        argv,
        strings(&[
            "/bin/rimz",
            "sidebar",
            "wake",
            "--reason",
            "panes-changed",
            "--session-name",
            "session-1",
            "--workspace-id",
            "workspace-1",
        ])
    );
}

#[test]
fn alive_wake_argv_carries_telemetry_before_session_name() {
    assert_eq!(
        wake_argv(
            &ctx(),
            WakeRequest::Alive(PluginTelemetry {
                plugin_id: Some(9),
                plugin_build: Some("wasm-build".to_owned()),
                loaded_at_ms: 1_000,
                mem_pages: 12,
                uptime_ms: 34,
                commands_completed: 56,
                commands_succeeded: 49,
                commands_failed: 7,
                stale_writer_rejections: 3,
                topology_failures: 2,
                other_failures: 2,
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
            "--plugin-id",
            "9",
            "--plugin-build",
            "wasm-build",
            "--plugin-loaded-at-ms",
            "1000",
            "--plugin-mem-pages",
            "12",
            "--plugin-uptime-ms",
            "34",
            "--plugin-commands",
            "56",
            "--plugin-commands-succeeded",
            "49",
            "--plugin-commands-failed",
            "7",
            "--plugin-stale-writer-rejections",
            "3",
            "--plugin-topology-failures",
            "2",
            "--plugin-other-failures",
            "2",
            "--plugin-zellij-version",
            "0.44.3",
            "--session-name",
            "session-1",
        ])),
    );
}

#[test]
fn command_counters_split_every_exit_bucket() {
    let mut counters = CommandCounters::default();
    counters.record(Some(0), true);
    counters.record(Some(STALE_WRITER_EXIT_CODE), true);
    counters.record(Some(1), true);
    counters.record(None, false);
    counters.record(Some(STALE_WRITER_EXIT_CODE), false);

    assert_eq!(counters.completed, 5);
    assert_eq!(counters.succeeded, 1);
    assert_eq!(counters.stale_writer_rejections, 1);
    assert_eq!(counters.topology_failures, 1);
    assert_eq!(counters.other_failures, 2);
    assert_eq!(counters.failed(), 4);
}

#[test]
fn switch_settled_wake_argv_carries_observation() {
    assert_eq!(
        wake_argv(
            &ctx(),
            WakeRequest::SwitchSettled {
                tab: 1,
                generation: 3,
                clients: vec![policy::ClientViewEntry {
                    client_id: 1,
                    pane_id: policy::ClientPaneId::Terminal(9),
                }],
            },
            None,
        ),
        Some(strings(&[
            "/bin/rimz",
            "sidebar",
            "wake",
            "--reason",
            "switch-settled",
            "--session-name",
            "session-1",
            "--active-tab",
            "1",
            "--focus-generation",
            "3",
            "--focus-clients",
            r#"[{"client_id":1,"pane_id":{"kind":"terminal","id":9}}]"#,
            "--workspace-id",
            "workspace-1",
        ])),
    );
}

#[test]
fn wake_argv_none_gates_session_bound_requests() {
    let no_session = WakeContext {
        rimz_bin: None,
        workspace_id: None,
        session_name: None,
    };
    assert_eq!(wake_argv(&no_session, WakeRequest::Changed, None), None,);
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

    assert!(kdl.starts_with("focus_follows_mouse false\nmouse_click_through true\nkeybinds {\n"));
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
    let writer =
        retire_generation(Some(r#"{"plugin_id":9,"loaded_at_ms":1000}"#)).expect("writer parses");

    assert_eq!(
        writer,
        policy::TopologyWriter {
            plugin_id: 9,
            loaded_at_ms: 1000,
            build: None,
            config: None,
        }
    );
    assert_eq!(retire_generation(None), None);
    assert_eq!(retire_generation(Some("garbage")), None);
}
