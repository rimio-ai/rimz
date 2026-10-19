#[cfg(feature = "testkit")]
#[test]
fn play_options_are_typed_and_validated_before_dispatch() {
    use clap::Parser;
    use clap::error::ErrorKind;
    use std::time::Duration;

    let parse = |args: &[&str]| {
        crate::cli::Cli::try_parse_from(
            ["rimz", "sidebar", "play", "pilot.scn"]
                .into_iter()
                .chain(args.iter().copied()),
        )
    };
    let cli = parse(&["--at", "1m30s", "--theme-mode", "256", "--width", "54"]).unwrap();
    let Some(crate::cli::Subcmd::Sidebar(super::SidebarArgs {
        command: super::SidebarSubcmd::Play { at, theme_mode, .. },
    })) = cli.subcommand
    else {
        panic!("expected play command");
    };
    assert_eq!(at, Some(Duration::from_secs(90)));
    assert_eq!(theme_mode, Some(rimz::config::ThemeMode::Indexed));

    let cli = parse(&["--frames", "out", "--motion", "500ms..1.5s"]).unwrap();
    let Some(crate::cli::Subcmd::Sidebar(super::SidebarArgs {
        command: super::SidebarSubcmd::Play { motion, .. },
    })) = cli.subcommand
    else {
        panic!("expected play command");
    };
    assert_eq!(
        motion,
        Some(Duration::from_millis(500)..Duration::from_millis(1500))
    );

    for (args, kind) in [
        (vec!["--at", "later"], ErrorKind::ValueValidation),
        (
            vec!["--frames", "out", "--motion", "1s"],
            ErrorKind::ValueValidation,
        ),
        (
            vec!["--frames", "out", "--motion", "9s..6s"],
            ErrorKind::ValueValidation,
        ),
        (
            vec!["--at", "0s", "--theme-mode", "rgb"],
            ErrorKind::InvalidValue,
        ),
        (vec!["--height", "34"], ErrorKind::MissingRequiredArgument),
        (vec!["--width", "54"], ErrorKind::MissingRequiredArgument),
        (
            vec!["--theme-mode", "auto"],
            ErrorKind::MissingRequiredArgument,
        ),
        (
            vec!["--theme-scheme", "dark"],
            ErrorKind::MissingRequiredArgument,
        ),
    ] {
        assert_eq!(parse(&args).unwrap_err().kind(), kind, "{args:?}");
    }
}

fn parse_wake(argv: &[&str]) -> Box<super::WakeArgs> {
    use clap::Parser;

    let cli = crate::cli::Cli::try_parse_from(argv.iter().copied()).expect("wake argv parses");
    let Some(crate::cli::Subcmd::Sidebar(super::SidebarArgs {
        command: super::SidebarSubcmd::Wake(args),
    })) = cli.subcommand
    else {
        panic!("expected sidebar wake command");
    };
    args
}

#[test]
fn plugin_focus_argv_parses_without_workspace_id() {
    use clap::Parser;

    crate::cli::Cli::try_parse_from([
        "rimz",
        "sidebar",
        "focus",
        "--toggle",
        "--session-name",
        "s",
        "--mux",
        "zellij",
    ])
    .expect("plugin focus argv must parse");

    assert!(
        crate::cli::Cli::try_parse_from([
            "rimz",
            "sidebar",
            "focus",
            "--toggle",
            "--workspace-id",
            "ws_0123456789abcdef01234567",
            "--session-name",
            "s",
            "--mux",
            "zellij",
        ])
        .is_err(),
        "sidebar focus intentionally accepts no workspace id",
    );
}

#[test]
fn legacy_zellij_wake_shape_remains_parseable() {
    use clap::Parser;

    crate::cli::Cli::try_parse_from([
        "rimz",
        "sidebar",
        "wake",
        "--reason",
        "pane-opened",
        "--command-arg",
        "codex",
        "--focused-pane-id",
        "terminal_7",
        "--unfocused-pane-id",
        "terminal_6",
        "--topology",
        r#"{"session_name":"rimz-test","produced_at_ms":1,"panes":[]}"#,
    ])
    .expect("legacy Zellij wake parses after a host upgrade");
}

#[test]
fn plugin_wake_argv_accepts_repeated_topology_chunks() {
    use clap::Parser;

    crate::cli::Cli::try_parse_from([
        "rimz",
        "sidebar",
        "wake",
        "--reason",
        "alive",
        "--topology",
        "{\"panes\":",
        "--topology",
        "--a-chunk-may-start-with-hyphens}",
    ])
    .expect("plugin topology chunks must append in order");
}

#[test]
fn settled_switch_wake_argv_parses() {
    use clap::Parser;

    crate::cli::Cli::try_parse_from([
        "rimz",
        "sidebar",
        "wake",
        "--reason",
        "switch-settled",
        "--session-name",
        "rimz-test",
        "--active-tab",
        "1",
        "--focus-generation",
        "8",
        "--focus-clients",
        r#"[{"client_id":1,"pane_id":{"kind":"terminal","id":10}}]"#,
    ])
    .expect("settled switch wake parses");
}

#[test]
fn plugin_telemetry_decodes_structured_and_legacy_argv() {
    let structured = parse_wake(&[
        "rimz",
        "sidebar",
        "wake",
        "--reason",
        "alive",
        "--plugin-telemetry",
        r#"{"plugin_id":9,"plugin_build":"wasm-build","loaded_at_ms":1000,"mem_pages":12,"uptime_ms":34,"commands_completed":56,"commands_succeeded":49,"commands_failed":7,"stale_writer_rejections":3,"topology_failures":2,"other_failures":2,"zellij_version":"0.44.3","last_failure":{"exit_code":1,"detail":"Error: could not publish accepted topology"}}"#,
        "--plugin-mem-pages",
        "999",
    ]);
    assert_eq!(
        structured.telemetry(),
        Some(rimz::sidebar::presence::ZellijPluginTelemetry {
            plugin_id: Some(9),
            build: Some("wasm-build".to_owned()),
            loaded_at_ms: 1_000,
            pages: 12,
            uptime_ms: 34,
            commands: 56,
            commands_succeeded: Some(49),
            stale_writer_rejections: Some(3),
            topology_failures: Some(2),
            other_failures: Some(2),
            zellij_version: Some("0.44.3".to_owned()),
            last_failure: Some(rimz::diag::plugin_presence::PluginCommandFailure {
                exit_code: Some(1),
                detail: "Error: could not publish accepted topology".to_owned(),
                // This payload predates the stamp, as a plugin loaded before it
                // still sends; the cause survives without a time.
                at_ms: None,
            }),
        }),
        "structured telemetry supersedes legacy flags",
    );

    let stamped = parse_wake(&[
        "rimz",
        "sidebar",
        "wake",
        "--reason",
        "alive",
        "--plugin-telemetry",
        r#"{"plugin_id":9,"loaded_at_ms":1000,"mem_pages":12,"uptime_ms":34,"commands_completed":56,"last_failure":{"exit_code":2,"detail":"No such file or directory (os error 2)","at_ms":4242}}"#,
    ]);
    assert_eq!(
        stamped
            .telemetry()
            .and_then(|telemetry| telemetry.last_failure)
            .and_then(|failure| failure.at_ms),
        Some(4_242),
        "a stamped cause carries the time the host judges it by",
    );

    let legacy = parse_wake(&[
        "rimz",
        "sidebar",
        "wake",
        "--reason",
        "alive",
        "--plugin-id",
        "8",
        "--plugin-build",
        "old-build",
        "--plugin-loaded-at-ms",
        "900",
        "--plugin-mem-pages",
        "10",
        "--plugin-uptime-ms",
        "30",
        "--plugin-commands",
        "40",
        "--plugin-commands-failed",
        "5",
        "--plugin-zellij-version",
        "0.43.1",
    ]);
    assert_eq!(
        legacy.telemetry(),
        Some(rimz::sidebar::presence::ZellijPluginTelemetry {
            plugin_id: Some(8),
            build: Some("old-build".to_owned()),
            loaded_at_ms: 900,
            pages: 10,
            uptime_ms: 30,
            commands: 40,
            commands_succeeded: None,
            stale_writer_rejections: None,
            topology_failures: None,
            other_failures: None,
            zellij_version: Some("0.43.1".to_owned()),
            last_failure: None,
        }),
    );
}

#[test]
fn focus_repair_writer_argv_parses_structured_record() {
    use clap::Parser;
    use rimz::diag::focus_repair::{FocusRepairOutcome, FocusRepairRecord};

    let record = FocusRepairRecord {
        at: jiff::Timestamp::from_second(20).expect("timestamp"),
        nonce: Some("nonce-1".to_owned()),
        workspace_id: rimz::WorkspaceId::parse("ws_0123456789abcdef01234567").expect("workspace"),
        session_name: "room".to_owned(),
        generation: 1,
        evidence: Vec::new(),
        observed: Vec::new(),
        attempts: 1,
        target: rimz::PaneId::from_parts(rimz::MuxName::Zellij, "terminal_1"),
        outcome: FocusRepairOutcome::AcceptedUnconfirmed,
        error: None,
    };
    let record_json = serde_json::to_string(&record).expect("serialize");
    crate::cli::Cli::try_parse_from([
        "rimz",
        "sidebar",
        "record-focus-repair",
        "--record-json",
        &record_json,
    ])
    .expect("focus repair writer argv must parse");
}
