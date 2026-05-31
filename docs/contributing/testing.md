# Testing

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

The contract this suite enforces is in [DESIGN.md](../../DESIGN.md). Tests prove it before real agent integrations ship.

Local runner: `cargo xtask test` (wraps `cargo nextest run`). Doctests run separately via `cargo xtask doctest`. Inside `cargo xtask ci` the suite runs once under coverage (`cargo llvm-cov nextest`), not as a second uninstrumented pass. The full gate stack is in [rust-conventions.md](./rust-conventions.md).

## Test tiers

- **Function/unit tests** live inline with the module under test and cover pure state transitions, parsers, render helpers, trust hashing, and formatting rules. They do not spawn subprocesses or touch real ledgers.
- **Integration tests** live under `crates/rimz/tests/integration/` and cover public CLI, ledger files, sockets, hooks, resolvers, and subprocess round trips through the shared harness.
- **Journey tests** live under `tests/integration/journey/` and assert rendered end-user flows through a real `rimz-sidebar serve` process and `vt100` screen capture.
- **Live-backend tests** cover real tmux/Zellij behavior and self-skip when the backend binary or socket permissions are unavailable. They stay narrow and backend-specific.
- **Performance tests** assert bounded work, fork counts, cache behavior, or single-flight behavior. They do not duplicate product semantics already covered by unit or journey tests.

Do not land ignored tests for future product targets. Keep planned behaviour in the roadmap or design docs, then add the executable test when the implementation is ready to pass in the normal nextest suite.

## M0 synthetic matrix

**M0a** runs without Zellij or tmux:

- project / worktree identity,
- ledger writes and snapshot rebuild,
- default-mode hook path (`native_ui`),
- resolver-mode bridge path (`bridge`),
- script-blocking path (`script` via `feed ask`),
- CAS resolution,
- nonce mismatch rejection,
- timeout and late-answer behaviour,
- torn event-log recovery,
- lock recovery,
- runtime path fallback (`/tmp/rimz-<uid>/` when `XDG_RUNTIME_DIR` is unset).

**M0b** runs the same matrix under Zellij and adds:

- session birth from a layout via `attach --create-background ... --default-layout` (left 30% `rimz-sidebar` pane + focused terminal); a second launch on the existing session is a no-op,
- self-close: a real `rimz-sidebar` whose tab's last terminal pane exits closes its own pane (non-plugin pane count drops to zero),
- sidebar heartbeat socket,
- the wakeup walk fans out one datagram per fresh instance and spawns no `zellij pipe` (the consumerless broadcast was removed; re-arms only with the future rail),
- `list-panes -j` parsing of `pane_command` and `pane_cwd` into `PaneRef`,
- minimum Zellij version detection.

**M0c** runs the same matrix under tmux and adds:

- native managed sidebar pane with workspace ID, cwd, and width-percentage passthrough,
- explicit `RIMZ_*` env injection with `tmux -e`,
- detach/reattach,
- `list-panes -F` parsing of `#{pane_current_command}` and `#{pane_current_path}` into `PaneRef`,
- optional status-line integration trust prompt,
- optional popup smoke test for tmux ≥ 3.2,
- minimum tmux version detection.

## Invariants — grep-style CI checks

**Decision-channel integrity.**
- No `Stdio::inherit` in hook subprocess paths.
- Blocking decision hooks are never installed as async.

**Sidebar / ledger separation.**
- No sidebar code imports ledger writer APIs.

**Trust-hash completeness.**
- Every command-executing config field enters the executable-surface hash.

**Pane primitive boundary.**
- No Rimz core path automatically calls `pane capture` or `pane send`.

**Removed dependencies.**
- No `chrono::` imports in workspace crates (timestamps go through `jiff`).
- No `bytes::` or `tokio_util::` imports in workspace crates.

## Agent goldens

For every supported agent event:

- neutral timeout stdout,
- allowed decision stdout,
- denied decision stdout,
- modified-input decision stdout where supported,
- malformed payload handling,
- unsupported version fallback.

## Snapshot discipline

All `insta` snapshots — CLI stdout, `--json` event payloads, hook stdout, sidebar render frames — share one set of rules.

- **Redaction filter.** Every snapshot routes through `tests/integration/common/redact.rs` before comparison. The filter strips UUIDs (`[0-9a-f]{8}-[0-9a-f]{4}-...`), Unix and RFC3339 timestamps, absolute paths under `$HOME` / `$XDG_RUNTIME_DIR` / `$XDG_STATE_HOME`, the workspace ID, and the multiplexer session name. Snapshots compare semantic shape, not transient identifiers. Snapshot churn from a transient ID is a redactor bug; fix the redactor.
- **Failure-shape snapshots.** Error messages, `--json` error envelopes, and hook neutral silence are snapshotted alongside success cases. Wire-shape error changes are reviewed events, not silent regressions.
- **Sidebar render snapshots.** `crates/rimz-sidebar` snapshot tests render through a `vt100::Parser`-backed ratatui backend and assert on the parsed screen contents, never on widget internals. Resize the backend within the test to exercise wrapping and truncation.

## Sidebar tests

- `fresh_sidebar_present` ignores absent directories, stale mtimes, unreadable JSON, and old heartbeat protocols.
- Start/attach orchestration skips launch on fresh heartbeat, opens once without a heartbeat, and treats `open_sidebar` errors as non-fatal.
- `native_ui` items render focus/dismiss only (never approve/deny).
- `script` items render declared options as answer buttons.
- `bridge` items enter the resolver group after the threshold.
- Worktree grouping is stable across reloads.
- Pane focus refuses reused pane IDs (reconciles process start time).
- Method labels render correctly for `hook_bridge`, `pane_send`, `cli`, and `sidebar`.

**Presence and liveness** (the reducer folds the live pane list in):

- A pane running a shell renders as a process row; a pane an agent stamped renders as that agent's row — one row, never both, and no pane id shared by two rows.
- Binding is by the hook-stamped pane id alone: a pane whose command/cwd merely *look* like an agent that never stamped it stays a process row (`agent_binds_only_by_stamped_pane_id`), and N panes yield N rows with no duplicated pane id (`each_live_pane_yields_exactly_one_row`).
- The one exception is the Codex app-server daemon, which fires hooks with no mux pane env so its session never stamps a pane: a pane-less `codex` agent binds the live `codex` pane in its own worktree by cwd (`paneless_codex_agent_binds_to_its_worktree_pane`), never a pane in another worktree (`paneless_codex_agent_in_other_worktree_stays_a_process_row`) and never a nested-worktree pane (`paneless_codex_agent_does_not_capture_a_nested_worktree_pane`); the cwd bind is `codex`-only (`paneless_codex_does_not_bind_a_non_codex_pane`, `paneless_claude_agent_is_never_rescued_by_cwd`), and two pane-less Codex sessions in one worktree bind the most-recently-active (`two_paneless_codex_in_one_worktree_bind_most_recent`).
- Panes group by their cwd's worktree; a pane outside every worktree lands in the `workspace` catch-all.
- The sidebar's own pane is excluded from the roster.
- Liveness: an agent renders only on its stamped live pane; a pane it no longer holds (reverted to a shell, closed, or absent from the list) drops the agent row. Rollup hygiene reaps pidless-past-TTL and superseded ghost sessions separately.
- Pending script attention produces a standalone row only while the owning `feed ask` waiter is live; pending agent attention folds onto the agent's stamped-pane row (one ask per session), and with no live stamped pane it does not render, so stale prompts cannot outlive their pane.
- Process rows sort below every agent row and are part of the cap-truncatable tail.

## End-user journey suite

`tests/integration/journey/` tells the session as a story from `docs/guide/product.md` and `docs/guide/experience.md`: launch the room, onboard, run an agent, watch the column move through `shell → idle → running → waiting → fleet`. `docs/internals/sidebar.md` owns renderer mechanics, not the story source. "Running an agent" fires its *installed* hook, never a hand-rolled `rimz hooks feed`: the harness onboards with `rimz hooks install` and then runs the exact `rimz hooks feed --source <agent> --event <event>` command the agent's config wires. An un-onboarded `agent_hook` is a no-op — exactly what a real agent does with no Rimz hook configured — so the suite fails when "I ran codex and nothing showed up" would. That faithfulness is non-negotiable: a journey test that fires hooks an un-wired agent could never fire would pass against a broken product.

- **Content** (`sidebar_phases.rs`) drives the real `rimz-sidebar serve` renderer through a `portable-pty` over a real ledger and asserts on the `vt100`-parsed pane (the `resize_redraw.rs` pattern). The renderer gets its own short `XDG_RUNTIME_DIR` so the per-instance wakeup socket stays under the AF_UNIX limit.
- **Deep smokes** (`deep.rs`) birth a real tmux/zellij session with a real sidebar pane, fire a hook, and capture the live pane content; they self-skip without the mux binary. They poll for a *complete* frame (every expected token) so a partial repaint captured mid-paint under load never reads as a failure.
- **Layout, tabs, and focus** live in `backend/zellij.rs` (left-30% sidebar, focused right terminal, every new tab born with the same split); tmux per-window parity — every new window born with its own sidebar via the `after-new-window` hook — is `backend/tmux.rs::new_window_is_born_with_a_sidebar_and_focused_terminal`.
- The whole journey suite is green against `main`, with no ignored future-target tests. Document planned phases in the roadmap until implementation makes the executable journey assertion pass under the normal nextest suite.

## Attach tests

- Auto mode execs the mux attach command only when stdin and stdout are TTYs and the caller is not already inside the selected mux.
- Non-TTY callers print the attach command.
- `--attach` forces exec; `--no-attach` and `--print` force print.

## Resolver tests

- Allowlist enrolment.
- Unauthorized heartbeat diagnostics (`rimz doctor` surfaces; bridge does not engage).
- Binary pinning where supported.
- Explicit `feed abstain` advances the chain.
- Budget elapse advances the chain.
- Out-of-turn resolver answer rejected by CAS.
- Human `--override-chain` accepted.
- Chain exhaustion falls back to native prompt.
- Stale heartbeat mid-chain is skipped.
