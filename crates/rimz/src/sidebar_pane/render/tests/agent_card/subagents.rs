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
    for (status, live, native, reopenable, error) in [
        (AgentStatus::Running, true, false, true, false),
        (AgentStatus::Success, false, false, true, false),
        (AgentStatus::Running, false, true, false, false),
        (AgentStatus::Failed, false, false, true, true),
        (AgentStatus::Success, false, false, false, false),
    ] {
        let mut snapshot = fast_child_snapshot(status, false);
        let child = &mut fast_children_mut(&mut snapshot)[0];
        child.fast = false;
        child.name = "claude".to_owned();
        child.pane = live.then_some(pane.clone());
        child.provider_native = native;
        child.reopenable = reopenable;
        child.description = Some("audit the trust hash".to_owned());
        child.turn_error_label = error.then(|| "limit reached".to_owned());
        let rolls = CostRolls::default();
        let pages = all_paged(&snapshot);
        let mut ctx = test_row_ctx(&snapshot, &theme, 54, 0, 0, &rolls);
        ctx.delegation_pages = &pages;
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
        let target_kind = |line| match block.interactions.target_at(1, line as u16) {
            Some(HitTarget::SubAgentPane(_)) => "pane",
            Some(HitTarget::ReopenSubAgent(_)) => "reopen",
            _ => "none",
        };
        actual.push((
            headline.style.fg,
            headline.style.add_modifier.contains(Modifier::ITALIC),
            target_kind(line_index),
            target_kind(metadata),
        ));
        let inert = !live && (native || !reopenable);
        let expected_fg = if inert {
            Some(theme.component(Component::SubagentInert))
        } else if live || error {
            theme.body().fg
        } else {
            theme.muted().fg
        };
        let expected_target = if live {
            "pane"
        } else if !inert {
            "reopen"
        } else {
            "none"
        };
        expected.push((expected_fg, error, expected_target, expected_target));
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
        let lines = group_lines_paged(&snapshot, &Theme::fixed(false), 0);
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

#[test]
fn compacting_child_draws_the_compacting_head_unless_its_status_wants_attention() {
    use crate::sidebar_pane::render::labels::{agent_glyph, role_glyph};
    let theme = truecolor_sidebar_theme();
    let compacting_tone = theme.animations.natural_color(AnimationRole::Compacting);
    assert!(compacting_tone.is_some());
    for (status, compacting, shows_compacting) in [
        (AgentStatus::Running, true, true),
        (AgentStatus::Success, true, true),
        (AgentStatus::Running, false, false),
        (AgentStatus::Success, false, false),
        (AgentStatus::Waiting, true, false),
        (AgentStatus::Failed, true, false),
        (AgentStatus::Paused, true, false),
    ] {
        let mut snapshot = fast_child_snapshot(status, false);
        let child = &mut fast_children_mut(&mut snapshot)[0];
        child.name = "claude".to_owned();
        child.fast = false;
        child.phase = TurnPhase::Idle;
        child.description = Some("audit the trust hash".to_owned());
        child.compacting = compacting;
        let lines = group_lines_paged(&snapshot, &theme, 0);
        let line = lines
            .iter()
            .find(|line| line.to_string().contains("claude · audit the trust hash"))
            .expect("the child entry line");
        let glyph = if shows_compacting {
            role_glyph(&theme, AnimationRole::Compacting, 0)
        } else {
            agent_glyph(&theme, status, TurnPhase::Idle, 0)
        };
        let lead = line
            .spans
            .iter()
            .find(|span| span.content == glyph)
            .unwrap_or_else(|| panic!("{status:?} compacting={compacting}: {line}"));
        assert_eq!(
            lead.style.fg == compacting_tone,
            shows_compacting,
            "{status:?} compacting={compacting}"
        );
    }
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
    let mut child = agent(
        "child",
        "claude",
        status,
        Some("/repo/main"),
        None,
        Some("worker"),
    );
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
                let pages = all_paged(&snapshot);
                let mut ctx = test_row_ctx(&snapshot, &theme, 54, 0, phase, &cost_rolls);
                ctx.delegation_pages = &pages;
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
            !group_lines_paged(&snapshot, &theme, 0)
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
            parent.context = Some(claude_context(fixed_now()));
            let mut child = agent(
                "child",
                "claude",
                status,
                Some("/repo/main"),
                None,
                Some("Explore"),
            );
            child.parent_agent_id = Some("parent".into());
            child.launch_depth = Some(1);
            child.compaction_count = count;
            child.model = Some("Haiku".to_owned());
            let snapshot = snapshot_with(vec![parent, child]);
            let theme = Theme::fixed(false);
            let lines = group_lines_paged(&snapshot, &theme, 0);
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
        let mut child = agent(
            "child",
            kind,
            AgentStatus::Running,
            Some("/repo/main"),
            None,
            Some(name),
        );
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
            let model_span = spans.iter().find(|span| span.content == model).unwrap();
            if launched {
                assert_eq!(
                    type_word.style.fg,
                    theme.body_brand(theme.provider_brand_tone(kind)).fg,
                    "{kind}"
                );
                assert_eq!(model_span.style.fg, theme.muted().fg);
            } else {
                let inert_fg = Some(theme.component(Component::SubagentInert));
                assert_eq!(type_word.style.fg, inert_fg, "{kind}");
                assert!(type_word.style.add_modifier.contains(Modifier::DIM));
                assert_eq!(model_span.style.fg, inert_fg, "{kind}");
                assert!(model_span.style.add_modifier.contains(Modifier::DIM));
            }
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
    let lines = group_lines_paged(&snapshot, &theme, 0);
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

/// A mark on a launched child's pane paints that child's entry, both its lines, and leaves
/// the parent's lines and the sibling entry as they were.
#[test]
fn a_closing_childs_entry_takes_the_tone_inside_its_parents_card() {
    let snapshot = focused_child_snapshot();
    let theme = Theme::fixed(false);
    let rolls = CostRolls::default();
    let closing = std::collections::HashMap::from([(
        PaneId::from_parts(MuxName::Zellij, "terminal_child"),
        0,
    )]);
    let mut ctx = test_row_ctx(&snapshot, &theme, 54, 0, 0, &rolls);
    let calm = worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None).lines;
    ctx.closing = &closing;
    let marked = worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None).lines;
    assert_eq!(
        marked.iter().map(Line::to_string).collect::<Vec<_>>(),
        calm.iter().map(Line::to_string).collect::<Vec<_>>()
    );
    let entry = marked
        .iter()
        .position(|line| line.to_string().contains("inspect the click seam"))
        .unwrap();
    let changed = (0..calm.len())
        .filter(|&index| calm[index] != marked[index])
        .collect::<Vec<_>>();
    assert_eq!(changed, [entry, entry + 1]);
    let tone = theme.component(Component::CardClosing);
    for text in ["explorer", "inspect the click seam"] {
        let span = marked[entry]
            .spans
            .iter()
            .find(|span| span.content == text)
            .unwrap();
        assert_eq!(span.style.fg, Some(tone), "{text}");
    }
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
        Some("/repo/main"),
        None,
        Some("Explore"),
    );
    child.parent_agent_id = Some("root".into());
    child.launch_depth = Some(1);
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
        let lines = group_lines_paged(&snapshot, &theme, 0);
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
                    elapsed_glyph(
                        &theme,
                        quiet as i64,
                        snapshot.attention.stalled_after_secs.get().into(),
                    ),
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
        model: None,
        ended_turn: false,
        class: crate::agents::TurnErrorClass::PausedRateLimit,
        at: fixed_now(),
        label: Some("usage limit reached".to_owned()),
        refill_at: None,
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
fn waiting_child_shows_question_glyph_and_names_subagent_on_running_parent() {
    let mut parent = agent(
        "parent",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("delegate"),
    );
    parent.open_ask = Some(crate::agents::OpenAsk {
        id: crate::ids::AskId::parse("ask_0123456789abcdef").unwrap(),
        kind: crate::agents::AskKind::Question,
        detail: Some("Stale parent question?".to_owned()),
        extra_questions: 0,
        native_key: None,
        since: fixed_now() - Duration::from_secs(120),
    });
    let mut child = agent(
        "child",
        "claude",
        AgentStatus::Waiting,
        None,
        None,
        Some("review"),
    );
    child.parent_agent_id = Some("parent".into());
    child.launch_depth = Some(1);
    child.name = Some("scout".to_owned());
    child.waiting_since = Some(fixed_now() - Duration::from_secs(5));
    child.last_activity = fixed_now() - Duration::from_secs(5);
    let snapshot = snapshot_with(vec![parent, child]);
    let ui = ViewState {
        selected_index: Some(0),
        ..Default::default()
    };
    let pane_ui = UiState::default();
    let bytes = snapshot_to_bytes_with_alert_and_ui(&snapshot, None, &ui, &pane_ui, 54, 28);
    let mut parser = vt100::Parser::new(28.try_into().unwrap(), 54.try_into().unwrap(), 0);
    parser.process(&bytes);
    let screen = parser.screen().contents();
    let parent_line = screen.lines().find(|line| line.contains("claude")).unwrap();
    assert!(
        parent_line.contains('?'),
        "running parent with waiting child must lead with ?: {screen}"
    );
    assert!(
        !parent_line.contains('⢿') && !parent_line.contains('⢄') && !parent_line.contains('⠁'),
        "running parent with waiting child must not draw a spinner: {screen}"
    );
    assert!(
        screen.contains("@scout"),
        "line 2 must name the waiting subagent: {screen}"
    );
    assert!(
        !screen.contains("Stale parent question?"),
        "line 2 must show no stale question of the parent's own: {screen}"
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
        model: None,
        ended_turn: false,
        class: crate::agents::TurnErrorClass::PausedRateLimit,
        at: fixed_now(),
        label: Some("usage limit\nreached".to_owned()),
        refill_at: None,
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

/// Pin `row`'s delegation section open or closed, as a click on its line does.
fn pin_delegation(ui: &mut ViewState, row: &SidebarRow, open: bool) {
    let turn = row.as_agent().and_then(|agent| agent.user_turn_started_at);
    ui.toggles
        .delegation_overrides
        .insert(row.id.clone(), DelegationPin { open, turn });
}

#[test]
fn delegation_lists_in_flight_children_and_pages_every_finished_one_behind_the_done_line() {
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
    // Spawn order task-0..task-8, all inside the parent's turn. task-1 and
    // task-4 still run; seven finished out of spawn order and wait behind the
    // done line with the prior-turn child.
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
        // The fold stamps a verdict's finish; the paging orders by it.
        child.finished_at = (!running).then_some(child.last_activity);
        child.subagent_cost_usd = Some(0.10);
        agents.push(child);
    }
    // Finished before the parent's turn: retained by the projection and paged with
    // the other finished children, and like them uncounted while its cost prices the line.
    let mut prior = agent(
        "child-prior",
        "claude",
        AgentStatus::Success,
        None,
        None,
        Some("task-prior"),
    );
    prior.parent_agent_id = Some("parent".into());
    prior.registered_at = Some(fixed_now() - Duration::from_secs(5_000));
    prior.last_activity = fixed_now() - Duration::from_secs(4_000);
    prior.last_seen = prior.last_activity;
    prior.subagent_cost_usd = Some(5.0);
    agents.push(prior);
    let mut snapshot = snapshot_with(agents);
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
    assert_eq!(row.as_agent().unwrap().sub_agents.len(), 10);
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
    for (selected_index, open, name) in [
        (0, None, "delegation_bands_selected"),
        (usize::MAX, None, "delegation_bands_unselected"),
        (0, Some(false), "delegation_bands_override_closed"),
    ] {
        let mut ui = ViewState {
            selected_index: Some(selected_index),
            ..Default::default()
        };
        if let Some(open) = open {
            pin_delegation(&mut ui, row, open);
        }
        let rendered =
            snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, &UiState::default(), 54, 44);
        assert!(
            rendered.lines().any(|line| line.contains("subagents (2)")
                && line.trim_end_matches(['▐', ' ']).ends_with(" $5.90")),
            "the line counts the in-flight children and prices every child:\n{rendered}"
        );
        assert!(!rendered.contains("task-prior"), "{rendered}");
        assert_snapshot(name, rendered.clone());
        screens.push(rendered);
    }
    let [selected, unselected, closed] = screens.as_slice() else {
        unreachable!("three scenarios");
    };
    let section_line = |screen: &str| {
        let line = screen
            .lines()
            .find(|line| line.contains("subagents ("))
            .unwrap_or_else(|| panic!("the section line in\n{screen}"));
        line.chars()
            .skip(1)
            .collect::<String>()
            .trim_end_matches(['▐', '🮇', ' '])
            .to_owned()
    };
    assert_eq!(section_line(selected), section_line(unselected));
    assert_eq!(section_line(selected), section_line(closed));
    assert_eq!(entries(selected), ["task-1", "task-4"]);
    let position = |screen: &str, needle: &str| {
        screen
            .lines()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("{needle} in\n{screen}"))
    };
    assert!(position(selected, "task-4") < position(selected, "deploy"));
    assert!(position(selected, "deploy") < position(selected, "▸ +8 done"));
    for screen in [unselected, closed] {
        assert!(entries(screen).is_empty(), "{screen}");
        assert!(!screen.contains("▸ +"), "{screen}");
    }
    // Each page shows the next five finished children, newest finish first, under the
    // in-flight entries and the waits.
    for (pages, listed, line) in [
        (
            1,
            vec![
                "task-1", "task-4", "task-2", "task-7", "task-0", "task-8", "task-5",
            ],
            Some("▸ +3 more"),
        ),
        (
            2,
            vec![
                "task-1",
                "task-4",
                "task-2",
                "task-7",
                "task-0",
                "task-8",
                "task-5",
                "task-3",
                "task-6",
                "task-prior",
            ],
            None,
        ),
    ] {
        let ui = ViewState {
            selected_index: Some(0),
            toggles: PresentationToggles {
                delegation_pages: std::collections::BTreeMap::from([paged(row, pages)]),
                ..Default::default()
            },
            ..Default::default()
        };
        let rendered =
            snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, &UiState::default(), 54, 60);
        assert_eq!(entries(&rendered), listed, "{pages} pages:\n{rendered}");
        assert!(position(&rendered, "deploy") < position(&rendered, "task-2"));
        match line {
            Some(line) => assert!(
                position(&rendered, "task-5") < position(&rendered, line),
                "{rendered}"
            ),
            None => assert!(
                !rendered.contains("more") && !rendered.contains("▸ +"),
                "{rendered}"
            ),
        }
        for removed in ["more finished", "earlier", "later", "more ended"] {
            assert!(!rendered.contains(removed), "{removed}:\n{rendered}");
        }
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
    // A bare model id — the renderer formats it through `ParsedModel`.
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
        &paged_view(&snapshot, 0),
        &UiState::default(),
        54,
        23,
    );

    assert!(
        rendered.contains("⧉ subagents (1)"),
        "the line counts the running child alone:\n{rendered}"
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
    child.lane = Some(
        crate::ids::Lane::own(crate::ids::LaneHome::Trunk, "/repo/main")
            .named(crate::ids::LaneName::dashed("docs")),
    );
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
        &paged_view(&snapshot, 0),
        &UiState::default(),
        60,
        20,
    );

    assert!(
        rendered.contains("explorer#docs · map sidebar"),
        "the child line names its profile and named lane:\n{rendered}"
    );
    assert!(
        !rendered.contains("helper"),
        "the child card must not render its petname:\n{rendered}"
    );
    assert!(
        rendered
            .lines()
            .any(|line| line.contains("explorer#docs · map sidebar") && line.contains("$0.42")),
        "the child cost pins on its line:\n{rendered}"
    );
    assert!(
        rendered.lines().any(|line| line.contains("⧉ subagents ")
            && line
                .trim_end_matches(['▐', '🮇', ' '])
                .ends_with("✓ done (1)  $0.42")),
        "the done title prices the finished child after its count:\n{rendered}"
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

fn prior_turn_child_snapshot(parent_status: AgentStatus) -> SidebarSnapshot {
    let mut parent = agent(
        "claude-root",
        "claude",
        parent_status,
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
    child.finished_at = Some(child.last_activity);
    child.ended_at = Some(child.last_activity);
    let mut context = crate::agents::AgentContext::new("codex", fixed_now());
    context.cost = Some(crate::agents::AgentCost {
        total_cost_usd: Some(0.42),
        ..crate::agents::AgentCost::default()
    });
    child.context = Some(context);
    snapshot_with(vec![parent, child])
}

#[test]
fn prior_turn_children_leave_the_closed_card_while_its_cost_keeps_them() {
    let snapshot = prior_turn_child_snapshot(AgentStatus::Running);
    let row = &snapshot.worktree_groups[0].rows[0];
    assert!(row.as_agent().unwrap().sub_agents[0].prior_turn);
    let theme = Theme::fixed(false);
    for selected in [usize::MAX, 0] {
        let lines = line_texts(&group_lines(&snapshot, &theme, selected));
        let text = lines.join("\n");
        // Only the selected card titles its finished child, uncounted and priced, and lists
        // it unasked.
        assert_eq!(text.contains("map sidebar"), selected == 0, "{text}");
        assert_eq!(
            lines.iter().any(|line| line.contains("⧉ subagents")
                && line
                    .trim_end_matches(['▐', '🮇', ' '])
                    .ends_with("✓ done (1)  $0.42")),
            selected == 0,
            "{text}"
        );
        assert!(!text.contains("▸ +"), "{text}");
        // The parent's own cost still carries the launched child's spend.
        assert!(lines[1].contains("$0.42"), "{text}");
    }
    for open in [true, false] {
        let mut ui = ViewState {
            selected_index: Some(0),
            ..Default::default()
        };
        pin_delegation(&mut ui, row, open);
        let rendered =
            snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, &UiState::default(), 60, 20);
        assert!(
            rendered.contains("⧉ subagents") && rendered.contains("✓ done (1)"),
            "{rendered}"
        );
        assert_eq!(rendered.contains("map sidebar"), open, "{rendered}");
    }
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
fn an_open_pin_holds_a_quiet_card_open_only_while_its_section_has_an_entry() {
    let mut snapshot = prior_turn_child_snapshot(AgentStatus::Success);
    snapshot.theme.display.stale_after_secs = 5;
    let row = snapshot.rows_mut().next().unwrap();
    row.unread = false;
    row.last_activity = fixed_now() - Duration::from_secs(600);
    let row = snapshot.worktree_groups[0].rows[0].clone();
    let draw = |snapshot: &SidebarSnapshot, pinned: bool| {
        let mut ui = ViewState::default();
        if pinned {
            pin_delegation(&mut ui, &row, true);
        }
        snapshot_to_screen_with_alert_and_ui(snapshot, None, &ui, &UiState::default(), 60, 20)
    };
    let stale = draw(&snapshot, false);
    assert!(!stale.contains("subagents"), "{stale}");
    // A prior-turn child is an entry: the pin opens the section and holds the card open.
    let open = draw(&snapshot, true);
    assert!(
        open.contains("✓ done (1)") && open.contains("map sidebar"),
        "{open}"
    );
    assert_ne!(open, stale);
    // With nothing in the section, the pin opens nothing and the card goes stale.
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)
        [0]
    .as_agent_mut()
    .unwrap()
    .sub_agents
    .clear();
    let empty_stale = draw(&snapshot, false);
    assert_eq!(draw(&snapshot, true), empty_stale);
    assert!(!empty_stale.contains("subagents"), "{empty_stale}");
}

#[test]
fn waits_keep_the_line_and_entries_without_a_current_child() {
    let mut snapshot = prior_turn_child_snapshot(AgentStatus::Running);
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
    let lines = line_texts(&group_lines(&snapshot, &Theme::fixed(false), 0));
    let text = lines.join("\n");
    assert!(
        text.contains("  ⧖ waits (1)") && !text.contains('⧉'),
        "{text}"
    );
    assert!(
        text.contains("deploy") && !text.contains("map sidebar"),
        "{text}"
    );
    assert!(lines.last().unwrap().contains("▸ +1 done"), "{text}");
}

#[test]
fn resting_unreached_cards_count_and_price_only_an_in_flight_current_child() {
    for (status, running) in [
        (AgentStatus::Idle, true),
        (AgentStatus::Success, true),
        (AgentStatus::Success, false),
    ] {
        let mut snapshot = prior_turn_child_snapshot(status);
        let row = &mut std::sync::Arc::make_mut(
            &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
        )[0];
        let card = row.as_agent_mut().unwrap();
        card.sub_agents[0].prior_turn = false;
        if running {
            card.sub_agents[0].status = AgentStatus::Running;
            card.sub_agents[0].finished_at = None;
        }
        let lines = line_texts(&group_lines(&snapshot, &Theme::fixed(false), usize::MAX));
        let text = lines.join("\n");
        // A child finished in the current turn leaves the line and its spend with it.
        assert_eq!(
            lines
                .iter()
                .any(|line| line.contains("⧉ subagents (1)") && line.contains("$0.42")),
            running,
            "{status:?}:\n{text}"
        );
        assert_eq!(text.contains('⧉'), running, "{status:?}:\n{text}");
        assert!(!text.contains("map sidebar"), "{status:?}:\n{text}");
    }
}

#[test]
fn prior_turn_children_do_not_widen_the_entry_grid() {
    let mut parent = agent(
        "parent",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("delegate"),
    );
    parent.user_turn_started_at = Some(fixed_now() - Duration::from_secs(600));
    let mut current = agent(
        "child-now",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("task-now"),
    );
    current.parent_agent_id = Some("parent".into());
    current.model = Some("claude-haiku-4-5".to_owned());
    current.last_activity = fixed_now() - Duration::from_secs(60);
    current.last_seen = current.last_activity;
    let mut prior = agent(
        "child-prior",
        "claude",
        AgentStatus::Success,
        None,
        None,
        Some("task-prior"),
    );
    prior.parent_agent_id = Some("parent".into());
    prior.model = Some("claude-opus-4-8".to_owned());
    prior.usage.fresh_input_tokens = Some(1_234_000);
    prior.last_activity = fixed_now() - Duration::from_secs(1_200);
    prior.last_seen = prior.last_activity;
    let theme = Theme::fixed(false);
    let alone = line_texts(&group_lines(
        &snapshot_with(vec![parent.clone(), current.clone()]),
        &theme,
        0,
    ));
    let beside = line_texts(&group_lines(
        &snapshot_with(vec![parent, current, prior]),
        &theme,
        0,
    ));
    assert_eq!(beside.len(), alone.len() + 1);
    assert_eq!(&beside[..alone.len()], alone.as_slice());
    assert!(beside.last().unwrap().contains("▸ +1 done"));
    assert!(alone.iter().any(|line| line.contains("Haiku 4.5")));
}

#[test]
fn an_open_card_with_nothing_in_flight_lists_its_current_finished_children_unasked() {
    // Every child finished inside the parent's turn: the open section titles them with
    // their done count and lists the first page with no activation.
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
    let row = &snapshot.worktree_groups[0].rows[0];
    pin_delegation(&mut ui, row, true);
    let unpaged = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, &pane_ui, 54, 30);
    assert!(
        unpaged.contains("⧉ subagents") && unpaged.contains("✓ done (3)"),
        "{unpaged}"
    );
    assert!(!unpaged.contains("subagents ("), "{unpaged}");
    assert!(!unpaged.contains("▸ +"), "{unpaged}");
    for index in 0..3 {
        assert!(unpaged.contains(&format!("task-{index}")), "{unpaged}");
    }
    ui.toggles.delegation_pages = std::collections::BTreeMap::from([paged(row, 1)]);
    let text = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, &pane_ui, 54, 30);
    assert_eq!(text, unpaged, "the first page is the one shown unasked");
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
        &paged_view(&snapshot, 0),
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
        &paged_view(&snapshot, 0),
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
            Some("/repo/main"),
            None,
            Some("Explore"),
        );
        child.parent_agent_id = Some("claude-1".into());
        child.launch_depth = Some(1);
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

#[test]
fn parked_launched_child_stays_unfinished_with_background_marker_and_frozen_clock() {
    let mut parent = agent(
        "parent",
        "claude",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        Some("delegate background check"),
    );
    parent.user_turn_started_at = Some(fixed_now() - Duration::from_secs(3_600));

    let mut finished = agent(
        "child-done",
        "claude",
        AgentStatus::Success,
        None,
        None,
        Some("review"),
    );
    finished.parent_agent_id = Some("parent".into());
    finished.launch_depth = Some(1);
    finished.profile = Some("reviewer".to_owned());
    finished.subagent_description = Some("checked diff".to_owned());
    finished.subagent_started_at = Some(fixed_now() - Duration::from_secs(120));
    finished.finished_at = Some(fixed_now() - Duration::from_secs(30));
    finished.last_activity = fixed_now() - Duration::from_secs(30);
    finished.last_seen = finished.last_activity;
    finished.registered_at = Some(fixed_now() - Duration::from_secs(120));
    finished.model = Some("claude-haiku-4-5".to_owned());
    finished.usage.fresh_input_tokens = Some(8_000);
    finished.subagent_cost_usd = Some(0.12);

    let mut parked = agent(
        "child-parked",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("worker"),
    );
    parked.parent_agent_id = Some("parent".into());
    parked.launch_depth = Some(1);
    parked.profile = Some("coder".to_owned());
    parked.phase = TurnPhase::Parked;
    parked.subagent_description = Some("build in background".to_owned());
    parked.subagent_started_at = Some(fixed_now() - Duration::from_secs(600));
    parked.finished_at = Some(fixed_now() - Duration::from_secs(540));
    parked.last_activity = fixed_now() - Duration::from_secs(540);
    parked.last_seen = parked.last_activity;
    parked.registered_at = Some(fixed_now() - Duration::from_secs(600));
    parked.model = Some("claude-sonnet-4-6".to_owned());
    parked.usage.fresh_input_tokens = Some(14_000);
    parked.subagent_cost_usd = Some(0.24);
    parked.pane = Some(crate::pane::PaneRef::from_id(
        "zellij:terminal_parked".parse().unwrap(),
    ));

    let snapshot = snapshot_with(vec![parent, finished, parked]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &paged_view(&snapshot, 0),
        &UiState::default(),
        54,
        24,
    );
    let lines: Vec<_> = rendered.lines().collect();
    let parked_idx = lines
        .iter()
        .position(|line| line.contains("coder"))
        .unwrap_or_else(|| panic!("missing parked child:\n{rendered}"));
    let finished_idx = lines
        .iter()
        .position(|line| line.contains("reviewer"))
        .unwrap_or_else(|| panic!("missing finished child:\n{rendered}"));
    assert!(
        parked_idx < finished_idx,
        "parked child must precede finished children:\n{rendered}"
    );
    assert!(
        lines[parked_idx].contains("✓ coder · build in background  ⋯ bg"),
        "expected parked marker on line 1:\n{rendered}"
    );
    assert!(
        lines[parked_idx + 1].contains("1m"),
        "expected frozen 1m clock on line 2:\n{rendered}"
    );
    assert_snapshot("parked_launched_child", rendered);
}

#[test]
fn parked_child_marker_covers_turn_error_focus_no_description_and_drop_widths() {
    let theme = Theme::fixed(false);
    let pane: PaneId = "zellij:terminal_parked".parse().unwrap();

    // Marker beside a turn-error headline, on a focused entry, and with no description.
    let mut cases = Vec::new();
    for (case, status, description, turn_error, focused) in [
        (
            "turn_error",
            AgentStatus::Success,
            Some("ignored"),
            Some("rate limited"),
            false,
        ),
        (
            "focused",
            AgentStatus::Success,
            Some("run checks"),
            None,
            true,
        ),
        ("no_desc", AgentStatus::Success, None, None, false),
        (
            "running_parked",
            AgentStatus::Running,
            Some("shell runs"),
            None,
            false,
        ),
        (
            "sleeping_parked",
            AgentStatus::Sleeping,
            Some("waits timer"),
            None,
            false,
        ),
    ] {
        let mut snapshot = fast_child_snapshot(status, false);
        let child = &mut fast_children_mut(&mut snapshot)[0];
        child.name = "coder".to_owned();
        child.fast = false;
        child.phase = TurnPhase::Parked;
        child.pane = Some(pane.clone());
        child.description = description.map(str::to_owned);
        child.task = None;
        child.first_prompt = None;
        child.turn_error_label = turn_error.map(str::to_owned);
        let rolls = CostRolls::default();
        let cursor = EntryCursor::SubAgent(pane.clone());
        let mut ctx = test_row_ctx(&snapshot, &theme, 54, 0, 0, &rolls);
        if focused {
            ctx.entry_cursor = Some(&cursor);
        }
        let block = worktree_group_block(&ctx, &snapshot.worktree_groups[0], true, None);
        let line = block
            .lines
            .iter()
            .find(|line| line.to_string().contains("coder"))
            .unwrap_or_else(|| panic!("{case}: missing child line"));
        let text = line.to_string();
        let marker_fg = line
            .spans
            .iter()
            .find(|span| span.content.contains("⋯ bg"))
            .map(|span| span.style.fg);
        cases.push((
            case,
            text.contains("⋯ bg"),
            marker_fg == Some(theme.styled(Component::EntryParkedBg, Modifier::empty()).fg),
            focused == text.contains(theme.glyph(GlyphRole::ChromeSpineEntry)),
            case != "no_desc" || !text.contains("·"),
            case != "turn_error" || text.contains("rate limited  ⋯ bg"),
        ));
    }
    assert_eq!(
        cases,
        vec![
            ("turn_error", true, true, true, true, true),
            ("focused", true, true, true, true, true),
            ("no_desc", true, true, true, true, true),
            ("running_parked", true, true, true, true, true),
            ("sleeping_parked", true, true, true, true, true),
        ]
    );

    let render_line = |width: usize, description: Option<&str>| {
        let mut snapshot = fast_child_snapshot(AgentStatus::Success, false);
        let child = &mut fast_children_mut(&mut snapshot)[0];
        child.name = "coder".to_owned();
        child.fast = false;
        child.phase = TurnPhase::Parked;
        child.pane = Some(pane.clone());
        child.description = description.map(str::to_owned);
        child.task = None;
        child.first_prompt = None;
        child.cost_usd = None;
        let rolls = CostRolls::default();
        let ctx = test_row_ctx(&snapshot, &theme, width, 0, 0, &rolls);
        let block = worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None);
        block
            .lines
            .iter()
            .find(|line| line.to_string().contains("coder"))
            .unwrap_or_else(|| panic!("width {width}: missing child line"))
            .to_string()
    };

    // Content width is the pane's minus 2. The lead and who take 11 cells, the seam 3
    // and the marker 6: the marker needs a pane of 19, a headline beside it one of 24
    // for its first character and `…`, and a headline that fills its room draws whole.
    let long = Some("a very long background build description");
    let seen: Vec<_> = [
        (18, None),
        (19, None),
        (20, None),
        (18, long),
        (19, long),
        (20, long),
        (23, long),
        (24, long),
        (25, long),
        (23, Some("ab")),
        (24, Some("ab")),
        (23, Some("a")),
    ]
    .into_iter()
    .map(|(width, description)| {
        // The line as drawn between the card's two spine cells.
        let line = render_line(width, description);
        let spine = line.char_indices().last().map_or(0, |(at, _)| at);
        let lead = line.find('✓').unwrap_or(0);
        (width, line[lead..spine].trim_end().to_owned())
    })
    .collect();
    assert_eq!(
        seen,
        [
            (18, "✓ coder"),
            (19, "✓ coder  ⋯ bg"),
            (20, "✓ coder  ⋯ bg"),
            (18, "✓ coder · a"),
            (19, "✓ coder  ⋯ bg"),
            (20, "✓ coder  ⋯ bg"),
            (23, "✓ coder  ⋯ bg"),
            (24, "✓ coder · a…  ⋯ bg"),
            (25, "✓ coder · a …  ⋯ bg"),
            (23, "✓ coder  ⋯ bg"),
            (24, "✓ coder · ab  ⋯ bg"),
            (23, "✓ coder · a  ⋯ bg"),
        ]
        .map(|(width, line)| (width, line.to_owned()))
    );
}

fn prior_child(
    id: &str,
    name: &str,
    status: AgentStatus,
    rested_at: Timestamp,
    reopenable: bool,
    provider_native: bool,
) -> crate::store::snapshot::SidebarSubAgent {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "kind": "claude",
        "name": name,
        "status": status.as_str(),
        "last_activity": rested_at,
        "description": format!("task {id}"),
        "model": "Haiku",
        "prior_turn": true,
        "reopenable": reopenable,
        "rested_at": rested_at,
        "provider_native": provider_native,
    }))
    .unwrap()
}

#[test]
fn the_done_title_lists_a_first_page_and_each_activation_shows_five_more_until_none_remain() {
    let mut snapshot = snapshot_with(vec![agent(
        "parent",
        "claude",
        AgentStatus::Idle,
        Some("/repo/main"),
        None,
        Some("build feature"),
    )]);
    let base = snapshot.now;
    let row = &mut std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0];
    let row_id = row.id.clone();
    let card = row.as_agent_mut().unwrap();
    card.prompt = Some("build feature".to_owned());
    card.user_turn_started_at = Some(base);
    // 12 prior children with distinct rest instants out of order; newest first they run
    // p12, p11, ..., p01.
    card.sub_agents = (1..=12)
        .map(|n| {
            let secs = if n % 2 == 0 { n * 10 } else { n * 10 - 5 };
            let rested = Timestamp::from_second(base.as_second() - 1_000 + secs).unwrap();
            prior_child(
                &format!("p{n:02}"),
                &format!("worker-{n:02}"),
                if n == 11 {
                    AgentStatus::Failed
                } else {
                    AgentStatus::Success
                },
                rested,
                true,
                false,
            )
        })
        .collect();
    let row = snapshot.worktree_groups[0].rows[0].clone();

    let theme = Theme::fixed(false);
    let rolls = CostRolls::default();
    let block_at = |pages: usize| {
        let map = std::collections::BTreeMap::from_iter((pages > 0).then(|| paged(&row, pages)));
        let mut ctx = test_row_ctx(&snapshot, &theme, 54, 0, 0, &rolls);
        ctx.delegation_pages = &map;
        let block = worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None);
        let text: Vec<_> = block.lines.iter().map(Line::to_string).collect();
        (block, text)
    };
    let workers = |text: &[String]| -> Vec<usize> {
        let mut listed: Vec<(usize, usize)> = (1..=12)
            .filter_map(|n| {
                text.iter()
                    .position(|line| line.contains(&format!("worker-{n:02}")))
                    .map(|pos| (pos, n))
            })
            .collect();
        listed.sort_unstable();
        listed.into_iter().map(|(_, n)| n).collect()
    };
    // With nothing in flight the first page lists with no activation, so no stored pages
    // and one stored page draw alike.
    for (pages, listed, line) in [
        (0, (8..=12).rev().collect::<Vec<_>>(), Some("▸ +7 more")),
        (1, (8..=12).rev().collect(), Some("▸ +7 more")),
        (2, (3..=12).rev().collect(), Some("▸ +2 more")),
        (3, (1..=12).rev().collect(), None),
    ] {
        let (block, text) = block_at(pages);
        let joined = text.join("\n");
        assert_eq!(workers(&text), listed, "{pages} pages:\n{joined}");
        assert!(
            text.iter().any(|line| line.contains("  ⧉ subagents ")
                && line
                    .trim_end_matches(['▐', '🮇', ' '])
                    .ends_with("✓ done (12)")),
            "{pages} pages: the done title counts the whole pile:\n{joined}"
        );
        assert!(!joined.contains("subagents ("), "{pages} pages:\n{joined}");
        match line {
            Some(line) => {
                let at = text
                    .iter()
                    .position(|text| text.contains(line))
                    .unwrap_or_else(|| panic!("{pages} pages: {line} in\n{joined}"));
                assert_eq!(at, text.len() - 1, "{pages} pages: the section's last line");
                assert!(
                    matches!(
                        block.interactions.target_at(4, at as u16),
                        Some(HitTarget::PageDelegation { row, .. }) if row == row_id
                    ),
                    "{pages} pages: the pager's target"
                );
            }
            None => assert!(
                !joined.contains("▸ +"),
                "{pages} pages: the line is gone once nothing is hidden:\n{joined}"
            ),
        }
    }
    let mut paged_view = ViewState {
        selected_index: Some(0),
        ..Default::default()
    };
    paged_view.toggles.delegation_pages = std::collections::BTreeMap::from([paged(&row, 1)]);
    let paged_screen = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &paged_view,
        &UiState::default(),
        54,
        40,
    );
    assert_snapshot("delegation_paged_with_more", paged_screen);

    // A paged prior-turn child shares the current children's grid, and a paneless one that
    // cannot reopen stays inert.
    let mut small = snapshot;
    let small_card =
        std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut small.worktree_groups)[0].rows)
            [0]
        .as_agent_mut()
        .unwrap();
    small_card.sub_agents.truncate(3);
    small_card.sub_agents[2].pane = Some(PaneId::from_parts(MuxName::Zellij, "terminal_live"));
    small_card.sub_agents[0].reopenable = false;
    small_card.sub_agents[0].provider_native = true;
    let mut current_wide = prior_child(
        "cur-wide",
        "current",
        AgentStatus::Running,
        base,
        true,
        false,
    );
    current_wide.prior_turn = false;
    current_wide.model = Some("claude-opus-4-8".to_owned());
    current_wide.tokens = Some(crate::store::snapshot::SubAgentTokens::Total(1_234_000));
    small_card.sub_agents.push(current_wide);
    let small_map = std::collections::BTreeMap::from([paged(&row, 1)]);
    let mut ctx_small = test_row_ctx(&small, &theme, 54, 0, 0, &rolls);
    ctx_small.delegation_pages = &small_map;
    let small_opened = worktree_group_block(&ctx_small, &small.worktree_groups[0], false, None);
    let small_text: Vec<_> = small_opened.lines.iter().map(Line::to_string).collect();
    assert!(
        !small_text.iter().any(|line| line.contains("▸ +")),
        "one page shows all three:\n{}",
        small_text.join("\n")
    );
    let current_at = small_text
        .iter()
        .position(|line| line.contains("current"))
        .expect("in-flight entry");
    let inert_idx = small_text
        .iter()
        .position(|line| line.contains("task p01"))
        .expect("inert prior-turn entry");
    assert!(current_at < inert_idx, "in-flight entries list first");
    assert_eq!(
        small_opened.interactions.target_at(4, inert_idx as u16),
        Some(HitTarget::Row(0)),
        "inert prior-turn entry falls through to the row"
    );
    let inert_span = small_opened.lines[inert_idx]
        .spans
        .iter()
        .find(|span| span.content == "task p01")
        .unwrap();
    assert_eq!(
        inert_span.style.fg,
        Some(theme.component(Component::SubagentInert))
    );
    assert!(inert_span.style.add_modifier.contains(Modifier::DIM));
    let prior_meta = small_text
        .iter()
        .find(|line| line.contains("Haiku"))
        .expect("prior-turn entry metadata line");
    let current_meta = small_text
        .iter()
        .find(|line| line.contains("Opus 4.8"))
        .expect("current entry metadata line");
    let column = |line: &str, needle: &str| line.find(needle).map(|at| line[..at].chars().count());
    assert_eq!(
        column(prior_meta, "Haiku"),
        column(current_meta, "Opus 4.8"),
        "one grid: {prior_meta:?} vs {current_meta:?}"
    );
    paged_view.toggles.delegation_pages = small_map;
    let small_screen = snapshot_to_screen_with_alert_and_ui(
        &small,
        None,
        &paged_view,
        &UiState::default(),
        54,
        28,
    );
    assert_snapshot("delegation_paged_with_inert", small_screen);
}

#[test]
fn a_card_with_nothing_in_flight_draws_nothing_unselected_and_its_done_title_when_opened() {
    let mut snapshot = snapshot_with(vec![
        agent(
            "parent",
            "claude",
            AgentStatus::Idle,
            Some("/repo/main"),
            None,
            Some("build feature"),
        ),
        agent(
            "other",
            "codex",
            AgentStatus::Idle,
            Some("/repo/main"),
            None,
            Some("other task"),
        ),
    ]);
    let base = snapshot.now;
    let row0 = &mut std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0];
    let row0_id = row0.id.clone();
    let card0 = row0.as_agent_mut().unwrap();
    card0.prompt = Some("build feature".to_owned());
    card0.sub_agents = vec![prior_child(
        "p1",
        "coder",
        AgentStatus::Success,
        Timestamp::from_second(base.as_second() - 60).unwrap(),
        true,
        false,
    )];

    let theme = Theme::fixed(false);
    let rolls = CostRolls::default();
    let text_of = |ctx: &RowCtx<'_>| {
        worktree_group_block(ctx, &snapshot.worktree_groups[0], false, None)
            .lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    };

    // Unselected in auto and compact density: no title and no entry.
    for density in [
        crate::config::CardDensityMode::Auto,
        crate::config::CardDensityMode::Compact,
    ] {
        let mut ctx = test_row_ctx(&snapshot, &theme, 54, 1, 0, &rolls);
        ctx.card_density = density;
        let text = text_of(&ctx);
        assert!(
            !text.contains("task p1") && !text.contains("subagents") && !text.contains("done ("),
            "unselected {density:?} card draws nothing new:\n{text}"
        );
    }

    // Selected (auto and compact), held (auto), and expanded (unselected): the done title
    // over the first page of finished entries, with no pager while none stays hidden.
    let held = std::collections::BTreeSet::from([row0_id.clone()]);
    for (label, selected_idx, density, use_held) in [
        (
            "auto selected",
            0,
            crate::config::CardDensityMode::Auto,
            false,
        ),
        (
            "compact selected",
            0,
            crate::config::CardDensityMode::Compact,
            false,
        ),
        ("auto held", 1, crate::config::CardDensityMode::Auto, true),
        (
            "expanded unselected",
            1,
            crate::config::CardDensityMode::Expanded,
            false,
        ),
    ] {
        let mut ctx = test_row_ctx(&snapshot, &theme, 54, selected_idx, 0, &rolls);
        ctx.card_density = density;
        if use_held {
            ctx.held_expansions = &held;
        }
        let text = text_of(&ctx);
        let lines: Vec<_> = text.lines().collect();
        let title = lines
            .iter()
            .position(|line| {
                line.contains("  ⧉ subagents ")
                    && line
                        .trim_end_matches(['▐', '🮇', ' '])
                        .ends_with("✓ done (1)")
            })
            .unwrap_or_else(|| panic!("{label}: expected the done title:\n{text}"));
        assert_eq!(
            lines.get(title + 1).map(|line| line.contains("task p1")),
            Some(true),
            "{label}: the finished entry lists under the title:\n{text}"
        );
        assert!(
            !text.contains("(0)") && !text.contains("▸ +"),
            "{label}:\n{text}"
        );
    }

    // A pin closing the selected card's section keeps the title to reopen it and drops
    // its entries.
    let pins = std::collections::BTreeMap::from([(
        row0_id.clone(),
        DelegationPin {
            open: false,
            turn: None,
        },
    )]);
    let mut ctx = test_row_ctx(&snapshot, &theme, 54, 0, 0, &rolls);
    ctx.delegation_overrides = &pins;
    let block = worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None);
    let lines = line_texts(&block.lines);
    let closed = lines.join("\n");
    let title = lines
        .iter()
        .position(|line| line.contains("✓ done (1)"))
        .unwrap_or_else(|| panic!("the title stays:\n{closed}"));
    assert!(!closed.contains("task p1"), "{closed}");
    assert_eq!(
        block.interactions.target_at(4, title as u16),
        Some(HitTarget::ToggleDelegation {
            row: row0_id,
            open: true,
        }),
        "a click on the title reopens the section"
    );

    let selected_screen = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &ViewState {
            selected_index: Some(0),
            ..Default::default()
        },
        &UiState::default(),
        54,
        22,
    );
    assert_snapshot("done_title_card_selected", selected_screen);
}

#[test]
fn subagent_entries_distinguish_live_reopenable_inert_and_inflight_reopening() {
    let mut snapshot = snapshot_with(vec![agent(
        "parent",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        None,
        Some("coordinate children"),
    )]);
    let base = snapshot.now;
    let live_pane = PaneId::from_parts(MuxName::Zellij, "terminal_live");
    let row = &mut std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0];
    let card = row.as_agent_mut().unwrap();
    let make = |id: &str, pane: Option<&PaneId>, reopenable: bool, native: bool| {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "kind": "claude",
            "name": id,
            "status": "success",
            "last_activity": base,
            "pane": pane,
            "description": format!("work {id}"),
            "model": "Haiku",
            "prior_turn": false,
            "reopenable": reopenable,
            "provider_native": native,
        }))
        .unwrap()
    };
    card.sub_agents = vec![
        make("live-child", Some(&live_pane), true, false),
        make("reopen-child", None, true, false),
        make("noresume-child", None, false, false),
        make("native-child", None, false, true),
    ];

    let theme = Theme::fixed(false);
    let rolls = CostRolls::default();
    let pages = all_paged(&snapshot);
    let mut ctx = test_row_ctx(&snapshot, &theme, 54, 0, 0, &rolls);
    ctx.delegation_pages = &pages;
    let block = worktree_group_block(&ctx, &snapshot.worktree_groups[0], false, None);
    let line_of = |block: &RenderedBlock, needle: &str| {
        block
            .lines
            .iter()
            .position(|line| line.to_string().contains(needle))
            .unwrap_or_else(|| panic!("missing line for {needle}"))
    };
    assert_eq!(
        block
            .interactions
            .target_at(4, line_of(&block, "work live-child") as u16),
        Some(HitTarget::SubAgentPane(live_pane.clone()))
    );
    assert_eq!(
        block
            .interactions
            .target_at(4, line_of(&block, "work reopen-child") as u16),
        Some(HitTarget::ReopenSubAgent("reopen-child".to_owned()))
    );
    // Inert entries have no target (fall through to Row(0)) and draw dimmed in SubagentInert.
    let inert_fg = theme.component(Component::SubagentInert);
    for needle in ["work noresume-child", "work native-child"] {
        let idx = line_of(&block, needle);
        assert_eq!(
            block.interactions.target_at(4, idx as u16),
            Some(HitTarget::Row(0)),
            "{needle} must fall through to the parent row"
        );
        let span = block.lines[idx]
            .spans
            .iter()
            .find(|span| span.content == needle)
            .unwrap();
        assert_eq!(span.style.fg, Some(inert_fg), "{needle} fg");
        assert!(
            span.style.add_modifier.contains(Modifier::DIM),
            "{needle} must be dimmed"
        );
    }

    // Cursor stops include live pane and reopenable child, skip both inert entries.
    let view = paged_view(&snapshot, 0);
    assert_eq!(
        view.entry_stops(&snapshot, 0),
        vec![
            EntryCursor::SubAgent(live_pane),
            EntryCursor::ReopenSubAgent("reopen-child".to_owned()),
        ]
    );

    // In-flight reopening repaints reopen-child's lines in SubagentReopening without shifting lines.
    let reopening = std::collections::HashMap::from([("reopen-child".to_owned(), 99_000)]);
    let mut ctx_inflight = test_row_ctx(&snapshot, &theme, 54, 0, 0, &rolls);
    ctx_inflight.reopening = &reopening;
    ctx_inflight.delegation_pages = &pages;
    let inflight = worktree_group_block(&ctx_inflight, &snapshot.worktree_groups[0], false, None);
    assert_eq!(
        inflight
            .lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>(),
        block.lines.iter().map(Line::to_string).collect::<Vec<_>>(),
        "in-flight mark moves no line"
    );
    let reopen_idx = line_of(&inflight, "work reopen-child");
    let reopen_fg = theme.component(Component::SubagentReopening);
    let span = inflight.lines[reopen_idx]
        .spans
        .iter()
        .find(|span| span.content == "work reopen-child")
        .unwrap();
    assert_eq!(span.style.fg, Some(reopen_fg));
}

#[test]
fn card_heights_hold_across_pages_and_in_flight_flips() {
    let mut snapshot = snapshot_with(vec![agent(
        "parent",
        "claude",
        AgentStatus::Idle,
        Some("/repo/main"),
        None,
        Some("build feature"),
    )]);
    let base = snapshot.now;
    let row = &mut std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0];
    let card = row.as_agent_mut().unwrap();
    card.prompt = Some("build feature".to_owned());
    card.user_turn_started_at = Some(base);
    card.sub_agents = (1..=7)
        .map(|n| {
            prior_child(
                &format!("p{n}"),
                "coder",
                AgentStatus::Success,
                Timestamp::from_second(base.as_second() - 30 * n).unwrap(),
                true,
                false,
            )
        })
        .collect();
    let row = snapshot.worktree_groups[0].rows[0].clone();
    // The same card with a child running in the current turn.
    let mut running = snapshot.clone();
    let mut child = prior_child("now", "coder", AgentStatus::Running, base, true, false);
    child.prior_turn = false;
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut running.worktree_groups)[0].rows)
        [0]
    .as_agent_mut()
    .unwrap()
    .sub_agents
    .push(child);

    let mut ui = UiState::default();
    let mut view = ViewState {
        selected_index: Some(0),
        ..Default::default()
    };
    // Composes as a pane does, with one theme Rc, so repeated composes of one snapshot share
    // a warm epoch. The epoch keys on the groups Arc and the snapshot clock, so the in-flight
    // flips (a child starting, the last one finishing) reach a cold epoch; the pages and pin
    // transitions stay inside a warm one. Every cached height read is checked against a
    // fresh paint (`worktree_group_lines_windowed`), and the hit count shows the repeat of
    // each state read what its first compose cached.
    let mut height = |snapshot: &SidebarSnapshot, view: &ViewState| {
        let theme = ui.theme(&snapshot.theme);
        let mut heights = std::mem::take(&mut ui.card_heights);
        let frame = crate::sidebar_pane::render::compose::compose_lines_with_meter(
            snapshot,
            None,
            view,
            &ui,
            &theme,
            54,
            60,
            None,
            Some(&mut heights),
        );
        let hits = heights.hits.len();
        ui.card_heights = heights;
        (frame.content_height, hits)
    };
    let warm = |height: &mut dyn FnMut(&SidebarSnapshot, &ViewState) -> (usize, usize),
                snapshot: &SidebarSnapshot,
                view: &ViewState,
                what: &str| {
        let (cold, first_hits) = height(snapshot, view);
        let (cached, hits) = height(snapshot, view);
        assert_eq!(cached, cold, "{what}");
        assert!(
            hits > first_hits,
            "{what}: the repeat reads the height the first compose cached ({hits} vs {first_hits})"
        );
        cold
    };
    let first_page = warm(&mut height, &snapshot, &view, "the implicit first page");
    let in_flight = warm(&mut height, &running, &view, "a child starting");
    assert_eq!(
        first_page,
        in_flight + 8,
        "five two-line entries above the pager replace the running child's two lines"
    );
    assert_eq!(
        warm(&mut height, &snapshot, &view, "the last child finishing"),
        first_page
    );
    // Inside the warm epoch: the first stored page lists the second.
    let (row_id, pages) = paged(&row, 2);
    view.toggles.delegation_pages.insert(row_id, pages);
    let second_page = warm(&mut height, &snapshot, &view, "a stored page");
    assert_eq!(
        second_page,
        first_page + 3,
        "two more entries, and the pager goes"
    );
    let paged_in_flight = warm(&mut height, &running, &view, "pages over a running child");
    assert!(
        paged_in_flight > in_flight,
        "the pages hold over a running child"
    );
    assert_eq!(
        warm(&mut height, &snapshot, &view, "back at rest"),
        second_page
    );
    // Inside the warm epoch: a pin closes the section, leaving the done title.
    pin_delegation(&mut view, &row, false);
    let closed = warm(&mut height, &snapshot, &view, "a pin closing it");
    assert!(closed < first_page, "a closed section keeps only its title");
    assert!(warm(&mut height, &running, &view, "closed over a running child") < in_flight);
    assert_eq!(
        warm(&mut height, &snapshot, &view, "closed at rest"),
        closed
    );
}
