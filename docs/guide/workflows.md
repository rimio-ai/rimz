# Workflows

A [loop task](./loops.md) is a cron line for agents: on a clock or a signal it fires one turn, and the next firing knows nothing about the last. That is the right tool for "every morning, sweep the PRs". It is the wrong one for "keep this PR alive until it merges": wait for CI, relaunch the sweeper when CI fails, stop after five tries, ask a cheap model whether the review notes need a real fix, and tell me when something needs a human. Written as loop tasks, that logic hides in prompts and in an agent's memory, and an agent that dies or compacts loses its place.

A workflow writes that logic down as a small graph. **Checks** read the world (a script printing one JSON object, or a read-only judge agent) and name an outcome. **Steps** launch one agent to do the work. Edges say where each outcome goes. RimZ keeps the run's position in a durable record, so a dead agent, a room restart, or a reboot costs at most one relaunch, never the plan.

## A run, end to end

Here is `babysit-pr`, which shepherds one worktree's pull request to merge:

```toml
# ~/.rimz/workflows/babysit-pr/workflow.toml
[workflow]
scope = "worktree"
start = "pr"

[checks.pr]
run = "uv run checks/pr.py"             # prints merged | ci-pending | needs-sweep
on = { merged = "notes", ci-pending = "@wait", needs-sweep = "sweep" }
timeout = "2d"

[checks.notes]
run = "uv run checks/notes.py"          # prints none | needed | unsure
on = { none = "@done", needed = "triage", unsure = "judge" }

[checks.judge]
agent = "haiku"
prompt-file = "checks/triage-judge.md"  # writes skip | triage
window = "5h.left>=42"
on = { skip = "@done", triage = "triage" }
timeout = "3d"

[steps.sweep]
agent = "sweeper"
prompt-file = "prompts/sweep.md"
takeover = true
then = "pr"
max-visits = 5
timeout = "6h"

[steps.triage]
agent = "yagni"
prompt-file = "prompts/triage.md"
window = "5h.left>=42"
takeover = true
then = "notes"
on.asked = "judge"
max-visits = 3
```

Read it as a loop the engine walks for you. `pr` runs `checks/pr.py`, which asks `gh` about the PR and prints `{"outcome": "ci-pending", "recheck_at": "..."}` while CI runs. `@wait` holds the run at `pr` until `recheck_at`, then asks again. When CI fails, `pr.py` says `needs-sweep`, and the `sweep` step launches the `sweeper` profile in the worktree. Whatever happens to that agent (it finishes, dies, times out after six hours, or hits a rate limit) the run follows `then` back to `pr`, which looks at the PR again. The sweeper never decides it is done: the check does. After five sweeps with no merge the run parks for you. Once the PR merges, `notes` decides whether the review left work, and only an unclear case pays for the `haiku` judge.

Check it, then start it in a worktree:

```sh
rimz workflow validate ~/.rimz/workflows/babysit-pr
rimz workflow run babysit-pr -w feat-x
```

`run` prints the run id and the run's work directory. From then on:

```sh
rimz workflow list                 # every active and parked run, with where it waits
rimz workflow show wf_0192...      # position, waiting line, and every check and attempt so far
```

## What a run does on your machine

- The run lives in `~/.rimz/ws/<workspace-dir>/owned/workflows/<run-id>/`: the record, an append-only history, a sealed copy of `workflow.toml`, and `work/`, the directory nodes may write. Your `~/.rimz/workflows/<name>/` is never written.
- Each check and step sees `RIMZ_WORKFLOW_DIR` pointing at `work/`, where `context.json` says which run, node, and visit this is, what the last check answered, and how the last attempt ended (its outcome, HEAD before and after, the subagents it left running).
- A script check runs in the worktree with no stdin. A judge runs headless with automatic permissions, and is told to leave the worktree alone. A step is an ordinary supervised agent run in a pane: the turn, room, and account [budgets](./budget.md) park it like any agent, while a loop task's own budget does not apply to a workflow task.
- A step with `takeover = true` may stop an idle agent that already occupies the worktree; one mid-turn makes the step wait. Without `takeover` any occupant makes it wait.
- The scheduler tick looks at every active run about once a minute, and a signal such as `pr.merged` wakes waiting checks early.

## Start runs from a loop

To babysit every PR without typing `run`, let a loop task start one run per worktree:

```toml
[tasks.babysit-pr]
workflow      = "babysit-pr"
each-worktree = true
when          = ["pr=open"]
for           = "3m"
```

Each firing starts a run in a worktree that has none. A worktree whose run is still active or parked is skipped, so you never get two. Once the PR merges `pr=open` goes false and the task stops firing there. Loop fields that belong to nodes (`stay`, `takeover`, `subscribe`, `check`, `judge`) are refused on a workflow task; the [loop task reference](../reference/cli/workflow.md#loop-tasks) lists what applies.

## When a run needs you

A run parks instead of guessing: a step hit `max-visits`, a check timed out, a script failed, an agent asked a question no edge answers, or a check routed to `@user` with its own reason. A parked run shows up three ways:

- in the sidebar cockpit, one line per run with its workflow, node, and reason, for the three oldest parks, then a `+N parked` count;
- under NEEDS YOU in `rimz loop list`, when a loop task started it;
- in `rimz workflow list` and `show`.

Fix what it names, then put it back to work, at the same node or another one, with more room if it ran out:

```sh
rimz workflow resume wf_0192...                         # same node, fresh timeout
rimz workflow resume wf_0192... --at pr --extend visits=+2
rimz workflow cancel wf_0192...                         # give up on this run
```

The definition stays as it was; a grant lives on the run.

## Rules for writing nodes

The engine is simple because nodes follow four rules.

- **Every node is idempotent.** A step launches the same prompt every visit and finds out how far things got by reading the world: the repo, the PR, the board, `$RIMZ_WORKFLOW_DIR/notes/`. A check may run twice for one decision (its host can die after the script ran), so its effects must be safe to repeat.
- **A relaunch may find a dirty tree.** A stopped attempt can leave uncommitted work behind, a killed subagent's included. The step's prompt says what to do with it on purpose: keep, commit, or discard.
- **Signs live in `signs/<step>.md`.** A judge that wants to tune a step writes `$RIMZ_WORKFLOW_DIR/signs/<step>.md`, which the next attempt reads after the prompt file. Never edit the shared prompt file from a run: every run of the workflow reads it.
- **Judges are read-only on the worktree.** A judge reads the world and writes its one JSON object. Work belongs in a step.

A check prints (or a judge writes to `output_path`) exactly one JSON object: `outcome` names an edge, and `reason`, `recheck_at`, and `key` are optional. Anything else rides along to the next node as `input`.

`rimz workflow schema` prints the JSON Schema of `workflow.toml`, and `rimz workflow validate --json` shows each node's resolved edges, the defaults included.

## See also

- [Workflow CLI](../reference/cli/workflow.md): every command and flag, the waiting line, park reasons, and the JSON shapes.
- [Loops](./loops.md): the task that starts runs, and the conditions that decide when.
- [Workflow internals](../internals/harness/workflow.md): how the engine decides, for readers of the code.
