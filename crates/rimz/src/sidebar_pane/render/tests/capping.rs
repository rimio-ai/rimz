use super::*;
use crate::sidebar_pane::render::fmt::dollars2;
use std::collections::{BTreeMap, HashSet};

#[test]
fn collapsed_cap_keeps_attention_focused_unread_and_liveness_process_rows() {
    let mut rows = idle_rows(8);
    rows.push(agent_row("failed", AgentStatus::Failed));
    assert_visible(
        &rows,
        None,
        false,
        "failed",
        "attention row remains visible past the calm-row cap",
    );

    let rows = idle_rows(8);
    let focused_pane = rows[7].pane.as_ref().expect("pane").pane_id.clone();
    let group = group(rows);
    let visible = visible_ids_with_context(&group, None, false, None, Some(&focused_pane));
    assert!(visible.contains(&"idle-7"));
    assert!(visible.len() < group.rows.len(), "tail still trims");

    let mut rows = idle_rows(8);
    rows[7].unread = true;
    assert_visible(
        &rows,
        None,
        false,
        "idle-7",
        "sticky unread idle row remains visible past the calm-row cap",
    );

    let mut rows = idle_rows(7)
        .into_iter()
        .map(|mut row| {
            row.inactive = true;
            row
        })
        .collect::<Vec<_>>();
    rows.push(process_row("proc-live"));
    assert_visible(
        &rows,
        None,
        false,
        "proc-live",
        "the only live process row remains visible as the group's liveness anchor",
    );
}

#[test]
fn collapsed_cap_trims_ordinary_idle_tail() {
    let group = group(idle_rows(9));
    let visible = visible_ids(&group, None, false);

    assert_eq!(
        visible,
        ["idle-0", "idle-1", "idle-2", "idle-3", "idle-4", "idle-5"]
    );
}

#[test]
fn expanded_and_filtered_groups_are_uncapped() {
    let group = group(idle_rows(9));

    assert_eq!(visible_ids(&group, None, true).len(), 9);
    assert_eq!(
        visible_ids(&group, Some(BodyFilter::Status(AgentStatus::Idle)), false).len(),
        9,
        "make-up filters show every matching row"
    );
}

#[test]
fn held_visible_rows_stay_visible_past_the_cap_and_update_more_count() {
    let group = group(idle_rows(9));
    let held = HashSet::from(["idle-8".to_owned()]);

    let visible = visible_ids_with_held(&group, None, false, Some(&held));

    assert!(visible.contains(&"idle-8"));
    assert_eq!(visible.len(), 7);

    let snapshot = snapshot_with(Vec::new());
    let theme = Theme::fixed(true);
    let cost_rolls = CostRolls::default();
    let ctx = test_row_ctx(&snapshot, &theme, 54, 0, 0, &cost_rolls);
    let roster =
        crate::sidebar_pane::view::VisibleRoster::single(&group, None, false, Some(&held), None);
    let lines = worktree_group_lines_projected(WorktreeRenderContext {
        row: &ctx,
        roster: &roster,
        group: &roster.groups()[0],
        meter_pixels: None,
    })
    .lines;
    let texts = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert!(
        texts.iter().any(|line| line.contains("+2 more")),
        "more count follows held visibility: {texts:?}"
    );
}

#[test]
fn expanded_group_keeps_less_control_when_hold_makes_all_rows_visible() {
    let group = group(idle_rows(9));
    let held = group
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<HashSet<_>>();

    let snapshot = snapshot_with(Vec::new());
    let theme = Theme::fixed(true);
    let cost_rolls = CostRolls::default();
    let ctx = test_row_ctx(&snapshot, &theme, 54, 0, 0, &cost_rolls);
    let roster =
        crate::sidebar_pane::view::VisibleRoster::single(&group, None, true, Some(&held), None);
    let block = worktree_group_lines_projected(WorktreeRenderContext {
        row: &ctx,
        roster: &roster,
        group: &roster.groups()[0],
        meter_pixels: None,
    });
    let lines = block.lines;
    let texts = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();

    assert!(
        texts.iter().any(|line| line.contains("− less")),
        "expanded group collapse control follows natural hidden tail: {texts:?}"
    );
    assert_eq!(block.interactions.regions().len(), 1);
}

#[test]
fn make_up_filter_ignores_held_visible_rows() {
    let group = group(idle_rows(9));
    let held = HashSet::from(["idle-8".to_owned()]);

    let visible = visible_ids_with_held(
        &group,
        Some(BodyFilter::Status(AgentStatus::Waiting)),
        false,
        Some(&held),
    );

    assert!(visible.is_empty(), "filter wins over held rows");
}

#[test]
fn finished_group_collapses_unread_success_until_revealed() {
    let mut rows = vec![
        agent_row("success-unread", AgentStatus::Success),
        agent_row("success", AgentStatus::Success),
    ];
    rows[0].unread = true;
    let mut group = group(rows);
    group.finished = true;

    assert!(
        visible_ids(&group, None, false).is_empty(),
        "terminal acceptance hides even unread success rows"
    );
    let held = HashSet::from(["success-unread".to_owned()]);
    assert_eq!(
        visible_ids_with_held(&group, None, false, Some(&held)),
        ["success-unread", "success"],
        "the order hold reveals the whole roster while the terminal collapse settles"
    );
    assert_eq!(visible_ids(&group, None, true).len(), 2);
    assert_eq!(
        visible_ids(
            &group,
            Some(BodyFilter::Status(AgentStatus::Success)),
            false
        )
        .len(),
        2,
        "a status filter reveals the terminal roster"
    );

    let focused_pane = group.rows[1].pane.as_ref().expect("pane").pane_id.clone();
    assert_eq!(
        visible_ids_with_context(&group, None, false, None, Some(&focused_pane)),
        ["success-unread", "success"],
        "a focused member reveals the whole finished roster"
    );
    let (focused_texts, focused_map, _) = render_group_with_focus(&group, false, &focused_pane);
    assert!(
        focused_texts.iter().all(|line| !line.contains('▸')),
        "a revealed finished roster renders full cards and no receipt: {focused_texts:?}"
    );
    assert!(
        focused_map.iter().skip(1).all(Option::is_some),
        "every line after the header belongs to a revealed card: {focused_map:?}"
    );
    group.rows[0]
        .as_agent_mut()
        .expect("agent row")
        .usage
        .total_tokens = Some(1_000);
    group.cohort_effort = Some(crate::SidebarCohortEffort {
        tokens: crate::agents::spending::EffortTokens {
            input: 1_000,
            ..crate::agents::spending::EffortTokens::default()
        },
        ..crate::SidebarCohortEffort::default()
    });

    let snapshot = snapshot_with(Vec::new());
    let theme = Theme::fixed(true);
    let cost_rolls = CostRolls::default();
    let ctx = test_row_ctx(&snapshot, &theme, 54, 0, 0, &cost_rolls);
    let block = worktree_group_block(&ctx, &group, false, None);
    let map = block.interactions.row_map();
    let more_hits = block.interactions.regions();
    let lines = block.lines;
    let texts = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();

    assert_eq!(
        lines.len(),
        3,
        "header plus the two-line terminal receipt: {texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|line| line.contains("✓ success-unread  ✓ success")),
        "terminal toggle names each hidden member: {texts:?}"
    );
    assert!(
        texts.iter().all(|line| !line.contains('$')),
        "absent costs keep the terminal toggle bare: {texts:?}"
    );
    assert_eq!(more_hits.len(), 3);
    assert_eq!(more_hits[0].rows, 0..1);
    assert_eq!(
        more_hits[0].target,
        HitTarget::ToggleGroup(group.key.clone())
    );
    assert_eq!(more_hits[1].rows, 1..2);
    assert_eq!(
        more_hits[1].target,
        HitTarget::ToggleGroup(group.key.clone())
    );
    assert_eq!(more_hits[2].rows, 2..3);
    assert_eq!(
        more_hits[2].target,
        HitTarget::ToggleGroup(group.key.clone())
    );
    assert_eq!(
        map,
        [None, None, None],
        "finished header and both receipt lines have only the toggle meaning"
    );
}

#[test]
fn finished_roster_names_keep_soft_provider_brand_tones() {
    let mut planner = agent_row("planner", AgentStatus::Success);
    planner.name = "claude".to_owned();
    planner.as_agent_mut().expect("agent row").handle = Some("planner".to_owned());
    let mut mystery = agent_row("mystery", AgentStatus::Success);
    mystery.name = "unregistered".to_owned();
    mystery.as_agent_mut().expect("agent row").handle = Some("mystery".to_owned());
    let mut finished = group(vec![planner, mystery]);
    finished.finished = true;

    let mut snapshot = snapshot_with(Vec::new());
    snapshot.worktree_groups = vec![finished];
    let theme = Theme::fixed(false);
    let lines = group_lines(&snapshot, &theme, 0);
    let receipt = lines
        .iter()
        .find(|line| line.spans.iter().any(|span| span.content.as_ref() == "▸"))
        .expect("finished roster receipt");
    let name_fg = |name: &str| {
        receipt
            .spans
            .iter()
            .find(|span| span.content.as_ref() == name)
            .unwrap_or_else(|| panic!("missing name span {name:?}: {receipt:?}"))
            .style
            .fg
    };

    assert_eq!(
        name_fg(" planner"),
        theme.body_brand(theme.clay()).fg,
        "registered kinds keep their softened definition brand"
    );
    assert_eq!(
        name_fg(" mystery"),
        theme.body_brand(Color::Indexed(244)).fg,
        "unregistered kinds use the shared neutral provider fallback"
    );
}

#[test]
fn finished_roster_leads_with_the_projected_team() {
    let mut finished = group(vec![
        agent_row("planner", AgentStatus::Success),
        agent_row("coder", AgentStatus::Success),
    ]);
    finished.finished = true;
    finished.team = Some("rimz".to_owned());

    let (texts, _, _) = render_group(&finished, false);
    assert!(
        roster_receipt(&texts).contains("▸ rimz  ✓ planner  ✓ coder"),
        "{texts:?}"
    );

    finished.team = None;
    let (texts, _, _) = render_group(&finished, false);
    assert!(
        roster_receipt(&texts).contains("▸ ✓ planner  ✓ coder"),
        "{texts:?}"
    );
}

#[test]
fn body_keeps_collapsed_finished_group_until_filter_empties_it() {
    let mut finished_rows = vec![
        agent_row("finished-one", AgentStatus::Success),
        agent_row("finished-two", AgentStatus::Success),
    ];
    finished_rows[0].name = "finished-one".to_owned();
    finished_rows[1].name = "finished-two".to_owned();
    let mut finished = group(finished_rows);
    finished.key = "/repo/merged-pod".to_owned();
    finished.label = "merged-pod".to_owned();
    finished.finished = true;

    let mut live_row = agent_row("live-row", AgentStatus::Running);
    live_row.name = "live-runner".to_owned();
    let mut live = group(vec![live_row]);
    live.key = "/repo/live-pod".to_owned();
    live.label = "live-pod".to_owned();

    let mut snapshot = snapshot_with(Vec::new());
    snapshot.worktree_groups = vec![finished, live];

    let screen = snapshot_to_screen(&snapshot, 54, 30);
    assert!(
        screen.contains("merged-pod"),
        "finished header remains:\n{screen}"
    );
    assert_eq!(screen.matches("finished-one").count(), 1, "{screen}");
    assert_eq!(screen.matches("finished-two").count(), 1, "{screen}");
    let receipt = screen
        .lines()
        .find(|line| line.contains("finished-one"))
        .expect("finished roster receipt");
    assert!(
        receipt.contains("✓ finished-one  ✓ finished-two"),
        "collapsed members share one roster line rather than full cards:\n{screen}"
    );
    assert!(
        screen.contains("live-runner"),
        "live group still renders:\n{screen}"
    );

    let expanded = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            expanded_groups: std::collections::BTreeSet::from(["/repo/merged-pod".to_owned()]),
            ..Default::default()
        },
        54,
        30,
    );
    assert!(
        expanded.contains("finished-one") && expanded.contains("finished-two"),
        "expanding the finished group reveals its member rows:\n{expanded}"
    );

    let filtered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            make_up_filter: Some(BodyFilter::Status(AgentStatus::Running)),
            ..Default::default()
        },
        54,
        30,
    );
    assert!(
        !filtered.contains("merged-pod") && !filtered.contains("finished-one"),
        "a filter-empty finished group is skipped whole:\n{filtered}"
    );
    assert!(
        filtered.contains("live-runner"),
        "the matching live group remains:\n{filtered}"
    );
}

#[test]
fn finished_roster_pins_the_member_cost() {
    let total = 0.42 + 0.58;
    let mut finished = group(vec![
        agent_row_with_cost("first", 0.42),
        agent_row_with_cost("second", 0.58),
    ]);
    finished.finished = true;
    finished.cohort_effort = Some(crate::SidebarCohortEffort {
        cost_usd: Some(total),
        ..crate::SidebarCohortEffort::default()
    });
    for row in &mut finished.rows {
        row.last_activity = fixed_now() - Duration::from_secs(2 * 60 * 60);
    }

    let (texts, _, _) = render_group(&finished, false);

    let roster = texts
        .iter()
        .find(|line| line.contains("✓ first  ✓ second"))
        .expect("member roster receipt");
    assert!(
        roster.trim_end().ends_with(&dollars2(total)),
        "finished roster pins the accepted work's cost: {texts:?}"
    );
    assert!(
        roster.starts_with(" ▸"),
        "the roster glyph leads the content without an extra indent: {texts:?}"
    );
    let totals = texts.last().expect("finished totals receipt");
    assert!(
        !totals.contains('$'),
        "cost stays off the token and age line: {texts:?}"
    );

    let mut no_cost = group(vec![
        agent_row_with_cost("sidecar-only", 2.0),
        agent_row("absent", AgentStatus::Success),
    ]);
    no_cost.finished = true;
    let (texts, _, _) = render_group(&no_cost, false);
    assert!(
        texts
            .iter()
            .any(|line| line.contains("✓ sidecar-only  ✓ absent"))
    );
    assert!(
        texts.iter().all(|line| !line.contains('$')),
        "missing cohort cost never falls back to live sidecars: {texts:?}"
    );
}

#[test]
fn expanded_finished_group_cards_show_seat_lifetime_cost() {
    let seats = BTreeMap::from([
        (
            "planner".to_owned(),
            crate::SidebarSeatEffort {
                cost_usd: Some(1.5),
                ..crate::SidebarSeatEffort::default()
            },
        ),
        (
            "coder".to_owned(),
            crate::SidebarSeatEffort {
                cost_usd: Some(2.5),
                ..crate::SidebarSeatEffort::default()
            },
        ),
    ]);
    let mut live = group(vec![
        agent_row_with_cost("planner", 0.42),
        agent_row_with_cost("coder", 0.58),
    ]);
    live.cohort_effort = Some(crate::SidebarCohortEffort {
        cost_usd: Some(4.0),
        seats: seats.clone(),
        ..crate::SidebarCohortEffort::default()
    });
    let (live_texts, _, _) = render_group(&live, false);
    assert!(
        live_texts
            .iter()
            .any(|line| line.contains("planner") && line.contains("$0.42")),
        "live cards retain session costs: {live_texts:?}"
    );
    assert!(
        live_texts
            .iter()
            .any(|line| line.contains("coder") && line.contains("$0.58")),
        "live cards retain session costs: {live_texts:?}"
    );

    let mut finished = group(vec![
        agent_row_with_cost("planner", 0.42),
        agent_row_with_cost("coder", 0.58),
    ]);
    finished.finished = true;
    finished.cohort_effort = Some(crate::SidebarCohortEffort {
        cost_usd: Some(4.0),
        seats,
        ..crate::SidebarCohortEffort::default()
    });

    let (receipt, _, _) = render_group(&finished, false);
    assert!(
        receipt.iter().any(|line| line.contains("$4.00")),
        "seat costs sum to the collapsed receipt: {receipt:?}"
    );
    let (expanded, _, _) = render_group(&finished, true);
    assert!(
        expanded
            .iter()
            .any(|line| line.contains("planner") && line.contains("$1.50")),
        "finished cards use lifetime seat costs: {expanded:?}"
    );
    assert!(
        expanded
            .iter()
            .any(|line| line.contains("coder") && line.contains("$2.50")),
        "finished cards use lifetime seat costs: {expanded:?}"
    );
    assert!(
        expanded
            .iter()
            .all(|line| !line.contains("$0.42") && !line.contains("$0.58")),
        "finished cards replace session costs: {expanded:?}"
    );
}

#[test]
fn finished_roster_folds_overflow_without_clipping_names() {
    let mut finished = group(vec![
        agent_row("planner", AgentStatus::Success),
        agent_row("coder", AgentStatus::Success),
        agent_row("reviewer", AgentStatus::Success),
        agent_row("observer", AgentStatus::Success),
    ]);
    finished.finished = true;

    let (texts, _, _) = render_group_at_width(&finished, false, 30);
    let receipt = roster_receipt(&texts);
    assert!(receipt.contains("▸ ✓ planner  ✓ coder  +2"), "{texts:?}");
    assert!(!receipt.contains("reviewer"), "{texts:?}");
    assert!(!receipt.contains("observer"), "{texts:?}");
}

#[test]
fn finished_roster_folds_process_rows_into_the_remainder() {
    let mut finished = group(vec![
        agent_row("planner", AgentStatus::Success),
        agent_row("coder", AgentStatus::Success),
        process_row("shell"),
    ]);
    finished.finished = true;

    let (texts, _, _) = render_group(&finished, false);
    let receipt = roster_receipt(&texts);
    assert!(receipt.contains("▸ ✓ planner  ✓ coder  +1"), "{texts:?}");
    assert!(!receipt.contains("zsh"), "{texts:?}");
}

#[test]
fn finished_process_only_group_stays_expanded() {
    let mut finished = group(vec![process_row("shell-one"), process_row("shell-two")]);
    finished.finished = true;

    let (texts, _, _) = render_group(&finished, false);
    assert!(!finished.collapses());
    assert!(
        texts.iter().all(|line| !line.contains("▸ +2 done")),
        "{texts:?}"
    );
    assert_eq!(
        visible_ids(&finished, None, false),
        ["shell-one", "shell-two"]
    );
    assert_eq!(texts.iter().filter(|line| line.contains("zsh")).count(), 2);
}

#[test]
fn held_member_reveals_the_whole_finished_roster() {
    let mut finished = group(vec![
        agent_row("planner", AgentStatus::Success),
        agent_row("coder", AgentStatus::Success),
        agent_row("reviewer", AgentStatus::Success),
    ]);
    finished.finished = true;
    let held = HashSet::from(["coder".to_owned()]);

    let snapshot = snapshot_with(Vec::new());
    let theme = Theme::fixed(true);
    let cost_rolls = CostRolls::default();
    let ctx = test_row_ctx(&snapshot, &theme, 54, 0, 0, &cost_rolls);
    let roster =
        crate::sidebar_pane::view::VisibleRoster::single(&finished, None, false, Some(&held), None);
    assert_eq!(
        roster
            .rows()
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["planner", "coder", "reviewer"]
    );
    let block = worktree_group_lines_projected(WorktreeRenderContext {
        row: &ctx,
        roster: &roster,
        group: &roster.groups()[0],
        meter_pixels: None,
    });
    let map = block.interactions.row_map();
    let lines = block.lines;
    let texts = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        texts.iter().filter(|line| line.contains('▸')).count(),
        0,
        "an order hold reveals full cards without a partial receipt: {texts:?}"
    );
    assert!(
        map.iter().skip(1).all(Option::is_some),
        "every line after the header belongs to a revealed card: {map:?}"
    );
}

#[test]
fn finished_header_toggles_both_directions_while_live_header_jumps_to_a_row() {
    let mut finished = group(idle_rows(2));
    finished.finished = true;
    let (texts, map, hits) = render_group(&finished, true);
    assert_eq!(map[0], None);
    assert!(
        texts.iter().all(|line| !line.contains("− less")),
        "expanded finished pods collapse through the header: {texts:?}"
    );
    assert!(hits.iter().any(|hit| {
        hit.rows == (0..1) && hit.target == HitTarget::ToggleGroup(finished.key.clone())
    }));

    let live = group(idle_rows(2));
    let (_, map, hits) = render_group(&live, false);
    assert_eq!(map[0], Some(0));
    assert!(
        hits.iter().all(|hit| !matches!(
            hit.target,
            HitTarget::ToggleGroup(ref key) if key == &live.key
        )),
        "live header retains only its row-jump behavior"
    );
}

#[test]
fn lone_finished_row_stays_visible_and_header_jumps_to_it() {
    for (kind, row, identity) in [
        ("agent", agent_row("solo", AgentStatus::Success), "✓ solo"),
        ("process", process_row("shell"), "○ zsh"),
    ] {
        let mut finished = group(vec![row]);
        finished.finished = true;

        let (texts, map, hits) = render_group(&finished, false);

        assert!(
            texts.iter().any(|line| line.contains(identity)),
            "lone finished {kind} keeps its natural card identity: {texts:?}"
        );
        assert!(
            texts.iter().all(|line| !line.contains('▸')),
            "lone finished {kind} emits no receipt: {texts:?}"
        );
        assert_eq!(map[0], Some(0), "{kind} header jumps to its row");
        assert!(
            map.iter().skip(1).all(|target| *target == Some(0)),
            "every {kind} card line targets row 0: {map:?}"
        );
        assert!(
            hits.iter().all(|hit| !matches!(
                hit.target,
                HitTarget::ToggleGroup(ref key) if key == &finished.key
            )),
            "lone finished {kind} header has no collapse target: {hits:?}"
        );
    }
}

#[test]
fn finished_receipt_pins_cost_then_tokens_and_muted_age() {
    let mut first = agent_row_with_cost("planner", 0.42);
    first.last_activity = fixed_now() - Duration::from_secs(2 * 60 * 60);
    let first_agent = first.as_agent_mut().expect("agent row");
    first_agent.registered_at = Some(fixed_now() - Duration::from_secs(152 * 60));
    first_agent.usage.total_tokens = Some(700_000);
    first_agent.usage.fresh_input_tokens = Some(200_000);
    first_agent.usage.output_tokens = Some(100_000);
    first_agent.usage.cache_read_input_tokens = Some(400_000);

    let mut second = agent_row_with_cost("coder", 0.58);
    second.last_activity = fixed_now() - Duration::from_secs(130 * 60);
    let second_agent = second.as_agent_mut().expect("agent row");
    second_agent.registered_at = Some(fixed_now() - Duration::from_secs(145 * 60));
    second_agent.usage.total_tokens = Some(500_000);
    second_agent.usage.fresh_input_tokens = Some(100_000);
    second_agent.usage.output_tokens = Some(80_000);
    second_agent.usage.cache_read_input_tokens = Some(300_000);

    let mut finished = group(vec![first, second]);
    finished.finished = true;
    finished.cohort_effort = Some(crate::SidebarCohortEffort {
        cost_usd: Some(1.0),
        tokens: crate::agents::spending::EffortTokens {
            input: 300_000,
            output: 180_000,
            cache_write: 0,
            cache_read: 700_000,
        },
        active_secs: Some(2 * 60 * 60),
        ..crate::SidebarCohortEffort::default()
    });
    let mut snapshot = snapshot_with(Vec::new());
    snapshot.worktree_groups = vec![finished];
    let theme = Theme::fixed(true);
    let lines = group_lines_at_width(&snapshot, &theme, 0, 80);
    let texts = line_texts(&lines);
    let roster = texts
        .iter()
        .find(|line| line.contains("✓ planner  ✓ coder"))
        .expect("finished roster receipt");
    let totals = lines.last().expect("finished totals receipt");
    let totals_text = texts.last().expect("finished totals text");

    assert!(
        roster.trim_end().ends_with("$1.00"),
        "cost pins the identity line: {texts:?}"
    );
    assert!(
        totals_text.contains("◇ 1M ↘ 300k ↗ 180k ◌ 700k"),
        "member token counters sum into one detailed breakdown: {texts:?}"
    );
    assert!(
        totals_text.trim_end().ends_with("◉ 2h"),
        "finished age is the rightmost pin: {texts:?}"
    );
    assert!(
        !totals_text.contains("32m") && !totals_text.contains('$'),
        "misleading duration and cost stay off the totals line: {texts:?}"
    );
    let age = totals
        .spans
        .iter()
        .find(|span| span.content.contains("◉ 2h"))
        .expect("finished age span");
    assert_eq!(
        age.style,
        theme.muted(),
        "a settled receipt keeps a flat age tone"
    );
}

#[test]
fn finished_receipt_aggregates_session_cache_hit_percent() {
    let mut finished = group(vec![
        agent_row("planner", AgentStatus::Success),
        agent_row("coder", AgentStatus::Success),
    ]);
    finished.finished = true;
    finished.cohort_effort = Some(crate::SidebarCohortEffort {
        tokens: crate::agents::spending::EffortTokens {
            input: 100,
            cache_read: 100,
            ..crate::agents::spending::EffortTokens::default()
        },
        ..crate::SidebarCohortEffort::default()
    });
    let (texts, _, _) = render_group(&finished, false);
    let totals = texts.last().expect("finished totals receipt");
    assert!(
        totals.contains("◇ 200") && totals.contains(" · 50%"),
        "the receipt aggregates member session counters: {texts:?}"
    );
    assert!(
        !totals.contains("90%") && !totals.contains("10%"),
        "no individual member ratio leaks into the team receipt: {texts:?}"
    );

    let mut no_counters = group(vec![
        agent_row("planner", AgentStatus::Success),
        agent_row("coder", AgentStatus::Success),
    ]);
    no_counters.finished = true;
    no_counters.cohort_effort = Some(crate::SidebarCohortEffort {
        tokens: crate::agents::spending::EffortTokens {
            output: 200,
            ..crate::agents::spending::EffortTokens::default()
        },
        ..crate::SidebarCohortEffort::default()
    });
    let (texts, _, _) = render_group(&no_counters, false);
    let totals = texts.last().expect("finished totals receipt");
    assert!(
        !totals.contains('%'),
        "a receipt without session counters omits the ratio: {texts:?}"
    );
}

#[test]
fn finished_totals_degrade_tokens_before_the_right_pin() {
    let mut row = agent_row_with_cost("planner", 1.0);
    row.last_activity = fixed_now() - Duration::from_secs(2 * 60 * 60);
    let agent = row.as_agent_mut().expect("agent row");
    agent.registered_at = Some(fixed_now() - Duration::from_secs(152 * 60));
    agent.usage.total_tokens = Some(1_200_000);
    agent.usage.fresh_input_tokens = Some(300_000);
    agent.usage.output_tokens = Some(180_000);
    agent.usage.cache_read_input_tokens = Some(700_000);
    let mut witness = agent_row("coder", AgentStatus::Success);
    witness.last_activity = row.last_activity;
    let mut finished = group(vec![row, witness]);
    finished.finished = true;
    finished.cohort_effort = Some(crate::SidebarCohortEffort {
        cost_usd: Some(1.0),
        tokens: crate::agents::spending::EffortTokens {
            input: 300_000,
            output: 180_000,
            cache_write: 0,
            cache_read: 700_000,
        },
        active_secs: Some(2 * 60 * 60),
        ..crate::SidebarCohortEffort::default()
    });

    let (texts, _, _) = render_group_at_width(&finished, false, 30);
    let summary = texts.last().expect("summary totals receipt");
    assert!(summary.contains("◇ 1M ◌ 700k"), "{texts:?}");
    assert!(
        !summary.contains('↘') && !summary.contains('↗'),
        "{texts:?}"
    );
    assert!(summary.trim_end().ends_with("◉ 2h"), "{texts:?}");
    assert!(
        !summary.contains("32m") && !summary.contains('$'),
        "{texts:?}"
    );

    let (texts, _, _) = render_group_at_width(&finished, false, 18);
    let total_only = texts.last().expect("total-only receipt");
    assert!(total_only.contains("◇ 1M"), "{texts:?}");
    assert!(!total_only.contains('◌'), "{texts:?}");
    assert!(total_only.trim_end().ends_with("◉ 2h"), "{texts:?}");
    assert!(
        !total_only.contains("32m") && !total_only.contains('$'),
        "right pin survives token degradation: {texts:?}"
    );
}

fn render_group(
    group: &crate::SidebarWorktreeGroup,
    expanded: bool,
) -> (Vec<String>, Vec<Option<usize>>, Vec<HitRegion>) {
    render_group_at_width(group, expanded, 54)
}

fn render_group_with_focus(
    group: &crate::SidebarWorktreeGroup,
    expanded: bool,
    focused_pane: &crate::PaneId,
) -> (Vec<String>, Vec<Option<usize>>, Vec<HitRegion>) {
    let snapshot = snapshot_with(Vec::new());
    let theme = Theme::fixed(true);
    let cost_rolls = CostRolls::default();
    let ctx = test_row_ctx(&snapshot, &theme, 54, 0, 0, &cost_rolls);
    let roster = crate::sidebar_pane::view::VisibleRoster::single(
        group,
        None,
        expanded,
        None,
        Some(focused_pane),
    );
    let block = worktree_group_lines_projected(WorktreeRenderContext {
        row: &ctx,
        roster: &roster,
        group: &roster.groups()[0],
        meter_pixels: None,
    });
    let texts = block
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect();
    (
        texts,
        block.interactions.row_map().to_vec(),
        block.interactions.regions().to_vec(),
    )
}

fn render_group_at_width(
    group: &crate::SidebarWorktreeGroup,
    expanded: bool,
    width: usize,
) -> (Vec<String>, Vec<Option<usize>>, Vec<HitRegion>) {
    let snapshot = snapshot_with(Vec::new());
    let theme = Theme::fixed(true);
    let cost_rolls = CostRolls::default();
    let ctx = test_row_ctx(&snapshot, &theme, width, 0, 0, &cost_rolls);
    let block = worktree_group_block(&ctx, group, expanded, None);
    let texts = block
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect();
    (
        texts,
        block.interactions.row_map().to_vec(),
        block.interactions.regions().to_vec(),
    )
}

fn roster_receipt(texts: &[String]) -> &str {
    texts
        .iter()
        .find(|line| line.contains('▸'))
        .map(String::as_str)
        .unwrap_or_else(|| panic!("missing finished roster receipt: {texts:?}"))
}

fn assert_visible(
    rows: &[crate::SidebarRow],
    filter: Option<BodyFilter>,
    expanded: bool,
    id: &str,
    message: &str,
) {
    let group = group(rows.to_vec());
    let visible = visible_ids_with_held(&group, filter, expanded, None);
    assert!(visible.contains(&id), "{message}: {visible:?}");
    assert!(visible.len() < group.rows.len(), "tail still trims");
}

fn visible_ids(
    group: &crate::SidebarWorktreeGroup,
    filter: Option<BodyFilter>,
    expanded: bool,
) -> Vec<&str> {
    visible_ids_with_held(group, filter, expanded, None)
}

fn visible_ids_with_held<'a>(
    group: &'a crate::SidebarWorktreeGroup,
    filter: Option<BodyFilter>,
    expanded: bool,
    held: Option<&HashSet<String>>,
) -> Vec<&'a str> {
    visible_ids_with_context(group, filter, expanded, held, None)
}

fn visible_ids_with_context<'a>(
    group: &'a crate::SidebarWorktreeGroup,
    filter: Option<BodyFilter>,
    expanded: bool,
    held: Option<&HashSet<String>>,
    focused_pane: Option<&crate::PaneId>,
) -> Vec<&'a str> {
    crate::sidebar_pane::view::VisibleRoster::single(group, filter, expanded, held, focused_pane)
        .rows()
        .iter()
        .copied()
        .map(|row| row.id.as_str())
        .collect()
}

fn group(rows: Vec<crate::SidebarRow>) -> crate::SidebarWorktreeGroup {
    crate::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        label_qualifier: None,
        kind: crate::SidebarWorktreeKind::Worktree,
        team: None,
        cohort_effort: None,
        status_counts: Vec::new(),
        rows,
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        worktree_backed: false,
        finished: false,
        clean: None,
        landed: None,
        trunk_sync: None,
        pr_state: None,
        pr_ci: None,
        pr_number: None,
        pr_url: None,
    }
}

fn idle_rows(count: usize) -> Vec<crate::SidebarRow> {
    (0..count)
        .map(|index| agent_row(&format!("idle-{index}"), AgentStatus::Idle))
        .collect()
}

fn agent_row(id: &str, status: AgentStatus) -> crate::SidebarRow {
    crate::SidebarRow {
        id: id.to_owned(),
        name: id.to_owned(),
        pane: Some(pane(&format!("%{id}"), "codex", "/repo/main")),
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: fixed_now(),
        card: crate::RowCard::Agent(Box::new(crate::AgentCard {
            status,
            ..crate::AgentCard::default()
        })),
    }
}

fn agent_row_with_cost(id: &str, cost: f64) -> crate::SidebarRow {
    let mut row = agent_row(id, AgentStatus::Success);
    let mut context = claude_context(fixed_now());
    context.cost = Some(AgentCost {
        total_cost_usd: Some(cost),
        ..AgentCost::default()
    });
    row.as_agent_mut().expect("agent row").context = Some(context);
    row
}

fn process_row(id: &str) -> crate::SidebarRow {
    crate::SidebarRow {
        id: id.to_owned(),
        name: "zsh".to_owned(),
        pane: Some(pane(&format!("%{id}"), "zsh", "/repo/main")),
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: fixed_now(),
        card: crate::RowCard::Process(crate::ProcessCard::default()),
    }
}
