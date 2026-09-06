use super::*;
use crate::config::TaskTarget;
use crate::harness::schedule::signal::{WatchOutcome, WatchVerdict};
use crate::store::event::SignalSource;

fn task() -> TaskEntry {
    TaskEntry {
        wake: Some(TaskTarget {
            kind: "claude".to_owned(),
            session: "session".to_owned(),
            handle: "@coder#feat-x".to_owned(),
        }),
        ..TaskEntry::default()
    }
}

fn meta(handle: &str) -> WakeMeta {
    WakeMeta {
        armed_by: WakeArmer::Agent {
            handle: handle.to_owned(),
        },
        armed_at: "2026-01-01T14:02:00Z".parse().unwrap(),
        delay: None,
        last_observed_at: None,
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
        watch: Some("cargo test".to_owned()),
        ..task()
    };
    for output_path in [None, Some("/state/wakes/wake-test.log".into())] {
        for output in ["", "  last line\nnext line  \n"] {
            let signal = Signal {
                watch: Some(WatchOutcome {
                    verdict: verdict.clone(),
                    output: output.to_owned(),
                    output_path: output_path.clone(),
                }),
                ..signal("wake.test", serde_json::json!({}))
            };
            let path = if output_path.is_some() {
                " · output: /state/wakes/wake-test.log"
            } else {
                ""
            };
            let tail = if output.is_empty() {
                "(no output)"
            } else {
                output
            };
            assert_eq!(
                compose_wake(
                    "wake-test",
                    &task,
                    Some(&meta("@coder#feat-x")),
                    Evidence::Signal(&signal),
                    "  Inspect {{branch}}.\nKeep this line.  \n",
                    now(),
                ),
                format!(
                    "waited on `cargo test`\n{label}{path} [wake-test]\n{tail}\n\n  Inspect {{{{branch}}}}.\nKeep this line.  \n"
                )
            );
        }
    }
}

#[test]
fn watch_exit_success_keeps_output_path_tail_and_note() {
    assert_watch(
        WatchVerdict::Exited {
            code: Some(0),
            elapsed_ms: 3_000,
        },
        "exit 0 after 3s",
    );
}

#[test]
fn watch_exit_failure_keeps_output_path_tail_and_note() {
    assert_watch(
        WatchVerdict::Exited {
            code: Some(1),
            elapsed_ms: 720_000,
        },
        "exit 1 after 12m",
    );
}

#[test]
fn watch_killed_by_signal_keeps_output_path_tail_and_note() {
    assert_watch(
        WatchVerdict::Exited {
            code: None,
            elapsed_ms: 3_000,
        },
        "killed by signal after 3s",
    );
}

#[test]
fn watch_timeout_keeps_output_path_tail_and_note() {
    assert_watch(
        WatchVerdict::TimedOut {
            elapsed_ms: 3_540_000,
        },
        "timed out after 59m",
    );
}

#[test]
fn watch_lost_keeps_output_path_tail_and_note() {
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
        compose_wake(
            "wake-test",
            &task(),
            Some(&meta("@coder#feat-x")),
            Evidence::Signal(&signal),
            "",
            now()
        ),
        "waited on ci.failed on feat-x (PR #91)\nfired after 18m [wake-test]\n{\"branch\":\"feat-x\",\"number\":91,\"signal\":\"ci.failed\"}"
    );
}

#[test]
fn signal_without_metadata_keeps_scope_and_has_no_elapsed_time() {
    for (name, payload, expected) in [
        (
            "pr.merged",
            serde_json::json!({"branch":"feat-x","number":91}),
            "waited on pr.merged on feat-x (PR #91)\nfired [wake-test]\n{\"branch\":\"feat-x\",\"number\":91,\"signal\":\"pr.merged\"}",
        ),
        (
            "agent.idle",
            serde_json::json!({"handle":"@coder"}),
            "waited on agent.idle @coder\nfired [wake-test]\n{\"handle\":\"@coder\",\"signal\":\"agent.idle\"}",
        ),
        (
            "team.idle",
            serde_json::json!({"instance":"forge#feat-x"}),
            "waited on team.idle forge#feat-x\nfired [wake-test]\n{\"instance\":\"forge#feat-x\",\"signal\":\"team.idle\"}",
        ),
        (
            "deploy.finished",
            serde_json::json!({}),
            "waited on deploy.finished\nfired [wake-test]\n{\"signal\":\"deploy.finished\"}",
        ),
    ] {
        assert_eq!(
            compose_wake(
                "wake-test",
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
fn expiry_names_subscription_scope_and_window() {
    let task = TaskEntry {
        signal: Some("ci.*".to_owned()),
        matches: Some([("path".to_owned(), "/home/you/code/app-feat-x".to_owned())].into()),
        timeout: Some("59m".to_owned()),
        ..task()
    };
    assert_eq!(
        compose_wake(
            "wake-test",
            &task,
            Some(&meta("@coder#feat-x")),
            Evidence::Expired,
            "{{branch}}",
            now()
        ),
        "waited on ci.* on /home/you/code/app-feat-x\nnothing in 59m; subscription closed [wake-test]\n\n{{branch}}"
    );
}

#[test]
fn delay_ends_with_name_and_suppresses_same_target_armer() {
    let meta = WakeMeta {
        delay: Some("30m".to_owned()),
        ..meta("@coder#feat-x")
    };
    assert_eq!(
        compose_wake(
            "wake-test",
            &task(),
            Some(&meta),
            Evidence::Scheduled,
            "",
            now()
        ),
        "waited 30m [wake-test]"
    );
}

#[test]
fn scheduled_wake_without_metadata_does_not_fabricate_delay() {
    assert_eq!(
        compose_wake("wake-test", &task(), None, Evidence::Scheduled, "", now()),
        "scheduled wake\nfired [wake-test]"
    );
}

#[test]
fn manual_watch_and_signal_name_subject_and_fire_by_hand() {
    for (task, expected) in [
        (
            TaskEntry {
                watch: Some("cargo test".to_owned()),
                ..task()
            },
            "waited on `cargo test`\nfired by hand [wake-test]",
        ),
        (
            TaskEntry {
                signal: Some("ci.*".to_owned()),
                matches: Some([("branch".to_owned(), "feat-x".to_owned())].into()),
                ..task()
            },
            "waited on ci.* on feat-x\nfired by hand [wake-test]",
        ),
    ] {
        assert_eq!(
            compose_wake(
                "wake-test",
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
fn foreign_armer_leads_and_note_is_verbatim() {
    let signal = signal(
        "ci.passed",
        serde_json::json!({"branch":"feat-x","number":91}),
    );
    for handle in ["@planner#feat-x", "@coder#other"] {
        assert_eq!(
            compose_wake(
                "wake-test",
                &task(),
                Some(&meta(handle)),
                Evidence::Signal(&signal),
                "  the migration window is open\n{{branch}}  \n",
                now()
            ),
            format!(
                "{handle} armed this wake on you.\nwaited on ci.passed on feat-x (PR #91)\nfired after 18m [wake-test]\n{{\"branch\":\"feat-x\",\"number\":91,\"signal\":\"ci.passed\"}}\n\n  the migration window is open\n{{{{branch}}}}  \n"
            )
        );
    }
}

#[test]
fn shell_armer_leads_even_when_subscription_expires() {
    let task = TaskEntry {
        signal: Some("ci.failed".to_owned()),
        timeout: Some("59m".to_owned()),
        ..task()
    };
    let meta = WakeMeta {
        armed_by: WakeArmer::Human,
        ..meta("@coder#feat-x")
    };
    assert_eq!(
        compose_wake(
            "wake-test",
            &task,
            Some(&meta),
            Evidence::Expired,
            "",
            now()
        ),
        "armed on you from the shell.\nwaited on ci.failed\nnothing in 59m; subscription closed [wake-test]"
    );
}

#[test]
fn signal_note_and_guard_evidence_remain_after_wake_body() {
    let signal = signal("deploy.failed", serde_json::json!({"branch":"feature"}));
    let body = compose_wake(
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
