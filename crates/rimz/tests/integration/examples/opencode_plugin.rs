//! Node-driven coverage for the embedded OpenCode plugin. OpenCode loads the
//! TypeScript source in-process, so this suite drives the exported factory and
//! captures the `rimz hooks feed` envelopes it spawns.

#[test]
#[cfg(unix)]
fn plugin_preserves_measured_usage_across_zero_only_updates() {
    use std::os::unix::fs::PermissionsExt as _;

    let capability = std::process::Command::new("node")
        .args([
            "--experimental-strip-types",
            "--input-type=module-typescript",
            "--eval",
            "const value: number = 1;",
        ])
        .output();
    let Ok(capability) = capability else {
        tracing::warn!("skipping: node not on PATH");
        return;
    };
    if !capability.status.success() {
        tracing::warn!("skipping: node cannot strip TypeScript syntax");
        return;
    }

    const PLUGIN_SOURCE: &str = include_str!("../../../src/agents/opencode/plugin.ts");

    let dir = tempfile::tempdir().unwrap();
    let plugin_path = dir.path().join("rimz.ts");
    let capture_path = dir.path().join("capture.jsonl");
    let stub_path = dir.path().join("rimz-capture");
    std::fs::write(&plugin_path, PLUGIN_SOURCE).unwrap();
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

const {{ RimzPlugin }} = await import({});
const plugin = await RimzPlugin({{
  directory: {},
  worktree: {},
  client: {{ config: {{ providers: async () => ({{ providers: [] }}) }} }},
}});

const update = async (info) => await plugin.event({{
  event: {{ type: "message.updated", properties: {{ info }} }},
}});
const idle = async () => await plugin.event({{
  event: {{ type: "session.idle", properties: {{ sessionID: "ses-1" }} }},
}});
const readPayloads = async () => {{
  try {{
    const text = await fs.readFile({}, "utf8");
    return text.trim().split("\n").filter(Boolean).map((line) => JSON.parse(line));
  }} catch {{
    return [];
  }}
}};
const waitForPayloads = async (count) => {{
  let payloads = [];
  for (let i = 0; i < 250; i += 1) {{
    payloads = await readPayloads();
    if (payloads.length >= count) return payloads;
    await new Promise((resolve) => setTimeout(resolve, 20));
  }}
  throw new Error(`expected ${{count}} payloads, captured ${{JSON.stringify(payloads)}}`);
}};
const assertTokens = (payload, expected) => {{
  for (const [field, value] of Object.entries(expected)) {{
    if (payload[field] !== value) {{
      throw new Error(`${{field}} was ${{payload[field]}}, captured ${{JSON.stringify(payload)}}`);
    }}
  }}
}};

await update({{
  sessionID: "ses-1",
  modelID: "gpt-5",
  providerID: "openai",
  variant: "high",
  tokens: {{ input: 2627, output: 8, cache: {{ read: 5632, write: 0 }}, total: 8267 }},
}});
await update({{
  sessionID: "ses-1",
  modelID: "gpt-5-mini",
  providerID: "openai",
  variant: "low",
  tokens: {{ input: 0, output: 0, cache: {{ read: 0, write: 0 }} }},
}});
await idle();
const first = (await waitForPayloads(1))[0];
assertTokens(first, {{
  input_tokens: 2627,
  output_tokens: 8,
  cache_read_input_tokens: 5632,
  cache_write_input_tokens: 0,
  total_tokens: 8267,
}});
if (first.model !== "gpt-5-mini" || first.effort !== "low") {{
  throw new Error(`zero-only update lost metadata: ${{JSON.stringify(first)}}`);
}}

await update({{
  sessionID: "ses-1",
  modelID: "gpt-5-mini",
  providerID: "openai",
  variant: "low",
  tokens: {{ input: 23, output: 0, cache: {{ read: 0, write: 0 }}, total: 23 }},
}});
await idle();
const second = (await waitForPayloads(2))[1];
assertTokens(second, {{
  input_tokens: 23,
  output_tokens: 0,
  cache_read_input_tokens: 0,
  cache_write_input_tokens: 0,
  total_tokens: 23,
}});
"#,
            serde_json::to_string(stub_path.to_str().unwrap()).unwrap(),
            serde_json::to_string(capture_path.to_str().unwrap()).unwrap(),
            serde_json::to_string(&format!("file://{}", plugin_path.display())).unwrap(),
            serde_json::to_string(dir.path().to_str().unwrap()).unwrap(),
            serde_json::to_string(dir.path().to_str().unwrap()).unwrap(),
            serde_json::to_string(capture_path.to_str().unwrap()).unwrap(),
        ),
    )
    .unwrap();

    let output = std::process::Command::new("node")
        .arg("--experimental-strip-types")
        .arg(&harness)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "node harness failed for OpenCode\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
