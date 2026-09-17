use super::*;
use crate::config::TaskTarget;
use crate::disk::summary::FileSummary;
use crate::harness::schedule::signal::{WatchOutcome, WatchVerdict};
use crate::store::event::SignalSource;

fn task() -> TaskEntry {
    TaskEntry {
        wait: Some(TaskTarget {
            kind: crate::ids::AgentKind::new_unchecked("claude"),
            session: "session".into(),
            handle: "@coder#feat-x".to_owned(),
        }),
        ..TaskEntry::default()
    }
}

fn meta(_handle: &str) -> WaitMeta {
    WaitMeta {
        armed_at: "2026-01-01T14:02:00Z".parse().unwrap(),
        delay: None,
    }
}

fn now() -> Timestamp {
    "2026-01-01T14:20:00Z".parse().unwrap()
}

fn signal(name: &str, payload: Value) -> Signal {
    Signal {
        name: name.parse().unwrap(),
        payload: payload.as_object().unwrap().clone(),
        source: SignalSource::Cli,
        watch: None,
    }
}

fn assert_watch(verdict: WatchVerdict, label: &str) {
    let task = TaskEntry {
        watch: Some(crate::config::WatchSpec::Command("cargo test".to_owned())),
        ..task()
    };
    for output_path in [None, Some("/tmp/rimz-waits/wait-test.output".into())] {
        for output in ["", "  last line\nnext line  \n"] {
            let signal = Signal {
                watch: Some(WatchOutcome {
                    verdict: verdict.clone(),
                    output: output.to_owned(),
                    output_path: output_path.clone(),
                    summary: if output.is_empty() {
                        FileSummary::default()
                    } else {
                        FileSummary {
                            bytes: 24,
                            lines: 2,
                            tokens: 6,
                        }
                    },
                }),
                ..signal("wait.test", serde_json::json!({}))
            };
            let path = match (&output_path, output.is_empty()) {
                (None, _) => "",
                (Some(_), true) => " · no output",
                (Some(_), false) => {
                    " · output: /tmp/rimz-waits/wait-test.output (<1k tokens, 2 lines)"
                }
            };
            assert_eq!(
                compose_wait(
                    "wait-test",
                    &task,
                    Some(&meta("@coder#feat-x")),
                    Evidence::Signal(&signal),
                    "  Inspect {{branch}}.\nKeep this line.  \n",
                    now(),
                ),
                format!(
                    "waited on `cargo test`\n{label}{path} [wait-test]\n\n  Inspect {{{{branch}}}}.\nKeep this line.  \n"
                )
            );
        }
    }
}

#[test]
fn watch_exit_success_keeps_output_summary_path_and_note() {
    assert_watch(
        WatchVerdict::Exited {
            code: Some(0),
            elapsed_ms: 3_000,
        },
        "exit 0 after 3s",
    );
}

#[test]
fn watch_exit_failure_keeps_output_summary_path_and_note() {
    assert_watch(
        WatchVerdict::Exited {
            code: Some(1),
            elapsed_ms: 720_000,
        },
        "exit 1 after 12m",
    );
}

#[test]
fn watch_killed_by_signal_keeps_output_summary_path_and_note() {
    assert_watch(
        WatchVerdict::Exited {
            code: None,
            elapsed_ms: 3_000,
        },
        "killed by signal after 3s",
    );
}

#[test]
fn polled_waits_name_their_spec_and_never_claim_no_output() {
    let meta = meta("@coder#feat-x");
    for (spec, headline) in [
        (
            crate::config::WatchSpec::Pid { pid: 42 },
            "waited on pid 42",
        ),
        (
            crate::config::WatchSpec::Check {
                check: "nc -z localhost 3000".to_owned(),
                every: "1s".to_owned(),
                on: crate::config::CheckOn::Success,
            },
            "waited on check `nc -z localhost 3000`",
        ),
        (
            crate::config::WatchSpec::File {
                file: "/repo/app.log".into(),
                grep: None,
                mark: None,
            },
            "waited on file /repo/app.log",
        ),
    ] {
        let task = TaskEntry {
            watch: Some(spec),
            timeout: Some("5m".to_owned()),
            ..task()
        };
        for (verdict, label, footer) in [
            (
                WatchVerdict::Met {
                    elapsed_ms: 3_000,
                    line: None,
                },
                "met after 3s",
                "",
            ),
            (
                WatchVerdict::NotMet {
                    elapsed_ms: 300_000,
                },
                "still not met after 5m",
                "\n\nStop it: rimz wait cancel wait-test\nAnother check-in: rimz wait --in 5m",
            ),
        ] {
            for (bytes, segment) in [
                (0, ""),
                (
                    20,
                    " · output: /tmp/rimz-waits/wait-test.output (<1k tokens, 1 line)",
                ),
            ] {
                let signal = Signal {
                    watch: Some(WatchOutcome {
                        verdict: verdict.clone(),
                        output: String::new(),
                        output_path: Some("/tmp/rimz-waits/wait-test.output".into()),
                        summary: FileSummary {
                            bytes,
                            lines: bytes.min(1),
                            tokens: bytes / 4,
                        },
                    }),
                    ..signal("wait.test", serde_json::json!({}))
                };
                assert_eq!(
                    compose_wait(
                        "wait-test",
                        &task,
                        Some(&meta),
                        Evidence::Signal(&signal),
                        "",
                        now(),
                    ),
                    format!("{headline}\n{label}{segment} [wait-test]{footer}")
                );
            }
        }
    }
}

#[test]
fn file_grep_wait_inlines_the_matched_line() {
    let task = TaskEntry {
        watch: Some(crate::config::WatchSpec::File {
            file: "/repo/app.log".into(),
            grep: Some("listening".to_owned()),
            mark: None,
        }),
        ..task()
    };
    let signal = Signal {
        watch: Some(WatchOutcome {
            verdict: WatchVerdict::Met {
                elapsed_ms: 42_000,
                line: Some("listening on :3000".to_owned()),
            },
            output: "listening on :3000".to_owned(),
            output_path: Some("/tmp/rimz-waits/wait-test.output".into()),
            summary: FileSummary {
                bytes: 19,
                lines: 1,
                tokens: 5,
            },
        }),
        ..signal("wait.test", serde_json::json!({}))
    };
    assert_eq!(
        compose_wait(
            "wait-test",
            &task,
            None,
            Evidence::Signal(&signal),
            "",
            now()
        ),
        "waited on file /repo/app.log for `listening`\nmet after 42s: `listening on :3000` · output: /tmp/rimz-waits/wait-test.output (<1k tokens, 1 line) [wait-test]"
    );
}

#[test]
fn watch_checkin_keeps_nonempty_summary_path_and_next_actions() {
    for timeout in [None, Some("1s"), Some("12m")] {
        let task = TaskEntry {
            watch: Some(crate::config::WatchSpec::Command("cargo test".to_owned())),
            timeout: timeout.map(str::to_owned),
            ..task()
        };
        for output in ["", "last line\n"] {
            let signal = Signal {
                watch: Some(WatchOutcome {
                    verdict: WatchVerdict::Running {
                        elapsed_ms: 1_800_000,
                    },
                    output: output.to_owned(),
                    output_path: Some("/tmp/rimz-waits/wait-test.output".into()),
                    summary: if output.is_empty() {
                        FileSummary::default()
                    } else {
                        FileSummary {
                            bytes: 10,
                            lines: 1,
                            tokens: 3,
                        }
                    },
                }),
                ..signal("wait.test", serde_json::json!({}))
            };
            let delay = timeout.unwrap_or("30m");
            let path = if output.is_empty() {
                " · no output"
            } else {
                " · output: /tmp/rimz-waits/wait-test.output (<1k tokens, 1 line)"
            };
            assert_eq!(
                compose_wait(
                    "wait-test",
                    &task,
                    None,
                    Evidence::Signal(&signal),
                    "",
                    now()
                ),
                format!(
                    "waited on `cargo test`\nstill running after 30m{path} [wait-test]\n\nStop it: rimz wait cancel wait-test\nAnother check-in: rimz wait --in {delay}"
                )
            );
        }
    }
}

#[test]
fn watch_timeout_keeps_output_summary_path_and_note() {
    assert_watch(
        WatchVerdict::TimedOut {
            elapsed_ms: 3_540_000,
        },
        "timed out after 59m",
    );
}

#[test]
fn watch_lost_keeps_output_summary_path_and_note() {
    assert_watch(
        WatchVerdict::Lost {
            detail: "lock disappeared".to_owned(),
            elapsed_ms: 180_000,
        },
        "watcher died after 3m; the command may still be running or may have died with it",
    );
}

#[test]
fn signal_uses_elapsed_time_and_compact_canonical_payload() {
    let signal = signal(
        "ci.failed",
        serde_json::json!({"branch":"feat-x","number":91,"signal":"not-canonical"}),
    );
    assert_eq!(
        compose_wait(
            "wait-test",
            &task(),
            Some(&meta("@coder#feat-x")),
            Evidence::Signal(&signal),
            "",
            now()
        ),
        "waited on ci.failed on feat-x (PR #91)\nfired after 18m [wait-test]\n{\"branch\":\"feat-x\",\"number\":91,\"signal\":\"ci.failed\"}"
    );
}

#[test]
fn signal_without_metadata_keeps_scope_and_has_no_elapsed_time() {
    for (name, payload, expected) in [
        (
            "pr.merged",
            serde_json::json!({"branch":"feat-x","number":91}),
            "waited on pr.merged on feat-x (PR #91)\nfired [wait-test]\n{\"branch\":\"feat-x\",\"number\":91,\"signal\":\"pr.merged\"}",
        ),
        (
            "agent.idle",
            serde_json::json!({"handle":"@coder"}),
            "waited on agent.idle @coder\nfired [wait-test]\n{\"handle\":\"@coder\",\"signal\":\"agent.idle\"}",
        ),
        (
            "team.idle",
            serde_json::json!({"instance":"forge#feat-x"}),
            "waited on team.idle forge#feat-x\nfired [wait-test]\n{\"instance\":\"forge#feat-x\",\"signal\":\"team.idle\"}",
        ),
        (
            "deploy.finished",
            serde_json::json!({}),
            "waited on deploy.finished\nfired [wait-test]\n{\"signal\":\"deploy.finished\"}",
        ),
    ] {
        assert_eq!(
            compose_wait(
                "wait-test",
                &task(),
                None,
                Evidence::Signal(&signal(name, payload)),
                "",
                now()
            ),
            expected
        );
    }
}

#[test]
fn delay_ends_with_name() {
    let meta = WaitMeta {
        delay: Some("30m".to_owned()),
        ..meta("@coder#feat-x")
    };
    assert_eq!(
        compose_wait(
            "wait-test",
            &task(),
            Some(&meta),
            Evidence::Scheduled,
            "",
            now()
        ),
        "waited 30m [wait-test]"
    );
}

#[test]
fn scheduled_wait_without_metadata_does_not_fabricate_delay() {
    assert_eq!(
        compose_wait("wait-test", &task(), None, Evidence::Scheduled, "", now()),
        "scheduled wait\nfired [wait-test]"
    );
}

#[test]
fn watch_command_preview_preserves_both_ends() {
    let command = format!("cargo test {} --all-targets", "界".repeat(140));
    let task = TaskEntry {
        watch: Some(crate::config::WatchSpec::Command(command.clone())),
        ..task()
    };
    let body = compose_wait("wait-test", &task, None, Evidence::Manual, "", now());
    let headline = body.lines().next().unwrap();
    assert!(headline.starts_with("waited on `cargo test "));
    assert!(headline.ends_with(" --all-targets`"));
    assert!(headline.contains('…'));
    assert_eq!(headline.chars().count(), "waited on ``".len() + 120);
    assert_eq!(task.watch, Some(crate::config::WatchSpec::Command(command)));
}

#[test]
fn manual_watch_and_signal_name_subject_and_fire_by_hand() {
    for (task, expected) in [
        (
            TaskEntry {
                watch: Some(crate::config::WatchSpec::Command("cargo test".to_owned())),
                ..task()
            },
            "waited on `cargo test`\nfired by hand [wait-test]",
        ),
        (
            TaskEntry {
                signal: Some("ci.*".to_owned()),
                matches: Some([("branch".to_owned(), "feat-x".to_owned())].into()),
                ..task()
            },
            "waited on ci.* on feat-x\nfired by hand [wait-test]",
        ),
    ] {
        assert_eq!(
            compose_wait(
                "wait-test",
                &task,
                Some(&meta("@coder#feat-x")),
                Evidence::Manual,
                "",
                now()
            ),
            expected
        );
    }
}

#[test]
fn signal_note_is_verbatim_regardless_of_armer() {
    let signal = signal(
        "ci.passed",
        serde_json::json!({"branch":"feat-x","number":91}),
    );
    for handle in ["@planner#feat-x", "@coder#other"] {
        assert_eq!(
            compose_wait(
                "wait-test",
                &task(),
                Some(&meta(handle)),
                Evidence::Signal(&signal),
                "  the migration window is open\n{{branch}}  \n",
                now()
            ),
            "waited on ci.passed on feat-x (PR #91)\nfired after 18m [wait-test]\n{\"branch\":\"feat-x\",\"number\":91,\"signal\":\"ci.passed\"}\n\n  the migration window is open\n{{branch}}  \n"
        );
    }
}

#[test]
fn signal_note_and_guard_evidence_remain_after_wait_body() {
    let signal = signal("deploy.failed", serde_json::json!({"branch":"feature"}));
    let body = compose_wait(
        "deployment",
        &task(),
        None,
        Evidence::Signal(&signal),
        "Inspect {{branch}}",
        now(),
    );
    let outcome = super::super::CheckOutcome::new(false, false, "failed guard".to_owned(), Some(1));
    assert_eq!(
        super::super::augment_prompt(body, "false", &outcome),
        "waited on deploy.failed\nfired [deployment]\n{\"branch\":\"feature\",\"signal\":\"deploy.failed\"}\n\nInspect {{branch}}\n\n--- check `false` exited 1 ---\nfailed guard"
    );
}
