#!/usr/bin/env node

const { spawnSync } = require("node:child_process");
const crypto = require("node:crypto");

if (process.argv.includes("--version")) {
  process.stdout.write("scriptbot 1.0.0\n");
  process.exit(0);
}

const resumeAt = process.argv.indexOf("--resume");
const sessionId = resumeAt >= 0 ? process.argv[resumeAt + 1] : crypto.randomUUID();
const prompt = process.argv.at(-1)?.startsWith("--") ? "demo turn" : process.argv.at(-1) || "demo turn";

function emit(hook_event_name, fields = {}) {
  const payload = JSON.stringify({
    protocol: 1,
    hook_event_name,
    session_id: sessionId,
    cwd: process.cwd(),
    model: "scriptbot-demo",
    context_window: 100000,
    context_pct: 12,
    total_tokens: 12000,
    ...fields,
  });
  spawnSync("rimz", ["hooks", "feed", "--source", "scriptbot", "--event", hook_event_name], {
    input: payload,
    stdio: ["pipe", "inherit", "inherit"],
  });
}

emit("session_start");
emit("turn_start", { prompt });
emit("context", { total_cost_usd: 0.02 });
emit("tool_use", { tool_name: "write", is_error: false });
emit("awaiting_input", { ask: "question", question: "Continue the scripted demo?" });
emit("turn_end", { errored: false, last_assistant_message: "Scripted demo complete." });
