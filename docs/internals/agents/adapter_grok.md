# Grok Build adapter

> Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer every adapter implements. Accounts and balances are in [providers.md](./providers.md), spend and pricing in [spending.md](./spending.md), and the raw upstream protocol is in [grok-reference.md](../../externals/agent-adapter/grok-reference.md).

RimZ launches the stock `grok` TUI, installs passive hooks in `${GROK_HOME:-~/.grok}/hooks/rimz.json`, and reads three durable files in each session directory under `${GROK_HOME:-~/.grok}/sessions/`: `updates.jsonl` for conversation, context, and spend, `summary.json` for model, effort, title, and lineage, and `signals.json` for the context window. The adapter makes no network request. Its mapping is checked against Grok Build 1.0.52 at source commit `2bdd1d6a`, the surface [grok-reference.md](../../externals/agent-adapter/grok-reference.md) records. ACP and remote control are outside the adapter.

The adapter lives in [`crates/rimz/src/agents/adapters/grok/`](../../../crates/rimz/src/agents/adapters/grok/mod.rs), one concern per file:

| File | Owns |
| --- | --- |
| [`mod.rs`](../../../crates/rimz/src/agents/adapters/grok/mod.rs) | the `AgentSpec` (`GROK_DESCRIPTOR`), the hook catalog (`GROK_HOOKS`), hook decoding (`decode_hook`, `lifecycle_signal`, `notification_ask`), resume discovery, and the local context refresh |
| [`install.rs`](../../../crates/rimz/src/agents/adapters/grok/install.rs) | the managed hook source and the managed-hooks policy check |
| [`paths.rs`](../../../crates/rimz/src/agents/adapters/grok/paths.rs) | the Grok home, the auth and hooks paths, and transcript validation |
| [`payloads.rs`](../../../crates/rimz/src/agents/adapters/grok/payloads.rs) | the tolerant hook payload |
| [`transcript.rs`](../../../crates/rimz/src/agents/adapters/grok/transcript.rs) | the rewind-aware branch fold, the `summary.json` and `signals.json` readers, and the composite stat |
| [`spend.rs`](../../../crates/rimz/src/agents/adapters/grok/spend.rs) | completed-turn spend |
| [`account.rs`](../../../crates/rimz/src/agents/adapters/grok/account.rs) | the secret-free projection of `auth.json` |

## Hooks and lifecycle

### Install

`rimz hooks install grok` writes one hook block per event in `GROK_HOOKS`, each running `rimz hooks feed --source grok` with a 4-second timeout (`crates/rimz/src/agents/adapters/grok/install.rs::SPEC`). The catalog is every event in the mapping table below. It omits `PreToolUse`, Grok's blocking decision channel, and `PermissionDenied`. Install keeps unrelated hooks in the file and reclaims any older entry whose command carries the same marker. Uninstall removes RimZ's entries and writes back the rest. A relative `GROK_HOME` refuses the install, and `RIMZ_GROK_HOOKS` overrides the hook file path for tests (`crates/rimz/src/agents/adapters/grok/paths.rs::hooks_path`).

The command sets no `RIMZ_AGENT_PID`, so the feed finds its agent by walking up to 32 ancestors for a process whose `comm` is in the spec's `process_names` (`crates/rimz/src/cli/hooks/proctree.rs::walk_to_agent_ancestor`). Those names are `grok` for official installs and `xai-grok-pager` for a source build. A name missing from `process_names` leaves hooks from that binary without an owning agent.

### Managed-hooks policy

Grok drops every hook that is not managed policy once a policy layer sets `allow_managed_hooks_only` (or Claude's spelling, `allowManagedHooksOnly`) to `true`. RimZ reads the same layers before it installs or launches (`crates/rimz/src/agents/adapters/grok/install.rs::policy_blocker_at`):

| Layer | Files |
| --- | --- |
| System | `/etc/grok/managed_config.toml`, `/etc/grok/requirements.toml` |
| User | `managed_config.toml` and `requirements.toml` in `GROK_HOME`, else `$HOME/.grok` |
| Claude managed settings | `/etc/claude-code/managed-settings.json` on Linux, `/Library/Application Support/ClaudeCode/managed-settings.json` on macOS |

The read follows upstream's. An unreadable or unparseable layer is skipped. A layer engages the pin when either key holds any value but `false`, or when the two spellings disagree. An engaged pin blocks RimZ unless a system layer carries a command hook whose command contains `rimz hooks feed --source grok`, since Grok keeps the `/etc/grok` files as managed policy. A handler in the user layer does not count. The user layer resolves from the login env each caller passes, so a pin in one account's Grok home blocks only the logins that use that home.

RimZ refuses a blocked login at two seams. `install_blocker` refuses `rimz hooks install grok`, makes detected installs skip Grok ([adapter.md](./adapter.md#hook-install)), and reaches `rimz doctor`. `hooks_disabled` makes `agents::preflight_hooks` refuse ahead of its hooks-missing check, which covers every launch, restart, resume, and fork, supervised runs, loop fires, and queued message delivery. Both refusals name the file that engaged the pin and the administrator's fix (`crates/rimz/src/agents/adapters/grok/install.rs::POLICY_FIX`). A shipped binary has no override. Only a `testkit` build re-roots the machine-wide paths under `RIMZ_TEST_GROK_POLICY_ROOT`, which the integration harness points at an empty per-test directory (`crates/rimz/tests/integration/common/env.rs`).

Two cases are accepted gaps. RimZ does not verify Grok's signature, so it ignores a handler in a signed user `requirements.toml`. It also does not read macOS MDM policy.

### Event names

Grok spells one event three ways: hook config keys are PascalCase, the stdin field is `hookEventName`, and that field's values are snake_case. `canonical_event_name` drops non-alphanumerics and folds case before it matches a known event, so every spelling reaches shared lifecycle dispatch as the PascalCase name.

### Event mapping

A hook belongs to a child session when its payload carries `subagentType`. Every row below applies to a root session unless it names the child case.

| Native event | Signal | Detail |
| --- | --- | --- |
| `SessionStart` | `registered` on `sessionId` | `source: new` marks fresh lineage. `source: load` marks forked lineage when `summary.json` names a nonempty `parent_session_id` and `session_kind` is `fork`, `worktree`, or `headless`. `summary.json` also supplies the model, effort, title, and a worktree fallback |
| `UserPromptSubmit` | `turn_started` | turn id `promptId`, with the sanitized optional prompt |
| `PostToolUse` | `tool_used` | mutating and editing by tool name from the spec's `tools` lists |
| `PostToolUseFailure` | `tool_used`, neither mutating nor editing | `error` becomes the turn-error label |
| `Notification` | `awaiting_input` for an exact ask, else nothing | see [Notifications](#notifications) |
| `Stop` | `turn_ended`, clean | only for `reason: end_turn`. Every other reason, such as the session-close `channel_closed` and `shutdown`, is ignored |
| `StopFailure` | `turn_ended`, errored | label `errorDetails`, else `error`, cut to 160 characters. `rate_limit` is `paused_rate_limit`, `server_error` is `paused_overloaded`, `authentication_failed`, `invalid_request`, and `max_output_tokens` fail, and any other kind is classified from the label (`TurnErrorClass::classify_label`) |
| `StopCancelled` | root: `turn_interrupted`. Child: `turn_ended` | a child's end is errored unless `cancelledBy` is `user`, because a child cannot rest idle |
| `SubagentStart` | `subagent_started` on `subagentId` | parented by `sessionId`. Task is `description`, else `subagentType`, else `agentType`. Dropped when either id is missing |
| `SubagentStop` | `turn_ended`, clean, on the child's own `sessionId` | shared subagent correlation resolves it under the spawning parent |
| `PreCompact` / `PostCompact` | `compacting` / `compaction_ended` | `source` `auto` or `automatic` is automatic, `manual` is manual |
| `SessionEnd` | `ended` | a child's `SessionEnd` is ignored, because its turn report already resolved the child |

### Turn reports

Grok 1.0.52 fires exactly one turn report per turn: `Stop` for a clean end, `StopFailure` for an API error, or `StopCancelled` for a cancel, a turn cap, or a stalled turn. A child session fires its own hooks with its own `sessionId` and `subagentType`, never `Stop`. No envelope names the parent, so shared subagent correlation joins the child through the `SubagentStart` record the parent fired.

A report can arrive late. Grok dispatches a cancelled turn's report off its command loop, so the report can land after the next `UserPromptSubmit`. An interrupt that kills a slow `Stop` hook also sends `StopCancelled` after the `Stop`. Root `UserPromptSubmit`, `Stop`, `StopFailure`, and `StopCancelled` therefore carry `promptId` as the turn id. The state machine ignores a report for a turn the latest prompt superseded, and a cancel for a turn that already reported ([late turn reports](./model.md#late-turn-reports)). Child reports carry no turn id, since correlation resolves them.

A root turn report also sets a supervised run's final message from `lastAssistantMessage`. A clean `Stop` without that field falls back to the transcript (`crates/rimz/src/agents/adapters/grok/transcript.rs::last_assistant_message`).

### Notifications

An ask opens only on an exact pair of `notificationType` and label, where the label is `message`, else `title` (`crates/rimz/src/agents/adapters/grok/mod.rs::notification_ask`):

| `notificationType` | Label | Ask |
| --- | --- | --- |
| `permission_prompt` | `Tool permission requested` | Permission |
| `permission_prompt` | `Diff review requested` | Permission |
| `permission_prompt` | `Plan approval requested` | PlanApproval |
| `elicitation_dialog` | `User question requested` | Question |

A near match opens nothing. An `agent_error` notification opens no ask and records its `error`, else its `message`, as the turn-error label. Human answers stay in Grok's native pane, and a `Notification` reply is always empty.

A root `idle_prompt` notification with a parseable timestamp writes an `Interrupted` context settle marker and no lifecycle report. The card rests only when that timestamp is newer than its last activity, so a delayed older notification cannot settle a newer prompt. A child notification, or one with a missing or invalid timestamp, attaches nothing. Local context refresh preserves the marker, and later hook activity clears it. The comparison holds because Grok and its feed child read the same host clock. One race remains: when Grok builds the envelope after a new prompt has already reached ingress, the card reads idle until the next hook.

### Hook stdout

Hook stdout is empty except on one path. During a supervised run, a `PostToolUse` hook from the run's own session can carry the run's deadline-rung text as `{"hookSpecificOutput":{"additionalContext":"..."}}`, the shape the spec's `hook_context` declares (`crates/rimz/src/cli/hooks.rs::attach_deadline_context`).

### Foreign payloads

Grok also runs hooks that other providers installed, so a Grok envelope can reach `rimz hooks feed --source claude` or another kind. `GrokAdapter::recognizes_hook_payload` claims a payload that carries both `hookEventName` and `workspaceRoot`, and Grok is the only adapter that implements the recognizer. Under any other `--source`, `AgentDefinition::is_foreign_hook_payload` makes the feed (`crates/rimz/src/cli/hooks.rs::run_feed_inner`) exit 0 with empty stdout before it resolves the event name. Nothing is recorded: no ingress, no lifecycle event, no agent row. The user's Grok configuration stays untouched.

## Launch and resume

| Purpose | Argv |
| --- | --- |
| Interactive | `grok [flags]` |
| Supervised prompt | adds `-p <prompt> --output-format streaming-json`, never on an interactive TUI |
| Resume | `grok --resume <id>` |
| Fork | `grok --resume <id> --fork-session` |
| Model | `--model` |
| Reasoning effort | `--reasoning-effort` |
| Headless turn cap | `--max-turns` |
| Manual compaction | types `/compact`, with any guidance as trailing text |
| Launch reminders | `--rules`, alias `--append-system-prompt` |
| Ask, Auto, Yolo | `--permission-mode default`, `--permission-mode auto`, `--yolo` |
| Plan | nothing |

Launch reminders merge into the last user-supplied `--rules` or `--append-system-prompt` value, joined by a blank line, so the launch carries one rules value (`crates/rimz/src/harness/launch.rs::merge_appended_system_text`).

Plan adds no argv. Grok's `/plan` is interactive, and RimZ maps Plan to no launch flag. [grok-reference.md](../../externals/agent-adapter/grok-reference.md) lists `plan` among the `--permission-mode` values, but whether that mode enforces a plan-only posture is unverified, so it stays unmapped (see [Known gaps](#known-gaps)).

Process discovery reads a running command line whose program is `grok` for `-r <uuid>`, `--resume <uuid>`, or `--resume=<uuid>` (`crates/rimz/src/agents/adapters/grok/mod.rs::resumed_session_id`). A title instead of a UUID, a missing value, or `--fork-session` anywhere on the line leaves the session unknown, and so does a source build's `xai-grok-pager`. A loaded fork records forked lineage at `SessionStart` from `summary.json`, so later resumes keep it without a heuristic at creation.

## Context and transcript

### Transcript resolution

A transcript path is accepted only when it is absolute, has no `..` component, is named `updates.jsonl`, and sits in a directory named exactly the `sessionId`. Its canonical path must then be a file under the canonical sessions root (`crates/rimz/src/agents/adapters/grok/paths.rs::validate_transcript_under`). The name checks run before canonicalization, so a symlink inside the root can resolve to a differently named file. Resolution tries the hook's path, then the previously recorded path, then a scan of `sessions/**/updates.jsonl` for a parent directory named by the session id (`crates/rimz/src/agents/adapters/grok/paths.rs::resolve_transcript`). The same resolver feeds lifecycle, context, history, and spend.

The sessions root comes from the login env the caller passes, with one exception. `decode_hook` carries no env and resolves from the ambient snapshot, so a frame from a Grok agent under another `GROK_HOME` decodes without a transcript until context refresh resolves it under the right home.

### The branch fold

The transcript is read as one logical branch (`crates/rimz/src/agents/adapters/grok/transcript.rs::fold`). Main-thread `user_message_chunk` records open prompts by `_meta.promptIndex`, and visible `agent_message_chunk` text forms assistant output. Thoughts, tool calls, metadata, `bash_command` content, and assistant chunks marked as a sidechain (`isSidechain`, `parentSessionId`, or `subagentId` in `_meta`) stay out of conversation history. User chunks are not sidechain-filtered. Once indexed prompts appear, a later unindexed user run opens no phantom prompt.

A `rewind_marker.target_prompt_index` truncates the fold to that prompt boundary before later records apply. History, context, cold spend, and the live cost of a changed session all reuse this fold, so one live refresh opens `updates.jsonl` once. The supervised final message takes the newest `end_turn` `turn_completed.agent_result` from the tail only when the tail holds no rewind, and otherwise reads it from the full fold. Incremental assistant streaming is append-only: it discards text before the last rewind marker in the newly read suffix, but cannot retract output delivered before the cursor.

### Context refresh

Local context refresh gates on one composite stat over `updates.jsonl`, `summary.json`, and `signals.json` (`crates/rimz/src/agents/adapters/grok/transcript.rs::combined_stat`), then folds the whole transcript. Grok reads no tail, unlike most adapters ([adapter.md](./adapter.md#reading-rules)).

| Field | Source |
| --- | --- |
| Model, reasoning effort, title | `summary.json` (`current_model_id`, `reasoning_effort`, `generated_title` else `session_summary`) |
| Occupancy | newest active-branch `_meta.totalTokens`. Without a rewind, falls back to `signals.json` `contextTokensUsed`, else zero |
| Window | `signals.json` `contextWindowTokens` alone |

After a rewind with no branch sample, occupancy is unknown, so the card never shows the abandoned branch's usage. Completed-turn `usage.inputTokens` sums every model call and child usage, so it is not occupancy, and its categories are not shown as current context detail. A missing or malformed `summary.json` or `signals.json` removes only its own enrichment.

## Account and balance

The probe reads `$GROK_AUTH_PATH`, else `${GROK_HOME:-~/.grok}/auth.json`, as metadata (`crates/rimz/src/agents/adapters/grok/account.rs::probe`). Those variables, the API-key variables, and `GROK_DISABLE_API_KEY_AUTH` all come from the login env, which is the env the launched Grok sees. Deserialization keeps only whether `key` and `refresh_token` exist, never their values.

The file maps record names to records. A record is usable when its `key` is nonempty, its `auth_mode` is usable, and it is unexpired (by `expires_at`, else 30 days after `create_time`) or carries a `refresh_token`:

| `auth_mode` | Usable | Plan label | Metered |
| --- | --- | --- | --- |
| `oidc` | yes | Session | yes |
| `external` | yes | External | yes |
| `api_key` | unless API-key login is disabled | API key | no |
| `web_login` (alias `grok`) | never | | |
| any other | never | | |

A session record beats an API-key record, then the newest `create_time` wins, then the record name in sort order. The winner's account id is `user_id`, else `principal_id`, `team_id`, or `organization_id`, and its label is the team name, else the organization name, email, display name, principal type, or record name. The scope is kind-wide.

With no usable record, a nonempty `XAI_API_KEY` or `GROK_CODE_XAI_API_KEY` reports an unmetered API-key login, and otherwise the probe reports logged out. A truthy `GROK_DISABLE_API_KEY_AUTH` disables API-key login entirely, as upstream does: stored `api_key` records stop counting and the environment key is ignored. Empty, `0`, `false`, `off`, and `no` (case-insensitive, trimmed) leave API-key login on, and every other value turns it off. An `auth.json` that cannot be read (other than missing) or parsed makes the account unavailable. Layered config authentication restrictions and team pins are not projected.

The adapter reports no billing or quota window.

## Cost

Spend comes from active-branch `turn_completed` records on the `_x.ai/session/update` method (`crates/rimz/src/agents/adapters/grok/spend.rs::entries_for_completion`). A record counts only when its `usage` is present and `usageIsIncomplete` is false. It is then priced on one of two paths.

When `costUsdTicks` is present, native dollars win. RimZ requires the value to be nonnegative and `costIsPartial` false, and divides by 10,000,000,000 ticks per USD. Per-model rows with trusted ticks carry exact attribution, and any tokens or ticks left over form one residual row. A partial or negative model row, or model rows that exceed the aggregate, fall back to one aggregate row.

When `costUsdTicks` is absent, RimZ prices the `modelUsage` token categories through the shared price book. A single-model turn prices the aggregate token counts, so a sparse model row cannot lose usage. A multi-model turn prices each row and records leftover tokens in a residual row at zero dollars. On this path a partial model row, or multi-model rows that exceed the aggregate, drop the whole turn, and a record with no `modelUsage` contributes nothing. An unknown model keeps its tokens at zero dollars and enters the shared pricing refresh chase. `grok-4.5` is a long-context canary the price-source check requires (`crates/rimz/src/agents/pricing/source.rs::LONG_CONTEXT_CANARIES`), and the price book's boundary-prefix match resolves Grok Build selectors such as `grok-4.5-build-free` to it.

`inputTokens` includes cached reads, so each entry records `inputTokens - cachedReadTokens` as fresh input and keeps cache reads separate (`crates/rimz/src/agents/adapters/grok/spend.rs::token_split`). `outputTokens` already contains reasoning and is recorded once. The dedup identity is `grok:<session>:<prompt>:<model>`, with `aggregate` for the aggregate and residual rows.

An ordinary refresh resumes at the file byte cursor and reads the new completion rows as written. A rewind in the new suffix triggers a cold fold of the whole branch and returns `SpendUpdate::Replace`, which removes abandoned prompts from the spending cache.

Two kinds of transcript would count a turn twice, and every spend path skips both:

- Fork copies keep the ancestor's `_meta.eventId` while rewriting `params.sessionId`. A completion counts only when its event id is absent or is exactly its own session id, a hyphen, and ASCII digits. This excludes inherited turns, forks of forks included, without opening an ancestor transcript.
- A child session keeps its own top-level `updates.jsonl`, but the parent's `turn_completed` usage already folds the child in. Spend skips any transcript whose `summary.json` `session_kind` starts with `subagent`, and replaces entries cached before the child was recognized. A transcript without that field is priced.

## Known gaps

`rimz coverage --wiring` prints the current wired, partial, and unsupported grid. These gaps have a reason worth recording:

- No quota or billing window. The adapter makes no network request, so the provider block carries spend without budget bars.
- Cost lands only at a completed turn. Native or locally estimated dollars arrive when `turn_completed` writes usage, never mid-turn.
- Child usage that lands after the parent's prompt closed reaches only Grok's session ledger, never a parent `turn_completed`, so fleet spend undercounts it.
- Cache writes are folded into fresh input. `inputTokens` also includes `cacheCreationTokens`, but the Responses backend reports that bucket as zero and per-model rows omit it, so RimZ keeps only the cache-read split.
- Plan launches and `--no-subagents` are unwired. Plan has no mapped launch flag, and Grok honours `--no-subagents` only in the TUI, not in headless runs.
- No shared-server ownership. The adapter has no verified switch to suppress Grok's native language servers, so `disable_native_lsp_args` keeps its no-op default, and configuring [shared servers](../lsp.md) leaves Grok's own servers running.
- Background parking, remote control, and ACP structured answers have no native signal. Human answers stay in Grok's pane.
