use super::*;
use rimz::agents::{AgentLifecycleObservation, LaunchParams, LaunchedBy, LifecycleSignal};
use rimz::ids::{AgentKind, MuxName, PaneId};

fn seed(room: &Room, name: &str, pane: u32, launch: LaunchParams, signal: LifecycleSignal) {
    let workspace = room.env.resolve_workspace(&room.env.project_root);
    let mut observation = AgentLifecycleObservation::new(Some(name.into()), signal);
    observation.agent_name = Some(name.into());
    observation.launch = launch;
    observation.pane_id = Some(PaneId::from_parts(
        MuxName::Zellij,
        format!("terminal_{pane}"),
    ));
    observation.worktree_path = Some(room.env.project_root.display().to_string());
    room.env
        .store()
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            workspace.workspace_id,
            workspace.session_name,
            "claude",
            "test",
            &observation,
        ))
        .unwrap();
}

#[test]
fn stops_pinned_companions_even_without_a_live_leader() {
    for ended_leader in [false, true] {
        let room = Room::new("");
        let (id, run) = room.launch();
        let launch = LaunchParams {
            workflow: run.workflow.clone(),
            ..Default::default()
        };
        if ended_leader {
            seed(
                &room,
                "ended-leader",
                70,
                launch.clone(),
                LifecycleSignal::Registered,
            );
            seed(
                &room,
                "ended-leader",
                70,
                launch.clone(),
                LifecycleSignal::Ended,
            );
        }
        seed(&room, "companion", 71, launch, LifecycleSignal::Registered);
        seed(
            &room,
            "unrelated",
            72,
            LaunchParams::default(),
            LifecycleSignal::Registered,
        );
        fs::write(room.env.project_root.join("panes.json"), r#"[{"id":71,"is_plugin":false,"tab_id":1,"title":"companion"},{"id":72,"is_plugin":false,"tab_id":1,"title":"unrelated"}]"#).unwrap();
        success(room.command(&["workflow", "cancel", &id]).output().unwrap());
        let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
        let closed: Vec<_> = trace
            .lines()
            .filter(|line| line.contains("close-pane"))
            .filter_map(|line| line.split_whitespace().last())
            .collect();
        assert!(
            closed.contains(&"terminal_71"),
            "pinned companion must stop: {trace}"
        );
        assert!(
            !closed.contains(&"terminal_72"),
            "unrelated pane must survive: {trace}"
        );
    }
}

#[test]
fn takeover_waits_for_a_working_occupant_in_an_omitted_live_tab() {
    let room = Room::new("takeover=true");
    fs::write(room.env.project_root.join("spawn.log"), "").unwrap();
    fs::write(room.env.project_root.join("panes.json"), "[]").unwrap();
    for event in ["SessionStart", "UserPromptSubmit"] {
        let mut cmd = room.hook_command("claude");
        cmd.env("RIMZ_AGENT_NAME", "resident")
            .env("ZELLIJ", "1")
            .env("ZELLIJ_PANE_ID", "71");
        success(
            room.env
                .spawn_payload(
                    cmd,
                    &json!({
                        "hook_event_name": event,
                        "session_id": "resident",
                        "cwd": room.env.project_root,
                    })
                    .to_string(),
                )
                .wait_with_output()
                .unwrap(),
        );
        room.env.drain_hooks();
    }
    let snapshot = room.env.store().snapshot_cached().unwrap();
    let resident = snapshot
        .agents
        .iter()
        .find(|row| row.name.as_deref() == Some("resident"))
        .unwrap();
    assert!(matches!(
        rimz::store::runtime::agent_liveness(resident),
        rimz::store::runtime::AgentLiveness::Live { .. }
    ));
    let id = room.start();
    let value = until(&room.env, &id, |value| {
        value["record"]["wait"]["outcome"] == "takeover"
            || value["record"]["attempt"]["run"].is_object()
    });
    assert_eq!(value["record"]["wait"]["outcome"], "takeover");
    assert!(value["record"]["visits"].as_object().unwrap().is_empty());
    assert!(value["record"]["attempt"].is_null());
    let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
    assert!(!trace.contains("new-tab"));
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
}

#[test]
fn takeover_waits_for_open_turn_then_stops_idle_occupant() {
    let room = Room::new("takeover=true");
    fs::write(
        room.env.project_root.join("panes.json"),
        r#"[{"id":71,"is_plugin":false,"tab_id":1,"title":"claude"}]"#,
    )
    .unwrap();
    seed(
        &room,
        "resident",
        71,
        LaunchParams::default(),
        LifecycleSignal::Registered,
    );
    seed(
        &room,
        "resident",
        71,
        LaunchParams::default(),
        LifecycleSignal::TurnStarted { turn_id: None },
    );
    let id = room.start();
    let value = until(&room.env, &id, |v| {
        v["record"]["wait"]["outcome"] == "takeover"
    });
    assert!(value["record"]["visits"].as_object().unwrap().is_empty());
    assert!(
        success(room.command(&["workflow", "show", &id]).output().unwrap()).contains("is working")
    );
    seed(
        &room,
        "resident",
        71,
        LaunchParams::default(),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    );
    success(
        room.command(&["workflow", "advance", &id])
            .output()
            .unwrap(),
    );
    let _ = room.attempt(&id);
    let snapshot = room
        .env
        .store()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap();
    assert!(
        snapshot
            .agents
            .iter()
            .find(|row| row.agent_id.as_str() == "resident")
            .unwrap()
            .ended_at
            .is_none(),
        "takeover must not fabricate a lifecycle end"
    );
    let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
    assert!(trace.contains("close-pane"));
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
}

#[test]
fn successor_visit_ignores_the_stopped_attempts_closed_pane() {
    let room = Room::new("timeout='1h'");
    let definition = room.env.rimz_home().join("workflows/review/workflow.toml");
    let spec = fs::read_to_string(&definition)
        .unwrap()
        .replace("on.done = '@done'", "on.done = 'build'");
    fs::write(definition, spec).unwrap();
    let (id, run) = room.launch();
    fs::write(
        room.env.project_root.join("panes.json"),
        r#"[{"id":61,"is_plugin":false,"tab_id":1,"title":"codex"}]"#,
    )
    .unwrap();
    let mut owner = dummy_agent_process();
    // Publish the live owner even when the launch fell inside the cadence window.
    fs::File::open(room.env.store().paths().cache_dir.join("publish.stamp"))
        .unwrap()
        .set_modified(std::time::SystemTime::UNIX_EPOCH)
        .unwrap();
    room.hook_with_owner(
        &id,
        &run,
        owner.id(),
        json!({"hook_event_name":"SessionStart"}),
    );
    let state = room.env.state_path_for(&room.env.project_root);
    {
        let guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
        let paths = state.workflow(&id.parse().unwrap());
        let mut record = records::load(&paths).unwrap();
        record.attempt.as_mut().unwrap().started_at =
            jiff::Timestamp::now() - jiff::SignedDuration::from_hours(2);
        records::write(&paths, &record, &guard).unwrap();
        // The trace shim cannot kill the owner when it closes the attempt's pane.
        // End it while admission is locked, leaving the published roster intact.
        owner.kill().unwrap();
        owner.wait().unwrap();
        let cached = rimz::store::snapshot::read_fresh_latest(&state)
            .expect("the live-owner hook published the roster");
        let ghost = cached
            .agents
            .iter()
            .find(|agent| agent.name == run.agent_name)
            .unwrap();
        assert_eq!(
            rimz::store::runtime::agent_liveness(ghost),
            rimz::store::runtime::AgentLiveness::Dead
        );
    }
    success(
        room.command(&["workflow", "advance", &id])
            .output()
            .unwrap(),
    );
    let value = until(&room.env, &id, |value| {
        value["record"]["visits"]["build"]["total"] == 2
            && value["record"]["attempt"]["run"].is_object()
    });
    assert_ne!(
        value["record"]["attempt"]["run"]["run_id"],
        run.run_id.as_str()
    );
    assert_eq!(value["record"]["prev_attempt"]["outcome"], "timeout");
    assert!(
        room.env
            .store()
            .runtime_projection(rimz::RuntimeScope::Audit)
            .unwrap()
            .agents
            .iter()
            .find(|row| row.name == run.agent_name)
            .unwrap()
            .ended_at
            .is_none(),
        "stopping a pane must not fabricate a lifecycle end"
    );
    let panes: Value =
        serde_json::from_slice(&fs::read(room.env.project_root.join("panes.json")).unwrap())
            .unwrap();
    assert_eq!(panes, json!([]));
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
}

#[test]
fn stop_records_recursive_children_and_leaves_peers_and_cohorts_live() {
    let room = Room::new("");
    let (id, run) = room.launch();
    room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
    let root = room
        .env
        .store()
        .snapshot_cached()
        .unwrap()
        .agents
        .iter()
        .find(|row| row.name == run.agent_name)
        .unwrap()
        .clone();
    let parent = root.launch_id.as_ref().unwrap_or(&root.agent_id).clone();
    seed(
        &room,
        "child",
        72,
        LaunchParams {
            parent_agent_id: Some(parent.clone()),
            parent_agent_kind: Some(root.kind.clone()),
            launch_depth: Some(1),
            ..Default::default()
        },
        LifecycleSignal::Registered,
    );
    seed(
        &room,
        "grandchild",
        73,
        LaunchParams {
            parent_agent_id: Some("child".into()),
            parent_agent_kind: Some(AgentKind::new_unchecked("claude")),
            launch_depth: Some(2),
            ..Default::default()
        },
        LifecycleSignal::Registered,
    );
    let peer = LaunchParams {
        launched_by: Some(Box::new(LaunchedBy {
            kind: root.kind.clone(),
            agent_id: parent,
        })),
        ..Default::default()
    };
    seed(&room, "peer", 74, peer.clone(), LifecycleSignal::Registered);
    seed(
        &room,
        "seat",
        75,
        LaunchParams {
            team: Some("forge".into()),
            role: Some("coder".into()),
            channel: Some("feature".into()),
            ..peer
        },
        LifecycleSignal::Registered,
    );
    fs::write(
        room.env.project_root.join("panes.json"),
        serde_json::to_vec(
            &[61, 72, 73, 74, 75]
                .map(|id| json!({"id":id,"is_plugin":false,"tab_id":1,"title":"claude"})),
        )
        .unwrap(),
    )
    .unwrap();
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
    let value = settled(&room.env, &id, "canceled");
    let children = &value["record"]["prev_attempt"]["children"];
    assert_eq!(children["stopped"].as_array().unwrap().len(), 2);
    assert_eq!(children["live"].as_array().unwrap().len(), 2);
    assert!(
        children["live"]
            .as_array()
            .unwrap()
            .iter()
            .any(|child| child["team"]["name"] == "forge")
    );
    let snapshot = room
        .env
        .store()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap();
    for row in snapshot
        .agents
        .iter()
        .filter(|row| ["child", "grandchild", "peer", "seat"].contains(&row.agent_id.as_str()))
    {
        assert!(
            row.ended_at.is_none(),
            "pane closure is not a lifecycle end"
        );
    }
    let panes: Value =
        serde_json::from_slice(&fs::read(room.env.project_root.join("panes.json")).unwrap())
            .unwrap();
    assert_eq!(
        panes
            .as_array()
            .unwrap()
            .iter()
            .map(|pane| pane["id"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![74, 75]
    );
    let human = success(room.command(&["workflow", "show", &id]).output().unwrap());
    assert!(human.contains("stopped (2)") && human.contains("live (2)") && human.contains("forge"));
}

#[test]
fn turn_error_wakes_and_preserves_typed_limit_reset() {
    for child in [false, true] {
        let room = Room::with_kind("", "claude");
        crate::common::trust_claude_project(&room.env, &room.env.project_root);
        let definition = room.env.rimz_home().join("workflows/review");
        let spec = fs::read_to_string(definition.join("workflow.toml")).unwrap();
        fs::write(
            definition.join("workflow.toml"),
            format!("{spec}on.pending='@wait'\ntimeout='1h'\n"),
        )
        .unwrap();
        fs::write(definition.join("checks/pr.sh"), "if grep -q '^  \"node\": \"pr\"' \"$RIMZ_WORKFLOW_DIR/context.json\"; then printf '{\"outcome\":\"go\"}'; else r=$(sed -n 's/^ *\"reset_at\": \\([^,]*\\),*$/\\1/p' \"$RIMZ_WORKFLOW_DIR/context.json\" | head -n 1); printf '{\"outcome\":\"pending\",\"recheck_at\":%s}' \"${r:-null}\"; fi\n").unwrap();
        let (id, run) = room.launch();
        room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
        room.hook(
            &id,
            &run,
            json!({"hook_event_name":"UserPromptSubmit", "prompt":"work"}),
        );
        let reset = super::super::judges::capacity(&room.env, 100);
        let mut payload = json!({"hook_event_name":"StopFailure", "error":"rate_limit", "last_assistant_message":"Try later"});
        if child {
            let snapshot = room.env.store().snapshot_cached().unwrap();
            let root = snapshot
                .agents
                .iter()
                .find(|row| row.name == run.agent_name)
                .unwrap();
            seed(
                &room,
                "limited-child",
                76,
                LaunchParams {
                    parent_agent_id: Some(
                        root.launch_id.as_ref().unwrap_or(&root.agent_id).clone(),
                    ),
                    parent_agent_kind: Some(root.kind.clone()),
                    launch_depth: Some(1),
                    ..Default::default()
                },
                LifecycleSignal::Registered,
            );
            seed(
                &room,
                "limited-child",
                76,
                LaunchParams::default(),
                LifecycleSignal::TurnStarted { turn_id: None },
            );
            payload["session_id"] = json!("limited-child");
            payload["cwd"] = json!(room.env.project_root);
            let mut cmd = room.hook_command("claude");
            cmd.env("ZELLIJ", "1")
                .env("ZELLIJ_PANE_ID", "76")
                .env("RIMZ_AGENT_NAME", "limited-child");
            success(
                room.env
                    .spawn_payload(cmd, &payload.to_string())
                    .wait_with_output()
                    .unwrap(),
            );
            room.env.drain_hooks();
            let snapshot = room.env.store().snapshot_cached().unwrap();
            let source = snapshot
                .agents
                .iter()
                .find(|row| row.agent_id.as_str() == "limited-child")
                .unwrap();
            assert!(
                source.displayed_turn_error().is_some(),
                "child limit must be actionable: {source:#?}"
            );
            success(
                room.command(&["workflow", "advance", &id])
                    .output()
                    .unwrap(),
            );
        } else {
            room.hook(&id, &run, payload);
            let snapshot = room.env.store().snapshot_cached().unwrap();
            if let Some(source) = snapshot
                .agents
                .iter()
                .find(|row| row.name == run.agent_name)
            {
                assert!(
                    source.displayed_turn_error().is_some(),
                    "root limit must be actionable: {source:#?}"
                );
            }
        }
        let value = until(&room.env, &id, |value| {
            value["record"]["wait"]["outcome"] == "pending"
        });
        assert_eq!(value["record"]["prev_attempt"]["outcome"], "limited");
        assert_eq!(
            value["record"]["prev_attempt"]["children"]["stopped"]
                .as_array()
                .unwrap()
                .len(),
            usize::from(child)
        );
        assert_eq!(
            value["record"]["prev_attempt"]["limited"]["reset_at"],
            reset.to_string()
        );
        assert!(
            success(room.command(&["workflow", "show", &id]).output().unwrap())
                .contains(&reset.to_string())
        );
        let count = events(&value, "evaluation").len();
        success(
            room.command(&["workflow", "advance", &id])
                .output()
                .unwrap(),
        );
        assert_eq!(events(&show(&room.env, &id), "evaluation").len(), count);
        success(room.command(&["workflow", "cancel", &id]).output().unwrap());
    }
}
