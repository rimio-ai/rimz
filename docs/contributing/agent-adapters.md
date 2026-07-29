# Integrating a built-in agent adapter

The sequenced playbook for wiring a new coding agent into RimZ as a compiled-in adapter: the order of work, the decision points at each step, and the complete deliverables checklist. Every step points at the leaf that owns its detail — this guide owns the sequence, never the detail. The agent model and adapter boundary live in [model.md](../internals/agents/model.md), the code contract in [`crates/rimz/src/agents/AGENTS.md`](../../crates/rimz/src/agents/AGENTS.md), and the account/spend half in [providers.md](../internals/agents/providers.md).

An adapter that follows this sequence gets the state machine, ranking, liveness, attention routing, messaging, supervised runs, and the sidebar row for free: the [`AgentLifecycleObservation`](../../crates/rimz/src/agents/observation.rs) it emits is agent-agnostic by construction, so nothing downstream needs to know the new agent exists.

## Step 0 — Confirm a built-in is the right shape

A third-party agent normally ships as a [process plugin](../reference/agent-plugins.md): one machine-tier manifest, an agent-side shim speaking the canonical envelope, optional probes, and no RimZ source change. A built-in is appropriate when RimZ must own a native config migration (a hook installer that writes the agent's own config) or a protocol surface the canonical wire cannot express (an out-of-band rich-context transport, a bespoke ask-answer path) — the gate is stated in [adapter.md → Adding an agent](../internals/agents/adapter.md#adding-an-agent). Everything below is the built-in path.

## Step 1 — Land the protocol reference first

The integration starts as a document, not code. `docs/externals/agent-adapter/<kind>-reference.md` maps the agent's upstream protocol surface, pinned to source URLs: hooks and their payloads, session identity and resume, transcripts, auth and account, headless modes. The recent references are the template — [kiro-reference.md](../externals/agent-adapter/kiro-reference.md) records a validated provider-owned store, excluded session classes, negative hook evidence, and the boundary for new claims. The reference is the artifact every later step reads, and it outlives the integration as the drift-check target.

Read alongside it, in order: [adapter.md](../internals/agents/adapter.md) end to end (the boundary, the capability traits, the hook path, context sources, declared coverage), [model.md](../internals/agents/model.md) (the state machine and displayed status your observations drive), [`crates/rimz/src/agents/AGENTS.md`](../../crates/rimz/src/agents/AGENTS.md) (the authoring contract), then one small worked adapter ([`pi/`](../../crates/rimz/src/agents/adapters/pi/mod.rs) with its internals doc [adapter_pi.md](../internals/agents/adapter_pi.md)) and one large ([`claude/`](../../crates/rimz/src/agents/adapters/claude/mod.rs) with [adapter_claude.md](../internals/agents/adapter_claude.md)).

## Step 2 — Map the protocol onto the model

Produce the mapping worksheet from the reference before writing Rust; every later step consumes one of its rows.

- **Lifecycle signals.** Each of the eleven [`LifecycleSignalKind`](../../crates/rimz/src/agents/lifecycle.rs)s gets a verdict: which native event carries it (*Native*), which derivation reconstructs it with what gap (*Derived*), or why it has no signal (*Absent*). Which native event means what is the lifecycle part of `decode_hook`; [model.md → The state machine](../internals/agents/model.md#the-state-machine) defines what each signal does.
- **Integration concerns.** Each of the sixteen [`IntegrationConcern`](../../crates/rimz/src/agents/definition.rs)s gets *Wired*, *Partial* (with the gap named), or *Unsupported* (with the reason). Conformance later cross-checks every claim, so honesty here is cheaper than honesty under test failure.
- **User capabilities.** Roll those concerns up into the six [`UserCapability`](../../crates/rimz/src/agents/definition.rs) marks the compatibility matrix prints — state, live, history, account, ask, subagents — each *Full*, *Partial* (what lands, and the limit), or *Unsupported*. Answer from the sidebar card rather than the protocol: what a person sees, and when they see it. The rubric is [agent-support.md → The six capabilities](../reference/agent-support.md#the-six-capabilities).
- **Blocking asks.** Which native events block, each one's [`AskKind`](../../crates/rimz/src/agents/lifecycle.rs), whether the agent draws its own ask UI, and — verified, never assumed — what an empty hook response means for *this* agent: neutral semantics diverge per agent ([adapter.md → Adding an agent](../internals/agents/adapter.md#adding-an-agent)).
- **Tool vocabularies.** The mutating tool set and its file-editing subset; the editing set drives the `reasoning → acting` phase edge.
- **Session identity.** Where the session id comes from, standalone vs daemon-routed hooks, lazy registration, the resume command shape, and what `/clear` or `/new` does to identity ([model.md → The instance lifecycle](../internals/agents/model.md#the-instance-lifecycle)).
- **Context sources.** Which of the three gauge sources the agent offers: a transcript tail, a rich out-of-band transport, or a gauge stamped onto a RimZ-authored hook wire ([adapter.md → Context sources](../internals/agents/adapter.md#context-sources)).
- **Account and spend.** The auth surface (OAuth, API key, keyring), where per-turn usage records live, and whether they carry dollars (used verbatim) or tokens (priced through the price book) — the worksheet rows for [providers.md → Adding a provider](../internals/agents/providers.md#adding-a-provider).
- **Launch surface.** Argv for the `auto`/`ask`/`yolo`/`plan` permission modes, the compact command, and model/effort flags.

## Step 3 — Scaffold and register

Create `crates/rimz/src/agents/adapters/<kind>/` on the established anatomy:

- `mod.rs` — the private unit-struct adapter, its `const` [`AgentSpec`](../../crates/rimz/src/agents/definition.rs), and its capability implementations (empty where unsupported).
- `payloads.rs` — typed structs for the native wire; structured parsers, never ad-hoc `Value` digging past the classify step.
- `spend.rs` — the read-only cost parser (step 8).
- `account.rs`, plus `oauth_usage.rs` when the provider has a usage API (step 8).
- The install surface: return one [`ManagedIntegration`](../../crates/rimz/src/agents/managed_source.rs) from the adapter. Use `ManagedSource` for a JSON hook merge like Claude/Droid/Qwen or a RimZ-authored whole file like Pi/OpenCode; implement the same Interface in the provider's `install.rs` for TOML or multi-file transactions such as Codex and Cursor (step 5).
- The `tests` module (step 9); past the size gate it becomes a sibling `tests.rs` or `tests/` dir.

Register the module privately in [`adapters/mod.rs`](../../crates/rimz/src/agents/adapters/mod.rs), then name one `AgentDefinition` in [`registry::BUILTINS`](../../crates/rimz/src/agents/registry.rs). Implement every capability trait on the adapter: real behavior where the agent has it, an empty `impl` where it does not. That entry is the whole hookup — kind resolution and the `<kind>-auto`/`-ask`/`-yolo`/`-plan` permission variants all derive from the definition, so no consumer grows a provider match. The one optional extra is the `BUILTIN_PEER` default-layout string in [`harness/spec.rs`](../../crates/rimz/src/harness/spec.rs) when the kind belongs in the zero-config room.

## Step 4 — Declare the definition

`AgentSpec` is the immutable half of the integration contract. Fill its kind, aliases, display and brand identity, plan label, tool tables, operational policy, both coverage declarations, default context window and model, process and binary names, transcript thread key, and declarative launch shape. The editing tool set stays a subset of the mutating set, and definition validation pins that relationship together with alias uniqueness. Add optional curated art to [`emblems.toml`](../../crates/rimz/src/agents/emblems.toml); kinds without an entry render the shared fallback.

Two coverage declarations sit side by side and answer different questions:

- `CoverageAnnotations` — **mechanism**: what the adapter reads from its agent. One mark per `IntegrationConcern`, sixteen in all, each `Wired { via }`, `Partial { via, gap }`, or `Unsupported { reason }`. This is the worksheet's concern column transcribed.
- `UserCoverage` — **behavior**: what the user sees, and when. One mark per `UserCapability`, six in all, each `Full { note }`, `Partial { shows, limit }`, or `Unsupported { reason }`. `ANTIGRAVITY_USER_COVERAGE` in [`antigravity/mod.rs`](../../crates/rimz/src/agents/adapters/antigravity/mod.rs) is the worked example that splits both ways.

Conformance links the two one-directionally: a **full** mark rests on wired mechanism, and an **unsupported** mark rests on unsupported mechanism. Between those ends the roll-up is your judgement — mechanism that reports still reads **partial** when the user-visible result is incomplete or arrives late. Cursor's subagent hooks exist and the children land only when the parent's turn ends; Antigravity's dollar figure is live and covers the current turn rather than a session total. Name that limit in the mark.

Write the six marks in product language. `note`, `shows`, `limit`, and `reason` print verbatim to end users through `rimz coverage`, so keep each one lowercase, without a trailing period, roughly six to fourteen words, and phrased in what the card shows rather than which hook carries it. Pick the rung that already exists: [agent-support.md → What the marks mean](../reference/agent-support.md#what-the-marks-mean) defines full, partial, and unsupported, and [The six capabilities](../reference/agent-support.md#the-six-capabilities) spells out each ladder per capability.

[`pi/mod.rs`](../../crates/rimz/src/agents/adapters/pi/mod.rs) is the canonical filled-in example. [Conformance](../../crates/rimz/src/agents/conformance.rs) auto-enrolls every registered definition and cross-checks concern coverage, capability grounding, native events, classification samples, behavioral fixtures, and decoded lifecycle output.

## Step 5 — Implement the hook wire and install

Implement `CoreCapability`, then add `HookCapability` only when the provider has a real decoder. The decoder parses one native payload and returns typed canonical facts plus an explicit `HookReply`; the CLI binds runtime workspace, pane, launch, role, team, and channel identity. The channels and mapping jobs are [adapter.md → The hook path](../internals/agents/adapter.md#the-hook-path). Keep conformance samples in the provider test home and cover the full native event surface, retaining every payload variant needed to exercise broad events. Use one provider-local hook catalog to drive install, detection, decoded routing, and ordinary sample rows when the native config otherwise repeats that knowledge.

The contract rules, each owned by [`crates/rimz/src/agents/AGENTS.md`](../../crates/rimz/src/agents/AGENTS.md) and [adapter.md → Hook stdout is the decision channel](../internals/agents/adapter.md#hook-stdout-is-the-decision-channel): blocking ask hooks install sync, neutral is the agent-native no-op on stdout with diagnostics on stderr, helper children get fresh piped stdio, install is idempotent by command-substring reclaim, and installed hook timeouts leave margin under the upstream deadline.

Install is the visible security step ([adapter.md → Hook install](../internals/agents/adapter.md#hook-install)): declare one `managed_integration` so it drives install, preview, uninstall, installed, partial-artifact, upgrade, wiring-path, wrapped-statusline, and trust reporting. `ManagedSource` implements this Interface for shared backends; custom provider file transactions stay in that provider's `install.rs`. Include every installed hook command in the [trust hash](../internals/harness/trust.md).

## Step 6 — Wire launch, resume, and presets

From the worksheet's launch row: `permission_args` for the four [`PermissionMode`](../../crates/rimz/src/harness/run.rs)s, `render_preset` (reject any `agents.toml` preset field the agent cannot render, so launch intent is never silently dropped), `resume_command` and `fork_command`, `compact_command` (declare the native manual command or a documented registry exception when the agent only compacts automatically), and `launch_command`/`launch_env`/`default_launch_model` where the stock invocation needs shaping. When the provider has a verified native delegation control, implement `lockdown_subagent_args` or `lockdown_subagent_env` so it overrides conflicting profile values without erasing unrelated settings; otherwise keep the prompt-only default.

## Step 7 — Wire context enrichment

Implement the context source(s) the worksheet found; either alone is valid, and enrichment is never correctness — a failure is an omitted field, never an error ([model.md → Enrichment](../internals/agents/model.md#enrichment)).

- **Transcript tail**: locate the transcript from the hook payload and parse the newest usage record on top of [`read_transcript_tail`](../../crates/rimz/src/agents/transcript_fs.rs) under the [reading rules](../internals/agents/adapter.md#reading-rules) — bounded, newest-first, lossy, and explicit about zero (fresh session) vs unknown (unreadable).
- **Rich transport**: normalize the provider's out-of-band payload through `observe_context` into [`AgentContext`](../../crates/rimz/src/agents/context.rs), every field `Option`, tolerantly parsed; `local_context_refresh` and `context_refresh_spawn` hook the refresh triggers.
- **Payload-stamped gauge**: when RimZ authors the hook wire itself (an in-process extension like Pi's), stamp the gauge onto every envelope and skip the tail entirely.

## Step 8 — Wire account, spend, and pricing

The provider half of the integration is [providers.md → Adding a provider](../internals/agents/providers.md#adding-a-provider): `probe_account` (login state, plan, rate-limit windows), the OAuth-usage probe where one exists, and full-history cost through `spending_sources` plus `parse_spend`. Keep `transcript_files` for live transcript/session lookup. Put a positive-cost spend, hook-turn-cost, or context-cost fixture in `conformance` unless the spec declares `RealtimeCost` unsupported.

`spend.rs` is sidebar-safe by construction: read-only, and the `ensure_spend_parser_boundaries` invariant grep ([`xtask/src/invariants.rs`](../../xtask/src/invariants.rs)) rejects store-write, run-wake, and broker imports in any spend path.

## Step 9 — Test it

The required set, consolidated from [adapter.md → Adding an agent](../internals/agents/adapter.md#adding-an-agent) and the [module contract](../../crates/rimz/src/agents/AGENTS.md):

- install / uninstall / preview, and install version drift (an under-wired config re-offers the merge)
- lifecycle mapping: native event → observation → state, through the real `step`
- ask classification, and neutral silence as an inline `insta` golden per blocking event
- malformed-payload handling goldens
- PID attribution
- context mapping from a fixture transcript tail and a fixture transport payload, including the fresh-session zero and unreadable-unknown cases
- the spend fixture parsing to real entries

Conformance, definition validation, and registry uniqueness tests enroll the adapter automatically — no opt-in. Unit tests follow the one-home rule: inline `#[cfg(test)] mod tests` until the size gate moves them to a sibling ([rust-conventions.md → Tests](./rust-conventions.md#tests)).

## Step 10 — Ship the documentation

- Write `docs/internals/agents/adapter_<kind>.md` on the per-kind skeleton — *Hooks and lifecycle* (the native event → signal table is its core), *Context and transcript*, *Account and balance*, *Cost* — with [adapter_pi.md](../internals/agents/adapter_pi.md) as the small template and [adapter_claude.md](../internals/agents/adapter_claude.md) showing where marker subsections earn their place.
- Link it everywhere the existing kinds are linked: the per-kind list in [`crates/rimz/src/agents/AGENTS.md`](../../crates/rimz/src/agents/AGENTS.md), the per-provider table in [providers.md](../internals/agents/providers.md#per-provider-mapping), the root [AGENTS.md](../../AGENTS.md) documentation map, and the [internals index](../internals/README.md).
- Update [agent-support.md](../reference/agent-support.md): a row in the six-capability compatibility matrix, a row in the wiring matrix, a per-agent detail section, and removal from *Agents not yet supported*.
- Update the [README](../../README.md) agent compatibility matrix with the same six marks. The `published_matrices_match_the_declared_capabilities` integration test reads both published tables against `rimz coverage --json`, so a hand-edited cell that drifts from the declaration fails CI.

## Step 11 — Gate it

Done means: `cargo xtask gate` is green (format, invariants, docs-links, lint, fast tests); `rimz coverage` shows the kind's six capability marks reading true and `rimz coverage --wiring` its concern and lifecycle-hook rows; `rimz doctor` reports the hook install state; and `rimz agents <kind>` launches the stock CLI into a pane whose row registers, runs, asks, and settles in the sidebar. Escalate to the journey and live-backend tiers when the change touches their surfaces ([AGENTS.md → Testing](../../AGENTS.md#testing)).

## The deliverables checklist

- [ ] `docs/externals/agent-adapter/<kind>-reference.md` — upstream protocol reference, pinned to sources
- [ ] Mapping worksheet: 11 lifecycle signals, 16 concerns, 6 user capabilities, asks, tools, identity, context sources, spend, launch argv
- [ ] `crates/rimz/src/agents/adapters/<kind>/mod.rs` — private adapter, spec, every capability trait (empty where unsupported)
- [ ] `payloads.rs` typed wire · `spend.rs` · `account.rs` (± `oauth_usage.rs`) · install surface
- [ ] Private module in `adapters/mod.rs`, one composed entry in `registry::BUILTINS`
- [ ] `decode_hook` · typed canonical facts · explicit `HookReply` · complete classification corpus
- [ ] One managed integration covering install / preview / uninstall / `hooks_installed`
- [ ] `permission_args` · `render_preset` · `resume_command` · `compact_command`
- [ ] Context source(s): tail parse, `observe_context` transport, or payload-stamped gauge
- [ ] `probe_account` · `spending_sources` + `parse_spend` · positive-cost conformance fixture
- [ ] The step-9 test set, stdout shapes as inline `insta` goldens
- [ ] `docs/internals/agents/adapter_<kind>.md` + links in the module contract, the providers table, and the root documentation map
- [ ] `agent-support.md` detail section and wiring-matrix row · the six-capability row in `agent-support.md` and the README, kept true by the `published_matrices_match_the_declared_capabilities` integration test
- [ ] `cargo xtask gate` green · `rimz coverage` capability marks honest · `rimz coverage --wiring` mechanism grids honest · `rimz doctor` reporting
