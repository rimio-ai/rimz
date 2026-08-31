# The adapter layer

Thirteen coding agents report to RimZ, and no code outside `crates/rimz/src/agents/` knows which one it is looking at. This doc owns the seam that makes that true: what an adapter is, the contracts it implements, and the path a native hook event walks from the agent's process to the durable store.

Read [model.md](./model.md) first if you have not: it defines the [`AgentLifecycleObservation`](../../../crates/rimz/src/agents/observation.rs) an adapter produces and the rollup that consumes it. This doc picks up on the producing side. Which native event means what for one agent is that agent's own page ([adapter_claude.md](./adapter_claude.md), [adapter_codex.md](./adapter_codex.md), and eleven siblings); the account and spend half lives in [providers.md](./providers.md); the third-party process-plugin wire is [plugin.md](./plugin.md).

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

Everything left of the boundary is provider knowledge. Everything right of it is agent-agnostic, so the sidebar, the harness, the store, and the CLI grow no per-agent match arms. Two mechanisms hold the seam shut:

- Compiler privacy. `agents::adapters` is private, and `cargo xtask invariants` fails a build where a generic module reaches for a concrete adapter type.
- One dispatch point. [`registry.rs`](../../../crates/rimz/src/agents/registry.rs) resolves a kind string to an [`AgentDefinition`](../../../crates/rimz/src/agents/definition.rs); callers use the definition's methods and never learn which struct is behind it.

## The registry

[`registry::BUILTINS`](../../../crates/rimz/src/agents/registry.rs) is one `AgentDefinition` per compiled-in adapter, in display order: `claude`, `codex`, `amp`, `copilot`, `kimi`, `pi`, `opencode`, `antigravity`, `cursor`, `droid`, `kiro`, `qwen`, `grok`. `all_definitions()` chains that slice with validated machine-tier [process plugins](./plugin.md), and every lookup goes through it:

| Function | Answers |
| --- | --- |
| `find_definition(kind)` / `definition_by_kind(kind)` | the `--source <agent>` tag, matching the spec's `kind` or one of its `aliases` |
| `spec_by_kind(kind)` | the same lookup for callers that need only const data (branding, tool tables) |
| `known_kinds()` | display-order iteration for doctor, wiring probes, and coverage |
| `command_agent_kind(command)` | which agent a pane's command line is running, after shell and launcher normalization |
| `resumed_session_id_from_cmdline(cmdline)` | the session a `<agent> --resume <id>` process reopened |

`command_agent_kind` is the one that earns explanation. A pane reports a command line, and RimZ has to decide whether an agent is running in it. The registry matches the resolved program basename against each spec's `bin_names`, accepting a target-triple release suffix (`codex-aarch64-apple-darwin` still names `codex`, including under the kernel's 15-character `comm` truncation). When the program is a launcher such as `node`, it falls back to the launcher's script path, then to the process `comm`, and a `comm` matched by two adapters abstains rather than guessing. A matched command still passes through `is_interactive_process`, which is how `codex app-server` and `codex remote-control start` stay out of the room while `codex` enters it.

## Two halves: the spec and the traits

Each adapter is a private unit struct under [`adapters/<kind>/`](../../../crates/rimz/src/agents/adapters/mod.rs) that carries one `const` [`AgentSpec`](../../../crates/rimz/src/agents/definition.rs) and implements ten capability traits. The split is: everything knowable at compile time is spec data, everything that reads a file or parses a payload is a trait method.

### The spec: const facts

| Group | Fields |
| --- | --- |
| Identity | `kind`, `aliases`, `display_name` |
| Presentation | `brand` (emblem, 256-color index, RGB), `plan_label`, `expected_windows` |
| Process | `process_names` (kernel `comm` candidates, including launchers), `bin_names` (`$PATH` probe order), `extra_bin_dirs` |
| Vocabulary | `tools.mutating`, `tools.editing` (a validated subset of mutating), `tools.blocking` (tool name paired with its [`AskKind`](../../../crates/rimz/src/agents/lifecycle.rs)) |
| Policy | `capabilities` (below) |
| Claims | `coverage`, `user_coverage`, `lifecycle_hooks` ([declared coverage](#declared-coverage)) |
| Defaults | `default_model`, `default_context_window`, `sub_providers`, `thread_key` |
| Launch | `launch`: program, fixed args, prompt style, resume and fork argv, per-mode permission args, max-turn flag, compact command, preset matchers |

The launch block is declarative on purpose. Because permission argv, resume shape, and preset flag spellings are data, one registry entry is what lights up `<kind>-auto`, `<kind>-ask`, `<kind>-plan`, `<kind>-yolo`, `rimz agents restart`, and `agents.toml` profile rendering for the new agent, with no code elsewhere.

`capabilities` is the operational policy that no coverage claim can derive:

| Field | What it governs |
| --- | --- |
| `native_ask_ui` | the agent draws its own permission and question prompts, so RimZ can mark the row waiting and route you to the pane |
| `transcript_tail_context` | a local transcript tail is a live context source, refreshable on a producer tick |
| `registers_lazily` | a session can exist before its pane stamp does, so binding goes through the [recovery ladder](./model.md#the-instance-lifecycle) |
| `local_session_discovery` | session identity and lifecycle come from a provider-owned local store rather than hooks |
| `daemon_hooked_sessions` | hooks fire from a per-user daemon that outlives any one conversation |
| `direct_account_usage` | an authoritative identity-bearing account-usage probe exists ([providers.md](./providers.md#refresh-cadences)) |
| `same_pane_session` | how co-resident open turns are ordered: `KeepPrimary` chooses the earliest registration and `FollowLatest` the latest; rested roots always follow latest activity |
| `remote_control` | which remote-control surfaces the provider hosts (pane sessions, background sessions) |

### The traits: behavior

[`capabilities.rs`](../../../crates/rimz/src/agents/capabilities.rs) declares ten traits, bundled into one `AgentIntegration` blanket. Every method carries a default, and that default is the single home for "this agent does not do that": an adapter writes an empty `impl` for a capability it has no behavior for, and the workflow reads as the default answer. No dispatch layer restates a gap.

| Trait | Owns | The default means |
| --- | --- | --- |
| `CoreCapability` | `spec()`, plus the test-only conformance fixtures | required, no default |
| `HookCapability` | `decode_hook`, `hook_ingress`, ask options and answer plans, subagent correlation and provider-settled child recovery | no native decoder: every event classifies unknown and records nothing |
| `InstallationCapability` | `managed_integration` and the install, preview, uninstall, detection, trust, and statusline-wrap surface it drives | hook installation is unavailable for this agent |
| `LaunchCapability` | `is_interactive_process`, `launch_command`, `resume_command`, `launch_env`, subagent argv/env lockdown, `room_env`, version probing | render argv from the spec and apply no provider-native subagent lockdown |
| `SessionCapability` | local-session discovery, resume-identity parsing, daemon evidence, turn-death refinement | no provider-owned session store to read |
| `TranscriptCapability` | `parse_transcript_messages`, streaming pages, source positions | no transcript surface; JSONL adapters inherit the byte-cursor implementation |
| `ContextCapability` | `observe_context`, `local_context_refresh`, `context_refresh_spawn`, local turn pricing | no out-of-band context source |
| `AccountCapability` | `probe_account`, `probe_account_usage`, realtime usage, reset credits, remote-control state | logged out, and no account-usage surface |
| `SpendingCapability` | `spending_sources`, `parse_spend`, `transcript_files`, `session_transcript`, `session_spend_transcripts` | no historical spend enters fleet aggregation |
| `RuntimeControlCapability` | remote-control readiness, host argv, reconciliation | remote control is disabled |

An adapter is therefore readable top to bottom as a list of `impl` blocks, and a gap is visible as an empty one. Grok is the compact worked example: a spec, a `decode_hook`, an install source, a transcript parser, a local context refresh, an account probe, a spend parser, and empty impls for the rest ([`adapters/grok/mod.rs`](../../../crates/rimz/src/agents/adapters/grok/mod.rs)).

## The hook path

Hooks are how an agent reports itself. An installed hook runs one command, `rimz hooks feed --source <agent>`, and that process is the whole ingestion path ([`cli/hooks.rs`](../../../crates/rimz/src/cli/hooks.rs)).

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
emit HookReply on stdout   the agent-native neutral no-op, and nothing else
```

`decode_hook` returns a [`HookOutput`](../../../crates/rimz/src/agents/hook_types.rs): one canonical event carrying a meaning (lifecycle, an ask of some `AskKind`, or unknown), a list of typed facts, stable routing, and an explicit reply. The facts are what the generic path consumes:

| Fact | Carries |
| --- | --- |
| `Lifecycle` | the [`AgentLifecycleObservation`](../../../crates/rimz/src/agents/observation.rs): one signal plus its enrichment |
| `Ask` | the question list and detail text for a blocking prompt |
| `NativeAnswers` | answers the agent recorded in its own UI |
| `AssistantOutput` / `FinalOutput` | streamed and final turn text, for transcripts and supervised runs |
| `Error` | a typed turn-death or park certificate |
| `Context` | a normalized [`AgentContext`](../../../crates/rimz/src/agents/context.rs) observation |
| `Progress` | this event proves work happened, so it touches the activity heartbeat |
| `SessionEnded` | the session is over |

An absent lifecycle fact means "no transition here", which is how high-frequency events stay silent. The adapter never decides a status: it emits the signal, and [`step`](./model.md#the-state-machine) derives the status from it.

### Two channels

`decode_hook` sorts every native event into one of two wired channels, and the difference is whether the hook can hold the agent open while RimZ answers.

Lifecycle hooks are fast and non-blocking. They drive status, turn phase, task, and enrichment, and they return silently.

Awaiting-user hooks record the waiting state and return neutral. A permission request, plan approval, or user question becomes an `awaiting_input` signal when the agent has its own ask UI: the row goes `waiting`, the question text lands as a transcript `Ask` entry, the agent-native no-op returns at once, and the prompt stays visible in the agent's pane. An agent whose spec declares `native_ask_ui` off (Pi) gets the same neutral no-op with no waiting observation, because there is no native prompt a `?` row could route you to.

Blocking decision hooks install synchronous. An async one would ignore the decision printed on stdout, so the installer rejects it as a hard error, reading the adapter's own hook catalog rather than the on-disk config.

### Hook stdout is the decision channel

This is the canonical statement of the rule the rest of the docs link to. A hook's stdout carries the agent-native neutral reply and nothing else, and the agent's own UI stays responsible for every decision. Three consequences:

- Logs go to stderr or to RimZ runtime state logs such as `binding.log.jsonl`. The `print_stdout` lint gates this ([rust-conventions.md](../../contributing/rust-conventions.md)).
- Hook helper children get fresh, fully piped stdio. A wrapped statusline's stderr or a notification helper's chatter must never reach the decision channel, and a CI grep rejects `Stdio::inherit` in hook paths.
- Every neutral shape is a golden test, including the agent-native no-op.

Neutral semantics diverge per agent, so verify rather than assume. Empty stdout hands the prompt back to Claude's, Codex's, and Droid's own UI; Cursor's contract requires JSON, so it returns `{}` on every wired event; and for Pi, which has no native prompt to fall back to, empty stdout *is* the allow.

### Hooks resolve the room they live in

A hook fires deep inside an agent's process tree, and it has to find the room that agent's pane belongs to. It resolves as a participant ([`WorkspaceResolver::resolve_participant`](../../../crates/rimz/src/workspace.rs)): the session's identity pin (`RIMZ_WORKSPACE_ID` and `RIMZ_PROJECT_ROOT`, stamped into the mux environment at room birth) wins over re-deriving identity from the current directory, so an agent working inside a nested repo still writes to the room its pane lives in. The pin is hash-verified, and a mismatch falls through to the static ladder (git root, marker file, directory), because a hook on the agent's critical path degrades on identity rather than failing. A CI grep keeps the create-mode resolver out of participant surfaces; room-choosing commands resolve statically, so a deliberate per-repo room can still be created from inside a parent room.

A daemon-routed hook is the interesting case. Codex fires its hooks from the shared app-server, which inherits the daemon's environment rather than the pane's, so an ambient workspace or pane pin can name the room that happened to launch the daemon. `rimz hooks feed` drops both pins for daemon-owned events and recovers the workspace pin from a sibling agent process instead: the daemon spawns hooks with the session's own working directory, so the in-pane agent process sharing that directory carries the right room identity in its environment. Each candidate is verified like a pane-owned pin and adopted only when every candidate names one root; a split scan degrades to the static ladder. The resolution orders are:

| Owner | Order |
| --- | --- |
| Pane-owned | `--root`, env pin, recovered sibling pin, static ladder |
| Daemon-owned | `--root`, recovered sibling pin, static ladder |

## Hook install

Installing hooks edits the agent's own config, so it is a visible security step rather than a silent one.

```sh
rimz hooks install --dry-run    # per-agent summary plus a unified diff; writes nothing
rimz hooks install              # every detected supported agent on PATH
rimz hooks uninstall            # removes RimZ-managed hooks, even if the agent binary is gone
rimz doctor                     # reports install state per agent
```

`rimz start` detects installed supported agents on each run and prints one consent prompt covering every missing agent, naming the config path, the additive impact, the undo command, and an example hook command. Enter installs all listed agents; `n` or EOF installs nothing. `hooks_installed()` makes the resulting state observable, so an agent run before its hooks land is invisible rather than silently broken.

An adapter declares one `managed_integration`, and that single Interface drives install, preview, uninstall, detection, partial-artifact cleanup, upgrades, wiring inputs, wrapped statuslines, and installed-hook trust. Adapters whose config is a JSON merge or a whole file RimZ authors return a shared [`ManagedSource`](../../../crates/rimz/src/agents/managed_source.rs) (Claude, Droid, Qwen, Grok, Amp, Pi, OpenCode). Adapters needing a TOML rewrite or a multi-file transaction implement the same Interface in their own `install.rs` (Codex, Cursor, Copilot, Kimi, Antigravity). Kiro is the one adapter whose Interface only uninstalls: its install and preview arms return the unavailable error, so `rimz hooks uninstall` still cleans up a legacy install while `rimz coverage` shows `install ✗`.

What install wires is every event the state machine needs (the turn-boundary signals) plus the high-frequency per-tool events that keep enrichment and audit depth current. Each adapter's hook catalog constant is the source of truth for that set. Detection requires the full canonical set, so an under-wired config re-offers the idempotent merge.

Inside whatever shape the agent's config takes, the installed form stays minimal:

- One command for every event: `RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source <agent>`. The helper reads the event name from the payload's `hook_event_name`, or the installed command passes `--event <event>` when the agent omits that field.
- Install reclaims every RimZ-owned entry by the stable command substring `rimz hooks feed --source <agent>`, then rewrites the canonical set, so duplicate or stale blocks never accumulate and user-authored hooks stay untouched.
- Claude, Codex, Droid, Qwen, and Grok have no wildcard event key, so install writes one block per wired event. Copilot, Amp, Pi, and OpenCode instead own one whole integration file that RimZ authors, so the payload schema is RimZ's by design ([adapter_copilot.md](./adapter_copilot.md), [adapter_amp.md](./adapter_amp.md), [adapter_pi.md](./adapter_pi.md), [adapter_opencode.md](./adapter_opencode.md)).

Every hook command enters the executable-surface hash, so a tampered hook config demotes project trust to stale ([trust.md](../harness/trust.md)). Per-tool payload *content* is gated by `[privacy] payload_mode` ([configuration.md](../../guide/configuration.md#sidecars-and-privacy)); the gate strips content, never whether a transition is observed.

## Context sources

A session's context gauge (how full the window is, what the turn cost) is the one thing no agent puts in its hook JSON. An adapter offers one to three sources, and any one alone is valid. All three normalize onto the same fields, so the rest of RimZ is unchanged by the choice. Enrichment is never correctness: a missing file, a torn line, or an absent binary each degrades to an omitted field, never a failed hook or a wrong decision.

**The transcript or store tail** is the universal floor. Every provider keeps a local usage store that its spend parser already understands (JSONL for Claude, Codex, Grok, and Pi; SQLite for OpenCode), so the row can derive a session dollar total from the same `parse_spend` path that feeds history. For Claude this is a low-frequency fallback because the statusline owns the live reading. For Codex the rollout tail is also the native live token, cost, and effort source: progress hooks and the elected snapshot producer run a stat-gated refresh that reads a bounded tail only when `(mtime, nanos, len)` changes. For Pi and OpenCode, a turn-ended signal resolves the current session store and sums its spend entries.

**A rich out-of-band transport** is the provider-specific upgrade where one exists. It carries what a local read cannot derive (rate-limit windows, account plan, PR info, model display name, version) on the provider's own cadence. Claude pushes statusline JSON; Codex reads read-only app-server methods. Transport payloads normalize through `observe_context` into one typed session observation whose [`AgentContext`](../../../crates/rimz/src/agents/context.rs) stays identity-free.

**A gauge stamped on the hook wire** is available to any provider whose hook wire RimZ authors. Pi's extension stamps its in-process context API onto every envelope; OpenCode's plugin maintains its gauge from `message.updated` events and stamps the latest split plus the model's window onto each lifecycle envelope. Neither then needs a tail or a transport.

Two trigger methods hang the refreshes off the hook path. `local_context_refresh(RefreshTrigger::Hook | Tick)` returns explicit keep, set, clear, and token-merge patches from a cheap bounded local read that runs inline. `context_refresh_spawn` returns argv for a detached `rimz` helper when the provider's transport needs network, a subprocess, or a broker connection: the caller spawns it with fully nulled stdio and never waits, so it adds no latency to the agent's turn. Store applies each patch under the sidecar record lock and owns persistence only; merge policy stays in the adapter.

### Reading rules

The tail reader is provider-agnostic ([`read_transcript_tail`](../../../crates/rimz/src/agents/transcript_fs.rs)), and every adapter parses on top of it under the same rules:

- Bounded. Read at most the trailing 64 KB, so a multi-megabyte log never stalls a hook.
- Newest first. Scan lines in reverse and take the most recent usage record; a truncated leading line from the seek fails to parse and is skipped.
- Stop when found. Bail as soon as the needed records are in hand.
- Lossy and forgiving. Decode as lossy UTF-8; any IO or parse failure yields empty fields.
- Zero is not unknown. A transcript that opens cleanly but carries no usage yet is a fresh session: report an explicit `0%` so the bar draws empty. A transcript that cannot be read stays `None`, meaning "the agent did not report it".

## Declared coverage

Every adapter declares what it supports, in three compile-time-complete records on its spec. Named fields make an omission a compile error, and [`conformance.rs`](../../../crates/rimz/src/agents/conformance.rs) cross-checks each claim against the adapter's capabilities, installed hook events, classification corpus, and spend fixture. `rimz coverage` leads with the user-facing capability grid and `rimz coverage --wiring` adds the mechanism beneath it, every cell carrying its reason:

```console
$ rimz coverage --wiring
RimZ coverage

WHAT EACH AGENT GIVES YOU
AGENT        state  live  history  account  ask  subagents
claude       ✓      ✓     ✓        ✓        ✓    ✓
antigravity  ✓      !     !        ✓        !    !
...
kiro         !      !     !        ✗        !    ✗
  legend ✓ full   ! partial   ✗ unsupported

WIRING — INTEGRATION CONCERNS
AGENT        turn  perm  plan  ask  answer  compact  sub  bg  end  idle  usage  live$  rich  install  spend  remote
claude       ✓     ✓     ✓     ✓    ✓       ✓        ✓    ✓   ✓    ✓     ✓      ✓      ✓     ✓        ✓      ✓
codex        ✓     ✓     ✓     ✓    ✓       ✓        ✓    ✗   !    !     ✓      ✓      ✓     ✓        ✓      ✓
...
kiro         !     !     ✗     ✗    ✗       ✗        ✗    ✗   !    !     !      ✗      ✗     ✗        ✗      ✗
  legend ✓ wired   ! partial   ✗ unsupported
```

[`CoverageAnnotations`](../../../crates/rimz/src/agents/definition.rs) has one field per `IntegrationConcern`, sixteen in all (`turn`, `perm`, `plan`, `ask`, `answer`, `compact`, `sub`, `bg`, `end`, `idle`, `usage`, `live$`, `rich`, `install`, `spend`, `remote`). Each reads:

| Arm | Meaning | Prints |
| --- | --- | --- |
| `Wired { via }` | the concern reaches a user-complete state; `via` names the path | the via |
| `Partial { via, gap }` | native coverage is incomplete and RimZ reconstructs the rest; `gap` names what is still missing | the gap |
| `Unsupported { reason }` | unreachable from the current protocol surface by any inference | the reason |

`LifecycleAnnotations` does the same for the eleven `LifecycleSignalKind`s, with arms `Native { event }`, `Derived { via, gap }`, and `Absent { reason }`.

Reserve `Partial` for a surface the user can still see is missing something. A value RimZ reconciles to its authoritative figure at every turn boundary is `Wired` even without a continuous native push, which is why Pi and OpenCode both claim `live$` wired: the extension pushes a running cost and the turn-end signal settles it to the session spend sum. Codex claims `end` and `idle` partial because no per-session end or idle hook exists, so pane liveness plus the reaper reconstruct end, and turn boundaries plus the ask path plus the stall window cover the attention slice of idle. Cursor claims `compact` partial because `preCompact` opens natively and the next lifecycle signal derives the close.

### The user-capability declaration

`CoverageAnnotations` answers what the adapter reads. [`UserCoverage`](../../../crates/rimz/src/agents/definition.rs) answers what the person watching the sidebar gets: one field per `UserCapability`, six in all — `state`, `live`, `history`, `account`, `ask`, `subagents` — and these are the marks the [compatibility matrix](../../reference/agent-support.md#the-compatibility-matrix) prints. Each reads:

| Arm | Meaning | Prints |
| --- | --- | --- |
| `Full { note }` | complete and live; the capability reads the way it does on Claude Code | the note |
| `Partial { shows, limit }` | a working version with a stated limit: part of the detail, or the whole of it a beat late | the limit |
| `Unsupported { reason }` | nothing to render for this capability | the reason |

How RimZ obtains the figure stays out of the mark. A value folded from a transcript tail and a value pushed by a native hook both read `Full` when the surface is complete and current, and a native signal carrying half the story reads `Partial`. The four strings print verbatim to end users through `rimz coverage`, so they carry product language — lowercase, no trailing period, roughly six to fourteen words, phrased in what the card shows. The rubric that fixes the meaning of each mark per capability is [agent-support.md](../../reference/agent-support.md#what-the-marks-mean); write against its ladders rather than inventing a rung.

`user_capabilities_are_complete_and_grounded` in [`conformance.rs`](../../../crates/rimz/src/agents/conformance.rs) links the two records one-directionally, through the concerns behind each capability:

| Capability | Backing concerns |
| --- | --- |
| `state` | `turn_lifecycle` |
| `live` | `context_usage`, `realtime_cost` |
| `history` | `account_spend` |
| `ask` | `permission`, `plan_approval`, `user_question` — any one suffices |
| `subagents` | `subagents` |

A `Full` mark needs its backing concerns wired — `ask` needs only one of its three, since a single blocking path reaching `rimz asks` is the whole user-visible claim — and an `Unsupported` mark needs every backing concern unsupported. Wired mechanism leaves the roll-up to the adapter's judgement: mark `Partial` when the user-visible result is still incomplete or lands late, and name which in `limit`. `account` carries no concern mapping, since its truth is the provider probe covered by the account fixtures ([providers.md](./providers.md)). Every mark carries non-empty text in every arm, and `Partial` names what does land alongside what is missing.

The value of honesty here is that a declared absence renders as a declared absence, in the sidebar and in `rimz doctor`, rather than as an accidental gap someone debugs for an hour.

## Adding an agent

A third-party agent normally ships as a [process plugin](../../reference/agent-plugins.md): one machine-tier manifest, an agent-side shim speaking the canonical envelope, optional probes, and no RimZ source change. A built-in earns its place when RimZ must own a native config migration (a hook installer that writes the agent's own config) or a protocol surface the canonical wire cannot express (an out-of-band rich-context transport, a bespoke ask-answer path). A built-in may also keep hook installation unsupported while a validated provider-owned local store supplies pulled truth, as Kiro does.

A built-in lands as one private directory under [`adapters/`](../../../crates/rimz/src/agents/adapters/mod.rs), one composed `registry::BUILTINS` entry, conformance coverage, and its own adapter doc. The directory anatomy is consistent across kinds:

| File | Holds |
| --- | --- |
| `mod.rs` | the unit-struct adapter, its `const AgentSpec`, and every capability `impl` |
| `payloads.rs` | typed structs for the native wire; structured parsers rather than ad-hoc `Value` digging |
| `install.rs` | the managed integration, when the shared `ManagedSource` backends do not fit |
| `account.rs`, `oauth_usage.rs` | the login probe and the account-usage query ([providers.md](./providers.md)) |
| `spend.rs` | the read-only full-history cost parser |
| `transcript.rs` | native transcript normalization |
| `tests.rs` or `tests/` | the conformance corpus and the inline `insta` stdout goldens |

`spend.rs` is sidebar-safe by construction: the `ensure_spend_parser_boundaries` invariant grep rejects store-write, run-wake, and broker imports in any spend path.

Because the observation an adapter emits is agent-agnostic, the new kind inherits the state machine, ranking, liveness, attention routing, messaging, supervised runs, and the sidebar row without anything downstream learning it exists. The sequenced playbook, from protocol reference to landed adapter with its deliverables checklist, is [agent-adapters.md](../../contributing/agent-adapters.md). The authoring contract is [`crates/rimz/src/agents/AGENTS.md`](../../../crates/rimz/src/agents/AGENTS.md).

### Extending the signal vocabulary

Adding a [`LifecycleSignal`](../../../crates/rimz/src/agents/lifecycle.rs) variant is deliberately harder than adding an adapter, because every variant costs an edge in one shared transition table. A new provider-observed variant requires both a concrete native event on a shipping provider that no existing variant plus enrichment expresses, and a distinct `(status, phase)` edge in `step`, landed with its edge test and the totality test extended. `CompactionEnded` and `TurnInterrupted` are the worked examples; `TurnInterrupted` is shared by Cursor's aborted stop and Pi's aborted settled outcome to land a canceled turn at idle without a false success or failure. Anything short of both bars is enrichment on an existing signal.

## See also

- [model.md](./model.md) — what happens to an observation: the rollup, the state machine, and the displayed-status projection.
- [providers.md](./providers.md) — the account, balance, spend, and pricing half of an integration.
- [plugin.md](./plugin.md) — the third-party process-plugin manifest, wire, and probes.
- [agent-adapters.md](../../contributing/agent-adapters.md) — the step-by-step integration playbook and deliverables checklist.
- [adapter_claude.md](./adapter_claude.md) and its twelve siblings — per-kind native mappings.
- [claude-reference.md](../../externals/agent-adapter/claude-reference.md) and its siblings — the raw upstream protocols adapters read, pinned to source URLs.
