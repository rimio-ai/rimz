//! Integration coverage for `rimz events follow`.

use std::io::{BufRead, BufReader};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::Duration;

use crate::common::Env;
use rimz::agents::{AgentLifecycleObservation, LifecycleEvent, LifecycleSignal};
use rimz::harness::schedule::signal::SignalSelector;
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::store::event::{SignalEventPayload, SignalName, SignalSource};
use rimz::store::writer::AgentLifecycleIntent;
use serde_json::json;

#[test]
fn signal_vocabulary_shape_and_cap_are_pinned() {
    let signal = SignalEventPayload {
        name: "deploy.done".parse().expect("signal name"),
        payload: json!({"a": 1}).as_object().expect("object payload").clone(),
        source: SignalSource::Cli,
    };
    assert_eq!(
        serde_json::to_value(&signal).expect("signal JSON"),
        json!({"name": "deploy.done", "payload": {"a": 1}, "source": "cli"})
    );
    assert!("x".repeat(64).parse::<SignalName>().is_ok());
    assert!("x".repeat(65).parse::<SignalName>().is_err());
    for family in ["agent", "wait", "team", "ci", "pr", "trunk", "worktree"] {
        assert!(
            format!("{family}.custom")
                .parse::<SignalName>()
                .unwrap()
                .is_reserved()
        );
    }
    assert_eq!(
        "x.*".parse::<SignalSelector>().expect("family selector"),
        SignalSelector::Family("x".into())
    );
    assert!(
        format!("{}.*", "x".repeat(63))
            .parse::<SignalSelector>()
            .is_err()
    );
    assert_eq!(
        format!("{}.*", "x".repeat(62))
            .parse::<SignalSelector>()
            .expect("64-byte selector"),
        SignalSelector::Family("x".repeat(62))
    );
    for (source, wire) in [
        (SignalSource::Cli, "cli"),
        (SignalSource::Watch, "watch"),
        (SignalSource::Lifecycle, "lifecycle"),
        (SignalSource::Forge, "forge"),
        (SignalSource::Git, "git"),
        (SignalSource::Worktree, "worktree"),
        (SignalSource::Team, "team"),
    ] {
        let serialized = serde_json::to_value(source).expect("signal source JSON");
        assert_eq!(serialized, json!(wire));
        assert_eq!(
            serde_json::from_value::<SignalSource>(serialized).expect("signal source"),
            source
        );
    }
}

#[test]
fn events_emit_is_durable_and_replayed_by_follow() {
    let env = Env::new();
    env.record(&env.project_root);
    let emitted = env
        .rimz()
        .args([
            "events",
            "emit",
            "deploy.finished",
            "--json",
            r#"{"revision":42}"#,
        ])
        .output()
        .expect("emit signal");
    assert!(
        emitted.status.success(),
        "{}",
        String::from_utf8_lossy(&emitted.stderr)
    );
    assert!(String::from_utf8_lossy(&emitted.stdout).contains("no task is subscribed to it"));

    let mut child = env
        .rimz()
        .args(["events", "follow", "--replay", "--json"])
        .env("RIMZ_EVENTS_POLL_MS", "5")
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn events follow");
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .expect("read signal event");
    child.kill().expect("stop events follower");
    child.wait().expect("reap events follower");
    let event: serde_json::Value = serde_json::from_str(&line).expect("signal JSON");
    assert_eq!(event["event"], "signal");
    assert_eq!(event["name"], "deploy.finished");
    assert_eq!(event["payload"]["revision"], 42);
}

#[test]
fn events_emit_rejects_reserved_names_and_non_object_payloads() {
    let env = Env::new();
    for (args, expected) in [
        (vec!["events", "emit", "agent.idle"], "reserved for RimZ"),
        (vec!["events", "emit", "wait.task"], "reserved for RimZ"),
        (
            vec!["events", "emit", "deploy.finished", "--json", "[]"],
            "must be a JSON object",
        ),
    ] {
        let output = env.rimz().args(args).output().expect("emit invalid signal");
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(expected), "{stderr}");
    }
}

#[test]
fn events_emit_distinguishes_started_disabled_and_sibling_subscriptions() {
    let env = Env::new();
    env.record(&env.project_root);
    std::fs::write(
        env.rimz_home().join("loop.toml"),
        format!(
            "[tasks.subscriber]\ncheck = \"true\"\nroot = {:?}\nsignal = \"deploy.done\"\n",
            env.project_root
        ),
    )
    .unwrap();
    let emit = |name| {
        let output = env.rimz().args(["events", "emit", name]).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    assert!(emit("deploy.other").contains("no task is subscribed to it"));
    let started = emit("deploy.done");
    assert!(
        started.contains("· fired 1 task\n  subscriber\n"),
        "{started}"
    );
    let output = env
        .rimz()
        .args(["loop", "disable", "subscriber"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let disabled = emit("deploy.done");
    assert!(
        disabled.contains("· fired no task\n  subscriber: did not start\n"),
        "{disabled}"
    );
}

fn append(env: &Env, signal: LifecycleSignal) {
    let store = env.store();
    let observation =
        AgentLifecycleObservation::new(Some(AgentSessionId::from("event-session")), signal);
    store
        .append_agent_lifecycle(AgentLifecycleIntent {
            session_name: "rimz-test",
            agent_kind: AgentKind::new_unchecked("claude"),
            event_name: "IntegrationEvent",
            observation: &observation,
            spawned_subagents: &[],
        })
        .expect("append lifecycle event");
}

#[test]
fn events_follow_replays_then_streams_across_rotation_without_a_gap() {
    let env = Env::new();
    env.record(&env.project_root);
    append(&env, LifecycleSignal::Registered);
    append(&env, LifecycleSignal::TurnStarted { turn_id: None });

    let mut child = env
        .rimz()
        .args(["events", "follow", "--replay", "--json"])
        .env("RIMZ_EVENTS_POLL_MS", "5")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn events follow");
    let stdout = child.stdout.take().expect("events stdout");
    let (send, recv) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if send.send(line).is_err() {
                break;
            }
        }
    });

    let first = next_event(&recv);
    let second = next_event(&recv);
    assert_eq!(first.signal, LifecycleSignal::Registered);
    assert_eq!(first.prior_status, None);
    assert_eq!(
        second.signal,
        LifecycleSignal::TurnStarted { turn_id: None }
    );
    assert_eq!(second.prior_status, Some(rimz::agents::AgentStatus::Idle));

    append(
        &env,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    );
    let ended = next_event(&recv);
    assert_eq!(ended.status, rimz::agents::AgentStatus::Success);

    env.store()
        .rotate_event_log(1, None)
        .expect("rotate event log");
    append(&env, LifecycleSignal::TurnStarted { turn_id: None });
    let resumed = next_event(&recv);
    assert_eq!(
        resumed.signal,
        LifecycleSignal::TurnStarted { turn_id: None }
    );
    assert_eq!(
        resumed.prior_status,
        Some(rimz::agents::AgentStatus::Success)
    );

    child.kill().expect("stop events follower");
    child.wait().expect("reap events follower");
    reader.join().expect("join events reader");
}

fn next_event(recv: &mpsc::Receiver<std::io::Result<String>>) -> LifecycleEvent {
    let line = recv
        .recv_timeout(Duration::from_secs(3))
        .expect("lifecycle event before timeout")
        .expect("read lifecycle event line");
    serde_json::from_str(&line).expect("lifecycle event JSON")
}

#[test]
fn events_follow_omits_snapshots_in_replay_and_live_output() {
    use rimz::agents::AgentStatus;

    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let record = |signal, snapshot: bool| {
        let mut observation =
            AgentLifecycleObservation::new(Some("snapshot-session".into()), signal);
        observation.state_snapshot = snapshot.then_some(true);
        store
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: "rimz-test",
                agent_kind: AgentKind::new_unchecked("opencode"),
                event_name: "IntegrationEvent",
                observation: &observation,
                spawned_subagents: &[],
            })
            .unwrap();
    };
    let start = LifecycleSignal::TurnStarted {
        turn_id: Some("work".into()),
    };
    let end = LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background: false,
        turn_id: Some("work".into()),
    };
    record(start.clone(), false);
    record(end.clone(), false);
    record(LifecycleSignal::Registered, true);
    record(LifecycleSignal::TurnStarted { turn_id: None }, true);
    record(end.clone(), false);
    let mut child = env
        .rimz()
        .args(["events", "follow", "--replay"])
        .env("RIMZ_EVENTS_POLL_MS", "5")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (send, recv) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    let mut events = (0..3).map(|_| next_event(&recv)).collect::<Vec<_>>();
    let next_start = LifecycleSignal::TurnStarted {
        turn_id: Some("next".into()),
    };
    record(LifecycleSignal::Registered, true);
    record(next_start.clone(), false);
    events.push(next_event(&recv));
    let tool = LifecycleSignal::ToolUsed {
        mutates: true,
        edits: false,
        name: Some("bash".into()),
        native_key: None,
        turn_id: Some("next".into()),
    };
    record(LifecycleSignal::TurnStarted { turn_id: None }, true);
    record(tool.clone(), false);
    events.push(next_event(&recv));
    child.kill().unwrap();
    child.wait().unwrap();
    reader.join().unwrap();
    assert_eq!(
        events
            .into_iter()
            .map(|event| (event.signal, event.prior_status, event.status))
            .collect::<Vec<_>>(),
        vec![
            (start, None, AgentStatus::Running),
            (
                end.clone(),
                Some(AgentStatus::Running),
                AgentStatus::Success
            ),
            (end, Some(AgentStatus::Running), AgentStatus::Success),
            (next_start, Some(AgentStatus::Success), AgentStatus::Running),
            (tool, Some(AgentStatus::Running), AgentStatus::Running),
        ]
    );
}

#[test]
fn events_list_filters_bounds_and_shares_follow_signal_lines() {
    let env = Env::new();
    env.record(&env.project_root);
    append(&env, LifecycleSignal::Registered);
    let store = env.store();
    for (name, seconds) in [("deploy.old", 7200), ("deploy.done", 10), ("ci.failed", 5)] {
        let mut event = rimz::store::event::EventEnvelope::new(
            rimz::ids::WorkspaceId::from_project_root(&env.project_root),
            "rimz-test",
            "test",
            "test",
            rimz::store::event::SIGNAL_METHOD,
            json!({"name": name, "payload": {"branch": "topic", "number": 7}, "source": "cli"}),
        );
        event.timestamp = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(seconds);
        rimz::store::event_log::append(&store.paths().events_log, &event).unwrap();
    }
    let list = |args: &[&str]| {
        let output = env
            .rimz()
            .args(["events", "list"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let rows = |text: &str| {
        text.lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>()
    };
    let all = rows(&list(&["--json"]));
    assert_eq!(
        all.iter()
            .map(|row| row["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ci.failed", "deploy.done", "deploy.old"]
    );
    assert_eq!(rows(&list(&["--json", "--signal", "deploy.*"])), all[1..]);
    assert_eq!(
        rows(&list(&["--json", "--signal", "deploy.done"])),
        all[1..2]
    );
    assert_eq!(rows(&list(&["--json", "--since", "1h"])), all[..2]);
    assert_eq!(
        rows(&list(&["--json", "--signal", "deploy.*", "-n", "1"])),
        all[1..2]
    );
    assert!(list(&["--json", "-n", "0"]).is_empty());
    let text = list(&[]);
    for expected in ["WHEN", "SIGNAL", "SOURCE", "SUBJECT", "on topic #7"] {
        assert!(text.contains(expected), "{text}");
    }
    let empty = list(&["--signal", "absent.*"]);
    assert!(
        empty.contains("no signal matched") && empty.contains("active log"),
        "{empty}"
    );

    let mut child = env
        .rimz()
        .args(["events", "follow", "--replay", "--signal", "deploy.*"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (send, recv) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    let followed: Vec<serde_json::Value> = (0..2)
        .map(|_| {
            let line = recv.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
            serde_json::from_str(&line).unwrap()
        })
        .collect();
    child.kill().unwrap();
    child.wait().unwrap();
    reader.join().unwrap();
    assert_eq!(followed, all[1..].iter().rev().cloned().collect::<Vec<_>>());
    assert!(
        recv.try_recv().is_err(),
        "filtered follower emitted an extra line"
    );

    store.rotate_event_log(1, None).unwrap();
    assert!(list(&["--json"]).is_empty(), "archives must not be listed");
    for _ in 0..22 {
        store
            .append_signal(
                "rimz-test",
                SignalEventPayload {
                    name: "deploy.done".parse().unwrap(),
                    payload: Default::default(),
                    source: SignalSource::Cli,
                },
            )
            .unwrap();
    }
    assert_eq!(rows(&list(&["--json"])).len(), 20);
    assert_eq!(rows(&list(&["--json", "--limit", "30"])).len(), 22);
}
