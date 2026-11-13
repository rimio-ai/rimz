use super::*;
use rimz::disk::lock::WorkspaceLock;
use rimz::ids::WorkflowRunId;

mod limits;

fn write_capacity_config(room: &Room, text: impl AsRef<str>) -> std::io::Result<()> {
    let mut tables: toml::Table = toml::from_str(text.as_ref()).unwrap();
    let core_path = room.env.rimz_home().join("config.toml");
    let mut core: toml::Table =
        toml::from_str(&fs::read_to_string(&core_path).unwrap_or_default()).unwrap();
    if let Some(throttle) = tables.remove("throttle") {
        core.insert("throttle".into(), throttle);
    }
    fs::write(core_path, toml::to_string(&core).unwrap())?;
    fs::write(
        room.env.rimz_home().join("loop.toml"),
        toml::to_string(&tables).unwrap(),
    )
}

fn pending(room: &Room, task: Option<&str>) -> records::RunRecord {
    let state = room.env.state_path_for(&room.env.project_root);
    let dir = room.env.rimz_home().join("workflows/review");
    let now = jiff::Timestamp::now();
    let record = serde_json::from_value(json!({
        "version":rimz::store::workflow::VERSION, "id":WorkflowRunId::new(), "workflow":"review", "task":task,
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

/// Hold the home's start-queue lock, as a slow admission elsewhere would.
fn hold_queue(room: &Room) -> WorkspaceLock {
    WorkspaceLock::acquire(&room.env.rimz_home().join("loops/throttle.lock")).unwrap()
}

/// Advance with admission's queue-lock wait cut short, so a held lock times out.
fn advance_queue_busy(room: &Room, record: &records::RunRecord) -> Value {
    success(
        room.command(&["workflow", "advance", record.id.as_str()])
            .env("RIMZ_TEST_THROTTLE_LOCK_WAIT_MS", "100")
            .output()
            .unwrap(),
    );
    show(&room.env, record.id.as_str())
}

#[test]
fn capacity_lock_timeouts_wait_without_a_visit_and_launch_once_the_queue_frees() {
    for task in [Some("nightly"), None] {
        let room = Room::new("[agent.build.admission]\ngive-up='2h'\nmax-parallel=5\n");
        let record = pending(&room, task);
        let queue = hold_queue(&room);
        let value = advance_queue_busy(&room, &record);
        let wait = &value["record"]["wait"];
        assert_eq!(value["record"]["state"], "active", "{value:#}");
        assert_eq!(wait["outcome"], "capacity", "{value:#}");
        assert_eq!(wait["hold"], "queue", "{value:#}");
        let detail = wait["detail"].as_str().unwrap();
        assert!(
            detail.starts_with("waiting for the start queue: timed out after")
                && detail.contains("loops/throttle.lock"),
            "{detail}"
        );
        assert_eq!(wait["kinds"], json!(["codex"]), "{value:#}");
        assert!(wait["gives_up_at"].is_string(), "{value:#}");
        assert_eq!(value["record"]["visits"], json!({}), "{value:#}");
        assert!(value["record"]["attempt"].is_null(), "{value:#}");
        assert!(events(&value, "attempt").is_empty(), "{value:#}");
        let shown = success(
            room.command(&["workflow", "show", record.id.as_str()])
                .output()
                .unwrap(),
        );
        assert!(shown.contains(detail), "{shown}");
        let again = advance_queue_busy(&room, &record);
        assert_eq!(again["record"]["wait"], *wait, "{again:#}");
        drop(queue);
        let value = advance(&room, &record);
        assert_eq!(value["record"]["visits"]["build"], 1, "{value:#}");
        assert!(value["record"]["attempt"].is_object(), "{value:#}");
        success(
            room.command(&["workflow", "cancel", record.id.as_str()])
                .output()
                .unwrap(),
        );
    }
}

#[test]
fn capacity_lock_timeouts_on_manual_and_restart_runs_wait_until_the_node_give_up() {
    for restart in [false, true] {
        let room = Room::new("[agent.build.admission]\ngive-up='2h'\nmax-parallel=5\n");
        let record = pending(&room, Some("nightly"));
        let queue = hold_queue(&room);
        let value = if restart {
            let receipt = success(
                room.command(&["workflow", "restart", record.id.as_str()])
                    .env("RIMZ_TEST_THROTTLE_LOCK_WAIT_MS", "100")
                    .output()
                    .unwrap(),
            );
            assert!(
                receipt.contains("step build · fresh · awaiting launch; visits reset"),
                "{receipt}"
            );
            show(&room.env, record.id.as_str())
        } else {
            let mut manual = show(&room.env, record.id.as_str())["record"].clone();
            manual["manual"] = json!(true);
            save(&room, &manual);
            advance_queue_busy(&room, &record)
        };
        let wait = &value["record"]["wait"];
        assert_eq!(value["record"]["state"], "active", "{value:#}");
        assert_eq!(value["record"]["restart"].is_object(), restart, "{value:#}");
        assert_eq!(wait["outcome"], "capacity", "{value:#}");
        assert_eq!(wait["hold"], "queue", "{value:#}");
        let detail = wait["detail"].as_str().unwrap();
        assert!(
            detail.starts_with("waiting for the start queue: timed out after")
                && detail.contains("loops/throttle.lock"),
            "{detail}"
        );
        assert!(wait["gives_up_at"].is_string(), "{value:#}");
        assert_eq!(value["record"]["visits"], json!({}), "{value:#}");
        assert!(value["record"]["attempt"].is_null(), "{value:#}");
        assert!(events(&value, "attempt").is_empty(), "{value:#}");
        let again = advance_queue_busy(&room, &record);
        assert_eq!(again["record"]["state"], "active", "{again:#}");
        assert!(
            !events(&again, "route")
                .iter()
                .any(|row| row["outcome"] == "capacity"),
            "{again:#}"
        );
        drop(queue);
        let value = advance(&room, &record);
        assert_eq!(value["record"]["visits"]["build"], 1, "{value:#}");
        assert!(value["record"]["attempt"].is_object(), "{value:#}");
        success(
            room.command(&["workflow", "cancel", record.id.as_str()])
                .output()
                .unwrap(),
        );
    }
}

#[test]
fn capacity_lock_timeout_waits_still_give_up_through_capacity() {
    for task in [Some("nightly"), None] {
        lock_timeout_gives_up(task);
    }
}

fn lock_timeout_gives_up(task: Option<&str>) {
    let room = Room::new("admission.give-up='2h'\n");
    write_capacity_config(&room, "[throttle]\nmax-parallel-agents=5\n").unwrap();
    let record = pending(&room, task);
    let _queue = hold_queue(&room);
    let mut value = advance_queue_busy(&room, &record);
    assert_eq!(value["record"]["wait"]["hold"], "queue", "{value:#}");
    value["record"]["wait"]["gives_up_at"] =
        json!(jiff::Timestamp::now() - jiff::SignedDuration::from_secs(1));
    save(&room, &value["record"]);
    let value = advance_queue_busy(&room, &record);
    assert_eq!(value["record"]["state"], "failed", "{value:#}");
    assert!(
        events(&value, "route")
            .iter()
            .any(|row| row["outcome"] == "capacity"
                && row["input"]["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.starts_with("waiting for the start queue"))),
        "{value:#}"
    );
    assert_eq!(value["record"]["visits"], json!({}), "{value:#}");
    assert!(events(&value, "attempt").is_empty(), "{value:#}");
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
            write_capacity_config(&room, "[throttle]\nmax-parallel-agents=1\n").unwrap();
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
            value["record"]["state"] == "failed",
            events(&value, "attempt")
                .iter()
                .any(|row| row["outcome"] == "died"),
        ));
    }
    assert_eq!(actual, [(true, true); 2]);
}

#[test]
fn capacity_hand_manual_and_scheduled_workflows_all_wait() {
    let mut actual = Vec::new();
    for verb in ["workflow", "fire", "run"] {
        let room = Room::new("");
        write_capacity_config(
            &room,
            format!(
                concat!(
                    "[throttle]\ncheck='exit 75'\n[tasks.nightly]\n",
                    "workflow='review'\nroot={:?}\nevery='1h'\n"
                ),
                room.env.project_root
            ),
        )
        .unwrap();
        let start = match verb {
            "workflow" => ["workflow", "run", "review"],
            verb => ["loop", verb, "nightly"],
        };
        success(room.command(&start).output().unwrap());
        let state = room.env.state_path_for(&room.env.project_root);
        let record = records::list(&state).unwrap().pop().unwrap();
        let value = until(&room.env, record.id.as_str(), |value| {
            value["record"]["visits"]["build"] == 1
                || value["record"]["wait"]["outcome"] == "capacity"
        });
        actual.push((
            value["record"]["visits"]["build"] == 1,
            value["record"]["manual"] == true,
            value["record"]["wait"]["outcome"] == "capacity",
        ));
        success(
            room.command(&["workflow", "cancel", record.id.as_str()])
                .output()
                .unwrap(),
        );
    }
    assert_eq!(
        actual,
        [
            (false, false, true),
            (false, true, true),
            (false, false, true)
        ]
    );
}

#[test]
fn capacity_receipts_of_run_fire_and_restart_say_the_start_is_held() {
    let room = Room::new("");
    let spec = room.env.rimz_home().join("workflows/review/workflow.toml");
    fs::write(
        &spec,
        fs::read_to_string(&spec)
            .unwrap()
            .replace("start = 'pr'", "start = 'build'"),
    )
    .unwrap();
    write_capacity_config(
        &room,
        format!(
            concat!(
                "[throttle]\ncheck='exit 75'\n[tasks.nightly]\n",
                "workflow='review'\nroot={:?}\nevery='1h'\n"
            ),
            room.env.project_root
        ),
    )
    .unwrap();
    let held = |line: &str, indent: &str| {
        line.strip_prefix(indent).is_some_and(|line| {
            line.starts_with("build held ")
                && line.ends_with("codex/default: no capacity (your check)")
        })
    };
    let run = success(
        room.command(&["workflow", "run", "review"])
            .output()
            .unwrap(),
    );
    let lines: Vec<_> = run.lines().collect();
    assert_eq!(lines.len(), 4, "{run}");
    assert!(lines[2].starts_with("→ rimz workflow show"), "{run}");
    assert!(held(lines[3], ""), "{run}");
    // A run no loop task started is listed by its address.
    let listed = success(room.command(&["workflow", "list"]).output().unwrap());
    assert!(
        listed.contains("review · 0 active · 1 held") && listed.contains("#main  held · "),
        "{listed}"
    );
    let id = lines[0];
    let restart = success(room.command(&["workflow", "restart", id]).output().unwrap());
    assert!(
        restart.lines().last().is_some_and(|line| held(line, "")),
        "{restart}"
    );
    let cancel = |id: &str| success(room.command(&["workflow", "cancel", id]).output().unwrap());
    cancel(id);
    let fire = success(room.command(&["loop", "fire", "nightly"]).output().unwrap());
    assert!(
        fire.lines().last().is_some_and(|line| held(line, "  ")),
        "{fire}"
    );
    let state = room.env.state_path_for(&room.env.project_root);
    for record in records::list(&state).unwrap() {
        assert_eq!(record.visits.get("build"), None, "{record:#?}");
        if !record.state.is_terminal() {
            cancel(record.id.as_str());
        }
    }
}

#[test]
fn capacity_give_up_takes_an_instant_and_ends_a_hand_run_there() {
    let room = Room::new("");
    let spec = room.env.rimz_home().join("workflows/review/workflow.toml");
    fs::write(
        &spec,
        fs::read_to_string(&spec)
            .unwrap()
            .replace("start = 'pr'", "start = 'build'"),
    )
    .unwrap();
    write_capacity_config(&room, "[throttle]\ncheck='exit 75'\n").unwrap();
    let refused = room
        .command(&["workflow", "run", "review", "--give-up", "30m"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert_eq!(refused.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains(concat!(
            "\"30m\" is not an instant; give a timestamp (2026-11-13T21:00:00Z), ",
            "a local date-time (2026-11-13T21:00), or a date (2026-11-14)"
        )),
        "{stderr}"
    );
    let state = room.env.state_path_for(&room.env.project_root);
    assert!(records::list(&state).unwrap().is_empty());
    let mut actual = Vec::new();
    for offset in [3600, -1] {
        let at = jiff::Timestamp::now() + jiff::SignedDuration::from_secs(offset);
        let receipt = success(
            room.command(&["workflow", "run", "review", "--give-up", &at.to_string()])
                .output()
                .unwrap(),
        );
        let id = receipt.lines().next().unwrap();
        let value = until(&room.env, id, |value| {
            if offset > 0 {
                value["record"]["wait"]["outcome"] == "capacity"
            } else {
                value["record"]["state"] == "failed"
            }
        });
        actual.push((
            value["record"]["give_up"] == json!(at),
            value["record"]["state"].as_str().unwrap().to_owned(),
            value["record"]["wait"]["gives_up_at"] == json!(at),
            events(&value, "route")
                .iter()
                .any(|row| row["outcome"] == "capacity"),
            value["record"]["visits"] == json!({}),
        ));
        if offset > 0 {
            success(room.command(&["workflow", "cancel", id]).output().unwrap());
        }
    }
    assert_eq!(
        actual,
        [
            (true, "active".into(), true, false, true),
            (true, "failed".into(), false, true, true)
        ]
    );
}

#[test]
fn capacity_workflow_wait_reaches_the_human_loop_list() {
    let room = Room::new("");
    write_capacity_config(
        &room,
        format!(
            concat!(
                "[throttle]\ncheck='exit 75'\n[tasks.nightly]\n",
                "workflow='review'\nroot={:?}\nevery='1h'\n"
            ),
            room.env.project_root
        ),
    )
    .unwrap();
    let record = pending(&room, Some("nightly"));
    let mut value = advance(&room, &record);
    let text = success(room.command(&["loop", "list"]).output().unwrap());
    assert!(
        text.contains("HELD  starts waiting for capacity")
            && text.contains("○ 1 held")
            && text.contains(value["record"]["wait"]["detail"].as_str().unwrap())
            && !text.contains("NEEDS YOU"),
        "{text}"
    );
    let listed: Value = serde_json::from_str(&success(
        room.command(&["loop", "list", "--json"]).output().unwrap(),
    ))
    .unwrap();
    assert_eq!(
        listed["rooms"][0]["tasks"][0]["capacity_wait"]["hold"],
        "capacity"
    );
    value["record"]["wait"]["hold"] = json!("check_failed");
    value["record"]["wait"]["detail"] = json!("capacity check failing: codex exit 2");
    save(&room, &value["record"]);
    let text = success(room.command(&["loop", "list"]).output().unwrap());
    assert!(
        text.contains("NEEDS YOU")
            && text.contains("capacity check failing: codex exit 2")
            && text.contains("→ rimz doctor"),
        "{text}"
    );
}

/// Bracket the binary's clock for the held age and day-qualified give-up clock.
fn assert_capacity_phrase(
    room: &Room,
    args: &[&str],
    wait: &serde_json::Value,
    gives_up_at: Option<jiff::Timestamp>,
) {
    let phrase = |now| {
        rimz::theme::fmt::held_start(
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
            "{}\n[agent.fallback]\nlifecycle='turn'\nagent='codex'\nprompt-file='prompt.md'\nthen='after'\n",
            fs::read_to_string(&spec)
                .unwrap()
                .replace("start = 'pr'", "start = 'pr'\non.deadline='fallback'")
        ),
    )
    .unwrap();
    write_capacity_config(
        &room,
        concat!(
            "[throttle]\n",
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
    let room = Room::new("admission.give-up='2h'\n");
    write_capacity_config(&room, "[throttle]\ncheck='exit 75'\n").unwrap();
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
    assert_capacity_phrase(
        &room,
        &["workflow", "show", record.id.as_str()],
        wait,
        gives_up_at,
    );
    let listed = success(room.command(&["workflow", "list"]).output().unwrap());
    assert!(
        listed.contains(" · 1 held")
            && listed.contains(wait["detail"].as_str().unwrap())
            && !listed.contains("×2"),
        "{listed}"
    );
}

#[test]
fn capacity_give_up_routes_failed_or_a_node_without_an_attempt() {
    let mut actual = Vec::new();
    for (edge, terminal) in [("", "failed"), ("on.capacity='after'\n", "done")] {
        let room = Room::new(&format!("admission.give-up='1s'\n{edge}"));
        write_capacity_config(&room, "[throttle]\ncheck='exit 75'\n").unwrap();
        let record = pending(&room, Some("nightly"));
        let now = jiff::Timestamp::now();
        let mut held = serde_json::to_value(&record).unwrap();
        held["wait"] = json!({
            "node":"build", "outcome":"capacity", "detail":"capacity check declined codex",
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
fn capacity_exempt_steps_and_skipped_starts_pass_the_door() {
    let mut actual = Vec::new();
    for (task, extra, skip) in [
        (None, "", false),
        (None, "admission.throttle='off'\n", false),
        (Some("nightly"), "admission.throttle='off'\n", false),
        (None, "", true),
    ] {
        let room = Room::new(extra);
        write_capacity_config(&room, "[throttle]\ncheck='exit 75'\n").unwrap();
        let throttle: Value = serde_json::from_str(&success(
            room.command(&["config", "get", "throttle", "--json"])
                .output()
                .unwrap(),
        ))
        .unwrap();
        assert_eq!(throttle["check"], "exit 75", "{throttle:#}");
        // The skipped start goes through the CLI flag, so its lowering onto the record is covered.
        let (id, value) = if skip {
            let spec = room.env.rimz_home().join("workflows/review/workflow.toml");
            let text = fs::read_to_string(&spec).unwrap();
            fs::write(&spec, text.replace("start = 'pr'", "start = 'build'")).unwrap();
            let receipt = success(
                room.command(&["workflow", "run", "review", "--skip-capacity"])
                    .output()
                    .unwrap(),
            );
            let id = receipt.lines().next().unwrap().to_owned();
            let value = until(&room.env, &id, |value| {
                value["record"]["visits"]["build"] == 1
                    || value["record"]["wait"]["outcome"] == "capacity"
            });
            (id, value)
        } else {
            let record = pending(&room, task);
            (record.id.to_string(), advance(&room, &record))
        };
        actual.push((
            value["record"]["visits"]["build"] == 1,
            value["record"]["attempt"]["started_past_limit"].clone(),
            tickets(&room).len(),
        ));
        success(room.command(&["workflow", "cancel", &id]).output().unwrap());
    }
    let launched = (true, Value::Null, 0);
    assert_eq!(
        actual,
        [
            (false, Value::Null, 0),
            launched.clone(),
            launched,
            (true, json!("codex/default: no capacity (your check)"), 1),
        ]
    );
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
    write_capacity_config(&room, "[throttle]\ncheck='sh capacity.sh'\n").unwrap();
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
        let room = Room::with_kind("admission.window='5h.left>=20'\n", "claude");
        super::super::judges::capacity(&room.env, 0);
        write_capacity_config(&room, "[throttle]\ncheck='exit 0'\n").unwrap();
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
                &value["record"]["input"]["outcome"]
            },
            closes,
            "{value:#}"
        );
    }
}
