# Agent mount views

Local contract for `crates/rimz/src/sandbox/` — opt-in agent filesystem views. Extends [crates/rimz/AGENTS.md](../../AGENTS.md); behaviour lives in [sandbox.md](../../../../docs/internals/sandbox.md).

## Boundaries

- Sandbox owns bubblewrap admission, ordered mount plans, profile skill views, and the environment pins those plans require. It provides a view, not containment.
- Provider home resolution and native override keys belong to the agent capability seam; multiplexer endpoint resolution belongs to the mux domain. Do not duplicate either resolver here.
- Skill roots and the user-only marker come from the adapter (`skills_home`, `manual_skill`); rewritten copies live under `StatePaths.skills_dir`, owned by room lifecycle like `tmp/`.
- Launch compilation applies environment pins after shell startup; CLI execution retains the probed wrapper path and routes preparation errors through launch/run failure cleanup.
- Room lifecycle owns tmp creation at birth and removal after process teardown; launch preparation only ensures the directory exists.
- Unit tests cover pure parsing and argv lowering. Filesystem and real bubblewrap tests live in `tests/integration/sandbox.rs`; room messaging and supervised handoff belong to the journey tier.
