//! The live pane frame: the single-flight `list-panes` cache, the raced-read
//! process rotation, and the `/proc` process-start stamp — everything the
//! producer publishes to `snapshot.json` for consumers to fold in process.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::Result;
use crate::ids::{AgentSessionId, MuxName, PaneId};
use crate::ledger::atomic;
use crate::ledger::single_flight::{self, Coalesced};
use crate::ledger::snapshot::SidebarPresence;
use crate::mux::{ClientFocusOptions, PaneListOptions, PaneListing};
use crate::schema::diag::{DiagEvent, FrameRejectReason};
use crate::sidebar::cache::{
    SNAPSHOT_CACHE_TTL, effective_pane_ttl, presence_stamp_age_ms, read_snapshot_cache,
    snapshot_cache_is_fresh, unix_now_ms,
};
use crate::sidebar::frame::{FrameInputs, PaneFrame, PaneMetrics};

mod carry;
mod starts;
mod validate;

use carry::{CarryOutcome, apply_carry_forward};
use starts::{drop_reused_pid_bindings, stamp_pane_process_starts};
use validate::{PublishVerdict, frame_publish_verdict, pane_count, shrink_needs_verification};

/// How a non-producing sidebar waits for the single producer's cache write
/// before giving up and producing locally. ~200ms total (10 × 20ms).
const SNAPSHOT_CACHE_WAIT_STEP: Duration = Duration::from_millis(20);
const SNAPSHOT_CACHE_WAIT_STEPS: u32 = 10;

/// Return a same-session cache entry younger than `ttl`, or `None` when it is
/// absent, stale, for another session, or unreadable. The caller picks the TTL
/// once per produce ([`effective_pane_ttl`]) — `SNAPSHOT_CACHE_TTL` in poll
/// mode, the stretched event-mode TTL while the presence stamp is fresh — and
/// the freshness verdict itself is the library's
/// ([`snapshot_cache_is_fresh`]), so the forced-freshness floor keeps
/// overriding in both modes.
fn fresh_snapshot_cache(
    cache_path: &Path,
    session: &str,
    min_produced_at_ms: Option<u64>,
    ttl: Duration,
) -> Option<PaneFrame> {
    let cache = read_snapshot_cache(cache_path, session)?;
    let ttl = if cache.carried_panes.is_empty() {
        ttl
    } else {
        std::cmp::min(ttl, SNAPSHOT_CACHE_TTL)
    };
    snapshot_cache_is_fresh(&cache, unix_now_ms(), min_produced_at_ms, ttl).then_some(cache)
}

/// The session's live panes from the mux — the `list-panes` round-trip the
/// snapshot cache amortizes across the fleet. The ledger rollup is read
/// separately (fresh from `latest.json`), so this enumerates only the pane set.
/// The per-view `is_focused` mark rides the pane list itself for the sidebar's
/// selection baseline. The elected producer also samples the attached clients'
/// viewed panes once per tick so focus-clearing unread is gated on the tab the
/// user is actually viewing.
fn list_session_panes(
    mux: MuxName,
    session: &str,
    workspace_id: crate::WorkspaceId,
    min_topology_produced_at_ms: Option<u64>,
    command_timeout: Option<Duration>,
) -> Result<PaneListing> {
    Ok(crate::mux::backend_for(mux).list_panes(PaneListOptions {
        session_name: Some(session.to_owned()),
        workspace_id: Some(workspace_id),
        min_topology_produced_at_ms,
        command_timeout,
    })?)
}

/// Join a fresh frame to the last published same-session frame. Raced-null
/// fields repair only when the process identity stayed stable; a command or
/// root-pid change rotates the prior current process to `previous` and keeps
/// the fresh process record clean.
fn rotate_from_cache(frame: &mut PaneFrame, cache_path: &Path, session: &str) {
    if let Some(prev) = read_snapshot_cache(cache_path, session) {
        frame.rotate_against_prior(&prev);
    }
}

/// The pane ids a fresh `list-panes` read left without a process start — the
/// set the `/proc` stamp owns ([`stamp_pane_process_starts`]). Captured before
/// the frame rotates against the prior publish, so a backend-reported start is
/// never confused with Rimz's own derived stamp and never overwritten by one.
fn natively_unstamped(frame: &PaneFrame) -> HashSet<PaneId> {
    frame
        .pane_states()
        .filter(|pane| pane.current.started_at.is_none())
        .map(|pane| pane.pane_id.clone())
        .collect()
}

fn stamp_pane_resumed_session_ids(
    frame: &mut PaneFrame,
    root_resume: &dyn Fn(u32) -> Option<AgentSessionId>,
) {
    for pane in frame.pane_states_mut() {
        if pane.current.resumed_session_id.is_some() {
            continue;
        }
        if pane
            .current
            .command
            .as_deref()
            .and_then(crate::ledger::snapshot::command_agent_kind)
            != Some("codex")
        {
            continue;
        }
        if let Some(resumed) = pane.current.pid.and_then(root_resume) {
            pane.current.resumed_session_id = Some(resumed);
        }
    }
}

fn repair_pane_frame(
    frame: &mut PaneFrame,
    runtime: &crate::RuntimePaths,
    cache_path: &Path,
    session: &str,
    enrich_metrics: bool,
) {
    let unstamped = natively_unstamped(frame);
    rotate_from_cache(frame, cache_path, session);
    if enrich_metrics {
        super::metrics::enrich_pane_metrics(frame, session, runtime);
    } else {
        super::metrics::backfill_zellij_pane_pids_from_proc(frame, session);
    }
    backfill_wrapper_spawn_commands(frame, &crate::proc::cmdline);
    drop_finished_active_commands(
        frame,
        &crate::proc::cmdline,
        &crate::proc::comm,
        &crate::proc::children,
    );
    backfill_pane_cwds(frame, &|pid| crate::proc::cwd(pid));
    stamp_pane_resumed_session_ids(
        frame,
        &crate::remote_control::codex_resumed_session_id_for_root,
    );
    stamp_pane_process_starts(
        frame,
        &unstamped,
        &crate::remote_control::in_pane_agent_start_for_root,
        &crate::remote_control::in_pane_agent_starts,
    );
    if crate::proc::process_start(std::process::id()).is_some() {
        drop_reused_pid_bindings(
            frame,
            &crate::remote_control::in_pane_agent_start_for_root,
            &crate::proc::process_start,
        );
    }
    annotate_elevated_agents(frame, &crate::remote_control::elevated_in_pane_agent);
    stamp_first_seen(frame);
}

/// Recover a pane's spawn command from Rimz's supervised agent wrapper when the
/// mux did not retain one. tmux reports only the foreground program basename
/// and no spawn command, so a `rimz agents exec <kind>` wrapper pane otherwise
/// classifies as neither its agent kind nor its worktree. Only an empty spawn
/// command is filled, and only from a cmdline that classifies as an agent, so a
/// real foreground is never overwritten.
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
            .filter(|cmdline| crate::ledger::snapshot::command_agent_kind(cmdline).is_some())
        {
            pane.current.spawn_command = Some(cmdline);
        }
    }
}

fn stamp_first_seen(frame: &mut PaneFrame) {
    let observed_at_ms = frame.observed_or_produced_at_ms();
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
    let cache_path = runtime.root.join("snapshot.json");
    let listing = match super::pane_list_fixture()? {
        Some(fixture) => PaneListing {
            panes: fixture,
            observed_at_ms: unix_now_ms(),
            source_active: std::collections::BTreeMap::new(),
            source_active_authoritative: false,
            served_from_topology: false,
        },
        None => list_session_panes(
            mux,
            session,
            runtime.workspace_id.clone(),
            None,
            Some(command_timeout),
        )?,
    };
    let (mut frame, diagnostics) = crate::sidebar::frame::assemble_frame_from_inputs(FrameInputs {
        panes: listing.panes,
        produced_at_ms: unix_now_ms(),
        observed_at_ms: listing.observed_at_ms,
        session_name: session.to_owned(),
        source_active: listing.source_active,
        source_active_authoritative: listing.source_active_authoritative,
        prior: read_snapshot_cache(&cache_path, session).as_ref(),
    });
    let diag = crate::diag::DiagSink::for_workspace(
        runtime.workspace_id.clone(),
        session.to_owned(),
        None,
    );
    emit_frame_diagnostics(diag.as_ref(), diagnostics);
    repair_pane_frame(&mut frame, runtime, &cache_path, session, false);
    Ok(frame)
}

/// Fill a pane's raced-empty cwd from `/proc/<pane_pid>/cwd` once the root pid
/// is known. A fresh `list-panes` can answer a just-born pane with an empty cwd
/// for a tick; without one the pane groups under `external` and flickers there
/// until the mux reports the path. Only an empty cwd is ever filled — a
/// mux-reported cwd is authoritative because it tracks OSC7/foreground chdir,
/// which can diverge from the root's `/proc` cwd. A `/proc` cwd that no longer
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

/// Clear an active foreground command when the pane's root process tree no
/// longer contains that command. Zellij's presence topology is a latency cache:
/// it can retain a finished `CommandChanged` foreground after the shell has
/// returned to idle. The mux-repaired root pid is live truth; if neither the
/// root nor any descendant still matches the active command, the command is
/// stale and the pane should fold as its idle shell.
fn drop_finished_active_commands(
    frame: &mut PaneFrame,
    proc_cmdline: &dyn Fn(u32) -> Option<String>,
    proc_comm: &dyn Fn(u32) -> Option<String>,
    proc_children: &dyn Fn(u32) -> Vec<u32>,
) {
    for pane in frame.pane_states_mut() {
        let Some(command) = pane.current.command.as_deref() else {
            continue;
        };
        if !crate::ledger::snapshot::process_is_active(command) {
            continue;
        }
        let Some(root_pid) = pane.current.pid else {
            continue;
        };
        let match_mode = ProcessCommandMatch::for_mux(pane.pane_id.mux());
        if process_tree_command_status(
            root_pid,
            command,
            match_mode,
            proc_cmdline,
            proc_comm,
            proc_children,
        ) == ProcessCommandStatus::Absent
        {
            pane.current.command = None;
            pane.metrics = PaneMetrics::default();
            pane.children.clear();
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
    /// against the live process program label/comm, not the full argv.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProcessCommandStatus {
    Present,
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
        ProcessCommandProbe::Match => return ProcessCommandStatus::Present,
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
            ProcessCommandProbe::Match => return ProcessCommandStatus::Present,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProcessCommandProbe {
    Match,
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
    let command_label = crate::ledger::snapshot::program_label(command);
    let mut saw_cmdline_mismatch = false;
    match proc_cmdline(pid).map(|cmdline| cmdline.trim().to_owned()) {
        Some(cmdline) if !cmdline.is_empty() => match match_mode {
            ProcessCommandMatch::ExactCmdline if cmdline == command => {
                return ProcessCommandProbe::Match;
            }
            ProcessCommandMatch::ExactCmdline => return ProcessCommandProbe::Mismatch,
            ProcessCommandMatch::ProgramLabel
                if crate::ledger::snapshot::program_label(&cmdline) == command_label =>
            {
                return ProcessCommandProbe::Match;
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
            Some(comm) if comm == command_label => ProcessCommandProbe::Match,
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
        if !crate::remote_control::command_starts_with_elevation_wrapper(command) {
            continue;
        }
        if let Some(pid) = pane.current.pid {
            pane.current.elevated_agent = elevated_agent(pid);
        }
    }
}

/// Return the live pane frame for `session` — the pane list plus the pane-source
/// observation stamp that event fusion orders against — sharing one `list-panes`
/// round-trip across every sidebar via a short-lived single-flight cache.
///
/// Fast path: a fresh same-session cache is read back with no mux work. Slow
/// path: a non-blocking `try_lock` elects one producer; losers poll briefly for
/// its write, then fall back to producing locally so a wedged producer never
/// strands them.
pub(super) fn cached_panes_or_produce(
    runtime: &crate::RuntimePaths,
    mux: MuxName,
    session: &str,
    min_pane_cache_ms: Option<u64>,
    own_pane: Option<&PaneId>,
    diag: Option<&crate::diag::DiagSink>,
) -> Result<PaneFrame> {
    let cache_path = runtime.root.join("snapshot.json");

    // Select the pane TTL once per call from the presence stamp: event mode
    // (EVENT_PANE_TTL) while a presence push channel is alive, else poll-mode
    // SNAPSHOT_CACHE_TTL. Zellij's plugin and tmux's control-mode watch both
    // write the stamp; tmux lapses to poll mode while the watch is absent or
    // idle. One small stamp read per produce; the fast path, the single-flight
    // `fresh` closure, and the loser re-check all read this one Duration, so a
    // loser never produces what the winner skipped.
    let pane_ttl = effective_pane_ttl(presence_stamp_age_ms(runtime));

    // One single-flight lock covers both arms: the slow path's full produce
    // and the fast path's metrics-only refresh, so only one elected producer
    // ever writes the shared caches.
    let lock_path = runtime.root.join("snapshot.lock");

    // Fast path: a fresh same-session entry needs no mux work. Metrics still
    // have their own cadence, so refresh them from the cached topology when
    // due instead of waiting for the pane cache to expire.
    if let Some(cache) = fresh_publishable_snapshot_cache(
        &cache_path,
        session,
        min_pane_cache_ms,
        pane_ttl,
        own_pane,
        diag,
    ) {
        return Ok(refresh_cached_metrics(
            cache,
            runtime,
            &cache_path,
            &lock_path,
            session,
            min_pane_cache_ms,
            pane_ttl,
            own_pane,
            diag,
        ));
    }

    // Slow path: elect one producer for this `(workspace, session)` refresh.
    // Losers read its write back; if it wedges, they fall back to an uncached
    // local produce rather than block.
    let fresh = || {
        fresh_publishable_snapshot_cache(
            &cache_path,
            session,
            min_pane_cache_ms,
            pane_ttl,
            own_pane,
            diag,
        )
    };
    let produce_candidate =
        |enrich_metrics: bool, min_topology_produced_at_ms: Option<u64>| -> Result<PaneFrame> {
            let listing = match list_session_panes(
                mux,
                session,
                runtime.workspace_id.clone(),
                min_topology_produced_at_ms,
                None,
            ) {
                Ok(panes) => panes,
                Err(err) => {
                    emit_mux_error(diag, &cache_path, session, &err);
                    return Err(err);
                }
            };
            let served_from_topology = listing.served_from_topology;
            let source_active_authoritative = listing.source_active_authoritative;
            let panes = filter_foreign_session_panes(listing.panes, session, diag);
            let (mut frame, diagnostics) =
                crate::sidebar::frame::assemble_frame_from_inputs(FrameInputs {
                    panes,
                    produced_at_ms: unix_now_ms(),
                    observed_at_ms: listing.observed_at_ms,
                    session_name: session.to_owned(),
                    source_active: listing.source_active,
                    source_active_authoritative,
                    prior: read_snapshot_cache(&cache_path, session).as_ref(),
                });
            emit_frame_diagnostics(diag, diagnostics);
            // Sample attached-client focus and presence only on a live read. A
            // topology-served frame forks no mux command (the topology cache's
            // contract), so carry the prior publish's client view forward as the
            // freshest non-forking estimate rather than reach for `list-clients`.
            if served_from_topology {
                if let Some(prior) = read_snapshot_cache(&cache_path, session) {
                    frame.viewed_panes = prior.viewed_panes;
                    frame.presence = prior.presence;
                }
            } else {
                match crate::mux::backend_for(mux).client_view(ClientFocusOptions {
                    session_name: Some(session.to_owned()),
                    ..Default::default()
                }) {
                    Ok(client_view) => {
                        frame.viewed_panes = client_view.viewed_panes;
                        frame.presence = Some(SidebarPresence::classify(
                            unix_now_ms(),
                            client_view.presence.human_clients,
                            client_view.presence.last_input_ms,
                        ));
                    }
                    Err(_) => {
                        frame.viewed_panes = Vec::new();
                        frame.presence = None;
                    }
                }
            }
            repair_pane_frame(&mut frame, runtime, &cache_path, session, enrich_metrics);
            Ok(frame)
        };
    match single_flight::coalesce(
        &lock_path,
        SNAPSHOT_CACHE_WAIT_STEP,
        SNAPSHOT_CACHE_WAIT_STEPS,
        fresh,
    ) {
        Coalesced::Shared(cache) => Ok(cache),
        // The producer wedged past the wait: produce locally rather than block.
        // The raced-read repair still applies — without it a dropped command/cwd
        // on this one path folds the anonymous row the winner path guards against.
        Coalesced::ProduceLocal => {
            let prior = read_snapshot_cache(&cache_path, session);
            let frame = produce_candidate(false, min_pane_cache_ms)?;
            let frame = confirm_and_carry(
                frame,
                prior.as_ref(),
                own_pane,
                &produce_candidate,
                diag,
                false,
                runtime,
            )?;
            validate_frame_for_publish(frame, prior, own_pane, diag, false, runtime, &cache_path)
        }
        // We won: fork `list-panes` and publish it. The guard holds the lock
        // until this arm returns.
        Coalesced::Produce(_guard) => {
            let prior = read_snapshot_cache(&cache_path, session);
            let frame = produce_candidate(true, min_pane_cache_ms)?;
            // A mid-tick `list-panes` race can drop a live pane's command/cwd/
            // process-start; rather than fold an anonymous `external`/`process`
            // row that blinks out next tick, run the shared repaired-frame
            // ladder before publishing.
            let frame = confirm_and_carry(
                frame,
                prior.as_ref(),
                own_pane,
                &produce_candidate,
                diag,
                true,
                runtime,
            )?;
            validate_frame_for_publish(frame, prior, own_pane, diag, true, runtime, &cache_path)
        }
    }
}

fn filter_foreign_session_panes(
    panes: Vec<crate::pane::PaneRef>,
    session: &str,
    diag: Option<&crate::diag::DiagSink>,
) -> Vec<crate::pane::PaneRef> {
    panes
        .into_iter()
        .filter_map(|pane| {
            if pane.session_name == session {
                Some(pane)
            } else {
                if let Some(diag) = diag {
                    diag.emit(DiagEvent::ForeignSessionPane {
                        pane_id: pane.pane_id,
                        session: pane.session_name,
                    });
                }
                None
            }
        })
        .collect()
}

fn emit_frame_diagnostics(diag: Option<&crate::diag::DiagSink>, events: Vec<DiagEvent>) {
    if let Some(diag) = diag {
        for event in events {
            diag.emit(event);
        }
    }
}

fn confirm_and_carry(
    frame: PaneFrame,
    prior: Option<&PaneFrame>,
    own_pane: Option<&PaneId>,
    produce_candidate: &dyn Fn(bool, Option<u64>) -> Result<PaneFrame>,
    diag: Option<&crate::diag::DiagSink>,
    enrich_metrics: bool,
    runtime: &crate::RuntimePaths,
) -> Result<PaneFrame> {
    let bindings = super::metrics::pane_root_bindings(runtime);
    let initial_now_ms = unix_now_ms();
    let initial_frame = frame.clone();
    let initial = apply_carry_forward(
        frame,
        prior,
        own_pane,
        &bindings,
        &read_start_ticks,
        initial_now_ms,
    );
    let needs_confirm =
        !initial.carried.is_empty() || shrink_needs_verification(&initial.frame, prior);
    if !needs_confirm {
        emit_carry_expired(diag, &initial);
        return Ok(initial.frame);
    }

    let prior_count = prior.map(pane_count).unwrap_or_default();
    let confirm_floor = Some(unix_now_ms());
    let verified = produce_candidate(enrich_metrics, confirm_floor)?;
    let verified_count = pane_count(&verified);
    let confirmed_at_ms = unix_now_ms();
    let verified_frame = verified.clone();
    let confirmed = apply_carry_forward(
        verified,
        prior,
        own_pane,
        &bindings,
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
            && let Some(diag) = diag
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
            &verified_frame,
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
    diag: Option<&crate::diag::DiagSink>,
    prior: Option<&PaneFrame>,
    offending_frame: &PaneFrame,
    outcome: &CarryOutcome,
    prior_count: usize,
    fresh_count: usize,
    at_ms: u64,
) {
    let Some(diag) = diag else {
        return;
    };
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
        cli_confirmed: true,
        frames_ref,
    });
}

fn emit_pane_carry_refuted(
    diag: Option<&crate::diag::DiagSink>,
    outcome: &CarryOutcome,
    prior_count: usize,
    fresh_count: usize,
    verified_count: usize,
) {
    let Some(diag) = diag else {
        return;
    };
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

fn emit_carry_expired(diag: Option<&crate::diag::DiagSink>, outcome: &CarryOutcome) {
    let Some(diag) = diag else {
        return;
    };
    for expired in &outcome.expired {
        diag.emit(DiagEvent::CarryForwardExpired {
            pane_id: expired.pane_id.clone(),
            pid: expired.pid,
            carried_ms: expired.carried_ms,
        });
    }
}

fn validate_frame_for_publish(
    frame: PaneFrame,
    prior: Option<PaneFrame>,
    own_pane: Option<&PaneId>,
    diag: Option<&crate::diag::DiagSink>,
    publish: bool,
    runtime: &crate::RuntimePaths,
    cache_path: &Path,
) -> Result<PaneFrame> {
    let now_ms = unix_now_ms();
    let prior = prior.and_then(|prior| publishable_prior(prior, own_pane, diag));
    emit_mixed_build_writers(diag, prior.as_ref());
    match frame_publish_verdict(&frame, prior.as_ref(), own_pane, now_ms) {
        PublishVerdict::Publish => {
            if publish {
                emit_pane_count_drop(diag, prior.as_ref(), &frame, now_ms);
                publish_frame(runtime, cache_path, &frame);
            }
            Ok(frame)
        }
        PublishVerdict::Escape { held_ms } => {
            if let Some(diag) = diag {
                diag.emit(DiagEvent::FrameRejectEscape { held_ms });
            }
            if publish {
                emit_pane_count_drop(diag, prior.as_ref(), &frame, now_ms);
                publish_frame(runtime, cache_path, &frame);
            }
            Ok(frame)
        }
        PublishVerdict::Reject(reason) => {
            emit_frame_rejected(diag, reason.clone(), prior.as_ref(), &frame, now_ms);
            prior.ok_or(crate::sidebar::produce::ProduceErr::FrameRejected(reason))
        }
    }
}

fn fresh_publishable_snapshot_cache(
    cache_path: &Path,
    session: &str,
    min_produced_at_ms: Option<u64>,
    ttl: Duration,
    own_pane: Option<&PaneId>,
    diag: Option<&crate::diag::DiagSink>,
) -> Option<PaneFrame> {
    let cache = fresh_snapshot_cache(cache_path, session, min_produced_at_ms, ttl)?;
    publishable_cached_frame(cache, own_pane, diag)
}

fn publishable_prior(
    frame: PaneFrame,
    own_pane: Option<&PaneId>,
    diag: Option<&crate::diag::DiagSink>,
) -> Option<PaneFrame> {
    publishable_cached_frame(frame, own_pane, diag)
}

/// A prior frame assembled by a different build means two rimz versions are
/// writing this workspace's snapshot — the upgrade-overlap window where stale
/// producers cause the subtlest regressions. Info evidence, rate-limited per
/// build pair; legacy frames without a stamp stay silent.
fn emit_mixed_build_writers(diag: Option<&crate::diag::DiagSink>, prior: Option<&PaneFrame>) {
    let (Some(diag), Some(prior)) = (diag, prior) else {
        return;
    };
    let (Some(prior_build), Some(own_build)) = (prior.build.as_deref(), crate::build_id::current())
    else {
        return;
    };
    if prior_build == own_build {
        return;
    }
    diag.emit(DiagEvent::MixedBuildWriters {
        prior_build: prior_build.to_owned(),
        own_build: own_build.to_owned(),
    });
}

fn publishable_cached_frame(
    frame: PaneFrame,
    own_pane: Option<&PaneId>,
    diag: Option<&crate::diag::DiagSink>,
) -> Option<PaneFrame> {
    match frame_publish_verdict(&frame, None, own_pane, unix_now_ms()) {
        PublishVerdict::Publish => Some(frame),
        PublishVerdict::Escape { .. } => Some(frame),
        PublishVerdict::Reject(reason) => {
            emit_frame_rejected(diag, reason, None, &frame, unix_now_ms());
            None
        }
    }
}

fn emit_mux_error(
    diag: Option<&crate::diag::DiagSink>,
    cache_path: &Path,
    session: &str,
    err: &dyn std::fmt::Display,
) {
    let Some(diag) = diag else {
        return;
    };
    let prior = read_snapshot_cache(cache_path, session);
    diag.emit(DiagEvent::FrameRejected {
        reason: FrameRejectReason::MuxError {
            stderr_excerpt: excerpt(&err.to_string(), 512),
        },
        prior_pane_count: prior.as_ref().map(pane_count).unwrap_or_default(),
        fresh_pane_count: 0,
        frames_ref: None,
    });
}

fn emit_frame_rejected(
    diag: Option<&crate::diag::DiagSink>,
    reason: FrameRejectReason,
    prior: Option<&PaneFrame>,
    fresh: &PaneFrame,
    at_ms: u64,
) {
    if let Some(diag) = diag {
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
}

fn emit_pane_count_drop(
    diag: Option<&crate::diag::DiagSink>,
    prior: Option<&PaneFrame>,
    fresh: &PaneFrame,
    at_ms: u64,
) {
    let Some(diag) = diag else {
        return;
    };
    let Some(prior) = prior else {
        return;
    };
    let prior_count = pane_count(prior);
    let fresh_count = pane_count(fresh);
    if fresh_count != 0 && prior_count.saturating_sub(fresh_count) < 2 {
        return;
    }
    let (removed, added) = pane_set_delta(prior, fresh);
    let frames_ref = diag.capture_frame_pair("pane_count_drop", prior, fresh, at_ms);
    diag.emit(DiagEvent::PaneCountDrop {
        prior: prior_count,
        new: fresh_count,
        removed,
        added,
        frames_ref,
    });
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

/// The fast path's metrics arm: re-sample `/proc` over a topology-fresh cached
/// frame when some pane's sample is due, and republish. The publish keeps the
/// frame's `produced_at_ms`, so a metrics-only refresh never masquerades as a
/// fresh pane listing; election rides the same snapshot lock as the full
/// produce, so one process samples per window and a loser serves the shared
/// write back.
#[allow(clippy::too_many_arguments)]
fn refresh_cached_metrics(
    frame: PaneFrame,
    runtime: &crate::RuntimePaths,
    cache_path: &Path,
    lock_path: &Path,
    session: &str,
    min_pane_cache_ms: Option<u64>,
    pane_ttl: Duration,
    own_pane: Option<&PaneId>,
    diag: Option<&crate::diag::DiagSink>,
) -> PaneFrame {
    if !super::metrics::pane_metrics_due(&frame, runtime) {
        return frame;
    }
    let fresh = || {
        let cache = fresh_publishable_snapshot_cache(
            cache_path,
            session,
            min_pane_cache_ms,
            pane_ttl,
            own_pane,
            diag,
        )?;
        (!super::metrics::pane_metrics_due(&cache, runtime)).then_some(cache)
    };
    match single_flight::coalesce(
        lock_path,
        SNAPSHOT_CACHE_WAIT_STEP,
        SNAPSHOT_CACHE_WAIT_STEPS,
        fresh,
    ) {
        Coalesced::Shared(cache) => cache,
        // A wedged producer must not block the visible tab. Keep rendering the
        // cached frame rather than writing shared metrics state outside the
        // elected producer path.
        Coalesced::ProduceLocal => frame,
        Coalesced::Produce(_guard) => {
            let mut latest = fresh_publishable_snapshot_cache(
                cache_path,
                session,
                min_pane_cache_ms,
                pane_ttl,
                own_pane,
                diag,
            )
            .unwrap_or(frame);
            if super::metrics::enrich_pane_metrics(&mut latest, session, runtime) {
                annotate_elevated_agents(
                    &mut latest,
                    &crate::remote_control::elevated_in_pane_agent,
                );
                publish_frame(runtime, cache_path, &latest);
            }
            latest
        }
    }
}

fn publish_frame(runtime: &crate::RuntimePaths, cache_path: &Path, frame: &PaneFrame) {
    if let Err(err) = atomic::write_temp_then_rename_cache(cache_path, frame) {
        tracing::warn!(path = %cache_path.display(), error = %err, "sidebar snapshot cache write failed");
    } else if let Err(err) = crate::ledger::wakeup::wake_sidebars_pane_frame_published(runtime) {
        tracing::debug!(error = %err, "sidebar pane-frame publication wakeup failed");
    }
}

#[cfg(test)]
mod tests;
