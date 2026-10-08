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
        reader: None,
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
    for output_path in [None, Some("/state/out/planner/wait-test.output".into())] {
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
                    " · output: /state/out/planner/wait-test.output (<1k tokens, 2 lines)"
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
fn wildcard_subscription_scope_uses_the_next_concrete_value() {
    for (path, suffix) in [(None, ""), (Some("/x"), " on /x")] {
        let mut task = task();
        task.signal = Some("pr.conflicted".to_owned());
        let mut matches = std::collections::BTreeMap::from([("branch".to_owned(), "*".to_owned())]);
        if let Some(path) = path {
            matches.insert("path".to_owned(), path.to_owned());
        }
        task.matches = Some(matches);
        let prompt = compose_wait(
            "any",
            &task,
            Some(&meta("@coder")),
            Evidence::Manual,
            "",
            now(),
        );
        assert_eq!(
            prompt.lines().next().unwrap(),
            format!("waited on pr.conflicted{suffix}")
        );
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
                "still not met after 5m · still watching",
                "\n\nStop it: rimz wait cancel wait-test\nAnother check-in: rimz wait --in 5m",
            ),
        ] {
            for (bytes, segment) in [
                (0, ""),
                (
                    20,
                    " · output: /state/out/planner/wait-test.output (<1k tokens, 1 line)",
                ),
            ] {
                let signal = Signal {
                    watch: Some(WatchOutcome {
                        verdict: verdict.clone(),
                        output: String::new(),
                        output_path: Some("/state/out/planner/wait-test.output".into()),
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
            output_path: Some("/state/out/planner/wait-test.output".into()),
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
        "waited on file /repo/app.log for `listening`\nmet after 42s: `listening on :3000` · output: /state/out/planner/wait-test.output (<1k tokens, 1 line) [wait-test]"
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
                    output_path: Some("/state/out/planner/wait-test.output".into()),
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
            let next = timeout
                .map(|delay| format!("\nAnother check-in: rimz wait --in {delay}"))
                .unwrap_or_default();
            let path = if output.is_empty() {
                " · no output"
            } else {
                " · output: /state/out/planner/wait-test.output (<1k tokens, 1 line)"
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
                    "waited on `cargo test`\nstill running after 30m · still watching{path} [wait-test]\n\nStop it: rimz wait cancel wait-test{next}"
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

fn fired(name: &str, payload: Value) -> String {
    compose_wait(
        "wait-test",
        &task(),
        None,
        Evidence::Signal(&signal(name, payload)),
        "",
        now(),
    )
}

fn forge(extra: Value) -> Value {
    let mut payload = serde_json::json!({
        "branch": "check-multi",
        "checks_url": "https://github.com/rimio-ai/rimz/commit/88824c2c52b8/checks",
        "head": "88824c2c52b8f12ec95df2fbaf57d4fa2122ad87",
        "number": 729,
        "path": "/work/rimz-worktrees/check-multi",
        "repo": "gh:github.com:rimio-ai/rimz",
        "url": "https://github.com/rimio-ai/rimz/pull/729",
    });
    payload
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    payload
}

fn without(mut payload: Value, key: &str) -> Value {
    payload.as_object_mut().unwrap().remove(key);
    payload
}

#[test]
fn signal_uses_elapsed_time_and_carries_no_payload_line() {
    let signal = signal("ci.failed", forge(serde_json::json!({})));
    assert_eq!(
        compose_wait(
            "wait-test",
            &task(),
            Some(&meta("@coder#feat-x")),
            Evidence::Signal(&signal),
            "",
            now()
        ),
        "waited on ci.failed on check-multi #729 @88824c2\nfired after 18m [wait-test]"
    );
}

#[test]
fn builtin_signal_is_one_subject_line_from_its_full_payload() {
    let worktree = serde_json::json!({
        "name": "temp-env",
        "branch": "temp-env-branch",
        "path": "/work/rimz-worktrees/temp-env",
        "repo": "/work/rimz",
        "base": "main",
        "from_pr": 12,
        "branch_deleted": true,
    });
    let agent = serde_json::json!({
        "kind": "claude",
        "session": "sess-1",
        "status": "idle",
        "errored": false,
        "handle": "@coder",
        "parent": "@planner",
    });
    let team = serde_json::json!({
        "team": "recon",
        "instance": "recon#lane-loops",
        "member": "@coder#lane-loops",
        "members": [{"handle": "@coder#lane-loops", "status": "idle"}],
    });
    let stage = serde_json::json!({
        "team": "recon",
        "instance": "recon#lane-loops",
        "to": "Review",
        "by": "coder",
        "board": "/work/blackboard.md",
        "at": "2026-01-01T14:02:00Z",
        "from": "Implement",
        "owner": "reviewer",
        "note": "ready",
    });
    let behind =
        serde_json::json!({"state": "open", "pr_head": "abc", "base": "main", "behind_by": 3});
    let queue = serde_json::json!({
        "state": "open",
        "pr_head": "abc",
        "base": "main",
        "queued_at": "2026-01-01T14:02:00Z",
    });
    for (name, payload, subject) in [
        (
            "ci.passed",
            forge(serde_json::json!({})),
            "ci.passed on check-multi #729 @88824c2",
        ),
        (
            "ci.custom",
            forge(serde_json::json!({})),
            "ci.custom on check-multi #729 @88824c2",
        ),
        (
            "ci.failed",
            without(forge(serde_json::json!({})), "number"),
            "ci.failed on check-multi @88824c2",
        ),
        (
            "ci.failed",
            without(forge(serde_json::json!({})), "head"),
            "ci.failed on check-multi #729",
        ),
        (
            "ci.failed",
            forge(serde_json::json!({"head": "88824", "number": "729"})),
            "ci.failed on check-multi @88824",
        ),
        (
            "pr.opened",
            forge(serde_json::json!({"state": "open"})),
            "pr.opened on check-multi #729",
        ),
        (
            "pr.merged",
            forge(serde_json::json!({"state": "merged"})),
            "pr.merged on check-multi #729",
        ),
        (
            "pr.closed",
            forge(serde_json::json!({"state": "closed"})),
            "pr.closed on check-multi #729",
        ),
        (
            "pr.queued",
            forge(queue.clone()),
            "pr.queued on check-multi #729",
        ),
        (
            "pr.something",
            forge(behind.clone()),
            "pr.something on check-multi #729",
        ),
        (
            "pr.behind",
            forge(behind.clone()),
            "pr.behind on check-multi #729 · 3 behind main",
        ),
        (
            "pr.behind",
            without(forge(behind.clone()), "base"),
            "pr.behind on check-multi #729 · 3 behind",
        ),
        (
            "pr.behind",
            without(forge(behind.clone()), "behind_by"),
            "pr.behind on check-multi #729",
        ),
        (
            "pr.conflicted",
            forge(behind.clone()),
            "pr.conflicted on check-multi #729 · with main",
        ),
        (
            "pr.conflicted",
            without(forge(behind), "base"),
            "pr.conflicted on check-multi #729",
        ),
        (
            "pr.dequeued",
            forge(queue),
            "pr.dequeued on check-multi #729",
        ),
        (
            "trunk.moved",
            serde_json::json!({
                "trunk": "main",
                "from": "aa24051f00000000",
                "to": "1bcceb9f00000000",
                "repo": "/work/rimz",
            }),
            "trunk.moved on main aa24051..1bcceb9",
        ),
        (
            "trunk.moved",
            serde_json::json!({"trunk": "main", "to": "1bcceb9f00000000", "repo": "/work/rimz"}),
            "trunk.moved on main",
        ),
        (
            "worktree.created",
            without(worktree.clone(), "branch_deleted"),
            "worktree.created temp-env from main",
        ),
        (
            "worktree.removed",
            worktree.clone(),
            "worktree.removed temp-env · branch deleted",
        ),
        (
            "worktree.removed",
            {
                let mut kept = worktree.clone();
                kept["branch_deleted"] = false.into();
                kept
            },
            "worktree.removed temp-env",
        ),
        ("worktree.renamed", worktree, "worktree.renamed temp-env"),
        ("agent.idle", agent.clone(), "agent.idle @coder"),
        (
            "agent.ended",
            without(agent, "handle"),
            "agent.ended sess-1",
        ),
        ("team.idle", team.clone(), "team.idle recon#lane-loops"),
        (
            "team.waiting",
            team.clone(),
            "team.waiting recon#lane-loops",
        ),
        ("team.ended", team.clone(), "team.ended recon#lane-loops"),
        ("team.paused", team.clone(), "team.paused recon#lane-loops"),
        (
            "team.failed",
            team.clone(),
            "team.failed recon#lane-loops · @coder",
        ),
        (
            "team.failed",
            without(team, "member"),
            "team.failed recon#lane-loops",
        ),
        (
            "team.stage",
            stage.clone(),
            "team.stage recon#lane-loops · Implement -> Review",
        ),
        (
            "team.stage",
            without(stage, "from"),
            "team.stage recon#lane-loops · Review",
        ),
    ] {
        assert_eq!(
            fired(name, payload),
            format!("waited on {subject}\nfired [wait-test]"),
            "{name}"
        );
    }
}

#[test]
fn dequeued_pr_names_its_reason_and_keeps_the_queue_checks_line() {
    let dequeued = forge(serde_json::json!({
        "state": "open",
        "pr_head": "abc",
        "base": "main",
        "dequeued_at": "2026-01-01T14:02:00Z",
        "reason": "FAILED_CHECKS",
        "queue_checks_url": "https://github.com/rimio-ai/rimz/commit/feed/checks",
    }));
    for (payload, expected) in [
        (
            dequeued.clone(),
            "waited on pr.dequeued on check-multi #729 · FAILED_CHECKS\nfired [wait-test]\nqueue checks: https://github.com/rimio-ai/rimz/commit/feed/checks",
        ),
        (
            without(dequeued.clone(), "reason"),
            "waited on pr.dequeued on check-multi #729\nfired [wait-test]\nqueue checks: https://github.com/rimio-ai/rimz/commit/feed/checks",
        ),
        (
            without(dequeued, "queue_checks_url"),
            "waited on pr.dequeued on check-multi #729 · FAILED_CHECKS\nfired [wait-test]",
        ),
    ] {
        assert_eq!(fired("pr.dequeued", payload), expected);
    }
}

#[test]
fn custom_signal_prints_one_line_per_field_in_key_order() {
    assert_eq!(
        fired(
            "deploy.finished",
            serde_json::json!({
                "signal": "not-canonical",
                "env": "prod",
                "attempt": 2,
                "detail": {"a": 1},
                "log": "first\nsecond",
                "path": "/srv/app",
                "ok": null,
                "a\nkey": "x",
            })
        ),
        "waited on deploy.finished\nfired [wait-test]\n\"a\\nkey\": x\nattempt: 2\ndetail: {\"a\":1}\nenv: prod\nlog: \"first\\nsecond\"\nok: null\npath: /srv/app\nsignal: not-canonical"
    );
    assert_eq!(
        fired("deploy.finished", serde_json::json!({})),
        "waited on deploy.finished\nfired [wait-test]"
    );
}

#[test]
fn builtin_subject_quotes_a_segment_holding_a_line_break() {
    let broken = "x\ny";
    let payload = serde_json::json!({
        "branch": broken,
        "head": "88824\rc2c52b8",
        "number": 7,
        "base": broken,
        "behind_by": 3,
        "reason": broken,
        "queue_checks_url": broken,
        "trunk": broken,
        "from": broken,
        "to": broken,
        "name": broken,
        "handle": broken,
        "session": "s\rid",
        "instance": broken,
        "member": "x\ny#lane",
    });
    let q = r#""x\ny""#;
    for (name, payload, subject) in [
        (
            "ci.failed",
            payload.clone(),
            format!(r#"ci.failed on {q} #7 @"88824\rc""#),
        ),
        (
            "pr.behind",
            payload.clone(),
            format!("pr.behind on {q} #7 · 3 behind {q}"),
        ),
        (
            "pr.conflicted",
            payload.clone(),
            format!("pr.conflicted on {q} #7 · with {q}"),
        ),
        (
            "pr.dequeued",
            without(payload.clone(), "queue_checks_url"),
            format!("pr.dequeued on {q} #7 · {q}"),
        ),
        (
            "trunk.moved",
            payload.clone(),
            format!("trunk.moved on {q} {q}..{q}"),
        ),
        (
            "worktree.created",
            payload.clone(),
            format!("worktree.created {q} from {q}"),
        ),
        ("agent.idle", payload.clone(), format!("agent.idle {q}")),
        (
            "agent.idle",
            without(payload.clone(), "handle"),
            r#"agent.idle "s\rid""#.to_owned(),
        ),
        (
            "team.failed",
            payload.clone(),
            format!("team.failed {q} · {q}"),
        ),
        (
            "team.stage",
            payload.clone(),
            format!("team.stage {q} · {q} -> {q}"),
        ),
        (
            "team.stage",
            without(payload.clone(), "from"),
            format!("team.stage {q} · {q}"),
        ),
    ] {
        assert_eq!(
            fired(name, payload),
            format!("waited on {subject}\nfired [wait-test]"),
            "{name}"
        );
    }
    assert_eq!(
        fired("pr.dequeued", payload).lines().nth(2),
        Some(format!("queue checks: {q}").as_str())
    );
}

#[test]
fn condition_readings_print_one_line_each_and_name_a_missing_one() {
    let condition = super::super::super::when::ConditionEvidence {
        since: None,
        when: "team.stage=Done".to_owned(),
        hold: None,
        held_ms: 0,
        readings: std::collections::BTreeMap::from([
            ("team.stage".to_owned(), Some("Done".to_owned())),
            ("pr.state".to_owned(), None),
            ("a\rkey".to_owned(), Some("plain".to_owned())),
        ]),
    };
    assert_eq!(
        compose_wait(
            "ship",
            &task(),
            None,
            Evidence::Condition(&condition),
            "",
            now()
        ),
        "waited on team.stage=Done\nfired [ship]\n\"a\\rkey\": plain\npr.state: unknown\nteam.stage: Done"
    );
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
            "waited on ci.passed on feat-x #91\nfired after 18m [wait-test]\n\n  the migration window is open\n{{branch}}  \n"
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
        super::super::augment_prompt(body, "check `false` exited 1", &outcome.output),
        "waited on deploy.failed\nfired [deployment]\nbranch: feature\n\nInspect {{branch}}\n\n--- check `false` exited 1 ---\nfailed guard"
    );
}
