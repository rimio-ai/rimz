use super::*;
use crate::sidebar_pane::render::layout::{spans_width, text_width};
use crate::sidebar_pane::render::theme::Component;

fn pipeline_snapshot() -> SidebarSnapshot {
    let agents = ["planner", "coder"].map(|role| {
        let mut member = agent(
            role,
            "claude",
            AgentStatus::Running,
            Some("/repo/pipeline"),
            Some("pipeline"),
            None,
        );
        member.team = Some("forge".to_owned());
        member.role = Some(role.to_owned());
        member
    });
    let mut snapshot = snapshot_with(agents.into());
    snapshot.worktree_groups[0].pipeline = Some(crate::store::snapshot::SidebarPipeline {
        stages: ["Explore", "Plan", "Implement", "Review", "Ship"]
            .map(str::to_owned)
            .into(),
        stage: "Implement".to_owned(),
        owner: Some("coder".to_owned()),
        started_at: Some(fixed_now() - Duration::from_secs(2_832)),
        stage_started_at: Some(fixed_now() - Duration::from_secs(2_832)),
        stage_prior_secs: 0,
        visited: Default::default(),
        done_at: None,
    });
    snapshot
}

#[test]
fn render_pipeline_states() {
    for (name, stage, clock, nerd, width) in [
        ("pipeline_running", "Implement", true, false, 54),
        ("pipeline_done_unicode", "Done", true, false, 54),
        ("pipeline_done_nerd", "Done", true, true, 54),
        ("pipeline_undeclared", "Investigate", true, false, 54),
        ("pipeline_no_clock", "Implement", false, false, 54),
        ("pipeline_narrow", "Implement", true, false, 22),
    ] {
        let mut snapshot = pipeline_snapshot();
        let pipeline = snapshot.worktree_groups[0].pipeline.as_mut().unwrap();
        pipeline.stage = stage.to_owned();
        if stage == "Done" {
            pipeline.done_at = Some(fixed_now() - Duration::from_secs(12));
        }
        if !clock {
            pipeline.started_at = None;
            pipeline.stage_started_at = None;
        }
        if nerd {
            snapshot.theme.glyphs.set = Some("nerd_font".to_owned());
        }
        let rendered = snapshot_to_screen(&snapshot, width, 24);
        assert!(
            rendered.contains(if width == 22 { "Impleme…" } else { stage }),
            "{rendered}"
        );
        if width == 22 {
            assert!(!rendered.contains('◉'), "track drops whole: {rendered}");
            assert!(rendered.contains("47m / 47m"));
            assert!(!rendered.contains("forge"), "team drops first: {rendered}");
        }
        assert_snapshot(name, rendered);
    }
}

#[test]
fn render_pipeline_folded_stage() {
    let mut snapshot = pipeline_snapshot();
    let group = &mut snapshot.worktree_groups[0];
    group.finished = true;
    group.pipeline.as_mut().unwrap().stage = "Done".to_owned();
    for row in &mut group.rows {
        row.as_agent_mut().unwrap().status = AgentStatus::Success;
    }
    let rendered = snapshot_to_screen(&snapshot, 54, 20);
    assert!(rendered.contains("forge · Done"), "{rendered}");
    assert!(!rendered.contains('●'));
    let theme = Theme::fixed(false);
    assert!(
        group_lines_at_width(&snapshot, &theme, 0, 54)[0]
            .spans
            .iter()
            .all(|span| !span.content.contains("forge"))
    );
    assert_snapshot("pipeline_folded", rendered);
}

#[test]
fn pipeline_click_and_status_style_follow_visible_owner() {
    let mut snapshot = pipeline_snapshot();
    let theme = Theme::fixed(false);
    let cost_rolls = CostRolls::default();
    for owner in [Some("coder"), Some("absent"), None] {
        let group = &mut snapshot.worktree_groups[0];
        group.pipeline.as_mut().unwrap().owner = owner.map(str::to_owned);
        let owner_index = group
            .rows
            .iter()
            .position(|row| row.display_name() == "coder")
            .unwrap();
        group.rows[owner_index].as_agent_mut().unwrap().status = AgentStatus::Waiting;
        let ctx = test_row_ctx(&snapshot, &theme, 54, 0, 7, &cost_rolls);
        let block = worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None);
        assert_eq!(
            block.interactions.target_at(4, 1),
            Some(HitTarget::Row(owner_index))
        );
        assert_eq!(block.interactions.row_map()[1], None);
        let current = block.lines[1]
            .spans
            .iter()
            .find(|span| span.content == theme.glyph(crate::config::GlyphRole::PipelineCurrent))
            .unwrap();
        assert_eq!(
            current.style,
            if owner == Some("coder") {
                super::super::labels::status_style_at(&theme, AgentStatus::Waiting, 7)
            } else {
                theme.muted()
            }
        );
        let name = block.lines[1]
            .spans
            .iter()
            .find(|span| span.content == "Implement")
            .unwrap();
        assert_eq!(name.style, current.style);
    }
}

#[test]
fn pipeline_completion_styles_and_width_admission() {
    let mut snapshot = pipeline_snapshot();
    let theme = Theme::fixed(false);
    for stage in ["Implement", "Done"] {
        snapshot.worktree_groups[0].pipeline.as_mut().unwrap().stage = stage.to_owned();
        let lines = group_lines_at_width(&snapshot, &theme, 0, 54);
        let line = &lines[1];
        for role in [GlyphRole::PipelinePassed, GlyphRole::PipelineDone] {
            let spans = line
                .spans
                .iter()
                .filter(|span| span.content == theme.glyph(role));
            for span in spans {
                assert_eq!(
                    span.style,
                    theme.styled(Component::PipelinePassed, Modifier::empty())
                );
            }
        }
        assert!(
            line.spans
                .iter()
                .any(|span| span.content == theme.glyph(GlyphRole::PipelinePassed))
        );
        if stage == "Done" {
            assert!(
                line.spans
                    .iter()
                    .any(|span| span.content == theme.glyph(GlyphRole::PipelineDone))
            );
        }
        let team = line.spans.iter().find(|span| span.content == "forge · ");
        assert!(
            team.is_some(),
            "the team badge must lead with its value seam"
        );
        let team = team.unwrap();
        assert_eq!(
            team.style,
            theme.styled(Component::TeamLabel, Modifier::empty())
        );
    }
    let pipeline = snapshot.worktree_groups[0].pipeline.as_mut().unwrap();
    pipeline.stage = "Implement".to_owned();
    pipeline.stage_started_at = Some(fixed_now() - Duration::from_secs(7));
    for (width, expected) in [
        (39, "● ● ◉ Implement ○ ○"),
        (40, "forge · ● ● ◉ Implement ○ ○"),
        (41, "forge · ● ● ◉ Implement ○ ○"),
        (31, "Implement"),
        (32, "● ● ◉ Implement ○ ○"),
        (33, "● ● ◉ Implement ○ ○"),
        (21, "Impleme…"),
        (22, "Implement"),
        (23, "Implement"),
        // The clock needs two cells of name beside it (`I…`), never a bare `…`.
        (14, "Implement"),
        (15, "I…"),
        (16, "Im…"),
    ] {
        let lines = group_lines_at_width(&snapshot, &theme, 0, width);
        let text = lines[1]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains(expected), "{width}: {text}");
        assert_eq!(text.contains("forge"), width >= 40, "{width}: {text}");
        assert_eq!(text.contains('◉'), width >= 32, "{width}: {text}");
        assert_eq!(text.ends_with("7s / 47m🮇"), width >= 15, "{width}: {text}");
    }
    // An undeclared stage has no track to drop, so it keeps its badge at a
    // width where a tracked stage has lost its badge.
    snapshot.worktree_groups[0].pipeline.as_mut().unwrap().stage = "Investigate".to_owned();
    let lines = group_lines_at_width(&snapshot, &theme, 0, 32);
    let text = lines[1]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(text.contains("forge · Investigate"), "{text}");
    snapshot.worktree_groups[0].pipeline.as_mut().unwrap().stage = "Implement".to_owned();
    snapshot.worktree_groups[0].label = "pipeline/forge".to_owned();
    let lines = group_lines_at_width(&snapshot, &theme, 0, 54);
    assert!(
        lines[1]
            .spans
            .iter()
            .all(|span| !span.content.contains("forge"))
    );
}

#[test]
fn pipeline_revisited_future_dots_are_warm_and_hollow() {
    let mut snapshot = pipeline_snapshot();
    let theme = Theme::fixed(false);
    let warm = theme.styled(Component::PipelineRevisited, Modifier::empty());
    assert_ne!(warm, theme.muted());
    for (stage, visited, warm_count) in [
        ("Implement", vec!["Review"], 1),
        ("Implement", vec!["Explore", "Plan"], 0),
        ("Done", vec!["Review"], 0),
        ("Investigate", vec!["Review"], 0),
    ] {
        let pipeline = snapshot.worktree_groups[0].pipeline.as_mut().unwrap();
        pipeline.stage = stage.to_owned();
        pipeline.visited = visited.into_iter().map(str::to_owned).collect();
        let lines = group_lines_at_width(&snapshot, &theme, 0, 54);
        let spans = &lines[1].spans;
        assert_eq!(
            spans.iter().filter(|span| span.style == warm).count(),
            warm_count
        );
        for span in spans.iter().filter(|span| span.style == warm) {
            assert_eq!(span.content, theme.glyph(GlyphRole::PipelineFuture));
        }
        if stage == "Implement" {
            let future = spans
                .iter()
                .filter(|span| span.content == theme.glyph(GlyphRole::PipelineFuture))
                .collect::<Vec<_>>();
            assert_eq!(future.len(), 2);
            assert_eq!(
                future[0].style,
                if warm_count == 1 { warm } else { theme.muted() }
            );
            assert_eq!(future[1].style, theme.muted());
            for span in spans
                .iter()
                .filter(|span| span.content == theme.glyph(GlyphRole::PipelinePassed))
            {
                assert_eq!(
                    span.style,
                    theme.styled(Component::PipelinePassed, Modifier::empty())
                );
            }
            let current = spans
                .iter()
                .find(|span| span.content == theme.glyph(GlyphRole::PipelineCurrent))
                .unwrap();
            let name = spans
                .iter()
                .find(|span| span.content == "Implement")
                .unwrap();
            assert_eq!(current.style, name.style);
        }
    }
}

#[test]
fn pipeline_clocks_are_independent_and_pinned_right() {
    let mut snapshot = pipeline_snapshot();
    let theme = Theme::fixed(false);
    for (stage, prior, stage_secs, total_secs, expected) in [
        ("Implement", 1080, Some(7), Some(2832), "18m / 47m"),
        ("Implement", 0, Some(1080), Some(3720), "18m / 1h"),
        ("Implement", 0, None, Some(3720), "1h"),
        ("Implement", 0, Some(1080), None, "18m"),
        ("Done", 0, Some(1080), Some(3720), "1h"),
        ("Implement", 0, None, None, ""),
    ] {
        let pipeline = snapshot.worktree_groups[0].pipeline.as_mut().unwrap();
        pipeline.stage = stage.to_owned();
        pipeline.stage_prior_secs = prior;
        pipeline.stage_started_at = stage_secs.map(|s| fixed_now() - Duration::from_secs(s));
        pipeline.started_at = total_secs.map(|s| fixed_now() - Duration::from_secs(s));
        pipeline.done_at = Some(fixed_now());
        let lines = group_lines_at_width(&snapshot, &theme, 0, 54);
        let line = &lines[1];
        let text = line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert!(text.starts_with("▎  forge · "), "{text}");
        if expected.is_empty() {
            assert!(!text.contains('/'));
            continue;
        }
        assert!(text.contains(expected), "expected {expected} in {text}");
        let clock = line.spans.iter().find(|s| s.content == expected).unwrap();
        assert_eq!(clock.style, theme.muted());
        assert!(text.ends_with(&format!("{expected}🮇")), "{text}");
        assert_eq!(spans_width(&line.spans), 54);
    }
}

#[test]
fn pipeline_does_not_change_attention_or_animation() {
    let mut snapshot = pipeline_snapshot();
    // Isolate row rendering from the team badge's header/pipeline hand-off.
    snapshot.worktree_groups[0].team = None;
    snapshot.worktree_groups[0].rows[0].unread = true;
    snapshot.worktree_groups[0].rows[0]
        .as_agent_mut()
        .unwrap()
        .status = AgentStatus::Waiting;
    let cadence = animation_cadence_for_test(&snapshot);
    let unread = lead_unread(&snapshot.worktree_groups).map(|(id, status)| (id.to_owned(), status));
    let with = snapshot_to_screen(&snapshot, 54, 24);
    snapshot.worktree_groups[0].pipeline = None;
    assert_eq!(cadence, animation_cadence_for_test(&snapshot));
    assert_eq!(
        unread,
        lead_unread(&snapshot.worktree_groups).map(|(id, status)| (id.to_owned(), status))
    );
    let without = snapshot_to_screen(&snapshot, 54, 24);
    let without_pipeline = with
        .lines()
        .filter(|line| !line.contains("Implement") && !line.trim().is_empty())
        .collect::<Vec<_>>();
    assert_eq!(
        without_pipeline,
        without
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>()
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
    let mut snapshot = snapshot_with(vec![claude])
        .with_root_class(crate::workspace::RootClass::Directory)
        .with_project_root(Some("/srv/agents".into()))
        .with_live_panes(vec![stamped, shell], None);
    let child = snapshot
        .worktree_groups
        .iter_mut()
        .find(|group| group.kind == crate::store::snapshot::SidebarWorktreeKind::Worktree)
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
    group.trunk_sync = Some(crate::store::snapshot::WorktreeTrunkSync::Diverged);

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
    snapshot.worktree_groups[0].pipeline = pipeline_snapshot().worktree_groups[0].pipeline.clone();
    assert!(
        group_lines_at_width(&snapshot, &theme, 0, 48)[0]
            .spans
            .iter()
            .all(|span| !span.content.contains("forge"))
    );
}

#[test]
fn selecting_named_team_member_expands_every_teammate_only() {
    let mut planner = agent(
        "planner",
        "claude",
        AgentStatus::Running,
        Some("/repo/worktrees/team-expand"),
        Some("team-expand"),
        Some("plan"),
    );
    planner.team = Some("forge".to_owned());
    planner.role = Some("planner".to_owned());
    let mut coder = agent(
        "coder",
        "codex",
        AgentStatus::Running,
        Some("/repo/worktrees/team-expand"),
        Some("team-expand"),
        Some("implement"),
    );
    coder.team = Some("forge".to_owned());
    coder.role = Some("coder".to_owned());
    let mut stray = agent(
        "stray",
        "claude",
        AgentStatus::Running,
        Some("/repo/worktrees/team-expand"),
        Some("team-expand"),
        Some("observe"),
    );
    stray.role = Some("stray".to_owned());

    let mut plan_child = agent(
        "plan-child",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("research"),
    );
    plan_child.parent_agent_id = Some("planner".into());
    plan_child.subagent_description = Some("map the renderer".to_owned());
    let mut code_child = agent(
        "code-child",
        "codex",
        AgentStatus::Running,
        None,
        None,
        Some("tests"),
    );
    code_child.parent_agent_id = Some("coder".into());
    code_child.subagent_description = Some("pin the frame".to_owned());
    let mut stray_child = agent(
        "stray-child",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("audit"),
    );
    stray_child.parent_agent_id = Some("stray".into());
    stray_child.subagent_description = Some("stay collapsed".to_owned());

    let snapshot = snapshot_with(vec![
        planner,
        coder,
        stray,
        plan_child,
        code_child,
        stray_child,
    ]);
    let selected_index = snapshot.worktree_groups[0]
        .rows
        .iter()
        .position(|row| row.id == "planner")
        .expect("planner row");
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index,
            ..UiState::default()
        },
        54,
        32,
    );

    assert!(
        rendered.contains("map the renderer") && rendered.contains("pin the frame"),
        "both named teammates expand:\n{rendered}"
    );
    assert!(
        !rendered.contains("stay collapsed"),
        "a non-team card keeps its resting shape:\n{rendered}"
    );
    assert!(
        rendered
            .lines()
            .find(|line| line.contains("planner"))
            .is_some_and(|line| line.starts_with('▌')),
        "the selected teammate keeps the selected gutter:\n{rendered}"
    );
    assert!(
        rendered
            .lines()
            .find(|line| line.contains("coder"))
            .is_some_and(|line| line.starts_with('▎')),
        "the expanded teammate keeps the lane gutter:\n{rendered}"
    );
    assert_snapshot("named_team_selection_expands_teammates", rendered);
}

#[test]
fn selecting_inline_cohort_member_expands_only_that_card() {
    let mut first = agent(
        "first",
        "claude",
        AgentStatus::Running,
        Some("/repo/worktrees/inline"),
        Some("inline"),
        Some("first"),
    );
    first.launch_group = Some("inline-1".to_owned());
    let mut second = agent(
        "second",
        "codex",
        AgentStatus::Running,
        Some("/repo/worktrees/inline"),
        Some("inline"),
        Some("second"),
    );
    second.launch_group = Some("inline-1".to_owned());
    let mut first_child = agent(
        "first-child",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("first child"),
    );
    first_child.parent_agent_id = Some("first".into());
    first_child.subagent_description = Some("selected child".to_owned());
    let mut second_child = agent(
        "second-child",
        "codex",
        AgentStatus::Running,
        None,
        None,
        Some("second child"),
    );
    second_child.parent_agent_id = Some("second".into());
    second_child.subagent_description = Some("collapsed child".to_owned());

    let snapshot = snapshot_with(vec![first, second, first_child, second_child]);
    let selected_index = snapshot.worktree_groups[0]
        .rows
        .iter()
        .position(|row| row.id == "first")
        .expect("first row");
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index,
            ..UiState::default()
        },
        54,
        24,
    );

    assert!(rendered.contains("selected child"), "{rendered}");
    assert!(
        !rendered.contains("collapsed child"),
        "unnamed launch cohorts retain single-card expansion:\n{rendered}"
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
    snapshot.worktree_groups[0].trunk_sync =
        Some(crate::store::snapshot::WorktreeTrunkSync::Pristine);
    snapshot.worktree_groups[0].pr_state = Some(crate::store::snapshot::WorktreePrState::Merged);

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
    snapshot.worktree_groups[0].trunk_sync =
        Some(crate::store::snapshot::WorktreeTrunkSync::Diverged);
    snapshot.worktree_groups[0].pr_state = None;

    let rendered = snapshot_to_screen(&snapshot, 44, 14);

    assert!(
        rendered.contains("⑂ codex-resets"),
        "diverged worktree channel keeps fork identity:\n{rendered}"
    );
    assert!(rendered.contains("⑂ main"), "header:\n{rendered}");
}

fn pristine_worktree_with_pr_state(
    pr_state: Option<crate::store::snapshot::WorktreePrState>,
) -> SidebarSnapshot {
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
    snapshot.worktree_groups[0].trunk_sync =
        Some(crate::store::snapshot::WorktreeTrunkSync::Pristine);
    snapshot.worktree_groups[0].pr_state = pr_state;
    snapshot
}

#[test]
fn render_pr_badge_keeps_identity_style_across_states() {
    let theme = Theme::fixed(false);
    for pr_state in [
        None,
        Some(crate::store::snapshot::WorktreePrState::Open),
        Some(crate::store::snapshot::WorktreePrState::Merged),
        Some(crate::store::snapshot::WorktreePrState::Closed),
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
    let mut snapshot =
        pristine_worktree_with_pr_state(Some(crate::store::snapshot::WorktreePrState::Open));
    snapshot.worktree_groups[0].pr_number = Some(91);
    snapshot.worktree_groups[0].ci = Some(crate::store::snapshot::WorktreeCi::Failing);

    for state in [
        crate::store::snapshot::WorktreePrState::Open,
        crate::store::snapshot::WorktreePrState::Merged,
    ] {
        snapshot.worktree_groups[0].pr_state = Some(state);
        let lines = group_lines(&snapshot, &theme, 0);
        let ci = lines[0]
            .spans
            .iter()
            .find(|span| span.content.as_ref() == " ✕")
            .expect("PR CI span");
        assert_eq!(
            ci.style,
            theme.styled(Component::WorktreeCiFailing, Modifier::empty())
        );
    }
}

#[test]
fn render_branch_ci_without_a_pr_badge() {
    let theme = Theme::fixed(false);
    let mut snapshot = pristine_worktree_with_pr_state(None);
    snapshot.worktree_groups[0].ci = Some(crate::store::snapshot::WorktreeCi::Passing);

    let lines = group_lines(&snapshot, &theme, 0);
    let header = &lines[0];
    let ci = header
        .spans
        .iter()
        .find(|span| span.content.as_ref() == " ✓")
        .expect("branch CI span");

    assert_eq!(
        ci.style,
        theme.styled(Component::WorktreeCiPassing, Modifier::empty())
    );
    assert!(
        header.spans.iter().all(|span| !span.content.contains('#')),
        "a branch verdict carries no invented PR badge"
    );
}

#[test]
fn render_pr_badge_leads_with_ci_glyph() {
    let mut snapshot =
        pristine_worktree_with_pr_state(Some(crate::store::snapshot::WorktreePrState::Open));
    snapshot.worktree_groups[0].pr_number = Some(888);
    snapshot.worktree_groups[0].ci = Some(crate::store::snapshot::WorktreeCi::Passing);

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
    let mut linked =
        pristine_worktree_with_pr_state(Some(crate::store::snapshot::WorktreePrState::Open));
    linked.worktree_groups[0].pr_number = Some(91);
    linked.worktree_groups[0].ci = Some(crate::store::snapshot::WorktreeCi::Passing);
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
    let mut snapshot =
        pristine_worktree_with_pr_state(Some(crate::store::snapshot::WorktreePrState::Merged));
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
fn render_pr_badge_drops_before_ci_as_the_name_shortens() {
    let theme = Theme::fixed(false);
    let mut snapshot =
        pristine_worktree_with_pr_state(Some(crate::store::snapshot::WorktreePrState::Open));
    snapshot.worktree_groups[0].pr_number = Some(91);
    snapshot.worktree_groups[0].ci = Some(crate::store::snapshot::WorktreeCi::Passing);

    let admitted: Vec<_> = (1..=54)
        .map(|width| {
            let text = line_texts(&group_lines_at_width(&snapshot, &theme, 0, width))[0].clone();
            (text.contains('✓'), text.contains("#91"))
        })
        .collect();
    for (index, &(ci, badge)) in admitted.iter().enumerate() {
        assert!(!badge || ci, "badge without CI at width {}", index + 1);
        if index > 0 {
            let (previous_ci, previous_badge) = admitted[index - 1];
            assert!(!previous_ci || ci, "CI lost at width {}", index + 1);
            assert!(
                !previous_badge || badge,
                "badge lost at width {}",
                index + 1
            );
        }
    }
    let ci_threshold = admitted.iter().position(|&(ci, _)| ci).unwrap() + 1;
    let badge_threshold = admitted.iter().position(|&(_, badge)| badge).unwrap() + 1;
    assert_eq!((ci_threshold, badge_threshold), (12, 16));
    assert_eq!(badge_threshold - ci_threshold, text_width(" #91"));
    for width in [11, 12, 13, 15, 16, 17] {
        assert_eq!(
            admitted[width - 1],
            (width >= 12, width >= 16),
            "width {width}"
        );
    }
    let threshold = group_lines_at_width(&snapshot, &theme, 0, 16);
    let band = group_lines_at_width(&snapshot, &theme, 0, 15);
    assert_eq!(text_width(&threshold[0].spans[1].content), 1);
    assert_eq!(text_width(&band[0].spans[1].content), 4);
}

#[test]
fn render_pr_badge_link_and_header_fill_follow_admission() {
    let theme = Theme::fixed(false);
    let mut snapshot =
        pristine_worktree_with_pr_state(Some(crate::store::snapshot::WorktreePrState::Open));
    snapshot.worktree_groups[0].pr_number = Some(91);
    snapshot.worktree_groups[0].ci = Some(crate::store::snapshot::WorktreeCi::Passing);
    let url = "https://github.com/org/repo/pull/91";
    snapshot.worktree_groups[0].pr_url = Some(url.to_owned());
    let open = format!("\x1b]8;;{url}\x1b\\");
    for width in 11..=17 {
        for selected in [0, usize::MAX] {
            let lines = group_lines_at_width(&snapshot, &theme, selected, width);
            let header = &lines[0];
            let content = &header.spans[1..header.spans.len() - 1];
            assert_eq!(
                spans_width(content),
                width - 2,
                "width {width}, selected {selected}"
            );
            let text = line_texts(&lines)[0].clone();
            assert!(
                text.contains("⑃ main"),
                "right cluster stays intact: {text:?}"
            );
        }
        // The link range the renderer hands the painter is the decision point
        // for where the link lands: it covers the admitted `#91` exactly and
        // never the CI glyph beside it. Painting it is `paint_hyperlinks`' job,
        // and at these widths a wrapped line above the header offsets its rows,
        // so the bytes are asserted at a realistic width in
        // `render_pr_badge_is_a_diff_safe_sanitized_hyperlink`.
        let links = group_hyperlinks_at_width(&snapshot, &theme, 0, width);
        let bytes = snapshot_to_bytes_with_alert_and_ui(
            &snapshot,
            None,
            &UiState::default(),
            width as u16,
            14,
        );
        let raw = String::from_utf8_lossy(&bytes);
        if width < 16 {
            assert!(links.is_empty(), "no link at width {width}: {links:?}");
            assert!(!raw.contains(&open), "no link at width {width}: {raw:?}");
            continue;
        }
        let text = line_texts(&group_lines_at_width(&snapshot, &theme, 0, width))[0].clone();
        let hash = text.find('#').expect("admitted badge");
        let start = u16::try_from(text_width(&text[..hash])).unwrap();
        assert_eq!(
            links,
            vec![(
                0,
                start..start + u16::try_from(text_width("#91")).unwrap(),
                url.to_owned()
            )],
            "width {width}"
        );
        assert!(
            text[..hash].ends_with("✓ "),
            "the CI glyph stays outside the link: {text:?}"
        );
        assert!(raw.contains(&open), "admitted badge is linked: {raw:?}");
    }
}

#[test]
fn render_bare_branch_ci_yields_to_the_name_at_extreme_width() {
    let theme = Theme::fixed(false);
    let mut snapshot = pristine_worktree_with_pr_state(None);
    snapshot.worktree_groups[0].ci = Some(crate::store::snapshot::WorktreeCi::Passing);

    for width in [11, 12, 13] {
        let text = line_texts(&group_lines_at_width(&snapshot, &theme, 0, width))[0].clone();
        assert_eq!(text.contains('✓'), width >= 12, "width {width}: {text:?}");
        assert!(text.contains('…'), "the name keeps its slot: {text:?}");
    }
}

#[test]
fn render_bare_pr_number_yields_to_the_name_at_extreme_width() {
    let theme = Theme::fixed(false);
    let mut snapshot =
        pristine_worktree_with_pr_state(Some(crate::store::snapshot::WorktreePrState::Open));
    snapshot.worktree_groups[0].pr_number = Some(91);

    for width in [13, 14, 15] {
        let text = line_texts(&group_lines_at_width(&snapshot, &theme, 0, width))[0].clone();
        assert_eq!(text.contains("#91"), width >= 14, "width {width}: {text:?}");
        assert!(text.contains('…'), "the name keeps its slot: {text:?}");
    }
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
    let snapshot =
        pristine_worktree_with_pr_state(Some(crate::store::snapshot::WorktreePrState::Merged));

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
    let mut snapshot =
        pristine_worktree_with_pr_state(Some(crate::store::snapshot::WorktreePrState::Open));
    let rendered = snapshot_to_screen(&snapshot, 38, 14);
    assert!(rendered.contains("⑃ main"), "header:\n{rendered}");
    assert!(!rendered.contains("≡ main"), "header:\n{rendered}");

    snapshot.worktree_groups[0].pr_state = Some(crate::store::snapshot::WorktreePrState::Closed);
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
    snapshot.worktree_groups[0].trunk_sync =
        Some(crate::store::snapshot::WorktreeTrunkSync::Merged);

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
    group.trunk_sync = Some(crate::store::snapshot::WorktreeTrunkSync::Merged);
    group.pr_state = Some(crate::store::snapshot::WorktreePrState::Open);

    let rendered = snapshot_to_screen(&snapshot, 38, 14);
    assert!(
        rendered.contains("⑃ main"),
        "open PR outranks local merge:\n{rendered}"
    );
    assert!(
        !rendered.contains("✓ main"),
        "local merge marker is gone:\n{rendered}"
    );

    snapshot.worktree_groups[0].pr_state = Some(crate::store::snapshot::WorktreePrState::Closed);
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
    snapshot.worktree_groups[0].trunk_sync =
        Some(crate::store::snapshot::WorktreeTrunkSync::Merged);

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
    snapshot.worktree_groups[0].trunk_sync =
        Some(crate::store::snapshot::WorktreeTrunkSync::Diverged);

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
    group.pr_state = Some(crate::store::snapshot::WorktreePrState::Open);

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
    snapshot.worktree_groups[0].trunk_sync =
        Some(crate::store::snapshot::WorktreeTrunkSync::Merged);

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
    snapshot.worktree_groups[0].trunk_sync =
        Some(crate::store::snapshot::WorktreeTrunkSync::Reconciling);

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
    snapshot.worktree_groups[0].trunk_sync =
        Some(crate::store::snapshot::WorktreeTrunkSync::Diverged);
    snapshot.worktree_groups[0].commits_ahead = Some(2);
    snapshot.worktree_groups[0].pr_state = Some(crate::store::snapshot::WorktreePrState::Open);

    let rendered = snapshot_to_screen(&snapshot, 42, 14);
    assert!(rendered.contains("⇡2"), "header:\n{rendered}");
    assert!(rendered.contains("⑃ main"), "header:\n{rendered}");

    snapshot.worktree_groups[0].pr_state = Some(crate::store::snapshot::WorktreePrState::Closed);
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
    group.trunk_sync = Some(crate::store::snapshot::WorktreeTrunkSync::Diverged);
    group.commits_ahead = Some(2);
    group.commits_behind = Some(1);
    group.diff_added = Some(12);
    group.diff_removed = Some(3);
    group.pr_state = Some(crate::store::snapshot::WorktreePrState::Merged);

    let merged = snapshot_to_screen(&snapshot, 48, 14);
    assert!(merged.contains("✓ main"), "header:\n{merged}");
    assert!(
        !merged.contains('⇡') && !merged.contains("+12"),
        "a merged verdict leaves only its marker:\n{merged}"
    );

    snapshot.worktree_groups[0].pr_state = Some(crate::store::snapshot::WorktreePrState::Closed);
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
