# Codex adapter

> Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer every adapter implements. Binding, same-pane ownership, and session death are in [instances.md](./instances.md); accounts and balances are in [providers.md](./providers.md), spend and pricing in [spending.md](./spending.md); the raw upstream protocol is in [codex-reference.md](../../externals/agent-adapter/codex-reference.md).

This page maps Codex onto RimZ's internal types: which hook carries which signal, how a Codex session launches, how rollout files feed the gauge, the turn markers, and spend, what the read-only `codex app-server` adds, and where the account figures come from. `CodexAdapter` implements every capability trait, and the `static` `CODEX_DESCRIPTOR` declares the launch facts. Codex has no statusline, so the live gauge comes from the rollout tail and the remaining session metadata from the app-server. The code lives in [`agents/adapters/codex/`](../../../crates/rimz/src/agents/adapters/codex/mod.rs):

| Module | Owns |
| --- | --- |
| `mod.rs` | the hook catalog `CODEX_HOOKS`, the one-of `CodexHook` payload, `decode_hook`, lifecycle mapping, `CODEX_DESCRIPTOR`, the capability impls, and `config_home` (the one Codex home rule) |
| `payloads.rs` | typed hook payloads and their one generic `parse` |
| `ask.rs` | ask questions, answers, and `rimz answer` key plans |
| `install.rs` | the `config.toml` hook merge, uninstall, installed-hook trust, the routine rules file, and `codex_config_path` |
| `project_trust.rs` | read-only `[projects]` directory trust for the launch preflight |
| `model_alias.rs` | model alias resolution against the account's catalog |
| `headless.rs` | the headless check form |
| `rollout.rs` | the rollout JSONL decoder: header, messages, usage, terminal records |
| `transcript.rs` | rollout lookup, the tail scan, the local context refresh, and turn-death confirmation |
| `session_index.rs` | `session_index.jsonl` thread names |
| `local_sessions.rs` | local session discovery over the rollout stores |
| `local_sessions/tests.rs`, `local_sessions/tests/pane.rs`, `local_sessions/tests/revisions.rs` | full-scan and per-pane discovery tests, archive moves, date coverage, filesystem work budgets, revision grouping, and the file-name grammar |
| `process.rs` | Codex CLI and daemon process classifiers |
| `app_server.rs`, `app_server/wire.rs`, `app_server/transport.rs` | the read-only app-server client, its response models, and the JSON-RPC transport |
| `app_server/daemon.rs` | the managed remote-control daemon lifecycle |
| `broker.rs` | the per-session warm app-server broker |
| `account.rs`, `oauth_usage.rs` | the account probe, window and credit normalization, and the direct OAuth usage probe |
| `spend.rs`, `spend/parse.rs` | the full-history spend parser |

## Hooks and lifecycle

Every hook runs `RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex`, and the neutral answer is empty stdout, so permission and question prompts stay in Codex's own UI. [`map_codex_lifecycle_signal`](../../../crates/rimz/src/agents/adapters/codex/mod.rs) turns each installed event into one [`LifecycleSignal`](../../../crates/rimz/src/agents/lifecycle.rs). The upstream payloads are in [codex-reference.md](../../externals/agent-adapter/codex-reference.md#hooks).

| Event | Matcher | Signal |
| --- | --- | --- |
| `SessionStart` | `startup\|resume\|clear\|compact` | `Registered`; source `compact` gives `CompactionEnded` with no trigger |
| `UserPromptSubmit` | none | `TurnStarted`; a root prompt's reply can carry the [Environment block](../harness/fleet.md#environment-at-prompt-submit) as `hookSpecificOutput.additionalContext` beside `hookEventName` |
| `SubagentStart` | `.*` | `SubagentStarted` |
| `SubagentStop` | `.*` | `SubagentStopped { errored }` |
| `Stop` | none | `AwaitingInput { PlanApproval }` when the rollout rests on a plan, otherwise `TurnEnded { errored, parked_on_background: false, turn_id }` |
| `Interrupt` | none | `TurnInterrupted { turn_id }` |
| `PermissionRequest` | `.*` | `AwaitingInput { Permission }` |
| `PreToolUse`, tool `request_user_input` | `.*` | `AwaitingInput { Question }` |
| `PreToolUse`, any other tool | `.*` | `ToolUsed` with no name, `mutates` and `edits` false |
| `PostToolUse` | `.*` | `ToolUsed` with the tool name, `mutates`, `edits`, and `turn_id` |
| `PostToolUse`, accepted `request_user_input_async` | `.*` | the same `ToolUsed`, plus one queued question per input question |
| `PreCompact` | `.*` | `Compacting` |
| `PostCompact` | `.*` | `CompactionEnded` with the auto or manual trigger |

A root `Registered` or `TurnStarted`, and the compaction close from `SessionStart`, also carry the rollout lineage ([session registration](#session-registration-and-launch-quirks)).

Codex's `SessionEnd` hook stays unwired, because upstream also fires it when the app-server unloads an idle thread, which does not prove the pane died. The [reaper](./instances.md#session-death) stamps the end once pane liveness proves the process gone.

Codex has no `Notification` hook and no background-task parking, so `parked_on_background` is always false. Background-shell tracking is unsupported too: `Stop` carries no task list.

### Tool events

`PostToolUse` carries the tool name, so the store appends every one and replay counts it into `tool_calls`. The unnamed `PreToolUse` signal is proof of work: it keeps the heartbeat current and is appended only under the rule in [model.md](./model.md#the-state-machine), so one call counts once. `apply_patch` sets `edits`.

The hooks are a progress signal, not an enforcement boundary. Upstream routes shell calls (reported as `Bash`), `apply_patch`, and MCP calls through `PreToolUse` and `PostToolUse`, while web search and some other tool paths bypass both. Those calls also fire both hooks when nested in a code-mode cell, carry their originating turn's id, and can complete after `Stop`. The [spend parser](#cost) reads tool calls from the rollout, so historical counts stay complete.

### Turn endings

On `Stop` and `SubagentStop`, `decode_hook` scans the rollout tail once with [`scan_transcript_tail`](../../../crates/rimz/src/agents/adapters/codex/transcript.rs). An explicit rollout error record, or an error flag in the stop payload, marks the signal errored, so a provider-killed turn never ends as success. The same scan decides whether `Stop` rests on a plan ([plan-approval marker](#plan-approval-marker)).

`Stop` forwards Codex's `turn_id`, as `Interrupt`, `PreToolUse`, and `PostToolUse` do. The rollup records the closed turn id on a turn end or interruption and ignores a trailing tool from that turn, so a late `PostToolUse` cannot reopen the rested root. A tool with a different or absent id still reconciles an idle or successful row to running; `UserPromptSubmit` remains id-less. `Interrupt` fires only for a root turn. A Codex version without that hook ignores the installed key, and the rollout's [interruption marker](#turn-interruption-marker) settles its turns.

Codex is the one adapter that declares `confirm_interrupt`, so `rimz message --interrupt` follows the event log for this `TurnInterrupted` after pressing Escape ([the interrupt key](../harness/messaging.md#the-interrupt-key) owns the sequence). It reads events, so a later `TurnStarted` that clears the snapshot's closed id cannot hide the stop. No event within the window means not confirmed: the turn may still be running, or the optional `Interrupt` hook may be untrusted. A confirmed stop is not a fresh turn. Codex replays pending steers but not text already committed to the running turn, an expanded question editor takes pasted text as an answer, and an Escape that the editor owns (Vim insert mode, history search) reaches the editor first.

### Asks and answers

| Ask | Opened by | Closed by | `rimz answer` |
| --- | --- | --- | --- |
| Permission | `PermissionRequest` | the agent's next activity ([waiting and asks](./model.md#waiting-and-asks)) | refused: "permission answers require the Codex pane" |
| Question | `PreToolUse` for `request_user_input`, with question ids, headings, labels, and descriptions parsed into options | `PostToolUse` for the same tool, carrying the id-keyed answers | one option per question: Down to the option, then Enter, which advances to the next question and submits after the last |
| Async question | an accepted `PostToolUse` for `request_user_input_async`; one ask per title, with optional string options | a `UserPromptSubmit` reply envelope that names the question; an ordinary prompt or a turn close clears all | refused with a pane pointer; answer in Codex's question editor with Shift+Left |
| Plan approval | `Stop` resting on a rollout plan | `UserPromptSubmit`, which records the submitted prompt (capped at 1000 characters) as the answer | the single option `implement`: Enter on the default "Yes, implement this plan" row, which also switches Codex from Plan mode to Default mode |

[`ask.rs`](../../../crates/rimz/src/agents/adapters/codex/ask.rs) drives blocking questions verified against Codex 0.144.3 and decodes async questions observed on 0.158.0. Multi-select questions, blocking free-text and "other" options, plan refinement text, and alternate plan actions stay in the pane. `update_plan` is a todo tracker and opens no ask. Leaving Plan mode with Shift+Tab without submitting a prompt leaves the plan ask open until the next hook.

Async questions ride the ordinary `ToolUsed` observation as an `ask_queue` edit, not as a waiting signal. The decoder accepts the hook's JSON-string response only when it reports `accepted: true`. Ingestion mints the AskIds and joins answered keys before folding, and transcript entries join each question by its exact AskId. A reply envelope attaches its answered keys to `TurnStarted` and stays out of the card prompt. Every other `UserPromptSubmit` clears the queue, keeping the prompt and task, with no version gate, and the clear adds no question or answer entry. The hook cannot tell the composer from the question editor, so a pasted reply envelope still counts as a partial answer. The `AskUserQuestion` profile switch does not disable the async tool, because upstream exposes no switch for it. Pane answering and the limits of delayed observation are in [the asks reference](../../reference/cli/asks.md#questions-while-codex-works).

Guardian reviews in Auto mode fire `PermissionRequest` before the automatic reviewer runs, so the card briefly shows permission waiting until the next activity hook. The payload does not identify the reviewer, and Guardian needs no `rimz answer`. Rule-allowed routine commands skip both the hook and Guardian. Guardian, `/review`, and memory-consolidation threads never appear as agents, and their spend still counts.

### Install

[`install.rs`](../../../crates/rimz/src/agents/adapters/codex/install.rs) rewrites Codex's `config.toml` in place. `codex_config_path` resolves the file the way Codex does: `RIMZ_CODEX_CONFIG` (a test and tooling override), then `$CODEX_HOME/config.toml`, then `~/.codex/config.toml`. It reads those keys from the login env its caller passes, so a named account's `CODEX_HOME` applies without touching the process env. Hook install, hook trust, room-root grants, and the [directory trust preflight](#directory-trust-preflight) all share it.

Install writes one inline `[[hooks.<Event>]]` table per catalog event, `Interrupt` included, each with exactly `type = "command"`, `command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex"`, and `timeout = 2`, and no `statusMessage` or `async` key. The group matcher is the one in the [event table](#hooks-and-lifecycle): `startup|resume|clear|compact` for `SessionStart`, absent for `UserPromptSubmit`, `Stop`, and `Interrupt`, and `.*` otherwise.

This definition is frozen, because changing a hashed field re-prompts every Codex account for trust and stalls unattended launches until the user reviews it. The golden `crates/rimz/src/agents/adapters/codex/tests/install.rs::installed_hook_hashes_are_frozen` pins all eleven hashes against Codex 0.163.0. To verify or re-pin it, install with the branch binary under an isolated home, ask `codex app-server` over stdio for `hooks/list` after `initialize` and `initialized`, and compare each `currentHash`. The real-Codex fixture test pins the mirror against older installed shapes independently.

TOML has no `_rimz_managed` marker, so install and uninstall reclaim RimZ's entries by the `rimz hooks feed --source codex` command substring and leave user hooks untouched. Uninstall also removes the `[hooks.rimz]` table that older RimZ builds wrote, which Codex ignores. A missing or empty file installs into an empty table, and invalid TOML fails with a parse error. Reinstall strips and appends the managed group, so user groups added after it can move its slot and require trust again.

### Hook trust

Codex gates each installed hook behind its own per-hash trust state and silently skips an untrusted one ([codex-reference.md](../../externals/agent-adapter/codex-reference.md#trust-state)). Only the user can trust it, through Codex's startup review or `/hooks`. RimZ neither writes trust nor bypasses the review.

`untrusted_installed_hooks` mirrors Codex's check:

- The `[hooks.state]` key is `<config-path>:<event>:<group>:<handler>`. Codex canonicalizes an explicit `CODEX_HOME` (the home, not the config file's symlink target), and its default `$HOME/.codex` stays lexical. The mirror accepts the account's lexical or canonical-home config path, never another account's keys in a shared file.
- An event is trusted only when a current RimZ handler's exact raw zero-based group and handler slot has a string `trusted_hash` equal to the hash of that on-disk handler, and `enabled` is not false. Invalid array elements still occupy slots.
- The hash input uses ordered maps for compact sorted JSON, independent of `serde_json` feature unification. An unsupported `additionalContextLimit` or a wrongly typed modelled field yields no hash, and so no trust.

It reports every installed but untrusted event, outdated trust included, to `rimz start` and `rimz doctor`. Hook-driven launch preflights (`untrusted_preflight_hooks`) require the same set minus `Interrupt`, which is `optional_for_preflight` because older Codex versions ignore the key and the rollout marker covers them.

Codex opts into `InstallationCapability::hook_trust_choice`: its startup review comes before any hook-dependent event and offers **Trust all and continue**. `ProviderLogin::health` still reports untrusted hooks, for account rows, doctor, account selection, and headless refusals. The CLI's `folder_trust::preflight_hooks` then decides by launch class ([the account check at launch](../../reference/cli/agents.md#the-account-check-at-launch) defines the classes):

| Launch | Untrusted Codex hooks |
| --- | --- |
| watched agent launch or relaunch | proceeds, with one stderr notice per account per command naming the trust choice |
| unattended launch | refuses with a short user-action fix |
| rebirth | skips the agent with a short user-run fix, using a team role's base agent profile or the provider kind, never a generated seat profile or a solo resume |

The Environment listing stays in the launch prompt when untrusted hooks fail `launch_plan::compile`'s check, and trusting later adds no duplicate prompt-submit listing.

### Routine command rules

`HookInstallOptions.allow_routine_rimz`, which the CLI supplies from machine config, controls a second managed artifact: `<config_home>/rules/rimz.rules`. `ManagedSource` owns that whole file under its first-line marker, and the shared routine-prefix list renders one Starlark `prefix_rule` per entry. Install with the option on writes the file. Install with it off, and uninstall, remove marked files only. Preview lists the file as its own new, update, or removal entry. An unmarked rules file is the user's own: it is not drift, and preview and install leave it alone.

Rules are applied before the hook config is written, so a rules failure cannot leave a partial hook install. Hook installation alone decides `installed`. `upgrade_available` also reports any current RimZ slot whose on-disk hash differs from its frozen definition, or cannot be computed, so an existing install receives the frozen definition through the usual consent prompt and diff. Rules drift counts too: a missing file with the option on, or a marked leftover with it off. `managed_artifacts_present` counts a marked leftover without hooks, so uninstall finds it. Codex's `rules` directory is a shared `Dir` home entry even for a standalone account, so Codex's own `default.rules` amendments share the same settings lifetime.

## Launch

`CODEX_DESCRIPTOR` renders launch intent into Codex argv. Every managed pane runs embedded through `--no-daemon`, and the initial prompt is positional, after `--`.

| Intent | Codex argv |
| --- | --- |
| Every launch, resume, pick, and fork | `--no-daemon -c tui.question_esc_back=false` |
| Resume a session | `codex resume <id> --no-daemon -c tui.question_esc_back=false` |
| Pick a session | `codex resume --no-daemon -c tui.question_esc_back=false`; binding waits for the first prompt's hooks |
| Fork a session | `codex fork <id> --no-daemon -c tui.question_esc_back=false` |
| Remote app-server | `--remote X` and `--remote=X` are refused at compile time with "remove --remote from the launch args" |
| Permission `ask` | `--ask-for-approval on-request -c approvals_reviewer="user"` |
| Permission `auto` | `--ask-for-approval on-request -c approvals_reviewer="auto_review"` |
| Permission `plan` | no flags |
| Permission `yolo` | `--dangerously-bypass-approvals-and-sandbox` |
| Room paths, every isolation and mode | the selection-dependent [room-root grant](#room-root-grants) |
| Model | `--model <model>` |
| Effort | `-c model_reasoning_effort=<effort>` |
| Fast | `-c service_tier=fast -c features.fast_mode=true`; core keeps the tier only with the feature on, and a user config can switch it off |
| Profile `auto-compact` | `-c model_auto_compact_token_limit=<tokens>`; under the default `total` scope Codex caps it at 90% of the context window, and `model_auto_compact_token_limit_scope = body_after_prefix` leaves it unclamped ([upstream](../../externals/agent-adapter/codex-reference.md#cli-config-overrides)) |
| System prompt replacement | `-c model_instructions_file=<path>`: the resolved base file, or a content-addressed composed file when fragments exist |
| Appended text, such as the subagent no-delegation reminder | `-c developer_instructions=<text>`, a developer-role message that leaves the user prompt unchanged |
| Native delegation disabled | for a `rimz subagents` child, or a launch with [callable `rimz-subagents`](../harness/fleet.md#launch-reminders) and an available catalog, removes earlier `agents.enabled` and `features.multi_agent_v2` config overrides and `--enable multi_agent_v2` (split or equals form), then appends `-c agents.enabled=false -c features.multi_agent_v2=false`; leaves `features.multi_agent` overrides alone |
| Host skill list | `-c skills.config` entries with `enabled=false` for each unlisted frontmatter name (directory name as fallback), which hides explicit invocation too; CLI skill overrides are replaced and Codex merges the user's file entries ([host mode](../sandbox.md#host-mode)) |
| Max turns | unsupported |
| Compaction | bare `/compact`; Codex accepts no summary brief, so the configured brief is never sent |

`--no-daemon` keeps the session inside the pane, so its hooks come from the in-pane process. It needs Codex 0.163.0 or newer (`MIN_MANAGED_LAUNCH`) for embedded sessions and Guardian review. The version check runs before the pane opens when the launch `PATH` is known, otherwise in the exec wrapper, and headless loop and workflow judges check the resolved binary before spawning. An unreadable version passes. `tui.question_esc_back=false` makes Escape interrupt the turn instead of collapsing an expanded question editor, and a later user override wins. Headless exec does not inherit these TUI args.

The descriptor also sets `registers_lazily`, `daemon_hooked_sessions`, `ThreadKey::PerFile`, the `KeepPrimary` same-pane policy, and process names `codex` and `node` (Codex commonly runs as a `node` bundle).

### Room-root grants

Codex keeps its own command sandbox at every isolation level, and RimZ only grants it the three room roots (`StatePaths.root`, `RuntimePaths.root`, `RuntimePaths.shared_root`) that `launch_plan::compile` carries in `LaunchReminders.permission_roots`. The process compiler calls `LaunchCapability::configure_room_permissions` for every launch with roots, Plan and mode-less launches included, passing the cwd, a linked checkout's main Git root, and the resolved isolation. Preflight and managed process compiles carry no roots and append nothing. The grant goes after the user's args. At host and scoped levels the adapter never selects a sandbox or changes network policy ([provider command sandboxes](../sandbox.md#provider-command-sandboxes)).

The grant depends on which permission form the launch selects. Codex has two: the `sandbox_mode` form (`read-only`, `workspace-write`, `danger-full-access`) and named permission profiles (`default_permissions`). The adapter takes the first rule that matches, reading valued arguments with `PresetArgMatcher`. A repeated flag or config key uses its last occurrence, and a config value parses as TOML with a bare-string fallback.

1. `--dangerously-bypass-approvals-and-sandbox` or `--yolo`: full access.
2. `--sandbox` or `-s`: that sandbox mode.
3. `-c`/`--config default_permissions=V`: profile `V`.
4. `-c`/`--config sandbox_mode=M`: sandbox mode `M`. Otherwise `--approve-for-me` or `--not-so-yolo`: sandbox mode `workspace-write`.
5. Config layers, in increasing precedence: the base config, the `$CODEX_HOME/<name>.config.toml` that `--profile`/`-p` names, then trusted `.codex/config.toml` layers from the nearest project-root marker down to the cwd, all read through `read_existing_table`. Deeper project layers win, and the provider home is never a project layer. Project trust checks the layer directory, the discovered project root, then the main Git root, canonical keys before lexical ones. In the winning layer, `default_permissions` selects a profile, otherwise `sandbox_mode` selects a sandbox mode.
6. No selection: sandbox mode, with Codex's implicit default.

Under sandbox isolation only, case 6 appends `--sandbox workspace-write` and then receives the `workspace-write` grant. Cases 1 to 5 stay as selected, full access and Yolo included, and RimZ adds no sandbox argument and no warning.

| Selection | Grant |
| --- | --- |
| Sandbox mode implicit or `workspace-write` | `-c sandbox_workspace_write.writable_roots=[<user roots>, <three room roots>]`; the user roots come from the last argument override, otherwise the highest-precedence config layer with that array, otherwise none |
| Sandbox mode `read-only` or `danger-full-access`; full access; an unknown mode | none |
| Profile `:workspace` | `--add-dir <root>` for each room root, in order |
| Profile `:read-only`, `:danger-full-access`, any other `:` name, or a name that is not a bare TOML key | none |
| User-defined profile `N` | `-c permissions.N.workspace_roots={"<root>"=true, ...}`; the profile's own workspace-root rules and network choice remain |

These are argv-level guarantees, and Codex validates the resulting permissions at startup. Their limits:

- RimZ does not read system config layers or managed requirements. A `workspace-write` grant replaces the roots those layers set, keeping only the argument, project, profile, or base array selected above. An explicit `:workspace` under a managed read-only fallback can still fail Codex's `--add-dir` startup check.
- A profile name that is not a bare TOML key (`[A-Za-z0-9_-]+`) gets no grant, because Codex splits a `-c` key on `.` without unquoting, so neither a dotted nor a quoted key reaches it.
- Obsolete selectors the user wrote stay user input, and RimZ does not migrate them.
- Resume and fork receive the same grants, which Codex treats as explicit overrides of the thread's saved permissions.

### Model catalog resolution

`crates/rimz/src/agents/capabilities.rs::LaunchCapability::resolve_model_alias` is the account-scoped seam, and its default abstains. Codex resolves only baked aliases whose name differs from their id, so a full id or an unknown name never reads or fetches a catalog. The exec wrapper calls it after selecting the room login and before compiling the process and binding its launch identity, and config load keeps the requested name. A machine pin takes precedence. A resume or fork resolves the alias afresh, except for a session with no `LaunchRecord` and no explicit model override, which replays the model it recorded and resolves afresh with a warning only when none is recorded (`crates/rimz/src/harness/launch_plan.rs::resolve_model`).

`crates/rimz/src/agents/adapters/codex/model_alias.rs::select` matches exact `gpt-<numeric-version>-<family>` ids, excluding hidden entries and suffixed variants, and orders version components numerically. Each candidate follows visible, present upgrade targets, with a cycle guard. The newest chain end that supports the requested effort wins, and skipping the newest or finding no supporting model returns a warning. Without an effort, the newest wins.

`crates/rimz/src/agents/adapters/codex/app_server.rs::CodexAppServer::model_catalog` pages through `nextCursor` with `includeHidden: true`. A fetch has one 2-second `Instant` budget across the [enrichment ladder](#app-server-enrichment) (broker, daemon, cold spawn), covering each transport open, handshake, and `model/list`. A failure at any point in an attempt, including a broker response that does not confirm the login, falls through to the next attempt while budget remains. The final error names the last attempt tried, even when the budget ran out before the next one, and is a bare timeout only when no attempt started.

The catalog cache is one versioned JSON file per login under the persistent shared cache. It stores the trimmed catalog, the fetch time, and the last catalog-derived alias targets. `RuntimePaths` mints both the cache and the lock path, and a per-login advisory lock encloses the read, fetch, resolution, and atomic durable publication. A catalog stays fresh for one hour, and an unreadable or wrong-version file counts as missing.

Resolution falls back from a fresh catalog to the cached catalog at any age, then to the baked id. Its warnings:

| Case | Warning |
| --- | --- |
| refresh failed, cached catalog younger than 24 hours | none |
| refresh failed or cooling down, cached catalog 24 hours or older | the catalog age and the real error |
| baked fallback | always, with the baked id and the reason |
| lock failure | the permissions fix |

These rules cover catalog refresh only, and effort-selection and cache-save warnings keep their own. A failed fetch stamps `failed_at` and `failed_error` after the fetch returns, and a success clears both. For 60 seconds after a failure, every alias on that login reuses the stored error (or `recent catalog fetch failed` for a version-1 file without one) with no fetch and no diagnostic record. For that reason the stored no-family error names no alias.

Each actual failed fetch, a fetched catalog with no selectable family included, returns `ModelAliasResolution::refresh_failure`. The exec wrapper writes the workspace-scoped [`model_catalog_refresh_failed`](../diagnostics.md#event-taxonomy) record before it reads the compiled launch outcome, so a later compile failure still leaves evidence. Only a catalog-derived target updates the remembered alias and returns a move, once under the lock, and a baked fallback never records a move. The adapter prints nothing and appends no assist. The capability also answers a cache-only membership query for configured pins, where no cached catalog means unknown, not absent.

### Directory trust preflight

An undecided launch directory stops Codex at its trust screen before the first prompt, so a supervised `rimz agents -p` run started there would sit in `Pending` until its deadline. [`preflight_agent`](../../../crates/rimz/src/launcher/supervised.rs) therefore checks Codex's `[projects]` trust for the resolved checkout after the hook preflight and before the run record, the store append, and any mux action. A `--worktree` launch has created its tree by then. A human interactive `rimz agents codex` launch only warns, because a human at the pane can answer the screen. An agent-started launch refuses.

[`trust_gap_at`](../../../crates/rimz/src/agents/adapters/codex/project_trust.rs) returns a `FolderTrust` decision without writing. An undecided result carries the target path, the canonical grant key, and the exact original and candidate text, or a repair instruction when no preview can be made. It follows [upstream's rules](../../externals/agent-adapter/codex-reference.md#project-directory-trust), which stay the authority:

- The first saved level wins, checking the launch cwd, the nearest `project_root_markers` ancestor (default `.git`), then the main repository root, each canonicalized before as written. `trusted` is Decided. `untrusted` is Restricted with the matching key. Restricted has no grant preview: Codex shows Folder access on every interactive launch, and RimZ never changes that saved decision.
- Without a saved level, a folder is Decided when neither the configured markers nor the default `.git` identify a project ancestor. A `.git` directory needs `HEAD`, and a `.git` file counts. Otherwise it is Undecided.
- The main repository root is the launch directory's own main checkout root, for `--cwd` too, without replacing the room pin. Before creating a worktree, the interactive launch preflight uses the main root that tree will belong to.
- Valid TOML with malformed trust values refuses with a repair instruction, even when another key would match. A missing or unreadable file normally fails the hook preflight first, and this check still reports it if the file changes between the two reads.

An Undecided message names both fixes: `rimz trust grant --agents codex`, or answering Codex's folder-trust prompt once at the grant key. A Restricted message says to pick Open restricted in the pane or trust the folder in Codex. What happens next depends on the launcher:

| Launcher | Undecided | Restricted | Check |
| --- | --- | --- | --- |
| human `rimz agents` command | warns | warns | `crates/rimz/src/launcher/trust.rs::preflight` |
| team command, resident loop | warns | refuses | `crates/rimz/src/launcher/trust.rs::preflight` |
| fresh agent-started launch | refuses, asking the user to grant or name a trusted directory to relaunch in | refuses, asking the user to trust the folder in Codex or relaunch from a trusted directory, since no pane opens | `crates/rimz/src/launcher/trust.rs::preflight` |
| supervised `rimz agents -p` launch, subagent resume | refuses | refuses | `agents::preflight_launch_dir`, `LaunchDirMode::Supervised` |
| headless loop or workflow judge | refuses | passes, because `codex exec` has no folder screen | `agents::preflight_launch_dir`, `LaunchDirMode::Headless` |

RimZ reads the one resolved config file, not Codex's merged layer stack. Provider passthrough `--cd`/`-C`, `-c` overrides, and launch-specific environment changes are not modeled.

The preview edits TOML through `toml_edit`, keeping existing comments and order, and sets only `projects.<key>.trust_level` to `trusted`. Its key is the main repository root when one is supplied, otherwise the nearest marker root. The neutral [`grant_folder_trust`](../../../crates/rimz/src/agents/folder_trust.rs) writer compares the current bytes with the preview's original before it publishes through the symlink-following provider-file writer. That is a freshness check before replacement, not a transaction against an upstream process writing at the same time. Detection never grants trust, and callers own consent.

### Headless form

`CodexHeadless` ([`codex/headless.rs`](../../../crates/rimz/src/agents/adapters/codex/headless.rs)) is the [headless form](./adapter.md#the-spec) that loop and workflow judges run. Its argv keeps the profile args except `--no-daemon`, then appends `-c features.hooks=false exec --json`, then `--output-schema <schema file>` unless the request's schema is open, then `-o <verdict file>` and `-- <prompt>`. Hooks are off, so the check fires no RimZ hook.

`read_result` reads two places:

- Usage comes from the `turn.completed` events on stdout, the last one winning, and lines that do not parse are skipped. Input tokens include cached input. The cost prices that split for the launch model, at the fast multiplier when the launch posture had fast on ([cost](#cost)). A model name that is not priceable, or has no price, leaves the cost unknown.
- The value is the verdict file `-o` wrote. A file that cannot be read or does not hold JSON is an `Other` error, and the usage read from stdout still lands.

The Codex form never reports `ProviderLimit`.

## Session registration and launch quirks

Codex registers its session lazily. A plain launch fires no hook, and the first prompt fires `SessionStart` and `UserPromptSubmit` together. Until then the pane is an instance with no session, and the sidebar shows its [idle placeholder row](./instances.md#before-a-session-binds). A `codex resume <id>` launch already knows its id, so the exec wrapper attaches the card before Codex registers ([binding](./instances.md#binding-a-session)).

`codex resume` reopens a session in place under the same id, while `codex fork` copies the conversation into a new id whose rollout header names the source in `forked_from_id` ([codex-reference.md](../../externals/agent-adapter/codex-reference.md#launch-prompt-resume-and-fork)). A resumed Codex session therefore continues its own card and never produces a second row.

Managed panes stamp their hooks like a standalone agent (observed on 0.159.2). A user-run `codex` or a phone-started background session may instead route its hooks through the shared per-user app-server, with the session cwd as working directory, the daemon's pid as `RIMZ_AGENT_PID`, and the daemon's environment. `rimz hooks feed` classifies that owner as a daemon, ignores the ambient workspace and pane pins, and recovers the room from the in-pane `codex` process at the same cwd ([adapter.md](./adapter.md#hooks-resolve-the-room-they-live-in)). Such a session stays unstamped until pane recovery binds it and re-owns its liveness to the in-pane CLI. An unbound daemon-owned session ages out through the ghost TTL, with `thread/loaded/list` as the faster keep-or-drop signal ([session death](./instances.md#session-death)). Hooks from app-servers RimZ spawns itself carry `RIMZ_CODEX_INTERNAL_APP_SERVER` and are dropped, so enrichment never feeds back into hooks.

Several roots can share one Codex pane, and the rollout header tells them apart. On a root identity event, [`session_origin`](../../../crates/rimz/src/agents/adapters/codex/transcript.rs) reads the root's `session_meta`:

| Rollout header or hook source | Meaning | Rule in instances.md |
| --- | --- | --- |
| `forked_from_id` null | a fresh `/clear` or `/new` conversation (Codex also fires `SessionStart` for it) | [fresh conversation](./instances.md#supersession) retires the rested predecessor |
| `forked_from_id` set, continuation of a compaction | the same conversation under a new id; RimZ stamps `compacted_from` | [compaction continuation](./instances.md#supersession), and the successor inherits `registered_at` |
| `forked_from_id` set, anything else | a persistent fork (`/fork`, `codex fork`) or a provider-death retry, which Codex does not distinguish | [forked retry](./instances.md#supersession) retires only an errored predecessor; otherwise [`KeepPrimary`](./instances.md#same-pane-ownership) decides |
| `SessionStart` source `fork`, null payload `transcript_path`, no resolved rollout | an ephemeral side conversation (`/side`, `/btw`) | `decode_hook` marks it `SideConversation`, which ingestion quarantines: never an agent session, and its hooks advance the [host card's clocks](./instances.md#same-pane-ownership) |
| unreadable | lineage unknown | no lineage rule applies |

The side-conversation row has a gap. Codex matches the `SessionStart` matcher against `source`, and the installed matcher omits `fork`, so an installed RimZ hook never receives that event. The side thread's later hooks carry no source, so `decode_hook` cannot mark them.

Every same-instance root carries the [launch identity](./instances.md#launch-identity-across-conversations), and launched children keep their parent link across forks and continuations ([subagents.md](../harness/subagents.md#launch-generations-and-parentage)). On the first prompt after `/clear`, daemon-routed recovery stamps the new root onto the focused Codex pane, or, without focus evidence, onto the sole occupied pane whose owner is at rest, so the fresh-conversation rule can fire.

### Local session discovery

Codex declares `local_session_discovery = PerPane`. [`local_sessions.rs`](../../../crates/rimz/src/agents/adapters/codex/local_sessions.rs) reads the first `session_meta` line of demanded rollouts under `$CODEX_HOME`, or `~/.codex`. The room host batches admitted Codex workspaces and publishes the observations, and consumers never enumerate the rollout store ([local session observations](./instances.md#local-session-observations)).

| Rule | Value |
| --- | --- |
| Named rollouts | validated hook path, remembered path, then the bounded newest-16-day-directory resolver and flat archive; a remembered path no hook named is resolved again every 30 seconds and gives way to a newer revision; no age or count cap on a found path |
| Active window | `sessions/YYYY/MM/DD/`, from the window clock's UTC date minus one through today's UTC date plus one |
| Archived window | flat `archived_sessions/` files filtered by filename date to the same range; per session the newest revision by filename wins, and an active file wins only over its archived twin of the same name |
| Window cap | newest 512 sessions by creation per admitted workspace; named sessions do not consume the cap |
| Rescan | cold start, topology change, window change, UTC date rollover, or the 30-second backstop; unchanged stamps reuse the cached header |
| Missing named id | retry the bounded resolver at most every 30 seconds; non-UUID ids are never looked up |
| Admission | header `cwd` exactly equals an admitted workspace |
| Excluded | Codex's `is_non_root_agent` (a `source` object with any `subagent` or `internal` key), a non-empty `parent_thread_id`, or a trimmed, case-insensitive `thread_source = "subagent"`; `forked_from_id` alone does not exclude |
| Session id | header `id`, else the filename's thread UUID (revision suffix dropped) |
| Clocks | created at the header timestamp; last activity at the file mtime, never earlier than creation, and at creation when the mtime cannot be read |
| Projection | `IdentityOnly` |

Codex writes day folders and filenames in local time. Including both adjacent UTC dates covers the writer's local date around midnight. The full scan used by `rimz agents resume` keeps its 512-file bound and today-back-14-days cutoff, adding tomorrow's UTC date too. It keeps one rollout per session, newest filename first, grouping revisions by the filename's thread id before reading headers; the header's session id remains authoritative. An active rollout wins only over its archived twin of the same filename, not over a newer archived revision. Named rollouts moving between the active tree and the archive are retried at their computed twin path on the next pass, without a tree walk. Hints outside the account home or naming another id are ignored, including traversal paths. Paths found through the window are remembered for named ids. Warm passes stat only named files, window candidates and watched folders, with no listings or transcript reads. The per-pane cache is keyed by account home. Learned headers survive window closes and workspace changes; they are pruned only when a fresh listing of their folder no longer contains the file, or the account cache expires after 30 seconds unused. The archive listing preserves learned headers outside the current date window too.

The cache never reads the store, so a cwd match alone is not identity proof. The snapshot fold applies the [pane reservation and incarnation-clock rules](./instances.md#local-session-observations), which admit the newest launch owner's own session on its matched live pane without a clock.

## Subagents

A child hook needs a non-empty `agent_id` that differs from the root `session_id`, and `resolve_subagent_identity` quarantines a malformed `SubagentStart` or `SubagentStop` instead of folding it onto the parent. Codex also puts the child's `agent_id` on `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`, `PostToolUse`, `PreCompact`, and `PostCompact`, so those signals update the child row and its heartbeat ([model.md](./model.md#subagents)). The hook-reported root stays `parent_agent_id` for every descendant. The rollout's `parent_thread_id` supplies immediate-parent metadata and excludes the thread from root discovery.

Child hooks never merge into the root's context or spawn the app-server refresh, so the root session id and rollout cannot contaminate the child. The child's rollout is `agent_transcript_path`, else `transcript_path` when its header id matches the child, else a search by the child's `agent_id`, never by the root id. When the header marks a non-root thread, it supplies the display metadata:

| Card field | Source | Fallback |
| --- | --- | --- |
| name | header `agent_nickname` | hook `agent_type` |
| task | header `agent_path`, with the leading `root` segment removed | hook `agent_type` |
| role | header `agent_role` | hook `agent_type` |

Descendants stay flat under the root card in creation order, and the relative path shows the nesting. A supervised child that RimZ launches gets the native delegation denial and reminder args from [Launch](#launch).

## Context and transcript

Codex writes each persistent thread's rollout at `sessions/YYYY/MM/DD/rollout-<timestamp>-<session_id>[_<rollout_id>].jsonl` under `$CODEX_HOME` (default `~/.codex`), where the optional rollout id names a later revision of a reverted thread, and moves archived rollouts into the flat `archived_sessions/`. A root hook uses its `transcript_path`. Otherwise [`find_session_transcript`](../../../crates/rimz/src/agents/adapters/codex/transcript.rs) takes the newest matching file, walking back up to 16 day directories and then the archive. `RIMZ_CODEX_SESSIONS` replaces the sessions directory RimZ searches, and the archive is searched only when that directory is named `sessions`. The header feeds lineage and child metadata. The tail, read under the shared [reading rules](./adapter.md#reading-rules), feeds usage and the resting-turn markers.

| Rollout record | Field | Internal |
| --- | --- | --- |
| `token_count` | `info.last_token_usage.input_tokens` | current context tokens |
| `token_count` | `info.model_context_window` | context window; 272,000 (`DEFAULT_CONTEXT_WINDOW`) for a whole-file read with no usage yet |
| `token_count` | `info.last_token_usage.total_tokens` | `total_tokens` for a child |
| `token_count` | `info.last_token_usage.cached_input_tokens` | `cache_read_input_tokens` (the card's `◌`) |
| `token_count` | `info.last_token_usage.cache_write_input_tokens` | `cache_write_input_tokens` (the card's `◍`) |
| `token_count` | `input_tokens` minus `cached_input_tokens` minus `cache_write_input_tokens` | `fresh_input_tokens` (the card's `↘`); `input_tokens` includes both cache slices |
| `token_count` | `info.last_token_usage.output_tokens` | `output_tokens` (the card's `↗`) |
| `token_count` | every event, through the [spend parser](#cost) | cumulative `session_usage` and `cost` |
| `turn_context` | `model`, `effort` | model and live reasoning effort |
| `event_msg` `thread_settings_applied` | `thread_settings.service_tier` | fast pricing for following token events ([cost](#cost)) |

The adapter emits raw tokens and the window, and the snapshot fold derives the gauge percentage. A whole-file read with no `token_count` yet reports zeros against the fallback window. A bounded tail without a `token_count` supplies no token or window reading, which keeps the prior values under the shared [reading rules](./adapter.md#reading-rules). For a child, `total_token_usage` includes the copied parent history, so it never becomes child lifetime usage or spend. The model falls back from the rollout to `model` in `config.toml`.

`parse_transcript_messages` is the one conversation parser. It keeps `user_message` and `agent_message` events and completed user and agent message items, with their timestamps.

### Local refresh

[`refresh_transcript_context`](../../../crates/rimz/src/agents/adapters/codex/transcript.rs) runs inline and stat-gated: it reads the tail only when the rollout's stat changed, or when a live spend fold still needs token-counter backfill. It resumes the spend fold from the byte cursor on the context sidecar, so later refreshes price only appended events. An event whose model has no cached price adds zero dollars, and the live fold never reprices it. Repricing belongs to the history walk ([spending.md](./spending.md#token-pricing)).

The same pass takes the newest name for the session from `$CODEX_HOME/session_index.jsonl` into `session_name`, ahead of the first-message preview. The [index](../../../crates/rimz/src/agents/adapters/codex/session_index.rs) is parsed once per file version into a process-wide id-to-name map keyed by its full stat identity (length, mtime, device, inode), shared by account homes that link to one file, so an unchanged index costs one `stat` per lookup.

Four callers run the refresh, and all of them write the runtime sidecar and wake the sidebar without touching the durable store:

- `rimz hooks feed` on `SessionStart`, `UserPromptSubmit`, `PostToolUse`, and `Stop`, after the decision is written
- the hidden `rimz agents refresh-context` helper, before its app-server work
- the host renderer's transcript watcher, on rollout growth ([state.md](../sidebar/state.md#push-channels))
- the host fetch worker, as a tick backstop for live root rows

### App-server enrichment

The official `codex app-server` supplies what the rollout cannot: rate-limit windows, account plan and credits, the model display name, the thread name and preview, and the Codex version. [`app_server.rs`](../../../crates/rimz/src/agents/adapters/codex/app_server.rs) speaks only read-only methods: `initialize`, `account/rateLimits/read`, `model/list`, `thread/read` (with `thread/list` as the name and preview fallback), and `thread/loaded/list`. It never calls `thread/resume` or `turn/start`, which would take over the user's live thread. Token usage appears only on a subscribing `thread/resume` notification, which is why the rollout owns the gauge. `model/list`'s `defaultReasoningEffort` is a catalog default, so it never fills the row's effort. The method schemas are in [codex-reference.md](../../externals/agent-adapter/codex-reference.md#app-server-api).

Hooks never call the app-server inline. `SessionStart`, `UserPromptSubmit`, and `Stop` spawn `rimz agents refresh-context` detached with null stdio, as an explicit `rimz agents refresh` does. Periodic session refresh reads only local context and spawns no helper. The helper does the local refresh, then reads the app-server only when `app_server_due` finds `rate_limits_observed_at` older than 20 seconds. Every failure leaves its field unset. Account budget windows refresh separately, through the per-login usage lane ([account and balance](#account-and-balance)).

Enrichment connects to the warmest server available, with a 2-second budget per warm attempt and 6 seconds for a cold spawn, handshake and reads included. Enrichment and the model catalog share this ladder:

1. The per-session [broker](../../../crates/rimz/src/agents/adapters/codex/broker.rs), for the room-default login only.
2. The per-user remote-control daemon's WebSocket control socket, at `RIMZ_CODEX_APP_SERVER_SOCK` or `$CODEX_HOME/app-server-control/app-server-control.sock`. An empty `RIMZ_CODEX_APP_SERVER_SOCK` skips it.
3. A cold-spawned `codex app-server`, so headless use still enriches.

The broker runs as `rimz codex app-server serve` in the `rimzd` tab and holds one handshaked `codex app-server` behind a unix socket. Every request, `initialize` included, first re-resolves the login and respawns the child when its home or `auth.json` stamp moved, and a resolver error keeps the child. `initialize` is then answered from cache while the child is live, and a dead child respawns before answering or returns the spawn error. A closed stream respawns and retries once.

The client accepts the broker only when it proves it serves the login. Every successful broker response frame carries a top-level `rimzServedBy` member, `{codexHome, authStamp}`, naming the canonical home and `auth.json` stamp of the child that produced it. The client accepts the attempt only when every response names the login's own canonical home and stamp (`ServedBy`). A missing or malformed member, another home, or another stamp is unconfirmed, and the whole attempt is discarded: none of its reads reach the observation, and the ladder goes on to the daemon, then the cold spawn. A broker binary that never adds the member is therefore skipped after one `initialize`. The broker reports and the client decides, and the check never compares plans.

The daemon socket is the login's own, under its Codex home, and the cold spawn runs under the login's env, so neither needs the confirmation. `RIMZ_CODEX_APP_SERVER_SOCK` is an explicit user override naming a socket tied to no login, which the account-link gate ignores and this ladder honors.

`RIMZ_CODEX_BIN` names the codex binary for every codex process RimZ starts on its own behalf: the broker's app-server, the cold spawn, and the `codex login status` probe (`crates/rimz/src/agents/adapters/codex/app_server/transport.rs::codex_bin`). Set and non-empty, it is the binary, with no fallback to `codex`. Unset or empty, the binary is the first `codex` on `PATH`. Codex counts as installed only when that name resolves to an executable (`installed_codex_bin`), and without it the room opens no broker pane, at birth or at repair. Agent launch, hook install, setup, and doctor do not read the variable: they find the `codex` a user launches through `PATH`.

### Resting-turn markers

The tail scan also classifies how the newest turn rests, which settles rows whose closing hook never came. `scan_transcript_tail` walks the tail newest first and returns a [`RestingTurnOutcome`](../../../crates/rimz/src/agents/adapters/codex/transcript.rs). A live record (`turn_context`, `agent_message`, `task_started`, or `user_message`) proves recovery and ends the scan with no outcome, and `token_count` is ignored. The local refresh writes the outcome into the context sidecar as `AgentContext.settle` or `AgentContext.turn_error`, with the record's timestamp, and clears both when the tail proves recovery. The projection uses a marker only while it postdates `last_activity` ([displayed status](./model.md#displayed-status)), so the next prompt retires it.

| Tail rests on | Outcome | Marker |
| --- | --- | --- |
| a clean `task_complete` whose turn has a completed non-empty `Plan` item | `PlanProposed` | [plan approval](#plan-approval-marker) |
| an error record | `Died` | [turn death](#turn-death-marker) |
| `task_complete` with no error field and an empty or missing `last_agent_message` | `Died`, class unknown | [turn death](#turn-death-marker) |
| `task_complete` with no error field and a non-empty `last_agent_message` | `Complete` | [turn completion](#turn-completion-marker) |
| `turn_aborted` without an error | `Interrupted` | [turn interruption](#turn-interruption-marker) |
| `task_complete` with an empty error field (`null`, `false`, `""`, `{}`) | none | none |

The stat gate skips an unchanged rollout, so a marker lands only after the rollout's next write.

#### Plan-approval marker

Codex Plan mode ends a planning turn with an `item_completed` record whose item is a non-empty `Plan`, followed by a clean `task_complete` for the same `turn_id`. The plan body is not in `last_agent_message`. On `Stop`, that shape replaces `TurnEnded` with `AwaitingInput { PlanApproval }`, so the ask feed records the plan and a supervised `-p` run waits instead of completing with an empty result.

When `Stop` was missed, the local refresh stamps `settle` as `PlanProposed`. A `running` row then displays `waiting`, which keeps message delivery away from the plan prompt. The marker creates no durable ask.

#### Turn-death marker

Codex writes most provider errors to the rollout. An error record is a `turn_error`, `stream_error`, or `error` event, or a `task_complete` or `turn_aborted` that carries an error. RimZ caps its label at 80 characters and classifies it, the Codex error kind deciding first, in either the app-server camelCase or the rollout snake_case spelling.

| Codex error kind | Class |
| --- | --- |
| `usage_limit_exceeded`, `rate_limit_exceeded` | `PausedRateLimit` |
| `server_overloaded`, `internal_server_error`, `flex_unavailable` | `PausedOverloaded` |
| `context_window_exceeded`, `unauthorized`, `bad_request`, `sandbox_error`, `cyber_policy`, `thread_rollback_failed`, `bio_policy`, `invalid_prompt`, `too_many_denials`, `misalignment_policy_violation`, `session_budget_exceeded` | `Failed` |
| `other`, unknown, or absent | the label, through [`TurnErrorClass::classify_label`](../../../crates/rimz/src/agents/context.rs) |

`classify_label` first keeps a `reached your specified … usage limits` cap terminal as `Failed`, then maps spend-limit text to `PausedSpendLimit`, rate-limit and quota text or HTTP 429 to `PausedRateLimit`, overload, server-error, timeout, and connection text or HTTP 5xx to `PausedOverloaded`, and everything else to `Failed`. On `Stop` the same record marks the turn errored, and the projection shows the row paused or failed by class.

A `task_complete` with no final message is the silent capacity kill: Codex can end a turn on a capacity or usage limit with no error record and no `Stop`, showing the warning only in the TUI. The local refresh records it as class unknown with the label `turn ended with no final message`, and two confirmation steps try to name it:

1. [`confirm_codex_turn_death_from_pane`](../../../crates/rimz/src/sidebar/refresh/sessions.rs) captures the last 60 lines of a bound live pane and scans the frame above the `›` input prompt from the bottom up. A line counts when it is a `⚠` banner or its label classifies as a limit, so ordinary agent output that mentions a server error cannot pass as a provider warning. The nearest match supplies the label and class, and an unrecognized `⚠` banner lands as `Failed` with its upstream text. Continuation lines join the label until a blank line, the prompt, or a line that starts with no letter or digit.
2. Without pane proof, `infer_turn_death_from_spent_window` reads the account's fused windows and sets `PausedRateLimit`, labelled `usage limit inferred (rate-limit window spent)`, only when a window is spent with a future reset.

The producer retries these steps each tick while the marker is unconfirmed and younger than 10 minutes (`CODEX_TURN_DEATH_RETRY_WINDOW`), and `rimz agents refresh @codex` runs them on demand. An explicit error record never needs them.

#### Turn-completion marker

Codex `/review` runs in review mode and ends on a clean `task_complete` without firing `Stop`, so the lifecycle row stays `running`. A clean `task_complete` has no error field and a non-empty `last_agent_message`. The local refresh stamps `settle` as `Complete`, and the projection shows the row as `success`.

#### Turn-interruption marker

Codex writes `turn_aborted` when Escape aborts a turn and when `/clear` interrupts one. Without an error, RimZ accepts any `reason`: the local refresh stamps `settle` as `Interrupted`, and the projection shows the row as `idle`. An abort that carries an error is a turn death instead, classified by its error kind with the message kept as its label. The Guardian denial-limit stop takes that path, not a user's Escape. This marker is the fallback for Codex versions without the [`Interrupt` hook](#turn-endings). A steer that replaces the turn writes new live records after the abort, so it never settles.

## Remote control

RimZ-launched panes run embedded and never join the daemon or appear in the mobile app. The remote-control toggle serves sessions started from the phone or outside RimZ, and the Codex `⇅ rc` badge reports that daemon's liveness, not remote access to managed panes.

[`app_server/daemon.rs`](../../../crates/rimz/src/agents/adapters/codex/app_server/daemon.rs) manages the per-user daemon that `codex remote-control start` runs. Room birth and runtime toggles call it through `readiness`, `ensure`, and `reconcile`, and the provider-neutral remote-control module coordinates the result with the room and sidebar. Provider commands run from the durable `CODEX_HOME`.

- `readiness` resolves a managed binary under `$CODEX_HOME/packages`: `app-server-daemon/current/bin/codex`, then `app-server-daemon/current/codex`, then the same two paths under `standalone`. A missing install returns install guidance (`curl -fsSL https://chatgpt.com/codex/install.sh | sh`).
- `ensure` recovers a stale updater, then starts the daemon. Recovery pairs the app and updater records within one namespace: the dedicated `daemon.pid` and `daemon-updater.pid`, or the older `app-server.pid` and `app-server-updater.pid`. It signals the updater only when identity, ownership, managed executable, updater argv (optionally with `--restore-release <release>`), and sole zombie child all match the known upstream stale shape. On Linux it evaluates a recognised `processIdentity` (alias `linuxProcessIdentity`) by boot id and start ticks, and an evaluated mismatch is final. An absent, unfamiliar, unreadable, or non-Linux identity falls back to the recorded `ps lstart` text.
- `reconcile` retries once, after a stale recovery or after a 3-second settle for a failed start, which absorbs the upstream stop-then-start teardown race.
- `updater_skew` is a `rimz doctor` advisory. It fires only when the control socket exists, the updater pid still matches its recorded identity and uid, and its executable is deleted or differs from its namespace's managed release. Comparing the package root plus `releases/<release>` avoids false skew between a macOS launcher and its bundle image, and other paths compare in full. The message says the updater's next hourly pass converges on its own, and prints the `app-server daemon bootstrap --remote-control` repair for that namespace's resolved managed binary, which restarts both processes under Codex's lifecycle lock.
- `writes_history` answers the account-link reconciler's daemon question, read-only ([live-writer gate](./logins.md#the-live-writer-gate)). A present `app-server-daemon/loaded-threads.json` is a pending restart handoff and answers unknown without opening a socket. Otherwise a live app record in either namespace confirms a daemon under the home, and only then is `thread/loaded/list` read over that home's own control socket. It ignores `RIMZ_CODEX_APP_SERVER_SOCK`, which could name another account's daemon. An empty list is clear, ids are that many live sessions, and a list that cannot be read is unknown.

The app-server client reuses this daemon as its second connection choice. Its `thread/loaded/list` has two readers: the loaded-thread reaper, through the socket the override can redirect ([session death](./instances.md#session-death)), and `writes_history`.

## Account and balance

Codex has two account sources, and both normalize through [`account.rs`](../../../crates/rimz/src/agents/adapters/codex/account.rs) into the model in [providers.md](./providers.md).

| Source | Supplies | When |
| --- | --- | --- |
| App-server `account/rateLimits/read` | plan (`planType`), `primary` and `secondary` windows with `windowDurationMins`, credits | on the session refresh, and first on each account usage refresh of the room-default login |
| Direct OAuth usage endpoint ([`oauth_usage.rs`](../../../crates/rimz/src/agents/adapters/codex/oauth_usage.rs)) | plan (`plan_type`), windows, credits, reset credits | on each account usage refresh, after the realtime read for the room-default login and alone for other logins, for a file-backed token, unless `RIMZ_OAUTH_USAGE_OFFLINE` is on |
| Account probe | login state and metering | out of band ([providers.md](./providers.md#the-out-of-band-probe)) |

The `rimz agents refresh-usage` helper runs for a metered Codex login whether or not a session is live, at the cadences in [refresh cadences](./providers.md#refresh-cadences) (every 5 minutes, `OAUTH_USAGE_TTL`, for a login in use). It claims the read in `credits.json`, publishes the realtime read when it made one, then runs the direct probe with the `tokens.account_id` it read (`crates/rimz/src/sidebar/refresh/usage.rs::refresh_claimed_account_usage_with`). An account id change drops the cached windows and refetches. A present realtime plan wins, and an absent one keeps the cached OAuth plan for idle and keyring-backed display.

The spec's plan-label table maps raw `prolite`, `pro`, and `promax` to `ChatGPT Pro 100`, `ChatGPT Pro 200`, and `ChatGPT Pro 500`, matching [Codex 0.163.0's subscription labels](https://github.com/openai/codex/blob/83086188ae0614bd882818ff1aa23f3194962012/codex-rs/tui/src/subscription.rs#L10-L37). Other values keep the `ChatGPT` prefix with a title-case fallback. The mapping is display only, and the raw plan strings stay unchanged.

Normalization clamps and rounds percentages and orders windows short to long. When an authoritative reading carries timed windows but no 5-hour row, it adds an empty lifted 5-hour row so the dashboard still shows the expected pair. Credits map as follows:

| Wire field | `ExtraCredits` |
| --- | --- |
| `overageLimitReached` | known, 0 remaining |
| `unlimited` | `ExtraCredits::Unlimited`, no cap on paid extra usage |
| `balance`, a number or numeric string | known, clamped at 0; the wire value is in credits, and `normalize_credits` divides by 25 credits per US dollar so `remaining_usd` holds dollars (the rate comes from observed account data, 62,500 credits against a $2,500 balance, not from upstream source) |
| `hasCredits: false` | disabled |

Reset credits, from the direct endpoint and from the app-server's `rateLimitResetCredits`, normalize through `ResetCredits::normalized` with the effect `RestartsWindow`: redeeming a Codex credit refills the window and restarts its natural reset a full window from the redemption. Auto-redeem reads that effect ([providers.md](./providers.md#auto-redeem)).

The account probe reads `$CODEX_HOME/auth.json` first, so a readable file decides without a Codex binary:

| `auth.json` | Result |
| --- | --- |
| missing | fall back to `codex login status`, for credentials in the OS keyring |
| unreadable | `Unavailable` |
| unparsable, or no credential | `LoggedOut` |
| `OPENAI_API_KEY` | logged in, unmetered |
| `tokens.access_token` | logged in, metered, with the trimmed `tokens.account_id` |

`codex login status` runs the binary `RIMZ_CODEX_BIN` resolves, the same one the app-server uses, and `parse_login_status` reads its lines:

| Output | Result |
| --- | --- |
| `Not logged in` | `LoggedOut` |
| `Logged in using ChatGPT` | logged in, metered |
| `Logged in using` an API key, an Amazon Bedrock API key, or Amazon Bedrock AWS access keys | logged in, unmetered |
| `Logged in using` any other mode | logged in, metering unknown |
| no recognized line, a timeout, or a binary that does not start | `Unavailable` |

Prompt-cache defaults use this billing classification: a metered subscription gets 12h; an API key, Bedrock, AWS, or unknown sign-in gets 30m. The agent's own account evidence wins over the cached login record. At startup a missing account reading uses 30m until the account probe classifies it; observed split cache writes override these defaults and settings ([lifetime resolution](../../guide/configuration.md#idle-compaction)).

## Cost

[`spend.rs`](../../../crates/rimz/src/agents/adapters/codex/spend.rs) parses Codex's full history for the [`SpendingWalker`](../../../crates/rimz/src/agents/spending/mod.rs), read-only and without network, into the windows described in [spending.md](./spending.md#cost-history). The stateful line parser is [`spend/parse.rs`](../../../crates/rimz/src/agents/adapters/codex/spend/parse.rs). It reads both the interactive rollout format and the headless flat `usage` format, and the live card's cost comes from the same parser ([local refresh](#local-refresh)).

- Discovery covers `sessions/`, `archived_sessions/`, and other JSONL files in each Codex home: every comma-separated `CODEX_HOME` entry, or `~/.codex` when it is unset. An active rollout wins over its archived twin, so archiving never double-counts. The walk outside the two stores stays open because Codex resumes a rollout at whatever path it already has. It skips `.tmp/`, `tmp/`, `plugins/`, `skills/`, `worktrees/`, `packages/`, `cache/`, and `log/`, which Codex fills with copied repository content (plugin checkouts and caches, skills, managed worktrees), managed binaries, caches, or diagnostics, where a usage-shaped fixture would otherwise count as a session.
- The role join keys each rollout by the thread id in its filename, dropping the revision's `_<rollout_id>` suffix, so a reverted thread's files share its role and count as one role session.
- Ordinary scans prune date partitions outside the 365-day window, and the 15-minute complete reconcile ignores pruning.
- Each `token_count` contributes its `last_token_usage`, and only an event without that field falls back to the delta of `total_token_usage`.
- A forked or subagent rollout can open with a copy of the parent's history. `probe_replay` detects it only when a `thread_spawn` or `forked_from_id` marker sits in the file's first 16 KiB and the first qualifying usage records share one timestamp second. The parser then suppresses that prefix and keeps its last cumulative total as the baseline for the first new delta. A paginated fork that references the inherited history through `history_base` copies no prefix ([codex-reference.md](../../externals/agent-adapter/codex-reference.md#rollout-files)).
- A fingerprint of `codex:`, the timestamp, the model, and the token split collapses events copied across files, while keeping events that differ at subsecond precision.
- Each `CodexTokenEvent` is priced per model: fresh input (input minus cache reads and writes) at the input rate, cache writes at the cache-create rate, cache reads at the cache-read rate when the price book states one and at the input rate otherwise, and output (which includes reasoning) at the output rate ([token pricing](./spending.md#token-pricing)).
- A token event bills at the model's fast multiplier when the latest `thread_settings_applied` before it carried `service_tier` `priority` (or `fast`). Codex writes that record at start, at resume, and on `/fast`, while `token_count` names no tier. An absent or other tier (`default`, `flex`) bills at the standard rate. The tier rides the spend cursor beside the tracked model, so a resumed parse keeps it. The live card's cost and its `AgentContext.fast` reading come from the same fold: before any settings record the reading is unknown and the badge falls back to the launch posture, and a standard settings record clears it. A cursor written without the standard tier stays unknown until the next settings record.
- A headless judge (`codex exec --json`) reports usage with no tier, so it bills at the fast multiplier when the judge's launch posture had fast on (`LaunchParams::fast`).
- The fallback model is `gpt-5`. A model with no price keeps its tokens at zero dollars while the pricing refresh looks for a rate.
- Tool calls come from `function_call`, `custom_tool_call`, `local_shell_call`, and `web_search_call` records and attach to the next token event, or to the last one for calls that trail it, so tools that bypass hooks still count.
