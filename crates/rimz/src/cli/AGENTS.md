# CLI layer

Local contract for `crates/rimz/src/cli/` — command parsing, presentation, and cross-command orchestration. Extends [crates/rimz/AGENTS.md](../../AGENTS.md); it never restates parent rules.

## What lives here

- clap argument types, stdin/file parsing, workspace lookup at the entry point, interactive prompts, stdout/stderr presentation, exit codes.
- Cross-command orchestration where one user action intentionally combines room, mux, store, agent, and message operations — `rimz start`, supervised runs, gc sweeps.
- Human and JSON rendering. Large render surfaces stay here: doctor, stats panels, transcript, pane, loop, and gc reports.
- Shared CLI-layer modules serve every command: `render/` (output streams, a process-lazy machine theme, and typed state presentation), `spinner`, `send` (shared send flags and outcome presentation), `address`, and the target-resolution helpers in `mod.rs`.

## What lives in the domain modules

A handler parses, calls the domain, and presents; the knowledge lives in its owning module:

- `harness` — launch compilation and validation vocabulary, placement, resume/rebirth planning, schedule policy, run waits.
- `message` — dispatch conditions, delivery causality, reply-wait state.
- `room` — room identity (session→mux resolution, the single-backend guard, session→workspace-record lookup), private room context, the one birth path.
- `config` — format-preserving config editing and bootstrap.
- `store` — event construction, rotation policy, pane/session binding eligibility.
- `sidebar` — presence ingestion, topology fencing, cache publication.
- `worktree` — removal assessment, protection rules, lifecycle cleanup.
- `agents` — provider argv vocabulary and per-kind context policy (field ownership, merge rules).

## Boundaries

- Human colors come from `render::palette` accessors, state tones come from typed `render::status` helpers, and provider names and handles come from `palette::identity`. JSON, hook stdout, pane capture, scripting values, and streaming protocols stay raw.
- A command module's internals are private: one command never imports another command's functions or types. Shared logic moves to the domain module that owns the knowledge, or to a shared CLI-layer module when it is pure presentation.
- A command may publish an orchestration entry that other commands call as its owning surface: `room` publishes attach execution (used by `remote`), `hooks` publishes the install UX (used by `room` start and `setup`), `supervised` publishes the run driver and run rendering (used by `agents_cmd` and `loop_cmd`).
- Concrete typed structs and functions suffice; generic service traits and ports stay out.
- Domain code reports warnings as returned values and the handler prints them; the CLI owns every write to stdout and stderr.
