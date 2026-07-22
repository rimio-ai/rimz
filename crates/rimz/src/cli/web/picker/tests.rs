use std::path::Path;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyEvent, MouseEvent};

use super::*;

fn room(name: &str, mux: MuxName, root: &str, agents: Option<RoomAgents>) -> RoomRow {
    RoomRow {
        room: rimz::web::LiveRoom {
            session_name: name.to_owned(),
            mux,
            project_root: root.into(),
            workspace_id: rimz::WorkspaceId::from_project_root(Path::new(root)),
        },
        agents,
    }
}

fn agents(kinds: &[(&str, usize)], attention: usize) -> RoomAgents {
    RoomAgents {
        by_kind: kinds
            .iter()
            .map(|(kind, count)| (AgentKind::new_unchecked(*kind), *count))
            .collect(),
        attention,
    }
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

fn rows() -> Vec<RoomRow> {
    vec![
        room(
            "rimz-docs",
            MuxName::Zellij,
            "/repo/docs",
            Some(agents(&[("claude", 2)], 1)),
        ),
        room(
            "rimz-infra",
            MuxName::Tmux,
            "/repo/infra",
            Some(agents(&[("codex", 1)], 0)),
        ),
        room("rimz-quiet", MuxName::Zellij, "/repo/quiet", None),
    ]
}

#[test]
fn filter_narrows_rows_and_enter_attaches_the_visible_selection() {
    let mut picker = Picker::new(None);
    picker.apply_probe(rows());

    assert_eq!(picker.selected.as_deref(), Some("rimz-docs"));
    assert_eq!(
        picker.handle_event(key(KeyCode::Char('i'), KeyModifiers::NONE)),
        None
    );
    assert_eq!(
        picker.handle_event(key(KeyCode::Char('n'), KeyModifiers::NONE)),
        None
    );
    assert_eq!(
        picker
            .visible()
            .iter()
            .map(|row| row.room.session_name.as_str())
            .collect::<Vec<_>>(),
        vec!["rimz-infra"]
    );
    assert_eq!(picker.selected.as_deref(), Some("rimz-infra"));
    assert_eq!(
        picker.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)),
        Some(Action::Attach("rimz-infra".to_owned(), MuxName::Tmux))
    );
}

#[test]
fn probe_retains_a_visible_session_selection_and_clamps_a_vanished_one() {
    let mut picker = Picker::new(None);
    picker.apply_probe(rows());
    picker.handle_event(key(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(picker.selected.as_deref(), Some("rimz-infra"));

    let mut refreshed = rows();
    refreshed.reverse();
    picker.apply_probe(refreshed);
    assert_eq!(picker.selected.as_deref(), Some("rimz-infra"));

    picker.apply_probe(vec![room("rimz-docs", MuxName::Zellij, "/repo/docs", None)]);
    assert_eq!(picker.selected.as_deref(), Some("rimz-docs"));
}

#[test]
fn escape_clears_filter_before_quitting_and_control_c_always_quits() {
    let mut picker = Picker::new(None);
    picker.apply_probe(rows());
    picker.handle_event(key(KeyCode::Char('d'), KeyModifiers::NONE));

    assert_eq!(
        picker.handle_event(key(KeyCode::Esc, KeyModifiers::NONE)),
        None
    );
    assert!(picker.filter.is_empty());
    assert_eq!(
        picker.handle_event(key(KeyCode::Esc, KeyModifiers::NONE)),
        Some(Action::Quit)
    );
    assert_eq!(
        picker.handle_event(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        Some(Action::Quit)
    );
}

#[test]
fn wheel_moves_selection_and_a_second_click_on_one_row_attaches() {
    let mut picker = Picker::new(None);
    picker.apply_probe(rows());
    picker.hit_rows = BTreeMap::from([(4, "rimz-docs".to_owned()), (5, "rimz-infra".to_owned())]);

    picker.handle_event(Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 2,
        row: 4,
        modifiers: KeyModifiers::NONE,
    }));
    assert_eq!(picker.selected.as_deref(), Some("rimz-infra"));

    let click = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 2,
        row: 4,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(picker.handle_event(click.clone()), None);
    assert_eq!(picker.selected.as_deref(), Some("rimz-docs"));
    assert_eq!(
        picker.handle_event(click),
        Some(Action::Attach("rimz-docs".to_owned(), MuxName::Zellij))
    );
}

#[test]
fn picker_render_snapshots_cover_populated_filtered_empty_and_notice_frames() {
    let mut populated = Picker::new(None);
    populated.apply_probe(rows());
    let mut filtered = Picker::new(None);
    filtered.apply_probe(rows());
    filtered.filter = "quiet".to_owned();
    filtered.normalize_selection();
    let mut empty = Picker::new(None);
    let mut notice = Picker::new(Some("retired-room"));
    notice.apply_probe(rows());

    let rendered = format!(
        "POPULATED\n{}\n\nFILTERED\n{}\n\nEMPTY\n{}\n\nNOTICE\n{}",
        render_text(&mut populated, 76, 9),
        render_text(&mut filtered, 76, 9),
        render_text(&mut empty, 76, 9),
        render_text(&mut notice, 76, 9),
    );

    insta::assert_snapshot!(rendered, @r###"
    POPULATED
    ╭ RimZ ── sessions ────────────────────────────────────────────────────────╮
    │▸ rimz-docs   zellij   /repo/docs                            claude ×2 ● 1│
    │  rimz-infra  tmux     /repo/infra                                codex ×1│
    │  rimz-quiet  zellij   /repo/quiet                                       –│
    │                                                                          │
    │                                                                          │
    │filter: _                                                                 │
    │↑↓ select · ⏎ attach · type to filter · esc quit                          │
    ╰──────────────────────────────────────────────────────────────────────────╯

    FILTERED
    ╭ RimZ ── sessions ────────────────────────────────────────────────────────╮
    │▸ rimz-quiet  zellij   /repo/quiet                                       –│
    │                                                                          │
    │                                                                          │
    │                                                                          │
    │                                                                          │
    │filter: quiet_                                                            │
    │↑↓ select · ⏎ attach · type to filter · esc quit                          │
    ╰──────────────────────────────────────────────────────────────────────────╯

    EMPTY
    ╭ RimZ ── sessions ────────────────────────────────────────────────────────╮
    │No live RimZ sessions — run `rimz start` in a project                     │
    │                                                                          │
    │                                                                          │
    │                                                                          │
    │                                                                          │
    │filter: _                                                                 │
    │↑↓ select · ⏎ attach · type to filter · esc quit                          │
    ╰──────────────────────────────────────────────────────────────────────────╯

    NOTICE
    ╭ RimZ ── sessions ────────────────────────────────────────────────────────╮
    │session `retired-room` is not a live RimZ room                            │
    │▸ rimz-docs   zellij   /repo/docs                            claude ×2 ● 1│
    │  rimz-infra  tmux     /repo/infra                                codex ×1│
    │  rimz-quiet  zellij   /repo/quiet                                       –│
    │                                                                          │
    │filter: _                                                                 │
    │↑↓ select · ⏎ attach · type to filter · esc quit                          │
    ╰──────────────────────────────────────────────────────────────────────────╯
    "###);
}

fn render_text(picker: &mut Picker, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    let theme = PickerTheme::resolve(&ThemeConfig::default(), false, true);
    terminal
        .draw(|frame| render(frame, picker, &theme))
        .expect("draw picker");
    buffer_text(terminal.backend().buffer())
}

fn buffer_text(buffer: &Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| {
            let mut line = String::new();
            for x in 0..buffer.area.width {
                line.push_str(buffer[(x, y)].symbol());
            }
            line.trim_end().to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
