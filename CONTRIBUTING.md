# Contributing

RimZ uses the Rust toolchain pinned in `rust-toolchain.toml`; install from source with:

```sh
cargo xtask install
cargo xtask gate
cargo xtask test
cargo xtask ci
```

`cargo xtask gate` is the everyday pre-PR check: it auto-formats, then runs invariants, doc links, lint, and the fast nextest subset. `cargo xtask test` runs the nextest suite. `cargo xtask ci` runs the local full stack: non-test checks plus the workspace nextest suite.

## Fast local builds

Install `sccache` to reuse compiler outputs across checkouts and worktrees:

```sh
cargo install sccache --locked
```

The xtask commands detect it automatically. To cover direct Cargo commands too, add this key to the `[build]` table in `~/.cargo/config.toml`:

```toml
[build]
rustc-wrapper = "sccache"
```

Leave `incremental` unset. sccache caches the registry dependencies every worktree shares, while Cargo rebuilds the workspace crates incrementally; the large `rimz` crate rarely hits the shared cache across branches, so incremental rebuilds are where its time is saved. Incremental state costs roughly 5-10 GB per active worktree `target/`.

Keep each Git worktree's Cargo `target/` directory local to that worktree. Share compiler work through sccache; do not list `target` in `.worktreelink`, because Cargo fingerprints and final executables from divergent branches can overwrite one another. See the [toolchain and cache guide](./docs/contributing/build-and-ci.md#toolchain) and the [worktree seeding guide](./docs/guide/worktrees.md#seed-the-tree) for details.

Xtask automatically prunes stale Cargo output daily after successful compile tasks; use `cargo xtask prune --dry-run` to preview cleanup (see the [pruning rules](./docs/contributing/build-and-ci.md#target-pruning)).

Contributor rules live in [AGENTS.md](./AGENTS.md). The code shape lives in [docs/contributing/rust-conventions.md](./docs/contributing/rust-conventions.md), the gate stack and test profiles in [docs/contributing/gates.md](./docs/contributing/gates.md), and the toolchain, CI, and release packaging in [docs/contributing/build-and-ci.md](./docs/contributing/build-and-ci.md).
