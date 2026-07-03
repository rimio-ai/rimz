use super::*;
use crate::SidebarWorktreeGroup;
use crate::agents::ATTENTION_AGE_CEILING_SECS;
use crate::ids::{MuxName, PaneId};
use crate::ledger::snapshot::group_live_agents_by_worktree;

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
fn hot_band_score_interleaves_attention_by_overdue_heat() {
    let snapshot = room_with_agent_panes(
        Vec::new(),
        vec![
            agent_in("fresh-wait", "/repo/main", AgentStatus::Waiting, 1_000).active_ago(120),
            agent_in("old-fail", "/repo/main", AgentStatus::Failed, 2_000).active_ago(50 * 60),
            agent_in("calm", "/repo/main", AgentStatus::Success, 3_000).active_ago(60),
        ],
    );

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec!["old-fail", "fresh-wait", "calm"],
        "heat lets an older failure outrank a fresh ask while attention still leads calm work"
    );
}

#[test]
fn warm_band_decay_can_sink_old_attention_below_recent_calm() {
    let snapshot = room_with_agent_panes(
        Vec::new(),
        vec![
            agent_in("old-ask", "/repo/main", AgentStatus::Waiting, 1_000).active_ago(20 * 60 * 60),
            agent_in("recent-done", "/repo/main", AgentStatus::Success, 2_000)
                .active_ago(2 * 60 * 60),
        ],
    );

    assert!(row(&snapshot, "old-ask").inactive);
    assert!(row(&snapshot, "recent-done").inactive);
    assert!(!row(&snapshot, "old-ask").archived);
    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec!["recent-done", "old-ask"],
        "warm decay favors recent finished work over a much older ask"
    );
}

#[test]
fn archive_band_parks_rows_below_warm_and_keeps_attention_above_idle() {
    let snapshot = room_with_agent_panes(
        Vec::new(),
        vec![
            agent_in("warm-idle", "/repo/main", AgentStatus::Idle, 1_000).active_ago(23 * 60 * 60),
            agent_in("archived-idle", "/repo/main", AgentStatus::Idle, 2_000)
                .active_ago(25 * 60 * 60),
            agent_in("archived-ask", "/repo/main", AgentStatus::Waiting, 3_000)
                .active_ago(25 * 60 * 60),
        ],
    );

    assert!(!row(&snapshot, "warm-idle").archived);
    assert!(row(&snapshot, "archived-ask").archived);
    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec!["warm-idle", "archived-ask", "archived-idle"],
        "archive rows never compete with warm rows, but attention still leads inside archive"
    );
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
fn unread_band_uses_flat_status_order_not_decayed_score() {
    let mut snapshot = room_with_agent_panes(
        Vec::new(),
        vec![
            agent_in("old-fail", "/repo/main", AgentStatus::Failed, 1_000).active_ago(50 * 60),
            agent_in("fresh-wait", "/repo/main", AgentStatus::Waiting, 2_000).active_ago(120),
        ],
    );
    row_mut(&mut snapshot, "old-fail").unread = true;
    row_mut(&mut snapshot, "fresh-wait").unread = true;
    snapshot.sort_groups_for_presentation();

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec!["fresh-wait", "old-fail"],
        "unread status order stays waiting before failed even when hot score would interleave them"
    );
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
fn agent_cards_lead_process_rows_even_when_inactive() {
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
        vec!["old-done", "zsh"],
        "agent cards lead the channel; the process row tails even an inactive agent"
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
        trunk_sync: None,
        pr_state: None,
    });
    snapshot.sort_groups_for_presentation();

    assert_eq!(
        group_labels(&snapshot),
        vec!["c", "b", "a"],
        "fresh calm groups outrank process groups, and inactive calm groups sink below both"
    );
}

#[test]
fn live_process_keeps_mixed_group_above_inactive_groups() {
    let mut snapshot = room_with_agent_panes(
        Vec::new(),
        vec![
            agent_in("old-a", "/repo/a", AgentStatus::Success, 1_000)
                .active_ago(ATTENTION_AGE_CEILING_SECS + 1),
            agent_in("old-b", "/repo/b", AgentStatus::Success, 2_000)
                .active_ago(ATTENTION_AGE_CEILING_SECS + 1),
            agent_in("fresh-c", "/repo/c", AgentStatus::Idle, 3_000),
        ],
    );
    snapshot
        .worktree_groups
        .iter_mut()
        .find(|group| group.label == "b")
        .expect("group b present")
        .rows
        .push(process_row("zsh", "/repo/b"));
    snapshot.sort_groups_for_presentation();

    assert_eq!(
        group_labels(&snapshot),
        vec!["c", "b", "a"],
        "a live process keeps its mixed group above inactive-only groups"
    );
    let mixed_order = snapshot
        .worktree_groups
        .iter()
        .find(|group| group.label == "b")
        .expect("group b present")
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        mixed_order,
        vec!["old-b", "zsh"],
        "agent rows still lead inside the mixed group"
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

#[test]
fn cohort_rows_hold_launch_order_across_status_and_unread_churn() {
    let mut snapshot = room_with_agent_panes(
        Vec::new(),
        vec![
            cohort_agent(
                "cohort-second",
                AgentStatus::Success,
                1_000,
                "launch_group_1",
                Some(1),
                "%1",
            ),
            cohort_agent(
                "cohort-tail",
                AgentStatus::Running,
                2_000,
                "launch_group_1",
                None,
                "%2",
            ),
            cohort_agent(
                "cohort-first",
                AgentStatus::Waiting,
                3_000,
                "launch_group_1",
                Some(0),
                "%3",
            ),
            agent_in("loose-unread", "/repo/main", AgentStatus::Success, 4_000).in_pane("%0"),
        ],
    );
    row_mut(&mut snapshot, "loose-unread").unread = true;
    snapshot.sort_groups_for_presentation();

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec![
            "loose-unread",
            "cohort-first",
            "cohort-second",
            "cohort-tail",
        ],
        "loose unread rows stay above the block; cohort rows keep launch order and ordinal-less members tail"
    );
}

#[test]
fn inline_cohorts_stay_contiguous_without_interleaving() {
    let snapshot = room_with_agent_panes(
        Vec::new(),
        vec![
            cohort_agent(
                "g1-a",
                AgentStatus::Success,
                1_000,
                "launch_g1",
                Some(0),
                "%1",
            ),
            cohort_agent(
                "g2-a",
                AgentStatus::Success,
                2_000,
                "launch_g2",
                Some(0),
                "%2",
            ),
            cohort_agent(
                "g2-b",
                AgentStatus::Success,
                3_000,
                "launch_g2",
                Some(1),
                "%3",
            ),
            cohort_agent(
                "g1-b",
                AgentStatus::Success,
                4_000,
                "launch_g1",
                Some(1),
                "%4",
            ),
        ],
    );

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec!["g1-a", "g1-b", "g2-a", "g2-b"],
        "block min pane ordinal orders cohorts; internal launch ordinal keeps each cohort contiguous"
    );
}

#[test]
fn team_blocks_rank_by_derived_state_and_stay_contiguous() {
    let blocked_a = cohort_agent(
        "blocked-a",
        AgentStatus::Waiting,
        1_000,
        "blocked",
        Some(0),
        "%4",
    );
    let blocked_b = cohort_agent(
        "blocked-b",
        AgentStatus::Success,
        2_000,
        "blocked",
        Some(1),
        "%5",
    );
    let success_a = cohort_agent(
        "success-a",
        AgentStatus::Success,
        3_000,
        "success",
        Some(0),
        "%6",
    );
    let working_a = cohort_agent(
        "working-a",
        AgentStatus::Running,
        5_000,
        "working",
        Some(0),
        "%0",
    );
    let idle_a = cohort_agent("idle-a", AgentStatus::Idle, 7_000, "idle", Some(0), "%2");

    let snapshot = room_with_agent_panes(
        Vec::new(),
        vec![blocked_b, idle_a, working_a, success_a, blocked_a],
    );

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec!["blocked-a", "blocked-b", "success-a", "working-a", "idle-a",],
        "blocked teams lead contiguously; then success, working, and idle blocks follow derived state"
    );
}

#[test]
fn inactive_cohort_sinks_until_unread_member_clamps_it_live() {
    let mut snapshot = room_with_agent_panes(
        Vec::new(),
        vec![
            agent_in("fresh-idle", "/repo/main", AgentStatus::Idle, 1_000),
            cohort_agent(
                "old-a",
                AgentStatus::Success,
                2_000,
                "launch_group_1",
                Some(0),
                "%1",
            )
            .active_ago(ATTENTION_AGE_CEILING_SECS + 1),
            cohort_agent(
                "old-b",
                AgentStatus::Idle,
                3_000,
                "launch_group_1",
                Some(1),
                "%2",
            )
            .active_ago(ATTENTION_AGE_CEILING_SECS + 1),
        ],
    );
    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(order, vec!["fresh-idle", "old-a", "old-b"]);

    row_mut(&mut snapshot, "old-b").unread = true;
    snapshot.sort_groups_for_presentation();
    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec!["old-a", "old-b", "fresh-idle"],
        "one unread member keeps the block in the live band without changing its internal order"
    );
}

#[test]
fn listing_roster_order_matches_row_order_when_rows_have_no_sidebar_state() {
    let mut cohort_first =
        agent_in("cohort-first", "/repo/main", AgentStatus::Waiting, 3_000).in_pane("%2");
    cohort_first.launch_group = Some("launch_group_1".to_owned());
    cohort_first.launch_ordinal = Some(0);
    let mut cohort_second =
        agent_in("cohort-second", "/repo/main", AgentStatus::Running, 2_000).in_pane("%3");
    cohort_second.launch_group = Some("launch_group_1".to_owned());
    cohort_second.launch_ordinal = Some(1);
    let agents = vec![
        agent_in("done-late", "/repo/main", AgentStatus::Success, 6_000).in_pane("%5"),
        agent_in(
            "running-early-pane",
            "/repo/main",
            AgentStatus::Running,
            5_000,
        )
        .in_pane("%0"),
        agent_in("wait", "/repo/main", AgentStatus::Waiting, 1_000).in_pane("%9"),
        agent_in("done-early", "/repo/main", AgentStatus::Success, 4_000).in_pane("%1"),
        cohort_second,
        agent_in("paneless", "/repo/main", AgentStatus::Idle, 7_000),
        cohort_first,
    ];

    let mut row_snapshot = room(Vec::new(), Vec::new());
    row_snapshot.worktree_groups = vec![SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        kind: SidebarWorktreeKind::Worktree,
        status_counts: Vec::new(),
        rows: agents
            .iter()
            .map(|agent| row_from_agent(agent, epoch()))
            .collect(),
        hidden_count: 0,
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        clean: None,
        landed: None,
        trunk_sync: None,
        pr_state: None,
    }];
    row_snapshot.sort_groups_for_presentation();
    let row_order = row_snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.as_str())
        .collect::<Vec<_>>();

    let listing_snapshot = room(Vec::new(), agents);
    let refs = listing_snapshot.agents.iter().collect::<Vec<_>>();
    let groups = group_live_agents_by_worktree(&refs, &listing_snapshot);
    let listing_order = groups
        .iter()
        .flat_map(|group| &group.agents)
        .map(|agent| agent.agent_id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(row_order, listing_order);
}

fn row_mut<'a>(snapshot: &'a mut SidebarSnapshot, id: &str) -> &'a mut SidebarRow {
    snapshot
        .worktree_groups
        .iter_mut()
        .flat_map(|group| group.rows.iter_mut())
        .find(|row| row.id == id)
        .unwrap_or_else(|| panic!("row {id} present"))
}

fn cohort_agent(
    id: &str,
    status: AgentStatus,
    rank: i64,
    launch_group: &str,
    launch_ordinal: Option<u32>,
    pane_raw: &str,
) -> AgentState {
    let mut agent = agent_in(id, "/repo/main", status, rank).in_pane(pane_raw);
    agent.launch_group = Some(launch_group.to_owned());
    agent.launch_ordinal = launch_ordinal;
    agent
}

fn process_row(id: &str, worktree: &str) -> SidebarRow {
    SidebarRow {
        id: id.to_owned(),
        name: id.to_owned(),
        pane: Some(PaneRef::from_id(PaneId::from_parts(MuxName::Tmux, id))),
        worktree_path: Some(worktree.to_owned()),
        worktree_branch: None,
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
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
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: epoch(),
        card: crate::RowCard::Agent(Box::new(crate::ledger::snapshot::row::AgentCard {
            status: AgentStatus::Idle,
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
