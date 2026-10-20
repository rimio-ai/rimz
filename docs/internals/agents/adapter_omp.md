# OMP adapter

> Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer. Accounts and balances are in [providers.md](./providers.md), spend and pricing in [spending.md](./spending.md); the upstream contract is [omp-reference.md](../../externals/agent-adapter/omp-reference.md).

OMP (Oh My Pi) is an experimental built-in, independent of Pi's adapter. Its RimZ-authored [extension.ts](../../../crates/rimz/src/agents/adapters/omp/extension.ts) forwards snapshots to `rimz hooks feed --source omp`. Tool handlers return immediately without awaiting RimZ; `before_agent_start` appends launch reminders to the turn's `systemPrompt` array. `session_shutdown` returns the queued delivery promise so OMP can await its envelope under the native two-second shutdown budget. Helper delivery is serialized in event order, with envelopes captured at event time. This prevents a slow registration helper from landing after turn start and resetting the running card to idle. A switch registration carries only session identity and cwd: OMP restores model, effort, and context after `session_switch`, so later envelopes supply those observations instead. Helpers receive fresh piped stdio, `RIMZ_AGENT_PID` is the OMP process PID, and `RIMZ_BIN` can select the helper executable. Helper output never becomes an OMP tool decision.

## Hooks and lifecycle

The catalog and decoder live in `crates/rimz/src/agents/adapters/omp/mod.rs::OMP_HOOKS` and `crates/rimz/src/agents/adapters/omp/mod.rs::OmpAdapter::decode_hook`. Wire event names below are RimZ's envelope names; the extension translates native camelCase fields to snake_case.

| Native event | Fed event | Signal and fields |
| --- | --- | --- |
| `session_start` | `session_start` | `Registered`; id and cwd from `ctx.sessionManager` |
| `session_switch` to a different id | `session_shutdown`, then `session_start` | `Ended` for the old id, then `Registered` for the new one; same-id resume emits nothing |
| `before_agent_start` | `before_agent_start` | `TurnStarted`; prompt labels the row |
| `agent_end` | `agent_end` | `TurnEnded` unless `will_continue` is true; `stop_reason: aborted` gives `TurnInterrupted`, `error` or a nonempty `error_message` gives an errored end; final assistant text supplies the supervised result |
| `turn_end` | `turn_end` | Progress and gauge refresh, no lifecycle signal |
| `message_update` | `message_update` | Progress on a changed gauge, at most once per second |
| `after_provider_response` | `after_provider_response` | Context only, including rate-limit windows |
| `tool_call` for `ask`, with UI | `tool_call` | `AwaitingInput { Question }`, keyed by `tool_call_id`; questions/options from `tool_input` |
| `tool_execution_end` | `tool_execution_end` | Named `ToolUsed { mutates, edits }`, keyed by `tool_call_id`; completes an ask wait |
| `tool_approval_requested`, with UI | `tool_approval_requested` | `AwaitingInput { Permission }`, keyed by `tool_call_id`; detail from reason or tool name |
| `tool_approval_resolved` | `tool_approval_resolved` | Unnamed keyed `ToolUsed`, clearing the permission wait |
| `session_before_compact` | `session_before_compact` | `Compacting` |
| `session_compact` | `session_compact` | `CompactionEnded { failed: false }` after a committed compaction |
| `auto_compaction_end` with abort, error, or skip | `auto_compaction_end` | `CompactionEnded { failed: true, auto: Some(true) }` |
| `session_shutdown` | `session_shutdown` | `Ended` |

Every handler excludes `ctx.agent?.kind === "sub"`. The extension drops late `ask` completions and approval resolutions after a final `agent_end`: an unnamed clearing signal on a settled row would reopen the turn. Headless asks and approval requests never raise Waiting. All neutral hook replies are empty stdout. The catalog's blocking classification describes the native ask, not synchronous RimZ delivery: `tool_call` never waits on or denies through RimZ. OMP treats a thrown or timed-out tool-call handler as a denial.

The descriptor declares native `agent_end` turn ends and wired turn/compaction concerns; the user-facing **state** capability remains Partial for the settlement gap below. Wiring totals are 10 Wired, 3 Partial, 6 Unsupported. Children have no lifecycle signal; `lost` derives from the exec wrapper as for every adapter.

The editing set is `edit`, `write`, `ast_edit`. The broader mutating set also includes `bash`, `eval`, `debug`, `ida`, `github`, `lsp`, `rewind`, `learn`, `manage_skill`, `security_scan`, and `task`. Named completion events feed live tool counts; session `toolCall` blocks feed history.

### Install and launch

`ManagedSource` owns `<HOME>/.omp/agent/extensions/rimz.ts`, resolving `HOME` from the login environment, not the install process. `RIMZ_OMP_EXTENSION` overrides that file for tests. Install and preview refuse an unmarked file; a first-line `_rimz_managed` file is refreshed verbatim, and uninstall removes it whole. XDG does not move this extension directory. Restart OMP after refreshing it: OMP has no built-in `/reload` command.

`crates/rimz/src/agents/adapters/omp/mod.rs::OmpAdapter::hooks_disabled` refuses non-default `OMP_PROFILE` (or `PI_PROFILE` only when `OMP_PROFILE` is unset), nonempty `PI_CONFIG_DIR`, and nonempty `PI_CODING_AGENT_DIR`, with the variable and unset fix. Empty, whitespace, and `default` profile values select the default; an empty `OMP_PROFILE` still wins over `PI_PROFILE`. No config-home override key is declared, so named RimZ accounts are unsupported. Following Pi's shared home key would risk overwriting Pi's independent `rimz.ts`.

Launch uses `omp -- <prompt>`. Native pickers use `omp --resume`; recorded sessions resume with `omp --resume <id>` and fork with `omp --fork <id>`. Importing an unrecorded session by id is unsupported. Ask and Plan both use `--approval-mode always-ask`, Auto uses `write`, and Yolo uses `yolo`. Plan is conservative approval, not native plan mode: OMP's `--plan` selects a model role, not a permission mode. An unmodified OMP launch defaults to yolo.

Model and effort render through `--model` and `--thinking`. System-prompt replacement passes text with `--system-prompt`; launch reminders instead use `RIMZ_LAUNCH_REMINDERS`, claimed once in the shared JS realm and appended only for the main agent. Using `--append-system-prompt` for reminders would displace the user's discovered `APPEND_SYSTEM.md`. The skill root is `<config_home>/skills`, with frontmatter manual skills; scoped profile skill lists are refused and there is no host restriction switch. Manual compaction is `/compact [instructions]`.

## Context and transcript

Except for switch registrations, envelopes can carry model, effort, rounded context fill/window/tokens, the latest assistant input/output/cache-read/cache-write split, and provider windows. `ctx.getContextUsage()` supplies the gauge; branch hydration distinguishes a fresh zero from unavailable usage. There is no transcript-tail gauge or live dollar push. `crates/rimz/src/agents/adapters/omp/payloads.rs::context` normalizes those observations.

`crates/rimz/src/agents/adapters/omp/transcript.rs::parse_messages` skips the title slot and session header, projects user and assistant text blocks, and sanitizes user prompts through the shared classifier. Tool results and thinking blocks are not visible conversation text.

## Account and balance

The adapter never opens `agent.db`, the SQLite store containing plaintext credentials alongside other agent state. Login identity, plan, and direct account usage are unknown. The extension reads lowercased provider response headers for Codex and Anthropic window families; windows appear only when reported. A pane can switch backing providers, so these are observations, not a verified account binding.

## Cost

`crates/rimz/src/agents/adapters/omp/mod.rs::OmpAdapter::spending_sources` scans `<config_home>/sessions` plus `$XDG_DATA_HOME/omp/sessions` when the nonempty XDG root's `omp` directory exists. `crates/rimz/src/agents/adapters/omp/mod.rs::OmpAdapter::spending_session_id` accepts underscore-suffixed UUID main-session names, excluding ordinary task filenames.

`crates/rimz/src/agents/adapters/omp/spend.rs::parse` counts assistant `message.usage` and standalone `model_usage` records across all appended branches, not just the active window. Compaction and branch-summary records carry no usage of their own. Nonnegative finite `usage.cost.total` is used verbatim, with price-book fallback when absent; it is OMP's accounting figure, not an invoice. Header cwd supplies project attribution. A changed header id or truncation reparses from the beginning with `SpendUpdate::Replace`, including an empty truncated file.

Dollars reconcile at turn end. `model_usage` does not reach extension handlers, so pushing assistant-only live totals would overwrite fuller history reconciliation. History remains Partial because child spend is excluded.

## Known gaps

- Only the default agent directory is wired. Relocated profiles/homes and named RimZ accounts are unsupported. Session-only relocation through `--session-dir` or `PI_CODING_AGENT_SESSION_DIR` can also put history outside the scanned roots.
- Task children are deliberately filtered from every handler, with no child rows or task-file spend. Whether parent records already include child usage is unverified; counting both risks double-counting.
- `agent_end` without `willContinue` ends the RimZ turn, but no dedicated extension settlement event proves complete RPC quiescence. Background work can outlive the displayed end. Background parking/shells and an idle-timeout notification are unsupported.
- Failed manual compaction emits no closing event. The card can remain compacting until later lifecycle activity; automatic abort/error/skip is covered.
- Plan review is unsupported. A keyed close after an idle review returned Running, not Idle, through the real fold; wiring it would reopen a turn. No shared state-machine branch was added.
- No live dollars, credential-store identity/plan, direct account query, or out-of-band `rimz answer`. Answer native approvals and questions in the OMP pane.
- Only prebuilt `omp` process identity is detected. Bun-run package installs are not: declaring generic `bun` would collide with OpenCode detection.
- Raw `--no-extensions`, `--trusted-extension`, or `--profile` arguments can bypass the ambient integration. Environment preflight does not inspect argv.

## Verified against omp 18.11.1

The live proof used disposable HOME/RIMZ_HOME/XDG roots, a private tmux server, and a keyless local OpenAI-compatible SSE provider. The built `rimz hooks install omp` installed 14 hooks; installed bytes matched the embedded source, and isolated `rimz doctor` reported `1 reporting to RimZ: omp`. No user credentials or `agent.db` were read.

`omp --model probe/probe -- <prompt>` submitted `hello`, `config`, and `-prefixed` unchanged in `before_agent_start`. For each run `/proc/<pid>/comm` was `omp`, and the logged `RIMZ_AGENT_PID` matched the pane's OMP PID. Observed delivered order: `session_start` → `before_agent_start` → `tool_execution_end(read)` → final `agent_end` with `will_continue` absent. A native logger also observed `tool_call(read)` and an earlier `agent_end` with `willContinue=true`; RimZ intentionally forwards `tool_call` only for interactive asks. The ordered-delivery consumer regression is in `crates/rimz/tests/integration/examples/omp_extension.rs::extension_orders_delivery_without_blocking_native_handlers`.

This proves prompt parsing, prebuilt identity, extension loading, and the exercised local lifecycle, not real-provider authentication/billing, live native asks/compaction, or Bun package launches. The [artifact evidence](../../externals/agent-adapter/omp-reference.md#verified-against-the-installed-18111-artifact) distinguishes bundle control flow from live observations.
