# Agent adapters

Local contract for `crates/rimz/src/agents/` — the integration layer. Extends [crates/rimz/AGENTS.md](../../AGENTS.md); it never restates parent rules.

Topic detail lives in the internals leaves the root map describes — [adapter.md](../../../../docs/internals/agents/adapter.md) (this layer: the registry, the capability traits, the hook path, install, context sources, and declared coverage), [model.md](../../../../docs/internals/agents/model.md) (the agent model the adapters feed: the rollup, the state machine, and the displayed-status ladder), the per-kind internal mappings ([claude.md](../../../../docs/internals/agents/claude.md) and its Codex, Copilot, Kimi, Pi, OpenCode, Antigravity, Cursor, Droid, Kiro, Qwen, and Grok siblings), [providers.md](../../../../docs/internals/agents/providers.md) (accounts, balances, spend, and pricing), and the per-agent upstream references in [agent-adapter/](../../../../docs/externals/agent-adapter/claude-reference.md). The sequenced integration playbook for a new built-in is [agent-adapters.md](../../../../docs/contributing/agent-adapters.md).

## Layout

- Shared, kind-agnostic code sits at the top level — [`definition.rs`](./definition.rs) owns the `AgentDefinition` catalog value and immutable `AgentSpec`, [`capabilities.rs`](./capabilities.rs) owns caller-aligned workflow contracts and the `AgentIntegration` bundle every adapter implements, and neutral services own lifecycle, session, context, runtime-control, account, transcript, and spending policy. Per-file detail lives in the `//!` headers.
- Private provider implementations live below [`adapters/`](./adapters/mod.rs). Each built-in directory owns its native payloads, parsers, installer, probes, and selected capability implementations; [`adapters/plugin/`](./adapters/plugin/mod.rs) loads validated machine-tier process plugins, while [`plugins.rs`](./plugins.rs) is the public provider-neutral plugin façade.
- `spend.rs` is the read-only, sidebar-safe full-history cost parser; a CI grep keeps every `spend.rs` free of store-write, run-wake, and broker imports.
- Amp, Pi, and OpenCode own their wire: each integrates through RimZ-authored in-process TypeScript installed whole-file — [`amp/plugin.ts`](./adapters/amp/plugin.ts), [`pi/extension.ts`](./adapters/pi/extension.ts), and [`opencode/plugin.ts`](./adapters/opencode/plugin.ts) — so the payload schema is RimZ's by design and drift is a RimZ bug, never an upstream one.
- OpenCode's provider-local [`database.rs`](./adapters/opencode/database.rs) owns storage discovery and read-only SQLite access below account, transcript, and spend consumers.

## The boundary

An adapter is the *single* place an agent protocol is normalized. It implements every capability trait — with behavior where it has any, and an empty `impl` where it has none, so a gap is visible in the adapter rather than inferred from a list elsewhere. Each trait method carries a default, and that default is the single home for "this agent does not do that": no dispatch layer restates it. [`registry::BUILTINS`](./registry.rs) names one `AgentDefinition` per adapter. Add a third-party agent with the manifest and canonical wire under [`adapters/plugin/`](./adapters/plugin/mod.rs). Add a built-in with a private adapter directory, an `AgentSpec`, capability implementations (empty where unsupported), and one registry definition.

Provider modules own native protocol and process interpretation. Code outside `agents` resolves definitions and calls neutral services; the `xtask` private-adapter invariant keeps provider modules and concrete adapter types behind this boundary.

`AgentDefinition::probe_account_usage` returns one identity-bearing provider-neutral result. Provider transports parse native wire shapes and provider account modules normalize plan/windows/credits. Sidebar refresh owns account cadence, durable claims, and cache publication; `agents::spending` owns fleet-spending cadence, refresh, and publication while sidebar supplies workspace inputs and consumes its durable results.

- **Plugin manifests derive claims.** The declared canonical event list, capabilities, and probes generate both descriptor matrices. Keep hook installation self-managed and neutral output empty; probes stay bounded, read-only, and off the store import graph.
- **Read upstream JSONC without rewriting it.** Read-only probes of user-editable upstream JSON files parse through [`jsonc.rs`](./jsonc.rs); install flows that rewrite a file keep strict parsing so comments are never silently discarded.

- **Adapters return provider-neutral intent.** [`cli/hooks.rs`](../cli/hooks.rs) binds runtime identity, emits only `HookReply` on stdout, and submits lifecycle write intent to Store.
- **Local context policy stays in adapters.** Local refreshes emit explicit `FieldPatch` and `LocalTokenPatch` operations; Store applies them under the record lock without branching on provider kind.
- **One command runs every provider's out-of-band refresh.** An adapter's `context_refresh_spawn` returns argv for the shared `rimz agents refresh-context` helper, and `refresh_session_context` returns that pass's write intent. The adapter reads only its own provider source; the CLI owns every durable write and the sidebar wakeup.
- **Emit only the normalized outputs generic flows consume** — lifecycle observations, blocking-ask classifications, hook ingress ownership, interactive process identity, and supervised-run final messages. Keep provider-only capabilities in deep provider modules until a second generic consumer proves an adapter seam.

## Hook discipline

- **Blocking ask hooks are sync.** Installing one as async is a hard install error — the source of truth for "must block" is the adapter's hook catalog or equivalent typed installer declaration, never the on-disk config.
- **Neutral follows the agent's decision contract.** Claude, Codex, Pi, OpenCode, and Grok use empty stdout; Cursor returns `{}` on every wired event because its hook contract requires JSON. Diagnostics stay off stdout. The fresh-stdio rule for helper children (a wrapped statusline, a notifier) is enforced by the no-`Stdio::inherit` CI grep.
- **Set installed hook timeouts from upstream deadlines**, leaving margin for RimZ to finish store writes before the agent kills the hook.
- **Install is idempotent.** Reclaim every rimz-owned entry by the stable command substring before rewriting the canonical set; leave user-authored entries untouched.

## Tests

Golden every stdout shape in the adapter's `tests` module with inline `insta::assert_*_snapshot!(... @"...")`: the neutral no-op and malformed-payload handling. Cover install/uninstall, lifecycle mapping, ask classification, and PID attribution.

Declare both halves of the support contract in the descriptor. `coverage` states the mechanism: every integration concern as wired, partial (no native signal, reconstructed by derivation, with the gap named), or unsupported. `user_coverage` states the behavior: every user capability as full, partial (what lands, plus the limit), or unsupported, written in what the user sees and when.

`conformance.rs` enforces completeness on both and grounds them one-directionally — a full capability rests on wired mechanism, an unsupported capability on unsupported mechanism — while cross-checking each concern claim against capabilities, installed events, the classification corpus, and the realtime-cost spend fixture. A wired concern still rolls up to partial when the user-visible result is incomplete or arrives late; that judgement is the adapter's.

Write capability text as product language: it prints verbatim to users, so keep it lowercase, without a trailing period, and about six to fourteen words. The rubric lives in [agent-support.md](../../../../docs/reference/agent-support.md). `rimz coverage` leads with the capability grid and `rimz coverage --wiring` adds the concern and lifecycle-hook grids, so a missing surface is visible product behavior.
