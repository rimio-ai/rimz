use super::*;

#[test]
fn stall_classes_cover_every_delivery_verdict() {
    let now = Timestamp::now();
    let mut cases = vec![
        (
            DeliveryVerdict::NoPane {
                pinned_pane_id: None,
            },
            Some(StallClass::NotWorking),
        ),
        (
            DeliveryVerdict::ProviderStarting,
            Some(StallClass::NotWorking),
        ),
        (
            DeliveryVerdict::ResumeUnrecovered,
            Some(StallClass::NotWorking),
        ),
        (DeliveryVerdict::AskWaiting, Some(StallClass::NotWorking)),
        (DeliveryVerdict::ReceiverGone, Some(StallClass::NotWorking)),
        (DeliveryVerdict::ReceiverEnded, Some(StallClass::NotWorking)),
        (DeliveryVerdict::Compacting, Some(StallClass::Busy)),
        (DeliveryVerdict::Expired { expires_at: now }, None),
        (
            DeliveryVerdict::Scheduled {
                not_before: Some(now),
            },
            None,
        ),
        (
            DeliveryVerdict::WaitingOnAfter {
                address: "@peer".into(),
                agent_present: true,
            },
            None,
        ),
        (
            DeliveryVerdict::WaitingOnWhen {
                address: "@peer".into(),
                expected: AgentStatus::Idle,
                current: Some(AgentStatus::Running),
                dwell_secs: 60,
                dwell_so_far_secs: None,
            },
            None,
        ),
        (DeliveryVerdict::BehindFifo { blocker: None }, None),
        (
            DeliveryVerdict::BehindFifo {
                blocker: Some(MessageId::new()),
            },
            None,
        ),
        (DeliveryVerdict::Ready, None),
    ];
    for status in [
        None,
        Some(AgentStatus::Running),
        Some(AgentStatus::Idle),
        Some(AgentStatus::Success),
        Some(AgentStatus::Failed),
        Some(AgentStatus::Waiting),
        Some(AgentStatus::Sleeping),
        Some(AgentStatus::Paused),
    ] {
        cases.push((
            DeliveryVerdict::GateClosed {
                gate: DeliveryGate::Done,
                status,
            },
            Some(if status == Some(AgentStatus::Running) {
                StallClass::Busy
            } else {
                StallClass::NotWorking
            }),
        ));
    }
    for (verdict, expected) in cases {
        assert_eq!(verdict.stall_class(), expected, "{verdict:?}");
        assert!(!verdict.reason().is_empty(), "{verdict:?}");
        if let Some(class) = expected {
            assert!(class.delay() > Duration::ZERO);
        }
    }
}

#[test]
fn explain_reports_expiry_before_other_blockers() {
    let now = Timestamp::from_second(10_000).unwrap();
    let receiver = agent("session", AgentStatus::Idle);
    let live = snapshot(receiver.clone(), true, now);
    let mut command = message(&receiver, 1, "/compact")
        .with_body(MessageBody::Command)
        .with_automated(true);
    command.enqueued_at = now - crate::store::message::DEFAULT_COMMAND_VALIDITY;
    let check = explain(&command, std::slice::from_ref(&command), &live, now);
    assert_eq!(
        check.verdict(),
        DeliveryVerdict::Expired { expires_at: now }
    );
    assert!(!check.passes());
    assert!(
        check.schedule.ready
            && check.fifo.head
            && check.agent.present
            && check.gate.open
            && check.pane.present
    );
    assert_eq!(
        serde_json::to_value(&check).unwrap()["expiry"],
        serde_json::json!({"expired": true, "expires_at": now})
    );
    command.not_before = Some(now + Duration::from_secs(60));
    assert_eq!(
        explain(&command, std::slice::from_ref(&command), &live, now).verdict(),
        DeliveryVerdict::Expired { expires_at: now }
    );
    let prompt = message(&receiver, 2, "next");
    let json =
        serde_json::to_value(explain(&prompt, std::slice::from_ref(&prompt), &live, now)).unwrap();
    assert_eq!(json["expiry"], serde_json::json!({"expired": false}));
}

#[test]
fn prompt_behind_expired_command_is_fifo_head() {
    let now = Timestamp::from_second(10_000).unwrap();
    let receiver = agent("session", AgentStatus::Idle);
    let live = snapshot(receiver.clone(), true, now);
    let mut command = message(&receiver, 1, "/compact")
        .with_body(MessageBody::Command)
        .with_automated(true);
    command.enqueued_at = now - crate::store::message::DEFAULT_COMMAND_VALIDITY;
    let prompt = message(&receiver, 2, "next");
    let check = explain(&prompt, &[command, prompt.clone()], &live, now);
    assert!(check.fifo.head);
    assert!(check.passes());
}

#[test]
fn expired_command_is_refused_under_every_delivery_policy() {
    let now = Timestamp::from_second(10_000).unwrap();
    let receiver = agent("session", AgentStatus::Idle);
    let live = snapshot(receiver.clone(), true, now);
    let mut command = message(&receiver, 1, "/compact")
        .with_body(MessageBody::Command)
        .with_automated(true);
    command.enqueued_at = now - crate::store::message::DEFAULT_COMMAND_VALIDITY;
    for policy in [
        DeliveryPolicy::Boundary,
        DeliveryPolicy::Steer { force: false },
        DeliveryPolicy::Steer { force: true },
        DeliveryPolicy::Interrupt { force: false },
        DeliveryPolicy::Interrupt { force: true },
    ] {
        let candidacy = delivery_candidate(
            std::slice::from_ref(&command),
            &live,
            &command.message_id,
            policy,
            now,
        );
        assert!(
            matches!(
                candidacy,
                Candidacy::Refused(DeliveryVerdict::Expired { expires_at }) if expires_at == now
            ),
            "{policy:?} must refuse expiry"
        );
    }
}

#[test]
fn ended_receiver_refuses_before_compaction_and_gate_checks() {
    let now = Timestamp::from_second(10_000).unwrap();
    let mut receiver = agent("session", AgentStatus::Idle);
    receiver.ended_at = Some(now);
    let candidate = message(&receiver, 1, "follow up");
    let live = snapshot(receiver, true, now);
    let mut check = explain(&candidate, std::slice::from_ref(&candidate), &live, now);
    assert_eq!(check.verdict(), DeliveryVerdict::ReceiverEnded);
    check.gate.compacting = true;
    check.gate.open = false;
    assert_eq!(check.verdict(), DeliveryVerdict::ReceiverEnded);
    check.agent.present = false;
    assert_eq!(check.verdict(), DeliveryVerdict::ReceiverGone);
    assert!(matches!(
        delivery_candidate(
            std::slice::from_ref(&candidate),
            &live,
            &candidate.message_id,
            DeliveryPolicy::Boundary,
            now
        ),
        Candidacy::Refused(DeliveryVerdict::ReceiverEnded)
    ));
}

#[test]
fn resumed_provider_refuses_delivery_before_resume_recovery() {
    let now = Timestamp::from_second(10_000).unwrap();
    let mut receiver = agent("session", AgentStatus::Idle);
    receiver.kind = crate::ids::AgentKind::new_unchecked("claude");
    receiver.resumed_at = Some(now);
    let candidate = message(&receiver, 1, "hello");
    let live = snapshot(receiver, true, now);
    let mut check = explain(&candidate, std::slice::from_ref(&candidate), &live, now);
    assert_eq!(check.verdict(), DeliveryVerdict::ProviderStarting);
    assert!(matches!(
        delivery_candidate(
            std::slice::from_ref(&candidate),
            &live,
            &candidate.message_id,
            DeliveryPolicy::Boundary,
            now
        ),
        Candidacy::Refused(DeliveryVerdict::ProviderStarting)
    ));
    for policy in [
        DeliveryPolicy::Steer { force: false },
        DeliveryPolicy::Interrupt { force: false },
    ] {
        assert!(matches!(
            delivery_candidate(
                std::slice::from_ref(&candidate),
                &live,
                &candidate.message_id,
                policy,
                now
            ),
            Candidacy::Ready(_)
        ));
    }
    check.gate.resume_recovered = Some(false);
    assert_eq!(check.verdict(), DeliveryVerdict::ProviderStarting);
    check.gate.open = false;
    assert!(matches!(
        check.verdict(),
        DeliveryVerdict::GateClosed { .. }
    ));
    assert_eq!(
        explain(
            &candidate,
            std::slice::from_ref(&candidate),
            &live,
            now + MessageBody::Prompt.delivery_window()
        )
        .verdict(),
        DeliveryVerdict::Ready
    );
}

use crate::agents::AgentState;
use crate::ids::WorkspaceId;
use crate::store::snapshot::PaneAgent;

#[test]
fn receiver_readiness_unifies_gate_waiting_and_force() {
    let now = Timestamp::now();
    let idle = agent("sess-idle", AgentStatus::Idle);
    assert!(receiver_readiness(&idle, DeliveryGate::Done, false, now).accepts_prompt());

    let running = agent("sess-running", AgentStatus::Running);
    assert!(!receiver_readiness(&running, DeliveryGate::Done, false, now).accepts_prompt());

    let mut waiting = agent("sess-waiting", AgentStatus::Waiting);
    waiting.waiting_since = Some(waiting.last_activity);
    assert!(!receiver_readiness(&waiting, DeliveryGate::Done, false, now).accepts_prompt());
    assert!(receiver_readiness(&waiting, DeliveryGate::Done, true, now).accepts_prompt());
}

#[test]
fn sweep_guard_is_single_flight() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/message-sweep")),
        dir.path(),
    )
    .expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");

    let first = try_start_sweep(&runtime)
        .expect("first guard")
        .expect("first guard starts");
    assert!(
        try_start_sweep(&runtime).expect("second guard").is_none(),
        "a running sweep keeps later helpers from entering delivery"
    );
    drop(first);
    assert!(
        try_start_sweep(&runtime).expect("third guard").is_some(),
        "the sweep lock releases when the helper exits"
    );
}

#[test]
fn explain_reports_actionable_delivery_blockers() {
    let now = Timestamp::now();
    let receiver = agent("sess-receiver", AgentStatus::Idle);
    let live = snapshot(receiver.clone(), true, now);

    let older = message(&receiver, 1, "first");
    let newer = message(&receiver, 2, "second");
    let check = explain(&newer, &[newer.clone(), older.clone()], &live, now);
    assert!(!check.fifo.head);
    assert_eq!(check.fifo.blocker, Some(older.message_id));

    let mut running = receiver.clone();
    running.status = AgentStatus::Running;
    let running = snapshot(running, true, now);
    let candidate = message(&receiver, 1, "wait");
    let check = explain(&candidate, std::slice::from_ref(&candidate), &running, now);
    assert_eq!(check.gate.status, Some(AgentStatus::Running));
    assert!(!check.gate.open);

    let mut parked = receiver.clone();
    parked.status = AgentStatus::Running;
    parked.phase = crate::agents::TurnPhase::Parked;
    let parked = snapshot(parked, true, now);
    let check = explain(&candidate, std::slice::from_ref(&candidate), &parked, now);
    assert_eq!(check.gate.status, Some(AgentStatus::Success));
    assert!(check.gate.open);

    let mut waiting = receiver.clone();
    waiting.status = AgentStatus::Waiting;
    waiting.waiting_since = Some(waiting.last_activity);
    let waiting = snapshot(waiting, true, now);
    let check = explain(&candidate, std::slice::from_ref(&candidate), &waiting, now);
    assert!(check.ask.waiting);

    let no_pane = snapshot(receiver.clone(), false, now);
    let check = explain(&candidate, std::slice::from_ref(&candidate), &no_pane, now);
    assert!(!check.pane.present);

    let not_before = now + jiff::SignedDuration::from_secs(60);
    let scheduled = message(&receiver, 1, "later").with_not_before(Some(not_before));
    let check = explain(&scheduled, std::slice::from_ref(&scheduled), &live, now);
    assert!(!check.schedule.ready);
    assert_eq!(check.schedule.not_before, Some(not_before));

    let mut upstream = agent("sess-planner", AgentStatus::Running);
    upstream.role = Some("planner".to_owned());
    let mut dependency = live;
    dependency.agents.push(upstream.clone());
    let after = message(&receiver, 1, "wait for plan").with_after(vec![AfterCondition {
        kind: upstream.kind.clone(),
        agent_id: upstream.agent_id.clone(),
        agent_name: upstream.name.clone(),
        address: "@planner".to_owned(),
        met_at: None,
    }]);
    let check = explain(&after, std::slice::from_ref(&after), &dependency, now);
    assert_eq!(check.after[0].address, "@planner");
    assert!(!check.after[0].met);
    assert!(check.after[0].agent_present);
    assert_eq!(check.after[0].status, Some(AgentStatus::Running));
    assert_eq!(
        check.verdict(),
        DeliveryVerdict::WaitingOnAfter {
            address: "@planner".to_owned(),
            agent_present: true,
        }
    );

    dependency.agents[1].status = AgentStatus::Idle;
    let check = explain(&after, std::slice::from_ref(&after), &dependency, now);
    assert!(check.after[0].met);
}

#[test]
fn explain_names_sent_prompt_and_claimed_fifo_blockers() {
    let now = Timestamp::from_second(10_000).unwrap();
    let receiver = agent("sess-receiver", AgentStatus::Idle);
    let live = snapshot(receiver.clone(), true, now);
    let candidate = message(&receiver, 2, "next");
    let mut older = message(&receiver, 1, "first");
    for status in [MessageStatus::Sent, MessageStatus::Claimed] {
        older.status = status;
        older.last_attempt_at = Some(now);
        assert_eq!(
            explain(&candidate, &[candidate.clone(), older.clone()], &live, now).verdict(),
            DeliveryVerdict::BehindFifo {
                blocker: Some(older.message_id.clone())
            }
        );
    }
    older.status = MessageStatus::Sent;
    older.body = MessageBody::Command;
    assert_eq!(
        explain(&candidate, &[older, candidate.clone()], &live, now).verdict(),
        DeliveryVerdict::Ready
    );
}

#[test]
fn delivery_check_reports_first_blocker_and_passes_only_when_ready() {
    let mut check = ready_check();
    assert!(check.passes());
    assert_eq!(check.verdict(), DeliveryVerdict::Ready);

    let not_before = Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_secs(60);
    check.schedule.ready = false;
    check.schedule.not_before = Some(not_before);
    check.after.push(AfterConditionCheck {
        address: "@planner".to_owned(),
        met: false,
        met_at: None,
        agent_present: false,
        status: None,
    });
    assert_eq!(
        check.verdict(),
        DeliveryVerdict::Scheduled {
            not_before: Some(not_before)
        }
    );

    check.schedule.ready = true;
    assert_eq!(
        check.verdict(),
        DeliveryVerdict::WaitingOnAfter {
            address: "@planner".to_owned(),
            agent_present: false,
        }
    );
    check.after[0].met = true;
    check.fifo.head = false;
    check.fifo.blocker = Some(message_id(7));
    assert_eq!(
        check.verdict(),
        DeliveryVerdict::BehindFifo {
            blocker: Some(message_id(7))
        }
    );
    check.fifo.head = true;
    check.agent.present = false;
    assert_eq!(check.verdict(), DeliveryVerdict::ReceiverGone);
    check.agent.present = true;
    check.gate.open = false;
    assert_eq!(
        check.verdict(),
        DeliveryVerdict::GateClosed {
            gate: DeliveryGate::Done,
            status: Some(AgentStatus::Idle),
        }
    );
    check.gate.open = true;
    check.gate.resume_recovered = Some(false);
    assert_eq!(check.verdict(), DeliveryVerdict::ResumeUnrecovered);
    check.gate.resume_recovered = None;
    check.ask.waiting = true;
    assert_eq!(check.verdict(), DeliveryVerdict::AskWaiting);
    check.ask.waiting = false;
    check.pane.present = false;
    check.pane.pinned_pane_id = Some(PaneId::from_parts(MuxName::Zellij, "terminal_9"));
    assert_eq!(
        check.verdict(),
        DeliveryVerdict::NoPane {
            pinned_pane_id: Some(PaneId::from_parts(MuxName::Zellij, "terminal_9"))
        }
    );
    assert!(!check.passes());
}

#[test]
fn candidate_uses_shared_evaluation_but_requires_durable_condition_stamp() {
    let now = Timestamp::from_second(10_000).unwrap();
    let receiver = agent("sess-receiver", AgentStatus::Idle);
    let upstream = agent("sess-upstream", AgentStatus::Idle);
    let mut live = snapshot(receiver.clone(), true, now);
    live.agents.push(upstream.clone());
    let mut candidate = message(&receiver, 1, "after upstream").with_after(vec![AfterCondition {
        kind: upstream.kind.clone(),
        agent_id: upstream.agent_id.clone(),
        agent_name: upstream.name.clone(),
        address: "@upstream".to_owned(),
        met_at: None,
    }]);
    let pending = vec![candidate.clone()];

    assert_eq!(
        explain(&candidate, &pending, &live, now).verdict(),
        DeliveryVerdict::Ready
    );
    assert!(
        matches!(
            delivery_candidate(
                &pending,
                &live,
                &candidate.message_id,
                DeliveryPolicy::Boundary,
                now,
            ),
            Candidacy::Refused(DeliveryVerdict::Ready)
        ),
        "dynamic truth explains readiness but cannot cross claim boundary"
    );

    candidate.after[0].met_at = Some(now);
    let pending = vec![candidate.clone()];
    assert!(matches!(
        delivery_candidate(
            &pending,
            &live,
            &candidate.message_id,
            DeliveryPolicy::Boundary,
            now,
        ),
        Candidacy::Ready(_)
    ));
}

#[test]
fn when_evaluation_handles_exact_boundary_clock_skew_and_expiry() {
    let now = Timestamp::from_second(10_000).unwrap();
    let mut watched = agent("sess-watched", AgentStatus::Running);
    watched.turn_started_at = Some(Timestamp::from_second(9_940).unwrap());
    let condition = WhenCondition {
        kind: watched.kind.clone(),
        agent_id: watched.agent_id.clone(),
        agent_name: watched.name.clone(),
        address: "@watched".to_owned(),
        status: AgentStatus::Running,
        dwell_secs: 60,
        met_at: None,
    };
    let live = snapshot(watched.clone(), false, now);
    let exact = evaluate_when_condition(&condition, &live, now, Duration::from_secs(30));
    assert!(exact.check.met);
    assert!(exact.stamp_needed);

    watched.turn_started_at = Some(Timestamp::from_second(10_010).unwrap());
    let skewed = snapshot(watched, false, now);
    let skewed = evaluate_when_condition(&condition, &skewed, now, Duration::from_secs(30));
    assert!(!skewed.check.met);
    assert_eq!(skewed.check.dwell_so_far_secs, Some(0));
    assert_eq!(
        skewed.check.trip_at,
        Some(Timestamp::from_second(10_070).unwrap())
    );

    let gone = snapshot(agent("other", AgentStatus::Idle), false, now);
    let gone = evaluate_when_condition(&condition, &gone, now, Duration::from_secs(30));
    assert_eq!(gone.archive_reason, Some(condition.expiry_reason()));
}

fn ready_check() -> DeliveryCheck {
    DeliveryCheck {
        expiry: ExpiryCheck {
            expired: false,
            expires_at: None,
        },
        schedule: ScheduleCheck {
            ready: true,
            not_before: None,
            retry_after: None,
        },
        after: Vec::new(),
        when: Vec::new(),
        fifo: FifoCheck {
            head: true,
            blocker: None,
        },
        agent: AgentCheck {
            present: true,
            ended: false,
        },
        gate: GateCheck {
            provider_start_pending: false,
            gate: DeliveryGate::Done,
            status: Some(AgentStatus::Idle),
            compacting: false,
            open: true,
            resume_recovered: None,
        },
        ask: AskCheck {
            waiting: false,
            force: false,
        },
        pane: PaneCheck {
            present: true,
            pane_id: Some(PaneId::from_parts(MuxName::Zellij, "terminal_3")),
            pinned_pane_id: None,
        },
    }
}

fn snapshot(agent: AgentState, with_pane: bool, now: Timestamp) -> SidebarSnapshot {
    let mut snapshot = SidebarSnapshot::build_with_agents(workspace_id(), vec![agent], now);
    if with_pane {
        let agent = &snapshot.agents[0];
        snapshot.agent_panes = vec![PaneAgent {
            root_lane: false,
            kind: agent.kind.clone(),
            kind_ordinal: agent.kind_ordinal,
            name: agent.name.clone(),
            name_explicit: agent.name_explicit,
            profile: agent.profile.clone(),
            role: agent.role.clone(),
            channel: agent.channel.clone(),
            agent_id: Some(agent.agent_id.clone()),
            pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_3"),
            pane_pid: None,
            worktree_path: agent.worktree_path.clone(),
            worktree_branch: agent.worktree_branch.clone(),
        }];
    }
    snapshot
}

fn message(agent: &AgentState, id: u64, text: &str) -> MessageRecord {
    let mut message =
        MessageRecord::new(workspace_id(), agent, text.to_owned(), DeliveryGate::Done);
    message.message_id = message_id(id);
    message
}

fn message_id(value: u64) -> MessageId {
    MessageId::parse(&format!("msg_{value:016}")).unwrap()
}

fn workspace_id() -> WorkspaceId {
    WorkspaceId::parse("ws_000000000000000000000000").unwrap()
}

fn agent(id: &str, status: AgentStatus) -> AgentState {
    AgentState::stub("claude", id, status)
}
