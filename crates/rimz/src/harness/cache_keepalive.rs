//! Best-effort prompt-cache pings for sleeping agents. Durable delivery belongs to the helper.

use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::agents::{AgentState, AgentStatus, PendingWaitTrigger};
use crate::config::HarnessConfig;
use crate::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
use crate::store::snapshot::SidebarSnapshot;
use crate::utils::time::{format_duration_coarse, format_duration_compact};

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

/// The ping text; `limit` is the reached maximum when this is the run's last ping.
pub fn prompt(agent: &AgentState, now: Timestamp, limit: Option<Duration>) -> String {
    let mut text = "Cache keepalive, no action needed. Waiting on:".to_owned();
    for wait in &agent.pending_waits {
        let trigger = &wait.trigger;
        let elapsed = wait
            .armed_at
            .map(|at| format_duration_coarse(now.duration_since(at).as_secs()));
        let mut facts = Vec::new();
        match trigger {
            PendingWaitTrigger::Team { .. } => {
                facts.extend(elapsed);
                facts.push(trigger.headline(now));
            }
            PendingWaitTrigger::Subagent {
                deadline_at,
                settled,
                ..
            } => {
                facts.extend(elapsed);
                facts.push(trigger.headline(now));
                if let (Some(deadline), None) = (deadline_at, settled) {
                    facts.push(if *deadline <= now {
                        "deadline passed".to_owned()
                    } else {
                        format!(
                            "deadline in {}",
                            format_duration_coarse(deadline.duration_since(now).as_secs())
                        )
                    });
                }
            }
            PendingWaitTrigger::Command { .. } | PendingWaitTrigger::Check { .. } => {
                facts.push(trigger.detail().unwrap_or_default());
                facts.extend(elapsed);
            }
            PendingWaitTrigger::Timer { due, .. } => {
                facts.push(if *due <= now {
                    "timer due".to_owned()
                } else {
                    format!(
                        "timer in {}",
                        format_duration_coarse(due.duration_since(now).as_secs())
                    )
                });
                facts.extend(elapsed);
            }
            PendingWaitTrigger::Pid { .. }
            | PendingWaitTrigger::Condition { .. }
            | PendingWaitTrigger::File { .. }
            | PendingWaitTrigger::Signal { .. } => {
                facts.push(format!("{} {}", trigger.kind_word(), trigger.headline(now)));
                facts.extend(elapsed);
            }
        }
        text.push_str(&format!("\n- {}: {}", wait.name, facts.join(", ")));
    }
    if let Some(limit) = limit {
        text.push_str(&format!(
            "\nKeepalive limit {} reached: this is the last ping until your next turn.",
            format_duration_compact(limit)
        ));
    }
    text
}

/// The configured maximum when a ping delivered at `now` is the last of its run: the next one would start at or past the cap.
pub fn final_ping(agent: &AgentState, config: &HarnessConfig, now: Timestamp) -> Option<Duration> {
    let max = config.cache_keepalive_max?;
    let fire_after = config
        .prompt_cache_ttl(&agent.kind)?
        .checked_sub(crate::config::PROMPT_CACHE_MARGIN)?;
    let since = agent.keepalive_since.or(agent.last_request_at())?;
    next_reaches_cap(since, now, fire_after, max).then_some(max)
}

/// Whether a ping one `fire_after` past `last_ping` would start at or past `max` after `since`; a negative span counts as reached.
fn next_reaches_cap(
    since: Timestamp,
    last_ping: Timestamp,
    fire_after: Duration,
    max: Duration,
) -> bool {
    Duration::try_from(last_ping.duration_since(since))
        .ok()
        .and_then(|span| span.checked_add(fire_after))
        .is_none_or(|next| next >= max)
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
    idle >= fire_after && idle < ttl && !run_reached_cap(agent, config, fire_after)
}

/// Whether the ping after the open keepalive run's last one would start at or past the cap. Reads only stamps, so the producer and the helper's recheck agree.
fn run_reached_cap(agent: &AgentState, config: &HarnessConfig, fire_after: Duration) -> bool {
    let (Some(max), Some(since), Some(last_ping)) = (
        config.cache_keepalive_max,
        agent.keepalive_since,
        agent.turn_started_at,
    ) else {
        return false;
    };
    next_reaches_cap(since, last_ping, fire_after, max)
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
        assert_eq!(agent.keepalive_since, None);
        assert!(should_keepalive(&agent, &config, ts(7085)));
        agent.keepalive_since = Some(ts(0));
        assert!(should_keepalive(&agent, &config, ts(7085)), "under the cap");
    }

    fn pinged_at(since: i64, ping: i64) -> AgentState {
        let mut agent = sleeping();
        agent.keepalive_since = Some(ts(since));
        agent.turn_started_at = Some(ts(ping));
        agent.turn_ended_at = Some(ts(ping + 5));
        agent
    }

    #[test]
    fn keepalive_cap_refuses_a_ping_that_would_start_at_the_maximum() {
        let config = HarnessConfig::default();
        let cap = 6 * 3600;
        let allowed = pinged_at(0, cap - 3540 - 1);
        assert!(should_keepalive(&allowed, &config, ts(cap - 1)));
        let capped = pinged_at(0, cap - 3540);
        assert!(!should_keepalive(&capped, &config, ts(cap)));
        assert!(
            !should_keepalive(&pinged_at(100, 50), &config, ts(3590)),
            "a negative span is refused"
        );
        let mut uncapped = config.clone();
        uncapped.cache_keepalive_max = None;
        assert!(should_keepalive(&capped, &uncapped, ts(cap)));
        let mut first = capped;
        first.keepalive_since = None;
        assert!(
            should_keepalive(&first, &config, ts(cap)),
            "the first ping of a sleep is never capped"
        );
    }

    #[test]
    fn final_ping_is_the_one_whose_successor_would_reach_the_cap() {
        let config = HarnessConfig::default();
        let max = Some(Duration::from_secs(6 * 3600));
        let cap = 6 * 3600;
        let fresh = sleeping();
        assert_eq!(final_ping(&fresh, &config, ts(cap - 3540 - 1)), None);
        assert_eq!(
            final_ping(&fresh, &config, ts(cap - 3540)),
            max,
            "falls back to the last request"
        );
        let run = pinged_at(100, 3640);
        assert_eq!(final_ping(&run, &config, ts(cap - 3540)), None);
        assert_eq!(final_ping(&run, &config, ts(cap - 3440)), max);
        let mut uncapped = config.clone();
        uncapped.cache_keepalive_max = None;
        assert_eq!(final_ping(&run, &uncapped, ts(cap)), None);
        let after_final = pinged_at(100, cap - 3440);
        assert!(
            !should_keepalive(&after_final, &config, ts(cap + 100)),
            "no ping follows a final one"
        );
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
        for trigger in [
            PendingWaitTrigger::Subagent {
                active_at: ts(3540),
                deadline_at: None,
                settled: None,
            },
            PendingWaitTrigger::Team {
                stage: Some("Review".into()),
            },
        ] {
            changed = agent.clone();
            changed.pending_waits[0].trigger = trigger;
            assert!(should_keepalive(&changed, &config, ts(3540)));
            changed.pending_waits.clear();
            assert!(!should_keepalive(&changed, &config, ts(3540)));
        }
        let mut disabled = config;
        disabled.cache_keepalive = false;
        assert!(!should_keepalive(&agent, &disabled, ts(3540)));
    }

    #[test]
    fn keepalive_prompt_is_neutral_and_includes_each_wait() {
        let mut agent = sleeping();
        agent.pending_waits.insert(
            0,
            PendingWait {
                name: "forge#feat-x".into(),
                trigger: serde_json::from_value(
                    serde_json::json!({"kind": "team", "stage": "Review"}),
                )
                .unwrap(),
                armed_at: Some(ts(-7340)),
            },
        );
        for (name, settled) in [("bright-owl", Some("completed")), ("calm-fox", None)] {
            agent.pending_waits.insert(
                0,
                PendingWait {
                    name: name.into(),
                    trigger: serde_json::from_value(serde_json::json!({
                        "kind": "subagent", "active_at": ts(3280),
                        "deadline_at": ts(4600), "settled": settled,
                    }))
                    .unwrap(),
                    armed_at: Some(ts(960)),
                },
            );
        }
        agent.pending_waits.push(PendingWait {
            name: "ci".into(),
            trigger: PendingWaitTrigger::Signal {
                selector: "pr.checks".into(),
            },
            armed_at: None,
        });
        agent.pending_waits.push(PendingWait {
            name: "nap".into(),
            trigger: PendingWaitTrigger::Timer {
                due: ts(4240),
                delay: None,
            },
            armed_at: Some(ts(3180)),
        });
        let text = "Cache keepalive, no action needed. Waiting on:\n- calm-fox: 42m, active 3m ago, deadline in 18m\n- bright-owl: 42m, completed, reporting\n- forge#feat-x: 3h, stage Review\n- gate: cargo xtask gate, 58m\n- ci: signal pr.checks\n- nap: timer in 12m, 5m";
        assert_eq!(prompt(&agent, ts(3485), None), text);
        assert_eq!(
            prompt(&agent, ts(3485), Some(Duration::from_secs(90 * 60))),
            format!(
                "{text}\nKeepalive limit 90m reached: this is the last ping until your next turn."
            )
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
