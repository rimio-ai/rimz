# Qwen Code adapter

Qwen Code is a standalone, eagerly registered adapter. RimZ installs native hooks in `~/.qwen/settings.json` (or `$QWEN_HOME/settings.json`), wraps a command-mode `ui.statusLine`, and reads session JSONL from the Qwen runtime tree.

## Hooks and lifecycle

| Native event | RimZ signal |
| --- | --- |
| `SessionStart` | register; `compact` closes compaction, while `startup` and `clear` mark fresh lineage |
| `UserPromptSubmit` | turn started |
| `PreToolUse` | plan/question wait for the two blocking tools; ordinary tool activity otherwise |
| `PostToolUse` / `PostToolUseFailure` | completed tool activity |
| `PermissionRequest` | permission wait unless the tool is already a plan/question gate |
| `Stop` / `StopFailure` | clean or failed turn end; pending `background_tasks` or `crons` keep the parent parked |
| `SubagentStart` / `SubagentStop` | child bracket |
| `PreCompact` / `PostCompact` | compaction bracket |
| `SessionEnd` | ended |

The child id is `agent_id`, the parent is the hook's root `session_id`, and `agent_type` labels the child. This wires the native child bracket and renders the tree. Qwen warns that concurrent-agent hooks are registered at session scope rather than firing scope; live fixtures still need to pin concurrent delivery and parent correlation.

Hook stdout stays empty on the neutral path. `PermissionRequest` and the `PreToolUse` matcher for `exit_plan_mode|ask_user_question` remain synchronous; narrowing the matcher keeps a RimZ subprocess off ordinary tool starts, whose completed activity arrives through `PostToolUse`. Install refuses a RimZ-managed blocking entry carrying `async: true`, reclaims owned entries by the `rimz hooks feed --source qwen` command marker, and leaves unrelated hooks intact.

## Context and transcript

At `SessionStart` and `Stop`, the hook path reads the complete transcript and follows the latest root record's `uuid`/`parentUuid` ancestry. The newest active root `assistant` with `usageMetadata` publishes its model and context window plus explicit `totalTokenCount`, or a derived total from known prompt, candidate, thought, and tool-use prompt components. Numeric strings are accepted and malformed optional usage fields stay absent without discarding the record. A readable usage-free transcript means fresh zero, while an unreadable path stays unknown. Tool hooks skip this complete-file enrichment.

Command-mode `ui.statusLine` is wrapped so `rimz statusline feed --source qwen` receives Qwen's rich JSON. It supplies the provider-selected context window and percentage, latest prompt-token gauge, model display name, version, Vim mode, and file-line totals. Cumulative `metrics.models` token totals stay out of the live gauge because they span the session and every routed model. A preset statusline has no command transport, so install leaves it untouched and context falls back to the transcript tail.

Main-thread conversation replay uses the same UUID-parent fold as hook-boundary usage. It indexes identity-bearing records last-wins, selects the latest record without `agentId` or `isSidechain: true`, and walks its ancestry with missing-parent and cycle guards; unknown and system records remain valid links but stay out of visible replay. A transcript with no usable root UUID graph preserves legacy physical order.

Each visible `user`/`assistant` record carries the Google `Content` shape, so replay joins its non-thought `text` parts and drops thought, `functionCall`, and `functionResponse` parts without prose. Incremental `rimz message --wait` and `-p --stream` reads use a separate physical append parser: an appended assistant can point to a parent before the byte cursor, so streaming validates and emits each new visible root assistant without requiring the page to contain its ancestry.

Manual compaction sends `/compress` (`/summarize` is Qwen's alias).

## Account and balance

The account probe reads `security.auth.selectedType` and checks only whether the selected provider's documented credential environment variable is present. It never reads a secret value into output. Qwen publishes no stable cross-provider quota API, so the adapter reports no balance or rate-limit windows.

## Cost

Session files are direct regular `.jsonl` children of `<runtime-base>/projects/<project>/chats/`, where runtime base is `$QWEN_RUNTIME_DIR`, then `$QWEN_HOME`, then `~/.qwen`. Nested child logs, sidecars, other extensions, and JSONL outside that exact tree stay out of discovery.

Each refresh cold-folds every readable changed file and authoritatively replaces that file's cached entries, so a root rewind retracts abandoned assistant spend. Active-root assistant records price known uncached prompt, cache-read, and candidate-plus-thought tokens through RimZ's price book; an unexplained `totalTokenCount` remainder never receives a guessed billing category. Unknown or off-book models retain known tokens at zero dollars and register for pricing refresh.

The transcript groups explicit and implicit cache hits in `cachedContentTokenCount`, so RimZ prices the whole category at the conservative implicit-cache rate of 20% of input; explicit hits may therefore be slightly overcounted.

`uuid` is the message dedup key, `sessionId` is the billing thread, and `agentId`/`isSidechain` retain physical sidechain attribution so copied fork and child records can be deduplicated downstream. Root branch rewinds are pruned; sidechain branch pruning remains unavailable from the captured wire. Multi-provider endpoints, subscription metering, and provider-specific off-book pricing remain the declared cost gaps.

## Integration boundary and deferred work

RimZ preserves Qwen's configured model by default because `security.auth.selectedType` can route to provider-specific catalogs; an `agents.toml` model preset adds `--model` explicitly. Qwen 0.19.9 also exposes direct `--system-prompt` text, while RimZ presets currently model system prompts as file paths, so Qwen system-prompt presets remain rejected until the shared preset abstraction can express text or a safe file-to-text rendering contract.

Hooks bind the session to the pane through the hook child process and `RIMZ_AGENT_PID`. Qwen's `<session>.runtime.json` can establish that binding before the first hook and recover it after hook gaps, but consuming it needs a shared adapter-owned pane/session attribution seam with descendant-process and PID-reuse validation; the adapter leaves that sidecar deferred rather than adding a Qwen-only binding path.

Dual output (`--json-file` plus `--input-file`) remains optional. It can improve prompt and permission coverage, but adopting it changes pane launch ownership and adds control-channel races; hooks and the native pane remain the current decision path.

Live verification remains required for native dialog cancellation, concurrent subagent parent correlation, Qwen runtime-sidecar transitions, and provider-specific billing behavior.
