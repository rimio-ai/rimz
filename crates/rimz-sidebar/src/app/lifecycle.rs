//! Exit latches: self-close when the tab empties, the daemon-view detach, and
//! the grow-resize classification behind the full-width-flash guard.

use std::time::Duration;

/// Decide whether the sidebar should exit so its own pane closes. The sidebar
/// shares a tab/view with the user's working pane(s); when the last of them
/// exits, the sidebar is alone and has no reason to stay.
///
/// Startup gets one empty observation before close: during session birth the
/// sidebar can run before Zellij materializes the terminal sibling, but a tab
/// born permanently sidebar-only must still clean itself up.
///
/// `sibling_count` is `None` when the count could not be determined (the
/// snapshot carries no `own_view` — no mux pane env var, so no
/// `--exclude-pane-id`, or our own pane was missing from the live list); in
/// that case we never close.
pub(super) fn self_close_decision(
    state: &mut SelfCloseState,
    sibling_count: Option<usize>,
) -> bool {
    state.should_close(sibling_count)
}

/// A resize that grows the pane width is the necessary precondition for the
/// self-close full-width flash: the mux handed the sidebar a freed sibling's
/// space. An unknown previous width (the first resize) counts as a grow so the
/// cautious held path is taken.
pub(super) fn resize_grew(prev: Option<u16>, new: u16) -> bool {
    prev.is_none_or(|p| new > p)
}

#[derive(Debug, Default)]
pub(super) struct SelfCloseState {
    pub(super) seen_sibling: bool,
    pub(super) empty_startup_observations: u8,
}

impl SelfCloseState {
    fn should_close(&mut self, sibling_count: Option<usize>) -> bool {
        match sibling_count {
            Some(0) if self.seen_sibling => true,
            Some(0) => {
                self.empty_startup_observations = self.empty_startup_observations.saturating_add(1);
                self.empty_startup_observations >= EMPTY_STARTUP_OBSERVATIONS_BEFORE_CLOSE
            }
            Some(_) => {
                self.seen_sibling = true;
                self.empty_startup_observations = 0;
                false
            }
            None => false,
        }
    }
}

const EMPTY_STARTUP_OBSERVATIONS_BEFORE_CLOSE: u8 = 2;
/// Watchdog interval for the self-close backstop: when no resize event arrives
/// (e.g. background Zellij sessions that omit SIGWINCH after a pane closes),
/// this asks the normal snapshot path for a fresh own-view count. Sized at 2s
/// so cleanup stays prompt even when a caller configured a much slower data tick.
pub(super) const SELF_CLOSE_WATCHDOG: Duration = Duration::from_secs(2);
/// Maximum time the self-close probe spends waiting for the mux backend's
/// `list-panes` subprocess. Shorter than the default 30s backend timeout so
/// a hung Zellij does not pin the sidebar open indefinitely. Resize probes are
/// the fast path for the full-width-flash guard; the periodic backstop uses the
/// shared snapshot fetch instead.
pub(super) const PROBE_COMMAND_TIMEOUT: Duration = Duration::from_secs(4);

/// Decide whether the daemon-view sidebar should detach the client because the
/// `rimzd` daemon tab is the only tab left in the session. Mirrors
/// [`SelfCloseState`], but it detaches the client (the session keeps running)
/// rather than exiting, fires once, and only after a working view has been seen.
///
/// `only_daemon` is the snapshot's `only_daemon_view_remains`, passed only when
/// this renderer's own view *is* the daemon view (the caller gates on
/// `SidebarOwnView::own_view_is_daemon`); otherwise `None`, which never detaches.
#[derive(Debug, Default)]
pub(super) struct SessionExitState {
    /// Latched once a non-daemon (working) view has ever been seen. Until then,
    /// "only the daemon view remains" is session birth (the `rimzd` tab is born
    /// first), not teardown, so it must never detach.
    seen_other_view: bool,
    /// Latched after a detach has been requested once, so a slow client teardown
    /// spanning the next few ticks does not spawn redundant detaches.
    fired: bool,
}

impl SessionExitState {
    pub(super) fn should_detach(&mut self, only_daemon: Option<bool>) -> bool {
        match only_daemon {
            // A working view still exists → latch it; never detach while the
            // user has work open.
            Some(false) => {
                self.seen_other_view = true;
                false
            }
            // Only the daemon view remains, a working view has come and gone, and
            // we have not detached yet → the room emptied: detach, once.
            Some(true) if self.seen_other_view && !self.fired => {
                self.fired = true;
                true
            }
            // Already fired, or session birth (no working view seen yet): hold.
            Some(true) => false,
            // Not in the daemon view, or unknown: never our call.
            None => false,
        }
    }
}

#[cfg(test)]
mod tests;
