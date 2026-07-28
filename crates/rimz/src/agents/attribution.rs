//! Durable agent and model effort attribution for one room scope.
//!
//! Identity, tool calls, and compactions come from the audit rollup; tokens and
//! dollars come from provider transcripts through the shared price book; active
//! time comes from runtime sidecars and can become unavailable after GC.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};

use jiff::Timestamp;
use serde::Serialize;

use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::store::active_time;
use crate::store::paths::RuntimePaths;
use crate::transcript::{TranscriptEntry, TranscriptKind};

use super::{AgentState, pricing, spending};

pub use super::spending::EffortTokens as TokenSplit;

pub const ATTRIBUTION_SCHEMA: u8 = 2;

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
    pub asks: u64,
    pub tool_calls: u64,
    pub compactions: u32,
    pub messages: MessageCounts,
    pub tokens: TokenSplit,
    pub cost_usd: Option<f64>,
    pub subagents: Vec<SubagentStat>,
}

impl AttributionMember {
    fn has_contribution(&self) -> bool {
        self.active_secs.unwrap_or(0) > 0
            || self.asks > 0
            || self.tool_calls > 0
            || self.compactions > 0
            || self.messages != MessageCounts::default()
            || self.tokens != TokenSplit::default()
            || self.cost_usd.is_some()
            || !self.subagents.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct MessageCounts {
    pub user: u64,
    pub agent: u64,
    pub sent: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SubagentStat {
    pub task: Option<String>,
    pub count: u32,
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct EffortTotals {
    pub agents: u32,
    pub active_secs: Option<u64>,
    pub wall_clock_secs: u64,
    pub cost_usd: Option<f64>,
    pub asks: u64,
    pub tool_calls: u64,
    pub compactions: u32,
    pub messages: MessageCounts,
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
    pub subagents: &'a [&'a AgentState],
    pub transcript: &'a [TranscriptEntry],
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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SlotKey {
    channel: Option<String>,
    kind: AgentKind,
    slot: Slot,
}

pub fn build(request: AttributionRequest<'_>) -> Attribution {
    let folded = fold(request.agents);
    let peer_representatives = representatives(request.peers);
    let conversation_counts = conversation_counts(request.transcript);
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
        .map(|(slot, records)| {
            let team = newest(&records).and_then(|agent| agent.team.clone());
            let opened_turn = records.iter().any(|agent| agent.turn_started_at.is_some());
            let member = member(
                &records,
                &peer_representatives,
                &request,
                &active_records,
                &prices,
                &conversation_counts,
            );
            (slot.channel, team, member, opened_turn)
        })
        .collect::<Vec<_>>();
    credit_sent_messages(&mut members, request.transcript);
    members.retain(|(_, _, member, opened_turn)| *opened_turn || member.has_contribution());
    members.sort_by(|left, right| member_order(&left.2, &right.2));

    let mut by_team = BTreeMap::<String, Vec<AttributionMember>>::new();
    let mut other = Vec::new();
    for (_, team, member, _) in members {
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
    SlotKey {
        channel: crate::harness::target::agent_channel(agent),
        kind: agent.kind.clone(),
        slot,
    }
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

pub fn slot_groups<'a>(agents: &[&'a AgentState]) -> Vec<Vec<&'a AgentState>> {
    fold(agents)
        .into_iter()
        .map(|(_, records)| records)
        .collect()
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
    request: &AttributionRequest<'_>,
    active_records: &BTreeMap<(AgentKind, AgentSessionId), active_time::ActiveTimeRecord>,
    prices: &pricing::PriceBook,
    conversation_counts: &HashMap<(AgentKind, AgentSessionId), ConversationCounts>,
) -> AttributionMember {
    // Every slot is created by `fold` only after its first record is inserted.
    let latest = newest(records).expect("folded attribution slot has records");
    let effort = spending::slot_effort_breakdown(
        &records
            .iter()
            .map(|agent| spending::EffortSessionRef::from_state(agent))
            .collect::<Vec<_>>(),
        prices,
    );
    let messages = records
        .iter()
        .fold(MessageCounts::default(), |mut total, agent| {
            if let Some(counts) =
                conversation_counts.get(&(agent.kind.clone(), agent.agent_id.clone()))
            {
                total.user = total.user.saturating_add(counts.messages.user);
                total.agent = total.agent.saturating_add(counts.messages.agent);
            }
            total
        });
    let asks = records.iter().fold(0u64, |total, agent| {
        total.saturating_add(
            conversation_counts
                .get(&(agent.kind.clone(), agent.agent_id.clone()))
                .map_or(0, |counts| counts.asks),
        )
    });
    let subagents = subagent_stats(records, request.subagents, &effort.subagents);
    let active_secs = records
        .iter()
        .filter_map(|agent| {
            active_records
                .get(&(agent.kind.clone(), agent.agent_id.clone()))
                .map(|record| record.display_secs(request.now, request.active_grace_secs))
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
        me: request
            .me
            .is_some_and(|me| records.iter().any(|agent| &agent.agent_id == me)),
        launch_ordinal: latest.launch_ordinal,
        sessions: u32::try_from(records.len()).unwrap_or(u32::MAX),
        registered_at: records.iter().filter_map(|agent| agent.registered_at).min(),
        last_activity: records
            .iter()
            .map(|agent| agent.last_activity)
            .max()
            .unwrap_or(latest.last_activity),
        active_secs,
        asks,
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
        messages,
        tokens: effort.total.tokens,
        cost_usd: effort.total.cost_usd,
        subagents,
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ConversationCounts {
    asks: u64,
    messages: MessageCounts,
}

fn conversation_counts(
    transcript: &[TranscriptEntry],
) -> HashMap<(AgentKind, AgentSessionId), ConversationCounts> {
    let mut counts = HashMap::new();
    for entry in transcript {
        let counts = counts
            .entry((entry.kind.clone(), entry.agent_id.clone()))
            .or_insert_with(ConversationCounts::default);
        match entry.entry {
            TranscriptKind::Prompt => {
                counts.messages.user = counts.messages.user.saturating_add(1);
            }
            TranscriptKind::Message => {
                counts.messages.agent = counts.messages.agent.saturating_add(1);
            }
            TranscriptKind::Ask => {
                counts.asks = counts.asks.saturating_add(1);
            }
            TranscriptKind::Assistant | TranscriptKind::Answer | TranscriptKind::Error => {}
        }
    }
    counts
}

fn credit_sent_messages(
    members: &mut [(Option<String>, Option<String>, AttributionMember, bool)],
    transcript: &[TranscriptEntry],
) {
    for entry in transcript
        .iter()
        .filter(|entry| entry.entry == TranscriptKind::Message)
    {
        let Some(from) = entry.from.as_deref() else {
            continue;
        };
        let (base, explicit_channel) = from
            .split_once('#')
            .map_or((from, None), |(base, channel)| (base, Some(channel)));
        let sender_channel = explicit_channel.or(entry.channel.as_deref());
        if let Some((_, _, member, _)) = members.iter_mut().find(|(channel, _, member, _)| {
            member
                .handle
                .split_once('#')
                .map_or(member.handle.as_str(), |(base, _)| base)
                == base
                && channel.as_deref() == sender_channel
        }) {
            member.messages.sent = member.messages.sent.saturating_add(1);
        }
    }
}

fn subagent_stats(
    records: &[&AgentState],
    subagents: &[&AgentState],
    spend: &BTreeMap<String, spending::SlotEffort>,
) -> Vec<SubagentStat> {
    let mut children = BTreeMap::<String, Option<String>>::new();
    for child in subagents.iter().copied().filter(|child| {
        records.iter().any(|parent| {
            child.parent_agent_id.as_ref() == Some(&parent.agent_id)
                && child
                    .parent_agent_kind
                    .as_ref()
                    .is_none_or(|kind| kind == &parent.kind)
        })
    }) {
        children.insert(
            child.agent_id.to_string(),
            child
                .task
                .as_deref()
                .map(str::trim)
                .filter(|task| !task.is_empty())
                .map(ToOwned::to_owned),
        );
    }
    for child_id in spend.keys() {
        children.entry(child_id.clone()).or_default();
    }

    let mut grouped = BTreeMap::<Option<String>, SubagentStat>::new();
    for (child_id, task) in children {
        let stat = grouped.entry(task.clone()).or_insert_with(|| SubagentStat {
            task,
            count: 0,
            cost_usd: None,
        });
        stat.count = stat.count.saturating_add(1);
        stat.cost_usd = spending::sum_optional_cost(
            stat.cost_usd,
            spend.get(&child_id).and_then(|effort| effort.cost_usd),
        );
    }
    let mut stats = grouped.into_values().collect::<Vec<_>>();
    stats.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| match (&left.task, &right.task) {
                (Some(left), Some(right)) => left.cmp(right),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            })
    });
    stats
}

fn newest<'a>(records: &[&'a AgentState]) -> Option<&'a AgentState> {
    records
        .iter()
        .copied()
        .max_by_key(|agent| (agent.last_activity, agent.registered_at))
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
        totals.cost_usd = spending::sum_optional_cost(totals.cost_usd, member.cost_usd);
        totals.asks = totals.asks.saturating_add(member.asks);
        totals.tool_calls = totals.tool_calls.saturating_add(member.tool_calls);
        totals.compactions = totals.compactions.saturating_add(member.compactions);
        totals.messages.user = totals.messages.user.saturating_add(member.messages.user);
        totals.messages.agent = totals.messages.agent.saturating_add(member.messages.agent);
        totals.messages.sent = totals.messages.sent.saturating_add(member.messages.sent);
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
