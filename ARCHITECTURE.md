# Architecture

Rimz is a Rust workspace built around one invariant:

```text
project repo == Rimz workspace == multiplexer session
```

Worktrees of the same repo group inside that workspace. Zellij and tmux own panes, views, sessions, attach/detach, and scrollback. Rimz owns project identity, the feed, durable state, resolver trust, hook entrypoints, and the sidebar rendering contract.

Product invariant and operating paths live in [DESIGN.md](./DESIGN.md); they are not restated here.

## Runtime architecture

```text
terminal emulator
  mux session (Zellij or tmux)
    native sidebar pane
    shells, scripts, agents, CI helpers
                │
                │  per-instance sidebar socket  (wakeup of record)
                │  zellij pipe                  (Zellij-only fast path)
                │  hook stdin/stdout            (decision channel)
                ▼
rimz CLI and hook subprocesses
  workspace identity, ledger writes, feed ask/resolve/wait
  mux commands through MuxBackend, hook installers
                │
                ▼
workspace ledger (~/.local/state/rimz/workspaces/<id>/)
  events.log.jsonl   snapshots/latest.json
  feed/<request_id>.json   locks/workspace.lock

runtime directory ($XDG_RUNTIME_DIR/rimz/<id>/)
  sock/feed.<short_id>.sock         per-request decision socket
  sock/sidebar.<instance_id>.sock   wakeup datagram socket
  heartbeat/sidebar.<instance_id>.json
  heartbeat/resolver.<resolver_id>.json
```

The CLI and hook subprocesses are the only durable-state writers. The sidebar reads through `rimz sidebar snapshot` and never touches ledger files. There is no Rimz daemon. The per-instance sidebar socket is the wakeup channel of record; the Zellij pipe is an optimization on top, never a correctness path.

## State ownership

| Owner | Owns | Does not own |
| --- | --- | --- |
| Multiplexer | panes, views, sessions, attach/detach, layout, scrollback | feed state, decisions, resolver trust |
| Rimz ledger | events, feed items, resolutions, snapshots | terminal rendering, pane mechanics |
| CLI / hook subprocesses | durable writes, bridge waiters, mux command calls | UI presentation |
| Sidebar | rendering, focus affordances, human actions through CLI | durable state files |
| Resolvers | optional decisions through the public CLI | core policy, automatic trust |
| Agents | native UI, prompts, sandboxing, bypass behaviour | Rimz feed state |
| Host | process resurrection, OS sandboxing | workspace state |

## Repository layout

Documentation-first today. Module paths are pinned so code can land without re-inventing the shape; entries marked `(planned)` are not yet implemented.

```text
.
|-- AGENTS.md              contributor and coding-agent contract (root)
|-- ARCHITECTURE.md        this file
|-- DESIGN.md              design commitments and the three operating paths
|-- README.md              product entry point
|-- docs/                  focused product and engineering docs
|   |-- guide/             user-facing tour and trust model
|   |-- reference/         CLI surface and configuration
|   |-- internals/         ledger, mux, sidebar, resolvers, agents
|   `-- contributing/      rust conventions, tests, roadmap
|-- Cargo.toml
|-- crates/
|   |-- rimz/              CLI binary plus runtime/domain library
|   `-- rimz-sidebar/      shared terminal sidebar renderer
|-- examples/resolvers/    reference resolver artifacts (Python, stdlib-only)
|-- tests/                 integration tests live under each crate's `tests/`
`-- xtask/                 contributor task runner; entry point for every quality gate
```

Add a crate only when ownership, target type, or dependency profile justifies it. The two-crate shape is intentional: the CLI is one runtime artifact, the sidebar renderer is another.

## Implementation ownership

General coding rules live in [AGENTS.md](./AGENTS.md); only crate-local constraints appear below.

### `crates/rimz`

CLI binary, hook entrypoints, and the runtime/domain library. Start here for any non-sidebar behaviour.

- `src/main.rs` — CLI bootstrap, top-level error reporting.
- `src/cli/` — command parsing and per-subcommand handlers (`workspace`, `list`, `event`, `feed`, `gc`, `pane`, `resolver`, `sidebar`, `hooks`, `trust`, `doctor`).
- `src/workspace.rs` — project root, worktree root, workspace ID, session name.
- `src/trust.rs` — executable-surface hash, per-machine grant record, trust state (no_config / untrusted / trusted / stale). `status` re-hashes every call so staleness is auto-detected.
- `src/ids.rs` — typed identifier newtypes (workspace, request, event, resolver, sidebar instance, pane, mux).
- `src/schema/` — event envelope, heartbeat, and protocol-version constants.
- `src/feed.rs` — feed item lifecycle, surfaces, statuses, resolution methods.
- `src/ledger/paths.rs` — XDG state/runtime paths and `/tmp/rimz-<uid>` fallback.
- `src/ledger/atomic.rs` — temp+rename and length-framed append helpers.
- `src/ledger/lock.rs` — workspace advisory locking.
- `src/ledger/event_log.rs` — length-framed append log, fsync, torn-trailing-record recovery, size-cap rotation, archive pruning.
- `src/ledger/feed_store.rs` — atomic feed item writes and status CAS.
- `src/ledger/gc.rs` — runtime garbage collection for stale liveness hints.
- `src/ledger/snapshot.rs` — reduced workspace snapshot rebuild, latest snapshot write, agent-rollup carryover across event-log rotation.
- `src/ledger/workspace_record.rs` — `workspace.json` maintenance index for migrate/prune.
- `src/ledger/wakeup.rs` — best-effort per-request and sidebar wakeup datagrams.
- `src/ledger/mod.rs` — `Ledger` handle (`Arc<LedgerInner>`); public methods take the workspace lock and drive `event_log`, `feed_store`, `snapshot` directly. No actor.
- `src/bridge.rs` — per-request sockets, waiter polling fallback, nonce validation.
- `src/mux/mod.rs` — `MuxBackend` trait and shared backend errors.
- `src/sidebar.rs` — sidebar heartbeat freshness check used to avoid duplicate native panes.
- `src/mux/zellij.rs` — Zellij commands, background session creation, native sidebar pane launch, pipe fast path.
- `src/mux/tmux.rs` — tmux commands, native managed sidebar pane, popup/status integrations.
- `src/mux/selection.rs` — backend selection precedence.
- `src/agents/mod.rs` — `AgentIntegration` trait.
- `src/agents/claude.rs` — Claude wrapper, hook installer, classification, rendering.
- `src/agents/codex.rs` — Codex hook install merge, classification, rendering.
- Additional agent adapters (OpenCode, Pi, etc.) land per
  [docs/contributing/roadmap.md](./docs/contributing/roadmap.md) once their hook surfaces
  and decision shapes are verified.
- `src/resolver/mod.rs` — re-exports for the resolver subsystem.
- `src/resolver/allowlist.rs` — per-machine TOML allowlist with atomic writes.
- `src/resolver/freshness.rs` — heartbeat TTL walk, single-resolver health check, TOCTOU `restat`.
- Hook stdout goldens live inline as `insta::assert_*_snapshot!(... @"...")` macros inside each adapter module.

Crate-local rules:

- Command handlers call domain modules; they never own domain logic.
- Domain modules stay free of Zellij, tmux, and agent-specific dependencies.
- Resolution matching uses `workspace_id`, `request_id`, and nonce — never PID alone.
- Raw pane IDs stay inside backend adapters; normalized IDs (`zellij:terminal_3`, `tmux:%3`) travel everywhere else.
- Backend-specific fast paths cannot become correctness requirements.
- Blocking decision hooks are sync. Installing one as async is a hard error.

### `crates/rimz-sidebar`

Shared native terminal sidebar renderer, packaged for both backends.

- `app.rs` — snapshot model, tick loop, wakeup handling, `FetchStatus`/`RenderState` recovery logic for snapshot or heartbeat failure.
- `render/` — four display groups and the agent rollup.

Crate-local rules:

- Read state through `rimz sidebar snapshot`. Never import ledger writer modules from `crates/rimz`.
- Write liveness through `rimz sidebar heartbeat`.
- Default-mode (`native_ui`) items show focus/dismiss, never approve/deny.

### `examples/resolvers`

Reference resolver artifacts for tests and documentation. Not shipped as product.

- `hook_bridge_resolver.py` — heartbeat loop, polls `rimz feed list`, runs a static `tool_name` allowlist policy, calls `rimz feed resolve --method hook-bridge` or `feed abstain`.
- `pane_send_resolver.py` — heartbeat loop, captures the active pane, matches against a bounded regex list, calls `rimz pane send` + `feed resolve --method pane-send`.

Both scripts are stdlib-only Python 3 single files, not built or shipped — the workspace `Cargo.toml` excludes them. They exist to prove the resolver protocol from `docs/internals/resolvers.md` is implementable through the public CLI alone.

Resolvers treat pane text as untrusted data. Match bounded prompt shapes; abstain on unknown shapes.

### Tests

Integration tests live under each crate's `tests/` directory; per-crate cargo `tests/common/` modules carry shared harness code.

- `crates/rimz/tests/ledger_bridge.rs` — synthetic hook tests over the per-request bridge socket and sidebar wakeup walk.
- `crates/rimz/tests/ledger_round_trip.rs` — push/resolve/dismiss/timeout round trips, CAS, snapshot rebuild.
- `crates/rimz/tests/wakeup_pipe.rs` — Zellij pipe wakeup fast path.
- `crates/rimz/tests/zellij_backend.rs` and `tmux_backend.rs` — backend parity, env injection, managed pane (self-skip when the mux binary is absent).
- `crates/rimz/tests/chain_advance.rs` — chain advancement on budget-elapse, heartbeat-stale, and chain-exhausted.
- `crates/rimz/tests/doctor_mode_pill.rs` — agent rollup rendering from `agent.lifecycle` events.
- `crates/rimz/tests/examples_hook_bridge_resolver.rs` and `examples_pane_send_resolver.rs` — end-to-end coverage for the reference Python resolvers (self-skip when `python3` is absent).
- `crates/rimz/tests/fixtures/` — `zellij-trace` shim and stable payload fixtures.
- `xtask/src/main.rs::invariants` — grep-style architectural rules (no `Stdio::inherit` in hook paths, no sidebar imports of ledger-write modules, no `chrono` / `bytes` / `tokio_util`).
