# Pi protocol reference

> The mapping onto RimZ's internal types lives beside this doc: [adapter_pi.md](../../internals/agents/adapter_pi.md) owns the lifecycle, context, account, and spend mapping; the agent-agnostic model is [model.md](../../internals/agents/model.md) and the account/spend model is [providers.md](../../internals/agents/providers.md).

This is the single home for the **Pi upstream protocol surface** the RimZ adapter binds to — the in-process extension API (events, payloads, blocking returns, response headers), the session JSONL, the headless RPC/JSON modes, the auth file, and the CLI/env surface. It is a hand-maintained mirror of the pi.dev docs, refreshed against npm release [`@earendil-works/pi-coding-agent` 0.84.3](https://www.npmjs.com/package/@earendil-works/pi-coding-agent/v/0.84.3) and upstream tag [`v0.84.3`](https://github.com/earendil-works/pi/tree/v0.84.3) (`4e58f32`, 2026-08-24); compatibility behavior is additionally checked against a local 0.84.3 install. The code binding this surface is the adapter directory [`pi/`](../../../crates/rimz/src/agents/adapters/pi/mod.rs): the embedded [`extension.ts`](../../../crates/rimz/src/agents/adapters/pi/extension.ts) forwards lifecycle through the final `agent_settled` boundary, closes both successful and failed compactions, stamps token composition, four-source cumulative cost, and response-header windows, and gates `tool_call` on the blocking bridge; the read-only spending parser [`pi/spend.rs`](../../../crates/rimz/src/agents/adapters/pi/spend.rs) walks the session tree.

Coverage is **depth on what the adapter wires, breadth as an index**: the events, session fields, and decision returns [`src/agents/pi/`](../../../crates/rimz/src/agents/adapters/pi/mod.rs) parses or emits are documented in full; the rest of the catalog is listed so a contributor wiring a new path knows it exists. [Mapping feasibility](#mapping-feasibility) closes the doc with what remains unwired; the landed verdict is the [adapter_pi.md](../../internals/agents/adapter_pi.md).

## Upstream sources

Re-fetch these pages to refresh this mirror. Each `pi.dev/docs/latest/<page>` page renders `packages/coding-agent/docs/<page>.md` from the GitHub repo — the markdown is the higher-fidelity fetch. The website publishes a moving `latest`; pair a refresh with the npm version and Git tag, and use the tag's documentation tree for the version-pinned record. Exact TypeScript types ship in `node_modules/@earendil-works/pi-coding-agent/dist/`.

| Surface | Source |
| --- | --- |
| Extension API (events, payloads, returns, locations) | <https://pi.dev/docs/latest/extensions> |
| Session storage, resume flags, tree navigation | <https://pi.dev/docs/latest/sessions> |
| Session JSONL (header, entry types, message/usage shapes) | <https://pi.dev/docs/latest/session-format> |
| Settings (`extensions`, `packages`, `sessionDir`) | <https://pi.dev/docs/latest/settings> |
| Providers and auth (`auth.json`, `/login`, env vars) | <https://pi.dev/docs/latest/providers> |
| CLI flags, env vars, design scope statement | <https://pi.dev/docs/latest/usage> |
| RPC mode (headless JSONL protocol) | <https://pi.dev/docs/latest/rpc> |
| JSON event-stream mode | <https://pi.dev/docs/latest/json> |
| Pi packages (`pi install`) | <https://pi.dev/docs/latest/packages> |
| Version-pinned docs, types, and examples for this refresh | <https://github.com/earendil-works/pi/tree/v0.84.3/packages/coding-agent> — `docs/`, `src/core/extensions/types.ts`, `examples/extensions/` |
| Changelog | <https://github.com/earendil-works/pi/blob/v0.84.3/packages/coding-agent/CHANGELOG.md> |
| Extension type definitions | npm [`@earendil-works/pi-coding-agent`](https://www.npmjs.com/package/@earendil-works/pi-coding-agent) |
| Structured questionnaire extension | npm [`@juicesharp/rpiv-ask-user-question` 2.7.1](https://www.npmjs.com/package/@juicesharp/rpiv-ask-user-question/v/2.7.1); [package source](https://github.com/juicesharp/rpiv-mono/tree/v2.7.1/packages/rpiv-ask-user-question) |
| Async workflow subagents | [`pi-subagents` 0.58.0](https://github.com/nicobailon/pi-subagents/tree/v0.58.0) |
| In-process subagents | [`@tintinweb/pi-subagents` 0.18.2](https://github.com/tintinweb/pi-subagents/tree/v0.18.2) |

## Integration surface — in-process extensions

Pi's primary integration surface is **TypeScript extensions loaded in-process** (via jiti, no compile step) — it ships no out-of-process hook protocol or statusline. Pi 0.84.0 added an experimental remote-session client/protocol; RimZ does not adopt that unstable surface. The adapter remains a RimZ-authored extension file that subscribes to lifecycle events and shells out to the `rimz` CLI, holding pi's turn open from inside a handler when a decision must block.

> **Divergence — the decision channel inverts.** Claude and Codex run RimZ as a child and read its stdout as the decision. Pi runs RimZ's *extension* in-process; the extension runs `rimz` as *its* child, reads the answer from the child's stdout, and applies it through the handler's return value. Hook-stdout discipline becomes child-stdout discipline, and the sync-install invariant has no on-disk shape to enforce — blocking is awaiting inside the handler.

Discovery, in load order:

| Location | Scope |
| --- | --- |
| `~/.pi/agent/extensions/*.ts` and `*/index.ts` | global |
| `.pi/extensions/*.ts` and `*/index.ts` | project-local, loaded after project trust resolves |
| `settings.json` — `extensions: [paths]`, `packages: ["npm:…", "git:…"]` | configured |
| `pi -e <path>` / `--extension <source>` | per-invocation |

Install for RimZ means **one RimZ-owned file** written to `~/.pi/agent/extensions/` — auto-discovered, hot-reloaded by `/reload`, removed by deleting the file, idempotent by path. The file executes arbitrary code with the user's permissions (upstream states this explicitly), so it belongs in the executable-surface trust hash like every hook config. `--no-extensions` disables discovered extensions while retaining explicit `-e` paths; `-p` (print) and `--mode json` still run extensions but provide no UI — see [Mapping feasibility](#mapping-feasibility).

An extension default-exports a (sync or async) factory receiving `ExtensionAPI` — `pi.on(event, handler)`, `pi.registerCommand`, `pi.registerTool`, `pi.exec`, `pi.appendEntry` (persist extension state in the session), `pi.registerEntryRenderer`, `pi.setSessionName`, `pi.getThinkingLevel` (the RimZ wire's `effort`), `pi.events` (inter-extension bus). Every normal handler receives `ExtensionContext`; the startup-only `project_trust` event receives the smaller `ProjectTrustContext` instead:

| Field | Carries |
| --- | --- |
| `ctx.sessionManager` | read access to session state — `getSessionId()`, `getSessionFile()`, `getCwd()`, `getEntries()`, `getBranch()`, `getLeafId()` |
| `ctx.getContextUsage()` | live context-token usage for the active model |
| `ctx.ui` | dialogs (`confirm`, `select`, `input`, `editor` — each with an optional `timeout` auto-dismiss) and fire-and-forget (`notify`, `setStatus`, `setWidget`, `setTitle`) |
| `ctx.mode` / `ctx.hasUI` | `"tui" \| "rpc" \| "json" \| "print"`; `hasUI` is true in TUI and RPC only — gate every dialog on it |
| `ctx.cwd`, `ctx.signal`, `ctx.isIdle()`, `ctx.hasPendingMessages()`, `ctx.abort()`, `ctx.shutdown()` | working directory, the active turn's abort signal, control-flow helpers |
| `ctx.isProjectTrusted()` | effective trust for project-local settings and resources |
| `ctx.getSystemPrompt()`, `ctx.compact()` | current assembled system prompt and extension-triggered compaction |
| `ctx.modelRegistry` / `ctx.model` | model catalog — each model carries `provider`, `id`, `contextWindow`, `maxTokens`, and cost rates including optional request-wide input tiers |

Node built-ins (`node:fs`, `node:child_process`, …) and npm dependencies (via an adjacent `package.json`) are importable.

## Extension events

The lifecycle, condensed (the full diagram is in the [extensions doc](https://pi.dev/docs/latest/extensions)):

```text
launch         ─► project_trust ─► session_start { reason: "startup" }
prompt         ─► input ─► before_agent_start ─► agent_start
                   ┌─ turn (one LLM call; repeats while tools run) ─┐
                   │ turn_start ─► context ─► before_provider_headers│
                   │   ─► before_provider_request ─► after_provider_response│
                   │   tool_execution_start ─► tool_call (can block) │
                   │   ─► tool_execution_update* ─► tool_result      │
                   │   ─► tool_execution_end                         │
                   │ turn_end { message, toolResults }               │
                   └─────────────────────────────────────────────────┘
               ─► agent_end { messages } ─► retry/compact/follow-up? ─► agent_settled
/name          ─► session_info_changed
/compact, auto ─► session_before_compact ─┬► session_compact
                                          └► session_compact_failed
/new, /resume  ─► session_before_switch ─► session_shutdown ─► session_start { reason }
/fork, /clone  ─► session_before_fork ─► session_shutdown ─► session_start { reason: "fork" }
exit (Ctrl+C, Ctrl+D, SIGHUP, SIGTERM) ─► session_shutdown { reason: "quit" }
```

Note pi's vocabulary: a pi **turn** is one LLM call, while `agent_start`/`agent_end` bracket one low-level agent run. An `agent_end` can still lead to automatic retry, compaction-and-retry, or a queued follow-up; `agent_settled` is the final boundary RimZ calls a completed turn.

### Events an adapter would wire

| Event | Fires | Payload fields | Handler return |
| --- | --- | --- | --- |
| `session_start` | launch, `/new`, `/resume`, fork/clone, `/reload` | `reason` (`startup`\|`reload`\|`new`\|`resume`\|`fork`), `previousSessionFile?` | — |
| `before_agent_start` | prompt submitted, before the agent loop | `prompt`, `images?`, `systemPrompt`, `systemPromptOptions` | may inject a message / replace the system prompt |
| `agent_start` / `agent_end` / `agent_settled` | low-level run start/end; final settled idle | `agent_end.messages` — the run's messages; the last assistant message carries `stopReason` and `errorMessage?`; `agent_settled` has no fields | — |
| `turn_end` | per LLM call inside the loop | `turnIndex`, `message` (assistant, with `usage`), `toolResults` | — |
| `tool_call` | before a tool executes — **can block** | `toolName`, `toolCallId`, `input` (mutable) | `{ block: true, reason?: string }` blocks; mutations to `input` patch the call |
| `tool_execution_end` | after a tool finishes | `toolCallId`, `toolName`, `result`, `isError` | — |
| `session_before_compact` / `session_compact` | compaction begins / succeeds, manual or auto | `preparation` / `compactionEntry`, `fromExtension`; both carry `reason` (`manual`\|`threshold`\|`overflow`) and `willRetry` | `before` may cancel or supply a custom summary |
| `session_compact_failed` | compaction fails or is aborted; no success event follows | `reason`, `errorMessage?`, `aborted`, `willRetry`, `fromExtension` | — |
| `session_shutdown` | quit (incl. Ctrl+C/SIGHUP/SIGTERM), `/new`, `/resume`, fork, `/reload` | `reason` (`quit`\|`reload`\|`new`\|`resume`\|`fork`), `targetSessionFile?` | — |
| `model_select` | `/model`, `Ctrl+P` cycling, session restore | `model`, `previousModel?`, `source` (`set`\|`cycle`\|`restore`) — `model` carries `contextWindow`/`maxTokens`/cost rates | — |
| `thinking_level_select` | thinking level change | `level` (`off`\|`minimal`\|`low`\|`medium`\|`high`\|`xhigh`\|`max`), `previousLevel` | — |

Session identity rides `ctx.sessionManager` rather than event payloads: `getSessionId()`, `getSessionFile()`, and `getCwd()` are valid from the first `session_start` — at launch, with no lazy-registration window.

### Event index (the rest)

`project_trust` (global and CLI extensions only; first `yes`/`no` decision wins), `resources_discover`, `session_info_changed`, `session_before_switch`, `session_before_fork`, `session_before_tree` / `session_tree` (the latter can carry a usage-bearing `summaryEntry`), `message_start` / `message_update` / `message_end` (streaming; `message_end` may replace the finalized message), `context` (mutate the message list before each LLM call), `before_provider_headers` (mutate request headers), `before_provider_request` (inspect/replace the provider payload), `after_provider_response` (`status`, normalized `headers` — the response-header surface), `tool_execution_start` / `tool_execution_update`, `tool_result` (patch `content`/`details`/`isError` and carries optional usage, middleware-chained), `input` (`text`, `images?`, `source`: `interactive`\|`rpc`\|`extension`, `streamingBehavior`; can transform or handle), `user_bash`.

Pi 0.84.3 emits `session_compact_failed` on manual failure, exhausted overflow recovery, extension cancellation, abort, and automatic-compaction failure. None of those paths emits `session_compact` afterward; all currently report `willRetry: false`. RimZ therefore treats the failure as the closing half of the lifecycle bracket without claiming a context reset.

### Blocking, dialogs, and error handling

`tool_call` is the one blocking return — `{ block: true, reason }` — and pi imposes **no deadline** on a handler: an awaiting handler holds the turn open indefinitely, so any cap is the extension's own (e.g. `ctx.ui` dialogs take a `timeout` option that auto-dismisses with a countdown — `confirm` resolves `false`, `select`/`input` resolve `undefined`). Tool preflight is sequential even in parallel tool mode, so a blocked `tool_call` also delays the assistant message's sibling tools.

Extension error handling is fail-safe in the right direction: a handler exception is logged and the agent continues, **except** in `tool_call`, where an exception blocks the tool. A custom tool's `execute` signals failure by throwing (sets `isError: true`); its return value never does.

### `rpiv-ask-user-question` questionnaire

RimZ consumes the questionnaire tool wire from [`@juicesharp/rpiv-ask-user-question` 2.7.1](https://www.npmjs.com/package/@juicesharp/rpiv-ask-user-question/v/2.7.1), whose source lives in the [`rpiv-mono` package directory](https://github.com/juicesharp/rpiv-mono/tree/v2.7.1/packages/rpiv-ask-user-question). The extension registers `ask_user_question`; Pi's awaited `tool_call` carries `input` in this shape:

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

The tool finishes through `tool_execution_end.result = { content, details }`. `details` is `{ answers, cancelled, globalNote?, error? }`; each answer is `{ questionIndex, question, kind, answer, selected?, notes?, preview? }`, where `kind` is `option`, `custom`, or `multi`, and `selected` carries multi-select labels. `cancelled: true` covers Esc/decline and may retain already confirmed answers, but RimZ records none from a cancelled result. Errors include `no_custom_ui`, `session_load_failed`, and `stale_module_cache`. RimZ forwards `result.details` only for this tool; it deliberately does not project `globalNote` into per-question `AskAnswer` records because the note has no question-scoped home and already reaches the model in the questionnaire envelope.

Focus starts at option zero. Every question appends a Type something. row, including multi-select and preview-carrying questions; focusing it enters the inline editor, and Enter commits a `custom` answer. Single-select Enter commits an option immediately. Multi-select appends Next after the custom row: Space or Enter toggles an option, while Enter on Next commits the checked labels. Up/Down wrap across the flat rows; one Down from the custom row exits input mode and advances to Next. Multiple questions add a final Submit tab with `Submit answers` focused above `Cancel`; `n` opens per-question notes or a global note on that tab, and Esc cancels the dialog except while closing a notes editor.

The extension bus publishes `rpiv:ask-user:prompt` before the dialog and `rpiv:ask-user:blocked { active }` around the awaited UI. RimZ does not subscribe to those package-specific events; the generic Pi prompt events planned upstream are the preferred future attention signal.

### `pi-subagents` child process shape

[`pi-subagents` 0.58.0](https://github.com/nicobailon/pi-subagents/tree/v0.58.0) launches both async and foreground children as `pi --mode json -p` subprocesses. Both spawn paths merge the current `process.env` into the child environment, and argument construction leaves globally discovered extensions enabled by default. Agent frontmatter that declares `extensions:` adds `--no-extensions` and reloads only the named extension paths; the generated child environment includes `PI_SUBAGENT_CHILD_AGENT` with the agent name.

### `@tintinweb/pi-subagents` in-process children

[`@tintinweb/pi-subagents` 0.18.2](https://github.com/tintinweb/pi-subagents/tree/v0.18.2) creates each child in-process through `createAgentSession`, assigns its session name, and binds extensions. Its resource loader discovers global extensions by default; an agent configuration with `extensions: false` sets `noExtensions` and omits them from the child.

## Session JSONL

Pi writes one session file per conversation:

```text
~/.pi/agent/sessions/--<cwd-with-/-as-->--/<timestamp>_<uuid>.jsonl
e.g.   sessions/--home-user-workspace-project-rimz-rimz--/2026-06-04T06-45-56-308Z_019e9161-a5d0-791d-879e-39679acd4ded.jsonl
```

The directory key is the working directory with `/` replaced by `-`; the filename stem is `<ISO timestamp, : and . as ->_<session uuid>`, so the session id is everything after the first `_`. Overrides: `--session-dir`, `PI_CODING_AGENT_SESSION_DIR`, settings `sessionDir`; `--no-session` skips persistence entirely. [`pi/spend.rs`](../../../crates/rimz/src/agents/adapters/pi/spend.rs) walks this tree fleet-wide for spending (its `PI_AGENT_DIR` env is a RimZ test override, not a pi variable).

The first line is the header; every later line is a tree entry (`id` is 8-char hex, `parentId` links it, `timestamp` is ISO):

```jsonc
{"type":"session","version":3,"id":"<session uuid>","timestamp":"2026-07-09T06:45:56.308Z","cwd":"/home/user/…","parentSession":"<path, fork/clone only>"}
```

Sessions are versioned (v1 linear → v2 tree → v3 renamed `hookMessage` → `custom`) and auto-migrate on load; parse around the `version` field. The header's `parentSession` is optional.

| Entry `type` | Carries |
| --- | --- |
| `message` | a `message` object — see below |
| `model_change` | `provider`, `modelId` — the user switched models mid-session |
| `thinking_level_change` | `thinkingLevel` |
| `compaction` | `summary`, `firstKeptEntryId`, `tokensBefore`, `details?`, `fromHook?`, optional top-level `usage` |
| `branch_summary` | `fromId`, `summary`, optional top-level `usage` — an abandoned branch's summary |
| `custom` / `custom_message` | extension state (`customType`, `data?`) / extension-injected context |
| `label` | `targetId`, `label` — `/tree` bookmarks |
| `session_info` | `name` — the `/name` display name |

The `message` roles are `user`, `assistant`, `toolResult`, `bashExecution` (`!` commands), `custom`, `branchSummary`, `compactionSummary`; message-level `timestamp` is Unix **ms**, unlike the ISO entry envelope. Assistant messages always carry `usage`; tool results may carry nested `message.usage`. The assistant shape follows the 0.84.3 types; `responseModel`, `responseId`, and redacted `diagnostics` are optional, so parse tolerantly:

```jsonc
{"type":"message","id":"a1b2c3d4","parentId":"…","timestamp":"2026-06-04T06:46:14.308Z","message":{
  "role": "assistant",
  "provider": "openai-codex", "model": "gpt-5.5", "responseModel": "gpt-5.6-sol", "api": "…", "responseId": "…",
  "content": [ {"type":"text","text":"…"} ],            // also "thinking", "toolCall" blocks
  "stopReason": "stop",                                  // stop | length | toolUse | error | aborted
  "errorMessage": "…",                                   // present on error
  "usage": { "input": 3435, "output": 6, "cacheRead": 0, "cacheWrite": 0, "cacheWrite1h": 0, "reasoning": 4, "totalTokens": 3441,
             "cost": { "input": 0.017175, "output": 0.00018, "cacheRead": 0, "cacheWrite": 0, "total": 0.017355 } }
}}
```

Two properties matter for any tail read:

- **Session totals have four usage sources.** Pi includes assistant-message usage (attributed to `responseModel ?? model`), optional tool-result `message.usage`, and top-level `usage` on `compaction` and `branch_summary` entries. RimZ counts all four. A present non-negative `usage.cost.total` is authoritative, including zero; a token-bearing assistant record without it falls back to model pricing. The token split mirrors Anthropic's: context tokens are `input + cacheRead + cacheWrite`; optional `cacheWrite1h` is a subset of cache writes, and optional `reasoning` is a subset of output, so neither adds to `totalTokens`. The transcript carries **no context window**, so a gauge divisor resolves from the model registry (`contextWindow` per model) or a table, the way Claude's payload model resolves its divisor.
- **RimZ's envelope carries the same live dollar sum.** `turn_end` adds the assistant message plus every tool-result usage; `session_compact` adds its `compactionEntry.usage`; `session_tree` adds a carried branch-summary usage. `agent_end` contributes no additional cost. `/resume` hydrates all four sources on the active branch while retaining the latest assistant usage gauge; the settled-boundary JSONL spend walk reconciles the authoritative session total.
- **The file is a tree, not a log.** `/tree` and `/fork` move the leaf to an earlier entry in place, so file order is append order, not branch order — the newest record by file position can sit on an abandoned branch right after a rewind. `buildSessionContext()` (the upstream context builder) walks leaf→root; a bounded tail read is an approximation that self-corrects on the next turn.

## Headless modes (index)

Recorded for breadth; a RimZ adapter targets the interactive TUI in a pane.

- **`--mode json`** — every session event as JSON lines on stdout. The union is the low-level `AgentEvent` set (including `agent_settled`) plus `queue_update`, `compaction_start` / `compaction_end` (`reason: manual|threshold|overflow`, with end carrying `willRetry` and a result whose `estimatedTokensAfter` is heuristic), and `auto_retry_start` / `auto_retry_end`. Extension compaction events carry the same `reason` and `willRetry` distinction, including `session_compact_failed` when no success follows.
- **`--mode rpc`** — bidirectional JSONL over stdio for embedding: commands (`prompt`, `steer`, `follow_up`, `abort`, `get_state`, `get_messages`, `get_entries`, `get_tree`, `get_session_stats`, `set_model`, `new_session`, `switch_session`, `fork`, `compact`, `bash`, …) correlate by `id` + `success`; the same events stream interleaved; extension `ctx.ui` dialogs surface as an RPC sub-protocol. `get_entries` supports an entry-id `since` cursor and includes abandoned/pre-compaction history; `get_tree` returns the full entry tree.
- **SDK** — `createAgentSession({ customTools, … })` embeds the agent loop in another Node program: <https://pi.dev/docs/latest/sdk>.

## Auth file

`~/.pi/agent/auth.json` (created `0600`) maps provider → credential; `/login` / `/logout` manage OAuth entries (live-verified shapes):

```jsonc
{
  "anthropic":    { "type": "oauth", "access": "…", "refresh": "…", "expires": <epoch ms> },
  "openai-codex": { "type": "oauth", "access": "…", "refresh": "…", "expires": <epoch ms>, "accountId": "…" },
  "openai":       { "type": "api_key", "key": "sk-…", "env": { "HTTP_PROXY": "http://proxy" } }
}
```

OAuth subscription logins: ChatGPT Plus/Pro (`openai-codex`), Claude Pro/Max (`anthropic` — upstream notes third-party usage bills per token as extra usage, outside plan limits), GitHub Copilot. An `api_key` credential may carry provider-scoped `env` values; these precede the process environment for credential interpolation, provider configuration, headers, and proxy/cache settings. The `key` accepts literals, `$ENV_VAR` / `${ENV_VAR}` interpolation, or a leading `!command`; plain uppercase strings are literals. API keys resolve in order: `--api-key` flag → `auth.json` → provider env var (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, …) → `models.json` custom-provider keys.

Non-interactive credential helpers expose the resolved provider auth without changing the file shape: `pi auth print-api-key --provider <provider> [--model <model>]`, `pi auth print-bearer-token --provider <provider> [--model <model>] [--min-expiry <duration>]`, and `pi auth check --provider <provider> [--json] [--credentials] [--no-refresh]`. RimZ continues to read `auth.json` directly and does not invoke these commands.

**Balance windows.** Pi exposes no plan tier, but RimZ derives windows from two local surfaces. The extension captures per-response headers via `after_provider_response` and publishes the resulting windows immediately: Codex OAuth traffic uses `x-codex-primary-*` and `x-codex-secondary-*`, while Anthropic OAuth traffic may expose `anthropic-ratelimit-unified-*` variants. The idle authoritative path reads the active OAuth credential from `auth.json` and reuses the Claude or Codex OAuth usage endpoint, including the `openai-codex.accountId` header when present. API-key credentials remain unmetered. See [adapter_pi.md → Account and balance](../../internals/agents/adapter_pi.md#account-and-balance) for how the dashboard fuses and caches these readings.

## CLI and environment surface

The flags and variables an adapter (and the resume-on-rebirth planner) cares about:

| Surface | Meaning |
| --- | --- |
| `pi --version` | version (`0.84.3` refresh target); current releases write plain output to stdout, while RimZ captures both streams for older releases |
| `pi -c` / `pi -r` | continue the most recent session / browse and pick |
| `pi --session <path\|id>` / `--session-id <id>` | resume a path/partial UUID, or use an exact project session id (creating it when absent) |
| `pi --model <provider/id\|model:level>` | select the model; the optional `:level` suffix selects thinking effort |
| `pi --provider <name>` | select the provider for a model name that omits one |
| `pi --thinking <off\|minimal\|low\|medium\|high\|xhigh\|max>` | select reasoning effort; `max` is opt-in and clamps to model support |
| `pi --fork <path\|id>`, `--no-session`, `--name <n>` | fork into a new file, ephemeral mode, display name |
| `pi -e <source>`, `--no-extensions` | load an extension / disable discovery |
| `--approve` / `--no-approve` | trust or ignore project-local settings and resources for this run; non-interactive modes do not draw a trust prompt |
| `-p`, `--mode json`, `--mode rpc` | headless modes (above) |
| `pi install npm:…\|git:…` / `pi remove` / `pi list` | pi package management — the distribution channel an npm-published RimZ extension would use |
| `pi auth print-api-key` / `print-bearer-token` / `auth check` | non-interactive credential resolution and validation; not used by the adapter |
| `PI_CODING_AGENT_DIR` | config root override (default `~/.pi/agent`) |
| `PI_CODING_AGENT_SESSION_DIR` | session-dir override (below `--session-dir`) |
| `PI_PACKAGE_DIR`, `PI_OFFLINE`, `PI_SKIP_VERSION_CHECK`, `PI_TELEMETRY` | package root, startup-network and telemetry switches |

## Mapping feasibility

The landed verdict — the native-event → signal table, the blocking `tool_call` gate and its decision shape, the turn-death and identity properties, and the install shape — is [adapter_pi.md](../../internals/agents/adapter_pi.md). Upstream's own scope statement ([usage.md](https://pi.dev/docs/latest/usage)) frames the gaps: pi intentionally ships **no built-in MCP, sub-agents, permission popups, plan mode, to-dos, or background bash** — those are extension territory. The session name, context gauge, token split, model/effort enrichment, and best-effort `total_cost_usd` ride every hook envelope; value-changing native events publish immediately, while `message_update` is change-deduplicated and throttled to one push per second per session, so no transcript-tail gauge is needed. On Pi 0.80.4+, RimZ retains the last `agent_end` verdict and applies it at `agent_settled`, avoiding a false idle boundary while Pi still has an automatic retry, compaction retry, or queued follow-up; the extension version-gates this path and normalizes older Pi's historical `agent_end` boundary to the same RimZ event.

The account probe is wired: [`pi/account.rs`](../../../crates/rimz/src/agents/adapters/pi/account.rs) reads `auth.json` (oauth → metered subscription, api_key → unmetered), labels the sub the fleet uses — the freshest session's `message.provider` picks among several credentials, else the first OAuth entry — and the separate adapter version probe attaches `pi --version`; mapping summary in [adapter_pi.md → Account and balance](../../internals/agents/adapter_pi.md#account-and-balance).

Wired increments from this refresh are complete: model, thinking level, session name, four-source usage/cost, and provider-window changes push immediate enrichment; streaming context changes push at most once per second; resume hydrates the active branch; `agent_settled` is the final RimZ turn boundary; successful and failed compactions both close the transient bracket; and compaction `reason` maps `manual` to manual while `threshold` and `overflow` map to automatic lifecycle completion. The optional Windows `powershell` tool is mutating but not file-editing, matching `bash`. `willRetry` remains on the RimZ-authored wire for diagnostics and future retry-specific presentation.

Post-0.84.3 upstream development adds `ui_prompt_start` / `ui_prompt_end`, which could eventually provide a package-neutral attention signal and supersede questionnaire-specific bus events. RimZ waits for a released contract before wiring them. The experimental remote-session protocol is likewise deliberately unadopted.
