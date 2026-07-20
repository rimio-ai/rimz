//! Zellij presence-plugin wake ingestion.
//!
//! `rimz sidebar wake` normalizes the plugin payload at the CLI boundary, then
//! this module owns the accepted-or-rejected transaction: topology writer
//! fencing and publication, presence stamping, plugin telemetry, and event
//! mapping. A stale writer returns before any accepted-wake side effect.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::diag::DiagSink;
use crate::diag::plugin_presence::{PluginPresenceSample, WASM_PAGE_BYTES};
use crate::diag::record::DiagEvent;
use crate::ids::{MuxName, PaneId};
use crate::mux::zellij::pane_topology::{
    PaneTopologyCache, PaneTopologyPane, TopologyWriter, ZellijPaneId,
};
use crate::pane::SIDEBAR_CHROME_TITLE;
use crate::sidebar::cache::{
    PresenceDesired, pane_topology_cache_is_fresh, read_pane_topology_cache, read_presence_desired,
    write_pane_topology_cache, write_presence_stamp,
};
use crate::sidebar::events::SidebarEvent;
use crate::sidebar::timing::unix_now_ms;
use crate::{RuntimePaths, StatePaths};

pub(crate) mod projector;
pub(crate) mod tmux;

use projector::{
    PaneEventEligibility, PaneObservation, PresencePaneRole, PresenceTransition, project_presence,
};

const TOPOLOGY_CONFLICT_DIAG_MS: u64 = 60_000;
/// Private `rimz sidebar wake` status consumed by the Zellij plugin. Three
/// consecutive publishes rejected with this code retire the losing writer.
pub const STALE_WRITER_EXIT_CODE: i32 = 73;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZellijWakeReason {
    Announced,
    Alive,
    FocusStranded,
    SwitchSettled,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct ZellijPluginTelemetry {
    pub plugin_id: Option<u32>,
    #[serde(rename = "plugin_build")]
    pub build: Option<String>,
    pub loaded_at_ms: u64,
    #[serde(rename = "mem_pages")]
    pub pages: u64,
    pub uptime_ms: u64,
    #[serde(rename = "commands_completed")]
    pub commands: u64,
    pub commands_succeeded: Option<u64>,
    pub stale_writer_rejections: Option<u64>,
    pub topology_failures: Option<u64>,
    pub other_failures: Option<u64>,
    pub zellij_version: Option<String>,
    #[serde(default)]
    pub last_failure: Option<PluginCommandFailure>,
}

/// Why the plugin's most recent wake failed, as the host itself reported it on
/// stderr before exiting, and the plugin clock reading it happened at.
///
/// The stamp arrived with the failure-retention fix; a plugin loaded before it
/// reports the cause without one. `None` therefore reads as "age unknown" and
/// keeps such a cause usable, rather than dating it to the epoch and hiding it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct PluginCommandFailure {
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub at_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZellijWake {
    pub reason: ZellijWakeReason,
    pub session_name: Option<String>,
    pub pane_id: Option<PaneId>,
    pub active_tab: Option<u64>,
    pub focus_generation: Option<u64>,
    pub focus_clients: Vec<crate::mux::ClientPaneView>,
    pub topology: Option<PaneTopologyCache>,
    pub telemetry: Option<ZellijPluginTelemetry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ZellijWakeOutcome {
    RejectedStaleWriter,
    Accepted(Vec<SidebarEvent>),
}

#[derive(Debug, thiserror::Error)]
pub enum ZellijWakeError {
    #[error("could not serialize topology writer selection: {0}")]
    TopologyLock(#[from] crate::store::lock::LockErr),
    #[error("could not publish accepted topology: {0}")]
    TopologyWrite(#[source] crate::store::atomic::AtomicErr),
    #[error("could not publish topology writer conflict: {0}")]
    ConflictWrite(#[source] crate::store::atomic::AtomicErr),
    #[error("could not clear topology writer conflict {path}: {source}")]
    ConflictClear {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Apply one normalized Zellij presence poke as a single policy transaction.
pub fn ingest_zellij_wake(
    state: &StatePaths,
    runtime: &RuntimePaths,
    wake: &ZellijWake,
) -> Result<ZellijWakeOutcome, ZellijWakeError> {
    let mut transitions = Vec::new();
    let mut accepted_topology = None;
    if let Some(incoming) = wake.topology.as_ref() {
        let _guard = crate::store::lock::WorkspaceLock::acquire_with_timeout(
            &runtime.topology_writer_lock(),
            Duration::from_secs(1),
        )?;
        let now_ms = unix_now_ms();
        let existing = read_pane_topology_cache(runtime, &incoming.session_name);
        let desired = read_presence_desired(runtime);
        if topology_decision(existing.as_ref(), incoming, desired.as_ref(), now_ms)
            == TopologyDecision::Reject
        {
            // Reject is reachable only with a fresh same-session cache.
            if let Some(existing) = existing.as_ref() {
                record_topology_write_rejected(state, runtime, incoming, existing, now_ms)?;
            }
            return Ok(ZellijWakeOutcome::RejectedStaleWriter);
        }
        let writer_changed = existing
            .as_ref()
            .is_none_or(|existing| incoming.writer != existing.writer);
        let mut cache = incoming.clone();
        sanitize_topology_cache(&mut cache);
        transitions = derive_zellij_transitions(
            existing.as_ref(),
            &cache,
            wake.reason == ZellijWakeReason::Announced,
        );
        write_pane_topology_cache(runtime, &cache).map_err(ZellijWakeError::TopologyWrite)?;
        accepted_topology = Some(cache);
        if let Some(existing) = existing.as_ref()
            && incoming.writer != existing.writer
        {
            emit_topology_writer_changed(
                state,
                &incoming.session_name,
                existing.writer.as_ref(),
                incoming.writer.as_ref(),
            );
        }
        if writer_changed {
            clear_superseded_conflict(runtime, incoming.writer.as_ref(), desired.as_ref())?;
        }
    }

    write_presence_stamp(runtime, MuxName::Zellij, wake.session_name.as_deref());
    if let Some(telemetry) = wake.telemetry.as_ref() {
        write_plugin_presence_sample(state, wake.session_name.clone(), telemetry);
    }
    if wake.topology.is_none() && wake.reason == ZellijWakeReason::Announced {
        transitions.push(PresenceTransition::Nudge);
    }
    if wake.reason == ZellijWakeReason::FocusStranded
        && let (Some(pane_id), Some(generation)) = (&wake.pane_id, wake.focus_generation)
    {
        transitions.push(PresenceTransition::ViewSwitched {
            focused: Some(PaneObservation {
                pane_id: pane_id.clone(),
                view: String::new(),
                command: None,
                role: PresencePaneRole::Sidebar,
                events: zellij_event_eligibility(PresencePaneRole::Sidebar),
            }),
            prior: None,
            has_working: true,
            generation,
            clients: wake.focus_clients.clone(),
        });
    }
    if wake.reason == ZellijWakeReason::SwitchSettled
        && let (Some(active_tab), Some(generation), Some(session_name)) = (
            wake.active_tab,
            wake.focus_generation,
            wake.session_name.as_deref(),
        )
    {
        let topology = accepted_topology
            .filter(|topology| topology.session_name == session_name)
            .or_else(|| read_pane_topology_cache(runtime, session_name));
        if let Some(transition) = topology.as_ref().and_then(|topology| {
            switch_settled_transition(topology, active_tab, generation, &wake.focus_clients)
        }) {
            transitions.push(transition);
        }
    }
    Ok(ZellijWakeOutcome::Accepted(project_presence(transitions)))
}

fn derive_zellij_transitions(
    existing: Option<&PaneTopologyCache>,
    incoming: &PaneTopologyCache,
    announced: bool,
) -> Vec<PresenceTransition> {
    if !announced {
        return Vec::new();
    }
    let Some(existing) = existing.filter(|cache| cache.writer == incoming.writer) else {
        return vec![PresenceTransition::Nudge];
    };
    let old = panes_by_native_id(existing);
    let new = panes_by_native_id(incoming);
    let mut transitions = Vec::new();

    for (id, pane) in &old {
        if id.terminal_id().is_some() && !new.contains_key(id) {
            transitions.push(PresenceTransition::PaneRemoved(pane_observation(pane)));
        }
    }
    for (id, pane) in &new {
        if !old.contains_key(id) && id.terminal_id().is_some() && pane.is_live_terminal() {
            let mut current = pane_observation(pane);
            current.command = pane
                .pane_command
                .as_deref()
                .or(pane.terminal_command.as_deref())
                .filter(|command| !command.is_empty() && !command_is_launch_chrome(command))
                .map(str::to_owned);
            transitions.push(PresenceTransition::PaneObserved {
                current,
                previous: None,
            });
        }
    }
    for (id, pane) in &new {
        let Some(previous) = old.get(id) else {
            continue;
        };
        if id.terminal_id().is_some()
            && previous.is_live_terminal()
            && pane.is_live_terminal()
            && previous.pane_command != pane.pane_command
        {
            transitions.push(PresenceTransition::PaneObserved {
                current: pane_observation(pane),
                previous: Some(pane_observation(previous)),
            });
        }
    }
    let prior_focus = existing.projected_session_focus();
    let current_focus = incoming.projected_session_focus();
    if prior_focus != current_focus {
        transitions.push(PresenceTransition::PaneFocused {
            focused: current_focus
                .as_ref()
                .and_then(|pane| observation_for_id(incoming, pane)),
            prior: prior_focus
                .as_ref()
                .and_then(|pane| observation_for_id(existing, pane)),
        });
    }
    transitions.push(PresenceTransition::Nudge);
    transitions
}

fn panes_by_native_id(cache: &PaneTopologyCache) -> BTreeMap<ZellijPaneId, &PaneTopologyPane> {
    cache
        .panes
        .iter()
        .map(|pane| (pane.native_id(), pane))
        .collect()
}

fn pane_observation(pane: &PaneTopologyPane) -> PaneObservation {
    let role = if topology_pane_is_sidebar(pane) {
        PresencePaneRole::Sidebar
    } else if pane
        .pane_command
        .as_deref()
        .or(pane.terminal_command.as_deref())
        .is_some_and(command_is_launch_chrome)
    {
        PresencePaneRole::LaunchChrome
    } else {
        PresencePaneRole::Working
    };
    PaneObservation {
        pane_id: PaneId::from(pane.native_id()),
        view: pane.tab_position.to_string(),
        command: pane
            .pane_command
            .as_deref()
            .filter(|command| !command.is_empty())
            .map(str::to_owned),
        role,
        events: zellij_event_eligibility(role),
    }
}

const fn zellij_event_eligibility(role: PresencePaneRole) -> PaneEventEligibility {
    match role {
        PresencePaneRole::Sidebar => PaneEventEligibility {
            open: false,
            ..PaneEventEligibility::ALL
        },
        PresencePaneRole::Working | PresencePaneRole::LaunchChrome => PaneEventEligibility::ALL,
    }
}

fn observation_for_id(cache: &PaneTopologyCache, pane_id: &PaneId) -> Option<PaneObservation> {
    let native = ZellijPaneId::try_from(pane_id).ok()?;
    cache
        .panes
        .iter()
        .find(|pane| pane.native_id() == native)
        .map(pane_observation)
}

fn switch_settled_transition(
    topology: &PaneTopologyCache,
    active_tab: u64,
    generation: u64,
    clients: &[crate::mux::ClientPaneView],
) -> Option<PresenceTransition> {
    let mut viewed = clients.iter().map(|client| &client.pane_id);
    let first = viewed.next()?;
    if viewed.any(|pane| pane != first) {
        return None;
    }
    match ZellijPaneId::try_from(first).ok()? {
        ZellijPaneId::Terminal(id) => {
            let pane = topology
                .panes
                .iter()
                .find(|pane| pane.native_id() == ZellijPaneId::Terminal(id))?;
            if !pane.is_live_terminal()
                || pane.tab_position == active_tab && !topology_pane_is_sidebar(pane)
            {
                return None;
            }
        }
        ZellijPaneId::Plugin(_) => {}
    }
    let mut sidebars = topology.panes.iter().filter(|pane| {
        pane.tab_position == active_tab && pane.is_live_terminal() && topology_pane_is_sidebar(pane)
    });
    let sidebar = sidebars.next()?;
    if sidebars.next().is_some()
        || !topology.panes.iter().any(|pane| {
            pane.tab_position == active_tab
                && pane.is_live_terminal()
                && !topology_pane_is_sidebar(pane)
        })
    {
        return None;
    }
    Some(PresenceTransition::ViewSwitched {
        focused: Some(pane_observation(sidebar)),
        prior: None,
        has_working: true,
        generation,
        clients: clients.to_vec(),
    })
}

fn topology_pane_is_sidebar(pane: &PaneTopologyPane) -> bool {
    !pane.is_plugin && pane.title.as_deref() == Some(SIDEBAR_CHROME_TITLE)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TopologyDecision {
    Accept,
    Reject,
}

fn topology_decision(
    existing: Option<&PaneTopologyCache>,
    incoming: &PaneTopologyCache,
    desired: Option<&PresenceDesired>,
    now_ms: u64,
) -> TopologyDecision {
    let Some(existing) = existing else {
        return TopologyDecision::Accept;
    };
    if !pane_topology_cache_is_fresh(existing, now_ms, None)
        || writer_rank(incoming.writer.as_ref(), desired)
            >= writer_rank(existing.writer.as_ref(), desired)
    {
        TopologyDecision::Accept
    } else {
        TopologyDecision::Reject
    }
}

fn writer_rank(
    writer: Option<&TopologyWriter>,
    desired: Option<&PresenceDesired>,
) -> (bool, u64, u32) {
    let matches_desired = desired.is_some_and(|desired| {
        writer.is_some_and(|writer| {
            writer.build.as_deref() == Some(desired.build.as_str())
                && writer.config.as_deref() == Some(desired.config.as_str())
        })
    });
    let (loaded_at_ms, plugin_id) = writer_generation(writer);
    (matches_desired, loaded_at_ms, plugin_id)
}

fn writer_generation(writer: Option<&TopologyWriter>) -> (u64, u32) {
    writer.map_or((0, 0), |writer| writer.generation())
}

fn emit_topology_writer_changed(
    state: &StatePaths,
    session_name: &str,
    prior: Option<&TopologyWriter>,
    incoming: Option<&TopologyWriter>,
) {
    let (prior_loaded_at_ms, prior_plugin_id) = writer_generation(prior);
    let (loaded_at_ms, plugin_id) = writer_generation(incoming);
    DiagSink::under(
        state.root.clone(),
        state.workspace_id.clone(),
        session_name,
        None,
    )
    .emit(DiagEvent::TopologyWriterChanged {
        prior_plugin_id,
        prior_loaded_at_ms,
        plugin_id,
        loaded_at_ms,
    });
}

fn record_topology_write_rejected(
    state: &StatePaths,
    runtime: &RuntimePaths,
    incoming: &PaneTopologyCache,
    existing: &PaneTopologyCache,
    now_ms: u64,
) -> Result<(), ZellijWakeError> {
    let mut conflict = read_topology_writer_conflict(runtime).unwrap_or_default();
    if conflict.stale_writer != incoming.writer || conflict.accepted_writer != existing.writer {
        conflict.rejected_count = 0;
    }
    conflict.stale_writer = incoming.writer.clone();
    conflict.accepted_writer = existing.writer.clone();
    conflict.rejected_count = conflict.rejected_count.saturating_add(1);
    conflict.last_ms = now_ms;
    let emit_diag = now_ms.saturating_sub(conflict.last_diag_ms) >= TOPOLOGY_CONFLICT_DIAG_MS;
    if emit_diag {
        conflict.last_diag_ms = now_ms;
    }
    write_topology_writer_conflict(runtime, &conflict).map_err(ZellijWakeError::ConflictWrite)?;
    if emit_diag {
        let (loaded_at_ms, plugin_id) = writer_generation(incoming.writer.as_ref());
        let (accepted_loaded_at_ms, accepted_plugin_id) =
            writer_generation(existing.writer.as_ref());
        DiagSink::under(
            state.root.clone(),
            state.workspace_id.clone(),
            &incoming.session_name,
            None,
        )
        .emit_unlimited(DiagEvent::TopologyWriteRejected {
            plugin_id,
            loaded_at_ms,
            accepted_plugin_id,
            accepted_loaded_at_ms,
            rejected_count: conflict.rejected_count,
        });
    }
    Ok(())
}

fn clear_superseded_conflict(
    runtime: &RuntimePaths,
    writer: Option<&TopologyWriter>,
    desired: Option<&PresenceDesired>,
) -> Result<(), ZellijWakeError> {
    let Some(conflict) = read_topology_writer_conflict(runtime) else {
        return Ok(());
    };
    if writer_rank(writer, desired) <= writer_rank(conflict.accepted_writer.as_ref(), desired) {
        return Ok(());
    }
    let path = topology_writer_conflict_path(runtime);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(ZellijWakeError::ConflictClear { path, source }),
    }
    Ok(())
}

fn sanitize_topology_cache(cache: &mut PaneTopologyCache) {
    for pane in &mut cache.panes {
        if pane
            .pane_command
            .as_deref()
            .is_some_and(command_is_launch_chrome)
        {
            pane.pane_command = None;
        }
    }
}

fn write_plugin_presence_sample(
    state: &StatePaths,
    session_name: Option<String>,
    telemetry: &ZellijPluginTelemetry,
) {
    crate::diag::plugin_presence::log(&state.root).append(&PluginPresenceSample {
        at_ms: unix_now_ms(),
        session_name,
        plugin_id: telemetry.plugin_id,
        build: telemetry.build.clone(),
        loaded_at_ms: telemetry.loaded_at_ms,
        pages: telemetry.pages,
        bytes: telemetry.pages.saturating_mul(WASM_PAGE_BYTES),
        uptime_ms: telemetry.uptime_ms,
        commands: telemetry.commands,
        commands_succeeded: telemetry.commands_succeeded,
        stale_writer_rejections: telemetry.stale_writer_rejections,
        topology_failures: telemetry.topology_failures,
        other_failures: telemetry.other_failures,
        zellij_version: telemetry.zellij_version.clone(),
        last_failure: telemetry.last_failure.clone(),
    });
}

pub(super) fn command_is_launch_chrome(command: &str) -> bool {
    let mut tokens = command.split_whitespace().filter(|token| !token.is_empty());
    let Some(program) = tokens.next() else {
        return false;
    };
    if program_basename(program) != "rimz" || tokens.next() != Some("agents") {
        return false;
    }
    let Some(spec_or_command) = tokens.next() else {
        return false;
    };
    !matches!(
        spec_or_command,
        "list" | "ls" | "show" | "focus" | "wait" | "stop" | "exec"
    )
}

fn program_basename(program: &str) -> &str {
    Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(program)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TopologyWriterConflict {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_writer: Option<TopologyWriter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_writer: Option<TopologyWriter>,
    #[serde(default)]
    pub rejected_count: u64,
    #[serde(default)]
    pub last_ms: u64,
    #[serde(default)]
    pub last_diag_ms: u64,
}

fn topology_writer_conflict_path(runtime: &RuntimePaths) -> std::path::PathBuf {
    runtime.root.join("topology-writer-conflict.json")
}

pub fn read_topology_writer_conflict(runtime: &RuntimePaths) -> Option<TopologyWriterConflict> {
    let bytes = std::fs::read(topology_writer_conflict_path(runtime)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_topology_writer_conflict(
    runtime: &RuntimePaths,
    conflict: &TopologyWriterConflict,
) -> crate::store::atomic::Result<()> {
    crate::store::atomic::write_temp_then_rename_cache(
        &topology_writer_conflict_path(runtime),
        conflict,
    )
}

#[cfg(test)]
mod tests;
