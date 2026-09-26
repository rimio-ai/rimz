# Integration suite

Local contract for `crates/rimz/tests/integration/` — the crate's single integration-test binary. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). Test tiers and runner rules live in [rust-conventions.md](../../../../docs/contributing/rust-conventions.md#tests).

## Harness

- One binary: every suite is a module of [`main.rs`](./main.rs), and [`common/`](./common/mod.rs) is declared once — no per-file harness duplication. Shared remote-connect PTY and terminal-query behavior lives in `common::ssh_trace`.
- Pick the tier by what the test drives: `common::Env` runs the `rimz` binary out of process with HOME, XDG, `TMUX_TMPDIR`, and `ZELLIJ_CONFIG_DIR` scoped to tempdirs; `common::Harness` opens a real in-process `rimz::Store` for direct API tests; `common::payloads` holds the golden agent hook payloads and `common::shim` the environment probes and fake executables, both shared across tiers.
- Every `Env` and `ZellijNamespace` arms an independent stdin-keepalive reaper before spawning children. The fixture HOME/runtime values are the process-ownership marker: on orderly drop or abrupt owner death the reaper stops both private mux endpoints, terminates every marker-carrying descendant, and removes the roots. A command that calls `env_clear()` must re-pin `HOME` or `XDG_RUNTIME_DIR` before it can become long-lived.
- Real tempdir, real store files — no in-memory stubs.
- `RIMZ_BIN` redirects every detached `rimz` helper spawn; point it at a missing path to exercise a spawn failure, which unit tests cannot reach.
- Every builder that runs `rimz` or creates a mux server scrubs the ambient session env at construction (`common::ScrubSessionEnvExt` — the `RIMZ_*` identity pin, the `ZELLIJ*`/`TMUX*` detection vars, and systemd's `INVOCATION_ID`, which would otherwise route loop ticks through `systemd-run --user` at a fixture runtime dir with no bus), so a suite run from inside a live RimZ room behaves like a clean shell; a test that needs one of these sets it explicitly afterwards.
- Stress a load-only flake in the binary itself, not through nextest: build once with `cargo xtask test --name <module>::<test>`, then loop the freshest `target/debug/deps/integration-<hash>` executable as `<bin> --exact <module>::<test> --test-threads=1` a few hundred times with output redirected to a file, while `nproc` busy loops (`sh -c 'while :; do :; done'`, launched from a script file so the cleanup `pkill -f` cannot match its own shell) hold every CPU. Count non-zero exits before and after the fix; a race that shows 1–3 in 400 loaded runs is typically 0 unloaded.

## Placement

- Subdirectory matches tier: `store/` durability and CAS, `backend/` live-mux parity plus the real-browser web suite (`backend/web/`: real ttyd + headless Chromium, self-skipping without them), `examples/` the RimZ-authored in-process scripts running under their real interpreter (the Pi extension, the OpenCode plugin), `journey/` rendered user flows, `performance/` bounded resource use. A new suite lands where its tier says, not beside a similar-looking file.
- Host dependencies self-skip: a test that needs `zellij`, `tmux`, or `node` probes for the capability and skips when absent — CI never requires an installed mux, and the OpenCode suite additionally skips a `node` that cannot strip TypeScript syntax.
- External seams are faked with the [`tests/fixtures/`](../fixtures/) shims (`zellij-trace`, `git-trace`, `ssh-trace`, `codex-appserver-stub`); mux-driving tests route `rimz` invocations at an isolated tmux server env and a private Zellij runtime so a developer's live sessions stay untouched.
- The journey's hook-firing stub agent fires no lifecycle hook for pasted text, so a message it receives stays `Sent`, never `Delivered`. To prove receipt, fire the receiving turn with `env.run_installed_hook_in_pane` (`UserPromptSubmit` carrying the rendered message text, then `Stop`). Split panes shrink the target pane, so capture with scrollback when asserting on text that may have scrolled.
- Time is deterministic: fixed-epoch fixtures, boundary-exact.
