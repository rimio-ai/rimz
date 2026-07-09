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
    assert!(rendered.contains("ask_"), "missing ask id: {rendered}");
}
