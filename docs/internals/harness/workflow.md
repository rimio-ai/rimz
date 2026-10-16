# Workflow engine

A workflow is a durable run through a flat graph of checks and steps. The run record is truth; a wake only asks the engine to read it again. The current milestone runs script checks only. Agent checks, agent steps, project scope, window admission, takeover, prompt composition, and loop integration are not executed yet. Definitions and their generated schema already describe the full format. A testkit-only step substitute exercises attempt accounting without launching an agent.

## Where the code lives

- `crates/rimz/src/harness/workflow/engine.rs::advance` owns the locked decision, off-lock evaluation, and sequence-checked settlement loop; `create`, `cancel`, `resume`, and `wake` are the run-control entries in that module.
- `crates/rimz/src/harness/workflow/state.rs::Transition` owns locked routing, bound decisions, and hosted-child termination; `crates/rimz/src/harness/workflow/script.rs::evaluate` hosts the interruptible process group.
- `crates/rimz/src/store/workflow.rs::RunRecord` owns the persisted vocabulary; its module owns the record and journal codecs, not transitions.
- `crates/rimz/src/cli/workflow.rs::run` parses and presents without creating rooms. `crates/rimz/src/harness/schedule/signal.rs::wake_workflows` is the read-and-spawn signal edge; `crates/rimz/src/store/gc/classes.rs::collect_owned` owns terminal-home retention.

## Definitions

Machine definitions live at `~/.rimz/workflows/<name>/workflow.toml`. `rimz workflow schema` derives JSON Schema from the same Rust types that deserialize TOML. Every table rejects unknown fields. `rimz workflow validate [dir] [--json]` loads profiles through the machine definition catalog and reports dotted paths, resolved edges, warnings, and errors. Validation never creates a room. A workflow directory must be inside the machine definitions directory.

`[workflow]` has required `scope` and `start`, optional `deadline`, and one header edge, `on.deadline`. Start names a check. Scope is `worktree` or `project`; this milestone refuses project scope at run admission. Deadlines accept RFC 3339 instants or local civil dates and datetimes. A bare date means local midnight; the record stores the resolved instant.

`[checks.<name>]` has exactly one of `run` or `agent`, optional `prompt-file`, `window`, `timeout`, and `on.<outcome>` edges. An agent requires a prompt file; a window requires an agent and accepts only provider-window terms. The engine supplies `timeout` and `invalid`, each defaulting to `@user`. An edge may name another node, `@wait`, `@done`, `@failed`, or `@user`, except that `on.timeout` cannot wait on its already-expired clock. A check that can wait must have a timeout.

`[steps.<name>]` requires `agent`, `prompt-file`, and `then`, which names a check, never another step or a special target. Optional fields are `window`, `takeover`, `timeout`, positive `max-visits`, and `on.asked` and `on.cap` (both default to `@user`). Step edges cannot target `@wait`, and `on.cap` cannot target the capped step itself. A cycle through steps must contain a visit bound; a check-only cycle must contain a wait edge. Checks and steps share one name namespace.

`run` is an argv parsed with `shlex`, not shell text. Pipes, variable expansion, and redirection belong inside a script. Every word containing `/` names a file relative to the workflow directory: only normal path components are accepted, and canonical resolution must stay inside the directory. Other words, such as `sh` or `uv`, resolve through `PATH`. Files are resolved and hashed again for every evaluation. Checks must be idempotent: a host killed after evaluation but before settlement causes a fresh evaluation, not recovery from its previous stdout.

## Starting and inspecting

`rimz workflow run <name|dir> [-w <worktree>] [--prompt <text>]` validates the parsed bytes it seals before creating a run. Only a single normal path component is a catalog name; `.` and `./` refer to the current definition directory. It requires an existing room (`rimz start` creates one), refuses unsupported nodes, and serializes the uniqueness check and creation under the room workspace lock. Active and parked runs both hold the `(workflow, scope checkout)` slot. Without `-w`, scope is the current checkout; `-w` resolves an existing RimZ-owned worktree, never creates one. The prompt is stored unchanged and exposed in context.

Run ids are `wf_` UUIDv7 identities. `list` enumerates active and parked runs across rooms. `show`, `cancel`, `resume`, and hidden `advance` resolve an id from any directory. `show` starts with the work directory, then renders position, bounds, visits, prompt, waiting state, and the journal; JSON is `{dir, record, journal}`. The journal is inspection evidence, never an input to engine decisions.

`cancel` is first-terminal-wins. `resume` refuses terminal runs. A parked run re-enters its current node, or `--at <node>`, with a fresh timeout clock. `--extend visits=+N` adds a run-wide grant to every step's cap; `--extend deadline=<when>` replaces and re-arms the deadline. Extensions without repositioning also apply to active runs.

## Storage and isolation boundary

All paths come from the disk path module. Each room holds `owned/workflows/<run-id>/`:

```text
record.json        current run, version 1
journal.jsonl      append-only transition evidence
workflow.toml      sealed specification bytes
work/              RIMZ_WORKFLOW_DIR, writable by nodes
  context.json     replaced before every evaluation or attempt
```

The engine-owned record, sealed spec, and history are outside the node-writable work directory so a later sandbox write bind needs only one subtree. Future notes, signs, and claims live under `work/`; the engine does not read claims. Soft reset retains workflow homes. Hard reset removes them, unlike supervised-run records. Class GC removes a terminal workflow's entire home when its record mtime is older than the seven-day owned grace; active, parked, and recent terminal records stay. A home without `record.json`, left by a crash during creation and skipped by every reader, goes once the home directory's own mtime passes the same grace.

The record stores scope, project root, sealed-spec digest and original definition directory, prompt, current node and entry clock, state and park, routed input and key, open wait and recheck time, per-step totals and per-key visits, grants, deadline state, sequence allocator, live host identity, previous attempt, and terminal time. Keys preserve integer versus string identity.

Journal entries carry time, node, optional attempt sequence, and an event: evaluation, attempt, merged wait, engine-bound route, park, terminal, cancel, resume, or signal wake. Evaluations retain the routed object and script digests; invalid results also retain a stderr tail. History appends are data-synced before record replacement under the same workspace lock. New run directories sync their new ancestors. A crash between append and record publication can duplicate an entry with the same sequence, never authorize a transition from history. Readers tolerate torn or unknown history rows.

## Evaluation and routing

The host is the short-lived `advance` process, not a supervised run. A lightweight supervised run would require agent launch machinery for a script that only needs a process group and deadline. The record first publishes a live attempt (`kind = script`, sequence, node, start time, host PID and process-start token). Preparation publishes its context under the same workspace lock, so a stale host cannot overwrite a newer attempt's context. The lock is released after child publication, before waiting for output. A second advance leaves a live host alone. A dead or reused host identity becomes `canceled`: a check re-runs without counting a step visit; a step substitute keeps the visit counted at launch and follows `then` with a canceled previous attempt.

Scripts have scope checkout cwd, null stdin, the room workspace pin, and `RIMZ_WORKFLOW_DIR`. Context contains `run`, `prompt`, `node`, `seq`, `key`, `input`, `output_path` (null for script checks), `signs_path`, and `prev_attempt`. Only steps have `visit` (under the current key) and `visits` (total), including this launch. Previous attempts expose step, outcome, sequence, visit, key, start/end, claim path, and nullable handle, supervised run id, HEAD pair, and children. Agent-dependent fields are null in this milestone.

A check prints exactly one JSON object: required string `outcome`; optional string `reason`, RFC 3339 `recheck_at`, and string or integer `key`; other fields remain payload. Nonzero exit, bad JSON or field types, unrouted or engine-reserved outcomes, invalid recheck time, and missing live script paths route `invalid`. A missing key inherits the run's current key. The full normalized object becomes the target's input. `recheck_at` only delays `@wait` and is clamped to the check's entry-time timeout.

Before settlement the host re-locks and verifies the live sequence. A stale result writes nothing. `@done`, `@failed`, and `canceled` never change again. `@user` parks with the bound's reason or the check object's reason text. A wait keeps its node and original timeout clock. Repeated identical waits merge in the record (first, last, count, recheck time); leaving the wait appends one summary to history. An early plain advance increases the wait count but does not evaluate.

Every advance checks bounds before waiting. Deadline fires once, with `{outcome: deadline, deadline: T}` as input, then stops gating until extended. A check timeout spans all its evaluations and waits; it routes `{outcome: timeout, node: C, timeout: S}`. A step timeout ends the attempt and follows `then`. A step launch over `max-visits + visit_grant` launches nothing and routes `{outcome: cap, step: S, visits: N, max: M}`. Engine objects inherit the current key.

A running host re-reads the record at the bounded subprocess runner's interrupt cadence. Spawn and publication of the child's group-leader PID and process-start token share the workspace lock with cancel and bounds. Any advance or cancel kills the recorded group under that lock before withdrawing the attempt, including when its host is stalled or dead, but only when the child's start token still matches. Missing or changed tokens never authorize a signal. The host reaps its child when it resumes; its stale sequence cannot settle again. Checks remain idempotent, not exactly-once effects: a host can die after a script's effects but before settlement.

One advance continues through transitions until it waits, parks, terminates, encounters a live host, or would evaluate a check twice without a step attempt between. Repeated cap-only routes also yield rather than spin without launching anything. These guards leave the run active for a future wake. One transition per wake would stall check chains in this milestone, which has no scheduler ticker.

## Wakes

Run and resume spawn detached advances; manual `advance` is also available. Every non-lifecycle signal routed through the scheduler's shared firing entry spawns one room helper when active workflow records exist. The signal path only reads records and spawns. The helper clears active runs' `recheck_at` under their workspace lock, journals the signal name, and marks live attempts as `woken` (missing on older records means false). If that check later answers `@wait`, settlement clears its new backoff and immediately re-evaluates once, bypassing the hot-loop guard for that wake. Wakes during the retry can request another pass; without another wake the ordinary guard applies. The helper spawns independent advances so one slow script cannot block other runs' wakes. A bad run warns without blocking later runs, and cross-room discovery similarly skips unreadable rooms. Lifecycle signals deliberately leave waits untouched: ordinary agent turns must not clear every workflow's backoff. There is no signal replay and no automatic clock ticker in this milestone; a lost wake delays progress until the next signal or manual advance. Attempt-specific lifecycle wakes and loop ticking belong to later milestones.
