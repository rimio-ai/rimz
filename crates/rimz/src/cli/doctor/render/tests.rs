use super::*;
use crate::cli::doctor::model::{
    HookRow, Host, IncidentAgent, LastIncident, LoopTaskRow, MessageProblemRow, MuxBinaries,
    OpenCounts, PresencePluginRow, PresencePluginStatus, PresencePluginTelemetry, PresencePlugins,
    RemoteAgent, StorageRootView, TmuxCaps,
};
use rimz::ids::MuxName;

fn strip(render_one: impl FnOnce(&mut anstream::StripStream<Vec<u8>>) -> io::Result<()>) -> String {
    let mut stream = anstream::StripStream::new(Vec::new());
    render_one(&mut stream).expect("render to in-memory buffer");
    String::from_utf8(stream.into_inner()).expect("utf-8")
}

fn terminal_fixture() -> Terminal {
    Terminal {
        theme_mode: rimz::config::ThemeMode::Auto,
        truecolor_advertised: true,
        resolved_depth: "truecolor",
        colorterm: None,
        term: Some("xterm-ghostty".to_owned()),
        terminfo_truecolor: true,
        fix: None,
    }
}

fn mux_fixture() -> Mux {
    Mux {
        name: MuxName::Tmux,
        version: Version::Reported {
            version: "tmux 3.5".to_owned(),
        },
        capabilities: Capabilities::Tmux(Probe::Ready(TmuxCaps {
            meets_min_version: true,
            min_version: (3, 5, 0),
            popup_supported: true,
        })),
        binaries: MuxBinaries {
            active: Some(MuxBinaryRow {
                path: "/usr/bin/tmux".to_owned(),
                version: Some("tmux 3.5".to_owned()),
            }),
            duplicates: Vec::new(),
            server_mismatches: Vec::new(),
        },
        log: MuxLog::Disabled {
            hint: "server logging off (start tmux with `-v` to enable)".to_owned(),
        },
        room: None,
        presence_plugins: None,
        zellij_socket: None,
        socket: Some("/tmp/tmux-1001/default".to_owned()),
        session_health: None,
        duplicate_sessions: None,
        presence: None,
        topology_writer: None,
        ttyd: None,
    }
}

fn storage_fixture() -> Storage {
    Storage {
        total_bytes: 0,
        roots: Vec::new(),
    }
}

fn report_fixture() -> DoctorReport {
    DoctorReport {
        schema: "rimz.doctor.v1",
        version: rimz::build_id::VERSION,
        host: Host {
            user: None,
            uid: 0,
            binary: None,
        },
        workspace: Probe::Unavailable {
            error: "test".to_owned(),
        },
        mux: Probe::Unavailable {
            error: "test".to_owned(),
        },
        terminal: terminal_fixture(),
        machine_config: MachineConfigHealth {
            broken_files: Vec::new(),
        },
        hooks: Vec::new(),
        plugins: Vec::new(),
        loop_tasks: LoopTasks { tasks: Vec::new() },
        remote_control: RemoteControl::Off,
        disk_usage: storage_fixture(),
        protocols: None,
        trust: None,
        agents: None,
        history_cleared_at: None,
        messages: None,
        diagnostics: None,
        last_incident: None,
    }
}

#[test]
fn render_identity_shows_user_and_binary() {
    let host = Host {
        user: Some("eddie".to_owned()),
        uid: 1001,
        binary: Some("/home/eddie/.cargo/bin/rimz".to_owned()),
    };
    let out = strip(|w| render_identity(w, "0.1.0", &host));
    assert!(out.contains("RimZ doctor"), "{out}");
    assert!(out.contains("0.1.0"), "{out}");
    assert!(out.contains("eddie (uid 1001)"), "{out}");
    assert!(out.contains("/home/eddie/.cargo/bin/rimz"), "{out}");
}

#[test]
fn terminal_section_renders_depth_signals_and_fix() {
    let terminal = Terminal {
        truecolor_advertised: false,
        resolved_depth: "256",
        term: Some("xterm-256color".to_owned()),
        terminfo_truecolor: false,
        fix: Some("set `[theme] mode = \"truecolor\"` to force RGB".to_owned()),
        ..terminal_fixture()
    };
    let out = strip(|w| {
        let mut tally = Tally::default();
        render_terminal(w, &terminal, &mut tally)
    });
    assert!(out.contains("TERMINAL"), "section title:\n{out}");
    assert!(out.contains("256 (mode auto)"), "resolved depth:\n{out}");
    assert!(out.contains("truecolor-advertised=false"), "{out}");
    assert!(out.contains("COLORTERM=unset"), "{out}");
    assert!(out.contains("TERM=xterm-256color"), "{out}");
    assert!(out.contains("terminfo-truecolor=false"), "{out}");
    assert!(
        out.contains("mode = \"truecolor\""),
        "fix command is present:\n{out}"
    );
}

#[test]
fn hooks_section_renders_glyph_status_and_fix() {
    let report = DoctorReport {
        hooks: vec![
            HookRow {
                kind: "claude".to_owned(),
                detected: true,
                status: HookStatus::Installed,
            },
            HookRow {
                kind: "codex".to_owned(),
                detected: true,
                status: HookStatus::NotInstalled {
                    fix: "run `rimz hooks install codex` to wire codex agents".to_owned(),
                },
            },
            HookRow {
                kind: "kiro".to_owned(),
                detected: false,
                status: HookStatus::Unsupported {
                    reason: "unsupported hooks".to_owned(),
                },
            },
            HookRow {
                kind: "grok".to_owned(),
                detected: false,
                status: HookStatus::NotDetected,
            },
        ],
        ..report_fixture()
    };
    let out = strip(|w| {
        let mut tally = Tally::default();
        render_hooks(w, &report, &mut tally)?;
        render_tally(w, &tally)
    });
    for expected in [
        "HOOKS",
        "✓",
        "✗",
        "installed",
        "not installed",
        "rimz hooks install codex",
        "not detected on this machine: grok, kiro",
        "hooks are offered automatically once an agent is installed",
        "✗ 1 problem, ! 0 warnings",
    ] {
        assert!(out.contains(expected), "missing {expected}:\n{out}");
    }
    assert!(!out.contains("unsupported"), "{out}");
}

#[test]
fn loop_section_lists_tasks_and_flags_invalid_ones() {
    let loop_tasks = LoopTasks {
        tasks: vec![
            LoopTaskRow {
                name: "morning".to_owned(),
                spec: "claude".to_owned(),
                when: "07:00 on weekdays".to_owned(),
                root: "/home/you/code/app".to_owned(),
                valid: true,
            },
            LoopTaskRow {
                name: "broken".to_owned(),
                spec: "codex".to_owned(),
                when: "invalid: bad time".to_owned(),
                root: "/home/you/code/other".to_owned(),
                valid: false,
            },
        ],
    };
    let out = strip(|w| {
        let mut tally = Tally::default();
        render_loop(w, &loop_tasks, &mut tally)
    });
    assert!(out.contains("LOOP TASKS"), "section title:\n{out}");
    assert!(
        out.contains("morning") && out.contains("07:00 on weekdays"),
        "{out}"
    );
    assert!(
        out.contains("broken") && out.contains("invalid: bad time"),
        "{out}"
    );
    assert!(
        out.contains('✗'),
        "an invalid schedule carries a cross:\n{out}"
    );
    assert!(
        out.contains("rimz loop list"),
        "the installed-state hint is present:\n{out}"
    );
}

#[test]
fn mux_section_shows_backend_socket() {
    let mux = mux_fixture();
    let out = strip(|w| {
        let mut tally = Tally::default();
        render_mux(w, &Probe::Ready(mux), &mut tally)
    });
    assert!(out.contains("MULTIPLEXER"), "{out}");
    assert!(out.contains("/tmp/tmux-1001/default"), "{out}");
}

#[test]
fn mux_section_renders_multiple_presence_plugin_generations() {
    let row = |plugin_id, loaded_at_ms, build: &str, status, rejected_count| PresencePluginRow {
        plugin_id,
        loaded_at_ms: Some(loaded_at_ms),
        build: Some(build.to_owned()),
        status,
        rejected_count,
        outdated: build != "desired-build",
        telemetry: Some(PresencePluginTelemetry {
            sample_count: 2,
            first_at_ms: loaded_at_ms,
            last_at_ms: loaded_at_ms,
            last_seen_age_secs: 3,
            zellij_version: Some("0.44.3".to_owned()),
            page_growth: 1,
            byte_growth: 65_536,
            commands_completed_delta: 2,
            commands_succeeded_delta: Some(2),
            stale_writer_rejections_delta: Some(0),
            topology_failures_delta: Some(0),
            other_failures_delta: Some(0),
        }),
    };
    let mut mux = mux_fixture();
    mux.presence_plugins = Some(Probe::Ready(PresencePlugins {
        desired_build: Some("desired-build".to_owned()),
        rows: vec![
            row(
                49,
                1_000,
                "desired-build",
                PresencePluginStatus::Active,
                None,
            ),
            row(
                41,
                2_000,
                "old-build",
                PresencePluginStatus::Rejected,
                Some(3),
            ),
        ],
        history: vec![
            "/tmp/plugin-presence.log.jsonl".to_owned(),
            "/tmp/plugin-presence.log.1.jsonl".to_owned(),
        ],
    }));

    let out = strip(|w| {
        let mut tally = Tally::default();
        render_mux(w, &Probe::Ready(mux), &mut tally)?;
        render_tally(w, &tally)
    });

    for expected in [
        "presence plugins",
        "desired desired-",
        "2 loaded",
        "multiple presence plugins",
        "plugin 49",
        "loaded 00:00:01",
        "build desired-",
        "zellij 0.44.3",
        "active",
        "plugin 41",
        "rejected ×3",
        "rimz reload",
        "outdated",
        "history",
        "/tmp/plugin-presence.log.jsonl (+ rotated .1)",
        "! 1 warning",
    ] {
        assert!(out.contains(expected), "missing {expected}:\n{out}");
    }
}

#[test]
fn mux_section_warns_on_recent_presence_plugin_failures() {
    let mut mux = mux_fixture();
    mux.presence_plugins = Some(Probe::Ready(PresencePlugins {
        desired_build: Some("desired-build".to_owned()),
        rows: vec![PresencePluginRow {
            plugin_id: 49,
            loaded_at_ms: Some(1_000),
            build: Some("desired-build".to_owned()),
            status: PresencePluginStatus::Active,
            rejected_count: None,
            outdated: false,
            telemetry: Some(PresencePluginTelemetry {
                sample_count: 2,
                first_at_ms: 1_000,
                last_at_ms: 2_000,
                last_seen_age_secs: 3,
                zellij_version: Some("0.44.3".to_owned()),
                page_growth: 0,
                byte_growth: 0,
                commands_completed_delta: 1,
                commands_succeeded_delta: Some(0),
                stale_writer_rejections_delta: Some(0),
                topology_failures_delta: Some(1),
                other_failures_delta: Some(0),
            }),
        }],
        history: Vec::new(),
    }));

    let out = strip(|w| {
        let mut tally = Tally::default();
        render_mux(w, &Probe::Ready(mux), &mut tally)?;
        render_tally(w, &tally)
    });

    assert!(out.contains("failures 1/0"), "{out}");
    assert!(out.contains("! 1 warning"), "{out}");
}

#[test]
fn mux_section_renders_presence_plugin_listing_unavailable() {
    let mut mux = mux_fixture();
    mux.presence_plugins = Some(Probe::Unavailable {
        error: "list-panes failed".to_owned(),
    });

    let out = strip(|w| {
        let mut tally = Tally::default();
        render_mux(w, &Probe::Ready(mux), &mut tally)?;
        render_tally(w, &tally)
    });

    assert!(
        out.contains("presence plugins") && out.contains("unavailable (list-panes failed)"),
        "{out}"
    );
    assert!(out.contains("! 1 warning"), "{out}");
}

#[test]
fn mux_section_warns_when_no_presence_plugin_is_loaded() {
    let mut mux = mux_fixture();
    mux.presence_plugins = Some(Probe::Ready(PresencePlugins {
        desired_build: Some("desired-build".to_owned()),
        rows: Vec::new(),
        history: vec!["/tmp/plugin-presence.log.jsonl".to_owned()],
    }));

    let out = strip(|w| {
        let mut tally = Tally::default();
        render_mux(w, &Probe::Ready(mux), &mut tally)?;
        render_tally(w, &tally)
    });

    for expected in [
        "0 loaded",
        "none loaded",
        "rimz reload",
        "history",
        "! 1 warning",
    ] {
        assert!(out.contains(expected), "missing {expected}:\n{out}");
    }
}

#[test]
fn mux_section_tallies_poll_presence_by_expectedness() {
    let out = strip(|w| {
        let mut tally = Tally::default();
        let mut mux = mux_fixture();
        mux.presence = Some(Presence::Poll {
            reason: "no sidebar running in this workspace".to_owned(),
            expected: true,
        });
        render_mux(w, &Probe::Ready(mux), &mut tally)?;
        render_tally(w, &tally)
    });
    assert!(out.contains("✓ polling — no sidebar running"), "{out}");
    assert!(out.contains("✓ no problems found"), "{out}");

    let out = strip(|w| {
        let mut tally = Tally::default();
        let mut mux = mux_fixture();
        mux.presence = Some(Presence::Poll {
            reason: "sidebar running but the live tmux watch is not attached".to_owned(),
            expected: false,
        });
        render_mux(w, &Probe::Ready(mux), &mut tally)?;
        render_tally(w, &tally)
    });
    assert!(
        out.contains("! polling — sidebar running but the live tmux watch is not attached"),
        "{out}"
    );
    assert!(out.contains("! 1 warning"), "{out}");
}

#[test]
fn mux_section_renders_room_ownership_and_neutral_inapplicable_presence() {
    let out = strip(|w| {
        let mut tally = Tally::default();
        let mut mux = mux_fixture();
        mux.name = MuxName::Zellij;
        mux.room = Some(Room {
            session_name: "rimz-test".to_owned(),
            selected_state: RoomState::Live,
            live_on: vec![MuxName::Zellij],
            conflict: false,
            zellij: RoomState::Live,
            tmux: RoomState::Absent,
        });
        render_mux(w, &Probe::Ready(mux), &mut tally)?;
        render_tally(w, &tally)
    });
    assert!(
        out.lines()
            .any(|line| line.trim_start().starts_with("room:") && line.ends_with("✓ live")),
        "{out}"
    );
    assert!(!out.contains("tmux absent"), "{out}");
    assert!(out.contains("✓ no problems found"), "{out}");

    let out = strip(|w| {
        let mut tally = Tally::default();
        let mut mux = mux_fixture();
        mux.room = Some(Room {
            session_name: "rimz-test".to_owned(),
            selected_state: RoomState::Absent,
            live_on: vec![MuxName::Zellij],
            conflict: false,
            zellij: RoomState::Live,
            tmux: RoomState::Absent,
        });
        mux.presence = Some(Presence::NotApplicable {
            reason: "this workspace room is live on zellij, not tmux".to_owned(),
        });
        render_mux(w, &Probe::Ready(mux), &mut tally)?;
        render_tally(w, &tally)
    });
    assert!(out.contains("absent here; live on zellij"), "{out}");
    assert!(out.contains("not applicable"), "{out}");
    assert!(out.contains("✓ no problems found"), "{out}");

    let out = strip(|w| {
        let mut tally = Tally::default();
        let mut mux = mux_fixture();
        mux.room = Some(Room {
            session_name: "rimz-test".to_owned(),
            selected_state: RoomState::Live,
            live_on: vec![MuxName::Zellij, MuxName::Tmux],
            conflict: true,
            zellij: RoomState::Live,
            tmux: RoomState::Live,
        });
        render_mux(w, &Probe::Ready(mux), &mut tally)?;
        render_tally(w, &tally)
    });
    assert!(out.contains("room ownership conflict"), "{out}");
    assert!(out.contains("✗ 1 problem"), "{out}");
}

#[test]
fn loop_section_reads_empty_when_unconfigured() {
    let out = strip(|w| {
        let mut tally = Tally::default();
        render_loop(w, &LoopTasks { tasks: Vec::new() }, &mut tally)
    });
    assert!(out.contains("LOOP TASKS"), "{out}");
    assert!(out.contains("none configured"), "{out}");
}

#[test]
fn storage_section_renders_total_and_roots() {
    let disk_usage = Storage {
        total_bytes: 13_018,
        roots: vec![
            StorageRootView {
                label: "state",
                path: "/home/you/.local/state/rimz".to_owned(),
                bytes: 13_018,
                present: true,
            },
            StorageRootView {
                label: "runtime",
                path: "/run/user/1000/rimz".to_owned(),
                bytes: 0,
                present: false,
            },
        ],
    };
    let out = strip(|w| {
        let mut tally = Tally::default();
        render_storage(w, &disk_usage, &mut tally)
    });
    assert!(out.contains("STORAGE"), "section title:\n{out}");
    assert!(out.contains("rimz on disk: 13 KB"), "total:\n{out}");
    assert!(
        out.contains("state") && out.contains("13 KB") && out.contains(".local/state/rimz"),
        "present root row:\n{out}"
    );
    assert!(
        out.contains("runtime") && out.contains("-") && out.contains("/run/user/1000/rimz"),
        "absent root row:\n{out}"
    );
}

#[test]
fn messages_section_renders_stuck_and_failure_rows() {
    let mut report = report_fixture();
    report.messages = Some(Probe::Ready(Messages {
        open: OpenCounts {
            queued: 2,
            claimed: 0,
            sent: 1,
        },
        stuck: vec![MessageProblemRow {
            message_id: "msg_stuck".to_owned(),
            status: "queued".to_owned(),
            target: "@coder".to_owned(),
            at: Timestamp::UNIX_EPOCH,
            problem: "attempts 3, pane rejected".to_owned(),
        }],
        recent_failures: vec![MessageProblemRow {
            message_id: "msg_failed".to_owned(),
            status: "errored".to_owned(),
            target: "codex:sess-1".to_owned(),
            at: Timestamp::UNIX_EPOCH,
            problem: "pane rejected input".to_owned(),
        }],
    }));
    let out = strip(|w| {
        let mut tally = Tally::default();
        render_messages(w, &report, &mut tally)?;
        render_tally(w, &tally)
    });
    assert!(out.contains("MESSAGES"), "{out}");
    assert!(out.contains("3 open: 2 queued, 1 sent"), "{out}");
    assert!(out.contains("msg_stuck") && out.contains("@coder"), "{out}");
    assert!(
        out.contains("msg_failed") && out.contains("pane rejected input"),
        "{out}"
    );
    assert!(
        out.contains("✗ 1 problem, ! 1 warning"),
        "mixed verdict counts message rows:\n{out}"
    );
}

#[test]
fn messages_section_renders_empty_state() {
    let mut report = report_fixture();
    report.messages = Some(Probe::Ready(Messages {
        open: OpenCounts::default(),
        stuck: Vec::new(),
        recent_failures: Vec::new(),
    }));
    let out = strip(|w| {
        let mut tally = Tally::default();
        render_messages(w, &report, &mut tally)
    });
    assert!(out.contains("MESSAGES"), "{out}");
    assert!(out.contains("no open messages"), "{out}");
}

#[test]
fn diagnostics_section_renders_history_watermark() {
    let mut report = report_fixture();
    report.history_cleared_at = Some(Timestamp::now());
    report.diagnostics = Some(Diagnostics::Ready {
        path: "/tmp/diagnostics".to_owned(),
        incidents: Vec::new(),
    });

    let out = strip(|w| {
        let mut tally = Tally::default();
        render_diagnostics(w, &report, &mut tally)
    });

    assert!(out.contains("history cleared 0s ago"), "{out}");
}

#[test]
fn last_incident_section_renders_cause_agents_and_forensics() {
    let mut report = report_fixture();
    report.last_incident = Some(LastIncident {
        cause: "crash",
        at: Timestamp::UNIX_EPOCH,
        lost_agents: vec![
            IncidentAgent {
                kind: "claude".to_owned(),
                name: Some("lucid-atlas".to_owned()),
                agent_id: "sess-a".to_owned(),
            },
            IncidentAgent {
                kind: "codex".to_owned(),
                name: Some("quiet-comet".to_owned()),
                agent_id: "sess-b".to_owned(),
            },
        ],
        recovered: Some(2),
        forensics: Some("/tmp/rimz/crashes/20260709T082717Z".to_owned()),
    });

    let out = strip(|w| {
        let mut tally = Tally::default();
        render_last_incident(w, &report, &mut tally)?;
        render_tally(w, &tally)
    });

    assert!(out.contains("LAST INCIDENT"), "{out}");
    assert!(out.contains("crash"), "{out}");
    assert!(
        out.contains("lucid-atlas") && out.contains("quiet-comet"),
        "{out}"
    );
    assert!(out.contains("recovered: 2 of 2"), "{out}");
    assert!(out.contains("forensics:"), "{out}");
    assert!(
        out.contains("✓ no problems found"),
        "info incident does not affect tally:\n{out}"
    );
}

#[test]
fn last_incident_section_is_absent_without_marker() {
    let report = report_fixture();
    let out = strip(|w| {
        let mut tally = Tally::default();
        render_last_incident(w, &report, &mut tally)
    });

    assert!(!out.contains("LAST INCIDENT"), "{out}");
}

#[test]
fn tally_renders_clean_verdict() {
    let out = strip(|w| render_tally(w, &Tally::default()));
    assert!(out.contains("✓ no problems found"), "{out}");
}

#[test]
fn remote_agent_fields_render_as_key_and_verdict() {
    let out = strip(|w| {
        let mut tally = Tally::default();
        render_remote_control(
            w,
            &RemoteControl::On {
                agents: vec![RemoteAgent {
                    kind: "claude",
                    detail: "enabled, blocked".to_owned(),
                    ready: false,
                }],
                refusals: vec!["disableRemoteControl: true".to_owned()],
                skipped: vec!["managed standalone Codex install is missing".to_owned()],
                advisories: vec![
                    "Codex remote-control updater version skew:\n    codex app-server daemon bootstrap --remote-control"
                        .to_owned(),
                ],
            },
            &mut tally,
        )
    });
    assert!(out.contains("claude"), "{out}");
    assert!(out.contains("enabled, blocked"), "{out}");
    assert!(out.contains("`rimz start` refuses"), "{out}");
    assert!(out.contains("disableRemoteControl: true"), "{out}");
    assert!(
        out.contains("enabled but not installed")
            && out.contains("skipped (the room still starts)"),
        "{out}"
    );
    assert!(
        out.contains("managed standalone Codex install is missing"),
        "{out}"
    );
    assert!(
        out.contains("provider daemon advisory (no start impact)")
            && out.contains("Codex remote-control updater version skew")
            && out.contains("bootstrap --remote-control"),
        "{out}"
    );
}
