# OpenCode protocol reference

This page mirrors the OpenCode surfaces an adapter binds to: the in-process plugin API (loading, hooks, bus events), the native permission and question requests, the server HTTP API, the SQLite session store, the auth file, and the CLI and environment. It records what OpenCode ships. How RimZ maps these surfaces onto its own types, and which of them it wires, is in [adapter_opencode.md](../../internals/agents/adapter_opencode.md).

**Baseline.** OpenCode **v1.18.30** (tag [`v1.18.30`](https://github.com/anomalyco/opencode/releases/tag/v1.18.30), commit `3104c1428ec91f809e5ab86631300de41eb6952e`, released 2026-09-09), with `@opencode-ai/plugin` and `@opencode-ai/sdk` 1.18.30 from npm. Docs, source, and packages were read 2026-09-13. Source anchors below are paths under `packages/` at that tag. A few live observations were made on older builds, and each names its release inline.

**Oldest usable release.** OpenCode 1.17.19 is the first release that forwards the CLI process environment into the TUI worker (at the baseline, `opencode/src/cli/cmd/tui.ts` hands the worker a copy of `process.env`). On 1.17.18 and earlier, variables set on the `opencode` command do not reliably reach a plugin.

## Upstream sources

The docs publish unversioned while OpenCode releases several times a week, so pair a refresh with `opencode --version`. The npm packages are the version-exact schema source: `@opencode-ai/sdk` ships the legacy types at `dist/gen/types.gen.d.ts` and the current bus and API types at `dist/v2/gen/types.gen.d.ts`. Where the docs and the source disagree, this page follows the source and flags the claim; at the baseline the server, CLI, permissions, and troubleshooting pages each lag the source somewhere.

| Surface | Source |
| --- | --- |
| Plugin API (loading, context, hooks) | <https://opencode.ai/docs/plugins/> |
| Server (`opencode serve`, HTTP API, SSE) | <https://opencode.ai/docs/server/> |
| SDK (`@opencode-ai/sdk`, OpenAPI-generated client) | <https://opencode.ai/docs/sdk/> |
| Config (locations, merge order, `plugin`, `permission`) | <https://opencode.ai/docs/config/>, JSON Schema at <https://opencode.ai/config.json> |
| Permissions (keys, rules, defaults) | <https://opencode.ai/docs/permissions/> |
| Agents and subagents | <https://opencode.ai/docs/agents/> |
| CLI (commands, flags, environment variables) | <https://opencode.ai/docs/cli/> |
| Models and providers (models.dev catalog, variants) | <https://opencode.ai/docs/models/>, <https://opencode.ai/docs/providers/> |
| Zen (the curated per-token gateway) | <https://opencode.ai/docs/zen/> |
| Source repo (storage, bus, permission internals) | <https://github.com/anomalyco/opencode/tree/v1.18.30> (`sst/opencode` redirects here) |
| Typed wire schemas | npm [`@opencode-ai/plugin`](https://www.npmjs.com/package/@opencode-ai/plugin), [`@opencode-ai/sdk`](https://www.npmjs.com/package/@opencode-ai/sdk) |
| OpenAPI 3.1 spec | `GET /doc` on a running server |
| Release notes | <https://github.com/anomalyco/opencode/releases> |

## Plugins

OpenCode's only integration surface is a TypeScript plugin loaded in-process by the Bun runtime. OpenCode ships no out-of-process hook protocol and no statusline. A plugin runs inside the OpenCode server, can call OpenCode through an in-process SDK client, and can spawn child processes.

### Runtime model

The TUI runs its application server in a worker and, by default, talks to it over worker RPC with no TCP listener. The TUI starts a listener only when `--port` or `--hostname` appears on the command line before `--`, or when `--mdns` is true; it ignores the `server` config block for this decision (`opencode/src/cli/cmd/tui.ts`, the `external` check; `opencode/src/cli/network.ts`, `resolveNetworkOptionsNoConfig`). `--mini` rejects every network flag. `opencode serve`, `opencode web`, and `opencode acp` always listen.

### Loading

A plugin module exports `PluginModule = { id?: string, server: Plugin, tui?: never }`, where `Plugin` is an async factory `(input: PluginInput, options?: PluginOptions) => Promise<Hooks>` (`plugin/src/index.ts`). `node:` built-ins and npm dependencies are importable.

| Location | Scope |
| --- | --- |
| `plugin/*.{ts,js}` and `plugins/*.{ts,js}` under `~/.config/opencode/` | global |
| the same globs under every `.opencode/` from the working directory up to the worktree root, under `~/.opencode/`, and under `OPENCODE_CONFIG_DIR` | project and extra config directories |
| `plugin` array in `opencode.json[c]`: `"name"`, `"name@version"`, `"./relative.ts"`, `"/abs/path.ts"`, `"file:///abs/path.ts"`, or `[spec, { options }]` | configured |
| `opencode plugin <module>` | CLI install: writes the spec into config |

The docs name only the plural `plugins/` directories; the source scans both spellings (`opencode/src/config/plugin.ts`, the `{plugin,plugins}/*.{ts,js}` glob; directory list in `opencode/src/config/paths.ts`, `directories`). A spec is a path when it starts with `.` or `file://` or is absolute; a relative path resolves against the config file that declares it. Every other spec goes to npm through `npm-package-arg`, a bare name resolves to `name@latest`, and packages install under `$XDG_CACHE_HOME/opencode/packages/` (`opencode/src/plugin/shared.ts`, `isPathPluginSpec` and `resolvePluginTarget`; `core/src/npm.ts`). `--pure` starts OpenCode without external plugins.

### Plugin input

| `PluginInput` field | Carries |
| --- | --- |
| `client` | an `@opencode-ai/sdk` client. With no listener it is built on the in-process `fetch` of the application server, so it works in every launch mode; its `baseUrl` is captured once at plugin init (`opencode/src/plugin/index.ts`) |
| `serverUrl` | the listening server's URL, or the fallback `http://localhost:4096` when no listener exists. The fallback is not a discovery address and may belong to an unrelated process |
| `project` / `directory` / `worktree` | project identity, working directory, git worktree root |
| `$` | Bun shell for spawning children |
| `experimental_workspace` | `register(type, adapter)` for workspace adapters |

## Plugin hooks

These are the `Hooks` members that observe sessions, verbatim in shape from `@opencode-ai/plugin` 1.18.30:

```ts
dispose?: () => Promise<void>
event?: (input: { event: Event }) => Promise<void>
"chat.message"?: (input: { sessionID, agent?, model?: { providerID, modelID }, messageID?, variant? },
                  output: { message: UserMessage, parts: Part[] }) => Promise<void>
"tool.execute.before"?: (input: { tool, sessionID, callID }, output: { args }) => Promise<void>
"tool.execute.after"?: (input: { tool, sessionID, callID, args }, output: { title, output, metadata }) => Promise<void>
"experimental.session.compacting"?: (input: { sessionID }, output: { context: string[], prompt? }) => Promise<void>
"permission.ask"?: (input: Permission, output: { status: "ask" | "deny" | "allow" }) => Promise<void>
```

| Hook | Behavior |
| --- | --- |
| `event` | receives every bus event whose location directory matches the plugin's directory, as `{ id, type, properties }`, with no type filter (`opencode/src/plugin/index.ts`) |
| `chat.message` | fires once per user prompt with the typed message and parts; `variant` is the model's reasoning variant (`"high"`, `"xhigh"`, and so on) |
| `tool.execute.before` / `tool.execute.after` | bracket each tool call; `before` may mutate `args` and `after` may rewrite `output` |
| `experimental.session.compacting` | fires when compaction starts; a plugin may add `context` lines or replace the compaction `prompt`. `session.compacted` is the trailing event |
| `dispose` | fires when the owning server shuts down |
| `permission.ask` | typed, but nothing calls it at the baseline: no `trigger("permission.ask", …)` exists in `opencode/src`, `core/src`, or `server/src`. Observe `permission.asked` instead ([Permission and question requests](#permission-and-question-requests)). [anomalyco/opencode#19927](https://github.com/anomalyco/opencode/issues/19927), which reported commands bypassing this hook, is closed as not planned |

The rest of the hook catalog, indexed:

| Hook | Purpose |
| --- | --- |
| `config` | read the merged config at startup |
| `tool` | register custom tools |
| `tool.definition` | rewrite a tool's description and parameters |
| `auth` | custom provider login flows (`oauth` and `api` methods with prompts) |
| `provider` | provider hook (`ProviderHook`) for a plugin-defined provider |
| `chat.params` | set temperature, `topP`, `topK`, `maxOutputTokens`, and provider options per request |
| `chat.headers` | add request headers per request |
| `command.execute.before` | rewrite the parts a slash command sends |
| `shell.env` | add environment variables to shell tool children |
| `experimental.chat.messages.transform` | rewrite the message history sent to the model |
| `experimental.chat.system.transform` | rewrite system prompt lines |
| `experimental.provider.small_model` | choose a provider's small model when config `small_model` is unset (`opencode/src/provider/provider.ts`, `getSmallModel`) |
| `experimental.compaction.autocontinue` | decide whether a session continues after compaction |
| `experimental.text.complete` | rewrite a completed text part |

## Bus events

The plugin `event` hook and the `GET /event` stream carry one tagged union, `{ id, type, properties }`. The catalog below follows the `@opencode-ai/sdk/v2` types and the runtime publishers at the baseline. The legacy `@opencode-ai/sdk` `Event` export lags the runtime: it names the old permission event `permission.updated`, omits the question events and `message.part.delta`, and still lists `lsp.client.diagnostics`. Parse the event boundary tolerantly.

### Session and message events

| Event | Properties | Carries |
| --- | --- | --- |
| `session.created` | `sessionID`, `info: Session` | a new session; a child session (subagent) carries `info.parentID` |
| `session.updated` | `sessionID`, `info: Session` | title, `time.compacting`, share and revert state |
| `session.idle` | `sessionID` | the session finished its work and is waiting for input |
| `session.status` | `sessionID`, `status` | `{type:"idle"}`, `{type:"busy"}`, or `{type:"retry", attempt, message, next, action?}`, where `action` is a provider hint (`reason`, `provider`, `title`, `message`, `label`, `link?`) |
| `session.error` | `sessionID?`, `error?` | a serialized error ([Errors](#errors)) |
| `session.compacted` | `sessionID` | compaction completed |
| `session.deleted` | `sessionID`, `info: Session` | session removed |
| `session.diff` | `sessionID`, `diff: SnapshotFileDiff[]` | per-session diff stats |
| `message.updated` | `sessionID`, `info: Message` | a user message, or an assistant message with `tokens`, `cost`, `modelID`, `providerID`, `agent`, `variant?`, `finish?`, `error?` |
| `message.part.updated` | `sessionID`, `part: Part`, `time` | a part's full state: tool parts step through `pending`, `running`, `completed`, or `error`; `step-finish` parts carry per-step `tokens` and `cost` |
| `message.part.delta` | `sessionID`, `messageID`, `partID`, `field`, `delta` | a streamed text increment for one part field (`schema/src/v1/session.ts`) |
| `todo.updated` | `sessionID`, `todos: Todo[]` | the todo list; each item has `content`, `status`, `priority` |
| `file.edited` | `file` | a file write, with no session id |

The request events `permission.asked`, `permission.replied`, `question.asked`, `question.replied`, and `question.rejected` are in [Permission and question requests](#permission-and-question-requests).

OpenCode 1.18.2 emits an aborted assistant `message.updated` with `input`, `output`, `cache.read`, and `cache.write` all zero and `tokens.total` absent (live-verified on 1.18.2). That shape stands for a measurement the stream never delivered, not for a call that used zero tokens.

### Event index

| Family | Events |
| --- | --- |
| messages | `message.removed`, `message.part.removed` |
| commands and files | `command.executed`, `file.watcher.updated` |
| installation | `installation.updated`, `installation.update-available` |
| LSP and MCP | `lsp.updated`, `mcp.tools.changed`, `mcp.browser.open.failed` |
| terminals | `pty.created`, `pty.updated`, `pty.exited`, `pty.deleted` |
| server | `server.connected`, `server.instance.disposed`, `global.disposed`, `catalog.updated`; `/event` also sends `server.heartbeat` every 10 seconds |
| TUI control | `tui.prompt.append`, `tui.command.execute`, `tui.toast.show`, `tui.session.select` |
| projects and VCS | `project.updated`, `project.directories.updated`, `vcs.branch.updated`, `workspace.ready`, `workspace.failed`, `workspace.status`, `worktree.ready`, `worktree.failed` |
| v2 requests | `permission.v2.asked`, `permission.v2.replied`, `question.v2.asked`, `question.v2.replied`, `question.v2.rejected`: published by the core `PermissionV2` and `QuestionV2` services for core tools and the `/api` routes. The default session loop uses the v1 permission service and its `permission.asked` event (`opencode/src/session/processor.ts`) |
| durable session events | `session.next.*` (prompt, step, text, reasoning, tool, shell, compaction, revert, model and agent switches), and versioned sync events (`session.created.1`, `message.updated.1`, …) that `/global/event` wraps as `{ type: "sync", syncEvent: { type, id, seq, aggregateID, data } }` |

### Payload shapes

```ts
type Session = {
  id: string                      // "ses_…"
  slug: string
  projectID: string
  workspaceID?: string
  directory: string               // the session's working directory
  path?: string
  parentID?: string               // set on a subagent's child session
  title: string
  version: string                 // the OpenCode version that wrote the session
  agent?: string
  model?: { id: string, providerID: string, variant?: string }
  cost?: number
  tokens?: { input, output, reasoning, cache: { read, write } }
  time: { created: number, updated: number, compacting?: number, archived?: number }   // epoch ms
  summary?: { additions, deletions, files, diffs? }
  share?: { url }, revert?: { messageID, partID?, snapshot?, diff? }, permission?, metadata?
}

type AssistantMessage = {
  id: string                      // "msg_…"
  sessionID: string
  parentID: string                // the user message this one answers
  role: "assistant"
  time: { created: number, completed?: number }
  error?: { name: string, data: Record<string, unknown> }
  modelID: string, providerID: string
  mode: string, agent: string, variant?: string
  path: { cwd: string, root: string }
  summary?: boolean               // true on a compaction summary message
  cost: number
  tokens: { total?, input, output, reasoning, cache: { read, write } }
  structured?: unknown
  finish?: string                 // "stop", …
}
```

The legacy `@opencode-ai/sdk` `AssistantMessage` omits `agent`, `variant`, and `tokens.total`; the v2 type and the wire carry them.

### Errors

A serialized error is `{ name, data }` (`core/src/util/error.ts`). The union on `session.error` and assistant `error` has these wire names: `ProviderAuthError`, `UnknownError`, `MessageOutputLengthError`, `MessageAbortedError`, `StructuredOutputError`, `ContextOverflowError`, `ContentFilterError`, and `APIError` (the generated type is `ApiError`, but the `name` string is `APIError`). Every variant carries its human-readable text in `data.message` except `MessageOutputLengthError`, whose `data` is empty (`schema/src/v1/session.ts`). `APIError` data also carries `statusCode?`, `isRetryable`, `responseHeaders?`, `responseBody?`, and `metadata?`.

## Permission and question requests

OpenCode asks the user in its own UI and publishes each open request on the bus. The permission service creates an awaited request, publishes `permission.asked`, and publishes `permission.replied` when the user answers (`opencode/src/permission/index.ts`). The question tool does the same with `question.asked`, then `question.replied` or `question.rejected` (`opencode/src/question/index.ts`).

```ts
type PermissionRequest = {        // permission.asked properties
  id: string
  sessionID: string
  permission: string              // "bash", "edit", "webfetch", …
  patterns: string[]              // matched command, path, or URL patterns
  metadata: { [key: string]: unknown }
  always: string[]                // patterns an "always" reply saves for the session
  tool?: { messageID: string, callID: string }
}

type QuestionRequest = {          // question.asked properties
  id: string
  sessionID: string
  questions: Array<{
    question: string              // the full question
    header: string                // short label, max 30 chars
    options: Array<{ label: string, description: string }>
    multiple?: boolean
    custom?: boolean
  }>
  tool?: { messageID: string, callID: string }
}
```

| Event | Properties |
| --- | --- |
| `permission.replied` | `sessionID`, `requestID`, `reply: "once" \| "always" \| "reject"` |
| `question.replied` | `sessionID`, `requestID`, `answers: string[][]` (one answer array per question) |
| `question.rejected` | `sessionID`, `requestID` |

The same requests are reachable over HTTP: `GET /permission` and `POST /permission/:requestID/reply`, and `GET /question`, `POST /question/:requestID/reply`, and `POST /question/:requestID/reject`. The server docs list none of these routes; the v2 SDK client defines them. The older `POST /session/:id/permissions/:permissionID` remains in the API.

### Permission config

Most tools run without asking. Rules live under `permission` in config, per agent, and in `OPENCODE_PERMISSION`. A rule is a single action (`allow`, `ask`, `deny`) or a pattern-to-action map, and the last matching rule wins. The typed keys are `read`, `edit`, `glob`, `grep`, `list`, `bash`, `task`, `external_directory`, `todowrite`, `question`, `webfetch`, `websearch`, `lsp`, `doom_loop`, and `skill`; any other tool name or wildcard is accepted too. The permissions docs omit `list` and `todowrite`, which the config schema carries.

The defaults at the baseline (`opencode/src/agent/agent.ts`):

| Key | Default |
| --- | --- |
| `*` | `allow` |
| `doom_loop` | `ask` |
| `external_directory` | `ask`, except OpenCode's temp, truncation, skill, and reference directories |
| `read` | `allow`, except `*.env` and `*.env.*`, which `ask`; `*.env.example` is `allow` |
| `question`, `plan_enter`, `plan_exit` | `deny` |

Built-in agents layer overrides between the defaults and the user's rules. `build` allows `question` and `plan_enter`. `plan` allows `question` and `plan_exit`, denies `edit` except plan files under `.opencode/plans/` and the data directory's `plans/`, and denies the `general` subagent through `task`. The `explore` subagent denies everything except `read`, `grep`, `glob`, `list`, `bash`, `webfetch`, and `websearch`. The permissions docs say `.env` reads are denied by default; the source asks.

`OPENCODE_PERMISSION` holds a JSON permission object that is deep-merged over the merged config's `permission`, so it overrides every config file; invalid JSON is logged and skipped (`opencode/src/config/config.ts`).

## Server HTTP API

A listener exists only in the launch modes described in [Runtime model](#runtime-model). Server flags are `--port` (default `0`, a random port), `--hostname` (default `127.0.0.1`), `--mdns` (defaults the hostname to `0.0.0.0`), `--mdns-domain` (default `opencode.local`), and `--cors`. The server docs describe `127.0.0.1:4096` as the default; the 1.18.30 CLI defaults `--port` to `0`. HTTP basic auth is enabled by `OPENCODE_SERVER_PASSWORD`, with `OPENCODE_SERVER_USERNAME` defaulting to `opencode`.

| Route | Returns |
| --- | --- |
| `GET /global/health` | `{"healthy":true,"version":"<installation version>"}` (shape live-verified on 1.17.9; unchanged in the 1.18.30 source) |
| `GET /event` | SSE of the bus events above for the current instance directory and workspace: opens with `server.connected`, sends `server.heartbeat` every 10 seconds, ends on `server.instance.disposed` |
| `GET /global/event` | SSE across every directory, each item `{ directory, project, workspace, payload }`, with durable events as `payload.type: "sync"` |
| `GET /api/event` | the v2 native stream, `{ id, type, data, … }`, with an SSE comment heartbeat every 15 seconds |
| `GET /api/session/:sessionID/event` | durable events for one session, replayed from `?after=` |
| `GET /config/providers` | `{ providers, default }`; each provider's `models` map carries display `name` and `limit: { context, input?, output }` |
| `GET /session/:id` | session metadata, the `Session` shape above |
| `GET /session/:id/message`, `POST /session/:id/message` | message history and prompting |
| `GET /permission`, `GET /question` and their reply routes | open native requests ([Permission and question requests](#permission-and-question-requests)) |
| `GET /doc` | the OpenAPI 3.1 spec; the version-exact route catalog |

Every route is also a method on the SDK client (`createOpencodeClient` from `@opencode-ai/sdk`), including the in-process `PluginInput.client`. `opencode acp` exposes the agent over the Agent Client Protocol.

## Session store

OpenCode keeps sessions in one SQLite database in WAL mode under `$XDG_DATA_HOME/opencode` (default `~/.local/share/opencode`), opened with `synchronous=NORMAL`, `busy_timeout=5000`, and `foreign_keys=ON` (`core/src/database/database.ts`). The docs' troubleshooting page still describes a JSON `storage/` tree; the source writes only SQLite.

The file name is chosen in this order (`core/src/database/database.ts`, `path`):

1. `OPENCODE_DB`: `:memory:` or an absolute path is used as given, and a relative value is joined to the data directory.
2. `opencode.db` when the build channel is `latest`, `beta`, or `prod`, or when `OPENCODE_DISABLE_CHANNEL_DB` is `1` or `true`.
3. Otherwise `opencode-<channel>.db`, with every character outside `[a-zA-Z0-9._-]` replaced by `-`.

The channel is a build-time constant that defaults to `local` (`core/src/installation/version.ts`), so a source or desktop build writes its own channel database while an `opencode.db` from a release build can sit beside it. `opencode db path` prints the path in use.

| Table | Columns |
| --- | --- |
| `session` | `id` (`ses_…`), `project_id`, `workspace_id`, `parent_id` (set on a subagent's child session), `slug`, `directory`, `path`, `title`, `version` (the writing OpenCode version), `agent`, `model` (JSON `{id, providerID, variant}`), `cost`, `tokens_input`, `tokens_output`, `tokens_reasoning`, `tokens_cache_read`, `tokens_cache_write`, `time_created`, `time_updated`, `time_compacting`, `time_archived` (epoch ms), `summary_additions`, `summary_deletions`, `summary_files`, `summary_diffs`, `share_url`, `permission`, `revert`, `metadata` |
| `message` | `id` (`msg_…`), `session_id`, `time_created`, `time_updated`, `data` (the message JSON shown below) |
| `part` | `id`, `message_id`, `session_id`, `time_created`, `time_updated`, `data` (JSON `Part`: `step-finish` parts carry per-step `tokens` and `cost`, `tool` parts carry the call state) |
| `session_message` | durable ordered session events: `id`, `session_id`, `type`, `seq`, timestamps, `data`; `(session_id, seq)` is unique |
| `session_input`, `session_context_epoch` | admitted and pending prompts; the active context baseline |
| the rest | `project`, `project_directory`, `workspace`, `permission` (per-project saved rules), `todo`, `session_share`, `event`, `event_sequence`, `credential`, `account`, `account_state`, `control_account`, `data_migration` |

The table and column list is from `core/src/database/schema.gen.ts` and `core/src/session/sql.ts`. OpenCode inserts an assistant `message` row when streaming starts and updates its `data` in place as tokens and cost arrive.

An assistant `message.data` blob (live row, paths trimmed; shape unchanged through 1.18.30):

```jsonc
{"parentID":"msg_…","role":"assistant","mode":"build","agent":"build","variant":"xhigh",
 "path":{"cwd":"…","root":"…"},
 "cost":0,
 "tokens":{"total":9664,"input":3481,"output":8,"reasoning":31,"cache":{"write":0,"read":6144}},
 "modelID":"gpt-5.5","providerID":"openai",
 "time":{"created":1780590149011,"completed":1780590154568},
 "finish":"stop"}
```

Three facts govern any read of these rows:

- **The token split is disjoint.** `input` excludes cached tokens, and `total` is `input + output + reasoning + cache.read + cache.write` (9664 in the row above). The prompt size of a call is `input + cache.read + cache.write`.
- **Zero `cost` means unpriced.** The row above is an OAuth subscription login with real token counts and `cost: 0`; a subscription login carries no per-token price. A positive `cost` is OpenCode's own figure.
- **No row carries a context window.** The window comes from the model catalog: `limit.input` where the model lists a separate input cap, otherwise `limit.context` (`GET /config/providers`). Session totals are precomputed on the `session` row.

Three CLI commands read the store without SQL of your own: `opencode export [sessionID]` prints a session as JSON (`--sanitize` redacts transcript and file data), `opencode stats` totals usage and cost, and `opencode db "<sql>" --format json` runs a query. Each starts the full application, so they suit probes more than frequent reads.

## Auth file

`$XDG_DATA_HOME/opencode/auth.json` (written with mode `0600`) maps a provider id to one credential, managed by `opencode providers login` and `logout` (`opencode/src/auth/index.ts`). `OPENCODE_AUTH_CONTENT`, when set, replaces the file's contents on read. A live file with OAuth logins looks like this:

```jsonc
{
  "openai":    { "type": "oauth", "access": "…", "refresh": "…", "expires": <epoch ms>, "accountId": "…" },
  "anthropic": { "type": "oauth", "access": "…", "refresh": "…", "expires": <epoch ms> },   // Claude Pro/Max login
  "deepseek":  { "type": "api", "key": "sk-…", "metadata": { … } }                          // any API-key provider
}
```

| `type` | Fields |
| --- | --- |
| `oauth` | `access`, `refresh`, `expires` (epoch ms), `accountId?`, `enterpriseUrl?` |
| `api` | `key`, `metadata?` |
| `wellknown` | `key`, `token` |

The v2 SDK `OAuth` type carries `accountId`; the legacy type omits it. MCP server OAuth tokens live apart in `mcp-auth.json` in the same directory, also mode `0600` (`opencode/src/mcp/auth.ts`).

The file holds credentials only. OpenCode publishes no rate-limit windows, quota, or plan tier: no route in the v2 SDK returns them, and the plugin never sees provider response headers. Provider throttling reaches the bus only as a `session.status` `retry` state and as `APIError` data. OpenCode is multi-provider: one session can use any configured provider, and a Zen (`opencode`) login bills per token.

## CLI and environment

The official installer (`curl -fsSL https://opencode.ai/install | bash`) puts the binary at `~/.opencode/bin/opencode` and appends that directory to `PATH` in a shell rc file, so a non-login or daemon environment often has OpenCode installed but not on `PATH`. The tables below follow `opencode --help` on 1.18.30.

### Commands

| Command | Purpose |
| --- | --- |
| `opencode [project]` | the TUI, started in `project` (resolved against `$PWD`) or the current directory |
| `opencode run [message..]` | run one prompt headless ([run flags](#run-flags)) |
| `opencode serve` / `web` / `acp` | headless server / server plus browser UI / Agent Client Protocol server; each takes the server flags |
| `opencode attach <url>` | point a TUI at a running server (`--dir`, `-c`, `-s`, `--fork`, `-p`/`--password`, `-u`/`--username`, `--mini`) |
| `opencode session list` / `session delete <sessionID>` | list sessions / delete one |
| `opencode export [sessionID]` / `import <file>` | session JSON out (`--sanitize`) / in, from a file or share URL |
| `opencode stats` | usage and cost totals (`--days`, `--tools`, `--models`, `--project`) |
| `opencode db [query]` / `db path` | interactive sqlite3 shell or one query (`--format tsv` default, or `json`) / print the database path |
| `opencode plugin <module>` (alias `plug`) | install a plugin and add it to project config under `.opencode/` (`-g`/`--global` for global config, `-f`/`--force` to replace a pinned version) |
| `opencode providers` (alias `auth`) | `list`, `login [url]`, `logout [provider]` against `auth.json`; the CLI docs name only `opencode auth` |
| `opencode models [provider]` | list the model catalog (`--verbose`, `--refresh` from models.dev) |
| `opencode agent create` / `agent list` | manage agents |
| `opencode mcp`, `debug`, `github`, `pr <number>`, `upgrade [target]`, `uninstall`, `completion` | MCP servers, diagnostics, GitHub agent, PR checkout, self-update, removal, shell completion |

### TUI flags

| Flag | Meaning |
| --- | --- |
| `-v`, `--version` | print the installed version |
| `-m`, `--model <provider/model>` | select the model |
| `--agent <name>` | select the primary agent; `--agent plan` starts in the built-in plan agent (live-verified on 1.17.20) |
| `-c`, `--continue` | continue the newest session |
| `-s`, `--session <id>` | continue a session by id |
| `--fork` | fork the session being continued (with `-c` or `-s`) |
| `--prompt <text>` | pre-fill the prompt and submit it once the session and model are ready; piped stdin is prepended |
| `--auto` | auto-approve permissions that are not explicitly denied (live-verified on 1.17.20) |
| `--mini` | the minimal interactive interface (`--no-replay`, `--replay-limit <n>`); rejects server flags |
| `--port`, `--hostname`, `--mdns`, `--mdns-domain`, `--cors` | start a TCP listener ([Runtime model](#runtime-model)) |
| `--pure` | run without external plugins |
| `--print-logs`, `--log-level <DEBUG\|INFO\|WARN\|ERROR>` | logs to stderr |

The TUI does not read arguments after `--`. The CLI parses them into a separate list that only `run` and `mcp` consume, so `opencode -- "fix the bug"` starts the TUI in the current directory and drops the text (`opencode/src/index.ts`, `populate--`; `opencode/src/cli/cmd/tui.ts`). Pass an initial prompt with `--prompt`.

### Run flags

`opencode run` takes `-c`, `-s`, `--fork`, `-m`, `--agent`, and `--auto` as the TUI does, plus:

| Flag | Meaning |
| --- | --- |
| `--format default\|json` | formatted output, or raw JSON events |
| `--command <name>` | run a slash command, with the message as its arguments |
| `-f`, `--file` | attach files |
| `--title` | session title |
| `--share` | share the session |
| `--attach <url>`, `-p`, `-u`, `--dir` | run against a running server, with basic auth and a remote directory |
| `--port` | port for the local server |
| `--variant <level>` | provider-specific reasoning effort (`high`, `max`, `minimal`, …); there is no TUI equivalent flag |
| `--thinking` | show thinking blocks |
| `-i`, `--interactive` | direct interactive split-footer mode |

### Environment variables

| Variable | Meaning |
| --- | --- |
| `XDG_DATA_HOME` / `XDG_CONFIG_HOME` / `XDG_CACHE_HOME` / `XDG_STATE_HOME` | relocate the data root (database, `auth.json`, logs), the config root (config, plugins), caches (npm plugins), and state |
| `OPENCODE_CONFIG` | explicit config file path |
| `OPENCODE_CONFIG_DIR` | an extra config directory, scanned for plugins too |
| `OPENCODE_CONFIG_CONTENT` | inline config JSON |
| `OPENCODE_PERMISSION` | JSON permission rules merged over all config ([Permission config](#permission-config)) |
| `OPENCODE_DB` / `OPENCODE_DISABLE_CHANNEL_DB` | database path / force `opencode.db` ([Session store](#session-store)); neither is in the CLI docs |
| `OPENCODE_AUTH_CONTENT` | replaces `auth.json` contents on read |
| `OPENCODE_SERVER_USERNAME` / `OPENCODE_SERVER_PASSWORD` | HTTP basic auth on the server |
| `OPENCODE=1`, `OPENCODE_PID`, `AGENT=1` | set by OpenCode on its own `process.env` at startup, for every command, so every child (shell tools, plugin-spawned processes) inherits them (`opencode/src/index.ts`). They are absent from the process's initial environment, so `/proc/<pid>/environ` of the OpenCode process does not show them |

The CLI docs list more variables (autoupdate, autocompact, LSP download, Claude Code compatibility, and an experimental table); see <https://opencode.ai/docs/cli/#environment-variables>.
