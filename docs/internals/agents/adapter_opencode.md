# OpenCode adapter

This page maps OpenCode 2 onto RimZ's types: the plugin RimZ installs, the events it forwards, launch and resume, the context gauge, the transcript, the account, and cost. The provider-neutral agent model is in [model.md](./model.md) and the integration layer in [adapter.md](./adapter.md). Accounts and balances are in [providers.md](./providers.md), spend and pricing in [spending.md](./spending.md). The upstream protocol, pinned at OpenCode 2.0.27, is in [opencode-reference.md](../../externals/agent-adapter/opencode-reference.md).

RimZ supports OpenCode 2 only. `crates/rimz/src/agents/adapters/opencode/mod.rs::OpencodeAdapter` declares a 2.0.0 floor through `min_version`. RimZ authors the whole wire: an in-process TypeScript plugin flattens OpenCode's hooks and bus events into a snake_case envelope that RimZ owns. A payload drift is therefore a RimZ bug in the plugin, never an upstream one.

## Files

| File | Owns |
| --- | --- |
| `crates/rimz/src/agents/adapters/opencode/mod.rs` | The `static AgentSpec` (`OPENCODE_DESCRIPTOR`), the declared coverage, the hook catalog (`OPENCODE_HOOKS`), `decode_hook`, the managed install, native delegation denial, and `context_window_for` |
| `crates/rimz/src/agents/adapters/opencode/plugin.ts` | The plugin RimZ installs: activation, root and child classification, the event mapping, the gauge, launch reminders, and cleanup |
| `crates/rimz/src/agents/adapters/opencode/payloads.rs` | `OpencodeHookPayload`, the typed envelope, and its tolerant parse |
| `crates/rimz/src/agents/adapters/opencode/database.rs` | Database discovery, the logical stat, read-only SQLite access, and credential loading |
| `crates/rimz/src/agents/adapters/opencode/account.rs` | The OpenCode `Config` for the shared delegated-account probes |
| `crates/rimz/src/agents/adapters/opencode/go_usage.rs` | The Console [Go status](#go-status) read: the URL override's trust check, the wire, and the 5h, 7d, and month windows with their amounts |
| `crates/rimz/src/agents/adapters/opencode/transcript.rs` | Transcript reads, the assistant-page cursor, and the final message |
| `crates/rimz/src/agents/adapters/opencode/spend.rs` | The spend parser |

Only the registry reaches `OpencodeAdapter`. The modules above stay private to the adapter directory, except `go_usage`: it is visible to `crate::agents` so that `crates/rimz/src/agents/delegated_account.rs::probe_account_usage` can call it.

## The plugin

### Install

The plugin is a whole file that RimZ writes, through `ManagedSource` (`OPENCODE_MANAGED_SOURCE` in `mod.rs`). It installs at `RIMZ_OPENCODE_PLUGIN` when that is set and nonempty, otherwise at `<config>/opencode/plugin/rimz.ts`, where `<config>` is `XDG_CONFIG_HOME` or `~/.config` (`crates/rimz/src/agents/adapters/opencode/mod.rs::opencode_plugin_path`). The path resolves from the process environment, not the login environment. The file's first line carries the `_rimz_managed` marker, and ownership follows the marker:

- Install and reinstall replace a marked file byte for byte.
- Install and preview refuse to overwrite an unmarked file, which the user owns.
- Uninstall removes only a marked file.
- The source is declared upgradeable, so setup offers a consented refresh when the marked file differs from the copy embedded in the build.

`OpencodeManagedIntegration::install_blocker` refuses install and preview when the located `opencode` reports a version below the floor (see [Launch and resume](#launch-and-resume)).

### Activation

OpenCode calls the plugin's `setup(ctx)` from its default export, `{ id: "rimz", setup }`. Setup does nothing unless `RIMZ_WORKSPACE_ID` is nonempty and an executable `rimz` resolves (`RIMZ_BIN`, otherwise the first `rimz` on `PATH`). A second setup on the same context returns the same activation, from a `WeakMap` keyed by context. Each activation keeps its own session, child, model, agent, form, and gauge maps. It registers the session `prompt` and `context` hooks and the tool `execute.after` hook, and it consumes `ctx.event.subscribe`.

The plugin classifies each session as a root or a child. `session.created` classifies by its `parentID`. A session first seen through another event or hook is classified through `ctx.session.get({sessionID}).parentID`, which covers a plugin reload and events that arrive before the creation event. A failed lookup leaves the session unclassified, and the plugin drops its events instead of crediting them to the root. The plugin also drops events from another `location.directory` and events for the `global` session.

Each envelope goes to RimZ as one `rimz hooks feed --source opencode` child, which reads the JSON line on stdin. The plugin discards the child's stdout and stderr, so a RimZ reply never reaches OpenCode. Every envelope carries the session id, the session directory as `cwd` and `worktree_path`, and the remembered title, version, model, effort, display name, window, and token split. `crates/rimz/src/agents/adapters/opencode/payloads.rs::parse_payload` is tolerant: a field of the wrong type degrades the whole envelope to an empty record.

Cleanup runs when OpenCode disposes the activation. It aborts the subscription, sends `session_ended` with reason `dispose` for every root, disposes the hook registrations, and waits at most 1.5 seconds for the pending sends and the disposals to settle. Every handler checks for disposal again after each `await`. A handler that throws does not end the subscription. Pane liveness and the reaper cover a crash, where no cleanup runs ([instances.md](./instances.md#session-death)).

### Process identity

RimZ launches OpenCode with `--standalone`, so the TUI starts a private `opencode serve` child that inherits the pane environment, and the plugin runs in that child ([reference](../../externals/agent-adapter/opencode-reference.md#opencode-2-plugins-and-runtime)). The plugin sets `RIMZ_AGENT_PID` to its own `process.ppid` in each feed child, which names the TUI. `crates/rimz/src/cli/hooks/owner.rs::attach_agent_owner` stamps that pid, with its process-start token, as the session's `RuntimeOwner`, which feeds the reaper and every same-instance proof ([instances.md](./instances.md#session-death)). Before the first hook, a fresh RimZ launch records the exec wrapper as the owner (`crates/rimz/src/cli/agents_cmd/exec.rs::attach_own_launch_pane`), and the first hook replaces it with the TUI. A resume attaches the provider pid it spawned before any hook fires ([instances.md](./instances.md#binding-a-session), step 2).

The default shared OpenCode server is outside this contract. Its environment and parent belong to no pane, so its hooks name neither the pane nor the TUI. Process detection still recognizes both `opencode` and `bun` (`process_names`).

## Events

| OpenCode event or hook | Wire event | RimZ effect |
| --- | --- | --- |
| `session.created`, root | `session_created` | `Registered`, then a metadata refresh |
| `session.created`, child | `SubagentStart`, with `parent_session_id` and the title as `prompt` | `SubagentStarted` |
| `prompt` hook, root | `chat_message`, with `prompt.text` | `TurnStarted` |
| `session.execution.started`, root | none | marks the root and refreshes metadata |
| `session.execution.succeeded` or `interrupted`, root | `session_idle`, with `plan_proposed` under the `plan` agent | `TurnEnded`, or `AwaitingInput` for `PlanApproval` |
| `session.execution.failed`, root | `session_error`, with `error_class` and `error_message` | `TurnEnded`, errored |
| `session.execution.*` terminal, child | `SubagentStop`, `is_error` on failure | `SubagentStopped` |
| `session.step.started`, `ended`, `failed` | none | the gauge (see [Context gauge](#context-gauge)) |
| `session.model.selected` | none | the remembered model |
| `session.agent.selected` | none | the remembered agent |
| `session.renamed`, metadata refresh | `session_updated`, when the title changes | the session name, no lifecycle signal |
| `execute.after` tool hook, root | `tool_after`, with `tool_name` and `is_error` | `ToolUsed` |
| `session.compaction.started` | `session_compacting` | `Compacting` |
| `session.compaction.ended` or `failed` | `session_compacted`, `is_error` on failure | `CompactionEnded` |
| `permission.asked` | `permission_ask`, with `request_id`, the action as `tool_name`, the resources as `patterns` | `AwaitingInput` for `Permission` |
| `permission.replied` | `permission_replied`, with `reply` | a native answer and `ToolUsed` |
| `form.created` | `question_ask`, with one question per field | `AwaitingInput` for `Question` |
| `form.replied` | `question_replied`, answers in the form's field order | native answers and `ToolUsed` |
| `form.cancelled` | `question_rejected` | the native answer `(rejected)` and `ToolUsed` |
| `session.deleted`, root | `session_ended`, reason `deleted` | `Ended` |
| activation cleanup | `session_ended` per root, reason `dispose` | `Ended` |

`decode_hook` in `mod.rs` maps the wire event to the signal. The tool tables in `OPENCODE_DESCRIPTOR` count `bash`, `edit`, `write`, `apply_patch`, and `patch` as mutating, and all of them except `bash` as editing. On `session_idle` and `session_error`, `decode_hook` reads the session's last assistant message from the database as the final message (`crates/rimz/src/agents/adapters/opencode/transcript.rs::last_assistant_message`). The plugin sends no transcript path, so the observation names the newest database (`database::newest_file`).

### Sessions

Registration is lazy (`registers_lazily`). A fresh OpenCode TUI creates its session at the first prompt, so `session.created` and `session_created` arrive only then ([reference](../../externals/agent-adapter/opencode-reference.md#opencode-2-cli-and-environment)). Until then the pane renders as the idle row of [instances.md](./instances.md#before-a-session-binds).

`/new` registers a new root inside the same process. The adapter declares `SamePaneSessionPolicy::FollowLatest`, so pane and card ownership follow the latest root ([instances.md](./instances.md#same-pane-ownership)). The older root is collapsed by the in-place-switch rule of [supersession](./instances.md#supersession). The fresh-conversation rule never applies, because the wire does not say how a root was created and RimZ records no `Fresh` origin for it.

### Subagents

A child session reports only its start, its model, and its terminal execution. When a child's model first resolves or later changes, through a step or `session.model.selected`, the plugin sends `SubagentStart` again. The repeat sends no prompt, so the card keeps the original task and start time. The plugin drops a child's prompt, tool, permission, form, compaction, and rename events, and gives it no launch reminder, so a child cannot move the parent's state. A deleted child is forgotten without a `SubagentStop`.

### Asks and plan approval

Permission and form asks raise waiting cards that keep their structured questions. `decode_hook` takes at most four questions from a form. Each question's text comes from the field's description, then its title, then its key, and its header from the field's title. The adapter declares `native_ask_ui`, so attention goes to OpenCode's own dialog. The asks carry no native key (keyless, [model.md](./model.md#edges)). A reply event records the native answer and emits `ToolUsed`, which clears waiting ([model.md](./model.md#waiting-and-asks)). `rimz answer` is unsupported: RimZ has no mapped transport to answer OpenCode.

Plan approval is derived from the agent the plugin remembers. A root execution that succeeds or is interrupted while that agent is `plan` sends `session_idle` with `plan_proposed`, and `decode_hook` raises `PlanApproval`. The next prompt clears it. A launch in Plan mode selects no agent (see [Launch and resume](#launch-and-resume)), so only a plan agent the user picks in the TUI raises the gate.

## Launch and resume

`OPENCODE_DESCRIPTOR.launch` renders:

| Action | Argv |
| --- | --- |
| launch | `opencode --standalone [--auto] [--prompt <text>]` |
| resume | `opencode --session <id> --standalone` |
| compact | `/compact`, typed into the pane |

| Permission mode | Arguments |
| --- | --- |
| Ask, Auto | none: OpenCode's default |
| Plan | none: no agent is selected |
| Yolo | `--auto` |

Detection searches `PATH`, then `~/.opencode/bin`. Launch, install, setup, and `rimz doctor` refuse a binary whose readable version is below 2.0.0, with the fix "run `opencode upgrade --major`" (`crates/rimz/src/agents/version.rs::check_launch_version_floor`). A version RimZ cannot read does not refuse. Fork and every preset are unsupported (`PresetMatchers::EMPTY`), and a configured model preset refuses before launch.

OpenCode never carries this launch text:

- A profile system-prompt replacement or prompt file refuses with `PromptComposeErr::Unsupported`, because the adapter declares no system-prompt matcher (`crates/rimz/src/harness/prompt_compose.rs`).
- A headless request refuses, because `launch.headless` is `None`.
- A compaction instruction is dropped without notice, because `CompactInstruction::Unsupported` renders only `/compact`.

Launch reminders arrive through `SystemTextChannel::ExtensionEnv`, which sets `RIMZ_LAUNCH_REMINDERS` ([fleet.md](../harness/fleet.md#launch-reminders) owns the text). The first activation in the process claims the value into a `globalThis` symbol and deletes the variable, so tool children never inherit it. The `context` hook then appends `{type: "text", text}` to the mutable `system` array for root sessions only. The hook checks the session and nothing else: it has no request-kind check.

An OpenCode launch covered by the [native delegation policy](../harness/subagents.md#native-delegation-policy) calls `crates/rimz/src/agents/adapters/opencode/mod.rs::OpencodeAdapter::disable_native_subagent_env` from `crates/rimz/src/harness/launch.rs::compile_agent_process_with_extra_env`. Its base `OPENCODE_CONFIG_CONTENT` is the value the process would otherwise see: composed launch env over inherited login env. The adapter parses it as JSONC, preserves every key including `permissions`, and appends `"-opencode.tool.subagent"` last in `plugins`, creating the array when absent and not doubling a selector already last. It writes strict JSON into the final launch env. This removes the built-in tool rather than denying a permission, so neither `--auto` nor an agent-specific allow restores it ([reference](../../externals/agent-adapter/opencode-reference.md#opencode-2-native-requests)).

Absent content becomes `{"plugins":["-opencode.tool.subagent"]}` without a warning. Invalid JSONC, a non-object root, or non-array `plugins` gets that same fallback plus one `crates/rimz/src/harness/launch_plan.rs::LaunchPlanWarning::NativeSubagentEnv`, carried from the process compiler to the launch plan. The warning names `OPENCODE_CONFIG_CONTENT` and the problem, says the launch ignored the configured value, and gives both fixes: set a JSON object whose `plugins` is an array, or unset it. The exec wrapper prints and records it with the other launch warnings.

## Context gauge

The plugin's gauge is a per-session record of the model, its window, and the latest token split, stamped onto every envelope. `session.step.started` stores the step's model and agent under its `assistantMessageID`. `session.step.ended` or `failed` removes that pair and reads the step's tokens:

- Output is OpenCode's output plus its reasoning.
- The total is input, output, cache read, and cache write.
- A positive total replaces the whole split, zero-valued fields included. An all-zero split keeps the last measurement.

`ctx.model.list()` gives the display name and the window: a positive `limit.input`, otherwise a positive `limit.context`, otherwise none. One activation reads the catalog once, and reads it again after an empty or failed result. When an envelope carries no window, `crates/rimz/src/agents/adapters/opencode/mod.rs::context_window_for` supplies 1M for an id containing `[1m]` or a Claude id containing `1m`, 200k for any other Claude id, and nothing for other models. The gauge counts input-side tokens only ([model.md](./model.md#enrichment)).

`ctx.session.get` refreshes the title, agent, and model at registration, at each prompt, and at each root execution boundary. The refresh runs in the background, so a slow lookup never holds a terminal envelope. The plugin never stores OpenCode's placeholder titles, `New session - ...` and `Child session - ...`. The version comes from `session.created` alone, because session lookup returns none.

## Transcript

`crates/rimz/src/agents/adapters/opencode/transcript.rs::read_messages` reads `session_message` rows for one session id only, since every OpenCode agent on the machine shares the database. It takes a user row's `data.text` and the `text` entries of an assistant row's `data.content[]`. Reasoning, tool content, and other record types stay out. Full reads order by `time_created`, then `rowid`.

The assistant-page cursor follows `rowid` over completed assistant rows only: `time.completed`, `finish`, or `error` marks completion. The cursor never advances over a message that is still streaming, so the in-place update that completes it is still read. An imported row with an older timestamp but a higher `rowid` still counts as new. The cursor's identity includes the database path and the session id.

`rimz agents history` joins these messages with the session's spend rows. Spend timestamps have second precision, so turn attribution compares boundaries at that precision while displayed timestamps keep theirs. When two turns open in the same second, the later boundary owns that second.

## Account and balance

Both account probes run through `crates/rimz/src/agents/delegated_account.rs`, which OpenCode shares with Pi. `crates/rimz/src/agents/adapters/opencode/account.rs::config` supplies the OpenCode inputs: `database::load_credentials`, the used provider from `database::latest_message_provider`, `key` as the API-key type, and the account-key domain `rimz/opencode-oauth-account-key/v1`. Both probes ignore the login environment, and database discovery reads the process environment, so a named login does not change what they read ([providers.md](./providers.md#logins)).

`crates/rimz/src/agents/adapters/opencode/database.rs::credentials_in_file` reads the newest database's `credential` table, ordered by `active DESC, time_created DESC, id DESC`, and keeps the first row that parses for each `integration_id`. The credential stamp is the largest `time_updated`, in milliseconds, of the rows kept. Session writes therefore never invalidate the OAuth usage cache. A missing database or `credential` table reads as logged out. A database that exists but fails to read is unavailable.

The account probe and the usage probe select the credential differently:

| Step | Account probe (`select_display`) | Usage probe (`select_usage_entries`) |
| --- | --- | --- |
| 1 | the exact provider key of the latest assistant row, across databases | the same |
| 2 | for `opencode-go` without an exact credential, the `opencode` Console credential | the same |
| 3 | the first OAuth credential in provider-key order | the same |
| 4 | the first credential in provider-key order | none: no credentials |
| a non-OAuth pick | shown, with its metering | refused as API-key only |

The account probe maps `oauth` to metered, `key` to unmetered, and `external` to unknown metering. It labels the plan `<Provider> OAuth`, `API Key`, or `External`, except for the Console `opencode` OAuth credential, whose plan comes from the Go status reply. The selected credential's raw provider key becomes `sub_provider`. The usage probe also refuses an empty access token and an expired or undated one. For the `openai` scope, the account key is the credential's top-level `accountId`, then `metadata.accountID`. An empty top-level value masks the metadata. Every other case hashes the refresh token, or the access token, under the account-key domain. Console credentials also hash a non-empty `metadata.orgID` into that key, so an organization switch changes the cache owner without recording the organization id.

| OpenCode provider | Quota surface | Account scope |
| --- | --- | --- |
| `anthropic` | Claude OAuth usage | `anthropic` OAuth |
| `openai`, `openai-codex` | Codex OAuth usage | `openai` OAuth |
| `opencode` | Go status | `opencode` OAuth |
| any other | none | none |

`crates/rimz/src/agents/delegated_account.rs::quota_surface` owns the table. An API-key, external, or unsupported credential carries no OAuth scope ([providers.md](./providers.md#account-scope)). The plugin wires no response-header enrichment, so the out-of-band usage probe is the only source of the bars ([providers.md](./providers.md#the-out-of-band-probe)).

### Go status

`crates/rimz/src/agents/adapters/opencode/go_usage.rs::fetch_usage_with_token` reads `GET https://opencode.ai/console/api/go/status` with the Console bearer token and, when present and non-empty, `metadata.orgID` as `x-org-id`. It uses the shared bounded OAuth transport, never the Zen usage endpoint, and never refreshes or writes credentials. `RIMZ_OPENCODE_GO_USAGE_URL` may override the URL only on loopback or the official HTTPS origin. Diagnostics retain status or schema classifications, never the reply, token, or organization id.

The reply's `product` supplies the plan: `go` renders as `Go`, `go-plus` as `Go Plus`, and an unknown product is title-cased without being mapped to Go. `access.meters.fiveHour`, `week`, and `month` supply integer `usedMicroCents` and `limitMicroCents` amounts and reset timestamps. The stable durations are 300, 10,080, and 43,200 minutes, independent of the subscription month's calendar length. Week and month carry `fixed_period: true`, so their countdowns run even with no usage; `fiveHour` keeps sliding-window detection ([not-started windows](./providers.md#not-started-windows)). Percentages use the shared floor derivation, reaching 100 only at the cap. The sidebar normally draws `5h` and `7d`; a tighter month contributes a ceiling tick, or takes the second row as `mo` when spent ([budget bars](../../interface/sidebar.md#budget-bars)).

An absent or expired `fiveHour` is a full, not-started bar with no countdown; an expired meter retains its limit with zero used. Missing week or month meters, invalid amounts, zero limits, and invalid reset timestamps fail the reading, leaving the cache's last truth. A 404 or a successful reply without `access` means no Go subscription; 401 and 403 mean rejected credentials. These are settled `NoCredentials` outcomes, not a lapsed-plan alarm. Other HTTP, transport, and schema failures are `Failed`. Without a prior cached Go reading, a Console login without Go has no usage-sourced plan or known windows and may read `never read` in `rimz accounts`; an unchanged owner's last reading follows the shared cache retention rules.

Go readings, including their cached entries, use `ProviderAccountScope::sub_provider("opencode", "oauth")`. They are display-only: kind-wide capacity reads do not expose them to auto-continue, auto-redeem, launch gates, or loop budget policy. Pi gains no Go quota surface. A stored `opencode-go` credential still wins over the Console alias, including an API-key credential, whose Go quota is unsupported.

## Cost

`crates/rimz/src/agents/adapters/opencode/database.rs::files` finds one database per data directory. The data directory is `RIMZ_OPENCODE_DATA_DIR` (a comma-separated list) when set, otherwise `XDG_DATA_HOME/opencode`, otherwise `~/.local/share/opencode`. A directory's candidates are `opencode.db` and every `opencode-<channel>.db` whose channel holds only ASCII letters, digits, `_`, and `-`. The candidate with the newest logical stat wins, where the logical stat is the main file plus its optional WAL (`database::logical_stat`). Ties go to `opencode.db`, then to channel names in lexical order. Credentials and the hook's transcript path take the newest database across directories (`database::newest_file`). Every access is read-only. A WAL change invalidates the spend and live-cost caches, and a shared-memory file change does not.

`crates/rimz/src/agents/adapters/opencode/spend.rs::parse` reads `assistant` and `compaction` rows from `session_message`, joined to `session_v2.directory` for the origin. OpenCode updates rows in place, so every parse cold-folds the whole table and returns `SpendUpdate::Replace` for that database's cache set ([spending.md](./spending.md#the-incremental-cache)). Each row keeps its `session_id`. A row's figures follow these rules:

- An assistant row needs `model.id`, `model.providerID`, and a token object with a nonzero split, or it is dropped, whatever its cost.
- A compaction row needs tokens or a positive cost. It takes its own model, otherwise the session's latest assistant model, and the row's `time_created`. Compaction rows never reach the transcript or the used-provider choice.
- Output adds reasoning. A reported `total` above the itemized sum adds the difference to output.
- A positive stored `cost` is authoritative. A zero cost is priced from tokens, with reasoning at the output rate and both cache rates. The price lookup tries the model id, a few aliases, and each name prefixed with the provider (`crates/rimz/src/agents/adapters/opencode/spend.rs::model_candidates`).
- An unknown model keeps its tokens at zero dollars until a pricing refresh.
- Token, time, and cost fields accept numeric strings.
- Tool counts come from each assistant row's `tool` content entries, by `name`. Live tool counts come from `tool_after`.

The live card's dollar figure comes from the database alone, since the plugin pushes no cost. At each turn end, `crates/rimz/src/cli/hooks/lifecycle/context.rs::supplement_realtime_cost` sums the session's spend rows. It writes the sum only when it exceeds the card's stored cost, and it skips the read when the database's logical stat is unchanged. A correction downward in the database therefore leaves the card overstated.

The spend cache stamps one origin per database file, either an origin override from a live agent or the first parsed row's directory. One OpenCode database holds the sessions of every project, so workspace-scoped spend attributes all of them to that one directory.

## Known gaps

`rimz coverage --wiring` prints the declared integration grid.

- Native answering, background parking, background shells, and remote control are unsupported.
- Idle attention is partial: OpenCode has no idle notification, so RimZ reconstructs it from turn boundaries, native asks, and the stall window.
- Plan mode at launch, presets, fork, system-prompt replacement, and headless runs are unsupported.
- RimZ does not read `OPENCODE_DB`, and it skips channel databases whose names contain a `.`, which OpenCode allows ([reference](../../externals/agent-adapter/opencode-reference.md#opencode-2-storage-and-credentials)).
- Workspace-scoped spend reads one origin for the whole shared database.
- The reference does not record whether OpenCode runs the `context` hook for title or generate requests, so whether those requests carry the launch reminder is unverified.
- No shared language-server suppression is verified for OpenCode, so [shared servers](../lsp.md) run beside OpenCode's own.
