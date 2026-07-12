// _rimz_managed: Amp plugin owned by Rimz. Do not edit by hand.
import { spawn } from "node:child_process";
import type {
  PluginAPI,
  PluginThread,
  ThreadMessage,
  ThreadState,
} from "@ampcode/plugin";

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
const SEND_TIMEOUT_MS = 10_000;

export default function (amp: PluginAPI) {
  const gauges = new Map<string, ThreadGauge>();
  const states = new Map<string, ThreadState>();
  const subscribedThreads = new Set<string>();
  const registeredThreads = new Set<string>();
  let sendQueue = Promise.resolve();

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

  function sendNow(payload: Envelope): Promise<void> {
    return new Promise((resolve) => {
      const child = spawn(RIMZ, RIMZ_ARGS, {
        cwd: payload.cwd,
        env: {
          ...process.env,
          RIMZ_AGENT_PID: String(process.pid),
        },
        stdio: ["pipe", "ignore", "ignore"],
      });
      let settled = false;
      const finish = () => {
        if (settled) return;
        settled = true;
        clearTimeout(timeout);
        resolve();
      };
      const timeout = setTimeout(() => {
        child.kill();
        finish();
      }, SEND_TIMEOUT_MS);
      timeout.unref();
      child.once("error", finish);
      child.once("close", finish);
      child.stdin.on("error", () => {});
      child.stdin.end(`${JSON.stringify(payload)}\n`);
    });
  }

  function send(payload: Envelope): void {
    // Separate hook helpers can otherwise race registration behind the first
    // turn event. Keep Amp non-blocking while preserving its event order.
    sendQueue = sendQueue.then(
      () => sendNow(payload),
      () => sendNow(payload),
    );
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

  function bindThread(thread: PluginThread): void {
    if (subscribedThreads.has(thread.id)) return;
    subscribedThreads.add(thread.id);
    thread.state.subscribe((state) => observeState(thread, state));
    void thread.state
      .get()
      .then((state) => {
        if (!states.has(thread.id)) observeState(thread, state);
      })
      .catch(() => {
        // State is enrichment; lifecycle events remain authoritative.
      });
  }

  amp.on("session.start", (event, ctx) => {
    const threadID = event.thread.id;
    // Registration is the ordering boundary. Agent metadata and state reads
    // are enrichment and must not delay it or let a focus switch drop it.
    registeredThreads.add(threadID);
    forward("session_start", threadID);
    bindThread(ctx.thread);
    void ctx.thread
      .agent()
      .then(({ definition }) => {
        gauges.set(threadID, {
          model:
            definition.kind === "builtin-agent"
              ? definition.mode
              : definition.model,
          effort: definition.reasoningEffort,
        });
      })
      .catch(() => {
        gauges.delete(threadID);
      });
  });

  amp.on("agent.start", (event) => {
    forward("agent_start", event.thread.id, { prompt: event.message });
  });

  amp.on("tool.result", (event) => {
    let filesModified: boolean | undefined;
    try {
      const files = amp.helpers.filesModifiedByToolCall(event);
      filesModified = files == null ? undefined : files.length > 0;
    } catch {
      // Omit the hint so the adapter's static vocabulary remains the fallback.
    }
    forward("tool_result", event.thread.id, {
      tool_name: event.tool,
      status: event.status,
      files_modified: filesModified,
    });
  });

  amp.on("agent.end", (event) => {
    forward("agent_end", event.thread.id, {
      status: event.status,
      last_assistant_message: lastAssistantMessage(event.messages),
    });
  });
}

function lastAssistantMessage(messages: ThreadMessage[]): string | undefined {
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (message.role !== "assistant") continue;
    const text = message.content
      .filter((block) => block.type === "text")
      .map((block) => block.text)
      .join("\n")
      .trim();
    if (text) return text;
  }
  return undefined;
}
