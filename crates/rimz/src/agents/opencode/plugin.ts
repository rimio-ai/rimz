// _rimz_managed: OpenCode plugin owned by Rimz. Do not edit by hand.
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
      model: currentGauge?.model,
      provider_id: currentGauge?.providerID,
      effort: currentGauge?.effort,
      input_tokens: currentGauge?.input,
      output_tokens: currentGauge?.output,
      cache_read_input_tokens: currentGauge?.cacheRead,
      cache_write_input_tokens: currentGauge?.cacheWrite,
      total_tokens: currentGauge?.total,
      ...extra,
    };
  }

  function spawnRimz(payload: Envelope, collectStdout: boolean): Promise<string> | void {
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

    if (!collectStdout) {
      child.on("close", () => {});
      return;
    }

    return new Promise((resolve) => {
      let stdout = "";
      child.stdout?.on("data", (chunk) => {
        stdout += String(chunk);
      });
      child.on("error", () => resolve(""));
      child.on("close", () => resolve(stdout));
    });
  }

  function send(payload: Envelope): void {
    spawnRimz(payload, false);
  }

  async function ask(payload: Envelope): Promise<string> {
    return (await spawnRimz(payload, true)) || "";
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
    gauge.set(sessionID, {
      model: info?.modelID,
      providerID: info?.providerID,
      effort: info?.variant,
      input: numberOrUndefined(tokens.input),
      output: numberOrUndefined(tokens.output),
      cacheRead: numberOrUndefined(cache.read),
      cacheWrite: numberOrUndefined(cache.write),
      total: numberOrUndefined(tokens.total),
    });
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
        send(base("session_created", sessionID, { cwd: info.directory }));
        return;
      }

      if (type === "session.idle") {
        const sessionID = properties.sessionID;
        if (typeof sessionID !== "string") return;
        const parentID = children.get(sessionID);
        send(base(parentID ? "SubagentStop" : "session_idle", sessionID, {
          parent_session_id: parentID,
        }));
        return;
      }

      if (type === "session.error") {
        const sessionID = properties.sessionID;
        if (typeof sessionID !== "string") return;
        const error = properties.error || {};
        const parentID = children.get(sessionID);
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

      if (type === "message.updated") {
        updateGauge(properties.info);
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
      if (hookInput.model) {
        const prior = gauge.get(sessionID) || {};
        gauge.set(sessionID, {
          ...prior,
          model: hookInput.model.modelID,
          providerID: hookInput.model.providerID,
          effort: hookInput.variant,
        });
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

    dispose: async () => {},
  };
};

const module: PluginModule = {
  id: "rimz",
  server: RimzPlugin,
};

export default module;
