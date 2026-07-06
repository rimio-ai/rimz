// Rimz bridge for pi — _rimz_managed, written by `rimz hooks install pi`.
// Re-install with `rimz hooks install pi`; remove the file (or run
// `rimz hooks uninstall pi`) to unwire. Edits are overwritten on re-install.
//
// Forwards pi's lifecycle events to `rimz hooks feed --source pi` as one JSON
// payload on the child's stdin — fire-and-forget with fresh, fully-piped
// stdio, so pi never blocks on Rimz and the child's output never reaches pi's
// UI. The one exception is `tool_call`, pi's blocking pre-tool gate: its
// handler awaits the child and reads the decision from stdout — `{"block":
// true, "reason": …}` blocks the tool, anything else (including an absent or
// broken rimz) lets it run. Rimz authors this wire; the event mapping it
// feeds is docs/internals/agents/adapter/pi.md and the upstream surface is
// docs/internals/adapter/pi-reference.md.
import { spawn } from "node:child_process";

const RIMZ = process.env.RIMZ_BIN || "rimz";
const usageBySession = new Map();
const costBySession = new Map();
let latestWindows = [];

const nowSec = () => Math.floor(Date.now() / 1000);
const roundMaybe = (value) =>
  value == null || !Number.isFinite(Number(value)) ? undefined : Math.round(Number(value));
const numberMaybe = (value) =>
  value == null || !Number.isFinite(Number(value)) ? undefined : Number(value);

const sessionId = (ctx) => ctx?.sessionManager?.getSessionId?.();

const recordUsage = (id, usage) => {
  if (!id || usage == null) return;
  const gauge = {
    input: roundMaybe(usage.input),
    output: roundMaybe(usage.output),
    cacheRead: roundMaybe(usage.cacheRead),
    cacheWrite: roundMaybe(usage.cacheWrite),
  };
  if (Object.values(gauge).some((value) => value != null)) {
    usageBySession.set(id, gauge);
  }
};

const addSessionCost = (id, usage) => {
  const cost = numberMaybe(usage?.cost?.total);
  if (id && cost != null && cost > 0) {
    costBySession.set(id, (costBySession.get(id) ?? 0) + cost);
  }
};

const usageFields = (id) => {
  const gauge = usageBySession.get(id);
  if (!gauge) return {};
  return {
    input_tokens: gauge.input,
    output_tokens: gauge.output,
    cache_read_input_tokens: gauge.cacheRead,
    cache_write_input_tokens: gauge.cacheWrite,
  };
};

const headerPairs = (headers) => {
  if (!headers) return [];
  const pairs = [];
  if (typeof headers.forEach === "function") {
    headers.forEach((value, key) => pairs.push([key, value]));
    return pairs;
  }
  if (Array.isArray(headers)) return headers;
  return Object.entries(headers);
};

const headerMap = (headers) => {
  const map = new Map();
  for (const [key, value] of headerPairs(headers)) {
    if (key != null && value != null) map.set(String(key).toLowerCase(), String(value));
  }
  return map;
};

const headerNumber = (headers, name) => numberMaybe(headers.get(name));

const windowFromHeaders = (headers, prefix, defaultMins, capturedAt) => {
  const used = headerNumber(headers, `${prefix}-used-percent`);
  const mins = headerNumber(headers, `${prefix}-window-minutes`) ?? defaultMins;
  const resetAfter = headerNumber(headers, `${prefix}-reset-after-seconds`);
  if (used == null && resetAfter == null) return undefined;
  return {
    used_percentage: used == null ? undefined : Math.round(Math.max(0, Math.min(100, used))),
    duration_mins: mins == null ? undefined : Math.round(mins),
    resets_at: resetAfter == null ? undefined : capturedAt + Math.round(resetAfter),
    observed_at: capturedAt,
  };
};

const updateWindows = (headers) => {
  const map = headerMap(headers);
  const capturedAt = nowSec();
  const candidates = [
    windowFromHeaders(map, "x-codex-primary", 300, capturedAt),
    windowFromHeaders(map, "x-codex-secondary", 10080, capturedAt),
    windowFromHeaders(map, "anthropic-ratelimit-unified-primary", 300, capturedAt),
    windowFromHeaders(map, "anthropic-ratelimit-unified-secondary", 10080, capturedAt),
    windowFromHeaders(map, "anthropic-ratelimit-unified-5h", 300, capturedAt),
    windowFromHeaders(map, "anthropic-ratelimit-unified-7d", 10080, capturedAt),
    windowFromHeaders(map, "anthropic-ratelimit-unified-five-hour", 300, capturedAt),
    windowFromHeaders(map, "anthropic-ratelimit-unified-seven-day", 10080, capturedAt),
  ].filter(Boolean);
  if (candidates.length > 0) latestWindows = candidates;
};

export default function rimz(pi) {
  const thinkingLevel = () => {
    try {
      return pi.getThinkingLevel?.();
    } catch {
      return undefined; // throwing stub before the runner binds — omit.
    }
  };

  // The common payload envelope. Every field is best-effort: a missing value
  // is omitted (JSON.stringify drops undefined) and the Rust adapter treats
  // absence as "the agent didn't report it". The context gauge rides every
  // event so the sidebar's bar stays current without a transcript read; the
  // counts are rounded because the adapter parses them as integers.
  const envelope = (event, ctx, fields) => {
    const usage = ctx?.getContextUsage?.();
    const id = sessionId(ctx);
    return {
      hook_event_name: event,
      session_id: id,
      cwd: ctx?.sessionManager?.getCwd?.() ?? ctx?.cwd,
      model: ctx?.model?.id,
      effort: thinkingLevel(),
      context_pct: usage?.percent == null ? undefined : Math.round(usage.percent),
      context_window: usage?.contextWindow,
      total_tokens: usage?.tokens == null ? undefined : Math.round(usage.tokens),
      total_cost_usd: costBySession.get(id),
      ...usageFields(id),
      rate_limits: latestWindows.length > 0 ? latestWindows : undefined,
      ...fields,
    };
  };

  const spawnRimz = (stdout) => {
    const child = spawn(RIMZ, ["hooks", "feed", "--source", "pi"], {
      env: { ...process.env, RIMZ_AGENT_PID: String(process.pid) },
      stdio: ["pipe", stdout, "ignore"],
    });
    // Both swallowed: a missing rimz binary or a child that exits before the
    // payload lands (EPIPE on stdin) must never surface inside pi.
    child.on("error", () => {});
    child.stdin.on("error", () => {});
    return child;
  };

  const feed = (event, ctx, fields) => {
    try {
      const child = spawnRimz("ignore");
      child.stdin.end(JSON.stringify(envelope(event, ctx, fields)));
    } catch {
      // Enrichment, never correctness: a missing rimz binary must not break pi.
    }
  };

  pi.on("session_start", (ev, ctx) => feed("session_start", ctx, { reason: ev?.reason }));
  pi.on("before_agent_start", (ev, ctx) =>
    feed("before_agent_start", ctx, { prompt: ev?.prompt }),
  );
  pi.on("agent_end", (ev, ctx) => {
    // The prompt's last assistant message carries the turn verdict and usage.
    const messages = Array.isArray(ev?.messages) ? ev.messages : [];
    const last = messages.filter((m) => m?.role === "assistant").at(-1);
    recordUsage(sessionId(ctx), last?.usage);
    const fields = { stop_reason: last?.stopReason, error_message: last?.errorMessage };
    // Only override the envelope's model/tokens when the message carries
    // them — an explicit undefined would drop the envelope value from the
    // JSON.
    if (last?.model) fields.model = last.model;
    if (last?.usage?.totalTokens != null) fields.total_tokens = Math.round(last.usage.totalTokens);
    feed("agent_end", ctx, fields);
  });
  pi.on("turn_end", (ev, ctx) => {
    const messages = Array.isArray(ev?.messages) ? ev.messages : [];
    const last = messages.filter((m) => m?.role === "assistant").at(-1);
    const usage = ev?.usage ?? last?.usage ?? ev?.message?.usage;
    const id = sessionId(ctx);
    recordUsage(id, usage);
    addSessionCost(id, usage);
  });
  pi.on("after_provider_response", (ev) => updateWindows(ev?.headers));
  pi.on("tool_execution_end", (ev, ctx) =>
    feed("tool_execution_end", ctx, {
      tool_name: ev?.toolName,
      is_error: ev?.isError === true,
    }),
  );
  pi.on("model_select", (ev, ctx) => feed("model_select", ctx, { model: ev?.model?.id }));
  pi.on("thinking_level_select", (ev, ctx) =>
    feed("thinking_level_select", ctx, { effort: ev?.level }),
  );
  pi.on("session_before_compact", (_ev, ctx) => feed("session_before_compact", ctx, {}));
  pi.on("session_compact", (_ev, ctx) => feed("session_compact", ctx, {}));
  pi.on("session_shutdown", (ev, ctx) => {
    // A /reload tears down and re-registers the SAME session id; both
    // children are fire-and-forget, so a tombstone racing the re-register
    // could drop the fresh row. Skip the tombstone — the reloaded
    // extension's session_start re-registers in place. quit/new/resume/fork
    // genuinely end this session.
    if (ev?.reason === "reload") return;
    const id = sessionId(ctx);
    usageBySession.delete(id);
    costBySession.delete(id);
    latestWindows = [];
    feed("session_shutdown", ctx, { reason: ev?.reason });
  });

  // The blocking pre-tool gate. Pi awaits this handler, so rimz returns the
  // neutral no-op immediately. Pi has no native ask UI, so no feed item is
  // created and every non-deny outcome — empty stdout, a parse
  // failure, a spawn error, a missing binary — resolves to "let the tool
  // run": pi has no native permission prompt, so blocking is opt-in via a
  // resolver and never a failure mode.
  pi.on("tool_call", (ev, ctx) =>
    new Promise((resolve) => {
      const allow = () => resolve(undefined);
      try {
        const child = spawnRimz("pipe");
        let out = "";
        // Decode as a stream, not per-chunk: a multi-byte character in the
        // deny reason must never split across chunk boundaries.
        child.stdout.setEncoding("utf8");
        child.stdout.on("data", (chunk) => {
          out += chunk;
        });
        child.on("error", allow);
        child.on("close", () => {
          try {
            const decision = JSON.parse(out);
            if (decision?.block === true) {
              resolve({
                block: true,
                reason: typeof decision.reason === "string" ? decision.reason : undefined,
              });
              return;
            }
          } catch {
            // Empty or non-JSON stdout is the neutral allow.
          }
          allow();
        });
        child.stdin.end(
          JSON.stringify(
            envelope("tool_call", ctx, { tool_name: ev?.toolName, tool_input: ev?.input }),
          ),
        );
      } catch {
        allow();
      }
    }),
  );
}
