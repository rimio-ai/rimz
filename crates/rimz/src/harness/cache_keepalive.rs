//! Best-effort prompt-cache pings for sleeping agents, and for idle agents a keep-warm horizon holds. Durable delivery belongs to the helper.

use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::schedule::when::{WhenExpr, WindowReadings, evaluate};
use crate::RuntimePaths;
use crate::agents::{AgentState, AgentStatus, PendingWaitTrigger};
use crate::config::{HarnessConfig, MachineConfig, ProfilesConfig, TeamsConfig};
use crate::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
use crate::store::snapshot::SidebarSnapshot;
use crate::utils::time::{format_duration_coarse, format_duration_compact};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheKeepaliveRequest {
    pub workspace_id: WorkspaceId,
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub pane_id: PaneId,
    /// The claimed request's [`AgentState::request_key`], which a switch of clock source
    /// does not move.
    pub anchor: Timestamp,
    pub label: String,
    /// The fire 30 s before the cache expires, which delivers past the typing hold.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub forced: bool,
}

/// How long before `last_request_at + TTL` a ping still unsent fires past the typing hold. It
/// covers the measured worst case from the producer's decision to the provider's request
/// (a fold up to 6 s old, the 1 s producer rest and fold wait, 4 s from enqueue to paste and
/// 7.5 s from paste to request) with about 10 s to spare.
const FORCED_LEAD: Duration = Duration::from_secs(30);

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

    /// The role-over-team or solo profile maximum, falling back to the harness default and
    /// admitted by the TTL floor and current stage condition. Unknown board readings permit
    /// a hold; a while with no resolved maximum takes the ping cap. A `rimz subagents` child
    /// has no configured horizon; an explicit session override also applies to a child.
    pub fn horizon(&self, agent: &AgentState, harness: &HarnessConfig) -> Option<Duration> {
        let (horizon, while_clause) = match agent.tune.keep_warm() {
            Some(horizon) => (horizon?, None),
            None => crate::config::effective::KeepWarmSeat::from(agent).declared_horizon(
                &self.teams,
                &self.profiles,
                harness,
            )?,
        };
        harness.ping_lifetime(agent)?;
        if let Some(clause) = while_clause
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
    /// `rimz subagents` child has no configured floor, but honours an explicit session override.
    pub fn idle_compact_floor(&self, agent: &AgentState, harness: &HarnessConfig) -> Option<u64> {
        agent.tune.idle_compact().unwrap_or_else(|| {
            crate::config::effective::IdleCompactSeat::from(agent).resolve(
                &self.teams,
                &self.profiles,
                harness,
            )
        })
    }

    /// The seat's smart-compaction line, including a pane-backed child's launch namespace.
    pub fn smart_compact(
        &self,
        agent: &AgentState,
        harness: &HarnessConfig,
    ) -> Option<crate::store::message::AutoCompact> {
        agent.tune.smart_compact().unwrap_or_else(|| {
            crate::config::effective::SmartCompactSeat::from(agent).resolve(
                &self.teams,
                &self.profiles,
                &self.subagent_profiles,
                harness,
            )
        })
    }

    /// Declared configuration without session overrides, TTL admission, or board evaluation.
    pub fn configured_values(
        &self,
        agent: &AgentState,
        harness: &HarnessConfig,
    ) -> ConfiguredValues {
        let keep_warm = crate::config::effective::KeepWarmSeat::from(agent);
        let idle_compact = crate::config::effective::IdleCompactSeat::from(agent);
        let smart_compact = crate::config::effective::SmartCompactSeat::from(agent);
        ConfiguredValues {
            keep_warm: keep_warm.declared_horizon(&self.teams, &self.profiles, harness),
            idle_compact: idle_compact.resolve(&self.teams, &self.profiles, harness),
            smart_compact: smart_compact.resolve(
                &self.teams,
                &self.profiles,
                &self.subagent_profiles,
                harness,
            ),
        }
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

/// The configured values a session's runtime overrides replace.
pub struct ConfiguredValues {
    pub keep_warm: Option<(Duration, Option<String>)>,
    pub idle_compact: Option<u64>,
    pub smart_compact: Option<crate::store::message::AutoCompact>,
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
    let cache_warm = harness
        .cache_cold_at(agent)
        .is_some_and(|cold_at| now < cold_at);
    (now < warm_until
        && cache_warm
        && agent.idle_stop.is_none()
        && harness.cache_keepalive
        && matches!(
            agent.effective_status(now),
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
    /// The agent still due this exact ping, and the keep-warm horizon holding it, if any. A
    /// row whose ping turn is open is not: another fire's ping is in flight for this anchor.
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
                && agent.request_key() == Some(self.anchor)
                && !agent.ping_turn
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
        // A ping in flight already answers for this anchor.
        if agent.ping_turn || !should_keepalive(agent, config, holding, snapshot.now) {
            continue;
        }
        let forced = in_forced_window(agent, config, snapshot.now);
        // The anchor names the request, not its instant: the record and the estimate of one
        // request differ, and a switch of source must neither refire nor drop the claim.
        let (Some(anchor), Some(pane_id)) = (
            agent.request_key(),
            snapshot.live_agent_pane(&agent.kind, &agent.agent_id),
        ) else {
            continue;
        };
        // Each fire claims its own file: a binary that knows only the polite claim never reads
        // the forced one, so neither refires the other.
        let path = runtime.live_path("cache-keepalive").join(format!(
            "{}{}.json",
            crate::store::sidecar::digest(agent.kind.as_str(), agent.agent_id.as_str()),
            if forced { ".forced" } else { "" }
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
            forced,
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
        || !(agent.effective_status(now) == AgentStatus::Sleeping || holding.is_some())
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

/// Whether a due ping is within [`FORCED_LEAD`] of its cache's expiry, so it fires past the
/// typing hold.
fn in_forced_window(agent: &AgentState, config: &HarnessConfig, now: Timestamp) -> bool {
    config
        .cache_cold_at(agent)
        .and_then(|cold_at| cold_at.checked_sub(FORCED_LEAD).ok())
        .is_some_and(|forced_at| now >= forced_at)
}

/// Whether the ping after the open keepalive run's last one, the request anchor a closed ping
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
