// Rimz lifecycle bridge for pi — _rimz_managed, written by `rimz hooks install`.
// Re-install with `rimz hooks install`; remove the file (or run
// `rimz hooks uninstall`) to unwire. Edits are overwritten on re-install.
//
// Forwards pi's lifecycle events to `rimz hooks feed --source pi` as one JSON
// payload on the child's stdin. Fire-and-forget with fresh, fully-piped stdio:
// pi never blocks on Rimz, and the child's output never reaches pi's UI. The
// event mapping this feeds is docs/internals/hooks.md → Appendix Pi; the
// upstream surface is docs/internals/adapter/pi-reference.md.
import { spawn } from "node:child_process";

export default function rimz(pi) {
  const feed = (event, ctx, fields) => {
    try {
      const payload = {
        hook_event_name: event,
        session_id: ctx?.sessionManager?.getSessionId?.(),
        cwd: ctx?.sessionManager?.getCwd?.() ?? ctx?.cwd,
        ...fields,
      };
      const child = spawn("rimz", ["hooks", "feed", "--source", "pi"], {
        env: { ...process.env, RIMZ_AGENT_PID: String(process.pid) },
        stdio: ["pipe", "ignore", "ignore"],
      });
      child.on("error", () => {});
      child.stdin.end(JSON.stringify(payload));
    } catch {
      // Enrichment, never correctness: a missing rimz binary must not break pi.
    }
  };

  pi.on("session_start", (ev, ctx) => feed("session_start", ctx, { reason: ev?.reason }));
  pi.on("before_agent_start", (ev, ctx) => feed("before_agent_start", ctx, { prompt: ev?.prompt }));
  pi.on("agent_end", (ev, ctx) => {
    // The prompt's last assistant message carries the turn verdict and usage.
    const messages = Array.isArray(ev?.messages) ? ev.messages : [];
    const last = messages.filter((m) => m?.role === "assistant").at(-1);
    feed("agent_end", ctx, {
      stop_reason: last?.stopReason,
      error_message: last?.errorMessage,
      model: last?.model,
      total_tokens: last?.usage?.totalTokens,
    });
  });
  pi.on("tool_execution_end", (ev, ctx) =>
    feed("tool_execution_end", ctx, {
      tool_name: ev?.toolName,
      is_error: ev?.isError === true,
    }),
  );
  pi.on("session_before_compact", (_ev, ctx) => feed("session_before_compact", ctx, {}));
  pi.on("session_shutdown", (ev, ctx) => feed("session_shutdown", ctx, { reason: ev?.reason }));
}
