# OpenCode protocol reference

> This doc mirrors OpenCode ahead of its adapter, the path [pi-reference.md](./pi-reference.md) proved. The mapping onto RimZ's internal types lands beside it with the adapter, under `docs/internals/agents/`: the agent-agnostic boundary, lifecycle, and context read-path is [model.md](../../internals/agents/model.md), and the account, balance, and spend model is [providers.md](../../internals/agents/providers.md). [Mapping feasibility](#mapping-feasibility) below is that work's starting brief.

This is the single home for the **OpenCode upstream protocol surface** a RimZ adapter binds to — the in-process plugin API (hooks, bus events, blocking returns, install surface), the SQLite session store, the server HTTP API, the auth file, and the CLI/env surface. It is a hand-maintained mirror of the opencode.ai docs and the published TypeScript wire types, pinned to the source URLs below. The runtime and storage shapes are live-verified against an installed `opencode` 1.17.9 binary; protocol changes are verified against the `v1.17.18` source tag and the published 1.17.18 SDK/plugin packages (2026-07-10).

Coverage is **depth on what an adapter would wire, breadth as an index**: the hooks, events, store fields, and decision returns an adapter would parse or emit are documented in full; the rest of the catalog is listed so a contributor wiring a new path knows it exists.

## Upstream sources

Re-fetch these pages to refresh this mirror. The docs publish unversioned and OpenCode releases near-daily, so pair a refresh with `opencode --version`; the npm packages are the version-exact schema source. `@opencode-ai/sdk` publishes the legacy-compatible generated types at `dist/gen/types.gen.d.ts` and the current bus/API types at `dist/v2/gen/types.gen.d.ts`; the server bridges current events into the plugin `event` hook even when the legacy union has not caught up. The repo moved orgs: `sst/opencode` 301-redirects to `anomalyco/opencode`.

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
| Source repo (storage, bus, permission internals) | <https://github.com/anomalyco/opencode/tree/v1.17.18> |
| Typed wire schemas | npm [`@opencode-ai/plugin`](https://www.npmjs.com/package/@opencode-ai/plugin), [`@opencode-ai/sdk`](https://www.npmjs.com/package/@opencode-ai/sdk) |
| OpenAPI 3.1 spec | `GET /doc` on a running server |

## Integration surface — in-process server plugins

OpenCode is client/server even on one machine: every `opencode` TUI launch embeds its own HTTP server on `127.0.0.1` (`--port` defaults to **0** — a random free port per launch) and drives it over HTTP plus an event stream. The integration surface is **TypeScript plugins loaded in-process by that server** (Bun runtime; the published module shape is `PluginModule = { id?, server: Plugin, tui?: never }` — an adapter sets only `server`) — OpenCode ships no out-of-process hook protocol and no statusline. A RimZ adapter is therefore a RimZ-authored plugin that subscribes to bus events and shells out to the `rimz` CLI.

> **Divergence — OpenCode owns the native prompts.** OpenCode runs RimZ's plugin in-process, and the plugin runs `rimz` as its child. Current permission and question gates arrive as bus observations after OpenCode opens an awaited native request; RimZ routes attention and leaves the answer in OpenCode's UI. The published legacy `permission.ask` hook still defines a child-stdout decision path, but OpenCode 1.17.18 no longer invokes that hook from its permission service.

Discovery:

| Location | Scope |
| --- | --- |
| `~/.config/opencode/plugins/` (canonical; `plugin/` also scanned) — `*.ts` / `*.js` | global |
| `.opencode/plugins/` (canonical; `plugin/` also scanned) | project-local |
| `opencode.json` — `plugin: ["npm:pkg", "file:./path.ts", ["spec", {options}]]` | configured; npm specifiers auto-install via Bun into `~/.cache/opencode/node_modules` |
| `opencode plugin <module>` | CLI install — writes the specifier into config |

Install for RimZ means **one RimZ-owned file** written to `~/.config/opencode/plugin/` — auto-discovered at the next launch, idempotent by path, removed by deleting the file. The file executes arbitrary code with the user's permissions inside every OpenCode server, so it belongs in the executable-surface trust hash like every hook config ([trust.md](../../internals/harness/trust.md)). `--pure` runs without external plugins — the integration-blind mode, same posture as an agent run before `rimz hooks install`.

A plugin module exports an async factory receiving `PluginInput` and returning its `Hooks`; `node:` built-ins and npm dependencies are importable.

| Field | Carries |
| --- | --- |
| `client` | an `@opencode-ai/sdk` HTTP client already pointed at the owning server |
| `serverUrl` | the embedded server's base URL — the only in-process place the random port surfaces |
| `project` / `directory` / `worktree` | project identity, working directory, git worktree root |
| `$` | Bun shell for spawning children |
| `experimental_workspace` | workspace-adapter registration (index only) |

## Plugin hooks

The `Hooks` members an adapter would wire (verbatim from the published 1.17.18 types):

```ts
event?: (input: { event: Event }) => Promise<void>
"permission.ask"?: (input: Permission, output: { status: "ask" | "deny" | "allow" }) => Promise<void>
"tool.execute.before"?: (input: { tool, sessionID, callID }, output: { args }) => Promise<void>
"tool.execute.after"?: (input: { tool, sessionID, callID, args }, output: { title, output, metadata }) => Promise<void>
"chat.message"?: (input: { sessionID, agent?, model?, messageID?, variant? }, output: { message: UserMessage, parts: Part[] }) => Promise<void>
dispose?: () => Promise<void>
```

`event` is the firehose — current bus events flow through it as `{ id, type, properties }`. `permission.ask` remains in the compatibility hook type, but the 1.17.18 permission service publishes `permission.asked` without triggering that hook; observe the bus event for current releases. `tool.execute.before` / `tool.execute.after` bracket each tool call and may mutate `args` or rewrite `output` — an adapter only observes. `chat.message` fires per user prompt with the typed message and parts; its `variant` is the model's reasoning variant (`"xhigh"`, …) — the effort surface. `dispose` fires when the owning server shuts down.

Index of the rest: `config`, `auth` (custom provider login flows), `provider`, `tool` (custom tool registration), `tool.definition`, `chat.params`, `chat.headers`, `command.execute.before`, `shell.env`, and the `experimental.*` family (`chat.messages.transform`, `chat.system.transform`, `session.compacting`, `compaction.autocontinue`, `text.complete`).

## Bus events

The `event` hook and the server event stream carry one tagged union — `{ id, type, properties }`. The catalog below follows the 1.17.18 runtime bridge and `@opencode-ai/sdk/v2` types. The package's legacy `@opencode-ai/sdk` `Event` export still names the old permission event `permission.updated` and omits question events; plugin code must parse the runtime boundary tolerantly until that compatibility union converges.

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
| `permission.asked` | `id`, `sessionID`, `permission`, `patterns`, `metadata`, `always`, `tool?` | a pending native permission request; the current attention signal |
| `permission.replied` | `sessionID`, `requestID`, `reply` | the native answer (`once` / `always` / `reject`) |
| `question.asked` | `id`, `sessionID`, `questions`, `tool?` | a pending question-tool request; each question carries `question`, `header`, `options`, `multiple?`, and `custom?` |
| `question.replied` / `question.rejected` | `sessionID`, `requestID`, `answers?` | the native question outcome |
| `todo.updated` | `sessionID`, `todos: Todo[]` | todo list (`content`, `status`, `priority` per item) |
| `file.edited` | `file` | a file-writing signal (no session id — session attribution rides tool parts instead) |

OpenCode 1.18.2 emits an aborted assistant `message.updated` with `input`, `output`, `cache.read`, and `cache.write` all zero and `tokens.total` omitted. This live-verified shape represents an unavailable streaming measurement; it does not report a fresh zero-usage call.

### Event index (the rest)

`message.removed`, `message.part.removed`, `command.executed`, `file.watcher.updated`, `installation.updated` / `installation.update-available`, `lsp.updated` / `lsp.client.diagnostics`, `pty.{created,updated,exited,deleted}`, `server.connected`, `server.instance.disposed`, `tui.{prompt.append,command.execute,toast.show,session.select}`, `vcs.branch.updated`, and workspace/worktree state events. The v2 SDK also exposes experimental durable `session.next.*` events; the compatibility plugin API does not require an adapter to bind to them.

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

A live 1.17.9 row additionally carries `agent` ("build", …) and `variant` ("xhigh", …) on the assistant blob — the legacy SDK type lags the wire; parse tolerantly.

## Native permission and question gates

The current permission service publishes `permission.asked` after creating an awaited request. Its runtime shape is:

```ts
type PermissionRequest = {
  id: string
  sessionID: string
  permission: string              // "bash", "edit", "webfetch", …
  patterns: string[]              // matched command/path/URL patterns
  metadata: { [key: string]: unknown }
  always: string[]                // patterns an "always" answer may persist for the session
  tool?: { messageID: string, callID: string }
}
```

The native UI replies with `once`, `always`, or `reject`, published as `permission.replied`. `GET /permission` lists pending requests and `POST /permission/:requestID/reply` answers one; the older `POST /session/:id/permissions/:permissionID` route remains in the compatibility API.

The published plugin `Hooks` type still contains `permission.ask(input, output)` with `output.status: "ask" | "deny" | "allow"`. OpenCode 1.17.18 does not call it from the permission service, so use `permission.asked` as the current observation contract. RimZ keeps the hook for compatibility with releases that invoke it and leaves its neutral status at `ask`.

**Asks are config-dependent.** Permission defaults are permissive — most tools run without asking. The current typed keys include `read`, `edit`, `glob`, `grep`, `list`, `bash`, `task`, `todowrite`, `question`, `webfetch`, `websearch`, `lsp`, `skill`, `doom_loop`, and `external_directory`; tool-specific and wildcard keys are also accepted. Rules may be a single action or a pattern → action map, with the last matching rule winning. `doom_loop` and `external_directory` default to `ask`, `.env` reads are denied by default, and most other permissions default to `allow`.

**Compatibility caveat.** [anomalyco/opencode#19927](https://github.com/anomalyco/opencode/issues/19927) reported first-encounter commands bypassing `permission.ask` and is closed as not planned. The 1.17.18 source has completed that shift: the permission service publishes the bus request and contains no `Plugin.trigger("permission.ask", …)` call. Treat the hook as backward compatibility rather than the current interception point.

**The `question` tool.** OpenCode's user-question primitive publishes `question.asked`, then `question.replied` or `question.rejected`. A request contains one or more questions with a full question string, short header, labeled options, and optional multiple/custom-answer flags. `GET /question`, `POST /question/:requestID/reply`, and `POST /question/:requestID/reject` expose the same awaited requests over HTTP. These events and routes are published in the 1.17.18 v2 SDK; the legacy `Event` union omits them.

## Server HTTP API (index)

Each TUI launch owns a private server; there is no fixed port and no published discovery surface (no lockfile, no well-known socket) — the one in-process place the port surfaces is the plugin's `serverUrl`. Detached modes exist: `opencode serve` (`--port`, `--hostname`, `--mdns`, `--mdns-domain`, `--cors`), `opencode web`, and `opencode attach <url>` to point a TUI at a running server. Optional HTTP basic auth rides `OPENCODE_SERVER_PASSWORD` (with `OPENCODE_SERVER_USERNAME`, default `opencode`).

- `GET /global/health` → `{"healthy":true,"version":"1.17.9"}` (live-verified shape) — the version probe.
- `GET /config/providers` → provider catalog, including `providers[].models` and display `name`; RimZ uses it read-only to map the lifecycle model hint to `model_display_name`.
- `GET /session/:id` → session metadata, including `title`, `version`, `model`, token/cost aggregates, and timestamps; RimZ uses it read-only for the session title.
- `GET /event` — the compatibility SSE stream of the bus events above; the v2 SDK also publishes a `/v2/event` subscription surface.
- `GET /doc` — the OpenAPI 3.1 spec the SDK is generated from; the version-exact method catalog.
- `GET /permission`, `POST /permission/:requestID/reply`, `GET /question`, `POST /question/:requestID/reply`, and `POST /question/:requestID/reject` expose current native gates.
- `GET /session`, `GET /session/:id/message`, `POST /session/:id/message`, `POST /session/:id/permissions/:permissionID`, `GET /config`, `GET /find/*`, … — the typed client is `createOpencodeClient` from `@opencode-ai/sdk`.

`opencode acp` exposes the same agent over the Agent Client Protocol (the Zed editor protocol) — an alternate embedding surface, recorded for breadth.

## Session store — SQLite

OpenCode 1.17 stores sessions in **one SQLite database** — `~/.local/share/opencode/opencode.db` (under the `XDG_DATA_HOME` root; WAL mode, so `-shm` / `-wal` siblings ride along), schema-managed by Drizzle migrations. Earlier releases wrote a flat JSON tree under `storage/`; a live 1.17.9 install leaves only the `storage/migration` marker and `storage/session_diff/<sessionID>.json`, and third-party writeups describing the flat tree are stale. The `message` / `part` compatibility projections remain the simplest transcript read for RimZ, while the current durable model also records ordered `session_message` events, pending `session_input`, and the active `session_context_epoch`.

| Table | Key columns (live-verified) |
| --- | --- |
| `session` | `id` (`ses_…`), `project_id`, **`parent_id`** (set on a subagent's child session), `slug`, `directory`, `title`, **`version`** (the writing OpenCode version), `agent`, `model` (JSON `{id, providerID, variant}`), **`cost`**, **`tokens_input` / `tokens_output` / `tokens_reasoning` / `tokens_cache_read` / `tokens_cache_write`** (precomputed per-session aggregates), `time_created` / `time_updated` / `time_compacting` / `time_archived` (epoch ms), `summary_*`, `share_url`, `permission`, `revert`, `workspace_id`, `metadata` |
| `message` | `id` (`msg_…`), `session_id`, `time_created`, `time_updated`, `data` (the JSON blob below) |
| `part` | `id`, `message_id`, `session_id`, `time_created`, `time_updated`, `data` (JSON — `step-finish` parts carry per-step `tokens` + `cost`; `tool` parts carry the call state) |
| `session_message` | durable ordered session messages: `id`, `session_id`, `type`, `seq`, timestamps, `data`; the `(session_id, seq)` pair is unique |
| `session_input` / `session_context_epoch` | admitted/pending prompts and the active context baseline/snapshot |
| the rest | `project` / `project_directory` (worktree identity), `workspace`, `permission` (per-project ruleset), `todo`, `session_share`, `event` / `event_sequence`, `credential` (v2 integration credential state), `account` / `account_state` / `control_account` (OpenCode-cloud/control-plane login), `data_migration`, `__drizzle_migrations` |

A live assistant `message.data` blob (1.17.9, paths trimmed):

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

**The balance gap.** OpenCode exposes no rate-limit windows and no plan tier — no statusline, no quota API, and `auth.json` carries credentials only. One account fact is probe-able: credential type (`oauth` → metered subscription, `api` → unmetered). The only place provider throttling surfaces is the `session.status` `retry` state (an attempt count and a message string), which is uncontracted enrichment, not a balance. OpenCode is also **multi-provider by design** — one session can run any configured provider, and a Zen (`opencode`) login meters per-token rather than per-window — so the provider dashboard keys by the agent kind and aggregates whatever accounts OpenCode used, the Pi posture ([provider.md → Per-provider mapping](../../internals/agents/providers.md#per-provider-mapping)).

## CLI and environment surface

The official `curl -fsSL https://opencode.ai/install | bash` installer places the binary at `~/.opencode/bin/opencode` and appends that directory to `PATH` through a shell rc, so a non-login or daemon environment commonly runs with `opencode` installed but absent from `PATH`.

The flags and variables an adapter (and the resume-on-rebirth planner) cares about:

| Surface | Meaning |
| --- | --- |
| `opencode -v` | version (`1.17.9` live-verified; `1.17.18` current stable at refresh) |
| `opencode [project]` | the TUI, embedding its private server (`--port` default 0 — random per launch; `--hostname` default `127.0.0.1`) |
| `opencode -c` / `--continue`, `-s <id>` / `--session <id>`, `--fork` | resume the newest session / resume by id — the resume-on-rebirth seed / branch into a copy |
| `opencode run [message…]` | headless one-shot; `--format json`, `--attach`, `--dir`, `--variant`, and `--thinking` cover structured/remote/reasoning modes |
| `opencode -m/--model <provider/model>` | select the provider model; the adapter passes this flag on interactive launches |
| `opencode run --variant <level>`, `opencode run --thinking` | headless-run-only reasoning/display flags; unavailable to the interactive pane launch |
| `opencode --agent <name>` | select the primary agent for the interactive session; `--agent plan` starts in the built-in plan agent (live-verified on 1.17.20) |
| `opencode --auto` | auto-approve permissions that are not explicitly denied (live-verified on 1.17.20) |
| `opencode serve` / `web` / `attach <url>` | detached server / browser UI / point a TUI at a running server; server modes expose mDNS domain and CORS flags |
| `opencode session list` / `session delete <id>` | list sessions (`--format json`, `--max-count`) / delete one |
| `opencode agent create` / `agent list`; `opencode models [provider]` | manage agents / inspect the model catalog |
| `opencode export [sessionID] [--sanitize]` / `import <file>` | session JSON out / in |
| `opencode stats --days N --models --tools --project` | usage and cost totals over the store |
| `opencode db "<sql>" --format json`; `opencode db path` | SQL over the store / print the database path |
| `opencode plugin <module>` (alias `plug`) | install a plugin specifier into project config; `--global` selects global config and `--force` replaces the pinned version |
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

The adapter verdict has landed in [opencode.md](../../internals/agents/opencode.md): OpenCode is wired as a first-class `AgentDefinition` through one RimZ-authored in-process plugin plus a read-only SQLite spend reader. Current `permission.asked` and `question.asked` events expose native prompts for RimZ to route; their reply/rejection events expose the answer and clear waiting after the user responds in OpenCode. Like Pi, the integration is one whole-file plugin that runs RimZ as its child.

| Native surface | Channel | Landed mapping |
| --- | --- | --- |
| `session.created` (no `parentID`) | lifecycle | `Registered` — worktree from `Session.directory` |
| `chat.message` hook (or a user `message.updated`) | lifecycle | `TurnStarted` — sanitized prompt labels the row; `variant` ↔ the `effort` carry-forward |
| root `session.idle` while the `plan` agent rests | awaiting-user | derived `PlanApproval` waiting row; the next prompt clears it after native review and mode switching |
| `session.idle` | lifecycle | `TurnEnded { errored: false }` |
| `session.error` | lifecycle | the error bit for the enclosing turn — a typed, in-band death certificate (`ApiError`, `MessageAbortedError`, …), Pi-grade: no transcript forensics needed |
| `tool.execute.after` (mutating tool) | lifecycle | `ToolUsed { mutates: true, edits }` — `edit` / `write` / `apply_patch` edit files; `bash` mutates only; read-only tools stay silent |
| `session.created` (with `parentID`) / child `session.idle` or `session.error` | lifecycle | `SubagentStarted` / `SubagentStopped` — the child session id keys the child, `parentID` links the parent |
| `experimental.session.compacting` → `session.compacted` | lifecycle | `Compacting` — a leading signal like Claude's `PreCompact`, cleared by the trailing event |
| `session.deleted` / `dispose` | lifecycle | normalized `session_ended`; deletion ends one root and dispose sweeps every tracked root with a bounded wait |
| `permission.asked` (legacy fallback: `permission.ask`) | awaiting-user | `waiting` — RimZ records the permission detail; OpenCode's native prompt remains responsible for the answer |
| `question.asked` | awaiting-user | `waiting` — RimZ records structured questions/options plus the joined title; OpenCode's native question UI remains responsible for the answer |
| `permission.replied` / `question.replied` / `question.rejected` | lifecycle + transcript | reconcile `waiting` to running and record the native answer choices or rejection |

- **Identity.** The plugin runs inside the server the pane's TUI embeds, so an interactive OpenCode is standalone and stampable — the in-process environment carries the pane id, and pid capture rides the spawned `rimz` child. A session exists only once created (typically at the first prompt), so OpenCode is a `registers_lazily` candidate — the Codex pattern: idle-row synthesis before the first turn, cwd-bind from `Session.directory` ([agent.md → The instance lifecycle](../../internals/agents/model.md#the-instance-lifecycle)). A session served by a detached `opencode serve`, reached over `attach`, or driven from the web UI is daemon-routed/remote — the documented remote-agent gap.
- **Context gauge.** Every assistant message carries the full token split — in-process on `message.updated`, at rest in SQLite — so the gauge rides lifecycle events with no transcript tail. The plugin resolves the context-window divisor for every model family from OpenCode's own model catalog as the model's max input tokens (`Model.limit.input`, falling back to the total `Model.limit.context`; read once per server launch via the in-process `client.config.providers()`), keyed `${providerID}/${modelID}` and stamped onto each lifecycle envelope; a Claude-family local table is the offline fallback when the catalog read is unavailable.
- **Rich context.** The plugin stamps `serverUrl` onto lifecycle envelopes, so Rust has a real out-of-band read lane to the same embedded server. `rimz opencode refresh-context` reads `GET /global/health`, `GET /config/providers`, and `GET /session/:id` after turn boundaries to fill `agent_version`, `model_display_name`, and the session title without blocking the hook. The route is display-only and read-only today; remote control (`POST /session/:id/message`, current permission/question reply routes) stays a separate subsystem.
- **Spend.** The SQLite store is the cost surface: per-message rows supply trailing-window bucketing and origin paths. The adapter opens SQLite read-only against the WAL database. Zero `cost` under a subscription login prices from tokens via [provider.md → Token pricing](../../internals/agents/providers.md#token-pricing) (the Codex rule); a positive `cost` is authoritative (the Pi rule).
- **Account probe and usage.** `auth.json` distinguishes oauth from API-key credentials per provider — enough for logged-in plus metered/unmetered on the dashboard, the same single account fact Pi's probe documents. The selected OAuth credential also feeds the out-of-band usage probe, which queries the backing provider's own quota endpoint over that token: an `anthropic` credential reuses Claude's Anthropic OAuth usage fetcher, `openai`/`openai-codex` reuse Codex's ChatGPT usage fetcher, and any other provider has no mapped endpoint and returns nothing. OpenCode introduces no endpoint of its own.

**What OpenCode cannot support:**

- **No realtime balance transport, no plan tier.** The plugin sees no provider response headers, so OpenCode surfaces no live rate-limit windows the way Claude's statusline or Codex's app-server do, and no plan tier anywhere. Its budget bars come entirely from the out-of-band OAuth usage probe over the backing-provider token ([provider.md → Per-provider mapping](../../internals/agents/providers.md#per-provider-mapping)); an API-key or `wellknown` login has no token, so it shows account identity and spend without bars. The `session.status` `retry` state is the one in-band throttling glimpse, and it is uncontracted.
- **Few permission asks by default.** Permission defaults are permissive, so permission attention engages only as far as the user's rules ask. Question-tool requests are separate and always carry their native answer UI. The closed [#19927](https://github.com/anomalyco/opencode/issues/19927) report documents why the compatibility `permission.ask` hook cannot be the current observation source.
- **Session end is observation-only.** `session.deleted` identifies a removed session directly, while the plugin converts server-scoped `dispose` into one bounded `session_ended` feed per tracked root. Pane liveness and the reaper remain the crash backstop.
