//! Paint one sidebar frame, including pixel-pet image residency.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use ratatui::Terminal;
use tracing::warn;

use crate::MuxName;
use crate::RuntimePaths;
use crate::config::{CellAspect, PixelMode};
use crate::sidebar_pane::pets::{
    PetAssets, PetBody, PetRenderTier, PetViewFrame, PixelPainter, effective_render_tier,
};
use crate::sidebar_pane::pixel::meter::{MeterPainter, MeterPixels};
use crate::sidebar_pane::pixel::probe::CAPS_REFRESH_INTERVAL;
use crate::sidebar_pane::pixel::{BEGIN_SYNC, END_SYNC, PixelLease, PixelRenderCaps, PixelSlot};
use crate::sidebar_pane::render::{self, UiState, ViewState};
use crate::store::snapshot::SidebarSnapshot;

use super::backend::{BufferedFrame, PaneBackend};

/// Spacing between slot requests while a denied pane stays on screen.
const SLOT_RETRY: Duration = Duration::from_secs(2);

/// A held pixel slot and every value derived from its image ids. Dropping it
/// frees the slot and forgets what was transmitted, without deletes.
struct HeldSlot {
    slot: PixelSlot,
    /// `None` only for test painters that start on a fixed slot.
    _lease: Option<PixelLease>,
    pet: PixelPainter,
    meter: MeterPainter,
    swept: bool,
    retire: Retire,
}

/// Where a held slot is in returning after routed frames lose their pixel surface.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Retire {
    Kept,
    /// A routed pass found no pixel surface; its frame's deletes are not written yet.
    Surfaceless,
    /// A routed frame carrying the deletes was written; the next routed pass
    /// without a surface returns the slot.
    DeletesWritten,
}

impl HeldSlot {
    fn new(slot: PixelSlot, lease: Option<PixelLease>, wrap: bool) -> Self {
        Self {
            slot,
            _lease: lease,
            pet: PixelPainter::with_slot(slot, wrap),
            meter: MeterPainter::new(wrap),
            swept: false,
            retire: Retire::Kept,
        }
    }

    /// Unbracketed: the caller's frame carries the synchronized-output bracket.
    fn sweep_once<W: Write>(&mut self, writer: &mut W, wrap: bool) -> io::Result<()> {
        if !self.swept {
            self.slot.sweep(writer, wrap)?;
            self.swept = true;
        }
        Ok(())
    }
}

enum SlotState {
    Free {
        retry_at: Option<Instant>,
        /// This denial episode has warned; it ends when a take succeeds.
        warned: bool,
    },
    Held(Box<HeldSlot>),
}

#[derive(Clone, Copy)]
pub(super) enum PaintKind {
    Routed,
    Mirror,
}

pub(super) struct FramePainter {
    assets: PetAssets,
    caps: PixelRenderCaps,
    last_caps_refresh: Instant,
    probed_aspect: Option<CellAspect>,
    /// The runtime whose shared root holds the user-wide slot locks.
    slot_pool: RuntimePaths,
    slot: SlotState,
    pixel_wrap: bool,
    #[cfg(test)]
    _slot_pool_dir: Option<tempfile::TempDir>,
}

impl FramePainter {
    #[cfg(feature = "testkit")]
    pub(super) fn prepare_play_pet(
        &mut self,
        view: &ViewState,
        ui: &mut UiState,
        snapshot: &SidebarSnapshot,
        now: Instant,
    ) -> bool {
        self.refresh_view(ui, view, snapshot, false, PaintKind::Routed, now);
        let ready = self.assets.settle_load(std::time::Duration::from_secs(5));
        self.refresh_view(ui, view, snapshot, false, PaintKind::Routed, now);
        ready
    }

    /// A painter already holding `slot` without its lock, over a private pool.
    #[cfg(test)]
    pub(super) fn with_slot(slot: Option<PixelSlot>, caps: PixelRenderCaps) -> Self {
        Self::test_held(PetAssets::default(), caps, false, slot)
    }

    #[cfg(test)]
    pub(super) fn with_assets(assets: PetAssets, caps: PixelRenderCaps, pixel_wrap: bool) -> Self {
        Self::test_held(assets, caps, pixel_wrap, Some(PixelSlot::new(0)))
    }

    #[cfg(test)]
    pub(super) fn test_held(
        assets: PetAssets,
        caps: PixelRenderCaps,
        pixel_wrap: bool,
        slot: Option<PixelSlot>,
    ) -> Self {
        let dir = tempfile::tempdir().expect("slot pool dir");
        let pool = RuntimePaths::under(crate::sidebar_pane::app::fixtures::workspace(), dir.path())
            .expect("slot pool");
        let mut painter = Self::new(caps, pixel_wrap, pool);
        painter.assets = assets;
        if let Some(slot) = slot {
            painter.slot = SlotState::Held(Box::new(HeldSlot::new(slot, None, pixel_wrap)));
        }
        painter._slot_pool_dir = Some(dir);
        painter
    }

    #[cfg(test)]
    pub(super) fn set_assets(&mut self, assets: PetAssets) {
        self.assets = assets;
    }

    pub(super) fn new(caps: PixelRenderCaps, pixel_wrap: bool, slot_pool: RuntimePaths) -> Self {
        Self {
            assets: PetAssets::default(),
            caps,
            last_caps_refresh: Instant::now(),
            probed_aspect: None,
            slot_pool,
            slot: SlotState::Free {
                retry_at: None,
                warned: false,
            },
            pixel_wrap,
            #[cfg(test)]
            _slot_pool_dir: None,
        }
    }

    /// Return the slot without writing: the pane's frame already carries no ids,
    /// and the next holder's sweep retires the images left behind. A free
    /// painter keeps its denial episode.
    pub(super) fn release_slot(&mut self, ui: &mut UiState) {
        if !self.holds_slot() {
            return;
        }
        self.slot = SlotState::Free {
            retry_at: None,
            warned: false,
        };
        ui.meter_pixels = None;
    }

    /// A routed frame was written; deletes it carried now let the slot retire.
    pub(super) fn routed_frame_written(&mut self) {
        if let SlotState::Held(held) = &mut self.slot
            && held.retire == Retire::Surfaceless
        {
            held.retire = Retire::DeletesWritten;
        }
    }

    /// A hidden pass ends any on-screen retry spacing, so the next arrival
    /// tries for a slot at once.
    pub(super) fn clear_slot_retry(&mut self) {
        if let SlotState::Free { retry_at, .. } = &mut self.slot {
            *retry_at = None;
        }
    }

    fn take_slot_if_due(&mut self, ui: &mut UiState, now: Instant) {
        let SlotState::Free { retry_at, warned } = &mut self.slot else {
            return;
        };
        if retry_at.is_some_and(|at| now < at) {
            return;
        }
        let denial = match PixelLease::acquire(&self.slot_pool) {
            Ok(Some(lease)) => {
                self.slot = SlotState::Held(Box::new(HeldSlot::new(
                    lease.slot,
                    Some(lease),
                    self.pixel_wrap,
                )));
                ui.meter_pixels = None;
                return;
            }
            Ok(None) => None,
            Err(err) => Some(err),
        };
        *retry_at = Some(now + SLOT_RETRY);
        if std::mem::replace(warned, true) {
            return;
        }
        match denial {
            None => warn!(
                "sidebar pixel slots are all held; this pane shows an empty pet box and cell \
                 meters, and retries"
            ),
            Some(err) => warn!(
                error = %err,
                "sidebar pixel slot lock failed; this pane shows an empty pet box and cell \
                 meters, and retries"
            ),
        }
    }

    /// The painted pane's cell aspect; `None` keeps the configured one.
    pub(super) fn set_probed_aspect(&mut self, aspect: Option<CellAspect>) {
        self.probed_aspect = aspect;
    }

    #[cfg(test)]
    pub(super) fn caps(&self) -> PixelRenderCaps {
        self.caps
    }

    #[cfg(test)]
    pub(super) fn set_caps(&mut self, caps: PixelRenderCaps) {
        self.caps = caps;
    }

    pub(super) fn refresh_caps_with(
        &mut self,
        mux: MuxName,
        session_name: &str,
        cell_aspect: Option<CellAspect>,
        detect: impl FnOnce(MuxName, &str, PixelRenderCaps) -> PixelRenderCaps,
    ) -> bool {
        let previous = (self.caps, self.probed_aspect);
        self.caps = detect(mux, session_name, self.caps);
        self.last_caps_refresh = Instant::now();
        self.probed_aspect = cell_aspect;
        (self.caps, self.probed_aspect) != previous
    }

    pub(super) fn refresh_caps_if_stale_with(
        &mut self,
        mux: MuxName,
        session_name: &str,
        now: Instant,
        detect: impl FnOnce(MuxName, &str, PixelRenderCaps) -> PixelRenderCaps,
    ) -> bool {
        if mux != MuxName::Tmux
            || now.saturating_duration_since(self.last_caps_refresh) < CAPS_REFRESH_INTERVAL
        {
            return false;
        }
        let previous = self.caps;
        self.caps = detect(mux, session_name, previous);
        self.last_caps_refresh = now;
        self.caps != previous
    }

    /// A routed frame takes a slot when a pixel surface resolves; a mirror draws
    /// no image ids, and the caller returns the slot once that frame is written.
    pub(super) fn refresh_view(
        &mut self,
        ui: &mut UiState,
        view: &ViewState,
        snapshot: &SidebarSnapshot,
        alert_active: bool,
        kind: PaintKind,
        now: Instant,
    ) {
        let action = render::selected_pet_action(snapshot, view);
        let theme = ui.theme(&snapshot.theme);
        let pet_body_enabled = theme.pet_body_enabled();
        let tier = effective_render_tier(
            snapshot.theme.pets.glyphs,
            snapshot.theme.display.pixel,
            self.caps,
            !snapshot.providers.is_empty() && pet_body_enabled,
        );
        let meter_surface = self.caps.pixel_transport
            && self.caps.kitty_clients
            && snapshot.theme.display.pixel == PixelMode::Auto;
        let pixel_surface =
            meter_surface || (snapshot.theme.pets.enabled && tier == PetRenderTier::Pixel);
        match kind {
            PaintKind::Mirror => {}
            PaintKind::Routed if pixel_surface => {
                self.take_slot_if_due(ui, now);
                if let SlotState::Held(held) = &mut self.slot {
                    held.retire = Retire::Kept;
                }
            }
            PaintKind::Routed => {
                if let SlotState::Held(held) = &mut self.slot {
                    if held.retire == Retire::DeletesWritten {
                        self.release_slot(ui);
                    } else {
                        held.retire = Retire::Surfaceless;
                    }
                }
            }
        }
        let id_base = match (&self.slot, kind) {
            (SlotState::Held(held), PaintKind::Routed) => Some(held.slot.base()),
            _ => None,
        };
        let body =
            (render::pets_on_dashboard(snapshot, alert_active) && pet_body_enabled).then_some(tier);
        let unread_triggered = if snapshot.theme.pets.enabled {
            self.assets
                .observe_unread_rows(render::unread_pet_row_ids(snapshot))
        } else {
            false
        };
        let cell_aspect = snapshot
            .theme
            .pets
            .cell_aspect
            .or(self.probed_aspect)
            .unwrap_or(CellAspect::NEUTRAL);
        ui.pet = self.assets.view(
            &snapshot.theme.pets,
            PetViewFrame {
                action,
                phase: view.cosmetic_phase.unwrap_or(view.animation_phase),
                frame_ms: if view.cosmetic_phase.is_some() {
                    Default::default()
                } else {
                    view.frame_ms
                },
                refresh_ms: snapshot.theme.display.resolved_refresh_ms(),
                body,
                pixel_id_base: id_base,
                cell_aspect,
                motion_enabled: render::pet_motion_enabled(&theme.animations, action),
                unread_triggered,
            },
        );
        if let SlotState::Held(held) = &mut self.slot
            && (!snapshot.theme.pets.enabled || tier != PetRenderTier::Pixel)
        {
            held.pet.release_process_payload();
        }
        match id_base.filter(|_| meter_surface) {
            Some(base) => {
                ui.meter_pixels
                    .get_or_insert_with(|| MeterPixels::new(base))
                    .begin_frame();
            }
            None => ui.meter_pixels = None,
        }
    }

    #[cfg(test)]
    pub(super) fn draw_and_paint(
        &mut self,
        terminal: &mut Terminal<PaneBackend>,
        snapshot: &SidebarSnapshot,
        alert: Option<&render::Alert>,
        view: &mut ViewState,
        ui: &mut UiState,
    ) -> io::Result<()> {
        let (body_result, frame) =
            self.compose_frame(terminal, snapshot, alert, view, ui, PaintKind::Routed);
        body_result.and(frame.write(terminal.backend_mut()))?;
        self.routed_frame_written();
        Ok(())
    }

    pub(super) fn compose_frame(
        &mut self,
        terminal: &mut Terminal<PaneBackend>,
        snapshot: &SidebarSnapshot,
        alert: Option<&render::Alert>,
        view: &mut ViewState,
        ui: &mut UiState,
        kind: PaintKind,
    ) -> (io::Result<()>, BufferedFrame) {
        terminal.backend_mut().begin_frame();
        let mut graphics = Vec::new();
        let mut bracket = false;
        let body_result = (|| {
            if std::mem::take(&mut ui.clear_pending) {
                terminal.clear()?;
            }
            let now_ms = u64::from(snapshot.theme.display.resolved_refresh_ms())
                .saturating_mul(view.animation_phase);
            let transmit_pixels = matches!(kind, PaintKind::Routed);
            if transmit_pixels {
                self.ensure_pixel_transmitted(&mut graphics, ui, now_ms)?;
                bracket = !graphics.is_empty();
                terminal.backend_mut().write_all(&graphics)?;
                graphics.clear();
            }
            render::draw_to_terminal(terminal, snapshot, alert, view, ui)?;
            if transmit_pixels {
                self.ensure_meters_transmitted(&mut graphics, ui, now_ms)?;
            }
            bracket |= !graphics.is_empty();
            terminal.backend_mut().write_all(&graphics)
        })();
        let width = terminal.get_frame().area().width;
        (
            body_result,
            terminal
                .backend_mut()
                .take_frame(bracket, width, matches!(kind, PaintKind::Mirror)),
        )
    }

    pub(super) fn ensure_pixel_transmitted<W: Write>(
        &mut self,
        writer: &mut W,
        ui: &UiState,
        now_ms: u64,
    ) -> io::Result<()> {
        let SlotState::Held(held) = &mut self.slot else {
            return Ok(());
        };
        if held.pet.pet_id.is_none() {
            held.pet.clear(writer)?;
        }
        let Some(PetBody::Pixel(pixel)) = ui.pet.as_ref().and_then(|view| view.body.as_ref())
        else {
            return Ok(());
        };
        held.sweep_once(writer, self.pixel_wrap)?;
        if let Some(png) = self.assets.pixel_frame(&pixel.pet_id, pixel.sprite_index) {
            held.pet.ensure_transmitted(writer, pixel, png, now_ms)?;
        }
        Ok(())
    }

    pub(super) fn ensure_meters_transmitted<W: Write>(
        &mut self,
        writer: &mut W,
        ui: &UiState,
        now_ms: u64,
    ) -> io::Result<()> {
        let SlotState::Held(held) = &mut self.slot else {
            return Ok(());
        };
        let Some(pixels) = &ui.meter_pixels else {
            return held.meter.clear(writer);
        };
        held.sweep_once(writer, self.pixel_wrap)?;
        for (image_id, raster) in pixels.visible_rasters() {
            held.meter
                .ensure_transmitted(writer, image_id, raster, now_ms)?;
        }
        Ok(())
    }

    /// Delete every resident image, then return the slot.
    pub(super) fn clear<W: Write>(&mut self, backend: &mut W) -> io::Result<()> {
        let SlotState::Held(held) = &mut self.slot else {
            return Ok(());
        };
        let mut graphics = Vec::new();
        held.pet.clear(&mut graphics)?;
        held.meter.clear(&mut graphics)?;
        self.slot = SlotState::Free {
            retry_at: None,
            warned: false,
        };
        if graphics.is_empty() {
            return Ok(());
        }
        backend.write_all(BEGIN_SYNC)?;
        let body_result = backend.write_all(&graphics);
        let end_result = backend.write_all(END_SYNC);
        let flush_result = backend.flush();
        body_result.and(end_result).and(flush_result)
    }
}
