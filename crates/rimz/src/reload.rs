//! `rimz reload` — pick up a freshly-installed build, converge every running
//! sidebar to a healthy set, and refresh held stats dashboards.
//!
//! User-scoped and cwd-independent: it enumerates every known workspace
//! ([`crate::workspace::known_workspaces`]), finds which have a live mux session,
//! and reconciles live sessions concurrently in place — one live sidebar per
//! working view, running the current binary — closing duplicate or unresponsive
//! sidebar panes and reaping orphaned sidebar processes whose pane is gone. Held
//! `rimz stats --refresh` dashboards are signalled to re-exec in place before
//! workspace enumeration, so
//! standalone dashboards reload even when no rooms exist. `rimz reload` is the
//! convergence path for moving long-lived sidebars and stats dashboards onto a
//! freshly-installed build. A workspace whose session is gone has its stale
//! runtime files and leftover daemons swept. Every step is best-effort: a hiccup
//! on one workspace is logged and never blocks the rest.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::config::{MachineConfig, MultiplexerConfig};
use crate::ids::{MuxName, PaneId};
use crate::mux::recovery;
use crate::mux::{
    MuxBackend, PaneListOptions, SidebarLiveness, SidebarPaneOptions, SidebarWidth, backend_for,
};
use crate::sidebar::heartbeat::SidebarHeartbeat;
use crate::sidebar::timing::{
    RECONCILE_LIST_TIMEOUT, RELOAD_CONVERGE_POLL, RELOAD_CONVERGE_TIMEOUT, unix_now_ms,
};
use crate::store::{RuntimePaths, StatePaths, wakeup, workspace_record};
use crate::workspace::{self, KnownWorkspace};

/// Resolve the on-disk binary that should be executed after the current image
/// may have been replaced by an atomic install.
pub fn current_reexec_target() -> Option<PathBuf> {
    resolve_reexec_target(std::env::current_exe().ok()?)
}

/// Pick the live binary behind a `current_exe()` reading.
///
/// A fresh `cargo install` replaces our binary via atomic rename, which unlinks
/// the inode the running process still holds. The kernel then annotates
/// `/proc/self/exe` (what `current_exe()` reads) with a trailing " (deleted)",
/// so the raw path no longer resolves on disk. The replacement now lives at the
/// un-annotated path, so strip that marker and prefer whichever path is a real
/// file. `None` means neither path exists, such as during a partial install.
pub fn resolve_reexec_target(exe: PathBuf) -> Option<PathBuf> {
    crate::proc::resolve_existing_or_replacement(&exe)
}

/// What a user-wide reload did, aggregated across workspaces, for the CLI report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReloadOutcome {
    /// Live sessions reconciled.
    pub sessions: usize,
    /// Zellij sessions whose presence channel did not publish after converge.
    pub presence_dead: usize,
    /// Located sidebars already publishing the on-disk build before reload.
    pub already_current: usize,
    /// Located sidebars that published the on-disk build after the reload signal.
    pub reexeced: usize,
    /// Sidebars closed and re-added because they did not reload in place.
    pub restarted: usize,
    /// Live sidebars whose build could not be verified.
    pub unverified: usize,
    /// Sidebars added (a view had none, or its only one was unresponsive).
    pub recovered: usize,
    /// Duplicate or unresponsive sidebar panes closed.
    pub closed: usize,
    /// Orphaned sidebar processes (pane gone) reaped.
    pub reaped: usize,
    /// Leftover processes swept from workspaces whose session is gone.
    pub dead_swept: usize,
    /// `rimz stats --refresh` dashboards signalled to re-exec onto the new build.
    pub stats_reloaded: usize,
    /// Views whose sidebar add or repair could not complete this pass.
    pub failed: usize,
    /// Views whose in-place add or geometry repair was deferred (no attached
    /// client).
    pub deferred: usize,
    /// Kept sidebar panes whose geometry was repaired in place.
    pub redocked: usize,
    /// Working sidebar panes that remain outside the verified full-height left
    /// dock after the bounded repair path.
    pub misdocked: usize,
}

impl ReloadOutcome {
    fn merge(&mut self, delta: Self) {
        let Self {
            sessions,
            presence_dead,
            already_current,
            reexeced,
            restarted,
            unverified,
            recovered,
            closed,
            reaped,
            dead_swept,
            stats_reloaded,
            failed,
            deferred,
            redocked,
            misdocked,
        } = delta;
        self.sessions += sessions;
        self.presence_dead += presence_dead;
        self.already_current += already_current;
        self.reexeced += reexeced;
        self.restarted += restarted;
        self.unverified += unverified;
        self.recovered += recovered;
        self.closed += closed;
        self.reaped += reaped;
        self.dead_swept += dead_swept;
        self.stats_reloaded += stats_reloaded;
        self.failed += failed;
        self.deferred += deferred;
        self.redocked += redocked;
        self.misdocked += misdocked;
    }
}

/// Reload and reconcile every running sidebar across all of this user's
/// workspaces. Returns the aggregate outcome.
pub fn reload_user_sidebars() -> ReloadOutcome {
    let mut outcome = ReloadOutcome {
        stats_reloaded: recovery::reload_stats_dashboards().len(),
        ..ReloadOutcome::default()
    };
    let workspaces = match workspace::known_workspaces() {
        Ok(workspaces) => workspaces,
        Err(err) => {
            tracing::warn!(
                tags.operation = "reload.enumerate_workspaces",
                error = &err as &dyn std::error::Error,
                "reload: cannot enumerate workspaces",
            );
            return outcome;
        }
    };
    if workspaces.is_empty() {
        return outcome;
    }

    let rimz_bin = current_reexec_target().unwrap_or_else(crate::proc::rimz_exe);
    let machine_config = MachineConfig::load_lenient();
    let live = LiveSessions::probe();
    let mut reconciled_sessions: HashSet<(MuxName, String)> = HashSet::new();
    let mut live_targets = Vec::new();

    for ws in workspaces {
        match live.mux_of(&ws.session_name) {
            Some(mux) => {
                if !claim_live_session(&mut reconciled_sessions, mux, &ws.session_name) {
                    tracing::debug!(
                        session = %ws.session_name,
                        workspace = %ws.workspace_id,
                        "reload: skipping duplicate workspace record for an already-reconciled session",
                    );
                    continue;
                }
                let runtime = match RuntimePaths::for_workspace(ws.workspace_id.clone()) {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        tracing::warn!(
                            workspace = %ws.workspace_id,
                            tags.operation = "reload.runtime_paths",
                            error = &err as &dyn std::error::Error,
                            "reload: runtime paths",
                        );
                        continue;
                    }
                };
                live_targets.push((mux, ws, runtime));
            }
            None => {
                let runtime = match RuntimePaths::for_workspace(ws.workspace_id.clone()) {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        tracing::warn!(
                            workspace = %ws.workspace_id,
                            tags.operation = "reload.runtime_paths",
                            error = &err as &dyn std::error::Error,
                            "reload: runtime paths",
                        );
                        continue;
                    }
                };
                // No live mux session lists this workspace. Drop its stale
                // heartbeat/socket files and reap any leftover sidebar/app-server
                // daemon — but never a mux server: "dead" here is inferred from a
                // best-effort probe, so a probe that misread a live session as
                // gone must not be able to tear that session down. Only
                // respawnable daemons are swept (`include_mux_server: false`).
                crate::sidebar::sweep_orphan_runtime(&runtime);
                let swept = recovery::sweep_orphan_processes(
                    ws.workspace_id.as_str(),
                    &ws.session_name,
                    false,
                );
                outcome.dead_swept += swept.len();
            }
        }
    }

    // Claimed sessions are independent: each target owns its mux server
    // round-trips and filters heartbeats by `(mux, session_name)`. Shared
    // workspace wakeup fanout and orphan-runtime sweeps are idempotent,
    // best-effort, and tolerate races.
    std::thread::scope(|scope| {
        let handles: Vec<_> = live_targets
            .iter()
            .map(|(mux, ws, runtime)| {
                let rimz_bin = &rimz_bin;
                let machine_config = &machine_config;
                scope.spawn(move || reconcile_live(*mux, ws, runtime, rimz_bin, machine_config))
            })
            .collect();
        for handle in handles {
            match handle.join() {
                Ok(delta) => outcome.merge(delta),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
    });

    outcome
}

fn claim_live_session(
    seen: &mut HashSet<(MuxName, String)>,
    mux: MuxName,
    session_name: &str,
) -> bool {
    seen.insert((mux, session_name.to_owned()))
}

/// Converge one live session: re-exec its live sidebars onto the current binary,
/// reconcile panes to one live sidebar per working view, then reap orphan
/// sidebar processes the mux could not close.
fn reconcile_live(
    mux: MuxName,
    ws: &KnownWorkspace,
    runtime: &RuntimePaths,
    rimz_bin: &Path,
    machine_config: &MachineConfig,
) -> ReloadOutcome {
    let mut outcome = ReloadOutcome {
        sessions: 1,
        ..ReloadOutcome::default()
    };
    let backend = backend_for(mux);
    record_live_room_bin(ws, rimz_bin);
    let before_signal = session_heartbeats(runtime, mux, &ws.session_name);
    let on_disk_build = on_disk_build(rimz_bin);

    // 1. Signal live sidebars to re-exec onto the freshly-installed binary.
    match wakeup::reload_sidebars(runtime) {
        Ok(_) => {}
        Err(err) => {
            tracing::warn!(
                session = %ws.session_name,
                tags.operation = "reload.signal_sidebars",
                error = &err as &dyn std::error::Error,
                "reload: signaling sidebars failed",
            );
        }
    }

    let liveness = match on_disk_build.as_deref() {
        Some(build) => {
            outcome.already_current += current_located_count(&before_signal, build);
            let awaiting = awaiting_panes(&before_signal, build);
            outcome.unverified += unlocated_unverified_count(&before_signal, build);
            let post_wait = if awaiting.is_empty() {
                before_signal
            } else {
                wait_for_convergence(runtime, mux, &ws.session_name, &awaiting, build)
            };
            let current = current_build_claims(&post_wait, build);
            outcome.reexeced += awaiting.intersection(&current).count();
            let stale_panes = awaiting.difference(&current).cloned().collect();
            current_build_liveness(&post_wait, build, stale_panes)
        }
        None => {
            outcome.unverified += before_signal.len();
            heartbeat_liveness(&before_signal)
        }
    };

    // 2. Converge the session's presence plugin onto the current wasm — reload
    //    is the explicit upgrade verb. Stale instances retire only after the
    //    replacement publishes topology from its new writer generation; a
    //    detached or degraded session keeps its prior plugin and retries on a
    //    later reload. Best-effort like every step here.
    let mux_config = MultiplexerConfig::from(machine_config);
    let presence_floor_ms = unix_now_ms();
    if let Some(wasm) = crate::mux::zellij::ensure_presence_plugin_artifact() {
        let presence = crate::mux::PresencePluginOptions {
            session_name: ws.session_name.clone(),
            workspace_id: ws.workspace_id.clone(),
            wasm,
            rimz_bin: rimz_bin.to_path_buf(),
            converge: true,
            seed_permissions: machine_config.web.enabled,
            focus_key: machine_config.sidebar.focus_key_label().map(str::to_owned),
            focus_follows_mouse: mux_config.zellij.focus_follows_mouse,
            mouse_click_through: mux_config.zellij.mouse_click_through,
        };
        if let Err(err) = backend.ensure_presence_plugin(&presence) {
            tracing::warn!(
                session = %ws.session_name,
                tags.operation = "reload.presence_converge",
                error = &err as &dyn std::error::Error,
                "reload: presence plugin convergence failed",
            );
        }
    }

    // 3. A Zellij reconcile depends on topology from this channel. Require a
    // post-converge publication rather than accepting a merely-young frozen
    // cache from a plugin whose host forks have stopped.
    if !presence_channel_is_live(mux, runtime, &ws.session_name, presence_floor_ms) {
        outcome.presence_dead += 1;
        crate::sidebar::sweep_orphan_runtime(runtime);
        return outcome;
    }

    // 4. Reconcile panes: keep each view's live sidebar, close duplicates and
    //    unresponsive ones, add to any working view left without one.
    let width = SidebarWidth::from_config(&machine_config.theme.display);
    let opts = SidebarPaneOptions {
        session_name: ws.session_name.clone(),
        workspace_id: ws.workspace_id.clone(),
        project_root: ws.project_root.clone(),
        extra_env: crate::agents::registry::room_env(runtime),
        cwd: ws.project_root.clone(),
        width,
        // A reload can run from a terminal unrelated to the session's clients,
        // so the bare cap only seeds a pane whose view geometry is unavailable.
        birth_size: width.birth_size(None),
        width_override: crate::sidebar::width_override::load(runtime),
        rimz_bin: rimz_bin.to_path_buf(),
        replace_existing: false,
        pristine_birth: false,
        config: mux_config.clone(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    let mut liveness = liveness;
    liveness.young_panes = young_sidebar_panes(mux, ws, jiff::Timestamp::now());
    match backend.reconcile_sidebars(&opts, &liveness) {
        Ok(report) => {
            outcome.restarted += report.restarted;
            outcome.recovered += report.recovered;
            outcome.closed += report.closed;
            outcome.failed += report.failed;
            outcome.deferred += report.deferred;
            outcome.redocked += report.redocked;
            outcome.misdocked += report.misdocked;
        }
        Err(err) => {
            tracing::warn!(
                session = %ws.session_name,
                tags.operation = "reload.reconcile",
                error = &err as &dyn std::error::Error,
                "reload: reconcile pass failed",
            );
        }
    }

    // 5. Reap orphan sidebar processes whose pane is gone — the mux cannot close a
    //    pane that no longer exists, so a wedged renderer would otherwise linger.
    outcome.reaped += reap_orphan_sidebars(backend.as_ref(), mux, ws);

    // 6. Sweep runtime files whose owner is gone — stale heartbeats and
    //    ownerless sockets accumulate in a live session too (every SIGKILLed or
    //    reaped renderer leaves a pair), and the sweep already spares anything
    //    fresh or still starting.
    crate::sidebar::sweep_orphan_runtime(runtime);
    outcome
}

const RELOAD_PRESENCE_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

fn presence_channel_is_live(
    mux: MuxName,
    runtime: &RuntimePaths,
    session_name: &str,
    min_produced_at_ms: u64,
) -> bool {
    if mux != MuxName::Zellij {
        return true;
    }
    let deadline = Instant::now() + RELOAD_PRESENCE_PROBE_TIMEOUT;
    loop {
        let now_ms = unix_now_ms();
        if crate::sidebar::cache::read_pane_topology_cache(runtime, session_name).is_some_and(
            |cache| {
                crate::sidebar::cache::pane_topology_cache_is_fresh(
                    &cache,
                    now_ms,
                    Some(min_produced_at_ms),
                )
            },
        ) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(crate::mux::zellij::TOPOLOGY_CACHE_POLL_STEP);
    }
}

fn record_live_room_bin(ws: &KnownWorkspace, rimz_bin: &Path) {
    let Ok(paths) = StatePaths::for_workspace(ws.workspace_id.clone()) else {
        return;
    };
    let Ok(mut record) = workspace_record::read(&paths.workspace_record) else {
        return;
    };
    record.rimz_bin = Some(rimz_bin.to_path_buf());
    record.updated_at = jiff::Timestamp::now();
    if let Err(err) = workspace_record::write_path(&paths.workspace_record, &record) {
        tracing::debug!(
            workspace = %ws.workspace_id,
            error = %err,
            "reload: recording room binary failed",
        );
    }
}

fn on_disk_build(rimz_bin: &Path) -> Option<String> {
    let binary = crate::build_id::resolve_on_disk_binary(rimz_bin)?;
    match crate::build_id::of_file(&binary) {
        Ok(build) => Some(build),
        Err(err) => {
            tracing::warn!(
                path = %binary.display(),
                tags.operation = "reload.digest_binary",
                error = &err as &dyn std::error::Error,
                "reload: cannot digest on-disk binary; convergence is unverified",
            );
            None
        }
    }
}

fn session_heartbeats(
    runtime: &RuntimePaths,
    mux: MuxName,
    session_name: &str,
) -> Vec<SidebarHeartbeat> {
    crate::sidebar::fresh_sidebar_heartbeats(runtime)
        .into_iter()
        .filter(|heartbeat| heartbeat.mux == mux && heartbeat.session_name == session_name)
        .collect()
}

fn current_located_count(heartbeats: &[SidebarHeartbeat], build: &str) -> usize {
    current_build_claims(heartbeats, build).len()
}

fn unlocated_unverified_count(heartbeats: &[SidebarHeartbeat], build: &str) -> usize {
    heartbeats
        .iter()
        .filter(|hb| hb.pane_id.is_none() && hb.build.as_deref() != Some(build))
        .count()
}

fn awaiting_panes(heartbeats: &[SidebarHeartbeat], build: &str) -> HashSet<PaneId> {
    heartbeats
        .iter()
        .filter(|hb| hb.build.as_deref() != Some(build))
        .filter_map(|hb| hb.pane_id.clone())
        .collect()
}

fn wait_for_convergence(
    runtime: &RuntimePaths,
    mux: MuxName,
    session_name: &str,
    awaiting: &HashSet<PaneId>,
    build: &str,
) -> Vec<SidebarHeartbeat> {
    wait_for_convergence_with(
        awaiting,
        build,
        RELOAD_CONVERGE_TIMEOUT,
        RELOAD_CONVERGE_POLL,
        || session_heartbeats(runtime, mux, session_name),
    )
}

fn wait_for_convergence_with(
    awaiting: &HashSet<PaneId>,
    build: &str,
    timeout: Duration,
    poll: Duration,
    mut read_heartbeats: impl FnMut() -> Vec<SidebarHeartbeat>,
) -> Vec<SidebarHeartbeat> {
    let deadline = Instant::now() + timeout;
    loop {
        let heartbeats = read_heartbeats();
        let current = current_build_claims(&heartbeats, build);
        if awaiting.is_subset(&current) || Instant::now() >= deadline {
            return heartbeats;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(remaining.min(poll));
    }
}

fn current_build_claims(heartbeats: &[SidebarHeartbeat], build: &str) -> HashSet<PaneId> {
    heartbeats
        .iter()
        .filter(|hb| hb.build.as_deref() == Some(build))
        .filter_map(|hb| hb.pane_id.clone())
        .collect()
}

fn current_build_liveness(
    heartbeats: &[SidebarHeartbeat],
    build: &str,
    stale_panes: HashSet<PaneId>,
) -> SidebarLiveness {
    let mut live = heartbeat_liveness(
        heartbeats
            .iter()
            .filter(|hb| hb.build.as_deref() == Some(build)),
    );
    live.stale_panes = stale_panes;
    live
}

fn heartbeat_liveness<'a>(
    heartbeats: impl IntoIterator<Item = &'a SidebarHeartbeat>,
) -> SidebarLiveness {
    let mut live = SidebarLiveness::default();
    for heartbeat in heartbeats {
        match heartbeat.pane_id.as_ref() {
            Some(pane) => {
                live.claimed_panes.insert(pane.clone());
            }
            None => live.has_unlocated = true,
        }
    }
    live
}

/// SIGTERM→SIGKILL this user's sidebar *processes* for `ws` whose pane the mux no
/// longer lists. A process we cannot attribute to a pane is left alone.
fn reap_orphan_sidebars(backend: &dyn MuxBackend, mux: MuxName, ws: &KnownWorkspace) -> usize {
    let now = jiff::Timestamp::now();
    let floor_ms =
        unix_now_ms().saturating_sub(crate::sidebar::FRESH_PANE_GRACE.as_millis() as u64);
    // Reap acts on pane absence, so topology cache hits must be no older than
    // the grace that also protects just-born sidebar processes below.
    let live_panes: HashSet<PaneId> = match backend.list_panes(PaneListOptions {
        session_name: Some(ws.session_name.clone()),
        runtime_paths: None,
        workspace_id: Some(ws.workspace_id.clone()),
        min_topology_produced_at_ms: Some(floor_ms),
        authoritative: false,
        require_authoritative: false,
        command_timeout: Some(RECONCILE_LIST_TIMEOUT),
    }) {
        Ok(listing) => listing.panes.into_iter().map(|pane| pane.pane_id).collect(),
        Err(err) => {
            tracing::warn!(
                session = %ws.session_name,
                tags.operation = "reload.reap_list_panes",
                error = &err as &dyn std::error::Error,
                "reload: pane listing for orphan reap failed; skipping reap",
            );
            return 0;
        }
    };
    let procs = crate::proc::list_processes();
    let protected = recovery::protected_pids(&procs, std::process::id());
    let my_uid = recovery::current_uid();
    let orphans: Vec<u32> = procs
        .iter()
        .filter(|proc| proc.real_uid == my_uid)
        .filter(|proc| !protected.contains(&proc.pid))
        .filter(|proc| {
            recovery::is_sidebar_serve(&proc.cmdline, ws.workspace_id.as_str(), &ws.session_name)
        })
        .filter(|proc| {
            !crate::proc::process_start(proc.pid)
                .is_some_and(|start| born_recently(start, now, crate::sidebar::FRESH_PANE_GRACE))
        })
        .filter(|proc| match recovery::attributed_pane(proc.pid, mux) {
            Some(pane) => !live_panes.contains(&pane),
            None => false,
        })
        .map(|proc| proc.pid)
        .collect();
    recovery::kill_pids(&orphans, recovery::SWEEP_GRACE).len()
}

/// The panes whose sidebar serve process for `ws` was born within
/// [`crate::sidebar::FRESH_PANE_GRACE`]: a just-added sidebar's first heartbeat
/// may not have landed yet, so the reconcile planner treats its unclaimed pane
/// as still starting rather than wedged. Attribution mirrors
/// [`reap_orphan_sidebars`] (cmdline scope + inherited mux pane env).
/// `/proc`-backed; empty elsewhere, where the planner just falls back to
/// close-and-readd.
fn young_sidebar_panes(mux: MuxName, ws: &KnownWorkspace, now: jiff::Timestamp) -> HashSet<PaneId> {
    crate::proc::list_processes()
        .iter()
        .filter(|proc| {
            recovery::is_sidebar_serve(&proc.cmdline, ws.workspace_id.as_str(), &ws.session_name)
        })
        .filter(|proc| {
            crate::proc::process_start(proc.pid)
                .is_some_and(|start| born_recently(start, now, crate::sidebar::FRESH_PANE_GRACE))
        })
        .filter_map(|proc| recovery::attributed_pane(proc.pid, mux))
        .collect()
}

/// Whether a process born at `start` is still within `grace` of `now` — the
/// youth predicate behind the fresh-pane grace, pure so the boundary is tested
/// without `/proc`. A start *after* `now` (clock fuzz on a just-spawned
/// process) reads as young.
fn born_recently(start: jiff::Timestamp, now: jiff::Timestamp, grace: Duration) -> bool {
    let grace = i64::try_from(grace.as_secs()).unwrap_or(0);
    now.as_second().saturating_sub(start.as_second()) <= grace
}

/// Both backends' live session names, so a workspace maps to the mux actually
/// running it (or to neither, meaning its session is gone).
struct LiveSessions {
    zellij: HashSet<String>,
    tmux: HashSet<String>,
}

impl LiveSessions {
    fn probe() -> Self {
        let names = |mux| -> HashSet<String> {
            backend_for(mux)
                .list_sessions()
                .unwrap_or_default()
                .into_iter()
                .collect()
        };
        Self {
            zellij: names(MuxName::Zellij),
            tmux: names(MuxName::Tmux),
        }
    }

    fn mux_of(&self, session: &str) -> Option<MuxName> {
        if self.zellij.contains(session) {
            Some(MuxName::Zellij)
        } else if self.tmux.contains(session) {
            Some(MuxName::Tmux)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reexec_target_resolves_the_replacement_after_an_install() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("rimz");
        std::fs::write(&real, b"x").unwrap();
        let deleted = PathBuf::from(format!("{} (deleted)", real.display()));
        assert!(!deleted.is_file(), "the annotated path must not exist");
        assert_eq!(resolve_reexec_target(deleted), Some(real.clone()));
        assert_eq!(resolve_reexec_target(real.clone()), Some(real));
    }

    #[test]
    fn reexec_target_is_none_when_nothing_exists_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("rimz");
        let deleted = PathBuf::from(format!("{} (deleted)", missing.display()));
        assert_eq!(resolve_reexec_target(deleted), None);
        assert_eq!(resolve_reexec_target(missing), None);
    }

    #[test]
    fn born_recently_holds_inside_the_grace_and_for_clock_fuzz() {
        let now = jiff::Timestamp::from_second(1_000_000).unwrap();
        let grace = crate::sidebar::FRESH_PANE_GRACE;
        let at = |secs_ago: i64| jiff::Timestamp::from_second(1_000_000 - secs_ago).unwrap();
        assert!(born_recently(at(0), now, grace));
        assert!(born_recently(
            at(i64::try_from(grace.as_secs()).unwrap()),
            now,
            grace
        ));
        assert!(
            !born_recently(at(i64::try_from(grace.as_secs()).unwrap() + 1), now, grace),
            "one second past the grace is old",
        );
        assert!(
            born_recently(at(-3), now, grace),
            "a start after `now` is a just-spawned process under clock fuzz",
        );
    }

    #[test]
    fn live_session_claim_is_once_per_mux_session() {
        let mut seen = HashSet::new();
        assert!(claim_live_session(
            &mut seen,
            MuxName::Zellij,
            "rimz-query-engine"
        ));
        assert!(!claim_live_session(
            &mut seen,
            MuxName::Zellij,
            "rimz-query-engine"
        ));
        assert!(
            claim_live_session(&mut seen, MuxName::Tmux, "rimz-query-engine"),
            "the same name on a different mux is a different live session",
        );
    }

    #[test]
    fn reload_outcome_merge_sums_every_field() {
        let mut base = ReloadOutcome {
            sessions: 1,
            presence_dead: 2,
            already_current: 3,
            reexeced: 4,
            restarted: 5,
            unverified: 6,
            recovered: 7,
            closed: 8,
            reaped: 9,
            dead_swept: 10,
            stats_reloaded: 11,
            failed: 12,
            deferred: 13,
            redocked: 14,
            misdocked: 15,
        };
        base.merge(ReloadOutcome {
            sessions: 16,
            presence_dead: 17,
            already_current: 18,
            reexeced: 19,
            restarted: 20,
            unverified: 21,
            recovered: 22,
            closed: 23,
            reaped: 24,
            dead_swept: 25,
            stats_reloaded: 26,
            failed: 27,
            deferred: 28,
            redocked: 29,
            misdocked: 30,
        });

        assert_eq!(
            base,
            ReloadOutcome {
                sessions: 17,
                presence_dead: 19,
                already_current: 21,
                reexeced: 23,
                restarted: 25,
                unverified: 27,
                recovered: 29,
                closed: 31,
                reaped: 33,
                dead_swept: 35,
                stats_reloaded: 37,
                failed: 39,
                deferred: 41,
                redocked: 43,
                misdocked: 45,
            }
        );
    }

    fn heartbeat(raw: &str, build: Option<&str>) -> SidebarHeartbeat {
        let pane = PaneId::from_parts(MuxName::Tmux, raw);
        let mut hb = SidebarHeartbeat::new(
            crate::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
            crate::SidebarInstanceId::new(),
            MuxName::Tmux,
            "rimz-test",
            std::path::PathBuf::from(format!("/tmp/{raw}.sock")),
            Some(pane),
        );
        hb.build = build.map(str::to_owned);
        hb
    }

    fn unlocated_heartbeat(build: Option<&str>) -> SidebarHeartbeat {
        let mut hb = SidebarHeartbeat::new(
            crate::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
            crate::SidebarInstanceId::new(),
            MuxName::Tmux,
            "rimz-test",
            std::path::PathBuf::from("/tmp/unlocated.sock"),
            None,
        );
        hb.build = build.map(str::to_owned);
        hb
    }

    #[test]
    fn build_partition_marks_missing_or_stale_builds_as_awaiting() {
        let heartbeats = vec![
            heartbeat("%1", Some("current")),
            heartbeat("%2", Some("old")),
            heartbeat("%3", None),
        ];

        assert_eq!(current_located_count(&heartbeats, "current"), 1);
        let awaiting = awaiting_panes(&heartbeats, "current");
        assert!(!awaiting.contains(&PaneId::from_parts(MuxName::Tmux, "%1")));
        assert!(awaiting.contains(&PaneId::from_parts(MuxName::Tmux, "%2")));
        assert!(awaiting.contains(&PaneId::from_parts(MuxName::Tmux, "%3")));
    }

    #[test]
    fn current_located_count_deduplicates_pane_claims() {
        let heartbeats = vec![
            heartbeat("%1", Some("current")),
            heartbeat("%1", Some("current")),
        ];

        assert_eq!(current_located_count(&heartbeats, "current"), 1);
    }

    #[test]
    fn current_build_liveness_excludes_stale_claims() {
        let heartbeats = vec![
            heartbeat("%1", Some("current")),
            heartbeat("%2", Some("old")),
            heartbeat("%3", None),
        ];

        let stale = [PaneId::from_parts(MuxName::Tmux, "%2")].into();
        let live = current_build_liveness(&heartbeats, "current", stale);
        assert!(
            live.claimed_panes
                .contains(&PaneId::from_parts(MuxName::Tmux, "%1"))
        );
        assert!(
            !live
                .claimed_panes
                .contains(&PaneId::from_parts(MuxName::Tmux, "%2"))
        );
        assert!(
            !live
                .claimed_panes
                .contains(&PaneId::from_parts(MuxName::Tmux, "%3"))
        );
        assert!(
            live.stale_panes
                .contains(&PaneId::from_parts(MuxName::Tmux, "%2"))
        );
    }

    #[test]
    fn current_build_liveness_keeps_unlocated_current_but_marks_stale_panes() {
        let heartbeats = vec![
            unlocated_heartbeat(Some("current")),
            heartbeat("%2", Some("old")),
        ];
        let stale = [PaneId::from_parts(MuxName::Tmux, "%2")].into();

        let live = current_build_liveness(&heartbeats, "current", stale);

        assert!(live.has_unlocated);
        assert!(
            live.stale_panes
                .contains(&PaneId::from_parts(MuxName::Tmux, "%2"))
        );
    }

    #[test]
    fn convergence_wait_settles_when_awaiting_pane_flips_current() {
        let awaiting = [PaneId::from_parts(MuxName::Tmux, "%1")].into();
        let mut reads = 0;

        let heartbeats = wait_for_convergence_with(
            &awaiting,
            "current",
            Duration::from_secs(1),
            Duration::ZERO,
            || {
                reads += 1;
                if reads == 1 {
                    vec![heartbeat("%1", Some("old"))]
                } else {
                    vec![heartbeat("%1", Some("current"))]
                }
            },
        );

        assert_eq!(reads, 2);
        assert_eq!(
            current_build_claims(&heartbeats, "current"),
            [PaneId::from_parts(MuxName::Tmux, "%1")].into(),
        );
    }

    #[test]
    fn convergence_wait_returns_latest_heartbeat_on_timeout() {
        let awaiting = [PaneId::from_parts(MuxName::Tmux, "%1")].into();

        let heartbeats =
            wait_for_convergence_with(&awaiting, "current", Duration::ZERO, Duration::ZERO, || {
                vec![heartbeat("%1", Some("old"))]
            });

        assert_eq!(heartbeats.len(), 1);
        assert_eq!(heartbeats[0].build.as_deref(), Some("old"));
    }
}
