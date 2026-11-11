use super::*;

#[test]
fn manual_condition_notice_only_warns_when_unmet_and_counts_conjunctions() {
    let now = Timestamp::now();
    for (condition, ok, expected) in [
        (
            Some("team.stage=Done"),
            false,
            "condition not met (0 of 1); running anyway, task stays armed\n",
        ),
        (
            Some("team.stage=Done || ci=passed"),
            false,
            "condition not met; running anyway, task stays armed\n",
        ),
        (Some("!team.stage=Done"), true, ""),
        (None, false, ""),
    ] {
        let entry = TaskEntry {
            when: condition.map(|expr| vec![expr.into()]),
            every: condition.is_none().then(|| "1h".into()),
            ..Default::default()
        };
        let shape = schedule::TaskShape::compile("task", &entry);
        let timing = schedule::TaskTiming::evaluate(
            shape.trigger(),
            TaskSource::Config,
            Some(now),
            None,
            &now.to_zoned(jiff::tz::TimeZone::UTC),
        );
        let timing = if condition.is_some() {
            timing.with_condition(
                &schedule::when::Verdict {
                    ok,
                    readings: BTreeMap::from([("team.stage".into(), None), ("ci".into(), None)]),
                    unread: Default::default(),
                },
                None,
                now,
            )
        } else {
            timing
        };
        let mut out = Vec::new();
        write_condition_notice(&mut out, &timing).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), expected);
    }
}

struct RunOutcome {
    record: LoopRunRecord,
    presentation: LoopRunPresentation,
}

impl RunOutcome {
    fn terminal(result: LoopRunResult) -> Self {
        Self {
            record: LoopRunRecord::new("test", result, LoopRunMode::Manual, 0),
            presentation: LoopRunPresentation::default(),
        }
    }

    fn completed(check: Option<CheckRecord>) -> Self {
        Self::terminal(LoopRunResult::Completed).with_check(check)
    }

    fn check_result(result: LoopRunResult, check: CheckRecord, duration_ms: u64) -> Self {
        let mut outcome = Self::terminal(result).with_check(Some(check));
        outcome.presentation.check_duration_ms = Some(duration_ms);
        outcome
    }

    fn delivery(target: &str, check: Option<CheckRecord>) -> Self {
        let mut outcome = Self::terminal(LoopRunResult::Delivered).with_check(check);
        outcome.record.target = Some(target.to_owned());
        outcome
    }

    fn target_gone(target: &str, check: Option<CheckRecord>) -> Self {
        let mut outcome = Self::terminal(LoopRunResult::TargetGone).with_check(check);
        outcome.record.target = Some(target.to_owned());
        outcome
    }

    fn expiry() -> Self {
        Self::terminal(LoopRunResult::Expired)
    }

    fn with_check(mut self, check: Option<CheckRecord>) -> Self {
        self.record.check = check;
        self
    }

    fn with_exit_code(mut self, exit_code: Option<i32>) -> Self {
        self.presentation.exit_code = exit_code;
        self
    }

    fn with_run_id(mut self, run_id: Option<String>) -> Self {
        self.record.run_id = run_id;
        self
    }

    fn with_transcript_path(mut self, path: Option<String>) -> Self {
        self.record.transcript_path = path;
        self
    }

    fn with_failure_tail(mut self, tail: Option<String>) -> Self {
        self.presentation.failure_tail = tail;
        self
    }

    fn with_last_message(mut self, message: Option<String>) -> Self {
        self.record.last_message = message;
        self
    }

    fn with_cost(
        mut self,
        cost_usd: Option<f64>,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
    ) -> Self {
        self.record.cost_usd = cost_usd;
        self.record.input_tokens = input_tokens;
        self.record.output_tokens = output_tokens;
        self
    }

    fn with_streamed(mut self, streamed: bool) -> Self {
        self.presentation.streamed = streamed;
        self
    }
}

#[test]
fn manual_tty_prompts_for_blocked_project_trust() {
    for state in [TrustState::Untrusted, TrustState::Stale] {
        assert_eq!(
            project_trust_decision(state, LoopRunMode::Manual, true),
            ProjectTrustDecision::Prompt
        );
        assert_eq!(
            project_trust_decision(state, LoopRunMode::Manual, false),
            ProjectTrustDecision::Refuse
        );
        assert_eq!(
            project_trust_decision(state, LoopRunMode::Scheduled, true),
            ProjectTrustDecision::Refuse
        );
    }
    assert_eq!(
        project_trust_decision(TrustState::Trusted, LoopRunMode::Manual, true),
        ProjectTrustDecision::Proceed
    );
}

#[test]
fn an_error_recorded_as_a_skip_succeeds_and_any_other_error_propagates() {
    use rimz::harness::schedule::runner::{TaskFireFinished, TaskFireNotice};

    let finished = |result, notice| TaskFireFinished {
        record: LoopRunRecord::new("test", result, LoopRunMode::Manual, 0),
        presentation: LoopRunPresentation::default(),
        transition: RunTransition::Recorded,
        notice,
    };
    let skipped = skip_or_error(
        finished(
            LoopRunResult::AccountSkipped,
            TaskFireNotice::Gate {
                reason: "unknown codex account `ghost`".to_owned(),
            },
        ),
        anyhow::anyhow!("launch codex"),
    )
    .expect("a recorded skip is not an error");
    assert_eq!(skipped.record.result, LoopRunResult::AccountSkipped);
    assert_eq!(skipped.presentation.exit_code, None);

    let failed = skip_or_error(
        finished(LoopRunResult::Errored, TaskFireNotice::None),
        anyhow::anyhow!("pane refused"),
    )
    .expect_err("an errored fire keeps its error");
    assert_eq!(failed.to_string(), "pane refused");
}

fn spawn_entry(check: bool, on: CheckOn) -> TaskEntry {
    TaskEntry {
        agent: Some("codex".to_owned()),
        check: check.then(|| "cargo test".into()),
        on: Some(on),
        ..TaskEntry::default()
    }
}

fn wait_entry(check: bool, on: CheckOn) -> TaskEntry {
    TaskEntry {
        wait: Some(TaskTarget {
            kind: rimz::ids::AgentKind::new_unchecked("claude"),
            session: "sess-planner".into(),
            handle: "@planner".to_owned(),
        }),
        check: check.then(|| "cargo test".into()),
        on: Some(on),
        ..TaskEntry::default()
    }
}

fn check_entry() -> TaskEntry {
    TaskEntry {
        check: Some("cargo test".into()),
        ..TaskEntry::default()
    }
}

fn summary(
    name: &str,
    entry: &TaskEntry,
    duration_ms: u64,
    mode: LoopRunMode,
    outcome: &RunOutcome,
) -> String {
    anstream::adapter::strip_str(&raw_summary(name, entry, duration_ms, mode, outcome)).to_string()
}

fn raw_summary(
    name: &str,
    entry: &TaskEntry,
    duration_ms: u64,
    mode: LoopRunMode,
    outcome: &RunOutcome,
) -> String {
    let mut record = outcome.record.clone();
    record.duration_ms = Some(duration_ms);
    let summary = RunSummary {
        record: &record,
        presentation: &outcome.presentation,
        prose: ui::prose::Prose::Raw,
    };
    let mut out = Vec::new();
    let action = schedule::TaskShape::compile(name, entry)
        .action()
        .unwrap()
        .clone();
    write_run_summary(&mut out, name, entry, &action, mode, &summary).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn failed_summary_links_run_transcript_and_loop_show() {
    let entry = spawn_entry(false, CheckOn::Fail);
    let outcome = RunOutcome::terminal(LoopRunResult::Failed)
        .with_exit_code(Some(1))
        .with_run_id(Some("run_0123456789abcdef01234567".to_owned()))
        .with_transcript_path(Some("/tmp/transcript.jsonl".to_owned()))
        .with_failure_tail(Some(
            "error: boom\nUsage: codex [OPTIONS] [PROMPT]".to_owned(),
        ));

    let out = summary("watchdog", &entry, 1_900, LoopRunMode::Manual, &outcome);

    assert!(out.contains("✗ failed (exit 1) in 1.9s"));
    assert!(out.contains("  │ error: boom\n  │ Usage: codex [OPTIONS] [PROMPT]"));
    assert!(out.contains("run: run_0123456789abcdef01234567"));
    assert!(out.contains("transcript: /tmp/transcript.jsonl"));
    assert!(out.contains("see: rimz loop show watchdog"));
}

#[test]
fn manual_run_summary_snapshot() {
    let entry = spawn_entry(false, CheckOn::Fail);
    let outcome = RunOutcome::terminal(LoopRunResult::Failed)
        .with_exit_code(Some(1))
        .with_run_id(Some("run_0123456789abcdef01234567".to_owned()))
        .with_transcript_path(Some("/tmp/transcript.jsonl".to_owned()))
        .with_failure_tail(Some(
            "error: boom\nUsage: codex [OPTIONS] [PROMPT]".to_owned(),
        ))
        .with_cost(Some(0.42), Some(12_000), Some(3_400));

    insta::assert_snapshot!(
        "manual_run_summary",
        summary("watchdog", &entry, 1_900, LoopRunMode::Manual, &outcome)
    );
}

#[test]
fn skipped_check_summary_uses_check_time_and_action_verbs() {
    let spawn = RunOutcome::check_result(
        LoopRunResult::CheckSkipped,
        CheckRecord {
            duration_ms: None,
            agent: None,
            output_path: None,
            code: Some(0),
            timed_out: false,
            output: "ok".to_owned(),
        },
        4_400,
    );
    let entry = spawn_entry(true, CheckOn::Fail);
    assert_eq!(
        summary("watchdog", &entry, 9_000, LoopRunMode::Manual, &spawn),
        "✓ check passed (exit 0) in 4.4s — codex not started; fires when the check fails\n"
    );
    let raw = raw_summary("watchdog", &entry, 9_000, LoopRunMode::Manual, &spawn);
    assert!(raw.contains(&ui::paint(
        ui::palette::good(),
        "✓ check passed (exit 0) in 4.4s"
    )));
    assert!(raw.contains(&ui::paint(
        ui::palette::muted(),
        " — codex not started; fires when the check fails"
    )));

    let wait = RunOutcome::check_result(
        LoopRunResult::CheckSkipped,
        CheckRecord {
            duration_ms: None,
            agent: None,
            output_path: None,
            code: Some(1),
            timed_out: false,
            output: "no".to_owned(),
        },
        2_000,
    );
    let entry = wait_entry(true, CheckOn::Success);
    assert_eq!(
        summary("nudge", &entry, 8_000, LoopRunMode::Manual, &wait),
        "○ check failed (exit 1) in 2.0s — @planner not woken; fires when the check passes\n"
    );

    for (pass, on, expected) in [
        (
            false,
            CheckOn::Success,
            "○ judge `haiku` declined in 2.0s — codex not started; fires when the guard passes\n",
        ),
        (
            true,
            CheckOn::Fail,
            "✓ judge `haiku` passed in 2.0s — codex not started; fires when the guard fails\n",
        ),
    ] {
        let check = CheckRecord {
            duration_ms: None,
            agent: Some(
                serde_json::from_value(serde_json::json!({
                    "profile": "haiku",
                    "kind": "claude",
                    "model": null,
                    "verdict": {"pass": pass, "reason": "Decision."},
                    "error": null,
                    "cost_usd": null,
                    "input_tokens": null,
                    "output_tokens": null
                }))
                .unwrap(),
            ),
            output_path: None,
            code: Some(if pass { 0 } else { 1 }),
            timed_out: false,
            output: "Decision.".into(),
        };
        let outcome = RunOutcome::check_result(LoopRunResult::CheckSkipped, check, 2_000);
        let mut entry = spawn_entry(true, on);
        entry.check = Some(TaskCheck::Agent(Judge {
            agent: "haiku".into(),
            prompt: Some("Question.".into()),
            prompt_file: None,
            recheck: None,
            timeout: None,
        }));
        assert_eq!(
            summary("watchdog", &entry, 8_000, LoopRunMode::Manual, &outcome),
            expected
        );
    }
}

#[test]
fn scheduled_check_skip_keeps_compact_task_prefix() {
    let entry = spawn_entry(true, CheckOn::Fail);
    let outcome = RunOutcome::check_result(
        LoopRunResult::CheckSkipped,
        CheckRecord {
            duration_ms: None,
            agent: None,
            output_path: None,
            code: Some(0),
            timed_out: false,
            output: "ok".to_owned(),
        },
        700,
    );

    assert_eq!(
        summary("watchdog", &entry, 900, LoopRunMode::Scheduled, &outcome),
        "loop `watchdog`: check passed (exit 0) in 700ms — codex not started; fires when the check fails\n"
    );
}

#[test]
fn completed_spawn_summary_prints_cost_and_message() {
    let outcome = RunOutcome::completed(None)
        .with_exit_code(Some(0))
        .with_run_id(Some("run_0123456789abcdef01234567".to_owned()))
        .with_transcript_path(Some("/tmp/transcript.jsonl".to_owned()))
        .with_last_message(Some("pong\n".to_owned()))
        .with_cost(Some(0.42), Some(12_000), Some(3_400));

    assert_eq!(
        summary(
            "watchdog",
            &spawn_entry(false, CheckOn::Fail),
            180_000,
            LoopRunMode::Manual,
            &outcome
        ),
        "✓ completed in 3m · $0.42 · ↘ 12k ↗ 3k\n  │ pong\n  run: run_0123456789abcdef01234567\n  transcript: /tmp/transcript.jsonl\n"
    );
}

#[test]
fn streamed_spawn_summary_skips_repeated_message_and_links_run() {
    let outcome = RunOutcome::completed(None)
        .with_run_id(Some("run_0123456789abcdef01234567".to_owned()))
        .with_transcript_path(Some("/tmp/transcript.jsonl".to_owned()))
        .with_last_message(Some("already streamed".to_owned()))
        .with_streamed(true);

    assert_eq!(
        summary(
            "watchdog",
            &spawn_entry(false, CheckOn::Fail),
            1_000,
            LoopRunMode::Manual,
            &outcome
        ),
        "✓ completed in 1.0s\n  run: run_0123456789abcdef01234567\n  transcript: /tmp/transcript.jsonl\n"
    );
}

#[test]
fn scheduled_summary_prints_run_spend() {
    let outcome = RunOutcome::completed(None).with_cost(Some(0.09), Some(14_000), Some(269));

    assert_eq!(
        summary(
            "watchdog",
            &spawn_entry(false, CheckOn::Fail),
            120_000,
            LoopRunMode::Scheduled,
            &outcome
        ),
        "loop `watchdog`: completed in 2m · $0.09 · ↘ 14k ↗ 269\n"
    );

    let outcome = RunOutcome::terminal(LoopRunResult::Failed)
        .with_exit_code(Some(1))
        .with_cost(Some(0.09), Some(14_000), Some(269));
    assert_eq!(
        summary(
            "watchdog",
            &spawn_entry(false, CheckOn::Fail),
            120_000,
            LoopRunMode::Scheduled,
            &outcome
        ),
        "loop `watchdog`: failed (exit 1) in 2m · $0.09 · ↘ 14k ↗ 269\n  see: rimz loop show watchdog\n"
    );
}

#[test]
fn completed_spawn_summary_falls_back_when_last_message_is_blank() {
    let outcome = RunOutcome::completed(None)
        .with_exit_code(Some(0))
        .with_run_id(Some("run_0123456789abcdef01234567".to_owned()))
        .with_last_message(Some(" \n".to_owned()));
    assert_eq!(
        summary(
            "watchdog",
            &spawn_entry(false, CheckOn::Fail),
            1_000,
            LoopRunMode::Manual,
            &outcome
        ),
        "✓ completed in 1.0s\n  no final message; see: rimz loop show watchdog\n  run: run_0123456789abcdef01234567\n"
    );
}

#[test]
fn delivered_summary_names_target_handle() {
    let outcome = RunOutcome::delivery("@planner", None);

    assert_eq!(
        summary(
            "nudge",
            &wait_entry(false, CheckOn::Fail),
            90,
            LoopRunMode::Manual,
            &outcome
        ),
        "✓ delivered to @planner in 90ms\n"
    );
}

#[test]
fn check_only_verdicts_name_the_check_fact() {
    for (result, code, timed_out, expected) in [
        (
            LoopRunResult::Completed,
            Some(0),
            false,
            "✓ check passed (exit 0) in 1.2s\n",
        ),
        (
            LoopRunResult::Failed,
            Some(1),
            false,
            "✗ check failed (exit 1) in 1.2s\n",
        ),
        (
            LoopRunResult::TimedOut,
            None,
            true,
            "✗ check timed out in 1.2s\n",
        ),
    ] {
        let outcome = RunOutcome::terminal(result).with_check(Some(CheckRecord {
            duration_ms: None,
            agent: None,
            output_path: None,
            code,
            timed_out,
            output: "detail".to_owned(),
        }));
        assert_eq!(
            summary(
                "certs",
                &check_entry(),
                1_200,
                LoopRunMode::Manual,
                &outcome
            ),
            expected
        );
    }
}

#[test]
fn keep_hint_prints_only_while_a_fresh_agent_run_can_still_reach_its_agent() {
    let spawn = spawn_entry(false, CheckOn::Fail);
    let stay = TaskEntry {
        stay: true,
        ..spawn_entry(false, CheckOn::Fail)
    };
    let hint = "  the agent's pane closes after its turn; fire with --keep to leave it open";
    for (entry, keep, ended, expected) in [
        (&spawn, false, false, Some(hint)),
        (&spawn, true, false, None),
        (&spawn, false, true, None),
        (&stay, false, false, None),
        (&check_entry(), false, false, None),
        (&wait_entry(false, CheckOn::Fail), false, false, None),
    ] {
        let action = schedule::TaskShape::compile("watchdog", entry)
            .action()
            .unwrap()
            .clone();
        assert_eq!(
            keep_hint(entry, &action, keep, ended),
            expected,
            "{entry:?} keep={keep} ended={ended}"
        );
    }
}

#[test]
fn manual_early_exits_explain_what_stays_in_place() {
    let entry = wait_entry(false, CheckOn::Fail);
    let gone = RunOutcome::target_gone("@planner", None);
    assert_eq!(
        summary("nudge", &entry, 100, LoopRunMode::Manual, &gone),
        "○ @planner not alive — schedule left in place\n"
    );

    let expired = RunOutcome::expiry();
    assert_eq!(
        summary("nudge", &entry, 100, LoopRunMode::Manual, &expired),
        "○ deadline expired — task left in place\n"
    );
}

#[test]
fn watch_trip_and_summary_use_verdict_elapsed_once() {
    use rimz::harness::signal::WatchVerdict;

    for verdict in [
        WatchVerdict::Exited {
            code: Some(3),
            elapsed_ms: 3_000,
        },
        WatchVerdict::Exited {
            code: None,
            elapsed_ms: 3_000,
        },
        WatchVerdict::TimedOut { elapsed_ms: 3_000 },
        WatchVerdict::Lost {
            detail: "watcher process exited without reporting".to_owned(),
            elapsed_ms: 3_000,
        },
    ] {
        let check = CheckRecord {
            duration_ms: None,
            agent: None,
            code: match verdict {
                WatchVerdict::Exited { code, .. } => code,
                _ => None,
            },
            timed_out: matches!(verdict, WatchVerdict::TimedOut { .. }),
            output: String::new(),
            output_path: Some("/tmp/wait.log".into()),
        };
        for result in [LoopRunResult::Delivered, LoopRunResult::CheckSkipped] {
            let mut outcome = RunOutcome::terminal(result).with_check(Some(check.clone()));
            outcome.record.watch = Some(verdict.clone());
            for mode in [LoopRunMode::Manual, LoopRunMode::Scheduled] {
                let out = summary(
                    "watch",
                    &wait_entry(true, CheckOn::Success),
                    0,
                    mode,
                    &outcome,
                );
                assert_eq!(out.matches(&verdict.label()).count(), 1, "{out}");
                assert!(!out.contains(" in 0ms"), "{out}");
            }
        }
    }
}
