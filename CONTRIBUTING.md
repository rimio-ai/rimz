# Contributing

Rimz uses the Rust toolchain pinned in `rust-toolchain.toml`; install from source with:

```sh
cargo xtask install
cargo xtask test
cargo xtask ci
```

`cargo xtask test` runs the nextest suite. `cargo xtask ci` runs the full gate stack: formatting, invariants, doc links, audits, plugin build, clippy, doctests, coverage, and semver checks.

Contributor rules live in [AGENTS.md](./AGENTS.md). Rust shape, test tiers, command surface, and gate details live in [docs/contributing/rust-conventions.md](./docs/contributing/rust-conventions.md).
