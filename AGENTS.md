# AGENTS.md

> **Invariant.** RimZ routes attention: it surfaces which agent needs you and takes you straight to its pane, where you answer in the agent's own UI.

## How to read this contract

An **invariant** is a product promise, an architectural boundary that `cargo xtask invariants` enforces, or a durability rule, and each one is marked. Change one deliberately, in its own commit, with the doc and the gate moving together. Everything else is a default: depart from it when the work argues otherwise, and say why in the PR or hand-off. The invariants are the goal, and the rest describes the current state: where a description and the code disagree, follow the code and fix the file in the same change; where the code breaks an invariant, the code is wrong.

Every `AGENTS.md` is in its reader's window on every turn, so each holds only what all work under its directory needs, and this root only what work of every kind needs. A trap tied to one function or type goes in that item's doc comment. A rule for one tree goes in that tree's `AGENTS.md`. A rule for one kind of change goes in the contributing page that owns that kind, and the `AGENTS.md` of the tree where such a change is made routes to it.

## Product invariants

Every bullet here is an invariant, and a change to one is a product decision. [DESIGN.md](./DESIGN.md) gives the reasons.

- **Durability first.** Correctness lives in durable records, CAS rules, nonces, and per-request sockets. Wakeups and pane reads are latency, never truth. Every durable write follows the [store durability contract](./docs/internals/store.md).
- **Only agents ask.** Whatever reaches the user is an agent asking in its own pane. Automation never holds the user's attention itself, and no automation surface moves outside the two named exceptions ([DESIGN.md](./DESIGN.md#invariants)).
- **Automation is accountable.** User-benefiting automation appends durable assist records and surfaces in `rimz stats`; internal repairs keep durable diagnostic records ([loops.md](./docs/internals/harness/loops.md#the-assist-log), [diagnostics.md](./docs/internals/diagnostics.md)).
- **One interface language.** Every human-facing surface resolves color through the [shared theme core](./docs/internals/theme.md).
- **Hook stdout is the decision channel.** Logs go to stderr or RimZ state logs.
- **Cross-backend parity.** Zellij and tmux are first-class; core behaviour never depends on a backend-only feature.
- **Pane I/O is explicit.** `pane capture` and `pane send` are public primitives, and `message` routes human text through the same send path. Core code reads a pane only at the sites `cargo xtask invariants` admits.
- **Transcript origin is explicit.** RimZ-introduced text never renders as the user merely because it lacks a header ([transcript.md](./docs/internals/harness/transcript.md#writing-entries)).
- **Sidebar is read-only on the store.** Store-write modules stay out of the sidebar's import graph.
- **Trust is product behaviour.** Every command-executing config field is in the trust hash, with a test that proves it.
- **Official launches are recognized.** A provider's official program reads as its agent, whether the user starts it by hand in a room pane or RimZ starts it through its `rimz agents exec` wrapper, with a test per provider that proves it ([instances.md](./docs/internals/agents/instances.md#recognizing-a-hosted-cli)).
- **Security surfaces stay visible.** Project trust, notification handlers, hook install diffs, provider folder-trust grants, pack install summaries and update holds, and privacy settings are product behaviour.

## Engineering principles

The house style is explicit Rust, and [rust-conventions.md](./docs/contributing/rust-conventions.md) is its authority.

- **Fail-fast at the point of use.** The room always opens: a configuration problem warns at entry, and the room runs as if that setting were absent. A configured capability that cannot work fails with the fix at the command that uses it, and that command never reads a broken setting as absent where that would change the account it runs on. Best-effort is for latency and enrichment, never for a precondition the user switched on.
- `unwrap`, `expect`, and panics belong in tests, build scripts, and provably impossible states, with a comment naming why the state is impossible.
- Wrap source comments by hand to the width of the code around them, at most 100 columns: a harness rule against hard-wrapping prose covers messages and Markdown, not source comments.
- A refactor you noticed on the way becomes a note unless it blocks you.
- Update the [code map](#code-map) when modules move, [ARCHITECTURE.md](./ARCHITECTURE.md) when the runtime shape changes, and [DESIGN.md](./DESIGN.md) only when a product or runtime invariant changes.
- Leave [CHANGELOG.md](./CHANGELOG.md) untouched in a pull request: it is written as a standalone change once the work merges.
- Do not read `examples/` unless the user asks: it is sample configuration for users to copy, never authority on behaviour.
- A change to any Markdown file, an `AGENTS.md` included, follows [docs/AGENTS.md](./docs/AGENTS.md).

## Before a change

These hold for a change anywhere in the tree, and no gate catches a skipped one.

- **Rule on every writer and reader.** Before you change a predicate, filter, guard, lookup, or default, or when a field is set or cleared, list every site that writes the value and every site that reads it (`rimz lsp refs`, `rimz lsp callers`), and decide what each does under the new rule. When a change removes a call, a callee left with no production caller goes in the same change, with the `refactor-target.toml` lines that name it.
- **Prove timing at the consumer.** That the producer emitted, or that a timestamp is fresh, is not evidence that the consumer saw it in time ([Timestamps and clocks](./docs/contributing/rust-conventions.md#timestamps-and-clocks)).
- **Sweep the sentences you made false.** After a changed rule, name, set, output, path, argv, or refusal, grep the old words, by stem rather than phrase, across `docs/`, every `AGENTS.md`, rustdoc and comments, clap help text, tests, and the `refactor-target.toml` verdicts, and fix each hit before hand-off ([Sweeps](./docs/AGENTS.md#sweeps)).
- **Run the tests that name what you changed.** Grep `crates/rimz/src` and `crates/rimz/tests` for the field, command, or message you changed, and run each module that names it ([Tests outside the change](./docs/contributing/running-gates.md#tests-outside-the-change)).

## Testing

[gates.md](./docs/contributing/gates.md) defines the gate stack and the test profiles.

- While iterating, run `cargo fmt --all`, then `cargo xtask check`, then `cargo xtask lint`, and `cargo xtask doc` when you touched rustdoc.
- Run `cargo xtask gate` before a PR or hand-off, on a cold or loaded tree with `RIMZ_XTASK_TIMEOUT=60m` set. Escalate past it ([journey, live, perf, externals, full CI](./docs/contributing/gates.md#past-gate)) when the change touches those surfaces, their fixtures, or shared infrastructure.
- Run one Cargo-family command at a time in a worktree. When another worktree's Cargo process holds the lock, start yours and let it queue.
- From a room shell, run tests only through `cargo xtask test` or `cargo xtask stress`, never a bare `cargo test`, `cargo nextest run`, or a test binary: those resolve the shell's `RIMZ_HOME`/`HOME`/`XDG_*`.
- Run no stress batch, mass repeat, or parallel copies of a test unless the user asks or allows it: the host is shared.
- Stage new files with `git add -N` before `cargo xtask invariants` or a gate: the check reads tracked files only.
- Read the matching section of [running-gates.md](./docs/contributing/running-gates.md) before you [rebase](./docs/contributing/running-gates.md#rebasing), run a gate or test from a Codex shell, name one test, or pick a test's profile. Read [Writing a test](./docs/contributing/rust-conventions.md#writing-a-test) before you write or delete a test. Read [flakes.md](./docs/contributing/flakes.md) before you run the live tier, report a failure you leave unfixed, or debug a flake. Read [probes.md](./docs/contributing/probes.md) before you build a mux, git, sshd, or provider probe by hand, isolate a home, or run a branch binary against real state.

## Code map

Runtime shape and the single-binary rationale live in [ARCHITECTURE.md](./ARCHITECTURE.md); each module's `//!` header is the per-file authority.

**Repository**: `crates/rimz/` (the binary and its runtime/domain library, plus `benches/`, `scenarios/`, and the `presence/`, `pricing/`, `themes/`, and `keymaps/` data it embeds), `crates/rimz-presence-zellij/` (Zellij presence plugin), `docs/` (`docs/externals/` mirrors upstream), `xtask/` (task runner, every gate, contributor automation), [`loops/`](./loops/README.md) (one directory per repository loop task), `examples/`, `ci/`, `docker/`, `scripts/`, `supply-chain/`.

**Subsystems** in `crates/rimz/src/`, each carrying its own `AGENTS.md` contract:

- `cli/`: command parsing, one `run(...)` per subcommand, shared `cli/render/` output.
- `agents/`: the provider-neutral agent catalog, capability contracts, state rollup, handle minting, provider adapters, spend/pricing/account machinery, provider login resolution.
- `room/`: managed-room context, birth/reset/teardown lifecycle, health gating.
- `sandbox/`: Linux bubblewrap mount views, provider-home binds, per-agent temp units, skill views, capability preflight.
- `harness/`: layout IR, runtime profile resolution, teams and board stage flips, launch plan and argv, supervised runs, workflows, the hook drainer, loop scheduling and signals, resume and rebirth recovery, hibernation, the note store.
- `message/`: delivery: park-vs-live dispatch, live-pane send, the shared turn reader, scheduled wakeups, synthetic card text.
- `store/`: durable state engine: snapshot schema, framed event log, hook ingress and drain cursor, message queue, run and workflow records, GC.
- `mux/`: Zellij/tmux seam: live-session snapshot, subprocess engine, reconcile planner, recovery, focus anchor, per-pane write lock.
- `sidebar/`: data plane: presence ingestion, host production, pulled-truth/event fusion, projection fold, heavy-lane refresh.
- `sidebar_pane/`: native renderer: per-pane supervisor, per-session host, attach wire, pets, ratatui edge.
- `remote/`: SSH grammar, reconnect policy, link health, `remote.toml`.
- `diag/`: diagnostic-only JSONL append surfaces.
- `config/`: machine settings, profile and team definitions, `rimz config` editing, and the config templates.
- `mcp/`: the `rimz mcp serve` server, its tool catalog, and its wording.

**Top-level modules** in `crates/rimz/src/`:

- `launcher/`: launch orchestration, checkout creation, session swaps and molt, the agent tree stop, the agent and team joins, and the production workflow host.
- `disk`: atomic publish, home and workspace paths, JSONL append, locks and leases, usage and output measurement.
- `address`: agent-address grammar, pane binding, launch-instance grouping, and the `agents list` selector. `workspace`: resolution and the room identity pin. `web`, `worktree`, `forge` (with PR state).
- `transcript`, `pane` (identity and classification), `pixel_wire` (kitty placeholder encoding), `sock`, `ids`, `trust`, `wakeup` (the sidebar wire).
- `utils::{time, path, parallel, tokens, env}`, `fs_watch`, `theme`.
- Daemon view: `daemon_view`, `daemon_content`, `reload`, `remote_control`.
- `observability`, `agent_activity`, `lane`, `proc`, `osc`, `build_id`, `child_process`, `tui`, `testkit`.
- `blocker` (start preconditions), `update` and `uninstall` (install lifecycle), `packs/` (pack sources), `lsp/` (shared language servers), `mcp_tools` (the `rimz mcp serve` tool runner).
