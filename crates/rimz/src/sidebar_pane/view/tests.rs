use super::*;
use crate::ids::WorkspaceId;
use crate::sidebar::test_support::pane;
use crate::store::snapshot::{AgentCard, RowCard, SidebarWorktreeKind, WorktreePrState};
use jiff::Timestamp;

#[test]
fn workflow_sections_partition_before_the_cap_and_survive_held_order() {
    let mut rows = idle_rows(10);
    for row in &mut rows[6..9] {
        row.as_agent_mut().unwrap().team = Some("crew".to_owned());
    }
    let step = &mut rows[9];
    step.name = "codex".to_owned();
    let mut group = group(rows);
    let mut run = crate::sidebar::scenario::Scenario::parse("workflow", "preset workflow")
        .unwrap()
        .at(std::time::Duration::ZERO)
        .worktree_groups[0]
        .workflow
        .clone()
        .unwrap();
    run.agent = Some((
        crate::ids::AgentKind::new_unchecked("codex"),
        "idle-9".to_owned().into(),
    ));
    group.workflow = Some(run);
    group.team = Some("crew".to_owned());
    group.pipeline = Some(crate::store::snapshot::SidebarPipeline {
        stages: vec![],
        stage: "Build".to_owned(),
        owner: None,
        started_at: None,
        stage_started_at: None,
        stage_prior_secs: 0,
        visited: Default::default(),
        done_at: None,
    });
    let snapshot = snapshot(vec![group]);
    let roster = VisibleRoster::baseline(&snapshot);
    assert_eq!(
        ids(&roster),
        ["idle-9", "idle-6", "idle-7", "idle-8", "idle-0", "idle-1"]
    );
    assert_eq!(roster.groups()[0].hidden_count(), 4);
    let held = HashSet::from(["idle-5".to_owned()]);
    let roster = VisibleRoster::new(
        &snapshot,
        &BodyLens::default(),
        &BTreeSet::new(),
        Some(&held),
    );
    assert_eq!(
        ids(&roster),
        [
            "idle-9", "idle-6", "idle-7", "idle-8", "idle-0", "idle-1", "idle-5"
        ]
    );
}

#[test]
fn rowless_workflow_visibility_has_no_phantom_ordinals() {
    let snapshot = crate::sidebar::scenario::Scenario::parse("workflow", "preset workflow")
        .unwrap()
        .at(std::time::Duration::ZERO);
    let lenses = [
        BodyLens::default(),
        BodyFilter::OpenPr.into(),
        BodyLens {
            query: Some("land".to_owned()),
            ..Default::default()
        },
        BodyFilter::Unread.into(),
        BodyFilter::Status(AgentStatus::Running).into(),
        BodyLens {
            query: Some("absent".to_owned()),
            ..Default::default()
        },
    ];
    let shown = lenses
        .iter()
        .map(|lens| {
            let roster = VisibleRoster::new(&snapshot, lens, &BTreeSet::new(), None);
            assert_eq!(roster.len(), 0);
            assert_eq!(roster.pane_at_ordinal(0), None);
            assert_eq!(roster.neighboring_group_head(0, 1), None);
            roster.groups().iter().filter(|group| group.shown()).count()
        })
        .collect::<Vec<_>>();
    assert_eq!(shown, [1, 1, 1, 0, 0, 0]);
}

#[test]
fn roster_projects_cap_expansion_filter_hold_and_control_counts_once() {
    let group = group(idle_rows(9));
    let snapshot = snapshot(vec![group]);

    let collapsed = VisibleRoster::new(&snapshot, &BodyLens::default(), &BTreeSet::new(), None);
    assert_eq!(
        ids(&collapsed),
        ["idle-0", "idle-1", "idle-2", "idle-3", "idle-4", "idle-5"]
    );
    assert_eq!(collapsed.groups()[0].natural_hidden_count(), 3);
    assert_eq!(collapsed.groups()[0].hidden_count(), 3);

    let expanded_keys = BTreeSet::from(["group-0".to_owned()]);
    let expanded = VisibleRoster::new(&snapshot, &BodyLens::default(), &expanded_keys, None);
    assert_eq!(expanded.len(), 9);
    assert!(expanded.groups()[0].expanded());
    assert_eq!(expanded.groups()[0].natural_hidden_count(), 3);
    assert_eq!(expanded.groups()[0].hidden_count(), 0);

    let filtered = VisibleRoster::new(
        &snapshot,
        &BodyLens::from(BodyFilter::Status(AgentStatus::Idle)),
        &BTreeSet::new(),
        None,
    );
    assert_eq!(filtered.len(), 9, "filters expose every matching row");

    let held_ids = HashSet::from(["idle-8".to_owned()]);
    let held = VisibleRoster::new(
        &snapshot,
        &BodyLens::default(),
        &BTreeSet::new(),
        Some(&held_ids),
    );
    assert_eq!(held.len(), 7);
    assert_eq!(held.groups()[0].natural_hidden_count(), 3);
    assert_eq!(held.groups()[0].hidden_count(), 2);
    assert!(ids(&held).contains(&"idle-8"));
}

#[test]
fn roster_keeps_attention_focus_unread_and_only_live_process_beyond_cap() {
    let mut rows = idle_rows(9);
    rows[6].unread = true;
    rows[8].card = RowCard::Agent(std::sync::Arc::new(AgentCard {
        status: AgentStatus::Failed,
        ..AgentCard::default()
    }));
    let mut attention_snapshot = snapshot(vec![group(rows)]);
    attention_snapshot.focused_pane = attention_snapshot.worktree_groups[0].rows[7]
        .pane
        .as_ref()
        .map(|pane| pane.pane_id.clone());
    let roster = VisibleRoster::baseline(&attention_snapshot);
    assert!(ids(&roster).contains(&"idle-6"));
    assert!(ids(&roster).contains(&"idle-7"));
    assert!(ids(&roster).contains(&"idle-8"));

    let mut rows = idle_rows(7);
    for row in &mut rows {
        row.inactive = true;
    }
    rows.push(process_row("shell"));
    let process_snapshot = snapshot(vec![group(rows)]);
    assert!(ids(&VisibleRoster::baseline(&process_snapshot)).contains(&"shell"));
}

#[test]
fn roster_finishes_groups_and_preserves_flat_group_ordinals() {
    let mut finished = group(vec![
        agent_row("done-unread", AgentStatus::Success),
        agent_row("done-focused", AgentStatus::Success),
    ]);
    finished.key = "finished".to_owned();
    finished.inactive = Some(crate::store::snapshot::SidebarInactive::Landed);
    std::sync::Arc::make_mut(&mut finished.rows)[0].unread = true;

    let mut active = group(vec![agent_row("active", AgentStatus::Running)]);
    active.key = "active".to_owned();
    let mut snapshot = snapshot(vec![finished, active]);
    let collapsed = VisibleRoster::baseline(&snapshot);
    assert_eq!(ids(&collapsed), ["active"]);
    assert_eq!(collapsed.groups()[0].hidden_count(), 2);
    assert_eq!(collapsed.groups()[1].range(), 0..1);

    snapshot.focused_pane = snapshot.worktree_groups[0].rows[1]
        .pane
        .as_ref()
        .map(|pane| pane.pane_id.clone());
    let focused = VisibleRoster::baseline(&snapshot);
    assert_eq!(ids(&focused), ["done-unread", "done-focused", "active"]);
    assert_eq!(focused.ordinal_of_id("active"), Some(2));
    assert_eq!(focused.neighboring_group_head(0, 1), Some(2));

    let filtered = VisibleRoster::new(
        &snapshot,
        &BodyLens::from(BodyFilter::Status(AgentStatus::Success)),
        &BTreeSet::new(),
        None,
    );
    assert!(
        ids(&filtered).is_empty(),
        "success excludes inactive groups even under focus"
    );
}

#[test]
fn roster_keeps_an_idle_parent_beyond_the_cap_when_its_child_is_focused() {
    let mut rows = idle_rows(9);
    let child = PaneId::from_parts(crate::MuxName::Zellij, "terminal_child");
    let now = rows[8].last_activity;
    rows[8].as_agent_mut().unwrap().sub_agents.push(
        serde_json::from_value(serde_json::json!({
            "id": "child", "kind": "claude", "name": "explorer", "status": "running",
            "last_activity": now, "pane": child,
        }))
        .unwrap(),
    );
    let mut snapshot = snapshot(vec![group(rows)]);
    snapshot.focused_pane = Some(child);
    let roster = VisibleRoster::baseline(&snapshot);
    assert!(ids(&roster).contains(&"idle-8"));
    assert!(roster.len() < 9);
}

#[test]
fn roster_collapses_lone_inactive_row() {
    let mut finished = group(vec![agent_row("done", AgentStatus::Success)]);
    finished.inactive = Some(crate::store::snapshot::SidebarInactive::Landed);
    let snapshot = snapshot(vec![finished]);

    let roster = VisibleRoster::baseline(&snapshot);

    assert!(ids(&roster).is_empty());
    assert_eq!(roster.groups()[0].hidden_count(), 1);
    assert_eq!(roster.groups()[0].natural_hidden_count(), 1);
}

#[test]
fn success_lens_reveals_only_active_results() {
    let mut finished = group(vec![agent_row("landed", AgentStatus::Success)]);
    finished.inactive = Some(crate::store::snapshot::SidebarInactive::Landed);
    let snapshot = snapshot(vec![
        finished,
        group(vec![agent_row("review", AgentStatus::Success)]),
    ]);
    let roster = VisibleRoster::new(
        &snapshot,
        &BodyLens::from(BodyFilter::Status(AgentStatus::Success)),
        &BTreeSet::new(),
        None,
    );
    assert_eq!(ids(&roster), ["review"]);
}

#[test]
fn open_pr_filter_keeps_every_row_in_open_pr_groups() {
    let mut open = group(idle_rows(8));
    open.key = "open".to_owned();
    std::sync::Arc::make_mut(&mut open.rows).push(process_row("open-shell"));
    open.pr_state = Some(WorktreePrState::Open);
    open.pr_number = Some(91);

    let mut closed = group(vec![agent_row("closed", AgentStatus::Failed)]);
    closed.key = "closed".to_owned();
    closed.pr_state = Some(WorktreePrState::Closed);
    let snapshot = snapshot(vec![open, closed]);

    let filtered = VisibleRoster::new(
        &snapshot,
        &BodyLens::from(BodyFilter::OpenPr),
        &BTreeSet::new(),
        None,
    );

    assert_eq!(filtered.len(), 9, "the PR lens bypasses the calm row cap");
    assert!(ids(&filtered).contains(&"open-shell"));
    assert!(!ids(&filtered).contains(&"closed"));
}

#[test]
fn query_matches_row_name_branch_and_handle_case_insensitively() {
    let mut rows = idle_rows(9);
    rows[0].name = "Auth helper".to_owned();
    rows[7].worktree_branch = Some("feature/AUTH".to_owned());
    if let Some(card) = rows[8].as_agent_mut() {
        card.handle = Some("AuthScout".to_owned());
    }
    rows.push(process_row("unrelated"));
    let mut unrelated = group(vec![agent_row("other", AgentStatus::Running)]);
    unrelated.key = "other".to_owned();
    let snapshot = snapshot(vec![group(rows), unrelated]);
    let lens = BodyLens {
        query: Some("aUtH".to_owned()),
        ..Default::default()
    };
    let roster = VisibleRoster::new(&snapshot, &lens, &BTreeSet::new(), None);
    assert_eq!(ids(&roster), ["idle-0", "idle-7", "idle-8"]);
    assert!(roster.groups()[1].is_empty());
    assert_eq!(roster.groups()[0].hidden_count(), 0);
    let lens = BodyLens {
        query: Some("@authscout".to_owned()),
        ..Default::default()
    };
    let roster = VisibleRoster::new(&snapshot, &lens, &BTreeSet::new(), None);
    assert_eq!(ids(&roster), ["idle-8"]);
}

#[test]
fn query_matches_row_team_in_a_mixed_team_group() {
    let mut rows = idle_rows(3);
    for (row, team) in rows.iter_mut().zip(["forge", "docs", "forge"]) {
        row.as_agent_mut().unwrap().team = Some(team.to_owned());
    }
    let snapshot = snapshot(vec![group(rows)]);
    let lens = BodyLens {
        query: Some("forge".to_owned()),
        ..Default::default()
    };
    let roster = VisibleRoster::new(&snapshot, &lens, &BTreeSet::new(), None);
    assert_eq!(ids(&roster), ["idle-0", "idle-2"]);
}

#[test]
fn group_query_matches_keep_all_rows_and_lift_the_fold_cap() {
    let mut matching = group(idle_rows(8));
    matching.label = "feature/Auth".to_owned();
    matching.label_qualifier = Some("RepoIdentity".to_owned());
    matching.team = Some("Forge".to_owned());
    matching.pr_number = Some(123);
    let snapshot = snapshot(vec![matching]);
    for query in ["AUTH", "identity", "forge", "#123", "123"] {
        let lens = BodyLens {
            query: Some(query.to_owned()),
            ..Default::default()
        };
        let roster = VisibleRoster::new(&snapshot, &lens, &BTreeSet::new(), None);
        assert_eq!(roster.len(), 8, "group query: {query}");
        assert_eq!(roster.groups()[0].hidden_count(), 0);
        assert_eq!(roster.groups()[0].natural_hidden_count(), 0);
    }
}

#[test]
fn query_composes_with_status_even_when_the_group_matches() {
    let mut matching = group(vec![
        agent_row("running", AgentStatus::Running),
        agent_row("idle", AgentStatus::Idle),
    ]);
    matching.label = "Auth".to_owned();
    let mut unrelated = group(vec![agent_row("other", AgentStatus::Running)]);
    unrelated.key = "other".to_owned();
    let snapshot = snapshot(vec![matching, unrelated]);
    let lens = BodyLens {
        filter: Some(BodyFilter::Status(AgentStatus::Running)),
        query: Some("auth".to_owned()),
    };
    let roster = VisibleRoster::new(&snapshot, &lens, &BTreeSet::new(), None);
    assert_eq!(ids(&roster), ["running"]);
}

#[test]
fn status_lens_shows_whole_units_with_query_alignment() {
    let mut rows = vec![
        agent_row("context", AgentStatus::Waiting),
        agent_row("match", AgentStatus::Running),
        agent_row("solo", AgentStatus::Idle),
    ];
    for row in &mut rows[..2] {
        row.as_agent_mut().unwrap().team = Some("crew".to_owned());
        row.name = row.id.clone();
    }
    let mut group = group(rows);
    let lens = BodyLens::from(BodyFilter::Status(AgentStatus::Running));
    let roster = VisibleRoster::single(&group, &lens, false, None, None);
    assert_eq!(ids(&roster), ["context", "match"]);
    let queried = BodyLens {
        query: Some("match".to_owned()),
        ..lens.clone()
    };
    let roster = VisibleRoster::single(&group, &queried, false, None, None);
    assert_eq!(ids(&roster), ["match"]);
    let queried = BodyLens {
        query: Some("context".to_owned()),
        ..lens.clone()
    };
    let roster = VisibleRoster::single(&group, &queried, false, None, None);
    assert_eq!(ids(&roster), ["context"]);
    let labelled = BodyLens {
        query: Some("main".to_owned()),
        ..lens.clone()
    };
    let roster = VisibleRoster::single(&group, &labelled, false, None, None);
    assert_eq!(ids(&roster), ["context", "match"]);
    let idle = BodyLens::from(BodyFilter::Status(AgentStatus::Sleeping));
    assert_eq!(
        VisibleRoster::single(&group, &idle, false, None, None).len(),
        0
    );
    std::sync::Arc::make_mut(&mut group.rows)[0].unread = true;
    let roster = VisibleRoster::single(
        &group,
        &BodyLens::from(BodyFilter::Unread),
        false,
        None,
        None,
    );
    assert_eq!(ids(&roster), ["context"]);
}

fn snapshot(groups: Vec<SidebarWorktreeGroup>) -> SidebarSnapshot {
    let workspace = WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap();
    let mut snapshot = SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now());
    snapshot.worktree_groups = groups.into();
    snapshot
}

fn group(rows: Vec<SidebarRow>) -> SidebarWorktreeGroup {
    SidebarWorktreeGroup {
        stalled_since: None,
        pr_stack: Default::default(),
        key: "group-0".to_owned(),
        label: "main".to_owned(),
        label_qualifier: None,
        kind: SidebarWorktreeKind::Worktree,
        team: None,
        cohort_effort: None,
        pipeline: None,
        workflow: None,
        status_counts: crate::sidebar::test_support::status_counts_from_rows(&rows),
        rows: rows.into(),
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        worktree_backed: false,
        root_lane: false,
        inactive: None,
        clean: None,
        landed: None,
        trunk_sync: None,
        pr_state: None,
        pr_queue: None,
        ci: None,
        pr_number: None,
        pr_url: None,
    }
}

fn idle_rows(count: usize) -> Vec<SidebarRow> {
    (0..count)
        .map(|index| agent_row(&format!("idle-{index}"), AgentStatus::Idle))
        .collect()
}

fn agent_row(id: &str, status: AgentStatus) -> SidebarRow {
    SidebarRow {
        checkout: None,
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
        last_activity: Timestamp::now(),
        card: RowCard::Agent(std::sync::Arc::new(AgentCard {
            status,
            ..AgentCard::default()
        })),
    }
}

fn process_row(id: &str) -> SidebarRow {
    let mut row = agent_row(id, AgentStatus::Idle);
    row.name = "zsh".to_owned();
    row.card = RowCard::Process(crate::store::snapshot::ProcessCard::default());
    row
}

fn ids<'a>(roster: &'a VisibleRoster<'_>) -> Vec<&'a str> {
    roster.rows().iter().map(|row| row.id.as_str()).collect()
}
