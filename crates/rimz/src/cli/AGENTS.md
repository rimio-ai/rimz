# CLI layer

Local contract for `crates/rimz/src/cli/`: argv to typed requests, the process edge (tty, signals, exit codes, stdout/stderr discipline), and presentation (view models and renderers). Extends [crates/rimz/AGENTS.md](../../AGENTS.md).

## What lives here

- A free-text positional (a note, text, prompt, instruction, or message body) sets `allow_hyphen_values = true`; its reference page names `--` as the escape for a value that is exactly one of the command's flags, `-h`/`--help` included. Exception: agent and subagent launches reserve `--` for provider passthrough; an interactive agent launch cannot take such a prompt, a print run can read it from `agents -p --stdin`, and subagents can use `--prompt-file`. Launch prompts guard prefixes matching value-taking short flags so attached values cannot silently become prompts; document the spaced option spelling and the guard's application after `--`.
- clap argument types, stdin/file parsing, workspace lookup at the entry point, interactive prompts, stdout/stderr presentation, exit codes. A new, removed, or renamed flag updates the `cli_surface_is_stable` snapshot in the same commit: `cargo xtask test --name cli::surface_tests::cli_surface_is_stable`. Accept the change with `cargo insta accept`; never copy the `.snap.new` over the snapshot, since its header differs.
- Existing cross-command orchestration combining room, mux, store, agent, and message operations (`rimz start`, supervised runs, gc sweeps) is debt for rehome passes, not the target. Multi-step workflows with decisions, retries, settlement, or recovery belong to the library module owning the concept; straight-line sequences of library calls stay here.
- `wait/` is the self-only face of the scheduler: it resolves the calling live agent for delay and command waits and presents receipts, pending lists, and caller-owned cancellation. It and `loop_cmd/` call `harness::schedule::arm` for delivery construction, signal defaults and guards, dedupe, and detached watcher spawning; neither command imports the other. The shared resolver supports `@me`, including bare `loop add --wait` ([loops.md](../../../../docs/internals/harness/loops.md#waits)).
- Human and JSON rendering. Large render surfaces stay here: doctor, stats panels, transcript, pane, loop, and gc reports.
- Shared CLI-layer modules serve every command: `ctx` (the participant entry: workspace, store, channel, and the snapshot flavours), `render/` (output streams, a process-lazy machine theme, and typed state presentation), `spinner`, `usage` (the usage error `main` exits 2 for, and the no-verb scene refusal), `send` (shared send flags and outcome presentation), `ask_commands` (the next-step, refusal, and receipt text `asks` and `answer` share), `address`, `profile_report` (profile/command catalog presentation), `loop_timer` (external loop tick and OS timer lifecycle shared with uninstall), `worktree_protection` (runtime pane and agent fact gathering for removal callers), and the target-resolution helpers in `mod.rs`.
- Participants open through `open_existing_store`, directly or through `Ctx`, and refuse when the room has no readable workspace record. Identity-only and read-only maintenance commands may tolerate an absent room; none creates its state tree or re-records its identity. Identity resolution through `WorkspaceResolver::resolve_participant` remains independent of whether the room exists.
- **Invariant.** Only room-choosing commands create stores: `start`, `attach`, `reset`, `workspace migrate`, supervised launch preparation (including a spawning `loop fire`), and launch checkout creation; the sessions picker and `web` reuse room preparation, and library birth and rebirth own their creation. `open_store` creates the state tree and records the workspace only for those commands. `ensure_room_creators` grep-enforces the closed creator file list; extending it is a deliberate decision.

## What lives in the domain modules

A handler parses, calls the domain, and presents; the knowledge lives in its owning module:

- `harness` — launch compilation and validation vocabulary, placement, resume/rebirth planning, schedule policy, run waits.
- `message` — dispatch conditions, delivery causality, reply-wait state.
- `room` — room identity (session→mux resolution, the single-backend guard, session→workspace-record lookup), private room context, the one birth path.
- `config` — format-preserving config editing and bootstrap.
- `store` — event construction, rotation policy, pane/session binding eligibility.
- `sidebar` — presence ingestion, topology fencing, cache publication.
- `worktree` — clean linear landing, sweep assessment, protection policy, and lifecycle cleanup.
- `agents` — provider argv vocabulary and per-kind context policy (field ownership, merge rules).

## Boundaries

- The resident loop helper reuses `agents_cmd::stop_resolved` before launching. The harness selects the launch checkout's occupants and what blocks a takeover (`schedule::takeover`); the helper gathers the snapshot and owned worktrees, stops, and presents.
- Human colors come from `render::palette` accessors, state tones come from typed `render::status` helpers, and provider names and handles come from `palette::identity`. Agent prose reaches a human through `render::prose::Prose`: markdown on a styled stdout and raw text when piped; one-line previews stay snippets. JSON, hook stdout, pane capture, scripting values, and streaming protocols stay raw.
- The CLI is compiled into the binary crate (`main.rs` owns its module) and reaches library items only through `rimz::`, so any item a handler calls must be `pub` in the library; `pub(crate)` is invisible here. A new `pub` item needs its `refactor-target.toml` admission (root [Testing](../../../../AGENTS.md#testing)).
- A command module's internals are private: one command never imports another command's functions or types. Shared logic moves to the domain module that owns the knowledge, or to a shared CLI-layer module when it is pure presentation.
- Existing published orchestration entries are migration debt: `room` attach execution (used by `remote`), `hooks` install UX (used by `room` start and `setup`), `supervised` run driver and pane placement (used by `agents_cmd`, `subagents`, and `loop_cmd`), `agents_cmd` resolved lifecycle operations and resume preconditions (used by `teams` and `subagents`) and its `--detach` policy mapping `report_to` (used by `subagents`), and `subagents` parent-only child resume doorway (used by `message`). The resident-loop exception is `agents_cmd::launch::launch_resolved`: it accepts an already-resolved host cwd and returns identities without printing a receipt; ordinary agent launches retain their CLI cwd resolution and receipt. Add no other published orchestration entries. Rehome passes move workflows to their library owners; rendering stays here, and `supervised` keeps publishing run rendering (used by `subagents`) as a sanctioned cross-command entry.
- Concrete typed structs and functions are the default shape here. A generic service trait earns its place once a second real caller needs the seam, never in anticipation of one.
- Domain code reports warnings as returned values and the handler prints them; the CLI owns every write to stdout and stderr.
