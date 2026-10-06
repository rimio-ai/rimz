//! Fixture and gallery serve loops for previewing supplied snapshots with the renderer's animation and pixel-painting paths.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use crate::RuntimePaths;
use crate::sidebar_pane::pixel::{BEGIN_SYNC, END_SYNC, PixelLease, detect_pixel_render_env};
use crate::sidebar_pane::render::{self, UiState};
use crate::store::snapshot::SidebarSnapshot;
use crate::tui::{MouseCapture, Screen, TerminalModeGuard};

use super::paint::FramePainter;

struct GalleryState {
    snapshot: SidebarSnapshot,
    ui: UiState,
    paint: FramePainter,
    _pixel_lease: Option<PixelLease>,
}

/// Demo painters lease like live workers, so a demo never sweeps a live
/// sidebar's slot on a shared terminal surface.
fn lease_demo_slot(runtime: &RuntimePaths) -> Option<PixelLease> {
    PixelLease::acquire(runtime).ok().flatten()
}

pub fn serve_fixture(snapshot: SidebarSnapshot, refresh_ms: u16) -> super::Result<()> {
    let refresh_ms = refresh_ms.max(1);
    let _input_mode = TerminalModeGuard::enable(MouseCapture::Off, Screen::Main)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut ui = UiState::default();
    let (caps, wrap_pixels) = detect_pixel_render_env();
    let pixel_lease = lease_demo_slot(&RuntimePaths::shared());
    let mut paint = FramePainter::new(
        caps,
        wrap_pixels,
        pixel_lease.as_ref().map(|lease| lease.slot),
    );
    paint.set_probed_aspect(crate::sidebar_pane::pets::probe_cell_aspect());
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
    mut columns: Vec<(SidebarSnapshot, usize)>,
    refresh_ms: u16,
) -> super::Result<()> {
    let refresh_ms = refresh_ms.max(1);
    let _input_mode = TerminalModeGuard::enable(MouseCapture::Stdout, Screen::Main)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let cap = terminal
        .size()
        .map(|size| render::gallery_column_cap(size.width))
        .unwrap_or(3);
    columns.truncate(cap);

    let (caps, wrap_pixels) = detect_pixel_render_env();
    let runtime = RuntimePaths::shared();
    let mut states = columns
        .into_iter()
        .map(|(snapshot, selected_index)| {
            let pixel_lease = lease_demo_slot(&runtime);
            GalleryState {
                ui: UiState {
                    selected_index,
                    ..UiState::default()
                },
                snapshot,
                paint: FramePainter::new(
                    caps,
                    wrap_pixels,
                    pixel_lease.as_ref().map(|lease| lease.slot),
                ),
                _pixel_lease: pixel_lease,
            }
        })
        .collect::<Vec<_>>();
    for state in &mut states {
        state
            .paint
            .set_probed_aspect(crate::sidebar_pane::pets::probe_cell_aspect());
    }
    let anim_start = Instant::now();
    let cadence = Duration::from_millis(u64::from(refresh_ms));

    loop {
        let phase = super::timing::wall_clock_phase(anim_start, refresh_ms);
        let now_ms = u64::from(refresh_ms).saturating_mul(phase);
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
                    .ensure_pixel_transmitted(terminal.backend_mut(), &state.ui, now_ms)?;
            }
            draw_gallery_to_terminal(&mut terminal, &mut states)?;
            for state in &mut states {
                state
                    .paint
                    .ensure_meters_transmitted(terminal.backend_mut(), &state.ui, now_ms)?;
            }
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
