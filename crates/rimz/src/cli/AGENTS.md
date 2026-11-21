# CLI layer

Local contract for `crates/rimz/src/cli/`: argv to typed requests, the process edge (tty, signals, exit codes, stdout and stderr), and presentation. Extends [crates/rimz/AGENTS.md](../../AGENTS.md).

## What a handler owns

- A handler parses, calls the domain, and presents. A multi-step workflow with decisions, retries, settlement, or recovery belongs to the library module that owns the concept; a straight-line sequence of library calls stays here. Handlers that orchestrate today (`rimz start`, gc sweeps) are debt for rehome passes, not a shape to copy.
- Domain code reports warnings as returned values and the handler prints them: the CLI owns every write to stdout and stderr.
- Concrete typed structs and functions are the default shape here. A generic service trait earns its place once a second real caller needs the seam, never in anticipation of one.

## Presentation

- Human colors come from `render::palette` accessors, state tones from typed `render::status` helpers, and provider names and handles from `palette::identity`. Agent prose reaches a human through `render::prose::Prose`: markdown on a styled stdout and raw text when piped; one-line previews stay snippets. JSON, hook stdout, pane capture, scripting values, and streaming protocols stay raw. Which `println!` sites are legal is in [Stdout and tracing](../../../../docs/contributing/rust-conventions.md#stdout-and-tracing).

## Stores

- Participants open through `open_existing_store`, directly or through `Ctx`, and refuse when the room has no readable workspace record. Identity-only and read-only maintenance commands may tolerate an absent room; none creates its state tree or re-records its identity.
- **Invariant.** Only room-choosing commands create stores: `start`, `attach`, `reset`, and `workspace migrate`; the sessions picker and `web` reuse room preparation, and library birth, rebirth, and the launcher's supervised preparation (including a spawning `loop fire`) and checkout creation own their creation. `cargo xtask invariants` (`ensure_room_creators`) rejects a creating store open in any file outside its closed creator list; extending that list is a deliberate decision.

## Command boundaries

- A command module's internals are private: one command never imports another command's functions or types. Shared logic moves to the domain module that owns the knowledge, or, when it is pure presentation, to a shared CLI-layer module: `ctx`, `render/`, `usage`, `send`, `address`, `spinner`, `wake_view`, `workflow_view`, `ask_commands`, `launch_interaction`, `worktree_entry`, `worktree_protection`, `profile_report`, `loop_timer`, and the target-resolution helpers in `mod.rs`.
- One parse entry is admitted across commands: `loop_launch.rs`, through which `rimz loop add` hands a task's parsed launch tail to the `check_loop_launch` of the command it names.
- The orchestration entries one command already publishes to another are migration debt: `room` attach execution (used by `remote`), `hooks` install UX (used by `room` start and `setup`), the `supervised` run driver (used by `agents_cmd`, `subagents`, and `loop_cmd`), `agents_cmd` resolved lifecycle operations and resume preconditions (used by `teams`, `subagents`, `workflow`, and `loop_cmd`), and the `subagents` parent-only child resume doorway (used by `message`). Add no other. Rehome passes move these workflows to their library owners; rendering stays here, and `supervised` keeps publishing run rendering (used by `subagents`) as a sanctioned cross-command entry.

## Before a CLI change

Before a change of one of these kinds, read the section named beside it and do what it asks. No gate catches a skipped one.

- A clap parser shape, a free-text positional, a mode that refuses flags another mode accepts, a `value_parser` refusal, a command that starts refusing or asking where it proceeded, or a flag renamed or removed: [CLI shape](../../../../docs/contributing/rust-conventions.md#cli-shape).
- A clap command or flag, or a top-level command's one-line help: [Snapshot tests](../../../../docs/contributing/rust-conventions.md#snapshot-tests).
- Human rendering, a `doctor` row, a finding or notice built from a template, or a command a view prints for the user to copy: [Changing rendered output](../../../../docs/contributing/cli-scenes.md#changing-rendered-output).
- A screen that prints stored records as rows or blocks (a history, recent runs, logs, a timeline): [Human history lists](../../../../docs/contributing/cli-scenes.md#human-history-lists).
- A `rimz agents`, `rimz teams`, or `rimz subagents` list, verb, exit code, or user-facing noun: [CLI scenes](../../../../docs/contributing/cli-scenes.md).
