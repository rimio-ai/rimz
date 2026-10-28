# Amp adapter

> Read [model.md](./model.md) for the provider-neutral agent model and [adapter.md](./adapter.md) for the integration layer every adapter implements. Binding, same-pane ownership, and supersession are in [instances.md](./instances.md). Accounts and balances are in [providers.md](./providers.md), spend and pricing in [spending.md](./spending.md), and the raw upstream protocol is in [amp-reference.md](../../externals/agent-adapter/amp-reference.md).

Amp exposes no command-hook, statusline, or interactive RPC protocol, so RimZ owns the wire. It installs a TypeScript plugin that observes Amp's own plugin API and forwards one JSON envelope per event to `rimz hooks feed --source amp`. The envelope schema is RimZ's, so a field the plugin stops sending after an upstream change is a RimZ adapter bug. The plugin never enters Amp's tool-decision path, and RimZ reads no Amp token, cost, or transcript source. The code lives in [`agents/adapters/amp/`](../../../crates/rimz/src/agents/adapters/amp/mod.rs):

| Module | Owns |
| --- | --- |
| `mod.rs` | `AMP_DESCRIPTOR` (the `static AgentSpec`), the coverage declarations, the hook catalog `AMP_HOOKS`, `AmpAdapter::decode_hook`, the plugin path (`amp_plugin_path`, `amp_config_home`), and `parse_amp_version` |
| `plugin.ts` | the plugin RimZ installs, embedded as `PLUGIN_SOURCE` |
| `payloads.rs` | `AmpHookPayload`, the typed envelope, and its tolerant `parse_payload` |
| `account.rs` | the secret-safe credential presence probe |
| `tests.rs` | launch and resume argv, lifecycle mapping, the edit-proof fallback, the final answer, install and uninstall, and the strings `plugin.ts` must keep |

## Hooks and lifecycle

The plugin turns four Amp plugin events and one thread-state change into five wire events, and `decode_hook` maps each to one [`LifecycleSignal`](../../../crates/rimz/src/agents/lifecycle.rs). The upstream events are in [amp-reference.md](../../externals/agent-adapter/amp-reference.md#events).

| Amp source | Wire event | Signal | Envelope fields the adapter reads |
| --- | --- | --- | --- |
| `session.start` | `session_start` | `Registered`, origin `Fresh` | `model` and `effort` once the enrichment read resolves |
| `agent.start` | `agent_start` | `TurnStarted` | `prompt`, sanitized into the task and prompt |
| `tool.result` | `tool_result` | `ToolUsed` | `files_modified`, else `tool_name` |
| `agent.end` | `agent_end` | `TurnEnded` or `TurnInterrupted` | `status`, `last_assistant_message` |
| thread state enters `awaiting-approval` | `permission_ask` | `AwaitingInput { Permission }` | none |

Every envelope carries `hook_event_name`, `session_id` (the Amp thread id, `T-…`), and `cwd` (the workspace root, else the runtime's working directory). `parse_payload` defaults every field when the payload does not deserialize, and an envelope without a non-blank `session_id` attaches no observation.

### Model and effort

The plugin reads `thread.agent()` after registration and stores the result per thread. A built-in mode reports the mode name (`medium`) as `model` and no effort. A custom agent definition reports its `model` and its declared `reasoningEffort`. Later envelopes for that thread carry both fields. The read itself sends nothing, so a turn that ends before it resolves never reports a model or effort.

### The active-thread filter

One Amp CLI can host several threads at once, and the plugin forwards an event only for the focused one. `isActive` passes a thread when `amp.activeThread.current` is `null` or names it, so a host with no focused thread forwards every thread's events. Whether a focus change fires `session.start` on current Amp builds is unverified ([amp-reference.md, item 10](../../externals/agent-adapter/amp-reference.md#undocumented-behaviour)).

Every `session_start` registers with `Fresh` origin, a returning thread included. Two `Fresh` roots on one pane fall under the fresh-conversation rule of [supersession](./instances.md#supersession): the newer one retires the older unless the older holds an open turn. The descriptor declares `KeepPrimary`, so between two open turns the earliest registered root keeps the card ([same-pane ownership](./instances.md#same-pane-ownership)).

### Delivery order

The plugin forwards registration before it starts the enrichment and thread-state reads, and it sends every envelope through one promise chain (`send`). Each envelope spawns its own `rimz hooks feed` helper only after the previous helper exits. This keeps a fast first turn from reaching the store ahead of its registration, and a slow enrichment read cannot delay or drop it. The chain has no length bound. Each helper is killed after 10 seconds (`SEND_TIMEOUT_MS`), and a failed or killed delivery is not retried.

### Permission observation

The permission path observes Amp's own approval UI without joining its decision chain. `observeState` subscribes to each registered thread's state and forwards `permission_ask` on the edge into `awaiting-approval`, only for a thread whose `session.start` this plugin instance saw, and only while that thread passes the active-thread filter. The state carries no tool, question, or resolver, so the card reads waiting with no ask detail, and the user answers in Amp. Leaving `awaiting-approval` sends nothing: the next turn or tool event moves the row on. The plugin does not register `tool.call`, whose cancellation and multi-plugin composition are undocumented upstream.

### Turn endings

`agent.end` carries a three-way `status`. `done` folds to a clean `TurnEnded`. `cancelled` folds to `TurnInterrupted`, which closes the turn at `idle` ([turn endings](./model.md#turn-endings-and-parked-turns)). Any other value, a missing status included, folds to `TurnEnded { errored: true }`, which resolves the row to `failed`.

### Tool events

Every `tool_result` is a mutating `ToolUsed` (`mutates: true`), whatever its status, so any completed tool counts as proof of work. The signal carries no tool name. Edit proof comes from `files_modified`, which the plugin derives from `amp.helpers.filesModifiedByToolCall`: true when the helper names at least one file, which covers Amp's edit tools and in-place `sed` edits. When the helper returns `null` or throws, the plugin omits the field, and the adapter falls back to the descriptor's `editing` list (`apply_patch`, `create_file`, `edit_file`) through `AgentSpec::tool_edits_files`.

### Events Amp lacks

Amp has no session-end, notification, compaction, or interactive subagent event. Pane liveness plus the rollup reaper derive the end stamp, and the `rimz exec` wrapper reports a lost multiplexer session. Turn end, `awaiting-approval`, and the stall window cover the attention-bearing part of idle. `rimz coverage --wiring` prints the full declared grid ([declared coverage](./adapter.md#declared-coverage)).

### Install

Install, preview, and uninstall own one whole file. `amp_plugin_path` resolves it in this order:

1. `RIMZ_AMP_PLUGIN`, when non-empty, overrides the whole path.
2. `$XDG_CONFIG_HOME/amp/plugins/rimz.ts`, when `XDG_CONFIG_HOME` is non-empty.
3. `~/.config/amp/plugins/rimz.ts`. Without `$HOME`, install fails.

The file opens with a `_rimz_managed` marker line. Install and preview refuse a file at that path without the marker, and uninstall leaves such a file untouched. The plugin runs `$RIMZ_BIN` when set, else `rimz`, and discards the helper's stdout, so the neutral hook answer is empty and permission prompts stay in Amp's own UI.

`AMP_MANAGED_SOURCE` is built with `upgradeable` false, the only whole-file adapter so built. A RimZ-owned plugin that differs from this build's `PLUGIN_SOURCE` is therefore never reported as `(upgrade available)` by `rimz setup` or refreshed by detected-hook install. An explicit `rimz hooks install amp` rewrites it, but reports `hooks up to date`.

### Runtime owner

Amp runs a system plugin in a child runtime process (`amp run <tmp>/amp-plugin-runtime-*/plugin-runtime.ts`), whose parent is the Amp CLI ([amp-reference.md, item 5](../../externals/agent-adapter/amp-reference.md#undocumented-behaviour)). The plugin therefore exports `process.ppid` as `RIMZ_AGENT_PID` to each helper, so the CLI that owns the pane, not the runtime, is the pid behind the session's `RuntimeOwner` ([session death](./instances.md#session-death)). A plugin reload replaces the runtime without changing that owner. The descriptor's `process_names` lists `amp` and `node`, the launcher of a JavaScript-bundle install, for the PID-attribution walk.

## Launch and resume

The descriptor declares these launch forms, pinned by `tests.rs::launch_resume_and_preset_commands_match_amp_cli`:

| Form | argv |
| --- | --- |
| interactive | `amp`, then the preset args |
| supervised prompt | `amp`, the preset args, then `-x <prompt> --plugin-ready-timeout 30 --no-archive-after-execute` |
| resume | `amp threads continue <T-id>` |
| fork, manual compaction, interrupt key, turn cap | none |

The supervised suffix fixes two upstream behaviours. Execute mode can start its turn before plugins load, which skips `agent.start` and `agent.end`, so `--plugin-ready-timeout 30` waits up to 30 seconds for them. Execute mode also archives the thread when it finishes, and Amp refuses `amp threads continue` on an archived thread, so `--no-archive-after-execute` keeps it resumable ([amp-reference.md](../../externals/agent-adapter/amp-reference.md#supervised-runs-and-stream-json)).

Profile presets render through `PresetMatchers`:

| Preset | Rendered as |
| --- | --- |
| `model` | `--mode <value>`, passed verbatim. Amp accepts `low`, `medium`, `high`, `ultra`, or a plugin mode by key or label ([modes](../../externals/agent-adapter/amp-reference.md#models-modes-and-effort)) |
| `fast` | `--fast` |
| `effort` | skipped with a warning at launch. Amp rejects `--effort` as an unknown option, and each mode's effort is set in Amp's Mode Dial settings |
| `auto-compact` | skipped with a warning at launch |

RimZ passes no permission-mode flag. Amp's default posture runs tools without approval, and its CLI has no flag for an ask or plan posture, so user settings and plugins own any approval UI.

`rimz agents amp -p` returns its answer through the same supervised-run path as the other built-ins. On `agent.end` the plugin walks `event.messages` backwards to the last assistant message with text, joins that message's text blocks with newlines, and sends the trimmed result as `last_assistant_message`. `decode_hook` sets it as the hook's final message, which feeds the transcript and the supervised-run result.

## Context and transcript

RimZ reads no Amp token, context, cost, transcript, or history source, so the card's live tokens, context gauge, estimated cost, and history stay empty. The plugin's `agent.end` slice is the only transcript content RimZ takes, and the plugin is the sole lifecycle and final-answer authority. Amp's plugin messages carry no token usage, cost, timestamps, or stable local path, and current Amp builds write no private thread cache ([amp-reference.md](../../externals/agent-adapter/amp-reference.md#private-local-thread-cache)).

## Account and balance

`AmpAdapter::probe_account` ignores the login environment and calls `account::probe`, which reads the process's own `AMP_API_KEY` and `$HOME`. It checks presence and never opens the secret:

| Condition | Probe result |
| --- | --- |
| `AMP_API_KEY` non-empty | `Found`: no plan, identity, or windows, `metered: Some(false)` |
| `~/.local/share/amp/secrets.json` is a file | `Found`, as above, with the file's mtime as `credentials_updated_at_ms` |
| that path is missing | `LoggedOut` |
| that path is not a file, or its metadata cannot be read | `Unavailable(ReadFailed)` |
| `$HOME` unset or empty | `Unavailable(SourceUnavailable)` |

`metered: Some(false)` marks an account without rate-limit windows, which the dashboard paints as unmetered. A present file is not a validated credential.

## Cost

Amp has no cost source RimZ reads. `amp usage` and thread usage print human-readable text with no documented machine schema, and no included-balance or usage-window source exists.

## Known gaps

`rimz coverage --wiring` prints the declared concern grid. Each gap below waits on an Amp signal that is not stable and machine-readable, or on a RimZ seam that does not exist yet.

- Tokens, context, cost, and history have no source. The supervised stream JSON (`amp -x --stream-json`) carries per-response `usage` and `max_tokens`, but RimZ does not launch it, so supervised runs carry no tokens or cost.
- The context window has no divisor. `amp plugins show-agent-options --json` lists each model's `contextWindow`, but RimZ does not read it, and a built-in mode does not name its backing model.
- The plugin's per-thread maps (`gauges`, `states`, `subscribedThreads`, `registeredThreads`) never shrink. Amp fires no thread-closed event, so a long-lived `amp --no-tui` runner keeps an entry for every thread it hosts. A cleanup needs such an event or a periodic prune against `amp.threads`.
- Permission coverage is declared wired but depends on another policy surface opening `awaiting-approval`: a plugin `tool.call` handler or an `amp.permissions` rule with an `ask` action. The default posture asks nothing, and whether a permission rule's `ask` enters that state like a plugin dialog is unverified ([amp-reference.md, item 3](../../externals/agent-adapter/amp-reference.md#undocumented-behaviour)).
- Permission presets have no rendering. A faithful `ask` or `plan` posture needs an adapter-owned prompt transform or a conditional policy in the managed plugin, since Amp has no CLI flag for either.
- Supervised threads stay unarchived, so they accumulate in `amp threads list` until the user archives them.
- Rich context is unsupported: credits and workspace billing, built-in subagent identity, permission detail and resolution, compaction brackets, and remote-control readiness. Amp does publish a paginated in-process transcript API, and a future transport can page `PluginThread.messages({ full: true })` inside the adapter.
