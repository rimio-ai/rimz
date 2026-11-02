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
        "host":{"pid":std::process::id(),"start":rimz::proc::process_start_token(std::process::id())}
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
fn limited_off_steps_hold_without_machine_config_and_give_up_through_capacity() {
    let mut actual = Vec::new();
    for (edge, terminal) in [("", "failed"), ("on.capacity='after'\n", "done")] {
        let room = Room::new(&format!(
            "throttle='off'\ncapacity-wait='1h'\n{edge}[steps.build.limit]\nmax-active=1\n"
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
        assert_eq!(value["record"]["wait"]["detail"], "limit build 1/1 working");
        assert_eq!(value["record"]["visits"], json!({}));
        for args in [&["workflow", "list"][..], &["loop", "list"][..]] {
            let listed = success(room.command(args).output().unwrap());
            assert!(
                listed.contains("HELD") && listed.contains("limit build 1/1 working"),
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
fn limited_manual_starts_launch_at_a_full_cap_and_stamp_both_pace_scopes() {
    for task in [None, Some("nightly")] {
        let room = Room::new("[steps.build.limit]\nmax-active=1\npace='2m'\n");
        let spec = room.env.rimz_home().join("workflows/review/workflow.toml");
        fs::write(
            &spec,
            fs::read_to_string(&spec).unwrap().replace(
                "[checks.pr]",
                "[workflow.limit]\nmax-active=1\npace='3m'\n[checks.pr]",
            ),
        )
        .unwrap();
        let mut record = pending(&room, task);
        fill_limit(&room, &record);
        record.manual = task.is_some();
        save(&room, &serde_json::to_value(&record).unwrap());
        let before = jiff::Timestamp::now().as_millisecond();
        let value = advance(&room, &record);
        assert_eq!(value["record"]["visits"]["build"]["total"], 1, "{value:#}");
        let pace = room.env.rimz_home().join("loops/workflow-pace.json");
        assert!(
            pace.is_file(),
            "manual limited launches must restart pace: {value:#}"
        );
        let pace: Value = serde_json::from_slice(&fs::read(pace).unwrap()).unwrap();
        let at = pace["review"]["last_launch_ms"].as_i64().unwrap();
        assert!(
            at >= before && at <= jiff::Timestamp::now().as_millisecond(),
            "{pace:#}"
        );
        assert_eq!(pace["review"]["steps"]["build"], at);
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
            definition.contains("limit max-active=1 · pace=3m"),
            "{definition}"
        );
        let step = success(
            room.command(&["workflow", "show", "review", "build"])
                .output()
                .unwrap(),
        );
        assert!(step.contains("max-active=1 · pace=2m"), "{step}");
        success(
            room.command(&["workflow", "cancel", record.id.as_str()])
                .output()
                .unwrap(),
        );
    }
}

#[test]
fn limited_off_admission_errors_settle_as_died_instead_of_bypassing_admission() {
    let room = Room::new("throttle='off'\n[steps.build.limit]\nmax-active=1\n");
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
    assert_eq!(value["record"]["state"], "done");
}
