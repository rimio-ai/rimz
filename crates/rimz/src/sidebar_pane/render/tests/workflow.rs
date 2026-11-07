use super::*;
use crate::sidebar_pane::render::theme::Component;
use crate::store::snapshot::{SidebarWorkflow, WorkflowNodeKind};

fn snapshot() -> SidebarSnapshot {
    crate::sidebar::scenario::Scenario::parse("workflow", "preset workflow")
        .unwrap()
        .at(Duration::ZERO)
}

fn run(snapshot: &mut SidebarSnapshot) -> &mut SidebarWorkflow {
    std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0]
        .workflow
        .as_mut()
        .unwrap()
}

fn move_current(run: &mut SidebarWorkflow, name: &str) {
    run.current.name = name.to_owned();
    run.current.reached = true;
    if let Some(position) = run.track.iter().position(|node| node.name == name) {
        for (index, node) in run.track.iter_mut().enumerate() {
            node.reached = index <= position;
        }
    }
}

#[test]
fn workflow_marks_history_instead_of_nodes_before_the_current_position() {
    for set in ["unicode", "nerd-font"] {
        let mut snapshot = snapshot();
        let run = run(&mut snapshot);
        move_current(run, "triage");
        run.track[1].reached = false;
        run.track[2].reached = false;
        snapshot.theme.glyphs.set = Some(set.to_owned());
        let theme = Theme::fixed_for_theme(false, &snapshot.theme);
        let lines = group_lines_at_width(&snapshot, &theme, 0, 96);
        let strip = &lines[1];
        let text = line_texts(&lines)[1].clone();
        for (glyph, count, style) in [
            (
                '●',
                1,
                theme.styled(Component::PipelinePassed, Modifier::empty()),
            ),
            (
                '◆',
                1,
                theme.styled(Component::PipelinePassed, Modifier::empty()),
            ),
            ('○', 1, theme.muted()),
            ('◇', 2, theme.muted()),
        ] {
            assert_eq!(text.matches(glyph).count(), count, "{set}: {text}");
            for span in strip
                .spans
                .iter()
                .filter(|span| span.content == glyph.to_string())
            {
                assert_eq!(span.style, style, "{set}: {glyph}");
            }
        }
        assert_eq!(text.matches('◉').count(), 1, "{set}: {text}");
    }
}

fn snapshot_to_screen_for_theme(
    snapshot: &SidebarSnapshot,
    no_color: bool,
    width: u16,
    height: u16,
) -> String {
    let pane_ui = UiState {
        theme_cache: Some((
            snapshot.theme.clone(),
            Rc::new(Theme::fixed_for_theme(no_color, &snapshot.theme)),
        )),
        ..Default::default()
    };
    let ui = ViewState {
        inactive_peek: Some(Default::default()),
        ..Default::default()
    };
    snapshot_to_screen_with_alert_and_ui(snapshot, None, &ui, &pane_ui, width, height)
}

#[test]
fn rowless_workflow_header_drops_churn_before_its_name() {
    let mut snapshot = snapshot();
    let group = &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0];
    group.trunk_sync = Some(crate::store::snapshot::WorktreeTrunkSync::Diverged);
    group.diff_added = Some(1234);
    group.diff_removed = Some(567);
    group.commits_ahead = Some(3);
    group.commits_behind = Some(1);
    let theme = Theme::fixed(false);
    let text = line_texts(&group_lines_at_width(&snapshot, &theme, 0, 42))[0].clone();
    assert!(text.contains("merge-lane"), "{text}");
    assert!(!text.contains("+1234"), "churn yields to the name: {text}");
    let costs = CostRolls::default();
    let ctx = test_row_ctx(&snapshot, &theme, 42, 0, 0, &costs);
    let block = worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None);
    assert_eq!(block.interactions.target_at(3, 0), None);
    assert!(
        !text.contains('▎'),
        "a rowless lane is never selected: {text}"
    );
    let shapes = [47, 46, 45, 35, 34, 33].map(|width| {
        let text = line_texts(&group_lines_at_width(&snapshot, &theme, 0, width))[0].clone();
        (
            text.contains("merge-lane"),
            text.contains("+1234"),
            text.contains("⇡3"),
        )
    });
    assert_eq!(
        shapes,
        [
            (true, true, true),
            (true, true, true),
            (true, false, true),
            (true, false, true),
            (true, false, true),
            (true, false, false)
        ]
    );
}

#[test]
fn workflow_narrow_trim_boundaries_keep_the_current_name_whole() {
    let mut snapshot = snapshot();
    let run = run(&mut snapshot);
    move_current(run, "fix");
    run.detour_parent = Some("watch".to_owned());
    run.visits = 2;
    run.capped = true;
    run.word = "held".to_owned();
    let theme = Theme::fixed(false);
    let cases = [
        (52, "land ● ◆ watch ╰─ ◉ fix ×2 held 23m ○ ◇ ○ ◇  25m"),
        (51, "land ● ◆ watch ╰─ ◉ fix ×2 held 23m ○ ◇ ○ ◇ 25m"),
        (50, "land ● ◆ watch ╰─ ◉ fix ×2 held 23m ○ ◇ ○ ◇"),
        (48, "land ● ◆ watch ╰─ ◉ fix ×2 held 23m ○ ◇ ○ ◇"),
        (47, "land ● ◆ watch ╰─ ◉ fix ×2 held 23m ○ ◇ ○ ◇"),
        (46, "land ◆ watch ╰─ ◉ fix ×2 held 23m"),
        (38, "land ◆ watch ╰─ ◉ fix ×2 held 23m"),
        (37, "land ◆ watch ╰─ ◉ fix ×2 held 23m"),
        (36, "land ◉ fix ×2 held 23m"),
        (27, "land ◉ fix ×2 held 23m"),
        (26, "land ◉ fix ×2 held 23m"),
        (25, "◉ fix ×2 held 23m"),
        (22, "◉ fix ×2 held 23m"),
        (21, "◉ fix ×2 held 23m"),
        (20, "◉ fix ×2 held"),
        (18, "◉ fix ×2 held"),
        (17, "◉ fix ×2 held"),
        (16, "◉ fix ×2 he…"),
    ];
    let actual = cases
        .iter()
        .map(|(width, _)| {
            line_texts(&group_lines_at_width(&snapshot, &theme, 0, *width))
                .get(1)
                .map(|text| text.trim().to_owned())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, cases.map(|(_, expected)| expected.to_owned()));
    for (width, _) in cases {
        assert_snapshot(
            &format!("workflow_narrow_{width}"),
            snapshot_to_screen_for_theme(&snapshot, false, width as u16, 16),
        );
    }
}

#[test]
fn rowless_workflow_lenses_queries_and_no_match_use_groups() {
    let snapshot = snapshot();
    let mut ui = ViewState::default();
    let pane_ui = UiState::default();
    for query in ["land", "merge-lane", "#91"] {
        ui.make_up_filter.query = Some(query.to_owned());
        let screen = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, &pane_ui, 54, 22);
        assert!(screen.contains("watch queued"), "{query}: {screen}");
        assert!(
            screen.contains("1/1"),
            "rowless match counts once: {screen}"
        );
        assert!(!screen.contains("no match"), "{screen}");
    }
    ui.make_up_filter.query = Some("absent".to_owned());
    let screen = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, &pane_ui, 54, 22);
    assert!(screen.contains("no match"), "{screen}");
    ui.make_up_filter.query = None;
    for filter in [BodyFilter::Unread, BodyFilter::Status(AgentStatus::Running)] {
        ui.make_up_filter.filter = Some(filter);
        let screen = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, &pane_ui, 54, 22);
        assert!(!screen.contains("merge-lane"), "{screen}");
    }
}

#[test]
fn workflow_strip_states_have_one_current_and_keep_the_state_word() {
    for (name, node, word) in [
        ("workflow_watch", "watch", "queued"),
        ("workflow_held", "triage", "held"),
        ("workflow_takeover", "sweep", "takeover"),
        ("workflow_clean", "clean", "running"),
        ("workflow_parked", "watch", "parked"),
        ("workflow_trackless", "watch", "unknown"),
        ("workflow_hold", "watch", "holding unqueued"),
        ("workflow_fix", "fix", "held"),
    ] {
        let mut snapshot = snapshot();
        let run = run(&mut snapshot);
        move_current(run, node);
        run.word = word.to_owned();
        if node == "fix" {
            run.detour_parent = Some("watch".to_owned());
            run.visits = 2;
            run.capped = true;
        }
        if word == "parked" {
            run.state = crate::store::workflow::RunState::Parked;
        }
        if word == "unknown" {
            run.track.clear();
        }
        if word.starts_with("holding") {
            run.hold_limit = Some("30m".to_owned());
        }
        for (nerd, no_color, suffix) in [
            (false, false, "unicode"),
            (true, false, "nerd"),
            (false, true, "no_color"),
        ] {
            snapshot.theme.glyphs.set = Some(if nerd { "nerd-font" } else { "unicode" }.to_owned());
            let theme = Theme::fixed_for_theme(no_color, &snapshot.theme);
            let lines = group_lines_at_width(&snapshot, &theme, 0, 64);
            let text = line_texts(&lines).join("\n");
            assert!(text.contains(&format!("◉ {node}")), "{name}: {text}");
            assert!(text.contains(word), "{name}: {text}");
            assert_eq!(text.matches('◉').count(), 1, "{text}");
            if node == "fix" {
                let count = lines[1]
                    .spans
                    .iter()
                    .find(|span| span.content.contains("×2"))
                    .unwrap();
                assert_eq!(
                    count.style,
                    theme.styled(Component::WorkflowVisitsCapped, Modifier::empty())
                );
            }
            if word == "parked" {
                let state = lines[1]
                    .spans
                    .iter()
                    .find(|span| span.content == "parked")
                    .unwrap();
                assert_eq!(
                    state.style,
                    theme.styled(Component::WorkflowParked, Modifier::empty())
                );
            }
            if no_color {
                assert!(lines[1].spans.iter().all(|span| span.style.fg.is_none()));
            }
            if node == "fix" {
                assert!(text.contains("◆ watch ╰─ ◉ fix ×2"), "{text}");
            }
            assert_snapshot(
                &format!("{name}_{suffix}"),
                snapshot_to_screen_for_theme(&snapshot, no_color, 64, 18),
            );
        }
        if node == "fix" {
            snapshot.theme.glyphs =
                toml::from_str("[unicode.chrome]\nbox_bottom_left = '+'\nhairline = '-'").unwrap();
            let theme = Theme::fixed_for_theme(true, &snapshot.theme);
            let text = line_texts(&group_lines_at_width(&snapshot, &theme, 0, 64)).join("\n");
            assert!(text.contains("◆ watch +- ◉ fix ×2"), "{text}");
            assert_snapshot(
                "workflow_fix_ascii",
                snapshot_to_screen_for_theme(&snapshot, true, 64, 18),
            );
        }
    }
}

#[test]
fn workflow_card_section_tone_click_and_ordinals_agree() {
    let mut snapshot = super::worktree::pipeline_snapshot();
    let workflow = run(&mut self::snapshot()).clone();
    let group = &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0];
    let mut step = group.rows[0].clone();
    step.id = "step-session".to_owned();
    step.as_agent_mut().unwrap().handle = Some("sweeper".to_owned());
    step.as_agent_mut().unwrap().team = None;
    step.as_agent_mut().unwrap().status = AgentStatus::Waiting;
    let mut shell = step.clone();
    shell.id = "shell".to_owned();
    shell.name = "zsh".to_owned();
    shell.card = crate::store::snapshot::RowCard::Process(Default::default());
    let mut rows = group.rows.to_vec();
    rows.insert(1, shell);
    rows.push(step.clone());
    group.rows = rows.into();
    group.workflow = Some(workflow);
    let workflow = group.workflow.as_mut().unwrap();
    move_current(workflow, "sweep");
    workflow.current.kind = WorkflowNodeKind::Step;
    workflow.agent = Some((
        crate::ids::AgentKind::new_unchecked(&step.name),
        step.id.clone().into(),
    ));
    let theme = Theme::fixed(false);
    let costs = CostRolls::default();
    let ctx = test_row_ctx(&snapshot, &theme, 64, 0, 7, &costs);
    let roster = VisibleRoster::baseline(&snapshot);
    assert_eq!(roster.rows()[0].id, "step-session");
    assert_eq!(roster.rows().last().unwrap().id, "shell");
    let block = worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None);
    let texts = line_texts(&block.lines);
    assert!(
        texts[1].contains("◉ sweep") && !texts[1].contains("queued"),
        "{texts:?}"
    );
    let team_line = texts
        .iter()
        .position(|text| text.contains("Implement"))
        .unwrap();
    assert!(
        team_line > 2,
        "workflow card precedes team strip: {texts:?}"
    );
    assert_eq!(block.interactions.row_map()[1], None);
    assert_eq!(block.interactions.target_at(4, 1), Some(HitTarget::Row(0)));
    let mark = block.lines[1]
        .spans
        .iter()
        .find(|span| span.content == "◉")
        .unwrap();
    assert_eq!(
        mark.style,
        super::super::labels::status_style_at(&theme, AgentStatus::Waiting, 7)
            .add_modifier(Modifier::BOLD)
    );
    assert_eq!(
        block
            .card_starts
            .iter()
            .map(|(row, _)| *row)
            .collect::<Vec<_>>(),
        (0..4).collect::<Vec<_>>()
    );
    assert_snapshot(
        "workflow_with_team",
        snapshot_to_screen_for_theme(&snapshot, false, 64, 32),
    );

    move_current(run(&mut snapshot), "triage");
    run(&mut snapshot).word = "held".to_owned();
    let group = &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0];
    std::sync::Arc::make_mut(&mut group.rows)
        .iter_mut()
        .find(|row| row.id == "step-session")
        .unwrap()
        .as_agent_mut()
        .unwrap()
        .handle = Some("yagni".to_owned());
    let screen = snapshot_to_screen_for_theme(&snapshot, false, 64, 32);
    assert!(
        screen.contains("◉ triage") && !screen.contains("triage held"),
        "{screen}"
    );
    assert_snapshot("workflow_triage_with_card", screen);

    run(&mut snapshot).agent.as_mut().unwrap().1 = "elsewhere".to_owned().into();
    let ctx = test_row_ctx(&snapshot, &theme, 64, 0, 7, &costs);
    let block = worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None);
    assert_eq!(block.interactions.target_at(4, 1), None);
    assert!(line_texts(&block.lines)[1].contains("held"));
    move_current(run(&mut snapshot), "sweep");
    run(&mut snapshot).word = "takeover".to_owned();
    assert_snapshot(
        "workflow_takeover_with_team",
        snapshot_to_screen_for_theme(&snapshot, false, 64, 32),
    );
}
