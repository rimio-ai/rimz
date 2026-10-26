// _rimz_managed: OpenCode client plugin owned by RimZ. Do not edit by hand.
import { spawn } from "node:child_process";
import { accessSync, constants, statSync } from "node:fs";
import { delimiter, resolve } from "node:path";
import type { Context } from "@opencode/plugin/tui/context";

type ModelRef = { id: string; providerID: string; variant?: string };
type Gauge = {
  model?: string; providerID?: string; effort?: string;
  input?: number; output?: number; cacheRead?: number; cacheWrite?: number; total?: number;
  contextWindow?: number; modelDisplayName?: string;
};
type Ask = { kind: "permission" | "form"; request: any; sessionID: string };
type Session = {
  info: any; gauge: Gauge; usageRevision: number;
  steps: Map<string, ModelRef>; tools: Map<string, string>;
};
type Envelope = Record<string, unknown> & { hook_event_name: string; session_id: string; cwd: string };
type Cleanup = () => Promise<void>;
const activations = new WeakMap<Context, Cleanup>();
const DEFAULT_SESSION_TITLE = /^(?:New session|Child session) - /;

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

async function bounded<T>(work: Promise<T>): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      work,
      new Promise<never>((_, reject) => { timer = setTimeout(() => reject(Error("read timed out")), 1000); }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

function setup(ctx: Context): Cleanup | undefined {
  if (!process.env.RIMZ_WORKSPACE_ID || process.argv.includes("--standalone")) return;
  const rimz = executable();
  if (!rimz) return;
  const existing = activations.get(ctx);
  if (existing) return existing;
  const cleanup = activate(ctx, resolve(rimz));
  activations.set(ctx, cleanup);
  return cleanup;
}

function activate(ctx: Context, rimz: string): Cleanup {
  // Session maps live only while selected, in a tab, or holding a forwarded ask.
  const sessions = new Map<string, Session>();
  const forwarded = new Map<string, Ask>();
  let disposed = false, unsubscribe: (() => void) | undefined;
  let selected: string | undefined, sampled: string | undefined, epoch = 0, arrived = false, reading: number | undefined;
  let feeds = Promise.resolve(), events = Promise.resolve();

  function current(): string | undefined {
    const route = ctx.ui.router.current();
    return route.type === "session" ? route.sessionID : undefined;
  }

  function tabbed(sessionID: string): boolean {
    return ctx.ui.tabs.list().some((tab) => tab.sessionID === sessionID);
  }

  function session(sessionID: string): Session {
    let state = sessions.get(sessionID);
    if (!state) {
      state = { info: ctx.data.session.get(sessionID) || {}, gauge: {}, usageRevision: 0, steps: new Map(), tools: new Map() };
      sessions.set(sessionID, state);
    }
    return state;
  }

  function base(name: string, sessionID: string, extra: Record<string, unknown> = {}): Envelope {
    const { info, gauge } = session(sessionID);
    const directory = info.location?.directory || ctx.location?.directory || process.cwd();
    return {
      hook_event_name: name, session_id: sessionID, cwd: directory, worktree_path: directory,
      session_name: typeof info.title === "string" && !DEFAULT_SESSION_TITLE.test(info.title) ? info.title : undefined,
      agent_version: info.version, model: gauge.model, provider_id: gauge.providerID, effort: gauge.effort,
      model_display_name: gauge.modelDisplayName, context_window: gauge.contextWindow,
      input_tokens: gauge.input, output_tokens: gauge.output,
      cache_read_input_tokens: gauge.cacheRead, cache_write_input_tokens: gauge.cacheWrite,
      total_tokens: gauge.total, ...extra,
    };
  }

  function send(payload: Envelope, onScreen = current() === payload.session_id): void {
    const stampEpoch = epoch;
    feeds = feeds.then(async () => {
      const focused = onScreen && (payload.hook_event_name === "session_ended" || current() === payload.session_id && stampEpoch === epoch);
      if (onScreen && payload.hook_event_name.startsWith("client_") && !focused) return;
      if (focused && payload.hook_event_name !== "session_ended") selected = payload.session_id;
      const env = { ...process.env };
      for (const key of Object.keys(env)) if (key.startsWith("RIMZ_AGENT_")) env[key] = "";
      for (const key of ["RIMZ_HOOK_OWNER_KIND", "RIMZ_RUN_ID", "RIMZ_WORKFLOW_RUN", "RIMZ_TEAM",
        "RIMZ_LAUNCH_GROUP", "RIMZ_LAUNCH_ORDINAL", "RIMZ_RUNTIME_ENV", "RIMZ_LOOP_TASK"]) env[key] = "";
      await new Promise<void>((finish) => {
        const child = spawn(rimz, ["hooks", "feed", "--source", "opencode"], {
          cwd: payload.cwd,
          env: { ...env, RIMZ_AGENT_PID: String(process.pid) },
          stdio: ["pipe", "ignore", "ignore"],
        });
        child.on("error", () => finish());
        child.on("close", () => finish());
        child.stdin.on("error", () => {});
        child.stdin.end(JSON.stringify({ ...payload, on_screen: focused ? true : undefined }) + "\n");
      });
    }).catch(() => {});
  }

  function tokenTotal(tokens: any): number {
    return (tokens?.input || 0) + (tokens?.output || 0) + (tokens?.reasoning || 0) +
      (tokens?.cache?.read || 0) + (tokens?.cache?.write || 0);
  }

  function rememberUsage(state: Session, tokens: any): void {
    const input = tokens?.input || 0, output = (tokens?.output || 0) + (tokens?.reasoning || 0);
    const cacheRead = tokens?.cache?.read || 0, cacheWrite = tokens?.cache?.write || 0;
    const total = input + output + cacheRead + cacheWrite;
    if (total <= 0) return;
    state.gauge = { ...state.gauge, input, output, cacheRead, cacheWrite, total };
    state.usageRevision++;
  }

  async function rememberModel(state: Session, model: ModelRef): Promise<void> {
    const prior = state.gauge;
    const same = prior.model === model.id && prior.providerID === model.providerID;
    state.gauge = {
      ...prior, model: model.id, providerID: model.providerID, effort: model.variant,
      contextWindow: same ? prior.contextWindow : undefined,
      modelDisplayName: same ? prior.modelDisplayName : undefined,
    };
    const location = state.info.location;
    await ctx.data.location.model.sync(location);
    const entry = ctx.data.location.model.list(location)?.find((item) => item.providerID === model.providerID && item.id === model.id);
    if (disposed || !entry || state.gauge.model !== model.id || state.gauge.providerID !== model.providerID) return;
    state.gauge = { ...state.gauge,
      contextWindow: [entry.limit.input, entry.limit.context].find((limit) => limit !== undefined && limit > 0),
      modelDisplayName: entry.name,
    };
  }

  async function enrich(sessionID: string, state: Session): Promise<void> {
    const revision = state.usageRevision;
    await ctx.data.session.message.sync(sessionID);
    let message = [...ctx.data.session.message.list(sessionID)].reverse().find((item) =>
      item.type === "assistant" && tokenTotal(item.tokens) > 0);
    if (!message) {
      let cursor: string | undefined;
      do {
        if (disposed || sessions.get(sessionID) !== state) return;
        const page = await ctx.client.message.list({ sessionID, limit: 200, type: "assistant", ...(cursor ? { cursor } : { order: "desc" as const }) });
        message = page.data.find((item) => item.type === "assistant" && tokenTotal(item.tokens) > 0);
        if (message || disposed || !page.data.length || !page.cursor.next || cursor === page.cursor.next) break;
        cursor = page.cursor.next;
      } while (sessions.get(sessionID) === state);
    }
    if (disposed || sessions.get(sessionID) !== state || state.usageRevision !== revision) return;
    if (message?.type === "assistant") {
      rememberUsage(state, message.tokens);
      await rememberModel(state, message.model);
    } else if (state.info.model) {
      await rememberModel(state, state.info.model);
    }
  }

  function pendingAsks(sessionID: string, exclude?: string): Ask[] {
    const state = session(sessionID);
    const requests: Ask[] = [
      ...(ctx.data.session.permission.list(sessionID) || []).map((request) => ({ kind: "permission" as const, request, sessionID })),
      ...(ctx.data.session.form.list(sessionID, state.info.location) || []).map((request) => ({ kind: "form" as const, request, sessionID })),
    ];
    return requests.filter((ask) => ask.request.id !== exclude);
  }

  function sendAsk(ask: Ask, repeat = false, onScreen = current() === ask.sessionID): void {
    const { sessionID, request, kind } = ask;
    if (!repeat && forwarded.has(request.id)) return;
    forwarded.set(request.id, ask);
    if (kind === "permission") {
      send(base("permission_ask", sessionID, {
        request_id: request.id, tool_name: request.action, patterns: request.resources,
        title: request.message || request.action,
      }), onScreen);
    } else {
      send(base("question_ask", sessionID, {
        request_id: request.id, title: request.title,
        questions: request.fields.map((field: any) => ({
          question: field.description || field.title || field.key, header: field.title || undefined,
          options: field.options || [], multiple: field.type === "multiselect", custom: field.custom === true,
        })),
      }), onScreen);
    }
  }

  async function sync(sessionID: string): Promise<Session> {
    await ctx.data.session.sync(sessionID);
    const state = session(sessionID);
    const info = ctx.data.session.get(sessionID);
    if (!info) throw Error("session unavailable");
    state.info = info;
    await Promise.all([
      ctx.data.session.permission.sync(sessionID),
      ctx.data.session.form.sync(sessionID, state.info.location),
    ]);
    return state;
  }

  function reconcileAsks(sessionID: string): { pending: Ask[]; cleared: boolean } {
    const pending = pendingAsks(sessionID);
    let cleared = false;
    for (const [id, ask] of forwarded) {
      if (ask.sessionID !== sessionID || pending.some((item) => item.request.id === id)) continue;
      forwarded.delete(id);
      cleared = true;
    }
    return { pending, cleared };
  }

  async function arrive(sessionID: string, arrivalEpoch: number): Promise<void> {
    try {
      const state = await bounded(sync(sessionID));
      try { await bounded(enrich(sessionID, state)); } catch { /* Enrichment rides the next envelope. */ }
      const active = await bounded(ctx.client.session.active());
      if (disposed || current() !== sessionID || epoch !== arrivalEpoch || !ctx.data.session.get(sessionID)) return;
      const ask = reconcileAsks(sessionID).pending[0];
      if (ask) sendAsk(ask);
      else send(base(active[sessionID]?.type === "running" ? "client_running" : "client_idle", sessionID));
      arrived = true;
    } catch {
      // Failed state reads retry on the next sampler pass without reporting idle.
    } finally {
      if (reading === arrivalEpoch) reading = undefined;
    }
  }

  async function sweep(): Promise<void> {
    const sessionIDs = new Set([...ctx.ui.tabs.list().map((tab) => tab.sessionID), ...[...forwarded.values()].map((ask) => ask.sessionID)]);
    for (const sessionID of sessionIDs) {
      if (sessionID === current()) continue;
      try {
        await bounded(sync(sessionID));
        if (disposed) return;
        if (sessionID === current() || !ctx.data.session.get(sessionID)) continue;
        const { pending, cleared } = reconcileAsks(sessionID);
        const ask = pending.find((item) => !forwarded.has(item.request.id));
        if (ask) sendAsk(ask, false, false);
        else if (cleared && !pending.length) {
          send(base(ctx.data.session.status(sessionID) === "running" ? "client_running" : "client_idle", sessionID), false);
        }
      } catch { /* A failed reconciliation keeps the forwarded ask until the next reconnect. */ }
    }
  }

  async function reconnect(): Promise<void> {
    await sweep();
    if (disposed) return;
    epoch++;
    arrived = false;
    sample();
  }

  async function onEvent(event: any): Promise<void> {
    if (disposed) return;
    if (event.type === "server.connected") { await reconnect(); return; }
    const data = event.data;
    const requestID = data?.requestID || data?.id;
    const sessionID = data?.form?.sessionID || data?.sessionID || forwarded.get(requestID)?.sessionID;
    if (!sessionID || sessionID === "global") return;
    if (event.type === "session.deleted") {
      const onScreen = sessionID === selected;
      if (onScreen || [...forwarded.values()].some((ask) => ask.sessionID === sessionID)) {
        send(base("session_ended", sessionID, { reason: "deleted" }), onScreen);
      }
      if (onScreen) selected = undefined;
      sessions.delete(sessionID);
      for (const [id, ask] of forwarded) if (ask.sessionID === sessionID) forwarded.delete(id);
      return;
    }
    const focused = sessionID === current();
    const replyEvent = ["permission.replied", "form.replied", "form.cancelled"].includes(event.type);
    const askEvent = replyEvent || event.type === "permission.asked" || event.type === "form.created";
    if (!focused && (!askEvent || !tabbed(sessionID) && !forwarded.has(requestID))) return;
    if (!ctx.data.session.get(sessionID) && !(replyEvent && forwarded.has(requestID))) {
      await bounded(ctx.data.session.sync(sessionID));
      if (disposed || !ctx.data.session.get(sessionID)) return;
      if (sessionID !== current() && (!askEvent || !tabbed(sessionID) && !forwarded.has(requestID))) return;
    }
    const state = session(sessionID);
    if (event.type === "permission.asked" || event.type === "form.created") {
      sendAsk({ kind: event.type === "permission.asked" ? "permission" : "form", request: data.form || data, sessionID });
      return;
    }
    if (event.type === "permission.replied" || event.type === "form.replied" || event.type === "form.cancelled") {
      const ask = forwarded.get(requestID);
      if (event.type === "permission.replied") send(base("permission_replied", sessionID, { request_id: requestID, reply: data.reply }));
      else if (event.type === "form.cancelled") send(base("question_rejected", sessionID, { request_id: requestID }));
      else {
        const answers = (ask?.request.fields || []).map((field: any) => {
          const value = data.answer[field.key];
          return value === undefined ? [] : (Array.isArray(value) ? value : [value]).map(String);
        });
        send(base("question_replied", sessionID, { request_id: requestID, answers }));
      }
      forwarded.delete(requestID);
      const next = pendingAsks(sessionID, requestID)[0];
      if (next) sendAsk(next, true);
      return;
    }
    switch (event.type) {
      case "session.inbox.enqueued":
        if (data.item.type === "user") send(base("chat_message", sessionID, { prompt: data.item.payload.text }));
        break;
      case "session.execution.started":
        send(base("client_running", sessionID));
        break;
      case "session.step.started":
        state.steps.set(data.assistantMessageID, data.model);
        state.info = { ...state.info, agent: data.agent };
        void rememberModel(state, data.model).catch(() => {});
        break;
      case "session.step.ended":
      case "session.step.failed": {
        rememberUsage(state, data.tokens);
        const model = state.steps.get(data.assistantMessageID);
        state.steps.delete(data.assistantMessageID);
        for (const id of state.tools.keys()) if (id.startsWith(data.assistantMessageID + "/")) state.tools.delete(id);
        if (model) void rememberModel(state, model).catch(() => {});
        break;
      }
      case "session.model.selected":
        void rememberModel(state, data.model).catch(() => {});
        break;
      case "session.agent.selected":
        state.info = { ...state.info, agent: data.agent };
        break;
      case "session.tool.input.started":
        state.tools.set(data.assistantMessageID + "/" + data.id, data.name);
        break;
      case "session.tool.success":
      case "session.tool.failed": {
        const key = data.assistantMessageID + "/" + data.id;
        const message = ctx.data.session.message.list(sessionID).find((item) => item.id === data.assistantMessageID);
        const cached = message?.type === "assistant" ? message.content.find((part) => part.type === "tool" && part.id === data.id) : undefined;
        send(base("tool_after", sessionID, { tool_name: state.tools.get(key) || (cached?.type === "tool" ? cached.name : undefined), is_error: event.type === "session.tool.failed" }));
        state.tools.delete(key);
        break;
      }
      case "session.renamed":
        state.info = { ...state.info, title: data.title };
        send(base("session_updated", sessionID, { session_name: data.title }));
        break;
      case "session.execution.succeeded":
      case "session.execution.interrupted":
      case "session.execution.failed": {
        const failed = event.type === "session.execution.failed";
        send(base(failed ? "session_error" : "session_idle", sessionID, {
          is_error: failed, error_class: data.error?.type, error_message: data.error?.message,
          plan_proposed: !failed && state.info.agent === "plan" ? true : undefined,
        }));
        break;
      }
      case "session.compaction.started":
        send(base("session_compacting", sessionID));
        break;
      case "session.compaction.ended":
      case "session.compaction.failed":
        send(base("session_compacted", sessionID, { is_error: event.type === "session.compaction.failed" }));
        break;
    }
  }

  function sample(): void {
    if (disposed) return;
    if (!unsubscribe) {
      if (!ctx.keymap.commands().some((command) => command.id === "service.restart")) return;
      unsubscribe = ctx.data.listen(({ details }) => { events = events.then(() => onEvent(details)).catch(() => {}); });
      events = events.then(sweep).catch(() => {});
    }
    const route = current();
    if (route !== sampled) {
      if (sampled) { sessions.get(sampled)?.steps.clear(); sessions.get(sampled)?.tools.clear(); }
      sampled = route; epoch++; arrived = false;
    }
    for (const id of sessions.keys()) {
      if (id !== route && !tabbed(id) && ![...forwarded.values()].some((ask) => ask.sessionID === id)) sessions.delete(id);
    }
    if (!route || arrived || reading === epoch) return;
    reading = epoch;
    void arrive(route, epoch);
  }

  const sampler = setInterval(sample, 200);
  void Promise.resolve().then(sample);
  return async () => {
    if (disposed) return;
    disposed = true;
    clearInterval(sampler);
    unsubscribe?.();
    activations.delete(ctx);
    let timer: ReturnType<typeof setTimeout> | undefined;
    await Promise.race([feeds, new Promise<void>((finish) => { timer = setTimeout(finish, 1500); })]);
    if (timer) clearTimeout(timer);
  };
}

export default { id: "rimz", setup };
