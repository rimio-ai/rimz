use tempfile::tempdir;

use super::*;
use crate::agents::AgentLifecycleObservation;
use crate::agents::lifecycle::LifecycleSignal;
use crate::agents::{AgentState, AgentStatus};
use crate::ids::{MuxName, PaneId, WorkspaceId};
use crate::ledger::event_log;
use crate::message::{AutoCompact, DeliveryGate, MessageSender};
use crate::{RuntimePaths, StatePaths};

#[test]
fn claim_moves_message_out_of_pending_until_send_failure_requeues() {
    let (_dir, ledger, workspace_id) = ledger();
    let message = message(&workspace_id);
    ledger.queue_message(&message, "session").unwrap();

    let claimed = ledger
        .claim_message_for_delivery(&message.message_id, Timestamp::now())
        .unwrap()
        .expect("claimed");
    assert_eq!(claimed.status, MessageStatus::Claimed);
    assert_eq!(claimed.attempts, 1);
    assert!(ledger.list_pending_messages().unwrap().is_empty());

    ledger
        .record_message_delivery_failure(&message.message_id, "pane missing", "session")
        .unwrap();
    let pending = ledger.list_pending_messages().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].status, MessageStatus::Queued);
    assert_eq!(pending[0].last_error.as_deref(), Some("pane missing"));
}

#[test]
fn defer_message_wake_sets_retry_after_only_for_queued_messages() {
    let (_dir, ledger, workspace_id) = ledger();
    let queued = message(&workspace_id);
    let until = Timestamp::now() + Duration::from_secs(30);
    ledger.queue_message(&queued, "session").unwrap();

    ledger
        .defer_message_wake(&queued.message_id, until)
        .unwrap();

    let pending = ledger.list_pending_messages().unwrap();
    assert_eq!(pending[0].retry_after, Some(until));

    let claimed = ledger
        .claim_message_for_delivery(&queued.message_id, Timestamp::now())
        .unwrap()
        .expect("claimed");
    assert_eq!(claimed.retry_after, None);
    let sent = ledger
        .record_sent_message(&claimed, "session")
        .unwrap()
        .expect("sent");
    ledger
        .defer_message_wake(&sent.message_id, until + Duration::from_secs(30))
        .unwrap();

    let messages = ledger.list_messages().unwrap();
    assert_eq!(messages[0].status, MessageStatus::Sent);
    assert_eq!(messages[0].retry_after, None);

    ledger
        .defer_message_wake(&MessageId::parse("msg_0000000000000000").unwrap(), until)
        .unwrap();
}

#[test]
fn edit_message_updates_queued_record_and_appends_event() {
    let (_dir, ledger, workspace_id) = ledger();
    let message =
        message(&workspace_id).with_not_before(Some(Timestamp::now() + Duration::from_secs(60)));
    let until = Timestamp::now() + Duration::from_secs(30);
    ledger.queue_message(&message, "session").unwrap();
    ledger
        .defer_message_wake(&message.message_id, until)
        .unwrap();

    let edited = ledger
        .edit_message(
            &message.message_id,
            MessageEdit {
                text: Some("edited".to_owned()),
                gate: Some(DeliveryGate::Any),
                not_before: Some(None),
                force: Some(true),
                enter: Some(false),
                auto_compact: Some(Some(AutoCompact::Percent(70))),
            },
            "session",
        )
        .unwrap();

    let EditOutcome::Edited(edited) = edited else {
        panic!("message should edit");
    };
    assert_eq!(edited.text, "edited");
    assert_eq!(edited.gate, DeliveryGate::Any);
    assert_eq!(edited.not_before, None);
    assert_eq!(edited.retry_after, None);
    assert!(edited.force);
    assert!(!edited.enter);
    assert_eq!(edited.auto_compact, Some(AutoCompact::Percent(70)));

    let live = ledger.list_messages().unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0], *edited);
    let events = event_log::read_all(&ledger.inner.paths.events_log).unwrap();
    let edited_event = events
        .iter()
        .find(|event| event.method == "message.edited")
        .expect("edited event");
    let params: serde_json::Value = serde_json::from_str(edited_event.params.get()).unwrap();
    assert_eq!(
        params["reason"],
        "text, gate, schedule, force, enter, smart_compact"
    );
    assert_eq!(params["status"], "queued");
}

#[test]
fn edit_message_refuses_claimed_terminal_and_missing_records() {
    let (_dir, ledger, workspace_id) = ledger();
    let claimed = message(&workspace_id);
    ledger.queue_message(&claimed, "session").unwrap();
    ledger
        .claim_message_for_delivery(&claimed.message_id, Timestamp::now())
        .unwrap()
        .expect("claimed");

    let outcome = ledger
        .edit_message(
            &claimed.message_id,
            MessageEdit {
                text: Some("edited".to_owned()),
                ..MessageEdit::default()
            },
            "session",
        )
        .unwrap();

    assert_eq!(outcome, EditOutcome::NotOpen(MessageStatus::Claimed));

    let terminal = message(&workspace_id);
    ledger.queue_message(&terminal, "session").unwrap();
    ledger
        .settle_message(
            &terminal.message_id,
            MessageStatus::Delivered,
            "session",
            None,
        )
        .unwrap();

    let outcome = ledger
        .edit_message(
            &terminal.message_id,
            MessageEdit {
                text: Some("edited".to_owned()),
                ..MessageEdit::default()
            },
            "session",
        )
        .unwrap();

    assert_eq!(outcome, EditOutcome::NotOpen(MessageStatus::Delivered));
    assert_eq!(
        ledger
            .edit_message(
                &message_id(99),
                MessageEdit {
                    text: Some("edited".to_owned()),
                    ..MessageEdit::default()
                },
                "session",
            )
            .unwrap(),
        EditOutcome::NotFound
    );
}

#[test]
fn steer_claim_skips_fifo_and_schedule_but_keeps_claim_ttl() {
    let (_dir, queue_ledger, workspace_id) = ledger();
    let first = message(&workspace_id);
    std::thread::sleep(std::time::Duration::from_millis(2));
    let second = message(&workspace_id);
    queue_ledger.queue_message(&first, "session").unwrap();
    queue_ledger.queue_message(&second, "session").unwrap();

    assert!(
        queue_ledger
            .claim_message_for_delivery(&second.message_id, Timestamp::now())
            .unwrap()
            .is_none()
    );
    let claimed = queue_ledger
        .claim_message_for_steer(&second.message_id, Timestamp::now())
        .unwrap()
        .expect("steer claims non-head");
    assert_eq!(claimed.message_id, second.message_id);
    queue_ledger
        .record_message_delivery_failure(&second.message_id, "pane missing", "session")
        .unwrap();
    assert!(
        queue_ledger
            .claim_message_for_steer(&second.message_id, Timestamp::now())
            .unwrap()
            .is_none(),
        "fresh failed claim keeps the TTL guard"
    );

    let (_scheduled_dir, scheduled_ledger, scheduled_workspace_id) = ledger();
    let scheduled = message(&scheduled_workspace_id)
        .with_not_before(Some(Timestamp::now() + jiff::SignedDuration::from_secs(60)));
    scheduled_ledger
        .queue_message(&scheduled, "session")
        .unwrap();
    assert!(
        scheduled_ledger
            .claim_message_for_steer(&scheduled.message_id, Timestamp::now())
            .unwrap()
            .is_some(),
        "steer claims scheduled messages"
    );
}

#[test]
fn fifth_send_failure_abandons_message() {
    let (_dir, ledger, workspace_id) = ledger();
    let message = message(&workspace_id);
    ledger.queue_message(&message, "session").unwrap();

    for attempt in 1..=MAX_DELIVERY_ATTEMPTS {
        let claimed = ledger
            .claim_message_for_delivery(
                &message.message_id,
                Timestamp::now() + jiff::SignedDuration::from_secs(i64::from(attempt) * 20),
            )
            .unwrap()
            .expect("claimed");
        assert_eq!(claimed.attempts, attempt);
        ledger
            .record_message_delivery_failure(&message.message_id, "pane missing", "session")
            .unwrap();
    }

    assert!(ledger.list_pending_messages().unwrap().is_empty());
    let messages = ledger.list_messages().unwrap();
    assert!(messages.is_empty());
    let events = event_log::read_all(&ledger.inner.paths.events_log).unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.method == "message.abandoned"),
        "abandoned event missing: {events:?}"
    );
}

#[test]
fn orphan_gc_keeps_provisional_message_when_registered_card_name_is_live() {
    let (_dir, ledger, workspace_id) = ledger();
    let mut provisional = agent();
    provisional.agent_id = AgentSessionId::from("launch_a");
    provisional.name = Some("lucid-atlas".to_owned());
    let message = MessageRecord::new(
        workspace_id.clone(),
        &provisional,
        "next".to_owned(),
        true,
        DeliveryGate::Done,
    );
    ledger.queue_message(&message, "session").unwrap();

    let mut observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("real-session")),
        LifecycleSignal::Registered,
    );
    observation.agent_name = Some("lucid-atlas".to_owned());
    let event = EventEnvelope::agent_lifecycle(
        workspace_id,
        "session",
        "claude",
        "SessionStart",
        &observation,
    );
    ledger.append_event(&event).unwrap();

    let archived = ledger.archive_orphan_messages("session").unwrap();

    assert_eq!(archived, 0);
    let messages = ledger.list_messages().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].status, MessageStatus::Queued);
}

#[test]
fn orphan_gc_archives_open_messages_for_missing_receivers() {
    let (_dir, ledger, workspace_id) = ledger();
    let message = message(&workspace_id);
    ledger.queue_message(&message, "session").unwrap();

    let archived = ledger.archive_orphan_messages("session").unwrap();

    assert_eq!(archived, 1);
    let messages = ledger.list_messages().unwrap();
    assert!(messages.is_empty());
    let events = event_log::read_all(&ledger.inner.paths.events_log).unwrap();
    let archived = events
        .iter()
        .find(|event| event.method == "message.archived")
        .expect("archived event");
    let params: serde_json::Value = serde_json::from_str(archived.params.get()).unwrap();
    assert_eq!(params["reason"], "receiver ended");
}

#[test]
fn archive_messages_for_card_archives_matching_open_messages() {
    let (_dir, ledger, workspace_id) = ledger();
    let target = message(&workspace_id);
    let mut other = message(&workspace_id);
    other.agent_id = AgentSessionId::from("sess-2");
    ledger.queue_message(&target, "session").unwrap();
    ledger.queue_message(&other, "session").unwrap();

    let archived = ledger
        .archive_messages_for_card(
            &target.kind,
            &target.agent_id,
            target.agent_name.as_deref(),
            "receiver ended",
            "session",
        )
        .unwrap();

    assert_eq!(archived, 1);
    let messages = ledger.list_messages().unwrap();
    let untouched = messages
        .iter()
        .find(|record| record.message_id == other.message_id)
        .unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(untouched.status, MessageStatus::Queued);
    let events = event_log::read_all(&ledger.inner.paths.events_log).unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.method == "message.archived"),
        "archived event missing: {events:?}"
    );
}

#[test]
fn archive_channel_messages_archives_matching_open_messages() {
    let (_dir, ledger, workspace_id) = ledger();
    let docs = message(&workspace_id).with_channel(Some("docs".to_owned()));
    let ops = message(&workspace_id).with_channel(Some("ops".to_owned()));
    ledger.queue_message(&docs, "session").unwrap();
    ledger.queue_message(&ops, "session").unwrap();

    let archived = ledger
        .archive_channel_messages("docs", "worktree removed", "session")
        .unwrap();

    assert_eq!(archived, 1);
    let messages = ledger.list_messages().unwrap();
    let ops = messages
        .iter()
        .find(|record| record.message_id == ops.message_id)
        .unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(ops.status, MessageStatus::Queued);
    let events = event_log::read_all(&ledger.inner.paths.events_log).unwrap();
    let archived = events
        .iter()
        .find(|event| event.method == "message.archived")
        .expect("archived event");
    let params: serde_json::Value = serde_json::from_str(archived.params.get()).unwrap();
    assert_eq!(params["reason"], "worktree removed");
}

#[test]
fn only_fifo_head_can_be_claimed() {
    let (_dir, ledger, workspace_id) = ledger();
    let first = message(&workspace_id);
    std::thread::sleep(std::time::Duration::from_millis(2));
    let second = message(&workspace_id);
    ledger.queue_message(&first, "session").unwrap();
    ledger.queue_message(&second, "session").unwrap();

    assert!(
        ledger
            .claim_message_for_delivery(&second.message_id, Timestamp::now())
            .unwrap()
            .is_none()
    );
    assert!(
        ledger
            .claim_message_for_delivery(&first.message_id, Timestamp::now())
            .unwrap()
            .is_some()
    );
}

#[test]
fn queued_message_persists_sender() {
    let (_dir, ledger, workspace_id) = ledger();
    let sender = MessageSender::Agent {
        kind: AgentKind::new_unchecked("codex"),
        name: Some("swift-otter".to_owned()),
        profile: None,
        role: None,
        channel: Some("docs".to_owned()),
    };
    let message = message(&workspace_id).with_sender(sender.clone());
    ledger.queue_message(&message, "session").unwrap();

    let messages = ledger.list_messages().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].sender, sender);
}

#[test]
fn record_sent_then_turn_start_confirms_delivery() {
    let (_dir, ledger, workspace_id) = ledger();
    let message = message(&workspace_id).with_pane_id(PaneId::from_parts(MuxName::Tmux, "%1"));

    let sent = ledger
        .record_sent_message(&message, "session")
        .unwrap()
        .expect("sent");
    assert_eq!(sent.status, MessageStatus::Sent);
    assert_eq!(sent.pane_id.as_ref().map(PaneId::as_str), Some("tmux:%1"));
    assert!(ledger.list_pending_messages().unwrap().is_empty());

    let delivered = ledger
        .confirm_delivered_for_card(
            &message.kind,
            &message.agent_id,
            None,
            MessageBody::Prompt,
            "session",
        )
        .unwrap()
        .expect("delivered");
    assert_eq!(delivered.status, MessageStatus::Delivered);
    assert!(delivered.delivered_at.is_some());
    assert!(ledger.list_messages().unwrap().is_empty());
    let events = event_log::read_all(&ledger.inner.paths.events_log).unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.method == "message.delivered"),
        "delivered event missing: {events:?}"
    );
}

#[test]
fn record_sent_copies_send_time_batch_id_onto_loaded_record() {
    let (_dir, ledger, workspace_id) = ledger();
    let message = message(&workspace_id);
    let batch_id = message.message_id.clone();
    ledger.queue_message(&message, "session").unwrap();
    let mut claimed = ledger
        .claim_message_for_delivery(&message.message_id, Timestamp::now())
        .unwrap()
        .expect("claimed");
    claimed.batch_id = Some(batch_id.clone());

    let sent = ledger
        .record_sent_message(&claimed, "session")
        .unwrap()
        .expect("sent");

    assert_eq!(sent.batch_id, Some(batch_id));
    assert_eq!(
        ledger.list_messages().unwrap()[0].batch_id,
        Some(message.message_id)
    );
}

#[test]
fn confirmation_matches_message_body() {
    let (_dir, ledger, workspace_id) = ledger();
    let prompt = message(&workspace_id).with_pane_id(PaneId::from_parts(MuxName::Tmux, "%1"));
    let command = message(&workspace_id)
        .with_body(MessageBody::Command)
        .with_pane_id(PaneId::from_parts(MuxName::Tmux, "%1"));
    ledger.record_sent_message(&prompt, "session").unwrap();
    ledger.record_sent_message(&command, "session").unwrap();

    let delivered_command = ledger
        .confirm_delivered_for_card(
            &command.kind,
            &command.agent_id,
            None,
            MessageBody::Command,
            "session",
        )
        .unwrap()
        .expect("command delivered");
    assert_eq!(delivered_command.message_id, command.message_id);

    let messages = ledger.list_messages().unwrap();
    let prompt = messages
        .iter()
        .find(|message| message.message_id == prompt.message_id)
        .expect("prompt remains");
    assert_eq!(prompt.status, MessageStatus::Sent);
}

#[test]
fn confirmation_delivers_all_members_with_shared_batch_id() {
    let (_dir, ledger, workspace_id) = ledger();
    let mut first = message(&workspace_id).with_pane_id(PaneId::from_parts(MuxName::Tmux, "%1"));
    let mut second = message(&workspace_id).with_pane_id(PaneId::from_parts(MuxName::Tmux, "%1"));
    let mut unrelated =
        message(&workspace_id).with_pane_id(PaneId::from_parts(MuxName::Tmux, "%1"));
    first.message_id = message_id(1);
    second.message_id = message_id(2);
    unrelated.message_id = message_id(3);
    first.batch_id = Some(first.message_id.clone());
    second.batch_id = Some(first.message_id.clone());
    unrelated.batch_id = Some(message_id(99));
    ledger.record_sent_message(&first, "session").unwrap();
    ledger.record_sent_message(&second, "session").unwrap();
    ledger.record_sent_message(&unrelated, "session").unwrap();

    let delivered = ledger
        .confirm_delivered_for_card(
            &first.kind,
            &first.agent_id,
            None,
            MessageBody::Prompt,
            "session",
        )
        .unwrap()
        .expect("delivered");

    assert_eq!(delivered.message_id, first.message_id);
    let messages = ledger.list_messages().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].message_id, unrelated.message_id);
    assert_eq!(messages[0].status, MessageStatus::Sent);
    let delivered_count = event_log::read_all(&ledger.inner.paths.events_log)
        .unwrap()
        .iter()
        .filter(|event| event.method == "message.delivered")
        .count();
    assert_eq!(delivered_count, 2);
}

#[test]
fn confirmation_without_batch_id_delivers_only_oldest() {
    let (_dir, ledger, workspace_id) = ledger();
    let mut first = message(&workspace_id).with_pane_id(PaneId::from_parts(MuxName::Tmux, "%1"));
    let mut second = message(&workspace_id).with_pane_id(PaneId::from_parts(MuxName::Tmux, "%1"));
    first.message_id = message_id(1);
    second.message_id = message_id(2);
    ledger.record_sent_message(&first, "session").unwrap();
    ledger.record_sent_message(&second, "session").unwrap();

    ledger
        .confirm_delivered_for_card(
            &first.kind,
            &first.agent_id,
            None,
            MessageBody::Prompt,
            "session",
        )
        .unwrap()
        .expect("delivered");

    let messages = ledger.list_messages().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].message_id, second.message_id);
    assert_eq!(messages[0].status, MessageStatus::Sent);
}

#[test]
fn stale_sent_message_requeues_before_attempt_cap() {
    let (_dir, ledger, workspace_id) = ledger();
    let mut message = message(&workspace_id).with_pane_id(PaneId::from_parts(MuxName::Tmux, "%1"));
    message.batch_id = Some(message.message_id.clone());
    ledger.record_sent_message(&message, "session").unwrap();

    let report = ledger
        .reconcile_stale_sent_messages("session", Timestamp::now(), Duration::ZERO, 3)
        .unwrap();

    assert_eq!(report.requeued, 1);
    assert_eq!(report.timed_out, 0);
    let messages = ledger.list_messages().unwrap();
    assert_eq!(messages[0].status, MessageStatus::Queued);
    assert_eq!(messages[0].pane_id, None);
    assert_eq!(messages[0].batch_id, None);
    assert_eq!(messages[0].attempts, 0);
    assert_eq!(messages[0].unconfirmed_sends, 1);
    assert_eq!(messages[0].last_attempt_at, None);
    assert_eq!(
        messages[0].last_error.as_deref(),
        Some("delivery unconfirmed; re-queued")
    );
    let events = event_log::read_all(&ledger.inner.paths.events_log).unwrap();
    let queued = events
        .iter()
        .find(|event| event.method == "message.queued")
        .expect("reconcile queued event");
    let params: serde_json::Value = serde_json::from_str(queued.params.get()).unwrap();
    assert_eq!(params["reason"], "reconcile");
}

#[test]
fn stale_sent_message_times_out_at_attempt_cap() {
    let (_dir, ledger, workspace_id) = ledger();
    let mut message = message(&workspace_id).with_pane_id(PaneId::from_parts(MuxName::Tmux, "%1"));
    message.unconfirmed_sends = 3;
    ledger.record_sent_message(&message, "session").unwrap();

    let report = ledger
        .reconcile_stale_sent_messages("session", Timestamp::now(), Duration::ZERO, 3)
        .unwrap();

    assert_eq!(report.requeued, 0);
    assert_eq!(report.timed_out, 1);
    let messages = ledger.list_messages().unwrap();
    assert!(messages.is_empty());
    let events = event_log::read_all(&ledger.inner.paths.events_log).unwrap();
    let timed_out = events
        .iter()
        .find(|event| event.method == "message.timed_out")
        .expect("reconcile timed_out event");
    let params: serde_json::Value = serde_json::from_str(timed_out.params.get()).unwrap();
    assert_eq!(params["reason"], "reconcile");
}

#[test]
fn fresh_sent_message_waits_for_reconcile_deadline() {
    let (_dir, ledger, workspace_id) = ledger();
    let message = message(&workspace_id).with_pane_id(PaneId::from_parts(MuxName::Tmux, "%1"));
    ledger.record_sent_message(&message, "session").unwrap();

    let report = ledger
        .reconcile_stale_sent_messages("session", Timestamp::now(), Duration::from_secs(60), 3)
        .unwrap();

    assert_eq!(report, ReconcileReport::default());
    let messages = ledger.list_messages().unwrap();
    assert_eq!(messages[0].status, MessageStatus::Sent);
}

#[test]
fn earliest_message_wake_includes_sent_reconcile_deadline() {
    let (_dir, ledger, workspace_id) = ledger();
    let sent = ledger
        .record_sent_message(
            &message(&workspace_id).with_pane_id(PaneId::from_parts(MuxName::Tmux, "%1")),
            "session",
        )
        .unwrap()
        .expect("sent");
    let scheduled_at = sent.updated_at + Duration::from_secs(120);
    let scheduled = message(&workspace_id).with_not_before(Some(scheduled_at));
    ledger.queue_message(&scheduled, "session").unwrap();

    let wake = ledger
        .earliest_message_wake(sent.updated_at, Duration::from_secs(30))
        .unwrap();

    assert_eq!(wake, Some(sent.updated_at + Duration::from_secs(30)));
}

fn ledger() -> (tempfile::TempDir, Ledger, WorkspaceId) {
    let dir = tempdir().unwrap();
    let state_root = dir.path().join("state");
    let runtime_root = dir.path().join("runtime");
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let state = StatePaths::under(workspace_id.clone(), &state_root).unwrap();
    let runtime = RuntimePaths::under(workspace_id.clone(), &runtime_root).unwrap();
    (dir, Ledger::open(state, runtime).unwrap(), workspace_id)
}

fn message(workspace_id: &WorkspaceId) -> MessageRecord {
    MessageRecord::new(
        workspace_id.clone(),
        &agent(),
        "next".to_owned(),
        true,
        DeliveryGate::Done,
    )
}

fn message_id(value: u64) -> MessageId {
    MessageId::parse(&format!("msg_{value:016}")).unwrap()
}

fn agent() -> AgentState {
    let now = Timestamp::now();
    AgentState {
        agent_id: AgentSessionId::from("sess-1"),
        kind: AgentKind::new_unchecked("claude"),
        name: None,
        kind_ordinal: None,
        profile: None,
        role: None,
        team: None,
        launch_group: None,
        launch_ordinal: None,
        channel: None,
        status: AgentStatus::Idle,
        phase: crate::agents::TurnPhase::Idle,
        pane: None,
        runtime_owner: None,
        parent_agent_id: None,
        worktree_path: None,
        worktree_branch: None,
        task: None,
        prompt: None,
        description: None,
        transcript_path: None,
        origin: None,
        recent_prompts: Vec::new(),
        model: None,
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        cache_read_input_tokens: None,
        cache_write_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
        context: None,
        subagent_description: None,
        subagent_started_at: None,
        turn_started_at: None,
        waiting_since: None,
        compacting_since: None,
        compaction_count: 0,
        last_compact_command_tokens: None,
        last_seen: now,
        last_activity: now,
        registered_at: Some(now),
    }
}
