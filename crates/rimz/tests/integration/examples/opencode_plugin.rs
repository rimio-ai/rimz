//! Node-driven coverage for the embedded OpenCode plugin. OpenCode loads the
//! TypeScript source in-process, so this suite drives the exported factory and
//! captures the `rimz hooks feed` envelopes it spawns.

#[test]
#[cfg(unix)]
fn plugin_preserves_usage_and_announces_child_models() {
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

    const PLUGIN_SOURCE: &str = include_str!("../../../src/agents/adapters/opencode/plugin.ts");

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
let sessionReads = 0;
const plugin = await RimzPlugin({{
  directory: {},
  worktree: {},
  client: {{
    config: {{
      providers: async () => ({{
        providers: [
          {{
            id: "openai",
            models: {{
              "gpt-5": {{ name: "GPT-5", limit: {{ input: 200000, context: 400000 }} }},
              "gpt-5-mini": {{ name: "GPT-5 Mini", limit: {{ context: 128000 }} }},
            }},
          }},
          {{
            id: "anthropic",
            models: {{
              "claude-sonnet-4-5": {{ name: "Claude Sonnet 4.5", limit: {{ context: 200000 }} }},
            }},
          }},
        ],
      }})
    }},
    session: {{
      get: async () => {{
        sessionReads += 1;
        return {{
          data: {{
            title: sessionReads === 1
              ? "New session - 2026-08-27T05:12:33.123Z"
              : "Fix OpenCode metadata",
            version: "1.18.23",
          }},
        }};
      }},
    }},
  }},
}});

const createRoot = async () => await plugin.event({{
  event: {{
    type: "session.created",
    properties: {{ info: {{ id: "ses-1", directory: {} }} }},
  }},
}});
const update = async (info) => await plugin.event({{
  event: {{ type: "message.updated", properties: {{ info }} }},
}});
const updateSession = async () => await plugin.event({{
  event: {{ type: "session.updated", properties: {{ info: {{ id: "ses-1" }} }} }},
}});
const idle = async () => await plugin.event({{
  event: {{ type: "session.idle", properties: {{ sessionID: "ses-1" }} }},
}});
const fail = async () => await plugin.event({{
  event: {{
    type: "session.error",
    properties: {{
      sessionID: "ses-1",
      error: {{
        name: "ProviderAuthError",
        data: {{ message: "Anthropic credentials expired", providerID: "anthropic" }},
      }},
    }},
  }},
}});
const failWithoutMessage = async () => await plugin.event({{
  event: {{
    type: "session.error",
    properties: {{
      sessionID: "ses-1",
      error: {{
        name: "MessageOutputLengthError",
        message: "MessageOutputLengthError",
        data: {{}},
      }},
    }},
  }},
}});
const createChild = async () => await plugin.event({{
  event: {{
    type: "session.created",
    properties: {{
      info: {{
        id: "ses-child",
        parentID: "ses-parent",
        directory: {},
        title: "review auth",
      }},
    }},
  }},
}});
const stepFinish = async (part) => await plugin.event({{
  event: {{ type: "message.part.updated", properties: {{ part }} }},
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

await createRoot();
const root = (await waitForPayloads(1))[0];
if (
  root.hook_event_name !== "session_created" ||
  root.session_name !== undefined ||
  root.agent_version !== undefined
) {{
  throw new Error(`placeholder metadata leaked: ${{JSON.stringify(root)}}`);
}}

await updateSession();
const refreshed = (await waitForPayloads(2))[1];
if (
  refreshed.hook_event_name !== "session_updated" ||
  refreshed.session_name !== "Fix OpenCode metadata" ||
  refreshed.agent_version !== "1.18.23"
) {{
  throw new Error(`updated metadata missing: ${{JSON.stringify(refreshed)}}`);
}}

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
const first = (await waitForPayloads(3))[2];
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
if (
  first.session_name !== "Fix OpenCode metadata" ||
  first.agent_version !== "1.18.23" ||
  first.model_display_name !== "GPT-5 Mini" ||
  first.context_window !== 128000
) {{
  throw new Error(`rich context missing: ${{JSON.stringify(first)}}`);
}}

await fail();
const failed = (await waitForPayloads(4))[3];
if (
  failed.error_class !== "ProviderAuthError" ||
  failed.error_message !== "Anthropic credentials expired"
) {{
  throw new Error(`structured error was not flattened: ${{JSON.stringify(failed)}}`);
}}

await failWithoutMessage();
const messageLess = (await waitForPayloads(5))[4];
if (
  messageLess.error_class !== "MessageOutputLengthError" ||
  messageLess.error_message !== undefined
) {{
  throw new Error(`message-less error grew fake text: ${{JSON.stringify(messageLess)}}`);
}}

await update({{
  sessionID: "ses-1",
  modelID: "gpt-5-mini",
  providerID: "openai",
  variant: "low",
  tokens: {{ input: 23, output: 0, cache: {{ read: 0, write: 0 }}, total: 23 }},
}});
await idle();
const second = (await waitForPayloads(6))[5];
assertTokens(second, {{
  input_tokens: 23,
  output_tokens: 0,
  cache_read_input_tokens: 0,
  cache_write_input_tokens: 0,
  total_tokens: 23,
}});

await createChild();
const created = (await waitForPayloads(7))[6];
if (
  created.hook_event_name !== "SubagentStart" ||
  created.session_id !== "ses-child" ||
  created.parent_session_id !== "ses-parent" ||
  created.prompt !== "review auth" ||
  created.model !== undefined
) {{
  throw new Error(`unexpected child creation: ${{JSON.stringify(created)}}`);
}}

await update({{
  sessionID: "ses-child",
  modelID: "claude-sonnet-4-5",
  providerID: "anthropic",
  tokens: {{ input: 100, output: 20, cache: {{ read: 30, write: 0 }}, total: 150 }},
}});
const announced = (await waitForPayloads(8))[7];
if (
  announced.hook_event_name !== "SubagentStart" ||
  announced.session_id !== "ses-child" ||
  announced.parent_session_id !== "ses-parent" ||
  announced.model !== "claude-sonnet-4-5" ||
  announced.prompt !== undefined
) {{
  throw new Error(`unexpected child model announcement: ${{JSON.stringify(announced)}}`);
}}
assertTokens(announced, {{
  input_tokens: 100,
  output_tokens: 20,
  cache_read_input_tokens: 30,
  cache_write_input_tokens: 0,
  total_tokens: 150,
}});

await update({{
  sessionID: "ses-child",
  modelID: "claude-sonnet-4-5",
  providerID: "anthropic",
  tokens: {{ input: 200, output: 40, cache: {{ read: 60, write: 0 }}, total: 300 }},
}});
await stepFinish({{
  type: "step-finish",
  sessionID: "ses-child",
  modelID: "gpt-5-mini",
  providerID: "openai",
  tokens: {{ input: 40, output: 10, cache: {{ read: 5, write: 0 }}, total: 55 }},
}});
const switched = (await waitForPayloads(9))[8];
if (switched.model !== "gpt-5-mini" || switched.prompt !== undefined) {{
  throw new Error(`unexpected child model switch: ${{JSON.stringify(switched)}}`);
}}
await new Promise((resolve) => setTimeout(resolve, 250));
const finalPayloads = await readPayloads();
if (finalPayloads.length !== 9) {{
  throw new Error(`child model was announced more than once: ${{JSON.stringify(finalPayloads)}}`);
}}

for (const [sessionID, session] of [
  ["ses-no-session-client", undefined],
  ["ses-session-error", {{ get: async () => {{ throw new Error("unavailable"); }} }}],
]) {{
  const resilientPlugin = await RimzPlugin({{
    directory: {},
    worktree: {},
    client: {{ config: {{ providers: async () => ({{ providers: [] }}) }}, session }},
  }});
  await resilientPlugin.event({{
    event: {{ type: "session.created", properties: {{ info: {{ id: sessionID }} }} }},
  }});
}}
const resilient = await waitForPayloads(11);
for (const payload of resilient.slice(9)) {{
  if (payload.session_name !== undefined || payload.agent_version !== undefined) {{
    throw new Error(`failed session metadata read leaked fields: ${{JSON.stringify(payload)}}`);
  }}
}}

const stalledPlugin = await RimzPlugin({{
  directory: {},
  worktree: {},
  client: {{
    config: {{ providers: async () => ({{ providers: [] }}) }},
    session: {{ get: async () => await new Promise(() => {{}}) }},
  }},
}});
await stalledPlugin.event({{
  event: {{ type: "session.created", properties: {{ info: {{ id: "ses-stalled" }} }} }},
}});
await waitForPayloads(12);
await stalledPlugin.event({{
  event: {{ type: "session.idle", properties: {{ sessionID: "ses-stalled" }} }},
}});
await waitForPayloads(13);
await stalledPlugin.event({{
  event: {{ type: "session.deleted", properties: {{ info: {{ id: "ses-stalled" }} }} }},
}});
const stalled = (await waitForPayloads(14)).slice(11);
if (
  stalled.map((payload) => payload.hook_event_name).join(",") !==
  "session_created,session_idle,session_ended"
) {{
  throw new Error(`stalled metadata read reordered lifecycle: ${{JSON.stringify(stalled)}}`);
}}
"#,
            serde_json::to_string(stub_path.to_str().unwrap()).unwrap(),
            serde_json::to_string(capture_path.to_str().unwrap()).unwrap(),
            serde_json::to_string(&format!("file://{}", plugin_path.display())).unwrap(),
            serde_json::to_string(dir.path().to_str().unwrap()).unwrap(),
            serde_json::to_string(dir.path().to_str().unwrap()).unwrap(),
            serde_json::to_string(dir.path().to_str().unwrap()).unwrap(),
            serde_json::to_string(dir.path().to_str().unwrap()).unwrap(),
            serde_json::to_string(capture_path.to_str().unwrap()).unwrap(),
            serde_json::to_string(dir.path().to_str().unwrap()).unwrap(),
            serde_json::to_string(dir.path().to_str().unwrap()).unwrap(),
            serde_json::to_string(dir.path().to_str().unwrap()).unwrap(),
            serde_json::to_string(dir.path().to_str().unwrap()).unwrap(),
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
