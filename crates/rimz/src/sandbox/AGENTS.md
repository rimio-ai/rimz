# Agent mount views

Local contract for `crates/rimz/src/sandbox/` — opt-in agent filesystem views. Extends [crates/rimz/AGENTS.md](../../AGENTS.md); behaviour lives in [sandbox.md](../../../../docs/internals/sandbox.md).

## Boundaries

- Sandbox owns bubblewrap admission, ordered mount plans, profile skill views, and the environment pins those plans require. It provides a view, not containment.
- `TmpView` owns host-to-agent output path mapping: under sandbox isolation the owner's scratch dir becomes `/tmp/scratchpad/…` and other room tmp paths become `/tmp/…`; they stay host paths otherwise. The host state path remains accessible; room tmp is separate from host `/tmp`, not hidden from the host.
- Provider home resolution and native override keys belong to the agent capability seam; multiplexer endpoint resolution belongs to the mux domain. Do not duplicate either resolver here.
- Skill roots and the user-only marker come from the adapter (`skills_home`, `manual_skill`); rewritten copies live beside the handle's scratch under its owned unit, or under the room cache for handleless launches (`StatePaths::agent_skills_dir`). Preserve provider-root symlinks and non-skill entries; shadow rewritten skills at canonical paths reached through preserved symlinks, including outside the root, and refuse conflicting alias policies.
- An unlisted skill the view cannot prepare is omitted, never bound unrewritten. Each omission is a returned warning (`SkippedSkill` on `SandboxPlan`) that the exec handler prints; failures listing the skill root or writing under `skills_dir` remain errors.
- `plan` reads skill sources and captures rewritten bytes, mount targets, and environment pins without creating files or directories. `apply` writes the captured copies and ensures room tmp; `prepare` is the combined entry for callers that need both.
- Launch compilation applies environment pins after shell startup; CLI execution retains the probed wrapper path and routes preparation errors through launch/run failure cleanup.
- `StatePaths::ensure_tmp_dir` builds the room tmp layout (`agents/`, `scratchpad/`, `shared/`, `rimz-waits/`, `rimz-subagents/`); `StatePaths::scratch_dir` is the one derivation of a launch's private scratch dir, which `sandbox::plan` binds at `/tmp/scratchpad`, and `StatePaths.shared_dir` the one derivation behind `RIMZ_SHARED` and `/tmp/shared`. Room lifecycle ensures it at sandbox birth and removes it after process teardown; sandbox launch preparation ensures it again, and output writers create it on demand in either isolation mode.
- Unit tests cover pure parsing and argv lowering. Filesystem and real bubblewrap tests live in `tests/integration/sandbox.rs`; room messaging and supervised handoff belong to the journey tier.
