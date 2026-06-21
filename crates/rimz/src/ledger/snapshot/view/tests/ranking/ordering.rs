use super::*;
use crate::SidebarWorktreeGroup;
use crate::agents::ATTENTION_AGE_CEILING_SECS;
use crate::ids::{MuxName, PaneId};

#[test]
fn bucket_order_puts_attention_first_and_idle_last() {
    // Scrambled input proves the sort, not the insertion order.
    let agents = [
        AgentStatus::Running,
        AgentStatus::Success,
        AgentStatus::Idle,
        AgentStatus::Paused,
        AgentStatus::Failed,
        AgentStatus::Waiting,
    ]
    .into_iter()
    .enumerate()
    .map(|(i, status)| agent_in(&format!("sess-{i}"), "/repo/main", status, 1_000 + i as i64))
    .collect::<Vec<_>>();

    let snapshot = room_with_agent_panes(Vec::new(), agents);

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.status())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec![
            Some(AgentStatus::Waiting),
            Some(AgentStatus::Failed),
            Some(AgentStatus::Paused),
            Some(AgentStatus::Success),
            Some(AgentStatus::Running),
            Some(AgentStatus::Idle),
        ],
        "attention leads; parked idle agents sink to the bottom of the group"
    );

    let counts = snapshot.worktree_groups[0]
        .status_counts
        .iter()
        .map(|count| count.status)
        .collect::<Vec<_>>();
    assert_eq!(
        counts,
        vec![
            AgentStatus::Waiting,
            AgentStatus::Failed,
            AgentStatus::Paused,
            AgentStatus::Success,
            AgentStatus::Running,
            AgentStatus::Idle,
        ],
        "status tallies stay in cockpit make-up order"
    );
}

#[test]
fn calm_order_uses_pane_ordinal_not_label() {
    let mut older = agent("codex", "older", AgentStatus::Idle, 1_000).worktree("/repo/main");
    older.pane = Some(pane("%0", "codex", "/repo/main"));
    let mut newer = agent("claude", "newer", AgentStatus::Idle, 9_000).worktree("/repo/main");
    newer.pane = Some(pane("%1", "claude", "/repo/main"));
    let mut snapshot = room_with_agent_panes(Vec::new(), vec![newer, older]);
    let row_order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    // The `%0` pane was created before `%1`, so its row leads — activity rank and
    // insertion order do not.
    assert_eq!(row_order, vec!["older", "newer"]);

    // A paneless idle row has no ordinal and tails the bucket, below every
    // pane-backed calm row.
    snapshot.worktree_groups[0]
        .rows
        .push(idle_agent_row("paneless", None));
    snapshot.sort_groups_for_presentation();
    let row_order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(row_order, vec!["older", "newer", "paneless"]);

    let mut older = agent_in("sess-b", "/repo/b", AgentStatus::Idle, 1_000);
    older.pane = Some(pane("%0", "node", "/repo/b"));
    let mut newer = agent_in("sess-a", "/repo/a", AgentStatus::Idle, 9_000);
    newer.pane = Some(pane("%1", "node", "/repo/a"));

    let snapshot = room_with_agent_panes(Vec::new(), vec![newer, older]);

    let groups = snapshot
        .worktree_groups
        .iter()
        .map(|group| group.label.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        groups,
        vec!["b", "a"],
        "groups order by their earliest pane ordinal (`b` holds `%0`); labels never decide calm order"
    );
}

#[test]
fn attention_bucket_sorts_longest_overdue_first() {
    // Scrambled input; a higher rank means more recent activity.
    let agents = vec![
        ("wait-new", AgentStatus::Waiting, 9_000),
        ("wait-old", AgentStatus::Waiting, 1_000),
        ("fail-new", AgentStatus::Failed, 8_000),
        ("fail-old", AgentStatus::Failed, 2_000),
    ]
    .into_iter()
    .map(|(id, status, rank)| agent_in(id, "/repo/main", status, rank))
    .collect::<Vec<_>>();

    let snapshot = room_with_agent_panes(Vec::new(), agents);

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    // Waiting leads failed; within each, the longest-overdue (oldest activity) rises.
    assert_eq!(order, vec!["wait-old", "wait-new", "fail-old", "fail-new"]);
}

#[test]
fn unread_rows_lead_read_attention() {
    let mut snapshot = room_with_agent_panes(
        Vec::new(),
        vec![
            agent_in("seen-wait", "/repo/a", AgentStatus::Waiting, 1_000),
            agent_in("new-done", "/repo/b", AgentStatus::Success, 2_000),
        ],
    );
    row_mut(&mut snapshot, "new-done").unread = true;
    snapshot.sort_groups_for_presentation();
    assert_eq!(
        group_labels(&snapshot),
        vec!["b", "a"],
        "unread result rows form the top inbox tier, above read attention"
    );

    let mut snapshot = room_with_agent_panes(
        Vec::new(),
        vec![
            agent_in("read-old", "/repo/main", AgentStatus::Waiting, 1_000),
            agent_in("new-wait", "/repo/main", AgentStatus::Waiting, 9_000),
        ],
    );
    row_mut(&mut snapshot, "new-wait").unread = true;
    snapshot.sort_groups_for_presentation();
    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(order, vec!["new-wait", "read-old"]);
}

#[test]
fn stale_attention_sinks_below_live_work_then_leads_inactive() {
    let snapshot = room_with_agent_panes(
        Vec::new(),
        vec![
            // Fresh idle work: live, so it outranks any stale card.
            agent_in("fresh-idle", "/repo/main", AgentStatus::Idle, 3_000),
            // An 8h-stale ask and an 8h-stale result: both cross into inactive.
            agent_in("old-wait", "/repo/main", AgentStatus::Waiting, 1_000)
                .active_ago(ATTENTION_AGE_CEILING_SECS + 1),
            agent_in("old-done", "/repo/main", AgentStatus::Success, 2_000)
                .active_ago(ATTENTION_AGE_CEILING_SECS + 1),
        ],
    );

    assert!(
        !row(&snapshot, "fresh-idle").inactive,
        "fresh work stays live"
    );
    assert!(
        row(&snapshot, "old-wait").inactive,
        "a stale ask sinks like any stale card — attention no longer pins it live"
    );
    assert!(row(&snapshot, "old-done").inactive);
    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec!["fresh-idle", "old-wait", "old-done"],
        "live idle leads the stale ask; within the inactive band the ask leads the result"
    );
}

#[test]
fn inactive_success_sinks_below_process_rows() {
    let mut snapshot = room_with_agent_panes(
        Vec::new(),
        vec![
            agent_in("old-done", "/repo/main", AgentStatus::Success, 1_000)
                .active_ago(ATTENTION_AGE_CEILING_SECS + 1),
        ],
    );
    snapshot.worktree_groups[0]
        .rows
        .push(process_row("zsh", "/repo/main"));
    snapshot.sort_groups_for_presentation();

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec!["zsh", "old-done"],
        "process rows sit above inactive calm agent rows"
    );
}

#[test]
fn inactive_idle_uses_the_hour_boundary_strictly() {
    let snapshot = room_with_agent_panes(
        Vec::new(),
        vec![
            agent_in("fresh-idle", "/repo/main", AgentStatus::Idle, 1_000)
                .active_ago(ATTENTION_AGE_CEILING_SECS),
            agent_in("old-idle", "/repo/main", AgentStatus::Idle, 2_000)
                .active_ago(ATTENTION_AGE_CEILING_SECS + 1),
        ],
    );

    assert!(!row(&snapshot, "fresh-idle").inactive);
    assert!(row(&snapshot, "old-idle").inactive);
}

#[test]
fn inactive_groups_sink_below_process_groups() {
    let mut snapshot = room_with_agent_panes(
        Vec::new(),
        vec![
            agent_in("old-done", "/repo/a", AgentStatus::Success, 1_000)
                .active_ago(ATTENTION_AGE_CEILING_SECS + 1),
            agent_in("fresh-idle", "/repo/c", AgentStatus::Idle, 2_000),
        ],
    );
    snapshot.worktree_groups.push(SidebarWorktreeGroup {
        key: "/repo/b".to_owned(),
        label: "b".to_owned(),
        kind: SidebarWorktreeKind::Worktree,
        status_counts: Vec::new(),
        rows: vec![process_row("zsh", "/repo/b")],
        hidden_count: 0,
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        clean: None,
        landed: None,
    });
    snapshot.sort_groups_for_presentation();

    assert_eq!(
        group_labels(&snapshot),
        vec!["c", "b", "a"],
        "fresh calm groups outrank process groups, and inactive calm groups sink below both"
    );
}

#[test]
fn cap_keeps_inactive_success_above_hidden_idle_tail() {
    let mut agents = vec![
        agent_in("old-done", "/repo/main", AgentStatus::Success, 1_000)
            .active_ago(ATTENTION_AGE_CEILING_SECS + 1),
    ];
    agents.extend((0..10).map(|i| {
        agent_in(
            &format!("old-idle-{i}"),
            "/repo/main",
            AgentStatus::Idle,
            2_000 + i,
        )
        .active_ago(ATTENTION_AGE_CEILING_SECS + 1)
    }));
    let snapshot = room_with_agent_panes(Vec::new(), agents);

    assert!(
        snapshot.worktree_groups[0]
            .rows
            .iter()
            .any(|row| row.id == "old-done"),
        "inactive success remains visible even when the inactive idle tail is capped"
    );
    assert!(snapshot.worktree_groups[0].hidden_count > 0);
}

fn row_mut<'a>(snapshot: &'a mut SidebarSnapshot, id: &str) -> &'a mut SidebarRow {
    snapshot
        .worktree_groups
        .iter_mut()
        .flat_map(|group| group.rows.iter_mut())
        .find(|row| row.id == id)
        .unwrap_or_else(|| panic!("row {id} present"))
}

fn process_row(id: &str, worktree: &str) -> SidebarRow {
    SidebarRow {
        id: id.to_owned(),
        name: id.to_owned(),
        pane: Some(PaneRef::from_id(PaneId::from_parts(MuxName::Tmux, id))),
        worktree_path: Some(worktree.to_owned()),
        worktree_branch: None,
        unread: false,
        inactive: false,
        last_activity: epoch(),
        card: crate::RowCard::Process(crate::ProcessCard::default()),
    }
}

fn idle_agent_row(id: &str, pane_raw: Option<&str>) -> SidebarRow {
    SidebarRow {
        id: id.to_owned(),
        name: id.to_owned(),
        pane: pane_raw.map(|raw| PaneRef::from_id(PaneId::from_parts(MuxName::Tmux, raw))),
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: None,
        unread: false,
        inactive: false,
        last_activity: epoch(),
        card: crate::RowCard::Agent(Box::new(crate::ledger::snapshot::row::AgentCard {
            status: Some(AgentStatus::Idle),
            ..Default::default()
        })),
    }
}

fn group_labels(snapshot: &SidebarSnapshot) -> Vec<String> {
    snapshot
        .worktree_groups
        .iter()
        .map(|group| group.label.clone())
        .collect()
}
