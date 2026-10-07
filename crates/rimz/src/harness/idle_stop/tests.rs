use super::*;
use crate::agents::context::{AgentContext, TurnSettle, TurnSettleOutcome};
use crate::agents::{
    AgentLifecycleObservation, LifecycleSignal, PendingWait, PendingWaitTrigger, PermissionMode,
};
use crate::ids::MuxName;
use crate::store::idle_stop::IdleStopRequest;
use crate::store::message::{DeliveryGate, MessageRecord, MessageStatus};
use crate::store::run::{PeerRun, ReportTo, RunRecord, RunStatus};
use crate::store::snapshot::PaneAgent;

fn ts(seconds: i64) -> Timestamp {
    Timestamp::from_second(seconds).expect("timestamp")
}

fn fixture() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let id = WorkspaceId::from_project_root(dir.path());
    let state = StatePaths::under(id.clone(), &dir.path().join("state")).expect("state");
    let runtime = RuntimePaths::under(id, &dir.path().join("runtime")).expect("runtime");
    (dir, Store::open(state, runtime).expect("store"))
}

/// Rested since 1000 with a three-minute stop requested at 500.
fn rested() -> (AgentState, IdleStop) {
    let mut agent = AgentState::seed(
        AgentKind::new_unchecked("claude"),
        AgentSessionId::from("session-1"),
        AgentStatus::Idle,
        ts(1_000),
    );
    agent.name = Some("coder".to_owned());
    agent.turn_ended_at = Some(ts(1_000));
    let stop = IdleStop {
        after_secs: 180,
        requested_at: ts(500),
        requested_by: None,
    };
    (agent, stop)
}

const DUE: i64 = 1_180;

fn decide_alone(store: &Store, agent: &AgentState, stop: &IdleStop, now: Timestamp) -> Verdict {
    decide(store, std::slice::from_ref(agent), agent, stop, now).expect("decide")
}

fn verdict(store: &Store, agent: &AgentState, stop: &IdleStop, now: i64) -> Verdict {
    decide(store, std::slice::from_ref(agent), agent, stop, ts(now)).expect("decide")
}

#[test]
fn each_rollup_term_holds_the_stop_alone_and_a_clear_agent_stops() {
    let (_dir, store) = fixture();
    let (clear, stop) = rested();
    assert_eq!(
        verdict(&store, &clear, &stop, DUE),
        Verdict::Stop { idle_secs: 180 }
    );
    assert_eq!(due_at(&clear, &stop), Some(ts(DUE)));

    let hold = |name: &str, expected: Hold, change: &dyn Fn(&mut AgentState)| {
        let mut agent = clear.clone();
        change(&mut agent);
        assert_eq!(
            verdict(&store, &agent, &stop, DUE),
            Verdict::Hold(expected),
            "{name}"
        );
        assert_eq!(due_at(&agent, &stop), None, "{name}");
    };
    hold("ended", Hold::Ended, &|agent| {
        agent.ended_at = Some(ts(1_100))
    });
    hold("provider subagent", Hold::ProviderSubagent, &|agent| {
        agent.parent_agent_id = Some("parent".into());
    });
    hold("awaiting input", Hold::AwaitingInput, &|agent| {
        agent.status = AgentStatus::Waiting;
        agent.waiting_since = Some(agent.last_activity);
    });
    hold("budget park", Hold::BudgetPark, &|agent| {
        agent.budget_park = Some(crate::agents::BudgetPark {
            cap_usd: 1.0,
            spend_usd: 1.0,
            window: crate::agents::BudgetWindow::Session,
            at: ts(1_000),
            scope: crate::agents::BudgetScope::Agent,
            account_kind: None,
            resets_at: None,
        });
    });
    hold("compacting", Hold::Compacting, &|agent| {
        agent.compacting_since = Some(ts(1_100));
    });
    for status in [
        AgentStatus::Running,
        AgentStatus::Failed,
        AgentStatus::Paused,
    ] {
        hold(status.as_str(), Hold::Busy, &|agent| agent.status = status);
    }
    hold("parked on background work", Hold::Busy, &|agent| {
        agent.status = AgentStatus::Running;
        agent.phase = TurnPhase::Parked;
    });
    hold("background shell", Hold::Busy, &|agent| {
        agent
            .background_shells
            .push(crate::agents::BackgroundShell {
                id: "shell-1".to_owned(),
                command: None,
                description: None,
                started_at: ts(900),
            });
    });
    hold("sleeping on a wait", Hold::Busy, &|agent| {
        agent.pending_waits.push(PendingWait {
            name: "wait-command".to_owned(),
            trigger: PendingWaitTrigger::Command {
                command: "cargo xtask gate".to_owned(),
            },
            armed_at: Some(ts(900)),
        });
    });
}

#[test]
fn the_clock_runs_from_the_later_of_turn_end_and_request() {
    let (_dir, store) = fixture();
    let (mut agent, mut stop) = rested();
    assert_eq!(
        verdict(&store, &agent, &stop, DUE - 1),
        Verdict::Hold(Hold::Clock)
    );

    // A new turn before the deadline restarts the clock at its end.
    agent.turn_ended_at = Some(ts(1_100));
    assert_eq!(
        verdict(&store, &agent, &stop, DUE),
        Verdict::Hold(Hold::Clock)
    );
    assert_eq!(due_at(&agent, &stop), Some(ts(1_280)));
    assert_eq!(
        verdict(&store, &agent, &stop, 1_280),
        Verdict::Stop { idle_secs: 180 }
    );

    // A request against a long-idle agent waits the full duration from the request.
    stop.requested_at = ts(5_000);
    assert_eq!(
        verdict(&store, &agent, &stop, 5_179),
        Verdict::Hold(Hold::Clock)
    );
    assert_eq!(
        verdict(&store, &agent, &stop, 5_200),
        Verdict::Stop { idle_secs: 200 }
    );

    // Fractions of a second count: the stop never comes early.
    let (mut fractional, three_minutes) = rested();
    let ms = |ms: i64| Timestamp::from_millisecond(ms).expect("timestamp");
    fractional.turn_ended_at = Some(ms(1_000_900));
    assert_eq!(due_at(&fractional, &three_minutes), Some(ms(1_180_900)));
    assert_eq!(
        decide_alone(&store, &fractional, &three_minutes, ms(1_180_899)),
        Verdict::Hold(Hold::Clock)
    );
    assert_eq!(
        decide_alone(&store, &fractional, &three_minutes, ms(1_180_900)),
        Verdict::Stop { idle_secs: 180 }
    );

    // An agent that never closed a turn rests from the request.
    agent.turn_ended_at = None;
    assert_eq!(due_at(&agent, &stop), Some(ts(5_180)));

    // Zero fires at the first evaluation.
    stop.after_secs = 0;
    assert_eq!(
        verdict(&store, &agent, &stop, 5_000),
        Verdict::Stop { idle_secs: 0 }
    );
}

/// A row the lifecycle left `status` whose provider marker proves it settled at 2000.
fn settled_by_marker(status: AgentStatus, outcome: TurnSettleOutcome) -> AgentState {
    let (mut agent, _) = rested();
    agent.status = status;
    agent.last_activity = ts(1_900);
    agent.context = Some(AgentContext {
        settle: Some(TurnSettle::new(ts(2_000), outcome)),
        ..AgentContext::default()
    });
    agent
}

#[test]
fn a_turn_settled_only_by_a_provider_marker_rests_from_the_marker() {
    let (_dir, store) = fixture();
    let (_, stop) = rested();
    for (status, outcome) in [
        (AgentStatus::Running, TurnSettleOutcome::Interrupted),
        (AgentStatus::Waiting, TurnSettleOutcome::Interrupted),
        (AgentStatus::Running, TurnSettleOutcome::Complete),
    ] {
        // The previous turn ended at 1000 and the request is older still, so
        // only the marker restarts the clock.
        let agent = settled_by_marker(status, outcome);
        let case = format!("{status:?} {outcome:?}");
        assert_eq!(due_at(&agent, &stop), Some(ts(2_180)), "{case}");
        assert_eq!(
            verdict(&store, &agent, &stop, 2_179),
            Verdict::Hold(Hold::Clock),
            "{case}"
        );
        assert_eq!(
            verdict(&store, &agent, &stop, 2_180),
            Verdict::Stop { idle_secs: 180 },
            "{case}"
        );

        let request = IdleStopRequest {
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            stop: stop.clone(),
        };
        crate::store::idle_stop::arm(store.paths(), request).expect("arm");
        let mut snapshot = SidebarSnapshot::build_with_agents(
            store.paths().workspace_id.clone(),
            vec![agent],
            ts(2_179),
        );
        let mut spawned = 0;
        stop_idle_agents_with(&snapshot, store.paths(), store.runtime_paths(), |_| {
            spawned += 1;
            true
        });
        assert_eq!(
            spawned, 0,
            "the producer waits out the marker's rest: {case}"
        );
        project_requests(&mut snapshot, store.paths());
        assert_eq!(
            snapshot.agents[0]
                .idle_stop
                .as_ref()
                .and_then(|stop| stop.due_at),
            Some(ts(2_180)),
            "{case}"
        );
    }

    // A marker the session's later events passed proves nothing.
    let mut stale = settled_by_marker(AgentStatus::Running, TurnSettleOutcome::Interrupted);
    stale.last_activity = ts(2_000);
    assert_eq!(
        verdict(&store, &stale, &stop, 9_000),
        Verdict::Hold(Hold::Busy)
    );
    let mut resting = settled_by_marker(AgentStatus::Idle, TurnSettleOutcome::Complete);
    resting.last_activity = ts(1_000);
    assert_eq!(
        due_at(&resting, &stop),
        Some(ts(DUE)),
        "a hook-ended turn ignores the marker"
    );
}

#[test]
fn a_failed_compaction_restarts_the_clock() {
    let (_dir, store) = fixture();
    for signal in [
        LifecycleSignal::Registered,
        LifecycleSignal::TurnStarted { turn_id: None },
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    ] {
        observe(&store, "session-1", None, signal);
    }
    std::thread::sleep(Duration::from_millis(5));
    for signal in [
        LifecycleSignal::Compacting,
        LifecycleSignal::CompactionEnded {
            auto: Some(false),
            failed: true,
        },
    ] {
        observe(&store, "session-1", None, signal);
    }
    let agent = store.snapshot_cached().expect("snapshot").agents.remove(0);
    let ended = agent.turn_ended_at.expect("turn end");
    assert!(
        resting(&agent).is_ok(),
        "the failed compaction leaves it resting"
    );
    assert!(
        agent.last_activity > ended,
        "the compaction came after the turn end"
    );
    let stop = IdleStop {
        after_secs: 180,
        requested_at: Timestamp::UNIX_EPOCH,
        requested_by: None,
    };
    let after = jiff::SignedDuration::from_secs(180);
    assert_eq!(
        decide_alone(&store, &agent, &stop, ended + after),
        Verdict::Hold(Hold::Clock),
        "rest is counted from the compaction, not the turn before it"
    );
    assert_eq!(
        decide_alone(&store, &agent, &stop, agent.last_activity + after),
        Verdict::Stop { idle_secs: 180 }
    );
}

/// A live `rimz subagents` child of `parent`, resting with nothing owed.
fn kept_child(parent: &AgentState, id: &str) -> AgentState {
    let (mut child, _) = rested();
    child.agent_id = id.into();
    child.name = Some(id.to_owned());
    child.parent_agent_id = Some(parent.agent_id.clone());
    child.parent_agent_kind = Some(parent.kind.clone());
    child.launch_depth = Some(1);
    child
}

#[test]
fn the_stop_waits_for_every_agent_it_would_close() {
    let (_dir, store) = fixture();
    let (parent, stop) = rested();
    let child = kept_child(&parent, "child");
    let tree =
        |agents: &[AgentState]| decide(&store, agents, &parent, &stop, ts(DUE)).expect("decide");
    assert_eq!(
        tree(&[parent.clone(), child.clone()]),
        Verdict::Stop { idle_secs: 180 },
        "a kept child at rest with nothing owed closes with its parent"
    );
    // The child needs no rest of its own: the grace is the root's.
    let mut fresh = child.clone();
    fresh.turn_ended_at = Some(ts(DUE));
    assert_eq!(
        tree(&[parent.clone(), fresh]),
        Verdict::Stop { idle_secs: 180 }
    );

    let mut working = child.clone();
    working.status = AgentStatus::Running;
    assert_eq!(
        tree(&[parent.clone(), working.clone()]),
        Verdict::Hold(Hold::Tree)
    );
    let mut ended = working.clone();
    ended.ended_at = Some(ts(1_100));
    assert_eq!(
        tree(&[parent.clone(), ended]),
        Verdict::Stop { idle_secs: 180 },
        "an ended child is not closed, so it holds nothing"
    );
    let mut grandchild = kept_child(&child, "grandchild");
    grandchild.status = AgentStatus::Running;
    assert_eq!(
        tree(&[parent.clone(), child.clone(), grandchild]),
        Verdict::Hold(Hold::Tree)
    );
    let mut unrelated = working;
    unrelated.parent_agent_id = Some("someone-else".into());
    assert_eq!(
        tree(&[parent.clone(), unrelated]),
        Verdict::Stop { idle_secs: 180 }
    );

    // Work queued for a kept, joined child holds the parent until it lands.
    let message = MessageRecord::new(
        store.paths().workspace_id.clone(),
        &child,
        "one more thing".to_owned(),
        DeliveryGate::Done,
    );
    store
        .queue_message(&message, "idle-stop-test")
        .expect("queue");
    assert_eq!(
        tree(&[parent.clone(), child.clone()]),
        Verdict::Hold(Hold::Tree)
    );
    assert!(
        store
            .cancel_message(&message.message_id, "idle-stop-test", "test")
            .expect("cancel")
    );
    assert_eq!(
        tree(&[parent.clone(), child]),
        Verdict::Stop { idle_secs: 180 }
    );
}

#[test]
fn an_undelivered_message_holds_until_it_is_terminal() {
    for status in [
        MessageStatus::Queued,
        MessageStatus::Sent,
        MessageStatus::Delivered,
    ] {
        let (_dir, store) = fixture();
        let (agent, stop) = rested();
        let mut message = MessageRecord::new(
            store.paths().workspace_id.clone(),
            &agent,
            "rebase first".to_owned(),
            DeliveryGate::Done,
        );
        // A scheduled message is owed as much as a ready one.
        message.not_before = Some(ts(9_000));
        message.status = status;
        store
            .queue_message(&message, "idle-stop-test")
            .expect("queue");
        let expected = if status.is_terminal() {
            Verdict::Stop { idle_secs: 180 }
        } else {
            Verdict::Hold(Hold::Message)
        };
        assert_eq!(verdict(&store, &agent, &stop, DUE), expected, "{status:?}");

        let (mut other, _) = rested();
        other.agent_id = "session-2".into();
        other.name = Some("other".to_owned());
        assert_eq!(
            verdict(&store, &other, &stop, DUE),
            Verdict::Stop { idle_secs: 180 },
            "another card's message holds nothing"
        );
    }
}

/// Register `id` in the store's log, as the owed-wake reads find it.
fn register(store: &Store, id: &str, parent: Option<&AgentState>) {
    observe(store, id, parent, LifecycleSignal::Registered);
}

fn observe(store: &Store, id: &str, parent: Option<&AgentState>, signal: LifecycleSignal) {
    let mut observation = AgentLifecycleObservation::new(Some(id.into()), signal);
    observation.agent_name = Some(id.to_owned());
    if let Some(parent) = parent {
        observation.launch.parent_agent_id = Some(parent.agent_id.clone());
        observation.launch.parent_agent_kind = Some(parent.kind.clone());
        observation.launch.launch_depth = Some(1);
    }
    store
        .append_event(&crate::store::event::EventEnvelope::agent_lifecycle(
            store.paths().workspace_id.clone(),
            "idle-stop-test",
            "claude",
            "test",
            &observation,
        ))
        .expect("register");
}

#[test]
fn a_launched_child_holds_until_its_answer_is_collected() {
    for (status, joined, expected) in [
        (
            RunStatus::Running,
            false,
            Verdict::Hold(Hold::Owed(OwedWake::Subagents)),
        ),
        (
            RunStatus::Completed,
            false,
            Verdict::Hold(Hold::Owed(OwedWake::Subagents)),
        ),
        (RunStatus::Completed, true, Verdict::Stop { idle_secs: 180 }),
    ] {
        let (dir, store) = fixture();
        let (agent, stop) = rested();
        register(&store, "session-1", None);
        register(&store, "child", Some(&agent));
        let mut run = RunRecord::new(
            store.paths().workspace_id.clone(),
            agent.kind.clone(),
            PermissionMode::Auto,
            "work".to_owned(),
            dir.path().to_owned(),
        );
        run.agent_id = Some("child".into());
        run.status = status;
        run.joined_at = joined.then_some(ts(1_100));
        super::super::run::create(store.paths(), &run).expect("run");
        assert_eq!(
            verdict(&store, &agent, &stop, DUE),
            expected,
            "{status:?} joined={joined}"
        );
    }
}

#[test]
fn a_detached_child_holds_the_tree_while_it_works_and_owes_nothing_after() {
    for (run_status, child_status, expected) in [
        (
            RunStatus::Running,
            AgentStatus::Running,
            Verdict::Hold(Hold::Tree),
        ),
        (
            RunStatus::Completed,
            AgentStatus::Idle,
            Verdict::Stop { idle_secs: 180 },
        ),
    ] {
        let (dir, store) = fixture();
        let (agent, stop) = rested();
        register(&store, "session-1", None);
        register(&store, "child", Some(&agent));
        let mut run = RunRecord::new(
            store.paths().workspace_id.clone(),
            agent.kind.clone(),
            PermissionMode::Auto,
            "work".to_owned(),
            dir.path().to_owned(),
        );
        run.agent_id = Some("child".into());
        run.report_to = ReportTo::Nobody;
        run.status = run_status;
        super::super::run::create(store.paths(), &run).expect("run");
        let mut child = kept_child(&agent, "child");
        child.status = child_status;
        assert_eq!(
            decide(&store, &[agent.clone(), child], &agent, &stop, ts(DUE)).expect("decide"),
            expected,
            "{run_status:?}: a detached answer is owed to nobody, a working child is still closed"
        );
    }
}

#[test]
fn a_harness_wake_in_flight_holds_until_it_lands() {
    use crate::store::message::{HarnessNotice, MessageSender};
    for notice in [
        HarnessNotice::Wait,
        HarnessNotice::Signal,
        HarnessNotice::SubagentReport,
        HarnessNotice::TeamReport,
    ] {
        for status in [MessageStatus::Queued, MessageStatus::Delivered] {
            let (_dir, store) = fixture();
            let (agent, stop) = rested();
            register(&store, "session-1", None);
            let mut message = MessageRecord::new(
                store.paths().workspace_id.clone(),
                &agent,
                "wake".to_owned(),
                DeliveryGate::Done,
            )
            .with_sender(MessageSender::Harness {
                notice: notice.clone(),
            });
            message.status = status;
            store
                .queue_message(&message, "idle-stop-test")
                .expect("queue");
            let expected = if status.is_terminal() {
                Verdict::Stop { idle_secs: 180 }
            } else {
                Verdict::Hold(Hold::Owed(OwedWake::WakeInFlight))
            };
            assert_eq!(
                verdict(&store, &agent, &stop, DUE),
                expected,
                "{notice:?} {status:?}"
            );
        }
    }
}

#[test]
fn an_open_run_holds_so_the_stop_never_cancels() {
    for peer in [false, true] {
        for status in [RunStatus::Running, RunStatus::Completed] {
            let (dir, store) = fixture();
            let (mut agent, stop) = rested();
            agent.launch_id = Some("launch-1".into());
            let mut run = RunRecord::new(
                store.paths().workspace_id.clone(),
                agent.kind.clone(),
                PermissionMode::Auto,
                "work".to_owned(),
                dir.path().to_owned(),
            );
            run.agent_id = Some(agent.agent_id.clone());
            run.peer = peer.then(|| PeerRun {
                launch_id: "launch-1".into(),
                opened_by: Vec::new(),
            });
            run.status = status;
            super::super::run::create(store.paths(), &run).expect("run");
            let expected = if status.is_terminal() {
                Verdict::Stop { idle_secs: 180 }
            } else {
                Verdict::Hold(Hold::OpenRun)
            };
            assert_eq!(
                verdict(&store, &agent, &stop, DUE),
                expected,
                "peer={peer} {status:?}"
            );
        }
    }
}

#[test]
fn a_team_seat_waits_for_its_board_to_read_done() {
    let (dir, store) = fixture();
    let (mut agent, stop) = rested();
    agent.team = Some("forge".to_owned());
    assert_eq!(
        verdict(&store, &agent, &stop, DUE),
        Verdict::Hold(Hold::Board),
        "a seat with no checkout has no board"
    );
    agent.worktree_path = Some(dir.path().to_string_lossy().into_owned());
    assert_eq!(
        verdict(&store, &agent, &stop, DUE),
        Verdict::Hold(Hold::Board),
        "no board file"
    );
    std::fs::write(dir.path().join("blackboard.md"), "Stage: Review (@judge)\n").unwrap();
    assert_eq!(
        verdict(&store, &agent, &stop, DUE),
        Verdict::Hold(Hold::Board)
    );
    std::fs::write(dir.path().join("blackboard.md"), "Stage: Done\n").unwrap();
    assert_eq!(
        verdict(&store, &agent, &stop, DUE),
        Verdict::Stop { idle_secs: 180 }
    );
}

#[test]
fn requests_project_onto_their_sessions_and_stale_ones_clear() {
    let (_dir, store) = fixture();
    let (agent, stop) = rested();
    let (mut other, _) = rested();
    other.agent_id = "session-2".into();
    other.idle_stop = Some(PendingIdleStop {
        stop: stop.clone(),
        due_at: None,
    });
    let (mut busy, _) = rested();
    busy.agent_id = "session-3".into();
    busy.status = AgentStatus::Running;
    for session in ["session-1", "session-3"] {
        crate::store::idle_stop::arm(
            store.paths(),
            IdleStopRequest {
                kind: agent.kind.clone(),
                agent_id: session.into(),
                stop: stop.clone(),
            },
        )
        .expect("arm");
    }
    let mut snapshot = SidebarSnapshot::build_with_agents(
        store.paths().workspace_id.clone(),
        vec![agent, other, busy],
        ts(DUE),
    );
    project_requests(&mut snapshot, store.paths());
    let pending = |id: &str| {
        find_agent(
            &snapshot.agents,
            &AgentKind::new_unchecked("claude"),
            &id.into(),
        )
        .expect("agent")
        .idle_stop
        .clone()
    };
    let running = pending("session-1").expect("resting agent's request");
    assert_eq!(
        running,
        PendingIdleStop {
            stop: stop.clone(),
            due_at: Some(ts(DUE)),
        }
    );
    assert_eq!(pending("session-2"), None);
    let held = pending("session-3").expect("busy agent's request");
    assert_eq!(held.due_at, None, "a busy agent's clock is not running");

    assert_eq!(held.label(ts(DUE)), "after 3m idle");
    assert_eq!(running.label(ts(DUE - 61)), "after 3m idle, in 2m");
    assert_eq!(running.label(ts(DUE)), "after 3m idle, due");
}

#[test]
fn producer_spawns_for_a_due_request_paces_and_leaves_the_record_alone() {
    let (_dir, store) = fixture();
    let (paths, runtime) = (store.paths(), store.runtime_paths());
    let (agent, stop) = rested();
    let request = IdleStopRequest {
        kind: agent.kind.clone(),
        agent_id: agent.agent_id.clone(),
        stop,
    };
    crate::store::idle_stop::arm(paths, request.clone()).expect("arm");
    let snapshot_at = |agent: &AgentState, now: i64, pane: bool| {
        let mut snapshot = SidebarSnapshot::build_with_agents(
            paths.workspace_id.clone(),
            vec![agent.clone()],
            ts(now),
        );
        if pane {
            snapshot.agent_panes.push(PaneAgent {
                root_lane: false,
                kind: agent.kind.clone(),
                kind_ordinal: None,
                name: None,
                name_explicit: false,
                profile: None,
                role: None,
                channel: None,
                agent_id: Some(agent.agent_id.clone()),
                pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
                pane_pid: None,
                worktree_path: None,
                worktree_branch: None,
            });
        }
        snapshot
    };
    let spawned = |snapshot: &SidebarSnapshot| {
        let mut requests = Vec::new();
        stop_idle_agents_with(snapshot, paths, runtime, |request| {
            requests.push(request.clone());
            true
        });
        requests
    };

    assert!(spawned(&snapshot_at(&agent, DUE - 1, true)).is_empty());
    assert!(spawned(&snapshot_at(&agent, DUE, false)).is_empty());
    let mut running = agent.clone();
    running.status = AgentStatus::Running;
    assert!(spawned(&snapshot_at(&running, DUE, true)).is_empty());

    assert_eq!(
        spawned(&snapshot_at(&agent, DUE, true)),
        [IdleStopHelperRequest {
            workspace_id: paths.workspace_id.clone(),
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
            label: "@claude".to_owned(),
        }]
    );
    // A declining helper leaves the request armed; the producer asks again
    // after the throttle with no new turn.
    assert!(spawned(&snapshot_at(&agent, DUE + 29, true)).is_empty());
    assert_eq!(spawned(&snapshot_at(&agent, DUE + 30, true)).len(), 1);
    assert_eq!(crate::store::idle_stop::read(paths), [request]);
    assert!(read_fire_record(&fire_record_path(runtime, &agent.kind, &agent.agent_id)).is_some());
}

#[test]
fn pings_neither_defer_a_stop_nor_let_keep_warm_hold_against_it() {
    let (_dir, store) = fixture();
    let ping = "Type: CACHE_KEEPALIVE\nFrom: @rimz\nContent:\nCache keepalive, no action needed.";
    let append = |signal: LifecycleSignal, prompt: Option<&str>| {
        let mut observation = AgentLifecycleObservation::new(Some("session-1".into()), signal);
        observation.prompt = crate::agents::SanitizedPrompt::new(prompt);
        store
            .append_agent_lifecycle(crate::store::writer::AgentLifecycleIntent {
                session_name: "idle-stop-test",
                agent_kind: AgentKind::new_unchecked("claude"),
                event_name: "test",
                observation: &observation,
                spawned_subagents: &[],
            })
            .expect("append");
    };
    let end = || LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background: false,
        turn_id: None,
    };
    append(LifecycleSignal::TurnStarted { turn_id: None }, Some("do X"));
    append(end(), None);
    let rested_at = store.snapshot().unwrap().agents[0].turn_ended_at.unwrap();
    append(LifecycleSignal::TurnStarted { turn_id: None }, Some(ping));
    append(end(), None);
    let mut agent = store.snapshot().unwrap().agents[0].clone();
    assert!(agent.pinged_at > Some(rested_at));
    let stop = IdleStop {
        after_secs: 180,
        requested_at: rested_at,
        requested_by: None,
    };
    let due = rested_at
        .checked_add(jiff::SignedDuration::from_secs(180))
        .unwrap();
    assert_eq!(
        decide_alone(&store, &agent, &stop, due),
        Verdict::Stop { idle_secs: 180 },
        "the stop runs from the real rest, not the ping"
    );

    agent.idle_stop = Some(PendingIdleStop { stop, due_at: None });
    assert_eq!(
        crate::harness::cache_keepalive::holds(
            &agent,
            Some(std::time::Duration::from_secs(7200)),
            &crate::config::HarnessConfig::default(),
            rested_at,
        ),
        None,
        "a requested stop ends the hold"
    );
}
