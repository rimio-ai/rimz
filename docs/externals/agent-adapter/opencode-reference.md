# OpenCode protocol reference

This page mirrors the OpenCode surfaces an adapter binds to: plugins, native requests, events, HTTP, session storage, credentials, and the CLI environment. The reference is OpenCode 2 only. How RimZ maps upstream surfaces onto its own types, and which of them it wires, is in [adapter_opencode.md](../../internals/agents/adapter_opencode.md). An upstream refresh does not establish RimZ adapter compatibility with a new major version.

**Current baseline.** OpenCode **2.0.27**, tag [`v2.0.27`](https://github.com/anomalyco/opencode/tree/v2.0.27), commit `1f4d07a4a519185955ae8ab2ea3607805509e386`, with npm `@opencode/cli`, `@opencode/plugin`, and `@opencode/client` 2.0.27. The CLI package was published 2026-10-12; source, registry metadata, and docs were read 2026-10-12. Check the package name as well as the version when refreshing: the legacy `opencode-ai`, `@opencode-ai/plugin`, and `@opencode-ai/sdk` packages do not describe these contracts.

## OpenCode 2 sources

Source paths in the OpenCode 2 sections are under `packages/` at `v2.0.27`. Follow versioned source when the live documentation or JSON Schema describes a different release.

| Surface | Source |
| --- | --- |
| Installation and migration | [OpenCode 2 docs](https://opencode.ai/v2/docs) |
| Plugin configuration | [Plugins](https://opencode.ai/v2/docs/plugins), [`schema/src/config/plugin.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/schema/src/config/plugin.ts) |
| Promise and Effect plugin APIs | [`plugin/src/promise/plugin.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/plugin/src/promise/plugin.ts), [`plugin/src/effect/index.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/plugin/src/effect/index.ts) |
| Client | [`client/src/promise/index.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/client/src/promise/index.ts), npm [`@opencode/client`](https://www.npmjs.com/package/@opencode/client/v/2.0.27) |
| HTTP contract | [`protocol/src/groups/`](https://github.com/anomalyco/opencode/tree/v2.0.27/packages/protocol/src/groups), `GET /openapi.json` on a running server |
| Wire schemas and event catalog | [`schema/src/`](https://github.com/anomalyco/opencode/tree/v2.0.27/packages/schema/src), `event-manifest.ts` and `session-event.ts` |
| Database | [`core/src/database/schema.gen.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/core/src/database/schema.gen.ts) |
| CLI command and flag catalog | [`cli/src/commands/commands.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/cli/src/commands/commands.ts) |

## OpenCode 2 plugins and runtime

The CLI normally connects to a persistent background server, starting it when needed. `--server <url>` selects an existing server; `--standalone` starts a private child server and cannot be combined with `--server`. Server plugins execute in the server process, not in the TUI process. See [`cli/src/services/server-connection.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/cli/src/services/server-connection.ts), `resolve`, and [`standalone.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/cli/src/services/standalone.ts), `command`.

Config uses plural `plugins`, with string specs or `{ package, options }` entries. Relative paths resolve from the declaring config. Automatic discovery scans both `plugin/` and `plugins/` under discovered config directories, accepting direct `.ts`/`.js` files and immediate package directories (`core/src/plugin/source-directory.ts`, `discover`). The live docs name only plural directories and show explicitly configured file paths, but the baseline's `core/src/config/plugin/source.ts`, `scan`, warns and skips configured paths that are files: use a directory for an explicit local plugin, or a discovered file. TUI-only plugins are configured separately in `cli.json`. See [plugin configuration](https://opencode.ai/v2/docs/plugins), [`source-directory.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/core/src/plugin/source-directory.ts), and [`source.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/core/src/config/plugin/source.ts).

The Promise API exports `Plugin.define` from `@opencode/plugin`; `@opencode/plugin/effect` is the Effect entry point. A module must default-export a definition with `id` and either `setup` or `effect` ([`core/src/plugin/module.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/core/src/plugin/module.ts), `Module`). A Promise plugin is `{ id: string, setup(context): Promise<Cleanup | void> | Cleanup | void }`, with `Cleanup = () => Promise<void> | void`. `Context` carries `app`, `location`, `options`, and domains such as `event`, `session`, `tool`, `permission`, `integration`, and `storage`. There is no `PluginInput.client`, `serverUrl`, or Bun shell helper in this context (`plugin/src/promise/plugin.ts`, `Plugin` and `Context`).

Hooks are registered through a domain, rather than returned as string-keyed members. A registration has `dispose(): Promise<void>` (`plugin/src/promise/registration.ts`). The adapter-relevant catalog is:

| Registration | Current shape |
| --- | --- |
| `context.session.hook("prompt", callback)` | `sessionID`, `messageID`, mutable `prompt`, `metadata?`, `delivery` |
| `context.session.hook("context", callback)` | `sessionID`, `agent`, `model`, mutable `system`, `messages`, `options`, `tools` |
| `context.session.hook("compaction", callback)` | the context shape plus optional `result` to replace model compaction |
| `context.session.hook("title" / "generate", callback)` | auxiliary session requests; title can supply a `result` |
| `context.session.hook("model.request" / "http.request" / "http.response", callback)` | session/model identity and `kind: "primary" \| "compaction" \| "title" \| "generate"`; the HTTP hooks expose the actual `Request` or `Response` |
| `context.session.hook("retry", callback)` | error, attempt, and mutable retry decision |
| `context.tool.hook("execute.before" / "execute.after", callback)` | tool, session, agent, message and call identity; mutable input before execution, completed result or error afterward |
| `context.permission.hook("evaluate", callback)` | action, resources, source, and mutable permission effect/message |
| `context.shell.hook("create.before", callback)` | mutable command, working directory, timeout, shell, and environment |
| `context.event.subscribe(...)` | the native async-iterable event subscription API |

The definitions are in [`plugin/src/promise/session.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/plugin/src/promise/session.ts), [`tool.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/plugin/src/promise/tool.ts), [`permission.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/plugin/src/promise/permission.ts), [`shell.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/plugin/src/promise/shell.ts), and [`event.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/plugin/src/promise/event.ts). The `http.response` hook exposes provider response headers.

## OpenCode 2 events and HTTP

Native events use `{ id, type, created, data, location?, metadata? }`, not `{ type, properties }`. Durable events additionally carry `durable: { aggregateID, seq, version }` (`schema/src/event.ts`, `Payload`). The stream-opening `server.connected` is an exception without `created` (`server/src/handlers/event.ts`). The session event family now uses `session.execution.*`, `session.step.*`, `session.text.*`, `session.reasoning.*`, `session.tool.*`, and other `session.*` types. `session.created.data` carries creation fields directly, not `info: Session`. The exact inventory is [`schema/src/session-event.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/schema/src/session-event.ts).

`session.status` still reports `idle`, `busy`, or `retry`; `session.idle` is marked deprecated in [`schema/src/session-status-event.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/schema/src/session-status-event.ts). Completion outcomes also have durable `session.execution.succeeded`, `session.execution.failed`, and `session.execution.interrupted` events.

Per-step usage arrives in `session.step.ended.data`, including `assistantMessageID`, `cost`, `tokens`, and `finish`. A native assistant timeline message uses `type: "assistant"`, `agent`, a `model` reference, `content`, and `time: { created, streamed?, completed? }`; `cost`, `tokens`, `error`, and `retry` may be absent. Its structured error is `{ type, message, status?, response?: { body } }`. See [`schema/src/session-message.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/schema/src/session-message.ts), `Assistant`, and [`session-error.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/schema/src/session-error.ts), `Error`.

| Route | Contract |
| --- | --- |
| `GET /api/info` | server `version`, `pid`, `urls`, `paths.tmp`, optional capabilities; replaces the old health response |
| `GET /api/event` | volatile SSE across server locations, including plugin `rpc.*` events; disconnections lose events and slow consumers can overflow |
| `GET /api/experimental/session/:sessionID/log` | durable session SSE; `after` is an exclusive aggregate sequence, `follow=true` continues live; `log.synced` marks the captured replay watermark |
| `GET /api/session`, `POST /api/session`, `GET /api/session/:sessionID` | list, create, and retrieve native sessions |
| `GET /api/session/:sessionID/message` | paginated timeline, `{ data, cursor }` |
| `GET /api/model` | native model catalog |
| `POST /api/session/:sessionID/prompt` | submit a prompt |
| `GET /api/session/:sessionID/context` | active context after the last compaction |
| `POST /api/session/:sessionID/interrupt` | interrupt execution |
| `GET /openapi.json` | the current HTTP schema, not the old `/doc` route |

The route definitions are in `protocol/src/groups/server.ts`, `event.ts`, `session.ts`, `message.ts`, and `model.ts`; the OpenAPI mount is in [`server/src/routes.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/server/src/routes.ts). `/api/event` sends SSE comment heartbeats every 15 seconds (`server/src/handlers/event.ts`). A durable log cursor comes from `durable.seq` or `log.synced.seq`, never an event ID or a sequence from another aggregate (`schema/src/event-log.ts`). The Promise client is constructed with `OpenCode.make` from `@opencode/client`, not the legacy `createOpencodeClient` (`client/src/promise/index.ts`).

Plugin subscriptions include the following adapter-relevant events ([`plugin/src/promise/event.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/plugin/src/promise/event.ts), [session event catalog](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/schema/src/session-event.ts)):

| Event | Relevant data |
| --- | --- |
| `session.created` | `sessionID`, `parentID?`, `location.directory`, `title?`, `version`, `agent?`, `model?` |
| `session.deleted` | `sessionID` |
| `session.execution.started` / `succeeded` / `failed` / `interrupted` | `sessionID`; failure has `error`, interruption has `reason` |
| `session.step.started` | `sessionID`, `assistantMessageID`, `agent`, `model` |
| `session.step.ended` | `sessionID`, `assistantMessageID`, `finish`, `cost`, `tokens` |
| `session.renamed` | `sessionID`, `title` |
| `session.model.selected` / `session.agent.selected` | `sessionID`, `model` / `agent` |
| `session.compaction.started` / `ended` / `failed` | `sessionID` |
| `form.created` / `replied` / `cancelled` | form and answer shapes below |
| `permission.asked` / `replied` | request and reply shapes below |

`ctx.session.get({sessionID})` returns session info directly, including title, agent, model, and location but not the creation version. `ctx.model.list()` returns `{data: ModelInfo[]}`; model entries carry `id`, `providerID`, `name`, and `limit: {context, input?, output}`. The context hook's mutable system array contains text parts, not strings: append `{type: "text", text}` ([session API](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/plugin/src/promise/session.ts), [model API](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/plugin/src/promise/model.ts)).

## OpenCode 2 native requests

`permission.asked.data` is `{ id, sessionID, action, resources, save?, metadata?, source?, message? }`. A tool source is `{ type: "tool", messageID, id }`. `permission.replied.data` remains `{ sessionID, requestID, reply: "once" | "always" | "reject" }`. Config uses plural `permissions`, an ordered array of `{ action, resource, effect: "allow" | "deny" | "ask" }` (`schema/src/permission.ts`, `Request`, `Ruleset`; `schema/src/config.ts`, `Info`).

Pending permissions are listed at `GET /api/permission/request` for a location or `GET /api/session/:sessionID/permission` for a session. Reply with `POST /api/session/:sessionID/permission/:requestID/reply`, body `{ decision: "once" | "always" | "reject", message? }`. Notice that the reply request calls the field `decision`, while the event calls it `reply` ([`protocol/src/groups/permission.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/protocol/src/groups/permission.ts)).

An explicit configured deny returns before the `evaluate` plugin hook. An `always` answer persists the request's `save` resources per project and reevaluates eligible pending requests; it is not merely a session-local approval ([`core/src/permission.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/core/src/permission.ts), `evaluateInput` and `reply`).

The native form protocol is `form.created` (`data.form`), `form.replied` (`data: { id, sessionID, answer }`), and `form.cancelled` (`data: { id, sessionID }`). A form has `id`, `sessionID`, `title`, `fields`, and optional metadata. Field kinds are `string`, `number`, `integer`, `boolean`, `multiselect`, and `external`; answers are a key-to-value object. See [`schema/src/form.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/schema/src/form.ts).

Use `GET /api/session/:sessionID/form` to list pending forms, `POST /api/session/:sessionID/form/:formID/reply` with `{ answer }` to answer, and `DELETE /api/session/:sessionID/form/:formID` to cancel (`protocol/src/groups/session.ts`).

## OpenCode 2 storage and credentials

The CLI still resolves a SQLite file below the OpenCode data directory, overridable by `OPENCODE_DB`; `:memory:` remains special. The `latest`, `dev`, `beta`, `next`, and `prod` channels use `opencode.db`, as does `OPENCODE_DISABLE_CHANNEL_DB=1` or `true`; other channels use a sanitized channel suffix. `opencode debug paths db` prints the selected path ([`cli/src/database-path.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/cli/src/database-path.ts)).

The current database schema has `session_v2`, `session_message`, `session_inbox`, `session_pending`, `credential`, `permission`, `event`, and `event_sequence`, among others. `session_message` holds projected timeline records (`id`, `session_id`, `type`, `seq`, timestamps, JSON `data`); it is neither the durable event log nor an assistant `message.data` row from the old store. Durable events live in `event`, keyed by aggregate and sequence (`core/src/event/sql.ts`, `EventTable`). `session_v2` carries precomputed cost/token columns and terminal state (`time_idle`, `idle_outcome`). Use the versioned schema ([`core/src/database/schema.gen.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/core/src/database/schema.gen.ts)).

`Session.Info` exposes `parentID?`, `projectID`, `agent?`, `model?`, `location`, `subpath?`, `cost`, `tokens`, `outcome?`, and time fields including `idle?`. Its token usage has `input`, `output`, `reasoning`, and `cache: { read, write }`, without a stored `total`; `TokenUsage.total` adds the five counters. See [`schema/src/session.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/schema/src/session.ts) and [`token-usage.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/schema/src/token-usage.ts).

Credentials now live in SQLite's `credential` table and support multiple accounts per integration. `Credential.Entry` is `{ id, integrationID, label, active, value }`; `value.type` is `oauth`, `key`, or `external`. OAuth carries `methodID`, `access`, `refresh`, `expires`, and optional metadata. The legacy `auth.json` is a migration input, not the current credential store (`core/src/database/migration/20260805200742_import_legacy_credentials.ts`). See [`schema/src/credential.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/schema/src/credential.ts); `opencode auth list`, `login`, `logout`, `switch`, `export`, and `import` are the current CLI surface.

## OpenCode 2 CLI and environment

Install via `https://opencode.ai/v2/install` or npm `@opencode/cli`, not the legacy `opencode-ai` package. The command definitions below are pinned to [`cli/src/commands/commands.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/cli/src/commands/commands.ts), rather than help from a legacy binary.

| Command | Adapter-relevant flags or behavior |
| --- | --- |
| `opencode [directory]` | TUI: `-c`/`--continue`, `-s`/`--session`, `--prompt`, `--auto`, `--server`, `--standalone`; no root `--model`, `--agent`, `--fork`, or `--mini` |
| `opencode mini` | separate minimal-interface command, including `--model`, `--agent`, `--fork`, `--prompt`, and replay controls |
| `opencode run [message..]` | headless: `--format default\|json`, `--model provider/model#variant`, `--agent`, `--file`, `--title`, `--thinking`, continue/session/fork and permission flags; server selection uses `--server`, not `--attach` |
| `opencode serve` | v2 API/web server: `--hostname`, `--port`, repeatable `--cors`, `--service`, `--stdio` |
| `opencode service start\|restart\|status\|stop` | persistent background server lifecycle; `get`, `set`, and `unset` manage service settings and environment |
| `opencode api` | call an OpenAPI operation ID or HTTP method/path; `--data`, `--header`, `--param` |
| `opencode plugin list\|add\|check\|update\|remove` | manage global plugins; replaces the old `plugin <module>` command |
| `opencode session list\|delete\|export\|import` | native session operations; export/import are now subcommands of `session` |
| `opencode upgrade [target]` | upgrade to latest or a named version; no `--major` flag in v2 |
| `opencode pair`, `reload`, `models`, `stats`, `acp`, `uninstall`, `debug` | pairing, config reload, catalogs, usage, ACP, installation, diagnostics |

`OPENCODE_PASSWORD` supplies HTTP basic-auth credentials; `OPENCODE_SERVER_PASSWORD` is retained as a fallback, with username `opencode` (`cli/src/env.ts`, `password`; `cli/src/services/server-connection.ts`, `resolve`). `Env.session()` copies the CLI environment excluding both password variables. The TUI supplies this environment for local sessions, but not for an explicit remote `--server` (`cli/src/commands/handlers/default.ts`). Per-session environment is set through `PUT /api/session/:sessionID/environment`. Do not treat a CLI launch variable as proof that a plugin in an already-running background server sees it in `process.env`.

`XDG_DATA_HOME`, `XDG_CONFIG_HOME`, `XDG_CACHE_HOME`, and `XDG_STATE_HOME` still relocate the corresponding OpenCode roots. `OPENCODE_CONFIG_DIR` replaces the config root (`util/src/global-roots.ts`, `roots`; [`util/src/global.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/util/src/global.ts), `layer`). Database overrides are described in [OpenCode 2 storage and credentials](#opencode-2-storage-and-credentials).

`OPENCODE_CONFIG_CONTENT` is parsed as the last, highest-priority config document ([`core/src/config.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/core/src/config.ts)). Global `permissions` arrays concatenate across documents; agent-specific rules append afterward, and evaluation is last-match-wins ([`config/plugin/agent.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/core/src/config/plugin/agent.ts), [`permission.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/core/src/permission.ts)). The native delegation action is `subagent`; a global deny is `{"permissions":[{"action":"subagent","resource":"*","effect":"deny"}]}`.

The standalone TUI launches `opencode serve --stdio --port 0` as a private child with inherited environment. Plugin `process.ppid` is therefore the TUI process; a shared service does not provide that relationship ([`cli/src/services/standalone.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/cli/src/services/standalone.ts)).

The migration command `opencode upgrade --major` is a legacy 1.19.0 flag, not a v2 flag ([legacy upgrade command](https://github.com/anomalyco/opencode/blob/v1.19.0/packages/opencode/src/cli/cmd/upgrade.ts)).
