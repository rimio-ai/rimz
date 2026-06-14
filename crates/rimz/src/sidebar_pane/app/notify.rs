use super::*;

/// One notification to render in this renderer's terminal: the desktop text, the
/// owned panes it targets, whether the tab bell re-checks unread, and the
/// producer's kind for the trace.
pub(super) struct BellNotice<'a> {
    pub title: &'a str,
    pub body: &'a str,
    pub panes: &'a [PaneId],
    pub recheck_unread: bool,
    pub kind: &'a str,
}

pub(super) fn emit_terminal_notification(
    config: &ServeConfig,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    snapshot: &SidebarSnapshot,
    notice: BellNotice<'_>,
    diag: Option<&crate::diag::DiagSink>,
) -> io::Result<bool> {
    let prefs = &config.notification_prefs;
    let mut bytes = Vec::new();
    if desktop_notification_targets_renderer(config.mux, snapshot, notice.panes) {
        bytes.extend(osc::desktop_notification_bytes(
            config.mux,
            prefs.desktop,
            notice.title,
            notice.body,
        ));
    }
    let bell = bell_decision(snapshot, notice.panes, notice.recheck_unread);
    if let Some(diag) = diag {
        diag.trace_notify(crate::schema::notify_trace::NotifyTraceEvent::BellRing {
            notification_kind: notice.kind.to_owned(),
            fired: bell.fired(),
            recheck_unread: notice.recheck_unread,
            panes: notice.panes.to_vec(),
            suppressed: bell.suppressed_reason().map(str::to_owned),
        });
    }
    if bell.fired() {
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

/// Why this renderer did or did not ring the sticky tab bell for a notification.
/// The bell is a mux tab marker the renderer cannot retract, so it is bound to
/// genuine, current unread attention: a daemon-only view never rings (its
/// siblings are infrastructure host panes, never agents that need you), and an
/// agent notification rings only while a targeted, owned pane's row is still
/// unread — the durable unread episode bit folded onto `SidebarRow::unread`,
/// which stays set until a human looks. Pre-vetted unread reminders clear
/// `recheck_unread` to ring whenever they own a targeted, non-daemon pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BellDecision {
    Fired,
    NoOwnView,
    DaemonView,
    PaneNotInView,
    NotUnread,
}

impl BellDecision {
    pub(super) fn fired(self) -> bool {
        matches!(self, Self::Fired)
    }

    fn suppressed_reason(self) -> Option<&'static str> {
        match self {
            Self::Fired => None,
            Self::NoOwnView => Some("no_own_view"),
            Self::DaemonView => Some("daemon_view"),
            Self::PaneNotInView => Some("pane_not_in_view"),
            Self::NotUnread => Some("not_unread"),
        }
    }
}

pub(super) fn bell_decision(
    snapshot: &SidebarSnapshot,
    panes: &[PaneId],
    recheck_unread: bool,
) -> BellDecision {
    let Some(view) = snapshot.own_view.as_ref() else {
        return BellDecision::NoOwnView;
    };
    if view.own_view_is_daemon {
        return BellDecision::DaemonView;
    }
    if !panes
        .iter()
        .any(|pane| view.working_pane_ids.contains(pane))
    {
        return BellDecision::PaneNotInView;
    }
    if !recheck_unread {
        return BellDecision::Fired;
    }
    if panes
        .iter()
        .any(|pane| view.working_pane_ids.contains(pane) && pane_row_unread(snapshot, pane))
    {
        BellDecision::Fired
    } else {
        BellDecision::NotUnread
    }
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
