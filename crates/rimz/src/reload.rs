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

use crate::config::{MachineConfig, MultiplexerConfig};
use crate::ids::{MuxName, PaneId};
use crate::ledger::RuntimePaths;
use crate::ledger::wakeup;
use crate::mux::recovery;
use crate::mux::{
    DEFAULT_SIDEBAR_WIDTH_PERCENT, MuxBackend, PaneListOptions, SidebarPaneOptions, backend_for,
};
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
    let opts = SidebarPaneOptions {
        session_name: ws.session_name.clone(),
        workspace_id: ws.workspace_id.clone(),
        cwd: ws.project_root.clone(),
        width_percent: DEFAULT_SIDEBAR_WIDTH_PERCENT,
        rimz_bin: rimz_bin.to_path_buf(),
        replace_existing: false,
        config: MultiplexerConfig::from(machine_config),
        resume_panes: Vec::new(),
    };
    let liveness = crate::sidebar::sidebar_liveness(runtime);
    match backend.reconcile_sidebars(&opts, &liveness) {
        Ok(report) => {
            outcome.recovered += report.recovered;
            outcome.closed += report.closed;
            outcome.failed += report.failed;
        }
        Err(err) => {
            tracing::warn!(session = %ws.session_name, error = %err, "reload: reconcile pass failed");
        }
    }

    // 3. Reap orphan sidebar processes whose pane is gone — the mux cannot close a
    //    pane that no longer exists, so a wedged renderer would otherwise linger.
    outcome.reaped += reap_orphan_sidebars(backend.as_ref(), mux, ws);
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
    let my_uid = current_uid();
    let orphans: Vec<u32> = procs
        .iter()
        .filter(|proc| proc.real_uid == my_uid)
        .filter(|proc| !protected.contains(&proc.pid))
        .filter(|proc| is_sidebar_serve(&proc.cmdline, ws.workspace_id.as_str(), &ws.session_name))
        .filter(|proc| match attributed_pane(proc.pid, mux) {
            Some(pane) => !live_panes.contains(&pane),
            None => false,
        })
        .map(|proc| proc.pid)
        .collect();
    recovery::kill_pids(&orphans, recovery::SWEEP_GRACE).len()
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

/// Whether `cmdline` is one of `(workspace, session)`'s sidebar *serve* processes
/// — the wrapper `rimz sidebar serve` or the renderer `rimz-sidebar serve` — and
/// not the mux server or the agent app-server. The exact, path-derived session
/// name plus the workspace id scope it; `sidebar` + `serve` selects the renderer
/// pair and excludes `rimz codex app-server serve`.
fn is_sidebar_serve(cmdline: &str, workspace_id: &str, session_name: &str) -> bool {
    cmdline.contains(session_name)
        && cmdline.contains(workspace_id)
        && cmdline.contains("sidebar")
        && cmdline.contains("serve")
}

/// The normalized pane a sidebar process paints, from its inherited mux env var —
/// the same mapping the renderer applies to its own pane (`own_pane_id`). `None`
/// when the var is absent, so reload never reaps a process it cannot place.
fn attributed_pane(pid: u32, mux: MuxName) -> Option<PaneId> {
    let key = match mux {
        MuxName::Zellij => "ZELLIJ_PANE_ID",
        MuxName::Tmux => "TMUX_PANE",
    };
    Some(pane_from_env_value(mux, &crate::proc::env_var(pid, key)?))
}

/// Normalize a raw mux pane env value into a [`PaneId`]: Zellij exposes a bare
/// integer (`terminal_<id>`), tmux the full raw id (`%<n>`).
fn pane_from_env_value(mux: MuxName, raw_env: &str) -> PaneId {
    let raw = match mux {
        MuxName::Zellij => format!("terminal_{raw_env}"),
        MuxName::Tmux => raw_env.to_owned(),
    };
    PaneId::from_parts(mux, raw)
}

#[cfg(target_os = "linux")]
fn current_uid() -> u32 {
    nix::unistd::Uid::current().as_raw()
}

#[cfg(not(target_os = "linux"))]
fn current_uid() -> u32 {
    // No `/proc`, so `list_processes` is empty and this is never compared.
    u32::MAX
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "rimz-home-marvin-workspace-project-rimz-rimz";
    const WS: &str = "ws_f89e49906df0621ad2765112";

    #[test]
    fn is_sidebar_serve_matches_wrapper_and_renderer_only() {
        let wrapper =
            format!("rimz sidebar serve --mux zellij --workspace-id {WS} --session-name {SESSION}");
        let renderer = format!(
            "rimz-sidebar serve --workspace-id {WS} --mux zellij --session-name {SESSION} --tick-seconds 1"
        );
        assert!(is_sidebar_serve(&wrapper, WS, SESSION));
        assert!(is_sidebar_serve(&renderer, WS, SESSION));
    }

    #[test]
    fn is_sidebar_serve_excludes_server_and_app_server() {
        let app_server =
            format!("rimz codex app-server serve --workspace-id {WS} --session-name {SESSION}");
        let mux_server =
            format!("zellij --server /run/user/1000/zellij/contract_version_1/{SESSION}");
        assert!(
            !is_sidebar_serve(&app_server, WS, SESSION),
            "app-server is not a sidebar"
        );
        assert!(
            !is_sidebar_serve(&mux_server, WS, SESSION),
            "the mux server is never reaped"
        );
    }

    #[test]
    fn is_sidebar_serve_is_scoped_to_the_workspace_and_session() {
        let other_session = "rimz-sidebar serve --workspace-id ws_other --session-name rimz-other";
        assert!(!is_sidebar_serve(other_session, WS, SESSION));
    }

    #[test]
    fn pane_from_env_value_normalizes_per_mux() {
        assert_eq!(
            pane_from_env_value(MuxName::Zellij, "3"),
            PaneId::from_parts(MuxName::Zellij, "terminal_3"),
        );
        assert_eq!(
            pane_from_env_value(MuxName::Tmux, "%5"),
            PaneId::from_parts(MuxName::Tmux, "%5"),
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
