# Architecture

RimZ is a single Rust binary that turns a Zellij or tmux session into a control room for coding agents. It owns no daemon and no database: it builds on the multiplexer you already run, a directory of flat files for durable state, and one CLI that every event flows through.

One invariant ties the runtime together:

```text
workspace root == RimZ workspace == multiplexer session
```

A **root** is the richest class a directory offers: an enclosing git repository (whose worktrees group inside one room), else a project-marker directory, else the directory itself — the workspace a headless box of agents gets with no source control. A pane's workspace is the session it lives in: session birth stamps the identity pin into the mux environment, and participating commands honor it before re-deriving from cwd ([workspace.rs](./crates/rimz/src/workspace.rs)). Zellij and tmux own panes, views, sessions, attach/detach, and scrollback; RimZ owns project identity, durable state, notification handlers, hook entrypoints, and the sidebar rendering contract.

The design pillars and product invariants live in [DESIGN.md](./DESIGN.md); this file is the structural map.

## How to read this map

Detail lives in four places, narrowing as you go:

- **[AGENTS.md § Code map](./AGENTS.md#code-map)** indexes *what lives where* — repository layout and per-module ownership.
- **This file** explains *how it runs* — runtime shape, on-disk state, and the structural rationale behind that code map.
- **The layered `AGENTS.md` contracts** state the *rules* for a subtree, loaded automatically when you work there: the root contract, [`crates/rimz/`](./crates/rimz/AGENTS.md), and a contract per major subtree ([`agents/`](./crates/rimz/src/agents/AGENTS.md), [`harness/`](./crates/rimz/src/harness/AGENTS.md), [`store/`](./crates/rimz/src/store/AGENTS.md), [`mux/`](./crates/rimz/src/mux/AGENTS.md), [`tests/integration/`](./crates/rimz/tests/integration/AGENTS.md)).
- **Each module's `//!` header** is the per-file authority. This map never restates it; the topic docs under `docs/` (mapped in [AGENTS.md](./AGENTS.md#documentation-map)) explain behavior.

## Runtime shape

There is no RimZ daemon. Every durable write is a short-lived CLI or hook subprocess; the sidebar is a native pane that reads store state in process.

```text
terminal emulator
  mux session (Zellij or tmux)
    sidebar renderer (native pane, read-only on the store)
      elected user-scoped spending service thread (disposable warm cache)
    shells, scripts, agents, CI helpers
                │
                │  per-instance sidebar socket   (typed wakeup events of record)
                │  hook stdin/stdout             (the decision channel)
                ▼
rimz CLI and hook subprocesses
  workspace identity · store writes · supervised-run waits
  mux commands through MuxBackend · hook installers
                │
                ▼
workspace store (a directory of flat files)
```

The spending service is a private thread inside whichever existing long-lived RimZ process wins its user-scoped lifetime lock. Its versioned Unix socket coordinates clients, while `spending.json`, provider/workspace publications, and their atomic-write and downgrade guards remain truth. Process exit discards the service and the next client re-elects an owner, so this warm-cache optimization introduces no RimZ daemon.

The CLI and hook subprocesses are the only writers of product truth. The sidebar reads the store read-only and writes its own runtime caches and read receipts; `rimz sidebar snapshot` is the one-shot inspection surface over the same pipeline. The per-instance sidebar socket is the wakeup channel of record — backend-specific fast paths are latency hints layered over it ([multiplexers.md](./docs/internals/multiplexers.md)). The producer/consumer split, push channels, and timing cadences are in [state.md](./docs/internals/sidebar/state.md).

### State ownership

| Owner | Owns | Does not own |
| --- | --- | --- |
| Multiplexer | panes, views, sessions, attach/detach, layout, scrollback | store state, agent status, handler trust |
| RimZ store | events, agent state, messages, runs, snapshots | terminal rendering, pane mechanics |
| CLI / hook subprocesses | durable writes, supervised-run waiters, mux command calls | UI presentation |
| Sidebar | rendering, focus affordances, human actions through the CLI | durable state files |
| Agents | native UI, prompts, sandboxing, bypass behaviour | RimZ store state |
| Host | process resurrection, OS sandboxing | workspace state |

### Durable state on disk

State is three tiers of plain files. The path constants and their exact filenames are owned by [`store/paths.rs`](./crates/rimz/src/store/paths.rs) (`StatePaths`, `RuntimePaths`); this is the shape, not the catalog.

```text
workspace store   ~/.local/state/rimz/workspaces/<id>/
  events.log.jsonl · snapshots/latest.json
  runs/<run_id>.json · messages/messages.jsonl · transcript/<date>.jsonl · locks/workspace.lock
  workspace.json · channels.json · live-roster.json
  diag.log.jsonl · diag-frames/                      durable truth

per-workspace runtime   $XDG_RUNTIME_DIR/rimz/<id>/   (or /tmp/rimz-<uid>/… )
  sock/*.sock          per-run and sidebar wakeup sockets
  heartbeat/ · read-marks/ · unread.json             liveness and attention
  snapshot.json · local-sessions.json · *.json caches
  agent_context/ · agent-activity/                   disposable enrichment
  agent-telemetry/                                  private provider export cache

shared persistent   ~/.local/state/rimz/shared/
  accounts.json · rate_limits.json · credits.json
  provider-spending.json · spending.json · pricing-cache.json

user-global persistent   ~/.local/state/rimz/
  loop-instances.json · loop-runs.log.jsonl

shared runtime      $XDG_RUNTIME_DIR/rimz/shared/
  accounts.lock · rate_limits.lock · credits.lock · spending.lock
  spending-service.v<wire>.c<cache>.sock · matching owner lock
```

The store tier is durable truth plus reboot-surviving producer caches, written with temp-file-plus-rename and a framed event log (the durability contract is [store.md](./docs/internals/store.md)). `live-roster.json` is the sidebar producer's last live root-agent set; rebirth recovery intersects it with the audit rollup before a new session starts. Shared persistent caches survive reboot so the dashboard and pace views open warm, while runtime tiers are disposable: locks, sockets, sidecars, and per-room best-effort caches that speed the next read and die with the session.

## Code and crate structure

The path index — repository layout and per-module ownership — is [AGENTS.md § Code map](./AGENTS.md#code-map). This section holds the structural rationale behind it.

Add a crate only when ownership, target type, or dependency profile demands it. `rimz` is the one host runtime artifact — CLI, domain library, and native sidebar renderer ship in the same executable, every renderer projecting the same `rimz sidebar snapshot` view-model. `rimz-presence-zellij` clears the bar because it is a wasm32-wasip1 plugin binary owned by the Zellij plugin-host boundary, depending on no rimz crate; every `rimz` build embeds the vendored wasm checked in under `crates/rimz/presence/` and materializes it under the user's data directory before loading it. Its decision logic is a `zellij-tile`-free pure `policy.rs`, and its host-boundary argv/KDL rendering is pure `wire.rs`; both host-test in the ordinary workspace run. The plugin talks to rimz exclusively through the wake/focus argv: it pokes `rimz sidebar wake` on pane-topology and focus changes so the producer can stretch its pane poll, publishes `pane-topology.json` as a Zellij-only cache, and flags tab switches that restore focus to the sidebar ([multiplexers.md → Zellij presence channel](./docs/internals/multiplexers.md#zellij-presence-channel)).

`build.rs` embeds the checked-in data artifacts at compile time, network-free: the generated token-pricing snapshot (`cargo xtask pricing-refresh` regenerates it; [provider.md → Token pricing](./docs/internals/agents/providers.md#token-pricing)), the Alacritty theme catalog (`cargo xtask theme-refresh`), and the presence-plugin wasm. Hidden helper subcommands are machinery, not humans; they are marked `#[command(hide = true)]` in [`cli/mod.rs`](./crates/rimz/src/cli/mod.rs), and their protocols live in the owning internals docs.

## Tests

Integration tests live under each crate's `tests/`; `crates/rimz` collects its suites into a single `tests/integration/` binary with a shared `common/` harness — layout and conventions in the [suite contract](./crates/rimz/tests/integration/AGENTS.md). The test tiers are defined in [rust-conventions.md → Tests](./docs/contributing/rust-conventions.md#tests), and grep-style architectural invariants (decision-channel integrity, sidebar/store separation, the trust hash, pane-primitive use, and more) run through `cargo xtask invariants`.
