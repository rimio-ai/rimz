//! Paint one sidebar frame, including pixel-pet image residency.

use std::io::{self, Write};
use std::time::Instant;

use ratatui::Terminal;

use crate::MuxName;
use crate::config::{CellAspect, PixelMode};
use crate::sidebar_pane::pets::{
    PetAssets, PetBody, PetViewFrame, PixelPainter, effective_render_tier,
};
use crate::sidebar_pane::pixel::meter::{MeterPainter, MeterPixels};
use crate::sidebar_pane::pixel::probe::CAPS_REFRESH_INTERVAL;
use crate::sidebar_pane::pixel::{BEGIN_SYNC, END_SYNC, PixelRenderCaps, PixelSlot};
use crate::sidebar_pane::render::{self, UiState};
use crate::store::snapshot::SidebarSnapshot;

use super::backend::PaneBackend;

enum PixelSession {
    Disabled,
    Pending(PixelSlot),
    Ready,
}

pub(super) struct FramePainter {
    assets: PetAssets,
    painter: PixelPainter,
    meter_painter: MeterPainter,
    caps: PixelRenderCaps,
    last_caps_refresh: Instant,
    probed_aspect: Option<CellAspect>,
    pixel_session: PixelSession,
    pixel_wrap: bool,
}

impl FramePainter {
    #[cfg(test)]
    pub(super) fn with_slot(slot: Option<PixelSlot>, caps: PixelRenderCaps) -> Self {
        Self::new(caps, false, slot)
    }

    pub(super) fn new(caps: PixelRenderCaps, pixel_wrap: bool, slot: Option<PixelSlot>) -> Self {
        let painter = PixelPainter::with_slot(slot.unwrap_or(PixelSlot::new(0)), pixel_wrap);
        let meter_painter = MeterPainter::new(pixel_wrap);
        Self {
            assets: PetAssets::default(),
            painter,
            meter_painter,
            caps,
            last_caps_refresh: Instant::now(),
            probed_aspect: None,
            pixel_session: slot.map_or(PixelSession::Disabled, PixelSession::Pending),
            pixel_wrap,
        }
    }

    #[cfg(test)]
    pub(super) fn with_assets(assets: PetAssets, caps: PixelRenderCaps, pixel_wrap: bool) -> Self {
        Self {
            assets,
            painter: PixelPainter::with_slot(PixelSlot::new(0), pixel_wrap),
            meter_painter: MeterPainter::new(pixel_wrap),
            caps,
            last_caps_refresh: Instant::now(),
            probed_aspect: None,
            pixel_session: PixelSession::Pending(PixelSlot::new(0)),
            pixel_wrap,
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

    pub(super) fn refresh_view(
        &mut self,
        ui: &mut UiState,
        snapshot: &SidebarSnapshot,
        alert_active: bool,
    ) {
        let action = render::selected_pet_action(snapshot, ui);
        let theme = ui.theme(&snapshot.theme);
        let pet_body_enabled = theme.pet_body_enabled();
        let leased = !matches!(self.pixel_session, PixelSession::Disabled);
        let tier = effective_render_tier(
            snapshot.theme.pets.glyphs,
            snapshot.theme.display.pixel,
            self.caps,
            leased && !snapshot.providers.is_empty() && pet_body_enabled,
        );
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
                phase: ui.animation_phase,
                refresh_ms: snapshot.theme.display.resolved_refresh_ms(),
                body,
                pixel_id_base: self.painter.id_base(),
                cell_aspect,
                motion_enabled: render::pet_motion_enabled(&theme.animations, action),
                unread_triggered,
            },
        );
        if !snapshot.theme.pets.enabled || tier != crate::sidebar_pane::pets::PetRenderTier::Pixel {
            self.painter.release_process_payload();
        }
        let meter_enabled = leased
            && self.caps.pixel_transport
            && self.caps.kitty_clients
            && snapshot.theme.display.pixel == PixelMode::Auto;
        if meter_enabled {
            ui.meter_pixels
                .get_or_insert_with(|| MeterPixels::new(self.painter.id_base()))
                .begin_frame();
        } else {
            ui.meter_pixels = None;
        }
    }

    pub(super) fn draw_and_paint(
        &mut self,
        terminal: &mut Terminal<PaneBackend>,
        snapshot: &SidebarSnapshot,
        alert: Option<&render::Alert>,
        ui: &mut UiState,
    ) -> io::Result<()> {
        terminal.backend_mut().begin_frame();
        let mut graphics = Vec::new();
        let mut bracket = false;
        let body_result = (|| {
            let now_ms = u64::from(snapshot.theme.display.resolved_refresh_ms())
                .saturating_mul(ui.animation_phase);
            self.ensure_pixel_transmitted(&mut graphics, ui, now_ms)?;
            bracket = !graphics.is_empty();
            terminal.backend_mut().write_all(&graphics)?;
            graphics.clear();
            render::draw_to_terminal(terminal, snapshot, alert, ui)?;
            self.ensure_meters_transmitted(&mut graphics, ui, now_ms)?;
            bracket |= !graphics.is_empty();
            terminal.backend_mut().write_all(&graphics)
        })();
        let end_result = terminal.backend_mut().end_frame(bracket);
        body_result.and(end_result)
    }

    pub(super) fn ensure_pixel_transmitted<W: Write>(
        &mut self,
        writer: &mut W,
        ui: &UiState,
        now_ms: u64,
    ) -> io::Result<()> {
        if matches!(self.pixel_session, PixelSession::Disabled) {
            return Ok(());
        }
        if self.painter.pet_id.is_none() {
            self.painter.clear(writer)?;
        }
        if matches!(
            ui.pet.as_ref().and_then(|view| view.body.as_ref()),
            Some(PetBody::Pixel(_))
        ) {
            self.sweep_if_pending(writer)?;
        }
        if let Some(PetBody::Pixel(pixel)) = ui.pet.as_ref().and_then(|view| view.body.as_ref())
            && let Some(frame) = self.assets.pixel_frame(&pixel.pet_id, pixel.sprite_index)
        {
            self.painter
                .ensure_transmitted(writer, pixel, frame, now_ms)?;
        }
        Ok(())
    }

    pub(super) fn ensure_meters_transmitted<W: Write>(
        &mut self,
        writer: &mut W,
        ui: &UiState,
        now_ms: u64,
    ) -> io::Result<()> {
        if matches!(self.pixel_session, PixelSession::Disabled) {
            return Ok(());
        }
        if let Some(pixels) = &ui.meter_pixels {
            self.sweep_if_pending(writer)?;
            for (image_id, raster) in pixels.visible_rasters() {
                self.meter_painter
                    .ensure_transmitted(writer, image_id, raster, now_ms)?;
            }
        } else {
            self.meter_painter.clear(writer)?;
        }
        Ok(())
    }

    fn sweep_if_pending<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if let PixelSession::Pending(slot) = self.pixel_session {
            slot.sweep(writer, self.pixel_wrap)?;
            self.pixel_session = PixelSession::Ready;
        }
        Ok(())
    }

    pub(super) fn clear<W: Write>(&mut self, backend: &mut W) -> io::Result<()> {
        let mut graphics = Vec::new();
        self.painter.clear(&mut graphics)?;
        self.meter_painter.clear(&mut graphics)?;
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
