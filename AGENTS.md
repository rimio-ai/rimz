# AGENTS.md

Working contract for humans and coding agents contributing to Rimz. Read on entry. Topic detail lives in the leaves linked from the [documentation map](#documentation-map); never duplicate it here.

> **Invariant.** Rimz routes attention: it surfaces which agent needs you and takes you straight to its pane, where you answer in the agent's own UI.

A child `AGENTS.md` under a subtree extends this file with local constraints; it never restates parent rules.

## Tone

Declarative, present tense. State the contract; don't narrate history. Prefer imperatives (`Use Result.`) over prohibitions.

Lead with the capability, and introduce a concept by what it does before leaning on it — say what queuing *does* (hold text for the agent's next open turn) rather than "a queued message never interrupts a turn". State a safety boundary as a positive commitment (`Hook stdout is the decision channel.`); avoid "X is a Y, not a Z" and defensive "we don't…" / "it never…" framing. Reserve negation for genuine disambiguation.

Markdown prose uses one logical line per paragraph, list item, and blockquote paragraph. Do not hard-wrap.

## Engineering principles

- **Explicit Rust.** Typed IDs, state machines, structured parsers, explicit errors. Domain errors return `Result`; `unwrap`/`expect`/panic belong in tests, build scripts, and provably-impossible states (with a comment).
- **Strong types** for workspace, request, pane, agent-kind, and agent-session IDs, and for surfaces, statuses, and protocol versions.
- **Structured parsers** for TOML, JSON, KDL, and agent payloads.
- **Store durability.** File-state writes use temp-file plus rename; event-log writes follow the [store durability contract](./docs/internals/store/store.md).
- **Fail-fast on preconditions.** A configured capability that cannot work fails at the entry point with the fix — `rimz start` refuses rather than launching a degraded surface. Best-effort is for latency and enrichment (sidebar wakeups, app-server context), never for a precondition the user switched on.

## Product invariants

- **Durability first.** Correctness lives in durable records, CAS rules, nonces, and per-request sockets. Wakeups and pane reads are latency, never truth.
- **Hook stdout is the decision channel.** Logs go to stderr or Rimz state logs; hook helper children get fresh stdio.
- **Cross-backend parity.** Zellij and tmux are first-class; core behaviour never depends on a backend-only feature.
- **Pane I/O is explicit.** `pane capture` and `pane send` are public primitives, and `message` routes human text through the same send path; pane reads stay in rendering, explicit `pane capture` calls, and Codex turn-death confirmation.
- **Sidebar is read-only on the store.** Sidebar code reads via `rimz sidebar snapshot`; store-write modules stay out of the sidebar's import graph.
- **Trust is product behaviour.** Every command-executing config field is in the trust hash, with a test that proves it.
- **Security surfaces stay visible.** Project trust, notification handlers, hook install diffs, and privacy settings are product behaviour.

## Room quick reference

Addresses are `@handle[#channel]`; full grammar lives in [agents.md](./docs/reference/cli/agents.md).

```sh
rimz agents                          # agent cards, current channel
rimz agents '#auth'                  # one lane's cards
rimz agents show @coder              # card: activity, context, messages, transcript
rimz agents logs @coder -n 20        # transcript tail (-f follows)
rimz message @coder "rebase first"   # park for the next turn boundary
rimz message --steer @coder "stop"   # interrupt the live turn now
rimz message show msg_<id>           # why a message hasn't landed
rimz pane list                       # every pane, labelled with @handles
rimz pane capture @coder             # what the agent's pane shows right now
rimz loop show <task>                # schedule, next fire, run forensics
```

## Implementation rules

- `AGENTS.md` and `CLAUDE.md` are one file via symlink; edits to either land in both.
- Rust unless the task targets docs, tests, scripts, examples, or build glue.
- Root docs stay short and authoritative; detail lives in `docs/` and is linked.
- Update the [code map](#code-map) when modules move, [ARCHITECTURE.md](./ARCHITECTURE.md) when the runtime shape changes, and [DESIGN.md](./DESIGN.md) only when a product or runtime invariant changes.
- Contributor automation lives in `xtask/`; command surface and gate stack in [rust-conventions.md](./docs/contributing/rust-conventions.md).
- Use `uv` for Python helpers.

## Testing requirements

- Run `cargo xtask gate` before a PR or hand-off: it auto-formats, then runs invariants, docs-links, lint, and the fast nextest subset. That subset excludes the live-backend and journey tiers; `cargo xtask test -P live` and `cargo xtask test -P journey` are the exact excluded complement.
- Use `cargo xtask test <filter>` for focused iteration; nextest is the only suite runner, so never run bare `cargo test`.
- Escalate to journey, live-backend, performance, dependency gates, or full CI (`cargo xtask ci`) when the change touches those surfaces, their fixtures, or shared infrastructure; instrumented coverage runs through `cargo xtask coverage`.
- CI's `tests-complete` is the single branch-protection check; the pipeline shape (archive build, per-tier slices, branch protection) lives in [rust-conventions.md](./docs/contributing/rust-conventions.md).
- Keep test tiers separate: unit tests stay in-module and pure, integration tests own subprocess/filesystem behaviour, journey tests own rendered user flows, live-backend tests own real tmux/Zellij, and performance tests assert bounded resource use.
- A module's unit tests have one home: inline `#[cfg(test)] mod tests`, or a sibling `tests.rs` past the size gate (enforced by `cargo xtask invariants`). Minimal usage examples live as unit tests. Shape and threshold in [rust-conventions.md](./docs/contributing/rust-conventions.md).
- Do not land ignored tests for future product targets. Capture planned behaviour in the owning doc, then add the executable test when the implementation can make it pass under nextest.
- Grep-style invariants (`cargo xtask invariants`) guard the architectural boundaries — decision-channel integrity, sidebar/store separation, the trust hash, pane-primitive use, and more.

## Code map

Rimz ships as one Rust binary: the `rimz` crate is CLI, domain library, and native sidebar renderer, with `rimz-presence-zellij` a standalone Zellij wasm plugin beside it. This indexes what lives where; runtime shape and the single-binary rationale live in [ARCHITECTURE.md](./ARCHITECTURE.md), and each module's `//!` header is the per-file authority.

**Repository** — `crates/rimz/` (the binary plus the runtime/domain library; `benches/`, and `presence/`/`pricing/`/`themes/` data `build.rs` embeds), `crates/rimz-presence-zellij/` (headless Zellij presence plugin, wasm32-wasip1, no rimz-crate deps), `docs/` (product and engineering docs; `docs/externals/` mirrors upstream), `xtask/` (task runner and every gate), `examples/`, `ci/`, `scripts/`, `supply-chain/`.

**Subsystems — `crates/rimz/src/`**, each carrying its own `AGENTS.md` contract:
- `cli/` — command parsing, one `run(...)` per subcommand, shared `cli/render/` output.
- `agents/` — the `AgentAdapter` trait, `state.rs` rollup, per-kind adapters (Claude, Codex, Pi, OpenCode), spend/pricing/account.
- `harness/` — layout IR, teams, address grammar, launch argv, supervised runs and their wake socket, loop scheduling, resume.
- `message/` — durable per-agent message queue: park-vs-live dispatch, live-pane send, scheduled wakeups.
- `store/` — durable state engine: framed event log, message/run stores, snapshot rebuild, wakeups, GC.
- `mux/` — Zellij/tmux seam: `MuxBackend`, subprocess engine, reconcile planner, recovery.
- `sidebar/` — data plane: producer election, pulled-truth/event fusion, projection fold, heavy-lane refresh.
- `sidebar_pane/` — native renderer process: serve loop, pets, the `render/theme/` color pipeline.
- `remote/` — SSH grammar, reconnect policy, link health, `remote.toml`.
- `diag/` — diagnostic-only JSONL append surfaces.

**Top-level modules** — identity and reach (`workspace`, `web`, `channel`, `worktree`, `disk_usage`, `forge`); transcript/panes/seams (`transcript`, `pane`, `sock`, `ids`, `trust`); daemon view (`remote_control`, `daemon_content`); process and config (`config`, `observability`, `agent_activity`, `lane`, `proc`, `reload`, `osc`, `build_id`, `child_process`, `tui`, `testkit`).

## Documentation map

Every other document is a leaf from here, grouped by purpose: **guide** (use it), **interface** (see it), **reference** (look it up), **internals** (how it works), **externals** (upstream surfaces), **contributing** (work on it).

**Root** — [README.md](./README.md) (product entry), [docs/README.md](./docs/README.md) (user documentation index, the README's Docs link), [DESIGN.md](./DESIGN.md) (the attention problem, design pillars, invariants, non-goals), [ARCHITECTURE.md](./ARCHITECTURE.md) (runtime shape, on-disk state, code-map rationale).

**Guide** — `docs/guide/`
- [installation.md](./docs/guide/installation.md) — prerequisites and every install path: Homebrew, prebuilt binaries, Cargo, source.
- [setup.md](./docs/guide/setup.md) — first-pass machine setup: config init, hooks, true color, pets, loop knobs, Zellij/tmux baselines.
- [product.md](./docs/guide/product.md) — the working tour: the room, the loop, and the scenarios from local fleet to scripted pipeline.
- [experience.md](./docs/guide/experience.md) — the first session walked step by step: install, consent gate, first agent, first fleet.
- [sidebar.md](./docs/guide/sidebar.md) — reading the sidebar: zones, agent cards and process rows, the agent lifecycle, attention routing and card ranking.
- [theme.md](./docs/guide/theme.md) — sidebar theming: palettes, color depth and slot overrides, custom themes, animations, provider branding, pets.
- [security.md](./docs/guide/security.md) — threat model and guardrails.

**Interface** — `docs/interface/`
- [sidebar.md](./docs/interface/sidebar.md) — the sidebar on screen: cockpit, agent cards, provider dashboard, rendered frames, glyph legend.

**Reference** — `docs/reference/`
- [cli.md](./docs/reference/cli.md) — CLI entry and command map; leaves [getting-started.md](./docs/reference/cli/getting-started.md) (start/attach/remote/web/list/setup/doctor), [web.md](./docs/reference/cli/web.md) (Zellij browser + token helpers), [agents.md](./docs/reference/cli/agents.md) (launch, `-p`, message, transcript, pane, worktree, loop, addressing), [channel.md](./docs/reference/cli/channel.md), [hooks-trust.md](./docs/reference/cli/hooks-trust.md), [maintenance.md](./docs/reference/cli/maintenance.md).
- [configuration.md](./docs/reference/configuration.md) — config tiers, per-machine template, project trust shape, privacy.

**Internals** — `docs/internals/`, one group per subsystem.
- **`harness/`** — [harness.md](./docs/internals/harness/harness.md) (layout IR, address grammar, `-p` runs and the run wake, loop tasks), [message.md](./docs/internals/harness/message.md) (routing, records, delivery, channels, transcript), [worktree.md](./docs/internals/harness/worktree.md) (Rimz-owned Git worktrees), [trust.md](./docs/internals/harness/trust.md) (the harness permission model: executable surface, grants, the stale diff).
- **`agents/`** — [agent.md](./docs/internals/agents/agent.md) (agent model: rollup, state machine, adapter boundary, live context), adapters [claude.md](./docs/internals/agents/adapter/claude.md)/[codex.md](./docs/internals/agents/adapter/codex.md)/[pi.md](./docs/internals/agents/adapter/pi.md)/[opencode.md](./docs/internals/agents/adapter/opencode.md), [provider.md](./docs/internals/agents/provider.md) (accounts, balances, spend, pricing).
- **`store/`** — [store.md](./docs/internals/store/store.md) (the durable state engine: on-disk shape, write classes, wakeups).
- **`mux/`** — [multiplexers.md](./docs/internals/mux/multiplexers.md) (Zellij/tmux contracts).
- **`sidebar/`** — [sidebar.md](./docs/internals/sidebar/sidebar.md) (mechanics: presence, ranking, reload), [state.md](./docs/internals/sidebar/state.md) (pulled truth, events, fusion, cadences), [notifications.md](./docs/internals/sidebar/notifications.md), [pets.md](./docs/internals/sidebar/pets.md).
- **`health/`** — [diagnostics.md](./docs/internals/health/diagnostics.md) (diagnostics log, frame observer, off-box Sentry), [performance.md](./docs/internals/health/performance.md) (render budget, cost map, fleet overhead), [profiling.md](./docs/internals/health/profiling.md) (live-fleet field guide).
- **`reach/`** — [welcome.md](./docs/internals/reach/welcome.md) (lobby and `rimz stats`), [remote.md](./docs/internals/reach/remote.md) (SSH attach, reconnect, link health), [web.md](./docs/internals/reach/web.md) (Zellij browser access).

**Externals** — `docs/externals/`, upstream protocol references pinned to source URLs.
- [claude-reference.md](./docs/externals/agent-adapter/claude-reference.md), [codex-reference.md](./docs/externals/agent-adapter/codex-reference.md), [pi-reference.md](./docs/externals/agent-adapter/pi-reference.md), [opencode-reference.md](./docs/externals/agent-adapter/opencode-reference.md) — agent adapter protocols: hooks, transcripts, APIs, auth.
- [zellij-reference.md](./docs/externals/mux-adapter/zellij-reference.md), [tmux-reference.md](./docs/externals/mux-adapter/tmux-reference.md) — mux backend surfaces: plugin/control API, config, layout, hooks, options.

**Contributing** — `docs/contributing/`
- [rust-conventions.md](./docs/contributing/rust-conventions.md) — Rust shape: CLI, errors, stdout discipline, actor pattern, test taxonomy, toolchain, quality gates.
- [sidebar-screenshots.md](./docs/contributing/sidebar-screenshots.md) — contributor PNG capture workflow for sidebar frames.
