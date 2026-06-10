use super::*;

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
fn calm_bucket_holds_stable_spawn_order() {
    // Idle agents with distinct spawn times (and one with no pane). The
    // bucket holds spawn order — oldest first — regardless of activity.
    let specs: [(&str, Option<i64>); 4] = [
        ("late", Some(100)),
        ("nopane", None),
        ("early", Some(300)),
        ("mid", Some(200)),
    ];
    let agents = specs
        .into_iter()
        .enumerate()
        .map(|(i, (id, ago_secs))| {
            let mut agent = agent_in(id, "/repo/main", AgentStatus::Idle, 1_000 + i as i64);
            agent.pane =
                ago_secs.map(|secs| pane_started(&format!("%{i}"), "/repo/main", ago(secs)));
            agent
        })
        .collect::<Vec<_>>();

    let snapshot = room_with_agent_panes(Vec::new(), agents);

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    // Oldest pane first; the paneless row keys on its `registered_at` — newer
    // than every pane start here — and falls to the bucket tail.
    assert_eq!(order, vec!["early", "mid", "late", "nopane"]);
}

#[test]
fn new_idle_agent_appends_below_calm_work() {
    // A brand-new agent registers idle, so wherever the snapshot catches it —
    // before or after its first prompt — it never lands above finished or
    // working agents: idle is the calm region's bottom bucket.
    let mut done = agent_in("done", "/repo/main", AgentStatus::Success, 1_000);
    done.pane = Some(pane_started("%0", "/repo/main", ago(600)));
    let mut work = agent_in("work", "/repo/main", AgentStatus::Running, 1_001);
    work.pane = Some(pane_started("%1", "/repo/main", ago(500)));
    let mut fresh = agent_in("fresh", "/repo/main", AgentStatus::Idle, 1_002);
    fresh.pane = Some(pane_started("%2", "/repo/main", ago(5)));

    let snapshot = room_with_agent_panes(Vec::new(), vec![fresh, work, done]);

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec!["done", "work", "fresh"],
        "the new idle card appends at the bottom of the calm region"
    );
}

#[test]
fn paneless_calm_order_uses_registration_not_label() {
    let mut older = agent("codex", "older", AgentStatus::Idle, 1_000).worktree("/repo/main");
    older.pane = Some(pane("%0", "codex", "/repo/main"));
    let mut newer = agent("claude", "newer", AgentStatus::Idle, 9_000).worktree("/repo/main");
    newer.pane = Some(pane("%1", "claude", "/repo/main"));
    let snapshot = room_with_agent_panes(Vec::new(), vec![newer, older]);
    let row_order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(row_order, vec!["older", "newer"]);

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
        "row and group spawn order hold without pane starts; labels never decide calm order"
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
fn status_leads_and_unread_breaks_status_ties() {
    let agents = vec![
        agent_in("seen-wait", "/repo/main", AgentStatus::Waiting, 1_000),
        agent_in("new-done", "/repo/main", AgentStatus::Success, 9_000),
        agent_in("seen-fail", "/repo/main", AgentStatus::Failed, 3_000),
        agent_in("seen-done", "/repo/main", AgentStatus::Success, 1_000),
    ];
    let mut snapshot = room_with_agent_panes(Vec::new(), agents);
    row_mut(&mut snapshot, "new-done").unread = true;
    snapshot.sort_groups_for_presentation();

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec!["seen-wait", "seen-fail", "new-done", "seen-done"],
        "status is primary; unread only lifts the success row inside its bucket"
    );
}

#[test]
fn unread_breaks_ties_but_not_status_buckets() {
    let mut snapshot = room_with_agent_panes(
        Vec::new(),
        vec![
            agent_in("seen-wait", "/repo/a", AgentStatus::Waiting, 1_000),
            agent_in("new-done", "/repo/b", AgentStatus::Success, 2_000),
        ],
    );
    row_mut(&mut snapshot, "new-done").unread = true;
    snapshot.sort_groups_for_presentation();
    assert_eq!(group_labels(&snapshot), vec!["a", "b"]);

    let mut snapshot = room_with_agent_panes(
        Vec::new(),
        vec![
            agent_in("read-done", "/repo/a", AgentStatus::Success, 1_000),
            agent_in("new-done", "/repo/b", AgentStatus::Success, 9_000),
        ],
    );
    row_mut(&mut snapshot, "new-done").unread = true;
    snapshot.sort_groups_for_presentation();
    assert_eq!(group_labels(&snapshot), vec!["b", "a"]);

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

fn row_mut<'a>(snapshot: &'a mut SidebarSnapshot, id: &str) -> &'a mut SidebarRow {
    snapshot
        .worktree_groups
        .iter_mut()
        .flat_map(|group| group.rows.iter_mut())
        .find(|row| row.id == id)
        .unwrap_or_else(|| panic!("row {id} present"))
}

fn group_labels(snapshot: &SidebarSnapshot) -> Vec<String> {
    snapshot
        .worktree_groups
        .iter()
        .map(|group| group.label.clone())
        .collect()
}
