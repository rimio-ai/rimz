# OpenCode adapter

This page maps OpenCode 2 onto RimZ's types: the plugins RimZ installs, the events they forward, launch and resume, the context gauge, the transcript, the account, and cost. The provider-neutral agent model is in [model.md](./model.md) and the integration layer in [adapter.md](./adapter.md). Accounts and balances are in [providers.md](./providers.md), spend and pricing in [spending.md](./spending.md). The upstream protocol, based on OpenCode 2.0.27 with versioned updates, the 2.1.4 client API, and the 2.1.4 model-selection server, is in [opencode-reference.md](../../externals/agent-adapter/opencode-reference.md).

RimZ supports OpenCode 2 only. `crates/rimz/src/agents/adapters/opencode/mod.rs::OpencodeAdapter` declares a 2.0.0 floor through `min_version`. RimZ authors the whole wire: an in-process TypeScript plugin flattens OpenCode's hooks and bus events into a snake_case envelope that RimZ owns. A payload drift is therefore a RimZ bug in the plugin, never an upstream one.

## Files

| File | Owns |
| --- | --- |
| `crates/rimz/src/agents/adapters/opencode/mod.rs` | The `static AgentSpec` (`OPENCODE_DESCRIPTOR`), the declared coverage, the hook catalog (`OPENCODE_HOOKS`), `decode_hook`, the managed install, system-prompt delivery and native delegation denial in one env hook, and `context_window_for` |
| `crates/rimz/src/agents/adapters/opencode/index.ts` | The server half: activation, root and child classification, the event mapping, the gauge, launch reminders, and cleanup |
| `crates/rimz/src/agents/adapters/opencode/tui.ts` | The shared-service client half: route and tab sampling, snapshots, native events and asks, usage hydration, and serialized feeds |
| `crates/rimz/src/agents/adapters/opencode/payloads.rs` | `OpencodeHookPayload`, the typed envelope, and its tolerant parse |
| `crates/rimz/src/agents/adapters/opencode/database.rs` | Database discovery, the logical stat, read-only SQLite access, and credential loading |
| `crates/rimz/src/agents/adapters/opencode/account.rs` | The OpenCode `Config` for the shared delegated-account probes |
| `crates/rimz/src/agents/adapters/opencode/go_usage.rs` | The Console [Go status](#go-status) read: the URL override's trust check, the wire, and the 5h, 7d, and month windows with their amounts |
| `crates/rimz/src/agents/adapters/opencode/transcript.rs` | Transcript reads, the assistant-page cursor, and the final message |
| `crates/rimz/src/agents/adapters/opencode/spend.rs` | The spend parser |
| `crates/rimz/src/agents/adapters/opencode/session_step.rs` | Strict model matching, the private helper lease, and session create or switch |

Only the registry reaches `OpencodeAdapter`. The modules above stay private to the adapter directory, except `go_usage`: it is visible to `crate::agents` so that `crates/rimz/src/agents/delegated_account.rs::probe_account_usage` can call it.

## The plugin

### Install

The bundle contains `index.ts` and `tui.ts`, with no package manifest. `RIMZ_OPENCODE_PLUGIN`, when nonempty, names the bundle directory, otherwise `<config>/opencode/plugins/rimz`, where `<config>` is `XDG_CONFIG_HOME` or `~/.config`. Paths resolve from the process environment, not the login environment. Each file's first line carries `_rimz_managed`; ownership follows that marker:

- Install preflights both targets and the legacy path before writing changed files atomically. Unmarked files and symlinked targets or bundle directories refuse with their paths.
- With no override, install removes a marked legacy `<config>/opencode/plugin/rimz.ts`, which otherwise shadows the new server half. An unmarked legacy file refuses with a move-aside fix. Preview shows both sources and the legacy removal.
- Installed means both files are marked and no legacy file shadows them. A stale, partial, or legacy-only owned install offers a refresh; doctor reports a partial or legacy-only install as missing.
- Uninstall removes only marked files and removes the bundle directory only if empty. Foreign siblings survive. The override never modifies the default install.
- Wiring inputs include both files, the legacy path when applicable, and the bundle directory, so admission notices creation, deletion, and replacement.

`OpencodeManagedIntegration::install_blocker` refuses install and preview when the located `opencode` reports a version below the floor (see [Launch and resume](#launch-and-resume)).

### Server activation

OpenCode calls the plugin's `setup(ctx)` from its default export, `{ id: "rimz", setup }`. Setup does nothing unless `RIMZ_WORKSPACE_ID` is nonempty, the process argv carries no `--service` (see [Process identity](#process-identity)), and an executable `rimz` resolves (`RIMZ_BIN`, otherwise the first `rimz` on `PATH`). A second setup on the same context returns the same activation, from a `WeakMap` keyed by context. Each activation keeps its own session, child, model, agent, form, and gauge maps. It registers the session `prompt` and `context` hooks and the tool `execute.after` hook, and it consumes `ctx.event.subscribe`.

The plugin classifies each session as a root or a child. `session.created` classifies by its `parentID`. A session first seen through another event or hook is classified through `ctx.session.get({sessionID}).parentID`, which covers a plugin reload and events that arrive before the creation event. A failed lookup leaves the session unclassified, and the plugin drops its events instead of crediting them to the root. The plugin also drops events from another `location.directory` and events for the `global` session.

Each envelope goes to RimZ as one `rimz hooks feed --source opencode` child, which reads the JSON line on stdin. The plugin discards the child's stdout and stderr, so a RimZ reply never reaches OpenCode. Every envelope carries the session id, the session directory as `cwd` and `worktree_path`, and the remembered title, version, model, effort, display name, window, and token split. `crates/rimz/src/agents/adapters/opencode/payloads.rs::parse_payload` is tolerant: a field of the wrong type degrades the whole envelope to an empty record.

Cleanup runs when OpenCode disposes the activation. It aborts the subscription, sends `session_ended` with reason `dispose` for every root, disposes the hook registrations, and waits at most 1.5 seconds for the pending sends and the disposals to settle. Every handler checks for disposal again after each `await`. A handler that throws does not end the subscription. Pane liveness and the reaper cover a crash, where no cleanup runs ([instances.md](./instances.md#session-death)).

### Process identity

RimZ launches OpenCode with `--standalone`, so the TUI starts a private `opencode serve` child that inherits the pane environment, and the plugin runs in that child ([reference](../../externals/agent-adapter/opencode-reference.md#opencode-2-plugins-and-runtime)). The plugin sets `RIMZ_AGENT_PID` to its own `process.ppid` in each feed child, which names the TUI. `crates/rimz/src/cli/hooks/owner.rs::attach_agent_owner` stamps that pid, with its process-start token, as the session's `RuntimeOwner`, which feeds the reaper and every same-instance proof ([instances.md](./instances.md#session-death)). Before the first hook, a fresh RimZ launch records the exec wrapper as the owner (`crates/rimz/src/cli/agents_cmd/exec.rs::attach_own_launch_pane`), and the first hook replaces it with the TUI. A resume attaches the provider pid it spawned before any hook fires ([instances.md](./instances.md#binding-a-session), step 2).

The server half stands down in OpenCode's shared background service, `opencode serve --service`. The first client to find no service spawns it with that client's environment ([reference](../../externals/agent-adapter/opencode-reference.md#opencode-2-plugins-and-runtime)). That environment cannot identify subsequent clients, so only the client half reports their conversations. The private `serve --stdio --port 0` child of a standalone TUI keeps the server half. Process detection recognizes both `opencode` and `bun` (`process_names`).

### Client activation

`crates/rimz/src/agents/adapters/opencode/tui.ts` activates only with `RIMZ_WORKSPACE_ID`, an executable RimZ, and `service.restart` in `ctx.keymap.commands()`, read after app initialization. Explicit `--standalone` is inert. Until that command appears, the client subscribes to no events and reads no session data. A 200 ms sampler follows `ui.router.current()` and exact session IDs from `ui.tabs.list()`. Session maps are evicted when a session is neither selected, tabbed, nor holding a forwarded ask; per-step maps clear on departure and step completion.

Each feed carries the TUI's own `process.pid`, its pane and room environment, and empty inherited launch-identity values: every `RIMZ_AGENT_*` except the overwritten PID, hook owner kind, run, workflow, team, launch group/ordinal, runtime environment, and loop task. Feeds are awaited one at a time. After waiting for an earlier feed, the client rechecks the route before stamping; a stale queued snapshot is dropped. A deletion end keeps the stamp of the session last reported on screen even after the route has left it.

Activation, session selection, and reconnect synchronize the selected session and its permission and form lists. A failed state read sends nothing and retries on the next sample. After bounded usage hydration, a bounded `client.session.active()` read gets the server's status rather than the asynchronously hydrated cache. The route is checked again: an unseen pending ask sends its stamped ask, an already-forwarded ask sends nothing, otherwise current status sends `client_idle` or `client_running`. These are state snapshots, not lifecycle edges ([model.md](./model.md)).

Background tabs forward asks and replies, unstamped. A forwarded request remains tracked after its tab closes, and its reply needs no cached session info. Replies normalize form answers in field order and forward the next pending ask. At activation and on `server.connected`, a sweep covers every tab and forwarded session except the one arrival covers. Each session gets a bounded sync of permissions and forms; the first pending unforwarded ask goes out unstamped. The sweep sends no snapshot except to clear a vanished forwarded background ask, preserving reconnect reconciliation without inventing a reply.

Deletion is handled before cache lookup, because upstream has already evicted the session. Deleting the session last reported on screen sends a stamped `session_ended`; deleting a session with a forwarded ask sends an unstamped end. Other deletions stay silent. Navigation, the home route, tab close, and cleanup alone send no end. Cleanup unsubscribes and drains feeds for at most 1.5 seconds; process liveness hides a dead client's rows.

## Events

The following table describes the server half. The client uses the same wire for the selected conversation, stamping `on_screen: true`, with these differences:

| Client event | Wire |
| --- | --- |
| Arrival or reconnect, no pending ask | `client_idle` or `client_running`, with `state_snapshot` set by decode |
| Activation or reconnect, unseen pending background ask | `permission_ask` or `question_ask`, unstamped; already-forwarded asks are not repeated |
| `session.inbox.enqueued`, user item only | `chat_message`, with `item.payload.text` |
| `session.execution.started` | `client_running` |
| `session.tool.success` or `failed` | `tool_after`; name joined from `session.tool.input.started` by message/tool ID, else cached tool content; unresolved names omitted |
| `session.deleted`, session last reported on screen or holding a forwarded ask | `session_ended`, stamped for the former and unstamped for a background asker; other deletions send nothing |
| Navigation or cleanup | None |

The client remembers step usage, model and agent selections without an envelope, forwards renames, execution terminals and compaction as below, and forwards permissions/forms and replies for the selected conversation or exact background tabs. It never emits child lineage; a child first seen on screen reports under its own native ID as a root.

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

Registration is lazy (`registers_lazily`). Without a requested model, a fresh OpenCode TUI creates its session at the first prompt, so `session.created` and `session_created` arrive only then ([reference](../../externals/agent-adapter/opencode-reference.md#opencode-2-cli-and-environment)). With a requested model, the [session step](#model-selection) creates the session before the TUI starts; its first `chat_message` adopts the card, since the TUI did not create that session and emits no creation event for it. Until the first prompt the pane renders as the idle row of [instances.md](./instances.md#before-a-session-binds). Restarting before that prompt starts fresh again, leaving an empty OpenCode session behind.

The adapter declares `SamePaneSessionPolicy::FollowLatest`. A plain client's card follows the on-screen conversation, even when the previous conversation is still running. A background ask takes ownership until answered, then the on-screen row takes it back without another envelope ([instances.md](./instances.md#same-pane-ownership)). Managed standalone launches retain unstamped latest-root succession. The fresh-conversation rule never applies, because the wire does not say how a root was created and RimZ records no `Fresh` origin for it.

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

Detection searches `PATH`, then `~/.opencode/bin`. Launch, install, setup, and `rimz doctor` refuse a binary whose readable version is below 2.0.0, with the fix "run `opencode upgrade --major`" (`crates/rimz/src/agents/version.rs::check_launch_version_floor`). A version RimZ cannot read does not refuse. Model selection is verified on 2.1.4, without raising the floor for model-less launches. Fork and effort presets are unsupported; unsupported fast and auto-compact settings warn and launch without the setting.

Other launch text limits:

- A headless request refuses, because `launch.headless` is `None`.
- A compaction instruction is dropped without notice, because `CompactInstruction::Unsupported` renders only `/compact`.

`crates/rimz/src/agents/adapters/opencode/mod.rs::OPENCODE_DESCRIPTOR` declares `SystemPromptFile` as `PresetArgMatcher::EnvConfigMember`, naming `OPENCODE_CONFIG_CONTENT` and `agents.build.system`. Every OpenCode definition therefore requires a nonempty `agents/opencode.md` kind base, even without a craft body. A home with no definitions needs none; there, a bare `rimz agents opencode` still launches without a replacement. The [kind-base validation rule](../../reference/definitions.md#kind-bases) supplies the missing-base error.

The [shared composition order](../harness/fleet.md#system-prompt-composition) applies to kind bases, crafts, profile bodies, CLI prompt files, and team layers. `crates/rimz/src/harness/prompt_compose.rs::plan_system_prompt` always plans a content-addressed `live/prompt/sys.<digest32>.md` artifact for this matcher, including a lone file base. Its materialized value adds no argv or env directly: `MaterializedSystemPrompt.hook_path` travels through `LaunchReminders.system_prompt_file` to the final env hook. `crates/rimz/src/harness/launch_plan.rs::compile` carries it for every launch action, resume included, and `crates/rimz/src/harness/launch_plan.rs::apply` writes the artifact before the provider starts. `rimz agents explain opencode` shows `OPENCODE_CONFIG_CONTENT agents.build.system` and the planned artifact without writing it.

`crates/rimz/src/harness/prompt_compose.rs::render` refuses a non-UTF-8 artifact path or one containing `{`, `}`, `"`, `\`, or a control character, before the provider starts. A `{` would let OpenCode's `{env:}` substitution, which runs before `{file:}`, consume the reference. The refusal is:

```text
opencode system prompt artifact `<path>` cannot be carried by a {file:} reference: its path must be UTF-8 and contain no `{`, `}`, `"`, `\`, or control characters; set XDG_RUNTIME_DIR to a directory whose path has none of them
```

Launch reminders arrive through `SystemTextChannel::ExtensionEnv`, which sets `RIMZ_LAUNCH_REMINDERS` ([fleet.md](../harness/fleet.md#launch-reminders) owns the text). The first activation in the process claims the value into a `globalThis` symbol and deletes the variable, so tool children never inherit it. The `context` hook then appends `{type: "text", text}` to the mutable `system` array for root sessions only. The hook checks the session and nothing else: it has no request-kind check.

Every managed OpenCode launch calls `crates/rimz/src/agents/adapters/opencode/mod.rs::OpencodeAdapter::finalize_launch_env` once from `crates/rimz/src/harness/launch.rs::compile_agent_process_with_extra_env`. The hook receives the effective login env (composed launch env over inherited login env), the mutable final env, the native-delegation decision, and the optional composed prompt path. All edits to one variable share one JSONC parse, so prompt delivery cannot erase delegation denial or vice versa. A value exported only by shell startup files is not an input to this merge; the final override wins over it ([env precedence](../harness/trust.md#env-application)).

When neither native delegation is switched off nor a prompt is delivered, the hook leaves `OPENCODE_CONFIG_CONTENT` untouched, even if malformed; an absent value stays absent. Otherwise it parses the effective value as a JSONC object (absent means `{}`), keeps every valid user key, and writes strict JSON after these edits:

- With [native delegation switched off](../harness/subagents.md#native-delegation-policy), append `"-opencode.tool.subagent"` last in `plugins`, creating the array when absent and not doubling a selector already last. This removes the built-in tool rather than denying a permission, so neither `--auto` nor an agent-specific allow restores it ([reference](../../externals/agent-adapter/opencode-reference.md#opencode-2-native-requests)).
- With a prompt, write `agents.build.system = "{file:<absolute artifact path>}"`, creating absent containers and preserving siblings in `agents` and `agents.build`. Leave `plugins` alone unless delegation is also switched off. OpenCode substitutes the file contents without re-scanning them for `{env:...}` or `{file:...}` ([upstream rules](../../externals/agent-adapter/opencode-reference.md#system-prompts-and-config-substitution)).

Invalid JSONC, a non-object root, non-array `plugins` when delegation edits it, or non-object `agents` or `agents.build` when prompt delivery edits them discards the entire configured value before applying all requested edits to `{}`. `crates/rimz/src/agents/adapters/opencode/mod.rs::parse_inline_config` checks `plugins` first, then `agents`, then `agents.build`, only when needed. It returns one warning naming the first offending member and its required shape (for a parse or root error, the first required member):

```text
OPENCODE_CONFIG_CONTENT <problem>; this launch ignored the configured value; set it to a JSON object whose <member> is <shape>, or unset it
```

When delivering a prompt, a pre-existing `agents.build.system` in valid effective inline content, any value including an empty string, is replaced with exactly this warning:

```text
OPENCODE_CONFIG_CONTENT set agents.build.system; RimZ's composed system prompt replaced it for this launch; move that prompt into agents/opencode.md or a profile's system-prompt-file, or remove it from the variable
```

The same hook always edits `OPENCODE_CLI_CONFIG_CONTENT`, regardless of delegation or prompt delivery. It reads the effective value from the same env snapshot and writes strict JSON with `tabs.mode = "off"`:

| Effective CLI config content | Final CLI config content | Warning |
| --- | --- | --- |
| Unset or exactly `""` | `{"tabs":{"mode":"off"}}` | None |
| JSONC object with absent or object-valued `tabs` | Same object with `tabs.mode = "off"`; every other key survives, including sibling tab settings and legacy `tabs.enabled` | None |
| Invalid JSONC (including whitespace-only), non-object root, or non-object `tabs` | `{"tabs":{"mode":"off"}}` | One `LaunchPlanWarning::LaunchEnv`, naming the variable and problem, saying this launch ignored the configured value, and giving the fix: set a JSON object whose `tabs` is an object, or unset it |

This hides the native session tab strip from OpenCode 2.0.13 onward; 2.0.0 through 2.0.12 keep their tabs, and the minimum supported version remains 2.0.0. Manual OpenCode launches, including from a room shell, are unchanged. RimZ validates only the root and `tabs` container shapes, not OpenCode's versioned settings schema: another invalid setting such as `{"theme":42}` or `{"tabs":{"scope":42}}` makes OpenCode drop the whole inline value, including RimZ's mode, and fall back to `cli.json`. The [upstream CLI config reference](../../externals/agent-adapter/opencode-reference.md#opencode-2-cli-and-environment) owns parsing, precedence, version support, and legacy-tab normalization.

Warnings return in order, core config before CLI config, at most one per variable. Each becomes `crates/rimz/src/harness/launch_plan.rs::LaunchPlanWarning::LaunchEnv`; the exec wrapper prints and records it with the other launch warnings.

### Replacement limits

- RimZ writes only `agents.build.system`; it does not pin `default_agent`. A different configured primary agent or a resumed session persisted under another agent uses that agent's own prompt. For `build`, OpenCode reads the configured system text per request, so a resume gets the replacement again without extra argv. [Upstream source](../../externals/agent-adapter/opencode-reference.md#system-prompts-and-config-substitution) shows this, and a live `rimz agents resume` on OpenCode 2.1.4 confirmed it: a prompt changed between stop and resume took effect in the resumed session.
- Project instructions and RimZ's plugin reminder still follow the replaced built-in prompt. Managed settings load after inline content and can override `agents.build.system`; RimZ does not discover them and cannot warn about that override.
- The artifact freezes the launched text, but runtime GC can sweep it by mtime after `gc.older_than` (default 7 days), even while a session remains open ([runtime retention](../store.md#garbage-collection)). A later OpenCode config reload then re-reads a missing file. On 2.0.30 and later, OpenCode warns and substitutes empty, restoring its built-in prompt until restart. On 2.0.0 through 2.0.29, the reload fails and keeps the previous config; a missing reference at startup instead fails the load ([versioned behavior](../../externals/agent-adapter/opencode-reference.md#system-prompts-and-config-substitution)).

## Model selection

`LaunchSpec::model_session_step` declares model support without an argv matcher. The OpenCode `LaunchCapability::session_step` implementation prepares a native session before the TUI runs. The hidden command is `rimz agents session-step opencode --model <name> [--session <id>] -- <provider argv…>`. A fresh launch with a model creates a session; a resume with an explicit model override switches that session. Model-less launches, plain resume, and restart do not run the step. The provider argv stays OpenCode's command for binary and version checks; the step runs inside the login shell, scoped view, and hibernation scope.

The step launches the provider argv's program as `serve --stdio --hostname 127.0.0.1 --port 0`, in its own process group. One 30-second deadline covers readiness, every HTTP request, and shutdown. The helper advertises its loopback URL on one stdout JSON line and is bound to the step by its stdin pipe. Killing the step closes that lease. Success closes stdin and waits for the helper's own exit. Every path then kills the helper's process group, so no worker it started outlives the step, and failure or expiry does so before returning a refusal. There is no retry inside the step.

The helper inherits the step's environment except `RIMZ` and every `RIMZ_*` key, making RimZ's installed plugin inert. Its `OPENCODE_CONFIG_CONTENT` is parsed as JSONC, preserving authored keys and policies and appending `integration.use` denies for `plugin:*` and `mcp:*`. Malformed content becomes a deny-only document. The helper sets `OPENCODE_DISABLE_MODELS_FETCH=1`, `OPENCODE_FILEWATCHER_DISABLE=1`, `OPENCODE_DISABLE_FFF=1`, and a fresh random `OPENCODE_PASSWORD`. None of these changes reaches the final TUI: its environment, including launch reminders, is unchanged.

Requests use Basic auth as `opencode:<password>`. `GET /api/model?location[directory]=<cwd>` reads the configured model list in the directory the agent sees. Matching accepts an exact full `providerID/id`, including an ID containing `/`, or a bare ID without `/` offered by exactly one provider. A name containing `/` names its provider, as OpenCode's own model references do, so it never matches another provider's ID: with only OpenRouter configured, `openai/gpt-5` is refused and `openrouter/openai/gpt-5` suggested. Ambiguous IDs list every full name. Unknown names suggest up to three nearest full names within two edits of either the full name or its ID, otherwise point at `opencode models`. A `#variant` suffix is refused. The list is not a credential check and is not cached by RimZ.

Create posts `{"id":<minted>,"location":{"directory":<cwd>},"model":{"providerID":…,"id":…}}` to `/api/session`; switch posts `{"model":{"providerID":…,"id":…}}` to `/api/session/<id>/model`. Only a successful match can reach either POST. IDs follow OpenCode's descending `ses_` shape. After closing stdin and waiting for the helper to exit, the step execs the provider command with the same PID. Create inserts `--session <minted>` immediately after the program; switch keeps argv unchanged. The [upstream reference](../../externals/agent-adapter/opencode-reference.md#model-selection-server-v214) pins these surfaces.

A refusal writes one final `rimz: OpenCode model check refused …` stderr line and exits 78. An HTTP error whose body carries OpenCode's own `message` quotes it. A failed model list keeps the verified-version note and the adapter's upgrade fix beside that message, since an OpenCode older than the verified version can lack the route. A failed switch onto a session OpenCode no longer has (404 `Session not found: <id>`) quotes the message alone. Every other helper or API failure adds the note and the fix. The exec wrapper does not retry exit 78 or record a provider-startup incident. Its pane hint points at the reason above and names the command to rerun: the fresh launch, or for a refused model switch on resume, the resume of that conversation. A failed subagent's captured tail carries the reason to its caller. Explicit subagent `--retries` still retries failed runs.

During this short step, hibernation finds no provider child and a Zellij pane-PID match can miss; the helper itself matches OpenCode's process names. These transients write no lasting identity. Exec preserves the spawned PID, and the TUI plugin then names that PID as its parent.

## Context gauge

The plugin's gauge is a per-session record of the model, its window, and the latest token split, stamped onto every envelope. `session.step.started` stores the step's model and agent under its `assistantMessageID`. `session.step.ended` or `failed` removes that pair and reads the step's tokens:

- Output is OpenCode's output plus its reasoning.
- The total is input, output, cache read, and cache write.
- A positive total replaces the whole split, zero-valued fields included. An all-zero split keeps the last measurement.

`ctx.model.list()` gives the display name and the window: a positive `limit.input`, otherwise a positive `limit.context`, otherwise none. One activation reads the catalog once, and reads it again after an empty or failed result. When an envelope carries no window, `crates/rimz/src/agents/adapters/opencode/mod.rs::context_window_for` supplies 1M for an id containing `[1m]` or a Claude id containing `1m`, 200k for any other Claude id, and nothing for other models. The gauge counts input-side tokens only ([model.md](./model.md#enrichment)).

`ctx.session.get` refreshes the title, agent, and model at registration, at each prompt, and at each root execution boundary. The refresh runs in the background, so a slow lookup never holds a terminal envelope. The plugin never stores OpenCode's placeholder titles, `New session - ...` and `Child session - ...`. The version comes from `session.created` alone, because session lookup returns none.

The client hydrates arrival usage from the newest nonzero assistant in the synced cache, otherwise `client.message.list` pages newest-first with limit 200. A short page is not terminal while it has a cursor; an empty page or absent next cursor is. Zero usage never wipes a measurement. Catalog matching uses provider plus model and the session's location; the numerator and input-window rule stay the same as the server half. Usage/catalog hydration gets about one second before status goes out without it; late enrichment rides the next envelope. No gauge-only lifecycle event is added.

## Transcript

`crates/rimz/src/agents/adapters/opencode/transcript.rs::read_messages` reads `session_message` rows for one session id only, since every OpenCode agent on the machine shares the database. It takes a user row's `data.text` and the `text` entries of an assistant row's `data.content[]`. Reasoning, tool content, and other record types stay out. Full reads order by `time_created`, then `rowid`.

The assistant-page cursor follows `rowid` over completed assistant rows only: `time.completed`, `finish`, or `error` marks completion. The cursor never advances over a message that is still streaming, so the in-place update that completes it is still read. An imported row with an older timestamp but a higher `rowid` still counts as new. The cursor's identity includes the database path and the session id.

`rimz agents history` joins these messages with the session's spend rows. Spend timestamps have second precision, so turn attribution compares boundaries at that precision while displayed timestamps keep theirs. When two turns open in the same second, the later boundary owns that second.

## Account and balance

Both account probes run through `crates/rimz/src/agents/delegated_account.rs`, which OpenCode shares with Pi. `crates/rimz/src/agents/adapters/opencode/account.rs::config` supplies the OpenCode inputs: `database::load_credentials`, the used provider from `database::latest_message_provider`, `key` as the API-key type, and the account-key domain `rimz/opencode-oauth-account-key/v1`. Both probes ignore the login environment, and database discovery reads the process environment, so a named login does not change what they read ([providers.md](./providers.md#logins)).

`crates/rimz/src/agents/adapters/opencode/database.rs::credentials_in_file` reads the newest database's `credential` table, ordered by `active DESC, time_created DESC, id DESC`, and keeps the first row that parses for each `integration_id`. The credential stamp is the largest `time_updated`, in milliseconds, of the rows kept. Session writes therefore never invalidate the OAuth usage cache. A missing database or `credential` table reads as logged out. A database that exists but fails to read is unavailable.

The account probe and the unnamed usage probe share credential resolution:

| Step | Account probe (`select_display`) | Usage probe (`select_usage_entries`) |
| --- | --- | --- |
| 1 | the exact provider key of the latest assistant row, across databases | the same |
| 2 | for `opencode-go` without an exact credential, the `opencode` Console credential | the same |
| 3 | a named but unlisted provider is an unlabeled, unmetered API key | no credentials |
| 4 | no provider and one credential selects that credential | the same |
| 5 | no provider and several credentials leaves plan and metering unknown, provisionally | no credentials |
| a non-OAuth pick | shown, with its metering | refused as API-key only |

The account probe maps `oauth` to metered, `key` to unmetered, and `external` to unknown metering. It labels the plan `<Provider> OAuth`, `API Key`, or `External`, except for the Console `opencode` OAuth credential, whose plan comes from the Go status reply. The selected credential's raw provider key becomes `sub_provider`. A caller can name an exact usage credential through `AccountCapability::probe_account_usage_for`, keeping the `opencode-go` alias above. The usage probe also refuses an empty access token and an expired or undated one. For the `openai` scope, the account key is the credential's top-level `accountId`, then `metadata.accountID`. An empty top-level value masks the metadata. Every other case hashes the refresh token, or the access token, under the account-key domain. Console credentials also hash a non-empty `metadata.orgID` into that key, so an organization switch changes the cache owner without recording the organization id.

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

- Plain shared-service clients need OpenCode's 2.1.4 TUI API and `service.restart` command. Explicit `--server` clients remain silent; a renamed command makes the client half inert. A disabled service uses the server half instead.
- The home route keeps the last conversation's card until that conversation is deleted, another is selected, or the process exits. One conversation open in two panes may hop between them.
- A child already recorded with parent lineage cannot take the card; a child first seen through the plain client can.
- A conversation left mid-turn is reaped and rests at Idle. Session-bound waits retire at the next real hook event. Plain clients have no messaging, adoption, wait, service-hibernation, or sidebar tab-switching promise; use managed launches for advanced workflows.
- Off-screen turn cost lags until that conversation's next on-screen turn end. Equal focus stamps fall back to ordinary ownership order; a backward wall-clock step can delay takeover.

- Native answering, background parking, background shells, and remote control are unsupported.
- Idle attention is partial: OpenCode has no idle notification, so RimZ reconstructs it from turn boundaries, native asks, and the stall window.
- Plan mode at launch, effort presets, fork, and headless runs are unsupported. System-prompt replacement has the [agent-selection, managed-settings, and artifact-retention limits](#replacement-limits) above.
- RimZ does not read `OPENCODE_DB`, and it skips channel databases whose names contain a `.`, which OpenCode allows ([reference](../../externals/agent-adapter/opencode-reference.md#opencode-2-storage-and-credentials)).
- Workspace-scoped spend reads one origin for the whole shared database.
- The reference does not record whether OpenCode runs the `context` hook for title or generate requests, so whether those requests carry the launch reminder is unverified.
- No shared language-server suppression is verified for OpenCode, so [shared servers](../lsp.md) run beside OpenCode's own.
