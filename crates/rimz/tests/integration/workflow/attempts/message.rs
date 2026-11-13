use super::*;

use rimz::store::message::{DeliveryGate, HarnessNotice, MessageSender};

fn tell(room: &Room, target: &str) -> String {
    tell_with(room, target, "")
}

fn tell_with(room: &Room, target: &str, extra: &str) -> String {
    let dir = room.env.rimz_home().join("workflows/tell");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("workflow.toml"),
        format!(
            "[workflow]\non-break='@failed'\nscope='worktree'\nstart='tell'\n[message.tell]\nmessage='{target}'\nprompt-file='prompt.md'\nthen='@done'\n{extra}"
        ),
    )
    .unwrap();
    fs::write(dir.join("prompt.md"), "Plan the fix").unwrap();
    let receipt = success(
        room.command(&["workflow", "run", "tell", "--prompt", "Ship it"])
            .output()
            .unwrap(),
    );
    receipt.lines().next().unwrap().to_owned()
}

fn leader(room: &Room, busy: bool) -> (rimz::ids::AgentSessionId, String, RunRecord) {
    let (id, run) = room.launch();
    room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
    if busy {
        room.hook(
            &id,
            &run,
            json!({"hook_event_name":"UserPromptSubmit", "prompt":"working"}),
        );
    }
    let agent = rimz::harness::run::load(
        &room.env.state_path_for(&room.env.project_root),
        &run.run_id,
    )
    .unwrap()
    .agent_id
    .expect("the leader registered");
    (agent, id, run)
}

fn notice(room: &Room, id: &str) -> (Value, rimz::store::message::MessageRecord) {
    let value = until(&room.env, id, |value| {
        value["record"]["attempt"].is_null() && value["record"]["state"] != "active"
    });
    let message: rimz::ids::MessageId = events(&value, "attempt")[0]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("the attempt row names its notice: {value:#}"))
        .parse()
        .unwrap();
    let store = room.env.store();
    let record = store
        .list_messages()
        .unwrap()
        .into_iter()
        .chain(store.list_message_history().unwrap())
        .find(|row| row.message_id == message)
        .unwrap_or_else(|| panic!("notice {message} not recorded: {value:#}"));
    (value, record)
}

#[test]
fn a_message_step_queues_one_wait_notice_for_a_busy_target_and_moves_on() {
    let room = Room::new("lifecycle='session'\n");
    let (agent, ..) = leader(&room, true);
    let id = tell(&room, "@codex");
    let (value, record) = notice(&room, &id);
    assert_eq!(value["record"]["state"], "done", "{value:#}");
    let attempts = events(&value, "attempt");
    assert_eq!(attempts.len(), 1, "{value:#}");
    assert_eq!(
        (&attempts[0]["outcome"], &attempts[0]["handle"]),
        (&json!("ended"), &json!("@codex#main")),
        "{value:#}"
    );
    assert_eq!(
        (
            &record.agent_id,
            &record.sender,
            record.automated,
            record.gate
        ),
        (
            &agent,
            &MessageSender::Harness {
                notice: HarnessNotice::Wait
            },
            true,
            DeliveryGate::Done
        )
    );
    assert!(
        record.text.starts_with(&format!(
            "Workflow `tell` step `tell` sends you this message (run {id}; `rimz workflow show {id}`).\n\nPlan the fix"
        )),
        "{}",
        record.text
    );
    assert_eq!(
        (record.status, record.last_sent_at),
        (rimz::store::message::MessageStatus::Queued, None),
        "{record:?}"
    );
}

#[test]
fn a_message_step_to_no_agent_follows_undelivered() {
    let room = Room::new("lifecycle='session'\n");
    let id = tell(&room, "@nobody");
    let value = until(&room.env, &id, |value| value["record"]["state"] == "failed");
    let attempts = events(&value, "attempt");
    assert_eq!(attempts.len(), 1, "{value:#}");
    assert_eq!(attempts[0]["outcome"], "died", "{value:#}");
    let routes = events(&value, "route");
    assert!(
        routes.iter().any(|route| route["outcome"] == "undelivered"
            && route["input"]["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("@nobody"))),
        "{value:#}"
    );
    assert!(room.env.store().list_messages().unwrap().is_empty());
}

#[test]
fn a_pinned_message_step_delivers_only_to_its_session() {
    let room = Room::new("lifecycle='session'\n");
    let (agent, ..) = leader(&room, true);
    let id = tell_with(&room, "@codex", &format!("session='{agent}'\n"));
    let (value, record) = notice(&room, &id);
    assert_eq!(value["record"]["state"], "done", "{value:#}");
    assert_eq!(record.agent_id, agent);

    let id = tell_with(&room, "@codex", "session='ses-elsewhere'\n");
    let value = until(&room.env, &id, |value| value["record"]["state"] == "failed");
    let routes = events(&value, "route");
    assert!(
        routes.iter().any(|route| route["outcome"] == "undelivered"
            && route["input"]["reason"]
                == json!(format!(
                    "@codex is session {agent}, not the pinned session ses-elsewhere"
                ))),
        "{value:#}"
    );
}

#[test]
fn a_message_step_attempts_a_resting_target_now() {
    let room = Room::new("lifecycle='session'\n");
    leader(&room, false);
    let id = tell(&room, "@codex");
    let (value, record) = notice(&room, &id);
    assert_eq!(value["record"]["state"], "done", "{value:#}");
    assert_eq!(
        (
            record.status,
            record.attempts,
            record.last_sent_at.is_some()
        ),
        (rimz::store::message::MessageStatus::Sent, 1, true),
        "{record:?}"
    );
}

/// Register a second codex agent in the leader's lane, from another checkout.
fn rival(room: &Room) {
    let env = &room.env;
    let workspace = env.resolve_workspace(&env.project_root);
    let checkout = env.project_root.join("rival");
    fs::create_dir_all(&checkout).unwrap();
    let kind = rimz::ids::AgentKind::new_unchecked("codex");
    env.store()
        .append_event(&rimz::store::event::EventEnvelope::agent_launched(
            workspace.workspace_id,
            &workspace.session_name,
            &kind,
            rimz::store::event::AgentLaunchPayload {
                system_prompt_digest: None,
                agent_id: "launch_rival".into(),
                launch_id: Some("launch_rival".into()),
                agent_name: "rival".into(),
                agent_name_explicit: true,
                launch: rimz::agents::LaunchParams::default(),
                state: rimz::store::event::AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: Some(rimz::pane::RuntimeOwner::new(
                    rimz::pane::RuntimeOwnerKind::Agent,
                    "rival",
                    env.agent_owner_pid(),
                    None,
                )),
                worktree_path: Some(checkout.display().to_string()),
                worktree_branch: Some("main".into()),
                prompt: None,
                description: None,
            },
        ))
        .unwrap();
    let mut registered = rimz::agents::AgentLifecycleObservation::new(
        Some("sess-rival".into()),
        rimz::agents::LifecycleSignal::Registered,
    );
    registered.agent_name = Some("rival".into());
    env.store()
        .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
            session_name: &workspace.session_name,
            agent_kind: kind,
            event_name: "test",
            observation: &registered,
            spawned_subagents: &[],
        })
        .unwrap();
    let snapshot = env.store().snapshot_cached().unwrap();
    assert!(
        rimz::address::addressable_agents(&snapshot)
            .iter()
            .any(|agent| agent.name.as_deref() == Some("rival")),
        "{:#?}",
        snapshot.agents
    );
}

/// Start `tell` toward a busy leader and hold its host at the result gate, its notice queued.
fn held(
    room: &Room,
) -> (
    std::process::Child,
    std::os::unix::net::UnixStream,
    String,
    rimz::store::message::MessageRecord,
) {
    use std::os::unix::net::UnixListener;
    use std::time::{Duration, Instant};

    let socket = room.env.runtime_root.join("message-result.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let dir = room.env.rimz_home().join("workflows/tell");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("workflow.toml"),
        "[workflow]\non-break='@failed'\nscope='worktree'\nstart='tell'\n[message.tell]\nmessage='@codex'\nthen='@done'\n",
    )
    .unwrap();
    let run = room
        .command(&["workflow", "run", "tell"])
        .env("RIMZ_TEST_WORKFLOW_RESULT_GATE", &socket)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let gate = loop {
        if let Ok((stream, _)) = listener.accept() {
            break stream;
        }
        assert!(
            Instant::now() < deadline,
            "message host never reached its result boundary"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let held = room.env.store().list_messages().unwrap();
    let [queued] = held.as_slice() else {
        panic!("one notice queued before the hold: {held:?}");
    };
    let id = rimz::store::workflow::list(&room.env.state_path_for(&room.env.project_root))
        .unwrap()
        .into_iter()
        .find(|record| record.workflow == "tell")
        .unwrap()
        .id
        .to_string();
    (run, gate, id, queued.clone())
}

#[test]
fn a_lost_message_host_resends_the_same_notice_once() {
    use std::time::{Duration, Instant};

    let room = Room::new("lifecycle='session'\n");
    let (agent, ..) = leader(&room, true);
    let (mut run, gate, id, queued) = held(&room);
    let deadline = Instant::now() + Duration::from_secs(30);
    let record = show(&room.env, &id)["record"].clone();
    assert_eq!(
        record["attempt"]["message"],
        queued.message_id.as_str(),
        "{record:#}"
    );
    let host = record["attempt"]["host"]["pid"].as_u64().unwrap() as u32;
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(host as i32),
        nix::sys::signal::Signal::SIGKILL,
    )
    .unwrap();
    while rimz::proc::process_is_live(host, None) {
        assert!(Instant::now() < deadline, "host {host} outlived SIGKILL");
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(gate);
    let _ = run.wait();
    // The retry keeps the queued notice's target even once the address names two agents.
    rival(&room);
    success(
        room.command(&["workflow", "advance", &id])
            .output()
            .unwrap(),
    );
    let value = until(&room.env, &id, |value| value["record"]["state"] != "active");
    assert_eq!(value["record"]["state"], "done", "{value:#}");
    let attempts = events(&value, "attempt");
    assert_eq!(attempts.len(), 1, "{value:#}");
    assert_eq!(
        (&attempts[0]["outcome"], &attempts[0]["handle"]),
        (&json!("ended"), &json!("@codex#main")),
        "{value:#}"
    );
    assert_eq!(
        attempts[0]["message"],
        queued.message_id.as_str(),
        "{value:#}"
    );
    assert_eq!(value["record"]["visits"]["tell"], 1, "{value:#}");
    let notices = room
        .env
        .store()
        .list_messages()
        .unwrap()
        .into_iter()
        .filter(|row| row.agent_id == agent)
        .collect::<Vec<_>>();
    assert_eq!(
        notices
            .iter()
            .map(|row| &row.message_id)
            .collect::<Vec<_>>(),
        [&queued.message_id],
        "the retry re-sends under the same id, which the queue keeps once"
    );
}

#[test]
fn a_cancel_while_the_host_is_held_retires_the_queued_notice() {
    let room = Room::new("lifecycle='session'\n");
    leader(&room, true);
    let (mut run, mut gate, id, queued) = held(&room);
    let host = show(&room.env, &id)["record"]["attempt"]["host"]["pid"]
        .as_u64()
        .unwrap() as u32;
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
    std::io::Write::write_all(&mut gate, &[1]).unwrap();
    let _ = run.wait();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while rimz::proc::process_is_live(host, None) {
        assert!(
            std::time::Instant::now() < deadline,
            "host {host} never relocked"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let value = show(&room.env, &id);
    assert_eq!(value["record"]["state"], "canceled", "{value:#}");
    let store = room.env.store();
    let pending = store.list_messages().unwrap();
    assert!(pending.is_empty(), "{pending:#?}\n{value:#}");
    let retired = store
        .list_message_history()
        .unwrap()
        .into_iter()
        .find(|row| row.message_id == queued.message_id)
        .expect("the notice is retired into history");
    assert_eq!(
        retired.status,
        rimz::store::message::MessageStatus::Canceled,
        "{retired:?}"
    );
}

#[test]
fn node_view_and_definition_rail_name_a_message_step_target() {
    let room = Room::new("lifecycle='session'\n");
    let dir = room.env.rimz_home().join("workflows/tell");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("workflow.toml"),
        "[workflow]\non-break='@failed'\nscope='worktree'\nstart='tell'\n[message.tell]\nmessage='@planner#feat-x'\nthen='@done'\n",
    )
    .unwrap();
    let show = |args: &[&str]| success(room.command(args).env("NO_COLOR", "1").output().unwrap());
    let node = show(&["workflow", "show", "tell", "tell"]);
    assert!(
        node.lines()
            .next()
            .is_some_and(|line| line == "tell · step in tell · message"),
        "{node}"
    );
    assert_eq!(
        node.lines()
            .filter(|line| line.contains("message") && line.contains("@planner#feat-x"))
            .count(),
        1,
        "{node}"
    );
    assert!(node.contains("undelivered → @failed"), "{node}");
    let rail = show(&["workflow", "show", "tell"]);
    assert!(rail.contains("step  @planner#feat-x"), "{rail}");
}
