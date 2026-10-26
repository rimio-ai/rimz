//! Node-driven coverage for the embedded Pi extension. The extension is
//! TypeScript-flavored JavaScript loaded in-process by Pi, so this suite drives
//! the same exported factory through Node and captures the `rimz hooks feed`
//! envelopes it would spawn.

#[test]
#[cfg(unix)]
fn extension_tracks_settled_boundary_spend_and_child_lineage() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        crate::common::skip("node not on PATH");
        return;
    }

    // Pi 0.80.4 added the native `agent_settled` idle boundary. The extension
    // reads the host package's exported `VERSION` to gate on it: 0.80.4+
    // forwards `agent_end` as enrichment and waits for native `agent_settled`,
    // while older releases emit `agent_settled` themselves on `agent_end`.
    // Both paths must accumulate every upstream usage source, clear cost on
    // shutdown, and stop child rows at the same settled boundary.
    run_extension_harness("1.1.0", "agent_end", "agent_settled", false, None);
    run_extension_harness("0.80.3", "agent_settled", "agent_end", false, None);
}

#[test]
#[cfg(unix)]
fn extension_native_abort_overrides_retained_verdict() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        crate::common::skip("node not on PATH");
        return;
    }
    run_extension_harness("1.1.0", "agent_end", "agent_settled", true, None);
}

#[test]
#[cfg(unix)]
fn extension_requests_fast_only_for_supported_openai_models() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        crate::common::skip("node not on PATH");
        return;
    }
    run_extension_harness("1.4.0", "agent_end", "agent_settled", false, Some(true));
    run_extension_harness("0.57.0", "agent_settled", "agent_end", false, Some(true));
    run_extension_harness("0.56.9", "agent_settled", "agent_end", false, Some(false));
}

/// Drive the embedded extension through Node as if `PI_VERSION` were
/// `pi_version`, asserting the turn verdict rides `boundary_event` (carrying the
/// accumulated cost and the `turn_end` token split), the legacy `absent_event`
/// is never forwarded, native settlement and later envelopes retain standalone
/// charges, shutdown clears the running cost, only in-turn extension dialogs
/// outside a questionnaire are forwarded, and child factories report their own
/// session identity (relabelled when the child session is renamed) through the
/// process-lineage markers.
#[cfg(unix)]
fn run_extension_harness(
    pi_version: &str,
    boundary_event: &str,
    absent_event: &str,
    aborted: bool,
    fast_supported: Option<bool>,
) {
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
import assert from "node:assert/strict";

process.env.RIMZ_BIN = {};
process.env.RIMZ_CAPTURE = {};
const boundaryEvent = {};
const absentEvent = {};
const hasNativeSettled = {};
const aborted = {aborted};
const fastSupported = {};
delete process.env.RIMZ_LAUNCH_FAST;

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
const anonymousUsage = {{ type: "usage", kind: "cache_warm", model: "gpt-5",
  usage: {{ input: 900, cost: {{ total: 0.1 }} }} }};
const rootBranch = [
  {{ ...anonymousUsage, id: "warm-1", usage: {{ cost: {{ total: 0.2 }} }} }},
  anonymousUsage,
  structuredClone(anonymousUsage),
];
const ctx = {{
  sessionManager: {{
    getSessionId: () => rootSessionId,
    getCwd: () => "/repo",
    getBranch: () => structuredClone(rootBranch),
  }},
  getContextUsage: () => ({{ percent: 45, contextWindow: 1000, tokens: 450 }}),
  model: {{ id: "gpt-5", provider: "openai-codex", api: "openai-codex-responses" }},
}};
const childCtx = {{
  sessionManager: {{
    getSessionId: () => "sess-child",
    getCwd: () => "/repo",
    getBranch: () => [{{ type: "session_info", name: "general-purpose#abc123" }}],
  }},
  getContextUsage: () => ({{ percent: 5, contextWindow: 1000, tokens: 50 }}),
  model: {{ id: "gpt-5-mini", provider: "openai", api: "openai-responses" }},
}};
const launchReminders = "<system_reminder>\nroom rules\n</system_reminder>";
process.env.RIMZ_LAUNCH_REMINDERS = launchReminders;
if (fastSupported !== null) process.env.RIMZ_LAUNCH_FAST = "1";
rimz(pi);
rimz(childPi);
const fastResults = [];
const fastExpected = [];
if (fastSupported !== null) {{
  assert.equal(process.env.RIMZ_LAUNCH_FAST, "1", "fast must remain inheritable");
  const payload = {{
    model: "kept", input: [{{ role: "user", content: "go" }}], service_tier: "flex",
  }};
  const codexModel = {{
    id: "gpt-5.6-sol", provider: "openai-codex", api: "openai-codex-responses",
  }};
  const openaiModel = {{ id: "gpt-5.4", provider: "openai", api: "openai-responses" }};
  const check = (label, model, body, admitted, hooks = handlers) => {{
    const original = structuredClone(body);
    const value = hooks.get("before_provider_request")?.({{ payload: body }}, {{ model }});
    fastResults.push([label, value ?? null, body]);
    fastExpected.push([
      label, admitted && fastSupported ? {{ ...original, service_tier: "priority" }} : null,
      original,
    ]);
  }};
  for (const id of ["gpt-6.1-sol", "gpt-6-astra", "gpt-6-sol", "gpt-6-luna",
    "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-5.4", "gpt-5.5", "gpt-5.3-codex"]) {{
    check(id, {{ ...codexModel, id }}, payload, true);
  }}
  check("openai", openaiModel, {{ input: "go" }}, true);
  check("child", openaiModel, payload, true, childHandlers);
  check("child unlisted", childCtx.model, payload, false, childHandlers);
  check("completions", {{ ...openaiModel, api: "openai-completions" }}, payload, false);
  check("virtual", {{ ...codexModel, api: "pi-virtual" }}, payload, false);
  check("other provider", {{ ...openaiModel, provider: "other" }}, payload, false);
  check("crossed pair", {{ ...openaiModel, api: "openai-codex-responses" }}, payload, false);
  check("unlisted", {{ ...codexModel, id: "gpt-5.6-sol-extra" }}, payload, false);
  check("no model", undefined, payload, false);
  for (const body of [null, [], "payload"]) check("malformed", codexModel, body, false);
  for (const env of [undefined, "0", "true"]) {{
    if (env === undefined) delete process.env.RIMZ_LAUNCH_FAST;
    else process.env.RIMZ_LAUNCH_FAST = env;
    check(`env ${{env}}`, codexModel, payload, false);
  }}
  process.env.RIMZ_LAUNCH_FAST = "1";
  const throwingPayload = {{ get input() {{ throw new Error("bad payload"); }} }};
  const exception = handlers.get("before_provider_request")?.(
    {{ payload: throwingPayload }}, {{ model: codexModel }},
  );
  fastResults.push(["exception", exception ?? null]);
  fastExpected.push(["exception", null]);
  ctx.model = codexModel;
}}
if ("RIMZ_LAUNCH_REMINDERS" in process.env) {{
  throw new Error("launch reminders stayed in the inheritable environment");
}}
handlers.get("session_start")({{ reason: "launch" }}, ctx);
if (fastSupported !== null) ctx.model = {{ ...ctx.model, id: "gpt-5" }};
const rootStart = await handlers.get("before_agent_start")({{ prompt: "go", systemPrompt: "base" }}, ctx);
if (rootStart?.systemPrompt !== `base\n\n${{launchReminders}}`) {{
  throw new Error(`primary system prompt lost the launch reminders: ${{JSON.stringify(rootStart)}}`);
}}

const busyUi = {{ ...ctx, hasUI: true, mode: "interactive", isIdle: () => false }};
handlers.get("ui_prompt_start")({{ reason: "open", kind: "confirm", title: " Allow deploy? " }}, busyUi);
handlers.get("ui_prompt_end")({{ reason: "close", kind: "confirm" }}, busyUi);
const idleUi = {{ ...busyUi, isIdle: () => true }};
handlers.get("ui_prompt_start")({{ reason: "open", kind: "select" }}, idleUi);
handlers.get("ui_prompt_end")({{ reason: "close", kind: "select" }}, idleUi);
handlers.get("ui_prompt_start")({{ reason: "open", kind: "select" }}, {{ ...busyUi, mode: "rpc" }});
const askCall = handlers.get("tool_call")({{
  toolCallId: "ask-call",
  toolName: "ask_user_question",
  input: {{ questions: [{{ question: "Ship?" }}] }},
}}, {{ ...ctx, hasUI: true }});
handlers.get("ui_prompt_start")({{ reason: "open", kind: "custom" }}, busyUi);
await askCall;
handlers.get("tool_execution_end")({{
  toolCallId: "sibling-call",
  toolName: "bash",
  isError: false,
}}, ctx);
rootBranch.push({{ ...anonymousUsage, id: "warm-2", usage: {{ cost: {{ total: 0.2 }} }} }});
handlers.get("turn_end")({{
  message: {{
    role: "assistant",
    usage: {{
      input: 10,
      output: 5,
      cacheRead: 3,
      cacheWrite: 2,
      totalTokens: 20,
      cost: {{ total: 0.25 }},
    }},
  }},
  toolResults: [{{
    role: "toolResult",
    usage: {{ output: 1, cost: {{ total: 0.05 }} }},
  }}],
}}, ctx);
handlers.get("session_tree")({{
  summaryEntry: {{
    type: "branch_summary",
    usage: {{ output: 2, cost: {{ total: 0.15 }} }},
  }},
}}, ctx);
handlers.get("session_compact")({{
  reason: "manual",
  willRetry: false,
  compactionEntry: {{
    type: "compaction",
    usage: {{
      input: 10,
      cost: {{ total: 0.10 }},
    }},
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
if (hasNativeSettled) {{
  rootBranch.push({{ ...anonymousUsage, id: "warm-3", usage: {{ cost: {{ total: 0.3 }} }} }});
  rootBranch.push(structuredClone(anonymousUsage));
  handlers.get("agent_settled")({{}}, ctx);
  handlers.get("before_agent_start")({{ prompt: "again", systemPrompt: "base" }}, ctx);
  handlers.get("agent_settled")({{}}, ctx);
}}
handlers.get("session_shutdown")({{ reason: "new" }}, ctx);
rootSessionId = "sess-2";
handlers.get("session_start")({{ reason: "new" }}, ctx);
if (globalThis[Symbol.for("rimz.pi.primary-session")]?.id !== "sess-2" ||
    process.env.RIMZ_PI_PARENT_SESSION !== "sess-2") {{
  throw new Error("primary markers did not follow the rotated session");
}}
childHandlers.get("session_start")({{ reason: "in-process-child" }}, childCtx);
const childStart = await childHandlers.get("before_agent_start")({{ prompt: "review", systemPrompt: "base" }}, childCtx);
if (childStart !== undefined) {{
  throw new Error(`in-process child inherited launch reminders: ${{JSON.stringify(childStart)}}`);
}}
childHandlers.get("session_info_changed")({{ name: "reviewer: fix the parser" }}, childCtx);
childHandlers.get("session_info_changed")({{ name: "reviewer: fix the parser" }}, childCtx);
childHandlers.get("agent_end")({{
  messages: [{{
    role: "assistant",
    stopReason: aborted ? "stop" : "error",
    errorMessage: aborted ? undefined : "child failed",
  }}],
}}, childCtx);
if (hasNativeSettled) childHandlers.get("agent_settled")(aborted ? {{ aborted: true }} : {{}}, childCtx);
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

if (hasNativeSettled) {{
  const {{ pi: dialogPi, handlers: dialogHandlers }} = makePi();
  const dialogCtx = {{
    sessionManager: {{ getSessionId: () => "sess-dialog", getCwd: () => "/repo" }},
    hasUI: true,
    mode: "interactive",
    isIdle: () => false,
  }};
  rimz(dialogPi);
  dialogHandlers.get("ui_prompt_start")({{ reason: "open", kind: "select" }}, dialogCtx);
  dialogHandlers.get("agent_settled")(aborted ? {{ aborted: true }} : {{}}, dialogCtx);
  dialogHandlers.get("ui_prompt_end")({{ reason: "close", kind: "select" }}, dialogCtx);
}}

const readPayloads = async () => {{
  try {{
    const text = await fs.readFile({}, "utf8");
    return text.trim().split("\n").filter(Boolean).map((line) => JSON.parse(line));
  }} catch {{
    return [];
  }}
}};

let payloads = [];
const expectedPayloads = hasNativeSettled ? 30 : 24;
for (let i = 0; i < 250; i += 1) {{
  payloads = await readPayloads();
  if (payloads.length >= expectedPayloads) break;
  await new Promise((resolve) => setTimeout(resolve, 20));
}}
// Give a wrongly forwarded straggler time to land before asserting absence.
await new Promise((resolve) => setTimeout(resolve, 200));
payloads = await readPayloads();
if (payloads.length < expectedPayloads) {{
  throw new Error(`expected ${{expectedPayloads}} forwarded payloads, got ${{payloads.length}}`);
}}
const rootPayloads = payloads.filter((payload) =>
  payload.session_id === "sess-1" && !payload.hook_event_name.startsWith("subagent_"));
const byEvent = Object.fromEntries(rootPayloads.map((payload) => [payload.hook_event_name, payload]));
if (fastSupported !== null) {{
  const subprocessStart = payloads.find((payload) =>
    payload.session_id === "sess-subprocess" && payload.hook_event_name === "session_start");
  fastResults.push([
    "envelopes", byEvent.session_start.fast, byEvent.before_agent_start.fast, subprocessStart?.fast,
  ]);
  fastExpected.push(["envelopes", fastSupported, false, undefined]);
  assert.deepEqual(fastResults, fastExpected, "fast requests and current-model envelopes");
}}
if (byEvent.tool_call?.tool_call_id !== "ask-call") {{
  throw new Error(`tool_call lost correlation: ${{JSON.stringify(byEvent.tool_call)}}`);
}}
const uiStarts = rootPayloads.filter((payload) => payload.hook_event_name === "ui_prompt_start");
const uiEnds = rootPayloads.filter((payload) => payload.hook_event_name === "ui_prompt_end");
if (uiStarts.length !== 1 || uiEnds.length !== 1 || uiStarts[0].ui_prompt_id !== "ui_prompt:1" ||
    uiStarts[0].ui_prompt_title !== "Allow deploy?" || uiStarts[0].has_ui !== true ||
    uiEnds[0].ui_prompt_id !== "ui_prompt:1") {{
  throw new Error(`idle, rpc, or questionnaire dialogs leaked, or the in-turn dialog lost its key: ${{JSON.stringify({{ uiStarts, uiEnds }})}}`);
}}
const dialogPayloads = payloads.filter((payload) => payload.session_id === "sess-dialog")
  .map((payload) => payload.hook_event_name).sort().join("|");
if (hasNativeSettled && dialogPayloads !== "agent_settled|ui_prompt_start") {{
  throw new Error(`a dialog closed after the settle reopened the turn: ${{dialogPayloads}}`);
}}
if (byEvent.tool_execution_end?.tool_call_id !== "sibling-call") {{
  throw new Error(`tool_execution_end lost correlation: ${{JSON.stringify(byEvent.tool_execution_end)}}`);
}}
const boundary = byEvent[boundaryEvent];
if (!boundary) {{
  throw new Error(`missing boundary ${{boundaryEvent}}: ${{JSON.stringify(payloads.map((p) => p.hook_event_name))}}`);
}}
if (!hasNativeSettled && absentEvent in byEvent) {{
  throw new Error(`unexpected ${{absentEvent}} forwarded for this pi version`);
}}
const childBoundary = payloads.find((payload) => payload.session_id === "sess-child" &&
  payload.hook_event_name === "agent_settled");
if (childBoundary?.stop_reason !== (aborted ? "aborted" : "error")) {{
  throw new Error(`child settled verdict was ${{JSON.stringify(childBoundary)}}`);
}}
if (aborted && !payloads.some((payload) => payload.session_id === "sess-dialog" &&
    payload.hook_event_name === "agent_settled" && payload.stop_reason === "aborted")) {{
  throw new Error("native abort without a retained verdict was lost");
}}
if (hasNativeSettled) {{
  const settled = rootPayloads.filter((payload) => payload.hook_event_name === "agent_settled");
  const next = rootPayloads.find((payload) => payload.hook_event_name === "before_agent_start" && payload.prompt === "again");
  if (settled.length !== 2 || settled.some((payload) => Math.abs(payload.total_cost_usd - 1.55) > 1e-9) ||
      Math.abs((next?.total_cost_usd ?? 0) - 1.55) > 1e-9) {{
    throw new Error(`standalone usage missing or counted twice across settle/next envelope: ${{JSON.stringify({{ settled, next }})}}`);
  }}
}}
if (Math.abs(byEvent.turn_end.total_cost_usd - 0.9) > 1e-9) {{
  throw new Error(`turn_end omitted new usage or recounted hydration: ${{JSON.stringify(byEvent.turn_end)}}`);
}}
if (Math.abs(boundary.total_cost_usd - 1.15) > 1e-9) {{
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
if (childStarts.length !== 3 || childStops.length !== 2) {{
  throw new Error(`primary sessions self-reported or a child feed was lost: ${{JSON.stringify({{ childStarts, childStops }})}}`);
}}
for (const child of [...childStarts, ...childStops]) {{
  if (child.subagent_source !== "pi-session" || "model" in child ||
      "total_cost_usd" in child || "total_tokens" in child) {{
    throw new Error(`child payload is not lean: ${{JSON.stringify(child)}}`);
  }}
}}
const inProcessStarts = childStarts.filter((child) => child.subagent_id === "sess-child");
const inProcessLabels = inProcessStarts.map((child) => child.subagent_label).sort().join("|");
const inProcessStop = childStops.find((child) => child.subagent_id === "sess-child");
if (inProcessStarts.some((child) => child.session_id !== "sess-2") ||
    inProcessLabels !== "general-purpose#abc123|reviewer: fix the parser" ||
    inProcessStop?.errored !== true) {{
  throw new Error(`in-process child self-identification was ${{JSON.stringify({{ inProcessStarts, inProcessStop }})}}`);
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
            serde_json::to_string(&fast_supported).unwrap(),
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
}
