# Grok Build adapter

Grok is an eagerly registered stock-TUI adapter. RimZ launches `grok`, installs passive global hooks in `${GROK_HOME:-~/.grok}/hooks/rimz.json`, and enriches each session from its durable `updates.jsonl`, `summary.json`, and `signals.json` files. ACP and provider-private billing APIs stay outside this adapter.

## Hooks and lifecycle

Grok uses three naming conventions on one surface: hook config keys are PascalCase, the stdin field is `hookEventName`, and the field's values are snake_case. The classifier accepts snake_case, camelCase, and PascalCase, then returns the canonical PascalCase name before shared lifecycle dispatch.

| Native event | RimZ signal |
| --- | --- |
| `SessionStart` | register the root `sessionId`; `new`, `startup`, and `clear` mark fresh lineage |
| `UserPromptSubmit` | start a turn with the sanitized optional prompt |
| `PostToolUse` / `PostToolUseFailure` | successful descriptor-classified tool activity or failed non-editing activity plus error detail |
| `Notification` | exact permission, plan, diff-review, or question wait |
| `Stop` | clean end, interruption, or errored end from `reason` |
| `StopFailure` | display-only error detail; it does not close a second turn |
| `SubagentStart` / `SubagentStop` | child bracket keyed by `subagentId`, parented by `sessionId` |
| `PreCompact` / `PostCompact` | compaction bracket with manual/automatic source |
| `SessionEnd` | end the session |

Notification classification is exact: `permission_prompt` plus `Tool permission requested` or `Diff review requested` is Permission; `Plan approval requested` is PlanApproval; and `elicitation_dialog` plus `User question requested` is Question. Near matches and `agent_error` do not open an ask. Human answers stay in Grok's native pane, and hook stdout stays empty.

The installer writes one four-second managed command, `RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source grok`, for every passive event. It omits `PreToolUse`, Grok's blocking decision channel, preserves unrelated global hooks, and restores only RimZ-owned entries on uninstall. `RIMZ_GROK_HOOKS` provides an isolated hook-file override for tests.

## Launch and sessions

Interactive launches remain `grok [flags]`. A supervised prompt alone adds `-p <prompt> --output-format streaming-json`; the streaming flags never reach an interactive TUI. Resume is `--resume <id>`, fork is `--resume <id> --fork-session`, model is `--model`, reasoning effort is `--reasoning-effort`, the headless turn cap is `--max-turns`, and manual compaction sends `/compact`.

Ask maps to `--permission-mode default`, Auto to `--permission-mode auto`, and Yolo to `--yolo`. Plan adds no argv because Grok exposes interactive `/plan` but no launch flag that enforces a plan-only posture. Grok has no provider-window ping profile.

A hook-supplied transcript path is accepted only when its canonical path is an `updates.jsonl` below the resolved sessions root and its parent directory exactly equals `sessionId`. Fallback discovery scans `${GROK_HOME:-~/.grok}/sessions/**/updates.jsonl` by that parent identity. The same resolver feeds lifecycle, context, history, and spend.

## Transcript branch and context

`updates.jsonl` is a logical branch. Main-thread `user_message_chunk` records establish prompt boundaries from `_meta.promptIndex`; visible `agent_message_chunk` text forms assistant output. Thoughts, tools, metadata, and subagent sidechains stay out of conversation history. Once indexed prompts appear, later unmarked user runs do not create phantom prompts.

A `rewind_marker.target_prompt_index` truncates the active fold to that prompt boundary before later records apply. History, final assistant output, context, and cold spend all reuse this fold. Incremental assistant streaming is append-only: it discards bytes before the last rewind marker in the newly read suffix, but cannot retract output already delivered before the cursor.

Local context refresh stat-gates the three session files. `summary.json` supplies model, reasoning effort, and stable title; the newest active-branch `_meta.totalTokens` supplies occupancy; `signals.json.contextWindowTokens` supplies the denominator and is the usage fallback before a rewind. After a rewind with no newer token sample, occupancy remains unknown rather than showing stale abandoned-branch usage. Missing or malformed companion files remove only their optional enrichment.

## Spend

Exact dollars come only from active-branch `_x.ai/session/update` `turn_completed` records. RimZ accepts `costUsdTicks` when it is nonnegative, `usageIsIncomplete` is false, and `costIsPartial` is false, then divides by 10,000,000,000 ticks per USD. Missing or rejected native cost produces no spend entry rather than `$0`.

`inputTokens` includes cached reads, so the spend entry records `inputTokens - cachedReadTokens` as fresh input and keeps cache reads separate. `outputTokens` already contains reasoning and is recorded once. Trusted per-model rows carry exact attribution; any remaining aggregate tokens or cost form one residual row, while inconsistent or partial model rows fall back to the trusted aggregate.

Ordinary refreshes resume at the file byte cursor. A rewind in the suffix triggers a cold branch fold with `replace_entries = true`, removing abandoned prompts from the spending cache. The stable dedup identity is the Grok session, prompt, and attributed model.

## Account and known gaps

The account probe reads `${GROK_HOME:-~/.grok}/auth.json` as non-secret metadata. It never retains `key` or `refresh_token`; deserialization records only whether each exists. The freshest valid session login wins over an API-key record, with stable scope order as the final tie-breaker. `XAI_API_KEY` contributes presence only when the file has no usable record. Session/OIDC login is metered, API-key login is unmetered, and malformed auth is unavailable.

The adapter makes no network request and reports no billing or quota window. Realtime cost remains completed-turn only. Background parking, remote control, ACP structured answers, and provider-owned billing extensions remain unsupported.
