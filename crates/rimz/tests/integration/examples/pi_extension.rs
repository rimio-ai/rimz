//! Node-driven coverage for the embedded Pi extension. The extension is
//! TypeScript-flavored JavaScript loaded in-process by Pi, so this suite drives
//! the same exported factory through Node and captures the `rimz hooks feed`
//! envelopes it would spawn.

#[test]
#[cfg(unix)]
fn extension_accumulates_cost_only_on_turn_end_and_clears_on_shutdown() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        tracing::warn!("skipping: node not on PATH");
        return;
    }

    use std::os::unix::fs::PermissionsExt as _;

    const EXTENSION_SOURCE: &str = include_str!("../../../src/agents/pi/extension.ts");

    let dir = tempfile::tempdir().unwrap();
    let extension_path = dir.path().join("rimz.mjs");
    let capture_path = dir.path().join("capture.jsonl");
    let stub_path = dir.path().join("rimz-capture");
    std::fs::write(&extension_path, EXTENSION_SOURCE).unwrap();
    std::fs::write(
        &stub_path,
        "#!/bin/sh\npayload=$(cat)\nprintf '%s\\n' \"$payload\" >> \"$RIMZ_CAPTURE\"\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&stub_path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&stub_path, permissions).unwrap();

    let harness = dir.path().join("harness.mjs");
    std::fs::write(
        &harness,
        format!(
            r#"
import fs from "node:fs/promises";

process.env.RIMZ_BIN = {};
process.env.RIMZ_CAPTURE = {};

const {{ default: rimz }} = await import({});
const handlers = new Map();
const pi = {{
  on: (event, handler) => handlers.set(event, handler),
  getThinkingLevel: () => "medium",
}};
const ctx = {{
  sessionManager: {{
    getSessionId: () => "sess-1",
    getCwd: () => "/repo",
  }},
  getContextUsage: () => ({{ percent: 45, contextWindow: 1000, tokens: 450 }}),
  model: {{ id: "gpt-5" }},
}};
rimz(pi);

handlers.get("turn_end")({{
  usage: {{
    input: 10,
    output: 5,
    cacheRead: 3,
    cacheWrite: 2,
    totalTokens: 20,
    cost: {{ total: 0.25 }},
  }},
}}, ctx);
handlers.get("agent_end")({{
  messages: [{{
    role: "assistant",
    model: "gpt-5.5",
    stopReason: "stop",
    usage: {{ totalTokens: 20, cost: {{ total: 0.25 }} }},
  }}],
}}, ctx);
handlers.get("session_shutdown")({{ reason: "quit" }}, ctx);

const readPayloads = async () => {{
  try {{
    const text = await fs.readFile({}, "utf8");
    return text.trim().split("\n").filter(Boolean).map((line) => JSON.parse(line));
  }} catch {{
    return [];
  }}
}};

let payloads = [];
for (let i = 0; i < 250; i += 1) {{
  payloads = await readPayloads();
  if (payloads.length >= 2) break;
  await new Promise((resolve) => setTimeout(resolve, 20));
}}
if (payloads.length < 2) {{
  throw new Error(`expected 2 forwarded payloads, got ${{payloads.length}}`);
}}
const byEvent = Object.fromEntries(payloads.map((payload) => [payload.hook_event_name, payload]));
if (byEvent.agent_end.total_cost_usd !== 0.25) {{
  throw new Error(`agent_end cost was ${{byEvent.agent_end.total_cost_usd}}`);
}}
if (byEvent.agent_end.input_tokens !== 10 || byEvent.agent_end.cache_write_input_tokens !== 2) {{
  throw new Error(`agent_end lost turn_end token split: ${{JSON.stringify(byEvent.agent_end)}}`);
}}
if ("total_cost_usd" in byEvent.session_shutdown) {{
  throw new Error(`shutdown kept cost: ${{JSON.stringify(byEvent.session_shutdown)}}`);
}}
"#,
            serde_json::to_string(stub_path.to_str().unwrap()).unwrap(),
            serde_json::to_string(capture_path.to_str().unwrap()).unwrap(),
            serde_json::to_string(&format!("file://{}", extension_path.display())).unwrap(),
            serde_json::to_string(capture_path.to_str().unwrap()).unwrap(),
        ),
    )
    .unwrap();

    let output = std::process::Command::new("node")
        .arg(&harness)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "node harness failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
