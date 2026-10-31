//! Best-effort prompt-cache pings for sleeping agents, and for idle agents a keep-warm horizon holds. Durable delivery belongs to the helper.

use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::schedule::when::{WhenExpr, WindowReadings, evaluate};
use crate::RuntimePaths;
use crate::agents::{AgentState, AgentStatus, PendingWaitTrigger};
use crate::config::{
    HarnessConfig, IdleCompact, KeepWarm, MachineConfig, ProfilesConfig, TeamsConfig,
};
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

/// Effective teams, roles, and agent profiles: per-field keep-warm resolution and its stage gate.
#[derive(Clone, Debug, Default)]
pub struct SeatPolicy {
    pub teams: TeamsConfig,
    pub profiles: ProfilesConfig,
    pub subagent_profiles: ProfilesConfig,
}

impl SeatPolicy {
    /// Trusted project definitions over the machine's, falling back to the machine snapshot.
    pub fn load(machine: &MachineConfig, project_root: Option<&Path>) -> Self {
        let (teams, profiles, subagent_profiles) =
            crate::config::effective::load_seat_tables(machine, project_root);
        Self {
            teams,
            profiles,
            subagent_profiles,
        }
    }

    /// The role-over-team maximum (or solo profile's), admitted by the TTL floor and the
    /// current stage condition. Unknown board readings permit a hold; a while-only hold
    /// takes the ping cap. A `rimz subagents` child never resolves one; an agent-launched
    /// peer resolves like any other seat.
    pub fn horizon(&self, agent: &AgentState, harness: &HarnessConfig) -> Option<Duration> {
        if agent.is_launched_child() {
            return None;
        }
        let setting = match &agent.team {
            Some(team) => self.teams.0.get(team)?.keep_warm(agent.role.as_deref())?,
            None => crate::config::KeepWarmFields {
                max: self.profiles.setting(agent.profile.as_deref()?, |profile| {
                    profile
                        .keep_warm
                        .as_ref()
                        .and_then(|policy| policy.fields().max)
                }),
                while_clause: None,
            },
        };
        let horizon = match setting.max {
            Some(KeepWarm::For(max)) => Some(max),
            _ => setting
                .while_clause
                .as_ref()
                .and(harness.cache_keepalive_max),
        }?;
        harness.ping_lifetime(agent)?;
        if let Some(clause) = setting.while_clause
            && let Some(root) = agent.lane_path()
        {
            let expr = WhenExpr::parse(&[clause]).ok()?;
            let verdict = evaluate(
                &expr,
                Path::new(root),
                None,
                None,
                None,
                &WindowReadings::new(None, Timestamp::now()),
            );
            if !verdict.ok && verdict.readings.values().all(Option::is_some) {
                return None;
            }
        }
        Some(horizon)
    }

    /// The token floor at which this seat is compacted while idle (`None` is off): a TOML team
    /// role's override, else the first value up the seat's profile chain (a Markdown role's
    /// value lives on its `<team>.<role>` profile), else `harness.idle_compact`. A
    /// `rimz subagents` child never resolves one; an agent-launched peer resolves like any
    /// other seat.
    pub fn idle_compact_floor(&self, agent: &AgentState, harness: &HarnessConfig) -> Option<u64> {
        if agent.is_launched_child() {
            return None;
        }
        let role = agent.team.as_ref().and_then(|team| {
            let role = agent.role.as_deref()?;
            self.teams
                .0
                .get(team)?
                .roles
                .iter()
                .find(|binding| binding.role == role)?
                .idle_compact
        });
        let setting = role
            .or_else(|| {
                self.profiles
                    .setting(agent.profile.as_deref()?, |profile| profile.idle_compact)
            })
            .unwrap_or(harness.idle_compact);
        match setting {
            IdleCompact::Off => None,
            IdleCompact::Tokens(floor) => Some(floor),
        }
    }

    /// The seat's smart-compaction line, including a pane-backed child's launch namespace.
    pub fn smart_compact(
        &self,
        agent: &AgentState,
        harness: &HarnessConfig,
    ) -> Option<crate::store::message::AutoCompact> {
        crate::config::effective::SmartCompactSeat::from(agent).resolve(
            &self.teams,
            &self.profiles,
            &self.subagent_profiles,
            harness,
        )
    }

    /// The declared horizon when `holds` says keep-warm is holding the agent at `now`.
    pub fn holding(
        &self,
        agent: &AgentState,
        harness: &HarnessConfig,
        now: Timestamp,
    ) -> Option<Duration> {
        holds(agent, self.horizon(agent, harness), harness, now)
    }
}

/// Whether keep-warm is holding `agent` at `now` under a declared `horizon`: the one answer the
/// ping, idle compaction, and the card's held clock share. A hold is a horizon the pings can
/// still maintain, so it needs pings switched on, an admitted agent lifetime, a resting pingable
/// agent with no idle stop pending (resting in its durable lifecycle too, since only there
/// does a ping fold as no work), a cache still warm from the
/// last request (a cold cache is not pinged back), `harness.cache_keepalive_max` still permitting
/// the next ping, and the horizon counted from the agent's last real turn.
pub(crate) fn holds(
    agent: &AgentState,
    horizon: Option<Duration>,
    harness: &HarnessConfig,
    now: Timestamp,
) -> Option<Duration> {
    let horizon = horizon?;
    let ttl = harness.ping_lifetime(agent)?;
    let warm_until = agent.turn_ended_at?.checked_add(horizon).ok()?;
    let cache_warm = agent
        .last_request_at()
        .and_then(|request| request.checked_add(ttl).ok())
        .is_some_and(|cold_at| now < cold_at);
    (now < warm_until
        && cache_warm
        && agent.idle_stop.is_none()
        && harness.cache_keepalive
        && matches!(
            agent.effective_status(),
            AgentStatus::Idle | AgentStatus::Success | AgentStatus::Sleeping
        )
        && agent.rests_in_lifecycle()
        && pingable(agent)
        && !run_reached_cap(agent, harness, ttl))
    .then_some(horizon)
}

/// The exclusions every cache ping shares, whichever path qualified the agent.
fn pingable(agent: &AgentState) -> bool {
    !(agent.is_provider_subagent()
        || agent.agent_id.is_empty()
        || agent.compacting_since.is_some()
        || agent.compacted_since_prompt()
        || agent.budget_park.is_some()
        || agent.hibernation.is_some()
        || agent.is_awaiting_input())
}

impl CacheKeepaliveRequest {
    /// The agent still due this exact ping, and the keep-warm horizon holding it, if any.
    pub fn target<'a>(
        &self,
        snapshot: &'a SidebarSnapshot,
        config: &HarnessConfig,
        policy: &SeatPolicy,
        now: Timestamp,
    ) -> Option<(&'a AgentState, Option<Duration>)> {
        let agent = snapshot.agents.iter().find(|agent| {
            agent.kind == self.kind
                && agent.agent_id == self.agent_id
                && agent.last_request_at() == Some(self.anchor)
                && snapshot
                    .live_agent_pane(&self.kind, &self.agent_id)
                    .as_ref()
                    == Some(&self.pane_id)
        })?;
        let holding = policy.holding(agent, config, now);
        should_keepalive(agent, config, holding, now).then_some((agent, holding))
    }
}

/// The ping text; `limit` is the reached maximum when this is the run's last ping.
pub fn prompt(agent: &AgentState, now: Timestamp, limit: Option<Duration>) -> String {
    let mut text = "Cache keepalive, no action needed.".to_owned();
    if !agent.pending_waits.is_empty() {
        text.push_str(" Waiting on:");
    }
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
        .ping_lifetime(agent)?
        .checked_sub(config.prompt_cache_margin)?;
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
    policy: &SeatPolicy,
    mut spawn: impl FnMut(&CacheKeepaliveRequest),
) {
    let Ok(Some(_guard)) =
        crate::disk::lock::WorkspaceLock::try_acquire(&runtime.live_path("cache-keepalive.lock"))
    else {
        return;
    };
    for agent in snapshot.agents.iter() {
        let holding = policy.holding(agent, config, snapshot.now);
        if !should_keepalive(agent, config, holding, snapshot.now) {
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
    policy: &SeatPolicy,
) {
    if !config.cache_keepalive {
        return;
    }
    keepalive_agents_with(snapshot, runtime, config, policy, |request| {
        let args = crate::child_process::agent_helper_argv("cache-keepalive", request);
        if let Err(err) =
            crate::child_process::spawn_detached_rimz(runtime, args, "agent-cache-keepalive")
        {
            tracing::debug!(error = %err, "failed to spawn cache keepalive");
        }
    });
}

/// Sleeping agents and those held by keep-warm ([`holds`]) may qualify, subject to ping admission.
fn should_keepalive(
    agent: &AgentState,
    config: &HarnessConfig,
    holding: Option<Duration>,
    now: Timestamp,
) -> bool {
    if !config.cache_keepalive
        || !(agent.effective_status() == AgentStatus::Sleeping || holding.is_some())
        || !pingable(agent)
    {
        return false;
    }
    let (Some(ttl), Some(anchor)) = (config.ping_lifetime(agent), agent.last_request_at()) else {
        return false;
    };
    let Some(fire_after) = ttl.checked_sub(config.prompt_cache_margin) else {
        return false;
    };
    let Ok(idle) = Duration::try_from(now.duration_since(anchor)) else {
        return false;
    };
    idle >= fire_after && idle < ttl && !run_reached_cap(agent, config, ttl)
}

/// Whether the ping after the open keepalive run's last one, the request anchor a ping's close
/// moves, would start at or past the cap. Reads only stamps, so the producer and the helper's
/// recheck agree.
fn run_reached_cap(agent: &AgentState, config: &HarnessConfig, ttl: Duration) -> bool {
    let (Some(max), Some(since), Some(last_ping), Some(fire_after)) = (
        config.cache_keepalive_max,
        agent.keepalive_since,
        agent.last_request_at(),
        ttl.checked_sub(config.prompt_cache_margin),
    ) else {
        return false;
    };
    next_reaches_cap(since, last_ping, fire_after, max)
}

#[cfg(test)]
mod tests;
