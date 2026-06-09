use super::*;

#[test]
fn remote_control_host_pane_renders_no_row() {
    // A `claude remote-control` pane (Zellij reports the full command line)
    // is ambient infrastructure: it no longer renders as any row — its
    // presence surfaces as the provider dashboard's `⇅ rc` flag instead.
    // Only the shell pane beside it remains a row.
    let snapshot = room(Vec::new(), Vec::new()).with_live_panes(
        vec![
            pane("%1", "zsh", "/repo/main"),
            pane("%2", "claude remote-control --spawn worktree", "/repo/main"),
        ],
        None,
    );

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1, "only the shell pane is a row: {rows:?}");
    assert!(rows[0].is_process());
    assert_eq!(rows[0].name, "zsh");
    assert!(
        rows.iter().all(|row| row.name != "claude"),
        "the host pane must not produce a claude row: {rows:?}",
    );
}

#[test]
fn remote_control_host_pane_filtered_when_detected_by_view_name() {
    // tmux reports only the `claude` basename, but names the window — so the
    // view name marks the host, and that pane is filtered out the same way.
    let mut rc_pane = pane("%2", "claude", "/repo/main");
    rc_pane.view_name = Some(crate::remote_control::VIEW_NAME.to_owned());
    let snapshot = room(Vec::new(), Vec::new()).with_live_panes(vec![rc_pane], None);

    let rows = rows(&snapshot);
    assert!(
        rows.is_empty(),
        "a host-only pane set produces no rows: {rows:?}",
    );
}

// ── Pane binding: stamped ids, live overlays, one pane = one row ─────────────

#[test]
fn live_panes_add_process_rows_without_attention_counts() {
    let snapshot =
        room(Vec::new(), Vec::new()).with_live_panes(vec![pane("%1", "zsh", "/repo/main")], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let row = &snapshot.worktree_groups[0].rows[0];
    assert!(row.is_process());
    assert_eq!(row.name, "zsh");
    assert_eq!(row.status(), None);
    assert!(snapshot.worktree_groups[0].status_counts.is_empty());
}
