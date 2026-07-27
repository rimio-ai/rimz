# Qwen Code adapter

> Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer every adapter implements. Accounts, balances, and spend are in [providers.md](./providers.md); the raw upstream protocol is in [qwen-reference.md](../../externals/agent-adapter/qwen-reference.md).

Qwen Code is a standalone, eagerly registered adapter. RimZ installs native hooks in `~/.qwen/settings.json` (or `$QWEN_HOME/settings.json`), wraps a command-mode `ui.statusLine`, and reads session JSONL from the Qwen runtime tree.

## Hooks and lifecycle

| Native event | RimZ signal |
| --- | --- |
| `SessionStart` | register; `compact` closes compaction, while `startup` and `clear` mark fresh lineage |
| `UserPromptSubmit` | turn started; a present-but-blank prompt is an internal continuation and stays inside the current turn |
| gate-tool `PreToolUse` | plan/question wait keyed by `tool_use_id`; matcher-scoped to `ask_user_question` and `exit_plan_mode` |
| `PostToolUse` / `PostToolUseFailure` | completed tool activity |
| `PermissionRequest` | permission wait; the plan/question gate tools classify as plan/question waits |
| `Stop` / `StopFailure` | clean or failed turn end; pending `background_tasks` or `crons` keep the parent parked |
| `SubagentStart` / `SubagentStop` | child bracket; both ends best-effort add model and description from the child metadata sidecar |
| `PreCompact` / `PostCompact` | compaction bracket |
| `SessionEnd` | ended |

**Two blocking events, one wait.** Hook stdout stays empty on the neutral path. `PermissionRequest` classifies the native approval stage. Auto-allowed question and plan tools can skip that event because executing the tool opens the dialog, so RimZ also installs a synchronous `PreToolUse` hook scoped by the `ask_user_question|exit_plan_mode` matcher. That execution event opens a typed wait keyed by `tool_use_id`, and the matching `PostToolUse` or `PostToolUseFailure` closes it without opening a new prompt boundary. Install refuses an async RimZ-managed entry for either blocking event, reclaims owned entries by the `rimz hooks feed --source qwen` command marker, and leaves unrelated hooks intact.

**Subagents.** The child id is `agent_id`, the parent is the hook's root `session_id`, and `agent_type` labels the child. At both `SubagentStart` and `SubagentStop`, RimZ best-effort reads `<project>/subagents/<parent-session-id>/agent-<agent_id>.meta.json`; `persistedCliFlags.model` supplies the model and `description` supplies the child task description. Qwen Code 0.21 and newer write that sidecar before the start hook, so running children receive the enrichment immediately; older releases may not expose it until stop. This wires the native child bracket and renders the tree.

## Launch and resume

RimZ preserves Qwen's configured model by default because `security.auth.selectedType` can route to provider-specific catalogs; an `agents.toml` model preset adds `--model` explicitly. Qwen's system-prompt flag takes text rather than a file path, so the shared exec materializer reads `system-prompt-file`, composes any ordered `append-system-prompt-files`, and passes the result through `--system-prompt`. This gives Qwen the same replacement semantics as path-flag adapters without putting provider-specific text in the profile model. Manual compaction sends `/compress` (`/summarize` is Qwen's alias).

## Context and transcript

At `SessionStart` and `Stop`, the hook path reads the complete transcript and follows the latest root record's `uuid`/`parentUuid` ancestry. The newest active root `assistant` with `usageMetadata` supplies its model, context window, total, cache-read prompt, fresh prompt, and normalized output; at `Stop`, RimZ accepts that summary only when its `promptTokenCount` matches the hook's direct `input_tokens`. The latest active `system/custom_title` record supplies the durable card description from `systemPayload.customTitle`. Until that title exists, the shared rollup keeps the session's first usable prompt as its stable unnamed-session label. Numeric strings are accepted and malformed optional usage fields stay absent without discarding the record. A readable usage-free transcript means fresh zero, while an unreadable path stays unknown. Tool hooks skip this complete-file enrichment.

**Rich context.** Command-mode `ui.statusLine` is wrapped so `rimz statusline feed --source qwen` receives Qwen's rich JSON. It owns the provider-selected context window, percentage, and scalar `current_usage` occupancy, plus the provider-prefix-stripped model display name, version, Vim mode, file-line totals, and complete locally estimated session cost. The card renders the scalar as its live `▤` total and a flat meter until an exact transcript call split has the same filled-input total; a matching split adds the cache, input, and output markers and meter segments, while a newer mismatch immediately returns to the scalar-only view. An explicit zero clears the live occupancy after compaction. Cumulative `metrics.models` counters stay private to the live cost estimate and do not become card token categories. A preset statusline has no command transport, so install leaves it untouched and context falls back to lifecycle enrichment.

**Conversation replay** uses the same UUID-parent fold as hook-boundary usage. It indexes identity-bearing records last-wins, selects the latest record without `agentId` or `isSidechain: true`, and walks its ancestry with missing-parent and cycle guards; unknown and system records remain valid links but stay out of visible replay. A transcript with no usable root UUID graph preserves legacy physical order.

Each visible `user`/`assistant` record carries the Google `Content` shape, so replay joins its non-thought `text` parts and drops thought, `functionCall`, and `functionResponse` parts without prose. Incremental `rimz message --wait` and `-p --stream` reads use a separate physical append parser: an appended assistant can point to a parent before the byte cursor, so streaming validates and emits each new visible root assistant without requiring the page to contain its ancestry.

## Account and balance

The account and quota probes share one effective-selection resolver over Qwen's JSONC settings. It joins `security.auth.selectedType`, `model.name`, the selected `model.baseUrl`, `modelProviders`, the top-level `providerProtocol` map, each model's exact endpoint, and its declared `envKey`; the transport protocol alone never decides the billing provider. Credential values resolve from the process environment, then `${QWEN_HOME:-~/.qwen}/.env`, then settings `env`. Values stay in memory only. The account key is a domain-separated SHA-256 fingerprint of provider, region, credential variable name, and secret bytes, so the same key has one identity across supported sources while a rotation in the same region changes identity; neither the credential nor fingerprint reaches logs or rendered output.

An exact official Coding Plan model endpoint plus its declared credential key, normally `BAILIAN_CODING_PLAN_API_KEY`, selects Alibaba International or China and produces a sub-provider account scope. Recognized direct OpenAI, Anthropic, and Gemini API-key selections are unmetered; missing selection is logged out, while custom endpoints, ADC/external managers, ambiguous provider records, and unevaluable credentials remain unavailable for a short-TTL retry.

**Alibaba quota enrichment is experimental.** RimZ posts the selected API key to that region's fixed Alibaba console host and normalizes an explicitly active instance into authoritative 5-hour, 7-day, and 30-day windows. The transport accepts neither browser cookies, endpoint overrides, redirects, nor alternate-region fallback. The rate-limit cache carries the exact scope and credential fingerprint that produced the windows; a region, provider, or key switch replaces the entry, so prior truth neither paints nor controls the new account.

**Quota-gated launches.** Fresh RimZ-managed Qwen supervised runs start in `PendingResolution`, then resolve the final cwd, launch environment, model, and structured argv into `Bound` for that exact account or `Unresolved` when those inputs cannot prove one; adapters without exact managed-account selection report `Unsupported`. `Bound` reads only a matching scope-and-key cache, while `Unresolved` reads no capacity and keeps the surplus gate closed; `Unsupported` retains ordinary kind-wide capacity. A matching exhausted 5-hour, 7-day, or 30-day window blocks the launch before its run record or pane and exits `125`; matching loop launches record `budget skipped` before their check. Surplus pacing uses the exact binding and the 5-hour/7-day sliding pair, while the 30-day window closes launches only when exhausted. Missing, mismatched, incomplete, resumed, forked, worktree-ambiguous, provider-overridden, or unsupported layered selections stay outside quota control.

That binding is ephemeral launch authority, not Qwen session identity. Hand-launched panes, `--wake` delivery, resumed sessions, displayed status, and reset-timed mid-run auto-continue remain unbound because Qwen exposes no session-bound provider identity and an interactive pane can switch providers. Native `StopFailure` rate-limit and overload reporting remains the session recovery signal, and transcript-priced Qwen dollars remain in the ordinary agent, room, and loop budget engine.

## Cost

Session files are direct regular `.jsonl` children of `<runtime-base>/projects/<project>/chats/`, where runtime base is `$QWEN_RUNTIME_DIR`, then `$QWEN_HOME`, then `~/.qwen`. Nested child logs, sidecars, other extensions, and JSONL outside that exact tree stay out of discovery.

Each refresh cold-folds every readable changed file and authoritatively replaces that file's cached entries, so a root rewind retracts abandoned assistant spend. Active-root assistant records price known uncached prompt, cache-read, and Qwen-normalized output through RimZ's price book. Output requires prompt accounting, prefers `totalTokenCount - promptTokenCount`, then treats thoughts as overlapping when candidates exceed thoughts and adds them otherwise. Unknown or off-book models retain known tokens at zero dollars and register for pricing refresh. Known-model dollars remain shared-pricebook local estimates that participate in RimZ display and soft budget control; they are neither Alibaba quota nor provider billing truth. Spending cache version 18 cold-reprices finalized Qwen transcripts once so historical output and dollars follow the corrected overlap rule.

The live statusline removes Qwen's optional leading provider decoration from each `metrics.models` key, prices every nonzero bucket independently, and sums a complete routed-model estimate into session-coverage card cost. One unknown or off-book material bucket suppresses the whole figure rather than publishing a partial subtotal; empty and zero buckets stay ignorable. These cumulative counters remain cost inputs only. The result uses the shared API price book rather than provider-billed or subscription valuation, so a custom/off-book model or plan without token rates remains honestly absent.

The transcript groups explicit and implicit cache hits in `cachedContentTokenCount`, so RimZ prices the whole category at the conservative implicit-cache rate of 20% of input; explicit hits may therefore be slightly overcounted.

`uuid` is the message dedup key, `sessionId` is the billing thread, and `agentId`/`isSidechain` retain physical sidechain attribution so copied fork and child records can be deduplicated downstream. Root branch rewinds are pruned; sidechain branch pruning remains unavailable from the captured wire.

## Known gaps

Run `rimz coverage` for the current wired/partial/unsupported matrix. The gaps below are the ones with a reason worth recording.

- **The runtime sidecar stays unread.** Hooks bind the session to the pane through the hook child process and `RIMZ_AGENT_PID`. Qwen's `<session>.runtime.json` can establish that binding before the first hook and recover it after hook gaps, but consuming it needs a shared adapter-owned pane/session attribution seam with descendant-process and PID-reuse validation. The adapter leaves it deferred rather than adding a Qwen-only binding path.
- **Dual output stays optional.** `--json-file` plus `--input-file` can improve prompt and permission coverage, but adopting it changes pane launch ownership and adds control-channel races. Hooks and the native pane remain the current decision path.
- **Multi-provider endpoints, subscription metering, and provider-specific off-book pricing** are the declared cost gaps.
- **Concurrent subagent delivery is unproven.** Qwen warns that concurrent-agent hooks are registered at session scope rather than firing scope; live fixtures still need to pin concurrent delivery and parent correlation.
- **Live verification remains required** for native dialog cancellation, Qwen runtime-sidecar transitions, and provider-specific billing behavior.
