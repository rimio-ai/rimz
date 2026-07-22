use std::path::Path;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyEvent, MouseEvent};

use super::*;

fn room(name: &str, mux: MuxName, root: &str, stats: Option<RoomStats>) -> RoomRow {
    RoomRow {
        room: rimz::web::LiveRoom {
            session_name: name.to_owned(),
            mux,
            project_root: root.into(),
            workspace_id: rimz::WorkspaceId::from_project_root(Path::new(root)),
        },
        stats,
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

fn stats(
    kinds: &[(&str, usize)],
    attention: usize,
    sessions: u32,
    tokens: u64,
    usd: f64,
) -> RoomStats {
    RoomStats {
        agents: agents(kinds, attention),
        headline: SpendWindow {
            sessions,
            tokens,
            usd,
            ..SpendWindow::default()
        },
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
            Some(stats(&[("claude", 2)], 1, 12, 88_000, 4.2)),
        ),
        room(
            "rimz-infra",
            MuxName::Tmux,
            "/repo/infra",
            Some(stats(&[("codex", 1)], 0, 3, 1_200, 0.75)),
        ),
        room("rimz-quiet", MuxName::Zellij, "/repo/quiet", None),
    ]
}

#[test]
fn room_agents_count_only_pane_bound_root_sessions() {
    let now = jiff::Timestamp::UNIX_EPOCH;
    let mut live = rimz::testkit::agent_state("codex", "live", now);
    live.status = rimz::agents::AgentStatus::Waiting;
    let mut departed = rimz::testkit::agent_state("claude", "departed", now);
    departed.status = rimz::agents::AgentStatus::Failed;
    departed.ended_at = Some(now);
    let mut snapshot = rimz::SidebarSnapshot::build_with_agents(
        rimz::WorkspaceId::from_project_root(Path::new("/repo")),
        vec![live.clone(), departed],
        now,
    );
    snapshot.agent_panes.push(rimz::PaneAgent {
        kind: live.kind.clone(),
        kind_ordinal: None,
        name: None,
        name_explicit: false,
        profile: None,
        role: None,
        channel: None,
        agent_id: Some(live.agent_id),
        pane_id: rimz::PaneId::from_parts(MuxName::Zellij, "%1"),
        pane_pid: None,
        worktree_path: None,
        worktree_branch: None,
    });
    let headline = SpendWindow {
        sessions: 4,
        tokens: 9_000,
        usd: 1.25,
        ..SpendWindow::default()
    };
    snapshot.workspace_value_tally = Some(rimz::SpendTally {
        headline,
        ..rimz::SpendTally::default()
    });

    assert_eq!(
        RoomStats::from_snapshot(&snapshot),
        RoomStats {
            agents: RoomAgents {
                by_kind: vec![(AgentKind::new_unchecked("codex"), 1)],
                attention: 1,
            },
            headline,
        }
    );
}

#[test]
fn session_sync_osc_sets_and_clears_the_browser_target() {
    assert_eq!(
        session_sync_osc(Some("rimz-docs-a1b2c3")),
        "\x1b]7717;rimz-session=rimz-docs-a1b2c3\x07"
    );
    assert_eq!(session_sync_osc(None), "\x1b]7717;rimz-session=\x07");
}

#[test]
fn probe_rows_sort_by_displayed_repo_then_path() {
    let mut picker = Picker::new(None);
    picker.apply_probe(vec![
        room("rimz-first", MuxName::Zellij, "/z/repo", None),
        room("rimz-second", MuxName::Tmux, "/a/repo", None),
        room("rimz-alpha", MuxName::Tmux, "/x/alpha", None),
    ]);

    assert_eq!(
        picker
            .rows
            .iter()
            .map(|row| row.room.session_name.as_str())
            .collect::<Vec<_>>(),
        ["rimz-alpha", "rimz-second", "rimz-first"]
    );
}

#[test]
fn card_width_drops_tokens_then_sessions_and_left_truncates_paths() {
    let row = rows().remove(0);
    let theme = PickerTheme::resolve(&ThemeConfig::default(), false, true);
    let stats_text = |width| {
        room_lines(&row, true, width, &theme)[1]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    };

    assert_eq!(stats_text(28), "  claude ×2 ● 1  ◎ 12  $4.20");
    assert_eq!(stats_text(22), "  claude ×2 ● 1  $4.20");
    assert_eq!(truncate_left_width("/very/long/path", 6), "…/path");
}

#[test]
fn filter_matches_displayed_repo_name_and_path_then_attaches() {
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

    picker.filter = "repo/docs".to_owned();
    picker.normalize_selection();
    assert_eq!(picker.selected.as_deref(), Some("rimz-docs"));

    picker.filter = "rimz-docs".to_owned();
    picker.normalize_selection();
    assert!(picker.visible().is_empty());
    assert_eq!(picker.selected, None);
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
fn wheel_moves_selection_and_both_card_lines_are_clickable() {
    let mut picker = Picker::new(None);
    picker.apply_probe(rows());
    let _ = render_text(&mut picker, 76, 13);
    assert_eq!(
        picker.hit_rows,
        BTreeMap::from([
            (1, "rimz-docs".to_owned()),
            (2, "rimz-docs".to_owned()),
            (4, "rimz-infra".to_owned()),
            (5, "rimz-infra".to_owned()),
            (7, "rimz-quiet".to_owned()),
            (8, "rimz-quiet".to_owned()),
        ])
    );

    picker.handle_event(Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 2,
        row: 1,
        modifiers: KeyModifiers::NONE,
    }));
    assert_eq!(picker.selected.as_deref(), Some("rimz-infra"));

    let click = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 2,
        row: 2,
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
    let mut degraded = Picker::new(None);
    degraded.apply_probe(rows());

    let rendered = format!(
        "POPULATED\n{}\n\nFILTERED\n{}\n\nEMPTY\n{}\n\nNOTICE\n{}\n\nDEGRADED\n{}",
        render_text(&mut populated, 76, 13),
        render_text(&mut filtered, 76, 9),
        render_text(&mut empty, 76, 9),
        render_text(&mut notice, 76, 14),
        render_text(&mut degraded, 30, 9),
    );

    insta::assert_snapshot!(rendered, @r###"
    POPULATED
    ╭ RimZ ── sessions ────────────────────────────────────────────────────────╮
    │▸ ⌘ docs                                                        /repo/docs│
    │  claude ×2 ● 1                                         ◎ 12  ◇ 88k  $4.20│
    │                                                                          │
    │  ⌘ infra                                                      /repo/infra│
    │  codex ×1                                                ◎ 3  ◇ 1k  $0.75│
    │                                                                          │
    │  ⌘ quiet                                                      /repo/quiet│
    │  –                                                                       │
    │                                                                          │
    │filter: _                                                                 │
    │↑↓ select · ⏎ attach · type to filter · esc quit                          │
    ╰──────────────────────────────────────────────────────────────────────────╯

    FILTERED
    ╭ RimZ ── sessions ────────────────────────────────────────────────────────╮
    │▸ ⌘ quiet                                                      /repo/quiet│
    │  –                                                                       │
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
    │▸ ⌘ docs                                                        /repo/docs│
    │  claude ×2 ● 1                                         ◎ 12  ◇ 88k  $4.20│
    │                                                                          │
    │  ⌘ infra                                                      /repo/infra│
    │  codex ×1                                                ◎ 3  ◇ 1k  $0.75│
    │                                                                          │
    │  ⌘ quiet                                                      /repo/quiet│
    │  –                                                                       │
    │                                                                          │
    │filter: _                                                                 │
    │↑↓ select · ⏎ attach · type to filter · esc quit                          │
    ╰──────────────────────────────────────────────────────────────────────────╯

    DEGRADED
    ╭ RimZ ── sessions ──────────╮
    │▸ ⌘ docs          /repo/docs│
    │  claude ×2 ● 1  ◎ 12  $4.20│
    │                            │
    │                            │
    │                            │
    │filter: _                   │
    │↑↓ select · ⏎ attach · type │
    ╰────────────────────────────╯
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
