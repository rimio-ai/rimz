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
// feeds is docs/internals/hooks.md → Appendix Pi and the upstream surface is
// docs/internals/adapter/pi-reference.md.
import { spawn } from "node:child_process";

const RIMZ = process.env.RIMZ_BIN || "rimz";

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
    return {
      hook_event_name: event,
      session_id: ctx?.sessionManager?.getSessionId?.(),
      cwd: ctx?.sessionManager?.getCwd?.() ?? ctx?.cwd,
      model: ctx?.model?.id,
      effort: thinkingLevel(),
      context_pct: usage?.percent == null ? undefined : Math.round(usage.percent),
      context_window: usage?.contextWindow,
      total_tokens: usage?.tokens == null ? undefined : Math.round(usage.tokens),
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
    const fields = { stop_reason: last?.stopReason, error_message: last?.errorMessage };
    // Only override the envelope's model/tokens when the message carries
    // them — an explicit undefined would drop the envelope value from the
    // JSON.
    if (last?.model) fields.model = last.model;
    if (last?.usage?.totalTokens != null) fields.total_tokens = Math.round(last.usage.totalTokens);
    feed("agent_end", ctx, fields);
  });
  pi.on("tool_execution_end", (ev, ctx) =>
    feed("tool_execution_end", ctx, {
      tool_name: ev?.toolName,
      is_error: ev?.isError === true,
    }),
  );
  pi.on("session_before_compact", (_ev, ctx) => feed("session_before_compact", ctx, {}));
  pi.on("session_shutdown", (ev, ctx) => feed("session_shutdown", ctx, { reason: ev?.reason }));

  // The blocking pre-tool gate. Pi awaits this handler, so the bridge wait
  // happens here: spawn rimz, await its exit, read the decision from stdout.
  // With no fresh enrolled resolver rimz answers immediately with no stdout
  // (= allow), so the un-enrolled path adds one short-lived process and no
  // human-visible latency. Every non-deny outcome — empty stdout, a parse
  // failure, a spawn error, a missing binary — resolves to "let the tool
  // run": pi has no native permission prompt, so blocking is opt-in via a
  // resolver and never a failure mode.
  pi.on("tool_call", (ev, ctx) =>
    new Promise((resolve) => {
      const allow = () => resolve(undefined);
      try {
        const child = spawnRimz("pipe");
        let out = "";
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
