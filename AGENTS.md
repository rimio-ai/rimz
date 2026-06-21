# AGENTS.md

Working contract for humans and coding agents contributing to Rimz. Read on entry. Topic detail lives in the leaves linked from the [documentation map](#documentation-map); never duplicate it here.

> **Invariant.** Rimz routes attention: it surfaces which agent needs you and takes you straight to its pane, where you answer in the agent's own UI. A resolver delegates routine answers only when you explicitly enrol one, and the chain ends with you.

If a child `AGENTS.md` appears under a subtree, it extends this file with local-only constraints — it never restates parent rules.

## Tone

Declarative, present tense. State the contract; don't narrate history. Prefer imperatives (`Use Result.`, `Resolvers own pane I/O.`) over prohibitions where the meaning carries.

Say what it is. Describe Rimz by what it does, builds, and offers — lead with the capability. Reader-facing docs especially: open on the value, not the guardrail. Introduce a concept by what it does before you lean on it, so a feature is never explained by negating a term the reader has not met yet (`the default loop answers nothing on your behalf` fails this — it leans on *resolver* before the reader has one; say what the loop *does* — get you to the question fast — instead). Avoid "X is a Y, not a Z" framing and defensive "we don't…" / "it never…" constructions; state a real safety boundary as a positive commitment (`Resolvers own pane I/O.`) rather than a prohibition. Reserve negation for genuine disambiguation a reader would otherwise get wrong.

Markdown prose uses one logical line per paragraph, list item, and blockquote paragraph. Do not hard-wrap prose.

## Engineering principles

- **Explicit Rust.** Typed IDs, typed state machines, structured parsers, explicit errors. Domain errors return `Result`; `unwrap`, `expect`, and panics belong in tests, build scripts, and provably-impossible states (with a comment).
- **Strong types** for workspace, request, resolver, pane, agent-kind, and agent-session IDs, and for surfaces, statuses, and protocol versions.
- **Structured parsers** for TOML, JSON, KDL, and agent payloads.
- **Ledger durability.** File-state writes use temp-file plus rename. Event-log writes follow the durability contract in [docs/internals/sidebar/ledger.md](./docs/internals/sidebar/ledger.md).
- **Fail-fast as a precondition, not best-effort.** A configured capability that cannot work fails at the entry point with the fix — `rimz start` refuses rather than launching a degraded surface that errors downstream. Best-effort is for latency and enrichment (sidebar wakeups, app-server context), never for a precondition the user switched on.

## Product invariants

- **Ledger first.** Correctness lives in the ledger, CAS rules, nonces, and per-request sockets. Sidebar wakeups are latency, not truth.
- **Hook stdout is the decision channel.** Logs go to stderr or Rimz state logs. Hook helper children get fresh stdio.
- **Cross-backend parity.** Zellij and tmux are first-class. Core behaviour never depends on a backend-only feature.
- **Pane I/O is explicit.** `pane capture` and `pane send` are public primitives; `steer` and `queue` route human-authored text through the same send path, while pane reads stay in rendering and resolver-owned inspection.
- **Sidebar is read-only on the ledger.** Sidebar code reads via `rimz sidebar snapshot`; ledger-write modules stay out of the sidebar's import graph.
- **Trust is product behaviour.** Every command-executing config field is in the trust hash, with a test that proves it.
- **Security surfaces stay visible.** Project trust, resolver allowlists, hook install diffs, and privacy settings are product behaviour, not implementation details.

## Implementation rules

- `AGENTS.md` and `CLAUDE.md` are the same file via symlink (`ln -s AGENTS.md CLAUDE.md`); edits to either land in both.
- Rust unless the task targets docs, tests, scripts, examples, or build glue.
- Root docs stay short and authoritative; detail lives in `docs/` and is linked.
- Update [ARCHITECTURE.md](./ARCHITECTURE.md) when modules move. Update [DESIGN.md](./DESIGN.md) only when a product or runtime invariant changes.
- Contributor automation lives in `xtask/`; command surface and gate stack live in [docs/contributing/rust-conventions.md](./docs/contributing/rust-conventions.md).
- Use `uv` for Python helpers when Python is needed.

## Testing requirements

- Run the suite with `cargo xtask test` (wraps `cargo nextest run`) — nextest is the only suite runner; never bare `cargo test`. Doctests go through `cargo xtask doctest`.
- Routine validation defaults to the fast relevant nextest subset plus lightweight gates; run journey, live-backend, performance, or full CI (`cargo xtask ci`) only when the change touches those surfaces, their fixtures, or shared infrastructure.
- Keep test tiers separate: function/unit tests stay in-module and pure, integration tests own subprocess/filesystem behavior, journey tests own rendered user flows, live-backend tests own real tmux/Zellij behavior, and performance tests assert bounded resource use rather than product semantics.
- A module's unit tests have one home: inline `#[cfg(test)] mod tests` by default; past the size gate the whole module moves to a sibling `tests.rs` (enforced by `cargo xtask invariants`). Doctests stay on public items as minimal usage examples. Shape and threshold in [docs/contributing/rust-conventions.md](./docs/contributing/rust-conventions.md).
- Do not land ignored tests for future product targets. Capture planned behaviour in the owning design or internals doc, then add the executable test when the implementation is ready to make it pass under nextest.
- Grep-style CI invariants (`cargo xtask invariants`) guard the architectural boundaries — decision-channel integrity, sidebar/ledger separation, the trust hash, pane-primitive use, and more.

## Documentation map

Every other document is a leaf from here. The `docs/` tree groups by purpose: **guide** (use it), **interface** (see it), **reference** (look it up), **internals** (how it works), **externals** (upstream surfaces), **contributing** (work on it).

**Root**
- [README.md](./README.md) — product entry point.
- [DESIGN.md](./DESIGN.md) — what Rimz offers, the attention problem, the design choices that answer it, the three operating paths, commitments, non-goals.
- [ARCHITECTURE.md](./ARCHITECTURE.md) — runtime shape, repository layout, module ownership.

**Guide** — `docs/guide/`
- [installation.md](./docs/guide/installation.md) — source install prerequisites and Rust toolchain setup for Linux and macOS.
- [zellij.md](./docs/guide/zellij.md) — configure your own Zellij `config.kdl`: the essential and recommended settings for a modern coding-agent session.
- [tmux.md](./docs/guide/tmux.md) — configure your own `~/.tmux.conf`: true color, copy-mode, status bar, and the behaviors agents rely on.
- [product.md](./docs/guide/product.md) — the working tour: the room, the loop, and the four scenarios people run.
- [experience.md](./docs/guide/experience.md) — first-run-to-fleet experience, section by section.
- [security.md](./docs/guide/security.md) — threat model and guardrails.

**Interface** — `docs/interface/`
- [sidebar.md](./docs/interface/sidebar.md) — the sidebar on screen: the cockpit, the agent cards, and the provider dashboard, with rendered frames and the glyph legend.

**Reference** — `docs/reference/`
- [cli.md](./docs/reference/cli.md) — CLI entry point and command map; grouped command details live under `docs/reference/cli/`.
- [configuration.md](./docs/reference/configuration.md) — config tiers, generated per-machine template, project trust shape, privacy.
- [theme.md](./docs/reference/theme.md) — sidebar theming: built-in and Ghostty-derived palettes, color depth and slot overrides, custom theme files, status-head animations, provider brand styling.

**Internals** — `docs/internals/`, grouped into four leaves by subsystem.

- **`agents/`** — integrate, operate, and house coding agents.
  - [agent.md](./docs/internals/agents/agent.md) — the agent model end to end: the rollup, state machine, turn phase, and liveness; the adapter boundary (the integration trait, the two hook channels, the decision channel, install, and adding an agent); and the live context read-path.
  - **`adapter/`** — per-kind native mappings: hooks/lifecycle, context/transcript, account, and spend for one agent.
    - [claude.md](./docs/internals/agents/adapter/claude.md) — Claude Code.
    - [codex.md](./docs/internals/agents/adapter/codex.md) — Codex.
    - [pi.md](./docs/internals/agents/adapter/pi.md) — Pi.
    - [opencode.md](./docs/internals/agents/adapter/opencode.md) — OpenCode.
  - [provider.md](./docs/internals/agents/provider.md) — provider accounts, balances, spend, and pricing: the plan/metered model, the out-of-band account probe, the provider-dashboard aggregation, the full-history cost/spending walk, and the three-layer token price table (embedded snapshot, remote refresh, builtins).
  - [resolvers.md](./docs/internals/agents/resolvers.md) — resolver protocol, chain, pane primitives.
  - [harness.md](./docs/internals/agents/harness.md) — the agent harness end to end: the layout IR and backend tab/split placement, the agent-address grammar, supervised `rimz agents -p` runs (records, wakeups, output/input formats, posture, shared launch params), the `rimz agents exec` wrapper and run-pane cleanup, and the in-tab `steer`/`queue` message path (targets, gates, delivery, attempts, hazards).
  - [loop.md](./docs/internals/agents/loop.md) — scheduled loop tasks: one supervised agent turn on an OS schedule, with calendar, interval, cron, one-shot, posture, and window-priming ping tasks.
  - [worktree.md](./docs/internals/agents/worktree.md) — Rimz-owned Git worktrees as the room's channels: creation and the `[worktree] dir`/base template, the `rimz-worktree.json` ownership marker, `.worktreeinclude` file seeding, and the landed-work cleanup decision plus the `rimz gc` sweep.
- **`sidebar/`** — the room and the substrate beneath it.
  - [sidebar.md](./docs/internals/sidebar/sidebar.md) — sidebar mechanics: presence, ranking, launch, reload recovery, and view-model behaviour (the on-screen look lives in [interface/sidebar.md](./docs/interface/sidebar.md)).
  - [state.md](./docs/internals/sidebar/state.md) — sidebar pulled truth, typed realtime events, fusion, process roles, and timing cadences.
  - [notifications.md](./docs/internals/sidebar/notifications.md) — best-effort desktop, bell, and command notifications layered over the sidebar attention model.
  - [ledger.md](./docs/internals/sidebar/ledger.md) — durable state and the blocking decision bridge.
  - [multiplexers.md](./docs/internals/sidebar/multiplexers.md) — Zellij and tmux backend contracts.
  - [trust.md](./docs/internals/sidebar/trust.md) — executable-surface hash, trust states, auto-revoke.
  - [pets.md](./docs/internals/sidebar/pets.md) — opt-in provider-dashboard pets: pane-local cell-art rendering, the fleet-status sprite model, canned captions, and the Codex-CDN asset cache.
- **`health/`** — is the running system correct and within budget.
  - [observe.md](./docs/internals/health/observe.md) — the sidebar observer: windowed flap detection and per-frame consistency checks over the rendered frame stream, recorded as typed anomalies in the [diagnostics](./docs/internals/health/diagnostics.md) channel.
  - [diagnostics.md](./docs/internals/health/diagnostics.md) — durable typed sidebar anomaly log: taxonomy, retention, frame captures, inspection examples, and the episode investigation workflow; plus the live-state read path for card-content questions.
  - [performance.md](./docs/internals/health/performance.md) — render-thread budget, the cost map, the CPU/RAM/IO/storage/network overhead estimated for a 20-100 agent fleet, and the rules a performance change follows.
  - [observability.md](./docs/internals/health/observability.md) — off-box error reporting: the per-machine Sentry opt-in, the single init point across every process, the `tracing` bridge that lifts `warn!`/`error!` and agent rate-limit/overload conditions into events, and the data boundary.
- **`reach/`** — how clients reach the room.
  - [welcome.md](./docs/internals/reach/welcome.md) — the lobby (the room picker shown when the entry path has no room to enter) and the standalone `rimz stats` pace surface, both projections of `rimz list` and the spend aggregate.
  - [remote.md](./docs/internals/reach/remote.md) — SSH remote attach, reconnect policy, ControlMaster probe stream, and link-health sidecar.
  - [web.md](./docs/internals/reach/web.md) — Zellij-only browser access and session-route design.

**Externals** — `docs/externals/`
- [claude-reference.md](./docs/externals/agent-adapter/claude-reference.md) — Claude Code upstream protocol reference: hook events and decision schema, the full statusline JSON schema, and the auth surface, each pinned to its source URL for refresh.
- [codex-reference.md](./docs/externals/agent-adapter/codex-reference.md) — Codex upstream protocol reference: hooks, the `notify` channel, the app-server JSON-RPC API, the rollout JSONL, and the auth file, each pinned to its source URL for refresh.
- [pi-reference.md](./docs/externals/agent-adapter/pi-reference.md) — Pi upstream protocol reference: the in-process extension API (events, payloads, blocking returns, install surface), the session JSONL, the headless RPC/JSON modes, and the auth file, each pinned to its source URL for refresh.
- [opencode-reference.md](./docs/externals/agent-adapter/opencode-reference.md) — OpenCode upstream protocol reference: the in-process plugin API (hooks, bus events, blocking returns, install surface), the SQLite session store, the server HTTP API, and the auth file, each pinned to its source URL for refresh.
- [zellij-reference.md](./docs/externals/mux-adapter/zellij-reference.md) — Zellij upstream reference: the wasm plugin API (lifecycle, events, commands, types, permissions, workers, pipes), the CLI control surface, the configuration options, the layout KDL, and session resurrection, each pinned to its source URL for refresh.
- [tmux-reference.md](./docs/externals/mux-adapter/tmux-reference.md) — tmux upstream reference: the client/server and socket model, the command surface the backend drives, the format language, hooks, options, the session environment, and the control-mode protocol, each pinned to its source URL for refresh.

**Contributing** — `docs/contributing/`
- [rust-conventions.md](./docs/contributing/rust-conventions.md) — Rust shape: CLI, errors, stdout discipline, actor pattern, test taxonomy, dependency snapshot, toolchain, quality gates.
- [sidebar-screenshots.md](./docs/contributing/sidebar-screenshots.md) — contributor PNG capture workflow for live and synthetic sidebar frames.
