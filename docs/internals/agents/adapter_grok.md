# Grok Build adapter

> Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer every adapter implements. Accounts and balances are in [providers.md](./providers.md), spend and pricing in [spending.md](./spending.md); the raw upstream protocol is in [grok-reference.md](../../externals/agent-adapter/grok-reference.md).

Grok is an eagerly registered stock-TUI adapter. RimZ launches `grok`, installs passive global hooks in `${GROK_HOME:-~/.grok}/hooks/rimz.json`, and enriches each session from its durable `updates.jsonl`, `summary.json`, `signals.json`, and optional `events.jsonl` files. ACP and provider-private billing APIs stay outside this adapter.

## Hooks and lifecycle

| Native event | RimZ signal |
| --- | --- |
| `SessionStart` | register the root `sessionId`; `new`, `startup`, and `clear` mark fresh lineage |
| `UserPromptSubmit` | start a turn with the sanitized optional prompt |
| `PostToolUse` / `PostToolUseFailure` | successful descriptor-classified tool activity or failed non-editing activity plus error detail |
| `Notification` | exact permission, plan, diff-review, or question wait |
| `Stop` | clean end for `reason: end_turn`; older releases' `cancelled` and `error` reasons still interrupt or fail the turn, and the session-close `channel_closed`/`shutdown` stops are ignored |
| `StopFailure` | errored end with `errorDetails` (or the `error` kind) as the label; `rate_limit` pauses on rate limit, `server_error` pauses as overloaded, `authentication_failed`, `invalid_request`, and `max_output_tokens` fail |
| `StopCancelled` | root session: interruption back to idle; inside a child (`subagentType` present): child end, errored unless `cancelledBy` is `user` |
| `SubagentStart` | child bracket keyed by `subagentId`, parented by `sessionId` |
| `SubagentStop` | fired by the child itself (`sessionId` equals `subagentId`, phase `gate`): a completed child turn that shared correlation resolves under the spawning parent; the parent-fired shape of older releases still closes the child from `exitCode` |
| `PreCompact` / `PostCompact` | compaction bracket with manual/automatic source |
| `SessionEnd` | end the session; a child's own `SessionEnd` is ignored because its turn report already resolved it |

Grok 1.0 fires exactly one turn report per turn: `Stop` for a clean end, `StopFailure` for an API error, or `StopCancelled` for a cancel, turn cap, or stalled turn. A child session fires its own hooks with its own `sessionId` and a `subagentType` field, never `Stop`, and no envelope names the parent: shared subagent correlation joins the child through the `SubagentStart` record the parent fired. Supervised final messages prefer the report's `lastAssistantMessage` and fall back to the transcript tail for `Stop`.

A cancelled turn's report is dispatched off Grok's command loop, so it can land after the next `UserPromptSubmit`, and an interrupt that kills a slow `Stop` hook sends `StopCancelled` after the `Stop`. Root `UserPromptSubmit`, `Stop`, `StopFailure`, and `StopCancelled` therefore carry `promptId` as the turn id, and the state machine ignores a report for the turn the latest prompt superseded, or a cancel for a turn that already reported ([late turn reports](./model.md#late-turn-reports)). Child reports carry no turn id, since subagent correlation resolves them.

Grok uses three naming conventions on one surface: hook config keys are PascalCase, the stdin field is `hookEventName`, and the field's values are snake_case. The classifier accepts snake_case, camelCase, and PascalCase, then returns the canonical PascalCase name before shared lifecycle dispatch.

Notification classification is exact: `permission_prompt` plus `Tool permission requested` or `Diff review requested` is Permission; `Plan approval requested` is PlanApproval; and `elicitation_dialog` plus `User question requested` is Question. Near matches and `agent_error` do not open an ask. Human answers stay in Grok's native pane, and hook stdout stays empty.

**Install.** The installer writes one four-second managed command, `rimz hooks feed --source grok`, for every passive event. The hook helper attributes the event through its bounded process-ancestor walk when `RIMZ_AGENT_PID` is absent. The installer omits `PreToolUse`, Grok's blocking decision channel, preserves unrelated global hooks, reclaims older marker-owned RimZ commands on reinstall, and restores only RimZ-owned entries on uninstall. `RIMZ_GROK_HOOKS` provides an isolated hook-file override for tests.

## Launch and resume

Interactive launches remain `grok [flags]`. A supervised prompt alone adds `-p <prompt> --output-format streaming-json`; the streaming flags never reach an interactive TUI. Resume is `--resume <id>`, fork is `--resume <id> --fork-session`, model is `--model`, reasoning effort is `--reasoning-effort`, the headless turn cap is `--max-turns`, manual compaction sends `/compact` with any guidance as trailing text, and launch reminders ride `--rules` (alias `--append-system-prompt`), merged into a user-supplied occurrence so Grok appends one `<human_rules>` block at session creation.

Ask maps to `--permission-mode default`, Auto to `--permission-mode auto`, and Yolo to `--yolo`. Plan adds no argv because Grok exposes interactive `/plan` but no launch flag that enforces a plan-only posture: 1.0.30 lists `--permission-mode plan`, yet a session launched with it does not start in plan mode.

## Context and transcript

A hook-supplied transcript path is accepted only when its canonical path is an `updates.jsonl` below the resolved sessions root and its parent directory exactly equals `sessionId`. Fallback discovery scans `${GROK_HOME:-~/.grok}/sessions/**/updates.jsonl` by that parent identity. The same resolver feeds lifecycle, context, history, and spend. Only after `updates.jsonl` validates may context enrichment derive an `events.jsonl` sibling whose canonical regular-file path stays in that same canonical session directory; the event file never discovers or identifies a session.

**The transcript is a logical branch.** Main-thread `user_message_chunk` records establish prompt boundaries from `_meta.promptIndex`; visible `agent_message_chunk` text forms assistant output. Thoughts, tools, metadata, and subagent sidechains stay out of conversation history. Once indexed prompts appear, later unmarked user runs do not create phantom prompts.

A `rewind_marker.target_prompt_index` truncates the active fold to that prompt boundary before later records apply. History, context, cold spend, and changed-session live cost reuse this authoritative fold, so live refresh opens `updates.jsonl` once. Final assistant extraction accepts a complete tail `turn_completed.agent_result` only when no rewind appears in that tail and otherwise falls back to the full branch fold. Incremental assistant streaming is append-only: it discards bytes before the last rewind marker in the newly read suffix, but cannot retract output already delivered before the cursor.

Local context refresh stat-gates the validated session files as one aggregate. `summary.json` supplies model, reasoning effort, and stable title; a completed turn's `usage.inputTokens` supplies occupancy and its cache/fresh/output categories supply the matching card detail. Before completion, the newest active-branch `_meta.totalTokens` supplies the live scalar; `signals.json.contextWindowTokens` supplies the denominator and is the usage fallback before a rewind. After a rewind with no newer token sample, occupancy remains unknown rather than showing stale abandoned-branch usage. Missing or malformed companion files remove only their optional enrichment.

**The permission sidecar.** The optional `events.jsonl` tail supplies one display-only permission bracket for Grok versions that persist `permission_requested` and `permission_resolved` records without firing `Notification`. RimZ matches append-ordered records by exact non-empty `tool_name`, publishes the newest unmatched request through `native_permission_wait`, and reads only the bounded record-aligned tail. This marker raises the waiting card and routes attention to Grok's pane; it creates no lifecycle wait, open ask, ask ID, or structured answer path. A later lifecycle activity timestamp self-clears a stale marker through the shared projection.

Exact `Notification` classification remains authoritative when Grok emits it: permission, plan approval, diff review, and question notifications create their existing durable lifecycle/open-ask state. The event sidecar neither broadens those mappings nor competes with them.

## Account and balance

The account probe reads `$GROK_AUTH_PATH`, else `${GROK_HOME:-~/.grok}/auth.json`, as non-secret metadata. It never retains `key` or `refresh_token`; deserialization records only whether each exists. The freshest valid session login wins over an API-key record, with stable scope order as the final tie-breaker. `XAI_API_KEY` or the legacy `GROK_CODE_XAI_API_KEY` contributes presence only when the file has no usable record. Session/OIDC login is metered, API-key login is unmetered, and malformed auth is unavailable.

The adapter makes no network request and reports no billing or quota window.

## Cost

Native dollars take precedence on active-branch `_x.ai/session/update` `turn_completed` records. RimZ accepts `costUsdTicks` when it is nonnegative, `usageIsIncomplete` is false, and `costIsPartial` is false, then divides by 10,000,000,000 ticks per USD. When an otherwise complete record omits native cost, RimZ prices its `modelUsage` token categories through the shared price book; the guaranteed `grok-4.5` row also resolves Grok Build selectors such as `grok-4.5-build-free`. An unknown model retains its tokens at zero dollars and enters the shared pricing refresh chase.

`inputTokens` includes cached reads, so the spend entry records `inputTokens - cachedReadTokens` as fresh input and keeps cache reads separate. `outputTokens` already contains reasoning and is recorded once. Trusted per-model rows carry exact attribution; any remaining aggregate tokens or cost form one residual row, while inconsistent or partial model rows fall back to the trusted aggregate. Locally priced single-model turns use the aggregate token counts so a sparse per-model row cannot lose usage.

Ordinary refreshes resume at the file byte cursor. A rewind in the suffix triggers a cold branch fold with `replace_entries = true`, removing abandoned prompts from the spending cache. The stable dedup identity is the Grok session, prompt, and attributed model.

A child session keeps its own top-level `updates.jsonl`, but the parent's `turn_completed` usage already folds the child in. Spend therefore skips any transcript whose `summary.json` `session_kind` starts with `subagent`; a transcript without that field is priced as before.

## Known gaps

Run `rimz coverage` for the current wired/partial/unsupported matrix. The gaps below are the ones with a reason worth recording.

- **No quota or billing window.** The adapter makes no network request, so the provider block carries spend without budget bars.
- **Realtime cost is completed-turn only.** Native or locally estimated dollars land when `turn_completed` writes usage, never mid-turn.
- **Child usage that lands after the parent's prompt closed** reaches only Grok's session ledger, never a parent `turn_completed`, so fleet spend undercounts it.
- **Cache writes are folded into fresh input.** `inputTokens` also includes `cacheCreationTokens`; the Responses backend reports that bucket as zero and per-model rows omit it, so RimZ keeps the cache-read split only.
- **A turn report without `promptId`** falls back to arrival order, so on a release that omits it a late `StopCancelled` can still idle the next turn, and an interrupted bash-mode command can still clear a success card.
- **Plan launches and `--no-subagents`** stay unwired: plan mode is not enforced by the launch flag, and Grok honours `--no-subagents` only in the TUI, not headless runs.
- **Background parking, remote control, and ACP structured answers** have no native signal. Human answers stay in Grok's pane.
