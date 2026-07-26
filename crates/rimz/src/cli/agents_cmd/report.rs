use std::collections::BTreeMap;

use jiff::Timestamp;
use serde::Serialize;

use crate::cli::render;
use rimz::agents::{
    AgentCardRef, AgentState, AgentStatus, ContextSeverity, OpenAsk, TurnErrorClass, TurnPhase,
    single_line_description,
};
use rimz::harness::run::PermissionMode;
use rimz::ids::{AgentKind, AgentSessionId, PaneId};

pub(super) const AGENT_REPORT_SCHEMA: u8 = 1;

#[derive(Clone, Debug, Serialize)]
pub(super) struct AgentListReport {
    pub schema: u8,
    pub agents: Vec<AgentReportEntry>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct AgentReportEntry {
    pub id: AgentSessionId,
    pub kind: AgentKind,
    pub handle: String,
    pub name: Option<String>,
    pub name_explicit: bool,
    pub profile: Option<String>,
    pub role: Option<String>,
    pub team: Option<String>,
    pub mode: Option<PermissionMode>,
    pub me: bool,
    pub status: AgentStatus,
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
    pub channel: Option<String>,
    pub worktree: Option<String>,
    pub branch: Option<String>,
    pub pane: Option<String>,
    pub pr: Option<PrInfo>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(super) struct PrInfo {
    pub number: Option<u64>,
    pub state: rimz::WorktreePrState,
    pub ci: Option<rimz::WorktreePrCi>,
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
    pub total_tokens: Option<u64>,
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
            kind: env_string(rimz::harness::run::ENV_AGENT_KIND).map(AgentKind::new_unchecked),
            name: env_string(rimz::harness::run::ENV_AGENT_NAME),
            profile: env_string(rimz::harness::run::ENV_AGENT_PROFILE),
            role: env_string(rimz::harness::run::ENV_AGENT_ROLE),
        }
    }

    pub fn resolve(&self, snapshot: &rimz::SidebarSnapshot) -> Option<AgentSessionId> {
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
        let mut matches = rimz::harness::target::addressable_agents(snapshot)
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
    snapshot: &rimz::SidebarSnapshot,
    agents: &[&AgentState],
    now: Timestamp,
    runtime: Option<&rimz::RuntimePaths>,
) -> AgentListReport {
    let identity = SelfIdentity::from_env();
    let me = identity.resolve(snapshot);
    let groups = rimz::store::snapshot::group_live_agents_by_worktree(agents, snapshot);
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
                    agent,
                    row_for_agent(snapshot, agent),
                    pr,
                    peers,
                    me,
                    now,
                    ReportOverrides {
                        runtime,
                        ..ReportOverrides::default()
                    },
                )
            })
        })
        .collect();
    AgentListReport {
        schema: AGENT_REPORT_SCHEMA,
        agents,
    }
}

pub(super) fn build_entry(
    agent: &AgentState,
    row: Option<&rimz::SidebarRow>,
    pr: Option<PrInfo>,
    peers: &[&AgentState],
    me: Option<&AgentSessionId>,
    now: Timestamp,
    overrides: ReportOverrides<'_>,
) -> AgentReportEntry {
    let card = row.and_then(rimz::SidebarRow::as_agent);
    let (status, phase) = card
        .map(|card| (card.status, card.phase))
        .unwrap_or_else(|| fallback_status_projection(agent));
    let displayed_error = agent.displayed_turn_error();
    let turn_error = displayed_error.map(|(class, state_label)| TurnErrorReport {
        class,
        label: row
            .and_then(rimz::SidebarRow::turn_error_label)
            .or(state_label)
            .map(ToOwned::to_owned),
    });
    let description = card
        .and_then(rimz::AgentCard::activity_description)
        .and_then(single_line_description)
        .or_else(|| agent.activity_line());
    let model = model_report(agent);
    let pane = row
        .and_then(|row| row.pane.as_ref())
        .or(agent.pane.as_ref())
        .map(|pane| pane.pane_id.to_string());
    let cost_usd = overrides.effort.map_or_else(
        || {
            card.and_then(|card| card.context.as_ref())
                .and_then(|context| context.cost.as_ref())
                .and_then(|cost| cost.total_cost_usd)
        },
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
        handle: rimz::harness::target::agent_handle(agent, peers, false),
        name: agent.name.clone(),
        name_explicit: agent.name_explicit,
        profile: agent.profile.clone(),
        role: agent.role.clone(),
        team: agent.team.clone(),
        mode: agent.mode,
        me: me == Some(&agent.agent_id),
        status,
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
            active_secs: overrides
                .active_secs
                .or_else(|| card.and_then(|card| card.estimated_active_secs)),
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
            channel: rimz::harness::target::agent_channel(agent),
            worktree: agent.worktree_path.clone(),
            branch: agent.worktree_branch.clone(),
            pane,
            pr,
        },
        budget,
        sub_agents: card
            .map(|card| {
                card.sub_agents
                    .iter()
                    .map(|sub_agent| SubAgentReport {
                        id: sub_agent.id.clone(),
                        name: sub_agent.name.clone(),
                        status: sub_agent.status,
                        phase: sub_agent.phase,
                        model: model_label(sub_agent.model.as_deref(), sub_agent.effort.as_deref()),
                        total_tokens: sub_agent.total_tokens,
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
        park: park.map(rimz::harness::budget::BudgetPark::label),
    }
}

pub(super) fn row_for_agent<'a>(
    snapshot: &'a rimz::SidebarSnapshot,
    agent: &AgentState,
) -> Option<&'a rimz::SidebarRow> {
    snapshot
        .rows()
        .find(|row| row.is_agent() && row.id == agent.agent_id.as_str())
}

pub(super) fn fallback_status_projection(agent: &AgentState) -> (AgentStatus, TurnPhase) {
    match agent.displayed_turn_error().map(|(class, _)| class) {
        Some(
            TurnErrorClass::PausedRateLimit
            | TurnErrorClass::PausedSpendLimit
            | TurnErrorClass::PausedOverloaded,
        ) => (AgentStatus::Paused, TurnPhase::Idle),
        Some(TurnErrorClass::Unknown | TurnErrorClass::Failed) => {
            (AgentStatus::Failed, TurnPhase::Idle)
        }
        None => {
            let status = agent.effective_status();
            let phase = if status == AgentStatus::Running {
                agent.phase
            } else {
                TurnPhase::Idle
            };
            (status, phase)
        }
    }
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
mod tests {
    use super::*;

    fn agent(id: &str) -> AgentState {
        rimz::testkit::agent_state("codex", id, Timestamp::UNIX_EPOCH)
    }

    fn pane_agent(id: &str, pane: &str) -> rimz::PaneAgent {
        rimz::PaneAgent {
            kind: AgentKind::new_unchecked("codex"),
            kind_ordinal: None,
            name: Some(format!("{id}-name")),
            name_explicit: false,
            profile: None,
            role: None,
            channel: None,
            agent_id: Some(AgentSessionId::from(id)),
            pane_id: PaneId::from_parts(rimz::MuxName::Tmux, pane),
            pane_pid: None,
            worktree_path: None,
            worktree_branch: None,
        }
    }

    #[test]
    fn pane_identity_wins_over_launch_identity() {
        let first = agent("first");
        let second = agent("second");
        let mut snapshot = rimz::SidebarSnapshot::build_with_agents(
            rimz::WorkspaceId::from_project_root(std::path::Path::new("/repo")),
            vec![first, second],
            Timestamp::UNIX_EPOCH,
        );
        snapshot.agent_panes = vec![pane_agent("first", "%1"), pane_agent("second", "%2")];
        let identity = SelfIdentity {
            pane: Some(PaneId::from_parts(rimz::MuxName::Tmux, "%2")),
            kind: Some(AgentKind::new_unchecked("codex")),
            name: Some("first-name".to_owned()),
            ..SelfIdentity::default()
        };

        assert_eq!(
            identity
                .resolve(&snapshot)
                .as_ref()
                .map(AgentSessionId::as_str),
            Some("second")
        );
    }

    #[test]
    fn launch_identity_matches_uniquely_and_missing_identity_matches_nothing() {
        let mut first = agent("first");
        first.name = Some("first-name".to_owned());
        first.profile = Some("planner".to_owned());
        first.role = Some("lead".to_owned());
        let second = agent("second");
        let snapshot = rimz::SidebarSnapshot::build_with_agents(
            rimz::WorkspaceId::from_project_root(std::path::Path::new("/repo")),
            vec![first, second],
            Timestamp::UNIX_EPOCH,
        );
        let identity = SelfIdentity {
            kind: Some(AgentKind::new_unchecked("codex")),
            name: Some("first-name".to_owned()),
            profile: Some("planner".to_owned()),
            role: Some("lead".to_owned()),
            ..SelfIdentity::default()
        };

        assert_eq!(
            identity
                .resolve(&snapshot)
                .as_ref()
                .map(AgentSessionId::as_str),
            Some("first")
        );
        assert_eq!(SelfIdentity::default().resolve(&snapshot), None);
    }

    #[test]
    fn projected_row_status_wins_and_rowless_error_falls_back_to_failed() {
        let now = Timestamp::from_second(1_000).unwrap();
        let mut state = agent("status");
        state.status = AgentStatus::Running;
        state.phase = TurnPhase::Acting;
        let mut context = rimz::agents::AgentContext::new("codex", now);
        context.turn_error = Some(rimz::agents::AgentTurnError {
            class: TurnErrorClass::Failed,
            at: now,
            label: Some("boom".to_owned()),
        });
        state.context = Some(context);
        let row = rimz::SidebarRow {
            id: "status".to_owned(),
            name: "codex".to_owned(),
            pane: None,
            worktree_path: None,
            worktree_branch: None,
            channel: None,
            unread: false,
            inactive: false,
            archived: false,
            attention_score: 0,
            last_activity: now,
            card: rimz::RowCard::Agent(Box::new(rimz::AgentCard {
                status: AgentStatus::Paused,
                phase: TurnPhase::Idle,
                ..rimz::AgentCard::default()
            })),
        };
        let peers = [&state];

        assert_eq!(
            build_entry(
                &state,
                Some(&row),
                None,
                &peers,
                None,
                now,
                ReportOverrides::default(),
            )
            .status,
            AgentStatus::Paused
        );
        assert_eq!(
            build_entry(
                &state,
                None,
                None,
                &peers,
                None,
                now,
                ReportOverrides::default(),
            )
            .status,
            AgentStatus::Failed
        );
    }

    #[test]
    fn full_entry_has_a_stable_projection() {
        let now = Timestamp::from_second(2_000).unwrap();
        let mut state = agent("full");
        state.name_explicit = true;
        state.profile = Some("builder".to_owned());
        state.role = Some("coder".to_owned());
        state.team = Some("forge".to_owned());
        state.mode = Some(PermissionMode::Yolo);
        state.channel = Some("auth".to_owned());
        state.worktree_path = Some("/repo/auth".to_owned());
        state.worktree_branch = Some("feature/auth".to_owned());
        state.model = Some("gpt-5".to_owned());
        state.effort = Some("high".to_owned());
        state.description = Some("ship\nrefresh".to_owned());
        state.usage.total_tokens = Some(54_210);
        state.usage.fresh_input_tokens = Some(4_180);
        state.usage.cache_read_input_tokens = Some(47_600);
        state.usage.cache_write_input_tokens = Some(1_920);
        state.usage.output_tokens = Some(510);
        state.usage.context_pct = Some(31);
        state.compaction_count = 2;
        state.tool_calls.insert("Bash".to_owned(), 12);
        state.registered_at = Some(Timestamp::from_second(1_000).unwrap());
        state.turn_started_at = Some(Timestamp::from_second(1_900).unwrap());
        state.budget = Some("$5.00".to_owned());
        let mut context = rimz::agents::AgentContext::new("codex", now);
        context.model_id = Some("gpt-5.5".to_owned());
        context.model_display_name = Some("GPT 5.5".to_owned());
        context.effort = Some("high".to_owned());
        context.cost = Some(rimz::agents::AgentCost {
            total_cost_usd: Some(0.87),
            ..rimz::agents::AgentCost::default()
        });
        state.context = Some(context.clone());
        let row = rimz::SidebarRow {
            id: "full".to_owned(),
            name: "codex".to_owned(),
            pane: None,
            worktree_path: state.worktree_path.clone(),
            worktree_branch: state.worktree_branch.clone(),
            channel: state.channel.clone(),
            unread: true,
            inactive: false,
            archived: false,
            attention_score: 42,
            last_activity: now,
            card: rimz::RowCard::Agent(Box::new(rimz::AgentCard {
                status: AgentStatus::Running,
                phase: TurnPhase::Acting,
                context: Some(context),
                context_severity: Some(ContextSeverity::Yellow),
                estimated_active_secs: Some(754),
                sub_agents: vec![rimz::SidebarSubAgent {
                    id: "child".to_owned(),
                    name: "review".to_owned(),
                    status: AgentStatus::Running,
                    phase: TurnPhase::Reasoning,
                    task: None,
                    model: Some("sonnet".to_owned()),
                    effort: Some("high".to_owned()),
                    description: None,
                    total_tokens: Some(1_200),
                    elapsed_secs: Some(12),
                    started_at: None,
                    last_activity: now,
                    registered_at: None,
                }],
                ..rimz::AgentCard::default()
            })),
        };
        let peers = [&state];
        let entry = build_entry(
            &state,
            Some(&row),
            Some(PrInfo {
                number: Some(91),
                state: rimz::WorktreePrState::Open,
                ci: Some(rimz::WorktreePrCi::Passing),
            }),
            &peers,
            Some(&state.agent_id),
            now,
            ReportOverrides::default(),
        );

        insta::assert_json_snapshot!("full_agent_report", entry);
    }

    #[test]
    fn sparse_entry_keeps_unknown_and_zero_keys() {
        let state = agent("sparse");
        let peers = [&state];
        let entry = build_entry(
            &state,
            None,
            None,
            &peers,
            None,
            Timestamp::UNIX_EPOCH,
            ReportOverrides::default(),
        );

        insta::assert_json_snapshot!("sparse_agent_report", entry);
    }

    #[test]
    fn lifetime_effort_overrides_live_stats_as_one_unit() {
        let now = Timestamp::from_second(2_000).unwrap();
        let mut state = agent("effort");
        state.usage.total_tokens = Some(999);
        state.usage.fresh_input_tokens = Some(999);
        state.budget = Some("$1.00".to_owned());
        let peers = [&state];
        let entry = build_entry(
            &state,
            None,
            None,
            &peers,
            None,
            now,
            ReportOverrides {
                effort: Some(rimz::agents::spending::SlotEffort {
                    tokens: rimz::agents::spending::EffortTokens {
                        input: 10,
                        output: 20,
                        cache_write: 30,
                        cache_read: 40,
                    },
                    cost_usd: Some(0.5),
                }),
                active_secs: Some(60),
                budget_cost_usd: Some(0.25),
                ..ReportOverrides::default()
            },
        );

        assert_eq!(entry.stats.total_tokens, Some(100));
        assert_eq!(entry.stats.fresh_input_tokens, Some(10));
        assert_eq!(entry.stats.output_tokens, Some(20));
        assert_eq!(entry.stats.cache_write_tokens, Some(30));
        assert_eq!(entry.stats.cache_read_tokens, Some(40));
        assert_eq!(entry.stats.cost_usd, Some(0.5));
        assert_eq!(entry.stats.active_secs, Some(60));
        assert_eq!(entry.budget.spent_usd, Some(0.25));
    }

    #[test]
    fn budget_projection_uses_effective_ledger_cap_and_live_spend() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_id = rimz::ids::WorkspaceId::from_project_root(dir.path());
        let runtime = rimz::RuntimePaths::under(workspace_id, dir.path()).expect("runtime");
        runtime.ensure_dirs().expect("runtime dirs");
        let now = Timestamp::from_second(2_000).unwrap();
        let mut state = agent("budget");
        state.budget = Some("$9.00".to_owned());
        let mut context = rimz::agents::AgentContext::new("codex", now);
        context.cost = Some(rimz::agents::AgentCost {
            total_cost_usd: Some(7.25),
            ..rimz::agents::AgentCost::default()
        });
        state.context = Some(context);
        let mut ledger = rimz::harness::budget::BudgetLedger::new("5/day".parse().expect("spec"));
        ledger.raised_cap_usd = Some(6.0);
        ledger.day_baseline = Some(rimz::harness::budget::DayBaseline {
            date: "2026-06-01".parse().expect("date"),
            cost_usd: 2.0,
        });
        rimz::harness::budget::write_ledger(&runtime, &state.kind, &state.agent_id, &ledger)
            .expect("write ledger");
        let peers = [&state];

        let entry = build_entry(
            &state,
            None,
            None,
            &peers,
            None,
            now,
            ReportOverrides {
                runtime: Some(&runtime),
                effort: Some(rimz::agents::spending::SlotEffort {
                    cost_usd: Some(100.0),
                    ..rimz::agents::spending::SlotEffort::default()
                }),
                ..ReportOverrides::default()
            },
        );

        assert_eq!(entry.budget.cap.as_deref(), Some("$6.00/day"));
        assert_eq!(entry.budget.spent_usd, Some(5.25));

        let snapshot = rimz::SidebarSnapshot::build_with_agents(
            rimz::WorkspaceId::from_project_root(dir.path()),
            vec![state.clone()],
            now,
        );
        let agents = rimz::harness::target::addressable_agents(&snapshot);
        let list = build_list_report(&snapshot, &agents, now, Some(&runtime));
        assert_eq!(list.agents[0].budget.cap.as_deref(), Some("$6.00/day"));
        assert_eq!(list.agents[0].budget.spent_usd, Some(5.25));

        ledger.disabled = true;
        rimz::harness::budget::write_ledger(&runtime, &state.kind, &state.agent_id, &ledger)
            .expect("disable ledger");
        let entry = build_entry(
            &state,
            None,
            None,
            &peers,
            None,
            now,
            ReportOverrides {
                runtime: Some(&runtime),
                ..ReportOverrides::default()
            },
        );
        assert_eq!(entry.budget.cap, None);
        assert_eq!(entry.budget.spent_usd, None);
    }
}
