//! Producer-side idle compaction: condense an idle seat's context, once it
//! reaches the seat's token floor, before its provider prompt cache expires.
//!
//! The room host and the detached `rimz agents idle-compact` helper
//! share one eligibility decision. The helper owns the durable message write;
//! this module writes only a disposable spawn-pacing record.

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::agents::{AgentState, AgentStatus};
use crate::config::{HarnessConfig, MachineConfig};
use crate::disk::atomic::write_temp_then_rename_cache;
use crate::harness::cache_keepalive::SeatPolicy;
use crate::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
use crate::store::snapshot::SidebarSnapshot;

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
    fired_for_request: Timestamp,
}

/// When an idle seat compacts: at or above `floor` tokens, once idle for
/// `fire_after` and before its prompt cache expires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdleWindow {
    pub floor: u64,
    pub fire_after: Duration,
    pub cache_expires: Duration,
}

/// The seat's window: its floor (`None` is off) and the provider's prompt-cache
/// TTL less `harness.prompt_cache_margin`. An unknown TTL never compacts.
pub fn fire_point(
    agent: &AgentState,
    floor: Option<u64>,
    harness: &HarnessConfig,
) -> Option<IdleWindow> {
    let ttl = harness.prompt_cache_ttl(&agent.kind)?;
    Some(IdleWindow {
        floor: floor?,
        fire_after: ttl.checked_sub(harness.prompt_cache_margin)?,
        cache_expires: ttl,
    })
}

fn eligible_seat(agent: &AgentState, floor: u64) -> bool {
    !agent.is_provider_subagent()
        && !agent.agent_id.is_empty()
        && agent.compacting_since.is_none()
        && !agent.compacted_since_prompt()
        && agent.budget_park.is_none()
        && agent.hibernation.is_none()
        && !agent.is_awaiting_input()
        && matches!(
            agent.effective_status(),
            AgentStatus::Idle | AgentStatus::Success
        )
        && agent.occupied_context_tokens().is_some_and(|tokens| {
            tokens >= floor && agent.last_compact_command_tokens != Some(tokens)
        })
}

pub(super) fn cohort_live(agent: &AgentState) -> bool {
    agent
        .lane_path()
        .and_then(|root| super::scratch::board_stage(Path::new(root)))
        .is_none_or(|stage| stage.name != crate::config::DONE_STAGE)
}

/// Shared producer/helper decision; spawn pacing is separate from delivery.
pub fn should_compact(
    agent: &AgentState,
    command: Option<&str>,
    window: Option<IdleWindow>,
    now: Timestamp,
) -> bool {
    let (Some(window), Some(last_request_at)) = (window, agent.last_request_at()) else {
        return false;
    };
    let idle = now.as_second() - last_request_at.as_second();
    eligible_seat(agent, window.floor)
        && command.is_some()
        && idle >= window.fire_after.as_secs().min(i64::MAX as u64) as i64
        && idle < window.cache_expires.as_secs().min(i64::MAX as u64) as i64
}

/// Compact each eligible seat whose floor and idle window are due; a seat keep-warm is holding waits for its horizon.
pub(crate) fn compact_idle_agents(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    config: &MachineConfig,
    policy: &SeatPolicy,
) {
    compact_idle_agents_with(snapshot, runtime, config, policy, |request| {
        spawn_idle_compact(runtime, request)
    });
}

fn compact_idle_agents_with(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    config: &MachineConfig,
    policy: &SeatPolicy,
    mut spawn: impl FnMut(&IdleCompactRequest) -> bool,
) {
    for agent in snapshot.agents.iter() {
        let floor = policy.idle_compact_floor(agent, &config.harness);
        let Some(window) = fire_point(agent, floor, &config.harness) else {
            continue;
        };
        let command = crate::agents::compact_command(agent, &config.harness);
        if !should_compact(agent, command.as_deref(), Some(window), snapshot.now)
            || policy
                .holding(agent, &config.harness, snapshot.now)
                .is_some()
        {
            continue;
        }
        let record_path = fire_record_path(runtime, &agent.kind, &agent.agent_id);
        if !spawn_due(agent, snapshot.now, read_fire_record(&record_path).as_ref()) {
            continue;
        }
        let Some(pane_id) = snapshot.live_agent_pane(&agent.kind, &agent.agent_id) else {
            continue;
        };
        let (Some(command), Some(occupied_tokens), Some(last_request_at)) = (
            command,
            agent.occupied_context_tokens(),
            agent.last_request_at(),
        ) else {
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
                    fired_for_request: last_request_at,
                },
            );
        }
    }
}

fn spawn_due(agent: &AgentState, now: Timestamp, record: Option<&FireRecord>) -> bool {
    record.is_none_or(|record| {
        Some(record.fired_for_request) != agent.last_request_at()
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
        agent.turn_started_at = Some(ts(activity));
        agent.turn_ended_at = Some(ts(activity));
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

    /// Default harness: Claude's 60m TTL less the 1m margin opens the window at 59m.
    const NOW: i64 = 6_000 + 3_550;

    fn due(agent: &AgentState) -> bool {
        should_compact(
            agent,
            Some("/compact"),
            fire_point(agent, Some(50_000), &HarnessConfig::default()),
            ts(NOW),
        )
    }

    #[test]
    fn predicate_requires_idle_threshold_and_the_seat_floor() {
        let candidate = agent(AgentStatus::Idle, 6_000, 50_000);
        assert!(due(&candidate));
        assert!(!due(&agent(AgentStatus::Idle, 6_011, 50_000)));
        assert!(
            !due(&agent(AgentStatus::Idle, 6_000, 49_999)),
            "one below the floor"
        );
        let mut compacted = candidate.clone();
        compacted.compacted_awaiting_prompt = Some(ts(6_000));
        assert!(!due(&compacted), "a compacted seat waits for a real prompt");
        assert!(!should_compact(
            &candidate,
            None,
            fire_point(&candidate, Some(50_000), &HarnessConfig::default()),
            ts(NOW),
        ));
        assert_eq!(
            fire_point(&candidate, None, &HarnessConfig::default()),
            None,
            "an off floor opens no window"
        );
    }

    #[test]
    fn window_opens_at_ttl_less_the_configured_margin_and_closes_at_ttl() {
        let candidate = agent(AgentStatus::Idle, 0, 80_000);
        let harness: HarnessConfig = toml::from_str("prompt_cache_margin = \"6m\"").unwrap();
        let window = fire_point(&candidate, Some(50_000), &harness);
        assert_eq!(
            window.map(|window| window.fire_after.as_secs()),
            Some(54 * 60)
        );
        for (now, expected) in [
            (54 * 60 - 1, false),
            (54 * 60, true),
            (60 * 60 - 1, true),
            (60 * 60, false),
            (86_400, false),
        ] {
            assert_eq!(
                should_compact(&candidate, Some("/compact"), window, ts(now)),
                expected,
                "{now}"
            );
        }
    }

    #[test]
    fn predicate_clamps_request_anchor_after_resume() {
        let mut candidate = agent(AgentStatus::Idle, 30 * 60, 80_000);
        candidate.turn_ended_at = Some(ts(0));
        let window = fire_point(&candidate, Some(50_000), &HarnessConfig::default());
        for (now, expected) in [(59 * 60 - 1, false), (59 * 60, true), (60 * 60, false)] {
            assert_eq!(
                should_compact(&candidate, Some("/compact"), window, ts(now)),
                expected
            );
        }
    }

    #[test]
    fn predicate_requires_a_completed_request() {
        let mut candidate = agent(AgentStatus::Idle, 0, 80_000);
        candidate.turn_ended_at = None;
        assert!(!due(&candidate));
    }

    #[test]
    fn predicate_counts_idle_from_the_last_request_not_the_response_end() {
        let mut candidate = agent(AgentStatus::Idle, 5_000, 80_000);
        candidate.last_tool_at = Some(ts(6_000));
        candidate.turn_ended_at = Some(ts(9_000));
        assert!(due(&candidate));
        candidate.last_tool_at = Some(ts(7_000));
        assert!(!due(&candidate));
    }

    #[test]
    fn solo_seats_and_team_seats_compact_whatever_the_board_says() {
        let mut candidate = agent(AgentStatus::Idle, 6_000, 80_000);
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("blackboard.md"), "Stage: Done\n").unwrap();
        candidate.worktree_path = Some(root.path().to_string_lossy().into_owned());
        candidate.team = None;
        assert!(due(&candidate), "a solo seat reads no board");
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
        assert!(due(&candidate), "Done still compacts");
    }

    #[test]
    fn cache_timing_reaches_the_request_before_expiry() {
        // Request anchoring leaves only tick, spawn, and submit latency.
        const PRODUCER_TICK_BOUND: u64 = 1;
        const HELPER_SPAWN_BOUND: u64 = 10;
        const KEYSTROKE_TO_REQUEST_BOUND: u64 = 10;
        for (kind, model, config, ttl) in [
            ("claude", "any", "", Some(3600)),
            ("codex", "gpt-5.6-terra", "", Some(1800)),
            ("codex", "gpt-6-luna", "", Some(1800)),
            ("codex", "gpt-5-codex", "", Some(1800)),
            ("amp", "any", "", None),
            (
                "claude",
                "any",
                "[prompt_cache_ttl]\nclaude = \"5m\"",
                Some(300),
            ),
            (
                "claude",
                "any",
                "[prompt_cache_ttl]\nclaude = \"off\"",
                None,
            ),
        ] {
            let mut candidate = agent(AgentStatus::Idle, 0, 80_000);
            candidate.kind = AgentKind::new_unchecked(kind);
            candidate.model = Some(model.to_owned());
            let harness: HarnessConfig = toml::from_str(config).unwrap();
            let point = fire_point(&candidate, Some(50_000), &harness);
            assert_eq!(
                point.map(|point| point.fire_after.as_secs()),
                ttl.map(|ttl| ttl - 60),
                "{kind}/{model}"
            );
            if let (Some(point), Some(ttl)) = (point, ttl) {
                assert!(
                    point.fire_after.as_secs()
                        + PRODUCER_TICK_BOUND
                        + HELPER_SPAWN_BOUND
                        + KEYSTROKE_TO_REQUEST_BOUND
                        < ttl
                );
                for (idle, expected) in [
                    (point.fire_after.as_secs(), true),
                    (point.fire_after.as_secs() - 1, false),
                    (ttl - 1, true),
                    (ttl, false),
                ] {
                    assert_eq!(
                        should_compact(&candidate, Some("/compact"), Some(point), ts(idle as i64)),
                        expected
                    );
                }
            }
        }
    }

    #[test]
    fn producer_fires_at_each_seats_own_floor_with_its_brief() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let mut candidate = agent(AgentStatus::Idle, 6_100, 80_000);
        candidate.turn_ended_at = Some(ts(6_000));
        let mut snapshot =
            SidebarSnapshot::build_with_agents(workspace_id, vec![candidate.clone()], ts(NOW));
        std::sync::Arc::make_mut(&mut snapshot.agent_panes).push(PaneAgent {
            checkout: None,
            root_lane: false,
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
        let config = crate::config::MachineConfig::default();
        let mut policy = SeatPolicy::default();
        for name in ["solo", "probe.lead"] {
            let mut profile: crate::config::Profile = toml::from_str("agent = \"claude\"").unwrap();
            profile.idle_compact = Some(crate::config::IdleCompact::Tokens(80_000));
            policy.profiles.0.insert(name.to_owned(), profile);
        }
        let mut team: crate::config::Team =
            toml::from_str("[[roles]]\nrole = \"coder\"\nprofile = \"claude\"").unwrap();
        team.roles[0].idle_compact = Some(crate::config::IdleCompact::Tokens(80_000));
        policy.teams.0.insert("probe".to_owned(), team);
        let path = fire_record_path(&runtime, &candidate.kind, &candidate.agent_id);
        let mut fire = |seat: &AgentState| {
            std::sync::Arc::make_mut(&mut snapshot.agents)[0] = seat.clone();
            let mut requests = Vec::new();
            compact_idle_agents_with(&snapshot, &runtime, &config, &policy, |request| {
                requests.push(request.clone());
                true
            });
            if !requests.is_empty() {
                assert_eq!(
                    Some(read_fire_record(&path).unwrap().fired_for_request),
                    seat.last_request_at()
                );
                std::fs::remove_file(&path).unwrap();
            }
            requests
        };

        assert!(
            fire(&candidate).is_empty(),
            "80k sits below the 100k default"
        );

        let mut toml_role = candidate.clone();
        toml_role.role = Some("coder".to_owned());
        let requests = fire(&toml_role);
        assert_eq!(requests.len(), 1, "a TOML role override");
        assert_eq!(
            requests[0].command,
            crate::agents::compact_command(&toml_role, &config.harness).unwrap()
        );
        assert!(requests[0].command.contains("This seat is a team member"));

        let mut markdown_role = candidate.clone();
        markdown_role.role = Some("lead".to_owned());
        markdown_role.profile = Some("probe.lead".to_owned());
        assert_eq!(
            fire(&markdown_role).len(),
            1,
            "a Markdown role's synthetic profile"
        );

        let mut solo = candidate.clone();
        solo.team = None;
        solo.profile = Some("solo".to_owned());
        let requests = fire(&solo);
        assert_eq!(requests.len(), 1, "a solo seat's profile floor");
        assert_eq!(
            requests[0].command,
            crate::agents::compact_command(&solo, &config.harness).unwrap()
        );
        assert!(!requests[0].command.contains("This seat is a team member"));

        solo.parent_agent_id = Some(AgentSessionId::from("parent"));
        solo.launch_depth = Some(1);
        assert!(
            fire(&solo).is_empty(),
            "a rimz subagents child never compacts"
        );
        assert!(!path.exists());
    }

    #[test]
    fn frozen_seats_never_receive_idle_compaction() {
        let mut candidate = agent(AgentStatus::Idle, 6_000, 50_000);
        assert!(due(&candidate));
        candidate.hibernation = Some(crate::agents::state::Hibernation {
            frozen_at: ts(9_000),
            step: 0,
            reclaimed_bytes: 200,
            wake_error: None,
        });
        assert!(!due(&candidate));
        candidate.hibernation = None;
        assert!(due(&candidate));
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
        assert!(!due(&sleeping));

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

        let same_turn = FireRecord {
            fired_at: ts(8_000),
            fired_for_request: ts(6_000),
        };
        let mut resumed = agent(AgentStatus::Idle, 7_800, 80_000);
        resumed.turn_ended_at = candidate.turn_ended_at;
        assert!(!spawn_due(&resumed, ts(10_000), Some(&same_turn)));
        candidate.turn_ended_at = Some(ts(8_500));
        assert!(!spawn_due(&candidate, ts(10_000), Some(&same_turn)));
        candidate.last_tool_at = Some(ts(8_000));
        assert!(spawn_due(&candidate, ts(10_000), Some(&same_turn)));

        let recent = FireRecord {
            fired_at: ts(9_500),
            fired_for_request: ts(5_000),
        };
        assert!(!spawn_due(&candidate, ts(10_000), Some(&recent)));
    }

    #[test]
    fn keep_warm_holding_defers_compaction_until_the_horizon() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let mut seat = agent(AgentStatus::Idle, 0, 80_000);
        seat.role = Some("coder".to_owned());
        let mut snapshot =
            SidebarSnapshot::build_with_agents(workspace_id, vec![seat.clone()], ts(3_540));
        std::sync::Arc::make_mut(&mut snapshot.agent_panes).push(PaneAgent {
            checkout: None,
            root_lane: false,
            kind: seat.kind.clone(),
            kind_ordinal: None,
            name: None,
            name_explicit: false,
            profile: None,
            role: None,
            channel: None,
            agent_id: Some(seat.agent_id.clone()),
            pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
            pane_pid: None,
            worktree_path: seat.worktree_path.clone(),
            worktree_branch: seat.worktree_branch.clone(),
        });
        let mut policy = SeatPolicy::default();
        let mut team: crate::config::Team =
            toml::from_str("[[roles]]\nrole = \"coder\"\nprofile = \"claude\"\nkeep-warm = \"2h\"")
                .unwrap();
        team.roles[0].idle_compact = Some(crate::config::IdleCompact::Tokens(50_000));
        policy.teams.0.insert("probe".to_owned(), team);
        let config = crate::config::MachineConfig::default();
        let mut spawned = 0;
        compact_idle_agents_with(&snapshot, &runtime, &config, &policy, |_| {
            spawned += 1;
            false
        });
        assert_eq!(spawned, 0, "holding outranks idle compaction");
        compact_idle_agents_with(
            &snapshot,
            &runtime,
            &config,
            &SeatPolicy {
                teams: {
                    let mut teams = policy.teams.clone();
                    teams.0.get_mut("probe").unwrap().roles[0].keep_warm = None;
                    teams
                },
                ..SeatPolicy::default()
            },
            |_| {
                spawned += 1;
                false
            },
        );
        assert_eq!(spawned, 1, "the same seat compacts without keep-warm");
        let mut pings_off = config.clone();
        pings_off.harness.cache_keepalive = false;
        compact_idle_agents_with(&snapshot, &runtime, &pings_off, &policy, |_| {
            spawned += 1;
            false
        });
        assert_eq!(
            spawned, 2,
            "a horizon no ping maintains does not defer compaction"
        );
        // Past the horizon, the window runs from the last ping's request.
        std::sync::Arc::make_mut(&mut snapshot.agents)[0].pinged_at = Some(ts(7_100));
        snapshot.now = ts(7_100 + 3_540);
        compact_idle_agents_with(&snapshot, &runtime, &config, &policy, |_| {
            spawned += 1;
            false
        });
        assert_eq!(spawned, 3, "compaction resumes once the horizon passed");
    }
}
