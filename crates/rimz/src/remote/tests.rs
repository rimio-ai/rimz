use super::*;

fn parse(input: &str) -> RemoteTarget {
    RemoteTarget::parse(input).expect("target parses")
}

fn attach_plan(
    target: &str,
    no_resume: bool,
    mux: Option<MuxName>,
    term: TermPlan,
    truecolor: bool,
) -> SshAttachPlan {
    SshAttachPlan::new(SshAttachOptions {
        target: parse(target),
        lineage: "0123456789abcdef".to_owned(),
        no_resume,
        mux,
        term,
        truecolor,
        client_size: None,
    })
}

#[test]
fn target_grammar_accepts_supported_forms() {
    struct TargetCase {
        input: &'static str,
        destination: &'static str,
        host: &'static str,
        spec: RemoteSpec,
    }

    for case in [
        TargetCase {
            input: "dev-box:query-engine",
            destination: "dev-box",
            host: "dev-box",
            spec: RemoteSpec::Session("query-engine".to_owned()),
        },
        TargetCase {
            input: "dev-box:~/code/query-engine",
            destination: "dev-box",
            host: "dev-box",
            spec: RemoteSpec::Path("$HOME/code/query-engine".to_owned()),
        },
        TargetCase {
            input: "dev-box:~",
            destination: "dev-box",
            host: "dev-box",
            spec: RemoteSpec::Path("$HOME".to_owned()),
        },
        TargetCase {
            input: "dev-box:/workspace/hello-world",
            destination: "dev-box",
            host: "dev-box",
            spec: RemoteSpec::Path("/workspace/hello-world".to_owned()),
        },
        TargetCase {
            input: "dev-box:code/query-engine",
            destination: "dev-box",
            host: "dev-box",
            spec: RemoteSpec::Path("code/query-engine".to_owned()),
        },
        TargetCase {
            input: "agent@1.1.1.1:/workspace/hello-world",
            destination: "agent@1.1.1.1",
            host: "1.1.1.1",
            spec: RemoteSpec::Path("/workspace/hello-world".to_owned()),
        },
        TargetCase {
            input: "dev-box:build@2",
            destination: "dev-box",
            host: "dev-box",
            spec: RemoteSpec::Session("build@2".to_owned()),
        },
        TargetCase {
            input: "dev-box:~/code/foo@v2",
            destination: "dev-box",
            host: "dev-box",
            spec: RemoteSpec::Path("$HOME/code/foo@v2".to_owned()),
        },
        TargetCase {
            input: "alice@corp.com@dev-box:query-engine",
            destination: "alice@corp.com@dev-box",
            host: "dev-box",
            spec: RemoteSpec::Session("query-engine".to_owned()),
        },
        TargetCase {
            input: "user@[::1]:/srv/app",
            destination: "user@[::1]",
            host: "::1",
            spec: RemoteSpec::Path("/srv/app".to_owned()),
        },
        TargetCase {
            input: "[::1]:query-engine",
            destination: "[::1]",
            host: "::1",
            spec: RemoteSpec::Session("query-engine".to_owned()),
        },
    ] {
        let target = parse(case.input);
        assert_eq!(
            target.ssh_destination().as_str(),
            case.destination,
            "{}",
            case.input
        );
        assert_eq!(target.host_display(), case.host, "{}", case.input);
        assert_eq!(target.spec, case.spec, "{}", case.input);
    }
}

#[test]
fn target_grammar_rejects_malformed_forms() {
    enum ErrorKind {
        Empty,
        MissingColon,
        EmptyTarget,
        EmptyHost,
        UnclosedBracket,
        TildeUser,
    }

    for (input, kind) in [
        ("", ErrorKind::Empty),
        ("dev-box", ErrorKind::MissingColon),
        ("dev-box:", ErrorKind::EmptyTarget),
        (":query-engine", ErrorKind::EmptyHost),
        ("user@:", ErrorKind::EmptyHost),
        ("user@:query-engine", ErrorKind::EmptyHost),
        ("[::1:query-engine", ErrorKind::UnclosedBracket),
        ("dev-box:~alice", ErrorKind::TildeUser),
        ("dev-box:~alice/code", ErrorKind::TildeUser),
    ] {
        let err = RemoteTarget::parse(input).expect_err("target must fail");
        assert!(
            matches!(
                (kind, err),
                (ErrorKind::Empty, RemoteTargetError::Empty)
                    | (ErrorKind::MissingColon, RemoteTargetError::MissingColon(_))
                    | (ErrorKind::EmptyTarget, RemoteTargetError::EmptyTarget(_))
                    | (ErrorKind::EmptyHost, RemoteTargetError::EmptyHost(_))
                    | (
                        ErrorKind::UnclosedBracket,
                        RemoteTargetError::UnclosedBracket(_)
                    )
                    | (ErrorKind::TildeUser, RemoteTargetError::TildeUser(_))
            ),
            "{input} returned wrong error"
        );
    }
}

#[test]
fn ssh_destination_grammar_accepts_supported_forms() {
    for (input, destination, host) in [
        ("dev-box", "dev-box", "dev-box"),
        ("alice@dev-box", "alice@dev-box", "dev-box"),
        (
            "alice@corp.com@dev-box",
            "alice@corp.com@dev-box",
            "dev-box",
        ),
        ("[::1]", "[::1]", "::1"),
        ("user@[::1]", "user@[::1]", "::1"),
    ] {
        let parsed = SshDestination::parse(input).expect("destination parses");
        assert_eq!(parsed.as_str(), destination, "{input}");
        assert_eq!(parsed.host_display(), host, "{input}");
    }
}

#[test]
fn ssh_destination_grammar_rejects_malformed_forms() {
    enum ErrorKind {
        Empty,
        EmptyHost,
        MissingColon,
        UnclosedBracket,
    }

    for (input, kind) in [
        ("", ErrorKind::Empty),
        ("user@", ErrorKind::EmptyHost),
        ("@", ErrorKind::EmptyHost),
        ("dev-box:room", ErrorKind::MissingColon),
        ("[::1", ErrorKind::UnclosedBracket),
    ] {
        let err = SshDestination::parse(input).expect_err("destination must fail");
        assert!(
            matches!(
                (kind, err),
                (ErrorKind::Empty, RemoteTargetError::Empty)
                    | (ErrorKind::EmptyHost, RemoteTargetError::EmptyHost(_))
                    | (ErrorKind::MissingColon, RemoteTargetError::MissingColon(_))
                    | (
                        ErrorKind::UnclosedBracket,
                        RemoteTargetError::UnclosedBracket(_)
                    )
            ),
            "{input} returned wrong error"
        );
    }
}

#[test]
fn quote_and_display_are_shell_safe() {
    assert_eq!(sh_quote("it's"), "'it'\\''s'");
    assert_eq!(sh_quote(""), "''");
    assert_eq!(quote_remote_path("$HOME"), "\"$HOME\"");
    assert_eq!(
        quote_remote_path("$HOME/code/query-engine"),
        "\"$HOME\"'/code/query-engine'"
    );
    assert_eq!(quote_remote_path("/abs path"), "'/abs path'");

    let line = display_ssh_command(
        &attach_plan("dev-box:query-engine", false, None, TermPlan::Keep, false)
            .initial()
            .plain(),
    );
    assert!(line.starts_with("ssh -o ServerAliveInterval=5"), "{line}");
    assert!(line.contains(" -t -- dev-box '"), "{line}");
    assert!(line.ends_with('\''), "{line}");

    let v6 = display_ssh_command(
        &attach_plan("[::1]:query-engine", false, None, TermPlan::Keep, false)
            .initial()
            .plain(),
    );
    assert!(
        v6.contains(" -- '[::1]' "),
        "bracketed destinations quote against shell globbing: {v6}"
    );
}

#[test]
fn master_spec_is_unattended_and_has_no_remote_command() {
    let spec = attach_plan("dev-box:query-engine", false, None, TermPlan::Keep, false)
        .master(Path::new("/tmp/rimz.sock"));

    assert_eq!(
        spec.args,
        [
            "-o",
            "ServerAliveInterval=5",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "Compression=yes",
            "-M",
            "-N",
            "-o",
            "BatchMode=yes",
            "-o",
            "ControlPath=/tmp/rimz.sock",
            "--",
            "dev-box",
        ]
    );
    assert!(!spec.args.iter().any(|arg| arg == "-t"));
}

#[test]
fn ssh_error_summary_uses_the_last_open_ssh_line() {
    assert_eq!(
        ssh_error_summary("debug noise\nssh: connect to host dev port 22: Connection refused\n"),
        Some("connect to host dev port 22: Connection refused".to_owned())
    );
    assert_eq!(
        ssh_error_summary("Permission denied (publickey).\n"),
        Some("Permission denied (publickey).".to_owned())
    );
    assert_eq!(
        ssh_error_summary("ssh: Could not resolve hostname dev: Name or service not known\n"),
        Some("Could not resolve hostname dev: Name or service not known".to_owned())
    );
    assert_eq!(ssh_error_summary("\n \n"), None);
    assert!(ssh_error_summary(&"x".repeat(100)).unwrap().ends_with('…'));
}

#[test]
fn term_plan_selects_keep_copy_or_downgrade() {
    for term in ["alacritty", "xterm-kitty", "xterm-ghostty"] {
        assert!(term_needs_terminfo_copy(term), "{term}");
    }
    for term in ["xterm-256color", "screen-256color", "tmux-256color"] {
        assert!(!term_needs_terminfo_copy(term), "{term}");
    }

    struct TermCase {
        term: Option<&'static str>,
        infocmp: Option<&'static str>,
        expected: TermPlan,
    }

    for case in [
        TermCase {
            term: None,
            infocmp: None,
            expected: TermPlan::Keep,
        },
        TermCase {
            term: Some(""),
            infocmp: None,
            expected: TermPlan::Keep,
        },
        TermCase {
            term: Some("xterm-256color"),
            infocmp: None,
            expected: TermPlan::Keep,
        },
        TermCase {
            term: Some("alacritty"),
            infocmp: Some("ALACRITTY|fake,"),
            expected: TermPlan::Copy {
                name: "alacritty".to_owned(),
                source: "ALACRITTY|fake,".to_owned(),
            },
        },
        TermCase {
            term: Some("xterm-kitty"),
            infocmp: None,
            expected: TermPlan::Downgrade,
        },
        TermCase {
            term: Some("xterm-ghostty"),
            infocmp: Some("  "),
            expected: TermPlan::Downgrade,
        },
    ] {
        assert_eq!(
            term_plan_from(case.term, |_| case.infocmp.map(ToOwned::to_owned)),
            case.expected
        );
    }
}

#[test]
fn ssh_attach_plan_compiles_session_path_flags_control_and_term() {
    struct SpecCase {
        name: &'static str,
        target: &'static str,
        no_resume: bool,
        mux: Option<MuxName>,
        term: TermPlan,
        truecolor: bool,
        control: Option<&'static Path>,
        destination_index: usize,
        snippet_contains: &'static [&'static str],
    }

    for case in [
        SpecCase {
            name: "session attach",
            target: "dev-box:query-engine",
            no_resume: false,
            mux: None,
            term: TermPlan::Keep,
            truecolor: false,
            control: None,
            destination_index: 10,
            snippet_contains: &[
                "command -v rimz",
                "rimz not found on dev-box",
                "rimz remote setup",
                "exit 127",
                "exec rimz attach --attach -- 'query-engine'",
            ],
        },
        SpecCase {
            name: "path start",
            target: "dev-box:~/code/query-engine",
            no_resume: false,
            mux: None,
            term: TermPlan::Keep,
            truecolor: false,
            control: None,
            destination_index: 10,
            snippet_contains: &["exec rimz start --attach -- \"$HOME\"'/code/query-engine'"],
        },
        SpecCase {
            name: "no resume and mux",
            target: "dev-box:query-engine",
            no_resume: true,
            mux: Some(MuxName::Tmux),
            term: TermPlan::Keep,
            truecolor: false,
            control: None,
            destination_index: 10,
            snippet_contains: &["exec rimz attach --attach --no-resume --mux tmux -- "],
        },
        SpecCase {
            name: "control master",
            target: "dev-box:query-engine",
            no_resume: false,
            mux: None,
            term: TermPlan::Keep,
            truecolor: false,
            control: Some(Path::new("/tmp/rimz.sock")),
            destination_index: 14,
            snippet_contains: &["exec rimz attach --attach -- 'query-engine'"],
        },
        SpecCase {
            name: "term downgrade",
            target: "dev-box:query-engine",
            no_resume: false,
            mux: None,
            term: TermPlan::Downgrade,
            truecolor: false,
            control: None,
            destination_index: 10,
            snippet_contains: &["export TERM=xterm-256color; exec rimz"],
        },
        SpecCase {
            name: "truecolor keep",
            target: "dev-box:query-engine",
            no_resume: false,
            mux: None,
            term: TermPlan::Keep,
            truecolor: true,
            control: None,
            destination_index: 10,
            snippet_contains: &["export COLORTERM=truecolor; exec rimz"],
        },
        SpecCase {
            name: "truecolor and term downgrade",
            target: "dev-box:query-engine",
            no_resume: false,
            mux: None,
            term: TermPlan::Downgrade,
            truecolor: true,
            control: None,
            destination_index: 10,
            snippet_contains: &[
                "export COLORTERM=truecolor; export TERM=xterm-256color; exec rimz",
            ],
        },
        SpecCase {
            name: "term copy",
            target: "dev-box:query-engine",
            no_resume: false,
            mux: None,
            term: TermPlan::Copy {
                name: "alacritty".to_owned(),
                source: "ALACRITTY|fake,".to_owned(),
            },
            truecolor: false,
            control: None,
            destination_index: 10,
            snippet_contains: &[concat!(
                "export TERM=xterm-256color; printf '%s\\n' 'ALACRITTY|fake,' | ",
                "tic -x - 2>/dev/null && export TERM='alacritty'; exec rimz"
            )],
        },
    ] {
        let plan = attach_plan(
            case.target,
            case.no_resume,
            case.mux,
            case.term,
            case.truecolor,
        );
        let attempt = plan.initial();
        let spec = match case.control {
            Some(path) => attempt.control(path),
            None => attempt.plain(),
        };
        assert_eq!(spec.program, "ssh", "{}", case.name);
        assert_eq!(
            spec.args[..8],
            [
                "-o",
                "ServerAliveInterval=5",
                "-o",
                "ServerAliveCountMax=3",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "Compression=yes",
            ],
            "{}",
            case.name
        );
        if case.control.is_some() {
            assert_eq!(
                spec.args[8..12],
                [
                    "-o",
                    "ControlMaster=auto",
                    "-o",
                    "ControlPath=/tmp/rimz.sock",
                ],
                "{}",
                case.name
            );
        }
        assert_eq!(spec.args[case.destination_index - 2], "-t", "{}", case.name);
        assert_eq!(spec.args[case.destination_index - 1], "--", "{}", case.name);
        assert_eq!(
            spec.args[case.destination_index], "dev-box",
            "{}",
            case.name
        );
        assert_eq!(
            spec.args.len(),
            case.destination_index + 2,
            "snippet is a single argv element: {}",
            case.name
        );
        let snippet = spec.args.last().expect("snippet");
        assert!(
            snippet.starts_with("PATH=\"$HOME/.cargo/bin"),
            "{}",
            case.name
        );
        for needle in case.snippet_contains {
            assert!(
                snippet.contains(needle),
                "{} missing {needle}: {snippet}",
                case.name
            );
        }
        assert!(
            !snippet.contains(crate::mux::CLIENT_SIZE_ENV),
            "{} has no client-size export: {snippet}",
            case.name,
        );
    }
}

#[test]
fn ssh_attach_plan_exports_client_size_when_present() {
    let plan = SshAttachPlan::new(SshAttachOptions {
        target: parse("dev-box:query-engine"),
        lineage: "0123456789abcdef".to_owned(),
        no_resume: false,
        mux: None,
        term: TermPlan::Keep,
        truecolor: false,
        client_size: Some((180, 50)),
    });
    let snippet = plan.initial().plain().args.pop().expect("snippet");

    assert!(
        snippet.contains("export RIMZ_CLIENT_SIZE=180x50; exec rimz"),
        "{snippet}",
    );
}

#[test]
fn ssh_attach_plan_marks_retries_only() {
    let plan = attach_plan(
        "dev-box:~/code/query-engine",
        false,
        None,
        TermPlan::Keep,
        false,
    );
    let attended = plan.initial().plain();
    let retry = plan.retry().plain();

    for attempt in [&attended, &retry] {
        let snippet = attempt.args.last().unwrap();
        assert!(
            snippet.contains("export RIMZ_REMOTE_LINEAGE='0123456789abcdef';"),
            "every attempt carries the stable client lineage"
        );
        assert!(
            snippet.contains(&format!(
                "export RIMZ_REMOTE_CLIENT_VERSION='{}';",
                crate::build_id::VERSION
            )),
            "every attempt carries the local RimZ version: {snippet}"
        );
    }
    assert!(
        !attended.args.last().unwrap().contains(REMOTE_RECONNECT_ENV),
        "the first connect stays attended"
    );
    assert!(
        retry
            .args
            .last()
            .unwrap()
            .contains("export RIMZ_REMOTE_RECONNECT=1;"),
        "retry snippet marks an unattended reconnect"
    );
}

#[test]
fn remote_lineage_is_stable_and_room_scoped() {
    let target = parse("dev-box:query-engine");
    let lineage = remote_lineage(&target, "laptop", "alice");

    assert_eq!(lineage, remote_lineage(&target, "laptop", "alice"));
    assert_eq!(lineage.len(), 16);
    assert_ne!(
        lineage,
        remote_lineage(&parse("other-box:query-engine"), "laptop", "alice")
    );
    assert_ne!(
        lineage,
        remote_lineage(&parse("dev-box:other-room"), "laptop", "alice")
    );
}

#[test]
fn verdict_and_backoff_classify_reconnects() {
    let base = Duration::from_secs(1);
    let cap = Duration::from_secs(30);
    let delays: Vec<Duration> = (0..7)
        .map(|failures| backoff(failures, base, cap))
        .collect();
    assert_eq!(
        delays,
        [
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4),
            Duration::from_secs(8),
            Duration::from_secs(16),
            Duration::from_secs(30),
            Duration::from_secs(30),
        ]
    );

    assert_eq!(verdict(Some(0), true), Verdict::CleanExit);
    assert_eq!(verdict(Some(SSH_TRANSPORT_EXIT), true), Verdict::Retry);
    assert_eq!(
        verdict(Some(SSH_TRANSPORT_EXIT), false),
        Verdict::Fatal {
            code: SSH_TRANSPORT_EXIT
        }
    );
    assert_eq!(
        verdict(Some(REMOTE_RIMZ_MISSING_EXIT), true),
        Verdict::Fatal {
            code: REMOTE_RIMZ_MISSING_EXIT
        }
    );
    assert_eq!(verdict(None, true), Verdict::Fatal { code: 1 });
}

#[test]
fn reachable_retry_delay_stays_flat_then_doubles_each_minute() {
    let policy = ReconnectPolicy::default();

    assert_eq!(
        policy.reachable_delay(Duration::from_secs(2 * 60 + 59)),
        Duration::from_secs(2)
    );
    assert_eq!(
        policy.reachable_delay(Duration::from_secs(3 * 60)),
        Duration::from_secs(4)
    );
    assert_eq!(
        policy.reachable_delay(Duration::from_secs(4 * 60)),
        Duration::from_secs(8)
    );
    assert_eq!(
        policy.reachable_delay(Duration::from_secs(5 * 60)),
        Duration::from_secs(16)
    );
    assert_eq!(
        policy.reachable_delay(Duration::from_secs(6 * 60)),
        Duration::from_secs(30)
    );
}

#[test]
fn transport_failures_are_classified_conservatively() {
    for summary in [
        "Operation timed out",
        "Connection refused",
        "No route to host",
        "Network is unreachable",
        "Could not resolve hostname dev",
        "Temporary failure in name resolution",
        "Connection reset by peer",
    ] {
        assert!(transport_failure(summary), "{summary}");
    }
    for summary in [
        "Permission denied (publickey).",
        "Host key verification failed.",
    ] {
        assert!(!transport_failure(summary), "{summary}");
    }
}

#[test]
fn unreachable_retry_delay_uses_the_user_safety_ladder() {
    let policy = ReconnectPolicy::default();
    assert_eq!(
        (0..14)
            .map(|failure| policy.unreachable_delay(failure).as_secs())
            .collect::<Vec<_>>(),
        [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 20, 30, 30, 30]
    );
}

#[test]
fn reconnect_state_settles_established_sessions_and_failures() {
    let mut state = ReconnectState::new();

    assert_eq!(
        state.settle(Some(SSH_TRANSPORT_EXIT), false),
        Verdict::Fatal {
            code: SSH_TRANSPORT_EXIT
        }
    );
    assert_eq!(state.consecutive_failures(), 0);

    assert_eq!(state.settle(Some(SSH_TRANSPORT_EXIT), true), Verdict::Retry);
    assert_eq!(state.consecutive_failures(), 1);

    assert_eq!(
        state.settle(Some(SSH_TRANSPORT_EXIT), false),
        Verdict::Retry
    );
    assert_eq!(state.consecutive_failures(), 2);

    state.settle_zombie_kill();
    assert_eq!(state.consecutive_failures(), 0);

    assert_eq!(
        state.settle(Some(REMOTE_RIMZ_MISSING_EXIT), true),
        Verdict::Fatal {
            code: REMOTE_RIMZ_MISSING_EXIT
        }
    );
    assert_eq!(state.consecutive_failures(), 0);
}
