use super::*;
use rimz::disk::lock::WorkspaceLock;
use rimz::ids::WorkflowRunId;

fn pending(room: &Room, task: Option<&str>) -> records::RunRecord {
    let state = room.env.state_path_for(&room.env.project_root);
    let dir = room.env.rimz_home().join("workflows/review");
    let now = jiff::Timestamp::now();
    let record = serde_json::from_value(json!({
        "version":1, "id":WorkflowRunId::new(), "workflow":"review", "task":task,
        "spec_dir":dir, "spec_digest":"test", "prompt":"Build it",
        "scope":{"kind":"worktree", "path":room.env.project_root},
        "project_root":room.env.project_root, "created_at":now, "state":"active",
        "node":"build", "entered_at":now, "input":{"outcome":"go", "key":7},
        "key":7, "next_seq":1, "visits":{}, "visit_grant":0, "deadline_fired":false
    }))
    .unwrap();
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    records::initialize(
        &state,
        &record,
        &fs::read(dir.join("workflow.toml")).unwrap(),
        &guard,
    )
    .unwrap();
    record
}

fn advance(room: &Room, record: &records::RunRecord) -> Value {
    success(
        room.command(&["workflow", "advance", record.id.as_str()])
            .output()
            .unwrap(),
    );
    show(&room.env, record.id.as_str())
}

fn save(room: &Room, record: &Value) {
    let state = room.env.state_path_for(&room.env.project_root);
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    let record: records::RunRecord = serde_json::from_value(record.clone()).unwrap();
    records::write(&state.workflow(&record.id), &record, &guard).unwrap();
}

fn tickets(room: &Room) -> Vec<Value> {
    fs::read_dir(room.env.rimz_home().join("loops/throttle"))
        .into_iter()
        .flatten()
        .map(|entry| serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap())
        .collect()
}

#[test]
fn capacity_resolution_and_admission_errors_end_the_attempt_as_died() {
    let mut actual = Vec::new();
    for failure in ["profile", "queue"] {
        let room = Room::new("");
        if failure == "profile" {
            let profile = room.env.rimz_home().join("agents/capacity-worker.md");
            fs::create_dir_all(profile.parent().unwrap()).unwrap();
            fs::write(&profile, "---\nkind: codex\n---\n").unwrap();
            let spec = room.env.rimz_home().join("workflows/review/workflow.toml");
            fs::write(
                &spec,
                fs::read_to_string(&spec)
                    .unwrap()
                    .replace("agent = 'codex'", "agent = 'capacity-worker'"),
            )
            .unwrap();
        }
        let record = pending(&room, Some("nightly"));
        if failure == "profile" {
            fs::remove_file(room.env.rimz_home().join("agents/capacity-worker.md")).unwrap();
        } else {
            fs::write(
                room.env.rimz_home().join("loop.toml"),
                "[throttle]\nmax-active=1\n",
            )
            .unwrap();
            fs::create_dir_all(room.env.rimz_home().join("loops")).unwrap();
            fs::write(
                room.env.rimz_home().join("loops/throttle"),
                "not a directory",
            )
            .unwrap();
        }
        let _ = room
            .command(&["workflow", "advance", record.id.as_str()])
            .output()
            .unwrap();
        let value = show(&room.env, record.id.as_str());
        actual.push((
            value["record"]["state"] == "done",
            events(&value, "attempt")
                .iter()
                .any(|row| row["outcome"] == "died"),
        ));
    }
    assert_eq!(actual, [(true, true); 2]);
}

#[test]
fn capacity_manual_loop_workflows_launch_but_scheduled_workflows_wait() {
    let mut actual = Vec::new();
    for verb in ["fire", "run"] {
        let room = Room::new("");
        fs::write(
            room.env.rimz_home().join("loop.toml"),
            format!(
                concat!(
                    "[throttle]\npace='0s'\ncheck='exit 75'\n[tasks.nightly]\n",
                    "workflow='review'\nroot={:?}\nevery='1h'\n"
                ),
                room.env.project_root
            ),
        )
        .unwrap();
        success(room.command(&["loop", verb, "nightly"]).output().unwrap());
        let state = room.env.state_path_for(&room.env.project_root);
        let record = records::list(&state).unwrap().pop().unwrap();
        let value = until(&room.env, record.id.as_str(), |value| {
            value["record"]["visits"]["build"]["total"] == 1
                || value["record"]["wait"]["outcome"] == "capacity"
        });
        actual.push((
            value["record"]["visits"]["build"]["total"] == 1,
            value["record"]["manual"] == true,
            value["record"]["wait"]["outcome"] == "capacity",
        ));
        success(
            room.command(&["workflow", "cancel", record.id.as_str()])
                .output()
                .unwrap(),
        );
    }
    assert_eq!(actual, [(true, true, false), (false, false, true)]);
}

#[test]
fn capacity_workflow_wait_reaches_the_human_loop_list() {
    let room = Room::new("");
    fs::write(
        room.env.rimz_home().join("loop.toml"),
        format!(
            concat!(
                "[throttle]\npace='0s'\ncheck='exit 75'\n[tasks.nightly]\n",
                "workflow='review'\nroot={:?}\nevery='1h'\n"
            ),
            room.env.project_root
        ),
    )
    .unwrap();
    let record = pending(&room, Some("nightly"));
    let value = advance(&room, &record);
    assert_capacity_phrase(&room, &["loop", "list"], &value["record"]["wait"], None);
}

/// Runs `args` and asserts its output holds the wait's capacity phrase. The phrase names
/// the day when the wait is not today, and the binary reads its own clock, so the
/// expected phrase is built from a clock read on each side of the command: a run that
/// crosses local midnight matches one of the two.
fn assert_capacity_phrase(
    room: &Room,
    args: &[&str],
    wait: &serde_json::Value,
    gives_up_at: Option<jiff::Timestamp>,
) {
    let phrase = |now| {
        rimz::theme::fmt::capacity_wait(
            &[rimz::ids::AgentKind::new_unchecked("codex")],
            wait["first"].as_str().unwrap().parse().unwrap(),
            gives_up_at,
            now,
        )
    };
    let before = phrase(jiff::Timestamp::now());
    let text = success(room.command(args).output().unwrap());
    let after = phrase(jiff::Timestamp::now());
    assert!(
        text.contains(&before) || text.contains(&after),
        "{before:?} or {after:?} in {text}"
    );
}

#[test]
fn capacity_deadline_route_cannot_reuse_another_nodes_turn() {
    use std::io::Write as _;
    use std::os::unix::net::UnixListener;

    let room = Room::with_kind("", "claude");
    let spec = room.env.rimz_home().join("workflows/review/workflow.toml");
    fs::write(
        &spec,
        format!(
            "{}\n[steps.fallback]\nagent='codex'\nprompt-file='prompt.md'\nthen='after'\n",
            fs::read_to_string(&spec)
                .unwrap()
                .replace("start = 'pr'", "start = 'pr'\non.deadline='fallback'")
        ),
    )
    .unwrap();
    fs::write(
        room.env.rimz_home().join("loop.toml"),
        concat!(
            "[throttle]\npace='0s'\n",
            "check='printf \"%s\\n\" \"$RIMZ_START_KIND\" >> capacity.log; ",
            "test \"$RIMZ_START_KIND\" = claude || exit 75'\n"
        ),
    )
    .unwrap();
    let record = pending(&room, Some("nightly"));
    let socket = room.env.tmp_root().join("admission-route.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut child = room
        .command(&["workflow", "advance", record.id.as_str()])
        .env("RIMZ_TEST_WORKFLOW_ADMISSION_GATE", &socket)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stream = loop {
        if let Ok((stream, _)) = listener.accept() {
            break stream;
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "advance ended before admission"
        );
        assert!(Instant::now() < deadline, "admission never returned");
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut current = serde_json::to_value(&record).unwrap();
    current["deadline"] = json!(jiff::Timestamp::now() - jiff::SignedDuration::from_secs(1));
    save(&room, &current);
    let routed = advance(&room, &record);
    assert_eq!(routed["record"]["node"], "fallback");
    assert_eq!(routed["record"]["wait"]["outcome"], "capacity");
    stream.write_all(&[1]).unwrap();
    while child.try_wait().unwrap().is_none() {
        if let Ok((mut stream, _)) = listener.accept() {
            stream.write_all(&[1]).unwrap();
        }
        assert!(
            Instant::now() < deadline,
            "advance did not settle after release"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    success(child.wait_with_output().unwrap());
    let value = show(&room.env, record.id.as_str());
    assert_eq!(value["record"]["wait"]["outcome"], "capacity", "{value:#}");
    assert!(value["record"]["attempt"].is_null(), "{value:#}");
    assert_eq!(value["record"]["visits"], json!({}), "{value:#}");
    assert_eq!(
        fs::read_to_string(room.env.project_root.join("capacity.log")).unwrap(),
        "claude\ncodex\n"
    );
    assert!(tickets(&room).is_empty());
}

#[test]
fn capacity_holds_merge_without_consuming_visits_and_render_the_shared_phrase() {
    let room = Room::new("capacity-wait='2h'\n");
    fs::write(
        room.env.rimz_home().join("loop.toml"),
        "[throttle]\npace='0s'\ncheck='exit 75'\n",
    )
    .unwrap();
    let record = pending(&room, Some("nightly"));
    let first = advance(&room, &record);
    let second = advance(&room, &record);
    assert_eq!(
        second["record"]["wait"]["outcome"], "capacity",
        "{second:#}"
    );
    assert_eq!(
        second["record"]["wait"]["first"],
        first["record"]["wait"]["first"]
    );
    assert_eq!(second["record"]["wait"]["count"], 2);
    assert_eq!(second["record"]["wait"]["kinds"], json!(["codex"]));
    assert!(second["record"]["wait"]["gives_up_at"].is_string());
    assert_eq!(second["record"]["input"], json!({"outcome":"go", "key":7}));
    assert_eq!(second["record"]["key"], 7);
    assert_eq!(second["record"]["next_seq"], 1);
    assert_eq!(second["record"]["visits"], json!({}));
    assert!(second["record"]["attempt"].is_null());
    assert!(tickets(&room).is_empty());
    let wait = &second["record"]["wait"];
    let gives_up_at = Some(wait["gives_up_at"].as_str().unwrap().parse().unwrap());
    for args in [
        vec!["workflow", "show", record.id.as_str()],
        vec!["workflow", "list"],
    ] {
        assert_capacity_phrase(&room, &args, wait, gives_up_at);
    }
}

#[test]
fn capacity_give_up_routes_failed_or_a_node_without_an_attempt() {
    let mut actual = Vec::new();
    for (edge, terminal) in [("", "failed"), ("on.capacity='after'\n", "done")] {
        let room = Room::new(&format!("capacity-wait='1s'\n{edge}"));
        fs::write(
            room.env.rimz_home().join("loop.toml"),
            "[throttle]\npace='0s'\nmax-active=0\n",
        )
        .unwrap();
        let record = pending(&room, Some("nightly"));
        let now = jiff::Timestamp::now();
        let mut held = serde_json::to_value(&record).unwrap();
        held["wait"] = json!({
            "node":"build", "outcome":"capacity", "detail":"machine's active agents 0 >= 0 ()",
            "first":now-jiff::SignedDuration::from_secs(2), "last":now,
            "count":1, "recheck_at":null, "kinds":["codex"],
            "gives_up_at":now-jiff::SignedDuration::from_secs(1)
        });
        save(&room, &held);
        let value = advance(&room, &record);
        actual.push((
            value["record"]["state"] == terminal,
            events(&value, "route").iter().any(|row| {
                row["outcome"] == "capacity"
                    && row["input"]["kinds"] == json!(["codex"])
                    && row["input"]["waited_ms"]
                        .as_u64()
                        .is_some_and(|ms| ms >= 2000)
            }),
            !value["record"]["visits"]
                .as_object()
                .unwrap()
                .contains_key("build"),
        ));
        assert!(tickets(&room).is_empty());
    }
    assert_eq!(actual, vec![(true, true, true); 2]);
}

#[test]
fn capacity_manual_and_exempt_steps_bypass_the_door() {
    let mut actual = Vec::new();
    for (task, extra) in [(None, ""), (Some("nightly"), "throttle='off'\n")] {
        let room = Room::new(extra);
        fs::write(
            room.env.rimz_home().join("loop.toml"),
            "[throttle]\npace='0s'\nmax-active=0\n",
        )
        .unwrap();
        let record = pending(&room, task);
        let value = advance(&room, &record);
        actual.push(value["record"]["visits"]["build"]["total"] == 1);
        assert!(tickets(&room).is_empty());
        success(
            room.command(&["workflow", "cancel", record.id.as_str()])
                .output()
                .unwrap(),
        );
    }
    assert_eq!(actual, vec![true; 2]);
}

#[test]
fn capacity_check_runs_off_the_room_lock_once_per_group_and_reaches_launch_commit() {
    if std::env::var_os("RIMZ_TEST_CAPACITY_LOCK_PROBE").is_some() {
        let fixture_home = PathBuf::from(std::env::var_os("RIMZ_HOME").unwrap());
        let state = rimz::StatePaths::for_project_root_under(
            &std::env::current_dir().unwrap(),
            &fixture_home,
        )
        .unwrap();
        assert!(
            WorkspaceLock::try_acquire(&state.workspace_lock)
                .unwrap()
                .is_some()
        );
        return;
    }
    let room = Room::new("");
    let record = pending(&room, Some("nightly"));
    let script = room.env.project_root.join("capacity.sh");
    let probe = format!(
        "{} --exact {}",
        shlex::try_quote(std::env::current_exe().unwrap().to_str().unwrap()).unwrap(),
        concat!(
            "workflow::attempts::capacity::",
            "capacity_check_runs_off_the_room_lock_once_per_group_and_reaches_launch_commit"
        ),
    );
    let body = format!(
        "RIMZ_TEST_CAPACITY_LOCK_PROBE=1 {probe} || exit 2\n\
         printf '%s %s %s\\n' \"$RIMZ_START_KIND\" \"$RIMZ_START_AGENTS\" \
         \"$RIMZ_START_TASK\" >> capacity.log\n"
    );
    fs::write(&script, body).unwrap();
    fs::write(
        room.env.rimz_home().join("loop.toml"),
        "[throttle]\npace='0s'\ncheck='sh capacity.sh'\n",
    )
    .unwrap();
    let value = advance(&room, &record);
    let log = fs::read_to_string(room.env.project_root.join("capacity.log")).unwrap_or_default();
    assert_eq!(log, "codex 1 nightly\n", "{value:#}");
    let queue = tickets(&room);
    assert_eq!(queue.len(), 1, "{queue:#?}");
    assert_eq!(queue[0]["state"], "launched", "{queue:#?}");
    success(
        room.command(&["workflow", "cancel", record.id.as_str()])
            .output()
            .unwrap(),
    );
}

#[test]
fn capacity_admission_rechecks_bounds_and_windows_and_releases_unused_turns() {
    use std::io::Write as _;
    use std::os::unix::net::UnixListener;

    for closes in ["deadline", "window"] {
        let room = Room::with_kind("window='5h.left>=20'\n", "claude");
        super::super::judges::capacity(&room.env, 0);
        fs::write(
            room.env.rimz_home().join("loop.toml"),
            "[throttle]\npace='0s'\ncheck='exit 0'\n",
        )
        .unwrap();
        let record = pending(&room, Some("nightly"));
        let socket = room.env.tmp_root().join("admission.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut child = room
            .command(&["workflow", "advance", record.id.as_str()])
            .env("RIMZ_TEST_WORKFLOW_ADMISSION_GATE", &socket)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            if let Ok((stream, _)) = listener.accept() {
                break stream;
            }
            assert!(
                child.try_wait().unwrap().is_none(),
                "advance ended without deciding capacity"
            );
            assert!(
                Instant::now() < deadline,
                "capacity admission never returned"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(tickets(&room)[0]["state"], "admitted");
        if closes == "deadline" {
            let mut current = show(&room.env, record.id.as_str())["record"].clone();
            current["deadline"] =
                json!(jiff::Timestamp::now() - jiff::SignedDuration::from_secs(1));
            save(&room, &current);
        } else {
            super::super::judges::capacity(&room.env, 100);
        }
        stream.write_all(&[1]).unwrap();
        success(child.wait_with_output().unwrap());
        let value = show(&room.env, record.id.as_str());
        assert!(value["record"]["attempt"].is_null(), "{value:#}");
        assert_eq!(value["record"]["visits"], json!({}));
        assert!(tickets(&room).is_empty());
        assert_eq!(
            if closes == "window" {
                &value["record"]["wait"]["outcome"]
            } else {
                &value["record"]["park"]["reason"]
            },
            closes,
            "{value:#}"
        );
    }
}
