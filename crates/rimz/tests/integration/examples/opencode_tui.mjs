import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import childProcess from "node:child_process";
import { syncBuiltinESMExports } from "node:module";
import { pathToFileURL } from "node:url";

const intervals = new Set();
globalThis.setInterval = (fn) => { intervals.add(fn); return fn; };
globalThis.clearInterval = (fn) => intervals.delete(fn);
let records = [], held = [], cleanups = [], holdFeeds = false;
childProcess.spawn = (binary, argv, options) => {
  assert.equal(binary, process.execPath);
  assert.deepEqual(argv, ["hooks", "feed", "--source", "opencode"]);
  assert.deepEqual(options.stdio, ["pipe", "ignore", "ignore"]);
  const child = new EventEmitter();
  child.stdin = new EventEmitter();
  child.stdin.end = (line) => {
    records.push({ payload: JSON.parse(line), env: options.env });
    if (holdFeeds) held.push(() => child.emit("close", 0));
    else setImmediate(() => child.emit("close", 0));
  };
  return child;
};
syncBuiltinESMExports();
process.env.RIMZ_BIN = process.execPath;
const { default: plugin } = await import(pathToFileURL(process.argv[2]));
const flush = async () => { for (let i = 0; i < 30; i++) await new Promise(setImmediate); };
const tick = async () => { for (const fn of intervals) fn(); await flush(); };
const model = { id: "m", providerID: "p" };
const usage = { input: 100, output: 20, reasoning: 5, cache: { read: 30, write: 40 } };
const assistant = (tokens, id = "msg") => ({ type: "assistant", id, model, tokens, time: { created: 1 }, content: [] });
const permission = (id, sessionID) => ({ id, sessionID, action: "bash", resources: ["git status"], message: "Run command" });
const form = (id, sessionID) => ({ id, sessionID, title: "Choose", fields: [
  { key: "choice", type: "multiselect", title: "Options", description: "Which?", options: [{ value: "A", label: "A" }], custom: true },
] });
const failures = [];
async function test(name, run) {
  records = []; held = []; cleanups = []; holdFeeds = false; intervals.clear();
  process.env.RIMZ_WORKSPACE_ID = "workspace";
  try { await run(); console.log("ok " + name); } catch (error) { failures.push(name + ": " + error.stack); }
  finally { for (const cleanup of cleanups) await cleanup?.(); }
}
function fixture() {
  let route = { type: "session", sessionID: "A" }, listener, reads = 0;
  const infos = new Map(["A", "B", "C"].map(id => [id, { id, title: id, version: "2.1.4", agent: "build", location: { directory: "/tmp" }, model }]));
  const permissions = new Map(), forms = new Map(), messages = new Map(), statuses = new Map(), active = new Map();
  const ctx = {
    keymap: { commands: () => [{ id: "service.restart" }] },
    ui: { router: { current: () => route }, tabs: { list: () => [{ sessionID: "A" }, { sessionID: "B" }] } },
    data: {
      listen: (fn) => { reads++; listener = fn; return () => { listener = undefined; }; },
      session: {
        sync: async () => { reads++; }, get: id => infos.get(id), status: id => statuses.get(id) || "idle",
        message: { list: id => messages.get(id) || [], sync: async () => {} },
        permission: { list: id => permissions.get(id) || [], sync: async () => {} },
        form: { list: id => forms.get(id) || [], sync: async () => {} },
      },
      location: { model: { list: () => [{ ...model, name: "Model", limit: { input: 200000, context: 400000 } }], sync: async () => {} } },
    },
    client: {
      message: { list: async () => ({ data: [], cursor: {} }) },
      session: { active: async () => Object.fromEntries(active) },
    },
  };
  const hydration = { read: () => ctx.client.session.active() };
  return {
    ctx, infos, permissions, forms, messages, statuses, active, hydration,
    select: id => { route = id ? { type: "session", sessionID: id } : { type: "home" }; },
    reads: () => reads, listening: () => !!listener,
    emit: async (type, data) => {
      if (type === "session.deleted") {
        for (const cache of [infos, permissions, forms, messages, statuses, active]) cache.delete(data.sessionID);
      }
      if (type === "server.connected") {
        void hydration.read().then(snapshot => {
          statuses.clear();
          for (const id of Object.keys(snapshot)) statuses.set(id, "running");
        }).catch(() => {});
      }
      listener?.({ name: type, details: { type, data } }); await flush();
    },
    start: async () => { const cleanup = await plugin.setup(ctx); cleanups.push(cleanup); await tick(); return cleanup; },
  };
}
const payloads = () => records.map(r => r.payload);
const last = () => payloads().at(-1);

await test("inert gates", async () => {
  const f = fixture();
  delete process.env.RIMZ_WORKSPACE_ID;
  await f.start();
  assert.equal(f.reads(), 0);
  process.env.RIMZ_WORKSPACE_ID = "workspace";
  f.ctx.keymap.commands = () => [];
  const cleanup = await f.start();
  assert.equal(f.reads(), 0);
  f.ctx.keymap.commands = () => [{ id: "service.restart" }];
  await tick();
  assert.equal(last()?.hook_event_name, "client_idle", "gate is checked after initialization");
  await cleanup?.();
  const standalone = fixture();
  process.argv.push("--standalone");
  await standalone.start();
  process.argv.pop();
  assert.equal(standalone.reads(), 0);
});
await test("arrival and identity", async () => {
  for (const key of ["RIMZ_AGENT_ID", "RIMZ_AGENT_KIND", "RIMZ_AGENT_NAME", "RIMZ_AGENT_PROFILE", "RIMZ_AGENT_ROLE", "RIMZ_AGENT_MODEL", "RIMZ_AGENT_EFFORT", "RIMZ_AGENT_BUDGET", "RIMZ_AGENT_FUTURE", "RIMZ_HOOK_OWNER_KIND", "RIMZ_RUN_ID", "RIMZ_WORKFLOW_RUN", "RIMZ_TEAM", "RIMZ_LAUNCH_GROUP", "RIMZ_LAUNCH_ORDINAL", "RIMZ_RUNTIME_ENV", "RIMZ_LOOP_TASK"]) process.env[key] = "inherited";
  process.env.RIMZ_CHANNEL = "#lane";
  process.env.TMUX_PANE = "%5";
  const f = fixture(); f.messages.set("A", [assistant(usage)]);
  const cleanup = await f.start();
  assert.equal(last()?.hook_event_name, "client_idle");
  assert.equal(last().session_id, "A"); assert.equal(last().on_screen, true);
  assert.equal(last().total_tokens, 195); assert.equal(last().context_window, 200000);
  assert.equal(last().session_name, "A");
  const env = records[0].env;
  assert.equal(env.RIMZ_AGENT_PID, String(process.pid));
  for (const [key, value] of Object.entries(process.env)) {
    if (value === "inherited") assert.equal(env[key], "", key);
  }
  assert.equal(env.RIMZ_CHANNEL, "#lane"); assert.equal(env.TMUX_PANE, "%5");
  await cleanup?.();
});
await test("running arrival and navigation", async () => {
  const f = fixture(); f.statuses.set("A", "running"); f.active.set("A", { type: "running" });
  const cleanup = await f.start();
  assert.equal(last()?.hook_event_name, "client_running");
  f.select("B"); await tick(); f.select("A"); await tick();
  assert.deepEqual(payloads().map(p => p.session_id), ["A", "B", "A"]);
  f.select(undefined); await tick(); await cleanup?.();
  assert.equal(payloads().some(p => p.hook_event_name === "session_ended"), false);
  assert.equal(f.listening(), false);
});
await test("permission arrival not repeated and background reply", async () => {
  const f = fixture(); f.permissions.set("A", [permission("ask", "A")]);
  const cleanup = await f.start();
  assert.equal(last()?.hook_event_name, "permission_ask");
  f.select("B"); await tick(); f.select("A"); await tick();
  assert.equal(payloads().filter(p => p.request_id === "ask").length, 1);
  f.select("B"); await tick();
  f.ctx.ui.tabs.list = () => [{ sessionID: "B" }];
  await f.emit("permission.replied", { sessionID: "A", requestID: "ask", reply: "once" });
  assert.equal(last().hook_event_name, "permission_replied"); assert.equal(last().on_screen, undefined);
  await cleanup?.();
});
await test("form arrival normalization", async () => {
  const f = fixture(); f.forms.set("A", [form("form", "A")]);
  const cleanup = await f.start();
  assert.equal(last()?.hook_event_name, "question_ask");
  assert.equal(last().questions[0].question, "Which?"); assert.equal(last().questions[0].multiple, true);
  await f.emit("form.replied", { id: "form", sessionID: "A", answer: { choice: ["A", "B"] } });
  assert.deepEqual(last().answers, [["A", "B"]]);
  await cleanup?.();
});
await test("exact background tabs and next ask", async () => {
  const f = fixture(); const cleanup = await f.start();
  await f.emit("permission.asked", permission("foreign", "C"));
  await f.emit("session.execution.failed", { sessionID: "B", error: { type: "Oops" } });
  assert.equal(payloads().length, 1);
  f.permissions.set("B", [permission("first", "B"), permission("second", "B")]);
  await f.emit("permission.asked", permission("first", "B"));
  assert.equal(last().hook_event_name, "permission_ask"); assert.equal(last().on_screen, undefined);
  f.permissions.set("B", [permission("second", "B")]);
  await f.emit("permission.replied", { sessionID: "B", requestID: "first", reply: "once" });
  assert.equal(last().request_id, "second");
  await f.emit("form.created", { form: form("background-form", "B") });
  assert.equal(last().hook_event_name, "question_ask"); assert.equal(last().on_screen, undefined);
  await f.emit("form.cancelled", { id: "background-form", sessionID: "B" });
  assert.ok(payloads().some(p => p.hook_event_name === "question_rejected"));
  await cleanup?.();
});
await test("stale read and failed state retry", async () => {
  const f = fixture(); let release;
  f.ctx.data.session.sync = id => id === "A" ? new Promise(resolve => { release = resolve; }) : Promise.resolve();
  const cleanup = await f.start(); f.select("B"); await tick();
  release?.(); await flush(); await tick();
  assert.equal(last()?.session_id, "B");
  assert.equal(payloads().some(p => p.session_id === "A"), false);
  f.ctx.data.session.sync = async () => { throw Error("offline"); };
  f.select("C"); const before = records.length; await tick(); assert.equal(records.length, before);
  f.ctx.data.session.sync = async () => {}; await tick();
  assert.equal(last().session_id, "C"); await cleanup?.();
});
await test("serial feeds and current stamp after queued spawn", async () => {
  const f = fixture(); holdFeeds = true;
  const cleanup = await f.start();
  assert.equal(records.length, 1, "arrival feed");
  await f.emit("session.inbox.enqueued", { sessionID: "A", item: { type: "user", payload: { text: "hello" } } });
  await f.emit("session.execution.started", { sessionID: "A" });
  assert.equal(records.length, 1, "feed must await prior close");
  f.select("B"); await tick();
  held.shift()(); await flush();
  assert.equal(last().hook_event_name, "chat_message"); assert.equal(last().prompt, "hello");
  assert.equal(last().on_screen, undefined, "queued event no longer owns the screen");
  while (held.length) { held.shift()(); await flush(); }
  await cleanup?.();
});
await test("step usage and mapped native events", async () => {
  const f = fixture(); const cleanup = await f.start();
  await f.emit("session.step.started", { sessionID: "A", assistantMessageID: "step", model, agent: "plan" });
  await f.emit("session.step.ended", { sessionID: "A", assistantMessageID: "step", tokens: usage });
  assert.equal(records.length, 1);
  await f.emit("session.tool.input.started", { sessionID: "A", assistantMessageID: "step", id: "tool", name: "bash" });
  await f.emit("session.tool.failed", { sessionID: "A", assistantMessageID: "step", id: "tool" });
  assert.equal(last().hook_event_name, "tool_after"); assert.equal(last().tool_name, "bash");
  assert.equal(last().total_tokens, 195); assert.equal(last().is_error, true);
  await f.emit("session.tool.success", { sessionID: "A", assistantMessageID: "step", id: "unknown" });
  assert.equal(last().tool_name, undefined);
  await f.emit("session.step.failed", { sessionID: "A", assistantMessageID: "zero", tokens: { input: 0 } });
  await f.emit("session.execution.succeeded", { sessionID: "A" });
  assert.equal(last().plan_proposed, true); assert.equal(last().total_tokens, 195);
  for (const [event, name] of [["session.renamed", "session_updated"], ["session.compaction.started", "session_compacting"], ["session.compaction.failed", "session_compacted"], ["session.execution.failed", "session_error"], ["session.deleted", "session_ended"]]) {
    await f.emit(event, { sessionID: "A", title: "Renamed", error: { type: "ApiError", message: "oops" } });
    assert.equal(last().hook_event_name, name, event); assert.equal(last().on_screen, true);
  }
  await cleanup?.();
});
await test("pagination past zero and short page", async () => {
  const f = fixture(); const queries = [];
  f.ctx.client.message.list = async query => {
    queries.push(query);
    return !query.cursor ? { data: [assistant({ input: 0 })], cursor: { next: "older" } } : { data: [assistant(usage, "old")], cursor: { next: "more" } };
  };
  const cleanup = await f.start();
  assert.equal(last()?.total_tokens, 195);
  assert.equal(queries.length, 2); assert.equal(queries[0].limit, 200);
  assert.equal(queries[0].order, "desc"); assert.equal(queries[1].order, undefined);
  await cleanup?.();
});
await test("reconnect clears stale closed-tab ask", async () => {
  const f = fixture(); const cleanup = await f.start();
  f.permissions.set("B", [permission("stale", "B")]);
  await f.emit("permission.asked", permission("stale", "B"));
  f.permissions.set("B", []); f.ctx.ui.tabs.list = () => [{ sessionID: "A" }];
  await f.emit("server.connected", {}); await tick();
  assert.ok(payloads().some(p => p.session_id === "B" && p.hook_event_name === "client_idle" && !p.on_screen));
  assert.equal(payloads().some(p => p.hook_event_name === "permission_replied"), false);
  await cleanup?.();
});
for (const leaveRoute of [false, true]) await test("evicted on-screen deletion" + (leaveRoute ? " after home route" : ""), async () => {
  const f = fixture(); await f.start();
  if (leaveRoute) { f.select(undefined); await tick(); }
  const before = records.length;
  await f.emit("session.deleted", { sessionID: "A" });
  const ended = payloads().slice(before);
  assert.equal(ended.length, 1, "deletion must end the last on-screen session after cache eviction");
  assert.equal(ended[0].hook_event_name, "session_ended");
  assert.equal(ended[0].session_id, "A"); assert.equal(ended[0].on_screen, true);
});
await test("evicted background asker deletion", async () => {
  const f = fixture(); await f.start();
  f.permissions.set("B", [permission("deleted-ask", "B")]);
  await f.emit("permission.asked", permission("deleted-ask", "B"));
  f.ctx.ui.tabs.list = () => [{ sessionID: "A" }];
  const before = records.length;
  await f.emit("session.deleted", { sessionID: "B" });
  const ended = payloads().slice(before);
  assert.equal(ended.length, 1, "deletion must retire a forwarded ask after cache eviction and tab close");
  assert.equal(ended[0].hook_event_name, "session_ended");
  assert.equal(ended[0].session_id, "B"); assert.equal(ended[0].on_screen, undefined);
});
await test("other evicted sessions stay silent", async () => {
  const f = fixture(); await f.start();
  const before = records.length;
  await f.emit("session.deleted", { sessionID: "B" });
  await f.emit("session.deleted", { sessionID: "C" });
  assert.equal(records.length, before, "deletion of an unreported background or foreign session stays silent");
});
for (const type of ["permission.replied", "form.replied", "form.cancelled"]) await test(type + " after eviction", async () => {
  const f = fixture(); await f.start();
  const permissionReply = type === "permission.replied";
  const request = permissionReply ? permission("evicted", "B") : form("evicted", "B");
  await f.emit(permissionReply ? "permission.asked" : "form.created", permissionReply ? request : { form: request });
  f.infos.delete("B"); f.ctx.ui.tabs.list = () => [{ sessionID: "A" }];
  const before = records.length;
  await f.emit(type, { sessionID: "B", requestID: "evicted", id: "evicted", reply: "once", answer: { choice: ["A"] } });
  const replies = payloads().slice(before);
  assert.equal(replies.length, 1, "a tracked reply must not require cached session info");
  assert.equal(replies[0].hook_event_name, permissionReply ? "permission_replied" : type === "form.replied" ? "question_replied" : "question_rejected");
  assert.equal(replies[0].request_id, "evicted"); assert.equal(replies[0].on_screen, undefined);
  if (type === "form.replied") assert.deepEqual(replies[0].answers, [["A"]]);
});
for (const activation of [true, false]) for (const kind of ["permission", "form"]) await test(kind + " sweep at " + (activation ? "activation" : "reconnect"), async () => {
  const f = fixture();
  if (!activation) await f.start();
  (kind === "permission" ? f.permissions : f.forms).set("B", [kind === "permission" ? permission("missed", "B") : form("missed", "B")]);
  if (activation) await f.start();
  else { await f.emit("server.connected", {}); await tick(); }
  const asks = payloads().filter(p => p.session_id === "B");
  assert.equal(asks.length, 1, "sweep must forward a missed background ask exactly once and no snapshot");
  assert.equal(asks[0].hook_event_name, kind === "permission" ? "permission_ask" : "question_ask");
  assert.equal(asks[0].on_screen, undefined);
  await f.emit("server.connected", {}); await tick();
  assert.equal(payloads().filter(p => p.request_id === "missed").length, 1, "sweep must not re-send a forwarded ask");
});
await test("sweep skips arrival and silent background tabs", async () => {
  const f = fixture(); let selectedSyncs = 0;
  f.permissions.set("A", [permission("selected", "A")]);
  f.ctx.data.session.permission.sync = async id => { if (id === "A") selectedSyncs++; };
  await f.start();
  assert.equal(selectedSyncs, 1, "arrival alone synchronizes the on-screen session");
  assert.deepEqual(payloads().map(p => [p.hook_event_name, p.session_id, p.on_screen]), [["permission_ask", "A", true]]);
});
await test("history enrichment outlives the arrival deadline", async () => {
  const f = fixture(); let historyCalls = 0;
  const synced = new Promise(resolve => { f.ctx.data.session.message.sync = async () => { await new Promise(done => setTimeout(done, 1100)); resolve(); }; });
  f.ctx.client.message.list = async () => { historyCalls++; return { data: [assistant(usage)], cursor: {} }; };
  await f.start(); await synced; await flush();
  assert.ok(historyCalls > 0, "slow cache sync must still page history after the arrival deadline");
  await f.emit("session.renamed", { sessionID: "A", title: "Late gauge" });
  assert.equal(last().total_tokens, 195, "late history usage rides the next real envelope");
});
for (const running of [false, true]) await test("arrival reads server status " + (running ? "running" : "idle"), async () => {
  const f = fixture();
  f.statuses.set("A", running ? "idle" : "running");
  if (running) f.active.set("A", { type: "running" });
  await f.start();
  assert.equal(last()?.hook_event_name, running ? "client_running" : "client_idle", "server status must supersede the stale cache");
});
await test("reconnect status does not wait for cache hydration", async () => {
  const f = fixture(); f.active.set("A", { type: "running" }); f.statuses.set("A", "running");
  await f.start(); f.active.clear();
  let release;
  f.hydration.read = () => new Promise(resolve => { release = resolve; });
  await f.emit("server.connected", {}); await tick();
  assert.equal(last()?.hook_event_name, "client_idle", "arrival must use server status while upstream hydration is pending");
  release({}); await flush();
});
await test("failed server status read retries without a snapshot", async () => {
  const f = fixture(); f.ctx.client.session.active = async () => { throw Error("offline"); };
  await f.start();
  assert.equal(records.length, 0, "failed state reads must not report idle");
  f.ctx.client.session.active = async () => ({}); await tick();
  assert.equal(last()?.hook_event_name, "client_idle");
});
await test("deletion during reconnect keeps the forwarded end", async () => {
  const f = fixture(); await f.start();
  f.permissions.set("B", [permission("deleting", "B")]);
  await f.emit("permission.asked", permission("deleting", "B"));
  let release;
  f.ctx.data.session.permission.sync = id => id === "B" ? new Promise(resolve => { release = resolve; }) : Promise.resolve();
  await f.emit("server.connected", {});
  assert.equal(typeof release, "function");
  const before = records.length;
  await f.emit("session.deleted", { sessionID: "B" });
  release(); await flush();
  assert.deepEqual(payloads().slice(before).filter(p => p.session_id === "B").map(p => [p.hook_event_name, p.on_screen]), [["session_ended", undefined]], "an awaited sweep must not clear the ask before its queued deletion ends it");
});
await test("deletion during arrival cannot revive the session", async () => {
  const f = fixture(); await f.start();
  let release;
  f.hydration.read = async () => ({});
  f.ctx.client.session.active = () => new Promise(resolve => { release = resolve; });
  await f.emit("server.connected", {}); await tick();
  assert.equal(typeof release, "function");
  const before = records.length;
  await f.emit("session.deleted", { sessionID: "A" });
  release({}); await flush();
  assert.deepEqual(payloads().slice(before).filter(p => p.session_id === "A").map(p => [p.hook_event_name, p.on_screen]), [["session_ended", true]], "a status read that outlives deletion must not revive the session");
});
assert.deepEqual(failures, [], failures.join("\n"));
