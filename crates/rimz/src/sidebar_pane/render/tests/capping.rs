use super::*;
use std::collections::HashSet;

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

    let mut rows = idle_rows(8);
    rows[7].pane.as_mut().expect("pane").is_focused = true;
    assert_visible(
        &rows,
        None,
        false,
        "idle-7",
        "focused row remains visible past the calm-row cap",
    );

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

    let mut lines = Vec::new();
    let mut map = Vec::new();
    let mut more_hits = Vec::new();
    let mut row_index = 0;
    let snapshot = snapshot_with(Vec::new());
    worktree_group_lines(
        &Theme::fixed(true),
        &group,
        &[],
        fixed_now(),
        54,
        &snapshot.theme.display.context_meter,
        snapshot.theme.display.card_density,
        None,
        false,
        Some(&held),
        &mut row_index,
        0,
        0,
        &CostRolls::default(),
        None,
        &mut lines,
        &mut map,
        &mut more_hits,
    );
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

    let mut lines = Vec::new();
    let mut map = Vec::new();
    let mut more_hits = Vec::new();
    let mut row_index = 0;
    let snapshot = snapshot_with(Vec::new());
    worktree_group_lines(
        &Theme::fixed(true),
        &group,
        &[],
        fixed_now(),
        54,
        &snapshot.theme.display.context_meter,
        snapshot.theme.display.card_density,
        None,
        true,
        Some(&held),
        &mut row_index,
        0,
        0,
        &CostRolls::default(),
        None,
        &mut lines,
        &mut map,
        &mut more_hits,
    );
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
    assert_eq!(more_hits.len(), 1);
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
    group_visible_rows(group, filter, expanded, held)
        .into_iter()
        .map(|row| row.id.as_str())
        .collect()
}

fn group(rows: Vec<crate::SidebarRow>) -> crate::SidebarWorktreeGroup {
    crate::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        kind: crate::SidebarWorktreeKind::Worktree,
        status_counts: Vec::new(),
        rows,
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        worktree_backed: false,
        clean: None,
        landed: None,
        trunk_sync: None,
        pr_state: None,
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
        name: "codex".to_owned(),
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
