# The adapter layer

Thirteen built-in coding agents report to RimZ, and no code outside `crates/rimz/src/agents/` knows which one it is looking at. This page owns the seam that makes that true: what an adapter is, the contracts it implements, the path a native hook event takes from the agent's process to the durable store, and how each adapter declares what it supports.

Read [model.md](./model.md) first. It defines the [`AgentLifecycleObservation`](../../../crates/rimz/src/agents/observation.rs) an adapter produces and the rollup that consumes it; this page covers the producing side. What each native event means for one agent is on that agent's page ([adapter_claude.md](./adapter_claude.md), [adapter_codex.md](./adapter_codex.md), and eleven siblings). Accounts and spend are in [providers.md](./providers.md), and the third-party process-plugin wire is in [plugin.md](./plugin.md).

## The one rule

An adapter is the single place one agent's protocol is normalized.

```text
native protocol            the adapter boundary              generic RimZ
─────────────────          ────────────────────              ────────────
hook JSON payload   ──►                              ──►  AgentLifecycleObservation
transcript JSONL    ──►    adapters/<kind>/          ──►  AgentContext · TranscriptMessage
auth files, APIs    ──►    (private module)          ──►  AgentAccount · RateLimitWindow
provider CLI argv   ──►                              ──►  argv · SpendEntry · AccountProbe
```

Everything left of the boundary is provider knowledge, and everything right of it is agent-agnostic, so the sidebar, the harness, the store, and the CLI carry no per-agent match arms. Two mechanisms keep it that way:

- Compiler privacy. The `adapters` module is private to `agents`, and the `ensure_private_agent_adapter_boundary` check in `cargo xtask invariants` fails a build where code outside `agents` names a provider module or a concrete adapter type.
- One dispatch point. [`registry.rs`](../../../crates/rimz/src/agents/registry.rs) resolves a kind string to an [`AgentDefinition`](../../../crates/rimz/src/agents/definition.rs). Callers use the definition's methods and never learn which struct is behind it.

## The registry

[`registry::BUILTINS`](../../../crates/rimz/src/agents/registry.rs) holds one `AgentDefinition` per compiled-in adapter, in display order: `claude`, `codex`, `amp`, `copilot`, `kimi`, `pi`, `opencode`, `antigravity`, `cursor`, `droid`, `kiro`, `qwen`, `grok`. `all_definitions()` chains that slice with the validated machine-tier [process plugins](./plugin.md), and every lookup goes through it:

| Function | Answers |
| --- | --- |
| `find_definition(kind)` / `definition_by_kind(kind)` | the definition for a `--source <agent>` tag, matching the spec's `kind` or one of its `aliases`; the second returns an error for an unknown kind |
| `spec_by_kind(kind)` | the same lookup for callers that need only const data (branding, tool tables) |
| `known_kinds()` | kinds in display order, for doctor, wiring probes, and coverage |
| `command_agent_kind(command)` | which agent a pane's command line is running, after shell and launcher normalization |
| `resumed_session_id_from_cmdline(cmdline)` | the session a `<agent> --resume <id>` process reopened; abstains when two adapters match |
| `room_env(runtime)` | the merged adapter environment a new room exports to the multiplexer as one opaque map |

`command_agent_kind` decides whether a pane is running an agent at all. It matches the resolved program basename against each spec's `bin_names`, accepting a target-triple release suffix: `codex-aarch64-apple-darwin` names `codex`, and so does its 15-character kernel `comm` truncation. When the program is a launcher such as `node`, matching falls back to the launcher's script path. Without a program match it tries the process `comm`, and a `comm` two adapters claim abstains. A matched command then passes the adapter's `is_interactive_process`, which is how `codex app-server` and `codex remote-control start` stay out of the room while `codex` enters it. How a pane whose multiplexer reports only `node` gets classified is in [instances.md](./instances.md#recognizing-a-hosted-cli).

## Two halves: the spec and the traits

Each adapter is a private unit struct under [`adapters/<kind>/`](../../../crates/rimz/src/agents/adapters/mod.rs) that carries one `const` [`AgentSpec`](../../../crates/rimz/src/agents/definition.rs) and implements ten capability traits. Everything knowable at compile time is spec data; everything that reads a file or parses a payload is a trait method.

### The spec: const facts

| Group | Fields |
| --- | --- |
| Identity | `kind`, `aliases`, `display_name` |
| Presentation | `brand` (emblem, 256-color index, RGB), `plan_label`, `expected_windows` |
| Process | `process_names` (kernel `comm` candidates, including launchers), `bin_names` (`$PATH` probe order), `bin_identity` (the check that keeps an ambiguous binary name, such as Cursor's `agent`, from resolving to another provider's install), `extra_bin_dirs` |
| Vocabulary | `tools.input_key` (the payload key holding tool arguments), `tools.mutating`, `tools.editing` (a validated subset of mutating), `tools.blocking` (tool name paired with its [`AskKind`](../../../crates/rimz/src/agents/lifecycle.rs)) |
| Policy | `capabilities` (below) |
| Claims | `coverage`, `user_coverage`, `lifecycle_hooks` ([declared coverage](#declared-coverage)) |
| Defaults | `default_model`, `default_context_window`, `sub_providers`, `thread_key` |
| Launch | `launch`: program, fixed args, prompt style, resume and fork argv, per-mode permission args, max-turn flag, compact command, preset matchers |

The launch block is data so that one registry entry is enough to light up `<kind>-auto`, `<kind>-ask`, `<kind>-plan`, `<kind>-yolo`, `rimz agents restart`, and `agents.toml` profile rendering for a new agent. Permission argv, resume shape, and preset flag spellings need no code elsewhere.

`capabilities` is operational policy that no coverage claim can derive:

| Field | What it governs |
| --- | --- |
| `native_ask_ui` | the agent draws its own permission and question prompts, so RimZ can mark the row waiting and route you to the pane; every built-in declares it, and a process plugin may not |
| `transcript_tail_context` | a local transcript tail is a live context source, refreshable on a producer tick |
| `registers_lazily` | a session can exist before its pane stamp does, so binding goes through the [recovery ladder](./instances.md#binding-a-session) |
| `local_session_discovery` | session identity and lifecycle come from a provider-owned local store, without hooks |
| `daemon_hooked_sessions` | hooks fire from a per-user daemon that outlives any one conversation |
| `direct_account_usage` | an authoritative identity-bearing account-usage probe exists ([providers.md](./providers.md#refresh-cadences)) |
| `same_pane_session` | how co-resident open turns are ordered: `KeepPrimary` chooses the earliest registration and `FollowLatest` the latest; rested roots always follow latest activity |
| `remote_control` | which remote-control surfaces the provider hosts (pane sessions, background sessions) |

### The traits: behavior

[`capabilities.rs`](../../../crates/rimz/src/agents/capabilities.rs) declares ten traits, bundled into one `AgentIntegration` blanket. Every method except `spec()` carries a default, and that default is the single home for "this agent does not do that". An adapter writes an empty `impl` for a trait it has no behavior for, callers read the default answer, and no dispatch layer restates the gap.

The table names each trait's central methods; the trait file is the full list.

| Trait | Central methods | The default means |
| --- | --- | --- |
| `CoreCapability` | `spec()`, plus the test-only conformance fixtures | required, no default |
| `HookCapability` | `decode_hook`, `hook_ingress`, `ask_options`, `answer_plan`, subagent correlation and provider-settled child recovery | every event classifies unknown and records nothing |
| `InstallationCapability` | `managed_integration`, which backs install, preview, uninstall, detection, statusline wrapping, and installed-hook trust; `launch_dir_trust_gap` for providers that gate a launch directory ([Codex](./adapter_codex.md#directory-trust-preflight)) | hook installation is unavailable, and no launch directory is gated |
| `LaunchCapability` | `is_interactive_process`, `launch_command`, `resume_command`, `launch_env`, `append_system_text_channel`, subagent argv and env lockdown, `room_env`, `config_home` and `skills_home`, version probing | argv renders from the spec, and no provider-native subagent lockdown applies |
| `SessionCapability` | `discover_local_sessions`, `resumed_session_id_from_cmdline`, `local_conversation_present`, daemon evidence, turn-death refinement | no provider-owned session store to read |
| `TranscriptCapability` | `parse_transcript_messages`, `read_transcript_messages`, streaming pages, source positions | no transcript surface; JSONL adapters inherit the byte-cursor reads |
| `ContextCapability` | `observe_context`, `local_context_refresh`, `context_refresh_spawn`, `refresh_session_context`, subagent context, local turn pricing | no out-of-band context source |
| `AccountCapability` | `probe_account`, `probe_account_usage`, `resolve_managed_launch`, realtime usage, reset credits, `remote_control_status` | logged out, and no account-usage surface |
| `SpendingCapability` | `spending_sources`, `parse_spend`, `transcript_files`, `session_transcript`, `session_spend_transcripts` | no historical spend enters fleet aggregation |
| `RuntimeControlCapability` | remote-control readiness, preparation, liveness, and reconciliation | remote control is disabled |

An adapter therefore reads top to bottom as a list of `impl` blocks. Grok is a compact example ([`adapters/grok/mod.rs`](../../../crates/rimz/src/agents/adapters/grok/mod.rs)): a spec, a `decode_hook`, a managed install source, a transcript parser, a local context refresh, an account probe, a spend parser, one-method launch and session impls, and an empty `RuntimeControlCapability`.

## The hook path

Hooks are how an agent reports itself. An installed hook runs one command, `rimz hooks feed --source <agent>`, and that process is the whole ingestion path ([`run_feed`](../../../crates/rimz/src/cli/hooks.rs)).

```text
agent fires a hook
  │  RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude
  ▼
hook_ingress(pid)          adapter: is this emitter mine, and is it the agent or its daemon?
  ▼
read stdin as JSON         the native payload
  ▼
resolve the workspace      participant resolution (below)
  ▼
decode_hook(event, json)   adapter: native payload → HookOutput
  ▼
   ├── lifecycle channel ──► bind identity, record agent.lifecycle, run enrichment
   └── awaiting-user channel ► record the ask when the agent has its own UI
  ▼
emit HookReply on stdout   the agent-native neutral reply, and nothing else
```

An ingress decision of `Ignore` ends the process before stdin is read. The event name comes from `--event` when the installed command passes one, and otherwise from the payload's `hook_event_name` (or `hookEventName`).

`decode_hook` returns a [`HookOutput`](../../../crates/rimz/src/agents/hook_types.rs): one canonical event carrying a meaning (lifecycle, an ask of some `AskKind`, or unknown), a list of typed facts, routing, and an explicit `HookReply`. The facts are what the generic path consumes:

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

Adapters with a hook catalog decode through [`decode_catalog_hook`](../../../crates/rimz/src/agents/hook_types.rs) instead of classifying by hand. It takes any iterator of `HookEventSpec` references, the event name, and an optional `AskKind`, and returns a `HookOutput` carrying the matching entry's progress and session-ended policy. A supplied `AskKind` classifies the event as awaiting-user. Without one, a catalog entry marked `lifecycle_fallback` classifies as lifecycle, and any other event, named or not, classifies unknown. The iterator parameter lets a catalog whose rows wrap the spec with install data use the same entry point: Antigravity pairs each spec with its config event, matcher, and installed command, and decodes through `ANTIGRAVITY_HOOKS.iter().map(|entry| &entry.hook)`.

`SessionStart` is decided once for every adapter that sees it. Claude, Codex, Droid, and Qwen carry the same `source` field, and [`SessionSource::session_start_signal`](../../../crates/rimz/src/agents/hook_types.rs) maps it: `compact` closes a compaction with `CompactionEnded`, and every other source, including an unrecognized one, is `Registered`.

### Two channels

`decode_hook` sorts every native event into one of two channels, and they differ in whether the hook is a prompt the agent is holding open.

Lifecycle hooks are fast and non-blocking. They drive status, turn phase, task, and enrichment, and they return the neutral reply.

Awaiting-user hooks record the waiting state and return neutral. When the agent's spec declares `native_ask_ui`, a permission request, plan approval, or user question becomes an `awaiting_input` signal: the row goes `waiting`, the question text lands as a transcript `Ask` entry, the neutral reply returns at once, and the prompt stays visible in the agent's pane. Without `native_ask_ui` the hook records nothing and returns the same neutral reply, because there is no native prompt a waiting row could route you to. Every built-in declares the flag; a process plugin can declare it off.

Blocking decision hooks install synchronous. An async one would ignore the reply printed on stdout, so the installer rejects it as a hard error, reading the adapter's own hook catalog rather than the on-disk config.

### Hook stdout is the decision channel

This is the canonical statement of the rule the rest of the docs link to. A hook's stdout carries the agent-native neutral reply and nothing else, and the agent's own UI stays responsible for every decision. Three rules follow:

- Logs go to stderr or to RimZ runtime state logs such as `binding.log.jsonl`. The `print_stdout` lint gates this ([rust-conventions.md](../../contributing/rust-conventions.md)), and `emit_reply` is the one allowed print.
- Hook helper children get fresh, fully piped stdio, so a wrapped statusline's stderr or a notification helper's output never reaches the decision channel. The `ensure_hook_stdio` invariant rejects `Stdio::inherit`.
- Every neutral shape has an inline `insta` golden in the adapter's tests.

What neutral looks like depends on the agent, so verify it for each one. `HookReply::Silent` prints nothing, which hands the prompt back to the native UI for Claude, Codex, Droid, and most others, and lets Pi's tools run and its `ask_user_question` extension open its questionnaire. Cursor and Antigravity return `HookReply::Json` on every wired event because their hook contracts require JSON; Cursor's is `{}`.

### Hooks resolve the room they live in

A hook fires deep inside an agent's process tree, and it has to find the room that agent's pane belongs to. It resolves as a participant ([`WorkspaceResolver`](../../../crates/rimz/src/workspace.rs)): the session's identity pin (`RIMZ_WORKSPACE_ID` and `RIMZ_PROJECT_ROOT`, stamped into the multiplexer environment at room birth) wins over re-deriving identity from the current directory, so an agent working inside a nested repo still writes to the room its pane lives in. The pin is hash-verified. A mismatch falls through to the static ladder (git root, marker file, directory), because a hook on the agent's critical path degrades on identity rather than failing. The `ensure_participant_identity` invariant keeps the create-mode resolver out of participant surfaces; room-choosing commands resolve statically, so a deliberate per-repo room can still be created from inside a parent room.

A daemon-routed hook cannot trust its environment. Codex fires hooks from the shared app-server, which inherits the environment of whichever room launched the daemon, so an ambient workspace or pane pin can name the wrong room. When `hook_ingress` classifies the emitter as a daemon, `rimz hooks feed` ignores the env pin and recovers the pin from a sibling agent process instead: the daemon spawns hooks in the session's working directory, and the in-pane agent process sharing that directory carries the right room identity. Each candidate is verified like an env pin and adopted only when every candidate names one root; a split scan falls to the static ladder.

| Owner | Resolution order |
| --- | --- |
| Pane-owned (`resolve_participant_with_pin_recovery`) | `--root`, env pin, recovered sibling pin, static ladder |
| Daemon-owned (`resolve_daemon_participant_with_pin_recovery`) | `--root`, recovered sibling pin, static ladder |

## Hook install

Installing hooks edits the agent's own config, so it is a visible security step. `rimz hooks install --dry-run` prints a per-agent summary and a unified diff without writing, and the commands are in [hooks-trust.md](../../reference/cli/hooks-trust.md#agent-hooks). `rimz start` checks every detected agent on each run and asks once for all agents whose hooks are missing or have an upgrade available: Enter installs or refreshes every listed agent, and `n` or EOF changes nothing. An unattended start prints a notice and installs nothing ([`ensure_detected_agent_hooks`](../../../crates/rimz/src/cli/hooks/hook_install.rs)).

An adapter declares one `managed_integration`, and that single interface drives install, preview, uninstall, detection, partial-artifact cleanup, upgrades, wiring inputs, wrapped statuslines, and installed-hook trust. Two backends implement it:

| Backend | Adapters |
| --- | --- |
| Shared [`ManagedSource`](../../../crates/rimz/src/agents/managed_source.rs): a JSON merge, or a whole file RimZ authors | Claude, Droid, Qwen, Grok, Amp, Pi, OpenCode |
| The adapter's own `install.rs`: a TOML rewrite or a multi-file transaction | Codex, Cursor, Copilot, Kimi, Antigravity, Kiro |

Copilot, Cursor, and Antigravity report their per-file rows in order through the shared [`install_report.rs`](../../../crates/rimz/src/agents/adapters/install_report.rs). Kiro installs a whole marked file through the shared managed source, after refusing a Kiro CLI older than 2.13.0 and reclaiming the unmarked file earlier RimZ builds wrote.

Install wires every event the state machine needs (the turn-boundary signals) plus the high-frequency per-tool events that keep enrichment current, and each adapter's hook catalog constant is the source of truth for that set. For the JSON-merge adapters, detection walks the whole catalog, so an under-wired config reports not installed and `rimz start` offers the idempotent merge again. An agent that runs before its hooks land is therefore invisible to RimZ rather than half-tracked, and `rimz doctor` reports the install state.

Inside whatever shape the agent's config takes, the installed form stays minimal:

- One command for every event: `RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source <agent>`, with `--event <event>` appended when the agent omits the event name from its payload.
- Install reclaims every RimZ-owned entry before rewriting the canonical set, so duplicate or stale blocks never accumulate and user-authored hooks stay untouched. JSON-merge entries are owned by a `_rimz_managed` marker key ([`managed_json_hooks.rs`](../../../crates/rimz/src/agents/managed_json_hooks.rs)); an unmarked entry counts as RimZ's when its command contains the hook command substring.
- Claude, Codex, Droid, Qwen, and Grok have no wildcard event key, so install writes one block per wired event. Copilot, Amp, Pi, and OpenCode instead get one whole integration file that RimZ authors, so the payload schema is RimZ's by design ([adapter_copilot.md](./adapter_copilot.md), [adapter_amp.md](./adapter_amp.md), [adapter_pi.md](./adapter_pi.md), [adapter_opencode.md](./adapter_opencode.md)).

Every hook command enters the executable-surface hash, so a tampered hook config demotes project trust to stale ([trust.md](../harness/trust.md)). Hook payloads can carry prompts, tool inputs, and file paths; no config filters that content today, and the planned `[privacy]` controls are described in [security.md](../../guide/security.md).

## Context sources

A session's context gauge (how full the window is, what the turn cost) is what no agent puts in its hook JSON. An adapter offers one to three sources, and any one alone is valid. All three normalize onto the same fields, so the rest of RimZ does not depend on the choice. Enrichment is never correctness: a missing file, a torn line, or an absent binary each leaves a field unset, never a failed hook or a wrong decision.

The transcript or store tail is the universal floor. Every provider keeps a local usage store its spend parser already understands (JSONL for Claude, Codex, Grok, and Pi; SQLite for OpenCode). For Claude the tail is a low-frequency fallback, because the statusline owns the live reading. For Codex the rollout tail is the live token, cost, and effort source: progress hooks and the elected snapshot producer run a stat-gated refresh that reads a bounded tail only when the file's stat changes. On a turn-ended hook whose refresh produced no dollar total, the shared hook path prices the session store through `session_transcript` and the spend parser, unless the adapter declares `live$` unsupported (`supplement_realtime_cost` in [`cli/hooks/lifecycle/context.rs`](../../../crates/rimz/src/cli/hooks/lifecycle/context.rs)).

A rich out-of-band transport is the provider-specific upgrade where one exists. It carries what a local read cannot derive (rate-limit windows, account plan, PR info, model display name, version) on the provider's own cadence. Claude pushes statusline JSON; Codex reads read-only app-server methods. Transport payloads normalize through `observe_context` into one typed session observation whose [`AgentContext`](../../../crates/rimz/src/agents/context.rs) carries no identity.

A gauge stamped on the hook wire is available to any provider whose hook wire RimZ authors. Pi's extension stamps its in-process context API onto every envelope. OpenCode's plugin maintains its gauge from `message.updated` events and stamps the latest split plus the model's window onto each lifecycle envelope. Neither needs a tail or a transport for the gauge.

Two methods attach refreshes to the hook path. `local_context_refresh` takes a [`RefreshTrigger`](../../../crates/rimz/src/agents/mod.rs) (`Hook`, `Tick`, or `Watch` for transcript growth) and returns explicit keep, set, clear, and token-merge patches from a cheap bounded local read that runs inline. `context_refresh_spawn` returns argv for the detached `rimz agents refresh-context` helper when the provider's source needs network, a subprocess, or a broker connection; the caller spawns it with nulled stdio and never waits, so it adds no latency to the agent's turn, and `refresh_session_context` is the body of that helper. The store applies each patch under the sidecar record lock and owns persistence only; merge policy stays in the adapter.

The unchanged-source gate on a local refresh is shared. The adapter resolves its source path and the stat that stands for it: usually `TranscriptStat::from_path`, and for Grok one composite stat over the transcript and its `summary.json`, `signals.json`, and events companions. It passes that stat to [`LocalContextRefreshCtx::changed_transcript`](../../../crates/rimz/src/agents/mod.rs), which returns it only when it differs from the stat the sidecar recorded and otherwise ends the refresh before any read. Amp, Copilot, Cursor, Grok, Kimi, and Kiro gate there. Three adapters keep their own gate because they sometimes must re-read a file whose stat is unchanged: Claude when its spend fold resets (a truncated file, or a stored fold older than the dedup window), Codex when a live fold still needs token-counter backfill, and Droid, whose telemetry read compares a composite settings-plus-transcript stat.

### Reading rules

The tail reader is provider-agnostic ([`read_transcript_tail`](../../../crates/rimz/src/agents/transcript_fs.rs)), and every adapter parses on top of it under the same rules:

- Bounded. Read the trailing 64 KB, so a multi-megabyte log never stalls a hook. When the newest record is larger than that window, the read extends back to the record's start and returns it whole.
- Whole records only. The reader drops a partial leading line cut by the seek and a torn final record still being written, so parsers see only complete JSONL lines.
- Newest first. Scan lines in reverse, take the most recent usage record, and stop as soon as the needed records are in hand.
- Forgiving. Decode as lossy UTF-8; any IO or parse failure yields empty fields.
- Zero is not unknown. A transcript that opens cleanly but carries no usage yet is a fresh session: report an explicit `0%` so the bar draws empty. A transcript that cannot be read stays `None`, meaning the agent did not report it.

## Declared coverage

Every adapter declares what it supports in three records on its spec. Each record has one named field per item, so an omission is a compile error, and [`conformance.rs`](../../../crates/rimz/src/agents/conformance.rs) cross-checks the claims against the adapter's capabilities, installed hook events, classification corpus, and spend fixture.

| Record | One field per | Arms |
| --- | --- | --- |
| `user_coverage` ([`UserCoverage`](../../../crates/rimz/src/agents/definition.rs)) | `UserCapability`: what the person watching the sidebar gets (6) | `Full { note }`, `Partial { shows, limit }`, `Unsupported { reason }` |
| `coverage` ([`CoverageAnnotations`](../../../crates/rimz/src/agents/definition.rs)) | `IntegrationConcern`: what the adapter reads from the agent (18) | `Wired { via }`, `Partial { via, gap }`, `Unsupported { reason }` |
| `lifecycle_hooks` (`LifecycleAnnotations`) | `LifecycleSignalKind`: the native event behind each signal (11) | `Native { event }`, `Derived { via, gap }`, `Absent { reason }` |

`rimz coverage` prints the capability grid with a detail table under it; `--wiring` adds the concern grid and the lifecycle-hook grid, each with its own detail table. Every detail cell carries its text, and a partial cell prints both halves (`via` with `gap`, `shows` with `limit`).

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
  legend ✓ full   ! partial   ✗ unsupported

DETAIL
...

WIRING — INTEGRATION CONCERNS
AGENT        turn  perm  plan  ask  answer  compact  sub  remind  bg  end  idle  usage  live$  rich  install  spend  tools  remote
claude       ✓     ✓     ✓     ✓    ✓       ✓        ✓    ✓       ✓   ✓    ✓     ✓      ✓      ✓     ✓        ✓      ✓      ✓
codex        ✓     ✓     ✓     ✓    ✓       ✓        ✓    ✓       ✗   !    !     ✓      ✓      ✓     ✓        ✓      !      ✓
...
kiro         !     !     ✗     ✗    ✗       ✗        ✗    ✗       ✗   !    !     !      ✗      ✗     ✗        ✗      ✗      ✗
...
  legend ✓ wired   ! partial   ✗ unsupported
...
WIRING — LIFECYCLE HOOKS
...
  legend ✓ native   ! derived   ✗ absent
```

The meaning of each concern, and the published matrices, are in [agent-support.md](../../reference/agent-support.md#the-wiring-matrix).

### Concern claims

A concern is `Wired` when it reaches a user-complete state, `Partial` when native coverage is incomplete and RimZ reconstructs the rest, and `Unsupported` when no inference reaches it from the current protocol surface. Reserve `Partial` for a surface the user can still see is missing something. A value RimZ reconciles to its authoritative figure at every turn boundary is `Wired` even without a continuous native push, which is why Pi and OpenCode claim `live$` wired: the running cost is pushed on the hook wire, and the turn-end signal settles it to the session spend. Codex claims `end` and `idle` partial because no per-session end or idle hook exists: pane liveness plus the reaper reconstruct end, and turn boundaries, the ask path, and the stall window cover the attention part of idle. Cursor claims `compact` partial because `preCompact` opens natively and the next lifecycle signal derives the close.

Conformance grounds some concerns in the adapter's own methods. `remind` must be `Wired` exactly when `append_system_text_channel` returns a channel, meaning RimZ-composed launch reminders can reach the agent's system or developer prompt, and it can never be `Partial`.

### The user-capability declaration

`user_coverage` states what the user gets, and these are the marks the [compatibility matrix](../../reference/agent-support.md#the-compatibility-matrix) prints. `Full` means complete and live, reading the way it does on Claude Code. `Partial` is a working version with a stated limit: part of the detail, or all of it late. `Unsupported` means there is nothing to render.

How RimZ obtains a figure does not affect its mark. A value folded from a transcript tail and a value pushed by a native hook both read `Full` when the surface is complete and current, and a native signal carrying half the story reads `Partial`. The strings print verbatim to users through `rimz coverage`, so they are product language: lowercase, no trailing period, roughly six to fourteen words, phrased as what the card shows. The rubric that fixes each mark per capability is [agent-support.md](../../reference/agent-support.md#what-the-marks-mean); write against its ladders rather than inventing a rung.

`user_capabilities_are_complete_and_grounded` in [`conformance.rs`](../../../crates/rimz/src/agents/conformance.rs) links the two records in one direction, through the concerns behind each capability:

| Capability | Backing concerns |
| --- | --- |
| `state` | `turn_lifecycle` |
| `live` | `context_usage`, `realtime_cost` |
| `history` | `account_spend` |
| `ask` | `permission`, `plan_approval`, `user_question` (any one suffices) |
| `subagents` | `subagents` |

A `Full` mark needs every backing concern wired, except `ask`, which needs one: a single blocking path reaching `rimz asks` is the whole user-visible claim. An `Unsupported` mark needs every backing concern unsupported. Between those bounds the roll-up is the adapter's judgement: mark `Partial` when the user-visible result is still incomplete or late, and say which in `limit`. `account` has no concern mapping, because its truth is the provider probe the account fixtures cover ([providers.md](./providers.md)). Every arm carries non-empty text, and a `Partial` names what does land as well as what is missing.

A declared absence renders as a declared absence, in the sidebar and in `rimz doctor`, so nobody debugs it as an accidental gap.

## Adding an agent

A third-party agent normally ships as a [process plugin](../../reference/agent-plugins.md): one machine-tier manifest, an agent-side shim speaking the canonical envelope, optional probes, and no RimZ source change. A built-in is warranted when RimZ must own a native config migration (a hook installer that writes the agent's own config) or a protocol surface the canonical wire cannot express (an out-of-band rich-context transport, a bespoke ask-answer path).

A built-in lands as one private directory under [`adapters/`](../../../crates/rimz/src/agents/adapters/mod.rs), one `registry::BUILTINS` entry, conformance coverage, and its own adapter page. The directory layout is consistent across kinds:

| File | Holds |
| --- | --- |
| `mod.rs` | the unit-struct adapter, its `const AgentSpec`, and every capability `impl` |
| `payloads.rs` | typed structs for the native wire, parsed structurally rather than by digging through `Value` |
| `install.rs` | the managed integration, when the shared `ManagedSource` backends do not fit |
| `account.rs`, `oauth_usage.rs` | the login probe and the account-usage query ([providers.md](./providers.md)) |
| `spend.rs` | the read-only full-history cost parser |
| `transcript.rs` | native transcript normalization |
| `tests.rs` or `tests/` | the conformance corpus and the inline `insta` stdout goldens |

`spend.rs` is sidebar-safe by construction: the `ensure_spend_parser_boundaries` invariant rejects store-write, run-wake, and broker imports in any spend path.

Because the observation an adapter emits is agent-agnostic, a new kind inherits the state machine, ranking, liveness, attention routing, messaging, supervised runs, and the sidebar row without anything downstream learning it exists. The sequenced playbook, from protocol reference to landed adapter with its deliverables checklist, is [agent-adapters.md](../../contributing/agent-adapters.md). The authoring contract is [`crates/rimz/src/agents/AGENTS.md`](../../../crates/rimz/src/agents/AGENTS.md).

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
- [adapter_claude.md](./adapter_claude.md) and its twelve siblings: per-kind native mappings.
- [claude-reference.md](../../externals/agent-adapter/claude-reference.md) and its siblings: the raw upstream protocols adapters read, pinned to source URLs.
