use super::*;

use crate::agents::LaunchParams;
use crate::agents::lifecycle::LifecycleSignal;
use crate::ids::{AgentSessionId, MuxName, PaneId};
use crate::pane::{RuntimeOwner, RuntimeOwnerKind};

fn workspace() -> WorkspaceId {
    WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-event-test"))
}

#[test]
fn envelope_without_ingress_keeps_its_old_shape_and_schema() {
    let old = format!(
        r#"{{"schema_version":"rimz.event.v2","event_id":"evt_0123456789abcdef0123456789abcdef","workspace_id":"{}","session_name":"session","source":"codex","source_kind":"agent","method":"future.event","timestamp":"2026-01-01T00:00:00Z","params":{{"text":"kept"}}}}"#,
        workspace(),
    );
    let decoded = serde_json::from_str::<EventEnvelope>(&old);
    assert!(
        decoded.is_ok(),
        "old-shape envelopes remain readable: {decoded:?}"
    );
    let event = decoded.unwrap();
    assert_eq!(event.ingress, None);
    assert_eq!(serde_json::to_string(&event).unwrap(), old);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.log.jsonl");
    std::fs::write(&path, format!("{} {old}\n", old.len())).unwrap();
    assert_eq!(
        crate::store::event_log::read_all(&path).unwrap(),
        vec![event]
    );
    let constructed = EventEnvelope::new(
        workspace(),
        "session",
        "codex",
        "agent",
        "future.event",
        json!({}),
    );
    assert_eq!(constructed.schema_version, "rimz.event.v2");
    assert!(
        !serde_json::to_value(constructed)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("ingress")
    );
}

#[test]
fn envelope_ingress_stamp_round_trips_and_distinguishes_frames() {
    let mut event = EventEnvelope::new(
        workspace(),
        "session",
        "codex",
        "agent",
        "future.event",
        json!({}),
    );
    let unstamped = event.clone();
    let id = EventId::new();
    event.ingress = Some(id.clone());
    assert_ne!(event, unstamped);
    let encoded = serde_json::to_string(&event).unwrap();
    assert_eq!(
        serde_json::from_str::<EventEnvelope>(&encoded)
            .unwrap()
            .ingress,
        Some(id)
    );
}

#[test]
fn launch_warnings_decode_with_optional_identity_and_future_fields() {
    for launch_id in [None, Some("launch-1")] {
        let event = EventEnvelope::new(
            workspace(),
            "session",
            "codex",
            "agent",
            "agent.launch_warnings",
            json!({"agent_id": "sess-1", "launch_id": launch_id,
                "warnings": ["degraded launch"], "future": true}),
        );
        assert!(!matches!(event.kind(), EventKind::Other { .. }));
        let constructed = EventEnvelope::agent_launch_warnings(
            workspace(),
            "session",
            &AgentKind::new_unchecked("codex"),
            AgentLaunchWarningsPayload {
                agent_id: "sess-1".into(),
                launch_id: launch_id.map(AgentSessionId::from),
                warnings: vec!["degraded launch".into()],
            },
        );
        assert_eq!(constructed.method, "agent.launch_warnings");
        assert_eq!(constructed.kind(), event.kind());
        assert_eq!(
            constructed.params_value().get("launch_id").is_some(),
            launch_id.is_some()
        );
    }
}

fn rich_lifecycle_observation() -> (AgentLifecycleObservation, Value) {
    let params = json!({
        "event_name": "Stop", "agent_id": "sess-1", "agent_name": "amber-atlas",
        "profile": "claude-reviewer", "mode": "ask", "role": "reviewer",
        "model": "claude-opus", "effort": "high", "budget": "$5.00/day",
        "team": "forge", "launch_group": "launch_group_1", "launch_ordinal": 2,
        "channel": "docs", "kind_ordinal": 2,
        "signal": {"signal": "turn_ended", "errored": false, "parked_on_background": false},
        "agent_pid": 42, "agent_process_start": "12345",
        "runtime_owner": {
            "kind": "agent", "subject_id": "sess-1", "pid": 42, "process_start": "12345",
        },
        "worktree_path": "/tmp/project", "worktree_branch": "main", "task": "Review",
        "prompt": "ship it", "transcript_path": "/tmp/transcript.jsonl", "origin": "fresh",
        "context_pct": 80, "context_window": 200_000, "total_tokens": 10_000,
        "cache_read_input_tokens": 7_000, "cache_write_input_tokens": 1_000,
        "fresh_input_tokens": 2_000, "output_tokens": 1_000,
        "pane_id": "tmux:%1",
        "pane_stamp": {
            "pane_id": "tmux:%1", "session_name": "", "view_id": null,
            "view_kind": null, "pane_process_start": null,
        },
        "parent_agent_id": "parent-1",
    });
    let payload: AgentLifecyclePayload =
        serde_json::from_value(params.clone()).expect("rich lifecycle fixture decodes");
    (payload.observation, params)
}

fn message_record() -> MessageRecord {
    let enqueued_at = Timestamp::from_second(1_700_000_000).expect("fixed timestamp");
    let mut message = MessageRecord::new_for_card(
        workspace(),
        AgentKind::new_unchecked("claude"),
        AgentSessionId::from("sess-1"),
        Some("amber-atlas".to_owned()),
        "secret prompt body".to_owned(),
        true,
        DeliveryGate::Done,
    );
    message.message_id = MessageId::parse("msg_0123456789abcdef").expect("fixed message id");
    message.address = Some("@reviewer#docs".to_owned());
    message.channel = Some("docs".to_owned());
    message.sender = MessageSender::Agent {
        agent_id: None,
        kind: AgentKind::new_unchecked("codex"),
        name: Some("swift-otter".to_owned()),
        profile: Some("codex-coder".to_owned()),
        role: Some("coder".to_owned()),
        channel: Some("docs".to_owned()),
    };
    message.force = true;
    message.pane_id = Some(PaneId::from_parts(MuxName::Tmux, "%1"));
    message.status = MessageStatus::Delivered;
    message.enqueued_at = enqueued_at;
    message.updated_at = enqueued_at;
    message.attempts = 2;
    message.unconfirmed_sends = 1;
    message.delivered_at =
        Some(Timestamp::from_second(1_700_000_030).expect("fixed delivered timestamp"));
    message.compacted_context_tokens = Some(120_000);
    message
}

#[test]
fn lifecycle_event_uses_flat_compact_wire_shape() {
    let (observation, expected_rich_params) = rich_lifecycle_observation();
    let rich =
        EventEnvelope::agent_lifecycle(workspace(), "session", "claude", "Stop", &observation);
    assert_eq!(rich.params_value(), expected_rich_params);
    assert!(
        rich.params_value().get("turn_error").is_none(),
        "sidecar-only turn errors stay out of durable events"
    );

    let minimal_observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-minimal")),
        LifecycleSignal::Registered,
    );
    let minimal = EventEnvelope::agent_lifecycle(
        workspace(),
        "session",
        "claude",
        "SessionStart",
        &minimal_observation,
    );
    assert_eq!(
        minimal.params_value(),
        json!({
            "event_name": "SessionStart",
            "agent_id": "sess-minimal",
            "signal": { "signal": "registered" },
        })
    );

    let EventKind::AgentLifecycle(payload) = rich.kind() else {
        panic!("rich lifecycle event decodes to its typed kind");
    };
    assert_eq!(*payload, AgentLifecyclePayload::new("Stop", &observation));
}

#[test]
fn lifecycle_event_projection_owns_carry_forward_wire_fields() {
    let mut full = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::Registered,
    );
    full.transcript_path = Some("/tmp/transcript.jsonl".to_owned());
    full.worktree_path = Some("/tmp/project".to_owned());
    full.worktree_branch = Some("feature".to_owned());
    full.launch = LaunchParams {
        role: Some("coder".to_owned()),
        team: Some("forge".to_owned()),
        channel: Some("event-log".to_owned()),
        profile: Some("claude-coder".to_owned()),
        ..LaunchParams::default()
    };
    full.pane_id = Some(PaneId::from_parts(MuxName::Tmux, "%1"));

    assert_eq!(observation_for_event(&full), full);
    full.signal = LifecycleSignal::TurnStarted { turn_id: None };
    let projected = observation_for_event(&full);
    let full_keys = serde_json::to_value(&full)
        .expect("full observation serializes")
        .as_object()
        .expect("observation is an object")
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let projected_keys = serde_json::to_value(&projected)
        .expect("projected observation serializes")
        .as_object()
        .expect("observation is an object")
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        full_keys
            .difference(&projected_keys)
            .cloned()
            .collect::<Vec<_>>(),
        ["channel", "profile", "role", "team", "transcript_path",]
    );
    assert_eq!(projected.pane_id.as_ref().map(PaneId::raw), Some("%1"));
    assert_eq!(projected.worktree_branch.as_deref(), Some("feature"));
    assert_eq!(projected.worktree_path.as_deref(), Some("/tmp/project"));

    full.signal = LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background: false,
        turn_id: None,
    };
    assert_eq!(
        observation_for_event(&full).transcript_path.as_deref(),
        Some("/tmp/transcript.jsonl")
    );
}

#[test]
fn lifecycle_decoder_handles_durable_compatibility_boundaries() {
    enum Expected {
        Legacy,
        Sparse,
        Other,
    }

    let cases = [
        (
            "legacy explicit nulls",
            AGENT_LIFECYCLE_METHOD,
            json!({
                "event_name": "Stop", "agent_id": "sess-1",
                "profile": null, "mode": null, "role": null, "model": null,
                "effort": null, "budget": null, "team": null,
                "launch_group": null, "launch_ordinal": null, "channel": null,
                "kind_ordinal": null,
                "signal": {"signal": "turn_ended", "errored": false,
                    "parked_on_background": false},
                "runtime_owner": {
                    "kind": "agent", "subject_id": "sess-1", "pid": 42,
                    "process_start": "12345",
                },
                "pane_id": null, "pane_stamp": null,
            }),
            Expected::Legacy,
        ),
        (
            "sparse signalled lifecycle",
            AGENT_LIFECYCLE_METHOD,
            json!({
                "event_name": "SessionStart",
                "signal": { "signal": "registered" },
            }),
            Expected::Sparse,
        ),
        (
            "signal-less lifecycle",
            AGENT_LIFECYCLE_METHOD,
            json!({ "status": "running", "event_name": "UserPromptSubmit" }),
            Expected::Other,
        ),
        (
            "non-object signal",
            AGENT_LIFECYCLE_METHOD,
            json!({ "signal": "not-an-object" }),
            Expected::Other,
        ),
        (
            "empty lifecycle",
            AGENT_LIFECYCLE_METHOD,
            json!({}),
            Expected::Other,
        ),
        (
            "unknown method",
            "feed.push",
            json!({ "request_id": "req_1" }),
            Expected::Other,
        ),
    ];

    for (name, method, params, expected) in cases {
        let event = EventEnvelope::new(
            workspace(),
            "session",
            "claude",
            "agent-hook",
            method,
            params.clone(),
        );
        match (expected, event.kind()) {
            (Expected::Legacy, EventKind::AgentLifecycle(payload)) => {
                assert_eq!(payload.event_name.as_deref(), Some("Stop"), "{name}");
                assert_eq!(
                    payload.observation.agent_id.as_deref(),
                    Some("sess-1"),
                    "{name}"
                );
                assert_eq!(
                    payload.observation.launch,
                    LaunchParams::default(),
                    "{name}"
                );
                assert_eq!(
                    payload.observation.runtime_owner,
                    Some(RuntimeOwner::new(
                        RuntimeOwnerKind::Agent,
                        "sess-1",
                        42,
                        Some("12345".to_owned()),
                    )),
                    "{name}"
                );
                assert_eq!(payload.observation.pane_id, None, "{name}");
                assert_eq!(payload.observation.pane_stamp, None, "{name}");
            }
            (Expected::Sparse, EventKind::AgentLifecycle(payload)) => {
                assert_eq!(payload.observation.agent_id, None, "{name}");
                assert_eq!(
                    payload.observation.signal,
                    LifecycleSignal::Registered,
                    "{name}"
                );
            }
            (
                Expected::Other,
                EventKind::Other {
                    method: raw_method,
                    params: raw,
                },
            ) => {
                assert_eq!(raw_method, method, "{name}");
                assert_eq!(
                    serde_json::from_str::<Value>(raw.get()).expect("raw params decode"),
                    params,
                    "{name}"
                );
            }
            _ => panic!("unexpected kind for {name}"),
        }
    }
}

#[test]
fn launch_event_uses_flat_compact_wire_shape() {
    let expected_rich_params = json!({
        "agent_id": "launch-1", "agent_name": "swift-otter", "agent_name_explicit": true,
        "profile": "codex-coder", "mode": "yolo", "role": "coder",
        "model": "gpt-5.5-codex", "effort": "xhigh", "budget": "$10.00/day",
        "team": "forge", "launch_group": "launch_group_1", "launch_ordinal": 2,
        "channel": "design", "kind_ordinal": 1, "state": "bound",
        "run_id": "run_0123456789abcdef0123456789abcdef",
        "pane_id": "tmux:%2",
        "runtime_owner": {
            "kind": "agent", "subject_id": "launch-1", "pid": 84, "process_start": "67890",
        },
        "worktree_path": "/tmp/project", "worktree_branch": "state-tests",
        "prompt": "reduce event tests", "description": "event schema pass",
    });
    let rich_payload: AgentLaunchPayload =
        serde_json::from_value(expected_rich_params.clone()).expect("rich launch fixture decodes");
    let rich = EventEnvelope::agent_launched(
        workspace(),
        "session",
        &AgentKind::new_unchecked("codex"),
        rich_payload.clone(),
    );
    assert_eq!(rich.params_value(), expected_rich_params);

    let minimal_payload: AgentLaunchPayload = serde_json::from_value(json!({
        "agent_id": "launch-minimal",
        "agent_name": "coder",
    }))
    .expect("minimal launch payload decodes with defaults");
    assert!(!minimal_payload.agent_name_explicit);
    assert_eq!(minimal_payload.state, AgentLaunchState::Starting);
    let minimal = EventEnvelope::agent_launched(
        workspace(),
        "session",
        &AgentKind::new_unchecked("codex"),
        minimal_payload,
    );
    assert_eq!(
        minimal.params_value(),
        json!({
            "agent_id": "launch-minimal", "agent_name": "coder", "state": "starting",
        })
    );

    let EventKind::AgentLaunch(payload) = rich.kind() else {
        panic!("rich launch event decodes to its typed kind");
    };
    assert_eq!(payload, rich_payload);
}

#[test]
fn attach_event_uses_typed_compact_wire_shape() {
    let pane_id = PaneId::from_parts(MuxName::Tmux, "%4");
    let payload = AgentAttachPayload {
        login: None,
        record: None,
        tier: None,
        mode: None,
        effective_isolation: None,
        agent_id: AgentSessionId::from("sess-1"),
        isolation: Some(crate::config::Isolation::Sandbox),
        launch_id: Some(AgentSessionId::from("launch-1")),
        pane_id: pane_id.clone(),
        pane_pid: Some(84),
        runtime_owner: RuntimeOwner::new(
            RuntimeOwnerKind::Agent,
            "sess-1",
            84,
            Some("67890".to_owned()),
        ),
    };
    let event = EventEnvelope::agent_attached(
        workspace(),
        "session",
        &AgentKind::new_unchecked("codex"),
        payload.clone(),
    );

    assert_eq!(event.source, "codex");
    assert_eq!(event.source_kind, "agent");
    assert_eq!(event.method, "agent.attached");
    assert_eq!(
        event.params_value(),
        json!({
            "agent_id": "sess-1",
            "launch_id": "launch-1",
            "isolation": "sandbox",
            "pane_id": "tmux:%4",
            "pane_pid": 84,
            "runtime_owner": {
                "kind": "agent",
                "subject_id": "sess-1",
                "pid": 84,
                "process_start": "67890",
            },
        })
    );
    let EventKind::AgentAttach(decoded) = event.kind() else {
        panic!("agent attach event decodes to its typed kind");
    };
    assert_eq!(decoded, payload);

    let absent = AgentAttachPayload {
        login: None,
        record: None,
        tier: None,
        mode: None,
        isolation: None,
        ..payload
    };
    let wire = serde_json::to_value(&absent).expect("serialize attach");
    assert!(wire.get("isolation").is_none());
    assert!(wire.get("login").is_none());
    let stamped = AgentAttachPayload {
        login: Some(crate::ids::LoginName::default()),
        ..absent.clone()
    };
    assert_eq!(
        serde_json::to_value(&stamped).expect("serialize attach")["login"],
        json!("default")
    );
    assert_eq!(
        serde_json::from_value::<AgentAttachPayload>(wire).expect("decode legacy attach"),
        absent,
    );

    let malformed = EventEnvelope::new(
        workspace(),
        "session",
        "codex",
        "agent",
        "agent.attached",
        json!({"agent_id": "sess-1"}),
    );
    assert!(matches!(malformed.kind(), EventKind::Other { .. }));
}

#[test]
fn session_boundaries_decode_to_typed_kinds() {
    let rebirth = EventEnvelope::session_rebirth(workspace(), "session");
    assert_eq!(rebirth.method, "session.rebirth");
    assert_eq!(rebirth.params_value(), json!({}));
    assert!(matches!(rebirth.kind(), EventKind::SessionRebirth));

    let death_payload = SessionDeathPayload {
        cause: SessionDeathCause::Crash,
        lost_agents: vec![SessionDeathAgent {
            kind: AgentKind::new_unchecked("claude"),
            agent_id: AgentSessionId::from("sess-1"),
            name: Some("amber-atlas".to_owned()),
        }],
    };
    let death = EventEnvelope::session_death(
        workspace(),
        "session",
        death_payload.cause,
        death_payload.lost_agents.clone(),
    );
    assert_eq!(death.method, "session.death");
    assert_eq!(
        death.params_value(),
        json!({
            "cause": "crash",
            "lost_agents": [{
                "kind": "claude", "agent_id": "sess-1", "name": "amber-atlas",
            }],
        })
    );
    let EventKind::SessionDeath(payload) = death.kind() else {
        panic!("session death decodes to its typed kind");
    };
    assert_eq!(payload, death_payload);
}

#[test]
fn message_event_redacts_text_and_preserves_audit_metadata() {
    let message = message_record();
    let reason = "confirmed by lifecycle";
    let event = EventEnvelope::message_event(
        &message,
        "session",
        MessageEventMethod::Delivered,
        Some(reason),
    );
    assert_eq!(
        event.params_value(),
        json!({
            "message_id": "msg_0123456789abcdef", "address": "@reviewer#docs",
            "kind": "claude", "agent_id": "sess-1", "agent_name": "amber-atlas",
            "channel": "docs", "gate": "done", "status": "delivered", "body": "prompt",
            "pane_id": "tmux:%1", "forced": true, "text_len": "secret prompt body".len(),
            "enter": true, "attempts": 2, "unconfirmed_sends": 1,
            "sender": {
                "origin": "agent", "kind": "codex", "name": "swift-otter",
                "profile": "codex-coder", "role": "coder", "channel": "docs",
            },
            "reason": reason, "enqueued_at": message.enqueued_at,
            "delivered_at": message.delivered_at, "compacted_context_tokens": 120_000,
        })
    );
    assert!(
        !event.params.get().contains("secret prompt body"),
        "message text stays out of durable audit events"
    );

    let EventKind::Message { method, payload } = event.kind() else {
        panic!("message event decodes to its typed kind");
    };
    assert_eq!(method, MessageEventMethod::Delivered);
    assert_eq!(
        payload,
        MessageEventPayload::from_record(&message, Some(reason))
    );
}

#[test]
fn unresolved_message_event_preserves_raw_target() {
    let event = EventEnvelope::unresolved_message_event(
        WorkspaceId::parse("ws_000000000000000000000000").expect("fixed workspace id"),
        "session",
        "@reviwer#docs".to_owned(),
        Some("docs".to_owned()),
        MessageSender::Human,
        12,
        "receiver not found".to_owned(),
    );
    let params = event.params_value();
    assert_eq!(params["address"], "@reviwer#docs");
    assert!(params.get("sender").is_none());

    let EventKind::Message { method, payload } = event.kind() else {
        panic!("unresolved message decodes to its typed kind");
    };
    assert_eq!(method, MessageEventMethod::Errored);
    assert_eq!(payload.status, MessageStatus::Errored);
    assert_eq!(payload.address.as_deref(), Some("@reviwer#docs"));
    assert_eq!(payload.kind.as_str(), "unknown");
    assert_eq!(payload.channel.as_deref(), Some("docs"));
    assert_eq!(payload.reason.as_deref(), Some("receiver not found"));
}

#[test]
fn message_method_wire_contract() {
    use MessageEventMethod as Method;
    use MessageStatus as Status;

    let cases = [
        (Method::Queued, "message.queued", None),
        (Method::Edited, "message.edited", None),
        (Method::AfterMet, "message.after_met", None),
        (Method::WhenMet, "message.when_met", None),
        (Method::Sent, "message.sent", None),
        (
            Method::Delivered,
            "message.delivered",
            Some(Status::Delivered),
        ),
        (
            Method::TimedOut,
            "message.timed_out",
            Some(Status::TimedOut),
        ),
        (Method::Errored, "message.errored", Some(Status::Errored)),
        (Method::Canceled, "message.canceled", Some(Status::Canceled)),
        (
            Method::Abandoned,
            "message.abandoned",
            Some(Status::Abandoned),
        ),
        (Method::Archived, "message.archived", Some(Status::Archived)),
        (Method::Expired, "message.expired", Some(Status::Expired)),
    ];

    for (method, wire, terminal_status) in cases {
        assert_eq!(method.as_str(), wire);
        assert_eq!(Method::parse(wire), Some(method));
        if let Some(status) = terminal_status {
            assert_eq!(Method::for_terminal_status(status), Some(method));
        }
    }
    assert_eq!(Method::parse("message.removed"), Some(Method::Canceled));
    for status in [Status::Queued, Status::Claimed, Status::Sent] {
        assert_eq!(Method::for_terminal_status(status), None);
    }
    assert_eq!(Method::parse("message.unknown"), None);
}

#[test]
fn signal_names_pin_the_public_grammar() {
    for valid in ["ci.finished", "deploy_done", "a-b.c2"] {
        assert_eq!(valid.parse::<SignalName>().unwrap().as_str(), valid);
    }
    for invalid in ["", ".ci", "ci.", "CI.finished", "ci finished", "_ci"] {
        assert!(invalid.parse::<SignalName>().is_err(), "{invalid}");
    }
    assert!("agent.idle".parse::<SignalName>().unwrap().is_reserved());
    assert!("wait.task".parse::<SignalName>().unwrap().is_reserved());
    for family in ["agent", "wait", "team", "ci", "pr", "trunk", "worktree"] {
        let name = format!("{family}.done").parse::<SignalName>().unwrap();
        assert_eq!(name.family(), family);
        assert!(name.is_reserved());
    }
    assert!(!"deploy.done".parse::<SignalName>().unwrap().is_reserved());
    for invalid in ["*", "a.*", "a.b.*", "a*"] {
        assert!(invalid.parse::<SignalName>().is_err());
    }
}
