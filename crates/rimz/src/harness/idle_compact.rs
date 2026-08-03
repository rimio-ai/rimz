//! Producer-side idle compaction: condense a warm, inactive agent context before
//! its provider prompt cache expires.
//!
//! The elected producer makes the pure eligibility decision and spawns the
//! detached `rimz agents idle-compact` helper. The helper owns the durable
//! message write; this module writes only a disposable pacing record.

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::{AgentState, AgentStatus};
use crate::config::{HarnessConfig, IdleCompactMode};
use crate::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
use crate::store::atomic::write_temp_then_rename_cache;
use crate::{RuntimePaths, store::snapshot::SidebarSnapshot, store::snapshot::WorktreePrState};

/// Below this fill, re-caching costs less than an extra compaction turn.
pub const IDLE_COMPACT_MIN_TOKENS: u64 = 50_000;

/// Bounds duplicate helper spawns while a frame or context reading catches up.
const IDLE_COMPACT_RESPAWN_THROTTLE: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdleCompactRequest {
    pub workspace_id: WorkspaceId,
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub pane_id: PaneId,
    pub command: String,
    pub occupied_tokens: u64,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FireRecord {
    fired_at: Timestamp,
    fired_for_activity: Timestamp,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct AutoSignals {
    teammate_working: bool,
    pr_open: bool,
}

/// Compact every eligible root agent whose idle threshold is due.
pub(crate) fn compact_idle_agents(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    config: &HarnessConfig,
) {
    if config.idle_compact == IdleCompactMode::Off {
        return;
    }
    let pr_cache = (config.idle_compact == IdleCompactMode::Auto)
        .then(|| crate::sidebar::refresh::pr::read_pr_state_cache(&runtime.pr_state_path()));
    for agent in &snapshot.agents {
        let command = crate::agents::spec_by_kind(agent.kind.as_str())
            .and_then(|spec| spec.launch.compact_command());
        let occupied = agent.occupied_context_tokens();
        let record_path = fire_record_path(runtime, &agent.kind, &agent.agent_id);
        let signals = if config.idle_compact == IdleCompactMode::Auto {
            AutoSignals {
                teammate_working: teammate_working(snapshot, agent),
                pr_open: pr_cache
                    .as_ref()
                    .is_some_and(|cache| worktree_pr_open(agent, cache)),
            }
        } else {
            AutoSignals::default()
        };
        if !should_compact(
            agent,
            command,
            occupied,
            config.idle_compact,
            config.idle_compact_after(),
            snapshot.now,
            signals,
            read_fire_record(&record_path).as_ref(),
        ) {
            continue;
        }
        let Some(pane_id) = snapshot.live_agent_pane(&agent.kind, &agent.agent_id) else {
            continue;
        };
        let (Some(command), Some(occupied)) = (command, occupied) else {
            continue;
        };
        let peers = crate::harness::target::addressable_agents(snapshot);
        let label = crate::harness::target::agent_handle(agent, &peers, false);
        if spawn_idle_compact(
            runtime,
            &agent.kind,
            &agent.agent_id,
            &pane_id,
            command,
            occupied,
            &label,
        ) {
            write_fire_record(
                &record_path,
                &FireRecord {
                    fired_at: snapshot.now,
                    fired_for_activity: agent.last_activity,
                },
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn should_compact(
    agent: &AgentState,
    compact_command: Option<&str>,
    occupied: Option<u64>,
    mode: IdleCompactMode,
    idle_after: Duration,
    now: Timestamp,
    signals: AutoSignals,
    record: Option<&FireRecord>,
) -> bool {
    if mode == IdleCompactMode::Off
        || agent.is_provider_subagent()
        || agent.agent_id.is_empty()
        || compact_command.is_none()
        || agent.compacting_since.is_some()
        || agent.budget_park.is_some()
        || agent.is_awaiting_input()
        || !matches!(
            agent.effective_status(),
            AgentStatus::Idle | AgentStatus::Success
        )
    {
        return false;
    }
    if mode == IdleCompactMode::Auto && !(signals.teammate_working || signals.pr_open) {
        return false;
    }
    let idle_secs = now.as_second() - agent.last_activity.as_second();
    if idle_secs < idle_after.as_secs().min(i64::MAX as u64) as i64 {
        return false;
    }
    let Some(occupied) = occupied.filter(|tokens| *tokens >= IDLE_COMPACT_MIN_TOKENS) else {
        return false;
    };
    if agent.last_compact_command_tokens == Some(occupied) {
        return false;
    }
    record.is_none_or(|record| {
        record.fired_for_activity != agent.last_activity
            && now.as_second() - record.fired_at.as_second()
                >= IDLE_COMPACT_RESPAWN_THROTTLE.as_secs() as i64
    })
}

fn teammate_working(snapshot: &SidebarSnapshot, candidate: &AgentState) -> bool {
    let Some(channel) = crate::harness::target::agent_channel(candidate) else {
        return false;
    };
    snapshot
        .agents
        .iter()
        .filter(|agent| !agent.is_provider_subagent())
        .any(|agent| {
            !(agent.kind == candidate.kind && agent.agent_id == candidate.agent_id)
                && crate::harness::target::agent_channel(agent).as_deref() == Some(channel.as_str())
                && agent.effective_status() == AgentStatus::Running
        })
}

fn worktree_pr_open(agent: &AgentState, cache: &crate::sidebar::refresh::pr::PrStateCache) -> bool {
    let (Some(path), Some(branch)) = (
        agent.worktree_path.as_deref(),
        agent.worktree_branch.as_deref(),
    ) else {
        return false;
    };
    cache.states.get(path).is_some_and(|link| {
        link.branch.as_deref() == Some(branch) && link.state == WorktreePrState::Open
    })
}

fn fire_record_path(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
) -> PathBuf {
    runtime.root.join("idle-compact").join(format!(
        "{}.json",
        crate::store::sidecar::digest(kind.as_str(), agent_id.as_str())
    ))
}

fn read_fire_record(path: &Path) -> Option<FireRecord> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

fn write_fire_record(path: &Path, record: &FireRecord) {
    if let Err(err) = write_temp_then_rename_cache(path, record) {
        tracing::warn!(
            tags.operation = "idle_compact.write_fire_record",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to record idle-compaction pacing",
        );
    }
}

fn spawn_idle_compact(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
    pane_id: &PaneId,
    command: &str,
    occupied: u64,
    label: &str,
) -> bool {
    let request = IdleCompactRequest {
        workspace_id: runtime.workspace_id.clone(),
        kind: kind.clone(),
        agent_id: agent_id.clone(),
        pane_id: pane_id.clone(),
        command: command.to_owned(),
        occupied_tokens: occupied,
        label: label.to_owned(),
    };
    let args = crate::child_process::agent_helper_argv("idle-compact", &request);
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        workspace = %runtime.workspace_id,
        kind = %kind,
        occupied,
        "sidebar: compacting idle agent",
    );
    if let Err(err) = crate::child_process::spawn_detached_rimz(runtime, args, "agent-idle-compact")
    {
        tracing::debug!(
            workspace = %runtime.workspace_id,
            tags.operation = "idle_compact.spawn",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn agent idle-compaction",
        );
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentStatus;
    use crate::ids::{MuxName, WorkspaceId};
    use crate::sidebar::refresh::pr::{PrLink, PrStateCache};
    use crate::store::snapshot::PaneAgent;

    fn ts(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).expect("timestamp")
    }

    fn agent(status: AgentStatus, activity: i64, tokens: u64) -> AgentState {
        let mut agent = AgentState::seed(
            AgentKind::new_unchecked("claude"),
            AgentSessionId::from("session-1"),
            status,
            ts(activity),
        );
        agent.usage.total_tokens = Some(tokens);
        agent.worktree_path = Some("/repo/worktree".to_owned());
        agent.worktree_branch = Some("feat/cache".to_owned());
        agent
    }

    #[test]
    fn fire_cache_path_preserves_existing_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("runtime");

        assert_eq!(
            fire_record_path(
                &runtime,
                &AgentKind::new_unchecked("claude"),
                &"sess".into()
            )
            .file_name()
            .and_then(|name| name.to_str()),
            Some("4a8d94f232e55a6a0879ba0858b59241.json")
        );
    }

    fn due(agent: &AgentState) -> bool {
        should_compact(
            agent,
            Some("/compact"),
            agent.occupied_context_tokens(),
            IdleCompactMode::Always,
            Duration::from_secs(59 * 60),
            ts(10_000),
            AutoSignals::default(),
            None,
        )
    }

    #[test]
    fn predicate_requires_idle_threshold_and_context_floor() {
        let candidate = agent(AgentStatus::Idle, 6_000, 50_000);
        assert!(due(&candidate));
        assert!(!due(&agent(AgentStatus::Idle, 6_461, 50_000)));
        assert!(!due(&agent(AgentStatus::Idle, 6_000, 49_999)));
        assert!(!should_compact(
            &candidate,
            None,
            Some(50_000),
            IdleCompactMode::Always,
            Duration::from_secs(59 * 60),
            ts(10_000),
            AutoSignals::default(),
            None,
        ));
        assert!(!should_compact(
            &candidate,
            Some("/compact"),
            Some(50_000),
            IdleCompactMode::Off,
            Duration::from_secs(59 * 60),
            ts(10_000),
            AutoSignals::default(),
            None,
        ));
    }

    #[test]
    fn predicate_skips_busy_parked_compacting_and_child_agents() {
        for status in [
            AgentStatus::Running,
            AgentStatus::Waiting,
            AgentStatus::Failed,
            AgentStatus::Paused,
        ] {
            assert!(!due(&agent(status, 6_000, 50_000)), "{status:?}");
        }

        let mut parked = agent(AgentStatus::Idle, 6_000, 50_000);
        parked.budget_park = Some(crate::harness::budget::BudgetPark {
            cap_usd: 1.0,
            spend_usd: 1.0,
            window: crate::harness::budget::BudgetWindow::Session,
            at: ts(6_000),
            scope: crate::harness::budget::BudgetScope::Agent,
            account_kind: None,
            resets_at: None,
        });
        assert!(!due(&parked));

        let mut compacting = agent(AgentStatus::Idle, 6_000, 50_000);
        compacting.compacting_since = Some(ts(9_000));
        assert!(!due(&compacting));

        let mut child = agent(AgentStatus::Idle, 6_000, 50_000);
        child.parent_agent_id = Some(AgentSessionId::from("parent"));
        assert!(!due(&child));
    }

    #[test]
    fn predicate_deduplicates_context_and_paces_spawns() {
        let mut candidate = agent(AgentStatus::Idle, 6_000, 80_000);
        candidate.last_compact_command_tokens = Some(80_000);
        assert!(!due(&candidate));
        candidate.last_compact_command_tokens = None;

        let same_activity = FireRecord {
            fired_at: ts(8_000),
            fired_for_activity: candidate.last_activity,
        };
        assert!(!should_compact(
            &candidate,
            Some("/compact"),
            Some(80_000),
            IdleCompactMode::Always,
            Duration::from_secs(59 * 60),
            ts(10_000),
            AutoSignals::default(),
            Some(&same_activity),
        ));

        let recent = FireRecord {
            fired_at: ts(9_500),
            fired_for_activity: ts(5_000),
        };
        assert!(!should_compact(
            &candidate,
            Some("/compact"),
            Some(80_000),
            IdleCompactMode::Always,
            Duration::from_secs(59 * 60),
            ts(10_000),
            AutoSignals::default(),
            Some(&recent),
        ));
    }

    #[test]
    fn auto_requires_a_working_teammate_or_open_pr() {
        let candidate = agent(AgentStatus::Idle, 6_000, 80_000);
        for (signals, expected) in [
            (AutoSignals::default(), false),
            (
                AutoSignals {
                    teammate_working: true,
                    pr_open: false,
                },
                true,
            ),
            (
                AutoSignals {
                    teammate_working: false,
                    pr_open: true,
                },
                true,
            ),
        ] {
            assert_eq!(
                should_compact(
                    &candidate,
                    Some("/compact"),
                    Some(80_000),
                    IdleCompactMode::Auto,
                    Duration::from_secs(59 * 60),
                    ts(10_000),
                    signals,
                    None,
                ),
                expected
            );
        }
        assert!(due(&candidate), "always ignores auto signals");
    }

    #[test]
    fn auto_signal_resolution_uses_channel_and_matching_open_pr_branch() {
        let candidate = agent(AgentStatus::Idle, 6_000, 80_000);
        let mut teammate = agent(AgentStatus::Running, 9_900, 1);
        teammate.agent_id = AgentSessionId::from("session-2");
        let snapshot = SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(Path::new("/repo")),
            vec![candidate.clone(), teammate],
            ts(10_000),
        );
        assert!(teammate_working(&snapshot, &candidate));

        let open = PrLink {
            branch: Some("feat/cache".to_owned()),
            incarnation: None,
            state: WorktreePrState::Open,
            number: None,
            url: None,
            ci: None,
            merge_sha: None,
        };
        let mut cache = PrStateCache::default();
        cache
            .states
            .insert("/repo/worktree".to_owned(), open.clone());
        assert!(worktree_pr_open(&candidate, &cache));
        for state in [WorktreePrState::Closed, WorktreePrState::Merged] {
            cache.states.get_mut("/repo/worktree").expect("link").state = state;
            assert!(!worktree_pr_open(&candidate, &cache));
        }
        cache.states.insert(
            "/repo/worktree".to_owned(),
            PrLink {
                branch: Some("other".to_owned()),
                ..open
            },
        );
        assert!(!worktree_pr_open(&candidate, &cache));
    }

    #[test]
    fn producer_records_one_spawn_for_an_idle_stretch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime");
        runtime.ensure_dirs().expect("runtime dirs");
        let candidate = agent(AgentStatus::Idle, 6_000, 80_000);
        let mut snapshot =
            SidebarSnapshot::build_with_agents(workspace_id, vec![candidate.clone()], ts(10_000));
        snapshot.agent_panes.push(PaneAgent {
            kind: candidate.kind.clone(),
            kind_ordinal: None,
            name: None,
            name_explicit: false,
            profile: None,
            role: None,
            channel: None,
            agent_id: Some(candidate.agent_id.clone()),
            pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
            pane_pid: None,
            worktree_path: candidate.worktree_path.clone(),
            worktree_branch: candidate.worktree_branch.clone(),
        });
        let config = HarnessConfig {
            idle_compact: IdleCompactMode::Always,
            idle_compact_after: Some(Duration::from_secs(59 * 60)),
            ..Default::default()
        };

        compact_idle_agents(&snapshot, &runtime, &config);
        let record = read_fire_record(&fire_record_path(
            &runtime,
            &candidate.kind,
            &candidate.agent_id,
        ))
        .expect("fire record");
        assert_eq!(record.fired_for_activity, candidate.last_activity);
        assert!(!should_compact(
            &candidate,
            Some("/compact"),
            Some(80_000),
            IdleCompactMode::Always,
            Duration::from_secs(59 * 60),
            ts(11_000),
            AutoSignals::default(),
            Some(&record),
        ));
    }
}
