# Workflow CLI

`rimz workflow` starts, inspects, and steers durable workflow runs: a sealed graph of checks and steps under `~/.rimz/workflows/<name>/`, driven by the run record rather than by any agent's memory. The [workflows guide](../../guide/workflows.md) teaches the format and a full example; how the engine decides is [workflow internals](../../internals/harness/workflow.md).

```sh
rimz workflow schema                              # the spec's JSON Schema
rimz workflow validate                            # every machine workflow
rimz workflow validate ~/.rimz/workflows/babysit-pr --json
rimz workflow run babysit-pr                      # in the current checkout
rimz workflow run babysit-pr -w feat-x --prompt 'merge before Friday'
rimz workflow list                                # active and parked runs, every room
rimz workflow show wf_0192...
rimz workflow resume wf_0192... --at pr --extend visits=+2
rimz workflow cancel wf_0192...
```

The [global flags](../cli.md#global-flags) apply. Every subcommand except `run` works from any directory: a run id resolves across every room on the machine.

## Run a workflow

`rimz workflow run <NAME|DIR> [-w <WORKTREE>] [--prompt <TEXT>]`

| Argument or flag | Meaning |
| --- | --- |
| `<NAME\|DIR>` | A single path component is a name under `~/.rimz/workflows/`; anything else is a directory, which must lie inside `~/.rimz/workflows/`. `.` is the current directory. |
| `-w, --worktree <WORKTREE>` | Select an existing RimZ-owned worktree instead of the current checkout. It never creates one. Project scope overrides this selection. |
| `--prompt <TEXT>` | Stored unchanged, appended to every agent step's prompt, and exposed as `prompt` in `context.json`. |

Before creating a run, the command validates the definition and refuses on any error. It needs an existing room (`rimz start` creates one). With `scope = "worktree"`, nodes run in the selected checkout; with `scope = "project"`, they run at the project root, even when invoked from an owned worktree. One active or parked run holds each `(workflow, scope path)` pair; a second start refuses and names the existing run before reading the definition or prompt. On success it prints the run id, then the run's work directory (`RIMZ_WORKFLOW_DIR`), and the first advance starts in the background. The work directory includes `notes/` for node-owned cross-node scratch and `signs/` for step guidance. Every evaluation and attempt takes a new `seq`, a retry included, so name a scratch write after the item it records to make a repeat overwrite it.

When `-w` selects a project-scoped workflow, stderr prints `note: <workflow> has scope = project; the run is at <project root>, not <worktree path>`. The command still exits 0 and stdout is unchanged.

A step selects exactly one of `agent`, `run`, or `shell`, and requires `then` naming a check. A run or shell step requires `timeout` and accepts the common visit bounds and `on.cap`. `run` is a literally passed argv, with slash-containing words confined to the definition directory; `shell` runs through `sh -c` without path confinement. Command steps refuse `prompt-file`, `lifecycle`, `window`, `takeover`, and `on.asked`, and never wait for checkout occupancy. Their stdin is closed, both streams go to `work/<seq>.step.log`, and they inherit the workspace pin and workflow environment.

An agent step requires `prompt-file`. Its `agent` accepts the fresh-launch layout grammar, including agent and command cells. `lifecycle` is `turn` by default, `stay` to keep the attempt live until the leader's session exits, or `until` to check `then` at each eligible leader turn end and close when its route is neither this step nor `@wait`. A staying step (`stay` or `until`) has neither `timeout` nor `on.asked`. A kept `@wait` on `until` has no timed recheck: the next leader turn end checks again. Teams, command-only layouts, and ambiguous prompt leaders refuse with a field-specific fix. Judge checks still take one profile. An agent step's `window` gates every agent cell's provider.

## Inspect runs

`rimz workflow list [--json]` prints every active and parked run across rooms: `RUN`, `WORKFLOW`, `CHECKOUT`, `STATE`, `NODE`, `SINCE` (when the run entered its node), and `DETAIL`. A live session's DETAIL is `asking you: <handle> (<kind>): <text>` when a blocking ask is open; otherwise it is `with you`, followed by `; checking <check>` while an `until` check runs, then `; <waiting line>` when a wait is open. Other runs keep this precedence: [waiting line](#the-waiting-line), [park reason](#park-reasons), asking, with you. Asking text is collapsed to one line, and the active state uses the waiting color while asking. JSON `detail` uses the same text. Finished runs are omitted. `show` prints `active (asking you)` while asking, otherwise `active (with you)` for a live staying attempt. The session attempt is `held.attempt` while checking, otherwise `attempt`; its `lifecycle` is `stay` or `until`. Turn attempts omit that field. An open blocking ask is stored in the session attempt's `asking`; `asked_ms` is the total closed pause time in milliseconds. Both are omitted when empty or zero. While held, NODE names the check, and `show` adds a `check` row but keeps the session's handle and sequence in its `attempt` row.

`rimz workflow show <RUN> [--json]` prints the work directory first, then `workflow`, `scope worktree` or `scope project`, `state` (with the park reason when parked), `current node` and since when, `attempt` (the live agent step's handle and sequence), `check` (the check evaluating a held session), `ask` (handle, kind, and text, while active and asking or parked as `asked`), `deadline` (`armed` or `fired`), and `prompt`. Then the waiting line, `takeover:` lines naming the agents that hold the scope path when a step waits on takeover (`blockers unavailable` when the agent roster or the project's worktrees cannot be read), one `step <name>: N visits of <max> + <grant> grant` line per recorded step, and the history. An unbounded step prints `unbounded` instead of the limit and grant. A step declaring `max-visits-per-key` adds one indented `  key <json>: N visits of <max-visits-per-key> + <grant> grant` row per recorded key, in record order; unkeyed visits print `key null`. Steps without that field print no key rows. Each history line is time, `#seq`, node, event, any turn-end action, outcome, target, count, and the routed object; an agent attempt adds its handle, short HEAD before and after, cost, stopped and live children, a provider reset, the ask, and the digest of each prompt, sign, and run prompt it used.

### The waiting line

A run at a wait carries one line, in local time:

```text
waiting: pick, said in-flight, since 14:02, ×12, recheck 14:40
waiting: triage, window 5h.left=12<42, since 15:02, ×40
waiting: sweep, takeover, since 09:10, ×3
```

| Part | Meaning |
| --- | --- |
| node | Where the run waits. |
| `said <outcome>` | A check answered an outcome routed to `@wait`. |
| `holding <outcome> <elapsed>/<for>` | A condition case keeps winning while its hold duration runs. |
| `unknown: <keys and reasons>` | A condition cannot read enough data to choose its first matching case. Bare `ci` can name a local HEAD not on the PR or CI read at a different PR head; see [JSON shapes](#json-shapes) for the reason forms. |
| `window <term>` | An agent step or judge whose provider `window` is closed, with the failing term and its reading. |
| `takeover` | The checkout holds an agent mid-turn, so the step cannot evict it yet. |
| `occupied` | The checkout holds an idle agent and the step has no `takeover = true`. |
| `since` | First time the run waited here. |
| `×N` | How many advances found it still waiting. |
| `recheck` | The check's `recheck_at`, before which nothing evaluates; a signal matching that check's subscription clears it. For a `for` hold it is when the hold completes, and subscribed signals and the minute tick re-sample the case before then. |

## Cancel and resume

`rimz workflow cancel <RUN>` ends a run as `canceled` and stops its live script, shell, judge, or step attempt. A run already finished prints `already terminal; unchanged`.

`rimz workflow resume <RUN> [--at <NODE>] [--extend <GRANT>]...` puts a parked run back to work, at its current node or at `--at <NODE>`, with a fresh timeout clock and a cleared check retry. `--extend` repeats:

| Grant | Effect |
| --- | --- |
| `visits=+N` | Adds N to every step's `max-visits` and `max-visits-per-key` for this run, for every key. |
| `deadline=<WHEN>` | Replaces the run's deadline and arms it again. `WHEN` is an RFC 3339 instant or a local date or date-time; a bare date is local midnight. |

An active run accepts `--extend` alone; `--at` needs a parked run. A finished run refuses. The definition stays sealed: grants live on the run record.

## Validate and schema

`rimz workflow validate [DIR] [--json]` checks one directory, or every directory under `~/.rimz/workflows/`. The table has `WORKFLOW`, `NODE`, `KIND`, `RUNNER` (the run command, shell text, step layout, judge profile, or `condition`), `BACKGROUND` (whether the step's leader reports background work), and `EDGES`, each edge as `outcome → target (explicit|default)`. A step's `asked` edge appears only when `on.asked` is set, always as explicit; staying steps have none. Errors and `warning:` lines follow. Any error exits 1. `--json` prints `{rows, warnings, errors}`, each error and warning carrying `path` and `message`. Each row has `runner = "run" | "shell" | "agent" | "condition"`, nullable `run`, `shell`, and `agent` fields, `workflow`, `node`, `kind`, `do_background`, and resolved `edges`. Condition stage names use the caller's effective teams plus `Done`; without a resolved workspace, a warning says stage names were not checked. Run admission always checks against the run's project.

Command-step validation rows have `kind = "step"`, `runner = "run" | "shell"`, the command in the corresponding field, `agent = null`, and `do_background = false`. Their edges are `then`, `cap`, and `deadline`, never `asked`; `BACKGROUND` prints `—`. Agent-only warnings do not apply.

Turn and `until` steps warn when the leader adapter has no background reporting: a turn end may finish the attempt while provider background work remains. Staying steps skip the auto-continue warning; live provider limits do not end their sessions.

`rimz workflow schema` prints the JSON Schema of `workflow.toml`.

## Loop tasks

A [loop task](./loop.md) starts a workflow with `workflow = "<name>"` in place of `agent` or `wait`:

```toml
[tasks.babysit-pr]
workflow      = "babysit-pr"
each-worktree = true
when          = ["pr=open"]
for           = "3m"
```

| Field | Meaning on a workflow task |
| --- | --- |
| `workflow` | The definition name under `~/.rimz/workflows/`. Conflicts with `agent` and `wait`. |
| `prompt`, `prompt-file` | The run's prompt. |
| `each-worktree` | Expand each fire over RimZ-owned checkouts. With `when`, launch once per checkout. A worktree-scoped run without it scopes to the task's `dir`, else its root; a project-scoped condition launch covers all checkouts permanently, while other triggers skip only a live held run. |
| `when`, `for`, schedules, `root` | As on any task. |
| `budget`, `budget-per-day` | Ignored: a workflow fire is not gated on spend, and step costs are not counted against the task. The turn, room, and account caps still park a step agent like any agent in the room. |

`loop show` joins each checkout to its recorded run, finished runs included. `loop list` keeps only active and parked runs; its launched-worktree count includes finished entries.

`stay`, `takeover`, `subscribe`, `check`, and `judge` are refused, each with the workflow node that owns the concept. A `when` task launches its workflow once per checkout: subsequent scheduled fires start nothing and record nothing, whether that run is active, parked, or finished. Project-scoped launches cover all of the task's checkouts, including later ones. `rimz loop fire <task>` bypasses this memory and replaces the entry only when a new run starts. `rimz workflow run` starts an independent run without changing the task's entry. Task replacement preserves the entries; rename moves them and removal clears them. Removal also detaches the task's active and parked workflow runs, which continue as independent runs.

An unrecorded checkout retries a sustained-true condition every five minutes. A fire that finds an active or parked run records `workflow skipped` with its id and adds no strike, checking before it reads the definition or prompt. If that run belongs to this task, the fire repairs its launch entry, so later scheduled fires are silent. A run started independently or by another task is not adopted, and the task retries until that run ends. Schedule, signal, and watch workflow tasks keep admitting a new run on each fire unless a live run holds the scope; finished runs do not block them.

## Park reasons

A parked run waits for you under NEEDS YOU in `rimz loop list`, in the sidebar cockpit, and in `list` and `show`.

| Reason | Cause |
| --- | --- |
| `user` | An edge routed to `@user`; the text is the check's `reason`. |
| `cap` | A step reached `max-visits` or `max-visits-per-key`, and `on.cap` defaults or routes to `@user`. |
| `deadline` | The run's deadline passed with no `on.deadline` edge. |
| `timeout` | A check's `timeout` passed with no `on.timeout` edge. |
| `invalid` | A script check failed or printed a bad object with no `on.invalid` edge. |
| `asked` | A turn step's leader or child blocked on a question or permission prompt and `on.asked` routes to `@user`; the text is the prompt. |
| `check` | A check evaluation failed twice. |

Routes, step launches, parks, and ends append `workflow_*` assists ([stats](./stats.md#the-assist-timeline)). A merged wait appends one route assist when it closes, with its final `count`; an open wait has no route assist yet. Stats renders a closed wait as `workflow <workflow> <node> waited <outcome> ×<count>`.

## JSON shapes

`list --json` prints an array of `{id, workflow, scope, state, node, since, detail}`, where `scope` is the scope path: the checkout, or the project root for `scope = "project"`; `detail` is the waiting line, park reason, `asking you: …`, or `with you` with its checking/waiting suffixes as printed, otherwise empty. The human column remains `CHECKOUT` for both kinds.

`show --json` prints `{dir, record, journal}`: the work directory, the run record, and the history entries. An active attempt's optional `asking` is `{since, text, kind, handle}`, with `kind` one of `question`, `permission`, or `plan_approval`; `asked_ms` accumulates closed pauses. These fields pause a turn step's timeout, not the run deadline. Evaluation rows carry `origin = "check"` for a check's answer or `"engine"` for an engine-decided outcome.

Runs whose sealed spec contains `until` use `record.version = 2`; other runs remain version 1. While checking an open session, optional `held` is `{attempt, entered_at, verdict?}`: the session attempt, the step's entry time, and the saved check verdict. Optional `attempt.judged` is `{agent_id, started_at, ended_at}`, the consumed leader turn end. A verdict is `{node, entered_at, target, object, reason}`; a closing attempt carries it in optional `attempt.ending.route` until the stop settles. Older binaries refuse version 2 and skip those runs in lists.

Each consumed turn end writes a journal row with `event = "turn_end"`, the session's `seq` and step `node`, `action = "kept" | "closed" | "discarded" | "interrupted"`, `turn = {agent_id, started_at, ended_at}`, and `check`. A saved verdict supplies optional `outcome` and `target`; `reason` explains an interruption or a moved-turn discard. Text history prints the action and any outcome and target.

`work/context.json` adds `live_attempt` while an `until` session is open with no staged ending: `{step, seq, visit, visits, key, started_at, handle, run_id, claim_path, head_start, turn_end}`. `turn_end` is the last consumed turn identity, or null. There is no outcome or end evidence in `live_attempt`; `prev_attempt` still describes the last ended attempt. During evaluation, the top-level context describes the check, without `visit` or `visits`; a keep or discard rewrites it to the session's step view, while a close leaves it until the next publication.

Command-step attempts and `record.prev_attempt` carry nullable `code` (exit code, null after a signal or an unobserved exit) and `log` (the absolute `work/<seq>.step.log` path when the file exists). Agent attempts write both as null; records predating these fields still decode. Command steps have null `handle`, `run_id`, `children`, `limited`, `asked`, and `cost_usd`. The hosting process samples `head_start` before the command and `head_end` after reaping it; `head_end` is null when another process settles it. `claim_path` still names `work/<seq>.claim.json`.

A command step records `ended` on exit 0, `died` on another exit, signal, spawn failure, or log-open failure, and `timeout` when its step timeout interrupts it; these follow `then`. A run deadline records `canceled` and follows `on.deadline`. Cancel records `canceled` and ends the run. A lost host records `canceled` with a reason and follows `then`, without rerunning the action. An interrupted command whose bound is no longer due follows `then` as `canceled`. A child that exited on its own keeps its exit outcome even if a bound fell due before settlement. The next check receives the original routed input; the command's evidence is in `prev_attempt`.

Text history keeps the attempt event line and adds `    exit <code|—> HEAD <start8|—> → <end8|—>` when `code` or `log` is present, followed by `    log: <path>` when present. Any attempt with a reason adds `    reason: <text>`. A running command has no live `attempt` header row; that row belongs to supervised agent runs.

A visit cap routes `on.cap` with `{outcome: "cap", step, bound: "max-visits" | "max-visits-per-key", visits, max}`, plus the inherited `key` when present. The total cap is checked first and wins when both trip. `visits` is the tripped bound's count; `max` is its limit plus the run's visit grant. The journal's route input carries the same object.

A shell answer is `{outcome, code, log}` plus an inherited `key` when present. Exit 0 is `pass`; another exit or signal is `fail`. `code` is an integer, or null after a signal. `log` is the absolute `work/<seq>.check.log` path, containing stdout and stderr together. Only `fail` has `reason` (`exited N` or `killed by signal`); output tails are not journaled. Both `on.pass` and `on.fail` are required. The only additional routes are `on.timeout` and `on.invalid`, each defaulting to `@user`; command spawn or log-open errors are `invalid`. The check's optional timeout spans waits and evaluations, without inheriting the loop's default timeout. A timeout routes `timeout`; a run deadline follows `workflow.on.deadline`. Both stop the shell's process group rather than answering `fail`. A `fail` routed to `@wait` shows `said fail` in the waiting line.

A condition's answer has `outcome`, `readings` (a map from term key to string or null), and the winning zero-based `case` (omitted for `else` and `unknown`). A case's fixed string or integer `key`, or the inherited run key, is included when present. `unknown` includes a `reason` naming each unread key. While a case's `for` hold runs, the outcome is `hold`; `record.wait` carries `case`, `detail` (the case's outcome), `first`, `last`, `count`, and `recheck_at`. Without a hold, `wait.case` is omitted. `unknown` defaults to `@wait`; `timeout` and `invalid` default to `@user`. Explicit `on` entries override those defaults. `hold`, `unknown`, `timeout`, `invalid`, `cap`, and `deadline` are reserved outcome words, and `on.hold` is refused.

The bare-`ci` commit fence sets `readings.ci` to null. When that makes the answer `unknown`, `reason` includes one of these, using the first seven characters of each commit OID (or all of a shorter OID):

- `ci (local HEAD <l7> is not on the PR, PR head <p7>)`
- `ci (read at <c7>, PR head <p7>)`

The local-HEAD reason takes precedence over the CI-commit reason; the pre-entry-probe reason takes precedence over both. The same text follows `unknown: ` in the waiting line and JSON `detail`, and remains in the journaled evaluation answer printed by `show` history.

`rimz loop list --json` carries `workflow_runs` for a workflow task: one object per active or parked run the task started in that room, `{id, checkout, state, node, since}` plus `reason` and `text` on a parked run. Both list and show add `detail` when the waiting line or park label is nonempty. `since` is when the run entered its node.

`rimz loop show --json` also includes every run named by the task's condition launch memory, each once, including terminal runs whose task was renamed. A run whose record is gone has only `id`, `checkout` (the launch entry's key), `state: "gone"`, and `since` (the launch time). Fan-out `worktrees[].run` repeats the matching `workflow_runs` object; project-scoped launches share one run across checkout rows. The human WORKTREES table names those runs, and RUNS lists only runs no worktree row names, folded rows included.

## Exit codes

`0` on success. `1` when a command refuses (an unknown run id, a duplicate start, a terminal resume, a validation error) or fails. `2` on a usage error.
