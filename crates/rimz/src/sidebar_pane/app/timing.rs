use super::*;

pub(super) fn is_animating(snapshot: &SidebarSnapshot, ui: &UiState, phase: u64) -> bool {
    render::has_live_animation(snapshot)
        || pet_animating(snapshot, ui)
        || ui.tally.any_rolling(phase)
        || ui.cost_rolls.any_rolling(phase)
        || ui.scrollbar.fading(phase)
        || ui.effects.any_active()
}

fn pet_animating(snapshot: &SidebarSnapshot, ui: &UiState) -> bool {
    snapshot.sidebar.pets.enabled
        && ui.pet.as_ref().is_some_and(|view| {
            view.loading
                || (view.grid.is_some()
                    && render::pet_body_enabled(snapshot)
                    && render::pet_motion_enabled(snapshot, view.status))
        })
        && matches!(
            render::active_dashboard_tab(snapshot, ui),
            Some(render::DashboardTabId::Pets)
        )
}

fn pet_frame_interval(
    snapshot: &SidebarSnapshot,
    ui: &UiState,
    refresh_ms: u16,
) -> Option<Duration> {
    if !snapshot.sidebar.pets.enabled
        || !matches!(
            render::active_dashboard_tab(snapshot, ui),
            Some(render::DashboardTabId::Pets)
        )
    {
        return None;
    }
    let view = ui.pet.as_ref()?;
    if view.loading {
        return Some(crate::sidebar::timing::animation_frame(refresh_ms));
    }
    if view.grid.is_some()
        && render::pet_body_enabled(snapshot)
        && render::pet_motion_enabled(snapshot, view.status)
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

/// The animation frame index for `now`, derived from elapsed wall-clock since
/// the serve loop's monotonic base. Every redraw path sets the phase from this,
/// so the spin advances on real time and survives re-fetches and ledger deltas
/// without a per-tick counter that a break-and-refetch could reset.
pub(super) fn wall_clock_phase(start: Instant, refresh_ms: u16) -> u64 {
    (start.elapsed().as_millis() / u128::from(refresh_ms)) as u64
}

pub(super) fn frame_interval(snapshot: &SidebarSnapshot, ui: &UiState) -> Duration {
    let refresh_ms = snapshot.sidebar.resolved_refresh_ms();
    let base = crate::sidebar::timing::animation_frame(refresh_ms);
    // A decaying one-shot flash needs the fast grid to read as motion; it is
    // brief and self-terminating, so the cost is bounded to the transition
    // window. Continuous row pulse rides the breath cadence below.
    if ui.scrollbar.fading(ui.animation_phase) || ui.effects.any_active() {
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
    // The foreground Pets tab paints on its track cadence, but a money climb in
    // the still-visible cockpit must keep sampling on the money grid, so a
    // rolling room takes the faster of the two.
    if let Some(pet_interval) = pet_frame_interval(snapshot, ui, refresh_ms) {
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
    crate::sidebar::timing::animation_frame(snapshot.sidebar.resolved_refresh_ms())
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
