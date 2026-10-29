use super::*;
use crate::agents::{PendingWait, PendingWaitTrigger};
use crate::sidebar_pane::render::EntryCursor;
use crate::sidebar_pane::render::labels::{activity_age_style, elapsed_glyph};
use crate::sidebar_pane::render::theme::Component;

#[test]
fn child_headlines_are_muted_exactly_when_the_entry_has_no_live_pane() {
    let theme = Theme::fixed(false);
    let pane: PaneId = "zellij:terminal_child".parse().unwrap();
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (status, live, native, error) in [
        (AgentStatus::Running, true, false, false),
        (AgentStatus::Success, false, false, false),
        (AgentStatus::Running, false, true, false),
        (AgentStatus::Failed, false, false, true),
    ] {
        let mut snapshot = fast_child_snapshot(status, false);
        let child = &mut fast_children_mut(&mut snapshot)[0];
        child.fast = false;
        child.name = "claude".to_owned();
        child.pane = live.then_some(pane.clone());
        child.provider_native = native;
        child.description = Some("audit the trust hash".to_owned());
        child.turn_error_label = error.then(|| "limit reached".to_owned());
        let rolls = CostRolls::default();
        let ctx = test_row_ctx(&snapshot, &theme, 54, 0, 0, &rolls);
        let block = worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None);
        let label = if error {
            "limit reached"
        } else {
            "audit the trust hash"
        };
        let (line_index, line) = block
            .lines
            .iter()
            .enumerate()
            .find(|(_, line)| line.to_string().contains(label))
            .unwrap();
        let headline = line
            .spans
            .iter()
            .find(|span| span.content == label)
            .unwrap();
        let metadata = block
            .lines
            .iter()
            .position(|line| line.to_string().contains("Haiku"))
            .unwrap();
        let has_target = |line| {
            matches!(
                block.interactions.target_at(1, line as u16),
                Some(HitTarget::SubAgentPane(_))
            )
        };
        actual.push((
            headline.style.fg,
            headline.style.add_modifier.contains(Modifier::ITALIC),
            has_target(line_index),
            has_target(metadata),
        ));
        expected.push((
            if live || error {
                theme.body().fg
            } else {
                theme.muted().fg
            },
            error,
            live,
            live,
        ));
    }
    assert_eq!(actual, expected);
}

#[test]
fn child_headline_uses_description_task_then_first_prompt_without_repeating_the_type() {
    let mut actual = Vec::new();
    for (description, task, prompt) in [
        (Some("description"), Some("task"), Some("prompt")),
        (None, Some("task"), Some("prompt")),
        (None, None, Some("prompt")),
        (Some("claude"), Some("claude"), Some("prompt")),
        (None, None, Some("claude")),
    ] {
        let mut snapshot = fast_child_snapshot(AgentStatus::Success, false);
        let child = &mut fast_children_mut(&mut snapshot)[0];
        child.name = "claude".to_owned();
        child.fast = false;
        child.description = description.map(str::to_owned);
        child.task = task.map(str::to_owned);
        let mut wire = serde_json::to_value(&*child).unwrap();
        wire["first_prompt"] = serde_json::json!(prompt);
        *child = serde_json::from_value(wire).unwrap();
        let lines = group_lines(&snapshot, &Theme::fixed(false), 0);
        actual.push(
            lines
                .iter()
                .map(Line::to_string)
                .find(|line| line.contains("✓ claude"))
                .unwrap()
                .trim_matches(['▌', '▐', ' '])
                .to_owned(),
        );
    }
    assert_eq!(
        actual,
        [
            "✓ claude · description",
            "✓ claude · task",
            "✓ claude · prompt",
            "✓ claude · prompt",
            "✓ claude"
        ]
    );
}

fn fast_children_mut(
    snapshot: &mut SidebarSnapshot,
) -> &mut Vec<crate::store::snapshot::SidebarSubAgent> {
    &mut std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .as_agent_mut()
    .unwrap()
    .sub_agents
}

fn fast_child_snapshot(status: AgentStatus, stalled: bool) -> SidebarSnapshot {
    let mut parent = agent("parent", "claude", AgentStatus::Idle, None, None, None);
    parent.prompt = Some("Delegate the work".to_owned());
    let mut child = agent("child", "claude", status, None, None, Some("worker"));
    child.parent_agent_id = Some("parent".into());
    child.launch_depth = Some(1);
    child.model = Some("Haiku".to_owned());
    child.usage.fresh_input_tokens = Some(12_000);
    child.registered_at = Some(fixed_now() - Duration::from_secs(120));
    let mut snapshot = snapshot_with(vec![parent, child]);
    let child = &mut fast_children_mut(&mut snapshot)[0];
    child.fast = true;
    child.status = status;
    child.stalled = stalled;
    snapshot
}

#[test]
fn fast_child_bolt_is_bold_and_still_in_every_status_and_absent_without_a_model() {
    for no_color in [false, true] {
        let theme = Theme::fixed(no_color);
        for (status, stalled) in [
            (AgentStatus::Running, false),
            (AgentStatus::Idle, false),
            (AgentStatus::Success, false),
            (AgentStatus::Failed, false),
            (AgentStatus::Failed, true),
        ] {
            let snapshot = fast_child_snapshot(status, stalled);
            for phase in 0..128 {
                let cost_rolls = CostRolls::default();
                let ctx = test_row_ctx(&snapshot, &theme, 54, 0, phase, &cost_rolls);
                let block = worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None);
                let metadata = block
                    .lines
                    .iter()
                    .find(|line| line.to_string().contains("Haiku"))
                    .unwrap();
                assert!(
                    metadata.to_string().contains("Haiku ϟ"),
                    "{status:?}, {stalled}, {phase}: {metadata}"
                );
                let bolt = metadata
                    .spans
                    .iter()
                    .find(|span| span.content == theme.glyph(GlyphRole::CardFast))
                    .unwrap();
                let expected = theme.styled(Component::FastMode, Modifier::BOLD);
                assert_eq!(bolt.style.fg, expected.fg);
                assert_eq!(bolt.style.add_modifier, expected.add_modifier);
                assert_eq!(bolt.style.sub_modifier, expected.sub_modifier);
            }
        }
        let mut snapshot = fast_child_snapshot(AgentStatus::Running, false);
        fast_children_mut(&mut snapshot)[0].model = None;
        assert!(
            !group_lines(&snapshot, &theme, 0)
                .iter()
                .any(|line| line.to_string().contains(theme.glyph(GlyphRole::CardFast)))
        );
    }
}

#[test]
fn fast_child_bolt_follows_effort_without_reserving_model_cells() {
    let theme = Theme::fixed(false);
    for (fast_model, plain_model) in [
        ("Haiku", "LongModel"),
        ("模型模型", "Haiku"),
        ("Haiku", "Haiku"),
    ] {
        let mut snapshot = fast_child_snapshot(AgentStatus::Running, false);
        let children = fast_children_mut(&mut snapshot);
        children[0].model = Some(fast_model.to_owned());
        children[0].effort = Some("high".to_owned());
        let mut sibling = children[0].clone();
        sibling.id = "sibling".to_owned();
        sibling.fast = false;
        sibling.model = Some(plain_model.to_owned());
        sibling.effort = Some("low".to_owned());
        children.push(sibling);
        let lines = group_lines(&snapshot, &theme, 0);
        let fast = lines
            .iter()
            .map(Line::to_string)
            .find(|line| line.contains(fast_model) && line.contains("high"))
            .unwrap();
        let plain = lines
            .iter()
            .map(Line::to_string)
            .find(|line| line.contains(plain_model) && line.contains("low"))
            .unwrap();
        assert!(fast.contains("high ϟ"), "{fast}");
        assert!(!plain.contains('ϟ'), "{plain}");
        let fast_prefix = &fast[..fast.find("high").unwrap()];
        let plain_prefix = &plain[..plain.find("low").unwrap()];
        assert_eq!(
            Line::from(fast_prefix).width(),
            Line::from(plain_prefix).width()
        );
        if fast_model == plain_model {
            assert_eq!(fast_prefix, plain_prefix);
            assert!(plain_prefix.ends_with("Haiku · "), "{plain}");
        }
    }
}

#[test]
fn fast_child_without_effort_attaches_bolt_to_model_without_reserving_sibling_cells() {
    let mut snapshot = fast_child_snapshot(AgentStatus::Running, false);
    let children = fast_children_mut(&mut snapshot);
    children[0].compaction_count = 1;
    let mut sibling = children[0].clone();
    sibling.id = "sibling".to_owned();
    sibling.fast = false;
    children.push(sibling);
    let theme = Theme::fixed(false);
    let lines = group_lines(&snapshot, &theme, 0);
    assert!(
        lines
            .iter()
            .any(|line| line.to_string().contains("Haiku ϟ · ↻ 1"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.to_string().contains("Haiku · ↻ 1")),
        "{lines:?}"
    );
}

#[test]
fn fast_child_two_cell_bolt_drops_whole_at_the_clock_boundary() {
    let mut snapshot = fast_child_snapshot(AgentStatus::Running, false);
    snapshot.theme = toml::from_str("[glyphs.unicode.card]\nfast = '++'").unwrap();
    let theme = Theme::fixed_for_theme(false, &snapshot.theme);
    for (effort, first_width, label) in [(None, 30, "Haiku ++"), (Some("high"), 37, "high ++")] {
        fast_children_mut(&mut snapshot)[0].effort = effort.map(str::to_owned);
        for (width, visible) in [
            (first_width - 1, false),
            (first_width, true),
            (first_width + 1, true),
        ] {
            let lines = group_lines_at_width(&snapshot, &theme, 0, width);
            let metadata = lines
                .iter()
                .find(|line| line.to_string().contains("Haiku"))
                .unwrap()
                .to_string();
            assert_eq!(metadata.contains(label), visible, "{width}: {metadata}");
            assert_eq!(
                metadata.contains('+'),
                visible,
                "no partial bolt: {metadata}"
            );
            assert!(
                metadata.trim_end_matches('▐').ends_with("   2m"),
                "{metadata}"
            );
            assert_eq!(Line::from(metadata).width(), width);
        }
    }
}

#[test]
fn fast_child_bolt_precedes_compactions_which_drop_first_at_the_clock_boundary() {
    let mut snapshot = fast_child_snapshot(AgentStatus::Running, false);
    let child = &mut fast_children_mut(&mut snapshot)[0];
    child.effort = Some("high".to_owned());
    child.compaction_count = 12;
    let theme = Theme::fixed(false);
    for (width, bolt, compactions) in [
        (44, true, true),
        (43, true, true),
        (42, true, false),
        (37, true, false),
        (36, true, false),
        (35, false, false),
    ] {
        let lines = group_lines_at_width(&snapshot, &theme, 0, width);
        let metadata = lines
            .iter()
            .find(|line| line.to_string().contains("Haiku"))
            .unwrap()
            .to_string();
        assert_eq!(metadata.contains("high ϟ"), bolt, "{width}: {metadata}");
        assert_eq!(metadata.contains('ϟ'), bolt, "{width}: {metadata}");
        assert_eq!(
            metadata.contains("↻ 12"),
            compactions,
            "{width}: {metadata}"
        );
        assert_eq!(metadata.contains('↻'), compactions, "{width}: {metadata}");
        if compactions {
            assert!(metadata.contains("high ϟ · ↻ 12"), "{metadata}");
        }
        assert!(
            metadata.trim_end_matches('▐').ends_with("   2m"),
            "{metadata}"
        );
        assert_eq!(Line::from(metadata).width(), width);
    }
}

#[test]
fn fast_child_adds_no_animation_cadence_at_rest_or_while_running() {
    for status in [AgentStatus::Idle, AgentStatus::Running] {
        let mut snapshot = fast_child_snapshot(status, false);
        let fast = animation_cadence_for_test(&snapshot);
        fast_children_mut(&mut snapshot)[0].fast = false;
        assert_eq!(fast, animation_cadence_for_test(&snapshot), "{status:?}");
    }
}

#[test]
fn child_compaction_count_uses_the_child_record_for_live_and_finished_entries() {
    for status in [AgentStatus::Running, AgentStatus::Success] {
        for count in [0, 1, 12] {
            let mut parent = agent("parent", "claude", AgentStatus::Running, None, None, None);
            parent.compaction_count = 9;
            let mut child = agent("child", "claude", status, None, None, Some("Explore"));
            child.parent_agent_id = Some("parent".into());
            child.compaction_count = count;
            child.model = Some("Haiku".to_owned());
            let snapshot = snapshot_with(vec![parent, child]);
            let theme = Theme::fixed(false);
            let lines = group_lines(&snapshot, &theme, 0);
            let metadata = lines
                .iter()
                .find(|line| line.to_string().contains("Haiku"))
                .unwrap();
            let text = metadata.to_string();
            let glyph = theme.glyph(GlyphRole::TokensCompaction);
            assert_eq!(
                text.contains(glyph),
                count > 0,
                "{status:?}, {count}: {text}"
            );
            if count > 0 {
                assert!(text.contains(&format!("{glyph} {count}")), "{text}");
                let marker = metadata
                    .spans
                    .iter()
                    .find(|span| span.content == glyph)
                    .unwrap();
                let parent_marker = lines
                    .iter()
                    .flat_map(|line| &line.spans)
                    .find(|span| span.content == glyph)
                    .unwrap();
                assert_eq!(marker.style, parent_marker.style);
            }
        }
    }
}

#[test]
fn child_compaction_count_drops_whole_at_the_clock_boundary_without_moving_columns() {
    let parent = agent("parent", "claude", AgentStatus::Running, None, None, None);
    let mut child = agent(
        "child",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("Explore"),
    );
    child.parent_agent_id = Some("parent".into());
    child.compaction_count = 12;
    child.usage.fresh_input_tokens = Some(12_000);
    child.model = Some("Haiku".to_owned());
    child.effort = Some("high".to_owned());
    child.subagent_started_at = Some(fixed_now() - Duration::from_secs(120));
    let mut sibling = child.clone();
    sibling.agent_id = "sibling".into();
    sibling.compaction_count = 0;
    let snapshot = snapshot_with(vec![parent, child, sibling]);
    let theme = Theme::fixed(false);
    for (width, visible) in [(39, false), (40, false), (41, true), (42, true)] {
        let lines = group_lines_at_width(&snapshot, &theme, 0, width);
        let metadata = lines
            .iter()
            .map(Line::to_string)
            .filter(|line| line.contains("Haiku"))
            .collect::<Vec<_>>();
        let [compacted, fresh] = metadata.as_slice() else {
            panic!("two metadata lines: {metadata:?}")
        };
        assert_eq!(
            compacted.contains("↻ 12"),
            visible,
            "width={width}: {compacted}"
        );
        assert_eq!(
            compacted.contains('↻'),
            visible,
            "no partial count: {compacted}"
        );
        assert!(!fresh.contains('↻'));
        if !visible {
            assert_eq!(compacted, fresh, "the seam drops with the count");
        }
        for word in ["Haiku", "high"] {
            assert_eq!(
                compacted.find(word),
                fresh.find(word),
                "width={width}: {metadata:?}"
            );
        }
        assert!(compacted.ends_with("   2m▐"), "{compacted}");
        assert_eq!(compacted.chars().count(), width);
    }
}

#[test]
fn child_type_uses_its_own_provider_tone_and_keeps_the_model_muted() {
    for (kind, launched, name) in [("codex", true, "implementer"), ("claude", false, "Explore")] {
        let parent = agent("parent", "claude", AgentStatus::Running, None, None, None);
        let mut child = agent("child", kind, AgentStatus::Running, None, None, Some(name));
        child.parent_agent_id = Some("parent".into());
        child.parent_agent_kind = Some(parent.kind.clone());
        let model = if launched { "GPT 5.5" } else { "Haiku" };
        child.model = Some(model.to_owned());
        if launched {
            child.launch_depth = Some(1);
            child.profile = Some(name.to_owned());
        }
        let snapshot = snapshot_with(vec![parent, child]);
        let theme = Theme::fixed(false);
        for selected in [0, usize::MAX] {
            let mut snapshot = snapshot.clone();
            snapshot.theme.display.card_density = crate::config::CardDensityMode::Expanded;
            let lines = group_lines(&snapshot, &theme, selected);
            let spans = lines
                .iter()
                .flat_map(|line| &line.spans)
                .collect::<Vec<_>>();
            let type_word = spans.iter().find(|span| span.content == name).unwrap();
            assert_eq!(
                type_word.style.fg,
                theme.body_brand(theme.provider_brand_tone(kind)).fg,
                "{kind}"
            );
            let model_span = spans.iter().find(|span| span.content == model).unwrap();
            assert_eq!(model_span.style.fg, theme.muted().fg);
        }
    }
}

#[test]
fn completed_compactions_keep_an_otherwise_metadata_free_finished_child_second_line() {
    let parent = agent("parent", "claude", AgentStatus::Running, None, None, None);
    let mut child = agent(
        "child",
        "claude",
        AgentStatus::Success,
        None,
        None,
        Some("Explore"),
    );
    child.parent_agent_id = Some("parent".into());
    child.compaction_count = 1;
    let snapshot = snapshot_with(vec![parent, child]);
    let theme = Theme::fixed(false);
    let lines = group_lines(&snapshot, &theme, 0);
    let metadata = lines
        .iter()
        .map(Line::to_string)
        .find(|line| line.contains("↻ 1"))
        .unwrap();
    let left = metadata.trim_start_matches(['▌', ' ']);
    assert!(left.starts_with('↻'), "no orphan seam: {metadata}");
}

fn focused_child_snapshot() -> SidebarSnapshot {
    let parent = agent(
        "parent",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        None,
        Some("delegate the scan"),
    );
    let mut snapshot = snapshot_with(vec![parent]);
    let child = PaneId::from_parts(MuxName::Zellij, "terminal_child");
    let card = std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .as_agent_mut()
    .unwrap();
    card.sub_agent_count = 2;
    card.sub_agents = [
        ("explorer", Some(child.clone()), "inspect the click seam"),
        ("reviewer", None, "review the parent card"),
    ]
    .into_iter()
    .map(|(name, pane, description)| {
        serde_json::from_value(serde_json::json!({
            "id": name, "kind": "codex", "name": name, "status": "running", "last_activity": snapshot.now,
            "pane": pane, "description": description, "model": "Haiku", "phase": "reasoning",
        }))
        .unwrap()
    })
    .collect();
    snapshot.focused_pane = Some(child);
    snapshot
}

#[test]
fn selected_parent_marks_both_focused_child_entry_lines() {
    let mut snapshot = focused_child_snapshot();
    let cursor = EntryCursor::SubAgent(snapshot.focused_pane.take().unwrap());
    for color in [false, true] {
        let theme = Theme::fixed(color);
        let rolls = CostRolls::default();
        let mut ctx = test_row_ctx(&snapshot, &theme, 54, 0, 0, &rolls);
        ctx.entry_cursor = Some(&cursor);
        let lines = worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None).lines;
        let marked = lines
            .iter()
            .filter(|line| line.to_string().contains("  ▏ "))
            .collect::<Vec<_>>();
        assert_eq!(marked.len(), 2);
        for line in marked {
            let marker = line
                .spans
                .iter()
                .skip(1)
                .find(|span| {
                    span.content == theme.glyph(crate::config::GlyphRole::ChromeSpineEntry)
                })
                .unwrap();
            assert_eq!(marker.style.fg, theme.selection().fg);
            assert!(!marker.style.add_modifier.contains(Modifier::BOLD));
        }
    }
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &ViewState {
            selected_index: Some(0),
            entry_cursor: Some(cursor),
            ..Default::default()
        },
        &UiState::default(),
        54,
        24,
    );
    assert!(
        rendered
            .lines()
            .any(|line| line.starts_with("▌  ▏ ⠁ explorer"))
    );
    assert!(
        rendered
            .lines()
            .any(|line| line.starts_with("▌  ▏   Haiku"))
    );
    assert!(
        rendered
            .lines()
            .any(|line| line.starts_with("▌    ⠁ reviewer"))
    );
    assert_snapshot("focused_child_selected", rendered);
}

#[test]
fn unselected_parent_does_not_mark_the_focused_child_entry() {
    let mut snapshot = focused_child_snapshot();
    snapshot.theme.display.card_density = crate::config::CardDensityMode::Expanded;
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &ViewState {
            entry_cursor: snapshot.focused_pane.clone().map(EntryCursor::SubAgent),
            ..Default::default()
        },
        &UiState::default(),
        54,
        24,
    );
    assert!(rendered.lines().any(|line| line.contains("explorer")));
    assert!(!rendered.contains("  ▏ "));
    assert_snapshot("focused_child_unselected", rendered);
}

#[test]
fn older_entry_marks_the_cursor_in_the_same_light_selection_tone() {
    let mut snapshot = focused_child_snapshot();
    let card = std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .as_agent_mut()
    .unwrap();
    card.sub_agents[1].status = AgentStatus::Success;
    card.sub_agents[1].last_activity = snapshot.now - Duration::from_secs(3_600);
    let theme = Theme::fixed(false);
    let frame = compose_lines(
        &snapshot,
        None,
        &ViewState {
            selected_index: Some(0),
            entry_cursor: Some(EntryCursor::Older),
            ..Default::default()
        },
        &UiState::default(),
        &theme,
        54,
        24,
    );
    let line = frame
        .lines
        .iter()
        .find(|line| line.to_string().contains("+1 older"))
        .unwrap();
    let marker = line.spans.iter().find(|span| span.content == "▏");
    assert!(marker.is_some(), "the older stop carries the entry cursor");
    let style = marker.unwrap().style;
    assert_eq!(style.fg, theme.selection().fg);
    assert!(!style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn child_clock_uses_muted_runtime_until_quiet_and_the_configured_stall_scale() {
    let parent = agent(
        "root",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        None,
        None,
    );
    let mut child = agent(
        "child",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("Explore"),
    );
    child.parent_agent_id = Some("root".into());
    child.subagent_started_at = Some(fixed_now() - Duration::from_secs(2 * 3_600));
    child.model = Some("Haiku".to_owned());
    for (status, quiet, ceiling, expected) in [
        (AgentStatus::Running, 299, 1_800, "   2h"),
        (AgentStatus::Running, 480, 1_800, "◔  8m"),
        (AgentStatus::Running, 960, 1_800, "◑ 16m"),
        (AgentStatus::Running, 960, 900, "◉ 16m"),
        (AgentStatus::Running, 2_400, 1_800, "◉ 40m"),
        (AgentStatus::Paused, 480, 1_800, "   1h"),
        (AgentStatus::Waiting, 480, 1_800, "   1h"),
        (AgentStatus::Success, 480, 1_800, "   1h"),
    ] {
        child.status = status;
        child.last_activity = fixed_now() - Duration::from_secs(quiet);
        let mut snapshot = snapshot_with(vec![parent.clone(), child.clone()]);
        snapshot.attention.stalled_after_secs = std::num::NonZeroU32::new(ceiling).unwrap();
        let theme = Theme::fixed(false);
        let lines = group_lines(&snapshot, &theme, 0);
        let metadata = lines
            .iter()
            .find(|line| line.spans.iter().any(|span| span.content == "Haiku"))
            .unwrap();
        let slot = metadata
            .spans
            .iter()
            .rev()
            .find(|span| span.content != "▐")
            .unwrap();
        assert_eq!(
            slot.content, expected,
            "status={status:?}, quiet={quiet}, ceiling={ceiling}"
        );
        let style = if status == AgentStatus::Running && quiet >= 300 {
            activity_age_style(&theme, quiet as i64, i64::from(ceiling))
        } else {
            theme.muted()
        };
        assert_eq!(slot.style.fg, style.fg);
        if quiet > u64::from(ceiling) && status == AgentStatus::Running {
            assert_eq!(slot.style.fg, theme.alarm(Modifier::empty()).fg);
        }
    }
}

#[test]
fn child_clock_shapes_survive_nerd_font_and_no_color() {
    let parent = agent(
        "root",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        None,
        None,
    );
    let mut child = agent(
        "child",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("Explore"),
    );
    child.parent_agent_id = Some("root".into());
    child.subagent_started_at = Some(fixed_now() - Duration::from_secs(2 * 3_600));
    child.model = Some("Haiku".to_owned());
    for (modern, no_color, name) in [
        (true, false, "child_clocks_nerd_font"),
        (true, true, "child_clocks_nerd_font_no_color"),
        (false, true, "child_clocks_no_color"),
    ] {
        let mut frame = Vec::new();
        for quiet in [0, 480, 2_400] {
            child.last_activity = fixed_now() - Duration::from_secs(quiet);
            let mut snapshot = snapshot_with(vec![parent.clone(), child.clone()]);
            snapshot.theme.style = modern.then_some(crate::config::ThemeStyle::Modern);
            let theme = Theme::fixed_for_theme(no_color, &snapshot.theme);
            let lines = group_lines(&snapshot, &theme, 0);
            let text = line_texts(&lines).join("\n");
            let expected = if quiet == 0 {
                "   2h".to_owned()
            } else {
                format!(
                    "{} {:>3}",
                    elapsed_glyph(&theme, quiet as i64, 1_800),
                    if quiet == 480 { "8m" } else { "40m" }
                )
            };
            assert!(text.contains(&format!("{expected}▐")), "{text}");
            frame.push(text);
        }
        assert_snapshot(name, frame.join("\n\n"));
    }
}

#[test]
fn paused_child_parks_parent_head_and_pet() {
    let parent = agent(
        "parent",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("delegate"),
    );
    let mut child = agent(
        "child",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("review"),
    );
    child.parent_agent_id = Some("parent".into());
    child.launch_depth = Some(1);
    child.name = Some("child".to_owned());
    child.last_activity = fixed_now() - Duration::from_secs(1);
    let mut context = crate::agents::AgentContext::new("claude", fixed_now());
    context.turn_error = Some(crate::agents::AgentTurnError {
        class: crate::agents::TurnErrorClass::PausedRateLimit,
        at: fixed_now(),
        label: Some("usage limit reached".to_owned()),
    });
    child.context = Some(context);
    let snapshot = snapshot_with(vec![parent, child]);
    let ui = ViewState {
        selected_index: Some(0),
        ..Default::default()
    };
    let pane_ui = UiState::default();
    assert_eq!(
        selected_pet_action(&snapshot, &ui),
        crate::sidebar_pane::pets::PetAction::Waiting
    );
    assert_ne!(
        animation_cadence_for_test(&snapshot),
        AnimationCadence::Fast
    );
    let bytes = snapshot_to_bytes_with_alert_and_ui(&snapshot, None, &ui, &pane_ui, 54, 28);
    let mut parser = vt100::Parser::new(28.try_into().unwrap(), 54.try_into().unwrap(), 0);
    parser.process(&bytes);
    let screen = parser.screen().contents();
    let parent_line = screen.lines().find(|line| line.contains("claude")).unwrap();
    assert!(parent_line.contains("⏸"), "{screen}");
    assert!(!parent_line.contains("⢄"), "{screen}");
    let (line_index, line) = screen
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains("@child: usage limit reached"))
        .unwrap();
    let col = line.chars().position(|ch| ch == '@').unwrap();
    assert!(
        parser
            .screen()
            .cell(line_index as u16, col as u16)
            .unwrap()
            .italic()
    );
}

#[test]
fn paused_child_headline_stays_in_live_band() {
    let parent = agent(
        "parent",
        "claude",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        Some("delegate"),
    );
    let mut paused = agent("paused", "claude", AgentStatus::Running, None, None, None);
    paused.parent_agent_id = Some("parent".into());
    paused.launch_depth = Some(1);
    paused.profile = Some("review".to_owned());
    paused.registered_at = Some(fixed_now() - Duration::from_secs(100));
    paused.last_activity = fixed_now() - Duration::from_secs(10);
    paused.description = Some("hidden task".to_owned());
    let mut context = crate::agents::AgentContext::new("claude", fixed_now());
    context.turn_error = Some(crate::agents::AgentTurnError {
        class: crate::agents::TurnErrorClass::PausedRateLimit,
        at: fixed_now(),
        label: Some("usage limit\nreached".to_owned()),
    });
    paused.context = Some(context);
    let mut running = paused.clone();
    running.agent_id = "running".into();
    running.profile = Some("build".to_owned());
    running.registered_at = Some(fixed_now() - Duration::from_secs(50));
    running.context = None;
    let snapshot = snapshot_with(vec![parent, paused, running]);
    let ui = ViewState {
        selected_index: Some(0),
        ..Default::default()
    };
    let pane_ui = UiState::default();
    for (width, name) in [
        (54, "paused_child_entry"),
        (28, "paused_child_entry_narrow"),
    ] {
        let bytes = snapshot_to_bytes_with_alert_and_ui(&snapshot, None, &ui, &pane_ui, width, 28);
        let mut parser = vt100::Parser::new(28.try_into().unwrap(), width.try_into().unwrap(), 0);
        parser.process(&bytes);
        let screen = parser.screen().contents();
        let (line_index, line) = screen
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("review ·"))
            .unwrap();
        assert!(line.contains("⏸"), "{screen}");
        assert!(line.contains("usage"), "{screen}");
        let col = line.chars().position(|ch| ch == 'u').unwrap();
        assert!(
            parser
                .screen()
                .cell(line_index as u16, col as u16)
                .unwrap()
                .italic()
        );
        assert!(screen.find("review").unwrap() < screen.find("build").unwrap());
        if width == 54 {
            assert!(line.contains("usage limit reached"));
        } else {
            assert!(!line.contains("usage limit reached"));
        }
        assert_snapshot(name, screen);
    }
}

#[test]
fn delegation_bands_keep_live_children_and_fold_older_ones() {
    let mut parent = agent(
        "parent",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("delegate"),
    );
    parent.user_turn_started_at = Some(fixed_now() - Duration::from_secs(3_600));
    let mut agents = vec![parent];
    // Spawn order task-0..task-8. task-1 and task-4 still run; the finished
    // children landed out of spawn order, six inside the 15m window (the cap
    // keeps five) and one outside it.
    let landed_secs_ago = [300, 0, 60, 780, 0, 600, 1_200, 180, 420];
    for (index, ago) in landed_secs_ago.into_iter().enumerate() {
        let running = matches!(index, 1 | 4);
        let mut child = agent(
            &format!("child-{index}"),
            "claude",
            if running {
                AgentStatus::Running
            } else {
                AgentStatus::Success
            },
            None,
            None,
            Some(&format!("task-{index}")),
        );
        child.parent_agent_id = Some("parent".into());
        child.registered_at = Some(fixed_now() - Duration::from_secs(1_300 - index as u64));
        child.last_activity = fixed_now() - Duration::from_secs(if running { 5 } else { ago });
        child.last_seen = child.last_activity;
        agents.push(child);
    }
    let mut snapshot = snapshot_with(agents);
    // A pending wait sits between the recent band and the folded tail, so the
    // history rows must land after it.
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)
        [0]
    .as_agent_mut()
    .unwrap()
    .pending_waits
    .push(PendingWait {
        class: crate::agents::PendingWaitClass::Sleeping,
        name: "deploy".to_owned(),
        trigger: PendingWaitTrigger::Signal {
            selector: "deploy.done".to_owned(),
        },
        armed_at: Some(fixed_now()),
    });
    let row = &snapshot.worktree_groups[0].rows[0];
    let turn = row.as_agent().unwrap().user_turn_started_at;
    let entries = |rendered: &str| {
        rendered
            .lines()
            .filter_map(|line| {
                line.split_whitespace()
                    .find(|word| word.starts_with("task-"))
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>()
    };
    let mut screens = Vec::new();
    for (selected_index, open, history, name) in [
        (0, None, false, "delegation_bands_selected"),
        (0, None, true, "delegation_bands_history"),
        (usize::MAX, None, false, "delegation_bands_unselected"),
        (0, Some(false), false, "delegation_bands_override_closed"),
    ] {
        let mut ui = ViewState {
            selected_index: Some(selected_index),
            ..Default::default()
        };
        let pane_ui = UiState::default();
        if let Some(open) = open {
            ui.delegation_overrides.insert(row.id.clone(), open);
        }
        if history {
            ui.delegation_history.insert(row.id.clone(), turn);
        }
        let rendered = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, &pane_ui, 54, 44);
        assert!(rendered.contains("subagents (9)"), "{rendered}");
        assert_snapshot(name, rendered.clone());
        screens.push(rendered);
    }
    let [selected, history, unselected, closed] = screens.as_slice() else {
        unreachable!("four scenarios");
    };
    assert_eq!(
        entries(selected),
        [
            "task-1", "task-4", "task-2", "task-7", "task-0", "task-8", "task-5"
        ]
    );
    assert!(selected.contains("+2 older"), "{selected}");
    let tail = selected
        .lines()
        .position(|line| line.contains("+2 older"))
        .expect("folded tail");
    assert!(
        selected
            .lines()
            .take(tail)
            .any(|line| line.contains("deploy"))
    );
    assert!(
        history.lines().take(tail).eq(selected.lines().take(tail)),
        "history must only append below the tail:\n{selected}\n{history}"
    );
    assert_eq!(&entries(history)[7..], ["task-3", "task-6"]);
    assert!(!history.contains("older"), "{history}");
    for screen in [unselected, closed] {
        assert!(entries(screen).is_empty(), "{screen}");
        assert!(!screen.contains("older"), "{screen}");
    }
}

#[test]
fn render_selected_card_keeps_finished_metadata_and_frozen_runtime() {
    // Finished and running children keep metadata and plain runtime, not a landed age.
    let mut parent = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    parent.context = Some(claude_context(fixed_now()));

    let mut child = agent(
        "child-1",
        "claude",
        AgentStatus::Success,
        None,
        None,
        Some("Explore"),
    );
    child.parent_agent_id = Some("claude-1".into());
    child.subagent_description = Some("locate the render seam".to_owned());
    child.subagent_cost_usd = Some(0.42);
    child.subagent_started_at = Some(fixed_now() - Duration::from_secs(90));
    child.last_activity = fixed_now() - Duration::from_secs(30);
    child.last_seen = fixed_now() - Duration::from_secs(30);
    child.usage.fresh_input_tokens = Some(12_400);
    // A bare model id — the renderer prettifies it through `model_label_parts`.
    child.model = Some("claude-opus-4-8".to_owned());
    // Claude reports the child's effort on its `SubagentStop`.
    child.effort = Some("high".to_owned());
    child.compaction_count = 2;

    let mut fresh = agent(
        "child-2",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("review"),
    );
    fresh.parent_agent_id = Some("claude-1".into());
    // Mid-reasoning: the child's leading cell is the thinking orbit (frame 0
    // of the animation at the test's fixed phase), not the static `⢿`.
    fresh.phase = crate::agents::TurnPhase::Reasoning;
    fresh.subagent_description = Some("audit the trust hash".to_owned());
    fresh.subagent_started_at = Some(fixed_now() - Duration::from_secs(30));
    fresh.usage.run_total_tokens = Some(3_100);
    // A sibling on a different model — the per-child label tells them apart.
    fresh.model = Some("claude-haiku-4-5".to_owned());

    let snapshot = snapshot_with(vec![parent, child, fresh]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &ViewState {
            selected_index: Some(0),
            ..Default::default()
        },
        &UiState::default(),
        54,
        23,
    );

    assert!(
        rendered.contains("⧉ subagents (2)"),
        "the expanded card lists its children:\n{rendered}"
    );
    assert!(
        rendered.contains("Explore · locate the render seam"),
        "the finished child keeps its type line:\n{rendered}"
    );
    let priced_line = rendered
        .lines()
        .find(|line| line.contains("Explore · locate the render seam"))
        .expect("priced child line");
    assert!(
        priced_line.ends_with(" $0.42▐") && !priced_line.contains('◔'),
        "the exact child cost pins right on line 1, the clock stays off it:\n{rendered}"
    );
    // The running child's leading cell is the thinking orbit (frame 0 at the
    // test's fixed animation phase), the agent-row head vocabulary verbatim.
    assert!(
        rendered.contains("⠁ review · audit the trust hash"),
        "a reasoning child wears the thinking head:\n{rendered}"
    );
    assert!(
        rendered.contains("◇  3k · Haiku 4.5"),
        "the running child carries its token spend and model:\n{rendered}"
    );
    assert!(
        rendered
            .lines()
            .any(|line| line.contains("Haiku 4.5") && line.ends_with("  <1m▐")),
        "the running child reads plain sub-minute runtime:\n{rendered}"
    );
    assert!(
        rendered
            .lines()
            .any(|line| line.contains("▤ 12k · Opus 4.8") && line.contains("· high")),
        "the finished child retains exact metadata:\n{rendered}"
    );
    let finished_line = rendered
        .lines()
        .find(|line| line.contains("▤ 12k"))
        .expect("finished child metadata line");
    assert!(finished_line.contains("↻ 2"), "{finished_line}");
    assert!(
        finished_line.ends_with("   1m▐"),
        "the finished child's frozen runtime pins right on line 2:\n{rendered}"
    );
    // Both metadata-bearing children render a second line.
    let subagent_metadata_rows = rendered
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            (line.contains("◇") || line.contains("▤"))
                && line.starts_with("▌      ")
                && !trimmed.starts_with("W:")
                && !trimmed.starts_with("M:")
        })
        .count();
    assert_eq!(
        subagent_metadata_rows, 2,
        "both metadata-bearing children carry a token row:\n{rendered}"
    );
    assert_snapshot("subagent_two_line_entry", rendered);
}

#[test]
fn launched_subagent_renders_profile_cost_and_parent_rollup() {
    let mut parent = agent(
        "claude-root",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("delegate"),
    );
    let mut parent_context = claude_context(fixed_now());
    parent_context.cost = Some(crate::agents::AgentCost {
        total_cost_usd: Some(0.10),
        ..crate::agents::AgentCost::default()
    });
    parent.context = Some(parent_context);
    let mut child = agent(
        "codex-child",
        "codex",
        AgentStatus::Success,
        None,
        None,
        Some("map sidebar"),
    );
    child.name = Some("helper".to_owned());
    child.parent_agent_id = Some(parent.agent_id.clone());
    child.parent_agent_kind = Some(parent.kind.clone());
    child.launch_depth = Some(1);
    child.profile = Some("explorer".to_owned());
    child.description = Some("map sidebar".to_owned());
    child.usage.fresh_input_tokens = Some(12_000);
    let mut child_context = crate::agents::AgentContext::new("codex", fixed_now());
    child_context.cost = Some(crate::agents::AgentCost {
        total_cost_usd: Some(0.42),
        ..crate::agents::AgentCost::default()
    });
    child_context.tokens = Some(crate::agents::AgentTokenUsage {
        session_usage: Some(crate::agents::AgentSessionUsage {
            input_tokens: Some(30_000),
            output_tokens: Some(5_000),
            cache_creation_input_tokens: Some(2_000),
            cache_read_input_tokens: Some(200_000),
            ..crate::agents::AgentSessionUsage::default()
        }),
        ..crate::agents::AgentTokenUsage::default()
    });
    child.context = Some(child_context);

    let snapshot = snapshot_with(vec![parent, child]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &ViewState {
            selected_index: Some(0),
            ..Default::default()
        },
        &UiState::default(),
        60,
        20,
    );

    assert!(
        rendered.contains("explorer · map sidebar"),
        "the child line names its profile:\n{rendered}"
    );
    assert!(
        !rendered.contains("helper"),
        "the child card must not render its petname:\n{rendered}"
    );
    assert!(
        rendered
            .lines()
            .any(|line| line.contains("explorer · map sidebar") && line.contains("$0.42")),
        "the child cost pins on its line:\n{rendered}"
    );
    assert!(
        rendered
            .lines()
            .any(|line| line.contains("⧉ subagents (1)") && line.contains("$0.42")),
        "the lifetime stats line carries child cost:\n{rendered}"
    );
    assert!(
        rendered.contains("▤ 12k"),
        "the child metadata prefers window occupancy over session tokens:\n{rendered}"
    );
    assert!(
        rendered.lines().any(|line| line.contains("$0.52")),
        "the parent cost includes delegated spend:\n{rendered}"
    );
}

#[test]
fn subagent_stats_line_outlives_the_turn() {
    let mut parent = agent(
        "claude-root",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("delegate"),
    );
    parent.turn_started_at = Some(fixed_now() - Duration::from_secs(10));
    parent.user_turn_started_at = parent.turn_started_at;

    let mut child = agent(
        "codex-child",
        "codex",
        AgentStatus::Success,
        None,
        None,
        Some("map sidebar"),
    );
    child.parent_agent_id = Some(parent.agent_id.clone());
    child.parent_agent_kind = Some(parent.kind.clone());
    child.launch_depth = Some(1);
    child.last_activity = fixed_now() - Duration::from_secs(60);
    child.last_seen = child.last_activity;
    child.ended_at = Some(child.last_activity);
    let mut context = crate::agents::AgentContext::new("codex", fixed_now());
    context.cost = Some(crate::agents::AgentCost {
        total_cost_usd: Some(0.42),
        ..crate::agents::AgentCost::default()
    });
    child.context = Some(context);

    let snapshot = snapshot_with(vec![parent, child]);
    let theme = Theme::fixed(false);
    let unselected = line_texts(&group_lines(&snapshot, &theme, usize::MAX));
    assert_eq!(unselected.len(), 5, "{}", unselected.join("\n"));
    assert!(
        unselected[4].contains("⧉ subagents (1)") && unselected[4].contains("$0.42"),
        "{}",
        unselected.join("\n")
    );

    // The child finished before the parent's current turn but landed inside
    // the recent window, so the selected card still lists it with its age.
    let selected = line_texts(&group_lines(&snapshot, &theme, 0));
    assert_eq!(selected.len(), 7, "{}", selected.join("\n"));
    assert!(selected[5].contains("⧉ subagents (1)"));
    assert!(selected[6].contains("map sidebar") && selected[6].contains("1m $0.42"));
    assert!(!selected.iter().any(|line| line.contains("older")));

    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &ViewState {
            selected_index: Some(usize::MAX),
            ..Default::default()
        },
        &UiState::default(),
        60,
        20,
    );
    assert_snapshot("subagent_stats_line", rendered);
}

#[test]
fn all_older_children_open_in_one_click_without_a_lone_fold() {
    // Every child landed outside the recent window, so no live or recent row
    // is visible. Opening the section shows the older rows directly rather
    // than a lone `+K older` fold that would cost a second click.
    let mut parent = agent(
        "claude-root",
        "claude",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("delegate"),
    );
    parent.user_turn_started_at = Some(fixed_now() - Duration::from_secs(7_200));
    let mut agents = vec![parent];
    for index in 0..3 {
        let mut child = agent(
            &format!("child-{index}"),
            "claude",
            AgentStatus::Success,
            None,
            None,
            Some(&format!("task-{index}")),
        );
        child.parent_agent_id = Some("claude-root".into());
        child.last_activity = fixed_now() - Duration::from_secs(3_600 + index);
        child.last_seen = child.last_activity;
        agents.push(child);
    }
    let snapshot = snapshot_with(agents);
    let mut ui = ViewState {
        selected_index: Some(0),
        ..Default::default()
    };
    let pane_ui = UiState::default();
    ui.delegation_overrides
        .insert(snapshot.worktree_groups[0].rows[0].id.clone(), true);
    let text = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, &pane_ui, 54, 30);
    assert!(text.contains("subagents (3)"), "{text}");
    assert!(!text.contains("older"), "{text}");
    for index in 0..3 {
        assert!(text.contains(&format!("task-{index}")), "{text}");
    }
}

#[test]
fn engaged_card_without_children_has_no_stats_line() {
    let parent = agent(
        "claude-root",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("delegate"),
    );
    let snapshot = snapshot_with(vec![parent]);
    let rendered = line_texts(&group_lines(&snapshot, &Theme::fixed(false), usize::MAX));

    assert_eq!(rendered.len(), 4, "{}", rendered.join("\n"));
    assert!(!rendered.iter().any(|line| line.contains("⧉ subagents")));
}

#[test]
fn narrow_subagent_line_truncates_description_before_exact_cost() {
    let parent = agent(
        "claude-root",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("delegate"),
    );
    let mut child = agent(
        "claude-child",
        "claude",
        AgentStatus::Success,
        None,
        None,
        Some("Explore"),
    );
    child.parent_agent_id = Some("claude-root".into());
    child.subagent_description =
        Some("trace every caller through the complete rendering pipeline".to_owned());
    child.subagent_cost_usd = Some(0.42);

    let snapshot = snapshot_with(vec![parent, child]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &ViewState {
            selected_index: Some(0),
            ..Default::default()
        },
        &UiState::default(),
        34,
        23,
    );
    let child_line = rendered
        .lines()
        .find(|line| line.contains("Explore"))
        .expect("child line");

    assert!(child_line.contains("$0.42▐"), "{rendered}");
    assert!(
        !child_line.contains("complete rendering pipeline"),
        "description yields width to the exact cost:\n{rendered}"
    );
}

#[test]
fn metadata_free_finished_subagent_stays_one_line() {
    let parent = agent(
        "copilot-root",
        "copilot",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("delegate"),
    );
    let mut child = agent(
        "copilot-child",
        "copilot",
        AgentStatus::Success,
        None,
        None,
        Some("cleanup"),
    );
    child.parent_agent_id = Some("copilot-root".into());
    child.subagent_started_at = Some(fixed_now() - Duration::from_secs(600));
    child.last_activity = fixed_now() - Duration::from_secs(60);
    child.subagent_cost_usd = Some(0.42);

    let snapshot = snapshot_with(vec![parent, child]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &ViewState {
            selected_index: Some(0),
            ..Default::default()
        },
        &UiState::default(),
        54,
        21,
    );
    let lines = rendered.lines().collect::<Vec<_>>();
    let child_line = lines
        .iter()
        .position(|line| line.contains("✓ cleanup"))
        .unwrap_or_else(|| panic!("finished child missing:\n{rendered}"));
    assert!(
        !lines
            .get(child_line + 1)
            .is_some_and(|line| line.starts_with("▌      ")),
        "metadata-free completion stays one line:\n{rendered}"
    );
    assert!(
        lines[child_line].ends_with("   9m $0.42▐") && !lines[child_line].contains('◔'),
        "with no line 2, frozen runtime pins ahead of cost:\n{rendered}"
    );
}
#[test]
fn subagent_metadata_blank_fills_the_per_card_grid() {
    // Window, total, and unknown figures share one grid; the token-less child's model stays aligned without a bare glyph or orphan seam.
    let mut parent = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    parent.context = Some(claude_context(fixed_now()));

    let mut spender = agent(
        "child-1",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("Explore"),
    );
    spender.parent_agent_id = Some("claude-1".into());
    spender.subagent_started_at = Some(fixed_now() - Duration::from_secs(90));
    spender.usage.fresh_input_tokens = Some(12_400);
    spender.model = Some("claude-opus-4-8".to_owned());
    spender.effort = Some("high".to_owned());

    // A sibling before its first `subagentStatusLine` report: no tokens yet,
    // its model already known from its own lifecycle events.
    let mut quiet = agent(
        "child-2",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("review"),
    );
    quiet.parent_agent_id = Some("claude-1".into());
    quiet.model = Some("claude-haiku-4-5".to_owned());

    let mut total = spender.clone();
    total.agent_id = "child-3".into();
    total.model = Some("claude-sonnet-4-6".to_owned());
    let mut snapshot = snapshot_with(vec![parent, spender, quiet, total]);
    for child in &mut std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .as_agent_mut()
    .unwrap()
    .sub_agents
    {
        child.tokens = match child.id.as_str() {
            "child-1" => Some(crate::store::snapshot::SubAgentTokens::Window(12_400)),
            "child-3" => Some(crate::store::snapshot::SubAgentTokens::Total(3_100)),
            _ => None,
        };
    }
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &ViewState {
            selected_index: Some(0),
            ..Default::default()
        },
        &UiState::default(),
        54,
        27,
    );

    // Anchor each lookup to the child's own metadata line — the parent's
    // identity line also reads `Opus 4.8`.
    let char_col = |line_needle: &str, col_needle: &str| {
        let line = rendered
            .lines()
            .find(|line| line.contains(line_needle))
            .unwrap_or_else(|| panic!("{line_needle:?} missing:\n{rendered}"));
        line[..line.find(col_needle).unwrap()].chars().count()
    };
    assert_eq!(
        char_col("▤ 12k", "Opus 4.8"),
        char_col("Haiku 4.5", "Haiku 4.5"),
        "the token-less child's model starts under its sibling's:\n{rendered}"
    );
    assert_eq!(char_col("▤ 12k", "▤"), char_col("◇  3k", "◇"));
    assert_eq!(char_col("▤ 12k", "k"), char_col("◇  3k", "k"));
    assert_eq!(
        char_col("▤ 12k", "Opus 4.8"),
        char_col("◇  3k", "Sonnet 4.6")
    );
    assert!(
        !rendered.contains("· Haiku 4.5"),
        "a blank-filled token slot carries no orphan seam:\n{rendered}"
    );
    let quiet_line = rendered
        .lines()
        .find(|line| line.contains("Haiku 4.5"))
        .expect("the token-less child still renders its metadata line");
    assert!(
        !quiet_line.contains('◇'),
        "no bare `◇` over a blank figure:\n{rendered}"
    );
}

#[test]
fn subagent_model_column_is_sized_by_display_width() {
    // `模型模型` is 4 chars but 8 columns, `Haiku` 5 chars and 5 columns: the
    // wide name sets the column only when it is measured in columns, so each
    // child's effort starts in the same column only under that measure.
    let parent = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    let mut wide = agent(
        "child-1",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("Explore"),
    );
    wide.parent_agent_id = Some("claude-1".into());
    wide.model = Some("模型模型".to_owned());
    wide.effort = Some("high".to_owned());
    let mut narrow = wide.clone();
    narrow.agent_id = "child-2".into();
    narrow.model = Some("Haiku".to_owned());
    narrow.effort = Some("low".to_owned());
    let snapshot = snapshot_with(vec![parent, wide, narrow]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &ViewState {
            selected_index: Some(0),
            ..Default::default()
        },
        &UiState::default(),
        54,
        27,
    );

    let column = |line_needle: &str, col_needle: &str| {
        let line = rendered
            .lines()
            .find(|line| line.contains(line_needle))
            .unwrap_or_else(|| panic!("{line_needle:?} missing:\n{rendered}"));
        Line::from(&line[..line.find(col_needle).unwrap()]).width()
    };
    assert_eq!(
        column("模型模型", "模型模型"),
        column("Haiku", "Haiku"),
        "both models start in one column:\n{rendered}"
    );
    assert_eq!(
        column("模型模型", "high"),
        column("Haiku", "low"),
        "the efforts stack after a wide model name:\n{rendered}"
    );
}

#[test]
fn codex_subagent_renders_nickname_nested_path_and_current_context() {
    let parent = agent(
        "codex-root",
        "codex",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("ship hooks"),
    );
    let mut child = agent(
        "codex-child",
        "codex",
        AgentStatus::Running,
        None,
        None,
        Some("research/explore_hooks"),
    );
    child.parent_agent_id = Some("codex-root".into());
    child.name = Some("Atlas".to_owned());
    child.name_explicit = true;
    child.usage.fresh_input_tokens = Some(32_100);
    child.model = Some("gpt-5.5-codex".to_owned());
    child.effort = Some("xhigh".to_owned());

    let snapshot = snapshot_with(vec![parent, child]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &ViewState {
            selected_index: Some(0),
            ..Default::default()
        },
        &UiState::default(),
        60,
        20,
    );

    assert!(
        rendered.contains("Atlas · research/explore_hooks"),
        "nickname and flat nested path stay distinct:\n{rendered}"
    );
    assert!(
        rendered.contains("▤ 32k"),
        "Codex's current context reading is rendered:\n{rendered}"
    );
}

#[test]
fn subagent_window_glyph_heats_like_the_parent_context() {
    let parent = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    let tokens = [("child-1", 12_400), ("child-2", 400_000)];
    let children = tokens.map(|(id, _)| {
        let mut child = agent(
            id,
            "claude",
            AgentStatus::Running,
            None,
            None,
            Some("Explore"),
        );
        child.parent_agent_id = Some("claude-1".into());
        child
    });
    let mut snapshot = snapshot_with([vec![parent], children.to_vec()].concat());
    for child in &mut std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .as_agent_mut()
    .unwrap()
    .sub_agents
    {
        let window = tokens
            .iter()
            .find(|(id, _)| *id == child.id)
            .map(|(_, window)| *window);
        child.tokens = window.map(crate::store::snapshot::SubAgentTokens::Window);
    }
    let theme = Theme::fixed(false);
    let lines = group_lines_at_width(&snapshot, &theme, 0, 54);
    let glyph_style = |figure: &str| {
        let line = lines
            .iter()
            .find(|line| line.spans.iter().any(|span| span.content.contains(figure)))
            .unwrap_or_else(|| panic!("{figure} missing"));
        line.spans
            .iter()
            .find(|span| span.content == theme.glyph(GlyphRole::TokensFilled))
            .expect("window glyph")
            .style
            .fg
    };
    assert_eq!(glyph_style("12k"), Some(theme.heat_tone(0.0)));
    assert_eq!(glyph_style("400k"), Some(theme.heat_tone(1.0)));

    for (window, percent) in [(Some(200_000), 95), (None, 0), (Some(0), 0)] {
        for child in &mut std::sync::Arc::make_mut(
            &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
        )[0]
        .as_agent_mut()
        .unwrap()
        .sub_agents
        {
            child.tokens = Some(crate::store::snapshot::SubAgentTokens::Window(190_000));
            child.context_window = window;
        }
        let lines = group_lines_at_width(&snapshot, &theme, 0, 54);
        let glyph = lines
            .iter()
            .find(|line| line.spans.iter().any(|span| span.content.contains("190k")))
            .expect("child token line")
            .spans
            .iter()
            .find(|span| span.content == theme.glyph(GlyphRole::TokensFilled))
            .expect("child window glyph");
        assert_eq!(
            glyph.style.fg,
            Some(crate::sidebar_pane::render::labels::severity_heat_color(
                &theme,
                crate::agents::ContextSeverity::Calm,
                percent,
                Some(190_000),
                &Default::default(),
            )),
            "reported window {window:?}"
        );
    }
}
