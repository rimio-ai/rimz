# Contributing

RimZ uses the Rust toolchain pinned in `rust-toolchain.toml`; install from source with:

```sh
cargo xtask install
cargo xtask gate
cargo xtask test
cargo xtask ci
```

`cargo xtask gate` is the everyday pre-PR check: it auto-formats, then runs invariants, doc links, lint, and the fast nextest subset. `cargo xtask test` runs the nextest suite. `cargo xtask ci` runs the local full stack: non-test checks plus the workspace nextest suite.

Contributor rules live in [AGENTS.md](./AGENTS.md). Rust shape, test tiers, command surface, and gate details live in [docs/contributing/rust-conventions.md](./docs/contributing/rust-conventions.md).
