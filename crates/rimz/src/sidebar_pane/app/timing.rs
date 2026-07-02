use super::*;

pub(super) fn is_animating(
    snapshot: &SidebarSnapshot,
    ui: &UiState,
    phase: u64,
    alert_active: bool,
) -> bool {
    render::has_live_animation(snapshot)
        || pet_frame_interval(
            snapshot,
            ui,
            alert_active,
            snapshot.theme.display.resolved_refresh_ms(),
        )
        .is_some()
        || ui.tally.any_rolling(phase)
        || ui.cost_rolls.any_rolling(phase)
        || ui.scrollbar.fading(phase)
}

fn pet_frame_interval(
    snapshot: &SidebarSnapshot,
    ui: &UiState,
    alert_active: bool,
    refresh_ms: u16,
) -> Option<Duration> {
    if !snapshot.theme.pets.enabled || !render::dashboard_present(snapshot, alert_active) {
        return None;
    }
    let view = ui.pet.as_ref()?;
    if view.loading {
        return Some(crate::sidebar::timing::animation_frame(refresh_ms));
    }
    if view.has_body()
        && render::pet_body_enabled(snapshot)
        && render::pet_motion_enabled(snapshot, view.action)
    {
        return Some(crate::sidebar_pane::pets::animation_frame(
            view.active_track,
            refresh_ms,
        ));
    }
    None
}

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
/// so the spin advances on real time and survives re-fetches and ledger deltas
/// without a per-tick counter that a break-and-refetch could reset.
pub(super) fn wall_clock_phase(start: Instant, refresh_ms: u16) -> u64 {
    (start.elapsed().as_millis() / u128::from(refresh_ms)) as u64
}

pub(super) fn frame_interval(
    snapshot: &SidebarSnapshot,
    ui: &UiState,
    alert_active: bool,
) -> Duration {
    let refresh_ms = snapshot.theme.display.resolved_refresh_ms();
    let base = crate::sidebar::timing::animation_frame(refresh_ms);
    // A scrollbar fade needs the fast grid to read as motion; it is brief and
    // self-terminating, so the cost is bounded to the settle window. Continuous
    // row pulse rides the breath cadence below.
    if ui.scrollbar.fading(ui.animation_phase) {
        return base;
    }
    let cadence = render::animation_cadence(snapshot);
    if cadence == render::AnimationCadence::Fast {
        return base;
    }
    // The money rolls click once per `CLICK_PHASES` phases, so a rolling room
    // samples on the matching money grid — one paint per distinct click, and
    // the one-click settle flash can never fall between samples. A fast room
    // (a working spinner) keeps the fast grid; the roll's painted value simply
    // holds across the extra frames. A slow-cadence room drops to the money
    // grid while a climb is in flight — the cosmetic breath repaints
    // idempotently, and the climb window bounds the extra paints.
    let money_rolling =
        ui.tally.any_rolling(ui.animation_phase) || ui.cost_rolls.any_rolling(ui.animation_phase);
    let money_grid =
        || crate::sidebar::timing::money_animation_frame(refresh_ms, render::CLICK_PHASES);
    // The dashboard pet paints on its track cadence, but a money climb in the
    // still-visible cockpit must keep sampling on the money grid, so a rolling
    // room takes the faster of the two.
    if let Some(pet_interval) = pet_frame_interval(snapshot, ui, alert_active, refresh_ms) {
        return if money_rolling {
            pet_interval.min(money_grid())
        } else {
            pet_interval
        };
    }
    match cadence {
        render::AnimationCadence::Fast => base,
        _ if money_rolling => money_grid(),
        render::AnimationCadence::Breath => {
            crate::sidebar::timing::breath_animation_frame(refresh_ms)
        }
        render::AnimationCadence::None => base,
    }
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
