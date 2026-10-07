use super::input::{InputMode, KeyAction, Wakeup, resolve_key};
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
use crate::sidebar_pane::pixel::{BEGIN_SYNC, END_SYNC, PixelRenderCaps, placeholder_cluster};
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

#[test]
fn worker_close_preserves_modes_only_after_a_signal() {
    const CHILD: &str = "RIMZ_TEST_WORKER_CLOSE_CHILD";
    let Ok(signal) = std::env::var(CHILD) else {
        for signal in ["false", "true"] {
            let pty = nix::pty::openpty(None, None).unwrap();
            let tty = std::fs::File::from(pty.slave);
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sidebar_pane::app::tests::worker_close_preserves_modes_only_after_a_signal",
                    "--nocapture",
                ])
                .env(CHILD, signal)
                .stdin(tty.try_clone().unwrap())
                .stdout(tty.try_clone().unwrap())
                .stderr(tty)
                .spawn()
                .unwrap();
            let capture = std::thread::spawn(move || {
                let mut master = std::fs::File::from(pty.master);
                let mut bytes = Vec::new();
                let _ = std::io::Read::read_to_end(&mut master, &mut bytes);
                bytes
            });
            let status = child.wait().unwrap();
            let bytes = capture.join().unwrap();
            assert!(status.success(), "{}", String::from_utf8_lossy(&bytes));
            assert_eq!(
                bytes
                    .windows(b"\x1b[?1006l\x1b[?1000l".len())
                    .any(|part| part == b"\x1b[?1006l\x1b[?1000l"),
                signal == "false",
                "only a genuine non-signal close restores mouse modes"
            );
        }
        return;
    };
    let original = nix::sys::termios::tcgetattr(io::stdout()).unwrap();
    let guard = TerminalModeGuard::enable(MouseCapture::Stdout, Screen::Main).unwrap();
    let raw = nix::sys::termios::tcgetattr(io::stdout()).unwrap();
    assert_ne!(raw, original);
    let signal = signal == "true";
    assert_eq!(
        finish_worker(AttachmentExit::Closed, guard, signal).unwrap(),
        ServeOutcome::Stopped
    );
    assert_eq!(
        nix::sys::termios::tcgetattr(io::stdout()).unwrap(),
        if signal { raw } else { original },
        "signal close preserves raw mode; non-signal close restores it"
    );
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
                .collect()
        })
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
        let mut ui = UiState::default();
        for frame in 0..800 {
            painter.refresh_view(&mut ui, &snapshot, false);
            compose_test_meters(&mut ui, restart * 800 + frame);
            let mut bytes = Vec::new();
            painter
                .ensure_meters_transmitted(&mut bytes, &ui, u64::from(frame) * 2500)
                .unwrap();
            painter
                .ensure_meters_transmitted(&mut bytes, &ui, u64::from(frame) * 2500 + 2000)
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
        let mut ui = UiState::default();
        painter.refresh_view(&mut ui, &snapshot, false);
        compose_test_meters(&mut ui, 1);
        let mut bytes = Vec::new();
        painter
            .ensure_meters_transmitted(&mut bytes, &ui, 0)
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
            .ensure_meters_transmitted(&mut repeat, &ui, 2500)
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
        let mut painter = paint::FramePainter::new(
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
        let card = snapshot.worktree_groups[0].rows[0].as_agent_mut().unwrap();
        card.usage.context_pct = Some(50);
        card.usage.context_window = Some(200_000);
        let mut ui = UiState::default();
        painter.refresh_view(&mut ui, &snapshot, false);
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
            .draw_and_paint(&mut terminal, &snapshot, None, &mut ui)
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
    let mut ui = UiState::default();
    painter.refresh_view(&mut ui, &snapshot, false);
    painter
        .draw_and_paint(&mut terminal, &snapshot, None, &mut ui)
        .unwrap();
    assert!(pty.drain().is_empty(), "an unsized frame ships no bytes");
}

#[test]
fn pixel_leases_are_exclusive_reused_and_exhaustion_disables_pixels() {
    use crate::sidebar_pane::pixel::PixelLease;
    let root = tempfile::tempdir().unwrap();
    let runtime = crate::disk::paths::RuntimePaths::under(workspace(), root.path()).unwrap();
    let mut leases = (0..256)
        .map(|_| PixelLease::acquire(&runtime).unwrap().expect("free slot"))
        .collect::<Vec<_>>();
    let bases = leases
        .iter()
        .map(|lease| lease.slot.base())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(bases.len(), 256);
    assert_eq!(leases[0].slot.base(), 0x520000);
    let exhausted = PixelLease::acquire(&runtime).unwrap();
    assert!(exhausted.is_none());
    let mut painter = paint::FramePainter::with_slot(
        exhausted.map(|lease| lease.slot),
        PixelRenderCaps {
            pixel_transport: true,
            kitty_clients: true,
        },
    );
    let mut ui = UiState::default();
    let mut snapshot = snapshot(&workspace());
    snapshot.providers = vec![crate::sidebar::test_support::provider_panel(
        "codex",
        vec![crate::agents::RateLimitWindow {
            used_percentage: Some(50),
            duration_mins: Some(300),
            resets_at: Some(Timestamp::from_second(3600).unwrap()),
            ..Default::default()
        }],
    )];
    painter.refresh_view(&mut ui, &snapshot, false);
    assert!(ui.meter_pixels.is_none(), "exhaustion selects cell meters");
    let mut bytes = Vec::new();
    painter
        .ensure_meters_transmitted(&mut bytes, &ui, 0)
        .unwrap();
    assert!(bytes.is_empty());
    let draw = |painter: &mut paint::FramePainter,
                snapshot: &crate::store::snapshot::SidebarSnapshot,
                ui: &mut UiState| {
        let output = SharedBuffer::default();
        let mut terminal = Terminal::with_options(
            PaneBackend::headless(output.clone()),
            ratatui::TerminalOptions {
                viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 44, 30)),
            },
        )
        .unwrap();
        painter
            .draw_and_paint(&mut terminal, snapshot, None, ui)
            .unwrap();
        drop(terminal);
        output.0.lock().unwrap().clone()
    };
    let exhausted_frame = draw(&mut painter, &snapshot, &mut ui);
    assert!(
        !String::from_utf8_lossy(&exhausted_frame).contains("\x1b[?2026"),
        "a disabled pixel session emits no synchronized-output markers"
    );
    snapshot.theme.display.pixel = crate::config::PixelMode::Off;
    let mut cell_painter = paint::FramePainter::with_slot(
        Some(crate::sidebar_pane::pixel::PixelSlot::new(0)),
        PixelRenderCaps {
            pixel_transport: true,
            kitty_clients: true,
        },
    );
    let mut cell_ui = UiState::default();
    cell_painter.refresh_view(&mut cell_ui, &snapshot, false);
    assert_eq!(
        exhausted_frame,
        draw(&mut cell_painter, &snapshot, &mut cell_ui)
    );
    assert!(
        String::from_utf8_lossy(&exhausted_frame).contains("5h"),
        "the budget bar is rendered: {:?}",
        String::from_utf8_lossy(&exhausted_frame)
    );
    drop(leases.remove(0));
    assert_eq!(
        PixelLease::acquire(&runtime).unwrap().unwrap().slot.base(),
        0x520000
    );
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
    let mut ui = UiState::default();
    for disable in [true, false] {
        snapshot.theme.display.pixel = crate::config::PixelMode::Auto;
        painter.refresh_view(&mut ui, &snapshot, false);
        compose_test_meters(&mut ui, 1);
        let mut bytes = Vec::new();
        painter
            .ensure_meters_transmitted(&mut bytes, &ui, 0)
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
            painter.refresh_view(&mut ui, &snapshot, false);
            painter
                .ensure_meters_transmitted(&mut bytes, &ui, 1)
                .unwrap();
        } else {
            painter.clear(&mut bytes).unwrap();
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
        painter.clear(&mut bytes).unwrap();
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
        .ensure_pixel_transmitted(&mut bytes, &ui, 0)
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
    painter.refresh_view(&mut ui, &snapshot, true);
    bytes.clear();
    painter
        .ensure_pixel_transmitted(&mut bytes, &ui, 1)
        .unwrap();
    assert!(
        bytes.is_empty(),
        "an alert hiding the pet keeps its sprites"
    );
    ui.pet = saved_pet;
    snapshot.theme.pets.enabled = false;
    painter.refresh_view(&mut ui, &snapshot, false);
    bytes.clear();
    painter
        .ensure_pixel_transmitted(&mut bytes, &ui, 1)
        .unwrap();
    let commands = kitty_commands(&bytes);
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0]["d"], "I");
    assert_eq!(commands[0]["i"], ids[0]);
}

#[test]
fn deferred_fetch_deadline_caps_event_loop_timeout() {
    let now = Instant::now();
    assert_eq!(
        fetch_deadline_timeout(
            Duration::from_secs(10),
            Some(now + Duration::from_secs(3)),
            now,
        ),
        Duration::from_secs(3),
    );
    assert_eq!(
        fetch_deadline_timeout(Duration::from_secs(10), Some(now), now),
        FRAME_MIN_TIMEOUT,
    );
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

    let keys = crate::config::SidebarKeys {
        narrower: "q ctrl+b".to_owned(),
        wider: "x /".to_owned(),
        ..Default::default()
    };
    let keymap = NavKeymap::from_config(&keys);
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
    ] {
        assert_eq!(resolve_key(&keymap, InputMode::Normal, code, mods), action);
    }
    for (code, mods) in [
        (KeyCode::Esc, KeyModifiers::NONE),
        (KeyCode::Char('y'), KeyModifiers::NONE),
        (KeyCode::Char('s'), KeyModifiers::CONTROL),
    ] {
        assert_eq!(
            resolve_key(&keymap, InputMode::Normal, code, mods),
            KeyAction::Other
        );
    }
    assert_eq!(
        resolve_key(
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
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let keymap = NavKeymap::from_config(&crate::config::SidebarKeys {
        wider: "/".to_owned(),
        ..Default::default()
    });

    for wakeup in [
        Wakeup::Press {
            code: KeyCode::Char('/'),
            mods: KeyModifiers::NONE,
        },
        Wakeup::Press {
            code: KeyCode::Char('r'),
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
        let mut ui = UiState {
            help_visible: true,
            selected_index: 0,
            scroll_offset: 4,
            interactions: render::FrameInteractions::from_parts(vec![Some(1)], Vec::new()),
            ..Default::default()
        };

        let outcome = handle_wakeup(wakeup, &mut ui, &snapshot, &keymap);

        assert_eq!(outcome, InputOutcome::redraw());
        assert!(!ui.help_visible);
        assert_eq!(ui.selected_index, 0, "key input was consumed");
        assert_eq!(ui.scroll_offset, 4, "scroll input was consumed");
        assert_eq!(ui.manual_scroll, None, "scroll input was consumed");
        assert_eq!(ui.browse, None, "key input was consumed");
    }

    let outcome = handle_wakeup(
        Wakeup::Press {
            code: KeyCode::Char('r'),
            mods: KeyModifiers::NONE,
        },
        &mut UiState::default(),
        &snapshot,
        &keymap,
    );
    assert_eq!(outcome.effect, Some(selection::InputEffect::Reload));
}

#[test]
fn refresh_pet_view_uses_fixed_pet_size_when_dashboard_present() {
    let ws = workspace();
    let mut snapshot = snapshot(&ws);
    snapshot.theme.pets.enabled = true;
    let mut ui = UiState::default();
    let mut painter = paint::FramePainter::new(
        PixelRenderCaps::default(),
        true,
        Some(crate::sidebar_pane::pixel::PixelSlot::new(0)),
    );

    painter.refresh_view(&mut ui, &snapshot, false);

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
    let mut ui = UiState::default();
    let mut painter = paint::FramePainter::new(
        PixelRenderCaps {
            pixel_transport: true,
            kitty_clients: true,
        },
        false,
        Some(crate::sidebar_pane::pixel::PixelSlot::new(0)),
    );

    painter.refresh_view(&mut ui, &snapshot, false);
    let raster = crate::sidebar_pane::pixel::meter::MeterRaster::new(
        2,
        0.5,
        [1, 2, 3],
        Vec::new(),
        [4, 5, 6],
    );
    let first_id = ui
        .meter_pixels
        .as_mut()
        .expect("meter pixels")
        .intern(raster.clone())
        .expect("first raster");

    painter.refresh_view(&mut ui, &snapshot, false);
    assert_eq!(
        ui.meter_pixels
            .as_mut()
            .expect("persistent meter pixels")
            .intern(raster),
        Some(first_id),
        "refreshing the view keeps the content interning table"
    );

    snapshot.theme.display.pixel = crate::config::PixelMode::Off;
    painter.refresh_view(&mut ui, &snapshot, false);
    assert!(ui.meter_pixels.is_none());
}

#[test]
fn meter_transmission_paints_visible_rasters_once() {
    let ws = workspace();
    let snapshot = snapshot(&ws);
    let mut ui = UiState::default();
    let mut painter = paint::FramePainter::new(
        PixelRenderCaps {
            pixel_transport: true,
            kitty_clients: true,
        },
        false,
        Some(crate::sidebar_pane::pixel::PixelSlot::new(0)),
    );

    painter.refresh_view(&mut ui, &snapshot, false);
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
        .ensure_meters_transmitted(&mut first, &ui, 0)
        .expect("transmit visible meter");
    let text = String::from_utf8_lossy(&first);
    assert!(text.contains("a=t"));
    assert!(text.contains(&format!("i={image_id}")));

    let mut repeat = Vec::new();
    painter
        .ensure_meters_transmitted(&mut repeat, &ui, 1)
        .expect("deduplicate resident meter");
    assert!(repeat.is_empty());

    ui.meter_pixels = None;
    let mut disabled = Vec::new();
    painter
        .ensure_meters_transmitted(&mut disabled, &ui, 2)
        .expect("skip disabled meters");
    let commands = kitty_commands(&disabled);
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0]["d"], "I");
    assert_eq!(commands[0]["i"], image_id.to_string());
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
        .draw_and_paint(&mut terminal, &snapshot, None, &mut ui)
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
        .draw_and_paint(&mut terminal, &snapshot, None, &mut ui)
        .expect("shifted draw");
    let second_len = output.0.lock().unwrap().len();

    painter
        .draw_and_paint(&mut terminal, &snapshot, None, &mut ui)
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
    assert!(!second.contains("\x1b[?2026"));
    assert!(!steady.contains("\x1b[?2026"));
    assert!(
        !String::from_utf8_lossy(&output[first_len..]).contains("a=d,d=I"),
        "layout shifts must keep resident kitty images alive"
    );
    let output = String::from_utf8_lossy(&output);
    assert_eq!(
        output
            .matches(std::str::from_utf8(BEGIN_SYNC).unwrap())
            .count(),
        1
    );
    assert_eq!(
        output
            .matches(std::str::from_utf8(END_SYNC).unwrap())
            .count(),
        1
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
    config.notification_prefs.enabled = false;
    config.notification_prefs.desktop = crate::config::DesktopNotificationMode::Osc;
    config.notification_prefs.sound = crate::config::NotificationSoundMode::Bell;
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
            pr_stack: Default::default(),
            key: "/repo/main".to_owned(),
            label: "main".to_owned(),
            label_qualifier: None,
            kind: crate::store::snapshot::SidebarWorktreeKind::Worktree,
            team: None,
            cohort_effort: None,
            pipeline: None,
            status_counts: Vec::new(),
            rows: vec![crate::store::snapshot::SidebarRow {
                id: "agent-1".to_owned(),
                name: "claude".to_owned(),
                pane: Some(pane("terminal_11", "tab_1", false)),
                worktree_path: Some("/repo/main".to_owned()),
                worktree_branch: Some("main".to_owned()),
                channel: None,
                unread,
                inactive: false,
                archived: false,
                attention_score: 0,
                last_activity: Timestamp::now(),
                card: crate::store::snapshot::RowCard::Agent(Box::new(
                    crate::store::snapshot::AgentCard {
                        status,
                        phase: crate::agents::TurnPhase::Idle,
                        ..crate::store::snapshot::AgentCard::default()
                    },
                )),
            }],
            diff_added: None,
            diff_removed: None,
            commits_ahead: None,
            commits_behind: None,
            trunk: None,
            worktree_backed: false,
            finished: false,
            clean: None,
            landed: None,
            trunk_sync: None,
            pr_state: None,
            pr_queue: None,
            ci: None,
            pr_number: None,
            pr_url: None,
        }];
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
    use super::socket::{forward, forwarding_socket};
    use std::sync::atomic::AtomicBool;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inbox.sock");
    let inbox = std::os::unix::net::UnixDatagram::bind(&path).unwrap();
    super::fixtures::fill_inbox(&path);
    let (tx, rx) = std::sync::mpsc::channel();
    let target = path.clone();
    std::thread::spawn(move || {
        let waker = forwarding_socket().unwrap();
        let _ = tx.send(forward(
            &waker,
            b"press:-:char:j",
            &target,
            &AtomicBool::new(true),
        ));
    });
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5)),
        Ok(false),
        "a stop is not held up by a renderer that stopped reading"
    );

    // Running, it waits for room rather than drop the word.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let waker = forwarding_socket().unwrap();
        let _ = tx.send(forward(
            &waker,
            b"press:-:char:j",
            &path,
            &AtomicBool::new(false),
        ));
    });
    std::thread::sleep(Duration::from_millis(400));
    let mut word = [0u8; 16];
    inbox.recv(&mut word).unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(true));
}
