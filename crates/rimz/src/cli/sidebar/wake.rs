use super::*;

pub(super) fn write_topology_cache(runtime: &RuntimePaths, topology: Option<&str>) {
    let Some(topology) = topology else {
        return;
    };
    match serde_json::from_str::<rimz::mux::zellij::pane_topology::PaneTopologyCache>(topology) {
        Ok(mut cache) => {
            sanitize_topology_cache(&mut cache);
            if let Err(err) = rimz::sidebar::cache::write_pane_topology_cache(runtime, &cache) {
                tracing::debug!(error = %err, "presence poke: topology cache write failed");
            }
        }
        Err(err) => {
            tracing::debug!(error = %err, "presence poke: topology payload parse failed");
        }
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
                command: command_from_args(command_args),
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

fn bin_name(stem: &str) -> String {
    format!("{stem}{}", std::env::consts::EXE_SUFFIX)
}

pub(crate) fn rimz_cli_program() -> PathBuf {
    env_path("RIMZ_BIN")
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from(bin_name("rimz")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_chrome_is_agents_launch_not_agents_subcommand() {
        assert!(command_is_launch_chrome(
            "rimz agents claude,codex --worktree=quality-pass"
        ));
        assert!(command_is_launch_chrome(
            "/home/me/.cargo/bin/rimz agents claude --worktree"
        ));
        assert!(!command_is_launch_chrome("rimz agents exec codex"));
        assert!(!command_is_launch_chrome("rimz agents wait swift-otter"));
    }
}
