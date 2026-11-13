use super::*;

#[test]
fn kept_companions_late_pane_bind_cannot_restore_the_workflow_pin() {
    let room = Room::new("keep=true\n");
    room.env.install_agent_hooks("claude");
    let path = room.env.rimz_home().join("workflows/review/workflow.toml");
    fs::write(
        &path,
        fs::read_to_string(&path)
            .unwrap()
            .replace("agent = 'codex'", "agent = 'codex,claude'"),
    )
    .unwrap();
    let (id, run) = room.launch();
    let store = room.env.store();
    let snapshot = store.snapshot().unwrap();
    let companion = snapshot
        .agents
        .iter()
        .find(|agent| agent.kind.as_str() == "claude")
        .unwrap();
    let identity = rimz::store::writer::AgentLaunchIdentity {
        kind: companion.kind.clone(),
        agent_id: companion.agent_id.clone(),
        name: companion.name.clone().unwrap(),
        name_explicit: false,
        launch: rimz::agents::LaunchParams {
            workflow: companion.workflow.clone(),
            ..Default::default()
        },
        run_id: None,
        prompt: None,
    };
    room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
    room.hook(&id, &run, json!({"hook_event_name":"Stop"}));
    settled(&room.env, &id, "done");
    let workspace = room.env.resolve_workspace(&room.env.project_root);
    store
        .bind_agent_launch(
            &identity,
            &workspace.session_name,
            &room.env.project_root,
            &rimz::PaneId::parse("zellij:terminal_62").unwrap(),
            None,
        )
        .unwrap();
    let cold_store = store.clone();
    let current = std::thread::spawn(move || cold_store.snapshot().unwrap())
        .join()
        .unwrap();
    let bound = current
        .agents
        .iter()
        .find(|agent| agent.agent_id == identity.agent_id)
        .unwrap();
    assert!(bound.pane.is_some());
    assert!(
        bound.workflow.is_none(),
        "late bind re-pinned a detached companion: {bound:#?}"
    );
}

#[test]
fn kept_turn_leader_and_companion_survive_settlement_without_workflow_pins() {
    let room = Room::new("keep=true\n");
    room.env.install_agent_hooks("claude");
    let path = room.env.rimz_home().join("workflows/review/workflow.toml");
    fs::write(
        &path,
        fs::read_to_string(&path)
            .unwrap()
            .replace("agent = 'codex'", "agent = 'codex,claude'"),
    )
    .unwrap();
    let (id, run) = room.launch();
    room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
    register_companion(&room);
    room.hook(&id, &run, json!({"hook_event_name":"Stop"}));
    let ended = settled(&room.env, &id, "done");
    assert_eq!(ended["record"]["prev_attempt"]["outcome"], "ended");
    let snapshot = room.env.store().snapshot_cached().unwrap();
    let cards: Vec<_> = snapshot
        .agents
        .iter()
        .filter(|agent| {
            agent.automation.as_ref().is_some_and(|automation| {
                matches!(automation, rimz::agents::Automation::Workflow { .. })
            })
        })
        .collect();
    assert_eq!(cards.len(), 2, "{snapshot:#?}");
    assert!(
        cards
            .iter()
            .all(|agent| agent.ended_at.is_none() && agent.workflow.is_none()),
        "{cards:#?}"
    );
    let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
    assert!(!trace.contains("close-pane"), "{trace}");
    for card in cards {
        let text = success(
            room.command(&[
                "agents",
                "show",
                &format!("@{}", card.name.as_deref().unwrap()),
            ])
            .output()
            .unwrap(),
        );
        assert!(!text.contains("Workflow:"), "{text}");
    }
}

fn register_companion(room: &Room) {
    room.list_pane(62, "claude");
    let store = room.env.store();
    let snapshot = store.snapshot().unwrap();
    let companion = snapshot
        .agents
        .iter()
        .find(|agent| agent.kind.as_str() == "claude")
        .unwrap();
    let mut observation = rimz::agents::AgentLifecycleObservation::new(
        Some("companion".into()),
        rimz::agents::LifecycleSignal::Registered,
    );
    observation.agent_name.clone_from(&companion.name);
    observation.pane_id = Some(rimz::PaneId::parse("zellij:terminal_62").unwrap());
    observation.runtime_owner = Some(rimz::pane::RuntimeOwner::new(
        rimz::pane::RuntimeOwnerKind::Agent,
        "companion",
        room.owner("companion"),
        rimz::proc::process_start_token(room.owner("companion")),
    ));
    let workspace = room.env.resolve_workspace(&room.env.project_root);
    store
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            workspace.workspace_id,
            workspace.session_name,
            "claude",
            "test",
            &observation,
        ))
        .unwrap();
    fs::write(
        room.env.project_root.join("panes.json"),
        serde_json::to_vec(&json!([
            {"id":61,"is_plugin":false,"tab_id":1,"title":"codex","pane_command":"codex"},
            {"id":62,"is_plugin":false,"tab_id":1,"title":"claude"},
        ]))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn cancel_after_pin_clears_still_stops_the_detached_layout() {
    for registered in [false, true] {
        let room = Room::new("keep=true\n");
        room.env.install_agent_hooks("claude");
        let path = room.env.rimz_home().join("workflows/review/workflow.toml");
        fs::write(
            &path,
            fs::read_to_string(&path)
                .unwrap()
                .replace("agent = 'codex'", "agent = 'codex,claude'"),
        )
        .unwrap();
        let (id, run) = room.launch();
        if registered {
            room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
        }
        register_companion(&room);
        let store = room.env.store();
        let snapshot = store.snapshot().unwrap();
        let workspace = room.env.resolve_workspace(&room.env.project_root);
        for card in snapshot.agents.iter() {
            store
                .clear_workflow_pin(
                    &card.kind,
                    &card.agent_id,
                    card.workflow.as_ref().unwrap(),
                    &workspace.session_name,
                )
                .unwrap();
        }
        success(room.command(&["workflow", "cancel", &id]).output().unwrap());
        settled(&room.env, &id, "canceled");
        let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
        let closed: Vec<_> = trace
            .lines()
            .filter(|line| line.contains("close-pane"))
            .filter_map(|line| line.split_whitespace().last())
            .collect();
        assert!(
            closed.contains(&"terminal_62") && (!registered || closed.contains(&"terminal_61")),
            "registered={registered}: {trace}"
        );
    }
}

#[test]
fn kept_layout_disables_supervised_wrapper_cleanup() {
    let room = Room::new("keep=true\n");
    let node = success(
        room.command(&["workflow", "show", "review", "build"])
            .output()
            .unwrap(),
    );
    let shows_keep = node
        .lines()
        .any(|line| line.trim_start().starts_with("keep:") && line.trim_end().ends_with("true"));
    assert!(shows_keep, "{node}");
    let (id, _) = room.launch();
    let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
    let rest = trace.split("\"--request-file\" ").nth(1).unwrap();
    let path = serde_json::Deserializer::from_str(rest)
        .into_iter::<String>()
        .next()
        .unwrap()
        .unwrap();
    let payload = fs::read_to_string(path).unwrap();
    let request = rimz::harness::launch::decode_exec_request("codex", None, &payload).unwrap();
    assert!(
        !request.exit_on_run_completion && !request.close_pane_on_exit,
        "{request:#?}"
    );
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
}

#[test]
fn clearing_a_workflow_pin_is_atomic_idempotent_and_survives_a_cold_fold() {
    let room = Room::new("");
    let (id, run) = room.launch();
    let store = room.env.store();
    let snapshot = store.snapshot_cached().unwrap();
    let card = snapshot
        .agents
        .iter()
        .find(|agent| agent.name == run.agent_name)
        .unwrap();
    let pin = card.workflow.as_ref().unwrap();
    let workspace = room.env.resolve_workspace(&room.env.project_root);
    let mut other = pin.clone();
    other.seq += 1;
    let results = [
        store
            .clear_workflow_pin(&card.kind, &card.agent_id, &other, &workspace.session_name)
            .unwrap(),
        store
            .clear_workflow_pin(&card.kind, &card.agent_id, pin, &workspace.session_name)
            .unwrap(),
        store
            .clear_workflow_pin(&card.kind, &card.agent_id, pin, &workspace.session_name)
            .unwrap(),
    ];
    assert_eq!(results, [false, true, false]);
    let cold_store = store.clone();
    let snapshot = std::thread::spawn(move || cold_store.snapshot().unwrap())
        .join()
        .unwrap();
    assert!(
        snapshot
            .agents
            .iter()
            .find(|agent| agent.agent_id == card.agent_id)
            .unwrap()
            .workflow
            .is_none()
    );
    let clears = store
        .read_events()
        .unwrap()
        .into_iter()
        .filter(|event| event.method == "agent.workflow_cleared")
        .count();
    assert_eq!(clears, 1);
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
}

#[test]
fn kept_settlement_reports_the_detached_companion_live() {
    let room = Room::new("keep=true\n");
    room.env.install_agent_hooks("claude");
    let path = room.env.rimz_home().join("workflows/review/workflow.toml");
    fs::write(
        &path,
        fs::read_to_string(&path)
            .unwrap()
            .replace("agent = 'codex'", "agent = 'codex,claude'"),
    )
    .unwrap();
    let (id, run) = room.launch();
    room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
    register_companion(&room);
    room.hook(&id, &run, json!({"hook_event_name":"Stop"}));
    let ended = settled(&room.env, &id, "done");
    let children = &ended["record"]["prev_attempt"]["children"];
    assert_eq!(children["stopped"], json!([]), "{children:#}");
    let live = children["live"].as_array().unwrap();
    assert_eq!(live.len(), 1, "{children:#}");
    assert_eq!(live[0]["kind"], "claude", "{children:#}");
    assert_ne!(
        live[0]["handle"],
        json!(format!("@{}", run.agent_name.as_deref().unwrap())),
        "the leader is never its own child: {children:#}"
    );
}
