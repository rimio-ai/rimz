use super::*;

#[test]
fn commandless_unbound_pane_folds_no_row() {
    // A pane whose command is still unknown after frame rotation — mid-birth,
    // or a raced first read — is presence without identity: it folds no row
    // rather than an anonymous `process` under `external`.
    let raced = PaneRef {
        command: None,
        cwd: None,
        ..pane("%1", "x", "/repo/main")
    };
    let snapshot = room(Vec::new(), Vec::new()).with_live_panes(vec![raced], None);

    let rows = rows(&snapshot);
    assert!(
        rows.is_empty(),
        "a command-less pane renders no row: {rows:?}"
    );
}

#[test]
fn spawn_only_unbound_pane_still_renders() {
    // Regression for Zellij topology/CLI source races: a frame with no
    // foreground command but a stable spawn command remains a known pane.
    let raced = PaneRef {
        command: None,
        spawn_command: Some("rimz agents exec codex --worktree-path /repo/main".to_owned()),
        cwd: None,
        ..pane("%1", "x", "/repo/main")
    };
    let snapshot = room(Vec::new(), Vec::new()).with_live_panes(vec![raced], None);

    let rows = rows(&snapshot);
    assert_eq!(
        rows.len(),
        1,
        "spawn identity keeps the row visible: {rows:?}"
    );
    assert_eq!(rows[0].name, "codex");
}

#[test]
fn commandless_pane_with_agent_still_renders_agent_row() {
    // Agent rows bind by stamped pane id, never by command, so a raced read
    // that drops the command never demotes or hides the agent's row.
    let claude = agent("claude", "sess-a", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    let raced = PaneRef {
        command: None,
        ..pane("%1", "claude", "/repo/main")
    };
    let snapshot = room(Vec::new(), vec![claude]).with_live_panes(vec![raced], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1, "the stamped agent row survives: {rows:?}");
    assert!(rows[0].is_agent());
}

#[test]
fn commandless_pane_does_not_form_empty_external_group() {
    // The raced read that drops a command usually drops the cwd too; the
    // filtered pane must not mint a stray `external` header on its way out.
    let root = "/repo/rimz";
    let raced = PaneRef {
        command: None,
        cwd: None,
        ..pane("%2", "x", "")
    };
    let snapshot = room(Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_live_panes(vec![pane("%1", "zsh", root), raced], None);

    assert_eq!(
        snapshot.worktree_groups.len(),
        1,
        "no external group for the filtered pane: {:?}",
        snapshot.worktree_groups,
    );
    assert_eq!(snapshot.worktree_groups[0].label, "rimz");
}

#[test]
fn commandless_pane_keeps_known_process_rows() {
    // The guard is per-pane: a sibling whose command read succeeded keeps
    // its named process row.
    let raced = PaneRef {
        command: None,
        ..pane("%2", "x", "/repo/main")
    };
    let snapshot = room(Vec::new(), Vec::new())
        .with_live_panes(vec![pane("%1", "zsh", "/repo/main"), raced], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1, "only the named pane is a row: {rows:?}");
    assert_eq!(rows[0].name, "zsh");
}

#[test]
fn duplicate_live_pane_ids_project_one_row_identity() {
    let claude = agent("claude", "sess-a", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    let duplicate = pane("%1", "claude", "/repo/main");
    let snapshot =
        room(Vec::new(), vec![claude]).with_live_panes(vec![duplicate.clone(), duplicate], None);

    let projected = rows(&snapshot);
    assert_eq!(projected.len(), 1, "duplicate pane ids fold once");
    assert_eq!(projected[0].id, "sess-a");
    assert_eq!(row_identity_violations(projected), Vec::<String>::new());
}

#[test]
fn duplicate_stamped_agent_identity_suppresses_second_pane() {
    let first = agent("claude", "sess-a", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    let duplicate = agent("claude", "sess-a", AgentStatus::Running, 999)
        .worktree("/repo/main")
        .in_pane("%2");
    let snapshot = room(Vec::new(), vec![first, duplicate]).with_live_panes(
        vec![
            pane("%1", "claude", "/repo/main"),
            pane("%2", "claude", "/repo/main"),
        ],
        None,
    );

    let projected = rows(&snapshot);
    assert_eq!(
        projected.len(),
        1,
        "the conflicting stamped pane is suppressed rather than rendered as process: {projected:?}"
    );
    assert_eq!(projected[0].id, "sess-a");
    assert_eq!(row_identity_violations(projected), Vec::<String>::new());
}

#[test]
fn newborn_unknown_cwd_pane_waits_one_frame_before_external() {
    let mut first =
        room(Vec::new(), Vec::new()).with_project_root(Some(PathBuf::from("/repo/main")));
    first.panes_produced_at_ms = Some(10);
    let newborn = PaneRef {
        cwd: None,
        first_seen_at_ms: Some(10),
        ..pane("%1", "zsh", "")
    };

    let first = first.with_live_panes(vec![newborn.clone()], None);
    assert!(
        first.worktree_groups.is_empty(),
        "newborn unknown-cwd pane is quarantined for its birth frame"
    );

    let mut second =
        room(Vec::new(), Vec::new()).with_project_root(Some(PathBuf::from("/repo/main")));
    second.panes_produced_at_ms = Some(11);
    let second = second.with_live_panes(vec![newborn], None);
    assert_eq!(second.worktree_groups.len(), 1);
    assert_eq!(
        second.worktree_groups[0].kind,
        SidebarWorktreeKind::External
    );
}

#[test]
fn legacy_frame_without_first_seen_does_not_quarantine() {
    let mut snapshot =
        room(Vec::new(), Vec::new()).with_project_root(Some(PathBuf::from("/repo/main")));
    snapshot.panes_produced_at_ms = Some(10);
    let legacy = PaneRef {
        cwd: None,
        first_seen_at_ms: None,
        ..pane("%1", "zsh", "")
    };

    let snapshot = snapshot.with_live_panes(vec![legacy], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    assert_eq!(
        snapshot.worktree_groups[0].kind,
        SidebarWorktreeKind::External
    );
}

#[test]
fn newborn_known_cwd_pane_paints_immediately() {
    let mut snapshot =
        room(Vec::new(), Vec::new()).with_project_root(Some(PathBuf::from("/repo/main")));
    snapshot.panes_produced_at_ms = Some(10);
    let newborn = PaneRef {
        first_seen_at_ms: Some(10),
        ..pane("%1", "zsh", "/repo/main")
    };

    let snapshot = snapshot.with_live_panes(vec![newborn], None);
    assert_eq!(snapshot.worktree_groups.len(), 1);
    assert_eq!(snapshot.worktree_groups[0].label, "main");
}
