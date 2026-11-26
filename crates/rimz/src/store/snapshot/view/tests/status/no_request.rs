use super::*;
use crate::store::snapshot::view::model::needs_you;

const SENTENCE: &str = "sent no request to the model 1m after the prompt; look at its pane";

/// A running Claude row at the request-record floor whose turn opened `turn_age` seconds
/// ago, with the prompt as its last activity and no request record.
fn unrequested(id: &str, turn_age: i64) -> AgentState {
    records_request_starts(
        agent("claude", id, AgentStatus::Running, 0)
            .worktree("/repo/main")
            .active_ago(turn_age)
            .turn_started_ago(turn_age),
    )
}

fn prior_turn(started_ago: i64, ended_ago: i64) -> Option<crate::agents::state::PriorTurn> {
    serde_json::from_value(serde_json::json!({
        "started_at": ago(started_ago),
        "ended_at": ago(ended_ago),
    }))
    .ok()
}

#[test]
fn a_turn_with_no_model_request_past_the_window_reads_needs_you() {
    let snapshot = room_with_agent_panes(vec![unrequested("root", 61)]);
    let row = row(&snapshot, "root");
    assert_eq!(row.status(), Some(AgentStatus::Waiting));
    assert_eq!(row.turn_error_label(), Some(SENTENCE));
    assert_eq!(
        row.as_agent().and_then(|card| card.overlay_raised_at),
        Some(ago(1)),
        "the alert opens at the turn start plus the window, never at the fold's clock"
    );
    let agent = rollup_agent(&snapshot, "root");
    assert_eq!(agent.status, AgentStatus::Running);
    assert_eq!(agent.effective_status(epoch()), AgentStatus::Running);
    let attention = needs_you(agent, Some(row), None, &Default::default(), epoch())
        .expect("the alerted card needs the user");
    assert_eq!(
        (
            attention.status,
            attention.reason.as_deref(),
            attention.since
        ),
        (AgentStatus::Waiting, Some(SENTENCE), ago(1))
    );
}

#[test]
fn progress_or_an_ungated_row_keeps_the_open_turn_running() {
    type Case = (&'static str, AgentState, Option<AgentState>);
    let with = |mutate: fn(&mut AgentState)| {
        let mut agent = unrequested("root", 61);
        mutate(&mut agent);
        agent
    };
    let cases: Vec<Case> = vec![
        ("inside the window", unrequested("root", 59), None),
        (
            "a request at turn open",
            with(|agent| {
                agent.context.as_mut().unwrap().recorded_request_at = Some(ago(61));
            }),
            None,
        ),
        (
            "a tool at turn open",
            with(|agent| agent.last_tool_at = Some(ago(61))),
            None,
        ),
        (
            "compacting",
            with(|agent| agent.compacting_since = Some(ago(30))),
            None,
        ),
        (
            "a compaction started this turn, past its window",
            {
                let mut agent = unrequested("root", 200);
                agent.compacting_since = Some(ago(150));
                agent
            },
            None,
        ),
        (
            "a compaction finished this turn",
            with(|agent| agent.compacted_at = Some(ago(30))),
            None,
        ),
        // The fold's shape after a restamp on an open turn: the start moves to the steer or
        // the child's start, while the close and the carry stay at the turn before
        // (`project::tests::timestamps::a_restamp_on_an_open_turn_keeps_the_earlier_close_and_carry`).
        (
            "a prompt landing on an open turn that reached the model",
            with(|agent| {
                agent.turn_ended_at = Some(ago(400));
                agent.prior_turn = prior_turn(500, 400);
                agent.context.as_mut().unwrap().recorded_request_at = Some(ago(299));
            }),
            None,
        ),
        (
            "a background child's start on an open turn that reached the model, the child gone",
            with(|agent| {
                agent.turn_ended_at = Some(ago(400));
                agent.prior_turn = prior_turn(500, 400);
                agent.last_tool_at = Some(ago(300));
                agent.context.as_mut().unwrap().recorded_request_at = Some(ago(299));
            }),
            None,
        ),
        (
            "a keep-warm ping landing on an open turn that reached the model",
            with(|agent| {
                agent.turn_ended_at = Some(ago(400));
                agent.prior_turn = prior_turn(500, 400);
                (agent.ping_opened_at, agent.pinged_at) = (Some(ago(30)), Some(ago(20)));
                // The turn's request predates its restamp: only the close bound holds it, and
                // a ping closing inside the turn must not move that bound past it.
                agent.context.as_mut().unwrap().recorded_request_at = Some(ago(100));
            }),
            None,
        ),
        (
            "a first turn's restamp on a session with no closed turn",
            with(|agent| {
                agent.registered_at = Some(ago(500));
                agent.context.as_mut().unwrap().recorded_request_at = Some(ago(299));
            }),
            None,
        ),
        (
            "a live child",
            unrequested("root", 61),
            Some(child_state("root", "child-1", AgentStatus::Running, 5)),
        ),
        (
            "a delegated wait",
            unrequested("root", 61),
            Some(child_state("root", "child-wait", AgentStatus::Waiting, 5)),
        ),
        (
            "a version below the floor",
            with(|agent| {
                agent.context.as_mut().unwrap().agent_version = Some("2.1.299".to_owned());
            }),
            None,
        ),
        (
            "an unknown version",
            with(|agent| agent.context.as_mut().unwrap().agent_version = None),
            None,
        ),
        (
            "an unparseable version",
            with(|agent| {
                agent.context.as_mut().unwrap().agent_version = Some("next".to_owned());
            }),
            None,
        ),
        (
            "a kind that records no request start",
            {
                let mut agent = unrequested("root", 61);
                agent.kind = AgentKind::new_unchecked("codex");
                agent
            },
            None,
        ),
    ];
    for (case, agent, child) in cases {
        let delegated = child
            .as_ref()
            .is_some_and(|child| child.status == AgentStatus::Waiting);
        let snapshot = room_with_agent_panes(std::iter::once(agent).chain(child).collect());
        let row = row(&snapshot, "root");
        let expected = if delegated {
            AgentStatus::Waiting
        } else {
            AgentStatus::Running
        };
        assert_eq!(row.status(), Some(expected), "{case}");
        assert_eq!(row.turn_error_label(), None, "{case}");
    }
}

#[test]
fn a_previous_turns_request_does_not_clear_the_next_turns_alert() {
    // The last case is the fold's shape after a keep-warm ping at rest
    // (`project::tests::timestamps::a_ping_at_rest_moves_only_the_ping_stamps`): its request
    // record is newer than the last turn's close, and the ping's own close bounds it.
    let pinged = Some((ago(302), ago(290)));
    for (prior, ended, ping) in [
        (None, None, None),
        (prior_turn(300, 200), Some(ago(200)), None),
        (prior_turn(500, 400), Some(ago(400)), pinged),
    ] {
        let mut agent = unrequested("root", 61);
        agent.prior_turn = prior;
        agent.turn_ended_at = ended;
        (agent.ping_opened_at, agent.pinged_at) = ping.unzip();
        agent.registered_at = Some(ago(250));
        agent.context.as_mut().unwrap().recorded_request_at = Some(ago(299));
        let snapshot = room_with_agent_panes(vec![agent]);
        let row = row(&snapshot, "root");
        assert_eq!(row.status(), Some(AgentStatus::Waiting), "prior {prior:?}");
        assert_eq!(row.turn_error_label(), Some(SENTENCE), "prior {prior:?}");
    }
}

#[test]
fn configured_no_request_window_moves_the_alert() {
    assert_eq!(
        crate::config::AttentionConfig::default()
            .no_request_after_secs
            .get(),
        60
    );
    let project = |turn_age| {
        let mut snapshot =
            list_agent_checkouts(room(vec![unrequested("root", turn_age).in_pane("%1")]));
        snapshot.attention.no_request_after_secs =
            std::num::NonZeroU32::new(30).expect("non-zero test window");
        snapshot.with_live_panes(vec![pane("%1", "claude", "/repo/main")], None)
    };
    let below = project(29);
    assert_eq!(row(&below, "root").status(), Some(AgentStatus::Running));
    let at = project(30);
    let alerted = row(&at, "root");
    assert_eq!(alerted.status(), Some(AgentStatus::Waiting));
    assert_eq!(
        alerted.turn_error_label(),
        Some("sent no request to the model 30s after the prompt; look at its pane")
    );
}

/// The stuck session's transcript: the prompt and its attachment lines, and no request
/// record, until Claude appends one when the menu in its pane is answered. A sidebar tick
/// reads the line with no hook, and the next fold clears the alert.
#[test]
fn the_alert_clears_on_the_fold_after_the_request_record_lands() {
    use crate::agents::context::record::AgentContextRecord;
    use std::io::Write as _;

    let definition = crate::agents::find_definition("claude").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let line = |value: serde_json::Value| format!("{value}\n");
    std::fs::write(
        &path,
        [
            line(serde_json::json!({"type": "user", "isSidechain": false,
                "timestamp": ago(61).to_string(),
                "message": {"role": "user", "content": "keep going"}})),
            line(
                serde_json::json!({"type": "attachment", "isSidechain": false,
                "timestamp": ago(61).to_string(), "attachment": {"type": "todo"}}),
            ),
        ]
        .concat(),
    )
    .unwrap();
    let mut record = AgentContextRecord::new("claude", "root", Default::default());
    record.context.agent_version = Some("2.1.300".to_owned());
    let project = |record: &mut AgentContextRecord| {
        let refresh = definition
            .local_context_refresh(
                crate::agents::RefreshTrigger::Tick,
                &crate::agents::LocalContextRefreshCtx {
                    open_permission_since: None,
                    prior_permission_settle: None,
                    login_env: &BTreeMap::new(),
                    agent_id: "root",
                    model_hint: None,
                    prior_session_name: None,
                    current_transcript_path: path.to_str(),
                    prior_transcript_path: path.to_str(),
                    prior_transcript_stat: record.transcript_stat.as_ref(),
                    prior_spend_fold: record.spend_fold.as_ref(),
                    prior_model_context_limits_key: None,
                    shared_pricing_cache_path: &dir.path().join("pricing.json"),
                },
            )
            .unwrap();
        record.apply_local_refresh(definition.spec(), refresh, epoch());
        let mut agent = unrequested("root", 61);
        agent.context = Some(record.context.clone());
        let snapshot = room_with_agent_panes(vec![agent]);
        row(&snapshot, "root").status()
    };
    assert_eq!(project(&mut record), Some(AgentStatus::Waiting));
    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap(),
        "{}",
        serde_json::json!({"type": "api-request", "timestamp": ago(1).to_string(),
            "querySource": "repl_main_thread"})
    )
    .unwrap();
    assert_eq!(project(&mut record), Some(AgentStatus::Running));
}

#[test]
fn a_nested_launched_child_held_past_the_window_asks_through_its_parent() {
    let launched = |turn_age| {
        let mut child = records_request_starts(
            child_state("root", "child", AgentStatus::Running, 5)
                .active_ago(turn_age)
                .turn_started_ago(turn_age),
        );
        child.launch_depth = Some(1);
        child.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
        child
    };
    let parent = || {
        agent("claude", "root", AgentStatus::Running, 0)
            .worktree("/repo/main")
            .active_ago(5)
    };
    let held = room_with_agent_panes(vec![parent(), launched(61)]);
    let root = row(&held, "root");
    let line = &root.sub_agents()[0];
    assert_eq!(
        (line.status, line.turn_error_label.as_deref()),
        (AgentStatus::Waiting, Some(SENTENCE))
    );
    assert_eq!(
        (
            root.status(),
            root.delegated_wait().map(|wait| wait.child_id.as_str())
        ),
        (Some(AgentStatus::Waiting), Some("child")),
        "the parent asks for the child it waits on"
    );
    let child = rollup_agent(&held, "child");
    let asks = needs_you(child, None, Some(line), &Default::default(), epoch())
        .expect("the held child needs the user");
    assert_eq!(
        (asks.reason.as_deref(), asks.since),
        (Some(SENTENCE), child.last_activity),
        "the CLI reason for a nested child is its line's sentence, aged as before"
    );
    assert_eq!(
        root.delegated_wait()
            .and_then(|wait| wait.detail.as_deref()),
        Some(SENTENCE),
        "the parent's wait names the child's reason"
    );
    let parent_asks = needs_you(
        rollup_agent(&held, "root"),
        Some(root),
        None,
        &Default::default(),
        epoch(),
    );
    assert!(
        parent_asks
            .and_then(|asks| asks.reason)
            .is_some_and(|reason| reason.ends_with(SENTENCE)),
        "the parent's CLI reason names the child's reason"
    );

    let inside = room_with_agent_panes(vec![parent(), launched(59)]);
    let root = row(&inside, "root");
    assert_eq!(root.sub_agents()[0].status, AgentStatus::Running);
    assert_eq!(
        (root.status(), root.delegated_wait().is_none()),
        (Some(AgentStatus::Running), true)
    );

    let native = records_request_starts(
        child_state("root", "native", AgentStatus::Running, 5)
            .active_ago(61)
            .turn_started_ago(61),
    );
    let snapshot = room_with_agent_panes(vec![parent(), native]);
    assert_eq!(
        row(&snapshot, "root").sub_agents()[0].status,
        AgentStatus::Running,
        "a provider-native child's requests are not its main thread's"
    );
}

/// A nested child's menu answered past the stall window writes only the request line, so the
/// child's silence counts from that request as a row's stall rung does.
#[test]
fn a_nested_child_answered_past_the_stall_window_reads_running() {
    let answered = |kind: &str| {
        let mut child = records_request_starts(
            child_state("root", "child", AgentStatus::Running, 1_000).turn_started_ago(1_000),
        );
        child.kind = AgentKind::new_unchecked(kind);
        child.launch_depth = Some(1);
        child.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
        child.context.as_mut().unwrap().recorded_request_at = Some(ago(10));
        child
    };
    let parent = || {
        agent("claude", "root", AgentStatus::Running, 0)
            .worktree("/repo/main")
            .active_ago(5)
    };
    let claude = room_with_agent_panes(vec![parent(), answered("claude")]);
    let line = &row(&claude, "root").sub_agents()[0];
    assert_eq!(
        (line.status, line.stalled, line.turn_error_label.as_deref()),
        (AgentStatus::Running, false, None)
    );
    let codex = room_with_agent_panes(vec![parent(), answered("codex")]);
    let line = &row(&codex, "root").sub_agents()[0];
    assert_eq!(
        (line.status, line.stalled),
        (AgentStatus::Failed, true),
        "a kind that records no request keeps the silence clock"
    );
}

/// Esc and a turn-ending denial fire no `Stop`, so the rested turn's close is only the
/// marker the transcript scan leaves, and the next reading after the prompt no longer shows it.
#[test]
fn a_turn_that_rested_with_no_stop_keeps_its_request_from_the_next_prompt() {
    use crate::agents::context::record::AgentContextRecord;
    type Rest = fn(AgentState) -> AgentState;
    let rests: [(&str, Rest); 2] = [
        ("an interruption", |agent| {
            agent.settle(400, crate::agents::TurnSettleOutcome::Interrupted)
        }),
        ("a turn-ending denial", |agent| {
            agent.turn_error_class(400, "denied", crate::agents::TurnErrorClass::Failed)
        }),
    ];
    for (case, rest) in rests {
        for merge in ["statusline", "mod report"] {
            let mut agent = rest(unrequested("root", 61));
            agent.registered_at = Some(ago(500));
            let mut context = agent.context.take().unwrap();
            context.recorded_request_at = Some(ago(450));
            let mut record = AgentContextRecord::new("claude", "root", context);
            let mut reading = crate::agents::AgentContext::new("claude", ago(60));
            reading.agent_version = Some("2.1.300".to_owned());
            if merge == "statusline" {
                record.apply_context_refresh("claude", "root", reading, None, true);
            } else {
                record.merge_mod_reading("claude", reading, None);
            }
            agent.context = Some(record.context);
            let snapshot = room_with_agent_panes(vec![agent]);
            assert_eq!(
                row(&snapshot, "root").status(),
                Some(AgentStatus::Waiting),
                "{case}, then a {merge} reading after the prompt"
            );
        }
    }
}

/// A menu held past the stall window and then answered writes only the request line, so the
/// turn reads running from that request until the next hook, not failed.
#[test]
fn a_request_landing_past_the_stall_window_reads_running() {
    let answered = |kind: &str| {
        let mut agent = unrequested("root", 1_000);
        agent.kind = AgentKind::new_unchecked(kind);
        agent.context.as_mut().unwrap().recorded_request_at = Some(ago(10));
        agent
    };
    let claude = room_with_agent_panes(vec![answered("claude")]);
    let row_claude = row(&claude, "root");
    assert_eq!(
        (row_claude.status(), row_claude.turn_error_label()),
        (Some(AgentStatus::Running), None)
    );
    let codex = room_with_agent_panes(vec![answered("codex")]);
    assert_eq!(
        row(&codex, "root").status(),
        Some(AgentStatus::Failed),
        "a kind that records no request keeps the stall clock"
    );
}
