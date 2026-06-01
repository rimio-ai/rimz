# Agent adapters

Local contract for `crates/rimz/src/agents/` — the integration layer. Extends the root [AGENTS.md](../../../../AGENTS.md); it never restates parent rules. The decision/lifecycle hook model lives in [docs/internals/hooks.md](../../../../docs/internals/hooks.md); the context read-path — transcript-tail parsing and the rich-context transports — is [docs/internals/transcript.md](../../../../docs/internals/transcript.md); the agent-state rollup is [docs/internals/agent.md](../../../../docs/internals/agent.md).

## The boundary

An adapter is the *single* place a native agent protocol is normalized. It owns `classify_hook`, `observe_lifecycle`, `render_decision`, `render_neutral`, and install / uninstall / preview for one agent. Adding an agent is implementing [`AgentIntegration`](./mod.rs) and nothing else.

- **Adapters never touch the ledger.** They are pure mappers; [`cli/hooks.rs`](../cli/hooks.rs) owns every ledger write and all bridge I/O, calling the adapter for classification and rendering only.
- **Emit only the two outputs downstream consumes** — an `AgentLifecycleObservation` and the decision `Value`. A native event name or payload field reached for outside this module is a mapping that belongs *in* an adapter.
- **Decision JSON is per-agent.** Never reuse one agent's decision shape for another — the providers diverge (e.g. Codex rejects `updatedInput` / `interrupt`). Render each agent's own shape.

## Hook discipline

- **Blocking decision hooks are sync.** Installing one as async is a hard install error — the source of truth for "must block" is the adapter's `BLOCKING_EVENTS`-style constant, never the on-disk config.
- **Hook stdout is the decision channel.** It carries the decision JSON on a resolver answer and nothing else; the neutral path is empty stdout. Any helper child (a wrapped statusline, a notifier) gets fresh, fully-piped stdio — never `Stdio::inherit` (CI grep).
- **Set `hook_cap` from the upstream's published deadline**, leaving margin so the bridge times out before the agent kills the hook.
- **Install is idempotent.** Reclaim every rimz-owned entry by the stable command substring before rewriting the canonical set; leave user-authored entries untouched.

## Tests

Golden every stdout shape inline in the adapter module with `insta::assert_*_snapshot!(... @"...")`: neutral no-op, allow, deny, modified-input (where supported), malformed payload, and version-drift fallback. Cover install/uninstall, lifecycle mapping, feed classification, and PID attribution. Run through `cargo xtask test`.
