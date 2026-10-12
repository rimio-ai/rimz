// _rimz_managed: OpenCode plugin owned by RimZ. Do not edit by hand.
import { spawn } from "node:child_process";
import { accessSync, constants, statSync } from "node:fs";
import { delimiter, resolve } from "node:path";
import type { Context } from "@opencode/plugin/promise/plugin";

type ModelRef = { id: string; providerID: string; variant?: string };
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
  modelDisplayName?: string;
};
type SessionInfo = { title?: string; version?: string; directory?: string };
type Envelope = Record<string, unknown> & { hook_event_name: string; session_id: string; cwd: string };
type Cleanup = () => Promise<void>;

const activations = new WeakMap<Context, Promise<Cleanup>>();
const LAUNCH_REMINDERS = Symbol.for("rimz.launch-reminders");
const DEFAULT_SESSION_TITLE = /^(?:New session|Child session) - /;

function claimLaunchReminders(): string | undefined {
  const claimed = globalThis as Record<symbol, string | undefined>;
  if (!(LAUNCH_REMINDERS in claimed)) {
    claimed[LAUNCH_REMINDERS] = process.env.RIMZ_LAUNCH_REMINDERS || undefined;
    delete process.env.RIMZ_LAUNCH_REMINDERS;
  }
  return claimed[LAUNCH_REMINDERS];
}

function executable(): string | undefined {
  const candidates = process.env.RIMZ_BIN
    ? [process.env.RIMZ_BIN]
    : (process.env.PATH || "").split(delimiter).map((dir) => resolve(dir, "rimz"));
  return candidates.find((path) => {
    try {
      accessSync(path, constants.X_OK);
      return statSync(path).isFile();
    } catch {
      return false;
    }
  });
}

function setup(ctx: Context): Promise<Cleanup> | undefined {
  if (!process.env.RIMZ_WORKSPACE_ID) return;
  const rimz = executable();
  if (!rimz) return;
  const existing = activations.get(ctx);
  if (existing) return existing;
  const activation = activate(ctx, resolve(rimz)).catch((error) => {
    activations.delete(ctx);
    throw error;
  });
  activations.set(ctx, activation);
  return activation;
}

async function activate(ctx: Context, rimz: string): Promise<Cleanup> {
  const reminders = claimLaunchReminders();
  const controller = new AbortController();
  const children = new Map<string, string>();
  const childModels = new Map<string, string>();
  const roots = new Set<string>();
  const sessions = new Map<string, SessionInfo>();
  const agents = new Map<string, string>();
  const gauges = new Map<string, Gauge>();
  const steps = new Map<string, { sessionID: string; model: ModelRef }>();
  const forms = new Map<string, string[]>();
  const pending = new Set<Promise<void>>();
  const registrations: Array<{ dispose(): Promise<void> }> = [];
  let disposed = false;
  let catalog: Promise<Map<string, { contextWindow?: number; modelDisplayName?: string }>> | undefined;

  function base(name: string, sessionID: string, extra: Record<string, unknown> = {}): Envelope {
    const gauge = gauges.get(sessionID);
    const info = sessions.get(sessionID);
    const directory = info?.directory || ctx.location.directory;
    return {
      hook_event_name: name, session_id: sessionID, cwd: directory, worktree_path: directory,
      session_name: info?.title, agent_version: info?.version,
      model: gauge?.model, provider_id: gauge?.providerID, effort: gauge?.effort,
      model_display_name: gauge?.modelDisplayName, context_window: gauge?.contextWindow,
      input_tokens: gauge?.input, output_tokens: gauge?.output,
      cache_read_input_tokens: gauge?.cacheRead, cache_write_input_tokens: gauge?.cacheWrite,
      total_tokens: gauge?.total, ...extra,
    };
  }

  function send(payload: Envelope): Promise<void> {
    const done = new Promise<void>((finish) => {
      const child = spawn(rimz, ["hooks", "feed", "--source", "opencode"], {
        cwd: payload.cwd,
        env: { ...process.env, RIMZ_AGENT_PID: String(process.ppid) },
        stdio: ["pipe", "ignore", "ignore"],
      });
      child.on("error", () => finish());
      child.on("close", () => finish());
      child.stdin.on("error", () => {});
      child.stdin.end(JSON.stringify(payload) + "\n");
    });
    pending.add(done);
    void done.then(() => pending.delete(done));
    return done;
  }

  function rememberTitle(sessionID: string, title: unknown): boolean {
    if (typeof title !== "string" || DEFAULT_SESSION_TITLE.test(title)) return false;
    const prior = sessions.get(sessionID) || {};
    if (prior.title === title) return false;
    sessions.set(sessionID, { ...prior, title });
    return true;
  }

  async function rememberModel(sessionID: string, model: ModelRef): Promise<void> {
    const prior = gauges.get(sessionID) || {};
    const same = prior.model === model.id && prior.providerID === model.providerID;
    gauges.set(sessionID, {
      ...prior, model: model.id, providerID: model.providerID, effort: model.variant,
      contextWindow: same ? prior.contextWindow : undefined,
      modelDisplayName: same ? prior.modelDisplayName : undefined,
    });
    if (!catalog) {
      catalog = ctx.model.list().then((result) => {
        const entries = new Map<string, { contextWindow?: number; modelDisplayName?: string }>();
        for (const item of result.data) {
          const contextWindow = [item.limit.input, item.limit.context].find((limit) => limit !== undefined && limit > 0);
          const entry = { contextWindow, modelDisplayName: item.name };
          entries.set(item.providerID + "/" + item.id, entry);
          entries.set(item.id, entry);
        }
        if (!entries.size) catalog = undefined;
        return entries;
      }).catch(() => {
        catalog = undefined;
        return new Map();
      });
    }
    const entries = await catalog;
    const entry = entries?.get(model.providerID + "/" + model.id) ?? entries?.get(model.id);
    const current = gauges.get(sessionID);
    if (!disposed && current?.model === model.id && current.providerID === model.providerID && entry) {
      gauges.set(sessionID, { ...current, ...entry });
    }
  }

  // Metadata is enrichment: a stalled lookup must not hold a terminal event.
  function refreshSession(sessionID: string): void {
    void ctx.session.get({ sessionID }).then(async (info) => {
      if (disposed || !roots.has(sessionID)) return;
      const changed = rememberTitle(sessionID, info.title);
      if (info.agent) agents.set(sessionID, info.agent);
      if (info.model) await rememberModel(sessionID, info.model);
      if (changed && !disposed && roots.has(sessionID)) void send(base("session_updated", sessionID));
    }).catch(() => {});
  }

  function announceChildModel(sessionID: string): void {
    const parent = children.get(sessionID);
    const model = gauges.get(sessionID)?.model;
    if (!parent || !model || childModels.get(sessionID) === model) return;
    childModels.set(sessionID, model);
    void send(base("SubagentStart", sessionID, { parent_session_id: parent }));
  }

  function forget(sessionID: string): void {
    roots.delete(sessionID);
    children.delete(sessionID);
    childModels.delete(sessionID);
    sessions.delete(sessionID);
    agents.delete(sessionID);
    gauges.delete(sessionID);
    for (const [id, step] of steps) if (step.sessionID === sessionID) steps.delete(id);
  }

  async function classifySession(sessionID: string): Promise<void> {
    if (disposed || roots.has(sessionID) || children.has(sessionID)) return;
    try {
      const info = await ctx.session.get({ sessionID });
      if (disposed) return;
      if (info.parentID) children.set(sessionID, info.parentID);
      else roots.add(sessionID);
    } catch {
      // Do not attribute an unknown child to the root when lookup fails.
    }
  }

  function endRoot(sessionID: string, reason: string): Promise<void> {
    const payload = base("session_ended", sessionID, { reason });
    forget(sessionID);
    return send(payload);
  }

  async function onEvent(event: any): Promise<void> {
    if (event.location?.directory && event.location.directory !== ctx.location.directory) return;
    const data = event.data;
    if (event.type === "form.created") {
      const form = data.form;
      if (form.sessionID === "global") return;
      await classifySession(form.sessionID);
      if (disposed || !roots.has(form.sessionID)) return;
      forms.set(form.id, form.fields.map((field: any) => field.key));
      void send(base("question_ask", form.sessionID, {
        request_id: form.id, title: form.title,
        questions: form.fields.map((field: any) => ({
          question: field.description || field.title || field.key,
          header: field.title || undefined, options: field.options || [],
          multiple: field.type === "multiselect", custom: field.custom === true,
        })),
      }));
      return;
    }
    const sessionID = data?.sessionID;
    if (typeof sessionID !== "string") return;
    if (event.type === "session.created") {
      sessions.set(sessionID, { version: data.version, directory: data.location.directory });
      rememberTitle(sessionID, data.title);
      if (data.agent) agents.set(sessionID, data.agent);
      if (data.parentID) children.set(sessionID, data.parentID);
      else roots.add(sessionID);
      if (data.model) await rememberModel(sessionID, data.model);
      if (disposed) return;
      if (data.parentID) {
        if (data.model) childModels.set(sessionID, data.model.id);
        void send(base("SubagentStart", sessionID, {
          parent_session_id: data.parentID,
          prompt: typeof data.title === "string" ? data.title.replace(/[\u0000-\u001f\u007f]/g, " ").trim() : undefined,
        }));
      } else {
        void send(base("session_created", sessionID));
        refreshSession(sessionID);
      }
      return;
    }
    if (sessionID === "global") return;
    await classifySession(sessionID);
    if (disposed || (!roots.has(sessionID) && !children.has(sessionID))) return;
    if (event.type === "session.step.started") {
      steps.set(data.assistantMessageID, { sessionID, model: data.model });
      agents.set(sessionID, data.agent);
      return;
    }
    if (event.type === "session.step.ended" || event.type === "session.step.failed") {
      const step = steps.get(data.assistantMessageID);
      steps.delete(data.assistantMessageID);
      const tokens = data.tokens || {};
      const input = tokens.input || 0, output = (tokens.output || 0) + (tokens.reasoning || 0);
      const cacheRead = tokens.cache?.read || 0, cacheWrite = tokens.cache?.write || 0;
      const total = input + output + cacheRead + cacheWrite;
      if (total > 0) gauges.set(sessionID, { ...gauges.get(sessionID), input, output, cacheRead, cacheWrite, total });
      if (step) await rememberModel(sessionID, step.model);
      if (disposed) return;
      announceChildModel(sessionID);
      return;
    }
    if (event.type === "session.model.selected") {
      await rememberModel(sessionID, data.model);
      if (disposed) return;
      announceChildModel(sessionID);
      return;
    }
    if (event.type === "session.agent.selected") {
      agents.set(sessionID, data.agent);
      return;
    }
    if (event.type === "session.deleted") {
      if (children.has(sessionID)) forget(sessionID);
      else void endRoot(sessionID, "deleted");
      return;
    }
    if (event.type === "session.execution.started") {
      if (!children.has(sessionID)) {
        roots.add(sessionID);
        refreshSession(sessionID);
      }
      return;
    }
    if (["session.execution.succeeded", "session.execution.interrupted", "session.execution.failed"].includes(event.type)) {
      const parent = children.get(sessionID);
      const failed = event.type === "session.execution.failed";
      if (!parent) roots.add(sessionID);
      void send(base(parent ? "SubagentStop" : failed ? "session_error" : "session_idle", sessionID, {
        parent_session_id: parent, is_error: failed,
        error_class: data.error?.type, error_message: data.error?.message,
        plan_proposed: !parent && !failed && agents.get(sessionID) === "plan" ? true : undefined,
      }));
      if (!parent) refreshSession(sessionID);
      return;
    }
    if (children.has(sessionID) || sessionID === "global") return;
    switch (event.type) {
      case "session.renamed":
        if (rememberTitle(sessionID, data.title)) void send(base("session_updated", sessionID));
        break;
      case "session.compaction.started":
        void send(base("session_compacting", sessionID));
        break;
      case "session.compaction.ended":
      case "session.compaction.failed":
        void send(base("session_compacted", sessionID, { is_error: event.type === "session.compaction.failed" }));
        break;
      case "permission.asked":
        void send(base("permission_ask", sessionID, {
          request_id: data.id, tool_name: data.action, patterns: data.resources,
          title: data.message || data.action,
        }));
        break;
      case "permission.replied":
        void send(base("permission_replied", sessionID, { request_id: data.requestID, reply: data.reply }));
        break;
      case "form.replied": {
        const answers = (forms.get(data.id) || []).map((key) => {
          const value = data.answer[key];
          return value === undefined ? [] : (Array.isArray(value) ? value : [value]).map(String);
        });
        forms.delete(data.id);
        void send(base("question_replied", sessionID, { request_id: data.id, answers }));
        break;
      }
      case "form.cancelled":
        forms.delete(data.id);
        void send(base("question_rejected", sessionID, { request_id: data.id }));
        break;
    }
  }

  registrations.push(await ctx.session.hook("prompt", async (input) => {
    await classifySession(input.sessionID);
    if (disposed || !roots.has(input.sessionID)) return;
    void send(base("chat_message", input.sessionID, { prompt: input.prompt.text }));
    refreshSession(input.sessionID);
  }));
  registrations.push(await ctx.session.hook("context", async (input) => {
    await classifySession(input.sessionID);
    if (disposed) return;
    if (reminders && roots.has(input.sessionID)) {
      input.system.push({ type: "text", text: reminders });
    }
  }));
  registrations.push(await ctx.tool.hook("execute.after", async (input) => {
    await classifySession(input.sessionID);
    if (disposed) return;
    if (roots.has(input.sessionID)) {
      void send(base("tool_after", input.sessionID, { tool_name: input.tool, is_error: input.status === "error" }));
    }
  }));
  void (async () => {
    try {
      for await (const event of ctx.event.subscribe({ signal: controller.signal })) {
        if (disposed) break;
        try {
          await onEvent(event);
        } catch {
          // A failed enrichment or handler must not end event delivery.
        }
      }
    } catch {
      // The subscription ends when OpenCode tears down this activation.
    }
  })();

  return async () => {
    if (disposed) return;
    disposed = true;
    activations.delete(ctx);
    controller.abort();
    for (const sessionID of [...roots]) void endRoot(sessionID, "dispose");
    let timer: ReturnType<typeof setTimeout> | undefined;
    await Promise.race([
      Promise.allSettled([...pending, ...registrations.map((registration) => registration.dispose())]),
      new Promise<void>((finish) => { timer = setTimeout(finish, 1500); }),
    ]);
    if (timer) clearTimeout(timer);
  };
}

export default { id: "rimz", setup };
