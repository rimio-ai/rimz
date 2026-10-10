//! Concurrent Stop hooks land durably before returning and fold after drain-through.

use std::process::Command;
use std::sync::Barrier;
use std::time::{Duration, Instant};

use rimz::disk::lock::WorkspaceLock;
use rimz::store::event::EventKind;
use rimz::store::ingress;
use rimz::testkit::fleet::seed_fleet_store;
use serde_json::json;

use crate::common::Env;

const CONCURRENT: usize = 24;

fn burst(seeded: bool) {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    if seeded {
        seed_fleet_store(store.paths(), 40, 2_000).unwrap();
        store.snapshot().unwrap();
    }
    store.runtime_paths().ensure_dirs().unwrap();
    let socket = store.runtime_paths().hook_drainer_socket_path();
    let mut drainer = env
        .rimz()
        .args(["hooks", "drain", "--project-root"])
        .arg(&env.project_root)
        .spawn()
        .unwrap();
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
    let mut warm = env.hook_command("codex");
    warm.env("RIMZ_WORKSPACE_ID", env.workspace_id.as_str())
        .env("RIMZ_PROJECT_ROOT", &env.project_root);
    let output = env
        .spawn_payload(warm, &json!({"hook_event_name": "Stop", "session_id": "warmup", "last_assistant_message": "done"}).to_string())
        .wait_with_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    env.drain_hooks();
    let prior: std::collections::HashSet<_> = env
        .read_events()
        .into_iter()
        .filter_map(|event| event.ingress)
        .collect();
    let barrier = Barrier::new(CONCURRENT);
    let mut owners: Vec<_> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..CONCURRENT)
            .map(|slot| {
                let env = &env;
                let barrier = &barrier;
                scope.spawn(move || {
                    let owner = Command::new("sleep")
                        .arg("60")
                        .env_clear()
                        .env("HOME", &env.home_root)
                        .env("XDG_RUNTIME_DIR", &env.runtime_root)
                        .current_dir(&env.project_root)
                        .spawn()
                        .unwrap();
                    let payload = json!({
                        "hook_event_name": "Stop",
                        "session_id": format!("budget-{slot}"),
                        "turn_id": format!("turn-{slot}"),
                        "cwd": env.project_root,
                        "last_assistant_message": "done"
                    })
                    .to_string();
                    let mut hook = env.hook_command("codex");
                    hook.env("RIMZ_WORKSPACE_ID", env.workspace_id.as_str())
                        .env("RIMZ_PROJECT_ROOT", &env.project_root)
                        .env("RIMZ_AGENT_NAME", format!("budget-agent-{slot}"))
                        .env("TMUX_PANE", format!("%{slot}"))
                        .env("RIMZ_AGENT_PID", owner.id().to_string());
                    barrier.wait();
                    let output = env
                        .spawn_payload(hook, &payload)
                        .wait_with_output()
                        .unwrap();
                    assert!(output.status.success(), "{output:?}");
                    assert!(output.stdout.is_empty(), "{output:?}");
                    owner
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect()
    });
    let lock = WorkspaceLock::acquire(&store.paths().workspace_lock).unwrap();
    let (frames, _) = ingress::read_from_offset(store.paths(), 0).unwrap();
    let mut landed: std::collections::HashSet<_> = frames
        .into_iter()
        .map(|(frame, _)| frame.event_id)
        .collect();
    landed.extend(
        env.read_events()
            .iter()
            .filter_map(|event| event.ingress.clone()),
    );
    landed.retain(|id| !prior.contains(id));
    drop(lock);
    assert_eq!(
        landed.len(),
        CONCURRENT,
        "every Stop must land before return"
    );
    env.drain_hooks();
    assert_eq!(
        env.read_events()
            .iter()
            .filter(
                |event| event.ingress.as_ref().is_some_and(|id| !prior.contains(id))
                    && matches!(event.kind(), EventKind::AgentLifecycle(_))
            )
            .count(),
        CONCURRENT,
        "every queued Stop must derive exactly one lifecycle event"
    );
    let snapshot = store.snapshot().unwrap();
    assert_eq!(
        snapshot
            .agents
            .iter()
            .filter(|agent| agent.agent_id.as_str().starts_with("budget-"))
            .count(),
        CONCURRENT,
        "the consumer fold must see every Stop after drain-through: {:?}",
        snapshot
            .agents
            .iter()
            .map(|agent| (&agent.agent_id, &agent.name))
            .collect::<Vec<_>>()
    );
    for owner in &mut owners {
        owner.kill().unwrap();
        owner.wait().unwrap();
    }
    drainer.kill().unwrap();
    drainer.wait().unwrap();
}

#[test]
fn every_concurrent_stop_lands_and_folds() {
    burst(false);
    burst(true);
}
