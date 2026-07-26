# AGENTS.md

Working contract for humans and coding agents contributing to RimZ. Read on entry. Topic detail lives in the leaves linked from the [documentation map](#documentation-map); never duplicate it here.

> **Invariant.** RimZ routes attention: it surfaces which agent needs you and takes you straight to its pane, where you answer in the agent's own UI.

A child `AGENTS.md` extends this file with what holds only inside its subtree.

## How to read this contract

Two kinds of statement live here, and they earn different deference.

**Invariants** are load-bearing: a product promise, an architectural boundary a `cargo xtask invariants` grep enforces, or a durability rule. Breaking one breaks the build or the product. The sections below say which statements are invariants. Change one deliberately, in its own commit, with the doc and the gate moving together.

**Everything else is a default** — the choice that has been right most of the time here. Follow it absent better information; depart when the work in front of you argues otherwise, and say why in the PR or hand-off. A default that keeps losing is a bug in this file, so fix the file.

Exercise judgment without asking on how to decompose a change, which tests earn their cost, and when a rule's rationale does not reach the case at hand. Push back when a request looks wrong, rests on a false premise, or has a simpler shape — an early correction beats a faithful implementation of the wrong thing. Ask when a wrong guess is expensive and a question is cheap.

Match the size of the change to the size of the ask, and leave the refactor you noticed on the way as a note unless it blocks you. When this map and the territory disagree, the code is what ships: follow the code, then fix the map in the same change.

## Tone

Declarative, present tense. State the contract; don't narrate history.

Lead with the capability, and introduce a concept by what it does before leaning on it — say what queuing *does* (hold text for the agent's next open turn) before saying what it avoids. State a safety boundary as a positive commitment (`Hook stdout is the decision channel.`), and reserve negation for genuine disambiguation.

Markdown prose uses one logical line per paragraph, list item, and blockquote paragraph. Do not hard-wrap.

## Engineering principles

The house style is explicit Rust: typed IDs for identifiers that cross a module boundary (workspace, request, pane, agent-kind, agent-session) and for surfaces, statuses, and protocol versions; structured parsers for TOML, JSON, KDL, and agent payloads; state machines where a bool would drift. Domain errors return `Result`. `unwrap`/`expect`/panic belong in tests, build scripts, and provably-impossible states — leave a comment naming why the state is impossible.

Two invariants sit underneath that style:

- **Store durability.** File-state writes use temp-file plus rename; event-log writes follow the [store durability contract](./docs/internals/store.md).
- **Fail-fast on preconditions.** A configured capability that cannot work fails at the entry point with the fix — `rimz start` refuses rather than launching a degraded surface. Best-effort is for latency and enrichment (sidebar wakeups, app-server context), never for a precondition the user switched on.

## Product invariants

These are the promises the product is built on. Treat a change to one as a product decision, not an implementation detail.

- **Durability first.** Correctness lives in durable records, CAS rules, nonces, and per-request sockets. Wakeups and pane reads are latency, never truth.
- **Automation is accountable.** User-benefiting automation appends durable assist records and surfaces in `rimz stats`; internal repairs keep durable diagnostic records ([loops.md](./docs/internals/harness/loops.md#the-assist-log), [diagnostics.md](./docs/internals/diagnostics.md)).
- **One interface language.** Every human-facing surface resolves color through the [shared theme core](./docs/internals/theme.md): provider identity for names and emblems, typed state roles, hierarchy tones, and quantity tones; invariants enforce the renderer boundaries.
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
rimz agents attribution --md         # credit the lane's agents in a PR footnote
rimz agents restart @coder           # bounce in place and resume the session
rimz agents resume '#docs'           # restore every closed place in one lane
rimz teams                           # configured teams and their live instances
rimz teams show forge                # resolved roles, validation, and live members
rimz teams forge -w feat-x           # launch one configured cohort in a worktree
rimz teams resume forge              # reopen a configured team cohort
rimz teams focus|restart|stop forge  # drive one live team cohort
rimz message @coder "rebase first"   # park for the next turn boundary
rimz message @coder --wait "did the migration land? one line" # ask and print the reply
rimz message --steer @coder "stop"   # interrupt the live turn now
rimz message show msg_<id>           # why a message hasn't landed
rimz asks --json                     # structured prompts that currently block agents
rimz answer @coder 2                 # answer the current supported prompt in its native UI
rimz pane list                       # every pane, labelled with @handles
rimz pane capture @coder             # what the agent's pane shows right now
rimz loop show <task>                # schedule, next fire, run forensics
rimz loop logs <task>                # full forensics for recent runs
```

## Implementation rules

- `AGENTS.md` and `CLAUDE.md` are one file per directory, joined by symlink; an edit to either lands in both.
- Rust unless the task targets docs, tests, scripts, examples, or build glue. Use `uv` for Python helpers.
- Root docs stay short and authoritative; detail lives in `docs/` and is linked. Update the [code map](#code-map) when modules move, [ARCHITECTURE.md](./ARCHITECTURE.md) when the runtime shape changes, and [DESIGN.md](./DESIGN.md) only when a product or runtime invariant changes.
- Leave [CHANGELOG.md](./CHANGELOG.md) untouched in a pull request. It is written as a standalone change once the work merges and before the version release, so concurrent branches never contend over the same lines.
- Contributor automation lives in `xtask/`; command surface and gate stack in [rust-conventions.md](./docs/contributing/rust-conventions.md).

## Testing

- Run `cargo xtask gate` before a PR or hand-off: it auto-formats, then runs invariants, docs-links, lint, and the fast nextest subset. That subset excludes the live-backend and journey tiers; `cargo xtask test -P live` and `cargo xtask test -P journey` are the exact excluded complement.
- Use `cargo xtask test <filter>` for focused iteration. nextest is the only suite runner, so bare `cargo test` does not work here.
- Run a built `target/.../rimz` against a real multiplexer through `cargo xtask sandbox -- <command>`; the sandbox gives HOME, XDG, tmux, and Zellij disposable roots and tears its mux servers down on exit.
- Judge the reach of your own change and escalate to journey, live-backend, performance, dependency gates, or full CI (`cargo xtask ci`) when it touches those surfaces, their fixtures, or shared infrastructure. CI's `tests` job is the branch-protection floor rather than the ceiling; the pipeline shape lives in [rust-conventions.md](./docs/contributing/rust-conventions.md). Instrumented coverage runs through `cargo xtask coverage`.
- Keep test tiers separate: unit tests stay in-module and pure, integration tests own subprocess/filesystem behaviour, journey tests own rendered user flows, live-backend tests own real tmux/Zellij, and performance tests assert bounded resource use.
- A module's unit tests have one home: inline `#[cfg(test)] mod tests`, or a sibling `tests.rs` past the size gate (enforced by `cargo xtask invariants`). Minimal usage examples live as unit tests. Shape and threshold in [rust-conventions.md](./docs/contributing/rust-conventions.md).
- Capture planned behaviour in the owning doc and add the executable test when the implementation can make it pass; an ignored test standing in for a future product target does not land.
- **Invariant.** `cargo xtask invariants` greps tracked text — Rust and Markdown alike — to guard the architectural boundaries: decision-channel integrity, sidebar/store separation, the trust hash, pane-primitive use, and more. Because it matches text rather than parsed code, an `AGENTS.md` inside a guarded subtree is scanned along with it: describe a boundary in prose and leave the path the rule bans unwritten.

## Code map

RimZ ships as one Rust binary: the `rimz` crate is CLI, domain library, and native sidebar renderer, with `rimz-presence-zellij` a standalone Zellij wasm plugin beside it. This indexes what lives where; runtime shape and the single-binary rationale live in [ARCHITECTURE.md](./ARCHITECTURE.md), and each module's `//!` header is the per-file authority.

**Repository** — `crates/rimz/` (the binary plus the runtime/domain library; `benches/`, and `presence/`/`pricing/`/`themes/` data `build.rs` embeds), `crates/rimz-presence-zellij/` (headless Zellij presence plugin, wasm32-wasip1, no rimz-crate deps), `docs/` (product and engineering docs; `docs/externals/` mirrors upstream), `xtask/` (task runner and every gate), `examples/`, `ci/`, `docker/`, `scripts/`, `supply-chain/`.

**Subsystems — `crates/rimz/src/`**, each carrying its own `AGENTS.md` contract:
- `cli/` — command parsing, one `run(...)` per subcommand, shared `cli/render/` output.
- `agents/` — the provider-neutral `AgentDefinition` catalog, caller-aligned capability contracts and services, `state.rs` rollup, private `adapters/` implementations for every built-in and process plugin, and shared spend/pricing/account machinery including upstream pricing projection.
- `room/` — private managed-room context, birth/reset lifecycle, sidebar/presence options, and health gating.
- `harness/` — layout IR, teams, address grammar, launch argv, supervised runs and their wake socket, loop scheduling, resume planning, and rebirth recovery inspection/materialization.
- `message/` — durable per-agent message queue: park-vs-live dispatch, live-pane send, scheduled wakeups.
- `store/` — durable state engine: framed event log, message/run stores, snapshot rebuild, wakeups, GC.
- `mux/` — Zellij/tmux seam: `MuxBackend`, subprocess engine, reconcile planner, recovery.
- `sidebar/` — data plane: Zellij presence ingestion, producer election, pulled-truth/event fusion, projection fold, heavy-lane refresh.
- `sidebar_pane/` — native renderer process: serve loop, pets, and the ratatui theme/component edge.
- `remote/` — SSH grammar, reconnect policy, link health, `remote.toml`.
- `diag/` — diagnostic-only JSONL append surfaces.

**Top-level modules** — identity and reach (`workspace`, `web`, `channel`, `worktree`, `disk_usage`, `forge`); transcript/panes/seams (`transcript`, `pane`, `sock`, `ids`, `trust`); shared presentation (`theme` semantic palette, provider identity, glyph resolution and setup probes, and value formats); daemon view (`daemon_view` spec and reconciliation, `daemon_content` supervisors, `remote_control` provider-neutral readiness/toggle coordination); process and config (`config`, `observability`, `agent_activity`, `lane`, `proc`, `reload`, `osc`, `build_id`, `child_process`, `tui`, `testkit`); install lifecycle (`update` install-origin detection, release verification, and atomic binary replacement; `uninstall` machine-wide removal mechanics).

## Documentation map

Every other document is a leaf from here, grouped by purpose: **interface** (see it), **reference** (look it up), **internals** (how it works), **externals** (upstream surfaces), **contributing** (work on it).

**Root** — [README.md](./README.md) (product entry), [docs/README.md](./docs/README.md) (user documentation index, the README's Docs link), [DESIGN.md](./DESIGN.md) (the attention problem, design pillars, invariants, non-goals), [ARCHITECTURE.md](./ARCHITECTURE.md) (runtime shape, on-disk state, code-map rationale), [CHANGELOG.md](./CHANGELOG.md) (user-visible change per release, one section per git tag; each section is generated from merged history ahead of its tag).

**Guide** — `docs/guide/` teaches the shipped product to people who never open the source; its own `AGENTS.md` and [docs/README.md](./docs/README.md) index the pages. Ship a user-visible behaviour change into the guide page that promises it, and get your own working knowledge from `docs/internals/` instead.

**Interface** — `docs/interface/`
- [sidebar.md](./docs/interface/sidebar.md) — the sidebar on screen: cockpit, agent cards, provider dashboard, rendered frames, glyph legend.

**Reference** — `docs/reference/`
- [cli.md](./docs/reference/cli.md) — CLI entry and command map; leaves [getting-started.md](./docs/reference/cli/getting-started.md) (start/attach/sessions/remote/web/list/setup/doctor), [web.md](./docs/reference/cli/web.md) (shared ttyd browser access and credential helpers), [agents.md](./docs/reference/cli/agents.md) (launch, `-p`, message, transcript, pane, worktree, loop, addressing), [teams.md](./docs/reference/cli/teams.md) (discover, inspect, install, launch, and resume named teams), [events.md](./docs/reference/cli/events.md) (streaming lifecycle transitions), [asks.md](./docs/reference/cli/asks.md) (structured blocking prompts and native answers), [channel.md](./docs/reference/cli/channel.md), [hooks-trust.md](./docs/reference/cli/hooks-trust.md), [maintenance.md](./docs/reference/cli/maintenance.md).
- [agent-support.md](./docs/reference/agent-support.md) — per-agent status, integration surface, and permission-mode mapping for Claude, Codex, Amp, Copilot, Kimi, Pi, OpenCode, Antigravity, Cursor, Droid, Kiro, Qwen, Grok; [agent-plugins.md](./docs/reference/agent-plugins.md) — external bundle, canonical wire, and probe contracts.

**Internals** — `docs/internals/`; [README.md](./docs/internals/README.md) is the index. The three multi-doc subsystems keep a folder; every other subsystem is one flat file.
- **`agents/`** — [model.md](./docs/internals/agents/model.md) (agent model: rollup, state machine, displayed status, instance lifecycle), [adapter.md](./docs/internals/agents/adapter.md) (the adapter layer: registry, capability traits, hook path, install, context sources, coverage), [plugin.md](./docs/internals/agents/plugin.md) (third-party process plugins), the per-kind mappings [claude.md](./docs/internals/agents/claude.md)/[codex.md](./docs/internals/agents/codex.md)/[amp.md](./docs/internals/agents/amp.md)/[copilot.md](./docs/internals/agents/copilot.md)/[kimi.md](./docs/internals/agents/kimi.md)/[pi.md](./docs/internals/agents/pi.md)/[opencode.md](./docs/internals/agents/opencode.md)/[antigravity.md](./docs/internals/agents/antigravity.md)/[cursor.md](./docs/internals/agents/cursor.md)/[droid.md](./docs/internals/agents/droid.md)/[kiro.md](./docs/internals/agents/kiro.md)/[qwen.md](./docs/internals/agents/qwen.md)/[grok.md](./docs/internals/agents/grok.md), [providers.md](./docs/internals/agents/providers.md) (accounts, balances, spend, pricing).
- **`harness/`** — [harness.md](./docs/internals/harness/harness.md) (subsystem overview, layout IR, exec wrapper, address grammar, resume, pane reclaim), [scripting.md](./docs/internals/harness/scripting.md) (supervised `-p` runs: record, wake socket, verify/retry, output), [loops.md](./docs/internals/harness/loops.md) (scheduled tasks: catalog, elder firing, fire gates, assist log), [budget.md](./docs/internals/harness/budget.md) (dollar caps: scopes, ledgers, verdict, waiver, park, gate), [messaging.md](./docs/internals/harness/messaging.md) (routing, records, delivery, reply waits, channels, transcript, asks), [worktrees.md](./docs/internals/harness/worktrees.md) (RimZ-owned Git worktrees), [trust.md](./docs/internals/harness/trust.md) (permission model: executable surface, grants, the stale diff).
- **`sidebar/`** — [sidebar.md](./docs/internals/sidebar/sidebar.md) (mechanics: presence, ranking, reload), [state.md](./docs/internals/sidebar/state.md) (pulled truth, events, fusion, cadences), [notifications.md](./docs/internals/sidebar/notifications.md), [pets.md](./docs/internals/sidebar/pets.md) (dashboard pet: actions, tracks, assets, render tiers).
- **Single-doc subsystems** — [theme.md](./docs/internals/theme.md) (four-layer color pipeline, glyph catalog, provider identity, value formats), [store.md](./docs/internals/store.md) (durable state engine: on-disk shape, event log, write and read paths, write classes, wakeups), [multiplexers.md](./docs/internals/multiplexers.md) (the `MuxBackend` seam: Zellij/tmux contracts and the Zellij presence plugin), [rimzd.md](./docs/internals/rimzd.md) (managed `rimzd` view: pane spec, command identity, reconciliation and repair), [remote.md](./docs/internals/remote.md) (SSH attach and aliases, reconnect supervisor, link health, port forwarding, bandwidth), [web.md](./docs/internals/web.md) (shared ttyd daemon, browser shim, credentials, and remote wire), [stats.md](./docs/internals/stats.md) (`rimz stats`: spend cache, window model, render ladder, held dashboard), [diagnostics.md](./docs/internals/diagnostics.md) (diagnostics log, frame observer, off-box Sentry), [performance.md](./docs/internals/performance.md) (performance model: threads and clocks, cost map, principles, fleet overhead), [profiling.md](./docs/internals/profiling.md) (live-fleet field guide).

**Externals** — `docs/externals/`, upstream protocol references pinned to source URLs.
- [claude-reference.md](./docs/externals/agent-adapter/claude-reference.md), [codex-reference.md](./docs/externals/agent-adapter/codex-reference.md), [grok-reference.md](./docs/externals/agent-adapter/grok-reference.md), [pi-reference.md](./docs/externals/agent-adapter/pi-reference.md), [opencode-reference.md](./docs/externals/agent-adapter/opencode-reference.md), [antigravity-reference.md](./docs/externals/agent-adapter/antigravity-reference.md), [amp-reference.md](./docs/externals/agent-adapter/amp-reference.md), [droid-reference.md](./docs/externals/agent-adapter/droid-reference.md), [copilot-reference.md](./docs/externals/agent-adapter/copilot-reference.md), [cursor-reference.md](./docs/externals/agent-adapter/cursor-reference.md), [kimi-reference.md](./docs/externals/agent-adapter/kimi-reference.md), [qwen-reference.md](./docs/externals/agent-adapter/qwen-reference.md), [kiro-reference.md](./docs/externals/agent-adapter/kiro-reference.md) — agent adapter protocols: hooks, transcripts, APIs, auth.
- [zellij-reference.md](./docs/externals/mux-adapter/zellij-reference.md), [tmux-reference.md](./docs/externals/mux-adapter/tmux-reference.md) — mux backend surfaces: plugin/control API, config, layout, hooks, options.

**Contributing** — `docs/contributing/`
- [rust-conventions.md](./docs/contributing/rust-conventions.md) — Rust shape: CLI, errors, stdout discipline, actor pattern, test taxonomy, toolchain, quality gates.
- [agent-adapters.md](./docs/contributing/agent-adapters.md) — the built-in adapter integration playbook: protocol reference to landed adapter, step by step, with the deliverables checklist.
- [sidebar-screenshots.md](./docs/contributing/sidebar-screenshots.md) — contributor PNG capture workflow for sidebar frames.
