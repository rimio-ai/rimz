use super::*;
use crate::config::CardDensityMode;
use crate::sidebar_pane::render::layout::text_width;
use crate::store::snapshot::CacheClock;

fn message_card(status: AgentStatus, description: Option<&str>) -> SidebarSnapshot {
    let mut snapshot = snapshot_with(vec![agent(
        "coder",
        "claude",
        status,
        Some("/repo/main"),
        Some("main"),
        description,
    )]);
    let row = snapshot.rows_mut().next().unwrap();
    row.name = "coder".to_owned();
    row.unread = false;
    row.as_agent_mut().unwrap().handle = Some("coder".to_owned());
    row.as_agent_mut().unwrap().delegated_cost_usd = Some(1.27);
    snapshot
}

fn set_message_marks(snapshot: &mut SidebarSnapshot, due: usize, typing: bool) {
    let panes = snapshot
        .rows_mut()
        .filter_map(|row| {
            if let Some(card) = row.as_agent_mut() {
                card.due_messages = due;
            }
            row.pane.as_ref().map(|pane| pane.pane_id.clone())
        })
        .collect::<Vec<_>>();
    snapshot.typing_until.clear();
    if typing {
        for pane in panes {
            snapshot
                .typing_until
                .insert(pane, snapshot.now + Duration::from_secs(30));
        }
    }
}

fn end_column(line: &str, text: &str) -> Option<usize> {
    line.split_once(text)
        .map(|(before, _)| text_width(before) + text_width(text))
}

#[test]
fn message_marks_align_with_the_percentage_across_card_shapes_and_tier_edges() {
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for set in ["unicode", "nerd_font"] {
        for width in [31, 32, 33, 45, 46, 47] {
            let mut running = message_card(AgentStatus::Running, Some("store refactor"));
            running.theme.glyphs.set = Some(set.to_owned());
            let theme = Theme::fixed_for_theme(false, &running.theme);
            let reference = line_texts(&group_lines_at_width(&running, &theme, 0, width));
            let percentage_end = reference
                .iter()
                .find_map(|line| end_column(line, "%"))
                .unwrap();
            let pen = theme.glyph(GlyphRole::CardTyping);
            let queued = theme.glyph(GlyphRole::CardMessageQueued);
            let cluster = if width < 32 {
                format!("{pen} {queued}")
            } else {
                format!("{pen} {queued} 3")
            };
            for shape in ["running", "fresh_labeled", "fresh", "ask"] {
                let mut snapshot = running.clone();
                let card = snapshot.rows_mut().next().unwrap().as_agent_mut().unwrap();
                if shape.starts_with("fresh") {
                    card.status = AgentStatus::Idle;
                    card.delegated_cost_usd = None;
                    if shape == "fresh" {
                        card.task = None;
                    }
                }
                if shape == "ask" {
                    card.status = AgentStatus::Waiting;
                    card.ask_detail = Some("Should I refresh the pricing cache and every downstream consumer before continuing the build?".to_owned());
                    card.extra_questions = 2;
                }
                set_message_marks(&mut snapshot, 3, true);
                let lines = line_texts(&group_lines_at_width(&snapshot, &theme, 0, width));
                actual.push((
                    set,
                    width,
                    shape,
                    lines.iter().find_map(|line| end_column(line, &cluster)),
                ));
                expected.push((set, width, shape, Some(percentage_end)));
            }
        }
    }
    assert_eq!(actual, expected);
}

#[test]
fn message_marks_yield_to_the_cost_with_one_cell_between() {
    let theme = Theme::fixed(false);
    let running = message_card(AgentStatus::Running, Some("store refactor"));
    let reference = line_texts(&group_lines_at_width(&running, &theme, 0, 46));
    let percentage_end = reference
        .iter()
        .find_map(|line| end_column(line, "%"))
        .unwrap();
    let actual = [(1.27, "$1.27"), (12.34, "$12.34")].map(|(cost, label)| {
        let mut snapshot = message_card(AgentStatus::Idle, None);
        snapshot.theme.display.card_density = CardDensityMode::Compact;
        let card = snapshot.rows_mut().next().unwrap().as_agent_mut().unwrap();
        card.prompt = Some("history".to_owned());
        card.delegated_cost_usd = Some(cost);
        set_message_marks(&mut snapshot, 3, true);
        let lines = line_texts(&group_lines_at_width(&snapshot, &theme, usize::MAX, 46));
        (
            lines[1].contains(&format!("✎ ↧ 3 {label}")),
            end_column(&lines[1], "✎ ↧ 3"),
        )
    });
    assert_eq!(
        actual,
        [
            (true, Some(percentage_end)),
            (true, Some(percentage_end - 1))
        ]
    );
}

#[test]
fn message_marks_follow_the_compaction_gaps_percentage_below_l2() {
    let theme = Theme::fixed(false);
    let actual = [true, false].map(|has_cache| {
        let mut snapshot = message_card(AgentStatus::Running, Some("store refactor"));
        let card = snapshot.rows_mut().next().unwrap().as_agent_mut().unwrap();
        card.compacted_unmeasured = true;
        card.compacted_since_prompt = !has_cache;
        card.usage.context_window = Some(100_000);
        card.usage.context_pct = Some(40);
        card.usage.total_tokens = Some(40_000);
        card.cache = Some(CacheClock {
            ceiling_secs: 1800,
            last_request_at: None,
            warm_until: None,
            rewarm_cost_usd: None,
            recorded: false,
        });
        set_message_marks(&mut snapshot, 3, true);
        let lines = line_texts(&group_lines_at_width(&snapshot, &theme, 0, 32));
        (
            lines.iter().find_map(|line| end_column(line, "✎ ↧ 3")),
            lines.iter().find_map(|line| end_column(line, "%")),
        )
    });
    assert_eq!(actual, [(Some(25), Some(25)), (Some(31), Some(31))]);
}

#[test]
fn message_marks_shrink_the_column_padding_before_dropping_a_mark() {
    let theme = Theme::fixed(false);
    let actual = [true, false].map(|has_cache| {
        let mut snapshot = message_card(AgentStatus::Idle, None);
        snapshot.theme.display.card_density = CardDensityMode::Compact;
        let card = snapshot.rows_mut().next().unwrap().as_agent_mut().unwrap();
        card.prompt = Some("history".to_owned());
        card.delegated_cost_usd = None;
        card.handle = Some("h".repeat(18));
        card.compacted_unmeasured = true;
        card.compacted_since_prompt = !has_cache;
        card.cache = Some(CacheClock {
            ceiling_secs: 1800,
            last_request_at: None,
            warm_until: None,
            rewarm_cost_usd: None,
            recorded: false,
        });
        set_message_marks(&mut snapshot, 3, true);
        let lines = line_texts(&group_lines_at_width(&snapshot, &theme, usize::MAX, 32));
        lines.iter().find_map(|line| end_column(line, "✎ ↧ 3"))
    });
    assert_eq!(actual, [Some(27), Some(31)]);
}

#[test]
fn message_marks_pin_to_description_with_each_mark_both_sets_and_no_color() {
    let mut actual = Vec::new();
    let mut frames = Vec::new();
    for (set, no_color) in [("unicode", false), ("nerd_font", false), ("unicode", true)] {
        for (due, typing, label) in [(3, false, "queued"), (0, true, "typing"), (3, true, "both")] {
            let mut snapshot = message_card(
                AgentStatus::Running,
                Some(
                    "store refactor with a very long description that must yield to message marks",
                ),
            );
            snapshot.theme.glyphs.set = Some(set.to_owned());
            set_message_marks(&mut snapshot, due, typing);
            let theme = Theme::fixed_for_theme(no_color, &snapshot.theme);
            let pen = theme.glyph(GlyphRole::CardTyping);
            let queued = theme.glyph(GlyphRole::CardMessageQueued);
            let cluster = match (due, typing) {
                (0, true) => pen.to_owned(),
                (_, false) => format!("{queued} {due}"),
                _ => format!("{pen} {queued} {due}"),
            };
            let lines = group_lines_at_width(&snapshot, &theme, 0, 46);
            let text = line_texts(&lines);
            let description = text
                .iter()
                .find(|line| line.contains("store refactor"))
                .unwrap();
            actual.push((
                description.trim_end_matches(['▐', ' ']).ends_with(&cluster),
                text[1].contains(&cluster),
                !no_color
                    || lines
                        .iter()
                        .flat_map(|line| &line.spans)
                        .all(|span| span.style.fg.is_none_or(|color| color == Color::Reset)),
            ));
            frames.push((
                format!("message_marks_{set}_{no_color}_{label}"),
                snapshot_to_screen(&snapshot, 46, 22),
            ));
        }
    }
    assert_eq!(actual, vec![(true, false, true); 9]);
    for (name, screen) in frames {
        assert_snapshot(&name, screen);
    }
}

#[test]
fn message_marks_follow_the_asks_intact_question_count_without_adding_lines() {
    let mut snapshot = message_card(AgentStatus::Waiting, None);
    let card = snapshot.rows_mut().next().unwrap().as_agent_mut().unwrap();
    card.ask_detail = Some("Should I refresh the pricing cache and every downstream consumer before continuing the build?".to_owned());
    card.extra_questions = 2;
    let theme = Theme::fixed(false);
    let before = group_lines_at_width(&snapshot, &theme, 0, 32).len();
    set_message_marks(&mut snapshot, 3, true);
    let lines = group_lines_at_width(&snapshot, &theme, 0, 32);
    let text = line_texts(&lines).join("\n");
    assert_eq!(
        (lines.len(), text.contains("… +2 ✎ ↧ 3")),
        (before, true),
        "{text}"
    );
    assert_snapshot("message_marks_ask", snapshot_to_screen(&snapshot, 32, 22));
}

#[test]
fn message_marks_use_identity_fallback_and_protect_long_handles() {
    let mut actual = Vec::new();
    let mut frames = Vec::new();
    for (fresh, selected) in [(false, false), (true, false), (true, true)] {
        let mut snapshot = message_card(AgentStatus::Idle, None);
        if !fresh {
            snapshot.theme.display.card_density = CardDensityMode::Compact;
            snapshot
                .rows_mut()
                .next()
                .unwrap()
                .as_agent_mut()
                .unwrap()
                .prompt = Some("history".to_owned());
        } else {
            snapshot
                .rows_mut()
                .next()
                .unwrap()
                .as_agent_mut()
                .unwrap()
                .delegated_cost_usd = None;
        }
        set_message_marks(&mut snapshot, 3, true);
        let theme = Theme::fixed(false);
        let lines = line_texts(&group_lines_at_width(
            &snapshot,
            &theme,
            if selected { 0 } else { usize::MAX },
            32,
        ));
        actual.push(
            lines[1].contains("coder")
                && lines[1].contains(if fresh {
                    "✎ ↧ 3"
                } else {
                    "✎ ↧ 3 $1.27"
                }),
        );
        let ui = ViewState {
            selected_index: selected.then_some(0),
            ..Default::default()
        };
        frames.push((
            format!("message_marks_identity_{fresh}_{selected}"),
            snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, &UiState::default(), 32, 22),
        ));
    }
    let mut long = message_card(AgentStatus::Idle, None);
    let card = long.rows_mut().next().unwrap().as_agent_mut().unwrap();
    card.handle = Some("a-long-full-handle".to_owned());
    long.theme.display.card_density = CardDensityMode::Compact;
    set_message_marks(&mut long, 3, true);
    let lines = line_texts(&group_lines_at_width(
        &long,
        &Theme::fixed(false),
        usize::MAX,
        28,
    ));
    actual.push(lines[1].contains("a-long-full-handle") && !lines[1].contains(['✎', '↧']));
    assert_eq!(actual, vec![true; 4], "{lines:?}");
    let edges = [28, 29, 30, 31, 32, 33, 34].map(|width| {
        let lines = line_texts(&group_lines_at_width(
            &long,
            &Theme::fixed(false),
            usize::MAX,
            width,
        ));
        (
            lines[1].contains("a-long-full-handle") && lines[1].contains("$1.27"),
            lines[1].contains('✎'),
            lines[1].contains('↧'),
        )
    });
    assert_eq!(
        edges,
        [
            (true, false, false),
            (true, false, false),
            (true, true, false),
            (true, true, false),
            (true, true, true),
            (true, true, true),
            (true, true, true)
        ]
    );
    frames.push((
        "message_marks_long_handle".to_owned(),
        snapshot_to_screen_with_alert_and_ui(
            &long,
            None,
            &ViewState::default(),
            &UiState::default(),
            28,
            22,
        ),
    ));
    for (name, screen) in frames {
        assert_snapshot(&name, screen);
    }
}

#[test]
fn message_marks_keep_every_template_height_at_tier_edges() {
    let mut failures = Vec::new();
    for density in [
        CardDensityMode::Auto,
        CardDensityMode::Compact,
        CardDensityMode::Expanded,
    ] {
        for status in [
            AgentStatus::Idle,
            AgentStatus::Running,
            AgentStatus::Waiting,
            AgentStatus::Success,
            AgentStatus::Failed,
            AgentStatus::Paused,
            AgentStatus::Sleeping,
        ] {
            for shape in ["fresh", "fresh_labeled", "engaged", "stale", "ask"] {
                for selected in [0, usize::MAX] {
                    for width in [30, 31, 32, 33, 44, 45, 46, 47] {
                        let mut snapshot =
                            message_card(status, (shape != "fresh").then_some("store refactor"));
                        snapshot.theme.display.card_density = density;
                        let row = snapshot.rows_mut().next().unwrap();
                        if shape.starts_with("fresh") {
                            row.as_agent_mut().unwrap().delegated_cost_usd = None;
                        }
                        if shape == "engaged" {
                            row.as_agent_mut().unwrap().prompt = Some("history".into());
                        }
                        if shape == "stale" {
                            row.last_activity = fixed_now() - Duration::from_secs(3 * 86400);
                        }
                        if shape == "ask" && status == AgentStatus::Waiting {
                            let card = row.as_agent_mut().unwrap();
                            card.ask_detail =
                                Some("Will this question fit on just one line?".into());
                            card.extra_questions = 2;
                        }
                        let theme = Theme::fixed(false);
                        let before = group_lines_at_width(&snapshot, &theme, selected, width).len();
                        set_message_marks(&mut snapshot, 3, true);
                        let lines = group_lines_at_width(&snapshot, &theme, selected, width);
                        let text = line_texts(&lines).join("\n");
                        let result = (
                            before == lines.len(),
                            text.contains("✎ ↧"),
                            width >= 32 || !text.contains("↧ 3"),
                            lines
                                .iter()
                                .all(|line| text_width(&line.to_string()) <= width),
                        );
                        if result != (true, true, true, true) {
                            failures.push((density, status, shape, selected, width, result, text));
                        }
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} bad shapes; first eight: {:?}",
        failures.len(),
        &failures[..failures.len().min(8)]
    );
}

#[test]
fn message_marks_expire_at_the_typing_deadline() {
    let mut snapshot = message_card(AgentStatus::Running, Some("strict deadline"));
    set_message_marks(&mut snapshot, 0, true);
    let deadline = *snapshot.typing_until.values().next().unwrap();
    let actual = [
        deadline - Duration::from_millis(1),
        deadline,
        deadline + Duration::from_millis(1),
    ]
    .map(|now| {
        snapshot.now = now;
        line_texts(&group_lines_at_width(
            &snapshot,
            &Theme::fixed(false),
            0,
            46,
        ))
        .join("\n")
        .contains('✎')
    });
    assert_eq!(actual, [true, false, false]);
}

#[test]
fn message_marks_measure_overrides_and_never_mark_process_rows() {
    let mut snapshot = message_card(AgentStatus::Running, Some("override glyph widths"));
    snapshot.theme =
        toml::from_str("[glyphs.unicode.card]\nmessage_queued = '漢'\ntyping = '字'").unwrap();
    set_message_marks(&mut snapshot, 3, true);
    let theme = Theme::fixed_for_theme(false, &snapshot.theme);
    let text = line_texts(&group_lines_at_width(&snapshot, &theme, 0, 32)).join("\n");
    assert!(text.contains("字 漢 3"), "{text}");
    assert_snapshot(
        "message_marks_wide_override",
        snapshot_to_screen(&snapshot, 32, 22),
    );

    let mut process = snapshot_with(Vec::new())
        .with_live_panes(vec![pane("%build", "cargo test", "/repo/main")], None);
    set_message_marks(&mut process, 3, true);
    let screen = snapshot_to_screen(&process, 32, 22);
    assert!(!screen.contains(['✎', '↧']), "{screen}");
    assert_snapshot("message_marks_process", screen);
}
