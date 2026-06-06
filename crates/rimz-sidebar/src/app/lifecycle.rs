//! Exit latches: self-close when the tab empties, and the grow-resize
//! classification behind the full-width-flash guard.

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

#[cfg(test)]
mod tests;
