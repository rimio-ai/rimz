use super::*;

use crate::agents::lifecycle::LifecycleSignal;
use crate::agents::{LaunchParams, SessionOrigin};
use crate::harness::run::PermissionMode;
use crate::ids::{AgentSessionId, MuxName, PaneId};
use crate::pane::{RuntimeOwner, RuntimeOwnerKind};

fn workspace() -> WorkspaceId {
    WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-event-test"))
}

fn lifecycle_observation() -> AgentLifecycleObservation {
    AgentLifecycleObservation {
        agent_id: Some(AgentSessionId::from("sess-1")),
        agent_name: Some("amber-atlas".to_owned()),
        launch: LaunchParams {
            profile: Some("claude-reviewer".to_owned()),
            mode: Some(PermissionMode::Ask),
            role: Some("reviewer".to_owned()),
            model: Some("claude-opus".to_owned()),
            effort: Some("high".to_owned()),
            budget: None,
            team: Some("forge".to_owned()),
            launch_group: None,
            launch_ordinal: None,
            channel: None,
            kind_ordinal: Some(2),
        },
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
        context_pct: Some(80),
        context_window: Some(200_000),
        total_tokens: Some(10_000),
        turn_error: None,
        cache_read_input_tokens: Some(7_000),
        cache_write_input_tokens: Some(1_000),
        fresh_input_tokens: Some(2_000),
        output_tokens: Some(1_000),
        pane_id: Some(PaneId::from_parts(MuxName::Tmux, "%1")),
        pane_stamp: None,
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
            "team": "forge",
            "profile": "claude-reviewer",
            "mode": "ask",
            "kind_ordinal": 2,
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
    assert_eq!(payload.event_name.as_deref(), Some("Stop"));
    assert_eq!(payload.observation, observation);
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
        "launch_group",
        "launch_ordinal",
        "profile",
        "mode",
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
        "pane_stamp",
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
    assert_eq!(payload.observation.launch.role, None);
    assert_eq!(payload.observation.pane_id, None);
    assert_eq!(payload.observation.pane_stamp, None);
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
        "profile": "codex-coder",
        "mode": "yolo",
        "role": "coder",
        "model": "gpt-5.5-codex",
        "effort": "xhigh",
        "team": "forge",
        "launch_group": "launch_group_1",
        "launch_ordinal": 2,
        "channel": "design",
        "kind_ordinal": 1,
    }))
    .unwrap();

    assert_eq!(payload.launch.profile.as_deref(), Some("codex-coder"));
    assert_eq!(payload.launch.mode, Some(PermissionMode::Yolo));
    assert_eq!(payload.launch.team.as_deref(), Some("forge"));
    assert_eq!(
        payload.launch.launch_group.as_deref(),
        Some("launch_group_1")
    );
    assert_eq!(payload.launch.launch_ordinal, Some(2));
    assert_eq!(payload.launch.channel.as_deref(), Some("design"));
    assert_eq!(payload.launch.kind_ordinal, Some(1));
    assert!(!payload.agent_name_explicit);
    let encoded = serde_json::to_value(&payload).unwrap();
    assert_eq!(encoded["profile"], "codex-coder");
    assert_eq!(encoded["mode"], "yolo");
    assert_eq!(encoded["role"], "coder");
    assert_eq!(encoded["model"], "gpt-5.5-codex");
    assert_eq!(encoded["effort"], "xhigh");
    assert_eq!(encoded["team"], "forge");
    assert_eq!(encoded["launch_group"], "launch_group_1");
    assert_eq!(encoded["launch_ordinal"], 2);
    assert_eq!(encoded["channel"], "design");
    assert_eq!(encoded["kind_ordinal"], 1);
    assert!(encoded.get("agent_name_explicit").is_none());
    assert!(encoded.get("launch").is_none());

    let explicit: AgentLaunchPayload = serde_json::from_value(json!({
        "agent_id": "launch-2",
        "agent_name": "writer",
        "agent_name_explicit": true,
    }))
    .unwrap();
    assert!(explicit.agent_name_explicit);
    assert_eq!(
        serde_json::to_value(&explicit).unwrap()["agent_name_explicit"],
        true
    );
}

#[test]
fn shared_launch_params_stay_top_level_in_launch_and_lifecycle_events() {
    const KEYS: [&str; 10] = [
        "profile",
        "mode",
        "role",
        "model",
        "effort",
        "team",
        "launch_group",
        "launch_ordinal",
        "channel",
        "kind_ordinal",
    ];

    let launch = EventEnvelope::agent_launched(
        workspace(),
        "session",
        &AgentKind::new_unchecked("codex"),
        AgentLaunchPayload {
            agent_id: AgentSessionId::from("launch-1"),
            agent_name: "swift-otter".to_owned(),
            agent_name_explicit: false,
            launch: LaunchParams {
                profile: Some("codex-coder".to_owned()),
                mode: Some(PermissionMode::Yolo),
                role: Some("coder".to_owned()),
                model: Some("gpt-5.5-codex".to_owned()),
                effort: Some("xhigh".to_owned()),
                budget: None,
                team: Some("forge".to_owned()),
                launch_group: Some("launch_group_1".to_owned()),
                launch_ordinal: Some(2),
                channel: Some("design".to_owned()),
                kind_ordinal: Some(1),
            },
            state: AgentLaunchState::Starting,
            run_id: None,
            pane_id: None,
            runtime_owner: None,
            worktree_path: None,
            worktree_branch: None,
            prompt: None,
            description: None,
        },
    );
    let launch_params = params_value(&launch);
    for key in KEYS {
        assert!(launch_params.get(key).is_some(), "{key} stays top-level");
    }
    assert!(launch_params.get("launch").is_none());

    let mut observation = lifecycle_observation();
    observation.launch.launch_group = Some("launch_group_1".to_owned());
    observation.launch.launch_ordinal = Some(2);
    observation.launch.channel = Some("design".to_owned());
    let lifecycle = EventEnvelope::agent_lifecycle(
        workspace(),
        "session",
        "codex",
        "SessionStart",
        &observation,
    );
    let lifecycle_params = params_value(&lifecycle);
    for key in KEYS {
        assert!(lifecycle_params.get(key).is_some(), "{key} stays top-level");
    }
    assert!(lifecycle_params.get("launch").is_none());
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
fn session_death_constructor_decodes_typed_payload() {
    let typed = EventEnvelope::session_death(
        workspace(),
        "session",
        SessionDeathCause::Crash,
        vec![SessionDeathAgent {
            kind: AgentKind::new_unchecked("claude"),
            agent_id: AgentSessionId::from("sess-1"),
            name: Some("amber-atlas".to_owned()),
        }],
    );

    let EventKind::SessionDeath(payload) = typed.kind() else {
        panic!("session.death decodes as typed event");
    };
    assert_eq!(payload.cause, SessionDeathCause::Crash);
    assert_eq!(payload.lost_agents[0].agent_id, "sess-1");
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
        address: None,
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
        unconfirmed_sends: 0,
        last_attempt_at: None,
        last_error: None,
        delivered_at: None,
        not_before: None,
        after: Vec::new(),
        retry_after: None,
        auto_compact: None,
        compacted_context_tokens: None,
        batch_id: None,
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
            "unconfirmed_sends": 0,
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
    assert_eq!(payload.unconfirmed_sends, 0);
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
fn unresolved_message_event_round_trips_raw_address() {
    let event = EventEnvelope::unresolved_message_event(
        WorkspaceId::parse("ws_000000000000000000000000").unwrap(),
        "session",
        "@reviwer#docs".to_owned(),
        Some("docs".to_owned()),
        MessageSender::Human,
        12,
        "receiver not found".to_owned(),
    );

    let EventKind::Message { method, payload } = event.kind() else {
        panic!("message.errored decodes to its typed kind");
    };
    assert_eq!(method, MessageEventMethod::Errored);
    assert_eq!(payload.status, MessageStatus::Errored);
    assert_eq!(payload.address.as_deref(), Some("@reviwer#docs"));
    assert_eq!(payload.kind.as_str(), "unknown");
    assert_eq!(payload.channel.as_deref(), Some("docs"));
    assert_eq!(payload.reason.as_deref(), Some("receiver not found"));
}

#[test]
fn message_event_methods_round_trip_archived() {
    for method in [
        MessageEventMethod::Queued,
        MessageEventMethod::Edited,
        MessageEventMethod::AfterMet,
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
