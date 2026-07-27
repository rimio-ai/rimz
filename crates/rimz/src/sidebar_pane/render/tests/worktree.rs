use super::*;
use crate::sidebar_pane::render::theme::Component;

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
    let mut snapshot = snapshot_with(vec![claude])
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

    let rendered = snapshot_to_screen(&snapshot, 44, 20);

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
    let rendered = snapshot_to_screen(&snapshot_with(vec![design]), 36, 18);

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
fn render_active_team_header_tolerates_strays_and_yields_to_git_facts() {
    let mut planner = agent(
        "planner",
        "claude",
        AgentStatus::Running,
        Some("/repo/worktrees/feature-migration"),
        Some("feature-migration"),
        Some("design"),
    );
    planner.team = Some("forge".to_owned());
    planner.role = Some("planner".to_owned());
    let stray = agent(
        "stray",
        "codex",
        AgentStatus::Idle,
        Some("/repo/worktrees/feature-migration"),
        Some("feature-migration"),
        None,
    );
    let mut snapshot = snapshot_with(vec![planner, stray]);
    let group = &mut snapshot.worktree_groups[0];
    assert_eq!(group.team.as_deref(), Some("forge"));
    assert_eq!(
        serde_json::to_value(&*group).unwrap()["team"],
        "forge",
        "the projection carries team identity into snapshot JSON"
    );
    group.diff_added = Some(12);
    group.diff_removed = Some(3);
    group.commits_ahead = Some(2);
    group.trunk = Some("main".to_owned());
    group.trunk_sync = Some(crate::WorktreeTrunkSync::Diverged);

    let theme = Theme::fixed(false);
    let header = &group_lines_at_width(&snapshot, &theme, 0, 48)[0];
    let team = header
        .spans
        .iter()
        .find(|span| span.content.as_ref() == " · forge")
        .expect("active team label");
    assert_eq!(
        team.style,
        theme.styled(Component::TeamLabel, Modifier::empty())
    );

    let narrow = group_lines_at_width(&snapshot, &theme, 0, 32)[0]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(
        narrow.contains('…'),
        "team label clips with the name: {narrow}"
    );
    assert!(
        narrow.contains("⑂ main"),
        "git verdict stays pinned: {narrow}"
    );
}

#[test]
fn render_colliding_group_qualifiers_are_muted_and_ellipsize_with_the_label() {
    let mut snapshot = snapshot_with(vec![
        agent(
            "claude-1",
            "claude",
            AgentStatus::Running,
            Some("/workspace/rimz"),
            Some("main"),
            Some("design API"),
        ),
        agent(
            "codex-1",
            "codex",
            AgentStatus::Idle,
            Some("/home/me/.agents"),
            Some("main"),
            None,
        ),
    ]);
    for group in &mut snapshot.worktree_groups {
        group.label_qualifier = group.key.rsplit('/').next().map(ToOwned::to_owned);
    }

    let theme = Theme::fixed(false);
    let expected_suffix = format!(
        " · {}",
        snapshot.worktree_groups[0]
            .label_qualifier
            .as_deref()
            .expect("first group qualifier")
    );
    let header = &group_lines(&snapshot, &theme, 0)[0];
    let qualifier = header
        .spans
        .iter()
        .find(|span| span.content.as_ref() == expected_suffix)
        .expect("repo qualifier span");
    assert_eq!(
        qualifier.style,
        theme.styled(Component::WorktreeQualifier, Modifier::empty())
    );

    let rendered = snapshot_to_screen(&snapshot, 44, 30);
    assert!(
        rendered.contains("· rimz") && rendered.contains("· .agents"),
        "both colliding headers carry checkout context:\n{rendered}"
    );
    assert_snapshot("colliding_group_qualifiers", rendered);

    let narrow = snapshot_to_screen(&snapshot, 18, 30);
    assert!(
        narrow.contains('…'),
        "qualifiers ellipsize with their labels:\n{narrow}"
    );
    assert_snapshot("colliding_group_qualifiers_narrow", narrow);
}

#[test]
fn render_worktree_channel_leads_with_merge_glyph() {
    let mut design = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/worktrees/codex-resets"),
        Some("codex-resets"),
        Some("reset flow"),
    );
    design.channel = Some("codex-resets".to_owned());
    let mut snapshot = snapshot_with(vec![design]);
    snapshot.worktree_groups[0].worktree_backed = true;
    snapshot.worktree_groups[0].trunk = Some("main".to_owned());
    snapshot.worktree_groups[0].trunk_sync = Some(crate::WorktreeTrunkSync::Pristine);
    snapshot.worktree_groups[0].pr_state = Some(crate::WorktreePrState::Merged);

    let rendered = snapshot_to_screen(&snapshot, 44, 14);

    assert!(rendered.contains("⮌ codex-resets"), "header:\n{rendered}");
    assert!(rendered.contains("✓ main"), "header:\n{rendered}");
    assert!(
        !rendered.contains("≡ main"),
        "merged PR outranks pristine equal marker:\n{rendered}"
    );
}

#[test]
fn render_worktree_channel_uses_fork_glyph_before_git_facts() {
    let mut design = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/worktrees/codex-resets"),
        Some("codex-resets"),
        Some("reset flow"),
    );
    design.channel = Some("codex-resets".to_owned());
    let mut snapshot = snapshot_with(vec![design]);
    snapshot.worktree_groups[0].worktree_backed = true;

    let rendered = snapshot_to_screen(&snapshot, 44, 14);

    assert!(
        rendered.contains("⑂ codex-resets"),
        "worktree-backed channel keeps fork identity before git facts:\n{rendered}"
    );
    assert!(
        !rendered.contains("# codex-resets"),
        "worktree-backed channel must not flash as a plain lane:\n{rendered}"
    );
}

#[test]
fn render_worktree_channel_carries_pr_badge() {
    let mut design = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/worktrees/codex-resets"),
        Some("codex-resets"),
        Some("reset flow"),
    );
    design.channel = Some("codex-resets".to_owned());
    let mut snapshot = snapshot_with(vec![design]);
    snapshot.worktree_groups[0].worktree_backed = true;
    snapshot.worktree_groups[0].pr_number = Some(91);

    let rendered = snapshot_to_screen(&snapshot, 44, 14);

    assert!(
        rendered.contains("⑂ codex-resets #91"),
        "worktree-backed channel names its linked PR:\n{rendered}"
    );
}

#[test]
fn render_worktree_channel_leads_with_fork_glyph() {
    let mut design = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/worktrees/codex-resets"),
        Some("codex-resets"),
        Some("reset flow"),
    );
    design.channel = Some("codex-resets".to_owned());
    let mut snapshot = snapshot_with(vec![design]);
    snapshot.worktree_groups[0].worktree_backed = true;
    snapshot.worktree_groups[0].trunk = Some("main".to_owned());
    snapshot.worktree_groups[0].trunk_sync = Some(crate::WorktreeTrunkSync::Diverged);
    snapshot.worktree_groups[0].pr_state = None;

    let rendered = snapshot_to_screen(&snapshot, 44, 14);

    assert!(
        rendered.contains("⑂ codex-resets"),
        "diverged worktree channel keeps fork identity:\n{rendered}"
    );
    assert!(rendered.contains("⑂ main"), "header:\n{rendered}");
}

fn pristine_worktree_with_pr_state(pr_state: Option<crate::WorktreePrState>) -> SidebarSnapshot {
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Idle,
        Some("/home/me/query-engine-wt/feature-migration"),
        Some("feature-migration"),
        None,
    );
    codex.last_activity = fixed_now() - Duration::from_secs(30);
    let mut snapshot = snapshot_with(vec![codex]);
    snapshot.worktree_groups[0].diff_added = Some(0);
    snapshot.worktree_groups[0].diff_removed = Some(0);
    snapshot.worktree_groups[0].commits_ahead = Some(0);
    snapshot.worktree_groups[0].commits_behind = Some(0);
    snapshot.worktree_groups[0].trunk = Some("main".to_owned());
    snapshot.worktree_groups[0].clean = Some(true);
    snapshot.worktree_groups[0].landed = Some(true);
    snapshot.worktree_groups[0].trunk_sync = Some(crate::WorktreeTrunkSync::Pristine);
    snapshot.worktree_groups[0].pr_state = pr_state;
    snapshot
}

#[test]
fn render_pr_badge_keeps_identity_style_across_states() {
    let theme = Theme::fixed(false);
    for pr_state in [
        None,
        Some(crate::WorktreePrState::Open),
        Some(crate::WorktreePrState::Merged),
        Some(crate::WorktreePrState::Closed),
    ] {
        let mut snapshot = pristine_worktree_with_pr_state(pr_state);
        snapshot.worktree_groups[0].pr_number = Some(91);
        let lines = group_lines(&snapshot, &theme, 0);
        let header = &lines[0];
        let name = header
            .spans
            .iter()
            .find(|span| span.content.contains("feature-migration"))
            .expect("worktree name span");
        let badge = header
            .spans
            .iter()
            .find(|span| span.content.as_ref() == " #91")
            .expect("PR badge span");

        assert!(name.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(
            badge.style,
            theme.styled(Component::WorktreePrBadge, Modifier::empty())
        );
        assert!(!badge.style.add_modifier.contains(Modifier::BOLD));
    }
}

#[test]
fn render_open_and_merged_pr_badges_carry_ci_glyph_and_tone() {
    let theme = Theme::fixed(false);
    let mut snapshot = pristine_worktree_with_pr_state(Some(crate::WorktreePrState::Open));
    snapshot.worktree_groups[0].pr_number = Some(91);
    snapshot.worktree_groups[0].pr_ci = Some(crate::WorktreePrCi::Failing);

    for state in [crate::WorktreePrState::Open, crate::WorktreePrState::Merged] {
        snapshot.worktree_groups[0].pr_state = Some(state);
        let lines = group_lines(&snapshot, &theme, 0);
        let ci = lines[0]
            .spans
            .iter()
            .find(|span| span.content.as_ref() == " ✕")
            .expect("PR CI span");
        assert_eq!(
            ci.style,
            theme.styled(Component::PrCiFailing, Modifier::empty())
        );
    }

    snapshot.worktree_groups[0].pr_state = Some(crate::WorktreePrState::Closed);
    let lines = group_lines(&snapshot, &theme, 0);
    assert!(
        lines[0]
            .spans
            .iter()
            .all(|span| span.content.as_ref() != " ✕"),
        "stale CI stays off a closed PR badge"
    );
}

#[test]
fn render_branch_ci_without_a_pr_badge() {
    let theme = Theme::fixed(false);
    let mut snapshot = pristine_worktree_with_pr_state(None);
    snapshot.worktree_groups[0].pr_ci = Some(crate::WorktreePrCi::Passing);

    let lines = group_lines(&snapshot, &theme, 0);
    let header = &lines[0];
    let ci = header
        .spans
        .iter()
        .find(|span| span.content.as_ref() == " ✓")
        .expect("branch CI span");

    assert_eq!(
        ci.style,
        theme.styled(Component::PrCiPassing, Modifier::empty())
    );
    assert!(
        header.spans.iter().all(|span| !span.content.contains('#')),
        "a branch verdict carries no invented PR badge"
    );
}

#[test]
fn render_pr_badge_leads_with_ci_glyph() {
    let mut snapshot = pristine_worktree_with_pr_state(Some(crate::WorktreePrState::Open));
    snapshot.worktree_groups[0].pr_number = Some(888);
    snapshot.worktree_groups[0].pr_ci = Some(crate::WorktreePrCi::Passing);

    let rendered = snapshot_to_screen(&snapshot, 44, 14);

    assert!(
        rendered.contains("✓ #888"),
        "CI leads the PR badge:\n{rendered}"
    );
    assert!(
        !rendered.contains("#888 ✓"),
        "PR number no longer leads CI:\n{rendered}"
    );
}

#[test]
fn render_pr_badge_is_a_diff_safe_sanitized_hyperlink() {
    let mut linked = pristine_worktree_with_pr_state(Some(crate::WorktreePrState::Open));
    linked.worktree_groups[0].pr_number = Some(91);
    linked.worktree_groups[0].pr_ci = Some(crate::WorktreePrCi::Passing);
    let unsafe_url = "https://github.com/org/repo/pull/91\x1b]8;;https://evil.test\u{7}";
    linked.worktree_groups[0].pr_url = Some(unsafe_url.to_owned());
    let mut plain = linked.clone();
    plain.worktree_groups[0].pr_url = None;

    assert_eq!(
        snapshot_to_screen(&linked, 44, 14),
        snapshot_to_screen(&plain, 44, 14),
        "OSC 8 metadata leaves the badge layout unchanged"
    );

    let bytes = snapshot_to_bytes_with_alert_and_ui(&linked, None, &UiState::default(), 44, 14);
    let raw = String::from_utf8_lossy(&bytes);
    let url = crate::osc::osc_text(unsafe_url);
    let open_hash = format!("\x1b]8;;{url}\x1b\\#");
    assert!(
        raw.contains(&open_hash),
        "raw render has no linked #: {raw:?}"
    );
    assert!(
        raw.contains("\x1b]8;;\x1b\\"),
        "raw render has no OSC 8 close: {raw:?}"
    );
    assert!(
        !raw.contains("\x1b]8;;https://evil.test"),
        "control bytes cannot inject a second hyperlink: {raw:?}"
    );
    assert!(
        !raw.contains(&format!("\x1b]8;;{url}\x1b\\✓")),
        "the adjacent CI glyph stays outside the PR link"
    );
    assert!(
        !raw.contains(&format!("\x1b]8;;{url}\x1b\\ ")),
        "the badge's leading space stays outside the PR link"
    );
}

#[test]
fn render_finished_header_dims_the_label_while_live_header_stays_full_tone() {
    let theme = Theme::fixed(false);
    let mut snapshot = pristine_worktree_with_pr_state(Some(crate::WorktreePrState::Merged));
    snapshot.worktree_groups[0].finished = true;

    let finished = group_lines(&snapshot, &theme, 0);
    let finished_label = finished[0]
        .spans
        .iter()
        .find(|span| span.content.contains("feature-migration"))
        .expect("finished worktree label");
    assert_eq!(
        finished_label.style,
        theme.muted().add_modifier(Modifier::BOLD)
    );

    snapshot.worktree_groups[0].finished = false;
    let live = group_lines(&snapshot, &theme, 0);
    let live_label = live[0]
        .spans
        .iter()
        .find(|span| span.content.contains("feature-migration"))
        .expect("live worktree label");
    assert_eq!(
        live_label.style,
        theme.styled(Component::WorktreeHeader, Modifier::BOLD)
    );
}

#[test]
fn render_pr_badge_yields_to_the_name_at_extreme_width() {
    let theme = Theme::fixed(false);
    let mut snapshot = pristine_worktree_with_pr_state(Some(crate::WorktreePrState::Open));
    snapshot.worktree_groups[0].pr_number = Some(91);
    snapshot.worktree_groups[0].pr_ci = Some(crate::WorktreePrCi::Passing);

    let header = &group_lines_at_width(&snapshot, &theme, 0, 7)[0];
    let text = header
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();

    assert!(!text.contains("#91"), "badge drops first: {text:?}");
    assert!(!text.contains('✓'), "CI drops with its badge: {text:?}");
    assert!(
        text.contains('…'),
        "the clipped name keeps the label slot: {text:?}"
    );
}

#[test]
fn render_bare_branch_ci_yields_to_the_name_at_extreme_width() {
    let theme = Theme::fixed(false);
    let mut snapshot = pristine_worktree_with_pr_state(None);
    snapshot.worktree_groups[0].pr_ci = Some(crate::WorktreePrCi::Passing);

    let header = &group_lines_at_width(&snapshot, &theme, 0, 7)[0];
    let text = header
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();

    assert!(!text.contains('✓'), "branch CI drops first: {text:?}");
    assert!(
        text.contains('…'),
        "the clipped name keeps the label slot: {text:?}"
    );
}

#[test]
fn render_worktree_equal_to_trunk() {
    // A worktree that IS the trunk tip — zero ahead, zero behind, zero diff,
    // and a proven-clean working tree — collapses the header's git cluster
    // to `≡ <trunk>`: this checkout is `main`, nothing of its own anywhere.
    let snapshot = pristine_worktree_with_pr_state(None);

    let rendered = snapshot_to_screen(&snapshot, 38, 14);

    assert!(rendered.contains("≡ main"), "header:\n{rendered}");
    assert!(
        rendered.contains("⑂ feature-migration"),
        "PR-less pristine branch keeps branch prefix:\n{rendered}"
    );
    assert!(
        !rendered.contains("+0 -0"),
        "the landed marker replaces the zero diff"
    );
}

#[test]
fn render_pr_merged_pristine_worktree_uses_merge_glyphs() {
    let snapshot = pristine_worktree_with_pr_state(Some(crate::WorktreePrState::Merged));

    let rendered = snapshot_to_screen(&snapshot, 38, 14);

    assert!(
        rendered.contains("⮌ feature-migration"),
        "merged PR swaps the left prefix:\n{rendered}"
    );
    assert!(rendered.contains("✓ main"), "header:\n{rendered}");
    assert!(
        !rendered.contains("≡ main"),
        "merged PR outranks pristine equal marker:\n{rendered}"
    );
}

#[test]
fn render_pristine_worktree_pr_state_outranks_equal_marker() {
    let mut snapshot = pristine_worktree_with_pr_state(Some(crate::WorktreePrState::Open));
    let rendered = snapshot_to_screen(&snapshot, 38, 14);
    assert!(rendered.contains("⑃ main"), "header:\n{rendered}");
    assert!(!rendered.contains("≡ main"), "header:\n{rendered}");

    snapshot.worktree_groups[0].pr_state = Some(crate::WorktreePrState::Closed);
    let rendered = snapshot_to_screen(&snapshot, 38, 14);
    assert!(rendered.contains("✕ main"), "header:\n{rendered}");
    assert!(!rendered.contains("≡ main"), "header:\n{rendered}");
}

#[test]
fn render_worktree_clear_removable() {
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
    let mut snapshot = snapshot_with(vec![codex]);
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
fn render_merged_worktree_pr_open_or_closed_outranks_merge_marker() {
    let codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Idle,
        Some("/home/me/query-engine-wt/feature-migration"),
        Some("feature-migration"),
        None,
    );
    let mut snapshot = snapshot_with(vec![codex]);
    let group = &mut snapshot.worktree_groups[0];
    group.trunk = Some("main".to_owned());
    group.clean = Some(true);
    group.landed = Some(true);
    group.trunk_sync = Some(crate::WorktreeTrunkSync::Merged);
    group.pr_state = Some(crate::WorktreePrState::Open);

    let rendered = snapshot_to_screen(&snapshot, 38, 14);
    assert!(
        rendered.contains("⑃ main"),
        "open PR outranks local merge:\n{rendered}"
    );
    assert!(
        !rendered.contains("✓ main"),
        "local merge marker is gone:\n{rendered}"
    );

    snapshot.worktree_groups[0].pr_state = Some(crate::WorktreePrState::Closed);
    let rendered = snapshot_to_screen(&snapshot, 38, 14);
    assert!(
        rendered.contains("✕ main"),
        "closed PR outranks local merge:\n{rendered}"
    );
    assert!(
        !rendered.contains("✓ main"),
        "local merge marker is gone:\n{rendered}"
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
    let mut snapshot = snapshot_with(vec![codex]);
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
    let mut snapshot = snapshot_with(vec![codex]);
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
    let mut snapshot = snapshot_with(vec![codex]);
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
fn render_trunk_worktree_pr_state_keeps_plain_cluster() {
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Idle,
        Some("/home/me/query-engine"),
        Some("main"),
        None,
    );
    codex.last_activity = fixed_now() - Duration::from_secs(30);
    let mut snapshot = snapshot_with(vec![codex]);
    let group = &mut snapshot.worktree_groups[0];
    group.diff_added = Some(3);
    group.diff_removed = Some(1);
    group.commits_ahead = Some(2);
    group.trunk = Some("main".to_owned());
    group.trunk_sync = None;
    group.pr_state = Some(crate::WorktreePrState::Open);

    let rendered = snapshot_to_screen(&snapshot, 42, 14);

    assert!(rendered.contains("⇡2"), "header:\n{rendered}");
    assert!(rendered.contains("+3 -1"), "header:\n{rendered}");
    assert!(
        !rendered.contains("⑃ main"),
        "trunk worktree keeps the plain cluster:\n{rendered}"
    );
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
    let mut snapshot = snapshot_with(vec![codex]);
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
    let mut snapshot = snapshot_with(vec![codex]);
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
    let mut snapshot = snapshot_with(vec![codex]);
    snapshot.worktree_groups[0].trunk = Some("main".to_owned());
    snapshot.worktree_groups[0].trunk_sync = Some(crate::WorktreeTrunkSync::Diverged);
    snapshot.worktree_groups[0].commits_ahead = Some(2);
    snapshot.worktree_groups[0].pr_state = Some(crate::WorktreePrState::Open);

    let rendered = snapshot_to_screen(&snapshot, 42, 14);
    assert!(rendered.contains("⇡2"), "header:\n{rendered}");
    assert!(rendered.contains("⑃ main"), "header:\n{rendered}");

    snapshot.worktree_groups[0].pr_state = Some(crate::WorktreePrState::Closed);
    let rendered = snapshot_to_screen(&snapshot, 42, 14);
    assert!(rendered.contains("✕ main"), "header:\n{rendered}");
}

#[test]
fn render_diverged_merged_pr_drops_spent_stats_but_closed_keeps_them() {
    let codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Idle,
        Some("/home/me/query-engine-wt/feature-migration"),
        Some("feature-migration"),
        None,
    );
    let mut snapshot = snapshot_with(vec![codex]);
    let group = &mut snapshot.worktree_groups[0];
    group.trunk = Some("main".to_owned());
    group.trunk_sync = Some(crate::WorktreeTrunkSync::Diverged);
    group.commits_ahead = Some(2);
    group.commits_behind = Some(1);
    group.diff_added = Some(12);
    group.diff_removed = Some(3);
    group.pr_state = Some(crate::WorktreePrState::Merged);

    let merged = snapshot_to_screen(&snapshot, 48, 14);
    assert!(merged.contains("✓ main"), "header:\n{merged}");
    assert!(
        !merged.contains('⇡') && !merged.contains("+12"),
        "a merged verdict leaves only its marker:\n{merged}"
    );

    snapshot.worktree_groups[0].pr_state = Some(crate::WorktreePrState::Closed);
    let closed = snapshot_to_screen(&snapshot, 48, 14);
    assert!(closed.contains("✕ main"), "header:\n{closed}");
    assert!(closed.contains("⇡2"), "header:\n{closed}");
    assert!(closed.contains("+12 -3"), "header:\n{closed}");
}
/// The borderless repo header (dashboard L1): the workspace name behind `⌘`
/// on the left, then the project path pinned to the right edge of the same
/// line — no `⌂` glyph, the dim path opposite the name reads as a path.
#[test]
fn repo_header_shows_name_then_path() {
    let mut snapshot = snapshot_with(Vec::new());
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
