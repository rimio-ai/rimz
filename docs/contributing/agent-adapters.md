# Integrating a built-in agent adapter

The sequenced playbook for wiring a new coding agent into RimZ as a compiled-in adapter: the order of work, the decision points at each step, and the complete deliverables checklist. Every step points at the leaf that owns its detail — this guide owns the sequence, never the detail. The agent model and adapter boundary live in [model.md](../internals/agents/model.md), the code contract in [`crates/rimz/src/agents/AGENTS.md`](../../crates/rimz/src/agents/AGENTS.md), and the account and spend half in [providers.md](../internals/agents/providers.md) and [spending.md](../internals/agents/spending.md).

An adapter that follows this sequence gets the state machine, ranking, liveness, attention routing, messaging, supervised runs, and the sidebar row for free: the [`AgentLifecycleObservation`](../../crates/rimz/src/agents/observation.rs) it emits is agent-agnostic by construction, so nothing downstream needs to know the new agent exists.

## Step 0 — Confirm a built-in is the right shape

A third-party agent normally ships as a [process plugin](../reference/agent-plugins.md): one machine-tier manifest, an agent-side shim speaking the canonical envelope, optional probes, and no RimZ source change. A built-in is appropriate when RimZ must own a native config migration (a hook installer that writes the agent's own config) or a protocol surface the canonical wire cannot express (an out-of-band rich-context transport, a bespoke ask-answer path) — the gate is stated in [adapter.md → Adding an agent](../internals/agents/adapter.md#adding-an-agent). Everything below is the built-in path.

## Step 1 — Land the protocol reference first

The integration starts as a document, not code. `docs/externals/agent-adapter/<kind>-reference.md` maps the agent's upstream protocol surface, pinned to source URLs: hooks and their payloads, session identity and resume, transcripts, auth and account, headless modes. The recent references are the template: [kiro-reference.md](../externals/agent-adapter/kiro-reference.md) records a validated provider-owned store, excluded session classes, negative hook evidence, and the boundary for new claims. The reference is the artifact every later step reads, and it outlives the integration as the drift-check target. Before writing the reference, run `command -v <bin>` and read its `--help` for an offline or echo provider. When one exists, capture hooks, env, parent process, and session store under a disposable `HOME` and XDG dirs, and record the version.

When the provider's CLI is installed on this machine (`command -v <bin>`, and its config home), its `--help` and a probe under a throwaway home with no credentials outrank the reference's gap table: a reference written before installation goes stale. Brief the first source trace with the whole mapping worksheet below in one pass: the built-in tool names, the fields each hook event actually sends, how permission modes are parsed, how folder trust gates hooks, and how hook replies are parsed. A second trace round for a question the first could have answered delays the design.

The pinned reference distinguishes upstream prose from versioned observations. Before you rest a design on which hook a provider does or does not fire, read the provider's own recorded sessions on disk; they carry the outcome fields upstream never documented. A design that rests on an upstream behaviour reads it at the adapter's `min_version` as well as at the reference baseline, and the reference records the first version where it changed: a missing OpenCode `{file:}` target failed the config load from 2.0.0 through 2.0.29 and has been substituted empty since 2.0.30 ([versioned behavior](../externals/agent-adapter/opencode-reference.md#system-prompts-and-config-substitution)).

Check whether the agent is installed (`command -v <bin>`, `<bin> --version`). A JS-bundled CLI embeds its source, so grep the installed bundle for the event names a design rests on before trusting a static reference.

Read alongside it, in order: [adapter.md](../internals/agents/adapter.md) end to end (the boundary, the capability traits, the hook path, context sources, declared coverage), [model.md](../internals/agents/model.md) (the state machine and displayed status your observations drive), [`crates/rimz/src/agents/AGENTS.md`](../../crates/rimz/src/agents/AGENTS.md) (the authoring contract), then one small worked adapter ([`pi/`](../../crates/rimz/src/agents/adapters/pi/mod.rs) with its internals doc [adapter_pi.md](../internals/agents/adapter_pi.md)) and one large ([`claude/`](../../crates/rimz/src/agents/adapters/claude/mod.rs) with [adapter_claude.md](../internals/agents/adapter_claude.md)).

## Step 2 — Map the protocol onto the model

Produce the mapping worksheet from the reference before writing Rust; every later step consumes one of its rows.

- **Lifecycle signals.** Each of the eleven [`LifecycleSignalKind`](../../crates/rimz/src/agents/lifecycle.rs)s gets a verdict: which native event carries it (*Native*), which derivation reconstructs it with what gap (*Derived*), or why it has no signal (*Absent*). A signal a native event carries on only some paths is *Derived*, and its `via` names that event; conformance fails a derived signal a corpus event produces without naming it. Which native event means what is the lifecycle part of `decode_hook`; [model.md → The state machine](../internals/agents/model.md#the-state-machine) defines what each signal does. A native cancel or interrupt event maps to `TurnInterrupted`, never `TurnEnded` ([model.md → Turn endings and parked turns](../internals/agents/model.md#turn-endings-and-parked-turns)). A tool event the user runs outside a model turn maps to no lifecycle signal, since a tool outside a turn moves an idle card to Running ([model.md → Edges](../internals/agents/model.md#edges)).
- **Integration concerns.** Each of the nineteen [`IntegrationConcern`](../../crates/rimz/src/agents/definition.rs)s gets *Wired*, *Partial* (with the gap named), or *Unsupported* (with the reason). Conformance later cross-checks every claim, so honesty here is cheaper than honesty under test failure.
- **User capabilities.** Roll those concerns up into the six [`UserCapability`](../../crates/rimz/src/agents/definition.rs) marks the compatibility matrix prints — state, live, history, account, ask, subagents — each *Full*, *Partial* (what lands, and the limit), or *Unsupported*. Answer from the sidebar card rather than the protocol: what a person sees, and when they see it. The rubric is [agent-support.md → The six capabilities](../reference/agent-support.md#the-six-capabilities).
- **Blocking asks.** Which native events block, each one's [`AskKind`](../../crates/rimz/src/agents/lifecycle.rs), whether the agent draws its own ask UI, and — verified, never assumed — what an empty hook response means for *this* agent: neutral semantics diverge per agent ([adapter.md → Hook stdout is the decision channel](../internals/agents/adapter.md#hook-stdout-is-the-decision-channel)).
- **Tool vocabularies.** The mutating tool set and its file-editing subset; the editing set drives the `reasoning → acting` phase edge.
- **Session identity.** Where the session id comes from, standalone vs daemon-routed hooks, lazy registration, the resume command shape, what `/clear` or `/new` does to identity, and the local-session discovery mode: `Unsupported`, `FullScan`, or `PerPane` ([instances.md → Binding a session](../internals/agents/instances.md#binding-a-session)).
- **Context sources.** Which of the three gauge sources the agent offers: a transcript tail, a rich out-of-band transport, or a gauge stamped onto a RimZ-authored hook wire ([adapter.md → Context sources](../internals/agents/adapter.md#context-sources)).
- **Account and spend.** The auth surface (OAuth, API key, keyring), where per-turn usage records live, and whether they carry dollars (used verbatim) or tokens (priced through the price book) — the worksheet rows for [providers.md → Adding a provider](../internals/agents/providers.md#adding-a-provider).
- **Launch surface.** Argv for the `auto`/`ask`/`yolo`/`plan` permission modes, the compact command, and model/effort flags.

## Step 3 — Scaffold and register

Create `crates/rimz/src/agents/adapters/<kind>/` on the established anatomy:

- `mod.rs` — the private unit-struct adapter, its `const` [`AgentSpec`](../../crates/rimz/src/agents/definition.rs), and its capability implementations (empty where unsupported).
- `payloads.rs` — typed structs for the native wire; structured parsers, never ad-hoc `Value` digging past the classify step.
- `spend.rs` — the read-only cost parser (step 8).
- `account.rs`, plus `oauth_usage.rs` when the provider has a usage API (step 8).
- The install surface: return one [`ManagedIntegration`](../../crates/rimz/src/agents/managed_source.rs) from the adapter. Use `ManagedSource` for a JSON hook merge like Claude/Droid/Qwen or a RimZ-authored whole file like Pi; implement the same interface in the adapter for TOML or multi-file transactions such as Codex, Cursor, and OpenCode (step 5).
- The `tests` module (step 9); past the size gate it becomes a sibling `tests.rs` or `tests/` dir.

Register the module privately in [`adapters/mod.rs`](../../crates/rimz/src/agents/adapters/mod.rs), then name one `AgentDefinition` in [`registry::BUILTINS`](../../crates/rimz/src/agents/registry.rs). Implement every capability trait on the adapter: real behavior where the agent has it, an empty `impl` where it does not. That entry is the whole hookup — kind resolution and the `<kind>-auto`/`-ask`/`-yolo`/`-plan` permission variants all derive from the definition, so no consumer grows a provider match. The one optional extra is the `BUILTIN_PEER` default-layout string in [`harness/spec.rs`](../../crates/rimz/src/harness/spec.rs) when the kind belongs in the zero-config room.

## Step 4 — Declare the definition

`AgentSpec` is the immutable half of the integration contract. Fill its kind, aliases, display and brand identity, plan label, tool tables, operational policy, both coverage declarations, default context window and model, process and binary names, transcript thread key, and declarative launch shape. The editing tool set stays a subset of the mutating set, and definition validation pins that relationship together with alias uniqueness. Add curated art to [`emblems.toml`](../../crates/rimz/src/agents/emblems.toml): `emblems.rs::tests::every_provider_has_curated_art_distinct_from_fallback` requires it for every built-in, and only plugin kinds render the shared fallback.

Declare `Capabilities::local_session_discovery` as `Unsupported` without a provider-local reader, `FullScan` to scan the admitted workspaces, or `PerPane` to read only what live panes can bind. A `PerPane` adapter implements `SessionCapability::discover_pane_sessions` as well as the full scan used by `rimz agents resume`: read every named session by id, then creation-ordered new files in watched folders since the demand's clock while the pane is unsettled. A path the adapter remembered for a named session, rather than one a hook named, is a cache: when the provider can start a new file for the same id (Codex writes a new rollout revision on a revert), the backstop resolves the id again and takes the newer file. A file whose head is not readable yet is rechecked each pass only while it could still start a session inside the window, so an abandoned headless file in history adds no warm metadata. Prove unchanged metadata cost independent of history size with stat-count tests: Claude and Codex's `local_sessions::tests::pane::pane_warm_work_is_independent_of_history_for_settled_and_open_windows` compare 50 and 500 historical transcripts, including zero warm listings and transcript reads. Prove equivalence to the full scan's bound pairs, merged rows and ghost-bind diagnostics through the store demand test. Bind-rejection diagnostics must remain a subset, and equal the full scan's when every pane has a process start and no reserved pane's own launch session was found.

Two coverage declarations sit side by side and answer different questions:

- `CoverageAnnotations` — **mechanism**: what the adapter reads from its agent. One mark per `IntegrationConcern`, nineteen in all, each `Wired { via }`, `Partial { via, gap }`, or `Unsupported { reason }`. This is the worksheet's concern column transcribed.
- `UserCoverage` — **behavior**: what the user sees, and when. One mark per `UserCapability`, six in all, each `Full { note }`, `Partial { shows, limit }`, or `Unsupported { reason }`. `ANTIGRAVITY_USER_COVERAGE` in [`antigravity/mod.rs`](../../crates/rimz/src/agents/adapters/antigravity/mod.rs) is the worked example that splits both ways.

A concern fed by a native event is Wired even when its behavior has a gap; the gap belongs on the user capability's limit and in the adapter's Known gaps.

Conformance links the two one-directionally: a **full** mark rests on wired mechanism, and an **unsupported** mark rests on unsupported mechanism. Between those ends the roll-up is your judgement — mechanism that reports still reads **partial** when the user-visible result is incomplete or arrives late. Cursor's subagent hooks exist and the children land only when the parent's turn ends; Antigravity's dollar figure is live and covers the current turn rather than a session total. Name that limit in the mark.

Write the six marks in product language. `note`, `shows`, `limit`, and `reason` print verbatim to end users through `rimz coverage`, so keep each one lowercase, without a trailing period, roughly six to fourteen words, and phrased in what the card shows rather than which hook carries it. Pick the rung that already exists: [agent-support.md → What the marks mean](../reference/agent-support.md#what-the-marks-mean) defines full, partial, and unsupported, and [The six capabilities](../reference/agent-support.md#the-six-capabilities) spells out each ladder per capability.

[`pi/mod.rs`](../../crates/rimz/src/agents/adapters/pi/mod.rs) is the canonical filled-in example. [Conformance](../../crates/rimz/src/agents/conformance.rs) auto-enrolls every registered definition and cross-checks concern coverage, capability grounding, native events, classification samples, behavioral fixtures, and decoded lifecycle output.

## Step 5 — Implement the hook wire and install

Implement `CoreCapability`, then add `HookCapability` only when the provider has a real decoder. The decoder parses one native payload and returns typed canonical facts plus an explicit `HookReply`; the CLI binds runtime workspace, pane, launch, role, team, and channel identity. The channels and mapping jobs are [adapter.md → The hook path](../internals/agents/adapter.md#the-hook-path). Keep conformance samples in the provider test home and cover the full native event surface, retaining every payload variant needed to exercise broad events. Use one provider-local hook catalog to drive install, detection, decoded routing, and ordinary sample rows when the native config otherwise repeats that knowledge, and decode through `decode_catalog_hook`, which takes any iterator of catalog entries. Map a `SessionStart` payload's source with [`SessionSource::session_start_signal`](../../crates/rimz/src/agents/hook_types.rs) instead of restating which source ends a compaction.

The contract rules, each owned by [`crates/rimz/src/agents/AGENTS.md`](../../crates/rimz/src/agents/AGENTS.md) and [adapter.md → Hook stdout is the decision channel](../internals/agents/adapter.md#hook-stdout-is-the-decision-channel): blocking ask hooks install sync, neutral is the agent-native no-op on stdout with diagnostics on stderr, helper children get fresh piped stdio, install idempotently reclaims owned entries or whole files, and installed hook timeouts leave margin under the upstream deadline. Use command-substring reclaim for shared JSON merges; Muse instead owns its [marker-free hook file](../internals/agents/adapter_muse.md#install) structurally.

Install is the visible security step ([adapter.md → Hook install](../internals/agents/adapter.md#hook-install)): declare one `managed_integration` so it drives install, preview, uninstall, installed, partial-artifact, upgrade, wiring-path, wrapped-statusline, and provider-native hook trust reporting. `ManagedSource` implements this Interface for shared backends; custom provider file transactions stay in that provider's `install.rs`. The [project trust hash](../internals/harness/trust.md) covers executable project configuration, not installed provider hook files; preview their edits and preserve any provider-native trust gate.

Install meets the machine, and seven traps have each cost an upgrade pass a review round:

- **Refusals block only the named install.** An install that refuses on this machine or in the login's provider home (a CLI too old for the hook surface, a missing prerequisite, a provider policy in that home) returns the reason from `ManagedIntegration::install_blocker`, which takes the login env. Detected installs in `rimz start`, `rimz setup`, and a bare `rimz hooks install` then skip the adapter, and `rimz hooks install <kind>` still refuses. A refusal raised from preview instead aborts every agent's install. Check it with `rimz hooks install --dry-run` and no agent named. A provider policy that keeps installed hooks from running also answers `ManagedIntegration::hooks_disabled`, which the hook preflight checks ahead of the hooks-missing check on every launch and delivery, so it reads files and never spawns a process.
- **Closed event schemas need a version gate.** Before adding an event to the installed catalog, check whether the oldest supported release validates hook event names against a closed set. If it does, a new event invalidates or strips the user's hooks on that release, so the event waits for an install-time version probe instead of shipping unconditionally.
- **Resolve provider paths from the pane's env.** When install, `installed()`, or a reader resolves a provider path from env (config home, statusline file, session store), resolve it the way the agent pane sees it. tmux panes carry `disk::paths::runtime_domain_env()`, which pins an unset `XDG_CONFIG_HOME` to `$HOME/.config`, while Zellij panes inherit the launching shell. Cover both with an out-of-process test that installs from a shell without `XDG_CONFIG_HOME` and checks `installed()` under the pinned env, and grep `crates/rimz/tests/integration/common` for fixtures that hardcode the old path.
- **Several user hook files can shadow each other.** When the agent merges hooks from more than one user file, confirm which file wins per event before choosing where install writes; a later user save can copy the merged set into the file that shadows RimZ's entries.
- **An in-process plugin's PID is not always the agent's.** A plugin or extension runtime can run in a child process, so `process.pid` there names the runtime and its parent names the agent CLI. Confirm which process the `RIMZ_AGENT_PID` stamp names.
- **A measured process name is not necessarily specific.** An agent-specific declared name must be one only that agent's process carries. A runtime's generic name (`node`, `node-MainThread`, `bun`) is shared, and a unique declaration of it claims every such process by comm. Use shared launcher names only where classification abstains, and verify unrelated runtime commands do not resolve to the agent.
- **A scrubbed hook environment breaks path resolution, not just identity.** A runner that does not inherit the agent's environment loses the feed's RimZ home and runtime root as well as workspace, pane, and run identity. Override `HookCapability::hook_inherits_env` to return `false` and stamp the verified agent's PID in `RIMZ_AGENT_PID`, so the feed restores the owner's environment before resolving paths.

### Probe a hook reply outside a room

A reply that injects context into the model (Claude's and Codex's `hookSpecificOutput.additionalContext`) has two questions no fixture answers: whether the model reads it, and how the provider stores it in its transcript. A disposable sandbox room cannot answer them, because it replaces `HOME` and holds no provider login ([sidebar-live-check.md → Check against a real provider login](./sidebar-live-check.md#check-against-a-real-provider-login)). Hand-feeding `rimz hooks feed` there proves the hook's output and nothing about the provider. Answer both with a throwaway hook under your own login, outside any room, before the reply shape is built on:

```sh
PROBE="$(mktemp -d)"
cat > "$PROBE/hook.sh" <<'EOF'
#!/bin/sh
cat >/dev/null
printf '%s\n' '{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"PROBE-MARKER-7731"}}'
EOF
chmod +x "$PROBE/hook.sh"
printf '{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"%s"}]}]}}\n' "$PROBE/hook.sh" > "$PROBE/settings.json"
cd "$PROBE"
claude -p 'Quote any marker you were given, without tools.' --settings "$PROBE/settings.json"
codex exec --skip-git-repo-check --dangerously-bypass-hook-trust -s read-only \
  -c "hooks.UserPromptSubmit=[{hooks=[{type=\"command\",command=\"$PROBE/hook.sh\"}]}]" \
  'Quote any marker you were given, without tools.'
```

The model quoting the marker shows it read the reply; a model may also decline to quote it (Codex has answered that it cannot quote private instructions), so the session file is the proof. Search the one each provider just wrote for the marker and read the entry that holds it: its type and role decide whether RimZ's transcript reader sees the text as the user's. On Claude 2.1.288 it is a separate `attachment` entry of type `hook_additional_context`; on codex-cli 0.160.0 it is a `developer` message in the rollout. Both leave the user entry as typed.

Codex takes the hook from the `-c` override; a project `.codex/hooks.json` in the probe directory was not loaded. The probe covers the provider and the model only. The chain from a prompt pasted into a pane through the installed hook still needs a host room with a login.

## Step 6 — Wire launch, resume, and presets

From the worksheet's launch row: `permission_args` for the four [`PermissionMode`](../../crates/rimz/src/agents/definition.rs)s, `render_preset` (keep the renderer strict; `LaunchSpec::normalize_preset` decides which unsupported tuning settings to skip with a launch warning), `resume_command` and `fork_command`, `compact_command` (declare the native manual command and use `CompactInstruction::Trailing` only when upstream verifies that it accepts summary guidance, or document a registry exception when the agent only compacts automatically), and `launch_command`/`launch_env`/`default_launch_model` where the stock invocation needs shaping. When the provider has a verified native delegation control, implement `disable_native_subagent_args` for argv or apply the delegation decision in `finalize_launch_env` for env-carried config; otherwise keep the prompt-only default. The final hook runs once on every managed launch with the effective env, the delegation decision, and an optional system-prompt artifact. Merge all edits to a config variable there so they override conflicting profile values without erasing unrelated settings or each other ([OpenCode mapping](../internals/agents/adapter_opencode.md#launch-and-resume)). Preserve the user's native command sandbox under scoped isolation. Only implement `disable_native_sandbox_args` when nesting is known to break that provider; Muse is the current exception ([sandbox.md](../internals/sandbox.md#provider-command-sandboxes)). When the provider documents a switch that turns its own sandbox on, map `sandbox` isolation to that switch and nothing past it: no dependency check, no fallback override, no warning about a permission mode, and no change to a mode the user selected. A provider without such a switch keeps the default, which warns and launches.

A mode vector sets every provider config key its meaning depends on (`-c key=value` or the flag), never leaving one to the provider's default: the user's config file overrides that default. Codex `ask` once relied on the default reviewer, so a global `approvals_reviewer = "auto_review"` routed it to Guardian instead of the user. Before pinning a vector, run the provider CLI with it under an isolated provider home and confirm the behaviour the mode promises.

Prove the launch `PromptStyle` live once: launch with a trivial prompt under a disposable `HOME` and confirm the pane submits it. An argv snapshot only proves the shape. OpenCode's and Kiro's TUIs silently dropped a `PositionalAfterDoubleDash` prompt that every unit test accepted, and a Commander root command treats a prompt equal to a subcommand name as that subcommand. Try a prompt that starts with `-` too, and name any form that has no working argv in the internals doc.

Keep [`rimz events follow`](../reference/cli/events.md) running through the adapter's live check, its output written to a file, and read each line's `status` against the card: a line the card contradicts is a bug in the follower or the rollup. A live OpenCode check passed on the card while the follower printed `failed` for a conversation the card rested at Idle.

## Step 7 — Wire context enrichment

Implement the context source(s) the worksheet found; either alone is valid, and enrichment is never correctness — a failure is an omitted field, never an error ([model.md → Enrichment](../internals/agents/model.md#enrichment)).

- **Transcript tail**: locate the transcript from the hook payload and parse the newest usage record on top of [`read_transcript_tail_with_status`](../../crates/rimz/src/agents/transcript_fs.rs) under the [reading rules](../internals/agents/adapter.md#reading-rules): bounded, newest-first, lossy, and explicit about zero (whole file read, no usage) vs unknown (unreadable, or a bounded tail with no usage). Gate the re-read with `ctx.changed_transcript(stat)` so an unchanged source costs nothing, and write your own gate only when a second condition forces a read anyway, as Claude's fold replay and Codex's token backfill do ([adapter.md → Context sources](../internals/agents/adapter.md#context-sources)).
- **Rich transport**: normalize the provider's out-of-band payload through `observe_context` into [`AgentContext`](../../crates/rimz/src/agents/context.rs), every field `Option`, tolerantly parsed; `local_context_refresh` and `context_refresh_spawn` hook the refresh triggers.
- **Payload-stamped gauge**: when RimZ authors the hook wire itself (an in-process extension like Pi's), stamp the gauge onto every envelope and skip the tail entirely.

## Step 8 — Wire account, spend, and pricing

The provider half of the integration is [providers.md → Adding a provider](../internals/agents/providers.md#adding-a-provider): `probe_account` (login state, plan, rate-limit windows), the OAuth-usage probe where one exists, and full-history cost through `spending_sources` plus `parse_spend`. Keep `transcript_files` for live transcript/session lookup. Put a positive-cost spend, hook-turn-cost, or context-cost fixture in `conformance` unless the spec declares `RealtimeCost` unsupported.

A matcher on a provider error body (an auth rejection, a lapsed plan) is written against a recorded response and pins the exact key path. Record the body in the adapter's externals reference first, with tokens and request IDs redacted, and pin the unit test to that body. A value scan over an object, or a key guessed from the value alone, does not land: the Claude lapsed-plan matcher guessed its key and missed the live 403 ([adapter_claude.md → OAuth usage probe](../internals/agents/adapter_claude.md#oauth-usage-probe)).

`spend.rs` is sidebar-safe by construction: read-only, and the `ensure_spend_parser_boundaries` invariant grep ([`xtask/src/invariants.rs`](../../xtask/src/invariants.rs)) rejects store-write, run-wake, and broker imports in any spend path.

Spend results are cached per file stat, so a `spend.rs` change reaches only transcripts parsed after it. A change that drops, adds, or reprices entries for transcripts already on disk bumps `SPENDING_CACHE_VERSION` in [`spending/cache.rs`](../../crates/rimz/src/agents/spending/cache.rs) with a one-line reason, and any branch that must clear entries an earlier parse cached returns `SpendUpdate::Replace`.

## Step 9 — Test it

Before a lifecycle field gates behaviour, inventory each adapter's durable set/clear producers and test the field across persisted-cache advancement with an empty delta; display-derived observations are not durable reset evidence.

The required set, consolidated from [adapter.md → Adding an agent](../internals/agents/adapter.md#adding-an-agent) and the [module contract](../../crates/rimz/src/agents/AGENTS.md):

- install / uninstall / preview, and install version drift (an under-wired config re-offers the merge)
- lifecycle mapping: native event → observation → state, through the real `step`
- ask classification, and neutral silence as an inline `insta` golden per blocking event
- malformed-payload handling goldens
- PID attribution
- context mapping from a fixture transcript tail and a fixture transport payload, including the fresh-session zero and unreadable-unknown cases
- the spend fixture parsing to real entries

Four rules keep those tests honest:

- **A new mapping's test fails without the mapping.** Give the payload a fallback input that would classify differently, so removing the new match arm turns the test red.
- **A keyed wait is tested across the settle.** A wait's clearing event must be dropped once the turn settles, because an unnamed `ToolUsed` on a settled row reopens a turn ([`lifecycle.rs`](../../crates/rimz/src/agents/lifecycle.rs) `reconcile_activity`). Test open, settle, then close.
- **"An adapter without capability X" is an unregistered kind.** A shared test that borrows a real built-in as the negative case breaks the day that built-in gains the capability; use `"unregistered"`.
- **Shared pins move with the definition.** Changing presets, hook events, or coverage marks moves counts and snapshots outside the adapter: the inline snapshots in `agents::conformance::render_preset_characterization` and `agents::conformance::fast_switch_characterization`, `crates/rimz/src/cli/coverage/tests.rs::coverage_pins_agent_matrix` and `crates/rimz/src/cli/coverage/tests.rs::hook_matrix_pins_lifecycle_signals`, the managed hook counts in `crates/rimz/tests/integration/hooks.rs`, and the adapter's own catalog test. A new kind also joins the private-boundary `provider_names`/`concrete_types` lists in `xtask/src/invariants.rs`, the per-kind home tables in `crates/rimz/src/agents/conformance.rs` that assert `len() == BUILTINS.len()`, and the hand-kept `AGENTS` display map in `crates/rimz/tests/integration/coverage.rs`. Add its curated art to `crates/rimz/src/agents/emblems.toml`. When it declares a home key, add that key to `xtask/src/sandbox.rs` `PROVIDER_HOME_ENV`; integration commands discover declared home keys automatically. Add extra provider path overrides to `crates/rimz/tests/integration/common/command.rs` `PROVIDER_SIDECAR_KEYS` and the sandbox list, and keep the provider-home guard in `xtask/src/invariants.rs` aligned. Without a new key those lists stay unchanged. Keep README and published support matrices aligned; a kind omitted from both the test array and docs otherwise passes silently. Run `cargo xtask test 'agents::conformance'`, `cargo xtask test 'cli::coverage'`, the adapter tests, `cargo xtask test -E 'binary(integration) & test(coverage::)'`, and `cargo xtask test -E 'binary(integration) & test(hooks::)'` as separate calls before the gate, and update each pin in the commit that moves it.

Conformance, definition validation, and registry uniqueness tests enroll the adapter automatically — no opt-in. Unit tests follow the one-home rule: inline `#[cfg(test)] mod tests` until the size gate moves them to a sibling ([rust-conventions.md → Tests](./rust-conventions.md#tests)).

## Step 10 — Ship the documentation

- Write `docs/internals/agents/adapter_<kind>.md` on the per-kind skeleton — *Hooks and lifecycle* (the native event → signal table is its core), *Context and transcript*, *Account and balance*, *Cost* — with [adapter_pi.md](../internals/agents/adapter_pi.md) as the small template and [adapter_claude.md](../internals/agents/adapter_claude.md) showing where marker subsections earn their place.
- Link it everywhere the existing kinds are linked: the per-kind list in [`crates/rimz/src/agents/AGENTS.md`](../../crates/rimz/src/agents/AGENTS.md), the per-provider table in [providers.md](../internals/agents/providers.md#per-provider-mapping), and the [internals index](../internals/README.md), which the root [AGENTS.md](../../AGENTS.md) documentation map links.
- Update [agent-support.md](../reference/agent-support.md): a row in the six-capability compatibility matrix, a row in the wiring matrix, a per-agent detail section, and removal from *Agents not yet supported*.
- Update the [README](../../README.md) agent compatibility matrix with the same six marks. The `published_matrices_match_the_declared_capabilities` integration test reads both published tables against `rimz coverage --json`, so a hand-edited cell that drifts from the declaration fails CI.
- Correct every count the new kind raises. A census by a sibling's name (`rg -i kiro`) cannot reach a sentence that states only a total, so grep the old total word and the sibling count derived from it as well: with seventeen built-ins before the change, `rg -n -i '\b(seventeen|sixteen siblings|fifteen siblings)\b' AGENTS.md README.md docs crates --no-ignore-dot`, then skip the hits that count something else.

## Step 11 — Gate it

Done means: `cargo xtask gate` is green (format, invariants, docs-links, lint, fast tests); `rimz coverage` shows the kind's six capability marks reading true and `rimz coverage --wiring` its concern and lifecycle-hook rows; `rimz doctor` reports the hook install state; and `rimz agents <kind>` launches the stock CLI into a pane whose row registers, runs, asks, and settles in the sidebar. Escalate to the journey and live-backend tiers when the change touches their surfaces ([AGENTS.md → Testing](../../AGENTS.md#testing)).

## The deliverables checklist

- [ ] `docs/externals/agent-adapter/<kind>-reference.md` — upstream protocol reference, pinned to sources
- [ ] Mapping worksheet: 11 lifecycle signals, 19 concerns, 6 user capabilities, asks, tools, identity, context sources, spend, launch argv
- [ ] `crates/rimz/src/agents/adapters/<kind>/mod.rs` — private adapter, spec, every capability trait (empty where unsupported), explicit `Unsupported` / `FullScan` / `PerPane` discovery mode with readers and cost/equivalence tests for `PerPane`
- [ ] `payloads.rs` typed wire · `spend.rs` · `account.rs` (± `oauth_usage.rs`) · install surface
- [ ] Private module in `adapters/mod.rs`, one composed entry in `registry::BUILTINS`
- [ ] Curated art for the kind in `crates/rimz/src/agents/emblems.toml`
- [ ] `decode_hook` · typed canonical facts · explicit `HookReply` · complete classification corpus
- [ ] One managed integration covering install / preview / uninstall / `hooks_installed` · machine and login-home refusals through `install_blocker` · provider paths checked under the tmux-pinned and inherited pane env
- [ ] `permission_args` · `render_preset` · `resume_command` · `compact_command` with verified instruction support · launch prompt style proven live
- [ ] Spend changes to existing transcripts bump `SPENDING_CACHE_VERSION` · shared pins moved in the same commit: the conformance preset snapshot, `permission_mode_table_matches_the_declared_arguments`, the `builtin_config_homes_*` cases, the coverage matrices, integration hook counts, the provider-name lists in `xtask/src/invariants.rs`, `PROVIDER_HOME_ENV` in `xtask/src/sandbox.rs`, and the prompt-context reply-wait kinds in `cli/hooks.rs::run_feed_inner` when the adapter answers with context
- [ ] `launch_reminders` coverage declared from `append_system_text_channel`
- [ ] Context source(s): tail parse, `observe_context` transport, or payload-stamped gauge
- [ ] `probe_account` · `spending_sources` + `parse_spend` · positive-cost conformance fixture
- [ ] The step-9 test set, stdout shapes as inline `insta` goldens
- [ ] `docs/internals/agents/adapter_<kind>.md` + links in the module contract, the providers table, and the internals index
- [ ] Every agent and sibling count raised, found by grepping the old number words as well as a sibling's name
- [ ] `agent-support.md` detail section and wiring-matrix row · the six-capability row in `agent-support.md` and the README, kept true by the `published_matrices_match_the_declared_capabilities` integration test
- [ ] `cargo xtask gate` green · `rimz coverage` capability marks honest · `rimz coverage --wiring` mechanism grids honest · `rimz doctor` reporting
