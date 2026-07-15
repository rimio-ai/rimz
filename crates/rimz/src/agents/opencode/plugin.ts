// _rimz_managed: OpenCode plugin owned by RimZ. Do not edit by hand.
import { spawn } from "node:child_process";
import type { Plugin, PluginModule } from "@opencode-ai/plugin";

type Gauge = {
  model?: string;
  providerID?: string;
  effort?: string;
  input?: number;
  output?: number;
  cacheRead?: number;
  cacheWrite?: number;
  total?: number;
  contextWindow?: number;
};

type Envelope = Record<string, unknown> & {
  hook_event_name: string;
  session_id?: string;
  cwd?: string;
};

const RIMZ = process.env.RIMZ_BIN || "rimz";
const RIMZ_ARGS = ["hooks", "feed", "--source", "opencode"];

export const RimzPlugin: Plugin = async (input) => {
  const children = new Map<string, string>();
  const gauge = new Map<string, Gauge>();
  const agents = new Map<string, string>();
  const roots = new Set<string>();

  function cwd(sessionDirectory?: unknown): string {
    if (typeof sessionDirectory === "string" && sessionDirectory.length > 0) {
      return sessionDirectory;
    }
    return input.worktree || input.directory;
  }

  function base(hookEventName: string, sessionID?: string, extra: Record<string, unknown> = {}): Envelope {
    const currentGauge = sessionID ? gauge.get(sessionID) : undefined;
    return {
      hook_event_name: hookEventName,
      session_id: sessionID,
      cwd: cwd(extra.cwd),
      server_url: input.serverUrl ? String(input.serverUrl) : undefined,
      model: currentGauge?.model,
      provider_id: currentGauge?.providerID,
      effort: currentGauge?.effort,
      input_tokens: currentGauge?.input,
      output_tokens: currentGauge?.output,
      cache_read_input_tokens: currentGauge?.cacheRead,
      cache_write_input_tokens: currentGauge?.cacheWrite,
      total_tokens: currentGauge?.total,
      context_window: currentGauge?.contextWindow,
      ...extra,
    };
  }

  function spawnRimz(payload: Envelope, collectStdout: boolean): Promise<string> {
    const child = spawn(RIMZ, RIMZ_ARGS, {
      cwd: payload.cwd,
      env: {
        ...process.env,
        RIMZ_AGENT_PID: String(process.pid),
      },
      stdio: ["pipe", collectStdout ? "pipe" : "ignore", "ignore"],
    });
    child.on("error", () => {});
    child.stdin.on("error", () => {});
    child.stdin.end(`${JSON.stringify(payload)}\n`);

    return new Promise((resolve) => {
      let stdout = "";
      if (collectStdout) {
        child.stdout?.on("data", (chunk) => {
          stdout += String(chunk);
        });
      }
      child.on("error", () => resolve(""));
      child.on("close", () => resolve(stdout));
    });
  }

  function send(payload: Envelope): void {
    void spawnRimz(payload, false);
  }

  async function ask(payload: Envelope): Promise<string> {
    return await spawnRimz(payload, true);
  }

  function endRoot(sessionID: string, reason: "deleted" | "dispose"): Promise<string> {
    const payload = base("session_ended", sessionID, { reason });
    roots.delete(sessionID);
    gauge.delete(sessionID);
    agents.delete(sessionID);
    return spawnRimz(payload, false);
  }

  // The model's context window is the gauge's divisor, and the gauge counts
  // input-side tokens only (input + cache, never output), so the divisor is the
  // model's max *input* tokens — the uniform context-window meaning across
  // agents. OpenCode carries no window on a message, so resolve it from the
  // server's own model catalog: prefer models.dev `Model.limit.input`, falling
  // back to the total `Model.limit.context` for a model that lists no separate
  // input cap. Key it `${providerID}/${modelID}` and by bare model id. The
  // catalog is static per server launch, so fetch it once; a failed or empty
  // read clears the memo so a later event retries, and until it resolves the
  // field is simply omitted (the Rust fallback covers Claude).
  let catalogPromise: Promise<Map<string, number>> | undefined;

  async function loadCatalog(): Promise<Map<string, number>> {
    const windows = new Map<string, number>();
    try {
      // The client may be built with throwOnError on (payload returned directly)
      // or off (wrapped in `{ data }`); accept either shape.
      const res: any = await input.client.config.providers();
      const providers = (res?.data ?? res)?.providers ?? [];
      for (const provider of providers) {
        const models = provider.models ?? {};
        for (const modelID of Object.keys(models)) {
          const limit = models[modelID]?.limit;
          const cap = limit?.input ?? limit?.context;
          if (typeof cap === "number" && Number.isFinite(cap) && cap > 0) {
            windows.set(`${provider.id}/${modelID}`, cap);
            windows.set(modelID, cap);
          }
        }
      }
    } catch {
      // best-effort: an unreachable catalog leaves the window to the Rust fallback
    }
    return windows;
  }

  function ensureCatalog(): Promise<Map<string, number>> {
    if (!catalogPromise) {
      catalogPromise = loadCatalog().then((windows) => {
        if (windows.size === 0) catalogPromise = undefined; // allow a later retry
        return windows;
      });
    }
    return catalogPromise;
  }

  function resolveWindow(sessionID: string, providerID?: string, modelID?: string): void {
    if (!modelID) return;
    void ensureCatalog().then((windows) => {
      const window =
        (providerID ? windows.get(`${providerID}/${modelID}`) : undefined) ?? windows.get(modelID);
      const prior = gauge.get(sessionID);
      if (typeof window !== "number" || !prior || prior.model !== modelID) return;
      gauge.set(sessionID, { ...prior, contextWindow: window });
    });
  }

  function updateGauge(info: any): void {
    const sessionID = info?.sessionID;
    if (typeof sessionID !== "string" || sessionID.length === 0) {
      return;
    }
    const tokens = info?.tokens;
    if (!tokens) {
      return;
    }
    const cache = tokens.cache || {};
    const prior = gauge.get(sessionID);
    const model = info?.modelID ?? prior?.model;
    const providerID = info?.providerID ?? prior?.providerID;
    gauge.set(sessionID, {
      model,
      providerID,
      effort: info?.variant ?? prior?.effort,
      input: numberOrUndefined(tokens.input),
      output: numberOrUndefined(tokens.output),
      cacheRead: numberOrUndefined(cache.read),
      cacheWrite: numberOrUndefined(cache.write),
      total: numberOrUndefined(tokens.total),
      // keep the resolved window across token updates; re-resolve on a model switch
      contextWindow: prior?.model === model ? prior?.contextWindow : undefined,
    });
    resolveWindow(sessionID, providerID, model);
  }

  function numberOrUndefined(value: unknown): number | undefined {
    return typeof value === "number" && Number.isFinite(value) ? value : undefined;
  }

  function userMessageText(message: any, parts: any[]): string | undefined {
    if (typeof message?.text === "string") {
      return message.text;
    }
    const content = message?.content;
    if (typeof content === "string") {
      return content;
    }
    if (Array.isArray(content)) {
      const joined = content
        .map((part) => (typeof part?.text === "string" ? part.text : ""))
        .filter(Boolean)
        .join("\n");
      if (joined.length > 0) return joined;
    }
    const joined = parts
      .map((part) => (typeof part?.text === "string" ? part.text : ""))
      .filter(Boolean)
      .join("\n");
    return joined.length > 0 ? joined : undefined;
  }

  return {
    event: async ({ event }) => {
      const type = (event as any)?.type;
      const properties = (event as any)?.properties || {};

      if (type === "session.created") {
        const info = properties.info || {};
        const sessionID = info.id;
        const parentID = info.parentID;
        if (typeof sessionID !== "string") return;
        if (typeof parentID === "string" && parentID.length > 0) {
          children.set(sessionID, parentID);
          send(base("SubagentStart", sessionID, {
            parent_session_id: parentID,
            cwd: info.directory,
            prompt: info.title,
          }));
          return;
        }
        roots.add(sessionID);
        send(base("session_created", sessionID, { cwd: info.directory }));
        return;
      }

      if (type === "session.idle") {
        const sessionID = properties.sessionID;
        if (typeof sessionID !== "string") return;
        const parentID = children.get(sessionID);
        if (!parentID) roots.add(sessionID);
        send(base(parentID ? "SubagentStop" : "session_idle", sessionID, {
          parent_session_id: parentID,
          plan_proposed: !parentID && agents.get(sessionID) === "plan" ? true : undefined,
        }));
        return;
      }

      if (type === "session.error") {
        const sessionID = properties.sessionID;
        if (typeof sessionID !== "string") return;
        const error = properties.error || {};
        const parentID = children.get(sessionID);
        if (!parentID) roots.add(sessionID);
        send(base(parentID ? "SubagentStop" : "session_error", sessionID, {
          parent_session_id: parentID,
          is_error: true,
          error_class: error.name || error.type,
          error_message: error.message,
        }));
        return;
      }

      if (type === "session.compacted") {
        const sessionID = properties.sessionID;
        if (typeof sessionID === "string") {
          send(base("session_compacted", sessionID));
        }
        return;
      }

      // OpenCode's current bus publishes the question tool's native blocking
      // prompt as `question.asked`. The legacy SDK Event union omits this
      // member, so keep the event boundary tolerant and read the runtime wire.
      if (type === "question.asked") {
        const sessionID = properties.sessionID;
        if (typeof sessionID !== "string") return;
        const questions = Array.isArray(properties.questions) ? properties.questions : [];
        const detail = questions
          .map((question) => (typeof question?.question === "string" ? question.question : ""))
          .filter(Boolean)
          .join("\n");
        send(base("question_ask", sessionID, {
          request_id: properties.id,
          title: detail || undefined,
          questions: questions.map((question) => ({
            question: typeof question?.question === "string" ? question.question : undefined,
            header: typeof question?.header === "string" ? question.header : undefined,
            options: Array.isArray(question?.options)
              ? question.options.map((option) => ({
                  label: typeof option?.label === "string" ? option.label : undefined,
                  description:
                    typeof option?.description === "string" ? option.description : undefined,
                }))
              : [],
            multiple: question?.multiple === true,
            custom: question?.custom === true,
          })),
        }));
        return;
      }

      // Current OpenCode publishes native permission prompts on the bus. The
      // legacy `permission.ask` plugin hook remains below for older releases,
      // but 1.17.18 no longer calls it from the permission service.
      if (type === "permission.asked") {
        const sessionID = properties.sessionID;
        if (typeof sessionID !== "string") return;
        const permission = typeof properties.permission === "string" ? properties.permission : undefined;
        const patterns = Array.isArray(properties.patterns)
          ? properties.patterns.filter((pattern) => typeof pattern === "string")
          : [];
        const title = [permission, patterns.join(", ")].filter(Boolean).join(": ");
        send(base("permission_ask", sessionID, {
          request_id: properties.id,
          tool_name: permission,
          permission_type: permission,
          title: title || undefined,
        }));
        return;
      }

      if (type === "permission.replied") {
        const sessionID = properties.sessionID;
        if (typeof sessionID !== "string" || children.has(sessionID)) return;
        send(base("permission_replied", sessionID, {
          request_id: properties.requestID,
          reply: properties.reply,
        }));
        return;
      }

      if (type === "question.replied") {
        const sessionID = properties.sessionID;
        if (typeof sessionID !== "string" || children.has(sessionID)) return;
        send(base("question_replied", sessionID, {
          request_id: properties.requestID,
          answers: properties.answers,
        }));
        return;
      }

      if (type === "question.rejected") {
        const sessionID = properties.sessionID;
        if (typeof sessionID !== "string" || children.has(sessionID)) return;
        send(base("question_rejected", sessionID, {
          request_id: properties.requestID,
        }));
        return;
      }

      if (type === "session.deleted") {
        const info = properties.info || {};
        const sessionID = info.id;
        if (typeof sessionID !== "string") return;
        if (children.has(sessionID) || (typeof info.parentID === "string" && info.parentID.length > 0)) {
          children.delete(sessionID);
          gauge.delete(sessionID);
          agents.delete(sessionID);
          return;
        }
        void endRoot(sessionID, "deleted");
        return;
      }

      if (type === "message.updated") {
        const info = properties.info;
        const sessionID = info?.sessionID;
        const agent = info?.mode ?? info?.agent;
        if (typeof sessionID === "string" && sessionID.length > 0 && typeof agent === "string") {
          agents.set(sessionID, agent);
        }
        updateGauge(info);
        return;
      }

      if (type === "message.part.updated") {
        const part = properties.part || {};
        if (part.type === "step-finish") {
          updateGauge(part);
        }
      }
    },

    "chat.message": async (hookInput, output) => {
      const sessionID = hookInput.sessionID;
      if (!children.has(sessionID)) roots.add(sessionID);
      if (typeof hookInput.agent === "string") agents.set(sessionID, hookInput.agent);
      if (hookInput.model) {
        const prior = gauge.get(sessionID) || {};
        const modelID = hookInput.model.modelID;
        const providerID = hookInput.model.providerID;
        gauge.set(sessionID, {
          ...prior,
          model: modelID,
          providerID,
          effort: hookInput.variant,
          contextWindow: prior.model === modelID ? prior.contextWindow : undefined,
        });
        resolveWindow(sessionID, providerID, modelID);
      }
      send(base("chat_message", sessionID, {
        prompt: userMessageText(output.message, output.parts),
        model: hookInput.model?.modelID,
        provider_id: hookInput.model?.providerID,
        effort: hookInput.variant,
      }));
    },

    "tool.execute.after": async (hookInput, output) => {
      if (children.has(hookInput.sessionID)) {
        return;
      }
      send(base("tool_after", hookInput.sessionID, {
        tool_name: hookInput.tool,
        is_error: Boolean(output.metadata?.error || output.metadata?.isError),
      }));
    },

    "experimental.session.compacting": async (hookInput) => {
      send(base("session_compacting", hookInput.sessionID));
    },

    "permission.ask": async (permission, output) => {
      const stdout = await ask(base("permission_ask", permission.sessionID, {
        request_id: (permission as any).id,
        tool_name: permission.type,
        permission_type: permission.type,
        title: permission.title,
      }));
      try {
        const parsed = JSON.parse(stdout);
        // Decision shape: {"status":"deny"} or {"status":"allow"}.
        if (parsed?.status === "deny") {
          output.status = "deny";
        } else if (parsed?.status === "allow") {
          output.status = "allow";
        }
      } catch {
        output.status = "ask";
      }
    },

    dispose: async () => {
      const pending = [...roots].map((sessionID) => endRoot(sessionID, "dispose"));
      if (pending.length === 0) return;
      let timer: ReturnType<typeof setTimeout> | undefined;
      await Promise.race([
        Promise.allSettled(pending),
        new Promise<void>((resolve) => {
          timer = setTimeout(resolve, 1500);
        }),
      ]);
      if (timer) clearTimeout(timer);
    },
  };
};

const module: PluginModule = {
  id: "rimz",
  server: RimzPlugin,
};

export default module;
