use super::*;
use crate::agents::PermissionMode;
use crate::ids::{AgentKind, AgentSessionId, WorkspaceId};
use crate::store::run::{RunRecord, RunStatus};

fn supervised(paths: &StatePaths, agent: &str, fire: Option<&FireId>, status: RunStatus) {
    let mut record = RunRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/repo")),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "task".into(),
        "/repo".into(),
    );
    record.agent_id = Some(AgentSessionId::from(agent));
    record.loop_fire = fire.cloned();
    record.status = status;
    crate::harness::run::create(paths, &record).unwrap();
}

fn agent(id: &str, fire: Option<&FireId>) -> AgentState {
    let mut row = crate::testkit::agent_state("codex", id, jiff::Timestamp::now());
    row.launch_id = Some(AgentSessionId::from(format!("launch-{id}").as_str()));
    row.loop_fire = fire.cloned();
    row
}

#[test]
fn alive_lists_the_tasks_stamped_launches_until_each_ends() {
    let home = tempfile::tempdir().unwrap();
    let paths = StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    let task = TaskId::new();
    let (first, successor) = (FireId::new(&task), FireId::new(&task));
    let other = FireId::new(&TaskId::new());
    supervised(&paths, "print-run", Some(&first), RunStatus::Running);
    supervised(&paths, "finished-run", Some(&first), RunStatus::Completed);
    supervised(&paths, "other-run", Some(&other), RunStatus::Running);
    supervised(&paths, "handed-off-run", None, RunStatus::Running);
    let mut ended = agent("ended", Some(&first));
    ended.ended_at = Some(jiff::Timestamp::now());
    let rows = [
        agent("print-run", Some(&first)),
        agent("room", Some(&first)),
        agent("lead", Some(&successor)),
        agent("member", Some(&successor)),
        ended,
        agent("other", Some(&other)),
        agent("handed-off", None),
    ];

    let alive = alive(&task, &paths, rows.iter()).unwrap();

    let listed = alive
        .iter()
        .map(|launch| match &launch.launch {
            Launch::Supervised(run) => (run.agent_id.clone().unwrap().to_string(), &launch.fire),
            Launch::Agent(row) => (row.agent_id.to_string(), &launch.fire),
            Launch::Workflow(run) => (run.id.to_string(), &launch.fire),
            Launch::Child { .. } => unreachable!("no fire mark carries a launch child here"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        listed,
        [
            ("print-run".to_owned(), &first),
            ("room".to_owned(), &first),
            ("lead".to_owned(), &successor),
            ("member".to_owned(), &successor),
        ]
    );
}
