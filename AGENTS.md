# AGENTS.md

> **Invariant.** RimZ routes attention: it surfaces which agent needs you and takes you straight to its pane, where you answer in the agent's own UI.

## How to read this contract

An **invariant** is a product promise, an architectural boundary that a `cargo xtask invariants` grep enforces, or a durability rule, and each one is marked. Change one deliberately, in its own commit, with the doc and the gate moving together. Everything else is a default: depart from it when the work argues otherwise, and say why in the PR or hand-off. When this file and the code disagree, follow the code, then fix the file in the same change.

Every `AGENTS.md` is in its reader's window on every turn, so each holds only what all work under its directory needs, and this root only what work of every kind needs. A trap tied to one function or type goes in that item's doc comment, where whoever edits it reads it. A rule for changes inside one tree goes in that tree's `AGENTS.md`. A rule for one kind of change goes in the contributing page that owns that kind, as a bullet in the section whose trigger it shares, and the `AGENTS.md` of the tree where such a change is made routes to that section.

## Product invariants

Every bullet here is an invariant, a promise the product is built on. A change to one is a product decision, not an implementation detail.

- **Durability first.** Correctness lives in durable records, CAS rules, nonces, and per-request sockets. Wakeups and pane reads are latency, never truth. Every durable write follows the [store durability contract](./docs/internals/store.md).
- **Only agents ask.** Whatever reaches the user is an agent asking in its own pane. Automation never holds the user's attention itself, and no automation surface moves outside the two named exceptions ([DESIGN.md](./DESIGN.md#invariants)).
- **Automation is accountable.** User-benefiting automation appends durable assist records and surfaces in `rimz stats`; internal repairs keep durable diagnostic records ([loops.md](./docs/internals/harness/loops.md#the-assist-log), [diagnostics.md](./docs/internals/diagnostics.md)).
- **One interface language.** Every human-facing surface resolves color through the [shared theme core](./docs/internals/theme.md); invariants enforce the renderer boundaries.
- **Hook stdout is the decision channel.** Logs go to stderr or RimZ state logs; hook helper children get fresh stdio.
- **Cross-backend parity.** Zellij and tmux are first-class; core behaviour never depends on a backend-only feature.
- **Pane I/O is explicit.** `pane capture` and `pane send` are public primitives, and `message` routes human text through the same send path. Pane reads stay in five places, which `cargo xtask invariants` admits: rendering, explicit `pane capture` calls, explicit `rimz agents show --capture` calls, a failed supervised run's tail capture before cleanup, and Codex turn-death confirmation.
- **Transcript origin is explicit.** RimZ-introduced text never renders as the user merely because it lacks a header ([contract and remaining launch gap](./docs/internals/harness/transcript.md#writing-entries)). `rimz pane send` is the boundary: the raw keystroke primitive carries no record by design, and its caller is the author.
- **Sidebar is read-only on the store.** Store-write modules stay out of the sidebar's import graph.
- **Trust is product behaviour.** Every command-executing config field is in the trust hash, with a test that proves it.
- **Security surfaces stay visible.** Project trust, notification handlers, hook install diffs, provider folder-trust grants, pack install summaries and update holds, and privacy settings are product behaviour.

## Engineering principles

The house style is explicit Rust, and [rust-conventions.md](./docs/contributing/rust-conventions.md) is its authority.

- `unwrap`, `expect`, and panics belong in tests, build scripts, and provably impossible states, with a comment naming why the state is impossible.
- Unsafe code is denied outside two `#[expect(unsafe_code, reason = "...")]` sites: the jemalloc configuration in `crates/rimz/src/main.rs` and the bench-only counting allocator in `crates/rimz/benches/fleet.rs`. A third site is a design decision.
- rustfmt leaves comments, macro calls it cannot parse, and expressions it cannot fit as written, and no gate catches them: wrap each by hand to the width of the code around it, at most 100 columns ([rust-conventions.md](./docs/contributing/rust-conventions.md#quality-gates)). A harness rule against hard-wrapping prose covers messages and Markdown, not source comments.
- A refactor you noticed on the way becomes a note unless it blocks you.
- Python helpers run under `uv`.
- `AGENTS.md` and `CLAUDE.md` are one file per directory, joined by symlink. Edit `AGENTS.md`, since tools refuse to write through the `CLAUDE.md` link, and give a new `AGENTS.md` its `CLAUDE.md` link.
- Update the [code map](#code-map) when modules move, [ARCHITECTURE.md](./ARCHITECTURE.md) when the runtime shape changes, and [DESIGN.md](./DESIGN.md) only when a product or runtime invariant changes. A new `disk/` module also gets a row in the Disk module table of `docs/internals/store.md`.
- Leave [CHANGELOG.md](./CHANGELOG.md) untouched in a pull request: it is written as a standalone change once the work merges.
- `examples/` is sample configuration for users to copy. Do not read it unless the user asks, and do not treat it as authority on behaviour.
- A change to any Markdown file follows [docs/AGENTS.md](./docs/AGENTS.md).

Two invariants sit underneath the house style:

- **Classed room paths.** Room state and runtime paths are constructed only by the disk path module; the classed-path invariant rejects callers assembling paths from room roots outside tests.
- **Fail-fast at the point of use.** The room always opens. A configuration problem (an unreadable file, an invalid value, a broken account, a plugin manifest, a remote-control blocker) warns at entry, the room runs as if that setting were absent, and the room's record keeps the warning for the sidebar and `rimz doctor`. A configured capability that cannot work fails with the fix at the command that uses it (an agent launch, a team, a subagent, a loop fire), and that command never reads a broken setting as absent where that would change the account it runs on. Best-effort is for latency and enrichment (sidebar wakeups, app-server context), never for a precondition the user switched on. The settled cases sit in the subsystem contracts as `Fail-fast, settled` bullets.

## Before a change

These hold for a change anywhere in the tree, and no gate catches a skipped one.

- **Rule on every writer and reader.** Before you change a predicate, filter, guard, lookup, or default, or when a field is set or cleared, list every site that writes the value and every site that reads it (`rimz lsp refs`, `rimz lsp callers`), counting the rows other paths synthesize and every launch path a launch default reaches, and rule what each does under the new rule.
- **One agent is one launch id.** A match that decides whether two rows are the same agent compares launch ids, never `(kind, session id)`.
- **Prove timing at the consumer.** An event or timing fix is proven at the consumer's decision point: that the producer emitted, or that a timestamp is fresh, is not evidence that the consumer saw it in time. Work placed between a decision and the action it gates makes the decision stale, so read it again after the work.
- **Sweep the sentences you made false.** A changed rule, a renamed identifier, a set that gains a member, a changed output, path, argv, or refusal: before hand-off, grep the old words, by stem rather than phrase, across `docs/`, every `AGENTS.md`, rustdoc and comments, tests, and the `refactor-target.toml` verdicts, and fix each hit.
- **Run the tests that name what you changed.** A test outside the modules you edited can pin the old behaviour: grep `crates/rimz/src` and `crates/rimz/tests` for the field, command, or message you changed and run each module that names it.
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
- A rebase runs no hook: before you rebase, read [Rebasing](./docs/contributing/running-gates.md#rebasing). [running-gates.md](./docs/contributing/running-gates.md) has one section for each other trap of the runner: naming a test, what `check` and `lint` hide, the surface ratchet (also read before a plan routes a call into another module), this worktree's binary and scratch worktrees, live runs and flakes, a gate in a Codex agent's shell, cross-target checks, and timing comparisons.
- Before building a tmux, Zellij, git, sshd, or provider CLI probe by hand, isolating a home, or running a branch binary against real state, read the matching section of [probes.md](./docs/contributing/probes.md).
- **Invariant.** `cargo xtask invariants` greps tracked text, Rust and Markdown alike. An `AGENTS.md` inside a guarded subtree is scanned with it: describe a boundary in prose and leave the banned path unwritten. A new untracked file escapes both `invariants` and `gate` until the pre-commit hook rejects it, so stage new files with `git add -N` before running either.

### Writing tests

- An inline `mod tests` over 500 lines fails the pre-commit invariant; move the whole module to a sibling `tests.rs`, in the same change when an edit leaves it within about 20 lines of the cap.
- Capture planned behaviour in the owning doc and add the executable test when the implementation can make it pass. An ignored test standing in for a future product target does not land.
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
- `config/`: machine settings, profile and team definitions, `rimz config` editing, and the config templates.

**Top-level modules** in `crates/rimz/src/`:

- `launcher/`: launch orchestration and checkout creation.
- `disk`: atomic publish, home and workspace paths, JSONL append, locks and leases (`disk::lease`), usage and output measurement.
- `address`: agent-address grammar, pane binding, launch-instance grouping, and `address::selector` (the `agents list` fields and `--filter`). `workspace`: resolution, `workspace.json`, and the room identity pin. `web`, `worktree`, `forge` (with PR state).
- `transcript`, `pane` (identity and classification), `pixel_wire` (kitty placeholder encoding), `sock`, `ids`, `trust`, `wakeup` (the sidebar wire).
- `utils::{time, path, parallel, tokens, env}` (`utils::time` holds the crate's one Unix-millisecond clock), `fs_watch`, `theme`.
- Daemon view: `daemon_view`, `daemon_content`, `reload`, `remote_control`.
- `observability`, `agent_activity`, `lane`, `proc`, `osc`, `build_id`, `child_process`, `tui`, `testkit`.
- `blocker` (start preconditions; the CLI owns their presentation), `update` and `uninstall` (install lifecycle), `packs/` (pack sources; definition and skill readers own item scanning), `lsp/` (shared language servers), `mcp/` (the `rimz mcp serve` tool catalog and stdio loop).
