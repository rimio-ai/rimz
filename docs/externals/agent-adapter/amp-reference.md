# Amp CLI protocol reference

> RimZ's mapping of this surface is [adapter_amp.md](../../internals/agents/adapter_amp.md). The agent-agnostic lifecycle contract is [model.md](../../internals/agents/model.md), and the account and spend contract is [providers.md](../../internals/agents/providers.md).

This page records the upstream Amp CLI surfaces a RimZ adapter binds to: the Plugin API and its events, thread state, transcripts, tools, permissions, compaction, modes, subagents, execute mode and stream JSON, remote control and runners, authentication and usage, and settings. It covers the current Amp architecture only (the one Amp's May 2026 launch posts called "Neo"): the dial modes are `low`, `medium`, `high`, and `ultra`, and pre-rebuild hooks, local thread schemas, and `--take-me-back` are out of scope.

Refresh baseline: `@ampcode/cli` [0.0.1789344113-g6e4515](https://www.npmjs.com/package/@ampcode/cli/v/0.0.1789344113-g6e4515), the npm `latest` dist-tag and the version pinned by the Homebrew formula in [ampcode/homebrew-tap](https://github.com/ampcode/homebrew-tap/blob/main/Formula/ampcode.rb), released 2026-09-14T00:01:53Z. Amp ships rolling builds and publishes no source or release tags, so the baseline is the newest published build. The docs at <https://ampcode.com/docs> are unversioned; they, the release news, and the installed baseline binary were read 2026-09-13. Plugin API types on this page are quoted from `amp plugins show-docs` of the baseline build, whose header carries `sitemap-lastmod: /docs/plugin-api 2026-09-01`. Where the docs and the binary disagree, the page follows the binary and flags the disagreement at the claim.

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
| `amp --help` | every command, global flags, environment variables, and a settings reference |
| `amp plugins show-docs` | the `@ampcode/plugin` declarations this build understands |
| `amp plugins show-agent-options --json` | `models[]` (`id`, `provider`, `contextWindow`, `maxOutputTokens`, `capabilities.efforts`) and `builtinToolNames[]` |
| `amp tools list` | active tools, including MCP tools |

On the baseline build, `amp <command> --help` prints that command's help, but a nested `amp <command> <subcommand> --help` (for example `amp threads usage --help`) prints the top-level help instead.

## Version and distribution

`amp version` prints the build token, then a parenthesized release annotation:

```text
0.0.1789344113-g6e4515 (released 2026-09-14T00:01:53.000Z, 3h ago)
```

The number after `0.0.` is the release time in Unix seconds. The relative age (`3h ago`) is part of the annotation.

Amp installs through the shell installer (`curl -fsSL https://ampcode.com/install.sh | bash`), the Homebrew formula `ampcode/tap/ampcode`, or the npm package `@ampcode/cli`, which Amp supports but does not recommend ([npm-package-changes](https://ampcode.com/news/npm-package-changes)). The CLI checks for and installs new builds in the background ([docs/cli](https://ampcode.com/docs/cli)).

| Update control | Behaviour |
| --- | --- |
| `amp update` (alias `up`) | installs the latest release; `--target-version <version>` installs a specific one |
| `amp update --porcelain` | prints `updated <version>` or `no update needed` to stdout |
| `amp.updates.mode` | `auto` runs the update, `warn` shows a notification, `disabled` turns checking off |
| `AMP_SKIP_UPDATE_CHECK=1` | skips the update check |

## Surface index

Amp has no command-hook protocol, statusline feed, or interactive stdio RPC: neither the docs nor the baseline `amp --help` describe one. An in-process plugin is the interactive integration seam, and execute mode with stream JSON is the scripted one.

| Concern | Upstream surface | What it lacks |
| --- | --- | --- |
| thread identity | plugin event `thread.id` (`T-${string}`); stream JSON `session_id` | none |
| turn start and end | [`agent.start`](#agentstart) and [`agent.end`](#agentend) | execute mode skips them unless plugins are ready ([readiness](#runtime-and-execute-mode-readiness)) |
| tool activity | [`tool.result`](#toolresult) | none |
| file edits | `amp.helpers.filesModifiedByToolCall` | covers edit, create, `apply_patch`, and in-place `sed` only |
| tool decision | [`tool.call`](#toolcall) | handler timeout, cancellation, and multi-plugin composition are undocumented |
| permission wait | [`ThreadState`](#thread-state-and-control) `awaiting-approval` | no tool, arguments, question, or resolver |
| plan approval, user question | none | no event or state |
| compaction | none | no event, percentage, or summary marker ([compaction](#compaction-and-context)) |
| subagents | `PluginThread.parentThreadID()`; stream JSON `parent_tool_use_id` | no subagent start or stop event ([subagents](#subagents-and-concurrent-threads)) |
| mode, model, effort | `thread.agent()`; stream JSON `agent_mode` | a built-in agent definition carries the mode only |
| context usage | stream JSON assistant `usage` | no interactive API; plugin messages carry no usage |
| cost | `amp usage`, `amp threads usage` | human text only |
| transcript | [`PluginThread.messages()`](#transcript) | no documented local file |
| steering | `appendUserMessage(..., { steer: true })`; stream JSON input `steer` | none |
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
| `amp plugins add <url>` | installs a single-file plugin from a URL; `--target workspace` installs it into `.amp/plugins/` |
| `amp plugins remove` (alias `rm`) | removes an installed plugin; in the baseline help, absent from the docs |
| `amp plugins update [<name>]` | updates installed plugins or one imported shared plugin |
| `amp plugins list` (alias `ls`) | lists loaded plugins of all four scopes |
| `amp plugins import` | imports a shared personal plugin into a plugin repository checkout |
| `amp plugins repositories` (alias `repos`) | lists Personal Plugins and Workspace Plugins repositories; `amp clone` checks one out |
| `amp plugins show-agent-options` | models and built-in tool names for plugin agents |
| `amp plugins exec` | executes a plugin with a given event |
| `amp plugins show-docs` | the plugin API declarations of this build |
| palette `plugins: reload` | reloads plugins; the guide also names `amp plugins reload`, which the baseline help does not list |

### Runtime and execute-mode readiness

A plugin default-exports a function that receives `PluginAPI`; the function runs once when the plugin loads. Plugins are long-lived processes that may serve several threads concurrently, so per-thread state must be keyed by `ThreadID`. An optional `export const description` must be a static string literal of at most 300 characters.

`amp.on(event, handler)` registers a handler and returns a `Subscription`. The type `PluginHandlerResult` splits events in two: `session.start` is fire-and-forget and returns `void`, while `tool.call`, `tool.result`, `agent.start`, and `agent.end` are request events whose handlers return a result (the `tool.result` and `agent.end` result types admit `void`). When several plugins listen to one event, handler order is undefined.

`amp.onDispose(callback)` runs cleanup when the plugin is unloaded, reloaded, or the host shuts down gracefully. All of a plugin's dispose callbacks share a budget of about 3 seconds, after which the process is terminated; they do not run when the plugin crashes or is killed with SIGKILL.

Logging goes through `amp.logger.log` or `ctx.logger.log`; handler log messages are appended to the handler's trace span events.

Execute mode can start its turn before plugins finish loading, which skips `agent.start` and `agent.end`. `--plugin-ready-timeout [seconds]` makes execute mode wait for plugin readiness first. The wait is off unless the flag is set; a bare flag waits 10 seconds, the maximum is 300, and `0` disables it.

### Events

The event catalog is five events:

```text
session.start
  └─ agent.start
       ├─ tool.call ──► tool.result   (zero or more)
       └─ agent.end
```

There is no `session.end`, compaction, model-change, notification, or subagent event. Every handler receives a context with `logger`, `$` (shell runner), `ui`, `ai`, `system`, an optional trace `span`, and `thread: PluginThread`.

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
}

interface AgentStartResult {
  message?: { content: string; display?: boolean }
}
```

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

Returning `undefined` keeps the native result; returning an object replaces it.

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

type AgentEndResult = { action: 'continue'; userMessage: string } | void
```

`message` and `id` identify the prompt that started the turn. `messages` holds every message since `agent.start`, including that prompt, in the [transcript schema](#transcript). Returning `continue` sends `userMessage` and starts another turn.

### Thread state and control

`ThreadState` is the agent activity state of one thread:

| State | Upstream meaning |
| --- | --- |
| `idle` | the agent is not working; the last turn, if any, has finished |
| `running` | inference or tool execution in progress |
| `awaiting-approval` | blocked waiting for a tool approval |
| `error` | the thread has an active error |

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
| `parentThreadID()` | the direct parent recorded when the thread was created with a `parentThreadID`, or `null`; becomes `null` if the parent is deleted; supported only for the plugin's current thread |
| `waitForResponse` | waits until the thread has been `running` or `awaiting-approval` and returns to `idle`, then resolves with the last assistant message; rejects on `error` or timeout (default 10 minutes) |
| `cancel()` | stops the current turn |
| `setVisibility` | makes the thread private or workspace-visible; private disables multiplayer; the user must own the thread |
| `setMultiplayer` | enables multiplayer for 5 minutes to 7 days, or disables it with `null`; requires an orb thread that is already shared |
| `appendUserMessage(..., { steer: true })` | when the thread is busy, queues the message as steering, preferred when the thread next dequeues work |

`amp.activeThread` is an observable of the thread the user is focused on in the UI, with a synchronous `.current` snapshot of `{ id }` or `null`. Events for background threads still arrive, and the type docs direct plugins to compare `event.thread.id` with `amp.activeThread.current` to tell the focused thread from a background one. `amp.threads.get(threadID)` returns a `PluginThread` for any thread ID.

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
| `full` | `false` | `false` reads what the next inference turn sees: after a compaction, the latest compaction summary (as a user message) and the messages from the cut point on; `true` reads the whole transcript, compacted-away messages included |
| `from` | `'end'` | end of the thread to count from |
| `offset` | `0` | messages to skip from `from` |
| `limit` | `10` | clamped to 20; page with `offset` |
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

A `tool_use.id` in an assistant message pairs with a `tool_result.toolUseID` in a later user message; `amp.helpers.toolCallsInMessages(messages)` returns the completed pairs. The message types carry no timestamp, model, token usage, cost, or compaction marker, and Amp documents no local transcript file.

### Tools and helpers

Tool names vary with mode, plugins, MCP servers, and build. `amp tools list` prints the tools active for the current settings, and `amp plugins show-agent-options --json` prints the built-in names offered to plugin agents. On the baseline build `builtinToolNames` is:

```text
apply_patch create_file create_thread edit_file find_thread finder get_thread_status
librarian list_agent_modes oracle painter Read read_mcp_resource read_thread
read_web_page send_thread_message shell_command shell_command_kill
shell_command_status skill Task view_media wait_for_threads web_search
```

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
| `amp.registerTool(definition)` | adds a tool the agent can call; `execute` returns `PluginToolResult` |
| `amp.registerCommand(id, options, handler)` | adds a command palette action with enabled, disabled, or hidden availability |
| `amp.registerSkill({ path })` | registers a skill directory of a directory plugin as `<plugin-name>:<skill-name>` |
| `ctx.ui.notify`, `input`, `confirm`, `select` | dialogs; `confirm` also takes editable `fields`, `select` takes `allowOther` |
| `amp.ai.generate`, `amp.ai.ask` | thread-billed text, structured object, or yes/no classification |
| `amp.attachments.upload` | uploads image bytes (at most 4.9 MB decoded, 8000 px per side) and returns a URL |
| `amp.configuration` | observable settings with `get`, `update`, and `delete` against workspace or global targets |
| `amp.system` | `open(url)`, `workspaceRoot`, `ampURL`, `user`, `executor` ([identity](#authentication-account-and-usage)) |
| `amp.$` | tagged-template shell runner resolving `{ exitCode, stdout, stderr }` |
| `amp.createAgent(config)`, `amp.getBuiltinAgent(mode)` | agent handles that create threads or run one-shot turns ([modes](#models-modes-and-effort)) |
| `amp.registerAgentMode(definition)` | adds a plugin mode to mode pickers; needs a matching `// @amp-agent-mode` comment |
| `amp.createWebhook(options)` | durable at-least-once webhook for a plugin in an Amp-managed orb |
| `amp.experimental` | unstable APIs, `createStatusItem`, and compatibility aliases |
| `defineAgent`, `defineTool` | static config for declarative agent directories (`agent.ts`, `tools/<name>.ts`) |

## Private local thread cache

Amp has at some builds written one JSON object per thread under `${AMP_DATA_DIR:-~/.local/share/amp}/threads/T-*.json`. The file is an implementation detail outside the Plugin API and the docs. The schema evidence is ccusage's Amp adapter at commit [`ba99c0d`](https://github.com/ryoppippi/ccusage/tree/ba99c0d09b6db9fd64a6187751e8b88a019f991a/rust/crates/ccusage/src/adapter/amp) and objects captured into RimZ's adapter fixtures; `AMP_DATA_DIR` is anchored there only.

Whether the baseline build writes this file is unverified. On the host that ran the baseline binary, `~/.local/share/amp/` held `device-id.json`, `history.jsonl`, `secrets.json`, and `session.json` and no `threads/` directory, while `amp threads list` showed two threads.

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

Amp runs tools without asking for approval by default. The docs direct users to a plugin for control over tool use ([tools](https://ampcode.com/docs/tools#permissions)): a `tool.call` handler decides, and `ctx.ui.confirm`, `input`, or `select` asks the user. While a tool approval is open, the thread state is `awaiting-approval`; the state carries no tool name, arguments, question, options, or resolver.

Amp has no native plan-approval event and no built-in user-question event. A plugin can register its own question tool with `amp.registerTool` and `ctx.ui`, which adds that tool to the agent's tool set.

The baseline binary ships permission rules, which the current docs do not describe. `amp --help` lists the settings `amp.permissions` ("Permission rules for tool calls"), `amp.guardedFiles.allowlist`, and `amp.dangerouslyAllowAll` ("Disable all command confirmation prompts"), and the command `amp permissions` with `list`, `test`, `edit`, and `add`. The rules as published at [notes/permissions](https://ampcode.com/notes/permissions):

| Aspect | Behaviour |
| --- | --- |
| storage | the `amp.permissions` settings key |
| matching | Amp checks the rules in order before every tool call and applies the first match |
| actions | `allow`, `reject`, `ask`, `delegate` |
| `delegate` | runs a helper program with the tool parameters as JSON on stdin; `AGENT_TOOL_NAME` names the tool |
| delegate exit code | `0` allows, `1` asks the user, `2` rejects with stderr shown to the model |

`amp.mcpPermissions` allows or blocks MCP servers by pattern ([settings](https://ampcode.com/docs/cli/settings)); the baseline help does not list it.

## Compaction and context

Amp compacts a thread automatically when its estimated input tokens cross a threshold: a configured percentage of the context window, 90% by default. A custom plugin agent can set an absolute `compactionThresholdTokens` instead. A system model writes the summary ([models and subagents](https://ampcode.com/docs/models-and-subagents)).

No plugin event marks compaction, and no API reports a context percentage or token count for an interactive thread. `messages()` shows the effect afterwards: the default view starts at the latest summary, which is an ordinary user message, and `{ full: true }` includes the discarded context. Per-response token counts exist only in [stream JSON](#supervised-runs-and-stream-json), and each model's `contextWindow` is listed by `amp plugins show-agent-options --json`.

## Models, modes, and effort

The dial has four built-in modes: `low`, `medium`, `high`, and `ultra`. Each mode combines a model, reasoning effort, system prompt, tools, and Oracle, and Amp changes the backing models over time. Settings → Mode Dial lets a user or workspace admin pick the model and effort behind each mode's main agent, Oracle, and subagents, and put plugin agents on the dial; personal choices take precedence over the workspace dial ([build-your-own-dial](https://ampcode.com/news/build-your-own-dial), 2026-09-10). `Ctrl+S` switches modes in the CLI.

| Flag | Values |
| --- | --- |
| `-m, --mode <value>` | `low`, `medium`, `high`, `ultra`, or a plugin mode by key or label, case-insensitive; controls the model, system prompt, and tool selection |
| `--features <value>` | `fast` (faster serving at a premium) or `pro` (GPT-5.6 Pro, OpenAI API only) |
| `--fast` | alias for `--features fast` |

The baseline `amp --help` lists no `--effort` flag, and no docs page read for this baseline documents one. Whether the CLI accepts `--effort` is unverified.

The modes `smart`, `deep`, and `rush` are deprecated. Existing threads in them keep working; new threads spawned from them start in the replacement mode: `rush` becomes `low`, `smart` and `deep` become `medium`.

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
  features?: readonly ('fast' | 'pro' | string)[]
  display?: { label: string; color?: string }
}
```

A built-in definition names the mode and carries no model or reasoning effort. A custom definition carries its resolved `model` (filled from the extended mode when `extends` is set and `model` is omitted) and any `reasoningEffort` it declares. The efforts a model accepts are in `capabilities.efforts` of `amp plugins show-agent-options --json`. Stream JSON `system/init` carries `agent_mode` and no effort field.

## Subagents and concurrent threads

Amp delegates work to built-in subagents: Search, Oracle, Librarian, and Read Thread. Each works in its own context, cannot be guided mid-task, and returns only a final summary to the main agent ([models and subagents](https://ampcode.com/docs/models-and-subagents)). The Plugin API has no subagent start or stop event.

`PluginThread.parentThreadID()` returns the parent of a thread created with a `parentThreadID`, for the plugin's current thread only. Plugin agents create such threads through `Agent.createThread({ parentThreadID })` or `Agent.run(message, { parentThreadID })`, which resolves with `{ threadID, text }`. Whether built-in subagent work runs in a thread with a recorded parent, and whether it fires plugin events, is undocumented.

Agents can also coordinate persistent child threads through the tools `create_thread` (with a per-call `agent_mode`), `list_agent_modes`, `get_thread_status`, `send_thread_message`, and `wait_for_threads`; `Task` instead runs a scoped subagent without a per-call mode.

In stream JSON, assistant and user messages carry `parent_tool_use_id`, which is `null` when the message has no parent tool use. The current docs do not say which tool use a subagent's messages point at.

One Amp process can host several threads: `amp.activeThread` names the focused thread while events for background threads keep arriving, and `amp.remoteThreadCreation.enabled` opens remotely created threads in a running TUI. Amp removed the TUI sidebar for switching between threads on 2026-08-27 ([so-long-tui-sidebar](https://ampcode.com/news/so-long-tui-sidebar)); the post does not say how the TUI changes focus afterwards.

## Supervised runs and stream JSON

`amp -x, --execute [message]` runs one turn without the TUI. The prompt comes from the argument or stdin, only the last assistant message is printed, and Amp exits. Redirecting stdout turns execute mode on. Execute mode archives a new thread when it finishes; `--no-archive-after-execute` leaves it unarchived (the flag applies to `amp review` too).

```sh
amp --execute "prompt" --stream-json --plugin-ready-timeout 30
amp threads continue <T-id> --execute "follow-up" --stream-json --plugin-ready-timeout 30
```

| Flag | Effect |
| --- | --- |
| `--stream-json` | with `--execute`, prints Claude Code-compatible stream JSON instead of plain text |
| `--stream-json-thinking` | adds `thinking` and `redacted_thinking` blocks; implies `--stream-json`; not Claude Code-compatible |
| `--stream-json-input` | reads JSON Lines user messages from stdin; requires `--execute` and `--stream-json` |
| `--plugin-ready-timeout [seconds]` | waits for plugins before the turn ([readiness](#runtime-and-execute-mode-readiness)) |
| `--no-archive-after-execute` | leaves the new thread unarchived |
| `--title <title>` | sets the new thread's title before the agent starts |
| `-l, --label <label>` | labels the created or continued thread; repeatable |
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

`steer: true` delivers the message at the agent's next pause point. Amp exits only after the assistant is done and stdin has closed. In the TUI, a message the user sends while the agent works is delivered at the next opportunity during the turn; built-in actions such as Ship and Review queue until the turn ends ([steer-dont-queue](https://ampcode.com/news/steer-dont-queue)).

## Remote control, runners, and executors

Cross-client access lets a user continue a running CLI thread from ampcode.com on desktop or mobile ([remote control](https://ampcode.com/docs/cli/remote-control)). Workspace admins turn it off in Member Settings, which also stops members starting threads on their runners from outside the CLI. "Require Passkey Authentication for Web & App Interaction" adds passkey verification, per user or enforced by Enterprise admins.

| Control | Behaviour |
| --- | --- |
| `--remote-control-terminal` / `--no-remote-control-terminal` | allows or denies terminal access from ampcode.com; the flag overrides the environment |
| `AMP_REMOTE_CONTROL_TERMINAL` | `1` enables, `0` disables when no flag is given; terminal access is off by default |
| `amp.remoteThreadCreation.enabled` | default `false`; lets ampcode.com create threads that open in the interactive TUI on this machine, in the directory where it started; palette command `amp: enable remote creation of threads` |
| `amp --no-tui` | runner mode: waits for and runs remotely created threads for the current directory without a TUI |
| `--runner-id <id>` | stable runner ID; must be a valid hostname; case-insensitive, casing preserved ([runners](https://ampcode.com/docs/cli/runners)) |
| `--executor <local \| orb \| runner:<id>>` | where a new thread runs: this client, an Amp-managed orb, or a live runner |
| `-ox, --orb-execute`, `--orb-size`, `--project` | execute in an orb, with an orb size and an Amp project other than the one inferred from Git remotes |

Plugin code sees only `amp.system.executor.kind`, one of `local`, `remote`, or `unknown`; it carries no runner ID, pane, PID, or cwd. `executor.keepAlive()` holds an orb awake and rejects outside an orb. Plugin agents choose an executor per thread with `executor: 'local' | 'orb' | { type: 'runner'; id }`.

## Authentication, account, and usage

`amp login` signs in interactively and `amp logout` removes the stored API key. Non-interactive use reads `AMP_API_KEY`, which must be an access token from Settings starting with `sgamp_`; the CLI rejects the short-lived session token `amp login` stores, which expires within an hour ([execute mode](https://ampcode.com/docs/cli/execute-mode)). `AMP_URL` selects the Amp service, `https://ampcode.com/` by default.

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

`user` is `null` when Amp is not authenticated. `workspaceRoot` is stable for the plugin process; plugins reload when the workspace changes.

| Command | Output |
| --- | --- |
| `amp usage` | current usage and credit balance, as human text; `--details` adds credit, token, and thread usage, and `--start` and `--end` take ISO 8601 bounds |
| `amp threads usage <T-id>` | usage for one thread; `--details` adds detail ([explain-usage](https://ampcode.com/news/explain-usage), 2026-08-21) |
| `amp threads export <T-id>` | the thread as JSON, undocumented schema |
| `amp threads raw <T-id>` | raw actor thread data as JSON; in the baseline help only |
| `amp threads markdown <T-id>` | the thread as Markdown |
| `amp top --stream-jsonl` | a JSON line whenever the live thread list changes; schema marked experimental |

No usage command documents a JSON output mode, and the baseline `amp usage --help` lists none. Plugin events and messages carry no cost.

Amp bills in tiers with included monthly usage (Hobby is free, Individual is $20 a month), and a user who exhausts the included usage adds paid credits ([pricing](https://ampcode.com/docs/pricing)). For non-enterprise accounts Amp deducts actual model cost from credits without markup, workspace members share paid workspace credits, and purchased credits expire twelve months after purchase. Enterprise uses pooled credits and custom pricing.

## Settings and environment

Amp reads JSON or JSONC settings; every key has the `amp.` prefix ([settings](https://ampcode.com/docs/cli/settings)).

| Layer | Location |
| --- | --- |
| managed (Enterprise) | `/etc/ampcode/managed-settings.json` (Linux), `/Library/Application Support/ampcode/managed-settings.json` (macOS), `%ProgramData%\ampcode\managed-settings.json` (Windows) |
| workspace | nearest `.amp/settings.json` or `.amp/settings.jsonc` found searching upward from the cwd |
| user | `~/.config/amp/settings.json` or `.jsonc`; `--settings-file <path>` or `AMP_SETTINGS_FILE` replaces the path |

Managed settings enforce policy, and workspace settings override user settings. User `amp.keymap` entries are the exception and override workspace entries. The managed-only `amp.admin.compatibilityDate` (`YYYY-MM-DD`) selects which backward-compatibility migrations apply.

| Setting | Effect |
| --- | --- |
| `amp.permissions`, `amp.guardedFiles.allowlist`, `amp.dangerouslyAllowAll` | [permission rules](#permissions) |
| `amp.mcpServers` | MCP servers, as local commands or remote URLs, exposed as tools |
| `amp.mcpPermissions` | allow or block MCP servers by pattern |
| `amp.tools.disable`, `amp.tools.enable` | remove tools, or allow only matching tool patterns |
| `amp.experimental.modes` | enable experimental agent modes by name |
| `amp.remoteThreadCreation.enabled` | [remote thread creation](#remote-control-runners-and-executors) |
| `amp.defaultVisibility` | default thread visibility per repository origin |
| `amp.thread.autoArchiveOnQuit` | archive open CLI threads on quit; default `false` |
| `amp.showCosts` | show costs while working; default `true` |
| `amp.notifications.enabled`, `amp.notifications.system.enabled` | completion alerts and system notifications |
| `amp.skills.path`, `amp.skills.disableClaudeCodeSkills` | extra skill directories; ignore `.claude` skill directories |
| `amp.git.commit.ampThread.enabled`, `amp.git.commit.coauthor.enabled` | `Amp-Thread` trailer and co-author trailer in commits |
| `amp.proxy`, `amp.network.timeout` | proxy URL and request timeout in seconds for the Amp server |
| `amp.updates.mode` | [update checking](#version-and-distribution) |

Several surfaces execute commands on the user's machine: project plugins in `.amp/plugins/`, `amp.mcpServers` commands, a `delegate` rule's helper program, and plugin install URLs. Amp asks for approval before starting a workspace MCP server (`amp mcp approve <name>`, with `amp mcp doctor` for status); servers from user settings or `--mcp-config` skip that approval.

| Variable | Effect |
| --- | --- |
| `AMP_API_KEY` | [access token](#authentication-account-and-usage) |
| `AMP_URL` | Amp service URL |
| `AMP_SETTINGS_FILE` | user settings path |
| `AMP_LOG_LEVEL`, `AMP_LOG_FILE` | log level and log file; the file defaults to `~/.cache/amp/logs/cli.log` |
| `AMP_REMOTE_CONTROL_TERMINAL` | [terminal access](#remote-control-runners-and-executors) |
| `AMP_FORCE_BEL` | notifications use the terminal bell |
| `AMP_SKIP_UPDATE_CHECK` | `1` skips the update check |
| `AMP_DISABLE_AMP_THREAD_TRAILER`, `AMP_DISABLE_AMP_COAUTHOR_TRAILER` | suppress the commit trailers |
| `NO_ANIMATION` | `1` disables terminal animations |
| `HTTP_PROXY`, `HTTPS_PROXY`, `NODE_EXTRA_CA_CERTS` | corporate proxy and CA certificates |
| `AMP_DATA_DIR` | data root of the [private thread cache](#private-local-thread-cache); anchored by ccusage only |

## Undocumented behaviour

Neither the docs nor the baseline binary settle these. Each needs a capture against a pinned build before code depends on it.

1. How long a `tool.call` handler may wait, how Amp cancels it, and what happens when the plugin process exits mid-call.
2. How `tool.call` results from several plugins compose, and what a handler exception does.
3. Whether `awaiting-approval` brackets a plugin's `ctx.ui.confirm` and a permission rule's `ask` the same way, and its order relative to `tool.call`.
4. Whether built-in subagent work fires plugin events, and under which thread ID.
5. Which process runs a system plugin, and so whether a plugin's PID identifies the Amp CLI; no payload carries a PID or pane.
6. Execute-mode exit codes for success, error, cancellation, permission denial, and plugin-readiness expiry.
7. The schemas of `amp usage`, `amp threads usage`, `amp threads export`, and `amp threads raw` output.
8. Whether the CLI accepts `--effort`.
9. Whether `amp threads continue` resumes a thread that execute mode archived.
10. How the TUI changes the focused thread since the sidebar's removal, and whether a focus change still fires `session.start`.
11. Whether the baseline build still writes the [private thread cache](#private-local-thread-cache).
