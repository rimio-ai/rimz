# Pi protocol reference

> RimZ's mapping of this surface lives in [adapter_pi.md](../../internals/agents/adapter_pi.md): the lifecycle signals, subagent lineage, context, account, and spend mapping, and the known gaps. The provider-neutral agent model is [model.md](../../internals/agents/model.md), and the account and spend model is [providers.md](../../internals/agents/providers.md).

This page mirrors the upstream surface of Pi (`@earendil-works/pi-coding-agent`) that a RimZ adapter binds to: the in-process extension API with its events, payloads, and blocking returns; the session JSONL; the headless JSON and RPC modes; the auth file; and the CLI and environment. It also records the third-party Pi extensions whose wire RimZ reads. Coverage is depth on what the adapter wires ([`extension.ts`](../../../crates/rimz/src/agents/adapters/pi/extension.ts), [`spend.rs`](../../../crates/rimz/src/agents/adapters/pi/spend.rs), [`account.rs`](../../../crates/rimz/src/agents/adapters/pi/account.rs), [`ask.rs`](../../../crates/rimz/src/agents/adapters/pi/ask.rs)) and breadth as an index for the rest.

**Baseline.** Pi [1.1.0](https://www.npmjs.com/package/@earendil-works/pi-coding-agent/v/1.1.0), tag [`v1.1.0`](https://github.com/earendil-works/pi/tree/v1.1.0) (commit `abe508e1b89912adde45528136c3221eb69acdd7`), released 2026-10-07; npm's `latest` release, docs, types, and source checked 2026-10-12 against the release checkout. The [third-party extensions](#third-party-extensions) section pins each package to its own release. Repository paths below are relative to `packages/coding-agent/` at that tag unless they name another package. This is an upstream reference, not a claim that RimZ already wires every new surface.

## Upstream sources

Each `pi.dev/docs/latest/<page>` renders `packages/coding-agent/docs/<page>.md` from the repository. The site tracks the moving latest release, so a refresh reads the markdown at the release tag. Exact TypeScript types ship in the npm package under `dist/` (`dist/core/extensions/types.d.ts`, `dist/core/session-manager.d.ts`) and in `@earendil-works/pi-ai` (`dist/types.d.ts`).

| Surface | Source |
| --- | --- |
| Extension API: events, payloads, returns, context, locations | <https://pi.dev/docs/latest/extensions> |
| Sessions: storage, resume flags, tree navigation | <https://pi.dev/docs/latest/sessions> |
| Session JSONL: header, entry types, message and usage shapes | <https://pi.dev/docs/latest/session-format> |
| Message roles, content blocks, and tool results | <https://pi.dev/docs/latest/message-types> |
| Settings (`extensions`, `packages`, `sessionDir`, `defaultProjectTrust`) | <https://pi.dev/docs/latest/settings> |
| Project trust | <https://pi.dev/docs/latest/security> |
| Providers and auth (`auth.json`, `/login`, key resolution) | <https://pi.dev/docs/latest/providers> |
| Interactive usage | <https://pi.dev/docs/latest/usage> |
| CLI option and command reference | <https://pi.dev/docs/latest/cli> |
| Environment variables and process markers | <https://pi.dev/docs/latest/environment-variables> |
| RPC mode | <https://pi.dev/docs/latest/rpc> |
| JSON event-stream mode | <https://pi.dev/docs/latest/json> |
| Pi packages (`pi install`) | <https://pi.dev/docs/latest/packages> |
| SDK (`createAgentSession`) | <https://pi.dev/docs/latest/sdk> |
| Built-in MCP and codemode | <https://pi.dev/docs/latest/mcp>; <https://pi.dev/docs/latest/codemode> |
| Terminal program status | <https://pi.dev/docs/latest/terminal-setup#program-status> |
| Version-pinned docs and types | <https://github.com/earendil-works/pi/tree/v1.1.0/packages/coding-agent>: `docs/`, `src/core/extensions/types.ts` |
| Changelog | <https://github.com/earendil-works/pi/blob/v1.1.0/packages/coding-agent/CHANGELOG.md> |
| Structured questionnaire extension | npm [`@juicesharp/rpiv-ask-user-question` 2.13.6](https://www.npmjs.com/package/@juicesharp/rpiv-ask-user-question/v/2.13.6); [source](https://github.com/juicesharp/rpiv-mono/tree/v2.13.6/packages/rpiv-ask-user-question) |
| Subagent extension (nicobailon) | [`pi-subagents` v0.77.0](https://github.com/nicobailon/pi-subagents/tree/v0.77.0) |
| Subagent extension (tintinweb) | [`@tintinweb/pi-subagents` v0.21.0](https://github.com/tintinweb/pi-subagents/tree/v0.21.0) |

## Extensions

Pi's integration surface is TypeScript extensions loaded in-process through [jiti](https://github.com/unjs/jiti), with no compile step. Pi ships no out-of-process hook protocol and no statusline command. An extension subscribes to events with `pi.on(event, handler)`; a handler that must decide something returns the decision, and a handler that must wait awaits inside itself while Pi holds the turn.

> **Divergence: the decision channel inverts.** Claude Code and Codex run a hook command as their child and read its stdout as the decision. Pi runs the extension inside its own process, so an extension that consults an external program spawns that program as *its* child, reads the child's stdout, and applies the answer through the handler's return value. Blocking is an awaited promise, and there is no installed hook config whose sync or async shape could be checked on disk.

Extensions run with the user's full permissions and can execute arbitrary code ([extensions.md → Extension Locations](https://pi.dev/docs/latest/extensions#extension-locations)). Pi discovers them from these locations:

| Location | Scope |
| --- | --- |
| `~/.pi/agent/extensions/*.ts` and `~/.pi/agent/extensions/*/index.ts` | global |
| `.pi/extensions/*.ts` and `.pi/extensions/*/index.ts` | project-local; loaded only after the project is trusted |
| `settings.json` `extensions: [paths]` and `packages: ["npm:…", "git:…"]` | configured |
| `pi -e <path>` / `--extension <path>` | one run; repeatable |

Extensions in the discovered locations hot-reload with `/reload`, which fires `session_shutdown` and `session_start` with `reason: "reload"`. `--no-extensions` (`-ne`) disables discovery and built-in extensions but still loads explicit `-e` paths, including `builtin:<name>`. Before project trust resolves, Pi loads only context files, global extensions, and `-e` extensions, and only those can answer the `project_trust` event ([security.md → Project Trust](https://pi.dev/docs/latest/security#project-trust)).

An extension default-exports a sync or async factory that receives `ExtensionAPI`. The package also exports `VERSION`, the running Pi version string, and `CONFIG_DIR_NAME` for building project-local paths. Node built-ins and npm dependencies from an adjacent `package.json` are importable.

| `ExtensionAPI` member | Purpose |
| --- | --- |
| `on(event, handler)` | subscribe to an [event](#extension-events); returns an unsubscribe function |
| `registerTool`, `registerCommand`, `registerShortcut`, `registerFlag` / `getFlag` | add tools, slash commands, key bindings, and CLI flags |
| `registerProvider` | add a model provider |
| `registerVirtualModel`, `unregisterVirtualModel` | register selectable routers; assistant messages record the physical model that answered |
| `registerMessageRenderer`, `registerEntryRenderer`, `registerMarkdownTransformer`, `registerToolRenderer` | TUI rendering for custom messages, entries, and tools, including tools not registered yet |
| `registerMcpServer`, `unregisterMcpServer`, `getMcpServers` | add, remove, or inspect built-in MCP server configurations |
| `sendMessage`, `sendUserMessage` | inject a custom message or a user message (`deliverAs: "steer"` or `"followUp"` while streaming) |
| `appendEntry(customType, data?)` | persist extension state as a `custom` session entry |
| `setSessionName`, `getSessionName`, `setLabel` | session display name and `/tree` labels |
| `exec(command, args, options?)` | run a subprocess with optional `signal` and `timeout` |
| `getActiveTools`, `getAllTools`, `setActiveTools`, `getCommands` | tool and command registry |
| `getSettings()` | a copy of effective merged settings |
| `setModel`, `getThinkingLevel`, `setThinkingLevel` | model and thinking level for the current session |
| `events` | inter-extension event bus |

Every handler except `project_trust` receives `ExtensionContext` (`src/core/extensions/types.ts`, `ExtensionContext`). `project_trust` receives `ProjectTrustContext`, which carries only `cwd`, `mode`, `hasUI`, and a `ui` limited to `select`, `confirm`, `input`, and `notify`.

| `ctx` member | Carries |
| --- | --- |
| `sessionManager` | read-only session state (`ReadonlySessionManager`): `getSessionId()`, `getSessionFile()`, `getSessionDir()`, `getCwd()`, `getHeader()`, `getEntries()`, `getEntry()`, `getBranch()`, `getTree()`, `getLeafId()`, `getLeafEntry()`, `getLabel()`, `buildContextEntries()`, `getSessionName()` |
| `getContextUsage()` | `{ tokens: number \| null, contextWindow: number, percent: number \| null }` or `undefined`; `tokens` and `percent` are `null` right after compaction, before the next response |
| `model`, `modelRegistry` | the active model and the catalog; a model carries `provider`, `id`, `contextWindow`, `maxTokens`, and cost rates |
| `thinkingLevel`, `scopedModels` | the effective thinking level; the models scoped by `--models` or `enabledModels` |
| `ui` | dialogs `select`, `confirm`, `input`, `editor`, `custom`; fire-and-forget `notify`, `setStatus`, `setWidget`, `setTitle`, `setEditorText` |
| `mode`, `hasUI` | run mode and whether dialogs work; see the table below |
| `cwd`, `signal` | working directory; the active turn's abort signal, usually `undefined` outside a turn |
| `isIdle()`, `hasPendingMessages()`, `abort()`, `shutdown()` | control flow; `isIdle()` is false during a run, automatic retry, compaction retry, or queued continuation |
| `isProjectTrusted()` | effective project trust, including one-run overrides |
| `getSystemPrompt()`, `compact(options?)` | the assembled system prompt; start a compaction without awaiting it |

| Mode | `ctx.mode` | `ctx.hasUI` | Behavior |
| --- | --- | --- | --- |
| Interactive TUI | `"tui"` | `true` | full UI |
| `--mode rpc` | `"rpc"` | `true` | dialogs travel over the RPC extension UI sub-protocol; `custom()` returns `undefined` |
| `--mode json` | `"json"` | `false` | UI methods are no-ops |
| `-p` | `"print"` | `false` | extensions run but cannot prompt |

## Extension events

The lifecycle, condensed from [extensions.md → Lifecycle Overview](https://pi.dev/docs/latest/extensions#lifecycle-overview):

```text
launch         ─► project_trust ─► session_start { reason: "startup" } ─► resources_discover
prompt         ─► input ─► before_agent_start ─► agent_start
                   ┌─ turn (one LLM call; repeats while tools run) ──────────┐
                   │ turn_start ─► context ─► before_provider_headers        │
                   │   ─► before_provider_request ─► after_provider_response │
                   │   tool_execution_start ─► tool_call (can block)         │
                   │   ─► tool_execution_update* ─► tool_result              │
                   │   ─► tool_execution_end                                 │
                   │ turn_end { message, toolResults }                       │
                   └─────────────────────────────────────────────────────────┘
               ─► agent_end { messages } ─► retry / compaction / follow-up?
               ─► agent_before_settle ─► continuation? ─► agent_settled { aborted }
ctx.ui dialog  ─► ui_prompt_start ─► (user answers) ─► ui_prompt_end
/name          ─► session_info_changed
/compact, auto ─► session_before_compact ─┬► session_compact
                                          └► session_compact_failed
/new, /resume  ─► session_before_switch ─► session_shutdown ─► session_start { reason }
/fork, /clone  ─► session_before_fork ─► session_shutdown ─► session_start { reason: "fork" }
/model, Ctrl+P ─► thinking_level_select (when the level changes or clamps) ─► model_select
exit (Ctrl+C, Ctrl+D, SIGHUP, SIGTERM) ─► session_shutdown { reason: "quit" }
```

A Pi **turn** is one LLM call. `agent_start` and `agent_end` bracket one low-level agent run, and an `agent_end` can still be followed by an automatic retry, a compaction-and-retry, or a queued follow-up. `agent_settled` fires once none of those remains; it exists from Pi 0.80.4 (CHANGELOG 0.80.4), and earlier releases end at `agent_end`.

Session identity comes from `ctx.sessionManager`, never from event payloads. `getSessionId()`, `getSessionFile()`, and `getCwd()` return values from the first `session_start` at launch.

### Events the adapter wires

| Event | Fires | Payload fields | Handler return |
| --- | --- | --- | --- |
| `session_start` | launch, `/new`, `/resume`, fork or clone, `/reload` | `reason` (`startup`\|`reload`\|`new`\|`resume`\|`fork`), `previousSessionFile?` (for `new`, `resume`, `fork`) | none |
| `before_agent_start` | prompt submitted, before the agent loop | `prompt`, `images?`, `systemPrompt`, `systemPromptOptions` | may inject a message or replace the system prompt |
| `agent_end` | one low-level run ends | `messages`; the last assistant message carries `stopReason`, `errorMessage?`, `usage` | none |
| `agent_settled` | the run fully settles | `aborted` (boolean, added in 1.1.0) | none |
| `turn_end` | each LLM call ends | `turnIndex`, `message` (assistant, with `usage`), `toolResults` (each may carry `usage`), `messageEntryId`, `toolResultEntryIds`, boundary state | may append boundary entries and request continuation |
| `after_provider_response` | provider HTTP response received, before the stream is read | `status`, `headers` (`Record<string, string>`) | none |
| `message_update` | each assistant streaming delta | `message`, `assistantMessageEvent` | none |
| `session_info_changed` | `/name` or `pi.setSessionName()` | `name` (`undefined` when cleared) | none |
| `session_tree` | after `/tree` navigation | `newLeafId`, `oldLeafId`, `summaryEntry?` (a `branch_summary` entry, may carry `usage`), `fromExtension?` | none |
| `tool_call` | before a tool executes; **can block** | `toolCallId`, `toolName`, `input` (mutable) | `{ block: true, reason?, terminate? }` blocks; mutating `input` patches the call |
| `tool_execution_end` | a tool finishes | `toolCallId`, `toolName`, `result`, `isError`, `durationMs?` (execution time; absent if the tool did not run), `parentToolCallId?` (nested call) | none |
| `model_select` | `/model`, Ctrl+P cycling, session restore | `model`, `previousModel?`, `source` (`set`\|`cycle`\|`restore`) | none |
| `thinking_level_select` | thinking level changes | `level`, `previousLevel`; levels are `off`\|`minimal`\|`low`\|`medium`\|`high`\|`xhigh`\|`max` | none |
| `session_before_compact` | compaction begins, manual or automatic | `preparation`, `branchEntries`, `customInstructions?`, `reason`, `willRetry`, `signal` | may cancel or supply a custom summary |
| `session_compact` | compaction succeeds | `compactionEntry` (may carry `usage`), `fromExtension`, `reason`, `willRetry` | none |
| `session_compact_failed` | compaction fails or is aborted | `reason`, `errorMessage?`, `aborted`, `willRetry`, `fromExtension` | none |
| `session_shutdown` | quit (including Ctrl+C, SIGHUP, SIGTERM), `/new`, `/resume`, fork, `/reload` | `reason` (`quit`\|`reload`\|`new`\|`resume`\|`fork`), `targetSessionFile?` | none |

Header names in `after_provider_response` arrive lowercase for fetch-based providers because Pi copies `Headers.entries()` unchanged (`packages/ai/src/utils/headers.ts`, `headersToRecord`). Pi itself does not normalize them, and no Google adapter emits the event.

Compaction is a bracket. `reason` is `manual` (`/compact`), `threshold` (the context threshold), or `overflow` (overflow recovery), and `willRetry` is true when the aborted turn retries after a successful compaction. At 1.1.0 every `session_compact_failed` emit site passes `willRetry: false` (`src/core/agent-session.ts`, `_emitSessionCompactFailed` and its three callers). An unrecoverable overflow can emit failure before `session_before_compact`, so consumers must not require an opening extension event for every failure.

### Extension UI prompt events

`ui_prompt_start` and `ui_prompt_end` exist from Pi 0.84.4 (CHANGELOG 0.84.4, #8355). They are notification-only: Pi wraps `ctx.ui.select`, `confirm`, `input`, `editor`, and `custom` and emits the pair around the wait, delivering each through `queueMicrotask` without awaiting handlers (`src/core/extensions/runner.ts`, `ExtensionRunner.withUIPrompt`).

| Field | Value |
| --- | --- |
| `reason` | always `"ui_prompt"` |
| `kind` | `select`\|`confirm`\|`input`\|`editor`\|`custom` |
| `title?` | the prompt title; absent for `custom` |

Nested or overlapping prompts coalesce into one outer span, so only the outermost prompt emits and `ui_prompt_end` repeats its `kind` and `title`. Neither event reports how the prompt ended (answer, cancel, or timeout). The pair also fires in RPC mode; there `custom()` returns immediately, so its span has no real wait. The events stream only to extensions; the JSON and RPC event streams do not carry them.

### Event index

| Event | Carries or does |
| --- | --- |
| `project_trust` | `cwd`; global and `-e` extensions only; returns `{ trusted: "yes"\|"no"\|"undecided", remember? }`, and the first yes or no wins |
| `resources_discover` | `cwd`, `reason` (`startup`\|`reload`); returns extra `skillPaths`, `promptPaths`, `themePaths` |
| `mcp_servers_change` | `servers`: all extension-registered servers after a registration change; startup registrations are read with `getMcpServers()` on `session_start` |
| `session_before_switch` | `reason` (`new`\|`resume`), `targetSessionFile?`; can cancel |
| `session_before_fork` | `entryId`, `position` (`before`\|`at`); can cancel |
| `session_before_tree` | `preparation`, `signal`; can cancel or customize the branch summary |
| `input` | `text`, `images?`, `source` (`interactive`\|`rpc`\|`extension`), `streamingBehavior?` (`steer`\|`followUp`); returns `continue`, `transform`, or `handled` |
| `agent_start` | no fields |
| `turn_start` | `turnIndex`, `timestamp` |
| `context` | `messages` without system messages; may replace conversation messages before each LLM call, then Pi restores prompt and tools |
| `context_with_system` | full `messages` after `context`; may replace the complete transcript, taking responsibility for prompt and tool declarations |
| `provider_stream_event` | `provider`, `api`, `model`, `data`: parsed provider stream data before normalization |
| `cache_warming_decision` | `warmCost`, `missCost`, `continuationProbability`, `action` (`warm`\|`stop`); may return `{ action? }` (`src/core/cache-warmer.ts`) |
| `agent_before_settle` | boundary state before final settlement; may append entries and request one more provider call |
| `before_provider_headers` | `headers`; mutate in place, `null` deletes a header |
| `before_provider_request` | `payload`; may inspect or replace it |
| `message_start` / `message_end` | `message`; `message_end` may return a replacement with the same `role` |
| `tool_execution_start` / `tool_execution_update` | `toolCallId`, `toolName`, `args`; the update adds `partialResult` |
| `tool_result` | `toolCallId`, `toolName`, `input`, `content`, `details`, `isError`, `usage?`, `structuredContent?`, `parentToolCallId?`; handlers chain and may patch the result |
| `user_bash` | `command`, `excludeFromContext` (`!!`), `cwd`; may handle the command |

`turn_end` and `agent_before_settle` expose `entries`, `continue`, a context preview, and `outcome` (`completed`\|`aborted`\|`error`). Their return is `{ entries?, continue? }`; drafts can be `custom`, `custom_message`, `context_edit`, or `compaction`. This is a durable context-edit/continuation surface, distinct from a `tool_call` permission decision (`src/core/extensions/types.ts`, `BoundaryState`, `BoundaryResult`).

Custom tools receive `ExtensionToolContext.executeTool()` for nested calls through Pi's tool runner, and may implement `prepareLoadout()` to control tools and prompt sections visible to the model. Tool definitions support `exposure`, `namespace`, `annotations`, `outputSchema`, and `defaultActive`. Active tools are model-declared tools, not the full callable set. Nested calls use ids `<parent>/<n>` and carry `parentToolCallId` on call/result/execution events; the parent's tool-result message stores bounded `nestedCalls` instead of separate nested transcript messages. A `tool_result` handler that replaces `content` without replacing `structuredContent` clears the latter (`src/core/extensions/types.ts`; `src/core/extensions/runner.ts`, `emitToolResult`).

### Blocking, dialogs, and errors

`tool_call` is the only blocking return. Pi awaits each `tool_call` handler in load order with no timeout, and the first `{ block: true }` short-circuits the handlers after it (`src/core/extensions/runner.ts`, `ExtensionRunner.emitToolCall`). An awaiting handler therefore holds the turn open indefinitely. `terminate: true` on a blocked call asks Pi to stop after the current tool batch, and takes effect only when every finalized result in the batch terminates.

In the default parallel tool mode, Pi preflights sibling tool calls from one assistant message sequentially and then executes them concurrently. A blocked or slow `tool_call` therefore delays its siblings, and `ctx.sessionManager` inside `tool_call` may lack sibling results from the same message.

Dialog caps belong to the extension. `select`, `confirm`, and `input` accept a `timeout` option that auto-dismisses with a countdown; on timeout `confirm` resolves `false` and `select` and `input` resolve `undefined`.

Ordinary notification-handler errors are logged and the agent continues. `tool_call` exceptions block the tool. `user_bash` exceptions or invalid defined returns stop dispatch and local execution; return `undefined` to continue, or exactly one valid `{ operations }` or `{ result }`. Custom tools can signal failure by throwing or returning `isError: true`; the latter preserves `details` and `structuredContent` (`src/core/extensions/runner.ts`, `emitUserBash`; `packages/agent/src/types.ts`, `AgentToolResult`).

## Third-party extensions

Pi leaves tool-permission policies, structured question tools, and subagents to extensions. Built-in project trust and provider/MCP login dialogs are separate surfaces. The three extensions below shape what RimZ reads, and each section is pinned to its own release.

### `rpiv-ask-user-question` questionnaire

[`@juicesharp/rpiv-ask-user-question` 2.13.6](https://www.npmjs.com/package/@juicesharp/rpiv-ask-user-question/v/2.13.6) registers the `ask_user_question` tool and draws a structured questionnaire in Pi's TUI through `ctx.ui.custom`. Pi's awaited `tool_call` carries its `input`:

```jsonc
{
  "questions": [{
    "question": "Which route?",
    "header": "Route",
    "options": [{ "label": "Safe", "description": "Stage it", "preview": "optional markdown" }],
    "multiSelect": false
  }]
}
```

The package normalizes line terminators in `question`, `header`, and each option's `label`, `description`, and `preview` at tool entry: `\r\n` becomes `\n` and a lone `\r` is deleted (2.10.0). From 2.13.4 it also decodes JSON-string arrays/objects in `questions`, individual questions, `options`, and individual options before validation (`tool/normalize-params.ts`, `prepareQuestionArguments`).

The tool finishes through `tool_execution_end` with `result = { content, details }`:

| `details` field | Carries |
| --- | --- |
| `answers` | one entry per answered question: `{ questionIndex, question, kind, answer, selected?, notes?, preview? }` |
| `answers[].kind` | `option` (a listed option), `custom` (single-select typed text), or `multi` (`selected` lists checked labels and `answer` carries optional custom text, or `null`) |
| `cancelled` | `true` on Esc or decline; already confirmed answers may remain in `answers` |
| `globalNote?` | a questionnaire-wide note from the Submit tab |
| `unanswered?` | question indexes left blank on a partial submit; absent when all questions are answered or the result is a decline |
| `error?` | UI errors `no_ui`, `no_custom_ui`; validation errors `no_questions`, `empty_options`, `too_many_questions`, `duplicate_question`, `duplicate_option_label`, `reserved_label`; loading errors `session_load_failed`, `stale_module_cache` |

The keyboard model follows. Focus starts at option zero, and Up and Down wrap across the flat rows.

| Question shape | Rows and keys |
| --- | --- |
| every question | a final `Type something.` row; focusing it enters the inline editor |
| single-select | Enter on an option commits it immediately |
| single-select custom row | Enter commits a `custom` answer |
| multi-select | a `Next` row follows the custom row; Space or Enter toggles an option. Enter in the custom editor or on `Next` commits one `multi` answer containing both checked labels and custom text; one Down from the custom row leaves input mode and lands on `Next` |
| several questions | a final Submit tab with `Submit answers` focused above `Cancel`; `n` opens per-question notes, or the global note on that tab |

Esc cancels the dialog, except while a notes editor is open, where it closes the editor. Around the awaited UI the package publishes `rpiv:ask-user:prompt` and then `rpiv:ask-user:blocked { active }` on `pi.events`. The dialog also triggers Pi's own [`ui_prompt_start` / `ui_prompt_end`](#extension-ui-prompt-events) with `kind: "custom"`.

### nicobailon `pi-subagents`

[`pi-subagents` v0.77.0](https://github.com/nicobailon/pi-subagents/tree/v0.77.0) (npm release 2026-10-10) runs every child as a native Pi `AgentSession` from `createAgentSession`; releases before v0.65.0 spawned `pi --mode json -p` subprocesses and set `PI_SUBAGENT_CHILD_AGENT`, which no longer exists. Paths are relative to that tag.

| | Foreground child | Background child |
| --- | --- | --- |
| Process | inside the parent Pi process (`src/runs/shared/child-session.ts`) | inside a detached runner process; one runner can host several children (`src/runs/background/async-execution.ts`, `spawnRunner`) |
| Runner launch | none | a standalone `pi` binary runs `pi --no-extensions --no-skills --no-prompt-templates --no-session --mode rpc --extension <binary-bootstrap.ts>`; otherwise Node runs JavaScript directly, TypeScript with native stripping when supported, or jiti as fallback. A configured agent `launcher` can wrap the runner argv |
| Global extensions | never loaded (`src/runs/shared/child-launch.ts`, `ambientExtensions` is false for the parent host) | loaded unless the agent declares `extensions` or a capability ceiling denies extensions |
| Extensions that load | pi-subagents runtime extensions, path-like `tools` entries, the agent's `extensions` list, and `subagentOnlyExtensions`; `subagents.defaultExtensions` in settings fills a missing list | the same set, plus global extensions when allowed |
| Module state | a fresh module instance per child; `process.env` and `globalThis` are the parent's | the runner's `process.env` and `globalThis`, shared by its children |
| Environment | the parent's | the parent's `process.env` minus `PI_SUBAGENT_EXTENSION_BINDINGS` and Git routing variables; the runner sets `PI_SUBAGENT_CHILD=1` |
| Session name | `"<agent>: <task excerpt>"` (at most 80 characters), set through `pi.setSessionName` in the child's `before_agent_start`, so absent at `session_start` | same |
| Session JSONL | standard Pi files below `<parent session file dir>/<parent basename>/<runId>/run-<n>` | below `<parent session file dir>/<parent basename>/async-<id>` |

The package now declares `@earendil-works/pi-ai >=0.86.1`; the old pinned `pi-server` dependency is gone. Child settings inherit the launching session's project-trust decision (`src/runs/shared/child-launch.ts`, `projectTrusted`; `src/runs/shared/child-session.ts`, `SettingsManager.create`), so declining project trust does not become permission to load project-local extensions in a child.

### tintinweb `@tintinweb/pi-subagents`

[`@tintinweb/pi-subagents` v0.21.0](https://github.com/tintinweb/pi-subagents/tree/v0.21.0) (2026-10-12) requires Pi 1.1.0 or newer and creates each child in-process through `createAgentSession` (`src/agent-runner.ts`). The resource loader discovers global extensions unless `extensions` is `false`, which sets `noExtensions`; an `isolated` launch forces `extensions` to `false`, and the agent config's value takes precedence over the caller's. The child inherits the parent's project trust; a cross-directory child reports its own cwd as untrusted while retaining the parent project's permitted settings and extensions. The child session is named after the agent config's `name` (or its type), suffixed `#<first 8 characters of the agent id>` when one exists. A child persists its session through `SessionManager.create` when `persistSession` resolves true (nested agents default to in-memory), and otherwise lives in memory.

## Session JSONL

Pi writes one session file per conversation ([session-format.md](https://pi.dev/docs/latest/session-format)):

```text
~/.pi/agent/sessions/--<cwd-with-/-as-->--/<timestamp>_<uuid>.jsonl
e.g.   sessions/--home-user-workspace-project-rimz-rimz--/2026-06-04T06-45-56-308Z_019e9161-a5d0-791d-879e-39679acd4ded.jsonl
```

The directory key removes the leading path separator and replaces `/`, `\`, and `:` with `-`, then wraps the result in `--`. The filename stem is the ISO timestamp with `:` and `.` replaced by `-`, then `_`, then the session id (a UUID by default; the SDK and `--session-id` can supply a custom id). `--session-dir` overrides the location, then `PI_CODING_AGENT_SESSION_DIR`, then settings `sessionDir`; `--no-session` writes nothing. Subagent extensions nest child sessions below the parent's directory ([nicobailon `pi-subagents`](#nicobailon-pi-subagents)).

The first line is the header; every later line is a tree entry with an `id` (usually 8-character hex, with a full-UUID fallback), a `parentId` (`null` on a root entry), and an ISO `timestamp`:

```jsonc
{"type":"session","version":3,"id":"<session uuid>","timestamp":"2026-07-09T06:45:56.308Z","cwd":"/home/user/…","parentSession":"<path, fork/clone only>"}
```

The session format is at version 3 (`CURRENT_SESSION_VERSION`): v1 was linear, v2 added the `id`/`parentId` tree, and v3 renamed the `hookMessage` role to `custom`. Pi migrates older files on load, so a reader outside Pi keys on `version`. `parentSession` is present only for a fork, clone, or `newSession({ parentSession })`.

| Entry `type` | Carries |
| --- | --- |
| `message` | a `message` object; see below |
| `model_change` | `provider`, `modelId` |
| `thinking_level_change` | `thinkingLevel` |
| `usage` | `kind` (arbitrary string, e.g. `cache_warm`), `provider`, `model`, `usage`, `note?`; accounting only, outside LLM context |
| `compaction` | `summary`, `firstKeptEntryId`, `tokensBefore`, `details?`, `fromHook?`, `usage?`, `systemMessage?` (prompt/tool checkpoint) |
| `context_edit` | `targetId`, `replacement` (`{ content }` or `null`); branch-relative replacement or omission in future model context, without changing raw history or accounting |
| `branch_summary` | `fromId`, `summary`, `details?`, `fromHook?`, `usage?`; the summary of an abandoned branch |
| `custom` | extension state: `customType`, `data?`; outside LLM context |
| `custom_message` | extension-injected context: `customType`, `content`, `display`, `details?` |
| `label` | `targetId`, `label`; `/tree` bookmarks |
| `session_info` | `name`; the `/name` display name |

`firstKeptEntryId` remains required; a retain-none compaction stores its own id. The 1.1.0 session-format docs now agree with the CLI wire: no `retainedTail`. `systemMessage`, when present, is the complete prompt/tool checkpoint for compacted context and replaces system messages among retained entries (`src/core/session-manager.ts`, `CompactionEntry`).

Message roles are `system`, `user`, `assistant`, `toolResult`, `bashExecution` (`!` commands), `custom`, `branchSummary`, and `compactionSummary`. A message's own `timestamp` is Unix milliseconds, unlike the ISO entry envelope. System messages persist the prompt and tool loadout: the first request records the complete state, and subsequent messages patch named `sections` (`null` removes a section) and `toolsAdded`/`toolsRemoved`. Older sessions gain their first system message on their next request. The assistant shape follows `AssistantMessage` in `packages/ai/src/types.ts`:

```jsonc
{"type":"message","id":"a1b2c3d4","parentId":"…","timestamp":"2026-06-04T06:46:14.308Z","message":{
  "role": "assistant",
  "provider": "openai-codex", "model": "gpt-5.5", "responseModel": "gpt-5.6-sol", "api": "…", "responseId": "…",
  "content": [ {"type":"text","text":"…"} ],            // also "thinking" and "toolCall" blocks
  "stopReason": "stop",                                  // stop | length | toolUse | error | aborted
  "errorMessage": "…",                                   // present on error
  "usage": { "input": 3435, "output": 6, "cacheRead": 0, "cacheWrite": 0, "cacheWrite1h": 0, "reasoning": 4, "totalTokens": 3441,
             "cost": { "input": 0.017175, "output": 0.00018, "cacheRead": 0, "cacheWrite": 0, "total": 0.017355 } },
  "timestamp": 1780555574308
}}
```

| Assistant field | Present when |
| --- | --- |
| `responseModel?`, `responseId?` | the provider reports the serving model or response id |
| `errorMessage?` | `stopReason` is `error` or `aborted` |
| `diagnostics?` | the provider attached diagnostics |
| `rawStopReason?` | the provider's native stop reason (from 0.83.0) |
| `endTurn?` | the provider says whether the model ended its turn (from 0.84.2) |
| `providerThinkingLevel?` | the exact provider effort, set by Anthropic Messages when `supportsMidConvoEffort` is on (from 0.85.0) |
| `thinkingLevel?` | Pi's requested thinking level for the response; absent on legacy messages or outside the agent loop |
| `durationMs?` | request duration; absent on legacy responses |

The pi-ai `StopReason` type also lists `pending` and `deferred`. `pending` marks partial streaming messages and never reaches the file ([session-format.md](https://pi.dev/docs/latest/session-format)). `deferred`, with a `deferred` handle, is available to providers that implement deferred responses.

`usage` has four token counters (`input`, `output`, `cacheRead`, `cacheWrite`) plus `totalTokens` and a `cost` object in dollars. Two optional counters are subsets and never add to `totalTokens`: `cacheWrite1h` is the part of `cacheWrite` written with one-hour retention (Anthropic only), and `reasoning` is the part of `output` spent thinking. Context tokens split the Anthropic way, as `input + cacheRead + cacheWrite`. The file carries no context window; a gauge resolves its divisor from the model's `contextWindow` in the registry.

Usage appears in five places, and Pi's session totals include all five: the assistant message's `usage` (attributable to `responseModel ?? model`), a tool result's optional `message.usage` for LLM work the tool performed, the optional top-level `usage` on `compaction` and `branch_summary` entries, and standalone `usage` entries. Unknown `usage.kind` values remain ordinary billable usage. A selected virtual model can differ from the physical `provider`/`model` that an assistant message records ([session-format.md](https://pi.dev/docs/latest/session-format)).

Tool-result messages may also carry `durationMs` and bounded `nestedCalls`; nested calls are stored on the parent result and are not separate model-context messages (`packages/ai/src/types.ts`, `ToolResultMessage`).

The file is a tree, not a log. `/tree` moves the leaf in place; `/fork` creates a new session file with the selected branch. File order is append order, so a tail can end on an abandoned branch right after a rewind. `buildSessionContext()` walks leaf to root; a bounded tail read only approximates the active branch.

## Headless modes

Pi's headless modes and SDK expose the same session events outside a TUI. RimZ's adapter targets the interactive TUI in a pane, so this section is an index.

| Mode | Wire |
| --- | --- |
| `--mode json` | JSON lines on stdout: the session header first, then every `AgentSessionEvent` ([json.md](https://pi.dev/docs/latest/json)) |
| `--mode rpc` | JSONL commands on stdin, responses and events on stdout, split on LF only ([rpc.md](https://pi.dev/docs/latest/rpc)) |
| SDK | `createAgentSession({ customTools, … })` embeds the agent loop in a Node program ([sdk.md](https://pi.dev/docs/latest/sdk)) |

The SDK now takes an async `modelRuntime` instead of the old `authStorage`/`modelRegistry` options. `ModelRuntime` owns model discovery and authentication; the extension-facing `ctx.modelRegistry` remains supported, but its `refresh()` is async and must be awaited before synchronous reads. Custom providers stream from `TranscriptContext`, which carries prompt/tool state in system messages instead of top-level context fields ([changelog](https://github.com/earendil-works/pi/blob/v1.1.0/packages/coding-agent/CHANGELOG.md); `src/core/extensions/types.ts`, `ProviderConfig`).

Both streams write every `AgentSessionEvent` through the same `toJsonEvent` projection (`src/modes/json-event.ts`), whose `message_update` records are delta-only: they omit the cumulative `message` and `assistantMessageEvent.partial` and carry a top-level cumulative `usage`; `toolcall_start` retains `id` and `toolName`. `message_end` is the authoritative completed message. Nested tool execution events carry `parentToolCallId?`. Beyond the `AgentEvent` set (`agent_start`, `turn_*`, `message_*`, `tool_execution_*`), `AgentSessionEvent` adds these (`src/core/agent-session.ts`, `AgentSessionEvent`):

| Event | Carries |
| --- | --- |
| `agent_end` | `messages` plus `willRetry`, true when an automatic retry follows |
| `agent_settled` | `aborted`; the run fully settled |
| `queue_update` | `steering`, `followUp`: the full pending queues |
| `compaction_start` / `compaction_end` | `reason` (`manual`\|`threshold`\|`overflow`); the end adds `result` (absent on abort or failure; its `estimatedTokensAfter` is heuristic), `aborted`, `willRetry`, `errorMessage?` |
| `auto_retry_start` / `auto_retry_end` | `attempt`, `maxAttempts`, `delayMs`, `errorMessage` / `success`, `attempt`, `finalError?` |
| `summarization_retry_scheduled` / `summarization_retry_attempt_start` / `summarization_retry_finished` | retries of compaction or branch-summary summarization |
| `bash_execution_update` | `id?`, `delta`: output of an RPC `bash` command |
| `entry_appended`, `session_info_changed`, `thinking_level_changed` | `entry`, `name`, `level`; in the type union but absent from json.md and rpc.md |

RPC mode also writes `extension_error` (`extensionPath`, `event`, `error`) when an extension throws.

RPC commands correlate by an optional `id` echoed in a `type: "response"` object with `success`. `prompt` returns `data.disposition: "started"|"queued"|"handled"`; `steer` and `follow_up` return `"queued"|"handled"`. `handled` means an extension consumed the input, so no new run or `agent_settled` is promised for that input. Extension `ctx.ui` dialogs travel as an extension UI sub-protocol of requests on stdout and responses on stdin (`src/modes/rpc/rpc-types.ts`).

| RPC command group | Commands |
| --- | --- |
| Prompting | `prompt`, `steer`, `follow_up`, `abort` (waits for idle before responding), `clear_queue` (returns and removes the queued `steering` and `followUp` messages), `new_session` |
| State | `get_state`, `get_messages` |
| Model and thinking | `set_model`, `cycle_model`, `get_available_models`, `set_thinking_level`, `cycle_thinking_level`, `get_available_thinking_levels` |
| Queues, compaction, retry | `set_steering_mode`, `set_follow_up_mode`, `compact`, `set_auto_compaction`, `set_auto_retry`, `abort_retry` |
| Bash | `bash`, `abort_bash` |
| Session | `get_session_stats`, `export_html`, `switch_session`, `fork`, `clone`, `get_fork_messages`, `get_entries`, `get_tree`, `get_last_assistant_text`, `set_session_name`, `get_commands` |

`get_entries` returns entries in append order, including abandoned branches and pre-compaction history, plus the current `leafId`; its `since` entry-id cursor returns only later entries and fails when the id is unknown. `get_tree` returns `{ entry, children, label?, labelTimestamp? }` nodes.

## Auth file

`~/.pi/agent/auth.json` maps a provider id to one credential. Pi creates it with mode `0600`, `/login` and `/logout` manage it, and OAuth tokens refresh automatically when expired ([providers.md](https://pi.dev/docs/latest/providers)). The credential shape is `OAuthCredential` or an API-key credential (`packages/ai/src/auth/types.ts`):

```jsonc
{
  "anthropic":      { "type": "oauth", "access": "…", "refresh": "…", "expires": <epoch ms> },
  "openai-codex":   { "type": "oauth", "access": "…", "refresh": "…", "expires": <epoch ms>, "accountId": "…" },
  "github-copilot": { "type": "oauth", "access": "…", "refresh": "…", "expires": <epoch ms>, "enterpriseUrl": "…" },
  "openai":         { "type": "api_key", "key": "sk-…", "env": { "HTTP_PROXY": "http://proxy" } }
}
```

| OAuth provider id | Login | Extra credential fields |
| --- | --- | --- |
| `anthropic` | Claude Pro/Max; third-party usage bills per token as extra usage, outside plan limits | none |
| `openai` | Sign in with ChatGPT for the OpenAI Responses API (not the Decisions API) | `clientId`, `scopes` |
| `openai-codex` | ChatGPT Plus/Pro (Codex) | `accountId` |
| `github-copilot` | GitHub Copilot | `enterpriseUrl` |
| `xai` | xAI (Grok/X subscription) | none |
| `openrouter` | OpenRouter; mints an API key billed from OpenRouter credits | `refresh: ""`, `expires: Number.MAX_SAFE_INTEGER` |
| `radius` | Radius gateway (default `https://radius.pi.dev`); `models.json` entries with `"oauth": "radius"` add more | none |
| `kimi-coding` | Kimi For Coding | none |
| `meta` | Meta Muse subscription; device login mints a Model API key | `access` is the minted key; `refresh` is the identity token used to re-mint it |

The provider ids come from the built-in provider catalog in `packages/ai/src/providers/all.ts` and each provider's `auth.oauth` at 1.1.0. `/login` enumerates those OAuth methods, including `kimi-coding` (`src/modes/interactive/interactive-mode.ts`, `getLoginProviderOptions`). Meta re-mints expiring keys automatically, but a dead identity session requires a new login (`packages/ai/src/auth/oauth/meta.ts`). The Azure API-key provider is now `azure`, renamed from `azure-openai-responses` in 1.0.3; stored provider keys and model/settings references need the new id.

An `api_key` credential's `key` accepts a literal, `$ENV_VAR` or `${ENV_VAR}` interpolation (also inside a larger string), or a leading `!command` whose stdout Pi caches for the process lifetime. `$$` and `$!` escape a literal `$` or `!`, and a plain uppercase string such as `MY_API_KEY` is a literal. Optional `env` values take precedence over the process environment for that provider's key interpolation, headers, and settings such as proxies and `PI_CACHE_RETENTION`.

Pi resolves a provider's credential in this order: the `--api-key` flag, then the `auth.json` entry, then the provider's environment variable (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, …), then custom-provider keys in `models.json`.

Non-interactive `pi auth` subcommands expose resolved credentials (`src/cli/auth-command.ts`). Resolution can refresh and persist stored OAuth credentials; `auth check --no-refresh` uses read-only storage. Each requires `--provider`, `--model`, or both. OpenAI's new ChatGPT login also persists a stable `deviceId` in global settings. Anthropic supports workload identity federation through `ANTHROPIC_FEDERATION_RULE_ID`, `ANTHROPIC_ORGANIZATION_ID`, and `ANTHROPIC_IDENTITY_TOKEN_FILE` when no key or token is set ([providers.md](https://github.com/earendil-works/pi/blob/v1.1.0/packages/coding-agent/docs/providers.md)).

| Command | Extra flags | Output |
| --- | --- | --- |
| `pi auth print-api-key` | none | the resolved API key |
| `pi auth print-bearer-token` | `--min-expiry <n>(ms\|s\|m\|h)` | an OAuth bearer token, refreshed when it would expire sooner |
| `pi auth check` | `--json`, `--credentials` (include the credential), `--no-refresh` (skip refreshing expired OAuth) | provider readiness |

Pi exposes no plan tier, quota, or usage endpoint of its own. The only per-response provider metadata an extension sees is the `status` and `headers` of [`after_provider_response`](#events-the-adapter-wires). How RimZ derives balance windows from those headers and the stored OAuth tokens is in [adapter_pi.md → Account and balance](../../internals/agents/adapter_pi.md#account-and-balance).

## CLI and environment

These are the flags a launch or resume plan uses; `pi --help` lists the rest.

| Flag | Meaning |
| --- | --- |
| `[messages...]`, `[@files...]` | initial prompt text and attached files; the interactive TUI submits the messages at startup |
| `--` | end option parsing; each later argument is a message, except one starting with `@`, which is a file (`src/cli/args.ts`, `parseArgs`) |
| `--version`, `-v` | print the version to stdout |
| `--continue`, `-c` / `--resume`, `-r` | continue the most recent session / browse and pick one |
| `--session <path\|id>` | open a session file or a partial session UUID |
| `--session-id <id>` | use an exact project session id, creating the session when absent |
| `--fork <path\|id>` | fork a session file or partial UUID into a new session |
| `--session-dir <dir>`, `--no-session` | session storage directory; ephemeral mode |
| `--name`, `-n <name>` | session display name |
| `--provider <name>` | provider for a model pattern without one; requires `--model` |
| `--model <pattern>` | model pattern or id; accepts `provider/id` and a `:<thinking>` suffix |
| `--models <patterns>` | comma-separated patterns for Ctrl+P cycling |
| `--thinking <level>` | `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, or `max` |
| `--system-prompt <text>`, `--append-system-prompt <text>` | replace the system prompt; append text or file contents (repeatable). Any `--append-system-prompt` replaces the discovered `APPEND_SYSTEM.md` instead of adding to it (`src/core/resource-loader.ts`, `DefaultResourceLoader.reload`) |
| `--extension`, `-e <path>`, `--no-extensions`, `-ne` | load an extension; disable discovery and built-ins. Explicit `-e builtin:<name>` still loads that built-in |
| `--tools`, `-t`, `--exclude-tools`, `-xt`, `--no-tools`, `-nt`, `--no-builtin-tools`, `-nbt` | tool allowlist, denylist, and defaults |
| `--no-mcp` | disable built-in MCP for this run |
| `--approve`, `-a` / `--no-approve`, `-na` | trust or ignore project-local files for this run; non-interactive modes never show a trust prompt |
| `--print`, `-p`, `--mode json\|rpc` | [headless modes](#headless-modes) |
| `--tui-mode fullscreen\|regular` | interactive terminal layout; defaults to fullscreen |
| `--offline` | disable startup network operations (same as `PI_OFFLINE=1`) |

`--tools` and `--exclude-tools` accept `*` patterns. A list containing only `+name`/`-name` entries adjusts default tools instead of replacing them; these take exact names and cannot mix with plain entries. `--tools` keeps MCP tools callable unless an entry starts with `mcp__`; `--exclude-tools` also filters MCP tools. `--no-tools` disables all tools, including MCP ([cli.md → Tools](https://pi.dev/docs/latest/cli#tools)).

| Command | Meaning |
| --- | --- |
| `pi install <source> [-l]`, `pi remove <source> [-l]`, `pi list` | manage Pi packages (`npm:…`, `git:…`, URLs, local paths) in global settings, or project `.pi/settings.json` with `-l` |
| `pi update [source\|self\|pi]` | update Pi, packages, or model catalogs |
| `pi auth <command>` | [credential helpers](#auth-file) |
| `pi mcp add`, `remove`, `list`, `login`, `logout` | manage built-in MCP servers and their authentication without a session ([cli.md → MCP commands](https://pi.dev/docs/latest/cli#mcp-commands)) |

| Variable | Meaning |
| --- | --- |
| `PI_CODING_AGENT_DIR` | config root (default `~/.pi/agent`); moves `auth.json`, `extensions/`, and `sessions/` |
| `PI_CODING_AGENT_SESSION_DIR` | session directory; `--session-dir` overrides it |
| `PI_PACKAGE_DIR` | package directory, for Nix or Guix store paths |
| `PI_OFFLINE` | disable startup network operations; implies `PI_SKIP_VERSION_CHECK` |
| `PI_SKIP_VERSION_CHECK` | skip the `pi.dev` latest-version request (read by `src/utils/version-check.ts`; absent from `pi --help`) |
| `PI_TELEMETRY` | override install and update telemetry and provider attribution headers |
| `PI_CACHE_RETENTION` | `long` requests extended provider prompt caching where supported |
| `PI_PROGRAM_STATUS` | `1` forces terminal program-status reports without capability negotiation; `0` disables them |

Pi also sets variables for the processes it starts ([environment-variables.md](https://pi.dev/docs/latest/environment-variables)). The CLI and RPC entry points set `AI_AGENT=pi` and `PI_CODING_AGENT=true`, which every child inherits. Commands run by the model's `bash` and `powershell` tools additionally receive `PI_SESSION_ID`, `PI_SESSION_FILE` (unset for ephemeral sessions), `PI_PROVIDER`, `PI_MODEL`, and `PI_REASONING_LEVEL`, resolved when each command starts; user-typed `!` commands do not.

## Upstream scope

Pi now ships built-in MCP support and the optional `codemode` and `tool_search` tools. MCP connects over stdio or streamable HTTP (not legacy SSE), reads global `~/.pi/agent/mcp.json` and trusted project `.pi/mcp.json`, and names tools `mcp__<server>__<tool>`. MCP OAuth state lives separately in `~/.pi/agent/mcp-auth.json`, keyed by server name and URL. Extensions can register servers through `pi.registerMcpServer()`. Provider/MCP login dialogs are native, but subagents, structured questionnaires, and tool-permission policies remain extension surfaces ([MCP](https://github.com/earendil-works/pi/blob/v1.1.0/packages/coding-agent/docs/mcp.md), [CLI tools](https://github.com/earendil-works/pi/blob/v1.1.0/packages/coding-agent/docs/cli.md#tools); `src/extensions/mcp/oauth.ts`, `McpOAuthCredentialStore`).

From 1.1.0, interactive Pi also reports terminal program status through OSC 7501: `working`, `blocked`, `done`, `error`, or `idle`. Dialogs and logins produce `blocked`; aborted settlement returns to `idle`. Reports include session names, dialog titles, or the first error line, never prompts or assistant output. Pi normally waits for a positive capability response; `PI_PROGRAM_STATUS` overrides it. This is terminal output, not a hook or JSON/RPC event ([terminal-setup.md → Program status](https://github.com/earendil-works/pi/blob/v1.1.0/packages/coding-agent/docs/terminal-setup.md#program-status); `src/modes/interactive/program-status-reporter.ts`).

Pi's experimental remote harness (the `server` and `client` commands and the `client` and `experimental/plugin` package subpaths) remains source-only at 1.1.0. It runs from a checkout with `PI_EXPERIMENTAL=1`, and the published npm runtime and standalone binaries exclude it ([services README](https://github.com/earendil-works/pi/blob/v1.1.0/packages/coding-agent/src/experimental/services/README.md); `package.json`, `files` and `exports`). Which surfaces RimZ wires, and the gaps that follow, are in [adapter_pi.md → Known gaps](../../internals/agents/adapter_pi.md#known-gaps).
