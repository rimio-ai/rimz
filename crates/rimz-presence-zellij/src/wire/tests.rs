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

#[test]
fn input_wake_carries_only_the_client_clock_not_topology() {
    let request = WakeRequest::Input {
        client_id: 17,
        last_input_ms: 1_234,
    };
    assert_eq!(
        wake_argv(&ctx(), request.clone(), Some("{\"topology\":true}")),
        Some(strings(&[
            "/bin/rimz",
            "sidebar",
            "wake",
            "--reason",
            "input",
            "--session-name",
            "session-1",
            "--workspace-id",
            "workspace-1",
            "--client-id",
            "17",
            "--last-input-ms",
            "1234",
        ]))
    );
    assert!(
        wake_argv(
            &WakeContext {
                session_name: None,
                ..ctx()
            },
            request,
            None
        )
        .is_none()
    );
}

fn pane(id: u32) -> PaneFields {
    PaneFields {
        id,
        is_plugin: false,
        is_fullscreen: false,
        is_suppressed: false,
        is_floating: false,
        exited: false,
        is_held: false,
        tab_position: 0,
        tab_viewed: false,
        stable_tab_id: Some(0),
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
fn topology_json_carries_writer_without_focus_verdict() {
    let panes = vec![pane(7)];
    let json = topology_json(
        Some("session-1"),
        42,
        Some(policy::TopologyWriter {
            plugin_id: 9,
            client_id: None,
            loaded_at_ms: 1_000,
            build: Some("wasm-build".to_owned()),
            config: Some("config-hash".to_owned()),
        }),
        None,
        false,
        &panes,
    )
    .expect("topology serializes");
    let payload: serde_json::Value = serde_json::from_str(&json).expect("topology is JSON");

    assert!(payload.get("focused_pane").is_none());
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
    let json = topology_json(Some("session-1"), 42, None, None, false, &panes)
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
    let json = topology_json(Some("session-1"), 42, None, Some(&clients), false, &panes)
        .expect("topology serializes");
    let payload: serde_json::Value = serde_json::from_str(&json).expect("topology is JSON");

    assert!(payload["clients"].get("human_clients").is_none());
    assert!(payload["clients"].get("viewed_panes").is_none());
    assert_eq!(payload["clients"]["views"][0]["client_id"], 1);

    let json = topology_json(Some("session-1"), 42, None, None, false, &panes)
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
                stale_writer_rejections: 3,
                topology_failures: 2,
                other_failures: 2,
                zellij_version: "0.44.3".to_owned(),
                last_failure: None,
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
            "--plugin-telemetry",
            r#"{"plugin_id":9,"plugin_build":"wasm-build","loaded_at_ms":1000,"mem_pages":12,"uptime_ms":34,"commands_completed":56,"commands_succeeded":49,"stale_writer_rejections":3,"topology_failures":2,"other_failures":2,"zellij_version":"0.44.3"}"#,
            "--session-name",
            "session-1",
        ])),
    );
}

#[test]
fn command_counters_split_every_exit_bucket() {
    let mut counters = CommandCounters::default();
    assert_eq!(counters.record(Some(0), true), CommandOutcome::Succeeded);
    assert_eq!(
        counters.record(Some(STALE_WRITER_EXIT_CODE), true),
        CommandOutcome::StaleWriter
    );
    assert_eq!(
        counters.record(Some(1), true),
        CommandOutcome::TopologyFailure
    );
    assert_eq!(counters.record(None, false), CommandOutcome::OtherFailure);
    assert_eq!(
        counters.record(Some(STALE_WRITER_EXIT_CODE), false),
        CommandOutcome::OtherFailure
    );

    assert_eq!(counters.completed, 5);
    assert_eq!(counters.succeeded, 1);
    assert_eq!(counters.stale_writer_rejections, 1);
    assert_eq!(counters.topology_failures, 1);
    assert_eq!(counters.other_failures, 2);
}

/// `rimz doctor` reads the other-failure count and the retained failure as wake
/// health, so a failing key binding must reach neither.
#[test]
fn key_command_results_leave_wake_counters_and_retained_failure_alone() {
    let mut counters = CommandCounters::default();
    let wake = fold_command_result(&mut counters, None, false, false, Some(1), b"wake died", 5);
    let after_key = fold_command_result(
        &mut counters,
        wake.clone(),
        true,
        false,
        Some(2),
        b"error: unrecognized subcommand 'walk'",
        6,
    );

    assert_eq!(after_key, wake);
    assert_eq!(counters.completed, 1);
    assert_eq!(counters.other_failures, 1);
    assert_eq!(
        fold_command_result(&mut counters, None, true, false, Some(2), b"gone", 7),
        None
    );
    assert_eq!(counters.completed, 1);
}

/// The host names the cause on its first stderr line; the rest is argv the
/// reader already has.
#[test]
fn command_failure_keeps_the_first_meaningful_stderr_line() {
    let failure = CommandFailure::new(
        Some(1),
        b"\n  \nError: could not serialize topology writer selection: lock timeout\n\nCaused by:\n    0: timed out\n",
        7_000,
    );

    assert_eq!(failure.exit_code, Some(1));
    assert_eq!(failure.at_ms, 7_000);
    assert_eq!(
        failure.detail,
        "Error: could not serialize topology writer selection: lock timeout"
    );
}

#[test]
fn command_failure_bounds_the_detail_on_a_char_boundary() {
    let failure = CommandFailure::new(None, "é".repeat(200).as_bytes(), 0);

    assert_eq!(failure.exit_code, None);
    assert!(failure.detail.len() <= FAILURE_DETAIL_MAX_BYTES);
    assert!(failure.detail.chars().all(|ch| ch == 'é'));
}

/// A wake that dies without writing anything still carries its exit status.
#[test]
fn command_failure_survives_silent_stderr() {
    assert_eq!(CommandFailure::new(Some(101), b"", 0).detail, "");
    assert_eq!(CommandFailure::new(Some(101), b"   \n\n", 0).detail, "");
}

/// The fence rejecting a losing writer is designed behaviour, so it must not
/// masquerade as the cause of the failures the reader is chasing.
#[test]
fn fold_failure_keeps_real_causes_and_ignores_the_writer_fence() {
    let real = fold_failure(
        None,
        CommandOutcome::TopologyFailure,
        Some(1),
        b"lock timeout",
        5_000,
    );
    assert_eq!(
        real.as_ref().map(|failure| failure.detail.as_str()),
        Some("lock timeout")
    );
    assert_eq!(real.as_ref().map(|failure| failure.at_ms), Some(5_000));

    // A stale-writer rejection neither reports a cause nor buries the last one.
    let after_fence = fold_failure(
        real.clone(),
        CommandOutcome::StaleWriter,
        Some(STALE_WRITER_EXIT_CODE),
        b"rejected as stale",
        6_000,
    );
    assert_eq!(after_fence, real);
    assert_eq!(
        fold_failure(
            None,
            CommandOutcome::StaleWriter,
            Some(STALE_WRITER_EXIT_CODE),
            b"",
            6_000
        ),
        None
    );

    // Wakes outrun telemetry sampling, so a success that cleared the evidence
    // took the cause of an intermittent failure with it before any sample could
    // carry it. The record survives the recovery and the host judges its age.
    assert_eq!(
        fold_failure(real.clone(), CommandOutcome::Succeeded, Some(0), b"", 7_000),
        real
    );

    // A later failure still replaces it, stamp and all.
    let newer = fold_failure(
        real,
        CommandOutcome::OtherFailure,
        Some(2),
        b"no such file",
        8_000,
    );
    assert_eq!(
        newer.map(|failure| (failure.detail, failure.at_ms)),
        Some(("no such file".to_owned(), 8_000))
    );
}

#[test]
fn alive_wake_argv_carries_the_last_failure() {
    let telemetry = PluginTelemetry {
        plugin_id: Some(9),
        plugin_build: None,
        loaded_at_ms: 1_000,
        mem_pages: 12,
        uptime_ms: 34,
        commands_completed: 56,
        commands_succeeded: 49,
        stale_writer_rejections: 3,
        topology_failures: 2,
        other_failures: 2,
        zellij_version: "0.44.3".to_owned(),
        last_failure: Some(CommandFailure {
            exit_code: Some(1),
            detail: "Error: could not publish accepted topology".to_owned(),
            at_ms: 2_500,
        }),
    };
    let argv = wake_argv(&ctx(), WakeRequest::Alive(telemetry), None).expect("argv");
    let payload = argv
        .iter()
        .skip_while(|arg| *arg != "--plugin-telemetry")
        .nth(1)
        .expect("telemetry payload");

    assert!(
        payload.contains(r#""last_failure":{"exit_code":1,"detail":"Error: could not publish accepted topology","at_ms":2500}"#),
        "payload carries the cause and when it happened: {payload}",
    );
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
fn palette_argv_preserves_the_opener_and_omits_absent_context() {
    assert_eq!(PALETTE_PIPE, "rimz:palette");
    assert_eq!(
        palette_argv(&ctx(), "commands", Some(policy::ClientPaneId::Terminal(7))),
        strings(&[
            "/bin/rimz",
            "palette",
            "--popup",
            "--mode",
            "commands",
            "--from-pane",
            "zellij:terminal_7",
            "--session-name",
            "session-1",
            "--mux",
            "zellij",
        ]),
    );
    assert_eq!(
        palette_argv(
            &WakeContext {
                rimz_bin: None,
                workspace_id: None,
                session_name: None,
            },
            "cards",
            None,
        ),
        strings(&[
            "rimz", "palette", "--popup", "--mode", "cards", "--mux", "zellij"
        ]),
    );
    assert_eq!(
        palette_argv(&ctx(), "new", Some(policy::ClientPaneId::Plugin(8))),
        strings(&[
            "/bin/rimz",
            "palette",
            "--popup",
            "--mode",
            "new",
            "--from-pane",
            "zellij:plugin_8",
            "--session-name",
            "session-1",
            "--mux",
            "zellij",
        ]),
    );
}

fn native_keys() -> Keymap {
    let binds = serde_json::json!([
        {"key":"Alt p","steps":[{"run":0}]},
        {"key":"Alt g","steps":[{"run":1}]}
    ]);
    serde_json::from_value(serde_json::json!({
        "report_modes":false,
        "commands":[
            ["sidebar","focus","--toggle","--session-name","session-1","--mux","zellij"],
            ["pane","zoom","--session-name","session-1","--mux","zellij"]
        ],
        "modes":[
            {"mode":"normal","clear":false,"binds":binds},
            {"mode":"locked","clear":false,"binds":binds}
        ]
    }))
    .unwrap()
}

#[test]
fn runtime_reconfigure_kdl_emits_options_without_keybinds() {
    let kdl = runtime_reconfigure_kdl(&RuntimeReconfigure {
        session_options: &[
            ("pane_frames".to_owned(), "true".to_owned()),
            ("empty".to_owned(), String::new()),
            ("escaped".to_owned(), "a\"b\n".to_owned()),
            ("non_ascii_digits".to_owned(), "１２".to_owned()),
        ],
        ..RuntimeReconfigure::default()
    })
    .expect("options produce a reconfigure payload");

    assert_eq!(
        kdl,
        "pane_frames true\nempty \"\"\nescaped \"a\\\"b\\n\"\nnon_ascii_digits \"１２\"\n"
    );
}

#[test]
fn runtime_reconfigure_kdl_drops_a_key_that_is_not_an_identifier() {
    let kdl = runtime_reconfigure_kdl(&RuntimeReconfigure {
        session_options: &[
            (
                "pane_frames true\nkeybinds { bind \"F1\"".to_owned(),
                "true".to_owned(),
            ),
            ("pane-frames".to_owned(), "true".to_owned()),
            (String::new(), "true".to_owned()),
            ("scroll_buffer_size".to_owned(), "123456".to_owned()),
        ],
        ..RuntimeReconfigure::default()
    })
    .expect("the identifier key still produces a reconfigure payload");

    assert_eq!(kdl, "scroll_buffer_size 123456\n");
}

#[test]
fn runtime_reconfigure_kdl_combines_options_and_room_keybinds() {
    let kdl = runtime_reconfigure_kdl(&RuntimeReconfigure {
        plugin_id: Some(42),
        keymap: Some(&native_keys()),
        rimz_bin: None,
        // Key order, the order `load` collects the host's entries from the
        // `BTreeMap` Zellij hands it.
        session_options: &[
            ("on_force_close".to_owned(), "quit".to_owned()),
            ("pane_frames".to_owned(), "false".to_owned()),
            ("scroll_buffer_size".to_owned(), "123456".to_owned()),
        ],
    })
    .expect("focus key and options produce a reconfigure payload");

    assert!(kdl.starts_with(
        "on_force_close \"quit\"\npane_frames false\nscroll_buffer_size 123456\nkeybinds {\n"
    ));
    assert!(kdl.contains("bind \"Alt p\""));
    assert!(kdl.contains("bind \"Alt g\""));
    assert_eq!(kdl.matches("name \"rimz:key\"").count(), 4);
    assert_eq!(kdl.matches("MessagePluginId 42").count(), 4);
    assert!(!kdl.contains("MessagePlugin \""));
    assert!(!kdl.contains("plugin_url"));
}

#[test]
fn runtime_reconfigure_kdl_emits_keybinds_without_options() {
    let kdl = runtime_reconfigure_kdl(&RuntimeReconfigure {
        plugin_id: Some(42),
        keymap: Some(&native_keys()),
        ..RuntimeReconfigure::default()
    })
    .expect("keybinds produce a reconfigure payload");

    assert!(kdl.starts_with("keybinds {\n"));
    assert!(kdl.contains("bind \"Alt p\""));
}

#[test]
fn runtime_reconfigure_kdl_skips_empty_payload() {
    assert!(runtime_reconfigure_kdl(&RuntimeReconfigure::default()).is_none());
    assert!(
        runtime_reconfigure_kdl(&RuntimeReconfigure {
            plugin_id: Some(42),
            ..RuntimeReconfigure::default()
        })
        .is_none()
    );
    assert!(
        runtime_reconfigure_kdl(&RuntimeReconfigure {
            keymap: Some(&native_keys()),
            ..RuntimeReconfigure::default()
        })
        .is_none()
    );
}

#[test]
fn fullscreen_payload_parses_terminal_and_plugin_ids() {
    assert_eq!(
        fullscreen_pane(Some("terminal_7")),
        Some(policy::ClientPaneId::Terminal(7))
    );
    assert_eq!(
        fullscreen_pane(Some("plugin_9")),
        Some(policy::ClientPaneId::Plugin(9))
    );
    assert_eq!(fullscreen_pane(Some("terminal_nope")), None);
    assert_eq!(fullscreen_pane(None), None);
}

#[test]
fn retire_generation_parses_writer_payload() {
    let writer =
        retire_generation(Some(r#"{"plugin_id":9,"loaded_at_ms":1000}"#)).expect("writer parses");

    assert_eq!(
        writer,
        policy::TopologyWriter {
            plugin_id: 9,
            client_id: None,
            loaded_at_ms: 1000,
            build: None,
            config: None,
        }
    );
    assert_eq!(retire_generation(None), None);
    assert_eq!(retire_generation(Some("garbage")), None);
}
