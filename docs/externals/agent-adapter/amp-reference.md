# Amp CLI protocol reference

> RimZ's mapping of this surface is [adapter_amp.md](../../internals/agents/adapter_amp.md). The agent-agnostic lifecycle contract is [model.md](../../internals/agents/model.md), and the account and spend contract is [providers.md](../../internals/agents/providers.md).

This reference covers Amp plugins, thread state, transcripts, permissions, modes, stream JSON, runners, authentication, and settings. It describes the architecture introduced as "Neo" in May 2026. The native dial modes are `low`, `medium`, `high`, and `ultra`. Historical cache evidence remains separate from the supported Plugin API.

Refresh baseline: `@ampcode/cli` [0.0.1791849701-g1448da](https://www.npmjs.com/package/@ampcode/cli/v/0.0.1791849701-g1448da), the npm `latest` dist-tag, checked 2026-10-12 (local date). The build reports release time 2026-10-13T00:01:41Z. [npm metadata](https://registry.npmjs.org/@ampcode%2fcli) records publication at 2026-10-13T00:22:19.687Z and `gitHead` `1448da90e9acbe29121179235b527069db917e31`. Amp publishes rolling builds without public source tags. The UTC release date falls after the local check date.

The unversioned docs and the released binary's help and Plugin API declarations were checked for this refresh. `amp plugins show-docs` carries `sitemap-lastmod: /docs/plugin-api 2026-10-06`. The [Linux package](https://registry.npmjs.org/@ampcode/cli-linux-x64/-/cli-linux-x64-0.0.1791849701-g1448da.tgz) supplies the build-specific command and type evidence below. Runtime captures dated 2026-09-14 retain their older build, `0.0.1789344113-g6e4515`. They were not repeated with authenticated threads. Where docs and declarations disagree, the page names the difference.

## Upstream sources

| Surface | Source |
| --- | --- |
| Docs index | <https://ampcode.com/docs> (the older `/manual` paths redirect here) |
| CLI, install, and update | <https://ampcode.com/docs/cli> |
| Execute mode | <https://ampcode.com/docs/cli/execute-mode> |
| Stream JSON schema | <https://ampcode.com/docs/cli/streaming-json> |
| Settings and environment | <https://ampcode.com/docs/cli/settings> |
| Runners and remote control | <https://ampcode.com/docs/cli/runners> · <https://ampcode.com/docs/cli/remote-control> |
| Plugins guide | <https://ampcode.com/docs/customize/plugins> |
| Plugin API type reference | <https://ampcode.com/docs/plugin-api> |
| Tools and permissions | <https://ampcode.com/docs/tools#permissions> · <https://ampcode.com/notes/permissions> (permission rules) |
| Modes and subagents | <https://ampcode.com/docs/models-and-subagents> · <https://ampcode.com/news/the-dial> · <https://ampcode.com/news/build-your-own-dial> |
| Thread commands | <https://ampcode.com/docs/threads> |
| Pricing and usage | <https://ampcode.com/docs/pricing> · <https://ampcode.com/news/explain-usage> |
| SDKs | <https://ampcode.com/docs/sdk> |
| Credentials and storage | <https://ampcode.com/security> |
| Architecture and TUI changes | <https://ampcode.com/news/neo> · <https://ampcode.com/news/drop-the-neo> · <https://ampcode.com/news/so-long-tui-sidebar> |
| Distribution | <https://www.npmjs.com/package/@ampcode/cli> · <https://ampcode.com/news/npm-package-changes> |

The installed binary answers for its own build. These commands are read-only:

| Command | Emits |
| --- | --- |
| `amp version` | build token and release timestamp |
| `amp --help` | public command list, global flags, environment variables, and a settings reference |
| `amp plugins show-docs` | the `@ampcode/plugin` declarations this build understands |
| `amp plugins show-agent-options --json` | `models[]` (`id`, `provider`, `contextWindow`, `maxOutputTokens`, `capabilities.efforts`) and `builtinToolNames[]` |
| `amp tools list` | active tools, including MCP tools |

Nested help works on this build. For example, `amp threads usage --help` prints thread-usage options, including `--details`, `--start`, and `--end`.

## Version and distribution

`amp version` prints the build token, then a parenthesized release annotation:

```text
0.0.1791849701-g1448da (released 2026-10-13T00:01:41.000Z, 1h ago)
```

The number after `0.0.` is the release time in Unix seconds. The relative age (`1h ago`) is part of the annotation.

Amp installs through the shell installer (`curl -fsSL https://ampcode.com/install.sh | bash`), the Homebrew formula `ampcode/tap/ampcode`, or the npm package `@ampcode/cli`, which Amp supports but does not recommend ([npm-package-changes](https://ampcode.com/news/npm-package-changes)). The CLI checks for and installs new builds in the background ([docs/cli](https://ampcode.com/docs/cli)).

| Update control | Behaviour |
| --- | --- |
| `amp update` (alias `up`) | installs the latest release. `--target-version <version>` installs a specific one |
| `amp update --porcelain` | prints `updated <version>` or `no update needed` to stdout |
| `amp.updates.mode` | `auto` runs the update, `warn` shows a notification, `disabled` turns checking off |
| `AMP_SKIP_UPDATE_CHECK=1` | skips the update check |

## Surface index

Amp has no command-hook protocol, statusline feed, or interactive stdio RPC: neither the docs nor the baseline `amp --help` describe one. The plugin runtime is the interactive integration seam, and execute mode with stream JSON is the scripted one.

| Concern | Upstream surface | What it lacks |
| --- | --- | --- |
| thread identity | plugin event `thread.id` (`T-${string}`). Stream JSON `session_id` | none |
| turn start and end | [`agent.start`](#agentstart) and [`agent.end`](#agentend) | execute mode skips them unless plugins are ready ([readiness](#runtime-and-execute-mode-readiness)) |
| tool activity | [`tool.result`](#toolresult) | none |
| file edits | `amp.helpers.filesModifiedByToolCall` | covers edit, create, `apply_patch`, and in-place `sed` only |
| tool decision | [`tool.call`](#toolcall) | handler timeout, cancellation, and multi-plugin composition are undocumented |
| permission wait | [`ThreadState`](#thread-state-and-control) `awaiting-approval` | no tool, arguments, question, or resolver |
| plan approval | none | no dedicated event or state |
| user question | `ask_user_choice`, plugin `ctx.ui` dialogs | no dedicated question event or documented question-state mapping |
| compaction | none | no event, percentage, or summary marker ([compaction](#compaction-and-context)) |
| subagents | `PluginThread.parentThreadID()`. Stream JSON `parent_tool_use_id` | no subagent start or stop event ([subagents](#subagents-and-concurrent-threads)) |
| mode, model, effort | `thread.agent()`. Stream JSON `agent_mode` | a built-in agent definition carries the mode only |
| context usage | stream JSON assistant `usage`, `amp threads context <T-id>` | plugin messages carry no usage. Context JSON schema is undocumented |
| cost | `amp usage`, `amp threads usage` | human text only |
| transcript | [`PluginThread.messages()`](#transcript) | no documented local file |
| steering | `appendUserMessage(..., { steer: true })`. Stream JSON input `steer` | none |
| session end | none | `session.start` has no matching end event |
| identity | `PluginSystem.user` | available only inside a running plugin |
| remote control | cross-client access, runners, orbs ([remote control](#remote-control-runners-and-executors)) | plugin executor kind is `local`, `remote`, or `unknown` only |

## Plugins

### Discovery

Amp plugins are TypeScript or JavaScript programs executed with Bun. A plugin is a single `.ts` or `.js` file, or a directory whose entry is `index.ts` or `index.js` (`index.ts` wins when both exist) ([plugins guide](https://ampcode.com/docs/customize/plugins)).

| Scope | Location | Precedence on a name clash |
| --- | --- | --- |
| project | `.amp/plugins/` | 1 |
| system | `$XDG_CONFIG_HOME/amp/plugins/`, default `~/.config/amp/plugins/` (Windows `%USERPROFILE%\.config\amp\plugins\`) | 2 |
| personal | User Settings on ampcode.com | 3 |
| workspace | Workspace Settings, pushed by workspace admins and loaded for every member | 4 |

The plugins guide and `amp plugins --help` name all four scopes. The `show-docs` header of the baseline build lists only project, system, and "Global plugins (limited experimental release)".

| Command | Effect |
| --- | --- |
| `amp plugins add <url>` | installs a single-file plugin from a URL. `--target workspace` installs it into `.amp/plugins/` |
| `amp plugins remove` (alias `rm`) | removes an installed plugin. In the baseline help, absent from the docs |
| `amp plugins update [<name>]` | updates installed plugins or one imported shared plugin |
| `amp plugins list` (alias `ls`) | loads and lists plugins for this command, not for a running Amp session |
| `amp plugins import` | imports a shared personal plugin into a plugin repository checkout |
| `amp plugins repositories` (alias `repos`) | lists Personal Plugins and Workspace Plugins repositories. `amp clone` checks one out |
| `amp plugins show-agent-options` | models and built-in tool names for plugin agents |
| `amp plugins exec` | executes a plugin with a given event |
| `amp plugins show-docs` | the plugin API declarations of this build |
| palette `plugins: reload` | reloads plugins. The guide also names `amp plugins reload`, which the baseline help does not list |

### Runtime and execute-mode readiness

A plugin default-exports a function that receives `PluginAPI`. The function runs once when the plugin loads. Plugins are long-lived processes that may serve several threads concurrently, so per-thread state must be keyed by `ThreadID`. An optional `export const description` must be a static string literal of at most 300 characters.

`amp.on(event, handler)` registers a handler and returns a `Subscription`. `session.start` is fire-and-forget and returns `void`. The request events are `tool.call`, `tool.result`, `agent.start`, `agent.end`, and `changes.prompt`. Their handlers return results defined by `PluginHandlerResult`. `tool.result`, `agent.end`, and `changes.prompt` also admit `void`. Handler order is generally undefined. `changes.prompt` appends results in plugin load order.

`amp.onDispose(callback)` runs cleanup when the plugin is unloaded, reloaded, or the host shuts down gracefully. All of a plugin's dispose callbacks share a budget of about 3 seconds, after which the process is terminated. They do not run when the plugin crashes or is killed with SIGKILL.

Logging goes through `amp.logger.log` or `ctx.logger.log`. Handler log messages are appended to the handler's trace span events.

Execute mode can start its turn before plugins finish loading, which skips `agent.start` and `agent.end`. `--plugin-ready-timeout [seconds]` makes execute mode wait for plugin readiness first. The wait is off unless the flag is set. A bare flag waits 10 seconds, the maximum is 300, and `0` disables it.

### Events

The [Plugin API](https://ampcode.com/docs/plugin-api) declares six events. Five describe the session and turn lifecycle:

```text
session.start
  └─ agent.start
       ├─ tool.call ──► tool.result   (zero or more)
       └─ agent.end
```

`changes.prompt` prepares a Ship or Push to Branch prompt before submission. There is no `session.end`, compaction, model-change, notification, or subagent event. Every handler receives a context with `logger`, `$` (shell runner), `ui`, `ai`, `system`, an optional trace `span`, and `thread: PluginThread`.

#### `session.start`

Fires when Amp starts a thread session: the first message in a new thread, or opening or switching to an existing thread.

```ts
interface SessionStartEvent {
  thread: { id: `T-${string}` }
}
```

#### `agent.start`

Fires when the user submits a prompt, initial or reply.

```ts
interface AgentStartEvent {
  thread: { id: `T-${string}` }
  message: string
  id: number | string
  changesWorkflow?: 'ship' | 'push_branch'
}

interface AgentStartResult {
  message?: { content: string; display?: boolean }
}
```

`changesWorkflow` identifies a turn started by Ship or Push to Branch. It is absent for typed prompts.

`id` is the user message ID: a stable string in current thread-actor threads, a number in legacy TUI threads. A returned `message` is appended after the user's content and shown in the UI only when `display` is true. Calling `ctx.thread.cancel()` during `agent.start` prevents the turn from starting.

#### `tool.call`

Fires before a tool executes. The handler returns the decision.

```ts
interface ToolCallEvent {
  thread: { id: `T-${string}` }
  toolUseID: string
  tool: string
  input: Record<string, unknown>
}

type ToolCallResult =
  | { action: 'allow' }
  | { action: 'reject-and-continue'; message: string }
  | { action: 'modify'; input: Record<string, unknown> }
  | { action: 'synthesize'; result: { output: string; exitCode?: number } }
  | { action: 'error'; message: string }
```

| `action` | Effect |
| --- | --- |
| `allow` | runs the tool with its original input |
| `reject-and-continue` | blocks this call and returns `message` to the agent, which continues |
| `modify` | runs the tool with `input` in place of the original |
| `synthesize` | skips execution and returns `result` as the tool output |
| `error` | stops the thread worker and shows an ephemeral error |

The event context carries no cancellation signal, and the API documents no deadline for an async handler.

#### `tool.result`

Fires after a tool executes and before its result goes back to the model.

```ts
interface ToolResultEvent {
  thread: { id: `T-${string}` }
  toolUseID: string
  tool: string
  input: Record<string, unknown>
  status: 'done' | 'error' | 'cancelled'
  error?: string
  output?: unknown
}

type ToolResultResult =
  | { status: 'done'; output?: unknown }
  | { status: 'error'; error?: string; output?: unknown }
  | { status: 'cancelled'; error?: string; output?: unknown }
  | undefined
  | void
```

Returning `undefined` keeps the native result. Returning an object replaces it.

#### `agent.end`

Fires when the agent finishes handling the prompt that started the turn.

```ts
interface AgentEndEvent {
  thread: { id: `T-${string}` }
  message: string
  id: number | string
  status: 'done' | 'error' | 'cancelled'
  messages: ThreadMessage[]
}

type AgentEndResult = { action: 'continue'; userMessage: string; maxContinuations?: number } | void
```

`message` and `id` identify the prompt that started the turn. `messages` holds every message since `agent.start`, including that prompt, in the [transcript schema](#transcript). Returning `continue` sends `userMessage` and starts another turn. `maxContinuations` defaults to 5 consecutive plugin continuations after the last user message. Amp ignores further continuations at that limit. A user message resets the count.

#### `changes.prompt`

Fires before a client shows the prompt for Ship or Push to Branch ([Plugin API](https://ampcode.com/docs/plugin-api)).

```ts
interface ChangesPromptEvent {
  thread: { id: `T-${string}` }
  workflow: 'ship' | 'push_branch'
}
type ChangesPromptResult = { append?: string } | void
```

Amp adds each plugin's `append` in plugin load order. The Ship dialog lets the user edit the assembled prompt before sending.

### Thread state and control

`ThreadState` is the agent activity state of one thread:

| State | Upstream meaning |
| --- | --- |
| `idle` | the agent is not working. The last turn, if any, has finished |
| `running` | inference or tool execution in progress, including a failed inference awaiting automatic retry |
| `awaiting-approval` | blocked waiting for a tool approval |
| `error` | the turn stopped with an error Amp will not retry automatically |

Each event context's `thread` is a `PluginThread`:

```ts
interface PluginThread {
  id: `T-${string}`
  agent(): Promise<Agent>
  parentThreadID(): Promise<ThreadID | null>
  readonly title: Observable<string | null> & { get(): Promise<string | null> }
  readonly state: Observable<ThreadState> & { get(): Promise<ThreadState> }
  waitForResponse(options?: { timeoutMs?: number }): Promise<ThreadAssistantMessage>
  cancel(): Promise<void>
  setVisibility(visibility: 'private' | 'workspace'): Promise<void>
  setMultiplayer(options: { ttlSeconds: number | null }): Promise<void>
  labels(): Promise<string[]>
  setLabels(labels: string[]): Promise<string[]>
  addLabels(labels: string[]): Promise<string[]>
  messages(options?: ThreadMessagesOptions): Promise<ThreadMessage[]>
  append(messages: UserMessage[]): Promise<void>
  appendUserMessage(message: UserMessage, options?: { steer?: boolean }): Promise<void>
}

interface UserMessage {
  type: 'user-message'
  content: string
}
```

| Member | Behaviour |
| --- | --- |
| `parentThreadID()` | the direct parent recorded when the thread was created with a `parentThreadID`, or `null`. Becomes `null` if the parent is deleted. Supported only for the plugin's current thread |
| `waitForResponse` | waits until the thread has been `running` or `awaiting-approval` and returns to `idle`, then resolves with the last assistant message. Rejects on `error` or timeout (default 10 minutes) |
| `cancel()` | stops the current turn |
| `labels()`, `setLabels()`, `addLabels()` | read alphabetically sorted labels, replace labels, or add labels |
| `setVisibility` | makes the thread private or workspace-visible. Private disables multiplayer. The user must own the thread |
| `setMultiplayer` | enables multiplayer for 5 minutes to 7 days, or disables it with `null`. Requires an orb or shared runner thread that is already shared |
| `appendUserMessage(..., { steer: true })` | when the thread is busy, queues the message as steering, preferred when the thread next dequeues work |

Labels are lowercased and deduplicated. A label starts with an alphanumeric character and contains only alphanumerics and hyphens, up to 32 characters. A thread holds at most 20 labels. Changes require permission to manage the thread ([Plugin API](https://ampcode.com/docs/plugin-api)).

`amp.activeThread` is an observable of the thread the user is focused on in the UI, with a synchronous `.current` snapshot of `{ id }` or `null`. Events for background threads still arrive, and the type docs direct plugins to compare `event.thread.id` with `amp.activeThread.current?.id` to tell the focused thread from a background one. `amp.threads.get(threadID)` returns a `PluginThread` for any thread ID.

### Transcript

`PluginThread.messages()` reads a thread in a stable plugin-facing schema.

```ts
interface ThreadMessagesOptions {
  full?: boolean
  from?: 'start' | 'end'
  offset?: number
  limit?: number
  roles?: Array<'user' | 'assistant'>
}
```

| Option | Default | Meaning |
| --- | --- | --- |
| `full` | `false` | `false` reads what the next inference turn sees: after a compaction, the latest compaction summary (as a user message) and the messages from the cut point on. `true` reads the whole transcript, compacted-away messages included |
| `from` | `'end'` | end of the thread to count from |
| `offset` | `0` | messages to skip from `from` |
| `limit` | `10` | clamped to 20. Page with `offset` |
| `roles` | all | filter to `user` and/or `assistant` |

```ts
type ThreadMessage = ThreadUserMessage | ThreadAssistantMessage | ThreadInfoMessage

interface ThreadUserMessage {
  role: 'user'
  id: number | string
  content: Array<
    | { type: 'text'; text: string }
    | { type: 'tool_result'; toolUseID: string; output?: PluginToolResult; status: 'done' | 'error' | 'cancelled' | 'running' | 'pending' }
  >
}

interface ThreadAssistantMessage {
  role: 'assistant'
  id: number | string
  content: Array<
    | { type: 'text'; text: string }
    | { type: 'thinking'; thinking: string }
    | { type: 'tool_use'; id: string; name: string; input: Record<string, unknown> }
  >
}

interface ThreadInfoMessage {
  role: 'info'
  id: number | string
  content: Array<{ type: 'text'; text: string }>
}

type PluginToolResult =
  | string
  | Array<
      | { type: 'text'; text: string }
      | { type: 'image'; mimeType: string; data: string }
      | { type: 'image'; mimeType: string; url: string }
    >
```

A `tool_use.id` in an assistant message pairs with a `tool_result.toolUseID` in a later user message. `amp.helpers.toolCallsInMessages(messages)` returns the completed pairs. The message types carry no timestamp, model, token usage, cost, or compaction marker, and Amp documents no local transcript file.

### Tools and helpers

Tool names vary with mode, plugins, MCP servers, and build. `amp tools list` prints the tools active for the current settings, and `amp plugins show-agent-options --json` prints the built-in names offered to plugin agents. On the baseline build `builtinToolNames` is:

```text
apply_patch ask_user_choice create_file create_thread edit_file find_thread finder get_thread_status
librarian list_agent_modes oracle painter Read read_mcp_resource read_thread
read_web_page send_thread_message shell_command shell_command_kill
shell_command_status skill Task view_media wait_for_threads web_search
```

The released declarations route MCP calls through `tool_search` and `code_exec` by default. Excluding both sends the selected MCP tools directly to the model instead ([Plugin API](https://ampcode.com/docs/plugin-api)). These two routing tools are absent from the baseline `show-agent-options` list above.

MCP tools are named `mcp__<server>__<tool>`. Plugin tools register under their bare names and also match `plugin__<pluginName>__<toolName>` in agent tool lists.

| Helper | Returns |
| --- | --- |
| `amp.helpers.shellCommandFromToolCall(call)` | `{ command, dir? }` for a `Bash` or `shell_command` call, else `null` |
| `amp.helpers.filesModifiedByToolCall(callOrResult)` | file URIs modified by an edit, create, or `apply_patch` tool or an in-place `sed` command, else `null` |
| `amp.helpers.filePathFromURI(uri)` | local filesystem path |
| `amp.helpers.toolCallsInMessages(messages)` | `{ call, result }` pairs of completed calls |
| `amp.helpers.isPluginUINotAvailableError(error)` | whether an error means no plugin UI is available |

### Rest of the Plugin API

The members below are outside the lifecycle path. Their full declarations are in `amp plugins show-docs`.

| Member | Purpose |
| --- | --- |
| `amp.registerTool(definition)` | adds a tool. Returning nothing produces `(no tool output)`. Throwing reports a tool error |
| `amp.registerLinkPattern(definition)` | turns matching tokens into transcript links, retained for later readers without an executor |
| `amp.registerCommand(id, options, handler)` | adds a command palette action with enabled, disabled, or hidden availability |
| `amp.registerSkill({ path })` | registers a skill directory of a directory plugin as `<plugin-name>:<skill-name>` |
| `ctx.ui.notify`, `input`, `confirm`, `select` | dialogs. `confirm` also takes editable `fields`, `select` takes `allowOther` |
| `amp.ai.generate`, `amp.ai.ask` | thread-billed text, structured object, or yes/no classification |
| `amp.ai.decide`, `choice`, `noul`, `score` | typed decisions and probabilities, with `typesafe/jev` by default or `openai/gpt-6-luna` explicitly |
| `amp.attachments.upload` | uploads image bytes (at most 4.9 MB decoded, 8000 px per side) and returns a URL |
| `amp.configuration` | observable settings with `get`, `update`, and `delete` against workspace or global targets |
| `amp.system` | `open(url)`, `workspaceRoot`, `ampURL`, `user`, `executor` ([identity](#authentication-account-and-usage)) |
| `amp.$` | tagged-template shell runner resolving `{ exitCode, stdout, stderr }` |
| `amp.createAgent(config)`, `amp.getBuiltinAgent(mode)` | agent handles that create threads or run one-shot turns ([modes](#models-modes-and-effort)) |
| `amp.registerAgentMode(definition)` | adds a plugin mode to mode pickers. Needs a matching `// @amp-agent-mode` comment |
| `amp.createWebhook(options)` | durable at-least-once webhook for a plugin in an Amp-managed orb |
| `amp.experimental` | unstable APIs, `createStatusItem`, and compatibility aliases |
| `defineAgent`, `defineTool` | static config for declarative agent directories (`agent.ts`, `tools/<name>.ts`), including `draft` and remote `mcpServers` declarations |

## Private local thread cache

Amp has at some builds written one JSON object per thread under `${AMP_DATA_DIR:-~/.local/share/amp}/threads/T-*.json`. The file is an implementation detail outside the Plugin API and the docs. The schema evidence is ccusage's Amp adapter at commit [`ba99c0d`](https://github.com/ryoppippi/ccusage/tree/ba99c0d09b6db9fd64a6187751e8b88a019f991a/rust/crates/ccusage/src/adapter/amp) and objects captured into RimZ's adapter fixtures. `AMP_DATA_DIR` is anchored there only.

Whether the current build writes this file is unverified. On the host that ran `0.0.1789344113-g6e4515` on 2026-09-14, `~/.local/share/amp/` held `device-id.json`, `history.jsonl`, `secrets.json`, and `session.json` and no `threads/` directory, while `amp threads list` showed two threads.

The file is rewritten whole on each update. Its root carries `id`, `messages`, and optionally `usageLedger.events`.

| Path | Shape |
| --- | --- |
| `id` | the `T-…` thread ID |
| `messages[].id` / `messageId` | string or number |
| `messages[].content` | a string, or the block array of the [transcript schema](#transcript) |
| `messages[].usage` (assistant, current objects) | `model`, `timestamp`, `inputTokens`, `outputTokens`, `cacheCreationInputTokens`, `cacheReadInputTokens`, sometimes `totalTokens` |
| `usageLedger.events[]` (legacy objects) | `id`, `timestamp`, `model`, `toMessageId`, `tokens.{input,output,total}` |

A legacy ledger event's `toMessageId` references an assistant message's `messageId`, whose `usage` holds the cache creation and cache read counts the ledger omits. RimZ's parsing rules for this file are in [adapter_amp.md](../../internals/agents/adapter_amp.md#context-and-transcript).

## Permissions

Amp runs tools without asking for approval by default. The docs direct users to a plugin for control over tool use ([tools](https://ampcode.com/docs/tools#permissions)): a `tool.call` handler decides, and `ctx.ui.confirm`, `input`, or `select` asks the user. While a tool approval is open, the thread state is `awaiting-approval`. The state carries no tool name, arguments, question, options, or resolver.

Amp has no native plan-approval event and no dedicated user-question event. The current built-in tool list includes `ask_user_choice`. A plugin can register its own question tool with `amp.registerTool` and `ctx.ui`, which adds that tool to the agent's tool set.

Plugin dialogs accept `requireHuman` and `skipIfUnwatched` ([Plugin API](https://ampcode.com/docs/plugin-api)). The latter resolves as dismissed (`undefined`) when nobody eligible has the thread open. Terminal users always count as present. Secret inputs and confirmations with secret fields always require the owner and a human response. Non-secret dialogs can also accept human contributors while multiplayer is active. Secret answers return only to the requester, without entering thread history or the model transcript.

`ui.input` supports `secret`, `fieldName`, and `placeholder`. Confirmation fields support `type: 'text' | 'image' | 'link'`, `optional`, `placeholder`, and `secret`. Image and link fields are omitted from returned values.

The baseline binary ships permission rules, which the current docs do not describe. `amp --help` lists the settings `amp.permissions` ("Permission rules for tool calls"), `amp.guardedFiles.allowlist`, and `amp.dangerouslyAllowAll` ("Disable all command confirmation prompts"), and the command `amp permissions` with `list`, `test`, `edit`, and `add`. The rules as published at [notes/permissions](https://ampcode.com/notes/permissions):

| Aspect | Behaviour |
| --- | --- |
| storage | the `amp.permissions` settings key |
| matching | Amp checks the rules in order before every tool call and applies the first match |
| actions | `allow`, `reject`, `ask`, `delegate` |
| `delegate` | runs a helper program with the tool parameters as JSON on stdin. `AGENT_TOOL_NAME` names the tool |
| delegate exit code | `0` allows, `1` asks the user, `2` rejects with stderr shown to the model |

`amp.mcpPermissions` allows or blocks MCP servers by pattern ([settings](https://ampcode.com/docs/cli/settings)). The baseline help does not list it.

## Compaction and context

Amp compacts a thread automatically when its estimated input tokens cross a threshold: a configured percentage of the context window, 90% by default. A custom plugin agent can set an absolute `compactionThresholdTokens` instead. A system model writes the summary ([models and subagents](https://ampcode.com/docs/models-and-subagents)).

No plugin event marks compaction, and plugin thread messages carry no context percentage or token count. `amp threads context <T-id>` (alias `analyze-context`) prints estimated input context as JSON. Its schema is undocumented in the [CLI reference](https://ampcode.com/docs/cli). `messages()` shows the effect afterwards: the default view starts at the latest summary, which is an ordinary user message, and `{ full: true }` includes the discarded context. Per-response token counts are available in [stream JSON](#supervised-runs-and-stream-json), and each model's `contextWindow` is listed by `amp plugins show-agent-options --json`.

## Models, modes, and effort

The dial has four built-in modes: `low`, `medium`, `high`, and `ultra`. Each mode combines a model, reasoning effort, system prompt, tools, and Oracle, and Amp changes the backing models over time. Settings → Mode Dial lets a user or workspace admin pick the model and effort behind each mode's main agent, Oracle, and subagents, and put plugin agents on the dial. Personal choices take precedence over the workspace dial ([build-your-own-dial](https://ampcode.com/news/build-your-own-dial), 2026-09-10). `Ctrl+S` switches modes in the CLI.

| Flag | Values |
| --- | --- |
| `-m, --mode <value>` | `low`, `medium`, `high`, `ultra`, or a plugin mode by key or label, case-insensitive. Controls the model, system prompt, and tool selection |
| `--features <value>` | comma-separated `fast`, `plaid`, or `pro`. `plaid` enables premium ultrafast GPT-6 Astra serving and implies Fast on other models. `pro` selects GPT-5.6 Pro (OpenAI API only) |
| `--fast` | alias for `--features fast`; `amp threads continue` accepts it too, but fast is fixed when a thread is created, so it does not change a continued thread (`amp --help`, `amp threads continue --help`, 0.0.1791792883, read 2026-10-13) |

Plaid requires Amp-provided inference. Linked ChatGPT subscriptions do not support it ([Plaid Speed](https://ampcode.com/news/plaid-mode)).

The Claude Code mode runs the Claude Agent SDK with a linked Claude Pro/Max subscription ([announcement](https://ampcode.com/news/use-your-claude-plan), 2026-10-10). `amp settings external-agents` lists, shows, updates, activates, deactivates, or resets its configuration. The four dial modes remain the built-in `BuiltinAgentMode` values for new native Amp threads. This refresh does not establish Plugin API event parity for the external Claude Code mode.

The baseline `amp --help` lists no `--effort` flag, and no docs page read for this baseline documents one. The 2026-09-14 build rejects it: `amp --effort high version` prints `error: unknown option '--effort'` and exits 1 (checked 2026-09-14).

The modes `smart`, `deep`, and `rush` are deprecated. Existing threads in them keep working. New threads spawned from them start in the replacement mode: `rush` becomes `low`, `smart` and `deep` become `medium`.

`thread.agent()` returns an `Agent` whose `definition` is one of two shapes:

```ts
type BuiltinAgentMode = 'low' | 'medium' | 'high' | 'ultra' | 'smart' | 'deep' | 'rush'
type AgentReasoningEffort = 'none' | 'minimal' | 'low' | 'medium' | 'high' | 'xhigh' | 'max'

interface BuiltinAgentDefinition {
  readonly kind: 'builtin-agent'
  mode: BuiltinAgentMode
}

interface CustomAgentDefinition extends CreateAgentConfig {
  readonly kind: 'agent-definition'
  model: `${string}/${string}`
  instructions: string
}

interface CreateAgentConfig {
  name?: string
  extends?: BuiltinAgentMode
  model?: `${string}/${string}`
  instructions?: string
  tools?: AgentToolSelection
  reasoningEffort?: AgentReasoningEffort
  oracle?: { model?: `${string}/${string}`; effort?: AgentReasoningEffort }
  subagents?: { model?: `${string}/${string}`; effort?: AgentReasoningEffort }
  compactionThresholdTokens?: number
  features?: readonly ('fast' | 'plaid' | 'pro' | string)[]
  display?: { label: string; color?: string }
}
```

A built-in definition names the mode and carries no model or reasoning effort. Custom agents extending a mode use its defaults, independently of the user's Tune Modes overrides. A custom definition carries its resolved `model` (filled from the extended mode when `extends` is set and `model` is omitted) and any `reasoningEffort` it declares. The efforts a model accepts are in `capabilities.efforts` of `amp plugins show-agent-options --json`. Stream JSON `system/init` carries `agent_mode` and no effort field.

## Subagents and concurrent threads

Amp delegates work to built-in subagents: Search, Oracle, Librarian, and Read Thread. Each works in its own context, cannot be guided mid-task, and returns only a final summary to the main agent ([models and subagents](https://ampcode.com/docs/models-and-subagents)). The Plugin API has no subagent start or stop event.

`PluginThread.parentThreadID()` returns the parent of a thread created with a `parentThreadID`, for the plugin's current thread only. Plugin agents create such threads through `Agent.createThread({ parentThreadID })` or `Agent.run(message, { parentThreadID })`, which resolves with `{ threadID, text }`. Whether built-in subagent work runs in a thread with a recorded parent, and whether it fires plugin events, is undocumented.

Agents can also coordinate persistent child threads through the tools `create_thread` (with a per-call `agent_mode`), `list_agent_modes`, `get_thread_status`, `send_thread_message`, and `wait_for_threads`. `Task` instead runs a scoped subagent without a per-call mode.

In stream JSON, assistant and user messages carry `parent_tool_use_id`, which is `null` when the message has no parent tool use. The current docs do not say which tool use a subagent's messages point at.

One Amp process can host several threads: `amp.activeThread` names the focused thread while events for background threads keep arriving, and `amp.remoteThreadCreation.enabled` opens remotely created threads in a running TUI. Amp removed the TUI sidebar for switching between threads on 2026-08-27 ([so-long-tui-sidebar](https://ampcode.com/news/so-long-tui-sidebar)). The post does not say how the TUI changes focus afterwards.

## Supervised runs and stream JSON

`amp -x, --execute [message]` runs one turn without the TUI. The prompt comes from the argument or stdin. Amp prints the last assistant message and exits. Execute mode reads stdin even when the prompt is an argument, so a non-interactive `amp -x "<prompt>"` with nothing piped fails with `Timeout while reading from stdin`. Redirect stdin from `/dev/null` (checked on 0.0.1789344113, 2026-09-14). Redirecting stdout turns execute mode on. Execute mode archives a new thread when it finishes. `--no-archive-after-execute` leaves it unarchived (the flag applies to `amp review` too). The 2026-09-14 build refuses to continue an archived thread: `amp threads continue <T-id> -x …` prints `This thread is archived and cannot be continued.` (checked 2026-09-14).

```sh
amp --execute "Summarize this repository" --stream-json --plugin-ready-timeout 30 </dev/null
```

| Flag | Effect |
| --- | --- |
| `--stream-json` | with `--execute`, prints Claude Code-compatible stream JSON instead of plain text |
| `--stream-json-thinking` | adds `thinking` and `redacted_thinking` blocks. Implies `--stream-json`. Not Claude Code-compatible |
| `--stream-json-input` | reads JSON Lines user messages from stdin. Requires `--execute` and `--stream-json` |
| `--plugin-ready-timeout [seconds]` | waits for plugins before the turn ([readiness](#runtime-and-execute-mode-readiness)) |
| `--no-archive-after-execute` | leaves the new thread unarchived |
| `--attach <path>` | uploads a local file for remote execute mode and adds its URL to the prompt. Repeatable |
| `--title <title>` | sets the new thread's title before the agent starts |
| `-l, --label <label>` | labels the created or continued thread. Repeatable |
| `-ox, --orb-execute` | runs the prompt in an orb on Amp's servers ([executors](#remote-control-runners-and-executors)) |

Each line of `--stream-json` output is one message, and every message carries `session_id`, the Amp thread ID ([streaming-json](https://ampcode.com/docs/cli/streaming-json)):

```ts
type Usage = {
  input_tokens: number
  cache_creation_input_tokens?: number
  cache_read_input_tokens?: number
  cache_creation?: { ephemeral_5m_input_tokens: number; ephemeral_1h_input_tokens: number }
  output_tokens: number
  max_tokens?: number
  service_tier?: 'standard' | 'enterprise'
}

type StreamJSONMessage =
  | {
      type: 'system'
      subtype: 'init'
      cwd: string
      session_id: string
      tools: string[]
      mcp_servers: {
        name: string
        status: 'awaiting-approval' | 'authenticating' | 'connecting' | 'reconnecting' | 'connected' | 'denied' | 'failed' | 'blocked-by-registry'
      }[]
      agent_mode?: string
    }
  | {
      type: 'assistant'
      message: {
        type: 'message'
        role: 'assistant'
        content: Array<
          | { type: 'text'; text: string }
          | { type: 'tool_use'; id: string; name: string; input: Record<string, unknown> }
          | { type: 'thinking'; thinking: string }
          | { type: 'redacted_thinking'; data: string }
        >
        stop_reason: 'end_turn' | 'max_tokens' | 'stop_sequence' | 'tool_use' | 'pause_turn' | 'refusal' | null
        usage?: Usage
      }
      parent_tool_use_id: string | null
      session_id: string
    }
  | {
      type: 'user'
      message: {
        role: 'user'
        content: Array<
          | { type: 'text'; text: string }
          | { type: 'tool_result'; tool_use_id: string; content: string; is_error: boolean }
        >
      }
      parent_tool_use_id: string | null
      session_id: string
    }
  | { type: 'result'; subtype: 'success'; duration_ms: number; duration_api_ms?: number; is_error: false; num_turns: number; result: string; session_id: string; usage?: Usage; permission_denials?: string[] }
  | { type: 'result'; subtype: 'error_during_execution' | 'error_max_turns'; duration_ms: number; duration_api_ms?: number; is_error: true; num_turns: number; error: string; session_id: string; usage?: Usage; permission_denials?: string[] }
  | { type: 'system'; subtype: 'error_max_turns' | 'error_during_execution'; error: string; session_id: string }
```

`usage` on an assistant message counts that one response. The schema has no dollar cost, model ID, or transcript path, and the docs state no process exit codes.

`--stream-json-input` reads messages of this shape until stdin closes:

```ts
type StreamJSONInputMessage = {
  type: 'user'
  steer?: boolean
  message: {
    role: 'user'
    content: Array<
      | { type: 'text'; text: string }
      | {
          type: 'image'
          source_path?: string
          source: { type: 'base64'; media_type: 'image/jpeg' | 'image/png' | 'image/gif' | 'image/webp'; data: string }
        }
    >
  }
}
```

`steer: true` delivers the message at the agent's next pause point. Amp exits only after the assistant is done and stdin has closed. In the TUI, a message the user sends while the agent works is delivered at the next opportunity during the turn. Built-in actions such as Ship and Review queue until the turn ends ([steer-dont-queue](https://ampcode.com/news/steer-dont-queue)).

## Remote control, runners, and executors

Cross-client access lets a user continue a running CLI thread from ampcode.com on desktop or mobile ([remote control](https://ampcode.com/docs/cli/remote-control)). Workspace admins turn it off in Member Settings, which also stops members starting threads on their runners from outside the CLI. "Require Passkey Authentication for Web & App Interaction" adds passkey verification, per user or enforced by Enterprise admins.

| Control | Behaviour |
| --- | --- |
| `--remote-control-terminal` / `--no-remote-control-terminal` | allows or denies terminal access from ampcode.com. The flag overrides the environment |
| `AMP_REMOTE_CONTROL_TERMINAL` | `1` enables, `0` disables when no flag is given. Terminal access is off by default |
| `amp.remoteThreadCreation.enabled` | default `false`. Lets ampcode.com create threads that open in the interactive TUI on this machine, in the directory where it started. Palette command `amp: enable remote creation of threads` |
| `amp --no-tui` | runner mode: serves remotely created threads without a TUI. Serves the current directory by default |
| `--dir <path>` | adds a served directory. Repeatable |
| `--discover-dirs[=<path>]` | discovers Git checkouts beneath the given path, or the current directory. Default depth: 2 |
| `--discover-depth <levels>` | discovery depth, 1 to 10. Alone, enables discovery beneath the current directory |
| `--discover-exclude <pattern>` | excludes directories by `.gitignore`-style pattern. Repeatable. Alone, enables discovery |
| `--no-serve-cwd` | stops serving the starting directory implicitly. Explicit directory selections still apply |
| `--runner-id <id>` | stable runner ID. Must be a valid hostname. Case-insensitive, casing preserved ([runners](https://ampcode.com/docs/cli/runners)) |
| `--runner-dir <path>` | selects an absolute directory served by the runner. Defaults to its starting directory. Ignored when continuing a thread |
| `--share` | shares a `--no-tui` runner with the workspace while running |
| `--desktop` | shares the Mac display or a private Linux virtual desktop with runner threads |
| `--amp-env` | injects Amp secrets and environment variables into runner shell commands, MCP servers, and plugins. Overrides process variables. Shared runners exclude personal values |
| `--executor <local \| orb \| runner:<id>>` | where a new thread runs: this client, an Amp-managed orb, or a live runner |
| `-ox, --orb-execute`, `--orb-size`, `--project` | execute in an orb, with an orb size and an Amp project other than the one inferred from Git remotes |

The directory flags above appear in the [runner docs](https://ampcode.com/docs/cli/runners) but are absent from top-level help. Use `--discover-dirs=<path>` to avoid consuming a following subcommand as its optional path.

`amp runner list` lists local runners. `amp runner dirs add`, `remove`, and `list` change or inspect served directories without restarting. Added directories persist across runner restarts. `amp runner desktop status` checks readiness. `amp runner desktop allow` requests macOS permissions ([runners](https://ampcode.com/docs/cli/runners)).

A shared runner runs workspace members' code with the owner's files, credentials, and logins ([shared runners](https://ampcode.com/news/shared-runners)). Concurrent threads can use the same directories. Restart without `--share` to stop sharing and disable multiplayer on its threads. With `--amp-env`, shared runners receive workspace and project values, never personal values, including for the owner's threads. The general `--amp-env` help omits this documented exception.

Runners can create worktrees through the directory picker or `create_thread` with `executor: "runner"`, `runner_id`, and `worktree` ([runner docs](https://ampcode.com/docs/cli/runners)). Amp creates a sibling directory and a branch with the requested name. It fetches the configured upstream first, preserving local commits ahead of upstream. Failed fetches and diverged branches stop creation. Branches without upstreams and detached checkouts use their current HEAD. The original checkout stays unchanged.

"Archive and Remove Worktree" archives the thread, runs `git worktree remove`, and deletes the branch without force. Uncommitted or untracked changes keep the worktree. Unmerged commits keep the branch. An offline runner leaves the worktree on disk, and unarchiving does not recreate it.

`Agent.createThread` and `Agent.run` accept `project` with an orb executor ([Plugin API](https://ampcode.com/docs/plugin-api)). It selects the project's repository, secrets, orb settings, and workspace. Do not take this value from untrusted input.

`amp orb provider` runs self-hosted orbs by pulling lifecycle jobs over HTTPS, according to the released CLI help. `amp orb redact-output` copies stdin to stdout with known tokens, thread secrets, and variables named by `AMP_SECRET_ENV_NAMES` redacted.

GitHub listeners can deliver comments, reviews, and CI results to an existing thread or start orbs for new issues and pull requests ([announcement](https://ampcode.com/news/github-listeners)). This server integration is separate from plugin webhooks.

Plugin webhooks have a 30-second handler deadline. Failures retry after 5 seconds, with exponential backoff capped at 5 minutes, for up to one hour. Later events can run while an earlier event waits. Archiving the owner thread makes its webhook URL unavailable and pauses queued delivery until restoration ([Plugin API](https://ampcode.com/docs/plugin-api)).

Plugin code sees only `amp.system.executor.kind`, one of `local`, `remote`, or `unknown`. It carries no runner ID, pane, PID, or cwd. `executor.keepAlive()` holds an orb awake and rejects outside an orb. Plugin agents choose an executor per thread with `executor: 'local' | 'orb' | { type: 'runner'; id }`.

## Authentication, account, and usage

Amp saves accounts separately for each server ([CLI](https://ampcode.com/docs/cli)). A running process keeps the account it started with. Switching accounts affects future processes only.

| Command | Effect |
| --- | --- |
| `amp login` | adds an account and activates it for the current server |
| `amp account list` | lists saved accounts |
| `amp account switch <email-or-user-id>` | selects the account for future CLI commands |
| `amp logout` | removes the active saved account, without selecting another |
| `amp logout <email-or-user-id>` | removes a specific saved account |
| `amp logout --all` | removes all saved CLI accounts across all Amp servers |

Logout leaves browser and native app sessions signed in. `AMP_API_KEY` takes precedence over saved accounts and remains active until unset. It must be an access token from Settings starting with `sgamp_`. The CLI rejects the short-lived session token stored by `amp login`, which expires within an hour ([execute mode](https://ampcode.com/docs/cli/execute-mode)). `AMP_URL` selects the service, `https://ampcode.com/` by default.

Stored credentials live in `~/.local/share/amp/secrets.json`, or `%USERPROFILE%\.local\share\amp\secrets.json` on Windows ([security](https://ampcode.com/security)). A running plugin reads identity from `amp.system`:

```ts
interface PluginSystem {
  open(url: string | URL): Promise<void>
  readonly workspaceRoot: URI | null
  readonly ampURL: URL
  readonly user: User | null
  readonly executor: { readonly kind: 'local' | 'remote' | 'unknown'; keepAlive(): Promise<Subscription> }
}

interface User {
  readonly id: string
  readonly email: string
  readonly firstName: string | null
  readonly lastName: string | null
  readonly username: string | null
  readonly workspace: { id: string; name: string; displayName: string | null } | null
}
```

`user` is `null` when Amp is not authenticated. `workspaceRoot` is stable for the plugin process. Plugins reload when the workspace changes.

| Command | Output |
| --- | --- |
| `amp usage` | current usage and credit balance, as human text. `--details` adds credit, token, and thread usage, and `--start` and `--end` take ISO 8601 bounds |
| `amp threads usage <T-id>` | human-readable usage. `--details` adds credits, tokens, models, and tools. `--start` and exclusive `--end` take ISO 8601 bounds |
| `amp threads context <T-id>` | estimated input context as JSON, with an undocumented schema |
| `amp threads export <T-id>` | the thread as JSON, undocumented schema |
| `amp threads raw <T-id>` | raw actor thread data as JSON. In the baseline help only |
| `amp threads markdown <T-id>` | the thread as Markdown |
| `amp threads import detect`, `codex`, `claude` | detects or imports other agents' conversations as read-only Amp threads. `claude-code` aliases `claude` |
| `amp top --stream-jsonl` | a JSON line whenever the live thread list changes. Schema marked experimental |

No usage command documents a JSON output mode, and the baseline `amp usage --help` lists none. Plugin events and messages carry no cost.

Amp is free with your own compute and model subscriptions or keys, except for Enterprise terms ([Free Agent](https://ampcode.com/news/free-agent)). Paid tiers include monthly usage. A user who exhausts it can add paid credits ([pricing](https://ampcode.com/docs/pricing)). Non-model tools such as web search also consume credits. Orbs have a separate usage allowance. For non-enterprise accounts Amp deducts actual model cost from credits without markup, workspace members share paid workspace credits, and purchased credits expire twelve months after purchase. Enterprise uses pooled credits and custom pricing.

## Settings and environment

Amp reads JSON or JSONC settings. Every key has the `amp.` prefix ([settings](https://ampcode.com/docs/cli/settings)).

| Layer | Location |
| --- | --- |
| managed (Enterprise) | `/etc/ampcode/managed-settings.json` (Linux), `/Library/Application Support/ampcode/managed-settings.json` (macOS), `%ProgramData%\ampcode\managed-settings.json` (Windows) |
| workspace | nearest `.amp/settings.json` or `.amp/settings.jsonc` found searching upward from the cwd |
| user | `~/.config/amp/settings.json` or `.jsonc`. `--settings-file <path>` or `AMP_SETTINGS_FILE` replaces the path |

Managed settings enforce policy, and workspace settings override user settings. User `amp.keymap` entries are the exception and override workspace entries. The managed-only `amp.admin.compatibilityDate` (`YYYY-MM-DD`) selects which backward-compatibility migrations apply.

| Setting | Effect |
| --- | --- |
| `amp.permissions`, `amp.guardedFiles.allowlist`, `amp.dangerouslyAllowAll` | [permission rules](#permissions) |
| `amp.mcpServers` | MCP servers, as local commands or remote URLs, exposed as tools |
| `amp.mcpPermissions` | allow or block MCP servers by pattern |
| `amp.tools.disable`, `amp.tools.enable` | remove tools, or allow only matching tool patterns |
| `amp.experimental.modes` | enable experimental agent modes by name |
| `amp.remoteThreadCreation.enabled` | [remote thread creation](#remote-control-runners-and-executors) |
| `amp.runner.autoUpdate.enabled` | default `true`. Idle runners install updates and restart at most once every 12 hours. `amp.updates.mode = "disabled"` disables this |
| `amp.runner.env.enabled` | default `false`. Same secret injection as `--amp-env` |
| `amp.defaultVisibility` | default thread visibility per repository origin |
| `amp.thread.autoArchiveOnQuit` | archive open CLI threads on quit. Default `false` |
| `amp.showCosts` | show costs while working. Default `true` |
| `amp.notifications.enabled`, `amp.notifications.system.enabled` | completion alerts and system notifications |
| `amp.skills.path`, `amp.skills.disableClaudeCodeSkills` | extra skill directories, with relative paths resolved against the workspace root. Ignore `.claude` skill directories |
| `amp.skills.disableGlobalAgentsSkills` | default `false`. Ignores `~/.config/agents/skills/` and `~/.agents/skills/`, without affecting workspace `.agents/skills/` |
| `amp.git.commit.ampThread.enabled`, `amp.git.commit.coauthor.enabled` | `Amp-Thread` trailer and co-author trailer in commits |
| `amp.proxy`, `amp.network.timeout` | proxy URL and request timeout in seconds for the Amp server |
| `amp.updates.mode` | [update checking](#version-and-distribution) |

Several surfaces execute commands on the user's machine: project plugins in `.amp/plugins/`, `amp.mcpServers` commands, a `delegate` rule's helper program, and plugin install URLs. Amp asks for approval before starting a workspace MCP server (`amp mcp approve <name>`, with `amp mcp doctor` for status). Servers from user settings or `--mcp-config` skip that approval.

| Variable | Effect |
| --- | --- |
| `AMP_API_KEY` | [access token](#authentication-account-and-usage) |
| `AMP_URL` | Amp service URL |
| `AMP_SETTINGS_FILE` | user settings path |
| `AMP_LOG_LEVEL`, `AMP_LOG_FILE` | log level and log file. The file defaults to `~/.cache/amp/logs/cli.log` |
| `AMP_REMOTE_CONTROL_TERMINAL` | [terminal access](#remote-control-runners-and-executors) |
| `AMP_FORCE_BEL` | notifications use the terminal bell |
| `AMP_SKIP_UPDATE_CHECK` | `1` skips the update check |
| `AMP_DISABLE_AMP_THREAD_TRAILER`, `AMP_DISABLE_AMP_COAUTHOR_TRAILER` | suppress the commit trailers |
| `NO_ANIMATION` | `1` disables terminal animations |
| `HTTP_PROXY`, `HTTPS_PROXY`, `NODE_EXTRA_CA_CERTS` | corporate proxy and CA certificates |
| `AMP_DATA_DIR` | data root of the [private thread cache](#private-local-thread-cache). Anchored by ccusage only |

## Undocumented behaviour

Neither the docs nor the baseline binary settle these. Each needs a capture against a pinned build before code depends on it.

1. How long a `tool.call` handler may wait, how Amp cancels it, and what happens when the plugin process exits mid-call.
2. How `tool.call` results from several plugins compose, and what a handler exception does.
3. Whether `awaiting-approval` brackets a plugin's `ctx.ui.confirm` and a permission rule's `ask` the same way, and its order relative to `tool.call`.
4. Whether built-in subagent work fires plugin events, and under which thread ID.
5. *(Captured 2026-09-14 on `0.0.1789344113-g6e4515`.)* A system plugin runs in a child process, `amp run <tmp>/amp-plugin-runtime-*/plugin-runtime.ts`, whose parent is the Amp CLI. The plugin's `process.pid` is that runtime and `process.ppid` is the CLI. No payload carries a PID or pane.
6. Execute-mode exit codes for success, error, cancellation, permission denial, and plugin-readiness expiry.
7. The output formats of `amp usage`, `amp threads usage`, `amp threads context`, `amp threads export`, and `amp threads raw`.
8. *(Captured 2026-09-14 on `0.0.1789344113-g6e4515`: the CLI rejects `--effort` as an unknown option.)*
9. *(Captured 2026-09-14 on `0.0.1789344113-g6e4515`: the CLI refuses archived threads.)*
10. How the TUI changes the focused thread since the sidebar's removal, and whether a focus change still fires `session.start`.
11. Whether the baseline build still writes the [private thread cache](#private-local-thread-cache).
