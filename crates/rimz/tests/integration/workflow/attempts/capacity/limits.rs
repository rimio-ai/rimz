use super::*;
use rimz::agents::{
    AgentLifecycleObservation, LifecycleSignal, WorkflowAttemptRef, WorkflowAttemptRole,
};

fn fill_limit(room: &Room, source: &records::RunRecord) {
    let state = room.env.state_path_for(&room.env.project_root);
    let mut record = serde_json::to_value(source).unwrap();
    let id = WorkflowRunId::new();
    let checkout = room.env.project_root.join("occupied");
    record["id"] = json!(id);
    record["scope"]["path"] = json!(checkout);
    record["attempt"] = json!({
        "kind":"agent", "seq":1, "node":"build", "started_at":jiff::Timestamp::now(),
        "host":{"pid":std::process::id(),"start":rimz::proc::process_start_token(std::process::id())},
        "run":{"run_id":rimz::ids::RunId::new(),"handle":"@codex"}
    });
    let record: records::RunRecord = serde_json::from_value(record).unwrap();
    {
        let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
        records::initialize(
            &state,
            &record,
            &fs::read(source.spec_dir.join("workflow.toml")).unwrap(),
            &guard,
        )
        .unwrap();
    }
    let workspace = room.env.resolve_workspace(&room.env.project_root);
    for signal in [
        LifecycleSignal::Registered,
        LifecycleSignal::TurnStarted { turn_id: None },
    ] {
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
    for machine_cap in [false, true] {
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
        let record = pending(&room, Some("nightly"));
        let value = advance(&room, &record);
        assert!(value["record"]["wait"].is_null(), "{value:#}");
        assert!(
            !events(&value, "route")
                .iter()
                .any(|row| row["outcome"] == "capacity"),
            "{value:#}"
        );
        actual.push((
            value["record"]["state"].as_str().unwrap().to_owned(),
            value["record"]["visits"]["build"].clone(),
            events(&value, "attempt")
                .iter()
                .find(|row| row["outcome"] == "died")
                .map(|row| row["reason"].clone()),
        ));
    }
    assert_eq!(
        actual,
        [
            ("active".into(), json!(1), None),
            (
                "failed".into(),
                json!(1),
                Some(json!(
                    "this start opens 2 agents, more than max-parallel-automation = 1 allows; raise it in ~/.rimz/config.toml"
                ))
            ),
        ]
    );
}

#[test]
fn restart_is_manual_for_window_and_capacity_but_still_refuses_layouts_over_a_machine_cap() {
    let mut actual = Vec::new();
    for (oversized, machine_cap) in [(false, false), (true, false), (true, true)] {
        let room = Room::new("[agent.build.admission]\nwindow='5h.left>=99'\nmax-parallel=1\n");
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
        fill_limit(&room, &record);
        success(
            room.command(&["workflow", "restart", record.id.as_str()])
                .output()
                .unwrap(),
        );
        let value = until(&room.env, record.id.as_str(), |value| {
            value["record"]["attempt"]["run"].is_object()
                || value["record"]["state"] == "failed"
                || value["record"]["state"] == "done"
        });
        actual.push((
            value["record"]["state"].as_str().unwrap().to_owned(),
            value["record"]["wait"].is_null(),
            events(&value, "attempt").iter().any(|event| {
                event["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("more than max-parallel"))
            }),
        ));
    }
    assert_eq!(
        actual,
        [
            ("active".into(), true, false),
            ("active".into(), true, false),
            ("failed".into(), true, true)
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
    fill_limit(&room, &record);
    let value = advance(&room, &record);
    assert_eq!(
        value["record"]["wait"]["hold"], "automation_cap",
        "{value:#}"
    );
    assert_eq!(
        value["record"]["wait"]["detail"],
        "1 of 1 automated agents open (@codex)"
    );
    assert_eq!(value["record"]["visits"], json!({}));
    assert!(tickets(&room).is_empty());
}

#[test]
fn limited_off_steps_hold_without_machine_config_and_give_up_through_capacity() {
    let mut actual = Vec::new();
    for (edge, terminal) in [("", "failed"), ("on.capacity='after'\n", "done")] {
        let room = Room::new(&format!(
            "{edge}[agent.build.admission]\nthrottle='off'\ngive-up='1h'\nmax-parallel=1\n"
        ));
        fs::write(
            room.env.rimz_home().join("loop.toml"),
            format!(
                "[tasks.nightly]\nworkflow='review'\nroot={:?}\nevery='1h'\n",
                room.env.project_root,
            ),
        )
        .unwrap();
        let record = pending(&room, Some("nightly"));
        fill_limit(&room, &record);
        let mut value = advance(&room, &record);
        assert_eq!(value["record"]["wait"]["hold"], "step_limit", "{value:#}");
        assert_eq!(
            value["record"]["wait"]["detail"],
            "step build: 1 of 1 agents open (@codex)"
        );
        assert_eq!(value["record"]["visits"], json!({}));
        for args in [&["workflow", "list"][..], &["loop", "list"][..]] {
            let listed = success(room.command(args).output().unwrap());
            assert!(
                listed.contains(if args[0] == "workflow" {
                    " · 1 held"
                } else {
                    "HELD"
                }) && listed.contains("step build: 1 of 1 agents open (@codex)"),
                "{listed}"
            );
            assert!(!listed.contains("NEEDS YOU"), "{listed}");
        }
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
fn limited_manual_starts_launch_at_a_full_cap_and_stamp_both_stagger_scopes() {
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
        fill_limit(&room, &record);
        record.manual = task.is_some();
        save(&room, &serde_json::to_value(&record).unwrap());
        let before = jiff::Timestamp::now().as_millisecond();
        let value = advance(&room, &record);
        assert_eq!(value["record"]["visits"]["build"], 1, "{value:#}");
        let skipped = "workflow review: 1 of 1 agents open (@codex)";
        assert_eq!(
            value["record"]["attempt"]["started_past_limit"], skipped,
            "{value:#}"
        );
        let shown = success(
            room.command(&["workflow", "show", record.id.as_str()])
                .output()
                .unwrap(),
        );
        let notice = format!("{skipped}; started anyway: manual starts do not wait");
        assert!(shown.contains(&notice), "{shown}");
        let stagger = room.env.rimz_home().join("loops/start-stamps.json");
        assert!(
            stagger.is_file(),
            "manual limited launches must restart stagger: {value:#}"
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
        assert_eq!(queue[0]["machine_gated"], false);
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
fn limited_manual_starts_under_a_clear_cap_store_and_print_no_skip() {
    let room = Room::new("[agent.build.admission]\nmax-parallel=1\n");
    let record = pending(&room, None);
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
    fill_limit(&room, &record);
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
