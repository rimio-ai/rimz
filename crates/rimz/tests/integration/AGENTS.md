# Integration suite

Local contract for `crates/rimz/tests/integration/` — the crate's single integration-test binary. Extends [crates/rimz/AGENTS.md](../../AGENTS.md); it never restates parent rules. Test tiers and runner rules live in [rust-conventions.md](../../../../docs/contributing/rust-conventions.md#tests).

## Harness

- One binary: every suite is a module of [`main.rs`](./main.rs), and [`common/`](./common/mod.rs) is declared once — no per-file harness duplication.
- Pick the tier by what the test drives: `common::Env` runs the `rimz` binary out of process with HOME, XDG, `TMUX_TMPDIR`, and `ZELLIJ_CONFIG_DIR` scoped to tempdirs; `common::Harness` opens a real in-process `rimz::Store` for direct API tests; `common::payloads` holds the golden agent hook payloads and environment probes both tiers share.
- Real tempdir, real store files — no in-memory stubs.
- Every builder that runs `rimz` or creates a mux server scrubs the ambient session env at construction (`common::ScrubSessionEnvExt` — the `RIMZ_*` identity pin and the `ZELLIJ*`/`TMUX*` detection vars), so a suite run from inside a live RimZ room behaves like a clean shell; a test that needs one of these sets it explicitly afterwards.

## Placement

- Subdirectory matches tier: `store/` durability and CAS, `backend/` live-mux parity, `examples/` the embedded and shipped script surfaces (the Pi extension, mux config samples), `journey/` rendered user flows, `performance/` bounded resource use. A new suite lands where its tier says, not beside a similar-looking file.
- Host dependencies self-skip: a test that needs `zellij`, `tmux`, or `python3` probes for the binary and skips when absent — CI never requires an installed mux.
- External seams are faked with the [`tests/fixtures/`](../fixtures/) shims (`zellij-trace`, `git-trace`, `ssh-trace`, `codex-appserver-stub`); mux-driving tests route `rimz` invocations at an isolated tmux server env and a private Zellij runtime so a developer's live sessions stay untouched.
- Time is deterministic: fixed-epoch fixtures, boundary-exact.
