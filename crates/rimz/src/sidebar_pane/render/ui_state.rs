use crate::config::ThemeConfig;
use crate::diag::record::GateRule;
use crate::ids::PaneId;
use crate::sidebar_pane::pets::PetView;
use crate::sidebar_pane::pixel::meter::MeterPixels;
use crate::sidebar_pane::view::{BodyLens, VisibleRoster};
use crate::store::snapshot::{SidebarRow, SidebarSnapshot};
use jiff::Timestamp;
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroUsize;
use std::rc::Rc;

use super::theme::Theme;
use super::{CostRolls, FrameInteractions, ScrollEase, ScrollbarFade, TallyAnim};

use crate::mux::focus_anchor::FocusPlacement;
pub(in crate::sidebar_pane) use crate::mux::focus_anchor::{FrozenOrder, FrozenRow};

#[derive(Clone, Debug, Default)]
pub(in crate::sidebar_pane) struct UiState {
    pub(in crate::sidebar_pane) theme_cache: Option<(ThemeConfig, Rc<Theme>)>,
    /// Typed hit geometry from the most recently painted frame.
    pub(in crate::sidebar_pane) interactions: FrameInteractions,
    /// Section labels in physical-row coordinates from that same paint.
    pub(in crate::sidebar_pane) sections: Vec<super::SectionLabel>,
    /// Row identity per roster ordinal in the last painted hit map.
    pub(in crate::sidebar_pane) painted_roster: Vec<String>,
    /// The pane `q` focuses, read from the room frame just before a
    /// `pane.return` press reduces and taken by it.
    pub(in crate::sidebar_pane) return_pane: Option<crate::ids::PaneId>,
    /// `Ctrl+l` asked for a full repaint: the next composition clears the
    /// pane first, so every cell is written again.
    pub(in crate::sidebar_pane) clear_pending: bool,
    /// Maximum offset in the last drawn frame, absent before the first draw.
    pub(in crate::sidebar_pane) drawn_scroll_max: Option<usize>,
    /// Visible cards' own first lines in absolute pane rows, signed when clipped above the zone.
    pub(in crate::sidebar_pane) painted_rows: Vec<(String, isize)>,
    /// Absolute pane row of the last draw's scroll-zone top.
    pub(in crate::sidebar_pane) scroll_zone_top: usize,
    /// The last draw's anchor correction hit the offset clamp, so the card moved.
    pub(in crate::sidebar_pane) anchor_clamped: bool,
    /// Whether the last frame painted on this pane showed a banner.
    pub(in crate::sidebar_pane) banner_shown: bool,
    /// Digest of the skipped-settings notice the last frame drew, `None` when it drew none.
    pub(in crate::sidebar_pane) skipped_shown: Option<String>,
    /// The current pet dashboard view, folded by the serve loop from the latest
    /// snapshot and the renderer-local asset cache before drawing. Render reads
    /// this data only; it never fetches, decodes, or slices pet assets.
    pub(in crate::sidebar_pane) pet: Option<PetView>,
    /// Pane-local context-meter interning state, persisted across frames so a
    /// quantized raster keeps its image id while ratatui diffs placeholders.
    pub(in crate::sidebar_pane) meter_pixels: Option<MeterPixels>,
    pub(in crate::sidebar_pane) card_heights: super::heights::CardHeights,
    /// Renderer-local status for a successful fetch that the regression gate is
    /// holding behind the last good frame. It is display-only evidence for the
    /// bottom chrome; the durable record is `gate_hold`/`gate_release`.
    pub(in crate::sidebar_pane) gate_notice: Option<GateNotice>,
    /// The distinct non-normal modes of the clients viewing this pane, in
    /// `RoomMode` order, set at each routed composition and empty in a mirror.
    pub(in crate::sidebar_pane) room_modes: Vec<crate::pane::RoomMode>,
}

/// A jumpable delegation entry under the selected card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::sidebar_pane) enum EntryCursor {
    SubAgent(PaneId),
}

/// Driver-clock milliseconds since the view context's animation start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::sidebar_pane) struct FrameTime(u64);

impl FrameTime {
    pub(in crate::sidebar_pane) const fn from_millis(elapsed_ms: u64) -> Self {
        Self(elapsed_ms)
    }

    pub(in crate::sidebar_pane) fn elapsed_since(self, earlier: Self) -> std::time::Duration {
        std::time::Duration::from_millis(self.0.saturating_sub(earlier.0))
    }

    pub(in crate::sidebar_pane) fn elapsed_frames_since(
        self,
        earlier: Self,
        refresh_ms: u16,
    ) -> u64 {
        let grid = u64::from(refresh_ms.max(1));
        (self.0 / grid).saturating_sub(earlier.0 / grid)
    }
}

/// Client presentation state, independent of a pane's render resources and hit geometry.
#[derive(Clone, Debug, Default)]
pub(in crate::sidebar_pane) struct ViewState {
    pub(in crate::sidebar_pane) layout_sequence: u64,
    pub(in crate::sidebar_pane) layout_key: Option<LayoutKey>,
    pub(in crate::sidebar_pane) hook_drainer_alert: Option<Alert>,
    pub(in crate::sidebar_pane) selected_index: Option<usize>,
    pub(in crate::sidebar_pane) entry_cursor: Option<EntryCursor>,
    /// Last agreed working-pane focus, finer-grained than the owning card baseline.
    pub(in crate::sidebar_pane) entry_focus: Option<PaneId>,
    /// Driver-clock phase for cyclic sampling only.
    /// Frame-sequence heads read `head_step` instead, so late paints never skip frames.
    pub(in crate::sidebar_pane) animation_phase: u64,
    /// Frame-sequence step, advanced only by routed composition, once per new phase.
    /// The first composition holds at zero; co-viewer paints in one phase share a step.
    pub(in crate::sidebar_pane) head_step: u64,
    /// Phase and `refresh_ms` at the last head advance, or at the first composition
    /// or grid change; absent before painting.
    pub(in crate::sidebar_pane) head_anchor: Option<(u64, u16)>,
    /// Playback pins cosmetic cycles, head steps, and pet frame time outside motion windows,
    /// leaving money rolls and scrollbar fade on the unpinned frame clock.
    pub(in crate::sidebar_pane) cosmetic_phase: Option<u64>,
    /// The one clock for effect lifetimes: elapsed driver milliseconds since the view's
    /// animation start, stamped beside the cyclic phase.
    pub(in crate::sidebar_pane) frame_ms: FrameTime,
    /// The cockpit spend's count-up state — one stepped roll for today's `$`.
    /// Folded forward on each data refresh (`TallyAnim::observe`) and read by the
    /// renderer at `frame_ms`; the serve loop keeps the fast tick alive
    /// while a roll is in flight. Crate-internal: an implementation detail of the
    /// renderer, not part of the public `ViewState` surface.
    pub(in crate::sidebar_pane) tally: TallyAnim,
    /// The highest cockpit spend displayed within the current headline-window
    /// epoch. Producer frames before the epoch field leave this inert.
    pub(in crate::sidebar_pane) spend_ratchet: SpendRatchet,
    /// The agent cards' `$cost` count-up state — one stepped roll per row,
    /// keyed by the row's durable id so a reorder or refresh re-anchors a
    /// climb to its agent. Folded next to `tally` on each data refresh
    /// (`CostRolls::observe`, which also prunes departed rows) and read by the
    /// card at `frame_ms`; ORed into the serve loop's animation gate
    /// beside the tally. Crate-internal, like `tally`.
    pub(in crate::sidebar_pane) cost_rolls: CostRolls,
    /// The pane the highlight is pinned to — selection keyed by identity, not
    /// position. Re-derived each fold by `app::reconcile_selection` from the
    /// derived `baseline_pane` and any live `browse`. Keying on the pane means
    /// a status-churn reorder re-anchors the highlight to the same pane
    /// instead of sliding it onto a neighbour.
    pub(in crate::sidebar_pane) selected_pane: Option<PaneId>,
    /// The hold-last baseline from this view's agreed working-pane focus.
    /// It advances on a `Some` derivation and holds across a `None` (sidebar
    /// focus, disagreement between co-viewers, or a focused pane without a row).
    /// Arrival from another pane to an unlisted destination clears it with the selection.
    pub(in crate::sidebar_pane) baseline_pane: Option<PaneId>,
    pub(in crate::sidebar_pane) previous_card: Option<PaneId>,
    /// Whether arrival or the last own-view fold found this view's tab on screen.
    /// Projection arms arrivals, preserving that latch through older hidden
    /// catch-up content. While a view stays routed, an off-screen→on-screen switch
    /// also arms the tab-read dwell; leaving disarms it. `None` seeds an
    /// already-viewed tab without sweeping.
    pub(in crate::sidebar_pane) viewing_own_tab: Option<bool>,
    /// The transient motion or click browse pick riding above the baseline, or `None`
    /// when not browsing (see [`Browse`]).
    pub(in crate::sidebar_pane) browse: Option<Browse>,
    /// The row/group order and visible row set actually painted last; this
    /// is the source for a layout freeze. Empty before the first paint, so a
    /// hold without a painted order adopts its first frame's order.
    pub(in crate::sidebar_pane) last_order: FrozenOrder,
    /// The armed layout freeze, or `None` while rows
    /// rank and cap live.
    pub(in crate::sidebar_pane) freeze: Option<LayoutFreeze>,
    /// Preserve the last painted order until the arriving pane's first composition, even when its hold has expired.
    pub(in crate::sidebar_pane) arrival_pending: bool,
    /// Target first scroll-zone content line of the agent-cards viewport.
    /// Resolved around the previous target window's selected or topmost card, or the
    /// held newborn's group header, clamped to the zone, and written back like frame
    /// interactions. Only this sidebar's own input reveals an off-screen card or a
    /// clipped entry within it, besides a birth reveal of a newborn under the focus.
    /// The line actually shown is [`Self::scroll_ease`]'s visible row.
    /// Travels with the client view between painting panes.
    pub(in crate::sidebar_pane) scroll_offset: usize,
    /// The glide from the last shown line toward `scroll_offset`: armed by a wheel
    /// tick or a birth reveal of at most one viewport, observed at every draw's
    /// write-back, translated by card corrections, and snapped by selection changes
    /// and explicit jumps.
    pub(in crate::sidebar_pane) scroll_ease: ScrollEase,
    /// Previous target-window placements relative to the scroll zone, independent of the wheel's painted position.
    pub(in crate::sidebar_pane) anchored_rows: Vec<(String, isize)>,
    /// Whether the anchored frame had a banner; a banner toggle preserves the card's pane row.
    pub(in crate::sidebar_pane) anchor_banner_shown: bool,
    /// Row id the last draw treated as selected; a different id is a selection change.
    pub(in crate::sidebar_pane) drawn_selection: Option<String>,
    /// Previous selections' own cards, kept open while the layout freeze is
    /// armed. Written back each draw; departed and live-selected ids drop out.
    /// Scrolling and toggles retain the set until the freeze releases it together
    /// with the order.
    /// A shared landing paint uses the source frame's carried set instead of
    /// these local candidates and the previous selection.
    /// Forked views retain these sources and the drawn selection.
    pub(in crate::sidebar_pane) held_expansions: BTreeSet<String>,
    /// Request stamp of the last jump scroll anchor this renderer applied, so
    /// its later acceptance cannot seed the viewport a second time. `0` before
    /// any jump handoff.
    pub(in crate::sidebar_pane) last_focus_anchor_ms: u64,
    /// What the next paint does with the viewport, issued through [`Self::request`].
    pub(in crate::sidebar_pane) viewport: ViewportRequest,
    /// Rows new to this view's presented rows, by row id, each dropped at the first fold
    /// past its birth window or once its row leaves, or moved to the new id when a
    /// provider registration adopts its card. Written by reconciliation and marked laid
    /// out by the draw, read by the offset-zero rule and the birth reveal.
    pub(in crate::sidebar_pane) newborns: BTreeMap<String, Newborn>,
    /// The card a birth reveal showed, whose group header anchors the view until the card
    /// leaves `Fresh`, the selection or the roster, or input makes the offset authoritative.
    /// A provider registration that adopts the card under a new id carries it.
    pub(in crate::sidebar_pane) birth_hold: Option<String>,
    /// The loop owns the dwell clock; render only consumes readiness.
    pub(in crate::sidebar_pane) focus_marker_ready: bool,
    /// The row id a manual mark-unread just reopened while it is the focused
    /// pane. While armed, focus-read auto-clear skips that row; moving focus
    /// off it releases the guard so a later revisit clears normally.
    pub(in crate::sidebar_pane) unread_guard: Option<String>,
    /// The agent-cards scrollbar's auto-hide fade: every draw folds the
    /// visible viewport offset into it as a write-back byproduct, and the
    /// `auto` scrollbar mode reads it to paint the bar only while the viewport
    /// moves plus a short settle window. Crate-internal, like `tally`.
    pub(in crate::sidebar_pane) scrollbar: ScrollbarFade,
    /// The last selection-derived agent kind the dashboard followed — its
    /// hold-last, one level finer than `baseline_pane`. Reconciliation
    /// advances it on an agent selection that has a dashboard panel and holds
    /// it across a non-agent selection, so focusing a shell row keeps the last
    /// agent's provider block instead of snapping to the first-panel default.
    /// Read by `active_dashboard_tab` below the live derivation and re-guarded
    /// against the panel still being on the dashboard.
    pub(in crate::sidebar_pane) last_agent_kind: Option<crate::ids::AgentKind>,
    /// Last selected credential per dashboard kind, retained while that kind
    /// has a panel and used only while the credential is still carried there.
    pub(in crate::sidebar_pane) last_sub_providers: BTreeMap<crate::ids::AgentKind, String>,
    /// The in-pane keys pressed so far toward a longer bound sequence, empty
    /// when none waits. It has no timeout: the next key resolves or cancels it.
    pub(in crate::sidebar_pane) pending_keys: Vec<crate::config::keys::sidebar::SidebarChord>,
    /// A refusal from the last in-pane key, cleared by the next key.
    pub(in crate::sidebar_pane) notice: Option<String>,
    /// Digest of the last skipped-settings list this view dismissed. Only a later
    /// dismissal replaces it, so a list shown again after another stays hidden.
    pub(in crate::sidebar_pane) dismissed_skipped: Option<String>,
    pub(in crate::sidebar_pane) toggles: PresentationToggles,
}

/// The one-paint viewport facts input, jumps and birth reveals leave for the next draw, which
/// clears them. They accumulate across passes until that draw, and a hidden mirror draws none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::sidebar_pane) struct ViewportRequest {
    /// Wheel, banner, and toggles bypass anchoring and selection reveal for one
    /// paint; explicit row and inactive-chip reveals can still nudge their target into view.
    pub(super) offset_authoritative: bool,
    /// Armed when the user toggles the inactive peek. The next draw reveals the
    /// chip and its first newly shown receipt, if any, then consumes it.
    pub(super) inactive_reveal: bool,
    /// The row a banner, focus marker, or inactive-group click reveals minimally
    /// on the next draw, then consumed.
    pub(super) reveal_row: Option<String>,
    /// A fresh shared click placement and its source opening set for the next
    /// draw to land, then consumed.
    pub(super) focus_placement: Option<FocusPlacement>,
    /// This sidebar's input changed the card or entry selection, or its requested focus landed.
    /// The next draw may reveal that stop, then consumes the mark.
    pub(super) own_input_reveal: bool,
    /// A newborn card that took the focus: the next draw shows it whole beneath its group
    /// header and starts the birth hold, unless it lost the selection or an authoritative
    /// offset is pending.
    pub(super) birth_reveal: Option<String>,
}

/// A row new to a view: absent from the snapshot the view presented before the fold. A card
/// that takes over another card's pane under a new id is no new birth: the departed card's
/// record, if any, moves to the new id unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sidebar_pane) struct Newborn {
    /// Fold-clock milliseconds of the fold that found the row new.
    pub(in crate::sidebar_pane) born_ms: i64,
    /// A birth reveal was issued for it, so the focus reaching it again issues none.
    pub(in crate::sidebar_pane) revealed: bool,
    /// A draw has laid the row out, so the offset-zero rule no longer counts it as born.
    pub(in crate::sidebar_pane) laid_out: bool,
}

/// What a caller asks of the next paint's viewport. Each intent changes only the facts it
/// names, so every other pending fact survives. Only input dispatch, and an arrival landing
/// the user's own wheel glide (`LandGlide`), issue an intent that sets authority: a change the
/// user did not ask for never does. An authoritative offset suppresses a pending
/// jump's scroll target but keeps its held set, and drops a pending birth reveal and the
/// birth hold. `RevealBirth`, the one intent reconciliation issues, sets no authority.
pub(in crate::sidebar_pane) enum ViewportIntent {
    /// A wheel tick: the offset is authoritative and glides there.
    Wheel,
    /// The offset stays where it is, without a glide.
    Hold,
    /// `Hold`, revealing this row minimally; `None` replaces a pending row reveal.
    RevealRow(Option<String>),
    /// `Hold`, revealing the inactive chip and its first newly shown receipt.
    RevealInactive,
    /// A jump's placement for the landing paint, replacing any pending one, without a glide.
    Jump(FocusPlacement),
    /// Drop the pending jump placement.
    DropJump,
    /// This sidebar's input changed the selection: the next draw may reveal that stop.
    OwnInput,
    /// An entry step: the offset is no longer authoritative, and pending reveals wait.
    EntryStep,
    /// A view arriving mid-glide lands the glide's target: as authoritative for the user's
    /// wheel glide, and without authority for a birth reveal's.
    LandGlide,
    /// A newborn card took the focus: the next draw shows it whole beneath its header.
    RevealBirth(String),
}

/// The presentation choices a user makes in a client view. They travel with the view across
/// tab moves, mirrors and forks, and a change to any of them publishes a new layout.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::sidebar_pane) struct PresentationToggles {
    pub(in crate::sidebar_pane) help_visible: bool,
    /// The dashboard provider tab the user picked by hand (`←`/`→` or a click
    /// on a tab label), riding above the selection-derived provider. Ends like
    /// a browse: it clears when the selection-derived provider kind *genuinely*
    /// changes from the value captured at pick time (a `None` derivation — a
    /// process row — holds it), or when its target leaves the dashboard.
    /// Travels with the client view, including a fork's initial state.
    pub(in crate::sidebar_pane) dashboard_tab: Option<DashboardTab>,
    /// Shared room-runtime lens narrowing the body through one `VisibleRoster` projection.
    ///
    /// Cockpit counts remain full-fleet. Toggling a pick or auto-clearing its empty bucket preserves the query; a zero-match query stays set.
    pub(in crate::sidebar_pane) make_up_filter: BodyLens,
    pub(in crate::sidebar_pane) search_draft: Option<String>,
    /// Worktree groups unfolded by the user in this client view.
    /// Presentation state only: the snapshot carries the full roster. A user
    /// fold clears the key, as does closing the section for inactive groups.
    /// The prune clears keys when a group leaves the roster or has nothing
    /// left to fold, measured without focus, and a collapsing inactive group's
    /// key while the section is closed, such as a `+K more` key carried from
    /// active work.
    pub(in crate::sidebar_pane) expanded_groups: BTreeSet<String>,
    /// Open pages of inactive groups' receipts, `None` while closed. It travels
    /// with the view across tab moves, mirrors and forks. It opens and closes
    /// only on the user toggle (chip or `z i`), `+K older` raises its page count,
    /// and the close resets it.
    pub(in crate::sidebar_pane) inactive_peek: Option<InactivePeek>,
    /// View-owned open/closed choice for an agent card's delegation
    /// section (row id → pin), set by a header click or `z a`. It outranks the
    /// default (open under selection or `expanded` density) in both directions
    /// while the parent's user turn is the one it was taken in, and drops once
    /// that turn moves or the row leaves the snapshot.
    /// Travels with the view, including a fork's initial state.
    pub(in crate::sidebar_pane) delegation_overrides: BTreeMap<String, DelegationPin>,
}

/// A delegation section's open/closed choice and the parent's
/// `user_turn_started_at` when it was made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sidebar_pane) struct DelegationPin {
    pub(in crate::sidebar_pane) open: bool,
    pub(in crate::sidebar_pane) turn: Option<Timestamp>,
}

#[cfg(test)]
impl DelegationPin {
    /// The map entry a click on `row_id`'s delegation line makes in its current turn.
    pub(in crate::sidebar_pane) fn current(
        snapshot: &SidebarSnapshot,
        row_id: &str,
        open: bool,
    ) -> (String, Self) {
        let turn = snapshot
            .rows()
            .find(|row| row.id == row_id)
            .and_then(SidebarRow::as_agent)
            .and_then(|agent| agent.user_turn_started_at);
        (row_id.to_owned(), Self { open, turn })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::sidebar_pane) struct InactivePeek {
    pub(in crate::sidebar_pane) pages: NonZeroUsize,
}

impl Default for InactivePeek {
    fn default() -> Self {
        Self {
            pages: NonZeroUsize::MIN,
        }
    }
}

#[derive(Clone, Debug)]
pub(in crate::sidebar_pane) struct LayoutKey {
    rows: Vec<(String, isize)>,
    groups: Vec<(String, isize)>,
    top_height: usize,
    bottom_height: usize,
    content_height: usize,
    scroll_max: usize,
    toggles: PresentationToggles,
}

impl LayoutKey {
    /// Compares in place, so recomposing an unchanged layout allocates nothing.
    fn describes(&self, frame: &super::compose::ComposedFrame, view: &ViewState) -> bool {
        self.rows == frame.anchored_rows
            && self.groups == frame.group_placements
            && self.top_height == frame.top_height
            && self.bottom_height == frame.bottom_height
            && self.content_height == frame.content_height
            && self.scroll_max == frame.scroll_max
            && self.toggles == view.toggles
    }
}

impl UiState {
    pub(in crate::sidebar_pane) fn theme(&mut self, config: &ThemeConfig) -> Rc<Theme> {
        if let Some((cached_config, theme)) = &self.theme_cache
            && cached_config == config
        {
            return Rc::clone(theme);
        }
        let theme = Rc::new(Theme::for_sidebar(config));
        self.theme_cache = Some((config.clone(), Rc::clone(&theme)));
        theme
    }

    pub(super) fn cached_theme(&self, config: &ThemeConfig) -> Option<Rc<Theme>> {
        let (cached_config, theme) = self.theme_cache.as_ref()?;
        (cached_config == config).then(|| Rc::clone(theme))
    }
}

impl ViewState {
    pub(in crate::sidebar_pane) fn advance_head_step(&mut self, refresh_ms: u16) {
        match self.head_anchor {
            // A phase no later than the anchor's on the same grid holds: a co-viewer that
            // read its clock before another pane's paint can stamp a phase one behind.
            Some((phase, refresh)) if refresh == refresh_ms && self.animation_phase <= phase => {
                return;
            }
            Some((_, refresh)) if refresh == refresh_ms => {
                self.head_step = self.head_step.wrapping_add(1);
            }
            // A changed `refresh_ms` rescales the phase (a raise lowers it), so the
            // head holds once and re-anchors rather than waiting for the phase to catch up.
            Some(_) | None => {}
        }
        self.head_anchor = Some((self.animation_phase, refresh_ms));
    }

    pub(in crate::sidebar_pane) fn request(&mut self, intent: ViewportIntent) {
        let viewport = &mut self.viewport;
        match intent {
            ViewportIntent::Wheel => {
                viewport.offset_authoritative = true;
                self.scroll_ease.arm();
            }
            ViewportIntent::Hold => {
                viewport.offset_authoritative = true;
                self.scroll_ease.cancel();
            }
            ViewportIntent::RevealRow(row) => {
                viewport.reveal_row = row;
                self.request(ViewportIntent::Hold);
            }
            ViewportIntent::RevealInactive => {
                viewport.inactive_reveal = true;
                self.request(ViewportIntent::Hold);
            }
            ViewportIntent::Jump(placement) => {
                viewport.focus_placement = Some(placement);
                self.scroll_ease.cancel();
            }
            ViewportIntent::DropJump => viewport.focus_placement = None,
            ViewportIntent::OwnInput => viewport.own_input_reveal = true,
            ViewportIntent::EntryStep => viewport.offset_authoritative = false,
            ViewportIntent::LandGlide if self.scroll_ease.birth_in_flight() => {
                self.scroll_ease.cancel();
            }
            ViewportIntent::LandGlide if self.scroll_ease.in_flight() => {
                self.request(ViewportIntent::Hold);
            }
            ViewportIntent::LandGlide => {}
            ViewportIntent::RevealBirth(row) => viewport.birth_reveal = Some(row),
        }
    }

    /// The pending jump placement, whatever the offset mode.
    pub(in crate::sidebar_pane) fn pending_jump(&self) -> Option<&FocusPlacement> {
        self.viewport.focus_placement.as_ref()
    }

    /// A copy for a hidden mirror, which draws none of the pending request, no glide and no
    /// birth hold, so it never ensures a held card or arms a birth glide.
    pub(in crate::sidebar_pane) fn hidden_mirror(&self) -> Self {
        let mut mirror = self.clone();
        mirror.viewport = ViewportRequest::default();
        mirror.scroll_ease.cancel();
        mirror.birth_hold = None;
        mirror
    }

    #[cfg(test)]
    pub(in crate::sidebar_pane) fn authoritative_offset(&self) -> bool {
        self.viewport.offset_authoritative
    }

    #[cfg(test)]
    pub(in crate::sidebar_pane) fn reveals_inactive(&self) -> bool {
        self.viewport.inactive_reveal
    }

    #[cfg(test)]
    pub(in crate::sidebar_pane) fn revealed_row(&self) -> Option<&str> {
        self.viewport.reveal_row.as_deref()
    }

    #[cfg(test)]
    pub(in crate::sidebar_pane) fn reveals_own_input(&self) -> bool {
        self.viewport.own_input_reveal
    }

    #[cfg(test)]
    pub(in crate::sidebar_pane) fn reveals_birth(&self) -> Option<&str> {
        self.viewport.birth_reveal.as_deref()
    }

    /// The first line of group `key` in the last draw's target window, zone-relative.
    pub(super) fn drawn_group_row(&self, key: &str) -> Option<isize> {
        let groups = &self.layout_key.as_ref()?.groups;
        groups.iter().find(|(id, _)| id == key).map(|&(_, row)| row)
    }

    pub(super) fn note_layout(&mut self, frame: &super::compose::ComposedFrame) {
        if self
            .layout_key
            .as_ref()
            .is_some_and(|key| key.describes(frame, self))
        {
            return;
        }
        self.layout_key = Some(LayoutKey {
            rows: frame.anchored_rows.clone(),
            groups: frame.group_placements.clone(),
            top_height: frame.top_height,
            bottom_height: frame.bottom_height,
            content_height: frame.content_height,
            scroll_max: frame.scroll_max,
            toggles: self.toggles.clone(),
        });
        self.layout_sequence += 1;
    }

    pub(super) fn effective_held_expansions(&self, roster: &VisibleRoster<'_>) -> BTreeSet<String> {
        let selected_id = self
            .selected_index
            .and_then(|index| roster.row(index))
            .map(|row| &row.id);
        let shared = self
            .pending_jump()
            .filter(|placement| Some(&placement.row_id) == selected_id)
            .map(|placement| &placement.held);
        let mut held = shared.unwrap_or(&self.held_expansions).clone();
        if shared.is_none()
            && selected_id != self.drawn_selection.as_ref()
            && let Some(previous) = &self.drawn_selection
        {
            held.insert(previous.clone());
        }
        held.retain(|id| Some(id) != selected_id && roster.ordinal_of_id(id).is_some());
        if self.freeze.is_none() {
            held.clear();
        }
        held
    }

    pub(in crate::sidebar_pane) fn entry_stops(
        &self,
        snapshot: &SidebarSnapshot,
        index: usize,
    ) -> Vec<EntryCursor> {
        if self.toggles.search_draft.is_some() {
            return Vec::new();
        }
        let roster = self.visible_roster(snapshot);
        let Some((row, expansion)) = self.card_expansion(&roster, snapshot, index) else {
            return Vec::new();
        };
        super::sections::delegation_entry_stops(
            row,
            expansion,
            &snapshot.theme.display,
            snapshot.now,
        )
    }

    /// The row id at `index` and whether its subagents and waits section is
    /// drawn open, as the card renders it.
    pub(in crate::sidebar_pane) fn delegation_open(
        &self,
        snapshot: &SidebarSnapshot,
        index: usize,
    ) -> Option<(String, bool)> {
        let roster = self.visible_roster(snapshot);
        let (row, expansion) = self.card_expansion(&roster, snapshot, index)?;
        Some((row.id.clone(), expansion.delegation))
    }

    fn card_expansion<'a>(
        &self,
        roster: &VisibleRoster<'a>,
        snapshot: &SidebarSnapshot,
        index: usize,
    ) -> Option<(&'a SidebarRow, super::sections::CardExpansion)> {
        let row = roster.row(index)?;
        let group = roster
            .groups()
            .iter()
            .find(|group| group.range().contains(&index))?;
        let held = self.effective_held_expansions(roster);
        let reach =
            super::sections::row_selection_reach(roster, group, index, self.selected_index, &held);
        let expansion = super::sections::CardExpansion::resolve(
            &self.toggles.delegation_overrides,
            snapshot.theme.display.card_density,
            row,
            reach,
        );
        Some((row, expansion))
    }

    pub(in crate::sidebar_pane) fn validate_entry_cursor(&mut self, snapshot: &SidebarSnapshot) {
        if let Some(cursor) = &self.entry_cursor
            && !self
                .selected_index
                .is_some_and(|index| self.entry_stops(snapshot, index).contains(cursor))
        {
            self.entry_cursor = None;
        }
    }

    pub(in crate::sidebar_pane) fn visible_roster<'a>(
        &self,
        snapshot: &'a SidebarSnapshot,
    ) -> VisibleRoster<'a> {
        let draft_lens = self.toggles.search_draft.as_ref().map(|draft| BodyLens {
            filter: self.toggles.make_up_filter.filter,
            query: (!draft.is_empty()).then(|| draft.clone()),
        });
        VisibleRoster::new(
            snapshot,
            draft_lens.as_ref().unwrap_or(&self.toggles.make_up_filter),
            &self.toggles.expanded_groups,
            self.freeze.as_ref().map(|hold| &hold.frozen.visible),
        )
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(in crate::sidebar_pane) struct SpendRatchet {
    epoch: Option<u64>,
    max_usd: f64,
}

/// Select the value and epoch the cockpit spend line displays and ratchets.
pub(in crate::sidebar_pane) fn cockpit_spend_target(
    snapshot: &SidebarSnapshot,
) -> Option<(f64, Option<u64>)> {
    if let Some(budget) = snapshot
        .fleet_budget
        .as_ref()
        .filter(|budget| budget.parked)
    {
        return Some((budget.spend_usd, snapshot.fleet_day_spend_epoch_secs));
    }
    snapshot
        .today_spend_live_usd
        .or_else(|| {
            snapshot
                .workspace_value_tally
                .as_ref()
                .map(|tally| tally.headline.usd)
        })
        .map(|usd| (usd, snapshot.today_spend_epoch_secs))
}

impl SpendRatchet {
    pub(in crate::sidebar_pane) fn observe(&mut self, epoch: Option<u64>, usd: f64) -> f64 {
        let Some(epoch) = epoch else {
            return usd;
        };
        if self.epoch == Some(epoch) {
            self.max_usd = self.max_usd.max(usd);
        } else {
            self.epoch = Some(epoch);
            self.max_usd = usd;
        }
        self.max_usd
    }

    pub(in crate::sidebar_pane) fn display(&self, epoch: Option<u64>, usd: f64) -> f64 {
        if epoch.is_some() && epoch == self.epoch {
            self.max_usd.max(usd)
        } else {
            usd
        }
    }
}

/// The manual dashboard-tab pick: the provider kind to show, plus the
/// selection-derived kind captured when the pick was made — the clear
/// condition, mirroring [`Browse`]: the pick holds until the derived kind
/// genuinely changes from `derived_at_start`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::sidebar_pane) struct DashboardTab {
    pub(in crate::sidebar_pane) login: crate::ids::LoginKey,
    pub(in crate::sidebar_pane) derived_at_start: Option<crate::ids::AgentKind>,
}

/// Browse pins `pane` above session focus. Motion roams every visible row
/// without moving focus; a click pins the owning card before dispatching focus.
/// Holds while the card baseline equals `baseline_at_start` and the agreed
/// working-pane focus stays unchanged, including between sibling children.
/// A `None` derivation holds both, so an inert frame never ends a browse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::sidebar_pane) struct Browse {
    pub(in crate::sidebar_pane) pane: PaneId,
    pub(in crate::sidebar_pane) baseline_at_start: Option<PaneId>,
}

/// View-owned layout freeze that keeps rows and groups stable while the user
/// is looking. Read state still clears immediately; position and held cap
/// exemptions are presentation-only.
#[derive(Clone, Debug)]
pub(in crate::sidebar_pane) struct LayoutFreeze {
    pub(in crate::sidebar_pane) frozen: FrozenOrder,
    pub(in crate::sidebar_pane) armed_ms: i64,
}

impl LayoutFreeze {
    pub(in crate::sidebar_pane) fn deadline_ms(&self, parked: bool) -> i64 {
        use crate::sidebar::timing::{LAYOUT_FREEZE, LAYOUT_FREEZE_PARKED};
        self.armed_ms
            + if parked {
                LAYOUT_FREEZE_PARKED
            } else {
                LAYOUT_FREEZE
            }
            .as_millis() as i64
    }
}

/// A sticky alert pinned to the bottom of the sidebar.
///
/// `since` is when the unhealthy episode began, so an active alert can show
/// `for Ns`. `recovered_at` is `None` while the loop is still unhealthy and
/// `Some(t)` once it healed — a recovered alert lingers as a dismissable
/// "last alert" notice rather than vanishing at the recovery observation.
#[derive(Clone, Debug)]
pub(in crate::sidebar_pane) struct Alert {
    pub(in crate::sidebar_pane) reason: String,
    pub(in crate::sidebar_pane) since: Timestamp,
    pub(in crate::sidebar_pane) recovered_at: Option<Timestamp>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::sidebar_pane) struct GateNotice {
    pub(in crate::sidebar_pane) rule: GateRule,
}

impl Alert {
    pub(in crate::sidebar_pane) fn active(reason: impl Into<String>, since: Timestamp) -> Self {
        Self {
            reason: reason.into(),
            since,
            recovered_at: None,
        }
    }

    pub(in crate::sidebar_pane) fn is_active(&self) -> bool {
        self.recovered_at.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_step_holds_first_and_same_phase_and_steps_once_over_any_gap() {
        let mut view = ViewState::default();
        let base = [(9, 0), (9, 0), (10, 1), (60, 2), (60, 2)].map(|(p, s)| (p, 100, s));
        let coviewer_inversion = [(59, 100, 2), (60, 100, 2), (61, 100, 3)];
        let raised_refresh = [(6, 1000, 3), (6, 1000, 3), (5, 1000, 3), (7, 1000, 4)];
        for (phase, refresh_ms, step) in base
            .into_iter()
            .chain(coviewer_inversion)
            .chain(raised_refresh)
        {
            view.animation_phase = phase;
            view.advance_head_step(refresh_ms);
            assert_eq!(view.head_step, step, "phase {phase} at {refresh_ms} ms");
        }
    }

    #[test]
    fn note_layout_publishes_each_toggle_and_geometry_change_once() {
        let ws = crate::sidebar_pane::app::fixtures::workspace();
        let snapshot = crate::sidebar_pane::app::fixtures::snapshot(&ws);
        let theme = Theme::fixed(false);
        let frame = super::super::compose_lines(
            &snapshot,
            None,
            &ViewState::default(),
            &UiState::default(),
            &theme,
            54,
            24,
        );
        let mut view = ViewState::default();
        view.note_layout(&frame);
        let mut published = view.layout_sequence;
        let mut assert_bumps = |view: &mut ViewState,
                                frame: &super::super::compose::ComposedFrame,
                                bumps: u64,
                                what: &str| {
            for _ in 0..2 {
                view.note_layout(frame);
            }
            published += bumps;
            assert_eq!(view.layout_sequence, published, "{what}");
        };
        type Toggle = fn(&mut ViewState);
        let toggles: [(&str, Toggle); 8] = [
            ("expanded_groups", |view| {
                view.toggles.expanded_groups.insert("group".into());
            }),
            ("inactive_peek opens", |view| {
                view.toggles.inactive_peek = Some(InactivePeek::default());
            }),
            ("inactive_peek pages", |view| {
                view.toggles.inactive_peek = Some(InactivePeek {
                    pages: NonZeroUsize::new(2).unwrap(),
                });
            }),
            ("delegation_overrides", |view| {
                view.toggles.delegation_overrides.insert(
                    "row".into(),
                    DelegationPin {
                        open: true,
                        turn: None,
                    },
                );
            }),
            ("dashboard_tab", |view| {
                view.toggles.dashboard_tab = Some(DashboardTab {
                    login: crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked(
                        "claude",
                    )),
                    derived_at_start: None,
                });
            }),
            ("make_up_filter", |view| {
                view.toggles.make_up_filter.query = Some("auth".into());
            }),
            ("help_visible", |view| view.toggles.help_visible = true),
            ("search_draft", |view| {
                view.toggles.search_draft = Some(String::new())
            }),
        ];
        for (what, toggle) in toggles {
            toggle(&mut view);
            assert_bumps(&mut view, &frame, 1, what);
        }
        type Reshape = fn(&mut super::super::compose::ComposedFrame);
        let geometry: [(&str, Reshape); 6] = [
            ("rows", |frame| frame.anchored_rows.push(("row".into(), 1))),
            ("groups", |frame| {
                frame.group_placements.push(("group".into(), 0));
            }),
            ("top_height", |frame| frame.top_height += 1),
            ("bottom_height", |frame| frame.bottom_height += 1),
            ("content_height", |frame| frame.content_height += 1),
            ("scroll_max", |frame| frame.scroll_max += 1),
        ];
        let mut changed = frame;
        for (what, change) in geometry {
            change(&mut changed);
            assert_bumps(&mut view, &changed, 1, what);
        }
        view.animation_phase += 7;
        view.frame_ms = FrameTime::from_millis(900);
        view.selected_index = Some(3);
        view.scroll_offset += 4;
        view.viewport.offset_authoritative = true;
        view.viewport.reveal_row = Some("row".into());
        view.viewport.inactive_reveal = true;
        view.viewport.own_input_reveal = true;
        assert_bumps(
            &mut view,
            &changed,
            0,
            "clock, selection and viewport request",
        );
    }
}
