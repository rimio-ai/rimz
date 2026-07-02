//! Durable notification trace log and record schema.
//!
//! A tab-bell notification with no matching unread card is invisible after the
//! fact, so the producer's emitted notifications, each renderer's bell decision,
//! and the unread mark/clear transitions append compact JSONL records under the
//! workspace state directory. The log is diagnostic state: append-only within a
//! size cap, never read by correctness code. Records are written through
//! [`super::DiagSink`], which already carries the workspace identity to
//! every emission site.

use std::path::Path;

use super::JsonlLog;
use serde::{Deserialize, Serialize};

use crate::ids::{AgentKind, AgentSessionId, PaneId, SidebarInstanceId, WorkspaceId};

pub const NOTIFY_TRACE_SCHEMA_VERSION: &str = "rimz.notify_trace.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyTraceEnvelope {
    pub v: String,
    /// Build id of the writing process, so overlapping old/new builds stay
    /// distinguishable in the evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    pub workspace_id: WorkspaceId,
    pub session_name: String,
    /// The sidebar renderer that wrote the record; absent on producer-side rows
    /// emitted outside a renderer instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<SidebarInstanceId>,
    pub at_ms: u64,
    pub event: NotifyTraceEvent,
}

impl NotifyTraceEnvelope {
    pub fn new(
        workspace_id: WorkspaceId,
        session_name: String,
        instance_id: Option<SidebarInstanceId>,
        at_ms: u64,
        event: NotifyTraceEvent,
    ) -> Self {
        Self {
            v: NOTIFY_TRACE_SCHEMA_VERSION.to_owned(),
            build: crate::build_id::current().map(str::to_owned),
            workspace_id,
            session_name,
            instance_id,
            at_ms,
            event,
        }
    }

    pub fn is_current_version(&self) -> bool {
        self.v == NOTIFY_TRACE_SCHEMA_VERSION
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NotifyTraceEvent {
    /// The producer flushed a notification for delivery (desktop/command/bell
    /// broadcast). The triggering status edge rides each agent.
    NotificationEmitted {
        /// `waiting` | `failed` | `paused` | `success` | `reminder`.
        notification_kind: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        agents: Vec<TraceAgent>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        panes: Vec<PaneId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unread_count: Option<usize>,
    },
    /// A renderer reached a tab-bell decision for a notification. `fired` is the
    /// sticky Zellij tab marker actually rung; `suppressed` says why it was not.
    BellRing {
        notification_kind: String,
        fired: bool,
        recheck_unread: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        panes: Vec<PaneId>,
        /// `no_own_view` | `daemon_view` | `pane_not_in_view` | `not_unread`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        suppressed: Option<String>,
    },
    /// A row reached a pending-look status on its own.
    UnreadMarked {
        row_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_kind: Option<AgentKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<AgentSessionId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pane_id: Option<PaneId>,
        /// `waiting` | `failed` | `paused` | `success`.
        status: String,
        episode_ms: i64,
    },
    /// A pending look was cleared. Under sticky semantics the only causes are a
    /// human look (`focus` / `tab_view` / `mark_read`) or the row disappearing
    /// (`row_gone`).
    UnreadCleared {
        row_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_kind: Option<AgentKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<AgentSessionId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pane_id: Option<PaneId>,
        /// `focus` | `tab_view` | `mark_read` | `row_gone`.
        cause: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cleared_at_ms: Option<i64>,
    },
}

/// An agent named by an emitted notification, with the reached status where the
/// notification is per-agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceAgent {
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<PaneId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_status: Option<String>,
}

const NOTIFY_LOG_NAME: &str = "notify.log.jsonl";
const NOTIFY_LOG_MAX_BYTES: u64 = 1_048_576;

pub fn log(state_root: &Path) -> JsonlLog {
    JsonlLog::new(state_root.join(NOTIFY_LOG_NAME), NOTIFY_LOG_MAX_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_writes_jsonl_record() {
        let dir = tempfile::tempdir().unwrap();
        let log = log(dir.path());
        log.append(&serde_json::json!({ "event": "bell_ring" }));

        let bytes = std::fs::read_to_string(log.path()).unwrap();
        assert_eq!(bytes, "{\"event\":\"bell_ring\"}\n");
    }

    #[test]
    fn unread_cleared_round_trips() {
        let event = NotifyTraceEvent::UnreadCleared {
            row_id: "claude-1".to_owned(),
            label: Some("api".to_owned()),
            agent_kind: Some(AgentKind::new_unchecked("claude")),
            agent_id: Some(AgentSessionId::from("claude-1")),
            worktree: Some("main".to_owned()),
            pane_id: Some(PaneId::from_parts(crate::ids::MuxName::Tmux, "%1")),
            cause: "tab_view".to_owned(),
            cleared_at_ms: Some(42),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"unread_cleared","row_id":"claude-1","label":"api","agent_kind":"claude","agent_id":"claude-1","worktree":"main","pane_id":"tmux:%1","cause":"tab_view","cleared_at_ms":42}"#
        );
        let back: NotifyTraceEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn bell_ring_omits_absent_suppressed() {
        let event = NotifyTraceEvent::BellRing {
            notification_kind: "success".to_owned(),
            fired: true,
            recheck_unread: true,
            panes: Vec::new(),
            suppressed: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"bell_ring","notification_kind":"success","fired":true,"recheck_unread":true}"#
        );
    }
}
