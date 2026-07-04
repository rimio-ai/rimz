use std::io::{self, Write};
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use crate::MuxName;
use crate::SidebarSnapshot;
use crate::sidebar_pane::pets::{
    BEGIN_SYNC, END_SYNC, PetRenderCaps, PixelPainter, detect_pet_render_caps,
};
use crate::sidebar_pane::render::{self, UiState};
use crate::tui::{MouseCapture, TerminalModeGuard};

use super::paint::FramePainter;

struct GalleryState {
    snapshot: SidebarSnapshot,
    ui: UiState,
    paint: FramePainter,
}

pub fn serve_fixture(
    snapshot: SidebarSnapshot,
    refresh_ms: u16,
    mux: MuxName,
    session_name: &str,
) -> super::Result<()> {
    let refresh_ms = refresh_ms.max(1);
    let _input_mode = TerminalModeGuard::enable(MouseCapture::Off)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut ui = UiState::default();
    let caps = detect_pet_render_caps(mux, session_name, PetRenderCaps::default());
    let mut paint = FramePainter::new(caps, mux == MuxName::Tmux);
    let anim_start = Instant::now();
    let cadence = Duration::from_millis(u64::from(refresh_ms));

    loop {
        ui.animation_phase = super::timing::wall_clock_phase(anim_start, refresh_ms);
        paint.refresh_view(&mut ui, &snapshot, false);
        paint.draw_and_paint(&mut terminal, &snapshot, None, &mut ui)?;

        if !event::poll(cadence)? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press && fixture_quit_key(key) => break,
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
    Ok(())
}

pub fn serve_gallery(
    columns: Vec<(SidebarSnapshot, usize)>,
    refresh_ms: u16,
    mux: MuxName,
    session_name: &str,
) -> super::Result<()> {
    let refresh_ms = refresh_ms.max(1);
    let _input_mode = TerminalModeGuard::enable(MouseCapture::Off)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let caps = detect_pet_render_caps(mux, session_name, PetRenderCaps::default());
    let id_base = PixelPainter::runtime_id_base();
    let mut states = columns
        .into_iter()
        .enumerate()
        .map(|(index, (snapshot, selected_index))| GalleryState {
            ui: UiState {
                selected_index,
                ..UiState::default()
            },
            snapshot,
            paint: FramePainter::with_id_base(
                id_base.wrapping_add((index as u32) << 12),
                mux == MuxName::Tmux,
                caps,
            ),
        })
        .collect::<Vec<_>>();
    let anim_start = Instant::now();
    let cadence = Duration::from_millis(u64::from(refresh_ms));

    loop {
        let phase = super::timing::wall_clock_phase(anim_start, refresh_ms);
        for state in &mut states {
            state.ui.animation_phase = phase;
            state
                .paint
                .refresh_view(&mut state.ui, &state.snapshot, false);
        }
        terminal.backend_mut().write_all(BEGIN_SYNC)?;
        let body_result = (|| {
            for state in &mut states {
                state
                    .paint
                    .ensure_pixel_transmitted(terminal.backend_mut(), &state.ui)?;
            }
            draw_gallery_to_terminal(&mut terminal, &mut states)?;
            Ok(())
        })();
        let end_result = terminal.backend_mut().write_all(END_SYNC);
        let flush_result = terminal.backend_mut().flush();
        body_result.and(end_result).and(flush_result)?;

        if !event::poll(cadence)? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press && fixture_quit_key(key) => break,
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
    Ok(())
}

fn draw_gallery_to_terminal<W: io::Write>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    states: &mut [GalleryState],
) -> io::Result<()> {
    let mut columns = states
        .iter_mut()
        .map(|state| render::GalleryColumn {
            snapshot: &state.snapshot,
            ui: &mut state.ui,
        })
        .collect::<Vec<_>>();
    render::draw_gallery_to_terminal(terminal, &mut columns)
}

fn fixture_quit_key(key: event::KeyEvent) -> bool {
    matches!(key.code, KeyCode::Esc | KeyCode::Char('q'))
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}
