use super::*;
use crate::sidebar_pane::render::compose::scroll_lines;
use crate::store::snapshot::{SidebarInactive, SidebarWorktreeKind, WorktreePrState};
use std::collections::BTreeSet;

fn inactive_snapshot() -> SidebarSnapshot {
    let agents = [
        "open-a",
        "open-b",
        "inactive-a",
        "inactive-b",
        "inactive-c",
        "external",
    ]
    .into_iter()
    .map(|name| {
        let status = if name == "inactive-a" {
            AgentStatus::Idle
        } else {
            AgentStatus::Success
        };
        let mut member = agent(
            name,
            "claude",
            status,
            Some(&format!("/repo/{name}")),
            Some(name),
            Some(&format!("task-{name}")),
        );
        member.role = Some(name.to_owned());
        if name.starts_with("inactive-") {
            member.registered_at = Some(fixed_now() - Duration::from_secs(7200));
        }
        member
    })
    .collect();
    let mut snapshot = snapshot_with(agents);
    for group in std::sync::Arc::make_mut(&mut snapshot.worktree_groups) {
        if group.label.starts_with("inactive-") {
            group.clean = Some(true);
            group.pr_state = Some(WorktreePrState::Merged);
        }
        if group.label == "external" {
            group.kind = SidebarWorktreeKind::External;
        }
    }
    snapshot.sort_groups_for_presentation();
    assert_eq!(snapshot.worktree_groups.len(), 6);
    snapshot
}

fn paged_snapshot() -> SidebarSnapshot {
    let mut snapshot = inactive_snapshot();
    let template = snapshot.worktree_groups[2].clone();
    let now = snapshot.now;
    let groups = std::sync::Arc::make_mut(&mut snapshot.worktree_groups);
    groups.retain(|group| group.inactive.is_none());
    for index in (1..=25).rev() {
        let name = format!("old-{index:02}");
        let mut group = template.clone();
        group.key = name.clone();
        group.label = name.clone();
        let row = &mut std::sync::Arc::make_mut(&mut group.rows)[0];
        row.id = name.clone();
        row.name = name.clone();
        row.last_activity = now - Duration::from_secs(7200 + index * 60);
        row.pane.as_mut().unwrap().pane_id =
            PaneId::from_parts(MuxName::Zellij, format!("terminal_old_{index}"));
        groups.push(group);
    }
    snapshot.sort_groups_for_presentation();
    snapshot
}

fn body(
    snapshot: &SidebarSnapshot,
    ui: &ViewState,
    pane_ui: &UiState,
    width: usize,
) -> RenderedBlock {
    let roster = ui.visible_roster(snapshot);
    scroll_lines(
        snapshot,
        ui,
        pane_ui,
        &roster,
        &BTreeSet::new(),
        width,
        &Theme::fixed(true),
        None,
    )
    .0
}

#[test]
fn closed_inactive_line_replaces_receipts_before_external() {
    let snapshot = inactive_snapshot();
    let block = body(&snapshot, &ViewState::default(), &UiState::default(), 54);
    let texts = line_texts(&block.lines);
    let fold = texts
        .iter()
        .position(|line| line.contains("[ ▸ 3 inactive ]"))
        .expect("closed fold line");
    assert!(texts[fold - 1].is_empty());
    assert!(texts[..fold].iter().any(|line| line.contains("open-b")));
    assert!(
        texts[fold + 1..]
            .iter()
            .any(|line| line.contains("external"))
    );
    assert!(
        texts.iter().all(|line| !line.contains("inactive-")),
        "{texts:?}"
    );
    assert_eq!(block.interactions.row_at_line(fold), None);
    let chip = block.lines[fold]
        .spans
        .iter()
        .find(|span| span.content == "[ ▸ 3 inactive ]")
        .unwrap();
    assert!(chip.style.add_modifier.contains(Modifier::REVERSED));
    for column in [0, 53] {
        assert_eq!(
            block.interactions.target_at(column, fold as u16),
            Some(HitTarget::ToggleInactive)
        );
    }
    assert_snapshot("inactive_closed", snapshot_to_screen(&snapshot, 54, 42));
}

#[test]
fn open_inactive_line_lists_collapsed_receipts_with_group_gaps() {
    let snapshot = inactive_snapshot();
    let block = body(
        &snapshot,
        &ViewState {
            inactive_peek: Some(Default::default()),
            ..Default::default()
        },
        &UiState::default(),
        54,
    );
    let texts = line_texts(&block.lines);
    let fold = texts
        .iter()
        .position(|line| line.contains("[ ▾ 3 inactive ]"))
        .expect("open fold line");
    for name in ["inactive-a", "inactive-b", "inactive-c"] {
        let header = texts.iter().position(|line| line.contains(name)).unwrap();
        assert!(header > fold);
        assert!(texts[header - 1].is_empty());
        assert!(texts[header + 1].contains('▸'), "{texts:?}");
    }
    assert!(texts.iter().all(|line| !line.contains("task-inactive")));
    assert_snapshot(
        "inactive_open",
        snapshot_to_screen_with_alert_and_ui(
            &snapshot,
            None,
            &ViewState {
                inactive_peek: Some(Default::default()),
                ..Default::default()
            },
            &UiState::default(),
            54,
            42,
        ),
    );
}

#[test]
fn closed_inactive_line_keeps_focused_held_and_expanded_groups() {
    let mut snapshot = inactive_snapshot();
    let group = snapshot
        .worktree_groups
        .iter()
        .find(|group| group.label == "inactive-a")
        .unwrap();
    let pane = group.rows[0].pane.as_ref().unwrap().pane_id.clone();
    let key = group.key.clone();
    snapshot.focused_pane = Some(pane);
    let focused =
        line_texts(&body(&snapshot, &ViewState::default(), &UiState::default(), 54).lines)
            .join("\n");
    assert!(
        focused.contains("[ ▸ 3 inactive ]") && focused.contains("inactive-a"),
        "{focused}"
    );
    assert!(!focused.contains("inactive-b"));
    snapshot.focused_pane = None;
    let expanded = line_texts(
        &body(
            &snapshot,
            &ViewState {
                expanded_groups: BTreeSet::from([key.clone()]),
                ..Default::default()
            },
            &UiState::default(),
            54,
        )
        .lines,
    )
    .join("\n");
    assert!(expanded.contains("[ ▸ 3 inactive ]") && expanded.contains("inactive-a"));
    let held = std::collections::HashSet::from([snapshot
        .worktree_groups
        .iter()
        .find(|group| group.key == key)
        .unwrap()
        .rows[0]
        .id
        .clone()]);
    let roster = VisibleRoster::new(
        &snapshot,
        &BodyLens::default(),
        &BTreeSet::new(),
        Some(&held),
    );
    let block = scroll_lines(
        &snapshot,
        &ViewState::default(),
        &UiState::default(),
        &roster,
        &BTreeSet::new(),
        54,
        &Theme::fixed(true),
        None,
    )
    .0;
    let text = line_texts(&block.lines).join("\n");
    assert!(text.contains("[ ▸ 3 inactive ]") && text.contains("inactive-a"));
}

#[test]
fn lenses_and_queries_reach_matching_inactive_rows_without_the_fold() {
    let snapshot = inactive_snapshot();
    for lens in [
        BodyLens::from(BodyFilter::Status(AgentStatus::Idle)),
        BodyLens {
            query: Some("inactive-a".to_owned()),
            ..Default::default()
        },
    ] {
        let text = line_texts(
            &body(
                &snapshot,
                &ViewState {
                    make_up_filter: lens,
                    ..Default::default()
                },
                &UiState::default(),
                54,
            )
            .lines,
        )
        .join("\n");
        assert!(text.contains("inactive-a"), "{text}");
        assert!(
            !text.contains("inactive ]") && !text.contains('▸'),
            "{text}"
        );
    }
    let text = line_texts(
        &body(
            &snapshot,
            &ViewState {
                make_up_filter: BodyFilter::Status(AgentStatus::Success).into(),
                inactive_peek: Some(Default::default()),
                ..Default::default()
            },
            &UiState::default(),
            54,
        )
        .lines,
    )
    .join("\n");
    assert!(!text.contains("inactive"), "{text}");
    let text = line_texts(
        &body(
            &snapshot,
            &ViewState {
                search_draft: Some("inactive-a".to_owned()),
                ..Default::default()
            },
            &UiState::default(),
            54,
        )
        .lines,
    )
    .join("\n");
    assert!(text.contains("inactive-a") && !text.contains("inactive ]"));
}

#[test]
fn inactive_chip_drops_breakdown_then_label_but_keeps_the_count() {
    for unlanded in [false, true] {
        let mut snapshot = inactive_snapshot();
        if unlanded {
            std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[2].inactive =
                Some(SidebarInactive::Unlanded);
        }
        for inner in [6, 7, 8, 15, 16, 17, 28, 29, 30] {
            let block = body(
                &snapshot,
                &ViewState::default(),
                &UiState::default(),
                inner + 2,
            );
            let texts = line_texts(&block.lines);
            let expected = if inner >= 29 && unlanded {
                "[ ▸ 3 inactive · 1 unlanded ]"
            } else if inner >= 16 {
                "[ ▸ 3 inactive ]"
            } else if inner >= 7 {
                "[ ▸ 3 ]"
            } else {
                "3"
            };
            assert!(
                texts.iter().any(|line| line.contains(expected)),
                "{inner}: {texts:?}"
            );
            let (_, line) = block
                .interactions
                .line_for_target(&HitTarget::ToggleInactive)
                .unwrap();
            for column in 0..inner + 2 {
                assert_eq!(
                    block.interactions.target_at(column as u16, line),
                    Some(HitTarget::ToggleInactive),
                    "width {}, column {column}",
                    inner + 2
                );
            }
        }
    }
}

#[test]
fn inactive_chip_hit_columns_follow_a_wide_dotted_glyph() {
    let mut snapshot = inactive_snapshot();
    std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[2].inactive =
        Some(SidebarInactive::Unlanded);
    let theme = Theme::fixed_for_theme(
        true,
        &toml::from_str("[glyphs.unicode.worktree]\ndotted = '界'").unwrap(),
    );
    assert_eq!(
        super::super::layout::text_width(theme.glyph(GlyphRole::WorktreeDotted)),
        2
    );
    let view = ViewState::default();
    let block = scroll_lines(
        &snapshot,
        &view,
        &UiState::default(),
        &view.visible_roster(&snapshot),
        &BTreeSet::new(),
        54,
        &theme,
        None,
    )
    .0;
    let texts = line_texts(&block.lines);
    let line = texts
        .iter()
        .position(|line| line.contains("3 inactive"))
        .unwrap();
    for column in 0..54 {
        assert_eq!(
            block.interactions.target_at(column, line as u16),
            Some(HitTarget::ToggleInactive),
            "column {column}"
        );
    }
}

#[test]
fn inactive_chip_is_one_target_in_both_states() {
    let mut snapshot = inactive_snapshot();
    std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[2].inactive =
        Some(SidebarInactive::Unlanded);
    for open in [false, true] {
        let view = ViewState {
            inactive_peek: open.then(Default::default),
            ..Default::default()
        };
        let block = body(&snapshot, &view, &UiState::default(), 54);
        let texts = line_texts(&block.lines);
        assert!(
            texts
                .iter()
                .any(|line| line.contains("3 inactive · 1 unlanded")),
            "{texts:?}"
        );
        let line = texts
            .iter()
            .position(|line| line.contains("3 inactive"))
            .unwrap();
        for column in 0..54 {
            assert_eq!(
                block.interactions.target_at(column, line as u16),
                Some(HitTarget::ToggleInactive)
            );
        }
        assert_snapshot(
            if open {
                "inactive_open_unlanded"
            } else {
                "inactive_closed_unlanded"
            },
            snapshot_to_screen_with_alert_and_ui(
                &snapshot,
                None,
                &view,
                &UiState::default(),
                54,
                42,
            ),
        );
    }
}

#[test]
fn expanded_frame_opens_inactive_and_every_card() {
    let snapshot = paged_snapshot();
    let mut ansi = Vec::new();
    render_expanded_line_ansi(&mut ansi, &snapshot, 54).unwrap();
    let text = String::from_utf8(ansi).unwrap();
    assert!(text.contains("[ ▾ 25 inactive ]"), "{text}");
    for index in 1..=25 {
        assert!(text.contains(&format!("old-{index:02}")), "{text}");
    }
    assert_eq!(text.matches("task-inactive-a").count(), 25, "{text}");
    assert!(!text.contains(" older"), "{text}");
}

#[test]
fn inactive_first_page_is_newest_first_with_a_muted_older_tail() {
    let snapshot = paged_snapshot();
    let view = ViewState {
        inactive_peek: Some(Default::default()),
        ..Default::default()
    };
    let block = body(&snapshot, &view, &UiState::default(), 54);
    let texts = line_texts(&block.lines);
    let drawn = texts
        .iter()
        .filter_map(|line| line.strip_prefix(" ↩ old-").map(str::trim_end))
        .collect::<Vec<_>>();
    assert_eq!(
        drawn,
        (1..=10)
            .map(|index| format!("{index:02}"))
            .collect::<Vec<_>>()
    );
    let tail = texts
        .iter()
        .position(|line| line.contains("+15 older"))
        .unwrap();
    assert!(
        texts[tail + 1..]
            .iter()
            .any(|line| line.contains("external"))
    );
    assert_eq!(block.lines[tail].spans[1].style, Theme::fixed(true).muted());
    assert_snapshot(
        "inactive_paged",
        snapshot_to_screen_with_alert_and_ui(&snapshot, None, &view, &UiState::default(), 54, 80),
    );
}

#[test]
fn inactive_page_keeps_focused_held_and_expanded_groups_beyond_its_cut() {
    for reveal in ["focused", "held", "expanded"] {
        let mut snapshot = paged_snapshot();
        let old = snapshot
            .worktree_groups
            .iter()
            .find(|group| group.label == "old-25")
            .unwrap();
        let key = old.key.clone();
        let row = old.rows[0].id.clone();
        if reveal == "focused" {
            snapshot.focused_pane = Some(old.rows[0].pane.as_ref().unwrap().pane_id.clone());
        }
        let view = ViewState {
            inactive_peek: Some(Default::default()),
            expanded_groups: if reveal == "expanded" {
                BTreeSet::from([key])
            } else {
                BTreeSet::new()
            },
            ..Default::default()
        };
        let held = (reveal == "held").then(|| std::collections::HashSet::from([row.clone()]));
        let roster = VisibleRoster::new(
            &snapshot,
            &BodyLens::default(),
            &view.expanded_groups,
            held.as_ref(),
        );
        assert!(roster.ordinal_of_id(&row).is_some(), "{reveal}");
        let block = scroll_lines(
            &snapshot,
            &view,
            &UiState::default(),
            &roster,
            &BTreeSet::new(),
            54,
            &Theme::fixed(true),
            None,
        )
        .0;
        let text = line_texts(&block.lines).join("\n");
        assert!(text.contains("old-25"), "{reveal}: {text}");
        assert!(!text.contains("old-24"), "{reveal}: {text}");
        assert!(text.contains("+14 older"), "{reveal}: {text}");
    }
}
