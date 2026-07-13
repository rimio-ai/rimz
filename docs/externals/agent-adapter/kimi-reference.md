# Kimi Code protocol reference

> RimZ's adapter mapping lives in [kimi.md](../../internals/agents/kimi.md). The agent-agnostic lifecycle contract is [model.md](../../internals/agents/model.md), and the account, balance, spend, and pricing contract is [providers.md](../../internals/agents/providers.md).

This is the single home for the upstream protocol surface of [`MoonshotAI/kimi-code`](https://github.com/MoonshotAI/kimi-code) relevant to RimZ: lifecycle hooks, durable agent records, session identity and storage, approvals and questions, context and token usage, authentication and quota, subagents, resume and fork behavior, permission modes, and non-interactive execution.

Coverage is depth on viable adapter inputs and breadth as an index. The hook payloads and `wire.jsonl` records are detailed enough to implement typed parsers. The SDK, local server, and ACP surfaces are indexed so an implementer can distinguish them from observation of the stock terminal UI.

## Refresh target and product identity

This mirror was refreshed against Kimi Code **0.23.6**, package release commit [`b5c236d00fd5d825a814bb5ceef0cd54a2acff96`](https://github.com/MoonshotAI/kimi-code/tree/b5c236d00fd5d825a814bb5ceef0cd54a2acff96), and agent-record protocol **1.4**. `kimi --version` prints the product version; the replacement has no `kimi info` command. Each persisted agent log carries its own protocol version in the metadata record.

The executable is `kimi`. The official installer and Homebrew package install a standalone binary; the npm package remains an alternate installation path. The application root is `~/.kimi-code`, not the retired Python CLI's `~/.kimi` root. The old [`MoonshotAI/kimi-cli`](https://github.com/MoonshotAI/kimi-cli) repository is a legacy product and is outside this reference. `kimi migrate` imports supported legacy configuration and sessions; OAuth credentials, MCP authorizations, and legacy plugins are not migrated.

Feature-gate the adapter on its explicitly tested Kimi Code semver range and the new data root. Do not use the executable name alone: both generations publish `kimi`.

## Upstream sources

Re-fetch the published pages and compare the pinned source when refreshing this mirror. Published docs describe the supported interface; pinned source resolves payload details the docs omit.

| Surface | Source |
| --- | --- |
| Repository, install, and release | <https://github.com/MoonshotAI/kimi-code>, [`apps/kimi-code/package.json`](https://github.com/MoonshotAI/kimi-code/blob/b5c236d00fd5d825a814bb5ceef0cd54a2acff96/apps/kimi-code/package.json) |
| CLI options and subcommands | <https://moonshotai.github.io/kimi-code/en/reference/kimi-command.html>, [`cli/options.ts`](https://github.com/MoonshotAI/kimi-code/blob/b5c236d00fd5d825a814bb5ceef0cd54a2acff96/apps/kimi-code/src/cli/options.ts) |
| Hooks | <https://moonshotai.github.io/kimi-code/en/customization/hooks.html>, [`hooks/types.ts`](https://github.com/MoonshotAI/kimi-code/blob/b5c236d00fd5d825a814bb5ceef0cd54a2acff96/packages/agent-core/src/session/hooks/types.ts), [`hooks/engine.ts`](https://github.com/MoonshotAI/kimi-code/blob/b5c236d00fd5d825a814bb5ceef0cd54a2acff96/packages/agent-core/src/session/hooks/engine.ts), [`hooks/runner.ts`](https://github.com/MoonshotAI/kimi-code/blob/b5c236d00fd5d825a814bb5ceef0cd54a2acff96/packages/agent-core/src/session/hooks/runner.ts) |
| Sessions and data locations | <https://moonshotai.github.io/kimi-code/en/guides/sessions.html>, <https://moonshotai.github.io/kimi-code/en/configuration/data-locations.html> |
| Session index and store | [`session-index.ts`](https://github.com/MoonshotAI/kimi-code/blob/b5c236d00fd5d825a814bb5ceef0cd54a2acff96/packages/agent-core/src/session/store/session-index.ts), [`session-store.ts`](https://github.com/MoonshotAI/kimi-code/blob/b5c236d00fd5d825a814bb5ceef0cd54a2acff96/packages/agent-core/src/session/store/session-store.ts) |
| Durable agent records | [`records/types.ts`](https://github.com/MoonshotAI/kimi-code/blob/b5c236d00fd5d825a814bb5ceef0cd54a2acff96/packages/agent-core/src/agent/records/types.ts), [`records/persistence.ts`](https://github.com/MoonshotAI/kimi-code/blob/b5c236d00fd5d825a814bb5ceef0cd54a2acff96/packages/agent-core/src/agent/records/persistence.ts), [`records/migration/index.ts`](https://github.com/MoonshotAI/kimi-code/blob/b5c236d00fd5d825a814bb5ceef0cd54a2acff96/packages/agent-core/src/agent/records/migration/index.ts), [`loop/events.ts`](https://github.com/MoonshotAI/kimi-code/blob/b5c236d00fd5d825a814bb5ceef0cd54a2acff96/packages/agent-core/src/loop/events.ts) |
| Live event protocol | [`protocol/events.ts`](https://github.com/MoonshotAI/kimi-code/blob/b5c236d00fd5d825a814bb5ceef0cd54a2acff96/packages/protocol/src/events.ts) |
| Approvals and questions | [`protocol/approval.ts`](https://github.com/MoonshotAI/kimi-code/blob/b5c236d00fd5d825a814bb5ceef0cd54a2acff96/packages/protocol/src/approval.ts), [`protocol/question.ts`](https://github.com/MoonshotAI/kimi-code/blob/b5c236d00fd5d825a814bb5ceef0cd54a2acff96/packages/protocol/src/question.ts) |
| Configuration and environment | <https://moonshotai.github.io/kimi-code/en/configuration/config-files.html>, <https://moonshotai.github.io/kimi-code/en/configuration/env-vars.html> |
| Agents and subagents | <https://moonshotai.github.io/kimi-code/en/customization/agents.html>, [`session/subagent-host.ts`](https://github.com/MoonshotAI/kimi-code/blob/b5c236d00fd5d825a814bb5ceef0cd54a2acff96/packages/agent-core/src/session/subagent-host.ts) |
| OAuth storage and managed usage | [`oauth/storage.ts`](https://github.com/MoonshotAI/kimi-code/blob/b5c236d00fd5d825a814bb5ceef0cd54a2acff96/packages/oauth/src/storage.ts), [`oauth/managed-usage.ts`](https://github.com/MoonshotAI/kimi-code/blob/b5c236d00fd5d825a814bb5ceef0cd54a2acff96/packages/oauth/src/managed-usage.ts) |
| ACP | <https://moonshotai.github.io/kimi-code/en/reference/kimi-acp.html>, [`packages/acp-adapter`](https://github.com/MoonshotAI/kimi-code/tree/b5c236d00fd5d825a814bb5ceef0cd54a2acff96/packages/acp-adapter) |

## Recommended adapter shape

Keep the stock interactive `kimi` TUI in the pane. Use command hooks as the lifecycle and blocking-wait channel: Kimi Code now exposes `PermissionRequest`, `PermissionResult`, and `Interrupt` in addition to session, prompt, tool, stop, compaction, and subagent hooks. Bind the pane to the exact session from `SessionStart`, then resolve the session directory through `session_index.jsonl`.

Tail `agents/main/wire.jsonl` as durable transcript and usage enrichment. Its records restore agent state; they are not the old Python Wire event stream. In particular, the file contains `turn.prompt`, context messages and loop events, usage records, config changes, and compaction records, but it does not persist the live `turn.ended`, approval-request, question-request, or subagent-lifecycle event union. Hooks remain the primary source for those boundaries.

| RimZ need | Primary surface | Backstop / note |
| --- | --- | --- |
| Pane-to-session binding | `SessionStart.session_id` + `cwd` | exact lookup in `session_index.jsonl` |
| Turn start and prompt | `UserPromptSubmit.prompt` | durable `turn.prompt.input` confirms the prompt |
| Clean turn close | `Stop` | `Stop` is blockable and fires only at a normal model stop; keep RimZ stdout neutral |
| Failed or cancelled close | `StopFailure` / `Interrupt` | pane death and the next prompt reconcile missed delivery |
| Tool work and acting | `PostToolUse` / `PostToolUseFailure` | durable `context.append_loop_event` carries `tool.call` and `tool.result` |
| Permission wait | `PermissionRequest` | `PermissionResult` closes it; correlate by `tool_call_id` |
| User question | `PreToolUse` for `AskUserQuestion` | correlated post-tool hook closes it; the hook API has no separate question event |
| Plan approval | `PermissionRequest` for `ExitPlanMode` | tool input plus the plan file supplies the plan body |
| Compaction | `PreCompact` / `PostCompact` | durable full-compaction records provide recovery evidence |
| Context and tokens | durable `usage.record` plus `agent.status.updated` when hosted | model and split are stored in each usage record |
| Subagents | session `state.json` agent map plus child `wire.jsonl` | hooks name the profile but omit child id; live SDK events carry the exact id |
| Model | `config.update` and `usage.record.model` | launch `--model` and effective config identify the alias |
| Auth/account | configured provider plus credential presence | keep token bytes out of output, logs, and hashes |
| Kimi Code quota | authenticated `GET <managed-base>/usages` | limits plus optional Booster balance/monthly cap |
| Supervised run | `-p/--prompt --output-format stream-json` | prompt mode applies auto permission and rejects `--yolo`, `--auto`, and `--plan` |
| Native resume | `--session <id>` / `--resume <id>` | `--continue` selects the worktree's most recent session |
| Native fork | interactive `/fork` | no documented fork launch flag |

Treat hooks and file tails as at-least-once inputs. Deduplicate by session plus stable turn/tool identifiers or persisted file offset. Parse unknown hook fields, record types, and nested payload fields forward-compatibly.

## Executable, launch flags, and process binding

`kimi` with no arguments creates a new session and starts the stock TUI. Relevant launch flags are:

| Flag | Meaning |
| --- | --- |
| `--version`, `-V` | print the Kimi Code version |
| `--session [id]`, `-S` | resume an id, or open the picker when no id is supplied |
| `--resume [id]`, `-r` | hidden alias for `--session` |
| `--continue`, `-c` | resume the current working directory's latest session |
| `--model <alias>`, `-m` | select a configured model alias for this launch |
| `--prompt <text>`, `-p` | run one prompt without opening the TUI |
| `--output-format text\|stream-json` | choose prompt-mode output; valid only with `--prompt` |
| `--yolo`, `-y` | auto-approve regular tool calls; plan exit still asks |
| `--auto` | handle approvals automatically and suppress user questions |
| `--plan` | start or resume in plan mode |
| `--skills-dir <path>` | replace discovered skill directories; repeatable |
| `--add-dir <path>` | add workspace scope; repeatable |

`--continue` conflicts with `--session`; `--yolo` conflicts with `--auto`. Prompt mode conflicts with `--yolo`, `--auto`, and `--plan`, because it applies auto permission itself. The retired `--afk`, `--print`, `--input-format`, `--final-message-only`, `--quiet`, `--work-dir`, `--agent`, `--agent-file`, and `--wire` interfaces do not belong to Kimi Code.

The normal process argv contains no generated session id, and the runtime sets its process title to `kimi-code`. Bind the pane through `SessionStart.session_id`, and use pane/process liveness as instance truth. The official standalone installer places the executable under `~/.kimi-code/bin`; `/new` and `/sessions` switch identity in-process and produce the corresponding session hooks.

## Configuration and trust surface

The default root is `$KIMI_CODE_HOME`, falling back to `~/.kimi-code`; all configuration and session paths move with it. The only user config file is `$KIMI_CODE_HOME/config.toml`. Kimi Code has no `--config-file`, inline `--config`, or `KIMI_SHARE_DIR` override.

Hooks are executable configuration:

```toml
[[hooks]]
event = "SessionStart"
command = "rimz hooks feed --source kimi"
timeout = 10

[[hooks]]
event = "PermissionRequest"
matcher = ".*"
command = "rimz hooks feed --source kimi"
timeout = 10
```

Each entry accepts `event`, `command`, optional regex `matcher`, and optional `timeout` in seconds. Published configuration accepts only those four keys and validates timeout as 1–600 seconds. The internal hook type also supports `cwd` and `env`, but they are not part of the documented user configuration surface at 0.23.6.

Commands execute through the platform shell with the session project directory as cwd. Matching rules run in parallel; duplicate `(cwd, command)` pairs run once per trigger. Kimi Code exposes no project-level hook tier or hook trust prompt. Include installed command, matcher, and timeout fields in RimZ's trust hash, preview the exact diff, preserve unrelated entries, and reserve hook stdout for Kimi's decision channel.

## Session identity and durable files

The data root contains:

```text
$KIMI_CODE_HOME/                       # default ~/.kimi-code
├── config.toml
├── tui.toml
├── session_index.jsonl
├── credentials/
│   └── kimi-code.json
├── sessions/
│   └── wd_<slug>_<sha256-prefix>/
│       └── <session-id>/
│           ├── state.json
│           ├── agents/
│           │   ├── main/wire.jsonl
│           │   └── agent-0/wire.jsonl
│           ├── tasks/
│           └── logs/kimi-code.log
└── logs/kimi-code.log
```

Each append-only `session_index.jsonl` line carries `sessionId`, absolute `sessionDir`, and `workDir`. Later valid lines win for the same id. Validate that the indexed directory stays inside `$KIMI_CODE_HOME/sessions` and ends in the stated session id. The index workdir can be stale, so use `state.json.workDir` as the authoritative workspace check; `state.json` carries no separate session-id field. Do not reimplement the bucket key to find a bound session.

The bucket key is `wd_<slug>_<first-12-hex-of-sha256>`. A normal session id is `session_<uuid>`. `state.json` carries creation/update metadata, title, last prompt, work directory, fork origin, custom metadata, and an `agents` map. Each agent entry carries its home directory, agent type, parent id, and optional swarm item.

## Command hooks

A hook receives one JSON object on stdin. All keys are snake_case. Common fields are:

```json
{
  "hook_event_name": "PermissionRequest",
  "session_id": "01J...",
  "cwd": "/absolute/project/path"
}
```

The common payload carries no transcript path, model, permission mode, agent id, parent id, pid, or timestamp. Hook commands inherit the ordinary environment; RimZ stamps the owner pid in its installed command when process attribution requires it.

### Event catalog

| Event | Matcher | Event-specific fields | Adapter use |
| --- | --- | --- | --- |
| `SessionStart` | `startup` or `resume` | `source` | register and bind the session |
| `UserPromptSubmit` | submitted text parts joined as text | `prompt: ContentPart[]` | open the user turn; blockable |
| `PreToolUse` | tool name | `tool_name`, `tool_input`, `tool_call_id` | early work/question/plan evidence; blockable |
| `PostToolUse` | tool name | tool identity/input, `tool_output` | completed work; output is text-truncated to 2,000 characters |
| `PostToolUseFailure` | tool name | tool identity/input, structured `error` | failed or denied tool work |
| `PermissionRequest` | tool name | `turn_id`, `tool_call_id`, `tool_name`, `action`, `tool_input`, `display` | open a native approval wait |
| `PermissionResult` | tool name | request identity/action, `decision`, optional scope/feedback/label or error | close the approval wait |
| `Stop` | empty | `stop_hook_active` | normal turn close; blockable once |
| `StopFailure` | error name | `error_type`, `error_message` | failed turn close |
| `Interrupt` | empty | `turn_id`, `reason: "cancelled"` | user-cancelled turn close |
| `SessionEnd` | `exit` | `reason` | remove the session |
| `SubagentStart` | profile name | `agent_name`, prompt preview | parent activity; prompt truncated to 500 characters |
| `SubagentStop` | profile name | `agent_name`, response preview | parent activity; response truncated to 500 characters |
| `PreCompact` | `manual` or `auto` | `trigger`, `token_count` | open compaction bracket |
| `PostCompact` | `manual` or `auto` | `trigger`, `estimated_token_count` | close compaction bracket |
| `Notification` | for example `task.completed` | sink/type/title/body/severity/source fields | background-task activity delivered into context |

`PermissionRequest` and `PermissionResult` are observation-only and fire only when the approval runtime actually asks its RPC client. Policy-approved, YOLO-approved, auto-approved, and statically denied calls do not produce an open native approval panel. Decisions are `approved`, `rejected`, `cancelled`, or `error`; `scope: "session"` means approve-for-session.

Subagent hooks name the profile but omit the generated child id, parent tool-call id, parent agent id, and background flag. Join them with `state.json` and child records, or use the live SDK event surface, before claiming child-row identity.

### Output and exit semantics

| Result | Native behavior |
| --- | --- |
| exit `0`, empty stdout | allow with no injected text |
| exit `0`, plain stdout | allow; blockable prompt/stop callers may append it as context |
| exit `0`, structured deny below | block a blockable event |
| exit `2` | block; trimmed stderr is the reason |
| another non-zero exit | fail open |
| timeout, spawn failure, invalid matcher, abort, or engine error | fail open |

Structured output accepts a top-level `message`, a nested `hookSpecificOutput.message`, and this deny shape:

```json
{
  "hookSpecificOutput": {
    "permissionDecision": "deny",
    "permissionDecisionReason": "explanation"
  }
}
```

Only `UserPromptSubmit`, `PreToolUse`, and `Stop` act on a block. A blocked `Stop` appends the reason as a synthetic user message and permits exactly one corrective continuation; the only emitted call has `stop_hook_active: false`. The neutral RimZ response is empty stdout with exit 0.

## Durable agent records

`agents/<agent-id>/wire.jsonl` is an ordered agent-state log. Each line is one record with its fields at the top level:

```json
{"type":"metadata","protocol_version":"1.4","created_at":1770000000000}
{"type":"turn.prompt","input":[{"type":"text","text":"fix the parser"}],"origin":{"kind":"user"},"time":1770000000100}
{"type":"context.append_loop_event","event":{"type":"tool.call","uuid":"...","turnId":"1","step":1,"stepUuid":"...","toolCallId":"...","name":"Bash","args":{"command":"cargo check"}},"time":1770000000200}
```

This is not the Python CLI's `{timestamp,message:{type,payload}}` Wire envelope. The metadata `protocol_version` gates record migration; the release uses the agent-record version written by `AgentRecords`. Unknown record types and fields remain forward-compatible.

The file writer batches pending records, appends complete newline-terminated JSON, fsyncs the file, and syncs the directory on first creation. Rewrites open with truncation. A reader tolerates only a malformed final unterminated line; malformed complete lines are corruption. A tailer holds an incomplete suffix, tracks file identity and offset, and restarts safely after rewrite or truncation.

Adapter-relevant record families are:

| Record | Key fields | Meaning |
| --- | --- | --- |
| `metadata` | `protocol_version`, `created_at` | file format gate |
| `turn.prompt` / `turn.steer` | `input`, `origin` | turn input; `origin.kind: "user"` identifies genuine human input |
| `turn.cancel` | optional `turnId` | durable cancellation request |
| `config.update` | cwd/model/profile/thinking fields | effective agent configuration |
| `permission.set_mode` | `mode` | `manual`, `yolo`, or `auto` |
| `permission.record_approval_result` | turn/tool/action/result | durable answered approval; no open-request record |
| `full_compaction.begin` / `.cancel` / `.complete` | source/instruction and bracket | full-context compaction recovery |
| `context.append_message` | `message` | model-facing transcript message |
| `context.append_loop_event` | recorded loop event | step, content, tool call, and tool result |
| `context.clear` | no payload | reset the model context and its token count |
| `context.apply_compaction` | summary and token counts | rebuilt context state |
| `usage.record` | `model`, four-way `usage`, optional `usageScope` | additive per-request token accounting; `turn` also contributes to current-turn usage, while `session` covers work such as full compaction |
| `llm.request` | provider/model/alias, effective options and hashes | request reconstruction and model attribution |
| `llm.tools_snapshot` | hash and tool schemas | content-addressed request tool table |

Recorded loop events are `step.begin`, `step.end`, `content.part`, `tool.call`, and `tool.result`. Normal assistant turns are reconstructed from ordered text `content.part` records between the step boundaries; thinking parts and tool plumbing are not chat messages. `context.append_message` carries model-facing context and explicit injected assistant output, not the ordinary assistant-turn reconstruction. Retry, interruption, deltas, and progress are live-only SDK events.

Record `time` is an optional millisecond timestamp. Absence leaves the normalized message or spend row without a time; seconds and file modification times are not timestamp fallbacks.

`step.end.usage` and `usage.record.usage` split `inputOther`, `output`, `inputCacheRead`, and `inputCacheCreation`. Context fill replaces its prior value with the sum of all four fields from the latest nonzero `step.end.usage`, resets on `context.clear`, and becomes `context.apply_compaction.tokensAfter` when compaction lands. Every `usage.record` is additive session spend. `usageScope: "turn"` also updates current-turn usage, while the missing/default `session` scope accounts for work outside a turn such as full compaction; it is not a cumulative session-total record.

`wire.jsonl` does not durably record clean `turn.ended`, `PermissionRequest`, unanswered questions, or the live `agent.status.updated` snapshot. Hooks and pane/process truth supply those facts. Never parse the file as the old Python Wire protocol.

## Blocking approvals, questions, and plan review

The terminal UI keeps the native prompt. RimZ observes it and routes the user to the pane; ordinary messaging continues through pane send.

Approval hooks correlate on `tool_call_id`. `PermissionRequest` carries a structured `display` object and the original tool input; `PermissionResult` closes it with `approved`, `rejected`, `cancelled`, or `error`. The durable `permission.record_approval_result` is a restart backstop for completed approvals, not open-wait truth.

`AskUserQuestion` is a tool whose protocol request supports one to four questions. Each question has text, optional header, two to four options, and optional multi-select; the RPC layer assigns stable ids and adds the free-text choice. The question protocol accepts single, multi, other, multi-with-other, and skipped answers. Command hooks expose the original tool input rather than a separate `QuestionRequest` event, so a stock-pane adapter opens the foreground question wait from `PreToolUse` for `AskUserQuestion` and closes it from the correlated post-tool hook, interrupt, or turn/session end. `background: true` registers a background question task and returns immediately, so it must not park the main row.

Plan mode writes plans under `agents/main/plans/<id>.md`. `ExitPlanMode` uses the normal approval runtime and always asks outside auto mode, including YOLO mode. Classify its `PermissionRequest` as `PlanApproval`; use its input/plan id to read the plan file when the body is needed.

## Native-event mapping

| Kimi observation | RimZ signal or enrichment | Note |
| --- | --- | --- |
| `SessionStart` | `registered` | bind session and record tail |
| `UserPromptSubmit` | `turn_started` | prompt is an array of content parts |
| `PostToolUse` / `PostToolUseFailure` | `tool_used` | successful `Write` and `Edit` prove file editing; failure clears a native wait without claiming mutation |
| `PermissionRequest` | `awaiting_input(Permission)` | specialize `ExitPlanMode` to plan approval |
| `PreToolUse(AskUserQuestion)` | `awaiting_input(Question)` | close on correlated post-tool event |
| `PermissionResult` / post-tool result | clear wait | does not end the turn |
| `Stop` | clean `turn_ended` | neutral hook output preserves the native stop |
| `StopFailure` | errored `turn_ended` | keep error name as enrichment, not a closed enum |
| `Interrupt` | interrupted turn marker | projection settles the row to idle |
| `PreCompact` / `PostCompact` | `compacting` / `compaction_ended` | trigger is `manual` or `auto` |
| `SessionEnd` | `ended` | pane/process liveness remains the backstop |
| `usage.record` | model and token enrichment | exact model accompanies every usage record |
| session agent map and child records | subagent enrichment | hook-only child identity remains partial |

Kimi Code built-in editing tools are `Write` and `Edit`; the shell tool is `Bash`. The old `WriteFile`, `StrReplaceFile`, and `Shell` names belong to the retired Python CLI.

## Context, model, tokens, and cost

`config.update` records the effective model alias, cwd, thinking effort, profile, and system prompt. Separate records carry permission and plan-mode changes. `llm.request` adds the provider, model id, alias, effective thinking/sampling/output controls, and request hashes. `usage.record` stores the model plus the four-way token split. This removes the old adapter's need to price model-less status updates against a guessed default.

The live SDK emits partial `agent.status.updated` events with model, context tokens, maximum context tokens, context ratio, plan/swarm/permission modes, and usage totals. That event surface is available to SDK, server, web, and ACP clients; it is not persisted wholesale to the stock pane's agent record. A stock-pane observer derives context from ordered durable step-end, clear, and compaction boundaries; only turn-scoped usage supplies the current-turn token split.

Kimi Code records tokens but no universal per-request USD price. The server protocol's `SessionUsage.total_cost_usd` currently has no local pricing source for a stock session. Walk records in order so each usage row inherits the latest `llm.request` provider and canonical model when its own model is an alias. Price only an identified model; an unknown alias remains unknown at zero dollars rather than inheriting a guessed Kimi model. Keep Kimi subscription quota units distinct from billable tokens and dollars.

## Subagents and background work

The built-in main profile exposes `coder`, `explore`, and `plan` subagents. A generated child id such as `agent-0` gets its own home and `wire.jsonl`. `state.json.agents` carries each child's parent id and type; the live event protocol adds exact `subagent.spawned`, `.started`, `.suspended`, `.completed`, and `.failed` events with profile, parent tool-call ids, background flag, result, usage, and context tokens where applicable.

Command hooks expose only profile name plus truncated prompt/response. Therefore hook-only child rows remain partial until the adapter joins the session agent map and child logs. Parent activity can update immediately from the hooks.

Background task records live under `<session>/tasks/`. Statuses are `running`, `completed`, `failed`, `timed_out`, `killed`, or `lost`. `Notification` hooks announce terminal background status when the result is delivered into context. Preserve a parent as parked only after joining task state; a normal `Stop` hook does not carry the active-task set.

## Authentication, account, and quota

`kimi login` and `/login` use the RFC 8628 device-code flow. The default managed provider is `managed:kimi-code`; its token is stored at `$KIMI_CODE_HOME/credentials/kimi-code.json`, with directory mode 0700 and file mode 0600. Writes use temp file, fsync, and rename. The JSON contains access and refresh tokens, expiry, scope, token type, and original lifetime. Treat file existence and parseable expiry as an auth hint; never log, render, hash, or copy token values.

For the managed provider, `/usage` calls:

```text
GET https://api.kimi.com/coding/v1/usages
Authorization: Bearer <resolved OAuth token>
Accept: application/json
```

The tolerant parser accepts a summary at `usage`, limit rows at `limits[]` or `limits[].detail`, and reset spelling variants. It also accepts `boosterWallet`: `balance.type == "BOOSTER"`, fixed-point balance and remaining amounts, monthly limit enablement, monthly limit/used money objects, and currency. Preserve source units and currencies; do not infer USD when the response declares another currency. HTTP 401 is an auth failure, 404 means usage is unavailable for the provider, and timeouts/network failures are enrichment failures.

## Headless and supervised runs

Use prompt mode for one supervised turn:

```sh
kimi -p "Run the focused checks" --output-format stream-json
```

Text mode sends assistant text to stdout and thinking/tool progress to stderr. Stream JSON emits assistant messages, tool calls, and tool results as one JSON object per stdout line; thinking and progress remain on stderr. Prompt mode applies auto permission, so it never opens human approvals or questions and rejects explicit `--yolo`, `--auto`, and `--plan` flags.

The CLI does not publish the old Python `0`/`1`/`75` exit-code contract. Normal success is zero; startup, auth, provider, and turn failures follow the Node CLI error path, while goal mode additionally maps blocked or paused terminal goals to distinct non-zero codes. Treat exact failure codes beyond success as release-sensitive and live-test them before promising a supervised-run mapping.

## Resume, clear, and fork

`kimi --continue` resumes the newest indexed session for the current directory. `kimi --session <id>` resumes an exact id; bare `--session` opens the selector. The hidden `--resume` alias is equivalent.

`/new` and its `/clear` alias create and switch to a new session. `/sessions` and `/resume` switch to an existing session. `/compact` compacts the current agent context. `/fork` copies the session into a new id, drops the TUI upcoming-goal queue, rewrites agent home paths, and appends fork markers to every agent record. There is no documented non-interactive fork flag.

## SDK, local server, and ACP index

The Node SDK exposes exact live `Event` objects stamped with `agentId` and `sessionId`, structured approval and question handlers, session creation/resume/fork, and context/status APIs. It is the richest integration surface when RimZ owns the process, but it is not an observation channel for an independently running stock TUI.

`kimi server` exposes authenticated REST and WebSocket APIs plus the web UI. Its session status vocabulary is `idle`, `running`, `awaiting_approval`, `awaiting_question`, and `aborted`. The default server is loopback-only and bearer-authenticated. `--dangerous-bypass-auth` grants filesystem, shell, and session access to every reachable client and is outside a safe adapter path.

`kimi acp` exposes Agent Client Protocol over stdio for IDE clients. It maps approvals, questions, sessions, modes, and tool events into ACP. Like the local server, it is an alternate host for a session rather than a passive feed from the stock pane.

## Implementation checklist and live verification gaps

1. Gate the Kimi kind on a tested `MoonshotAI/kimi-code` version and refuse the retired Python protocol with the migration fix.
2. Install the canonical hook set into `$KIMI_CODE_HOME/config.toml`, preserve unrelated hooks, and include executable fields in the trust hash.
3. Parse every hook into a Kimi-native enum before mapping it to `AgentLifecycleObservation`; keep neutral stdout empty.
4. Resolve the exact session directory from `KIMI_CODE_HOME`, parsed `session_index.jsonl`, hook cwd, and hook session id.
5. Tail `agents/main/wire.jsonl` with typed top-level agent records, newline safety, rewrite detection, and metadata-version gating.
6. Use `PermissionRequest` and `PermissionResult` for approval waits; correlate question waits through `AskUserQuestion` tool hooks.
7. Parse `config.update`, `usage.record`, `llm.request`, and loop-event records for model, context, token, tool, and transcript enrichment.
8. Join `state.json.agents` with child records before declaring exact subagent rows; join `tasks/` before declaring background parking.
9. Probe OAuth/account state without exposing credentials and treat managed quota/Booster data as best-effort enrichment.
10. Implement `-p --output-format stream-json` with stdout/stderr separation and release-pinned exit-code tests.
11. Golden-test hook stdin/stdout, record metadata and samples, approval resolution, session switching, trust diffs, and malformed/unknown fields.

Before declaring the new adapter complete, live-verify:

- the minimum supported Kimi Code version and agent-record protocol version;
- hook ordering for normal stop, provider failure, user interrupt, blocked stop continuation, and session switching;
- approval and question ordering for approve, approve-for-session, reject, dismiss, and interrupt;
- exact `state.json`, `session_index.jsonl`, and child-agent behavior across `/new`, `/sessions`, `/fork`, and resume;
- record rewrite/truncation behavior across undo, fork, and compaction;
- model/context/token attribution after `--model`, `/model`, provider refresh, and resume;
- background task and subagent status across foreground, background, timeout, failure, and process exit;
- managed usage and Booster payloads for each plan RimZ renders;
- prompt-mode error and goal exit codes on the minimum and newest supported releases;
- protocol collision handling, so a legacy `MoonshotAI/kimi-cli` executable is refused with a migration message instead of silently misparsed.
