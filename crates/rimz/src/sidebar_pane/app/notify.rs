use super::*;

pub(super) fn emit_terminal_notification(
    config: &ServeConfig,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    snapshot: &SidebarSnapshot,
    title: &str,
    body: &str,
    panes: &[PaneId],
    recheck_unread: bool,
) -> io::Result<bool> {
    let prefs = &config.notification_prefs;
    let mut bytes = Vec::new();
    if desktop_notification_targets_renderer(config.mux, snapshot, panes) {
        bytes.extend(osc::desktop_notification_bytes(
            config.mux,
            prefs.desktop,
            title,
            body,
        ));
    }
    if bell_targets_own_view(snapshot, panes, recheck_unread) {
        bytes.extend(osc::sound_notification_bytes(prefs.sound));
    }
    if bytes.is_empty() {
        return Ok(false);
    }
    let backend = terminal.backend_mut();
    backend.write_all(&bytes)?;
    backend.flush()?;
    Ok(true)
}

/// Whether this renderer rings the sticky tab bell for a notification. The bell
/// is a mux tab marker the renderer cannot retract, so it is bound to genuine,
/// current unread attention: a daemon-only view never rings (its siblings are
/// infrastructure host panes, never agents that need you), and an agent
/// notification rings only while a targeted, owned pane's row is still unread —
/// the same `UnreadTracker` signal stamped onto `SidebarRow::unread`, which
/// clears the instant the agent returns to running. Link reachability alerts and
/// pre-vetted unread reminders clear `recheck_unread` to ring whenever they own
/// a targeted, non-daemon pane.
pub(super) fn bell_targets_own_view(
    snapshot: &SidebarSnapshot,
    panes: &[PaneId],
    recheck_unread: bool,
) -> bool {
    let Some(view) = snapshot.own_view.as_ref() else {
        return false;
    };
    if view.own_view_is_daemon {
        return false;
    }
    if !panes
        .iter()
        .any(|pane| view.working_pane_ids.contains(pane))
    {
        return false;
    }
    if !recheck_unread {
        return true;
    }
    panes
        .iter()
        .any(|pane| view.working_pane_ids.contains(pane) && pane_row_unread(snapshot, pane))
}

/// Whether the agent row bound to `pane` is currently unread. Mirrors the row
/// lookup the producer uses in [`crate::sidebar::notify`], reading the unread
/// bit the fold already stamped onto each row.
fn pane_row_unread(snapshot: &SidebarSnapshot, pane: &PaneId) -> bool {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .find(|row| row.pane.as_ref().is_some_and(|p| &p.pane_id == pane))
        .is_some_and(|row| row.unread)
}

pub(super) fn desktop_notification_targets_renderer(
    mux: MuxName,
    snapshot: &SidebarSnapshot,
    panes: &[PaneId],
) -> bool {
    match mux {
        MuxName::Tmux => snapshot.own_view.is_some(),
        MuxName::Zellij => notification_targets_own_view(snapshot, panes),
    }
}

pub(super) fn notification_targets_own_view(snapshot: &SidebarSnapshot, panes: &[PaneId]) -> bool {
    snapshot.own_view.as_ref().is_some_and(|view| {
        panes
            .iter()
            .any(|pane| view.working_pane_ids.contains(pane))
    })
}
