//! Producer-side idle compaction: condense an inactive team member's context
//! before its provider prompt cache expires.
//!
//! The elected producer and the detached `rimz agents idle-compact` helper
//! share one eligibility decision. The helper owns the durable message write;
//! this module writes only a disposable spawn-pacing record.

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::agents::{AgentAccount, AgentState, AgentStatus};
use crate::config::{IdleCompactMode, MachineConfig, TeamsConfig};
use crate::disk::atomic::write_temp_then_rename_cache;
use crate::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
use crate::store::snapshot::SidebarSnapshot;

/// Below this fill, re-caching costs less than an extra compaction turn.
pub const IDLE_COMPACT_MIN_TOKENS: u64 = 50_000;

/// Covers the final generation, producer tick, helper spawn, and submission.
pub const PROMPT_CACHE_MARGIN: Duration = Duration::from_secs(3 * 60);

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

pub fn fire_point(
    agent: &AgentState,
    mode: IdleCompactMode,
    account: Option<&AgentAccount>,
) -> Option<Duration> {
    match mode {
        IdleCompactMode::Off => None,
        IdleCompactMode::After(duration) => Some(duration),
        IdleCompactMode::On => crate::agents::find_definition(agent.kind.as_str())?
            .prompt_cache_ttl(agent.model.as_deref(), account)?
            .checked_sub(PROMPT_CACHE_MARGIN)
            .filter(|duration| !duration.is_zero()),
    }
}

pub fn resolve_teams(config: &MachineConfig, project_root: Option<&Path>) -> TeamsConfig {
    project_root
        .and_then(|root| {
            crate::config::effective::load(config, root)
                .ok()
                .map(|effective| effective.teams)
        })
        .unwrap_or_else(|| config.agents.teams.clone())
}

pub fn resolve_mode(
    agent: &AgentState,
    teams: &TeamsConfig,
    default: IdleCompactMode,
) -> IdleCompactMode {
    agent
        .team
        .as_ref()
        .and_then(|name| teams.0.get(name))
        .map_or(default, |team| {
            team.idle_compact(agent.role.as_deref().unwrap_or(""), default)
        })
}

fn eligible_seat(agent: &AgentState) -> bool {
    agent.team.is_some()
        && !agent.is_provider_subagent()
        && !agent.agent_id.is_empty()
        && agent.compacting_since.is_none()
        && agent.budget_park.is_none()
        && !agent.is_awaiting_input()
        && matches!(
            agent.effective_status(),
            AgentStatus::Idle | AgentStatus::Success | AgentStatus::Sleeping
        )
        && agent.occupied_context_tokens().is_some_and(|tokens| {
            tokens >= IDLE_COMPACT_MIN_TOKENS && agent.last_compact_command_tokens != Some(tokens)
        })
}

fn cohort_live(agent: &AgentState) -> bool {
    agent
        .worktree_path
        .as_deref()
        .and_then(|root| super::scratch::board_stage(Path::new(root)))
        .is_none_or(|stage| stage.name != crate::config::DONE_STAGE)
}

/// Shared producer/helper decision; spawn pacing is separate from delivery.
pub fn should_compact(
    agent: &AgentState,
    command: Option<&str>,
    idle_after: Option<Duration>,
    now: Timestamp,
) -> bool {
    let Some(idle_after) = idle_after else {
        return false;
    };
    eligible_seat(agent)
        && command.is_some()
        && cohort_live(agent)
        && now.as_second() - agent.last_activity.as_second()
            >= idle_after.as_secs().min(i64::MAX as u64) as i64
}

/// Compact each eligible team member whose idle threshold is due.
pub(crate) fn compact_idle_agents(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    config: &MachineConfig,
) {
    compact_idle_agents_with(snapshot, runtime, config, |request| {
        spawn_idle_compact(runtime, request)
    });
}

fn compact_idle_agents_with(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    config: &MachineConfig,
    mut spawn: impl FnMut(&IdleCompactRequest) -> bool,
) {
    let teams = std::cell::OnceCell::new();
    for agent in &snapshot.agents {
        if !eligible_seat(agent) {
            continue;
        }
        let teams = teams.get_or_init(|| resolve_teams(config, snapshot.project_root.as_deref()));
        let mode = resolve_mode(agent, teams, config.harness.idle_compact);
        let account =
            crate::sidebar::refresh::accounts::cached_account(runtime, &agent.login_key());
        let command = crate::agents::compact_command(agent, &config.harness);
        if !should_compact(
            agent,
            command.as_deref(),
            fire_point(agent, mode, account.as_ref()),
            snapshot.now,
        ) {
            continue;
        }
        let record_path = fire_record_path(runtime, &agent.kind, &agent.agent_id);
        if !spawn_due(agent, snapshot.now, read_fire_record(&record_path).as_ref()) {
            continue;
        }
        let Some(pane_id) = snapshot.live_agent_pane(&agent.kind, &agent.agent_id) else {
            continue;
        };
        let (Some(command), Some(occupied_tokens)) = (command, agent.occupied_context_tokens())
        else {
            continue;
        };
        let peers = crate::address::addressable_agents(snapshot);
        let request = IdleCompactRequest {
            workspace_id: runtime.workspace_id.clone(),
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            pane_id,
            command,
            occupied_tokens,
            label: crate::address::agent_handle(agent, &peers, false),
        };
        if spawn(&request) {
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

fn spawn_due(agent: &AgentState, now: Timestamp, record: Option<&FireRecord>) -> bool {
    record.is_none_or(|record| {
        record.fired_for_activity != agent.last_activity
            && now.as_second() - record.fired_at.as_second()
                >= IDLE_COMPACT_RESPAWN_THROTTLE.as_secs() as i64
    })
}

fn fire_record_path(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
) -> PathBuf {
    runtime.live_path("idle-compact").join(format!(
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

fn spawn_idle_compact(runtime: &RuntimePaths, request: &IdleCompactRequest) -> bool {
    let args = crate::child_process::agent_helper_argv("idle-compact", request);
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        workspace = %runtime.workspace_id,
        kind = %request.kind,
        occupied = request.occupied_tokens,
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
    use crate::agents::{AgentStatus, PendingWait, PendingWaitTrigger};
    use crate::ids::{MuxName, WorkspaceId};
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
        agent.team = Some("probe".to_owned());
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
            Some(Duration::from_secs(59 * 60)),
            ts(10_000),
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
            Some(Duration::from_secs(59 * 60)),
            ts(10_000),
        ));
        assert!(!should_compact(
            &candidate,
            Some("/compact"),
            fire_point(&candidate, IdleCompactMode::Off, None),
            ts(10_000),
        ));
    }

    #[test]
    fn solo_and_done_cohorts_never_compact() {
        let mut candidate = agent(AgentStatus::Idle, 6_000, 80_000);
        candidate.team = None;
        assert!(!due(&candidate), "solo seats must never compact");
        candidate.team = Some("probe".to_owned());
        let root = tempfile::tempdir().unwrap();
        candidate.worktree_path = Some(root.path().to_string_lossy().into_owned());
        assert!(due(&candidate), "no board is live");
        std::fs::write(
            root.path().join("blackboard.md"),
            "Stage: Review (@judge)\n",
        )
        .unwrap();
        assert!(due(&candidate));
        std::fs::write(root.path().join("blackboard.md"), "Stage: Done\n").unwrap();
        assert!(!due(&candidate), "Done suppresses idle compaction");
    }

    #[test]
    fn cache_timing_reaches_the_request_before_expiry() {
        // Runtime premises: a one-second producer tick, ten seconds to spawn, ten seconds to submit, and two minutes for the previous final generation.
        const PRODUCER_TICK_BOUND: u64 = 1;
        const HELPER_SPAWN_BOUND: u64 = 10;
        const KEYSTROKE_TO_REQUEST_BOUND: u64 = 10;
        const FINAL_GENERATION_BOUND: u64 = 120;
        for (kind, model, metered, ttl) in [
            ("claude", "any", Some(true), Some(3600)),
            ("codex", "gpt-5.6-terra", None, Some(1800)),
            ("codex", "gpt-6-luna", Some(false), Some(1800)),
            ("codex", "gpt-5-codex", Some(true), None),
            ("claude", "any", Some(false), None),
            ("claude", "any", None, None),
            ("amp", "any", Some(true), None),
        ] {
            let mut candidate = agent(AgentStatus::Idle, 0, 80_000);
            candidate.kind = AgentKind::new_unchecked(kind);
            candidate.model = Some(model.to_owned());
            let account = metered.map(|metered| crate::agents::AgentAccount {
                metered: Some(metered),
                ..Default::default()
            });
            let point = fire_point(&candidate, IdleCompactMode::default(), account.as_ref());
            assert_eq!(
                point.map(|point| point.as_secs()),
                ttl.map(|ttl| ttl - 180),
                "{kind}/{model}/{metered:?}"
            );
            if let (Some(point), Some(ttl)) = (point, ttl) {
                assert!(
                    point.as_secs()
                        + PRODUCER_TICK_BOUND
                        + HELPER_SPAWN_BOUND
                        + KEYSTROKE_TO_REQUEST_BOUND
                        + FINAL_GENERATION_BOUND
                        < ttl
                );
                for (idle, expected) in [(point.as_secs(), true), (point.as_secs() - 1, false)] {
                    assert_eq!(
                        should_compact(&candidate, Some("/compact"), Some(point), ts(idle as i64)),
                        expected
                    );
                }
            }
        }
    }

    #[test]
    fn producer_sends_team_brief_and_never_spawns_for_solo() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let mut candidate = agent(AgentStatus::Idle, 6_000, 80_000);
        candidate.team = Some("probe".to_owned());
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
        let mut config = crate::config::MachineConfig::default();
        config.harness.idle_compact = IdleCompactMode::After(Duration::from_secs(59 * 60));
        let mut requests = Vec::new();
        compact_idle_agents_with(&snapshot, &runtime, &config, |request| {
            requests.push(request.clone());
            true
        });
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].command,
            crate::agents::compact_command(&candidate, &config.harness).unwrap()
        );
        assert!(requests[0].command.contains("This seat is a team member"));
        let path = fire_record_path(&runtime, &candidate.kind, &candidate.agent_id);
        assert_eq!(
            read_fire_record(&path).unwrap().fired_for_activity,
            candidate.last_activity
        );
        std::fs::remove_file(&path).unwrap();
        snapshot.agents[0].team = None;
        compact_idle_agents_with(&snapshot, &runtime, &config, |_| panic!("solo spawn"));
        assert!(!path.exists());
    }

    #[test]
    fn predicate_skips_busy_parked_compacting_and_child_agents() {
        let mut sleeping = agent(AgentStatus::Idle, 6_000, 50_000);
        sleeping.pending_waits.push(PendingWait {
            name: "wait-command".to_owned(),
            trigger: PendingWaitTrigger::Command {
                command: "cargo xtask gate".to_owned(),
            },
            armed_at: Some(ts(6_000)),
        });
        assert_eq!(sleeping.effective_status(), AgentStatus::Sleeping);
        assert!(due(&sleeping));

        for status in [
            AgentStatus::Running,
            AgentStatus::Waiting,
            AgentStatus::Failed,
            AgentStatus::Paused,
        ] {
            assert!(!due(&agent(status, 6_000, 50_000)), "{status:?}");
        }

        let mut parked = agent(AgentStatus::Idle, 6_000, 50_000);
        parked.budget_park = Some(crate::agents::BudgetPark {
            cap_usd: 1.0,
            spend_usd: 1.0,
            window: crate::agents::BudgetWindow::Session,
            at: ts(6_000),
            scope: crate::agents::BudgetScope::Agent,
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
        assert!(!spawn_due(&candidate, ts(10_000), Some(&same_activity)));

        let recent = FireRecord {
            fired_at: ts(9_500),
            fired_for_activity: ts(5_000),
        };
        assert!(!spawn_due(&candidate, ts(10_000), Some(&recent)));
    }
}
