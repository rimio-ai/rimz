//! Relative hook latency with an interleaved process-start baseline.

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
const ROUNDS: usize = 2;

fn burst(seeded: bool, round: usize) -> Vec<(Duration, Duration)> {
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
    let mut samples_and_owners: Vec<_> = std::thread::scope(|scope| {
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
                    let mut version = env.rimz();
                    version.arg("--version");
                    barrier.wait();
                    let mut baseline = || {
                        let start = Instant::now();
                        let output = version.output().unwrap();
                        let elapsed = start.elapsed();
                        assert!(output.status.success(), "{output:?}");
                        elapsed
                    };
                    let stop = || {
                        let start = Instant::now();
                        let output = env
                            .spawn_payload(hook, &payload)
                            .wait_with_output()
                            .unwrap();
                        let elapsed = start.elapsed();
                        assert!(output.status.success(), "{output:?}");
                        assert!(output.stdout.is_empty(), "{output:?}");
                        elapsed
                    };
                    let sample = if (round + slot).is_multiple_of(2) {
                        let hook = stop();
                        (hook, baseline())
                    } else {
                        let version = baseline();
                        (stop(), version)
                    };
                    (sample, owner)
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
    for (_, owner) in &mut samples_and_owners {
        owner.kill().unwrap();
        owner.wait().unwrap();
    }
    drainer.kill().unwrap();
    drainer.wait().unwrap();
    samples_and_owners
        .into_iter()
        .map(|(sample, _)| sample)
        .collect()
}

fn p99(mut samples: Vec<Duration>) -> Duration {
    samples.sort_unstable();
    samples[(samples.len() * 99).div_ceil(100) - 1]
}

#[test]
#[expect(
    clippy::print_stdout,
    reason = "performance evidence includes both interleaved baselines"
)]
fn concurrent_stops_hold_the_relative_budget() {
    let mut empty = Vec::new();
    let mut seeded = Vec::new();
    for round in 0..ROUNDS {
        empty.extend(burst(false, round));
        seeded.extend(burst(true, round));
    }
    let empty_hook = p99(empty.iter().map(|sample| sample.0).collect());
    let empty_version = p99(empty.iter().map(|sample| sample.1).collect());
    let seeded_hook = p99(seeded.iter().map(|sample| sample.0).collect());
    let seeded_version = p99(seeded.iter().map(|sample| sample.1).collect());
    let numbers = format!(
        "p99: empty hook {empty_hook:?}, version {empty_version:?}; seeded hook {seeded_hook:?}, version {seeded_version:?} ({} samples per side)",
        CONCURRENT * ROUNDS
    );
    println!("{numbers}");
    assert!(empty_hook <= empty_version * 2, "{numbers}");
    assert!(seeded_hook <= seeded_version * 2, "{numbers}");
    // Load during one burst moves hook and baseline together, and a quiet baseline burst moves
    // the baseline alone; a slower seeded hook exceeds the bound both raw and normalised.
    let relative = |hook: Duration, version: Duration| hook.as_secs_f64() / version.as_secs_f64();
    let raw = relative(seeded_hook, empty_hook);
    let normalised = relative(seeded_hook, seeded_version) / relative(empty_hook, empty_version);
    assert!(
        raw <= 1.25 || normalised <= 1.25,
        "seeded/empty hook p99 is {raw:.3}x raw and {normalised:.3}x normalised by each side's baseline; {numbers}"
    );
}
