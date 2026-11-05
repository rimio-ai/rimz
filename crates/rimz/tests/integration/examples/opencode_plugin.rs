//! Node-driven coverage for the installed OpenCode 2 plugin.

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
        crate::common::skip("node not on PATH");
        return;
    };
    if !capability.status.success() {
        crate::common::skip("node cannot strip TypeScript syntax");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let plugin_path = dir.path().join("index.ts");
    let capture_path = dir.path().join("capture.jsonl");
    let stub_path = dir.path().join("rimz-capture");
    std::fs::write(
        &plugin_path,
        include_str!("../../../src/agents/adapters/opencode/index.ts"),
    )
    .unwrap();
    // The stub holds the "slow" ask's feed so an unchained reply would commit first.
    std::fs::write(&stub_path, "#!/bin/sh\npayload=$(cat)\ncase \"$payload\" in *question_ask*'\"request_id\":\"slow\"'*) sleep 0.3;; esac\nprintf '{\"pid\":\"%s\",\"envelope\":%s}\\n' \"$RIMZ_AGENT_PID\" \"$payload\" >> \"$RIMZ_CAPTURE\"\n").unwrap();
    std::fs::set_permissions(&stub_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let harness = dir.path().join("harness.mjs");
    std::fs::write(&harness, r#"
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import {pathToFileURL} from "node:url";
import childProcess from "node:child_process";
import {syncBuiltinESMExports} from "node:module";
let spawns = 0;
let releasePermission;
const feedsClosed = new Set();
const spawn = childProcess.spawn;
childProcess.spawn = (...args) => {
  spawns++;
  const child = spawn(...args);
  const end = child.stdin.end.bind(child.stdin);
  child.stdin.end = (line) => {
    const payload = JSON.parse(line);
    child.on("close", () => {
      if (payload.hook_event_name === "permission_ask") feedsClosed.add(payload.request_id);
    });
    if (autoApprove && payload.request_id === "permission" && !releasePermission) {
      releasePermission = () => end(line);
      return child.stdin;
    }
    return end(line);
  };
  return child;
};
syncBuiltinESMExports();
const [pluginPath, capturePath, binary, directory, marker] = process.argv.slice(2);
const autoApprove = marker === "1";
if (marker !== "absent") process.env.RIMZ_LAUNCH_AUTO_APPROVE = marker;
const inheritedMarker = process.env.RIMZ_LAUNCH_AUTO_APPROVE;
process.env.RIMZ_BIN = binary;
process.env.RIMZ_CAPTURE = capturePath;
delete process.env.RIMZ_WORKSPACE_ID;
const reminders = "<system_reminder>room rules</system_reminder>";
process.env.RIMZ_LAUNCH_REMINDERS = reminders;
const {default: plugin} = await import(pathToFileURL(pluginPath));
assert.equal(plugin.id, "rimz");
assert.equal(typeof plugin.setup, "function", "v2 loader requires setup");
assert.equal("server" in plugin, false);
assert.equal(await plugin.setup({}), undefined, "outside-room setup must be inert");
assert.equal(process.env.RIMZ_LAUNCH_REMINDERS, reminders);
assert.equal(process.env.RIMZ_LAUNCH_AUTO_APPROVE, inheritedMarker);
process.env.RIMZ_WORKSPACE_ID = "workspace";
process.env.RIMZ_BIN = directory + "/missing-rimz";
assert.equal(await plugin.setup({}), undefined, "missing executable must be inert");
assert.equal(process.env.RIMZ_LAUNCH_AUTO_APPROVE, inheritedMarker);
process.env.RIMZ_BIN = binary;
const argv = process.argv;
process.argv = [...argv, "serve", "--service"];
assert.equal(await plugin.setup({}), undefined, "shared service setup must be inert");
assert.equal(process.env.RIMZ_LAUNCH_REMINDERS, reminders);
assert.equal(process.env.RIMZ_LAUNCH_AUTO_APPROVE, inheritedMarker);
process.argv = argv;

const hooks = new Map();
const replies = [], replyErrors = [];
const stderr = console.error;
console.error = (...args) => replyErrors.push(args.join(" "));
let subscriptions = 0, catalogReads = 0, deliver, handled;
let metadata = {title: "New session - 2026-08-27T05:12:33.123Z", agent: "plan"};
let stalled = false;
async function register(name, callback) {
  assert.equal(hooks.has(name), false, "duplicate hook registration: " + name);
  hooks.set(name, callback);
  return {dispose: async () => hooks.delete(name)};
}
const ctx = {
  location: {directory},
  session: {get: async () => stalled ? await new Promise(() => {}) : metadata, hook: register},
  tool: {hook: register},
  permission: {reply: async (request) => {
    replies.push(request);
    if (request.requestID === "permission") {
      check("ask feed closes before reply", () => assert.equal(feedsClosed.has("permission"), true));
    }
    if (request.requestID === "rejected") throw new Error("reply unavailable");
    if (request.requestID === "gone") throw {_tag: "PermissionNotFoundError"};
  }},
  model: {list: async () => {
    catalogReads++;
    return {data: [
      {id: "gpt-5", providerID: "openai", name: "GPT-5", limit: {input: 200000, context: 400000}},
      {id: "gpt-5-mini", providerID: "openai", name: "GPT-5 Mini", limit: {context: 128000}},
      {id: "negative-input", providerID: "openai", name: "Fallback", limit: {input: -1, context: 100}},
      {id: "negative-context", providerID: "openai", name: "Unknown", limit: {context: -1}},
    ]};
  }},
  event: {subscribe: async function* ({signal}) {
    subscriptions++;
    while (!signal.aborted) {
      const event = await new Promise(resolve => {
        deliver = resolve;
        signal.addEventListener("abort", () => resolve(undefined), {once: true});
      });
      if (!event) return;
      yield event;
      handled?.();
      handled = undefined;
    }
  }},
};
async function eventually(check) {
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    const value = await check();
    if (value) return value;
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  throw new Error("timed out waiting for plugin output: " + JSON.stringify(await payloads()));
}
async function payloads() {
  try {
    return (await fs.readFile(capturePath, "utf8")).trim().split("\n").filter(Boolean).map(line => {
      const record = JSON.parse(line);
      assert.equal(record.pid, String(process.ppid));
      return record.envelope;
    });
  } catch (error) {
    if (error.code === "ENOENT") return [];
    throw error;
  }
}
async function emit(type, data, location = directory) {
  await eventually(() => deliver);
  const done = new Promise(resolve => { handled = resolve; });
  const send = deliver;
  deliver = undefined;
  send({type, data, created: Date.now(), location: {directory: location}});
  await done;
}
async function envelope(name, session = "root", count = 1) {
  return eventually(async () => {
    const found = (await payloads()).filter(p => p.hook_event_name === name && p.session_id === session);
    return found.length >= count && found[count - 1];
  });
}
const regressions = [];
function check(name, test) {
  try { test(); } catch (error) { regressions.push(name + ": " + error.message); }
}
const cleanup = await plugin.setup(ctx);
await plugin.setup(ctx);
assert.equal(subscriptions, 1);
assert.equal(hooks.size, 3);
assert.equal(process.env.RIMZ_LAUNCH_REMINDERS, undefined);
check("auto-approve marker claimed", () => assert.equal(process.env.RIMZ_LAUNCH_AUTO_APPROVE, undefined));
await emit("session.created", {sessionID: "root", location: {directory}, version: "2.0.27", agent: "plan"});
const root = await envelope("session_created");
assert.equal(root.cwd, directory);
assert.equal(root.agent_version, "2.0.27");
assert.equal(root.session_name, undefined);

const system = [];
await hooks.get("context")({sessionID: "root", system});
assert.deepEqual(system, [{type: "text", text: reminders}]);
metadata = {title: "Fix OpenCode metadata", agent: "plan", model: {id: "gpt-5", providerID: "openai"}};
await hooks.get("prompt")({sessionID: "root", prompt: {text: "fix it"}});
assert.equal((await envelope("chat_message")).prompt, "fix it");
await envelope("session_updated");
await emit("session.step.started", {sessionID: "root", assistantMessageID: "m1", agent: "plan", model: {id: "gpt-5", providerID: "openai"}});
await emit("session.step.ended", {sessionID: "root", assistantMessageID: "m1", tokens: {input: 100, output: 20, reasoning: 5, cache: {read: 30, write: 40}}});
await emit("session.execution.succeeded", {sessionID: "root"});
const idle = await envelope("session_idle");
for (const [field, expected] of Object.entries({input_tokens:100, output_tokens:25, cache_read_input_tokens:30, cache_write_input_tokens:40, total_tokens:195, context_window:200000, plan_proposed:true})) {
  assert.equal(idle[field], expected, field);
}
assert.equal(idle.session_name, "Fix OpenCode metadata");
assert.equal(idle.agent_version, "2.0.27");
assert.equal(idle.model_display_name, "GPT-5");

await emit("session.step.started", {sessionID:"root", assistantMessageID:"failed", agent:"plan", model:{id:"gpt-5-mini", providerID:"openai"}});
await emit("session.step.failed", {sessionID:"root", assistantMessageID:"failed", tokens:{input:7, output:3}});
await emit("session.execution.interrupted", {sessionID:"root"});
const failedStep = await envelope("session_idle", "root", 2);
check("A2 failed step usage", () => assert.equal(failedStep.total_tokens, 10));
// A later unmatched terminal must not recover a stale failed step's model.
await emit("session.model.selected", {sessionID:"root", model:{id:"negative-input", providerID:"openai"}});
await emit("session.step.ended", {sessionID:"root", assistantMessageID:"failed", tokens:{input:1}});
await hooks.get("execute.after")({sessionID:"root", tool:"probe", status:"completed"});
const fallback = await envelope("tool_after");
check("A2 failed step cleanup", () => assert.equal(fallback.model, "negative-input"));
check("A3 positive context fallback", () => assert.equal(fallback.context_window, 100));
await emit("session.model.selected", {sessionID:"root", model:{id:"negative-context", providerID:"openai"}});
await hooks.get("execute.after")({sessionID:"root", tool:"probe", status:"completed"});
const invalidWindow = await envelope("tool_after", "root", 2);
check("A3 invalid context omitted", () => assert.equal(invalidWindow.context_window, undefined));

metadata = {parentID:"root", title:"existing child", model:{id:"gpt-5-mini", providerID:"openai"}};
const resumedChildSystem = [];
await hooks.get("context")({sessionID:"resumed-child", system:resumedChildSystem});
await hooks.get("prompt")({sessionID:"early-child", prompt:{text:"child prompt"}});
check("A5 resumed child reminders", () => assert.deepEqual(resumedChildSystem, []));
metadata = {title:"Fix OpenCode metadata", agent:"plan", model:{id:"gpt-5", providerID:"openai"}};

await emit("session.created", {sessionID: "child", parentID: "root", title: "review auth", location: {directory}, version: "2.0.27"});
assert.equal((await envelope("SubagentStart", "child")).prompt, "review auth");
const childSystem = [];
await hooks.get("context")({sessionID: "child", system: childSystem});
assert.deepEqual(childSystem, []);
await emit("session.step.started", {sessionID: "child", assistantMessageID: "c1", agent: "build", model: {id: "gpt-5-mini", providerID: "openai"}});
await emit("session.step.ended", {sessionID: "child", assistantMessageID: "c1", tokens: {input: 2, output: 1, cache: {read: 0, write: 0}}});
assert.equal((await envelope("SubagentStart", "child", 2)).model, "gpt-5-mini");
await emit("session.model.selected", {sessionID: "child", model: {id: "gpt-5-mini", providerID: "openai"}});
await hooks.get("execute.after")({sessionID: "child", tool: "bash", status: "completed"});
await emit("session.execution.failed", {sessionID: "child", error: {type:"ApiError", message:"failed"}});
assert.equal((await envelope("SubagentStop", "child")).is_error, true);

await hooks.get("execute.after")({sessionID:"root", tool:"edit", status:"error"});
assert.equal((await envelope("tool_after", "root", 3)).is_error, true);
await emit("form.created", {form: {id:"form", sessionID:"root", title:"Choose", fields:[
  {key:"q0", type:"multiselect", title:"Options", description:"Which options should we use?", options:[{value:"A", label:"A", description:"First option"}], custom:true},
  {key:"q1", type:"string", title:"Proceed", description:"Should we continue?", options:[], custom:true},
  {key:"q2", type:"string", title:"Note", description:"Any notes?", options:[], custom:true},
]}});
const question = await envelope("question_ask");
assert.equal(question.title, "Choose");
assert.equal(question.questions[0].multiple, true);
assert.equal(question.questions[0].options[0].label, "A");
check("B2 question", () => assert.equal(question.questions[0].question, "Which options should we use?"));
check("B2 header", () => assert.equal(question.questions[0].header, "Options"));
assert.deepEqual(question.questions[0].options[0], {value:"A", label:"A", description:"First option"});
await emit("form.replied", {id:"form", sessionID:"root", answer:{q1:"yes", q0:["A","B"]}});
assert.deepEqual((await envelope("question_replied")).answers, [["A","B"],["yes"],[]]);
await emit("form.cancelled", {id:"cancelled", sessionID:"root"});
await envelope("question_rejected");
await emit("form.created", {form: {id:"slow", sessionID:"root", title:"Slow", fields:[]}});
await emit("form.cancelled", {id:"slow", sessionID:"root"});
await envelope("question_rejected", "root", 2);
assert.deepEqual((await payloads()).filter(p => p.request_id === "slow").map(p => p.hook_event_name),
  ["question_ask", "question_rejected"], "a reply must not commit ahead of its own ask");
await emit("permission.asked", {id:"permission", sessionID:"root", action:"bash", resources:["git status"], message:"Run command"});
if (autoApprove) {
  assert.deepEqual(replies, [], "reply must wait for the ask feed without blocking events");
  releasePermission();
}
const permission = await envelope("permission_ask");
assert.equal(permission.tool_name, "bash");
assert.deepEqual(permission.patterns, ["git status"]);
await emit("permission.asked", {id:"permission", sessionID:"root", action:"bash", resources:[]});
await emit("permission.asked", {id:"child-permission", sessionID:"child", action:"bash", resources:[]});
await emit("permission.asked", {id:"foreign-permission", sessionID:"foreign", action:"bash", resources:[]}, "/elsewhere");
await emit("permission.asked", {id:"rejected", sessionID:"root", action:"bash", resources:[]});
await emit("permission.asked", {id:"gone", sessionID:"root", action:"bash", resources:[]});
await emit("permission.replied", {requestID:"permission", sessionID:"root", reply:"once"});
assert.equal((await envelope("permission_replied")).reply, "once");
await emit("session.compaction.started", {sessionID:"root"});
await envelope("session_compacting");
await emit("session.compaction.failed", {sessionID:"root"});
assert.equal((await envelope("session_compacted")).is_error, true);
await emit("session.execution.failed", {sessionID:"root", error:{type:"ProviderAuthError", message:"expired"}});
const failure = await envelope("session_error");
assert.equal(failure.error_class, "ProviderAuthError");
assert.equal(failure.error_message, "expired");
if (autoApprove) {
  try { await eventually(() => replies.length === 5); }
  catch (error) { regressions.push("auto-replies settled: " + error.message); }
}
stalled = true;
await emit("session.created", {sessionID:"stalled", location:{directory}, version:"2.0.27"});
await envelope("session_created", "stalled");
await emit("session.execution.interrupted", {sessionID:"stalled"});
await envelope("session_idle", "stalled");
await emit("session.deleted", {sessionID:"stalled"});
assert.equal((await envelope("session_ended", "stalled")).reason, "deleted");
await emit("session.created", {sessionID:"foreign", location:{directory:"/elsewhere"}, version:"2.0.27"}, "/elsewhere");
await cleanup();
await envelope("session_ended");
const all = await payloads();
check("permission replies", () => assert.deepEqual(
  replies.map(reply => reply.requestID).sort(),
  autoApprove ? ["child-permission", "foreign-permission", "gone", "permission", "rejected"] : [],
));
for (const reply of replies) {
  assert.deepEqual(reply, {sessionID: reply.requestID === "child-permission" ? "child" : reply.requestID === "foreign-permission" ? "foreign" : "root", requestID: reply.requestID, decision:"once"});
}
check("reply failure is diagnostic only", () => assert.equal(replyErrors.length, autoApprove ? 1 : 0, JSON.stringify(replyErrors)));
if (autoApprove) check("reply error logged", () => assert.match(replyErrors[0] || "", /reply unavailable/));
assert.equal(all.some(p => p.request_id === "child-permission" || p.request_id === "foreign-permission"), false);
console.error = stderr;
assert.equal(all.filter(p => p.hook_event_name === "SubagentStart" && p.session_id === "child").length, 2, JSON.stringify(all));
assert.equal(all.some(p => p.hook_event_name === "tool_after" && p.session_id === "child"), false);
assert.equal(all.some(p => p.session_id === "foreign"), false);
assert.equal(catalogReads, 1);
assert.equal(hooks.size, 0);
check("A5 early child prompt", () => assert.equal(all.some(p => p.session_id === "early-child" && p.hook_event_name === "chat_message"), false));

// A catalog lookup already in flight must not announce a child after disposal.
stalled = false;
let releaseCatalog;
ctx.model.list = () => new Promise(resolve => {releaseCatalog = resolve;});
const cleanupDelayed = await plugin.setup(ctx);
const delayed = emit("session.created", {sessionID:"delayed", parentID:"root", location:{directory}, model:{id:"gpt-5", providerID:"openai"}});
await eventually(() => releaseCatalog);
await cleanupDelayed();
const spawnsAfterDispose = spawns;
releaseCatalog({data:[]});
await delayed;
check("A6 no post-dispose child", () => assert.equal(spawns, spawnsAfterDispose));

ctx.model.list = () => { throw new Error("catalog unavailable"); };
deliver = undefined;
const cleanupErrors = await plugin.setup(ctx);
await eventually(() => deliver);
deliver({type:"session.model.selected", data:{sessionID:"root", model:{id:"gpt-5", providerID:"openai"}}});
deliver = undefined;
try {
  await eventually(() => deliver);
  await emit("session.execution.interrupted", {sessionID:"root"});
  await envelope("session_idle", "root", 3);
} catch (error) { regressions.push("A4 event failure must not end subscription: " + error.message); }
await cleanupErrors();
assert.deepEqual(regressions, [], regressions.join("\n"));
"#).unwrap();
    for marker in ["1", "absent", "true"] {
        let _ = std::fs::remove_file(&capture_path);
        let output = std::process::Command::new("node")
            .arg("--experimental-strip-types")
            .arg(&harness)
            .args([&plugin_path, &capture_path, &stub_path, dir.path()])
            .arg(marker)
            .env_remove("RIMZ_LAUNCH_AUTO_APPROVE")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "node harness failed for OpenCode (auto-approve {marker})\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
#[cfg(unix)]
fn client_tracks_focus_and_background_asks_without_navigation_edges() {
    let capability = std::process::Command::new("node")
        .args([
            "--experimental-strip-types",
            "--input-type=module-typescript",
            "--eval",
            "const n: number = 1;",
        ])
        .output();
    let Ok(capability) = capability else {
        crate::common::skip("node not on PATH");
        return;
    };
    if !capability.status.success() {
        crate::common::skip("node cannot strip TypeScript syntax");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let plugin = dir.path().join("tui.ts");
    let harness = dir.path().join("harness.mjs");
    std::fs::write(
        &plugin,
        include_str!("../../../src/agents/adapters/opencode/tui.ts"),
    )
    .unwrap();
    std::fs::write(&harness, include_str!("opencode_tui.mjs")).unwrap();
    let output = std::process::Command::new("node")
        .arg("--experimental-strip-types")
        .arg(&harness)
        .arg(&plugin)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "client harness failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
