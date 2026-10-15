# Trae adapter

Trae is a launch-only built-in. The [protocol reference](../../externals/agent-adapter/trae-reference.md) records rolling CLI 2.0 documentation, not binary-backed evidence. No executable was installed and no provider session was captured. CLI access requires TRAE Enterprise entitlement; installation and capture still require user approval.

The user explicitly requested a built-in despite [playbook Step 0](../../contributing/agent-adapters.md#step-0--confirm-a-built-in-is-the-right-shape): there is no native config migration or verified protocol beyond the canonical wire yet. A future installer into the documented `traecli.toml` is the intended reason to retain the built-in, but awaits CLI hook evidence. A process-plugin shim would have no verified events to translate today.

## Launch and resume

`crates/rimz/src/agents/adapters/trae/mod.rs::TraeAdapter` renders `traecli [--model M] [--yolo] [prompt]`. The prompt is positional; dash-leading prompts refuse rather than guessing separator behavior. Initial prompt submission is documented, not live-proven. `process_names` and `bin_names` both contain `traecli`; the runtime process name is an unverified premise. Unmanaged panes remain plain process rows because no discovery or installed reporting integration admits them.

Yolo alone has a verified documented meaning: disable approvals and provider sandbox. Ask and Plan add nothing and preserve the provider default; the `trae-auto` virtual cell refuses. Model renders `--model`; effort, auto-compaction presets, system-prompt replacement, headless execution, turn caps, and interrupt keys are unsupported. Explicit manual compaction sends bare `/compact`, with no instruction and no reported compaction bracket.

Bare RimZ `--resume` runs `traecli resume`, the id-free picker. Native `traecli --resume` instead chooses the latest session. RimZ never learns a provider session id, so the picker binds nothing back to the card; exact-id resume and fork refuse.

## Home and skills

The launch environment resolves an absolute, non-root `TRAE_HOME` that is neither `HOME` nor its ancestor, else `$HOME/.trae`, matching the defensive Kiro home pattern. No ambient provider-home reads occur in the adapter. There is no verified skill root: `skills_home` is `None`, manual skill policy is Unsupported, and sandbox profiles with any `skills` list refuse.

An existing home is bound into sandbox isolation. Run `traecli` once on the host first: when the home does not exist, the sandbox binds nothing and a first login inside it does not persist. Hook health does not create or require the home for a kind with no hook installer.

## Presence and messages

`crates/rimz/src/agents/login.rs::ProviderLogin::health` skips hook health for every kind declaring HookInstall Unsupported, before checking its home. `crates/rimz/src/store/snapshot/project.rs::assemble_launch_state` leaves prompted launches Idle/Idle when TurnLifecycle is Unsupported, retaining the prompt as task. This means present, turns untracked, not a claim that Trae has stopped. It avoids a false stalled-turn failure. Wrapper exit and the reaper provide cleanup; nonzero provisional exits still fail normally. No snapshot schema changes.

`crates/rimz/src/message/dispatch.rs::dispatch_targets` refuses CLI boundary delivery to a kind declaring TurnLifecycle Unsupported before any live or parked send, regardless of status. The error explains the absent turn boundaries and `--steer` alternative; it never recommends installing nonexistent hooks. The shared `crates/rimz/src/message/deliver.rs::receiver_readiness` also closes the Done gate for records queued directly by RimZ. Trae receives no RimZ boundary traffic: subagent digests, team stage wakes, or idle compaction. Queued records stay durable and pending with the lifecycle blocker in `rimz message show`, subject to existing expiry rules for automated commands. `rimz wait` self-wakes use steer rather than a boundary gate, so `crates/rimz/src/cli/wait/add.rs::run` refuses their arming with the declared lifecycle reason. The Any gate and explicit `--steer` delivery are unchanged. Reply waits, supervised runs, subagents, loop residents, and schedules retain their existing target-kind lifecycle preflight refusals; launching a supported child from Trae does not give its parent turn tracking.

## Coverage and evidence gaps

All nineteen concerns and all six user capabilities are Unsupported. Lifecycle signals are Absent except `lost`, Derived from RimZ's reaper. Conformance pairs an Unsupported SessionEnd concern with an Absent `ended` signal; wrapper cleanup of a launched pane does not claim provider session-end reporting. The conformance corpus is empty. No CLI hook names or payload fields, session paths, transcript schema, usage probe, or ACP handshake are assumed. The IDE hook contract is explicitly inapplicable.

The reference's [evidence requirements](../../externals/agent-adapter/trae-reference.md#evidence-required-before-implementation) govern any expansion. Until then the public tier is Launch only: argv, picker, model, yolo, and RimZ-launched presence; no turn state, asks, context, spend, history, account, or child tracking.

## Tests

`crates/rimz/src/agents/adapters/trae/tests.rs` pins documented argv, prompt refusal, presets, picker, and homes. Shared health, reducer, and message integration tests prove the coverage-keyed behavior. Registry, conformance, coverage matrices, homes, emblems, invariants, and atlas enroll the adapter without a guessed fixture corpus. A stub CLI smoke proves RimZ's launch path, not any real TRAE CLI protocol.
