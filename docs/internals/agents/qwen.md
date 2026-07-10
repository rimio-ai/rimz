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

The child id is `agent_id`, the parent is the hook's root `session_id`, and `agent_type` labels the child. Qwen warns that concurrent-agent hooks are registered at session scope rather than firing scope; child association remains display enrichment until live fixtures pin every concurrent shape.

Hook stdout stays empty on the neutral path. `PermissionRequest` and `PreToolUse` entries remain synchronous; install refuses a RimZ-managed blocking entry carrying `async: true`. Install reclaims owned entries by the `rimz hooks feed --source qwen` command marker and leaves unrelated hooks intact.

## Context and transcript

The hook-time tail scan takes the newest root `assistant` record with `usageMetadata`, excluding `isSidechain: true` and records with `agentId`. It publishes `totalTokenCount`, `model`, and `contextWindowSize`; a readable usage-free transcript means fresh zero, while an unreadable path stays unknown.

Command-mode `ui.statusLine` is wrapped so `rimz statusline feed --source qwen` receives Qwen's rich JSON. It supplies the provider-selected context window and percentage, model display name, version, Vim mode, token categories across every `metrics.models` entry, and file-line totals. A preset statusline has no command transport, so install leaves it untouched and context falls back to the transcript tail.

Manual compaction sends `/compress` (`/summarize` is Qwen's alias).

## Account and balance

The account probe reads `security.auth.selectedType` and checks only whether the selected provider's documented credential environment variable is present. It never reads a secret value into output. Qwen publishes no stable cross-provider quota API, so the adapter reports no balance or rate-limit windows.

## Cost

Session files live below `<runtime-base>/projects/*/chats/`, where runtime base is `$QWEN_RUNTIME_DIR`, then `$QWEN_HOME`, then `~/.qwen`. Each assistant record prices uncached prompt, cache-read, and candidate-plus-thought tokens through RimZ's price book. Unknown or off-book models retain tokens at zero dollars and register for pricing refresh.

The transcript groups explicit and implicit cache hits in `cachedContentTokenCount`, so RimZ prices the whole category at the conservative implicit-cache rate of 20% of input; explicit hits may therefore be slightly overcounted.

`uuid` is the message dedup key, `sessionId` is the billing thread, and `agentId`/`isSidechain` retain sidechain attribution so copied fork and child records can be deduplicated downstream. Multi-provider endpoints and subscription metering remain the declared cost gap.

Live verification remains required for native dialog cancellation, concurrent subagent parent correlation, and provider-specific billing behavior.
