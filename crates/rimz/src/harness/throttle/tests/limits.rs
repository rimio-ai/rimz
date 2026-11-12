use super::*;
use crate::agents::{WorkflowAttemptRef, WorkflowAttemptRole};
use crate::harness::throttle::{StartLimit, WorkflowStart};
use crate::ids::{RunId, WorkflowRunId};
use crate::store::workflow::{self, StepLifecycle};
use serde_json::json;

fn limit(text: &str) -> StartLimit {
    let limit: Limit = toml::from_str(text).unwrap();
    StartLimit {
        max_parallel: limit.max_parallel,
        stagger: limit
            .stagger
            .as_deref()
            .map(crate::harness::duration::duration),
    }
}

fn limited(workflow: Option<&str>, step: Option<&str>) -> Run {
    Run {
        machine_gated: false,
        limits: Some(WorkflowStart {
            workflow: "review".into(),
            step: "build".into(),
            run: WorkflowRunId::new(),
            arrival: Timestamp::from_millisecond(1_000_000).unwrap(),
            workflow_limit: workflow.map(limit),
            step_limit: step.map(limit),
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

fn attempt(root: &str, node: &str, lifecycle: StepLifecycle) -> workflow::RunRecord {
    let id = WorkflowRunId::new();
    serde_json::from_value(json!({
        "version":crate::store::workflow::VERSION, "id":id, "workflow":"review", "task":"nightly", "spec_dir":"/defs",
        "spec_digest":"test", "scope":{"kind":"worktree", "path":root},
        "project_root":root, "created_at":Timestamp::UNIX_EPOCH,
        "entered_at":Timestamp::UNIX_EPOCH, "node":node, "state":"active",
        "visits":{}, "visit_grant":0, "deadline_fired":false, "next_seq":2,
        "attempt":{
            "kind":"agent", "seq":1, "node":node, "lifecycle":lifecycle,
            "started_at":Timestamp::UNIX_EPOCH, "host":{"pid":1,"start":null},
            "run":{"run_id":RunId::new(),"handle":"@leader"}
        }
    }))
    .unwrap()
}

fn named(mut record: workflow::RunRecord, handle: Option<&str>) -> workflow::RunRecord {
    let attempt = record.attempt.as_mut().unwrap();
    match handle {
        Some(handle) => attempt.run.as_mut().unwrap().handle = handle.into(),
        None => attempt.run = None,
    }
    record
}

fn judge(node: &str) -> workflow::LiveAttempt {
    serde_json::from_value(json!({
        "kind":"judge", "seq":2, "node":node, "started_at":Timestamp::UNIX_EPOCH,
        "host":{"pid":1,"start":null}
    }))
    .unwrap()
}

/// An open agent row stamped for `review`'s `node`, as a launched step leaves one.
fn stamped(record: &workflow::RunRecord, node: &str) -> AgentState {
    let mut leader = agent(&format!("leader-{}", record.id), AgentStatus::Running);
    leader.name = Some("leader".into());
    leader.workflow = Some(WorkflowAttemptRef {
        run: record.id.clone(),
        seq: 1,
        role: WorkflowAttemptRole::Step,
    });
    leader.automation = Some(crate::agents::Automation::Workflow {
        workflow: "review".into(),
        step: node.into(),
    });
    leader
}

fn install(fake: &FakeHost, record: workflow::RunRecord) {
    fake.workflows.lock().unwrap().push(record);
}

#[test]
fn limits_count_one_start_per_live_run_attempt_across_rooms() {
    let fake = FakeHost::new();
    let request = limited(Some("max-parallel=4"), None);
    install(
        &fake,
        named(
            attempt("/repo", "build", StepLifecycle::Session),
            Some("@first"),
        ),
    );
    let mut held_session = named(
        attempt("/repo", "fix", StepLifecycle::Session),
        Some("@second"),
    );
    held_session.held = Some(workflow::HeldSession {
        attempt: held_session.attempt.take().unwrap(),
        entered_at: Timestamp::UNIX_EPOCH,
        verdict: None,
    });
    held_session.node = "check".into();
    held_session.attempt = Some(judge("check"));
    install(&fake, held_session);
    let mut parked = named(
        attempt("/repo", "build", StepLifecycle::Turn),
        Some("@parked"),
    );
    parked.state = workflow::RunState::Parked;
    install(&fake, parked);
    let mut terminal = named(
        attempt("/repo", "build", StepLifecycle::Turn),
        Some("@done"),
    );
    terminal.state = workflow::RunState::Done;
    install(&fake, terminal);
    let mut other = named(
        attempt("/repo", "build", StepLifecycle::Turn),
        Some("@other"),
    );
    other.workflow = "other".into();
    install(&fake, other);
    let mut judged = attempt("/repo", "build", StepLifecycle::Turn);
    judged.attempt = Some(judge("build"));
    install(&fake, judged);
    let mut idle = attempt("/repo", "build", StepLifecycle::Turn);
    idle.attempt = None;
    install(&fake, idle);
    let mut own = named(attempt("/repo", "build", StepLifecycle::Turn), Some("@own"));
    own.id = request.limits.as_ref().unwrap().run.clone();
    install(&fake, own);
    install(
        &fake,
        named(attempt("/launching", "build", StepLifecycle::Turn), None),
    );

    let workflow = held(try_admit_in(&fake.host(), &config(""), &request).unwrap());
    assert_eq!(
        (workflow.kind, workflow.text.as_str()),
        (
            HoldKind::WorkflowLimit,
            "workflow review: 4 of 4 agents open (@first, @second, +2)"
        )
    );
    let step_request = |step: &str, cap: &str| {
        let mut step_request = request.clone();
        let start = step_request.limits.as_mut().unwrap();
        start.workflow_limit = None;
        start.step_limit = Some(limit(cap));
        start.step = step.into();
        step_request
    };
    let step = held(
        try_admit_in(
            &fake.host(),
            &config(""),
            &step_request("build", "max-parallel=3"),
        )
        .unwrap(),
    );
    assert_eq!(
        (step.kind, step.text.as_str()),
        (
            HoldKind::StepLimit,
            "step build: 3 of 3 agents open (@first, @parked, +1)"
        )
    );
    assert_eq!(
        held(
            try_admit_in(
                &fake.host(),
                &config(""),
                &step_request("fix", "max-parallel=1")
            )
            .unwrap()
        )
        .text,
        "step fix: 1 of 1 agents open (@second)"
    );
    drop(turn(
        try_admit_in(
            &fake.host(),
            &config(""),
            &step_request("check", "max-parallel=1"),
        )
        .unwrap(),
    ));
}

#[test]
fn a_workflow_limit_cannot_loosen_the_machine_automation_cap() {
    let fake = FakeHost::new();
    let record = attempt("/repo", "build", StepLifecycle::Session);
    fake.set_agents(
        &WorkspaceId::from_project_root(&record.project_root),
        vec![stamped(&record, "build")],
    );
    install(&fake, record);
    let request = limited(Some("max-parallel=10"), None);
    let hold =
        held(try_admit_in(&fake.host(), &config("max-parallel-automation=1"), &request).unwrap());
    assert_eq!(hold.text, "1 of 1 automated agents open (@claude)");
}

#[test]
fn limits_read_no_agent_snapshot() {
    let fake = FakeHost::new();
    let record = attempt("/repo", "build", StepLifecycle::Turn);
    let mut companion = stamped(&record, "build");
    companion.agent_id = "companion".into();
    fake.set_agents(
        &WorkspaceId::from_project_root(Path::new("/elsewhere")),
        vec![stamped(&record, "build"), companion],
    );
    fake.rooms.lock().unwrap().insert(
        WorkspaceId::from_project_root(&record.project_root).to_string(),
        Err("torn log".into()),
    );
    install(&fake, record);
    let full = limited(Some("max-parallel=1"), Some("max-parallel=1"));
    assert_eq!(
        held(try_admit_in(&fake.host(), &config(""), &full).unwrap()).text,
        "workflow review: 1 of 1 agents open (@leader)"
    );
    drop(turn(
        try_admit_in(
            &fake.host(),
            &config(""),
            &limited(Some("max-parallel=2"), Some("max-parallel=2")),
        )
        .unwrap(),
    ));
    assert_eq!(fake.agent_reads.load(Ordering::SeqCst), 0);
}

#[test]
fn limits_count_a_wide_start_as_one_at_both_scopes() {
    for workflow_limit in [true, false] {
        let fake = FakeHost::new();
        install(&fake, attempt("/repo", "build", StepLifecycle::Session));
        let limit = |cap: &str| {
            let mut request = if workflow_limit {
                limited(Some(cap), None)
            } else {
                limited(None, Some(cap))
            };
            request.groups[0].agents = 2;
            request
        };
        drop(turn(
            try_admit_in(&fake.host(), &config(""), &limit("max-parallel=2")).unwrap(),
        ));
        let scope = if workflow_limit {
            "workflow review"
        } else {
            "step build"
        };
        assert_eq!(
            held(try_admit_in(&fake.host(), &config(""), &limit("max-parallel=1")).unwrap()).text,
            format!("{scope}: 1 of 1 agents open (@leader)")
        );
    }
}

#[test]
fn wide_starts_wait_behind_a_launch_and_refuse_only_machine_caps() {
    for workflow_limit in [true, false] {
        let fake = FakeHost::new();
        let mut request = if workflow_limit {
            limited(Some("max-parallel=1"), None)
        } else {
            limited(None, Some("max-parallel=1"))
        };
        let launching = turn(try_admit_in(&fake.host(), &config(""), &request).unwrap());
        request.groups[0].agents = 2;
        let scope = if workflow_limit {
            "workflow review"
        } else {
            "step build"
        };
        assert_eq!(
            held(try_admit_in(&fake.host(), &config(""), &request).unwrap()).text,
            format!("{scope}: 1 start launching")
        );
        drop(launching);
        drop(turn(
            try_admit_in(&fake.host(), &config(""), &request).unwrap(),
        ));
        assert_eq!(
            try_admit_in(&fake.host(), &config("max-parallel-agents=1"), &request)
                .unwrap_err()
                .to_string(),
            "this start opens 2 agents, more than max-parallel-agents = 1 allows; raise it in ~/.rimz/config.toml"
        );
    }
}

#[test]
fn limits_reserve_the_last_slot_under_the_shared_queue_lock() {
    let fake = FakeHost::new();
    install(
        &fake,
        named(
            attempt("/existing", "build", StepLifecycle::Turn),
            Some("@existing"),
        ),
    );
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
    let (request, answer) = results
        .iter()
        .find(|(_, answer)| matches!(answer, Attempt::Turn(_)))
        .unwrap();
    let Attempt::Turn(turn) = answer else {
        unreachable!()
    };
    let later = || {
        held(
            try_admit_in(
                &fake.host(),
                &config(""),
                &limited(Some("max-parallel=2"), None),
            )
            .unwrap(),
        )
    };
    let launching = (
        HoldKind::WorkflowLimit,
        "workflow review: 1 start launching".to_owned(),
    );
    let loser = results
        .iter()
        .find_map(|(_, answer)| match answer {
            Attempt::Held(hold) => Some((hold.kind, hold.text.clone())),
            _ => None,
        })
        .expect("one start held");
    assert_eq!(loser, launching);
    let hold = later();
    assert_eq!((hold.kind, hold.text), launching);
    let mut record = named(
        attempt(request.root.to_str().unwrap(), "build", StepLifecycle::Turn),
        None,
    );
    record.id = request.limits.as_ref().unwrap().run.clone();
    install(&fake, record);
    let hold = later();
    assert_eq!((hold.kind, hold.text), launching);
    turn.report_launch(&request.workspace, &"provisional".into());
    let hold = later();
    assert_eq!(
        (hold.kind, hold.text.as_str()),
        (
            HoldKind::WorkflowLimit,
            "workflow review: 2 of 2 agents open (@existing, +1)"
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
    let mut record = named(
        attempt("/repo", "build", StepLifecycle::Turn),
        Some("@first"),
    );
    record.id = gated.limits.as_ref().unwrap().run.clone();
    install(&fake, record);
    first.report_launch(&workspace(), &"provisional".into());
    fake.set_agents(
        &workspace(),
        vec![agent("provisional", AgentStatus::Running)],
    );
    let later = limited(Some("max-parallel=1"), None);
    let hold = held(try_admit_in(&fake.host(), &configured, &later).unwrap());
    assert_eq!(
        (hold.kind, hold.text.as_str()),
        (
            HoldKind::WorkflowLimit,
            "workflow review: 1 of 1 agents open (@first)"
        )
    );
    assert_eq!(
        held(try_admit_in(&fake.host(), &config(""), &run("ordinary")).unwrap()).kind,
        HoldKind::Queue
    );
    let mut adopted = agent("adopted", AgentStatus::Idle);
    adopted.launch_id = Some("provisional".into());
    fake.set_agents(&workspace(), vec![adopted]);
    drop(turn(
        try_admit_in(&fake.host(), &config(""), &run("ordinary")).unwrap(),
    ));
    fake.workflows.lock().unwrap().clear();
    drop(turn(
        try_admit_in(&fake.host(), &configured, &later).unwrap(),
    ));
}

#[test]
fn limits_keep_fifo_standing_while_a_machine_turn_is_in_flight() {
    let fake = FakeHost::new();
    let configured = config("");
    install(&fake, attempt("/running", "build", StepLifecycle::Turn));
    let machine = turn(try_admit_in(&fake.host(), &config(""), &run("ordinary")).unwrap());

    let mut earlier = limited(Some("max-parallel=1"), None);
    earlier.machine_gated = true;
    let hold = held(try_admit_in(&fake.host(), &configured, &earlier).unwrap());
    assert_eq!(
        (hold.kind, hold.text.as_str()),
        (
            HoldKind::WorkflowLimit,
            "workflow review: 1 of 1 agents open (@leader)"
        )
    );
    let mut waiter = attempt("/earlier", "build", StepLifecycle::Turn);
    waiter.attempt = None;
    let first = Timestamp::from_millisecond(999_999).unwrap();
    waiter.wait = Some(
        serde_json::from_value(json!({
            "node":"build", "outcome":"capacity", "hold":hold.kind, "first":first, "last":first,
            "count":1
        }))
        .unwrap(),
    );
    fake.workflows.lock().unwrap().push(waiter);

    drop(machine);
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
    let admitted = try_admit_in(&fake.host(), &configured, &other).unwrap();
    assert_eq!(
        (ticket.machine_gated, matches!(admitted, Attempt::Turn(_))),
        (false, true),
        "admission: {admitted:?}"
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
        let mut waiter = attempt("/waiter", step, StepLifecycle::Turn);
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
fn skipped_starts_launch_past_every_hold_and_carry_each_one() {
    let mut actual = Vec::new();
    for scenario in [
        "full",
        "clear",
        "unreadable",
        "stagger",
        "no-room",
        "check-error",
        "memory",
    ] {
        let fake = FakeHost::new();
        let mut skipped = limited(None, Some("max-parallel=1\nstagger='10m'"));
        skipped.machine_gated = true;
        skipped.skip = true;
        let mut configured = config("check='capacity'");
        match scenario {
            "full" => install(&fake, attempt("/repo", "build", StepLifecycle::Turn)),
            "unreadable" => {
                install(&fake, attempt("/repo", "build", StepLifecycle::Turn));
                fake.run_errors
                    .lock()
                    .unwrap()
                    .insert(workspace().to_string(), "broken room".into());
            }
            "stagger" => {
                let first = turn(try_admit_in(&fake.host(), &configured, &skipped).unwrap());
                assert!(first.started_past_limit().is_none());
                first.report_launch(&workspace(), &"skipped".into());
                drop(first);
                for path in ticket_paths(&fake.queue_dir()).unwrap() {
                    std::fs::remove_file(path).unwrap();
                }
                fake.checks.lock().unwrap().clear();
            }
            "no-room" => *fake.check_answer.lock().unwrap() = Ok((Some(75), false)),
            "check-error" => *fake.check_answer.lock().unwrap() = Ok((Some(2), false)),
            "memory" => {
                configured.min_memory = Some("1GB".into());
                *fake.memory.lock().unwrap() = Ok(0);
            }
            _ => {}
        }
        let admitted = turn(try_admit_in(&fake.host(), &configured, &skipped).unwrap());
        actual.push((
            scenario,
            admitted.started_past_limit().map(str::to_owned),
            fake.checks.lock().unwrap().len(),
        ));
    }
    assert_eq!(
        actual,
        [
            (
                "full",
                Some("step build: 1 of 1 agents open (@leader)".into()),
                1
            ),
            ("clear", None, 1),
            (
                "unreadable",
                Some("cannot count open agents: broken room".into()),
                1
            ),
            ("stagger", Some("step build: next start in 10m".into()), 1),
            (
                "no-room",
                Some("claude: no capacity (your check)".into()),
                1
            ),
            (
                "check-error",
                Some("capacity check failing: claude exit 2".into()),
                1
            ),
            ("memory", Some("available memory 0 B < 1 GB".into()), 1),
        ]
    );
}

#[test]
fn skipped_starts_still_claim_and_stamp_the_workflow_stagger() {
    let fake = FakeHost::new();
    install(&fake, attempt("/repo", "build", StepLifecycle::Turn));
    let mut skipped = limited(Some("max-parallel=1\nstagger='10m'"), None);
    skipped.skip = true;
    let first = turn(
        try_admit_in(
            &fake.host(),
            &config("max-parallel-agents=1\ncheck='capacity'"),
            &skipped,
        )
        .unwrap(),
    );
    assert_eq!(
        first.started_past_limit(),
        Some("workflow review: 1 of 1 agents open (@leader)")
    );
    first.report_launch(&workspace(), &"skipped".into());
    assert!(fake.start_stamps().is_file());
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
    install(&fake, attempt("/repo", "build", StepLifecycle::Turn));
    fake.run_errors
        .lock()
        .unwrap()
        .insert(workspace().to_string(), "broken room".into());
    let hold = held(
        try_admit_in(
            &fake.host(),
            &config(""),
            &limited(None, Some("max-parallel=1")),
        )
        .unwrap(),
    );
    assert_eq!(
        (hold.kind, hold.text.as_str()),
        (HoldKind::Store, "cannot count open agents: broken room")
    );
}

#[test]
fn system_census_skips_a_stranded_or_broken_run_home_but_not_an_unreadable_one() {
    let root = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(root.path());
    let state = crate::StatePaths::for_workspace(workspace.clone()).unwrap();
    let mut live = attempt(
        root.path().to_str().unwrap(),
        "build",
        StepLifecycle::Session,
    );
    live.project_root = root.path().to_owned();
    std::fs::create_dir_all(&state.root).unwrap();
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    workflow::initialize(&state, &live, b"[workflow]", &guard).unwrap();
    let old = attempt(
        root.path().to_str().unwrap(),
        "build",
        StepLifecycle::Session,
    );
    workflow::initialize(&state, &old, b"[workflow]", &guard).unwrap();
    let mut value = serde_json::to_value(&old).unwrap();
    value["version"] = json!(3);
    std::fs::write(state.workflow(&old.id).record, value.to_string()).unwrap();
    drop(guard);
    let ids: Vec<_> = SystemHost
        .workflows(&workspace)
        .unwrap()
        .into_iter()
        .map(|run| run.id)
        .collect();
    assert_eq!(ids, [live.id.clone()]);
    std::fs::write(state.workflow(&old.id).record, "{").unwrap();
    let ids: Vec<_> = SystemHost
        .workflows(&workspace)
        .unwrap()
        .into_iter()
        .map(|run| run.id)
        .collect();
    assert_eq!(
        ids,
        [live.id],
        "a record that is no JSON object holds no launch"
    );
    // A directory where the record belongs fails the read for any uid.
    std::fs::remove_file(state.workflow(&old.id).record).unwrap();
    std::fs::create_dir(state.workflow(&old.id).record).unwrap();
    let error = SystemHost.workflows(&workspace).unwrap_err();
    assert!(error.contains("unreadable"), "{error}");
}
