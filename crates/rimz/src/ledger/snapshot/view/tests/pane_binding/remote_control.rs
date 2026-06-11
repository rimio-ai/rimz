use super::*;

#[test]
fn remote_control_host_panes_render_no_rows() {
    let mut rc_pane = pane("%2", "claude", "/repo/main");
    rc_pane.view_name = Some(crate::remote_control::VIEW_NAME.to_owned());

    for (label, panes, expected_names) in [
        (
            "full command line host beside a shell",
            vec![
                pane("%1", "zsh", "/repo/main"),
                pane("%2", "claude remote-control --spawn worktree", "/repo/main"),
            ],
            vec!["zsh"],
        ),
        ("host detected by view name", vec![rc_pane], Vec::new()),
    ] {
        let snapshot = room(Vec::new(), Vec::new()).with_live_panes(panes, None);
        let names = rows(&snapshot)
            .iter()
            .map(|row| row.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, expected_names, "{label}");
    }
}

// ── Pane binding: stamped ids, live overlays, one pane = one row ─────────────
