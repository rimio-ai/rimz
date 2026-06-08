# Pi protocol reference

> The mapping onto Rimz's internal types lives beside this doc: [hooks.md → Appendix Pi](../../internals/hooks.md#appendix--pi) owns the lifecycle mapping and install shape, [transcript.md](../../internals/transcript.md) the context read-path, [account.md](../../internals/account.md) the account/balance mapping.

This is the single home for the **Pi upstream protocol surface** the Rimz adapter binds to — the in-process extension API (events, payloads, blocking returns), the session JSONL, the headless RPC/JSON modes, the auth file, and the CLI/env surface. It is a hand-maintained mirror of the pi.dev docs, pinned to the source URLs below; the session and auth shapes are additionally verified against a live `pi` 0.78.0 install (2026-06-04). The code binding this surface is the adapter directory [`pi/`](../../../crates/rimz/src/agents/pi/mod.rs): the embedded [`extension.ts`](../../../crates/rimz/src/agents/pi/extension.ts) forwards the lifecycle events, including the `session_before_compact`/`session_compact` bracket; gates `tool_call` on the blocking bridge; and the read-only spending parser [`pi/spend.rs`](../../../crates/rimz/src/agents/pi/spend.rs) walks the session tree.

Coverage is **depth on what the adapter wires, breadth as an index**: the events, session fields, and decision returns [`src/agents/pi/`](../../../crates/rimz/src/agents/pi/mod.rs) parses or emits are documented in full; the rest of the catalog is listed so a contributor wiring a new path knows it exists. [Mapping feasibility](#mapping-feasibility) closes the doc with what remains unwired; the landed verdict is the [hooks.md appendix](../../internals/hooks.md#appendix--pi).

## Upstream sources

Re-fetch these pages to refresh this mirror. Each `pi.dev/docs/latest/<page>` page renders `packages/coding-agent/docs/<page>.md` from the GitHub repo — the markdown is the higher-fidelity fetch. Docs publish only as `latest` (no version-pinned tree), so pair a refresh with `pi --version` and the repo commit; exact TypeScript types ship in `node_modules/@earendil-works/pi-coding-agent/dist/`.

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
| Markdown doc sources + extension examples | <https://github.com/earendil-works/pi> — `packages/coding-agent/docs/`, `packages/coding-agent/examples/extensions/` |
| Extension type definitions | npm [`@earendil-works/pi-coding-agent`](https://www.npmjs.com/package/@earendil-works/pi-coding-agent) |

## Integration surface — in-process extensions

Pi's integration surface is **TypeScript extensions loaded in-process** (via jiti, no compile step) — it ships no out-of-process hook protocol, no statusline, and no app-server. A Rimz adapter is therefore a Rimz-authored extension file that subscribes to lifecycle events and shells out to the `rimz` CLI, holding pi's turn open from inside a handler when a decision must block.

> **Divergence — the decision channel inverts.** Claude and Codex run Rimz as a child and read its stdout as the decision. Pi runs Rimz's *extension* in-process; the extension runs `rimz` as *its* child, reads the answer from the child's stdout, and applies it through the handler's return value. Hook-stdout discipline becomes child-stdout discipline, and the sync-install invariant has no on-disk shape to enforce — blocking is awaiting inside the handler.

Discovery, in load order:

| Location | Scope |
| --- | --- |
| `~/.pi/agent/extensions/*.ts` and `*/index.ts` | global |
| `.pi/extensions/*.ts` and `*/index.ts` | project-local |
| `settings.json` — `extensions: [paths]`, `packages: ["npm:…", "git:…"]` | configured |
| `pi -e <path>` / `--extension <source>` | per-invocation |

Install for Rimz means **one Rimz-owned file** written to `~/.pi/agent/extensions/` — auto-discovered, hot-reloaded by `/reload`, removed by deleting the file, idempotent by path. The file executes arbitrary code with the user's permissions (upstream states this explicitly), so it belongs in the executable-surface trust hash like every hook config. `--no-extensions`, `-p` (print), and `--mode json` run pi without UI-capable extensions — see [Mapping feasibility](#mapping-feasibility).

An extension default-exports a (sync or async) factory receiving `ExtensionAPI` — `pi.on(event, handler)`, `pi.registerCommand`, `pi.registerTool`, `pi.exec`, `pi.appendEntry` (persist extension state in the session), `pi.setSessionName`, `pi.getThinkingLevel` (the Rimz wire's `effort`), `pi.events` (inter-extension bus). Every handler receives `ExtensionContext`:

| Field | Carries |
| --- | --- |
| `ctx.sessionManager` | read access to session state — `getSessionId()`, `getSessionFile()`, `getCwd()`, `getEntries()`, `getBranch()`, `getLeafId()` |
| `ctx.getContextUsage()` | live context-token usage for the active model |
| `ctx.ui` | dialogs (`confirm`, `select`, `input`, `editor` — each with an optional `timeout` auto-dismiss) and fire-and-forget (`notify`, `setStatus`, `setWidget`, `setTitle`) |
| `ctx.mode` / `ctx.hasUI` | `"tui" \| "rpc" \| "json" \| "print"`; `hasUI` is true in TUI and RPC only — gate every dialog on it |
| `ctx.cwd`, `ctx.signal`, `ctx.isIdle()`, `ctx.abort()`, `ctx.shutdown()` | working directory, the active turn's abort signal, control-flow helpers |
| `ctx.modelRegistry` / `ctx.model` | model catalog — each model carries `provider`, `id`, `contextWindow`, `maxTokens`, and per-token cost rates |

Node built-ins (`node:fs`, `node:child_process`, …) and npm dependencies (via an adjacent `package.json`) are importable.

## Extension events

The lifecycle, condensed (the full diagram is in the [extensions doc](https://pi.dev/docs/latest/extensions)):

```text
launch         ─► session_start { reason: "startup" }
prompt         ─► input ─► before_agent_start ─► agent_start
                   ┌─ turn (one LLM call; repeats while tools run) ─┐
                   │ turn_start ─► context ─► before_provider_request│
                   │   tool_execution_start ─► tool_call (can block) │
                   │   ─► tool_execution_update* ─► tool_result      │
                   │   ─► tool_execution_end                         │
                   │ turn_end { message, toolResults }               │
                   └─────────────────────────────────────────────────┘
               ─► agent_end { messages }
/compact, auto ─► session_before_compact ─► session_compact
/new, /resume  ─► session_before_switch ─► session_shutdown ─► session_start { reason }
/fork, /clone  ─► session_before_fork ─► session_shutdown ─► session_start { reason: "fork" }
exit (Ctrl+C, Ctrl+D, SIGHUP, SIGTERM) ─► session_shutdown { reason: "quit" }
```

Note pi's vocabulary: a pi **turn** is one LLM call, and `agent_start`/`agent_end` bracket one user prompt — pi's `agent_*` pair is what Rimz calls a turn.

### Events an adapter would wire

| Event | Fires | Payload fields | Handler return |
| --- | --- | --- | --- |
| `session_start` | launch, `/new`, `/resume`, fork/clone, `/reload` | `reason` (`startup`\|`reload`\|`new`\|`resume`\|`fork`), `previousSessionFile?` | — |
| `before_agent_start` | prompt submitted, before the agent loop | `prompt`, `images?`, `systemPrompt`, `systemPromptOptions` | may inject a message / replace the system prompt |
| `agent_start` / `agent_end` | once per user prompt | `agent_end.messages` — the prompt's messages; the last assistant message carries `stopReason` and `errorMessage?` | — |
| `turn_end` | per LLM call inside the loop | `turnIndex`, `message` (assistant, with `usage`), `toolResults` | — |
| `tool_call` | before a tool executes — **can block** | `toolName`, `toolCallId`, `input` (mutable) | `{ block: true, reason?: string }` blocks; mutations to `input` patch the call |
| `tool_execution_end` | after a tool finishes | `toolCallId`, `toolName`, `result`, `isError` | — |
| `session_before_compact` / `session_compact` | compaction, manual or auto | `preparation` / `compactionEntry`, `fromExtension` | `before` may cancel or supply a custom summary |
| `session_shutdown` | quit (incl. Ctrl+C/SIGHUP/SIGTERM), `/new`, `/resume`, fork, `/reload` | `reason` (`quit`\|`reload`\|`new`\|`resume`\|`fork`), `targetSessionFile?` | — |
| `model_select` | `/model`, `Ctrl+P` cycling, session restore | `model`, `previousModel?`, `source` (`set`\|`cycle`\|`restore`) — `model` carries `contextWindow`/`maxTokens`/cost rates | — |
| `thinking_level_select` | thinking level change | `level` (`off`\|`minimal`\|`low`\|`medium`\|`high`\|`xhigh`), `previousLevel` | — |

Session identity rides `ctx.sessionManager` rather than event payloads: `getSessionId()`, `getSessionFile()`, and `getCwd()` are valid from the first `session_start` — at launch, with no lazy-registration window.

### Event index (the rest)

`resources_discover`, `session_before_switch`, `session_before_fork`, `session_before_tree` / `session_tree`, `message_start` / `message_update` / `message_end` (streaming; `message_end` may replace the finalized message), `context` (mutate the message list before each LLM call), `before_provider_request` (inspect/replace the provider payload), `after_provider_response` (`status`, normalized `headers` — the only place provider rate-limit response headers surface), `tool_execution_start` / `tool_execution_update`, `tool_result` (patch `content`/`details`/`isError`, middleware-chained), `input` (`text`, `images?`, `source`: `interactive`\|`rpc`\|`extension`, `streamingBehavior`; can transform or handle), `user_bash`.

### Blocking, dialogs, and error handling

`tool_call` is the one blocking return — `{ block: true, reason }` — and pi imposes **no deadline** on a handler: an awaiting handler holds the turn open indefinitely, so any cap is the extension's own (e.g. `ctx.ui` dialogs take a `timeout` option that auto-dismisses with a countdown — `confirm` resolves `false`, `select`/`input` resolve `undefined`). Tool preflight is sequential even in parallel tool mode, so a blocked `tool_call` also delays the assistant message's sibling tools.

Extension error handling is fail-safe in the right direction: a handler exception is logged and the agent continues, **except** in `tool_call`, where an exception blocks the tool. A custom tool's `execute` signals failure by throwing (sets `isError: true`); its return value never does.

## Session JSONL

Pi writes one session file per conversation:

```text
~/.pi/agent/sessions/--<cwd-with-/-as-->--/<timestamp>_<uuid>.jsonl
e.g.   sessions/--home-marvin-workspace-project-rimz-rimz--/2026-06-04T06-45-56-308Z_019e9161-a5d0-791d-879e-39679acd4ded.jsonl
```

The directory key is the working directory with `/` replaced by `-`; the filename stem is `<ISO timestamp, : and . as ->_<session uuid>`, so the session id is everything after the first `_`. Overrides: `--session-dir`, `PI_CODING_AGENT_SESSION_DIR`, settings `sessionDir`; `--no-session` skips persistence entirely. [`pi/spend.rs`](../../../crates/rimz/src/agents/pi/spend.rs) walks this tree fleet-wide for spending (its `PI_AGENT_DIR` env is a Rimz test override, not a pi variable).

The first line is the header; every later line is a tree entry (`id` is 8-char hex, `parentId` links it, `timestamp` is ISO):

```jsonc
{"type":"session","version":3,"id":"<session uuid>","timestamp":"2026-06-04T06:45:56.308Z","cwd":"/home/marvin/…","parentSession":"<path, fork/clone only>"}
```

Sessions are versioned (v1 linear → v2 tree → v3 renamed `hookMessage` → `custom`) and auto-migrate on load; parse around the `version` field.

| Entry `type` | Carries |
| --- | --- |
| `message` | a `message` object — see below |
| `model_change` | `provider`, `modelId` — the user switched models mid-session |
| `thinking_level_change` | `thinkingLevel` |
| `compaction` | `summary`, `firstKeptEntryId`, `tokensBefore`, `details?`, `fromHook?` |
| `branch_summary` | `fromId`, `summary` — an abandoned branch's summary |
| `custom` / `custom_message` | extension state (`customType`, `data?`) / extension-injected context |
| `label` | `targetId`, `label` — `/tree` bookmarks |
| `session_info` | `name` — the `/name` display name |

The `message` roles are `user`, `assistant`, `toolResult`, `bashExecution` (`!` commands), `custom`, `branchSummary`, `compactionSummary`; message-level `timestamp` is Unix **ms**, unlike the ISO entry envelope. The assistant shape (live-verified; `responseId` is present on the wire though undocumented — parse tolerantly):

```jsonc
{"type":"message","id":"a1b2c3d4","parentId":"…","timestamp":"2026-06-04T06:46:14.308Z","message":{
  "role": "assistant",
  "provider": "openai-codex", "model": "gpt-5.5", "api": "…", "responseId": "…",
  "content": [ {"type":"text","text":"…"} ],            // also "thinking", "toolCall" blocks
  "stopReason": "stop",                                  // stop | length | toolUse | error | aborted
  "errorMessage": "…",                                   // present on error
  "usage": { "input": 3435, "output": 6, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 3441,
             "cost": { "input": 0.017175, "output": 0.00018, "cacheRead": 0, "cacheWrite": 0, "total": 0.017355 } }
}}
```

Two properties matter for any tail read:

- **Dollars are logged directly.** `usage.cost.total` is the priced cost per assistant message — no pricing-table multiplication ([`pi/spend.rs`](../../../crates/rimz/src/agents/pi/spend.rs) reads it verbatim). The token split mirrors Anthropic's: context tokens are `input + cacheRead + cacheWrite`; the transcript carries **no context window**, so a gauge divisor resolves from the model registry (`contextWindow` per model) or a table, the way Claude's payload model resolves its divisor.
- **The file is a tree, not a log.** `/tree` and `/fork` move the leaf to an earlier entry in place, so file order is append order, not branch order — the newest record by file position can sit on an abandoned branch right after a rewind. `buildSessionContext()` (the upstream context builder) walks leaf→root; a bounded tail read is an approximation that self-corrects on the next turn.

## Headless modes (index)

Recorded for breadth; a Rimz adapter targets the interactive TUI in a pane.

- **`--mode json`** — every session event as JSON lines on stdout. The union is the extension `AgentEvent` set plus `queue_update`, `compaction_start` / `compaction_end` (`reason: manual|threshold|overflow`), and `auto_retry_start` / `auto_retry_end`. Interactive extension `session_compact` carries `compactionEntry` and `fromExtension` and omits this reason.
- **`--mode rpc`** — bidirectional JSONL over stdio for embedding: commands (`prompt`, `steer`, `follow_up`, `abort`, `get_state`, `get_messages`, `get_session_stats`, `set_model`, `new_session`, `switch_session`, `fork`, `compact`, `bash`, …) correlate by `id` + `success`; the same events stream interleaved; extension `ctx.ui` dialogs surface as an RPC sub-protocol.
- **SDK** — `createAgentSession({ customTools, … })` embeds the agent loop in another Node program: <https://pi.dev/docs/latest/sdk>.

## Auth file

`~/.pi/agent/auth.json` (created `0600`) maps provider → credential; `/login` / `/logout` manage OAuth entries (live-verified shapes):

```jsonc
{
  "anthropic":    { "type": "oauth", "access": "…", "refresh": "…", "expires": <epoch ms> },
  "openai-codex": { "type": "oauth", "access": "…", "refresh": "…", "expires": <epoch ms>, "accountId": "…" },
  "openai":       { "type": "api_key", "key": "sk-…" }   // key also takes "$ENV_VAR" or "!command"
}
```

OAuth subscription logins: ChatGPT Plus/Pro (`openai-codex`), Claude Pro/Max (`anthropic` — upstream notes third-party usage bills per token as extra usage, outside plan limits), GitHub Copilot. API keys resolve in order: `--api-key` flag → `auth.json` → provider env var (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, …) → `models.json` custom-provider keys.

**The balance gap.** Pi exposes no rate-limit windows and no plan tier — no statusline, no app-server, no probe-able quota surface. `auth.json` supports exactly one account fact: credential type (`oauth` → metered subscription, `api_key` → unmetered). The only place window data could ever surface is per-response HTTP headers via `after_provider_response`, which is provider-specific and uncontracted. See [account.md → Per-provider mapping](../../internals/account.md#per-provider-mapping) for what this means for the dashboard.

## CLI and environment surface

The flags and variables an adapter (and the resume-on-rebirth planner) cares about:

| Surface | Meaning |
| --- | --- |
| `pi --version` | version (`0.78.1` verified) |
| `pi -c` / `pi -r` | continue the most recent session / browse and pick |
| `pi --session <path\|id>` | resume a specific session; accepts a partial UUID — the resume-on-rebirth seed |
| `pi --fork <path\|id>`, `--no-session`, `--name <n>` | fork into a new file, ephemeral mode, display name |
| `pi -e <source>`, `--no-extensions` | load an extension / disable discovery |
| `-p`, `--mode json`, `--mode rpc` | headless modes (above) |
| `pi install npm:…\|git:…` / `pi remove` / `pi list` | pi package management — the distribution channel an npm-published Rimz extension would use |
| `PI_CODING_AGENT_DIR` | config root override (default `~/.pi/agent`) |
| `PI_CODING_AGENT_SESSION_DIR` | session-dir override (below `--session-dir`) |
| `PI_PACKAGE_DIR`, `PI_OFFLINE`, `PI_SKIP_VERSION_CHECK`, `PI_TELEMETRY` | package root, startup-network and telemetry switches |

## Mapping feasibility

The landed verdict — the native-event → signal table, the blocking `tool_call` gate and its decision shape, the turn-death and identity properties, and the install shape — is [hooks.md → Appendix Pi](../../internals/hooks.md#appendix--pi). Upstream's own scope statement ([usage.md](https://pi.dev/docs/latest/usage)) frames the gaps: pi intentionally ships **no built-in MCP, sub-agents, permission popups, plan mode, to-dos, or background bash** — those are extension territory. The context gauge and model/effort enrichment now ride every hook envelope (`ctx.getContextUsage()`, `ctx.model.id`, the thinking level), so no transcript-tail gauge is needed.

The account probe is wired: [`pi/account.rs`](../../../crates/rimz/src/agents/pi/account.rs) reads `auth.json` (oauth → metered subscription, api_key → unmetered), labels the sub the fleet uses — the freshest session's `message.provider` picks among several credentials, else the first OAuth entry — and the separate adapter version probe attaches `pi --version`; mapping summary in [account.md](../../internals/account.md#per-provider-mapping).

What remains unwired, for the adapter's next increments:

- **Model-change marker.** `model_select` / `thinking_level_select` could stamp a mid-session model switch the moment it happens; today the change rides the next event's envelope as carry-forward.
