//! Node-driven delivery and lifecycle coverage for OMP's passive extension.

#[test]
#[cfg(unix)]
fn extension_orders_delivery_without_blocking_native_handlers() {
    use crate::common::Env;
    use serde_json::Value;

    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        crate::common::skip("node not on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let extension = dir.path().join("rimz.mjs");
    std::fs::write(
        &extension,
        include_str!("../../../src/agents/adapters/omp/extension.ts"),
    )
    .unwrap();
    let harness = dir.path().join("harness.mjs");
    std::fs::write(&harness, r#"
import assert from "node:assert/strict";
import childProcess from "node:child_process";
import {EventEmitter} from "node:events";
import {syncBuiltinESMExports} from "node:module";
import {pathToFileURL} from "node:url";
const delivered = [];
let complete;
let registrationStarted;
let finishRegistration;
const registration = new Promise(resolve => { registrationStarted = resolve; });
const waitFor = count => delivered.length >= count ? Promise.resolve() : new Promise(resolve => {
  complete = () => { if (delivered.length >= count) resolve(); };
});
process.env.RIMZ_BIN = "capture";
process.env.RIMZ_LAUNCH_REMINDERS = "remember";
childProcess.spawn = (binary, args, options) => {
  assert.equal(binary, "capture");
  assert.deepEqual(args, ["hooks", "feed", "--source", "omp"]);
  assert.deepEqual(options.stdio, ["pipe", "pipe", "pipe"]);
  assert.equal(options.env.RIMZ_AGENT_PID, String(process.pid));
  const child = new EventEmitter();
  child.stdout = {resume() {}};
  child.stderr = {resume() {}};
  child.unref = () => {};
  child.stdin = new EventEmitter();
  child.stdin.end = body => {
    const payload = JSON.parse(body);
    const deliver = () => {
      delivered.push(payload);
      child.emit("close", 0);
      complete?.();
    };
    if (payload.hook_event_name === "session_start" && payload.session_id === "main-two") {
      finishRegistration = deliver;
      registrationStarted();
    } else {
      setTimeout(deliver, payload.hook_event_name === "session_start" ? 80 : 0);
    }
  };
  return child;
};
syncBuiltinESMExports();
const {default: rimz} = await import(pathToFileURL(process.argv[2]));
const handlers = new Map();
let effort = "high";
let usage = {percent: 0, tokens: 0, contextWindow: 32768};
rimz({on: (name, handler) => handlers.set(name, handler), getThinkingLevel: () => effort});
let id = "main-one";
const ctx = {agent: {kind: "main"}, hasUI: true, model: {id: "probe"},
  sessionManager: {getSessionId: () => id, getCwd: () => process.argv[3], getBranch: () => []},
  getContextUsage: () => usage};
const fire = (name, event = {}) => handlers.get(name)(event, ctx);
assert.equal(fire("session_start"), undefined);
assert.deepEqual(fire("before_agent_start", {prompt: "hello", systemPrompt: ["original"]}),
  {systemPrompt: ["original", "remember"]});
assert.equal(process.env.RIMZ_LAUNCH_REMINDERS, undefined);
await waitFor(2);
assert.equal(fire("session_switch"), undefined);
id = "main-two";
fire("session_switch", {reason: "resume"});
ctx.model = {id: "restored", contextWindow: 65536};
effort = "low";
usage = {percent: 50, tokens: 32768, contextWindow: 65536};
fire("before_agent_start", {prompt: "again", systemPrompt: []});
assert.equal(fire("tool_call", {toolName: "ask", toolCallId: "q", input: {questions: [{question: "Which?"}]}}), undefined);
fire("agent_end", {messages: [{role: "assistant", stopReason: "stop", content: "Done"}]});
fire("tool_execution_end", {toolName: "ask", toolCallId: "q"});
fire("tool_approval_resolved", {toolCallId: "p"});
for (const handler of handlers.values()) {
  assert.equal(handler({}, {...ctx, agent: {kind: "sub"}}), undefined);
}
await registration;
const shutdown = fire("session_shutdown");
assert(shutdown instanceof Promise, "shutdown must let OMP await queued delivery");
let shutdownResolved = false;
shutdown.then(() => { shutdownResolved = true; });
await Promise.resolve();
assert.equal(shutdownResolved, false, "shutdown must wait behind the in-flight helper");
finishRegistration();
await shutdown;
assert.equal(delivered.at(-1).hook_event_name, "session_shutdown", "shutdown must resolve after its envelope is delivered");
const switched = delivered.find(event => event.hook_event_name === "session_start" && event.session_id === "main-two");
assert.deepEqual(Object.keys(switched).sort(), ["cwd", "hook_event_name", "session_id"],
  "switch registration must not publish unrestored context");
const started = delivered.find(event => event.hook_event_name === "before_agent_start" && event.session_id === "main-two");
assert.equal(started.model, "restored");
assert.equal(started.effort, "low");
assert.equal(started.context_pct, 50);
assert.equal(started.context_window, 65536);
assert.equal(started.total_tokens, 32768);
assert(delivered.every(event => !("total_cost_usd" in event)));
console.log(JSON.stringify(delivered));
"#).unwrap();
    let env = Env::new();
    env.record(&env.project_root);
    let output = std::process::Command::new("node")
        .arg(&harness)
        .arg(&extension)
        .arg(&env.project_root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        events.len(),
        8,
        "same-id switches and late closes are neutral"
    );
    for event in &events[..2] {
        let output = env.run_hook("omp", &event.to_string());
        assert!(output.status.success(), "{output:?}");
        assert!(output.stdout.is_empty());
    }
    assert_eq!(
        env.snapshot_json()["agents"][0]["status"],
        "running",
        "a slow registration helper must not reset a started turn to idle"
    );
    let names = events
        .iter()
        .map(|event| {
            (
                event["hook_event_name"].as_str().unwrap(),
                event["session_id"].as_str().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            ("session_start", "main-one"),
            ("before_agent_start", "main-one"),
            ("session_shutdown", "main-one"),
            ("session_start", "main-two"),
            ("before_agent_start", "main-two"),
            ("tool_call", "main-two"),
            ("agent_end", "main-two"),
            ("session_shutdown", "main-two"),
        ]
    );
    for event in &events[2..events.len() - 1] {
        assert!(env.run_hook("omp", &event.to_string()).status.success());
    }
    let settled = env.snapshot_json();
    assert!(
        settled["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|agent| agent["status"] == "success"),
        "{settled}"
    );
    assert!(
        env.run_hook("omp", &events.last().unwrap().to_string())
            .status
            .success()
    );
}
