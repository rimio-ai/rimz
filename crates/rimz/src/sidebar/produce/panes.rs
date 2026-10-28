//! Retained host pane production and private CLI reads share raced-read repair, confirmation, carry, and acceptance. Only the host publishes `snapshot.json` and metrics history.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::Result;
use crate::diag::record::{
    DiagEvent, FrameRejectReason, ManagedPaneEvidence, PaneDropEvidence, PaneDropViewEvidence,
};
use crate::disk::atomic;
use crate::ids::{AgentKind, MuxName, PaneId};
use crate::mux::{ClientFocusOptions, PaneListOptions, PaneListing, PaneReadConsistency};
use crate::sidebar::cache::{
    effective_pane_ttl, hibernated_check_due, presence_stamp_age_ms, read_snapshot_cache,
    snapshot_cache_is_fresh,
};
use crate::sidebar::frame::{FrameInputs, PaneFrame, PaneMetrics};
use crate::sidebar::timing::{PRESENCE_SAMPLE_TTL, SNAPSHOT_CACHE_TTL};
use crate::store::snapshot::PresenceSample;
use crate::utils::time::unix_now_ms;

mod carry;
mod starts;
mod validate;

use carry::{CarryOutcome, apply_carry_forward};
use starts::{
    HostedCarryDrop, carry_hosted_agent_stamps, drop_reused_pid_bindings,
    stamp_hosted_agent_processes, stamp_pane_process_starts,
};
use validate::{
    PublishVerdict, frame_publish_verdict, own_pane_missing, pane_count, shrink_needs_verification,
};

use super::metrics::{MetricsSampleCache, read_metrics_sample_cache, write_metrics_sample_cache};

pub(crate) struct PaneProducer {
    pub(super) prior: Option<Arc<PaneFrame>>,
    pub(super) hibernated_panes: HashSet<PaneId>,
    pub(super) agent_panes: HashSet<PaneId>,
    native_tenants: HashMap<PaneId, (u32, Option<String>)>,
    held_checks: HashMap<PaneId, Instant>,
    metrics: MetricsSampleCache,
    presence_probed_at_ms: Option<u64>,
}

impl PaneProducer {
    #[cfg(test)]
    pub(crate) fn seed_test_frame(&mut self, frame: Option<Arc<PaneFrame>>) {
        self.prior = frame;
    }
    pub(crate) fn seeded(runtime: &crate::RuntimePaths, session: &str) -> Self {
        let fixture = super::pane_fixture_active();
        Self {
            prior: (!fixture)
                .then(|| read_snapshot_cache(&runtime.pane_frame_path(), session))
                .flatten(),
            metrics: if fixture {
                MetricsSampleCache::default()
            } else {
                read_metrics_sample_cache(&runtime.lane_path("metrics-sample.json"))
            },
            presence_probed_at_ms: None,
            hibernated_panes: HashSet::new(),
            agent_panes: HashSet::new(),
            native_tenants: HashMap::new(),
            held_checks: HashMap::new(),
        }
    }

    pub(crate) fn tick(
        &mut self,
        runtime: &crate::RuntimePaths,
        mux: MuxName,
        session: &str,
        floor: Option<u64>,
        own_pane: Option<&PaneId>,
        diag: &crate::diag::DiagSink,
    ) -> Result<Arc<PaneFrame>> {
        self.tick_with(
            runtime,
            mux,
            session,
            floor,
            own_pane,
            diag,
            &mut |ctx, prior| ctx.produce(prior),
            &sample_client_presence,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn tick_with(
        &mut self,
        runtime: &crate::RuntimePaths,
        mux: MuxName,
        session: &str,
        floor: Option<u64>,
        own_pane: Option<&PaneId>,
        diag: &crate::diag::DiagSink,
        produce: &mut dyn FnMut(&mut FrameContext<'_>, Option<&PaneFrame>) -> Result<Accepted>,
        sample_presence: &dyn Fn(MuxName, &str) -> Option<PresenceSample>,
    ) -> Result<Arc<PaneFrame>> {
        if let Some(panes) = super::pane_list_fixture()? {
            let frame = Arc::new(crate::sidebar::frame::assemble_frame(
                panes,
                unix_now_ms(),
                session.to_owned(),
            ));
            self.prior = Some(Arc::clone(&frame));
            return Ok(frame);
        }
        let mut ttl = effective_pane_ttl(
            presence_stamp_age_ms(runtime),
            self.prior
                .as_ref()
                .is_some_and(|prior| prior.viewed_panes.is_empty()),
        );
        if self
            .prior
            .as_ref()
            .is_some_and(|prior| !prior.carried_panes.is_empty())
        {
            ttl = ttl.min(SNAPSHOT_CACHE_TTL);
        }
        let hibernated: HashSet<PaneId> = self.prior.as_ref().map_or_else(HashSet::new, |frame| {
            frame
                .pane_states()
                .filter(|pane| {
                    self.hibernated_panes.contains(&pane.pane_id)
                        && self.held_checks.contains_key(&pane.pane_id)
                })
                .map(|pane| pane.pane_id.clone())
                .collect()
        });
        self.held_checks.retain(|pane, _| hibernated.contains(pane));
        let fresh = self.prior.as_ref().filter(|prior| {
            snapshot_cache_is_fresh(prior, unix_now_ms(), floor, ttl)
                && matches!(
                    frame_publish_verdict(prior, own_pane),
                    PublishVerdict::Publish
                )
        });
        let frame = if let Some(prior) = fresh {
            let mut frame = Arc::clone(prior);
            let sampled = crate::proc::with_read_memo(|| {
                if !super::metrics::pane_metrics_due(
                    &frame,
                    &self.metrics,
                    &hibernated,
                    &hibernated,
                ) || !super::metrics::enrich_pane_metrics(
                    Arc::make_mut(&mut frame),
                    session,
                    &mut self.metrics,
                    &hibernated,
                    &hibernated,
                    &self.agent_panes,
                ) {
                    return false;
                }
                with_due_panes(Arc::make_mut(&mut frame), Some(prior), &hibernated, |due| {
                    annotate_elevated_agents(due, RepairReaders::system().elevated_agent);
                });
                true
            });
            if sampled {
                write_metrics_sample_cache(runtime, &self.metrics);
                publish_frame(
                    &runtime.pane_frame_path(),
                    Arc::make_mut(&mut frame),
                    PaneFramePublicationKind::Metrics,
                    own_pane,
                    diag,
                );
            }
            if presence_sample_due(&frame, self.presence_probed_at_ms, unix_now_ms()) {
                self.presence_probed_at_ms = Some(unix_now_ms());
                if let Some(sample) = sample_presence(mux, session)
                    && apply_presence_sample(Arc::make_mut(&mut frame), sample)
                {
                    publish_frame(
                        &runtime.pane_frame_path(),
                        Arc::make_mut(&mut frame),
                        PaneFramePublicationKind::Presence,
                        own_pane,
                        diag,
                    );
                }
            }
            frame
        } else {
            let mut native_tenants = self.native_tenants.clone();
            let mut held_checks = self.held_checks.clone();
            let mut ctx = FrameContext {
                runtime,
                mux,
                session,
                own_pane,
                diag,
                metrics: &mut self.metrics,
                sampled: false,
                hibernated_panes: &self.hibernated_panes,
                agent_panes: &self.agent_panes,
                native_tenants: &mut native_tenants,
                held_checks: &mut held_checks,
                checked_panes: HashSet::new(),
            };
            let accepted = produce(&mut ctx, self.prior.as_deref());
            if ctx.sampled {
                write_metrics_sample_cache(runtime, ctx.metrics);
            }
            match accepted? {
                Accepted::Fresh(mut frame) => {
                    self.native_tenants = native_tenants;
                    self.held_checks = held_checks;
                    publish_frame(
                        &runtime.pane_frame_path(),
                        &mut frame,
                        PaneFramePublicationKind::Topology,
                        own_pane,
                        diag,
                    );
                    Arc::new(frame)
                }
                Accepted::Held(frame) => Arc::new(frame),
            }
        };
        self.prior = Some(Arc::clone(&frame));
        Ok(frame)
    }
}

pub(crate) fn read_pane_frame(
    runtime: &crate::RuntimePaths,
    mux: MuxName,
    session: &str,
    own_pane: Option<&PaneId>,
    diag: &crate::diag::DiagSink,
) -> Result<PaneFrame> {
    if let Some(panes) = super::pane_list_fixture()? {
        return Ok(crate::sidebar::frame::assemble_frame(
            panes,
            unix_now_ms(),
            session.to_owned(),
        ));
    }
    let prior = read_snapshot_cache(&runtime.pane_frame_path(), session);
    let mut metrics = read_metrics_sample_cache(&runtime.lane_path("metrics-sample.json"));
    match (FrameContext {
        runtime,
        mux,
        session,
        own_pane,
        diag,
        metrics: &mut metrics,
        sampled: false,
        hibernated_panes: &HashSet::new(),
        agent_panes: &HashSet::new(),
        native_tenants: &mut HashMap::new(),
        held_checks: &mut HashMap::new(),
        checked_panes: HashSet::new(),
    })
    .produce(prior.as_deref())?
    {
        Accepted::Fresh(frame) | Accepted::Held(frame) => Ok(frame),
    }
}

struct FrameContext<'a> {
    runtime: &'a crate::RuntimePaths,
    mux: MuxName,
    session: &'a str,
    own_pane: Option<&'a PaneId>,
    diag: &'a crate::diag::DiagSink,
    metrics: &'a mut MetricsSampleCache,
    sampled: bool,
    hibernated_panes: &'a HashSet<PaneId>,
    agent_panes: &'a HashSet<PaneId>,
    native_tenants: &'a mut HashMap<PaneId, (u32, Option<String>)>,
    held_checks: &'a mut HashMap<PaneId, Instant>,
    checked_panes: HashSet<PaneId>,
}

/// Enumerate only the session's panes; the store rollup is read separately. The host amortizes this read across the fleet, while private CLI calls always invoke it.
/// Zellij topology may carry plugin focus/presence; other paths retain the direct backend sample fallback.
fn list_session_panes(
    mux: MuxName,
    session: &str,
    runtime_paths: Option<crate::RuntimePaths>,
    workspace_id: crate::WorkspaceId,
    min_topology_produced_at_ms: Option<u64>,
    command_timeout: Option<Duration>,
    prefer_authoritative: bool,
) -> Result<PaneListing> {
    Ok(crate::mux::backend_for(mux).list_panes(PaneListOptions {
        session_name: Some(session.to_owned()),
        runtime_paths,
        workspace_id: Some(workspace_id),
        min_topology_produced_at_ms,
        consistency: if prefer_authoritative {
            PaneReadConsistency::PreferAuthoritative
        } else {
            PaneReadConsistency::Cached
        },
        command_timeout,
    })?)
}

fn client_view(mux: MuxName, session: &str) -> Result<crate::mux::ClientView> {
    let view = crate::mux::backend_for(mux).client_view(ClientFocusOptions {
        session_name: Some(session.to_owned()),
        ..Default::default()
    })?;
    Ok(view)
}

fn presence_sample_from_client_view(view: &crate::mux::ClientView) -> PresenceSample {
    PresenceSample {
        human_clients: view.presence.human_clients,
        last_input_ms: view.presence.last_input_ms,
        sampled_at_ms: unix_now_ms(),
    }
}

fn sample_client_presence(mux: MuxName, session: &str) -> Option<PresenceSample> {
    client_view(mux, session)
        .ok()
        .map(|view| presence_sample_from_client_view(&view))
}

/// Join a fresh frame to the last published same-session frame. Raced-null
/// fields repair only when the process identity stayed stable; a command or
/// root-pid change rotates the prior current process to `previous` and keeps
/// the fresh process record clean.
fn rotate_from_prior(frame: &mut PaneFrame, prior: Option<&PaneFrame>) {
    if let Some(prev) = prior {
        frame.rotate_against_prior(prev);
    }
}

/// The pane ids a fresh mux roster read left without a process start — the
/// set the derived process stamp owns ([`stamp_pane_process_starts`]). Captured before
/// the frame rotates against the prior publish, so a backend-reported start is
/// never confused with RimZ's own derived stamp and never overwritten by one.
fn natively_unstamped(frame: &PaneFrame) -> HashSet<PaneId> {
    frame
        .pane_states()
        .filter(|pane| pane.current.started_at.is_none())
        .map(|pane| pane.pane_id.clone())
        .collect()
}

fn stamp_pane_resumed_session_ids(
    frame: &mut PaneFrame,
    root_resume: &dyn Fn(u32) -> crate::agents::ResumeProbe,
) {
    for pane in frame.pane_states_mut() {
        if pane.current.resumed_session_id.is_some() {
            continue;
        }
        let supports_resume_discovery = pane
            .current
            .command
            .as_deref()
            .and_then(crate::agents::registry::command_agent_kind)
            .or_else(|| {
                pane.current
                    .hosted_agent_kind
                    .as_ref()
                    .map(crate::ids::AgentKind::as_str)
            })
            .and_then(crate::agents::find_definition)
            .is_some();
        if !supports_resume_discovery {
            continue;
        }
        if let Some(crate::agents::ResumeProbe::Resumed(resumed)) =
            pane.current.pid.map(root_resume)
        {
            pane.current.resumed_session_id = Some(resumed);
        }
    }
}

struct RepairReaders<'a> {
    cmdline: &'a dyn Fn(u32) -> Option<String>,
    comm: &'a dyn Fn(u32) -> Option<String>,
    children: &'a dyn Fn(u32) -> Vec<u32>,
    hosted_process: &'a dyn Fn(u32) -> Option<crate::proc::HostedAgentProcess>,
    cwd: &'a dyn Fn(u32) -> Option<PathBuf>,
    resumed_session: &'a dyn Fn(u32) -> crate::agents::ResumeProbe,
    root_start: &'a dyn Fn(&str, u32) -> Option<jiff::Timestamp>,
    cwd_starts: &'a dyn Fn(&str, &str) -> Vec<jiff::Timestamp>,
    hosted_absent: &'a dyn Fn(&str, u32) -> bool,
    process_start: &'a dyn Fn(u32) -> Option<jiff::Timestamp>,
    elevated_agent: &'a dyn Fn(u32) -> Option<crate::pane::ElevatedAgent>,
    backfill_pids: &'a dyn Fn(&mut PaneFrame, &str),
}

impl RepairReaders<'_> {
    fn system() -> Self {
        Self {
            cmdline: &crate::proc::cmdline,
            comm: &crate::proc::comm,
            children: &crate::proc::children,
            hosted_process: &crate::proc::hosted_agent_process_for_root,
            cwd: &crate::proc::cwd,
            resumed_session: &crate::agents::resumed_session_id_for_root,
            root_start: &crate::proc::in_pane_agent_start_for_root,
            cwd_starts: &crate::proc::in_pane_agent_starts,
            hosted_absent: &crate::proc::hosted_agent_absent_under_root,
            process_start: &crate::proc::process_start,
            elevated_agent: &crate::proc::elevated_in_pane_agent,
            backfill_pids: &|frame, session| {
                super::metrics::backfill_zellij_pane_pids_from_proc(frame, session);
            },
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn repair_pane_frame(
    frame: &mut PaneFrame,
    prior: Option<&PaneFrame>,
    session: &str,
    metrics: Option<&mut MetricsSampleCache>,
    hibernated_panes: &HashSet<PaneId>,
    skipped: &HashSet<PaneId>,
    agent_panes: &HashSet<PaneId>,
    readers: &RepairReaders<'_>,
) -> (Vec<HostedCarryDrop>, bool) {
    crate::proc::with_read_memo(|| {
        let mut unstamped = natively_unstamped(frame);
        unstamped.retain(|pane| !skipped.contains(pane));
        rotate_from_prior(frame, prior);
        let sampled = if let Some(metrics) = metrics {
            super::metrics::enrich_pane_metrics(
                frame,
                session,
                metrics,
                hibernated_panes,
                skipped,
                agent_panes,
            )
        } else {
            (readers.backfill_pids)(frame, session);
            false
        };
        let drops = repair_processes(frame, prior, &unstamped, skipped, readers);
        (drops, sampled)
    })
}

fn repair_processes(
    frame: &mut PaneFrame,
    prior: Option<&PaneFrame>,
    unstamped: &HashSet<PaneId>,
    skipped: &HashSet<PaneId>,
    readers: &RepairReaders<'_>,
) -> Vec<HostedCarryDrop> {
    with_due_panes(frame, prior, skipped, |due| {
        backfill_pane_commands(due, readers.cmdline, readers.comm);
        backfill_wrapper_spawn_commands(due, readers.cmdline);
        reconcile_active_commands(due, readers.cmdline, readers.comm, readers.children);
        stamp_hosted_agent_processes(due, readers.hosted_process);
        backfill_pane_cwds(due, readers.cwd);
        stamp_pane_resumed_session_ids(due, readers.resumed_session);
    });
    // Held starts still participate in duplicate detection and the cwd census.
    stamp_pane_process_starts(frame, unstamped, readers.root_start, readers.cwd_starts);
    with_due_panes(frame, prior, skipped, |due| {
        let hosted_carry_drops =
            carry_hosted_agent_stamps(due, prior, unix_now_ms(), readers.hosted_absent);
        if (readers.process_start)(std::process::id()).is_some() {
            drop_reused_pid_bindings(due, readers.root_start, readers.process_start);
        }
        annotate_elevated_agents(due, readers.elevated_agent);
        stamp_first_seen(due);
        hosted_carry_drops
    })
}

fn hibernated_panes_for(
    frame: &PaneFrame,
    native_tenants: &HashMap<PaneId, (u32, Option<String>)>,
    requested: &HashSet<PaneId>,
) -> HashSet<PaneId> {
    frame
        .pane_states()
        .filter(|pane| {
            requested.contains(&pane.pane_id)
                && pane.current.pid.is_some()
                && native_tenants
                    .get(&pane.pane_id)
                    .is_some_and(|(pid, spawn)| {
                        pane.current.pid == Some(*pid) && pane.current.spawn_command == *spawn
                    })
        })
        .map(|pane| pane.pane_id.clone())
        .collect()
}

fn held_panes_for_check(
    frame: &PaneFrame,
    hibernated: &HashSet<PaneId>,
    checks: &mut HashMap<PaneId, Instant>,
    now: Instant,
) -> HashSet<PaneId> {
    checks.retain(|pane, _| hibernated.contains(pane));
    frame
        .pane_states()
        .filter(|pane| {
            !hibernated_check_due(
                checks,
                pane.pane_id.clone(),
                hibernated.contains(&pane.pane_id),
                now,
            )
        })
        .map(|pane| pane.pane_id.clone())
        .collect()
}

fn with_due_panes<T>(
    frame: &mut PaneFrame,
    prior: Option<&PaneFrame>,
    skipped: &HashSet<PaneId>,
    repair: impl FnOnce(&mut PaneFrame) -> T,
) -> T {
    if skipped.is_empty() {
        return repair(frame);
    }
    let previous: HashMap<_, _> = prior
        .into_iter()
        .flat_map(PaneFrame::pane_states)
        .map(|pane| (&pane.pane_id, pane))
        .collect();
    let mut held = Vec::new();
    for (tab_index, tab) in frame.tabs.iter_mut().enumerate() {
        tab.panes = std::mem::take(&mut tab.panes)
            .into_iter()
            .enumerate()
            .filter_map(|(index, mut pane)| {
                if !skipped.contains(&pane.pane_id) {
                    return Some(pane);
                }
                if let Some(old) = previous.get(&pane.pane_id) {
                    pane.current = old.current.clone();
                    pane.previous = old.previous.clone();
                    pane.children = old.children.clone();
                    pane.metrics = old.metrics;
                    pane.first_seen_at_ms = old.first_seen_at_ms;
                    pane.hosted_carry_since_ms = old.hosted_carry_since_ms;
                }
                held.push((tab_index, index, pane));
                None
            })
            .collect();
    }
    let result = repair(frame);
    for (tab, index, pane) in held {
        frame.tabs[tab].panes.insert(index, pane);
    }
    result
}

/// Name a pane the mux reported with a root pid but no command from that
/// root's command line, falling back to its base command name. A fresh Zellij
/// shell arrives this way, and without a command it folds no row until
/// Zellij's own command probe reports one. A root running RimZ itself stays
/// commandless: those panes are named by their title or spawn command, and
/// the launcher's argv is chrome.
fn backfill_pane_commands(
    frame: &mut PaneFrame,
    proc_cmdline: &dyn Fn(u32) -> Option<String>,
    proc_comm: &dyn Fn(u32) -> Option<String>,
) {
    for pane in frame.pane_states_mut() {
        if pane.current.command.is_some() {
            continue;
        }
        let Some(pid) = pane.current.pid else {
            continue;
        };
        pane.current.command = proc_cmdline(pid)
            .filter(|cmdline| !cmdline.is_empty())
            .or_else(|| proc_comm(pid))
            .filter(|command| crate::proc::program_label(command) != "rimz");
    }
}

/// Recover a pane's spawn command from RimZ's supervised agent wrapper when the
/// mux did not retain one. This covers panes born without backend spawn
/// metadata; tmux otherwise reports its immutable `pane_start_command`. Only an
/// empty spawn command is filled, and only from a cmdline that classifies as an
/// agent, so real metadata is never overwritten.
fn backfill_wrapper_spawn_commands(
    frame: &mut PaneFrame,
    proc_cmdline: &dyn Fn(u32) -> Option<String>,
) {
    for pane in frame.pane_states_mut() {
        if pane.current.spawn_command.is_some() {
            continue;
        }
        let Some(pid) = pane.current.pid else {
            continue;
        };
        if let Some(cmdline) = proc_cmdline(pid)
            .filter(|cmdline| crate::agents::registry::command_agent_kind(cmdline).is_some())
        {
            pane.current.spawn_command = Some(cmdline);
        }
    }
}

fn stamp_first_seen(frame: &mut PaneFrame) {
    let observed_at_ms = frame.observed_at_ms;
    for pane in frame.pane_states_mut() {
        if pane.first_seen_at_ms.is_none() {
            pane.first_seen_at_ms = Some(observed_at_ms);
        }
    }
}

pub fn repaired_pane_frame_for_binding(
    runtime: &crate::RuntimePaths,
    mux: MuxName,
    session: &str,
    command_timeout: Duration,
) -> Result<PaneFrame> {
    let cache_path = runtime.pane_frame_path();
    let listing = match super::pane_list_fixture()? {
        Some(fixture) => PaneListing {
            authoritative: true,
            panes: fixture,
            views: Vec::new(),
            observed_at_ms: unix_now_ms(),
            session_focus: None,
            client_view: None,
            client_sample_withheld: false,
        },
        None => list_session_panes(
            mux,
            session,
            Some(runtime.clone()),
            runtime.workspace_id.clone(),
            None,
            Some(command_timeout),
            false,
        )?,
    };
    let prior = read_snapshot_cache(&cache_path, session);
    let (mut frame, diagnostics) = crate::sidebar::frame::assemble_frame_from_inputs(FrameInputs {
        panes: listing.panes,
        views: listing.views,
        produced_at_ms: unix_now_ms(),
        observed_at_ms: listing.observed_at_ms,
        session_name: session.to_owned(),
        session_focus: None,
        client_viewed: &[],
        client_views: &[],
        client_view_fresh: false,
        client_sample_withheld: listing.client_sample_withheld,
        prior: prior.as_deref(),
    });
    let diag = crate::diag::DiagSink::for_workspace(
        runtime.workspace_id.clone(),
        session.to_owned(),
        None,
    );
    emit_frame_diagnostics(&diag, diagnostics);
    let (hosted_carry_drops, _) = repair_pane_frame(
        &mut frame,
        prior.as_deref(),
        session,
        None,
        &HashSet::new(),
        &HashSet::new(),
        &HashSet::new(),
        &RepairReaders::system(),
    );
    emit_hosted_carry_dropped(&diag, hosted_carry_drops);
    Ok(frame)
}

/// Fill a pane's raced-empty cwd from the process backend once the root pid
/// is known. A fresh mux roster can answer a just-born pane with an empty cwd
/// for a tick; without one the pane groups under `external` and flickers there
/// until the mux reports the path. Only an empty cwd is ever filled — a
/// mux-reported cwd is authoritative because it tracks OSC7/foreground chdir,
/// which can diverge from the root process cwd. A process cwd that no longer
/// exists is also skipped, since Linux annotates deleted cwd targets with a
/// publish-unsafe `" (deleted)"` suffix.
fn backfill_pane_cwds(frame: &mut PaneFrame, proc_cwd: &dyn Fn(u32) -> Option<PathBuf>) {
    for pane in frame.pane_states_mut() {
        if pane
            .current
            .cwd
            .as_deref()
            .is_some_and(|cwd| !cwd.is_empty())
        {
            continue;
        }
        let Some(pid) = pane.current.pid else {
            continue;
        };
        if let Some(cwd) = proc_cwd(pid)
            .filter(|path| path.exists())
            .and_then(|path| path.into_os_string().into_string().ok())
        {
            pane.current.cwd = Some(cwd);
        }
    }
}

/// Reconcile foreground commands against the pane's root process tree. Zellij's presence topology is a latency cache: an active command absent from the tree is cleared after the shell returns to idle. Comm-only muxes are probed even for idle-looking shells, retaining matched argv for process-row activity and detail, never identity.
/// The synthetic sidebar chrome marker is classification metadata and stays
/// exempt from this probe.
fn reconcile_active_commands(
    frame: &mut PaneFrame,
    proc_cmdline: &dyn Fn(u32) -> Option<String>,
    proc_comm: &dyn Fn(u32) -> Option<String>,
    proc_children: &dyn Fn(u32) -> Vec<u32>,
) {
    for pane in frame.pane_states_mut() {
        let Some(command) = pane.current.command.as_deref() else {
            continue;
        };
        // The sidebar chrome marker is classification metadata, not a live
        // foreground claim: the renderer process is labelled `rimz`, so a
        // process probe for `rimz-sidebar` would always clear it and break
        // chrome admission and sibling classification downstream.
        if crate::pane::command_is_sidebar_chrome(command) {
            continue;
        }
        let match_mode = ProcessCommandMatch::for_mux(pane.pane_id.mux());
        if match_mode == ProcessCommandMatch::ExactCmdline
            && !crate::store::snapshot::process_is_active(command)
        {
            continue;
        }
        let Some(root_pid) = pane.current.pid else {
            continue;
        };
        match process_tree_command_status(
            root_pid,
            command,
            match_mode,
            proc_cmdline,
            proc_comm,
            proc_children,
        ) {
            ProcessCommandStatus::Present(foreground_cmdline) => {
                pane.current.foreground_cmdline = foreground_cmdline;
            }
            ProcessCommandStatus::Absent => {
                pane.current.command = None;
                pane.current.foreground_cmdline = None;
                pane.metrics = PaneMetrics::default();
                pane.children.clear();
            }
            ProcessCommandStatus::Unknown => {}
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProcessCommandMatch {
    /// Zellij reports a full foreground argv/cmdline, and its pid backfill uses
    /// that same exact equality. Keep the check symmetric so an unrelated
    /// same-program descendant cannot keep a stale full command alive.
    ExactCmdline,
    /// tmux reports `#{pane_current_command}`: a short program name. Match it
    /// against the raw process basename/comm, without wrapper projection.
    ProgramLabel,
}

impl ProcessCommandMatch {
    fn for_mux(mux: MuxName) -> Self {
        if crate::mux::lists_full_cmdline(mux) {
            Self::ExactCmdline
        } else {
            Self::ProgramLabel
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ProcessCommandStatus {
    Present(Option<String>),
    Absent,
    Unknown,
}

fn process_tree_command_status(
    root_pid: u32,
    command: &str,
    match_mode: ProcessCommandMatch,
    proc_cmdline: &dyn Fn(u32) -> Option<String>,
    proc_comm: &dyn Fn(u32) -> Option<String>,
    proc_children: &dyn Fn(u32) -> Vec<u32>,
) -> ProcessCommandStatus {
    let mut found_process_evidence = false;
    match process_command_probe(root_pid, command, match_mode, proc_cmdline, proc_comm) {
        ProcessCommandProbe::Match(cmdline) => return ProcessCommandStatus::Present(cmdline),
        ProcessCommandProbe::Mismatch => found_process_evidence = true,
        ProcessCommandProbe::Unknown => {}
    }

    let mut seen = HashSet::new();
    let mut stack = proc_children(root_pid);
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        match process_command_probe(pid, command, match_mode, proc_cmdline, proc_comm) {
            ProcessCommandProbe::Match(cmdline) => return ProcessCommandStatus::Present(cmdline),
            ProcessCommandProbe::Mismatch => found_process_evidence = true,
            ProcessCommandProbe::Unknown => {}
        }
        stack.extend(proc_children(pid));
    }
    if found_process_evidence {
        ProcessCommandStatus::Absent
    } else {
        ProcessCommandStatus::Unknown
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ProcessCommandProbe {
    Match(Option<String>),
    Mismatch,
    Unknown,
}

fn process_command_probe(
    pid: u32,
    command: &str,
    match_mode: ProcessCommandMatch,
    proc_cmdline: &dyn Fn(u32) -> Option<String>,
    proc_comm: &dyn Fn(u32) -> Option<String>,
) -> ProcessCommandProbe {
    let command = command.trim();
    if command.is_empty() {
        return ProcessCommandProbe::Unknown;
    }
    let command_label = crate::proc::command::argv0_label(command);
    let mut saw_cmdline_mismatch = false;
    match proc_cmdline(pid).map(|cmdline| cmdline.trim().to_owned()) {
        Some(cmdline) if !cmdline.is_empty() => match match_mode {
            ProcessCommandMatch::ExactCmdline if cmdline == command => {
                return ProcessCommandProbe::Match(None);
            }
            ProcessCommandMatch::ExactCmdline => return ProcessCommandProbe::Mismatch,
            ProcessCommandMatch::ProgramLabel
                if crate::proc::command::argv0_label(&cmdline) == command_label =>
            {
                return ProcessCommandProbe::Match(Some(cmdline));
            }
            ProcessCommandMatch::ProgramLabel => saw_cmdline_mismatch = true,
        },
        _ => {}
    }
    match match_mode {
        ProcessCommandMatch::ExactCmdline => ProcessCommandProbe::Unknown,
        ProcessCommandMatch::ProgramLabel => match proc_comm(pid)
            .map(|comm| comm.trim().to_owned())
            .filter(|comm| !comm.is_empty())
        {
            Some(comm) if comm == command_label => ProcessCommandProbe::Match(None),
            Some(_) => ProcessCommandProbe::Mismatch,
            None if saw_cmdline_mismatch => ProcessCommandProbe::Mismatch,
            None => ProcessCommandProbe::Unknown,
        },
    }
}

/// Mark panes whose foreground command is an elevation wrapper and whose
/// descendant tree contains a known agent CLI running as another real uid. The
/// marker is display-only: it does not rewrite the pane command, so the snapshot
/// bind ladder cannot mistake the foreign user's agent for a local session.
fn annotate_elevated_agents(
    frame: &mut PaneFrame,
    elevated_agent: &dyn Fn(u32) -> Option<crate::pane::ElevatedAgent>,
) {
    for pane in frame.pane_states_mut() {
        pane.current.elevated_agent = None;
        let Some(command) = pane.current.command.as_deref() else {
            continue;
        };
        if !crate::proc::command_starts_with_elevation_wrapper(command) {
            continue;
        }
        if let Some(pid) = pane.current.pid {
            pane.current.elevated_agent = elevated_agent(pid);
        }
    }
}

impl FrameContext<'_> {
    fn candidate(
        &mut self,
        prior: Option<&PaneFrame>,
        enrich_metrics: bool,
        min_topology_produced_at_ms: Option<u64>,
        authoritative: bool,
    ) -> Result<(PaneFrame, bool)> {
        let listing = match list_session_panes(
            self.mux,
            self.session,
            Some(self.runtime.clone()),
            self.runtime.workspace_id.clone(),
            min_topology_produced_at_ms,
            None,
            authoritative,
        ) {
            Ok(panes) => panes,
            Err(err) => {
                emit_mux_error(self.diag, prior, &err);
                return Err(err);
            }
        };
        let PaneListing {
            authoritative: listing_authoritative,
            panes,
            views,
            observed_at_ms,
            session_focus,
            client_view: pushed_client_view,
            client_sample_withheld,
        } = listing;
        let panes = filter_foreign_session_panes(panes, self.session, self.diag);
        // Renderer paint gating depends on fresh client focus. Zellij's
        // topology push carries it; backends without pushed presence fall back
        // to a direct client sample.
        let prior_client_view = || {
            prior.as_ref().map_or_else(
                || (Vec::new(), Vec::new(), None),
                |prior| {
                    (
                        prior.viewed_panes.clone(),
                        prior.client_views.clone(),
                        prior.presence,
                    )
                },
            )
        };
        let (viewed_panes, client_views, presence, client_view_resolved) = match pushed_client_view
        {
            Some(client_view) => {
                let presence = presence_sample_from_client_view(&client_view);
                (
                    client_view.viewed_panes,
                    client_view.clients,
                    Some(presence),
                    true,
                )
            }
            None => match client_view(self.mux, self.session) {
                Ok(client_view) => {
                    let presence = presence_sample_from_client_view(&client_view);
                    (
                        client_view.viewed_panes,
                        client_view.clients,
                        Some(presence),
                        true,
                    )
                }
                Err(_) if self.mux == MuxName::Zellij => {
                    let (viewed, clients, presence) = prior_client_view();
                    (viewed, clients, presence, false)
                }
                Err(_) => (Vec::new(), Vec::new(), None, false),
            },
        };
        let (mut frame, diagnostics) =
            crate::sidebar::frame::assemble_frame_from_inputs(FrameInputs {
                panes,
                views,
                produced_at_ms: unix_now_ms(),
                observed_at_ms,
                session_name: self.session.to_owned(),
                session_focus,
                client_viewed: &viewed_panes,
                client_views: &client_views,
                client_view_fresh: client_view_resolved,
                client_sample_withheld,
                prior,
            });
        frame.presence = presence;
        emit_frame_diagnostics(self.diag, diagnostics);
        let hibernated = hibernated_panes_for(&frame, self.native_tenants, self.hibernated_panes);
        *self.native_tenants = frame
            .pane_states()
            .filter_map(|pane| {
                Some((
                    pane.pane_id.clone(),
                    (pane.current.pid?, pane.current.spawn_command.clone()),
                ))
            })
            .collect();
        for pane in &self.checked_panes {
            self.held_checks.remove(pane);
        }
        let skipped = held_panes_for_check(&frame, &hibernated, self.held_checks, Instant::now());
        self.checked_panes.extend(
            frame
                .pane_states()
                .filter(|pane| !skipped.contains(&pane.pane_id))
                .map(|pane| pane.pane_id.clone()),
        );
        let (hosted_carry_drops, sampled) = repair_pane_frame(
            &mut frame,
            prior,
            self.session,
            enrich_metrics.then_some(&mut *self.metrics),
            &hibernated,
            &skipped,
            self.agent_panes,
            &RepairReaders::system(),
        );
        self.sampled |= sampled;
        emit_hosted_carry_dropped(self.diag, hosted_carry_drops);
        Ok((frame, listing_authoritative))
    }

    fn produce(&mut self, prior: Option<&PaneFrame>) -> Result<Accepted> {
        let frame = self.candidate(prior, true, None, false)?;
        let bindings = super::metrics::pane_root_bindings(self.metrics);
        let diag = self.diag;
        let own_pane = self.own_pane;
        let frame = confirm_and_carry_with(
            frame,
            prior,
            own_pane,
            &mut |enrich, floor, authoritative| self.candidate(prior, enrich, floor, authoritative),
            diag,
            true,
            &bindings,
        )?;
        accept(frame, prior.cloned(), own_pane, diag)
    }
}

fn filter_foreign_session_panes(
    panes: Vec<crate::pane::PaneRef>,
    session: &str,
    diag: &crate::diag::DiagSink,
) -> Vec<crate::pane::PaneRef> {
    panes
        .into_iter()
        .filter_map(|pane| {
            if pane.session_name == session {
                Some(pane)
            } else {
                diag.emit(DiagEvent::ForeignSessionPane {
                    pane_id: pane.pane_id,
                    session: pane.session_name,
                });
                None
            }
        })
        .collect()
}

fn emit_frame_diagnostics(diag: &crate::diag::DiagSink, events: Vec<DiagEvent>) {
    for event in events {
        diag.emit(event);
    }
}

/// Repair incomplete frames with a prefer-authoritative re-pull. Server listings
/// refute missing siblings in tabs they list; wholly omitted tabs and cache
/// fallbacks retain carry protection. Own-pane omissions and large shrinks also
/// trigger confirmation.
fn confirm_and_carry_with(
    (frame, authoritative): (PaneFrame, bool),
    prior: Option<&PaneFrame>,
    own_pane: Option<&PaneId>,
    produce_candidate: &mut dyn FnMut(bool, Option<u64>, bool) -> Result<(PaneFrame, bool)>,
    diag: &crate::diag::DiagSink,
    enrich_metrics: bool,
    bindings: &std::collections::HashMap<PaneId, super::metrics::PaneRootBinding>,
) -> Result<PaneFrame> {
    let initial_now_ms = unix_now_ms();
    let initial_frame = frame.clone();
    let initial = apply_carry_forward(
        frame,
        authoritative,
        prior,
        own_pane,
        bindings,
        &read_start_ticks,
        initial_now_ms,
    );
    let needs_confirm = !initial.carried.is_empty()
        || initial.ambiguous_loss
        || shrink_needs_verification(&initial.frame, prior)
        || own_pane_missing(&initial.frame, own_pane);
    if !needs_confirm {
        emit_carry_expired(diag, &initial);
        return Ok(initial.frame);
    }

    let prior_count = prior.map(pane_count).unwrap_or_default();
    let confirm_floor = Some(unix_now_ms());
    let (verified, authoritative) = produce_candidate(enrich_metrics, confirm_floor, true)?;
    let verified_count = pane_count(&verified);
    let confirmed_at_ms = unix_now_ms();
    let verified_frame = verified.clone();
    let confirmed = apply_carry_forward(
        verified,
        authoritative,
        prior,
        own_pane,
        bindings,
        &read_start_ticks,
        confirmed_at_ms,
    );
    if confirmed.carried.is_empty() {
        if !initial.carried.is_empty() {
            emit_pane_carry_refuted(
                diag,
                &initial,
                prior_count,
                pane_count(&initial_frame),
                verified_count,
            );
        }
        if verified_count == pane_count(&initial.frame)
            && shrink_needs_verification(&initial.frame, prior)
        {
            diag.emit(DiagEvent::FrameShrinkVerified {
                prior: prior_count,
                fresh: verified_count,
            });
        }
    } else {
        emit_pane_carry_forward(
            diag,
            prior,
            (&verified_frame, authoritative),
            &confirmed,
            prior_count,
            verified_count,
            confirmed_at_ms,
        );
    }
    emit_carry_expired(diag, &confirmed);
    Ok(confirmed.frame)
}

fn read_start_ticks(pid: u32) -> Option<u64> {
    crate::proc::stat_metrics(pid).and_then(live_start_ticks)
}

fn live_start_ticks(stat: crate::proc::StatMetrics) -> Option<u64> {
    (stat.state != 'Z').then_some(stat.start_ticks)
}

fn emit_pane_carry_forward(
    diag: &crate::diag::DiagSink,
    prior: Option<&PaneFrame>,
    (offending_frame, authoritative): (&PaneFrame, bool),
    outcome: &CarryOutcome,
    prior_count: usize,
    fresh_count: usize,
    at_ms: u64,
) {
    let carried = outcome
        .carried
        .iter()
        .map(|carried| carried.pane_id.clone())
        .collect::<Vec<_>>();
    let mut pids = outcome
        .carried
        .iter()
        .filter_map(|carried| carried.pid)
        .collect::<Vec<_>>();
    pids.sort_unstable();
    pids.dedup();
    let frames_ref = prior.and_then(|prior| {
        diag.capture_frame_pair("pane_carry_forward", prior, offending_frame, at_ms)
    });
    diag.emit(DiagEvent::PaneCarryForward {
        carried,
        pids,
        prior: prior_count,
        fresh: fresh_count,
        cli_confirmed: authoritative,
        frames_ref,
    });
}

fn emit_pane_carry_refuted(
    diag: &crate::diag::DiagSink,
    outcome: &CarryOutcome,
    prior_count: usize,
    fresh_count: usize,
    verified_count: usize,
) {
    let carried = outcome
        .carried
        .iter()
        .map(|carried| carried.pane_id.clone())
        .collect::<Vec<_>>();
    let mut pids = outcome
        .carried
        .iter()
        .filter_map(|carried| carried.pid)
        .collect::<Vec<_>>();
    pids.sort_unstable();
    pids.dedup();
    diag.emit(DiagEvent::PaneCarryRefuted {
        carried,
        pids,
        prior: prior_count,
        fresh: fresh_count,
        verified: verified_count,
        frames_ref: None,
    });
}

fn emit_carry_expired(diag: &crate::diag::DiagSink, outcome: &CarryOutcome) {
    for expired in &outcome.expired {
        diag.emit(DiagEvent::CarryForwardExpired {
            pane_id: expired.pane_id.clone(),
            pid: expired.pid,
            carried_ms: expired.carried_ms,
        });
    }
}

fn emit_hosted_carry_dropped(diag: &crate::diag::DiagSink, drops: Vec<HostedCarryDrop>) {
    for dropped in drops {
        diag.emit(DiagEvent::HostedCarryDropped {
            pane_id: dropped.pane_id,
            agent_kind: dropped.kind,
            reason: dropped.reason,
        });
    }
}

#[derive(Debug)]
enum Accepted {
    Fresh(PaneFrame),
    Held(PaneFrame),
}

fn accept(
    frame: PaneFrame,
    prior: Option<PaneFrame>,
    own_pane: Option<&PaneId>,
    diag: &crate::diag::DiagSink,
) -> Result<Accepted> {
    let now_ms = unix_now_ms();
    let prior = prior.and_then(|prior| publishable_cached_frame(prior, own_pane, diag));
    match frame_publish_verdict(&frame, own_pane) {
        PublishVerdict::Publish => {
            emit_pane_count_drop(diag, prior.as_ref(), &frame, now_ms);
            Ok(Accepted::Fresh(frame))
        }
        PublishVerdict::Reject(reason) => {
            emit_frame_rejected(diag, reason.clone(), prior.as_ref(), &frame, now_ms);
            match prior {
                Some(prior) => Ok(Accepted::Held(prior)),
                // The producer runs inside this pane; a first frame missing only that pane is still usable room truth.
                None if matches!(reason, FrameRejectReason::MissingOwnPane) => {
                    Ok(Accepted::Fresh(frame))
                }
                None => Err(crate::sidebar::produce::ProduceErr::FrameRejected(reason)),
            }
        }
    }
}

fn publishable_cached_frame(
    frame: PaneFrame,
    own_pane: Option<&PaneId>,
    diag: &crate::diag::DiagSink,
) -> Option<PaneFrame> {
    match frame_publish_verdict(&frame, own_pane) {
        PublishVerdict::Publish => Some(frame),
        PublishVerdict::Reject(reason) => {
            emit_frame_rejected(diag, reason, None, &frame, unix_now_ms());
            None
        }
    }
}

fn emit_mux_error(
    diag: &crate::diag::DiagSink,
    prior: Option<&PaneFrame>,
    err: &dyn std::fmt::Display,
) {
    diag.emit(DiagEvent::FrameRejected {
        reason: FrameRejectReason::MuxError {
            stderr_excerpt: excerpt(&err.to_string(), 512),
        },
        prior_pane_count: prior.map(pane_count).unwrap_or_default(),
        fresh_pane_count: 0,
        frames_ref: None,
    });
}

fn emit_frame_rejected(
    diag: &crate::diag::DiagSink,
    reason: FrameRejectReason,
    prior: Option<&PaneFrame>,
    fresh: &PaneFrame,
    at_ms: u64,
) {
    let frames_ref = if matches!(reason, FrameRejectReason::MissingOwnPane) {
        prior.and_then(|prior| diag.capture_frame_pair("frame_rejected", prior, fresh, at_ms))
    } else {
        None
    };
    diag.emit(DiagEvent::FrameRejected {
        reason,
        prior_pane_count: prior.map(pane_count).unwrap_or_default(),
        fresh_pane_count: pane_count(fresh),
        frames_ref,
    });
}

fn emit_pane_count_drop(
    diag: &crate::diag::DiagSink,
    prior: Option<&PaneFrame>,
    fresh: &PaneFrame,
    at_ms: u64,
) {
    let Some(prior) = prior else {
        return;
    };
    let prior_count = pane_count(prior);
    let fresh_count = pane_count(fresh);
    if fresh_count != 0 && prior_count.saturating_sub(fresh_count) < 2 {
        return;
    }
    let (removed, added) = pane_set_delta(prior, fresh);
    let evidence = Some(pane_drop_evidence(prior, fresh, &removed));
    let frames_ref = diag.capture_frame_pair("pane_count_drop", prior, fresh, at_ms);
    diag.emit(DiagEvent::PaneCountDrop {
        prior: prior_count,
        new: fresh_count,
        removed,
        added,
        evidence,
        frames_ref,
    });
}

fn pane_drop_evidence(
    prior: &PaneFrame,
    fresh: &PaneFrame,
    removed: &[PaneId],
) -> PaneDropEvidence {
    let removed = removed.iter().collect::<HashSet<_>>();
    let fresh_ids = fresh
        .pane_states()
        .map(|pane| &pane.pane_id)
        .collect::<HashSet<_>>();
    let mut affected_views = prior
        .tabs
        .iter()
        .filter_map(|tab| {
            let removed_panes = tab
                .panes
                .iter()
                .filter(|pane| removed.contains(&pane.pane_id))
                .collect::<Vec<_>>();
            if removed_panes.is_empty() {
                return None;
            }
            let managed_panes = removed_panes
                .iter()
                .filter_map(|pane| {
                    pane.current
                        .hosted_agent_kind
                        .clone()
                        .or_else(|| {
                            pane.current
                                .command
                                .as_deref()
                                .and_then(crate::agents::registry::command_agent_kind)
                                .map(AgentKind::new_unchecked)
                        })
                        .map(|agent_kind| ManagedPaneEvidence {
                            pane_id: pane.pane_id.clone(),
                            agent_kind,
                        })
                })
                .collect();
            Some(PaneDropViewEvidence {
                view_id: tab.view_id.to_string(),
                prior_panes: tab.panes.len(),
                remaining_panes: tab
                    .panes
                    .iter()
                    .filter(|pane| fresh_ids.contains(&pane.pane_id))
                    .count(),
                removed_pane_ids: removed_panes
                    .into_iter()
                    .map(|pane| pane.pane_id.clone())
                    .collect(),
                managed_panes,
            })
        })
        .collect::<Vec<_>>();
    affected_views.sort_by(|left, right| left.view_id.cmp(&right.view_id));
    let prior_panes = pane_count(prior);
    let fresh_panes = pane_count(fresh);
    PaneDropEvidence {
        prior_panes,
        fresh_panes,
        mass_shrink: prior_panes > 0 && fresh_panes.saturating_mul(2) < prior_panes,
        affected_views,
    }
}

fn pane_set_delta(prior: &PaneFrame, fresh: &PaneFrame) -> (Vec<PaneId>, Vec<PaneId>) {
    let prior_ids: HashSet<PaneId> = prior
        .pane_states()
        .map(|pane| pane.pane_id.clone())
        .collect();
    let fresh_ids: HashSet<PaneId> = fresh
        .pane_states()
        .map(|pane| pane.pane_id.clone())
        .collect();
    let mut removed = prior_ids
        .difference(&fresh_ids)
        .cloned()
        .collect::<Vec<_>>();
    let mut added = fresh_ids
        .difference(&prior_ids)
        .cloned()
        .collect::<Vec<_>>();
    removed.sort_by_key(|pane| pane.to_string());
    added.sort_by_key(|pane| pane.to_string());
    (removed, added)
}

fn excerpt(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn presence_sample_due(frame: &PaneFrame, probe_at_ms: Option<u64>, now_ms: u64) -> bool {
    let Some(sample) = frame.presence else {
        return false;
    };
    let sampled_or_probed_at_ms = probe_at_ms
        .map(|probe_at_ms| probe_at_ms.max(sample.sampled_at_ms))
        .unwrap_or(sample.sampled_at_ms);
    sample.last_input_ms.is_some()
        && sample.human_clients > 0
        && now_ms.saturating_sub(sampled_or_probed_at_ms) >= PRESENCE_SAMPLE_TTL.as_millis() as u64
}

fn presence_meaningfully_changed(prior: Option<&PresenceSample>, sample: &PresenceSample) -> bool {
    match prior {
        Some(prior) => {
            prior.last_input_ms != sample.last_input_ms
                || prior.human_clients != sample.human_clients
        }
        None => true,
    }
}

fn apply_presence_sample(frame: &mut PaneFrame, sample: PresenceSample) -> bool {
    let changed = presence_meaningfully_changed(frame.presence.as_ref(), &sample);
    frame.presence = Some(sample);
    changed
}

fn publish_frame(
    cache_path: &Path,
    frame: &mut PaneFrame,
    publication: PaneFramePublicationKind,
    own_pane: Option<&PaneId>,
    diag: &crate::diag::DiagSink,
) {
    stamp_publication(frame, publication);
    if let Err(err) = atomic::write_temp_then_rename_cache(cache_path, frame) {
        tracing::warn!(path = %cache_path.display(), error = %err, "sidebar snapshot cache write failed");
        return;
    }
    let pane_ids: HashSet<_> = frame
        .pane_states()
        .map(|pane| pane.pane_id.clone())
        .collect();
    diag.trace_focus(
        crate::diag::focus_trace::FocusTraceEvent::FramePublished {
            produced_at_ms: frame.produced_at_ms,
            observed_at_ms: frame.observed_at_ms,
            focused_pane: frame.focused_pane.clone(),
            focus_origin: frame.focus_origin,
            client_view_fresh: frame.client_view_fresh,
            client_sample_withheld: frame.client_sample_withheld,
            own_pane_listed: own_pane.map(|own| pane_ids.contains(own)),
            pane_count: pane_ids.len(),
            publication: match publication {
                PaneFramePublicationKind::Presence => "presence",
                PaneFramePublicationKind::Topology | PaneFramePublicationKind::Metrics => {
                    "topology"
                }
            }
            .to_owned(),
        },
        pane_ids,
    );
}

/// Which producer-owned input changed in a published pane frame.
#[derive(Clone, Copy)]
enum PaneFramePublicationKind {
    Topology,
    Metrics,
    Presence,
}

fn stamp_publication(frame: &mut PaneFrame, publication: PaneFramePublicationKind) {
    match publication {
        PaneFramePublicationKind::Topology => {
            let floor = frame
                .topology_stamp_ms
                .unwrap_or_default()
                .saturating_add(1);
            frame.topology_stamp_ms = Some(unix_now_ms().max(floor));
        }
        PaneFramePublicationKind::Metrics | PaneFramePublicationKind::Presence => {}
    }
}

#[cfg(test)]
mod tests;
