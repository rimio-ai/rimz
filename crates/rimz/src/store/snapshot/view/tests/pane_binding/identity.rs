use super::*;

#[test]
fn commandless_panes_require_an_agent_or_spawn_identity() {
    let anonymous = PaneRef {
        command: None,
        cwd: None,
        ..pane("%1", "x", "/repo/main")
    };
    assert!(
        rows(&room(Vec::new(), Vec::new()).with_live_panes(vec![anonymous], None)).is_empty(),
        "presence without command, cwd, spawn, or agent identity folds no row"
    );

    let spawn_only = PaneRef {
        command: None,
        spawn_command: Some("rimz agents exec codex --worktree-path /repo/main".to_owned()),
        cwd: None,
        ..pane("%1", "x", "/repo/main")
    };
    let snapshot = room(Vec::new(), Vec::new()).with_live_panes(vec![spawn_only], None);
    assert_eq!(rows(&snapshot)[0].name, "codex");

    let claude = agent("claude", "sess-a", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    let commandless_agent_pane = PaneRef {
        command: None,
        ..pane("%1", "claude", "/repo/main")
    };
    let snapshot =
        room(Vec::new(), vec![claude]).with_live_panes(vec![commandless_agent_pane], None);
    assert!(
        rows(&snapshot)[0].is_agent(),
        "agent stamp binds by pane id"
    );

    let raced_sibling = PaneRef {
        command: None,
        ..pane("%2", "x", "/repo/main")
    };
    let snapshot = room(Vec::new(), Vec::new())
        .with_live_panes(vec![pane("%1", "zsh", "/repo/main"), raced_sibling], None);
    assert_eq!(rows(&snapshot)[0].name, "zsh", "guard is per-pane");

    let root = "/repo/rimz";
    let external_race = PaneRef {
        command: None,
        cwd: None,
        ..pane("%2", "x", "")
    };
    let snapshot = room(Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_live_panes(vec![pane("%1", "zsh", root), external_race], None);
    assert_eq!(snapshot.worktree_groups.len(), 1);
    assert_eq!(snapshot.worktree_groups[0].label, "rimz");
}

#[test]
fn duplicate_pane_or_agent_id_projects_one_row_identity() {
    let claude = agent("claude", "sess-a", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    let duplicate = pane("%1", "claude", "/repo/main");
    let snapshot =
        room(Vec::new(), vec![claude]).with_live_panes(vec![duplicate.clone(), duplicate], None);
    assert_single_clean_row(&snapshot, "duplicate pane ids fold once");

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
    assert_single_clean_row(
        &snapshot,
        "conflicting stamped pane suppresses the second pane",
    );
}

fn assert_single_clean_row(snapshot: &SidebarSnapshot, label: &str) {
    let projected = rows(snapshot);
    assert_eq!(projected.len(), 1, "{label}: {projected:?}");
    assert_eq!(projected[0].id, "sess-a", "{label}");
    assert_eq!(row_identity_violations(projected), Vec::<String>::new());
}

#[test]
fn newborn_unknown_cwd_quarantine_only_applies_to_the_birth_frame() {
    for (label, pane_ref, produced_at, expected) in [
        (
            "birth frame unknown cwd",
            PaneRef {
                cwd: None,
                first_seen_at_ms: Some(10),
                ..pane("%1", "zsh", "")
            },
            10,
            None,
        ),
        (
            "next frame unknown cwd",
            PaneRef {
                cwd: None,
                first_seen_at_ms: Some(10),
                ..pane("%1", "zsh", "")
            },
            11,
            Some(SidebarWorktreeKind::External),
        ),
        (
            "frameless fold",
            PaneRef {
                cwd: None,
                first_seen_at_ms: None,
                ..pane("%1", "zsh", "")
            },
            10,
            Some(SidebarWorktreeKind::External),
        ),
        (
            "known cwd",
            PaneRef {
                first_seen_at_ms: Some(10),
                ..pane("%1", "zsh", "/repo/main")
            },
            10,
            Some(SidebarWorktreeKind::Worktree),
        ),
    ] {
        let mut snapshot =
            room(Vec::new(), Vec::new()).with_project_root(Some(PathBuf::from("/repo/main")));
        snapshot.panes_produced_at_ms = Some(produced_at);
        let snapshot = snapshot.with_live_panes(vec![pane_ref], None);
        assert_eq!(
            snapshot.worktree_groups.first().map(|group| group.kind),
            expected,
            "{label}"
        );
    }
}
