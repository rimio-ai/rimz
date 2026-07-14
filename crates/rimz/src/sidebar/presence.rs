//! Zellij presence-plugin wake ingestion.
//!
//! `rimz sidebar wake` normalizes the plugin payload at the CLI boundary, then
//! this module owns the accepted-or-rejected transaction: topology writer
//! fencing and publication, presence stamping, plugin telemetry, and event
//! mapping. A stale writer returns before any accepted-wake side effect.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::diag::DiagSink;
use crate::diag::plugin_presence::PluginPresenceSample;
use crate::diag::record::DiagEvent;
use crate::ids::PaneId;
use crate::mux::zellij::pane_topology::{PaneTopologyCache, TopologyWriter};
use crate::sidebar::cache::{
    pane_topology_cache_is_fresh, read_pane_topology_cache, write_pane_topology_cache,
    write_presence_stamp,
};
use crate::sidebar::events::SidebarEvent;
use crate::sidebar::timing::unix_now_ms;
use crate::{RuntimePaths, StatePaths};

const TOPOLOGY_CONFLICT_DIAG_MS: u64 = 60_000;

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
    pub pages: u64,
    pub uptime_ms: u64,
    pub commands: u64,
    pub commands_failed: u64,
    pub zellij_version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZellijWake {
    pub reason: ZellijWakeReason,
    pub session_name: Option<String>,
    pub pane_id: Option<PaneId>,
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

/// Apply one normalized Zellij presence poke as a single policy transaction.
pub fn ingest_zellij_wake(
    state: &StatePaths,
    runtime: &RuntimePaths,
    wake: &ZellijWake,
) -> ZellijWakeOutcome {
    if let Some(incoming) = wake.topology.as_ref() {
        let now_ms = unix_now_ms();
        let existing = read_pane_topology_cache(runtime, &incoming.session_name);
        if topology_decision(existing.as_ref(), incoming, now_ms) == TopologyDecision::Reject {
            // Reject is reachable only with a fresh same-session cache.
            if let Some(existing) = existing.as_ref() {
                record_topology_write_rejected(state, runtime, incoming, existing, now_ms);
            }
            return ZellijWakeOutcome::RejectedStaleWriter;
        }
        if let Some(existing) = existing.as_ref()
            && incoming.writer != existing.writer
        {
            emit_topology_writer_changed(
                state,
                &incoming.session_name,
                existing.writer,
                incoming.writer,
            );
        }
        let mut cache = incoming.clone();
        sanitize_topology_cache(&mut cache);
        if let Err(err) = write_pane_topology_cache(runtime, &cache) {
            tracing::debug!(error = %err, "presence poke: topology cache write failed");
        }
    }

    write_presence_stamp(runtime);
    if let Some(telemetry) = wake.telemetry.as_ref() {
        write_plugin_presence_sample(state, wake.session_name.clone(), telemetry);
    }
    ZellijWakeOutcome::Accepted(wake_event(wake))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TopologyDecision {
    Accept,
    Reject,
}

fn topology_decision(
    existing: Option<&PaneTopologyCache>,
    incoming: &PaneTopologyCache,
    now_ms: u64,
) -> TopologyDecision {
    let Some(existing) = existing else {
        return TopologyDecision::Accept;
    };
    if !pane_topology_cache_is_fresh(existing, now_ms, None)
        || writer_generation(incoming.writer) >= writer_generation(existing.writer)
    {
        TopologyDecision::Accept
    } else {
        TopologyDecision::Reject
    }
}

fn writer_generation(writer: Option<TopologyWriter>) -> (u64, u32) {
    writer.map_or((0, 0), |writer| (writer.loaded_at_ms, writer.plugin_id))
}

fn emit_topology_writer_changed(
    state: &StatePaths,
    session_name: &str,
    prior: Option<TopologyWriter>,
    incoming: Option<TopologyWriter>,
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
) {
    let mut conflict = read_topology_writer_conflict(runtime).unwrap_or_default();
    conflict.stale_writer = incoming.writer;
    conflict.accepted_writer = existing.writer;
    conflict.rejected_count = conflict.rejected_count.saturating_add(1);
    conflict.last_ms = now_ms;
    let emit_diag = now_ms.saturating_sub(conflict.last_diag_ms) >= TOPOLOGY_CONFLICT_DIAG_MS;
    if emit_diag {
        conflict.last_diag_ms = now_ms;
    }
    if let Err(err) = write_topology_writer_conflict(runtime, &conflict) {
        tracing::debug!(error = %err, "presence poke: topology writer conflict write failed");
    }
    if emit_diag {
        let (loaded_at_ms, plugin_id) = writer_generation(incoming.writer);
        let (accepted_loaded_at_ms, accepted_plugin_id) = writer_generation(existing.writer);
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
    crate::diag::plugin_presence::log(&state.root).append(&PluginPresenceSample::new(
        unix_now_ms(),
        session_name,
        telemetry.pages,
        telemetry.uptime_ms,
        telemetry.commands,
        telemetry.commands_failed,
        telemetry.zellij_version.clone(),
    ));
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
        ZellijWakeReason::FocusStranded => {
            wake.pane_id
                .as_ref()
                .map(|pane_id| SidebarEvent::FocusStranded {
                    pane_id: pane_id.clone(),
                })
        }
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

    #[test]
    fn generation_classification_fences_only_fresh_older_writers() {
        let now_ms = crate::sidebar::timing::PRESENCE_STAMP_FRESH.as_millis() as u64 + 100_000;
        let fresh = topology(now_ms, Some(writer(2, 200)));
        let stale = topology(0, Some(writer(2, 200)));
        let older = topology(now_ms, Some(writer(1, 100)));
        let newer = topology(now_ms, Some(writer(3, 300)));

        assert_eq!(
            topology_decision(None, &older, now_ms),
            TopologyDecision::Accept
        );
        assert_eq!(
            topology_decision(Some(&fresh), &older, now_ms),
            TopologyDecision::Reject,
        );
        assert_eq!(
            topology_decision(Some(&fresh), &newer, now_ms),
            TopologyDecision::Accept,
        );
        assert_eq!(
            topology_decision(Some(&stale), &older, now_ms),
            TopologyDecision::Accept,
        );
    }

    #[test]
    fn legacy_writer_zero_remains_a_valid_generation() {
        let now_ms = 100_000;
        let legacy = topology(now_ms, None);
        assert_eq!(
            topology_decision(Some(&legacy), &legacy, now_ms),
            TopologyDecision::Accept,
        );
        assert_eq!(writer_generation(None), (0, 0));
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
}
