//! Zellij presence-plugin wake ingestion.
//!
//! `rimz sidebar wake` normalizes the plugin payload at the CLI boundary, then
//! this module owns the accepted-or-rejected transaction: topology writer
//! fencing and publication, presence stamping, plugin telemetry, and event
//! mapping. A stale writer returns before any accepted-wake side effect.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::diag::DiagSink;
use crate::diag::plugin_presence::{PluginPresenceSample, WASM_PAGE_BYTES};
use crate::diag::record::DiagEvent;
use crate::ids::{MuxName, PaneId};
use crate::mux::zellij::pane_topology::{PaneTopologyCache, TopologyWriter};
use crate::sidebar::cache::{
    PresenceDesired, pane_topology_cache_is_fresh, read_pane_topology_cache, read_presence_desired,
    write_pane_topology_cache, write_presence_stamp,
};
use crate::sidebar::events::SidebarEvent;
use crate::sidebar::timing::unix_now_ms;
use crate::{RuntimePaths, StatePaths};

const TOPOLOGY_CONFLICT_DIAG_MS: u64 = 60_000;
/// Private `rimz sidebar wake` status consumed by the Zellij plugin. Three
/// consecutive publishes rejected with this code retire the losing writer.
pub const STALE_WRITER_EXIT_CODE: i32 = 73;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZellijWakeReason {
    PanesChanged,
    PaneOpened,
    PaneClosed,
    FocusStranded,
    CommandChanged,
    FocusChanged,
    Alive,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZellijPluginTelemetry {
    pub plugin_id: Option<u32>,
    pub build: Option<String>,
    pub loaded_at_ms: u64,
    pub pages: u64,
    pub uptime_ms: u64,
    pub commands: u64,
    pub commands_succeeded: Option<u64>,
    pub commands_failed: u64,
    pub stale_writer_rejections: Option<u64>,
    pub topology_failures: Option<u64>,
    pub other_failures: Option<u64>,
    pub zellij_version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZellijWake {
    pub reason: ZellijWakeReason,
    pub session_name: Option<String>,
    pub pane_id: Option<PaneId>,
    pub focus_generation: Option<u64>,
    pub focus_clients: Vec<crate::mux::ClientPaneView>,
    pub command: Option<String>,
    pub focused_pane_ids: Vec<PaneId>,
    pub unfocused_pane_ids: Vec<PaneId>,
    pub topology: Option<PaneTopologyCache>,
    pub telemetry: Option<ZellijPluginTelemetry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ZellijWakeOutcome {
    RejectedStaleWriter,
    Accepted(Option<SidebarEvent>),
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
        write_pane_topology_cache(runtime, &cache).map_err(ZellijWakeError::TopologyWrite)?;
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
    Ok(ZellijWakeOutcome::Accepted(wake_event(wake)))
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
        commands_failed: telemetry.commands_failed,
        stale_writer_rejections: telemetry.stale_writer_rejections,
        topology_failures: telemetry.topology_failures,
        other_failures: telemetry.other_failures,
        zellij_version: telemetry.zellij_version.clone(),
    });
}

/// Map a poke reason onto its typed event. `None` means the poke carries no
/// event of its own (`alive` is stamp-only). Producer-verifying pane reasons
/// missing their pane data degrade to the identity-free `PanesChanged` nudge,
/// so a sparse poke still triggers the producer's verifying pull.
fn wake_event(wake: &ZellijWake) -> Option<SidebarEvent> {
    match wake.reason {
        ZellijWakeReason::Alive => None,
        ZellijWakeReason::PanesChanged => Some(SidebarEvent::PanesChanged),
        ZellijWakeReason::PaneOpened => Some(match wake.pane_id.as_ref() {
            Some(pane_id) => SidebarEvent::PaneOpened {
                pane_id: pane_id.clone(),
                command: wake
                    .command
                    .clone()
                    .filter(|command| !command_is_launch_chrome(command)),
            },
            None => SidebarEvent::PanesChanged,
        }),
        ZellijWakeReason::PaneClosed => Some(match wake.pane_id.as_ref() {
            Some(pane_id) => SidebarEvent::PaneClosed {
                pane_id: pane_id.clone(),
            },
            None => SidebarEvent::PanesChanged,
        }),
        ZellijWakeReason::FocusStranded => wake.pane_id.as_ref().and_then(|pane_id| {
            wake.focus_generation
                .map(|generation| SidebarEvent::FocusStranded {
                    pane_id: pane_id.clone(),
                    generation,
                    clients: wake.focus_clients.clone(),
                })
        }),
        ZellijWakeReason::CommandChanged => Some(match (&wake.pane_id, &wake.command) {
            (Some(_), Some(command)) if command_is_launch_chrome(command) => {
                SidebarEvent::PanesChanged
            }
            (Some(pane_id), Some(command)) => SidebarEvent::CommandChanged {
                pane_id: pane_id.clone(),
                command: command.clone(),
            },
            _ => SidebarEvent::PanesChanged,
        }),
        ZellijWakeReason::FocusChanged => Some(SidebarEvent::FocusChanged {
            focused: wake.focused_pane_ids.clone(),
            unfocused: wake.unfocused_pane_ids.clone(),
        }),
    }
}

fn command_is_launch_chrome(command: &str) -> bool {
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
mod tests {
    use super::*;
    use crate::ids::MuxName;

    fn writer(plugin_id: u32, loaded_at_ms: u64) -> TopologyWriter {
        TopologyWriter {
            plugin_id,
            loaded_at_ms,
            build: None,
            config: None,
        }
    }

    fn identified_writer(
        plugin_id: u32,
        loaded_at_ms: u64,
        build: &str,
        config: &str,
    ) -> TopologyWriter {
        TopologyWriter {
            plugin_id,
            loaded_at_ms,
            build: Some(build.to_owned()),
            config: Some(config.to_owned()),
        }
    }

    fn desired() -> PresenceDesired {
        PresenceDesired {
            build: "desired-build".to_owned(),
            config: "desired-config".to_owned(),
            recorded_at_ms: 1,
        }
    }

    fn topology(produced_at_ms: u64, writer: Option<TopologyWriter>) -> PaneTopologyCache {
        PaneTopologyCache {
            session_name: "rimz-test".to_owned(),
            produced_at_ms,
            writer,
            focused_pane: None,
            clients: None,
            panes: Vec::new(),
        }
    }

    fn wake(reason: ZellijWakeReason) -> ZellijWake {
        ZellijWake {
            reason,
            session_name: Some("rimz-test".to_owned()),
            pane_id: None,
            focus_generation: None,
            focus_clients: Vec::new(),
            command: None,
            focused_pane_ids: Vec::new(),
            unfocused_pane_ids: Vec::new(),
            topology: None,
            telemetry: None,
        }
    }

    fn zellij_pane(raw: &str) -> PaneId {
        PaneId::from_parts(MuxName::Zellij, raw)
    }

    fn paths() -> (tempfile::TempDir, StatePaths, RuntimePaths) {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = crate::WorkspaceId::from_project_root(dir.path());
        let state = StatePaths::under(workspace.clone(), dir.path()).expect("state paths");
        let runtime = RuntimePaths::under(workspace, dir.path()).expect("runtime paths");
        state.ensure_dirs().expect("state dirs");
        runtime.ensure_dirs().expect("runtime dirs");
        (dir, state, runtime)
    }

    #[test]
    fn generation_classification_fences_only_fresh_older_writers() {
        let now_ms = crate::sidebar::timing::PRESENCE_STAMP_FRESH.as_millis() as u64 + 100_000;
        let fresh = topology(now_ms, Some(writer(2, 200)));
        let stale = topology(0, Some(writer(2, 200)));
        let older = topology(now_ms, Some(writer(1, 100)));
        let newer = topology(now_ms, Some(writer(3, 300)));

        assert_eq!(
            topology_decision(None, &older, None, now_ms),
            TopologyDecision::Accept
        );
        assert_eq!(
            topology_decision(Some(&fresh), &older, None, now_ms),
            TopologyDecision::Reject,
        );
        assert_eq!(
            topology_decision(Some(&fresh), &newer, None, now_ms),
            TopologyDecision::Accept,
        );
        assert_eq!(
            topology_decision(Some(&stale), &older, None, now_ms),
            TopologyDecision::Accept,
        );
    }

    #[test]
    fn desired_identity_outranks_later_nonmatching_writers() {
        let now_ms = 100_000;
        let desired = desired();
        let accepted = topology(
            now_ms,
            Some(identified_writer(1, 100, &desired.build, &desired.config)),
        );
        let later_other = topology(
            now_ms,
            Some(identified_writer(2, 200, "other-build", "other-config")),
        );

        assert_eq!(
            topology_decision(Some(&accepted), &later_other, Some(&desired), now_ms),
            TopologyDecision::Reject,
        );
        assert_eq!(
            topology_decision(Some(&later_other), &later_other, Some(&desired), now_ms),
            TopologyDecision::Accept,
            "a sole nonmatching writer keeps refreshing its cache",
        );
        assert_eq!(
            topology_decision(Some(&later_other), &accepted, Some(&desired), now_ms),
            TopologyDecision::Accept,
        );
    }

    #[test]
    fn desired_record_fences_a_later_nonmatching_wake() {
        let (_dir, state, runtime) = paths();
        let desired = desired();
        crate::sidebar::cache::write_presence_desired(&runtime, &desired).unwrap();
        let produced_at_ms = unix_now_ms();
        let accepted = topology(
            produced_at_ms,
            Some(identified_writer(1, 100, &desired.build, &desired.config)),
        );
        write_pane_topology_cache(&runtime, &accepted).unwrap();
        let mut incoming = wake(ZellijWakeReason::Alive);
        incoming.topology = Some(topology(
            produced_at_ms,
            Some(identified_writer(2, 200, "other-build", "other-config")),
        ));

        assert_eq!(
            ingest_zellij_wake(&state, &runtime, &incoming).unwrap(),
            ZellijWakeOutcome::RejectedStaleWriter,
        );
        assert_eq!(
            read_pane_topology_cache(&runtime, "rimz-test"),
            Some(accepted),
        );
    }

    #[test]
    fn legacy_writer_zero_remains_a_valid_generation() {
        let now_ms = 100_000;
        let legacy = topology(now_ms, None);
        assert_eq!(
            topology_decision(Some(&legacy), &legacy, None, now_ms),
            TopologyDecision::Accept,
        );
        assert_eq!(writer_generation(None), (0, 0));
    }

    #[test]
    fn writer_conflict_count_restarts_when_the_incident_changes() {
        let (_dir, state, runtime) = paths();
        write_topology_writer_conflict(
            &runtime,
            &TopologyWriterConflict {
                stale_writer: Some(writer(1, 100)),
                accepted_writer: Some(writer(2, 200)),
                rejected_count: 7,
                last_ms: 800,
                last_diag_ms: 500,
            },
        )
        .expect("seed writer conflict");
        let incoming = topology(900, Some(writer(3, 300)));
        let existing = topology(900, Some(writer(4, 400)));

        record_topology_write_rejected(&state, &runtime, &incoming, &existing, 1_000).unwrap();
        let conflict = read_topology_writer_conflict(&runtime).expect("updated conflict");
        assert_eq!(conflict.rejected_count, 1);
        assert_eq!(
            conflict.last_diag_ms, 500,
            "diagnostic throttle spans incidents"
        );

        record_topology_write_rejected(&state, &runtime, &incoming, &existing, 1_001).unwrap();
        assert_eq!(
            read_topology_writer_conflict(&runtime)
                .expect("updated conflict")
                .rejected_count,
            2,
        );
    }

    #[test]
    fn newer_accepted_writer_clears_a_superseded_conflict() {
        let (_dir, state, runtime) = paths();
        let produced_at_ms = unix_now_ms();
        let existing = topology(produced_at_ms, Some(writer(2, 200)));
        write_pane_topology_cache(&runtime, &existing).expect("seed topology cache");
        write_topology_writer_conflict(
            &runtime,
            &TopologyWriterConflict {
                stale_writer: Some(writer(1, 100)),
                accepted_writer: existing.writer,
                rejected_count: 3,
                last_ms: produced_at_ms,
                last_diag_ms: produced_at_ms,
            },
        )
        .expect("seed writer conflict");
        let mut accepted = wake(ZellijWakeReason::Alive);
        accepted.topology = Some(topology(produced_at_ms, Some(writer(3, 300))));

        assert_eq!(
            ingest_zellij_wake(&state, &runtime, &accepted).unwrap(),
            ZellijWakeOutcome::Accepted(None),
        );
        assert!(read_topology_writer_conflict(&runtime).is_none());
    }

    #[test]
    fn equal_or_older_writer_keeps_the_conflict_sidecar() {
        let (_dir, _state, runtime) = paths();
        let conflict = TopologyWriterConflict {
            stale_writer: Some(writer(1, 100)),
            accepted_writer: Some(writer(2, 200)),
            rejected_count: 3,
            last_ms: 300,
            last_diag_ms: 300,
        };
        write_topology_writer_conflict(&runtime, &conflict).expect("seed writer conflict");

        clear_superseded_conflict(&runtime, Some(&writer(2, 200)), None).unwrap();
        assert!(read_topology_writer_conflict(&runtime).is_some());
        clear_superseded_conflict(&runtime, Some(&writer(9, 100)), None).unwrap();
        assert!(read_topology_writer_conflict(&runtime).is_some());
    }

    #[test]
    fn desired_writer_clears_a_newer_nonmatching_conflict() {
        let (_dir, _state, runtime) = paths();
        let desired = desired();
        write_topology_writer_conflict(
            &runtime,
            &TopologyWriterConflict {
                stale_writer: None,
                accepted_writer: Some(identified_writer(2, 200, "other-build", "other-config")),
                rejected_count: 3,
                last_ms: 300,
                last_diag_ms: 300,
            },
        )
        .expect("seed writer conflict");
        let matching = identified_writer(1, 100, &desired.build, &desired.config);

        clear_superseded_conflict(&runtime, Some(&matching), Some(&desired)).unwrap();

        assert!(read_topology_writer_conflict(&runtime).is_none());
    }

    #[test]
    fn sparse_event_inputs_keep_their_fallbacks() {
        for reason in [
            ZellijWakeReason::PaneOpened,
            ZellijWakeReason::PaneClosed,
            ZellijWakeReason::CommandChanged,
        ] {
            assert_eq!(wake_event(&wake(reason)), Some(SidebarEvent::PanesChanged));
        }
        assert_eq!(wake_event(&wake(ZellijWakeReason::FocusStranded)), None);
        assert_eq!(wake_event(&wake(ZellijWakeReason::Alive)), None);
    }

    #[test]
    fn normalized_command_and_focus_inputs_pass_through() {
        let mut command = wake(ZellijWakeReason::CommandChanged);
        command.pane_id = Some(zellij_pane("terminal_7"));
        command.command = Some("codex --search".to_owned());
        assert_eq!(
            wake_event(&command),
            Some(SidebarEvent::CommandChanged {
                pane_id: zellij_pane("terminal_7"),
                command: "codex --search".to_owned(),
            }),
        );

        let mut focus = wake(ZellijWakeReason::FocusChanged);
        focus.focused_pane_ids = vec![zellij_pane("terminal_8")];
        focus.unfocused_pane_ids = vec![zellij_pane("terminal_7")];
        assert_eq!(
            wake_event(&focus),
            Some(SidebarEvent::FocusChanged {
                focused: vec![zellij_pane("terminal_8")],
                unfocused: vec![zellij_pane("terminal_7")],
            }),
        );
    }

    #[test]
    fn launch_chrome_is_agents_launch_not_agents_subcommand() {
        assert!(command_is_launch_chrome(
            "rimz agents claude,codex --worktree=quality-pass"
        ));
        assert!(command_is_launch_chrome(
            "/home/me/.cargo/bin/rimz agents claude --worktree"
        ));
        assert!(!command_is_launch_chrome("cargo build"));
        assert!(!command_is_launch_chrome("rimz agents exec codex"));
        assert!(!command_is_launch_chrome("rimz agents wait swift-otter"));
        assert!(!command_is_launch_chrome("rimz agents list"));
        assert!(!command_is_launch_chrome("rimz agents ls"));
        assert!(!command_is_launch_chrome("rimz agents show swift-otter"));
        assert!(!command_is_launch_chrome("rimz agents focus swift-otter"));
        assert!(!command_is_launch_chrome("rimz agents stop swift-otter"));
    }

    #[test]
    fn launch_chrome_events_keep_existing_fallbacks() {
        let launch = "rimz agents claude,codex --worktree=quality-pass".to_owned();
        let mut opened = wake(ZellijWakeReason::PaneOpened);
        opened.pane_id = Some(zellij_pane("terminal_7"));
        opened.command = Some(launch.clone());
        assert_eq!(
            wake_event(&opened),
            Some(SidebarEvent::PaneOpened {
                pane_id: zellij_pane("terminal_7"),
                command: None,
            }),
        );

        let mut changed = wake(ZellijWakeReason::CommandChanged);
        changed.pane_id = Some(zellij_pane("terminal_7"));
        changed.command = Some(launch);
        assert_eq!(wake_event(&changed), Some(SidebarEvent::PanesChanged));
    }

    #[test]
    fn topology_write_failure_returns_error_before_accepted_side_effects() {
        let (_dir, state, runtime) = paths();
        std::fs::create_dir_all(crate::sidebar::cache::pane_topology_cache_path(&runtime)).unwrap();
        let mut incoming = wake(ZellijWakeReason::Alive);
        incoming.topology = Some(topology(unix_now_ms(), Some(writer(2, 200))));
        incoming.telemetry = Some(ZellijPluginTelemetry {
            plugin_id: Some(2),
            build: Some("wasm-build".to_owned()),
            loaded_at_ms: 200,
            pages: 1,
            uptime_ms: 1,
            commands: 1,
            commands_succeeded: Some(1),
            commands_failed: 0,
            stale_writer_rejections: Some(0),
            topology_failures: Some(0),
            other_failures: Some(0),
            zellij_version: Some("0.44.3".to_owned()),
        });

        assert!(matches!(
            ingest_zellij_wake(&state, &runtime, &incoming),
            Err(ZellijWakeError::TopologyWrite(_))
        ));
        assert!(!crate::sidebar::cache::presence_stamp_path(&runtime).exists());
        assert!(
            !crate::diag::plugin_presence::log(&state.root)
                .path()
                .exists()
        );
    }

    #[test]
    fn topology_writer_lock_contention_returns_typed_timeout() {
        let (_dir, state, runtime) = paths();
        let _held =
            crate::store::lock::WorkspaceLock::acquire(&runtime.topology_writer_lock()).unwrap();
        let mut incoming = wake(ZellijWakeReason::Alive);
        incoming.topology = Some(topology(unix_now_ms(), Some(writer(2, 200))));

        assert!(matches!(
            ingest_zellij_wake(&state, &runtime, &incoming),
            Err(ZellijWakeError::TopologyLock(
                crate::store::lock::LockErr::Timeout { .. }
            ))
        ));
    }

    #[test]
    fn concurrent_topology_writers_finish_on_newest_generation() {
        for round in 0..16 {
            let (_dir, state, runtime) = paths();
            let at_ms = unix_now_ms();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
            let run = |plugin_id, loaded_at_ms| {
                let state = state.clone();
                let runtime = runtime.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut incoming = wake(ZellijWakeReason::Alive);
                    incoming.topology = Some(topology(
                        at_ms,
                        Some(writer(plugin_id, loaded_at_ms + round)),
                    ));
                    barrier.wait();
                    ingest_zellij_wake(&state, &runtime, &incoming)
                })
            };
            let older = run(1, 100);
            let newer = run(2, 200);
            barrier.wait();
            older.join().unwrap().unwrap();
            newer.join().unwrap().unwrap();

            assert_eq!(
                read_pane_topology_cache(&runtime, "rimz-test")
                    .unwrap()
                    .writer,
                Some(writer(2, 200 + round))
            );
        }
    }
}
