use std::io;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use crate::SidebarSnapshot;
use crate::sidebar_pane::pets::PetAssets;
use crate::sidebar_pane::render::{self, UiState};
use crate::tui::{MouseCapture, TerminalModeGuard};

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

fn fixture_quit_key(key: event::KeyEvent) -> bool {
    matches!(key.code, KeyCode::Esc | KeyCode::Char('q'))
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}
