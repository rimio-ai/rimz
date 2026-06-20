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
                │  per-instance sidebar socket  (typed wakeup events of record)
                │  hook stdin/stdout            (decision channel)
                ▼
rimz CLI and hook subprocesses
  workspace identity, ledger writes, feed ask/resolve/wait
  mux commands through MuxBackend, hook installers
                │
                ▼
workspace ledger (~/.local/state/rimz/workspaces/<id>/)
  events.log.jsonl   snapshots/latest.json
  feed/<request_id>.json   runs/<run_id>.json   locks/workspace.lock
  diag.log.jsonl   diag-frames/

runtime directory ($XDG_RUNTIME_DIR/rimz/<id>/, or /tmp/rimz-<uid>/rimz/<id>/ when XDG_RUNTIME_DIR is unset)
  sock/feed.<short_id>.sock         per-request decision socket
  sock/run.<short_id>.sock          supervised-run completion socket
  sock/sidebar.<short_instance_id>.sock
                                      wakeup datagram socket
  heartbeat/sidebar.<instance_id>.json
  heartbeat/resolver.<resolver_id>.json
  read-marks/sidebar.<instance_id>.json
  read-marks/manual.json
  unread.json
  snapshot.json  pane-topology.json  presence.stamp    runtime caches
  diff-stats.json  metrics-sample.json  live-spend-baselines.json
  workspace-spending.<scope_hash>.json
  link-stats.json  binding.log.jsonl
  agent_context/  subagent_context/  agent-activity/   per-session sidecars

shared runtime directory ($XDG_RUNTIME_DIR/rimz/shared/)
  accounts.json  accounts.lock
  rate_limits.json  rate_limits.lock
  credits.json  credits.lock
  provider-spending.json  spending.json  spending.lock
  pricing-cache.json  rate-limit-probe.codex*
```

The CLI and hook subprocesses are the only durable-state writers for product truth. The sidebar reads ledger state in process, read-only; every renderer writes its own runtime read-mark receipt file, `rimz sidebar mark-read` writes the durable manual receipt and removes reached `unread.json` episodes, while the elder renderer additionally writes status-derived `unread.json` episode opens/prunes and producer runtime caches through `sidebar::produce`, refreshes its room's `live-spend-baselines.json` display sidecar when the shared spending walk advances, refreshes the disposable context sidecars from its producer-side triggers (the produce backstop and the transcript watcher — [state.md](./docs/internals/sidebar/state.md#push-channels)), and appends diagnostic-only anomaly records through `diag`. `rimz sidebar snapshot` is the same pipeline's one-shot inspection surface. There is no Rimz daemon. The per-instance sidebar socket is the wakeup channel of record; backend-specific fast paths are latency hints layered over it ([multiplexers.md](./docs/internals/sidebar/multiplexers.md)).

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
|-- crates/
|   |-- rimz/              CLI binary plus runtime/domain library
|   `-- rimz-presence-zellij/  headless Zellij presence plugin (wasm32-wasip1)
|-- examples/resolvers/    reference resolver artifacts (Python, stdlib-only)
|-- supply-chain/          cargo-vet audit state
`-- xtask/                 contributor task runner; entry point for every quality gate
```

Add a crate only when ownership, target type, or dependency profile justifies it. `rimz` is the one host runtime artifact: the CLI, domain library, and native sidebar renderer ship in the same executable. `rimz-presence-zellij` clears the crate bar because it is a wasm32-wasip1 plugin binary owned by the Zellij plugin-host boundary, with `zellij-tile` as a wasm-only dependency no host artifact links. Release builds embed the wasm into `rimz`, which materializes it under the user's data directory before loading it. It runs `rimz sidebar wake`, ships a compact pane-topology latency hint through the wake argv for `rimz` to write, and flags switched-to tabs whose focus restored to the sidebar so that tab's sidebar can refocus its working pane; it depends on no rimz crate, and its pure policy unit-tests on the host ([multiplexers.md → Zellij presence channel](./docs/internals/sidebar/multiplexers.md#zellij-presence-channel)). Every renderer projects the same `rimz sidebar snapshot` JSON view-model.

## Module ownership

Contracts live in the layered `AGENTS.md` files — the root contract plus a local contract per subtree, loaded automatically when you work there. This map says what lives where; per-file detail lives in each module's `//!` header.

### `crates/rimz` — [local contract](./crates/rimz/AGENTS.md)

| Subtree | Owns | Detail |
| --- | --- | --- |
| `src/cli/` | command parsing and per-subcommand handlers; one `run(...)` per subcommand, with oversized command internals split under matching leaves such as `cli/agents_cmd/` and `cli/remote/`; `cli/transcript.rs` owns the conversation-inspection command; human-facing output is styled and aligned through the shared `cli/render/` presentation layer | [cli.md](./docs/reference/cli.md) |
| `src/ledger/` | durable state: atomic helpers, framed event log, feed store, split write path, snapshot rebuild and staged view projection (`snapshot/view/`), wakeups, GC | [contract](./crates/rimz/src/ledger/AGENTS.md) · [ledger.md](./docs/internals/sidebar/ledger.md) |
| `src/mux/` | the Zellij/tmux seam: `MuxBackend`, bounded subprocess engine, reconcile planner, recovery | [contract](./crates/rimz/src/mux/AGENTS.md) · [multiplexers.md](./docs/internals/sidebar/multiplexers.md) |
| `src/agents/` | the agent integration layer: adapter trait, registry, per-provider adapters, provider-agnostic transcript grouping/fusion in `agents/transcript.rs`, spend/pricing/account | [contract](./crates/rimz/src/agents/AGENTS.md) · [agent.md](./docs/internals/agents/agent.md) · [provider.md](./docs/internals/agents/provider.md) |
| `src/resolver/` | per-machine allowlist, heartbeat freshness, TOCTOU restat | [resolvers.md](./docs/internals/agents/resolvers.md) |
| `src/remote/` | pure SSH remote target grammar, guarded ssh command builder, reconnect policy, link-health protocol, and the `remote.toml` alias store | [cli.md](./docs/reference/cli.md) · [remote.md](./docs/internals/reach/remote.md) |
| `src/diag.rs`, `src/rotating_log.rs`, `src/binding_log.rs`, `src/notify_log.rs` | diagnostic-only JSONL append surfaces: typed sidebar anomaly records, shared rotating append helper, pane-binding recovery logs, and the notification trace (emitted/bell/unread timeline, written through `DiagSink`) | [diagnostics.md](./docs/internals/health/diagnostics.md) · [notifications.md](./docs/internals/sidebar/notifications.md) |
| `src/sidebar/` | sidebar data plane: producer election and heartbeats (`mod.rs`), durable unread episodes (`unread.rs`), runtime read-mark receipts (`read_marks.rs`), timing constants and cadence registry (`timing.rs`), renderer event store (`events.rs`), typed pane topology (`frame.rs`), pure pulled-truth/event fusion (`fuse.rs`), notification policy over unread episode opens (`notify.rs`), runtime cache formats and reads (`cache.rs`), the in-process no-fork consumer read (`consumer.rs`), the shared enrichment fold (`enrich.rs` plus `enrich/` account, Codex, OAuth usage refresh, rate-limit, credits, live-spend, and rate-limit-park auto-continue leaves), the producer pipeline (`produce/` — panes/process-starts, metrics/Zellij pid backfill, git/root enumeration, spending), and the rendered-stream anomaly observer (`observe.rs` plus `observe/` signature, detector, and writer leaves, emitting through `diag`) | [state.md](./docs/internals/sidebar/state.md) · [sidebar.md](./docs/internals/sidebar/sidebar.md) · [notifications.md](./docs/internals/sidebar/notifications.md) · [observe.md](./docs/internals/health/observe.md) · [performance.md](./docs/internals/health/performance.md) |
| `src/sidebar_pane/` | native pane-resident sidebar process: the fixed-timestep serve loop and producer election (`app/`, split into loop state, socket/heartbeat, timing, notification, fetch/state/gate/health/lifecycle/reload/selection/watch leaves), renderer-local pets (`pets/` asset fetch/cache, WebP decode, frame slicing, cell-art conversion, fleet-status tracks, and canned captions), and frame composition over the snapshot view-model (`render/`, split into UI state, `animation.rs` status-head resolution, the `theme/` four-layer color pipeline (raw palette → semantic slots → component tokens → depth-quantized emit, with identity tones beside it) fed by `scheme.rs` Alacritty-theme parsing and `oklab.rs` perceptual blending, then compose, chrome, ANSI serialization, effects, labels, and section leaves including `agent_card/` and `pets/`). The `theme/` module owns the one place hue is decided; the `ensure_no_hardcoded_ui_colors` invariant keeps the rest of `render/` naming color through component tokens and semantic accessors | [sidebar.md](./docs/internals/sidebar/sidebar.md) · [notifications.md](./docs/internals/sidebar/notifications.md) · [interface/sidebar.md](./docs/interface/sidebar.md) · [pets.md](./docs/internals/sidebar/pets.md) |
| `src/schema/` | durable event envelope, typed sidebar event envelope, diagnostic envelope, heartbeat shape, protocol-version constants | [ledger.md](./docs/internals/sidebar/ledger.md) · [state.md](./docs/internals/sidebar/state.md) · [diagnostics.md](./docs/internals/health/diagnostics.md) |

Top-level domain modules keep their `//!` headers as the detail entry point: `workspace` (project identity), `worktree` (Rimz-owned Git worktrees and cleanup, with `worktree_include` seeding new worktrees from the project's `.worktreeinclude` and `worktree_link` symlinking shared directories from `.worktreelink`; [worktree.md](./docs/internals/agents/worktree.md)), `forge` (pure pull-request number, URL, remote-host, and forge-ref parsing for Git-backed PR worktrees), `agents_spec` (team/profile resolution plus the inline agent layout DSL and IR; [harness.md](./docs/internals/agents/harness.md#the-layout-ir)), `launch` (default-shell resolution, shell-startup argv construction, and final PATH probing), `trust` (executable-surface hash and grant state; [trust.md](./docs/internals/sidebar/trust.md)), `feed` (item lifecycle, surfaces, statuses), `target` (the agent-address grammar — `@<kind|role|profile|petname|kind-n>#<channel>` parsing, snapshot resolution by arity, the role/profile-first canonical handle renderer that is the inverse of the parser and shared by every agent-bearing listing, and the create-on-miss / broadcast helpers; [harness.md](./docs/internals/agents/harness.md#the-address)), `message` (deferred per-agent text queue; [harness.md](./docs/internals/agents/harness.md#talk-and-queue)), `run` (supervised one-shot run records and lifecycle completion; [harness.md](./docs/internals/agents/harness.md#supervised-runs)), `schedule` (scheduled loop task parsing and systemd/cron artifact rendering; [loop.md](./docs/internals/agents/loop.md)), `bridge` (per-request and per-run sockets, nonce validation), `sock` (shared AF_UNIX socket budget and remedy), `ids` (typed identifier newtypes), `resume` (resume-on-rebirth planner), `remote_control` (agent remote-control launch), `osc` (terminal notification OSC/BEL bytes), `observability` (off-box error reporting wiring: Sentry init and the `tracing` bridge layer; [observability.md](./docs/internals/health/observability.md)), `config` (per-machine settings mirrored from `config.toml`, `theme.toml`, and `agents.toml`, with config-family leaves under `src/config/`, including `theme.rs` for appearance, `color.rs` for typed theme colors, and `animation.rs` for status-head overrides), `agent_activity` (liveness hints), `proc` (`/proc` reader), `reload` (binary-upgrade convergence and re-exec target resolution).

`build.rs` embeds the ignored generated pricing snapshot (`pricing/litellm-pricing.json`) at compile time as compressed bytes, network-free; `cargo xtask pricing-refresh` regenerates it, and `cargo xtask dist` runs that refresh before cross-building both macOS release archives and ad-hoc signing the Apple Silicon binary ([provider.md → Token pricing](./docs/internals/agents/provider.md#token-pricing)). Hidden subcommands are machinery, not humans — the `sidebar` and `statusline` helper APIs, the `claude refresh-usage` helper, and the `codex` helpers (`refresh-context`, `refresh-rate-limits`, and the `app-server serve` broker hosted in the `rimzd` daemon view) — listed in [cli.md](./docs/reference/cli.md#commands-rimz-calls-for-you).

### `crates/rimz-presence-zellij`

The Zellij presence plugin: a headless wasm32-wasip1 binary Zellij loads into every rimz session, poking `rimz sidebar wake` on pane-topology and focus changes so the producer can stretch its pane poll, publishing `pane-topology.json` as a Zellij-only cache, and flagging tab switches that restore focus to Rimz's sidebar ([multiplexers.md → Zellij presence channel](./docs/internals/sidebar/multiplexers.md#zellij-presence-channel)). Everything decision-shaped lives in `policy.rs` — a `zellij-tile`-free pure core, host-tested in the ordinary workspace run — and the wasm shell in `main.rs` only projects host events into it and executes the resulting wake pokes. Talks to rimz exclusively through the wake argv; depends on no rimz crate.

### `examples/resolvers`

Reference resolver artifacts — stdlib-only Python 3, excluded from the workspace, never shipped. They prove the resolver protocol in [resolvers.md](./docs/internals/agents/resolvers.md) is implementable through the public CLI alone; each script and its discipline is described in [examples/resolvers/README.md](./examples/resolvers/README.md).

### Tests

Integration tests live under each crate's `tests/`; `crates/rimz` collects its suites into a single `tests/integration/` binary with a shared `common/` harness and `tests/fixtures/` trace shims — layout and conventions in the [suite contract](./crates/rimz/tests/integration/AGENTS.md). The test tiers live in [rust-conventions.md](./docs/contributing/rust-conventions.md#tests), and grep-style architectural invariants run through `cargo xtask invariants`.
