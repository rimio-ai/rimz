//! Durable agent and model effort attribution for one room scope.
//!
//! Identity, tool calls, and compactions come from the audit rollup; tokens and
//! dollars come from provider transcripts through the shared price book; active
//! time comes from runtime sidecars and can become unavailable after GC.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use jiff::Timestamp;
use serde::Serialize;

use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::store::active_time;
use crate::store::paths::RuntimePaths;
use crate::transcript::{TranscriptEntry, TranscriptKind};

use super::{AgentState, pricing, spending};

pub use super::spending::EffortTokens as TokenSplit;

pub const ATTRIBUTION_SCHEMA: u8 = 3;
const SUBAGENT_TYPE_MAX_CHARS: usize = 24;

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
    pub asks_answered: u64,
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
    pub from_user: u64,
    pub from_teammates: u64,
    pub to_teammates: u64,
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
    pub asks_answered: u64,
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
    pub require_contribution: bool,
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

struct FoldedMember {
    channel: Option<String>,
    team: Option<String>,
    attribution: AttributionMember,
    opened_turn: bool,
}

pub fn build(request: AttributionRequest<'_>) -> Attribution {
    let folded = fold_seats(request.agents);
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
            let seat = split_seat_records(&records);
            let team = newest(&seat.identity).and_then(|agent| agent.team.clone());
            let opened_turn = seat
                .identity
                .iter()
                .any(|agent| agent.turn_started_at.is_some());
            let member = member(
                &records,
                &seat,
                &peer_representatives,
                &request,
                &active_records,
                &prices,
                &conversation_counts,
            );
            FoldedMember {
                channel: slot.channel,
                team,
                attribution: member,
                opened_turn,
            }
        })
        .collect::<Vec<_>>();
    credit_sent_messages(&mut members, request.transcript);
    members.retain(|member| {
        member.attribution.has_contribution()
            || (member.opened_turn && !request.require_contribution)
    });
    members.sort_by(|left, right| member_order(&left.attribution, &right.attribution));

    let mut by_team = BTreeMap::<String, Vec<AttributionMember>>::new();
    let mut other = Vec::new();
    for member in members {
        match member.team {
            Some(team) => by_team.entry(team).or_default().push(member.attribution),
            None => other.push(member.attribution),
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

/// Fold pane-backed children into the durable seat that launched them.
/// Orphaned children retain their own slot so their effort is never lost.
fn fold_seats<'a>(agents: &[&'a AgentState]) -> Vec<(SlotKey, Vec<&'a AgentState>)> {
    let parents = agents
        .iter()
        .copied()
        .filter(|agent| !agent.is_launched_child())
        .collect::<Vec<_>>();
    let children = agents
        .iter()
        .copied()
        .filter(|agent| agent.is_launched_child())
        .collect::<Vec<_>>();
    let mut slots = fold(&parents).into_iter().collect::<HashMap<_, _>>();
    for (child_slot, child_records) in fold(&children) {
        let parent = child_records.iter().find_map(|child| {
            parents
                .iter()
                .copied()
                .find(|parent| is_launched_child_of(child, parent))
        });
        let key = parent.map(slot).unwrap_or(child_slot);
        slots.entry(key).or_default().extend(child_records);
    }
    let mut slots = slots.into_iter().collect::<Vec<_>>();
    for (_, records) in &mut slots {
        records.sort_by_key(|agent| (agent.registered_at, agent.last_activity));
    }
    slots
}

fn is_launched_child_of(child: &AgentState, parent: &AgentState) -> bool {
    child.is_launched_child()
        && child.parent_agent_id.as_ref().is_some_and(|parent_id| {
            parent_id == &parent.agent_id || parent.launch_id.as_ref() == Some(parent_id)
        })
        && child
            .parent_agent_kind
            .as_ref()
            .is_none_or(|kind| kind == &parent.kind)
}

/// Lifetime records for one seat, including pane-backed children it launched.
pub fn slot_groups<'a>(agents: &[&'a AgentState]) -> Vec<Vec<&'a AgentState>> {
    fold_seats(agents)
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
    seat: &SeatRecords<'_>,
    peers: &[&AgentState],
    request: &AttributionRequest<'_>,
    active_records: &BTreeMap<(AgentKind, AgentSessionId), active_time::ActiveTimeRecord>,
    prices: &pricing::PriceBook,
    conversation_counts: &HashMap<(AgentKind, AgentSessionId), ConversationCounts>,
) -> AttributionMember {
    // Every slot is created only after its first record is inserted.
    let latest = newest(&seat.identity).expect("folded attribution slot has records");
    let mut effort = spending::slot_effort_breakdown(
        &seat
            .identity
            .iter()
            .map(|agent| spending::EffortSessionRef::from_state(agent))
            .collect::<Vec<_>>(),
        prices,
    );
    let launched_effort = fold(&seat.children)
        .into_iter()
        .map(|(_, child_records)| {
            let child_effort = spending::slot_effort(
                &child_records
                    .iter()
                    .map(|child| spending::EffortSessionRef::from_state(child))
                    .collect::<Vec<_>>(),
                prices,
            );
            effort.total.tokens.add_assign(child_effort.tokens);
            effort.total.cost_usd =
                spending::sum_optional_cost(effort.total.cost_usd, child_effort.cost_usd);
            (
                newest(&child_records).expect("folded child slot has records"),
                child_effort,
            )
        })
        .collect::<Vec<_>>();
    let messages = records
        .iter()
        .fold(MessageCounts::default(), |mut total, agent| {
            if let Some(counts) =
                conversation_counts.get(&(agent.kind.clone(), agent.agent_id.clone()))
            {
                total.from_user = total.from_user.saturating_add(counts.messages.from_user);
                total.from_teammates = total
                    .from_teammates
                    .saturating_add(counts.messages.from_teammates);
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
    let asks_answered = records.iter().fold(0u64, |total, agent| {
        total.saturating_add(
            conversation_counts
                .get(&(agent.kind.clone(), agent.agent_id.clone()))
                .map_or(0, |counts| counts.asks_answered),
        )
    });
    let subagents = subagent_stats(
        &seat.identity,
        request.subagents,
        &effort.subagents,
        &launched_effort,
    );
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
        presence: if seat.identity.iter().any(|agent| agent.ended_at.is_none()) {
            Presence::Live
        } else {
            Presence::Exited
        },
        me: request
            .me
            .is_some_and(|me| seat.identity.iter().any(|agent| &agent.agent_id == me)),
        launch_ordinal: latest.launch_ordinal,
        sessions: u32::try_from(seat.identity.len()).unwrap_or(u32::MAX),
        registered_at: seat
            .identity
            .iter()
            .filter_map(|agent| agent.registered_at)
            .min(),
        last_activity: records
            .iter()
            .map(|agent| agent.last_activity)
            .max()
            .unwrap_or(latest.last_activity),
        active_secs,
        asks,
        asks_answered,
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

struct SeatRecords<'a> {
    identity: Vec<&'a AgentState>,
    children: Vec<&'a AgentState>,
}

fn split_seat_records<'a>(records: &[&'a AgentState]) -> SeatRecords<'a> {
    let identity = records
        .iter()
        .copied()
        .filter(|agent| !agent.is_launched_child())
        .collect::<Vec<_>>();
    if identity.is_empty() {
        return SeatRecords {
            identity: records.to_vec(),
            children: Vec::new(),
        };
    }
    let children = records
        .iter()
        .copied()
        .filter(|agent| agent.is_launched_child())
        .collect();
    SeatRecords { identity, children }
}

#[derive(Clone, Copy, Debug, Default)]
struct ConversationCounts {
    asks: u64,
    asks_answered: u64,
    messages: MessageCounts,
}

fn conversation_counts(
    transcript: &[TranscriptEntry],
) -> HashMap<(AgentKind, AgentSessionId), ConversationCounts> {
    let answered = transcript
        .iter()
        .filter(|entry| entry.entry == TranscriptKind::Answer)
        .filter_map(|entry| entry.id.as_ref())
        .collect::<HashSet<_>>();
    let mut counts = HashMap::new();
    for entry in transcript {
        let counts = counts
            .entry((entry.kind.clone(), entry.agent_id.clone()))
            .or_insert_with(ConversationCounts::default);
        match entry.entry {
            TranscriptKind::Prompt => {
                if entry.from.as_deref() != Some("rimz") {
                    counts.messages.from_user = counts.messages.from_user.saturating_add(1);
                }
            }
            TranscriptKind::Message => {
                counts.messages.from_teammates = counts.messages.from_teammates.saturating_add(1);
            }
            TranscriptKind::Ask => {
                counts.asks = counts.asks.saturating_add(1);
                if entry.id.as_ref().is_some_and(|id| answered.contains(id)) {
                    counts.asks_answered = counts.asks_answered.saturating_add(1);
                }
            }
            TranscriptKind::Assistant | TranscriptKind::Answer | TranscriptKind::Error => {}
        }
    }
    counts
}

fn credit_sent_messages(members: &mut [FoldedMember], transcript: &[TranscriptEntry]) {
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
        if let Some(member) = members.iter_mut().find(|member| {
            member.attribution.handle == base && member.channel.as_deref() == sender_channel
        }) {
            member.attribution.messages.to_teammates =
                member.attribution.messages.to_teammates.saturating_add(1);
        }
    }
}

fn subagent_stats(
    records: &[&AgentState],
    subagents: &[&AgentState],
    spend: &BTreeMap<String, spending::SlotEffort>,
    launched: &[(&AgentState, spending::SlotEffort)],
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
            subagent_type(child.task.as_deref()),
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
    for (child, effort) in launched {
        let task = subagent_type(child.profile.as_deref());
        let stat = grouped.entry(task.clone()).or_insert_with(|| SubagentStat {
            task,
            count: 0,
            cost_usd: None,
        });
        stat.count = stat.count.saturating_add(1);
        stat.cost_usd = spending::sum_optional_cost(stat.cost_usd, effort.cost_usd);
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

fn subagent_type(task: Option<&str>) -> Option<String> {
    let task = task?.trim();
    (!task.is_empty()
        && task.chars().count() <= SUBAGENT_TYPE_MAX_CHARS
        && task
            .chars()
            .all(|character| !character.is_whitespace() && !character.is_control()))
    .then(|| task.to_owned())
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
        totals.asks_answered = totals.asks_answered.saturating_add(member.asks_answered);
        totals.tool_calls = totals.tool_calls.saturating_add(member.tool_calls);
        totals.compactions = totals.compactions.saturating_add(member.compactions);
        totals.messages.from_user = totals
            .messages
            .from_user
            .saturating_add(member.messages.from_user);
        totals.messages.from_teammates = totals
            .messages
            .from_teammates
            .saturating_add(member.messages.from_teammates);
        totals.messages.to_teammates = totals
            .messages
            .to_teammates
            .saturating_add(member.messages.to_teammates);
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
