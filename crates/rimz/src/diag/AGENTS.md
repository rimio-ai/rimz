# Diagnostics

Local contract for `crates/rimz/src/diag/`. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). Behaviour lives in [diagnostics.md](../../../../docs/internals/diagnostics.md). Before adding a kind, a record shape, or a log surface, read its [Rules a change keeps](../../../../docs/internals/diagnostics.md#rules-a-change-keeps).

- **Invariant.** Diagnostics are evidence for a human. No correctness path reads a record back, nothing here gates product behaviour, and `store` constructs diagnostics without ever consuming one.
- **Invariant.** [`diag.rs`](../diag.rs) alone names the main log file and the frame-capture directory; `cargo xtask invariants` rejects those two literals in any other source file.
- **Invariant.** `cargo xtask invariants` rejects `warn!` and `error!` under `store/snapshot/`: a projection re-folds per frame, and either would flood the off-box channel.
- Writes are best-effort: a failed append logs at debug and returns, and the caller continues unchanged.
- A pure projection layer returns its diagnostics as data.
- Records are anomaly-first.
- A new key or record shape gets a new serde tag, never a distinguishing field on an existing variant. Builds of different ages share one home, and an older build ignores unknown fields, so it would read the new shape as the old one.
- `observability.rs` is the separate off-box channel: keep local-only episodes local.
- An internal repair's durable record (root: Automation is accountable) follows [`focus_repair.rs`](./focus_repair.rs), which pairs the repair with its own rotating log.
- Test fixtures pin time, so a test asserts a record's whole envelope, not an approximation.
