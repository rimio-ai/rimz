//! Two-phase previous-incarnation inspection through the shared recovery plan.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use jiff::Timestamp;

use crate::Store;
use crate::agents::AgentState;
use crate::config::{MachineConfig, ProfilesConfig, TeamsConfig};
use crate::diag::DiagSink;
use crate::diag::record::DiagEvent;
use crate::disk::paths::{RuntimePaths, StatePaths, cache_home};
use crate::harness::resume::{
    MaterializedRecovery, RecoveryMaterializer, RecoveryPlan, RecoveryTabAgents, ResumePlan,
    ResumeSkipReason, plan_resume_detailed, resume_session_present, split_team_and_flat,
};
use crate::ids::{AgentKind, AgentSessionId, WorkspaceId};
use crate::mux::{MuxBackend, ResumeTab};
use crate::store::event::{LastDeathMarker, SessionDeathAgent, SessionDeathCause};
use crate::store::runtime::{AgentLiveness, agent_liveness};
use crate::store::snapshot::find_agent;
use crate::store::{live_roster, pending_recovery};

/// How long a boundary inspection waits for the dead room's agent processes:
/// an exec wrapper holds its provider through two signal graces after the
/// pane's hangup, and the margin covers a loaded host.
const OWNER_EXIT_BOUND: Duration = Duration::from_secs(3);
const OWNER_EXIT_POLL: Duration = Duration::from_millis(25);

#[derive(Debug, thiserror::Error)]
pub enum RebirthErr {
    #[error(transparent)]
    Inspect(#[from] anyhow::Error),
}

/// Failure to remove durably ended replacement seats from pending recovery.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct RefillSettlementErr(pending_recovery::PendingRecoveryErr);

/// What a settlement does with root recovery candidates. Only an attended
/// decision declines or drops them; children and live-refilled seats end
/// automatically under every disposition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RebirthDisposition {
    /// Nobody was asked: every candidate stays pending.
    Defer,
    /// The user declined recovery: every candidate is ended.
    Decline,
    /// Resume what the plan can; the rest stay pending.
    RecoverKeep,
    /// Resume what the plan can; the user dropped the rest.
    RecoverDrop,
}

impl RebirthDisposition {
    pub const fn recovers(self) -> bool {
        matches!(self, Self::RecoverKeep | Self::RecoverDrop)
    }
}

/// A root recovery candidate the plan does not resume and a recovering
/// settlement ends only when the user drops the rest. Children are settled
/// automatically, never listed here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnresumableAgent {
    pub label: String,
    /// The planner's reason, where it gave one.
    pub reason: Option<ResumeSkipReason>,
}

#[derive(Clone, Debug)]
pub struct RebirthPreview {
    death: Option<LastDeathMarker>,
    pane_count: usize,
    labels: Vec<String>,
    requires_sandbox: bool,
    candidate_count: usize,
    refilled_count: usize,
    unresumable: Vec<UnresumableAgent>,
    recovery_off: bool,
}

impl RebirthPreview {
    pub fn death(&self) -> Option<&LastDeathMarker> {
        self.death.as_ref()
    }

    pub const fn pane_count(&self) -> usize {
        self.pane_count
    }

    pub fn labels(&self) -> &[String] {
        &self.labels
    }

    /// Whether recovery resumes an agent whose effective isolation is sandbox,
    /// so the caller must preflight bubblewrap before choosing to recover.
    pub const fn requires_sandbox(&self) -> bool {
        self.requires_sandbox
    }

    /// Lost roots awaiting a decision: neither ended, live, nor already replaced.
    pub const fn candidate_count(&self) -> usize {
        self.candidate_count
    }

    /// Parked seats already held by live replacements, not planned Fresh seeds.
    pub const fn refilled_count(&self) -> usize {
        self.refilled_count
    }

    pub fn unresumable(&self) -> &[UnresumableAgent] {
        &self.unresumable
    }

    /// Whether `--no-resume` or `[resume] on_rebirth = false` switched
    /// recovery off, so the plan resumes nobody.
    pub const fn recovery_off(&self) -> bool {
        self.recovery_off
    }
}

#[derive(Clone, Debug)]
pub struct RebirthPlan {
    paths: StatePaths,
    runtime: RuntimePaths,
    /// Whether this birth ends an incarnation; a live reattach settles only.
    boundary: bool,
    boot_token: Option<String>,
    death: Option<LastDeathMarker>,
    crash_roster: Vec<AgentState>,
    crash_cache: CrashCacheSnapshot,
    candidates: Vec<AgentState>,
    children: Vec<AgentState>,
    /// Lost agents already ended by other means: they only leave the record.
    ended: BTreeSet<(AgentKind, AgentSessionId)>,
    refilled: BTreeSet<(AgentKind, AgentSessionId)>,
    planned: RecoveryPlan,
    recovery_off: bool,
    requires_sandbox: bool,
}

#[derive(Clone, Debug, Default)]
struct CrashCacheSnapshot {
    entries: Vec<CrashCacheEntry>,
    error: Option<String>,
}

#[derive(Clone, Debug)]
enum CrashCacheEntry {
    Directory(PathBuf),
    File { path: PathBuf, bytes: Vec<u8> },
}

#[derive(Default)]
struct SettlementPlan {
    candidates: Vec<AgentState>,
    children: Vec<AgentState>,
    ended: BTreeSet<(AgentKind, AgentSessionId)>,
    planned: RecoveryPlan,
}

impl RebirthPlan {
    pub(crate) fn is_empty(&self) -> bool {
        self.candidates.is_empty()
            && self.children.is_empty()
            && self.ended.is_empty()
            && self.refilled.is_empty()
    }
    /// Inspect prior state without changing markers, archives, event logs, the
    /// persisted live roster, or the pending-recovery record.
    pub(crate) fn inspect(
        backend: &dyn MuxBackend,
        workspace_id: &WorkspaceId,
        session_name: &str,
        project_root: &Path,
        machine: &MachineConfig,
        disabled: bool,
    ) -> std::result::Result<Self, RebirthErr> {
        let paths = StatePaths::for_workspace(workspace_id.clone()).map_err(anyhow::Error::from)?;
        let runtime =
            RuntimePaths::for_workspace(workspace_id.clone()).map_err(anyhow::Error::from)?;
        let boot = boot_token();
        let cache_sources = backend.resurrection_cache_paths(session_name);
        inspect_at(
            paths,
            runtime,
            boot,
            cache_sources,
            project_root,
            machine,
            disabled,
            OWNER_EXIT_BOUND,
        )
        .map_err(RebirthErr::Inspect)
    }

    /// Plan the parked agents of a session that is already live, as read-only
    /// as [`Self::inspect`]. Materializing the result settles without a
    /// boundary: no session event, roster, or boot-marker write.
    pub(crate) fn inspect_live(
        workspace_id: &WorkspaceId,
        project_root: &Path,
        machine: &MachineConfig,
        disabled: bool,
    ) -> std::result::Result<Self, RebirthErr> {
        let paths = StatePaths::for_workspace(workspace_id.clone()).map_err(anyhow::Error::from)?;
        let runtime =
            RuntimePaths::for_workspace(workspace_id.clone()).map_err(anyhow::Error::from)?;
        Ok(inspect_live_at(
            paths,
            runtime,
            project_root,
            machine,
            disabled,
        ))
    }

    pub fn preview(&self) -> RebirthPreview {
        let pane_count = self.planned.pane_count();
        let labels = self.planned.labels();
        let resumable = self.planned.resumed_keys();
        let unresumable = self
            .candidates
            .iter()
            .filter_map(|agent| {
                let key = (agent.kind.clone(), agent.agent_id.clone());
                if resumable.contains(&key) {
                    return None;
                }
                Some(match self.planned.skip_for(&key) {
                    Some(skip) => UnresumableAgent {
                        label: skip.label.clone(),
                        reason: Some(skip.reason.clone()),
                    },
                    None => UnresumableAgent {
                        label: agent
                            .name
                            .clone()
                            .unwrap_or_else(|| agent.agent_id.to_string()),
                        reason: self
                            .planned
                            .worktree_gone()
                            .contains(&key)
                            .then_some(ResumeSkipReason::WorktreeGone),
                    },
                })
            })
            .collect();
        RebirthPreview {
            death: self.death.clone(),
            pane_count,
            labels,
            requires_sandbox: self.requires_sandbox,
            candidate_count: self.candidates.len(),
            refilled_count: self.refilled.len(),
            unresumable,
            recovery_off: self.recovery_off,
        }
    }

    /// Distinct checkouts that recovery will launch into, before materialization.
    pub fn checkout_roots(&self) -> std::collections::BTreeSet<&Path> {
        self.planned.checkout_roots()
    }

    /// Commit the boundary, when this birth ends an incarnation, and settle
    /// the candidates as `disposition` says, after the multiplexer session exists.
    /// The caller must park the roster before creating that session. The
    /// resumed agents stay in the pending record until
    /// [`SeededRecovery::confirm`] learns whether their tab is open.
    pub(crate) fn settle(
        self,
        disposition: RebirthDisposition,
        session_name: &str,
    ) -> SeededRecovery {
        if let Some(boot) = self.boot_token.as_deref() {
            write_boot_marker(&self.paths.boot_marker, boot);
        }

        let store = match Store::open(self.paths.clone(), self.runtime.clone()) {
            Ok(store) => Some(store),
            Err(err) => {
                tracing::warn!(workspace = %self.paths.workspace_id, error = %err, "rebirth store unavailable");
                None
            }
        };
        if let Some(death) = self.death.as_ref() {
            if let Some(store) = store.as_ref() {
                append_session_death(store, &self.paths.workspace_id, session_name, death);
            }
            write_last_death_marker(&self.paths, death);
        }
        if self
            .death
            .as_ref()
            .is_some_and(|death| death.cause == SessionDeathCause::Crash)
            && let Err(err) = archive_crash(
                &self.paths,
                &self.crash_cache,
                &self.crash_roster,
                self.death
                    .as_ref()
                    .map_or(Timestamp::now(), |death| death.at),
            )
        {
            tracing::debug!(workspace = %self.paths.workspace_id, error = %err, "crash archive skipped");
        }

        // Before recovery respawns panes, so no recovered peer's fresh turn is failed.
        if let Some(store) = store.as_ref() {
            for agent in &self.crash_roster {
                if let Err(err) =
                    crate::harness::run::fail_peer_run(store, agent, "peer room session ended")
                {
                    tracing::warn!(workspace = %self.paths.workspace_id, agent_id = %agent.agent_id, error = %err, "rebirth: could not fail peer turn");
                }
            }
            if self.boundary {
                let lost = self
                    .crash_roster
                    .iter()
                    .map(|agent| (agent.kind.clone(), agent.agent_id.clone()))
                    .collect();
                cancel_child_runs(store, &self.paths, &self.crash_roster, &lost);
            }
        }
        let (resume, tab_agents) = match disposition {
            RebirthDisposition::RecoverKeep | RebirthDisposition::RecoverDrop => {
                materialize_recovery(store.as_ref(), &self.paths, session_name, self.planned)
            }
            RebirthDisposition::Defer | RebirthDisposition::Decline => {
                (ResumePlan::default(), Vec::new())
            }
        };
        let seeded = tab_agents
            .iter()
            .flat_map(|agents| agents.resumed.iter().chain(&agents.refilled))
            .cloned()
            .collect::<BTreeSet<_>>();
        let worktree_gone = resume
            .agents_to_end
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let dropped_as = match disposition {
            RebirthDisposition::Decline => Some("rimz.recovery-declined"),
            RebirthDisposition::RecoverDrop => Some("rimz.not-resumed"),
            RebirthDisposition::Defer | RebirthDisposition::RecoverKeep => None,
        };
        // A key leaves pending only after its resumed tab is confirmed or its
        // end stamp is durable.
        let mut settled = self.ended;
        let mut declined = 0;
        if let Some(store) = store.as_ref() {
            let children = self
                .children
                .iter()
                .map(|agent| (agent.kind.clone(), agent.agent_id.clone()))
                .filter(|key| !seeded.contains(key))
                .collect();
            cancel_child_runs(store, &self.paths, &self.children, &children);
            let ended = record_agents_ended(
                store,
                &self.paths.workspace_id,
                session_name,
                &children,
                "rimz.child-not-resumed",
            );
            let sink = DiagSink::for_workspace(self.paths.workspace_id.clone(), session_name, None);
            for (kind, agent_id) in &ended {
                sink.emit(DiagEvent::RecoveryChildEnded {
                    agent_kind: kind.clone(),
                    agent_id: agent_id.clone(),
                });
            }
            settled.extend(ended);
            settled.extend(record_refilled_seats(
                store,
                &self.paths.workspace_id,
                session_name,
                &self.refilled,
            ));
        }
        if let Some(store) = store.as_ref()
            && let Some(event_name) = dropped_as
        {
            settled.extend(record_agents_ended(
                store,
                &self.paths.workspace_id,
                session_name,
                &worktree_gone,
                "rimz.worktree-gone",
            ));
            let rest = self
                .candidates
                .iter()
                .map(|agent| (agent.kind.clone(), agent.agent_id.clone()))
                .filter(|key| !seeded.contains(key) && !worktree_gone.contains(key))
                .collect();
            let dropped = record_agents_ended(
                store,
                &self.paths.workspace_id,
                session_name,
                &rest,
                event_name,
            );
            if disposition == RebirthDisposition::Decline {
                declined = dropped.len();
            }
            settled.extend(dropped);
        }
        if let Err(err) = pending_recovery::settle(&self.paths, &settled) {
            tracing::warn!(workspace = %self.paths.workspace_id, error = %err, "rebirth: settled agents stay in the pending-recovery record");
        }
        if self.boundary {
            close_boundary(store.as_ref(), &self.paths, session_name);
        }
        SeededRecovery {
            paths: self.paths,
            runtime: self.runtime,
            death: self.death,
            session_name: session_name.to_owned(),
            resume,
            tab_agents,
            declined,
        }
    }
}

/// A settlement whose resume tabs are planned but not yet known to be open.
/// Its resumed agents wait in the pending record for [`Self::confirm`].
pub(crate) struct SeededRecovery {
    paths: StatePaths,
    runtime: RuntimePaths,
    death: Option<LastDeathMarker>,
    session_name: String,
    resume: ResumePlan,
    /// The agents each of `resume.tabs` resumes or replaces, by position.
    tab_agents: Vec<RecoveryTabAgents>,
    declined: usize,
}

impl SeededRecovery {
    pub(crate) const fn declined_count(&self) -> usize {
        self.declined
    }

    /// The tabs whose agents this settlement resumes.
    pub(crate) fn tabs(&self) -> &[ResumeTab] {
        &self.resume.tabs
    }

    /// Finish the settlement tab by tab. `outcome` says whether the tab at a
    /// position is open: a confirmed tab's agents leave the pending record
    /// and count as recovered, while an unconfirmed tab fails its launch
    /// batch, leaves the returned plan with a warning, and keeps its agents
    /// pending for a later rebirth or explicit resume.
    pub(crate) fn confirm<E: std::fmt::Display>(
        self,
        mut outcome: impl FnMut(usize, &ResumeTab) -> std::result::Result<(), E>,
    ) -> ResumePlan {
        let Self {
            paths,
            runtime,
            death,
            session_name,
            mut resume,
            tab_agents,
            declined: _,
        } = self;
        let planned = std::mem::take(&mut resume.tabs);
        let mut confirmed = BTreeSet::new();
        let mut refilled = BTreeSet::new();
        // Where each planned tab sits in the returned plan, if it is there.
        let mut returned = Vec::with_capacity(planned.len());
        for (index, (tab, agents)) in planned.into_iter().zip(tab_agents).enumerate() {
            match outcome(index, &tab) {
                Ok(()) => {
                    returned.push(Some(resume.tabs.len()));
                    confirmed.extend(agents.resumed);
                    refilled.extend(agents.refilled);
                    resume.tabs.push(tab);
                }
                Err(error) => {
                    returned.push(None);
                    resume.warnings.push(format!(
                        "could not open resumed tab {}: {error}; its agents stay pending for a later rebirth or explicit resume",
                        tab.label
                    ));
                }
            }
        }
        let (kept, failed): (Vec<_>, Vec<_>) = std::mem::take(&mut resume.team_launches)
            .into_iter()
            .partition(|launch| returned.get(launch.tab).is_some_and(Option::is_some));
        resume.team_launches = kept
            .into_iter()
            .filter_map(|mut launch| {
                launch.tab = (*returned.get(launch.tab)?)?;
                Some(launch)
            })
            .collect();
        if !failed.is_empty() || !refilled.is_empty() {
            match Store::open(paths.clone(), runtime) {
                Ok(store) => {
                    if let Err(err) = settle_refilled_seats(&store, &session_name, &refilled) {
                        resume
                            .warnings
                            .push(format!("replaced agents stay pending: {err}"));
                    }
                    for launch in &failed {
                        let _ = store.fail_agent_launch_batch(&launch.batch);
                    }
                }
                Err(err) => resume.warnings.push(format!(
                    "could not settle replacement seats or failed launch batches: {err}; replaced agents stay pending"
                )),
            }
        }
        if let Err(err) = pending_recovery::settle(&paths, &confirmed) {
            resume
                .warnings
                .push(format!("resumed agents stay pending: {err}"));
        }
        record_recovery(&paths, death, &session_name, &resume.tabs);
        resume
    }
}

fn record_recovery(
    paths: &StatePaths,
    death: Option<LastDeathMarker>,
    session_name: &str,
    tabs: &[ResumeTab],
) {
    let recovered = tabs.iter().map(ResumeTab::pane_count).sum();
    if recovered > 0 {
        crate::harness::assist_log::append(&crate::harness::assist_log::AssistRecord {
            at: Timestamp::now(),
            assist: crate::harness::assist_log::Assist::AutoResume {
                workspace_id: paths.workspace_id.clone(),
                session_name: session_name.to_owned(),
                cause: death.as_ref().map(|death| death.cause),
                recovered,
                labels: tabs.iter().map(|tab| tab.label.clone()).collect(),
            },
        });
    }
    match death {
        Some(mut death) => {
            death.recovered = Some(recovered);
            write_last_death_marker(paths, &death);
        }
        None if recovered > 0 => count_later_recovery(paths, recovered),
        None => {}
    }
}

fn cancel_child_runs(
    store: &Store,
    paths: &StatePaths,
    candidates: &[AgentState],
    affected: &BTreeSet<(AgentKind, AgentSessionId)>,
) {
    let runs = match crate::harness::run::list(paths) {
        Ok(runs) => runs,
        Err(err) => {
            tracing::warn!(workspace = %paths.workspace_id, error = %err, "rebirth: could not list unrecovered child runs");
            return;
        }
    };
    for agent in candidates {
        if agent.parent_agent_id.is_none()
            || !affected.contains(&(agent.kind.clone(), agent.agent_id.clone()))
        {
            continue;
        }
        let Some(run) =
            crate::harness::fleet::newest_run(agent, &runs).filter(|run| !run.status.is_terminal())
        else {
            continue;
        };
        if let Err(err) = crate::harness::run::cancel_and_wake(store, &run.run_id) {
            tracing::warn!(workspace = %paths.workspace_id, run_id = %run.run_id, error = %err, "rebirth: could not cancel unrecovered child run");
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the boundary inspection's inputs, with the owner-exit bound a test shortens"
)]
fn inspect_at(
    paths: StatePaths,
    runtime: RuntimePaths,
    current_boot: Option<String>,
    cache_sources: Vec<PathBuf>,
    project_root: &Path,
    machine: &MachineConfig,
    disabled: bool,
    owner_exit_bound: Duration,
) -> Result<RebirthPlan> {
    let owners_exited_by = Instant::now() + owner_exit_bound;
    let previous_boot = read_boot_marker(&paths.boot_marker);
    let reboot = boot_changed(previous_boot.as_deref(), current_boot.as_deref());
    let audit = Store::open_existing(paths.clone(), runtime.clone()).and_then(|store| {
        store
            .runtime_projection(crate::RuntimeScope::Audit)
            .ok()
            .map(|projection| (store, projection))
    });
    let roster = audit
        .as_ref()
        .map(|(_, projection)| recovery_roster(&paths, &projection.agents))
        .unwrap_or_default();
    if audit.is_none() {
        tracing::debug!(workspace = %paths.workspace_id, "rebirth: no readable store, nothing to recover");
    }
    let recover_agents = reboot || !roster.is_empty();
    let death = audit
        .as_ref()
        .filter(|_| recover_agents)
        .map(|(_, projection)| {
            let cause = if reboot {
                SessionDeathCause::Reboot
            } else {
                SessionDeathCause::Crash
            };
            LastDeathMarker {
                cause,
                lost_agents: lost_agent_summaries(&projection.agents, &roster),
                at: Timestamp::now(),
                recovered: None,
            }
        });
    let crash_roster = audit
        .as_ref()
        .map(|(_, projection)| lost_agent_roster(&projection.agents, &roster))
        .unwrap_or_default();
    let crash_cache = if death
        .as_ref()
        .is_some_and(|death| death.cause == SessionDeathCause::Crash)
    {
        capture_cache_sources(&cache_home(), &cache_sources)
    } else {
        CrashCacheSnapshot::default()
    };

    let mut scope = pending_recovery::read(&paths.pending_recovery);
    tracing::debug!(workspace = %paths.workspace_id, roster = roster.len(), pending = scope.len(), reboot, "rebirth: recovery scope");
    scope.extend(roster.iter().cloned());
    let recovery_off = disabled || !machine.resume.on_rebirth;
    // A provider that dies inside the wait may record its own end meanwhile,
    // so a wait that slept plans from a fresh read of the log.
    let waited = audit.as_ref().is_some_and(|(_, projection)| {
        await_owner_exits(&projection.agents, &scope, owners_exited_by)
    });
    let refreshed = audit
        .as_ref()
        .filter(|_| waited)
        .and_then(|(store, _)| store.runtime_projection(crate::RuntimeScope::Audit).ok());
    let SettlementPlan {
        candidates,
        children,
        ended,
        planned,
    } = plan_settlement(
        refreshed
            .as_ref()
            .or(audit.as_ref().map(|(_, projection)| projection)),
        &paths,
        &runtime,
        &scope,
        project_root,
        machine,
        recovery_off,
    );
    let requires_sandbox = planned.requires_sandbox(machine.agents.isolation);
    Ok(RebirthPlan {
        paths,
        runtime,
        boundary: true,
        boot_token: current_boot,
        death,
        crash_roster,
        crash_cache,
        candidates,
        children,
        ended,
        refilled: planned.refilled().clone(),
        planned,
        recovery_off,
        requires_sandbox,
    })
}

/// Blocks until no agent a settlement could offer (in `scope`, not ended) has a
/// live owner, or `deadline` passes, and reports whether it slept. The room
/// these owners ran in is gone, so a live one is on its wrapper's exit ladder;
/// the wrapper owns the kill.
fn await_owner_exits(
    agents: &[AgentState],
    scope: &BTreeSet<(AgentKind, AgentSessionId)>,
    deadline: Instant,
) -> bool {
    let exiting = |agent: &AgentState| {
        agent.ended_at.is_none()
            && scope.contains(&(agent.kind.clone(), agent.agent_id.clone()))
            && matches!(agent_liveness(agent), AgentLiveness::Live { .. })
    };
    let mut slept = false;
    while agents.iter().any(exiting) && Instant::now() < deadline {
        std::thread::sleep(OWNER_EXIT_POLL);
        slept = true;
    }
    slept
}

fn inspect_live_at(
    paths: StatePaths,
    runtime: RuntimePaths,
    project_root: &Path,
    machine: &MachineConfig,
    disabled: bool,
) -> RebirthPlan {
    // The roster is this session's live set; only parked agents are lost.
    let scope = pending_recovery::read(&paths.pending_recovery);
    inspect_live_scope(paths, runtime, project_root, machine, disabled, scope)
}

fn inspect_live_scope(
    paths: StatePaths,
    runtime: RuntimePaths,
    project_root: &Path,
    machine: &MachineConfig,
    disabled: bool,
    scope: BTreeSet<(AgentKind, AgentSessionId)>,
) -> RebirthPlan {
    let projection = (!scope.is_empty())
        .then(|| Store::open_existing(paths.clone(), runtime.clone()))
        .flatten()
        .and_then(|store| store.runtime_projection(crate::RuntimeScope::Audit).ok());
    let recovery_off = disabled || !machine.resume.on_rebirth;
    let SettlementPlan {
        candidates,
        children,
        ended,
        planned,
    } = plan_settlement(
        projection.as_ref(),
        &paths,
        &runtime,
        &scope,
        project_root,
        machine,
        recovery_off,
    );
    let requires_sandbox = planned.requires_sandbox(machine.agents.isolation);
    RebirthPlan {
        paths,
        runtime,
        boundary: false,
        boot_token: None,
        death: None,
        crash_roster: Vec::new(),
        crash_cache: CrashCacheSnapshot::default(),
        candidates,
        children,
        ended,
        refilled: planned.refilled().clone(),
        planned,
        recovery_off,
        requires_sandbox,
    }
}

/// Lost roots awaiting a decision, non-live children to end, the agents in
/// `scope` already ended, and the plan that resumes the roots it can.
fn plan_settlement(
    projection: Option<&crate::RuntimeProjection>,
    paths: &StatePaths,
    runtime: &RuntimePaths,
    scope: &BTreeSet<(AgentKind, AgentSessionId)>,
    project_root: &Path,
    machine: &MachineConfig,
    recovery_off: bool,
) -> SettlementPlan {
    let Some(projection) = projection else {
        return SettlementPlan::default();
    };
    let ended = projection
        .agents
        .iter()
        .filter(|agent| agent.ended_at.is_some())
        .map(|agent| (agent.kind.clone(), agent.agent_id.clone()))
        .filter(|key| scope.contains(key))
        .collect();
    let lost = projection
        .agents
        .iter()
        .filter(|agent| scope.contains(&(agent.kind.clone(), agent.agent_id.clone())))
        .filter(|agent| match agent_liveness(agent) {
            AgentLiveness::Live { pid } => {
                tracing::debug!(kind = %agent.kind, session = %agent.agent_id, pid, ended = agent.ended_at.is_some(), "rebirth: not a candidate: owner live");
                false
            }
            AgentLiveness::Dead | AgentLiveness::Unknown => true,
        })
        .cloned()
        .collect::<Vec<_>>();
    let (mut candidates, children): (Vec<_>, Vec<_>) = lost
        .iter()
        .filter(|agent| {
            if agent.ended_at.is_some() {
                tracing::debug!(kind = %agent.kind, session = %agent.agent_id, "rebirth: not a candidate: ended");
            }
            agent.ended_at.is_none()
        })
        .cloned()
        .partition(|agent| {
            if agent.parent_agent_id.is_some() {
                tracing::debug!(kind = %agent.kind, session = %agent.agent_id, "rebirth: not a candidate: launched child");
                return false;
            }
            true
        });
    for (kind, session) in scope {
        if !projection
            .agents
            .iter()
            .any(|agent| agent.kind == *kind && agent.agent_id == *session)
        {
            tracing::debug!(%kind, %session, "rebirth: not a candidate: no audit row");
        }
    }
    if recovery_off || candidates.is_empty() {
        let planned = RecoveryPlan::default();
        trace_unplanned(&candidates, &planned, recovery_off);
        return SettlementPlan {
            candidates,
            children,
            ended,
            planned,
        };
    }
    let availability =
        crate::harness::plan::LaunchAvailability::read(runtime, paths, machine, Timestamp::now());
    let teams_and_profiles = effective_teams_and_profiles(machine, project_root, &availability);
    let planned = plan_recovery(
        projection,
        paths,
        runtime,
        &lost,
        project_root,
        machine,
        &teams_and_profiles,
    );
    candidates.retain(|agent| {
        !planned
            .refilled()
            .contains(&(agent.kind.clone(), agent.agent_id.clone()))
    });
    trace_unplanned(&candidates, &planned, recovery_off);
    SettlementPlan {
        candidates,
        children,
        ended,
        planned,
    }
}

/// Debug evidence for each candidate the plan neither resumes nor reports as
/// skipped, which a silent start would otherwise drop with nothing printed.
fn trace_unplanned(candidates: &[AgentState], planned: &RecoveryPlan, recovery_off: bool) {
    let resumable = planned.resumed_keys();
    for agent in candidates {
        let key = (agent.kind.clone(), agent.agent_id.clone());
        if resumable.contains(&key) || planned.skip_for(&key).is_some() {
            continue;
        }
        let reason = if recovery_off {
            "recovery off"
        } else if planned.worktree_gone().contains(&key) {
            "worktree gone"
        } else {
            "no resume tab planned"
        };
        tracing::debug!(kind = %agent.kind, session = %agent.agent_id, name = ?agent.name, parent = ?agent.parent_agent_id, worktree = ?agent.worktree_path, reason, "rebirth: candidate not resumed");
    }
}

fn effective_teams_and_profiles(
    machine: &MachineConfig,
    project_root: &Path,
    availability: &crate::harness::plan::LaunchAvailability,
) -> (TeamsConfig, ProfilesConfig) {
    match crate::config::effective::load(machine, project_root) {
        Ok(mut launch) => {
            if let Err(error) = launch.route(
                &machine.tiers,
                crate::config::effective::ProfileScope::Agents,
                None,
                None,
                None,
                None,
                |kind, model| availability.unavailable(kind, model),
            ) {
                tracing::warn!(%error, "cannot route team restore profiles; recovering existing members only");
                return (TeamsConfig::default(), machine.agents.profiles.clone());
            }
            (launch.teams, launch.profiles)
        }
        Err(err) => {
            let (config, detail) = err
                .diagnosis()
                .map(|diagnosis| (diagnosis.path().display().to_string(), diagnosis.summary()))
                .unwrap_or_default();
            tracing::warn!(
                error = %err,
                config = %config,
                detail = %detail,
                "effective agent config unavailable; recovering existing members with machine profiles only"
            );
            (TeamsConfig::default(), machine.agents.profiles.clone())
        }
    }
}

fn plan_recovery(
    projection: &crate::RuntimeProjection,
    paths: &StatePaths,
    runtime: &RuntimePaths,
    agents: &[AgentState],
    project_root: &Path,
    machine: &MachineConfig,
    teams_and_profiles: &(TeamsConfig, ProfilesConfig),
) -> RecoveryPlan {
    let (teams, profiles) = teams_and_profiles;
    let logins = crate::agents::room_accounts(&paths.workspace_record, Some(project_root), machine)
        .unwrap_or_else(crate::agents::RoomAccounts::unavailable);
    let catalog = crate::agents::LoginCatalog::room_view(&machine.accounts).0;
    let (team, flat_agents, refilled) = split_team_and_flat(
        agents,
        &logins,
        &catalog,
        teams,
        profiles,
        &machine.agents.commands,
        Some(project_root),
        &paths.workspace_id,
        Path::is_dir,
        resume_session_present,
        false,
        &projection.agents,
        agent_liveness,
    );
    let team_panes = team
        .iter()
        .map(|planned| planned.cohort.seeds.len())
        .sum::<usize>();
    let flat = plan_resume_detailed(
        &flat_agents,
        &projection.ended,
        crate::harness::resume::ResumeContext {
            project_root: Some(project_root),
            workspace_id: &paths.workspace_id,
            rimz_bin: &crate::proc::rimz_exe(),
            runtime,
            profiles,
            max: machine.resume.max.saturating_sub(team_panes),
            logins: &logins,
            catalog: &catalog,
        },
        Path::is_dir,
        resume_session_present,
    );
    let mut plan = RecoveryPlan::new(teams.clone(), team, flat, refilled);
    plan.sort_by_freshness();
    plan
}

fn materialize_recovery(
    store: Option<&Store>,
    paths: &StatePaths,
    session_name: &str,
    planned: RecoveryPlan,
) -> (ResumePlan, Vec<RecoveryTabAgents>) {
    let MaterializedRecovery { resume, tab_agents } = match planned.materialize(
        session_name,
        RecoveryMaterializer::BestEffort {
            store,
            workspace_id: &paths.workspace_id,
        },
    ) {
        Ok(materialized) => materialized,
        Err(err) => {
            tracing::warn!(workspace = %paths.workspace_id, error = %err, "rebirth recovery materialization skipped");
            MaterializedRecovery {
                resume: ResumePlan::default(),
                tab_agents: Vec::new(),
            }
        }
    };
    (resume, tab_agents)
}

fn record_agents_ended(
    store: &Store,
    workspace_id: &WorkspaceId,
    session_name: &str,
    agents: &BTreeSet<(AgentKind, AgentSessionId)>,
    event_name: &str,
) -> BTreeSet<(AgentKind, AgentSessionId)> {
    let mut ended = BTreeSet::new();
    for (kind, agent_id) in agents {
        let observation = crate::agents::AgentLifecycleObservation::new(
            Some(agent_id.clone()),
            crate::agents::LifecycleSignal::Ended,
        );
        let event = crate::EventEnvelope::agent_lifecycle(
            workspace_id.clone(),
            session_name,
            kind.as_str(),
            event_name,
            &observation,
        );
        match store.append_event(&event) {
            Ok(_) => {
                ended.insert((kind.clone(), agent_id.clone()));
            }
            Err(err) => {
                tracing::warn!(workspace = %workspace_id, kind = %kind, agent_id = %agent_id, error = %err, "rebirth: could not stamp unrecovered agent ended");
            }
        }
    }
    ended
}

/// End `refilled` `rimz.seat-refilled` and take the ended keys out of the
/// pending record. Called once the tab or pane their replacement holds is open.
pub fn settle_refilled_seats(
    store: &Store,
    session_name: &str,
    refilled: &BTreeSet<(AgentKind, AgentSessionId)>,
) -> std::result::Result<(), RefillSettlementErr> {
    if refilled.is_empty() {
        return Ok(());
    }
    let ended = record_refilled_seats(store, &store.paths().workspace_id, session_name, refilled);
    pending_recovery::settle(store.paths(), &ended).map_err(RefillSettlementErr)
}

fn record_refilled_seats(
    store: &Store,
    workspace_id: &WorkspaceId,
    session_name: &str,
    agents: &BTreeSet<(AgentKind, AgentSessionId)>,
) -> BTreeSet<(AgentKind, AgentSessionId)> {
    let ended = record_agents_ended(
        store,
        workspace_id,
        session_name,
        agents,
        "rimz.seat-refilled",
    );
    if ended.is_empty() {
        return ended;
    }
    let sink = DiagSink::for_workspace(workspace_id.clone(), session_name.to_owned(), None);
    for (kind, agent_id) in &ended {
        sink.emit(DiagEvent::RecoverySeatRefilled {
            agent_kind: kind.clone(),
            agent_id: agent_id.clone(),
        });
    }
    ended
}

fn recovery_roster(
    paths: &StatePaths,
    agents: &[AgentState],
) -> BTreeSet<(AgentKind, AgentSessionId)> {
    let Some(roster) = live_roster::read(&paths.live_roster) else {
        tracing::debug!(path = %paths.live_roster.display(), "rebirth: live roster absent or unreadable");
        return BTreeSet::new();
    };
    let audited = agents
        .iter()
        .map(|agent| (agent.kind.clone(), agent.agent_id.clone()))
        .collect::<BTreeSet<_>>();
    let recoverable = roster
        .agents
        .intersection(&audited)
        .cloned()
        .collect::<BTreeSet<_>>();
    tracing::debug!(
        entries = roster.agents.len(),
        audited = recoverable.len(),
        "rebirth: live roster read"
    );
    recoverable
}

fn lost_agent_summaries(
    agents: &[AgentState],
    lost: &BTreeSet<(AgentKind, AgentSessionId)>,
) -> Vec<SessionDeathAgent> {
    lost.iter()
        .map(|(kind, agent_id)| SessionDeathAgent {
            kind: kind.clone(),
            agent_id: agent_id.clone(),
            name: find_agent(agents, kind, agent_id).and_then(|agent| agent.name.clone()),
        })
        .collect()
}

fn lost_agent_roster(
    agents: &[AgentState],
    lost: &BTreeSet<(AgentKind, AgentSessionId)>,
) -> Vec<AgentState> {
    agents
        .iter()
        .filter(|agent| lost.contains(&(agent.kind.clone(), agent.agent_id.clone())))
        .cloned()
        .collect()
}

fn append_session_death(
    store: &Store,
    workspace_id: &WorkspaceId,
    session_name: &str,
    marker: &LastDeathMarker,
) {
    let event = crate::EventEnvelope::session_death(
        workspace_id.clone(),
        session_name,
        marker.cause,
        marker.lost_agents.clone(),
    );
    if let Err(err) = store.append_event(&event) {
        tracing::warn!(workspace = %workspace_id, session = %session_name, error = %err, "session death event skipped");
    }
}

fn write_last_death_marker(paths: &StatePaths, marker: &LastDeathMarker) {
    if let Err(err) = crate::disk::atomic::write_temp_then_rename(&paths.last_death_marker, marker)
    {
        tracing::debug!(path = %paths.last_death_marker.display(), error = %err, "last death marker write skipped");
    }
}

/// Count panes a later settlement resumed in the incident that lost them.
fn count_later_recovery(paths: &StatePaths, recovered: usize) {
    let Some(mut death) = std::fs::read(&paths.last_death_marker)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<LastDeathMarker>(&bytes).ok())
    else {
        return;
    };
    death.recovered = Some(death.recovered.unwrap_or(0) + recovered);
    write_last_death_marker(paths, &death);
}

/// Park the dead incarnation's roster in the pending-recovery record, where
/// its agents wait for a decision. Birth must succeed here before creating a
/// session whose producer can replace the roster.
pub(crate) fn park_roster(paths: &StatePaths) -> Result<()> {
    let roster = live_roster::read(&paths.live_roster)
        .map(|roster| roster.agents)
        .unwrap_or_default();
    if roster.is_empty() {
        return Ok(());
    }
    pending_recovery::park(paths, &roster).with_context(|| format!(
        "cannot park lost agents in {}; restore write access to the record and release any stuck workspace lock, then retry rimz start; the live roster is unchanged",
        paths.pending_recovery.display()
    ))
}

fn close_boundary(store: Option<&Store>, paths: &StatePaths, session_name: &str) {
    if let Some(store) = store {
        let event = crate::EventEnvelope::session_rebirth(paths.workspace_id.clone(), session_name);
        if let Err(err) = store.append_event(&event) {
            tracing::warn!(workspace = %paths.workspace_id, error = %err, "rebirth boundary skipped");
        }
    }
    match std::fs::remove_file(&paths.live_roster) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            tracing::debug!(path = %paths.live_roster.display(), error = %err, "live roster clear skipped")
        }
    }
}

/// Record a birth whose roster was parked before the session was created.
pub(crate) fn record_boundary(workspace_id: &WorkspaceId, session_name: &str) {
    let result = (|| -> Result<()> {
        let paths = StatePaths::for_workspace(workspace_id.clone())?;
        let runtime = RuntimePaths::for_workspace(workspace_id.clone())?;
        record_boundary_at(paths, runtime, workspace_id, session_name);
        Ok(())
    })();
    if let Err(err) = result {
        tracing::warn!(workspace = %workspace_id, error = %err, "rebirth boundary skipped");
    }
}

fn record_boundary_at(
    paths: StatePaths,
    runtime: RuntimePaths,
    workspace_id: &WorkspaceId,
    session_name: &str,
) {
    let Some(store) = Store::open_existing(paths.clone(), runtime) else {
        tracing::warn!(workspace = %workspace_id, "rebirth store unavailable");
        return;
    };
    if let Ok(projection) = store.runtime_projection(crate::RuntimeScope::Audit) {
        let roster = recovery_roster(&paths, &projection.agents);
        let agents = lost_agent_roster(&projection.agents, &roster);
        cancel_child_runs(&store, &paths, &agents, &roster);
    }
    close_boundary(Some(&store), &paths, session_name);
}

fn archive_crash(
    paths: &StatePaths,
    cache: &CrashCacheSnapshot,
    roster: &[AgentState],
    at: Timestamp,
) -> Result<()> {
    let archive = paths.crashes_dir.join(archive_name(at));
    let mux_cache = archive.join("mux-cache");
    std::fs::create_dir_all(&mux_cache)
        .with_context(|| format!("creating crash archive {}", mux_cache.display()))?;
    write_cache_snapshot(cache, &mux_cache)?;
    crate::disk::atomic::write_temp_then_rename(&archive.join("roster.json"), &roster)
        .with_context(|| format!("writing crash roster {}", archive.display()))?;
    Ok(())
}

fn archive_name(at: Timestamp) -> String {
    at.strftime("%Y%m%dT%H%M%SZ").to_string()
}

fn cache_archive_relative(cache_root: &Path, source: &Path) -> PathBuf {
    if let Ok(relative) = source.strip_prefix(cache_root)
        && !relative.as_os_str().is_empty()
    {
        return relative.to_path_buf();
    }
    PathBuf::from(source.file_name().unwrap_or_else(|| OsStr::new("cache")))
}

fn capture_cache_sources(cache_root: &Path, sources: &[PathBuf]) -> CrashCacheSnapshot {
    let mut snapshot = CrashCacheSnapshot::default();
    for source in sources {
        if let Err(err) = capture_cache_path(
            source,
            &cache_archive_relative(cache_root, source),
            &mut snapshot.entries,
        )
        .with_context(|| format!("reading mux cache {}", source.display()))
        {
            snapshot.error = Some(format!("{err:#}"));
            break;
        }
    }
    snapshot
}

fn capture_cache_path(
    source: &Path,
    relative: &Path,
    entries: &mut Vec<CrashCacheEntry>,
) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(source)?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_dir() {
        entries.push(CrashCacheEntry::Directory(relative.to_path_buf()));
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            capture_cache_path(&entry.path(), &relative.join(entry.file_name()), entries)?;
        }
    } else if meta.is_file() {
        entries.push(CrashCacheEntry::File {
            path: relative.to_path_buf(),
            bytes: std::fs::read(source)?,
        });
    }
    Ok(())
}

fn write_cache_snapshot(cache: &CrashCacheSnapshot, mux_cache: &Path) -> Result<()> {
    for entry in &cache.entries {
        match entry {
            CrashCacheEntry::Directory(path) => std::fs::create_dir_all(mux_cache.join(path))
                .with_context(|| format!("archiving mux cache {}", path.display()))?,
            CrashCacheEntry::File { path, bytes } => {
                let destination = mux_cache.join(path);
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&destination, bytes)
                    .with_context(|| format!("archiving mux cache {}", path.display()))?;
            }
        }
    }
    if let Some(error) = cache.error.as_deref() {
        anyhow::bail!("{error}");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn boot_token() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .and_then(|id| tagged_boot_token("uuid", &id))
        .or_else(|| {
            std::fs::read_to_string("/proc/stat")
                .ok()
                .and_then(|stat| parse_proc_btime(&stat))
                .and_then(|btime| tagged_boot_token("btime", &btime))
        })
}

#[cfg(target_os = "macos")]
fn boot_token() -> Option<String> {
    sysctl_value("kern.bootsessionuuid")
        .and_then(|id| tagged_boot_token("uuid", &id))
        .or_else(|| {
            sysctl_value("kern.boottime")
                .and_then(|out| parse_kern_boottime(&out))
                .and_then(|btime| tagged_boot_token("btime", &btime))
        })
}

#[cfg(target_os = "macos")]
fn sysctl_value(name: &str) -> Option<String> {
    std::process::Command::new("sysctl")
        .args(["-n", name])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn boot_token() -> Option<String> {
    None
}

fn tagged_boot_token(source: &str, value: &str) -> Option<String> {
    non_empty_trimmed(value).map(|value| format!("{source}:{value}"))
}

fn non_empty_trimmed(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_proc_btime(stat: &str) -> Option<String> {
    stat.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        if fields.next()? != "btime" {
            return None;
        }
        let epoch = fields.next()?;
        if fields.next().is_some() || !epoch.chars().all(|ch| ch.is_ascii_digit()) {
            return None;
        }
        Some(epoch.to_owned())
    })
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_kern_boottime(out: &str) -> Option<String> {
    out.split([',', '{', '}']).find_map(|part| {
        let (key, value) = part.split_once('=')?;
        if key.trim() != "sec" {
            return None;
        }
        let epoch = value.trim();
        if epoch.is_empty() || !epoch.chars().all(|ch| ch.is_ascii_digit()) {
            return None;
        }
        Some(epoch.to_owned())
    })
}

#[derive(serde::Deserialize, serde::Serialize)]
struct BootMarker {
    boot_id: String,
}

fn read_boot_marker(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let marker = serde_json::from_slice::<BootMarker>(&bytes).ok()?;
    non_empty_trimmed(&marker.boot_id)
}

fn write_boot_marker(path: &Path, boot_id: &str) {
    let marker = BootMarker {
        boot_id: boot_id.to_owned(),
    };
    if let Err(err) = crate::disk::atomic::write_temp_then_rename_cache(path, &marker) {
        tracing::debug!(path = %path.display(), error = %err, "boot marker write skipped");
    }
}

fn boot_changed(previous: Option<&str>, current: Option<&str>) -> bool {
    match (previous, current) {
        (Some(previous), Some(current)) => previous != current,
        (None, Some(_)) => true,
        (_, None) => false,
    }
}

#[cfg(test)]
mod tests;
