use std::path::Path;

use rimz::agents::{AgentCardRef, AgentState, AgentStatus, LaunchedBy};
use rimz::disk::lock::WorkspaceLock;
use rimz::harness::run;
use rimz::ids::{AgentKind, RunId};
use rimz::store::message::{
    DeliveryGate, HarnessNotice, MessageBody, MessageRecord, MessageSender, MessageStatus,
};
use rimz::store::run::RunStatus;
use rimz::store::writer::{DeliveryAck, DeliveryAckMatch};

use crate::common::Harness;

fn peer() -> AgentState {
    let mut peer = AgentState::stub("claude", "session", AgentStatus::Idle);
    peer.launch_id = Some("peer-launch".into());
    peer.name = Some("peer".into());
    peer.launched_by = Some(LaunchedBy {
        kind: AgentKind::new_unchecked("codex"),
        agent_id: "launcher".into(),
    });
    peer
}

fn launcher() -> MessageSender {
    MessageSender::Agent {
        kind: AgentKind::new_unchecked("codex"),
        agent_id: Some("launcher".into()),
        name: Some("launcher".into()),
        profile: None,
        role: None,
        channel: None,
    }
}

fn sent(
    h: &Harness,
    peer: &AgentState,
    texts: &[&str],
    sender: MessageSender,
    body: MessageBody,
) -> Vec<MessageRecord> {
    let records = texts
        .iter()
        .map(|text| {
            let mut record = MessageRecord::new(
                h.workspace_id.clone(),
                peer,
                (*text).into(),
                DeliveryGate::Done,
            );
            record.sender = sender.clone();
            record.body = body;
            h.store.queue_message(&record, "test").unwrap();
            record
        })
        .collect::<Vec<_>>();
    h.store.record_sent_batch(&records, "test").unwrap()
}

fn ack(h: &Harness, peer: &AgentState, ack: DeliveryAck<'_>) -> Vec<MessageRecord> {
    ack_in_run(h, peer, ack, None)
}

fn ack_in_run(
    h: &Harness,
    peer: &AgentState,
    ack: DeliveryAck<'_>,
    run_id: Option<&RunId>,
) -> Vec<MessageRecord> {
    let adapter = rimz::agents::registry::definition_by_kind("claude").unwrap();
    let mut observed = false;
    let delivered = h
        .store
        .confirm_delivered_for_card_with(
            AgentCardRef::new(&peer.kind, &peer.agent_id, peer.name.as_deref()),
            ack,
            "test",
            |records, selection| {
                observed = true;
                assert!(
                    WorkspaceLock::try_acquire(&h.store.paths().workspace_lock)
                        .unwrap()
                        .is_none()
                );
                assert!(
                    records
                        .iter()
                        .all(|record| record.status == MessageStatus::Sent)
                );
                assert!(
                    h.store
                        .list_messages()
                        .unwrap()
                        .iter()
                        .filter(|record| records
                            .iter()
                            .any(|selected| selected.message_id == record.message_id))
                        .all(|record| record.status == MessageStatus::Sent)
                );
                run::record_run_delivery(
                    h.store.paths(),
                    peer,
                    adapter,
                    records,
                    selection,
                    Path::new("/repo"),
                    run_id,
                )
                .unwrap();
                if selection == DeliveryAckMatch::PromptCorrelated
                    && !peer.is_team_seat()
                    && records.iter().any(|record| record.sender == launcher())
                {
                    assert!(
                        run::open_peer_run(h.store.paths(), peer).unwrap().is_some(),
                        "run is durable while ack still holds the lock"
                    );
                }
            },
        )
        .unwrap();
    assert_eq!(
        observed,
        !delivered.is_empty(),
        "ack consumer receives the selected records"
    );
    delivered
}

#[test]
fn foreground_peer_with_its_own_run_id_still_enrolls_launcher_turns() {
    for collected in [false, true] {
        foreground_peer_enrolls(collected);
    }
}

fn foreground_peer_enrolls(collected: bool) {
    let h = Harness::new();
    let peer = peer();
    let foreground = rimz::store::run::RunRecord::new(
        h.workspace_id.clone(),
        peer.kind.clone(),
        rimz::agents::PermissionMode::Auto,
        "foreground".into(),
        Path::new("/repo").into(),
    );
    if !collected {
        run::create(h.store.paths(), &foreground).unwrap();
    }
    sent(&h, &peer, &["first"], launcher(), MessageBody::Prompt);
    ack_in_run(
        &h,
        &peer,
        DeliveryAck::TurnStarted {
            prompt: Some("Type: AGENT_MESSAGE\nFrom: @launcher\nContent:\nfirst"),
        },
        Some(&foreground.run_id),
    );
    let enrolled = run::open_peer_run(h.store.paths(), &peer).unwrap().unwrap();
    assert_ne!(enrolled.run_id, foreground.run_id);
    assert_eq!(enrolled.prompt, "first", "collected={collected}");
}

#[test]
fn peer_ack_enrolls_one_run_and_joins_later_launcher_messages() {
    let h = Harness::new();
    let peer = peer();
    let messages = sent(
        &h,
        &peer,
        &["first", "second"],
        launcher(),
        MessageBody::Prompt,
    );
    let prompt = "Type: AGENT_MESSAGE\nFrom: @launcher\nContent:\nfirst\n\nType: AGENT_MESSAGE\nFrom: @launcher\nContent:\nsecond";
    assert_eq!(
        ack(
            &h,
            &peer,
            DeliveryAck::TurnStarted {
                prompt: Some(prompt)
            }
        )
        .len(),
        2
    );
    let record = run::open_peer_run(h.store.paths(), &peer).unwrap().unwrap();
    assert_eq!(record.status, RunStatus::Running);
    assert_eq!(record.agent_id.as_ref(), Some(&peer.agent_id));
    assert_eq!(record.prompt, "first\n\nsecond");
    assert_eq!(
        record.peer.as_ref().unwrap().opened_by,
        messages
            .iter()
            .map(|m| m.message_id.clone())
            .collect::<Vec<_>>()
    );
    assert!(!record.subagent);
    assert!(
        record.deadline_at.is_none()
            && record.timeout.is_none()
            && record.grace.is_none()
            && record.warn.is_empty()
    );
    let follow_up = sent(&h, &peer, &["continue"], launcher(), MessageBody::Prompt);
    ack(
        &h,
        &peer,
        DeliveryAck::TurnStarted {
            prompt: Some("Type: AGENT_MESSAGE\nFrom: @launcher\nContent:\ncontinue"),
        },
    );
    let joined = run::open_peer_run(h.store.paths(), &peer).unwrap().unwrap();
    assert_eq!(joined.run_id, record.run_id);
    assert_eq!(joined.prompt, record.prompt);
    assert!(
        joined
            .peer
            .unwrap()
            .opened_by
            .contains(&follow_up[0].message_id)
    );
    assert_eq!(run::list(h.store.paths()).unwrap().len(), 1);
}

#[test]
fn peer_ack_rejects_nonlauncher_and_uncorrelated_delivery() {
    let agent_prompt = Some("Type: AGENT_MESSAGE\nFrom: @launcher\nContent:\nwork");
    let mut wrong_id = launcher();
    if let MessageSender::Agent { agent_id, .. } = &mut wrong_id {
        *agent_id = Some("other".into());
    }
    let mut missing_id = launcher();
    if let MessageSender::Agent { agent_id, .. } = &mut missing_id {
        *agent_id = None;
    }
    let mut wrong_kind = launcher();
    if let MessageSender::Agent { kind, .. } = &mut wrong_kind {
        *kind = AgentKind::new_unchecked("claude");
    }
    for (sender, prompt, body) in [
        (
            MessageSender::Human,
            Some("Type: USER_MESSAGE\nFrom: @user\nContent:\nwork"),
            MessageBody::Prompt,
        ),
        (wrong_id, agent_prompt, MessageBody::Prompt),
        (missing_id, agent_prompt, MessageBody::Prompt),
        (wrong_kind, agent_prompt, MessageBody::Prompt),
        (
            MessageSender::Harness {
                notice: HarnessNotice::SubagentReport,
            },
            Some("Type: SUBAGENT_REPORT\nFrom: @rimz\nContent:\nwork"),
            MessageBody::Prompt,
        ),
        (launcher(), None, MessageBody::Prompt),
        (launcher(), Some("  "), MessageBody::Prompt),
        (launcher(), None, MessageBody::Command),
    ] {
        let h = Harness::new();
        let peer = peer();
        sent(&h, &peer, &["work"], sender, body);
        let confirmation = if body == MessageBody::Command {
            DeliveryAck::Compaction
        } else {
            DeliveryAck::TurnStarted { prompt }
        };
        let delivered = ack(&h, &peer, confirmation);
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].status, MessageStatus::Delivered);
        assert!(run::list(h.store.paths()).unwrap().is_empty());
    }
}

#[test]
fn launcher_steer_never_enrolls_a_later_human_turn() {
    for prompt in [Some("human follow-up"), None] {
        let h = Harness::new();
        let peer = peer();
        let mut steer = MessageRecord::new(
            h.workspace_id.clone(),
            &peer,
            "launcher correction mid-turn".into(),
            DeliveryGate::Any,
        );
        steer.sender = launcher();
        h.store.queue_message(&steer, "test").unwrap();
        let records = h.store.record_sent_batch(&[steer], "test").unwrap();
        let delivered = ack(&h, &peer, DeliveryAck::TurnStarted { prompt });
        assert_eq!(delivered.len(), usize::from(prompt.is_none()));
        let persisted = h.store.list_messages().unwrap();
        if prompt.is_some() {
            assert_eq!(persisted[0].message_id, records[0].message_id);
            assert_eq!(persisted[0].status, MessageStatus::Sent);
        } else {
            assert!(persisted.is_empty());
            assert_eq!(delivered[0].message_id, records[0].message_id);
            assert_eq!(delivered[0].status, MessageStatus::Delivered);
        }
        assert!(run::list(h.store.paths()).unwrap().is_empty());
    }
}

#[test]
fn peer_launch_prompt_is_pending_and_failure_is_terminal() {
    let h = Harness::new();
    let mut peer = peer();
    let adapter = rimz::agents::registry::definition_by_kind("claude").unwrap();
    assert!(run::peer_can_report(adapter));
    let record = run::create_peer_prompt(
        h.store.paths(),
        &peer,
        Some("launcher"),
        adapter,
        "launch task",
        Path::new("/repo"),
        rimz::store::run::ReportTo::Launcher,
    )
    .unwrap()
    .unwrap();
    assert_eq!(record.status, RunStatus::Pending);
    assert_eq!(record.reader.as_deref(), Some("launcher"));
    assert!(record.peer.as_ref().unwrap().opened_by.is_empty());
    assert!(
        record.agent_id.is_none(),
        "the first provider hook binds the session"
    );
    assert_eq!(
        run::create_peer_prompt(
            h.store.paths(),
            &peer,
            None,
            adapter,
            "duplicate",
            Path::new("/repo"),
            rimz::store::run::ReportTo::Launcher,
        )
        .unwrap()
        .unwrap()
        .run_id,
        record.run_id
    );
    let failed = run::fail_peer_run(&h.store, &peer, "restarting peer")
        .unwrap()
        .unwrap();
    assert_eq!(failed.status, RunStatus::Failed);
    assert_eq!(failed.failure_tail.as_deref(), Some("restarting peer"));
    assert!(
        run::open_peer_run(h.store.paths(), &peer)
            .unwrap()
            .is_none()
    );
    assert!(
        run::fail_peer_run(&h.store, &peer, "again")
            .unwrap()
            .is_none()
    );
    peer.launched_by = None;
    assert!(
        run::create_peer_prompt(
            h.store.paths(),
            &peer,
            None,
            adapter,
            "human",
            Path::new("/repo"),
            rimz::store::run::ReportTo::Launcher,
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(run::list(h.store.paths()).unwrap(), vec![failed]);
}

#[test]
fn team_seat_launch_prompt_opens_a_team_run_and_no_peer_turns() {
    let h = Harness::new();
    let mut leader = peer();
    leader.team = Some("forge".into());
    leader.channel = Some("feat-x".into());
    let adapter = rimz::agents::registry::definition_by_kind("claude").unwrap();
    let record = run::create_peer_prompt(
        h.store.paths(),
        &leader,
        None,
        adapter,
        "build the feature",
        Path::new("/repo"),
        rimz::store::run::ReportTo::Launcher,
    )
    .unwrap()
    .unwrap();
    assert!(record.peer.is_none(), "a team seat opens no peer turn");
    let team = record.team.as_ref().expect("the leader holds the team run");
    assert_eq!(team.instance, "forge#feat-x");
    assert_eq!(team.launch_id.as_str(), "peer-launch");
    assert_eq!(
        run::open_team_run(h.store.paths(), &leader)
            .unwrap()
            .map(|run| run.run_id),
        Some(record.run_id.clone())
    );
    assert!(
        run::open_peer_run(h.store.paths(), &leader)
            .unwrap()
            .is_none()
    );

    // A launcher message to any seat opens no per-turn report.
    let messages = sent(&h, &leader, &["next"], launcher(), MessageBody::Prompt);
    let prompt = "Type: AGENT_MESSAGE\nFrom: @launcher\nContent:\nnext";
    assert_eq!(
        ack(
            &h,
            &leader,
            DeliveryAck::TurnStarted {
                prompt: Some(prompt)
            }
        )
        .len(),
        messages.len()
    );
    assert_eq!(run::list(h.store.paths()).unwrap(), vec![record.clone()]);

    let failed = run::fail_peer_run(&h.store, &leader, "team launch failed")
        .unwrap()
        .unwrap();
    assert_eq!(failed.run_id, record.run_id);
    assert_eq!(failed.status, RunStatus::Failed);
}
