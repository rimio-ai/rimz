//! Shared calling-session and armed-wait fixtures.

use super::Env;
use rimz::agents::{AgentLifecycleObservation, LaunchParams, LifecycleSignal};
use rimz::config::Tasks;
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
