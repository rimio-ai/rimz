use super::*;

pub(super) fn wait_for_pane_close(room: &Room, child: &mut std::process::Child) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if fs::read_to_string(room.env.project_root.join("spawn.log"))
            .unwrap()
            .contains("close-pane")
        {
            return;
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "restart exited before closing"
        );
        assert!(Instant::now() < deadline, "restart never closed the pane");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Report the `@break` agent's launch, as its provider would. A restart or resume detaches the
/// debugger and leaves it running, so its launch keeps its machine turn until it reports. It
/// reports from a pane of its own, beside the kept previous card's pane 61.
fn report_break_launch(room: &Room, id: &str) {
    let run = room.attempt(id);
    room.list_pane(62, run.kind.as_str());
    let payload = json!({
        "hook_event_name":"SessionStart", "session_id":"break-session",
        "cwd":room.env.project_root
    });
    let mut cmd = room.hook_command(run.kind.as_str());
    cmd.env(
        "RIMZ_AGENT_PID",
        room.owner(run.run_id.as_str()).to_string(),
    )
    .env("RIMZ_RUN_ID", run.run_id.as_str())
    .env("RIMZ_AGENT_NAME", run.agent_name.as_deref().unwrap())
    .env("RIMZ_WORKFLOW_RUN", id)
    .env("ZELLIJ", "1")
    .env("ZELLIJ_PANE_ID", "62")
    .env(
        "RIMZ_TEST_ZELLIJ_LIST_PANES",
        r#"[{"id":62,"is_plugin":false,"tab_id":1,"title":"sh"}]"#,
    );
    success(
        room.env
            .spawn_payload(cmd, &payload.to_string())
            .wait_with_output()
            .unwrap(),
    );
    room.env.drain_hooks();
}

#[test]
fn agents_restart_clears_a_former_steps_pin_and_stays_outside_the_run() {
    let room = Room::new("");
    let (id, run) = room.launch();
    let transcript = room.env.project_root.join("session.jsonl");
    fs::write(&transcript, "{}\n").unwrap();
    room.hook(
        &id,
        &run,
        json!({"hook_event_name":"SessionStart", "transcript_path":transcript}),
    );
    fs::write(
        room.env.project_root.join("panes.json"),
        r#"[{"id":61,"is_plugin":false,"tab_id":1,"title":"codex","pane_command":"codex"}]"#,
    )
    .unwrap();
    let state = room.env.state_path_for(&room.env.project_root);
    let paths = state.workflow(&id.parse().unwrap());
    let mut record = records::load(&paths).unwrap();
    record.state = records::RunState::Done;
    {
        let guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
        records::write(&paths, &record, &guard).unwrap();
    }
    let reference = format!("@{}", run.agent_name.as_deref().unwrap());
    let receipt = success(
        room.command(&["agents", "restart", &reference])
            .output()
            .unwrap(),
    );
    let snapshot = room.env.store().snapshot().unwrap();
    assert!(
        snapshot.agents.iter().all(|agent| agent.workflow.is_none()),
        "{snapshot:#?}"
    );
    assert!(
        receipt.contains(&format!("{reference} is now outside run `{id}`"))
            && !receipt.contains("via rimz workflow restart"),
        "{receipt}"
    );
}

#[test]
fn cancel_during_restart_keeps_the_terminal_history_copy() {
    use std::io::Write as _;
    use std::os::unix::net::UnixListener;

    let room = Room::new("");
    let (id, run) = room.launch();
    let mut owner = dummy_agent_process();
    let transcript = room.env.project_root.join("session.jsonl");
    fs::write(&transcript, "{}\n").unwrap();
    room.hook_with_owner(
        &id,
        &run,
        owner.id(),
        json!({"hook_event_name":"SessionStart", "transcript_path":transcript}),
    );
    fs::write(
        room.env.project_root.join("panes.json"),
        r#"[{"id":61,"is_plugin":false,"tab_id":1,"title":"codex","pane_command":"codex"}]"#,
    )
    .unwrap();
    #[cfg(target_os = "linux")]
    super::rest::wait_for_advances_to_exit(&room.env, &id);
    let state = room.env.state_path_for(&room.env.project_root);
    let paths = state.workflow(&id.parse().unwrap());
    let history = super::super::memory::history_path(&room.env, &id);
    let mut cancel = room.command(&["workflow", "cancel", &id]);
    let socket = room.env.runtime_root.join("restart-stop.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut restart = room
        .command(&["workflow", "restart", &id])
        .env("RIMZ_TEST_WORKFLOW_STOP_GATE", &socket)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut stopping = loop {
        if let Ok((stream, _)) = listener.accept() {
            break stream;
        }
        assert!(restart.try_wait().unwrap().is_none(), "restart exited");
        assert!(Instant::now() < deadline, "restart never reached its stop");
        std::thread::sleep(Duration::from_millis(10));
    };
    let held = records::load(&paths).unwrap();
    let attempt = held.attempt.as_ref().unwrap();
    assert!(matches!(
        &attempt.ending.as_ref().unwrap().cause,
        records::EndCause::Restart { continued: false }
    ));
    assert_eq!(attempt.host.pid, restart.id());
    assert!(rimz::proc::process_is_live(
        attempt.host.pid,
        attempt.host.start.as_deref()
    ));
    success(cancel.output().unwrap());
    let pending = records::load(&paths).unwrap();
    assert_eq!(
        pending.state,
        records::RunState::Active,
        "cancel must land during the stop: {pending:#?}"
    );
    let pending = pending.attempt.as_ref().unwrap();
    assert_eq!(pending.host, attempt.host);
    assert_eq!(
        pending.ending.as_ref().unwrap().cause,
        records::EndCause::Cancel
    );
    assert!(!history.exists(), "cancel must leave the copy to restart");
    owner.kill().unwrap();
    owner.wait().unwrap();
    stopping.write_all(&[1]).unwrap();
    let receipt = success(restart.wait_with_output().unwrap());
    assert!(
        receipt.contains("run is canceled; no live replacement"),
        "{receipt}"
    );
    super::super::memory::assert_history_copy(&room.env, &id);
}

#[test]
fn agents_restart_refuses_a_current_judge_without_clearing_its_pin() {
    let room = Room::new("");
    let (id, run) = room.launch();
    let transcript = room.env.project_root.join("session.jsonl");
    fs::write(&transcript, "{}\n").unwrap();
    room.hook(
        &id,
        &run,
        json!({"hook_event_name":"SessionStart", "transcript_path":transcript}),
    );
    fs::write(
        room.env.project_root.join("panes.json"),
        r#"[{"id":61,"is_plugin":false,"tab_id":1,"title":"codex","pane_command":"codex"}]"#,
    )
    .unwrap();
    let store = room.env.store();
    let snapshot = store.snapshot().unwrap();
    let card = snapshot
        .agents
        .iter()
        .find(|agent| agent.name == run.agent_name)
        .unwrap();
    let mut pin = card.workflow.clone().unwrap();
    pin.role = rimz::agents::WorkflowAttemptRole::Judge;
    let mut observation = rimz::agents::AgentLifecycleObservation::new(
        Some(card.agent_id.clone()),
        rimz::agents::LifecycleSignal::Registered,
    );
    observation.launch.workflow = Some(pin.clone());
    let workspace = room.env.resolve_workspace(&room.env.project_root);
    store
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            workspace.workspace_id,
            workspace.session_name,
            run.kind.as_str(),
            "test",
            &observation,
        ))
        .unwrap();
    let state = store.paths();
    let paths = state.workflow(&id.parse().unwrap());
    let record = {
        let guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
        let mut record = records::load(&paths).unwrap();
        let attempt = record.attempt.as_mut().unwrap();
        attempt.kind = records::AttemptKind::Judge;
        // A judge's evaluator stays alive, unlike the finished step-launch host.
        attempt.host = records::Host {
            pid: std::process::id(),
            start: rimz::proc::process_start_token(std::process::id()),
        };
        records::write(&paths, &record, &guard).unwrap();
        record
    };
    success(
        room.command(&["workflow", "advance", &id])
            .output()
            .unwrap(),
    );
    let reference = format!("@{}", run.agent_name.as_deref().unwrap());
    let output = room
        .command(&["agents", "restart", &reference])
        .output()
        .unwrap();
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success()
            && error.contains(&format!(
                "agent is still owned by run `{id}`, judging check {}; wait for its answer or \
                 cancel the run with `rimz workflow cancel {id}`",
                record.attempt.as_ref().unwrap().node
            ))
            && !error.contains("workflow restart"),
        "{output:?}"
    );
    let current = store.snapshot().unwrap();
    assert_eq!(
        current
            .agents
            .iter()
            .find(|agent| agent.agent_id == card.agent_id)
            .unwrap()
            .workflow,
        Some(pin)
    );
    assert_eq!(records::load(&paths).unwrap(), record);
    let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
    assert!(!trace.contains("close-pane"), "{trace}");
}

#[test]
fn agents_restart_keeps_the_pin_of_an_unreadable_run_but_drops_one_whose_run_is_gone() {
    let mut actual = Vec::new();
    let mut outputs = Vec::new();
    for missing in [false, true] {
        let room = Room::new("");
        let (id, run) = room.launch();
        let transcript = room.env.project_root.join("session.jsonl");
        fs::write(&transcript, "{}\n").unwrap();
        room.hook(
            &id,
            &run,
            json!({"hook_event_name":"SessionStart", "transcript_path":transcript}),
        );
        fs::write(
            room.env.project_root.join("panes.json"),
            r#"[{"id":61,"is_plugin":false,"tab_id":1,"title":"codex","pane_command":"codex"}]"#,
        )
        .unwrap();
        let store = room.env.store();
        let snapshot = store.snapshot().unwrap();
        let card = snapshot
            .agents
            .iter()
            .find(|agent| agent.name == run.agent_name)
            .unwrap();
        let paths = store.paths().workflow(&id.parse().unwrap());
        if missing {
            fs::remove_file(&paths.record).unwrap();
        } else {
            fs::write(&paths.record, "invalid JSON").unwrap();
        }
        let reference = format!("@{}", run.agent_name.as_deref().unwrap());
        let output = room
            .command(&["agents", "restart", &reference])
            .output()
            .unwrap();
        let receipt = String::from_utf8_lossy(&output.stdout);
        let warning = String::from_utf8_lossy(&output.stderr);
        let current = store.snapshot().unwrap();
        let unchanged = current
            .agents
            .iter()
            .find(|agent| agent.agent_id == card.agent_id)
            .is_some_and(|agent| agent.workflow == card.workflow);
        actual.push((
            output.status.success(),
            warning.contains(&format!("warning: cannot read run `{id}`"))
                && warning.contains("keeping its workflow pin"),
            receipt.contains("restarted"),
            receipt.contains(&format!("is now outside run `{id}`")),
            unchanged,
        ));
        outputs.push(output);
    }
    // A broken record keeps the pin with a warning; a run with no record left is over, so its
    // pin is a former one and is cleared with a receipt.
    assert_eq!(
        actual,
        vec![
            (true, true, true, false, true),
            (true, false, true, true, false)
        ],
        "{outputs:#?}"
    );
}

#[test]
fn fresh_restart_replaces_the_step_and_receipts_its_first_visit() {
    let room = Room::new("");
    let (id, old) = room.launch();
    let memory = super::super::memory::history_path(&room.env, &id);
    fs::write(&memory, "earlier history").unwrap();
    let receipt = success(
        room.command(&["workflow", "restart", &id])
            .output()
            .unwrap(),
    );
    let value = until(&room.env, &id, |value| {
        value["record"]["attempt"]["run"]["run_id"]
            .as_str()
            .is_some_and(|run| run != old.run_id.as_str())
    });
    assert_eq!(value["record"]["visits"]["build"], 1);
    assert_eq!(value["record"]["attempt"]["visit"], 1);
    let handle = value["record"]["attempt"]["run"]["handle"]
        .as_str()
        .unwrap();
    assert!(
        receipt.contains(handle)
            && receipt.contains("fresh")
            && receipt.contains("visits restarted at 1"),
        "{receipt}"
    );
    assert_eq!(events(&value, "restart").len(), 1, "{value:#}");
    let shown = success(room.command(&["workflow", "show", &id]).output().unwrap());
    assert!(
        shown.contains("◇ after"),
        "the then check has not been reached: {shown}"
    );
    assert!(!shown.contains("build → after"), "{shown}");
    let history = success(
        room.command(&["workflow", "show", &id, "--history"])
            .output()
            .unwrap(),
    );
    let rows: Vec<_> = history
        .lines()
        .filter_map(|line| line.split_once("#— ").map(|(_, row)| row))
        .collect();
    assert_eq!(
        rows,
        [
            "build all visits reset on restart",
            "build restart · fresh · visits restarted at 1",
        ],
        "{history}"
    );
    let replacement = room.attempt(&id);
    room.hook(&id, &replacement, json!({"hook_event_name":"SessionStart"}));
    let snapshot = room.env.store().snapshot().unwrap();
    let card = rimz::store::snapshot::find_agent(
        &snapshot.agents,
        &replacement.kind,
        &"workflow-session".into(),
    )
    .unwrap();
    assert_eq!(card.name.as_deref(), Some(handle.trim_start_matches('@')));
    assert_eq!(fs::read_to_string(&memory).unwrap(), "earlier history");
    room.hook(&id, &replacement, json!({"hook_event_name":"Stop"}));
    settled(&room.env, &id, "done");
    super::super::memory::assert_history_copy(&room.env, &id);
}

#[test]
fn continue_without_a_conversation_refuses_before_stopping() {
    let room = Room::new("");
    let (id, old) = room.launch();
    let output = room
        .command(&["workflow", "restart", &id, "--continue"])
        .output()
        .unwrap();
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success()
            && error.contains("no resumable session")
            && error.contains(&format!("rimz workflow restart {id}")),
        "{error}"
    );
    let value = show(&room.env, &id);
    assert_eq!(
        value["record"]["attempt"]["run"]["run_id"],
        old.run_id.as_str()
    );
    let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
    assert!(!trace.contains("close-pane"), "{trace}");
}

#[test]
fn agents_restart_routes_the_current_step_through_workflow_continue() {
    continued_restart(true);
}

#[test]
fn continued_restart_receipts_the_addressable_handle() {
    continued_restart(false);
}

#[test]
fn fresh_restart_from_a_cap_break_leaves_the_kept_previous_card_live_and_detached() {
    let room = Room::new("keep=true\n");
    on_break(&room);
    let path = room.env.rimz_home().join("workflows/review/workflow.toml");
    fs::write(
        &path,
        fs::read_to_string(&path)
            .unwrap()
            .replace("max-visits = 2", "max-visits = 1")
            .replace("on.done = '@done'", "on.done = 'build'"),
    )
    .unwrap();
    let (id, run) = room.launch();
    // Another checkout keeps ordinary occupancy from gating the fresh launch.
    let kept_checkout = room
        .env
        .project_root
        .parent()
        .unwrap()
        .join("kept-checkout");
    fs::create_dir_all(&kept_checkout).unwrap();
    room.hook(
        &id,
        &run,
        json!({"hook_event_name":"SessionStart", "worktree_path":kept_checkout}),
    );
    fs::write(
        room.env.project_root.join("panes.json"),
        r#"[{"id":61,"is_plugin":false,"tab_id":1,"title":"codex","pane_command":"codex"}]"#,
    )
    .unwrap();
    room.hook(
        &id,
        &run,
        json!({"hook_event_name":"Stop", "worktree_path":kept_checkout}),
    );
    let broken = until(&room.env, &id, |v| v["record"]["node"] == "@break");
    assert_eq!(broken["record"]["input"]["outcome"], "cap");
    assert_eq!(
        broken["record"]["prev_attempt"]["run_id"],
        run.run_id.as_str()
    );
    let snapshot = room.env.store().snapshot().unwrap();
    let kept = snapshot
        .agents
        .iter()
        .find(|agent| agent.name == run.agent_name)
        .unwrap();
    assert!(kept.pane.is_some(), "{kept:#?}");
    assert_eq!(kept.working_path(), kept_checkout.to_str());
    assert_eq!(
        kept.lane.as_ref().map(rimz::ids::Lane::path),
        room.env.project_root.to_str(),
        "the kept card's lane stays homed where it launched"
    );
    assert!(
        kept.ended_at.is_none() && kept.workflow.is_none(),
        "{kept:#?}"
    );
    report_break_launch(&room, &id);

    success(
        room.command(&["workflow", "restart", &id])
            .output()
            .unwrap(),
    );
    assert_ne!(room.attempt(&id).run_id, run.run_id);
    let current = room.env.store().snapshot().unwrap();
    let previous = current
        .agents
        .iter()
        .find(|agent| agent.agent_id == kept.agent_id);
    assert!(
        previous.is_some_and(|agent| agent.ended_at.is_none() && agent.workflow.is_none()),
        "a fresh restart must leave the kept previous card live and detached: {previous:#?}"
    );
    let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
    assert!(!trace.contains("close-pane"), "{trace}");
}

fn continued_restart(routed: bool) {
    let room = Room::new("");
    let (id, run) = room.launch();
    let transcript = room.env.project_root.join("session.jsonl");
    fs::write(&transcript, "{}\n").unwrap();
    let mut owner = dummy_agent_process();
    room.hook_with_owner(
        &id,
        &run,
        owner.id(),
        json!({"hook_event_name":"SessionStart", "transcript_path":transcript}),
    );
    fs::write(
        room.env.project_root.join("panes.json"),
        r#"[{"id":61,"is_plugin":false,"tab_id":1,"title":"codex","pane_command":"codex"}]"#,
    )
    .unwrap();
    let reference = format!("@{}", run.agent_name.as_deref().unwrap());
    if routed {
        // The step belongs to the workflow, which takes neither restart flag.
        for flag in [&["--account", "default"][..], &["--no-continue"][..]] {
            let output = room
                .command(&[&["agents", "restart", &reference][..], flag].concat())
                .output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                !output.status.success()
                    && stderr.contains(&format!(
                        "restart refused: this agent runs step build of workflow run {id}, \
                         which `--account` and `--no-continue` do not apply to; run `rimz \
                         workflow restart {id} --continue` instead"
                    )),
                "{stderr}"
            );
        }
    }
    let args = if routed {
        vec!["agents", "restart", &reference]
    } else {
        vec!["workflow", "restart", &id, "--continue"]
    };
    let mut child = room
        .command(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_pane_close(&room, &mut child);
    // The trace shim closes the pane but cannot terminate its owner.
    owner.kill().unwrap();
    owner.wait().unwrap();
    let receipt = success(child.wait_with_output().unwrap());
    assert!(
        (!routed || receipt.contains(&format!("via rimz workflow restart {id} --continue")))
            && receipt.contains("resumed")
            && receipt.contains("visits restarted at 1"),
        "{receipt}"
    );
    let value = until(&room.env, &id, |value| {
        value["record"]["attempt"]["seq"]
            .as_u64()
            .is_some_and(|seq| seq > 2)
            && value["record"]["attempt"]["run"].is_object()
    });
    assert_eq!(events(&value, "restart")[0]["continued"], true);
    let replacement = room.attempt(&id);
    room.hook(&id, &replacement, json!({"hook_event_name":"SessionStart"}));
    let snapshot = room.env.store().snapshot().unwrap();
    let card = rimz::store::snapshot::find_agent(
        &snapshot.agents,
        &replacement.kind,
        &"workflow-session".into(),
    )
    .unwrap();
    let expected = format!("@{}", card.name.as_deref().unwrap());
    assert_eq!(value["record"]["attempt"]["run"]["handle"], expected);
    assert!(
        receipt.contains(&format!("as {expected} · resumed")),
        "{receipt}"
    );
}

#[test]
fn continue_rechecks_the_history_pool_after_stopping() {
    let room = Room::new("");
    on_break(&room);
    success(
        room.command(&[
            "accounts",
            "add",
            "codex",
            "other",
            "--history",
            "standalone",
        ])
        .output()
        .unwrap(),
    );
    rimz::agents::trust_hook_config_for_test(
        &room
            .env
            .rimz_home()
            .join("accounts/codex/other/config.toml"),
        &[],
    )
    .unwrap();
    let (id, run) = room.launch();
    let transcript = room.env.project_root.join("session.jsonl");
    fs::write(&transcript, "{}\n").unwrap();
    let mut owner = dummy_agent_process();
    room.hook_with_owner(
        &id,
        &run,
        owner.id(),
        json!({"hook_event_name":"SessionStart", "transcript_path":transcript}),
    );
    fs::write(
        room.env.project_root.join("panes.json"),
        r#"[{"id":61,"is_plugin":false,"tab_id":1,"title":"codex","pane_command":"codex"}]"#,
    )
    .unwrap();
    let mut child = room
        .command(&["workflow", "restart", &id, "--continue"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_pane_close(&room, &mut child);
    room.env
        .store()
        .switch_room_login(
            &room.env.resolve_workspace(&room.env.project_root),
            &run.kind,
            &"other".parse().unwrap(),
        )
        .unwrap();
    owner.kill().unwrap();
    owner.wait().unwrap();
    success(child.wait_with_output().unwrap());
    let value = until(&room.env, &id, |value| {
        value["record"]["attempt"]["run"]["run_id"]
            .as_str()
            .is_some_and(|current| current != run.run_id.as_str())
            || value["record"]["state"] != "active"
    });
    assert!(
        events(&value, "attempt").iter().any(|event| {
            event["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("rimz accounts use codex default"))
        }),
        "a pool change during stop must refuse the resume: {value:#}"
    );
    let fix = format!("rimz workflow restart {id}");
    assert_eq!(value["record"]["node"], "@break", "{value:#}");
    assert!(
        events(&value, "attempt").iter().any(|event| {
            event["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains(&fix))
        }),
        "an after-stop refusal must name the fresh restart command: {value:#}"
    );
    // The receipt can precede settlement; read the remedy after the persisted break.
    let shown = success(
        room.command(&["workflow", "show", &id, "--history"])
            .output()
            .unwrap(),
    );
    assert!(shown.contains(&fix), "{shown}");
    report_break_launch(&room, &id);
    success(room.command(&["workflow", "resume", &id]).output().unwrap());
    let restarted = until(&room.env, &id, |v| {
        v["record"]["attempt"]["node"] == "build" && v["record"]["attempt"]["run"].is_object()
    });
    assert_ne!(
        restarted["record"]["attempt"]["run"]["run_id"],
        run.run_id.as_str()
    );
}

fn on_break(room: &Room) {
    let path = room.env.rimz_home().join("workflows/review/workflow.toml");
    fs::write(
        &path,
        fs::read_to_string(&path)
            .unwrap()
            .replace("on-break='@failed'", "on-break={agent='codex'}"),
    )
    .unwrap();
}
