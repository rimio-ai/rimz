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

The xtask commands detect it automatically. To cover direct Cargo commands too, add these keys to the `[build]` table in `~/.cargo/config.toml`:

```toml
[build]
rustc-wrapper = "sccache"
incremental = false
```

Keep each Git worktree's Cargo `target/` directory local to that worktree. Share compiler work through sccache; do not list `target` in `.worktreelink`, because Cargo fingerprints and final executables from divergent branches can overwrite one another. See the [toolchain and cache guide](./docs/contributing/rust-conventions.md#toolchain) and the [worktree seeding guide](./docs/guide/worktrees.md#seed-the-tree) for details.

Contributor rules live in [AGENTS.md](./AGENTS.md). Rust shape, test tiers, command surface, and gate details live in [docs/contributing/rust-conventions.md](./docs/contributing/rust-conventions.md).
