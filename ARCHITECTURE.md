# Architecture

Rimz is a Rust workspace built around one invariant:

```text
workspace root == Rimz workspace == multiplexer session
```

A root is the richest class its directory offers — a git repo (worktrees of the same repo group inside one workspace), a project-marker directory, or any directory at all, the first-class directory workspace a headless box of agents gets with no source control. A pane's workspace is the session it lives in: session birth stamps the identity pin into the mux environment, and participating commands honor it before re-deriving from cwd ([workspace.rs](./crates/rimz/src/workspace.rs)). Zellij and tmux own panes, views, sessions, attach/detach, and scrollback. Rimz owns project identity, the feed, durable state, resolver trust, hook entrypoints, and the sidebar rendering contract.

Product invariant and operating paths live in [DESIGN.md](./DESIGN.md); they are not restated here.

## Runtime architecture

```text
terminal emulator
  mux session (Zellij or tmux)
    sidebar renderer (native pane)
    shells, scripts, agents, CI helpers
                │
                │  per-instance sidebar socket  (wakeup of record)
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

The CLI and hook subprocesses are the only durable-state writers. The sidebar reads ledger state in process, read-only; the elder renderer additionally writes the shared runtime caches through `sidebar::produce`, and `rimz sidebar snapshot` is the same pipeline's one-shot inspection surface. There is no Rimz daemon. The per-instance sidebar socket is the wakeup channel of record; backend-specific fast paths are latency hints layered over it ([multiplexers.md](./docs/internals/multiplexers.md)).

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

```text
.
|-- AGENTS.md              root of the layered contributor contract
|-- ARCHITECTURE.md        this file
|-- DESIGN.md              design commitments and the three operating paths
|-- README.md              product entry point
|-- docs/                  product and engineering docs (map in AGENTS.md);
|                          docs/externals/ holds the local mirrors of upstream
|                          reference docs, pinned to source URLs for refresh
|-- Cargo.toml
|-- Makefile               thin aliases over cargo xtask
|-- crates/
|   |-- rimz/              CLI binary plus runtime/domain library
|   `-- rimz-presence-zellij/  headless Zellij presence plugin (wasm32-wasip1)
|-- examples/resolvers/    reference resolver artifacts (Python, stdlib-only)
|-- supply-chain/          cargo-vet audit state
`-- xtask/                 contributor task runner; entry point for every quality gate
```

Add a crate only when ownership, target type, or dependency profile justifies it. `rimz` is the one host runtime artifact: the CLI, domain library, and native sidebar renderer ship in the same executable. `rimz-presence-zellij` clears the crate bar because it is a wasm32-wasip1 plugin binary owned by the Zellij plugin-host boundary, with `zellij-tile` as a wasm-only dependency no host artifact links. Release builds embed the wasm into `rimz`, which materializes it under the user's data directory before loading it. It runs `rimz sidebar wake`, ships no data, and corrects switched-to tabs whose focus restored to the sidebar; it depends on no rimz crate, and its pure policy unit-tests on the host ([multiplexers.md → Zellij presence channel](./docs/internals/multiplexers.md#zellij-presence-channel)). Every renderer projects the same `rimz sidebar snapshot` JSON view-model; a future renderer (the planned Zellij plugin rail, [roadmap](./docs/contributing/roadmap.md)) joins as its own crate projecting the same snapshot.

## Module ownership

Contracts live in the layered `AGENTS.md` files — the root contract plus a local contract per subtree, loaded automatically when you work there. This map says what lives where; per-file detail lives in each module's `//!` header.

### `crates/rimz` — [local contract](./crates/rimz/AGENTS.md)

| Subtree | Owns | Detail |
| --- | --- | --- |
| `src/cli/` | command parsing and per-subcommand handlers; one `run(...)` per subcommand | [cli.md](./docs/reference/cli.md) |
| `src/ledger/` | durable state: atomic helpers, event log, feed store, snapshot rebuild, wakeups, GC | [contract](./crates/rimz/src/ledger/AGENTS.md) · [ledger.md](./docs/internals/ledger.md) |
| `src/mux/` | the Zellij/tmux seam: `MuxBackend`, bounded subprocess engine, reconcile planner, recovery | [contract](./crates/rimz/src/mux/AGENTS.md) · [multiplexers.md](./docs/internals/multiplexers.md) |
| `src/agents/` | the agent integration layer: adapter trait, registry, per-provider adapters, spend/pricing/account | [contract](./crates/rimz/src/agents/AGENTS.md) · [hooks.md](./docs/internals/hooks.md) |
| `src/resolver/` | per-machine allowlist, heartbeat freshness, TOCTOU restat | [resolvers.md](./docs/internals/resolvers.md) |
| `src/sidebar/` | sidebar data plane: producer election and heartbeats (`mod.rs`), runtime cache formats/TTLs (`cache.rs`), the in-process no-fork consumer read (`consumer.rs`), the shared enrichment fold (`enrich.rs`), and the producer pipeline (`produce/` — panes, metrics, git, spending) | [sidebar.md](./docs/internals/sidebar.md) · [performance.md](./docs/internals/performance.md) |
| `src/sidebar_renderer/` | native terminal sidebar renderer: the pane-resident serve loop and frame composition over the snapshot view-model | [sidebar.md](./docs/internals/sidebar.md) · [interface/sidebar.md](./docs/interface/sidebar.md) |
| `src/schema/` | event envelope, heartbeat shape, protocol-version constants | [ledger.md](./docs/internals/ledger.md) |

Top-level domain modules are one file each, their `//!` headers carrying the detail: `workspace` (project identity), `worktree` (Rimz-owned Git worktrees and cleanup; [worktrees.md](./docs/internals/worktrees.md)), `tab_layout` (agent-tab layout DSL and IR; [worktrees.md](./docs/internals/worktrees.md)), `trust` (executable-surface hash and grant state; [trust.md](./docs/internals/trust.md)), `feed` (item lifecycle, surfaces, statuses), `bridge` (per-request sockets, nonce validation), `ids` (typed identifier newtypes), `resume` (resume-on-rebirth planner), `remote` and `remote_control` (SSH attach, agent remote-control launch), `config` (per-machine settings), `agent_activity` (liveness hints), `proc` (`/proc` reader), `reload` (binary-upgrade convergence).

`build.rs` embeds the checked-in pricing snapshot (`pricing/litellm-pricing.json`) at compile time, network-free; `cargo xtask pricing-refresh` refreshes it ([pricing.md](./docs/internals/pricing.md)). Hidden subcommands are machinery, not humans — the `sidebar` and `statusline` helper APIs and the `codex` helpers (`refresh-context`, `refresh-rate-limits`, and the `app-server serve` broker hosted in the `rimzd` daemon view) — listed in [cli.md](./docs/reference/cli.md#commands-rimz-calls-for-you).

### `crates/rimz-presence-zellij`

The Zellij presence plugin: a headless wasm32-wasip1 binary Zellij loads into every rimz session, poking `rimz sidebar wake` on pane-topology and focus changes so the producer can stretch its pane poll, and redirecting tab switches that would otherwise land on Rimz's sidebar ([multiplexers.md → Zellij presence channel](./docs/internals/multiplexers.md#zellij-presence-channel)). Everything decision-shaped lives in `policy.rs` — a `zellij-tile`-free pure core, host-tested in the ordinary workspace run — and the wasm shell in `main.rs` only projects host events into it and executes the resulting pokes/focus action. Talks to rimz exclusively through the wake argv; depends on no rimz crate.

### `examples/resolvers`

Reference resolver artifacts — stdlib-only Python 3, excluded from the workspace, never shipped. They prove the resolver protocol in [resolvers.md](./docs/internals/resolvers.md) is implementable through the public CLI alone; each script and its discipline is described in [examples/resolvers/README.md](./examples/resolvers/README.md).

### Tests

Integration tests live under each crate's `tests/`; `crates/rimz` collects its suites into a single `tests/integration/` binary with a shared `common/` harness and `tests/fixtures/` trace shims — layout and conventions in the [suite contract](./crates/rimz/tests/integration/AGENTS.md). The required matrix and the grep-style architectural invariants (`cargo xtask invariants`) live in [docs/contributing/testing.md](./docs/contributing/testing.md).
