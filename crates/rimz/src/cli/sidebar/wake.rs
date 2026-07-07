use super::*;

pub(super) fn write_topology_cache(runtime: &RuntimePaths, topology: Option<&str>) {
    let Some(topology) = topology else {
        return;
    };
    match serde_json::from_str::<rimz::mux::zellij::pane_topology::PaneTopologyCache>(topology) {
        Ok(mut cache) => {
            sanitize_topology_cache(&mut cache);
            if !topology_write_is_accepted(runtime, &cache) {
                return;
            }
            if let Err(err) = rimz::sidebar::cache::write_pane_topology_cache(runtime, &cache) {
                tracing::debug!(error = %err, "presence poke: topology cache write failed");
            }
        }
        Err(err) => {
            tracing::debug!(error = %err, "presence poke: topology payload parse failed");
        }
    }
}

const TOPOLOGY_CONFLICT_DIAG_MS: u64 = 60_000;

fn topology_write_is_accepted(
    runtime: &RuntimePaths,
    incoming: &rimz::mux::zellij::pane_topology::PaneTopologyCache,
) -> bool {
    let now_ms = rimz::sidebar::timing::unix_now_ms();
    let Some(existing) =
        rimz::sidebar::cache::read_pane_topology_cache(runtime, &incoming.session_name)
    else {
        return true;
    };
    if !rimz::sidebar::cache::pane_topology_cache_is_fresh(&existing, now_ms, None) {
        if incoming.writer != existing.writer {
            emit_topology_writer_changed(
                runtime,
                &incoming.session_name,
                existing.writer,
                incoming.writer,
            );
        }
        return true;
    }
    if writer_generation(incoming.writer) >= writer_generation(existing.writer) {
        if incoming.writer != existing.writer {
            emit_topology_writer_changed(
                runtime,
                &incoming.session_name,
                existing.writer,
                incoming.writer,
            );
        }
        return true;
    }
    record_topology_write_rejected(runtime, incoming, &existing, now_ms);
    false
}

fn writer_generation(
    writer: Option<rimz::mux::zellij::pane_topology::TopologyWriter>,
) -> (u64, u32) {
    writer.map_or((0, 0), |writer| (writer.loaded_at_ms, writer.plugin_id))
}

fn emit_topology_writer_changed(
    runtime: &RuntimePaths,
    session_name: &str,
    prior: Option<rimz::mux::zellij::pane_topology::TopologyWriter>,
    incoming: Option<rimz::mux::zellij::pane_topology::TopologyWriter>,
) {
    let (prior_loaded_at_ms, prior_plugin_id) = writer_generation(prior);
    let (loaded_at_ms, plugin_id) = writer_generation(incoming);
    rimz::diag::DiagSink::for_workspace(runtime.workspace_id.clone(), session_name, None).emit(
        rimz::diag::record::DiagEvent::TopologyWriterChanged {
            prior_plugin_id,
            prior_loaded_at_ms,
            plugin_id,
            loaded_at_ms,
        },
    );
}

fn record_topology_write_rejected(
    runtime: &RuntimePaths,
    incoming: &rimz::mux::zellij::pane_topology::PaneTopologyCache,
    existing: &rimz::mux::zellij::pane_topology::PaneTopologyCache,
    now_ms: u64,
) {
    let mut conflict =
        rimz::sidebar::cache::read_topology_writer_conflict(runtime).unwrap_or_default();
    conflict.stale_writer = incoming.writer;
    conflict.accepted_writer = existing.writer;
    conflict.rejected_count = conflict.rejected_count.saturating_add(1);
    conflict.last_ms = now_ms;
    let emit_diag = now_ms.saturating_sub(conflict.last_diag_ms) >= TOPOLOGY_CONFLICT_DIAG_MS;
    if emit_diag {
        conflict.last_diag_ms = now_ms;
    }
    if let Err(err) = rimz::sidebar::cache::write_topology_writer_conflict(runtime, &conflict) {
        tracing::debug!(error = %err, "presence poke: topology writer conflict write failed");
    }
    if emit_diag {
        let (loaded_at_ms, plugin_id) = writer_generation(incoming.writer);
        let (accepted_loaded_at_ms, accepted_plugin_id) = writer_generation(existing.writer);
        rimz::diag::DiagSink::for_workspace(
            runtime.workspace_id.clone(),
            &incoming.session_name,
            None,
        )
        .emit_unlimited(rimz::diag::record::DiagEvent::TopologyWriteRejected {
            plugin_id,
            loaded_at_ms,
            accepted_plugin_id,
            accepted_loaded_at_ms,
            rejected_count: conflict.rejected_count,
        });
    }
}

fn sanitize_topology_cache(cache: &mut rimz::mux::zellij::pane_topology::PaneTopologyCache) {
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

/// Map a poke reason onto its typed event. `None` means the poke carries no
/// event of its own (`alive` is stamp-only). Producer-verifying pane reasons
/// missing their pane data degrade to the identity-free `PanesChanged` nudge,
/// so a sparse poke still triggers the producer's verifying pull.
pub(super) fn wake_event(
    reason: WakeReason,
    pane_id: Option<&str>,
    command_args: &[String],
    focused_pane_ids: &[String],
    unfocused_pane_ids: &[String],
) -> Option<SidebarEvent> {
    let zellij_pane = |raw: &str| rimz::ids::PaneId::from_parts(rimz::ids::MuxName::Zellij, raw);
    match reason {
        WakeReason::Alive => None,
        WakeReason::PanesChanged => Some(SidebarEvent::PanesChanged),
        WakeReason::PaneOpened => Some(match pane_id {
            Some(pane_id) => SidebarEvent::PaneOpened {
                pane_id: zellij_pane(pane_id),
                command: command_from_args(command_args)
                    .filter(|command| !command_is_launch_chrome(command)),
            },
            None => SidebarEvent::PanesChanged,
        }),
        WakeReason::PaneClosed => Some(match pane_id {
            Some(pane_id) => SidebarEvent::PaneClosed {
                pane_id: zellij_pane(pane_id),
            },
            None => SidebarEvent::PanesChanged,
        }),
        WakeReason::FocusStranded => pane_id.map(|pane_id| SidebarEvent::FocusStranded {
            pane_id: zellij_pane(pane_id),
        }),
        WakeReason::CommandChanged => Some(match pane_id.zip(command_from_args(command_args)) {
            Some((_pane_id, command)) if command_is_launch_chrome(&command) => {
                SidebarEvent::PanesChanged
            }
            Some((pane_id, command)) => SidebarEvent::CommandChanged {
                pane_id: zellij_pane(pane_id),
                command,
            },
            None => SidebarEvent::PanesChanged,
        }),
        WakeReason::FocusChanged => Some(SidebarEvent::FocusChanged {
            focused: zellij_pane_ids(focused_pane_ids),
            unfocused: zellij_pane_ids(unfocused_pane_ids),
        }),
    }
}

fn zellij_pane_ids(raws: &[String]) -> Vec<rimz::ids::PaneId> {
    raws.iter()
        .filter(|raw| !raw.is_empty())
        .map(|raw| rimz::ids::PaneId::from_parts(rimz::ids::MuxName::Zellij, raw))
        .collect()
}

fn command_from_args(args: &[String]) -> Option<String> {
    let command = args
        .iter()
        .filter(|arg| !arg.is_empty())
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    (!command.is_empty()).then_some(command)
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
    std::path::Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(program)
}

pub(super) fn session_name_from_record(state: &StatePaths) -> Option<String> {
    workspace_record::read(&state.workspace_record)
        .ok()
        .map(|record| record.session_name)
}

pub(crate) fn rimz_cli_program() -> PathBuf {
    rimz::proc::rimz_exe()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zellij_pane(raw: &str) -> rimz::ids::PaneId {
        rimz::ids::PaneId::from_parts(rimz::ids::MuxName::Zellij, raw)
    }

    fn writer(
        plugin_id: u32,
        loaded_at_ms: u64,
    ) -> rimz::mux::zellij::pane_topology::TopologyWriter {
        rimz::mux::zellij::pane_topology::TopologyWriter {
            plugin_id,
            loaded_at_ms,
        }
    }

    fn topology_json(
        produced_at_ms: u64,
        writer: Option<rimz::mux::zellij::pane_topology::TopologyWriter>,
    ) -> String {
        serde_json::to_string(&rimz::mux::zellij::pane_topology::PaneTopologyCache {
            session_name: "rimz-test".to_owned(),
            produced_at_ms,
            writer,
            focused_pane: None,
            panes: Vec::new(),
        })
        .expect("topology serializes")
    }

    fn runtime() -> (tempfile::TempDir, RuntimePaths) {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_id = rimz::ids::WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace_id, dir.path()).expect("runtime");
        runtime.ensure_dirs().expect("runtime dirs");
        (dir, runtime)
    }

    #[test]
    fn topology_gate_rejects_older_writer_while_cache_is_fresh() {
        let (_dir, runtime) = runtime();
        write_topology_cache(
            &runtime,
            Some(&topology_json(
                rimz::sidebar::timing::unix_now_ms(),
                Some(writer(2, 200)),
            )),
        );

        write_topology_cache(
            &runtime,
            Some(&topology_json(
                rimz::sidebar::timing::unix_now_ms(),
                Some(writer(1, 100)),
            )),
        );

        let cache = rimz::sidebar::cache::read_pane_topology_cache(&runtime, "rimz-test")
            .expect("topology cache");
        assert_eq!(cache.writer, Some(writer(2, 200)));
        let conflict = rimz::sidebar::cache::read_topology_writer_conflict(&runtime)
            .expect("conflict sidecar");
        assert_eq!(conflict.rejected_count, 1);
        assert_eq!(conflict.stale_writer, Some(writer(1, 100)));
        assert_eq!(conflict.accepted_writer, Some(writer(2, 200)));
    }

    #[test]
    fn topology_gate_accepts_older_writer_when_cache_is_stale() {
        let (_dir, runtime) = runtime();
        let stale_at = rimz::sidebar::timing::unix_now_ms()
            .saturating_sub(rimz::sidebar::timing::PRESENCE_STAMP_FRESH.as_millis() as u64 + 1);
        write_topology_cache(
            &runtime,
            Some(&topology_json(stale_at, Some(writer(2, 200)))),
        );

        write_topology_cache(
            &runtime,
            Some(&topology_json(stale_at + 1, Some(writer(1, 100)))),
        );

        let cache = rimz::sidebar::cache::read_pane_topology_cache(&runtime, "rimz-test")
            .expect("topology cache");
        assert_eq!(cache.writer, Some(writer(1, 100)));
        assert!(rimz::sidebar::cache::read_topology_writer_conflict(&runtime).is_none());
    }

    #[test]
    fn topology_gate_accepts_legacy_writer_over_legacy_cache() {
        let (_dir, runtime) = runtime();
        write_topology_cache(
            &runtime,
            Some(&topology_json(rimz::sidebar::timing::unix_now_ms(), None)),
        );
        write_topology_cache(
            &runtime,
            Some(&topology_json(
                rimz::sidebar::timing::unix_now_ms().saturating_add(1),
                None,
            )),
        );

        let cache = rimz::sidebar::cache::read_pane_topology_cache(&runtime, "rimz-test")
            .expect("topology cache");
        assert_eq!(cache.writer, None);
        assert!(rimz::sidebar::cache::read_topology_writer_conflict(&runtime).is_none());
    }

    #[test]
    fn launch_chrome_is_agents_launch_not_agents_subcommand() {
        assert!(command_is_launch_chrome(
            "rimz agents claude,codex --worktree=quality-pass"
        ));
        assert!(command_is_launch_chrome(
            "/home/me/.cargo/bin/rimz agents claude --worktree"
        ));
        assert!(command_is_launch_chrome(
            "/home/me/.cargo/bin/rimz agents claude,codex --worktree=quality-pass"
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
    fn pane_opened_strips_launch_chrome_command() {
        assert_eq!(
            wake_event(
                WakeReason::PaneOpened,
                Some("terminal_7"),
                &["rimz agents claude,codex --worktree=quality-pass".to_owned()],
                &[],
                &[],
            ),
            Some(SidebarEvent::PaneOpened {
                pane_id: zellij_pane("terminal_7"),
                command: None,
            }),
        );
        assert_eq!(
            wake_event(
                WakeReason::PaneOpened,
                Some("terminal_8"),
                &["codex".to_owned()],
                &[],
                &[],
            ),
            Some(SidebarEvent::PaneOpened {
                pane_id: zellij_pane("terminal_8"),
                command: Some("codex".to_owned()),
            }),
        );
    }
}
