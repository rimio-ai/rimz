# OMP adapter

> Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer. Accounts and balances are in [providers.md](./providers.md), spend and pricing in [spending.md](./spending.md), and the upstream contract in [omp-reference.md](../../externals/agent-adapter/omp-reference.md).

OMP (Oh My Pi) is a fork of Pi with its own executable, `omp`, and its own home, `~/.omp/agent`. Its adapter is an experimental built-in and shares nothing with Pi's: no home key, no extension file, no decoder. RimZ integrates through one TypeScript extension that it authors and installs whole, so the envelope schema is RimZ's, and drift between the extension and the decoder is a RimZ bug.

| File | Owns |
| --- | --- |
| [`crates/rimz/src/agents/adapters/omp/extension.ts`](../../../crates/rimz/src/agents/adapters/omp/extension.ts) | The in-process extension: handlers, envelope assembly, ordered helper delivery, launch reminders, response-header windows |
| [`crates/rimz/src/agents/adapters/omp/mod.rs`](../../../crates/rimz/src/agents/adapters/omp/mod.rs) | `OMP_DESCRIPTOR` (launch argv, tool sets, coverage), the `OMP_HOOKS` catalog, `OmpAdapter::decode_hook`, install through `OMP_SOURCE`, the relocation preflight, spend discovery |
| [`crates/rimz/src/agents/adapters/omp/payloads.rs`](../../../crates/rimz/src/agents/adapters/omp/payloads.rs) | Envelope fields, ask questions, the context observation and its rate-limit windows |
| [`crates/rimz/src/agents/adapters/omp/transcript.rs`](../../../crates/rimz/src/agents/adapters/omp/transcript.rs) | The visible conversation projection |
| [`crates/rimz/src/agents/adapters/omp/spend.rs`](../../../crates/rimz/src/agents/adapters/omp/spend.rs) | Main-session JSONL accounting |

## The extension

The extension never makes a tool decision. Every handler returns at once without awaiting RimZ, `tool_call` included, and no return value blocks or alters a tool. OMP treats a thrown or timed-out tool-call handler as a block ([upstream](../../externals/agent-adapter/omp-reference.md#lifecycle-and-blocking-decisions)), so a slow or missing `rimz` must cost OMP nothing. Two handlers return a value: `before_agent_start` returns the appended `systemPrompt` (see launch reminders below), and `session_shutdown` returns the delivery promise.

Delivery is serialized in event order. `feed` builds each envelope when its event fires, then chains the send onto one promise, so a helper starts only after the previous helper has closed. Without the chain, a slow registration helper could land after the turn start and reset a running card to idle. `session_shutdown` returns that promise, and OMP awaits the queued envelopes inside its two-second shutdown budget ([upstream](../../externals/agent-adapter/omp-reference.md#lifecycle-and-blocking-decisions)).

Each send spawns `$RIMZ_BIN` (default `rimz`) as `hooks feed --source omp` and writes the envelope to its stdin. The child gets fresh pipes for all three streams, and the extension drains stdout and stderr without reading them. `RIMZ_AGENT_PID` in the child's environment is the OMP process's own PID, which [hook ingress](./adapter.md#the-feed) checks against the agent. A spawn error or a broken pipe resolves the send silently.

Every handler returns early when `ctx.agent?.kind === "sub"`. Task children therefore feed nothing, and the adapter tracks no child rows (see [Known gaps](#known-gaps)).

After a final `agent_end` (one without `willContinue`), the extension drops `tool_execution_end` for `ask` and every `tool_approval_resolved`. Both decode to a `ToolUsed` that clears a wait, and a `ToolUsed` on a settled row would reopen the turn. The next `session_start` or `before_agent_start` clears the settled flag.

A headless run never raises Waiting. The extension forwards `tool_call` only for the `ask` tool with `ctx.hasUI`, and `tool_approval_requested` only with UI. The decoder applies the same gate to the envelope's `has_ui`.

A switch to a new session id ends the old session and registers the new one. The extension feeds `session_shutdown` with the previous id, then a `session_start` that carries only the session id and cwd. OMP restores model, effort, and context after `session_switch` fires, so the registration blanks those fields and the next envelope carries the restored values. A switch to the active id, such as the same-file resume that OMP's `reload()` emits, feeds nothing.

Launch reminders arrive in `RIMZ_LAUNCH_REMINDERS`, the `SystemTextChannel::ExtensionEnv` channel ([fleet.md](../harness/fleet.md#launch-reminders)). The extension reads the variable once per JS realm, keeps the value under a global symbol, and deletes the variable, so processes OMP spawns later do not inherit it. The main agent's `before_agent_start` returns `[...systemPrompt, reminders]`. RimZ does not use `--append-system-prompt` for reminders, because that flag replaces the user's discovered `APPEND_SYSTEM.md` ([upstream](../../externals/agent-adapter/omp-reference.md#extension-dispatch-and-decisions)).

## Hooks and lifecycle

`OMP_HOOKS` lists the 14 wired events, and `OmpAdapter::decode_hook` decodes them. The wire column gives RimZ's envelope name, and the extension renames native camelCase fields to snake_case.

| Native event | Wire event | Signal and fields |
| --- | --- | --- |
| `session_start` | `session_start` | `Registered`. Id and cwd come from `ctx.sessionManager` |
| `session_switch` to a different id | `session_shutdown`, then `session_start` | `Ended` for the old id, then `Registered` for the new one. A switch to the same id feeds nothing |
| `before_agent_start` | `before_agent_start` | `TurnStarted`. The prompt labels the row |
| `agent_end` | `agent_end` | `TurnEnded` unless `will_continue` is true. `stop_reason: aborted` gives `TurnInterrupted`, and `error` or a non-empty `error_message` gives an errored end. The final assistant text is the supervised result |
| `turn_end` | `turn_end` | Progress and a gauge refresh, no lifecycle signal |
| `message_update` | `message_update` | Progress when the gauge changed, at most once per second |
| `after_provider_response` | `after_provider_response` | Context only, including rate-limit windows |
| `tool_call` for `ask`, with UI | `tool_call` | `AwaitingInput { Question }`, keyed by `tool_call_id`. Questions and options come from `tool_input` |
| `tool_execution_end` | `tool_execution_end` | Named `ToolUsed { mutates, edits }`, keyed by `tool_call_id`. Completes an ask wait |
| `tool_approval_requested`, with UI | `tool_approval_requested` | `AwaitingInput { Permission }`, keyed by `tool_call_id`. The detail is the reason, else the tool name |
| `tool_approval_resolved` | `tool_approval_resolved` | Unnamed keyed `ToolUsed`, which clears the permission wait |
| `session_before_compact` | `session_before_compact` | `Compacting` |
| `session_compact` | `session_compact` | `CompactionEnded { failed: false }` after a committed compaction |
| `auto_compaction_end` with abort, error, or skip | `auto_compaction_end` | `CompactionEnded { failed: true, auto: Some(true) }` |
| `session_shutdown` | `session_shutdown` | `Ended` |

Every neutral reply is empty stdout. The catalog marks `tool_call` and `tool_approval_requested` as blocking because the native ask blocks OMP. That classification describes OMP's wait: RimZ never answers or denies through either hook.

`rimz coverage --wiring` prints 10 Wired, 3 Partial, and 6 Unsupported concerns from `OMP_DESCRIPTOR.coverage`. Turn lifecycle is wired, but the user-facing state capability is Partial: an `agent_end` without `willContinue` does not prove that OMP's background work has settled (see [Known gaps](#known-gaps)). No native event reports child agents. `lost` derives from the exec wrapper, as for every adapter. A pane follows its latest registered session (`FollowLatest`, [instances.md](./instances.md#same-pane-ownership)).

| Tool set | Tools | Feeds |
| --- | --- | --- |
| Editing | `edit`, `write`, `ast_edit` | `ToolUsed { edits }` |
| Mutating | the editing set, plus `bash`, `eval`, `debug`, `ida`, `github`, `lsp`, `rewind`, `learn`, `manage_skill`, `security_scan`, `task` | `ToolUsed { mutates }` |

Named `tool_execution_end` events feed the card's live tool counts. History tool counts come from `toolCall` blocks in the session files (see [Cost](#cost)).

## Install

`OMP_SOURCE` is a whole-file `ManagedSource` at `<HOME>/.omp/agent/extensions/rimz.ts` (`extension_path`). `HOME` comes from the login environment the caller passes, not from the installing process, and XDG variables do not move the file. A non-empty `RIMZ_OMP_EXTENSION` replaces the path, for tests. Install and preview refuse a file without the `_rimz_managed` first-line marker. Install replaces a marked file with the embedded source, byte for byte, and uninstall removes the file. The source is upgradeable, so a marked file that differs from the embedded source is an upgrade candidate. Restart OMP after a refresh, because OMP has no built-in `/reload` command ([upstream](../../externals/agent-adapter/omp-reference.md#extension-dispatch-and-decisions)).

The extension is wired only at the default home. `OmpAdapter::hooks_disabled` reports the first variable that relocates OMP, with an `unset <variable>` fix. A launch with the extension installed refuses on it, and `rimz doctor` shows the hook row as installed but disabled.

| Variable | Relocates OMP when |
| --- | --- |
| `OMP_PROFILE` | set to anything but empty, whitespace, or `default` |
| `PI_PROFILE` | `OMP_PROFILE` is unset and the value is anything but empty, whitespace, or `default` |
| `PI_CONFIG_DIR` | non-empty |
| `PI_CODING_AGENT_DIR` | non-empty |

An empty `OMP_PROFILE` still wins over `PI_PROFILE`, as in OMP itself ([upstream](../../externals/agent-adapter/omp-reference.md#paths-files-and-approval-flags)). The adapter declares no config-home override key: `config_home` reads `HOME` alone, so named RimZ accounts are unsupported. Following Pi's home key instead would risk overwriting Pi's own `rimz.ts`.

## Launch

| Launch | Argv |
| --- | --- |
| Fresh, with a prompt | `omp -- <prompt>`. The `--` keeps a prompt such as `config` from reading as a subcommand |
| Native picker | `omp --resume` |
| Resume a recorded session | `omp --resume <id>` |
| Fork a recorded session | `omp --fork <id>` |
| Ask and Plan | `--approval-mode always-ask` |
| Auto | `--approval-mode write` |
| Yolo | `--approval-mode yolo` |
| Model | `--model <model>` |
| Effort | `--thinking <level>` |
| System-prompt replacement | `--system-prompt <text>` |
| Manual compaction | `/compact [instructions]`, typed into the pane |

Importing an unrecorded session by id is unsupported. Plan maps to conservative approval because OMP has no plan permission mode: `--plan` selects a model role. A launch without a permission mode keeps OMP's native yolo default ([upstream](../../externals/agent-adapter/omp-reference.md#executable-and-launch-arguments)). `conformance.rs` pins the four permission rows against the [Permission modes table](../../reference/agent-support.md#permission-modes), so a change to one updates that table too.

The skill root is `<HOME>/.omp/agent/skills`, and OMP marks a skill user-only in its frontmatter (`ManualSkill::Frontmatter`). In scoped isolation, a profile's skill list applies through the scoped view, which rewrites unlisted skills as user-only ([sandbox.md](../sandbox.md#profile-skill-views)). On the host, OMP has no restriction switch (`HostSkills::Unsupported`), so the launch warns `HostSkillsUnenforced` and runs with every skill callable.

## Context and transcript

Every envelope except a switch registration can carry the model, the thinking level, the rounded context fill, window, and token count, the latest assistant input, output, cache-read, and cache-write split, and provider windows. `ctx.getContextUsage()` supplies the gauge. On a branch with no assistant message yet, the extension sends a missing fill or token count as 0, so a fresh session reads as empty instead of unknown. `payloads::context` folds these fields into one `ContextObservation`. There is no transcript-tail gauge, and the envelope carries no dollars (see [Cost](#cost)).

`transcript::parse_messages` keeps `message` entries with the role `user` or `assistant`. The title slot, the session header, tool results, and thinking blocks therefore never appear as conversation text. User text passes through the shared prompt classifier, `sanitize_user_prompt`.

## Account and balance

The adapter never opens `agent.db`, OMP's SQLite store, which holds plaintext credentials beside other agent state. Login identity, plan, and direct account usage are therefore unknown. The only account facts are rate-limit windows from provider response headers, which `after_provider_response` reads by lowercased name:

| Header prefix | Default window |
| --- | --- |
| `x-codex-primary`, `anthropic-ratelimit-unified-primary`, `anthropic-ratelimit-unified-5h`, `anthropic-ratelimit-unified-five-hour` | 300 minutes |
| `x-codex-secondary`, `anthropic-ratelimit-unified-secondary`, `anthropic-ratelimit-unified-7d`, `anthropic-ratelimit-unified-seven-day` | 10,080 minutes |

Each prefix is read from its `-used-percent`, `-reset-after-seconds`, and `-window-minutes` headers, and `-window-minutes` overrides the default window. A response with none of these headers keeps the previous windows. `payloads::context` marks every window `BestEffort`. A pane can switch its backing provider, so the windows are observations of whatever provider answered, not a verified account binding.

## Cost

`OmpAdapter::spending_sources` scans `<HOME>/.omp/agent/sessions` for `**/*.jsonl`, plus `$XDG_DATA_HOME/omp/sessions` when `XDG_DATA_HOME` is non-empty and its `omp` directory exists. `OmpAdapter::spending_session_id` accepts a file only when the stem's text after its last `_` is a UUID. That matches main-session names (`<timestamp>_<uuidv7>.jsonl`) and excludes task files, which `parse_spend` stalls on.

`spend::parse` counts assistant `message.usage` and standalone `model_usage` records across every branch appended to the file, not only the active window that OMP's own session stats sum ([upstream](../../externals/agent-adapter/omp-reference.md#model-context-and-accounting)). Compaction and branch-summary records carry no usage of their own. The rules per record:

- The session header, read from the first two lines because the title slot comes first, gives the session id and the cwd that attributes the project.
- A `totalTokens` figure above the four counted fields adds the excess to output.
- A non-negative finite `usage.cost.total` is the cost verbatim. It is OMP's accounting figure, not an invoice. Without it, the price book prices the split.
- `toolCall` content blocks count by name for history tool stats.
- A changed header id, or a file shorter than the cursor, reparses from the start with `SpendUpdate::Replace`. A truncation to an empty file replaces too.

Dollars arrive when the turn ends, through history reconciliation. `model_usage` never reaches an extension handler, so a live push of assistant-only totals would overwrite the fuller reconciled figure. That is why realtime cost is Partial. History is Partial because task children's spend is excluded.

## Known gaps

- Only the default agent directory is wired: relocated profiles and homes refuse, and named RimZ accounts are unsupported. Session-only relocation through `--session-dir` or `PI_CODING_AGENT_SESSION_DIR` passes the preflight but puts history outside the scanned roots. An `omp` launched from a named-Pi pane inherits `PI_CODING_AGENT_DIR` and refuses with the existing fix to unset that variable for OMP panes; RimZ does not strip another provider's exported home.
- Task children are filtered from every handler, so they have no rows and no spend. Whether a parent's records already include child usage is unverified, and counting both risks double-counting.
- An `agent_end` without `willContinue` ends the RimZ turn, but no extension settlement event proves complete RPC quiescence. Background work can outlive the displayed end. Background parking and background shells are unsupported, and there is no idle-timeout notification.
- A failed manual compaction emits no closing event. The card can stay compacting until later lifecycle activity. Automatic compaction's abort, error, and skip are covered.
- Plan review is unsupported. OMP opens a review from idle, and a keyed close of that wait folds the row to Running instead of Idle, so wiring it would reopen a settled turn.
- There is no live dollar push, no credential-store identity or plan, no direct account query, and no `rimz answer`. Answer native approvals and questions in the OMP pane.
- Only the prebuilt `omp` process is detected. A package install run under Bun is not, and declaring the generic `bun` name would collide with OpenCode detection.
- Raw `--no-extensions`, `--trusted-extension`, or `--profile` arguments can bypass the extension. The relocation preflight reads the environment, not argv.

## Verified against omp 18.11.1

The adapter is verified against the 18.11.1 prebuilt. An isolated live proof used disposable `HOME`, `RIMZ_HOME`, and XDG roots, a private tmux server, and a keyless local OpenAI-compatible SSE provider, and read no credentials or `agent.db`. `rimz hooks install omp` installed the 14-hook extension byte-identical to the embedded source, and `rimz doctor` reported `1 reporting to RimZ: omp`. Under `omp --model probe/probe -- <prompt>`, the prompts `hello`, `config`, and `-prefixed` reached `before_agent_start` unchanged. `/proc/<pid>/comm` read `omp`, and `RIMZ_AGENT_PID` matched the pane's OMP PID. The delivered order was `session_start` → `before_agent_start` → `tool_execution_end(read)` → a final `agent_end` without `will_continue`. A native logger also saw `tool_call(read)`, which the extension forwards only for `ask`, and an earlier `agent_end` with `willContinue=true`.

This proves prompt parsing, prebuilt identity, extension loading, and the exercised local lifecycle. It does not prove real-provider authentication or billing, live native asks or compaction, or Bun package launches. The [artifact evidence](../../externals/agent-adapter/omp-reference.md#verified-against-the-installed-18111-artifact) separates bundle control flow from live observations.

The repeatable guard is `crates/rimz/tests/integration/examples/omp_extension.rs::extension_orders_delivery_without_blocking_native_handlers`. It runs the extension under Node with a stubbed spawn, and asserts that handlers return at once, the exact delivery order, sub-agent and late-close suppression, the stripped switch registration, and the shutdown wait. It skips when `node` is not on `PATH`.
