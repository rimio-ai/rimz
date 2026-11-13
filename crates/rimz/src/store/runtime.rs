//! Runtime projection for durable store records.
//!
//! Expel is read-time filtering: default live views keep only records whose
//! recorded owner process is still the same live process. Audit views bypass
//! this filter and read durable history as written.

use crate::agents::state::LaunchHolders;
use crate::agents::{AgentState, AgentStatus};
use crate::ids::{AgentKind, AgentSessionId};
use crate::pane::{RuntimeOwner, RuntimeOwnerKind};
use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeScope {
    Runtime,
    Audit,
}

/// Shared durable rows, including ended sessions, without derived replaced marks.
/// Role decisions require the replaced marks of [`RuntimeProjection`] in [`RuntimeScope::Audit`],
/// not these raw rows.
#[derive(Clone, Debug, Default)]
pub struct AuditRows {
    agents: super::snapshot::AgentRollup,
    replaced: Arc<OnceLock<HashSet<(AgentKind, AgentSessionId)>>>,
}

impl AuditRows {
    pub(super) fn new(agents: super::snapshot::AgentRollup) -> Self {
        Self {
            agents,
            replaced: Arc::default(),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &AgentState> + Clone {
        self.agents.iter()
    }

    pub fn find(&self, kind: &str, agent_id: &AgentSessionId) -> Option<&AgentState> {
        self.agents.find(kind, agent_id)
    }

    /// Copy one row with the same replaced mark as the full Audit projection.
    pub(crate) fn marked_copy(&self, agent: &AgentState) -> AgentState {
        let replaced = self.replaced.get_or_init(|| {
            let holders = LaunchHolders::new(self.iter());
            self.iter()
                .filter(|row| holders.replaced(row))
                .map(|row| (row.kind.clone(), row.agent_id.clone()))
                .collect()
        });
        let mut projected = agent.clone();
        projected.replaced = replaced.contains(&(agent.kind.clone(), agent.agent_id.clone()));
        super::snapshot::count_rows_materialized(1);
        projected
    }
}

/// Read shared durable rows without a writer-capable [`crate::Store`].
pub(crate) fn audit_rows(paths: &crate::StatePaths) -> crate::store::snapshot::Result<AuditRows> {
    let (_, agents, _) = super::snapshot::catch_up_rollup(paths)?;
    Ok(AuditRows::new(agents))
}

/// Tri-state process liveness for an agent session record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentLiveness {
    Live { pid: u32 },
    Dead,
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RuntimeProjection {
    pub ended: BTreeSet<(AgentKind, AgentSessionId)>,
    pub expelled: BTreeSet<(AgentKind, AgentSessionId)>,
    pub agents: Vec<AgentState>,
    /// Every terminal [`super::snapshot::ResumeOutcome`] within retention.
    ///
    /// Independent of liveness scope, for durable retry/demotion decisions.
    pub resume_outcomes: Vec<super::snapshot::ResumeOutcome>,
}

/// Read the durable agent rollup without opening a writer-capable [`crate::Store`].
pub(crate) fn audit_projection(
    paths: &crate::StatePaths,
) -> crate::store::snapshot::Result<RuntimeProjection> {
    let (_, agents, resume_outcomes) = super::snapshot::catch_up_rollup(paths)?;
    Ok(RuntimeProjection::from_parts(
        &agents,
        RuntimeScope::Audit,
        resume_outcomes,
    ))
}

impl RuntimeProjection {
    /// Audit keeps every row, so it owns a copy of each; runtime owns only
    /// the rows it keeps.
    pub(super) fn from_parts(
        agents: &super::snapshot::AgentRollup,
        scope: RuntimeScope,
        resume_outcomes: Vec<super::snapshot::ResumeOutcome>,
    ) -> Self {
        match scope {
            RuntimeScope::Audit => {
                let holders = LaunchHolders::new(agents.iter());
                let mut projected = agents.to_vec();
                for agent in &mut projected {
                    agent.replaced = holders.replaced(agent);
                }
                Self {
                    ended: ended_keys(agents.iter()),
                    expelled: BTreeSet::new(),
                    agents: projected,
                    resume_outcomes,
                }
            }
            RuntimeScope::Runtime => Self::runtime_from_refs(agents.iter(), resume_outcomes),
        }
    }

    /// The runtime-scoped projection over borrowed rows, cloning only the
    /// rows it keeps — a long-lived reader's rollup stays shared. A launched
    /// child that is not visible on its own stays while its parent is visible
    /// and has no parent of its own; an ended grandchild drops out. A row a
    /// swap replaced drops out whatever its liveness, and a live one is
    /// expelled so provider rediscovery cannot graft it back. A bound
    /// replacement that failed stays under the handle when its owner is dead.
    pub(crate) fn runtime_from_refs<'a>(
        agents: impl IntoIterator<Item = &'a AgentState>,
        resume_outcomes: Vec<super::snapshot::ResumeOutcome>,
    ) -> Self {
        let agents = agents.into_iter().collect::<Vec<_>>();
        let holders = LaunchHolders::new(agents.iter().copied());
        let agents = agents
            .into_iter()
            .map(|agent| {
                let replaced = holders.replaced(agent);
                (
                    agent,
                    replaced,
                    !replaced && (agent_is_runtime_visible(agent) || failed_swap_holder(agent)),
                )
            })
            .collect::<Vec<_>>();
        let visible_parents = agents
            .iter()
            .filter(|(agent, _, visible)| agent.parent_agent_id.is_none() && *visible)
            .flat_map(|(agent, ..)| agent.identity_ids().map(move |id| (&agent.kind, id)))
            .collect::<HashSet<_>>();
        let visible_parent_launches = agents
            .iter()
            .filter(|(agent, _, visible)| agent.parent_agent_id.is_none() && *visible)
            .filter_map(|(agent, ..)| agent.launch_id.as_ref())
            .collect::<HashSet<_>>();
        let mut expelled = BTreeSet::new();
        let kept = agents
            .iter()
            .filter_map(|(agent, replaced, independently_visible)| {
                let visible = *independently_visible
                    || (!*replaced
                        && agent.is_launched_child()
                        && agent.parent_agent_id.as_ref().is_some_and(|id| {
                            visible_parent_launches.contains(id)
                                || visible_parents.contains(&(
                                    agent.parent_agent_kind.as_ref().unwrap_or(&agent.kind),
                                    id,
                                ))
                        }));
                if !visible && agent.ended_at.is_none() {
                    expelled.insert((agent.kind.clone(), agent.agent_id.clone()));
                }
                visible.then(|| AgentState {
                    replaced: false,
                    ..(*agent).clone()
                })
            })
            .collect::<Vec<_>>();
        super::snapshot::count_rows_materialized(kept.len() as u64);
        Self {
            ended: ended_keys(agents.iter().map(|(agent, ..)| *agent)),
            expelled,
            agents: kept,
            resume_outcomes,
        }
    }
}

fn ended_keys<'a>(
    agents: impl IntoIterator<Item = &'a AgentState>,
) -> BTreeSet<(AgentKind, AgentSessionId)> {
    agents
        .into_iter()
        .filter_map(|agent| {
            agent
                .ended_at
                .map(|_| (agent.kind.clone(), agent.agent_id.clone()))
        })
        .collect()
}

/// Runtime visibility for an agent. Liveness suppresses; it never gates an
/// agent in. An unknown pid abstains (foreground/pane corroboration carries
/// liveness — see `docs/internals/agents/model.md`); a known owner that is known-dead
/// suppresses the stale overlay. A launched child's liveness follows its visible
/// parent, matching a native child whose runtime owner is the parent's process.
/// The same rule as `sidebar::produce::hosting::runtime_visible`: a change to one is made in
/// both.
fn agent_is_runtime_visible(agent: &AgentState) -> bool {
    agent.ended_at.is_none() && agent.runtime_owner.as_ref().is_none_or(owner_is_live)
}

/// A swap's bound replacement that failed while it held its agent: its card
/// stays under the handle after the wrapper that owned it exits, since no
/// other session of the agent remains to show. The caller has ruled the row
/// is not replaced.
fn failed_swap_holder(agent: &AgentState) -> bool {
    agent.epoch > 0
        && agent.agent_id.is_provisional()
        && agent.launch_bound
        && agent.status == AgentStatus::Failed
}

pub fn current_process_owner(
    kind: RuntimeOwnerKind,
    subject_id: impl Into<String>,
) -> RuntimeOwner {
    let pid = std::process::id();
    RuntimeOwner::new(kind, subject_id, pid, crate::proc::process_start_token(pid))
}

pub fn process_owner(
    kind: RuntimeOwnerKind,
    subject_id: impl Into<String>,
    pid: u32,
) -> RuntimeOwner {
    RuntimeOwner::new(kind, subject_id, pid, crate::proc::process_start_token(pid))
}

pub(super) fn owner_is_live(owner: &RuntimeOwner) -> bool {
    crate::proc::process_is_live(owner.pid, owner.process_start.as_deref())
}

pub fn agent_liveness(agent: &AgentState) -> AgentLiveness {
    if let Some(owner) = &agent.runtime_owner {
        if owner.kind == RuntimeOwnerKind::Daemon {
            return if owner_is_live(owner) {
                AgentLiveness::Unknown
            } else {
                AgentLiveness::Dead
            };
        }
        return if owner_is_live(owner) {
            AgentLiveness::Live { pid: owner.pid }
        } else {
            AgentLiveness::Dead
        };
    }
    AgentLiveness::Unknown
}

/// How long stops and boundary inspections wait for departing agent processes:
/// an exec wrapper holds its provider through two signal graces after the
/// pane's hangup, and the margin covers a loaded host.
pub const OWNER_EXIT_BOUND: Duration = Duration::from_secs(3);
const OWNER_EXIT_POLL: Duration = Duration::from_millis(25);

/// Wait until no supplied agent has a live owner, or `deadline` passes.
///
/// Reports whether it slept, so callers can refresh records written during the
/// wait. Never signals owners; their wrappers own termination.
pub fn await_owner_exits(agents: &[&AgentState], deadline: Instant) -> bool {
    let mut slept = false;
    while agents
        .iter()
        .any(|agent| matches!(agent_liveness(agent), AgentLiveness::Live { .. }))
        && Instant::now() < deadline
    {
        std::thread::sleep(OWNER_EXIT_POLL);
        slept = true;
    }
    slept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentStatus;
    use crate::store::snapshot::AgentRollup;

    use jiff::Timestamp;

    impl From<Vec<AgentState>> for AuditRows {
        fn from(agents: Vec<AgentState>) -> Self {
            Self::new(agents.into())
        }
    }

    fn agent(owner: Option<RuntimeOwner>) -> AgentState {
        AgentState {
            status: AgentStatus::Idle,
            runtime_owner: owner,
            ..crate::testkit::agent_state("claude", "sess-1", Timestamp::UNIX_EPOCH)
        }
    }

    /// One agent swapped twice: its first session, a reservation that failed
    /// before binding, and the bound replacement that holds it now.
    fn swap_chain() -> Vec<AgentState> {
        let mut first = agent(None);
        first.launch_id = Some("launch_old".into());
        let mut failed = agent(None);
        failed.agent_id = "launch_middle".into();
        failed.launch_id = first.launch_id.clone();
        failed.epoch = 1;
        failed.status = AgentStatus::Failed;
        let mut holder = agent(None);
        holder.agent_id = "launch_new".into();
        holder.launch_id = first.launch_id.clone();
        holder.epoch = 2;
        holder.launch_bound = true;
        holder.status = AgentStatus::Running;
        vec![first, failed, holder]
    }

    #[test]
    fn runtime_scope_keeps_only_the_holder_across_a_failed_swap() {
        let agents = swap_chain();
        let projection = RuntimeProjection::runtime_from_refs(&agents, Vec::new());
        let kept: Vec<_> = projection
            .agents
            .iter()
            .map(|row| row.agent_id.as_str())
            .collect();
        assert_eq!(kept, ["launch_new"]);
        let from_parts =
            RuntimeProjection::from_parts(&agents.into(), RuntimeScope::Runtime, Vec::new());
        assert_eq!(projection, from_parts);
    }

    #[cfg(unix)]
    #[test]
    fn a_bound_replacement_that_failed_stays_under_the_handle_after_its_wrapper_dies() {
        let dead = || {
            Some(RuntimeOwner::new(
                RuntimeOwnerKind::Agent,
                "w",
                u32::MAX,
                None,
            ))
        };
        let mut agents = swap_chain();
        agents[2].status = AgentStatus::Failed;
        agents[2].runtime_owner = dead();
        let projection = RuntimeProjection::runtime_from_refs(&agents, Vec::new());
        let kept: Vec<_> = projection
            .agents
            .iter()
            .map(|row| row.agent_id.as_str())
            .collect();
        assert_eq!(
            kept,
            ["launch_new"],
            "the failed holder keeps the agent's card"
        );

        let mut lone = agents.pop().unwrap();
        lone.epoch = 0;
        let projection = RuntimeProjection::runtime_from_refs([&lone], Vec::new());
        assert!(
            projection.agents.is_empty(),
            "an ordinary failed launch still drops out"
        );
    }

    #[test]
    fn audit_scope_marks_every_session_the_holder_replaced() {
        let projection =
            RuntimeProjection::from_parts(&swap_chain().into(), RuntimeScope::Audit, Vec::new());
        let marks: Vec<_> = projection.agents.iter().map(|row| row.replaced).collect();
        assert_eq!(marks, [true, true, false]);
    }

    #[test]
    fn team_signal_handles_match_owned_audit_across_a_swap() {
        use crate::agents::{AskKind, LifecycleEvent, LifecycleSignal};
        use crate::harness::schedule::signal::team_lifecycle_signals;
        use crate::ids::{EventId, WorkspaceId};

        let mut rows = swap_chain();
        for row in &mut rows {
            row.team = Some("forge".to_owned());
            row.channel = Some("auth".to_owned());
            row.role = Some("coder".to_owned());
        }
        rows[2].status = AgentStatus::Waiting;
        let signal = LifecycleSignal::AwaitingInput {
            extra_questions: 0,
            kind: AskKind::Question,
            ask_id: None,
            detail: None,
            native_key: None,
        };
        let mut prior = rows[2].lifecycle();
        prior.status = AgentStatus::Running;
        let transition = crate::agents::step(
            Some(&prior),
            None,
            crate::agents::lifecycle::PriorTurnIds::default(),
            &signal,
        );
        let event = LifecycleEvent::new(
            EventId::new(),
            Timestamp::UNIX_EPOCH,
            WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
            rows[2].kind.clone(),
            rows[2].agent_id.clone(),
            None,
            None,
            signal,
            Some(AgentStatus::Running),
            transition,
        );
        let rollup = AgentRollup::from(rows);
        let audit = RuntimeProjection::from_parts(&rollup, RuntimeScope::Audit, Vec::new());
        let rows = AuditRows::new(rollup);
        let member = rows.find(&event.kind, &event.agent_id).unwrap();
        let cohorts = crate::address::team_cohorts(rows.iter());
        assert_eq!(cohorts.len(), 1);
        assert_eq!(cohorts[0].members.len(), 1, "only the holder is the member");
        let actual = team_lifecycle_signals(
            &event,
            member,
            &cohorts[0].members,
            &[],
            &[],
            &BTreeSet::new(),
            &rows,
        );
        let expected = crate::address::team_cohorts(&audit.agents);
        let members = &expected[0].members;
        let member = members
            .iter()
            .find(|row| row.agent_id == event.agent_id)
            .unwrap();
        assert_eq!(actual.len(), 1);
        assert_eq!(actual[0].name.as_str(), "team.waiting");
        assert_eq!(
            actual[0].payload["member"],
            crate::address::agent_handle(member, members, true)
        );
        let handles: Vec<_> = actual[0].payload["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|member| member["handle"].as_str().unwrap())
            .collect();
        let expected: Vec<_> = members
            .iter()
            .map(|member| crate::address::agent_handle(member, members, true))
            .collect();
        assert_eq!(handles, expected);
    }

    #[test]
    fn a_swap_that_fails_before_binding_leaves_the_first_session_the_agent() {
        let mut agents = swap_chain();
        agents.pop();
        for scope in [RuntimeScope::Runtime, RuntimeScope::Audit] {
            let projection =
                RuntimeProjection::from_parts(&agents.clone().into(), scope, Vec::new());
            let first = &projection.agents[0];
            assert_eq!((first.agent_id.as_str(), first.replaced), ("sess-1", false));
        }
    }

    #[test]
    fn projections_overwrite_a_cached_replaced_mark() {
        let mut agent = agent(None);
        agent.replaced = true;
        for scope in [RuntimeScope::Runtime, RuntimeScope::Audit] {
            let projection =
                RuntimeProjection::from_parts(&vec![agent.clone()].into(), scope, Vec::new());
            assert!(!projection.agents[0].replaced);
        }
    }

    #[test]
    fn runtime_projection_keeps_unknown_agents_and_drops_known_dead_ones() {
        let agents = vec![agent(None)];
        #[cfg(unix)]
        let agents = {
            let mut agents = agents;
            let mut dead = agent(Some(RuntimeOwner::new(
                RuntimeOwnerKind::Agent,
                "sess-dead",
                u32::MAX,
                None,
            )));
            dead.agent_id = AgentSessionId::from("sess-dead");
            agents.push(dead);
            agents
        };

        let projection =
            RuntimeProjection::from_parts(&agents.into(), RuntimeScope::Runtime, Vec::new());

        assert_eq!(
            projection.agents.len(),
            1,
            "unknown pid abstains while known-dead owners suppress stale overlays"
        );
        #[cfg(unix)]
        assert_eq!(
            projection.expelled,
            [(
                AgentKind::new_unchecked("claude"),
                AgentSessionId::from("sess-dead"),
            )]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn runtime_projection_keeps_ended_launched_child_only_with_visible_parent() {
        let parent = agent(None);
        let mut child = agent(Some(RuntimeOwner::new(
            RuntimeOwnerKind::Agent,
            "sess-child",
            u32::MAX,
            None,
        )));
        child.kind = AgentKind::new_unchecked("codex");
        child.agent_id = AgentSessionId::from("sess-child");
        child.parent_agent_kind = Some(parent.kind.clone());
        child.parent_agent_id = Some(parent.agent_id.clone());
        child.launch_depth = Some(1);
        child.ended_at = Some(Timestamp::UNIX_EPOCH);
        child.status = AgentStatus::Success;

        let projection = RuntimeProjection::from_parts(
            &AgentRollup::from(vec![parent.clone(), child.clone()]),
            RuntimeScope::Runtime,
            Vec::new(),
        );
        assert_eq!(projection.agents, vec![parent.clone(), child.clone()]);

        let without_parent = RuntimeProjection::from_parts(
            &AgentRollup::from(vec![child.clone()]),
            RuntimeScope::Runtime,
            Vec::new(),
        );
        assert!(without_parent.agents.is_empty());

        let mut ended_parent = parent;
        ended_parent.ended_at = Some(Timestamp::UNIX_EPOCH);
        let ended_parent_projection = RuntimeProjection::from_parts(
            &AgentRollup::from(vec![ended_parent, child]),
            RuntimeScope::Runtime,
            Vec::new(),
        );
        assert!(ended_parent_projection.agents.is_empty());
    }

    #[test]
    fn runtime_projection_keeps_child_through_parent_launch() {
        for parent_kind in [None, Some(AgentKind::new_unchecked("claude"))] {
            let mut old = agent(None);
            old.launch_id = Some("L".into());
            old.ended_at = Some(Timestamp::UNIX_EPOCH);
            let mut successor = agent(None);
            successor.agent_id = "NEW".into();
            successor.launch_id = old.launch_id.clone();
            let mut child = agent(None);
            child.agent_id = "child".into();
            child.parent_agent_id = Some("L".into());
            child.parent_agent_kind = parent_kind;
            child.launch_depth = Some(1);
            child.ended_at = Some(Timestamp::UNIX_EPOCH);

            let projection = RuntimeProjection::from_parts(
                &AgentRollup::from(vec![old.clone(), successor.clone(), child.clone()]),
                RuntimeScope::Runtime,
                Vec::new(),
            );
            assert_eq!(projection.agents, vec![successor, child.clone()]);
            let without_successor = RuntimeProjection::from_parts(
                &AgentRollup::from(vec![old, child]),
                RuntimeScope::Runtime,
                Vec::new(),
            );
            assert!(without_successor.agents.is_empty());
        }
    }

    #[test]
    fn runtime_projection_parent_session_aliases_are_kind_scoped() {
        let mut parent = agent(None);
        parent.launch_id = Some("launch".into());
        parent.prior_launch_ids = vec!["prior".into()];
        for id in ["sess-1", "launch", "prior"] {
            for kind in ["claude", "codex"] {
                let mut child = agent(None);
                child.agent_id = "child".into();
                child.parent_agent_id = Some(id.into());
                child.parent_agent_kind = Some(AgentKind::new_unchecked(kind));
                child.launch_depth = Some(1);
                child.ended_at = Some(Timestamp::UNIX_EPOCH);
                let projection =
                    RuntimeProjection::runtime_from_refs([&child, &parent], Vec::new());
                let expected = if kind == "claude" || id == "launch" {
                    vec![child.clone(), parent.clone()]
                } else {
                    vec![parent.clone()]
                };
                assert_eq!(projection.agents, expected, "{id}, {kind}");
                assert_eq!(projection.ended, [(child.kind, child.agent_id)].into());
                assert!(projection.expelled.is_empty());
            }
        }
    }

    #[test]
    fn runtime_projection_dead_parent_does_not_hide_a_visible_child() {
        let mut parent = agent(Some(RuntimeOwner::new(
            RuntimeOwnerKind::Agent,
            "parent",
            u32::MAX,
            None,
        )));
        parent.agent_id = "parent".into();
        let mut child = agent(None);
        child.parent_agent_id = Some(parent.agent_id.clone());
        child.launch_depth = Some(1);
        for visible in [true, false] {
            child.runtime_owner = if visible {
                None
            } else {
                parent.runtime_owner.clone()
            };
            let projection = RuntimeProjection::runtime_from_refs([&parent, &child], Vec::new());
            assert_eq!(
                projection.agents,
                if visible { vec![child.clone()] } else { vec![] }
            );
            let mut expelled = BTreeSet::from([(parent.kind.clone(), parent.agent_id.clone())]);
            if !visible {
                expelled.insert((child.kind.clone(), child.agent_id.clone()));
            }
            assert_eq!(projection.expelled, expelled);
        }
    }

    #[test]
    fn runtime_projection_hides_ended_rows_while_audit_retains_them() {
        let active = agent(None);
        let mut ended = agent(None);
        ended.agent_id = AgentSessionId::from("sess-ended");
        ended.ended_at = Some(Timestamp::UNIX_EPOCH);
        let key = (ended.kind.clone(), ended.agent_id.clone());

        let runtime = RuntimeProjection::from_parts(
            &AgentRollup::from(vec![active.clone(), ended.clone()]),
            RuntimeScope::Runtime,
            Vec::new(),
        );
        assert_eq!(runtime.agents, vec![active]);
        assert_eq!(runtime.ended, [key.clone()].into_iter().collect());
        assert!(runtime.expelled.is_empty());

        let audit = RuntimeProjection::from_parts(
            &AgentRollup::from(vec![ended.clone()]),
            RuntimeScope::Audit,
            Vec::new(),
        );
        assert_eq!(audit.agents, vec![ended]);
        assert_eq!(audit.ended, [key].into_iter().collect());
        assert!(audit.expelled.is_empty());
    }

    #[test]
    fn agent_liveness_reports_live_runtime_owner() {
        let owner = current_process_owner(RuntimeOwnerKind::Agent, "sess-live");
        assert_eq!(
            agent_liveness(&agent(Some(owner))),
            AgentLiveness::Live {
                pid: std::process::id()
            }
        );
    }

    #[test]
    fn agent_liveness_daemon_owner_abstains_while_process_lives() {
        let owner = current_process_owner(RuntimeOwnerKind::Daemon, "sess-daemon");
        assert_eq!(agent_liveness(&agent(Some(owner))), AgentLiveness::Unknown);
    }

    #[test]
    fn agent_liveness_reports_unknown_without_process_identity() {
        assert_eq!(agent_liveness(&agent(None)), AgentLiveness::Unknown);
    }

    #[cfg(unix)]
    #[test]
    fn agent_liveness_reports_dead_for_missing_or_wrong_process() {
        let missing = RuntimeOwner::new(RuntimeOwnerKind::Agent, "sess-missing", u32::MAX, None);
        assert_eq!(agent_liveness(&agent(Some(missing))), AgentLiveness::Dead);

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let wrong_start = RuntimeOwner::new(
                RuntimeOwnerKind::Agent,
                "sess-wrong-start",
                std::process::id(),
                Some("definitely-not-this-process".to_owned()),
            );
            assert_eq!(
                agent_liveness(&agent(Some(wrong_start))),
                AgentLiveness::Dead
            );
        }

        let daemon = RuntimeOwner::new(RuntimeOwnerKind::Daemon, "sess-daemon", u32::MAX, None);
        assert_eq!(agent_liveness(&agent(Some(daemon))), AgentLiveness::Dead);
    }
}
