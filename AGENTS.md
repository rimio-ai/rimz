# AGENTS.md

Working contract for humans and coding agents contributing to RimZ. Read on entry. Topic detail lives in the leaves linked from the [documentation map](#documentation-map); never duplicate it here.

> **Invariant.** RimZ routes attention: it surfaces which agent needs you and takes you straight to its pane, where you answer in the agent's own UI.

A child `AGENTS.md` under a subtree extends this file with local constraints; it never restates parent rules.

## Tone

Declarative, present tense. State the contract; don't narrate history. Prefer imperatives (`Use Result.`) over prohibitions.

Lead with the capability, and introduce a concept by what it does before leaning on it — say what queuing *does* (hold text for the agent's next open turn) rather than "a queued message never interrupts a turn". State a safety boundary as a positive commitment (`Hook stdout is the decision channel.`); avoid "X is a Y, not a Z" and defensive "we don't…" / "it never…" framing. Reserve negation for genuine disambiguation.

Markdown prose uses one logical line per paragraph, list item, and blockquote paragraph. Do not hard-wrap.

## Engineering principles

- **Explicit Rust.** Typed IDs, state machines, structured parsers, explicit errors. Domain errors return `Result`; `unwrap`/`expect`/panic belong in tests, build scripts, and provably-impossible states (with a comment).
- **Strong types** for workspace, request, pane, agent-kind, and agent-session IDs, and for surfaces, statuses, and protocol versions.
- **Structured parsers** for TOML, JSON, KDL, and agent payloads.
- **Store durability.** File-state writes use temp-file plus rename; event-log writes follow the [store durability contract](./docs/internals/store.md).
- **Fail-fast on preconditions.** A configured capability that cannot work fails at the entry point with the fix — `rimz start` refuses rather than launching a degraded surface. Best-effort is for latency and enrichment (sidebar wakeups, app-server context), never for a precondition the user switched on.

## Product invariants

- **Durability first.** Correctness lives in durable records, CAS rules, nonces, and per-request sockets. Wakeups and pane reads are latency, never truth.
- **Hook stdout is the decision channel.** Logs go to stderr or RimZ state logs; hook helper children get fresh stdio.
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
rimz agents history @coder -n 10     # per-turn tokens, cost, and outcome
rimz agents restart @coder           # bounce in place and resume the session
rimz message @coder "rebase first"   # park for the next turn boundary
rimz message @coder --wait "did the migration land? one line" # ask and print the reply
rimz message --steer @coder "stop"   # interrupt the live turn now
rimz message show msg_<id>           # why a message hasn't landed
rimz asks --json                     # structured prompts that currently block agents
rimz answer @coder 2                 # answer the current supported prompt in its native UI
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
- CI's `tests` job is the branch-protection check; the pipeline shape (single compile, profile steps, branch protection) lives in [rust-conventions.md](./docs/contributing/rust-conventions.md).
- Keep test tiers separate: unit tests stay in-module and pure, integration tests own subprocess/filesystem behaviour, journey tests own rendered user flows, live-backend tests own real tmux/Zellij, and performance tests assert bounded resource use.
- A module's unit tests have one home: inline `#[cfg(test)] mod tests`, or a sibling `tests.rs` past the size gate (enforced by `cargo xtask invariants`). Minimal usage examples live as unit tests. Shape and threshold in [rust-conventions.md](./docs/contributing/rust-conventions.md).
- Do not land ignored tests for future product targets. Capture planned behaviour in the owning doc, then add the executable test when the implementation can make it pass under nextest.
- Grep-style invariants (`cargo xtask invariants`) guard the architectural boundaries — decision-channel integrity, sidebar/store separation, the trust hash, pane-primitive use, and more.

## Code map

RimZ ships as one Rust binary: the `rimz` crate is CLI, domain library, and native sidebar renderer, with `rimz-presence-zellij` a standalone Zellij wasm plugin beside it. This indexes what lives where; runtime shape and the single-binary rationale live in [ARCHITECTURE.md](./ARCHITECTURE.md), and each module's `//!` header is the per-file authority.

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
- [setup.md](./docs/guide/setup.md) — first-pass machine setup: config init, hooks, true color, pets, loop knobs, multiplexer essentials.
- [multiplexer.md](./docs/guide/multiplexer.md) — Zellij and tmux baselines: recommended options, parity Alt chords, themed status bar, shipped under `examples/`.
- [agents.md](./docs/guide/agents.md) — launching agents by name, permission-mode suffixes, profiles, and the layout grammar.
- [teams.md](./docs/guide/teams.md) — named teams: role handles, the `agents.toml` shape, relaunch and resume, and the sidebar's one-block treatment.
- [worktrees.md](./docs/guide/worktrees.md) — RimZ-owned Git worktrees: isolating a layout or team for parallel work, seeded files, and supervised cleanup.
- [messaging.md](./docs/guide/messaging.md) — addresses, park/steer/schedule delivery, smart compaction, agent-to-agent chat, and channels.
- [sidebar.md](./docs/guide/sidebar.md) — reading the sidebar: zones, agent cards and process rows, the agent lifecycle, attention routing and card ranking.
- [insight.md](./docs/guide/insight.md) — token and dollar insight: the cockpit and provider-dashboard figures, `rimz stats` and its heatmap and breakdowns, and how every figure is calculated.
- [remote.md](./docs/guide/remote.md) — attaching to a room on another host over SSH: a multiplexer attach, the self-healing link, and continuity across reboots.
- [web.md](./docs/guide/web.md) — browser access: the local Zellij web server, remote `--web` tunnels, and login tokens.
- [scripting.md](./docs/guide/scripting.md) — supervised `-p` runs: exit codes, JSON and streaming output, background runs and wait, and the orchestration primitives.
- [loops.md](./docs/guide/loops.md) — scheduled turns, watchdogs, budget-window priming, agent self-wakes, and the unattended permission posture.
- [notifications.md](./docs/guide/notifications.md) — off-screen attention: desktop banners, unread nudges, and handlers that push to your channels or clear routine prompts.
- [theme.md](./docs/guide/theme.md) — sidebar theming: palettes, color depth and slot overrides, custom themes, animations, provider branding.
- [pets.md](./docs/guide/pets.md) — the dashboard pet: what it acts out, built-in and petdex pets, bring-your-own sheets, pixel vs cell-art tiers, offline and privacy.
- [configuration.md](./docs/guide/configuration.md) — the whole config model: the two tiers and merge order, `rimz config set` and safe regeneration, and a section per file (config, agents, loop, theme, project trust).
- [troubleshooting.md](./docs/guide/troubleshooting.md) — `rimz doctor` first, then room-start refusals, hooks not reporting, degraded banners, version drift, reset and GC.
- [security.md](./docs/guide/security.md) — threat model and guardrails.

**Interface** — `docs/interface/`
- [sidebar.md](./docs/interface/sidebar.md) — the sidebar on screen: cockpit, agent cards, provider dashboard, rendered frames, glyph legend.

**Reference** — `docs/reference/`
- [cli.md](./docs/reference/cli.md) — CLI entry and command map; leaves [getting-started.md](./docs/reference/cli/getting-started.md) (start/attach/remote/web/list/setup/doctor), [web.md](./docs/reference/cli/web.md) (Zellij browser + token helpers), [agents.md](./docs/reference/cli/agents.md) (launch, `-p`, message, transcript, pane, worktree, loop, addressing), [asks.md](./docs/reference/cli/asks.md) (structured blocking prompts and native answers), [channel.md](./docs/reference/cli/channel.md), [hooks-trust.md](./docs/reference/cli/hooks-trust.md), [maintenance.md](./docs/reference/cli/maintenance.md).
- [agent-support.md](./docs/reference/agent-support.md) — per-agent status, integration surface, and permission-mode mapping for Claude, Codex, Pi, OpenCode.

**Internals** — `docs/internals/`; [README.md](./docs/internals/README.md) is the index. The three multi-doc subsystems keep a folder; every other subsystem is one flat file.
- **`agents/`** — [model.md](./docs/internals/agents/model.md) (agent model: rollup, state machine, adapter boundary, live context), the per-kind mappings [claude.md](./docs/internals/agents/claude.md)/[codex.md](./docs/internals/agents/codex.md)/[pi.md](./docs/internals/agents/pi.md)/[opencode.md](./docs/internals/agents/opencode.md), [providers.md](./docs/internals/agents/providers.md) (accounts, balances, spend, pricing).
- **`harness/`** — [harness.md](./docs/internals/harness/harness.md) (layout IR, address grammar, `-p` runs and the run wake, loop tasks), [messaging.md](./docs/internals/harness/messaging.md) (routing, records, delivery, channels, transcript), [worktrees.md](./docs/internals/harness/worktrees.md) (RimZ-owned Git worktrees), [trust.md](./docs/internals/harness/trust.md) (permission model: executable surface, grants, the stale diff).
- **`sidebar/`** — [sidebar.md](./docs/internals/sidebar/sidebar.md) (mechanics: presence, ranking, reload), [state.md](./docs/internals/sidebar/state.md) (pulled truth, events, fusion, cadences), [notifications.md](./docs/internals/sidebar/notifications.md), [pets.md](./docs/internals/sidebar/pets.md).
- **Single-doc subsystems** — [store.md](./docs/internals/store.md) (durable state engine: on-disk shape, write classes, wakeups), [multiplexers.md](./docs/internals/multiplexers.md) (Zellij/tmux contracts), [remote.md](./docs/internals/remote.md) (SSH attach, reconnect, link health), [web.md](./docs/internals/web.md) (Zellij browser access), [welcome.md](./docs/internals/welcome.md) (lobby and `rimz stats`), [diagnostics.md](./docs/internals/diagnostics.md) (diagnostics log, frame observer, off-box Sentry), [performance.md](./docs/internals/performance.md) (render budget, cost map, fleet overhead), [profiling.md](./docs/internals/profiling.md) (live-fleet field guide).

**Externals** — `docs/externals/`, upstream protocol references pinned to source URLs.
- [claude-reference.md](./docs/externals/agent-adapter/claude-reference.md), [codex-reference.md](./docs/externals/agent-adapter/codex-reference.md), [pi-reference.md](./docs/externals/agent-adapter/pi-reference.md), [opencode-reference.md](./docs/externals/agent-adapter/opencode-reference.md) — agent adapter protocols: hooks, transcripts, APIs, auth.
- [zellij-reference.md](./docs/externals/mux-adapter/zellij-reference.md), [tmux-reference.md](./docs/externals/mux-adapter/tmux-reference.md) — mux backend surfaces: plugin/control API, config, layout, hooks, options.

**Contributing** — `docs/contributing/`
- [rust-conventions.md](./docs/contributing/rust-conventions.md) — Rust shape: CLI, errors, stdout discipline, actor pattern, test taxonomy, toolchain, quality gates.
- [sidebar-screenshots.md](./docs/contributing/sidebar-screenshots.md) — contributor PNG capture workflow for sidebar frames.
