//! Pane binding: which store agent owns which live pane, the own-view
//! projection, and the daemon-view predicates.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::process::{pane_agent_kind, pane_worktree_path};
use crate::agents::AgentState;
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::pane::{PaneRef, RuntimeOwnerKind};
use crate::store::session_death::agent_owner_pid;
use crate::store::snapshot::row::SidebarRow;

mod lazy;

pub(super) use lazy::compute_lazy_agent_pairings_with_index;
pub(super) use lazy::row_from_frame_pane;
pub use lazy::{
    HookPaneRecoveryCandidate, HookPaneRecoveryContext, HookPaneRecoveryMethod,
    HookPaneRecoveryPhase, HookPaneRecoverySelection,
};
pub(crate) use lazy::{
    LazyAgentPairingDiagnostic, LazyAgentPairingResult, compute_lazy_agent_pairings,
};

type AgentKey = (AgentKind, AgentSessionId);

pub(super) struct PaneBindingIndex<'a> {
    agents: &'a [AgentState],
    roots: BTreeMap<AgentKey, usize>,
    stamped_by_pane: HashMap<PaneId, Vec<usize>>,
    lazy_by_worktree: BTreeMap<(AgentKind, String), Vec<usize>>,
}

impl<'a> PaneBindingIndex<'a> {
    pub(super) fn new(agents: &'a [AgentState]) -> Self {
        let mut roots = BTreeMap::new();
        let mut stamped_by_pane: HashMap<PaneId, Vec<usize>> = HashMap::new();
        let mut lazy_by_worktree: BTreeMap<(AgentKind, String), Vec<usize>> = BTreeMap::new();
        for (index, agent) in agents.iter().enumerate() {
            if let Some(pane) = &agent.pane {
                stamped_by_pane
                    .entry(pane.pane_id.clone())
                    .or_default()
                    .push(index);
            }
            if agent.parent_agent_id.is_some() {
                continue;
            }
            roots
                .entry((agent.kind.clone(), agent.agent_id.clone()))
                .or_insert(index);
            if let Some(worktree) = agent.worktree_path.as_ref() {
                lazy_by_worktree
                    .entry((agent.kind.clone(), worktree.clone()))
                    .or_default()
                    .push(index);
            }
        }
        Self {
            agents,
            roots,
            stamped_by_pane,
            lazy_by_worktree,
        }
    }

    pub(super) fn agent(&self, index: usize) -> Option<&'a AgentState> {
        self.agents.get(index)
    }

    pub(super) fn root_index(&self, kind: &AgentKind, agent_id: &AgentSessionId) -> Option<usize> {
        self.roots.get(&(kind.clone(), agent_id.clone())).copied()
    }

    pub(super) fn lazy_indices(&self, kind: &AgentKind, worktree: &str) -> &[usize] {
        self.lazy_by_worktree
            .get(&(kind.clone(), worktree.to_owned()))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub(super) fn stamped_agent(&self, pane: &PaneRef) -> Option<&'a AgentState> {
        self.stamped_by_pane
            .get(&pane.pane_id)?
            .iter()
            .filter_map(|index| self.agents.get(*index))
            .filter(|agent| agent.parent_agent_id.is_none())
            .filter(|agent| {
                agent
                    .pane
                    .as_ref()
                    .is_some_and(|stamped| stamped_agent_matches_live_pane(agent, stamped, pane))
            })
            .min_by(|left, right| left.compare_same_pane_owner(right))
    }

    pub(super) fn stamped_agents<'b>(
        &'b self,
        pane: &'b PaneRef,
    ) -> impl Iterator<Item = &'a AgentState> + 'b {
        self.stamped_by_pane
            .get(&pane.pane_id)
            .into_iter()
            .flatten()
            .filter_map(|index| self.agents.get(*index))
            .filter(|agent| {
                agent.parent_agent_id.is_none()
                    && agent.pane.as_ref().is_some_and(|stamped| {
                        stamped_agent_matches_live_pane(agent, stamped, pane)
                    })
            })
    }

    /// The newest RimZ launch, root or launched child, stamped on this live pane.
    /// Unlike [`Self::stamped_agent`], this deliberately ignores the adapter's
    /// same-pane primary policy: a later launch is the pane-incarnation clock
    /// even while an older session remains retained in the rollup.
    pub(super) fn latest_launch_agent(
        &self,
        pane: &PaneRef,
        kind: &AgentKind,
    ) -> Option<&'a AgentState> {
        self.stamped_by_pane
            .get(&pane.pane_id)?
            .iter()
            .filter_map(|index| self.agents.get(*index))
            .filter(|agent| {
                (agent.parent_agent_id.is_none() || agent.is_launched_child())
                    && agent.kind == *kind
                    && (agent.launch_id.is_some() || agent.agent_id.is_provisional())
            })
            .filter(|agent| {
                agent
                    .pane
                    .as_ref()
                    .is_some_and(|stamped| stamped_agent_matches_live_pane(agent, stamped, pane))
            })
            .max_by(|left, right| {
                left.registered_at
                    .cmp(&right.registered_at)
                    .then_with(|| left.agent_id.cmp(&right.agent_id))
            })
    }

    pub(super) fn stamped_launched_child(&self, pane: &PaneRef) -> Option<&'a AgentState> {
        self.stamped_by_pane
            .get(&pane.pane_id)?
            .iter()
            .filter_map(|index| self.agents.get(*index))
            .filter(|agent| agent.is_launched_child())
            .filter(|agent| {
                agent
                    .pane
                    .as_ref()
                    .is_some_and(|stamped| stamped_agent_matches_live_pane(agent, stamped, pane))
            })
            .min_by(|left, right| left.compare_same_pane_owner(right))
    }

    pub(super) fn live_foreign_owner(
        &self,
        evidence: PaneBindingEvidence<'_>,
        kind: &AgentKind,
        agent_id: &AgentSessionId,
    ) -> Option<&'a AgentState> {
        self.stamped_by_pane
            .get(&evidence.pane.pane_id)?
            .iter()
            .filter_map(|index| self.agents.get(*index))
            .filter(|agent| {
                agent.kind == *kind
                    && agent.agent_id != *agent_id
                    && pane_start_allows_bind(agent.last_activity, evidence.pane)
            })
            .min_by(|left, right| left.compare_same_pane_owner(right))
    }
}

pub(super) enum PaneBindingDisposition<'a> {
    Agent(&'a AgentState),
    /// A pane-backed launched child remains addressable and normally renders
    /// inside its root parent's card. The row projection promotes it when that
    /// parent has no live row.
    NestedAgent(&'a AgentState),
    Idle(Box<SidebarRow>),
    NotTracked {
        kind: Option<AgentKind>,
        row: Box<SidebarRow>,
        lane: Option<crate::ids::Lane>,
    },
    DuplicatePane,
    Conflict {
        kind: AgentKind,
        agent_id: AgentSessionId,
        bound_pane: PaneId,
    },
    Quarantined,
    Process,
    Ignored,
}

pub(super) struct PaneWiring<'a> {
    pub(super) kinds: &'a [String],
    pub(super) default_models: &'a BTreeMap<String, String>,
    pub(super) untracked: &'a BTreeMap<String, crate::agents::NotTracked>,
    pub(super) own_lanes: &'a BTreeMap<String, crate::ids::Lane>,
}

pub(super) struct PaneBinder<'a> {
    index: PaneBindingIndex<'a>,
    pairings: &'a LazyAgentPairingResult,
    bound_agents: BTreeSet<AgentKey>,
    bound_agent_panes: HashMap<AgentKey, PaneId>,
    seen_panes: HashSet<PaneId>,
    wiring: PaneWiring<'a>,
    panes_produced_at_ms: Option<u64>,
    now: Timestamp,
}

impl<'a> PaneBinder<'a> {
    pub(super) fn new(
        index: PaneBindingIndex<'a>,
        pairings: &'a LazyAgentPairingResult,
        wiring: PaneWiring<'a>,
        panes_produced_at_ms: Option<u64>,
        now: Timestamp,
    ) -> Self {
        Self {
            index,
            pairings,
            bound_agents: BTreeSet::new(),
            bound_agent_panes: HashMap::new(),
            seen_panes: HashSet::new(),
            wiring,
            panes_produced_at_ms,
            now,
        }
    }

    pub(super) fn resolve(&mut self, pane: &PaneRef) -> PaneBindingDisposition<'a> {
        if !self.seen_panes.insert(pane.pane_id.clone()) {
            return PaneBindingDisposition::DuplicatePane;
        }
        if let Some(agent) = self.index.stamped_agent(pane) {
            return self.bind(agent, pane);
        }
        if let Some(agent) = self.index.stamped_launched_child(pane) {
            return self.bind_nested(agent, pane);
        }
        if let Some(bind) = lazy::agent_pane_for_pane(
            pane,
            self.index.agents,
            self.pairings,
            &self.bound_agents,
            &self.wiring,
            self.now,
        ) {
            return match bind {
                lazy::AgentPaneRow::Agent(agent) => self.bind(agent, pane),
                lazy::AgentPaneRow::Idle(row) => PaneBindingDisposition::Idle(row),
                lazy::AgentPaneRow::NotTracked { kind, row, lane } => {
                    PaneBindingDisposition::NotTracked { kind, row, lane }
                }
                lazy::AgentPaneRow::SuppressedDuplicate { kind, agent_id } => {
                    self.conflict(kind, agent_id)
                }
            };
        }
        if newborn_unknown_cwd(pane, self.panes_produced_at_ms) {
            PaneBindingDisposition::Quarantined
        } else if super::process::pane_command_is_known(pane) {
            PaneBindingDisposition::Process
        } else {
            PaneBindingDisposition::Ignored
        }
    }

    fn bind(&mut self, agent: &'a AgentState, pane: &PaneRef) -> PaneBindingDisposition<'a> {
        let key = (agent.kind.clone(), agent.agent_id.clone());
        if let Some(bound_pane) = self.bound_agent_panes.get(&key) {
            return PaneBindingDisposition::Conflict {
                kind: agent.kind.clone(),
                agent_id: agent.agent_id.clone(),
                bound_pane: bound_pane.clone(),
            };
        }
        self.bound_agents.insert(key.clone());
        self.bound_agent_panes.insert(key, pane.pane_id.clone());
        PaneBindingDisposition::Agent(agent)
    }

    fn bind_nested(&mut self, agent: &'a AgentState, pane: &PaneRef) -> PaneBindingDisposition<'a> {
        let key = (agent.kind.clone(), agent.agent_id.clone());
        self.bound_agents.insert(key.clone());
        self.bound_agent_panes.insert(key, pane.pane_id.clone());
        PaneBindingDisposition::NestedAgent(agent)
    }

    fn conflict(&self, kind: AgentKind, agent_id: AgentSessionId) -> PaneBindingDisposition<'a> {
        self.bound_agent_panes
            .get(&(kind.clone(), agent_id.clone()))
            .map_or(PaneBindingDisposition::Ignored, |bound_pane| {
                PaneBindingDisposition::Conflict {
                    kind,
                    agent_id,
                    bound_pane: bound_pane.clone(),
                }
            })
    }
}

fn newborn_unknown_cwd(pane: &PaneRef, panes_produced_at_ms: Option<u64>) -> bool {
    panes_produced_at_ms.is_some()
        && pane.first_seen_at_ms == panes_produced_at_ms
        && pane_worktree_path(pane).is_none()
        && super::process::pane_command_is_known(pane)
}

/// One sidebar's view of the panes sharing its tab/window. `None` on the
/// snapshot means the count could not be determined (no `--exclude-pane-id`, or
/// the caller's pane was absent from the live list); the renderer treats that
/// as "never self-close".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidebarOwnView {
    pub sibling_count: usize,
    /// The view's working (non-sidebar) sibling pane ids. Notify targeting,
    /// self-close, and stranded-focus repair use this set. `#[serde(default)]`
    /// keeps the wire shape stable.
    #[serde(default)]
    pub working_pane_ids: Vec<PaneId>,
    /// Whether the caller's own view is the `rimzd` daemon view: its siblings,
    /// after dropping any sidebar pane, are non-empty and all daemon-dashboard
    /// infrastructure panes (`pane_is_host`).
    /// `#[serde(default)]` keeps the wire shape stable for older producers.
    #[serde(default)]
    pub own_view_is_daemon: bool,
}

/// Whether `agent` is owned by a shared app-server daemon
/// ([`crate::agents::codex::codex_daemon_pids`] today): a root (non-subagent)
/// daemon-hooked session whose recorded hook owner is a daemon by kind or pid.
pub(super) fn is_daemon_owned(agent: &AgentState, daemon_pids: &BTreeSet<u32>) -> bool {
    let daemon_hooked = crate::agents::spec_by_kind(agent.kind.as_str())
        .is_some_and(|definition| definition.capabilities.daemon_hooked_sessions);
    if !daemon_hooked || agent.parent_agent_id.is_some() {
        return false;
    }
    if agent
        .runtime_owner
        .as_ref()
        .is_some_and(|owner| owner.kind == RuntimeOwnerKind::Daemon)
    {
        return true;
    }
    agent_owner_pid(agent).is_some_and(|pid| daemon_pids.contains(&pid))
}

/// Admission feeds rows, agent_panes, rimz agents, and message addressing. A user can start a real agent in the daemon view, so that view requires a stamped agent rather than blanket exclusion.
pub(super) fn pane_admits_card(
    pane: &PaneRef,
    exclude: Option<&PaneId>,
    bindings: &PaneBindingIndex<'_>,
) -> bool {
    if exclude.is_some_and(|excluded| pane.pane_id == *excluded) {
        return false;
    }
    if pane.is_rimz_sidebar() {
        return false;
    }
    if crate::pane::pane_runs_daemon_host(pane) {
        return false;
    }
    if pane.view_name.as_deref() == Some(crate::pane::VIEW_NAME) {
        return bindings.stamped_agent(pane).is_some()
            || bindings.stamped_launched_child(pane).is_some();
    }
    true
}

/// Facts a pane exposes to binding policies. Raw cwd stays separate from the
/// projection worktree because hook recovery intentionally requires the mux
/// cwd while lazy snapshot pairing also accepts supervised-wrapper identity.
#[derive(Clone, Copy)]
pub(super) struct PaneBindingEvidence<'a> {
    pub(super) pane: &'a PaneRef,
    pub(super) agent_kind: Option<&'static str>,
    pub(super) raw_cwd: Option<&'a str>,
    pub(super) projection_worktree: Option<&'a str>,
    pub(super) process_start: Option<Timestamp>,
    pub(super) resumed_session_id: Option<&'a AgentSessionId>,
}

pub(super) fn pane_binding_evidence(pane: &PaneRef) -> PaneBindingEvidence<'_> {
    PaneBindingEvidence {
        pane,
        agent_kind: pane_agent_kind(pane),
        raw_cwd: pane.cwd.as_deref(),
        projection_worktree: pane_worktree_path(pane),
        process_start: pane.pane_process_start,
        resumed_session_id: pane.resumed_session_id.as_ref(),
    }
}

/// What a row's recorded owner says about the live pane its stamp names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OwnerProof {
    /// The pane's root process is the owner or one of its recorded parents.
    Proven,
    /// The owner's recorded parent chain does not reach the pane's root.
    Disproven,
    /// No owner of this unended row, a daemon or script owner, another
    /// subject, an empty chain, or a pane without a root pid.
    Unknown,
}

fn owner_proof(agent: &AgentState, pane: &PaneRef) -> OwnerProof {
    let Some(owner) = agent.runtime_owner.as_ref().filter(|owner| {
        agent.ended_at.is_none()
            && owner.kind == RuntimeOwnerKind::Agent
            && owner.subject_id == agent.agent_id.as_str()
    }) else {
        return OwnerProof::Unknown;
    };
    let Some(root) = pane.pane_pid else {
        return OwnerProof::Unknown;
    };
    if owner.pid == root || owner.ancestors.contains(&root) {
        OwnerProof::Proven
    } else if owner.ancestors.is_empty() {
        OwnerProof::Unknown
    } else {
        OwnerProof::Disproven
    }
}

/// The one gate a stamped row passes to bind the live pane its stamp names,
/// for every agent kind. A proven owner binds with no clock or program
/// evidence, a disproven one never binds, and an unknown one binds only while
/// the pane's start, activity clocks and program agree with the row.
/// Ancestry proves containment, not tenancy, so a pane whose hosted process
/// names another kind rejects the row in every tier.
fn stamped_agent_matches_live_pane(agent: &AgentState, stamped: &PaneRef, pane: &PaneRef) -> bool {
    if stamped.pane_id != pane.pane_id {
        return false;
    }
    let hosts_other_kind = pane
        .hosted_agent_kind
        .as_ref()
        .is_some_and(|kind| kind != &agent.kind);
    match owner_proof(agent, pane) {
        OwnerProof::Proven => !hosts_other_kind,
        OwnerProof::Disproven => false,
        OwnerProof::Unknown => {
            pane_start_matches_agent_stamp(stamped, pane)
                && (crate::agents::spec_by_kind(agent.kind.as_str()).is_none()
                    || (!hosts_other_kind && unproven_tenancy_holds(agent, pane)))
        }
    }
}

/// An unproven row holds its pane only while the pane's program names its
/// kind and no clock shows the row belongs to an older instance: activity
/// older than the pane's start or its hosted process's start.
fn unproven_tenancy_holds(agent: &AgentState, pane: &PaneRef) -> bool {
    if !pane_start_allows_bind(agent.last_activity, pane)
        || pane
            .hosted_agent_process_start
            .is_some_and(|start| agent.last_activity < start)
    {
        return false;
    }
    match pane_agent_kind(pane) {
        Some(kind) => kind == agent.kind.as_str(),
        None => pane
            .hosted_agent_kind
            .as_ref()
            .is_some_and(|kind| kind == &agent.kind),
    }
}

/// Defensive guard for exact-identity read-time binds: when the pane's process
/// start is known, a session whose `last_activity` predates that start belongs
/// to an older instance, not the process now in the pane. Fail-open on an
/// unknown start; the lazy cwd fallback adds its own evidence gate on top.
fn pane_start_allows_bind(last_activity: Timestamp, pane: &PaneRef) -> bool {
    pane.pane_process_start
        .is_none_or(|start| last_activity >= start)
}

fn pane_start_matches_agent_stamp(expected: &PaneRef, actual: &PaneRef) -> bool {
    match (expected.pane_process_start, actual.pane_process_start) {
        (Some(expected), Some(actual)) => expected <= actual,
        _ => true,
    }
}

#[cfg(test)]
mod tests;
