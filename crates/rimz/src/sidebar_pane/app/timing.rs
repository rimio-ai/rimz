//! Wall-clock phase and frame-grid helpers, with receive and focus-resume timing bounds.

use std::time::{Duration, Instant};

use crate::store::snapshot::SidebarSnapshot;

/// Floor for the frame-boundary recv timeout. When the loop is at or past the
/// next frame boundary, the time-to-boundary is zero; a 1ms floor lets an
/// already-queued datagram drain on this turn without a zero-timeout busy spin.
pub(super) const FRAME_MIN_TIMEOUT: Duration = Duration::from_millis(1);

/// How long own-pane focus keeps cosmetic animation on the watched grid before
/// the authoritative pane frame confirms. Too short risks a suspend/resume
/// flicker after a slow produce; too long spends animation on a hidden pane
/// after a quick switch away.
pub(super) const FOCUS_RESUME_WATCH_WINDOW: Duration = Duration::from_secs(3);

/// The animation frame index for `now`, derived from elapsed wall-clock since
/// the serve loop's monotonic base. Every redraw path sets the phase from this,
/// so the spin advances on real time and survives re-fetches and store deltas
/// without a per-tick counter that a break-and-refetch could reset.
pub(super) fn wall_clock_phase(start: Instant, refresh_ms: u16) -> u64 {
    (start.elapsed().as_millis() / u128::from(refresh_ms)) as u64
}

/// Animation tick: how often an animated row advances a spin frame - a running
/// agent's head, a resolver, or an active process spinning on real work. Pure
/// in-process redraw from the cached snapshot never forks a fetch, so the spin
/// layer is decoupled from the data layer and stays smooth regardless of fetch
/// latency.
pub(super) fn animation_frame(snapshot: &SidebarSnapshot) -> Duration {
    crate::sidebar::timing::animation_frame(snapshot.theme.display.resolved_refresh_ms())
}

pub(super) fn tick_for(seconds: u64) -> Duration {
    Duration::from_secs(seconds.max(1))
}

/// The next frame boundary after painting the frame scheduled for `scheduled`
/// at wall-clock `now`. The grid normally advances by exactly one `frame`, so
/// paints hold a fixed cadence regardless of how long a paint took. When the
/// loop has fallen a full frame or more behind — a slow paint or a scheduler
/// hiccup — it snaps onto the boundary one `frame` ahead of `now` rather than
/// replaying every missed boundary, so a backlog can never spiral into a burst
/// of catch-up paints.
pub(super) fn next_frame_after(scheduled: Instant, now: Instant, frame: Duration) -> Instant {
    let advanced = scheduled + frame;
    if advanced <= now {
        now + frame
    } else {
        advanced
    }
}

pub(super) fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}
