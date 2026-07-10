use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
use rimz::message::{DeliveryGate, MessageRecord};
use rimz::store::event::EventEnvelope;

use crate::common::Env;

#[test]
fn bash_registration_calls_the_rimz_binary() {
    let env = Env::new();
    let output = env
        .rimz()
        .env("COMPLETE", "bash")
        .output()
        .expect("generate bash completion");
    assert!(
        output.status.success(),
        "completion registration failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let script = String::from_utf8_lossy(&output.stdout);
    assert!(script.contains("_clap_complete_rimz"), "{script}");
    assert!(
        script.contains(&env.rimz_bin().display().to_string()),
        "{script}"
    );
}

#[test]
fn dynamic_completion_reads_live_handles_and_message_ids() {
    let env = Env::new();
    let mut observation =
        AgentLifecycleObservation::new(Some("sess-coder".into()), LifecycleSignal::Registered);
    observation.launch.role = Some("coder".to_owned());
    observation.worktree_path = Some(env.project_root.display().to_string());
    env.store()
        .append_event(&EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            "rimz-test",
            "codex",
            "SessionStart",
            &observation,
        ))
        .expect("append lifecycle");

    let snapshot = env.store().snapshot_cached().expect("snapshot");
    let agent = snapshot.agents.first().expect("seeded agent");
    let message = MessageRecord::new(
        env.workspace_id.clone(),
        agent,
        "review the completion path".to_owned(),
        true,
        DeliveryGate::Done,
    );
    let message_id = message.message_id.to_string();
    env.store()
        .queue_message(&message, "rimz-test")
        .expect("queue message");

    let handles = complete(&env, 2, ["rimz", "message", "@"]);
    assert!(handles.lines().any(|line| line == "@coder"), "{handles}");

    let message_ids = complete(&env, 3, ["rimz", "message", "show", ""]);
    assert!(
        message_ids.lines().any(|line| line == message_id),
        "{message_ids}"
    );
}

fn complete<const N: usize>(env: &Env, index: usize, words: [&str; N]) -> String {
    let output = env
        .rimz()
        .env("COMPLETE", "bash")
        .env("_CLAP_COMPLETE_INDEX", index.to_string())
        .env("_CLAP_COMPLETE_COMP_TYPE", "9")
        .env("_CLAP_COMPLETE_SPACE", "false")
        .env("_CLAP_IFS", "\n")
        .arg("--")
        .args(words)
        .output()
        .expect("run dynamic completion");
    assert!(
        output.status.success(),
        "dynamic completion failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("completion output is utf-8")
}
