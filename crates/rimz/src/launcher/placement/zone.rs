//! Shared child-zone placement and durable pane binding.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::agents::AgentState;
use crate::diag::record::{
    DiagEvent, SubagentPaneBindCaller, SubagentPaneBindRead, SubagentPaneBindRow,
    SubagentZoneLookup,
};
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::mux::{
    CompanionAppendErr, CompanionPaneAppend, LayoutColumn, LayoutPanes, PaneCmd, PaneListOptions,
    PaneReadConsistency, SidebarPaneOptions, SplitDirection, SplitPaneOptions, SplitPlacement,
    SplitTarget, TabOptions,
};
use crate::pane::PaneRef;

const SUBAGENT_PANE_BIND_TIMEOUT: Duration = Duration::from_secs(3);
const SUBAGENT_PANE_BIND_POLL: Duration = Duration::from_millis(25);

pub struct PaneBindWait<'a> {
    pub run_id: &'a crate::RunId,
    pub launch_id: &'a AgentSessionId,
    pub kind: &'a AgentKind,
    pub caller: SubagentPaneBindCaller,
    pub open: Duration,
}

impl PaneBindWait<'_> {
    pub(super) fn wait_with(
        &self,
        sink: &crate::diag::DiagSink,
        mut read: impl FnMut() -> Result<Vec<AgentState>, String>,
        timeout: Duration,
        poll: Duration,
    ) -> bool {
        let started = Instant::now();
        let mut polls = 0;
        let mut last_read = SubagentPaneBindRead::Rows(Vec::new());
        let bound = wait_for_subagent_pane_bind_with(
            || {
                polls += 1;
                match read() {
                    Ok(agents) => {
                        let bound = launch_has_bound_pane(&agents, self.kind, self.launch_id);
                        last_read = SubagentPaneBindRead::Rows(
                            agents
                                .iter()
                                .filter(|agent| agent.launch_id.as_ref() == Some(self.launch_id))
                                .map(|agent| SubagentPaneBindRow {
                                    kind: agent.kind.clone(),
                                    pane: agent.pane.as_ref().map(|pane| pane.pane_id.clone()),
                                    ended_at: agent.ended_at,
                                })
                                .collect(),
                        );
                        bound
                    }
                    Err(error) => {
                        tracing::debug!(agent = %self.launch_id, %error, "subagent pane bind wait could not read durable state");
                        last_read = SubagentPaneBindRead::Failed { error };
                        false
                    }
                }
            },
            timeout,
            poll,
        );
        if !bound {
            sink.emit(DiagEvent::SubagentPaneBindTimeout {
                run_id: self.run_id.clone(),
                launch_id: self.launch_id.clone(),
                agent_kind: self.kind.clone(),
                caller: self.caller,
                open_ms: self.open.as_millis() as u64,
                waited_ms: started.elapsed().as_millis() as u64,
                polls,
                last_read,
            });
        }
        bound
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SubagentZoneStrategy {
    Split {
        session_name: String,
        pane_id: PaneId,
        placement: SplitPlacement,
    },
    CompanionTab {
        title: String,
    },
    CompanionStack {
        anchors: Vec<(String, PaneId)>,
        title: String,
    },
}

#[derive(Debug)]
pub enum SubagentZoneOpen {
    Opened,
    Failed(crate::mux::MuxErr),
}

pub(super) fn select_subagent_zone_strategy(
    agents: &[AgentState],
    live_panes: &[PaneRef],
    caller: &AgentState,
    fallback_session: &str,
    theme: &crate::config::ThemeConfig,
    base: &str,
) -> Option<SubagentZoneStrategy> {
    let team = caller.team.as_deref().filter(|team| !team.is_empty());
    if let Some(strategy) = companion_stack_strategy(live_panes, fallback_session, theme, base) {
        return Some(strategy);
    }
    if team.is_some() {
        return Some(SubagentZoneStrategy::CompanionTab {
            title: base.to_owned(),
        });
    }
    let live_caller = caller
        .pane
        .as_ref()
        .and_then(|pane| live_panes.iter().find(|live| live.pane_id == pane.pane_id));
    if let Some(pane) = crate::address::launched_children(agents, caller)
        .into_iter()
        .filter(|agent| {
            agent.pane.as_ref().is_some_and(|pane| {
                live_panes.iter().any(|live| {
                    live.pane_id == pane.pane_id
                        && live_caller.is_some_and(|parent| {
                            live.session_name == parent.session_name
                                && !live.is_floating
                                && !live.is_rimz_sidebar()
                                && live.view_id == parent.view_id
                                && live.view_name == parent.view_name
                        })
                })
            })
        })
        .max_by_key(|agent| (agent.registered_at, agent.agent_id.clone()))
        .and_then(|agent| agent.pane.as_ref())
    {
        return Some(SubagentZoneStrategy::Split {
            session_name: pane_session_name(pane, fallback_session),
            pane_id: pane.pane_id.clone(),
            placement: SplitPlacement::Stacked,
        });
    }
    caller.pane.as_ref().and_then(|pane| {
        live_panes
            .iter()
            .any(|live| live.pane_id == pane.pane_id)
            .then(|| SubagentZoneStrategy::Split {
                session_name: pane_session_name(pane, fallback_session),
                pane_id: pane.pane_id.clone(),
                placement: SplitPlacement::Directional(SplitDirection::Right),
            })
    })
}

fn companion_stack_strategy(
    live_panes: &[PaneRef],
    fallback_session: &str,
    theme: &crate::config::ThemeConfig,
    base: &str,
) -> Option<SubagentZoneStrategy> {
    let mut views = BTreeMap::<(usize, String, String), Vec<&PaneRef>>::new();
    for pane in live_panes.iter().filter(|pane| !pane.is_floating) {
        let Some(name) = pane.view_name.as_deref() else {
            continue;
        };
        let name = crate::theme::strip_status_glyph_suffix(name, theme);
        let number = if name == base {
            1
        } else if let Some(suffix) = name.strip_prefix(&format!("{base} "))
            && let Ok(number) = suffix.parse::<usize>()
            && number >= 2
            && suffix == number.to_string()
        {
            number
        } else {
            continue;
        };
        views
            .entry((
                number,
                pane_session_name(pane, fallback_session),
                pane.view_id.clone().unwrap_or_else(|| name.to_owned()),
            ))
            .or_default()
            .push(pane);
    }
    let next = views
        .keys()
        .map(|(number, _, _)| *number)
        .max()?
        .saturating_add(1);
    let title = format!("{base} {next}");
    let anchors = views
        .into_iter()
        .filter_map(|((_, session, _), panes)| {
            let work = panes
                .iter()
                .filter(|pane| !pane.is_rimz_sidebar())
                .collect::<Vec<_>>();
            // A sidebar-only listing may hide retained terminals on Zellij.
            // Always let the backend inspect physical occupancy before birth.
            work.first()
                .copied()
                .or_else(|| panes.first())
                .map(|pane| (session, pane.pane_id.clone()))
        })
        .collect::<Vec<_>>();
    if anchors.is_empty() {
        Some(SubagentZoneStrategy::CompanionTab { title })
    } else {
        Some(SubagentZoneStrategy::CompanionStack { anchors, title })
    }
}

fn pane_session_name(pane: &PaneRef, fallback: &str) -> String {
    if pane.session_name.is_empty() {
        fallback.to_owned()
    } else {
        pane.session_name.clone()
    }
}

fn companion_title(caller: &AgentState, theme: &crate::config::ThemeConfig) -> String {
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
                    crate::theme::strip_status_glyph_suffix(name, theme)
                )
            },
        )
}

fn channel_companion_title(channel: &str) -> String {
    format!("#{channel} subagents")
}

pub(super) fn subagent_companion_title(store: &crate::Store, channel: Option<&str>) -> String {
    if let Some(channel) = channel {
        return channel_companion_title(channel);
    }
    let machine = crate::config::MachineConfig::load_lenient();
    store
        .runtime_projection(crate::RuntimeScope::Audit)
        .ok()
        .and_then(|projection| {
            crate::harness::ancestry::resolve_calling_agent(&projection.agents)
                .ok()
                .map(|caller| companion_title(caller, &machine.theme))
        })
        .unwrap_or_else(|| "subagents".to_owned())
}

pub fn lock_subagent_zone(
    store: &crate::Store,
) -> crate::disk::lock::Result<crate::disk::lock::WorkspaceLock> {
    crate::disk::lock::WorkspaceLock::acquire(
        &store.runtime_paths().lock_path("subagent-zone.lock"),
    )
}

/// Open a supervised child in the caller's durable subagent zone, falling
/// back to a companion tab when mux placement is unavailable.
#[expect(
    clippy::too_many_arguments,
    reason = "one mux effect carries the complete pane birth contract"
)]
pub fn split_into_subagent_zone(
    backend: &dyn crate::mux::MuxBackend,
    store: &crate::Store,
    workspace: &crate::ResolvedWorkspace,
    cwd: &Path,
    env: BTreeMap<String, String>,
    sidebar: SidebarPaneOptions,
    pane: &PaneCmd,
    child_name: &str,
    channel: Option<&str>,
    after: Option<PaneId>,
    run_id: &crate::RunId,
) -> SubagentZoneOpen {
    let title = channel.map_or_else(|| "subagents".to_owned(), channel_companion_title);
    let lookup_failed = |lookup, error| {
        crate::diag::DiagSink::under(
            store.paths().root.clone(),
            workspace.workspace_id.clone(),
            &workspace.session_name,
            None,
        )
        .emit(DiagEvent::SubagentZoneLookupFailed {
            run_id: run_id.clone(),
            lookup,
            error,
        });
    };
    let open_companion = |title, after| match backend.open_tab(&TabOptions {
        env: env.clone(),
        title,
        panes: LayoutPanes {
            columns: vec![LayoutColumn {
                panes: vec![pane.clone()],
                stacked: false,
            }],
            focused_pane: 0,
        },
        focus: false,
        dock_sidebar: true,
        after,
        sidebar: sidebar.clone(),
    }) {
        Ok(()) => SubagentZoneOpen::Opened,
        Err(err) => {
            // A failed response may follow a successful tab birth. Never
            // execute the same durable run in a second tab.
            tracing::debug!(error = %err, "subagent companion tab could not be confirmed");
            SubagentZoneOpen::Failed(err)
        }
    };
    let mut projection = match store.runtime_projection(crate::RuntimeScope::Audit) {
        Ok(projection) => projection,
        Err(err) => {
            lookup_failed(SubagentZoneLookup::Projection, format!("{err:#}"));
            return open_companion(title, after);
        }
    };
    crate::store::agent_context::attach_rest_certificates(
        store.runtime_paths(),
        &mut projection.agents,
    );
    let caller = match crate::harness::ancestry::resolve_calling_agent(&projection.agents) {
        Ok(caller) => caller,
        Err(err) => {
            lookup_failed(SubagentZoneLookup::Caller, format!("{err:#}"));
            return open_companion(title, after);
        }
    };
    let machine = crate::config::MachineConfig::load_lenient();
    let title = if channel.is_none() {
        companion_title(caller, &machine.theme)
    } else {
        title
    };
    let after = caller
        .pane
        .as_ref()
        .map(|pane| pane.pane_id.clone())
        .or(after);
    let live_panes = match list_subagent_zone_panes(backend, store, workspace) {
        Ok(listing) => listing.panes,
        Err(err) => {
            lookup_failed(SubagentZoneLookup::PaneListing, format!("{err:#}"));
            return open_companion(title, after);
        }
    };
    let strategy = select_subagent_zone_strategy(
        &projection.agents,
        &live_panes,
        caller,
        &workspace.session_name,
        &machine.theme,
        &title,
    )
    .unwrap_or_else(|| SubagentZoneStrategy::CompanionTab {
        title: title.clone(),
    });
    let split = |session_name: String, pane_id: PaneId, placement: SplitPlacement| match backend
        .split_pane(SplitPaneOptions {
            target: SplitTarget::SessionPane {
                session_name: session_name.clone(),
                pane_id: pane_id.clone(),
            },
            title: Some(child_name.to_owned()),
            env: env.clone(),
            placement,
            ..SplitPaneOptions::from_command(pane, cwd)
        }) {
        Ok(_) => SubagentZoneOpen::Opened,
        Err(err) => {
            tracing::debug!(
                session = %session_name,
                pane = %pane_id,
                error = &err as &dyn std::error::Error,
                "subagent zone split failed",
            );
            if ambiguous_pane_open(&err) {
                SubagentZoneOpen::Failed(err)
            } else {
                open_companion(title.clone(), after.clone())
            }
        }
    };
    match strategy {
        SubagentZoneStrategy::Split {
            session_name,
            pane_id,
            placement,
        } => split(session_name, pane_id, placement),
        SubagentZoneStrategy::CompanionTab { title } => open_companion(title, after),
        SubagentZoneStrategy::CompanionStack { anchors, title } => {
            for (session_name, pane_id) in anchors {
                match backend.append_companion_pane(SplitPaneOptions {
                    target: SplitTarget::SessionPane {
                        session_name,
                        pane_id,
                    },
                    title: Some(child_name.to_owned()),
                    env: env.clone(),
                    ..SplitPaneOptions::from_command(pane, cwd)
                }) {
                    Ok(CompanionPaneAppend::Opened) => return SubagentZoneOpen::Opened,
                    Ok(CompanionPaneAppend::Full) => continue,
                    Err(CompanionAppendErr::BeforeSpawn(err)) => {
                        lookup_failed(SubagentZoneLookup::CompanionStack, format!("{err:#}"));
                        return open_companion(title, after);
                    }
                    Err(CompanionAppendErr::Spawn(err)) => {
                        // A spawn timeout may have opened the pane. Never launch
                        // the same durable run twice after an ambiguous result.
                        tracing::warn!(error = %err, "subagent stack append failed");
                        if ambiguous_pane_open(&err) {
                            return SubagentZoneOpen::Failed(err);
                        }
                    }
                }
            }
            open_companion(title, after)
        }
    }
}

fn ambiguous_pane_open(error: &crate::mux::MuxErr) -> bool {
    matches!(
        error,
        crate::mux::MuxErr::Timeout { .. } | crate::mux::MuxErr::Output { .. }
    )
}

fn list_subagent_zone_panes(
    backend: &dyn crate::mux::MuxBackend,
    store: &crate::Store,
    workspace: &crate::ResolvedWorkspace,
) -> Result<crate::mux::PaneListing> {
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

pub fn wait_for_subagent_pane_bind(
    store: &crate::Store,
    workspace: &crate::ResolvedWorkspace,
    wait: PaneBindWait<'_>,
) -> bool {
    let sink = crate::diag::DiagSink::under(
        store.paths().root.clone(),
        workspace.workspace_id.clone(),
        workspace.session_name.clone(),
        None,
    );
    let bound = wait.wait_with(
        &sink,
        || {
            store
                .runtime_projection(crate::RuntimeScope::Audit)
                .map(|projection| projection.agents)
                .map_err(|err| err.to_string())
        },
        SUBAGENT_PANE_BIND_TIMEOUT,
        SUBAGENT_PANE_BIND_POLL,
    );
    if !bound {
        tracing::debug!(
            agent = %wait.launch_id,
            timeout_ms = SUBAGENT_PANE_BIND_TIMEOUT.as_millis(),
            "subagent pane bind was not visible before the launch returned",
        );
    }
    bound
}

pub fn launch_has_bound_pane(
    agents: &[AgentState],
    kind: &AgentKind,
    launch_id: &AgentSessionId,
) -> bool {
    agents.iter().any(|agent| {
        agent.kind == *kind
            && agent.launch_id.as_ref() == Some(launch_id)
            && agent.pane.is_some()
            && agent.ended_at.is_none()
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
