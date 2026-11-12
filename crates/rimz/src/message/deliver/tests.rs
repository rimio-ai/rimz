use super::*;

#[test]
fn typing_hold_replay_policy_uses_its_own_force() {
    let until = Timestamp::from_second(10_000).unwrap();
    let cases = [
        ("steer", false, DeliveryPolicy::Steer { force: false }),
        ("steer", true, DeliveryPolicy::Steer { force: true }),
        (
            "interrupt",
            false,
            DeliveryPolicy::Interrupt { force: false },
        ),
        ("interrupt", true, DeliveryPolicy::Interrupt { force: true }),
    ];
    let observed = cases.map(|(replay, force, _)| {
        let hold = serde_json::from_value(serde_json::json!({
            "until": until,
            "replay": replay,
            "force": force,
        }))
        .unwrap();
        DeliveryPolicy::from_typing_hold(hold)
    });
    assert_eq!(observed, cases.map(|(_, _, policy)| Some(policy)));
}

#[test]
fn stored_typing_hold_applies_to_every_policy_except_a_forced_interrupt_or_keepalive() {
    let now = Timestamp::from_second(10_000).unwrap();
    let receiver = agent("receiver", AgentStatus::Idle);
    let live = snapshot(receiver.clone(), true, now);
    let mut encoded = serde_json::to_value(message(&receiver, 1, "go")).unwrap();
    encoded["typing_hold"] = serde_json::json!({"until": now + Duration::from_secs(30)});
    let held: MessageRecord = serde_json::from_value(encoded).unwrap();
    let mut forced_keepalive = held.clone();
    forced_keepalive.forced_keepalive = true;
    let observed = [
        (&held, DeliveryPolicy::Boundary),
        (&held, DeliveryPolicy::Steer { force: false }),
        (&held, DeliveryPolicy::Steer { force: true }),
        (&held, DeliveryPolicy::Interrupt { force: false }),
        (&held, DeliveryPolicy::Interrupt { force: true }),
        (&forced_keepalive, DeliveryPolicy::Boundary),
    ]
    .map(|(record, policy)| {
        matches!(
            delivery_candidate(std::slice::from_ref(record), &live, &record.message_id, policy, now),
            Candidacy::Refused(verdict) if verdict.reason().contains("someone is using")
        )
    });
    assert_eq!(observed, [true, true, true, true, false, false]);
}

#[test]
fn forced_interrupt_ignores_a_stored_typing_hold_when_the_pane_is_gone() {
    let now = Timestamp::from_second(10_000).unwrap();
    let receiver = agent("receiver", AgentStatus::Idle);
    let live = snapshot(receiver.clone(), false, now);
    let mut held = message(&receiver, 1, "go");
    held.typing_hold = Some(TypingHold {
        until: now + Duration::from_secs(30),
        replay: None,
        force: false,
    });
    let verdict = match delivery_candidate(
        std::slice::from_ref(&held),
        &live,
        &held.message_id,
        DeliveryPolicy::Interrupt { force: true },
        now,
    ) {
        Candidacy::Refused(verdict) => Some(verdict),
        _ => None,
    };
    assert_eq!(verdict, Some(DeliveryVerdict::NoPane));
}

#[test]
fn explain_releases_the_stored_typing_hold_at_the_exact_deadline() {
    let now = Timestamp::from_second(10_000).unwrap();
    let receiver = agent("receiver", AgentStatus::Idle);
    let live = snapshot(receiver.clone(), true, now);
    let mut encoded = serde_json::to_value(message(&receiver, 1, "go")).unwrap();
    let observed = [-1, 0, 1].map(|offset| {
        encoded["typing_hold"] =
            serde_json::json!({"until": now + jiff::SignedDuration::from_secs(offset)});
        let held: MessageRecord = serde_json::from_value(encoded.clone()).unwrap();
        let check = explain(&held, std::slice::from_ref(&held), &live, now);
        let value = serde_json::to_value(&check).unwrap();
        value
            .get("typing_hold")
            .is_some_and(serde_json::Value::is_object)
    });
    assert_eq!(observed, [false, false, true]);
}

fn explain(
    message: &MessageRecord,
    pending: &[MessageRecord],
    snapshot: &SidebarSnapshot,
    now: Timestamp,
) -> DeliveryCheck {
    super::explain(
        message,
        pending,
        snapshot,
        now,
        &CardStatus::from_demotion(crate::agents::ParkDemotion::default()),
    )
}

fn delivery_candidate<'a>(
    pending: &[MessageRecord],
    snapshot: &'a SidebarSnapshot,
    message_id: &MessageId,
    policy: DeliveryPolicy,
    now: Timestamp,
) -> Candidacy<'a> {
    super::delivery_candidate(
        pending,
        snapshot,
        message_id,
        policy,
        now,
        &CardStatus::from_demotion(crate::agents::ParkDemotion::default()),
    )
}

fn receiver_readiness(
    agent: &AgentState,
    gate: DeliveryGate,
    force: bool,
    now: Timestamp,
) -> ReceiverReadiness {
    super::receiver_readiness(
        agent,
        gate,
        force,
        now,
        &snapshot(agent.clone(), false, now),
        &CardStatus::from_demotion(crate::agents::ParkDemotion::default()),
    )
}

fn spent_receiver(now: Timestamp) -> (AgentState, CardStatus) {
    let mut receiver = agent("spent", AgentStatus::Running);
    receiver.last_activity = now - Duration::from_secs(1);
    receiver.context = Some(crate::agents::AgentContext {
        turn_error: Some(crate::agents::AgentTurnError {
            ended_turn: false,
            class: crate::agents::TurnErrorClass::PausedRateLimit,
            at: now,
            label: None,
        }),
        ..crate::agents::AgentContext::new("claude", now)
    });
    let exhausted = [(receiver.kind.clone(), receiver.agent_id.clone())].into();
    let reader = CardStatus::from_demotion(crate::agents::ParkDemotion::new(
        &Default::default(),
        exhausted,
        now,
    ));
    (receiver, reader)
}

#[test]
fn explain_reports_a_spent_pause_as_failed_but_resume_as_paused() {
    let now = Timestamp::from_second(10_000).unwrap();
    let (receiver, reader) = spent_receiver(now);
    let live = snapshot(receiver.clone(), true, now);
    let observed = [DeliveryGate::Any, DeliveryGate::Done, DeliveryGate::Resume].map(|gate| {
        let mut candidate = message(&receiver, 1, "retry");
        candidate.gate = gate;
        let check = super::explain(
            &candidate,
            std::slice::from_ref(&candidate),
            &live,
            now,
            &reader,
        );
        (check.gate.status, check.gate.open)
    });
    assert_eq!(
        observed,
        [
            (Some(AgentStatus::Failed), true),
            (Some(AgentStatus::Failed), false),
            (Some(AgentStatus::Paused), true)
        ]
    );
}

#[test]
fn after_a_spent_pause_requires_any_and_an_empty_due_queue() {
    let now = Timestamp::from_second(10_000).unwrap();
    let (receiver, reader) = spent_receiver(now);
    let live = snapshot(receiver.clone(), true, now);
    let condition = AfterCondition {
        kind: receiver.kind.clone(),
        agent_id: receiver.agent_id.clone(),
        agent_name: receiver.name.clone(),
        address: "@spent".into(),
        met_at: None,
    };
    let due = message(&receiver, 1, "upstream work");
    let observed = [
        (DeliveryGate::Any, &[][..]),
        (DeliveryGate::Done, &[][..]),
        (DeliveryGate::Any, std::slice::from_ref(&due)),
    ]
    .map(|(gate, pending)| {
        let check = evaluate_after_condition(&condition, gate, pending, &live, now, &reader).check;
        (check.status, check.met)
    });
    assert_eq!(
        observed,
        [
            (Some(AgentStatus::Failed), true),
            (Some(AgentStatus::Failed), false),
            (Some(AgentStatus::Failed), false)
        ]
    );
}

#[test]
fn spent_pause_candidate_carries_failed_into_batch_claims() {
    let now = Timestamp::from_second(10_000).unwrap();
    let (receiver, reader) = spent_receiver(now);
    let live = snapshot(receiver.clone(), true, now);
    let mut candidate = message(&receiver, 1, "retry");
    candidate.gate = DeliveryGate::Any;
    let status = match super::delivery_candidate(
        std::slice::from_ref(&candidate),
        &live,
        &candidate.message_id,
        DeliveryPolicy::Boundary,
        now,
        &reader,
    ) {
        Candidacy::Ready(candidate) => Some(candidate.status),
        Candidacy::Refused(_) | Candidacy::Gone => None,
    };
    assert_eq!(status, Some(AgentStatus::Failed));
}

#[test]
fn delivery_without_a_provider_pause_builds_no_evidence() {
    let now = Timestamp::from_second(10_000).unwrap();
    let receiver = agent("session", AgentStatus::Idle);
    let live = snapshot(receiver.clone(), true, now);
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(
        crate::StatePaths::under(workspace_id(), dir.path()).unwrap(),
        RuntimePaths::under(workspace_id(), dir.path()).unwrap(),
    )
    .unwrap();
    let reader = CardStatus::new(&store);
    let candidate = message(&receiver, 1, "hello");
    let pending = std::slice::from_ref(&candidate);
    assert!(super::explain(&candidate, pending, &live, now, &reader).passes());
    assert!(matches!(
        super::delivery_candidate(
            pending,
            &live,
            &candidate.message_id,
            DeliveryPolicy::Boundary,
            now,
            &reader
        ),
        Candidacy::Ready(_)
    ));
    assert!(!reader.evidence_built());
}

#[test]
fn stall_classes_cover_every_delivery_verdict() {
    let now = Timestamp::now();
    let mut cases = vec![
        (DeliveryVerdict::NoPane, Some(StallClass::NotWorking)),
        (
            DeliveryVerdict::ProviderStarting,
            Some(StallClass::NotWorking),
        ),
        (
            DeliveryVerdict::ResumeUnrecovered,
            Some(StallClass::NotWorking),
        ),
        (DeliveryVerdict::AskWaiting, Some(StallClass::NotWorking)),
        (DeliveryVerdict::PaneHeld, Some(StallClass::NotWorking)),
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
    std::sync::Arc::make_mut(&mut dependency.agents).push(upstream.clone());
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

    std::sync::Arc::make_mut(&mut dependency.agents)[1].status = AgentStatus::Idle;
    let check = explain(&after, std::slice::from_ref(&after), &dependency, now);
    assert!(check.after[0].met);
}

#[test]
fn a_stored_pane_never_filters_the_agents_current_pane() {
    let now = Timestamp::from_second(10_000).unwrap();
    let receiver = agent("sess-receiver", AgentStatus::Idle);
    let live = snapshot(receiver.clone(), true, now);
    // A record queued before the rule keeps the pane it was written for on disk.
    let stale =
        message(&receiver, 1, "go").with_pane_id(PaneId::from_parts(MuxName::Zellij, "terminal_9"));
    let check = explain(&stale, std::slice::from_ref(&stale), &live, now);
    assert_eq!(check.verdict(), DeliveryVerdict::Ready);
    assert_eq!(
        check.pane.pane_id,
        Some(PaneId::from_parts(MuxName::Zellij, "terminal_3"))
    );
    let paneless = snapshot(receiver, false, now);
    let check = explain(&stale, std::slice::from_ref(&stale), &paneless, now);
    assert_eq!(check.verdict().reason(), "stuck: no live pane");
}

#[test]
fn the_no_pane_recognizer_also_claims_the_old_pinned_pane_text() {
    for blocker in [
        "stuck: no live pane",
        "stuck: pinned pane zellij:terminal_9 is not live",
    ] {
        assert!(is_no_pane_blocker(blocker), "{blocker}");
    }
    assert!(!is_no_pane_blocker("pane missing"));
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
    check.ask.turn_ended = true;
    assert_eq!(check.verdict(), DeliveryVerdict::PaneHeld);
    check.ask.turn_ended = false;
    check.ask.waiting = false;
    check.pane.present = false;
    assert_eq!(check.verdict(), DeliveryVerdict::NoPane);
    assert!(!check.passes());
}

#[test]
fn candidate_uses_shared_evaluation_but_requires_durable_condition_stamp() {
    let now = Timestamp::from_second(10_000).unwrap();
    let receiver = agent("sess-receiver", AgentStatus::Idle);
    let upstream = agent("sess-upstream", AgentStatus::Idle);
    let mut live = snapshot(receiver.clone(), true, now);
    std::sync::Arc::make_mut(&mut live.agents).push(upstream.clone());
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

#[test]
fn waiting_dwell_starts_when_permission_review_becomes_visible() {
    let at = Timestamp::from_second(1_000).unwrap();
    let mut watched = crate::testkit::agent_state("codex", "review", at);
    watched.status = AgentStatus::Waiting;
    watched.mode = Some(crate::agents::PermissionMode::Auto);
    watched.waiting_since = Some(at);
    watched.open_ask = Some(crate::agents::OpenAsk {
        extra_questions: 0,
        id: crate::ids::AskId::new(),
        kind: crate::agents::AskKind::Permission,
        detail: None,
        native_key: None,
        since: at,
    });
    let condition = WhenCondition {
        kind: watched.kind.clone(),
        agent_id: watched.agent_id.clone(),
        agent_name: None,
        address: "@codex".to_owned(),
        status: AgentStatus::Waiting,
        dwell_secs: 10,
        met_at: None,
    };
    for (second, status, dwell, met) in [
        (1_059, AgentStatus::Running, None, false),
        (1_060, AgentStatus::Waiting, Some(0), false),
        (1_069, AgentStatus::Waiting, Some(9), false),
        (1_070, AgentStatus::Waiting, Some(10), true),
    ] {
        let now = Timestamp::from_second(second).unwrap();
        let live = snapshot(watched.clone(), false, now);
        let evaluation = evaluate_when_condition(&condition, &live, now, Duration::from_secs(30));
        assert_eq!(evaluation.check.status, Some(status), "at {second}");
        assert_eq!(evaluation.check.dwell_so_far_secs, dwell);
        assert_eq!(evaluation.check.met, met);
        let trip_at = Timestamp::from_second(1_070).unwrap();
        assert_eq!(evaluation.check.trip_at, Some(trip_at));
        assert_eq!(evaluation.retry_at, (!met).then_some(trip_at));
    }
    watched.context = Some(crate::agents::AgentContext {
        permission_settle: Some(crate::agents::context::PermissionSettle {
            ask_since: at,
            at: at + Duration::from_secs(2),
        }),
        ..Default::default()
    });
    for second in [1_059, 1_070, 1_090] {
        let now = Timestamp::from_second(second).unwrap();
        let evaluation = evaluate_when_condition(
            &condition,
            &snapshot(watched.clone(), false, now),
            now,
            Duration::from_secs(30),
        );
        assert_eq!(evaluation.check.status, Some(AgentStatus::Running));
        assert!(!evaluation.check.met);
        assert!(evaluation.retry_at.is_some_and(|retry| retry > now));
    }
    watched
        .context
        .as_mut()
        .unwrap()
        .permission_settle
        .as_mut()
        .unwrap()
        .ask_since = at - Duration::from_secs(1);
    watched.last_activity = at + Duration::from_secs(4);
    let now = at + Duration::from_secs(90);
    let evaluation = evaluate_when_condition(
        &condition,
        &snapshot(watched, false, now),
        now,
        Duration::from_secs(30),
    );
    assert_eq!(evaluation.check.status, Some(AgentStatus::Waiting));
    assert!(
        evaluation.check.met,
        "without matching proof, keep raw-status matching"
    );
}

fn ready_check() -> DeliveryCheck {
    DeliveryCheck {
        typing_hold: None,
        register_by: None,
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
            permission_review: false,
            turn_ended: false,
        },
        pane: PaneCheck {
            present: true,
            pane_id: Some(PaneId::from_parts(MuxName::Zellij, "terminal_3")),
        },
    }
}

fn snapshot(agent: AgentState, with_pane: bool, now: Timestamp) -> SidebarSnapshot {
    let mut snapshot = SidebarSnapshot::build_with_agents(workspace_id(), vec![agent], now);
    if with_pane {
        let agent = &snapshot.agents[0];
        snapshot.agent_panes = vec![PaneAgent {
            checkout: None,
            root_lane: false,
            lane: None,
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
        }]
        .into();
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
