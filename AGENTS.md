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
- **Strong types** for workspace, request, resolver, and pane IDs, and for surfaces, statuses, and protocol versions.
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
- Pin the write root once per session: `git rev-parse --show-toplevel` of the working directory is the only root, and every file write resolves under it. Never address a file by a hard-coded absolute path — a worktree mirrors `main`'s tree, so an absolute path is exactly how a write meant for the worktree lands on `main`. A path a subagent reports — Explore, Plan, search — is a place to read, not where to write: re-resolve it under the pinned root before editing. A write that resolves outside the pinned root is a bug, not a shortcut.
- Rust unless the task targets docs, tests, scripts, examples, or build glue.
- Root docs stay short and authoritative; detail lives in `docs/` and is linked.
- Update [ARCHITECTURE.md](./ARCHITECTURE.md) when modules move. Update [DESIGN.md](./DESIGN.md) only when a product or runtime invariant changes.
- Reuse the canonical example in docs: `~/code/query-engine` with `main` + `feature-migration` worktrees.
- `cargo xtask ci` is the single contributor entry point for every quality gate; new automation lands as an `xtask` task, not a shell script. Gate stack lives in [docs/contributing/rust-conventions.md](./docs/contributing/rust-conventions.md).

## Testing requirements

- Run the suite with `cargo xtask test` (wraps `cargo nextest run`) — nextest is the only suite runner; never bare `cargo test`. Doctests go through `cargo xtask doctest`.
- Routine validation defaults to the fast relevant nextest subset plus lightweight gates; run journey, live-backend, performance, or full CI only when the change touches those surfaces, their fixtures, or shared infrastructure.
- Unit tests around state machines, schema rendering, and trust decisions.
- Integration tests for ledger CAS, bridge timeouts, socket wakeups, and backend parity.
- Keep test tiers separate: function/unit tests stay inline and pure, integration tests own subprocess/filesystem behavior, journey tests own rendered user flows, live-backend tests own real tmux/Zellij behavior, and performance tests assert bounded resource use rather than product semantics.
- Do not land ignored tests for future product targets. Capture planned behaviour in docs/roadmap, then add the executable test when the implementation is ready to make it pass under nextest.
- Golden tests for every agent hook stdout shape, including neutral timeout output.
- Every command-executing config field projected into `ExecutableSurface` (asserted by the `hash_covers_every_documented_surface_field` unit test in `crates/rimz/src/trust.rs`).
- Grep-style CI invariants reject:
  - `Stdio::inherit` in hook subprocess paths,
  - sidebar imports of ledger-write modules,
  - core auto-use of `pane capture` or `pane send`.

Full matrix in [docs/contributing/testing.md](./docs/contributing/testing.md).

## Documentation map

Every other document is a leaf from here. The `docs/` tree groups by audience: **guide** (use it), **interface** (see it), **reference** (look it up), **internals** (how it works), **contributing** (work on it).

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
- [hooks.md](./docs/internals/hooks.md) — the agent boundary: the integration trait, the two hook channels, install, and the Claude/Codex native-event mappings.
- [agent.md](./docs/internals/agent.md) — agent state model: the rollup, state machine, posture, and liveness.
- [transcript.md](./docs/internals/transcript.md) — agent context read-path: transcript discovery, tail parsing, the Claude/Codex JSONL→internal mapping, and the statusline / app-server rich-context transports.
- [account.md](./docs/internals/account.md) — agent accounts and balances: the plan/metered model, the per-provider auth and rate-limit mapping, the out-of-band account probe, and the provider-dashboard aggregation.
- [adapter/claude-reference.md](./docs/internals/adapter/claude-reference.md) — Claude Code upstream protocol reference: hook events and decision schema, the full statusline JSON schema, and the auth surface, each pinned to its source URL for refresh.
- [adapter/codex-reference.md](./docs/internals/adapter/codex-reference.md) — Codex upstream protocol reference: hooks, the `notify` channel, the app-server JSON-RPC API, the rollout JSONL, and the auth file, each pinned to its source URL for refresh.
- [web.md](./docs/internals/web.md) — Zellij-only browser access and session-route design.
- [performance.md](./docs/internals/performance.md) — render-thread hot path, the cost map, and the rules a performance change follows.

**Contributing** — `docs/contributing/`
- [rust-conventions.md](./docs/contributing/rust-conventions.md) — Rust shape: CLI, errors, stdout discipline, actor pattern, test taxonomy, dependency snapshot, toolchain, quality gates.
- [testing.md](./docs/contributing/testing.md) — required test matrix and invariants.
- [roadmap.md](./docs/contributing/roadmap.md) — build order and current milestone.
