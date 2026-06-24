use std::io;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use crate::SidebarSnapshot;
use crate::sidebar_pane::pets::PetAssets;
use crate::sidebar_pane::render::{self, UiState};
use crate::tui::{MouseCapture, TerminalModeGuard};

struct GalleryState {
    snapshot: SidebarSnapshot,
    ui: UiState,
    pets: PetAssets,
}

pub fn serve_fixture(snapshot: SidebarSnapshot, refresh_ms: u16) -> super::Result<()> {
    let refresh_ms = refresh_ms.max(1);
    let _input_mode = TerminalModeGuard::enable(MouseCapture::Off)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut ui = UiState::default();
    let mut pets = PetAssets::default();
    let anim_start = Instant::now();
    let cadence = Duration::from_millis(u64::from(refresh_ms));

    loop {
        ui.animation_phase = super::timing::wall_clock_phase(anim_start, refresh_ms);
        super::refresh_pet_view(
            &mut ui,
            &mut pets,
            &snapshot,
            false,
            terminal.size().ok().map(|size| (size.width, size.height)),
        );
        render::draw_to_terminal_with_ui(&mut terminal, &snapshot, None, &mut ui)?;

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

pub fn serve_gallery(snapshots: Vec<SidebarSnapshot>, refresh_ms: u16) -> super::Result<()> {
    let refresh_ms = refresh_ms.max(1);
    let _input_mode = TerminalModeGuard::enable(MouseCapture::Off)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut states = snapshots
        .into_iter()
        .map(|snapshot| GalleryState {
            snapshot,
            ui: UiState::default(),
            pets: PetAssets::default(),
        })
        .collect::<Vec<_>>();
    let anim_start = Instant::now();
    let cadence = Duration::from_millis(u64::from(refresh_ms));

    loop {
        let phase = super::timing::wall_clock_phase(anim_start, refresh_ms);
        let terminal_size = terminal
            .size()
            .ok()
            .map(|size| (gallery_column_width(size.width, states.len()), size.height));
        for state in &mut states {
            state.ui.animation_phase = phase;
            super::refresh_pet_view(
                &mut state.ui,
                &mut state.pets,
                &state.snapshot,
                false,
                terminal_size,
            );
        }
        let mut columns = states
            .iter_mut()
            .map(|state| render::GalleryColumn {
                snapshot: &state.snapshot,
                ui: &mut state.ui,
            })
            .collect::<Vec<_>>();
        render::draw_gallery_to_terminal(&mut terminal, &mut columns)?;

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

fn gallery_column_width(width: u16, column_count: usize) -> u16 {
    let count = column_count.max(1).min(usize::from(u16::MAX)) as u16;
    let delimiters = column_count.saturating_sub(1).min(usize::from(u16::MAX)) as u16;
    width.saturating_sub(delimiters) / count
}

fn fixture_quit_key(key: event::KeyEvent) -> bool {
    matches!(key.code, KeyCode::Esc | KeyCode::Char('q'))
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}
