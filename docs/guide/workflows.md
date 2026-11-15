# Workflows

A [loop task](./loops.md) runs one command when its trigger fires, and [`rimz agents -p --verify`](./scripting.md#verify-and-retry) re-prompts one agent until a check passes. Those two cover "sweep the PRs every morning" and "fix until the tests pass". They do not cover work in stages that waits on the world between them: keep this PR alive until it merges, launch a fixer each time CI fails, stop after five tries, and hand the failure to someone who can read it. Written as a prompt, that plan lives in one agent's context, and a crashed or compacted agent loses it. Written as a shell `while` loop, it dies with the terminal and cannot wait on a PR for two days.

A workflow writes the plan down as a small graph in `~/.rimz/workflows/<name>/workflow.toml`. Steps do work: they launch an agent, run a command, or message an agent. Checks decide: they run a test, read the PR, or ask a model, and answer with an outcome that picks the next node. RimZ keeps the run's position in a record on disk, so the run continues from the same node after an agent dies or the machine reboots.

## Write a workflow

The smallest useful workflow runs the test suite, sends an agent to fix it when it fails, and gives up after three tries. A `--verify` launch could do this one alone. It comes first because every larger graph uses the same parts. Create two files:

```text
~/.rimz/workflows/fix-tests/
  workflow.toml
  prompts/fix.md
```

`workflow.toml` is the graph:

```toml
[workflow]
scope = "worktree"
start = "tests"

[test.tests]
shell = "cargo test"
timeout = "30m"
on = { pass = "@done", fail = "fix" }

[agent.fix]
agent = "claude"
prompt-file = "prompts/fix.md"
lifecycle = "turn"
then = "tests"
max-visits = 3
timeout = "1h"
```

`prompts/fix.md` is what the agent receives each time the step launches:

```markdown
The test suite in this checkout fails. Run `rimz workflow show --history` to read the check's answer and log path, and read your working-memory file named in the Workflow note. Fix the code until the suite passes, then commit. An earlier attempt may have left uncommitted changes: inspect them and continue from them. Do not report that the suite passes. The workflow runs it again after your turn.
```

RimZ adds a `### Workflow` section to every step agent's launch reminder, the instructions it gives an agent at launch. That section shows the graph, why the run arrived at this step, and the name of the step's working-memory file: a notes file the agent keeps across attempts ([notes](#rules-for-writing-nodes)).

Read the graph from `start`. Each node is a table named `[<kind>.<name>]`, and its edges name other nodes by name.

- `scope = "worktree"` gives each checkout its own run. `start` names the first node.
- `[test.tests]` is a check. It runs `shell` through `sh -c` in the checkout. Exit 0 answers `pass` and any other exit answers `fail`. Replace `cargo test` with your suite's command.
- `on` sends each answer somewhere. `@done` ends the run as done. `fail` goes to the `fix` step.
- `timeout` bounds the check. Every check needs one, so a hung command cannot hold the run forever.
- `[agent.fix]` is a step. It starts Claude Code in the checkout with the prompt file. `agent` takes an agent kind such as `claude` or `codex`, or a [profile](./fleet.md#profiles-shape-an-agent-for-one-job) you defined.
- `lifecycle = "turn"` makes the attempt one turn. When the agent's turn ends cleanly, RimZ stops the agent and follows `then` back to `tests`.
- `max-visits = 3` refuses a fourth launch of `fix`. `timeout = "1h"` ends an attempt that runs longer than an hour.

The check decides whether the work is done, never the agent. The check's answer becomes the next node's input, and the agent reads it with `rimz workflow show --history`, as the prompt says. RimZ does not expand variables in the prompt text.

This graph routes no failure, so a fixer that dies, times out, or uses up its three visits ends the run as failed. [When something fails](#when-something-fails) shows how to route failures to a node of your own.

## Check it and start a run

A workflow agent step is a [supervised run](./scripting.md#what-a-run-does-on-your-machine) underneath, so it needs what any RimZ launch needs. Before the first run:

1. Install the agent's reporting hooks with `rimz hooks install claude` (or your kind), if `rimz setup` did not.
2. Run `rimz doctor` in the project. When it reports the folder as undecided (not yet trusted) for your account, run the `rimz trust grant` command it prints. Problems it lists for agent kinds you do not use do not block a run.

Validate the definition. The command prints every node with the edges it resolved, including the defaults RimZ adds:

```console
$ rimz workflow validate ~/.rimz/workflows/fix-tests
WORKFLOW   NODE   KIND   RUNNER      BACKGROUND  EDGES
fix-tests  tests  test   cargo test  —           fail → fix (explicit), pass → @done (explicit), died → @failed (default), timeout → @failed (default), deadline → @failed (default, only with a deadline)
fix-tests  fix    agent  claude      yes         then → tests (explicit), died → @failed (default), timeout → @failed (default), limited → @failed (default), visits → @failed (default), capacity → @failed (default), deadline → @failed (default, only with a deadline)
warning: fix-tests: agent.fix: auto-continue is off; a step paused on a provider limit waits for you
```

The edges marked `explicit` are the ones you wrote. Every failure edge reads `(default)` and goes to `@failed`, since the graph routes no failure. A `deadline` edge fires only when the run has a [deadline](#when-something-fails). `BACKGROUND` says whether the agent reports work it left running in the background. The warning appears while [auto-continue](./loops.md#auto-continue) is off, which is the default: a step that hits a provider rate limit keeps its agent open and waits for you in its pane. Every field is checked, so a typo or a field another kind owns refuses with its dotted path, such as `agent.fix.max-visits`. The [validation reference](../reference/cli/workflow.md#validate-and-schema) lists the rules.

Then start the run:

1. Open the project's room with `rimz start`. The room is the multiplexer session RimZ manages for the project. Run the next commands from the project's directory, in the room or in another terminal.
2. Create a RimZ worktree, so the agent's edits stay off your branch. If the suite passes there, the run ends at `@done` at once, so break a test first to watch the loop. The [worktrees guide](./worktrees.md) covers what this creates.

   ```sh
   rimz worktree new feat-x
   ```

3. Start the run in that worktree, and keep its id:

   ```sh
   RUN=$(rimz workflow run fix-tests -w feat-x | sed -n 1p)
   ```

`run` validates again, prints any warnings to stderr, and prints three lines: the run id (`wf_` and 32 hexadecimal digits), the run's work directory, and a `rimz workflow show` command for the run. The first node starts in the background. When the first step cannot start yet (no capacity, a closed usage window, an agent already in the checkout), a fourth line says what it waits for. The work directory moves when the run ends ([Stop a run and clean up](#stop-a-run-and-clean-up)).

Without `-w` the run uses the checkout you are in. `-w` selects an existing RimZ worktree and never creates one. A workflow holds at most one unfinished run per checkout, so a second `run` there refuses and names the run that holds it. `--prompt 'keep the public API unchanged'` adds text to the end of every agent step's prompt for this run.

## Watch a run

Four commands cover watching:

```sh
rimz workflow list                   # every unfinished run, with what it waits on
rimz workflow show "$RUN"            # one run: where it is, the graph, a short trail
rimz workflow show "$RUN" --history  # every check answer and attempt, repeats folded
rimz workflow show fix-tests         # the definition as a graph, then its live runs
```

Read a definition's routes as a graph before you start a larger one. `show` with a workflow name draws the graph from `start`, hangs a returning step beneath the check that sends it, and keeps each limit beside its node. Checks draw `◇` and steps `○`. `→` points down the graph and `↑` back up it:

```console
$ rimz workflow show fix-tests
fix-tests
one run per worktree · no loop task starts it; from a loop: rimz loop add '<task>' --each-worktree --every '<duration>' -- rimz workflow run fix-tests · by hand: rimz workflow run fix-tests -w '<worktree>'

◇ tests  check  sh: cargo test · waits ≤30m, then @failed
│   fail → fix · pass @done
│
├─○ fix  step  claude · 1 turn ≤1h · ↑ tests · 3 visits, then @failed
│

A turn step goes on to its next check when its turn ends cleanly; one whose provider exits first died.
An unrouted failure ends the run as failed.
No deadline is set; an unrouted deadline granted to a run ends it as failed.
one node in full: rimz workflow show fix-tests tests · source ~/.rimz/workflows/fix-tests/workflow.toml

no active or parked runs of fix-tests
```

`rimz workflow show fix-tests fix` prints one node in full, with every resolved edge, including the default ones the graph leaves out.

`list` names each run by an address such as `fix-tests#feat-x`: the workflow, then the checkout's directory name, or `main` for the project root. Use that address in place of `$RUN` in `show`, `resume`, `restart`, and `cancel`. The runs that need you come first, under NEEDS YOU: an agent step with a question open, and a capacity hold that needs a fix. The other runs group by workflow. The two captures below come from a test room: its checkout is named `probe`, its test was `false`, and other agents held the checkout, so `fix` could not start:

```console
$ rimz workflow list
fix-tests · 1 active · started by hand
  ☾ #probe  occupied  36s
      ◆ tests
      ╰─ ☾ fix ↑
```

`☾` marks a node that waits, here for the checkout to be free ([Share the checkout](#share-the-checkout)). `show` on the run starts with where the run is and what it waits for. Below that it paints the graph from the run's own copy of the definition: filled glyphs (`●`, `◆`) for nodes the run reached, hollow ones for nodes it has not, and each reached node's last answer beside it. A trail of the last routing decisions follows, then the work directory. The output here stops at the work directory, after which `show` prints the notes path and the `--history` command:

```console
$ rimz workflow show 'fix-tests#probe'
fix-tests#probe
☾ fix occupied
started 03:15 by hand · 0 attempts

◆ tests  check  sh: false · waits ≤30m, then @failed · fail · 03:15
│   fail → fix · pass @done
│
├─☾ fix  step  claude · 1 turn ≤1h · ↑ tests · 3 visits, then @failed · since 03:15
│
trail  03:15 tests fail → fix · now
work dir ~/.rimz/ws/room-770e/owned/workflows/wf_01a1de7d104d7480a6ce0e557f583b77/work
```

Read the second line first: it names the node the run is at and why it is not moving. `◆ tests` is filled because the run reached it, and `fail · 03:15` is its last answer. While an agent step runs, its agent has a [card](./sidebar.md) in the sidebar and a pane you can watch, and the sidebar draws the run as a [workflow strip](../interface/sidebar.md#workflow-strips) above its checkout's cards. The [run view reference](../reference/cli/workflow.md#inspect-runs) explains every mark.

From inside a workflow step, or one of its subagents, omit the run: `rimz workflow show` finds the step's own run.

Keep the room open while a run works. The room checks every unfinished run about once a minute, and that check moves a waiting run on. With no room open, only the [loop timer](./loops.md#who-keeps-time) does it, if you installed one.

## What a run does on your machine

`rimz workflow run` creates the run directory `~/.rimz/ws/<workspace-dir>/owned/workflows/<run-id>/`, with these entries:

- `record.json` holds the run's current position. It is the truth the engine reads.
- `journal.jsonl` is the append-only history behind `show --history`.
- `workflow.toml` and `definition/` are a sealed copy of the definition folder, made when the run starts. Every prompt and script path is read from this copy, so an edit under `~/.rimz/workflows/<name>/` changes later runs, never this one. Agent profiles are not part of the copy: each launch reads the profile as it is then.
- `work/` is the work directory. It holds `context.json`, the logs of each check and command, judges' answers, `signs/`, and a `notes/` link to the run's kept notes.
- `history.txt` appears when the run ends: the `--history` view, saved.

Nothing writes to `~/.rimz/workflows/`. No daemon keeps the run. Each move is a short `rimz workflow advance <run-id>` process: it reads the record, decides the next node, writes the record again, and exits. `run`, `resume`, and `restart` start one, and so do a matching signal and the end of a step's turn. The room's minute check starts one for every run that is due. When an advance dies, the next one picks up from the record. An interrupted check runs again. A command step whose host died fails as `died`, so its action never runs twice by accident.

A test or script runs as a child process in the checkout, with no stdin and with `RIMZ_WORKFLOW_DIR` (the work directory), `RIMZ_WORKFLOW_RUN` (the run id), and `RIMZ_WORKFLOW_DEFINITION` (the sealed definition folder) set. Its output goes to a log in `work/`, and the log's path travels with its answer. A condition reads RimZ's own state in the advance and runs no process. A check's `timeout` counts from the moment the run enters it, across every evaluation and wait there. A timeout, the run's deadline, or a cancel stops the check's whole process group.

An agent step launches its agent in a new, unfocused tab titled `run <kind>`, as `rimz agents -p` would. The agent's prompt is the prompt file, then the step's sign if one exists ([signs](#rules-for-writing-nodes)), then the run's `--prompt`. Its launch reminder's `### Workflow` section also names the visits used and what the check after its turn will run. A `turn` step that ends cleanly stops the agent and its subagents, records the commit before and after, and follows `then`. A failed or interrupted turn, a provider limit, or a budget park keeps the agent open, and a later clean turn completes the attempt. A provider that exits before a clean turn end fails the step with cause `died`. The room's [budgets](./budget.md) apply to the agent as to any other.

## Stay with an agent across turns

A `turn` step suits work an agent finishes alone. For work you want to steer, set `lifecycle = "session"`: the agent stays open across turns, and you talk to it in its pane. `rimz workflow list` reads `with you in @<handle>` for that run. Questions stay on the agent's [card](./sidebar.md), and you answer them in its pane.

A session ends when its provider process exits. When you quit the agent after a clean turn, with nothing still running, the attempt ends and the run follows `then`, so the check judges the work. An exit at any other time, such as mid-turn or with a question open, fails the step with cause `died`. The [agent step reference](../reference/cli/workflow.md#agent-steps) lists exactly what counts as an exit at rest.

A session that should end when the work is done, rather than when you close it, names the check that decides in `rest-check`:

```toml
[agent.fix]
agent = "claude"
prompt-file = "prompts/fix.md"
lifecycle = "session"
rest-check = ["tests"]
then = "tests"
max-visits = 5
timeout = "6h"
```

Each time the agent's turn ends cleanly and it is at rest, RimZ runs `tests`. Where the answer routes decides what happens to the session:

- A route back to `fix` sends the answer to the agent as its next prompt in the same session, and counts one visit. The prompt names the check, its outcome, and its log.
- A route to `@wait` keeps the session open at the check without spending a visit. The check runs again on its [wake signals](#wait-on-the-outside-world) and about once a minute, while the agent stays at rest.
- Any other route closes the session and the run goes there. Here, a passing suite goes to `@done`.

A new turn by the agent abandons a check in flight, and the next clean rest is judged again. `rest-check = ["tests", "review"]` puts a cheap check before a costly one, such as a [judge](#let-a-model-decide), without closing the agent between them: route `tests`'s passing outcome to `review`. The step's `timeout` keeps running while the session rests at its check, so it still ends a session that never satisfies the check.

Set `keep = true` on an agent step of either lifecycle to leave its agents open, outside the run, when the attempt finishes on its own. The workflow moves on and no longer owns them. A cancel, a restart, the step's timeout, and an agent that died still stop them.

## Share the checkout

An agent step does not start in a checkout another agent is using. Its `admission` table says what it does instead. With the default `occupancy = "wait"`, it waits until the checkout is free. With `occupancy = "takeover"`, it stops an idle agent there to make room, and still waits for an agent in the middle of a turn. Agents in other worktrees do not count.

A step that only reads the checkout, such as a reviewer, does not need it to itself. With `occupancy = "share"` it launches beside whoever is there and never stops anyone:

```toml
[agent.review]
agent = "reviewer"
lifecycle = "turn"
prompt-file = "prompts/review.md"
admission = { occupancy = "share" }
then = "verdict"
```

A sharing agent still occupies the checkout, so a later step that does not share waits for it to leave.

## Do mechanical work without an agent

Once a check sees green CI, merging the PR is a command, not a reasoning task. A command step runs it without launching or paying for an agent:

```toml
[command.merge]
run = "gh pr merge --auto"
then = "pr"
timeout = "1m"
max-visits = 3
```

Route the green outcome to `merge`, and let a `pr` check (a condition, as in the [next section](#wait-on-the-outside-world)) decide whether the PR merged or still needs to wait. `then` names the check that judges the command's effect. A command step takes exactly one of `run` or `shell`, and a `timeout`:

- `run` is split into words and started with no shell. A path in it (a word holding `/`) must stay inside the workflow folder, so a script you ship beside `workflow.toml` runs from the sealed copy. Start the command with `sh`, `uv run`, or another interpreter when it needs a pipe or a variable.
- `shell` runs through `sh -c`, with any path.

Tests and scripts take the same two fields with the same rules, so `run = "sh checks/status.sh"` runs a script from the definition folder. The command runs in the checkout with no stdin, and its output goes to a numbered `.step.log` file in `work/`. Exit 0 follows `then`. Any other exit, or a signal, fails the step with cause `died`.

Make the command safe to repeat, and route a cycle through a waiting check. A command returns at once, so a route straight back to it can spend its whole visit cap in one advance.

### Hand work to an agent that is already running

A planner or a lead often already sits in the room with the context a workflow's result needs. A fresh agent costs a cold start and lacks that context. A message step queues one message for the running agent and moves on:

```toml
[message.tell]
message = "@planner"
prompt-file = "merged.md"
then = "@done"
```

`message` takes an agent address, `@<handle>` or `@<handle>#<channel>` (a [channel](./messaging.md) narrows the handle), resolved from the run's checkout as [`rimz message`](./messaging.md) resolves it. Each visit queues one message for the agent's next turn boundary. The message opens with a line naming the workflow, the step, and the run, then carries the prompt file. The step follows `then` once the message is queued. When no single living agent matches, the step fails with cause `undelivered`. Like a command step it returns at once, so a cycle through it needs `max-visits` and a waiting check.

## Wait on the outside world

Some checks can only say "not yet". A PR whose CI still runs is one. Route that outcome to `@wait`, and the run stays at the check and asks again later. This workflow keeps one worktree's PR alive until it merges:

```toml
# ~/.rimz/workflows/babysit-pr/workflow.toml
[workflow]
scope = "worktree"
start = "pr"

[condition.pr]
summary = "Wait for merge or sustained CI trouble"
else = "pending"
on = { merged = "@done", queued = "@wait", red = "sweep", pending = "@wait", unknown = "@wait" }
timeout = "2d"
cases = [
  { when = ["pr=merged"], outcome = "merged" },
  { when = ["pr.queue=queued"], outcome = "queued" },
  { when = ["ci=failed || pr.queue=dequeued"], outcome = "red", for = "2m" },
]

[agent.sweep]
summary = "Repair CI and re-arm the PR"
agent = "claude"
prompt-file = "prompts/sweep.md"
lifecycle = "turn"
admission = { occupancy = "takeover" }
then = "pr"
max-visits = 5
timeout = "6h"
on = { died = "pr", timeout = "pr", limited = "pr" }
```

A condition is a check that reads facts RimZ already tracks, with no command to run. `summary` is an optional one-line description that `show` and the launch reminder print. Its `cases` are tried in order and the first true case wins. Each `when` array is ANDed, and each string takes the [loop condition grammar](./loops.md#wait-for-done-and-green): keys such as `ci`, `pr`, and `pr.queue`, combined with `!`, `&&`, `||`, and parentheses. When no case is true, the condition answers `else`. Every outcome needs an `on` edge.

Here a merged PR ends the run, and a queued or pending PR waits. CI that has failed, or a PR dequeued from the merge queue, for two minutes running launches `sweep`. The `for = "2m"` hold keeps a brief red from launching an agent. `occupancy = "takeover"` lets the sweeper stop an idle agent you left in the worktree. A clean turn follows `then` back to `pr`, and so does a sweep that died, timed out, or was refused by a budget, through its `on` routes, so the condition reads the PR again. A sixth sweep fails the run. The sweep prompt must re-arm the PR after a queue removal (enable auto-merge or queue it again), since a push alone does not clear `pr.queue=dequeued`.

Unread is not false. When a case before the first true case cannot read its facts yet, the condition answers `unknown`, even under `!`. Every condition routes `unknown`, and `unknown = "@wait"` waits for the evidence. Forge facts come from the room, which asks GitHub (through `gh`) or Gitea (through `tea`) about the checkout's PR. A fact read before the run reached the check does not count, and an idle repository's next read can take up to five minutes, so keep the room open. A checkout whose `origin` is on no supported forge stays `unknown` until the check's `timeout`. The [condition reference](../reference/cli/workflow.md#conditions) gives the freshness rules, and the [condition keys](../reference/cli/loop.md#conditions) list every reading.

A waiting check runs again about once a minute. A condition also wakes at once on the signals its keys produce: `ci.*` and `pr.*` for the run's checkout, `team.stage` for its team's board. A test, script, or judge declares its own signals in `wake`:

```toml
[script.review]
run = "checks/review.py"
on = { ready = "@done", pending = "@wait" }
timeout = "2d"
wake = ["ci.failed", { signal = "deploy.finished", match = { env = "prod" } }]
```

Entries take the [loop signal grammar](../reference/cli/loop.md#signals). `ci` and `pr` signals default to the run's checkout. The [wake reference](../reference/cli/workflow.md#wake-and-subscribe) gives the other defaults.

Since a check's `timeout` starts each time the run enters the check, each return from `sweep` starts a new two days.

A script answers with a JSON object, so it can say more than pass or fail: `{"outcome": "pending", "recheck_at": "2026-11-15T10:00:00Z"}` waits and asks again at that time. [The check answer](#the-check-answer) lists the fields. A step's `limited` failure routed to a script carries the provider's reset time when RimZ knows it, so the script can wait until then. The [sample `pr.py`](../../crates/rimz/tests/fixtures/workflows/babysit-pr/checks/pr.py) does this.

### Hand signals to a session

A check's `wake` re-runs the check, and tells the agent nothing. A session that stays open across turns often needs the news itself: CI failed again, a reviewer asked for changes. `subscribe` on a session step delivers each matching signal to the step's agent as a message at its next turn boundary:

```toml
[agent.fix]
summary = "Keep the PR green"
agent = "claude"
prompt-file = "prompts/fix.md"
lifecycle = "session"
rest-check = ["pr"]
then = "pr"
subscribe = ["ci.failed", { signal = "review.changes", match = { repo = "acme/app" } }]
on = { died = "pr", limited = "pr" }
```

`review.changes` stands for a signal your own tooling fires with [`rimz events emit`](../reference/cli/loop.md#signals). Entries take the same forms and checkout defaults as `wake`. A forward moves the run nowhere, and `rimz workflow show --history` prints one `signal` row for each. Forwarding stops when the attempt ends. A `turn` step refuses `subscribe`, since its turn ends where the message would land.

## Sleep between nodes

A sleep node pauses the run without an agent or a script. Its `then` is where to go when the time runs out, or when its optional `when` becomes true:

```toml
[sleep.cooldown]
sleep = "30m"
then = "fix"

[sleep.drain]
sleep = "6h"
when = ["!pr.queue=queued"]
then = "next"
```

`cooldown` pauses for 30 minutes. `drain` sleeps while the PR is queued, for at most six hours. `when` takes the same terms and unread rules as condition cases, and a `for` beside it requires the terms to hold that long. A sleep counts no visit and passes on the input it received, so the next step still reads the answer of the check before the sleep. `workflow list` reads `sleeping 12m/30m → fix`. Use a sleep node for a pause between nodes: a command that sleeps holds a process, needs a `timeout` longer than the pause, and shows as running in `workflow list`.

## Let a model decide

Some decisions need judgment a script cannot give: whether review comments need a code change, or whether a diff matches its issue. A judge is a check that asks a model:

```toml
[judge.review]
summary = "Review the diff for correctness"
agent = "claude"
prompt-file = "checks/review.md"
admission = { window = "5h.left>=40" }
on = { approve = "@done", revise = "fix" }
timeout = "1d"
```

`checks/review.md` says what to judge and what each outcome means:

```markdown
Read the review comments on this branch's PR with `gh pr view --comments`.
Answer "revise" with a one-line reason when any comment asks for a code
change that is not already on the branch. Otherwise answer "approve".
```

A judge runs headless in the checkout, with no pane. RimZ adds the answer contract to its prompt: its final message is one JSON object of the same shape a script prints. Its permission mode is automatic unless its profile sets one, so it does not stop on prompts. A judge must leave the checkout alone. Under [scoped isolation](./security.md#scoped-isolation) RimZ mounts the checkout read-only for it, and under the default host isolation only the prompt says so. A judge needs `summary`, and `validate` refuses one whose agent has no headless mode.

A judge whose process fails, or that writes no answer or a bad one, runs once more. `admission.window` holds the judge until its provider's usage window has room: `5h.left>=40` waits until at least 40 percent of the five-hour window is left, read as in [loop conditions](./loops.md#wait-for-room-in-a-usage-window).

Put the cheap case in a script and the unclear case in a judge. A script that answers `unsure`, routed to a judge, pays for a model only when it has to.

## When something fails

A node fails with a cause, such as `died` or `timeout`. The run then takes the first route set among these:

1. The node's route for that cause, such as `on.died`.
2. The node's `on.failed`.
3. The workflow's `on.failed`, set under `[workflow]`.

With none set, the run ends as failed. A failure never follows `then`. Each kind raises its own causes:

| Cause | Raised when |
| --- | --- |
| `died` | An agent fails to start or exits early, or a command or script exits nonzero. |
| `timeout` | The node's `timeout` passed. |
| `visits` | A step would pass its `max-visits`. Nothing launches. |
| `limited` | A budget refused an agent's launch or stopped it, or a judge's account could not spend or hit a provider limit. |
| `capacity` | A step gave up waiting for room to start. |
| `invalid` | A script or judge gave an answer the run cannot use, such as an outcome its `on` does not name. |
| `undelivered` | A message step found no single agent to receive it. |
| `deadline` | The run's deadline passed. |

The node a failure routes to receives it as its input: `{"outcome": "failed", "cause": "visits", "node": "fix", "reason": "fix has used 3 of 3 visits", "detail": {...}}`. The [failures reference](../reference/cli/workflow.md#failures) lists the causes of each kind and the [failure input](../reference/cli/workflow.md#the-failure-input) the members of `detail`.

To get someone involved, route the failure to an agent step of your own. It reads the failure from `rimz workflow show --history`, and it may fix the cause and send the run on, or ask you in its own pane. RimZ itself never asks you anything: every route ends the run or leads to an agent.

A question or permission prompt from an agent step's agent keeps the attempt open, and the run waits for you to answer in the agent's pane. `workflow list` shows the run under NEEDS YOU as `asking you: <handle> (<kind>): <text>`, and the step's `timeout` pauses while the question is open. Set `on.asked` on the step to end the attempt on a question instead and route it.

`deadline = "2026-12-01"` under `[workflow]` bounds the whole run. It takes a date (local midnight), a local date and time, or an RFC 3339 instant. When it passes, RimZ stops a live attempt and fails the current node with cause `deadline`, once. The node that failure routes to runs without a deadline.

A failure handler, or you, sends a run on with `resume`:

```sh
rimz workflow resume "$RUN" --at tests                   # move the run to another node
rimz workflow resume "$RUN" --at fix --extend visits=+2  # back to fix, with 2 more visits
rimz workflow resume "$RUN" --extend deadline=2027-01-08 # grant a new deadline
```

`--at` moves the run to that node with a fresh timeout, and stops an attempt live at the current node. Nothing resets a visit count, so a step that spent its visits needs `--extend visits=+N` to run again. It raises every step's `max-visits` by N, for this run only. The definition does not change.

### Reflect before a failed ending

A hand-written retry loop often leaves only its last error when it gives up. An ordinary agent step can leave an account a cold reader can follow instead. If your status script answers `retry`, `done`, or `broken`, this graph sends a broken answer, and every failure of any node, through `reflect`. Success still ends directly. Save it as `~/.rimz/workflows/fix-and-reflect/workflow.toml`, with the fixer prompt at `prompts/fix.md`, and choose a deadline for your job:

```toml
[workflow]
scope = "worktree"
start = "result"
deadline = "2027-01-15T18:00:00Z"
on.failed = "reflect"

[script.result]
run = "sh checks/status.sh"
timeout = "5m"
on = { retry = "fix", done = "@done", broken = "reflect" }

[agent.fix]
agent = "claude"
prompt-file = "prompts/fix.md"
lifecycle = "turn"
then = "result"
max-visits = 3
timeout = "1h"

[agent.reflect]
agent = "reflector"
prompt-file = "prompts/reflect.md"
lifecycle = "turn"
then = "@failed"
timeout = "10m"
on.failed = "@failed"
```

Write the status script at `checks/status.sh`. It prints one JSON object ([the check answer](#the-check-answer)) and exits 0:

```sh
#!/bin/sh
if ! cargo build -q >/dev/null 2>&1; then
  echo '{"outcome": "broken", "reason": "the build fails"}'
elif cargo test -q >/dev/null 2>&1; then
  echo '{"outcome": "done"}'
else
  echo '{"outcome": "retry", "reason": "tests fail"}'
fi
```

The workflow's `on.failed` catches every failure no node routes itself. `reflect` sets its own `on.failed`, so a reflection that fails ends the run instead of reaching itself again. The script answers `broken`, since `failed` is a reserved word.

Use a cheap [profile](./fleet.md#profiles-shape-an-agent-for-one-job) for the reflection, such as `~/.rimz/agents/reflector.md` on your [Claude kind base](../reference/definitions.md#kind-bases):

```markdown
---
description: Summarize a workflow's failed work
agent: claude
model: haiku
tools: [Bash, Read, Write]
---
```

Save this prompt as `~/.rimz/workflows/fix-and-reflect/prompts/reflect.md`:

```markdown
Run `rimz workflow show --history`. Read every step's notes in the folder named by `rimz workflow show`, including your own earlier notes if present. Write only your own reflect-notes.md there. Explain for someone arriving cold what was tried, what failed, what remains, and which changes happened outside the repository. Suggest a concrete improvement to the next run's prompt or checks. Do not modify other steps' notes or change the code.
```

Run `rimz workflow validate fix-and-reflect` before you start it. The reflection writes its notes, then `then = "@failed"` ends the run as failed, so the result stays a failure.

## Limit how many agents start

An agent step also waits for the [machine's start limits](./configuration.md#agent-start-limits) before it launches, however the run started. A held step spends no visit and retries about once a minute. `workflow list` shows it as `held` with the reason, and `show` prints how long it has waited.

By default a held step waits as long as it takes. Set `admission.give-up = "6h"` on the step to give up after six hours, or start the run with `--give-up 2027-01-15T06:00` to bound every step that sets none. Giving up fails the step with cause `capacity`, which takes the failure routes. When a run is urgent, start it with `--skip-capacity` to launch its steps past every capacity hold. A usage window or an agent in the checkout still holds them.

### Limit one workflow or step

The machine limits protect the whole fleet. When one workflow, or one step of it, needs a tighter ceiling, put the limit on it:

```toml
[workflow.limit]
max-parallel = 4
stagger = "2m"

[agent.triage.admission]
max-parallel = 3
stagger = "10m"
```

`[workflow.limit]` covers the open agent steps of every run of this workflow, across rooms. A step's `admission` limit covers that step alone. `max-parallel` caps how many such steps are open at once, and `stagger` sets the least time between two launches. A step counts from the moment its attempt starts until the attempt ends, whether its agent is working, idle, or waiting at a rest check. Its subagents and judges count for nothing. Each run obeys the limits sealed in its copy, so an edit changes new runs, not waiting ones. The [limits reference](../reference/cli/workflow.md#limits-and-capacity) gives the counting rules and the hold reasons.

## Stop a run and clean up

When a step's agent is stuck or has lost the thread, `rimz workflow restart "$RUN"` replaces it with a fresh agent, and `--continue` resumes its conversation instead. Both reset that step's visits to 1 and keep the input it was given.

`rimz workflow cancel "$RUN"` ends the run as `canceled` and stops whatever it is running: a check's command, a judge, a command step, or every agent of an agent step with its subagents. The commits and uncommitted edits in the checkout stay.

A run also ends on its own: as `done` at `@done`, or as `failed` at `@failed`. At any ending the run leaves `rimz workflow list`, and its run directory moves to `~/.rimz/ws/<workspace-dir>/owned/finished-workflows/<run-id>/`. `rimz workflow show` still reads it there for seven days. After that, garbage collection removes it, which runs about once a day per open room. At every ending RimZ saves the `--history` view as `history.txt` in the run directory and in the notes folder.

An unfinished run holds its checkout, so `rimz worktree remove feat-x` refuses while one runs there, and names the `rimz workflow cancel` command. Once the run has ended, `rimz worktree remove` takes the tree, with its usual refusals for uncommitted changes and commits that have not landed. `rimz worktree remove feat-x --force` removes the tree and then cancels the unfinished runs in it. Files your suite writes in the checkout, such as a `__pycache__/` or a coverage report, count as uncommitted changes unless `.gitignore` covers them. The [cleanup section](./worktrees.md#cleanup-once-work-lands) covers landing the work first.

To undo everything a workflow set up:

1. Cancel its unfinished runs: `rimz workflow cancel fix-tests#feat-x` for each row `rimz workflow list` shows.
2. Disable or remove any loop task that starts it: `rimz loop disable <task>` or `rimz loop remove <task>`.
3. Remove the worktrees you made for it: `rimz worktree remove feat-x`.
4. Delete the definition: `rm -r ~/.rimz/workflows/fix-tests`. A run already started keeps its sealed copy.
5. Delete any profile you made for it, such as `~/.rimz/agents/reflector.md`.

Finished run directories age out on their own. The notes folders stay after them, until [`gc.automation_notes_retention`](./configuration.md#garbage-collection) expires them or `rimz reset --hard` removes them.

## Start runs from a loop

A loop task can start the runs for you: its command is the `rimz workflow run` you would type. This one starts a `babysit-pr` run in every RimZ worktree of the project once its PR has been open for three minutes:

```sh
rimz loop add babysit-pr --each-worktree --when pr=open --for 3m --repeat -- rimz workflow run babysit-pr
```

Each fire runs `rimz workflow run` in the checkout that met the condition. The fire ends `completed` as soon as the run exists, and the run goes on as its own: `rimz loop stop` does not cancel it ([loop tasks reference](../reference/cli/workflow.md#loop-tasks)). `--repeat` fires a checkout again only after its condition turns false and then true again. A fire whose `rimz workflow run` finds a live run already in that checkout fails and counts a [strike](./loops.md#strikes), so give a repeating task a trigger that does not outpace its runs. Follow the runs with `rimz workflow list`.

## One run per project

Some work is project-wide: a monthly dependency sweep, or a loop that takes the next item off a shared backlog. Set `scope = "project"` under `[workflow]` for it. Every node then runs at the project root, and the workflow holds one unfinished run for the whole project. Starting it from a worktree still runs it at the root, and `-w` refuses. Start it from a loop task without `--each-worktree`: with that flag the task fires in every worktree, and every fire after the first fails with a strike while the project's run is unfinished.

## Rules for writing nodes

The engine stays simple because nodes follow four rules:

1. **Make every node safe to repeat.** An agent step launches the same prompt on every visit, and a command step runs the same command. Both learn how far things got from the world: the repository, the PR, and the run's notes. A check can run twice for one decision, because the process hosting it can die after the check ran.
2. **Say what to do with a dirty tree.** A stopped attempt can leave uncommitted work. An agent step's prompt tells the agent to keep it, commit it, or discard it, and a command handles that state itself.
3. **Tune an agent step through its sign, never its prompt file.** The prompt file is shared by every run of the workflow. A sign is a file, `$RIMZ_WORKFLOW_DIR/signs/<step>.md`, that RimZ adds after the step's prompt at every launch of this run until someone deletes it. A judge can write one to steer the next attempt, and so can you before a resume.
4. **Keep judges read-only.** A judge reads and answers. Work belongs in a step.

Each agent step gets a working-memory file, `<step>-notes.md`, in the run's notes folder, which `$RIMZ_WORKFLOW_DIR/notes/` links to and `rimz workflow show` prints. The launch reminder tells the agent to keep it for someone arriving cold, including changes outside the repository such as pushes and PR comments. The file outlives restarts and the run itself. Set `notes = false` on a step to leave the instruction out. RimZ never reads the notes. A check that walks a list can record its finished items there too, one file per item, so a repeat overwrites its own file.

A `summary` is one line describing the workflow (under `[workflow]`) or a node. `show` and the agents' launch reminders print it. A judge requires one.

### The check answer

A script prints exactly one JSON object, and a judge gives one as its final message:

| Field | Required | Meaning |
| --- | --- | --- |
| `outcome` | yes | The edge to follow, from the check's `on` table. |
| `reason` | no | Text for the history and the next prompt. |
| `recheck_at` | no | An RFC 3339 time. On `@wait`, the check does not run again before it, unless a subscribed signal arrives. |
| `key` | no | A string or integer naming the item the run now works on, such as an issue number. Later nodes see it. An answer without `key` keeps the current one. |

Any other field travels too: the whole object becomes the next node's input. A script must exit 0. A nonzero exit fails it with cause `died`, and an answer it cannot use (not one object, or an outcome its `on` does not name) fails it with `invalid`. So a script that uses `set -e` must catch the failures it expects. The outcome words `failed`, `asked`, `unknown`, `hold`, and the eight causes are reserved, and a script that answers one fails with `invalid`.

Scripts and command steps read their input from `$RIMZ_WORKFLOW_DIR/context.json`, which RimZ rewrites before every check and attempt. It carries `input` (the answer that routed here), `key`, the step's `visit` count, and `prev_attempt` (how the last attempt ended, its exit code, and the commits before and after). The [context file reference](../reference/cli/workflow.md#the-context-file) lists every field. Agents read the same facts through `rimz workflow show`.

## See also

- [Workflow CLI](../reference/cli/workflow.md): every command, flag, and field, the output of `list` and `show`, and the JSON shapes.
- [Loops](./loops.md): loop tasks that start runs, the condition grammar, and signals.
- [Scripting](./scripting.md): the supervised run each agent step's leader is.
- [Workflow internals](../internals/harness/workflow.md): how the engine decides, for readers of the code.
