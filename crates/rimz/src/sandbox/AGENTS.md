# Agent mount views

Local contract for `crates/rimz/src/sandbox/`. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). Behaviour lives in [sandbox.md](../../../../docs/internals/sandbox.md): preflight, mount order, temp units, the SSH-config mirror, and skill views.

- The scoped view is not containment. Do not present a mount as a security boundary, in code or docs.
- Provider home resolution and native override keys belong to the agent capability seam, and multiplexer endpoint resolution to `mux`. Do not duplicate either resolver here.
- `plan` creates no file or directory, so `rimz agents explain` can run it alone. Every write goes in `apply`, and a caller that needs both runs `plan` first.
- Read [Protected paths](../../../../docs/internals/sandbox.md#protected-paths) before you add a mount, and [Tests](../../../../docs/internals/sandbox.md#tests) before you change which paths a view binds or what the SSH-config mirror copies.
- Fail-fast, settled: Profile `skills` lists apply at every isolation level, via the scoped view or the provider's host-mode switch. Host providers without a switch warn and run unrestricted, except that a listed name found only in a project folder the provider does not read refuses the launch.
- Fail-fast, settled: The `sandbox` isolation level is flag mapping, owned by the adapters: RimZ adds each provider's own sandbox flags and owns nothing past them. Admission at that level probes nothing (no PATH check, no namespace probe), and a provider without a switch warns and launches. Do not add a check here that the provider itself makes or skips.
- Fail-fast, settled: a named exception to the root rule. Off Linux, where no scoped view exists, a configured `scoped` isolation falls back to host silently, shown only as information in `rimz doctor`'s SANDBOX section, while an explicit `--isolation scoped` still refuses.
