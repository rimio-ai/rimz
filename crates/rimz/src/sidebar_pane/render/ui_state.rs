use crate::agents::AgentStatus;
use crate::config::ThemeConfig;
use crate::diag::record::GateRule;
use crate::ids::PaneId;
use crate::sidebar_pane::pets::PetView;
use jiff::Timestamp;
use std::rc::Rc;

use super::sections::{MakeUpHit, ProviderTabHit};
use super::theme::Theme;
use super::{CostRolls, ScrollbarFade, TallyAnim};

#[derive(Clone, Debug, Default)]
pub struct UiState {
    pub selected_index: usize,
    pub help_visible: bool,
    /// Wall-clock animation frame counter, advanced by the serve loop's
    /// animation tick. The renderer derives the running-agent spin frame from
    /// it; freshness gating (per row) keeps a quiet agent frozen.
    pub animation_phase: u64,
    pub(crate) theme_cache: Option<(ThemeConfig, Rc<Theme>)>,
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
    /// The hold-last derived baseline: the session focus register from the last
    /// frame that named a rendered row. Selection is *derived* — recomputed from
    /// the queried mux state every fold, so it advances on a `Some` derivation
    /// and holds across a `None` (the sidebar itself is focused, or the focused
    /// pane is not a row).
    pub(crate) baseline_pane: Option<PaneId>,
    /// Whether this renderer's own tab was the on-screen tab on the last fold
    /// that carried an own-view. `None` until the first own-view fold, so
    /// attaching to an already-viewed tab seeds the latch without sweeping and
    /// a real off-screen→on-screen switch arms the tab-wide read dwell; leaving
    /// disarms it.
    pub(crate) viewing_own_tab: Option<bool>,
    /// The transient arrow-key browse pick riding above the baseline, or `None`
    /// when not browsing (see [`Browse`]).
    pub(crate) browse: Option<Browse>,
    /// The row/group order actually painted last fold; this is the source for
    /// an order hold. Empty before the first paint, which makes the first hold
    /// a no-op for ordering.
    pub(crate) last_order: FrozenOrder,
    /// The active renderer-local order hold, or `None` while rows rank live.
    pub(crate) order_hold: Option<OrderHold>,
    /// First scroll-zone content line visible in the agent-cards viewport.
    /// Resolved by every draw — clamped to the zone, then auto-scrolled so the
    /// selected card stays in view unless a [`ManualScroll`] pin holds it —
    /// and written back as a byproduct of the draw, like `line_map`.
    pub(crate) scroll_offset: usize,
    /// Stamp of the last jump scroll anchor this renderer applied, so a given
    /// anchor seeds the viewport at most once. `0` before any jump handoff.
    pub(crate) last_focus_anchor_ms: u64,
    /// Armed for one paint when a fold adopts an external focus change — a tab
    /// switch, or the first focused pane learned on attach. The next draw
    /// scrolls the focused card's worktree header into view alongside the card,
    /// then consumes the flag. A sidebar-initiated jump clears it in
    /// `apply_focus_anchor`, because the fresh anchor freezes the clicked row.
    pub(crate) focus_group_reveal: bool,
    /// The transient wheel-scroll pin riding above the auto-follow, or `None`
    /// while the viewport follows the selection (see [`ManualScroll`]).
    pub(crate) manual_scroll: Option<ManualScroll>,
    /// The unread row id the viewport is snapped to: armed when a fresh
    /// actionable unread arrives and held — overriding selection-follow, the
    /// higher-priority target — until the user engages or the row stops being
    /// the actionable lead. A manual scroll pin is respected: a new unread
    /// arriving mid-browse never yanks the view (the jump banner keeps it
    /// reachable). Crate-internal renderer state, like `manual_scroll`.
    pub(crate) unread_focus: Option<String>,
    /// The row id a manual mark-unread just reopened while it is the focused
    /// pane. While armed, focus-read auto-clear skips that row; moving focus
    /// off it releases the guard so a later revisit clears normally.
    pub(crate) unread_guard: Option<String>,
    /// The lead-unread row id observed on the previous fold — the edge detector
    /// for `unread_focus`. A change to a fresh `Some` arms the snap once, so a
    /// lead the user has already dismissed is never re-snapped while it lingers.
    pub(crate) last_lead_unread: Option<String>,
    /// The agent-cards scrollbar's auto-hide fade: every draw folds the
    /// resolved viewport offset into it as a write-back byproduct, and the
    /// `auto` scrollbar mode reads it to paint the bar only while the viewport
    /// moves plus a short settle window. Crate-internal, like `tally`.
    pub(crate) scrollbar: ScrollbarFade,
    /// The dashboard provider tab the user picked by hand (`←`/`→` or a click
    /// on a tab label), riding above the selection-derived provider. Ends like
    /// a browse: it clears when the selection-derived provider kind *genuinely*
    /// changes from the value captured at pick time (a `None` derivation — a
    /// process row — holds it), or when its target leaves the dashboard.
    pub(crate) dashboard_tab: Option<DashboardTab>,
    /// The last selection-derived agent kind the dashboard followed — its
    /// hold-last, one level finer than `baseline_pane`. Reconciliation
    /// advances it on an agent selection that has a dashboard panel and holds
    /// it across a non-agent selection, so focusing a shell row keeps the last
    /// agent's provider block instead of snapping to the first-panel default.
    /// Read by `active_dashboard_tab` below the live derivation and re-guarded
    /// against the panel still being on the dashboard.
    pub(crate) last_agent_kind: Option<String>,
    /// The current pet dashboard view, folded by the serve loop from the latest
    /// snapshot and the renderer-local asset cache before drawing. Render reads
    /// this data only; it never fetches, decodes, or slices pet assets.
    pub(crate) pet: Option<PetView>,
    /// Hit-test map of the dashboard tab rail in the most recently drawn
    /// frame: the absolute screen line and column range of each tab's
    /// cap-to-cap footprint, written as a byproduct of every draw like
    /// `line_map`. Empty when no rail is on screen.
    pub(crate) tab_hits: Vec<ProviderTabHit>,
    /// The cockpit filter target the user picked to filter the agent-card
    /// body, or `None` for the resting show-all view.
    /// Renderer-local display state — the producer, the ledger, and the
    /// cockpit counts (always the full fleet) are untouched; only the body
    /// iteration narrows, through the one shared [`row_passes_filter`]
    /// predicate. A pure toggle: a click on the active target clears it, and
    /// it auto-clears when its count drops to zero — the make-up
    /// twin of a dashboard tab pick ending when its panel leaves.
    pub(crate) make_up_filter: Option<BodyFilter>,
    /// Hit-test map of the cockpit filter targets in the most recently drawn
    /// frame: the absolute screen line and column range of each non-zero
    /// make-up bucket and the unread count's footprint, written as a
    /// byproduct of every draw like `line_map` and `tab_hits`.
    pub(crate) make_up_hits: Vec<MakeUpHit>,
    /// Screen row of the `↑ N need you` jump banner in the most recently drawn
    /// frame, or `None` when the lead-unread card is in view and the banner is
    /// hidden. The mouse hit-test reads it to scroll the cards to the top;
    /// written as a draw byproduct like `line_map`.
    pub(crate) banner_line: Option<usize>,
    /// Renderer-local status for a successful fetch that the regression gate is
    /// holding behind the last good frame. It is display-only evidence for the
    /// bottom chrome; the durable record is `gate_hold`/`gate_release`.
    pub(crate) gate_notice: Option<GateNotice>,
}

impl UiState {
    pub(crate) fn theme(&mut self, config: &ThemeConfig) -> Rc<Theme> {
        if let Some((cached_config, theme)) = &self.theme_cache
            && cached_config == config
        {
            return Rc::clone(theme);
        }
        let theme = Rc::new(Theme::for_sidebar(config));
        self.theme_cache = Some((config.clone(), Rc::clone(&theme)));
        theme
    }

    pub(crate) fn cached_theme(&self, config: &ThemeConfig) -> Option<Rc<Theme>> {
        let (cached_config, theme) = self.theme_cache.as_ref()?;
        (cached_config == config).then(|| Rc::clone(theme))
    }
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

/// A snapshot of painted row/group order. Group keys and row ids are in
/// on-screen order; row ids are globally unique, so one flat row list can
/// reorder rows across every group.
#[derive(Clone, Debug, Default)]
pub(crate) struct FrozenOrder {
    pub(crate) groups: Vec<String>,
    pub(crate) rows: Vec<String>,
}

/// Renderer-local order hold that keeps rows and groups stable while the user
/// is looking. Read state still clears immediately; only position holds.
#[derive(Clone, Debug)]
pub(crate) struct OrderHold {
    pub(crate) frozen: FrozenOrder,
    pub(crate) expires_ms: i64,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BodyFilter {
    Status(AgentStatus),
    Unread,
}
