use std::path::{Path, PathBuf};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyEvent, MouseEvent};

use super::*;

const NOW_SECONDS: i64 = 2_000_000;

fn at(seconds: i64) -> jiff::Timestamp {
    jiff::Timestamp::from_second(seconds).unwrap()
}

fn now() -> jiff::Timestamp {
    at(NOW_SECONDS)
}

fn room(
    name: &str,
    mux: MuxName,
    root: &str,
    updated_at: jiff::Timestamp,
    stats: Option<RoomStats>,
) -> RoomRow {
    RoomRow {
        room: LiveRoom {
            session_name: name.to_owned(),
            mux,
            project_root: root.into(),
            workspace_id: rimz::WorkspaceId::from_project_root(Path::new(root)),
            updated_at,
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
    last_prompt_at: Option<jiff::Timestamp>,
) -> RoomStats {
    RoomStats {
        agents: agents(kinds, attention),
        headline: SpendWindow {
            sessions,
            tokens,
            usd,
            ..SpendWindow::default()
        },
        last_prompt_at,
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
            at(NOW_SECONDS - 300),
            Some(stats(
                &[("claude", 2)],
                1,
                12,
                88_000,
                4.2,
                Some(at(NOW_SECONDS - 60)),
            )),
        ),
        room(
            "rimz-infra",
            MuxName::Tmux,
            "/repo/infra",
            at(NOW_SECONDS - 200),
            Some(stats(
                &[("codex", 1)],
                0,
                3,
                1_200,
                0.75,
                Some(at(NOW_SECONDS - 120)),
            )),
        ),
        room(
            "rimz-quiet",
            MuxName::Zellij,
            "/repo/quiet",
            at(NOW_SECONDS - 100),
            None,
        ),
    ]
}

#[test]
fn room_agents_count_only_pane_bound_root_sessions() {
    let now = jiff::Timestamp::UNIX_EPOCH;
    let mut live = rimz::testkit::agent_state("codex", "live", now);
    live.status = rimz::agents::AgentStatus::Waiting;
    live.turn_started_at = Some(now);
    let mut departed = rimz::testkit::agent_state("claude", "departed", now);
    departed.status = rimz::agents::AgentStatus::Failed;
    departed.ended_at = Some(now);
    departed.turn_started_at = Some(now + jiff::SignedDuration::from_secs(1));
    let mut snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        rimz::WorkspaceId::from_project_root(Path::new("/repo")),
        vec![live.clone(), departed],
        now,
    );
    snapshot.agent_panes.push(rimz::store::snapshot::PaneAgent {
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
        headline: headline.clone(),
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
            last_prompt_at: Some(now + jiff::SignedDuration::from_secs(1)),
        }
    );
}

#[test]
fn session_sync_is_emitted_only_in_web_mode() {
    assert!(session_sync_enabled(Mode::Web));
    assert!(!session_sync_enabled(Mode::Terminal));
}

#[test]
fn probe_rows_rank_recent_prompts_then_workspace_recency() {
    let mut picker = Picker::new(None);
    let room_stats = |last_prompt_at| stats(&[], 0, 0, 0, 0.0, Some(last_prompt_at));
    picker.apply_probe(
        vec![
            room(
                "rimz-boundary",
                MuxName::Zellij,
                "/repo/boundary",
                at(NOW_SECONDS - 20),
                Some(room_stats(at(NOW_SECONDS - PROMPT_RECENCY_WINDOW_SECS))),
            ),
            room(
                "rimz-recent-older",
                MuxName::Tmux,
                "/repo/recent-older",
                at(NOW_SECONDS - 500),
                Some(room_stats(at(NOW_SECONDS - 2))),
            ),
            room(
                "rimz-unreadable",
                MuxName::Tmux,
                "/repo/unreadable",
                at(NOW_SECONDS - 10),
                None,
            ),
            room(
                "rimz-stale",
                MuxName::Tmux,
                "/repo/stale",
                at(NOW_SECONDS - 30),
                Some(room_stats(at(NOW_SECONDS - PROMPT_RECENCY_WINDOW_SECS - 1))),
            ),
            room(
                "rimz-recent-newer",
                MuxName::Zellij,
                "/repo/recent-newer",
                at(NOW_SECONDS - 1_000),
                Some(room_stats(at(NOW_SECONDS - 1))),
            ),
            room(
                "rimz-tie-z",
                MuxName::Tmux,
                "/z/repo",
                at(NOW_SECONDS - 40),
                None,
            ),
            room(
                "rimz-tie-a",
                MuxName::Tmux,
                "/a/repo",
                at(NOW_SECONDS - 40),
                None,
            ),
        ],
        now(),
    );

    assert_eq!(
        picker
            .rows
            .iter()
            .map(|row| row.room.session_name.as_str())
            .collect::<Vec<_>>(),
        [
            "rimz-recent-newer",
            "rimz-recent-older",
            "rimz-unreadable",
            "rimz-boundary",
            "rimz-stale",
            "rimz-tie-a",
            "rimz-tie-z",
        ]
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
fn session_and_money_metrics_use_cockpit_color_roles() {
    let theme = PickerTheme::resolve(&ThemeConfig::default(), false, false);
    let spans = metric_spans(rows()[0].stats.as_ref().unwrap(), true, true, &theme);

    assert_eq!(spans[0].style, theme.accent());
    assert_eq!(spans.last().unwrap().style, theme.money());
}

#[test]
fn banner_lines_have_uniform_width() {
    assert!(
        BANNER
            .iter()
            .all(|line| UnicodeWidthStr::width(*line) == 30)
    );
}

#[test]
fn filter_matches_displayed_repo_name_and_path_then_attaches() {
    let mut picker = Picker::new(None);
    picker.apply_probe(rows(), now());

    assert_eq!(picker.selected.as_deref(), Some("rimz-docs"));
    assert_eq!(
        picker.handle_event(key(KeyCode::Char('f'), KeyModifiers::NONE)),
        None
    );
    assert_eq!(
        picker.handle_event(key(KeyCode::Char('r'), KeyModifiers::NONE)),
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
        Some(Action::Attach(
            "rimz-infra".to_owned(),
            "infra".to_owned(),
            MuxName::Tmux,
        ))
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
    picker.apply_probe(rows(), now());
    picker.handle_event(key(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(picker.selected.as_deref(), Some("rimz-infra"));

    let mut refreshed = rows();
    refreshed.reverse();
    picker.apply_probe(refreshed, now());
    assert_eq!(picker.selected.as_deref(), Some("rimz-infra"));

    picker.apply_probe(
        vec![room(
            "rimz-docs",
            MuxName::Zellij,
            "/repo/docs",
            now(),
            None,
        )],
        now(),
    );
    assert_eq!(picker.selected.as_deref(), Some("rimz-docs"));
}

#[test]
fn new_key_opens_the_overlay_while_other_letters_filter_rooms() {
    let mut picker = Picker::new(None);
    picker.apply_probe(rows(), now());

    assert_eq!(
        picker.handle_event(key(KeyCode::Char('n'), KeyModifiers::NONE)),
        None
    );
    assert!(matches!(picker.view, View::NewSession(_)));
    assert!(picker.filter.is_empty());

    assert_eq!(
        picker.handle_event(key(KeyCode::Esc, KeyModifiers::NONE)),
        None
    );
    assert!(matches!(picker.view, View::Rooms));
    picker.handle_event(key(KeyCode::Char('d'), KeyModifiers::NONE));
    assert_eq!(picker.filter, "d");
}

#[test]
fn new_session_overlay_filters_navigates_and_confirms_every_row_kind() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    let alpha = home.join("alpha");
    let beta = home.join("beta");
    std::fs::create_dir_all(&alpha).unwrap();
    std::fs::create_dir_all(&beta).unwrap();
    std::fs::create_dir_all(home.join(".hidden")).unwrap();
    let recent = KnownWorkspace {
        workspace_id: rimz::WorkspaceId::from_project_root(Path::new("/repo/recent")),
        project_root: PathBuf::from("/repo/recent"),
        session_name: "rimz-recent".to_owned(),
        root_class: rimz::workspace::RootClass::Repo,
        rimz_bin: None,
        updated_at: now(),
    };
    let mut overlay = NewSession::new(home.to_path_buf(), vec![recent]);

    let rows = overlay.rows();
    assert!(matches!(rows[0], NewSessionRow::Recent { .. }));
    assert!(matches!(rows[1], NewSessionRow::Current { .. }));
    assert!(matches!(rows[2], NewSessionRow::Directory { .. }));
    assert_eq!(
        overlay.selected_action(),
        Some(Action::Create(PathBuf::from("/repo/recent")))
    );
    overlay.selected = 1;
    assert_eq!(
        overlay.selected_action(),
        Some(Action::Create(home.to_path_buf()))
    );
    overlay.selected = 2;
    assert_eq!(
        overlay.selected_action(),
        Some(Action::Create(alpha.clone()))
    );

    overlay.input = "bet".to_owned();
    overlay.selected = 0;
    assert_eq!(
        overlay.rows(),
        vec![NewSessionRow::Directory {
            name: "beta".to_owned(),
            path: beta,
        }]
    );
    overlay.input.clear();
    overlay.selected = 2;
    overlay.handle_key(KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(overlay.current_dir, alpha);
    overlay.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(overlay.current_dir, home);
}

#[test]
fn new_session_overlay_render_snapshot() {
    let recent = KnownWorkspace {
        workspace_id: rimz::WorkspaceId::from_project_root(Path::new("/repo/recent")),
        project_root: PathBuf::from("/repo/recent"),
        session_name: "rimz-recent".to_owned(),
        root_class: rimz::workspace::RootClass::Repo,
        rimz_bin: None,
        updated_at: now(),
    };
    let mut picker = Picker::new(None);
    picker.view = View::NewSession(NewSession {
        current_dir: PathBuf::from("/home/dev"),
        input: String::new(),
        selected: 0,
        dormant: vec![recent],
        directories: vec![
            PathBuf::from("/home/dev/code"),
            PathBuf::from("/home/dev/notes"),
        ],
        notice: None,
    });

    insta::assert_snapshot!(render_text(&mut picker, 70, 20), @r"
                  ██████╗ ██╗███╗   ███╗███████╗
                  ██╔══██╗██║████╗ ████║╚══███╔╝
                  ██████╔╝██║██╔████╔██║  ███╔╝
                  ██╔══██╗██║██║╚██╔╝██║ ███╔╝
                  ██║  ██║██║██║ ╚═╝ ██║███████╗
                  ╚═╝  ╚═╝╚═╝╚═╝     ╚═╝╚══════╝

    ╭ new session ───────────────────────────────────────────╮
    │ path: /home/dev                                        │
    │ recent                                                 │
    │ ▸ recent  /repo/recent                                 │
    │ directories                                            │
    │   .  /home/dev                                         │
    │   code/                                                │
    │   notes/                                               │
    │                                                        │
    │                                                        │
    │ filter: _                                              │
    ╰────────────────────────────────────────────────────────╯
      ↑↓ select · →/tab open · ← back · ⏎ create · esc cancel
    ");
}

#[test]
fn escape_clears_filter_before_quitting_and_control_c_always_quits() {
    let mut picker = Picker::new(None);
    picker.apply_probe(rows(), now());
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
    picker.apply_probe(rows(), now());
    let _ = render_text(&mut picker, 100, 28);
    assert_eq!(
        picker.hit_rows,
        BTreeMap::from([
            (8, "rimz-docs".to_owned()),
            (9, "rimz-docs".to_owned()),
            (11, "rimz-infra".to_owned()),
            (12, "rimz-infra".to_owned()),
            (14, "rimz-quiet".to_owned()),
            (15, "rimz-quiet".to_owned()),
        ])
    );

    picker.handle_event(Event::Mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 2,
        row: 11,
        modifiers: KeyModifiers::NONE,
    }));
    assert_eq!(picker.selected.as_deref(), Some("rimz-infra"));

    let click = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 2,
        row: 9,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(picker.handle_event(click.clone()), None);
    assert_eq!(picker.selected.as_deref(), Some("rimz-docs"));
    assert_eq!(
        picker.handle_event(click),
        Some(Action::Attach(
            "rimz-docs".to_owned(),
            "docs".to_owned(),
            MuxName::Zellij,
        ))
    );
}

#[test]
fn picker_render_snapshots_cover_populated_filtered_empty_and_notice_frames() {
    let mut populated = Picker::new(None);
    populated.apply_probe(rows(), now());
    let mut filtered = Picker::new(None);
    filtered.apply_probe(rows(), now());
    filtered.filter = "quiet".to_owned();
    filtered.normalize_selection();
    let mut empty = Picker::new(None);
    let mut notice = Picker::new(Some("retired-room"));
    notice.apply_probe(rows(), now());
    let mut degraded = Picker::new(None);
    degraded.apply_probe(rows(), now());

    let rendered = format!(
        "POPULATED\n{}\n\nFILTERED\n{}\n\nEMPTY\n{}\n\nNOTICE\n{}\n\nDEGRADED\n{}",
        render_text(&mut populated, 100, 28),
        render_text(&mut filtered, 100, 20),
        render_text(&mut empty, 100, 20),
        render_text(&mut notice, 100, 28),
        render_text(&mut degraded, 40, 10),
    );

    insta::assert_snapshot!(rendered, @r###"
    POPULATED
                                       ██████╗ ██╗███╗   ███╗███████╗
                                       ██╔══██╗██║████╗ ████║╚══███╔╝
                                       ██████╔╝██║██╔████╔██║  ███╔╝
                                       ██╔══██╗██║██║╚██╔╝██║ ███╔╝
                                       ██║  ██║██║██║ ╚═╝ ██║███████╗
                                       ╚═╝  ╚═╝╚═╝╚═╝     ╚═╝╚══════╝

                         ╭ sessions ──────────────────────────────────────────────╮
                         │ ▸ ⌘ docs                                    /repo/docs │
                         │   claude ×2 ● 1                     ◎ 12  ◇ 88k  $4.20 │
                         │                                                        │
                         │   ⌘ infra                                  /repo/infra │
                         │   codex ×1                            ◎ 3  ◇ 1k  $0.75 │
                         │                                                        │
                         │   ⌘ quiet                                  /repo/quiet │
                         │   –                                                    │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │ filter: _                                              │
                         ╰────────────────────────────────────────────────────────╯
                          ↑↓ select · ⏎ attach · n new · type to filter · esc quit

    FILTERED
                                       ██████╗ ██╗███╗   ███╗███████╗
                                       ██╔══██╗██║████╗ ████║╚══███╔╝
                                       ██████╔╝██║██╔████╔██║  ███╔╝
                                       ██╔══██╗██║██║╚██╔╝██║ ███╔╝
                                       ██║  ██║██║██║ ╚═╝ ██║███████╗
                                       ╚═╝  ╚═╝╚═╝╚═╝     ╚═╝╚══════╝

                         ╭ sessions ──────────────────────────────────────────────╮
                         │ ▸ ⌘ quiet                                  /repo/quiet │
                         │   –                                                    │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │ filter: quiet_                                         │
                         ╰────────────────────────────────────────────────────────╯
                          ↑↓ select · ⏎ attach · n new · type to filter · esc quit

    EMPTY
                                       ██████╗ ██╗███╗   ███╗███████╗
                                       ██╔══██╗██║████╗ ████║╚══███╔╝
                                       ██████╔╝██║██╔████╔██║  ███╔╝
                                       ██╔══██╗██║██║╚██╔╝██║ ███╔╝
                                       ██║  ██║██║██║ ╚═╝ ██║███████╗
                                       ╚═╝  ╚═╝╚═╝╚═╝     ╚═╝╚══════╝

                         ╭ sessions ──────────────────────────────────────────────╮
                         │ No live RimZ sessions — run `rimz start` in a project  │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │ filter: _                                              │
                         ╰────────────────────────────────────────────────────────╯
                          ↑↓ select · ⏎ attach · n new · type to filter · esc quit

    NOTICE
                                       ██████╗ ██╗███╗   ███╗███████╗
                                       ██╔══██╗██║████╗ ████║╚══███╔╝
                                       ██████╔╝██║██╔████╔██║  ███╔╝
                                       ██╔══██╗██║██║╚██╔╝██║ ███╔╝
                                       ██║  ██║██║██║ ╚═╝ ██║███████╗
                                       ╚═╝  ╚═╝╚═╝╚═╝     ╚═╝╚══════╝

                         ╭ sessions ──────────────────────────────────────────────╮
                         │ session `retired-room` is not a live RimZ room         │
                         │ ▸ ⌘ docs                                    /repo/docs │
                         │   claude ×2 ● 1                     ◎ 12  ◇ 88k  $4.20 │
                         │                                                        │
                         │   ⌘ infra                                  /repo/infra │
                         │   codex ×1                            ◎ 3  ◇ 1k  $0.75 │
                         │                                                        │
                         │   ⌘ quiet                                  /repo/quiet │
                         │   –                                                    │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │                                                        │
                         │ filter: _                                              │
                         ╰────────────────────────────────────────────────────────╯
                          ↑↓ select · ⏎ attach · n new · type to filter · esc quit

    DEGRADED
    ╭ RimZ ── sessions ────────────────────╮
    │ ▸ ⌘ docs                  /repo/docs │
    │   claude ×2 ● 1   ◎ 12  ◇ 88k  $4.20 │
    │                                      │
    │   ⌘ infra                /repo/infra │
    │   codex ×1          ◎ 3  ◇ 1k  $0.75 │
    │                                      │
    │ filter: _                            │
    │ ↑↓ select · ⏎ attach · n new · type  │
    ╰──────────────────────────────────────╯
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
