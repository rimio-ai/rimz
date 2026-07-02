# Architecture

Rimz is a single Rust binary that turns a Zellij or tmux session into a control room for coding agents. It owns no daemon and no database: it builds on the multiplexer you already run, a directory of flat files for durable state, and one CLI that every event flows through.

One invariant ties the runtime together:

```text
workspace root == Rimz workspace == multiplexer session
```

A **root** is the richest class a directory offers: an enclosing git repository (whose worktrees group inside one room), else a project-marker directory, else the directory itself — the workspace a headless box of agents gets with no source control. A pane's workspace is the session it lives in: session birth stamps the identity pin into the mux environment, and participating commands honor it before re-deriving from cwd ([workspace.rs](./crates/rimz/src/workspace.rs)). Zellij and tmux own panes, views, sessions, attach/detach, and scrollback; Rimz owns project identity, the feed, durable state, resolver trust, hook entrypoints, and the sidebar rendering contract.

Product invariants and the operating paths live in [DESIGN.md](./DESIGN.md); this file is the structural map.

## How to read this map

Detail lives in three places, narrowing as you go:

- **This file** says *what lives where* — the runtime shape, on-disk state, and module ownership.
- **The layered `AGENTS.md` contracts** state the *rules* for a subtree, loaded automatically when you work there: the root contract, [`crates/rimz/`](./crates/rimz/AGENTS.md), and a contract per major subtree ([`agents/`](./crates/rimz/src/agents/AGENTS.md), [`harness/`](./crates/rimz/src/harness/AGENTS.md), [`ledger/`](./crates/rimz/src/ledger/AGENTS.md), [`mux/`](./crates/rimz/src/mux/AGENTS.md), [`tests/integration/`](./crates/rimz/tests/integration/AGENTS.md)).
- **Each module's `//!` header** is the per-file authority. This map never restates it; the topic docs under `docs/` (mapped in [AGENTS.md](./AGENTS.md#documentation-map)) explain behavior.

## Runtime shape

There is no Rimz daemon. Every durable write is a short-lived CLI or hook subprocess; the sidebar is a native pane that reads ledger state in process.

```text
terminal emulator
  mux session (Zellij or tmux)
    sidebar renderer (native pane, read-only on the ledger)
    shells, scripts, agents, CI helpers
                │
                │  per-instance sidebar socket   (typed wakeup events of record)
                │  hook stdin/stdout             (the decision channel)
                ▼
rimz CLI and hook subprocesses
  workspace identity · ledger writes · feed ask/resolve/wait
  mux commands through MuxBackend · hook installers
                │
                ▼
workspace ledger (a directory of flat files)
```

The CLI and hook subprocesses are the only writers of product truth. The sidebar reads the ledger read-only and writes its own runtime caches and read receipts; `rimz sidebar snapshot` is the one-shot inspection surface over the same pipeline. The per-instance sidebar socket is the wakeup channel of record — backend-specific fast paths are latency hints layered over it ([multiplexers.md](./docs/internals/sidebar/multiplexers.md)). The producer/consumer split, push channels, and timing cadences are in [state.md](./docs/internals/sidebar/state.md).

### State ownership

| Owner | Owns | Does not own |
| --- | --- | --- |
| Multiplexer | panes, views, sessions, attach/detach, layout, scrollback | feed state, decisions, resolver trust |
| Rimz ledger | events, feed items, resolutions, snapshots | terminal rendering, pane mechanics |
| CLI / hook subprocesses | durable writes, bridge waiters, mux command calls | UI presentation |
| Sidebar | rendering, focus affordances, human actions through the CLI | durable state files |
| Resolvers | optional decisions through the public CLI | core policy, automatic trust |
| Agents | native UI, prompts, sandboxing, bypass behaviour | Rimz feed state |
| Host | process resurrection, OS sandboxing | workspace state |

### Durable state on disk

State is three tiers of plain files. The path constants and their exact filenames are owned by [`ledger/paths.rs`](./crates/rimz/src/ledger/paths.rs) (`StatePaths`, `RuntimePaths`); this is the shape, not the catalog.

```text
workspace ledger   ~/.local/state/rimz/workspaces/<id>/
  events.log.jsonl · snapshots/latest.json · feed/<request_id>.json
  runs/<run_id>.json · messages/messages.jsonl · transcript/<date>.jsonl · locks/workspace.lock
  workspace.json · channels.json
  diag.log.jsonl · diag-frames/                      durable truth

per-workspace runtime   $XDG_RUNTIME_DIR/rimz/<id>/   (or /tmp/rimz-<uid>/… )
  sock/*.sock          per-request, per-run, and sidebar wakeup sockets
  heartbeat/ · read-marks/ · unread.json             liveness and attention
  *.json caches · agent_context/ · agent-activity/   disposable enrichment

shared persistent   ~/.local/state/rimz/shared/
  accounts.json · rate_limits.json · credits.json
  provider-spending.json · spending.json · pricing-cache.json

user-global persistent   ~/.local/state/rimz/
  loop-instances.json · loop-runs.log.jsonl

shared runtime      $XDG_RUNTIME_DIR/rimz/shared/
  accounts.lock · rate_limits.lock · credits.lock · spending.lock
```

The ledger tier is durable truth, written with temp-file-plus-rename and a framed event log (the durability contract is [ledger.md](./docs/internals/sidebar/ledger.md)). Shared persistent caches survive reboot so the dashboard and pace views open warm, while runtime tiers are disposable: locks, sockets, sidecars, and per-room best-effort caches that speed the next read and die with the session.

## Repository layout

```text
.
|-- AGENTS.md              root of the layered contributor contract (== CLAUDE.md)
|-- ARCHITECTURE.md        this file — runtime shape and module ownership
|-- DESIGN.md              design commitments and the three operating paths
|-- README.md              product entry point
|-- ci/                    Dockerfile for the CI runner image (pinned Zellij and tmux)
|-- docs/                  product and engineering docs (map in AGENTS.md);
|                          docs/externals/ mirrors upstream references, pinned to source URLs
|-- Cargo.toml
|-- crates/
|   |-- rimz/              the CLI binary plus the runtime/domain library;
|   |                      benches/ holds non-gating divan performance benches, and
|   |                      presence/, pricing/, themes/ hold checked-in data build.rs embeds
|   `-- rimz-presence-zellij/   headless Zellij presence plugin (wasm32-wasip1)
|-- examples/resolvers/    reference resolver artifacts (Python, stdlib-only)
|-- scripts/               repo maintenance helpers (sync-repo)
|-- supply-chain/          cargo-vet audit state
`-- xtask/                 contributor task runner; entry point for every quality gate
```

Add a crate only when ownership, target type, or dependency profile demands it. `rimz` is the one host runtime artifact — CLI, domain library, and native sidebar renderer ship in the same executable, every renderer projecting the same `rimz sidebar snapshot` view-model. `rimz-presence-zellij` clears the bar because it is a wasm32-wasip1 plugin binary owned by the Zellij plugin-host boundary, depending on no rimz crate; every `rimz` build embeds the vendored wasm checked in under `crates/rimz/presence/` and materializes it under the user's data directory before loading it. Its decision logic is a `zellij-tile`-free pure `policy.rs`, host-tested in the ordinary workspace run ([multiplexers.md → Zellij presence channel](./docs/internals/sidebar/multiplexers.md#zellij-presence-channel)). `examples/resolvers/` are stdlib-only Python 3 artifacts, excluded from the workspace and never shipped; they prove the [resolver protocol](./docs/internals/agents/resolvers.md) is implementable through the public CLI alone ([examples/resolvers/README.md](./examples/resolvers/README.md)).

## Module map — `crates/rimz/src`

The major subsystems live in subtree modules, indexed in the table with their contracts and detail docs; the remaining top-level domain modules are grouped below it. Per-file detail lives in the `//!` headers.

| Subtree | Owns | Detail |
| --- | --- | --- |
| `cli/` | command parsing and one `run(...)` per subcommand; oversized commands split under matching leaves (`cli/agents_cmd/`, `cli/loop_cmd/`, `cli/doctor/`, `cli/stats/`, …); human-facing output flows through the shared `cli/render/` presentation layer | [cli.md](./docs/reference/cli.md) |
| `agents/` | the agent integration layer: the `AgentAdapter` trait, the `state.rs` agent rollup model, registry, the per-kind adapters (Claude, Codex, Pi, OpenCode), provider-agnostic transcript fusion, and spend/pricing/account | [contract](./crates/rimz/src/agents/AGENTS.md) · [agent.md](./docs/internals/agents/agent.md) · [provider.md](./docs/internals/agents/provider.md) |
| `harness/` | the agent harness: layout IR and teams, the address grammar, launch argv, petnames, supervised runs, loop scheduling and runner domain, auto-continue policy, and resume-on-rebirth planning | [contract](./crates/rimz/src/harness/AGENTS.md) · [harness.md](./docs/internals/agents/harness.md) |
| `message/` | the durable per-agent message queue: the record and lifecycle domain model, park-vs-live dispatch, the live-pane send engine, queued-delivery sweeps, and elder-fired scheduled wakeups | [message.md](./docs/internals/agents/message.md) |
| `ledger/` | durable state: atomic helpers, framed event log, feed store, message and run stores, `transcript_log.rs`, snapshot rebuild and staged view projection, wakeups, GC | [contract](./crates/rimz/src/ledger/AGENTS.md) · [ledger.md](./docs/internals/sidebar/ledger.md) |
| `mux/` | the Zellij/tmux seam: `MuxBackend`, the bounded subprocess engine, the reconcile planner, recovery | [contract](./crates/rimz/src/mux/AGENTS.md) · [multiplexers.md](./docs/internals/sidebar/multiplexers.md) |
| `sidebar/` | the sidebar data plane: producer election and heartbeats, unread episodes, read-mark receipts, the best-effort notification policy, pulled-truth/event fusion, the projection fold, the heavy-lane refresh directory (accounts, spending, usage, PR state, Codex sidecars, git stats), the producer pipeline, producer tick meter, and the rendered-stream anomaly observer | [state.md](./docs/internals/sidebar/state.md) · [sidebar.md](./docs/internals/sidebar/sidebar.md) · [notifications.md](./docs/internals/sidebar/notifications.md) · [observe.md](./docs/internals/health/observe.md) |
| `sidebar_pane/` | the native pane-resident sidebar process: the fixed-timestep serve loop, elder cache refresher and loop-task firing, renderer-local pets, and frame composition over the snapshot view-model, with the three-layer `render/theme/` color pipeline as the one place hue is decided | [sidebar.md](./docs/internals/sidebar/sidebar.md) · [interface/sidebar.md](./docs/interface/sidebar.md) · [pets.md](./docs/internals/sidebar/pets.md) |
| `schema/` | the durable event envelope, typed sidebar-event and diagnostic envelopes, the notification-trace record, the Zellij pane-topology cache, heartbeat shape, and protocol-version constants | [ledger.md](./docs/internals/sidebar/ledger.md) · [state.md](./docs/internals/sidebar/state.md) |
| `resolver/` | the per-machine allowlist, heartbeat freshness, and TOCTOU restat | [resolvers.md](./docs/internals/agents/resolvers.md) |
| `remote/` | pure SSH target grammar, the guarded ssh command builder, reconnect policy, the link-health protocol, and the `remote.toml` alias store | [remote.md](./docs/internals/reach/remote.md) |
| `diag/` | diagnostic-only JSONL append surfaces (sidebar anomalies, the shared rotating helper, pane-binding recovery, the notification trace, and Zellij presence-plugin telemetry) | [diagnostics.md](./docs/internals/health/diagnostics.md) · [notifications.md](./docs/internals/sidebar/notifications.md) |

### Top-level domain modules

Each keeps its `//!` header as the entry point; grouped here by what they serve.

- **Project identity** — `workspace` (root/worktree resolution and env pinning), `channel` (durable named lanes; [message.md § Channels](./docs/internals/agents/message.md#channels)), `worktree` (Rimz-owned Git worktrees with `.worktreeinclude` seeding and `.worktreelink` linking; [worktree.md](./docs/internals/agents/worktree.md)), `storage` (symlink-safe Rimz-owned disk measurement), `forge` (pure PR-number/URL/remote-host parsing).
- **Feed, panes, and the decision seam** — `feed` (item lifecycle, surfaces, statuses), `pane` (shared pane references and runtime owner metadata), `bridge` (per-request and per-run sockets, nonce validation), `sock` (the shared AF_UNIX budget and remedy), `ids` (typed identifier newtypes), `trust` (the executable-surface hash and grant state; [trust.md](./docs/internals/sidebar/trust.md)).
- **Daemon view and managed hosts** — `remote_control` (Claude/Codex remote-control host auto-launch), `daemon_content` (daemon-view middle-column content resolution and the live-reload supervisor).
- **Process and config infrastructure** — `config` (per-machine settings mirrored from `config.toml`/`theme.toml`/`agents.toml`/`loop.toml`, with the config-family leaves under `src/config/`), `observability` (off-box error reporting; [observability.md](./docs/internals/health/observability.md)), `agent_activity` (liveness hints), `lane` (thread-local producer lane tags and the per-lane counters the tick meter reads), `proc` (the `/proc` reader plus the spawn-count testkit seam), `reload` (binary-upgrade convergence and re-exec), `osc` (terminal notification bytes), `build_id`/`child_process`/`tui` (executable identity, detached-child helpers, shared TUI mode lifecycle), `testkit` (feature-gated synthetic-fleet builders for tests and benches).

`build.rs` embeds the checked-in data artifacts at compile time, network-free: the generated token-pricing snapshot (`cargo xtask pricing-refresh` regenerates it; [provider.md → Token pricing](./docs/internals/agents/provider.md#token-pricing)), the Alacritty theme catalog (`cargo xtask theme-refresh`), and the presence-plugin wasm. Hidden helper subcommands are machinery, not humans; the catalog lives in [cli.md → Commands Rimz calls for you](./docs/reference/cli.md#commands-rimz-calls-for-you).

### `crates/rimz-presence-zellij`

A headless wasm32-wasip1 binary Zellij loads into every rimz session. It pokes `rimz sidebar wake` on pane-topology and focus changes so the producer can stretch its pane poll, publishes `pane-topology.json` as a Zellij-only cache, and flags tab switches that restore focus to Rimz's sidebar. Everything decision-shaped lives in the `zellij-tile`-free `policy.rs` (host-tested); the `main.rs` wasm shell only projects host events into it and executes the resulting wake pokes. It talks to rimz exclusively through the wake argv and depends on no rimz crate.

## Tests

Integration tests live under each crate's `tests/`; `crates/rimz` collects its suites into a single `tests/integration/` binary with a shared `common/` harness — layout and conventions in the [suite contract](./crates/rimz/tests/integration/AGENTS.md). The test tiers are defined in [rust-conventions.md → Tests](./docs/contributing/rust-conventions.md#tests), and grep-style architectural invariants (decision-channel integrity, sidebar/ledger separation, the trust hash, pane-primitive use, and more) run through `cargo xtask invariants`.
