use super::*;
use crate::agents::{PendingWait, PendingWaitTrigger, PermissionMode};
use crate::ids::MuxName;
use crate::store::idle_stop::IdleStopRequest;
use crate::store::message::{DeliveryGate, MessageRecord, MessageStatus};
use crate::store::run::{PeerRun, RunRecord, RunStatus};
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

fn verdict(store: &Store, agent: &AgentState, stop: &IdleStop, now: i64) -> Verdict {
    decide(store, agent, stop, ts(now)).expect("decide")
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
        decide(&store, &fractional, &three_minutes, ms(1_180_899)).expect("decide"),
        Verdict::Hold(Hold::Clock)
    );
    assert_eq!(
        decide(&store, &fractional, &three_minutes, ms(1_180_900)).expect("decide"),
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
    use crate::agents::{AgentLifecycleObservation, LifecycleSignal};
    let mut observation =
        AgentLifecycleObservation::new(Some(id.into()), LifecycleSignal::Registered);
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
