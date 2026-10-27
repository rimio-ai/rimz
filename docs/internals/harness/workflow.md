# Workflow engine

A workflow is a durable run through a flat graph of checks and steps, defined in `workflow.toml` and sealed into the run when it starts. The run record is the only truth. Every decision reads it under the room workspace lock, a wake only asks the engine to read it again, and the journal beside it is evidence for people, never an input. A check decides where the run goes next: a `run` command, a shell command, a condition over loop facts, or a headless judge. A step launches a layout with one supervised leader or runs a command, then hands the run to a check. A loop task can start runs, and the scheduler tick re-drives every active run so its clock bounds fire on time.

The [workflows guide](../../guide/workflows.md) teaches the format, and the [workflow CLI reference](../../reference/cli/workflow.md) owns every command, printed line, park reason, and JSON shape. This page owns how the engine decides.

## Where the code lives

Paths are under `crates/rimz/src/`.

| Path | Owns |
| --- | --- |
| `harness/workflow/spec.rs` | `WorkflowSpec` and the serde types `rimz workflow schema` derives from, plus check- and step-kind selection (`Check::kind`, `Step::kind`), default edges (`Check::edges`), `run` argv resolution, deadline and window parsing. In the sidebar import graph. |
| `harness/workflow/validate.rs` | `validate` and `validate_spec`: every definition rule and the `rimz workflow validate` rows. |
| `harness/workflow/engine.rs` | Run control: `create`, `advance` and its locked `prepare`, `cancel`, `resume`, `wake`, `wake_run`, `rename_task`, and `persist`, the one publication path. |
| `harness/workflow/engine/agent.rs` | The agent-attempt protocol: `compose`, `launch`, `prepare_end` (admits the off-lock stop), `finish` (settles it). |
| `harness/workflow/engine/accounting.rs` | `transition` and `step`: the assist and diagnostic record for each persisted transition. |
| `harness/workflow/state.rs` | `Transition`: locked routing, waits and holds, bounds, agent-end staging, shared step settlement, non-agent end recording (`finish_script`), and the hosted-child kill. |
| `harness/workflow/script.rs` | `evaluate` and `execute_step`: host checks and command steps behind one interruptible child-publication boundary (`with_child`), and dispatch checks to `script/condition.rs` and `script/judge.rs`. |
| `harness/workflow/mod.rs` | The `AttemptHost` seam, `ScopeRoots`, and `advance_args`, the one `workflow advance` argv that run creation and both wake edges spawn. |
| `store/workflow.rs` | `RunRecord`, `JournalEntry`, their codecs, `list`, and `journal`. No transitions. |
| `cli/workflow.rs`, `cli/workflow/host.rs` | Parsing and presentation, without creating rooms. The private `Host` implements `AttemptHost` over the library launcher, the room snapshot, and the stop cascade. |
| `crates/rimz/src/harness/schedule/signal.rs::wake_workflows` | The signal edge: reads records and spawns the wake helper. |
| `crates/rimz/src/harness/schedule/fire.rs::tick_workflows` | The clock edge: paces and spawns one `advance` per due run. |
| `crates/rimz/src/harness/schedule/runner.rs::TaskFire::prepare` | The loop fire that starts a run for a `workflow` task. |
| `crates/rimz/src/store/gc/classes.rs::collect_owned` | Retention of terminal run homes. |

## Terms

| Term | Meaning |
| --- | --- |
| node | A check or a step. The run sits at one node at a time (`RunRecord.node`), entered at `entered_at`. |
| attempt | One evaluation of a check or one launch of a step, published on the record as a `LiveAttempt` of kind `script`, `judge`, or `agent`. Run and shell checks, conditions, and command steps use `script`. |
| seq | The attempt's number, allocated from `RunRecord.next_seq`. A settlement whose seq is no longer the live attempt's writes nothing. |
| host | The process that owns a live attempt (`LiveAttempt.host`: PID and process-start token). For a hosted check or command step it is the `advance` running it; for an agent step it launches or stops the layout. |
| child | A hosted check or command step's process group (`LiveAttempt.child`: leader PID and start token). |
| wait | The run stays at its node: a check routed to `@wait`, or a step whose admission is blocked. Repeats merge into `RunRecord.wait`. |
| hold | A wait a condition case with `for` creates, with outcome `hold`. |
| park | State `parked`, reached by routing to `@user`. Only `resume` leaves it. |
| bound | The run deadline, a node's timeout, or a step's visit cap. The engine routes a bound itself; a step timeout forwards the existing input, and other bounds supply an engine object. |
| terminal | `done`, `failed`, or `canceled`. A terminal record never changes again. |

## Definitions

Definitions live at `~/.rimz/workflows/<name>/workflow.toml`. Every table rejects unknown fields, and `rimz workflow schema` derives its JSON Schema from the same Rust types that deserialize the TOML. `rimz workflow validate [dir] [--json]` loads agent profiles through the machine definition catalog and reports dotted paths, resolved edges, warnings, and errors. It never creates a room, and a directory it checks must sit inside the definitions directory. The committed fixture `crates/rimz/tests/fixtures/workflows/babysit-pr/` shows script checks with declared wakes, supervised steps, a judge, and signs. Nothing installs it.

### The header

| Field | Rule |
| --- | --- |
| `scope` | Required. `worktree` runs every node in the selected checkout, and `project` runs every node at the room's project root. |
| `start` | Required. Names a check. |
| `deadline` | Optional. An RFC 3339 instant, or a local civil date or datetime. A bare date is local midnight. The record stores the resolved instant. |
| `on.deadline` | The one header edge. Defaults to `@user`. |

### Checks

`[checks.<name>]` selects exactly one runner (`Check::kind`):

| Kind | Selected by | Also takes | Routes |
| --- | --- | --- | --- |
| run | `run` | `wake`, `timeout`, `on` | Its own outcomes, plus `timeout` and `invalid`. |
| shell | nonempty `shell` | `wake`, `timeout`, `on` | Requires `on.pass` and `on.fail`. Accepts only those plus `on.timeout` and `on.invalid`. |
| judge | `agent`, with a required `prompt-file` | `window`, `wake`, `timeout`, `on` | Its own outcomes, plus `timeout` and `invalid`. The profile's adapter must have a headless launch. |
| condition | nonempty `cases` | required `else`, `provider`, `account`, `timeout`, `on` | Every case outcome and `else` must be routed. Any other route is refused, except `unknown`, `timeout`, and `invalid`. |

Every field outside a kind's row is refused on that kind. A `window` holds provider-window terms only. A condition takes `provider` exactly when a case reads a `window.*` term, and `account` only with `provider`.

A condition's cases are ordered `[[checks.<name>.cases]]` tables:

| Field | Rule |
| --- | --- |
| `when` | A loop-grammar term array. A term may not match the value `unknown`: route `on.unknown` instead. Stage names are checked against the effective teams plus `Done` when validation runs from a workspace, and a warning says so otherwise. |
| `outcome` | Required. Not a reserved outcome. |
| `for` | Optional nonzero hold in `s`, `m`, `h`, or `d`. A held case must route its outcome away from `@wait`. |
| `key` | Optional string or integer. |

`wake` is a list of selector strings or strict `{ signal, match }` tables (`match` optional), parsed by the loop's selector and match validator. Agent-family selectors, and team selectors other than exact `team.stage`, are refused (`SpecErr::LifecycleWake`).

### Steps

`[steps.<name>]` selects exactly one of `agent`, `run`, or `shell` (`crates/rimz/src/harness/workflow/spec.rs::Step::kind`), and requires `then` naming a check, never a step or a special target. Common optional fields are `timeout`, `max-visits`, `max-visits-per-key`, and `on.cap`. A run or shell step requires `timeout` and refuses the agent-only `prompt-file`, `lifecycle`, `window`, `takeover`, and `on.asked`, including explicit default values. Run argv resolution is the same as for run checks; shell text is nonempty and unconfined.

An agent step requires `prompt-file`. `agent` takes the fresh-launch layout grammar: kinds, profiles, virtual cells, and mixed agent/command layouts. Teams, command-only layouts and ambiguous prompt leaders refuse; judges still take one profile. Agent-only optional fields are `lifecycle` (`turn` by default, or `stay`), `window`, `takeover`, and `on.asked`. Staying steps refuse `timeout` and `on.asked`.

Both visit bounds must be at least 1. When both are set, `max-visits-per-key` must be below `max-visits`, since the total cap trips first. Per-key counts accumulate for the run, and unkeyed visits share one count. `on.cap` and an agent step's `on.asked` default to `@user`, neither may target `@wait`, and `on.cap` may not target its own step.

### Edges and the graph

An edge names another node, `@wait`, `@done`, `@failed`, or `@user`. Checks and steps share one name namespace, and a node or workflow name is nonempty ASCII letters, digits, `-`, and `_` (`crates/rimz/src/harness/schedule.rs::validate_name`). One edge resolver, `Check::edges`, supplies the defaults: `timeout` and `invalid` go to `@user`, and a condition's `unknown` goes to `@wait`. An explicit `on` entry overrides a default. Validation rows, routing, timeout admission, and cycle detection all read the resolved edges, so a default can never route differently from what `validate` printed.

| Rule | Why |
| --- | --- |
| `timeout`, `invalid`, `cap`, `deadline`, `unknown`, and `hold` are reserved (`crates/rimz/src/harness/workflow/spec.rs::RESERVED_OUTCOMES`). No case or `else` uses one, and no check answers one, except a condition's `unknown`. | The engine produces them for its own routes. |
| `on.timeout` cannot be `@wait`. | The timeout clock has already expired. |
| A check with any resolved `@wait` edge needs a `timeout`. | Otherwise a wait never ends. |
| A cycle through steps needs a `max-visits` on one of them. | A per-key cap alone cannot bound a check that names a new key every time. |
| A check-only cycle needs a wait edge. A hold is not an edge and does not count. | Otherwise one advance would loop without pause. |

Validation warns, without refusing, about a turn step whose leader adapter does not report background work (`do_background`, which every row carries), a judge sharing its target step leader's profile, and turn steps or judges while machine auto-continue is off.

### `run` and `shell`

`run` is an argv parsed with `shlex`, never shell text: pipes, variable expansion, and redirection belong inside a script. Every word containing `/` names a file relative to the workflow directory (`crates/rimz/src/harness/workflow/spec.rs::resolve_argv`). Only normal path components are accepted, and canonical resolution must stay inside the directory. Other words, such as `sh` or `uv`, resolve through `PATH`. Files are resolved and hashed again at every evaluation.

`shell` is unrestricted `sh -c` text. Its sealed spec digest covers the command, and the paths it references are neither confined nor hashed one by one.

Checks must be idempotent. A host killed after a script's effects but before settlement causes a fresh evaluation, never recovery from the previous stdout.

## Run control

`engine::create` admits a run in this order:

1. Compare every nonterminal run of the same workflow against the candidate, each by the path `ScopeRoots::path` gives for that run's own scope kind. A match is the typed `WorkflowErr::Exists`.
2. Read the prompt and the definition bytes, and validate those bytes with `validate_spec` against the run's project.
3. Require the room's workspace record (`rimz start` creates the room).
4. Under the workspace lock, repeat step 1, then write the run home (`store::workflow::initialize`).
5. Spawn a detached `advance`.

Step 1 runs before anything is read, so a held run wins over a broken definition, a prompt-file error, or a scope-kind edit: a loop fire records each as a skip. Comparing each existing run by its own kind keeps a worktree run at the project root from blocking a different worktree. `rimz workflow run` validates the definition directory before it calls `create`, so a manual start of a broken definition refuses on the definition even while a run holds the slot. The CLI prints `Exists` as a refusal, and a loop fire records it as `workflow_skipped`.

`rimz workflow run` resolves `-w` to an existing RimZ-owned worktree before it calls `create`, never creating one, so an unknown worktree refuses even a project-scoped run. Without `-w` the checkout is the current one. Worktree scope runs in that checkout. Project scope always runs at the root, and the CLI prints a stderr notice when `-w` named a worktree. Run ids are `wf_` identities. `list` enumerates active and parked runs across rooms, and `show`, `cancel`, `resume`, and the hidden `advance` find a run id from any directory through `crates/rimz/src/disk/paths.rs::workflow_runs_under`, which skips a room whose run homes cannot be read. A live staying attempt has DETAIL `with you` in list text and JSON `detail`, and show reads `active (with you)`.

`cancel` takes the workspace lock and returns false on a terminal record, so the first terminal write wins. It kills the child of a hosted check or command step and ends the run `canceled`. For an agent step it stages a `canceled` end with cause `Cancel` and stops the attempt ([Stopping an attempt](#stopping-an-attempt)). While a live host is mid-launch or already ending the attempt, cancel leaves the stop to that host.

`resume` refuses a terminal run, and on an active run it accepts only `--extend`. A parked run re-enters its current node, or `--at <node>`, with a fresh `entered_at` (so a fresh timeout clock) and a cleared judge retry. `--extend visits=+N` adds a run-wide `visit_grant` to every step's `max-visits` and `max-visits-per-key`, for every key, and `--extend deadline=<when>` replaces the deadline and re-arms it.

## Storage and isolation boundary

Each room holds one home per run, `owned/workflows/<run-id>/`. All its paths come from the disk path module (`crates/rimz/src/disk/paths.rs::WorkflowPaths`):

```text
record.json        current run, version 1
journal.jsonl      append-only transition evidence
workflow.toml      sealed specification bytes
work/              RIMZ_WORKFLOW_DIR, writable by nodes
  context.json     replaced before every evaluation or attempt
  notes/           node-owned cross-node scratch, created with the run
  signs/           live step guidance, created with the run
  <seq>.step.log   command-step stdout and stderr
```

The record, the sealed spec, and the journal sit outside the node-writable `work/`. Notes, signs, and step claims live under `work/`, and the engine reads only `signs/<step>.md`, never notes or claims. Every evaluation and attempt takes a fresh `seq`, a judge retry and a re-run after a dead host included. A node therefore keys a `notes/` write by the item it records, since keying it by `seq` would not make a repeat a no-op.

Soft reset keeps run homes, and hard reset removes them, unlike supervised-run records. Garbage collection removes a terminal run's whole home once `record.json` is older than the seven-day owned grace. A home without `record.json` is a crash during creation, which every reader skips, and it goes once the home directory itself passes the same grace.

The record stores the scope, project root, sealed-spec digest and original definition directory, prompt, current node and entry time, state and park, routed input and key, open wait and recheck time, per-step totals and per-key visits, the visit grant, deadline state, the sequence allocator, the live attempt, the previous step attempt, and the terminal time. Keys keep integer versus string identity.

A journal entry carries its time, node, optional attempt sequence, and one event: evaluation, attempt, closed wait, engine route, park, terminal, cancel, resume, or signal wake. An evaluation keeps the routed object and its `origin` (`check` or `engine`, defaulting to `check` when absent). An invalid answer also keeps a stderr tail. Script and judge evaluations keep the digests published with the attempt's child, whichever process ends the attempt, and an evaluation whose child never spawned has none.

`engine::persist` is the one publication path. Under the workspace lock it appends each journal entry and data-syncs the journal, then replaces the record, then writes accounting. A crash between append and replacement can leave a duplicate entry with the same sequence, and history never authorizes a transition. Creation syncs every new directory up to the room root. Readers skip torn or unknown journal rows.

## The advance loop

`engine::advance` is one loop of three phases, and each phase boundary is a lock boundary:

1. Prepare, locked (`crates/rimz/src/harness/workflow/engine.rs::prepare`). Load the record and decide: return, route a due bound, reclaim a dead host's hosted attempt, count an early advance against a wait, merge a step admission wait, ask for an eviction, or publish a new attempt with its `seq` and `context.json`.
2. Run, off-lock: evaluate the check, run a command step, evict the checkout's occupants, launch an agent step, or stop an ending agent step.
3. Settle, locked: reload the record, confirm the attempt's `seq` is still live, route, and persist.

A stale settlement writes nothing. One advance keeps going until the run waits, parks, ends, meets a live host, or would evaluate the same check twice with no step attempt between. Repeated cap routes also yield rather than spin without launching. Each guard leaves the run active, and the next wake or tick drives it on.

### Hosted children

Run checks, shell checks, judges, and command steps run inside the short-lived `advance` process, never as supervised runs. Preparation publishes the live attempt (kind, sequence, node, start time, host PID and start token) and its context under the workspace lock, so a stale host cannot overwrite a newer attempt's context. `crates/rimz/src/harness/workflow/script.rs::with_child` then re-takes the lock, re-reads the bounds, spawns the child, and publishes the child's group-leader PID, start token, and file digests in one write, before it releases the lock and waits for output. Spawn and publication thus share the lock with cancel and bounds. Run steps pass argv directly through the hosted runner; shell steps use `sh -c`. Both tee stdout and stderr to `crates/rimz/src/disk/paths.rs::WorkflowPaths::step_log`, `work/<seq>.step.log`.

While it waits, the host re-reads the record at the bounded subprocess runner's interrupt cadence and interrupts its child when the attempt is no longer live or a bound is due. Another advance leaves a live host alone while no bound is due. A check host that is dead, or whose PID was reused, turns its attempt into an engine-origin `canceled` evaluation, which re-runs free. A command step instead journals a `canceled` attempt with a lost-host reason and follows `then`, without repeating its side effect or counting another visit.

Any advance or cancel kills the recorded group under the lock before withdrawing the attempt, including when the host is stalled or dead (`Transition::kill_child`). It signals only when the leader's start token still matches or, once the leader's token is unreadable, when a live member of that group carries this run's `RIMZ_WORKFLOW_RUN` marker. Membership is checked again after reading the marker and liveness. A missing or changed token alone never authorizes a signal. A hosted leader stays unreaped until its pipes close, so the published PID and token stay a valid kill handle. A surviving host reaps its child when it resumes, and its stale sequence cannot settle again.

A run or shell check or command step runs in the scope checkout with null stdin, the room workspace pin, `RIMZ_WORKFLOW_RUN`, and `RIMZ_WORKFLOW_DIR`. `context.json` holds `run`, `prompt`, `node`, `seq`, `key`, `input`, `output_path` (`work/<seq>.check.json` for a judge, null otherwise), `retry`, `signs_path`, and `prev_attempt`. A step's context adds `visit` (under the current key) and `visits` (total), both counting this launch. `prev_attempt` exposes the step, outcome, sequence, visit, key, start and end, claim path, handle, supervised run id, HEAD pair, stopped and surviving children, ask or limit evidence, cost, exit `code`, and `log`, with null for whatever was not observed. Signs and the run prompt compose only agent prompts.

### Command-step settlement

`crates/rimz/src/harness/workflow/state.rs::Transition::finish_script` is the end-recording entry for non-agent attempts across the hosting advance, bound, cancel, and dead-host reclaim. The node table decides the journal shape: a step records `Attempt` and `prev_attempt`, a check keeps the engine `Evaluation`. Agent and command steps share the attempt journal and routing tail. Exit 0 is `ended`; nonzero, an external signal, spawn failure, and log-open failure are `died`. These and a step timeout (`timeout`) follow `then` with the original input. The run deadline is `canceled` and follows `on.deadline`; cancel is `canceled` and terminal. An interrupted child whose bound was extended before settlement is `canceled` and follows `then`.

Only an interrupted child is classified by the bounds at settlement. A child that exited on its own keeps its exit outcome; the next prepare applies any still-due run deadline. Another advance meets the same rule: `Transition::bound` leaves a step whose published child has exited to its live host, and that advance stops. The hosting process samples HEAD after reaping, off-lock. Other settlers leave `head_end` null. `code` is null for a signal or an unobserved exit, and `log` is set only when the step log exists. Agent-only evidence is null, while `claim_path` still names the sequence's claim file. Both new evidence fields default when absent in older records and journal rows.

Command steps bypass `AttemptHost`, prompt composition, provider windows, and occupancy completely. The common cap check still precedes visit and sequence allocation. A fast cycle has no new throttle: it can consume its cap within one advance, so workflow authors route through a waiting check.

Three windows stay open. Only the hosting process writes the step log, so when another advance enforces a bound on a stalled host, the next check can read the log before the host drains what the killed child wrote. A host killed after the spawn and before it publishes the child leaves `child` unset, so the dead-host reclaim kills nothing and the command can outlive its settled attempt. Checks share that second window. A live host that stalls (SIGSTOP, a frozen cgroup) after its command's group leader exits holds the step: `Transition::bound` leaves it to that host, so no other advance enforces the step timeout or the run deadline, and a group member still running keeps running. The host's own bound kills the group once it resumes, and the dead-host reclaim kills it once the host dies. Waiting for the group to empty would not close this window: `kill(-pgid, 0)` succeeds while the exited leader is still unreaped, so another advance would again overwrite the exit the host is about to record.

### Check answers

A run check or a judge answers exactly one JSON object: a required string `outcome`, an optional string `reason`, an RFC 3339 `recheck_at`, and a string or integer `key`. Other fields stay in the object as payload. The full normalized object (`crates/rimz/src/harness/workflow/state.rs::normalize_object`) becomes the next node's input, and a missing key inherits the run's current key.

For a run check, these route `invalid` with an engine object: a nonzero exit, stdout that is not one object, a wrong field type, an unrouted or reserved outcome, a bad `recheck_at`, and a `run` file that no longer resolves. A judge's failures settle differently ([Headless judges](#headless-judges)).

`recheck_at` only delays a `@wait`. `Transition::bounded_recheck` clamps it to the earlier of the check's timeout and the run's unfired deadline, so the tick cannot sleep past either bound. A judge's provider-limit reset is clamped the same way.

### Shell checks

A shell check runs through the loop's `crates/rimz/src/harness/schedule/runner.rs::run_command` in its hosted mode: direct `sh -c` in a new process group, with no session trampoline and no SIGINT listener. Before polling, the spawn callback publishes the group leader under the workflow lock, and a callback error kills the group and returns the error. The interrupt predicate runs every 20 ms, kills the group at once, and bounds pipe draining to 200 ms. Output is teed to `work/<seq>.check.log`.

The engine builds the answer from the exit: `pass` for exit 0, otherwise `fail` with a reason (the [reference](../../reference/cli/workflow.md#json-shapes) owns the shape). Digests are empty and no tail is journaled. A spawn or log-open failure routes `invalid`. The workflow's own timeout and deadline interrupt through the same predicate and settle through the engine's bound routes, never as `fail`, and no loop default timeout applies.

### Conditions and holds

A condition runs the same prepare, evaluate, and settle protocol with kind `script` and no hosted runner child (`crates/rimz/src/harness/workflow/script/condition.rs::evaluate`). The loop evaluator and leaf matcher stay two-valued. A projection used only by workflows computes Kleene truth from the readings: the first true case wins, an unknown case before it answers `unknown`, and all false selects `else`.

Every forge reading is cleared unless the latest probe succeeded with `checked_at >= entered_at`. Stage and projected usage-window readings have no such floor. The floor has two latency costs: a check can wait about five minutes at idle probe cadence, and consecutive forge checks each wait for a new probe. A closed room leaves the last post-entry facts in force. The answer carries the winning case index (omitted for `else` and `unknown`), the fixed or inherited key, the readings after the floor and commit fence, and, for `unknown` only, a reason naming each unread key. Its origin is `check` and its digests are empty.

After the floor passes, bare `ci` on an Open link with a nonempty `open.head` is fenced by commit. First, a readable local HEAD differing from the PR head makes `ci` unread only when `git merge-base --is-ancestor <local> <pr_head>` exits exactly 1. Otherwise, a known, nonempty `ci_sha` differing from the PR head makes it unread. Rule 1 wins when both apply; the floor's reason precedes both. The [reference](../../reference/cli/workflow.md#json-shapes) owns the two reason forms. An unreadable local HEAD skips rule 1, not rule 2. An ancestry exit 0, 128 (including an unfetched PR head), other exit, timeout, or spawn error also leaves rule 2 to decide. An unknown `ci_sha` skips only rule 2. Without a PR head, including legacy empty heads, merged or closed links, Gitea, and GitHub facts misses, the floor alone applies. Branch selectors, `pr`, `pr.queue`, PR-less `branch_ci`, and loop conditions retain their previous behavior, including propagation lag.

The evaluator reads the cache once, then reads fresh local HEAD in the scope path, comparing the PR head with local HEAD and `ci_sha` with the PR head. Both git reads are bounded at two seconds through `crates/rimz/src/proc/mod.rs::run_bounded_git_output`, outside the sidebar graph. Git runs only for a case naming bare `ci` after the floor passes and an open PR head is known; ancestry runs only when local HEAD differs. Settlement under the workspace lock does not re-read either input: the answer describes the cached PR head at `checked_at`. A later commit or push is a new fact for the next evaluation in this check, or on returning from a step.

No probe cadence or signal changes accompany the fence. A local HEAD change nudges one probe; if it beats the forge, the next probe uses the repository TTL (20 seconds hot, 300 seconds idle). CI signals fire only on verdict changes, so `failed` at an old commit becoming `failed` at the new one is sampled by the minute workflow tick rather than a CI signal. The wait therefore adds up to a tick after the cache catches up.

Settlement under the lock decides holds, never the off-lock evaluator. A winning case with `for` merges a wait keyed by `(outcome, case)`, with outcome `hold`, the case's outcome as detail, and the case index in `Wait.case`. A hold keeps the run's routed input and key, and the case's key lands only when the hold fires. Once `now - first >= for`, the case routes its own outcome. Any other answer closes the hold, and a later true case starts it again. Every evaluation recomputes the bounded `first + for` recheck, so a wake keeps `first`. The tick re-samples a hold before its recheck time, and a transition that falls entirely between two samples goes unseen.

### Waits and parks

A wait keeps its node and the node's original timeout clock. Repeated identical waits merge in the record (`first`, `last`, `count`, `recheck_at`), and leaving the wait appends one summary to the journal. An advance that arrives before an ordinary wait's `recheck_at` increments its count without evaluating. A hold is re-sampled instead.

`@user` parks with the typed bound reason or the check object's `reason` text. A check-timeout park carries no text of its own; the last journaled check answer preserves why the check waited, and `show` prints it in the history. `@done` and `@failed` end the run.

### Bounds

Every advance checks bounds before anything else. The engine routes a bound with an engine object that inherits the current key:

| Bound | Fires when | Input object | Route |
| --- | --- | --- | --- |
| Deadline | Once, when the unfired deadline passes. It stops gating until `--extend deadline` re-arms it. | `{outcome: deadline, deadline: T}` | `on.deadline` |
| Check timeout | The check's `timeout` after `entered_at`. It spans every evaluation and wait at the node. | `{outcome: timeout, node: C, timeout: S}` | `on.timeout` |
| Step timeout | The step's `timeout` after the attempt started. | The run's routed input | Ends the attempt as `timeout`, then `then`. |
| Visit caps | Before launch, the total visits reach `max-visits + visit_grant`, or the current key's visits reach `max-visits-per-key + visit_grant`. Total is checked first. Nothing launches, and no visit or sequence is consumed. | `{outcome: cap, step: S, bound: "max-visits" or "max-visits-per-key", visits: N, max: M}`. `visits` and `max` belong to the tripped bound, with the grant included in `max`. | `on.cap` |

## Agent attempts

Agent steps launch through `launcher::prepare_layout` and `launcher::execute`, with room Birth and a detached leader run (`turn` for turn steps, `session` for staying steps). They open an unfocused new tab titled `run <leader kind>`, never as the advancing agent's children. The leader retains supervised preflight and defaults an unset permission mode to Auto; companions use layout preflight with loop-equivalent trust verdicts and workflow-named refusals. Every agent cell carries `WorkflowAttemptRef {run, seq, role}` and pins `RIMZ_WORKFLOW_RUN` and `RIMZ_WORKFLOW_DIR` under every isolation level. Only the leader has a run; command cells follow its durable record and close themselves after it settles. Subagents do not inherit this workflow identity. `LiveAttempt.lifecycle` defaults to turn and omits that default, preserving old records and sealed specs.

### Admission

Admission checks, in order, bounds, the visit caps (total first, then the current key, whose count is zero when absent), each agent cell's provider window, and checkout occupancy. Each blocked window or occupancy check merges a wait that keeps the routed input and key and spends no visit:

| Blocker | Wait |
| --- | --- |
| The step's `window` is closed for any agent cell's provider. | `window`, with the blocking term and its reading in `Wait.detail` (`5h.left=12<42`), which the waiting line prints. |
| An occupant is mid-turn. | `takeover` |
| An idle occupant, and the step has no `takeover = true`. | `occupied` |

Who occupies the checkout follows the [shared takeover occupancy rule](./loops.md#condition-planning). At the project root, owned nested worktrees are excluded. With `takeover = true`, eviction runs off-lock, and admission then reads bounds and the window again. A successful eviction admits only the immediately following launch, fenced by its next sequence, without another occupancy read. A failed eviction becomes a counted `died` attempt.

Prompt composition and `git rev-parse HEAD` run under the lock after the first bound read, and they take time. So `prepare` takes a fresh clock after them and reads the bounds again, and a bound that came due meanwhile routes without publishing an attempt, spending no visit and no sequence. Only after that second read does an admitted launch count the visit, record HEAD, and publish its context and live attempt, stamped with the fresh clock. The bounds are re-read rather than the work moved off-lock, because admission and publication share the lock on purpose. The window reading is not re-read: it was sampled in the same pass, and provider windows move in minutes.

### Prompt composition

`crates/rimz/src/harness/workflow/engine/agent.rs::compose` joins three texts with blank lines at every launch: the prompt file, `signs/<step>.md` when present, and the run prompt. A sign's canonical path must stay inside `work/`. The digests are keyed by the spec's prompt-file spelling, `signs/<step>.md`, and `prompt`, and absent optional text has no digest. A relaunch can find a dirty checkout left by a stopped attempt, and the step's prompt must handle it.

### How an attempt ends

`crates/rimz/src/harness/workflow/engine/agent.rs::prepare_end` maps what the host observes to the attempt's outcome:

| Observed | Outcome | Route |
| --- | --- | --- |
| Supervised run `completed` | `ended` | `then` |
| `failed`, `verify_failed`, or `timed_out` | `died` | `then` |
| `canceled` | `canceled` | `then` |
| Turn run `budget_exceeded` | `limited`, with no known reset | `then` |
| Turn leader or a stopped descendant with a `paused_rate_limit` or `paused_spend_limit` turn error | `limited` | `then` |
| Turn leader or a stopped descendant with a blocking ask | `asked` | `on.asked`, or a park with the ask's text and kind |
| A turn step's own `timeout` | `timeout` | `then` |
| The run deadline | `canceled` | `on.deadline`, once |

A route through `then` forwards the run's routed input unchanged, with the current key: the attempt's outcome and evidence go to `prev_attempt`, never into the input. A queued asynchronous question does not end an attempt. The host reads asks by ask id and limits from typed turn-error classes, taking the reset from the affected agent's account and model, never from provider text. A launch refusal is `died`, or `limited` for a budget refusal, with the refusal text as the attempt's reason.

A staying attempt ignores live asks and provider limits and has no step timeout. Turn ends, interruptions, session-end hooks, and RimZ budget parks keep its session run open. A park still interrupts the provider and stamps its ledger; a budget-gate refusal before launch still reads `limited`. Provider exit completes a registered session as `ended`, or an unregistered one as `died`; a crash after registration also reads `ended`. The run deadline beats a completed observation and routes `on.deadline` with outcome `canceled`. Cancel, reset and rebirth also cancel staying attempts. A stay end records no ask in `prev_attempt`, and companion trees never control an attempt's end.

### Stopping an attempt

Every end publishes `ending` and saves the observation into `prev_attempt` under the lock before anything stops, which keeps the asks and limits the stop would retire. The host stops every pinned layout agent and its tree off-lock and samples spend and HEAD after the stop. It then re-locks, checks the sequence and the stopper's identity, journals the attempt, and routes. No successor launches before the stop returns.

- A later advance leaves a live stopper alone and reclaims a dead one, merging the staged child evidence with the new stop inventory.
- A cancel during launch leaves the live launch owner to publish and stop the run it gets back.
- When the launcher is dead, the stopper finds the unpublished supervised run by its workflow id and attempt sequence and stops it before settlement.
- Live launched descendants are stopped first; they and companion cells are recorded under `children.stopped`. Independently launched peers and team cohorts survive under `children.live`, with their board stage when available.

Stops use the shared cascade and add no workflow-specific lifecycle events. The next visit's [occupancy read](./loops.md#condition-planning) skips a stopped attempt whose owner has exited, even while its cached row remains. The host stops every unended row with the attempt pin, even if the leader row is absent or ended, falling back to the supervised stop only when no pinned row remains. Attempt cost comes from the leader run after the stop; companion spend stays on cards and in stats, and unstamped spend stays null.

## Headless judges

A judge (`script/judge.rs`) runs through the shared headless profile resolver and launch planner with automatic permission mode. Its profile must resolve to exactly one agent cell, or the evaluation fails (`crates/rimz/src/harness/headless.rs::HeadlessProfile::resolve`). Its prompt names `RIMZ_WORKFLOW_DIR`, `context.json`, and `output_path`, and demands exactly one JSON object with the check fields and no prose or Markdown fence. The schema stays open, so extra fields stay routed input. Claude receives it as its schema argument (`crates/rimz/src/agents/adapters/claude/headless.rs::ClaudeHeadless::render_argv`). Codex omits its strict structured-output switch for an open schema and reads the prompt-shaped last message from its output file. Closed schemas, loop verdicts among them, keep Codex schema enforcement. The provider's structured value is saved to `output_path`, the engine reads that file when there is none, and the value goes through the same normalizer as script stdout.

A judge is read-only on the scope checkout and its Git admin and common directories. Scoped isolation overlays them read-only after all reach rebinds, and host isolation enforces the rule only through the prompt. The root view and the required-path rebinds leave `work/` writable, a room home under host `/tmp` included. A judge evaluation never produces `asked`: nothing reads asks from a headless child, and what the provider does with a permission request in automatic mode is provider behaviour. Claude classifies provider limits in its adapter. Codex supplies no typed limit result, so a limited Codex judge fails as `died`.

Folder-trust and adapter minimum-version checks run before the judge takes the workspace lock to spawn. A failed judge evaluation settles by its failure (`Transition::finish_judge`):

| Failure | Result |
| --- | --- |
| Engine-decided `canceled` (dead host) | Re-runs free. |
| Typed provider limit | Spends no retry. Waits until the known reset, clamped like `recheck_at`, or until a subscribed wake or the tick when the reset is unknown. A matching wake clears a known reset's backoff. |
| `died`: a launch refusal, a nonzero exit, no object, or an output file that is not JSON. `invalid`: an object the normalizer rejects. | One immediate retry with `context.retry = {outcome, reason}`, then a park as `check`. A launch refusal carries its text as `reason`. |

The record stores `check_retry` and `check_retry_context`, so retry evidence survives a dead host without reading the journal. Both reset on node entry and on a parked resume.

## Wakes

A wake only spawns an `advance`, which reads the record again. `run` and `resume` spawn one, and `rimz workflow advance <id>` is the manual form. There is no signal replay: a lost wake delays progress until the next tick.

### The signal helper

Every signal routed through the scheduler's shared firing entry, except lifecycle signals, spawns one room helper when the room has an active run (`crates/rimz/src/harness/schedule/signal.rs::wake_workflows`). That path only reads records and spawns, carrying the whole signal as JSON in the helper's argv. The helper (`engine::wake`) takes the workspace lock per run, reloads each active run and its sealed spec, and matches only the current check's subscriptions, through the same delivery matcher as loop triggers. A sibling skip does not wake. A step, or a check with no matching subscription, gets no write, journal entry, or spawn.

On a match the helper clears the record's and the wait's `recheck_at`, journals the signal name, and marks a live evaluation `woken`. If that evaluation then answers `@wait`, settlement clears the new backoff and re-evaluates immediately, once, bypassing the hot-loop guard for that wake. Wakes during the retry can ask for another pass, and without one the ordinary guard applies. The helper spawns one `advance` per matched run, so one slow script cannot hold back another run. A run that fails to load warns without blocking the others.

### Subscriptions

A check's subscriptions come from its `wake` list, or, for a condition, from its terms (`Check::subscriptions`):

| Condition term | Derived selector |
| --- | --- |
| Checkout `ci` | `ci.*` |
| Checkout `pr` or `pr.queue` | `pr.*` |
| Stage | `team.stage` |
| Branch-selector or `window.*` | nothing |

Negated terms derive the same selectors. At match time, never persisted, `ci` and `pr` selectors add `path = scope.path` unless `path` or `branch` is explicit, and `team.stage` adds the board `crates/rimz/src/harness/scratch.rs::durable_board_path` resolves for the scope unless `team`, `instance`, or `board` is explicit. Other families stay room-wide. A checkout relink therefore follows its new archive, and project scope matches the project root. A check with no subscriptions runs only on its recheck time and the tick. A matched wake still clears a hold's or a judge limit's backoff.

### Supervised-run edges

An agent step's supervised run wakes its own workflow directly, through `engine::wake_run`. Every newly terminal run write through the harness wakes the run named in its `workflow` field after releasing the workspace lock: provider exits without hooks, wrapper failures, turn-run budget parks, timeouts, and rebirth cancellation included ([scripting.md](./scripting.md#the-record)). A hook-ended run that is not a peer turn settles its session spend before the wake, so the advance reads the stamped cost. Non-terminal lifecycle and turn-error frames carrying `RIMZ_WORKFLOW_RUN` wake only that run.

Room teardown cancels supervised runs without waking workflows, and the next advance settles those attempts. Rebirth excludes every workflow-pinned cell from recovery and cancels nonterminal workflow runs even outside the roster, unless a live provider or wrapper still serves them, then wakes each workflow to decide again. A stop never closes a supervised run's pre-rebirth pane id in the new session. A descendant's frames carry no workflow pin, so its asks and limits are sampled at the next step edge or tick, within about a minute for turn attempts.

## The tick

`crates/rimz/src/harness/schedule/fire.rs::fire_due_tasks` ends every scheduler tick, from the room host or the external timer, by calling `tick_workflows` for the root. It reads the room's run records and spawns `rimz --root <root> workflow advance <id>` for each due active run. A run is due when it has no `recheck_at`, when its `recheck_at` or its unfired deadline has passed, or while it holds a condition case. Parked and terminal runs are never spawned. The engine decides everything else (bounds, window, occupancy, a live host). The tick reads `recheck_at` so that early advances do not inflate an ordinary wait's `×N` count, and it never clears backoff the way the signal helper does. Under the systemd timer the spawned advance leaves the oneshot service's cgroup, as a loop run does.

Spawns are paced by the runtime cache `lanes/workflow-tick.json`, a map of run id to last spawn: at most one spawn per run per 60 seconds (`WORKFLOW_TICK_SECS`), except that a `recheck_at` or deadline that passed since the last spawn fires at once. The tick writes the cache before spawning and drops runs no longer active. A read failure counts as an empty cache, and a write failure warns and skips that tick's spawns. `fire.rs` is in the sidebar import graph, so it writes only that cache and spawns, and the invariant grep keeps it off the writer and the wake socket.

For workflow authors, a `@wait` without `recheck_at` is re-evaluated about every minute, so a check that wants a slower pace names `recheck_at`. A live step is observed every tick, so a descendant's ask or provider limit is noticed within a minute.

The external timer's roots (`crates/rimz/src/cli/loop_timer.rs::task_roots`) include every room with a nonterminal run, found through `crates/rimz/src/disk/paths.rs::workflow_runs_under`, so a manual run in a room with no loop task and no sidebar still ticks.

## Loop tasks

A `workflow = "<name>"` task is the fourth loop action (`TaskAction::Workflow`). Its fire takes the run lock, checks the room deadline, births the room when needed, and calls `engine::create` with the task's resolved prompt and `task = Some(<name>)`. It takes no start-throttle turn: the fire starts no agent, and only the run's agent steps launch as supervised runs. A created run is a `launched` history row whose `run_id` is the `wf_` id. A run already holding the scope, active or parked, is a strike-neutral `workflow_skipped` row carrying the existing id, and a finished run does not block. With a sustained-true `when`, the planner re-fires a workflow task at the resident cadence, like `stay`, so an active run collects one skipped row per retry per checkout.

`each-worktree` takes `workflow` as well as `stay`: the fire runs per owned checkout under that checkout's run lock and stamps `checkout` on its row. A project-scoped workflow under `each-worktree` creates one run at the root, and every later checkout fire skips it. `stay`, `takeover`, `subscribe`, `check`, and `judge` are refused on a workflow task, each naming the workflow node that owns the concept (`crates/rimz/src/harness/schedule.rs::TaskActionErr::WorkflowField`). `agent` and `wait` conflict with `workflow`, `overlap` is refused because workflow admission owns the runs, and `account`, `verify`, and `max-attempts` are refused because they need an agent action (`crates/rimz/src/harness/schedule.rs::TaskAction::from_entry`). The trust hash covers `workflow` (`crates/rimz/src/trust.rs::ExecutableTask`).

`RunRecord.task` is the provenance `rimz loop list` and `rimz loop show` select by, together with the record's project root. A manual run has none, so it appears in `rimz workflow list` and the sidebar under no task. `loop rename` rewrites `task` on the task's nonterminal records under the workspace lock (`engine::rename_task`) and leaves terminal records unchanged.

## Attention

`crates/rimz/src/sidebar/enrich.rs::enrich_core` fills `SidebarSnapshot.parked_workflows` from the run records on every fold, producer and passive reader alike, through the read-only `store::workflow::list`, oldest park first. The cockpit draws an inert line for each of the three oldest parks and folds the rest into a count (`sidebar_pane/render/compose.rs`, [interface](../../interface/sidebar.md)). A park appends no event, so nothing but the next fold shows it. The room host refolds every cycle, so a park reaches a watched sidebar within one data tick, or when an in-progress cross-tab jump releases the attachment, and a passive `rimz sidebar snapshot` shows it at once.

## Accounting

`crates/rimz/src/harness/workflow/engine/accounting.rs::transition` runs after `persist` publishes a transition and maps the journal entries it wrote. Accounting keys on evaluation origin, not on outcome words, because a check may legally route `canceled` or `limited`.

| Journal entry | Accounting |
| --- | --- |
| Explicit `Route` | One `workflow_route`, unless the record now waits. Suppresses the transition's evaluation and attempt accounting. |
| Check-origin evaluation | One `workflow_route`, unless the record now waits. |
| Engine-origin `canceled`, run active | `workflow_rerun` diagnostic, no assist. |
| Engine-origin `limited`, or run canceled | No assist. |
| Other engine-origin evaluation, active with retry context | `workflow_retry` diagnostic, no assist. |
| Other engine-origin evaluation | One `workflow_route` (including a second judge failure parking at `@user`). |
| Attempt end | One `workflow_route`, unless the run was canceled or an explicit route accounts for it. |
| Closed `Wait` | One `workflow_route` to `@wait` at the summary's timestamp, with `count` set to the final merged count, even on cancel. |
| Park or terminal | `workflow_park` or `workflow_end`. |

An open wait has no route assist yet: `list`, `show`, and the sidebar carry its live state. Closing a window, takeover, or occupied wait follows the same summary rule. Route records outside waits omit `count`. Explicit-route suppression also keeps a bound's engine evaluation from counting twice. `crates/rimz/src/harness/workflow/engine/accounting.rs::step` appends `workflow_step` after an agent step's supervised run or a command step's child is published; a command step has null handle and supervised run id. The loop runner appends `workflow_start` on creation. All five assist kinds count under the workflow category in `rimz stats`. Diagnostics are rate-limited forensics that stats never reads.
