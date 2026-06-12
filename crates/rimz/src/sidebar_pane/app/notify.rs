use super::*;

pub(super) fn emit_terminal_notification(
    config: &ServeConfig,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    snapshot: &SidebarSnapshot,
    prefs: &NotificationsPrefs,
    title: &str,
    body: &str,
    panes: &[PaneId],
) -> io::Result<bool> {
    let mut bytes = Vec::new();
    if desktop_notification_targets_renderer(config.mux, snapshot, panes) {
        bytes.extend(osc::desktop_notification_bytes(
            config.mux,
            prefs.desktop,
            title,
            body,
        ));
    }
    if notification_targets_own_view(snapshot, panes) {
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
