//! `rimz reload` — pick up a freshly-installed build and converge every running
//! sidebar to a healthy set, across all of this user's workspaces.
//!
//! User-scoped and cwd-independent: it enumerates every known workspace
//! ([`crate::workspace::known_workspaces`]), finds which have a live mux session,
//! and reconciles each in place — one live sidebar per working view, running the
//! current binary — closing duplicate or unresponsive sidebar panes and reaping
//! orphaned sidebar processes whose pane is gone. A workspace whose session is
//! gone has its stale runtime files and leftover daemons swept. Every step is
//! best-effort: a hiccup on one workspace is logged and never blocks the rest.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::{MachineConfig, MultiplexerConfig};
use crate::ids::{MuxName, PaneId};
use crate::ledger::RuntimePaths;
use crate::ledger::wakeup;
use crate::mux::recovery;
use crate::mux::{MuxBackend, PaneListOptions, SidebarPaneOptions, SidebarWidth, backend_for};
use crate::workspace::{self, KnownWorkspace};

/// What a user-wide reload did, aggregated across workspaces, for the CLI report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReloadOutcome {
    /// Live sessions reconciled.
    pub sessions: usize,
    /// Live sidebars told to re-exec onto the current binary.
    pub signaled: usize,
    /// Sidebars added (a view had none, or its only one was unresponsive).
    pub recovered: usize,
    /// Duplicate or unresponsive sidebar panes closed.
    pub closed: usize,
    /// Orphaned sidebar processes (pane gone) reaped.
    pub reaped: usize,
    /// Leftover processes swept from workspaces whose session is gone.
    pub dead_swept: usize,
    /// Views whose in-place add failed.
    pub failed: usize,
    /// Views whose in-place add was deferred (no attached client).
    pub deferred: usize,
    /// Kept sidebar panes whose geometry was repaired in place.
    pub redocked: usize,
}

/// Reload and reconcile every running sidebar across all of this user's
/// workspaces. Returns the aggregate outcome.
pub fn reload_user_sidebars() -> ReloadOutcome {
    let mut outcome = ReloadOutcome::default();
    let workspaces = match workspace::known_workspaces() {
        Ok(workspaces) => workspaces,
        Err(err) => {
            tracing::warn!(error = %err, "reload: cannot enumerate workspaces");
            return outcome;
        }
    };
    if workspaces.is_empty() {
        return outcome;
    }

    let rimz_bin = std::env::current_exe().unwrap_or_else(|err| {
        tracing::warn!(error = %err, "current executable unavailable; reload uses bare `rimz`");
        PathBuf::from("rimz")
    });
    let machine_config = MachineConfig::load().unwrap_or_else(|err| {
        tracing::warn!(error = %err, "reading per-machine config; using built-in defaults");
        MachineConfig::default()
    });
    let live = LiveSessions::probe();
    let mut reconciled_sessions: HashSet<(MuxName, String)> = HashSet::new();

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
                        tracing::warn!(workspace = %ws.workspace_id, error = %err, "reload: runtime paths");
                        continue;
                    }
                };
                reconcile_live(mux, &ws, &runtime, &rimz_bin, &machine_config, &mut outcome);
            }
            None => {
                let runtime = match RuntimePaths::for_workspace(ws.workspace_id.clone()) {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        tracing::warn!(workspace = %ws.workspace_id, error = %err, "reload: runtime paths");
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
    outcome: &mut ReloadOutcome,
) {
    outcome.sessions += 1;
    let backend = backend_for(mux);

    // 1. Signal live sidebars to re-exec onto the freshly-installed binary.
    match wakeup::reload_sidebars(runtime) {
        Ok(signaled) => outcome.signaled += signaled,
        Err(err) => {
            tracing::warn!(session = %ws.session_name, error = %err, "reload: signaling sidebars");
        }
    }

    // 2. Reconcile panes: keep each view's live sidebar, close duplicates and
    //    unresponsive ones, add to any working view left without one.
    let width = SidebarWidth::from_config(&machine_config.sidebar);
    let opts = SidebarPaneOptions {
        session_name: ws.session_name.clone(),
        workspace_id: ws.workspace_id.clone(),
        project_root: ws.project_root.clone(),
        cwd: ws.project_root.clone(),
        width,
        // A reload can run from a terminal (or no terminal) unrelated to the
        // session's clients, so there is no probe to resolve a verdict from —
        // and reconcile never consumes one: the heal paths size from the
        // session's own live geometry (`width`).
        birth_size: width.birth_size(None),
        rimz_bin: rimz_bin.to_path_buf(),
        replace_existing: false,
        config: MultiplexerConfig::from(machine_config),
        resume_panes: Vec::new(),
        refresh_ms: None,
    };
    let mut liveness = crate::sidebar::sidebar_liveness(runtime);
    liveness.young_panes = young_sidebar_panes(mux, ws, jiff::Timestamp::now());
    match backend.reconcile_sidebars(&opts, &liveness) {
        Ok(report) => {
            outcome.recovered += report.recovered;
            outcome.closed += report.closed;
            outcome.failed += report.failed;
            outcome.deferred += report.deferred;
            outcome.redocked += report.redocked;
        }
        Err(err) => {
            tracing::warn!(session = %ws.session_name, error = %err, "reload: reconcile pass failed");
        }
    }

    // 3. Converge the session's presence plugin onto the current wasm — reload
    //    is the explicit upgrade verb, so a running plugin re-loads in place
    //    when a client is attached (a detached session converges on its next
    //    attached reload; the plugin is at worst the prior build, and poll
    //    mode backstops it regardless). Best-effort like every step here.
    if let Some(wasm) = crate::mux::zellij::presence_plugin_path() {
        let presence = crate::mux::PresencePluginOptions {
            session_name: ws.session_name.clone(),
            workspace_id: ws.workspace_id.clone(),
            wasm,
            rimz_bin: rimz_bin.to_path_buf(),
            converge: true,
        };
        if let Err(err) = backend.ensure_presence_plugin(&presence) {
            tracing::warn!(session = %ws.session_name, error = %err, "reload: presence plugin convergence failed");
        }
    }

    // 4. Reap orphan sidebar processes whose pane is gone — the mux cannot close a
    //    pane that no longer exists, so a wedged renderer would otherwise linger.
    outcome.reaped += reap_orphan_sidebars(backend.as_ref(), mux, ws);

    // 5. Sweep runtime files whose owner is gone — stale heartbeats and
    //    ownerless sockets accumulate in a live session too (every SIGKILLed or
    //    reaped renderer leaves a pair), and the sweep already spares anything
    //    fresh or still starting.
    crate::sidebar::sweep_orphan_runtime(runtime);
}

/// SIGTERM→SIGKILL this user's sidebar *processes* for `ws` whose pane the mux no
/// longer lists. A process we cannot attribute to a pane is left alone.
fn reap_orphan_sidebars(backend: &dyn MuxBackend, mux: MuxName, ws: &KnownWorkspace) -> usize {
    let live_panes: HashSet<PaneId> = match backend.list_panes(PaneListOptions {
        session_name: Some(ws.session_name.clone()),
        ..Default::default()
    }) {
        Ok(panes) => panes.into_iter().map(|pane| pane.pane_id).collect(),
        Err(err) => {
            tracing::warn!(session = %ws.session_name, error = %err, "reload: listing panes to reap orphans");
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
}
