# Amp CLI protocol reference

> RimZ's landed mapping is [adapter_amp.md](../../internals/agents/adapter_amp.md). This document records the upstream surface behind it; the agent-agnostic lifecycle and enrichment contracts are [model.md](../../internals/agents/model.md), and the account/spend contract is [providers.md](../../internals/agents/providers.md).

This is the single home for the **Amp CLI upstream protocol surface** relevant to RimZ: the Plugin API lifecycle and decision seam, thread identity and state, transcript access, tool classification, execute-mode JSONL, permissions, configuration and trust, authentication and usage, remote control, runners, and launch modes. It is the implementation research and drift-check record; current RimZ support claims live in [adapter_amp.md](../../internals/agents/adapter_amp.md) and [agent-support.md](../../reference/agent-support.md).

Refresh baseline: Amp CLI [`@ampcode/cli` 0.0.1783946745-g8c4c0a](https://www.npmjs.com/package/@ampcode/cli/v/0.0.1783946745-g8c4c0a), released **2026-07-13**, and the rolling official Amp manual and generated `@ampcode/plugin` type reference available on **2026-07-13**. The exact CLI reports `0.0.1783946745-g8c4c0a (released 2026-07-13T12:45:45.000Z)`, and `amp plugins show-docs` from that binary is the type-reference baseline used here.

This reference supports **the current post-rebuild Amp architecture only**. Amp calls that architecture “Neo” in its May 2026 launch material, then dropped the name when it became the only current architecture. Do not carry forward pre-rebuild hooks, local thread schemas, toolbox behavior, `--take-me-back`, or the old `smart` / `deep` / `rush` / `large` mode contract. The current modes are `low`, `medium`, `high`, and `ultra`; old mode names exist only as deprecated compatibility inputs or separately installable classic plugins.

Amp deliberately evolves without a backwards-compatibility commitment, and its public manual is rolling rather than versioned. Refresh the npm version, `amp version`, `amp --help`, `amp plugins show-docs`, and the web manual together before implementation.

## Upstream sources

| Surface | Source |
| --- | --- |
| Owner's manual: current modes, CLI, plugins, settings, remote control, pricing | <https://ampcode.com/manual> |
| Generated Plugin API types and examples | <https://ampcode.com/manual/plugin-api> |
| Stream JSON and legacy permission schemas | <https://ampcode.com/manual/appendix> · <https://ampcode.com/manual/appendix/legacy-permissions-rules.txt> |
| TypeScript SDK and generated type reference | <https://ampcode.com/manual/sdk> · <https://ampcode.com/manual/sdk/typescript> |
| Python SDK reference | <https://ampcode.com/manual/sdk/python> |
| Current mode migration | <https://ampcode.com/news/the-dial> |
| Current architecture, compaction, queuing, permissions | <https://ampcode.com/news/neo> · <https://ampcode.com/news/drop-the-neo> |
| Remote control, multi-thread UI, and runner mode | <https://ampcode.com/news/agents-everywhere> · <https://ampcode.com/news/agents-anywhere> |
| Binary/package distribution | <https://ampcode.com/news/npm-package-changes> · <https://www.npmjs.com/package/@ampcode/cli> |
| Credentials, service boundaries, storage, and retention | <https://ampcode.com/security> |

The installed executable is an authoritative companion to the rolling pages:

```sh
amp version
amp --help
amp plugins --help
amp plugins show-docs
amp plugins show-agent-options --json
amp tools list
```

`amp plugins show-docs` emits the `@ampcode/plugin` declarations understood by that exact binary. Prefer it over cached web text whenever the two disagree.

The pinned `amp --version` shape is `0.0.1783946745-g8c4c0a (released 2026-07-13T12:45:45.000Z)`. RimZ validates the leading numeric plus `-g<hex>` build token and normalizes this example to `0.0.1783946745-g8c4c0a`; release annotation and other prose stay outside the version field.

The executable and launch kind are both `amp`. Official installation paths are the direct shell/PowerShell installer, Homebrew (`ampcode/tap/ampcode`), and the `@ampcode/cli` npm package; npm is supported but not recommended. `amp update` refreshes direct installs, while `amp.updates.mode` controls automatic checking. Because this adapter targets only the latest architecture, preflight should reject a different version with the exact supported-version fix instead of guessing across Amp's intentionally moving surface.

## Recommended adapter shape

Use a RimZ-authored **system plugin** at `~/.config/amp/plugins/rimz.ts` as the interactive lifecycle seam. The plugin runs with the stock Amp TUI, receives stable `T-…` thread IDs on every event, can observe thread state, and preserves Amp's native terminal and web surfaces.

Use `agent.start`, `tool.result`, and `agent.end` as durable lifecycle truth. Use `session.start` to register a thread and subscribe to its state. Use `tool.result`, rather than `tool.call`, for ordinary proof-of-work so an observation-only RimZ plugin does not enter Amp's decision path.

Use `PluginThread.state` as synchronous state enrichment for `awaiting-approval` and `error`. A RimZ-owned approval mode can additionally register `tool.call`, await RimZ's per-request answer socket, and return Amp's decision object. Keep that path opt-in: `tool.call` is a request event with no neutral return, multiple plugin handler order is unspecified, and the public contract does not define how competing decisions compose.

Use `PluginThread.messages({ full: true, … })` as the documented transcript read API and lifecycle-safe authority. Amp publishes no stable local transcript path or file schema for the current architecture. The private rewritten cache described below may enrich history and display best-effort, but its failure never synthesizes lifecycle or replaces the plugin's final-answer authority. Treat `amp threads export` and `amp threads raw` output as opaque until their schemas are officially documented or captured and version-gated from the exact supported binary.

Use `amp -x --stream-json --plugin-ready-timeout` for supervised runs. Streaming JSON is the only documented CLI transport that carries thread identity, cwd, mode/effort, tool calls, per-response token usage, terminal result, and subagent correlation. The plugin-ready wait is required because execute mode may otherwise start before `agent.start` / `agent.end` handlers are ready.

Amp publishes no command-hook protocol, statusline transport, OpenTelemetry schema, ACP server, or interactive stdio RPC in the current CLI. Do not adapt a similarly named surface from another agent or from pre-rebuild Amp.

The candidate transport matrix is:

| RimZ concern | Primary upstream surface | Gap / backstop |
| --- | --- | --- |
| session identity | plugin event `thread.id` (`T-${string}`) | `session.start` also fires when an existing thread is opened or switched to |
| turn start/end | `agent.start` / `agent.end` | execute mode can skip them without `--plugin-ready-timeout` |
| proof of work | `tool.result` | `tool.call` is the blocking policy seam, not a neutral observation seam |
| reasoning → acting | `filesModifiedByToolCall(tool.result)` | helper covers known edit/create/apply-patch and in-place `sed`; tolerate new tools |
| permission wait | `PluginThread.state == "awaiting-approval"` | no ask details or answer handle in the state value |
| RimZ-owned permission decision | opt-in `tool.call` handler + per-request socket | multi-plugin decision composition and failure policy are undocumented |
| plan approval / user question | none | a plugin may add its own UI/tool, but stock Amp exposes no dedicated event |
| compaction | none | transcript reads expose post-compaction context, but no lifecycle event or percentage |
| subagents | execute JSONL `parent_tool_use_id` | interactive plugin events expose no parent/child relation for built-in subagents |
| model / mode / effort | `thread.agent()`; execute `system.init` mode/effort | built-in agent definitions expose mode, not the backing model; mode→model wiring changes |
| context usage | execute assistant `usage` and `max_tokens` | plugin transcript messages omit usage; no interactive context percentage API |
| live cost | none | `amp threads usage` is a human CLI; no documented machine schema |
| transcript | `PluginThread.messages()` | no documented local transcript file |
| message queue / steer | `appendUserMessage(..., { steer })` | RimZ's pane-first message contract still selects pane send for ordinary human text |
| session end | none | pane/process liveness must reap; `session.start` explicitly has no matching end event |
| auth identity | `PluginSystem.user`; `amp usage` | plugin identity exists only while Amp is running; usage output is human text |
| account balance / spend | `amp usage`; `amp threads usage <id>` | no documented JSON; Enterprise analytics API is a separate gated surface |
| remote control | built-in web control and runner mode | one CLI process can host several foreground/background threads |

## Plugin discovery and runtime

Amp plugins are TypeScript files executed with Bun. A plugin default-exports a function receiving `PluginAPI`; that function runs once when the plugin loads. Plugins are long-lived processes and may serve multiple threads concurrently, so keep all per-thread state keyed by `ThreadID` and make handler code concurrency-safe.

Discovery locations:

| Scope | Location | RimZ implication |
| --- | --- | --- |
| project | `.amp/plugins/*.ts` | repository executable surface; project trust must cover it |
| system | `~/.config/amp/plugins/*.ts` | preferred RimZ-owned install location |
| global workspace | centrally configured by Amp workspace | limited experimental release; administrator-controlled executable surface |

Install one bounded system file such as `~/.config/amp/plugins/rimz.ts`. Make install, diff, upgrade, and uninstall idempotent by path. Amp's command palette actions `plugins: reload` and `plugins: list` reload and inspect plugins; the CLI also exposes `amp plugins add/remove/update/list`, but a RimZ-owned local file gives the hook installer an auditable artifact without depending on a remote plugin URL.

Plugins apply to interactive `amp` sessions and `amp --execute`. Plugin UI mirrors between TUI and web surfaces. The API provides `ctx.logger` / `amp.logger`; use those for diagnostics and reserve any RimZ helper stdout for the helper's structured response.

The public API says handler order is undefined when multiple plugins listen to the same event. It does not document request-result merge precedence, handler timeout, or exception/fail-open behavior. Capture all three before shipping a `tool.call` decision handler. Observation handlers should avoid changing results: `tool.result` may return `undefined`, `agent.start` may return `{}`, and `agent.end` may return nothing.

Execute mode starts as soon as its turn is ready, which may beat plugin startup. `--plugin-ready-timeout` waits until plugins are ready or the bound expires; a bare flag uses 10 seconds, an explicit value may be at most 300 seconds, and `0` disables the wait. RimZ supervised launches should always pass a nonzero value and treat expiry as a failed precondition rather than silently accepting missing lifecycle events.

## Plugin lifecycle events

The complete current event catalog is five events:

```text
session.start
  └─ agent.start
       ├─ tool.call ──► tool.result   (zero or more)
       └─ agent.end
```

There is no `session.end`, compaction, model-change, effort-change, notification, subagent-start, or subagent-stop event.

### `session.start`

Fires when Amp starts a thread session: the first message in a new thread, or opening/switching to an existing thread. Multiple threads can remain active concurrently in one CLI.

```ts
interface SessionStartEvent {
  thread: { id: `T-${string}` }
}
```

Use it to emit `registered`, bind the thread ID, subscribe to `ctx.thread.state`, and read the thread's agent definition. Do not interpret it as a new conversation or a fresh turn.

### `agent.start`

Fires when a user prompt starts a turn.

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

`id` is the user-message ID. Current thread-actor threads use stable string IDs; the union retains numeric IDs for legacy TUI threads. A handler may append context to the user message, optionally visible in the UI. A RimZ observation handler returns no added message.

Map this event to `turn_started`, with `message` as prompt/task and `thread.id` as `agent_id`.

### `tool.call`

Fires before execution and requires a decision result from every registered handler.

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

`allow` runs the original input. `reject-and-continue` blocks this call and returns the message to the agent. `modify` replaces the input. `synthesize` bypasses execution and supplies a result. `error` stops the thread worker and displays an ephemeral error.

Amp publishes no deadline for an async handler and no documented cancellation signal in `PluginEventContext`. Verify indefinite waits, process exit, and UI cancellation against the supported binary before using this event as RimZ's blocking bridge.

### `tool.result`

Fires after execution and before the result is sent back to the model.

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

Return nothing to preserve the native result. Map every root-thread result to proof-of-work; mark editing only when `filesModifiedByToolCall(event)` returns one or more file URIs. Tool failure is activity evidence, not a completed-turn verdict; `agent.end.status` owns that verdict.

### `agent.end`

Fires when Amp finishes the turn started by the paired user message.

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

`messages` contains all messages since `agent.start`, including the starting user message. `done` maps to a clean `turn_ended`; `error` and `cancelled` map to errored completion unless a later implementation captures a distinct provider-park marker. Returning `continue` appends a user message and starts another turn, so the RimZ observation path returns nothing.

## Thread state and control

Every plugin event context carries `thread: PluginThread`. The stable state vocabulary is:

```ts
type ThreadState = 'idle' | 'running' | 'awaiting-approval' | 'error'
```

| State | Upstream meaning | RimZ use |
| --- | --- | --- |
| `idle` | no work in flight; last turn finished | enrichment only; `agent.end` is the durable boundary |
| `running` | inference or tool execution in progress | heartbeat / missed-start reconciliation |
| `awaiting-approval` | blocked on a tool approval | `awaiting_input { permission }`, without prompt detail |
| `error` | active thread error | failure enrichment / missed-end reconciliation |

`PluginThread` exposes:

```ts
interface PluginThread {
  id: `T-${string}`
  agent(): Promise<Agent>
  readonly title: Observable<string | null> & { get(): Promise<string | null> }
  readonly state: Observable<ThreadState> & { get(): Promise<ThreadState> }
  waitForResponse(options?: { timeoutMs?: number }): Promise<ThreadAssistantMessage>
  cancel(): Promise<void>
  messages(options?: ThreadMessagesOptions): Promise<ThreadMessage[]>
  append(messages: UserMessage[]): Promise<void>
  appendUserMessage(message: UserMessage, options?: { steer?: boolean }): Promise<void>
}
```

`waitForResponse` waits until the thread has been running or awaiting approval and returns to idle, then returns the last assistant message. It rejects on `error` or timeout; the default timeout is ten minutes. `cancel()` stops the current turn. `appendUserMessage(..., { steer: true })` marks a busy-thread message as steering so Amp prefers it at the next interruption point.

`amp.activeThread` is an observable plus a synchronous `.current` snapshot containing `{ id }` or `null`. It identifies the thread focused in the UI; background thread events continue to arrive. Compare each event's ID to `activeThread.current` before treating it as the pane-visible agent.

The API can address any known thread through `amp.threads.get(threadID)`. This can support explicit remote steering or cancellation, but ordinary `rimz message` continues to use the pane send path to preserve RimZ's cross-backend interaction contract.

## Transcript schema

`PluginThread.messages()` is the stable plugin-facing transcript read API. Defaults are `{ from: "end", limit: 10 }`; `limit` is clamped to 20, so page with `offset`. `roles` may filter to `user` / `assistant`.

```ts
interface ThreadMessagesOptions {
  full?: boolean
  from?: 'start' | 'end'
  offset?: number
  limit?: number
  roles?: Array<'user' | 'assistant'>
}
```

By default, a compacted thread returns what the next inference sees: the latest compaction summary represented as a user message, followed by messages after the compaction cut point. `{ full: true }` returns the entire transcript including compacted-away messages.

The message union is:

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
```

Do not render `thinking` as ordinary assistant text. Correlate tool blocks by assistant `tool_use.id` ↔ user `tool_result.toolUseID`. The Plugin API also provides `toolCallsInMessages(messages)` to return completed call/result pairs.

These message types carry no timestamp, model, token usage, cost, parent-thread ID, transcript path, or explicit compaction marker. Amp's security reference says the client keeps local thread history and the server syncs/stores threads, but it publishes no current local on-disk schema. Build correctness against `PluginThread.messages()`; confine any discovered-cache use to tolerant, best-effort enrichment.

## Private local cache (unsupported upstream surface)

Amp currently rewrites one JSON object per thread under `${AMP_DATA_DIR:-~/.local/share/amp}/threads/T-*.json`. This is an implementation detail, not part of the Plugin API or manual. The schema evidence for this section is ccusage's Amp adapter at commit [`ba99c0d09b6db9fd64a6187751e8b88a019f991a`](https://github.com/ryoppippi/ccusage/tree/ba99c0d09b6db9fd64a6187751e8b88a019f991a/rust/crates/ccusage/src/adapter/amp) plus captured current and legacy objects encoded as RimZ fixtures.

The root carries `id`, `messages`, and optionally `usageLedger.events`. Message IDs and ledger references occur as strings or numbers. Visible message `content` occurs as a string in captured caches and as the public block array described above; only user/assistant text is conversation content. Current assistant records carry `usage.model`, `usage.timestamp`, `inputTokens`, `outputTokens`, `cacheCreationInputTokens`, `cacheReadInputTokens`, and sometimes `totalTokens`. Legacy ledger events carry `id`, `timestamp`, `model`, `toMessageId`, and `tokens.{input,output,total}`; correlating `toMessageId` to assistant `messageId` recovers cache creation/read counts from the message usage object.

Treat the file as a rewritten object. Parse it whole, skip malformed child rows, and reject malformed root JSON or a missing/mismatched root ID. Prefer ledger usage only when at least one ledger row is usable, otherwise fall back to current assistant usage. A total-only record preserves the total approximately as output so token history remains visible. A valid empty thread authoritatively replaces prior cached entries; an unreadable, torn, malformed, or mismatched rewrite preserves the previous good fold. `$AMP_DATA_DIR` is Amp's single live data root for session binding even though ccusage additionally accepts comma-separated archive roots for its fleet reports.

## Tool classification

Tool names are dynamic across modes, plugins, MCP servers, and releases. Discover the exact runtime list with `amp tools list` and the broader built-in plugin-agent list with `amp plugins show-agent-options --json`. The refresh binary exposed `apply_patch`, `shell_command`, and `shell_command_status` in its current mode, while the broader built-in catalog also included `create_file`, `edit_file`, `Task`, `Read`, `oracle`, and others.

Prefer Amp's helpers over a hard-coded edit list:

```ts
amp.helpers.shellCommandFromToolCall(event)  // { command, dir? } | null
amp.helpers.filesModifiedByToolCall(event)  // URI[] | null
amp.helpers.filePathFromURI(uri)             // local path
amp.helpers.toolCallsInMessages(messages)    // paired calls/results
```

`filesModifiedByToolCall` officially covers edit/create/apply-patch tools and in-place `sed` shell commands. Its positive result is RimZ's file-edit proof. Treat every other completed tool as work proof. Keep unknown tool names valid; plugin and MCP names expand the vocabulary at runtime.

## Permissions and blocking asks

Current Amp runs tools without approval by default. The old `--dangerously-allow-all` behavior is now the default, and the latest CLI no longer advertises that flag. If settings contain `amp.permissions`, `amp.guardedFiles.allowlist`, or `amp.dangerouslyAllowAll: false`, Amp activates its bundled legacy-permissions plugin; Amp recommends custom Plugin API policy for new integrations.

The Plugin API's `tool.call` + `ctx.ui.confirm/input/select` is the current native policy seam. While a native confirmation is open, `PluginThread.state` can report `awaiting-approval`. The state does not carry tool name, arguments, question text, options, or a resolver; a RimZ adapter that only observes another plugin's dialog can surface waiting but cannot implement `rimz answer` for it.

A RimZ-owned decision bridge can hold its `tool.call` handler, send `{ thread.id, toolUseID, tool, input }` to a per-request RimZ socket, and return `allow` or `reject-and-continue` after the answer. This gives `rimz answer` a real resolver without taking over Amp's transcript. Verify handler cancellation and multi-plugin composition first, and make this bridge opt-in rather than silently adding prompts to Amp's default posture.

Amp exposes no native plan-approval event and no built-in general user-question event. Plugins can add a custom question tool using `ctx.ui.input/select`, but installing such a tool changes the agent's available tool set and does not expose an external resolver. Declare plan approval and user-question answering unsupported until Amp publishes a native event or RimZ deliberately owns and documents a custom tool.

The legacy permission rule format remains relevant for users who already opted in. First match wins; actions are `allow`, `reject`, `ask`, or `delegate`; `context` may restrict a rule to `thread` or `subagent`. `delegate` runs an external program with tool arguments as JSON on stdin and exports `AMP_THREAD_ID`, `AGENT_TOOL_NAME`, and `AGENT=amp`; exit `0` allows, `1` asks in Amp's UI, and `>=2` rejects with stderr surfaced to the model. This is a useful compatibility seam but not the recommended new RimZ transport because it covers only calls matched by legacy rules and hands `ask` back to Amp's UI.

## Compaction and context

Current Amp automatically compacts a thread at 90% of its context window, summarizes the current context, starts a fresh window with that summary, and continues. The current Plugin API has no pre/post compaction event, percentage, token counter, or explicit summary marker.

`messages()` exposes the effect after the fact: the default view begins at the latest compaction summary, while `{ full: true }` includes discarded context. The summary is represented as a normal user message in the public plugin schema, so content inspection cannot safely open or close RimZ's compaction bracket.

The supported upstream surface leaves interactive context usage and compaction lifecycle unavailable. RimZ can partially enrich interactive tokens from the private cache at turn boundaries and on a producer tick, but the cache has no stable context-window divisor. Execute mode carries per-response token composition and `max_tokens`, but that transport applies only to RimZ-supervised runs and should not be projected onto unrelated interactive threads.

## Models, modes, and effort

The current built-in dial is `low`, `medium`, `high`, and `ultra`. The modes express capability/cost tiers, and Amp changes their backing models as models improve. Do not hard-code today's mode→model table as adapter protocol.

`thread.agent()` returns an `Agent` whose definition is either:

```ts
type AgentDefinition =
  | { kind: 'builtin-agent'; mode: 'low' | 'medium' | 'high' | 'ultra' | 'smart' | 'deep' | 'rush'; reasoningEffort?: AgentReasoningEffort }
  | { kind: 'agent-definition'; name?: string; model: `${provider}/${string}`; instructions: string; tools?: AgentToolSelection; reasoningEffort?: AgentReasoningEffort; display?: { label: string; color?: string } }

type AgentReasoningEffort = 'none' | 'minimal' | 'low' | 'medium' | 'high' | 'xhigh' | 'max'
```

For a built-in agent, record the stable mode and optional effort; the definition does not expose the backing model. For a custom plugin agent, record its explicit model and effort. Execute stream `system.init` separately carries `agent_mode?` and `reasoning_effort?`.

The current CLI exposes `-m/--mode` and `--effort`. On the refresh binary the two surfaces disagree: `amp --help` advertises `-m, --mode (low, medium, high)`, while `amp plugins show-docs` still types `BuiltinAgentMode = 'low' | 'medium' | 'high' | 'ultra' | 'smart' | 'deep' | 'rush'`. Treat the plugin type as the wider truth and the help line as a lagging summary; RimZ profile launch passes a configured mode verbatim rather than validating it against the narrower help set, and passes a mode only when the user configured one. Suggested permission posture mapping is: `auto` and `yolo` use Amp's upstream default; `ask` requires the explicit RimZ/plugin approval policy; `plan` has no native flag and requires prompt-level “do not edit” guidance rather than pretending Amp has a plan state.

## Subagents and concurrent threads

Amp may spawn built-in subagents for complex work. Each has its own context window and tools; the main agent receives the final summary. Amp's interactive Plugin API publishes no subagent start/stop event, child ID, parent ID, or parent-tool-use ID.

Custom plugin agents can create a thread with `parentThreadID`, and one-shot `Agent.run(...)` returns the created `threadID`, but that identifies only subagents created through the plugin itself. Do not infer the same relation for Amp's built-in `Task`, oracle, or librarian behavior.

Execute stream JSON provides the missing supervised-run correlation: subagent messages set `parent_tool_use_id` to the parent `Task` call ID, root messages use `null`, and the final `result` waits for every subagent to finish. The correlation is a tool-use ID rather than a child thread ID, so it can support nested progress rendering but not a durable RimZ child session without another identity source.

One Amp CLI can keep several threads running concurrently and switch the focused thread without changing panes. Background and remotely created threads may produce plugin events while another thread is visible. The first adapter should either render only `amp.activeThread.current` as pane-bound and treat background threads as enrichment, or extend RimZ's instance/session binding deliberately; letting every thread compete for one pane would violate pane primacy.

## Supervised runs and stream JSON

`amp -x <prompt>` runs one non-interactive turn, prints the last assistant text, and exits. Redirecting stdout enables execute mode automatically. Execute mode consumes paid credits rather than Amp Free usage. The latest CLI archives new execute-mode threads by default and offers `--no-archive-after-execute` to retain them.

Use:

```sh
amp --execute "prompt" --stream-json --plugin-ready-timeout 30
amp threads continue <T-id> --execute "follow-up" --stream-json --plugin-ready-timeout 30
```

The JSONL sequence is `system/init`, user and assistant messages, then one terminal `result`. Every object carries `session_id`, which is the Amp thread ID.

```ts
type Init = {
  type: 'system'
  subtype: 'init'
  cwd: string
  session_id: string
  tools: string[]
  mcp_servers: Array<{ name: string; status: 'connected' | 'connecting' | 'connection-failed' | 'disabled' }>
  agent_mode?: string
  reasoning_effort?: string
}
```

Assistant messages contain `text` and `tool_use` blocks, plus `thinking` / `redacted_thinking` only with `--stream-json-thinking`. They carry `stop_reason: "end_turn" | "tool_use" | "max_tokens" | null` and optional usage:

```ts
type Usage = {
  input_tokens: number
  max_tokens: number
  cache_creation_input_tokens?: number
  cache_read_input_tokens?: number
  output_tokens: number
  service_tier?: string
}
```

The token composition is per assistant response. Context numerator is `input_tokens + cache_creation_input_tokens + cache_read_input_tokens`; `output_tokens` is generated output and should not be double-counted into an input-side context gauge. `max_tokens` is the published divisor available to this transport.

Terminal results:

```ts
type Result =
  | { type: 'result'; subtype: 'success'; duration_ms: number; is_error: false; num_turns: number; result: string; session_id: string; usage?: Usage; permission_denials?: string[] }
  | { type: 'result'; subtype: 'error_during_execution' | 'error_max_turns'; duration_ms: number; is_error: true; num_turns: number; error: string; session_id: string; usage?: Usage; permission_denials?: string[] }
```

The public schema does not promise a dollar cost, model ID, transcript path, or process exit-code mapping. Derive the supervised result from the terminal object and separately capture the exact CLI exit behavior before making it RimZ's scripting contract.

`--stream-json-input` reads JSONL user messages until stdin closes and requires execute + stream JSON. Input supports text and base64 image blocks. `{ steer: true }` marks a message for the next interruption point. Amp exits only after stdin is closed and the agent is done, so RimZ must close the writer deliberately on cancellation and timeout.

## Remote control and runners

Amp threads sync with ampcode.com and can be continued, queued, steered, or cancelled from the web UI. Remote control is built in for a running CLI thread; individual users and workspace administrators can require recent passkey authentication.

`amp.remoteThreadCreation.enabled` (default `false`) lets ampcode.com create new threads in a running client's working directory. Every enabled client accepts new remote threads. This setting is command-executing product behavior and belongs in RimZ's trust hash if RimZ ever manages it.

`amp --no-tui` starts runner mode: a headless client that waits for remote threads. Multiple runners may run on one machine when started in different directories, and Amp identifies each by host plus working directory. Plugin executor metadata reports only `local | remote | unknown`; it does not expose the runner ID, pane, PID, or cwd per thread.

RimZ should not claim Amp remote-control readiness merely because the user is authenticated. A future badge must distinguish a normal running CLI thread, `remoteThreadCreation.enabled`, and a live `--no-tui` runner.

## Authentication, account, and spend

Interactive `amp login` writes credentials; `amp logout` removes the stored API key. Non-interactive use accepts `AMP_API_KEY`. `AMP_URL` selects a custom Amp service URL and is persisted during login when present, though the environment continues to take precedence.

The official credential location is `~/.local/share/amp/secrets.json` on Linux/macOS and `%USERPROFILE%\.local\share\amp\secrets.json` on Windows. Treat it as secret material: use presence/permissions only for preflight, never log or parse token values. The Plugin API's safer live identity surface is:

```ts
interface PluginSystem {
  readonly workspaceRoot: URI | null
  readonly ampURL: URL
  readonly user: User | null
  readonly executor: { kind: 'local' | 'remote' | 'unknown' }
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

`user == null` is the machine-readable live unauthenticated signal. Do not persist email or names in RimZ state when the opaque ID and workspace identity suffice.

`amp usage` prints the signed-in identity and current individual/workspace credit balance. `amp threads usage <T-id>` prints detailed per-thread cost when available. Verified on the refresh binary: neither `amp usage --help` nor `amp threads usage --help` exposes a `--json` flag or any machine-schema option, so an authoritative account probe must either parse explicitly version-gated human output or remain unsupported. (`amp tools list --json` does exist, so the absence on the spend commands is a deliberate gap, not a blanket policy.) `PluginThread.messages` and plugin events carry no costs. The private cache carries model/token records that support estimated spend, but those estimates do not reconcile credits or workspace billing.

Amp is pay-as-you-go rather than a subscription plan: individual and non-enterprise workspace usage is passed through at provider cost, credits are pooled for a workspace, and Enterprise has different pricing and optional entitlements. Model providers vary by Amp mode, so count Amp as the provider/account while retaining model IDs only where upstream exposes them.

## Configuration and trust

Amp reads JSON or JSONC user settings from `~/.config/amp/settings.{json,jsonc}` and the nearest workspace `.amp/settings.{json,jsonc}` found upward to the repository root (or cwd outside Git). `--settings-file` replaces the user-settings location. Workspace settings override user settings; user keymap entries are the documented exception and override workspace keymaps. Enterprise managed settings enforce policy from `/etc/ampcode/managed-settings.json` on Linux, `/Library/Application Support/ampcode/managed-settings.json` on macOS, or `%ProgramData%\ampcode\managed-settings.json` on Windows.

Relevant executable/security settings are:

| Setting | Adapter relevance |
| --- | --- |
| `amp.permissions` / `amp.guardedFiles.allowlist` / `amp.dangerouslyAllowAll` | activates legacy tool-policy plugin |
| `amp.remoteThreadCreation.enabled` | accepts cloud-created work in this local cwd |
| `amp.mcpServers` | local commands or remote endpoints exposed as tools |
| `amp.mcpPermissions` | allow/reject policy for MCP server startup |
| `amp.tools.disable` | changes observable tool vocabulary |
| `amp.defaultVisibility` | changes server-side thread sharing |
| `amp.thread.autoArchiveOnQuit` | archives current/background CLI threads on quit |
| `amp.updates.mode` | auto/warn/disabled binary updates; `AMP_SKIP_UPDATE_CHECK=1` overrides |

Project `.amp/plugins/*.ts`, workspace MCP commands, permission delegates, plugin install URLs, and settings that enable remote execution are command-executing trust surfaces. Include them in the effective trust hash and test each field. Amp separately asks for approval before starting workspace MCP servers; global settings and `--mcp-config` servers bypass that project approval.

Environment relevant to launch/preflight: `AMP_API_KEY`, `AMP_URL`, `AMP_SETTINGS_FILE`, `AMP_LOG_LEVEL`, `AMP_LOG_FILE`, `AMP_FORCE_BEL`, `AMP_SKIP_UPDATE_CHECK`, `HTTP_PROXY`, `HTTPS_PROXY`, and `NODE_EXTRA_CA_CERTS`.

## Mapping feasibility

The first adapter can land a useful honest subset:

| Concern | Coverage available from current upstream |
| --- | --- |
| turn lifecycle | wired: `agent.start` / `agent.end` |
| permission waiting | partial: `ThreadState`; full only for a RimZ-owned `tool.call` policy |
| plan approval | unsupported: no native event/state |
| user question | unsupported: no native event/state |
| external answer | partial: possible only when RimZ owns the `tool.call` waiter |
| tool activity / acting phase | wired: `tool.result` + `filesModifiedByToolCall` |
| compaction | unsupported: automatic but no event |
| built-in subagents | partial in supervised JSONL; unsupported in interactive plugin lifecycle |
| background parking | partial: multiple thread states exist, but no parent turn parking signal |
| session end | unsupported: reap from pane/process liveness |
| idle notification | partial: state transitions and native notifications setting, no notification event |
| context usage | partial from the private cache; no stable context-window divisor |
| realtime cost | partial estimated pricing from private-cache tokens |
| rich transcript | wired through paginated `PluginThread.messages`; best-effort offline history from the private cache |
| hook install | wired: one system plugin file |
| account identity | wired live through `PluginSystem.user` |
| account balance/spend | balance is human CLI only; spend is a partial private-cache estimate |
| remote control | native product surface; readiness requires live-process detection |

Before implementation, capture these unresolved contracts against the exact supported binary and convert each into a fixture or explicit unsupported declaration:

1. Whether a long-running `tool.call` handler may wait indefinitely, how Amp cancels it, and what happens when the plugin process exits.
2. How multiple plugins' `tool.call` results compose, including allow vs reject vs modify and handler exceptions.
3. Whether `ThreadState.awaiting-approval` brackets custom `ctx.ui.confirm` and legacy-permission dialogs identically, and the exact transition order relative to `tool.call`.
4. Whether built-in subagent work fires plugin events under the parent thread ID, a hidden child ID, or only as the parent `Task` call/result.
5. How plugin process identity can be joined to the correct RimZ pane when two Amp CLIs run in the same cwd; no documented payload field carries PID or pane identity.
6. Exact CLI exit codes for successful, errored, cancelled, permission-denied, timed-out, and plugin-readiness-failed execute runs.
7. The versioned output shapes, if RimZ chooses to use `amp usage`, `amp threads usage`, `amp threads export`, or `amp threads raw`. *(Partly settled on the refresh binary: `amp usage` and `amp threads usage` expose no `--json`; their output stays human text.)*
8. Whether `ultra` is accepted by `--mode` at runtime even when the help line lists only `low, medium, high`. *(Partly settled: the help line is narrow, but `amp plugins show-docs` still types `ultra` in `BuiltinAgentMode`. Runtime acceptance on a live account is still unverified.)*
9. Whether `amp -x` auto-archiving (default) leaves a thread that `amp threads continue <T-id>` can still resume, or whether RimZ must pass `--no-archive-after-execute` to keep supervised-run sessions resumable. The `amp review` code-review agent mode shares the same archive-after-run posture.

The primary architectural risk is Amp's one-client/many-thread model. Solve pane binding first: lifecycle normalization is straightforward once each event is assigned to the correct visible or background instance.
