use super::*;
use rimz::agents::LifecycleSignal;
use rimz::disk::lock::{IngressAppendLock, WorkspaceLock};
use rimz::ids::{AgentKind, EventId};
use rimz::store::event::EventKind;
use rimz::store::ingress::{self, HookDrainCursor, HookIngress};
use rimz::store::writer::AgentLifecycleIntent;
use std::collections::BTreeMap;

fn stop_frame(env: &Env) -> HookIngress {
    let command = env.hook_command("claude");
    let mut captured: BTreeMap<_, _> = command
        .get_envs()
        .filter_map(|(key, value)| {
            value.map(|value| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
        })
        .collect();
    captured.extend([
        ("RIMZ_AGENT_PID".into(), env.agent_owner_pid().to_string()),
        ("RIMZ_WORKSPACE_ID".into(), env.workspace_id.to_string()),
        (
            "RIMZ_PROJECT_ROOT".into(),
            env.project_root.display().to_string(),
        ),
    ]);
    HookIngress {
        schema_version: "1".into(),
        event_id: EventId::new(),
        ts: Timestamp::now(),
        source: AgentKind::new_unchecked("claude"),
        event: Some("Stop".into()),
        payload: json!({"session_id": "drain-session", "last_assistant_message": "finished"})
            .to_string(),
        cwd: env.project_root.clone(),
        hook_pid: std::process::id(),
        env: captured,
    }
}

fn append_stop(env: &Env) -> (HookIngress, u64) {
    append_frame(env, stop_frame(env))
}

fn append_frame(env: &Env, frame: HookIngress) -> (HookIngress, u64) {
    let store = env.store();
    let lock = IngressAppendLock::acquire(&store.paths().hook_ingress_lock).unwrap();
    let end = ingress::append(store.paths(), &frame, &lock).unwrap();
    (frame, end)
}

#[test]
fn hook_ignores_non_utf8_environment_outside_the_allowlist() {
    use std::os::unix::ffi::OsStringExt;

    let env = Env::new();
    env.record(&env.project_root);
    let mut hook = env.hook_command("claude");
    hook.args(["--event", "Stop"])
        .env("RIMZ_WORKSPACE_ID", env.workspace_id.as_str())
        .env("RIMZ_PROJECT_ROOT", &env.project_root)
        .env(
            "UNRELATED_BINARY_VALUE",
            std::ffi::OsString::from_vec(vec![0xff]),
        )
        .env(std::ffi::OsString::from_vec(vec![0xff]), "ignored");
    let output = env
        .spawn_payload(hook, &stop_frame(&env).payload)
        .wait_with_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    env.drain_hooks();
    assert!(
        env.read_events()
            .iter()
            .any(|event| event.ingress.is_some())
    );
}

#[test]
fn pinned_hook_appends_from_a_deleted_working_directory() {
    let env = Env::new();
    env.record(&env.project_root);
    let cwd = env.project_root.join("removed-checkout");
    std::fs::create_dir(&cwd).unwrap();
    let mut hook = env.hook_command("claude");
    hook.args(["--event", "Stop"])
        .env("RIMZ_WORKSPACE_ID", env.workspace_id.as_str())
        .env("RIMZ_PROJECT_ROOT", &env.project_root)
        .env("PWD", &cwd);
    let mut removed_cwd_hook = Command::new("/bin/sh");
    removed_cwd_hook
        .args(["-c", "rmdir \"$PWD\" && exec \"$@\"", "deleted-cwd"])
        .arg(hook.get_program())
        .args(hook.get_args())
        .envs(
            hook.get_envs()
                .filter_map(|(key, value)| value.map(|value| (key, value))),
        )
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = env
        .spawn_payload(removed_cwd_hook, &stop_frame(&env).payload)
        .wait_with_output()
        .unwrap();
    assert!(
        output.status.success(),
        "a pinned hook must survive a removed cwd: {output:?}"
    );
    env.drain_hooks();
    assert!(
        env.read_events()
            .iter()
            .any(|event| event.ingress.is_some())
    );
}

#[test]
fn queued_frame_with_a_deleted_cwd_applies_from_the_project_root() {
    let env = Env::new();
    env.record(&env.project_root);
    let cwd = env.project_root.join("removed-checkout");
    std::fs::create_dir(&cwd).unwrap();
    let mut frame = stop_frame(&env);
    frame.cwd = cwd.clone();
    let (frame, _) = append_frame(&env, frame);
    std::fs::remove_dir(&cwd).unwrap();
    once(&env);
    assert!(
        !derived(&env, &frame.event_id).is_empty(),
        "a vanished checkout must not drop the frame"
    );
}

#[test]
fn drainer_truncates_a_torn_ingress_tail_and_records_the_repair() {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let (frame, _) = append_stop(&env);
    rimz::disk::atomic::append_record_bytes(&store.paths().hook_ingress_log, b"20 deadbeef {")
        .unwrap();
    let _output = drain_command(&env).arg("--once").output().unwrap();
    assert!(!derived(&env, &frame.event_id).is_empty());
    assert_eq!(
        std::fs::metadata(&store.paths().hook_ingress_log)
            .unwrap()
            .len(),
        0,
        "a torn tail must not keep pending ingress stuck forever"
    );
    let diagnostics =
        std::fs::read_to_string(store.paths().audit_path("hook-drain.log.jsonl")).unwrap();
    assert!(diagnostics.contains("truncate_tail"), "{diagnostics}");
}

#[test]
fn drainer_resyncs_corrupt_ingress_and_applies_following_frames() {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    rimz::disk::atomic::append_record_bytes(&store.paths().hook_ingress_log, b"20 deadbeef {1")
        .unwrap();
    let (fused, _) = append_stop(&env);
    let (first, _) = append_stop(&env);
    let (second, _) = append_stop(&env);
    let _output = drain_command(&env).arg("--once").output().unwrap();
    for frame in [fused, first, second] {
        assert!(
            !derived(&env, &frame.event_id).is_empty(),
            "good ingress after corruption must still apply: {}",
            frame.event_id
        );
    }
    assert_eq!(
        std::fs::metadata(&store.paths().hook_ingress_log)
            .unwrap()
            .len(),
        0
    );
    let diagnostics =
        std::fs::read_to_string(store.paths().audit_path("hook-drain.log.jsonl")).unwrap();
    assert!(diagnostics.contains("skip_record"), "{diagnostics}");
}

#[test]
fn failed_apply_leaves_a_durable_diagnostic() {
    let env = Env::new();
    env.record(&env.project_root);
    let mut frame = stop_frame(&env);
    frame.payload = "{".into();
    let (frame, _) = append_frame(&env, frame);
    once(&env);
    let path = env.store().paths().audit_path("hook-drain.log.jsonl");
    assert!(
        path.is_file(),
        "a dropped frame needs durable diagnostic evidence"
    );
    let diagnostics = std::fs::read_to_string(path).unwrap();
    assert!(
        diagnostics.contains("apply_failed") && diagnostics.contains(frame.event_id.as_str()),
        "{diagnostics}"
    );
}

#[test]
fn deferred_lifecycle_and_active_time_use_the_ingress_timestamp() {
    let env = Env::new();
    env.record(&env.project_root);
    let started = Timestamp::now() - Duration::from_secs(10);
    let mut prompt = stop_frame(&env);
    prompt.ts = started;
    prompt.event = Some("UserPromptSubmit".into());
    prompt.payload = json!({"session_id": "drain-session", "prompt": "work on this"}).to_string();
    let (prompt, _) = append_frame(&env, prompt);
    let mut stop = stop_frame(&env);
    stop.ts = started + Duration::from_secs(10);
    let (stop, _) = append_frame(&env, stop);
    once(&env);
    for frame in [&prompt, &stop] {
        let events = derived(&env, &frame.event_id);
        assert!(!events.is_empty(), "{events:?}");
        assert!(
            events.iter().all(|event| event.timestamp == frame.ts),
            "all frame events must retain ingress time: {events:?}"
        );
    }
    let store = env.store();
    let records = rimz::store::active_time::read_for_keys(
        store.runtime_paths(),
        [("claude", "drain-session")],
    );
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].last_progress, stop.ts);
    assert_eq!(
        records[0].credited_ms, 10_000,
        "deferred apply must not collapse the working span"
    );
    let agents = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    let agent = agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "drain-session")
        .unwrap();
    assert_eq!(agent.turn_started_at, Some(prompt.ts));
    assert_eq!(agent.turn_ended_at, Some(stop.ts));
}

fn apply_barrier(listener: &std::os::unix::net::UnixListener) -> std::os::unix::net::UnixStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match listener.accept() {
            Ok((stream, _)) => return stream,
            Err(error) => {
                assert!(
                    Instant::now() < deadline,
                    "apply never reached its barrier: {error}"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

fn drainer_connection(store: &rimz::Store) -> std::os::unix::net::UnixStream {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match std::os::unix::net::UnixStream::connect(
            store.runtime_paths().hook_drainer_socket_path(),
        ) {
            Ok(stream) => return stream,
            Err(error) => {
                assert!(Instant::now() < deadline, "drainer did not bind: {error}");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

#[test]
fn prompt_receipt_does_not_wait_for_a_later_frame() {
    use std::io::{BufRead as _, BufReader};
    use std::os::unix::net::{UnixListener, UnixStream};

    let env = Env::new();
    crate::common::git::init_repo(&env.project_root);
    env.record(&env.project_root);
    let store = env.store();
    let first_socket = store.runtime_paths().sock_dir.join("first-apply.sock");
    let later_socket = store.runtime_paths().sock_dir.join("later-apply.sock");
    let first_barrier = UnixListener::bind(&first_socket).unwrap();
    let _later_barrier = UnixListener::bind(&later_socket).unwrap();
    let mut prompt = stop_frame(&env);
    prompt.event = Some("UserPromptSubmit".into());
    prompt.payload = json!({"session_id": "drain-session", "prompt": "work on this"}).to_string();
    prompt.env.insert("RIMZ_RUNTIME_ENV".into(), "1".into());
    prompt.env.insert(
        "RIMZ_TEST_HOOK_APPLY".into(),
        first_socket.display().to_string(),
    );
    let (prompt, through) = append_frame(&env, prompt);
    let mut later = stop_frame(&env);
    later.env.insert(
        "RIMZ_TEST_HOOK_APPLY".into(),
        later_socket.display().to_string(),
    );
    append_frame(&env, later);
    let mut drainer = drain_command(&env).spawn().unwrap();
    let mut release = apply_barrier(&first_barrier);
    let mut request =
        UnixStream::connect(store.runtime_paths().hook_drainer_socket_path()).unwrap();
    request
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    writeln!(
        request,
        "{}",
        json!({"through": through, "reply_for": prompt.event_id})
    )
    .unwrap();
    release.write_all(&[1]).unwrap();
    let mut line = String::new();
    let received = BufReader::new(request).read_line(&mut line);
    drainer.kill().unwrap();
    drainer.wait().unwrap();
    assert!(
        received.is_ok(),
        "an applied prompt must be answered while the later frame is blocked: {received:?}"
    );
    let receipt: Value = serde_json::from_str(&line).unwrap();
    assert!(receipt["applied"].as_u64().unwrap() >= through);
    assert!(
        receipt["reply"]["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .contains("$ git status --short --branch")
    );
}

#[test]
fn late_request_claims_a_cached_reply_once() {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let mut frame = stop_frame(&env);
    frame.source = AgentKind::new_unchecked("cursor");
    frame.event = Some("sessionStart".into());
    let (frame, through) = append_frame(&env, frame);
    let mut drainer = drain_command(&env).spawn().unwrap();
    let _lease = drainer_connection(&store);
    rimz::harness::hook_drain::drain_through(&store, through, None, Duration::from_secs(5))
        .unwrap();
    let receipt = rimz::harness::hook_drain::drain_through(
        &store,
        through,
        Some(frame.event_id.clone()),
        Duration::from_secs(2),
    )
    .unwrap();
    let second = rimz::harness::hook_drain::drain_through(
        &store,
        through,
        Some(frame.event_id),
        Duration::from_secs(2),
    )
    .unwrap();
    drainer.kill().unwrap();
    drainer.wait().unwrap();
    assert_eq!(
        receipt.reply,
        Some(json!({})),
        "a request arriving after apply must still get its computed decision"
    );
    assert_eq!(second.reply, None, "a reply is claimed only once");
}

#[test]
fn truncated_ingress_receipts_do_not_reuse_the_old_offset() {
    use std::io::{BufRead as _, BufReader};
    use std::os::unix::net::UnixStream;

    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let (_, through) = append_stop(&env);
    let mut drainer = drain_command(&env).spawn().unwrap();
    let _lease = drainer_connection(&store);
    let old =
        rimz::harness::hook_drain::drain_through(&store, through, None, Duration::from_secs(5))
            .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::fs::metadata(&store.paths().hook_ingress_log)
        .unwrap()
        .len()
        != 0
    {
        assert!(
            Instant::now() < deadline,
            "drainer never truncated its completed log"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut request =
        UnixStream::connect(store.runtime_paths().hook_drainer_socket_path()).unwrap();
    request
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    writeln!(request, "{}", json!({"through": 0, "reply_for": null})).unwrap();
    let mut line = String::new();
    BufReader::new(request).read_line(&mut line).unwrap();
    drainer.kill().unwrap();
    drainer.wait().unwrap();
    let empty: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        empty["applied"], 0,
        "a new ingress epoch has applied zero bytes"
    );
    assert_ne!(
        empty["epoch"],
        serde_json::to_value(old).unwrap()["epoch"],
        "offset reuse must be distinguishable on the wire"
    );
}

#[test]
fn inline_drain_finishes_a_started_frame_after_the_reply_deadline() {
    use std::os::unix::net::UnixListener;

    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let socket = store.runtime_paths().sock_dir.join("inline-apply.sock");
    let barrier = UnixListener::bind(&socket).unwrap();
    let mut frame = stop_frame(&env);
    frame
        .env
        .insert("RIMZ_TEST_HOOK_APPLY".into(), socket.display().to_string());
    let (started, started_end) = append_frame(&env, frame);
    let (pending, _) = append_stop(&env);
    let mut hook = env.hook_command("claude");
    hook.env("RIMZ_BIN", env.project_root.join("missing-rimz"));
    let child = env.spawn_payload(
        hook,
        &json!({"hook_event_name": "Stop", "session_id": "deadline-stop"}).to_string(),
    );
    let mut release = apply_barrier(&barrier);
    std::thread::sleep(Duration::from_millis(1300));
    let _ = release.write_all(&[1]);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        derived(&env, &started.event_id).len(),
        1,
        "the reply deadline must not kill and consume a started frame"
    );
    let cursor = ingress::read_cursor(store.paths()).unwrap();
    assert_eq!(cursor.applied, started_end);
    assert_eq!(cursor.claimed, started_end);
    let (frames, _) = ingress::read_from_offset(store.paths(), started_end).unwrap();
    assert_eq!(frames.len(), 2, "later frames must remain pending");
    for (frame, _) in &frames {
        assert!(derived(&env, &frame.event_id).is_empty());
    }
    assert_eq!(frames[0].0.event_id, pending.event_id);
    let diagnostics = std::fs::read_to_string(store.paths().audit_path("hook-drain.log.jsonl"))
        .unwrap_or_default();
    assert!(!diagnostics.contains("apply_failed"), "{diagnostics}");
    once(&env);
    for (frame, _) in frames {
        assert_eq!(derived(&env, &frame.event_id).len(), 1);
    }
}

#[test]
fn inline_drain_stops_at_the_hook_reply_deadline() {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let (frame, _) = append_stop(&env);
    let workspace = WorkspaceLock::acquire(&store.paths().workspace_lock).unwrap();
    let mut hook = env.hook_command("claude");
    hook.env("RIMZ_BIN", env.project_root.join("missing-rimz"));
    let mut child = env.spawn_payload(
        hook,
        &json!({"hook_event_name": "Stop", "session_id": "deadline-stop"}).to_string(),
    );
    let deadline = Instant::now() + Duration::from_millis(2500);
    let completed = loop {
        if child.try_wait().unwrap().is_some() {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    if !completed {
        child.kill().unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        completed,
        "an inline drain must not wait past the hook deadline to start a frame"
    );
    assert!(output.status.success(), "{output:?}");
    drop(workspace);
    assert!(derived(&env, &frame.event_id).is_empty());
    assert_eq!(
        ingress::read_cursor(store.paths()).unwrap(),
        HookDrainCursor::default(),
        "a frame reached with no time left must not be claimed"
    );
    let (frames, _) = ingress::read_from_offset(store.paths(), 0).unwrap();
    assert_eq!(frames.len(), 2, "both frames must remain pending");
}

#[test]
#[cfg(target_os = "linux")]
fn hook_append_completes_while_another_process_holds_workspace_lock() {
    use std::io::{BufRead as _, BufReader};

    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let _spawning =
        WorkspaceLock::acquire(&store.runtime_paths().hook_drainer_spawn_lock()).unwrap();
    let mut holder = Command::new("flock")
        .arg(&store.paths().workspace_lock)
        .args(["/bin/sh", "-c", "printf 'locked\\n'; read release"])
        .env_clear()
        .env("HOME", &env.home_root)
        .env("XDG_RUNTIME_DIR", &env.runtime_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut ready = String::new();
    BufReader::new(holder.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready, "locked\n");
    let mut hook = env.hook_command("codex");
    hook.env("RIMZ_WORKSPACE_ID", env.workspace_id.as_str())
        .env("RIMZ_PROJECT_ROOT", &env.project_root);
    let mut child = env.spawn_payload(hook, &json!({"hook_event_name": "Stop", "session_id": "unblocked-stop", "last_assistant_message": "done"}).to_string());
    let deadline = Instant::now() + Duration::from_secs(2);
    let completed = loop {
        if child.try_wait().unwrap().is_some() {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    drop(holder.stdin.take());
    holder.wait().unwrap();
    if !completed {
        child.kill().unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(completed, "a hook append must not wait on workspace.lock");
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert_eq!(
        ingress::read_from_offset(store.paths(), 0).unwrap().0.len(),
        1
    );
    assert!(env.read_events().is_empty());
}

#[test]
fn queued_codex_stops_decode_each_captured_provider_home() {
    let env = Env::new();
    env.record(&env.project_root);
    let mut frames = Vec::new();
    for (session, errored) in [("failed-session", true), ("success-session", false)] {
        let home = env.project_root.join(session);
        let sessions = home.join("sessions");
        let day = sessions.join("2026/06/11");
        std::fs::create_dir_all(&day).unwrap();
        let tail = if errored {
            json!({"timestamp": "2026-06-11T07:18:00.000Z", "type": "event_msg", "payload": {"type": "turn_error", "message": "You've hit your usage limit", "codexErrorInfo": "usageLimitExceeded"}})
        } else {
            json!({"timestamp": "2026-06-11T07:18:00.000Z", "type": "event_msg", "payload": {"type": "task_complete", "last_agent_message": "finished"}})
        };
        std::fs::write(
            day.join(format!("rollout-2026-06-11T07-18-00-{session}.jsonl")),
            format!("{tail}\n"),
        )
        .unwrap();
        let mut frame = stop_frame(&env);
        frame.source = AgentKind::new_unchecked("codex");
        frame.payload = json!({"session_id": session}).to_string();
        frame
            .env
            .insert("CODEX_HOME".into(), home.display().to_string());
        frames.push((append_frame(&env, frame).0.event_id, errored));
    }
    once(&env);
    for (id, expected_error) in frames {
        let observed_error = derived(&env, &id)
            .into_iter()
            .find_map(|event| match event.kind() {
                EventKind::AgentLifecycle(payload) => match payload.observation.signal {
                    LifecycleSignal::TurnEnded { errored, .. } => Some(errored),
                    _ => None,
                },
                _ => None,
            });
        assert_eq!(
            observed_error,
            Some(expected_error),
            "decode must use this frame's provider home"
        );
    }
}

fn drain_command(env: &Env) -> Command {
    let mut command = env.rimz();
    command
        .args(["hooks", "drain", "--project-root"])
        .arg(&env.project_root);
    command
}

fn once(env: &Env) {
    let output = drain_command(env)
        .arg("--once")
        .env("RIMZ_TEST_HOOK_DRAIN_IDLE_MS", "1000")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
}

fn derived(env: &Env, id: &EventId) -> Vec<rimz::EventEnvelope> {
    env.read_events()
        .into_iter()
        .filter(|event| event.ingress.as_ref() == Some(id))
        .collect()
}

fn paused_drainer(
    env: &Env,
    failpoint: &str,
) -> (std::process::Child, std::os::unix::net::UnixStream) {
    let barrier = env.store().runtime_paths().sock_dir.join("drain-test.sock");
    let listener = std::os::unix::net::UnixListener::bind(&barrier).unwrap();
    listener.set_nonblocking(true).unwrap();
    let child = drain_command(env).env(failpoint, &barrier).spawn().unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        match listener.accept() {
            Ok((stream, _)) => return (child, stream),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < until =>
            {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(error) => panic!(
                "drainer did not reach the crash boundary: {error}; log: {:?}",
                std::fs::read_to_string(env.store().paths().hook_drainer_log())
            ),
        }
    }
}

#[test]
fn hung_apply_is_killed_and_the_next_frame_is_applied() {
    use std::io::{BufRead as _, BufReader, Read as _};
    use std::os::unix::net::{UnixListener, UnixStream};

    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let barrier = store.runtime_paths().sock_dir.join("hung-apply.sock");
    let listener = UnixListener::bind(&barrier).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut frame = stop_frame(&env);
    frame
        .env
        .insert("RIMZ_TEST_HOOK_APPLY".into(), barrier.display().to_string());
    let (hung, _) = append_frame(&env, frame);
    let (next, through) = append_stop(&env);
    let mut drainer = drain_command(&env)
        .env("RIMZ_TEST_HOOK_APPLY_TIMEOUT_MS", "1000")
        .stderr(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(store.paths().hook_drainer_log())
                .unwrap(),
        )
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut blocked = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) => {
                assert!(Instant::now() < deadline, "apply did not hang: {error}");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };
    let children = rimz::proc::children(drainer.id());
    assert_eq!(children.len(), 1, "{children:?}");
    let child_pid = children[0];
    let mut request =
        UnixStream::connect(store.runtime_paths().hook_drainer_socket_path()).unwrap();
    request
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    writeln!(
        request,
        "{}",
        json!({"through": through, "reply_for": null})
    )
    .unwrap();
    let mut reply = String::new();
    let received = BufReader::new(request).read_line(&mut reply);
    drainer.kill().unwrap();
    drainer.wait().unwrap();
    assert!(
        received.is_ok(),
        "a hung apply must not block the next frame: {received:?}"
    );
    let receipt: Value = serde_json::from_str(&reply).unwrap();
    assert!(receipt["applied"].as_u64().unwrap() >= through);
    blocked
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    assert_eq!(
        blocked.read(&mut [0]).unwrap(),
        0,
        "hung child must close its socket"
    );
    assert!(
        !rimz::proc::process_is_live(child_pid, None),
        "hung child must be reaped"
    );
    let log = std::fs::read_to_string(store.paths().hook_drainer_log()).unwrap();
    assert!(
        log.contains("hook ingress apply failed") && log.contains("timed out"),
        "{log}"
    );
    assert!(derived(&env, &hung.event_id).is_empty());
    assert_eq!(derived(&env, &next.event_id).len(), 1);
    assert_eq!(
        ingress::read_cursor(store.paths()).unwrap(),
        HookDrainCursor::default()
    );
    assert_eq!(
        std::fs::metadata(&store.paths().hook_ingress_log)
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn leased_idle_drainer_blocks_without_progress_wakeups() {
    use std::io::{BufRead as _, BufReader, Read as _};
    use std::os::unix::net::{UnixListener, UnixStream};

    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let counter = store.runtime_paths().sock_dir.join("drain-ticks.sock");
    let listener = UnixListener::bind(&counter).unwrap();
    let mut drainer = drain_command(&env)
        .env("RIMZ_TEST_HOOK_DRAIN_TICKS", &counter)
        .env("RIMZ_TEST_HOOK_DRAIN_IDLE_MS", "50")
        .spawn()
        .unwrap();
    let (mut ticks, _) = listener.accept().unwrap();
    let lease = UnixStream::connect(store.runtime_paths().hook_drainer_socket_path()).unwrap();
    let mut request =
        UnixStream::connect(store.runtime_paths().hook_drainer_socket_path()).unwrap();
    writeln!(request, "{}", json!({"through": 0, "reply_for": null})).unwrap();
    request
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut reply = String::new();
    BufReader::new(request).read_line(&mut reply).unwrap();
    ticks.set_nonblocking(true).unwrap();
    while ticks.read(&mut [0; 1024]).is_ok_and(|count| count > 0) {}
    ticks.set_nonblocking(false).unwrap();
    ticks
        .set_read_timeout(Some(Duration::from_millis(150)))
        .unwrap();
    // One iteration can still enter its wait after the receipt reaches us.
    let mut wakeups = 0;
    while ticks.read(&mut [0]).is_ok_and(|count| count > 0) {
        wakeups += 1;
        if wakeups > 1 {
            break;
        }
    }
    let alive = drainer.try_wait().unwrap().is_none();
    drop(lease);
    drainer.kill().unwrap();
    drainer.wait().unwrap();
    assert!(alive, "the lease must keep the idle drainer alive");
    assert!(
        wakeups <= 1,
        "a leased idle drainer must block, not repoll: {wakeups} wakeups"
    );
}

#[test]
fn reset_stops_a_leased_drainer_and_allows_a_successor() {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let mut drainer = drain_command(&env).spawn().unwrap();
    let _lease = drainer_connection(&store);
    store.reset_records(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while drainer.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let stopped = drainer.try_wait().unwrap().is_some();
    if !stopped {
        drainer.kill().unwrap();
        drainer.wait().unwrap();
    }
    assert!(
        stopped,
        "reset must stop even a leased drainer before clearing ingress"
    );
    let mut hook = env.hook_command("claude");
    hook.args(["--event", "Stop"])
        .env("RIMZ_WORKSPACE_ID", env.workspace_id.as_str())
        .env("RIMZ_PROJECT_ROOT", &env.project_root);
    let output = env
        .spawn_payload(hook, &stop_frame(&env).payload)
        .wait_with_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let _successor = rimz::harness::hook_drain::DrainerLease::acquire(&store).unwrap();
    let (frame, through) = append_stop(&env);
    rimz::harness::hook_drain::drain_through(&store, through, None, Duration::from_secs(5))
        .unwrap();
    assert!(!derived(&env, &frame.event_id).is_empty());
}

#[test]
fn drainer_exits_without_unlinking_a_replacement_socket() {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let mut drainer = drain_command(&env).spawn().unwrap();
    let lease = drainer_connection(&store);
    let socket = store.runtime_paths().hook_drainer_socket_path();
    std::fs::remove_file(&socket).unwrap();
    let _replacement = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    drop(lease);
    let deadline = Instant::now() + Duration::from_secs(1);
    while drainer.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let stopped = drainer.try_wait().unwrap().is_some();
    if !stopped {
        drainer.kill().unwrap();
        drainer.wait().unwrap();
    }
    assert!(
        stopped,
        "a lost listener must stop the drainer despite its lease"
    );
    assert!(
        socket.exists(),
        "the old owner must not unlink a replacement listener"
    );
}

#[test]
fn drainer_discards_buffered_frames_after_its_cursor_is_reset() {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    append_stop(&env);
    let (stale, _) = append_stop(&env);
    let (mut drainer, mut release) = paused_drainer(&env, "RIMZ_TEST_HOOK_DRAIN_AFTER_APPLY");
    {
        let _workspace = WorkspaceLock::acquire(&store.paths().workspace_lock).unwrap();
        let _ingress = WorkspaceLock::acquire(&store.paths().hook_ingress_lock).unwrap();
        std::fs::remove_file(&store.paths().hook_ingress_log).unwrap();
        std::fs::remove_file(&store.paths().hook_drain_cursor).unwrap();
    }
    let (fresh, _) = append_stop(&env);
    release.write_all(&[1]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while drainer.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let stopped = drainer.try_wait().unwrap().is_some();
    if !stopped {
        drainer.kill().unwrap();
        drainer.wait().unwrap();
    }
    assert!(
        derived(&env, &stale.event_id).is_empty(),
        "buffered pre-reset work must not be applied"
    );
    assert!(stopped, "a cursor mismatch must stop its owner");
    once(&env);
    assert!(!derived(&env, &fresh.event_id).is_empty());
}

#[test]
fn crash_redo_records_the_keyed_native_answer() {
    let env = Env::new();
    env.record(&env.project_root);
    let mut ask = stop_frame(&env);
    ask.event = Some("PreToolUse".into());
    ask.payload = json!({
        "hook_event_name": "PreToolUse", "session_id": "native-replay", "tool_name": "AskUserQuestion", "tool_use_id": "ask-call",
        "tool_input": {"questions": [{"question": "Choose?", "options": [{"label": "safe"}]}]}
    }).to_string();
    let mut apply = env.rimz();
    apply
        .args(["hooks", "apply"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let output = env
        .spawn_payload(apply, &serde_json::to_string(&ask).unwrap())
        .wait_with_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let mut frame = stop_frame(&env);
    frame.event = Some("PostToolUse".into());
    frame.payload = json!({"session_id": "native-replay", "tool_name": "AskUserQuestion", "tool_use_id": "ask-call", "tool_response": {"answers": {"Choose?": "safe"}, "questions": [{"question": "Choose?", "options": [{"label": "safe"}]}]}}).to_string();
    append_frame(&env, frame);
    kill_apply_after_locked_phase(&env);
    once(&env);
    let entries = rimz::transcript::read_all(env.store().paths()).unwrap();
    let answers: Vec<_> = entries
        .iter()
        .filter(|entry| entry.entry == rimz::transcript::TranscriptKind::Answer)
        .collect();
    assert_eq!(
        answers.len(),
        1,
        "replay must finish the answer keyed by the native ask id"
    );
    assert!(answers[0].id.is_some());
}

#[test]
fn crash_redo_merges_a_keyed_locally_priced_cursor_turn() {
    let env = Env::new();
    env.record(&env.project_root);
    let pricing = env.runtime_paths().shared_pricing_cache_path();
    std::fs::create_dir_all(pricing.parent().unwrap()).unwrap();
    std::fs::write(pricing, json!({"schema": 4, "models": {"gpt-5.4": {"input": 0.000001, "output": 0.000005, "cache_read": 0.0000002, "cache_create": 0.0, "cache_read_explicit": true, "fast_multiplier": 1.0}}}).to_string()).unwrap();
    let mut frame = stop_frame(&env);
    frame.source = AgentKind::new_unchecked("cursor");
    frame.event = Some("stop".into());
    frame.payload = json!({"conversation_id": "priced-replay", "generation_id": "gen-replay", "status": "completed", "model_id": "gpt-5.4", "input_tokens": 1000, "output_tokens": 100}).to_string();
    append_frame(&env, frame);
    kill_apply_after_locked_phase(&env);
    once(&env);
    let record = rimz::store::agent_context::read_one(
        env.store().runtime_paths(),
        "cursor",
        "priced-replay",
    );
    assert!(
        record
            .as_ref()
            .is_some_and(|record| record.context.cost.is_some()),
        "replay must finish the cost merge keyed by generation id: {record:?}"
    );
}

#[test]
fn crash_redo_arms_the_registered_teams_subscription() {
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::write_definition(
        &env,
        "agents",
        "claude",
        "description: Claude base",
        "Follow instructions.",
    );
    crate::common::write_definition(
        &env,
        "agents",
        "worker",
        "description: Worker\nagent: claude\ntools: []",
        "",
    );
    crate::common::write_definition(
        &env,
        "teams",
        "forge",
        "leader: coder\nstages: [Build]\nroles:\n  - role: coder\n    agent: worker\n    owns: [Build]\n    signals: [{signal: probe.done, prompt: Continue.}]",
        "Complete the work.",
    );
    std::fs::write(
        env.project_root.join("blackboard.md"),
        "# Work\nStage: Build (@coder)\n",
    )
    .unwrap();
    let mut frame = stop_frame(&env);
    frame.event = Some("SessionStart".into());
    frame.payload = json!({"session_id": "subscription-replay"}).to_string();
    frame.env.extend([
        ("RIMZ_TEAM".into(), "forge".into()),
        ("RIMZ_AGENT_ROLE".into(), "coder".into()),
        ("RIMZ_AGENT_NAME".into(), "worker".into()),
        ("RIMZ_CHANNEL".into(), "alpha".into()),
    ]);
    append_frame(&env, frame);
    kill_apply_after_locked_phase(&env);
    once(&env);
    let path = env.store().paths().root.join("records/loop-instances.json");
    let tasks: BTreeMap<String, Value> =
        serde_json::from_slice(&std::fs::read(&path).unwrap_or_else(|error| {
            assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
            b"{}".to_vec()
        }))
        .unwrap();
    assert!(
        tasks.contains_key("team-forge-alpha-coder-probe-done"),
        "replay must arm a standing subscription, not only the stage delivery: {tasks:?}"
    );
}

fn kill_apply_after_locked_phase(env: &Env) {
    let (mut drainer, release) = paused_drainer(env, "RIMZ_TEST_HOOK_DRAIN_AFTER_LOCKED_APPLY");
    let children = rimz::proc::children(drainer.id());
    assert_eq!(children.len(), 1, "{children:?}");
    drainer.kill().unwrap();
    drainer.wait().unwrap();
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(children[0] as i32),
        nix::sys::signal::Signal::SIGKILL,
    )
    .unwrap();
    drop(release);
}

#[test]
fn crash_redo_records_the_prompt_after_the_lifecycle_batch() {
    let env = Env::new();
    env.record(&env.project_root);
    let mut frame = stop_frame(&env);
    frame.event = Some("UserPromptSubmit".into());
    frame.payload =
        json!({"session_id": "drain-session", "prompt": "keep this prompt"}).to_string();
    let (frame, _) = append_frame(&env, frame);
    kill_apply_after_locked_phase(&env);
    once(&env);
    let entries = rimz::transcript::read_all(env.store().paths()).unwrap();
    let prompts: Vec<_> = entries
        .iter()
        .filter(|entry| entry.entry == rimz::transcript::TranscriptKind::Prompt)
        .collect();
    assert_eq!(prompts.len(), 1, "replay must finish the prompt write");
    assert_eq!(prompts[0].text, "keep this prompt");
    assert_eq!(prompts[0].ingress.as_ref(), Some(&frame.event_id));
    assert_eq!(derived(&env, &frame.event_id).len(), 1);
}

#[test]
fn crash_redo_does_not_repeat_the_prompt_after_the_effects() {
    let env = Env::new();
    env.record(&env.project_root);
    let mut frame = stop_frame(&env);
    frame.event = Some("UserPromptSubmit".into());
    frame.payload =
        json!({"session_id": "drain-session", "prompt": "keep this prompt"}).to_string();
    let (frame, _) = append_frame(&env, frame);
    let (mut drainer, release) = paused_drainer(&env, "RIMZ_TEST_HOOK_DRAIN_AFTER_APPLY");
    let before = rimz::transcript::read_all(env.store().paths()).unwrap();
    assert_eq!(before.len(), 1);
    drainer.kill().unwrap();
    drainer.wait().unwrap();
    drop(release);
    once(&env);
    let after = rimz::transcript::read_all(env.store().paths()).unwrap();
    assert_eq!(after, before, "replay must not append its prompt again");
    assert_eq!(after[0].ingress.as_ref(), Some(&frame.event_id));
    assert_eq!(derived(&env, &frame.event_id).len(), 1);
}

#[test]
fn crash_redo_closes_the_working_span_at_the_ingress_time() {
    let env = Env::new();
    env.record(&env.project_root);
    let started = Timestamp::now() - Duration::from_secs(10);
    let mut prompt = stop_frame(&env);
    prompt.ts = started;
    prompt.event = Some("UserPromptSubmit".into());
    prompt.payload = json!({"session_id": "drain-session", "prompt": "work on this"}).to_string();
    let mut apply = env.rimz();
    apply
        .args(["hooks", "apply"])
        .stdin(std::process::Stdio::piped());
    let output = env
        .spawn_payload(apply, &serde_json::to_string(&prompt).unwrap())
        .wait_with_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let mut stop = stop_frame(&env);
    stop.ts = started + Duration::from_secs(10);
    let (stop, _) = append_frame(&env, stop);
    kill_apply_after_locked_phase(&env);
    once(&env);
    let store = env.store();
    let records = rimz::store::active_time::read_for_keys(
        store.runtime_paths(),
        [("claude", "drain-session")],
    );
    assert_eq!(records.len(), 1);
    assert!(!records[0].active, "replay must close the working span");
    assert_eq!(records[0].last_progress, stop.ts);
    assert_eq!(records[0].credited_ms, 10_000);
}

#[test]
fn crash_redo_does_not_repeat_an_envelopeless_cursor_transcript_entry() {
    let env = Env::new();
    env.record(&env.project_root);
    let mut frame = stop_frame(&env);
    frame.source = AgentKind::new_unchecked("cursor");
    frame.event = Some("afterAgentResponse".into());
    frame.payload =
        json!({"conversation_id": "cursor-reply", "text": "answer only once"}).to_string();
    let (frame, _) = append_frame(&env, frame);
    let (mut drainer, release) = paused_drainer(&env, "RIMZ_TEST_HOOK_DRAIN_AFTER_APPLY");
    let before = rimz::transcript::read_all(env.store().paths()).unwrap();
    assert_eq!(before.len(), 1);
    drainer.kill().unwrap();
    drainer.wait().unwrap();
    drop(release);
    once(&env);
    let after = rimz::transcript::read_all(env.store().paths()).unwrap();
    assert_eq!(
        after.len(),
        before.len(),
        "redo must recognize a frame with no lifecycle observation"
    );
    assert_eq!(
        serde_json::to_value(&after[0]).unwrap()["ingress"],
        json!(frame.event_id),
        "the assistant entry must carry its durable ingress key"
    );
    assert!(
        derived(&env, &frame.event_id).is_empty(),
        "a transcript-only frame must not append a marker event"
    );
}

#[test]
fn redo_after_crash_applies_each_frame_once() {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let mut observation = rimz::agents::AgentLifecycleObservation::new(
        Some("drain-session".into()),
        LifecycleSignal::Registered,
    );
    observation.launch.team = Some("drain-team".into());
    observation.worktree_path = Some(env.project_root.display().to_string());
    observation.runtime_owner = Some(rimz::store::runtime::process_owner(
        rimz::RuntimeOwnerKind::Agent,
        "drain-session",
        env.agent_owner_pid(),
    ));
    for signal in [
        LifecycleSignal::Registered,
        LifecycleSignal::TurnStarted { turn_id: None },
    ] {
        observation.signal = signal;
        store
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: "rimz-test",
                agent_kind: AgentKind::new_unchecked("claude"),
                event_name: "fixture",
                observation: &observation,
                spawned_subagents: &[],
            })
            .unwrap();
    }
    let mut run = rimz::store::run::RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        rimz::agents::PermissionMode::Auto,
        "finish".into(),
        env.project_root.clone(),
    );
    run.agent_id = Some("drain-session".into());
    run.status = rimz::store::run::RunStatus::Running;
    rimz::harness::run::create(store.paths(), &run).unwrap();
    let wake = std::os::unix::net::UnixDatagram::bind(
        store
            .runtime_paths()
            .sock_dir
            .join(format!("run.{}.sock", run.run_id.short())),
    )
    .unwrap();
    wake.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let mut frame = stop_frame(&env);
    frame
        .env
        .insert("RIMZ_RUN_ID".into(), run.run_id.to_string());
    let (frame, end) = append_frame(&env, frame);
    let (mut first, blocked) = paused_drainer(&env, "RIMZ_TEST_HOOK_DRAIN_AFTER_APPLY");
    let events = derived(&env, &frame.event_id);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind(), EventKind::AgentLifecycle(_)))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind(), EventKind::Signal(_)))
            .count(),
        1,
        "the normal path records team.idle once"
    );
    let cursor = ingress::read_cursor(store.paths()).unwrap();
    assert_eq!(
        (cursor.applied, cursor.claimed),
        (0, end),
        "signals fire before the applied cursor advances"
    );
    let mut bytes = [0; 1024];
    let received = wake.recv(&mut bytes).unwrap();
    let first_wake: Value = serde_json::from_slice(&bytes[..received]).unwrap();
    let completed = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
    assert_eq!(completed.status, rimz::store::run::RunStatus::Completed);
    let transcript = rimz::transcript::read_all(store.paths()).unwrap();
    assert!(
        !transcript.is_empty(),
        "the normal path records the unkeyed Stop text"
    );
    first.kill().unwrap();
    first.wait().unwrap();
    drop(blocked);
    once(&env);
    assert_eq!(
        derived(&env, &frame.event_id)
            .iter()
            .map(|event| &event.event_id)
            .collect::<Vec<_>>(),
        events
            .iter()
            .map(|event| &event.event_id)
            .collect::<Vec<_>>(),
        "redo must not append a second derived event"
    );
    let received = wake.recv(&mut bytes).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes[..received]).unwrap(),
        first_wake,
        "the idempotent terminal wake is resent"
    );
    assert_eq!(
        rimz::harness::run::load(store.paths(), &run.run_id).unwrap(),
        completed,
        "terminal settlement is sticky"
    );
    assert_eq!(
        rimz::transcript::read_all(store.paths()).unwrap().len(),
        transcript.len(),
        "redo skips unkeyed transcript appends"
    );

    let stage = Env::new();
    stage.record(&stage.project_root);
    stage.install_agent_hooks("claude");
    crate::common::write_definition(
        &stage,
        "agents",
        "claude",
        "description: Claude base",
        "Follow instructions.",
    );
    crate::common::write_definition(
        &stage,
        "agents",
        "worker",
        "description: Worker\nagent: claude\ntools: []",
        "",
    );
    crate::common::write_definition(
        &stage,
        "teams",
        "forge",
        "leader: coder\nstages: [Build]\nroles:\n  - role: coder\n    agent: worker\n    owns: [Build]",
        "Complete the work.",
    );
    std::fs::write(
        stage.project_root.join("blackboard.md"),
        "# Work\nStage: Build (@coder)\n",
    )
    .unwrap();
    let mut frame = stop_frame(&stage);
    frame.event = Some("SessionStart".into());
    frame.payload = json!({"session_id": "stage-session"}).to_string();
    frame.env.extend([
        ("RIMZ_TEAM".into(), "forge".into()),
        ("RIMZ_AGENT_ROLE".into(), "coder".into()),
        ("RIMZ_AGENT_NAME".into(), "worker".into()),
        ("RIMZ_CHANNEL".into(), "alpha".into()),
    ]);
    let (frame, _) = append_frame(&stage, frame);
    let (mut first, blocked) = paused_drainer(&stage, "RIMZ_TEST_HOOK_DRAIN_AFTER_LOCKED_APPLY");
    assert_eq!(derived(&stage, &frame.event_id).len(), 1);
    first.kill().unwrap();
    first.wait().unwrap();
    drop(blocked);
    once(&stage);
    let events = derived(&stage, &frame.event_id);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind(), EventKind::Signal(_))),
        "replayed stage re-wake must not append or fire a signal"
    );
    let notices = stage.store().list_pending_messages().unwrap();
    assert_eq!(
        notices.len(),
        1,
        "recovery re-runs the keyed stage delivery: {}",
        std::fs::read_to_string(stage.store().paths().hook_drainer_log()).unwrap_or_default()
    );
    assert!(matches!(
        notices[0].sender,
        rimz::store::message::MessageSender::Harness {
            notice: rimz::store::message::HarnessNotice::Stage
        }
    ));
}

#[test]
fn drained_log_is_truncated_and_cursor_reset() {
    let env = Env::new();
    env.record(&env.project_root);
    for (session, name, channel, pane_key, pane) in [
        ("first-session", "first", "alpha", "TMUX_PANE", "%45"),
        ("second-session", "second", "beta", "ZELLIJ_PANE_ID", "53"),
    ] {
        let mut frame = stop_frame(&env);
        frame.event = Some("SessionStart".into());
        frame.payload = json!({"session_id": session}).to_string();
        frame.env.insert("RIMZ_AGENT_NAME".into(), name.into());
        frame.env.insert("RIMZ_CHANNEL".into(), channel.into());
        frame.env.insert(pane_key.into(), pane.into());
        let home = env.project_root.join(format!("home-{name}"));
        let project = home.join("projects").join("room");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join(format!("{session}.jsonl")), "").unwrap();
        frame
            .env
            .insert("CLAUDE_CONFIG_DIR".into(), home.display().to_string());
        let (frame, _) = append_frame(&env, frame);
        // Both frames are queued before one drainer observes either environment.
        assert!(derived(&env, &frame.event_id).is_empty());
    }
    let output = drain_command(&env)
        .arg("--once")
        .env("RIMZ_TEST_HOOK_DRAIN_IDLE_MS", "1000")
        .env("CLAUDE_CODE_ENVIRONMENT_KIND", "bridge")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    assert_eq!(
        env.read_events()
            .iter()
            .filter(|event| event.ingress.is_some()
                && matches!(event.kind(), EventKind::AgentLifecycle(_)))
            .count(),
        2,
        "accepted frames must not inherit the drainer's ingress policy"
    );
    let store = env.store();
    let agents = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    for (session, name, channel, pane) in [
        ("first-session", "first", "alpha", "tmux:%45"),
        ("second-session", "second", "beta", "zellij:terminal_53"),
    ] {
        let agent = agents
            .iter()
            .find(|agent| agent.agent_id.as_str() == session)
            .unwrap();
        assert_eq!(agent.name.as_deref(), Some(name));
        assert_eq!(agent.channel.as_deref(), Some(channel));
        assert_eq!(
            rimz::store::agent_context::read_one(store.runtime_paths(), "claude", session)
                .and_then(|record| record.transcript_path),
            Some(
                env.project_root
                    .join(format!("home-{name}/projects/room/{session}.jsonl"))
                    .display()
                    .to_string()
            ),
            "context enrichment uses this frame's provider home"
        );
        assert_eq!(
            agent
                .pane
                .as_ref()
                .map(|pane| pane.pane_id.to_string())
                .as_deref(),
            Some(pane)
        );
    }
    assert_eq!(
        std::fs::metadata(&store.paths().hook_ingress_log)
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        ingress::read_cursor(store.paths()).unwrap(),
        HookDrainCursor::default()
    );
    use std::os::unix::fs::PermissionsExt as _;
    let socket = store.runtime_paths().hook_drainer_socket_path();
    assert_eq!(
        std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let mut reply_frame = stop_frame(&env);
    reply_frame.source = AgentKind::new_unchecked("cursor");
    reply_frame.event = Some("sessionStart".into());
    reply_frame.payload =
        json!({"conversation_id": "cursor-session", "workspace_roots": [&env.project_root]})
            .to_string();
    let (reply_frame, through) = append_frame(&env, reply_frame);
    let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    writeln!(
        stream,
        "{}",
        json!({"through": through, "reply_for": reply_frame.event_id})
    )
    .unwrap();
    use std::io::BufRead as _;
    let mut reply = String::new();
    std::io::BufReader::new(stream)
        .read_line(&mut reply)
        .unwrap();
    let reply: Value = serde_json::from_str(&reply).unwrap();
    assert!(reply["applied"].as_u64().unwrap() >= through);
    assert_eq!(
        reply["reply"],
        json!({}),
        "the decision reply stays on the private wire"
    );
    assert_eq!(
        std::fs::metadata(&store.paths().hook_ingress_log)
            .unwrap()
            .len(),
        0
    );
    let fallback = Env::new();
    fallback.record(&fallback.project_root);
    let (frame, _) = append_stop(&fallback);
    let output = drain_command(&fallback)
        .arg("--once")
        .env("RIMZ_BIN", fallback.project_root.join("missing-rimz"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    assert_eq!(
        derived(&fallback, &frame.event_id).len(),
        1,
        "a failed spawn drains inline"
    );
    assert!(
        !fallback
            .store()
            .runtime_paths()
            .hook_drainer_socket_path()
            .exists()
    );
}

#[test]
fn idle_exit_closes_the_socket_before_the_final_drain() {
    use std::os::unix::net::UnixListener;

    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let barrier = store.runtime_paths().sock_dir.join("final-drain.sock");
    let listener = UnixListener::bind(&barrier).unwrap();
    let before = store
        .runtime_paths()
        .sock_dir
        .join("before-final-drain.sock");
    let before_listener = UnixListener::bind(&before).unwrap();
    let mut drainer = drain_command(&env)
        .env("RIMZ_TEST_HOOK_DRAIN_IDLE_MS", "50")
        .env("RIMZ_TEST_HOOK_DRAIN_BEFORE_FINAL_DRAIN", &before)
        .env("RIMZ_TEST_HOOK_DRAIN_AFTER_FINAL_DRAIN", &barrier)
        .spawn()
        .unwrap();
    let mut start = apply_barrier(&before_listener);
    let socket = store.runtime_paths().hook_drainer_socket_path();
    assert!(
        !socket.exists(),
        "the socket must already be gone before the final drain starts"
    );
    start.write_all(&[1]).unwrap();
    let mut release = apply_barrier(&listener);
    assert!(
        !socket.exists(),
        "the socket must already be gone when the final drain has read to end"
    );
    let output = env
        .spawn_payload(
            env.hook_command("claude"),
            &json!({"hook_event_name": "Stop", "session_id": "after-final-drain"}).to_string(),
        )
        .wait_with_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    release.write_all(&[1]).unwrap();
    assert!(drainer.wait().unwrap().success());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if env.read_events().iter().any(|event| {
            event.method == "agent.lifecycle"
                && event.params_value()["agent_id"] == "after-final-drain"
                && event.ingress.is_some()
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "a hook after the final empty read must be applied without another nudge"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(socket.exists(), "the arriving hook must elect a successor");
}

#[test]
fn idle_drainer_exits_and_the_next_hook_spawns_a_successor() {
    let env = Env::new();
    env.record(&env.project_root);
    let feed = |session| {
        let mut command = env.hook_command("claude");
        command.env("RIMZ_TEST_HOOK_DRAIN_IDLE_MS", "1000");
        let output = env
            .spawn_payload(
                command,
                &json!({"hook_event_name": "Stop", "session_id": session}).to_string(),
            )
            .wait_with_output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        env.drain_hooks();
        let event = env
            .read_events()
            .into_iter()
            .find(|event| {
                event.method == "agent.lifecycle" && event.params_value()["agent_id"] == session
            })
            .unwrap();
        assert!(
            event.ingress.is_some(),
            "the hook queues its frame before the drainer applies it"
        );
        event.ingress.unwrap()
    };
    let first = feed("idle-first");
    assert_eq!(derived(&env, &first).len(), 1);
    let socket = env.store().runtime_paths().hook_drainer_socket_path();
    assert!(socket.exists(), "the first drain must elect a resident");
    let deadline = Instant::now() + Duration::from_secs(3);
    while socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!socket.exists(), "idle drainer must unlink its socket");
    let next = feed("idle-next");
    assert!(!derived(&env, &next).is_empty());
    assert!(socket.exists(), "the next frame starts a successor");
}

#[test]
fn two_starters_elect_one_drainer() {
    let env = Env::new();
    env.record(&env.project_root);
    let (frame, _) = append_stop(&env);
    let mut first = drain_command(&env)
        .env("RIMZ_TEST_HOOK_DRAIN_IDLE_MS", "500")
        .spawn()
        .unwrap();
    let mut second = drain_command(&env)
        .env("RIMZ_TEST_HOOK_DRAIN_IDLE_MS", "500")
        .spawn()
        .unwrap();
    assert!(first.wait().unwrap().success());
    assert!(second.wait().unwrap().success());
    assert_eq!(
        derived(&env, &frame.event_id).len(),
        1,
        "one elected drainer applies the frame"
    );
    assert!(
        WorkspaceLock::try_acquire(&env.store().runtime_paths().hook_drainer_lock())
            .unwrap()
            .is_some()
    );
}

#[test]
fn stop_appends_one_frame_and_the_drainer_derives_the_lifecycle_event() {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let lifetime = WorkspaceLock::acquire(&store.runtime_paths().hook_drainer_lock()).unwrap();
    let socket =
        std::os::unix::net::UnixListener::bind(store.runtime_paths().hook_drainer_socket_path())
            .unwrap();
    let before = env.read_events();
    let output = env
        .spawn_hook(
            "codex",
            &json!({
                "hook_event_name": "Stop", "session_id": "queued-stop",
                "last_assistant_message": "finished"
            })
            .to_string(),
        )
        .wait_with_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert_eq!(
        env.read_events(),
        before,
        "the hook must not fold lifecycle state"
    );
    let (frames, end) = ingress::read_from_offset(store.paths(), 0).unwrap();
    assert_eq!(frames.len(), 1, "one raw frame per accepted hook");
    assert_eq!(frames[0].0.source.as_str(), "codex");
    assert_eq!(
        frames[0].0.env["RIMZ_AGENT_PID"],
        env.agent_owner_pid().to_string()
    );
    drop(socket);
    std::fs::remove_file(store.runtime_paths().hook_drainer_socket_path()).unwrap();
    drop(lifetime);
    env.drain_hooks();
    let events = derived(&env, &frames[0].0.event_id);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind(), EventKind::AgentLifecycle(_)))
            .count(),
        1,
        "{events:?}"
    );
    // A completed tail is acknowledged before the log and cursor are reset together.
    assert!(end > 0);
    assert_eq!(
        ingress::read_cursor(store.paths()).unwrap(),
        HookDrainCursor::default()
    );
}

#[test]
fn user_prompt_submit_reply_arrives_through_the_drainer() {
    for source in ["claude", "codex"] {
        let env = Env::new();
        crate::common::git::init_repo(&env.project_root);
        env.record(&env.project_root);
        let mut command = env.hook_command(source);
        command.env("RIMZ_RUNTIME_ENV", "1");
        let output = env.spawn_payload(command, &json!({
            "hook_event_name": "UserPromptSubmit", "session_id": "reply-root", "prompt": "question"
        }).to_string()).wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            reply["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap()
                .contains("$ git status --short --branch")
        );
        assert!(
            env.store()
                .runtime_paths()
                .hook_drainer_socket_path()
                .exists(),
            "the reply must come from the elected drainer"
        );
        assert!(
            env.read_events()
                .iter()
                .any(|event| event.ingress.is_some())
        );
    }
}

#[test]
fn user_prompt_submit_reply_is_neutral_when_the_drainer_is_late() {
    let env = Env::new();
    crate::common::git::init_repo(&env.project_root);
    env.record(&env.project_root);
    let store = env.store();
    let _lifetime = WorkspaceLock::acquire(&store.runtime_paths().hook_drainer_lock()).unwrap();
    let _socket =
        std::os::unix::net::UnixListener::bind(store.runtime_paths().hook_drainer_socket_path())
            .unwrap();
    let mut command = env.hook_command("claude");
    command.env("RIMZ_RUNTIME_ENV", "1");
    let output = env.spawn_payload(command, &json!({
        "hook_event_name": "UserPromptSubmit", "session_id": "late-root", "prompt": "question"
    }).to_string()).wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        output.stdout.is_empty(),
        "a late enrichment reply is neutral: {output:?}"
    );
    assert!(env.read_events().is_empty());
    assert_eq!(
        ingress::read_from_offset(store.paths(), 0).unwrap().0.len(),
        1
    );
}

#[test]
fn copilot_native_child_does_not_claim_its_parents_due_rung() {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let _lifetime = WorkspaceLock::acquire(&store.runtime_paths().hook_drainer_lock()).unwrap();
    let _socket =
        std::os::unix::net::UnixListener::bind(store.runtime_paths().hook_drainer_socket_path())
            .unwrap();
    let mut run = rimz::store::run::RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("copilot"),
        rimz::agents::PermissionMode::Auto,
        "task".into(),
        env.project_root.clone(),
    );
    run.agent_id = Some("parent-session".into());
    run.status = rimz::store::run::RunStatus::Running;
    run.timeout = Some(Duration::from_secs(1800));
    run.warn = vec![Duration::from_secs(360)];
    run.deadline_at = Some(Timestamp::now() + Duration::from_secs(299));
    rimz::harness::run::create(store.paths(), &run).unwrap();
    let mut hook = env.hook_command("copilot");
    hook.args(["--event", "postToolUse"])
        .env("RIMZ_RUN_ID", run.run_id.as_str());
    let output = env.spawn_payload(hook, &json!({"sessionId": "toolu_child", "toolName": "edit", "toolArgs": {"path": "source.rs"}}).to_string()).wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        output.stdout.is_empty(),
        "a native child with no agent_id must not receive its parent's deadline: {output:?}"
    );
    assert!(
        rimz::harness::run::load(store.paths(), &run.run_id)
            .unwrap()
            .deadline_notice_at
            .is_none()
    );
}

#[test]
fn post_tool_use_rung_is_answered_without_the_drainer() {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let _lifetime = WorkspaceLock::acquire(&store.runtime_paths().hook_drainer_lock()).unwrap();
    let _socket =
        std::os::unix::net::UnixListener::bind(store.runtime_paths().hook_drainer_socket_path())
            .unwrap();
    let mut run = rimz::store::run::RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        rimz::agents::PermissionMode::Auto,
        "task".into(),
        env.project_root.clone(),
    );
    run.status = rimz::store::run::RunStatus::Running;
    run.timeout = Some(Duration::from_secs(1800));
    run.warn = vec![Duration::from_secs(360), Duration::from_secs(180)];
    run.deadline_at = Some(Timestamp::now() + Duration::from_secs(299));
    rimz::harness::run::create(store.paths(), &run).unwrap();
    for (native_child, expects_reply) in [(true, false), (false, true), (false, false)] {
        let mut command = env.hook_command("claude");
        command.env("RIMZ_RUN_ID", run.run_id.as_str());
        let mut payload = json!({"hook_event_name": "PostToolUse", "session_id": "rung-root", "tool_name": "Read", "tool_input": {}, "tool_response": {}});
        if native_child {
            payload["agent_id"] = json!("native-child");
        }
        let output = env
            .spawn_payload(command, &payload.to_string())
            .wait_with_output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(!output.stdout.is_empty(), expects_reply, "{output:?}");
        if expects_reply {
            let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(
                reply["hookSpecificOutput"]["additionalContext"]
                    .as_str()
                    .unwrap()
                    .starts_with("4m of 30m left.")
            );
        }
    }
    assert!(
        env.read_events().is_empty(),
        "rung selection must not apply lifecycle effects"
    );
    assert_eq!(
        ingress::read_from_offset(store.paths(), 0).unwrap().0.len(),
        3
    );
}

#[test]
fn cursor_hook_prints_its_payload_reply_without_the_drainer() {
    payload_reply_without_drainer("cursor", "sessionStart", json!({}));
}

#[test]
fn leased_drainer_does_not_retain_unrequested_replies() {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let mut drainer = drain_command(&env)
        .env("RIMZ_TEST_HOOK_REPLY_RETENTION_MS", "0")
        .spawn()
        .unwrap();
    let socket = store.runtime_paths().hook_drainer_socket_path();
    let deadline = Instant::now() + Duration::from_secs(5);
    let _lease = loop {
        match std::os::unix::net::UnixStream::connect(&socket) {
            Ok(lease) => break lease,
            Err(error) => {
                assert!(Instant::now() < deadline, "drainer did not listen: {error}");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };
    for _ in 0..3 {
        let mut frame = stop_frame(&env);
        frame.source = AgentKind::new_unchecked("cursor");
        frame.event = Some("sessionStart".into());
        frame.payload = json!({"session_id": "unrequested-reply"}).to_string();
        let (frame, end) = append_frame(&env, frame);
        rimz::harness::hook_drain::drain_through(&store, end, None, Duration::from_secs(30))
            .unwrap();
        let receipt = rimz::harness::hook_drain::drain_through(
            &store,
            end,
            Some(frame.event_id),
            Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(
            receipt.reply, None,
            "a lease must not preserve orphan replies"
        );
    }
    drainer.kill().unwrap();
    drainer.wait().unwrap();
}

#[test]
fn antigravity_hook_reply_arrives_through_the_drainer() {
    let env = Env::new();
    env.record(&env.project_root);
    let output = env.spawn_hook("antigravity", &json!({"hook_event_name": "Stop", "conversation_id": "native-reply", "invocationNum": 0}).to_string()).wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"decision": ""})
    );
    assert!(
        env.store()
            .runtime_paths()
            .hook_drainer_socket_path()
            .exists(),
        "Antigravity must wait for its provider-reading decode"
    );
}

#[test]
fn antigravity_reply_is_native_neutral_when_the_drainer_is_late() {
    payload_reply_without_drainer("antigravity", "PreInvocation", json!({}));
    payload_reply_without_drainer("antigravity", "Stop", json!({"decision": ""}));
}

fn payload_reply_without_drainer(source: &str, event: &str, expected: Value) {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let _lifetime = WorkspaceLock::acquire(&store.runtime_paths().hook_drainer_lock()).unwrap();
    let _socket =
        std::os::unix::net::UnixListener::bind(store.runtime_paths().hook_drainer_socket_path())
            .unwrap();
    let output = env.spawn_hook(source, &json!({"hook_event_name": event, "conversation_id": "payload-root", "session_id": "payload-root", "conversationId": "payload-root", "invocationNum": 0}).to_string()).wait_with_output().unwrap();
    assert!(output.status.success(), "{source}: {output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{expected}\n"),
        "{source}: native JSON must survive unavailable lifecycle decode"
    );
    assert_eq!(
        ingress::read_from_offset(store.paths(), 0).unwrap().0.len(),
        1,
        "reply must leave lifecycle work queued"
    );
    assert!(env.read_events().is_empty());
}

#[test]
fn failed_drainer_spawn_applies_the_ingress_inline() {
    let env = Env::new();
    env.record(&env.project_root);
    let mut command = env.hook_command("claude");
    command.env("RIMZ_BIN", env.project_root.join("missing-rimz"));
    let output = env
        .spawn_payload(
            command,
            &json!({"hook_event_name": "Stop", "session_id": "inline-stop"}).to_string(),
        )
        .wait_with_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(
        env.read_events()
            .iter()
            .any(|event| event.method == "agent.lifecycle" && event.ingress.is_some()),
        "a failed spawn must still apply the queued ingress"
    );
    assert!(
        !env.store()
            .runtime_paths()
            .hook_drainer_socket_path()
            .exists()
    );
}

#[test]
fn failed_inline_drain_keeps_the_durable_hook_successful() {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let lifetime = WorkspaceLock::acquire(&store.runtime_paths().hook_drainer_lock()).unwrap();
    let mut command = env.hook_command("cursor");
    command.env("RIMZ_BIN", env.project_root.join("missing-rimz"));
    let output = env
        .spawn_payload(
            command,
            &json!({"hook_event_name": "sessionStart", "conversation_id": "durable-hook"})
                .to_string(),
        )
        .wait_with_output()
        .unwrap();
    assert!(
        output.status.success(),
        "a drainer error after append must not fail the hook: {output:?}"
    );
    insta::assert_snapshot!(String::from_utf8(output.stdout).unwrap(), @"{}\n");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("inline drain failed"),
        "{:?}",
        output.stderr
    );
    let (frames, _) = ingress::read_from_offset(store.paths(), 0).unwrap();
    assert_eq!(frames.len(), 1);
    assert!(env.read_events().is_empty());
    drop(lifetime);
    env.drain_hooks();
    assert_eq!(derived(&env, &frames[0].0.event_id).len(), 1);
}

#[test]
fn apply_child_returns_the_native_reply_and_stamps_its_frame() {
    let env = Env::new();
    env.record(&env.project_root);
    let mut frame = stop_frame(&env);
    frame.source = AgentKind::new_unchecked("antigravity");
    frame.payload = json!({"conversation_id": "child-reply", "fullyIdle": true}).to_string();
    let mut command = env.rimz();
    command
        .args(["hooks", "apply"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = env
        .spawn_payload(
            command,
            &format!("{}\n", serde_json::to_string(&frame).unwrap()),
        )
        .wait_with_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "{\"decision\":\"\"}\n",
        "the apply child owns decode and its native reply"
    );
    assert!(!derived(&env, &frame.event_id).is_empty());
}

#[test]
fn ingress_captures_only_the_declared_environment() {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let _lifetime = WorkspaceLock::acquire(&store.runtime_paths().hook_drainer_lock()).unwrap();
    let _socket =
        std::os::unix::net::UnixListener::bind(store.runtime_paths().hook_drainer_socket_path())
            .unwrap();
    let mut command = env.hook_command("claude");
    command
        .env("RIMZ_CUSTOM_PIN", "captured")
        .env("PI_AGENT_DIR", "/tmp/pi")
        .env("ZELLIJ_SOCKET_DIR", "/tmp/hook-zellij")
        .env("TMUX", "/tmp/hook-tmux/default,123,0")
        .env("UNDECLARED_TOKEN", "not-captured");
    let output = env
        .spawn_payload(
            command,
            &json!({"hook_event_name": "Stop", "session_id": "env-capture"}).to_string(),
        )
        .wait_with_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let (frames, _) = ingress::read_from_offset(store.paths(), 0).unwrap();
    let captured = &frames[0].0.env;
    assert_eq!(
        captured.get("RIMZ_CUSTOM_PIN").map(String::as_str),
        Some("captured"),
        "every RimZ pin is captured"
    );
    assert_eq!(
        captured.get("PI_AGENT_DIR").map(String::as_str),
        Some("/tmp/pi")
    );
    assert!(captured.contains_key("PATH"));
    assert_eq!(
        captured.get("ZELLIJ_SOCKET_DIR").map(String::as_str),
        Some("/tmp/hook-zellij")
    );
    assert_eq!(
        captured.get("TMUX").map(String::as_str),
        Some("/tmp/hook-tmux/default,123,0")
    );
    assert!(!captured.contains_key("UNDECLARED_TOKEN"));
}

#[test]
fn sandbox_hook_without_a_host_drainer_applies_inline_without_ingress() {
    let env = Env::new();
    env.record(&env.project_root);
    let mut command = env.hook_command("claude");
    command
        .env("RIMZ_ISOLATION", "sandbox")
        .env("TMPDIR", "/tmp");
    let output = env
        .spawn_payload(
            command,
            &json!({"hook_event_name": "Stop", "session_id": "sandbox-inline"}).to_string(),
        )
        .wait_with_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        ingress::read_from_offset(env.store().paths(), 0)
            .unwrap()
            .0
            .is_empty(),
        "an unreachable host drainer must not orphan a sandbox frame"
    );
    assert!(
        env.read_events()
            .iter()
            .any(|event| event.method == "agent.lifecycle" && event.ingress.is_none()),
        "fallback uses the caller's native view"
    );
    assert!(
        !env.store()
            .runtime_paths()
            .hook_drainer_socket_path()
            .exists(),
        "a sandbox hook must never spawn a worker"
    );
}

#[test]
fn sandbox_hook_without_the_mount_pin_applies_inline_with_a_live_drainer() {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let mut drainer = drain_command(&env).spawn().unwrap();
    let lease = drainer_connection(&store);
    let ingress_before = std::fs::read(&store.paths().hook_ingress_log).ok();
    let mut command = env.hook_command("claude");
    command
        .env("RIMZ_ISOLATION", "sandbox")
        .env_remove(rimz::sandbox::HOOK_HOST_PATHS_ENV)
        .env("TMPDIR", "/tmp");
    let output = env
        .spawn_payload(
            command,
            &json!({"hook_event_name": "Stop", "session_id": "sandbox-unpinned"}).to_string(),
        )
        .wait_with_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read(&store.paths().hook_ingress_log).ok(),
        ingress_before,
        "a caller without the mount pin must not append, even with a reachable drainer"
    );
    assert!(
        env.read_events().iter().any(|event| {
            event.method == "agent.lifecycle"
                && event.params_value()["agent_id"] == "sandbox-unpinned"
                && event.ingress.is_none()
        }),
        "the unpinned hook must derive its lifecycle in the caller's view"
    );
    assert!(drainer.try_wait().unwrap().is_none());
    drop(lease);
    drainer.kill().unwrap();
    drainer.wait().unwrap();
}

#[test]
fn sandbox_frame_without_the_mount_pin_is_diagnosed_not_applied() {
    let env = Env::new();
    env.record(&env.project_root);
    let mut frame = stop_frame(&env);
    frame.env.insert("RIMZ_ISOLATION".into(), "sandbox".into());
    frame.env.remove(rimz::sandbox::HOOK_HOST_PATHS_ENV);
    let (frame, _) = append_frame(&env, frame);
    once(&env);
    assert!(
        derived(&env, &frame.event_id).is_empty(),
        "a sandbox frame without its captured mount pin must not apply"
    );
    let diagnostic =
        std::fs::read_to_string(env.store().paths().audit_path("hook-drain.log.jsonl"))
            .unwrap_or_default();
    assert!(
        diagnostic.contains("sandbox hook requires RIMZ_SANDBOX_HOST_PATHS"),
        "{diagnostic}"
    );
}

#[test]
fn sandbox_apply_preserves_the_mount_plans_tmp_checkout_and_provider_home() {
    use rimz::sandbox::{EnvPin, Mount, ProviderHome, SandboxInputs, SkillInputs};

    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let checkout = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let day = home.path().join("sessions/2026/06/11");
    std::fs::create_dir_all(&day).unwrap();
    std::fs::write(day.join("rollout-2026-06-11T07-18-00-bound-session.jsonl"), format!("{}\n", json!({"timestamp": "2026-06-11T07:18:00.000Z", "type": "event_msg", "payload": {"type": "turn_error", "message": "You've hit your usage limit", "codexErrorInfo": "usageLimitExceeded"}}))).unwrap();
    let mut frame = stop_frame(&env);
    frame.source = AgentKind::new_unchecked("codex");
    frame.cwd = checkout.path().into();
    frame.payload = json!({"session_id": "bound-session"}).to_string();
    frame.env.insert("RIMZ_ISOLATION".into(), "sandbox".into());
    frame.env.insert("RIMZ_AGENT_NAME".into(), "scout".into());
    frame
        .env
        .insert("CODEX_HOME".into(), home.path().display().to_string());
    let plan = rimz::sandbox::plan(&SandboxInputs {
        env: &frame.env,
        cwd: &frame.cwd,
        project_root: &env.project_root,
        worktree: Some(checkout.path()),
        tmp_dir: &store.paths().temp_unit_dir(Some("scout")),
        skills_dir: &checkout.path().join("skills"),
        provider_home: Some(ProviderHome {
            source: home.path().into(),
            target: home.path().into(),
        }),
        provider_home_env_keys: &["CODEX_HOME"],
        default_home: None,
        skills: SkillInputs {
            kind: "codex",
            home: None,
            manual: rimz::agents::ManualSkill::Frontmatter,
            callable: None,
        },
    })
    .unwrap();
    let bound: Vec<_> = plan
        .plan
        .mounts
        .iter()
        .filter_map(|mount| match mount {
            Mount::Bind { source, target } | Mount::RoBind { source, target }
                if source == target =>
            {
                Some(target)
            }
            _ => None,
        })
        .collect();
    frame.env.insert(
        "RIMZ_SANDBOX_HOST_PATHS".into(),
        serde_json::to_string(&bound).unwrap(),
    );
    for (key, value) in plan.pins {
        match value {
            EnvPin::Set(value) => {
                frame.env.insert(key, value);
            }
            EnvPin::Unset => {
                frame.env.remove(&key);
            }
        }
    }
    #[cfg(target_os = "linux")]
    let barrier = {
        let path = store.runtime_paths().sock_dir.join("bound-apply.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        frame
            .env
            .insert("RIMZ_TEST_HOOK_APPLY".into(), path.display().to_string());
        listener
    };
    let (frame, _) = append_frame(&env, frame);
    let child = drain_command(&env).arg("--once").spawn().unwrap();
    #[cfg(target_os = "linux")]
    let actual_cwd = {
        let mut release = apply_barrier(&barrier);
        let pid =
            nix::sys::socket::getsockopt(&release, nix::sys::socket::sockopt::PeerCredentials)
                .unwrap()
                .pid();
        let cwd = std::fs::read_link(format!("/proc/{pid}/cwd")).unwrap();
        release.write_all(&[1]).unwrap();
        cwd
    };
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    #[cfg(target_os = "linux")]
    assert_eq!(
        actual_cwd,
        checkout.path(),
        "apply must use the host-bound worktree, not a temp-unit rewrite or fallback"
    );
    let error = derived(&env, &frame.event_id)
        .into_iter()
        .find_map(|event| match event.kind() {
            EventKind::AgentLifecycle(payload) => match payload.observation.signal {
                LifecycleSignal::TurnEnded { errored, .. } => Some(errored),
                _ => None,
            },
            _ => None,
        });
    assert_eq!(
        error,
        Some(true),
        "provider decode must read its host-bound home"
    );
}

#[test]
fn sandbox_hook_nudges_the_host_and_maps_provider_paths_once() {
    use rimz::sandbox::{EnvPin, SandboxInputs, SkillInputs};

    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let unit = store.paths().temp_unit_dir(Some("scout"));
    std::fs::create_dir_all(&unit).unwrap();
    let transcript = unit.join("transcript.jsonl");
    #[cfg(target_os = "linux")]
    let barrier = {
        let listener = std::os::unix::net::UnixListener::bind(
            store.runtime_paths().sock_dir.join("apply-env.sock"),
        )
        .unwrap();
        listener.set_nonblocking(true).unwrap();
        listener
    };
    std::fs::write(&transcript, format!("{}\n", json!({"timestamp": "2026-06-11T07:18:00.000Z", "type": "event_msg", "payload": {"type": "turn_error", "message": "You've hit your usage limit", "codexErrorInfo": "usageLimitExceeded"}}))).unwrap();
    let mut drainer = drain_command(&env)
        .env("RIMZ_TEST_HOOK_DRAIN_IDLE_MS", "1000")
        .env("UNDECLARED_TOKEN", "worker-only")
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let lease = loop {
        if let Ok(stream) = std::os::unix::net::UnixStream::connect(
            store.runtime_paths().hook_drainer_socket_path(),
        ) {
            break stream;
        }
        assert!(Instant::now() < deadline, "host drainer did not bind");
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut command = env.hook_command("codex");
    command
        .env("RIMZ_ISOLATION", "sandbox")
        .env("RIMZ_AGENT_NAME", "scout")
        .env("TMPDIR", "/tmp")
        .env("CODEX_HOME", "/tmp/account")
        .env("RIMZ_BIN", env.project_root.join("must-not-spawn"));
    let captured = command
        .get_envs()
        .filter_map(|(key, value)| {
            value.map(|value| {
                (
                    key.to_str().unwrap().to_owned(),
                    value.to_str().unwrap().to_owned(),
                )
            })
        })
        .collect();
    let plan = rimz::sandbox::plan(&SandboxInputs {
        env: &captured,
        cwd: &env.project_root,
        project_root: &env.project_root,
        worktree: None,
        tmp_dir: &unit,
        skills_dir: &unit,
        provider_home: None,
        provider_home_env_keys: &["CODEX_HOME"],
        default_home: None,
        skills: SkillInputs {
            kind: "codex",
            home: None,
            manual: rimz::agents::ManualSkill::Frontmatter,
            callable: None,
        },
    })
    .unwrap();
    let EnvPin::Set(pin) = &plan.pins[rimz::sandbox::HOOK_HOST_PATHS_ENV] else {
        panic!("sandbox planner must pin its identity binds");
    };
    command.env(rimz::sandbox::HOOK_HOST_PATHS_ENV, pin);
    #[cfg(target_os = "linux")]
    command.env(
        "RIMZ_TEST_HOOK_APPLY",
        store.runtime_paths().sock_dir.join("apply-env.sock"),
    );
    let output = env.spawn_payload(command, &json!({"hook_event_name": "Stop", "session_id": "sandbox-queued", "transcript_path": "/tmp/transcript.jsonl"}).to_string()).wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    #[cfg(target_os = "linux")]
    {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut release = loop {
            match barrier.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("apply child did not reach its environment boundary: {error}"),
            }
        };
        let pid =
            nix::sys::socket::getsockopt(&release, nix::sys::socket::sockopt::PeerCredentials)
                .unwrap()
                .pid();
        assert_ne!(
            pid as u32,
            drainer.id(),
            "provider decode must run in a child"
        );
        let environ = std::fs::read(format!("/proc/{pid}/environ")).unwrap();
        let values: BTreeMap<_, _> = environ
            .split(|byte| *byte == 0)
            .filter_map(|entry| std::str::from_utf8(entry).unwrap().split_once('='))
            .collect();
        assert_eq!(values.get("TMPDIR").copied(), unit.to_str());
        assert_eq!(
            values.get("CODEX_HOME").copied(),
            unit.join("account").to_str()
        );
        assert_eq!(values.get("RIMZ_ISOLATION").copied(), Some("sandbox"));
        assert!(
            !values.contains_key("UNDECLARED_TOKEN"),
            "the child environment must be cleared before captured values are installed"
        );
        release.write_all(&[1]).unwrap();
    }
    env.drain_hooks();
    let observed = env
        .read_events()
        .into_iter()
        .find_map(|event| match event.kind() {
            EventKind::AgentLifecycle(payload)
                if payload
                    .observation
                    .agent_id
                    .as_ref()
                    .is_some_and(|id| id.as_str() == "sandbox-queued") =>
            {
                Some((
                    payload.observation.signal.clone(),
                    payload.observation.transcript_path.clone(),
                    event.ingress.clone(),
                ))
            }
            _ => None,
        })
        .unwrap();
    assert!(
        matches!(observed.0, LifecycleSignal::TurnEnded { errored: true, .. }),
        "apply must read the host-side transcript: {observed:?}"
    );
    assert_eq!(observed.1.as_deref(), transcript.to_str());
    assert!(observed.2.is_some());
    drop(lease);
    drainer.kill().unwrap();
    drainer.wait().unwrap();
}

#[test]
fn an_inflight_apply_child_keeps_the_election_locked_after_drainer_death() {
    let env = Env::new();
    env.record(&env.project_root);
    let (frame, _) = append_stop(&env);
    let (mut drainer, mut release) =
        paused_drainer(&env, "RIMZ_TEST_HOOK_DRAIN_AFTER_LOCKED_APPLY");
    drainer.kill().unwrap();
    drainer.wait().unwrap();
    let lock =
        WorkspaceLock::try_acquire(&env.store().runtime_paths().hook_drainer_lock()).unwrap();
    assert!(
        lock.is_none(),
        "the election must not admit a successor while the old apply child can still write"
    );
    release.write_all(&[1]).unwrap();
    drop(release);
    env.drain_hooks();
    assert_eq!(
        derived(&env, &frame.event_id)
            .iter()
            .filter(|event| matches!(event.kind(), EventKind::AgentLifecycle(_)))
            .count(),
        1
    );
}
