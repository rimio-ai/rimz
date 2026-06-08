//! Event envelope appended to `events.log.jsonl`.

use jiff::Timestamp;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agents::AgentLifecycleObservation;
use crate::feed::FeedItem;
use crate::feed::RuntimeOwner;
use crate::ids::{AgentSessionId, EventId, MuxName, PaneId, WorkspaceId};
use crate::schema::EVENT_SCHEMA_VERSION;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AgentLifecyclePayload {
    #[serde(default)]
    pub event_name: Option<String>,
    #[serde(flatten)]
    pub observation: AgentLifecycleObservation,
}

impl AgentLifecyclePayload {
    pub fn new(event_name: impl Into<String>, observation: &AgentLifecycleObservation) -> Self {
        Self {
            event_name: Some(event_name.into()),
            observation: observation.clone(),
        }
    }

    pub fn from_params(params: &Value) -> Option<Self> {
        let signal = params
            .get("signal")
            .and_then(|value| serde_json::from_value(value.clone()).ok())?;
        Some(Self {
            event_name: optional_string(params, "event_name"),
            observation: AgentLifecycleObservation {
                agent_id: optional_string(params, "agent_id").map(AgentSessionId::from),
                signal,
                agent_pid: optional_deserialize(params, "agent_pid"),
                agent_process_start: optional_string(params, "agent_process_start"),
                runtime_owner: optional_deserialize::<RuntimeOwner>(params, "runtime_owner"),
                worktree_path: optional_string(params, "worktree_path"),
                worktree_branch: optional_string(params, "worktree_branch"),
                task: optional_string(params, "task"),
                prompt: optional_string(params, "prompt"),
                transcript_path: optional_string(params, "transcript_path"),
                model: optional_string(params, "model"),
                effort: optional_string(params, "effort"),
                context_pct: optional_u64(params, "context_pct").map(|v| v.min(100) as u8),
                context_window: optional_u64(params, "context_window"),
                total_tokens: optional_u64(params, "total_tokens"),
                cache_read_input_tokens: optional_u64(params, "cache_read_input_tokens"),
                fresh_input_tokens: optional_u64(params, "fresh_input_tokens"),
                output_tokens: optional_u64(params, "output_tokens"),
                todo_done: optional_u64(params, "todo_done").map(clamp_u32),
                todo_total: optional_u64(params, "todo_total").map(clamp_u32),
                pane_id: optional_string(params, "pane_id")
                    .and_then(|raw| PaneId::parse(&raw).ok()),
                parent_agent_id: optional_string(params, "parent_agent_id")
                    .map(AgentSessionId::from),
            },
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum EventKind<'a> {
    AgentLifecycle(Box<AgentLifecyclePayload>),
    SessionRebirth,
    /// Deliberate carrier for audit/user events that have not graduated to a
    /// folded typed variant yet, including `feed.*`, `event.emit`, and unknown
    /// methods from older or newer binaries.
    Other {
        method: &'a str,
        params: &'a Value,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub schema_version: String,
    pub event_id: EventId,
    pub workspace_id: WorkspaceId,
    pub session_name: String,
    pub mux: Option<MuxName>,
    pub source: String,
    pub source_kind: String,
    pub method: String,
    pub timestamp: Timestamp,
    pub params: Value,
}

impl EventEnvelope {
    pub fn new(
        workspace_id: WorkspaceId,
        session_name: impl Into<String>,
        source: impl Into<String>,
        source_kind: impl Into<String>,
        method: impl Into<String>,
        params: Value,
    ) -> Self {
        Self {
            schema_version: EVENT_SCHEMA_VERSION.to_owned(),
            event_id: EventId::new(),
            workspace_id,
            session_name: session_name.into(),
            mux: None,
            source: source.into(),
            source_kind: source_kind.into(),
            method: method.into(),
            timestamp: Timestamp::now(),
            params,
        }
    }

    /// Constructor for the `session.rebirth` boundary a genuine mux-session
    /// birth appends. A reborn session renumbers panes from zero, so every
    /// pane stamp recorded before this instant names a pane that no longer
    /// exists — the agent rollup fold clears them all at this point in the
    /// log ([`crate::ledger::snapshot`]'s reducer), keeping a prior
    /// incarnation's session off a reused pane id. The sessions themselves
    /// stay: the boundary unstamps, it never tombstones.
    pub fn session_rebirth(workspace_id: WorkspaceId, session_name: impl Into<String>) -> Self {
        Self::new(
            workspace_id,
            session_name,
            "rimz",
            "runtime",
            "session.rebirth",
            json!({}),
        )
    }

    pub fn kind(&self) -> EventKind<'_> {
        match self.method.as_str() {
            "agent.lifecycle" => AgentLifecyclePayload::from_params(&self.params)
                .map(Box::new)
                .map(EventKind::AgentLifecycle)
                .unwrap_or(EventKind::Other {
                    method: self.method.as_str(),
                    params: &self.params,
                }),
            "session.rebirth" => EventKind::SessionRebirth,
            _ => EventKind::Other {
                method: self.method.as_str(),
                params: &self.params,
            },
        }
    }

    /// Convenience constructor for a `feed.push` event from a `FeedItem`.
    pub fn feed_pushed(item: &FeedItem, session_name: impl Into<String>) -> Self {
        Self::new(
            item.workspace_id.clone(),
            session_name,
            item.source.clone(),
            item.source_kind.clone(),
            "feed.push",
            json!({
                "request_id": item.request_id,
                "surface": item.surface,
                "kind": item.kind,
            }),
        )
    }

    /// Convenience constructor for an `agent.lifecycle` event. The CLI hook
    /// path calls this after a lifecycle hook fires, so the sidebar's agent
    /// rollup sees the status and enrichment update without each adapter
    /// touching the ledger.
    pub fn agent_lifecycle(
        workspace_id: WorkspaceId,
        session_name: impl Into<String>,
        agent_kind: impl Into<String>,
        event_name: impl Into<String>,
        observation: &AgentLifecycleObservation,
    ) -> Self {
        let agent_kind = agent_kind.into();
        let params = serde_json::to_value(AgentLifecyclePayload::new(event_name, observation))
            .expect("AgentLifecyclePayload contains only JSON-serializable fields");
        Self::new(
            workspace_id,
            session_name,
            agent_kind.clone(),
            "agent-hook",
            "agent.lifecycle",
            params,
        )
    }
}

fn optional_string(params: &Value, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn optional_u64(params: &Value, key: &str) -> Option<u64> {
    params.get(key).and_then(Value::as_u64)
}

fn optional_deserialize<T: DeserializeOwned>(params: &Value, key: &str) -> Option<T> {
    params
        .get(key)
        .and_then(|value| serde_json::from_value(value.clone()).ok())
}

fn clamp_u32(value: u64) -> u32 {
    value.min(u32::MAX as u64) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::agents::lifecycle::LifecycleSignal;
    use crate::feed::{RuntimeOwner, RuntimeOwnerKind};
    use crate::ids::{AgentSessionId, MuxName, PaneId};

    fn workspace() -> WorkspaceId {
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-event-test"))
    }

    fn lifecycle_observation() -> AgentLifecycleObservation {
        AgentLifecycleObservation {
            agent_id: Some(AgentSessionId::from("sess-1")),
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
            model: Some("claude-opus".to_owned()),
            effort: Some("high".to_owned()),
            context_pct: Some(80),
            context_window: Some(200_000),
            total_tokens: Some(10_000),
            cache_read_input_tokens: Some(7_000),
            fresh_input_tokens: Some(2_000),
            output_tokens: Some(1_000),
            todo_done: Some(3),
            todo_total: Some(5),
            pane_id: Some(PaneId::from_parts(MuxName::Tmux, "%1")),
            parent_agent_id: Some(AgentSessionId::from("parent-1")),
        }
    }

    #[test]
    fn agent_lifecycle_constructor_keeps_the_existing_wire_shape() {
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
                "fresh_input_tokens": 2_000,
                "output_tokens": 1_000,
                "todo_done": 3,
                "todo_total": 5,
                "pane_id": "tmux:%1",
                "parent_agent_id": "parent-1",
            }),
        );
        legacy.event_id = typed.event_id.clone();
        legacy.timestamp = typed.timestamp;

        assert_eq!(
            serde_json::to_vec(&typed).unwrap(),
            serde_json::to_vec(&legacy).unwrap(),
            "typed construction must not migrate event-log bytes"
        );
        let EventKind::AgentLifecycle(payload) = typed.kind() else {
            panic!("agent.lifecycle decodes to its typed kind");
        };
        let payload = *payload;
        assert_eq!(payload.event_name.as_deref(), Some("Stop"));
        assert_eq!(payload.observation, observation);
    }

    #[test]
    fn agent_lifecycle_constructor_serializes_absent_fields_as_null() {
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
            "agent_process_start",
            "runtime_owner",
            "worktree_path",
            "worktree_branch",
            "task",
            "prompt",
            "transcript_path",
            "model",
            "effort",
            "context_pct",
            "context_window",
            "total_tokens",
            "cache_read_input_tokens",
            "fresh_input_tokens",
            "output_tokens",
            "todo_done",
            "todo_total",
            "pane_id",
            "parent_agent_id",
        ] {
            assert_eq!(
                event.params.get(key),
                Some(&Value::Null),
                "{key} must stay present as null to preserve partial-event bytes",
            );
        }
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
        assert_eq!(raw, &params);
    }

    #[test]
    fn lifecycle_kind_tolerates_missing_identity_and_bad_optional_fields() {
        let event = EventEnvelope::new(
            workspace(),
            "session",
            "claude",
            "agent-hook",
            "agent.lifecycle",
            json!({
                "event_name": "SessionStart",
                "signal": { "signal": "registered" },
                "agent_pid": "not-a-pid",
                "context_pct": 300,
                "todo_done": u64::MAX,
                "pane_id": "not-a-pane",
            }),
        );

        let EventKind::AgentLifecycle(payload) = event.kind() else {
            panic!("partial lifecycle payload with a signal is still typed");
        };
        let payload = *payload;
        assert_eq!(payload.observation.agent_id, None);
        assert_eq!(payload.observation.signal, LifecycleSignal::Registered);
        assert_eq!(payload.observation.agent_pid, None);
        assert_eq!(payload.observation.context_pct, Some(100));
        assert_eq!(payload.observation.todo_done, Some(u32::MAX));
        assert_eq!(payload.observation.pane_id, None);
    }

    #[test]
    fn signal_less_lifecycle_event_decodes_as_other() {
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
}
