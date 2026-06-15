# OpenCode protocol reference

> This doc mirrors OpenCode ahead of its adapter, the path [pi-reference.md](./pi-reference.md) proved. The mapping onto Rimz's internal types lands beside it with the adapter, under `docs/internals/agents/adapter/`: the agent-agnostic boundary, lifecycle, and context read-path is [agent.md](../../internals/agents/agent.md), and the account, balance, and spend model is [provider.md](../../internals/agents/provider.md). [Mapping feasibility](#mapping-feasibility) below is that work's starting brief.

This is the single home for the **OpenCode upstream protocol surface** a Rimz adapter binds to — the in-process plugin API (hooks, bus events, blocking returns, install surface), the SQLite session store, the server HTTP API, the auth file, and the CLI/env surface. It is a hand-maintained mirror of the opencode.ai docs and the published TypeScript wire types, pinned to the source URLs below; the storage, auth, event, and CLI shapes are additionally verified against a live `opencode` 1.15.13 install (2026-06-04).

Coverage is **depth on what an adapter would wire, breadth as an index**: the hooks, events, store fields, and decision returns an adapter would parse or emit are documented in full; the rest of the catalog is listed so a contributor wiring a new path knows it exists.

## Upstream sources

Re-fetch these pages to refresh this mirror. The docs publish unversioned and OpenCode releases near-daily, so pair a refresh with `opencode --version`; the npm packages are the version-exact schema source — `@opencode-ai/sdk`'s `dist/gen/types.gen.d.ts` is generated from the server's OpenAPI spec, the analogue of Codex's `generate-json-schema`. The repo moved orgs: `sst/opencode` 301-redirects to `anomalyco/opencode`.

| Surface | Source |
| --- | --- |
| Plugin API (loading, context, hooks) | <https://opencode.ai/docs/plugins/> |
| Server (`opencode serve`, HTTP API, SSE) | <https://opencode.ai/docs/server/> |
| SDK (`@opencode-ai/sdk`, OpenAPI-generated client) | <https://opencode.ai/docs/sdk/> |
| Config (locations, merge order, `plugin`, `permission`) | <https://opencode.ai/docs/config/> |
| Permissions (types, defaults, per-agent overrides) | <https://opencode.ai/docs/permissions/> |
| Agents and subagents | <https://opencode.ai/docs/agents/> |
| CLI (run / serve / attach / auth, flags) | <https://opencode.ai/docs/cli/> |
| Models and providers (models.dev catalog, variants) | <https://opencode.ai/docs/models/>, <https://opencode.ai/docs/providers/> |
| Zen (the curated per-token gateway) | <https://opencode.ai/docs/zen/> |
| Source repo (storage, bus, permission internals) | <https://github.com/anomalyco/opencode> |
| Typed wire schemas | npm [`@opencode-ai/plugin`](https://www.npmjs.com/package/@opencode-ai/plugin), [`@opencode-ai/sdk`](https://www.npmjs.com/package/@opencode-ai/sdk) |
| OpenAPI 3.1 spec | `GET /doc` on a running server |

## Integration surface — in-process server plugins

OpenCode is client/server even on one machine: every `opencode` TUI launch embeds its own HTTP server on `127.0.0.1` (`--port` defaults to **0** — a random free port per launch) and drives it over HTTP plus an SSE event stream. The integration surface is **TypeScript plugins loaded in-process by that server** (Bun runtime; the published module shape is `PluginModule = { id?, server: Plugin, tui?: never }` — an adapter sets only `server`) — OpenCode ships no out-of-process hook protocol and no statusline. A Rimz adapter is therefore a Rimz-authored plugin that subscribes to bus events and shells out to the `rimz` CLI, holding a permission open from inside the `permission.ask` hook when a decision must block.

> **Divergence — the decision channel inverts, as with Pi.** Claude and Codex run Rimz as a child and read its stdout as the decision. OpenCode runs Rimz's *plugin* in-process; the plugin runs `rimz` as *its* child, reads the answer from the child's stdout, and applies it through the hook's `output.status`. Hook-stdout discipline becomes child-stdout discipline, and the sync-install invariant has no on-disk shape to enforce — blocking is awaiting inside the handler.

Discovery:

| Location | Scope |
| --- | --- |
| `~/.config/opencode/plugin/` (also `plugins/`) — `*.ts` / `*.js` | global |
| `.opencode/plugin/` (also `plugins/`) | project-local |
| `opencode.json` — `plugin: ["npm:pkg", "file:./path.ts", ["spec", {options}]]` | configured; npm specifiers auto-install via Bun into `~/.cache/opencode/node_modules` |
| `opencode plugin <module>` | CLI install — writes the specifier into config |

Install for Rimz means **one Rimz-owned file** written to `~/.config/opencode/plugin/` — auto-discovered at the next launch, idempotent by path, removed by deleting the file. The file executes arbitrary code with the user's permissions inside every OpenCode server, so it belongs in the executable-surface trust hash like every hook config ([trust.md](../../internals/sidebar/trust.md)). `--pure` runs without external plugins — the integration-blind mode, same posture as an agent run before `rimz hooks install`.

A plugin module exports an async factory receiving `PluginInput` and returning its `Hooks`; `node:` built-ins and npm dependencies are importable.

| Field | Carries |
| --- | --- |
| `client` | an `@opencode-ai/sdk` HTTP client already pointed at the owning server |
| `serverUrl` | the embedded server's base URL — the only in-process place the random port surfaces |
| `project` / `directory` / `worktree` | project identity, working directory, git worktree root |
| `$` | Bun shell for spawning children |
| `experimental_workspace` | workspace-adapter registration (index only) |

## Plugin hooks

The `Hooks` members an adapter would wire (verbatim from the published 1.15.13 types):

```ts
event?: (input: { event: Event }) => Promise<void>
"permission.ask"?: (input: Permission, output: { status: "ask" | "deny" | "allow" }) => Promise<void>
"tool.execute.before"?: (input: { tool, sessionID, callID }, output: { args }) => Promise<void>
"tool.execute.after"?: (input: { tool, sessionID, callID, args }, output: { title, output, metadata }) => Promise<void>
"chat.message"?: (input: { sessionID, agent?, model?, messageID?, variant? }, output: { message: UserMessage, parts: Part[] }) => Promise<void>
dispose?: () => Promise<void>
```

`event` is the firehose — every bus event below flows through it. `permission.ask` is the one blocking hook ([the decision channel](#the-decision-channel--permissionask)). `tool.execute.before` / `tool.execute.after` bracket each tool call and may mutate `args` or rewrite `output` — an adapter only observes. `chat.message` fires per user prompt with the typed message and parts; its `variant` is the model's reasoning variant (`"xhigh"`, …) — the effort surface. `dispose` fires when the owning server shuts down.

Index of the rest: `config`, `auth` (custom provider login flows), `provider`, `tool` (custom tool registration), `tool.definition`, `chat.params`, `chat.headers`, `command.execute.before`, `shell.env`, and the `experimental.*` family (`chat.messages.transform`, `chat.system.transform`, `session.compacting`, `compaction.autocontinue`, `text.complete`).

## Bus events

The `event` hook and the server's `GET /event` SSE stream carry one tagged union — `{ type, properties }`. The catalog below is extracted from the live 1.15.13 SDK types; upstream rebuilt the event system in v1.15.0, so re-verify names on each refresh.

### Events an adapter would wire

| Event | Properties | Carries |
| --- | --- | --- |
| `session.created` | `info: Session` | session registration; a child session carries `parentID` — the subagent signal |
| `session.updated` | `info: Session` | title, `time.compacting`, revert/share state |
| `session.idle` | `sessionID` | the turn boundary — the prompt's work completed |
| `session.error` | `sessionID?`, `error?` | a typed error union at the turn boundary: `ProviderAuthError \| UnknownError \| MessageOutputLengthError \| MessageAbortedError \| ApiError` — an in-band death certificate |
| `session.status` | `sessionID`, `status` | `{type:"idle"} \| {type:"busy"} \| {type:"retry", attempt, message, next}` — `retry` is the only place provider throttling surfaces |
| `session.deleted` | `info: Session` | session removed |
| `session.compacted` | `sessionID` | compaction completed (trailing) |
| `session.diff` | `sessionID`, `diff: FileDiff[]` | per-session diff stats |
| `message.updated` | `info: Message` | a user message is the prompt; an assistant message carries `tokens`, `cost`, `modelID` / `providerID`, `finish`, `error?` |
| `message.part.updated` | `part: Part`, `delta?` | tool parts step `pending → running → completed/error`; `step-finish` parts carry per-step `tokens` + `cost` |
| `permission.updated` | `Permission` | a pending native ask (the observe side of the decision channel) |
| `permission.replied` | `sessionID`, `permissionID`, `response` | the native answer (`once` / `always` / `reject`) |
| `todo.updated` | `sessionID`, `todos: Todo[]` | todo list (`content`, `status`, `priority` per item) |
| `file.edited` | `file` | a file-writing signal (no session id — session attribution rides tool parts instead) |

### Event index (the rest)

`message.removed`, `message.part.removed`, `command.executed`, `file.watcher.updated`, `installation.updated` / `installation.update-available`, `lsp.updated` / `lsp.client.diagnostics`, `pty.{created,updated,exited,deleted}`, `server.connected`, `server.instance.disposed`, `tui.{prompt.append,command.execute,toast.show}`, `vcs.branch.updated`.

### Key payload shapes

```ts
type Session = {
  id: string                      // "ses_…"
  projectID: string
  directory: string               // the session's cwd — the worktree/cwd bind
  parentID?: string               // present on a subagent's child session
  title: string
  version: string                 // the OpenCode version that wrote it
  time: { created: number, updated: number, compacting?: number }   // epoch ms
  summary?: { additions, deletions, files, diffs? }
  share?: { url }, revert?: { … }
}

type AssistantMessage = {
  id: string                      // "msg_…"
  sessionID: string
  parentID: string                // "msg_…" — the message this one answers
  role: "assistant"
  time: { created: number, completed?: number }
  error?: ProviderAuthError | UnknownError | MessageOutputLengthError | MessageAbortedError | ApiError
  modelID: string, providerID: string
  mode: string
  path: { cwd: string, root: string }
  summary?: boolean               // true on a compaction summary message
  cost: number
  tokens: { input, output, reasoning, cache: { read, write } }
  finish?: string                 // "stop", …
}
```

A live 1.15.13 row additionally carries `agent` ("build", …) and `variant` ("xhigh", …) on the assistant blob — the published SDK type lags the wire; parse tolerantly.

## The decision channel — `permission.ask`

`permission.ask` fires when a tool call needs permission, before the native dialog is created:

```ts
"permission.ask"?: (input: Permission, output: { status: "ask" | "deny" | "allow" }) => Promise<void>

type Permission = {
  id: string
  type: string                    // the permission: "bash", "edit", "webfetch", …
  pattern?: string | string[]     // the matched rule, e.g. the bash command pattern
  sessionID: string
  messageID: string
  callID?: string
  title: string                   // the human line the dialog shows
  metadata: { [key: string]: unknown }
  time: { created: number }
}
```

Setting `output.status` to `allow` or `deny` short-circuits the dialog; leaving `ask` falls through to the native TUI dialog, which emits `permission.updated` (pending) and `permission.replied` (answered: `once` / `always` / `reject`). The native reply also rides HTTP: `POST /session/:id/permissions/:permissionID`.

**The neutral path is `ask`.** Rimz's three operating paths map directly: a fresh resolver holds the hook open on the bridge and answers `allow` / `deny`; the timeout and no-resolver paths return `ask`, so the agent's own dialog asks the human — the `native_ui` fallback is a first-class upstream value rather than an empty-stdout convention.

**Asks are config-dependent.** Permission defaults are permissive — most tools run without asking. The typed config keys are `edit`, `bash` (a single action or a pattern → action map, last match wins), `webfetch`, `doom_loop`, and `external_directory`; `doom_loop` and `external_directory` default to `ask`, `.env` reads are denied by default, and everything else defaults to `allow`. A default-config OpenCode therefore fires few native asks, and the blocking channel engages only as far as the user's `permission` config asks — closer to Claude's `bypassPermissions` than to its default mode.

**Pinned caveat.** [anomalyco/opencode#19927](https://github.com/anomalyco/opencode/issues/19927) reports first-encounter commands bypassing the `permission.ask` hook on some paths; re-verify against the current release before relying on the hook as the sole interception point.

**The `question` tool.** OpenCode ships a built-in `question` tool (its user-question primitive). The 1.15.13 SDK publishes no `question.*` event; newer development builds reference a `question.asked` bus event — re-verify on refresh before wiring a user-question feed kind.

## Server HTTP API (index)

Each TUI launch owns a private server; there is no fixed port and no published discovery surface (no lockfile, no well-known socket) — the one in-process place the port surfaces is the plugin's `serverUrl`. Detached modes exist: `opencode serve` (`--port`, `--hostname`, `--mdns`, `--cors`), `opencode web`, and `opencode attach <url>` to point a TUI at a running server. Optional HTTP basic auth rides `OPENCODE_SERVER_PASSWORD` (with `OPENCODE_SERVER_USERNAME`, default `opencode`).

- `GET /global/health` → `{"healthy":true,"version":"1.15.13"}` (live-verified) — the version probe.
- `GET /event` — the SSE stream of the bus events above.
- `GET /doc` — the OpenAPI 3.1 spec the SDK is generated from; the version-exact method catalog.
- `GET /session`, `GET /session/:id/message`, `POST /session/:id/message`, `POST /session/:id/permissions/:permissionID`, `GET /config`, `GET /find/*`, … — the typed client is `createOpencodeClient` from `@opencode-ai/sdk`.

`opencode acp` exposes the same agent over the Agent Client Protocol (the Zed editor protocol) — an alternate embedding surface, recorded for breadth.

## Session store — SQLite

OpenCode 1.15 stores sessions in **one SQLite database** — `~/.local/share/opencode/opencode.db` (under the `XDG_DATA_HOME` root; WAL mode, so `-shm` / `-wal` siblings ride along), schema-managed by Drizzle migrations. Earlier releases wrote a flat JSON tree under `storage/`; a live 1.15.13 leaves only the `storage/migration` marker and `storage/session_diff/<sessionID>.json`, and third-party writeups describing the flat tree are stale. The session/message/part tables are the transcript: per-row JSON blobs in a `data` column, with the hot fields lifted into typed columns.

| Table | Key columns (live-verified) |
| --- | --- |
| `session` | `id` (`ses_…`), `project_id`, **`parent_id`** (set on a subagent's child session), `slug`, `directory`, `title`, **`version`** (the writing OpenCode version), `agent`, `model` (JSON `{id, providerID, variant}`), **`cost`**, **`tokens_input` / `tokens_output` / `tokens_reasoning` / `tokens_cache_read` / `tokens_cache_write`** (precomputed per-session aggregates), `time_created` / `time_updated` / `time_compacting` / `time_archived` (epoch ms), `summary_*`, `share_url`, `permission`, `revert`, `workspace_id`, `metadata` |
| `message` | `id` (`msg_…`), `session_id`, `time_created`, `time_updated`, `data` (the JSON blob below) |
| `part` | `id`, `message_id`, `session_id`, `time_created`, `time_updated`, `data` (JSON — `step-finish` parts carry per-step `tokens` + `cost`; `tool` parts carry the call state) |
| the rest | `project` (worktree identity), `workspace`, `permission` (per-project ruleset), `todo`, `session_share`, `event` / `event_sequence`, `account` / `account_state` (OpenCode-cloud login, not provider auth), `data_migration`, `__drizzle_migrations` |

A live assistant `message.data` blob (1.15.13, paths trimmed):

```jsonc
{"parentID":"msg_…","role":"assistant","mode":"build","agent":"build","variant":"xhigh",
 "path":{"cwd":"…","root":"…"},
 "cost":0,
 "tokens":{"total":9664,"input":3481,"output":8,"reasoning":31,"cache":{"write":0,"read":6144}},
 "modelID":"gpt-5.5","providerID":"openai",
 "time":{"created":1780590149011,"completed":1780590154568},
 "finish":"stop"}
```

Three properties matter for any read:

- **The per-session aggregates are precomputed.** `cost` plus the five token columns live on `session`, so a spend walk can total sessions without touching messages and drop to per-message rows (`time_created` is epoch ms) only where a trailing-window boundary splits a session.
- **Zero `cost` means unpriced, not free.** The live oauth row above logs `cost: 0` with real token counts — a subscription login carries no per-token price — so dollars resolve from tokens through a pricing table, while a positive `cost` is authoritative.
- **Context tokens are `input + cache.read + cache.write`** (the Anthropic-style split, output excluded), and no row carries a context window — the divisor resolves from the models.dev catalog as the model's max input tokens (`Model.limit.input`, falling back to the total `Model.limit.context` when a model lists no separate input cap), the registry-resolved pattern [pi-reference.md](./pi-reference.md#session-jsonl) documents.

CLI read alternatives: `opencode export [sessionID]` prints a session as JSON (`--sanitize` redacts), `opencode stats` totals usage and cost (`--days`, `--models`, `--tools`, `--project`), and `opencode db "<sql>" --format json` runs arbitrary SQL over the store — each spawns the full app, so they suit probes, not per-tick reads.

## Auth file

`~/.local/share/opencode/auth.json` (created `0600`) maps provider → credential; `opencode auth login` manages entries (live-verified oauth shape):

```jsonc
{
  "openai":    { "type": "oauth", "access": "…", "refresh": "…", "expires": <epoch ms>, "accountId": "…" },
  "anthropic": { "type": "oauth", "access": "…", "refresh": "…", "expires": <epoch ms> },   // Claude Pro/Max login
  "deepseek":  { "type": "api", "key": "sk-…", "metadata": { … } }                          // any API-key provider
}
```

The record union is `oauth { access, refresh, expires (epoch ms), accountId?, enterpriseUrl? }` | `api { key, metadata? }` | `wellknown { key, token }`; `accountId` rides the live file but lags the published SDK `Auth` type — the file shape is the surface an adapter reads. MCP-server OAuth tokens live apart in `mcp-auth.json`.

**The balance gap.** OpenCode exposes no rate-limit windows and no plan tier — no statusline, no quota API, and `auth.json` carries credentials only. One account fact is probe-able: credential type (`oauth` → metered subscription, `api` → unmetered). The only place provider throttling surfaces is the `session.status` `retry` state (an attempt count and a message string), which is uncontracted enrichment, not a balance. OpenCode is also **multi-provider by design** — one session can run any configured provider, and a Zen (`opencode`) login meters per-token rather than per-window — so the provider dashboard keys by the agent kind and aggregates whatever accounts OpenCode used, the Pi posture ([provider.md → Per-provider mapping](../../internals/agents/provider.md#per-provider-mapping)).

## CLI and environment surface

The official `curl -fsSL https://opencode.ai/install | bash` installer places the binary at `~/.opencode/bin/opencode` and appends that directory to `PATH` through a shell rc, so a non-login or daemon environment commonly runs with `opencode` installed but absent from `PATH`.

The flags and variables an adapter (and the resume-on-rebirth planner) cares about:

| Surface | Meaning |
| --- | --- |
| `opencode -v` | version (`1.15.13` verified) |
| `opencode [project]` | the TUI, embedding its private server (`--port` default 0 — random per launch; `--hostname` default `127.0.0.1`) |
| `opencode -c` / `--continue`, `-s <id>` / `--session <id>`, `--fork` | resume the newest session / resume by id — the resume-on-rebirth seed / branch into a copy |
| `opencode run [message…]` | headless one-shot |
| `opencode serve` / `web` / `attach <url>` | detached server / browser UI / point a TUI at a running server |
| `opencode export [sessionID] [--sanitize]` / `import <file>` | session JSON out / in |
| `opencode stats --days N --models --tools --project` | usage and cost totals over the store |
| `opencode db "<sql>" --format json` | SQL over the store |
| `opencode plugin <module>` | install a plugin specifier into config |
| `opencode providers` (alias `auth`) | login management (`auth.json`) |
| `opencode acp` / `github` / `pr <n>` | ACP server, GitHub agent, PR checkout (index) |
| `--pure` | run without external plugins — the integration-blind mode |
| `--print-logs`, `--log-level` | logs to stderr |

| Variable | Meaning |
| --- | --- |
| `XDG_DATA_HOME` / `XDG_CONFIG_HOME` / `XDG_CACHE_HOME` / `XDG_STATE_HOME` | relocate the data root (db, `auth.json`, logs), the config root (config, plugins), caches, and state |
| `OPENCODE_CONFIG` | explicit config-file path (docs-sourced) |
| `OPENCODE_SERVER_USERNAME` / `OPENCODE_SERVER_PASSWORD` | HTTP basic auth on the server |
| `OPENCODE=1` / `OPENCODE_PID` / `AGENT=1` | stamped into the environment of shells and tools OpenCode spawns (repo-sourced; absent from the TUI process's own environ — live-checked), so a child can detect it runs under OpenCode |

## Mapping feasibility

The adapter verdict has landed in [opencode.md](../../internals/agents/adapter/opencode.md): OpenCode is wired as a first-class `AgentAdapter` through one Rimz-authored in-process plugin plus a read-only SQLite spend reader. Unlike Pi, the three operating paths engage natively because `permission.ask` is a real blocking decision channel with an upstream `ask` fallback. Like Pi, the integration is one whole-file plugin that runs Rimz as its child.

| Native surface | Channel | Landed mapping |
| --- | --- | --- |
| `session.created` (no `parentID`) | lifecycle | `Registered` — worktree from `Session.directory` |
| `chat.message` hook (or a user `message.updated`) | lifecycle | `TurnStarted` — sanitized prompt labels the row; `variant` ↔ the `effort` carry-forward |
| `session.idle` | lifecycle | `TurnEnded { errored: false }` |
| `session.error` | lifecycle | the error bit for the enclosing turn — a typed, in-band death certificate (`ApiError`, `MessageAbortedError`, …), Pi-grade: no transcript forensics needed |
| `tool.execute.after` (mutating tool) | lifecycle | `ToolUsed { mutates: true, edits }` — `edit` / `write` / `apply_patch` edit files; `bash` mutates only; read-only tools stay silent |
| `session.created` (with `parentID`) / child `session.idle` or `session.error` | lifecycle | `SubagentStarted` / `SubagentStopped` — the child session id keys the child, `parentID` links the parent |
| `experimental.session.compacting` → `session.compacted` | lifecycle | `Compacting` — a leading signal like Claude's `PreCompact`, cleared by the trailing event |
| `dispose` | — | not forwarded — server-scoped and carries no session id; pane liveness and the rollup reaper are the session-end posture |
| `permission.ask` | blocking-feed | `waiting` — bridge wait inside the hook; `allow` / `deny` on a resolver answer, `ask` as the neutral path |

- **Identity.** The plugin runs inside the server the pane's TUI embeds, so an interactive OpenCode is standalone and stampable — the in-process environment carries the pane id, and pid capture rides the spawned `rimz` child. A session exists only once created (typically at the first prompt), so OpenCode is a `registers_lazily` candidate — the Codex pattern: idle-row synthesis before the first turn, cwd-bind from `Session.directory` ([agent.md → The instance lifecycle](../../internals/agents/agent.md#the-instance-lifecycle)). A session served by a detached `opencode serve`, reached over `attach`, or driven from the web UI is daemon-routed/remote — the documented remote-agent gap.
- **Context gauge.** Every assistant message carries the full token split — in-process on `message.updated`, at rest in SQLite — so the gauge rides lifecycle events with no transcript tail. The plugin resolves the context-window divisor for every model family from OpenCode's own model catalog as the model's max input tokens (`Model.limit.input`, falling back to the total `Model.limit.context`; read once per server launch via the in-process `client.config.providers()`), keyed `${providerID}/${modelID}` and stamped onto each lifecycle envelope; a Claude-family local table is the offline fallback when the catalog read is unavailable.
- **Spend.** The SQLite store is the cost surface: per-message rows supply trailing-window bucketing and origin paths. The adapter opens SQLite read-only against the WAL database. Zero `cost` under a subscription login prices from tokens via [provider.md → Token pricing](../../internals/agents/provider.md#token-pricing) (the Codex rule); a positive `cost` is authoritative (the Pi rule).
- **Account probe and usage.** `auth.json` distinguishes oauth from API-key credentials per provider — enough for logged-in plus metered/unmetered on the dashboard, the same single account fact Pi's probe documents. The selected OAuth credential also feeds the out-of-band usage probe, which queries the backing provider's own quota endpoint over that token: an `anthropic` credential reuses Claude's Anthropic OAuth usage fetcher, `openai`/`openai-codex` reuse Codex's ChatGPT usage fetcher, and any other provider has no mapped endpoint and returns nothing. OpenCode introduces no endpoint of its own.

**What OpenCode cannot support:**

- **No realtime balance transport, no plan tier.** The plugin sees no provider response headers, so OpenCode surfaces no live rate-limit windows the way Claude's statusline or Codex's app-server do, and no plan tier anywhere. Its budget bars come entirely from the out-of-band OAuth usage probe over the backing-provider token ([provider.md → Per-provider mapping](../../internals/agents/provider.md#per-provider-mapping)); an API-key or `wellknown` login has no token, so it shows account identity and spend without bars. The `session.status` `retry` state is the one in-band throttling glimpse, and it is uncontracted.
- **No rich-context transport.** The per-launch server sits on a random port with no discovery surface, so Rust has no statusline or app-server analogue to read out of band; the in-process plugin reads its owning server directly (the model catalog for the context window), and the events plus the SQLite store cover the rest of the gauge. A future increment: the Rimz plugin publishes its `serverUrl` to a runtime sidecar, the way the Codex broker holds a warm connection, should Rust need an out-of-band read beyond what the plugin already stamps.
- **Few native asks by default.** Permission defaults are permissive, so the blocking channel engages only as far as the user's `permission` config asks. The [#19927](https://github.com/anomalyco/opencode/issues/19927) hook-bypass report stays pinned as an upstream caveat to re-check on reference refresh.
- **No per-session end event.** `dispose` fires per server, not per session; a session that ends inside a still-running instance leaves by pane liveness and the reaper alone, the Codex posture.
