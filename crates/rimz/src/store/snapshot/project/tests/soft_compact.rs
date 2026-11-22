use super::*;
use crate::ids::MessageId;
use crate::store::event::MessageEventMethod;
use crate::store::message::{DeliveryGate, MessageRecord};

fn lifecycle(at: i64, id: &str, signal: serde_json::Value) -> EventEnvelope {
    raw_lifecycle_at("codex", at, json!({"agent_id": id, "signal": signal}))
}

fn message(at: i64, id: &str, tag: &str, status: MessageStatus) -> EventEnvelope {
    let method = match status {
        MessageStatus::Sent => MessageEventMethod::Sent,
        status => {
            MessageEventMethod::for_terminal_status(status).unwrap_or(MessageEventMethod::Queued)
        }
    };
    rewrite(at, id, tag, status, method, None)
}

/// A message event `method` with `reason`, as a requeue, an edit, or a rebind writes one.
fn rewrite(
    at: i64,
    id: &str,
    tag: &str,
    status: MessageStatus,
    method: MessageEventMethod,
    reason: Option<&str>,
) -> EventEnvelope {
    let mut record = MessageRecord::new(
        workspace(),
        &agent("codex", id, AgentStatus::Running, 0),
        "wrap up".to_owned(),
        DeliveryGate::Any,
    );
    record.status = status;
    record.message_id = tagged_id(tag);
    let mut event = EventEnvelope::message_event(&record, "session", method, reason);
    let mut params = event.params_value();
    params["soft_compact"] = json!(tag);
    event.params = serde_json::value::to_raw_value(&params).unwrap();
    event.timestamp = epoch() + std::time::Duration::from_secs(at.try_into().unwrap());
    event
}

/// One message id per tag, so a tag's events in a test name the same record, as the
/// rebind and every rewrite of one record do.
fn tagged_id(tag: &str) -> MessageId {
    let digit = if tag == "continue" { 'c' } else { 'a' };
    MessageId::parse(&format!("msg_{}", digit.to_string().repeat(16))).unwrap()
}

/// `event` rewritten to name another record, a second continue beside the tag's own.
fn naming(mut event: EventEnvelope, message_id: &MessageId) -> EventEnvelope {
    let mut params = event.params_value();
    params["message_id"] = json!(message_id.as_str());
    event.params = serde_json::value::to_raw_value(&params).unwrap();
    event
}

/// The mark's owed continue, the tag's record enqueued `at` seconds past the epoch.
fn queued_at(at: i64) -> serde_json::Value {
    json!({
        "message_id": tagged_id("continue").as_str(),
        "queued_at": epoch() + std::time::Duration::from_secs(at.try_into().unwrap()),
    })
}

fn mark(events: &[EventEnvelope], id: &str) -> serde_json::Value {
    let agents = reduce_agent_states(events);
    let agent = agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == id)
        .unwrap();
    serde_json::to_value(agent).unwrap()["soft_compact_mark"].clone()
}

#[test]
fn reminder_mark_records_its_epoch_once_sent_and_continue_queued_marks_it_owed() {
    let mut events = vec![
        lifecycle(0, "sess-1", json!({"signal": "registered"})),
        lifecycle(1, "sess-1", json!({"signal": "compacting"})),
        lifecycle(
            2,
            "sess-1",
            json!({"signal": "compaction_ended", "auto": false}),
        ),
        lifecycle(3, "sess-1", json!({"signal": "turn_started"})),
    ];
    let steps = [
        message(4, "sess-1", "wrap_up", MessageStatus::Queued),
        message(5, "sess-1", "wrap_up", MessageStatus::Claimed),
        message(6, "sess-1", "wrap_up", MessageStatus::Sent),
        message(7, "sess-1", "wrap_up", MessageStatus::Delivered),
        message(8, "sess-1", "continue", MessageStatus::Sent),
        message(9, "sess-1", "continue", MessageStatus::Queued),
        message(10, "sess-1", "wrap_up", MessageStatus::Delivered),
    ];
    let outcomes = steps.map(|event| {
        events.push(event);
        mark(&events, "sess-1")
    });
    let reminded = json!({"compactions": 1});
    let owed = json!({"compactions": 1, "continue_owed": queued_at(9)});
    assert_eq!(
        outcomes,
        [
            json!(null),
            json!(null),
            reminded.clone(),
            reminded.clone(),
            reminded,
            owed.clone(),
            owed
        ]
    );
    let bare = vec![
        lifecycle(0, "sess-1", json!({"signal": "registered"})),
        lifecycle(1, "sess-1", json!({"signal": "turn_started"})),
        message(2, "sess-1", "continue", MessageStatus::Queued),
    ];
    assert_eq!(
        mark(&bare, "sess-1"),
        json!({"continue_owed": queued_at(2)}),
        "a self-compaction owes its continue without a reminder"
    );
}

#[test]
fn owed_continue_settles_at_any_terminal_status_of_its_record() {
    let queued = vec![
        lifecycle(0, "sess-1", json!({"signal": "registered"})),
        lifecycle(1, "sess-1", json!({"signal": "turn_started"})),
        message(2, "sess-1", "continue", MessageStatus::Queued),
    ];
    let owed = |events: &[EventEnvelope]| !mark(events, "sess-1")["continue_owed"].is_null();
    assert!(owed(&queued));
    for status in [MessageStatus::Claimed, MessageStatus::Sent] {
        let mut open = queued.clone();
        open.push(message(3, "sess-1", "continue", status));
        assert!(owed(&open), "{status:?}");
    }
    for status in [
        MessageStatus::Delivered,
        MessageStatus::TimedOut,
        MessageStatus::Errored,
        MessageStatus::Canceled,
        MessageStatus::Abandoned,
        MessageStatus::Expired,
        MessageStatus::Archived,
    ] {
        assert!(status.is_terminal());
        let mut settled = queued.clone();
        settled.push(message(3, "sess-1", "continue", status));
        assert!(!owed(&settled), "{status:?}");
    }
}

#[test]
fn owed_continue_clears_on_interrupt_clear_or_user_prompt_and_a_clear_rearms_the_reminder() {
    let queued = vec![
        lifecycle(0, "sess-1", json!({"signal": "registered"})),
        lifecycle(1, "sess-1", json!({"signal": "turn_started"})),
        message(2, "sess-1", "wrap_up", MessageStatus::Sent),
        message(3, "sess-1", "continue", MessageStatus::Queued),
    ];
    let owed = json!({"compactions": 0, "continue_owed": queued_at(3)});
    let cleared = json!({"compactions": 0});
    let keepalive = "Type: CACHE_KEEPALIVE\nFrom: @rimz\nContent:\nKeep warm.";
    let notice = "Type: WAIT\nFrom: @rimz\nContent:\nwaited on `cargo test`";
    let started = |prompt: Option<&str>| {
        let mut params = json!({"agent_id": "sess-1", "signal": {"signal": "turn_started"}});
        if let Some(prompt) = prompt {
            params["prompt"] = json!(prompt);
        }
        params
    };
    let turn_end = json!({"signal": "turn_ended", "errored": false, "parked_on_background": false});
    let cases = [
        (
            "tool use",
            vec![
                json!({"agent_id": "sess-1", "signal": {"signal": "tool_used", "mutates": true, "edits": true}}),
            ],
            &owed,
        ),
        (
            "clean turn end",
            vec![json!({"agent_id": "sess-1", "signal": turn_end})],
            &owed,
        ),
        (
            "compaction",
            vec![
                json!({"agent_id": "sess-1", "signal": turn_end}),
                json!({"agent_id": "sess-1", "signal": {"signal": "compacting"}}),
                json!({"agent_id": "sess-1", "signal": {"signal": "compaction_ended", "auto": false}}),
            ],
            &owed,
        ),
        ("keepalive ping", vec![started(Some(keepalive))], &owed),
        ("harness notice", vec![started(Some(notice))], &owed),
        (
            "harness notice after the compaction",
            vec![
                json!({"agent_id": "sess-1", "signal": turn_end}),
                json!({"agent_id": "sess-1", "signal": {"signal": "compacting"}}),
                json!({"agent_id": "sess-1", "signal": {"signal": "compaction_ended", "auto": false}}),
                started(Some(notice)),
            ],
            &cleared,
        ),
        (
            "keepalive ping after the compaction",
            vec![
                json!({"agent_id": "sess-1", "signal": turn_end}),
                json!({"agent_id": "sess-1", "signal": {"signal": "compacting"}}),
                json!({"agent_id": "sess-1", "signal": {"signal": "compaction_ended", "auto": false}}),
                started(Some(keepalive)),
            ],
            &owed,
        ),
        (
            "automatic compaction inside the open turn",
            vec![
                json!({"agent_id": "sess-1", "signal": {"signal": "compacting"}}),
                json!({"agent_id": "sess-1", "signal": {"signal": "compaction_ended", "auto": true}}),
            ],
            &cleared,
        ),
        (
            "state snapshot",
            vec![
                json!({"agent_id": "sess-1", "signal": {"signal": "turn_started"}, "state_snapshot": true}),
            ],
            &owed,
        ),
        (
            "user prompt",
            vec![started(Some("keep going on the parser"))],
            &cleared,
        ),
        (
            "interrupt",
            vec![json!({"agent_id": "sess-1", "signal": {"signal": "turn_interrupted"}})],
            &cleared,
        ),
        (
            "/clear",
            vec![json!({"agent_id": "sess-1", "signal": {"signal": "registered"}})],
            &json!(null),
        ),
    ];
    for (label, params, expected) in cases {
        let mut events = queued.clone();
        for (offset, params) in params.into_iter().enumerate() {
            events.push(raw_lifecycle_at(
                "codex",
                4 + i64::try_from(offset).unwrap(),
                params,
            ));
        }
        assert_eq!(&mark(&events, "sess-1"), expected, "{label}");
    }
}

#[test]
fn owed_continue_follows_a_linked_compaction_continuation() {
    let events = vec![
        lifecycle(0, "predecessor", json!({"signal": "registered"})),
        lifecycle(1, "predecessor", json!({"signal": "turn_started"})),
        message(2, "predecessor", "wrap_up", MessageStatus::Sent),
        message(3, "predecessor", "continue", MessageStatus::Queued),
        lifecycle(
            4,
            "predecessor",
            json!({"signal": "turn_ended", "errored": false, "parked_on_background": false}),
        ),
    ];
    let mut events = events.clone();
    events.push(raw_lifecycle_at(
        "codex",
        5,
        json!({
            "agent_id": "continuation", "compacted_from": "predecessor",
            "signal": {"signal": "compaction_ended"}
        }),
    ));
    assert_eq!(
        mark(&events, "continuation"),
        json!({"continue_owed": queued_at(3)}),
        "the successor inherits the owed continue and a fresh reminder epoch"
    );
    events.push(message(
        6,
        "continuation",
        "continue",
        MessageStatus::Delivered,
    ));
    assert_eq!(mark(&events, "continuation"), json!({}));
}

#[test]
fn a_rewrite_of_a_cleared_continue_does_not_owe_it_again() {
    let queued = vec![
        lifecycle(0, "sess-1", json!({"signal": "registered"})),
        lifecycle(1, "sess-1", json!({"signal": "turn_started"})),
        message(2, "sess-1", "continue", MessageStatus::Queued),
        lifecycle(3, "sess-1", json!({"signal": "turn_interrupted"})),
    ];
    assert_eq!(mark(&queued, "sess-1"), json!({}));
    let rewrites = [
        (
            MessageEventMethod::Queued,
            "pane busy; claim released",
            "a released claim or a reconcile requeue",
        ),
        (MessageEventMethod::Edited, "text", "an edit"),
    ];
    for (method, reason, label) in rewrites {
        let mut events = queued.clone();
        events.push(rewrite(
            4,
            "sess-1",
            "continue",
            MessageStatus::Queued,
            method,
            Some(reason),
        ));
        assert_eq!(mark(&events, "sess-1"), json!({}), "{label}");
    }
    let mut events = queued;
    events.push(raw_lifecycle_at(
        "codex",
        4,
        json!({
            "agent_id": "continuation", "compacted_from": "sess-1",
            "signal": {"signal": "compaction_ended"}
        }),
    ));
    events.push(rewrite(
        5,
        "continuation",
        "continue",
        MessageStatus::Queued,
        MessageEventMethod::Edited,
        Some("compaction continuation changed session"),
    ));
    assert_eq!(
        mark(&events, "continuation"),
        json!({}),
        "the rebind onto a Codex successor does not owe a continue the predecessor cleared"
    );
}

#[test]
fn only_the_owed_continues_own_ending_settles_it() {
    let newer = MessageId::parse("msg_dddddddddddddddd").unwrap();
    let mut events = vec![
        lifecycle(0, "sess-1", json!({"signal": "registered"})),
        lifecycle(1, "sess-1", json!({"signal": "turn_started"})),
        message(2, "sess-1", "continue", MessageStatus::Queued),
        naming(
            message(3, "sess-1", "continue", MessageStatus::Queued),
            &newer,
        ),
    ];
    let owed_newer = json!({"continue_owed": {
        "message_id": newer.as_str(),
        "queued_at": epoch() + std::time::Duration::from_secs(3),
    }});
    assert_eq!(mark(&events, "sess-1"), owed_newer);
    events.push(message(4, "sess-1", "continue", MessageStatus::Canceled));
    assert_eq!(
        mark(&events, "sess-1"),
        owed_newer,
        "the older continue's withdrawal leaves the newer one owed"
    );
    events.push(naming(
        message(5, "sess-1", "continue", MessageStatus::Delivered),
        &newer,
    ));
    assert_eq!(mark(&events, "sess-1"), json!({}));
}
