# Pi protocol reference

> The mapping onto Rimz's internal types lands beside this doc when the adapter does: [hooks.md](../hooks.md) owns the lifecycle/feed mapping recipe ([Adding an agent](../hooks.md#adding-an-agent)), [transcript.md](../transcript.md) the context read-path, [account.md](../account.md) the account/balance mapping.

This is the single home for the **Pi upstream protocol surface** a Rimz adapter binds to — the in-process extension API (events, payloads, blocking returns), the session JSONL, the headless RPC/JSON modes, the auth file, and the CLI/env surface. It is a hand-maintained mirror of the pi.dev docs, pinned to the source URLs below; the session and auth shapes are additionally verified against a live `pi` 0.78.0 install (2026-06-04). The only code reading this surface today is the adapter's read-only spending parser [`pi/spend.rs`](../../../crates/rimz/src/agents/pi/spend.rs); the hook surface is roadmap work ([roadmap.md](../../contributing/roadmap.md)).

Coverage is **depth on what an adapter would wire, breadth as an index**: the events, session fields, and decision returns a future `src/agents/pi/` adapter parses or emits are documented in full; the rest of the catalog is listed so a contributor wiring a new path knows it exists. [Mapping feasibility](#mapping-feasibility) closes the doc with the Rimz-side analysis — what maps cleanly and what Pi cannot support — and migrates into a [hooks.md](../hooks.md) appendix when the adapter lands.

## Upstream sources

Re-fetch these pages to refresh this mirror. Each `pi.dev/docs/latest/<page>` page renders `packages/coding-agent/docs/<page>.md` from the GitHub repo — the markdown is the higher-fidelity fetch. Docs publish only as `latest` (no version-pinned tree), so pair a refresh with `pi -v` and the repo commit; exact TypeScript types ship in `node_modules/@earendil-works/pi-coding-agent/dist/`.

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

An extension default-exports a (sync or async) factory receiving `ExtensionAPI` — `pi.on(event, handler)`, `pi.registerCommand`, `pi.registerTool`, `pi.exec`, `pi.appendEntry` (persist extension state in the session), `pi.setSessionName`, `pi.events` (inter-extension bus). Every handler receives `ExtensionContext`:

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

- **`--mode json`** — every session event as JSON lines on stdout. The union is the extension `AgentEvent` set plus `queue_update`, `compaction_start` / `compaction_end` (`reason: manual|threshold|overflow`), and `auto_retry_start` / `auto_retry_end`.
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

**The balance gap.** Pi exposes no rate-limit windows and no plan tier — no statusline, no app-server, no probe-able quota surface. `auth.json` supports exactly one account fact: credential type (`oauth` → metered subscription, `api_key` → unmetered). The only place window data could ever surface is per-response HTTP headers via `after_provider_response`, which is provider-specific and uncontracted. See [account.md → Per-provider mapping](../account.md#per-provider-mapping) for what this means for the dashboard.

## CLI and environment surface

The flags and variables an adapter (and the resume-on-rebirth planner) cares about:

| Surface | Meaning |
| --- | --- |
| `pi -v` | version (`0.78.0` verified) |
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

Rimz-side analysis, recorded here until the adapter lands and it becomes a [hooks.md](../hooks.md) appendix. Upstream's own scope statement ([usage.md](https://pi.dev/docs/latest/usage)) frames it: pi intentionally ships **no built-in MCP, sub-agents, permission popups, plan mode, to-dos, or background bash** — those are extension territory.

### What maps cleanly

- **Lifecycle.** `session_start` → `Registered`, `before_agent_start`/`agent_start` → `TurnStarted` (with the prompt), `agent_end` → `TurnEnded` (the error bit from the last assistant message's `stopReason`), `tool_execution_end` → `ToolUsed { mutates, edits }` (`edit`/`write` edit files; `bash` mutates) and the activity heartbeat, `session_before_compact` → `Compacting` (a leading signal, like Claude's `PreCompact`).
- **Session end.** `session_shutdown` fires on quit *including* Ctrl+C/SIGHUP/SIGTERM and on every session replacement — a true `ends_session` signal Codex lacks, with `/new` and `/resume` handled as shutdown + start rather than Codex's silent `/clear`.
- **Turn death, in band.** A failed or aborted LLM call still ends with an assistant message carrying `stopReason: "error" | "aborted"` plus `errorMessage` — an explicit death certificate at the turn boundary, with no transcript forensics needed.
- **Identity.** The extension runs in-process in the pane: pane env and pi's own pid are directly readable, the session id exists from launch (no lazy-registration window), and there is no daemon or remote mode — every pi session is standalone and stamped.
- **Context gauge.** `ctx.getContextUsage()` is a first-class in-process reading, and every assistant message carries the full token split; the transcript-tail floor works with a registry-resolved window divisor.
- **Enrichment.** Direct `costUSD` (already powering [spending](../transcript.md#cost-history)), `model` + `contextWindow` on every `model_select`, `thinkingLevel` ↔ the `effort` carry-forward.
- **Resume-on-rebirth.** `pi --session <session_id>` restores a rollup-recorded session by id.
- **Install.** One Rimz-owned extension file under `~/.pi/agent/extensions/`, idempotent by path, hot-reloadable via `/reload`, visible in the trust hash and the install diff.

### What Pi cannot support

1. **No native blocking-feed channel.** Pi never asks permission, never plan-approves, never poses questions — there is nothing for Rimz to observe and route, so `waiting` and the three operating paths never engage natively. An extension *can* build a gate (`tool_call` blocks; `ctx.ui.confirm` renders in pi's own pane), but that has Rimz *inventing* the prompt rather than routing it — viable only as an explicit opt-in, never the default install. A gate also sets its own deadline (pi imposes none, so `hook_cap` is Rimz-chosen) and delays sibling tools while blocked.
2. **No subagents, todos, background tasks, or MCP.** `SubagentStarted/Stopped`, todo progress, and `parked_on_background` (`⋯ bg`) never fire; pi rows simply lack those enrichments. All are optional in the agent model.
3. **No account balance surface.** Rate-limit windows and plan tier have no source (the gap above), so the provider dashboard can show logged-in plus metered/unmetered, and the `rate_limited` derived status never triggers for pi.
4. **One agent, many provider accounts.** A single pi session switches provider mid-session (`model_change` entries; parallel oauth logins in one `auth.json`), so pi spend and auth cannot attribute to one upstream provider account — the dashboard models *pi* as the provider kind, and its panel aggregates whatever accounts pi used.
5. **Tail reads can momentarily lag a rewind.** In-file branching means the newest record by file position may sit on an abandoned branch right after `/tree` or `/fork`; a bounded-tail gauge self-corrects on the next turn. Display-only, acceptable.
6. **Integration-blind modes.** `--no-extensions` runs with no events at all, and `-p` / `--mode json` run extensions with `ctx.hasUI == false` — no dialogs, no asks. Same posture as an agent run before `rimz hooks install`: invisible, never silently broken.
