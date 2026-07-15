use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use jiff::Timestamp;
use serde::Serialize;

use super::{PaneBindingEvidence, PaneBindingIndex, pane_binding_evidence, pane_start_allows_bind};
use crate::agents::AgentDescriptor;
use crate::agents::lifecycle::TurnPhase;
use crate::agents::{AgentState, AgentStatus, SamePaneSessionPolicy, SessionOrigin};
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::pane::PaneRef;
use crate::store::snapshot::process::{pane_command_is_known, row_from_process};
use crate::store::snapshot::row::{AgentCard, RowCard, SidebarRow};

/// What a live pane running an agent command resolves to when no stamped agent
/// claimed its pane id.
pub(crate) enum AgentPaneRow<'a> {
    /// An unstamped session bound to this pane by exact worktree cwd.
    Agent(&'a AgentState),
    /// A wired instance with no session bound yet.
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
    wired_kinds: &[String],
    wired_default_models: &BTreeMap<String, String>,
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
    wired_kinds.iter().any(|wired| wired == kind).then(|| {
        AgentPaneRow::Idle(Box::new(idle_agent_row(
            pane,
            descriptor,
            cwd,
            wired_default_models
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

/// Lifecycle phase whose hook may recover a missing pane stamp. Occupied-pane
/// adoption belongs only to the first turn-start signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookPaneRecoveryPhase {
    Registered,
    TurnStarted,
}

/// Typed prior-rollup context for pure hook pane recovery. Construction derives
/// registration chronology once; [`Self::already_stamped`] stays cheap enough
/// for CLI to avoid live mux probes on later turns.
pub struct HookPaneRecoveryContext<'a> {
    kind: &'a AgentKind,
    agent_id: &'a AgentSessionId,
    origin: Option<SessionOrigin>,
    phase: HookPaneRecoveryPhase,
    registered_at: Option<Timestamp>,
    prior_agents: &'a [AgentState],
    binding_index: PaneBindingIndex<'a>,
}

struct HookCandidate<'pane, 'agent> {
    evidence: PaneBindingEvidence<'pane>,
    record: HookPaneRecoveryCandidate,
    occupied_by: Option<&'agent AgentState>,
}

struct HookCandidatePool {
    indices: Vec<usize>,
    candidate_count: usize,
    occupied_sole: bool,
}

struct HookSelectionDecision {
    pane_index: Option<usize>,
    candidate_count: usize,
    method: HookPaneRecoveryMethod,
}

impl<'a> HookPaneRecoveryContext<'a> {
    pub fn new(
        kind: &'a AgentKind,
        agent_id: &'a AgentSessionId,
        origin: Option<SessionOrigin>,
        phase: HookPaneRecoveryPhase,
        prior_agents: &'a [AgentState],
    ) -> Self {
        let registered_at = prior_agents
            .iter()
            .find(|agent| agent.kind == *kind && agent.agent_id == *agent_id)
            .and_then(|agent| agent.registered_at);
        Self {
            kind,
            agent_id,
            origin,
            phase,
            registered_at,
            prior_agents,
            binding_index: PaneBindingIndex::new(prior_agents),
        }
    }

    pub fn already_stamped(&self) -> bool {
        self.prior_agents.iter().any(|agent| {
            agent.kind == *self.kind && agent.agent_id == *self.agent_id && agent.pane.is_some()
        })
    }

    pub fn select(
        &self,
        worktree_path: &str,
        panes: &[PaneRef],
        client_focus: Option<&[PaneId]>,
    ) -> HookPaneRecoverySelection {
        let mut candidates = panes
            .iter()
            .map(pane_binding_evidence)
            .map(|evidence| self.candidate(worktree_path, evidence))
            .collect::<Vec<_>>();
        let decision = self.select_candidate(&mut candidates, client_focus);
        let pane = decision
            .pane_index
            .map(|index| candidates[index].evidence.pane.clone());
        HookPaneRecoverySelection {
            pane_id: pane.as_ref().map(|pane| pane.pane_id.clone()),
            pane,
            candidate_count: decision.candidate_count,
            method: decision.method,
            candidates: candidates
                .into_iter()
                .map(|candidate| candidate.record)
                .collect(),
        }
    }

    fn select_candidate(
        &self,
        candidates: &mut [HookCandidate<'_, '_>],
        client_focus: Option<&[PaneId]>,
    ) -> HookSelectionDecision {
        let mut pool = HookCandidatePool {
            indices: selectable_candidate_indices(candidates, false),
            candidate_count: 0,
            occupied_sole: false,
        };
        if pool.indices.is_empty() {
            pool = self.occupied_candidate_pool(candidates, client_focus);
        }
        if pool.candidate_count == 0 {
            pool.candidate_count = pool.indices.len();
        }
        match pool.indices.as_slice() {
            [] => HookSelectionDecision {
                pane_index: None,
                candidate_count: pool.candidate_count,
                method: HookPaneRecoveryMethod::None,
            },
            [pane_index] => HookSelectionDecision {
                pane_index: Some(*pane_index),
                candidate_count: pool.candidate_count,
                method: if pool.occupied_sole {
                    HookPaneRecoveryMethod::OccupiedSoleCandidate
                } else {
                    HookPaneRecoveryMethod::SingleCandidate
                },
            },
            _ => select_focused_candidate(candidates, &pool.indices, client_focus),
        }
    }

    fn occupied_candidate_pool(
        &self,
        candidates: &mut [HookCandidate<'_, '_>],
        client_focus: Option<&[PaneId]>,
    ) -> HookCandidatePool {
        if self.phase != HookPaneRecoveryPhase::TurnStarted || !self.can_share_occupied_pane() {
            return HookCandidatePool {
                indices: Vec::new(),
                candidate_count: 0,
                occupied_sole: false,
            };
        }
        let follows_latest = self.follows_latest();
        let mut indices = selectable_candidate_indices(candidates, true)
            .into_iter()
            .filter(|index| pane_has_focus_evidence(candidates[*index].evidence.pane, client_focus))
            .filter(|index| !follows_latest || occupied_owner_is_resting(&candidates[*index]))
            .collect::<Vec<_>>();
        let mut occupied_sole = false;
        let mut candidate_count = 0;
        if indices.is_empty() && (self.origin == Some(SessionOrigin::Fresh) || follows_latest) {
            indices = selectable_candidate_indices(candidates, true)
                .into_iter()
                .filter(|index| {
                    let candidate = &candidates[*index];
                    occupied_owner_is_resting(candidate)
                        && (follows_latest
                            || candidate
                                .occupied_by
                                .is_some_and(|owner| owner.origin == Some(SessionOrigin::Fresh)))
                })
                .collect();
            occupied_sole = indices.len() == 1;
            if !occupied_sole {
                candidate_count = indices.len();
                indices.clear();
            }
        }
        allow_occupied_candidates(candidates, &indices);
        HookCandidatePool {
            indices,
            candidate_count,
            occupied_sole,
        }
    }

    fn candidate<'pane>(
        &self,
        worktree_path: &str,
        evidence: PaneBindingEvidence<'pane>,
    ) -> HookCandidate<'pane, 'a> {
        let mut reject_reasons = Vec::new();
        if evidence.raw_cwd != Some(worktree_path) {
            reject_reasons.push(HookPaneRecoveryRejectReason::CwdMismatch {
                got: evidence.pane.cwd.clone(),
            });
        }
        if evidence.agent_kind != Some(self.kind.as_str()) {
            reject_reasons.push(HookPaneRecoveryRejectReason::CommandMismatch {
                got: evidence.pane.command.clone(),
            });
        }
        let occupied_by_agent =
            self.binding_index
                .live_foreign_owner(evidence, self.kind, self.agent_id);
        let occupied_by_agent_id = occupied_by_agent.map(|agent| agent.agent_id.to_string());
        if let Some(agent) = occupied_by_agent.filter(|agent| !agent.agent_id.is_provisional()) {
            tracing::debug!(
                target: "rimz::agent::binding",
                kind = self.kind.as_str(),
                agent_id = self.agent_id.as_str(),
                stamped_agent_id = agent.agent_id.as_str(),
                pane = %evidence.pane.pane_id,
                "pane carries another live agent's durable stamp",
            );
            reject_reasons.push(HookPaneRecoveryRejectReason::StampedToOther {
                agent_id: agent.agent_id.to_string(),
            });
        }
        if self.registered_at.is_some_and(|registered_at| {
            evidence
                .process_start
                .is_some_and(|started_at| started_at > registered_at)
                && evidence.resumed_session_id != Some(self.agent_id)
        }) {
            reject_reasons.push(HookPaneRecoveryRejectReason::StartedAfterSession);
        }
        HookCandidate {
            evidence,
            occupied_by: occupied_by_agent,
            record: HookPaneRecoveryCandidate {
                pane_id: evidence.pane.pane_id.clone(),
                cwd: evidence.pane.cwd.clone(),
                command: evidence.pane.command.clone(),
                is_focused: evidence.pane.is_focused,
                pane_process_start: evidence.process_start,
                occupied_by_agent_id,
                reject_reasons,
            },
        }
    }

    fn can_share_occupied_pane(&self) -> bool {
        crate::agents::descriptor_by_kind(self.kind.as_str()).is_some_and(|descriptor| {
            descriptor.capabilities.daemon_hooked_sessions
                || descriptor.capabilities.same_pane_session == SamePaneSessionPolicy::FollowLatest
        }) && !self
            .prior_agents
            .iter()
            .any(|agent| agent.kind == *self.kind && agent.agent_id == *self.agent_id)
    }

    fn follows_latest(&self) -> bool {
        crate::agents::descriptor_by_kind(self.kind.as_str()).is_some_and(|descriptor| {
            descriptor.capabilities.same_pane_session == SamePaneSessionPolicy::FollowLatest
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookPaneRecoverySelection {
    pub pane: Option<PaneRef>,
    pub pane_id: Option<PaneId>,
    pub candidate_count: usize,
    pub method: HookPaneRecoveryMethod,
    pub candidates: Vec<HookPaneRecoveryCandidate>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookPaneRecoveryMethod {
    None,
    SingleCandidate,
    OccupiedSoleCandidate,
    ClientFocus,
    TabFocus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HookPaneRecoveryCandidate {
    pane_id: PaneId,
    cwd: Option<String>,
    command: Option<String>,
    is_focused: bool,
    pane_process_start: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    occupied_by_agent_id: Option<String>,
    reject_reasons: Vec<HookPaneRecoveryRejectReason>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum HookPaneRecoveryRejectReason {
    CwdMismatch { got: Option<String> },
    CommandMismatch { got: Option<String> },
    StampedToOther { agent_id: String },
    StartedAfterSession,
    NotInClientFocus,
    NotTabFocused,
    Ambiguous { n: usize },
}

fn pane_has_focus_evidence(pane: &PaneRef, client_focus: Option<&[PaneId]>) -> bool {
    client_focus
        .map(|focused| focused.iter().any(|pane_id| pane_id == &pane.pane_id))
        .unwrap_or(pane.is_focused)
}

fn selectable_candidate_indices(
    candidates: &[HookCandidate<'_, '_>],
    allow_occupied: bool,
) -> Vec<usize> {
    candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            hook_candidate_selectable(&candidate.record, allow_occupied).then_some(index)
        })
        .collect()
}

fn hook_candidate_selectable(record: &HookPaneRecoveryCandidate, allow_occupied: bool) -> bool {
    record.reject_reasons.is_empty()
        || allow_occupied
            && record
                .reject_reasons
                .iter()
                .all(|reason| matches!(reason, HookPaneRecoveryRejectReason::StampedToOther { .. }))
}

fn allow_occupied_candidates(candidates: &mut [HookCandidate<'_, '_>], selected: &[usize]) {
    for index in selected {
        candidates[*index].record.reject_reasons.retain(|reason| {
            !matches!(reason, HookPaneRecoveryRejectReason::StampedToOther { .. })
        });
    }
}

fn occupied_owner_is_resting(candidate: &HookCandidate<'_, '_>) -> bool {
    candidate
        .occupied_by
        .is_some_and(|owner| !matches!(owner.status, AgentStatus::Running | AgentStatus::Waiting))
}

fn select_focused_candidate(
    candidates: &mut [HookCandidate<'_, '_>],
    viable: &[usize],
    client_focus: Option<&[PaneId]>,
) -> HookSelectionDecision {
    let (focused, reject_reason, method) = if let Some(client_focus) = client_focus {
        (
            viable
                .iter()
                .copied()
                .filter(|index| {
                    client_focus
                        .iter()
                        .any(|pane_id| pane_id == &candidates[*index].evidence.pane.pane_id)
                })
                .collect::<Vec<_>>(),
            HookPaneRecoveryRejectReason::NotInClientFocus,
            HookPaneRecoveryMethod::ClientFocus,
        )
    } else {
        (
            viable
                .iter()
                .copied()
                .filter(|index| candidates[*index].evidence.pane.is_focused)
                .collect::<Vec<_>>(),
            HookPaneRecoveryRejectReason::NotTabFocused,
            HookPaneRecoveryMethod::TabFocus,
        )
    };
    for index in viable {
        if !focused.contains(index) {
            candidates[*index]
                .record
                .reject_reasons
                .push(reject_reason.clone());
        }
    }
    let pane_index = unique_candidate_index(candidates, &focused);
    if pane_index.is_none() && focused.len() > 1 {
        for index in &focused {
            candidates[*index]
                .record
                .reject_reasons
                .push(HookPaneRecoveryRejectReason::Ambiguous { n: focused.len() });
        }
    }
    HookSelectionDecision {
        pane_index,
        candidate_count: viable.len(),
        method,
    }
}

fn unique_candidate_index(
    candidates: &[HookCandidate<'_, '_>],
    focused: &[usize],
) -> Option<usize> {
    let first = *focused.first()?;
    focused
        .iter()
        .all(|index| {
            candidates[*index].evidence.pane.pane_id == candidates[first].evidence.pane.pane_id
        })
        .then_some(first)
}

pub(crate) fn compute_lazy_agent_pairings(
    panes: &[PaneRef],
    agents: &[AgentState],
) -> LazyAgentPairingResult {
    let index = PaneBindingIndex::new(agents);
    compute_lazy_agent_pairings_with_index(panes, &index)
}

pub(in crate::store::snapshot) fn compute_lazy_agent_pairings_with_index(
    panes: &[PaneRef],
    index: &PaneBindingIndex<'_>,
) -> LazyAgentPairingResult {
    let (candidates, live_stamped_agents) = lazy_pane_candidates(panes, index);
    let live_panes: HashSet<&PaneId> = panes.iter().map(|pane| &pane.pane_id).collect();

    let mut pairings: HashMap<PaneId, usize> = HashMap::new();
    let mut diagnostics = Vec::new();
    let mut used_agents: BTreeSet<(AgentKind, AgentSessionId)> = BTreeSet::new();
    let mut used_panes: HashSet<PaneId> = HashSet::new();

    pair_resumed_sessions(
        &candidates,
        index,
        &live_stamped_agents,
        &mut pairings,
        &mut used_agents,
        &mut used_panes,
    );

    let candidate_groups = candidates
        .iter()
        .map(|candidate| (AgentKind::new_unchecked(candidate.kind), candidate.cwd))
        .collect::<BTreeSet<_>>();
    let mut sessions = candidate_groups
        .iter()
        .flat_map(|(kind, cwd)| index.lazy_indices(kind, cwd).iter().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|agent_index| index.agent(agent_index).map(|agent| (agent_index, agent)))
        .filter(|(_, agent)| {
            let Some(stamped) = agent.pane.as_ref() else {
                return true;
            };
            !live_panes.contains(&stamped.pane_id)
                && crate::agents::descriptor_by_kind(agent.kind.as_str())
                    .is_some_and(|descriptor| descriptor.capabilities.registers_lazily)
        })
        .filter(|(_, agent)| !used_agents.contains(&(agent.kind.clone(), agent.agent_id.clone())))
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
            .filter(|candidate| !used_panes.contains(&candidate.evidence.pane.pane_id))
            .filter(|candidate| agent.kind == candidate.kind)
            .filter(|candidate| agent.worktree_path.as_deref() == Some(candidate.cwd))
            .collect::<Vec<_>>();
        let selected = viable
            .iter()
            .copied()
            .filter(|candidate| {
                candidate
                    .evidence
                    .process_start
                    .is_some_and(|start| start <= first_event)
            })
            .max_by_key(|candidate| {
                (
                    candidate.evidence.process_start,
                    Reverse(candidate.evidence.pane.pane_id.to_string()),
                )
            })
            .map(|candidate| (candidate, LazyAgentPairingMethod::StartProximity))
            .or_else(|| {
                viable
                    .first()
                    .copied()
                    .filter(|candidate| {
                        pane_start_allows_bind(agent.last_activity, candidate.evidence.pane)
                    })
                    .map(|candidate| (candidate, LazyAgentPairingMethod::DeterministicFallback))
            });
        if let Some((candidate, method)) = selected {
            if viable.len() > 1 {
                diagnostics.push(lazy_pairing_diagnostic(agent, candidate, method, &viable));
            }
            pairings.insert(candidate.evidence.pane.pane_id.clone(), agent_index);
            used_panes.insert(candidate.evidence.pane.pane_id.clone());
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
    index: &PaneBindingIndex<'_>,
) -> (
    Vec<LazyPaneCandidate<'a>>,
    BTreeSet<(AgentKind, AgentSessionId)>,
) {
    let mut live_stamped_agents = BTreeSet::new();
    let mut candidates = Vec::new();
    for pane in panes {
        if let Some(agent) = index.stamped_agent(pane) {
            live_stamped_agents.insert((agent.kind.clone(), agent.agent_id.clone()));
            continue;
        }
        let evidence = pane_binding_evidence(pane);
        let Some((kind, _, cwd)) = agent_pane_identity_from_evidence(evidence) else {
            continue;
        };
        candidates.push(LazyPaneCandidate {
            evidence,
            kind,
            cwd,
        });
    }
    candidates.sort_by(|left, right| {
        left.evidence
            .process_start
            .cmp(&right.evidence.process_start)
            .then_with(|| {
                left.evidence
                    .pane
                    .pane_id
                    .to_string()
                    .cmp(&right.evidence.pane.pane_id.to_string())
            })
    });
    (candidates, live_stamped_agents)
}

fn pair_resumed_sessions(
    candidates: &[LazyPaneCandidate<'_>],
    index: &PaneBindingIndex<'_>,
    live_stamped_agents: &BTreeSet<(AgentKind, AgentSessionId)>,
    pairings: &mut HashMap<PaneId, usize>,
    used_agents: &mut BTreeSet<(AgentKind, AgentSessionId)>,
    used_panes: &mut HashSet<PaneId>,
) {
    for candidate in candidates {
        let Some(resumed) = candidate.evidence.resumed_session_id else {
            continue;
        };
        let Some((agent_index, agent)) = resumed_agent_for_candidate(
            index,
            candidate,
            resumed,
            live_stamped_agents,
            used_agents,
        ) else {
            continue;
        };
        pairings.insert(candidate.evidence.pane.pane_id.clone(), agent_index);
        used_panes.insert(candidate.evidence.pane.pane_id.clone());
        used_agents.insert((agent.kind.clone(), agent.agent_id.clone()));
    }
}

fn resumed_agent_for_candidate<'a>(
    index: &'a PaneBindingIndex<'a>,
    candidate: &LazyPaneCandidate<'_>,
    resumed: &AgentSessionId,
    live_stamped_agents: &BTreeSet<(AgentKind, AgentSessionId)>,
    used_agents: &BTreeSet<(AgentKind, AgentSessionId)>,
) -> Option<(usize, &'a AgentState)> {
    let kind = AgentKind::new_unchecked(candidate.kind);
    let agent_index = index.root_index(&kind, resumed)?;
    let agent = index.agent(agent_index)?;
    (agent.worktree_path.as_deref() == Some(candidate.cwd)
        && !live_stamped_agents.contains(&(agent.kind.clone(), agent.agent_id.clone()))
        && !used_agents.contains(&(agent.kind.clone(), agent.agent_id.clone())))
    .then_some((agent_index, agent))
}

#[derive(Clone, Copy)]
struct LazyPaneCandidate<'a> {
    evidence: PaneBindingEvidence<'a>,
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
        selected_pane: selected.evidence.pane.pane_id.clone(),
        selected_pane_process_start: selected.evidence.process_start,
        method,
        candidates: viable
            .iter()
            .map(|candidate| LazyAgentPairingCandidateDiagnostic {
                pane_id: candidate.evidence.pane.pane_id.clone(),
                pane_process_start: candidate.evidence.process_start,
                resumed_session_id: candidate.evidence.resumed_session_id.cloned(),
            })
            .collect(),
    }
}

/// The common live-pane identity for agent commands: foreground command names a
/// known agent kind, the pane is not marked as a foreign-user elevated agent,
/// and the pane has a non-empty worktree path from the mux cwd or Rimz's
/// supervised-wrapper manifest.
fn agent_pane_identity(pane: &PaneRef) -> Option<(&'static str, &'static AgentDescriptor, &str)> {
    agent_pane_identity_from_evidence(pane_binding_evidence(pane))
}

fn agent_pane_identity_from_evidence(
    evidence: PaneBindingEvidence<'_>,
) -> Option<(&'static str, &'static AgentDescriptor, &str)> {
    if evidence.pane.elevated_agent.is_some() {
        return None;
    }
    let kind = evidence.agent_kind?;
    let descriptor = crate::agents::descriptor_by_kind(kind)?;
    let worktree_path = evidence.projection_worktree?;
    Some((kind, descriptor, worktree_path))
}

/// The resting row for a wired agent pane that no session claimed: `○ <kind>`
/// with adapter-owned model/window defaults when known.
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
            task: None,
            prompt: None,
            description: None,
            model: default_model.map(ToOwned::to_owned),
            effort: None,
            handle: None,
            team: None,
            launch_group: None,
            launch_ordinal: None,
            context_pct: None,
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
            sub_agents: Vec::new(),
            compacting: false,
            compaction_count: 0,
            turn_error_label: None,
        })),
    }
}

/// Project a realtime command overlay for an already frame-admitted pane. This
/// uses the same agent-pane identity gate as the verified pull path, but only
/// synthesizes the unbound idle row for wired agents.
pub(crate) fn row_from_frame_pane(
    pane: &PaneRef,
    wired_kinds: &[String],
    wired_default_models: &BTreeMap<String, String>,
    now: Timestamp,
) -> Option<SidebarRow> {
    if let Some((kind, descriptor, worktree_path)) = agent_pane_identity(pane)
        && wired_kinds.iter().any(|wired| wired == kind)
    {
        return Some(idle_agent_row(
            pane,
            descriptor,
            worktree_path,
            wired_default_models
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
