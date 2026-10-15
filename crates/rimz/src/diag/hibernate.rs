//! Best-effort wake latency, refault samples, and lock-timeout evidence.

use std::path::Path;

use crate::disk::paths::StatePaths;
use crate::ids::{PaneId, WorkspaceId};

#[derive(serde::Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(crate) enum WakeEvent<'a> {
    Wake {
        frozen_at: jiff::Timestamp,
        step: u32,
        anon_at_freeze: u64,
        reclaimed_bytes: u64,
        trigger: &'a str,
        thaw_ms: u64,
        focus_to_thaw_ms: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        dwell: Option<&'a str>,
        error: Option<&'a str>,
    },
    WakeRefault {
        thawed_at: jiff::Timestamp,
        samples: &'a [RefaultSample],
    },
}

pub(crate) fn record(
    paths: &StatePaths,
    pane_id: &PaneId,
    kind: &crate::ids::AgentKind,
    agent_id: &crate::ids::AgentSessionId,
    event: WakeEvent<'_>,
) {
    #[derive(serde::Serialize)]
    struct Record<'a> {
        at: jiff::Timestamp,
        workspace_id: &'a WorkspaceId,
        pane_id: &'a PaneId,
        kind: &'a crate::ids::AgentKind,
        agent_id: &'a crate::ids::AgentSessionId,
        #[serde(flatten)]
        event: WakeEvent<'a>,
    }
    crate::disk::rotating::append(
        &paths.audit_path("hibernate.log.jsonl"),
        crate::disk::retention::ROTATING_LOG_MAX_BYTES,
        &Record {
            at: jiff::Timestamp::now(),
            workspace_id: &paths.workspace_id,
            pane_id,
            kind,
            agent_id,
            event,
        },
    );
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct RefaultSample {
    pub after_ms: u64,
    pub refault_anon: u64,
    pub refault_file: u64,
}

pub(crate) fn wake_lock_timeout(
    paths: &StatePaths,
    pane_id: &PaneId,
    lock_path: &Path,
    withdrawn: usize,
) {
    #[derive(serde::Serialize)]
    #[serde(tag = "event", rename = "wake_lock_timeout")]
    struct Record<'a> {
        at: jiff::Timestamp,
        workspace_id: &'a WorkspaceId,
        pane_id: &'a PaneId,
        lock_path: &'a Path,
        withdrawn: usize,
    }
    crate::disk::rotating::append(
        &paths.audit_path("hibernate.log.jsonl"),
        crate::disk::retention::ROTATING_LOG_MAX_BYTES,
        &Record {
            at: jiff::Timestamp::now(),
            workspace_id: &paths.workspace_id,
            pane_id,
            lock_path,
            withdrawn,
        },
    );
}
