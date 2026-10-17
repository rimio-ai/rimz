use super::*;
use crate::cli::doctor::model::{
    AccountRow, HookRow, Host, IncidentAgent, LastIncident, LegacySession, LoopTaskRow, LoopTimer,
    MachineConfigProblem, MachineConfigProblemKind, MessageProblemRow, MuxBinaries, MuxLogIssue,
    OpenCounts, PresenceCommandFailure, PresencePluginRow, PresencePluginStatus,
    PresencePluginTelemetry, PresencePlugins, RemoteAgent, StorageRootView, TmuxCaps, ZellijCaps,
    ZellijKittyGraphics,
};
use rimz::ids::MuxName;

fn strip(render_one: impl FnOnce(&mut anstream::StripStream<Vec<u8>>) -> io::Result<()>) -> String {
    let mut stream = anstream::StripStream::new(Vec::new());
    render_one(&mut stream).expect("render to in-memory buffer");
    String::from_utf8(stream.into_inner()).expect("utf-8")
}

#[test]
fn hibernate_reports_active_or_skipped() {
    for skipped in [None, Some("swap is absent".to_owned())] {
        let report = rimz::harness::hibernate::PreflightReport {
            enabled: true,
            skipped: skipped.clone(),
            checks: Vec::new(),
        };
        let text = strip(|w| render_hibernate(w, &report, &mut Tally::default()));
        assert!(text.contains("active"), "{text}");
        assert!(
            text.contains(
                &skipped.map_or_else(|| "yes".into(), |reason| format!("skipped: {reason}"))
            ),
            "{text}"
        );
    }
    let report = rimz::harness::hibernate::PreflightReport {
        enabled: true,
        skipped: Some("swap is absent; add a swapfile".into()),
        checks: vec![rimz::harness::hibernate::Check {
            name: "swap",
            summary: None,
            fix: None,
            status: rimz::harness::hibernate::CheckStatus::Warning,
            detail: "swap is absent; add a swapfile".into(),
        }],
    };
    let mut tally = Tally::default();
    let body = strip(|w| render_hibernate(w, &report, &mut tally));
    assert!(body.contains("swap is absent; add a swapfile"), "{body}");
    let closing = strip(|w| render_tally(w, &tally));
    assert!(
        closing.contains("fix: repair the reported hibernation configuration"),
        "{closing}"
    );
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

#[test]
fn startup_checks_count_failures_but_only_warn_for_timeouts() {
    use rimz::lsp::broker::probe::{Check, Outcome};
    let cause = rimz::lsp::registry::CrashCause {
        at_ms: 0,
        exit_code: Some(1),
        signal: None,
        stderr_tail: "hidden\none\ntwo\nthree\nfour\nfive\n".into(),
        error: None,
    };
    let lsp = Probe::Ready(super::super::model::Lsp {
        servers: [
            (
                "crashed",
                serde_json::json!({"dormant": {"reason": "crashed", "since_ms": 0}}),
            ),
            ("running", serde_json::json!("ready")),
        ]
        .into_iter()
        .map(|(name, state)| super::super::model::LspServer {
            entry: serde_json::from_value(serde_json::json!({
                "root": "/checkout", "server": name, "nonce": "nonce",
                "broker_pid": 1, "broker_start_token": "token",
                "state": state, "started_at_ms": 0, "estimate_bytes": 0,
                "settings_hash": "hash", "request_count": 0, "peak_rss_kb": 0,
                "leases": [], "last_crash": cause.clone()
            }))
            .unwrap(),
            rss_bytes: 1024,
        })
        .collect(),
        last_refusal: Some(rimz::diag::lsp::Record {
            at: "2026-01-01T00:00:00Z".parse().unwrap(),
            root: "/other".into(),
            server: "rust".into(),
            event: "queue_timeout".into(),
            details: serde_json::json!({}),
        }),
        checks_error: Some("bad project config".into()),
        checks: vec![
            Check {
                server: "failed".into(),
                root: "/checkout".into(),
                outcome: Outcome::Failed {
                    cause: cause.clone(),
                    fix: "repair".into(),
                },
            },
            Check {
                server: "slow".into(),
                root: "/checkout".into(),
                outcome: Outcome::TimedOut {
                    cause,
                    fix: "wait".into(),
                },
            },
            Check {
                server: "bad".into(),
                root: "/checkout".into(),
                outcome: Outcome::Invalid {
                    error: "empty command".into(),
                    fix: "configure".into(),
                },
            },
            Check {
                server: "project".into(),
                root: "/checkout".into(),
                outcome: Outcome::Untrusted {
                    fix: "run rimz trust".into(),
                },
            },
            Check {
                server: "healthy".into(),
                root: "/checkout".into(),
                outcome: Outcome::Started {
                    version: Some("1.2".into()),
                },
            },
            Check {
                server: "live".into(),
                root: "/checkout".into(),
                outcome: Outcome::Running {
                    state: rimz::lsp::registry::State::Ready,
                },
            },
        ],
    });
    let mut tally = Tally::default();
    let rendered = strip(|w| render_lsp(w, &lsp, &mut tally));
    assert!(!rendered.contains("fix:"), "{rendered}");
    assert_eq!(tally.alarms.len(), 5, "{rendered}");
    assert_eq!(tally.warns.len(), 2, "{rendered}");
    assert!(rendered.contains("failed cannot start in /checkout: exit code 1"));
    assert!(rendered.contains("slow did not answer initialize within 10s"));
    assert!(rendered.contains("healthy starts (1.2)"));
    assert!(rendered.contains("live running"));
    let closing = strip(|w| render_tally(w, &tally));
    assert!(closing.contains("fix: repair"), "{closing}");
    assert!(closing.contains("rimz trust grant"), "{closing}");
    assert!(!rendered.contains("hidden"));
    assert!(rendered.contains("cannot check this checkout's language servers: bad project config"));
}

#[test]
fn shared_language_servers_show_dormant_reason_and_last_refusal() {
    let entry = serde_json::from_value(serde_json::json!({
        "root": "/checkout", "server": "rust", "nonce": "nonce", "broker_pid": 1, "broker_start_token": "token",
        "server_pid": null, "server_start_token": null,
        "state": {"dormant": {"reason": "memory pressure", "since_ms": 1}},
        "started_at_ms": 0, "ready_at_ms": null, "estimate_bytes": 0, "settings_hash": "hash",
        "request_count": 0, "last_request_at_ms": null, "peak_rss_kb": 0, "leases": []
    })).unwrap();
    let lsp = Probe::Ready(super::super::model::Lsp {
        checks: vec![],
        checks_error: None,
        servers: vec![super::super::model::LspServer {
            entry,
            rss_bytes: 0,
        }],
        last_refusal: Some(rimz::diag::lsp::Record {
            at: "2026-01-01T00:00:00Z".parse().unwrap(),
            root: "/other".into(),
            server: "rust".into(),
            event: "queue_timeout".into(),
            details: serde_json::json!({"reason": "waiting for available memory"}),
        }),
    });
    let rendered = strip(|w| render_lsp(w, &lsp, &mut Tally::default()));
    assert!(
        rendered.contains("1 more dormant or not started — rimz lsp list --all"),
        "{rendered}"
    );
    assert!(!rendered.contains("dormant: memory pressure"), "{rendered}");
    assert!(
        rendered.contains("/other rust timed out in queue")
            && rendered.contains("ago: waiting for available memory"),
        "{rendered}"
    );
    let mut tally = Tally {
        audit: true,
        ..Tally::default()
    };
    let audit = strip(|w| render_lsp(w, &lsp, &mut tally));
    assert!(audit.contains("· dormant: memory pressure"), "{audit}");
    assert!(tally.alarms.is_empty() && tally.warns.len() == 1);
    let Probe::Ready(mut lsp) = lsp else {
        unreachable!()
    };
    let refusal = lsp.last_refusal.as_mut().unwrap();
    refusal.event = "refused".into();
    refusal.details = serde_json::json!({"estimate_bytes": 8_000_000_000_u64, "available_bytes": 3_000_000_000_u64, "reserve_bytes": 1_000_000_000_u64, "committed_bytes": 9_000_000_000_u64, "holders": [{"root": "/holder", "server": "rust", "rss_bytes": 4_000_000_000_u64}]});
    refusal.details["root"] = serde_json::json!("/other");
    refusal.details["server"] = serde_json::json!("rust");
    let readable = strip(|w| render_lsp(w, &Probe::Ready(lsp), &mut Tally::default()));
    assert!(
        readable.contains("needs 8 GB, 3 GB available, 1 GB reserved; held by /holder rust (4 GB)"),
        "{readable}"
    );
    assert!(!readable.contains("committed_bytes"), "{readable}");
}

#[test]
fn shared_language_servers_read_like_lsp_list() {
    use rimz::lsp::registry::{State, StopReason};
    let server = |root: &str, name: &str, state| super::super::model::LspServer {
        entry: rimz::lsp::registry::Entry {
            kind: None,
            editor_check_on_save: None,
            root: root.into(),
            project: None,
            server: name.into(),
            nonce: String::new(),
            broker_pid: 1,
            broker_start_token: String::new(),
            server_pid: Some(2),
            server_start_token: None,
            state,
            started_at_ms: 0,
            ready_at_ms: None,
            estimate_bytes: 0,
            settings_hash: String::new(),
            request_count: 0,
            last_request_at_ms: None,
            peak_rss_kb: 0,
            restarts: 0,
            last_crash: None,
            leases: vec![],
            attached: vec![],
        },
        rss_bytes: 4_700_000_000,
    };
    let dormant = |reason| State::Dormant {
        since_ms: 0,
        reason,
    };
    let lsp = Probe::Ready(super::super::model::Lsp {
        checks: vec![],
        checks_error: None,
        servers: vec![
            server("/b", "rust", dormant(None)),
            server("/a", "z", dormant(Some(StopReason::Idle))),
            server("/c", "rust", dormant(Some(StopReason::Crashed))),
            server("/z", "rust", State::Ready),
            server("/a", "a", dormant(None)),
        ],
        last_refusal: None,
    });
    let rendered = strip(|w| {
        render_lsp(
            w,
            &lsp,
            &mut Tally {
                audit: true,
                ..Tally::default()
            },
        )
    });
    let rows: Vec<Vec<&str>> = rendered
        .lines()
        .map(|line| line.split_whitespace().collect())
        .filter(|row: &Vec<&str>| !row.is_empty())
        .collect();
    assert_eq!(
        rows,
        vec![
            vec!["LSP"],
            vec!["STATE", "CHECKOUT", "SERVER", "RSS", "LEASES"],
            vec!["✓", "ready", "/z", "rust", "4.7", "GB", "0"],
            vec!["✗", "dormant:", "crashed", "/c", "rust", "-", "0"],
            vec!["·", "not", "started", "/a", "a", "-", "0"],
            vec!["·", "dormant:", "idle", "/a", "z", "-", "0"],
            vec!["·", "not", "started", "/b", "rust", "-", "0"],
        ]
    );
}

#[test]
fn sandbox_probe_failure_only_counts_when_sandbox_is_enabled() {
    for mode in [
        rimz::config::Isolation::Host,
        rimz::config::Isolation::Sandbox,
    ] {
        let sandbox = super::super::model::Sandbox {
            mode,
            path: Some("/usr/bin/bwrap".into()),
            version: Some("bubblewrap 0.11.0".to_owned()),
            error: Some(
                "probe failed; check user namespaces or set agents.isolation to host".to_owned(),
            ),
        };
        let mut tally = Tally::default();
        let out = strip(|w| render_sandbox(w, &sandbox, &mut tally));
        for expected in [
            "SANDBOX",
            "/usr/bin/bwrap",
            "bubblewrap 0.11.0",
            "probe failed",
        ] {
            assert!(out.contains(expected), "missing {expected}: {out}");
        }
        assert!(tally.warns.is_empty());
        assert_eq!(
            tally.alarms.is_empty(),
            mode == rimz::config::Isolation::Host
        );
        assert!(out.contains("probe failed; check user namespaces"), "{out}");
        let closing = strip(|w| render_tally(w, &tally));
        assert_eq!(
            closing.contains("fix: install bubblewrap, or set agents.isolation to host"),
            mode == rimz::config::Isolation::Sandbox,
            "{closing}"
        );
    }
}

#[test]
fn machine_config_section_keeps_the_classified_problem_compact() {
    let config = MachineConfigHealth {
        broken_files: vec![MachineConfigProblem {
            diagnosis: None,
            path: "/home/eddie/.rimz/config.toml".to_owned(),
            error: "line 144: `auto_continue` is defined more than once in the same table"
                .to_owned(),
            kind: MachineConfigProblemKind::Parse,
        }],
    };
    let out = strip(|w| {
        let mut tally = Tally::default();
        render_machine_config(w, &config, &mut tally)?;
        render_tally(w, &tally)
    });

    assert!(
        out.contains("line 144: `auto_continue` is defined more than once"),
        "{out}"
    );
    assert!(!out.contains("TOML parse error"), "{out}");
    assert!(
        out.contains("fix: repair the reported configuration file"),
        "{out}"
    );
    assert!(
        out.contains("settings in this file use built-in defaults"),
        "{out}"
    );
}

#[test]
fn machine_config_definition_problem_names_launch_precondition() {
    let config = MachineConfigHealth {
        broken_files: vec![MachineConfigProblem {
            diagnosis: None,
            path: "/home/eddie/.agents/teams/bad.md".to_owned(),
            error: "empty layout cell in `claude,,codex`".to_owned(),
            kind: MachineConfigProblemKind::Definition,
        }],
    };
    let out = strip(|w| {
        let mut tally = Tally::default();
        render_machine_config(w, &config, &mut tally)?;
        render_tally(w, &tally)
    });

    assert!(out.contains("cannot be used"), "{out}");
    assert!(
        out.contains("cannot be used: empty layout cell in `claude,,codex`"),
        "{out}"
    );
    assert!(
        out.contains("fix: ") && out.contains("`rimz agents validate`"),
        "{out}"
    );
    assert!(
        out.contains("launches selecting these definitions are refused"),
        "{out}"
    );
    assert!(!out.contains("; `rimz agents`, `rimz subagents`"), "{out}");
    assert!(!out.contains("unparseable"), "{out}");
    assert!(!out.contains("built-in defaults"), "{out}");
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

#[test]
fn zellij_graphics_capability_names_each_probe_outcome() {
    let cases = [
        (
            ZellijKittyGraphics::Supported,
            "supported by the host terminal; Zellij rooms still draw cell art (Zellij rejects the placement the pixel tier uses)",
        ),
        (
            ZellijKittyGraphics::Unsupported,
            "unsupported by host terminal",
        ),
        (
            ZellijKittyGraphics::NoReply,
            "graphics disabled or session predates Zellij 0.45",
        ),
        (
            ZellijKittyGraphics::BelowMinimum,
            "requires Zellij >= 0.45.0",
        ),
        (ZellijKittyGraphics::NotProbed, "not probed"),
    ];

    for (kitty_graphics, expected) in cases {
        let out = strip(|w| {
            let mut kv = KeyVals::new();
            let mut tally = Tally::default();
            push_capabilities(
                &mut kv,
                &mut tally,
                &Capabilities::Zellij(Probe::Ready(ZellijCaps {
                    meets_min_version: true,
                    min_version: (0, 44, 0),
                    kitty_graphics,
                })),
            );
            kv.render(w)
        });

        assert!(out.contains("zellij kitty graphics"), "{out}");
        assert!(out.contains(expected), "{out}");
        let graphics = out
            .lines()
            .find(|line| line.contains("zellij kitty graphics"))
            .expect("graphics row");
        assert!(!graphics.contains(parts(Health::Ok).0), "{graphics}");
    }
}

#[test]
fn agents_home_override_names_precedence_and_resolved_path() {
    let mut home = report_fixture().home;
    home.agents_home_override = Some("/resolved/agents-home".to_owned());
    let out = strip(|w| render_home(w, &home, &mut Tally::default()));

    assert!(out.contains("RIMZ_AGENTS_HOME"), "{out}");
    assert!(out.contains("overrides RIMZ_HOME"), "{out}");
    assert!(out.contains("/resolved/agents-home"), "{out}");
}

#[test]
fn zellij_graphics_capability_does_not_degrade_doctor_health() {
    for kitty_graphics in [
        ZellijKittyGraphics::Unsupported,
        ZellijKittyGraphics::NoReply,
        ZellijKittyGraphics::BelowMinimum,
        ZellijKittyGraphics::NotProbed,
    ] {
        let mut kv = KeyVals::new();
        let mut tally = Tally::default();
        push_capabilities(
            &mut kv,
            &mut tally,
            &Capabilities::Zellij(Probe::Ready(ZellijCaps {
                meets_min_version: true,
                min_version: (0, 44, 0),
                kitty_graphics,
            })),
        );

        assert!(tally.warns.is_empty(), "{kitty_graphics:?}");
        assert!(tally.alarms.is_empty(), "{kitty_graphics:?}");
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
        audit: false,
        hibernate: rimz::harness::hibernate::PreflightReport {
            enabled: false,
            skipped: Some("disabled".into()),
            checks: Vec::new(),
        },
        folder_trust: None,
        empty_dir: super::super::model::EmptyDir {
            check_fix: "repair access to the empty root",
            rows: Vec::new(),
            check: Probe::Ready(()),
            profiles_error: None,
        },
        schema: "rimz.doctor.v1",
        lsp: Probe::Ready(super::super::model::Lsp {
            checks: vec![],
            checks_error: None,
            servers: Vec::new(),
            last_refusal: None,
        }),
        version: rimz::build_id::VERSION,
        host: Host {
            user: None,
            uid: 0,
            binary: None,
        },
        workspace: Probe::Unavailable {
            diagnosis: None,
            error: "test".to_owned(),
        },
        mux: Probe::Unavailable {
            diagnosis: None,
            error: "test".to_owned(),
        },
        terminal: terminal_fixture(),
        home: super::super::model::Home {
            path: "/home/u/.rimz".to_owned(),
            from_env: false,
            agents_home_override: None,
            legacy_roots: Vec::new(),
            fix: None,
        },
        machine_config: MachineConfigHealth {
            broken_files: Vec::new(),
        },
        sandbox: super::super::model::Sandbox {
            mode: rimz::config::Isolation::Host,
            path: None,
            version: None,
            error: Some(
                "bwrap not found; install bubblewrap or set agents.isolation to host".to_owned(),
            ),
        },
        hooks: Vec::new(),
        accounts: Probe::Ready(Accounts { rows: Vec::new() }),
        plugins: Vec::new(),
        loop_tasks: LoopTasks {
            tasks: Vec::new(),
            timer: LoopTimer::NotInstalled,
            unscheduled: Vec::new(),
        },
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
fn agents_keep_actionable_rows_counted_and_account_limits_contextual() {
    use super::super::model::AgentRow;
    for (status, attributed, restartable, problems, warnings, fix) in [
        (
            AgentStatus::Failed,
            false,
            true,
            1,
            0,
            "rimz agents restart @fox",
        ),
        (
            AgentStatus::Failed,
            false,
            false,
            1,
            0,
            "start a new codex agent",
        ),
        (AgentStatus::Paused, true, true, 0, 0, ""),
        (AgentStatus::Waiting, false, true, 0, 0, ""),
    ] {
        let report = DoctorReport {
            agents: Some(AgentRollup::Observed {
                counts: AgentCounts::default(),
                rows: vec![AgentRow {
                    kind: "codex".into(),
                    agent_id: "session".into(),
                    branch: Some("main".into()),
                    status,
                    phase: rimz::agents::TurnPhase::default(),
                    last_seen: Timestamp::now(),
                    handle: "@fox".into(),
                    account: "codex@team".into(),
                    turn_error: Some("reason".into()),
                    attributed_to_account: attributed,
                    restartable,
                    historical: false,
                }],
            }),
            ..report_fixture()
        };
        let mut tally = Tally::default();
        let body = strip(|w| render_agents(w, &report, &mut tally));
        assert!(
            body.contains("@fox") && body.contains("codex@team") && body.contains("reason"),
            "{body}"
        );
        assert!(!body.contains("fix:"), "{body}");
        assert_eq!(tally.alarms.len(), problems, "{body}");
        assert_eq!(tally.warns.len(), warnings, "{body}");
        let closing = strip(|w| render_tally(w, &tally));
        assert!(closing.contains(fix), "{closing}");
        if attributed {
            assert!(!closing.contains("fix:"), "{closing}");
        }
    }
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
        render_terminal(w, &terminal, &mut tally)?;
        render_tally(w, &tally)
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
                kind: "amp".to_owned(),
                detected: true,
                status: HookStatus::InstalledDisabled {
                    fix: "enable hooks in the provider settings".into(),
                },
            },
            HookRow {
                kind: "droid".to_owned(),
                detected: true,
                status: HookStatus::InstalledDisabled {
                    fix: "enable hooks in the provider settings".into(),
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
    let (body, closing) = out.split_once("✗ 1 problem in HOOKS").unwrap();
    assert!(
        !body.contains("FIX") && !body.contains("rimz hooks install"),
        "{out}"
    );
    assert!(closing.contains("✗ HOOKS: codex not installed"), "{out}");
    assert!(out.contains("! 2 warnings in HOOKS"), "{out}");
    let grouped = "! HOOKS: 2 findings: amp installed, disabled, droid installed, disabled";
    assert!(closing.contains(grouped), "{out}");
    assert!(closing.find("✗ HOOKS:").unwrap() < closing.find(grouped).unwrap());
    assert_eq!(closing.matches("fix: enable hooks").count(), 1, "{out}");
    assert!(
        closing.contains("      fix: run `rimz hooks install codex`"),
        "{out}"
    );
    assert!(
        !out.lines()
            .any(|line| line.contains("claude") && line.contains('│')),
        "a working agent never spends a table row:\n{out}"
    );
}

#[test]
fn hooks_section_renders_blocked_upgrade_fix() {
    let report = DoctorReport {
        hooks: vec![HookRow {
            kind: "opencode".to_owned(),
            detected: true,
            status: HookStatus::Blocked {
                fix: "run `opencode upgrade --major`".to_owned(),
            },
        }],
        ..report_fixture()
    };
    let out = strip(|w| {
        let mut tally = Tally::default();
        render_hooks(w, &report, &mut tally)?;
        render_tally(w, &tally)
    });
    assert!(out.contains("blocked"), "{out}");
    assert!(out.contains("opencode upgrade --major"), "{out}");
    assert!(out.contains('✗'), "{out}");
}

#[test]
fn accounts_section_alarms_only_for_the_room_account_it_cannot_launch_into() {
    let row = |name: &str, room: bool, problem: Option<&str>| AccountRow {
        kind: "claude".to_owned(),
        name: name.to_owned(),
        home: (name != "default").then(|| format!("/srv/{name}")),
        room,
        machine_default: false,
        default_for: rimz::room::Scopes::default(),
        problem: problem.map(|text| text.to_owned().into()),
        usage: None,
    };
    let accounts = Probe::Ready(Accounts {
        rows: vec![
            row("default", false, None),
            row("travel", true, Some("unknown claude account `travel`")),
            row("work", false, Some("RimZ hooks are missing")),
        ],
    });
    let out = strip(|w| {
        let mut tally = Tally::default();
        render_accounts(w, &accounts, &mut tally)?;
        render_tally(w, &tally)
    });
    for expected in [
        "ACCOUNTS",
        "unknown claude account `travel`",
        "RimZ hooks are missing",
        "/srv/work",
        "1 problem in ACCOUNTS",
    ] {
        assert!(out.contains(expected), "missing {expected}:\n{out}");
    }

    let quiet = strip(|w| {
        render_accounts(
            w,
            &Probe::Ready(Accounts { rows: Vec::new() }),
            &mut Tally::default(),
        )
    });
    assert!(quiet.is_empty(), "no accounts, no section:\n{quiet}");
}

#[test]
fn accounts_section_speaks_the_accounts_list_vocabulary() {
    let mut accounts = Probe::Ready(Accounts {
        rows: vec![AccountRow {
            kind: "codex".to_owned(),
            name: "team".to_owned(),
            home: Some("/srv/team".to_owned()),
            room: true,
            machine_default: true,
            default_for: rimz::room::Scopes::default(),
            problem: None,
            usage: None,
        }],
    });
    let out = strip(|w| render_accounts(w, &accounts, &mut Tally::default()));
    let header = out.lines().find(|line| line.contains("KIND"));
    assert!(
        header.is_some_and(|line| line.ends_with("STATUS  DEFAULT FOR")),
        "{out}"
    );
    let row = out.lines().find(|line| line.contains("team"));
    assert!(row.is_some_and(|line| line.ends_with("ready   -")), "{out}");
    assert!(
        !out.contains("launch default") && !out.contains("yes"),
        "{out}"
    );
    use super::super::model::{AccountStanding, AccountUsage};
    for standing in [
        AccountStanding::Spent,
        AccountStanding::Exhausted,
        AccountStanding::Ready,
    ] {
        let Probe::Ready(rows) = &mut accounts else {
            unreachable!()
        };
        rows.rows[0].usage = Some(AccountUsage {
            read_at: Timestamp::now() - std::time::Duration::from_secs(120),
            standing,
            resets_at: (standing == AccountStanding::Spent)
                .then(|| Timestamp::now() + std::time::Duration::from_secs(3600)),
            agents: if standing == AccountStanding::Ready {
                vec![]
            } else {
                vec!["@fox".into()]
            },
        });
        let mut tally = Tally::default();
        let body = strip(|w| render_accounts(w, &accounts, &mut tally));
        assert!(body.contains("read 2m ago"), "{body}");
        if standing != AccountStanding::Ready {
            assert!(body.contains("1 agent paused or failed"), "{body}");
        }
        assert!(tally.alarms.is_empty(), "{body}");
        assert_eq!(
            tally.warns.len(),
            usize::from(standing == AccountStanding::Exhausted),
            "{body}"
        );
        match standing {
            AccountStanding::Spent => assert!(
                body.contains('▸') && body.contains("spent, resets in") && body.contains("@fox"),
                "{body}"
            ),
            AccountStanding::Exhausted => {
                let closing = strip(|w| render_tally(w, &tally));
                assert!(closing.contains("read 2m ago"), "{closing}");
                assert!(
                    closing.contains("fix: add credits to the account\n"),
                    "{closing}"
                );
                assert!(!closing.contains("move those agents"), "{closing}");
            }
            AccountStanding::Ready => assert!(body.contains("ready, read 2m ago"), "{body}"),
        }
    }
}

#[test]
fn account_error_fixes_only_appear_in_the_closing_block() {
    use rimz::agents::BirthLoginErr;
    let kind = rimz::ids::AgentKind::new_unchecked("codex");
    for error in [
        BirthLoginErr::MachineUnknown {
            kind: kind.clone(),
            name: "work".parse().unwrap(),
            configured: vec![],
            path: "/accounts.toml".into(),
        },
        BirthLoginErr::MissingHome {
            kind: kind.clone(),
            name: "work".parse().unwrap(),
            home: "/missing".into(),
        },
        BirthLoginErr::HooksMissing {
            kind: kind.clone(),
            name: "work".parse().unwrap(),
            home: "/home/work".into(),
        },
    ] {
        let accounts = Probe::Ready(Accounts {
            rows: vec![AccountRow {
                kind: "codex".into(),
                name: "work".into(),
                home: None,
                room: true,
                machine_default: false,
                default_for: Default::default(),
                usage: None,
                problem: Some(error.into()),
            }],
        });
        let mut tally = Tally::default();
        let body = strip(|w| render_accounts(w, &accounts, &mut tally));
        assert!(!body.contains("rimz accounts"), "{body}");
        let closing = strip(|w| render_tally(w, &tally));
        assert!(
            closing.contains("fix: run `rimz accounts add codex work`"),
            "{closing}"
        );
    }
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
                room_open: true,
            },
            LoopTaskRow {
                name: "broken".to_owned(),
                spec: "codex".to_owned(),
                when: "invalid: bad time".to_owned(),
                root: "/home/you/code/other".to_owned(),
                valid: false,
                room_open: false,
            },
        ],
        timer: LoopTimer::NotInstalled,
        unscheduled: Vec::new(),
    };
    let out = strip(|w| {
        let mut tally = Tally::default();
        render_loop(w, &loop_tasks, &mut tally)
    });
    assert!(out.contains("LOOP TASKS"), "section title:\n{out}");
    assert!(out.contains("2 tasks, 1 needs you"), "{out}");
    assert!(!out.contains("morning"), "{out}");
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
        render_mux(w, &Probe::Ready(mux), &mut tally)?;
        render_tally(w, &tally)
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
            log_text_omitted: false,
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
fn mux_log_names_omitted_text_only_when_requested() {
    for log_text_omitted in [false, true] {
        let mux = Mux {
            log: MuxLog::Ready {
                path: "/tmp/zellij.log".to_owned(),
                scope: LogScope::Server,
                size_bytes: 0,
                scanned_bytes: 0,
                logical_records: 0,
                records_before_cutoff: 0,
                since: None,
                problem_records: 0,
                omitted_issue_groups: 0,
                log_text_omitted,
                issues: Vec::new(),
            },
            ..mux_fixture()
        };
        let out = strip(|w| render_mux(w, &Probe::Ready(mux), &mut Tally::default()));
        assert_eq!(
            out.contains("--no-log-text: no log record text in this report"),
            log_text_omitted,
            "{out}"
        );
    }
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
            log_text_omitted: false,
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
        diagnosis: None,
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
            summary: None,
            fix: None,
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
            reason: "sidebar running but the live tmux watch is not attached; reattach or run `rimz reload`".to_owned(),
            expected: false,
            summary: Some("sidebar running but the live tmux watch is not attached"),
            fix: Some("reattach or run `rimz reload`"),
        });
        render_mux(w, &Probe::Ready(mux), &mut tally)?;
        render_tally(w, &tally)
    });
    assert!(
        out.contains("! polling — sidebar running but the live tmux watch is not attached"),
        "{out}"
    );
    assert!(out.contains("! 1 warning"), "{out}");
    let (body, closing) = out.split_at(out.rfind("\n! ").unwrap());
    assert!(!body.contains("rimz reload"), "{body}");
    assert!(
        closing.contains("fix: reattach or run `rimz reload`"),
        "{closing}"
    );
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
        render_loop(
            w,
            &LoopTasks {
                tasks: Vec::new(),
                timer: LoopTimer::NotInstalled,
                unscheduled: Vec::new(),
            },
            &mut tally,
        )
    });
    assert!(out.contains("LOOP TASKS"), "{out}");
    assert!(out.contains("none configured"), "{out}");
}

#[test]
fn loop_section_reports_scheduler_coverage() {
    for (timer, unscheduled, label, warnings) in [
        (
            LoopTimer::NotInstalled,
            vec!["morning".into()],
            "not installed",
            1,
        ),
        (
            LoopTimer::Installed {
                backend: "systemd user",
                active: false,
            },
            vec!["morning".into()],
            "inactive (systemd user)",
            1,
        ),
        (
            LoopTimer::Installed {
                backend: "systemd user",
                active: true,
            },
            vec![],
            "active (systemd user)",
            0,
        ),
        (
            LoopTimer::Unavailable {
                error: "probe failed".into(),
            },
            vec![],
            "unavailable (probe failed)",
            1,
        ),
    ] {
        let tasks = LoopTasks {
            tasks: vec![LoopTaskRow {
                name: "morning".into(),
                spec: "claude".into(),
                when: "every minute".into(),
                root: "/project".into(),
                valid: true,
                room_open: false,
            }],
            timer,
            unscheduled,
        };
        let mut tally = Tally::default();
        let out = strip(|w| {
            render_loop(w, &tasks, &mut tally)?;
            render_tally(w, &tally)
        });
        assert!(out.contains("loop timer") && out.contains(label), "{out}");
        assert_eq!(tally.warns.len(), warnings, "{out}");
        if !tasks.unscheduled.is_empty() {
            for text in [
                "morning",
                "no room is open for its root",
                "rimz loop timer install",
                "rimz loop timer status",
                "rimz start",
            ] {
                assert!(out.contains(text), "{out}");
            }
        }
    }
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
        failure_counts: std::collections::BTreeMap::from([("errored".to_owned(), 1)]),
    }));
    let out = strip(|w| {
        let mut tally = Tally::default();
        render_messages(w, &report, &mut tally)?;
        render_tally(w, &tally)
    });
    assert!(out.contains("MESSAGES"), "{out}");
    assert!(out.contains("3 open: 2 queued, 1 sent"), "{out}");
    assert!(out.contains("msg_stuck") && out.contains("@coder"), "{out}");
    assert!(!out.contains("msg_failed"), "{out}");
    assert!(
        out.contains("▸ 1 recent failure: 1 errored — rimz message list --all --system"),
        "{out}"
    );
    assert!(
        !out.contains("problem in MESSAGES") && out.contains("! 1 warning in MESSAGES"),
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
        failure_counts: std::collections::BTreeMap::new(),
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
fn drain_lag_rows_count_only_while_investigative() {
    use crate::cli::doctor::model::{DiagIncident, DrainLag};
    for recovered in [false, true] {
        let mut report = report_fixture();
        report.diagnostics = Some(Diagnostics::Ready {
            path: "/tmp/diagnostics".into(),
            incidents: vec![DiagIncident {
                kind: "hook_drain_lag".into(),
                source_severity: rimz::diag::record::DiagSeverity::Warn,
                state: if recovered {
                    DoctorState::Recovered
                } else {
                    DoctorState::Investigate
                },
                impact: if recovered {
                    DoctorImpact::Info
                } else {
                    DoctorImpact::Warn
                },
                first_at_ms: 1,
                last_at_ms: 70_002,
                record_count: 3,
                distinct_observer_count: 0,
                observer_ids: Vec::new(),
                sink_suppressed: 7,
                dropped_messages: 0,
                drain_lag: Some(DrainLag {
                    worst_lag_ms: 1_399_944,
                    peak_pending_bytes: 5_474_624,
                }),
                summary: "worst lag 23m19s; peak pending 5.2 MB".into(),
                build: None,
                stale_build: false,
                evidence_refs: Vec::new(),
            }],
        });
        let out = strip(|w| {
            let mut tally = Tally::default();
            render_diagnostics(w, &report, &mut tally)?;
            render_tally(w, &tally)
        });
        if recovered {
            assert!(
                out.contains("1 handled and closed themselves: hook_drain_lag"),
                "{out}"
            );
            assert!(out.contains("everything checked is healthy"), "{out}");
            assert!(!out.contains("worst lag"), "{out}");
        } else {
            for part in [
                "hook_drain_lag",
                "23m19s",
                "5.2 MB",
                "3 records",
                "7 suppressed",
                "1 warning in DIAGNOSTICS",
            ] {
                assert!(out.contains(part), "missing {part}: {out}");
            }
            assert!(
                out.contains(" to "),
                "the row retains the episode span: {out}"
            );
        }
        let audit = strip(|w| {
            let mut tally = Tally {
                audit: true,
                ..Default::default()
            };
            render_diagnostics(w, &report, &mut tally)?;
            render_tally(w, &tally)
        });
        assert!(audit.contains("worst lag 23m19s"), "{audit}");
        if recovered {
            assert!(
                audit.contains('▸') && audit.contains("everything checked is healthy"),
                "{audit}"
            );
            assert!(!audit.contains("handled and closed themselves"), "{audit}");
        }
    }
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
        !out.contains("lucid-atlas") && !out.contains("quiet-comet"),
        "{out}"
    );
    assert!(
        !out.contains("lost:") && !out.contains("recovered:") && !out.contains("forensics:"),
        "{out}"
    );
    assert_eq!(out.lines().filter(|line| line.contains('▸')).count(), 1);
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
fn audit_expands_routine_mux_groups_without_counting_them() {
    let log = MuxLog::Ready {
        path: "/tmp/log".into(),
        scope: LogScope::Server,
        size_bytes: 0,
        scanned_bytes: 0,
        logical_records: 5,
        records_before_cutoff: 0,
        since: None,
        problem_records: 5,
        omitted_issue_groups: 0,
        log_text_omitted: false,
        issues: (0..5)
            .map(|i| {
                log_issue(
                    &format!("routine-{i}"),
                    DoctorState::Expected,
                    DoctorImpact::Info,
                    1,
                )
            })
            .collect(),
    };
    let mut tally = Tally {
        audit: true,
        ..Default::default()
    };
    let out = strip(|w| {
        render_mux_log_notes(w, &log, &mut tally)?;
        render_tally(w, &tally)
    });
    for i in 0..5 {
        assert!(out.contains(&format!("routine-{i} (once)")), "{out}");
    }
    assert!(out.contains("everything checked is healthy"), "{out}");
}

#[test]
fn outdated_presence_build_counts_with_a_reload_fix() {
    let row = PresencePluginRow {
        plugin_id: 1,
        loaded_at_ms: Some(1),
        build: Some("old".into()),
        status: PresencePluginStatus::Active,
        rejected_count: None,
        outdated: true,
        telemetry: None,
    };
    let mut tally = Tally::default();
    presence_plugin_verdict(&mut tally, &row);
    let out = strip(|w| render_tally(w, &tally));
    assert!(out.contains("1 warning"), "{out}");
    assert!(out.contains("fix: run `rimz reload`"), "{out}");
}

#[test]
fn hibernate_uses_the_producers_structured_fix() {
    let report = rimz::harness::hibernate::PreflightReport {
        enabled: true,
        skipped: Some("cgroup.freeze is absent; enable delegation".into()),
        checks: vec![rimz::harness::hibernate::Check {
            name: "cgroup.freeze",
            status: rimz::harness::hibernate::CheckStatus::Warning,
            detail: "cgroup.freeze is absent; enable delegation".into(),
            summary: Some("cgroup.freeze is absent".into()),
            fix: Some("enable delegation"),
        }],
    };
    let mut tally = Tally::default();
    let body = strip(|w| render_hibernate(w, &report, &mut tally));
    assert!(!body.contains("enable delegation"), "{body}");
    let closing = strip(|w| render_tally(w, &tally));
    assert!(closing.contains("fix: enable delegation"), "{closing}");
}

#[test]
fn topology_conflict_retains_its_uncounted_remedy() {
    let writer = TopologyWriterHealth {
        recorded_bin: None,
        conflict: Some(super::super::model::TopologyWriterConflict {
            stale: None,
            accepted: None,
            rejected_count: 1,
            age_secs: 1,
            fix: "reattach to retire the legacy plugin".into(),
        }),
    };
    let mut tally = Tally::default();
    let out = strip(|w| {
        let mut kv = KeyVals::new();
        push_topology_writer(&mut kv, &mut tally, &writer);
        kv.render(w)
    });
    assert!(
        out.contains("reattach to retire the legacy plugin"),
        "{out}"
    );
    assert!(tally.warns.is_empty());
}

fn shortfall_details() -> serde_json::Value {
    serde_json::json!({
        "root": "/project", "server": "rust", "estimate_bytes": 8_000_000_000_u64,
        "available_bytes": 3_000_000_000_u64, "reserve_bytes": 1_000_000_000_u64,
        "holders": [{"root": "/holder", "server": "rust", "rss_bytes": 4_000_000_000_u64}]
    })
}

fn assert_refusal_shape(event: &str, details: serde_json::Value, memory: bool) {
    let record = rimz::diag::lsp::Record {
        at: Timestamp::now(),
        root: "/project".into(),
        server: "rust".into(),
        event: event.into(),
        details,
    };
    let mut tally = Tally::default();
    let body = strip(|w| render_lsp_refusal(w, &record, &mut tally));
    assert!(
        body.contains(if event == "queue_timeout" {
            "timed out in queue"
        } else {
            "refused"
        }),
        "{body}"
    );
    assert!(body.contains("0s ago"), "{body}");
    let closing = strip(|w| render_tally(w, &tally));
    if memory {
        assert!(
            body.contains("needs 8 GB, 3 GB available, 1 GB reserved; held by /holder rust (4 GB)"),
            "{event}: {body}"
        );
    } else {
        assert!(body.contains("server command is not trusted"), "{body}");
    }
    assert_eq!(closing.contains("free memory"), memory, "{closing}");
    assert!(!body.contains("committed_bytes"), "{body}");
}

#[test]
fn refused_shortfall_reports_memory() {
    assert_refusal_shape("refused", shortfall_details(), true);
}

#[test]
fn queue_timeout_shortfall_reports_memory() {
    assert_refusal_shape("queue_timeout", shortfall_details(), true);
}

#[test]
fn refused_reason_does_not_prescribe_freeing_memory() {
    assert_refusal_shape(
        "refused",
        serde_json::json!({"reason": "server command is not trusted"}),
        false,
    );
}

#[test]
fn legacy_shortfall_ignores_committed_bytes() {
    let mut details = shortfall_details();
    details["committed_bytes"] = serde_json::json!(9_000_000_000_u64);
    assert_refusal_shape("refused", details, true);
}

#[test]
fn semicolons_in_probe_errors_are_not_remedies() {
    let error = "transport unavailable; retry budget was 3";
    let sandbox = super::super::model::Sandbox {
        mode: rimz::config::Isolation::Sandbox,
        path: None,
        version: None,
        error: Some(error.into()),
    };
    let mut tally = Tally::default();
    let body = strip(|w| render_sandbox(w, &sandbox, &mut tally));
    assert!(body.contains(error), "{body}");
    let closing = strip(|w| render_tally(w, &tally));
    assert!(!closing.contains("fix: retry budget was 3"), "{closing}");
    let report = rimz::harness::hibernate::PreflightReport {
        enabled: true,
        skipped: Some(error.into()),
        checks: vec![rimz::harness::hibernate::Check {
            name: "swap",
            status: rimz::harness::hibernate::CheckStatus::Warning,
            detail: error.into(),
            summary: None,
            fix: None,
        }],
    };
    let mut tally = Tally::default();
    let body = strip(|w| render_hibernate(w, &report, &mut tally));
    assert!(body.contains(error), "{body}");
    let closing = strip(|w| render_tally(w, &tally));
    assert!(!closing.contains("fix: retry budget was 3"), "{closing}");
    for presence in [
        Presence::Poll {
            reason: error.into(),
            expected: false,
            summary: None,
            fix: None,
        },
        Presence::Unavailable {
            error: error.into(),
        },
    ] {
        let mut tally = Tally::default();
        presence_cell(&mut tally, &presence);
        let closing = strip(|w| render_tally(w, &tally));
        assert!(closing.contains(error), "{closing}");
        assert!(!closing.contains("fix: retry budget was 3"), "{closing}");
    }
}

#[test]
fn audit_adds_agent_context_without_changing_findings() {
    use super::super::model::AgentRow;
    let row = |handle: &str, status, historical: bool| AgentRow {
        kind: "codex".into(),
        agent_id: handle.into(),
        branch: None,
        status,
        phase: rimz::agents::TurnPhase::default(),
        last_seen: Timestamp::now(),
        handle: handle.into(),
        account: "codex@team".into(),
        turn_error: None,
        attributed_to_account: false,
        restartable: !historical,
        historical,
    };
    let mut report = report_fixture();
    let render = |report: &DoctorReport| {
        let mut tally = Tally::default();
        strip(|w| render_agents(w, report, &mut tally));
        strip(|w| render_tally(w, &tally))
    };
    report.agents = Some(AgentRollup::Observed {
        counts: AgentCounts::default(),
        rows: vec![row("@failed", AgentStatus::Failed, false)],
    });
    let plain = render(&report);
    report.audit = true;
    let Some(AgentRollup::Observed { rows, .. }) = &mut report.agents else {
        unreachable!()
    };
    rows.extend([
        row("@waiting", AgentStatus::Waiting, false),
        row("@ended", AgentStatus::Failed, true),
    ]);
    assert_eq!(render(&report), plain);
}

#[test]
fn shared_fixes_group_within_their_section_without_changing_counts() {
    let mut tally = Tally::default();
    tally.enter("MACHINE CONFIG");
    for name in ["one.md", "two.md", "three.md"] {
        tally.record(
            Severity::Warning,
            format!("{name} cannot be used"),
            "rimz agents validate",
        );
    }
    let closing = strip(|w| render_tally(w, &tally));
    assert!(
        closing.contains("3 warnings in MACHINE CONFIG"),
        "{closing}"
    );
    assert_eq!(
        closing.matches("fix: rimz agents validate").count(),
        1,
        "{closing}"
    );
    tally.enter("OTHER");
    tally.record(Severity::Warning, "another subject", "rimz agents validate");
    let closing = strip(|w| render_tally(w, &tally));
    assert!(
        closing.contains("4 warnings in MACHINE CONFIG, OTHER"),
        "{closing}"
    );
    assert!(closing.contains("MACHINE CONFIG: 3 findings"), "{closing}");
    assert_eq!(
        closing.matches("fix: rimz agents validate").count(),
        2,
        "{closing}"
    );
    for name in ["one.md", "two.md", "three.md"] {
        assert!(closing.contains(name), "{closing}");
    }
}

#[test]
fn a_waived_host_renders_off_for_this_run_without_a_refusal_line() {
    let out = strip(|w| {
        let mut tally = Tally::default();
        render_remote_control(
            w,
            &RemoteControl::On {
                agents: vec![RemoteAgent {
                    kind: "claude",
                    detail: "off for this run (chosen at `rimz start`)".to_owned(),
                    ready: true,
                    blocker: Some(
                        "Remote control is blocked.\n  - Unset it: unset DISABLE_GROWTHBOOK"
                            .to_owned(),
                    ),
                }],
                refusals: Vec::new(),
                skipped: Vec::new(),
                advisories: Vec::new(),
            },
            &mut tally,
        )
    });
    assert!(
        out.contains("off for this run (chosen at `rimz start`)"),
        "{out}"
    );
    assert!(out.contains("claude would still refuse"), "{out}");
    assert!(out.contains("unset DISABLE_GROWTHBOOK"), "{out}");
    assert!(!out.contains("`rimz start` refuses"), "{out}");
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
                    blocker: None,
                }],
                refusals: vec!["disableRemoteControl: true".to_owned()],
                skipped: vec!["managed Codex install is missing".to_owned()],
                advisories: vec![
                    "Codex remote-control updater version skew:\n    codex app-server daemon bootstrap --remote-control"
                        .to_owned(),
                ],
            },
            &mut tally,
        )?;
        render_tally(w, &tally)
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
    assert!(out.contains("managed Codex install is missing"), "{out}");
    assert!(
        out.contains("provider daemon advisory (no start impact)")
            && out.contains("Codex remote-control updater version skew")
            && out.contains("bootstrap --remote-control"),
        "{out}"
    );
}
