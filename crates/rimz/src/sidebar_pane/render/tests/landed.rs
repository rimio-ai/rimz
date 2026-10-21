use super::*;
use crate::sidebar_pane::render::compose::scroll_lines;
use crate::store::snapshot::{SidebarWorktreeKind, WorktreePrState};
use std::collections::BTreeSet;

fn landed_snapshot() -> SidebarSnapshot {
    let agents = [
        "open-a", "open-b", "landed-a", "landed-b", "landed-c", "external",
    ]
    .into_iter()
    .map(|name| {
        let status = if name == "landed-a" {
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
        member
    })
    .collect();
    let mut snapshot = snapshot_with(agents);
    for group in std::sync::Arc::make_mut(&mut snapshot.worktree_groups) {
        if group.label.starts_with("landed-") {
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

fn body(snapshot: &SidebarSnapshot, ui: &UiState, width: usize) -> RenderedBlock {
    let roster = ui.visible_roster(snapshot);
    scroll_lines(
        snapshot,
        ui,
        &roster,
        &BTreeSet::new(),
        width,
        &Theme::fixed(true),
        None,
    )
    .0
}

#[test]
fn closed_landed_line_replaces_receipts_before_external() {
    let snapshot = landed_snapshot();
    let block = body(&snapshot, &UiState::default(), 54);
    let texts = line_texts(&block.lines);
    let fold = texts
        .iter()
        .position(|line| line.contains("[ ▸ 3 landed ]"))
        .expect("closed fold line");
    assert!(texts[fold - 1].is_empty());
    assert!(texts[..fold].iter().any(|line| line.contains("open-b")));
    assert!(
        texts[fold + 1..]
            .iter()
            .any(|line| line.contains("external"))
    );
    assert!(
        texts.iter().all(|line| !line.contains("landed-")),
        "{texts:?}"
    );
    assert_eq!(block.interactions.row_at_line(fold), None);
    let chip = block.lines[fold]
        .spans
        .iter()
        .find(|span| span.content == "[ ▸ 3 landed ]")
        .unwrap();
    assert!(chip.style.add_modifier.contains(Modifier::REVERSED));
    for column in [0, 27, 53] {
        assert_eq!(
            block.interactions.target_at(column, fold as u16),
            Some(HitTarget::ToggleLanded)
        );
    }
    assert_snapshot("landed_closed", snapshot_to_screen(&snapshot, 54, 42));
}

#[test]
fn open_landed_line_lists_collapsed_receipts_with_group_gaps() {
    let snapshot = landed_snapshot();
    let block = body(
        &snapshot,
        &UiState {
            view: ViewState {
                landed_open: true,
                ..Default::default()
            },
            ..UiState::default()
        },
        54,
    );
    let texts = line_texts(&block.lines);
    let fold = texts
        .iter()
        .position(|line| line.contains("[ ▾ 3 landed ]"))
        .expect("open fold line");
    for name in ["landed-a", "landed-b", "landed-c"] {
        let header = texts.iter().position(|line| line.contains(name)).unwrap();
        assert!(header > fold);
        assert!(texts[header - 1].is_empty());
        assert!(texts[header + 1].contains('▸'), "{texts:?}");
    }
    assert!(texts.iter().all(|line| !line.contains("task-landed")));
    assert_snapshot(
        "landed_open",
        snapshot_to_screen_with_alert_and_ui(
            &snapshot,
            None,
            &UiState {
                view: ViewState {
                    landed_open: true,
                    ..Default::default()
                },
                ..UiState::default()
            },
            54,
            42,
        ),
    );
}

#[test]
fn closed_landed_line_keeps_focused_held_and_expanded_groups() {
    let mut snapshot = landed_snapshot();
    let group = snapshot
        .worktree_groups
        .iter()
        .find(|group| group.label == "landed-a")
        .unwrap();
    let pane = group.rows[0].pane.as_ref().unwrap().pane_id.clone();
    let key = group.key.clone();
    snapshot.focused_pane = Some(pane);
    let focused = line_texts(&body(&snapshot, &UiState::default(), 54).lines).join("\n");
    assert!(
        focused.contains("[ ▸ 3 landed ]") && focused.contains("landed-a"),
        "{focused}"
    );
    assert!(!focused.contains("landed-b"));
    snapshot.focused_pane = None;
    let expanded = line_texts(
        &body(
            &snapshot,
            &UiState {
                view: ViewState {
                    expanded_groups: BTreeSet::from([key.clone()]),
                    ..Default::default()
                },
                ..UiState::default()
            },
            54,
        )
        .lines,
    )
    .join("\n");
    assert!(expanded.contains("[ ▸ 3 landed ]") && expanded.contains("landed-a"));
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
        &UiState::default(),
        &roster,
        &BTreeSet::new(),
        54,
        &Theme::fixed(true),
        None,
    )
    .0;
    let text = line_texts(&block.lines).join("\n");
    assert!(text.contains("[ ▸ 3 landed ]") && text.contains("landed-a"));
}

#[test]
fn lenses_and_queries_reach_matching_landed_rows_without_the_fold() {
    let snapshot = landed_snapshot();
    for lens in [
        BodyLens::from(BodyFilter::Status(AgentStatus::Idle)),
        BodyLens {
            query: Some("landed-a".to_owned()),
            ..Default::default()
        },
    ] {
        let text = line_texts(
            &body(
                &snapshot,
                &UiState {
                    view: ViewState {
                        make_up_filter: lens,
                        ..Default::default()
                    },
                    ..UiState::default()
                },
                54,
            )
            .lines,
        )
        .join("\n");
        assert!(text.contains("landed-a"), "{text}");
        assert!(!text.contains("landed ]") && !text.contains('▸'), "{text}");
    }
    let text = line_texts(
        &body(
            &snapshot,
            &UiState {
                view: ViewState {
                    make_up_filter: BodyFilter::Status(AgentStatus::Success).into(),
                    landed_open: true,
                    ..Default::default()
                },
                ..UiState::default()
            },
            54,
        )
        .lines,
    )
    .join("\n");
    assert!(!text.contains("landed"), "{text}");
    let text = line_texts(
        &body(
            &snapshot,
            &UiState {
                view: ViewState {
                    search_draft: Some("landed-a".to_owned()),
                    ..Default::default()
                },
                ..UiState::default()
            },
            54,
        )
        .lines,
    )
    .join("\n");
    assert!(text.contains("landed-a") && !text.contains("landed ]"));
}

#[test]
fn landed_label_keeps_its_width_before_rules_and_trims_only_when_needed() {
    let snapshot = landed_snapshot();
    let label = "[ ▸ 3 landed ]";
    let label_width = super::super::layout::text_width(label);
    for inner in [
        label_width - 1,
        label_width,
        label_width + 1,
        label_width + 2,
    ] {
        let texts = line_texts(&body(&snapshot, &UiState::default(), inner + 2).lines);
        let line = texts
            .iter()
            .find(|line| line.contains('▸'))
            .expect("fold label");
        assert!(super::super::layout::text_width(line) <= inner + 1);
        if inner >= label_width {
            assert!(line.contains(label), "{inner}: {line}");
        } else {
            assert_eq!(line.trim(), "[ ▸ 3 landed");
        }
    }
}

#[test]
fn expanded_frame_opens_landed_and_every_card() {
    let snapshot = landed_snapshot();
    let mut ansi = Vec::new();
    render_expanded_line_ansi(&mut ansi, &snapshot, 54).unwrap();
    let text = String::from_utf8(ansi).unwrap();
    assert!(text.contains("[ ▾ 3 landed ]"), "{text}");
    for name in ["landed-a", "landed-b", "landed-c"] {
        assert!(text.contains(&format!("task-{name}")));
    }
}
