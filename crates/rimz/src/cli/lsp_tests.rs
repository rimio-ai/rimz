use super::*;
use registry::{State, StopReason};
use render::status::{self, StateRole};

#[test]
fn query_server_names_include_config_and_checkout_rows_only() {
    let config = serde_json::from_value(
        serde_json::json!({"command":["stub"],"extensions":["rs"],"root-markers":[]}),
    )
    .unwrap();
    let mut context = QueryContext {
        root: "/checkout".into(),
        entries: vec![
            entry("/checkout", "registered", State::Ready),
            entry("/elsewhere", "elsewhere", State::Ready),
        ],
        servers: std::collections::BTreeMap::from([("configured".into(), config)]),
    };
    let error = context.validate_server(Some("nope")).unwrap_err();
    assert!(error.is::<super::super::usage::UsageError>());
    assert_eq!(
        error.to_string(),
        "no language server named nope; configured: configured, registered"
    );
    assert!(context.validate_server(None).is_ok());
    assert!(context.validate_server(Some("configured")).is_ok());
    assert!(context.validate_server(Some("registered")).is_ok());
    assert!(context.validate_server(Some("elsewhere")).is_err());
    context.servers.clear();
    context.entries.clear();
    assert_eq!(
        context
            .validate_server(Some("nope"))
            .unwrap_err()
            .to_string(),
        "no language server named nope; configured: none configured"
    );
}

#[test]
fn help_puts_queries_before_editor_bridges() {
    use clap::CommandFactory;
    let mut command = crate::cli::Cli::command();
    command.build();
    let lsp = command.find_subcommand("lsp").unwrap();
    let names: Vec<_> = lsp
        .get_subcommands()
        .filter(|command| !command.is_hide_set() && command.get_name() != "help")
        .map(clap::Command::get_name)
        .collect();
    assert_eq!(
        names,
        [
            "def", "refs", "hover", "impl", "callers", "callees", "symbols", "find", "show",
            "check", "list", "status", "stop", "attach", "shim"
        ]
    );
}

#[test]
fn visible_lsp_arguments_explain_their_input() {
    use clap::CommandFactory;
    let mut command = crate::cli::Cli::command();
    command.build();
    let lsp = command.find_subcommand("lsp").unwrap();
    for command in lsp
        .get_subcommands()
        .filter(|command| !command.is_hide_set() && command.get_name() != "help")
    {
        for argument in command
            .get_arguments()
            .filter(|argument| !argument.is_hide_set())
        {
            let help = argument
                .get_help()
                .expect("visible argument needs help")
                .to_string();
            assert!(
                !help.is_empty(),
                "{} {}",
                command.get_name(),
                argument.get_id()
            );
            if argument.get_id() == "targets" {
                let expected = match command.get_name() {
                    "symbols" => "File paths to outline",
                    "find" => "Symbol-name search strings",
                    _ => {
                        "Targets: path:line:col (1-based, column required), path::Symbol, or a name"
                    }
                };
                assert_eq!(
                    help,
                    expected,
                    "{} TARGET help must match its accepted forms",
                    command.get_name()
                );
            }
        }
    }
}

#[test]
fn no_tests_is_refs_and_callers_only_and_conflicts_with_json() {
    use clap::Parser;
    for verb in ["refs", "callers"] {
        assert!(
            crate::cli::Cli::try_parse_from(["rimz", "lsp", verb, "work", "--no-tests", "--all"])
                .is_ok()
        );
        let error =
            crate::cli::Cli::try_parse_from(["rimz", "lsp", verb, "work", "--no-tests", "--json"])
                .unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }
    for verb in ["impl", "callees", "find"] {
        let error = crate::cli::Cli::try_parse_from(["rimz", "lsp", verb, "work", "--no-tests"])
            .unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    }
}

#[test]
fn query_exit_uses_severity_not_argument_order() {
    let codes = [0, 5, 6, 7, 1, 4, 3];
    for (rank, code) in codes.iter().enumerate() {
        for other in &codes[..=rank] {
            assert_eq!(query_exit(*code, *other), *code);
            assert_eq!(query_exit(*other, *code), *code);
        }
    }
}

#[test]
fn query_failures_reuse_server_conditions_only() {
    for code in [1, 3, 4, 8, 9] {
        let mut failures = QueryFailures::default();
        let mut requests = 0;
        for _ in 0..3 {
            let result = failures.execute("rust", || {
                requests += 1;
                Err(match code {
                    3 => QueryErr::Unavailable {
                        root: "/repo".into(),
                        reason: query::UnavailableReason::NotRunning,
                    },
                    4 => QueryErr::Indexing {
                        server: "rust".into(),
                        seconds: 30,
                    },
                    8 => QueryErr::MemoryShort {
                        root: "/repo".into(),
                        server: "rust".into(),
                        shortfall: None,
                    },
                    9 => QueryErr::MemoryPressure {
                        root: "/repo".into(),
                        server: "rust".into(),
                    },
                    _ => rimz::lsp::LspErr::Protocol("bad request".into()).into(),
                })
            });
            assert_eq!(
                result.err().unwrap().exit_code(),
                if code >= 8 { 4 } else { code }
            );
        }
        assert_eq!(requests, if code == 1 { 3 } else { 1 });
        assert!(
            failures
                .execute("python", || Ok(query::Output::Answer {
                    result: serde_json::Value::Null,
                    document_uri: None
                }))
                .is_ok()
        );
    }
}

fn entry(root: &str, server: &str, state: State) -> registry::Entry {
    registry::Entry {
        kind: None,
        editor_check_on_save: None,
        root: root.into(),
        project: None,
        server: server.into(),
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
        request_count: 3,
        last_request_at_ms: None,
        peak_rss_kb: 4,
        restarts: 2,
        last_crash: None,
        leases: vec![],
        attached: vec![],
    }
}

#[test]
fn room_entries_keep_the_room_repository() {
    let registered = |root: &str, project: Option<&str>| registry::Entry {
        project: project.map(Into::into),
        ..entry(root, "rust", State::Ready)
    };
    let workspace = rimz::ResolvedWorkspace {
        workspace_id: rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/room")),
        project_root: "/room".into(),
        cwd_project_root: Some("/other".into()),
        root_class: rimz::workspace::RootClass::Repo,
        worktree_root: "/other".into(),
        worktree_branch: None,
        session_name: "room".into(),
        mux_hint: None,
    };
    let (kept, _) = room_entries(
        vec![
            registered("/room", Some("/room")),
            registered("/worktrees/feat", Some("/room")),
            registered("/other", Some("/other")),
            registered("/elsewhere", Some("/elsewhere")),
            registered("/room", None),
        ],
        &workspace,
    );
    let roots: Vec<_> = kept.iter().map(|entry| entry.root.as_path()).collect();
    assert_eq!(
        roots,
        ["/room", "/worktrees/feat", "/other"].map(std::path::Path::new)
    );
}

#[test]
fn list_table_orders_and_styles_server_states() {
    let mut ready = entry("/z", "rust", State::Ready);
    ready.last_request_at_ms = Some(11_000);
    let entries = [
        entry(
            "/b",
            "rust",
            State::Dormant {
                since_ms: 0,
                reason: None,
            },
        ),
        entry(
            "/a",
            "z",
            State::Dormant {
                since_ms: 0,
                reason: Some(StopReason::Idle),
            },
        ),
        entry(
            "/a",
            "m",
            State::Dormant {
                since_ms: 0,
                reason: Some(StopReason::MemoryPressure),
            },
        ),
        entry(
            "/c",
            "rust",
            State::Dormant {
                since_ms: 0,
                reason: Some(StopReason::Crashed),
            },
        ),
        ready,
        entry(
            "/a",
            "a",
            State::Dormant {
                since_ms: 0,
                reason: None,
            },
        ),
        entry(
            "/d",
            "rust",
            State::Stopped {
                at_ms: 0,
                reason: StopReason::Released,
            },
        ),
    ];
    let readings: Vec<_> = entries
        .iter()
        .map(|entry| {
            assert_eq!(entry.server_pid, Some(2));
            rimz::lsp::memory::Reading {
                rss_kb: 2,
                swap_kb: 10,
                peak_kb: 8,
            }
        })
        .collect();
    let table = list_table(entries.iter().zip(&readings), 56_000);
    let mut stripped = anstream::StripStream::new(Vec::new());
    table.render(&mut stripped).unwrap();
    let text = String::from_utf8(stripped.into_inner()).unwrap();
    let rows: Vec<Vec<&str>> = text
        .lines()
        .map(|line| line.split_whitespace().collect())
        .collect();
    assert_eq!(
        rows,
        vec![
            vec![
                "STATE", "CHECKOUT", "SERVER", "RSS", "SWAP", "TOTAL", "PEAK", "LAST", "LEASES"
            ],
            vec![
                "✓", "ready", "/z", "rust", "2", "KB", "10.2", "KB", "12.3", "KB", "8.2", "KB",
                "45s", "ago", "0"
            ],
            vec![
                "·", "dormant:", "memory", "pressure", "/a", "m", "-", "-", "-", "4.1", "KB", "-",
                "0"
            ],
            vec![
                "✗", "dormant:", "crashed", "/c", "rust", "-", "-", "-", "4.1", "KB", "-", "0"
            ],
            vec![
                "·", "not", "started", "/a", "a", "-", "-", "-", "4.1", "KB", "-", "0"
            ],
            vec![
                "·", "dormant:", "idle", "/a", "z", "-", "-", "-", "4.1", "KB", "-", "0"
            ],
            vec![
                "·", "not", "started", "/b", "rust", "-", "-", "-", "4.1", "KB", "-", "0"
            ],
            vec![
                "·", "stopped:", "released", "/d", "rust", "-", "-", "-", "4.1", "KB", "-", "0"
            ],
        ]
    );
    let mut lines = text.lines();
    let header = lines.next().unwrap();
    let ready = lines.next().unwrap();
    let mut from = ready.find("rust").unwrap() + "rust".len();
    for (column, value) in [
        ("RSS", "2 KB"),
        ("SWAP", "10.2 KB"),
        ("TOTAL", "12.3 KB"),
        ("PEAK", "8.2 KB"),
        ("LAST", "45s ago"),
        ("LEASES", "0"),
    ] {
        let end = from + ready[from..].find(value).unwrap() + value.len();
        let header_end = header.find(column).unwrap() + column.len();
        assert_eq!(
            ready[..end].chars().count(),
            header[..header_end].chars().count(),
            "{column} must be right-aligned"
        );
        from = end;
    }
    let mut raw = Vec::new();
    table.render(&mut raw).unwrap();
    let raw = String::from_utf8(raw).unwrap();
    assert!(raw.contains(&render::paint(status::role(StateRole::Success), "✓ ready")));
    assert!(raw.contains(&render::paint(
        status::role(StateRole::Failed),
        "✗ dormant: crashed"
    )));
    assert!(raw.contains(&render::paint(
        status::role(StateRole::Neutral),
        "· dormant: memory pressure"
    )));
}

fn headroom() -> rimz::lsp::admission::Headroom {
    rimz::lsp::admission::Headroom {
        available_bytes: 10_000_000_000,
        swap_free_bytes: 2_000_000_000,
        reserve_bytes: 3_000_000_000,
    }
}

#[test]
fn list_summarizes_room_memory_needs_alarms_and_folded_reasons() {
    let mut entries = vec![entry("/ready", "rust", State::Ready)];
    for (root, reason) in [
        ("/idle-a", Some(StopReason::Idle)),
        ("/idle-b", Some(StopReason::Idle)),
        ("/recycled", Some(StopReason::Recycled)),
        ("/evicted", Some(StopReason::Evicted)),
        ("/new", None),
        ("/crashed", Some(StopReason::Crashed)),
        ("/pressure", Some(StopReason::MemoryPressure)),
    ] {
        let mut row = entry(
            root,
            "rust",
            State::Dormant {
                since_ms: 0,
                reason,
            },
        );
        row.estimate_bytes = 4_000_000_000;
        entries.push(row);
    }
    entries.push(entry(
        "/python",
        "python",
        State::Dormant {
            since_ms: 0,
            reason: None,
        },
    ));
    let spaced_root = PathBuf::from(std::env::var_os("HOME").unwrap()).join("my project");
    let mut others = vec![
        entry("/other/worktree", "rust", State::Ready),
        entry("/other", "python", State::Indexing),
        entry(spaced_root.to_str().unwrap(), "rust", State::Starting),
        entry(
            "/hidden",
            "rust",
            State::Dormant {
                since_ms: 0,
                reason: None,
            },
        ),
    ];
    others[0].project = Some("/other".into());
    others[1].project = Some("/other".into());
    let render = |all| {
        let mut out = anstream::StripStream::new(Vec::new());
        let samples = std::cell::RefCell::new(std::collections::BTreeSet::new());
        ListView {
            entries: &entries,
            others: &others,
            all,
            now_ms: 56_000,
            headroom: Some(headroom()),
        }
        .render(
            &mut out,
            |entry| {
                assert!(entry.state.is_running(), "do not sample dormant servers");
                assert!(
                    samples
                        .borrow_mut()
                        .insert((entry.root.clone(), entry.server.clone())),
                    "sample each running row once"
                );
                rimz::lsp::memory::Reading {
                    rss_kb: 1_000,
                    swap_kb: 2_000,
                    peak_kb: 1_500,
                }
            },
            |entry| {
                if entry.root == std::path::Path::new("/idle-a") {
                    5_000_000_000
                } else {
                    entry.estimate_bytes
                }
            },
        )
        .unwrap();
        assert_eq!(samples.into_inner().len(), 4);
        String::from_utf8(out.into_inner()).unwrap()
    };
    let text = render(false);
    let lines: Vec<_> = text.lines().collect();
    assert!(
        lines.len() >= 4,
        "list must show summary, other rooms and the table: {text}"
    );
    assert_eq!(
        lines[0],
        "1 running: 3.1 MB; 9 GB free (10 GB available + 2 GB swap, 3 GB reserved); a start needs 0 B (python), 5 GB (rust)"
    );
    assert_eq!(
        lines[1],
        "other room: 6.1 MB in /other (2 running); rimz --root /other lsp list"
    );
    assert_eq!(
        lines[2],
        "other room: 3.1 MB in ~/my project (1 running); rimz --root ~/'my project' lsp list"
    );
    assert!(lines[3].starts_with("STATE"), "{text}");
    assert!(
        text.contains("dormant: crashed") && text.contains("dormant: memory pressure"),
        "{text}"
    );
    assert!(
        !text.contains("/idle-a") && !text.contains("/recycled") && !text.contains("/hidden"),
        "{text}"
    );
    assert_eq!(
        lines.last().unwrap(),
        &"6 not running: 2 idle, 2 not started, 1 evicted, 1 recycled; add --all to list them"
    );
    let all = render(true);
    assert!(
        all.contains("/idle-a") && all.contains("/recycled"),
        "{all}"
    );
    assert!(!all.contains("add --all"), "{all}");
}

#[test]
fn list_empty_room_and_unavailable_headroom_have_no_invented_figures() {
    for entries in [
        vec![],
        vec![entry(
            "/new",
            "rust",
            State::Dormant {
                since_ms: 0,
                reason: None,
            },
        )],
    ] {
        let mut out = Vec::new();
        ListView {
            entries: &entries,
            others: &[],
            all: false,
            now_ms: 0,
            headroom: None,
        }
        .render(&mut out, |_| panic!("no running server"), |_| 1_000_000)
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        if entries.is_empty() {
            assert_eq!(
                text,
                "0 running\nno language server registered in this room\n"
            );
        } else {
            assert_eq!(
                text,
                "0 running; a start needs 1 MB (rust)\n1 not running: 1 not started; add --all to list them\n"
            );
        }
    }
}

#[test]
fn status_orders_details_and_predicts_the_next_query_from_each_state() {
    for (state, since_ms, next) in [
        (State::Ready, 20_000, "answers at once"),
        (
            State::Starting,
            10_000,
            "waits for indexing (up to 110 s), then exits 4",
        ),
        (
            State::Indexing,
            10_000,
            "waits for indexing (up to 110 s), then exits 4",
        ),
        (
            State::Dormant {
                since_ms: 30_000,
                reason: None,
            },
            30_000,
            "starts it (needs 5 GB, 9 GB free (10 GB available + 2 GB swap, 3 GB reserved))",
        ),
        (
            State::Dormant {
                since_ms: 30_000,
                reason: Some(StopReason::Crashed),
            },
            30_000,
            "starts it (needs 5 GB, 9 GB free (10 GB available + 2 GB swap, 3 GB reserved))",
        ),
        (
            State::Stopped {
                at_ms: 40_000,
                reason: StopReason::Released,
            },
            40_000,
            "exits 3 (stopped: released)",
        ),
    ] {
        let mut row = entry("/checkout", "rust", state);
        row.started_at_ms = 10_000;
        row.ready_at_ms = Some(20_000);
        row.last_request_at_ms = Some(50_000);
        row.editor_check_on_save = Some(false);
        row.last_crash = Some(registry::CrashCause {
            at_ms: 0,
            exit_code: Some(1),
            signal: None,
            stderr_tail: "one\ntwo\nthree\nfour\nfive\nsix".into(),
            error: None,
        });
        let mut out = anstream::StripStream::new(Vec::new());
        write_status(
            &mut out,
            &row,
            60_000,
            rimz::lsp::memory::Reading {
                rss_kb: 1_000,
                swap_kb: 2_000,
                peak_kb: 8,
            },
            Some(headroom()),
            5_000_000_000,
        )
        .unwrap();
        let text = String::from_utf8(out.into_inner()).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert!(
            lines.len() >= 5,
            "status must show state age and next query: {text}"
        );
        assert!(
            lines[3].ends_with(&format!(
                "{}, {}s ago",
                render::lsp_state_label(&row.state),
                (60_000 - since_ms) / 1000
            )),
            "{text}"
        );
        assert!(lines[4].ends_with(next), "{text}");
        let keys: Vec<_> = lines
            .iter()
            .filter_map(|line| line.split_once(':').map(|(key, _)| key))
            .collect();
        let mut expected = vec!["checkout", "server", "kind", "state", "next query"];
        if row.state.is_running() {
            expected.push("memory");
            assert!(text.contains("1 MB RSS + 2 MB swap"), "{text}");
        } else {
            assert!(!text.contains(" RSS"), "{text}");
        }
        expected.extend([
            "peak",
            "last query",
            "requests",
            "restarts",
            "leases",
            "check on save",
            "last crash",
            "stderr",
            "broker pid",
            "server pid",
        ]);
        assert_eq!(keys, expected, "{text}");
        assert!(
            text.contains("10s ago")
                && text.contains(if row.state.is_running() {
                    "8.2 KB"
                } else {
                    "4.1 KB"
                }),
            "{text}"
        );
        assert!(
            !text.contains("one") && text.contains("two\nthree\nfour\nfive\nsix"),
            "{text}"
        );
    }
}

#[test]
fn status_predicts_memory_short_or_unknown_for_every_dormant_reason() {
    for reason in [
        None,
        Some(StopReason::Idle),
        Some(StopReason::Recycled),
        Some(StopReason::Evicted),
        Some(StopReason::MemoryPressure),
        Some(StopReason::Crashed),
        Some(StopReason::StoppedByHand),
    ] {
        let mut row = entry(
            "/checkout",
            "rust",
            State::Dormant {
                since_ms: 0,
                reason,
            },
        );
        row.peak_rss_kb = 0;
        for (headroom, needs, expected) in [
            (
                Some(headroom()),
                9_000_000_000,
                "starts it (needs 9 GB, 9 GB free (10 GB available + 2 GB swap, 3 GB reserved))",
            ),
            (
                Some(headroom()),
                10_000_000_000,
                "needs 10 GB, 9 GB free (10 GB available + 2 GB swap, 3 GB reserved); exits 4 unless idle servers can be stopped to make room",
            ),
            (
                None,
                5_000_000_000,
                "starts it if memory admits it (needs 5 GB), otherwise exits 4",
            ),
        ] {
            let mut out = anstream::StripStream::new(Vec::new());
            write_status(
                &mut out,
                &row,
                0,
                rimz::lsp::memory::Reading::default(),
                headroom,
                needs,
            )
            .unwrap();
            let text = String::from_utf8(out.into_inner()).unwrap();
            assert!(
                text.lines()
                    .any(|line| line.starts_with("next query") && line.ends_with(expected)),
                "{text}"
            );
            assert!(
                text.lines()
                    .any(|line| line.starts_with("last query") && line.ends_with("never")),
                "{text}"
            );
            assert!(
                text.lines()
                    .any(|line| line.starts_with("peak") && line.ends_with('-')),
                "{text}"
            );
        }
    }
}
