# The adapter layer

Eighteen built-in coding agents report to RimZ through one seam. Code outside `crates/rimz/src/agents/` resolves an [`AgentDefinition`](../../../crates/rimz/src/agents/definition.rs) and calls its methods, and it never names a provider module or a concrete adapter type. This page owns that seam: what an adapter is, the contracts it implements, the path a native hook event takes from the agent's process to the durable store, where context readings come from, and how each adapter declares what it supports.

Read [model.md](./model.md) first. It defines the [`AgentLifecycleObservation`](../../../crates/rimz/src/agents/observation.rs) an adapter produces and the rollup that consumes it, and this page covers the producing side. What each native event means for one agent is on that agent's page ([adapter_claude.md](./adapter_claude.md), [adapter_codex.md](./adapter_codex.md), and sixteen siblings). Accounts and spend are in [providers.md](./providers.md), and the third-party process-plugin wire is in [plugin.md](./plugin.md).

## The one rule

An adapter is the single place one agent's protocol is normalized.

```text
native protocol            the adapter boundary              generic RimZ
─────────────────          ────────────────────              ────────────
hook JSON payload   ──►                              ──►  AgentLifecycleObservation
transcript JSONL    ──►    adapters/<kind>/          ──►  AgentContext · TranscriptMessage
auth files, APIs    ──►    (private module)          ──►  AgentAccount · RateLimitWindow
provider CLI argv   ──►                              ──►  argv · SpendParse · AccountProbe
```

Everything left of the boundary is provider knowledge, and everything right of it is agent-agnostic. Two mechanisms hold the line:

- Compiler privacy. The `adapters` module is private to `agents`, and the `ensure_private_agent_adapter_boundary` check in `cargo xtask invariants` fails a build where Rust code outside `agents` names a provider module or a concrete adapter type.
- One dispatch point. [`registry.rs`](../../../crates/rimz/src/agents/registry.rs) resolves a kind string to an `AgentDefinition`. Callers use the definition's methods and never learn which struct is behind it.

The invariant matches module paths and type names, not kind strings, so it does not see a branch on `kind == "codex"`. A few such branches exist outside `agents`. `crates/rimz/src/cli/hooks.rs::run_feed_inner` waits for the drainer's reply only for `antigravity` and for a Claude or Codex `UserPromptSubmit` ([the feed](#the-feed)). `crates/rimz/src/sidebar/refresh/mod.rs::codex_origin_overrides` and `crates/rimz/src/sidebar/refresh/sessions.rs::codex_turn_death_retry_due` serve Codex alone. A new adapter that needs one of those behaviours has to be added at that site. A new behaviour belongs on the spec or a trait, where every adapter answers it.

## The registry

[`registry::BUILTINS`](../../../crates/rimz/src/agents/registry.rs) holds one `AgentDefinition` per compiled-in adapter, in display order: `claude`, `codex`, `amp`, `copilot`, `kimi`, `pi`, `opencode`, `antigravity`, `cursor`, `droid`, `kiro`, `qwen`, `grok`, `hermes`, `qoder`, `omp`, `muse`, `minimax`. `all_definitions()` chains that slice with the validated machine-tier [process plugins](./plugin.md), and every lookup goes through it:

| Function | Answers |
| --- | --- |
| `find_definition(kind)` / `definition_by_kind(kind)` | the definition for a `--source <agent>` tag, matching the spec's `kind` or one of its `aliases`. The second returns an error for an unknown kind |
| `spec_by_kind(kind)` | the same lookup for callers that need only const data (branding, tool tables) |
| `known_kinds()` | kinds in display order, for doctor, wiring probes, and coverage |
| `command_agent_kind(command)` | which agent a pane's command line is running, after shell and launcher normalization |
| `resumed_session_id_from_cmdline(cmdline)` | the session a `<agent> --resume <id>` process reopened. Abstains when two adapters match |
| `room_env(runtime)` | the merged adapter environment a new room exports to the multiplexer as one opaque map |

`command_agent_kind` decides whether a pane is running an agent at all. It matches the resolved program basename against each spec's `bin_names`, accepting a target-triple release suffix: `codex-aarch64-apple-darwin` names `codex`, and so does its 15-character kernel `comm` truncation. When the program is a launcher such as `node`, matching falls back to the launcher's script path. Without a program match it tries the process `comm`, and a `comm` two adapters claim abstains. A matched command then passes the adapter's `is_interactive_process`, which is how `codex app-server` and `codex remote-control start` stay out of the room while `codex` enters it. How a pane whose multiplexer reports only `node` gets classified is in [instances.md](./instances.md#recognizing-a-hosted-cli).

## Two halves: the spec and the traits

Each adapter is a private unit struct under [`adapters/<kind>/`](../../../crates/rimz/src/agents/adapters/mod.rs) that returns one `static` [`AgentSpec`](../../../crates/rimz/src/agents/definition.rs) and implements ten capability traits. Everything knowable at compile time is spec data. Everything that reads a file or parses a payload is a trait method.

### The spec

| Group | Fields |
| --- | --- |
| Identity | `kind`, `aliases`, `display_name` |
| Presentation | `brand` (emblem, 256-color index, RGB), `plan_label`, `expected_windows` |
| Process | `process_names` (kernel `comm` candidates, including launchers), `bin_names` (`$PATH` probe order), `bin_identity` (the check that keeps an ambiguous binary name from resolving to another provider's install. No built-in claims such a name), `extra_bin_dirs` |
| Vocabulary | `tools.input_key` (the payload key holding tool arguments), `tools.mutating`, `tools.editing` (a validated subset of mutating), `tools.blocking` (tool name paired with its [`AskKind`](../../../crates/rimz/src/agents/lifecycle.rs)) |
| Policy | `capabilities` (below) |
| Claims | `coverage`, `user_coverage`, `lifecycle_hooks` ([declared coverage](#declared-coverage)) |
| Accounts and spend | `sub_providers` (the subscription providers whose budget the agent meters), `thread_key` (how transcript files map to billing threads) |
| Defaults | `default_model`, `default_context_window` |
| Skills | `host_skills` (the provider's host-mode skill switch, or `Unsupported`), `tool_rules` (how a definition's tool rules render into provider settings, or `Unsupported`) |
| Launch | `launch`: program, fixed args, prompt style, resume, picker, and fork argv, per-mode permission args, max-turn flag, compact command, interrupt key, preset matchers, optional headless form |

The launch block is data, so one registry entry is enough to render `rimz agents restart`, resolved profiles, and the permission presets for a new agent. `<kind>-ask` and `<kind>-plan` always resolve. `<kind>-auto` and `<kind>-yolo` resolve only when the spec declares argv for that mode (`crates/rimz/src/harness/spec.rs::supported_virtual_agent_args`), so a provider without an auto switch has no `-auto` preset.

The optional `launch.headless` form is a [`HeadlessForm`](../../../crates/rimz/src/agents/headless.rs) with two methods. `render_argv` builds schema-enforced argv for a `HeadlessRequest`, and `read_result` normalizes captured output into a `HeadlessResult`: a JSON `verdict`, cost, tokens, and a typed error (`ProviderLimit` or `Other`, with text). The consumers are the [loop judge](../harness/loops.md#guard-chains), which decodes the strict `HeadlessVerdict`, and the [workflow judge](../harness/workflow.md#headless-judges), which checks the value against its own contract. Only Claude and Codex declare a form ([Claude](./adapter_claude.md#headless-form), [Codex](./adapter_codex.md#headless-form)). A headless request for any other kind fails at compile with `AgentProcessCompileErr::NoHeadless`, naming the kind (`crates/rimz/src/harness/launch.rs`).

`capabilities` is operational policy that no coverage claim can derive:

| Field | What it governs |
| --- | --- |
| `hook_context` | the reply shape (`HookSpecificOutput` or a `TopLevel` key) in which a native post-tool hook reply carries additive model context such as a deadline rung. `AgentDefinition::attach_hook_context` is its one writer, and `None` sends the rung through the pane |
| `prompt_context` | a native prompt-submit reply can carry additive model context ([below](#hook-stdout-is-the-decision-channel)). Claude and Codex declare it |
| `native_ask_ui` | the agent draws its own permission and question prompts, so RimZ can mark the row waiting and route you to the pane. Every built-in declares it, and a process plugin may not |
| `transcript_tail_context` | a local transcript tail is a live context source, refreshable on a producer tick |
| `registers_lazily` | a session can exist before its pane stamp does, so binding goes through the [recovery ladder](./instances.md#binding-a-session), and a stamp must also pass kind, activity, and process-start guards |
| `local_session_discovery` | session identity and lifecycle come from a provider-owned local store, without hooks |
| `daemon_hooked_sessions` | hooks fire from a per-user daemon that outlives any one conversation |
| `direct_account_usage` | an authoritative identity-bearing account-usage probe exists ([providers.md](./providers.md#refresh-cadences)) |
| `same_pane_session` | how co-resident open turns are ordered: `KeepPrimary` chooses the earliest registration and `FollowLatest` the latest. Rested roots always follow latest activity |
| `remote_control` | which remote-control surfaces the provider hosts (pane sessions, background sessions) |

### The traits

[`capabilities.rs`](../../../crates/rimz/src/agents/capabilities.rs) declares ten traits, bundled into one `AgentIntegration` blanket. Every method except `CoreCapability::spec` carries a default, and that default is the single home for "this agent does not do that". An adapter writes an empty `impl` for a trait it has no behaviour for, callers read the default answer, and no dispatch layer restates the gap.

The table names each trait's central methods. The trait file is the full list.

| Trait | Central methods | The default means |
| --- | --- | --- |
| `CoreCapability` | `spec()`, plus the test-only `conformance()` fixtures | `spec()` has no default. `conformance()` defaults to empty fixtures |
| `HookCapability` | `decode_hook`, `hook_ingress`, `payload_hook_reply`, `recognizes_hook_payload`, `ask_options`, `answer_plan`, subagent correlation and provider-settled child recovery, `attach_prompt_context` | every event classifies unknown and records nothing, the reply is silent, and no reply takes context |
| `InstallationCapability` | `managed_integration`, which backs install, preview, uninstall, detection, statusline wrapping, and installed-hook trust. `folder_trust` returns a decision and an exact grant preview for [Codex](./adapter_codex.md#directory-trust-preflight), [Claude](./adapter_claude.md#folder-trust), and [Qoder](./adapter_qoder.md#launch-preconditions-and-folder-trust) | hook installation is unavailable, and folder trust is not modeled |
| `LaunchCapability` | `is_interactive_process`, `launch_command`, `resume_command`, `launch_env`, `append_system_text_channel`, subagent argv and env lockdown, room-root grants, sandbox settings, `disable_native_sandbox_args` for providers that cannot nest under scoped isolation, `room_env`, `config_home` and `skills_home`, version probing | argv renders from the spec, no provider-native subagent lockdown applies, and a provider command sandbox stays as configured |
| `SessionCapability` | `discover_local_sessions`, `resumed_session_id_from_cmdline`, `local_conversation_present`, daemon evidence, turn-death refinement | no provider-owned session store to read |
| `TranscriptCapability` | `parse_transcript_messages`, `read_transcript_messages`, streaming pages, source positions | no transcript surface. JSONL adapters inherit the byte-cursor reads |
| `ContextCapability` | `observe_context`, `local_context_refresh`, `context_refresh_spawn`, `refresh_session_context`, `merge_session_context`, subagent context, local turn pricing | no context source ([context sources](#context-sources)) |
| `AccountCapability` | `probe_account`, `probe_account_usage`, `has_quota_surface` for a backing sub-provider, `resolve_managed_launch`, realtime usage, reset credits, `remote_control_status` | logged out, no account-usage surface, and no sub-provider quota surface (`false`) |
| `SpendingCapability` | `spending_sources`, `parse_spend`, `transcript_files`, `session_transcript`, `session_spend_transcripts` | no historical spend enters fleet aggregation |
| `RuntimeControlCapability` | remote-control readiness, preparation, liveness, and reconciliation | remote control is disabled |

`LaunchCapability::config_home` resolves a login's provider home from the env keys the adapter lists in `config_home_env_keys`. Any adapter can resolve a home this way, but only the kinds `AccountsConfig::named` lists (Claude and Codex) can be configured as named accounts ([providers.md](./providers.md)).

`LaunchCapability::skills_home` is the root for host-mode skill links. `agents::skill_links` plans and applies them, and uninstall resolves the same root for every native and declared login. A link is RimZ's when its target, resolved against the root and normalized lexically without canonicalizing, lies under the RimZ skill library. Every other entry is the user's and stays.

An adapter therefore reads top to bottom as a list of `impl` blocks. Grok is a compact example ([`adapters/grok/mod.rs`](../../../crates/rimz/src/agents/adapters/grok/mod.rs)): a spec, a `decode_hook`, a managed install source, a transcript parser, a local context refresh, an account probe, a spend parser, short launch and session impls, and an empty `RuntimeControlCapability`.

## The hook path

Hooks are how an agent reports itself. An installed hook runs `rimz hooks feed --source <agent>`. That frontend appends the native payload to the room's ingress log and returns the agent's reply. The elected per-room drainer later applies the frame in a child process, where the adapter decodes it and the store records the result ([runtime shape](../../../ARCHITECTURE.md#runtime-shape)).

```text
agent fires a hook
  │  RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude
  ▼
rimz hooks feed            ingress policy, room, payload, append, reply      (fast, on the agent's turn)
  │
  ▼  ingress frame
hook drainer               one rimz hooks apply child per frame, in order
  │  decode_hook(event, json) with the frame's captured environment
  ├── lifecycle ─────────► bind identity, record agent.lifecycle, run enrichment
  ├── awaiting-user ─────► record the ask when the agent has its own UI
  └── unknown ───────────► record nothing
```

### The feed

[`run_feed_inner`](../../../crates/rimz/src/cli/hooks.rs) runs these steps in order:

1. Ingress policy. `hook_ingress` reads `RIMZ_AGENT_PID` and decides whether the emitter is this adapter's agent, its daemon, or something to ignore. `Ignore` exits 0 before stdin is read.
2. Room. The feed resolves the room the agent's pane belongs to ([below](#hooks-resolve-the-room-they-live-in)).
3. Payload. The feed reads stdin as JSON. A payload that another adapter's `recognizes_hook_payload` claims exits 0 with nothing recorded and no reply, because Grok also runs hooks installed for other providers ([adapter_grok.md](./adapter_grok.md)). The event name comes from `--event` when the installed command passes one, then from the payload's `hook_event_name` or `hookEventName`.
4. Reply. `payload_hook_reply` computes the reply from the payload alone. It is silent for every built-in except Cursor and Antigravity, whose contracts require JSON.
5. Append. The feed appends a frame carrying the payload, the working directory, the captured emitter environment, and the room pin.
6. Hand off. For an Antigravity event or a Claude or Codex `UserPromptSubmit`, the feed waits up to 1.2 s (`REPLY_WAIT`) for the drainer's reply to that frame and prints it, falling back to the payload reply on a miss. Every other event nudges the drainer and prints the payload reply at once, and a `PostToolUse` may first gain a deadline rung (`attach_deadline_context`).

A hook inside a scoped sandbox view connects to the host drainer before it appends. When the host drainer is unreachable, the hook applies its frame inline in its own view and appends nothing.

The drainer applies frames serially, each in a fresh `rimz hooks apply` child with a 30-second deadline (`APPLY_TIMEOUT` in `crates/rimz/src/cli/hooks.rs`). `crates/rimz/src/harness/hook_drain.rs::wait_for_apply` kills and reaps a child that overruns. A failed or timed-out apply logs a warning and a `HookApplyFailed` diagnostic, and the cursor advances without retrying the frame, so a hung child cannot stall every later hook in the room. An inline drain's caller deadline only stops the next frame from starting: a started apply keeps its own timeout and writes its cursor even after the caller deadline. Election, the drainer socket, and crash recovery of a claimed frame are in [ARCHITECTURE.md](../../../ARCHITECTURE.md#runtime-shape).

### What a decoded event carries

`decode_hook` returns a [`HookOutput`](../../../crates/rimz/src/agents/hook_types.rs): one canonical event carrying a class (lifecycle, an ask of some `AskKind`, or unknown), a list of typed facts, routing, and an explicit `HookReply`. The facts are what the generic path consumes:

| Fact | Carries |
| --- | --- |
| `Lifecycle` | the [`AgentLifecycleObservation`](../../../crates/rimz/src/agents/observation.rs): one signal plus its enrichment |
| `Ask` | the question list and detail text for a blocking prompt |
| `NativeAnswers` | answers the agent recorded in its own UI |
| `AssistantOutput` / `FinalOutput` | streamed and final turn text, for transcripts and supervised runs |
| `Error` | a typed turn-death or park certificate |
| `Context` | a normalized [`AgentContext`](../../../crates/rimz/src/agents/context.rs) observation |
| `Progress` | the event proves work happened, so it touches the activity heartbeat |
| `SessionEnded` | the session is over |

An event with no `Lifecycle` fact is not a transition, which is how high-frequency events stay silent. The adapter never decides a status: it emits the signal, and [`step`](./model.md#the-state-machine) derives the status from it.

Every built-in classifies through [`decode_catalog_hook`](../../../crates/rimz/src/agents/hook_types.rs) instead of by hand. It takes any iterator of `HookEventSpec` references, the event name, and an optional `AskKind`, and returns a `HookOutput` carrying the matching entry's progress and session-ended policy. A supplied `AskKind` classifies the event as awaiting-user. Without one, a catalog entry marked `lifecycle_fallback` classifies as lifecycle, and any other event, named or not, classifies unknown. The iterator parameter lets a catalog whose rows wrap the spec with install data use the same entry point: Antigravity pairs each spec with its config event, matcher, and installed command, and decodes through `ANTIGRAVITY_HOOKS.iter().map(|entry| &entry.hook)`.

`SessionStart` is decided once, by [`SessionSource::session_start_signal`](../../../crates/rimz/src/agents/hook_types.rs): `compact` closes a compaction with `CompactionEnded`, and every other source, including `new` and an unrecognized one, is `Registered`. Claude, Codex, Droid, Qwen, Qoder, MiniMax, and Muse map the payload's `source` field through it. Kiro and Hermes, whose start events carry no source, map them through it with a fixed source.

Claude, Qwen, and Qoder take a permission ask's detail text from the shared `crates/rimz/src/agents/question.rs::permission_detail`, the line the sidebar card, `rimz agents show`, and `rimz asks` print. It is the tool name, then the first input field in `PERMISSION_TARGETS` that holds a nonblank string: a shell command, a file path, a URL, a search pattern or query, then a bare path. A path inside the payload's `cwd` prints relative to it. An input with no such field prints as compact JSON, so a tool RimZ does not know still names its arguments. The summary after the tool name stops at 160 characters and ends in `…` when cut. The other adapters pass their provider's own prompt text or no detail.

### Two channels

`decode_hook` sorts each native event into the lifecycle channel or the awaiting-user channel, or classifies it unknown, which records nothing. The two channels differ in whether the hook is a prompt the agent is holding open.

Lifecycle hooks are fast and non-blocking. They drive status, turn phase, task, and enrichment, and they return the neutral reply.

Awaiting-user hooks queue their payload and return neutral before the deferred state write. When the agent's spec declares `native_ask_ui`, the drainer turns a permission request, plan approval, or user question into an `awaiting_input` signal: the row goes `waiting` and the question text lands as a transcript `Ask` entry. The prompt stays visible in the agent's pane throughout. Without `native_ask_ui` the drainer records no ask, because there is no native prompt a waiting row could route you to. Every built-in declares the flag, and a process plugin can declare it off.

Blocking decision hooks install synchronous, because an async hook ignores the reply printed on stdout. The adapter's hook catalog marks which events must block, and the JSON-merge installer refuses, as a hard error, a config whose RimZ-owned entry for such an event is async (`crates/rimz/src/agents/managed_json_hooks.rs::ManagedJsonHookSpec::reject_async_blocking`).

### Hook stdout is the decision channel

A hook's stdout carries the agent-native neutral reply and nothing else, and the agent's own UI stays responsible for every decision. Three rules follow:

- Logs go to stderr or to RimZ runtime state logs such as `binding.log.jsonl`. The `print_stdout` lint gates this ([rust-conventions.md](../../contributing/rust-conventions.md)), and `emit_reply` is the one allowed print.
- Hook helper children get fresh, fully piped stdio, so a wrapped statusline's stderr or a notification helper's output never reaches the decision channel. The `ensure_hook_stdio` invariant rejects `Stdio::inherit`.
- Each adapter's tests pin its neutral shapes, as inline `insta` goldens for most adapters and as plain assertions for Antigravity.

What neutral looks like depends on the agent, so verify it for each one. `HookReply::Silent` prints nothing. For every built-in except Cursor and Antigravity that hands the prompt back to the native UI, and for Pi it lets the tools run and its `ask_user_question` extension open its questionnaire. Cursor and Antigravity return `HookReply::Json` on every wired event because their hook contracts require JSON, and Cursor's is `{}`.

Additive model context is the one thing RimZ puts on that channel beyond neutral, and it decides nothing. An adapter that declares `prompt_context` answers a root prompt submit with the native context reply (`hookSpecificOutput.additionalContext`), which carries the [prompt-submit Environment block](../harness/fleet.md#environment-at-prompt-submit), and the prompt proceeds as typed. `conformance.rs` holds the flag to the adapter's replies, and each supporting adapter goldens the exact line. The feed's reply wait is keyed by kind name, so a third adapter that declares `prompt_context` also needs its kind added to `run_feed_inner`, or its context never reaches stdout. A post-tool deadline rung rides the reply shape `hook_context` declares.

### Hooks resolve the room they live in

A hook fires deep inside an agent's process tree, and it has to find the room that agent's pane belongs to. It resolves as a participant ([`WorkspaceResolver`](../../../crates/rimz/src/workspace.rs)): the session's identity pin (`RIMZ_WORKSPACE_ID` and `RIMZ_PROJECT_ROOT`, stamped into the multiplexer environment at room birth) wins over re-deriving identity from the current directory, so an agent working inside a nested repo still writes to the room its pane lives in. The pin is hash-verified. A pin that fails verification falls to sibling-pin recovery and then to the static ladder (git root, marker file, directory), because a hook on the agent's critical path degrades on identity rather than failing. The `ensure_participant_identity` invariant keeps the create-mode resolver out of participant surfaces. Room-choosing commands resolve statically, so a deliberate per-repo room can still be created from inside a parent room.

A daemon-routed hook cannot trust its environment. Codex fires hooks from the shared app-server, which inherits the environment of whichever room launched the daemon, so an ambient workspace or pane pin can name the wrong room. When `hook_ingress` classifies the emitter as a daemon, `rimz hooks feed` ignores the env pin and recovers the pin from a sibling agent process instead: the daemon spawns hooks in the session's working directory, and the in-pane agent process sharing that directory carries the right room identity. Each candidate is verified like an env pin and adopted only when every candidate names one root. A split scan falls to the static ladder.

| Owner | Resolution order |
| --- | --- |
| Pane-owned (`resolve_participant_with_pin_recovery`) | `--root`, env pin, recovered sibling pin, static ladder |
| Daemon-owned (`resolve_daemon_participant_with_pin_recovery`) | `--root`, recovered sibling pin, static ladder |

A hook outside a recorded room creates nothing. `rimz hooks feed` prints the adapter's payload-only reply without decoding the event, warns on stderr with the resolved root and `rimz start` as the fix, and exits 0. A drainer that cannot be nudged after the frame is appended is also a stderr warning: the feed drains inline, warns again if that fails, prints its reply, and exits 0. Missing RimZ state must not break the agent's turn or put diagnostics on its decision channel.

That resolution is also the trap in driving a hook by hand. Feeding a payload to `rimz hooks feed` from a pane inside a live room writes into that room whatever `HOME` says: the identity pin and the multiplexer detection come from the environment and the process tree, not from `HOME`, so the fake session registers as a real agent row beside the ones you are working with. Clear the `RIMZ_*` pin and the `ZELLIJ*` / `TMUX*` detection variables along with the home and runtime directories. The integration harness is the worked example (`crates/rimz/tests/integration/common`, `ScrubSessionEnvExt`, which every `Env` applies before it spawns `rimz`).

## Hook install

Installing hooks edits the agent's own config, so it is a visible security step. `rimz hooks install --dry-run` prints a per-agent summary and a unified diff without writing, and the commands are in [hooks-trust.md](../../reference/cli/hooks-trust.md#agent-hooks). `rimz start` checks every detected agent on each run and asks once for all agents whose hooks are missing or have an upgrade available: Enter installs or refreshes every listed agent, and `n` or EOF changes nothing. An unattended start prints a notice and installs nothing ([`ensure_detected_agent_hooks`](../../../crates/rimz/src/cli/hooks/hook_install.rs)).

An adapter declares one `managed_integration`, and that single interface drives install, preview, uninstall, detection, partial-artifact cleanup, upgrades, wiring inputs, wrapped statuslines, and provider-native installed-hook trust. The shared [`ManagedSource`](../../../crates/rimz/src/agents/managed_source.rs) does a JSON merge or writes a whole file RimZ authors, and the adapters build on it in three ways:

| Integration | Adapters |
| --- | --- |
| A `ManagedSource` is the integration | Claude, Droid, Qwen, Amp, Pi |
| A thin wrapper around a `ManagedSource` adds an install blocker, a version floor, or a hooks-disabled check | Qoder, Grok, Kiro, OpenCode, OMP |
| The adapter's own `install.rs` runs a TOML or YAML rewrite, a multi-file transaction, or a native plugin package, using a `ManagedSource` for one file where it fits | Codex, Copilot, Cursor, Kimi, Antigravity, Hermes, Muse, MiniMax |

Copilot, Cursor, and Antigravity report their per-file rows in order through the shared [`install_report.rs`](../../../crates/rimz/src/agents/adapters/install_report.rs). Kiro's wrapper refuses a Kiro CLI older than 2.31.0 and reclaims an unmarked file from an older RimZ install. MiniMax owns one real native package directory and publishes its manifest last, and detection compares all embedded file bytes. Its hook runner scrubs the launch environment, so the feed restores the verified owner's environment before resolving RimZ paths ([adapter_minimax.md](./adapter_minimax.md)).

An adapter whose install is refused on this machine or in the login's provider home reports it through `install_blocker` before any file is touched: OpenCode, Grok (its managed-hooks policy), Hermes, Kimi, Qoder, Kiro, and Muse implement one. Detected installs (`rimz start`, `rimz setup`, bare `rimz hooks install`) skip a blocked adapter, while an install that names it still refuses. A successful version probe behind a blocker is reused per binary file per process, so detection, preview, and install cost one spawn unless the binary changes. A failed probe stays retryable.

Install wires every event the state machine needs (the turn-boundary signals) plus the high-frequency per-tool events that keep enrichment current, and each adapter's hook catalog constant is the source of truth for that set. For the JSON-merge adapters, detection walks the whole catalog, so an under-wired config reports not installed and `rimz start` offers the idempotent merge again. An agent that runs before its hooks land gets no row, unless its adapter declares `local_session_discovery`, which admits sessions from the provider's local store (`crates/rimz/src/sidebar/agent_projection.rs::probe_adapters`). `rimz doctor` reports the install state.

Inside whatever shape the agent's config takes, the installed form stays minimal:

- One feed command for every event, commonly `RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source <agent>`, with `--event <event>` appended when the agent omits the event name from its payload. Muse uses a shell-neutral `rimz hooks feed --source muse` and managed env passthrough ([adapter_muse.md](./adapter_muse.md#managed-installation-and-identity)).
- Install reclaims every RimZ-owned entry before rewriting the canonical set, so duplicate or stale blocks never accumulate and user-authored hooks stay untouched. JSON-merge entries are owned by a `_rimz_managed` marker key ([`managed_json_hooks.rs`](../../../crates/rimz/src/agents/managed_json_hooks.rs)), and an unmarked entry counts as RimZ's when its command contains the hook command substring.
- Claude, Codex, Droid, Qwen, Qoder, and Grok have no wildcard event key, so install writes one block per wired event. Copilot, Amp, Pi, OMP, and OpenCode instead get one whole integration file that RimZ authors, so the payload schema is RimZ's by design ([adapter_copilot.md](./adapter_copilot.md), [adapter_amp.md](./adapter_amp.md), [adapter_pi.md](./adapter_pi.md), [adapter_omp.md](./adapter_omp.md), [adapter_opencode.md](./adapter_opencode.md)).

Installed provider hooks are outside RimZ's project-config executable-surface hash. The `hooks` field in `crates/rimz/src/trust.rs::ExecutableSurface` covers project `[[hooks]]`, not provider settings, and installation diffs expose provider-file changes for consent ([trust.md](../harness/trust.md)). Hook payloads can carry prompts, tool inputs, and file paths. No config filters that content, and the hook path forwards none of it off the machine ([security.md](../../guide/security.md#what-leaves-your-machine)).

## Context sources

A session's context gauge says how full the window is and what the turn cost. An adapter fills it from none, one, or several sources (Qoder and Muse have none), and every source normalizes onto the same [`AgentContext`](../../../crates/rimz/src/agents/context.rs) fields, so nothing downstream depends on which one fed the row. A failed read leaves a field unset and never fails a hook. Most context is display data. The turn-error, settle, and attention markers are the exception: they feed displayed status, and [model.md](./model.md#rich-context) owns how.

Four `ContextCapability` methods feed the gauge:

| Method | Runs | Implemented by |
| --- | --- | --- |
| `observe_context(source, payload)` | inline, on a payload pushed to RimZ: a statusline's stdin (`rimz statusline`) or a hook envelope | Claude, Antigravity, Qwen, Copilot, Cursor, Pi, OMP, OpenCode, and the plugin adapter |
| `local_context_refresh(trigger, ctx)` | inline, on a hook (`RefreshTrigger::Hook`), a producer tick (`Tick`), or transcript growth (`Watch`), for a cheap local read | Claude, Codex, Copilot, Cursor, Droid, Grok, Kimi, Kiro |
| `context_refresh_spawn` with `refresh_session_context` | in the detached `rimz agents refresh-context` helper, for a source that needs network, a subprocess, or a broker connection | Codex (its app-server) |
| `merge_session_context` | under the sidecar record lock, folding a rich reading from that helper onto the stored record | Codex |

A pushed payload carries what a local read cannot derive (rate-limit windows, account plan, PR info, model display name, version) on the provider's own cadence. Claude pushes statusline JSON. Pi's and OMP's extensions stamp their in-process context APIs onto every envelope. OpenCode's plugin keeps its gauge from paired `session.step.started` and `session.step.ended` or `session.step.failed` events and stamps the latest split plus the model's window onto each lifecycle envelope. Hermes's native `post_api_request` hook carries the request's prompt tokens, window, and model, so its adapter stamps the gauge from that payload and reads no tail.

A local refresh returns explicit keep, set, clear, and token-merge patches. The store applies each patch under the sidecar record lock and owns persistence only, so merge policy stays in the adapter. For Claude the tail is a low-frequency fallback, because the statusline owns the live reading. For Codex the rollout tail is the live token, cost, and effort source: progress hooks and the host fetch worker run a stat-gated refresh. On a turn-ended hook whose refresh produced no dollar total, the shared hook path prices the session store through `session_transcript` and the spend parser, unless the adapter declares `live$` unsupported (`supplement_realtime_cost` in [`cli/hooks/lifecycle/context.rs`](../../../crates/rimz/src/cli/hooks/lifecycle/context.rs)).

The detached helper keeps network and subprocess work off the agent's turn. `context_refresh_spawn` returns argv for `rimz agents refresh-context`, the caller spawns it with nulled stdio and never waits, and `refresh_session_context` is the body of that helper. Codex's app-server builds its observation directly, without `observe_context`.

The unchanged-source gate on a local refresh is shared. The adapter resolves its source path and the stat that stands for it: usually `TranscriptStat::from_path`, and for Grok one composite stat over the transcript and its `summary.json` and `signals.json` companions. It passes that stat to [`LocalContextRefreshCtx::changed_transcript`](../../../crates/rimz/src/agents/mod.rs), which returns it only when it differs from the stat the sidecar recorded and otherwise ends the refresh before any read. Copilot, Cursor, Grok, Kimi, and Kiro gate there. Three adapters keep their own gate because they sometimes must re-read a file whose stat is unchanged: Claude when its spend fold resets (a truncated file, or a stored fold older than the dedup window), Codex when a live fold still needs token-counter backfill, and Droid, whose telemetry read compares a composite settings-plus-transcript stat.

### Reading rules

A transcript tail is read through the provider-agnostic [`read_transcript_tail`](../../../crates/rimz/src/agents/transcript_fs.rs) or `read_transcript_tail_with_status`, and a parser on top of it follows these rules:

- Bounded. The reader returns the trailing 64 KiB, so a multi-megabyte log never stalls a hook. When the newest record is larger than that window, the read extends back to the record's start and returns it whole.
- Whole records only. The reader drops a partial leading line cut by the seek and a torn final record still being written, so parsers see only complete JSONL lines.
- Newest first. Scan lines in reverse, take the most recent usage record, and stop as soon as the needed records are in hand.
- Forgiving. Decode as lossy UTF-8. Any IO or parse failure yields empty fields.
- Zero is not unknown. Report an explicit zero for a fresh session only when the whole file was read and holds no usage. A bounded tail with no usage, or a transcript that cannot be read, stays `None`: no reading, so the fold carries the last known value rather than resetting it.

Two adapters depart from the tail. Grok folds its whole transcript behind the stat gate (`crates/rimz/src/agents/adapters/grok/transcript.rs::read`), and Copilot picks the newest telemetry record by timestamp rather than by scan order (`crates/rimz/src/agents/adapters/copilot/otel.rs::latest_chat_usage`).

## Declared coverage

Every adapter declares what it supports in three records on its spec. Each record has one named field per item, so an omission is a compile error, and [`conformance.rs`](../../../crates/rimz/src/agents/conformance.rs) cross-checks the claims against the adapter's capabilities, installed hook events, classification corpus, and spend fixture.

| Record | One field per | Arms |
| --- | --- | --- |
| `user_coverage` ([`UserCoverage`](../../../crates/rimz/src/agents/definition.rs)) | `UserCapability`: what the person watching the sidebar gets (6) | `Full { note }`, `Partial { shows, limit }`, `Unsupported { reason }` |
| `coverage` ([`CoverageAnnotations`](../../../crates/rimz/src/agents/definition.rs)) | `IntegrationConcern`: what the adapter reads from the agent (19) | `Wired { via }`, `Partial { via, gap }`, `Unsupported { reason }` |
| `lifecycle_hooks` (`LifecycleAnnotations`) | `LifecycleSignalKind`: the native event behind each signal (11) | `Native { event }`, `Derived { via, gap }`, `Absent { reason }` |

`rimz coverage` prints the capability grid with a detail table under it. `--wiring` adds the concern grid and the lifecycle-hook grid, each with its own detail table. Every detail cell carries its text, and a partial cell prints both halves (`via` with `gap`, `shows` with `limit`).

```console
$ rimz coverage --wiring
RimZ coverage

WHAT EACH AGENT GIVES YOU
AGENT        state  live  history  account  ask  subagents
claude       ✓      ✓     ✓        ✓        ✓    ✓
codex        ✓      ✓     ✓        ✓        ✓    ✓
...
kiro         !      !     !        ✗        !    ✗
...
qoder        ✓      ✗     ✗        ✗        ✓    !
  legend ✓ full   ! partial   ✗ unsupported

DETAIL
...

WIRING — INTEGRATION CONCERNS
AGENT        turn  perm  plan  ask  answer  compact  sub  remind  bg  bgsh  end  idle  usage  live$  rich  install  spend  tools  remote
claude       ✓     ✓     ✓     ✓    ✓       ✓        ✓    ✓       ✓   ✓     ✓    ✓     ✓      ✓      ✓     ✓        ✓      ✓      ✓
codex        ✓     ✓     ✓     ✓    ✓       ✓        ✓    ✓       ✗   ✗     !    !     ✓      ✓      ✓     ✓        ✓      !      ✓
...
kiro         ✓     !     ✗     !    ✗       ✗        ✗    ✗       ✗   ✗     ✓    !     !      ✗      !     ✓        ✗      ✗      ✗
...
qoder        ✓     ✓     ✓     ✓    ✗       ✓        ✓    ✓       ✗   ✗     ✓    ✓     ✗      ✗      ✗     ✓        ✗      ✗      ✗
  legend ✓ wired   ! partial   ✗ unsupported
...
WIRING — LIFECYCLE HOOKS
AGENT        registered  turn_started  turn_ended  tool_used  awaiting_input  subagent_started  subagent_stopped  compacting  compaction_ended  ended  lost
...
qoder        ✓           ✓             ✓           ✓          ✓               ✓                 ✓                 ✓           ✓                 ✓      !
  legend ✓ native   ! derived   ✗ absent
```

The meaning of each concern, and the published matrices, are in [agent-support.md](../../reference/agent-support.md#the-wiring-matrix).

### Concern claims

A concern is `Wired` when it reaches a user-complete state, `Partial` when native coverage is incomplete and RimZ reconstructs the rest, and `Unsupported` when no inference reaches it from the current protocol surface. Reserve `Partial` for a surface the user can still see is missing something. A value RimZ reconciles to its authoritative figure at every turn boundary is `Wired` even without a continuous native push, which is why Pi and OpenCode claim `live$` wired: the running cost is pushed on the hook wire, and the turn-end signal settles it to the session spend. Codex claims `end` and `idle` partial because no per-session end or idle hook exists: pane liveness plus the reaper reconstruct end, and turn boundaries, the ask path, and the stall window cover the attention part of idle. Cursor claims `compact` partial because `preCompact` opens natively and the next lifecycle signal derives the close.

Conformance grounds some concerns in the adapter's own methods. `remind` must be `Wired` exactly when `append_system_text_channel` returns a channel, meaning RimZ-composed launch reminders can reach the agent's system or developer prompt, and it can never be `Partial`.

### The user-capability declaration

`user_coverage` states what the user gets, and these are the marks the [compatibility matrix](../../reference/agent-support.md#the-compatibility-matrix) prints. `Full` means complete and live, reading the way it does on Claude Code. `Partial` is a working version with a stated limit: part of the detail, or all of it late. `Unsupported` means there is nothing to render.

How RimZ obtains a figure does not affect its mark. A value folded from a transcript tail and a value pushed by a native hook both read `Full` when the surface is complete and current, and a native signal carrying half the story reads `Partial`. The strings print verbatim to users through `rimz coverage`, so they are product language: lowercase, no trailing period, roughly six to fourteen words, phrased as what the card shows. The rubric that fixes each mark per capability is [agent-support.md](../../reference/agent-support.md#what-the-marks-mean). Write against its ladders rather than inventing a rung.

`user_capabilities_are_complete_and_grounded` in [`conformance.rs`](../../../crates/rimz/src/agents/conformance.rs) links the two records in one direction, through the concerns behind each capability:

| Capability | Backing concerns |
| --- | --- |
| `state` | `turn_lifecycle` |
| `live` | `context_usage`, `realtime_cost` |
| `history` | `account_spend` |
| `ask` | `permission`, `plan_approval`, `user_question` (any one suffices) |
| `subagents` | `subagents` |

A `Full` mark needs every backing concern wired, except `ask`, which needs one: a single blocking path reaching `rimz asks` is the whole user-visible claim. An `Unsupported` mark needs every backing concern unsupported. Between those bounds the roll-up is the adapter's judgement: mark `Partial` when the user-visible result is still incomplete or late, and say which in `limit`. `account` has no concern mapping, because its truth is the provider probe the account fixtures cover ([providers.md](./providers.md)). Every arm carries non-empty text, and a `Partial` names what does land as well as what is missing.

A declared absence reads as one wherever it surfaces: `rimz coverage` prints every reason, and `rimz doctor`'s hook row prints an unsupported hook install with its reason (`crates/rimz/src/cli/doctor/agents.rs`). Nobody has to debug it as an accidental gap.

## Adding an agent

A third-party agent normally ships as a [process plugin](../../reference/agent-plugins.md): one machine-tier manifest, an agent-side shim speaking the canonical envelope, optional probes, and no RimZ source change. A built-in is warranted when RimZ must own a native config migration (a hook installer that writes the agent's own config) or a protocol surface the canonical wire cannot express (an out-of-band rich-context transport, a bespoke ask-answer path).

A built-in lands as one private directory under [`adapters/`](../../../crates/rimz/src/agents/adapters/mod.rs), one `registry::BUILTINS` entry, conformance coverage, and its own adapter page. The directory layout is consistent across kinds:

| File | Holds |
| --- | --- |
| `mod.rs` | the unit-struct adapter, its `static AgentSpec`, and every capability `impl` |
| `payloads.rs` | typed structs for the native wire, parsed structurally rather than by digging through `Value` |
| `install.rs` | the managed integration, when a bare `ManagedSource` does not fit |
| `account.rs`, `oauth_usage.rs` | the login probe and the account-usage query ([providers.md](./providers.md)) |
| `spend.rs` | the read-only full-history cost parser |
| `transcript.rs` | native transcript normalization |
| `tests.rs` or `tests/` | the conformance corpus and the stdout goldens |

`spend.rs` is sidebar-safe by construction: the `ensure_spend_parser_boundaries` invariant rejects store-write, run-wake, and broker imports in any spend path.

Because the observation an adapter emits is agent-agnostic, a new kind inherits the state machine, ranking, liveness, attention routing, messaging, supervised runs, and the sidebar row. The exceptions are the kind-string branches listed under [the one rule](#the-one-rule). The sequenced playbook, from protocol reference to landed adapter with its deliverables checklist, is [agent-adapters.md](../../contributing/agent-adapters.md). The authoring contract is [`crates/rimz/src/agents/AGENTS.md`](../../../crates/rimz/src/agents/AGENTS.md).

### Extending the signal vocabulary

Adding a [`LifecycleSignal`](../../../crates/rimz/src/agents/lifecycle.rs) variant is deliberately harder than adding an adapter, because every variant costs an edge in one shared transition table. A new provider-observed variant needs both a concrete native event on a shipping provider that no existing variant plus enrichment expresses, and a distinct `(status, phase)` edge in `step`, landed with its edge test and the totality test extended. Anything short of both is enrichment on an existing signal.

`CompactionEnded` and `TurnInterrupted` are the worked examples. `TurnInterrupted` lands a canceled turn at idle without a false success or failure, and Cursor's aborted stop and Pi's aborted settled outcome share it. A variant need not add a `LifecycleSignalKind`: `TurnInterrupted` declares coverage under `turn_ended`.

## See also

- [model.md](./model.md): what happens to an observation: the rollup, the state machine, and the displayed-status projection.
- [instances.md](./instances.md): how a session binds to its pane, including lazy registration.
- [providers.md](./providers.md): the account and balance half of an integration.
- [spending.md](./spending.md): the spend parser's consumer and the price book.
- [plugin.md](./plugin.md): the third-party process-plugin manifest, wire, and probes.
- [agent-adapters.md](../../contributing/agent-adapters.md): the step-by-step integration playbook and deliverables checklist.
- [agent-support.md](../../reference/agent-support.md): the published capability, wiring, and lifecycle-hook matrices.
- [adapter_claude.md](./adapter_claude.md) and its seventeen siblings: per-kind native mappings.
- [claude-reference.md](../../externals/agent-adapter/claude-reference.md) and its siblings: the raw upstream protocols adapters read, pinned to source URLs.
