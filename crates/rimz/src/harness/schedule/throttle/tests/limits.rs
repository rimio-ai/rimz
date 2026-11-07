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

fn attempt(root: &str, node: &str, lifecycle: StepLifecycle) -> (workflow::RunRecord, AgentState) {
    let id = WorkflowRunId::new();
    let run_id = RunId::new();
    let reference = format!("leader-{id}");
    let pin = WorkflowAttemptRef {
        run: id.clone(),
        seq: 1,
        role: WorkflowAttemptRole::Step,
    };
    let mut leader = agent(&reference, AgentStatus::Running);
    leader.name = Some("leader".into());
    leader.workflow = Some(pin.clone());
    leader.automation = Some(crate::agents::Automation::Workflow {
        workflow: "review".into(),
        step: node.into(),
    });
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
    (workflow, leader)
}

fn install(fake: &FakeHost, record: workflow::RunRecord, agents: Vec<AgentState>) {
    fake.set_agents(
        &WorkspaceId::from_project_root(&record.project_root),
        agents,
    );
    fake.workflows.lock().unwrap().push(record);
}

#[test]
fn limits_count_stamped_open_rows_across_rooms_even_after_the_run_ends() {
    let fake = FakeHost::new();
    let (first, leader) = attempt("/first", "build", StepLifecycle::Stay);
    let mut companion = leader.clone();
    companion.agent_id = "companion".into();
    companion.name = Some("companion".into());
    let mut subagent = leader.clone();
    subagent.parent_agent_id = Some(leader.agent_id.clone());
    let mut child = leader.clone();
    child.agent_id = "child".into();
    child.name = Some("child".into());
    child.parent_agent_id = Some(leader.agent_id.clone());
    child.launch_depth = Some(1);
    child.workflow = None;
    install(&fake, first, vec![leader, companion, subagent, child]);
    let (mut second, leader) = attempt("/second", "fix", StepLifecycle::Until);
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
    install(&fake, second, vec![leader]);
    for (root, state, name, status) in [
        (
            "/idle",
            workflow::RunState::Active,
            "review",
            AgentStatus::Idle,
        ),
        (
            "/errored",
            workflow::RunState::Active,
            "review",
            AgentStatus::Failed,
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
        let (mut record, mut leader) = attempt(root, "build", StepLifecycle::Stay);
        record.state = state;
        record.workflow = name.into();
        leader.automation = Some(crate::agents::Automation::Workflow {
            workflow: name.into(),
            step: "build".into(),
        });
        leader.status = status;
        install(&fake, record, vec![leader]);
    }
    let workflow = held(
        try_admit_in(
            &fake.host(),
            &config(""),
            &limited(Some("max-parallel=7"), None),
        )
        .unwrap(),
    );
    assert_eq!(
        (workflow.kind, workflow.text.as_str()),
        (
            HoldKind::WorkflowLimit,
            "workflow review: 7 of 7 agents open (@claude, @claude, +5)"
        )
    );
    let step = held(
        try_admit_in(
            &fake.host(),
            &config(""),
            &limited(None, Some("max-parallel=6")),
        )
        .unwrap(),
    );
    assert_eq!(
        (step.kind, step.text.as_str()),
        (
            HoldKind::StepLimit,
            "step build: 6 of 6 agents open (@claude, @leader, +4)"
        )
    );
    let mut fix = limited(None, Some("max-parallel=1"));
    fix.limits.as_mut().unwrap().step = "fix".into();
    assert_eq!(
        held(try_admit_in(&fake.host(), &config(""), &fix).unwrap()).text,
        "step fix: 1 of 1 agents open (@claude)"
    );
}

#[test]
fn a_workflow_limit_cannot_loosen_the_machine_automation_cap() {
    let fake = FakeHost::new();
    let (record, leader) = attempt("/repo", "build", StepLifecycle::Stay);
    install(&fake, record, vec![leader]);
    let request = limited(Some("max-parallel=10"), None);
    let hold =
        held(try_admit_in(&fake.host(), &config("max-parallel-automation=1"), &request).unwrap());
    assert_eq!(hold.text, "1 of 1 automated agents open (@claude)");
}

#[test]
fn limits_count_each_stamped_row_without_attempt_deduplication() {
    let fake = FakeHost::new();
    let (mut record, leader) = attempt("/repo", "build", StepLifecycle::Turn);
    record.attempt.as_mut().unwrap().run = None;
    let mut companion = leader.clone();
    companion.agent_id = "companion".into();
    companion.name = Some("companion".into());
    install(&fake, record, vec![leader, companion]);
    let request = limited(None, Some("max-parallel=2"));
    for seq in [1, 2] {
        fake.workflows.lock().unwrap()[0]
            .attempt
            .as_mut()
            .unwrap()
            .seq = seq;
        assert_eq!(
            held(try_admit_in(&fake.host(), &config(""), &request).unwrap()).text,
            "step build: 2 of 2 agents open (@leader, @companion)"
        );
    }
}

#[test]
fn limit_holds_name_the_incoming_group_at_both_scopes() {
    for workflow_limit in [true, false] {
        let fake = FakeHost::new();
        let (record, leader) = attempt("/repo", "build", StepLifecycle::Stay);
        let mut companion = leader.clone();
        companion.agent_id = "companion".into();
        companion.name = Some("companion".into());
        install(&fake, record, vec![leader, companion]);
        let mut request = if workflow_limit {
            limited(Some("max-parallel=3"), None)
        } else {
            limited(None, Some("max-parallel=3"))
        };
        request.groups[0].agents = 2;
        let scope = if workflow_limit {
            "workflow review"
        } else {
            "step build"
        };
        assert_eq!(
            held(try_admit_in(&fake.host(), &config(""), &request).unwrap()).text,
            format!("{scope}: 2 of 3 agents open (@leader, @companion); this start opens 2")
        );
    }
}

#[test]
fn oversized_groups_refuse_workflow_and_step_limits_even_behind_a_launch() {
    for workflow_limit in [true, false] {
        let fake = FakeHost::new();
        let mut request = if workflow_limit {
            limited(Some("max-parallel=1"), None)
        } else {
            limited(None, Some("max-parallel=1"))
        };
        let _launching = turn(try_admit_in(&fake.host(), &config(""), &request).unwrap());
        request.groups[0].agents = 2;
        let result = try_admit_in(&fake.host(), &config(""), &request);
        assert!(result.is_err(), "{result:?}");
        let scope = if workflow_limit {
            "workflow review"
        } else {
            "step build"
        };
        assert_eq!(
            result.unwrap_err().to_string(),
            format!(
                "this start opens 2 agents, more than max-parallel = 1 allows; raise max-parallel in {scope}'s definition"
            )
        );
    }
}

#[test]
fn limits_reserve_the_last_slot_under_the_shared_queue_lock() {
    let fake = FakeHost::new();
    let (record, leader) = attempt("/existing", "build", StepLifecycle::Turn);
    install(&fake, record, vec![leader]);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let results = std::thread::scope(|scope| {
        let starts: Vec<_> = ["/one", "/two"]
            .into_iter()
            .map(|root| {
                let mut request = limited(Some("max-parallel=2"), None);
                request.workspace = WorkspaceId::from_project_root(Path::new(root));
                request.root = root.into();
                let barrier = Arc::clone(&barrier);
                let host = fake.host();
                scope.spawn(move || {
                    barrier.wait();
                    let answer = try_admit_in(&host, &config(""), &request).unwrap();
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
    let (mut record, mut leader) =
        attempt(request.root.to_str().unwrap(), "build", StepLifecycle::Turn);
    record.id = request.limits.as_ref().unwrap().run.clone();
    leader.workflow.as_mut().unwrap().run = record.id.clone();
    let agent_id = leader.agent_id.clone();
    install(&fake, record, vec![leader]);
    turn.report_launch(&request.workspace, &agent_id);
    let hold = held(
        try_admit_in(
            &fake.host(),
            &config(""),
            &limited(Some("max-parallel=2"), None),
        )
        .unwrap(),
    );
    assert_eq!(
        (hold.kind, hold.text.as_str()),
        (
            HoldKind::WorkflowLimit,
            "workflow review: 2 of 2 agents open (@claude, @claude)"
        )
    );
}

#[test]
fn limits_scope_inflight_turns_and_keep_the_machine_release_rule() {
    let fake = FakeHost::new();
    let request = limited(Some("max-parallel=1"), None);
    let configured = config("");
    let first = turn(try_admit_in(&fake.host(), &configured, &request).unwrap());
    let hold = held(try_admit_in(&fake.host(), &configured, &request).unwrap());
    assert_eq!(
        (hold.kind, hold.text.as_str()),
        (
            HoldKind::WorkflowLimit,
            "workflow review: 1 start launching"
        )
    );
    let other_machine = turn(try_admit_in(&fake.host(), &config(""), &run("ordinary")).unwrap());
    let mut other = limited(None, Some("max-parallel=1"));
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
    idle.automation = Some(crate::agents::Automation::Workflow {
        workflow: "review".into(),
        step: "build".into(),
    });
    fake.set_agents(&workspace(), vec![idle]);
    drop(turn(
        try_admit_in(&fake.host(), &config(""), &run("ordinary")).unwrap(),
    ));
    let hold = held(try_admit_in(&fake.host(), &configured, &request).unwrap());
    assert_eq!(
        (hold.kind, hold.text.as_str()),
        (
            HoldKind::WorkflowLimit,
            "workflow review: 1 of 1 agents open (@claude)"
        )
    );
    fake.set_agents(&workspace(), Vec::new());
    drop(turn(
        try_admit_in(&fake.host(), &configured, &request).unwrap(),
    ));
}

#[test]
fn limits_keep_fifo_standing_while_a_machine_turn_is_in_flight() {
    let fake = FakeHost::new();
    let configured = config("");
    let (running, leader) = attempt("/running", "build", StepLifecycle::Turn);
    let running_room = WorkspaceId::from_project_root(&running.project_root);
    install(&fake, running, vec![leader]);
    let machine = turn(try_admit_in(&fake.host(), &config(""), &run("ordinary")).unwrap());

    let mut earlier = limited(Some("max-parallel=1"), None);
    earlier.machine_gated = true;
    let hold = held(try_admit_in(&fake.host(), &configured, &earlier).unwrap());
    assert_eq!(
        (hold.kind, hold.text.as_str()),
        (
            HoldKind::WorkflowLimit,
            "workflow review: 1 of 1 agents open (@claude)"
        )
    );
    let (mut waiter, _) = attempt("/earlier", "build", StepLifecycle::Turn);
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
    let later = limited(Some("max-parallel=1"), None);
    let hold = held(try_admit_in(&fake.host(), &configured, &later).unwrap());
    assert_eq!(
        (hold.kind, hold.text.as_str()),
        (
            HoldKind::WorkflowLimit,
            "workflow review: 1 earlier start waiting"
        )
    );
}

#[test]
fn limits_without_machine_gates_do_not_serialize_unrelated_workflows() {
    let fake = FakeHost::new();
    let configured = config("");
    let mut first = limited(Some("max-parallel=1"), None);
    first.machine_gated = false;
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
fn limits_stagger_is_stamped_only_at_report_and_releases_at_the_exact_gap() {
    let fake = FakeHost::new();
    let request = limited(Some("stagger='10m'"), Some("stagger='5m'"));
    let config = config("");
    drop(turn(try_admit_in(&fake.host(), &config, &request).unwrap()));
    assert!(!fake.start_stamps().exists());
    let first = turn(try_admit_in(&fake.host(), &config, &request).unwrap());
    fake.clock.fetch_add(60_000, Ordering::SeqCst);
    first.report_launch(&workspace(), &"leader".into());
    assert!(
        fake.start_stamps().is_file(),
        "a reported limited launch must persist its stagger"
    );
    fake.set_agents(&workspace(), vec![agent("leader", AgentStatus::Running)]);
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fake.start_stamps()).unwrap()).unwrap();
    assert_eq!(value["workflows"]["review"]["last_launch_ms"], 1_060_000);
    assert_eq!(value["workflows"]["review"]["steps"]["build"], 1_060_000);
    fake.clock.fetch_add(60_000, Ordering::SeqCst);
    first.report_launch(&workspace(), &"retry".into());
    let hold = held(try_admit_in(&fake.host(), &config, &request).unwrap());
    assert_eq!(
        (hold.kind, hold.text.as_str()),
        (HoldKind::WorkflowLimit, "workflow review: next start in 9m")
    );
    let mut step_only = request.clone();
    step_only.limits.as_mut().unwrap().workflow_limit = None;
    assert_eq!(
        held(try_admit_in(&fake.host(), &config, &step_only).unwrap()).text,
        "step build: next start in 4m"
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
        let (mut waiter, _) = attempt("/waiter", step, StepLifecycle::Turn);
        waiter.attempt = None;
        waiter.state = state;
        let mut request = limited(Some("max-parallel=1"), Some("max-parallel=1"));
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
        let answer = try_admit_in(&fake.host(), &config(""), &request).unwrap();
        actual.push((matches!(answer, Attempt::Held(_)), expected));
        if let Attempt::Held(hold) = answer {
            let name = if kind == HoldKind::WorkflowLimit {
                "review"
            } else {
                "build"
            };
            assert_eq!(hold.kind, kind);
            assert_eq!(
                hold.text,
                format!(
                    "{} {name}: 1 earlier start waiting",
                    if kind == HoldKind::WorkflowLimit {
                        "workflow"
                    } else {
                        "step"
                    }
                )
            );
        }
    }
    assert!(
        actual.iter().all(|(held, expected)| held == expected),
        "{actual:?}"
    );
}

#[test]
fn manual_claims_carry_only_the_limit_they_skipped() {
    let mut actual = Vec::new();
    for scenario in ["full", "clear", "unreadable", "stagger", "unlimited"] {
        let fake = FakeHost::new();
        let mut manual = limited(None, Some("max-parallel=1\nstagger='10m'"));
        manual.limits.as_mut().unwrap().may_wait = false;
        if scenario == "full" {
            let (record, leader) = attempt("/repo", "build", StepLifecycle::Turn);
            install(&fake, record, vec![leader]);
        }
        if scenario == "unreadable" || scenario == "unlimited" {
            let (record, _) = attempt("/repo", "build", StepLifecycle::Turn);
            fake.workflows.lock().unwrap().push(record);
            fake.rooms
                .lock()
                .unwrap()
                .insert(workspace().to_string(), Err("broken room".into()));
        }
        if scenario == "unlimited" {
            manual.limits.as_mut().unwrap().step_limit = None;
        }
        if scenario == "stagger" {
            let first = turn(try_admit_in(&fake.host(), &config(""), &manual).unwrap());
            assert!(first.started_past_limit().is_none());
            first.report_launch(&workspace(), &"manual".into());
            drop(first);
            for path in ticket_paths(&fake.queue_dir()).unwrap() {
                std::fs::remove_file(path).unwrap();
            }
        }
        let admitted = turn(
            try_admit_in(
                &fake.host(),
                &config("max-parallel-agents=1\ncheck='capacity'"),
                &manual,
            )
            .unwrap(),
        );
        actual.push((scenario, admitted.started_past_limit().map(str::to_owned)));
        assert!(fake.checked_groups().is_empty());
    }
    assert_eq!(
        actual,
        [
            (
                "full",
                Some("step build: 1 of 1 agents open (@claude)".into())
            ),
            ("clear", None),
            ("unreadable", None),
            ("stagger", Some("step build: next start in 10m".into())),
            ("unlimited", None),
        ]
    );
}

#[test]
fn limits_manual_claims_never_wait_and_restart_workflow_stagger() {
    let fake = FakeHost::new();
    let (record, leader) = attempt("/repo", "build", StepLifecycle::Turn);
    install(&fake, record, vec![leader]);
    let mut manual = limited(Some("max-parallel=1\nstagger='10m'"), None);
    manual.limits.as_mut().unwrap().may_wait = false;
    let first = turn(
        try_admit_in(
            &fake.host(),
            &config("max-parallel-agents=1\ncheck='capacity'"),
            &manual,
        )
        .unwrap(),
    );
    first.report_launch(&workspace(), &"manual".into());
    assert!(fake.start_stamps().is_file());
    assert!(fake.checked_groups().is_empty());
}

#[test]
fn limits_fail_closed_on_unreadable_rooms_and_corrupt_stamp_records_they_read() {
    let fake = FakeHost::new();
    std::fs::write(fake.start_stamps(), "broken").unwrap();
    let request = limited(None, Some("stagger='1m'"));
    assert_eq!(
        held(try_admit_in(&fake.host(), &config(""), &request).unwrap()).kind,
        HoldKind::Store
    );
    drop(turn(
        try_admit_in(
            &fake.host(),
            &config(""),
            &limited(None, Some("max-parallel=1")),
        )
        .unwrap(),
    ));
    std::fs::remove_file(fake.start_stamps()).unwrap();
    let (record, _) = attempt("/repo", "build", StepLifecycle::Turn);
    fake.workflows.lock().unwrap().push(record);
    fake.rooms
        .lock()
        .unwrap()
        .insert(workspace().to_string(), Err("broken room".into()));
    assert_eq!(
        held(try_admit_in(&fake.host(), &config(""), &request).unwrap()).kind,
        HoldKind::Store
    );
}
