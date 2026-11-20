# Architecture

RimZ is a single Rust binary that turns a Zellij or tmux session into a control room for coding agents. Room truth needs no database and no always-on state service: RimZ builds on the multiplexer you already run, a directory of flat files for durable state, and one CLI that every event flows through. Optional browser access adds a machine-wide authenticated ttyd process for writable transport and a separate unauthenticated, input-blocked process for explicitly shared rooms.

One invariant ties the runtime together:

```text
workspace root == RimZ workspace == multiplexer session
```

A directory's **root** is the first of these that exists: an enclosing git repository (whose worktrees group inside one room), a project-marker directory, or the directory itself. The last is the workspace a headless box of agents gets with no source control.

A pane's workspace is the session it lives in ([DESIGN.md](./DESIGN.md#invariants)); [workspace.rs](./crates/rimz/src/workspace.rs) owns the identity pin.

## How to read this map

[DESIGN.md](./DESIGN.md) holds the design pillars and product invariants. Structural detail lives in four places, narrowing as you go:

- [AGENTS.md § Code map](./AGENTS.md#code-map) indexes what lives where: repository layout and per-module ownership.
- This file explains how it runs: runtime shape, on-disk state, and the structural rationale behind that code map.
- The layered `AGENTS.md` contracts state the rules for a subtree, loaded automatically when you work there: the root contract, [`crates/rimz/`](./crates/rimz/AGENTS.md), and a contract per major subtree ([`agents/`](./crates/rimz/src/agents/AGENTS.md), [`harness/`](./crates/rimz/src/harness/AGENTS.md), [`store/`](./crates/rimz/src/store/AGENTS.md), [`mux/`](./crates/rimz/src/mux/AGENTS.md), [`tests/integration/`](./crates/rimz/tests/integration/AGENTS.md)).
- Each module's `//!` header is the per-file authority. This map never restates it; the topic docs under `docs/` (mapped in [docs/AGENTS.md](./docs/AGENTS.md#documentation-map)) explain behavior.

## Runtime shape

There is no general RimZ daemon. The only writers of product truth are the CLI and hook subprocesses and the elected per-workspace hook drainer. The sidebar is a native pane that reads store state in process, read-only.

```text
terminal emulator
  mux session (Zellij or tmux)
    sidebar panes (one supervisor each, holding the pane's terminal)
  sidebar host (one per session, detached; paints every pane, read-only on the store)
      shared room lanes, including the ranked four-slot hidden-mirror dispatcher
      elected user-scoped spending service and walker threads (disposable warm cache)
    shells, scripts, agents, CI helpers
                │
                │  host room socket              (the wakeup channel of record)
                │  hook stdin/stdout             (the decision channel)
                ▼
rimz CLI and hook subprocesses
  workspace identity · store writes · supervised-run waits
  mux commands through MuxBackend · hook installers
                │
                ▼
workspace store (a directory of flat files)
```

The sidebar host (the room host) alone publishes pane frames, agent projection, and metrics history, and passive CLI reads consume those publications. Its room socket is the wakeup channel of record, and backend-specific fast paths are latency hints layered over it ([multiplexers.md](./docs/internals/multiplexers.md)). Its wire is [`wakeup/`](./crates/rimz/src/wakeup/mod.rs), a leaf below the store. The shared host data plane, push channels, and timing cadences are in [state.md](./docs/internals/sidebar/state.md).

### Processes and helpers

Each entry is one process, helper, or shared record beside the one-shot CLI.

- **Hook drainer.** The hook frontend appends a raw ingress frame and nudges the hidden `rimz hooks drain`. One elected drainer per workspace runs each frame through a fresh `rimz hooks apply` child with its captured environment, so provider reads never share another agent's account home, and exits after sixty idle seconds. Claimed frames recover by their ingress IDs; signal firing and unkeyed transcript writes are at-most-once ([harness contract](./crates/rimz/src/harness/AGENTS.md)).
- **Agent exec wrapper.** Every interactive launch retains its wrapper. On Unix it has two images: startup compiles and binds the provider, then same-PID exec enters a thin parked image, with fold-dependent supervision in hidden helper children; off Unix, supervision stays in process. A scoped launch's host-side wrapper also holds a drainer socket lease for the provider's lifetime ([the exec wrapper](./docs/internals/harness/fleet.md#resident-wrapper)).
- **Scoped launch view.** With machine `agents.isolation = "scoped"`, the wrapper launches its ready process through Linux bubblewrap. Each agent pane gets a host-visible mount view with a per-agent `/tmp` shared with its subagents; this is not process or network containment, and the mux stays on the host ([sandbox.md](./docs/internals/sandbox.md)).
- **OpenCode session step.** OpenCode model selection adds an adapter-owned pre-exec session step inside the provider's launch view. It uses a bounded, stdin-leased helper to prepare the session, then replaces itself with the provider at the same PID and unchanged environment ([model selection](./docs/internals/agents/adapter_opencode.md#model-selection)).
- **Run-bound command cells.** A thin `agents run-command` wrapper terminates its command's process group when the leader run ends and closes only its own pane; ordinary command cells execute raw argv ([command cells](./docs/internals/harness/scripting.md#run-bound-command-cells)).
- **Sidebar host.** The sidebar's only painter is one detached `rimz sidebar host` per mux session. Each tab's sidebar pane runs a `rimz sidebar serve` supervisor that holds the pane's terminal and hands the host its output. The host is started by the first pane that finds none, exits ten seconds after its last pane detaches, and is replaced on reload ([state.md](./docs/internals/sidebar/state.md#the-room-host-and-its-attachments), [renderer contract](./crates/rimz/src/sidebar_pane/AGENTS.md)).
- **Terminal-input record.** On Linux, the sidebar host, message delivery, and the pane writer share a locked room-runtime terminal-input record ([messaging](./docs/internals/harness/messaging.md#four-modes-on-one-timing-axis)).
- **rimzd view.** The [rimzd view](./docs/internals/rimzd.md) holds the loop panel, Codex broker, content-slot supervisors, and a hidden `rimz daemon host` wrapper around Claude remote control. Each follows verified build changes in the room record and execs in place, keeping its pid and pane.
- **Language-server brokers.** Optional [shared language servers](./docs/internals/lsp.md) each have a hidden `rimz lsp serve` broker per checkout/server key with an on-demand stdio server, held by process leases from agent wrappers and attached editors. Brokers write their own runtime entries, cost history, and diagnostics, never agent store state.
- **MCP server.** Each Claude or Codex agent's provider starts its own stateless `rimz mcp serve`, a stdio [MCP server](./docs/internals/mcp.md), unless `[agents] rimz-mcp-server = false` or the profile's `mcps` list names no RimZ capability ([launch registration](./docs/reference/cli/mcp.md#launch-registration)). Each tool call runs one `rimz` one-shot as that agent.
- **Loops and signals.** The optional loop timer is an OS-owned one-minute trigger for a one-off `rimz loop tick`, not a resident RimZ process, and it yields roots with a live sidebar ([the external tick](./docs/internals/harness/loops.md#the-external-tick)). A signal fires its listening tasks in place, in whichever process observes the event ([the signal vocabulary](./docs/internals/harness/loops.md#the-signal-vocabulary)), and a `rimz wait --run` watcher is a detached subprocess ([watched commands](./docs/internals/harness/loops.md#watched-commands)). Retaining tasks keep busy triggers in a per-room durable backlog ([the backlog protocol](./docs/internals/harness/loops.md#waiting-trigger-backlog)).
- **Browser access.** A RimZ gate binds the configured writable port, authenticates, and forwards to ttyd on a private loopback port; a second no-auth, input-blocked ttyd serves only rooms that `rimz web share` allowlists. A hidden `rimz web exec` shim validates each connection's session, so ttyd owns transport but no session authority ([web.md](./docs/internals/web.md)).
- **Spending service.** Private service and walker threads run inside whichever host-eligible long-lived RimZ process wins the lifetime lock; one-shot inspection commands connect or fall back directly. `spending.json` and the provider/workspace publications remain truth, and process exit discards the service, so this warm cache introduces no RimZ daemon. A publishing walk requests a detached `stats --rebuild --unattended` helper while saved spend days have pending usage ([spending.md](./docs/internals/agents/spending.md#closed-day-spend-lines)).

### State ownership

Each row names what one owner holds and what it leaves to the others.

| Owner | Owns | Does not own |
| --- | --- | --- |
| Multiplexer | panes, views, sessions, attach/detach, layout, scrollback | store state, agent status, handler trust |
| RimZ store | events, agent state, messages, runs, snapshots | terminal rendering, pane mechanics |
| CLI / hook subprocesses | durable writes, supervised-run waiters, mux command calls | UI presentation |
| Sidebar | rendering, focus affordances, human actions through the CLI | durable state files |
| ttyd browser daemons | authenticated writable transport; no-auth input-blocked transport | workspace identity, session validation, broadcast allowlist, durable state |
| Agents | native UI, prompts, sandboxing, bypass behaviour | RimZ store state |
| Language-server broker | server stdio, saved-file watcher, query and attach socket, editor buffers, process leases, memory accounting | agent state, panes, room lifecycle |
| Host | process resurrection, OS sandboxing | workspace state |

### State on disk

Everything durable lives under one home, `~/.rimz` (`RIMZ_HOME` relocates it), and `rimz paths` prints the current project's locations. Machine preferences are `config.toml`, `theme.toml`, `loop.toml`, and `remote.toml` at its top. The home is never a project marker: a `.rimz/` that is the home is not read as project config. Project config is `.rimz/config.toml`, which is TOML and trust-tracked.

The home also holds the definitions: `agents/`, `subagents/`, `teams/`, and `traits/` are Markdown definitions resolved on config load, beside the shared `skills/` library and installed `packs/`. `RIMZ_AGENTS_HOME` relocates those trees, the library, `workflows/`, and packs together, overriding `RIMZ_HOME` for them. Pack definitions merge below home files before team seats bind. The `packs` module owns source identity, Git staging and publication, exclusions, and namespace claims, not item scanning. Definition loaders compare committed and staged pack sets; the CLI renders the review and asks for consent. [Definitions](./docs/reference/definitions.md) owns the tier rules and source format; [packs](./docs/reference/cli/packs.md) owns installation and layout.

State is tiers of plain files, scoped by what each one outlives. [`disk/paths.rs`](./crates/rimz/src/disk/paths.rs) (`StatePaths`, `RuntimePaths`) owns the path constants, and [store.md → What is on disk](./docs/internals/store.md#what-is-on-disk) is the file-by-file catalog; this is the map.

```text
workspace store         ~/.rimz/ws/<basename>-<hex>/
  one room's durable truth: the framed event log and the records beside it,
  plus the producer caches that survive a reboot
  log/, records/, audit/, cache/, owned/, tmp/, shared/, team-notes/, automation-notes/,
  out/, locks/ group files by lifetime;
  every room flock lives in locks/, never cleared by reset or teardown
  room.lock: exclusive birth/reset claim, shared pane-supervisor lifetime holds
  workspace.json (layout: 2) and rimz remain room identity at the root
  records/lsp-usage.jsonl retains room-scoped query counts across broker restarts
  owned/workflows/<run-id>/ holds live workflow records, inline runs' definitions and journals;
  its work/ subtree is the node-writable workflow directory; an ended run's home
  moves whole to owned/finished-workflows/<run-id>/ with its saved history.txt

empty agent directories ~/.rimz/empty/<workspace-dir>/<candidate-handle>/
  persistent agent cwd, separate from temp units; retained session rows keep it
  after exit, and room GC removes unrecorded directories past the owned grace

per-workspace runtime   $XDG_RUNTIME_DIR/rimz/ws/<basename>-<hex>/  (or /tmp/rimz-<uid>/…)
  sock/, live/, lanes/ group disposable coordination by lifetime;
  reset and teardown clear all three; lanes have no age sweep;
  snapshots/ holds rebuildable store checkpoints

shared runtime          $XDG_RUNTIME_DIR/rimz/shared/
  the account-global election locks and the spending service's versioned socket

language-server runtime $XDG_RUNTIME_DIR/rimz/lsp/
  machine admission lock and queue; per-key entries, sockets, publication locks

machine-wide persistent ~/.rimz/{accounts,builds,loops,workflows,logs,web,cache}/
  accounts/<kind>/<name>/ provider homes RimZ placed, builds/<build_id>/rimz
  immutable executable generations, the loop registry, append-only logs, the
  browser daemon pid/port records and token store, the broadcast room allowlist,
  and cache/ for downloaded assets, the presence plugin, and cache/providers/
  (accounts, rate limits, credits, spend, pricing, account budget ledgers,
  and auto-redeem stamps and rate memos)
  accounts/.history/ holds shared role, effort, and closed-day spend records
  ~/.rimz/lsp-history.jsonl holds learned peaks; logs/lsp.log.jsonl holds evidence

note store              ~/.rimz/notes/<kind>/<YYYYMMDD>/<slug>.md
  Markdown notes `rimz notes add` writes once, owner-only; RimZ never deletes,
  moves, or renames one, and `rimz uninstall` keeps the tree
```

One rule sorts a new file into a tier: **persistent tiers hold what must survive a reboot, runtime tiers hold what is meaningless without the process that wrote it.** A socket, or a cache that only speeds the next read, is runtime and dies with the session; a durable record, or a cache the dashboard needs to open warm, is persistent.

Provider caches rebuild at the cost of a cold dashboard and one full spending walk, but that same tree also holds what does not rebuild: account budget choices and park state, and auto-redeem stamps and rate memos. `accounts/` holds provider logins that nothing can regenerate and no RimZ command removes: a shared account's credentials beside links into the provider's own home, or a standalone account's credentials, settings, and transcripts. Its `.history/` store shares role, effort, and closed-day spend records across homes and survives cache deletion.

The store tier's durability contract (temp-file-plus-rename, the framed log, and the write classes) is [store.md](./docs/internals/store.md); the provider files are [providers.md](./docs/internals/agents/providers.md) and [spending.md](./docs/internals/agents/spending.md); the loop registry is [loops.md](./docs/internals/harness/loops.md); executable staging is [sidebar.md → Build promotion](./docs/internals/sidebar/sidebar.md#build-promotion).

## Code and crate structure

This section gives the structural reasons behind the [code map](./AGENTS.md#code-map).

Add a crate only when ownership, target type, or dependency profile demands it. `rimz` is the one host runtime artifact: CLI, domain library, and native sidebar renderer ship in the same executable, every renderer projecting the same `rimz sidebar snapshot` view-model.

`rimz-presence-zellij` clears that bar because it is a wasm32-wasip1 plugin binary owned by the Zellij plugin-host boundary, depending on no rimz crate. Every `rimz` build embeds the vendored wasm checked in under `crates/rimz/presence/` and materializes it under `~/.rimz/cache/` before loading it. Its decision logic is a `zellij-tile`-free pure `policy.rs`, and its host-boundary argv/KDL rendering is pure `wire.rs`; both host-test in the ordinary workspace run. The plugin talks to rimz only through wake/focus argv: it observes pane and client changes, publishes `pane-topology.json` as a Zellij-only cache, and reports settled switch generations. Host code derives session focus, pane roles, repair ownership, and typed events through the common presence projector ([multiplexers.md → Zellij presence channel](./docs/internals/multiplexers.md#the-zellij-presence-plugin)).

The top-level `theme` module is renderer-neutral presentation policy: it resolves schemes, semantic tones, provider identity, resolved glyph vocabulary and setup probes, and shared human value formats. The CLI and native sidebar convert its `Tone` values only at their renderer edges, so the runtime ships one interface language without coupling the core to `anstyle` or ratatui ([theme.md](./docs/internals/theme.md)).

`build.rs` embeds the data artifacts at compile time, network-free:

- The generated fresh-install token-pricing seed for the shared immutable SQLite price table. [`agents/pricing/source.rs`](./crates/rimz/src/agents/pricing/source.rs) projects it through the hidden `rimz pricing-refresh` helper, and `cargo xtask pricing-refresh` delegates to it ([spending.md → Token pricing](./docs/internals/agents/spending.md#token-pricing)).
- The Alacritty theme catalog (`cargo xtask theme-refresh`).
- The presence-plugin wasm.

Hidden helper subcommands are machinery for other processes, not for humans. They are marked `#[command(hide = true)]` where [`cli/`](./crates/rimz/src/cli/mod.rs) declares them, and their protocols live in the owning internals docs.

## Tests

Integration tests live under each crate's `tests/`. `crates/rimz` collects its suites into a single `tests/integration/` binary with a shared `common/` harness; layout and conventions are in the [suite contract](./crates/rimz/tests/integration/AGENTS.md). The shape of each kind of test is in [rust-conventions.md → Tests](./docs/contributing/rust-conventions.md#tests), the test profiles are in [gates.md](./docs/contributing/gates.md#test-profiles), and the grep-style architectural invariants (decision-channel integrity, sidebar/store separation, pane-primitive use, and more) run through `cargo xtask invariants`.
