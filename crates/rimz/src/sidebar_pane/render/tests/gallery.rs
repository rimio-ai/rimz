use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;

#[test]
fn render_gallery_draws_delimiters_between_columns() {
    let alpha = gallery_snapshot("alpha-room", "agent-alpha", "claude");
    let bravo = gallery_snapshot("bravo-room", "agent-bravo", "codex");
    let charlie = gallery_snapshot("charlie-room", "agent-charlie", "pi");
    let mut alpha_ui = UiState::default();
    let mut bravo_ui = UiState::default();
    let mut charlie_ui = UiState::default();
    let mut columns = vec![
        GalleryColumn {
            snapshot: &alpha,
            ui: &mut alpha_ui,
        },
        GalleryColumn {
            snapshot: &bravo,
            ui: &mut bravo_ui,
        },
        GalleryColumn {
            snapshot: &charlie,
            ui: &mut charlie_ui,
        },
    ];
    let backend = TestBackend::new(62, 12);
    let mut terminal = Terminal::new(backend).unwrap();

    draw_gallery_to_terminal(&mut terminal, &mut columns).unwrap();

    let buffer = terminal.backend().buffer();
    for y in 0..12 {
        assert_eq!(buffer[(20, y)].symbol(), "│");
        assert_eq!(buffer[(41, y)].symbol(), "│");
    }
    assert!(band_text(buffer, 0..20).contains("alpha-room"));
    assert!(band_text(buffer, 21..41).contains("bravo-room"));
    assert!(band_text(buffer, 42..62).contains("charlie-room"));
}

fn gallery_snapshot(display_name: &str, id: &str, kind: &str) -> SidebarSnapshot {
    let mut snapshot = snapshot_with(vec![agent(
        id,
        kind,
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("render gallery"),
    )]);
    snapshot.display_name = display_name.to_owned();
    snapshot
}

fn band_text(buffer: &Buffer, xs: std::ops::Range<u16>) -> String {
    let mut text = String::new();
    for y in 0..buffer.area.height {
        for x in xs.clone() {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}
