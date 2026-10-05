# Agent mount views

Local contract for `crates/rimz/src/sandbox/` — opt-in agent filesystem views. Extends [crates/rimz/AGENTS.md](../../AGENTS.md); behaviour lives in [sandbox.md](../../../../docs/internals/sandbox.md).

## Boundaries

- Sandbox owns bubblewrap admission, ordered mount plans, profile skill views, and the environment pins those plans require. It provides a view, not containment.
- `TmpView` owns the one mapping between an agent's temp unit and what the agent sees: under sandbox isolation the unit is `/tmp` (and `/var/tmp` maps back to it); paths stay host paths otherwise. It is built at the launch compile site and for a caller's own paths (`cli::caller_host_path`), never to print a result path. The host state path remains accessible; a temp unit is separate from host `/tmp`, not hidden from the host.
- Provider home resolution and native override keys belong to the agent capability seam; multiplexer endpoint resolution belongs to the mux domain. Do not duplicate either resolver here.
- Skill roots and the user-only marker come from the adapter (`skills_home`, `manual_skill`); rewritten copies live under the handle's owned unit, or under the room cache for handleless launches (`StatePaths::agent_skills_dir`). Preserve provider-root symlinks and non-skill entries; shadow rewritten skills at canonical paths reached through preserved symlinks, including outside the root, and refuse conflicting alias policies.
- An unlisted skill the view cannot prepare is omitted, never bound unrewritten. Each omission is a returned warning (`SkippedSkill` on `SandboxPlan`) that the exec handler prints; failures listing the skill root or writing under `skills_dir` remain errors.
- `plan` reads skill sources and captures rewritten bytes, mount targets, and environment pins without creating files or directories. `apply` writes the captured copies and ensures the temp unit; a caller that needs both runs them in that order.
- Launch compilation applies environment pins after shell startup; CLI execution retains the probed wrapper path and routes preparation errors through launch/run failure cleanup.
- `sandbox::plan` binds the launch's temp unit (`StatePaths::temp_unit_dir`, owner resolved by `launch_plan::compile`) at `/tmp` and `/var/tmp`, refuses a required path equal to either mount (`TmpCollision`), and rebinds required paths beneath them. `StatePaths::ensure_temp_unit` creates the unit and the room's `shared/`, both `0700`, whose host path `RIMZ_SHARED` names in both isolations; teardown leaves both for GC, a hard reset ends the processes that still have a unit as their `/tmp` before it deletes `tmp/`, and RimZ's result files live in the state `out/` class instead.
- Unit tests cover pure parsing and argv lowering. Filesystem and real bubblewrap tests live in `tests/integration/sandbox.rs`; room messaging and supervised handoff belong to the journey tier.
