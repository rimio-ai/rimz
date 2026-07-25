//! Durable agent and model effort attribution for one room scope.
//!
//! Identity, tool calls, and compactions come from the audit rollup; tokens and
//! dollars come from provider transcripts through the shared price book; active
//! time comes from runtime sidecars and can become unavailable after GC.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use jiff::Timestamp;
use serde::Serialize;

use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::store::active_time;
use crate::store::paths::RuntimePaths;

use super::{AgentState, pricing, spending};

pub const ATTRIBUTION_SCHEMA: u8 = 1;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Attribution {
    pub schema: u8,
    pub generated_at: Timestamp,
    pub rimz_version: String,
    pub scope: AttributionScope,
    pub groups: Vec<AttributionGroup>,
    pub totals: EffortTotals,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AttributionScope {
    pub selector: Option<String>,
    pub channel: Option<String>,
    pub branch: Option<String>,
    pub worktree: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AttributionGroup {
    pub team: Option<TeamRef>,
    pub totals: EffortTotals,
    pub members: Vec<AttributionMember>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TeamRef {
    pub name: String,
    pub roles: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AttributionMember {
    pub handle: String,
    pub role: Option<String>,
    pub name: Option<String>,
    pub kind: AgentKind,
    pub provider: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub presence: Presence,
    pub me: bool,
    pub launch_ordinal: Option<u32>,
    pub sessions: u32,
    pub registered_at: Option<Timestamp>,
    pub last_activity: Timestamp,
    pub active_secs: Option<u64>,
    pub tool_calls: u64,
    pub compactions: u32,
    pub tokens: TokenSplit,
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TokenSplit {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

impl TokenSplit {
    fn add_assign(&mut self, other: Self) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct EffortTotals {
    pub agents: u32,
    pub active_secs: Option<u64>,
    pub wall_clock_secs: u64,
    pub cost_usd: Option<f64>,
    pub tool_calls: u64,
    pub compactions: u32,
    pub tokens: TokenSplit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Presence {
    Live,
    Exited,
}

pub struct AttributionRequest<'a> {
    pub agents: &'a [&'a AgentState],
    pub peers: &'a [&'a AgentState],
    pub me: Option<&'a AgentSessionId>,
    pub runtime: &'a RuntimePaths,
    pub active_grace_secs: u32,
    pub scope: AttributionScope,
    pub now: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Slot {
    Role { team: String, role: String },
    Cohort { group: String, ordinal: u32 },
    Name(String),
    Pane(PaneId),
    Session(AgentSessionId),
}

type SlotKey = (AgentKind, Slot);

pub fn build(request: AttributionRequest<'_>) -> Attribution {
    let folded = fold(request.agents);
    let peer_representatives = representatives(request.peers);
    let active_records = active_time::read_for_keys(
        request.runtime,
        request
            .agents
            .iter()
            .map(|agent| (agent.kind.as_str(), agent.agent_id.as_str())),
    )
    .into_iter()
    .map(|record| ((record.kind.clone(), record.agent_id.clone()), record))
    .collect::<BTreeMap<_, _>>();
    let prices = pricing::cached_book(&request.runtime.shared_pricing_cache_path());

    let mut members = folded
        .into_iter()
        .map(|(_, records)| {
            let team = newest(&records).and_then(|agent| agent.team.clone());
            let member = member(
                &records,
                &peer_representatives,
                request.me,
                request.active_grace_secs,
                request.now,
                &active_records,
                &prices,
            );
            (team, member)
        })
        .collect::<Vec<_>>();
    members.sort_by(|left, right| member_order(&left.1, &right.1));

    let mut by_team = BTreeMap::<String, Vec<AttributionMember>>::new();
    let mut other = Vec::new();
    for (team, member) in members {
        match team {
            Some(team) => by_team.entry(team).or_default().push(member),
            None => other.push(member),
        }
    }

    let mut groups = by_team
        .into_iter()
        .map(|(name, members)| {
            let roles = members
                .iter()
                .filter_map(|member| member.role.clone())
                .fold(Vec::new(), |mut roles, role| {
                    if !roles.contains(&role) {
                        roles.push(role);
                    }
                    roles
                });
            AttributionGroup {
                team: Some(TeamRef { name, roles }),
                totals: totals(&members),
                members,
            }
        })
        .collect::<Vec<_>>();
    if !other.is_empty() {
        groups.push(AttributionGroup {
            team: None,
            totals: totals(&other),
            members: other,
        });
    }

    let all_members = groups
        .iter()
        .flat_map(|group| group.members.iter())
        .collect::<Vec<_>>();
    let totals = totals_from_refs(&all_members);
    Attribution {
        schema: ATTRIBUTION_SCHEMA,
        generated_at: request.now,
        rimz_version: crate::build_id::VERSION.to_owned(),
        scope: request.scope,
        groups,
        totals,
    }
}

fn slot(agent: &AgentState) -> SlotKey {
    let slot = match (
        agent.team.as_deref().filter(|value| !value.is_empty()),
        agent.role.as_deref().filter(|value| !value.is_empty()),
    ) {
        (Some(team), Some(role)) => Slot::Role {
            team: team.to_owned(),
            role: role.to_owned(),
        },
        _ => match (
            agent
                .launch_group
                .as_deref()
                .filter(|value| !value.is_empty()),
            agent.launch_ordinal,
        ) {
            (Some(group), Some(ordinal)) => Slot::Cohort {
                group: group.to_owned(),
                ordinal,
            },
            _ if agent.name_explicit => agent
                .name
                .clone()
                .map(Slot::Name)
                .unwrap_or_else(|| fallback_slot(agent)),
            _ => fallback_slot(agent),
        },
    };
    (agent.kind.clone(), slot)
}

fn fallback_slot(agent: &AgentState) -> Slot {
    agent
        .pane
        .as_ref()
        .map(|pane| Slot::Pane(pane.pane_id.clone()))
        .unwrap_or_else(|| Slot::Session(agent.agent_id.clone()))
}

fn fold<'a>(agents: &[&'a AgentState]) -> Vec<(SlotKey, Vec<&'a AgentState>)> {
    let mut slots = HashMap::<SlotKey, Vec<&AgentState>>::new();
    for &agent in agents {
        slots.entry(slot(agent)).or_default().push(agent);
    }
    let mut slots = slots.into_iter().collect::<Vec<_>>();
    for (_, records) in &mut slots {
        records.sort_by_key(|agent| (agent.registered_at, agent.last_activity));
    }
    slots
}

fn representatives<'a>(peers: &[&'a AgentState]) -> Vec<&'a AgentState> {
    fold(peers)
        .into_iter()
        .filter_map(|(_, records)| newest(&records))
        .collect()
}

fn member(
    records: &[&AgentState],
    peers: &[&AgentState],
    me: Option<&AgentSessionId>,
    active_grace_secs: u32,
    now: Timestamp,
    active_records: &BTreeMap<(AgentKind, AgentSessionId), active_time::ActiveTimeRecord>,
    prices: &pricing::PriceBook,
) -> AttributionMember {
    // Every slot is created by `fold` only after its first record is inserted.
    let latest = newest(records).expect("folded attribution slot has records");
    let (tokens, cost_usd) = records.iter().fold(
        (TokenSplit::default(), None),
        |(mut tokens, cost), agent| {
            let (session_tokens, session_cost) = session_effort(agent, prices);
            tokens.add_assign(session_tokens);
            (tokens, sum_optional_cost(cost, session_cost))
        },
    );
    let active_secs = records
        .iter()
        .filter_map(|agent| {
            active_records
                .get(&(agent.kind.clone(), agent.agent_id.clone()))
                .map(|record| record.display_secs(now, active_grace_secs))
        })
        .reduce(u64::saturating_add);
    AttributionMember {
        handle: crate::harness::target::agent_handle(latest, peers, false),
        role: latest.role.clone(),
        name: latest.name.clone(),
        kind: latest.kind.clone(),
        provider: super::spec_by_kind(latest.kind.as_str())
            .map(|spec| spec.display_name.to_owned())
            .unwrap_or_else(|| latest.kind.to_string()),
        model: latest.model.clone(),
        effort: latest.effort.clone(),
        presence: if records.iter().any(|agent| agent.ended_at.is_none()) {
            Presence::Live
        } else {
            Presence::Exited
        },
        me: me.is_some_and(|me| records.iter().any(|agent| &agent.agent_id == me)),
        launch_ordinal: latest.launch_ordinal,
        sessions: u32::try_from(records.len()).unwrap_or(u32::MAX),
        registered_at: records.iter().filter_map(|agent| agent.registered_at).min(),
        last_activity: records
            .iter()
            .map(|agent| agent.last_activity)
            .max()
            .unwrap_or(latest.last_activity),
        active_secs,
        tool_calls: records.iter().fold(0u64, |total, agent| {
            total.saturating_add(
                agent
                    .tool_calls
                    .values()
                    .map(|count| u64::from(*count))
                    .sum::<u64>(),
            )
        }),
        compactions: records.iter().fold(0u32, |total, agent| {
            total.saturating_add(agent.compaction_count)
        }),
        tokens,
        cost_usd,
    }
}

fn newest<'a>(records: &[&'a AgentState]) -> Option<&'a AgentState> {
    records
        .iter()
        .copied()
        .max_by_key(|agent| (agent.last_activity, agent.registered_at))
}

fn session_effort(agent: &AgentState, prices: &pricing::PriceBook) -> (TokenSplit, Option<f64>) {
    let Some(adapter) = super::find_definition(agent.kind.as_str()) else {
        return (TokenSplit::default(), None);
    };
    let prior_path = agent
        .transcript_path
        .as_deref()
        .filter(|path| !path.is_empty())
        .map(Path::new);
    let Some(path) = adapter.session_transcript(agent.agent_id.as_str(), prior_path) else {
        return (TokenSplit::default(), None);
    };
    let parsed = adapter.parse_spend(&path, None, prices);
    let entries = spending::session_entries(&parsed.entries, agent.agent_id.as_str());
    if entries.is_empty() {
        return (TokenSplit::default(), None);
    }
    entries.into_iter().fold(
        (TokenSplit::default(), None),
        |(mut tokens, cost), entry| {
            tokens.add_assign(TokenSplit {
                input: entry.input,
                output: entry.output,
                cache_write: entry.cache_write,
                cache_read: entry.cache_read,
            });
            let entry_cost =
                (entry.cost_usd.is_finite() && entry.cost_usd > 0.0).then_some(entry.cost_usd);
            (tokens, sum_optional_cost(cost, entry_cost))
        },
    )
}

fn sum_optional_cost(total: Option<f64>, value: Option<f64>) -> Option<f64> {
    match (total, value) {
        (Some(total), Some(value)) => Some(total + value),
        (Some(total), None) => Some(total),
        (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn member_order(left: &AttributionMember, right: &AttributionMember) -> Ordering {
    left.launch_ordinal
        .unwrap_or(u32::MAX)
        .cmp(&right.launch_ordinal.unwrap_or(u32::MAX))
        .then_with(|| {
            left.registered_at
                .unwrap_or(Timestamp::MAX)
                .cmp(&right.registered_at.unwrap_or(Timestamp::MAX))
        })
        .then_with(|| left.handle.cmp(&right.handle))
}

fn totals(members: &[AttributionMember]) -> EffortTotals {
    totals_from_refs(&members.iter().collect::<Vec<_>>())
}

fn totals_from_refs(members: &[&AttributionMember]) -> EffortTotals {
    let mut totals = EffortTotals {
        agents: u32::try_from(members.len()).unwrap_or(u32::MAX),
        ..EffortTotals::default()
    };
    for member in members {
        totals.active_secs = match (totals.active_secs, member.active_secs) {
            (Some(total), Some(value)) => Some(total.saturating_add(value)),
            (Some(total), None) => Some(total),
            (None, Some(value)) => Some(value),
            (None, None) => None,
        };
        totals.cost_usd = sum_optional_cost(totals.cost_usd, member.cost_usd);
        totals.tool_calls = totals.tool_calls.saturating_add(member.tool_calls);
        totals.compactions = totals.compactions.saturating_add(member.compactions);
        totals.tokens.add_assign(member.tokens);
    }
    let started = members
        .iter()
        .filter_map(|member| member.registered_at)
        .min();
    let ended = members.iter().map(|member| member.last_activity).max();
    totals.wall_clock_secs = started
        .zip(ended)
        .map(|(started, ended)| ended.duration_since(started).as_secs().max(0) as u64)
        .unwrap_or(0);
    totals
}

#[cfg(test)]
mod tests;
