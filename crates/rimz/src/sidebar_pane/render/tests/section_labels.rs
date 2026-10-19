use super::*;

fn painted(
    snapshot: &SidebarSnapshot,
    alert: Option<&Alert>,
    width: u16,
    height: u16,
) -> (UiState, Buffer) {
    let mut ui = fixed_theme_ui(snapshot, &selected_ui());
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    draw_to_terminal(&mut terminal, snapshot, alert, &mut ui).unwrap();
    (ui, terminal.backend().buffer().clone())
}

#[test]
fn physical_rows_label_cockpit_providers_chrome_groups_cards_processes_and_padding() {
    let mut coder = agent(
        "coder",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("Implement routing"),
    );
    coder.role = Some("coder".to_owned());
    let mut snapshot = snapshot_with(vec![coder]);
    let mut panes = vec![pane("%agent-0", "claude", "/repo/main")];
    panes.push(pane("%shell", "zsh", "/repo/main"));
    snapshot = snapshot.with_live_panes(panes, None);
    snapshot.providers = vec![provider_panel(
        "claude",
        "Claude",
        117,
        true,
        false,
        Some((35, 50)),
    )];
    let (ui, buffer) = painted(&snapshot, None, 54, 60);
    let rows = ui
        .frame_rows(&buffer)
        .map(|row| (row.label.as_str().to_owned(), row.plain_text()))
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 60);
    for label in [
        "cockpit",
        "providers",
        "chrome",
        "#main",
        "@coder#main",
        "zsh#main",
        "",
    ] {
        assert!(
            rows.iter().any(|(actual, _)| actual == label),
            "missing {label:?}: {rows:?}"
        );
    }
    for (needle, label) in [
        ("query-engine", "cockpit"),
        ("Implement routing", "@coder#main"),
        ("zsh", "zsh#main"),
        ("? for help", "chrome"),
    ] {
        assert!(
            rows.iter()
                .any(|(actual, text)| actual == label && text.contains(needle)),
            "{needle:?} should belong to {label}: {rows:?}"
        );
    }
    for row in ui.frame_rows(&buffer) {
        let mut ansi = Vec::new();
        row.write_ansi(&mut ansi).unwrap();
        let ansi = String::from_utf8(ansi).unwrap();
        assert_eq!(
            anstream::adapter::strip_str(&ansi).to_string(),
            format!("{}\n", row.plain_text())
        );
        assert!(!row.plain_text().contains('\x1b'));
    }
}

#[test]
fn wrapped_alert_continuations_keep_chrome_label_on_painted_rows() {
    let snapshot = snapshot_with(Vec::new());
    let alert = Alert {
        reason: "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega".to_owned(),
        since: fixed_now(),
        recovered_at: None,
    };
    let (ui, buffer) = painted(&snapshot, Some(&alert), 26, 30);
    let rows = ui
        .frame_rows(&buffer)
        .map(|row| (row.label.as_str().to_owned(), row.plain_text()))
        .collect::<Vec<_>>();
    let first = rows
        .iter()
        .position(|(_, text)| text.contains("alpha"))
        .expect("alert start");
    let last = rows
        .iter()
        .position(|(_, text)| text.contains("omega"))
        .expect("alert continuation");
    assert!(last > first);
    assert!(
        rows[first..=last]
            .iter()
            .all(|(label, _)| label == "chrome"),
        "{rows:?}"
    );
}

#[test]
fn painted_row_text_strips_osc_links_and_ansi_keeps_them() {
    let ui = UiState::default();
    let mut buffer = Buffer::empty(Rect::new(4, 2, 6, 1));
    buffer[(4, 2)].set_symbol("\x1b]8;;https://example.test\x1b\\X\x1b]8;;\x1b\\");
    buffer[(5, 2)].set_symbol("界");
    buffer[(7, 2)].set_symbol("Z");
    let row = ui.frame_rows(&buffer).next().unwrap();
    assert_eq!(row.plain_text(), "X界Z  ");
    let mut ansi = Vec::new();
    row.write_ansi(&mut ansi).unwrap();
    assert!(ansi.windows(4).any(|bytes| bytes == b"\x1b]8;"));
    let ansi = String::from_utf8(ansi).unwrap();
    assert_eq!(anstream::adapter::strip_str(&ansi).to_string(), "X界Z  \n");
}

#[test]
fn section_filter_matches_exact_rows_and_group_suffixes() {
    for (label, section, expected) in [
        ("#main", "#main", true),
        ("@coder#main", "#main", true),
        ("zsh#main", "#main", true),
        ("@coder#mainly", "#main", false),
        ("@coder#main", "@coder#main", true),
        ("@coder#main", "@coder", false),
        ("cockpit", "cockpit", true),
    ] {
        assert_eq!(
            SectionLabel(label.to_owned()).matches(section),
            expected,
            "{label} vs {section}"
        );
    }
    let mut coder = agent(
        "coder",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("Implement routing"),
    );
    coder.role = Some("coder".to_owned());
    let snapshot = snapshot_with(vec![coder]);
    for height in [Some(32), None] {
        for section in ["cockpit", "#main", "@coder#main", "missing"] {
            let mut output = Vec::new();
            render_section_line_ansi(&mut output, &snapshot, 54, height, section).unwrap();
            let text = String::from_utf8(output).unwrap();
            let text = anstream::adapter::strip_str(&text).to_string();
            if section == "missing" {
                assert!(text.is_empty());
                continue;
            }
            assert!(!text.is_empty(), "section {section} missing");
            assert_eq!(text.contains("query-engine"), section == "cockpit");
            assert_eq!(text.contains("Implement routing"), section != "cockpit");
        }
    }
}
