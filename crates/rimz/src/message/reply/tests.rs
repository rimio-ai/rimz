use std::io::Write;

use jiff::Timestamp;

use super::*;
use crate::ids::WorkspaceId;
use crate::message::DeliveryGate;
use crate::store::event::{EventEnvelope, MessageEventMethod};
use crate::store::{RuntimePaths, StatePaths};

fn card(status: AgentStatus, started: i64) -> Option<CardView> {
    Some(CardView {
        status,
        turn_started_at: Some(Timestamp::from_second(started).unwrap()),
    })
}

#[test]
fn delivery_and_reply_transitions_preserve_turn_boundaries() {
    assert_eq!(
        step(
            WaitPhase::Delivery,
            false,
            MessageStatus::Queued,
            card(AgentStatus::Idle, 1),
        ),
        Step::Wait(WaitPhase::Delivery)
    );
    assert_eq!(
        step(
            WaitPhase::Delivery,
            true,
            MessageStatus::Sent,
            card(AgentStatus::Running, 1),
        ),
        Step::Wait(WaitPhase::Reply {
            turn_started_at: Some(Timestamp::from_second(1).unwrap()),
        })
    );
    assert_eq!(
        step(
            WaitPhase::Reply {
                turn_started_at: Some(Timestamp::from_second(1).unwrap()),
            },
            true,
            MessageStatus::Sent,
            card(AgentStatus::Running, 2),
        ),
        Step::Finish(RunStatus::Completed)
    );
    for status in [AgentStatus::Waiting, AgentStatus::Paused] {
        assert!(matches!(
            step(
                WaitPhase::Reply {
                    turn_started_at: None,
                },
                false,
                MessageStatus::Delivered,
                card(status, 1),
            ),
            Step::Wait(WaitPhase::Reply { .. })
        ));
    }
}

#[test]
fn construction_settled_leg_is_reported_before_store_poll() {
    let target = ReplyTarget {
        kind: AgentKind::new_unchecked("codex"),
        agent_id: AgentSessionId::from("session"),
        agent_name: None,
        label: "@coder".to_owned(),
        cursor: None,
        transcript_path: None,
    };
    let outcome = DispatchOutcome::SkippedWaiting {
        label: "@coder".to_owned(),
        message_id: MessageId::new(),
    };
    let mut wait = ReplyWait {
        legs: vec![Leg::new(target, &outcome, 0)],
        steer: true,
        join: ReplyJoin::Any,
        caller_identity: None,
        tick: 0,
    };

    let indices = wait.take_unreported();
    let update = wait.update(indices);

    assert_eq!(update.settled.len(), 1);
    assert_eq!(
        update.settled[0].failure,
        Some(ReplyFailure::WaitingForInput)
    );
    assert_eq!(update.join.unwrap().status, RunStatus::Failed);
}

#[test]
fn reply_target_anchors_transcript_before_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("transcript.jsonl");
    std::fs::write(
        &transcript,
        "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"old answer\"}]}}\n",
    )
    .unwrap();
    let mut agent = crate::testkit::agent_state("claude", "sess-reply", Timestamp::UNIX_EPOCH);
    agent.transcript_path = Some(transcript.to_string_lossy().into_owned());
    let adapter = crate::agents::find_definition("claude").unwrap();
    let mut target = ReplyTarget::new(&agent, "@claude".to_owned(), adapter);

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    writeln!(
        file,
        "{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"fresh answer\"}}]}}}}"
    )
    .unwrap();

    let messages = target.cursor.as_mut().unwrap().messages(
        agent.transcript_path.as_deref(),
        Some(&agent.agent_id),
        adapter,
    );
    assert_eq!(messages, ["fresh answer"]);
}

#[test]
fn parked_reply_reanchors_when_delivery_starts() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    runtime.ensure_dirs().unwrap();
    let store = Store::open(paths, runtime).unwrap();
    let transcript = dir.path().join("transcript.jsonl");
    std::fs::write(
        &transcript,
        "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"old answer\"}]}}\n",
    )
    .unwrap();
    let mut agent = crate::testkit::agent_state("claude", "sess-reply", Timestamp::UNIX_EPOCH);
    agent.status = AgentStatus::Running;
    agent.transcript_path = Some(transcript.to_string_lossy().into_owned());
    let adapter = crate::agents::find_definition("claude").unwrap();
    let target = ReplyTarget::new(&agent, "@claude".to_owned(), adapter);
    let outcome = DispatchOutcome::Queued {
        label: "@claude".to_owned(),
        message_id: MessageId::new(),
        reason: None,
    };
    let mut leg = Leg::new(target, &outcome, 0);

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    writeln!(
        file,
        "{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"intervening answer\"}}]}}}}"
    )
    .unwrap();
    let mut message = MessageRecord::new(
        workspace_id.clone(),
        &agent,
        "queued request".to_owned(),
        true,
        DeliveryGate::Any,
    );
    message.message_id = leg.message_id.clone();
    message.status = MessageStatus::Delivered;
    let running = SidebarSnapshot::build_with_agents(
        workspace_id.clone(),
        vec![agent.clone()],
        Timestamp::now(),
    );

    assert!(!advance_leg(&mut leg, &store, &[message.clone()], &running, false).unwrap());
    assert_eq!(leg.last_message, None);

    agent.status = AgentStatus::Idle;
    let idle = SidebarSnapshot::build_with_agents(workspace_id, vec![agent], Timestamp::now());
    assert!(advance_leg(&mut leg, &store, &[message], &idle, false).unwrap());
    assert_eq!(leg.result().final_message, None);
}

#[test]
fn terminal_delivery_failures_win_over_missing_card() {
    for status in [
        MessageStatus::TimedOut,
        MessageStatus::Errored,
        MessageStatus::Canceled,
        MessageStatus::Abandoned,
        MessageStatus::Archived,
    ] {
        assert_eq!(
            step(WaitPhase::Delivery, false, status, None),
            Step::DeliveryFailed(status)
        );
    }
    assert_eq!(
        step(
            WaitPhase::Reply {
                turn_started_at: None,
            },
            false,
            MessageStatus::Delivered,
            None,
        ),
        Step::AgentGone
    );
}

#[test]
fn progress_reports_phase_and_fanout_counts() {
    let single = wait_with_legs(
        vec![leg("@planner", MessageStatus::Queued, None)],
        ReplyJoin::All,
    );
    assert_eq!(
        single.progress(),
        ReplyProgress::Target {
            label: "@planner".to_owned(),
            parked: true,
        }
    );
    let fanout = wait_with_legs(
        vec![
            leg(
                "@planner",
                MessageStatus::Delivered,
                Some(RunStatus::Completed),
            ),
            leg("@reviewer", MessageStatus::Sent, None),
            leg("@coder", MessageStatus::Queued, None),
        ],
        ReplyJoin::All,
    );
    assert_eq!(
        fanout.progress(),
        ReplyProgress::Fanout {
            pending: 2,
            total: 3,
        }
    );
}

#[test]
fn gather_uses_first_failure_in_target_order() {
    let wait = wait_with_legs(
        vec![
            leg("@one", MessageStatus::Delivered, Some(RunStatus::Completed)),
            leg(
                "@two",
                MessageStatus::Delivered,
                Some(RunStatus::BudgetExceeded),
            ),
            leg("@three", MessageStatus::Delivered, Some(RunStatus::Failed)),
        ],
        ReplyJoin::All,
    );
    assert_eq!(
        wait.join_result(None).unwrap().status,
        RunStatus::BudgetExceeded
    );
}

#[test]
fn any_uses_observed_winner_status() {
    let wait = wait_with_legs(
        vec![
            leg("@one", MessageStatus::Sent, None),
            leg("@two", MessageStatus::Delivered, Some(RunStatus::Failed)),
            leg(
                "@three",
                MessageStatus::Delivered,
                Some(RunStatus::Completed),
            ),
        ],
        ReplyJoin::Any,
    );
    assert_eq!(wait.join_result(Some(1)).unwrap().status, RunStatus::Failed);
    assert!(wait.join_result(None).is_none());
}

#[test]
fn terminal_message_poll_reads_only_appended_bytes_after_base() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    runtime.ensure_dirs().unwrap();
    let store = Store::open(paths.clone(), runtime).unwrap();
    let agent = agent("reply", AgentStatus::Idle);
    let mut old = MessageRecord::new(
        workspace_id.clone(),
        &agent,
        "old".to_owned(),
        true,
        DeliveryGate::Any,
    );
    old.status = MessageStatus::Delivered;
    event_log::append(
        &paths.events_log,
        &EventEnvelope::message_event(&old, "session", MessageEventMethod::Delivered, None),
    )
    .unwrap();
    let mut base = store.wait_fold_base().unwrap();

    let before = event_log::testkit::bytes_read();
    assert_eq!(
        latest_terminal_message_status(&store, &old.message_id, &mut base).unwrap(),
        None
    );
    assert_eq!(event_log::testkit::bytes_read() - before, 0);

    let mut message = MessageRecord::new(
        workspace_id,
        &agent,
        "new".to_owned(),
        true,
        DeliveryGate::Any,
    );
    message.status = MessageStatus::Delivered;
    event_log::append(
        &paths.events_log,
        &EventEnvelope::message_event(&message, "session", MessageEventMethod::Delivered, None),
    )
    .unwrap();
    let log_len = std::fs::metadata(&paths.events_log).unwrap().len();
    let appended = log_len - base;
    let before = event_log::testkit::bytes_read();

    assert_eq!(
        latest_terminal_message_status(&store, &message.message_id, &mut base).unwrap(),
        Some(MessageStatus::Delivered)
    );
    assert_eq!(event_log::testkit::bytes_read() - before, appended);
    assert_eq!(base, log_len);
}

#[test]
fn detects_mutual_queued_cycle() {
    let agents = [
        agent("a", AgentStatus::Running),
        agent("b", AgentStatus::Running),
    ];
    let live = [
        wait_message(&agents[0], &agents[1], 1, MessageStatus::Queued),
        wait_message(&agents[1], &agents[0], 2, MessageStatus::Queued),
    ];

    let cycle = wait_cycle(&live, &[], &agents, &agents[0].kind, "a", &agents[1])
        .expect("mutual wait closes a cycle");

    assert_eq!(cycle, [hop("@b", 2)]);
}

#[test]
fn detects_an_immediate_self_wait_cycle_without_edges() {
    let agents = [agent("a", AgentStatus::Running)];

    let cycle = wait_cycle(&[], &[], &agents, &agents[0].kind, "a", &agents[0])
        .expect("waiting on the caller itself is an immediate cycle");

    assert!(cycle.is_empty());
}

#[test]
fn detects_three_agent_chain_and_renders_it() {
    let agents = [
        agent("a", AgentStatus::Running),
        agent("b", AgentStatus::Running),
        agent("c", AgentStatus::Running),
    ];
    let live = [
        wait_message(&agents[1], &agents[2], 2, MessageStatus::Sent),
        wait_message(&agents[2], &agents[0], 3, MessageStatus::Claimed),
    ];

    let cycle = wait_cycle(&live, &[], &agents, &agents[0].kind, "a", &agents[1])
        .expect("chain reaches caller");

    assert_eq!(cycle, [hop("@b", 2), hop("@c", 3)]);
    assert_eq!(render_chain(&cycle).as_deref(), Some("@b → @c → you"));
}

#[test]
fn detects_delivered_wait_that_opened_reply_turn() {
    let mut agents = [
        agent("a", AgentStatus::Running),
        agent("b", AgentStatus::Running),
    ];
    let delivered = wait_message(&agents[1], &agents[0], 7, MessageStatus::Delivered);
    agents[0].context = Some(crate::agents::AgentContext::new("codex", Timestamp::now()));
    agents[0].context.as_mut().unwrap().turn_opened_by = vec![delivered.message_id.clone()];

    let cycle = wait_cycle(&[], &[delivered], &agents, &agents[0].kind, "a", &agents[1])
        .expect("delivered wait remains live through reply turn");

    assert_eq!(cycle, [hop("@b", 7)]);
}

#[test]
fn dependency_graph_ignores_non_running_and_unnamed_senders() {
    let mut agents = [
        agent("a", AgentStatus::Running),
        agent("b", AgentStatus::Idle),
    ];
    let mut message = wait_message(&agents[1], &agents[0], 2, MessageStatus::Queued);
    assert!(
        wait_cycle(
            &[message.clone()],
            &[],
            &agents,
            &agents[0].kind,
            "a",
            &agents[1]
        )
        .is_none()
    );

    agents[1].status = AgentStatus::Running;
    if let MessageSender::Agent { name, .. } = &mut message.sender {
        *name = None;
    }
    assert!(wait_cycle(&[message], &[], &agents, &agents[0].kind, "a", &agents[1]).is_none());
}

#[test]
fn youngest_message_yields_cycle() {
    let cycle = [hop("@b", 2), hop("@c", 9)];
    assert_eq!(youngest_wait_message(&cycle, &message_id(7)), message_id(9));
    assert_eq!(
        youngest_wait_message(&cycle, &message_id(10)),
        message_id(10)
    );
}

fn wait_with_legs(legs: Vec<Leg>, join: ReplyJoin) -> ReplyWait {
    ReplyWait {
        legs,
        steer: false,
        join,
        caller_identity: None,
        tick: 0,
    }
}

fn leg(label: &str, status: MessageStatus, done: Option<RunStatus>) -> Leg {
    Leg {
        target: ReplyTarget {
            kind: AgentKind::new_unchecked("codex"),
            agent_id: AgentSessionId::from("session"),
            agent_name: None,
            label: label.to_owned(),
            cursor: None,
            transcript_path: None,
        },
        message_id: MessageId::new(),
        phase: WaitPhase::Delivery,
        message_status: status,
        wait_base: 0,
        cursor: None,
        last_message: None,
        transcript_path: None,
        done,
        failure: None,
        reported: false,
    }
}

fn agent(name: &str, status: AgentStatus) -> AgentState {
    let mut agent = AgentState::stub("codex", &format!("sess-{name}"), status);
    agent.name = Some(name.to_owned());
    agent.name_explicit = true;
    agent.kind_ordinal = None;
    agent
}

fn wait_message(
    sender: &AgentState,
    receiver: &AgentState,
    id: u64,
    status: MessageStatus,
) -> MessageRecord {
    let mut message = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-wait-guard")),
        receiver,
        "reply".to_owned(),
        true,
        DeliveryGate::Done,
    )
    .with_sender(MessageSender::Agent {
        kind: sender.kind.clone(),
        name: sender.name.clone(),
        profile: None,
        role: None,
        channel: None,
    })
    .with_reply_wait(true);
    message.message_id = message_id(id);
    message.status = status;
    message
}

fn hop(handle: &str, id: u64) -> WaitCycleHop {
    WaitCycleHop {
        handle: handle.to_owned(),
        message_id: message_id(id),
    }
}

fn message_id(id: u64) -> MessageId {
    MessageId::parse(&format!("msg_{id:016x}")).unwrap()
}
