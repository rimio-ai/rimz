# Agent adapters

Local contract for `crates/rimz/src/agents/` — the integration layer. Extends [crates/rimz/AGENTS.md](../../AGENTS.md); it never restates parent rules.

Topic detail lives in the internals leaves the root map describes — [model.md](../../../../docs/internals/agents/model.md) (the agent model, the adapter boundary, and the adding-an-agent recipe), the per-kind internal mappings ([claude.md](../../../../docs/internals/agents/claude.md) and its Codex, Copilot, Kimi, Pi, OpenCode, Antigravity, Cursor, Droid, Kiro, Qwen, and Grok siblings), [providers.md](../../../../docs/internals/agents/providers.md) (accounts, balances, spend, and pricing), and the per-agent upstream references in [agent-adapter/](../../../../docs/externals/agent-adapter/claude-reference.md). The sequenced integration playbook for a new built-in is [agent-adapters.md](../../../../docs/contributing/agent-adapters.md).

## Layout

- Shared, kind-agnostic code sits at the top level — the [`AgentAdapter`](./mod.rs) trait, [`state.rs`](./state.rs) rollup types, [`open_ask.rs`](./open_ask.rs) provider-neutral state/transcript materialization seam, descriptor/registry/lifecycle/context companions, the account and account-usage probe contracts, [`spending/`](./spending/mod.rs) aggregation, the [`pricing/`](./pricing/mod.rs) tables, and helper modules for payloads ([`payload.rs`](./payload.rs)), identity ([`identity.rs`](./identity.rs)), location ([`locate.rs`](./locate.rs)), provider-path stamps and local-discovery cache policy ([`local_session_cache.rs`](./local_session_cache.rs)), and managed integration sources with whole-file and JSON-merge backends ([`managed_source.rs`](./managed_source.rs)); [`delegated_account.rs`](./delegated_account.rs) is the concrete shared `auth.json` seam used only by Pi and OpenCode; per-file detail lives in the `//!` headers.
- Each built-in agent kind is a sibling directory ([`claude/`](./claude/mod.rs), [`codex/`](./codex/mod.rs), [`amp/`](./amp/mod.rs), [`copilot/`](./copilot/mod.rs), [`kimi/`](./kimi/mod.rs), [`pi/`](./pi/mod.rs), [`opencode/`](./opencode/mod.rs), [`antigravity/`](./antigravity/mod.rs), [`cursor/`](./cursor/mod.rs), [`droid/`](./droid/mod.rs), [`kiro/`](./kiro/mod.rs), [`qwen/`](./qwen/mod.rs), [`grok/`](./grok/mod.rs)) owning its integration and the typed payloads and enrichment surfaces it supports; [`plugin/`](./plugin/mod.rs) is one kind-agnostic adapter for every validated machine-tier process plugin.
- `spend.rs` is the read-only, sidebar-safe full-history cost parser; a CI grep keeps every `spend.rs` free of store-write, run-wake, and broker imports.
- Amp, Pi, and OpenCode own their wire: each integrates through RimZ-authored in-process TypeScript installed whole-file — [`amp/plugin.ts`](./amp/plugin.ts), [`pi/extension.ts`](./pi/extension.ts), and [`opencode/plugin.ts`](./opencode/plugin.ts) — so the payload schema is RimZ's by design and drift is a RimZ bug, never an upstream one.
- OpenCode's provider-local [`database.rs`](./opencode/database.rs) owns storage discovery and read-only SQLite access below account, transcript, and spend consumers.

## The boundary

An adapter is the *single* place an agent protocol is normalized. Its `decode_hook` parses one native event into routing, lifecycle, ask/transcript, sidecar evidence, and neutral stdout; a built-in also owns install / uninstall / preview for one agent. Add a third-party agent with the manifest and canonical wire under [`plugin/`](./plugin/mod.rs). Add a built-in by implementing [`AgentAdapter`](./mod.rs), declaring its [`AgentDescriptor`](./descriptor.rs) — including tool vocabularies and the full `IntegrationConcern` coverage table — and one line in [`registry::ADAPTERS`](./registry.rs).

Provider modules own native protocol and process interpretation. Generic hook ingress consumes `hook_ingress`, and generic process discovery consumes registry command identity; provider-only product coordinators may call deep provider modules when the behavior remains unique to that provider.

`AgentAdapter::probe_account_usage` returns one identity-bearing provider-neutral result. Provider transports parse native wire shapes and provider account modules normalize plan/windows/credits. Sidebar refresh owns account cadence, durable claims, and cache publication; `agents::spending` owns fleet-spending cadence, refresh, and publication while sidebar supplies workspace inputs and consumes its durable results.

- **Plugin manifests derive claims.** The declared canonical event list, capabilities, and probes generate both descriptor matrices. Keep hook installation self-managed and neutral output empty; probes stay bounded, read-only, and off the store import graph.
- **Read upstream JSONC without rewriting it.** Read-only probes of user-editable upstream JSON files parse through [`jsonc.rs`](./jsonc.rs); install flows that rewrite a file keep strict parsing so comments are never silently discarded.

- **Adapters never touch the store.** They are pure mappers; [`cli/hooks.rs`](../cli/hooks.rs) calls the adapter for classification, neutral rendering, and normalized run output, then submits lifecycle write intent to Store.
- **Local context policy stays in adapters.** Local refreshes emit explicit `FieldPatch` and `LocalTokenPatch` operations; Store applies them under the record lock without branching on provider kind.
- **Emit only the normalized outputs generic flows consume** — lifecycle observations, blocking-ask classifications, hook ingress ownership, interactive process identity, and supervised-run final messages. Keep provider-only capabilities in deep provider modules until a second generic consumer proves an adapter seam.

## Hook discipline

- **Blocking ask hooks are sync.** Installing one as async is a hard install error — the source of truth for "must block" is the adapter's hook catalog or equivalent typed installer declaration, never the on-disk config.
- **Neutral follows the agent's decision contract.** Claude, Codex, Pi, OpenCode, and Grok use empty stdout; Cursor returns `{}` on every wired event because its hook contract requires JSON. Diagnostics stay off stdout. The fresh-stdio rule for helper children (a wrapped statusline, a notifier) is enforced by the no-`Stdio::inherit` CI grep.
- **Set installed hook timeouts from upstream deadlines**, leaving margin for RimZ to finish store writes before the agent kills the hook.
- **Install is idempotent.** Reclaim every rimz-owned entry by the stable command substring before rewriting the canonical set; leave user-authored entries untouched.

## Tests

Golden every stdout shape in the adapter's `tests` module with inline `insta::assert_*_snapshot!(... @"...")`: the neutral no-op and malformed-payload handling. Cover install/uninstall, lifecycle mapping, ask classification, and PID attribution.
Declare every integration concern as wired, partial (no native signal, reconstructed by derivation, with the gap named), or unsupported in the descriptor; `conformance.rs` enforces completeness and cross-checks the claim against capabilities, installed events, the classification corpus, and the realtime-cost spend fixture. `rimz coverage` surfaces the same matrix — wired, partial, unsupported — so a missing surface is visible product behavior.
