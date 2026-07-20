use super::*;
use crate::cli::doctor::model::{
    HookRow, Host, IncidentAgent, LastIncident, LegacySession, LoopTaskRow, MessageProblemRow,
    MuxBinaries, MuxLogIssue, OpenCounts, PresenceCommandFailure, PresencePluginRow,
    PresencePluginStatus, PresencePluginTelemetry, PresencePlugins, RemoteAgent, StorageRootView,
    TmuxCaps,
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
        socket: Some("/run/user/1001/rimz/tmux/server".to_owned()),
        legacy_session: None,
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
        "1 reporting to RimZ: claude",
        "✗",
        "not installed",
        "rimz hooks install codex",
        "not found on this machine: grok, kiro",
        "RimZ offers their hooks as soon as one appears",
        "✗ 1 problem in HOOKS",
    ] {
        assert!(out.contains(expected), "missing {expected}:\n{out}");
    }
    assert!(!out.contains("unsupported"), "{out}");
    assert!(
        !out.lines()
            .any(|line| line.contains("claude") && line.contains('│')),
        "a working agent never spends a table row:\n{out}"
    );
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
    // The RimZ-owned endpoint, not the user's default server.
    assert!(out.contains("/run/user/1001/rimz/tmux/server"), "{out}");
}

#[test]
fn mux_section_names_a_stranded_legacy_session_with_its_fix() {
    let mux = Mux {
        legacy_session: Some(LegacySession {
            session: "rimz-project-a1b2c3".to_owned(),
            socket: "/tmp/tmux-1001/default".to_owned(),
            fix: "tmux -S /tmp/tmux-1001/default kill-session -t rimz-project-a1b2c3".to_owned(),
        }),
        ..mux_fixture()
    };
    let out = strip(|w| {
        let mut tally = Tally::default();
        render_mux(w, &Probe::Ready(mux), &mut tally)
    });

    assert!(out.contains("legacy session"), "{out}");
    // Session-scoped: the user's other sessions on that server survive.
    assert!(out.contains("kill-session -t rimz-project-a1b2c3"), "{out}");
    assert!(!out.contains("kill-server"), "{out}");
}

fn log_issue(
    summary: &str,
    state: DoctorState,
    impact: DoctorImpact,
    occurrences: usize,
) -> MuxLogIssue {
    MuxLogIssue {
        source_severity: "error".to_owned(),
        state,
        impact,
        summary: summary.to_owned(),
        occurrences,
        first_occurrence: None,
        last_occurrence: None,
        samples: vec!["ERROR raw record text".to_owned()],
        evidence_truncated: false,
    }
}

/// A span is worth naming only when it covers real time. A burst inside one
/// second reads as a broken clock if it is reported as "over 0s".
#[test]
fn issue_span_names_a_real_span_and_drops_a_sub_second_one() {
    let now = jiff::Timestamp::now();
    let spanned = |first_ms: i64, last_ms: i64| {
        let mut issue = log_issue("boom", DoctorState::Investigate, DoctorImpact::Warn, 4);
        issue.first_occurrence = Some(now - jiff::SignedDuration::from_millis(first_ms));
        issue.last_occurrence = Some(now - jiff::SignedDuration::from_millis(last_ms));
        issue_span(&issue)
    };

    // Four hits spread over two minutes: the span is the finding.
    assert!(
        spanned(125_000, 5_000).starts_with("4× over 2m, last "),
        "{}",
        spanned(125_000, 5_000)
    );
    // Four hits inside one second: no span worth printing, just the recency.
    let burst = spanned(3_600_400, 3_600_000);
    assert!(!burst.contains("over"), "sub-second span is named: {burst}");
    assert!(burst.starts_with("4×, "), "{burst}");
}

#[test]
fn mux_log_spends_lines_on_issues_and_counts_the_lifecycle_noise() {
    let mux = Mux {
        log: MuxLog::Ready {
            path: "/tmp/zellij-log/zellij.log".to_owned(),
            scope: LogScope::HostUser { uid: 1001 },
            size_bytes: 3_500_000,
            scanned_bytes: 262_144,
            logical_records: 900,
            records_before_cutoff: 812,
            since: Some(Timestamp::now() - std::time::Duration::from_secs(300)),
            problem_records: 1_190,
            omitted_issue_groups: 0,
            issues: vec![
                log_issue(
                    "a client left the session",
                    DoctorState::Expected,
                    DoctorImpact::Info,
                    1_000,
                ),
                log_issue(
                    "zellij acknowledged CliPipe late (the action still ran)",
                    DoctorState::Expected,
                    DoctorImpact::Info,
                    180,
                ),
                log_issue(
                    "plugin pane queries timed out",
                    DoctorState::Investigate,
                    DoctorImpact::Warn,
                    10,
                ),
            ],
        },
        ..mux_fixture()
    };
    let out = strip(|w| {
        let mut tally = Tally::default();
        render_mux(w, &Probe::Ready(mux), &mut tally)
    });

    assert!(
        out.contains("plugin pane queries timed out (10×)"),
        "an open issue leads with what it means and how often:\n{out}"
    );
    assert!(
        out.contains("1180 records are routine room lifecycle"),
        "expected records are counted, never listed one line each:\n{out}"
    );
    assert!(
        out.lines()
            .filter(|line| line.contains("a client left the session"))
            .count()
            == 1,
        "the fold names an expected group once:\n{out}"
    );
    assert!(
        !out.contains("ERROR raw record text"),
        "raw evidence is for alarms alone:\n{out}"
    );
    assert!(
        out.contains("read last 256 KB of 3.3 MB") && out.contains("812 older records dismissed"),
        "the log line says how far back the verdict reaches:\n{out}"
    );
}

#[test]
fn mux_log_carries_raw_evidence_for_an_alarm() {
    let mux = Mux {
        log: MuxLog::Ready {
            path: "/tmp/zellij-log/zellij.log".to_owned(),
            scope: LogScope::Server,
            size_bytes: 1_000,
            scanned_bytes: 1_000,
            logical_records: 4,
            records_before_cutoff: 0,
            since: None,
            problem_records: 1,
            omitted_issue_groups: 0,
            issues: vec![log_issue(
                "Panic occurred",
                DoctorState::Investigate,
                DoctorImpact::Alarm,
                1,
            )],
        },
        ..mux_fixture()
    };
    let out = strip(|w| {
        let mut tally = Tally::default();
        render_mux(w, &Probe::Ready(mux), &mut tally)
    });

    assert!(out.contains("Panic occurred (once)"), "{out}");
    assert!(
        out.contains("ERROR raw record text"),
        "an alarm keeps the record a reader needs to act on it:\n{out}"
    );
}

fn presence_telemetry_fixture() -> PresencePluginTelemetry {
    PresencePluginTelemetry {
        sample_count: 17,
        first_at_ms: 1_000,
        last_at_ms: 481_000,
        last_seen_age_secs: 3,
        zellij_version: Some("0.44.3".to_owned()),
        page_growth: 5,
        byte_growth: 327_680,
        commands_completed_delta: 142,
        commands_succeeded_delta: Some(142),
        stale_writer_rejections_delta: Some(0),
        topology_failures_delta: Some(0),
        other_failures_delta: Some(0),
        last_failure: None,
    }
}

fn presence_plugins_fixture(rows: Vec<PresencePluginRow>) -> Probe<PresencePlugins> {
    Probe::Ready(PresencePlugins {
        desired_build: Some("desired-build".to_owned()),
        rows,
        history: vec!["/tmp/plugin-presence.log.jsonl".to_owned()],
    })
}

/// The common case is one current plugin doing its job: name the job, and keep
/// the raw counters off the human report.
#[test]
fn mux_section_states_what_a_healthy_presence_plugin_does() {
    let mut mux = mux_fixture();
    mux.version = Version::Reported {
        version: "zellij 0.44.3".to_owned(),
    };
    mux.presence_plugins = presence_plugins_fixture(vec![PresencePluginRow {
        plugin_id: 80,
        loaded_at_ms: Some(1_000),
        build: Some("desired-build".to_owned()),
        status: PresencePluginStatus::Active,
        rejected_count: None,
        outdated: false,
        telemetry: Some(presence_telemetry_fixture()),
    }])
    .into();

    let out = strip(|w| {
        let mut tally = Tally::default();
        render_mux(w, &Probe::Ready(mux), &mut tally)?;
        render_tally(w, &tally)
    });

    for expected in [
        "presence plugin",
        "writing pane topology",
        "build desired-, current",
        "last report 3s ago",
        "plugin #80",
        "last 8m",
        "142 commands",
        "all applied",
        "memory +320 KB",
        "telemetry log",
    ] {
        assert!(out.contains(expected), "missing {expected}:\n{out}");
    }
    for absent in ["pages", "succeeded", "zellij 0.44.3 ·", "warning"] {
        assert!(!out.contains(absent), "unexpected {absent}:\n{out}");
    }
}

fn failing_presence_plugin(telemetry: PresencePluginTelemetry) -> Probe<PresencePlugins> {
    presence_plugins_fixture(vec![PresencePluginRow {
        plugin_id: 80,
        loaded_at_ms: Some(1_000),
        build: Some("desired-build".to_owned()),
        status: PresencePluginStatus::Active,
        rejected_count: None,
        outdated: false,
        telemetry: Some(telemetry),
    }])
}

/// A plugin that reported no cause is all the doctor had before the wake
/// carried evidence, so the generic remedy still stands in for one.
#[test]
fn mux_section_falls_back_to_the_generic_remedy_without_failure_evidence() {
    let mut mux = mux_fixture();
    mux.presence_plugins = failing_presence_plugin(PresencePluginTelemetry {
        commands_succeeded_delta: Some(125),
        topology_failures_delta: Some(17),
        ..presence_telemetry_fixture()
    })
    .into();

    let out = strip(|w| {
        let mut tally = Tally::default();
        render_mux(w, &Probe::Ready(mux), &mut tally)?;
        render_tally(w, &tally)
    });

    for expected in [
        "wakes are failing",
        "pane discovery lags",
        "rimz reload",
        "17 failed to apply topology",
        "! 1 warning",
    ] {
        assert!(out.contains(expected), "missing {expected}:\n{out}");
    }
    // The traffic line owns every count; the verdict owns the cause.
    assert!(
        !out.contains("17 of 142"),
        "the verdict restates the traffic line's count:\n{out}"
    );
}

/// Once the plugin reports what the host said, the verdict names the cause
/// instead of prescribing a reload that would not fix it.
#[test]
fn mux_section_names_the_cause_a_failing_wake_reported() {
    let mut mux = mux_fixture();
    mux.presence_plugins = failing_presence_plugin(PresencePluginTelemetry {
        commands_succeeded_delta: Some(125),
        topology_failures_delta: Some(17),
        last_failure: Some(PresenceCommandFailure {
            exit_code: Some(1),
            detail: "Error: could not serialize topology writer selection: lock timeout".to_owned(),
            at_ms: Some(1_000),
        }),
        ..presence_telemetry_fixture()
    })
    .into();

    let out = strip(|w| {
        let mut tally = Tally::default();
        render_mux(w, &Probe::Ready(mux), &mut tally)?;
        render_tally(w, &tally)
    });

    assert!(
        out.contains("could not serialize topology writer selection: lock timeout"),
        "the verdict names the reported cause:\n{out}"
    );
    assert!(
        !out.contains("rimz reload"),
        "a named cause replaces the reload that would not fix it:\n{out}"
    );
}

/// A wake killed before it could speak still leaves its exit status behind.
#[test]
fn mux_section_reports_a_silent_failure_by_its_exit_status() {
    let mut mux = mux_fixture();
    mux.presence_plugins = failing_presence_plugin(PresencePluginTelemetry {
        other_failures_delta: Some(3),
        last_failure: Some(PresenceCommandFailure {
            exit_code: None,
            detail: String::new(),
            at_ms: Some(1_000),
        }),
        ..presence_telemetry_fixture()
    })
    .into();

    let out = strip(|w| {
        let mut tally = Tally::default();
        render_mux(w, &Probe::Ready(mux), &mut tally)?;
        render_tally(w, &tally)
    });

    assert!(
        out.contains("killed before it could report"),
        "a silent failure still reports how it died:\n{out}"
    );
    assert!(
        out.contains("3 other failures"),
        "the traffic line still owns the count:\n{out}"
    );
}

/// A plugin loaded under a different Zellij than the running server is the one
/// case worth repeating the version for.
#[test]
fn mux_section_names_a_presence_plugin_loaded_under_another_zellij() {
    let mut mux = mux_fixture();
    mux.version = Version::Reported {
        version: "zellij 0.44.3".to_owned(),
    };
    mux.presence_plugins = presence_plugins_fixture(vec![PresencePluginRow {
        plugin_id: 80,
        loaded_at_ms: Some(1_000),
        build: Some("desired-build".to_owned()),
        status: PresencePluginStatus::Active,
        rejected_count: None,
        outdated: false,
        telemetry: Some(PresencePluginTelemetry {
            zellij_version: Some("0.43.1".to_owned()),
            ..presence_telemetry_fixture()
        }),
    }])
    .into();

    let out = strip(|w| {
        let mut tally = Tally::default();
        render_mux(w, &Probe::Ready(mux), &mut tally)
    });

    assert!(out.contains("loaded under zellij 0.43.1"), "{out}");
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
            first_at_ms: loaded_at_ms,
            last_at_ms: loaded_at_ms,
            ..presence_telemetry_fixture()
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
        "2 loaded",
        "only one may write pane topology",
        "plugin #49",
        "loaded 00:00:01",
        "build desired-, current",
        "writing pane topology",
        "plugin #41",
        "a newer plugin took over",
        "3 of its topology writes were ignored",
        "rimz reload",
        "telemetry log",
        "/tmp/plugin-presence.log.jsonl (+ rotated .1)",
        "! 2 warnings",
    ] {
        assert!(out.contains(expected), "missing {expected}:\n{out}");
    }
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
        out.contains("presence plugin") && out.contains("unavailable (list-panes failed)"),
        "{out}"
    );
    assert!(out.contains("! 1 warning"), "{out}");
}

#[test]
fn mux_section_warns_when_no_presence_plugin_is_loaded() {
    let mut mux = mux_fixture();
    mux.presence_plugins = presence_plugins_fixture(Vec::new()).into();

    let out = strip(|w| {
        let mut tally = Tally::default();
        render_mux(w, &Probe::Ready(mux), &mut tally)?;
        render_tally(w, &tally)
    });

    for expected in [
        "none loaded",
        "want build desired-",
        "the sidebar cannot see panes",
        "rimz reload",
        "telemetry log",
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
    assert!(out.contains("✓ everything checked is healthy"), "{out}");

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
    assert!(out.contains("✓ everything checked is healthy"), "{out}");

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
    assert!(out.contains("✓ everything checked is healthy"), "{out}");

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
        out.contains("✗ 1 problem in MESSAGES") && out.contains("! 1 warning in MESSAGES"),
        "a mixed verdict counts message rows and names where they are:\n{out}"
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
        out.contains("✓ everything checked is healthy"),
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
    assert!(out.contains("✓ everything checked is healthy"), "{out}");
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
