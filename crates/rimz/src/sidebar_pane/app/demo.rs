use std::io;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use crate::MuxName;
use crate::SidebarSnapshot;
use crate::sidebar_pane::pets::{PetAssets, PixelPainter, detect_pet_render_caps};
use crate::sidebar_pane::render::{self, UiState};
use crate::tui::{MouseCapture, TerminalModeGuard};

struct GalleryState {
    snapshot: SidebarSnapshot,
    ui: UiState,
    pets: PetAssets,
    pixel_painter: PixelPainter,
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
    let mut pets = PetAssets::default();
    let caps = detect_pet_render_caps(mux, snapshot.theme.pets.glyphs, session_name);
    let mut pixel_painter = PixelPainter::new(mux == MuxName::Tmux);
    let anim_start = Instant::now();
    let cadence = Duration::from_millis(u64::from(refresh_ms));

    loop {
        ui.animation_phase = super::timing::wall_clock_phase(anim_start, refresh_ms);
        super::refresh_pet_view(&mut ui, &mut pets, &snapshot, caps, false);
        render::draw_to_terminal_with_ui(&mut terminal, &snapshot, None, &mut ui)?;
        if pixel_painter.needs_full_redraw(super::paintable_pet_pixel(&ui, &pets)) {
            terminal.clear()?;
            render::draw_to_terminal_with_ui(&mut terminal, &snapshot, None, &mut ui)?;
        }
        super::paint_pet_pixel(&ui, &pets, &mut pixel_painter, &mut terminal)?;

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

    let glyphs = columns
        .iter()
        .map(|(snapshot, _)| snapshot)
        .find(|snapshot| snapshot.theme.pets.enabled)
        .map(|snapshot| snapshot.theme.pets.glyphs)
        .unwrap_or_default();
    let caps = detect_pet_render_caps(mux, glyphs, session_name);
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
            pets: PetAssets::default(),
            pixel_painter: PixelPainter::with_id_base(
                id_base.wrapping_add((index as u32) << 12),
                mux == MuxName::Tmux,
            ),
        })
        .collect::<Vec<_>>();
    let anim_start = Instant::now();
    let cadence = Duration::from_millis(u64::from(refresh_ms));

    loop {
        let phase = super::timing::wall_clock_phase(anim_start, refresh_ms);
        for state in &mut states {
            state.ui.animation_phase = phase;
            super::refresh_pet_view(&mut state.ui, &mut state.pets, &state.snapshot, caps, false);
        }
        draw_gallery_to_terminal(&mut terminal, &mut states)?;
        if states.iter().any(|state| {
            state
                .pixel_painter
                .needs_full_redraw(super::paintable_pet_pixel(&state.ui, &state.pets))
        }) {
            terminal.clear()?;
            draw_gallery_to_terminal(&mut terminal, &mut states)?;
        }
        for state in &mut states {
            super::paint_pet_pixel(
                &state.ui,
                &state.pets,
                &mut state.pixel_painter,
                &mut terminal,
            )?;
        }

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
