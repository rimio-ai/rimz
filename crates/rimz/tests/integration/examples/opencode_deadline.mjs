import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import childProcess from "node:child_process";
import { syncBuiltinESMExports } from "node:module";
import { pathToFileURL } from "node:url";

// tool name -> [delay ms, stdout] the fake feed answers a tool_after with.
const answers = {
  rung: [100, '{"additional_context":"2m left. Wrap up now."}\n'],
  stop: [0, '{"additional_context":"Time is up."}\n'],
  garbage: [0, "not json\n"],
  stall: [2500, '{"additional_context":"late rung"}\n'],
};
let records = [];
childProcess.spawn = (binary, argv, options) => {
  const child = new EventEmitter();
  child.stdin = new EventEmitter();
  child.stdout = options.stdio[1] === "pipe" ? Object.assign(new EventEmitter(), { setEncoding() { return this; } }) : null;
  child.stdin.end = (line) => {
    const payload = JSON.parse(line);
    records.push({ payload, stdio: options.stdio });
    const [delay, out] = (payload.hook_event_name === "tool_after" && answers[payload.tool_name]) || [0, ""];
    setTimeout(() => {
      if (child.stdout && out) child.stdout.emit("data", out);
      child.emit("close", 0);
    }, delay);
  };
  return child;
};
syncBuiltinESMExports();
process.env.RIMZ_BIN = process.execPath;
process.env.RIMZ_WORKSPACE_ID = "workspace";
const { default: plugin } = await import(pathToFileURL(process.argv[2]));

async function activate(supervised) {
  if (supervised) process.env.RIMZ_RUN_ID = "run-1";
  else delete process.env.RIMZ_RUN_ID;
  const hooks = new Map();
  let deliver;
  const ctx = {
    location: { directory: "/tmp" },
    session: {
      get: async () => ({}),
      hook: async (name, callback) => { hooks.set(name, callback); return { dispose: async () => hooks.delete(name) }; },
    },
    tool: { hook: async (name, callback) => { hooks.set(name, callback); return { dispose: async () => hooks.delete(name) }; } },
    model: { list: async () => ({ data: [] }) },
    permission: { reply: async () => {} },
    event: { subscribe: async function* ({ signal }) {
      while (!signal.aborted) {
        const event = await new Promise((resolve) => {
          deliver = resolve;
          signal.addEventListener("abort", () => resolve(undefined), { once: true });
        });
        if (!event) return;
        yield event;
      }
    } },
  };
  const cleanup = await plugin.setup(ctx);
  const emit = async (type, data) => {
    while (!deliver) await new Promise(setImmediate);
    const send = deliver;
    deliver = undefined;
    send({ type, data, location: { directory: "/tmp" } });
    while (!deliver) await new Promise(setImmediate);
  };
  const tool = (sessionID, name) => hooks.get("execute.after")({ sessionID, tool: name, status: "completed" });
  const system = async (sessionID) => {
    const system = [];
    await hooks.get("context")({ sessionID, system });
    return system.map((part) => part.text);
  };
  await emit("session.created", { sessionID: "root", location: { directory: "/tmp" } });
  await emit("session.created", { sessionID: "child", parentID: "root", location: { directory: "/tmp" } });
  return { cleanup, emit, tool, system };
}

const failures = [];
async function test(name, run) {
  records = [];
  try { await run(); } catch (error) { failures.push(name + ": " + error.stack); }
}

await test("a claimed rung reaches every later request of the turn", async () => {
  const { cleanup, emit, tool, system } = await activate(true);
  await tool("root", "quiet");
  assert.deepEqual(await system("root"), []);
  await tool("root", "rung");
  // The feed answers after the hook returns: the next request waits for it.
  assert.deepEqual(await system("root"), ["2m left. Wrap up now."]);
  assert.deepEqual(await system("root"), ["2m left. Wrap up now."]);
  await tool("root", "quiet");
  assert.deepEqual(await system("root"), ["2m left. Wrap up now."], "a silent feed keeps the newest rung");
  await tool("root", "garbage");
  assert.deepEqual(await system("root"), ["2m left. Wrap up now."], "unparsable stdout is no rung");
  await tool("root", "stop");
  assert.deepEqual(await system("root"), ["Time is up."], "a newer rung replaces the older");
  await tool("child", "rung");
  assert.deepEqual(await system("child"), [], "a native child session takes no rung");
  await emit("session.execution.succeeded", { sessionID: "root" });
  assert.deepEqual(await system("root"), [], "the turn end clears the rung");
  assert.ok(records.filter((r) => r.payload.hook_event_name === "tool_after").every((r) => r.stdio[1] === "pipe"));
  assert.ok(records.filter((r) => r.payload.hook_event_name !== "tool_after").every((r) => r.stdio[1] === "ignore"));
  await cleanup();
});

await test("a stalled feed holds the request only for the reply wait", async () => {
  const { cleanup, tool, system } = await activate(true);
  await tool("root", "stall");
  const started = Date.now();
  assert.deepEqual(await system("root"), []);
  const waited = Date.now() - started;
  assert.ok(waited >= 1000 && waited < 2400, "waited " + waited + " ms");
  await new Promise((resolve) => setTimeout(resolve, 1500));
  assert.deepEqual(await system("root"), ["late rung"], "a late rung reaches the next request");
  await cleanup();
});

await test("a rung that lands after its turn ended is dropped", async () => {
  const { cleanup, emit, tool, system } = await activate(true);
  await tool("root", "stall");
  await emit("session.execution.interrupted", { sessionID: "root" });
  await new Promise((resolve) => setTimeout(resolve, 2700));
  assert.deepEqual(await system("root"), []);
  await cleanup();
});

await test("an unsupervised session never reads the feed", async () => {
  const { cleanup, tool, system } = await activate(false);
  await tool("root", "rung");
  assert.deepEqual(await system("root"), []);
  assert.ok(records.length > 0 && records.every((r) => r.stdio[1] === "ignore"));
  await cleanup();
});

assert.deepEqual(failures, [], failures.join("\n"));
