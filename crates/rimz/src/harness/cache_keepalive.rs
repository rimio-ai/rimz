//! Best-effort prompt-cache pings for sleeping agents. Durable delivery belongs to the helper.

use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::agents::{AgentState, AgentStatus, PendingWaitTrigger};
use crate::config::HarnessConfig;
use crate::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
use crate::store::snapshot::SidebarSnapshot;
use crate::utils::time::format_duration_compact;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheKeepaliveRequest {
    pub workspace_id: WorkspaceId,
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub pane_id: PaneId,
    pub anchor: Timestamp,
    pub label: String,
}

impl CacheKeepaliveRequest {
    pub fn target<'a>(
        &self,
        snapshot: &'a SidebarSnapshot,
        config: &HarnessConfig,
        now: Timestamp,
    ) -> Option<&'a AgentState> {
        snapshot.agents.iter().find(|agent| {
            agent.kind == self.kind
                && agent.agent_id == self.agent_id
                && agent.last_request_at() == Some(self.anchor)
                && should_keepalive(agent, config, now)
                && snapshot
                    .live_agent_pane(&self.kind, &self.agent_id)
                    .as_ref()
                    == Some(&self.pane_id)
        })
    }
}

pub fn prompt(agent: &AgentState, now: Timestamp) -> String {
    let mut text = "Prompt cache keepalive. Pending waits:".to_owned();
    for wait in &agent.pending_waits {
        let trigger = &wait.trigger;
        let detail = match trigger {
            PendingWaitTrigger::Command { .. } | PendingWaitTrigger::Check { .. } => {
                trigger.detail().unwrap_or_default()
            }
            _ => trigger.headline(now),
        };
        text.push_str(&format!(
            "\n- {}: {} {detail}",
            wait.name,
            trigger.kind_word()
        ));
        if let Some(armed_at) = wait.armed_at {
            let elapsed = Duration::from_secs(now.duration_since(armed_at).as_secs().max(0) as u64);
            text.push_str(&format!(", {} elapsed", format_duration_compact(elapsed)));
        }
    }
    text
}

fn keepalive_agents_with(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    config: &HarnessConfig,
    mut spawn: impl FnMut(&CacheKeepaliveRequest),
) {
    let Ok(Some(_guard)) =
        crate::disk::lock::WorkspaceLock::try_acquire(&runtime.live_path("cache-keepalive.lock"))
    else {
        return;
    };
    for agent in &snapshot.agents {
        if !should_keepalive(agent, config, snapshot.now) {
            continue;
        }
        let (Some(anchor), Some(pane_id)) = (
            agent.last_request_at(),
            snapshot.live_agent_pane(&agent.kind, &agent.agent_id),
        ) else {
            continue;
        };
        let path = runtime.live_path("cache-keepalive").join(format!(
            "{}.json",
            crate::store::sidecar::digest(agent.kind.as_str(), agent.agent_id.as_str())
        ));
        let fired: Option<Timestamp> = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        if fired.is_some_and(|fired| fired >= anchor) {
            continue;
        }
        // Claim before spawning: every producer shares the lock and the same anchor record.
        if let Err(err) = crate::disk::atomic::write_temp_then_rename_cache(&path, &anchor) {
            tracing::warn!(error = %err, "failed to pace cache keepalive");
            continue;
        }
        spawn(&CacheKeepaliveRequest {
            workspace_id: runtime.workspace_id.clone(),
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            pane_id,
            anchor,
            label: crate::address::agent_handle(
                agent,
                &crate::address::addressable_agents(snapshot),
                false,
            ),
        });
    }
}

pub(crate) fn keepalive_agents(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    config: &HarnessConfig,
) {
    if !config.cache_keepalive {
        return;
    }
    keepalive_agents_with(snapshot, runtime, config, |request| {
        let args = crate::child_process::agent_helper_argv("cache-keepalive", request);
        if let Err(err) =
            crate::child_process::spawn_detached_rimz(runtime, args, "agent-cache-keepalive")
        {
            tracing::debug!(error = %err, "failed to spawn cache keepalive");
        }
    });
}

fn should_keepalive(agent: &AgentState, config: &HarnessConfig, now: Timestamp) -> bool {
    if !config.cache_keepalive
        || agent.effective_status() != AgentStatus::Sleeping
        || agent.is_provider_subagent()
        || agent.agent_id.is_empty()
        || agent.compacting_since.is_some()
        || agent.budget_park.is_some()
        || agent.is_awaiting_input()
    {
        return false;
    }
    let (Some(ttl), Some(anchor)) = (
        config.prompt_cache_ttl(&agent.kind),
        agent.last_request_at(),
    ) else {
        return false;
    };
    let Some(fire_after) = ttl.checked_sub(crate::config::PROMPT_CACHE_MARGIN) else {
        return false;
    };
    let Ok(idle) = Duration::try_from(now.duration_since(anchor)) else {
        return false;
    };
    idle >= fire_after && idle < ttl
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentStatus, PendingWait, PendingWaitTrigger};

    fn ts(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).unwrap()
    }

    fn sleeping() -> AgentState {
        let mut agent = AgentState::stub("claude", "session", AgentStatus::Idle);
        agent.turn_started_at = Some(ts(0));
        agent.turn_ended_at = Some(ts(10));
        agent.pending_waits.push(PendingWait {
            name: "gate".into(),
            trigger: PendingWaitTrigger::Command {
                command: "cargo xtask gate".into(),
            },
            armed_at: Some(ts(0)),
        });
        agent
    }

    #[test]
    fn keepalive_window_and_recurrence_follow_requests_not_wait_arm_time() {
        let mut agent = sleeping();
        let config = HarnessConfig::default();
        assert!(should_keepalive(&agent, &config, ts(3540)));
        assert!(!should_keepalive(&agent, &config, ts(3539)));
        assert!(!should_keepalive(&agent, &config, ts(3600)));
        agent.turn_started_at = Some(ts(3545));
        agent.turn_ended_at = Some(ts(3550));
        assert!(!should_keepalive(&agent, &config, ts(3550)));
        assert!(should_keepalive(&agent, &config, ts(7085)));
    }

    #[test]
    fn keepalive_excludes_non_sleepers_and_disabled_or_unsafe_seats() {
        let agent = sleeping();
        let config = HarnessConfig::default();
        assert!(
            should_keepalive(&agent, &config, ts(3540)),
            "solo agent qualifies"
        );
        let mut changed = agent.clone();
        changed.pending_waits.clear();
        assert!(!should_keepalive(&changed, &config, ts(3540)));
        changed = agent.clone();
        changed.status = AgentStatus::Running;
        assert!(!should_keepalive(&changed, &config, ts(3540)));
        changed = agent.clone();
        changed.compacting_since = Some(ts(3539));
        assert!(!should_keepalive(&changed, &config, ts(3540)));
        changed = agent.clone();
        changed.turn_ended_at = None;
        assert!(!should_keepalive(&changed, &config, ts(3540)));
        changed = agent.clone();
        changed.parent_agent_id = Some("parent".into());
        assert!(!should_keepalive(&changed, &config, ts(3540)));
        changed = agent.clone();
        changed.status = AgentStatus::Waiting;
        changed.waiting_since = Some(changed.last_activity);
        assert!(!should_keepalive(&changed, &config, ts(3540)));
        changed = agent.clone();
        changed.kind = AgentKind::new_unchecked("amp");
        assert!(!should_keepalive(&changed, &config, ts(3540)));
        changed = agent.clone();
        changed.budget_park = Some(crate::agents::BudgetPark {
            cap_usd: 1.0,
            spend_usd: 1.0,
            window: crate::agents::BudgetWindow::Session,
            at: ts(0),
            scope: crate::agents::BudgetScope::Agent,
            account_kind: None,
            resets_at: None,
        });
        assert!(!should_keepalive(&changed, &config, ts(3540)));
        changed = agent.clone();
        changed.pending_waits[0].trigger = PendingWaitTrigger::Timer {
            due: ts(7200),
            delay: Some("2h".into()),
        };
        assert!(should_keepalive(&changed, &config, ts(3540)));
        let mut disabled = config;
        disabled.cache_keepalive = false;
        assert!(!should_keepalive(&agent, &disabled, ts(3540)));
    }

    #[test]
    fn keepalive_prompt_is_neutral_and_includes_each_wait() {
        let mut agent = sleeping();
        agent.pending_waits.push(PendingWait {
            name: "ci".into(),
            trigger: PendingWaitTrigger::Signal {
                selector: "pr.checks".into(),
            },
            armed_at: None,
        });
        assert_eq!(
            prompt(&agent, ts(3480)),
            "Prompt cache keepalive. Pending waits:\n- gate: command cargo xtask gate, 58m elapsed\n- ci: signal pr.checks"
        );
    }

    #[test]
    fn keepalive_producers_share_pacing_even_during_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let agent = sleeping();
        let mut snapshot =
            SidebarSnapshot::build_with_agents(workspace_id, vec![agent.clone()], ts(3540));
        let config = HarnessConfig::default();
        keepalive_agents_with(&snapshot, &runtime, &config, |_| panic!("no live pane"));
        snapshot
            .agent_panes
            .push(crate::store::snapshot::PaneAgent {
                kind: agent.kind.clone(),
                kind_ordinal: None,
                name: None,
                name_explicit: false,
                profile: None,
                role: None,
                channel: None,
                agent_id: Some(agent.agent_id.clone()),
                pane_id: PaneId::parse("tmux:%1").unwrap(),
                pane_pid: None,
                worktree_path: None,
                worktree_branch: None,
            });
        let mut count = 0;
        keepalive_agents_with(&snapshot, &runtime, &config, |request| {
            assert_eq!(request.anchor, ts(0));
            assert!(request.target(&snapshot, &config, ts(3540)).is_some());
            let mut changed = snapshot.clone();
            changed.agents[0].last_tool_at = Some(ts(1));
            assert!(
                request.target(&changed, &config, ts(3541)).is_none(),
                "moved anchor"
            );
            changed = snapshot.clone();
            changed.agents[0].pending_waits.clear();
            assert!(
                request.target(&changed, &config, ts(3540)).is_none(),
                "wait completed"
            );
            changed = snapshot.clone();
            changed.agent_panes.clear();
            assert!(
                request.target(&changed, &config, ts(3540)).is_none(),
                "pane vanished"
            );
            count += 1;
            keepalive_agents_with(&snapshot, &runtime, &config, |_| panic!("concurrent spawn"));
        });
        assert_eq!(count, 1);
        keepalive_agents_with(&snapshot, &runtime, &config, |_| panic!("repeated anchor"));
        let stale = snapshot.clone();
        snapshot.agents[0].turn_started_at = Some(ts(3545));
        snapshot.agents[0].turn_ended_at = Some(ts(3550));
        snapshot.now = ts(7085);
        keepalive_agents_with(&snapshot, &runtime, &config, |_| {
            count += 1;
        });
        assert_eq!(count, 2);
        keepalive_agents_with(&stale, &runtime, &config, |_| {
            panic!("stale producer respawned an older anchor")
        });
    }
}
