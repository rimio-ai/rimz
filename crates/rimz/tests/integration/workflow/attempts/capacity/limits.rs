use super::*;
use rimz::agents::{
    AgentLifecycleObservation, LifecycleSignal, WorkflowAttemptRef, WorkflowAttemptRole,
};

/// Occupy the limit with a run of the same workflow whose leader works, or, `at_rest`, whose
/// session leader closed its turn since the attempt was published.
fn fill_limit(room: &Room, source: &records::RunRecord, at_rest: bool) {
    let state = room.env.state_path_for(&room.env.project_root);
    let mut record = serde_json::to_value(source).unwrap();
    let id = WorkflowRunId::new();
    let checkout = room.env.project_root.join("occupied");
    let mut run = rimz::store::run::RunRecord::new(
        room.env.workspace_id.clone(),
        rimz::ids::AgentKind::new_unchecked("codex"),
        rimz::agents::PermissionMode::Auto,
        "occupied".into(),
        checkout.clone(),
    );
    run.agent_id = Some("existing".into());
    run.status = rimz::store::run::RunStatus::Running;
    run.workflow = Some(WorkflowAttemptRef {
        run: id.clone(),
        seq: 1,
        role: WorkflowAttemptRole::Step,
    });
    record["id"] = json!(id);
    record["scope"]["path"] = json!(checkout);
    record["attempt"] = json!({
        "kind":"agent", "seq":1, "node":"build", "started_at":jiff::Timestamp::now(),
        "host":{"pid":std::process::id(),"start":rimz::proc::process_start_token(std::process::id())},
        "run":{"run_id":run.run_id,"handle":"@codex"}
    });
    if at_rest {
        record["attempt"]["lifecycle"] = json!("session");
        rimz::harness::run::create(&state, &run).unwrap();
    }
    let record: records::RunRecord = serde_json::from_value(record).unwrap();
    {
        let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
        records::initialize(&state, &record, &guard).unwrap();
    }
    let workspace = room.env.resolve_workspace(&room.env.project_root);
    let end = at_rest.then_some(LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background: false,
        turn_id: None,
    });
    for signal in [
        LifecycleSignal::Registered,
        LifecycleSignal::TurnStarted { turn_id: None },
    ]
    .into_iter()
    .chain(end)
    {
        let mut observation = AgentLifecycleObservation::new(Some("existing".into()), signal);
        observation.agent_name = Some("existing".into());
        observation.worktree_path = Some(checkout.display().to_string());
        observation.launch.workflow = Some(WorkflowAttemptRef {
            run: id.clone(),
            seq: 1,
            role: WorkflowAttemptRole::Step,
        });
        observation.launch.automation = Some(rimz::agents::Automation::Workflow {
            workflow: "review".into(),
            step: "build".into(),
            depth: None,
        });
        room.env
            .store()
            .append_event(&rimz::EventEnvelope::agent_lifecycle(
                workspace.workspace_id.clone(),
                &workspace.session_name,
                "codex",
                "test",
                &observation,
            ))
            .unwrap();
    }
}

#[test]
fn wide_step_groups_start_under_a_step_limit_and_fail_only_a_machine_cap() {
    let mut actual = Vec::new();
    for (machine_cap, skip) in [(false, false), (true, false), (true, true)] {
        let room = Room::new(
            "on.capacity='after'\n[agent.build.admission]\nthrottle='off'\nmax-parallel=1\n",
        );
        let spec = room.env.rimz_home().join("workflows/review/workflow.toml");
        fs::write(
            &spec,
            fs::read_to_string(&spec)
                .unwrap()
                .replace("agent = 'codex'", "agent = 'codex:lead,codex'"),
        )
        .unwrap();
        if machine_cap {
            let core = room.env.rimz_home().join("config.toml");
            let mut config = fs::read_to_string(&core).unwrap_or_default();
            config.push_str("\n[throttle]\nmax-parallel-automation=1\n");
            fs::write(core, config).unwrap();
        }
        let mut record = pending(&room, Some("nightly"));
        if skip {
            record.capacity = records::StartCapacity::Skip;
            save(&room, &serde_json::to_value(&record).unwrap());
        }
        let value = advance(&room, &record);
        assert!(value["record"]["wait"].is_null(), "{value:#}");
        assert!(
            !events(&value, "attempt")
                .iter()
                .any(|row| row["outcome"] == "died"),
            "{value:#}"
        );
        actual.push((
            value["record"]["state"].as_str().unwrap().to_owned(),
            value["record"]["visits"]["build"].clone(),
            events(&value, "route")
                .iter()
                .find(|row| row["outcome"] == "capacity")
                .map(|row| {
                    (
                        row["input"]["node"].clone(),
                        row["input"]["detail"]["waited_ms"].clone(),
                        row["input"]["detail"]["never_fits"].clone(),
                    )
                }),
        ));
    }
    assert_eq!(
        actual,
        [
            ("active".into(), json!(1), None),
            (
                "done".into(),
                json!(null),
                Some((json!("build"), json!(0), json!(true)))
            ),
            (
                "done".into(),
                json!(null),
                Some((json!("build"), json!(0), json!(true)))
            ),
        ]
    );
}

#[test]
fn restart_waits_for_window_and_capacity_and_refuses_layouts_over_a_machine_cap() {
    let mut actual = Vec::new();
    for (window, oversized, machine_cap) in [
        (true, false, false),
        (false, false, false),
        (false, true, false),
        (false, true, true),
    ] {
        let window = if window { "window='5h.left>=99'\n" } else { "" };
        let room = Room::new(&format!(
            "[agent.build.admission]\n{window}max-parallel=1\n"
        ));
        if machine_cap {
            let core = room.env.rimz_home().join("config.toml");
            let mut config = fs::read_to_string(&core).unwrap_or_default();
            config.push_str("\n[throttle]\nmax-parallel-automation=1\n");
            fs::write(core, config).unwrap();
        }
        let spec = room.env.rimz_home().join("workflows/review/workflow.toml");
        if oversized {
            fs::write(
                &spec,
                fs::read_to_string(&spec)
                    .unwrap()
                    .replace("agent = 'codex'", "agent = 'codex:lead,codex'"),
            )
            .unwrap();
        }
        let record = pending(&room, Some("nightly"));
        fill_limit(&room, &record, false);
        success(
            room.command(&["workflow", "restart", record.id.as_str()])
                .output()
                .unwrap(),
        );
        let value = until(&room.env, record.id.as_str(), |value| {
            value["record"]["attempt"]["run"].is_object()
                || value["record"]["wait"]["node"] == "build"
                || value["record"]["state"] == "failed"
                || value["record"]["state"] == "done"
        });
        actual.push((
            value["record"]["state"].as_str().unwrap().to_owned(),
            value["record"]["wait"]["outcome"]
                .as_str()
                .map(str::to_owned),
            value["record"]["visits"]["build"].clone(),
            events(&value, "route").iter().any(|event| {
                event["outcome"] == "capacity"
                    && event["input"]["reason"]
                        .as_str()
                        .is_some_and(|reason| reason.contains("more than max-parallel"))
            }),
        ));
    }
    assert_eq!(
        actual,
        [
            ("active".into(), Some("window".into()), json!(null), false),
            ("active".into(), Some("capacity".into()), json!(null), false),
            ("active".into(), Some("capacity".into()), json!(null), false),
            ("failed".into(), None, json!(null), true)
        ]
    );
}

#[test]
fn off_steps_still_hold_at_the_machine_automation_cap() {
    let room = Room::new("admission.throttle='off'\nadmission.give-up='1h'\n");
    let core = room.env.rimz_home().join("config.toml");
    let mut config = fs::read_to_string(&core).unwrap_or_default();
    config.push_str("\n[throttle]\nmax-parallel-automation=1\n");
    fs::write(core, config).unwrap();
    let record = pending(&room, Some("nightly"));
    fill_limit(&room, &record, false);
    let value = advance(&room, &record);
    assert_eq!(
        value["record"]["wait"]["hold"], "automation_cap",
        "{value:#}"
    );
    assert_eq!(
        value["record"]["wait"]["detail"],
        "1 of 1 automated agents open (@codex (@existing))"
    );
    assert_eq!(value["record"]["visits"], json!({}));
    assert!(tickets(&room).is_empty());
}

#[test]
fn a_session_waiting_for_the_user_frees_its_slot_for_the_next_start() {
    let room = Room::new("[agent.build.admission]\nthrottle='off'\ngive-up='1h'\nmax-parallel=1\n");
    let record = pending(&room, Some("nightly"));
    fill_limit(&room, &record, true);
    let value = advance(&room, &record);
    assert!(value["record"]["wait"].is_null(), "{value:#}");
    assert_eq!(value["record"]["visits"]["build"], 1, "{value:#}");
}

#[test]
fn limited_off_steps_hold_without_machine_config_and_give_up_through_capacity() {
    let mut actual = Vec::new();
    for (edge, terminal) in [("", "failed"), ("on.capacity='after'\n", "done")] {
        let room = Room::new(&format!(
            "{edge}[agent.build.admission]\nthrottle='off'\ngive-up='1h'\nmax-parallel=1\n"
        ));
        // A task-owned run as an older RimZ's loop fire wrote it: an automated start, held.
        let record = pending(&room, Some("nightly"));
        fill_limit(&room, &record, false);
        let mut value = advance(&room, &record);
        assert_eq!(value["record"]["wait"]["hold"], "step_limit", "{value:#}");
        assert_eq!(
            value["record"]["wait"]["detail"],
            "step build: 1 of 1 agents working (@codex)"
        );
        assert_eq!(value["record"]["visits"], json!({}));
        let listed = success(room.command(&["workflow", "list"]).output().unwrap());
        assert!(
            listed.contains(" · 1 held")
                && listed.contains("step build: 1 of 1 agents working (@codex)"),
            "{listed}"
        );
        assert!(!listed.contains("NEEDS YOU"), "{listed}");
        let now = jiff::Timestamp::now();
        value["record"]["wait"]["first"] = json!(now - jiff::SignedDuration::from_hours(2));
        value["record"]["wait"]["gives_up_at"] = json!(now - jiff::SignedDuration::from_hours(1));
        save(&room, &value["record"]);
        let value = advance(&room, &record);
        actual.push((
            value["record"]["state"] == terminal,
            events(&value, "route")
                .iter()
                .any(|row| row["outcome"] == "capacity"),
            value["record"]["visits"] == json!({}),
        ));
    }
    assert_eq!(actual, [(true, true, true); 2]);
}

#[test]
fn skipped_limited_starts_launch_at_a_full_cap_and_stamp_both_stagger_scopes() {
    for task in [None, Some("nightly")] {
        let room = Room::new("[agent.build.admission]\nmax-parallel=1\nstagger='2m'\n");
        let spec = room.env.rimz_home().join("workflows/review/workflow.toml");
        fs::write(
            &spec,
            fs::read_to_string(&spec).unwrap().replace(
                "[script.pr]",
                "[workflow.limit]\nmax-parallel=1\nstagger='3m'\n[script.pr]",
            ),
        )
        .unwrap();
        let mut record = pending(&room, task);
        fill_limit(&room, &record, false);
        record.capacity = records::StartCapacity::Skip;
        save(&room, &serde_json::to_value(&record).unwrap());
        let before = jiff::Timestamp::now().as_millisecond();
        let value = advance(&room, &record);
        assert_eq!(value["record"]["visits"]["build"], 1, "{value:#}");
        let skipped = "workflow review: 1 of 1 agents working (@codex)";
        assert_eq!(
            value["record"]["attempt"]["started_past_limit"], skipped,
            "{value:#}"
        );
        let shown = success(
            room.command(&["workflow", "show", record.id.as_str()])
                .output()
                .unwrap(),
        );
        let notice = format!("{skipped}; started anyway: the run skips capacity");
        assert!(shown.contains(&notice), "{shown}");
        let stagger = room.env.rimz_home().join("loops/start-stamps.json");
        assert!(
            stagger.is_file(),
            "skipped limited launches must restart stagger: {value:#}"
        );
        let stagger: Value = serde_json::from_slice(&fs::read(stagger).unwrap()).unwrap();
        let at = stagger["workflows"]["review"]["last_launch_ms"]
            .as_i64()
            .unwrap();
        assert!(
            at >= before && at <= jiff::Timestamp::now().as_millisecond(),
            "{stagger:#}"
        );
        assert_eq!(stagger["workflows"]["review"]["steps"]["build"], at);
        let queue = tickets(&room);
        assert_eq!(queue.len(), 1, "{queue:#?}");
        assert_eq!(queue[0]["state"], "launched");
        assert_eq!(queue[0]["machine_gated"], true);
        let definition = success(
            room.command(&["workflow", "show", "review"])
                .output()
                .unwrap(),
        );
        assert!(
            definition.contains("limit max-parallel=1 · stagger=3m"),
            "{definition}"
        );
        let step = success(
            room.command(&["workflow", "show", "review", "build"])
                .output()
                .unwrap(),
        );
        assert!(step.contains("max-parallel=1 · stagger=2m"), "{step}");
        success(
            room.command(&["workflow", "cancel", record.id.as_str()])
                .output()
                .unwrap(),
        );
        let value = show(&room.env, record.id.as_str());
        assert!(
            events(&value, "attempt")
                .iter()
                .any(|row| row["started_past_limit"] == skipped),
            "{value:#}"
        );
        let history = success(
            room.command(&["workflow", "show", record.id.as_str(), "--history"])
                .output()
                .unwrap(),
        );
        assert!(history.contains(&notice), "{history}");
    }
}

#[test]
fn skipped_starts_under_a_clear_cap_store_and_print_no_skip() {
    let room = Room::new("[agent.build.admission]\nmax-parallel=1\n");
    let mut record = pending(&room, None);
    record.capacity = records::StartCapacity::Skip;
    save(&room, &serde_json::to_value(&record).unwrap());
    let value = advance(&room, &record);
    assert_eq!(value["record"]["visits"]["build"], 1, "{value:#}");
    assert!(
        value["record"]["attempt"]
            .get("started_past_limit")
            .is_none(),
        "{value:#}"
    );
    let shown = success(
        room.command(&["workflow", "show", record.id.as_str()])
            .output()
            .unwrap(),
    );
    assert!(!shown.contains("started anyway"), "{shown}");
    success(
        room.command(&["workflow", "cancel", record.id.as_str()])
            .output()
            .unwrap(),
    );
    let value = show(&room.env, record.id.as_str());
    assert!(
        events(&value, "attempt")
            .iter()
            .all(|row| row.get("started_past_limit").is_none()),
        "{value:#}"
    );
    let history = success(
        room.command(&["workflow", "show", record.id.as_str(), "--history"])
            .output()
            .unwrap(),
    );
    assert!(!history.contains("started anyway"), "{history}");
}

#[test]
fn limited_off_admission_errors_settle_as_died_instead_of_bypassing_admission() {
    let room = Room::new("[agent.build.admission]\nthrottle='off'\nmax-parallel=1\n");
    let record = pending(&room, None);
    fs::create_dir_all(room.env.rimz_home().join("loops")).unwrap();
    fs::write(
        room.env.rimz_home().join("loops/throttle"),
        "not a directory",
    )
    .unwrap();
    let value = advance(&room, &record);
    assert!(
        events(&value, "attempt")
            .iter()
            .any(|row| row["outcome"] == "died"),
        "{value:#}"
    );
    assert_eq!(value["record"]["state"], "failed");
}

#[test]
fn lock_timeouts_leave_a_limit_wait_as_recorded_and_its_standing_ahead() {
    let room = Room::new("[agent.build.admission]\ngive-up='1h'\nmax-parallel=1\n");
    let record = pending(&room, Some("nightly"));
    fill_limit(&room, &record, false);
    let limited = advance(&room, &record);
    assert_eq!(
        limited["record"]["wait"]["hold"], "step_limit",
        "{limited:#}"
    );
    let queue = hold_queue(&room);
    let value = advance_queue_busy(&room, &record);
    assert_eq!(value["record"]["state"], "active", "{value:#}");
    assert_eq!(
        value["record"]["wait"], limited["record"]["wait"],
        "{value:#}"
    );
    assert_eq!(value["record"]["visits"], json!({}), "{value:#}");
    assert!(events(&value, "attempt").is_empty(), "{value:#}");
    drop(queue);
    let later = pending(&room, Some("nightly"));
    let value = advance(&room, &later);
    assert_eq!(value["record"]["wait"]["hold"], "step_limit", "{value:#}");
    assert_eq!(
        value["record"]["wait"]["detail"], "step build: 1 earlier start waiting",
        "{value:#}"
    );
}
