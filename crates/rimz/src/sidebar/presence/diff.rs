//! Host-side derivation of typed Zellij sidebar events from accepted topology.

use std::collections::BTreeMap;

use crate::ids::{MuxName, PaneId};
use crate::mux::zellij::pane_topology::{PaneTopologyCache, PaneTopologyPane};
use crate::pane::SIDEBAR_CHROME_TITLE;
use crate::sidebar::events::SidebarEvent;

use super::command_is_launch_chrome;

type PaneKey = (bool, u64);

pub(super) fn derive_sidebar_events(
    existing: Option<&PaneTopologyCache>,
    incoming: &PaneTopologyCache,
    announced: bool,
) -> Vec<SidebarEvent> {
    if !announced {
        return Vec::new();
    }
    let Some(existing) = existing.filter(|cache| cache.writer == incoming.writer) else {
        return vec![SidebarEvent::PanesChanged];
    };

    let old = panes_by_key(existing);
    let new = panes_by_key(incoming);
    let mut events = Vec::new();

    for &(is_plugin, id) in old.keys() {
        if !is_plugin && !new.contains_key(&(is_plugin, id)) {
            events.push(SidebarEvent::PaneClosed {
                pane_id: terminal_pane_id(id),
            });
        }
    }
    for (&key @ (is_plugin, id), pane) in &new {
        if !old.contains_key(&key)
            && !is_plugin
            && pane.is_live_terminal()
            && pane.title.as_deref() != Some(SIDEBAR_CHROME_TITLE)
        {
            let command = pane
                .pane_command
                .as_deref()
                .or(pane.terminal_command.as_deref())
                .filter(|command| !command.is_empty() && !command_is_launch_chrome(command))
                .map(str::to_owned);
            events.push(SidebarEvent::PaneOpened {
                pane_id: terminal_pane_id(id),
                command,
            });
        }
    }
    for (&key @ (is_plugin, id), pane) in &new {
        let Some(previous) = old.get(&key) else {
            continue;
        };
        if !is_plugin && previous.is_live_terminal() && pane.is_live_terminal() {
            let command = pane
                .pane_command
                .as_deref()
                .filter(|command| !command.is_empty());
            if previous.pane_command != pane.pane_command
                && let Some(command) = command
            {
                events.push(SidebarEvent::CommandChanged {
                    pane_id: terminal_pane_id(id),
                    command: command.to_owned(),
                });
            }
        }
    }
    if existing.focused_pane != incoming.focused_pane {
        events.push(SidebarEvent::FocusChanged {
            focused: incoming
                .focused_pane
                .into_iter()
                .map(terminal_pane_id)
                .collect(),
            unfocused: existing
                .focused_pane
                .into_iter()
                .map(terminal_pane_id)
                .collect(),
        });
    }

    if events.is_empty() {
        events.push(SidebarEvent::PanesChanged);
    }
    events
}

fn panes_by_key(cache: &PaneTopologyCache) -> BTreeMap<PaneKey, &PaneTopologyPane> {
    cache
        .panes
        .iter()
        .map(|pane| ((pane.is_plugin, pane.id), pane))
        .collect()
}

fn terminal_pane_id(id: u64) -> PaneId {
    PaneId::from_parts(MuxName::Zellij, format!("terminal_{id}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::zellij::pane_topology::TopologyWriter;

    fn writer(plugin_id: u32) -> TopologyWriter {
        TopologyWriter {
            plugin_id,
            loaded_at_ms: u64::from(plugin_id),
            build: None,
            config: None,
        }
    }

    fn topology(writer: TopologyWriter, panes: Vec<PaneTopologyPane>) -> PaneTopologyCache {
        PaneTopologyCache {
            session_name: "rimz-test".to_owned(),
            produced_at_ms: 1,
            writer: Some(writer),
            focused_pane: None,
            clients: None,
            panes,
        }
    }

    fn pane(id: u64, command: Option<&str>) -> PaneTopologyPane {
        PaneTopologyPane {
            id,
            is_plugin: false,
            is_held: false,
            exited: false,
            is_suppressed: false,
            is_floating: false,
            tab_position: 1,
            tab_name: Some("work".to_owned()),
            pane_columns: Some(80),
            pane_x: Some(0),
            title: Some(format!("pane-{id}")),
            pane_command: command.map(str::to_owned),
            pane_cwd: None,
            pane_pid: None,
            terminal_command: None,
        }
    }

    #[test]
    fn absent_cache_and_writer_change_are_baselines() {
        let first = topology(writer(1), vec![pane(1, Some("codex"))]);
        assert_eq!(
            derive_sidebar_events(None, &first, true),
            vec![SidebarEvent::PanesChanged],
        );
        let replacement = topology(writer(2), vec![pane(2, Some("cargo"))]);
        assert_eq!(
            derive_sidebar_events(Some(&first), &replacement, true),
            vec![SidebarEvent::PanesChanged],
        );
        assert!(derive_sidebar_events(Some(&first), &replacement, false).is_empty());
    }

    #[test]
    fn one_snapshot_can_open_multiple_card_panes() {
        let current_writer = writer(1);
        let existing = topology(current_writer.clone(), vec![pane(1, None)]);
        let mut launch = pane(3, None);
        launch.terminal_command = Some("rimz agents claude,codex".to_owned());
        let incoming = topology(
            current_writer,
            vec![pane(1, None), pane(2, Some("codex --search")), launch],
        );
        assert_eq!(
            derive_sidebar_events(Some(&existing), &incoming, true),
            vec![
                SidebarEvent::PaneOpened {
                    pane_id: terminal_pane_id(2),
                    command: Some("codex --search".to_owned()),
                },
                SidebarEvent::PaneOpened {
                    pane_id: terminal_pane_id(3),
                    command: None,
                },
            ],
        );
    }

    #[test]
    fn removal_command_and_focus_transitions_are_typed() {
        let current_writer = writer(1);
        let mut existing = topology(
            current_writer.clone(),
            vec![pane(1, Some("cargo")), pane(2, Some("shell"))],
        );
        existing.focused_pane = Some(1);
        let mut incoming = topology(current_writer, vec![pane(1, Some("codex"))]);
        incoming.focused_pane = Some(2);
        assert_eq!(
            derive_sidebar_events(Some(&existing), &incoming, true),
            vec![
                SidebarEvent::PaneClosed {
                    pane_id: terminal_pane_id(2),
                },
                SidebarEvent::CommandChanged {
                    pane_id: terminal_pane_id(1),
                    command: "codex".to_owned(),
                },
                SidebarEvent::FocusChanged {
                    focused: vec![terminal_pane_id(2)],
                    unfocused: vec![terminal_pane_id(1)],
                },
            ],
        );
    }

    #[test]
    fn anonymous_and_identical_announcements_nudge() {
        let current_writer = writer(1);
        let existing = topology(current_writer.clone(), vec![pane(1, Some("cargo"))]);
        assert_eq!(
            derive_sidebar_events(Some(&existing), &existing, true),
            vec![SidebarEvent::PanesChanged],
        );
        let incoming = topology(current_writer, vec![pane(1, None)]);
        assert_eq!(
            derive_sidebar_events(Some(&existing), &incoming, true),
            vec![SidebarEvent::PanesChanged],
        );
        assert!(derive_sidebar_events(Some(&existing), &incoming, false).is_empty());
    }
}
