//! Pending `rimz agents stop --when-idle` requests, at most one per session.
//!
//! The CLI arms or withdraws a request and session retirement removes it, each
//! a read-modify-write from its own process, so every writer takes the
//! workspace lock and publishes durably. Readers stay lock-free; an absent or
//! unreadable record reads as no requests.

use serde::{Deserialize, Serialize};

use crate::agents::state::IdleStop;
use crate::disk::atomic;
use crate::disk::lock::{self, WorkspaceLock};
use crate::disk::paths::StatePaths;
use crate::ids::{AgentKind, AgentSessionId};

const IDLE_STOP_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum IdleStopErr {
    #[error(transparent)]
    Lock(#[from] lock::LockErr),
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdleStopRequest {
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    #[serde(flatten)]
    pub stop: IdleStop,
}

#[derive(Serialize, Deserialize)]
struct IdleStopRecord {
    version: u32,
    requests: Vec<IdleStopRequest>,
}

pub fn read(paths: &StatePaths) -> Vec<IdleStopRequest> {
    std::fs::read(&paths.idle_stop_requests)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<IdleStopRecord>(&bytes).ok())
        .filter(|record| record.version == IDLE_STOP_VERSION)
        .map(|record| record.requests)
        .unwrap_or_default()
}

/// Record `request`, replacing the session's earlier one.
pub fn arm(paths: &StatePaths, request: IdleStopRequest) -> Result<(), IdleStopErr> {
    update(
        paths,
        &request.kind.clone(),
        &request.agent_id.clone(),
        Some(request),
    )?;
    Ok(())
}

/// Remove the session's request; `false` when it had none. Session
/// retirement calls this on every end, so a session without a request costs
/// one lock-free read.
pub fn withdraw(
    paths: &StatePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
) -> Result<bool, IdleStopErr> {
    if !read(paths)
        .iter()
        .any(|request| request.kind == *kind && request.agent_id == *agent_id)
    {
        return Ok(false);
    }
    update(paths, kind, agent_id, None)
}

/// Drop the session's request and append `replacement`; returns whether one was dropped.
fn update(
    paths: &StatePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
    replacement: Option<IdleStopRequest>,
) -> Result<bool, IdleStopErr> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    let mut requests = read(paths);
    let before = requests.len();
    requests.retain(|request| request.kind != *kind || request.agent_id != *agent_id);
    let dropped = requests.len() != before;
    if !dropped && replacement.is_none() {
        return Ok(false);
    }
    requests.extend(replacement);
    atomic::write_temp_then_rename(
        &paths.idle_stop_requests,
        &IdleStopRecord {
            version: IDLE_STOP_VERSION,
            requests,
        },
    )?;
    Ok(dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;
    use jiff::Timestamp;

    fn request(id: &str, after_secs: u64) -> IdleStopRequest {
        IdleStopRequest {
            kind: AgentKind::new_unchecked("claude"),
            agent_id: AgentSessionId::from(id),
            stop: IdleStop {
                after_secs,
                requested_at: Timestamp::UNIX_EPOCH,
                requested_by: (after_secs == 180).then(|| "@lead".to_owned()),
            },
        }
    }

    #[test]
    fn a_request_round_trips_is_replaced_and_withdraws() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("paths");
        let kind = AgentKind::new_unchecked("claude");

        assert!(read(&paths).is_empty());
        assert!(!withdraw(&paths, &kind, &"a".into()).expect("withdraw nothing"));
        assert!(!paths.idle_stop_requests.exists());

        arm(&paths, request("a", 180)).expect("arm a");
        arm(&paths, request("b", 60)).expect("arm b");
        assert_eq!(read(&paths), [request("a", 180), request("b", 60)]);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &std::fs::read(&paths.idle_stop_requests).expect("record")
            )
            .expect("json"),
            serde_json::json!({"version": 1, "requests": [
                {"kind": "claude", "agent_id": "a", "after_secs": 180,
                 "requested_at": "1970-01-01T00:00:00Z", "requested_by": "@lead"},
                {"kind": "claude", "agent_id": "b", "after_secs": 60,
                 "requested_at": "1970-01-01T00:00:00Z"},
            ]})
        );

        arm(&paths, request("a", 0)).expect("replace a");
        assert_eq!(read(&paths), [request("b", 60), request("a", 0)]);

        assert!(!withdraw(&paths, &AgentKind::new_unchecked("codex"), &"a".into()).expect("kind"));
        assert!(withdraw(&paths, &kind, &"a".into()).expect("withdraw a"));
        assert_eq!(read(&paths), [request("b", 60)]);
    }

    #[test]
    fn bad_or_unknown_version_reads_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("paths");
        arm(&paths, request("a", 180)).expect("arm");
        assert_eq!(read(&paths).len(), 1);

        std::fs::write(&paths.idle_stop_requests, b"not json").expect("write bad json");
        assert!(read(&paths).is_empty());

        std::fs::write(
            &paths.idle_stop_requests,
            r#"{"version":999,"requests":[]}"#,
        )
        .expect("write unknown version");
        assert!(read(&paths).is_empty());
    }
}
