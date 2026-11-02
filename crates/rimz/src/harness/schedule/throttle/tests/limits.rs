use super::*;
use crate::agents::{WorkflowAttemptRef, WorkflowAttemptRole};
use crate::harness::schedule::throttle::WorkflowStart;
use crate::ids::{RunId, WorkflowRunId};
use crate::store::workflow::{self, StepLifecycle};
use serde_json::json;

fn limited(workflow: Option<&str>, step: Option<&str>) -> Run {
    Run {
        machine_gated: false,
        limits: Some(WorkflowStart {
            workflow: "review".into(),
            step: "build".into(),
            run: WorkflowRunId::new(),
            arrival: Timestamp::from_millisecond(1_000_000).unwrap(),
            workflow_limit: workflow.map(|text| toml::from_str(text).unwrap()),
            step_limit: step.map(|text| toml::from_str(text).unwrap()),
            may_wait: true,
        }),
        ..run("nightly")
    }
}

fn held(attempt: Attempt) -> Hold {
    assert!(matches!(attempt, Attempt::Held(_)), "{attempt:?}");
    let Attempt::Held(hold) = attempt else {
        unreachable!()
    };
    hold
}

fn turn(attempt: Attempt) -> Turn {
    assert!(matches!(attempt, Attempt::Turn(_)), "{attempt:?}");
    let Attempt::Turn(turn) = attempt else {
        unreachable!()
    };
    turn
}

fn attempt(
    root: &str,
    node: &str,
    lifecycle: StepLifecycle,
) -> (workflow::RunRecord, AgentState, RunRecord) {
    let id = WorkflowRunId::new();
    let run_id = RunId::new();
    let reference = format!("leader-{id}");
    let pin = WorkflowAttemptRef {
        run: id.clone(),
        seq: 1,
        role: WorkflowAttemptRole::Step,
    };
    let mut leader = agent(&reference, AgentStatus::Running);
    leader.workflow = Some(pin.clone());
    let workflow = serde_json::from_value(json!({
        "version":1, "id":id, "workflow":"review", "task":"nightly", "spec_dir":"/defs",
        "spec_digest":"test", "scope":{"kind":"worktree", "path":root},
        "project_root":root, "created_at":Timestamp::UNIX_EPOCH,
        "entered_at":Timestamp::UNIX_EPOCH, "node":node, "state":"active",
        "visits":{}, "visit_grant":0, "deadline_fired":false, "next_seq":2,
        "attempt":{
            "kind":"agent", "seq":1, "node":node, "lifecycle":lifecycle,
            "started_at":Timestamp::UNIX_EPOCH, "host":{"pid":1,"start":null},
            "run":{"run_id":run_id,"handle":reference}
        }
    }))
    .unwrap();
    let supervised = serde_json::from_value(json!({
        "run_id":run_id, "workspace_id":WorkspaceId::from_project_root(Path::new(root)),
        "kind":"claude", "agent_id":reference, "workflow":pin, "status":"running",
        "permission_mode":"auto", "prompt":"build", "worktree_path":root,
        "started_at":Timestamp::UNIX_EPOCH, "updated_at":Timestamp::UNIX_EPOCH
    }))
    .unwrap();
    (workflow, leader, supervised)
}

fn install(fake: &FakeHost, record: workflow::RunRecord, agents: Vec<AgentState>, run: RunRecord) {
    fake.set_agents(
        &WorkspaceId::from_project_root(&record.project_root),
        agents,
    );
    fake.workflows.lock().unwrap().push(record);
    fake.runs.lock().unwrap().push(run);
}

#[test]
fn limits_count_working_leaders_across_rooms_and_held_steps_with_you() {
    let fake = FakeHost::new();
    let (first, leader, supervised) = attempt("/first", "build", StepLifecycle::Stay);
    let mut companion = leader.clone();
    companion.agent_id = "companion".into();
    companion.name = Some("companion".into());
    let mut subagent = leader.clone();
    subagent.parent_agent_id = Some(leader.agent_id.clone());
    let mut judge = leader.clone();
    judge.workflow.as_mut().unwrap().role = WorkflowAttemptRole::Judge;
    install(
        &fake,
        first,
        vec![leader, companion, subagent, judge],
        supervised,
    );
    let (mut second, leader, supervised) = attempt("/second", "fix", StepLifecycle::Until);
    second.held = Some(workflow::HeldSession {
        attempt: second.attempt.take().unwrap(),
        entered_at: Timestamp::UNIX_EPOCH,
        verdict: None,
    });
    second.attempt = Some(
        serde_json::from_value(json!({
            "kind":"judge", "seq":2, "node":"check", "started_at":Timestamp::UNIX_EPOCH,
            "host":{"pid":1,"start":null}
        }))
        .unwrap(),
    );
    install(&fake, second, vec![leader], supervised);
    for (root, state, name, status) in [
        (
            "/idle",
            workflow::RunState::Active,
            "review",
            AgentStatus::Idle,
        ),
        (
            "/terminal",
            workflow::RunState::Done,
            "review",
            AgentStatus::Running,
        ),
        (
            "/other",
            workflow::RunState::Active,
            "other",
            AgentStatus::Running,
        ),
    ] {
        let (mut record, mut leader, supervised) = attempt(root, "build", StepLifecycle::Stay);
        record.state = state;
        record.workflow = name.into();
        leader.status = status;
        install(&fake, record, vec![leader], supervised);
    }
    let workflow = held(
        try_admit_in(
            &fake.host(),
            &config("pace='0s'"),
            &limited(Some("max-active=2"), None),
        )
        .unwrap(),
    );
    assert_eq!(
        (workflow.kind, workflow.text.as_str()),
        (
            HoldKind::WorkflowLimit,
            "limit review 2/2 working (2 with you)"
        )
    );
    let step = held(
        try_admit_in(
            &fake.host(),
            &config("pace='0s'"),
            &limited(None, Some("max-active=1")),
        )
        .unwrap(),
    );
    assert_eq!(
        (step.kind, step.text.as_str()),
        (HoldKind::StepLimit, "limit build 1/1 working (1 with you)")
    );
    let mut fix = limited(None, Some("max-active=1"));
    fix.limits.as_mut().unwrap().step = "fix".into();
    assert_eq!(
        held(try_admit_in(&fake.host(), &config("pace='0s'"), &fix).unwrap()).text,
        "limit fix 1/1 working (1 with you)"
    );
}

#[test]
fn limits_count_an_unbound_launch_once_but_only_match_live_steps() {
    let fake = FakeHost::new();
    let (mut record, leader, supervised) = attempt("/repo", "build", StepLifecycle::Turn);
    record.attempt.as_mut().unwrap().run = None;
    let mut companion = leader.clone();
    companion.agent_id = "companion".into();
    companion.name = Some("companion".into());
    install(&fake, record, vec![leader, companion], supervised);
    let request = limited(None, Some("max-active=1"));
    assert_eq!(
        held(try_admit_in(&fake.host(), &config("pace='0s'"), &request).unwrap()).text,
        "limit build 1/1 working"
    );
    fake.workflows.lock().unwrap()[0]
        .attempt
        .as_mut()
        .unwrap()
        .seq = 2;
    drop(turn(
        try_admit_in(&fake.host(), &config("pace='0s'"), &request).unwrap(),
    ));
}

#[test]
fn limits_reserve_the_last_slot_under_the_shared_queue_lock() {
    let fake = FakeHost::new();
    let (record, leader, supervised) = attempt("/existing", "build", StepLifecycle::Turn);
    install(&fake, record, vec![leader], supervised);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let results = std::thread::scope(|scope| {
        let starts: Vec<_> = ["/one", "/two"]
            .into_iter()
            .map(|root| {
                let mut request = limited(Some("max-active=2"), None);
                request.workspace = WorkspaceId::from_project_root(Path::new(root));
                request.root = root.into();
                let barrier = Arc::clone(&barrier);
                let host = fake.host();
                scope.spawn(move || {
                    barrier.wait();
                    let answer = try_admit_in(&host, &config("pace='0s'"), &request).unwrap();
                    (request, answer)
                })
            })
            .collect();
        starts
            .into_iter()
            .map(|start| start.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        results
            .iter()
            .filter(|(_, attempt)| matches!(attempt, Attempt::Turn(_)))
            .count(),
        1,
        "{results:?}"
    );
    assert_eq!(
        results
            .iter()
            .filter(|(_, attempt)| matches!(attempt, Attempt::Held(_)))
            .count(),
        1,
        "{results:?}"
    );
    let (request, answer) = results
        .iter()
        .find(|(_, answer)| matches!(answer, Attempt::Turn(_)))
        .unwrap();
    let Attempt::Turn(turn) = answer else {
        unreachable!()
    };
    let (mut record, mut leader, mut supervised) =
        attempt(request.root.to_str().unwrap(), "build", StepLifecycle::Turn);
    record.id = request.limits.as_ref().unwrap().run.clone();
    leader.workflow.as_mut().unwrap().run = record.id.clone();
    supervised.workflow = leader.workflow.clone();
    let agent_id = leader.agent_id.clone();
    install(&fake, record, vec![leader], supervised);
    turn.report_launch(&request.workspace, &agent_id);
    let hold = held(
        try_admit_in(
            &fake.host(),
            &config("pace='0s'"),
            &limited(Some("max-active=2"), None),
        )
        .unwrap(),
    );
    assert_eq!(
        (hold.kind, hold.text.as_str()),
        (HoldKind::WorkflowLimit, "limit review 2/2 working")
    );
}

#[test]
fn limits_scope_inflight_turns_and_keep_the_machine_release_rule() {
    let fake = FakeHost::new();
    let request = limited(Some("max-active=1"), None);
    let configured = config("pace='0s'");
    let first = turn(try_admit_in(&fake.host(), &configured, &request).unwrap());
    let hold = held(try_admit_in(&fake.host(), &configured, &request).unwrap());
    assert_eq!(
        (hold.kind, hold.text.as_str()),
        (HoldKind::WorkflowLimit, "limit review: 1 start launching")
    );
    assert!(matches!(
        try_admit_in(&fake.host(), &configured, &run("ordinary")).unwrap(),
        Attempt::Open
    ));
    let other_machine =
        turn(try_admit_in(&fake.host(), &config("pace='10s'"), &run("ordinary")).unwrap());
    let mut other = limited(None, Some("max-active=1"));
    other.limits.as_mut().unwrap().workflow = "other".into();
    drop(turn(
        try_admit_in(&fake.host(), &configured, &other).unwrap(),
    ));
    drop(first);
    drop(other_machine);

    let mut gated = request.clone();
    gated.machine_gated = true;
    let first = turn(try_admit_in(&fake.host(), &configured, &gated).unwrap());
    first.report_launch(&workspace(), &"provisional".into());
    let mut idle = agent("adopted", AgentStatus::Idle);
    idle.launch_id = Some("provisional".into());
    fake.set_agents(&workspace(), vec![idle]);
    drop(turn(
        try_admit_in(&fake.host(), &config("pace='10s'"), &run("ordinary")).unwrap(),
    ));
    let hold = held(try_admit_in(&fake.host(), &configured, &request).unwrap());
    assert_eq!(
        (hold.kind, hold.text.as_str()),
        (HoldKind::WorkflowLimit, "limit review: 1 start launching")
    );
    fake.clock.fetch_add(120_000, Ordering::SeqCst);
    drop(turn(
        try_admit_in(&fake.host(), &configured, &request).unwrap(),
    ));
}

#[test]
fn limits_keep_fifo_standing_while_a_machine_turn_is_in_flight() {
    let fake = FakeHost::new();
    let configured = config("pace='0s'");
    let (running, leader, supervised) = attempt("/running", "build", StepLifecycle::Turn);
    let running_room = WorkspaceId::from_project_root(&running.project_root);
    install(&fake, running, vec![leader], supervised);
    let machine =
        turn(try_admit_in(&fake.host(), &config("pace='10s'"), &run("ordinary")).unwrap());

    let mut earlier = limited(Some("max-active=1"), None);
    earlier.machine_gated = true;
    let hold = held(try_admit_in(&fake.host(), &configured, &earlier).unwrap());
    assert_eq!(
        (hold.kind, hold.text.as_str()),
        (HoldKind::WorkflowLimit, "limit review 1/1 working")
    );
    let (mut waiter, _, _) = attempt("/earlier", "build", StepLifecycle::Turn);
    waiter.attempt = None;
    let first = Timestamp::from_millisecond(999_999).unwrap();
    waiter.wait = Some(
        serde_json::from_value(json!({
            "node":"build", "outcome":"capacity", "hold":hold.kind, "first":first, "last":first,
            "count":1
        }))
        .unwrap(),
    );
    fake.set_agents(
        &WorkspaceId::from_project_root(&waiter.project_root),
        vec![],
    );
    fake.workflows.lock().unwrap().push(waiter);

    drop(machine);
    fake.set_agents(&running_room, vec![]);
    let later = limited(Some("max-active=1"), None);
    let hold = held(try_admit_in(&fake.host(), &configured, &later).unwrap());
    assert_eq!(
        (hold.kind, hold.text.as_str()),
        (
            HoldKind::WorkflowLimit,
            "limit review: 1 earlier start waiting"
        )
    );
}

#[test]
fn limits_without_machine_gates_do_not_serialize_unrelated_workflows() {
    let fake = FakeHost::new();
    let configured = config("pace='0s'");
    let mut first = limited(Some("max-active=1"), None);
    first.machine_gated = true;
    let first_turn = turn(try_admit_in(&fake.host(), &configured, &first).unwrap());
    let ticket = read_ticket(&first_turn.0.ticket).unwrap().unwrap();
    let mut other = first.clone();
    other.limits.as_mut().unwrap().workflow = "other".into();
    let inspected = look_in(fake.as_ref(), &configured, &other).unwrap();
    let admitted = try_admit_in(&fake.host(), &configured, &other).unwrap();
    assert_eq!(
        (
            ticket.machine_gated,
            inspected.is_none(),
            matches!(admitted, Attempt::Turn(_)),
        ),
        (false, true, true),
        "inspection: {inspected:?}; admission: {admitted:?}"
    );
}

#[test]
fn limits_pace_is_stamped_only_at_report_and_releases_at_the_exact_gap() {
    let fake = FakeHost::new();
    let request = limited(Some("pace='10m'"), Some("pace='5m'"));
    let config = config("pace='0s'");
    drop(turn(try_admit_in(&fake.host(), &config, &request).unwrap()));
    assert!(!fake.workflow_pace().exists());
    let first = turn(try_admit_in(&fake.host(), &config, &request).unwrap());
    fake.clock.fetch_add(60_000, Ordering::SeqCst);
    first.report_launch(&workspace(), &"leader".into());
    assert!(
        fake.workflow_pace().is_file(),
        "a reported limited launch must persist its pace"
    );
    fake.set_agents(&workspace(), vec![agent("leader", AgentStatus::Running)]);
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fake.workflow_pace()).unwrap()).unwrap();
    assert_eq!(value["review"]["last_launch_ms"], 1_060_000);
    assert_eq!(value["review"]["steps"]["build"], 1_060_000);
    fake.clock.fetch_add(60_000, Ordering::SeqCst);
    first.report_launch(&workspace(), &"retry".into());
    let hold = held(try_admit_in(&fake.host(), &config, &request).unwrap());
    assert_eq!(
        (hold.kind, hold.text.as_str()),
        (HoldKind::WorkflowLimit, "limit review next start in 9m")
    );
    let mut step_only = request.clone();
    step_only.limits.as_mut().unwrap().workflow_limit = None;
    assert_eq!(
        held(try_admit_in(&fake.host(), &config, &step_only).unwrap()).text,
        "limit build next start in 4m"
    );
    fake.clock.fetch_add(240_000, Ordering::SeqCst);
    drop(turn(
        try_admit_in(&fake.host(), &config, &step_only).unwrap(),
    ));
    fake.clock.fetch_add(300_000, Ordering::SeqCst);
    drop(turn(try_admit_in(&fake.host(), &config, &request).unwrap()));
}

#[test]
fn limits_fifo_uses_capacity_arrival_scope_and_run_id_ties() {
    let mut actual = Vec::new();
    for (kind, step, state, offset, expected) in [
        (
            HoldKind::WorkflowLimit,
            "other",
            workflow::RunState::Active,
            -1,
            true,
        ),
        (
            HoldKind::StepLimit,
            "build",
            workflow::RunState::Active,
            -1,
            true,
        ),
        (
            HoldKind::StepLimit,
            "other",
            workflow::RunState::Active,
            -1,
            false,
        ),
        (
            HoldKind::WorkflowLimit,
            "build",
            workflow::RunState::Parked,
            -1,
            false,
        ),
        (
            HoldKind::WorkflowLimit,
            "build",
            workflow::RunState::Active,
            1,
            false,
        ),
        (
            HoldKind::WorkflowLimit,
            "build",
            workflow::RunState::Active,
            0,
            true,
        ),
    ] {
        let fake = FakeHost::new();
        let (mut waiter, _, _) = attempt("/waiter", step, StepLifecycle::Turn);
        waiter.attempt = None;
        waiter.state = state;
        let mut request = limited(Some("max-active=1"), Some("max-active=1"));
        request.limits.as_mut().unwrap().run = WorkflowRunId::new();
        let first = Timestamp::from_millisecond(1_000_000 + offset).unwrap();
        waiter.wait = Some(serde_json::from_value(json!({
            "node":step, "outcome":"capacity", "hold":kind, "first":first, "last":first, "count":1
        })).unwrap());
        fake.set_agents(
            &WorkspaceId::from_project_root(&waiter.project_root),
            vec![],
        );
        fake.workflows.lock().unwrap().push(waiter);
        let answer = try_admit_in(&fake.host(), &config("pace='0s'"), &request).unwrap();
        actual.push((matches!(answer, Attempt::Held(_)), expected));
        if let Attempt::Held(hold) = answer {
            let name = if kind == HoldKind::WorkflowLimit {
                "review"
            } else {
                "build"
            };
            assert_eq!(hold.kind, kind);
            assert_eq!(hold.text, format!("limit {name}: 1 earlier start waiting"));
        }
    }
    assert!(
        actual.iter().all(|(held, expected)| held == expected),
        "{actual:?}"
    );
}

#[test]
fn limits_manual_claims_never_wait_and_exemption_is_workflow_max_active_only() {
    let fake = FakeHost::new();
    let (record, leader, supervised) = attempt("/repo", "build", StepLifecycle::Turn);
    install(&fake, record, vec![leader], supervised);
    let mut manual = limited(Some("max-active=1\npace='10m'"), None);
    manual.limits.as_mut().unwrap().may_wait = false;
    let first = turn(
        try_admit_in(
            &fake.host(),
            &config("max-active=1\ncheck='capacity'"),
            &manual,
        )
        .unwrap(),
    );
    first.report_launch(&workspace(), &"manual".into());
    assert!(fake.workflow_pace().is_file());
    assert!(fake.checked_groups().is_empty());
    fake.clock.fetch_add(120_000, Ordering::SeqCst);
    std::fs::remove_file(fake.workflow_pace()).unwrap();
    let mut automated = limited(Some("max-active=2"), None);
    automated.machine_gated = true;
    drop(turn(
        try_admit_in(&fake.host(), &config("max-active-per-task=1"), &automated).unwrap(),
    ));
    automated.limits.as_mut().unwrap().workflow_limit = None;
    automated.limits.as_mut().unwrap().step_limit = Some(toml::from_str("max-active=2").unwrap());
    assert_eq!(
        held(try_admit_in(&fake.host(), &config("max-active-per-task=1"), &automated).unwrap())
            .kind,
        HoldKind::TaskCap
    );
    automated.limits.as_mut().unwrap().workflow_limit =
        Some(toml::from_str("max-active=2").unwrap());
    assert_eq!(
        held(try_admit_in(&fake.host(), &config("max-active=1"), &automated).unwrap()).kind,
        HoldKind::MachineCap
    );
}

#[test]
fn limits_fail_closed_on_unreadable_rooms_and_corrupt_pace_records_they_read() {
    let fake = FakeHost::new();
    std::fs::write(fake.workflow_pace(), "broken").unwrap();
    let request = limited(None, Some("pace='1m'"));
    assert_eq!(
        held(try_admit_in(&fake.host(), &config("pace='0s'"), &request).unwrap()).kind,
        HoldKind::Store
    );
    drop(turn(
        try_admit_in(
            &fake.host(),
            &config("pace='0s'"),
            &limited(None, Some("max-active=1")),
        )
        .unwrap(),
    ));
    std::fs::remove_file(fake.workflow_pace()).unwrap();
    let (record, _, _) = attempt("/repo", "build", StepLifecycle::Turn);
    fake.workflows.lock().unwrap().push(record);
    fake.rooms
        .lock()
        .unwrap()
        .insert(workspace().to_string(), Err("broken room".into()));
    assert_eq!(
        held(try_admit_in(&fake.host(), &config("pace='0s'"), &request).unwrap()).kind,
        HoldKind::Store
    );
}
