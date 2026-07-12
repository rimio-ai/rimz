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

The hook-time tail scan takes the newest root `assistant` record with `usageMetadata`, excluding `isSidechain: true` and records with `agentId`. It publishes `totalTokenCount`, `model`, and `contextWindowSize`; a readable usage-free transcript means fresh zero, while an unreadable path stays unknown.

Command-mode `ui.statusLine` is wrapped so `rimz statusline feed --source qwen` receives Qwen's rich JSON. It supplies the provider-selected context window and percentage, latest prompt-token gauge, model display name, version, Vim mode, and file-line totals. Cumulative `metrics.models` token totals stay out of the live gauge because they span the session and every routed model. A preset statusline has no command transport, so install leaves it untouched and context falls back to the transcript tail.

Manual compaction sends `/compress` (`/summarize` is Qwen's alias).

## Account and balance

The account probe reads `security.auth.selectedType` and checks only whether the selected provider's documented credential environment variable is present. It never reads a secret value into output. Qwen publishes no stable cross-provider quota API, so the adapter reports no balance or rate-limit windows.

## Cost

Session files live below `<runtime-base>/projects/*/chats/`, where runtime base is `$QWEN_RUNTIME_DIR`, then `$QWEN_HOME`, then `~/.qwen`. Each assistant record prices uncached prompt, cache-read, and candidate-plus-thought tokens through RimZ's price book. Unknown or off-book models retain tokens at zero dollars and register for pricing refresh.

The transcript groups explicit and implicit cache hits in `cachedContentTokenCount`, so RimZ prices the whole category at the conservative implicit-cache rate of 20% of input; explicit hits may therefore be slightly overcounted.

`uuid` is the message dedup key, `sessionId` is the billing thread, and `agentId`/`isSidechain` retain sidechain attribution so copied fork and child records can be deduplicated downstream. The current parser prices physical assistant records and does not reconstruct the `parentUuid` chain after `/rewind`, so abandoned branch records can overstate spend. Multi-provider endpoints, rewind pruning, and subscription metering remain the declared cost gaps.

## Integration boundary and deferred work

RimZ preserves Qwen's configured model by default because `security.auth.selectedType` can route to provider-specific catalogs; an `agents.toml` model preset adds `--model` explicitly. Qwen 0.19.9 also exposes direct `--system-prompt` text, while RimZ presets currently model system prompts as file paths, so Qwen system-prompt presets remain rejected until the shared preset abstraction can express text or a safe file-to-text rendering contract.

Hooks bind the session to the pane through the hook child process and `RIMZ_AGENT_PID`. Qwen's `<session>.runtime.json` can establish that binding before the first hook and recover it after hook gaps, but consuming it needs a shared adapter-owned pane/session attribution seam with descendant-process and PID-reuse validation; the adapter leaves that sidecar deferred rather than adding a Qwen-only binding path.

Active-branch spend requires a parser result that can retract cached entries when a later `system/rewind` changes the selected `parentUuid` chain. The shared append-only spend cursor accepts additions only, so branch-aware accounting remains deferred to a replacement/retraction-capable parser contract.

Dual output (`--json-file` plus `--input-file`) remains optional. It can improve prompt and permission coverage, but adopting it changes pane launch ownership and adds control-channel races; hooks and the native pane remain the current decision path.

Live verification remains required for native dialog cancellation, concurrent subagent parent correlation, Qwen runtime-sidecar transitions, and provider-specific billing behavior.
