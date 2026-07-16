# Qwen Code adapter

Qwen Code is a standalone, eagerly registered adapter. RimZ installs native hooks in `~/.qwen/settings.json` (or `$QWEN_HOME/settings.json`), wraps a command-mode `ui.statusLine`, and reads session JSONL from the Qwen runtime tree.

## Hooks and lifecycle

| Native event | RimZ signal |
| --- | --- |
| `SessionStart` | register; `compact` closes compaction, while `startup` and `clear` mark fresh lineage |
| `UserPromptSubmit` | turn started; a present-but-blank prompt is an internal continuation and stays inside the current turn |
| `PostToolUse` / `PostToolUseFailure` | completed tool activity |
| `PermissionRequest` | permission wait; the plan/question gate tools classify as plan/question waits |
| `Stop` / `StopFailure` | clean or failed turn end; pending `background_tasks` or `crons` keep the parent parked |
| `SubagentStart` / `SubagentStop` | child bracket |
| `PreCompact` / `PostCompact` | compaction bracket |
| `SessionEnd` | ended |

The child id is `agent_id`, the parent is the hook's root `session_id`, and `agent_type` labels the child. This wires the native child bracket and renders the tree. Qwen warns that concurrent-agent hooks are registered at session scope rather than firing scope; live fixtures still need to pin concurrent delivery and parent correlation.

Hook stdout stays empty on the neutral path. Qwen presents gate dialogs in the tool-confirmation stage, which fires synchronous `PermissionRequest` with the tool's name and input; `PreToolUse` fires only at execution after the user has answered, so RimZ installs no `PreToolUse` hook and classifies both gates from `PermissionRequest`. Install refuses a RimZ-managed `PermissionRequest` entry carrying `async: true`, reclaims owned entries by the `rimz hooks feed --source qwen` command marker, and leaves unrelated hooks intact.

## Context and transcript

At `SessionStart` and `Stop`, the hook path reads the complete transcript and follows the latest root record's `uuid`/`parentUuid` ancestry. The newest active root `assistant` with `usageMetadata` supplies its model, context window, total, cache-read prompt, fresh prompt, and normalized output; at `Stop`, RimZ accepts that summary only when its `promptTokenCount` matches the hook's direct `input_tokens`. Numeric strings are accepted and malformed optional usage fields stay absent without discarding the record. A readable usage-free transcript means fresh zero, while an unreadable path stays unknown. Tool hooks skip this complete-file enrichment.

Command-mode `ui.statusLine` is wrapped so `rimz statusline feed --source qwen` receives Qwen's rich JSON. It owns the provider-selected context window, percentage, and scalar `current_usage` occupancy, plus the provider-prefix-stripped model display name, version, Vim mode, file-line totals, and complete locally estimated session cost. The card renders the scalar as its live `▤` total and a flat meter until an exact transcript call split has the same filled-input total; a matching split adds the cache, input, and output markers and meter segments, while a newer mismatch immediately returns to the scalar-only view. An explicit zero clears the live occupancy after compaction. Cumulative `metrics.models` counters stay private to the live cost estimate and do not become card token categories. A preset statusline has no command transport, so install leaves it untouched and context falls back to lifecycle enrichment.

Main-thread conversation replay uses the same UUID-parent fold as hook-boundary usage. It indexes identity-bearing records last-wins, selects the latest record without `agentId` or `isSidechain: true`, and walks its ancestry with missing-parent and cycle guards; unknown and system records remain valid links but stay out of visible replay. A transcript with no usable root UUID graph preserves legacy physical order.

Each visible `user`/`assistant` record carries the Google `Content` shape, so replay joins its non-thought `text` parts and drops thought, `functionCall`, and `functionResponse` parts without prose. Incremental `rimz message --wait` and `-p --stream` reads use a separate physical append parser: an appended assistant can point to a parent before the byte cursor, so streaming validates and emits each new visible root assistant without requiring the page to contain its ancestry.

Manual compaction sends `/compress` (`/summarize` is Qwen's alias).

## Account and balance

The account and quota probes share one effective-selection resolver over Qwen's JSONC settings. It joins `security.auth.selectedType`, `model.name`, the selected `model.baseUrl`, `modelProviders`, the top-level `providerProtocol` map, each model's exact endpoint, and its declared `envKey`; the transport protocol alone never decides the billing provider. Credential values resolve from the process environment, then `${QWEN_HOME:-~/.qwen}/.env`, then settings `env`. Values stay in memory only, while the account key records normalized provider/region, the credential variable name, source kind/path, and file modification time without hashing or persisting the secret.

An exact official Coding Plan model endpoint plus its declared credential key, normally `BAILIAN_CODING_PLAN_API_KEY`, selects Alibaba International or China and produces a sub-provider account scope. Recognized direct OpenAI, Anthropic, and Gemini API-key selections are unmetered; missing selection is logged out, while custom endpoints, ADC/external managers, ambiguous provider records, and unevaluable credentials remain unavailable for a short-TTL retry.

Alibaba quota enrichment is experimental. RimZ posts the selected API key to that region's fixed Alibaba console host and normalizes an explicitly active instance into authoritative 5-hour, 7-day, and 30-day windows. The transport accepts neither browser cookies, endpoint overrides, redirects, nor alternate-region fallback. A region or provider switch changes the account scope immediately, so cached plan, credits, and windows from the previous selection do not paint the panel.

Alibaba-scoped quota is display-only because durable Qwen sessions do not yet carry the effective provider identity needed for safe control. These windows do not mark sessions rate-limited, arm auto-continue, contribute surplus, or suppress scheduled priming; existing kind-wide providers retain those controls.

## Cost

Session files are direct regular `.jsonl` children of `<runtime-base>/projects/<project>/chats/`, where runtime base is `$QWEN_RUNTIME_DIR`, then `$QWEN_HOME`, then `~/.qwen`. Nested child logs, sidecars, other extensions, and JSONL outside that exact tree stay out of discovery.

Each refresh cold-folds every readable changed file and authoritatively replaces that file's cached entries, so a root rewind retracts abandoned assistant spend. Active-root assistant records price known uncached prompt, cache-read, and Qwen-normalized output through RimZ's price book. Output requires prompt accounting, prefers `totalTokenCount - promptTokenCount`, then treats thoughts as overlapping when candidates exceed thoughts and adds them otherwise. Unknown or off-book models retain known tokens at zero dollars and register for pricing refresh. Known-model dollars remain shared-pricebook local estimates that participate in RimZ display and soft budget control; they are neither Alibaba quota nor provider billing truth. Spending cache version 18 cold-reprices finalized Qwen transcripts once so historical output and dollars follow the corrected overlap rule.

The live statusline removes Qwen's optional leading provider decoration from each `metrics.models` key, prices every nonzero bucket independently, and sums a complete routed-model estimate into session-coverage card cost. One unknown or off-book material bucket suppresses the whole figure rather than publishing a partial subtotal; empty and zero buckets stay ignorable. These cumulative counters remain cost inputs only. The result uses the shared API price book rather than provider-billed or subscription valuation, so a custom/off-book model or plan without token rates remains honestly absent.

The transcript groups explicit and implicit cache hits in `cachedContentTokenCount`, so RimZ prices the whole category at the conservative implicit-cache rate of 20% of input; explicit hits may therefore be slightly overcounted.

`uuid` is the message dedup key, `sessionId` is the billing thread, and `agentId`/`isSidechain` retain physical sidechain attribution so copied fork and child records can be deduplicated downstream. Root branch rewinds are pruned; sidechain branch pruning remains unavailable from the captured wire. Multi-provider endpoints, subscription metering, and provider-specific off-book pricing remain the declared cost gaps.

## Integration boundary and deferred work

RimZ preserves Qwen's configured model by default because `security.auth.selectedType` can route to provider-specific catalogs; an `agents.toml` model preset adds `--model` explicitly. Qwen 0.19.9 also exposes direct `--system-prompt` text, while RimZ presets currently model system prompts as file paths, so Qwen system-prompt presets remain rejected until the shared preset abstraction can express text or a safe file-to-text rendering contract.

Hooks bind the session to the pane through the hook child process and `RIMZ_AGENT_PID`. Qwen's `<session>.runtime.json` can establish that binding before the first hook and recover it after hook gaps, but consuming it needs a shared adapter-owned pane/session attribution seam with descendant-process and PID-reuse validation; the adapter leaves that sidecar deferred rather than adding a Qwen-only binding path.

Dual output (`--json-file` plus `--input-file`) remains optional. It can improve prompt and permission coverage, but adopting it changes pane launch ownership and adds control-channel races; hooks and the native pane remain the current decision path.

Live verification remains required for native dialog cancellation, concurrent subagent parent correlation, Qwen runtime-sidecar transitions, and provider-specific billing behavior.
