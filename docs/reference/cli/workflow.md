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
| `-w, --worktree <WORKTREE>` | Run in an existing RimZ-owned worktree of this room instead of the current checkout. It never creates one. |
| `--prompt <TEXT>` | Stored unchanged, appended to every step's prompt, and exposed as `prompt` in `context.json`. |

The command validates the definition first and refuses on any error. It needs an existing room (`rimz start` creates one) and refuses `scope = "project"`, which this version does not run. One active or parked run holds each `(workflow, checkout)` pair; a second start refuses and names the existing run. On success it prints the run id, then the run's work directory (`RIMZ_WORKFLOW_DIR`), and the first advance starts in the background.

## Inspect runs

`rimz workflow list [--json]` prints every active and parked run across rooms: `RUN`, `WORKFLOW`, `CHECKOUT`, `STATE`, `NODE`, `SINCE` (when the run entered its node), and `DETAIL`, which is the [waiting line](#the-waiting-line) for a waiting run and the [park reason](#park-reasons) for a parked one. Finished runs are omitted.

`rimz workflow show <RUN> [--json]` prints the work directory first, then `workflow`, `scope checkout`, `state` (with the park reason when parked), `current node` and since when, `attempt` (the live step's handle and sequence), `ask` (handle, kind, and text, on a run parked as `asked`), `deadline` (`armed` or `fired`), and `prompt`. Then the waiting line, `takeover:` lines naming the agents that hold the checkout when a step waits on takeover (`blockers unavailable` when the multiplexer cannot answer), one `step <name>: N visits of <max> + <grant> grant` line per step, and the history. Each history line is time, `#seq`, node, event, outcome, count, and the routed object; an attempt adds its handle, short HEAD before and after, cost, stopped and live children, a provider reset, the ask, and the digest of each prompt, sign, and run prompt it used.

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
| `window <term>` | A step or judge whose provider `window` is closed, with the failing term and its reading. |
| `takeover` | The checkout holds an agent mid-turn, so the step cannot evict it yet. |
| `occupied` | The checkout holds an idle agent and the step has no `takeover = true`. |
| `since` | First time the run waited here. |
| `×N` | How many advances found it still waiting. |
| `recheck` | The check's `recheck_at`, before which nothing evaluates; a signal clears it. |

## Cancel and resume

`rimz workflow cancel <RUN>` ends a run as `canceled` and stops its live script, judge, or step attempt. A run already finished prints `already terminal; unchanged`.

`rimz workflow resume <RUN> [--at <NODE>] [--extend <GRANT>]...` puts a parked run back to work, at its current node or at `--at <NODE>`, with a fresh timeout clock and a cleared check retry. `--extend` repeats:

| Grant | Effect |
| --- | --- |
| `visits=+N` | Adds N to every step's `max-visits` for this run. |
| `deadline=<WHEN>` | Replaces the run's deadline and arms it again. `WHEN` is an RFC 3339 instant or a local date or date-time; a bare date is local midnight. |

An active run accepts `--extend` alone; `--at` needs a parked run. A finished run refuses. The definition stays sealed: grants live on the run record.

## Validate and schema

`rimz workflow validate [DIR] [--json]` checks one directory, or every directory under `~/.rimz/workflows/`. The table has `WORKFLOW`, `NODE`, `KIND`, `RUNNER` (the script or agent profile), `BACKGROUND` (whether the step's agent reports background work), and `EDGES`, each edge as `outcome → target (explicit|default)`. Errors and `warning:` lines follow. Any error exits 1. `--json` prints `{rows, warnings, errors}`, each error and warning carrying `path` and `message`.

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
| `each-worktree` | Fire once per RimZ-owned checkout, one run each; without it the run scopes to the task root. |
| `when`, `for`, schedules, `root` | As on any task. |
| `budget`, `budget-per-day` | Ignored: a workflow fire is not gated on spend, and step costs are not counted against the task. The turn, room, and account caps still park a step agent like any agent in the room. |

`stay`, `takeover`, `subscribe`, `check`, and `judge` are refused, each with the workflow node that owns the concept. A fire that finds an active or parked run for its checkout records `workflow skipped` with the existing run id and adds no strike, and it checks for that run before it reads the definition or the prompt, so editing either while a run holds the checkout never errors a fire; a finished run does not block the next fire. While the task's `when` stays true it re-fires at the resident retry cadence and records a skipped row each time, so prefer a condition that goes false when the work is done (`pr=open`).

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

Every transition also appends a `workflow_*` assist ([stats](./stats.md#the-assist-timeline)).

## JSON shapes

`list --json` prints an array of `{id, workflow, scope, state, node, since, detail}`, where `scope` is the checkout path and `detail` the waiting line or park reason as printed.

`show --json` prints `{dir, record, journal}`: the work directory, the run record, and the history entries.

`rimz loop list --json` and `rimz loop show --json` carry `workflow_runs` for a workflow task: one object per active or parked run the task started in that room, `{id, checkout, state, node, since}` plus `reason` and `text` on a parked run. `since` is when the run entered its node.

## Exit codes

`0` on success. `1` when a command refuses (an unknown run id, a duplicate start, a terminal resume, a validation error) or fails. `2` on a usage error.
