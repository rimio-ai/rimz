# AGENTS.md

> **Invariant.** RimZ routes attention: it surfaces which agent needs you and takes you straight to its pane, where you answer in the agent's own UI.

## How to read this contract

Two kinds of statement live here, and they earn different deference.

**Invariants** are load-bearing: a product promise, an architectural boundary a `cargo xtask invariants` grep enforces, or a durability rule. Breaking one breaks the build or the product. The sections below say which statements are invariants. Change one deliberately, in its own commit, with the doc and the gate moving together.

**Everything else is a default** — the choice that has been right most of the time here. Follow it absent better information; depart when the work in front of you argues otherwise, and say why in the PR or hand-off. A default that keeps losing is a bug in this file, so fix the file.

Exercise judgment without asking on how to decompose a change, which tests earn their cost, and when a rule's rationale does not reach the case at hand. Push back when a request looks wrong, rests on a false premise, or has a simpler shape — an early correction beats a faithful implementation of the wrong thing. Ask when a wrong guess is expensive and a question is cheap.

Match the size of the change to the size of the ask, and leave the refactor you noticed on the way as a note unless it blocks you. When this map and the territory disagree, the code is what ships: follow the code, then fix the map in the same change.

## Engineering principles

The house style is explicit Rust — typed IDs across module boundaries, structured parsers over ad-hoc string work, state machines where a bool would drift, typed `Result` errors at library boundaries — and [rust-conventions.md](./docs/contributing/rust-conventions.md) is its authority. `unwrap`/`expect`/panic belong in tests, build scripts, and provably-impossible states — leave a comment naming why the state is impossible.

For an event or timing bug, write the red end-to-end test first and prove the proposed signal reaches the consumer's decision point: producer emission and timestamp freshness are not evidence of consumer freshness.

Two invariants sit underneath that style:

- **Store durability.** Every durable write follows the [store durability contract](./docs/internals/store.md).
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
rimz agents profiles                 # configured profiles, commands, and descriptions
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
rimz subagents claude "review this"  # from an agent: launch a supervised child
rimz subagents profiles              # configured child profiles, commands, and descriptions
rimz subagents wait                  # join every live child and collect results
rimz subagents list|stop --all       # inspect or stop the caller's own children
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
- Bulk command output — `--json` snapshots, transcript tails, gate logs — reaches an agent truncated, so a command that prints one to stdout burns a turn and still loses the part that mattered. Redirect to a file under `/tmp` in the same command, then narrow it with `jq` or a targeted read.

## Testing

- Take `cargo xtask check` (singular — `checks` is the non-test gate composite) as the early broad compile signal while iterating, run `cargo xtask gate` before a PR or hand-off, batch focused tests with `cargo xtask test --name <test> [--name <test>...]` (exact names only — run a whole module through a bare nextest filter, `cargo xtask test 'sidebar_pane::app::width_control'`), and use `cargo xtask sandbox -- <command>` to drive a real multiplexer from disposable roots. Run one Cargo-family command at a time in a worktree; concurrent builds only queue on the same target-directory lock. The gate stack, the nextest-only runner, the sandbox roots, the test tiers and what each one owns, and the one-home rule for a module's unit tests all live in [rust-conventions.md](./docs/contributing/rust-conventions.md).
- Judge the reach of your own change and escalate past `gate` to journey, live-backend, performance, dependency gates, or full CI (`cargo xtask ci`) when it touches those surfaces, their fixtures, or shared infrastructure. CI's `tests` job is the branch-protection floor rather than the ceiling.
- Capture planned behaviour in the owning doc and add the executable test when the implementation can make it pass; an ignored test standing in for a future product target does not land.
- **Invariant.** `cargo xtask invariants` greps tracked text — Rust and Markdown alike — to guard the architectural boundaries. Because it matches text rather than parsed code, an `AGENTS.md` inside a guarded subtree is scanned along with it: describe a boundary in prose and leave the path the rule bans unwritten.

## Code map

This indexes what lives where; runtime shape and the single-binary rationale live in [ARCHITECTURE.md](./ARCHITECTURE.md), and each module's `//!` header is the per-file authority.

**Repository** — `crates/rimz/` (the binary plus the runtime/domain library; `benches/`, and `presence/`/`pricing/`/`themes/` data `build.rs` embeds), `crates/rimz-presence-zellij/` (headless Zellij presence plugin, wasm32-wasip1, no rimz-crate deps), `docs/` (product and engineering docs; `docs/externals/` mirrors upstream), `xtask/` (task runner and every gate), `examples/`, `ci/`, `docker/`, `scripts/`, `supply-chain/`.

**Subsystems — `crates/rimz/src/`**, each carrying its own `AGENTS.md` contract:
- `cli/` — command parsing, one `run(...)` per subcommand, shared `cli/render/` output.
- `agents/` — the provider-neutral `AgentDefinition` catalog, caller-aligned capability contracts and services, `state.rs` rollup, private `adapters/` implementations for every built-in and process plugin, and shared spend/pricing/account machinery including upstream pricing projection.
- `room/` — private managed-room context, birth/reset lifecycle, sidebar/presence options, and health gating.
- `harness/` — layout IR, teams, address grammar, launch argv, supervised runs and their wake socket, loop scheduling, resume planning, and rebirth recovery inspection/materialization.
- `message/` — durable per-agent message queue: park-vs-live dispatch, live-pane send, scheduled wakeups.
- `store/` — durable state engine: `Store` handle, canonical snapshot schema, writer mutation vocabulary/choreography, framed event log, message/run stores, wakeups, GC.
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

**Reference** — `docs/reference/` answers a specific flag or field. [cli.md](./docs/reference/cli.md) is the command map and indexes a page per scene under `docs/reference/cli/`; [agent-support.md](./docs/reference/agent-support.md) carries per-agent status, integration surface, and permission-mode mapping for every built-in adapter, and [agent-plugins.md](./docs/reference/agent-plugins.md) the external bundle, canonical wire, and probe contracts.

**Internals** — `docs/internals/` documents each subsystem for people who read the code; [README.md](./docs/internals/README.md) is the index and the per-page table. The three multi-doc subsystems (`agents/`, `harness/`, `sidebar/`) keep a folder; every other subsystem is one flat file.

**Externals** — `docs/externals/`, upstream protocol references pinned to source URLs.
- [claude-reference.md](./docs/externals/agent-adapter/claude-reference.md), [codex-reference.md](./docs/externals/agent-adapter/codex-reference.md), [grok-reference.md](./docs/externals/agent-adapter/grok-reference.md), [pi-reference.md](./docs/externals/agent-adapter/pi-reference.md), [opencode-reference.md](./docs/externals/agent-adapter/opencode-reference.md), [antigravity-reference.md](./docs/externals/agent-adapter/antigravity-reference.md), [amp-reference.md](./docs/externals/agent-adapter/amp-reference.md), [droid-reference.md](./docs/externals/agent-adapter/droid-reference.md), [copilot-reference.md](./docs/externals/agent-adapter/copilot-reference.md), [cursor-reference.md](./docs/externals/agent-adapter/cursor-reference.md), [kimi-reference.md](./docs/externals/agent-adapter/kimi-reference.md), [qwen-reference.md](./docs/externals/agent-adapter/qwen-reference.md), [kiro-reference.md](./docs/externals/agent-adapter/kiro-reference.md) — agent adapter protocols: hooks, transcripts, APIs, auth.
- [zellij-reference.md](./docs/externals/mux-adapter/zellij-reference.md), [tmux-reference.md](./docs/externals/mux-adapter/tmux-reference.md) — mux backend surfaces: plugin/control API, config, layout, hooks, options.

**Contributing** — `docs/contributing/`
- [rust-conventions.md](./docs/contributing/rust-conventions.md) — Rust shape: CLI, errors, stdout discipline, actor pattern, test taxonomy, toolchain, quality gates.
- [agent-adapters.md](./docs/contributing/agent-adapters.md) — the built-in adapter integration playbook: protocol reference to landed adapter, step by step, with the deliverables checklist.
- [atlas.md](./docs/contributing/atlas.md) — operating guide for `cargo xtask atlas` refactor analysis: reading each verb, the target-driven program shape, and the `refactor-target.toml` convergence loop.
- [sidebar-screenshots.md](./docs/contributing/sidebar-screenshots.md) — contributor PNG capture workflow for sidebar frames.
