# Architecture

RimZ is a single Rust binary that turns a Zellij or tmux session into a control room for coding agents. It owns no database or always-on state service: it builds on the multiplexer you already run, a directory of flat files for durable state, and one CLI that every event flows through. Optional browser access adds a machine-wide authenticated ttyd process for writable transport and a separate unauthenticated, input-blocked process for explicitly shared rooms.

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

There is no general RimZ daemon. Every durable write is a short-lived CLI or hook subprocess; the sidebar is a native pane that reads store state in process.

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

When browser access is enabled, one authenticated ttyd daemon binds the configured loopback port for every Zellij and tmux room. A second no-auth daemon binds its own port only after `rimz web share` allowlists a live room and omits ttyd's write flag. Each connection supplies a session argument to a hidden `rimz web exec` shim; the writable shim validates durable workspace ownership and live mux state, while the broadcast shim also validates the durable per-room allowlist, so ttyd owns transport but no session authority ([web.md](./docs/internals/web.md)).

The spending service is a private thread inside whichever host-eligible long-lived RimZ process wins its persistent/discovery-namespace lifetime lock; one-shot inspection commands connect or fall back directly without becoming the warm owner. Its schema- and namespace-versioned Unix socket accepts clients concurrently while one try-locked walker owns stale work, so a slow request cannot queue another workspace's refresh tick. `spending.json`, provider/workspace publications, and their atomic-write and downgrade guards remain truth. Process exit discards the service and the next eligible client re-elects an owner, so this warm-cache optimization introduces no RimZ daemon.

The CLI and hook subprocesses are the only writers of product truth. The sidebar reads the store read-only and writes its own runtime caches and read receipts; `rimz sidebar snapshot` is the one-shot inspection surface over the same pipeline. The per-instance sidebar socket is the wakeup channel of record — backend-specific fast paths are latency hints layered over it ([multiplexers.md](./docs/internals/multiplexers.md)). The producer/consumer split, push channels, and timing cadences are in [state.md](./docs/internals/sidebar/state.md).

### State ownership

| Owner | Owns | Does not own |
| --- | --- | --- |
| Multiplexer | panes, views, sessions, attach/detach, layout, scrollback | store state, agent status, handler trust |
| RimZ store | events, agent state, messages, runs, snapshots | terminal rendering, pane mechanics |
| CLI / hook subprocesses | durable writes, supervised-run waiters, mux command calls | UI presentation |
| Sidebar | rendering, focus affordances, human actions through the CLI | durable state files |
| ttyd browser daemons | authenticated writable transport; no-auth input-blocked transport | workspace identity, session validation, broadcast allowlist, durable state |
| Agents | native UI, prompts, sandboxing, bypass behaviour | RimZ store state |
| Host | process resurrection, OS sandboxing | workspace state |

### State on disk

State is five tiers of plain files, scoped by what each one outlives. [`store/paths.rs`](./crates/rimz/src/store/paths.rs) (`StatePaths`, `RuntimePaths`) owns the path constants, and [store.md → What is on disk](./docs/internals/store.md#what-is-on-disk) is the file-by-file catalog; this is the map.

```text
workspace store         ~/.local/state/rimz/workspaces/<workspace_id>/
  one room's durable truth: the framed event log and the records beside it,
  plus the producer caches that survive a reboot

per-workspace runtime   $XDG_RUNTIME_DIR/rimz/<workspace_id>/  (or /tmp/rimz-<uid>/…)
  one room's disposable tier: wakeup sockets, heartbeats, read receipts,
  and enrichment sidecars

shared persistent       ~/.local/state/rimz/shared/
  account-global provider state: accounts, rate limits, credits, spend, pricing

shared runtime          $XDG_RUNTIME_DIR/rimz/shared/
  the account-global election locks and the spending service's versioned socket

user-global persistent  ~/.local/state/rimz/
  builds/<build_id>/rimz immutable executable generations, the loop registry,
  the browser daemon pid/port records and credential, and the broadcast room allowlist
```

One rule sorts a new file into a tier: **persistent tiers hold what must survive a reboot, runtime tiers hold what is meaningless without the process that wrote it.** A lock, a socket, or a cache that only speeds the next read is runtime and dies with the session; a durable record, or a cache the dashboard needs to open warm, is persistent. The store tier's durability contract — temp-file-plus-rename, the framed log, and the write classes — is [store.md](./docs/internals/store.md), the provider files are [providers.md](./docs/internals/agents/providers.md), the loop registry is [loops.md](./docs/internals/harness/loops.md), and executable staging is [sidebar.md → Build promotion](./docs/internals/sidebar/sidebar.md#build-promotion).

## Code and crate structure

The path index — repository layout and per-module ownership — is [AGENTS.md § Code map](./AGENTS.md#code-map). This section holds the structural rationale behind it.

Add a crate only when ownership, target type, or dependency profile demands it. `rimz` is the one host runtime artifact — CLI, domain library, and native sidebar renderer ship in the same executable, every renderer projecting the same `rimz sidebar snapshot` view-model. `rimz-presence-zellij` clears the bar because it is a wasm32-wasip1 plugin binary owned by the Zellij plugin-host boundary, depending on no rimz crate; every `rimz` build embeds the vendored wasm checked in under `crates/rimz/presence/` and materializes it under the user's data directory before loading it. Its decision logic is a `zellij-tile`-free pure `policy.rs`, and its host-boundary argv/KDL rendering is pure `wire.rs`; both host-test in the ordinary workspace run. The plugin talks to rimz exclusively through wake/focus argv: it observes pane and client changes, publishes `pane-topology.json` as a Zellij-only cache, and reports settled switch generations. Host code derives session focus, pane roles, repair ownership, and typed events through the common presence projector ([multiplexers.md → Zellij presence channel](./docs/internals/multiplexers.md#the-zellij-presence-plugin)).

The top-level `theme` module is renderer-neutral presentation policy: it resolves schemes, semantic tones, provider identity, resolved glyph vocabulary and setup probes, and shared human value formats. The CLI and native sidebar convert its `Tone` values only at their renderer edges, so the runtime ships one interface language without coupling the core to `anstyle` or ratatui ([theme.md](./docs/internals/theme.md)).

`build.rs` embeds the checked-in data artifacts at compile time, network-free: the generated token-pricing snapshot (`cargo xtask pricing-refresh` regenerates it; [providers.md → Token pricing](./docs/internals/agents/providers.md#token-pricing)), the Alacritty theme catalog (`cargo xtask theme-refresh`), and the presence-plugin wasm. Hidden helper subcommands are machinery, not humans; they are marked `#[command(hide = true)]` in [`cli/mod.rs`](./crates/rimz/src/cli/mod.rs), and their protocols live in the owning internals docs.

## Tests

Integration tests live under each crate's `tests/`; `crates/rimz` collects its suites into a single `tests/integration/` binary with a shared `common/` harness — layout and conventions in the [suite contract](./crates/rimz/tests/integration/AGENTS.md). The test tiers are defined in [rust-conventions.md → Tests](./docs/contributing/rust-conventions.md#tests), and grep-style architectural invariants (decision-channel integrity, sidebar/store separation, the trust hash, pane-primitive use, and more) run through `cargo xtask invariants`.
