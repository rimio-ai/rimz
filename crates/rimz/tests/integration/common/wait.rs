//! Shared calling-session and armed-wait fixtures.

use super::Env;
use rimz::agents::{AgentLifecycleObservation, LaunchParams, LifecycleSignal};
use rimz::config::{TaskEntry, TaskTarget, Tasks};
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::pane::RuntimeOwner;
use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope};
use rimz::store::writer::AgentLifecycleIntent;

pub fn wait_ok(env: &Env, args: &[&str]) -> String {
    let output = agent_wait(env)
        .args(args)
        .output()
        .expect("run wait command");
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

pub fn loop_instances_path(env: &Env) -> std::path::PathBuf {
    env.state_path_for(&env.project_root)
        .root
        .join("records/loop-instances.json")
}

pub fn wait_instances(env: &Env) -> Tasks {
    let path = loop_instances_path(env);
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// Rewrites the room's `loop-instances.json` as the product's writers do: `edit` gets the rows
/// read under `loop-instances.lock` (a missing file reads as the default), and the result is
/// published whole by a rename. A starting `rimz shell watcher` strict-loads the file without
/// that lock, so a truncating write can hand it an empty file and lose the shell's outcome; and
/// a product write that runs beside the rewrite lands before its read or after its publish.
pub fn rewrite_loop_instances<T, R>(env: &Env, edit: impl FnOnce(&mut T) -> R) -> R
where
    T: serde::Serialize + serde::de::DeserializeOwned + Default,
{
    let state = env.state_path_for(&env.project_root);
    let lock = state.workspace_lock.with_file_name("loop-instances.lock");
    let _guard = rimz::disk::lock::WorkspaceLock::acquire(&lock).expect("loop instances lock");
    let path = state.root.join("records/loop-instances.json");
    let mut rows = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).expect("loop instances"),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => T::default(),
        Err(err) => panic!("read {}: {err}", path.display()),
    };
    let result = edit(&mut rows);
    std::fs::create_dir_all(path.parent().expect("records dir")).expect("mkdir records");
    rimz::disk::atomic::write_temp_then_rename_cache(&path, &rows).expect("publish loop instances");
    result
}

pub fn loop_runs_path(env: &Env) -> std::path::PathBuf {
    env.rimz_home().join("logs/loop-runs.log.jsonl")
}

/// The run log's records up to its last newline: a record still being appended has none yet.
pub fn read_loop_run_records(env: &Env) -> Vec<rimz::harness::schedule::run_log::LoopRunRecord> {
    let text = std::fs::read_to_string(loop_runs_path(env)).unwrap_or_default();
    text[..text.rfind('\n').map_or(0, |end| end + 1)]
        .lines()
        .map(|line| serde_json::from_str(line).expect("loop run record"))
        .collect()
}

/// Writes the delivery row that wakes the calling agent on `signal`, the shape a team
/// binding stores; `loop add` no longer arms one.
pub fn seed_signal_wait(env: &Env, name: &str, signal: &str, repeat: bool) {
    rewrite_loop_instances(env, |tasks: &mut Tasks| {
        tasks.0.insert(
            name.to_owned(),
            TaskEntry {
                root: env.project_root.clone(),
                signal: Some(signal.to_owned()),
                repeat,
                wait: Some(TaskTarget {
                    kind: AgentKind::new_unchecked("claude"),
                    session: AgentSessionId::from("provider-session"),
                    handle: "@planner#main".to_owned(),
                }),
                ..TaskEntry::default()
            },
        );
    });
}

/// Waits until no process in `env`'s sandbox runs `rimz loop run <task>`. A delivering
/// runner appends its row before it releases its run lock, and `loop show` reads that held
/// lock with no row as a run in flight, so a read or the next signal waits for its exit.
#[cfg(target_os = "linux")]
pub fn wait_for_loop_runs_to_exit(env: &Env, task: &str) {
    let spec = rimz::testkit::sandbox::SandboxSpec {
        home_root: env.home_root.clone(),
        runtime_root: env.runtime_root.clone(),
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while rimz::testkit::sandbox::sandbox_processes(&spec)
        .into_iter()
        .filter_map(rimz::proc::argv)
        .any(|argv| {
            ["loop", "run", task]
                .iter()
                .all(|word| argv.iter().any(|arg| arg == word))
        })
    {
        assert!(
            std::time::Instant::now() < deadline,
            "`rimz loop run {task}` did not exit"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

pub fn register_calling_agent(env: &Env) {
    register_calling_agent_with_launch(env, LaunchParams::default());
}

pub fn register_calling_agent_with_launch(env: &Env, launch: LaunchParams) {
    register_agent(env, "provider-session", "launch-session", "planner", launch);
}

pub fn register_agent(
    env: &Env,
    session_id: &str,
    launch_id: &str,
    name: &str,
    launch: LaunchParams,
) {
    register_agent_with_owner(env, session_id, launch_id, name, launch, None);
}

pub fn register_agent_with_owner(
    env: &Env,
    session_id: &str,
    launch_id: &str,
    name: &str,
    launch: LaunchParams,
    runtime_owner: Option<RuntimeOwner>,
) {
    let mut launch = launch;
    // A launch records the lane it decided, a stamped channel's at trunk.
    launch.lane = launch.lane.take().or_else(|| {
        Some(match launch.channel.as_deref() {
            Some(channel) => stamped_lane(env, &env.project_root, channel),
            None => rimz::worktree::own_lane(&env.project_root, &env.project_root),
        })
    });
    let store = env.store();
    let workspace = env.resolve_workspace(&env.project_root);
    store
        .append_event(&EventEnvelope::agent_launched(
            workspace.workspace_id,
            &workspace.session_name,
            &AgentKind::new_unchecked("claude"),
            AgentLaunchPayload {
                system_prompt_digest: None,
                agent_id: AgentSessionId::from(session_id),
                launch_id: Some(AgentSessionId::from(launch_id)),
                agent_name: name.to_owned(),
                agent_name_explicit: launch.parent_agent_id.is_none(),
                launch: launch.clone(),
                state: AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner,
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: Some(name.to_owned()),
                prompt: None,
                description: None,
            },
        ))
        .expect("seed launched target");
    let mut observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from(session_id)),
        LifecycleSignal::Registered,
    );
    observation.agent_name = launch.parent_agent_id.is_none().then(|| name.to_owned());
    observation.launch = launch;
    store
        .append_agent_lifecycle(AgentLifecycleIntent {
            session_name: "rimz-test",
            agent_kind: AgentKind::new_unchecked("claude"),
            event_name: "test",
            observation: &observation,
            spawned_subagents: &[],
        })
        .expect("register target");
}

/// Seeds `@planner`'s Bound launch of `kind` in the project root, then its registration in the
/// zellij pane `pane` with `transcript`: a seat of a kind `register_agent` does not seed.
pub fn register_seat(
    env: &Env,
    kind: &str,
    launch: LaunchParams,
    pane: &str,
    transcript: &std::path::Path,
) {
    let workspace = env.resolve_workspace(&env.project_root);
    let launch = LaunchParams {
        lane: Some(rimz::worktree::own_lane(
            &env.project_root,
            &env.project_root,
        )),
        ..launch
    };
    env.store()
        .append_event(&EventEnvelope::agent_launched(
            workspace.workspace_id.clone(),
            &workspace.session_name,
            &AgentKind::new_unchecked(kind),
            AgentLaunchPayload {
                system_prompt_digest: None,
                agent_id: AgentSessionId::from("provider-session"),
                launch_id: Some(AgentSessionId::from("launch-session")),
                agent_name: "planner".to_owned(),
                agent_name_explicit: true,
                launch: launch.clone(),
                state: AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: None,
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: Some("planner".to_owned()),
                prompt: None,
                description: None,
            },
        ))
        .expect("seed launched seat");
    let mut observation = AgentLifecycleObservation::new(
        Some("provider-session".into()),
        LifecycleSignal::Registered,
    );
    observation.agent_name = Some("planner".to_owned());
    observation.launch_id = Some("launch-session".into());
    observation.launch = launch;
    observation.pane_id =
        Some(rimz::ids::PaneId::parse(&format!("zellij:{pane}")).expect("pane id"));
    observation.transcript_path = Some(transcript.display().to_string());
    env.store()
        .append_event(&EventEnvelope::agent_lifecycle(
            workspace.workspace_id,
            &workspace.session_name,
            kind,
            "SessionStart",
            &observation,
        ))
        .expect("register seat");
}

/// A lane built through its wire form, since the lane builders are crate-private.
pub fn lane(home: &str, path: &std::path::Path, name: Option<&str>) -> rimz::ids::Lane {
    let mut wire = serde_json::json!({ "home": home, "path": path.display().to_string() });
    if let Some(name) = name {
        wire["name"] = name.into();
    }
    serde_json::from_value(wire).expect("lane wire form")
}

/// The lane a root's registration hook stamps: its checkout's own lane, or the
/// lane its stamped channel names there. A fixture that appends a registration
/// straight to the store calls it in place of the hook.
pub fn stamp_hook_lane(env: &Env, observation: &mut AgentLifecycleObservation) {
    if observation.parent_agent_id.is_some() || observation.launch.lane.is_some() {
        return;
    }
    let Some(path) = observation.worktree_path.as_deref() else {
        return;
    };
    let own = rimz::worktree::own_lane(&env.project_root, std::path::Path::new(path));
    observation.launch.lane = Some(own.adopt(observation.launch.channel.as_deref()));
}

/// The lane an old `channel` stamp means at the checkout holding `path`: the
/// value a launch event now carries where a fixture used to stamp `channel`.
pub fn stamped_lane(env: &Env, path: &std::path::Path, stamp: &str) -> rimz::ids::Lane {
    rimz::worktree::own_lane(&env.project_root, path).adopt(Some(stamp))
}

/// Seeds the Bound launch of `session_id` in `lane`, the durable launch record
/// a later hook registration of that session adopts. Registration builds only
/// its checkout's own lane, so a named lane reaches a hook-registered row this way.
pub fn seed_launch_lane(
    env: &Env,
    session_id: &str,
    name: &str,
    lane: rimz::ids::Lane,
    team: Option<&str>,
) {
    seed_launch_lane_as(env, "claude", session_id, name, lane, team);
}

/// [`seed_launch_lane`] for an agent of `kind`, whose registration adopts it.
pub fn seed_launch_lane_as(
    env: &Env,
    kind: &str,
    session_id: &str,
    name: &str,
    lane: rimz::ids::Lane,
    team: Option<&str>,
) {
    let workspace = env.resolve_workspace(&env.project_root);
    let worktree_path = lane.path().to_owned();
    env.store()
        .append_event(&EventEnvelope::agent_launched(
            workspace.workspace_id,
            &workspace.session_name,
            &AgentKind::new_unchecked(kind),
            AgentLaunchPayload {
                system_prompt_digest: None,
                agent_id: AgentSessionId::from(session_id),
                launch_id: None,
                agent_name: name.to_owned(),
                agent_name_explicit: true,
                launch: LaunchParams {
                    lane: Some(lane),
                    team: team.map(ToOwned::to_owned),
                    ..LaunchParams::default()
                },
                state: AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: None,
                worktree_path: Some(worktree_path),
                worktree_branch: None,
                prompt: None,
                description: None,
            },
        ))
        .expect("seed launched lane");
}

pub fn publish_claude_5h(env: &Env, used: u8) -> jiff::Timestamp {
    let resets_at = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(2);
    publish_claude_5h_resetting_at(env, used, resets_at);
    resets_at
}

/// A reset instant as a window reading's `(resets <time>)` names it, in the fixture's zone.
pub fn reset_minute(at: jiff::Timestamp) -> String {
    at.to_zoned_datetime(jiff::tz::TimeZone::system())
        .strftime("%Y-%m-%d %H:%M")
        .to_string()
}

/// Publishes a 5h claude window on the room's default account, `used` percent
/// spent and resetting at `resets_at`.
pub fn publish_claude_5h_resetting_at(env: &Env, used: u8, resets_at: jiff::Timestamp) {
    use rimz::agents::{AgentRateLimits, RateLimitCacheEntry, RateLimitWindow, RateLimitsCache};
    env.publish_rate_limits(&RateLimitsCache {
        entries: std::collections::BTreeMap::from([(
            "claude@default".parse().unwrap(),
            RateLimitCacheEntry {
                limits: AgentRateLimits {
                    windows: vec![RateLimitWindow {
                        used_percentage: Some(used),
                        resets_at: Some(resets_at),
                        duration_mins: Some(rimz::agents::WindowSpan::FiveHour.minutes()),
                        ..RateLimitWindow::default()
                    }],
                },
                ..Default::default()
            },
        )]),
        ..Default::default()
    });
}

pub fn agent_wait(env: &Env) -> std::process::Command {
    agent_wait_at(env, &env.rimz_bin())
}

/// `agent_wait` through `bin`, a wrapper around the RimZ binary.
pub fn agent_wait_at(env: &Env, bin: &std::path::Path) -> std::process::Command {
    let mut command = env.rimz_at(bin);
    command
        .env("RIMZ_AGENT_KIND", "claude")
        .env("RIMZ_AGENT_ID", "launch-session")
        .env("RIMZ_AGENT_NAME", "planner");
    command
}
