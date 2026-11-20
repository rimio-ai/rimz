# AGENTS.md

> **Invariant.** RimZ routes attention: it surfaces which agent needs you and takes you straight to its pane, where you answer in the agent's own UI.

## How to read this contract

An **invariant** is a product promise, an architectural boundary that a `cargo xtask invariants` grep enforces, or a durability rule. Each section says which of its statements are invariants. Change one deliberately, in its own commit, with the doc and the gate moving together.

**Everything else is a default.** Depart from it when the work argues otherwise, and say why in the PR or hand-off. A default that keeps losing is a bug in this file, so fix the file. When this file and the code disagree, follow the code, then fix the file in the same change.

This file is in every agent's window on every turn, so it holds only what work of every kind needs. A rule for one kind of change, or a lesson from one piece of work, goes in the contributing page that owns that kind, as a bullet in the section whose trigger it shares. [Before a change](#before-a-change) routes to it: add the trigger to that section's line there when the line does not already name it.

## Product invariants

Every bullet here is an invariant, a promise the product is built on. A change to one is a product decision, not an implementation detail.

- **Durability first.** Correctness lives in durable records, CAS rules, nonces, and per-request sockets. Wakeups and pane reads are latency, never truth. Every durable write follows the [store durability contract](./docs/internals/store.md).
- **Only agents ask.** Whatever reaches the user is an agent asking in its own pane. Automation never holds the user's attention itself, and no automation surface moves outside the two named exceptions ([DESIGN.md](./DESIGN.md#invariants)).
- **Automation is accountable.** User-benefiting automation appends durable assist records and surfaces in `rimz stats`; internal repairs keep durable diagnostic records ([loops.md](./docs/internals/harness/loops.md#the-assist-log), [diagnostics.md](./docs/internals/diagnostics.md)).
- **One interface language.** Every human-facing surface resolves color through the [shared theme core](./docs/internals/theme.md); invariants enforce the renderer boundaries.
- **Hook stdout is the decision channel.** Logs go to stderr or RimZ state logs; hook helper children get fresh stdio.
- **Cross-backend parity.** Zellij and tmux are first-class; core behaviour never depends on a backend-only feature.
- **Pane I/O is explicit.** `pane capture` and `pane send` are public primitives, and `message` routes human text through the same send path. Pane reads stay in five places: rendering, explicit `pane capture` calls, explicit `rimz agents show --capture` calls, a failed supervised run's tail capture before cleanup, and Codex turn-death confirmation. `cargo xtask invariants` admits each of the last three only behind its `rimz-invariant` marker comment.
- **Transcript origin is explicit.** RimZ-introduced text never renders as the user merely because it lacks a header ([contract and remaining launch gap](./docs/internals/harness/transcript.md#writing-entries)). `rimz pane send` is the boundary: the raw keystroke primitive carries no record by design, and its caller is the author.
- **Sidebar is read-only on the store.** Store-write modules stay out of the sidebar's import graph.
- **Trust is product behaviour.** Every command-executing config field is in the trust hash, with a test that proves it.
- **Security surfaces stay visible.** Project trust, notification handlers, hook install diffs, provider folder-trust grants, pack install summaries and update holds, and privacy settings are product behaviour.

## Engineering principles

The house style is explicit Rust, and [rust-conventions.md](./docs/contributing/rust-conventions.md) is its authority.

- `unwrap`, `expect`, and panics belong in tests, build scripts, and provably impossible states, with a comment naming why the state is impossible.
- Unsafe code is denied outside two `#[expect(unsafe_code, reason = "...")]` sites: the jemalloc configuration in `crates/rimz/src/main.rs` and the bench-only counting allocator in `crates/rimz/benches/fleet.rs`. A third site is a design decision.
- Prove an event or timing fix at the consumer's decision point. That the producer emitted, or that a timestamp is fresh, is not evidence that the consumer saw it in time.
- rustfmt leaves comments, macro calls it cannot parse, and expressions it cannot fit as written, and no gate catches them: wrap each by hand to the width of the code around it, at most 100 columns ([rust-conventions.md](./docs/contributing/rust-conventions.md#quality-gates)). A harness rule against hard-wrapping prose covers messages and Markdown, not source comments.
- A refactor you noticed on the way becomes a note unless it blocks you.
- Python helpers run under `uv`.
- `AGENTS.md` and `CLAUDE.md` are one file per directory, joined by symlink. Edit `AGENTS.md`: tools refuse to write through the `CLAUDE.md` link.
- Update the [code map](#code-map) when modules move, [ARCHITECTURE.md](./ARCHITECTURE.md) when the runtime shape changes, and [DESIGN.md](./DESIGN.md) only when a product or runtime invariant changes. A new `disk/` module also gets a row in the Disk module table of `docs/internals/store.md`.
- Leave [CHANGELOG.md](./CHANGELOG.md) untouched in a pull request: it is written as a standalone change once the work merges.
- `examples/` is sample configuration for users to copy. Do not read it unless the user asks, and do not treat it as authority on behaviour.

Two invariants sit underneath the house style:

- **Classed room paths.** Room state and runtime paths are constructed only by the disk path module; the classed-path invariant rejects callers assembling paths from room roots outside tests.
- **Fail-fast at the point of use.** The room always opens. A configuration problem (an unreadable file, an invalid value, a broken account, a plugin manifest, a remote-control blocker) warns at entry, the room runs as if that setting were absent, and the room's record keeps the warning for the sidebar and `rimz doctor`. A configured capability that cannot work fails with the fix at the command that uses it (an agent launch, a team, a subagent, a loop fire), and that command never reads a broken setting as absent where that would change the account it runs on. Best-effort is for latency and enrichment (sidebar wakeups, app-server context), never for a precondition the user switched on. The settled cases sit in the subsystem contracts, indexed under [Refusals and warnings](./docs/contributing/change-checklists.md#refusals-and-warnings).

## Before a change

Before a change of one of these kinds, read the section named beside it and do what it asks; when a change matches several lines, read each. Each section lists what such a change enumerates, greps, pins with a test, or runs, and no gate catches a skipped one.

[Change checklists](./docs/contributing/change-checklists.md):

- A stamp, clock, deadline, wake, polled state, or cached answer, or an event's power to set or clear a state: [Events and time](./docs/contributing/change-checklists.md#events-and-time).
- A default that a stored document's parser or a launch reads, a stored record's shape or fields, or what a shared parser refuses: [Defaults and stored records](./docs/contributing/change-checklists.md#defaults-and-stored-records).
- A deleted filter or guard, a predicate over a status, enum value, or count, a rule keyed on a lifecycle signal, a reading that gains a state, a lookup a new class of name reaches, or readers pointed at a wider table: [Lookups, guards, and predicates](./docs/contributing/change-checklists.md#lookups-guards-and-predicates).
- A field made mandatory on an agent row, a match that decides whether two rows are the same agent, or a caller of `address::launch_row`, `address::launch_occupants`, or the hosting lookup: [Agent rows and launch identity](./docs/contributing/change-checklists.md#agent-rows-and-launch-identity).
- A config key a user can type as a bare number, or a config command that accepts a table key: [Config keys](./docs/contributing/change-checklists.md#config-keys).
- A `rimz` child spawned to act on another room's object, or a read pointed at rooms beyond the caller's: [Rooms other than the caller's](./docs/contributing/change-checklists.md#rooms-other-than-the-callers).
- A workflow under `.github/workflows/` or `.gitea/workflows/`, or a claim about which CI jobs build a platform: [CI workflows](./docs/contributing/change-checklists.md#ci-workflows).
- A documented rule, a renamed or removed identifier, a set that gains a member, a value or line a command prints, a path RimZ reads or writes, a subprocess argv, or a command that starts to refuse: [Stale wording sweep](./docs/contributing/change-checklists.md#stale-wording-sweep).
- A workflow, a launch or run, a CLI flag or verb, human rendering, or sidebar behaviour, including its tests outside that subtree: [Workflow engine](./docs/contributing/change-checklists.md#workflow-engine), [Harness launches and runs](./docs/contributing/change-checklists.md#harness-launches-and-runs), [CLI commands and flags](./docs/contributing/change-checklists.md#cli-commands-and-flags), [Rendered output](./docs/contributing/change-checklists.md#rendered-output), [Sidebar](./docs/contributing/change-checklists.md#sidebar).
- A sentence or `console` block in a doc, rustdoc, or help text: [Doc sentences](./docs/contributing/change-checklists.md#doc-sentences), [Console blocks](./docs/contributing/change-checklists.md#console-blocks).
- When a refusal, error, or warning fires, what it says, or whether a broken setting refuses, warns, or falls back: [Refusals and warnings](./docs/contributing/change-checklists.md#refusals-and-warnings).
- What a command prints or does to durable state, when a field is set or cleared, a probe or poll path, a testkit counter, a reader bounded by a window relative to now, a shared `Arc`'s owner, a `Cargo.toml` dependency, a config template, a shell snippet another host runs, the code the parked exec wrapper runs, `harness/team_stage.rs`, the bytes a message delivery pastes into a pane, what a launch flag does to a run's pane, or what a workflow run does to its panes: [Tests a change also runs](./docs/contributing/change-checklists.md#tests-a-change-also-runs).

[Running gates and tests](./docs/contributing/running-gates.md):

- A new `pub` or `pub(crate)` item, module, or cross-module import, which fails the pre-commit hook until `refactor-target.toml` admits it, or a plan that routes a call into another module: [The surface ratchet](./docs/contributing/running-gates.md#the-surface-ratchet).
- A rebase, which runs no hook, or a branch's scope against trunk: [Rebasing](./docs/contributing/running-gates.md#rebasing).
- Naming one test or module for a run, reading test durations, or where unit tests resolve their roots: [Selecting tests](./docs/contributing/running-gates.md#selecting-tests).
- What `check` and `lint` hide (`testkit`-only calls, compiler warnings), or a remove-and-compile census: [Compile and lint signals](./docs/contributing/running-gates.md#compile-and-lint-signals).
- This worktree's built binary, a throwaway probe test, or a `cargo xtask` run from a scratch worktree: [Builds and scratch worktrees](./docs/contributing/running-gates.md#builds-and-scratch-worktrees).
- A `-P live` run, a failing test you leave unfixed, a failure to compare against trunk, or a test that behaves differently in CI: [Live tests and flakes](./docs/contributing/running-gates.md#live-tests-and-flakes).
- A gate, or a failing `rimz` command, in a Codex agent's shell: [Codex agent shells](./docs/contributing/running-gates.md#codex-agent-shells).
- `nix`, `libc`, or platform-gated code: [Cross-target checks](./docs/contributing/running-gates.md#cross-target-checks).
- A base-versus-head timing, or a change to the snapshot fold, `sidebar::enrich::enrich_core`, the room spend slice, or the spending walker's memo build: [Timing comparisons](./docs/contributing/running-gates.md#timing-comparisons).

[Hand-built probes and live checks](./docs/contributing/probes.md): read the matching section before building a tmux, Zellij, git, sshd, or provider CLI probe by hand, isolating a home, or running a branch binary against real state.

A change to any Markdown file follows [docs/AGENTS.md](./docs/AGENTS.md), which also holds the documentation map.

## Testing

The gate stack, the test tiers, and the one-home rule for a module's unit tests live in [rust-conventions.md](./docs/contributing/rust-conventions.md).

### Running gates and tests

- While iterating, run `cargo fmt --all`, then `cargo xtask check` followed by `cargo xtask lint` (`check` is singular; `checks` is the non-test gate composite). Add `cargo xtask doc` for a change that adds or edits rustdoc: neither denies rustdoc warnings.
- Run `cargo xtask gate` before a PR or hand-off: `--keep-going` reports every failure in one pass, and `--check` verifies formatting instead of applying it. On a cold or loaded tree the gate's 15-minute budget trips inside the test step, so run it there as `RIMZ_XTASK_TIMEOUT=90m cargo xtask gate`. When the test step still times out, rerun that step alone with `cargo xtask test --profile gate --no-fail-fast`.
- Run one Cargo-family command at a time in a worktree. A Cargo process from another worktree queues yours on the shared target directory, so start your command and let it queue; do not wait for that process to end.
- Start a background gate or test run from an agent pane (by a `rimz wait --run` watcher, or with a trailing `&`) as `setsid --wait <command>`; otherwise its launch and sandbox tests time out: [Background runs](./docs/contributing/running-gates.md#background-runs).
- From a room shell, run tests only through `cargo xtask test` or `cargo xtask stress`, never a bare `cargo test`, `cargo nextest run`, or a loop over `target/*/deps/` binaries: the `cli/` unit tests and the integration binary resolve the shell's `RIMZ_HOME`/`HOME`/`XDG_*`.
- Run no `cargo xtask stress` batch, mass repeat, or parallel copies of a test, on any test in any crate, unless the user asks or allows it: the host is shared. [Live tests and flakes](./docs/contributing/running-gates.md#live-tests-and-flakes) says how to triage a flake without that leave.
- Escalate past `gate` to the journey (`cargo xtask test -P journey`), live-backend and `journey::deep` (`cargo xtask test -P live`), performance (`cargo xtask perf`), or dependency (`cargo xtask externals`) gates, or to full CI (`cargo xtask ci`), when the change touches those surfaces, their fixtures, or shared infrastructure. A test's profile is set by the filters in `.config/nextest.toml`, not by its directory: get the exact name from `cargo xtask test --list <fragment>` and match it against those filters.
- **Invariant.** `cargo xtask invariants` greps tracked text, Rust and Markdown alike. An `AGENTS.md` inside a guarded subtree is scanned with it: describe a boundary in prose and leave the banned path unwritten. A new untracked file escapes both `invariants` and `gate` until the pre-commit hook rejects it, so stage new files with `git add -N` before running either.

### Writing tests

- An inline `mod tests` over 500 lines fails the pre-commit invariant; move the whole module to a sibling `tests.rs`, in the same change when an edit leaves it within about 20 lines of the cap.
- Capture planned behaviour in the owning doc and add the executable test when the implementation can make it pass. An ignored test standing in for a future product target does not land.
- Before writing or debugging an integration test, read the section of [integration-tests.md](./docs/contributing/integration-tests.md) for what it touches.
- A change that deletes a test lists the inputs it covered and carries each into a surviving test, or names the dropped input in the hand-off.

## Code map

Runtime shape and the single-binary rationale live in [ARCHITECTURE.md](./ARCHITECTURE.md); each module's `//!` header is the per-file authority.

**Repository**: `crates/rimz/` (the binary and its runtime/domain library, plus `benches/`, `scenarios/`, and the `presence/`, `pricing/`, `themes/`, and `keymaps/` data it embeds), `crates/rimz-presence-zellij/` (Zellij presence plugin, wasm32-wasip1, no rimz-crate deps), `docs/` (`docs/externals/` mirrors upstream), `xtask/` (task runner, every gate, contributor automation), [`loops/`](./loops/README.md) (one directory per repository loop task), `examples/`, `ci/`, `docker/`, `scripts/`, `supply-chain/`.

**Subsystems** in `crates/rimz/src/`, each carrying its own `AGENTS.md` contract:

- `cli/`: command parsing, one `run(...)` per subcommand, shared `cli/render/` output. Declared in `main.rs`, it calls the library as `rimz::` and reaches only `pub` items.
- `agents/`: the provider-neutral `AgentDefinition` catalog, capability contracts, `state.rs` rollup, handle minting (`petname`), private `adapters/`, spend/pricing/account machinery, provider login resolution (`login`).
- `room/`: managed-room context, birth/reset/teardown lifecycle, health gating.
- `sandbox/`: Linux bubblewrap mount views, provider-home binds, per-agent temp units, skill views, capability preflight.
- `harness/`: layout IR, teams and board stage flips, launch plan and argv, supervised runs, workflows (`workflow/`), the hook drainer (`hook_drain.rs`), loop scheduling and signals (`schedule/fire.rs` and the read-only `when.rs` evaluator are in the sidebar graph), resume and rebirth recovery, hibernation (`hibernate.rs`, `hibernate/`, with `proc/cgroup.rs`), the note store (`notes.rs`).
- `message/`: delivery: park-vs-live dispatch, live-pane send, the shared turn reader, scheduled wakeups, synthetic card text.
- `store/`: durable state engine: `Store` handle, snapshot schema, framed event log, hook ingress and drain cursor (`ingress.rs`), message queue, run and workflow records, GC.
- `mux/`: Zellij/tmux seam: `MuxBackend`, live-session snapshot, subprocess engine, reconcile planner, recovery, focus anchor, per-pane write lock.
- `sidebar/`: data plane: presence ingestion, host production, pulled-truth/event fusion, projection fold, heavy-lane refresh.
- `sidebar_pane/`: native renderer: per-pane supervisor, per-session host, attach wire, pets, ratatui edge.
- `remote/`: SSH grammar, reconnect policy, link health, `remote.toml`.
- `diag/`: diagnostic-only JSONL append surfaces.

**Top-level modules** in `crates/rimz/src/`:

- `launcher/`: launch orchestration and checkout creation.
- `disk`: atomic publish, home and workspace paths, JSONL append, locks and leases (`disk::lease`), usage and output measurement.
- `address`: agent-address grammar, pane binding, launch-instance grouping, and `address::selector` (the `agents list` fields and `--filter`). `workspace`: resolution, `workspace.json`, and the room identity pin. `web`, `worktree`, `forge` (with PR state).
- `transcript`, `pane` (identity and classification), `pixel_wire` (kitty placeholder encoding), `sock`, `ids`, `trust`, `wakeup` (the sidebar wire).
- `utils::{time, path, parallel, tokens, env}` (`utils::time` holds the crate's one Unix-millisecond clock), `fs_watch`, `theme`.
- Daemon view: `daemon_view`, `daemon_content`, `reload`, `remote_control`.
- `config`, `observability`, `agent_activity`, `lane`, `proc`, `osc`, `build_id`, `child_process`, `tui`, `testkit`.
- `blocker` (start preconditions; the CLI owns their presentation), `update` and `uninstall` (install lifecycle), `packs/` (pack sources; definition and skill readers own item scanning), `lsp/` (shared language servers), `mcp/` (the `rimz mcp serve` tool catalog and stdio loop).
