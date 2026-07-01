use super::*;

use crate::agents::codex::SessionOrigin;
use crate::agents::lifecycle::LifecycleSignal;
use crate::ids::{AgentSessionId, MuxName, PaneId};
use crate::pane::{RuntimeOwner, RuntimeOwnerKind};

fn workspace() -> WorkspaceId {
    WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-event-test"))
}

fn lifecycle_observation() -> AgentLifecycleObservation {
    AgentLifecycleObservation {
        agent_id: Some(AgentSessionId::from("sess-1")),
        agent_name: Some("amber-atlas".to_owned()),
        role: Some("reviewer".to_owned()),
        team: Some("pcr".to_owned()),
        channel: None,
        profile: Some("claude-reviewer".to_owned()),
        kind_ordinal: Some(2),
        signal: LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        },
        agent_pid: Some(42),
        agent_process_start: Some("12345".to_owned()),
        runtime_owner: Some(RuntimeOwner::new(
            RuntimeOwnerKind::Agent,
            "sess-1",
            42,
            Some("12345".to_owned()),
        )),
        worktree_path: Some("/tmp/project".to_owned()),
        worktree_branch: Some("main".to_owned()),
        task: Some("Review".to_owned()),
        prompt: Some("ship it".to_owned()),
        transcript_path: Some("/tmp/transcript.jsonl".to_owned()),
        origin: None,
        model: Some("claude-opus".to_owned()),
        effort: Some("high".to_owned()),
        context_pct: Some(80),
        context_window: Some(200_000),
        total_tokens: Some(10_000),
        turn_error: None,
        cache_read_input_tokens: Some(7_000),
        cache_write_input_tokens: Some(1_000),
        fresh_input_tokens: Some(2_000),
        output_tokens: Some(1_000),
        pane_id: Some(PaneId::from_parts(MuxName::Tmux, "%1")),
        parent_agent_id: Some(AgentSessionId::from("parent-1")),
    }
}

fn params_value(event: &EventEnvelope) -> Value {
    serde_json::from_str(event.params.get()).expect("event params decode")
}

fn raw_params_value(raw: &RawValue) -> Value {
    serde_json::from_str(raw.get()).expect("raw params decode")
}

#[test]
fn agent_lifecycle_constructor_serializes_compact_wire_shape() {
    let workspace = workspace();
    let observation = lifecycle_observation();
    let typed = EventEnvelope::agent_lifecycle(
        workspace.clone(),
        "session",
        "claude",
        "Stop",
        &observation,
    );
    let mut legacy = EventEnvelope::new(
        workspace,
        "session",
        "claude",
        "agent-hook",
        "agent.lifecycle",
        json!({
            "event_name": "Stop",
            "agent_id": "sess-1",
            "agent_name": "amber-atlas",
            "role": "reviewer",
            "team": "pcr",
            "profile": "claude-reviewer",
            "kind_ordinal": 2,
            "signal": {
                "signal": "turn_ended",
                "errored": false,
                "parked_on_background": false,
            },
            "agent_pid": 42,
            "agent_process_start": "12345",
            "worktree_path": "/tmp/project",
            "worktree_branch": "main",
            "task": "Review",
            "prompt": "ship it",
            "transcript_path": "/tmp/transcript.jsonl",
            "model": "claude-opus",
            "effort": "high",
            "context_pct": 80,
            "context_window": 200_000,
            "total_tokens": 10_000,
            "cache_read_input_tokens": 7_000,
            "cache_write_input_tokens": 1_000,
            "fresh_input_tokens": 2_000,
            "output_tokens": 1_000,
            "pane_id": "tmux:%1",
            "parent_agent_id": "parent-1",
        }),
    );
    legacy.event_id = typed.event_id.clone();
    legacy.timestamp = typed.timestamp;

    assert_eq!(
        serde_json::to_vec(&typed).unwrap(),
        serde_json::to_vec(&legacy).unwrap(),
        "typed construction owns the compact lifecycle event shape"
    );
    let EventKind::AgentLifecycle(payload) = typed.kind() else {
        panic!("agent.lifecycle decodes to its typed kind");
    };
    let payload = *payload;
    let mut expected_observation = observation;
    expected_observation.runtime_owner = None;
    assert_eq!(payload.event_name.as_deref(), Some("Stop"));
    assert_eq!(payload.observation, expected_observation);
}

#[test]
fn agent_lifecycle_constructor_omits_absent_fields() {
    let observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-null")),
        LifecycleSignal::Registered,
    );
    let event = EventEnvelope::agent_lifecycle(
        workspace(),
        "session",
        "claude",
        "SessionStart",
        &observation,
    );

    for key in [
        "agent_pid",
        "agent_name",
        "role",
        "team",
        "profile",
        "kind_ordinal",
        "agent_process_start",
        "runtime_owner",
        "worktree_path",
        "worktree_branch",
        "task",
        "prompt",
        "transcript_path",
        "origin",
        "model",
        "effort",
        "context_pct",
        "context_window",
        "total_tokens",
        "cache_read_input_tokens",
        "cache_write_input_tokens",
        "fresh_input_tokens",
        "output_tokens",
        "pane_id",
        "parent_agent_id",
    ] {
        let params = params_value(&event);
        assert!(
            params.get(key).is_none(),
            "{key} must be omitted when absent"
        );
    }
}

#[test]
fn old_shape_agent_lifecycle_params_still_decode() {
    let params = json!({
        "event_name": "Stop",
        "agent_id": "sess-1",
        "agent_name": "amber-atlas",
        "role": null,
        "team": null,
        "profile": null,
        "kind_ordinal": null,
        "signal": {
            "signal": "turn_ended",
            "errored": false,
            "parked_on_background": false,
        },
        "agent_pid": 42,
        "agent_process_start": "12345",
        "runtime_owner": {
            "kind": "agent",
            "subject_id": "sess-1",
            "pid": 42,
            "process_start": "12345",
        },
        "worktree_path": null,
        "worktree_branch": null,
        "task": null,
        "prompt": null,
        "transcript_path": null,
        "model": null,
        "effort": null,
        "context_pct": null,
        "context_window": null,
        "total_tokens": null,
        "cache_read_input_tokens": null,
        "cache_write_input_tokens": null,
        "fresh_input_tokens": null,
        "output_tokens": null,
        "pane_id": null,
        "parent_agent_id": null,
    });
    let payload: AgentLifecyclePayload = serde_json::from_value(params).expect("decode");

    assert_eq!(payload.event_name.as_deref(), Some("Stop"));
    assert_eq!(
        payload.observation.runtime_owner,
        Some(RuntimeOwner::new(
            RuntimeOwnerKind::Agent,
            "sess-1",
            42,
            Some("12345".to_owned()),
        ))
    );
    assert_eq!(payload.observation.role, None);
    assert_eq!(payload.observation.pane_id, None);
}

#[test]
fn agent_lifecycle_params_round_trip_codex_origin() {
    let params = json!({
        "event_name": "SessionStart",
        "agent_id": "codex-root",
        "signal": { "signal": "registered" },
        "origin": "fresh",
    });

    let payload: AgentLifecyclePayload = serde_json::from_value(params).expect("decode");

    assert_eq!(payload.observation.origin, Some(SessionOrigin::Fresh));
    let event = EventEnvelope::agent_lifecycle(
        workspace(),
        "session",
        "codex",
        "SessionStart",
        &payload.observation,
    );
    assert_eq!(params_value(&event).get("origin"), Some(&json!("fresh")));
}

#[test]
fn agent_launch_payload_round_trips_channel_identity() {
    let payload: AgentLaunchPayload = serde_json::from_value(json!({
        "agent_id": "launch-1",
        "agent_name": "swift-otter",
        "role": "coder",
        "team": "pcr",
        "channel": "design",
    }))
    .unwrap();

    assert_eq!(payload.team.as_deref(), Some("pcr"));
    assert_eq!(payload.channel.as_deref(), Some("design"));
    let encoded = serde_json::to_value(&payload).unwrap();
    assert_eq!(encoded["team"], "pcr");
    assert_eq!(encoded["channel"], "design");
}

#[test]
fn session_rebirth_constructor_keeps_the_existing_wire_shape() {
    let workspace = workspace();
    let typed = EventEnvelope::session_rebirth(workspace.clone(), "session");
    let mut legacy = EventEnvelope::new(
        workspace,
        "session",
        "rimz",
        "runtime",
        "session.rebirth",
        json!({}),
    );
    legacy.event_id = typed.event_id.clone();
    legacy.timestamp = typed.timestamp;

    assert_eq!(
        serde_json::to_vec(&typed).unwrap(),
        serde_json::to_vec(&legacy).unwrap()
    );
    assert!(matches!(typed.kind(), EventKind::SessionRebirth));
}

#[test]
fn agent_resumed_records_an_audit_event_on_the_other_carrier() {
    // Auto-continue is audit-only: the event rides the generic `Other`
    // carrier (it never folds into the agent rollup), so the rollup reduce
    // and wakeup paths skip it untouched while `feed list --audit` still
    // surfaces it by method.
    let event = EventEnvelope::agent_resumed(
        workspace(),
        "session",
        &AgentKind::new_unchecked("claude"),
        &AgentSessionId::from("sess-1"),
        &PaneId::from_parts(MuxName::Tmux, "%1"),
        "rate_limit_window_reset",
    );
    assert_eq!(event.method, "agent.resumed");
    let EventKind::Other { method, params } = event.kind() else {
        panic!("agent.resumed rides the Other audit carrier");
    };
    let params = raw_params_value(params);
    assert_eq!(method, "agent.resumed");
    assert_eq!(params["kind"], "claude");
    assert_eq!(params["agent_id"], "sess-1");
    assert_eq!(params["pane_id"], "tmux:%1");
    assert_eq!(params["reason"], "rate_limit_window_reset");
}

#[test]
fn message_event_constructor_keeps_text_out_of_the_wire_shape() {
    let now = Timestamp::now();
    let message = MessageRecord {
        message_id: MessageId::new(),
        workspace_id: workspace(),
        kind: AgentKind::new_unchecked("claude"),
        agent_id: AgentSessionId::from("sess-1"),
        agent_name: Some("lucid-atlas".to_owned()),
        channel: None,
        sender: MessageSender::Human,
        body: MessageBody::Prompt,
        text: "secret prompt body".to_owned(),
        enter: true,
        gate: DeliveryGate::Done,
        force: false,
        pane_id: None,
        status: MessageStatus::Queued,
        enqueued_at: now,
        updated_at: now,
        attempts: 0,
        last_attempt_at: None,
        last_error: None,
        delivered_at: None,
        not_before: None,
        auto_compact: None,
        compacted_context_tokens: None,
    };
    let typed = EventEnvelope::message_event(&message, "session", MessageEventMethod::Queued, None);
    let mut legacy = EventEnvelope::new(
        message.workspace_id.clone(),
        "session",
        "rimz",
        "cli",
        "message.queued",
        json!({
            "message_id": message.message_id.as_str(),
            "kind": "claude",
            "agent_id": "sess-1",
            "agent_name": "lucid-atlas",
            "gate": "done",
            "status": "queued",
            "body": "prompt",
            "forced": false,
            "text_len": "secret prompt body".len(),
            "enter": true,
            "attempts": 0,
            "reason": null,
            "enqueued_at": now,
        }),
    );
    legacy.event_id = typed.event_id.clone();
    legacy.timestamp = typed.timestamp;

    assert_eq!(
        serde_json::to_vec(&typed).unwrap(),
        serde_json::to_vec(&legacy).unwrap()
    );
    assert!(!typed.params.get().contains("secret prompt body"));
    let EventKind::Message { method, payload } = typed.kind() else {
        panic!("message.queued decodes to its typed kind");
    };
    assert_eq!(method, MessageEventMethod::Queued);
    assert_eq!(payload.message_id, message.message_id);
    assert_eq!(payload.text_len, "secret prompt body".len());
    assert_eq!(payload.reason, None);
    assert_eq!(payload.agent_name.as_deref(), Some("lucid-atlas"));
    assert_eq!(payload.enqueued_at, Some(now));

    let sender = MessageSender::Agent {
        kind: AgentKind::new_unchecked("codex"),
        name: Some("swift-otter".to_owned()),
        profile: None,
        role: None,
        channel: Some("docs".to_owned()),
    };
    let attributed = message.with_sender(sender.clone());
    let event =
        EventEnvelope::message_event(&attributed, "session", MessageEventMethod::Queued, None);

    let params = params_value(&event);
    assert_eq!(params["sender"]["kind"], "codex");
    assert!(!event.params.get().contains("secret prompt body"));
    let EventKind::Message { payload, .. } = event.kind() else {
        panic!("message.queued decodes to its typed kind");
    };
    assert_eq!(payload.sender, Some(sender));
}

#[test]
fn message_event_methods_round_trip_archived() {
    for method in [
        MessageEventMethod::Queued,
        MessageEventMethod::Sent,
        MessageEventMethod::Delivered,
        MessageEventMethod::TimedOut,
        MessageEventMethod::Errored,
        MessageEventMethod::Removed,
        MessageEventMethod::Abandoned,
        MessageEventMethod::Archived,
    ] {
        assert_eq!(MessageEventMethod::parse(method.as_str()), Some(method));
    }
    assert_eq!(
        MessageEventMethod::for_terminal_status(MessageStatus::Archived),
        Some(MessageEventMethod::Archived)
    );
}

#[test]
fn unknown_event_kind_carries_the_raw_method_and_params() {
    let params = json!({ "request_id": "req_1" });
    let event = EventEnvelope::new(
        workspace(),
        "session",
        "claude",
        "agent-hook",
        "feed.push",
        params.clone(),
    );

    let EventKind::Other {
        method,
        params: raw,
    } = event.kind()
    else {
        panic!("feed.push stays in the open audit-event channel");
    };
    assert_eq!(method, "feed.push");
    assert_eq!(raw_params_value(raw), params);
}

#[test]
fn lifecycle_kind_tolerates_missing_identity() {
    let event = EventEnvelope::new(
        workspace(),
        "session",
        "claude",
        "agent-hook",
        "agent.lifecycle",
        json!({
            "event_name": "SessionStart",
            "signal": { "signal": "registered" },
        }),
    );

    let EventKind::AgentLifecycle(payload) = event.kind() else {
        panic!("partial lifecycle payload with a signal is still typed");
    };
    let payload = *payload;
    assert_eq!(payload.observation.agent_id, None);
    assert_eq!(payload.observation.signal, LifecycleSignal::Registered);
}

#[test]
fn non_conforming_lifecycle_event_decodes_as_other() {
    for params in [
        json!({ "status": "running", "event_name": "UserPromptSubmit" }),
        json!({ "signal": "not-an-object" }),
        json!({}),
    ] {
        let event = EventEnvelope::new(
            workspace(),
            "session",
            "claude",
            "agent-hook",
            "agent.lifecycle",
            params,
        );
        assert!(matches!(
            event.kind(),
            EventKind::Other {
                method: "agent.lifecycle",
                ..
            }
        ));
    }
}
