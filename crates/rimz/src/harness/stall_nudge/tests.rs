use super::*;
use crate::agents::AgentStatus;
use crate::store::snapshot::SidebarPipeline;
use std::sync::Arc;

fn ts(seconds: i64) -> Timestamp {
    Timestamp::from_second(seconds).unwrap()
}

fn staged() -> SidebarSnapshot {
    let agents = ["coder", "reviewer", "tester"].map(|role| {
        let mut agent = crate::testkit::agent_state("claude", role, ts(6_000));
        agent.team = Some("crew".into());
        agent.role = Some(role.into());
        agent.worktree_path = Some("/repo/main".into());
        agent.worktree_branch = Some("main".into());
        agent.pane = Some(
            serde_json::from_value(serde_json::json!({
                "pane_id": format!("tmux:%{role}"), "session_name": "test",
                "command": "claude", "cwd": "/repo/main",
            }))
            .unwrap(),
        );
        agent
    });
    let panes = agents
        .iter()
        .map(|agent| agent.pane.clone().unwrap())
        .collect();
    let mut snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(std::path::Path::new("/repo/main")),
        agents.into(),
        ts(10_000),
    )
    .with_live_panes(panes, None);
    let group = &mut Arc::make_mut(&mut snapshot.worktree_groups)[0];
    group.pipeline = Some(SidebarPipeline {
        stages: vec!["Build".into()],
        stage: "Build".into(),
        owner: Some("coder".into()),
        started_at: Some(ts(5_000)),
        stage_started_at: Some(ts(5_000)),
        stage_prior_secs: 0,
        visited: Default::default(),
        done_at: None,
    });
    // The heavy pass receives the stuck projection, including the owner's failed card.
    group.stalled_since = Some(ts(6_700));
    Arc::make_mut(&mut group.rows)
        .iter_mut()
        .find(|row| row.id == "coder")
        .unwrap()
        .as_agent_mut()
        .unwrap()
        .status = AgentStatus::Failed;
    snapshot
}

fn enabled() -> MachineConfig {
    let mut config = MachineConfig::default();
    config.harness.stall_nudge = true;
    config
}

fn team_table() -> TeamsConfig {
    toml::from_str(concat!(
        "[crew]\nleader = 'reviewer'\nstages = ['Build']\n",
        "[[crew.roles]]\nrole = 'coder'\nprofile = 'claude'\nowns = ['Build']\n",
        "[[crew.roles]]\nrole = 'reviewer'\nprofile = 'claude'\n",
    ))
    .unwrap()
}

fn evaluate(
    snapshot: &SidebarSnapshot,
    config: &MachineConfig,
    teams: &TeamsConfig,
) -> Vec<StallNudgeRequest> {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(snapshot.workspace_id.clone(), dir.path()).unwrap();
    let mut requests = Vec::new();
    nudge_stalled_teams_with(snapshot, &runtime, config, teams, |request| {
        requests.push(request.clone());
        true
    });
    requests
}

#[test]
fn decision_is_opt_in_and_requires_the_stuck_warning_first() {
    let actual =
        [(false, 3_300), (true, 3_299), (true, 3_300), (true, 3_301)].map(|(enabled, window)| {
            let mut config = MachineConfig::default();
            config.harness.stall_nudge = enabled;
            config.agents.attention =
                toml::from_str(&format!("stalled_after_secs = {window}")).unwrap();
            decision(&config)
        });
    assert_eq!(
        actual,
        [
            Decision::Off,
            Decision::On,
            Decision::Shadowed,
            Decision::Shadowed
        ]
    );
}

#[test]
fn linked_worktree_uses_the_host_resolved_team_leader() {
    let mut snapshot = staged();
    snapshot.project_root = Some("/repo".into());
    snapshot.workspace_id =
        WorkspaceId::from_project_root(snapshot.project_root.as_deref().unwrap());
    let mut config = enabled();
    config.agents.teams = TeamsConfig::default();
    let requests = evaluate(&snapshot, &config, &team_table());
    assert_eq!(
        requests
            .first()
            .map(|request| (request.checkout.as_path(), request.leader_role.as_deref())),
        Some((std::path::Path::new("/repo/main"), Some("reviewer")))
    );
}

#[test]
fn no_spawn_without_a_host_resolved_team() {
    assert!(evaluate(&staged(), &enabled(), &TeamsConfig::default()).is_empty());
}

#[test]
fn spawns_at_3300_seconds_but_not_3299() {
    let actual = [9_999, 10_000].map(|now| {
        let mut snapshot = staged();
        snapshot.now = ts(now);
        evaluate(&snapshot, &enabled(), &team_table()).len()
    });
    assert_eq!(actual, [0, 1]);
}

#[test]
fn no_spawn_without_the_stuck_projection() {
    let mut snapshot = staged();
    let before = evaluate(&snapshot, &enabled(), &team_table()).len();
    Arc::make_mut(&mut snapshot.worktree_groups)[0].stalled_since = None;
    assert_eq!(
        [before, evaluate(&snapshot, &enabled(), &team_table()).len()],
        [1, 0]
    );
}

#[test]
fn no_spawn_without_a_stage_visit_identity() {
    let mut snapshot = staged();
    let before = evaluate(&snapshot, &enabled(), &team_table()).len();
    Arc::make_mut(&mut snapshot.worktree_groups)[0]
        .pipeline
        .as_mut()
        .unwrap()
        .stage_started_at = None;
    assert_eq!(
        [before, evaluate(&snapshot, &enabled(), &team_table()).len()],
        [1, 0]
    );
}

#[test]
fn no_spawn_when_the_flag_is_off() {
    let snapshot = staged();
    assert_eq!(
        [
            evaluate(&snapshot, &enabled(), &team_table()).len(),
            evaluate(&snapshot, &MachineConfig::default(), &team_table()).len()
        ],
        [1, 0]
    );
}

#[test]
fn no_spawn_when_the_flag_is_shadowed() {
    let snapshot = staged();
    let mut config = enabled();
    let before = evaluate(&snapshot, &config, &team_table()).len();
    config.agents.attention = toml::from_str("stalled_after_secs = 3300").unwrap();
    assert_eq!(
        [before, evaluate(&snapshot, &config, &team_table()).len()],
        [1, 0]
    );
}

#[test]
fn latest_active_member_wins_over_the_owner() {
    let mut snapshot = staged();
    let group = &mut Arc::make_mut(&mut snapshot.worktree_groups)[0];
    let reviewer = Arc::make_mut(&mut group.rows)
        .iter_mut()
        .find(|row| row.id == "reviewer")
        .unwrap();
    reviewer.last_activity = ts(6_700);
    let requests = evaluate(&snapshot, &enabled(), &team_table());
    assert_eq!(
        requests
            .iter()
            .map(|request| request.target.1.as_str())
            .collect::<Vec<_>>(),
        ["reviewer"]
    );
}

#[test]
fn ties_prefer_the_owner_then_the_lowest_row_id() {
    let mut snapshot = staged();
    let owner = evaluate(&snapshot, &enabled(), &team_table());
    let group = &mut Arc::make_mut(&mut snapshot.worktree_groups)[0];
    for row in Arc::make_mut(&mut group.rows) {
        if row.id != "coder" {
            row.last_activity = ts(6_700);
        }
    }
    let others = evaluate(&snapshot, &enabled(), &team_table());
    assert_eq!(
        [
            owner.first().map(|request| request.target.1.as_str()),
            others.first().map(|request| request.target.1.as_str())
        ],
        [Some("coder"), Some("reviewer")]
    );
}

#[test]
fn pacing_suppresses_the_same_clock_but_not_a_new_one() {
    let mut snapshot = staged();
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(snapshot.workspace_id.clone(), dir.path()).unwrap();
    let mut counts = Vec::new();
    for clock in [6_700, 6_700, 6_699] {
        Arc::make_mut(&mut snapshot.worktree_groups)[0].stalled_since = Some(ts(clock));
        let mut count = 0;
        nudge_stalled_teams_with(&snapshot, &runtime, &enabled(), &team_table(), |_| {
            count += 1;
            true
        });
        counts.push(count);
    }
    assert_eq!(counts, [1, 0, 1]);
}

#[test]
fn failed_spawn_writes_no_pacing_record_and_is_retried() {
    let snapshot = staged();
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(snapshot.workspace_id.clone(), dir.path()).unwrap();
    let mut attempts = 0;
    nudge_stalled_teams_with(&snapshot, &runtime, &enabled(), &team_table(), |_| {
        attempts += 1;
        false
    });
    let recorded_after_failure = runtime.live_path("stall-nudge").exists();
    nudge_stalled_teams_with(&snapshot, &runtime, &enabled(), &team_table(), |_| {
        attempts += 1;
        true
    });
    assert_eq!(
        (
            attempts,
            recorded_after_failure,
            runtime.live_path("stall-nudge").exists()
        ),
        (2, false, true)
    );
}
