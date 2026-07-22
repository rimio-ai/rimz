//! Integration coverage for `rimz events follow`.

use std::io::{BufRead, BufReader};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::Duration;

use crate::common::Env;
use rimz::agents::{AgentLifecycleObservation, LifecycleEvent, LifecycleSignal};
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::store::AgentLifecycleIntent;

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
    append(&env, LifecycleSignal::Registered);
    append(&env, LifecycleSignal::TurnStarted);

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
    assert_eq!(second.signal, LifecycleSignal::TurnStarted);
    assert_eq!(second.prior_status, Some(rimz::agents::AgentStatus::Idle));

    append(
        &env,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        },
    );
    let ended = next_event(&recv);
    assert_eq!(ended.status, rimz::agents::AgentStatus::Success);

    env.store()
        .rotate_event_log(1, None)
        .expect("rotate event log");
    append(&env, LifecycleSignal::TurnStarted);
    let resumed = next_event(&recv);
    assert_eq!(resumed.signal, LifecycleSignal::TurnStarted);
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
