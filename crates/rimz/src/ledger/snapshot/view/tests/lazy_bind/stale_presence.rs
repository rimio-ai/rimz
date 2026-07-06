use super::*;

#[test]
fn two_paneless_codex_in_one_worktree_bind_most_recent() {
    // When two pane-less Codex sessions claim one worktree — a lingering
    // closed session and a live one — the most-recently-active binds the
    // single live pane; the stale session does not render.
    let snapshot = room(
        Vec::new(),
        vec![
            paneless_codex("sess-old", "/repo/main", 1_000),
            paneless_codex("sess-new", "/repo/main", 2_000),
        ],
    )
    .with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_agent());
    assert_eq!(rows[0].id, "sess-new");
}

#[test]
fn paneless_codex_and_new_stamped_codex_share_one_worktree_without_idle_row() {
    // Daemon-routed Codex can first bind one session by cwd, then recover a
    // newer session's focused pane at hook ingestion. The older paneless
    // session must survive long enough to bind the other same-cwd pane.
    let newer = paneless_codex("sess-new", "/repo/main", 2_000).in_pane("%2");
    let snapshot = room(
        Vec::new(),
        vec![paneless_codex("sess-old", "/repo/main", 1_000), newer],
    )
    .with_live_panes(
        vec![
            pane("%1", "codex", "/repo/main"),
            pane("%2", "codex", "/repo/main"),
        ],
        None,
    );

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.is_agent()));
    let old = rows
        .iter()
        .find(|row| row.id == "sess-old")
        .expect("older session renders");
    let new = rows
        .iter()
        .find(|row| row.id == "sess-new")
        .expect("newer session renders");
    assert_eq!(old.pane.as_ref().unwrap().pane_id.raw(), "%1");
    assert_eq!(new.pane.as_ref().unwrap().pane_id.raw(), "%2");
}

#[test]
fn resumed_codex_pane_binds_the_matching_session_and_heals_stale_stamp() {
    let mut old = paneless_codex("sess-old", "/repo/main", 1_000);
    old.registered_at = Some(ago(1_000));
    old.last_activity = ago(1_000);
    let newer = paneless_codex("sess-new", "/repo/main", 2_000).active_ago(-1);
    let resumed_pane = PaneRef {
        command: Some("codex resume sess-old".to_owned()),
        pane_process_start: Some(epoch()),
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        resumed_session_id: Some("sess-old".into()),
        ..pane("term1", "codex", "/repo/main")
    };

    let snapshot = room(Vec::new(), vec![newer, old]).with_live_panes(vec![resumed_pane], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "sess-old");

    let mut stale_stamp = pane("term1", "codex", "/repo/main");
    stale_stamp.pane_process_start = Some(ago(1_000));
    let mut old = paneless_codex("sess-old", "/repo/main", 1_000);
    old.registered_at = Some(ago(1_000));
    old.last_activity = ago(900);
    old.pane = Some(stale_stamp);
    let resumed_pane = PaneRef {
        command: Some("codex".to_owned()),
        pane_process_start: Some(ago(1)),
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        resumed_session_id: Some("sess-old".into()),
        ..pane("term1", "codex", "/repo/main")
    };

    let snapshot = room(Vec::new(), vec![old]).with_live_panes(vec![resumed_pane], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "sess-old");
    assert_eq!(
        rows[0].pane.as_ref().unwrap().pane_process_start,
        Some(ago(1))
    );
}

#[test]
fn paneless_codex_sessions_pair_by_latest_process_start_before_first_event() {
    let mut older = paneless_codex("sess-old", "/repo/main", 1_000);
    older.registered_at = Some(ago(3_000));
    older.last_activity = ago(2_000);
    let mut newer = paneless_codex("sess-new", "/repo/main", 2_000);
    newer.registered_at = Some(ago(8));
    newer.last_activity = ago(1);
    let old_pane = PaneRef {
        pane_process_start: Some(ago(3_600)),
        ..pane("terminal_4", "codex", "/repo/main")
    };
    let new_pane = PaneRef {
        pane_process_start: Some(ago(9)),
        ..pane("terminal_58", "codex", "/repo/main")
    };

    for (agents, panes) in [
        (
            vec![older.clone(), newer.clone()],
            vec![old_pane.clone(), new_pane.clone()],
        ),
        (vec![newer, older], vec![new_pane, old_pane]),
    ] {
        let snapshot = room(Vec::new(), agents).with_live_panes(panes, None);
        assert_eq!(
            row(&snapshot, "sess-old")
                .pane
                .as_ref()
                .unwrap()
                .pane_id
                .raw(),
            "terminal_4"
        );
        assert_eq!(
            row(&snapshot, "sess-new")
                .pane
                .as_ref()
                .unwrap()
                .pane_id
                .raw(),
            "terminal_58"
        );
    }
}
