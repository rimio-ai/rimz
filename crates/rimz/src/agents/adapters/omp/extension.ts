// _rimz_managed — OMP extension installed by RimZ.
// Reinstall with `rimz hooks install omp`; remove with `rimz hooks uninstall omp`.
// All hooks are passive: even tool_call returns immediately and never denies a tool.
import { spawn } from "node:child_process";

const RIMZ = process.env.RIMZ_BIN || "rimz";
const REMINDERS = Symbol.for("rimz.omp.launch-reminders");
const claimLaunchReminders = () => {
  if (!(REMINDERS in globalThis)) {
    globalThis[REMINDERS] = process.env.RIMZ_LAUNCH_REMINDERS || undefined;
    delete process.env.RIMZ_LAUNCH_REMINDERS;
  }
  return globalThis[REMINDERS];
};
const number = (value) => value == null || !Number.isFinite(Number(value)) ? undefined : Number(value);
const rounded = (value) => number(value) == null ? undefined : Math.round(Number(value));
const visibleText = (message) => {
  if (typeof message?.content === "string") return message.content.trim() || undefined;
  if (!Array.isArray(message?.content)) return undefined;
  return message.content.filter(b => b?.type === "text" && typeof b.text === "string").map(b => b.text.trim()).filter(Boolean).join("\n") || undefined;
};
const windowFromHeaders = (headers, prefix, minutes, now) => {
  const used = number(headers[`${prefix}-used-percent`]);
  const reset = number(headers[`${prefix}-reset-after-seconds`]);
  if (used == null && reset == null) return undefined;
  return {
    used_percentage: used == null ? undefined : Math.round(Math.max(0, Math.min(100, used))),
    duration_mins: rounded(headers[`${prefix}-window-minutes`]) ?? minutes,
    resets_at: reset == null ? undefined : now + Math.round(reset),
    observed_at: now,
  };
};

export default function rimz(pi) {
  const reminders = claimLaunchReminders();
  let activeId;
  let settled = false;
  let lastUsage;
  let fresh = false;
  let windows = [];
  let lastPush = 0;
  let lastGauge;
  let delivery = Promise.resolve();

  const hydrate = (ctx) => {
    const branch = ctx.sessionManager.getBranch();
    if (!Array.isArray(branch)) return;
    const assistant = branch.map(entry => entry?.message).findLast(message => message?.role === "assistant");
    lastUsage = assistant?.usage;
    fresh = !assistant;
  };
  const envelope = (event, ctx, fields = {}) => {
    const usage = ctx.getContextUsage();
    return {
      hook_event_name: event,
      session_id: ctx.sessionManager.getSessionId(),
      cwd: ctx.sessionManager.getCwd(),
      model: ctx.model?.id,
      effort: pi.getThinkingLevel(),
      context_pct: rounded(usage?.percent) ?? (fresh ? 0 : undefined),
      context_window: rounded(usage?.contextWindow ?? ctx.model?.contextWindow),
      total_tokens: rounded(usage?.tokens) ?? (fresh ? 0 : undefined),
      input_tokens: rounded(lastUsage?.input),
      output_tokens: rounded(lastUsage?.output),
      cache_read_input_tokens: rounded(lastUsage?.cacheRead),
      cache_write_input_tokens: rounded(lastUsage?.cacheWrite),
      rate_limits: windows.length ? windows : undefined,
      ...fields,
    };
  };
  const send = (body) => new Promise((finish) => {
    try {
      const child = spawn(RIMZ, ["hooks", "feed", "--source", "omp"], {
        env: { ...process.env, RIMZ_AGENT_PID: String(process.pid) },
        stdio: ["pipe", "pipe", "pipe"],
      });
      child.on("error", () => finish());
      child.on("close", () => finish());
      child.stdin.on("error", () => {});
      child.stdout.resume();
      child.stderr.resume();
      child.stdin.end(body);
      child.unref();
    } catch {
      finish();
    }
  });
  const feed = (event, ctx, fields = {}) => {
    try {
      const body = JSON.stringify(envelope(event, ctx, fields));
      // Preserve lifecycle order without making a tool handler await RimZ.
      delivery = delivery.then(() => send(body));
    } catch {
      // A missing or unavailable RimZ must never become an OMP tool denial.
    }
  };

  pi.on("session_start", (_ev, ctx) => {
    if (ctx.agent?.kind === "sub") return;
    activeId = ctx.sessionManager.getSessionId();
    settled = false;
    hydrate(ctx);
    feed("session_start", ctx);
  });
  pi.on("session_switch", (_ev, ctx) => {
    if (ctx.agent?.kind === "sub") return;
    const id = ctx.sessionManager.getSessionId();
    if (id === activeId) return;
    if (activeId) feed("session_shutdown", ctx, { session_id: activeId });
    activeId = id;
    settled = false;
    lastGauge = undefined;
    lastPush = 0;
    hydrate(ctx);
    // OMP restores these observations after session_switch; register identity only.
    feed("session_start", ctx, {
      model: undefined,
      effort: undefined,
      context_pct: undefined,
      context_window: undefined,
      total_tokens: undefined,
      input_tokens: undefined,
      output_tokens: undefined,
      cache_read_input_tokens: undefined,
      cache_write_input_tokens: undefined,
      rate_limits: undefined,
    });
  });
  pi.on("before_agent_start", (ev, ctx) => {
    if (ctx.agent?.kind === "sub") return;
    settled = false;
    feed("before_agent_start", ctx, { prompt: ev.prompt });
    if (reminders) return { systemPrompt: [...ev.systemPrompt, reminders] };
  });
  pi.on("agent_end", (ev, ctx) => {
    if (ctx.agent?.kind === "sub") return;
    const assistant = ev.messages?.findLast(message => message?.role === "assistant");
    if (assistant?.usage) lastUsage = assistant.usage;
    if (assistant) fresh = false;
    settled = ev.willContinue !== true;
    feed("agent_end", ctx, {
      stop_reason: assistant?.stopReason,
      error_message: assistant?.errorMessage,
      last_assistant_message: visibleText(assistant),
      will_continue: ev.willContinue,
    });
  });
  pi.on("turn_end", (ev, ctx) => {
    if (ctx.agent?.kind === "sub") return;
    if (ev.message?.usage) { lastUsage = ev.message.usage; fresh = false; }
    feed("turn_end", ctx);
  });
  pi.on("message_update", (ev, ctx) => {
    if (ctx.agent?.kind === "sub") return;
    if (ev.message?.usage) { lastUsage = ev.message.usage; fresh = false; }
    const snapshot = envelope("message_update", ctx);
    const gauge = JSON.stringify([snapshot.context_pct, snapshot.total_tokens, snapshot.input_tokens, snapshot.output_tokens, snapshot.cache_read_input_tokens, snapshot.cache_write_input_tokens]);
    const now = Date.now();
    if (gauge === lastGauge || now - lastPush < 1000) return;
    lastGauge = gauge;
    lastPush = now;
    feed("message_update", ctx);
  });
  pi.on("after_provider_response", (ev, ctx) => {
    if (ctx.agent?.kind === "sub") return;
    const now = Math.floor(Date.now() / 1000);
    const candidates = [
      ["x-codex-primary", 300], ["x-codex-secondary", 10080],
      ["anthropic-ratelimit-unified-primary", 300], ["anthropic-ratelimit-unified-secondary", 10080],
      ["anthropic-ratelimit-unified-5h", 300], ["anthropic-ratelimit-unified-7d", 10080],
      ["anthropic-ratelimit-unified-five-hour", 300], ["anthropic-ratelimit-unified-seven-day", 10080],
    ].map(([prefix, minutes]) => windowFromHeaders(ev.headers ?? {}, prefix, minutes, now)).filter(Boolean);
    if (candidates.length) windows = candidates;
    feed("after_provider_response", ctx);
  });
  pi.on("tool_call", (ev, ctx) => {
    if (ctx.agent?.kind === "sub") return;
    if (ev.toolName !== "ask" || ctx.hasUI !== true) return;
    feed("tool_call", ctx, { tool_name: ev.toolName, tool_call_id: ev.toolCallId, tool_input: ev.input, has_ui: ctx.hasUI === true });
  });
  pi.on("tool_execution_end", (ev, ctx) => {
    if (ctx.agent?.kind === "sub") return;
    if (settled && ev.toolName === "ask") return;
    feed("tool_execution_end", ctx, { tool_name: ev.toolName, tool_call_id: ev.toolCallId });
  });
  pi.on("tool_approval_requested", (ev, ctx) => {
    if (ctx.agent?.kind === "sub") return;
    if (ctx.hasUI !== true) return;
    feed("tool_approval_requested", ctx, { tool_name: ev.toolName, tool_call_id: ev.toolCallId, reason: ev.reason, has_ui: ctx.hasUI === true });
  });
  pi.on("tool_approval_resolved", (ev, ctx) => {
    if (ctx.agent?.kind === "sub") return;
    if (settled) return;
    feed("tool_approval_resolved", ctx, { tool_call_id: ev.toolCallId });
  });
  pi.on("session_before_compact", (_ev, ctx) => {
    if (ctx.agent?.kind === "sub") return;
    feed("session_before_compact", ctx);
  });
  pi.on("session_compact", (_ev, ctx) => {
    if (ctx.agent?.kind === "sub") return;
    hydrate(ctx);
    feed("session_compact", ctx);
  });
  pi.on("auto_compaction_end", (ev, ctx) => {
    if (ctx.agent?.kind === "sub") return;
    if (!ev.aborted && !ev.errorMessage && !ev.skipped) return;
    feed("auto_compaction_end", ctx, { aborted: ev.aborted, error_message: ev.errorMessage, skipped: ev.skipped });
  });
  pi.on("session_shutdown", (_ev, ctx) => {
    if (ctx.agent?.kind === "sub") return;
    feed("session_shutdown", ctx);
    return delivery;
  });
}
