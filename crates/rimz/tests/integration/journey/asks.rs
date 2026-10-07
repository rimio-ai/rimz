use serde_json::json;

use crate::common::{CommandTimeoutExt, Env};

#[test]
fn open_asks_render_as_an_actionable_list() {
    let env = Env::new();
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "sess-rendered-ask",
        "tool_name": "AskUserQuestion",
        "tool_input": {
            "questions": [{
                "question": "Choose deployment path?",
                "options": [
                    { "label": "safe", "description": "Use staged rollout" },
                    { "label": "fast" }
                ],
                "multiSelect": false
            }]
        }
    });
    let hook = env.run_hook("claude", &payload.to_string());
    assert!(
        hook.status.success(),
        "hook failed: {}",
        String::from_utf8_lossy(&hook.stderr)
    );

    let output = env
        .rimz()
        .arg("asks")
        .bounded_output()
        .expect("render open asks");
    assert!(
        output.status.success(),
        "asks failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rendered = String::from_utf8_lossy(&output.stdout);
    assert!(rendered.contains("ASK"), "missing list header: {rendered}");
    assert!(rendered.contains("@claude"), "missing agent: {rendered}");
    assert!(
        rendered.contains("Choose deployment path?"),
        "missing question: {rendered}"
    );
    assert!(!rendered.contains("ask_"), "unexpected ask id: {rendered}");
    assert!(
        rendered.contains("rimz answer @claude#main <1|2>"),
        "missing answer command: {rendered}"
    );
}

#[test]
fn answer_scoped_miss_offers_a_retypeable_root_correction() {
    let env = Env::new();
    let mut observation = rimz::agents::AgentLifecycleObservation::new(
        Some("root-coder".into()),
        rimz::agents::LifecycleSignal::Registered,
    );
    observation.launch.role = Some("coder".into());
    observation.worktree_path = Some(env.project_root.to_string_lossy().into_owned());
    env.store()
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            "rimz-test",
            "claude",
            "SessionStart",
            &observation,
        ))
        .unwrap();
    let output = env
        .rimz()
        .env(rimz::workspace::ENV_CHANNEL, "scratch")
        .args(["answer", "@coder", "1"])
        .bounded_output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    let command = stderr
        .lines()
        .find_map(|line| line.trim().strip_prefix("try: "))
        .unwrap_or_else(|| panic!("{stderr}"));
    let command = shlex::split(command).unwrap();
    assert_eq!(
        std::path::Path::new(&command[0]).file_name().unwrap(),
        "rimz"
    );
    assert_eq!(&command[1..], ["answer", "@coder#main", "1"]);
}
