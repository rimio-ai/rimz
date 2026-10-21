use std::collections::BTreeSet;
use std::sync::Arc;

use proptest::prelude::*;

use super::*;
use crate::agents::{BackgroundShell, PendingWait, PendingWaitTrigger};
use crate::store::snapshot::{CacheClock, SidebarWorktreeKind};

fn fleet(count: usize) -> SidebarSnapshot {
    let mut agents = Vec::new();
    for index in 0..count {
        let group = index % 3;
        let status = if index % 4 == 0 {
            AgentStatus::Idle
        } else {
            AgentStatus::Running
        };
        let mut parent = agent(
            &format!("card-{index}"),
            "codex",
            status,
            Some(&format!("/repo/group-{group}")),
            Some(&format!("group-{group}")),
            Some("build the feature"),
        );
        parent.last_activity = fixed_now() - Duration::from_secs(59);
        if index == 0 || index == 3 {
            parent.team = Some("paired".into());
        }
        parent.usage.context_pct = Some(42);
        parent.usage.context_window = Some(200_000);
        if index % 5 == 0 {
            parent.pending_waits.push(PendingWait {
                class: crate::agents::PendingWaitClass::Sleeping,
                name: "build".into(),
                trigger: PendingWaitTrigger::Signal {
                    selector: "build.done".into(),
                },
                armed_at: Some(fixed_now()),
            });
            parent.background_shells.push(BackgroundShell {
                id: format!("shell-{index}"),
                command: Some("cargo test".into()),
                description: Some("check feature".into()),
                started_at: fixed_now(),
            });
            let mut child = agent(
                &format!("child-{index}"),
                "codex",
                AgentStatus::Running,
                None,
                None,
                Some("inspect"),
            );
            child.parent_agent_id = Some(parent.agent_id.clone());
            child.subagent_started_at = Some(fixed_now());
            agents.push(child);
        }
        agents.push(parent);
    }
    let mut snapshot = snapshot_with(agents);
    snapshot.theme.display.stale_after_secs = 60;
    for group in Arc::make_mut(&mut snapshot.worktree_groups) {
        group.kind = SidebarWorktreeKind::Worktree;
        group.finished = group.label == "group-2";
        for row in Arc::make_mut(&mut group.rows) {
            row.as_agent_mut().unwrap().cache = Some(CacheClock {
                ceiling_secs: 60,
                last_request_at: Some(fixed_now() - Duration::from_secs(59)),
                warm_until: Some(fixed_now() + Duration::from_secs(1)),
                held_since: None,
                rewarm_cost_usd: None,
            });
        }
    }
    snapshot
}

fn compose_cached(
    snapshot: &SidebarSnapshot,
    ui: &mut UiState,
    width: u16,
    height: u16,
) -> compose::ComposedFrame {
    let theme = ui.theme(&snapshot.theme);
    let mut heights = std::mem::take(&mut ui.card_heights);
    let frame = compose_lines_with_meter(
        snapshot,
        None,
        ui,
        &theme,
        width,
        height,
        None,
        Some(&mut heights),
    );
    ui.card_heights = heights;
    frame
}

fn write_back(ui: &mut UiState, frame: compose::ComposedFrame) {
    ui.interactions = frame.interactions;
    ui.scrollbar.shift(frame.correction);
    ui.view
        .scrollbar
        .observe(frame.visible_scroll_offset, ui.animation_phase);
    ui.scroll_ease = frame.scroll_ease;
    ui.view
        .scroll_ease
        .observe(frame.scroll_offset, ui.frame_ms);
    ui.scroll_offset = frame.scroll_offset;
    ui.drawn_scroll_max = Some(frame.scroll_max);
    ui.painted_rows = frame.painted_rows;
    ui.anchored_rows = frame.anchored_rows;
    ui.anchor_banner_shown = frame.banner_shown;
    ui.drawn_selection = frame.drawn_selection;
    ui.held_expansions = frame.held_expansions;
    ui.scroll_zone_top = frame.top_height;
    ui.banner_shown = frame.banner_shown;
    ui.anchor_clamped = frame.anchor_clamped;
    ui.offset_authoritative = false;
    ui.landed_reveal = false;
    ui.reveal_row = None;
    ui.focus_placement = None;
    ui.own_input_reveal = false;
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]
    #[test]
    fn cached_frames_equal_full_compose(
        count in 1usize..=130, mut width in 20u16..=80, height in 8u16..=60,
        steps in prop::collection::vec((0u8..12, any::<u16>()), 8..=16),
    ) {
        let mut snapshot = fleet(count);
        let mut ui = UiState::default();
        for (step, (action, value)) in steps.into_iter().enumerate() {
            ui.frame_ms += 40;
            ui.animation_phase += 1;
            match action {
                0 => {
                    ui.scroll_ease.arm();
                    ui.scroll_offset = usize::from(value) % (ui.drawn_scroll_max.unwrap_or(0) + 1);
                    ui.offset_authoritative = true;
                }
                1 => ui.selected_index = Some(usize::from(value) % ui.visible_roster(&snapshot).len().max(1)),
                2 => snapshot.now += Duration::from_secs(2),
                3 => snapshot.rows_mut().next().unwrap().name = format!("edited-{step}"),
                // Same copy-on-write mutation as the app's private set_rows_unread.
                4 => { let row = snapshot.rows_mut().next().unwrap(); row.unread = !row.unread; }
                5 => ui.freeze = Some(LayoutFreeze { frozen: FrozenOrder::default(), armed_ms: snapshot.now.as_millisecond() }),
                6 => ui.freeze = None,
                7 => {
                    let key = snapshot.worktree_groups[0].key.clone();
                    if !ui.expanded_groups.remove(&key) { ui.expanded_groups.insert(key); }
                }
                8 => width = 20 + value % 61,
                9 => ui.landed_open = !ui.landed_open,
                10 => ui.search_draft = Some(if value % 2 == 0 { "card-1".into() } else { String::new() }),
                _ => {
                    let row = snapshot.worktree_groups[0].rows[0].id.clone();
                    ui.delegation_overrides.insert(row.clone(), value % 2 == 0);
                    ui.delegation_history.insert(row, None);
                }
            }
            let before = ui.clone();
            let theme = ui.theme(&snapshot.theme);
            let expected = compose_lines(&snapshot, None, &before, &theme, width, height);
            let actual = compose_cached(&snapshot, &mut ui, width, height);
            prop_assert_eq!(&actual, &expected, "step {} action {}", step, action);
            write_back(&mut ui, actual);
        }
    }
}

fn visible_cards(
    frame: &compose::ComposedFrame,
    snapshot: &SidebarSnapshot,
    ui: &UiState,
) -> BTreeSet<usize> {
    let roster = ui.visible_roster(snapshot);
    frame
        .painted_rows
        .iter()
        .map(|(id, _)| roster.ordinal_of_id(id).unwrap())
        .collect()
}

fn assert_painted(ui: &UiState, expected: BTreeSet<usize>) {
    let painted = &ui.card_heights.painted;
    assert_eq!(
        painted.len(),
        expected.len(),
        "each card must be painted at most once"
    );
    assert_eq!(painted.iter().copied().collect::<BTreeSet<_>>(), expected);
}

#[test]
fn paint_scope_tracks_window_epoch_and_selection() {
    let mut snapshot = fleet(124);
    let mut ui = UiState {
        view: ViewState {
            expanded_groups: snapshot
                .worktree_groups
                .iter()
                .map(|g| g.key.clone())
                .collect(),
            landed_open: true,
            ..Default::default()
        },
        ..UiState::default()
    };
    let count = ui.visible_roster(&snapshot).len();
    assert_eq!(count, 124);
    let first = compose_cached(&snapshot, &mut ui, 54, 40);
    assert_painted(&ui, (0..count).collect());
    write_back(&mut ui, first);
    ui.scroll_offset = 80;
    ui.offset_authoritative = true;
    let second = compose_cached(&snapshot, &mut ui, 54, 40);
    let visible = visible_cards(&second, &snapshot, &ui);
    assert!(visible.len() < count / 2);
    assert_painted(&ui, visible);
    write_back(&mut ui, second);
    snapshot.now += Duration::from_secs(2);
    let third = compose_cached(&snapshot, &mut ui, 54, 40);
    assert_painted(&ui, (0..count).collect());
    write_back(&mut ui, third);
    let roster = ui.visible_roster(&snapshot);
    let paired = ["card-0", "card-3"].map(|id| roster.ordinal_of_id(id).unwrap());
    let solo = roster.ordinal_of_id("card-1").unwrap();
    for selected in [paired[0], solo] {
        ui.selected_index = Some(selected);
        let frame = compose_cached(&snapshot, &mut ui, 54, 40);
        let mut expected = visible_cards(&frame, &snapshot, &ui);
        expected.extend(paired);
        if selected == solo {
            expected.insert(solo);
        }
        assert_painted(&ui, expected);
        write_back(&mut ui, frame);
    }
}
