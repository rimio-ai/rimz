//! Exit-request latches and the bounded grow-resize paint hold.
//!
//! Cache-backed sibling counts decide when the worker requests self-close;
//! the supervisor owns the authoritative mux confirmation and pane lifetime.

use std::time::Instant;

/// Decide whether the sidebar should exit so its own pane closes. The sidebar
/// shares a tab/view with the user's working pane(s); when the last of them
/// exits, the sidebar is alone and has no reason to stay.
///
/// A zero-sibling read after a real sibling was observed closes immediately:
/// the producer verified the shrink before publishing the empty count. The
/// confirm window guards only session birth or resurrection, where the sidebar
/// can run before Zellij materializes sibling panes; a tab born permanently
/// sidebar-only still cleans itself up once the window elapses.
///
/// `sibling_count` is `None` when the count could not be determined (the
/// snapshot carries no `own_view` — no mux pane env var, so no
/// `--exclude-pane-id`, or our own pane was missing from the live list); in
/// that case we never close.
pub(super) fn self_close_decision(
    state: &mut SelfCloseState,
    sibling_count: Option<usize>,
    now: Instant,
) -> bool {
    state.should_close(sibling_count, now)
}

/// A resize that grows the pane width is one precondition for the self-close
/// full-width flash. An unknown previous width (the first resize) counts as a
/// grow; the loop combines it with the legitimate-width bound and a prior
/// sibling observation before taking the cautious held path.
pub(super) fn resize_grew(prev: Option<u16>, new: u16) -> bool {
    prev.is_none_or(|p| new > p)
}

const GROW_LEGIT_SLACK_COLS: u16 = 2;

/// A grow at or below the legitimate width bound is startup or attach sizing
/// and paints immediately. Only a grow beyond that bound can be space freed by
/// a closing sibling.
pub(super) fn grow_beyond_legit(width: u16, max_legit_cols: u16) -> bool {
    width > max_legit_cols.saturating_add(GROW_LEGIT_SLACK_COLS)
}

#[derive(Debug, Default)]
pub(super) struct SelfCloseState {
    pub(super) seen_sibling: bool,
    empty_since: Option<Instant>,
}

impl SelfCloseState {
    pub(super) fn confirming_empty(&self) -> bool {
        self.empty_since.is_some()
    }

    fn should_close(&mut self, sibling_count: Option<usize>, now: Instant) -> bool {
        match sibling_count {
            Some(0) => {
                // A working pane we had observed is gone. The producer verifies
                // any shrink toward empty before publishing a zero, carrying live
                // panes by /proc liveness, so this zero is real. Only the
                // birth/resurrection path waits out the confirm window.
                if self.seen_sibling {
                    return true;
                }
                let empty_since = *self.empty_since.get_or_insert(now);
                now.duration_since(empty_since) >= SELF_CLOSE_EMPTY_CONFIRM
            }
            Some(_) => {
                self.seen_sibling = true;
                self.empty_since = None;
                false
            }
            None => false,
        }
    }
}

/// Bounded paint hold armed by a pane-width grow. It waits for a pane-frame
/// observation stamped after the grow before painting at the new size, with a
/// wall-clock ceiling so a lost wakeup cannot suppress every future frame.
#[derive(Debug, Default)]
pub(super) struct PaintHold {
    deadline: Option<Instant>,
    engaged_at_ms: Option<u64>,
}

impl PaintHold {
    pub(super) fn engage(&mut self, now: Instant, now_ms: u64) {
        self.deadline = Some(now + RESIZE_PAINT_HOLD_CEILING);
        self.engaged_at_ms = Some(now_ms);
    }

    pub(super) fn release(&mut self) {
        self.deadline = None;
        self.engaged_at_ms = None;
    }

    pub(super) fn is_engaged(&self) -> bool {
        self.deadline.is_some()
    }

    pub(super) fn blocks_paint(&mut self, now: Instant) -> bool {
        let Some(deadline) = self.deadline else {
            return false;
        };
        if now < deadline {
            return true;
        }
        self.release();
        false
    }

    pub(super) fn releases_on_stamp(&self, observed_at_ms: Option<u64>) -> bool {
        let Some(engaged_at_ms) = self.engaged_at_ms else {
            return false;
        };
        observed_at_ms.is_some_and(|observed_at_ms| observed_at_ms >= engaged_at_ms)
    }
}

pub(super) use crate::sidebar::timing::{
    RESIZE_PAINT_HOLD_CEILING, SELF_CLOSE_EMPTY_CONFIRM, SELF_CLOSE_WATCHDOG,
};

#[cfg(test)]
mod tests;
