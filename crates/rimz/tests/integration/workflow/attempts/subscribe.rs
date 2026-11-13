use super::*;

fn subscribed(entry: &str, scope: &str) -> (Room, String) {
    let room = Room::new(&format!("lifecycle='session'\nsubscribe=[{entry}]\n"));
    let path = room.env.rimz_home().join("workflows/review/workflow.toml");
    let spec = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        spec.replace("scope = 'worktree'", &format!("scope = '{scope}'")),
    )
    .unwrap();
    let (id, _) = room.launch();
    (room, id)
}

fn wake(room: &Room, name: &str, payload: Value) {
    let signal = json!({"name":name, "payload":payload, "source":"forge"});
    success(
        room.command(&["workflow", "advance", "--wake-signal", &signal.to_string()])
            .output()
            .unwrap(),
    );
}

fn assert_unforwarded(room: &Room, id: &str, name: &str, payload: Value) {
    let before = show(&room.env, id);
    wake(room, name, payload.clone());
    let after = show(&room.env, id);
    assert_eq!(after["journal"], before["journal"], "{name} {payload}");
    assert_eq!(after["record"], before["record"], "{name} {payload}");
}

fn assert_forwarded(room: &Room, id: &str, name: &str, payload: Value) {
    let before = show(&room.env, id);
    wake(room, name, payload);
    let after = show(&room.env, id);
    let signals = events(&after, "signal");
    assert_eq!(
        signals.len(),
        events(&before, "signal").len() + 1,
        "{after}"
    );
    assert_eq!(signals.last().unwrap()["signal"], name);
    assert_eq!(signals.last().unwrap()["node"], "build");
    assert_eq!(
        after["record"], before["record"],
        "a step-only match changes no run state"
    );
}

#[test]
fn step_forge_subscriptions_filter_checkout_and_ignore_sibling_skips() {
    for (name, sibling) in [("ci.failed", "ci.passed"), ("pr.opened", "pr.closed")] {
        for scope in ["worktree", "project"] {
            let (room, id) = subscribed(&format!("'{name}'"), scope);
            let root = &room.env.project_root;
            assert_unforwarded(&room, &id, name, json!({"path":"/another/checkout"}));
            assert_unforwarded(&room, &id, name, json!({}));
            assert_unforwarded(&room, &id, sibling, json!({"path":root}));
            assert_forwarded(&room, &id, name, json!({"path":root}));
        }
    }
}

#[test]
fn step_stage_subscriptions_follow_the_current_archive_link() {
    use std::os::unix::fs::symlink;
    let room = Room::new("lifecycle='session'\nsubscribe=['team.stage']\n");
    let archive = room
        .env
        .state_path_for(&room.env.project_root)
        .team_notes_dir;
    let first = archive.join("first");
    let second = archive.join("second");
    for dir in [&first, &second] {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("blackboard.md"), "# Board\n\nStage: Plan\n").unwrap();
    }
    let first_board = first.canonicalize().unwrap().join("blackboard.md");
    let second_board = second.canonicalize().unwrap().join("blackboard.md");
    let link = room.env.project_root.join(".rimz/team");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    symlink(&first, &link).unwrap();
    let (id, _) = room.launch();
    assert_unforwarded(&room, &id, "team.stage", json!({"board":second_board}));
    assert_unforwarded(&room, &id, "team.stage", json!({}));
    assert_forwarded(&room, &id, "team.stage", json!({"board":first_board}));
    fs::remove_file(&link).unwrap();
    symlink(&second, &link).unwrap();
    assert_unforwarded(&room, &id, "team.stage", json!({"board":first_board}));
    assert_forwarded(&room, &id, "team.stage", json!({"board":second_board}));
}

#[test]
fn a_forwarded_signal_is_canceled_at_delivery_once_its_attempt_ends() {
    for ending in ["detach", "stop"] {
        let room = Room::new("lifecycle='session'\nsubscribe=['review.ready']\n");
        let (id, run) = room.launch();
        room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
        room.hook(
            &id,
            &run,
            json!({"hook_event_name":"UserPromptSubmit", "prompt":"working"}),
        );
        wake(&room, "review.ready", json!({}));
        let shown = show(&room.env, &id);
        let message: rimz::ids::MessageId = events(&shown, "signal")[0]["message"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let handle = shown["record"]["attempt"]["run"]["handle"]
            .as_str()
            .unwrap();
        let history = success(
            room.command(&["workflow", "show", &id, "--history"])
                .env("NO_COLOR", "1")
                .output()
                .unwrap(),
        );
        assert_eq!(
            history
                .lines()
                .filter(|line| line.contains(&format!("signal review.ready → {handle}")))
                .count(),
            1,
            "{history}"
        );
        let store = room.env.store();
        let queued = store
            .list_messages()
            .unwrap()
            .into_iter()
            .find(|row| row.message_id == message)
            .unwrap_or_else(|| panic!("{ending}: forward not queued: {shown:#}"));
        assert_eq!(
            (
                &queued.sender,
                queued.automated,
                queued.gate,
                queued.about_run.as_ref()
            ),
            (
                &rimz::store::message::MessageSender::Harness {
                    notice: rimz::store::message::HarnessNotice::Signal
                },
                true,
                rimz::store::message::DeliveryGate::Done,
                Some(&run.run_id)
            ),
            "{ending}"
        );
        assert!(queued.text.starts_with("review.ready\n"), "{}", queued.text);
        assert!(
            queued.text.contains(&format!("rimz workflow show {id}")),
            "{}",
            queued.text
        );
        match ending {
            "detach" => {
                rimz::harness::run::cancel_and_wake(&store, &run.run_id).unwrap();
            }
            _ => {
                success(room.command(&["workflow", "cancel", &id]).output().unwrap());
            }
        }
        success(
            room.command(&["message", "deliver", "--message-id", message.as_str()])
                .env("RIMZ_MESSAGE_SETTLE_MS", "0")
                .output()
                .unwrap(),
        );
        assert!(
            store
                .list_messages()
                .unwrap()
                .iter()
                .all(|row| row.message_id != message),
            "{ending}"
        );
        let history = store.list_message_history().unwrap();
        let row = history
            .iter()
            .find(|row| row.message_id == message)
            .unwrap();
        assert!(
            matches!(
                row.status,
                rimz::store::message::MessageStatus::Canceled
                    | rimz::store::message::MessageStatus::Archived
            ) && row.last_sent_at.is_none(),
            "{ending}: {row:?}"
        );
        let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
        assert!(!trace.contains("review.ready"), "{ending}: {trace}");
    }
}

#[test]
fn node_view_and_live_rail_list_a_session_subscriptions_with_their_scope() {
    let room = Room::new(
        "lifecycle='session'\nsubscribe=['ci.failed', {signal='deploy.done', match={env='prod'}}]\n",
    );
    let node = success(
        room.command(&["workflow", "show", "review", "build"])
            .env("NO_COLOR", "1")
            .output()
            .unwrap(),
    );
    let receives = node.lines().find(|line| line.contains("receives:"));
    assert!(
        receives.is_some_and(
            |line| line.contains("ci.failed for this checkout, deploy.done where env=prod")
        ),
        "{node}"
    );
    let (id, _) = room.launch();
    let rail = success(
        room.command(&["workflow", "show", &id])
            .env("NO_COLOR", "1")
            .output()
            .unwrap(),
    );
    let current = rail.lines().find(|line| line.contains("since "));
    assert!(
        current.is_some_and(|line| line.contains(" · receives ci.failed deploy.done")),
        "{rail}"
    );
}
