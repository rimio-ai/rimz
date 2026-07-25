# Diagnostics

Local contract for `crates/rimz/src/diag/` — append-only JSONL evidence. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). Subsystem behaviour — the schema table, the envelope, rate limiting, frame captures, off-box reporting — lives in [docs/internals/diagnostics.md](../../../../docs/internals/diagnostics.md).

## Evidence, not input

- **Diagnostics are for a human.** Correctness reads the store, the CAS rules, and the caches. No correctness path reads a diagnostic record back, and nothing here gates product behaviour.
- **Records are anomaly-only.** A steady-state tick writes nothing, which is what keeps the log readable when something does go wrong.
- **Writes are best-effort.** A failed append logs at debug and returns; the calling path continues unchanged.
- **Only the sink touches disk.** Pure projection layers return diagnostics as data and hand them to `DiagSink`, so a fold stays testable and quiet.
- Rate limiting lives with the sink: an identity window plus a per-kind ceiling bounds a loop that would otherwise flood the log.

## Accountability

An internal repair keeps a durable record of what it did — [`focus_repair.rs`](./focus_repair.rs) is the worked example, pairing automatic sidebar focus repair with its own rotating log. User-benefiting automation instead appends an assist record and surfaces in `rimz stats`; the split lives in [loops.md](../../../../docs/internals/harness/loops.md#the-assist-log).

## Layout and boundaries

- [`diag.rs`](../diag.rs) owns the sink, the rate limiter, and the frame-capture ring. It also owns the main diagnostics log name and the frame-capture directory name; `cargo xtask invariants` rejects those two literals anywhere else, so the primary surface stays enumerable from one file.
- [`record.rs`](./record.rs) owns the schema and its version; [`rotating.rs`](./rotating.rs) is the shared rotating JSONL helper, with schema and path left to the caller.
- Per-surface logs stay in their own file: [`notify.rs`](./notify.rs), [`binding.rs`](./binding.rs), [`focus_repair.rs`](./focus_repair.rs), [`plugin_presence.rs`](./plugin_presence.rs). Each is documented by the subsystem that writes it.
- [`store/`](../store/AGENTS.md) owns durable truth and never consumes a record from here. `observability.rs` is the separate opt-in off-box channel behind the `sentry` build feature; keep local-only episodes local.
- Snapshot projection stays quiet: the invariant rejects `warn!` and `error!` under `store/snapshot/`, so a per-fold diagnostic never floods the off-box channel.

## Tests

Severity mapping, rotation, and rate-limit windows stay in-module beside the file they cover. Fixtures pin time, so a record's envelope is asserted rather than approximated.
