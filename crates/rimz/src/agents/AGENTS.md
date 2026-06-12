# Agent adapters

Local contract for `crates/rimz/src/agents/` — the integration layer. Extends [crates/rimz/AGENTS.md](../../AGENTS.md); it never restates parent rules.

Topic detail lives in the internals leaves the root map describes — [agent.md](../../../../docs/internals/agents/agent.md) (the agent model, the adapter boundary, and the adding-an-agent recipe), the per-kind internal mappings in [adapter/](../../../../docs/internals/agents/adapter/claude.md), [provider.md](../../../../docs/internals/agents/provider.md) (accounts, balances, spend, and pricing), and the per-agent upstream references in [agent-adapter/](../../../../docs/externals/agent-adapter/claude-reference.md).

## Layout

- Shared, provider-agnostic code sits at the top level — the [`AgentAdapter`](./mod.rs) trait and its descriptor/registry/lifecycle/context companions, the wire enums, the account probe contract, [`spending`](./spending.rs) aggregation, and the [`pricing/`](./pricing/mod.rs) tables; per-file detail lives in the `//!` headers.
- Each provider is a sibling directory ([`claude/`](./claude/mod.rs), [`codex/`](./codex/mod.rs), [`pi/`](./pi/mod.rs)) owning its integration, typed payloads, rich-context transport, account probe, and `spend.rs`.
- `spend.rs` is the read-only, sidebar-safe full-history cost parser; a CI grep keeps every `spend.rs` free of ledger-write, bridge, and broker imports.
- Pi owns its wire: pi has no hook config — install ships the Rimz-authored [`pi/extension.ts`](./pi/extension.ts), so the payload schema is Rimz's by design and drift is a Rimz bug, never an upstream one.

## The boundary

An adapter is the *single* place a native agent protocol is normalized. It owns `classify_hook`, `observe_lifecycle`, `render_decision`, `render_neutral`, and install / uninstall / preview for one agent. Adding an agent is implementing [`AgentAdapter`](./mod.rs), declaring its [`AgentDescriptor`](./descriptor.rs), and one line in [`registry::ADAPTERS`](./registry.rs) — nothing else.

- **Adapters never touch the ledger.** They are pure mappers; [`cli/hooks.rs`](../cli/hooks.rs) owns every ledger write and all bridge I/O, calling the adapter for classification, rendering, and normalized run output.
- **Emit only the normalized outputs downstream consumes** — an `AgentLifecycleObservation`, a decision `Value`, and a supervised-run final assistant message. A native event name or payload field reached for outside this module is a mapping that belongs *in* an adapter.
- **Decision JSON is per-agent.** Never reuse one agent's decision shape for another — the providers diverge (e.g. Codex rejects `updatedInput` / `interrupt`). Render each agent's own shape.

## Hook discipline

- **Blocking decision hooks are sync.** Installing one as async is a hard install error — the source of truth for "must block" is the adapter's `BLOCKING_EVENTS`-style constant, never the on-disk config.
- **Neutral is empty stdout.** A resolver answer renders as the agent's own decision JSON; anything else stays off stdout. The fresh-stdio rule for helper children (a wrapped statusline, a notifier) is enforced by the no-`Stdio::inherit` CI grep.
- **Set `hook_cap` from the upstream's published deadline**, leaving margin so the bridge times out before the agent kills the hook.
- **Install is idempotent.** Reclaim every rimz-owned entry by the stable command substring before rewriting the canonical set; leave user-authored entries untouched.

## Tests

Golden every stdout shape in the adapter's `tests` module with inline `insta::assert_*_snapshot!(... @"...")`: neutral no-op, allow, deny, modified-input (where supported), malformed payload, and version-drift fallback. Cover install/uninstall, lifecycle mapping, feed classification, and PID attribution.
