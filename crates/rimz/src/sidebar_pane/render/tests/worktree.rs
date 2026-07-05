use super::*;

#[test]
fn render_worktree_attention_map() {
    let workspace = fixed_workspace();
    let mut native = FeedItem::new(
        workspace.clone(),
        Surface::NativeUi,
        FeedKind::Permission,
        "psql DROP TABLE invoices",
        "claude",
        "agent-hook",
    );
    native.worktree_path = Some("/home/me/query-engine".to_owned());
    native.updated_at = fixed_now() - Duration::from_secs(12 * 60);
    native.payload = json!({ "session_id": "claude-1" });
    let mut script = FeedItem::new(
        workspace,
        Surface::Script,
        FeedKind::Question,
        "Deploy staging?",
        "deploy.sh",
        "cli",
    );
    script.options = vec!["yes".to_owned(), "no".to_owned()];
    script.updated_at = fixed_now() - Duration::from_secs(5 * 60);
    let mut deploy_pane = pane("%deploy", "deploy.sh", "");
    deploy_pane.cwd = None;
    script.pane = Some(deploy_pane);
    let mut permission = agent(
        "claude-1",
        "claude",
        AgentStatus::Waiting,
        Some("/home/me/query-engine"),
        Some("main"),
        None,
    );
    permission.prompt = Some("investigate destructive query guard".to_owned());
    permission.last_activity = native.updated_at;
    let mut running = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/home/me/query-engine"),
        Some("main"),
        Some("add tests"),
    );
    running.model = Some("GPT-5.5".to_owned());
    running.effort = Some("high".to_owned());
    running.last_activity = fixed_now() - Duration::from_secs(8);

    let snapshot = snapshot_with(vec![native, script], vec![permission, running]);

    assert_snapshot(
        "worktree_attention_map",
        snapshot_to_screen(&snapshot, 38, 20),
    );
}
#[test]
fn render_directory_room_root_pod_is_name_only() {
    // A directory room: a git-backed row's resolved worktree keeps the full
    // `⑂` pod header with its git cluster, while the room's own pod renders
    // name-only — no fork glyph, no git story — and still anchors its rows.
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/srv/agents/query-engine"),
        Some("main"),
        Some("db migrate"),
    );
    let stamped = pane("%1", "claude", "/srv/agents/query-engine");
    claude.pane = Some(stamped.clone());
    let shell = pane("%2", "zsh", "/srv/agents");
    let mut snapshot = snapshot_with(Vec::new(), vec![claude])
        .with_root_class(crate::workspace::RootClass::Directory)
        .with_project_root(Some("/srv/agents".into()))
        .with_live_panes(vec![stamped, shell], None);
    let child = snapshot
        .worktree_groups
        .iter_mut()
        .find(|group| group.kind == crate::SidebarWorktreeKind::Worktree)
        .expect("the git-backed worktree pod");
    child.diff_added = Some(12);
    child.diff_removed = Some(3);
    child.commits_ahead = Some(2);

    let rendered = snapshot_to_screen(&snapshot, 44, 18);

    assert!(
        rendered.contains("⑂ main"),
        "the git-backed row keeps the fork-glyph pod header:\n{rendered}"
    );
    assert!(
        rendered.contains("agents") && !rendered.contains("⑂ agents"),
        "the room's own pod is name-only:\n{rendered}"
    );
    assert_snapshot("directory_room_root_pod", rendered);
}

#[test]
fn render_named_channel_header_uses_hash_glyph_and_bare_label() {
    let mut design = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("design API"),
    );
    design.channel = Some("design".to_owned());
    let rendered = snapshot_to_screen(&snapshot_with(Vec::new(), vec![design]), 36, 18);

    assert!(
        rendered.contains("# design"),
        "channel header uses the hash glyph and bare label:\n{rendered}"
    );
    assert!(
        !rendered.contains("##design"),
        "channel label must not include its address sigil twice:\n{rendered}"
    );
}

#[test]
fn render_worktree_channel_keeps_hash_label_and_git_cluster() {
    let mut design = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/worktrees/codex-resets"),
        Some("codex-resets"),
        Some("reset flow"),
    );
    design.channel = Some("codex-resets".to_owned());
    let mut snapshot = snapshot_with(Vec::new(), vec![design]);
    snapshot.worktree_groups[0].trunk = Some("main".to_owned());
    snapshot.worktree_groups[0].trunk_sync = Some(crate::WorktreeTrunkSync::Diverged);
    snapshot.worktree_groups[0].pr_state = Some(crate::WorktreePrState::Merged);

    let rendered = snapshot_to_screen(&snapshot, 44, 14);

    assert!(rendered.contains("# codex-resets"), "header:\n{rendered}");
    assert!(rendered.contains("✓ main"), "header:\n{rendered}");
    assert!(
        !rendered.contains("⑂ codex-resets"),
        "channel keeps hash identity:\n{rendered}"
    );
}

#[test]
fn render_worktree_equal_to_trunk() {
    // A worktree that IS the trunk tip — zero ahead, zero behind, zero diff,
    // and a proven-clean working tree — collapses the header's git cluster
    // to `≡ <trunk>`: this checkout is `main`, nothing of its own anywhere.
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Idle,
        Some("/home/me/query-engine-wt/feature-migration"),
        Some("feature-migration"),
        None,
    );
    codex.last_activity = fixed_now() - Duration::from_secs(30);
    let mut snapshot = snapshot_with(Vec::new(), vec![codex]);
    snapshot.worktree_groups[0].diff_added = Some(0);
    snapshot.worktree_groups[0].diff_removed = Some(0);
    snapshot.worktree_groups[0].commits_ahead = Some(0);
    snapshot.worktree_groups[0].commits_behind = Some(0);
    snapshot.worktree_groups[0].trunk = Some("main".to_owned());
    snapshot.worktree_groups[0].clean = Some(true);
    snapshot.worktree_groups[0].landed = Some(true);
    snapshot.worktree_groups[0].trunk_sync = Some(crate::WorktreeTrunkSync::Pristine);

    let rendered = snapshot_to_screen(&snapshot, 38, 14);

    assert!(rendered.contains("≡ main"), "header:\n{rendered}");
    assert!(
        !rendered.contains("+0 -0"),
        "the landed marker replaces the zero diff"
    );
}
#[test]
fn render_worktree_clear_safe_to_remove() {
    // A content-landed worktree with a clean status whose trunk has moved on
    // collapses to `✓ <trunk>`: done, safe to remove. Behind picks the marker,
    // never paints a `⇣` of its own.
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Idle,
        Some("/home/me/query-engine-wt/feature-migration"),
        Some("feature-migration"),
        None,
    );
    codex.last_activity = fixed_now() - Duration::from_secs(30);
    let mut snapshot = snapshot_with(Vec::new(), vec![codex]);
    snapshot.worktree_groups[0].diff_added = Some(0);
    snapshot.worktree_groups[0].diff_removed = Some(0);
    snapshot.worktree_groups[0].commits_ahead = Some(0);
    snapshot.worktree_groups[0].commits_behind = Some(5);
    snapshot.worktree_groups[0].trunk = Some("main".to_owned());
    snapshot.worktree_groups[0].clean = Some(true);
    snapshot.worktree_groups[0].landed = Some(true);
    snapshot.worktree_groups[0].trunk_sync = Some(crate::WorktreeTrunkSync::Merged);

    let rendered = snapshot_to_screen(&snapshot, 38, 14);

    assert!(rendered.contains("✓ main"), "header:\n{rendered}");
    assert!(
        !rendered.contains("≡"),
        "behind keeps the equal marker off:\n{rendered}"
    );
    assert!(
        !rendered.contains('⇣'),
        "behind stays out of the clear header"
    );
}

#[test]
fn render_content_landed_worktree_uses_marker_over_ancestry_delta() {
    // Content, not raw ancestry, drives the landed marker: a clean branch can
    // still be commits ahead of the trunk by ancestry after squash/rebase/merge
    // landings, and the header should call it removable instead of showing the
    // delta cluster.
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Idle,
        Some("/home/me/query-engine-wt/feature-migration"),
        Some("feature-migration"),
        None,
    );
    codex.last_activity = fixed_now() - Duration::from_secs(30);
    let mut snapshot = snapshot_with(Vec::new(), vec![codex]);
    snapshot.worktree_groups[0].diff_added = Some(14);
    snapshot.worktree_groups[0].diff_removed = Some(3);
    snapshot.worktree_groups[0].commits_ahead = Some(2);
    snapshot.worktree_groups[0].commits_behind = Some(5);
    snapshot.worktree_groups[0].trunk = Some("main".to_owned());
    snapshot.worktree_groups[0].clean = Some(true);
    snapshot.worktree_groups[0].landed = Some(true);
    snapshot.worktree_groups[0].trunk_sync = Some(crate::WorktreeTrunkSync::Merged);

    let rendered = snapshot_to_screen(&snapshot, 38, 14);

    assert!(rendered.contains("✓ main"), "header:\n{rendered}");
    assert!(
        !rendered.contains('⇡') && !rendered.contains("+14"),
        "the marker replaces ancestry and diff clusters:\n{rendered}"
    );
}

#[test]
fn render_worktree_dirty_tree_keeps_the_cluster() {
    // A dirty tree — here an untracked binary the line count can't see, so
    // every numeric column still reads zero — blocks both landed markers:
    // the header falls back to the plain cluster (`⇣5` is all that's left)
    // rather than calling an unremovable worktree done.
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Idle,
        Some("/home/me/query-engine-wt/feature-migration"),
        Some("feature-migration"),
        None,
    );
    codex.last_activity = fixed_now() - Duration::from_secs(30);
    let mut snapshot = snapshot_with(Vec::new(), vec![codex]);
    snapshot.worktree_groups[0].diff_added = Some(0);
    snapshot.worktree_groups[0].diff_removed = Some(0);
    snapshot.worktree_groups[0].commits_ahead = Some(0);
    snapshot.worktree_groups[0].commits_behind = Some(5);
    snapshot.worktree_groups[0].trunk = Some("main".to_owned());
    snapshot.worktree_groups[0].clean = Some(false);
    snapshot.worktree_groups[0].landed = Some(true);
    snapshot.worktree_groups[0].trunk_sync = Some(crate::WorktreeTrunkSync::Diverged);

    let rendered = snapshot_to_screen(&snapshot, 38, 14);

    assert!(
        !rendered.contains("≡") && !rendered.contains("✓ main"),
        "a dirty tree wears no landed marker:\n{rendered}"
    );
    assert!(rendered.contains("⇣5"), "header:\n{rendered}");
}
#[test]
fn render_trunk_worktree_skips_the_landed_marker() {
    // The trunk worktree is trivially "landed on itself," so the landed
    // markers would be noise there: a clean main-branch group with zero
    // stats keeps a bare header, and the markers stay reserved for a
    // removable feature worktree.
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Idle,
        Some("/home/me/query-engine"),
        Some("main"),
        None,
    );
    codex.last_activity = fixed_now() - Duration::from_secs(30);
    let mut snapshot = snapshot_with(Vec::new(), vec![codex]);
    snapshot.worktree_groups[0].diff_added = Some(0);
    snapshot.worktree_groups[0].diff_removed = Some(0);
    snapshot.worktree_groups[0].commits_ahead = Some(0);
    snapshot.worktree_groups[0].commits_behind = Some(0);
    snapshot.worktree_groups[0].trunk = Some("main".to_owned());
    snapshot.worktree_groups[0].clean = Some(true);
    snapshot.worktree_groups[0].landed = Some(true);
    snapshot.worktree_groups[0].trunk_sync = None;

    let rendered = snapshot_to_screen(&snapshot, 38, 14);

    assert!(
        !rendered.contains('≡') && !rendered.contains("✓ main"),
        "no landed marker on the trunk worktree:\n{rendered}"
    );
    assert!(rendered.contains("⑂ main"), "header:\n{rendered}");
}

#[test]
fn render_merged_worktree_uses_merge_glyph_on_left() {
    let codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Idle,
        Some("/home/me/query-engine-wt/feature-migration"),
        Some("feature-migration"),
        None,
    );
    let mut snapshot = snapshot_with(Vec::new(), vec![codex]);
    snapshot.worktree_groups[0].trunk = Some("main".to_owned());
    snapshot.worktree_groups[0].trunk_sync = Some(crate::WorktreeTrunkSync::Merged);

    let rendered = snapshot_to_screen(&snapshot, 38, 14);

    assert!(
        rendered.contains("⮌ feature-migration"),
        "header:\n{rendered}"
    );
    assert!(
        !rendered.contains("⑂ feature-migration"),
        "merged header swaps the branch glyph:\n{rendered}"
    );
}

#[test]
fn render_reconciling_worktree_keeps_stats_and_merge_queue_marker() {
    let codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Idle,
        Some("/home/me/query-engine-wt/feature-migration"),
        Some("feature-migration"),
        None,
    );
    let mut snapshot = snapshot_with(Vec::new(), vec![codex]);
    snapshot.worktree_groups[0].diff_added = Some(3);
    snapshot.worktree_groups[0].diff_removed = Some(1);
    snapshot.worktree_groups[0].commits_ahead = Some(1);
    snapshot.worktree_groups[0].commits_behind = Some(0);
    snapshot.worktree_groups[0].trunk = Some("main".to_owned());
    snapshot.worktree_groups[0].trunk_sync = Some(crate::WorktreeTrunkSync::Reconciling);

    let rendered = snapshot_to_screen(&snapshot, 48, 14);

    assert!(rendered.contains("⇡1"), "header:\n{rendered}");
    assert!(rendered.contains("+3 -1"), "header:\n{rendered}");
    assert!(rendered.contains("⟳ main"), "header:\n{rendered}");
}

#[test]
fn render_diverged_worktree_uses_pr_state_marker() {
    let codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Idle,
        Some("/home/me/query-engine-wt/feature-migration"),
        Some("feature-migration"),
        None,
    );
    let mut snapshot = snapshot_with(Vec::new(), vec![codex]);
    snapshot.worktree_groups[0].trunk = Some("main".to_owned());
    snapshot.worktree_groups[0].trunk_sync = Some(crate::WorktreeTrunkSync::Diverged);
    snapshot.worktree_groups[0].pr_state = Some(crate::WorktreePrState::Open);

    let rendered = snapshot_to_screen(&snapshot, 42, 14);
    assert!(rendered.contains("⊙ main"), "header:\n{rendered}");

    snapshot.worktree_groups[0].pr_state = Some(crate::WorktreePrState::Closed);
    let rendered = snapshot_to_screen(&snapshot, 42, 14);
    assert!(rendered.contains("✕ main"), "header:\n{rendered}");
}
/// The borderless repo header (dashboard L1): the workspace name behind `⌘`
/// on the left, then the project path pinned to the right edge of the same
/// line — no `⌂` glyph, the dim path opposite the name reads as a path.
#[test]
fn repo_header_shows_name_then_path() {
    let mut snapshot = snapshot_with(Vec::new(), Vec::new());
    snapshot.project_root = Some(std::path::PathBuf::from("/srv/code/query-engine"));
    let rendered = snapshot_to_screen(&snapshot, 44, 12);
    let first = rendered.lines().next().unwrap_or_default();
    let name_at = first.find("⌘ query-engine").expect("name on line 1");
    let path_at = first
        .find("/srv/code/query-engine")
        .expect("path on line 1");
    assert!(name_at < path_at, "name leads, path pins right: {first:?}");
    assert!(
        !rendered.contains('⌂'),
        "the ⌂ path glyph is gone:\n{rendered}"
    );
}
#[test]
fn home_abbreviation_collapses_only_a_home_prefix() {
    assert_eq!(
        abbreviate_under("/home/dev/code/query-engine", Some("/home/dev")),
        "~/code/query-engine"
    );
    assert_eq!(abbreviate_under("/home/dev", Some("/home/dev")), "~");
    // A path that merely shares a textual prefix is not under home.
    assert_eq!(
        abbreviate_under("/home/developer/x", Some("/home/dev")),
        "/home/developer/x"
    );
    // Outside home, or no home, passes through.
    assert_eq!(
        abbreviate_under("/srv/code", Some("/home/dev")),
        "/srv/code"
    );
    assert_eq!(abbreviate_under("/srv/code", None), "/srv/code");
}
