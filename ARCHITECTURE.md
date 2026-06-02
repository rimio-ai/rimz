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
    sidebar renderer (native pane — default; optional Zellij plugin rail)
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
|   |-- rimz-sidebar/      native terminal sidebar renderer (default, both backends)
|   `-- rimz-sidebar-zellij/  optional Zellij plugin rail (cdylib, wasm32-wasip1)
|-- examples/resolvers/    reference resolver artifacts (Python, stdlib-only)
|-- tests/                 integration tests live under each crate's `tests/`
`-- xtask/                 contributor task runner; entry point for every quality gate
```

Add a crate only when ownership, target type, or dependency profile justifies it. The CLI is one runtime artifact and the native sidebar renderer is another; the optional Zellij plugin rail is a third because its target type differs (`wasm32-wasip1` `cdylib`). All renderers project the same `rimz sidebar snapshot` JSON view-model — there is no shared render crate.

## Implementation ownership

General coding rules live in [AGENTS.md](./AGENTS.md); only crate-local constraints appear below.

### `crates/rimz`

CLI binary, hook entrypoints, and the runtime/domain library. Start here for any non-sidebar behaviour.

- `src/main.rs` — CLI bootstrap, top-level error reporting.
- `src/cli/` — command parsing and per-subcommand handlers (`workspace`, `list`, `event`, `feed`, `gc`, `pane`, `resolver`, `sidebar`, `hooks`, `codex`, `trust`, `doctor`). `codex refresh-context` is the hidden entrypoint the Codex hook spawns to refresh the app-server `AgentContext` sidecar; `codex app-server serve` is the hidden entrypoint `rimz start` runs in the `rimzd` daemon tab to host the per-session app-server broker.
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
- `src/ledger/snapshot.rs` — reduced workspace snapshot rebuild, latest snapshot write, agent-rollup carryover across event-log rotation, and the pure pane-presence fold-in (the `sidebar` CLI supplies the live pane list; the reducer never calls the mux).
- `src/ledger/workspace_record.rs` — `workspace.json` maintenance index for `workspace migrate` and `gc`.
- `src/ledger/wakeup.rs` — best-effort per-request and sidebar wakeup datagrams.
- `src/ledger/mod.rs` — `Ledger` handle (`Arc<LedgerInner>`); public methods take the workspace lock and drive `event_log`, `feed_store`, `snapshot` directly. No actor.
- `src/bridge.rs` — per-request sockets, waiter polling fallback, nonce validation.
- `src/mux/mod.rs` — `MuxBackend` trait and shared backend errors.
- `src/sidebar.rs` — sidebar heartbeat freshness check used to avoid duplicate native panes.
- `src/mux/zellij.rs` — Zellij commands, background session creation, native sidebar pane launch, pipe fast path.
- `src/mux/tmux.rs` — tmux commands, native managed sidebar pane, popup/status integrations.
- `src/mux/selection.rs` — backend selection precedence.
- `src/agents/` — the agent integration layer; conventions in its own [`AGENTS.md`](./crates/rimz/src/agents/AGENTS.md), the decision boundary in [docs/internals/hooks.md](./docs/internals/hooks.md), the context read-path in [docs/internals/transcript.md](./docs/internals/transcript.md), the account/balance mapping in [docs/internals/account.md](./docs/internals/account.md). Shared, provider-agnostic types sit at the top level; each provider's behaviour is a sibling directory.
- `src/agents/mod.rs` — `AgentIntegration` trait, the agent registry, the shared bounded transcript-tail reader, and the shared posture/classification helpers.
- `src/agents/observation.rs` — `AgentLifecycleObservation` (the unified event shape every reducer reads) and the `observe_lifecycle` scaffolding both adapters share (worktree fields, the payload-overrides-transcript context-gauge resolution).
- `src/agents/context.rs` — the agent-agnostic `AgentContext` and its sub-records, the normalized target every rich-context transport produces.
- `src/agents/hook_types.rs` — wire enums shared across adapters (`PermissionMode`, `SessionSource`, …), tolerant of unknown upstream values.
- `src/agents/account.rs` — the out-of-band provider account probe (`claude auth status`, `~/.codex/auth.json`) behind the `AccountProbe` outcome; producer-only, cached as `accounts.json`. The account/balance model and dashboard aggregation are in [docs/internals/account.md](./docs/internals/account.md).
- `src/agents/spending.rs` — today/week/month spend aggregation over agent transcript history (cache types, cross-file Claude dedup); dispatches to the per-provider parsers in `transcript/`.
- `src/agents/transcript/` — read-only, sidebar-safe full-history cost/usage parsers (`claude.rs`, `codex.rs`, `pi.rs`) consumed by `spending.rs`, with shared filesystem helpers in its `mod.rs`. Distinct from the bounded-tail context gauge each adapter reads inline.
- `src/agents/claude/` — the Claude hook adapter: `mod.rs` (`ClaudeIntegration` — classification, rendering, install/uninstall, transcript `message.usage` gauge), `payloads.rs` (typed hook wire structs), `statusline.rs` (statusline payload parser and the wrap/restore of an existing `statusLine`).
- `src/agents/codex/` — the Codex hook adapter: `mod.rs` (`CodexIntegration` — install merge, classification, rendering, rollout-tail gauge and discovery, `refresh_context`), `payloads.rs` (typed hook wire structs), `app_server.rs` (read-only Codex app-server JSON-RPC client for rate limits / model display name / version, behind a transport seam, preferring broker → daemon → cold-spawn), `broker.rs` (the per-session `codex app-server serve` broker holding one warm handshaked server over a unix socket, run as a pane in the `rimzd` daemon tab).
- Additional agent adapters (OpenCode, Pi, etc.) land as new `src/agents/<name>/` directories per [docs/contributing/roadmap.md](./docs/contributing/roadmap.md) once their hook surfaces and decision shapes are verified (the mapping recipe is in [docs/internals/hooks.md](./docs/internals/hooks.md#adding-an-agent)).
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

Native terminal sidebar renderer — the default on both backends.

- `app.rs` — snapshot model, tick loop, wakeup handling, `FetchStatus`/`RenderState` recovery logic for snapshot or heartbeat failure.
- `render/` — projects the snapshot view-model into worktree-grouped, attention-ranked rows (agents, attention items, and bare process rows for non-agent panes).

Crate-local rules:

- Read state through `rimz sidebar snapshot`. Never import ledger writer modules from `crates/rimz`.
- Write liveness in-process through the `rimz::sidebar::write_heartbeat` helper (a runtime-file write, not a ledger-writer import).
- Default-mode (`native_ui`) items show focus/dismiss, never approve/deny.

### `crates/rimz-sidebar-zellij`

Optional Zellij plugin rail (`cdylib`, `wasm32-wasip1`): the same view-model as a docked, persistent left rail. Not on the correctness path — the native pane is the fallback.

Crate-local rules:

- Project the `rimz sidebar snapshot --json` view-model; do not re-derive grouping. There is no shared render code with `crates/rimz-sidebar` — visual parity is a maintained discipline, aligned through the semantic→glyph conventions in [docs/internals/sidebar.md](./docs/internals/sidebar.md).
- Read state inside the wasm sandbox through the snapshot JSON only — no sockets, no ledger-writer imports. The `zellij pipe --name rimz::feed` wakeup plus a keepalive tick trigger refetches.

### `examples/resolvers`

Reference resolver artifacts for tests and documentation. Not shipped as product.

- `hook_bridge_resolver.py` (**auto-approve**) — heartbeat loop, polls `rimz feed list`, runs a `tool_name` allowlist policy, approves matching permission requests via `rimz feed resolve --method hook-bridge` (else `feed abstain`).
- `pane_send_resolver.py` (**rate-limit-resume**) — heartbeat loop, captures the active pane, matches a bounded prompt-shape list, resumes the agent via `rimz pane send` + `feed resolve --method pane-send`.

Both scripts are stdlib-only Python 3 single files, not built or shipped — the workspace `Cargo.toml` excludes them. They exist to prove the resolver protocol from `docs/internals/resolvers.md` is implementable through the public CLI alone.

Resolvers treat pane text as untrusted data. Match bounded prompt shapes; abstain on unknown shapes.

### Tests

Integration tests live under each crate's `tests/` directory; `crates/rimz` collects its suites into a single `tests/integration/` binary whose `common/` module carries the shared harness.

- `crates/rimz/tests/integration/ledger/bridge.rs` — synthetic hook tests over the per-request bridge socket and sidebar wakeup walk.
- `crates/rimz/tests/integration/ledger/round_trip.rs` — push/resolve/dismiss/timeout round trips, CAS, snapshot rebuild.
- `crates/rimz/tests/integration/wakeup_pipe.rs` — Zellij pipe wakeup fast path.
- `crates/rimz/tests/integration/backend/{zellij,tmux}.rs` — backend parity, env injection, managed pane (self-skip when the mux binary is absent).
- `crates/rimz/tests/integration/chain_advance.rs` — chain advancement on budget-elapse, heartbeat-stale, and chain-exhausted.
- `crates/rimz/tests/integration/doctor.rs` — agent rollup rendering from `agent.lifecycle` events.
- `crates/rimz/tests/integration/examples/{hook_bridge,pane_send}.rs` — end-to-end coverage for the reference Python resolvers (self-skip when `python3` is absent).
- `crates/rimz/tests/fixtures/` — `zellij-trace` shim and stable payload fixtures.
- `xtask/src/main.rs::invariants` — grep-style architectural rules (no `Stdio::inherit` in hook paths, no sidebar imports of ledger-write modules, no `chrono` / `bytes` / `tokio_util`).
