# Store

Local contract for `crates/rimz/src/store/`: durable workspace state. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). [snapshot/AGENTS.md](./snapshot/AGENTS.md) extends this one for the fold and the snapshot it builds.

[store.md](../../../../docs/internals/store.md) owns the on-disk shape, the event log, the lock order, the write and read paths, and maintenance: read the matching section before changing one. Its [Where the code lives](../../../../docs/internals/store.md#where-the-code-lives) says which file owns what and which way imports run.

- **Invariant.** Every durable write routes through [`disk/atomic.rs`](../disk/atomic.rs), and every fsync syscall lives inside it: `cargo xtask invariants` rejects a `sync_all` or `sync_data` call anywhere else.
- Appends stay O(1) over log history.
- Worktrees share one store, so binaries of different versions read each other's records. Before you change a type that an event's params or a message record carries, read [What is in it](../../../../docs/internals/store.md#what-is-in-it).
