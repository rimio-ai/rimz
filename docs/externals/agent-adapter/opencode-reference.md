# OpenCode protocol reference

This page mirrors the OpenCode surfaces an adapter binds to: plugins, native requests, events, HTTP, session storage, credentials, and the CLI environment. The reference is OpenCode 2 only. How RimZ maps upstream surfaces onto its own types, and which of them it wires, is in [adapter_opencode.md](../../internals/agents/adapter_opencode.md). An upstream refresh does not establish RimZ adapter compatibility with a new major version.

**Current baseline.** OpenCode **2.0.27**, tag [`v2.0.27`](https://github.com/anomalyco/opencode/tree/v2.0.27), commit `1f4d07a4a519185955ae8ab2ea3607805509e386`, with npm `@opencode/cli`, `@opencode/plugin`, and `@opencode/client` 2.0.27. The CLI package was published 2026-10-12 at 04:15:00 UTC ([npm metadata](https://registry.npmjs.org/@opencode%2fcli)). Baseline checked 2026-10-12 against source, registry metadata, and docs. System-prompt replacement, config substitution, and managed-settings precedence below were read at **2.1.4**; the replacement and inline-substitution route was also checked at **2.0.0**, and missing-file behavior at **2.0.0**, **2.0.29**, and **2.0.30**. Check the package name as well as the version when refreshing: the legacy `opencode-ai`, `@opencode-ai/plugin`, and `@opencode-ai/sdk` packages do not describe these contracts.

## Model-selection server (v2.1.4)

This section alone is pinned to [`v2.1.4`](https://github.com/anomalyco/opencode/tree/v2.1.4); the plugin and storage baseline below remains 2.0.27.

- `opencode serve --stdio --hostname 127.0.0.1 --port 0` prints one JSON readiness line containing `url`. Stdin EOF stops the server ([server process](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/cli/src/server-process.ts)). `OPENCODE_PASSWORD` sets the password; HTTP Basic auth uses username `opencode` ([HTTP process](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/server/src/process.ts)).
- `GET /api/model?location[directory]=<directory>` returns `{location, data:[{providerID,id,…}]}`. This is the configured-provider catalog, not proof that its credentials work ([model routes](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/protocol/src/groups/model.ts), [location envelope](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/schema/src/location.ts)). `OPENCODE_DISABLE_MODELS_FETCH` skips the background catalog refresh; cache or the bundled snapshot still supplies models ([server startup](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/cli/src/server-process.ts)).
- `POST /api/session` accepts a caller-supplied `id`, `location:{directory}`, and `model:{providerID,id}`. Creation with an existing ID is idempotent. `POST /api/session/:sessionID/model` accepts `{model:{providerID,id}}` and returns 204 ([session routes](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/protocol/src/groups/session.ts)). IDs need only start with `ses`; generated IDs are `ses_` plus 12 hex characters of an inverted millisecond/counter stamp and 14 base62 characters ([identifier](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/schema/src/identifier.ts)).
- Model references split `provider/id[#variant]` at the first `/`; IDs can themselves contain `/` ([model schema](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/schema/src/model.ts)).
- `OPENCODE_CONFIG_CONTENT` policies of `{action:"integration.use",resource:"plugin:*",effect:"deny"}` and the corresponding `mcp:*` resource disable external plugins and MCP for that process, not config-defined providers ([policy schema](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/schema/src/config/policy.ts), [plugin loading](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/core/src/plugin/module.ts), [policy application](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/core/src/config/plugin/policy.ts)). The server also reads `OPENCODE_FILEWATCHER_DISABLE` and `OPENCODE_DISABLE_FFF` ([server startup](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/cli/src/server-process.ts)).

## OpenCode 2 sources

Source paths in the OpenCode 2 sections are under `packages/` at `v2.0.27` unless another tag is linked. Follow versioned source when the live documentation or JSON Schema describes a different release.

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

The CLI normally connects to a persistent background server, starting it when needed. `--server <url>` selects an existing server. `--standalone` starts a private child server and cannot be combined with `--server`. Server plugins execute in the server process, not in the TUI process. See [`cli/src/services/server-connection.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/cli/src/services/server-connection.ts), `resolve`, and [`standalone.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/cli/src/services/standalone.ts), `command`.

The standalone TUI launches `opencode serve --stdio --port 0` as a private child with inherited environment. Plugin `process.ppid` is therefore the TUI process. A shared service does not provide that relationship ([`cli/src/services/standalone.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/cli/src/services/standalone.ts)).

At 2.1.4, the shared service runs `opencode serve --service`, a command the CLI builds itself; the service config file overrides only its environment ([`cli/src/services/service-config.ts`](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/cli/src/services/service-config.ts), `options`). The first client that finds no service spawns it detached with the client's own environment under the configured entries ([`client/src/service-contender.ts`](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/client/src/service-contender.ts), `spawnServiceContender`), so the service inherits that client's environment, and the client stays its parent until it exits. An embedder calling `@opencode/client` directly may pass its own `command` ([`client/src/service.ts`](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/client/src/service.ts), `EnsureOptions`).

Config uses plural `plugins`, with string specs or `{ package, options }` entries. Relative paths resolve from the declaring config. Automatic discovery scans both `plugin/` and `plugins/` under discovered config directories, accepting direct `.ts`/`.js` files and immediate package directories (`core/src/plugin/source-directory.ts`, `discover`). For an explicit local plugin, configure a directory. Configured file paths produce a warning and are skipped. This differs from the live docs, which show explicit file paths and name only plural discovery directories. TUI-only plugins are configured separately in `cli.json`. See [plugin configuration](https://opencode.ai/v2/docs/plugins), [`source-directory.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/core/src/plugin/source-directory.ts), and [`source.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/core/src/config/plugin/source.ts).

A local directory resolves its server entry from `server`, then `index` ([`plugin/src/host.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/plugin/src/host.ts)). The host supplies `effect` and `@opencode/plugin` runtime imports ([`runtime-modules.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/plugin/src/runtime-modules.ts)).

### Client (TUI) plugins

This subsection is pinned separately to **v2.1.4**, commit `68bc26b7da1990ce10ffecfbc1e09eb13aa5d824`. Client discovery scans immediate directories under `plugins/`, not legacy `plugin/` files. A local directory needs physical `tui.ts` and, for a server half, `index.ts` or `server.ts`; no package.json or precompiled JavaScript is needed by the standard Bun binary. Both halves may default-export `{ id: "rimz", setup }`: server and client ID registries are separate. A legacy same-ID `plugin/rimz.ts` wins the server registry while the directory's TUI half can still load ([discovery](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/tui/src/plugin/discovery.ts), [host](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/plugin/src/host.ts), [server registry](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/core/src/plugin/supervisor.ts)).

The client context exposes `ui.router.current()` as the route itself (`{type:"session",sessionID}` or home/plugin), and `ui.tabs.list()` as this client's tabs. `data.listen` receives `{name,details}`, where `details` is an OpenCode event; its `location` is optional and the stream spans all server locations. Session getters are cache-only; `session.sync`, `session.message.sync`, `session.permission.sync`, and `session.form.sync` fetch them. `session.status(id)` returns `idle` or `running` and defaults to idle. Location model lists match by provider plus model ([context](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/plugin/src/tui/context.ts), [client data](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/client/src/solid/data.ts)).

The data store handles `session.deleted` by synchronously evicting the session's info, status, messages, permissions and forms before an asynchronously queued plugin listener runs ([`removeSession` and `handleEvent`](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/client/src/solid/data.ts#L602-L683), [store subscription](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/client/src/solid/data.ts#L2006-L2010)). A plugin cannot require cached session info to handle deletion or a tracked request's later reply. On `server.connected`, `handleEvent` asynchronously re-hydrates `session.active` without emitting another event; lifecycle events received during hydration override that snapshot ([rehydration](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/client/src/solid/data.ts#L634-L656)). The TUI's `client.session.active()` reads the server directly and returns a session-ID map of `{type:"running"}` entries; absent IDs are idle ([generated client](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/client/src/promise/generated/client.ts#L635-L645), [server handler](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/server/src/handlers/session.ts#L148-L156)).

`client.message.list` takes `sessionID`, limit up to 200, initial `order:"desc"`, and then `cursor` without `order`. Even short nonempty pages may have `cursor.next`; stop on an empty page or no next cursor. User inbox events carry `item.type === "user"` and `item.payload.text`. Tool terminal events carry no name: join `session.tool.input.started`'s `name` by `assistantMessageID` and tool `id`, or read the cached assistant's tool content. `server.connected` marks both initial connection and reconnect ([generated types](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/client/src/promise/generated/types.ts), [message handler](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/server/src/handlers/message.ts)).

An isolated probe of the installed 2.1.4 binary confirmed that client `process.pid` equals the pane PID, both plain and standalone launches load `tui.ts`, and explicit `--standalone` stays in argv. After initialization, `keymap.commands()` includes `service.restart` for a managed shared-service connection, but not standalone or disabled-service modes ([command registration](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/tui/src/app.tsx)). Cleanup ran on Ctrl+C and pane destruction in that probe, but unconditional awaiting is not promised: unmount can discard the disposal promise ([plugin provider](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/tui/src/plugin/context.tsx)).

### Server plugin API

The Promise API exports `Plugin.define` from `@opencode/plugin`. `@opencode/plugin/effect` is the Effect entry point. A module must default-export a definition with `id` and either `setup` or `effect` ([`core/src/plugin/module.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/core/src/plugin/module.ts), `Module`). A Promise plugin is `{ id: string, setup(context): Promise<Cleanup | void> | Cleanup | void }`, with `Cleanup = () => Promise<void> | void`. `Context` carries `app`, `location`, `options`, and domains including `event`, `session`, `model`, `tool`, `shell`, `permission`, `integration`, and `storage`. Use these domains for server operations. The v1 `PluginInput.client`, `serverUrl`, and Bun shell helper are absent from this context (`plugin/src/promise/plugin.ts`, `Plugin` and `Context`).

Register hooks through a domain. Each hook call returns `Promise<Registration>`, and its callback accepts one input object and returns `void` or `Promise<void>`. Await the registration before using `dispose(): Promise<void>` ([`plugin/src/promise/registration.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/plugin/src/promise/registration.ts)). The adapter-relevant catalog is:

| Registration | Current shape |
| --- | --- |
| `context.session.hook("prompt", callback)` | `sessionID`, `messageID`, mutable `prompt`, `metadata?`, `delivery` |
| `context.session.hook("context", callback)` | `sessionID`, `agent`, `model`, mutable `system`, `messages`, `options`, `tools` |
| `context.session.hook("compaction", callback)` | the context shape plus optional `result` to replace model compaction |
| `context.session.hook("title" / "generate", callback)` | auxiliary session requests. Title can supply a `result` |
| `context.session.hook("model.request" / "http.request" / "http.response", callback)` | session/model identity and `kind: "primary" \| "compaction" \| "title" \| "generate"`. The HTTP hooks expose the actual `Request` or `Response` |
| `context.session.hook("retry", callback)` | error, attempt, and mutable retry decision |
| `context.tool.hook("execute.before" / "execute.after", callback)` | tool, session, agent, message and call identity. Mutable input before execution, completed result or error afterward |
| `context.permission.hook("evaluate", callback)` | action, resources, source, and mutable permission effect/message |
| `context.shell.hook("create.before", callback)` | mutable command, working directory, timeout, shell, and environment |
| `context.event.subscribe(...)` | the native async-iterable event subscription API |

The definitions are in [`plugin/src/promise/session.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/plugin/src/promise/session.ts), [`tool.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/plugin/src/promise/tool.ts), [`permission.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/plugin/src/promise/permission.ts), [`shell.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/plugin/src/promise/shell.ts), and [`event.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/plugin/src/promise/event.ts). The `http.response` hook exposes provider response headers.

`context.session.get({sessionID})` returns session info directly, including title, agent, model, and location but not the creation version. `context.model.list()` returns `{data: ModelInfo[]}`. Model entries carry `id`, `providerID`, `name`, and `limit: {context, input?, output}`. The context hook's mutable system array contains text parts, not strings: append `{type: "text", text}` ([session API](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/plugin/src/promise/session.ts), [model API](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/plugin/src/promise/model.ts)).

## OpenCode 2 events and HTTP

A location identifies an absolute working directory and its project. `Location.Ref` carries `directory` and optional `workspaceID`. `Location.Info` adds project identity and paths ([`schema/src/location.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/schema/src/location.ts)).

Native events use `{ id, type, created, data, location?, metadata? }`, not `{ type, properties }`. Durable events additionally carry `durable: { aggregateID, seq, version }` (`schema/src/event.ts`, `Payload`). The stream-opening `server.connected` is an exception without `created` (`server/src/handlers/event.ts`). The session event family uses `session.execution.*`, `session.step.*`, `session.text.*`, `session.reasoning.*`, `session.tool.*`, and other `session.*` types. `session.created.data` carries creation fields directly, not `info: Session`. `session.created.data.version` records the application version at creation ([`core/src/session.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/core/src/session.ts)). The exact inventory is [`schema/src/session-event.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/schema/src/session-event.ts).

`session.status` reports `idle`, `busy`, or `retry`. `session.idle` is marked deprecated in [`schema/src/session-status-event.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/schema/src/session-status-event.ts). Completion outcomes also have durable `session.execution.succeeded`, `session.execution.failed`, and `session.execution.interrupted` events.

Per-step usage arrives in `session.step.ended.data`, including `assistantMessageID`, `cost`, `tokens`, and `finish`. A native assistant timeline message uses `type: "assistant"`, `agent`, a `model` reference, `content`, and `time: { created, streamed?, completed? }`. `cost`, `tokens`, `error`, and `retry` may be absent. Its structured error is `{ type, message, status?, response?: { body } }`. See [`schema/src/session-message.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/schema/src/session-message.ts), `Assistant`, and [`session-error.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/schema/src/session-error.ts), `Error`.

| Route | Contract |
| --- | --- |
| `GET /api/info` | server `version`, `pid`, `urls`, `paths.tmp`, optional capabilities. Replaces the old health response |
| `GET /api/event` | volatile SSE across server locations, including plugin `rpc.*` events. Disconnections lose events and slow consumers can overflow |
| `GET /api/experimental/session/:sessionID/log` | durable session SSE. `after` is an exclusive aggregate sequence, `follow=true` continues live. `log.synced` marks the captured replay watermark |
| `GET /api/session`, `POST /api/session`, `GET /api/session/:sessionID` | list, create, and retrieve native sessions. Creation accepts optional caller-chosen `id` |
| `GET /api/session/:sessionID/message` | paginated timeline, `{ data, cursor }` |
| `GET /api/model` | native model catalog |
| `POST /api/session/:sessionID/prompt` | submit a prompt |
| `GET /api/session/:sessionID/context` | active context after the last compaction |
| `POST /api/session/:sessionID/interrupt` | interrupt execution |
| `GET /openapi.json` | the current HTTP schema, not the old `/doc` route |

The route definitions are in `protocol/src/groups/server.ts`, `event.ts`, `session.ts`, `message.ts`, and `model.ts`. The OpenAPI mount is in [`server/src/routes.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/server/src/routes.ts). `/api/event` sends SSE comment heartbeats every 15 seconds (`server/src/handlers/event.ts`). A durable log cursor comes from `durable.seq` or `log.synced.seq`, never an event ID or a sequence from another aggregate (`schema/src/event-log.ts`). The Promise client is constructed with `OpenCode.make` from `@opencode/client`, not the legacy `createOpencodeClient` (`client/src/promise/index.ts`).

The `session.*` events in this table are durable. The `form.*` and `permission.*` events are ephemeral. Plugin subscriptions include these adapter-relevant events ([`plugin/src/promise/event.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/plugin/src/promise/event.ts), [session event catalog](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/schema/src/session-event.ts)):

| Event | Relevant data |
| --- | --- |
| `session.created` | `sessionID`, `parentID?`, `location.directory`, `title?`, `version`, `agent?`, `model?` |
| `session.deleted` | `sessionID` |
| `session.execution.started` / `succeeded` / `failed` / `interrupted` | `sessionID`. Failure has `error`, interruption has `reason` |
| `session.step.started` | `sessionID`, `assistantMessageID`, `agent`, `model` |
| `session.step.ended` | `sessionID`, `assistantMessageID`, `finish`, `cost`, `tokens` |
| `session.renamed` | `sessionID`, `title` |
| `session.model.selected` / `session.agent.selected` | `sessionID`, `model` / `agent` |
| `session.compaction.started` / `ended` / `failed` | `sessionID` |
| `form.created` / `replied` / `cancelled` | form and answer shapes below |
| `permission.asked` / `replied` | request and reply shapes below |

## OpenCode 2 native requests

`permission.asked.data` is `{ id, sessionID, action, resources, save?, metadata?, source?, message? }`. A tool source is `{ type: "tool", messageID, id }`. `permission.replied.data` is `{ sessionID, requestID, reply: "once" | "always" | "reject" }`. Config uses plural `permissions`, an ordered array of `{ action, resource, effect: "allow" | "deny" | "ask" }` (`schema/src/permission.ts`, `Request`, `Ruleset`, `schema/src/config.ts`, `Info`).

Global `permissions` arrays concatenate across documents. Agent-specific rules append afterward, and evaluation is last-match-wins ([`config/plugin/agent.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/core/src/config/plugin/agent.ts), [`permission.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/core/src/permission.ts)). The native delegation action is `subagent`. A global deny is `{"permissions":[{"action":"subagent","resource":"*","effect":"deny"}]}`.

Pending permissions are listed at `GET /api/permission/request` for a location or `GET /api/session/:sessionID/permission` for a session. Reply with `POST /api/session/:sessionID/permission/:requestID/reply`, body `{ decision: "once" | "always" | "reject", message? }`. Notice that the reply request calls the field `decision`, while the event calls it `reply` ([`protocol/src/groups/permission.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/protocol/src/groups/permission.ts)).

Plugins reply through `context.permission.reply({ sessionID, requestID, decision, message? })` ([permission domain](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/plugin/src/promise/permission.ts)). The experimental HTTP reply route returns `204 No Content`. An unknown or already-answered request produces `PermissionNotFoundError` ([route definitions](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/protocol/src/groups/permission.ts)).

`--auto` auto-approves permissions except explicit denies ([CLI flags](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/cli/src/commands/commands.ts)). An explicit configured deny returns before the `evaluate` plugin hook. An `always` answer persists the request's `save` resources per project and reevaluates eligible pending requests ([`core/src/permission.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/core/src/permission.ts), `evaluateInput` and `reply`).

A `reject` also rejects every pending request in that session. A nonempty `message` supplies model feedback for that rejection batch. Other decisions ignore `message`. An `always` answer can emit replies for other requests that the saved rule permits ([permission reply](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/core/src/permission.ts)).

Permission events are ephemeral and are absent from the durable session log. Pending requests live in memory. Closing the permission service rejects them ([permission schema](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/schema/src/permission.ts), [service](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/core/src/permission.ts)). The database `permission` table stores saved rules ([database schema](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/core/src/database/schema.gen.ts)).

The native form protocol is `form.created` (`data.form`), `form.replied` (`data: { id, sessionID, answer }`), and `form.cancelled` (`data: { id, sessionID }`). A form has `id`, `sessionID`, `title`, `fields`, and optional metadata. Field kinds are `string`, `number`, `integer`, `boolean`, `multiselect`, and `external`. Answers map field keys to strings, numbers, booleans, or string arrays. MCP elicitations can use `sessionID: "global"`, which is not a session ID. See [`schema/src/form.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/schema/src/form.ts).

Use `GET /api/session/:sessionID/form` to list pending forms, `POST /api/session/:sessionID/form/:formID/reply` with `{ answer }` to answer, and `DELETE /api/session/:sessionID/form/:formID` to cancel (`protocol/src/groups/session.ts`).

## OpenCode 2 storage and credentials

The CLI resolves a SQLite file below the OpenCode data directory, overridable by `OPENCODE_DB`. `OPENCODE_DB=:memory:` selects an in-memory database. The `latest`, `dev`, `beta`, `next`, and `prod` channels use `opencode.db`, as does `OPENCODE_DISABLE_CHANNEL_DB=1` or `true`. Other release channels use `opencode-<channel>.db`, replacing characters outside `[a-zA-Z0-9._-]` with `-`. `opencode debug paths db` prints the selected path ([`cli/src/database-path.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/cli/src/database-path.ts)).

The current database schema has `session_v2`, `session_message`, `session_inbox`, `session_pending`, `credential`, `permission`, `event`, and `event_sequence`, among others. `session_message` holds projected timeline records (`id`, `session_id`, `type`, `seq`, timestamps, JSON `data`). It is neither the durable event log nor an assistant `message.data` row from the old store. Durable events live in `event`, keyed by aggregate and sequence (`core/src/event/sql.ts`, `EventTable`). `session_v2` carries precomputed cost/token columns and terminal state (`time_idle`, `idle_outcome`). Use the versioned schema ([`core/src/database/schema.gen.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/core/src/database/schema.gen.ts)).

`Session.Info` exposes `parentID?`, `projectID`, `agent?`, `model?`, `location`, `subpath?`, `cost`, `tokens`, `outcome?`, and time fields including `idle?`. Its token usage has `input`, `output`, `reasoning`, and `cache: { read, write }`, without a stored `total`. `TokenUsage.total` adds the five counters. See [`schema/src/session.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/schema/src/session.ts) and [`token-usage.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/schema/src/token-usage.ts).

Credentials live in SQLite's `credential` table and support multiple accounts per integration. `Credential.Entry` is `{ id, integrationID, label, active, value }`. `value.type` is `oauth`, `key`, or `external`. OAuth carries `methodID`, `access`, `refresh`, `expires`, and optional metadata. The legacy `auth.json` is a migration input, not the current credential store (`core/src/database/migration/20260805200742_import_legacy_credentials.ts`). See [`schema/src/credential.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/schema/src/credential.ts). `opencode auth list`, `login`, `logout`, `switch`, `export`, and `import` are the current CLI surface.

## OpenCode 2 CLI and environment

Install via `https://opencode.ai/v2/install` or npm `@opencode/cli`, not the legacy `opencode-ai` package. The command definitions below are pinned to [`cli/src/commands/commands.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/cli/src/commands/commands.ts), with resume behavior in [`handlers/default.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/cli/src/commands/handlers/default.ts).

| Command | Adapter-relevant flags or behavior |
| --- | --- |
| `opencode [directory]` | TUI: `-c`/`--continue` resumes the last session. `-s`/`--session` resumes an ID or reserves it for creation on the first prompt. Also `--prompt`, `--auto`, `--server`, `--standalone`. No root `--model`, `--agent`, `--fork`, or `--mini` |
| `opencode mini` | separate minimal-interface command, including `--model`, `--agent`, `--fork`, `--prompt`, and replay controls |
| `opencode run [message..]` | headless: `--format default\|json`, `--model provider/model#variant`, `--agent`, `--file`, `--title`, `--thinking`, `--continue`/`-c`, `--session`/`-s`, `--fork`, and `--auto`. Server selection uses `--server`, not `--attach` |
| `opencode serve` | v2 API/web server: `--hostname`, `--port`, repeatable `--cors`, `--service`, `--stdio` |
| `opencode service start\|restart\|status\|stop` | persistent background server lifecycle. `get`, `set`, and `unset` manage service settings and environment |
| `opencode api` | call an OpenAPI operation ID or HTTP method/path. `--data`, `--header`, `--param` |
| `opencode plugin list\|add\|check\|update\|remove` | manage global plugins. Replaces the old `plugin <module>` command |
| `opencode session list\|delete\|export\|import` | native session operations. Export/import are subcommands of `session` |
| `opencode upgrade [target]` | upgrade to latest or a named version. No `--major` flag in v2 |
| `opencode auth` | [Credential commands](#opencode-2-storage-and-credentials) |
| `opencode pair`, `reload`, `models`, `stats`, `acp`, `uninstall`, `debug` | pairing, config reload, catalogs, usage, ACP, installation, diagnostics |

Session IDs must start with `ses` ([ID schema](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/schema/src/session-id.ts)). The TUI preserves a supplied, missing `--session` ID for creation on the first prompt. For `run` and `mini`, `--session` takes precedence over `--continue`. Their `--continue` selects the newest top-level session whose working directory exactly matches the resolved location. Combining `--fork` with a missing explicit session fails ([session targeting](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/cli/src/session-target.ts)).

`OPENCODE_PASSWORD` supplies HTTP basic-auth credentials. `OPENCODE_SERVER_PASSWORD` is retained as a fallback, with username `opencode` (`cli/src/env.ts`, `password`, `cli/src/services/server-connection.ts`, `resolve`). [`Env.session()`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/cli/src/env.ts) copies the CLI environment excluding both password variables. The TUI supplies this environment for both standalone and local background-service sessions, but not for an explicit `--server` (`cli/src/commands/handlers/default.ts`). The TUI sets per-session environment through `PUT /api/session/:sessionID/environment` ([TUI request](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/tui/src/app.tsx)). Do not treat a CLI launch variable as proof that a plugin in an already-running background server sees it in `process.env`.

`XDG_DATA_HOME`, `XDG_CONFIG_HOME`, `XDG_CACHE_HOME`, and `XDG_STATE_HOME` relocate the corresponding OpenCode roots. `OPENCODE_CONFIG_DIR` replaces the config root (`util/src/global-roots.ts`, `roots`, [`util/src/global.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/util/src/global.ts), `layer`). Database overrides are described in [OpenCode 2 storage and credentials](#opencode-2-storage-and-credentials).

`OPENCODE_CONFIG_CONTENT` loads after discovered user and project config documents, but is not the highest-priority document. Managed settings load afterward: the system directory overrides inline content, and macOS managed preferences outrank that directory ([`core/src/config.ts`, 2.1.4](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/core/src/config.ts), `load`).

Since 2.0.6, `OPENCODE_CLI_CONFIG_CONTENT` supplies a separate inline CLI config object over `cli.json`. Unset or exactly `""` is absent. The loader accepts JSONC comments and trailing commas; a parse error, non-object root, or schema decode failure silently drops the whole inline value. Objects merge recursively, while arrays and other values replace the lower layer ([`cli/src/config/config.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/cli/src/config/config.ts)).

Since 2.0.13, `tabs.mode` accepts `"auto"`, `"on"`, or `"off"` ([`tui/src/config/index.tsx`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/tui/src/config/index.tsx)). Each layer normalizes legacy `tabs.enabled` to `mode` only when `mode` is absent, so an explicit `mode` wins ([`cli/src/config/schema.ts`](https://github.com/anomalyco/opencode/blob/v2.0.27/packages/cli/src/config/schema.ts)).

To migrate from 1.19.0, run that legacy binary's `opencode upgrade --major` ([legacy upgrade command](https://github.com/anomalyco/opencode/blob/v1.19.0/packages/opencode/src/cli/cmd/upgrade.ts)).

### System prompts and config substitution

`agents.<name>.system` supplies that agent's replacement system text. The config plugin copies `item.system` into `agent.system` ([`core/src/config/plugin/agent.ts`, 2.1.4](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/core/src/config/plugin/agent.ts), also present at [2.0.0](https://github.com/anomalyco/opencode/blob/v2.0.0/packages/core/src/config/plugin/agent.ts)). Each request's base transcript uses nonempty `agent.system` instead of `SessionSystemPrompt.make(...)`, then adds its initial project instructions; an empty string falls back to the built-in prompt ([`core/src/session/model-request.ts`, 2.1.4](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/core/src/session/model-request.ts), `baseTranscript`). The selected agent comes from the session, falling back to the configured default, not necessarily `build` ([`core/src/session/context.ts`](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/core/src/session/context.ts), `select`, and [`core/src/config/plugin/agent.ts`](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/core/src/config/plugin/agent.ts)).

Inline `OPENCODE_CONFIG_CONTENT` goes through `ConfigVariable.substitute` before its JSONC parse ([`core/src/config.ts`, 2.1.4](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/core/src/config.ts), also present at [2.0.0](https://github.com/anomalyco/opencode/blob/v2.0.0/packages/core/src/config.ts)). Substitution first expands `{env:...}` over the raw config text, then matches `{file:...}` tokens with `/\{file:[^}]+\}/g`. A file token on a line whose preceding text, trimmed at the start, begins with `//` is skipped. Absolute paths are used as written, `~/` expands to the home, and relative paths resolve from the config directory (the location directory for inline content). File contents are trimmed and JSON-escaped into the config text, not re-scanned for either token form ([`core/src/config/variable.ts`, 2.1.4](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/core/src/config/variable.ts), `substitute` and `substituteFiles`).

Config reloads on watched config-file changes and re-runs inline substitution, so a `{file:...}` target must remain readable after startup ([`core/src/config.ts`, 2.1.4](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/core/src/config.ts), `load` and `reload`). A missing or unreadable target differs by version:

- **2.0.0 through 2.0.29:** the inline load fails with `bad file reference` ([2.0.0](https://github.com/anomalyco/opencode/blob/v2.0.0/packages/core/src/config/variable.ts), [2.0.29](https://github.com/anomalyco/opencode/blob/v2.0.29/packages/core/src/config/variable.ts)). At startup the server cannot load its config. A failed reload logs `failed to reload config` and keeps the previous config ([`core/src/config.ts`, 2.0.29](https://github.com/anomalyco/opencode/blob/v2.0.29/packages/core/src/config.ts), `reload`).
- **From 2.0.30:** a missing or unreadable target warns and substitutes empty ([2.0.30](https://github.com/anomalyco/opencode/blob/v2.0.30/packages/core/src/config/variable.ts), [2.1.4](https://github.com/anomalyco/opencode/blob/v2.1.4/packages/core/src/config/variable.ts)). An empty `agents.<name>.system` therefore selects the built-in prompt.
