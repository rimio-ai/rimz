use serde_json::json;

use super::*;
use crate::ids::WorkspaceId;

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_second(seconds).expect("test timestamp")
}

fn agent(id: &str, kind: &str, registered: i64) -> AgentState {
    serde_json::from_value(json!({
        "agent_id": id,
        "kind": kind,
        "status": "idle",
        "phase": "idle",
        "pane": null,
        "worktree_path": "/repo/lane",
        "worktree_branch": "feature",
        "task": null,
        "model": "model",
        "effort": "high",
        "last_seen": at(registered + 10),
        "last_activity": at(registered + 10),
        "registered_at": at(registered),
    }))
    .expect("test agent")
}

fn build_for(agents: &[AgentState]) -> Attribution {
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime paths");
    let refs = agents.iter().collect::<Vec<_>>();
    build(AttributionRequest {
        agents: &refs,
        peers: &refs,
        me: None,
        runtime: &runtime,
        active_grace_secs: 180,
        scope: AttributionScope::default(),
        now: at(100),
    })
}

#[test]
fn folds_compaction_continuations_and_sums_rollup_effort() {
    let mut first = agent("one", "codex", 10);
    first.team = Some("forge".to_owned());
    first.role = Some("coder".to_owned());
    first.launch_ordinal = Some(1);
    first.ended_at = Some(at(20));
    first.tool_calls.insert("exec".to_owned(), 2);
    first.compaction_count = 1;
    let mut second = agent("two", "codex", 30);
    second.team = Some("forge".to_owned());
    second.role = Some("coder".to_owned());
    second.launch_ordinal = Some(1);
    second.tool_calls.insert("exec".to_owned(), 3);
    second.compaction_count = 2;

    let report = build_for(&[first, second]);
    let member = &report.groups[0].members[0];
    assert_eq!(member.handle, "@coder");
    assert_eq!(member.sessions, 2);
    assert_eq!(member.tool_calls, 5);
    assert_eq!(member.compactions, 3);
    assert_eq!(member.presence, Presence::Live);
}

#[test]
fn team_roles_and_provider_kinds_keep_distinct_slots() {
    let mut planner = agent("planner", "claude", 10);
    planner.team = Some("forge".to_owned());
    planner.role = Some("planner".to_owned());
    planner.launch_ordinal = Some(0);
    let mut coder = agent("coder", "codex", 11);
    coder.team = Some("forge".to_owned());
    coder.role = Some("coder".to_owned());
    coder.launch_ordinal = Some(1);
    let mut other_kind = coder.clone();
    other_kind.agent_id = "claude-coder".into();
    other_kind.kind = AgentKind::new_unchecked("claude");

    let report = build_for(&[planner, coder, other_kind]);
    assert_eq!(report.groups[0].members.len(), 3);
}

#[test]
fn identical_team_roles_in_different_lanes_keep_distinct_slots() {
    let mut auth = agent("auth-coder", "codex", 10);
    auth.team = Some("forge".to_owned());
    auth.role = Some("coder".to_owned());
    auth.channel = Some("auth".to_owned());
    let mut docs = agent("docs-coder", "codex", 20);
    docs.team = Some("forge".to_owned());
    docs.role = Some("coder".to_owned());
    docs.channel = Some("docs".to_owned());

    let report = build_for(&[auth, docs]);
    assert_eq!(report.groups[0].members.len(), 2);
    assert_eq!(report.groups[0].totals.agents, 2);
    assert!(
        report.groups[0]
            .members
            .iter()
            .all(|member| member.sessions == 1)
    );
}

#[test]
fn roleless_cohorts_and_reused_panes_fold() {
    let mut first = agent("one", "codex", 10);
    first.launch_group = Some("group".to_owned());
    first.launch_ordinal = Some(0);
    let mut continuation = agent("two", "codex", 20);
    continuation.launch_group = Some("group".to_owned());
    continuation.launch_ordinal = Some(0);
    let mut pane_first = agent("pane-one", "claude", 30);
    pane_first.pane = Some(crate::pane::PaneRef::from_id(
        crate::ids::PaneId::from_parts(crate::ids::MuxName::Tmux, "%1"),
    ));
    let mut pane_second = agent("pane-two", "claude", 40);
    pane_second.pane = pane_first.pane.clone();

    let report = build_for(&[first, continuation, pane_first, pane_second]);
    let sessions = report.groups[0]
        .members
        .iter()
        .map(|member| member.sessions)
        .collect::<Vec<_>>();
    assert_eq!(sessions, [2, 2]);
}

#[test]
fn exited_presence_wall_clock_and_teamless_order_are_honest() {
    let mut team = agent("team", "codex", 10);
    team.team = Some("forge".to_owned());
    team.role = Some("coder".to_owned());
    team.ended_at = Some(at(20));
    let stray = agent("stray", "claude", 30);

    let report = build_for(&[team, stray]);
    assert_eq!(report.groups.len(), 2);
    assert_eq!(report.groups[0].team.as_ref().unwrap().name, "forge");
    assert!(report.groups[1].team.is_none());
    assert_eq!(report.groups[0].members[0].presence, Presence::Exited);
    assert_eq!(report.totals.wall_clock_secs, 30);
    assert_eq!(report.totals.cost_usd, None);
}
