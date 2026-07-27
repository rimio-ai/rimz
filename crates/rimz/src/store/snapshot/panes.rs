//! Pane binding: which store agent owns which live pane, the own-view
//! projection, and the daemon-view predicates.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::process::{pane_agent_kind, pane_worktree_path};
use crate::agents::{AgentState, SamePaneSessionPolicy};
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
            .min_by(|left, right| compare_same_pane_owners(left, right))
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
            .min_by(|left, right| compare_same_pane_owners(left, right))
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
            .min_by(|left, right| {
                registered_rank(left)
                    .cmp(&registered_rank(right))
                    .then_with(|| left.agent_id.cmp(&right.agent_id))
            })
    }
}

pub(super) enum PaneBindingDisposition<'a> {
    Agent(&'a AgentState),
    /// A pane-backed launched child remains addressable and normally renders
    /// inside its root parent's card. The row projection promotes it when that
    /// parent has no live row.
    NestedAgent(&'a AgentState),
    Idle(Box<SidebarRow>),
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

pub(super) struct PaneBinder<'a> {
    index: PaneBindingIndex<'a>,
    pairings: &'a LazyAgentPairingResult,
    bound_agents: BTreeSet<AgentKey>,
    bound_agent_panes: HashMap<AgentKey, PaneId>,
    seen_panes: HashSet<PaneId>,
    wired_kinds: &'a [String],
    default_models: &'a BTreeMap<String, String>,
    panes_produced_at_ms: Option<u64>,
    now: Timestamp,
}

impl<'a> PaneBinder<'a> {
    pub(super) fn new(
        index: PaneBindingIndex<'a>,
        pairings: &'a LazyAgentPairingResult,
        wired_kinds: &'a [String],
        default_models: &'a BTreeMap<String, String>,
        panes_produced_at_ms: Option<u64>,
        now: Timestamp,
    ) -> Self {
        Self {
            index,
            pairings,
            bound_agents: BTreeSet::new(),
            bound_agent_panes: HashMap::new(),
            seen_panes: HashSet::new(),
            wired_kinds,
            default_models,
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
            self.wired_kinds,
            self.default_models,
            self.now,
        ) {
            return match bind {
                lazy::AgentPaneRow::Agent(agent) => self.bind(agent, pane),
                lazy::AgentPaneRow::Idle(row) => PaneBindingDisposition::Idle(row),
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
    /// infrastructure panes ([`crate::daemon_view::pane_is_host`]).
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

/// The card-admission verdict for one live pane: admitted, or the named reason
/// it renders nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CardAdmission {
    Admitted,
    ExcludedPaneId,
    SidebarChrome,
    RemoteControlOrAppServerHost,
}

impl CardAdmission {
    pub(super) fn admits(self) -> bool {
        self == Self::Admitted
    }
}

pub(super) fn pane_admits_card(pane: &PaneRef, exclude: Option<&PaneId>) -> CardAdmission {
    if exclude.is_some_and(|excluded| pane.pane_id == *excluded) {
        return CardAdmission::ExcludedPaneId;
    }
    if pane
        .command
        .as_deref()
        .is_some_and(super::process::command_is_sidebar_chrome)
    {
        return CardAdmission::SidebarChrome;
    }
    if crate::daemon_view::pane_is_host(pane) {
        return CardAdmission::RemoteControlOrAppServerHost;
    }
    CardAdmission::Admitted
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

/// The root agent stamped on this exact live pane id, regardless of whether
/// another row already bound it. For lazy-registering kinds, the stamp survives
/// non-agent child foregrounds only while the pane carries the hosted-process
/// signal for that agent kind; a positively different agent command
/// disqualifies it.
pub fn stamped_agent_for_pane<'a>(
    pane: &PaneRef,
    agents: &'a [AgentState],
) -> Option<&'a AgentState> {
    PaneBindingIndex::new(agents).stamped_agent(pane)
}

fn compare_same_pane_owners(left: &AgentState, right: &AgentState) -> Ordering {
    let follows_latest = left.kind == right.kind
        && crate::agents::spec_by_kind(left.kind.as_str()).is_some_and(|definition| {
            definition.capabilities.same_pane_session == SamePaneSessionPolicy::FollowLatest
        });
    if !follows_latest {
        return registered_rank(left)
            .cmp(&registered_rank(right))
            .then_with(|| left.agent_id.cmp(&right.agent_id));
    }
    compare_latest_registration(left, right)
        .then_with(|| right.last_activity.cmp(&left.last_activity))
        .then_with(|| right.agent_id.cmp(&left.agent_id))
}

fn compare_latest_registration(left: &AgentState, right: &AgentState) -> Ordering {
    match (left.registered_at, right.registered_at) {
        (Some(left), Some(right)) => right.cmp(&left),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Registration sort key: an earlier `registered_at` ranks first (the pane's
/// primary owner), and an absent stamp (a rollup persisted before the field
/// existed) ranks last so a known primary always outranks it. `is_none()` is
/// `false` for `Some` and `true` for `None`, so `Some` sorts ahead of `None`;
/// within `Some`, the earlier timestamp sorts first.
fn registered_rank(agent: &AgentState) -> (bool, Option<Timestamp>) {
    (agent.registered_at.is_none(), agent.registered_at)
}

fn stamped_agent_matches_live_pane(agent: &AgentState, stamped: &PaneRef, pane: &PaneRef) -> bool {
    if stamped.pane_id != pane.pane_id || !pane_start_matches_agent_stamp(stamped, pane) {
        return false;
    }
    let Some(definition) = crate::agents::spec_by_kind(agent.kind.as_str()) else {
        return true;
    };
    if !definition.capabilities.registers_lazily {
        return true;
    }
    let stamp_owned_by_live_root = stamp_owned_by_live_pane_root(agent, stamped, pane);
    if !stamp_owned_by_live_root && !pane_start_allows_bind(agent.last_activity, pane) {
        return false;
    }
    if !stamp_owned_by_live_root
        && pane
            .hosted_agent_process_start
            .is_some_and(|start| agent.last_activity < start)
    {
        return false;
    }
    if pane
        .hosted_agent_kind
        .as_ref()
        .is_some_and(|kind| kind != &agent.kind)
    {
        return false;
    }
    // A live foreground that positively names a different agent kind is not
    // ours. A foreground naming this agent still binds; otherwise a stamped
    // lazy agent holds only when the producer confirmed its in-pane process is
    // still alive under the pane root.
    match pane_agent_kind(pane) {
        Some(kind) => kind == agent.kind.as_str(),
        None => pane
            .hosted_agent_kind
            .as_ref()
            .is_some_and(|kind| kind == &agent.kind),
    }
}

/// Whether the card's owning agent process is still this pane's root. A resume
/// wrapper records the same pid as both runtime owner and pane root, and that
/// pid survives its exec into the provider. Hook-enriched stamps can carry the
/// root pid too, but a shell-hosted agent's runtime owner is the child CLI, so
/// it keeps the activity-clock guard below.
fn stamp_owned_by_live_pane_root(agent: &AgentState, stamped: &PaneRef, pane: &PaneRef) -> bool {
    let (Some(stamped_pid), Some(live_pid), Some(owner)) = (
        stamped.pane_pid,
        pane.pane_pid,
        agent.runtime_owner.as_ref(),
    ) else {
        return false;
    };
    stamped_pid == live_pid
        && owner.kind == crate::pane::RuntimeOwnerKind::Agent
        && owner.subject_id == agent.agent_id.as_str()
        && owner.pid == live_pid
}

/// Defensive guard for read-time binds: when the pane's process start is known,
/// a session whose `last_activity` predates that start belongs to an older
/// instance, not the process now in the pane — so it must not bind.
pub fn pane_start_allows_bind(last_activity: Timestamp, pane: &PaneRef) -> bool {
    pane.pane_process_start
        .is_none_or(|start| last_activity >= start)
}

#[cfg(test)]
fn pane_start_matches(expected: &PaneRef, actual: &PaneRef) -> bool {
    match (expected.pane_process_start, actual.pane_process_start) {
        (Some(expected), Some(actual)) => expected == actual,
        _ => true,
    }
}

fn pane_start_matches_agent_stamp(expected: &PaneRef, actual: &PaneRef) -> bool {
    match (expected.pane_process_start, actual.pane_process_start) {
        (Some(expected), Some(actual)) => expected <= actual,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::agents::AgentStatus;
    use crate::ids::{AgentKind, MuxName, PaneId};
    use crate::store::snapshot::SidebarSnapshot;
    use crate::store::snapshot::testkit::{AgentStateFx, agent, ago};

    /// A pane fixture with an explicit command and optional window name, so a
    /// test can build daemon hosts, sidebars, and working shells across views.
    fn pane_cmd(raw: &str, view: &str, command: &str, view_name: Option<&str>) -> PaneRef {
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Zellij, raw),
            session_name: "rimz-test".to_owned(),
            view_id: Some(view.to_owned()),
            view_kind: Some(crate::ids::ViewKind::Tab),
            view_name: view_name.map(str::to_owned),
            title: None,
            is_floating: false,
            command: Some(command.to_owned()),
            foreground_cmdline: None,
            spawn_command: None,
            cwd: Some("/repo/main".to_owned()),
            pane_pid: None,
            pane_process_start: None,
            hosted_agent_kind: None,
            hosted_agent_process_start: None,
            resumed_session_id: None,
            elevated_agent: None,
            first_seen_at_ms: None,
        }
    }

    #[test]
    fn only_daemon_view_classifies_view_sets() {
        for (label, panes, expected) in [
            (
                "daemon view only",
                vec![
                    pane_cmd("terminal_0", "tab_0", "rimz-sidebar", None),
                    pane_cmd(
                        "terminal_1",
                        "tab_0",
                        "claude remote-control --spawn worktree",
                        None,
                    ),
                    pane_cmd("terminal_2", "tab_0", "rimz codex app-server serve", None),
                ],
                true,
            ),
            (
                "working view exists",
                vec![
                    pane_cmd("terminal_0", "tab_0", "rimz-sidebar", None),
                    pane_cmd(
                        "terminal_1",
                        "tab_0",
                        "claude remote-control --spawn worktree",
                        None,
                    ),
                    pane_cmd("terminal_3", "tab_1", "rimz-sidebar", None),
                    pane_cmd("terminal_4", "tab_1", "zsh", None),
                ],
                false,
            ),
            (
                "no daemon view",
                vec![
                    pane_cmd("terminal_0", "tab_0", "rimz-sidebar", None),
                    pane_cmd("terminal_1", "tab_0", "zsh", None),
                ],
                false,
            ),
            ("empty session", Vec::new(), false),
            (
                "sidebar-only limbo view ignored",
                vec![
                    pane_cmd("terminal_0", "tab_0", "rimz-sidebar", None),
                    pane_cmd("terminal_1", "tab_0", "rimz codex app-server serve", None),
                    pane_cmd("terminal_3", "tab_1", "rimz-sidebar", None),
                ],
                true,
            ),
        ] {
            assert_eq!(
                SidebarSnapshot::only_daemon_view(&panes),
                expected,
                "{label}"
            );
        }
    }

    #[test]
    fn card_admission_names_card_blockers() {
        let working = pane_cmd("terminal_1", "tab_0", "zsh", None);
        let unreadable = PaneRef {
            command: None,
            ..pane_cmd("terminal_2", "tab_0", "zsh", None)
        };
        for (label, pane, exclude, expected) in [
            (
                "working pane",
                working.clone(),
                None,
                CardAdmission::Admitted,
            ),
            (
                "excluded pane id",
                working.clone(),
                Some(working.pane_id.clone()),
                CardAdmission::ExcludedPaneId,
            ),
            (
                "sidebar chrome",
                pane_cmd("terminal_3", "tab_0", "rimz-sidebar", None),
                None,
                CardAdmission::SidebarChrome,
            ),
            (
                "remote-control host",
                pane_cmd("terminal_4", "tab_0", "rimz codex app-server serve", None),
                None,
                CardAdmission::RemoteControlOrAppServerHost,
            ),
            (
                "identityless pane",
                unreadable,
                None,
                CardAdmission::Admitted,
            ),
        ] {
            assert_eq!(
                pane_admits_card(&pane, exclude.as_ref()),
                expected,
                "{label}"
            );
        }
    }

    #[test]
    fn agent_stamp_tolerates_floor_to_exact_start_drift_but_items_do_not() {
        let floor: Timestamp = "2026-06-05T13:49:53Z".parse().unwrap();
        let exact: Timestamp = "2026-06-05T14:22:43Z".parse().unwrap();
        let stamped = PaneRef {
            pane_process_start: Some(floor),
            ..pane_cmd("terminal_1", "tab_0", "codex", None)
        };
        let live = PaneRef {
            pane_process_start: Some(exact),
            ..pane_cmd("terminal_1", "tab_0", "codex", None)
        };

        assert!(
            pane_start_matches_agent_stamp(&stamped, &live),
            "old floor-era agent stamps still attach to the now-exact live process"
        );
        assert!(
            !pane_start_matches(&stamped, &live),
            "standalone item pane refs still require exact start identity"
        );
    }

    #[test]
    fn stamped_lazy_agent_binds_with_carried_hosted_stamp_and_pidless_pane() {
        let agent = agent("codex", "sess-1", AgentStatus::Running, 1_000)
            .worktree("/repo/main")
            .in_pane("%1");
        let live = PaneRef {
            pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
            command: Some("git".to_owned()),
            pane_pid: None,
            hosted_agent_kind: Some(AgentKind::new_unchecked("codex")),
            hosted_agent_process_start: Some(ago(120)),
            ..pane_cmd("%1", "tab_0", "git", None)
        };

        assert!(stamped_agent_matches_live_pane(
            &agent,
            agent.pane.as_ref().expect("stamped pane"),
            &live,
        ));
    }

    #[test]
    fn resumed_stamp_from_live_pane_root_binds_before_new_activity() {
        let mut agent = agent("codex", "sess-resumed", AgentStatus::Idle, 1)
            .worktree("/repo/main")
            .active_ago(120)
            .in_pane("%4");
        agent.pane.as_mut().expect("stamped pane").pane_pid = Some(84);
        agent.runtime_owner = Some(crate::pane::RuntimeOwner::new(
            crate::pane::RuntimeOwnerKind::Agent,
            "sess-resumed",
            84,
            None,
        ));
        let live = PaneRef {
            pane_id: PaneId::from_parts(MuxName::Tmux, "%4"),
            command: Some("codex".to_owned()),
            pane_pid: Some(84),
            pane_process_start: Some(ago(60)),
            hosted_agent_kind: Some(AgentKind::new_unchecked("codex")),
            hosted_agent_process_start: Some(ago(60)),
            ..pane_cmd("%4", "tab_0", "codex", None)
        };

        assert!(stamped_agent_matches_live_pane(
            &agent,
            agent.pane.as_ref().expect("stamped pane"),
            &live,
        ));
    }

    #[test]
    fn shell_root_stamp_keeps_clock_guard_when_agent_owner_is_child() {
        let mut agent = agent("codex", "sess-retired", AgentStatus::Idle, 1)
            .worktree("/repo/main")
            .active_ago(120)
            .in_pane("%4");
        agent.pane.as_mut().expect("stamped pane").pane_pid = Some(84);
        agent.runtime_owner = Some(crate::pane::RuntimeOwner::new(
            crate::pane::RuntimeOwnerKind::Agent,
            "sess-retired",
            85,
            None,
        ));
        let live = PaneRef {
            pane_id: PaneId::from_parts(MuxName::Tmux, "%4"),
            command: Some("zsh".to_owned()),
            pane_pid: Some(84),
            pane_process_start: Some(ago(60)),
            hosted_agent_kind: Some(AgentKind::new_unchecked("codex")),
            hosted_agent_process_start: Some(ago(60)),
            ..pane_cmd("%4", "tab_0", "zsh", None)
        };

        assert!(!stamped_agent_matches_live_pane(
            &agent,
            agent.pane.as_ref().expect("stamped pane"),
            &live,
        ));
    }

    #[test]
    fn carried_stamp_without_live_root_identity_keeps_clock_guard() {
        for pane_pid in [None, Some(83)] {
            let mut agent = agent("codex", "sess-stale", AgentStatus::Idle, 1)
                .worktree("/repo/main")
                .active_ago(120)
                .in_pane("%4");
            agent.pane.as_mut().expect("stamped pane").pane_pid = pane_pid;
            let live = PaneRef {
                pane_id: PaneId::from_parts(MuxName::Tmux, "%4"),
                command: Some("codex".to_owned()),
                pane_pid: Some(84),
                pane_process_start: Some(ago(60)),
                hosted_agent_kind: Some(AgentKind::new_unchecked("codex")),
                hosted_agent_process_start: Some(ago(60)),
                ..pane_cmd("%4", "tab_0", "codex", None)
            };

            assert!(!stamped_agent_matches_live_pane(
                &agent,
                agent.pane.as_ref().expect("stamped pane"),
                &live,
            ));
        }
    }

    #[test]
    fn live_root_stamp_still_rejects_a_different_hosted_kind() {
        let mut agent = agent("codex", "sess-resumed", AgentStatus::Idle, 1)
            .worktree("/repo/main")
            .active_ago(120)
            .in_pane("%4");
        agent.pane.as_mut().expect("stamped pane").pane_pid = Some(84);
        agent.runtime_owner = Some(crate::pane::RuntimeOwner::new(
            crate::pane::RuntimeOwnerKind::Agent,
            "sess-resumed",
            84,
            None,
        ));
        let live = PaneRef {
            pane_id: PaneId::from_parts(MuxName::Tmux, "%4"),
            command: Some("node".to_owned()),
            pane_pid: Some(84),
            pane_process_start: Some(ago(60)),
            hosted_agent_kind: Some(AgentKind::new_unchecked("claude")),
            hosted_agent_process_start: Some(ago(60)),
            ..pane_cmd("%4", "tab_0", "node", None)
        };

        assert!(!stamped_agent_matches_live_pane(
            &agent,
            agent.pane.as_ref().expect("stamped pane"),
            &live,
        ));
    }
}
