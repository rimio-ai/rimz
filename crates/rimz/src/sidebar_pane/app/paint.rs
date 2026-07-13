//! Paint one sidebar frame, including pixel-pet image residency.

use std::io::{self, Write};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::MuxName;
use crate::SidebarSnapshot;
use crate::config::{CellAspect, PixelMode};
use crate::sidebar_pane::pets::{
    BEGIN_SYNC, END_SYNC, PetAssets, PetBody, PetViewFrame, PixelPainter, PixelRenderCaps,
    detect_pixel_render_caps, effective_render_tier, probe_cell_aspect,
};
use crate::sidebar_pane::pixel::meter::{MeterPainter, MeterPixels};
use crate::sidebar_pane::render::{self, UiState};

pub(super) struct FramePainter {
    assets: PetAssets,
    painter: PixelPainter,
    meter_painter: MeterPainter,
    caps: PixelRenderCaps,
    probed_aspect: Option<CellAspect>,
}

impl FramePainter {
    pub(super) fn new(caps: PixelRenderCaps, pixel_wrap: bool) -> Self {
        let painter = PixelPainter::new(pixel_wrap);
        let meter_painter = MeterPainter::new(painter.id_base(), pixel_wrap);
        Self {
            assets: PetAssets::default(),
            painter,
            meter_painter,
            caps,
            probed_aspect: probe_cell_aspect(),
        }
    }

    pub(super) fn with_id_base(id_base: u32, pixel_wrap: bool, caps: PixelRenderCaps) -> Self {
        Self {
            assets: PetAssets::default(),
            painter: PixelPainter::with_id_base(id_base, pixel_wrap),
            meter_painter: MeterPainter::new(id_base, pixel_wrap),
            caps,
            probed_aspect: probe_cell_aspect(),
        }
    }

    #[cfg(test)]
    pub(super) fn with_assets(assets: PetAssets, caps: PixelRenderCaps, pixel_wrap: bool) -> Self {
        Self {
            assets,
            painter: PixelPainter::with_id_base(0x120000, pixel_wrap),
            meter_painter: MeterPainter::new(0x120000, pixel_wrap),
            caps,
            probed_aspect: None,
        }
    }

    #[cfg(test)]
    pub(super) fn caps(&self) -> PixelRenderCaps {
        self.caps
    }

    #[cfg(test)]
    pub(super) fn set_caps(&mut self, caps: PixelRenderCaps) {
        self.caps = caps;
    }

    pub(super) fn refresh_caps(&mut self, mux: MuxName, session_name: &str) {
        self.refresh_caps_with(mux, session_name, detect_pixel_render_caps);
        self.probed_aspect = probe_cell_aspect();
    }

    pub(super) fn refresh_caps_with(
        &mut self,
        mux: MuxName,
        session_name: &str,
        detect: impl FnOnce(MuxName, &str, PixelRenderCaps) -> PixelRenderCaps,
    ) {
        self.caps = detect(mux, session_name, self.caps);
    }

    pub(super) fn refresh_view(
        &mut self,
        ui: &mut UiState,
        snapshot: &SidebarSnapshot,
        alert_active: bool,
    ) {
        let action = render::selected_pet_action(snapshot, ui);
        let theme = ui.theme(&snapshot.theme);
        let tier = effective_render_tier(
            snapshot.theme.pets.glyphs,
            snapshot.theme.display.pixel,
            self.caps,
            !snapshot.providers.is_empty() && render::pet_body_enabled(snapshot),
        );
        let body = (snapshot.theme.pets.enabled
            && render::dashboard_present(snapshot, alert_active)
            && render::pet_body_enabled(snapshot))
        .then_some(tier);
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
        ui.meter_pixels = (self.caps.pixel_transport
            && self.caps.kitty_term
            && snapshot.theme.display.pixel == PixelMode::Auto)
            .then(|| MeterPixels::new(self.painter.id_base()));
    }

    pub(super) fn draw_and_paint<W: Write>(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<W>>,
        snapshot: &SidebarSnapshot,
        alert: Option<&render::Alert>,
        ui: &mut UiState,
    ) -> io::Result<()> {
        terminal.backend_mut().write_all(BEGIN_SYNC)?;
        let body_result = (|| {
            let now_ms = u64::from(snapshot.theme.display.resolved_refresh_ms())
                .saturating_mul(ui.animation_phase);
            self.ensure_pixel_transmitted(terminal.backend_mut(), ui, now_ms)?;
            render::draw_to_terminal_with_ui(terminal, snapshot, alert, ui)?;
            if let Some(pixels) = &ui.meter_pixels {
                for (slot, spec) in pixels.specs.iter().enumerate() {
                    self.meter_painter.ensure_transmitted(
                        terminal.backend_mut(),
                        slot,
                        spec,
                        now_ms,
                    )?;
                }
            }
            Ok(())
        })();
        let end_result = terminal.backend_mut().write_all(END_SYNC);
        let flush_result = terminal.backend_mut().flush();
        body_result.and(end_result).and(flush_result)
    }

    pub(super) fn ensure_pixel_transmitted<W: Write>(
        &mut self,
        writer: &mut W,
        ui: &UiState,
        now_ms: u64,
    ) -> io::Result<()> {
        if let Some(PetBody::Pixel(pixel)) = ui.pet.as_ref().and_then(|view| view.body.as_ref())
            && let Some(frame) = self.assets.pixel_frame(&pixel.pet_id, pixel.sprite_index)
        {
            self.painter
                .ensure_transmitted(writer, pixel, frame, now_ms)?;
        }
        Ok(())
    }

    pub(super) fn clear<W: Write>(&mut self, backend: &mut W) -> io::Result<()> {
        self.painter.clear(backend)?;
        self.meter_painter.clear(backend)
    }
}
