use super::*;

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

fn spawn_entry(check: bool, on: CheckOn) -> TaskEntry {
    TaskEntry {
        agent: Some("codex".to_owned()),
        check: check.then(|| "cargo test".to_owned()),
        on: Some(on),
        ..TaskEntry::default()
    }
}

fn wake_entry(check: bool, on: CheckOn) -> TaskEntry {
    TaskEntry {
        wake: Some(TaskTarget {
            kind: "claude".to_owned(),
            session: "sess-planner".to_owned(),
            handle: "@planner".to_owned(),
        }),
        check: check.then(|| "cargo test".to_owned()),
        on: Some(on),
        ..TaskEntry::default()
    }
}

fn check_entry() -> TaskEntry {
    TaskEntry {
        check: Some("cargo test".to_owned()),
        ..TaskEntry::default()
    }
}

fn summary(
    name: &str,
    entry: &TaskEntry,
    duration_ms: u64,
    mode: LoopRunMode,
    keep: bool,
    outcome: &RunOutcome,
) -> String {
    anstream::adapter::strip_str(&raw_summary(name, entry, duration_ms, mode, keep, outcome))
        .to_string()
}

fn raw_summary(
    name: &str,
    entry: &TaskEntry,
    duration_ms: u64,
    mode: LoopRunMode,
    keep: bool,
    outcome: &RunOutcome,
) -> String {
    let mut record = outcome.record.clone();
    record.duration_ms = Some(duration_ms);
    let summary = RunSummary {
        record: &record,
        presentation: &outcome.presentation,
    };
    let mut out = Vec::new();
    let action = TaskAction::from_entry(name, entry).unwrap();
    write_run_summary(&mut out, name, entry, &action, mode, keep, &summary).unwrap();
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

    let out = summary(
        "watchdog",
        &entry,
        1_900,
        LoopRunMode::Manual,
        false,
        &outcome,
    );

    assert!(out.contains("✗ failed (exit 1) in 1.9s"));
    assert!(out.contains("  │ error: boom\n  │ Usage: codex [OPTIONS] [PROMPT]"));
    assert!(out.contains("run: run_0123456789abcdef01234567"));
    assert!(out.contains("transcript: /tmp/transcript.jsonl"));
    assert!(out.contains("see: rimz loop show watchdog"));
}

#[test]
fn skipped_check_summary_uses_check_time_and_action_verbs() {
    let spawn = RunOutcome::check_result(
        LoopRunResult::CheckSkipped,
        CheckRecord {
            code: Some(0),
            timed_out: false,
            output: "ok".to_owned(),
        },
        4_400,
    );
    let entry = spawn_entry(true, CheckOn::Fail);
    assert_eq!(
        summary(
            "watchdog",
            &entry,
            9_000,
            LoopRunMode::Manual,
            false,
            &spawn,
        ),
        "✓ check passed (exit 0) in 4.4s — codex not started; fires when the check fails\n"
    );
    let raw = raw_summary(
        "watchdog",
        &entry,
        9_000,
        LoopRunMode::Manual,
        false,
        &spawn,
    );
    assert!(raw.contains(&ui::paint(
        ui::palette::good(),
        "✓ check passed (exit 0) in 4.4s"
    )));
    assert!(raw.contains(&ui::paint(
        ui::palette::muted(),
        " — codex not started; fires when the check fails"
    )));

    let wake = RunOutcome::check_result(
        LoopRunResult::CheckSkipped,
        CheckRecord {
            code: Some(1),
            timed_out: false,
            output: "no".to_owned(),
        },
        2_000,
    );
    let entry = wake_entry(true, CheckOn::Success);
    assert_eq!(
        summary("nudge", &entry, 8_000, LoopRunMode::Manual, false, &wake,),
        "○ check failed (exit 1) in 2.0s — @planner not woken; fires when the check passes\n"
    );
}

#[test]
fn scheduled_check_skip_keeps_compact_task_prefix() {
    let entry = spawn_entry(true, CheckOn::Fail);
    let outcome = RunOutcome::check_result(
        LoopRunResult::CheckSkipped,
        CheckRecord {
            code: Some(0),
            timed_out: false,
            output: "ok".to_owned(),
        },
        700,
    );

    assert_eq!(
        summary(
            "watchdog",
            &entry,
            900,
            LoopRunMode::Scheduled,
            false,
            &outcome,
        ),
        "loop `watchdog`: check passed (exit 0) in 700ms — codex not started; fires when the check fails\n"
    );
}

#[test]
fn trip_line_names_check_fact_and_action() {
    let check = CheckRecord {
        code: Some(101),
        timed_out: false,
        output: "failed".to_owned(),
    };
    let mut out = Vec::new();

    write_check_trip_line(
        &mut out,
        &TaskAction::Spawn("codex".to_owned()),
        &check,
        12_000,
    )
    .unwrap();

    assert_eq!(
        anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string(),
        "  ✗ check failed (exit 101) in 12s → starting codex\n"
    );
}

#[test]
fn completed_spawn_summary_prints_cost_message_and_keep_hint() {
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
            false,
            &outcome,
        ),
        "✓ completed in 3m · $0.42 · ↘ 12k ↗ 3k\n  │ pong\n  run: run_0123456789abcdef01234567\n  transcript: /tmp/transcript.jsonl\n  pane closed; rerun with --keep to watch\n"
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
            true,
            &outcome,
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
            false,
            &outcome,
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
            false,
            &outcome,
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
            false,
            &outcome,
        ),
        "✓ completed in 1.0s\n  no final message; see: rimz loop show watchdog\n  run: run_0123456789abcdef01234567\n  pane closed; rerun with --keep to watch\n"
    );
}

#[test]
fn delivered_summary_names_target_handle() {
    let outcome = RunOutcome::delivery("@planner", None);

    assert_eq!(
        summary(
            "nudge",
            &wake_entry(false, CheckOn::Fail),
            90,
            LoopRunMode::Manual,
            false,
            &outcome,
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
                false,
                &outcome,
            ),
            expected
        );
    }
}

#[test]
fn keep_hint_only_prints_for_manual_spawn_without_keep() {
    let outcome = RunOutcome::completed(None)
        .with_exit_code(Some(0))
        .with_run_id(Some("run_0123456789abcdef01234567".to_owned()))
        .with_last_message(Some("done".to_owned()));
    let entry = spawn_entry(false, CheckOn::Fail);

    for (mode, keep, should_hint) in [
        (LoopRunMode::Manual, false, true),
        (LoopRunMode::Manual, true, false),
        (LoopRunMode::Scheduled, false, false),
    ] {
        let stripped = summary("watchdog", &entry, 100, mode, keep, &outcome);
        assert_eq!(
            stripped.contains("pane closed; rerun with --keep to watch"),
            should_hint,
            "{mode:?} keep={keep}: {stripped}"
        );
    }
}

#[test]
fn manual_early_exits_explain_what_stays_in_place() {
    let entry = wake_entry(false, CheckOn::Fail);
    let gone = RunOutcome::target_gone("@planner", None);
    assert_eq!(
        summary("nudge", &entry, 100, LoopRunMode::Manual, false, &gone,),
        "○ @planner not alive — schedule left in place\n"
    );

    let expired = RunOutcome::expiry();
    assert_eq!(
        summary("nudge", &entry, 100, LoopRunMode::Manual, false, &expired,),
        "○ deadline expired — task left in place\n"
    );
}
