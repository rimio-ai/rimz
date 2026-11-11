use super::*;
use crate::store::event::MessageEventMethod;
use crate::store::message::{DeliveryGate, MessageRecord};

fn lifecycle(at: i64, id: &str, signal: serde_json::Value) -> EventEnvelope {
    raw_lifecycle_at("codex", at, json!({"agent_id": id, "signal": signal}))
}

fn message(at: i64, id: &str, tag: &str, status: MessageStatus) -> EventEnvelope {
    let mut record = MessageRecord::new(
        workspace(),
        &agent("codex", id, AgentStatus::Running, 0),
        "wrap up".to_owned(),
        DeliveryGate::Any,
    );
    record.status = status;
    let method = match status {
        MessageStatus::Sent => MessageEventMethod::Sent,
        status => {
            MessageEventMethod::for_terminal_status(status).unwrap_or(MessageEventMethod::Queued)
        }
    };
    let mut event = EventEnvelope::message_event(&record, "session", method, None);
    let mut params = event.params_value();
    params["soft_compact"] = json!(tag);
    event.params = serde_json::value::to_raw_value(&params).unwrap();
    event.timestamp = epoch() + std::time::Duration::from_secs(at.try_into().unwrap());
    event
}

fn mark(events: &[EventEnvelope], id: &str) -> serde_json::Value {
    let agents = reduce_agent_states(events);
    let agent = agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == id)
        .unwrap();
    serde_json::to_value(agent).unwrap()["soft_compact"].clone()
}

#[test]
fn soft_compact_mark_records_only_a_clean_turn_end_at_or_after_the_steer() {
    let mut events = vec![
        lifecycle(0, "sess-1", json!({"signal": "registered"})),
        lifecycle(1, "sess-1", json!({"signal": "turn_started"})),
        message(2, "sess-1", "wrap_up", MessageStatus::Sent),
    ];
    for (at, signal) in [
        (3, json!({"signal": "compacting"})),
        (4, json!({"signal": "compaction_ended", "auto": true})),
        (
            5,
            json!({"signal": "turn_ended", "errored": true, "parked_on_background": false}),
        ),
    ] {
        events.push(lifecycle(at, "sess-1", signal));
        assert_ne!(mark(&events, "sess-1")["progress"], "turn_ended");
    }
    events.push(lifecycle(6, "sess-1", json!({"signal": "turn_started"})));
    events.push(lifecycle(
        6,
        "sess-1",
        json!({"signal": "turn_ended", "errored": false, "parked_on_background": false}),
    ));
    assert_eq!(mark(&events, "sess-1")["progress"], "turn_ended");

    let mut tied = events[..3].to_vec();
    tied.push(lifecycle(
        2,
        "sess-1",
        json!({"signal": "turn_ended", "errored": false, "parked_on_background": false}),
    ));
    assert_eq!(mark(&tied, "sess-1")["progress"], "turn_ended");
}

#[test]
fn soft_compact_mark_tracks_wrap_up_sent_and_continue_queued_only() {
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
    let steered =
        json!({"steered_at": events[6].timestamp, "compactions": 1, "progress": "wrapping_up"});
    let mut queued = steered.clone();
    queued["progress"] = json!("continue_queued");
    assert_eq!(
        outcomes,
        [
            json!(null),
            json!(null),
            steered.clone(),
            steered.clone(),
            steered,
            queued.clone(),
            queued
        ]
    );
}

#[test]
fn soft_compact_mark_finishes_when_its_continue_reaches_any_terminal_status() {
    let events = vec![
        lifecycle(0, "sess-1", json!({"signal": "registered"})),
        lifecycle(1, "sess-1", json!({"signal": "turn_started"})),
        message(2, "sess-1", "wrap_up", MessageStatus::Sent),
        lifecycle(
            3,
            "sess-1",
            json!({"signal": "turn_ended", "errored": false, "parked_on_background": false}),
        ),
    ];
    let progress = |events: &[EventEnvelope]| mark(events, "sess-1")["progress"].clone();
    // A continue that settles without its queued step leaves the owed step in place.
    let mut unqueued = events.clone();
    unqueued.push(message(4, "sess-1", "continue", MessageStatus::Canceled));
    assert_eq!(progress(&unqueued), "turn_ended");

    let mut queued = events;
    queued.push(message(4, "sess-1", "continue", MessageStatus::Queued));
    assert_eq!(progress(&queued), "continue_queued");
    for status in [MessageStatus::Claimed, MessageStatus::Sent] {
        let mut open = queued.clone();
        open.push(message(5, "sess-1", "continue", status));
        assert_eq!(progress(&open), "continue_queued", "{status:?}");
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
        settled.push(message(5, "sess-1", "continue", status));
        assert_eq!(progress(&settled), "continued", "{status:?}");
    }
}

#[test]
fn soft_compact_mark_finishes_when_a_real_turn_opens_after_its_continue_queued() {
    let queued = vec![
        lifecycle(0, "sess-1", json!({"signal": "registered"})),
        lifecycle(1, "sess-1", json!({"signal": "turn_started"})),
        message(2, "sess-1", "wrap_up", MessageStatus::Sent),
        lifecycle(
            3,
            "sess-1",
            json!({"signal": "turn_ended", "errored": false, "parked_on_background": false}),
        ),
        message(4, "sess-1", "continue", MessageStatus::Queued),
    ];
    let progress = |events: &[EventEnvelope]| mark(events, "sess-1")["progress"].clone();
    let keepalive = "Type: CACHE_KEEPALIVE\nFrom: @rimz\nContent:\nKeep warm.";
    for (label, params) in [
        (
            "keepalive ping",
            json!({"agent_id": "sess-1", "signal": {"signal": "turn_started"}, "prompt": keepalive}),
        ),
        (
            "state snapshot",
            json!({"agent_id": "sess-1", "signal": {"signal": "turn_started"}, "state_snapshot": true}),
        ),
    ] {
        let mut events = queued.clone();
        events.push(raw_lifecycle_at("codex", 5, params));
        assert_eq!(progress(&events), "continue_queued", "{label}");
    }

    let mut opened = queued.clone();
    opened.push(lifecycle(5, "sess-1", json!({"signal": "turn_started"})));
    assert_eq!(progress(&opened), "continued");
    opened.push(message(6, "sess-1", "continue", MessageStatus::Delivered));
    assert_eq!(progress(&opened), "continued");

    // A continue that settles on the predecessor leaves the continuation's
    // inherited mark queued until the continuation's own turn opens.
    let mut continuation = queued;
    continuation.push(raw_lifecycle_at(
        "codex",
        5,
        json!({
            "agent_id": "continuation", "compacted_from": "sess-1",
            "signal": {"signal": "compaction_ended"}
        }),
    ));
    continuation.push(message(6, "sess-1", "continue", MessageStatus::Delivered));
    assert_eq!(
        mark(&continuation, "continuation")["progress"],
        "continue_queued"
    );
    continuation.push(lifecycle(
        7,
        "continuation",
        json!({"signal": "turn_started"}),
    ));
    assert_eq!(mark(&continuation, "continuation")["progress"], "continued");
}

#[test]
fn soft_compact_mark_survives_lifecycle_but_not_interrupt_or_clear() {
    let events = vec![
        lifecycle(0, "sess-1", json!({"signal": "registered"})),
        lifecycle(1, "sess-1", json!({"signal": "turn_started"})),
        message(2, "sess-1", "wrap_up", MessageStatus::Sent),
    ];
    let steered =
        json!({"steered_at": events[2].timestamp, "compactions": 0, "progress": "wrapping_up"});
    let mut ended = steered.clone();
    ended["progress"] = json!("turn_ended");
    let outcomes = [
        vec![json!({"signal": "tool_used", "mutates": true, "edits": true})],
        vec![json!({"signal": "turn_ended", "errored": false, "parked_on_background": false})],
        vec![
            json!({"signal": "compacting"}),
            json!({"signal": "compaction_ended", "auto": true}),
        ],
        vec![
            json!({"signal": "turn_ended", "errored": false, "parked_on_background": false}),
            json!({"signal": "compacting"}),
            json!({"signal": "compaction_ended", "auto": false}),
        ],
        vec![json!({"signal": "turn_started"})],
        vec![json!({"signal": "turn_interrupted"})],
        vec![json!({"signal": "registered"})],
    ]
    .map(|signals| {
        let mut events = events.clone();
        for (offset, signal) in signals.into_iter().enumerate() {
            events.push(lifecycle(
                3 + i64::try_from(offset).unwrap(),
                "sess-1",
                signal,
            ));
        }
        mark(&events, "sess-1")
    });
    assert_eq!(
        outcomes,
        [
            steered.clone(),
            ended.clone(),
            steered.clone(),
            ended,
            steered,
            json!(null),
            json!(null)
        ]
    );
}

#[test]
fn soft_compact_mark_follows_a_linked_compaction_continuation() {
    let events = vec![
        lifecycle(0, "predecessor", json!({"signal": "registered"})),
        lifecycle(1, "predecessor", json!({"signal": "turn_started"})),
        message(2, "predecessor", "wrap_up", MessageStatus::Sent),
        lifecycle(
            3,
            "predecessor",
            json!({"signal": "turn_ended", "errored": false, "parked_on_background": false}),
        ),
    ];
    for already_continued in [false, true] {
        let mut events = events.clone();
        if already_continued {
            events.push(message(4, "predecessor", "continue", MessageStatus::Queued));
        }
        events.push(raw_lifecycle_at(
            "codex",
            5,
            json!({
                "agent_id": "continuation", "compacted_from": "predecessor",
                "signal": {"signal": "compaction_ended"}
            }),
        ));
        let inherited = mark(&events, "continuation");
        let owed = json!({
            "steered_at": events[2].timestamp, "compactions": null,
            "progress": if already_continued { "continue_queued" } else { "turn_ended" }
        });
        events.push(message(
            6,
            "continuation",
            "continue",
            MessageStatus::Queued,
        ));
        let mut queued = owed.clone();
        queued["progress"] = json!("continue_queued");
        assert_eq!(
            (inherited, mark(&events, "continuation")),
            (owed.clone(), queued)
        );
        events.push(message(
            7,
            "continuation",
            "continue",
            MessageStatus::Delivered,
        ));
        let mut continued = owed;
        continued["progress"] = json!("continued");
        assert_eq!(mark(&events, "continuation"), continued);
    }
}
