use crate::feed::AgentStatus;
use crate::ids::PaneId;
use crate::schema::diag::GateRule;
use jiff::Timestamp;

use super::sections::{MakeUpHit, ProviderTabHit};
use super::{CostRolls, EffectState, ScrollbarFade, TallyAnim};

#[derive(Clone, Debug, Default)]
pub struct UiState {
    pub selected_index: usize,
    pub help_visible: bool,
    /// Wall-clock animation frame counter, advanced by the serve loop's
    /// animation tick. The renderer derives the running-agent spin frame from
    /// it; freshness gating (per row) keeps a quiet agent frozen.
    pub animation_phase: u64,
    /// The cockpit spend's count-up state — one stepped roll for today's `$`.
    /// Folded forward on each data refresh (`TallyAnim::observe`) and read by the
    /// renderer at `animation_phase`; the serve loop keeps the fast tick alive
    /// while a roll is in flight. Crate-internal: an implementation detail of the
    /// renderer, not part of the public `UiState` surface.
    pub(crate) tally: TallyAnim,
    /// The agent cards' `$cost` count-up state — one stepped roll per row,
    /// keyed by the row's durable id so a reorder or refresh re-anchors a
    /// climb to its agent. Folded next to `tally` on each data refresh
    /// (`CostRolls::observe`, which also prunes departed rows) and read by the
    /// card at `animation_phase`; ORed into the serve loop's animation gate
    /// beside the tally. Crate-internal, like `tally`.
    pub(crate) cost_rolls: CostRolls,
    /// The post-render effects pass's memory — the transition detector's diff
    /// base and the live one-shot flashes ([`effects::EffectState`]). Observed
    /// and painted as a byproduct of every draw, after the paragraph render;
    /// the serve loop keeps the fast tick alive while a flash decays
    /// (`EffectState::any_active`), the tally's twin — and like the tally,
    /// crate-internal, not part of the public `UiState` surface.
    pub(crate) effects: EffectState,
    /// Hit-test map of the most recently drawn frame: one entry per inner-area
    /// content line, `Some(row)` for a jump-target row line (in
    /// `app::visible_rows()` order) and `None` for chrome. The renderer writes
    /// it as a byproduct of every draw; the mouse hit-test reads it. Empty
    /// before the first draw.
    pub line_map: Vec<Option<usize>>,
    /// The pane the highlight is pinned to — selection keyed by identity, not
    /// position. Re-derived each fold by `app::reconcile_selection` from the
    /// derived `baseline_pane` and any live `browse`. Keying on the pane means
    /// a status-churn reorder re-anchors the highlight to the same pane
    /// instead of sliding it onto a neighbour.
    pub selected_pane: Option<PaneId>,
    /// The hold-last derived baseline: the own view's active working pane from
    /// the last frame that reported one. Selection is *derived* — recomputed
    /// from the queried mux state every fold, so it is same-tab by construction
    /// and can never desynchronize, only lag a frame. It advances on a `Some`
    /// derivation and holds across a `None` (the sidebar itself is the view's
    /// active pane, or the active pane is not a row).
    pub(crate) baseline_pane: Option<PaneId>,
    /// The transient arrow-key browse pick riding above the baseline, or `None`
    /// when not browsing (see [`Browse`]).
    pub(crate) browse: Option<Browse>,
    /// First scroll-zone content line visible in the agent-cards viewport.
    /// Resolved by every draw — clamped to the zone, then auto-scrolled so the
    /// selected card stays in view unless a [`ManualScroll`] pin or the open
    /// help overlay holds it — and written back as a byproduct of the draw,
    /// like `line_map`.
    pub(crate) scroll_offset: usize,
    /// The transient wheel-scroll pin riding above the auto-follow, or `None`
    /// while the viewport follows the selection (see [`ManualScroll`]).
    pub(crate) manual_scroll: Option<ManualScroll>,
    /// The agent-cards scrollbar's auto-hide fade: every draw folds the
    /// resolved viewport offset into it as a write-back byproduct, and the
    /// `auto` scrollbar mode reads it to paint the bar only while the viewport
    /// moves plus a short settle window. Crate-internal, like `tally`.
    pub(crate) scrollbar: ScrollbarFade,
    /// The dashboard tab the user picked by hand (`←`/`→` or a click on a tab
    /// label), riding above the selection-derived default. Ends like a browse:
    /// it clears when the selection-derived provider kind *genuinely* changes
    /// from the value captured at pick time (a `None` derivation — a process
    /// row — holds it), or when its panel leaves the dashboard.
    pub(crate) dashboard_tab: Option<DashboardTab>,
    /// Hit-test map of the dashboard tab rail in the most recently drawn
    /// frame: the absolute screen line and column range of each tab's
    /// cap-to-cap footprint, written as a byproduct of every draw like
    /// `line_map`. Empty when no rail is on screen.
    pub(crate) tab_hits: Vec<ProviderTabHit>,
    /// The cockpit make-up bucket the user clicked to filter the agent-card
    /// body to one status, or `None` for the resting show-all view.
    /// Renderer-local display state — the producer, the ledger, and the
    /// cockpit counts (always the full fleet) are untouched; only the body
    /// iteration narrows, through the one shared [`row_passes_filter`]
    /// predicate. A pure toggle: a click on the active bucket clears it, and
    /// it auto-clears when its bucket's count drops to zero — the make-up
    /// twin of a dashboard tab pick ending when its panel leaves.
    pub(crate) make_up_filter: Option<BodyFilter>,
    /// Hit-test map of the cockpit make-up line in the most recently drawn
    /// frame: the absolute screen line and column range of each non-zero
    /// bucket's footprint, written as a byproduct of every draw like
    /// `line_map` and `tab_hits`. Empty when no make-up line is on screen.
    pub(crate) make_up_hits: Vec<MakeUpHit>,
    /// Renderer-local status for a successful fetch that the regression gate is
    /// holding behind the last good frame. It is display-only evidence for the
    /// bottom chrome; the durable record is `gate_hold`/`gate_release`.
    pub(crate) gate_notice: Option<GateNotice>,
}

/// The manual dashboard-tab pick: the provider kind to show, plus the
/// selection-derived kind captured when the pick was made — the clear
/// condition, mirroring [`Browse`]: the pick holds until the derived kind
/// genuinely changes from `derived_at_start`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DashboardTab {
    pub(crate) kind: String,
    pub(crate) derived_at_start: Option<String>,
}

/// Arrow-key browse: pins `pane` WITHOUT moving focus, roaming every visible
/// row — other tabs' rows included, so any card is one keystroke from
/// expanding. Holds until the derived baseline genuinely changes from
/// `baseline_at_start` — the value captured when browsing began. A `None`
/// derivation holds the baseline, so an inert frame never ends a browse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Browse {
    pub(crate) pane: PaneId,
    pub(crate) baseline_at_start: Option<PaneId>,
}

/// Wheel scroll: pins the viewport offset WITHOUT moving the selection, so the
/// user can peek at cards beyond the fold. Holds until the selection genuinely
/// changes from `selection_at_start` — the value captured when the scroll began
/// — then the viewport snaps back to following the selected card. The browse
/// twin, one layer down: browse pins *which card*, this pins *which window*.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManualScroll {
    pub(crate) selection_at_start: Option<PaneId>,
}

/// A sticky health alert pinned to the bottom of the sidebar.
///
/// `since` is when the unhealthy episode began, so an active alert can show
/// `for Ns`. `recovered_at` is `None` while the loop is still unhealthy and
/// `Some(t)` once it healed — a recovered alert lingers as a dismissable
/// "last alert" notice rather than vanishing the instant a fetch succeeds.
#[derive(Clone, Debug)]
pub struct Alert {
    pub reason: String,
    pub since: Timestamp,
    pub recovered_at: Option<Timestamp>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GateNotice {
    pub(crate) rule: GateRule,
}

impl Alert {
    pub fn active(reason: impl Into<String>, since: Timestamp) -> Self {
        Self {
            reason: reason.into(),
            since,
            recovered_at: None,
        }
    }

    pub fn is_active(&self) -> bool {
        self.recovered_at.is_none()
    }
}

/// The fastest animation class currently visible in the snapshot. Fast motion
/// changes every frame (working/thinking spinners, resolver work, active
/// process rows). Breath motion is the attention/result blink and the calm
/// resting breathe, sampled near the base grid without paying the full spinner
/// cadence for calm rooms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimationCadence {
    None,
    Breath,
    Fast,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BodyFilter {
    Status(AgentStatus),
    Unread,
}
