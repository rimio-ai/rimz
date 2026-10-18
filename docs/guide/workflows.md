# Workflows

A [loop task](./loops.md) is a cron line for agents. On a clock or a signal it fires one turn, and the next firing knows nothing about the last. That fits "every morning, sweep the PRs". It does not fit "fix this branch until the tests pass, but stop after three tries", or "keep this PR alive until it merges". That logic needs memory between turns, and in a loop task it ends up in prompts and in an agent's context, where a dead or compacted agent loses it.

A workflow writes that logic down as a small graph in `~/.rimz/workflows/<name>/workflow.toml`. Each node is a check or a step. A check reads the world (a script you write, or a read-only model call) and answers with an outcome word. A step launches one agent to do work. Edges send each outcome to the next node. RimZ keeps the run's position in a durable record on disk, so a dead agent, a closed room, or a reboot costs at most one relaunch. The run keeps its place.

## Write a workflow

This workflow runs the test suite, sends an agent to fix it when it fails, and tries three times at most. It works in any repository whose tests run from one command. Create these three files:

```text
~/.rimz/workflows/fix-tests/
  workflow.toml
  checks/tests.sh
  prompts/fix.md
```

`workflow.toml` is the graph:

```toml
[workflow]
scope = "worktree"
start = "tests"

[checks.tests]
run = "sh checks/tests.sh"
on = { pass = "@done", fail = "fix" }

[steps.fix]
agent = "claude"
prompt-file = "prompts/fix.md"
then = "tests"
max-visits = 3
timeout = "1h"
```

`checks/tests.sh` is the check. It runs in the checkout and prints exactly one JSON object on stdout. The suite's own output goes to a file, so it cannot mix into the answer. Replace `cargo test` with your suite's command:

```sh
log="$RIMZ_WORKFLOW_DIR/notes/tests.log"
if cargo test >"$log" 2>&1; then
  echo '{"outcome": "pass"}'
else
  printf '{"outcome": "fail", "reason": "tests failed", "log": "%s"}\n' "$log"
fi
```

`prompts/fix.md` is what the agent receives each time the step launches:

```markdown
The test suite in this checkout fails. Read context.json in the directory
$RIMZ_WORKFLOW_DIR names: its "input" object holds the check's answer, and
input.log is the file with the failing output. Fix the code until the suite
passes, then commit. An earlier attempt may have left uncommitted changes:
inspect them and continue from them. Do not report that the suite passes.
The workflow runs it again after your turn.
```

Read the graph from `start`. The run begins at the `tests` check. An answer of `pass` goes to `@done`, which ends the run. An answer of `fail` goes to the `fix` step, which starts Claude Code in the checkout with the prompt file. When that agent's turn ends, the step follows `then` back to `tests`, and the check decides whether the work is done. The agent never decides that. Any extra field in a check's answer (here `log`) travels to the next node as its input.

Two fields bound the loop. `max-visits = 3` stops the fourth launch of `fix`: the run parks and waits for you instead. `timeout = "1h"` ends an attempt that runs longer than an hour and follows `then` as if the turn had ended. Timeouts take `s`, `m`, `h`, and `d`.

Paths in `run` and `prompt-file` are relative to the workflow's directory and must stay inside it. `run` is a command line split into words, and no shell reads it. A pipe or a variable belongs inside the script, so start it with `sh`, `uv run`, or another interpreter. `agent` takes an agent kind such as `claude` or `codex`, or the name of a [profile](./configuration.md) you defined.

## Check it and start a run

Validate the definition first. The command prints every node with the edges it resolved, including the defaults the engine adds:

```console
$ rimz workflow validate ~/.rimz/workflows/fix-tests
WORKFLOW   NODE   KIND   RUNNER              BACKGROUND  EDGES
fix-tests  tests  check  sh checks/tests.sh  —           deadline → @user (default), fail → fix (explicit), invalid → @user (default), pass → @done (explicit), timeout → @user (default)
fix-tests  fix    step   claude              yes         asked → @user (default), cap → @user (default), deadline → @user (default), then → tests (explicit)
warning: fix-tests: steps.fix: auto-continue is off; a provider limit mid-attempt ends the attempt as limited
```

Every `(default)` edge to `@user` parks the run for you. The `tests` row shows that a broken script (`invalid`) parks. The `fix` row shows that the cap (`cap`) and a question from the agent (`asked`) park too. The warning appears on a default install. Without [auto-continue](./loops.md#auto-continue), an attempt that hits a provider rate limit ends there, and the run goes on to `then`. Turn auto-continue on to resume the agent in the same attempt.

A step is a [supervised run](./scripting.md#what-a-run-does-on-your-machine), so it has the same prerequisites. The agent's reporting hooks must be installed, and a Codex step needs folder trust for the checkout. `rimz doctor` reports both.

Start the run from inside the project while its room is open (`rimz start` opens one). Give it its own RimZ worktree so the agent's edits stay off your branch. `rimz worktree new feat-x` creates one, and the [worktrees guide](./worktrees.md) covers the rest:

```sh
rimz workflow run fix-tests -w feat-x
```

`run` validates again, creates the run, and prints two lines: the run id (`wf_` followed by a UUID) and the run's work directory. The first check starts in the background. Without `-w` the run uses the checkout you are in. `-w` selects an existing RimZ worktree and never creates one. Each workflow holds at most one active or parked run per checkout, so a second `run` there refuses and names the run that holds it.

`--prompt 'keep the public API unchanged'` adds text to the end of every step's prompt for this run.

## Watch a run

```sh
rimz workflow list                 # every active and parked run, with its node and what it waits on
rimz workflow show wf_0192...      # one run: position, visit counts, and its full history
```

`show` lists each check's answer and each attempt in order. An attempt line names the agent's handle, the commit before and after, and its cost, so you can see what each visit changed. While a step runs, its agent is an ordinary agent in the room. Its card appears in the sidebar, and you can watch or answer it in its pane. The [workflow reference](../reference/cli/workflow.md#inspect-runs) lists every field the two commands print.

## What a run does on your machine

`rimz workflow run` copies `workflow.toml` into the run's own directory, `~/.rimz/ws/<workspace-dir>/owned/workflows/<run-id>/`. That directory holds:

- `record.json`, the run's current position, which is the truth the engine reads.
- `journal.jsonl`, the append-only history `show` prints.
- `workflow.toml`, the sealed copy. Editing the original changes later runs, never this one.
- `work/`, the only directory your nodes may write. `RIMZ_WORKFLOW_DIR` points here. It starts with `notes/` and `signs/`, and holds `context.json`.

RimZ rewrites `work/context.json` before every check and every attempt. It names the run, the node, and this visit, carries the input from the last check, and describes how the last attempt ended. [The context file](#the-context-file) lists its fields.

A check runs as a child process in the checkout, with no stdin, and `RIMZ_WORKFLOW_DIR` and `RIMZ_WORKFLOW_RUN` set. RimZ reads its stdout once it exits. A script that exits nonzero or prints anything other than one JSON object routes to `invalid`.

A step launches its agent in a new pane, with the prompt file, then `signs/<step>.md` when it exists, then the run's `--prompt`. The room's turn, room, and account [budgets](./budget.md) apply to it like any agent. When its turn ends, RimZ stops the agent and its subagents, records the commit before and after, and follows `then`. A peer or a team the agent launched keeps running.

A step will not start in a checkout another agent is using. It waits until the checkout is free. With `takeover = true` it stops an idle agent there to make room, but an agent in the middle of a turn still makes it wait.

The room's scheduler checks every active run about once a minute. A [signal](./loops.md#signals-the-rooms-event-bus) such as `pr.merged` wakes waiting checks at once. With the room closed, the run advances only while the [loop timer](./loops.md#who-keeps-time) ticks for the project. Your `~/.rimz/workflows/` directory is never written.

## Wait on the outside world

Some checks can only say "not yet". A PR whose CI still runs is one. Route that outcome to `@wait`, and the run stays at the check and asks again later. This workflow keeps one worktree's PR alive until it merges:

```toml
# ~/.rimz/workflows/babysit-pr/workflow.toml
[workflow]
scope = "worktree"
start = "pr"

[checks.pr]
run = "uv run checks/pr.py"
on = { merged = "@done", ci-pending = "@wait", needs-sweep = "sweep" }
timeout = "2d"

[steps.sweep]
agent = "claude"
prompt-file = "prompts/sweep.md"
takeover = true
then = "pr"
max-visits = 5
timeout = "6h"
```

`checks/pr.py` asks `gh` about the branch's PR and prints one of three answers:

```json
{"outcome": "merged"}
{"outcome": "ci-pending", "recheck_at": "2026-10-18T14:40:00Z"}
{"outcome": "needs-sweep"}
```

While CI runs, `ci-pending` holds the run at `pr` until `recheck_at`, and then the check runs again. Without `recheck_at`, a waiting check runs again about every minute. A signal clears `recheck_at` and runs the check at once, so a merge is seen when the room sees it. When CI fails, `needs-sweep` launches the `sweep` agent in the worktree to fix the failures and push. `takeover = true` lets it stop an idle agent you left in that worktree. However the agent's turn ends, `then` sends the run back to `pr`, which reads the PR again.

A check that can wait must have a `timeout`. It counts from the moment the run reaches the check, across every wait, and when it passes the run parks. The [RimZ repository's copy](../../crates/rimz/tests/fixtures/workflows/babysit-pr/checks/pr.py) of `pr.py` also waits ten minutes after a dead attempt and waits for the reset after a rate limit, both from `context.json`.

## Let a model decide

Some decisions need judgment that a script cannot give: whether review comments need a code change, or whether a diff matches its issue. A check with `agent` in place of `run` is a judge. It runs the agent headless with automatic permissions, and its prompt file names the outcomes it may return:

```toml
[checks.review]
agent = "claude"
prompt-file = "checks/review.md"
window = "5h.left>=40"
on = { approve = "@done", revise = "fix" }
timeout = "1d"
```

RimZ tells the judge where `context.json` is and where to write its answer, which must be one JSON object of the same shape a script prints. A judge must leave the checkout alone. Under sandbox isolation RimZ mounts the checkout and its Git directories read-only for it. Under host isolation only the prompt tells it so. `rimz workflow validate` refuses a judge whose agent has no headless mode.

`window` holds a judge or a step until the provider's usage window has room. `5h.left` and `7d.left` are the percent left in the 5-hour and 7-day windows, compared with a whole percent, as in [loop conditions](./loops.md#wait-for-room-in-a-usage-window). While it holds, `rimz workflow list` shows `window 5h.left=12<40`.

Put the cheap case in a script and the unclear case in a judge. A script that answers `unsure` and routes that to a judge pays for a model only when it has to.

## When a run needs you

A run parks when it would otherwise have to guess:

- a step reached `max-visits`,
- a check's `timeout` passed,
- a script failed or printed a bad answer,
- a judge failed twice,
- a step's agent asked a question or hit a permission prompt that no `on.asked` edge answers,
- the run's `deadline` passed,
- a check routed an outcome to `@user`, with its `reason` as the text.

`[workflow] deadline = "2026-11-01"` bounds the whole run. It takes a date (local midnight), a local date and time, or an RFC 3339 instant. When it passes, RimZ stops a live attempt and parks the run, unless `on.deadline` routes it somewhere else.

A parked run appears in three places: a line in the [sidebar cockpit](../interface/sidebar.md), the NEEDS YOU section of `rimz loop list` when a loop task started it, and `rimz workflow list` with its reason. The [park reasons table](../reference/cli/workflow.md#park-reasons) gives the exact word for each case.

Fix what the reason names, then send the run back to work:

```sh
rimz workflow resume wf_0192...                          # the same node, with a fresh timeout
rimz workflow resume wf_0192... --at tests               # another node
rimz workflow resume wf_0192... --extend visits=+2       # two more visits for every step
rimz workflow resume wf_0192... --extend deadline=2026-11-08
```

An extension applies to this run only. The definition does not change.

## Stop a run and clean up

```sh
rimz workflow cancel wf_0192...
```

`cancel` ends the run as `canceled` and stops whatever it is running: the check's script, the judge, or the step's agent and its subagents. The commits and uncommitted edits in the checkout stay, and so does the run's directory. Remove the worktree when you are done with it ([cleanup](./worktrees.md#cleanup-once-work-lands)).

A run also ends on its own at `@done`, or as `failed` at `@failed`. A finished run leaves `rimz workflow list`, and its directory stays for seven days, where `rimz workflow show` can still read it. Then RimZ's garbage collection removes it. `rimz reset` keeps run directories, and `rimz reset --hard` removes them.

## Start runs from a loop

A loop task can start the runs for you. This one starts a `babysit-pr` run in every RimZ worktree with an open PR:

```toml
[tasks.babysit-pr]
workflow      = "babysit-pr"
each-worktree = true
when          = ["pr=open"]
for           = "3m"
```

Each firing starts a run in a worktree that has none. A worktree with an active or parked run is skipped, so it never gets two. After the merge, `pr=open` turns false and the task stops firing there. Write this task by hand in `loop.toml`, since `rimz loop add` has no flag for it. The task's own `budget` does not apply, because the steps are ordinary agents under the room's budgets. The [loop task reference](../reference/cli/workflow.md#loop-tasks) lists the fields a workflow task takes.

## One run per project

Some work is project-wide: a monthly dependency sweep, or a loop that takes the next item off a shared backlog. Set `[workflow] scope = "project"` for it. Every node then runs at the project root, and the workflow holds one active or parked run for the whole project. Starting it from a worktree still selects the root, and `-w` prints a notice that says so. A loop task with `each-worktree` starts one run and skips the other worktrees.

A step at the root follows the same rule as in a worktree: agents working at the root make it wait, and `takeover = true` may stop the idle ones. Agents in RimZ worktrees do not count.

## Rules for writing nodes

The engine stays simple because nodes follow four rules.

1. **Make every node safe to repeat.** A step launches the same prompt on every visit and learns how far things got from the world: the repository, the PR, `$RIMZ_WORKFLOW_DIR/notes/`. A check can run twice for one decision, because the process hosting it can die after the script ran.
2. **Say what to do with a dirty tree.** A stopped attempt can leave uncommitted work behind. The step's prompt tells the agent to keep it, commit it, or discard it.
3. **Tune a step through its sign, never its prompt file.** A judge that wants to steer the next attempt writes `$RIMZ_WORKFLOW_DIR/signs/<step>.md`, and RimZ adds it after the step's prompt file. The prompt file is shared by every run of the workflow.
4. **Keep judges read-only.** A judge reads and answers. Work belongs in a step.

`notes/` is scratch for your nodes, and RimZ never reads it. For example, a check that walks a list can record the items it finished there. To make a retry harmless, name each write after the `seq` in `context.json`: a retry has the same `seq`, so it overwrites its own file.

### The check answer

A script prints, and a judge writes, exactly one JSON object:

| Field | Required | Meaning |
| --- | --- | --- |
| `outcome` | yes | The edge to follow, from the check's `on` table. |
| `reason` | no | Text for the history, and the park text when the outcome routes to `@user`. |
| `recheck_at` | no | An RFC 3339 time. On `@wait`, nothing runs the check again before it, unless a signal arrives. |
| `key` | no | A string or integer naming the item the run now works on. It appears as `key` in later context files. |

Any other field goes to the next node as `input`. The edge targets are a node name, `@wait`, `@done`, `@failed`, and `@user`. A step's `then` must name a check. `rimz workflow schema` prints the JSON Schema of `workflow.toml`.

### The context file

| Field | Meaning |
| --- | --- |
| `run`, `node`, `seq` | The run id, the current node, and this evaluation's sequence number. |
| `prompt` | The run's `--prompt`, or null. |
| `input` | The answer that routed here, or the engine's object after a cap, timeout, or deadline. |
| `key` | The current item key, or null. |
| `visit`, `visits` | Steps only: this step's visits for the current key, and its visits in total, both including this launch. |
| `output_path` | Judges only: the file to write the answer to. Null for scripts and steps. |
| `retry` | Judges only: set when the judge runs again after a failed first try, with that try's outcome and reason. |
| `signs_path` | The `signs/` directory. |
| `prev_attempt` | How the last step attempt ended, or null. |

`prev_attempt.outcome` is `ended`, `died`, `timeout`, `asked`, `limited`, or `canceled`. The object also carries the attempt's start and end times (`started_at`, `ended_at`), the agent's handle, the commit before and after (`head_start`, `head_end`), its cost, and, when they apply, the question it asked (`asked`) and the provider's reset time (`limited.reset_at`).

## See also

- [Workflow CLI](../reference/cli/workflow.md): every command and flag, the waiting line, park reasons, and the JSON shapes.
- [Loops](./loops.md): loop tasks that start runs, signals, and window conditions.
- [Scripting](./scripting.md): the supervised run every step is.
- [Workflow internals](../internals/harness/workflow.md): how the engine decides, for readers of the code.
