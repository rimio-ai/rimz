# AGENTS.md

Working contract for humans and coding agents contributing to Rimz. Read on entry. Topic detail lives in the leaves linked from the [documentation map](#documentation-map); never duplicate it here.

> **Invariant.** Rimz routes attention: it surfaces which agent needs you and takes you straight to its pane, where you answer in the agent's own UI. A resolver delegates routine answers only when you explicitly enrol one, and the chain ends with you.

If a child `AGENTS.md` appears under a subtree, it extends this file with local-only constraints — it never restates parent rules.

## Tone

Declarative, present tense. State the contract; don't narrate history. Prefer imperatives (`Use Result.`, `Resolvers own pane I/O.`) over prohibitions where the meaning carries.

Say what it is. Describe Rimz by what it does, builds, and offers — lead with the capability. Reader-facing docs especially: open on the value, not the guardrail. Avoid "X is a Y, not a Z" framing and defensive "we don't…" / "it never…" constructions; state a real safety boundary as a positive commitment (`Resolvers own pane I/O.`) rather than a prohibition. Reserve negation for genuine disambiguation a reader would otherwise get wrong.

Markdown prose uses one logical line per paragraph, list item, and blockquote paragraph. Do not hard-wrap prose.

## Engineering principles

- **Explicit Rust.** Typed IDs, typed state machines, structured parsers, explicit errors. Domain errors return `Result`; `unwrap`, `expect`, and panics belong in tests, build scripts, and provably-impossible states (with a comment).
- **Strong types** for workspace, request, resolver, pane, agent-kind, and agent-session IDs, and for surfaces, statuses, and protocol versions.
- **Structured parsers** for TOML, JSON, KDL, and agent payloads.
- **Ledger durability.** File-state writes use temp-file plus rename. Event-log writes follow the durability contract in [docs/internals/ledger.md](./docs/internals/ledger.md).
- **Fail-fast as a precondition, not best-effort.** A configured capability that cannot work fails at the entry point with the fix — `rimz start` refuses rather than launching a degraded surface that errors downstream. Best-effort is for latency and enrichment (sidebar wakeups, app-server context), never for a precondition the user switched on.

## Product invariants

- **Ledger first.** Correctness lives in the ledger, CAS rules, nonces, and per-request sockets. Sidebar wakeups are latency, not truth.
- **Hook stdout is the decision channel.** Logs go to stderr or Rimz state logs. Hook helper children get fresh stdio.
- **Cross-backend parity.** Zellij and tmux are first-class. Core behaviour never depends on a backend-only feature.
- **Resolvers own pane I/O.** `pane capture` and `pane send` are public resolver primitives; core treats panes as opaque.
- **Sidebar is read-only on the ledger.** Sidebar code reads via `rimz sidebar snapshot`; ledger-write modules stay out of the sidebar's import graph.
- **Trust is product behaviour.** Every command-executing config field is in the trust hash, with a test that proves it.
- **Security surfaces stay visible.** Project trust, resolver allowlists, hook install diffs, and privacy settings are product behaviour, not implementation details.

## Implementation rules

- `AGENTS.md` and `CLAUDE.md` are the same file via symlink (`ln -s AGENTS.md CLAUDE.md`); edits to either land in both.
- Rust unless the task targets docs, tests, scripts, examples, or build glue.
- Root docs stay short and authoritative; detail lives in `docs/` and is linked.
- Update [ARCHITECTURE.md](./ARCHITECTURE.md) when modules move. Update [DESIGN.md](./DESIGN.md) only when a product or runtime invariant changes.
- Contributor automation lives in `xtask/`; command surface and gate stack live in [docs/contributing/rust-conventions.md](./docs/contributing/rust-conventions.md).

## Testing requirements

- Run the suite with `cargo xtask test` (wraps `cargo nextest run`) — nextest is the only suite runner; never bare `cargo test`. Doctests go through `cargo xtask doctest`.
- Routine validation defaults to the fast relevant nextest subset plus lightweight gates; run journey, live-backend, performance, or full CI (`cargo xtask ci`) only when the change touches those surfaces, their fixtures, or shared infrastructure.
- Keep test tiers separate: function/unit tests stay in-module and pure, integration tests own subprocess/filesystem behavior, journey tests own rendered user flows, live-backend tests own real tmux/Zellij behavior, and performance tests assert bounded resource use rather than product semantics.
- A module's unit tests have one home: inline `#[cfg(test)] mod tests` by default; past the size gate the whole module moves to a sibling `tests.rs` (enforced by `cargo xtask invariants`). Doctests stay on public items as minimal usage examples. Shape and threshold in [docs/contributing/rust-conventions.md](./docs/contributing/rust-conventions.md).
- Do not land ignored tests for future product targets. Capture planned behaviour in docs/roadmap, then add the executable test when the implementation is ready to make it pass under nextest.
- Grep-style CI invariants (`cargo xtask invariants`) guard the architectural boundaries — decision-channel integrity, sidebar/ledger separation, the trust hash, pane-primitive use, and more.

Full matrix and the invariant list in [docs/contributing/testing.md](./docs/contributing/testing.md).

## Documentation map

Every other document is a leaf from here. The `docs/` tree groups by purpose: **guide** (use it), **interface** (see it), **reference** (look it up), **internals** (how it works), **externals** (upstream surfaces), **contributing** (work on it).

**Root**
- [README.md](./README.md) — product entry point.
- [DESIGN.md](./DESIGN.md) — what Rimz offers, the attention problem, the design choices that answer it, the three operating paths, commitments, non-goals.
- [ARCHITECTURE.md](./ARCHITECTURE.md) — runtime shape, repository layout, module ownership.

**Guide** — `docs/guide/`
- [product.md](./docs/guide/product.md) — five-minute tour, audiences, sidebar walk-through.
- [experience.md](./docs/guide/experience.md) — first-run-to-fleet experience, phase by phase.
- [security.md](./docs/guide/security.md) — threat model and guardrails.

**Interface** — `docs/interface/`
- [sidebar.md](./docs/interface/sidebar.md) — the sidebar on screen: the cockpit, the agent cards, and the provider dashboard, with rendered frames and the glyph legend.

**Reference** — `docs/reference/`
- [cli.md](./docs/reference/cli.md) — every command, grouped by intent.
- [configuration.md](./docs/reference/configuration.md) — project/per-machine config, layout IR, privacy.

**Internals** — `docs/internals/`
- [ledger.md](./docs/internals/ledger.md) — durable state and the blocking decision bridge.
- [multiplexers.md](./docs/internals/multiplexers.md) — Zellij and tmux backend contracts.
- [sidebar.md](./docs/internals/sidebar.md) — sidebar mechanics: presence, ranking, launch, reload recovery, view-model (the on-screen look lives in [interface/sidebar.md](./docs/interface/sidebar.md)).
- [resolvers.md](./docs/internals/resolvers.md) — resolver protocol, chain, pane primitives.
- [trust.md](./docs/internals/trust.md) — executable-surface hash, trust states, auto-revoke.
- [hooks.md](./docs/internals/hooks.md) — the agent boundary: the integration trait, the two hook channels, install, and the Claude/Codex/Pi native-event mappings.
- [agent.md](./docs/internals/agent.md) — agent state model: the rollup, state machine, turn phase, and liveness.
- [transcript.md](./docs/internals/transcript.md) — agent context read-path: transcript discovery, tail parsing, the Claude/Codex JSONL→internal mapping, the statusline / app-server rich-context transports, and the full-history cost/spending read-path.
- [pricing.md](./docs/internals/pricing.md) — per-model token pricing: the three-layer table (embedded snapshot, remote refresh, builtins), model resolution, and how Codex token counts become dollars.
- [account.md](./docs/internals/account.md) — agent accounts and balances: the plan/metered model, the per-provider auth and rate-limit mapping, the out-of-band account probe, and the provider-dashboard aggregation.
- [web.md](./docs/internals/web.md) — Zellij-only browser access and session-route design.
- [worktrees.md](./docs/internals/worktrees.md) — Rimz-owned git worktrees, agent tab layouts, supervised cleanup, and backend tab rendering.
- [performance.md](./docs/internals/performance.md) — render-thread hot path, the cost map, and the rules a performance change follows.

**Externals** — `docs/externals/`
- [claude-reference.md](./docs/externals/agent-adapter/claude-reference.md) — Claude Code upstream protocol reference: hook events and decision schema, the full statusline JSON schema, and the auth surface, each pinned to its source URL for refresh.
- [codex-reference.md](./docs/externals/agent-adapter/codex-reference.md) — Codex upstream protocol reference: hooks, the `notify` channel, the app-server JSON-RPC API, the rollout JSONL, and the auth file, each pinned to its source URL for refresh.
- [pi-reference.md](./docs/externals/agent-adapter/pi-reference.md) — Pi upstream protocol reference: the in-process extension API (events, payloads, blocking returns, install surface), the session JSONL, the headless RPC/JSON modes, and the auth file, each pinned to its source URL for refresh.
- [opencode-reference.md](./docs/externals/agent-adapter/opencode-reference.md) — OpenCode upstream protocol reference: the in-process plugin API (hooks, bus events, blocking returns, install surface), the SQLite session store, the server HTTP API, and the auth file, each pinned to its source URL for refresh.
- [zellij-reference.md](./docs/externals/mux-adapter/zellij-reference.md) — Zellij upstream reference: the wasm plugin API (lifecycle, events, commands, types, permissions, workers, pipes), the CLI control surface, the configuration options, the layout KDL, and session resurrection, each pinned to its source URL for refresh.
- [tmux-reference.md](./docs/externals/mux-adapter/tmux-reference.md) — tmux upstream reference: the client/server and socket model, the command surface the backend drives, the format language, hooks, options, the session environment, and the control-mode protocol, each pinned to its source URL for refresh.

**Contributing** — `docs/contributing/`
- [rust-conventions.md](./docs/contributing/rust-conventions.md) — Rust shape: CLI, errors, stdout discipline, actor pattern, test taxonomy, dependency snapshot, toolchain, quality gates.
- [testing.md](./docs/contributing/testing.md) — required test matrix and invariants.
- [roadmap.md](./docs/contributing/roadmap.md) — build order and current milestone.
