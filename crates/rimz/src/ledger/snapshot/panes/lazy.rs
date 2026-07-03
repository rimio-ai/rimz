use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use jiff::Timestamp;
use serde::Serialize;

use super::{agent_for_pane, pane_start_allows_bind};
use crate::agents::AgentDescriptor;
use crate::agents::lifecycle::TurnPhase;
use crate::agents::{AgentState, AgentStatus};
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::ledger::snapshot::process::{
    pane_agent_kind, pane_command_is_known, pane_worktree_path, row_from_process,
};
use crate::ledger::snapshot::row::{AgentCard, RowCard, SidebarRow};
use crate::pane::PaneRef;

/// What a live pane running an agent command resolves to when no stamped agent
/// claimed its pane id.
pub(crate) enum AgentPaneRow<'a> {
    /// An unstamped session bound to this pane by exact worktree cwd.
    Agent(&'a AgentState),
    /// A wired lazy-registering instance with no session bound yet.
    Idle(Box<SidebarRow>),
    /// This pane resolves to a session already bound to another pane. It folds
    /// no row; the projection emits the diagnostic.
    SuppressedDuplicate {
        kind: AgentKind,
        agent_id: AgentSessionId,
    },
}

/// Resolve a live pane running a known agent command ([`AgentPaneRow`]) to its
/// row — the relaxation of stamped-id binding, kept tightly scoped here.
pub(crate) fn agent_pane_for_pane<'a>(
    pane: &PaneRef,
    agents: &'a [AgentState],
    pairings: &LazyAgentPairingResult,
    bound: &BTreeSet<(AgentKind, AgentSessionId)>,
    wired_lazy_kinds: &[String],
    lazy_agent_default_models: &BTreeMap<String, String>,
    now: Timestamp,
) -> Option<AgentPaneRow<'a>> {
    let (kind, descriptor, cwd) = agent_pane_identity(pane)?;
    if let Some(agent) = pairings
        .pairings
        .get(&pane.pane_id)
        .and_then(|agent_index| agents.get(*agent_index))
        .filter(|agent| agent.kind == kind && agent.worktree_path.as_deref() == Some(cwd))
    {
        if bound.contains(&(agent.kind.clone(), agent.agent_id.clone())) {
            return Some(AgentPaneRow::SuppressedDuplicate {
                kind: agent.kind.clone(),
                agent_id: agent.agent_id.clone(),
            });
        }
        return Some(AgentPaneRow::Agent(agent));
    }
    if !descriptor.capabilities.registers_lazily {
        return None;
    }
    wired_lazy_kinds.iter().any(|wired| wired == kind).then(|| {
        AgentPaneRow::Idle(Box::new(idle_agent_row(
            pane,
            descriptor,
            cwd,
            lazy_agent_default_models
                .get(kind)
                .map(String::as_str)
                .or(descriptor.default_model),
            now,
        )))
    })
}

#[cfg(test)]
fn lazy_agent_pairing_diagnostics(
    panes: &[PaneRef],
    agents: &[AgentState],
) -> Vec<LazyAgentPairingDiagnostic> {
    compute_lazy_agent_pairings(panes, agents).diagnostics
}

pub(crate) struct LazyAgentPairingResult {
    pairings: HashMap<PaneId, usize>,
    diagnostics: Vec<LazyAgentPairingDiagnostic>,
}

impl LazyAgentPairingResult {
    pub(crate) fn diagnostics(&self) -> &[LazyAgentPairingDiagnostic] {
        &self.diagnostics
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LazyAgentPairingDiagnostic {
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub worktree_path: String,
    pub session_registered_at: Option<Timestamp>,
    pub session_last_activity: Timestamp,
    pub selected_pane: PaneId,
    pub selected_pane_process_start: Option<Timestamp>,
    pub method: LazyAgentPairingMethod,
    pub candidates: Vec<LazyAgentPairingCandidateDiagnostic>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LazyAgentPairingCandidateDiagnostic {
    pub pane_id: PaneId,
    pub pane_process_start: Option<Timestamp>,
    pub resumed_session_id: Option<AgentSessionId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LazyAgentPairingMethod {
    StartProximity,
    DeterministicFallback,
}

pub(crate) fn compute_lazy_agent_pairings(
    panes: &[PaneRef],
    agents: &[AgentState],
) -> LazyAgentPairingResult {
    let (candidates, live_stamped_agents) = lazy_pane_candidates(panes, agents);
    let live_panes: HashSet<&PaneId> = panes.iter().map(|pane| &pane.pane_id).collect();

    let mut pairings: HashMap<PaneId, usize> = HashMap::new();
    let mut diagnostics = Vec::new();
    let mut used_agents: BTreeSet<(AgentKind, AgentSessionId)> = BTreeSet::new();
    let mut used_panes: HashSet<PaneId> = HashSet::new();

    pair_resumed_sessions(
        &candidates,
        agents,
        &live_stamped_agents,
        &mut pairings,
        &mut used_agents,
        &mut used_panes,
    );

    let mut sessions = agents
        .iter()
        .enumerate()
        .filter(|(_, agent)| {
            let Some(stamped) = agent.pane.as_ref() else {
                return true;
            };
            !live_panes.contains(&stamped.pane_id)
                && crate::agents::descriptor_by_kind(agent.kind.as_str())
                    .is_some_and(|descriptor| descriptor.capabilities.registers_lazily)
        })
        .filter(|(_, agent)| agent.parent_agent_id.is_none())
        .filter(|(_, agent)| !used_agents.contains(&(agent.kind.clone(), agent.agent_id.clone())))
        .filter(|(_, agent)| {
            candidates.iter().any(|candidate| {
                agent.kind == candidate.kind
                    && agent.worktree_path.as_deref() == Some(candidate.cwd)
            })
        })
        .collect::<Vec<_>>();
    sessions.sort_by(|left, right| {
        right
            .1
            .last_activity
            .cmp(&left.1.last_activity)
            .then(left.1.agent_id.cmp(&right.1.agent_id))
    });

    for (agent_index, agent) in sessions {
        let first_event = agent.registered_at.unwrap_or(agent.last_activity);
        let viable = candidates
            .iter()
            .filter(|candidate| !used_panes.contains(&candidate.pane.pane_id))
            .filter(|candidate| agent.kind == candidate.kind)
            .filter(|candidate| agent.worktree_path.as_deref() == Some(candidate.cwd))
            .collect::<Vec<_>>();
        let selected = viable
            .iter()
            .copied()
            .filter(|candidate| {
                candidate
                    .pane
                    .pane_process_start
                    .is_some_and(|start| start <= first_event)
            })
            .max_by_key(|candidate| {
                (
                    candidate.pane.pane_process_start,
                    Reverse(candidate.pane.pane_id.to_string()),
                )
            })
            .map(|candidate| (candidate, LazyAgentPairingMethod::StartProximity))
            .or_else(|| {
                viable
                    .first()
                    .copied()
                    .filter(|candidate| pane_start_allows_bind(agent.last_activity, candidate.pane))
                    .map(|candidate| (candidate, LazyAgentPairingMethod::DeterministicFallback))
            });
        if let Some((candidate, method)) = selected {
            if viable.len() > 1 {
                diagnostics.push(lazy_pairing_diagnostic(agent, candidate, method, &viable));
            }
            pairings.insert(candidate.pane.pane_id.clone(), agent_index);
            used_panes.insert(candidate.pane.pane_id.clone());
            used_agents.insert((agent.kind.clone(), agent.agent_id.clone()));
        }
    }

    LazyAgentPairingResult {
        pairings,
        diagnostics,
    }
}

fn lazy_pane_candidates<'a>(
    panes: &'a [PaneRef],
    agents: &'a [AgentState],
) -> (
    Vec<LazyPaneCandidate<'a>>,
    BTreeSet<(AgentKind, AgentSessionId)>,
) {
    let stamped_bound = BTreeSet::new();
    let mut live_stamped_agents = BTreeSet::new();
    let mut candidates = Vec::new();
    for pane in panes {
        if let Some(agent) = agent_for_pane(pane, agents, &stamped_bound) {
            live_stamped_agents.insert((agent.kind.clone(), agent.agent_id.clone()));
            continue;
        }
        let Some((kind, _, cwd)) = agent_pane_identity(pane) else {
            continue;
        };
        candidates.push(LazyPaneCandidate { pane, kind, cwd });
    }
    candidates.sort_by(|left, right| {
        left.pane
            .pane_process_start
            .cmp(&right.pane.pane_process_start)
            .then_with(|| {
                left.pane
                    .pane_id
                    .to_string()
                    .cmp(&right.pane.pane_id.to_string())
            })
    });
    (candidates, live_stamped_agents)
}

fn pair_resumed_sessions(
    candidates: &[LazyPaneCandidate<'_>],
    agents: &[AgentState],
    live_stamped_agents: &BTreeSet<(AgentKind, AgentSessionId)>,
    pairings: &mut HashMap<PaneId, usize>,
    used_agents: &mut BTreeSet<(AgentKind, AgentSessionId)>,
    used_panes: &mut HashSet<PaneId>,
) {
    for candidate in candidates {
        let Some(resumed) = candidate.pane.resumed_session_id.as_ref() else {
            continue;
        };
        let Some((agent_index, agent)) = resumed_agent_for_candidate(
            agents,
            candidate,
            resumed,
            live_stamped_agents,
            used_agents,
        ) else {
            continue;
        };
        pairings.insert(candidate.pane.pane_id.clone(), agent_index);
        used_panes.insert(candidate.pane.pane_id.clone());
        used_agents.insert((agent.kind.clone(), agent.agent_id.clone()));
    }
}

fn resumed_agent_for_candidate<'a>(
    agents: &'a [AgentState],
    candidate: &LazyPaneCandidate<'_>,
    resumed: &AgentSessionId,
    live_stamped_agents: &BTreeSet<(AgentKind, AgentSessionId)>,
    used_agents: &BTreeSet<(AgentKind, AgentSessionId)>,
) -> Option<(usize, &'a AgentState)> {
    agents.iter().enumerate().find(|(_, agent)| {
        agent.parent_agent_id.is_none()
            && agent.kind == candidate.kind
            && agent.agent_id == *resumed
            && agent.worktree_path.as_deref() == Some(candidate.cwd)
            && !live_stamped_agents.contains(&(agent.kind.clone(), agent.agent_id.clone()))
            && !used_agents.contains(&(agent.kind.clone(), agent.agent_id.clone()))
    })
}

#[derive(Clone, Copy)]
struct LazyPaneCandidate<'a> {
    pane: &'a PaneRef,
    kind: &'static str,
    cwd: &'a str,
}

fn lazy_pairing_diagnostic(
    agent: &AgentState,
    selected: &LazyPaneCandidate<'_>,
    method: LazyAgentPairingMethod,
    viable: &[&LazyPaneCandidate<'_>],
) -> LazyAgentPairingDiagnostic {
    LazyAgentPairingDiagnostic {
        kind: agent.kind.clone(),
        agent_id: agent.agent_id.clone(),
        worktree_path: agent.worktree_path.clone().unwrap_or_default(),
        session_registered_at: agent.registered_at,
        session_last_activity: agent.last_activity,
        selected_pane: selected.pane.pane_id.clone(),
        selected_pane_process_start: selected.pane.pane_process_start,
        method,
        candidates: viable
            .iter()
            .map(|candidate| LazyAgentPairingCandidateDiagnostic {
                pane_id: candidate.pane.pane_id.clone(),
                pane_process_start: candidate.pane.pane_process_start,
                resumed_session_id: candidate.pane.resumed_session_id.clone(),
            })
            .collect(),
    }
}

/// The common live-pane identity for agent commands: foreground command names a
/// known agent kind, the pane is not marked as a foreign-user elevated agent,
/// and the pane has a non-empty worktree path from the mux cwd or Rimz's
/// supervised-wrapper manifest.
fn agent_pane_identity(pane: &PaneRef) -> Option<(&'static str, &'static AgentDescriptor, &str)> {
    if pane.elevated_agent.is_some() {
        return None;
    }
    let kind = pane_agent_kind(pane)?;
    let descriptor = crate::agents::descriptor_by_kind(kind)?;
    let worktree_path = pane_worktree_path(pane)?;
    Some((kind, descriptor, worktree_path))
}

/// The resting row for a wired lazy-agent pane that no session claimed: `○
/// <kind>` with adapter-owned model/window defaults when known.
fn idle_agent_row(
    pane: &PaneRef,
    descriptor: &AgentDescriptor,
    worktree_path: &str,
    default_model: Option<&str>,
    now: Timestamp,
) -> SidebarRow {
    SidebarRow {
        id: pane.pane_id.to_string(),
        name: descriptor.kind.to_owned(),
        pane: Some(pane.clone()),
        worktree_path: Some(worktree_path.to_owned()),
        worktree_branch: None,
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: pane.pane_process_start.unwrap_or(now),
        card: RowCard::Agent(Box::new(AgentCard {
            status: AgentStatus::Idle,
            phase: TurnPhase::Idle,
            request_id: None,
            surface: None,
            task: None,
            prompt: None,
            description: None,
            model: default_model.map(ToOwned::to_owned),
            effort: None,
            handle: None,
            team: None,
            launch_group: None,
            launch_ordinal: None,

            // Agent rows draw the started-session gauge at `Some(0)` — matching
            // a freshly-bound session.
            context_pct: Some(0),
            context_window: descriptor.default_context_window,
            total_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            context: None,
            context_severity: None,
            // No session yet — the pane's process start is this row's spawn key.
            registered_at: None,
            resolver: None,
            options: Vec::new(),
            sub_agents: Vec::new(),
            compacting: false,
            compaction_count: 0,
            turn_error_label: None,
        })),
    }
}

/// Project a realtime command overlay for an already frame-admitted pane. This
/// uses the same agent-pane identity gate as the verified pull path, but only
/// synthesizes the unbound idle row for lazy-registering agents.
pub(crate) fn row_from_frame_pane(
    pane: &PaneRef,
    wired_lazy_kinds: &[String],
    lazy_agent_default_models: &BTreeMap<String, String>,
    now: Timestamp,
) -> Option<SidebarRow> {
    if let Some((kind, descriptor, worktree_path)) = agent_pane_identity(pane)
        && descriptor.capabilities.registers_lazily
        && wired_lazy_kinds.iter().any(|wired| wired == kind)
    {
        return Some(idle_agent_row(
            pane,
            descriptor,
            worktree_path,
            lazy_agent_default_models
                .get(kind)
                .map(String::as_str)
                .or(descriptor.default_model),
            now,
        ));
    }
    pane_command_is_known(pane).then(|| row_from_process(pane, now))
}

#[cfg(test)]
mod tests;
