// _rimz_managed: Amp plugin owned by Rimz. Do not edit by hand.
import { spawn } from "node:child_process";
import type { PluginAPI, PluginThread, ThreadState } from "@ampcode/plugin";

type ThreadGauge = {
  model?: string;
  effort?: string;
};

type Envelope = Record<string, unknown> & {
  hook_event_name: string;
  session_id: string;
  cwd: string;
};

const RIMZ = process.env.RIMZ_BIN || "rimz";
const RIMZ_ARGS = ["hooks", "feed", "--source", "amp"];

export default function (amp: PluginAPI) {
  const gauges = new Map<string, ThreadGauge>();
  const states = new Map<string, ThreadState>();
  const subscribedThreads = new Set<string>();
  const registeredThreads = new Set<string>();

  function cwd(): string {
    const root = amp.system.workspaceRoot;
    return root ? amp.helpers.filePathFromURI(root) : process.cwd();
  }

  function isActive(threadID: string): boolean {
    const active = amp.activeThread.current;
    return active == null || active.id === threadID;
  }

  function envelope(
    hookEventName: string,
    threadID: string,
    extra: Record<string, unknown> = {},
  ): Envelope {
    return {
      hook_event_name: hookEventName,
      session_id: threadID,
      cwd: cwd(),
      ...gauges.get(threadID),
      ...extra,
    };
  }

  function send(payload: Envelope): void {
    const child = spawn(RIMZ, RIMZ_ARGS, {
      cwd: payload.cwd,
      env: {
        ...process.env,
        RIMZ_AGENT_PID: String(process.pid),
      },
      stdio: ["pipe", "ignore", "ignore"],
    });
    child.on("error", () => {});
    child.on("close", () => {});
    child.stdin.on("error", () => {});
    child.stdin.end(`${JSON.stringify(payload)}\n`);
  }

  function forward(
    hookEventName: string,
    threadID: string,
    extra: Record<string, unknown> = {},
  ): void {
    if (isActive(threadID)) send(envelope(hookEventName, threadID, extra));
  }

  function observeState(thread: PluginThread, state: ThreadState): void {
    const prior = states.get(thread.id);
    states.set(thread.id, state);
    if (
      state === "awaiting-approval" &&
      prior !== "awaiting-approval" &&
      registeredThreads.has(thread.id)
    ) {
      forward("permission_ask", thread.id);
    }
  }

  async function bindThread(thread: PluginThread): Promise<void> {
    if (subscribedThreads.has(thread.id)) return;
    subscribedThreads.add(thread.id);
    thread.state.subscribe((state) => observeState(thread, state));
    try {
      const state = await thread.state.get();
      if (!states.has(thread.id)) observeState(thread, state);
    } catch {
      // State is enrichment; lifecycle events remain authoritative.
    }
  }

  amp.on("session.start", async (event, ctx) => {
    const threadID = event.thread.id;
    try {
      const definition = (await ctx.thread.agent()).definition;
      gauges.set(threadID, {
        model: definition.kind === "builtin-agent" ? definition.mode : definition.model,
        effort: definition.reasoningEffort,
      });
    } catch {
      gauges.delete(threadID);
    }
    await bindThread(ctx.thread);
    forward("session_start", threadID);
    registeredThreads.add(threadID);
    if (states.get(threadID) === "awaiting-approval") {
      forward("permission_ask", threadID);
    }
  });

  amp.on("agent.start", (event) => {
    forward("agent_start", event.thread.id, { prompt: event.message });
  });

  amp.on("tool.result", (event) => {
    const files = amp.helpers.filesModifiedByToolCall(event);
    forward("tool_result", event.thread.id, {
      tool_name: event.tool,
      status: event.status,
      files_modified: files != null && files.length > 0,
    });
  });

  amp.on("agent.end", (event) => {
    forward("agent_end", event.thread.id, { status: event.status });
  });
}
