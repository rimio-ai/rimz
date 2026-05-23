//! Reduced workspace snapshot. The sidebar consumes this via
//! `rimz sidebar snapshot --json`; correctness lives in the feed files and
//! event log this is derived from.

use std::collections::BTreeMap;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::feed::{AgentMode, AgentState, AgentStatus, FeedItem, FeedStatus, Surface};
use crate::ids::WorkspaceId;
use crate::ledger::atomic::{self, write_temp_then_rename};
use crate::ledger::event_log::{self, EventLogErr};
use crate::ledger::feed_store::{self, FeedStoreErr};
use crate::ledger::paths::StatePaths;
use crate::schema::event::EventEnvelope;

#[derive(Debug, thiserror::Error)]
pub enum SnapshotErr {
    #[error(transparent)]
    FeedStore(#[from] FeedStoreErr),
    #[error(transparent)]
    EventLog(#[from] EventLogErr),
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
}

pub type Result<T> = std::result::Result<T, SnapshotErr>;

/// Sidebar groups: needs attention, resolver chain working, recently
/// answered, recent activity. The agent rollup folds `agent.lifecycle`
/// events into the latest status + mode per agent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SidebarSnapshot {
    pub workspace_id: WorkspaceId,
    pub generated_at: Timestamp,
    pub needs_attention: Vec<FeedItem>,
    pub resolver_working: Vec<FeedItem>,
    pub recently_answered: Vec<FeedItem>,
    pub recent_activity: Vec<SidebarActivity>,
    pub agents: Vec<AgentState>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SidebarActivity {
    Feed { item: Box<FeedItem> },
    Event { event: Box<EventEnvelope> },
}

impl SidebarActivity {
    pub fn timestamp(&self) -> Timestamp {
        match self {
            Self::Feed { item } => item.updated_at,
            Self::Event { event } => event.timestamp,
        }
    }
}

impl SidebarSnapshot {
    pub fn build(
        workspace_id: WorkspaceId,
        mut items: Vec<FeedItem>,
        events: Vec<EventEnvelope>,
    ) -> Self {
        items.sort_by_key(|item| std::cmp::Reverse(item.updated_at));

        let agents = reduce_agent_states(&events);

        let mut needs_attention = Vec::new();
        let mut resolver_working = Vec::new();
        let mut recently_answered = Vec::new();
        let mut recent_activity = Vec::new();

        for item in items {
            match (item.status, item.surface) {
                (FeedStatus::Pending, Surface::NativeUi | Surface::Script) => {
                    needs_attention.push(item);
                }
                (FeedStatus::Pending, Surface::Bridge) => resolver_working.push(item),
                (FeedStatus::Resolved, _) => recently_answered.push(item),
                _ => recent_activity.push(SidebarActivity::Feed {
                    item: Box::new(item),
                }),
            }
        }

        recent_activity.extend(
            events
                .into_iter()
                .filter(|event| !event.method.starts_with("feed."))
                .map(|event| SidebarActivity::Event {
                    event: Box::new(event),
                }),
        );
        recent_activity.sort_by_key(|activity| std::cmp::Reverse(activity.timestamp()));

        Self {
            workspace_id,
            generated_at: Timestamp::now(),
            needs_attention,
            resolver_working,
            recently_answered,
            recent_activity,
            agents,
        }
    }
}

/// Fold `agent.lifecycle` events into the latest [`AgentState`] per
/// agent_id, keyed by `(agent_kind, agent_id)`. Anonymous lifecycle events
/// (no agent_id) collapse to a single rollup keyed by `agent_kind`. Events
/// are walked in log order, so the newest observation wins.
fn reduce_agent_states(events: &[EventEnvelope]) -> Vec<AgentState> {
    let mut map: BTreeMap<(String, String), AgentState> = BTreeMap::new();
    for event in events {
        if event.method != "agent.lifecycle" {
            continue;
        }
        let kind = event.source.clone();
        let agent_id = event
            .params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("{kind}:anonymous"));
        let status: AgentStatus = event
            .params
            .get("status")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(AgentStatus::Idle);
        let mode: AgentMode = event
            .params
            .get("mode")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(AgentMode::Unknown);
        let worktree_branch = event
            .params
            .get("worktree_branch")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned);
        let state = AgentState {
            agent_id: agent_id.clone(),
            kind: kind.clone(),
            status,
            mode,
            pane: None,
            worktree_path: None,
            worktree_branch,
            last_seen: event.timestamp,
        };
        map.insert((kind, agent_id), state);
    }
    map.into_values().collect()
}

/// Rebuild the snapshot from the feed dir and persist it atomically. The
/// resulting JSON is what `rimz sidebar snapshot --json` reads on attach.
///
/// Cost is O(events + items) per call because the reducer walks the entire
/// event log and feed dir. Acceptable today because both are bounded by
/// per-project session length; event-log rotation and incremental snapshot
/// maintenance are paired work in M4.
pub fn rebuild(paths: &StatePaths) -> Result<SidebarSnapshot> {
    let snapshot = build_from(paths)?;
    write_temp_then_rename(&paths.latest_snapshot, &snapshot)?;
    Ok(snapshot)
}

pub fn build_from(paths: &StatePaths) -> Result<SidebarSnapshot> {
    let items = feed_store::list(&paths.feed_dir)?;
    let events = event_log::read_all(&paths.events_log)?;
    Ok(SidebarSnapshot::build(
        paths.workspace_id.clone(),
        items,
        events,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::FeedKind;
    use crate::ids::WorkspaceId;
    use std::path::Path;

    #[test]
    fn build_groups_by_surface_and_status() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut native = FeedItem::new(
            workspace.clone(),
            Surface::NativeUi,
            FeedKind::Permission,
            "n",
            "rimz",
            "cli",
        );
        let bridge = FeedItem::new(
            workspace.clone(),
            Surface::Bridge,
            FeedKind::Permission,
            "b",
            "rimz",
            "cli",
        );
        let mut answered = FeedItem::new(
            workspace.clone(),
            Surface::Bridge,
            FeedKind::Permission,
            "a",
            "rimz",
            "cli",
        );
        answered.status = FeedStatus::Resolved;
        let mut timed = FeedItem::new(
            workspace,
            Surface::Bridge,
            FeedKind::Permission,
            "t",
            "rimz",
            "cli",
        );
        timed.status = FeedStatus::TimedOut;
        native.updated_at += std::time::Duration::from_secs(1);

        let snap = SidebarSnapshot::build(
            WorkspaceId::from_project_root(Path::new("/tmp/x")),
            vec![native, bridge, answered, timed],
            Vec::new(),
        );
        assert_eq!(snap.needs_attention.len(), 1);
        assert_eq!(snap.resolver_working.len(), 1);
        assert_eq!(snap.recently_answered.len(), 1);
        assert_eq!(snap.recent_activity.len(), 1);
    }

    #[test]
    fn build_includes_non_feed_events_in_recent_activity() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let event = EventEnvelope::new(
            workspace.clone(),
            "session",
            "rimz",
            "cli",
            "event.emit",
            serde_json::json!({ "kind": "build.started", "title": "Building web" }),
        );

        let snap = SidebarSnapshot::build(workspace, Vec::new(), vec![event]);

        assert_eq!(snap.recent_activity.len(), 1);
        assert!(matches!(
            snap.recent_activity[0],
            SidebarActivity::Event { .. }
        ));
    }
}
