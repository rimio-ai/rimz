//! Supervised-run pane lookup and reclamation effects.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::cli::GlobalFlags;
use rimz::agents::AgentState;
use rimz::harness::run::RunRecord;
use rimz::ids::{AgentKind, AgentSessionId, PaneId};
use rimz::mux::{
    LayoutColumn, LayoutPanes, PaneCmd, PaneListOptions, PaneReadConsistency, SidebarPaneOptions,
    SplitDirection, SplitPaneOptions, SplitPlacement, SplitTarget, TabOptions,
};
use rimz::pane::PaneRef;
use rimz::room::session::MissingSessionReport;

pub(crate) const STOP_BACKSTOP_GRACE: Duration = Duration::from_secs(3);
const STOP_BACKSTOP_POLL: Duration = Duration::from_millis(250);
const SUBAGENT_PANE_BIND_TIMEOUT: Duration = Duration::from_secs(3);
const SUBAGENT_PANE_BIND_POLL: Duration = Duration::from_millis(25);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SubagentZoneStrategy {
    Split {
        session_name: String,
        pane_id: PaneId,
        placement: SplitPlacement,
        on_failure: SubagentSplitFallback,
    },
    CompanionTab {
        title: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SubagentSplitFallback {
    CompanionTab,
    RunTab,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SubagentZoneOpen {
    Opened,
    CompanionTab,
    RunTab,
}

pub(super) fn select_subagent_zone_strategy(
    agents: &[AgentState],
    live_panes: &[PaneRef],
    caller: &AgentState,
    fallback_session: &str,
    theme: &rimz::config::ThemeConfig,
) -> Option<SubagentZoneStrategy> {
    let team = caller.team.as_deref().filter(|team| !team.is_empty());
    let parent_ids = match team {
        Some(_) => rimz::harness::target::team_cohorts(agents)
            .into_iter()
            .find(|cohort| {
                cohort
                    .members
                    .iter()
                    .any(|member| member.agent_id == caller.agent_id)
            })
            .map(|cohort| {
                cohort
                    .members
                    .into_iter()
                    .map(|member| member.agent_id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| vec![caller.agent_id.clone()]),
        None => vec![caller.agent_id.clone()],
    };
    if let Some(pane) = agents
        .iter()
        .filter(|agent| {
            agent.is_launched_child()
                && agent
                    .parent_agent_id
                    .as_ref()
                    .is_some_and(|parent| parent_ids.contains(parent))
                && agent
                    .pane
                    .as_ref()
                    .is_some_and(|pane| live_panes.iter().any(|live| live.pane_id == pane.pane_id))
        })
        .max_by_key(|agent| (agent.registered_at, agent.agent_id.clone()))
        .and_then(|agent| agent.pane.as_ref())
    {
        return Some(SubagentZoneStrategy::Split {
            session_name: pane_session_name(pane, fallback_session),
            pane_id: pane.pane_id.clone(),
            placement: SplitPlacement::Stacked,
            on_failure: if team.is_some() {
                SubagentSplitFallback::RunTab
            } else {
                SubagentSplitFallback::CompanionTab
            },
        });
    }
    if team.is_some() {
        let title = companion_title(caller, theme);
        if let Some(pane) = live_panes.iter().rev().find(|pane| {
            pane_in_named_view(pane, &title, theme) && !pane.is_floating && !pane.is_rimz_sidebar()
        }) {
            return Some(SubagentZoneStrategy::Split {
                session_name: pane_session_name(pane, fallback_session),
                pane_id: pane.pane_id.clone(),
                placement: SplitPlacement::Stacked,
                on_failure: SubagentSplitFallback::RunTab,
            });
        }
        if let Some(pane) = live_panes.iter().find(|pane| {
            pane_in_named_view(pane, &title, theme) && !pane.is_floating && pane.is_rimz_sidebar()
        }) {
            return Some(SubagentZoneStrategy::Split {
                session_name: pane_session_name(pane, fallback_session),
                pane_id: pane.pane_id.clone(),
                placement: SplitPlacement::Directional(SplitDirection::Right),
                on_failure: SubagentSplitFallback::RunTab,
            });
        }
        return Some(SubagentZoneStrategy::CompanionTab { title });
    }
    caller.pane.as_ref().and_then(|pane| {
        live_panes
            .iter()
            .any(|live| live.pane_id == pane.pane_id)
            .then(|| SubagentZoneStrategy::Split {
                session_name: pane_session_name(pane, fallback_session),
                pane_id: pane.pane_id.clone(),
                placement: SplitPlacement::Directional(SplitDirection::Right),
                on_failure: SubagentSplitFallback::CompanionTab,
            })
    })
}

fn pane_in_named_view(pane: &PaneRef, name: &str, theme: &rimz::config::ThemeConfig) -> bool {
    pane.view_name
        .as_deref()
        .is_some_and(|observed| rimz::theme::strip_status_glyph_suffix(observed, theme) == name)
}

fn pane_session_name(pane: &PaneRef, fallback: &str) -> String {
    if pane.session_name.is_empty() {
        fallback.to_owned()
    } else {
        pane.session_name.clone()
    }
}

fn companion_title(caller: &AgentState, theme: &rimz::config::ThemeConfig) -> String {
    caller
        .pane
        .as_ref()
        .and_then(|pane| pane.view_name.as_deref())
        .filter(|name| !name.is_empty())
        .map_or_else(
            || "subagents".to_owned(),
            |name| {
                format!(
                    "{} subagents",
                    rimz::theme::strip_status_glyph_suffix(name, theme)
                )
            },
        )
}

pub(super) fn subagent_companion_title(store: &rimz::Store) -> String {
    let machine = rimz::config::MachineConfig::load_lenient();
    store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .ok()
        .and_then(|projection| {
            rimz::harness::ancestry::resolve_launch_caller_from_env(&projection.agents)
                .ok()
                .map(|caller| companion_title(caller, &machine.theme))
        })
        .unwrap_or_else(|| "subagents".to_owned())
}

pub(super) fn lock_subagent_zone(
    store: &rimz::Store,
) -> rimz::store::lock::Result<rimz::store::lock::WorkspaceLock> {
    rimz::store::lock::WorkspaceLock::acquire(&store.paths().locks_dir.join("subagent-zone.lock"))
}

/// Open a supervised child in the caller's durable subagent zone, or select
/// the tab fallback that preserves the launch when mux placement is unavailable.
#[expect(
    clippy::too_many_arguments,
    reason = "one mux effect carries the complete pane birth contract"
)]
pub(super) fn split_into_subagent_zone(
    backend: &dyn rimz::mux::MuxBackend,
    store: &rimz::Store,
    workspace: &rimz::ResolvedWorkspace,
    cwd: &Path,
    env: BTreeMap<String, String>,
    sidebar: SidebarPaneOptions,
    pane: &PaneCmd,
    child_name: &str,
) -> SubagentZoneOpen {
    let projection = match store.runtime_projection(rimz::RuntimeScope::Audit) {
        Ok(projection) => projection,
        Err(err) => {
            tracing::debug!(
                error = &err as &dyn std::error::Error,
                "subagent zone lookup failed; falling back to a run tab",
            );
            return SubagentZoneOpen::RunTab;
        }
    };
    let caller = match rimz::harness::ancestry::resolve_launch_caller_from_env(&projection.agents) {
        Ok(caller) => caller,
        Err(err) => {
            tracing::debug!(
                error = &err as &dyn std::error::Error,
                "subagent zone caller lookup failed; falling back to a run tab",
            );
            return SubagentZoneOpen::RunTab;
        }
    };
    let live_panes = match list_subagent_zone_panes(backend, store, workspace) {
        Ok(listing) => listing.panes,
        Err(err) => {
            tracing::debug!(
                error = %err,
                "subagent zone pane lookup failed; falling back to a run tab",
            );
            return SubagentZoneOpen::RunTab;
        }
    };
    let machine = rimz::config::MachineConfig::load_lenient();
    let Some(strategy) = select_subagent_zone_strategy(
        &projection.agents,
        &live_panes,
        caller,
        &workspace.session_name,
        &machine.theme,
    ) else {
        return SubagentZoneOpen::CompanionTab;
    };
    let split = |session_name: String,
                 pane_id: PaneId,
                 placement: SplitPlacement,
                 on_failure: SubagentSplitFallback| {
        match backend.split_pane(SplitPaneOptions {
            target: SplitTarget::SessionPane {
                session_name: session_name.clone(),
                pane_id: pane_id.clone(),
            },
            cwd: Some(cwd.to_string_lossy().into_owned()),
            command: Some(pane.argv.clone()),
            title: Some(child_name.to_owned()),
            close_on_exit: false,
            env: env.clone(),
            placement,
            focus: false,
        }) {
            Ok(()) => SubagentZoneOpen::Opened,
            Err(err) => {
                tracing::debug!(
                    session = %session_name,
                    pane = %pane_id,
                    error = &err as &dyn std::error::Error,
                    ?on_failure,
                    "subagent zone split failed",
                );
                match on_failure {
                    SubagentSplitFallback::CompanionTab => SubagentZoneOpen::CompanionTab,
                    SubagentSplitFallback::RunTab => SubagentZoneOpen::RunTab,
                }
            }
        }
    };
    match strategy {
        SubagentZoneStrategy::Split {
            session_name,
            pane_id,
            placement,
            on_failure,
        } => split(session_name, pane_id, placement, on_failure),
        SubagentZoneStrategy::CompanionTab { title } => match backend.open_tab(&TabOptions {
            title,
            panes: LayoutPanes {
                columns: vec![LayoutColumn {
                    panes: vec![pane.clone()],
                    stacked: true,
                }],
            },
            focus: false,
            dock_sidebar: true,
            sidebar,
        }) {
            Ok(()) => SubagentZoneOpen::Opened,
            Err(err) => {
                tracing::debug!(
                    error = &err as &dyn std::error::Error,
                    "subagent companion tab failed; falling back to a run tab",
                );
                SubagentZoneOpen::RunTab
            }
        },
    }
}

fn list_subagent_zone_panes(
    backend: &dyn rimz::mux::MuxBackend,
    store: &rimz::Store,
    workspace: &rimz::ResolvedWorkspace,
) -> Result<rimz::mux::PaneListing> {
    backend
        .list_panes(PaneListOptions {
            session_name: Some(workspace.session_name.clone()),
            runtime_paths: Some(store.runtime_paths().clone()),
            workspace_id: Some(workspace.workspace_id.clone()),
            command_timeout: Some(Duration::from_millis(500)),
            consistency: PaneReadConsistency::RequireAuthoritative,
            ..Default::default()
        })
        .map_err(anyhow::Error::from)
}

pub(super) fn wait_for_subagent_pane_bind(
    store: &rimz::Store,
    kind: &AgentKind,
    launch_id: &AgentSessionId,
) {
    let bound = wait_for_subagent_pane_bind_with(
        || {
            store
                .runtime_projection(rimz::RuntimeScope::Audit)
                .map(|projection| launch_has_bound_pane(&projection.agents, kind, launch_id))
                .unwrap_or_else(|err| {
                    tracing::debug!(
                        agent = %launch_id,
                        error = &err as &dyn std::error::Error,
                        "subagent pane bind wait could not read durable state",
                    );
                    false
                })
        },
        SUBAGENT_PANE_BIND_TIMEOUT,
        SUBAGENT_PANE_BIND_POLL,
    );
    if !bound {
        tracing::debug!(
            agent = %launch_id,
            timeout_ms = SUBAGENT_PANE_BIND_TIMEOUT.as_millis(),
            "subagent pane bind was not visible before the launch returned",
        );
    }
}

fn launch_has_bound_pane(
    agents: &[AgentState],
    kind: &AgentKind,
    launch_id: &AgentSessionId,
) -> bool {
    agents.iter().any(|agent| {
        agent.kind == *kind && agent.launch_id.as_ref() == Some(launch_id) && agent.pane.is_some()
    })
}

pub(super) fn wait_for_subagent_pane_bind_with(
    mut is_bound: impl FnMut() -> bool,
    timeout: Duration,
    poll: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if is_bound() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(poll);
    }
}

/// Split a run pane into the loop zone, repairing a missing loop panel first.
/// `Ok(false)` means the caller should fall back to a run tab.
pub(super) fn split_into_loop_zone(
    backend: &dyn rimz::mux::MuxBackend,
    workspace: &rimz::ResolvedWorkspace,
    cwd: &Path,
    env: BTreeMap<String, String>,
    pane: &PaneCmd,
) -> Result<bool> {
    let listing = match list_loop_zone_panes(backend, workspace) {
        Some(listing) => listing,
        None => return Ok(false),
    };
    let panel = match rimz::daemon_view::find_loop_panel(&listing.panes) {
        Some(panel) => panel.clone(),
        None => {
            let machine = rimz::config::MachineConfig::load_lenient();
            let rimz_bin = rimz::proc::rimz_exe();
            // One gate decides whether a host launches. A cheaper local check
            // would spawn a host that stalls on its first-run prompt or a
            // version it cannot serve from, in the one path no operator watches.
            rimz::remote_control::prepare_hosts(&machine.remote_control);
            let readiness = rimz::remote_control::ReadinessSnapshot::probe(&machine.remote_control);
            let claude_host_argv = readiness.claude_host_argv().map(<[String]>::to_vec);
            let view =
                rimz::daemon_view::daemon_view_spec(rimz::daemon_view::DaemonViewSpecParams {
                    claude_host_argv: claude_host_argv.as_deref(),
                    daemon: &machine.daemon,
                    rimz_bin: &rimz_bin,
                    workspace_id: &workspace.workspace_id,
                    session_name: &workspace.session_name,
                    project_root: &workspace.project_root,
                    worktree_root: &workspace.worktree_root,
                    codex_present: which::which("codex").is_ok(),
                });
            match rimz::daemon_view::ensure_loop_panel(
                backend,
                &workspace.session_name,
                &workspace.workspace_id,
                &view,
            ) {
                Some(panel) => panel,
                None => return Ok(false),
            }
        }
    };
    match backend.split_pane(SplitPaneOptions {
        target: SplitTarget::SessionPane {
            session_name: workspace.session_name.clone(),
            pane_id: panel.pane_id.clone(),
        },
        cwd: Some(cwd.to_string_lossy().into_owned()),
        command: Some(pane.argv.clone()),
        title: None,
        close_on_exit: false,
        env,
        placement: SplitPlacement::Stacked,
        focus: false,
    }) {
        Ok(()) => Ok(true),
        Err(err) => {
            tracing::debug!(
                session = %workspace.session_name,
                pane = %panel.pane_id,
                error = &err as &dyn std::error::Error,
                "loop zone split failed; falling back to a run tab",
            );
            Ok(false)
        }
    }
}

fn list_loop_zone_panes(
    backend: &dyn rimz::mux::MuxBackend,
    workspace: &rimz::ResolvedWorkspace,
) -> Option<rimz::mux::PaneListing> {
    match backend.list_panes(PaneListOptions {
        session_name: Some(workspace.session_name.clone()),
        workspace_id: Some(workspace.workspace_id.clone()),
        command_timeout: Some(Duration::from_millis(500)),
        consistency: PaneReadConsistency::PreferAuthoritative,
        ..Default::default()
    }) {
        Ok(listing) => Some(listing),
        Err(err) => {
            tracing::debug!(
                session = %workspace.session_name,
                error = &err as &dyn std::error::Error,
                "loop zone lookup failed; falling back to a run tab",
            );
            None
        }
    }
}

pub(super) fn backend_for_workspace_session(
    workspace: &rimz::ResolvedWorkspace,
    globals: &GlobalFlags,
) -> Result<Box<dyn rimz::mux::MuxBackend>> {
    let mux =
        crate::cli::render::room::present_mux_pick(rimz::room::session::pick_mux_for_session(
            &workspace.session_name,
            globals.mux,
            MissingSessionReport::Silent,
        ))?;
    Ok(rimz::mux::backend_for(mux))
}

pub(super) fn close_run_pane(
    backend: &dyn rimz::mux::MuxBackend,
    store: &rimz::Store,
    session_name: &str,
    record: &RunRecord,
) {
    if let Some(pane_id) = record.pane_id.as_ref() {
        match backend.close_pane(session_name, pane_id) {
            Ok(()) => return,
            Err(err) => tracing::debug!(
                run_id = %record.run_id,
                pane = %pane_id,
                error = %err,
                "run cleanup could not close the recorded pane",
            ),
        }
    }
    let Some(pane) = resolve_run_pane_from_snapshot(store, session_name, record) else {
        return;
    };
    if let Err(err) = backend.close_pane(&pane.session_name, &pane.pane_id) {
        tracing::debug!(
            run_id = %record.run_id,
            pane = %pane.pane_id,
            error = %err,
            "run cleanup could not close the agent pane",
        );
    }
}

pub(crate) fn capture_failure_tail(
    backend: &dyn rimz::mux::MuxBackend,
    pane_id: &PaneId,
) -> Option<String> {
    // rimz-invariant: run-failure-capture
    let capture = match backend.capture_pane(pane_id, None, false) {
        Ok(capture) => capture,
        Err(err) => {
            tracing::debug!(
                pane = %pane_id,
                error = %err,
                "run failure pane capture unavailable",
            );
            return None;
        }
    };
    let tail = capture.raw_text.trim_end();
    if tail.trim().is_empty() {
        None
    } else {
        Some(tail.to_owned())
    }
}

pub(super) fn close_stopped_run_pane_after_grace(
    backend: &dyn rimz::mux::MuxBackend,
    store: &rimz::Store,
    session_name: &str,
    record: &RunRecord,
    grace: Duration,
) {
    let deadline = Instant::now() + grace;
    loop {
        let Some((latest, pane)) = latest_resolved_run_pane(store, session_name, record) else {
            if Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(STOP_BACKSTOP_POLL);
            continue;
        };
        match backend.list_panes(PaneListOptions {
            session_name: Some(pane.session_name.clone()),
            command_timeout: Some(STOP_BACKSTOP_POLL),
            ..Default::default()
        }) {
            Ok(listing)
                if listing
                    .panes
                    .iter()
                    .any(|candidate| candidate.pane_id == pane.pane_id) =>
            {
                if Instant::now() >= deadline {
                    close_run_pane(backend, store, session_name, &latest);
                    return;
                }
            }
            Ok(_) => return,
            Err(err) => {
                tracing::debug!(
                    run_id = %record.run_id,
                    error = %err,
                    "run stop backstop skipped; pane list unavailable",
                );
                return;
            }
        }
        std::thread::sleep(STOP_BACKSTOP_POLL);
    }
}

pub(super) fn latest_resolved_run_pane(
    store: &rimz::Store,
    session_name: &str,
    fallback: &RunRecord,
) -> Option<(RunRecord, ResolvedRunPane)> {
    let latest = latest_run_record(store, fallback);
    let pane = resolve_run_pane(store, session_name, &latest)?;
    Some((latest, pane))
}

fn latest_run_record(store: &rimz::Store, fallback: &RunRecord) -> RunRecord {
    rimz::harness::run::load(store.paths(), &fallback.run_id).unwrap_or_else(|err| {
        tracing::debug!(
            run_id = %fallback.run_id,
            error = %err,
            "run stop backstop using stale record; latest record unavailable",
        );
        fallback.clone()
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ResolvedRunPane {
    pub(super) pane_id: PaneId,
    pub(super) session_name: String,
}

pub(super) fn resolve_run_pane(
    store: &rimz::Store,
    session_name: &str,
    record: &RunRecord,
) -> Option<ResolvedRunPane> {
    record
        .pane_id
        .as_ref()
        .map(|pane_id| ResolvedRunPane {
            pane_id: pane_id.clone(),
            session_name: session_name.to_owned(),
        })
        .or_else(|| resolve_run_pane_from_snapshot(store, session_name, record))
}

fn resolve_run_pane_from_snapshot(
    store: &rimz::Store,
    session_name: &str,
    record: &RunRecord,
) -> Option<ResolvedRunPane> {
    let snapshot = match store.snapshot_cached() {
        Ok(snapshot) => snapshot,
        Err(err) => {
            tracing::debug!(run_id = %record.run_id, error = %err, "run pane resolution skipped; snapshot unavailable");
            return None;
        }
    };
    resolve_run_pane_in_snapshot(&snapshot, session_name, record)
}

pub(super) fn resolve_run_pane_in_snapshot(
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    session_name: &str,
    record: &RunRecord,
) -> Option<ResolvedRunPane> {
    let agent_id = record.agent_id.as_ref()?;
    let pane = snapshot
        .agents
        .iter()
        .find(|agent| agent.kind == record.kind && agent.agent_id == *agent_id)
        .and_then(|agent| agent.pane.as_ref())?;
    Some(ResolvedRunPane {
        pane_id: pane.pane_id.clone(),
        session_name: if pane.session_name.is_empty() {
            session_name.to_owned()
        } else {
            pane.session_name.clone()
        },
    })
}
