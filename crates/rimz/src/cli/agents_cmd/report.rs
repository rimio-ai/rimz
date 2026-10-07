use std::collections::BTreeMap;

use jiff::Timestamp;
use serde::Serialize;

use crate::cli::render;
use rimz::agents::PermissionMode;
use rimz::agents::{
    AgentCardRef, AgentState, AgentStatus, ContextSeverity, OpenAsk, TurnErrorClass, TurnPhase,
};
use rimz::ids::{AgentKind, AgentSessionId, LoginKey, PaneId};
use rimz::store::snapshot::{
    AgentCard, SidebarRow, SidebarSnapshot, SubAgentTokens, WorktreeCi, WorktreePrState,
    group_live_agents_by_worktree,
};
#[cfg(test)]
use rimz::store::snapshot::{PaneAgent, RowCard, SidebarSubAgent};

pub(super) const AGENT_LIST_SCHEMA: u8 = 1;

#[derive(Clone, Debug, Serialize)]
pub(super) struct AgentListReport {
    pub schema: u8,
    pub agents: Vec<AgentReportEntry>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct AgentReportEntry {
    pub id: AgentSessionId,
    pub kind: AgentKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login: Option<LoginKey>,
    pub handle: String,
    pub name: Option<String>,
    pub name_explicit: bool,
    pub profile: Option<String>,
    pub role: Option<String>,
    pub team: Option<String>,
    pub mode: Option<PermissionMode>,
    pub launch_warnings: Vec<String>,
    pub me: bool,
    pub status: AgentStatus,
    pub pending_waits: Vec<rimz::agents::state::PendingWait>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_stop: Option<rimz::agents::PendingIdleStop>,
    pub background_shells: Vec<rimz::agents::BackgroundShell>,
    pub phase: TurnPhase,
    pub turn_error: Option<TurnErrorReport>,
    pub ask: Option<AskReport>,
    pub unread: bool,
    pub attention_score: u32,
    pub description: Option<String>,
    pub model: ModelReport,
    pub context: ContextReport,
    pub stats: StatsReport,
    pub timeline: TimelineReport,
    pub placement: PlacementReport,
    pub budget: BudgetReport,
    pub sub_agents: Vec<SubAgentReport>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct TurnErrorReport {
    pub class: TurnErrorClass,
    pub label: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct AskReport {
    pub id: rimz::ids::AskId,
    pub kind: rimz::agents::AskKind,
    pub detail: Option<String>,
    pub native_key: Option<String>,
    pub since: Timestamp,
}

impl From<&OpenAsk> for AskReport {
    fn from(ask: &OpenAsk) -> Self {
        Self {
            id: ask.id.clone(),
            kind: ask.kind,
            detail: ask.detail.clone(),
            native_key: ask.native_key.clone(),
            since: ask.since,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct ModelReport {
    pub id: Option<String>,
    pub effort: Option<String>,
    pub label: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct ContextReport {
    pub fill_pct: Option<u8>,
    pub used_tokens: Option<u64>,
    pub window: Option<u64>,
    pub severity: Option<ContextSeverity>,
    pub compactions: u32,
    pub compacting: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct StatsReport {
    pub total_tokens: Option<u64>,
    pub fresh_input_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub active_secs: Option<u64>,
    pub tool_calls: BTreeMap<String, u32>,
    pub tool_repeat: Option<rimz::agent_activity::ToolRepeat>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct TimelineReport {
    pub registered_at: Option<Timestamp>,
    pub turn_started_at: Option<Timestamp>,
    pub last_activity: Timestamp,
    pub last_seen: Timestamp,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct PlacementReport {
    #[serde(skip)]
    pub lane_label: Option<String>,
    pub channel: Option<String>,
    pub worktree: Option<String>,
    pub branch: Option<String>,
    pub pane: Option<String>,
    pub pr: Option<PrInfo>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(super) struct PrInfo {
    pub number: Option<u64>,
    pub state: WorktreePrState,
    pub ci: Option<WorktreeCi>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct BudgetReport {
    pub cap: Option<String>,
    pub spent_usd: Option<f64>,
    pub parked: bool,
    pub park: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct SubAgentReport {
    pub id: String,
    pub name: String,
    pub status: AgentStatus,
    pub phase: TurnPhase,
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<SubAgentTokens>,
    pub elapsed_secs: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ReportOverrides<'a> {
    pub runtime: Option<&'a rimz::RuntimePaths>,
    pub effort: Option<rimz::agents::spending::SlotEffort>,
    pub active_secs: Option<u64>,
    pub budget_cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct SelfIdentity {
    pane: Option<PaneId>,
    kind: Option<AgentKind>,
    name: Option<String>,
    profile: Option<String>,
    role: Option<String>,
}

impl SelfIdentity {
    pub fn from_env() -> Self {
        Self {
            pane: rimz::mux::ambient_pane_id(),
            kind: env_string(rimz::harness::launch::ENV_AGENT_KIND).map(AgentKind::new_unchecked),
            name: env_string(rimz::harness::launch::ENV_AGENT_NAME),
            profile: env_string(rimz::harness::launch::ENV_AGENT_PROFILE),
            role: env_string(rimz::harness::launch::ENV_AGENT_ROLE),
        }
    }

    pub fn resolve(&self, snapshot: &SidebarSnapshot) -> Option<AgentSessionId> {
        if let Some(pane) = self.pane.as_ref()
            && let Some(agent_id) = snapshot
                .agent_panes
                .iter()
                .find(|binding| &binding.pane_id == pane)
                .and_then(|binding| binding.agent_id.clone())
        {
            return Some(agent_id);
        }

        let kind = self.kind.as_ref()?;
        let mut matches = rimz::address::addressable_agents(snapshot)
            .into_iter()
            .filter(|agent| agent.ended_at.is_none())
            .filter(|agent| {
                launch_identity_matches(
                    agent,
                    kind,
                    self.name.as_deref(),
                    self.profile.as_deref(),
                    self.role.as_deref(),
                )
            });
        let agent_id = matches.next()?.agent_id.clone();
        matches.next().is_none().then_some(agent_id)
    }
}

pub(super) fn build_list_report(
    demotion: &rimz::agents::ParkDemotion,
    snapshot: &SidebarSnapshot,
    agents: &[&AgentState],
    now: Timestamp,
    runtime: Option<&rimz::RuntimePaths>,
    slots: &[Vec<&AgentState>],
    active_secs: &rimz::agents::attribution::ActiveSecs,
) -> AgentListReport {
    let identity = SelfIdentity::from_env();
    let me = identity.resolve(snapshot);
    let groups = group_live_agents_by_worktree(agents, snapshot);
    let peers: Vec<&AgentState> = groups
        .iter()
        .flat_map(|group| group.agents.iter().copied())
        .collect();
    let agents = groups
        .into_iter()
        .flat_map(|group| {
            let pr = super::list::group_pr(snapshot, &group.key).and_then(super::list::pr_info);
            let peers = &peers;
            let me = me.as_ref();
            group.agents.into_iter().map(move |agent| {
                build_entry(
                    demotion,
                    agent,
                    row_for_agent(snapshot, agent),
                    pr,
                    peers,
                    me,
                    now,
                    ReportOverrides {
                        runtime,
                        active_secs: rimz::agents::attribution::seat_active_secs(
                            slot_records_for_agent(slots, agent),
                            active_secs,
                        ),
                        ..ReportOverrides::default()
                    },
                )
            })
        })
        .collect();
    AgentListReport {
        schema: AGENT_LIST_SCHEMA,
        agents,
    }
}

pub(super) fn slot_records_for_agent<'a>(
    slots: &[Vec<&'a AgentState>],
    agent: &'a AgentState,
) -> Vec<&'a AgentState> {
    slots
        .iter()
        .find(|slot| slot.iter().any(|record| record.agent_id == agent.agent_id))
        .cloned()
        .unwrap_or_else(|| vec![agent])
}

#[expect(
    clippy::too_many_arguments,
    reason = "report projection keeps durable demotion and presentation overrides explicit"
)]
pub(super) fn build_entry(
    demotion: &rimz::agents::ParkDemotion,
    agent: &AgentState,
    row: Option<&SidebarRow>,
    pr: Option<PrInfo>,
    peers: &[&AgentState],
    me: Option<&AgentSessionId>,
    now: Timestamp,
    overrides: ReportOverrides<'_>,
) -> AgentReportEntry {
    let card = row.and_then(SidebarRow::as_agent);
    let (status, phase) = card
        .map(|card| (card.status, card.phase))
        .unwrap_or_else(|| agent.rowless_status(demotion));
    let displayed_error = agent.displayed_turn_error();
    let turn_error = displayed_error.map(|(class, error)| TurnErrorReport {
        class,
        label: row
            .and_then(SidebarRow::turn_error_label)
            .or(error.label.as_deref())
            .map(ToOwned::to_owned),
    });
    let description = render::agent_activity_line(agent, card);
    let model = model_report(agent);
    let pane = row
        .and_then(|row| row.pane.as_ref())
        .or(agent.pane.as_ref())
        .map(|pane| pane.pane_id.to_string());
    let cost_usd = overrides.effort.map_or_else(
        || card.and_then(AgentCard::cost_usd),
        |effort| effort.cost_usd,
    );
    let effort_tokens = overrides
        .effort
        .map(|effort| effort.tokens)
        .filter(|tokens| tokens.display_total() > 0);
    let budget = budget_report(overrides.runtime, agent, overrides.budget_cost_usd);

    AgentReportEntry {
        id: agent.agent_id.clone(),
        kind: agent.kind.clone(),
        login: agent.login.as_ref().map(|_| agent.login_key()),
        handle: rimz::address::agent_handle(agent, peers, false),
        name: agent.name.clone(),
        name_explicit: agent.name_explicit,
        profile: agent.profile.clone(),
        role: agent.role.clone(),
        team: agent.team.clone(),
        mode: agent.mode,
        launch_warnings: agent.launch_warnings.clone(),
        me: me == Some(&agent.agent_id),
        status,
        pending_waits: agent.pending_waits.clone(),
        idle_stop: agent.idle_stop.clone(),
        background_shells: agent.background_shells.clone(),
        phase,
        turn_error,
        ask: agent
            .open_ask
            .as_ref()
            .filter(|_| agent.is_awaiting_input())
            .map(AskReport::from),
        unread: row.is_some_and(|row| row.unread),
        attention_score: row.map_or(0, |row| row.attention_score),
        description,
        model,
        context: ContextReport {
            fill_pct: agent
                .context_fill_pct()
                .map(|pct| pct.round().clamp(0.0, 100.0) as u8),
            used_tokens: agent.context_used_tokens(),
            window: agent.resolved_context_window(),
            severity: card.and_then(|card| card.context_severity),
            compactions: agent.compaction_count,
            compacting: agent.is_compacting(now),
        },
        stats: StatsReport {
            total_tokens: effort_tokens
                .map(rimz::agents::spending::EffortTokens::display_total)
                .or(agent.usage.total_tokens),
            fresh_input_tokens: effort_tokens
                .map(|tokens| tokens.input)
                .or(agent.usage.fresh_input_tokens),
            cache_read_tokens: effort_tokens
                .map(|tokens| tokens.cache_read)
                .or(agent.usage.cache_read_input_tokens),
            cache_write_tokens: effort_tokens
                .map(|tokens| tokens.cache_write)
                .or(agent.usage.cache_write_input_tokens),
            output_tokens: effort_tokens
                .map(|tokens| tokens.output)
                .or(agent.usage.output_tokens),
            cost_usd,
            active_secs: overrides.active_secs,
            tool_calls: agent.tool_calls.clone(),
            tool_repeat: agent.tool_repeat.clone(),
        },
        timeline: TimelineReport {
            registered_at: agent.registered_at,
            turn_started_at: agent.turn_started_at,
            last_activity: agent.last_activity,
            last_seen: agent.last_seen,
        },
        placement: PlacementReport {
            lane_label: agent.lane_label(),
            channel: agent.channel(),
            worktree: agent.worktree_path.clone(),
            branch: agent.worktree_branch.clone(),
            pane,
            pr,
        },
        budget,
        sub_agents: card
            .map(|card| {
                card.current_sub_agents()
                    .map(|sub_agent| SubAgentReport {
                        id: sub_agent.id.clone(),
                        name: sub_agent
                            .petname
                            .clone()
                            .unwrap_or_else(|| sub_agent.name.clone()),
                        status: sub_agent.status,
                        phase: sub_agent.phase,
                        model: model_label(sub_agent.model.as_deref(), sub_agent.effort.as_deref()),
                        tokens: sub_agent.tokens,
                        elapsed_secs: sub_agent.elapsed_secs,
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn budget_report(
    runtime: Option<&rimz::RuntimePaths>,
    agent: &AgentState,
    session_cost: Option<f64>,
) -> BudgetReport {
    let park = agent.budget_park.as_ref();
    let fallback_cap = park
        .map(|park| rimz::harness::budget::BudgetSpec {
            cap_usd: park.cap_usd,
            window: park.window,
        })
        .or_else(|| {
            agent
                .budget
                .as_deref()
                .and_then(|raw| raw.parse::<rimz::harness::budget::BudgetSpec>().ok())
        });
    let (cap, spent_usd) = match runtime {
        Some(runtime) => {
            let spend = rimz::harness::budget::agent_budget_spend(runtime, agent, session_cost);
            (spend.cap, spend.spent_usd)
        }
        None => {
            let spent_usd = park.map(|park| park.spend_usd).or_else(|| {
                fallback_cap.and_then(|cap| {
                    rimz::harness::budget::total_cost_usd(agent)
                        .or(session_cost)
                        .map(|total| rimz::harness::budget::BudgetLedger::new(cap).spend_usd(total))
                })
            });
            (fallback_cap, spent_usd)
        }
    };
    BudgetReport {
        cap: cap.map(|cap| cap.to_string()),
        spent_usd,
        parked: park.is_some(),
        park: park.map(rimz::agents::BudgetPark::label),
    }
}

pub(super) fn row_for_agent<'a>(
    snapshot: &'a SidebarSnapshot,
    agent: &AgentState,
) -> Option<&'a SidebarRow> {
    snapshot
        .rows()
        .find(|row| row.is_agent() && row.id == agent.agent_id.as_str())
}

pub(super) fn status_style(entry: &AgentReportEntry) -> anstyle::Style {
    render::status::agent(entry.status, entry.phase)
}

/// Context fill warms as it climbs: gold past 75%, rose past 90%.
pub(super) fn context_cell(fill_pct: Option<u8>) -> render::Cell {
    let text = fill_pct
        .map(|pct| format!("{pct}%"))
        .unwrap_or_else(|| "-".to_owned());
    let cell = render::cell(text);
    match fill_pct {
        Some(pct) if pct >= 90 => cell.fg(render::palette::alarm()),
        Some(pct) if pct >= 75 => cell.fg(render::palette::warn()),
        Some(_) => cell,
        None => cell.dash(),
    }
}

pub(super) fn model_report(agent: &AgentState) -> ModelReport {
    let context = agent.context.as_ref();
    let id = context
        .and_then(|context| context.model_id.clone())
        .or_else(|| agent.model.clone());
    let effort = context
        .and_then(|context| context.effort.clone())
        .or_else(|| agent.effort.clone());
    let display = context
        .and_then(|context| context.model_display_name.as_deref())
        .or(id.as_deref());
    let label = model_label(display, effort.as_deref());
    ModelReport { id, effort, label }
}

fn model_label(model: Option<&str>, effort: Option<&str>) -> Option<String> {
    match (model, effort) {
        (Some(model), Some(effort)) => Some(format!("{model}@{effort}")),
        (Some(model), None) => Some(model.to_owned()),
        (None, Some(effort)) => Some(format!("auto@{effort}")),
        (None, None) => None,
    }
}

fn launch_identity_matches(
    agent: &AgentState,
    kind: &AgentKind,
    name: Option<&str>,
    profile: Option<&str>,
    role: Option<&str>,
) -> bool {
    if &agent.kind != kind {
        return false;
    }
    if let Some(name) = name {
        // Launch identities have no session id; an empty id is impossible for
        // adapter observations, so AgentCardRef exercises only its stable-name join.
        let env_id = AgentSessionId::from("");
        if !AgentCardRef::new(kind, &env_id, Some(name)).matches(agent.card_ref()) {
            return false;
        }
    }
    profile.is_none_or(|profile| agent.profile.as_deref() == Some(profile))
        && role.is_none_or(|role| agent.role.as_deref() == Some(role))
}

fn env_string(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests;
