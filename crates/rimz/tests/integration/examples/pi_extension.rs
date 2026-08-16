//! Node-driven coverage for the embedded Pi extension. The extension is
//! TypeScript-flavored JavaScript loaded in-process by Pi, so this suite drives
//! the same exported factory through Node and captures the `rimz hooks feed`
//! envelopes it would spawn.

use serde_json::{Value, json};

use crate::common::Env;

#[test]
#[cfg(unix)]
fn extension_tracks_settled_boundary_cost_and_child_lineage() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        tracing::warn!("skipping: node not on PATH");
        return;
    }

    // Pi 0.80.4 added the native `agent_settled` idle boundary. The extension
    // reads the host package's exported `VERSION` to gate on it: 0.80.4+
    // forwards `agent_end` as enrichment and waits for native `agent_settled`,
    // while older releases emit `agent_settled` themselves on `agent_end`.
    // Both paths must accumulate cost on `turn_end`, clear it on shutdown, and
    // stop child rows at the same settled boundary.
    run_extension_harness("0.80.6", "agent_end", "agent_settled");
    run_extension_harness("0.80.3", "agent_settled", "agent_end");
}

/// Drive the embedded extension through Node as if `PI_VERSION` were
/// `pi_version`, asserting the turn verdict rides `boundary_event` (carrying the
/// accumulated cost and the `turn_end` token split), `absent_event` is never
/// forwarded, shutdown clears the running cost, and child factories report
/// their own session identity through the process-lineage markers.
#[cfg(unix)]
fn run_extension_harness(pi_version: &str, boundary_event: &str, absent_event: &str) {
    use std::os::unix::fs::PermissionsExt as _;

    const EXTENSION_SOURCE: &str = include_str!("../../../src/agents/adapters/pi/extension.ts");

    let dir = tempfile::tempdir().unwrap();
    let extension_path = dir.path().join("rimz.mjs");
    let capture_path = dir.path().join("capture.jsonl");
    let stub_path = dir.path().join("rimz-capture");
    std::fs::write(&extension_path, EXTENSION_SOURCE).unwrap();

    // Pi's extension loader aliases the host package so an extension can import
    // runtime values like `VERSION`. Standalone Node has no such alias, so
    // stand up the minimal module the bare import resolves to.
    let pkg_dir = dir
        .path()
        .join("node_modules/@earendil-works/pi-coding-agent");
    std::fs::create_dir_all(&pkg_dir).unwrap();
    std::fs::write(
        pkg_dir.join("package.json"),
        r#"{"name":"@earendil-works/pi-coding-agent","version":"0.0.0","type":"module","exports":"./index.js"}"#,
    )
    .unwrap();
    std::fs::write(
        pkg_dir.join("index.js"),
        format!(
            "export const VERSION = {};\n",
            serde_json::to_string(pi_version).unwrap()
        ),
    )
    .unwrap();

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
delete process.env.RIMZ_PI_PARENT_SESSION;
delete process.env.PI_SUBAGENT_CHILD_AGENT;
const boundaryEvent = {};
const absentEvent = {};
const hasNativeSettled = {};

const {{ default: rimz }} = await import({});
const makePi = () => {{
  const handlers = new Map();
  const busHandlers = new Map();
  const pi = {{
    on: (event, handler) => handlers.set(event, handler),
    events: {{
      on: (event, handler) => {{
        busHandlers.set(event, handler);
        return () => busHandlers.delete(event);
      }},
    }},
    getThinkingLevel: () => "medium",
  }};
  return {{ pi, handlers, busHandlers }};
}};
const {{ pi, handlers, busHandlers }} = makePi();
const {{
  pi: childPi,
  handlers: childHandlers,
  busHandlers: childBusHandlers,
}} = makePi();
let rootSessionId = "sess-1";
const ctx = {{
  sessionManager: {{
    getSessionId: () => rootSessionId,
    getCwd: () => "/repo",
  }},
  getContextUsage: () => ({{ percent: 45, contextWindow: 1000, tokens: 450 }}),
  model: {{ id: "gpt-5" }},
}};
const childCtx = {{
  sessionManager: {{
    getSessionId: () => "sess-child",
    getCwd: () => "/repo",
    getBranch: () => [{{ type: "session_info", name: "general-purpose#abc123" }}],
  }},
  getContextUsage: () => ({{ percent: 5, contextWindow: 1000, tokens: 50 }}),
  model: {{ id: "gpt-5-mini" }},
}};
rimz(pi);
rimz(childPi);
handlers.get("session_start")({{ reason: "launch" }}, ctx);

await handlers.get("tool_call")({{
  toolCallId: "ask-call",
  toolName: "ask_user_question",
  input: {{ questions: [{{ question: "Ship?" }}] }},
}}, {{ ...ctx, hasUI: true }});
handlers.get("tool_execution_end")({{
  toolCallId: "sibling-call",
  toolName: "bash",
  isError: false,
}}, ctx);
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
handlers.get("session_shutdown")({{ reason: "new" }}, ctx);
rootSessionId = "sess-2";
handlers.get("session_start")({{ reason: "new" }}, ctx);
if (globalThis[Symbol.for("rimz.pi.primary-session")]?.id !== "sess-2" ||
    process.env.RIMZ_PI_PARENT_SESSION !== "sess-2") {{
  throw new Error("primary markers did not follow the rotated session");
}}
childHandlers.get("session_start")({{ reason: "in-process-child" }}, childCtx);
childHandlers.get("agent_end")({{
  messages: [{{
    role: "assistant",
    stopReason: "error",
    errorMessage: "child failed",
  }}],
}}, childCtx);
if (hasNativeSettled) childHandlers.get("agent_settled")({{}}, childCtx);
handlers.get("session_shutdown")({{ reason: "quit" }}, ctx);
if (busHandlers.size !== 0 || childBusHandlers.size !== 0) {{
  throw new Error("extension registered plugin bus handlers");
}}

delete globalThis[Symbol.for("rimz.pi.primary-session")];
process.env.RIMZ_PI_PARENT_SESSION = "env-parent";
process.env.PI_SUBAGENT_CHILD_AGENT = "reviewer";
const {{ pi: subprocessPi, handlers: subprocessHandlers }} = makePi();
const subprocessCtx = {{
  sessionManager: {{
    getSessionId: () => "sess-subprocess",
    getCwd: () => "/repo/subprocess",
  }},
}};
rimz(subprocessPi);
subprocessHandlers.get("session_start")({{ reason: "launch" }}, subprocessCtx);
if (globalThis[Symbol.for("rimz.pi.primary-session")]?.id !== "sess-subprocess" ||
    process.env.RIMZ_PI_PARENT_SESSION !== "sess-subprocess") {{
  throw new Error("subprocess child did not claim its own process markers");
}}
subprocessHandlers.get("session_shutdown")({{ reason: "quit" }}, subprocessCtx);

delete process.env.PI_SUBAGENT_CHILD_AGENT;
globalThis[Symbol.for("rimz.pi.primary-session")] = {{ id: "sess-legacy-root" }};
process.env.RIMZ_PI_PARENT_SESSION = "sess-legacy-root";
const {{ pi: reloadPi, handlers: reloadHandlers }} = makePi();
const reloadCtx = {{
  sessionManager: {{
    getSessionId: () => "sess-legacy-root",
    getCwd: () => "/repo/reload",
  }},
}};
rimz(reloadPi);
reloadHandlers.get("session_start")({{ reason: "reload" }}, reloadCtx);

const readPayloads = async () => {{
  try {{
    const text = await fs.readFile({}, "utf8");
    return text.trim().split("\n").filter(Boolean).map((line) => JSON.parse(line));
  }} catch {{
    return [];
  }}
}};

let payloads = [];
const expectedPayloads = hasNativeSettled ? 18 : 17;
for (let i = 0; i < 250; i += 1) {{
  payloads = await readPayloads();
  if (payloads.length >= expectedPayloads) break;
  await new Promise((resolve) => setTimeout(resolve, 20));
}}
if (payloads.length < expectedPayloads) {{
  throw new Error(`expected ${{expectedPayloads}} forwarded payloads, got ${{payloads.length}}`);
}}
const rootPayloads = payloads.filter((payload) =>
  payload.session_id === "sess-1" && !payload.hook_event_name.startsWith("subagent_"));
const byEvent = Object.fromEntries(rootPayloads.map((payload) => [payload.hook_event_name, payload]));
const sessionStarts = payloads.filter((payload) => payload.hook_event_name === "session_start");
const sessionStart = (id) => sessionStarts.find((payload) => payload.session_id === id);
for (const id of ["sess-1", "sess-2", "sess-legacy-root"]) {{
  const payload = sessionStart(id);
  if (payload?.session_lineage !== "root" || "parent_session_id" in payload) {{
    throw new Error(`root session lineage was ${{JSON.stringify(payload)}}`);
  }}
}}
for (const [id, parentId] of [["sess-child", "sess-2"], ["sess-subprocess", "env-parent"]]) {{
  const payload = sessionStart(id);
  if (payload?.session_lineage !== "child" || payload.parent_session_id !== parentId) {{
    throw new Error(`child session lineage was ${{JSON.stringify(payload)}}`);
  }}
}}
if (byEvent.tool_call?.tool_call_id !== "ask-call") {{
  throw new Error(`tool_call lost correlation: ${{JSON.stringify(byEvent.tool_call)}}`);
}}
if (byEvent.tool_execution_end?.tool_call_id !== "sibling-call") {{
  throw new Error(`tool_execution_end lost correlation: ${{JSON.stringify(byEvent.tool_execution_end)}}`);
}}
const boundary = byEvent[boundaryEvent];
if (!boundary) {{
  throw new Error(`missing boundary ${{boundaryEvent}}: ${{JSON.stringify(payloads.map((p) => p.hook_event_name))}}`);
}}
if (absentEvent in byEvent) {{
  throw new Error(`unexpected ${{absentEvent}} forwarded for this pi version`);
}}
if (boundary.total_cost_usd !== 0.25) {{
  throw new Error(`boundary cost was ${{boundary.total_cost_usd}}`);
}}
if (boundary.input_tokens !== 10 || boundary.cache_write_input_tokens !== 2) {{
  throw new Error(`boundary lost turn_end token split: ${{JSON.stringify(boundary)}}`);
}}
if ("total_cost_usd" in byEvent.session_shutdown) {{
  throw new Error(`shutdown kept cost: ${{JSON.stringify(byEvent.session_shutdown)}}`);
}}
const childStarts = payloads.filter((payload) => payload.hook_event_name === "subagent_started");
const childStops = payloads.filter((payload) => payload.hook_event_name === "subagent_stopped");
if (childStarts.length !== 2 || childStops.length !== 2) {{
  throw new Error(`primary sessions self-reported or a child feed was lost: ${{JSON.stringify({{ childStarts, childStops }})}}`);
}}
for (const child of [...childStarts, ...childStops]) {{
  if (child.subagent_source !== "pi-session" || "model" in child ||
      "total_cost_usd" in child || "total_tokens" in child) {{
    throw new Error(`child payload is not lean: ${{JSON.stringify(child)}}`);
  }}
}}
const inProcessStart = childStarts.find((child) => child.subagent_id === "sess-child");
const inProcessStop = childStops.find((child) => child.subagent_id === "sess-child");
if (inProcessStart?.session_id !== "sess-2" ||
    inProcessStart.subagent_label !== "general-purpose#abc123" ||
    inProcessStop?.errored !== true) {{
  throw new Error(`in-process child self-identification was ${{JSON.stringify({{ inProcessStart, inProcessStop }})}}`);
}}
const subprocessStart = childStarts.find((child) => child.subagent_id === "sess-subprocess");
const subprocessStop = childStops.find((child) => child.subagent_id === "sess-subprocess");
if (subprocessStart?.session_id !== "env-parent" || subprocessStart.subagent_label !== "reviewer" ||
    subprocessStop?.errored !== false) {{
  throw new Error(`subprocess child self-identification was ${{JSON.stringify({{ subprocessStart, subprocessStop }})}}`);
}}
"#,
            serde_json::to_string(stub_path.to_str().unwrap()).unwrap(),
            serde_json::to_string(capture_path.to_str().unwrap()).unwrap(),
            serde_json::to_string(boundary_event).unwrap(),
            serde_json::to_string(absent_event).unwrap(),
            boundary_event == "agent_end",
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
        "node harness failed for pi {pi_version}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let payloads = std::fs::read_to_string(&capture_path)
        .expect("read captured Pi extension payloads")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("captured payload JSON"))
        .collect::<Vec<_>>();
    let reload_payload = payloads
        .iter()
        .find(|payload| {
            payload["hook_event_name"] == "session_start"
                && payload["session_id"] == "sess-legacy-root"
        })
        .expect("legacy reload session_start payload");
    assert_eq!(reload_payload["session_lineage"], "root");

    let env = Env::new();
    let stale_child = json!({
        "hook_event_name": "subagent_started",
        "session_id": "temporary-session",
        "subagent_id": "sess-legacy-root",
        "subagent_label": "resumed lane",
        "subagent_source": "pi-session",
        "cwd": env.project_root,
    });
    for payload in [&stale_child, reload_payload] {
        let output = env.run_hook("pi", &payload.to_string());
        assert!(
            output.status.success(),
            "pi hook failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty(), "pi lifecycle hook is silent");
    }
    let snapshot = env.snapshot_json();
    let repaired = snapshot["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .find(|agent| agent["agent_id"] == "sess-legacy-root")
        .expect("repaired Pi root row");
    assert_eq!(repaired["parent_agent_id"], Value::Null);
}
