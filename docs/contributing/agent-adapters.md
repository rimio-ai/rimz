# Integrating a built-in agent adapter

The sequenced playbook for wiring a new coding agent into RimZ as a compiled-in adapter: the order of work, the decision points at each step, and the complete deliverables checklist. Every step points at the leaf that owns its detail — this guide owns the sequence, never the detail. The agent model and adapter boundary live in [model.md](../internals/agents/model.md), the code contract in [`crates/rimz/src/agents/AGENTS.md`](../../crates/rimz/src/agents/AGENTS.md), and the account/spend half in [providers.md](../internals/agents/providers.md).

An adapter that follows this sequence gets the state machine, ranking, liveness, attention routing, messaging, supervised runs, and the sidebar row for free: the [`AgentLifecycleObservation`](../../crates/rimz/src/agents/observation.rs) it emits is agent-agnostic by construction, so nothing downstream needs to know the new agent exists.

## Step 0 — Confirm a built-in is the right shape

A third-party agent normally ships as a [process plugin](../reference/agent-plugins.md): one machine-tier manifest, an agent-side shim speaking the canonical envelope, optional probes, and no RimZ source change. A built-in is appropriate when RimZ must own a native config migration (a hook installer that writes the agent's own config) or a protocol surface the canonical wire cannot express (an out-of-band rich-context transport, a bespoke ask-answer path) — the gate is stated in [model.md → Adding an agent](../internals/agents/model.md#adding-an-agent). Everything below is the built-in path.

## Step 1 — Land the protocol reference first

The integration starts as a document, not code. `docs/externals/agent-adapter/<kind>-reference.md` maps the agent's upstream protocol surface, pinned to source URLs: hooks and their payloads, session identity and resume, transcripts, auth and account, headless modes. The recent references are the template — [kiro-reference.md](../externals/agent-adapter/kiro-reference.md) carries the full shape, including a *Recommended adapter shape* section and a closing implementation checklist naming what only live verification can settle. The reference is the artifact every later step reads, and it outlives the integration as the drift-check target.

Read alongside it, in order: [model.md](../internals/agents/model.md) end to end (the boundary, the state machine, displayed status, enrichment), [`crates/rimz/src/agents/AGENTS.md`](../../crates/rimz/src/agents/AGENTS.md) (the authoring contract), then one small worked adapter ([`pi/`](../../crates/rimz/src/agents/pi/mod.rs) with its internals doc [pi.md](../internals/agents/pi.md)) and one large ([`claude/`](../../crates/rimz/src/agents/claude/mod.rs) with [claude.md](../internals/agents/claude.md)).

## Step 2 — Map the protocol onto the model

Produce the mapping worksheet from the reference before writing Rust; every later step consumes one of its rows.

- **Lifecycle signals.** Each of the eleven [`LifecycleSignalKind`](../../crates/rimz/src/agents/lifecycle.rs)s gets a verdict: which native event carries it (*Native*), which derivation reconstructs it with what gap (*Derived*), or why it has no signal (*Absent*). Which native event means what is the whole job of `observe_lifecycle`; [model.md → The state machine](../internals/agents/model.md#the-state-machine) defines what each signal does.
- **Integration concerns.** Each of the sixteen [`IntegrationConcern`](../../crates/rimz/src/agents/descriptor.rs)s gets *Wired*, *Partial* (with the gap named), or *Unsupported* (with the reason). Conformance later cross-checks every claim, so honesty here is cheaper than honesty under test failure.
- **Blocking asks.** Which native events block, each one's [`AskKind`](../../crates/rimz/src/agents/lifecycle.rs), whether the agent draws its own ask UI, and — verified, never assumed — what an empty hook response means for *this* agent: neutral semantics diverge per agent ([model.md → Adding an agent](../internals/agents/model.md#adding-an-agent)).
- **Tool vocabularies.** The mutating tool set and its file-editing subset; the editing set drives the `reasoning → acting` phase edge.
- **Session identity.** Where the session id comes from, standalone vs daemon-routed hooks, lazy registration, the resume command shape, and what `/clear` or `/new` does to identity ([model.md → The instance lifecycle](../internals/agents/model.md#the-instance-lifecycle)).
- **Context sources.** Which of the three gauge sources the agent offers: a transcript tail, a rich out-of-band transport, or a gauge stamped onto a RimZ-authored hook wire ([model.md → Two sources](../internals/agents/model.md#two-sources)).
- **Account and spend.** The auth surface (OAuth, API key, keyring), where per-turn usage records live, and whether they carry dollars (used verbatim) or tokens (priced through the price book) — the worksheet rows for [providers.md → Adding a provider](../internals/agents/providers.md#adding-a-provider).
- **Launch surface.** Argv for the `auto`/`ask`/`yolo`/`plan` permission modes, the compact command, ping args for budget-window priming, and model/effort flags.

## Step 3 — Scaffold and register

Create `crates/rimz/src/agents/<kind>/` on the established anatomy:

- `mod.rs` — the unit-struct adapter, its `const` [`AgentDescriptor`](../../crates/rimz/src/agents/descriptor.rs), and the two matrices.
- `payloads.rs` — typed structs for the native wire; structured parsers, never ad-hoc `Value` digging past the classify step.
- `spend.rs` — the read-only cost parser (step 8).
- `account.rs`, plus `oauth_usage.rs` when the provider has a usage API (step 8).
- The install surface: config-merge install like Claude and Codex, or a RimZ-authored whole-file shim owned through [`managed_source.rs`](../../crates/rimz/src/agents/managed_source.rs) like Pi and OpenCode (step 5).
- The `tests` module (step 9); past the size gate it becomes a sibling `tests.rs` or `tests/` dir.

Register it: `pub mod <kind>;` plus the adapter re-export in [`agents/mod.rs`](../../crates/rimz/src/agents/mod.rs), and one `&<Kind>Adapter` line in [`registry::ADAPTERS`](../../crates/rimz/src/agents/registry.rs). That line is the whole hookup — kind resolution, the `<kind>-auto`/`-ask`/`-yolo`/`-plan` permission variants, and `<kind>-ping` all derive from the registry plus adapter methods, so no shared `match` grows an arm. The one optional extra is the `BUILTIN_PEER` default-layout string in [`harness/spec.rs`](../../crates/rimz/src/harness/spec.rs) when the kind belongs in the zero-config room.

## Step 4 — Declare the descriptor

The descriptor is the adapter's contract with the rest of RimZ; every behavior downstream is capability-gated on it. Fill every field of [`AgentDescriptor`](../../crates/rimz/src/agents/descriptor.rs): kind, display name, brand, plan label, the tool tables (the editing set must be a subset of the mutating set — a descriptor test pins it), `Capabilities`, the sixteen-row `coverage` table and eleven-row `lifecycle_hooks` table straight from the step-2 worksheet, default context window and model, `process_names` (the bare comm plus launchers like `node`; release-binary triple suffixes are handled automatically), and `thread_key` for transcript-to-session mapping.

[`pi/mod.rs`](../../crates/rimz/src/agents/pi/mod.rs) is the canonical filled-in example. [Conformance](../../crates/rimz/src/agents/conformance.rs) auto-enrolls every registered adapter: both matrices must be complete (each concern and signal kind exactly once), and every *Wired*/*Native* claim is cross-checked against the capabilities, the installed events, the classification corpus, and `observe_lifecycle` output.

## Step 5 — Implement the hook wire and install

The trait has three required methods — `descriptor`, `classify_hook`, `render_neutral` — plus `observe_lifecycle` as the core native-event → [`LifecycleSignal`](../../crates/rimz/src/agents/lifecycle.rs) mapper; the channels and the four jobs of the mapping are [model.md → The adapter boundary](../internals/agents/model.md#the-adapter-boundary). Two conformance inputs ship with it: `classification_corpus` (a real sample payload per installed event, each classifying as declared) and `installed_hook_events` (the wired set, the adapter's single source of truth for install detection).

The contract rules, each owned by [`crates/rimz/src/agents/AGENTS.md`](../../crates/rimz/src/agents/AGENTS.md) and [model.md → Hook stdout is the decision channel](../internals/agents/model.md#hook-stdout-is-the-decision-channel): blocking ask hooks install sync, neutral is the agent-native no-op on stdout with diagnostics on stderr, helper children get fresh piped stdio, install is idempotent by command-substring reclaim, and installed hook timeouts leave margin under the upstream deadline.

Install is the visible security step ([model.md → Hook install](../internals/agents/model.md#hook-install-the-visible-security-step)): implement `install_hooks`, `preview_hook_install`, `uninstall_hooks`, and `hooks_installed`, wire the kind into consent and doctor by those methods alone, and note that every installed hook command enters the [trust hash](../internals/harness/trust.md).

## Step 6 — Wire launch, resume, and presets

From the worksheet's launch row: `permission_args` for the four [`PermissionMode`](../../crates/rimz/src/harness/run.rs)s, `render_preset` (reject any `agents.toml` preset field the agent cannot render, so launch intent is never silently dropped), `resume_command` and `fork_command`, `compact_command` (a registry test requires every adapter to answer `/compact`-style smart compaction), `ping_args` (returning `Some` is what lights up `<kind>-ping`), and `launch_command`/`launch_env`/`default_launch_model` where the stock invocation needs shaping.

## Step 7 — Wire context enrichment

Implement the context source(s) the worksheet found; either alone is valid, and enrichment is never correctness — a failure is an omitted field, never an error ([model.md → Enrichment](../internals/agents/model.md#enrichment)).

- **Transcript tail**: locate the transcript from the hook payload and parse the newest usage record on top of [`read_transcript_tail`](../../crates/rimz/src/agents/transcript_fs.rs) under the [reading rules](../internals/agents/model.md#reading-rules) — bounded, newest-first, lossy, and explicit about zero (fresh session) vs unknown (unreadable).
- **Rich transport**: normalize the provider's out-of-band payload through `observe_context` into [`AgentContext`](../../crates/rimz/src/agents/context.rs), every field `Option`, tolerantly parsed; `local_context_refresh` and `context_refresh_spawn` hook the refresh triggers.
- **Payload-stamped gauge**: when RimZ authors the hook wire itself (an in-process extension like Pi's), stamp the gauge onto every envelope and skip the tail entirely.

## Step 8 — Wire account, spend, and pricing

The provider half of the integration is [providers.md → Adding a provider](../internals/agents/providers.md#adding-a-provider): `probe_account` (login state, plan, rate-limit windows), the OAuth-usage probe where one exists, and full-history cost through `transcript_files` plus `parse_spend`. Ship a `spend_fixture` — conformance requires one yielding a positive session cost unless the descriptor declares `RealtimeCost` unsupported.

`spend.rs` is sidebar-safe by construction: read-only, and the `ensure_spend_parser_boundaries` invariant grep ([`xtask/src/invariants.rs`](../../xtask/src/invariants.rs)) rejects store-write, run-wake, and broker imports in any spend path.

## Step 9 — Test it

The required set, consolidated from [model.md → Adding an agent](../internals/agents/model.md#adding-an-agent) and the [module contract](../../crates/rimz/src/agents/AGENTS.md):

- install / uninstall / preview, and install version drift (an under-wired config re-offers the merge)
- lifecycle mapping: native event → observation → state, through the real `step`
- ask classification, and neutral silence as an inline `insta` golden per blocking event
- malformed-payload handling goldens
- PID attribution
- context mapping from a fixture transcript tail and a fixture transport payload, including the fresh-session zero and unreadable-unknown cases
- the spend fixture parsing to real entries

Conformance, the descriptor subset test, and the registry uniqueness tests enroll the adapter automatically — no opt-in. Unit tests follow the one-home rule: inline `#[cfg(test)] mod tests` until the size gate moves them to a sibling ([rust-conventions.md → Tests](./rust-conventions.md#tests)).

## Step 10 — Ship the documentation

- Write `docs/internals/agents/<kind>.md` on the per-kind skeleton — *Hooks and lifecycle* (the native event → signal table is its core), *Context and transcript*, *Account and balance*, *Cost* — with [pi.md](../internals/agents/pi.md) as the small template and [claude.md](../internals/agents/claude.md) showing where marker subsections earn their place.
- Link it everywhere the existing kinds are linked: the per-kind lists in [model.md](../internals/agents/model.md) and [`crates/rimz/src/agents/AGENTS.md`](../../crates/rimz/src/agents/AGENTS.md), and the root [AGENTS.md](../../AGENTS.md) documentation map.
- Update [agent-support.md](../reference/agent-support.md): a coverage-matrix row, a per-agent detail section, and removal from *Agents not yet supported*.
- Update the [README](../../README.md) agent compatibility matrix.

## Step 11 — Gate it

Done means: `cargo xtask gate` is green (format, invariants, docs-links, lint, fast tests); `rimz coverage` shows the kind with its wired/partial/unsupported rows reading true; `rimz doctor` reports the hook install state; and `rimz agents <kind>` launches the stock CLI into a pane whose row registers, runs, asks, and settles in the sidebar. Escalate to the journey and live-backend tiers when the change touches their surfaces ([AGENTS.md → Testing requirements](../../AGENTS.md#testing-requirements)).

## The deliverables checklist

- [ ] `docs/externals/agent-adapter/<kind>-reference.md` — upstream protocol reference, pinned to sources
- [ ] Mapping worksheet: 11 lifecycle signals, 16 concerns, asks, tools, identity, context sources, spend, launch argv
- [ ] `crates/rimz/src/agents/<kind>/mod.rs` — adapter, descriptor, both matrices
- [ ] `payloads.rs` typed wire · `spend.rs` · `account.rs` (± `oauth_usage.rs`) · install surface
- [ ] `pub mod` + re-export in `agents/mod.rs`, one line in `registry::ADAPTERS`
- [ ] `classify_hook` · `observe_lifecycle` · `render_neutral` · `classification_corpus` · `installed_hook_events`
- [ ] Install / preview / uninstall / `hooks_installed`
- [ ] `permission_args` · `render_preset` · `resume_command` · `compact_command` · `ping_args`
- [ ] Context source(s): tail parse, `observe_context` transport, or payload-stamped gauge
- [ ] `probe_account` · `transcript_files` + `parse_spend` · `spend_fixture`
- [ ] The step-9 test set, stdout shapes as inline `insta` goldens
- [ ] `docs/internals/agents/<kind>.md` + links in model.md, the module contract, and the root documentation map
- [ ] `agent-support.md` row and section · README matrix
- [ ] `cargo xtask gate` green · `rimz coverage` honest · `rimz doctor` reporting
