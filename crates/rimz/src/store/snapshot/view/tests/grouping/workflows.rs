use super::*;
use crate::agents::ParkDemotion;
use crate::store::snapshot::{SidebarInactive, SidebarWorkflow};
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
        let groups =
            snapshot.build_worktree_groups(Vec::new(), &ParkDemotion::default(), &runs, &[]);
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
fn open_pr_roots_seed_one_rowless_group_per_listed_worktree() {
    let snapshot = base();
    let seeds = ["/trees/lane", "/elsewhere", "/repo"].map(str::to_owned);
    let groups = snapshot.build_worktree_groups(Vec::new(), &ParkDemotion::default(), &[], &seeds);
    assert_eq!(
        groups
            .iter()
            .map(|group| group.key.as_str())
            .collect::<Vec<_>>(),
        ["channel:lane"],
        "an external path and the root lane seed nothing"
    );
    assert!(groups[0].rows.is_empty() && groups[0].workflow.is_none());
    assert!(groups[0].unit_statuses().is_empty());
    assert!(groups[0].pipeline_owner().is_none());

    let mut member = agent("codex", "member", AgentStatus::Idle, 0).worktree("/trees/lane");
    member.channel = Some("lane".to_owned());
    let run = workflow("/trees/lane", RunState::Active);
    let groups = base_with(vec![member.clone()]).build_worktree_groups(
        vec![row_from_agent(&member, epoch())],
        &ParkDemotion::default(),
        std::slice::from_ref(&run),
        &seeds,
    );
    assert_eq!(groups.len(), 1, "the seed merges into the channel's group");
    assert_eq!(groups[0].key, "channel:lane");
    assert_eq!(groups[0].rows.len(), 1);
    assert_eq!(groups[0].workflow.as_ref(), Some(&run));
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
        &[],
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
fn a_run_on_a_path_with_two_branches_joins_its_one_group() {
    let run = workflow("/repo/shared", RunState::Active);
    let mut feature = agent("codex", "fixer", AgentStatus::Running, 1_000)
        .worktree("/repo/shared")
        .branch("feature");
    feature.channel = None;
    let mut main = agent("codex", "other", AgentStatus::Idle, 1_100)
        .worktree("/repo/shared")
        .branch("main");
    main.channel = None;
    let agents = vec![feature, main];
    let rows = agents
        .iter()
        .map(|agent| row_from_agent(agent, epoch()))
        .collect();

    let groups = base_with(agents).build_worktree_groups(
        rows,
        &ParkDemotion::default(),
        std::slice::from_ref(&run),
        &[],
    );

    assert_eq!(groups.len(), 1, "the run mints no group beside the path's");
    assert_eq!(groups[0].key, "/repo/shared");
    assert_eq!(groups[0].rows.len(), 2);
    assert_eq!(groups[0].workflow.as_ref(), Some(&run));
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
    let mut open_pr = empty.clone();
    open_pr.key = "channel:open-pr".into();
    open_pr.workflow = None;
    open_pr.pr_state = Some(WorktreePrState::Open);
    let mut merged_pr = open_pr.clone();
    merged_pr.key = "channel:merged-pr".into();
    merged_pr.pr_state = Some(WorktreePrState::Merged);
    Arc::make_mut(&mut snapshot.worktree_groups).extend([empty, open_pr, merged_pr]);
    snapshot.remove_pane_rows(&"tmux:%absent".parse().unwrap());
    assert_eq!(
        snapshot
            .worktree_groups
            .iter()
            .map(|group| group.key.as_str())
            .collect::<Vec<_>>(),
        ["channel:lane", "channel:other", "channel:open-pr"],
        "unrelated pane exclusion keeps rowless runs and open PRs"
    );
    snapshot.remove_pane_rows(&live.pane_id);
    assert_eq!(
        snapshot.worktree_groups.len(),
        3,
        "closing the step pane leaves its run"
    );
    assert!(snapshot.worktree_groups.iter().all(|group| {
        group.rows.is_empty()
            && (group.workflow.is_some() || group.pr_state == Some(WorktreePrState::Open))
    }));
}

#[test]
fn live_runs_and_open_prs_prevent_inactivity_and_ended_ones_use_member_clocks() {
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for state in [
        RunState::Active,
        RunState::Parked,
        RunState::Done,
        RunState::Failed,
        RunState::Canceled,
    ] {
        use WorktreePrState::{Closed, Merged, Open};
        let cases = if state.is_terminal() {
            vec![
                (Some(Merged), Some(3601), Some(SidebarInactive::Landed)),
                (Some(Merged), Some(3600), None),
                (Some(Closed), Some(3601), Some(SidebarInactive::Landed)),
                (None, Some(90000), Some(SidebarInactive::Unlanded)),
                (Some(Open), Some(90000), None),
            ]
        } else {
            vec![
                (Some(Open), None, None),
                (Some(Merged), None, None),
                (Some(Open), Some(90000), None),
                (Some(Merged), Some(90000), None),
            ]
        };
        for (pr_state, row_age, inactive) in cases {
            let members = row_age
                .map(|age| {
                    let mut member =
                        agent("codex", "member", AgentStatus::Idle, 0).worktree("/trees/lane");
                    member.registered_at = Some(ago(age));
                    member.last_activity = ago(age);
                    member
                })
                .into_iter()
                .collect::<Vec<_>>();
            let rows = members
                .iter()
                .map(|member| row_from_agent(member, epoch()))
                .collect();
            let mut projected = base_with(members);
            let mut run = workflow("/trees/lane", state);
            run.entered_at = if state.is_terminal() {
                epoch()
            } else {
                ago(90000)
            };
            let mut groups =
                projected.build_worktree_groups(rows, &ParkDemotion::default(), &[run], &[]);
            let group = &mut groups[0];
            group.clean = Some(true);
            group.pr_state = pr_state;
            projected.worktree_groups = groups.into();
            projected.sort_groups_for_presentation();
            let group = &projected.worktree_groups[0];
            actual.push((
                state,
                pr_state,
                row_age,
                group.workflow.is_some(),
                group.inactive,
            ));
            expected.push((state, pr_state, row_age, !state.is_terminal(), inactive));
        }
    }
    assert_eq!(actual, expected);
}

#[test]
fn rowless_runs_follow_members_then_newest_node_entry_then_open_prs_by_label() {
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
    let mut open_pr = older.clone();
    open_pr.workflow = None;
    open_pr.pr_state = Some(WorktreePrState::Open);
    let pr_only = ["b-pr", "a-pr"].map(|label| {
        let mut group = open_pr.clone();
        group.key = format!("channel:{label}");
        group.label = label.to_owned();
        group
    });
    let member = snapshot.worktree_groups[0].clone();
    snapshot.worktree_groups = [vec![older, newer, member], pr_only.into()].concat().into();
    snapshot.sort_groups_for_presentation();
    assert_eq!(
        snapshot
            .worktree_groups
            .iter()
            .map(|group| (group.label.as_str(), group.inactive))
            .collect::<Vec<_>>(),
        [
            ("members", None),
            ("z-new", None),
            ("a-old", None),
            ("a-pr", None),
            ("b-pr", None)
        ],
        "PR-only groups follow every rowless run, by label"
    );
}
