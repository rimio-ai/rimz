# Agent transcripts and context

> See [DESIGN.md → Commitments](../../DESIGN.md#commitments) for the no-transcript-correctness rule this doc operationalizes, and [hooks.md](./hooks.md) for the agent boundary it sits beside.

Every coding agent writes a session transcript and exposes a richer out-of-band surface. Rimz reads both to paint a row's context-window gauge, token count, model, cost, and rate-limit windows. This doc owns that read-path: where each provider's data lives, how Rimz parses it, and the per-provider mapping onto Rimz's internal types.

It is the **single home for context/usage enrichment mapping**: how each provider's transcript and rich-context transport fold onto Rimz's internal types ([`AgentContext`](../../crates/rimz/src/agents/context.rs) and the `context_pct` / `total_tokens` / `model` fields of [`AgentLifecycleObservation`](../../crates/rimz/src/agents/mod.rs)). The raw upstream shapes this mapping reads — the statusline JSON schema, the rollout event layout, the app-server methods — live in the per-provider reference: [adapter/claude.md](./adapter/claude.md) and [adapter/codex.md](./adapter/codex.md). The seam is two-sided: this doc *produces* enrichment from a provider's surfaces; [agent.md](./agent.md#rich-context-agentcontext) *stores* it on the rollup and folds it onto the row. The rich-context blob also carries account and balance facts (plan, metered, the rate-limit windows); this doc owns the transport that delivers them, and [account.md](./account.md) owns what they mean and how they aggregate.

Enrichment is **never correctness**. A missing file, a torn line, an absent agent — each degrades to an omitted field, never a failed hook or a wrong decision. The decision/lifecycle half of the provider mapping (native event → status, decision JSON, install) lives in [hooks.md](./hooks.md); this is its quiet, display-only twin.

## Two sources

A session's context data has two origins. Both flow through one adapter — the [`AgentIntegration`](../../crates/rimz/src/agents/mod.rs) — and normalize onto the same internal fields, so a new provider implements one or both and the rest of Rimz is unchanged.

**The transcript tail** is the universal floor. Every provider writes a JSONL session log, so every agent gets a context gauge from it. Capture is low-frequency: it runs inside `observe_lifecycle` on the turn-boundary events Rimz already fires (session start, prompt submit, turn end), and only once a payload supplies a session id. It reads a bounded tail, takes the most recent usage record, and never blocks the hook.

**The rich-context transport** is the high-frequency upgrade, where a provider offers one. It carries everything the transcript cannot — cost, rate-limit windows, account plan, PR info, model display name — and refreshes far more often than turn boundaries. Each provider's transport differs; both normalize through `observe_context` into one [`AgentContext`](../../crates/rimz/src/agents/context.rs).

|                     | transcript tail                                  | rich-context transport                         |
| ------------------- | ------------------------------------------------ | ---------------------------------------------- |
| Claude Code         | hook payload `transcript_path`                   | statusline pipe (`rimz statusline feed`)       |
| Codex               | `~/.codex/sessions/…/rollout-*.jsonl` (by id)    | `codex app-server` JSON-RPC (read-only)        |
| frequency           | turn boundaries, after a session id appears      | every render / poll                            |
| produces            | `context_pct`, `total_tokens`, `model`           | the full `AgentContext` (gauges, cost, limits) |
| target              | observation gauge fields ([agent.md](./agent.md#the-rollup)) | `AgentContext` ([context.rs](../../crates/rimz/src/agents/context.rs)) |

## Reading rules

The tail reader is provider-agnostic ([`read_transcript_tail`](../../crates/rimz/src/agents/mod.rs)) and every adapter parses on top of it under the same rules:

- **Bounded.** Read at most the trailing 64 KB, so a multi-megabyte log never stalls a hook.
- **Newest-first.** Scan lines in reverse and take the most recent usage record; a truncated leading line from the tail seek simply fails to parse and is skipped.
- **Stop when found.** Bail as soon as the needed records are in hand (Codex tracks the latest `token_count` and `model` separately and stops once both are filled).
- **Lossy and forgiving.** Decode as lossy UTF-8; any IO or parse failure yields empty fields.
- **Zero vs unknown.** A transcript that opens cleanly but carries no usage yet is a *fresh* session — report an explicit `0%` so the bar draws empty rather than vanishing. A transcript that cannot be read stays unknown (`None`): "the agent did not report it," never a false zero.

## Adding a provider

A new agent earns its context gauge by implementing the transcript half, and its rich row by implementing the transport half — either alone is valid. The work mirrors [hooks.md → Adding an agent](./hooks.md#adding-an-agent):

1. **Locate the transcript** from whatever the hook payload offers (a path, or a session id plus a discovery walk).
2. **Map the usage record** onto raw context tokens (or a percentage) plus the cumulative total and the model, normalizing to the observation gauge fields.
3. **Map the transport**, if any, onto `AgentContext` through `observe_context` — every field `Option`, tolerantly parsed, so a sparse or evolved payload always parses.
4. **Stay best-effort** throughout: a failure is an omitted field, never an error.

Per [testing.md](../contributing/testing.md), golden the mapping from a fixture tail and a fixture transport payload, including the fresh-session zero and the unreadable-unknown cases.

---

## Appendix — Claude Code

**Transcript.** Claude names the active session log in every hook payload's `transcript_path`. Each assistant line carries a `message.usage` block; the newest one wins:

| Transcript field                                                          | Internal                                  |
| ------------------------------------------------------------------------- | ----------------------------------------- |
| `usage.input_tokens` + `usage.cache_read_input_tokens` + `usage.cache_creation_input_tokens` | context tokens (the gauge numerator) |
| context tokens + `usage.output_tokens`                                    | `total_tokens`                            |
| `message.model`                                                           | `model` (bare id — no capability marker)  |

Claude writes only the bare model id, so the **window divisor is resolved by the caller, not the transcript**: [`context_window_for`](../../crates/rimz/src/agents/claude.rs) reads the `[1m]` marker off the *hook payload's* `model` (1,000,000 tokens) and otherwise assumes the 200,000 standard window, then scales the context tokens to `context_pct`. The marker rides only the payload, never the transcript, so resolving the payload model first is what keeps the 1M gauge correct.

**Rich context.** Claude `exec`s its configured `statusLine` command on every render and pipes a JSON blob to stdin (the full schema is in [adapter/claude.md → Statusline JSON](./adapter/claude.md#statusline-json)). Install points `statusLine` at `rimz statusline feed --source claude`; [`observe_context`](../../crates/rimz/src/agents/statusline.rs) parses that blob into `AgentContext`. When the user already has a `statusLine`, install **wraps** it rather than replacing it: it captures the JSON, passes it unchanged to the original command, and forwards that command's stdout and exit code, so rendering is visually identical. The original is stored verbatim under `_rimz_wrapped` and restored on uninstall. The wrap is a visible security surface — the consent gate summarizes it and the install diff shows it in full — and its child's stdio is fully piped, never inherited. Field-exact shapes are the inline goldens in [`statusline.rs`](../../crates/rimz/src/agents/statusline.rs).

## Appendix — Codex

**Transcript.** Codex writes one rollout file per session at `~/.codex/sessions/YYYY/MM/DD/rollout-*-<session_id>.jsonl`. Given the payload's `session_id`, [`find_session_transcript`](../../crates/rimz/src/agents/codex.rs) descends the date tree newest-first, bounded by a day-directory budget so a large archive never stalls the walk; `RIMZ_CODEX_SESSIONS` overrides the root for tests. Two rollout event types feed the gauge:

| Rollout event   | Field                                          | Internal                                  |
| --------------- | ---------------------------------------------- | ----------------------------------------- |
| `token_count`   | `payload.info.last_token_usage.input_tokens` / `payload.info.model_context_window` | `context_pct` (input × 100 ÷ window, clamped) |
| `token_count`   | `payload.info.last_token_usage.total_tokens`   | `total_tokens`                            |
| `turn_context`  | `payload.model`                                | `model` (display name)                    |

Unlike Claude — which stores raw tokens and derives the window from the payload model — Codex stores a **precomputed `context_pct`**, because the rollout carries the window (`model_context_window`) directly.

**Rich context.** Codex has no statusline, so its `AgentContext` is read out of band from the official `codex app-server` (JSON-RPC 2.0 over stdio) by [`codex_app_server.rs`](../../crates/rimz/src/agents/codex_app_server.rs). The client speaks only **read-only, non-interfering** methods — the handshake, the rate-limit read, and the model list (the methods and their response schemas are in [adapter/codex.md → App-server API](./adapter/codex.md#app-server-api)). It never calls `thread/resume`, `turn/start`, or any write, which would rejoin and own the user's live thread.

The trigger is never inline: a turn-boundary hook spawns `rimz codex refresh-context` detached with null stdio, so the hook returns before the round-trip and adds no latency. That helper writes the same per-session `AgentContext` sidecar Claude's statusline produces. The producer also keeps the account-scoped balance windows fresh between turns on a bounded cadence — the rate-limit refresh logic (and the `rimz codex refresh-rate-limits` idle path) lives in [account.md → Refresh cadences](./account.md#refresh-cadences). `RIMZ_CODEX_BIN` overrides the binary for tests. Connection preference is warmest-first, all best-effort with a cold-spawn floor so enrichment never depends on any one of them:

1. **The per-session broker** ([`codex_broker.rs`](../../crates/rimz/src/agents/codex_broker.rs), run as `rimz codex app-server serve` in the `rimzd` daemon tab) holds one warm, already-handshaked `codex app-server` and serves it over a unix socket, so each datapoint skips the cold handshake.
2. **The per-user remote-control daemon**, re-used via `codex app-server proxy --sock <path>` (overridable by `RIMZ_CODEX_APP_SERVER_SOCK`).
3. **A fresh cold-spawned `codex app-server`** — the always-present fallback, so headless / no-mux still enriches.

The one datapoint the app-server does **not** expose read-only is token / context-window usage: it rides only the live `thread/tokenUsage/updated` notification behind a subscribing `thread/resume`. So `AgentContext.tokens` stays `None` and Codex's context gauge is sourced from the rollout transcript above.
