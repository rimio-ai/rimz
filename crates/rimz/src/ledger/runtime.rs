//! Runtime projection for durable ledger records.
//!
//! Expel is read-time filtering: default live views keep only records whose
//! recorded owner process is still the same live process. Audit views bypass
//! this filter and read durable history as written.

use crate::agents::AgentState;
use crate::feed::{FeedItem, Surface};
use crate::ids::{AgentKind, AgentSessionId};
use crate::pane::{RuntimeOwner, RuntimeOwnerKind};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeScope {
    Runtime,
    Audit,
}

/// Tri-state process liveness for an agent session record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentLiveness {
    Live { pid: u32 },
    Dead,
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RuntimeProjection {
    pub items: Vec<FeedItem>,
    pub ended: BTreeSet<(AgentKind, AgentSessionId)>,
    pub lost: BTreeSet<(AgentKind, AgentSessionId)>,
    pub agents: Vec<AgentState>,
}

impl RuntimeProjection {
    pub fn from_parts(
        items: Vec<FeedItem>,
        ended: BTreeSet<(AgentKind, AgentSessionId)>,
        lost: BTreeSet<(AgentKind, AgentSessionId)>,
        agents: Vec<AgentState>,
        scope: RuntimeScope,
    ) -> Self {
        match scope {
            RuntimeScope::Audit => Self {
                items,
                ended,
                lost,
                agents,
            },
            RuntimeScope::Runtime => Self {
                items: items.into_iter().filter(item_is_runtime_visible).collect(),
                ended,
                lost,
                agents: agents
                    .into_iter()
                    .filter(agent_is_runtime_visible)
                    .collect(),
            },
        }
    }
}

/// Runtime visibility for a feed item. The owner-required liveness gate is a
/// script concern: a script that exits must not strand its prompt as attention.
/// Agent and bridge asks are governed by the agent rollup join in the snapshot
/// reducer (`agent_hook_session_stale`), so a missing owner there is not by
/// itself a reason to hide — only a *known-dead* owner suppresses them.
fn item_is_runtime_visible(item: &FeedItem) -> bool {
    if item.surface == Surface::Script {
        return item.runtime_owner.as_ref().is_some_and(owner_is_live);
    }
    item.runtime_owner.as_ref().is_none_or(owner_is_live)
}

/// Runtime visibility for an agent. Liveness suppresses; it never gates an
/// agent in. An unknown pid abstains (foreground/pane corroboration carries
/// liveness — see `docs/internals/agents/agent.md`); a known owner that is known-dead
/// suppresses the stale overlay.
fn agent_is_runtime_visible(agent: &AgentState) -> bool {
    agent.runtime_owner.as_ref().is_none_or(owner_is_live)
}

pub fn current_process_owner(
    kind: RuntimeOwnerKind,
    subject_id: impl Into<String>,
) -> RuntimeOwner {
    let pid = std::process::id();
    RuntimeOwner::new(kind, subject_id, pid, process_start_token(pid))
}

pub fn process_owner(
    kind: RuntimeOwnerKind,
    subject_id: impl Into<String>,
    pid: u32,
) -> RuntimeOwner {
    RuntimeOwner::new(kind, subject_id, pid, process_start_token(pid))
}

pub fn owner_is_live(owner: &RuntimeOwner) -> bool {
    process_is_live(owner.pid, owner.process_start.as_deref())
}

pub fn agent_liveness(agent: &AgentState) -> AgentLiveness {
    if let Some(owner) = &agent.runtime_owner {
        return if owner_is_live(owner) {
            AgentLiveness::Live { pid: owner.pid }
        } else {
            AgentLiveness::Dead
        };
    }
    let Some(pid) = agent.agent_pid else {
        return AgentLiveness::Unknown;
    };
    if process_is_live(pid, agent.agent_process_start.as_deref()) {
        AgentLiveness::Live { pid }
    } else {
        AgentLiveness::Dead
    }
}

#[cfg(target_os = "linux")]
pub fn process_start_token(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    linux_process_start_from_stat(&stat).map(ToOwned::to_owned)
}

#[cfg(not(target_os = "linux"))]
pub fn process_start_token(_pid: u32) -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
fn process_is_live(pid: u32, expected_start: Option<&str>) -> bool {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(_) => return false,
    };
    match expected_start {
        Some(expected) => linux_process_start_from_stat(&stat) == Some(expected),
        None => true,
    }
}

#[cfg(not(target_os = "linux"))]
fn process_is_live(_pid: u32, _expected_start: Option<&str>) -> bool {
    true
}

#[cfg(target_os = "linux")]
fn linux_process_start_from_stat(stat: &str) -> Option<&str> {
    let after_comm = stat.rsplit_once(") ")?.1;
    after_comm.split_whitespace().nth(19)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentStatus;
    use crate::agents::TurnPhase;
    use crate::feed::{FeedKind, Surface};
    use crate::ids::WorkspaceId;
    use jiff::Timestamp;
    use std::path::Path;

    fn agent(owner: Option<RuntimeOwner>) -> AgentState {
        AgentState {
            agent_id: "sess-1".into(),
            kind: crate::ids::AgentKind::new_unchecked("claude"),
            name: None,
            kind_ordinal: None,
            profile: None,
            role: None,
            team: None,
            launch_group: None,
            launch_ordinal: None,
            channel: None,
            status: AgentStatus::Idle,
            phase: TurnPhase::Idle,
            pane: None,
            agent_pid: owner.as_ref().map(|owner| owner.pid),
            agent_process_start: owner.as_ref().and_then(|owner| owner.process_start.clone()),
            runtime_owner: owner,
            parent_agent_id: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            prompt: None,
            description: None,
            transcript_path: None,
            origin: None,
            recent_prompts: Vec::new(),
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            compacting_since: None,
            compaction_count: 0,
            last_compact_command_tokens: None,
            last_seen: Timestamp::UNIX_EPOCH,
            last_activity: Timestamp::UNIX_EPOCH,
            registered_at: Some(Timestamp::UNIX_EPOCH),
        }
    }

    #[test]
    fn runtime_projection_filters_items_by_owner_and_scope() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let item = |surface, owner| {
            let mut item = FeedItem::new(
                workspace.clone(),
                surface,
                FeedKind::Question,
                "deploy?",
                "rimz",
                "cli",
            );
            item.runtime_owner = owner;
            item
        };

        let runtime = RuntimeProjection::from_parts(
            vec![
                item(
                    Surface::Script,
                    Some(current_process_owner(RuntimeOwnerKind::Script, "live")),
                ),
                item(Surface::Script, None),
                item(Surface::Bridge, None),
                #[cfg(target_os = "linux")]
                item(
                    Surface::Script,
                    Some(RuntimeOwner::new(
                        RuntimeOwnerKind::Script,
                        "dead",
                        u32::MAX,
                        None,
                    )),
                ),
                #[cfg(target_os = "linux")]
                item(
                    Surface::Script,
                    Some(RuntimeOwner::new(
                        RuntimeOwnerKind::Script,
                        "reused",
                        std::process::id(),
                        Some("definitely-not-this-process".to_owned()),
                    )),
                ),
            ],
            BTreeSet::new(),
            BTreeSet::new(),
            Vec::new(),
            RuntimeScope::Runtime,
        );
        assert_eq!(
            runtime.items.len(),
            2,
            "live script and ownerless bridge stay; ownerless/dead scripts drop"
        );

        let audit = RuntimeProjection::from_parts(
            vec![item(Surface::Script, None)],
            BTreeSet::new(),
            BTreeSet::new(),
            Vec::new(),
            RuntimeScope::Audit,
        );
        assert_eq!(audit.items.len(), 1, "audit keeps durable history");
    }

    #[test]
    fn runtime_projection_keeps_unknown_agents_and_drops_known_dead_ones() {
        let mut agents = vec![agent(None)];
        #[cfg(target_os = "linux")]
        agents.push(agent(Some(RuntimeOwner::new(
            RuntimeOwnerKind::Agent,
            "sess-dead",
            u32::MAX,
            None,
        ))));

        let projection = RuntimeProjection::from_parts(
            Vec::new(),
            BTreeSet::new(),
            BTreeSet::new(),
            agents,
            RuntimeScope::Runtime,
        );

        assert_eq!(
            projection.agents.len(),
            1,
            "unknown pid abstains while known-dead owners suppress stale overlays"
        );
    }

    #[test]
    fn agent_liveness_reports_live_runtime_owner() {
        let owner = current_process_owner(RuntimeOwnerKind::Agent, "sess-live");
        assert_eq!(
            agent_liveness(&agent(Some(owner))),
            AgentLiveness::Live {
                pid: std::process::id()
            }
        );
    }

    #[test]
    fn agent_liveness_reports_unknown_without_process_identity() {
        assert_eq!(agent_liveness(&agent(None)), AgentLiveness::Unknown);
    }

    #[test]
    fn agent_liveness_checks_agent_pid_without_runtime_owner() {
        let mut state = agent(None);
        state.agent_pid = Some(std::process::id());
        state.agent_process_start = process_start_token(std::process::id());

        assert_eq!(
            agent_liveness(&state),
            AgentLiveness::Live {
                pid: std::process::id()
            }
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn agent_liveness_reports_dead_for_missing_or_wrong_process() {
        let mut state = agent(None);
        state.agent_pid = Some(u32::MAX);
        assert_eq!(agent_liveness(&state), AgentLiveness::Dead);

        state.agent_pid = Some(std::process::id());
        state.agent_process_start = Some("definitely-not-this-process".to_owned());
        assert_eq!(agent_liveness(&state), AgentLiveness::Dead);
    }
}
