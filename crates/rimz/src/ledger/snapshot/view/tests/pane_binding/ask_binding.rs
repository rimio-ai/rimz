use super::*;

fn script_ask_for_pane(pane: Option<PaneRef>) -> FeedItem {
    let mut item = FeedItem::new(
        workspace(),
        Surface::Script,
        FeedKind::Question,
        "approve deploy?",
        "deploy",
        "script",
    );
    item.pane = pane;
    item
}

#[test]
fn standalone_script_ask_requires_matching_frame_pane_and_uses_frame_truth() {
    let stale_pane = PaneRef {
        view_id: Some("@stale".to_owned()),
        command: Some("old-deploy".to_owned()),
        cwd: Some("/old".to_owned()),
        ..pane("%7", "old-deploy", "/old")
    };
    let mut frame_pane = pane("%7", "deploy", "/repo/main");
    frame_pane.view_id = Some("@fresh".to_owned());
    frame_pane.is_focused = true;
    let item = script_ask_for_pane(Some(stale_pane));
    let request_id = item.request_id.clone();

    let snapshot = room(vec![item], Vec::new()).with_live_panes(vec![frame_pane.clone()], None);

    let projected = rows(&snapshot);
    assert_eq!(projected.len(), 1, "the ask owns the pane row slot");
    let row = projected[0];
    assert_eq!(row.request_id(), Some(&request_id));
    assert_eq!(row.task(), Some("approve deploy?"));
    assert_eq!(row.worktree_path.as_deref(), Some("/repo/main"));
    assert_eq!(row.pane.as_ref(), Some(&frame_pane));

    for case in [
        (
            "without pane",
            script_ask_for_pane(None),
            vec![pane("%1", "zsh", "/repo/main")],
        ),
        (
            "absent pane",
            script_ask_for_pane(Some(pane("%7", "deploy", "/repo/main"))),
            vec![pane("%8", "zsh", "/repo/main")],
        ),
        (
            "reused pane id",
            script_ask_for_pane(Some(pane_started("%7", "/repo/main", ago(60)))),
            vec![pane_started("%7", "/repo/main", ago(5))],
        ),
    ] {
        let (label, item, panes) = case;
        let request_id = item.request_id.clone();

        let snapshot = room(vec![item], Vec::new()).with_live_panes(panes, None);

        let rows = rows(&snapshot);
        assert_eq!(rows.len(), 1, "{label}");
        assert!(
            rows.iter().all(|row| row.request_id() != Some(&request_id)),
            "{label}: unmatched asks remain metadata only"
        );
    }
}

#[test]
fn standalone_ask_on_an_agents_pane_folds_onto_the_agent_row() {
    // A script ask raised from inside an agent's pane (the agent shelling out
    // to `rimz feed ask`) folds onto the agent's row — identity and capability
    // line kept, the ask's waiting status and request taken — and outranks the
    // session's own pending agent-hook ask: the script blocks the pane's
    // foreground right now.
    let mut claude = agent("claude", "sess-a", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    claude.model = Some("opus-4".to_owned());
    let script_ask = script_ask_for_pane(Some(pane("%1", "claude", "/repo/main")));
    let request_id = script_ask.request_id.clone();
    let items = vec![
        agent_ask(FeedKind::Permission, "claude", "sess-a"),
        script_ask,
    ];

    let snapshot =
        room(items, vec![claude]).with_live_panes(vec![pane("%1", "claude", "/repo/main")], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1, "one pane, one row: {rows:?}");
    let row = rows[0];
    assert!(row.is_agent());
    assert_eq!(row.id, "sess-a", "the agent keeps the row identity");
    assert_eq!(row.name, "claude");
    assert_eq!(
        row.model(),
        Some("opus-4"),
        "the capability line survives the fold"
    );
    assert_eq!(row.status(), Some(AgentStatus::Waiting));
    assert_eq!(
        row.request_id(),
        Some(&request_id),
        "the pane-blocking script ask outranks the agent-hook ask"
    );
    assert_eq!(row.surface(), Some(Surface::Script));
}

#[test]
fn standalone_ask_on_a_wired_idle_lazy_pane_folds_onto_the_idle_row() {
    let item = script_ask_for_pane(Some(pane("term1", "codex", "/repo/main")));
    let request_id = item.request_id.clone();
    let mut snapshot = room(vec![item], Vec::new());
    snapshot.wired_kinds = vec!["codex".to_owned()];

    let snapshot = snapshot.with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1, "one pane, one row: {rows:?}");
    let row = rows[0];
    assert!(row.is_agent());
    assert_eq!(
        row.name, "codex",
        "the idle lazy identity survives the fold"
    );
    assert_eq!(row.id, "tmux:term1");
    assert_eq!(row.status(), Some(AgentStatus::Waiting));
    assert_eq!(row.request_id(), Some(&request_id));
}
