//! Paint one sidebar frame, including pixel-pet overlays and redraw recovery.

use std::io::{self, Write};

use ratatui::Terminal;
use ratatui::backend::{ClearType, CrosstermBackend};
use ratatui::layout::Rect;

use crate::MuxName;
use crate::SidebarSnapshot;
use crate::sidebar_pane::pets::{
    PetAssets, PetBody, PetPixelView, PetRenderCaps, PetViewFrame, PixelPainter,
    detect_pet_render_caps, effective_render_tier,
};
use crate::sidebar_pane::render::{self, UiState};

pub(super) struct FramePainter {
    assets: PetAssets,
    painter: PixelPainter,
    caps: PetRenderCaps,
}

impl FramePainter {
    pub(super) fn new(caps: PetRenderCaps, pixel_wrap: bool) -> Self {
        Self {
            assets: PetAssets::default(),
            painter: PixelPainter::new(pixel_wrap),
            caps,
        }
    }

    pub(super) fn with_id_base(id_base: u32, pixel_wrap: bool, caps: PetRenderCaps) -> Self {
        Self {
            assets: PetAssets::default(),
            painter: PixelPainter::with_id_base(id_base, pixel_wrap),
            caps,
        }
    }

    #[cfg(test)]
    pub(super) fn with_assets(assets: PetAssets, caps: PetRenderCaps, pixel_wrap: bool) -> Self {
        Self {
            assets,
            painter: PixelPainter::new(pixel_wrap),
            caps,
        }
    }

    #[cfg(test)]
    pub(super) fn caps(&self) -> PetRenderCaps {
        self.caps
    }

    #[cfg(test)]
    pub(super) fn set_caps(&mut self, caps: PetRenderCaps) {
        self.caps = caps;
    }

    #[cfg(test)]
    pub(super) fn seed_pixel_for_test<W: Write>(
        &mut self,
        writer: &mut W,
        rect: Rect,
        pixel: &PetPixelView,
    ) -> io::Result<()> {
        let frame = self
            .assets
            .pixel_frame(&pixel.pet_id, pixel.sprite_index)
            .expect("test pixel pet has a loaded frame");
        self.painter.paint(writer, rect, pixel, frame)
    }

    pub(super) fn refresh_caps(&mut self, mux: MuxName, session_name: &str) {
        self.refresh_caps_with(mux, session_name, detect_pet_render_caps);
    }

    pub(super) fn refresh_caps_with(
        &mut self,
        mux: MuxName,
        session_name: &str,
        detect: impl FnOnce(MuxName, &str) -> PetRenderCaps,
    ) {
        self.caps = detect(mux, session_name);
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
        ui.pet = self.assets.view(
            &snapshot.theme.pets,
            PetViewFrame {
                action,
                phase: ui.animation_phase,
                refresh_ms: snapshot.theme.display.resolved_refresh_ms(),
                body,
                motion_enabled: render::pet_motion_enabled(&theme.animations, action),
                unread_triggered,
            },
        );
    }

    pub(super) fn draw_and_paint<W: Write>(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<W>>,
        snapshot: &SidebarSnapshot,
        alert: Option<&render::Alert>,
        ui: &mut UiState,
    ) -> io::Result<()> {
        render::draw_to_terminal_with_ui(terminal, snapshot, alert, ui)?;
        // `draw_into` writes the fresh pixel rect into `ui`, so placement-shift
        // recovery must run after one draw. A pre-draw check only sees the
        // previous frame's rect and misses the steady same-pet layout shift.
        if self.needs_full_redraw(ui) {
            ratatui::backend::Backend::clear_region(terminal.backend_mut(), ClearType::All)?;
            // The terminal contents are gone, so make ratatui diff against an
            // empty previous buffer on the redraw without querying the real cursor.
            terminal.swap_buffers();
            render::draw_to_terminal_with_ui(terminal, snapshot, alert, ui)?;
        }
        self.paint_after_draw(ui, terminal)
    }

    pub(super) fn needs_full_redraw(&self, ui: &UiState) -> bool {
        let next = self.paintable_pet_pixel(ui);
        self.painter
            .needs_full_redraw(next.as_ref().map(|(rect, pixel)| (*rect, pixel)))
    }

    pub(super) fn paint_after_draw<W: Write>(
        &mut self,
        ui: &UiState,
        terminal: &mut Terminal<CrosstermBackend<W>>,
    ) -> io::Result<()> {
        if let Some((rect, pixel)) = self.paintable_pet_pixel(ui) {
            let frame = self
                .assets
                .pixel_frame(&pixel.pet_id, pixel.sprite_index)
                .expect("paintable pixel pet has a loaded frame");
            return self
                .painter
                .paint(terminal.backend_mut(), rect, &pixel, frame);
        }
        self.painter.hide_after_draw(terminal.backend_mut())
    }

    pub(super) fn clear<W: Write>(&mut self, backend: &mut W) -> io::Result<()> {
        self.painter.clear(backend)
    }

    fn paintable_pet_pixel(&self, ui: &UiState) -> Option<(Rect, PetPixelView)> {
        let rect = ui.pet_pixel_rect?;
        let PetBody::Pixel(pixel) = ui.pet.as_ref()?.body.as_ref()? else {
            return None;
        };
        self.assets
            .pixel_frame(&pixel.pet_id, pixel.sprite_index)
            .map(|_| (rect, pixel.clone()))
    }
}
