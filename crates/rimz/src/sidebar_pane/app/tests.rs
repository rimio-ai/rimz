use super::input::{InputMode, KeyAction, Wakeup, resolve_one};
use super::loop_state::handle_wakeup;
use super::notify::{
    BellDecision, bell_decision, desktop_notification_targets_renderer,
    notification_targets_own_view,
};
use super::selection::InputOutcome;
use super::socket::heartbeat_write_due;
use super::timing::{next_frame_after, tick_for};
use super::*;
use crate::ids::PaneId;
use crate::sidebar::timing::HEARTBEAT_WRITE_INTERVAL;
use crate::sidebar_pane::app::fixtures::{
    agent_snapshot, focus_fixture, pane, snapshot, snapshot_with_panes, workspace,
};
use crate::sidebar_pane::pets::{PetAssets, PetPixelView};
use crate::sidebar_pane::pixel::{
    BEGIN_SYNC, END_SYNC, ImagePermit, PixelRenderCaps, placeholder_cluster,
};
use crate::sidebar_pane::render::PresentationToggles;
use crate::sidebar_pane::render::{self, UiState};
use jiff::Timestamp;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

#[derive(Clone, Default)]
struct SharedBuffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for SharedBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn kitty_commands(bytes: &[u8]) -> Vec<std::collections::BTreeMap<String, String>> {
    String::from_utf8_lossy(bytes)
        .split("\x1b_G")
        .skip(1)
        .map(|command| {
            command
                .split(';')
                .next()
                .unwrap()
                .split(',')
                .map(|field| {
                    let (key, value) = field.split_once('=').expect("kitty control field");
                    (key.to_owned(), value.to_owned())
                })
                .collect::<std::collections::BTreeMap<_, _>>()
        })
        // A continuation chunk of a long transmission carries only `m`.
        .filter(|command| command.contains_key("a"))
        .collect()
}

fn compose_test_meters(ui: &mut UiState, frame: u32) {
    let pixels = ui.meter_pixels.as_mut().expect("pixels enabled");
    let lines = (0..4)
        .map(|meter| {
            let id = pixels
                .intern(crate::sidebar_pane::pixel::meter::MeterRaster::new(
                    8,
                    f64::from((frame + meter) % 64 + 1) / 65.0,
                    [frame as u8, (frame >> 8) as u8, meter as u8],
                    Vec::new(),
                    [1, 2, 3],
                    None,
                ))
                .expect("room for visible meters");
            ratatui::text::Line::from(ratatui::text::Span::styled(
                placeholder_cluster(0, 0),
                ratatui::style::Style::default().fg(crate::sidebar_pane::pixel::image_id_color(id)),
            ))
        })
        .collect::<Vec<_>>();
    pixels.observe_visible(&lines);
}

#[test]
fn pixel_churn_and_restarts_stay_in_one_slot_with_one_placement() {
    use crate::sidebar_pane::pixel::PixelSlot;
    let snapshot = snapshot(&workspace());
    let mut ids = std::collections::BTreeSet::new();
    let mut puts = std::collections::BTreeSet::new();
    for restart in 0..4 {
        let mut painter = paint::FramePainter::with_slot(
            Some(PixelSlot::new(7)),
            PixelRenderCaps {
                pixel_transport: true,
                kitty_clients: true,
            },
        );
        let view_ui = render::ViewState::default();
        let mut ui = UiState::default();
        for frame in 0..800 {
            painter.refresh_view(&mut ui, &view_ui, &snapshot, false, ROUTED, Instant::now());
            compose_test_meters(&mut ui, restart * 800 + frame);
            let mut bytes = Vec::new();
            painter
                .ensure_meters_transmitted(&mut bytes, &ui, u64::from(frame) * 2500, &PERMIT)
                .unwrap();
            painter
                .ensure_meters_transmitted(&mut bytes, &ui, u64::from(frame) * 2500 + 2000, &PERMIT)
                .unwrap();
            for command in kitty_commands(&bytes) {
                let id: u32 = command["i"].parse().unwrap();
                assert!(
                    (0x520e00..0x521000).contains(&id),
                    "id outside leased slot: {id:x}"
                );
                ids.insert(id);
                if command["a"] == "p" {
                    assert_eq!(command.get("p").map(String::as_str), Some("1"));
                    puts.insert((id, command["p"].clone()));
                }
            }
        }
    }
    assert!(puts.len() <= ids.len());
    assert!(puts.len() <= 256);
}

#[test]
fn pixel_slot_sweep_precedes_first_transmit_once() {
    use crate::sidebar_pane::pixel::PixelSlot;
    for _restart in 0..2 {
        let mut painter = paint::FramePainter::with_slot(
            Some(PixelSlot::new(0)),
            PixelRenderCaps {
                pixel_transport: true,
                kitty_clients: true,
            },
        );
        let snapshot = snapshot(&workspace());
        let view_ui = render::ViewState::default();
        let mut ui = UiState::default();
        painter.refresh_view(&mut ui, &view_ui, &snapshot, false, ROUTED, Instant::now());
        compose_test_meters(&mut ui, 1);
        let mut bytes = Vec::new();
        painter
            .ensure_meters_transmitted(&mut bytes, &ui, 0, &PERMIT)
            .unwrap();
        let commands = kitty_commands(&bytes);
        let sweep = commands
            .iter()
            .take_while(|command| command["a"] == "d")
            .collect::<Vec<_>>();
        assert_eq!(sweep.len(), 512);
        for (index, command) in sweep.iter().enumerate() {
            assert_eq!(command["d"], "I");
            assert_eq!(
                command["i"].parse::<u32>().unwrap(),
                0x520000 + index as u32
            );
        }
        assert_eq!(
            commands[512]["a"], "t",
            "the sweep precedes the first transmit"
        );
        assert!(
            !bytes.starts_with(BEGIN_SYNC),
            "the frame's bracket carries the sweep"
        );
        let mut repeat = Vec::new();
        painter
            .ensure_meters_transmitted(&mut repeat, &ui, 2500, &PERMIT)
            .unwrap();
        assert!(
            kitty_commands(&repeat)
                .iter()
                .all(|command| command["a"] != "d")
        );
    }
}

#[test]
fn pixel_meter_only_frame_brackets_draw_sweep_and_transmit() {
    for wrap in [false, true] {
        let mut painter = paint::FramePainter::test_held(
            PetAssets::default(),
            PixelRenderCaps {
                pixel_transport: true,
                kitty_clients: true,
            },
            wrap,
            Some(crate::sidebar_pane::pixel::PixelSlot::new(0)),
        );
        let mut snapshot = agent_snapshot(&workspace());
        snapshot.theme.mode = crate::config::ThemeMode::Truecolor;
        snapshot.theme.pets.enabled = false;
        let card = std::sync::Arc::make_mut(
            &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
        )[0]
        .as_agent_mut()
        .unwrap();
        card.usage.context_pct = Some(50);
        card.usage.context_window = Some(200_000);
        let mut view_ui = render::ViewState::default();
        let mut ui = UiState::default();
        painter.refresh_view(&mut ui, &view_ui, &snapshot, false, ROUTED, Instant::now());
        assert!(ui.pet.is_none());
        assert!(
            ui.meter_pixels
                .as_ref()
                .unwrap()
                .visible_rasters()
                .next()
                .is_none()
        );
        let output = SharedBuffer::default();
        let mut terminal = Terminal::with_options(
            PaneBackend::headless(output.clone()),
            ratatui::TerminalOptions {
                viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 44, 30)),
            },
        )
        .unwrap();

        painter
            .draw_and_paint(&mut terminal, &snapshot, None, &mut view_ui, &mut ui)
            .unwrap();
        let bytes = output.0.lock().unwrap().clone();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("a=t"), "the draw composes a visible meter");
        assert!(bytes.starts_with(BEGIN_SYNC) && bytes.ends_with(END_SYNC));
        assert_eq!(text.matches("[?2026h").count(), 1);
        assert_eq!(text.matches("[?2026l").count(), 1);
        assert!(text.find(&placeholder_cluster(0, 0)).unwrap() < text.find("a=d,d=I").unwrap());
        assert!(text.find("a=d,d=I").unwrap() < text.find("a=t").unwrap());
    }
}

#[test]
fn an_unsized_pixel_frame_writes_no_sync_markers() {
    let mut pty = Pty::open(0, 0);
    let mut terminal = Terminal::with_options(
        PaneBackend::for_fd(pty.slave()).unwrap(),
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 44, 30)),
        },
    )
    .unwrap();
    let mut painter = paint::FramePainter::with_slot(
        Some(crate::sidebar_pane::pixel::PixelSlot::new(0)),
        PixelRenderCaps {
            pixel_transport: true,
            kitty_clients: true,
        },
    );
    let snapshot = snapshot(&workspace());
    let mut view_ui = render::ViewState::default();
    let mut ui = UiState::default();
    painter.refresh_view(&mut ui, &view_ui, &snapshot, false, ROUTED, Instant::now());
    painter
        .draw_and_paint(&mut terminal, &snapshot, None, &mut view_ui, &mut ui)
        .unwrap();
    assert!(pty.drain().is_empty(), "an unsized frame ships no bytes");
}

const FULL_CAPS: PixelRenderCaps = PixelRenderCaps {
    pixel_transport: true,
    kitty_clients: true,
};

fn slot_pool() -> (tempfile::TempDir, crate::disk::paths::RuntimePaths) {
    let root = tempfile::tempdir().unwrap();
    let runtime = crate::disk::paths::RuntimePaths::under(workspace(), root.path()).unwrap();
    (root, runtime)
}

fn first_free_base(pool: &crate::disk::paths::RuntimePaths) -> Option<u32> {
    crate::sidebar_pane::pixel::PixelLease::acquire(pool, &PERMIT)
        .unwrap()
        .map(|lease| lease.slot.base())
}

/// A dashboard with a pixel pet beside a 5h budget bar, and a card with a context meter.
fn slot_snapshot() -> crate::store::snapshot::SidebarSnapshot {
    let mut snapshot = agent_snapshot(&workspace());
    snapshot.theme.mode = crate::config::ThemeMode::Truecolor;
    snapshot.theme.pets.enabled = true;
    snapshot.theme.pets.pet = "codex".to_owned();
    snapshot.providers = vec![crate::sidebar::test_support::provider_panel(
        "codex",
        vec![crate::agents::RateLimitWindow {
            used_percentage: Some(50),
            duration_mins: Some(300),
            resets_at: Some(Timestamp::from_second(3600).unwrap()),
            ..Default::default()
        }],
    )];
    let card = std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .as_agent_mut()
    .unwrap();
    card.usage.context_pct = Some(50);
    card.usage.context_window = Some(200_000);
    snapshot
}

fn slot_painter(
    pool: &crate::disk::paths::RuntimePaths,
    caps: PixelRenderCaps,
) -> paint::FramePainter {
    let mut painter = paint::FramePainter::new(caps, false, pool.clone());
    painter.set_assets(PetAssets::test_loaded_pixel_frame("codex"));
    painter
}

struct SlotFrame {
    bytes: String,
    cells: Vec<((u16, u16), String, ratatui::style::Color)>,
}

impl SlotFrame {
    fn graphics(&self) -> bool {
        self.bytes.contains("\x1b_G")
    }

    fn image_ids(&self) -> Vec<u32> {
        kitty_commands(self.bytes.as_bytes())
            .into_iter()
            .map(|command| command["i"].parse().unwrap())
            .chain(self.placeholders().map(|(_, id)| id))
            .collect()
    }

    fn placeholders(&self) -> impl Iterator<Item = ((u16, u16), u32)> + '_ {
        self.cells.iter().filter_map(|(at, symbol, fg)| {
            let ratatui::style::Color::Rgb(r, g, b) = fg else {
                return None;
            };
            symbol
                .starts_with('\u{10eeee}')
                .then(|| (*at, u32::from_be_bytes([0, *r, *g, *b])))
        })
    }

    /// Placeholder cells whose image id sits in the pet's or the meter's part of a slot.
    fn placeholders_at(&self, meter: bool) -> Vec<(u16, u16)> {
        self.placeholders()
            .filter(|(_, id)| ((id - 0x520000) % 0x200 >= 0x100) == meter)
            .map(|(at, _)| at)
            .collect()
    }

    fn symbol(&self, at: (u16, u16)) -> &str {
        &self.cells.iter().find(|(cell, ..)| *cell == at).unwrap().1
    }

    fn sextants(&self) -> bool {
        self.cells.iter().any(|(_, symbol, _)| {
            symbol
                .chars()
                .any(|ch| ('\u{1fb00}'..='\u{1fb3b}').contains(&ch))
        })
    }
}

const PERMIT: ImagePermit = ImagePermit::for_test();
const ROUTED: paint::PaintKind<'static> = paint::PaintKind::Routed(Some(&PERMIT));

fn paint_slot_frame(
    painter: &mut paint::FramePainter,
    snapshot: &crate::store::snapshot::SidebarSnapshot,
    ui: &mut UiState,
    kind: paint::PaintKind<'_>,
    now: Instant,
) -> SlotFrame {
    let mut view = render::ViewState::default();
    painter.refresh_view(ui, &view, snapshot, false, kind, now);
    let output = SharedBuffer::default();
    let area = ratatui::layout::Rect::new(0, 0, 44, 40);
    let mut terminal = Terminal::with_options(
        PaneBackend::headless(output.clone()),
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(area),
        },
    )
    .unwrap();
    let (result, frame) = painter.compose_frame(&mut terminal, snapshot, None, &mut view, ui, kind);
    result.and(frame.write(terminal.backend_mut())).unwrap();
    if matches!(kind, paint::PaintKind::Routed(_)) {
        painter.routed_frame_written(ui);
    }
    let mut cells =
        Terminal::new(ratatui::backend::TestBackend::new(area.width, area.height)).unwrap();
    render::draw_to_terminal(&mut cells, snapshot, None, &mut view, ui).unwrap();
    let buffer = cells.backend().buffer();
    let bytes = String::from_utf8_lossy(&output.0.lock().unwrap()).into_owned();
    SlotFrame {
        bytes,
        cells: area
            .positions()
            .map(|at| {
                let cell = &buffer[at];
                ((at.x, at.y), cell.symbol().to_owned(), cell.fg)
            })
            .collect(),
    }
}

/// A hidden frame, the slot return the loop makes once that frame is written,
/// and the retry clear its unwatched pass makes.
fn paint_hidden_slot_frame(
    painter: &mut paint::FramePainter,
    snapshot: &crate::store::snapshot::SidebarSnapshot,
    ui: &mut UiState,
    now: Instant,
) -> SlotFrame {
    let grant = mirror_queue::Grant::for_test(PaneId::from_parts(crate::MuxName::Tmux, "%1"));
    let frame = paint_slot_frame(painter, snapshot, ui, paint::PaintKind::Mirror(&grant), now);
    painter.release_slot(ui);
    painter.clear_slot_retry();
    frame
}

#[test]
fn a_routed_frame_takes_a_slot_and_a_hidden_frame_returns_it_blank() {
    let (_root, pool) = slot_pool();
    let snapshot = slot_snapshot();
    let mut painter = slot_painter(&pool, FULL_CAPS);
    let mut ui = UiState::default();
    let now = Instant::now();
    let pet_drawn = !crate::tui::no_color();

    let routed = paint_slot_frame(&mut painter, &snapshot, &mut ui, ROUTED, now);
    let commands = kitty_commands(routed.bytes.as_bytes());
    assert!(commands.len() > 512);
    for (index, command) in commands[..512].iter().enumerate() {
        assert_eq!((command["a"].as_str(), command["d"].as_str()), ("d", "I"));
        assert_eq!(
            command["i"].parse::<u32>().unwrap(),
            0x520000 + index as u32
        );
    }
    assert!(commands[512..].iter().any(|command| command["a"] == "t"));
    let pet_cells = routed.placeholders_at(false);
    let meter_cells = routed.placeholders_at(true);
    assert!(
        !meter_cells.is_empty(),
        "the routed frame draws a pixel meter"
    );
    assert_eq!(
        !pet_cells.is_empty(),
        pet_drawn,
        "the routed frame draws the pixel pet"
    );
    assert_eq!(first_free_base(&pool), Some(0x520200), "slot 0 stays held");

    let hidden = paint_hidden_slot_frame(&mut painter, &snapshot, &mut ui, now);
    assert!(
        !hidden.graphics(),
        "a hidden frame writes no kitty graphics"
    );
    assert!(hidden.placeholders().next().is_none());
    assert!(!hidden.bytes.contains('\u{10eeee}'));
    assert!(!hidden.sextants(), "the pet box is blank, not cell art");
    assert!(ui.meter_pixels.is_none());
    if pet_drawn {
        assert_eq!(
            ui.pet.as_ref().and_then(|pet| pet.body.clone()),
            Some(crate::sidebar_pane::pets::PetBody::Blank(
                crate::sidebar_pane::pets::PetGridSize { cols: 15, rows: 9 }
            ))
        );
    }
    for at in &pet_cells {
        assert_eq!(
            hidden.symbol(*at),
            " ",
            "the pet's footprint stays, blank, at {at:?}"
        );
    }
    for at in &meter_cells {
        assert_ne!(
            hidden.symbol(*at),
            " ",
            "the cell meter replaces the pixel meter at {at:?}"
        );
    }
    let held = crate::sidebar_pane::pixel::PixelLease::acquire(&pool, &PERMIT)
        .unwrap()
        .expect("the hidden frame returned slot 0");
    assert_eq!(held.slot.base(), 0x520000);

    let arrival = paint_slot_frame(&mut painter, &snapshot, &mut ui, ROUTED, now);
    let commands = kitty_commands(arrival.bytes.as_bytes());
    assert_eq!(
        commands
            .iter()
            .filter(|command| command["a"] == "d")
            .count(),
        512,
        "each take sweeps its slot once"
    );
    assert_eq!(commands[0]["i"].parse::<u32>().unwrap(), 0x520200);
    assert!(commands.iter().any(|command| command["a"] == "t"));
    let ids = arrival.image_ids();
    assert!(!ids.is_empty());
    for id in ids {
        assert!((0x520200..0x520400).contains(&id), "{id:x} outside slot 1");
    }
    assert_eq!(arrival.placeholders_at(false).is_empty(), !pet_drawn);
}

#[test]
fn a_denied_pane_shows_the_blank_box_and_cell_meter_and_retries() {
    use crate::sidebar_pane::pixel::PixelLease;
    let (_root, pool) = slot_pool();
    let mut leases = (0..256)
        .map(|_| {
            PixelLease::acquire(&pool, &PERMIT)
                .unwrap()
                .expect("free slot")
        })
        .collect::<Vec<_>>();
    let bases = leases
        .iter()
        .map(|lease| lease.slot.base())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(bases.len(), 256);
    assert_eq!(leases[0].slot.base(), 0x520000);
    assert!(PixelLease::acquire(&pool, &PERMIT).unwrap().is_none());
    let snapshot = slot_snapshot();
    let mut painter = slot_painter(&pool, FULL_CAPS);
    let mut ui = UiState::default();
    let start = Instant::now();

    let denied = paint_slot_frame(&mut painter, &snapshot, &mut ui, ROUTED, start);
    assert!(!denied.graphics(), "a denied pane writes no kitty graphics");
    assert!(denied.placeholders().next().is_none());
    assert!(!denied.sextants(), "a denied pane draws the blank pet box");
    assert!(ui.meter_pixels.is_none(), "a denied pane draws cell meters");
    assert!(
        denied.bytes.contains("5h") && denied.bytes.contains('▰'),
        "the budget bar is drawn as cells"
    );

    drop(leases.remove(0));
    let soon = paint_slot_frame(
        &mut painter,
        &snapshot,
        &mut ui,
        ROUTED,
        start + Duration::from_millis(500),
    );
    assert!(!soon.graphics(), "an on-screen retry waits out its spacing");
    let retried = paint_slot_frame(
        &mut painter,
        &snapshot,
        &mut ui,
        ROUTED,
        start + Duration::from_secs(2),
    );
    assert!(
        retried.bytes.contains("a=t"),
        "the retry after its spacing paints pixels"
    );

    paint_hidden_slot_frame(&mut painter, &snapshot, &mut ui, start);
    leases.push(
        PixelLease::acquire(&pool, &PERMIT)
            .unwrap()
            .expect("slot 0 returned"),
    );
    let later = start + Duration::from_secs(10);
    let denied = paint_slot_frame(&mut painter, &snapshot, &mut ui, ROUTED, later);
    assert!(!denied.graphics());
    drop(leases.pop());
    paint_hidden_slot_frame(&mut painter, &snapshot, &mut ui, later);
    let arrival = paint_slot_frame(&mut painter, &snapshot, &mut ui, ROUTED, later);
    assert!(
        arrival.bytes.contains("a=t"),
        "an arrival after a hidden frame retries at once"
    );
    painter.clear(&mut Vec::new(), Some(&PERMIT)).unwrap();
    assert_eq!(
        first_free_base(&pool),
        Some(0x520000),
        "slot 0 is reusable after its lease drops"
    );

    let mut pets_off = slot_snapshot();
    pets_off.theme.pets.enabled = false;
    leases.push(PixelLease::acquire(&pool, &PERMIT).unwrap().unwrap());
    let mut denied_painter = slot_painter(&pool, FULL_CAPS);
    let denied = paint_slot_frame(
        &mut denied_painter,
        &pets_off,
        &mut UiState::default(),
        ROUTED,
        later,
    );
    let mut off = pets_off.clone();
    off.theme.display.pixel = crate::config::PixelMode::Off;
    let mut off_painter = slot_painter(&pool, FULL_CAPS);
    let cells = paint_slot_frame(
        &mut off_painter,
        &off,
        &mut UiState::default(),
        ROUTED,
        later,
    );
    assert_eq!(
        denied.bytes, cells.bytes,
        "a denied pane without a pet draws the pixel-off frame"
    );
}

#[test]
fn a_denied_pane_warns_once_across_hidden_frames() {
    use crate::sidebar_pane::pixel::PixelLease;
    let (_root, pool) = slot_pool();
    let _held = (0..256)
        .map(|_| PixelLease::acquire(&pool, &PERMIT).unwrap().unwrap())
        .collect::<Vec<_>>();
    let snapshot = slot_snapshot();
    let mut painter = slot_painter(&pool, FULL_CAPS);
    let mut ui = UiState::default();
    let now = Instant::now();
    let logs = tempfile::NamedTempFile::new().unwrap();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(std::sync::Arc::new(logs.reopen().unwrap()))
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        for _ in 0..2 {
            let kind = ROUTED;
            let denied = paint_slot_frame(&mut painter, &snapshot, &mut ui, kind, now);
            assert!(!denied.graphics(), "every arrival is denied");
            paint_hidden_slot_frame(&mut painter, &snapshot, &mut ui, now);
        }
    });
    let logs = std::fs::read_to_string(logs.path()).unwrap();
    assert_eq!(
        logs.matches("sidebar pixel slots are all held").count(),
        1,
        "a denial episode warns once, across hidden frames: {logs}"
    );
}

#[test]
fn pixels_turned_off_return_the_slot_after_a_written_frame_of_deletes() {
    let (_root, pool) = slot_pool();
    let mut snapshot = slot_snapshot();
    let mut painter = slot_painter(&pool, FULL_CAPS);
    let mut ui = UiState::default();
    let now = Instant::now();
    let routed = ROUTED;
    let pixels = paint_slot_frame(&mut painter, &snapshot, &mut ui, routed, now);
    assert!(pixels.bytes.contains("a=t"));
    snapshot.theme.display.pixel = crate::config::PixelMode::Off;
    let view = render::ViewState::default();
    for _ in 0..2 {
        painter.refresh_view(&mut ui, &view, &snapshot, false, routed, now);
    }
    assert!(
        painter.holds_slot(),
        "routed passes that write no frame keep the slot"
    );
    let deletes = paint_slot_frame(&mut painter, &snapshot, &mut ui, routed, now);
    assert!(deletes.bytes.contains("a=d"), "the frame deletes the ids");
    assert!(painter.holds_slot());
    assert_eq!(first_free_base(&pool), Some(0x520200));
    painter.refresh_view(&mut ui, &view, &snapshot, false, routed, now);
    assert!(!painter.holds_slot(), "the next routed frame returns it");
    assert_eq!(first_free_base(&pool), Some(0x520000));
}

#[test]
fn more_sidebars_than_slots_take_turns_on_screen() {
    use crate::sidebar_pane::pixel::PixelLease;
    let (_root, pool) = slot_pool();
    let _held = (0..255)
        .map(|_| PixelLease::acquire(&pool, &PERMIT).unwrap().unwrap())
        .collect::<Vec<_>>();
    let snapshot = slot_snapshot();
    let mut painters = (0..3)
        .map(|_| (slot_painter(&pool, FULL_CAPS), UiState::default()))
        .collect::<Vec<_>>();
    let now = Instant::now();
    for turn in 0..6 {
        let (painter, ui) = &mut painters[turn % 3];
        let frame = paint_slot_frame(painter, &snapshot, ui, ROUTED, now);
        assert!(frame.bytes.contains("a=t"), "turn {turn} paints pixels");
        assert!(
            frame
                .image_ids()
                .iter()
                .all(|id| (0x53fe00..0x540000).contains(id))
        );
        paint_hidden_slot_frame(painter, &snapshot, ui, now);
    }
}

#[test]
fn panes_without_a_pixel_surface_open_no_slot_lock() {
    let snapshot = slot_snapshot();
    let mut off = snapshot.clone();
    off.theme.display.pixel = crate::config::PixelMode::Off;
    let transportless = PixelRenderCaps {
        pixel_transport: false,
        kitty_clients: true,
    };
    for (snapshot, caps) in [
        (&snapshot, PixelRenderCaps::default()),
        (&off, FULL_CAPS),
        (&snapshot, transportless),
    ] {
        let (_root, pool) = slot_pool();
        let mut painter = slot_painter(&pool, caps);
        let mut ui = UiState::default();
        let grant = mirror_queue::Grant::for_test(PaneId::from_parts(crate::MuxName::Tmux, "%1"));
        for kind in [ROUTED, paint::PaintKind::Mirror(&grant), ROUTED] {
            let frame = paint_slot_frame(&mut painter, snapshot, &mut ui, kind, Instant::now());
            assert!(!frame.graphics());
        }
        assert!(
            !pool.shared_pixel_slot_lock(0).exists(),
            "no slot lock file is opened for {caps:?}"
        );
    }

    let mut sextant = slot_snapshot();
    sextant.theme.pets.glyphs = crate::config::PetsGlyphMode::Sextant;
    let (_root, pool) = slot_pool();
    let mut painter = paint::FramePainter::new(FULL_CAPS, false, pool.clone());
    let mut ui = UiState::default();
    let now = Instant::now();
    let ready = painter.prepare_play_pet(&render::ViewState::default(), &mut ui, &sextant, now);
    let cell_art = ready && !crate::tui::no_color();
    let routed = paint_slot_frame(&mut painter, &sextant, &mut ui, ROUTED, now);
    assert_eq!(
        routed.sextants(),
        cell_art,
        "sextant glyphs draw cell art when routed"
    );
    assert!(
        !routed.placeholders_at(true).is_empty(),
        "the meter still takes a slot"
    );
    assert!(routed.placeholders_at(false).is_empty());
    assert_eq!(first_free_base(&pool), Some(0x520200));
    let hidden = paint_hidden_slot_frame(&mut painter, &sextant, &mut ui, now);
    assert_eq!(
        hidden.sextants(),
        cell_art,
        "sextant glyphs draw cell art when hidden"
    );
    assert!(hidden.placeholders().next().is_none());
    assert_eq!(first_free_base(&pool), Some(0x520000));
}

#[test]
fn pixel_disable_and_clear_retire_resident_meter_ids() {
    use crate::sidebar_pane::pixel::PixelSlot;
    let mut painter = paint::FramePainter::with_slot(
        Some(PixelSlot::new(0)),
        PixelRenderCaps {
            pixel_transport: true,
            kitty_clients: true,
        },
    );
    let mut snapshot = snapshot(&workspace());
    let view_ui = render::ViewState::default();
    let mut ui = UiState::default();
    for disable in [true, false] {
        snapshot.theme.display.pixel = crate::config::PixelMode::Auto;
        painter.refresh_view(&mut ui, &view_ui, &snapshot, false, ROUTED, Instant::now());
        compose_test_meters(&mut ui, 1);
        let mut bytes = Vec::new();
        painter
            .ensure_meters_transmitted(&mut bytes, &ui, 0, &PERMIT)
            .unwrap();
        let ids = kitty_commands(&bytes)
            .into_iter()
            .filter(|command| command["a"] == "t")
            .map(|command| command["i"].clone())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), 4, "re-enable transmits afresh");
        bytes.clear();
        if disable {
            snapshot.theme.display.pixel = crate::config::PixelMode::Off;
            painter.refresh_view(&mut ui, &view_ui, &snapshot, false, ROUTED, Instant::now());
            painter
                .ensure_meters_transmitted(&mut bytes, &ui, 1, &PERMIT)
                .unwrap();
        } else {
            painter.clear(&mut bytes, Some(&PERMIT)).unwrap();
            assert!(bytes.starts_with(BEGIN_SYNC) && bytes.ends_with(END_SYNC));
        }
        let commands = kitty_commands(&bytes);
        assert!(
            commands
                .iter()
                .all(|command| command["a"] == "d" && command["d"] == "I")
        );
        assert_eq!(
            commands
                .into_iter()
                .map(|command| command["i"].clone())
                .collect::<std::collections::BTreeSet<_>>(),
            ids
        );
        bytes.clear();
        painter.clear(&mut bytes, Some(&PERMIT)).unwrap();
        assert!(
            bytes.is_empty(),
            "clearing an empty residency writes nothing"
        );
    }
}

#[test]
fn pixel_pet_disable_retires_sprite_on_next_paint() {
    let mut painter = paint::FramePainter::with_assets(
        PetAssets::test_loaded_pixel_frame("codex"),
        PixelRenderCaps {
            pixel_transport: true,
            kitty_clients: true,
        },
        false,
    );
    let mut snapshot = snapshot(&workspace());
    snapshot.theme.pets.enabled = true;
    let view_ui = render::ViewState::default();
    let mut ui = UiState {
        pet: Some(crate::sidebar_pane::pets::PetView {
            body: Some(crate::sidebar_pane::pets::PetBody::Pixel(PetPixelView {
                pet_id: "codex".to_owned(),
                sprite_index: 0,
                image_id: 0x520000,
                size: crate::sidebar_pane::pets::PetGridSize { cols: 2, rows: 1 },
            })),
            caption: None,
            frame_interval: None,
        }),
        ..Default::default()
    };
    let mut bytes = Vec::new();
    painter
        .ensure_pixel_transmitted(&mut bytes, &ui, 0, &PERMIT)
        .unwrap();
    let ids = kitty_commands(&bytes)
        .into_iter()
        .filter(|command| command["a"] == "t")
        .map(|command| command["i"].clone())
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 1);
    snapshot.providers = vec![crate::sidebar::test_support::provider_panel(
        "codex",
        Vec::new(),
    )];
    let saved_pet = ui.pet.clone();
    painter.refresh_view(&mut ui, &view_ui, &snapshot, true, ROUTED, Instant::now());
    bytes.clear();
    painter
        .ensure_pixel_transmitted(&mut bytes, &ui, 1, &PERMIT)
        .unwrap();
    assert!(
        bytes.is_empty(),
        "an alert hiding the pet keeps its sprites"
    );
    ui.pet = saved_pet;
    snapshot.theme.pets.enabled = false;
    painter.refresh_view(&mut ui, &view_ui, &snapshot, false, ROUTED, Instant::now());
    bytes.clear();
    painter
        .ensure_pixel_transmitted(&mut bytes, &ui, 1, &PERMIT)
        .unwrap();
    let commands = kitty_commands(&bytes);
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0]["d"], "I");
    assert_eq!(commands[0]["i"], ids[0]);
}

#[test]
fn tick_for_clamps_zero_and_honours_explicit_seconds() {
    assert_eq!(tick_for(5), Duration::from_secs(5));
    assert_eq!(tick_for(0), Duration::from_secs(1));
}

#[test]
fn frame_grid_advances_one_frame_or_snaps_past_missed_frames() {
    let base = Instant::now();
    let frame = crate::sidebar::timing::animation_frame(
        crate::config::DisplayConfig::default().resolved_refresh_ms(),
    );
    assert_eq!(next_frame_after(base, base, frame), base + frame);
    let now = base + frame * 5;
    assert_eq!(next_frame_after(base, now, frame), now + frame);
}

#[test]
fn configurable_width_bindings_shadow_fixed_actions() {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    let keymap = crate::config::keys::sidebar::SidebarKeymap::rebound(&[
        ("q", "width.narrower"),
        ("Ctrl+b", "width.narrower"),
        ("x", "width.wider"),
        ("/", "width.wider"),
    ]);
    for (code, mods, action) in [
        (
            KeyCode::Char('q'),
            KeyModifiers::NONE,
            KeyAction::WidthNarrower,
        ),
        (
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
            KeyAction::WidthNarrower,
        ),
        (
            KeyCode::Char('x'),
            KeyModifiers::NONE,
            KeyAction::WidthWider,
        ),
        (KeyCode::Esc, KeyModifiers::NONE, KeyAction::Cancel),
    ] {
        assert_eq!(resolve_one(&keymap, InputMode::Normal, code, mods), action);
    }
    for (code, mods) in [
        (KeyCode::Esc, KeyModifiers::CONTROL),
        (KeyCode::Esc, KeyModifiers::ALT),
        (KeyCode::Char('y'), KeyModifiers::NONE),
        (KeyCode::Char('s'), KeyModifiers::CONTROL),
    ] {
        assert_eq!(
            resolve_one(&keymap, InputMode::Normal, code, mods),
            KeyAction::Other
        );
    }
    assert_eq!(
        resolve_one(
            &keymap,
            InputMode::Help,
            KeyCode::Char('/'),
            KeyModifiers::NONE
        ),
        KeyAction::Other,
        "help consumes even a rebound slash before the keymap",
    );
}

#[test]
fn help_popup_dismisses_and_consumes_any_user_input() {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    let ws = workspace();
    let snapshot = snapshot_with_panes(
        &ws,
        vec![pane("terminal_1", "tab_0"), pane("terminal_2", "tab_0")],
    );
    let keymap = crate::config::keys::sidebar::SidebarKeymap::rebound(&[("/", "width.wider")]);

    for wakeup in [
        Wakeup::Press {
            code: KeyCode::Esc,
            mods: KeyModifiers::NONE,
        },
        Wakeup::Press {
            code: KeyCode::Char('/'),
            mods: KeyModifiers::NONE,
        },
        Wakeup::Press {
            code: KeyCode::Char('R'),
            mods: KeyModifiers::NONE,
        },
        Wakeup::Press {
            code: KeyCode::Char('n'),
            mods: KeyModifiers::CONTROL,
        },
        Wakeup::Key(KeyAction::Down),
        Wakeup::Key(KeyAction::Other),
        Wakeup::MouseClick { column: 1, row: 0 },
        Wakeup::Scroll { down: true },
    ] {
        let mut view = render::ViewState {
            selected_index: Some(0),
            scroll_offset: 4,
            toggles: PresentationToggles {
                help_visible: true,
                make_up_filter: crate::sidebar_pane::view::BodyLens {
                    query: Some("kept".to_owned()),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let mut ui = UiState {
            interactions: render::FrameInteractions::from_parts(vec![Some(1)], Vec::new()),
            ..Default::default()
        };

        let outcome = handle_wakeup(wakeup, &mut view, &mut ui, &snapshot, &keymap, false);

        assert_eq!(outcome, InputOutcome::redraw());
        assert!(!view.toggles.help_visible);
        assert_eq!(view.toggles.make_up_filter.query.as_deref(), Some("kept"));
        assert_eq!(view.selected_index, Some(0), "key input was consumed");
        assert_eq!(view.scroll_offset, 4, "scroll input was consumed");
        assert!(!view.authoritative_offset(), "scroll input was consumed");
        assert_eq!(view.browse, None, "key input was consumed");
    }

    let outcome = handle_wakeup(
        Wakeup::Press {
            code: KeyCode::Char('R'),
            mods: KeyModifiers::NONE,
        },
        &mut render::ViewState::default(),
        &mut UiState::default(),
        &snapshot,
        &keymap,
        false,
    );
    assert_eq!(outcome.effects, vec![selection::InputEffect::Reload]);
}

#[test]
fn refresh_pet_view_uses_fixed_pet_size_when_dashboard_present() {
    let ws = workspace();
    let mut snapshot = snapshot(&ws);
    snapshot.theme.pets.enabled = true;
    let view_ui = render::ViewState::default();
    let mut ui = UiState::default();
    let mut painter = paint::FramePainter::test_held(
        PetAssets::default(),
        PixelRenderCaps::default(),
        true,
        Some(crate::sidebar_pane::pixel::PixelSlot::new(0)),
    );

    painter.refresh_view(&mut ui, &view_ui, &snapshot, false, ROUTED, Instant::now());

    let pet = ui.pet.expect("pet view");
    assert_eq!(pet.body, None);
    let body_enabled = !crate::tui::no_color();
    if body_enabled {
        assert!(pet.frame_interval.is_some());
    } else {
        assert!(
            pet.frame_interval.is_none(),
            "NO_COLOR suppresses pet body loading"
        );
    }
    assert_eq!(pet.caption.as_deref(), Some("resting"));
}

#[test]
fn refresh_view_gates_pixel_meter_frame_with_caps_and_master_switch() {
    let ws = workspace();
    let mut snapshot = snapshot(&ws);
    let view_ui = render::ViewState::default();
    let mut ui = UiState::default();
    let mut painter = paint::FramePainter::test_held(
        PetAssets::default(),
        PixelRenderCaps {
            pixel_transport: true,
            kitty_clients: true,
        },
        false,
        Some(crate::sidebar_pane::pixel::PixelSlot::new(0)),
    );

    painter.refresh_view(&mut ui, &view_ui, &snapshot, false, ROUTED, Instant::now());
    let raster = crate::sidebar_pane::pixel::meter::MeterRaster::new(
        2,
        0.5,
        [1, 2, 3],
        Vec::new(),
        [4, 5, 6],
        None,
    );
    let first_id = ui
        .meter_pixels
        .as_mut()
        .expect("meter pixels")
        .intern(raster.clone())
        .expect("first raster");

    painter.refresh_view(&mut ui, &view_ui, &snapshot, false, ROUTED, Instant::now());
    assert_eq!(
        ui.meter_pixels
            .as_mut()
            .expect("persistent meter pixels")
            .intern(raster),
        Some(first_id),
        "refreshing the view keeps the content interning table"
    );

    snapshot.theme.display.pixel = crate::config::PixelMode::Off;
    painter.refresh_view(&mut ui, &view_ui, &snapshot, false, ROUTED, Instant::now());
    assert!(ui.meter_pixels.is_none());
}

#[test]
fn meter_transmission_paints_visible_rasters_once() {
    let ws = workspace();
    let snapshot = snapshot(&ws);
    let view_ui = render::ViewState::default();
    let mut ui = UiState::default();
    let mut painter = paint::FramePainter::test_held(
        PetAssets::default(),
        PixelRenderCaps {
            pixel_transport: true,
            kitty_clients: true,
        },
        false,
        Some(crate::sidebar_pane::pixel::PixelSlot::new(0)),
    );

    painter.refresh_view(&mut ui, &view_ui, &snapshot, false, ROUTED, Instant::now());
    let image_id = ui
        .meter_pixels
        .as_mut()
        .expect("meter pixels")
        .intern(crate::sidebar_pane::pixel::meter::MeterRaster::new(
            2,
            0.5,
            [1, 2, 3],
            Vec::new(),
            [4, 5, 6],
            None,
        ))
        .expect("meter raster");
    let placeholder = ratatui::text::Line::from(ratatui::text::Span::styled(
        placeholder_cluster(0, 0),
        ratatui::style::Style::default().fg(crate::sidebar_pane::pixel::image_id_color(image_id)),
    ));
    ui.meter_pixels
        .as_mut()
        .expect("meter pixels")
        .observe_visible(&[placeholder]);

    let mut first = Vec::new();
    painter
        .ensure_meters_transmitted(&mut first, &ui, 0, &PERMIT)
        .expect("transmit visible meter");
    let text = String::from_utf8_lossy(&first);
    assert!(text.contains("a=t"));
    assert!(text.contains(&format!("i={image_id}")));

    let mut repeat = Vec::new();
    painter
        .ensure_meters_transmitted(&mut repeat, &ui, 1, &PERMIT)
        .expect("deduplicate resident meter");
    assert!(repeat.is_empty());

    ui.meter_pixels = None;
    let mut disabled = Vec::new();
    painter
        .ensure_meters_transmitted(&mut disabled, &ui, 2, &PERMIT)
        .expect("skip disabled meters");
    let commands = kitty_commands(&disabled);
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0]["d"], "I");
    assert_eq!(commands[0]["i"], image_id.to_string());
}

#[test]
fn dashboard_budget_bars_reach_the_visible_set_and_transmit_once() {
    let (_root, pool) = slot_pool();
    let snapshot = slot_snapshot();
    let mut painter = slot_painter(&pool, FULL_CAPS);
    let mut ui = UiState::default();
    let now = Instant::now();
    let routed = paint_slot_frame(&mut painter, &snapshot, &mut ui, ROUTED, now);
    if crate::tui::no_color() {
        return;
    }
    let budget = ui
        .meter_pixels
        .as_ref()
        .expect("meter pixels")
        .visible_images()
        .filter(|(_, image)| {
            matches!(
                image,
                crate::sidebar_pane::pixel::meter::MeterImage::Budget(_)
            )
        })
        .map(|(image_id, _)| image_id)
        .collect::<Vec<_>>();
    assert_eq!(budget.len(), 1, "the 5h bar is one visible image");
    let transmitted = |frame: &SlotFrame| {
        kitty_commands(frame.bytes.as_bytes())
            .into_iter()
            .filter(|command| {
                command.get("a").is_some_and(|action| action == "t")
                    && command.get("i") == Some(&budget[0].to_string())
            })
            .count()
    };
    assert_eq!(transmitted(&routed), 1, "the bar is transmitted once");
    assert!(routed.placeholders().any(|(_, id)| id == budget[0]));
    assert!(!routed.bytes.contains('▰'), "no text bar beside the image");

    let steady = paint_slot_frame(&mut painter, &snapshot, &mut ui, ROUTED, now);
    assert_eq!(transmitted(&steady), 0, "a resident bar is not resent");
}

#[test]
fn pixel_layout_shift_uses_ratatui_diff_without_full_clear() {
    let ws = workspace();
    let mut snapshot = snapshot(&ws);
    snapshot.providers = vec![crate::sidebar::test_support::provider_panel(
        "codex",
        Vec::new(),
    )];
    snapshot.theme.pets.enabled = true;
    snapshot.theme.pets.pet = "codex".to_owned();
    snapshot.theme.pets.glyphs = crate::config::PetsGlyphMode::Pixel;
    let pixel = PetPixelView {
        pet_id: "codex".to_owned(),
        sprite_index: 0,
        image_id: 0x520000,
        size: crate::sidebar_pane::pets::PetGridSize { cols: 2, rows: 1 },
    };
    let mut view_ui = render::ViewState::default();
    let mut ui = UiState {
        pet: Some(crate::sidebar_pane::pets::PetView {
            body: Some(crate::sidebar_pane::pets::PetBody::Pixel(pixel.clone())),
            caption: Some("resting".to_owned()),
            frame_interval: None,
        }),
        ..Default::default()
    };
    let mut painter = paint::FramePainter::with_assets(
        PetAssets::test_loaded_pixel_frame("codex"),
        PixelRenderCaps::default(),
        true,
    );
    let output = SharedBuffer::default();
    let backend = PaneBackend::headless(output.clone());
    let viewport = ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 80, 12));
    let mut terminal =
        Terminal::with_options(backend, ratatui::TerminalOptions { viewport }).expect("terminal");

    painter
        .draw_and_paint(&mut terminal, &snapshot, None, &mut view_ui, &mut ui)
        .expect("first draw");
    let first_len = output.0.lock().unwrap().len();

    snapshot.providers[0].windows = vec![
        crate::agents::RateLimitWindow {
            used_percentage: Some(25),
            duration_mins: Some(300),
            ..Default::default()
        },
        crate::agents::RateLimitWindow {
            used_percentage: Some(40),
            duration_mins: Some(10_080),
            ..Default::default()
        },
    ];
    painter
        .draw_and_paint(&mut terminal, &snapshot, None, &mut view_ui, &mut ui)
        .expect("shifted draw");
    let second_len = output.0.lock().unwrap().len();

    painter
        .draw_and_paint(&mut terminal, &snapshot, None, &mut view_ui, &mut ui)
        .expect("steady draw");
    let output = output.0.lock().unwrap().clone();
    let second = String::from_utf8_lossy(&output[first_len..second_len]);
    let steady = String::from_utf8_lossy(&output[second_len..]);

    assert!(
        !second.contains("\u{1b}[2J"),
        "layout shift must not full-clear the terminal"
    );
    if !crate::tui::no_color() {
        assert!(
            second.contains(&placeholder_cluster(0, 0)),
            "ratatui owns and rewrites shifted placeholder cells: {}",
            second.escape_debug()
        );
    }
    assert!(
        !second.contains("\u{1b}_G"),
        "resident sprite is not re-transmitted on layout shift"
    );
    assert!(
        !steady.contains(&placeholder_cluster(0, 0)),
        "unchanged frame emits no placeholder bytes"
    );
    assert!(
        !steady.contains("\u{1b}_G"),
        "unchanged frame emits no kitty graphics bytes"
    );
    assert!(
        kitty_commands(&output[first_len..second_len]).is_empty(),
        "layout shifts emit no kitty graphics commands"
    );
    assert!(!steady.contains("\x1b[?2026"));
    assert!(
        !String::from_utf8_lossy(&output[first_len..]).contains("a=d,d=I"),
        "layout shifts must keep resident kitty images alive"
    );
    assert!(
        second.starts_with("\x1b[?2026h") && second.ends_with("\x1b[?2026l"),
        "a layout shift rewrites most of its touched rows, so it is one synchronized update"
    );
    let output = String::from_utf8_lossy(&output);
    assert_eq!(
        output
            .matches(std::str::from_utf8(BEGIN_SYNC).unwrap())
            .count(),
        2,
        "the first paint's sprite and the layout shift each carry one bracket"
    );
    assert_eq!(
        output
            .matches(std::str::from_utf8(END_SYNC).unwrap())
            .count(),
        2
    );
}

#[test]
fn heartbeat_write_due_on_first_or_aged_write_only() {
    assert!(heartbeat_write_due(None));
    assert!(!heartbeat_write_due(Some(Instant::now())));
    assert!(heartbeat_write_due(Some(
        Instant::now() - HEARTBEAT_WRITE_INTERVAL
    )));
}

#[test]
fn suppressed_produce_panic_hook_chains_without_renderer_diagnostic() {
    let _hook_guard = PANIC_HOOK_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let sink =
        crate::diag::DiagSink::under(dir.path().to_path_buf(), workspace(), "rimz-test", None);
    let prior_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let prior_called_hook = prior_called.clone();
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |_| {
        prior_called_hook.store(true, std::sync::atomic::Ordering::SeqCst);
    }));
    install_panic_diagnostic_hook(sink.clone());

    let result = with_produce_panic_diagnostic_suppressed(|| {
        std::panic::catch_unwind(|| panic!("caught produce panic"))
    });
    let _installed = std::panic::take_hook();
    std::panic::set_hook(original);

    assert!(result.is_err());
    assert!(
        prior_called.load(std::sync::atomic::Ordering::SeqCst),
        "the suppressed diagnostic branch still chains the previously installed panic hook"
    );
    assert!(
        !sink.log_path().unwrap().exists(),
        "caught producer panics are converted to fetch failures, not renderer-panic diagnostics"
    );
}

#[test]
fn notification_targeting_matches_mux_reachability_rules() {
    let (targeted_snapshot, _sidebar, first_work, _second_work) = focus_fixture();
    assert!(notification_targets_own_view(
        &targeted_snapshot,
        std::slice::from_ref(&first_work)
    ));
    assert!(!notification_targets_own_view(&targeted_snapshot, &[]));

    let foreign = PaneId::from_parts(MuxName::Zellij, "terminal_99");
    assert!(!notification_targets_own_view(
        &targeted_snapshot,
        &[foreign]
    ));

    let no_own_view = snapshot(&workspace());
    assert!(!notification_targets_own_view(
        &no_own_view,
        std::slice::from_ref(&first_work)
    ));

    assert!(desktop_notification_targets_renderer(
        MuxName::Tmux,
        &targeted_snapshot,
        &[]
    ));

    let foreign = PaneId::from_parts(MuxName::Tmux, "%99");
    assert!(desktop_notification_targets_renderer(
        MuxName::Tmux,
        &targeted_snapshot,
        &[foreign]
    ));

    let no_own_view = snapshot(&workspace());
    assert!(!desktop_notification_targets_renderer(
        MuxName::Tmux,
        &no_own_view,
        std::slice::from_ref(&first_work)
    ));

    assert!(desktop_notification_targets_renderer(
        MuxName::Zellij,
        &targeted_snapshot,
        std::slice::from_ref(&first_work)
    ));

    let foreign = PaneId::from_parts(MuxName::Zellij, "terminal_99");
    assert!(!desktop_notification_targets_renderer(
        MuxName::Zellij,
        &targeted_snapshot,
        &[foreign]
    ));
    assert!(!desktop_notification_targets_renderer(
        MuxName::Zellij,
        &targeted_snapshot,
        &[]
    ));
}

#[test]
fn disabled_terminal_notifications_write_nothing_and_trace_suppression() {
    use super::notify::{BellNotice, emit_terminal_notification};

    let ws = workspace();
    let dir = tempfile::tempdir().unwrap();
    let diag =
        crate::diag::DiagSink::under(dir.path().to_path_buf(), ws.clone(), "rimz-test", None);
    let mut config = fixtures::serve_config(&ws);
    config.room.notification_prefs.enabled = false;
    config.room.notification_prefs.desktop = crate::config::DesktopNotificationMode::Osc;
    config.room.notification_prefs.sound = crate::config::NotificationSoundMode::Bell;
    let mut snap = snapshot(&ws);
    let work = PaneId::from_parts(MuxName::Zellij, "terminal_11");
    snap.own_view = Some(crate::store::snapshot::SidebarOwnView {
        sibling_count: 1,
        working_pane_ids: vec![work.clone()],
        own_view_is_daemon: false,
    });
    let mut output = Vec::new();
    let backend = CrosstermBackend::new(&mut output);
    let viewport = ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 80, 12));
    let mut terminal =
        Terminal::with_options(backend, ratatui::TerminalOptions { viewport }).unwrap();
    assert!(
        !emit_terminal_notification(
            &config,
            &mut terminal,
            &snap,
            BellNotice {
                title: "Test title",
                body: "Test body",
                panes: std::slice::from_ref(&work),
                recheck_unread: false,
                kind: "waiting",
            },
            &diag,
        )
        .unwrap()
    );
    drop(terminal);
    assert!(output.is_empty());
    let trace = std::fs::read_to_string(crate::StatePaths::class_path(
        dir.path(),
        crate::disk::paths::Class::Audit,
        "notify.log.jsonl",
    ))
    .unwrap();
    let record: serde_json::Value = serde_json::from_str(&trace).unwrap();
    assert_eq!(record["event"]["fired"], false);
    assert_eq!(record["event"]["suppressed"], "notifications_disabled");
}

#[test]
fn bell_rings_only_for_unread_owned_panes_off_daemon_views() {
    use crate::agents::AgentStatus;

    let ws = workspace();
    let work = PaneId::from_parts(MuxName::Zellij, "terminal_11");
    let foreign = PaneId::from_parts(MuxName::Zellij, "terminal_99");

    let scene = |unread: bool, status: AgentStatus, daemon: bool| {
        let mut snap = snapshot(&ws);
        snap.panes_produced_at_ms = Some(1);
        snap.own_view = Some(crate::store::snapshot::SidebarOwnView {
            sibling_count: 2,
            working_pane_ids: vec![work.clone()],
            own_view_is_daemon: daemon,
        });
        snap.worktree_groups = vec![crate::store::snapshot::SidebarWorktreeGroup {
            stalled_since: None,
            pr_stack: Default::default(),
            key: "/repo/main".to_owned(),
            label: "main".to_owned(),
            label_qualifier: None,
            branch: None,
            kind: crate::store::snapshot::SidebarWorktreeKind::Worktree,
            team: None,
            cohort_effort: None,
            pipeline: None,
            workflow: None,
            status_counts: Vec::new(),
            rows: vec![crate::store::snapshot::SidebarRow {
                checkout: None,
                id: "agent-1".to_owned(),
                name: "claude".to_owned(),
                pane: Some(pane("terminal_11", "tab_1")),
                worktree_path: Some("/repo/main".to_owned()),
                worktree_branch: Some("main".to_owned()),
                channel: None,
                lane: None,
                unread,
                inactive: false,
                archived: false,
                attention_score: 0,
                attention_at_ms: None,
                last_activity: Timestamp::now(),
                card: crate::store::snapshot::RowCard::Agent(std::sync::Arc::new(
                    crate::store::snapshot::AgentCard {
                        status,
                        phase: crate::agents::TurnPhase::Idle,
                        ..crate::store::snapshot::AgentCard::default()
                    },
                )),
            }]
            .into(),
            diff_added: None,
            diff_removed: None,
            commits_ahead: None,
            commits_behind: None,
            trunk: None,
            place: None,
            root_lane: false,
            inactive: None,
            clean: None,
            landed: None,
            trunk_sync: None,
            pr_state: None,
            pr_queue: None,
            pr_conflicting: false,
            ci: None,
            pr_number: None,
            pr_url: None,
        }]
        .into();
        snap
    };

    // Agent path: rings only while the owned row is unread.
    let unread_waiting = scene(true, AgentStatus::Waiting, false);
    assert_eq!(
        bell_decision(&unread_waiting, std::slice::from_ref(&work), true),
        BellDecision::Fired
    );

    // Resumed to running and no longer unread — a thinking agent never rings.
    let running = scene(false, AgentStatus::Running, false);
    assert_eq!(
        bell_decision(&running, std::slice::from_ref(&work), true),
        BellDecision::NotUnread
    );

    // A foreign pane the view does not own never rings.
    assert_eq!(
        bell_decision(&unread_waiting, std::slice::from_ref(&foreign), true),
        BellDecision::PaneNotInView
    );

    // Link/reminder path bypasses the unread re-check and rings on an owned pane.
    assert!(bell_decision(&running, std::slice::from_ref(&work), false).fired());

    // A daemon-only view (rimzd) never rings, on either path.
    let daemon = scene(true, AgentStatus::Waiting, true);
    assert_eq!(
        bell_decision(&daemon, std::slice::from_ref(&work), true),
        BellDecision::DaemonView
    );
    assert!(!bell_decision(&daemon, std::slice::from_ref(&work), false).fired());

    // No own view at all never rings.
    assert_eq!(
        bell_decision(&snapshot(&ws), std::slice::from_ref(&work), false),
        BellDecision::NoOwnView
    );
}

#[test]
fn a_stopped_forwarder_gives_up_on_a_full_inbox() {
    use std::io::{ErrorKind, Write};
    use std::os::unix::net::UnixStream;
    let (mut writer, _inbox) = UnixStream::pair().unwrap();
    writer.set_nonblocking(true).unwrap();
    loop {
        match writer.write(&[b'x'; 4096]) {
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::WouldBlock => break,
            Err(err) => panic!("filling stream: {err}"),
        }
    }
    writer.set_nonblocking(false).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(
            super::socket::send_input(&mut writer, "press:-:char:j").map_err(|err| err.kind()),
        );
    });
    assert!(
        matches!(
            rx.recv_timeout(Duration::from_secs(5)),
            Ok(Err(ErrorKind::WouldBlock | ErrorKind::TimedOut))
        ),
        "a full stream drops the word within the forwarder's stop bound"
    );
}

/// A stream end that accepts `budgets` bytes per write in turn, `None` timing out, then everything.
struct ScriptedStream {
    delivered: Vec<u8>,
    budgets: std::collections::VecDeque<Option<usize>>,
}

impl std::io::Write for ScriptedStream {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let accepted = match self.budgets.pop_front() {
            Some(None) => return Err(std::io::ErrorKind::TimedOut.into()),
            Some(Some(budget)) => budget.min(bytes.len()),
            None => bytes.len(),
        };
        self.delivered.extend_from_slice(&bytes[..accepted]);
        Ok(accepted)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Forward `words` into a stream scripted by `budgets`, the terminal failing after the last word.
fn forward_scripted(
    words: &[&str],
    budgets: &[Option<usize>],
) -> (Option<super::socket::ForwarderEnd>, String) {
    let mut stream = ScriptedStream {
        delivered: Vec::new(),
        budgets: budgets.iter().copied().collect(),
    };
    let mut words: std::collections::VecDeque<String> =
        words.iter().map(|word| (*word).to_owned()).collect();
    let end = super::socket::forward_events(
        &mut stream,
        |_| {
            words
                .pop_front()
                .map(Some)
                .ok_or_else(|| std::io::Error::other("terminal gone"))
        },
        Duration::from_millis(1),
        &std::sync::atomic::AtomicBool::new(false),
    );
    (end, String::from_utf8(stream.delivered).unwrap())
}

#[test]
fn a_word_torn_by_a_write_timeout_ends_the_forwarder_before_the_next_word() {
    let (end, delivered) =
        forward_scripted(&["press:-:char:j", "press:-:char:k"], &[Some(3), None]);
    // Only the unterminated fragment stays on the wire, which the host reader
    // discards at EOF (`host::tests::stream_input_discards_a_torn_word_left_at_eof`).
    assert_eq!(delivered, "pre");
    assert_eq!(end, Some(super::socket::ForwarderEnd::Stream));
}

#[test]
fn a_write_timeout_before_any_byte_drops_only_that_word() {
    let (end, delivered) = forward_scripted(&["press:-:char:j", "press:-:char:k"], &[None]);
    assert_eq!(delivered, "press:-:char:k\n");
    assert_eq!(end, Some(super::socket::ForwarderEnd::Input));
}
