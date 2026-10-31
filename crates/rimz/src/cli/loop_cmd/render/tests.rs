use super::super::run_report::render_record_detail;
use super::*;

fn collapsed_run_rows(records: &[LoopRunRecord]) -> Vec<CollapsedRunRow<'_>> {
    super::collapsed_run_rows(records, &refused_fire_attribution(records))
}

fn stale_clause(records: &[LoopRunRecord], has_active_run: bool) -> Option<String> {
    super::stale_clause(records, &refused_fire_attribution(records), has_active_run)
}

fn write_runs_table(
    out: &mut impl Write,
    records: &[LoopRunRecord],
    limit: usize,
    now: Timestamp,
) -> std::io::Result<()> {
    let rows = collapsed_run_rows(records);
    super::write_runs_table(out, records, &rows, limit, now)
}

#[test]
fn agent_check_rows_name_the_profile() {
    let decoded = toml::from_str::<TaskEntry>(
        "agent = 'codex'\non = 'success'\n[check]\nagent = 'haiku'\nprompt = 'q'",
    );
    assert!(
        decoded.is_ok(),
        "agent check table must be accepted: {decoded:?}"
    );
    let entry = decoded.unwrap();
    let action = TaskAction::Spawn("codex".into());
    assert_eq!(
        check_summary(&entry, Some(&action)).as_deref(),
        Some("asks haiku (cap 5m); acts when it passes or gives no verdict")
    );
    assert_eq!(
        task_run_rule(&entry, &action),
        "judge `haiku`, then start codex on success"
    );
}

#[test]
fn guard_value_single_judge_explains_no_verdict_on_failure() {
    let entry: TaskEntry =
        toml::from_str("on = 'fail'\n[check]\nagent = 'haiku'\nprompt = 'q'").unwrap();
    assert_eq!(
        guard_value(&entry),
        "asks haiku (cap 5m); acts when it fails or gives no verdict"
    );
}

#[test]
fn guard_value_mixed_chain_explains_no_verdict_for_both_polarities() {
    let mut entry: TaskEntry =
        toml::from_str("check = 'cargo test'\n[[judge]]\nagent = 'haiku'\nprompt = 'q'").unwrap();
    for (on, expected) in [(CheckOn::Fail, "fail"), (CheckOn::Success, "pass")] {
        entry.on = Some(on);
        assert_eq!(
            guard_value(&entry),
            format!(
                "runs cargo test (cap 5m), then asks haiku (cap 5m); acts when both {expected}, a judge with no verdict counting as a {expected}"
            )
        );
    }
}

#[test]
fn guard_value_longer_chain_explains_no_verdict_for_all_stages() {
    let mut entry: TaskEntry = toml::from_str("check = 'cargo test'\n[[judge]]\nagent = 'haiku'\nprompt = 'q'\n[[judge]]\nagent = 'codex'\nprompt = 'q'").unwrap();
    for (on, expected) in [(CheckOn::Success, "pass"), (CheckOn::Fail, "fail")] {
        entry.on = Some(on);
        assert_eq!(
            guard_value(&entry),
            format!(
                "runs cargo test (cap 5m), then asks haiku (cap 5m), then asks codex (cap 5m); acts when all {expected}, a judge with no verdict counting as a {expected}"
            )
        );
    }
}

#[test]
fn guard_value_keeps_shell_only_and_any_wording() {
    let mut entry = TaskEntry {
        check: Some("cargo test".into()),
        ..Default::default()
    };
    for (on, expected) in [
        (CheckOn::Fail, "when it fails"),
        (CheckOn::Success, "when it passes"),
        (CheckOn::Any, "whatever the outcome"),
    ] {
        entry.on = Some(on);
        assert_eq!(
            guard_value(&entry),
            format!("runs cargo test (cap 5m); acts {expected}")
        );
    }
    let mut entry: TaskEntry =
        toml::from_str("on = 'any'\n[check]\nagent = 'haiku'\nprompt = 'q'").unwrap();
    assert_eq!(
        guard_value(&entry),
        "asks haiku (cap 5m); acts whatever the outcome"
    );
    let Some(TaskCheck::Agent(judge)) = entry.check.take() else {
        panic!("judge fixture");
    };
    entry.check = Some("cargo test".into());
    entry.judge.push(judge);
    assert_eq!(
        guard_value(&entry),
        "runs cargo test (cap 5m), then asks haiku (cap 5m); acts whatever the outcome"
    );
}

fn show_view(each_worktree: bool) -> ShowView {
    let now = Timestamp::from_second(100).unwrap();
    let entry = TaskEntry {
        root: "/repo".into(),
        agent: Some("sweeper".into()),
        check: Some("true".into()),
        when: Some(vec!["pr=open && ci=passed".into()]),
        stay: each_worktree,
        each_worktree,
        takeover: each_worktree,
        subscribe: ["ci.failed", "pr.merged"]
            .map(|signal| rimz::config::TeamSignalBinding {
                signal: signal.into(),
                matches: BTreeMap::new(),
                prompt: None,
            })
            .into(),
        ..TaskEntry::default()
    };
    let timing = schedule::TaskTiming::evaluate(
        schedule::TaskShape::compile("sweep", &entry).trigger(),
        TaskSource::Config,
        None,
        None,
        &now.to_zoned(jiff::tz::TimeZone::UTC),
    );
    let mut failed = record(20, LoopRunResult::Errored);
    failed.error = Some("launch failed".into());
    let show_agent_runs =
        has_agent_runs_section(&entry, Some(&TaskAction::Spawn("sweeper".into())));
    ShowView {
        capacity: BTreeMap::new(),
        name: "sweep".into(),
        kind: schedule::TaskKind::Routine,
        entry,
        source: TaskSource::Config,
        timing,
        now_zoned: now.to_zoned(jiff::tz::TimeZone::UTC),
        records: vec![failed],
        heard: None,
        launches: BTreeMap::from([
            (
                PathBuf::from("/repo/older"),
                schedule::launch_ledger::LaunchRecord {
                    destination: None,
                    at: Timestamp::from_second(10).unwrap(),
                    leader: "old-leader".into(),
                },
            ),
            (
                PathBuf::from("/repo/newer"),
                schedule::launch_ledger::LaunchRecord {
                    destination: None,
                    at: Timestamp::from_second(90).unwrap(),
                    leader: "new-leader".into(),
                },
            ),
        ]),
        in_flight: None,
        backlog: None,
        condition: "  pr=open   ✓ open\n  ci=passed   ? not checked yet\n".into(),
        subscriptions: vec![SubscriptionView {
            name: "wake".into(),
            checkout: "/repo/newer".into(),
            target: None,
            signal: Some("ci.failed".into()),
            last: "never fired".into(),
            last_style: ui::palette::muted(),
        }],
        room_is_open: false,
        strike_count: 0,
        leader_statuses: BTreeMap::new(),
        you: false,
        config: MachineConfig::default().into(),
        throttle: ShowThrottle::Off,
        show_agent_runs,
        worktrees: [("older", "old-leader", 10), ("newer", "new-leader", 90)]
            .map(
                |(name, leader, second)| schedule::fanout::CheckoutCondition {
                    checkout: PathBuf::from(format!("/repo/{name}")),
                    name: name.into(),
                    state: schedule::fanout::CheckoutState::Launched {
                        leader: leader.into(),
                        at: Timestamp::from_second(second).unwrap(),
                    },
                },
            )
            .into(),
        worktrees_error: None,
        workflow_runs: Vec::new(),
        all: false,
    }
}

fn show_text(view: &ShowView) -> String {
    let mut out = Vec::new();
    render_show(&mut out, view, 10).unwrap();
    anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string()
}

fn workflow_view() -> ShowView {
    let mut view = show_view(true);
    view.entry.agent = None;
    view.entry.workflow = Some("review".into());
    view.entry.stay = false;
    view.entry.subscribe.clear();
    view.launches.clear();
    view.records.clear();
    view.subscriptions.clear();
    view.worktrees.clear();
    let mut records = Vec::new();
    let mut launches = BTreeMap::new();
    for (index, state) in ["active", "parked", "failed", "done", "canceled", "gone"]
        .into_iter()
        .enumerate()
    {
        let id = rimz::ids::WorkflowRunId::new();
        let at = Timestamp::from_second(10 + index as i64).unwrap();
        let checkout = PathBuf::from(format!("/repo/{state}"));
        if state != "gone" {
            records.push(
                serde_json::from_value(serde_json::json!({
                    "version": 1, "id": id, "workflow": "review", "task": "sweep",
                    "spec_dir": "/spec", "spec_digest": "digest", "scope": {
                        "kind": "worktree", "path": checkout,
                    },
                    "project_root": "/repo", "created_at": at, "state": state,
                    "park": (state == "parked").then(|| serde_json::json!({
                        "reason": "user", "text": "review this", "at": at,
                    })),
                    "node": "gate", "entered_at": at + jiff::SignedDuration::from_secs(30),
                    "visits": {}, "visit_grant": 0,
                    "deadline_fired": false, "next_seq": 1,
                }))
                .unwrap(),
            );
        }
        launches.insert(
            checkout.clone(),
            schedule::launch_ledger::WorkflowLaunchRecord {
                at,
                run: id.clone(),
                scope: rimz::store::workflow::ScopeKind::Worktree,
            },
        );
        view.worktrees.push(CheckoutCondition {
            checkout,
            name: state.into(),
            state: CheckoutState::Workflow { run: id, at },
        });
    }
    view.workflow_runs =
        list::WorkflowRunRow::for_show(&records, "sweep", Path::new("/repo"), &launches);
    view
}

#[test]
fn workflow_run_projection_keeps_terminal_and_gone_launches() {
    let view = workflow_view();
    let json = serde_json::to_value(&view.workflow_runs).unwrap();
    assert_eq!(json.as_array().unwrap().len(), 6, "{json}");
    let gone = json
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["state"] == "gone")
        .unwrap();
    let CheckoutState::Workflow { run, at } = &view.worktrees[5].state else {
        panic!("workflow fixture");
    };
    assert_eq!(
        gone,
        &serde_json::json!({
            "id": run, "checkout": "/repo/gone", "state": "gone", "since": at,
        })
    );
    assert_eq!(
        json.as_array()
            .unwrap()
            .iter()
            .filter(|row| row["state"] == "active")
            .count(),
        1
    );
}

#[test]
fn workflow_worktrees_keep_attention_and_unfold_finished_runs() {
    let mut view = workflow_view();
    let text = show_text(&view);
    assert!(text.contains("RUN") && !text.contains("LEADER"), "{text}");
    for (index, state) in ["active", "parked", "failed"].into_iter().enumerate() {
        let CheckoutState::Workflow { run, .. } = &view.worktrees[index].state else {
            panic!("workflow fixture");
        };
        assert!(
            text.lines()
                .any(|line| line.contains(&format!("{run}  {state} · gate"))),
            "{text}"
        );
    }
    assert!(text.contains("· user: review this"), "{text}");
    assert!(
        text.contains("1 canceled") && text.contains("1 done") && text.contains("1 gone"),
        "{text}"
    );
    assert!(
        text.contains("newest gone") && text.contains("reasons and runs:"),
        "{text}"
    );
    view.all = true;
    let all = show_text(&view);
    assert!(!all.contains("… 3 launched"), "{all}");
    for (index, state) in ["done", "canceled", "gone"].into_iter().enumerate() {
        let CheckoutState::Workflow { run, .. } = &view.worktrees[index + 3].state else {
            panic!("workflow fixture");
        };
        assert!(
            all.lines()
                .any(|line| line.contains(&format!("{run}  {state}"))),
            "{all}"
        );
    }
}

#[test]
fn workflow_worktrees_json_joins_the_same_run_projection() {
    let view = workflow_view();
    let runs = serde_json::to_value(&view.workflow_runs).unwrap();
    let worktrees = serde_json::to_value(worktrees_json(&view)).unwrap();
    for row in worktrees.as_array().unwrap() {
        let run = runs
            .as_array()
            .unwrap()
            .iter()
            .find(|run| run["checkout"] == row["checkout"])
            .unwrap();
        let checkout = view
            .worktrees
            .iter()
            .find(|checkout| serde_json::json!(checkout.checkout) == row["checkout"])
            .unwrap();
        let CheckoutState::Workflow { at, .. } = &checkout.state else {
            panic!("workflow fixture");
        };
        let entered_at = if run["state"] == "gone" {
            *at
        } else {
            *at + jiff::SignedDuration::from_secs(30)
        };
        assert_eq!(&row["run"], run, "{row}");
        assert_eq!(row["since"], serde_json::json!(at));
        assert_eq!(run["since"], serde_json::json!(entered_at));
        for key in ["leader", "leader_status", "armed"] {
            assert!(row.get(key).is_none(), "{row}");
        }
    }
}

#[test]
fn workflow_runs_table_omits_worktree_runs_including_folded_rows() {
    let mut view = workflow_view();
    let text = show_text(&view);
    assert!(!text.contains("\nRUNS\n"), "{text}");
    view.entry.each_worktree = false;
    view.worktrees.clear();
    let text = show_text(&view);
    assert!(text.contains("\nRUNS\n"), "{text}");
    for run in &view.workflow_runs {
        assert_eq!(text.matches(run.id.as_str()).count(), 1, "{text}");
    }
    assert!(text.contains("gone"), "{text}");
}

#[test]
fn named_launch_rows_and_failed_attention_show_destination_not_scope() {
    let mut view = show_view(false);
    let launch = view.launches.get_mut(Path::new("/repo/newer")).unwrap();
    let mut value = serde_json::to_value(&launch).unwrap();
    value["destination"] = serde_json::json!("/worktrees/landing");
    *launch = serde_json::from_value(value).unwrap();
    view.leader_statuses
        .insert("new-leader".into(), rimz::agents::AgentStatus::Failed);
    let text = show_text(&view);
    assert!(text.contains("landing  @new-leader"), "{text}");
    assert!(text.contains("@new-leader in landing"), "{text}");
    assert!(!text.contains("@new-leader in newer"), "{text}");
}

#[test]
fn show_header_names_kind_checkout_and_condition_progress_once() {
    let mut view = show_view(false);
    view.entry.dir = Some("/repo/card-ux".into());
    view.timing = schedule::TaskTiming::evaluate(
        schedule::TaskShape::compile("sweep", &view.entry).trigger(),
        TaskSource::Config,
        Some(view.now_zoned.timestamp()),
        None,
        &view.now_zoned,
    )
    .with_condition(
        &Verdict {
            ok: false,
            readings: BTreeMap::from([("pr".into(), Some("open".into())), ("ci".into(), None)]),
        },
        None,
        view.now_zoned.timestamp(),
    );
    let text = show_text(&view);
    assert_eq!(
        text.lines().next(),
        Some("sweep · routine · worktree card-ux · waiting · 1 of 2 met"),
        "{text}"
    );
}

#[test]
fn show_keeps_grouped_condition_and_hold_once_for_each_scope() {
    for each in [false, true] {
        let mut view = show_view(each);
        view.entry.when = Some(vec!["pr=open || (ci=passed && !pr.queue=queued)".into()]);
        view.entry.hold = Some("5m".into());
        view.timing = schedule::TaskTiming::evaluate(
            schedule::TaskShape::compile("sweep", &view.entry).trigger(),
            TaskSource::Config,
            None,
            None,
            &view.now_zoned,
        );
        let text = show_text(&view);
        let expression = "when pr=open || ci=passed && !pr.queue=queued, for 5m";
        assert_eq!(text.matches(expression).count(), 1, "{text}");
        let following = if each { "WORKTREES" } else { "pr=open   ✓" };
        assert!(
            text.find(expression).unwrap() < text.find(following).unwrap(),
            "{text}"
        );
    }
}

#[test]
fn show_keeps_recent_overlapping_runs() {
    let mut view = show_view(false);
    view.records = vec![
        record(10, LoopRunResult::Overlapped),
        record(20, LoopRunResult::Completed),
    ];
    let text = show_text(&view);
    assert!(text.contains("RECENT RUNS"), "{text}");
    assert!(text.contains("skipped"), "{text}");
}

#[test]
fn worktrees_unread_cells_are_unknown_with_a_reason() {
    use schedule::fanout::CheckoutState;
    let mut view = worktree_view();
    view.worktrees.retain(|row| row.name == "ready");
    let CheckoutState::Ready { verdict, .. } = &mut view.worktrees[0].state else {
        panic!("ready fixture")
    };
    verdict.readings.insert("ci".into(), None);
    let text = show_text(&view);
    assert!(text.contains("? not read"), "{text}");
    assert!(!text.contains("✗ unknown"), "{text}");
}

#[test]
fn show_keeps_folded_evidence_but_omits_empty_single_run_detail() {
    let mut view = show_view(false);
    view.records = [(10, 900), (20, 450_489), (30, 900)]
        .into_iter()
        .map(|(second, duration_ms)| {
            let mut run = scheduled(second, LoopRunResult::Completed);
            run.duration_ms = Some(duration_ms);
            run.check = Some(check(Some(0), ""));
            run
        })
        .collect();
    let text = show_text(&view);
    assert!(text.contains("RECENT RUNS"), "{text}");
    assert!(
        text.contains("✓ completed ×3") && text.contains("900ms–7m") && text.contains("over 20s"),
        "{text}"
    );
    assert!(!text.contains("LAST RUN"), "{text}");

    view.records.truncate(1);
    let text = show_text(&view);
    assert!(!text.contains("RECENT RUNS"), "{text}");
    assert!(!text.contains("LAST RUN"), "{text}");
}

#[test]
fn show_header_leads_with_the_finished_verdict_before_waiting_fires() {
    use rimz::config::OverlapPolicy;
    let cases = [
        (
            OverlapPolicy::Coalesce,
            1,
            5,
            "  6 fires waiting · one run follows when the active run ends",
        ),
        (
            OverlapPolicy::Queue,
            3,
            0,
            "  3 fires queued (limit 10) · each runs in turn when the active run ends",
        ),
        (
            OverlapPolicy::Replace,
            1,
            0,
            "  1 fire waiting · one run follows when the active run ends · stopping pid 77",
        ),
    ];
    let actual = cases.map(|(policy, count, absorbed, _)| {
        let mut view = show_view(false);
        let mut completed = scheduled(20, LoopRunResult::Completed);
        completed.duration_ms = Some(10);
        view.records = vec![completed];
        view.entry.overlap = Some(policy);
        view.entry.overlap_limit = Some(10);
        view.in_flight = Some(InFlightRun {
            holder: None,
            run: None,
            held: Vec::new(),
        });
        let triggers = vec![
            serde_json::json!({
                "arrived_at": "2026-01-01T00:00:00Z", "signal": null, "absorbed": absorbed
            });
            count
        ];
        let stopping = (policy == OverlapPolicy::Replace).then_some(schedule::backlog::StopMark {
            pid: 77,
            started_at: Timestamp::from_second(0).unwrap(),
            cause: schedule::backlog::StopCause::Replace,
        });
        view.backlog = Some(
            serde_json::from_value(serde_json::json!({
                "triggers": triggers, "stopping": stopping
            }))
            .unwrap(),
        );
        show_text(&view)
            .lines()
            .take_while(|line| !line.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    });
    let expected = cases.map(|(_, _, _, waiting)| {
        vec![
            "sweep · routine · ▸ running".to_owned(),
            "  ✓ healthy · last finished run 1m ago, completed in 10ms".to_owned(),
            waiting.to_owned(),
            "  stop with `rimz loop stop sweep`".to_owned(),
        ]
    });
    assert_eq!(actual, expected);
}

#[test]
fn last_run_ignores_fifty_heard_siblings() {
    let mut completed = record(10, LoopRunResult::Completed);
    completed.check = Some(check(Some(0), "ok"));
    let mut records = vec![completed];
    for second in 11..61 {
        let mut heard = record(second, LoopRunResult::SignalSkipped);
        heard.signal = Some(run_log::SignalRecord {
            name: "ci.passed".parse().unwrap(),
            payload: serde_json::Map::new(),
        });
        records.push(heard);
    }
    assert!(last_run(&records).contains("LAST RUN — ✓ completed"));
}

#[test]
fn empty_history_hints_only_for_clock_triggers() {
    for entry in [
        TaskEntry {
            when: Some(vec!["pr=open".into()]),
            ..Default::default()
        },
        TaskEntry {
            signal: Some("ci.failed".into()),
            ..Default::default()
        },
        TaskEntry {
            watch: Some(rimz::config::WatchSpec::Command("true".into())),
            ..Default::default()
        },
        TaskEntry {
            every: Some("1h".into()),
            ..Default::default()
        },
    ] {
        let mut out = Vec::new();
        write_empty_history(&mut out, "sweep", Some(&entry)).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(
            text.contains("try `rimz loop fire sweep`"),
            entry.every.is_some(),
            "{text}"
        );
        assert!(text.starts_with("no runs recorded"), "{text}");
    }
}

#[test]
fn empty_delivery_history_names_the_wake_target_without_a_fire_hint() {
    for trigger in ["clock", "signal", "watch"] {
        let entry = TaskEntry {
            wait: Some(TaskTarget {
                kind: rimz::ids::AgentKind::new_unchecked("claude"),
                session: "sess-planner".into(),
                handle: "@planner#topic".into(),
            }),
            fire_at: (trigger == "clock").then(|| "2030-01-01T00:00:00Z".parse().unwrap()),
            signal: (trigger == "signal").then(|| "ci.failed".into()),
            watch: (trigger == "watch").then(|| rimz::config::WatchSpec::Command("true".into())),
            ..TaskEntry::default()
        };
        let mut out = Vec::new();
        write_empty_history(&mut out, "wake", Some(&entry)).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "wakes @planner#topic; not fired yet\n",
            "{trigger}"
        );
    }
}

#[test]
fn watch_timeout_is_a_check_in() {
    let mut view = show_view(false);
    view.entry.watch = Some(rimz::config::WatchSpec::Command("true".into()));
    view.entry.timeout = Some("5m".into());
    let text = show_text(&view);
    assert!(
        text.contains("check-in:") && !text.contains("timeout:"),
        "{text}"
    );
}

#[test]
fn logs_name_signal_or_manual_trigger() {
    let mut row = record(10, LoopRunResult::Completed);
    row.signal = Some(run_log::SignalRecord {
        name: "ci.failed".parse().unwrap(),
        payload: serde_json::Map::new(),
    });
    for (mode, trigger) in [
        (LoopRunMode::Scheduled, "signal ci.failed"),
        (LoopRunMode::Manual, "manual"),
    ] {
        row.mode = Some(mode);
        let mut out = Vec::new();
        write_log_records(&mut out, None, &[&row]).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.lines().next().unwrap().contains(trigger), "{text}");
    }
}

#[test]
fn show_fanout_verdict_precedes_worktrees_and_has_no_subscriptions_section() {
    let view = show_view(true);
    let text = show_text(&view);
    assert!(
        text.lines()
            .nth(1)
            .is_some_and(|line| line.starts_with("  ✗ failing")),
        "{text}"
    );
    assert!(
        text.lines().next().unwrap().ends_with(" · each worktree"),
        "{text}"
    );
    let sections = ["WORKTREES (2 owned · 2 launched)", "LAST RUN", "  action:"];
    let positions = sections.map(|section| text.find(section).expect(section));
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{text}");
    assert!(!text.contains("SUBSCRIPTIONS"), "{text}");
    assert!(!text.contains("AGENT RUNS"), "{text}");
    assert!(text.contains("… 2 launched: 2 ended"), "{text}");
    assert!(show_text(&show_view(false)).contains("AGENT RUNS"));
    assert!(
        !text.contains("LAUNCHES") && !text.contains("condition:"),
        "{text}"
    );
    assert!(
        !text.contains("/repo/newer") && !text.contains("1970-"),
        "{text}"
    );
}

fn worktree_view() -> ShowView {
    use schedule::fanout::{CheckoutCondition, CheckoutState};
    let mut view = show_view(true);
    view.now_zoned = Timestamp::from_second(3600)
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::UTC);
    let verdict = |pr: Option<&str>, ci: Option<&str>| schedule::when::Verdict {
        ok: pr == Some("open") && ci == Some("passed"),
        readings: BTreeMap::from([
            ("pr".into(), pr.map(str::to_owned)),
            ("ci".into(), ci.map(str::to_owned)),
        ]),
    };
    let since = Timestamp::from_second(3480).unwrap();
    view.worktrees = [
        (
            "declined-new",
            CheckoutState::Declined {
                at: since,
                reason: "Nothing actionable.\nA whole judge paragraph.".into(),
                profile: "haiku".into(),
                recheck_at: Some(Timestamp::from_second(10800).unwrap()),
            },
        ),
        (
            "declined-old",
            CheckoutState::Declined {
                at: Timestamp::from_second(10).unwrap(),
                reason: "Already handled.".into(),
                profile: "haiku".into(),
                recheck_at: None,
            },
        ),
        (
            "running",
            CheckoutState::Launched {
                leader: "running-leader".into(),
                at: Timestamp::from_second(3599).unwrap(),
            },
        ),
        (
            "paused",
            CheckoutState::Launched {
                leader: "paused-leader".into(),
                at: since,
            },
        ),
        (
            "success",
            CheckoutState::Launched {
                leader: "success-leader".into(),
                at: Timestamp::from_second(3500).unwrap(),
            },
        ),
        (
            "ended",
            CheckoutState::Launched {
                leader: "ended-leader".into(),
                at: since,
            },
        ),
        (
            "waiting-many",
            CheckoutState::Waiting {
                verdict: verdict(None, Some("failed")),
            },
        ),
        (
            "older",
            CheckoutState::Launched {
                leader: "old-leader".into(),
                at: Timestamp::from_second(10).unwrap(),
            },
        ),
        (
            "waiting-one",
            CheckoutState::Waiting {
                verdict: verdict(Some("open"), Some("failed")),
            },
        ),
        (
            "holding",
            CheckoutState::Holding {
                since,
                hold: Duration::from_secs(180),
                verdict: verdict(Some("open"), Some("passed")),
            },
        ),
        (
            "newer",
            CheckoutState::Launched {
                leader: "new-leader".into(),
                at: since,
            },
        ),
        (
            "blocked",
            CheckoutState::Ready {
                since,
                verdict: verdict(Some("open"), Some("passed")),
            },
        ),
        (
            "ready",
            CheckoutState::Ready {
                since,
                verdict: verdict(Some("open"), Some("passed")),
            },
        ),
    ]
    .map(|(name, state)| CheckoutCondition {
        checkout: format!("/repo/{name}").into(),
        name: name.into(),
        state,
    })
    .into();
    let mut refusal = record(1800, LoopRunResult::TakeoverBlocked);
    refusal.checkout = Some("/repo/blocked".into());
    refusal.error = Some("@busy is working".into());
    view.records = vec![refusal];
    view.leader_statuses = BTreeMap::from([
        ("old-leader".into(), rimz::agents::AgentStatus::Success),
        ("new-leader".into(), rimz::agents::AgentStatus::Failed),
        ("running-leader".into(), rimz::agents::AgentStatus::Running),
        ("paused-leader".into(), rimz::agents::AgentStatus::Paused),
        ("success-leader".into(), rimz::agents::AgentStatus::Success),
    ]);
    view
}

#[test]
fn show_worktrees_fresh_hold_renders_zero_elapsed() {
    let mut view = worktree_view();
    let row = view
        .worktrees
        .iter_mut()
        .find(|row| row.name == "holding")
        .unwrap();
    let CheckoutState::Holding { since, .. } = &mut row.state else {
        panic!("holding fixture must carry a hold");
    };
    *since = view.now_zoned.timestamp();
    let text = show_text(&view);
    assert!(text.contains("holding 0s/3m"), "{text}");
}

#[test]
fn worktrees_repeated_key_readings_have_no_leaf_glyph() {
    let mut view = worktree_view();
    view.entry.when = Some(vec!["pr=open || pr=merged && ci=passed".into()]);
    view.timing = schedule::TaskTiming::evaluate(
        schedule::TaskShape::compile("sweep", &view.entry).trigger(),
        view.source,
        None,
        None,
        &view.now_zoned,
    );
    view.room_is_open = true;
    view.worktrees.retain(|row| row.name == "ready");
    let CheckoutState::Ready { verdict, .. } = &mut view.worktrees[0].state else {
        panic!("ready fixture must carry a verdict");
    };
    verdict.readings.insert("pr".into(), Some("merged".into()));
    let text = show_text(&view);
    let row = text
        .lines()
        .find(|line| line.starts_with("  ready "))
        .unwrap();
    assert!(
        row.contains("merged") && !row.contains("✗ merged") && !row.contains("✓ merged"),
        "{text}"
    );
    assert!(row.contains("✓ passed"), "{text}");
}

#[test]
fn negated_leaf_glyphs_and_short_read_through_the_negation() {
    let mut view = worktree_view();
    view.entry.when = Some(vec!["pr=open && !ci=pending,failed".into()]);
    view.timing = schedule::TaskTiming::evaluate(
        schedule::TaskShape::compile("sweep", &view.entry).trigger(),
        view.source,
        None,
        None,
        &view.now_zoned,
    );
    view.room_is_open = true;
    view.worktrees.retain(|row| row.name == "ready");
    let ready_row = |view: &ShowView| {
        let text = show_text(view);
        assert!(
            text.lines()
                .any(|line| line.contains("WORKTREE ") && line.contains("!ci")),
            "{text}"
        );
        text.lines()
            .find(|line| line.starts_with("  ready "))
            .unwrap()
            .to_owned()
    };
    let short =
        |view: &ShowView| serde_json::to_value(worktrees_json(view)).unwrap()[0]["short"].clone();
    let row = ready_row(&view);
    assert!(row.contains("✓ open") && row.contains("✓ passed"), "{row}");
    assert_eq!(short(&view), serde_json::json!([]));

    let CheckoutState::Ready { verdict, .. } = &mut view.worktrees[0].state else {
        panic!("ready fixture must carry a verdict");
    };
    verdict.readings.insert("ci".into(), Some("failed".into()));
    let row = ready_row(&view);
    assert!(row.contains("✗ failed"), "{row}");
    assert_eq!(short(&view), serde_json::json!(["ci"]));

    let expr = schedule::when::WhenExpr::parse(&["pr=open && !ci=pending,failed".into()]).unwrap();
    let CheckoutState::Ready { verdict, .. } = &view.worktrees[0].state else {
        panic!("ready fixture must carry a verdict");
    };
    let mut out = Vec::new();
    super::super::condition::write_terms(&mut out, &view.entry, &expr, verdict).unwrap();
    assert_eq!(
        anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string(),
        "  pr=open   ✓ open\n  !ci=pending,failed   ✗ failed\n"
    );
    let mut unread = verdict.clone();
    unread.readings.insert("ci".into(), None);
    let mut out = Vec::new();
    super::super::condition::write_terms(&mut out, &view.entry, &expr, &unread).unwrap();
    assert!(
        anstream::adapter::strip_str(&String::from_utf8(out).unwrap())
            .to_string()
            .contains("!ci=pending,failed   ✓ readings paused (room closed)")
    );
    unread.readings.insert("pr".into(), None);
    let mut out = Vec::new();
    super::super::condition::write_terms(&mut out, &view.entry, &expr, &unread).unwrap();
    assert!(
        anstream::adapter::strip_str(&String::from_utf8(out).unwrap())
            .to_string()
            .contains("pr=open   ? readings paused (room closed)")
    );
}

#[test]
fn worktrees_order_counts_terms_and_fold_lines() {
    let view = worktree_view();
    let text = show_text(&view);
    assert!(
        text.contains(
            "WORKTREES (13 owned · 1 blocked · 1 ready · 1 holding · 2 waiting · 2 declined · 6 launched)"
        ),
        "{text}"
    );
    let table = text
        .split("WORKTREES")
        .nth(1)
        .unwrap()
        .split("RECENT RUNS")
        .next()
        .unwrap();
    let positions = [
        "  blocked ",
        "  ready ",
        "  holding ",
        "  running ",
        "  newer ",
        "  waiting-many ",
        "  waiting-one ",
    ]
    .map(|name| table.find(name).expect(name));
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{text}");
    assert!(table.contains("ready · blocked 30m ago"), "{text}");
    assert!(table.contains("holding 2m/3m"), "{text}");
    let header = table
        .lines()
        .find(|line| line.contains("WORKTREE "))
        .unwrap();
    assert!(
        header.find("pr").unwrap() < header.find("ci").unwrap(),
        "{text}"
    );
    let waiting = table
        .lines()
        .find(|line| line.starts_with("  waiting-one "))
        .unwrap();
    assert!(
        waiting.contains("✓ open") && waiting.contains("✗ failed"),
        "{text}"
    );
    let blocked = table
        .lines()
        .find(|line| line.starts_with("  blocked "))
        .unwrap();
    assert!(
        blocked.contains("@busy is working") && !blocked.contains("✓"),
        "{text}"
    );
    let launched = table
        .lines()
        .find(|line| line.starts_with("  newer "))
        .unwrap();
    assert!(
        launched.contains("@new-leader  failed · 1 armed") && !launched.contains("✓"),
        "{text}"
    );
    assert!(
        table.contains("… 2 declined by judge haiku · next recheck in 2h · newest declined-new 2m ago: Nothing actionable."),
        "{text}"
    );
    assert!(
        table.contains(
            "… 4 launched: 2 success · 1 ended · 1 paused · newest success @success-leader 1m ago"
        ),
        "{text}"
    );
    assert!(
        table.contains("→ reasons and leaders: rimz loop show sweep --all"),
        "{text}"
    );
    assert!(
        header.find("LEADER").unwrap() < header.find("REASON").unwrap(),
        "{text}"
    );
    assert!(
        [
            "ended",
            "older",
            "paused",
            "success",
            "declined-new",
            "declined-old"
        ]
        .iter()
        .all(|name| !table.contains(&format!("  {name} "))),
        "{text}"
    );
    assert!(
        table.contains(
            "ci and pr readings come from the room's sidebar; the loop timer alone never sees them"
        ),
        "{text}"
    );
}

#[test]
fn worktrees_all_unfolds_declined_and_launched_rows() {
    let mut view = worktree_view();
    view.all = true;
    view.room_is_open = true;
    let text = show_text(&view);
    assert!(text.contains("WORKTREES"), "{text}");
    let table = text
        .split("WORKTREES")
        .nth(1)
        .unwrap()
        .split("RECENT RUNS")
        .next()
        .unwrap();
    assert!(
        !table.lines().any(|line| line.trim_start().starts_with('…'))
            && !table.contains("→ reasons and leaders:"),
        "{text}"
    );
    let many = table
        .lines()
        .find(|line| line.starts_with("  waiting-many "))
        .expect("unfolded waiting row");
    assert!(
        many.contains("? not checked yet") && many.contains("✗ failed"),
        "{text}"
    );
    assert!(
        table.find("  waiting-many ").unwrap() < table.find("  waiting-one ").unwrap(),
        "{text}"
    );
    assert!(
        table.find("  waiting-one ").unwrap() < table.find("  declined-new ").unwrap()
            && table.find("  declined-new ").unwrap() < table.find("  declined-old ").unwrap()
            && table.find("  declined-old ").unwrap() < table.find("  success ").unwrap()
            && table.find("  success ").unwrap() < table.find("  ended ").unwrap()
            && table.find("  ended ").unwrap() < table.find("  paused ").unwrap()
            && table.find("  paused ").unwrap() < table.find("  older ").unwrap(),
        "{text}"
    );
    assert!(table.contains("@ended-leader ended"), "{text}");
    for row in &view.worktrees {
        assert!(table.contains(&format!("  {} ", row.name)), "{text}");
    }
    assert!(
        table
            .lines()
            .any(|line| line.contains("LEADER") && line.ends_with("REASON")),
        "{text}"
    );
    assert!(table.contains("judge haiku:"), "{text}");
    assert!(!table.contains("A whole judge paragraph."), "{text}");
    assert!(!table.contains("ci and pr readings come"), "{text}");
}

#[test]
fn show_single_checkout_condition_follows_verdict_and_keeps_subscriptions() {
    let mut view = show_view(false);
    view.launches.clear();
    let text = show_text(&view);
    assert!(
        text.lines()
            .nth(1)
            .is_some_and(|line| line.starts_with("  ✗ failing")),
        "{text}"
    );
    assert_eq!(
        text.lines().nth(2),
        Some("  when pr=open && ci=passed"),
        "{text}"
    );
    assert_eq!(text.lines().nth(3), Some("  pr=open   ✓ open"), "{text}");
    assert!(
        text.find("LAST RUN").unwrap() < text.find("SUBSCRIPTIONS").unwrap(),
        "{text}"
    );
    assert!(
        text.find("SUBSCRIPTIONS").unwrap() < text.find("  action:").unwrap(),
        "{text}"
    );
}

#[test]
fn worktrees_declined_fold_names_next_recheck_and_newest_reason() {
    let view = worktree_view();
    let text = show_text(&view);
    assert!(text.contains("… 2 declined by judge haiku · next recheck in 2h · newest declined-new 2m ago: Nothing actionable."), "{text}");
    assert!(!text.contains("A whole judge paragraph."), "{text}");
}

#[test]
fn worktrees_launched_fold_counts_only_folded_leaders() {
    let view = worktree_view();
    let text = show_text(&view);
    assert!(
        text.contains(
            "… 4 launched: 2 success · 1 ended · 1 paused · newest success @success-leader 1m ago"
        ),
        "{text}"
    );
}

#[test]
fn worktrees_reason_cell_clips_to_terminal_width() {
    use unicode_width::UnicodeWidthStr;
    let mut view = worktree_view();
    view.all = true;
    view.worktrees.retain(|row| row.name == "declined-new");
    let CheckoutState::Declined { reason, .. } = &mut view.worktrees[0].state else {
        panic!("declined fixture");
    };
    *reason = "A long judge paragraph. ".repeat(20);
    let text = show_text(&view);
    let row = text
        .lines()
        .find(|line| line.starts_with("  declined-new "))
        .unwrap();
    assert!(
        row.width() <= ui::terminal_columns(120) && row.ends_with('…'),
        "{text}"
    );
}

#[test]
fn worktrees_declined_fold_without_recheck_names_reopening_and_first_line() {
    let mut view = worktree_view();
    view.worktrees.retain(|row| row.name == "declined-new");
    let CheckoutState::Declined { recheck_at, .. } = &mut view.worktrees[0].state else {
        panic!("declined fixture");
    };
    *recheck_at = None;
    let text = show_text(&view);
    assert!(text.contains("… 1 declined by judge haiku · re-judged when the guard changes · newest declined-new 2m ago:"), "{text}");
    assert!(!text.contains("A whole judge paragraph."), "{text}");
}

#[test]
fn worktrees_declined_fold_clips_reason_after_reopening() {
    use unicode_width::UnicodeWidthStr;
    let mut view = worktree_view();
    view.worktrees.retain(|row| row.name == "declined-new");
    let CheckoutState::Declined { reason, .. } = &mut view.worktrees[0].state else {
        panic!("declined fixture");
    };
    *reason = "A long judge paragraph. ".repeat(20);
    let rows = worktree_rows(&view, worktree_expr(&view));
    let mut out = Vec::new();
    write_worktree_folds(&mut out, &view, &rows, 80).unwrap();
    let text = anstream::adapter::strip_str(std::str::from_utf8(&out).unwrap()).to_string();
    let fold = text.lines().next().expect("declined fold");
    assert!(fold.contains("next recheck in 2h"), "{text}");
    assert!(fold.width() <= 80 && fold.ends_with('…'), "{text}");
}

#[test]
fn worktrees_declined_fold_uses_earliest_recheck_and_sorted_profiles() {
    let mut view = worktree_view();
    let CheckoutState::Declined {
        recheck_at,
        profile,
        ..
    } = &mut view.worktrees[1].state
    else {
        panic!("declined fixture");
    };
    *recheck_at = Some(view.now_zoned.timestamp());
    *profile = "codex".into();
    let text = show_text(&view);
    assert!(
        text.contains(
            "… 2 declined by judges codex, haiku · next recheck due · newest declined-new"
        ),
        "{text}"
    );
}

#[test]
fn worktrees_columns_follow_rendered_rows_and_armed_follows_subscribe() {
    let mut view = worktree_view();
    view.entry.subscribe.clear();
    let text = show_text(&view);
    let launched = text
        .lines()
        .find(|line| line.starts_with("  newer "))
        .unwrap();
    assert!(
        launched.contains("@new-leader  failed") && !launched.contains("armed"),
        "{text}"
    );
    view.worktrees.retain(|row| row.name == "ready");
    let text = show_text(&view);
    let header = text
        .lines()
        .find(|line| line.contains("WORKTREE "))
        .unwrap();
    assert!(
        !header.contains("LEADER") && !header.contains("REASON"),
        "{text}"
    );
    assert!(!text.contains("→ reasons and leaders:"), "{text}");
}

#[test]
fn worktrees_keeps_waiting_leaders_and_folds_idle_and_sleeping() {
    let mut view = worktree_view();
    view.worktrees.retain(|row| row.name == "running");
    for status in [
        rimz::agents::AgentStatus::Waiting,
        rimz::agents::AgentStatus::Idle,
        rimz::agents::AgentStatus::Sleeping,
    ] {
        view.leader_statuses.insert("running-leader".into(), status);
        let text = show_text(&view);
        assert_eq!(
            text.contains("  running "),
            status == rimz::agents::AgentStatus::Waiting,
            "{text}"
        );
        assert_eq!(
            text.contains("… 1 launched:"),
            status != rimz::agents::AgentStatus::Waiting,
            "{text}"
        );
    }
}

#[test]
fn worktrees_json_rechecks_are_optional_and_all_does_not_change_json() {
    let mut view = worktree_view();
    let json = serde_json::to_value(worktrees_json(&view)).unwrap();
    let declined = json
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "declined-new")
        .unwrap();
    assert_eq!(declined["recheck_at"], "1970-01-01T03:00:00Z");
    let old = json
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "declined-old")
        .unwrap();
    assert!(old.get("recheck_at").is_none());
    view.all = true;
    assert_eq!(serde_json::to_value(worktrees_json(&view)).unwrap(), json);
}

#[test]
fn worktrees_json_preserves_the_full_accepted_hold_duration() {
    let mut view = show_view(true);
    view.entry.hold = Some(format!("{}s", u64::MAX));
    let parsed = schedule::parse_trigger(&view.name, &view.entry).unwrap();
    let schedule::Trigger::Condition {
        hold: Some(hold), ..
    } = parsed.trigger
    else {
        panic!("accepted condition hold");
    };
    view.worktrees = vec![schedule::fanout::CheckoutCondition {
        checkout: "/repo/holding".into(),
        name: "holding".into(),
        state: schedule::fanout::CheckoutState::Holding {
            since: view.now_zoned.timestamp(),
            hold,
            verdict: schedule::when::Verdict {
                ok: true,
                readings: BTreeMap::new(),
            },
        },
    }];
    let serialized = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        serde_json::to_string(&worktrees_json(&view))
    }));
    assert!(
        serialized.is_ok(),
        "an accepted hold must serialize without panicking"
    );
    #[derive(serde::Deserialize)]
    struct HoldJson {
        hold_ms: u128,
    }
    let rows: Vec<HoldJson> = serde_json::from_str(&serialized.unwrap().unwrap()).unwrap();
    assert_eq!(rows[0].hold_ms, hold.as_millis());
}

#[test]
fn show_resident_facts_name_action_and_wakes_without_timeout() {
    let mut view = show_view(true);
    view.entry.timeout = Some("1h".into());
    view.entry.account = Some("work".parse().unwrap());
    let text = show_text(&view);
    assert!(
        text.contains("check, then start sweeper · account work · pane stays open · stops idle agents in the checkout first"),
        "{text}"
    );
    assert!(
        text.contains("leader on ci.failed, pr.merged · 1 armed"),
        "{text}"
    );
    assert!(
        !text.contains("timeout:") && !text.contains("  task:"),
        "{text}"
    );
}

#[test]
fn show_wakes_with_no_subscriptions_and_no_live_leader() {
    let mut view = show_view(true);
    view.subscriptions.clear();
    let text = show_text(&view);
    assert!(
        text.contains("leader on ci.failed, pr.merged · 0 armed, no live leader"),
        "{text}"
    );
}

#[test]
fn show_wakes_with_a_live_leader_and_no_subscriptions() {
    let mut view = show_view(true);
    view.subscriptions.clear();
    view.leader_statuses
        .insert("new-leader".into(), rimz::agents::AgentStatus::Idle);
    let text = show_text(&view);
    assert!(
        text.contains("leader on ci.failed, pr.merged · 0 armed"),
        "{text}"
    );
    assert!(!text.contains("no live leader"), "{text}");
}

#[test]
fn show_check_only_action_and_timeout_for_nonresident_agents() {
    let view = show_view(false);
    assert!(show_text(&view).contains("timeout:"));
    let mut view = view;
    view.entry.agent = None;
    view.entry.when = None;
    view.entry.subscribe.clear();
    view.entry.every = Some("1h".into());
    let text = show_text(&view);
    assert!(
        text.lines()
            .any(|line| line.trim_start().starts_with("action:")
                && line.ends_with("run check · true")),
        "{text}"
    );
    assert!(
        !text.contains("  check:") && !text.contains("timeout:"),
        "{text}"
    );
}

#[test]
fn show_without_limits_omits_even_an_explicit_throttle_switch() {
    let mut view = show_view(false);
    view.entry.agent = None;
    view.entry.subscribe.clear();
    view.throttle = ShowThrottle::Off;
    let text = show_text(&view);
    assert!(
        !text.contains("throttle:") && !text.contains("load:") && !text.contains("THROTTLE"),
        "{text}"
    );
    for switch in [
        rimz::config::ThrottleSwitch::On,
        rimz::config::ThrottleSwitch::Off,
    ] {
        view.entry.throttle = Some(switch);
        let text = show_text(&view);
        assert!(
            !text.contains("throttle:") && !text.contains("load:") && !text.contains("THROTTLE"),
            "{text}"
        );
    }
}

#[test]
fn show_facts_follow_the_reference_order() {
    let mut view = show_view(false);
    view.source = TaskSource::Project {
        state: rimz::trust::TrustState::Untrusted,
    };
    view.entry.label = Some("repair the build".into());
    view.entry.verify = Some("cargo check".into());
    view.entry.dir = Some("/repo/lane".into());
    view.entry.budget = Some("$5".into());
    view.entry.budget_per_day = Some("$20".into());
    view.entry.surplus = Some("1.5x".into());
    view.strike_count = 1;
    let mut out = Vec::new();
    write_show_facts(&mut out, &view).unwrap();
    let text = String::from_utf8(out).unwrap();
    let text = anstream::adapter::strip_str(&text).to_string();
    let keys = text
        .lines()
        .map(|line| line.trim_start().split_once(':').unwrap().0)
        .collect::<Vec<_>>();
    assert_eq!(
        keys,
        [
            "action",
            "check",
            "wakes",
            "label",
            "verify",
            "root",
            "dir",
            "source",
            "timeout",
            "will not fire",
            "budget",
            "surplus",
            "spend",
            "strikes",
        ],
        "{text}"
    );
}

#[test]
fn show_no_limits_omits_throttle_and_load() {
    let text = show_text(&show_view(true));
    assert!(
        !text.contains("throttle:") && !text.contains("load:"),
        "{text}"
    );
    assert!(!text.contains("THROTTLE"), "{text}");
}

#[test]
fn show_configured_limits_keep_full_throttle_section() {
    let mut view = show_view(true);
    view.throttle = ShowThrottle::Full(vec![("cpu pressure", "avg10 2% (limit 25%)".into())]);
    let text = show_text(&view);
    assert!(text.contains("THROTTLE\n"), "{text}");
    assert!(!text.contains("no limits set"), "{text}");
}

fn refusal(second: i64, checkout: &str, reason: &str) -> LoopRunRecord {
    let mut row = scheduled(second, LoopRunResult::TakeoverBlocked);
    row.checkout = Some(PathBuf::from(checkout));
    row.error = Some(reason.into());
    row
}

#[test]
fn takeover_refusals_fold_alternating_checkouts_with_latest_reason_and_oldest_age() {
    let records = [
        refusal(10, "/repo/alpha", "@old is working"),
        refusal(20, "/repo/beta", "@beta is waiting on you"),
        refusal(70, "/repo/alpha", "@alpha is compacting"),
        refusal(80, "/repo/beta", "@beta is working"),
    ];
    let text = runs_table(&records, 10);
    assert_eq!(text.matches("○ takeover blocked ×2").count(), 2, "{text}");
    assert!(
        text.contains("beta · @beta is working · since 1m ago"),
        "{text}"
    );
    assert!(
        text.contains("alpha · @alpha is compacting · since 1m ago"),
        "{text}"
    );
    assert!(
        text.find("beta ·").unwrap() < text.find("alpha ·").unwrap(),
        "{text}"
    );
    assert!(text.contains("4 of 4"), "{text}");
    assert!(!text.contains("/repo/") && !text.contains("@old"), "{text}");
    let limited = runs_table(&records, 1);
    assert!(
        limited.contains("2 of 4") && limited.contains("beta ·"),
        "{limited}"
    );
    assert!(!limited.contains("alpha ·"), "{limited}");
}

#[test]
fn takeover_refusal_folding_preserves_other_rows_and_overlap_counts() {
    let mut completed = scheduled(30, LoopRunResult::Completed);
    completed.checkout = Some("/repo/alpha".into());
    let alpha_overlap = |second| {
        let mut row = overlap(second);
        row.checkout = Some("/repo/alpha".into());
        row
    };
    let records = [
        alpha_overlap(5),
        refusal(10, "/repo/alpha", "@old is working"),
        completed,
        scheduled(40, LoopRunResult::Failed),
        scheduled(50, LoopRunResult::Failed),
        alpha_overlap(60),
        refusal(70, "/repo/alpha", "@new is waiting on you"),
        scheduled(80, LoopRunResult::Failed),
    ];
    let text = runs_table(&records, 10);
    assert_eq!(text.matches("takeover blocked ×1").count(), 2, "{text}");
    assert!(
        text.contains("alpha · @new is waiting on you · since 30s ago · 1 fire skipped"),
        "{text}"
    );
    assert!(
        text.contains("alpha · @old is working · since 1m ago · 1 fire skipped"),
        "{text}"
    );
    assert!(text.contains("✗ failed ×2"), "{text}");
    assert_eq!(text.matches("✗ failed").count(), 2, "{text}");
    assert!(text.contains("8 of 8"), "{text}");
    let limited = runs_table(&records, 2);
    assert!(limited.contains("3 of 8"), "{limited}");
}

#[test]
fn takeover_refusal_runs_have_separate_counts_and_ages_matching_current_block() {
    let mut launch = scheduled(30, LoopRunResult::Launched);
    launch.checkout = Some("/repo/alpha".into());
    launch.target = Some("@leader".into());
    let records = [
        refusal(10, "/repo/alpha", "@old is working"),
        refusal(20, "/repo/alpha", "@old is working"),
        launch,
        refusal(70, "/repo/alpha", "@new is working"),
        refusal(80, "/repo/alpha", "@new is working"),
        refusal(90, "/repo/alpha", "@new is working"),
    ];
    let text = runs_table(&records, 10);
    assert!(text.contains("○ takeover blocked ×3"), "{text}");
    assert!(text.contains("○ takeover blocked ×2"), "{text}");
    let newest = text
        .find("alpha · @new is working · since 30s ago")
        .unwrap();
    let launched = text.find("✓ launched").unwrap();
    let oldest = text.find("alpha · @old is working · since 1m ago").unwrap();
    assert!(newest < launched && launched < oldest, "{text}");
    assert!(text.contains("6 of 6"), "{text}");
    let blocks = takeover_refusals(&records);
    let block = &blocks[Path::new("/repo/alpha")];
    assert_eq!(block.count, 3);
    assert_eq!(
        block.active_since,
        Some(Timestamp::from_second(70).unwrap())
    );
}

#[test]
fn takeover_refusal_run_ends_at_an_overlapped_fire_in_the_same_checkout() {
    let mut skipped = overlap(50);
    skipped.checkout = Some("/repo/alpha".into());
    let text = runs_table(
        &[
            refusal(10, "/repo/alpha", "@old is working"),
            refusal(20, "/repo/alpha", "@old is working"),
            skipped,
            refusal(70, "/repo/alpha", "@new is working"),
            refusal(80, "/repo/alpha", "@new is working"),
        ],
        10,
    );
    assert_eq!(text.matches("○ takeover blocked ×2").count(), 2, "{text}");
    assert!(
        text.contains("alpha · @new is working · since 30s ago · 1 fire skipped"),
        "{text}"
    );
    assert!(
        text.contains("alpha · @old is working · since 1m ago"),
        "{text}"
    );
    assert!(text.contains("5 of 5"), "{text}");
}

#[test]
fn show_attention_warns_at_30m_only_for_current_unledgered_blocks() {
    let mut view = show_view(true);
    view.now_zoned = Timestamp::from_second(4000)
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::UTC);
    let mut cleared = scheduled(2100, LoopRunResult::Completed);
    cleared.checkout = Some("/repo/retried".into());
    let mut resolved = cleared.clone();
    resolved.checkout = Some("/repo/resolved".into());
    view.records = vec![
        scheduled(5, LoopRunResult::Launched),
        refusal(10, "/repo/retried", "@busy is working"),
        refusal(20, "/repo/resolved", "@busy is working"),
        refusal(30, "/repo/newer", "@busy is working"),
        cleared,
        resolved,
        refusal(2200, "/repo/alpha", "@busy is working"),
        refusal(2200, "/repo/beta", "@busy is working"),
        refusal(2260, "/repo/retried", "@busy is working"),
        refusal(3900, "/repo/alpha", "@busy is compacting"),
    ];
    view.worktrees.extend(
        ["alpha", "beta", "retried", "resolved"].map(|name| CheckoutCondition {
            checkout: format!("/repo/{name}").into(),
            name: name.into(),
            state: CheckoutState::Ready {
                since: Timestamp::from_second(2100).unwrap(),
                verdict: Verdict {
                    ok: true,
                    readings: BTreeMap::from([
                        ("pr".into(), Some("open".into())),
                        ("ci".into(), Some("passed".into())),
                    ]),
                },
            },
        }),
    );
    let text = show_text(&view);
    assert_eq!(
        text.lines().nth(2),
        Some("  ! 2 ready but blocked over 30m: alpha, beta"),
        "{text}"
    );
    assert!(
        text.lines().nth(1).unwrap().starts_with("  ✓ healthy"),
        "{text}"
    );
    view.now_zoned = Timestamp::from_second(3940)
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::UTC);
    assert!(!show_text(&view).contains("ready but blocked over 30m"));
}

#[test]
fn show_fanout_refusals_do_not_override_waiting_conditions() {
    let mut view = worktree_view();
    view.worktrees.retain(|row| row.name == "blocked");
    view.worktrees[0].state = CheckoutState::Waiting {
        verdict: Verdict {
            ok: false,
            readings: BTreeMap::from([
                ("pr".into(), Some("open".into())),
                ("ci".into(), Some("failed".into())),
            ]),
        },
    };
    let text = show_text(&view);
    assert!(text.contains("WORKTREES (1 owned · 1 waiting)"), "{text}");
    assert!(!text.contains("ready but blocked over 30m"), "{text}");
    let row = text
        .lines()
        .find(|line| line.starts_with("  blocked "))
        .unwrap();
    assert!(
        row.contains("waiting") && row.contains("✗ failed"),
        "{text}"
    );
    let json = serde_json::to_value(worktrees_json(&view)).unwrap();
    assert_eq!(json[0]["state"], "waiting");
    assert!(json[0].get("blocked").is_none(), "{json}");
}

#[test]
fn show_fanout_refusals_do_not_override_holding_conditions() {
    let mut view = worktree_view();
    view.worktrees.retain(|row| row.name == "holding");
    view.records = vec![refusal(1800, "/repo/holding", "@busy is working")];
    let text = show_text(&view);
    assert!(text.contains("WORKTREES (1 owned · 1 holding)"), "{text}");
    assert!(text.contains("holding 2m/3m"), "{text}");
    assert!(!text.contains("ready but blocked over 30m"), "{text}");
    let json = serde_json::to_value(worktrees_json(&view)).unwrap();
    assert_eq!(json[0]["state"], "holding");
    assert!(json[0].get("blocked").is_none(), "{json}");
}

#[test]
fn show_fanout_refusals_on_unowned_checkouts_do_not_warn() {
    let mut view = worktree_view();
    view.worktrees.retain(|row| row.name == "ready");
    let text = show_text(&view);
    assert!(!text.contains("ready but blocked over 30m"), "{text}");
    assert!(text.contains("WORKTREES (1 owned · 1 ready)"), "{text}");
    let json = serde_json::to_value(worktrees_json(&view)).unwrap();
    assert_eq!(json.as_array().unwrap().len(), 1);
    assert_eq!(json[0]["name"], "ready");
    assert!(json[0].get("blocked").is_none(), "{json}");

    view.entry.each_worktree = false;
    assert!(show_text(&view).contains("! 1 ready but blocked over 30m: blocked"));
}

#[test]
fn show_attention_names_failed_live_leaders_but_not_ended_leaders() {
    let mut view = show_view(true);
    view.records = vec![scheduled(20, LoopRunResult::Launched)];
    let mut failed = rimz::testkit::agent_state("codex", "live", view.now_zoned.timestamp());
    failed.name = Some("new-leader".into());
    failed.status = rimz::agents::AgentStatus::Failed;
    view.leader_statuses = leader_statuses(&view.launches, &[failed]);
    let text = show_text(&view);
    assert_eq!(
        text.lines().nth(2),
        Some("  ! 1 failed leader: @new-leader in newer"),
        "{text}"
    );
    assert!(
        text.lines().nth(1).unwrap().starts_with("  ✓ healthy"),
        "{text}"
    );
    view.leader_statuses
        .insert("old-leader".into(), rimz::agents::AgentStatus::Failed);
    view.records
        .insert(0, refusal(-1800, "/repo/blocked", "@busy is working"));
    view.worktrees.push(CheckoutCondition {
        checkout: "/repo/blocked".into(),
        name: "blocked".into(),
        state: CheckoutState::Ready {
            since: view.now_zoned.timestamp(),
            verdict: Verdict {
                ok: true,
                readings: BTreeMap::new(),
            },
        },
    });
    let text = show_text(&view);
    assert_eq!(
        text.lines().nth(2),
        Some("  ! 1 ready but blocked over 30m: blocked"),
        "{text}"
    );
    assert_eq!(
        text.lines().nth(3),
        Some("  ! 2 failed leaders: @new-leader in newer, @old-leader in older"),
        "{text}"
    );
    view.leader_statuses.clear();
    let text = show_text(&view);
    assert!(!text.contains("failed leader"), "{text}");
}

#[test]
fn leader_lookup_uses_ledger_petnames_and_effective_status() {
    let view = show_view(true);
    let mut live = rimz::testkit::agent_state("codex", "live", view.now_zoned.timestamp());
    live.name = Some("new-leader".into());
    live.profile = Some("sweeper".into());
    live.status = rimz::agents::AgentStatus::Running;
    live.phase = rimz::agents::TurnPhase::Parked;
    let mut unrelated = live.clone();
    unrelated.name = Some("sweeper".into());
    unrelated.status = rimz::agents::AgentStatus::Failed;
    let statuses = leader_statuses(&view.launches, &[live, unrelated]);
    assert_eq!(
        statuses,
        BTreeMap::from([("new-leader".into(), rimz::agents::AgentStatus::Success)])
    );
}

#[test]
fn true_condition_without_hold_renders_due() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".rimz/team")).unwrap();
    std::fs::write(
        root.path().join(".rimz/team/blackboard.md"),
        "Stage: Done\n",
    )
    .unwrap();
    let entry = TaskEntry {
        root: root.path().to_owned(),
        when: Some(vec!["team.stage=Done".to_owned()]),
        check: Some("true".into()),
        ..TaskEntry::default()
    };
    let parsed = schedule::parse_trigger("ready", &entry);
    let mut out = Vec::new();
    super::super::condition::write_receipt(&mut out, &entry, parsed.as_ref().unwrap()).unwrap();
    assert!(String::from_utf8(out).unwrap().contains("now: due\n"));
    let now = Timestamp::now();
    let timing = schedule::TaskTiming::evaluate(
        &parsed,
        schedule::catalog::TaskSource::Config,
        Some(now),
        None,
        &now.to_zoned(jiff::tz::TimeZone::UTC),
    );
    let timing = super::super::condition::observe("ready", &entry, timing, now);
    let mut out = Vec::new();
    super::super::condition::write_show(&mut out, &entry, &timing).unwrap();
    assert!(
        anstream::adapter::strip_str(&String::from_utf8(out).unwrap())
            .to_string()
            .contains("team.stage=Done   ✓ Done")
    );
}

fn record(second: i64, result: LoopRunResult) -> LoopRunRecord {
    LoopRunRecord {
        kind: None,
        judge: Vec::new(),
        checkout: None,
        task: "wait".to_owned(),
        root: None,
        at: Timestamp::from_second(second).expect("timestamp"),
        result,
        mode: None,
        duration_ms: None,
        throttle_wait_ms: None,
        holder: None,
        run_lock: None,
        absorbed: 0,
        error: None,
        check: None,
        watch: None,
        signal: None,
        condition: None,
        message_id: None,
        run_id: None,
        transcript_path: None,
        last_message: None,
        target: None,
        cost_usd: None,
        input_tokens: None,
        output_tokens: None,
    }
}

fn check(code: Option<i32>, output: &str) -> CheckRecord {
    CheckRecord {
        duration_ms: None,
        agent: None,
        output_path: None,
        code,
        timed_out: false,
        output: output.to_owned(),
    }
}

#[test]
fn agent_check_skips_name_a_decline_without_changing_passed_labels() {
    let mut row = record(1, LoopRunResult::CheckSkipped);
    let mut check = check(Some(1), "No work.");
    check.agent = Some(serde_json::from_str(r#"{"profile":"haiku","kind":"claude","model":null,"verdict":{"pass":false,"reason":"No work."},"error":null,"cost_usd":0.02,"input_tokens":10,"output_tokens":5}"#).unwrap());
    row.check = Some(check);
    assert_eq!(check_skipped_label(&row), "judge declined");
    row.check
        .as_mut()
        .unwrap()
        .agent
        .as_mut()
        .unwrap()
        .verdict
        .as_mut()
        .unwrap()
        .pass = true;
    assert_eq!(check_skipped_label(&row), "judge passed");
}

fn overlap(second: i64) -> LoopRunRecord {
    let mut row = record(second, LoopRunResult::Overlapped);
    row.mode = Some(LoopRunMode::Scheduled);
    row.duration_ms = Some(0);
    row.holder = Some(RunLockInfo {
        pid: 42424,
        started_at: Timestamp::from_second(0).unwrap(),
    });
    row.error = Some("previous run still active — skipped".into());
    row
}

fn deferred(second: i64) -> LoopRunRecord {
    let mut row = overlap(second);
    row.result = LoopRunResult::Deferred;
    row.error = Some("previous run still active — waiting".into());
    row
}

fn old_deferred(second: i64) -> LoopRunRecord {
    let mut row = deferred(second);
    row.holder = None;
    row.error = Some(format!(
        "previous run still active (pid 42424, started {second}m ago) — waiting"
    ));
    row
}

fn scheduled(second: i64, result: LoopRunResult) -> LoopRunRecord {
    let mut row = record(second, result);
    row.mode = Some(LoopRunMode::Scheduled);
    row
}

fn runs_table(records: &[LoopRunRecord], limit: usize) -> String {
    let mut out = Vec::new();
    write_runs_table(
        &mut out,
        records,
        limit,
        Timestamp::from_second(100).unwrap(),
    )
    .unwrap();
    anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string()
}

fn last_run(records: &[LoopRunRecord]) -> String {
    let mut out = Vec::new();
    write_last_run(
        &mut out,
        "wait",
        &TaskEntry::default(),
        records,
        Timestamp::from_second(100).unwrap(),
        ui::prose::Prose::Raw,
    )
    .unwrap();
    anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string()
}

#[test]
fn throttle_skip_is_a_refused_fire_with_a_visible_reason_and_a_waited_row_says_queued() {
    let mut skipped = record(0, LoopRunResult::ThrottleSkipped);
    skipped.error = Some("cpu pressure 41% >= 25%; held 30m".into());
    assert!(is_refused_fire(&skipped));

    let mut waited = record(0, LoopRunResult::Launched);
    waited.throttle_wait_ms = Some(192_000);
    assert_eq!(record_note(&waited).as_deref(), Some("queued 3m"));
    waited.target = Some("@fixer".into());
    assert_eq!(record_note(&waited).as_deref(), Some("queued 3m · @fixer"));
    waited.checkout = Some(PathBuf::from("/repo/lane"));
    assert_eq!(
        record_note(&waited).as_deref(),
        Some("lane queued 3m · @fixer")
    );
}

#[test]
fn checkout_without_a_note_has_no_trailing_separator() {
    let mut row = record(0, LoopRunResult::Launched);
    row.checkout = Some(PathBuf::from("/repo/lane"));
    assert_eq!(record_note(&row).as_deref(), Some("lane"));
    row.target = Some("@fixer".into());
    assert_eq!(record_note(&row).as_deref(), Some("lane @fixer"));
}

#[test]
fn task_rules_and_check_rows_use_action_specific_verbs() {
    let spawn = TaskEntry {
        agent: Some("codex".to_owned()),
        check: Some("cargo test".into()),
        on: Some(CheckOn::Fail),
        ..TaskEntry::default()
    };
    let spawn_action = schedule::TaskShape::compile("task", &spawn)
        .action()
        .unwrap()
        .clone();
    assert_eq!(
        task_run_rule(&spawn, &spawn_action),
        "check, then start codex on fail"
    );
    assert_eq!(
        check_summary(&spawn, Some(&spawn_action)).as_deref(),
        Some("cargo test (starts codex on fail)")
    );

    let wait = TaskEntry {
        wait: Some(TaskTarget {
            kind: rimz::ids::AgentKind::new_unchecked("claude"),
            session: "sess-planner".into(),
            handle: "@planner".to_owned(),
        }),
        check: Some("cargo test".into()),
        on: Some(CheckOn::Success),
        ..TaskEntry::default()
    };
    let wait_action = schedule::TaskShape::compile("task", &wait)
        .action()
        .unwrap()
        .clone();
    assert_eq!(
        task_run_rule(&wait, &wait_action),
        "check, then wake @planner on success"
    );
    assert_eq!(
        check_summary(&wait, Some(&wait_action)).as_deref(),
        Some("cargo test (wakes @planner on success)")
    );

    let check = TaskEntry {
        check: Some("cargo test".into()),
        ..TaskEntry::default()
    };
    let check_action = schedule::TaskShape::compile("task", &check)
        .action()
        .unwrap()
        .clone();
    assert_eq!(task_run_rule(&check, &check_action), "check");

    let spawn = TaskEntry {
        agent: Some("claude".to_owned()),
        verify: Some("cargo xtask gate".to_owned()),
        max_attempts: Some(4),
        ..TaskEntry::default()
    };
    let spawn_action = schedule::TaskShape::compile("task", &spawn)
        .action()
        .unwrap()
        .clone();
    assert_eq!(
        task_run_rule(&spawn, &spawn_action),
        "start claude, verify `cargo xtask gate` (up to 4 attempts)"
    );

    let mut wait_only = wait;
    wait_only.check = None;
    let wait_action = schedule::TaskShape::compile("task", &wait_only)
        .action()
        .unwrap()
        .clone();
    assert_eq!(task_run_rule(&wait_only, &wait_action), "wake @planner");
}

#[test]
fn budget_label_names_the_run_and_daily_caps() {
    let entry = TaskEntry {
        budget: Some("$5.00".to_owned()),
        budget_per_day: Some("$20.00".to_owned()),
        ..TaskEntry::default()
    };
    assert_eq!(
        budget_label(&entry).as_deref(),
        Some("$5 per run · $20 per day")
    );
}

#[test]
fn surplus_label_shows_explicit_and_implied_thresholds() {
    let explicit = TaskEntry {
        surplus: Some("1.5x".to_owned()),
        surplus_after: Some("3d".to_owned()),
        ..TaskEntry::default()
    };
    assert_eq!(
        surplus_label(&explicit).as_deref(),
        Some("surplus ≥ 1.5x · after 3d of window")
    );
    let implied = TaskEntry {
        surplus_after: Some("2d".to_owned()),
        ..TaskEntry::default()
    };
    assert_eq!(
        surplus_label(&implied).as_deref(),
        Some("surplus ≥ 1.0x · after 2d of window")
    );
}

#[test]
fn spend_label_renders_today_last_and_cost_window() {
    let now = "2026-06-02T12:00:00Z[UTC]".parse::<jiff::Zoned>().unwrap();
    let entry = TaskEntry {
        budget_per_day: Some("20".to_owned()),
        ..TaskEntry::default()
    };
    let mut records = Vec::new();
    for (second, cost) in [(1, 0.28), (2, 0.42)] {
        let mut run = record(second, LoopRunResult::Completed);
        run.at = now.timestamp() + jiff::SignedDuration::from_secs(second);
        run.cost_usd = Some(cost);
        records.push(run);
    }

    assert_eq!(
        spend_label(&entry, &records, &now, true).as_deref(),
        Some("$0.70 today of $20 · $0.42 last · ø $0.35 over 2 runs")
    );
    assert_eq!(
        spend_label(&entry, &records, &now, false).as_deref(),
        Some("$0.70 today of $20")
    );
    assert_eq!(spend_label(&TaskEntry::default(), &[], &now, true), None);
}

#[test]
fn verdict_names_the_newest_run_and_the_bound_of_its_streak() {
    let now = Timestamp::from_second(50).unwrap();
    let failed = record(10, LoopRunResult::Failed);
    let mut passed_check = record(20, LoopRunResult::CheckSkipped);
    passed_check.check = Some(check(Some(0), "ok"));
    let neutral = record(30, LoopRunResult::Overlapped);
    let mut completed = record(40, LoopRunResult::Completed);
    completed.duration_ms = Some(5_000);

    let (healthy, style) = verdict_line(
        &[failed, passed_check, neutral.clone(), completed.clone()],
        false,
        now,
    )
    .unwrap();
    assert_eq!(
        healthy,
        "✓ healthy · last run 10s ago, completed in 5.0s · 2 runs in a row since a failure 40s ago"
    );
    assert_eq!(style, ui::palette::good());

    let errored = record(40, LoopRunResult::Errored);
    let failed = record(20, LoopRunResult::Failed);
    let (failing, style) = verdict_line(
        &[completed.clone(), failed, neutral.clone(), errored.clone()],
        false,
        now,
    )
    .unwrap();
    assert_eq!(
        failing,
        "✗ failing · last run 10s ago, error · 2 runs in a row since a good run 10s ago"
    );
    assert_eq!(style, ui::palette::alarm());

    let (first, _) = verdict_line(&[completed.clone(), errored], false, now).unwrap();
    assert_eq!(
        first,
        "✗ failing · last run 10s ago, error · first since a good run 10s ago"
    );
    let (unbounded, _) = verdict_line(&[completed.clone(), completed], false, now).unwrap();
    assert_eq!(
        unbounded,
        "✓ healthy · last run 10s ago, completed in 5.0s · 2 runs in a row"
    );
    assert!(verdict_line(&[neutral], false, now).is_none());
}

#[test]
fn agent_run_predicate_counts_spawn_and_delivery_attempts() {
    let mut spawned = record(1, LoopRunResult::Completed);
    spawned.run_id = Some("run_0123456789abcdef01234567".to_owned());
    assert!(is_agent_run(&spawned));
    assert!(is_agent_run(&record(2, LoopRunResult::Delivered)));
    assert!(is_agent_run(&record(3, LoopRunResult::TargetGone)));
    for result in [
        LoopRunResult::CheckSkipped,
        LoopRunResult::BudgetSkipped,
        LoopRunResult::SurplusSkipped,
        LoopRunResult::AccountSkipped,
        LoopRunResult::Overlapped,
        LoopRunResult::Expired,
    ] {
        assert!(!is_agent_run(&record(4, result)), "{result:?}");
    }
}

#[test]
fn agent_runs_heading_aggregates_all_valid_costs() {
    let now = Timestamp::from_second(50).unwrap();
    let mut costed = record(10, LoopRunResult::Completed);
    costed.run_id = Some("run_0123456789abcdef01234567".to_owned());
    costed.cost_usd = Some(0.25);
    let mut delivered = record(20, LoopRunResult::Delivered);
    delivered.cost_usd = Some(0.75);
    let skipped = record(30, LoopRunResult::CheckSkipped);
    let mut out = Vec::new();
    write_agent_runs(&mut out, &[costed, delivered, skipped.clone()], now).unwrap();
    let out = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
    assert!(
        out.contains("AGENT RUNS — 2 of 3 runs · $1.00 total · ø $0.50"),
        "{out}"
    );

    let mut cost_free = record(10, LoopRunResult::Delivered);
    cost_free.cost_usd = Some(f64::NAN);
    let mut out = Vec::new();
    write_agent_runs(&mut out, &[cost_free], now).unwrap();
    let out = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
    assert!(out.contains("AGENT RUNS — 1 of 1 runs"), "{out}");
    assert!(!out.contains("total"), "{out}");

    let mut out = Vec::new();
    write_agent_runs(&mut out, &[skipped], now).unwrap();
    let out = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
    assert!(out.contains("AGENT RUNS — none in 1 runs"), "{out}");
    assert!(!out.contains("WHEN"), "{out}");
}

#[test]
fn check_failure_line_uses_last_non_empty_failed_check_line() {
    let mut failed = record(10, LoopRunResult::Failed);
    failed.check = Some(CheckRecord {
        duration_ms: None,
        agent: None,
        output_path: None,
        code: Some(127),
        timed_out: false,
        output: "ignored\n\nmissing command\n".to_owned(),
    });
    assert_eq!(
        check_failure_line(&failed).as_deref(),
        Some("missing command")
    );

    let mut passed = record(11, LoopRunResult::Completed);
    passed.check = Some(CheckRecord {
        duration_ms: None,
        agent: None,
        output_path: None,
        code: Some(0),
        timed_out: false,
        output: "ok".to_owned(),
    });
    assert_eq!(check_failure_line(&passed), None);
}

#[test]
fn record_note_prefers_error_then_failed_check_output() {
    let mut failed = record(10, LoopRunResult::Failed);
    failed.check = Some(CheckRecord {
        duration_ms: None,
        agent: None,
        output_path: None,
        code: Some(1),
        timed_out: false,
        output: "first\ncheck failed".to_owned(),
    });
    failed.last_message = Some("last message".to_owned());
    assert_eq!(record_note(&failed), Some("check failed".to_owned()));

    failed.error = Some("outer error\nignored detail".to_owned());
    assert_eq!(record_note(&failed), Some("outer error".to_owned()));
}

#[test]
fn run_status_names_check_skipped_outcomes() {
    let mut skipped = record(10, LoopRunResult::CheckSkipped);
    skipped.check = Some(CheckRecord {
        duration_ms: None,
        agent: None,
        output_path: None,
        code: Some(0),
        timed_out: false,
        output: "ok".to_owned(),
    });
    let status = run_status(&skipped);
    assert_eq!(status.glyph, "✓");
    assert_eq!(status.label, "check passed");
    assert_eq!(status.style, ui::palette::good());

    skipped.check = Some(CheckRecord {
        duration_ms: None,
        agent: None,
        output_path: None,
        code: Some(1),
        timed_out: false,
        output: "not yet".to_owned(),
    });
    let status = run_status(&skipped);
    assert_eq!(status.glyph, "○");
    assert_eq!(status.label, "check failed");
    assert_eq!(status.style, ui::palette::muted());

    skipped.check = Some(CheckRecord {
        duration_ms: None,
        agent: None,
        output_path: None,
        code: None,
        timed_out: true,
        output: "too slow".to_owned(),
    });
    let status = run_status(&skipped);
    assert_eq!(status.glyph, "○");
    assert_eq!(status.label, "check timed out");
    assert_eq!(status.style, ui::palette::warn());

    assert_eq!(
        loop_result_mark(LoopRunResult::SurplusSkipped).style,
        ui::palette::muted()
    );
}

#[test]
fn run_result_marks_and_static_labels_cover_every_variant() {
    for (name, glyph, style) in [
        ("deferred", "○", ui::palette::muted()),
        ("replaced", "✗", ui::palette::alarm()),
    ] {
        let result = serde_json::from_value::<LoopRunResult>(serde_json::json!(name));
        assert!(result.is_ok(), "{name} must be a rendered history result");
        let result = result.unwrap();
        assert_eq!(loop_result_mark(result).glyph, glyph);
        assert_eq!(loop_result_mark(result).style, style);
        assert_eq!(result.label(), name);
    }
    let cases = [
        (
            LoopRunResult::CapacityHeld,
            "○",
            ui::palette::warn(),
            "capacity held",
        ),
        (
            LoopRunResult::Launched,
            "✓",
            ui::palette::good(),
            "launched",
        ),
        (
            LoopRunResult::SignalSkipped,
            "○",
            ui::palette::muted(),
            "skipped",
        ),
        (
            LoopRunResult::WorkflowSkipped,
            "○",
            ui::palette::muted(),
            "skipped",
        ),
        (
            LoopRunResult::Completed,
            "✓",
            ui::palette::good(),
            "completed",
        ),
        (
            LoopRunResult::Delivered,
            "✓",
            ui::palette::good(),
            "delivered",
        ),
        (LoopRunResult::Failed, "✗", ui::palette::alarm(), "failed"),
        (
            LoopRunResult::VerifyFailed,
            "✗",
            ui::palette::alarm(),
            "verify failed",
        ),
        (
            LoopRunResult::TimedOut,
            "✗",
            ui::palette::alarm(),
            "timed out",
        ),
        (
            LoopRunResult::BudgetExceeded,
            "✗",
            ui::palette::alarm(),
            "budget exceeded",
        ),
        (LoopRunResult::Errored, "✗", ui::palette::alarm(), "error"),
        (
            LoopRunResult::StartFailed,
            "✗",
            ui::palette::alarm(),
            "start failed",
        ),
        (LoopRunResult::Expired, "○", ui::palette::warn(), "expired"),
        (
            LoopRunResult::Canceled,
            "○",
            ui::palette::warn(),
            "canceled",
        ),
        (
            LoopRunResult::TargetGone,
            "○",
            ui::palette::warn(),
            "target gone",
        ),
        (
            LoopRunResult::Overlapped,
            "○",
            ui::palette::warn(),
            "overlapped",
        ),
        (
            LoopRunResult::BudgetSkipped,
            "○",
            ui::palette::warn(),
            "budget skipped",
        ),
        (
            LoopRunResult::SurplusSkipped,
            "○",
            ui::palette::muted(),
            "surplus skipped",
        ),
        (
            LoopRunResult::AccountSkipped,
            "○",
            ui::palette::warn(),
            "account skipped",
        ),
        (
            LoopRunResult::ThrottleSkipped,
            "○",
            ui::palette::warn(),
            "throttle skipped",
        ),
        (
            LoopRunResult::TakeoverBlocked,
            "○",
            ui::palette::warn(),
            "takeover blocked",
        ),
        (
            LoopRunResult::CheckSkipped,
            "○",
            ui::palette::muted(),
            "skipped",
        ),
    ];

    for (result, glyph, style, label) in cases {
        let mark = loop_result_mark(result);
        assert_eq!(mark.glyph, glyph, "{result:?}");
        assert_eq!(mark.style, style, "{result:?}");
        assert_eq!(result.label(), label, "{result:?}");
        if result != LoopRunResult::CheckSkipped {
            let status = run_status(&record(10, result));
            assert_eq!(status.glyph, glyph, "{result:?}");
            assert_eq!(status.style, style, "{result:?}");
            assert_eq!(
                status.label,
                if result == LoopRunResult::SignalSkipped {
                    "heard"
                } else {
                    label
                },
                "{result:?}"
            );
        }
    }
}

#[test]
fn team_source_label_requires_an_instance_row() {
    let entry = TaskEntry {
        root: PathBuf::from("/tmp/project"),
        team: Some("forge#feat-x".parse().unwrap()),
        ..TaskEntry::default()
    };
    assert_eq!(
        source_description(TaskSource::Instance, &entry),
        "team forge#feat-x"
    );
    assert_eq!(source_description(TaskSource::Config, &entry), "machine");
    assert_eq!(
        source_description(
            TaskSource::Project {
                state: TrustState::Untrusted
            },
            &entry
        ),
        "project · untrusted"
    );
    assert_eq!(
        source_description(TaskSource::Instance, &TaskEntry::default()),
        "state"
    );
    assert!(source_detail(TaskSource::Instance, &entry).starts_with("team forge#feat-x — "));
}

#[test]
fn source_detail_names_definition_path() {
    let entry = TaskEntry {
        root: PathBuf::from("/repo"),
        ..TaskEntry::default()
    };

    assert_eq!(
        source_detail(TaskSource::Config, &entry),
        format!(
            "machine — {}",
            ui::home_relative(MachineConfig::loop_path().to_string_lossy().as_ref())
        )
    );
    assert_eq!(
        source_detail(
            TaskSource::Project {
                state: TrustState::Untrusted
            },
            &entry,
        ),
        "project · untrusted — /repo/.rimz/config.toml"
    );
    assert_eq!(
        source_detail(TaskSource::Instance, &entry),
        format!(
            "state — {}",
            ui::home_relative(
                StatePaths::for_project_root(&entry.resolved_root())
                    .expect("state paths")
                    .root
                    .join("records/loop-instances.json")
                    .to_string_lossy()
                    .as_ref(),
            )
        )
    );
}

#[test]
fn blocked_project_rendering_names_the_gate_and_fix() {
    assert_eq!(
        blocked_notice(TrustState::Untrusted),
        "project trust is untrusted — review with `rimz trust`, approve with `rimz trust grant`"
    );
}

fn interval_timing(
    blocked: Option<TrustState>,
    last_fire: Option<Timestamp>,
    arming: Option<&Arming>,
    now: Timestamp,
) -> schedule::TaskTiming {
    let entry = TaskEntry {
        agent: Some("claude".to_owned()),
        every: Some("15m".to_owned()),
        ..TaskEntry::default()
    };
    schedule::TaskTiming::evaluate(
        schedule::TaskShape::compile("task", &entry).trigger(),
        blocked.map_or(TaskSource::Config, |state| TaskSource::Project { state }),
        last_fire,
        arming,
        &now.to_zoned(jiff::tz::TimeZone::UTC),
    )
}

#[test]
fn signal_timing_renders_trigger_matches_and_listening_state() {
    let now = Timestamp::from_second(10_000).unwrap();
    let entry = TaskEntry {
        signal: Some("ci.failed".to_owned()),
        matches: Some(BTreeMap::from([(
            "branch".to_owned(),
            "feature".to_owned(),
        )])),
        ..TaskEntry::default()
    };
    let timing = schedule::TaskTiming::evaluate(
        schedule::TaskShape::compile("task", &entry).trigger(),
        TaskSource::Config,
        None,
        None,
        &now.to_zoned(jiff::tz::TimeZone::UTC),
    );

    let mut out = Vec::new();
    write_show_headline(
        &mut out,
        "task",
        &timing,
        None,
        now,
        &entry,
        schedule::TaskKind::Routine,
    )
    .unwrap();
    let show = String::from_utf8(out).unwrap();
    assert!(
        show.contains("on ci.failed [branch=feature] · listening"),
        "{show}"
    );
}

#[test]
fn running_replaces_the_headline_state_and_scales_its_elapsed_time() {
    let now = Timestamp::from_second(100_000).unwrap();
    let holder = |age: i64| RunLockInfo {
        pid: 4_162_080,
        started_at: now - jiff::SignedDuration::from_secs(age),
    };
    for (age, text) in [
        (45, "▸ running 45s"),
        (190 * 60, "▸ running 3h"),
        (-5, "▸ running 0s"),
    ] {
        assert_eq!(running_text(Some(holder(age)), now), text);
    }
    assert_eq!(running_text(None, now), "▸ running");
    let holderless = InFlightRun {
        holder: None,
        run: None,
        held: Vec::new(),
    };
    assert_eq!(running_text_full(&holderless, now), "▸ running");

    let since_ms = u64::try_from((now - jiff::SignedDuration::from_secs(192)).as_millisecond())
        .expect("after the epoch");
    let mut held = Held {
        pid: holder(200).pid,
        checkout: "/repo/.worktrees/a".into(),
        since_ms,
        reason: Some("cpu pressure 41% >= 25%".to_owned()),
        kinds: Vec::new(),
        gives_up_ms: None,
        position: 2,
    };
    let waiting = InFlightRun {
        holder: Some(holder(200)),
        run: None,
        held: vec![held.clone()],
    };
    let phrase = rimz::theme::fmt::capacity_wait(
        &held.kinds,
        Timestamp::from_millisecond(since_ms as i64).unwrap(),
        None,
    );
    assert_eq!(
        running_text_full(&waiting, now),
        format!(
            "{phrase} · cpu pressure 41% >= 25% · pid {}",
            holder(200).pid
        )
    );
    let json = held_json(&held);
    assert_eq!(json["pid"], holder(200).pid);
    assert_eq!(json["checkout"], "/repo/.worktrees/a");
    assert_eq!(json["reason"], "cpu pressure 41% >= 25%");
    assert_eq!(json["position"], 2);
    assert_eq!(
        json["since"],
        serde_json::json!(now - jiff::SignedDuration::from_secs(192))
    );
    held.reason = None;
    assert_eq!(held_run_text(&held), phrase);

    let mut readings = Vec::new();
    write_throttle_readings(
        &mut readings,
        &[("cpu pressure", "avg10 2% · avg60 3%".into())],
    )
    .expect("write");
    let readings = String::from_utf8(readings).expect("utf8");
    assert!(readings.starts_with("\nTHROTTLE\n"), "{readings}");
    assert!(readings.contains("cpu pressure"), "{readings}");
    assert!(readings.contains("avg10 2% · avg60 3%"), "{readings}");

    let pause = Arming {
        enabled: true,
        at: None,
        pause_until: Some(now + jiff::SignedDuration::from_secs(300)),
        strikes: None,
    };
    let timing = interval_timing(None, None, Some(&pause), now);
    let in_flight = InFlightRun {
        holder: Some(holder(180)),
        run: None,
        held: Vec::new(),
    };
    let mut out = Vec::new();
    write_show_headline(
        &mut out,
        "task",
        &timing,
        Some(&in_flight),
        now,
        &TaskEntry::default(),
        schedule::TaskKind::Routine,
    )
    .unwrap();
    let show = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
    assert!(
        show.ends_with(" · ▸ running 3m · pid 4162080\n") && !show.contains("paused"),
        "{show}"
    );
}

#[test]
fn show_headline_keeps_blocked_before_pause() {
    let now = Timestamp::from_second(10_000).unwrap();
    let pause = Arming {
        enabled: true,
        at: None,
        pause_until: Timestamp::from_second(10_300).ok(),
        strikes: None,
    };
    let timing = interval_timing(Some(TrustState::Untrusted), None, Some(&pause), now);
    let mut out = Vec::new();

    write_show_headline(
        &mut out,
        "task",
        &timing,
        None,
        now,
        &TaskEntry::default(),
        schedule::TaskKind::Routine,
    )
    .unwrap();

    let out = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
    assert!(out.contains("next blocked · trust"), "{out}");
    assert!(!out.contains("paused"), "{out}");
    assert!(!out.contains("loop enable"), "{out}");
}

#[test]
fn record_exit_maps_terminal_spawn_results_only_with_run_id() {
    for (result, expected) in [
        (LoopRunResult::Completed, Some("0")),
        (LoopRunResult::Failed, Some("1")),
        (LoopRunResult::VerifyFailed, Some("123")),
        (LoopRunResult::TimedOut, Some("124")),
        (LoopRunResult::BudgetExceeded, Some("125")),
        (LoopRunResult::Canceled, Some("130")),
        (LoopRunResult::BudgetSkipped, None),
        (LoopRunResult::SurplusSkipped, None),
        (LoopRunResult::AccountSkipped, None),
        (LoopRunResult::Delivered, None),
        (LoopRunResult::TargetGone, None),
        (LoopRunResult::CheckSkipped, None),
        (LoopRunResult::SignalSkipped, None),
        (LoopRunResult::Expired, None),
        (LoopRunResult::Errored, None),
        (LoopRunResult::StartFailed, None),
        (LoopRunResult::Overlapped, None),
        (LoopRunResult::TakeoverBlocked, None),
    ] {
        let mut run = record(10, result);
        assert_eq!(record_exit(&run), None, "{result:?} without run_id");

        run.run_id = Some("run_0123456789abcdef01234567".to_owned());
        assert_eq!(record_exit(&run).as_deref(), expected, "{result:?}");
    }
}

#[test]
fn run_status_merges_failed_check_exit() {
    let mut failed = record(10, LoopRunResult::Failed);
    failed.run_id = Some("run_0123456789abcdef01234567".to_owned());
    failed.check = Some(CheckRecord {
        duration_ms: None,
        agent: None,
        output_path: None,
        code: Some(127),
        timed_out: false,
        output: "missing".to_owned(),
    });

    let status = run_status(&failed);

    assert_eq!(status.glyph, "✗");
    assert_eq!(status.label, "failed (exit 127)");
    assert_eq!(record_exit(&failed).as_deref(), Some("127"));
}

#[test]
fn runs_table_shows_tokens_only_when_present() {
    let now = Timestamp::from_second(30).expect("timestamp");
    let without_tokens = record(10, LoopRunResult::Completed);
    let mut out = Vec::new();
    write_runs_table(&mut out, &[without_tokens], 10, now).unwrap();
    let out = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
    assert!(!out.contains("TOKENS"), "{out}");

    let mut with_tokens = record(20, LoopRunResult::Completed);
    with_tokens.input_tokens = Some(14_000);
    with_tokens.output_tokens = Some(269);
    let without_tokens = record(10, LoopRunResult::Failed);
    let mut out = Vec::new();
    write_runs_table(&mut out, &[without_tokens, with_tokens], 10, now).unwrap();
    let out = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
    assert!(out.contains("TOKENS"), "{out}");
    assert!(out.contains("↘ 14k ↗ 269"), "{out}");
    assert!(
        out.lines()
            .any(|line| line.contains("✗ failed") && line.ends_with('-')),
        "{out}"
    );
}

#[test]
fn sub_second_run_folds_keep_counts_without_a_note_column() {
    let first = scheduled(10, LoopRunResult::Completed);
    let mut latest = first.clone();
    latest.at += jiff::SignedDuration::from_millis(999);
    let table = runs_table(&[first.clone(), latest.clone()], 10);
    assert!(table.contains("✓ completed ×2"), "{table}");
    assert!(!table.contains("NOTE"), "{table}");
    assert!(!table.contains("over"), "{table}");

    let mut failed = scheduled(9, LoopRunResult::Failed);
    failed.error = Some("boom".into());
    let table = runs_table(&[failed, first, latest], 10);
    assert!(table.contains("NOTE"), "{table}");
    assert!(
        table
            .lines()
            .any(|line| line.contains("✓ completed ×2") && line.ends_with('-')),
        "{table}"
    );
}

#[test]
fn collapsed_run_rows_ignore_reason_text_and_keep_the_newest_note() {
    let mut first = record(10, LoopRunResult::Failed);
    first.mode = Some(LoopRunMode::Scheduled);
    first.duration_ms = Some(10);
    first.check = Some(CheckRecord {
        duration_ms: None,
        agent: None,
        output_path: None,
        code: Some(1),
        timed_out: false,
        output: "boom".to_owned(),
    });
    let mut second = first.clone();
    second.at = Timestamp::from_second(20).expect("timestamp");
    second.duration_ms = Some(20);
    let mut third = second.clone();
    third.at = Timestamp::from_second(30).expect("timestamp");
    third.check = Some(CheckRecord {
        duration_ms: None,
        agent: None,
        output_path: None,
        code: Some(1),
        timed_out: false,
        output: "different".to_owned(),
    });
    let mut records = vec![first, second, third];

    let rows = collapsed_run_rows(&records);

    assert_eq!(
        rows.iter()
            .map(|row| (
                row.count,
                row.latest.at.as_second(),
                record_note(row.latest)
            ))
            .collect::<Vec<_>>(),
        [(3, 30, Some("different".into()))]
    );

    for (record, millis) in records.iter_mut().zip([10_000, 10_400, 10_999]) {
        record.at = Timestamp::from_millisecond(millis).unwrap();
    }
    let table = runs_table(&records, 10);
    assert!(table.contains("✗ failed (exit 1) ×3"), "{table}");
    assert!(!table.contains("over"), "{table}");
    assert!(
        table
            .lines()
            .any(|line| line.ends_with("10ms–20ms  different")),
        "{table}"
    );
}

fn coalesce_history(old_shape: bool) -> Vec<LoopRunRecord> {
    let completed = |second, duration_ms| {
        let mut row = scheduled(second, LoopRunResult::Completed);
        row.duration_ms = Some(duration_ms);
        row
    };
    let mut records = vec![completed(1, 834), completed(61, 906), completed(121, 1391)];
    records.extend((180..=540).step_by(60).map(|second| {
        if old_shape {
            old_deferred(second)
        } else {
            deferred(second)
        }
    }));
    records.push(completed(600, 450489));
    let mut follow_up = completed(601, 1583);
    follow_up.absorbed = 6;
    records.extend([follow_up, completed(660, 840), completed(720, 1720)]);
    records
}

#[test]
fn refused_fires_stay_with_the_holder_that_refused_them() {
    let gone = RunLockInfo {
        pid: 3003569,
        started_at: Timestamp::from_second(0).unwrap(),
    };
    let next = RunLockInfo {
        pid: 42424,
        started_at: Timestamp::from_second(70).unwrap(),
    };
    let mut records = (10..=60)
        .step_by(10)
        .map(|second| {
            let mut row = deferred(second);
            row.holder = Some(gone);
            row
        })
        .collect::<Vec<_>>();
    let mut waiting = deferred(80);
    waiting.holder = Some(next);
    records.push(waiting);
    records.push(
        serde_json::from_value(serde_json::json!({
            "task": "wait", "at": Timestamp::from_second(90).unwrap(),
            "result": "completed", "mode": "scheduled", "duration_ms": 47000,
            "run_lock": next,
        }))
        .unwrap(),
    );

    let table = runs_table(&records, 10);
    let lines = table.lines().collect::<Vec<_>>();
    assert_eq!(
        (
            lines.iter().position(|line| line.contains("✓ completed")
                && line.contains("47s")
                && line.ends_with("1 fire waited on it")),
            lines.iter().position(|line| line.contains("○ no record")
                && line.ends_with("pid 3003569 · 6 fires waited on it")),
            table.contains("8 of 8"),
        ),
        (Some(3), Some(4), true),
        "{table}"
    );
}

fn run_lock_info(pid: u32, second: i64) -> RunLockInfo {
    RunLockInfo {
        pid,
        started_at: Timestamp::from_second(second).unwrap(),
    }
}

fn refused_by(mut record: LoopRunRecord, holder: RunLockInfo) -> LoopRunRecord {
    record.holder = Some(holder);
    record
}

fn with_run_lock(mut record: LoopRunRecord, holder: RunLockInfo) -> LoopRunRecord {
    record.run_lock = Some(holder);
    record
}

#[test]
fn an_uncontended_run_does_not_take_an_unrecorded_holders_fires() {
    let gone = run_lock_info(3003569, 0);
    let next = run_lock_info(42424, 70);
    let mut records = (10..=60)
        .step_by(10)
        .map(|second| refused_by(deferred(second), gone))
        .collect::<Vec<_>>();
    records.push(with_run_lock(scheduled(90, LoopRunResult::Completed), next));
    let table = runs_table(&records, 10);
    assert_eq!(
        collapsed_run_rows(&records)
            .iter()
            .map(|row| (row.count, row.waited))
            .collect::<Vec<_>>(),
        [(0, 6), (1, 0)],
        "{table}"
    );
    assert!(table.contains("7 of 7"), "{table}");
}

#[test]
fn a_new_holder_separates_old_fires_even_when_its_record_has_no_stamp() {
    let gone = run_lock_info(3003569, 0);
    let next = run_lock_info(42424, 70);
    let mut records = (10..=60)
        .step_by(10)
        .map(|second| refused_by(deferred(second), gone))
        .collect::<Vec<_>>();
    records.extend([
        refused_by(deferred(80), next),
        scheduled(90, LoopRunResult::Completed),
    ]);
    let table = runs_table(&records, 10);
    assert_eq!(
        collapsed_run_rows(&records)
            .iter()
            .map(|row| (row.count, row.waited))
            .collect::<Vec<_>>(),
        [(0, 6), (1, 1)],
        "{table}"
    );
    assert!(table.contains("8 of 8"), "{table}");
}

#[test]
fn stamped_runs_take_holderless_fires_but_not_another_tenures_fires() {
    let gone = run_lock_info(42424, 0);
    let next = run_lock_info(42424, 25);
    let records = [
        old_deferred(10),
        refused_by(deferred(20), gone),
        with_run_lock(scheduled(30, LoopRunResult::Completed), next),
    ];
    let table = runs_table(&records, 10);
    assert_eq!(
        collapsed_run_rows(&records)
            .iter()
            .map(|row| (row.count, row.waited))
            .collect::<Vec<_>>(),
        [(0, 1), (1, 1)],
        "{table}"
    );
    assert!(table.contains("3 of 3"), "{table}");
}

#[test]
fn stopped_holders_take_their_fires_without_a_checkout_on_the_record() {
    let stopped = run_lock_info(11, 0);
    let next = run_lock_info(22, 30);
    let mut records = [
        refused_by(deferred(10), stopped),
        refused_by(overlap(11), stopped),
        with_run_lock(scheduled(20, LoopRunResult::Canceled), stopped),
        with_run_lock(scheduled(40, LoopRunResult::Completed), next),
    ];
    for index in [0, 1, 3] {
        records[index].checkout = Some("/repo/alpha".into());
    }
    let table = runs_table(&records, 10);
    assert_eq!(
        collapsed_run_rows(&records)
            .iter()
            .map(|row| (row.latest.result, row.skipped, row.waited))
            .collect::<Vec<_>>(),
        [
            (LoopRunResult::Canceled, 1, 1),
            (LoopRunResult::Completed, 0, 0),
        ],
        "{table}"
    );
    assert!(table.contains("4 of 4"), "{table}");
}

#[test]
fn recorded_gate_refusals_take_their_matched_fires_and_keep_the_stale_clause() {
    let holder = run_lock_info(11, 0);
    for result in [
        LoopRunResult::ThrottleSkipped,
        LoopRunResult::CapacityHeld,
        LoopRunResult::BudgetSkipped,
        LoopRunResult::AccountSkipped,
        LoopRunResult::SurplusSkipped,
    ] {
        let records = [
            refused_by(overlap(10), holder),
            refused_by(deferred(11), holder),
            with_run_lock(scheduled(20, result), holder),
        ];
        let table = runs_table(&records, 10);
        let rows = collapsed_run_rows(&records);
        assert_eq!((rows[0].skipped, rows[0].waited), (1, 1), "{table}");
        assert!(table.contains("3 of 3"), "{table}");
        assert_eq!(
            stale_clause(&records, false).as_deref(),
            Some("1 fire skipped, nothing has run since")
        );
    }
}

#[test]
fn late_skip_policy_refusals_fold_back_into_their_recorded_holder() {
    let holder = run_lock_info(11, 0);
    let records = [
        with_run_lock(scheduled(10, LoopRunResult::Completed), holder),
        refused_by(overlap(20), holder),
    ];
    let table = runs_table(&records, 10);
    let rows = collapsed_run_rows(&records);
    assert_eq!((rows.len(), rows[0].skipped), (1, 1), "{table}");
    assert!(table.contains("2 of 2"), "{table}");
}

#[test]
fn late_refusals_of_an_earlier_run_do_not_make_the_newest_run_stale() {
    let first = run_lock_info(11, 0);
    let newest = run_lock_info(22, 25);
    let records = [
        with_run_lock(scheduled(10, LoopRunResult::Completed), first),
        refused_by(overlap(20), first),
        with_run_lock(scheduled(30, LoopRunResult::Completed), newest),
    ];
    assert_eq!(stale_clause(&records, false), None);
    let rows = collapsed_run_rows(&records);
    assert_eq!((rows[0].skipped, rows[1].skipped), (1, 0));

    let holderless = |mut record: LoopRunRecord| {
        record.holder = None;
        record
    };
    let in_checkout = |mut record: LoopRunRecord, checkout: &str| {
        record.checkout = Some(checkout.into());
        record
    };
    let newest_run = with_run_lock(scheduled(30, LoopRunResult::Completed), newest);
    for records in [
        [holderless(overlap(20)), newest_run.clone()],
        [refused_by(overlap(20), newest), newest_run.clone()],
        // A killed holder's fire in one fan-out checkout, then a run in another: no row takes it.
        [
            in_checkout(refused_by(overlap(20), first), "/repo/alpha"),
            in_checkout(newest_run.clone(), "/repo/beta"),
        ],
        // A legacy `stay` history ending in a stop-ladder record, which names no checkout.
        [
            in_checkout(holderless(overlap(20)), "/repo"),
            scheduled(30, LoopRunResult::Canceled),
        ],
    ] {
        assert_eq!(
            stale_clause(&records, false).as_deref(),
            Some("a fire skipped during the last run, nothing has run since")
        );
    }
}

#[test]
fn lock_holding_at_lock_records_keep_their_own_row_and_matched_fires() {
    let holder = run_lock_info(11, 0);
    let detail = "resident launch completed during room birth — skipped";
    for (result, label, skipped, waited) in [
        (LoopRunResult::Overlapped, "overlapped", 1, 0),
        (LoopRunResult::Deferred, "deferred", 0, 1),
    ] {
        let mut held = with_run_lock(scheduled(20, result), holder);
        held.error = Some(detail.into());
        held.duration_ms = Some(2000);
        let records = [
            refused_by(deferred(10), holder),
            held.clone(),
            scheduled(30, LoopRunResult::Completed),
        ];
        let table = runs_table(&records, 10);
        let lines = table.lines().collect::<Vec<_>>();
        assert_eq!(
            (
                lines
                    .iter()
                    .position(|line| line.contains("✓ completed") && !line.contains("fire")),
                lines.iter().position(|line| line.contains(label)
                    && line.contains("2.0s")
                    && line.ends_with(&format!("{detail} · 1 fire waited on it"))),
                table.contains("3 of 3"),
                table.contains("○ no record"),
            ),
            (Some(3), Some(4), true, false),
            "{table}"
        );
        let rows = collapsed_run_rows(&records);
        assert_eq!(
            rows.iter()
                .map(|row| (
                    row.latest.at.as_second(),
                    row.count,
                    row.skipped,
                    row.waited
                ))
                .collect::<Vec<_>>(),
            [(20, 1, 0, 1), (30, 1, 0, 0)]
        );
        assert_eq!(stale_clause(&records, false), None, "{table}");

        let records = [held, scheduled(30, LoopRunResult::Completed)];
        let table = runs_table(&records, 10);
        let rows = collapsed_run_rows(&records);
        assert_eq!(
            (
                rows.len(),
                rows[0].latest.result,
                rows[0].skipped,
                rows[0].waited
            ),
            (1, LoopRunResult::Completed, skipped, waited),
            "{table}"
        );
        assert!(table.contains("2 of 2"), "{table}");
    }
}

#[test]
fn late_refusals_of_unrecorded_holders_keep_the_already_observed_new_tenure() {
    let gone = run_lock_info(11, 0);
    let next = run_lock_info(22, 15);
    let records = [
        with_run_lock(scheduled(20, LoopRunResult::Completed), next),
        refused_by(overlap(30), gone),
        scheduled(40, LoopRunResult::Completed),
    ];
    let table = runs_table(&records, 10);
    assert_eq!(
        collapsed_run_rows(&records)
            .iter()
            .map(|row| (row.count, row.skipped))
            .collect::<Vec<_>>(),
        [(1, 0), (0, 1), (1, 0)],
        "{table}"
    );
    assert_eq!(stale_clause(&records, false), None);
}

#[test]
fn legacy_run_boundaries_preserve_single_holder_gaps_even_for_late_refusals() {
    let first = run_lock_info(11, 0);
    let second = run_lock_info(22, 25);
    let records = [
        refused_by(overlap(10), first),
        scheduled(20, LoopRunResult::Completed),
        refused_by(overlap(30), second),
        scheduled(40, LoopRunResult::Completed),
        refused_by(overlap(50), first),
        scheduled(60, LoopRunResult::Completed),
    ];
    let table = runs_table(&records, 10);
    assert_eq!(
        collapsed_run_rows(&records)
            .iter()
            .map(|row| (row.count, row.skipped))
            .collect::<Vec<_>>(),
        [(3, 3)],
        "{table}"
    );
    assert!(
        table.contains("6 of 6") && !table.contains("no record"),
        "{table}"
    );
}

#[test]
fn unrecorded_holders_do_not_make_a_later_run_stale() {
    let gone = run_lock_info(11, 0);
    let next = run_lock_info(22, 30);
    let records = [
        refused_by(overlap(10), gone),
        refused_by(overlap(20), gone),
        with_run_lock(scheduled(40, LoopRunResult::Completed), next),
    ];
    assert_eq!(stale_clause(&records, false), None);
}

#[test]
fn stale_clause_excludes_fires_on_a_no_record_row_from_its_tail() {
    let gone = run_lock_info(11, 11);
    let next = run_lock_info(22, 31);
    let records = [
        scheduled(10, LoopRunResult::Completed),
        refused_by(overlap(20), gone),
        refused_by(overlap(30), gone),
        refused_by(overlap(40), next),
    ];
    assert_eq!(
        [false, true].map(|running| stale_clause(&records, running)),
        [
            Some("last fire skipped, nothing has run since".into()),
            Some("1 fire skipped while the active run holds the lock".into()),
        ]
    );
}

#[test]
fn no_record_rows_keep_their_position_counts_and_empty_measurements() {
    let first = run_lock_info(11, 0);
    let second = run_lock_info(22, 30);
    let third = run_lock_info(33, 50);
    let mut completed = with_run_lock(scheduled(90, LoopRunResult::Completed), third);
    completed.cost_usd = Some(1.0);
    completed.input_tokens = Some(10);
    let mut records = vec![
        refused_by(deferred(10), first),
        refused_by(deferred(20), first),
        refused_by(deferred(40), second),
        completed,
    ];
    for record in &mut records {
        record.checkout = Some("/repo/alpha".into());
    }
    let table = runs_table(&records, 10);
    assert_eq!(
        collapsed_run_rows(&records)
            .iter()
            .map(|row| (row.latest.at.as_second(), row.count, row.waited))
            .collect::<Vec<_>>(),
        [(20, 0, 2), (40, 0, 1), (90, 1, 0)],
        "{table}"
    );
    assert!(!table.contains("MODE"), "{table}");
    assert!(table.contains("4 of 4"), "{table}");
    let missing = table
        .lines()
        .filter(|line| line.contains("○ no record"))
        .collect::<Vec<_>>();
    assert_eq!(missing.len(), 2, "{table}");
    assert!(
        missing[0].ends_with("alpha pid 22 · 1 fire waited on it"),
        "{table}"
    );
    assert!(
        missing[1].ends_with("alpha pid 11 · 2 fires waited on it"),
        "{table}"
    );
    for line in missing {
        assert!(!line.contains('×'), "{table}");
        assert_eq!(
            line.split_once("○ no record")
                .unwrap()
                .1
                .split_whitespace()
                .take(3)
                .collect::<Vec<_>>(),
            ["-", "-", "-"],
            "{table}"
        );
    }

    let mut manual = record(95, LoopRunResult::Completed);
    manual.mode = Some(LoopRunMode::Manual);
    records.push(manual);
    let table = runs_table(&records, 10);
    assert!(table.contains("MODE"), "{table}");
    for line in table.lines().filter(|line| line.contains("○ no record")) {
        assert_eq!(
            line.split_once("○ no record")
                .unwrap()
                .0
                .split_whitespace()
                .last(),
            Some("-"),
            "{table}"
        );
    }
}

#[test]
fn coalesce_history_separates_the_busy_run_and_folds_the_follow_up() {
    let records = coalesce_history(false);
    let table = runs_table(&records, 10);
    assert_eq!(
        (
            collapsed_run_rows(&records)
                .iter()
                .map(|row| row.count)
                .collect::<Vec<_>>(),
            table.contains("deferred"),
            table
                .lines()
                .any(|line| line.contains("7m") && line.ends_with("7 fires waited on it")),
            table.matches("✓ completed ×3").count(),
            table.contains("840ms–1.7s  over 1m"),
            table.contains("14 of 14"),
        ),
        (vec![3, 1, 3], false, true, 2, true, true),
        "{table}"
    );
}

#[test]
fn old_deferred_prose_folds_like_typed_holder_records() {
    assert_eq!(
        runs_table(&coalesce_history(true), 10),
        runs_table(&coalesce_history(false), 10)
    );
}

#[test]
fn at_lock_fires_wait_for_a_run_in_their_own_checkout_past_other_gates() {
    let in_checkout = |mut row: LoopRunRecord, checkout: &str| {
        row.checkout = Some(PathBuf::from(checkout));
        row
    };
    let records = [
        in_checkout(deferred(10), "/repo/alpha"),
        in_checkout(overlap(11), "/repo/alpha"),
        in_checkout(scheduled(20, LoopRunResult::Completed), "/repo/beta"),
        in_checkout(scheduled(30, LoopRunResult::BudgetSkipped), "/repo/alpha"),
        in_checkout(scheduled(40, LoopRunResult::Completed), "/repo/alpha"),
    ];
    let table = runs_table(&records, 10);
    let notes = table
        .lines()
        .filter(|line| line.contains("✓ completed"))
        .collect::<Vec<_>>();
    assert_eq!(
        (
            collapsed_run_rows(&records).len(),
            notes.iter().any(|line| line
                .ends_with("alpha · 1 fire waited on it · 1 fire skipped, run already active")),
            notes.iter().any(|line| line.ends_with("beta")),
            table.contains("5 of 5"),
        ),
        (3, true, true, true),
        "{table}"
    );
}

#[test]
fn typed_run_keys_fold_watch_times_and_gate_reasons_but_keep_identities_and_verdicts() {
    use rimz::harness::schedule::signal::WatchVerdict;
    let mut watch = scheduled(10, LoopRunResult::Completed);
    watch.watch = Some(WatchVerdict::Exited {
        code: Some(0),
        elapsed_ms: 1000,
    });
    let mut later_watch = watch.clone();
    later_watch.at = Timestamp::from_second(20).unwrap();
    later_watch.watch = Some(WatchVerdict::Exited {
        code: Some(0),
        elapsed_ms: 2000,
    });
    let mut budget = scheduled(10, LoopRunResult::BudgetSkipped);
    budget.error = Some("daily cap reached".into());
    let mut later_budget = budget.clone();
    later_budget.at = Timestamp::from_second(20).unwrap();
    later_budget.error = Some("room budget reached".into());
    let mut run = scheduled(10, LoopRunResult::Completed);
    run.run_id = Some("run_first".into());
    let mut later_run = run.clone();
    later_run.run_id = Some("run_second".into());
    let mut judge = scheduled(10, LoopRunResult::CheckSkipped);
    judge.judge = vec![
        serde_json::from_value(serde_json::json!({
            "profile": "review", "kind": "claude", "verdict": true
        }))
        .unwrap(),
    ];
    let mut declined = judge.clone();
    declined.judge[0].verdict = Some(false);
    let collided = vec![
        overlap(10),
        scheduled(20, LoopRunResult::Completed),
        overlap(30),
        overlap(40),
        scheduled(50, LoopRunResult::Completed),
    ];
    let cases = [
        vec![watch, later_watch],
        vec![budget, later_budget],
        vec![run, later_run],
        vec![judge, declined],
        collided,
    ];
    let actual = cases
        .iter()
        .map(|records| {
            collapsed_run_rows(records)
                .iter()
                .map(|row| (row.count, row.skipped))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        [
            vec![(2, 0)],
            vec![(2, 0)],
            vec![(1, 0), (1, 0)],
            vec![(1, 0), (1, 0)],
            vec![(2, 3)]
        ]
    );
    let table = runs_table(&cases[1], 10);
    assert!(table.contains("over 10s · room budget reached"), "{table}");
}

#[test]
fn collapsed_judge_skips_sum_spend_and_tokens_and_range_durations() {
    let records = [
        (10, 1000, Some(0.1), Some(1000), Some(100)),
        (20, 2000, Some(0.2), Some(2000), Some(200)),
        (30, 1500, None, None, None),
    ]
    .map(|(second, duration, cost, input, output)| {
        let mut row = scheduled(second, LoopRunResult::CheckSkipped);
        row.judge = vec![
            serde_json::from_value(serde_json::json!({
                "profile": "review", "kind": "claude", "verdict": false, "reason": "not yet"
            }))
            .unwrap(),
        ];
        row.duration_ms = Some(duration);
        row.cost_usd = cost;
        row.input_tokens = input;
        row.output_tokens = output;
        row
    });
    let table = runs_table(&records, 10);
    assert_eq!(
        (
            table.contains("judge declined ×3"),
            table.contains("$0.30"),
            table.contains("↘ 3k ↗ 300"),
            table.contains("1.0s–2.0s"),
            table.contains("over 20s · not yet")
        ),
        (true, true, true, true, true),
        "{table}"
    );
}

#[test]
fn detail_index_keeps_the_acting_run_across_refusals() {
    let mut error = record(10, LoopRunResult::Errored);
    error.error = Some("reading prompt-file\nmissing".to_owned());
    let mut failed = record(20, LoopRunResult::Failed);
    failed.run_id = Some("run_0123456789abcdef01234567".to_owned());
    let mut records = vec![error, failed];
    assert_eq!(detail_index(&records), Some(1));

    let mut skipped = record(30, LoopRunResult::BudgetSkipped);
    skipped.error = Some("daily budget reached".to_owned());
    records.push(skipped);
    assert_eq!(detail_index(&records), Some(1));

    let mut recovered = record(40, LoopRunResult::Completed);
    recovered.check = Some(check(Some(0), "ok"));
    records.push(recovered);
    assert_eq!(detail_index(&records), Some(3));
}

#[test]
fn detail_indices_skip_a_trailing_overlap() {
    let mut completed = record(10, LoopRunResult::Completed);
    completed.check = Some(check(Some(0), "ok"));
    let records = vec![completed, overlap(20)];

    assert_eq!(detail_index(&records), Some(0));
    assert!(last_run(&records).contains("LAST RUN — ✓ completed"));
}

#[test]
fn detail_indices_skip_last_run_after_a_clean_launch_even_with_a_trailing_overlap() {
    let mut error = record(10, LoopRunResult::Errored);
    error.error = Some("launch failed".to_owned());
    let mut launched = record(20, LoopRunResult::Launched);
    launched.checkout = Some(PathBuf::from("/repo/lane"));
    launched.target = Some("@fixer".to_owned());
    let mut records = vec![error, launched];

    assert_eq!(detail_index(&records), None);
    records.push(overlap(30));
    assert!(!last_run(&records).contains("LAST RUN"));
}

#[test]
fn render_record_detail_titles_status_age_and_mode() {
    let mut detail = record(20, LoopRunResult::Errored);
    detail.mode = Some(LoopRunMode::Manual);
    detail.error = Some("outer error\ninner detail".to_owned());
    detail.cost_usd = Some(0.42);
    detail.input_tokens = Some(12_000);
    detail.output_tokens = Some(3_400);
    let entry = TaskEntry {
        root: PathBuf::from("/tmp/rimz-run"),
        ..TaskEntry::default()
    };
    let mut out = Vec::new();

    render_record_detail(
        &mut out,
        &entry,
        &detail,
        "LAST FAILURE",
        "rimz loop logs wait -n 1",
        Timestamp::from_second(30).expect("timestamp"),
        ui::prose::Prose::Raw,
    )
    .unwrap();

    let raw = String::from_utf8(out).unwrap();
    assert!(raw.contains(&ui::paint(ui::palette::muted(), "  error:")));
    let out = anstream::adapter::strip_str(&raw).to_string();
    assert!(out.contains("LAST FAILURE — ✗ error · "));
    assert!(out.contains(" · manual"));
    assert!(out.contains("  error:\n  │ outer error\n  │ inner detail"));
    assert!(out.contains("  cost: $0.42 · ↘ 12k ↗ 3k"));
}

#[test]
fn render_record_detail_marks_failed_check_output() {
    let mut detail = record(20, LoopRunResult::Failed);
    detail.check = Some(CheckRecord {
        duration_ms: None,
        agent: None,
        output_path: None,
        code: Some(2),
        timed_out: false,
        output: "first line\nsecond line".to_owned(),
    });
    let entry = TaskEntry {
        root: PathBuf::from("/tmp/rimz-run"),
        ..TaskEntry::default()
    };
    let mut out = Vec::new();

    render_record_detail(
        &mut out,
        &entry,
        &detail,
        "LAST FAILURE",
        "rimz loop logs wait -n 1",
        Timestamp::from_second(30).expect("timestamp"),
        ui::prose::Prose::Raw,
    )
    .unwrap();

    let raw = String::from_utf8(out).unwrap();
    assert!(raw.contains(&ui::paint(ui::palette::alarm(), "first line")));
    let out = anstream::adapter::strip_str(&raw).to_string();
    assert!(out.contains("LAST FAILURE — ✗ failed (exit 2)"));
    assert!(out.contains("  │ first line\n  │ second line"));
}

#[test]
fn last_run_header_carries_duration_exit_and_signal() {
    let mut completed = scheduled(90, LoopRunResult::Completed);
    completed.duration_ms = Some(5_000);
    completed.check = Some(check(Some(0), "ok"));
    completed.signal = Some(rimz::harness::schedule::run_log::SignalRecord {
        name: "trunk.moved".parse().unwrap(),
        payload: Default::default(),
    });

    let out = last_run(&[completed.clone()]);
    assert!(
        out.contains("LAST RUN — ✓ completed · 10s ago · 5.0s · exit 0 · signal trunk.moved\n"),
        "{out}"
    );
    assert!(!out.contains("signal:"), "{out}");

    completed.mode = Some(LoopRunMode::Manual);
    completed.signal = None;
    let out = last_run(&[completed]);
    assert!(
        out.contains("LAST RUN — ✓ completed · 10s ago · 5.0s · exit 0 · manual\n"),
        "{out}"
    );
}

#[test]
fn last_run_omits_empty_scheduled_output_but_keeps_manual_and_signal_context() {
    let mut completed = scheduled(90, LoopRunResult::Completed);
    completed.check = Some(check(Some(0), " \n\t"));
    let scheduled = last_run(&[completed.clone()]);
    completed.mode = Some(LoopRunMode::Manual);
    let manual = last_run(&[completed.clone()]);
    completed.mode = Some(LoopRunMode::Scheduled);
    completed.signal = Some(run_log::SignalRecord {
        name: "ci.passed".parse().unwrap(),
        payload: Default::default(),
    });
    let signal = last_run(&[completed]);
    assert_eq!(
        (
            scheduled.is_empty(),
            manual.contains("LAST RUN") && manual.contains(" · manual\n  │ -\n"),
            signal.contains("LAST RUN") && signal.contains(" · signal ci.passed\n  │ -\n"),
        ),
        (true, true, true),
        "{scheduled}\n{manual}\n{signal}"
    );
}

#[test]
fn last_run_cuts_a_passing_check_to_its_tail_and_points_at_the_rest() {
    let seven = "l1\nl2\nl3\nl4\nl5\nl6\nl7\n";
    let mut passed = scheduled(80, LoopRunResult::Completed);
    passed.check = Some(check(Some(0), seven));
    let out = last_run(&[passed.clone(), overlap(90)]);
    assert!(
        out.contains("exit 0\n  │ l3\n  │ l4\n  │ l5\n  │ l6\n  │ l7\n"),
        "{out}"
    );
    assert!(
        out.ends_with("  full output: rimz loop logs wait -n 2\n"),
        "{out}"
    );

    passed.check = Some(check(Some(0), "l1\nl2\nl3\nl4\nl5\n"));
    let out = last_run(&[passed]);
    assert!(out.contains("  │ l1\n  │ l2\n"), "{out}");
    assert!(!out.contains("full output"), "{out}");

    let mut failed = scheduled(80, LoopRunResult::Failed);
    failed.check = Some(check(Some(1), seven));
    let mut gated = scheduled(80, LoopRunResult::CheckSkipped);
    gated.check = Some(check(Some(1), seven));
    assert!(last_run(&[gated.clone()]).is_empty());
    for record in [failed, gated] {
        let mut out = Vec::new();
        write_log_records(&mut out, None, &[&record]).unwrap();
        let out = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
        assert!(out.contains("  │ l1\n  │ l2\n  │ l3\n"), "{out}");
        assert!(!out.contains("full output"), "{out}");
    }
}

#[test]
fn overlaps_fold_into_the_run_that_follows_them() {
    let mut records = (1..=4).map(overlap).collect::<Vec<_>>();
    let mut failed = scheduled(5, LoopRunResult::Failed);
    failed.error = Some("checkout changed".to_owned());
    records.push(failed);

    let rows = collapsed_run_rows(&records);
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].count, rows[0].skipped), (1, 4));
    let table = runs_table(&records, 10);
    assert!(!table.contains("overlapped"), "{table}");
    assert!(
        table.contains("checkout changed · 4 fires skipped, run already active"),
        "{table}"
    );
    assert!(
        table.contains("RECENT RUNS (newest first · 5 of 5)"),
        "{table}"
    );

    let records = vec![
        scheduled(10, LoopRunResult::Completed),
        overlap(20),
        scheduled(30, LoopRunResult::Completed),
    ];
    let rows = collapsed_run_rows(&records);
    let table = runs_table(&records, 10);
    assert_eq!(
        (
            rows.iter()
                .map(|row| (row.count, row.skipped))
                .collect::<Vec<_>>(),
            table.matches("✓ completed").count(),
            table.contains("✓ completed ×2"),
        ),
        (vec![(1, 0), (1, 1)], 2, false),
        "{table}"
    );
    assert!(
        table.contains("1 fire skipped, run already active"),
        "{table}"
    );
}

#[test]
fn trailing_at_lock_fires_take_no_row_and_keep_the_stale_verdict() {
    let mut records = vec![
        scheduled(10, LoopRunResult::Completed),
        overlap(20),
        overlap(30),
    ];
    let table = runs_table(&records, 10);
    let views = [false, true].map(|running| {
        let mut view = show_view(false);
        view.records = records.clone();
        view.records.push(deferred(40));
        view.in_flight = running.then_some(InFlightRun {
            holder: None,
            run: None,
            held: Vec::new(),
        });
        show_text(&view)
    });
    assert_eq!(
        (
            table.contains("overlapped"),
            table.matches("✓ completed").count(),
            runs_table(&[overlap(10), deferred(20)], 10).is_empty(),
            views
                .iter()
                .all(|view| !view.contains("○ overlapped") && !view.contains("○ deferred")),
        ),
        (false, 1, true, true),
        "{table}\n{views:?}"
    );
    assert_eq!(
        stale_clause(&records, false).as_deref(),
        Some("last 2 fires skipped, nothing has run since")
    );
    assert_eq!(
        stale_clause(&records, true).as_deref(),
        Some("2 fires skipped while the active run holds the lock")
    );

    records.push(scheduled(40, LoopRunResult::Completed));
    assert_eq!(
        stale_clause(&records, false).as_deref(),
        Some("fires skipped during the last run, nothing has run since")
    );
    assert_eq!(stale_clause(&records, true), None);

    records.push(scheduled(50, LoopRunResult::Completed));
    assert_eq!(stale_clause(&records, false), None);
    assert_eq!(
        stale_clause(&[overlap(10)], false).as_deref(),
        Some("last fire skipped, nothing has run since")
    );

    // A fire refused at an earlier gate ran nothing: it is neither the run a
    // skipped fire waits for nor a skipped fire itself, and it does not end a count.
    let completed = |second| scheduled(second, LoopRunResult::Completed);
    for refused in [
        LoopRunResult::BudgetSkipped,
        LoopRunResult::AccountSkipped,
        LoopRunResult::SurplusSkipped,
    ] {
        let refused = |second| scheduled(second, refused);
        let clause = |records: &[LoopRunRecord], has_active_run| {
            stale_clause(records, has_active_run).unwrap_or_default()
        };
        assert_eq!(
            clause(&[overlap(10), completed(20), refused(30)], false),
            "a fire skipped during the last run, nothing has run since"
        );
        assert_eq!(clause(&[overlap(10), completed(20), refused(30)], true), "");
        assert_eq!(
            clause(&[completed(10), overlap(20), refused(30)], true),
            "1 fire skipped while the active run holds the lock"
        );
        // An overlap with another refused fire after it is no longer the last fire.
        assert_eq!(
            clause(&[completed(10), overlap(20), refused(30)], false),
            "1 fire skipped, nothing has run since"
        );
        assert_eq!(
            clause(&[overlap(10), refused(20)], false),
            "1 fire skipped, nothing has run since"
        );
        assert_eq!(
            clause(&[completed(10), refused(20), overlap(30)], false),
            "last fire skipped, nothing has run since"
        );
        assert_eq!(
            clause(
                &[completed(10), refused(20), overlap(30), overlap(40)],
                false
            ),
            "last 2 fires skipped, nothing has run since"
        );
        assert_eq!(
            clause(
                &[
                    completed(10),
                    overlap(20),
                    refused(30),
                    overlap(40),
                    completed(50)
                ],
                false
            ),
            "fires skipped during the last run, nothing has run since"
        );
        assert_eq!(
            clause(
                &[
                    completed(10),
                    refused(20),
                    overlap(30),
                    refused(40),
                    overlap(50)
                ],
                false
            ),
            "2 fires skipped, nothing has run since"
        );
        assert_eq!(clause(&[completed(10), refused(20)], false), "");
        assert_eq!(clause(&[refused(10)], false), "");
    }
}

#[test]
fn runs_table_is_newest_first_and_limits_from_the_newest() {
    let records = vec![
        scheduled(10, LoopRunResult::Failed),
        scheduled(20, LoopRunResult::Completed),
    ];
    let table = runs_table(&records, 10);
    assert!(
        table.find("✓ completed").unwrap() < table.find("✗ failed").unwrap(),
        "{table}"
    );

    let table = runs_table(&records, 1);
    assert!(table.contains("✓ completed"), "{table}");
    assert!(!table.contains("✗ failed"), "{table}");
    assert!(
        table.contains("RECENT RUNS (newest first · 1 of 2)"),
        "{table}"
    );
}

#[test]
fn runs_table_drops_uniform_mode_and_empty_cost() {
    let mut records = vec![
        scheduled(10, LoopRunResult::Failed),
        scheduled(20, LoopRunResult::Completed),
    ];
    let table = runs_table(&records, 10);
    assert!(
        !table.contains("MODE") && !table.contains("COST"),
        "{table}"
    );

    let mut manual = record(30, LoopRunResult::Failed);
    manual.mode = Some(LoopRunMode::Manual);
    manual.cost_usd = Some(0.25);
    records.push(manual);
    let table = runs_table(&records, 10);
    assert!(table.contains("MODE"), "{table}");
    assert!(table.contains("manual"), "{table}");
    assert!(table.contains("COST"), "{table}");
    assert!(table.contains("$0.25"), "{table}");
}

#[test]
fn agent_runs_are_newest_first() {
    let mut out = Vec::new();
    write_agent_runs(
        &mut out,
        &[
            record(10, LoopRunResult::TargetGone),
            record(20, LoopRunResult::Delivered),
        ],
        Timestamp::from_second(100).unwrap(),
    )
    .unwrap();
    let out = anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string();
    assert!(
        out.find("delivered").unwrap() < out.find("target gone").unwrap(),
        "{out}"
    );
}

#[test]
fn loop_logs_forensics_print_a_signal_payload_under_its_name() {
    use super::super::run_report::{Forensics, write_record_forensics};
    use rimz::harness::schedule::run_log::SignalRecord;

    let render = |payload: serde_json::Map<String, serde_json::Value>| {
        let mut detail = record(20, LoopRunResult::Delivered);
        detail.signal = Some(SignalRecord {
            name: "deploy.finished".parse().expect("signal name"),
            payload,
        });
        let mut out = Vec::new();
        write_record_forensics(
            &mut out,
            None,
            &detail,
            ui::prose::Prose::Raw,
            Forensics::Full,
        )
        .unwrap();
        anstream::adapter::strip_str(&String::from_utf8(out).unwrap()).to_string()
    };

    let payload = serde_json::json!({"detail": {"a": 1}, "env": "prod"});
    let out = render(payload.as_object().expect("object").clone());
    assert!(
        out.contains("  signal: deploy.finished\n  {\"detail\":{\"a\":1},\"env\":\"prod\"}\n"),
        "{out}"
    );

    let out = render(serde_json::Map::new());
    assert!(out.ends_with("  signal: deploy.finished\n"), "{out}");
}

#[test]
fn watch_history_uses_verdict_words_and_output_path() {
    use rimz::harness::schedule::signal::WatchVerdict;

    for (verdict, expected) in [
        (
            WatchVerdict::Exited {
                code: Some(0),
                elapsed_ms: 3_000,
            },
            "exit 0 after 3s",
        ),
        (
            WatchVerdict::Exited {
                code: Some(3),
                elapsed_ms: 3_000,
            },
            "exit 3 after 3s",
        ),
        (
            WatchVerdict::Exited {
                code: None,
                elapsed_ms: 3_000,
            },
            "killed by signal after 3s",
        ),
        (
            WatchVerdict::TimedOut { elapsed_ms: 3_000 },
            "timed out after 3s",
        ),
        (
            WatchVerdict::Lost {
                detail: "watcher process exited without reporting".to_owned(),
                elapsed_ms: 3_000,
            },
            "watcher died after 3s; the command may still be running or may have died with it",
        ),
    ] {
        for result in [
            LoopRunResult::Delivered,
            LoopRunResult::Failed,
            LoopRunResult::TimedOut,
            LoopRunResult::CheckSkipped,
        ] {
            let mut detail = record(20, result);
            detail.watch = Some(verdict.clone());
            detail.duration_ms = Some(0);
            detail.check = Some(CheckRecord {
                duration_ms: None,
                agent: None,
                code: match verdict {
                    WatchVerdict::Exited { code, .. } => code,
                    _ => None,
                },
                timed_out: matches!(verdict, WatchVerdict::TimedOut { .. }),
                output: "last line".to_owned(),
                output_path: Some("/tmp/wait.log".into()),
            });
            assert_eq!(record_exit(&detail).as_deref(), Some(expected));
            let mut out = Vec::new();
            render_record_detail(
                &mut out,
                &TaskEntry::default(),
                &detail,
                "last run",
                "rimz loop logs wait -n 1",
                Timestamp::from_second(30).unwrap(),
                ui::prose::Prose::Raw,
            )
            .unwrap();
            let raw = String::from_utf8(out).unwrap();
            let out = anstream::adapter::strip_str(&raw).to_string();
            assert_eq!(out.matches(expected).count(), 1, "{out}");
            assert!(
                out.contains("  output: /tmp/wait.log\n  │ last line"),
                "{out}"
            );
            assert!(!out.contains("after 3s in"), "{out}");
            let mut table = Vec::new();
            write_runs_table(
                &mut table,
                &[detail],
                5,
                Timestamp::from_second(30).unwrap(),
            )
            .unwrap();
            let table = String::from_utf8(table).unwrap();
            assert!(table.contains("3.0s"), "{table}");
            assert!(!table.contains("0ms"), "{table}");
        }
    }
}

#[test]
fn a_fan_out_shows_its_running_checkout_and_the_others_waiting_beside_it() {
    let now = Timestamp::from_second(100_000).unwrap();
    let running = RunLockInfo {
        pid: 41,
        started_at: now - jiff::SignedDuration::from_secs(120),
    };
    let since_ms = u64::try_from((now - jiff::SignedDuration::from_secs(30)).as_millisecond())
        .expect("after the epoch");
    let waiting = |pid: u32, checkout: &str, reason: Option<&str>, position| Held {
        pid,
        checkout: checkout.into(),
        since_ms,
        reason: reason.map(str::to_owned),
        kinds: Vec::new(),
        gives_up_ms: None,
        position,
    };
    // Checkout a launched and runs; b and c wait in other processes.
    let held = [
        waiting(42, "/repo/.worktrees/b", Some("2 starts ahead"), 3),
        waiting(43, "/repo/.worktrees/c", None, 4),
    ];
    assert_eq!(
        in_flight_text(Some(running), &held, now),
        "▸ running 2m · 2 held: 2 starts ahead"
    );
    let in_flight = InFlightRun {
        holder: Some(running),
        run: None,
        held: held.to_vec(),
    };
    assert_eq!(
        running_text_full(&in_flight, now),
        "▸ running 2m · pid 41 · 2 held: 2 starts ahead"
    );
    let (own, others) = split_held(Some(running), &held);
    assert!(own.is_none());
    assert_eq!(others.map(|held| held.pid).collect::<Vec<_>>(), [42, 43]);

    // The holder is itself held: its own reason, never another checkout's.
    let held = [
        waiting(42, "/repo/.worktrees/b", Some("1 start ahead"), 2),
        waiting(41, "/repo/.worktrees/a", Some("cpu pressure 41% >= 25%"), 3),
    ];
    assert_eq!(
        in_flight_text(Some(running), &held, now),
        format!(
            "{} · cpu pressure 41% >= 25% · 1 held: 1 start ahead",
            rimz::theme::fmt::capacity_wait(
                &held[1].kinds,
                Timestamp::from_millisecond(held[1].since_ms as i64).unwrap(),
                None
            )
        )
    );
    let (own, _) = split_held(Some(running), &held);
    assert_eq!(own.map(|held| held.pid), Some(41));
    // A holder the lock does not name owns no ticket.
    assert_eq!(
        in_flight_text(None, &held, now),
        "▸ running · 2 held: 1 start ahead"
    );
}

#[test]
fn a_sub_cent_run_cost_is_not_shown_as_free() {
    assert_eq!(run_cost(0.0), "$0.00");
    assert_eq!(run_cost(0.0031), "<$0.01");
    assert_eq!(run_cost(0.006), "$0.01");
    assert_eq!(run_cost(1.239), "$1.24");
}

#[test]
fn capacity_blocking_display_uses_the_shared_phrase_without_changing_held_json() {
    let since: Timestamp = "2026-06-01T15:02:00Z".parse().unwrap();
    let held = Held {
        pid: 42,
        checkout: "/repo".into(),
        since_ms: since.as_millisecond() as u64,
        kinds: vec![rimz::ids::AgentKind::new_unchecked("claude")],
        gives_up_ms: None,
        reason: Some("1 start ahead".into()),
        position: 2,
    };
    let phrase = rimz::theme::fmt::capacity_wait(&held.kinds, since, None);
    assert_eq!(held_run_text(&held), format!("{phrase} · 1 start ahead"));
    assert!(held_json(&held).get("kinds").is_none());
    assert!(held_json(&held).get("gives_up_ms").is_none());
}
