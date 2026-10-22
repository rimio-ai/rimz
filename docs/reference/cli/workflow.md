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
| `--prompt <TEXT>` | Stored unchanged, appended to every step's prompt, and exposed as `prompt` in `context.json`. |

Before creating a run, the command validates the definition and refuses on any error. It needs an existing room (`rimz start` creates one). With `scope = "worktree"`, nodes run in the selected checkout; with `scope = "project"`, they run at the project root, even when invoked from an owned worktree. One active or parked run holds each `(workflow, scope path)` pair; a second start refuses and names the existing run before reading the definition or prompt. On success it prints the run id, then the run's work directory (`RIMZ_WORKFLOW_DIR`), and the first advance starts in the background. The work directory includes `notes/` for node-owned cross-node scratch and `signs/` for step guidance. Every evaluation and attempt takes a new `seq`, a retry included, so name a scratch write after the item it records to make a repeat overwrite it.

When `-w` selects a project-scoped workflow, stderr prints `note: <workflow> has scope = project; the run is at <project root>, not <worktree path>`. The command still exits 0 and stdout is unchanged.

## Inspect runs

`rimz workflow list [--json]` prints every active and parked run across rooms: `RUN`, `WORKFLOW`, `CHECKOUT`, `STATE`, `NODE`, `SINCE` (when the run entered its node), and `DETAIL`, which is the [waiting line](#the-waiting-line) for a waiting run and the [park reason](#park-reasons) for a parked one. Finished runs are omitted.

`rimz workflow show <RUN> [--json]` prints the work directory first, then `workflow`, `scope worktree` or `scope project`, `state` (with the park reason when parked), `current node` and since when, `attempt` (the live step's handle and sequence), `ask` (handle, kind, and text, on a run parked as `asked`), `deadline` (`armed` or `fired`), and `prompt`. Then the waiting line, `takeover:` lines naming the agents that hold the scope path when a step waits on takeover (`blockers unavailable` when the agent roster or the project's worktrees cannot be read), one `step <name>: N visits of <max> + <grant> grant` line per step, and the history. Each history line is time, `#seq`, node, event, outcome, count, and the routed object; an attempt adds its handle, short HEAD before and after, cost, stopped and live children, a provider reset, the ask, and the digest of each prompt, sign, and run prompt it used.

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
| `unknown: <keys and reasons>` | A condition cannot read enough data to choose its first matching case. |
| `window <term>` | A step or judge whose provider `window` is closed, with the failing term and its reading. |
| `takeover` | The checkout holds an agent mid-turn, so the step cannot evict it yet. |
| `occupied` | The checkout holds an idle agent and the step has no `takeover = true`. |
| `since` | First time the run waited here. |
| `×N` | How many advances found it still waiting. |
| `recheck` | The check's `recheck_at`, before which nothing evaluates; a signal clears it. For a `for` hold it is when the hold completes, and signals and the minute tick re-sample the case before then. |

## Cancel and resume

`rimz workflow cancel <RUN>` ends a run as `canceled` and stops its live script, shell, judge, or step attempt. A run already finished prints `already terminal; unchanged`.

`rimz workflow resume <RUN> [--at <NODE>] [--extend <GRANT>]...` puts a parked run back to work, at its current node or at `--at <NODE>`, with a fresh timeout clock and a cleared check retry. `--extend` repeats:

| Grant | Effect |
| --- | --- |
| `visits=+N` | Adds N to every step's `max-visits` for this run. |
| `deadline=<WHEN>` | Replaces the run's deadline and arms it again. `WHEN` is an RFC 3339 instant or a local date or date-time; a bare date is local midnight. |

An active run accepts `--extend` alone; `--at` needs a parked run. A finished run refuses. The definition stays sealed: grants live on the run record.

## Validate and schema

`rimz workflow validate [DIR] [--json]` checks one directory, or every directory under `~/.rimz/workflows/`. The table has `WORKFLOW`, `NODE`, `KIND`, `RUNNER` (the run command, shell text, agent profile, or `condition`), `BACKGROUND` (whether the step's agent reports background work), and `EDGES`, each edge as `outcome → target (explicit|default)`. Errors and `warning:` lines follow. Any error exits 1. `--json` prints `{rows, warnings, errors}`, each error and warning carrying `path` and `message`. Each row has `runner = "run" | "shell" | "agent" | "condition"`, nullable `run`, `shell`, and `agent` fields, `workflow`, `node`, `kind`, `do_background`, and resolved `edges`. Condition stage names use the caller's effective teams plus `Done`; without a resolved workspace, a warning says stage names were not checked. Run admission always checks against the run's project.

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
| `each-worktree` | Fire once per RimZ-owned checkout. A worktree-scoped run without it scopes to the task's `dir`, else its root; a project-scoped run always scopes to the root, so subsequent checkout fires skip the held run. |
| `when`, `for`, schedules, `root` | As on any task. |
| `budget`, `budget-per-day` | Ignored: a workflow fire is not gated on spend, and step costs are not counted against the task. The turn, room, and account caps still park a step agent like any agent in the room. |

`stay`, `takeover`, `subscribe`, `check`, and `judge` are refused, each with the workflow node that owns the concept. A fire that finds an active or parked run for its scope records `workflow skipped` with the existing run id and adds no strike, and it checks for that run before it reads the definition or the prompt, so editing either (including the scope kind) while a run holds the scope never errors a fire; a finished run does not block the next fire. While the task's `when` stays true it re-fires at the resident retry cadence and records a skipped row each time, so prefer a condition that goes false when the work is done (`pr=open`).

## Park reasons

A parked run waits for you under NEEDS YOU in `rimz loop list`, in the sidebar cockpit, and in `list` and `show`.

| Reason | Cause |
| --- | --- |
| `user` | An edge routed to `@user`; the text is the check's `reason`. |
| `cap` | A step reached `max-visits` with no `on.cap` edge. |
| `deadline` | The run's deadline passed with no `on.deadline` edge. |
| `timeout` | A check's `timeout` passed with no `on.timeout` edge. |
| `invalid` | A script check failed or printed a bad object with no `on.invalid` edge. |
| `asked` | A step's agent blocked on a question or permission prompt with no `on.asked` edge; the text is the prompt. |
| `check` | A check evaluation failed twice. |

Routes, step launches, parks, and ends append `workflow_*` assists ([stats](./stats.md#the-assist-timeline)). A merged wait appends one route assist when it closes, with its final `count`; an open wait has no route assist yet. Stats renders a closed wait as `workflow <workflow> <node> waited <outcome> ×<count>`.

## JSON shapes

`list --json` prints an array of `{id, workflow, scope, state, node, since, detail}`, where `scope` is the scope path: the checkout, or the project root for `scope = "project"`; `detail` is the waiting line or park reason as printed. The human column remains `CHECKOUT` for both kinds.

`show --json` prints `{dir, record, journal}`: the work directory, the run record, and the history entries. Evaluation rows carry `origin = "check"` for a check's answer or `"engine"` for an engine-decided outcome.

A shell answer is `{outcome, code, log}` plus an inherited `key` when present. Exit 0 is `pass`; another exit or signal is `fail`. `code` is an integer, or null after a signal. `log` is the absolute `work/<seq>.check.log` path, containing stdout and stderr together. Only `fail` has `reason` (`exited N` or `killed by signal`); output tails are not journaled. Both `on.pass` and `on.fail` are required. The only additional routes are `on.timeout` and `on.invalid`, each defaulting to `@user`; command spawn or log-open errors are `invalid`. The check's optional timeout spans waits and evaluations, without inheriting the loop's default timeout. A timeout routes `timeout`; a run deadline follows `workflow.on.deadline`. Both stop the shell's process group rather than answering `fail`. A `fail` routed to `@wait` shows `said fail` in the waiting line.

A condition's answer has `outcome`, `readings` (a map from term key to string or null), and the winning zero-based `case` (omitted for `else` and `unknown`). A case's fixed string or integer `key`, or the inherited run key, is included when present. `unknown` includes a `reason` naming each unread key. While a case's `for` hold runs, the outcome is `hold`; `record.wait` carries `case`, `detail` (the case's outcome), `first`, `last`, `count`, and `recheck_at`. Without a hold, `wait.case` is omitted. `unknown` defaults to `@wait`; `timeout` and `invalid` default to `@user`. Explicit `on` entries override those defaults. `hold`, `unknown`, `timeout`, `invalid`, `cap`, and `deadline` are reserved outcome words, and `on.hold` is refused.

`rimz loop list --json` and `rimz loop show --json` carry `workflow_runs` for a workflow task: one object per active or parked run the task started in that room, `{id, checkout, state, node, since}` plus `reason` and `text` on a parked run. `since` is when the run entered its node.

## Exit codes

`0` on success. `1` when a command refuses (an unknown run id, a duplicate start, a terminal resume, a validation error) or fails. `2` on a usage error.
