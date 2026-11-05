use super::*;
use crate::agents::ParkDemotion;
use crate::store::snapshot::{SidebarInactive, SidebarWorkflow, SidebarWorktreeGroup};
use crate::store::workflow::RunState;
use std::sync::Arc;

fn workflow(path: &str, state: RunState) -> SidebarWorkflow {
    serde_json::from_value(serde_json::json!({
        "run": crate::ids::WorkflowRunId::new(), "workflow": "land", "scope_path": path,
        "state": state, "track": [],
        "current": {"name": "watch", "kind": "check", "reached": false},
        "detour_parent": null, "visits": 0, "capped": false, "word": "queued",
        "since": ago(3601), "hold_limit": null, "entered_at": ago(3601),
        "created_at": ago(7200), "agent": null
    }))
    .unwrap()
}

fn base() -> SidebarSnapshot {
    base_with(Vec::new())
}

fn base_with(agents: Vec<AgentState>) -> SidebarSnapshot {
    let marked = ["lane", "members"].map(|name| (PathBuf::from(format!("/trees/{name}")), name));
    room(agents)
        .with_project_root(Some(PathBuf::from("/repo")))
        .with_worktree_roots(
            marked.iter().map(|(path, _)| path.clone()).collect(),
            marked
                .iter()
                .map(|(path, name)| (path.clone(), (*name).to_owned()))
                .collect(),
        )
}

#[test]
fn live_runs_seed_groups_but_terminal_and_external_runs_do_not() {
    let snapshot = base();
    for state in [
        RunState::Active,
        RunState::Parked,
        RunState::Done,
        RunState::Failed,
        RunState::Canceled,
    ] {
        let runs = [
            workflow("/trees/lane", state),
            workflow("/elsewhere", state),
        ];
        let groups = snapshot.build_worktree_groups(Vec::new(), &ParkDemotion::default(), &runs);
        let expected = usize::from(!state.is_terminal());
        assert_eq!(
            groups.len(),
            expected,
            "{state:?} contributes only a live non-external scope"
        );
        if let Some(group) = groups.first() {
            assert_eq!(group.key, "channel:lane");
            assert_eq!(group.workflow.as_ref(), Some(&runs[0]));
            assert!(group.rows.is_empty());
            assert!(group.unit_statuses().is_empty());
            assert!(!group.collapses());
            assert!(group.pipeline_owner().is_none());
        }
    }
}

#[test]
fn step_launch_cwd_joins_the_run_group_and_newest_run_wins() {
    let mut step = agent("codex", "sweeper", AgentStatus::Running, 0).worktree("/trees/lane");
    step.channel = None;
    let snapshot = base();
    let older = workflow("/trees/lane", RunState::Active);
    let mut newer = older.clone();
    newer.run = crate::ids::WorkflowRunId::new();
    newer.created_at = ago(600);
    let groups = snapshot.build_worktree_groups(
        vec![row_from_agent(&step, epoch())],
        &ParkDemotion::default(),
        &[newer.clone(), older],
    );
    assert_eq!(
        groups.len(),
        1,
        "the launcher's channel=None and cwd=scope share one resolver"
    );
    assert_eq!(groups[0].key, "channel:lane");
    assert_eq!(groups[0].rows[0].id, "sweeper");
    assert_eq!(groups[0].workflow.as_ref(), Some(&newer));
}

#[test]
fn a_run_on_a_branch_split_path_joins_a_split_group() {
    let run = workflow("/repo/shared", RunState::Active);
    let mut feature = agent("codex", "fixer", AgentStatus::Running, 1_000)
        .worktree("/repo/shared")
        .branch("feature");
    feature.channel = None;
    let mut main = agent("codex", "other", AgentStatus::Idle, 1_100)
        .worktree("/repo/shared")
        .branch("main");
    main.channel = None;
    let groups_for = |agents: Vec<AgentState>| {
        let rows = agents
            .iter()
            .map(|agent| row_from_agent(agent, epoch()))
            .collect();
        base_with(agents).build_worktree_groups(
            rows,
            &ParkDemotion::default(),
            std::slice::from_ref(&run),
        )
    };
    let run_group = |groups: &[SidebarWorktreeGroup]| {
        assert_eq!(
            groups
                .iter()
                .map(|group| group.key.as_str())
                .collect::<std::collections::BTreeSet<_>>(),
            ["/repo/shared\nfeature", "/repo/shared\nmain"].into(),
            "the run mints no bare-path group beside the split ones"
        );
        let held = groups.iter().filter(|group| group.workflow.is_some());
        held.map(|group| group.label.clone()).collect::<Vec<_>>()
    };

    assert_eq!(
        run_group(&groups_for(vec![feature.clone(), main.clone()])),
        ["main"],
        "without a step row the newest split group holds the run"
    );
    feature.workflow = Some(crate::agents::WorkflowAttemptRef {
        run: run.run.clone(),
        seq: 1,
        role: crate::agents::WorkflowAttemptRole::Step,
    });
    assert_eq!(
        run_group(&groups_for(vec![feature, main])),
        ["feature"],
        "the step root's group holds the run"
    );
}

#[test]
fn pane_exclusion_keeps_unrelated_and_newly_rowless_run_groups() {
    let live = pane("%1", "zsh", "/trees/lane");
    let mut snapshot = base().with_live_panes(vec![live.clone()], None);
    Arc::make_mut(&mut snapshot.worktree_groups)[0].workflow =
        Some(workflow("/trees/lane", RunState::Active));
    let mut empty = snapshot.worktree_groups[0].clone();
    empty.key = "channel:other".into();
    empty.rows = Vec::new().into();
    Arc::make_mut(&mut snapshot.worktree_groups).push(empty);
    snapshot.remove_pane_rows(&"tmux:%absent".parse().unwrap());
    assert_eq!(
        snapshot.worktree_groups.len(),
        2,
        "unrelated pane exclusion keeps rowless runs"
    );
    snapshot.remove_pane_rows(&live.pane_id);
    assert_eq!(
        snapshot.worktree_groups.len(),
        2,
        "closing the step pane leaves its run"
    );
    assert!(
        snapshot
            .worktree_groups
            .iter()
            .all(|group| group.rows.is_empty() && group.workflow.is_some())
    );
}

#[test]
fn run_inactivity_uses_landed_window_and_latest_node_or_member() {
    let snapshot = base().with_live_panes(vec![pane("%1", "zsh", "/trees/lane")], None);
    let template = snapshot.worktree_groups[0].clone();
    for state in [RunState::Active, RunState::Parked] {
        for (landed, age, row_age, expected) in [
            (false, 90000, None, None),
            (true, 3600, None, None),
            (true, 3601, None, Some(SidebarInactive::Landed)),
            (true, 3601, Some(30), None),
            (true, 30, Some(90000), None),
        ] {
            let mut projected = snapshot.clone();
            let mut group = template.clone();
            let mut run = workflow("/trees/lane", state);
            run.entered_at = ago(age);
            group.workflow = Some(run);
            group.clean = Some(true);
            group.pr_state = Some(if landed {
                WorktreePrState::Merged
            } else {
                WorktreePrState::Open
            });
            if let Some(age) = row_age {
                Arc::make_mut(&mut group.rows)[0].last_activity = ago(age);
            } else {
                group.rows = Vec::new().into();
            }
            projected.worktree_groups = vec![group].into();
            projected.sort_groups_for_presentation();
            assert_eq!(
                projected.worktree_groups[0].inactive, expected,
                "{state:?}, landed={landed}, node={age}, member={row_age:?}"
            );
        }
    }
}

#[test]
fn rowless_runs_follow_members_then_newest_node_entry() {
    let mut snapshot = base().with_live_panes(vec![pane("%1", "zsh", "/trees/members")], None);
    let mut older = snapshot.worktree_groups[0].clone();
    older.key = "channel:a-old".into();
    older.label = "a-old".into();
    older.rows = Vec::new().into();
    older.workflow = Some(workflow("/trees/a-old", RunState::Active));
    let mut newer = older.clone();
    newer.key = "channel:z-new".into();
    newer.label = "z-new".into();
    newer.workflow.as_mut().unwrap().entered_at = ago(5);
    let member = snapshot.worktree_groups[0].clone();
    snapshot.worktree_groups = vec![older, newer, member].into();
    snapshot.sort_groups_for_presentation();
    assert_eq!(
        snapshot
            .worktree_groups
            .iter()
            .map(|group| group.label.as_str())
            .collect::<Vec<_>>(),
        ["members", "z-new", "a-old"]
    );
}
