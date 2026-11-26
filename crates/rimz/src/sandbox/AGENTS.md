# Agent mount views

Local contract for `crates/rimz/src/sandbox/`. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). Behaviour lives in [sandbox.md](../../../../docs/internals/sandbox.md): preflight, mount order, temp units, the SSH-config mirror, and skill views.

- The scoped view is not containment. Do not present a mount as a security boundary, in code or docs.
- Provider home resolution and native override keys belong to the agent capability seam, and multiplexer endpoint resolution to `mux`. Do not duplicate either resolver here.
- `plan` creates no file or directory, so `rimz agents explain` can run it alone. Every write goes in `apply`, and a caller that needs both runs `plan` first.
- `TmpView` is the one mapping between a temp unit and the `/tmp` an agent sees. A path RimZ reports to a reader is never spelled through it; the one path spelled through it is a subagent's own result file, in that subagent's launch text.
- An unlisted skill the view cannot prepare is omitted and returned as a `SkippedSkill` warning, never bound unrewritten. A failure listing the skill root or writing under `skills_dir` stays an error.
- A single-file bind detaches in every running view when the host renames a new file over its destination, which is how package managers and most config tools write. The live guard re-binds only the protected identity entries. For any other mount, bind a directory or a file that is rarely replaced, and say which in sandbox.md.
- Real-bubblewrap tests self-skip in CI, so give a property of the view a bubblewrap-free test of `plan` and `apply` as well.
- A change to which paths a mount plan binds tests each entry kind in `tests/integration/sandbox.rs`: a plain file, a directory, a symlink, a missing path, a `.git` file pointing elsewhere, and a linked worktree. An SSH-config mirror change covers a regular file, a symlink, a dangling symlink, a directory, a FIFO, and a missing or unreadable source.
- Fail-fast, settled: Profile `skills` lists apply at every isolation level, via the scoped view or the provider's host-mode switch. Host providers without a switch warn and run unrestricted.
- Fail-fast, settled: The `sandbox` isolation level is flag mapping, owned by the adapters: RimZ adds each provider's own sandbox flags and owns nothing past them. Admission at that level probes nothing (no PATH check, no namespace probe), and a provider without a switch warns and launches. Do not add a check here that the provider itself makes or skips.
- Fail-fast, settled: a named exception to the root rule. Off Linux, where no scoped view exists, a configured `scoped` isolation falls back to host silently, shown only as information in `rimz doctor`'s SANDBOX section, while an explicit `--isolation scoped` still refuses.
