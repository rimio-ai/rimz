//! Node-driven coverage for the embedded Claude Code mod's hooks module. Claude
//! loads it in-process and hands each hook a `$` host API; this suite drives the
//! module's `register` through Node with a fake `$` whose `process.run` spawns a
//! stubbed `rimz` the way Claude's does (argv, no shell, resolving any exit code
//! and rejecting when the command cannot start).

#[test]
#[cfg(unix)]
fn each_hook_reports_after_next_and_never_changes_the_result() {
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
    let module_path = dir.path().join("rimz.ts");
    std::fs::write(
        &module_path,
        include_str!("../../../src/agents/adapters/claude/rimz_mod/rimz.ts"),
    )
    .unwrap();
    let capture = dir.path().join("capture.jsonl");
    for (name, body) in [
        (
            "rimz-capture",
            "#!/bin/sh\npayload=$(cat)\nprintf '%s\\t%s\\n' \"$*\" \"$payload\" >> \"$RIMZ_CAPTURE\"\n",
        ),
        (
            "rimz-failing",
            "#!/bin/sh\ncat >/dev/null\necho 'unknown command mod' >&2\nexit 2\n",
        ),
    ] {
        let path = dir.path().join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let harness = dir.path().join("harness.mjs");
    std::fs::write(&harness, HARNESS).unwrap();
    let output = std::process::Command::new("node")
        .arg("--experimental-strip-types")
        .arg(&harness)
        .arg(&module_path)
        .arg(dir.path())
        .env("RIMZ_CAPTURE", &capture)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "node harness failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

const HARNESS: &str = r#"
import fs from "node:fs";
import path from "node:path";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { pathToFileURL } from "node:url";

const [modulePath, dir] = process.argv.slice(2);
const capture = process.env.RIMZ_CAPTURE;
const { register } = await import(pathToFileURL(modulePath).href);

const hooks = new Map();
register((event, hook) => hooks.set(event, hook));
assert.deepEqual(
  [...hooks.keys()],
  ["session.start", "agent.spawn", "tool.call", "turn.complete"],
);
const sessionStart = hooks.get("session.start");

const USAGE = {
  startedAt: 1794967641885,
  context: { tokens: 38328, window: 1000000, percent: 4 },
  rateLimits: [{ kind: "seven_day", percentUsed: 45, resetsAt: "2026-11-23T06:00:00.000Z" }],
  cost: { usd: 0.00459113 },
};

const makeHost = ({
  bin,
  agentList = () => [{ id: "a1", name: "explore" }],
  usage = async () => USAGE,
}) => {
  const runs = [];
  // Every host call the mod makes, in order: a hook that has not yet resolved
  // `next` must have made none.
  const calls = [];
  const logged = (name, fn) => (...args) => {
    calls.push(name);
    return fn(...args);
  };
  const $ = {
    session: {
      id: logged("session.id", async () => "sess-1"),
      info: logged("session.info", async () => ({ transcriptPath: "/t/sess-1.jsonl" })),
      version: logged("session.version", async () => ({ version: "2.1.326" })),
      usage: logged("session.usage", usage),
    },
    agent: { list: logged("agent.list", agentList) },
    env: { get: logged("env.get", async (key) => (key === "RIMZ_BIN" ? bin : undefined)) },
    process: {
      run: logged("process.run", (argv, { stdin, timeoutMs }) => {
        runs.push({ argv, timeoutMs });
        return new Promise((resolve, reject) => {
          const child = spawn(argv[0], argv.slice(1), { stdio: ["pipe", "pipe", "pipe"] });
          let stdout = "";
          let stderr = "";
          child.stdout.on("data", (chunk) => (stdout += chunk));
          child.stderr.on("data", (chunk) => (stderr += chunk));
          child.on("error", reject);
          child.on("close", (exitCode) => resolve({ exitCode, stdout, stderr }));
          child.stdin.on("error", () => {});
          child.stdin.end(stdin);
        });
      }),
    },
  };
  return { $, runs, calls };
};

const order = [];
const result = { decision: "untouched" };
const next = async (e) => {
  order.push("next");
  assert.deepEqual(e, { source: "startup" });
  return result;
};
const captured = () =>
  fs.existsSync(capture)
    ? fs.readFileSync(capture, "utf8").trim().split("\n").filter(Boolean)
    : [];

// A reachable rimz receives one envelope on stdin after `next` resolved.
{
  const host = makeHost({ bin: path.join(dir, "rimz-capture") });
  const returned = await sessionStart(host.$, { source: "startup" }, async (e) => {
    assert.deepEqual(host.calls, [], "no host call before next resolves");
    const value = await next(e);
    assert.deepEqual(host.calls, [], "no host call before next resolves");
    return value;
  });
  assert.equal(returned, result, "the hook returns next's own result");
  const [line, ...rest] = captured();
  assert.equal(rest.length, 0);
  const [args, payload] = line.split("\t");
  assert.equal(args, "mod feed --source claude");
  assert.deepEqual(JSON.parse(payload), {
    v: "1.1.0",
    event: "session.start",
    session_id: "sess-1",
    transcript_path: "/t/sess-1.jsonl",
    claude_version: "2.1.326",
    agents: [{ id: "a1", name: "explore" }],
    usage: USAGE,
  });
  assert.equal(host.runs[0].timeoutMs, 2000);
  fs.rmSync(capture);
}

// A failing rimz, a missing one, a throwing agent list and a throwing usage read
// never reach the session.
for (const [label, options] of [
  ["failing stub", { bin: path.join(dir, "rimz-failing") }],
  ["missing binary", { bin: path.join(dir, "no-such-rimz") }],
  [
    "agent.list throws",
    {
      bin: path.join(dir, "rimz-capture"),
      agentList: () => {
        throw new Error("session ended");
      },
    },
  ],
  [
    "session.usage throws",
    {
      bin: path.join(dir, "rimz-capture"),
      usage: () => {
        throw new Error("usage unavailable");
      },
    },
  ],
]) {
  const host = makeHost(options);
  const returned = await sessionStart(host.$, { source: "startup" }, next);
  assert.equal(returned, result, label);
  assert.equal(host.runs.length, 1, label);
}
const [listless, usageless] = captured().map((line) => JSON.parse(line.split("\t")[1]));
assert.equal(listless.agents, undefined);
assert.deepEqual(listless.usage, USAGE, "a throwing agent list keeps the readings");
assert.equal(usageless.usage, undefined, "a throwing usage read still checks in");
assert.deepEqual(usageless.agents, [{ id: "a1", name: "explore" }]);
fs.rmSync(capture);

// Every other report point: what it sends, after next, with next's result kept.
// Main-loop events report without an id, a child's with its own; every report
// carries the readings, and a turn's end its reason.
for (const [event, input, resolved, agentId, reason] of [
  ["agent.spawn", { subagentType: "Explore" }, { model: "claude-haiku-5-5", agentId: "a1" }, "a1"],
  ["tool.call", { tool: "Bash", agentId: "a1" }, { text: "ok" }, "a1"],
  ["tool.call", { tool: "Bash" }, { text: "[Request interrupted by user for tool use]" }, undefined],
  ["turn.complete", { reason: "answer", agentId: "a1" }, { text: "done" }, "a1", "answer"],
  ["turn.complete", { reason: "aborted" }, { text: "" }, undefined, "aborted"],
  ["turn.complete", { reason: 7 }, { text: "" }, undefined],
]) {
  const label = `${event} ${JSON.stringify(input)}`;
  const hook = hooks.get(event);
  const host = makeHost({ bin: path.join(dir, "rimz-capture") });
  const returned = await hook(host.$, input, async (e) => {
    assert.equal(e, input, label);
    assert.deepEqual(host.calls, [], `${label}: no host call before next resolves`);
    await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(host.calls, [], `${label}: no host call before next resolves`);
    return resolved;
  });
  assert.equal(returned, resolved, `${label}: the hook returns next's own result`);
  const lines = captured();
  assert.equal(lines.length, 1, label);
  const envelope = JSON.parse(lines[0].split("\t")[1]);
  assert.equal(envelope.event, event, label);
  assert.equal(envelope.agent_id, agentId, label);
  assert.equal("agent_id" in envelope, agentId !== undefined, `${label}: absent, not null`);
  assert.deepEqual(envelope.agents, [{ id: "a1", name: "explore" }], label);
  assert.deepEqual(envelope.usage, USAGE, label);
  assert.equal(envelope.reason, reason, label);
  assert.equal("reason" in envelope, reason !== undefined, `${label}: absent, not null`);
  fs.rmSync(capture);

  for (const bin of ["rimz-failing", "no-such-rimz"]) {
    const failing = makeHost({ bin: path.join(dir, bin) });
    const kept = await hook(failing.$, input, async () => resolved);
    assert.equal(kept, resolved, `${label} with ${bin}`);
    assert.equal(failing.runs.length, 1, `${label} with ${bin}`);
  }
}

// An unset RIMZ_BIN falls back to `rimz` on PATH.
{
  const host = makeHost({ bin: undefined });
  host.$.process.run = async (argv) => {
    host.runs.push({ argv });
    return { exitCode: 0, stdout: "", stderr: "" };
  };
  await sessionStart(host.$, { source: "startup" }, next);
  assert.deepEqual(host.runs[0].argv, ["rimz", "mod", "feed", "--source", "claude"]);
}

// A throwing next propagates unchanged, and nothing is reported.
{
  const host = makeHost({ bin: path.join(dir, "rimz-capture") });
  const failure = new Error("next failed");
  await assert.rejects(
    sessionStart(host.$, { source: "startup" }, async () => {
      throw failure;
    }),
    (error) => error === failure,
  );
  assert.equal(host.runs.length, 0);
  assert.equal(captured().length, 0);
}
"#;
