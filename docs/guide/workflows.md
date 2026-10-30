# Workflows

A [loop task](./loops.md) is a cron line for agents. On a clock or a signal it fires one turn, and `--verify` can re-prompt that turn until a command passes. That covers "every morning, sweep the PRs" and "fix until the tests pass". It does not cover work in stages that waits on the world between them: keep this PR alive until it merges, launch a fixer each time CI fails, stop after five tries, and call me when something needs a human. Written as loop tasks, that plan lives in prompts and in an agent's context, and a dead or compacted agent loses it.

A workflow writes the plan down as a small graph in `~/.rimz/workflows/<name>/workflow.toml`. Each node is a check or a step. A check reads the world (ordered conditions, a shell command, a script you write, or a model call) and answers with an outcome word. A step does work: it launches a fresh agent, optionally with companions, or runs a command. Edges send each outcome to the next node. RimZ keeps the run's position in a record on disk. When an agent dies or the machine reboots, the run continues from that record. An interrupted check can run again; a command step whose host died goes to its next check rather than repeating the action.

## Write a workflow

The smallest useful workflow runs the test suite, sends an agent to fix it when it fails, and gives up after three tries. A loop task's `--verify` could do this one alone. It is shown first because every larger graph uses the same parts. Create two files:

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

[checks.tests]
shell = "cargo test"
on = { pass = "@done", fail = "fix" }

[steps.fix]
agent = "claude"
prompt-file = "prompts/fix.md"
then = "tests"
max-visits = 3
timeout = "1h"
```

`shell` runs the command in the checkout through `sh -c`. Exit 0 answers `pass`; any other exit or signal answers `fail`. RimZ saves both output streams in a per-evaluation log and gives its absolute path to the next node as `input.log`. Replace `cargo test` with your suite's command. A shell check requires both `on.pass` and `on.fail`; its only other routes are `on.timeout` and `on.invalid`. Use `run` instead when a script needs to answer more than pass or fail.

`prompts/fix.md` is what the agent receives each time the step launches. RimZ sets `RIMZ_WORKFLOW_DIR` in the agent's environment, and the agent reads it there. RimZ does not expand variables in the prompt text.

```markdown
The test suite in this checkout fails. Read context.json in the directory
$RIMZ_WORKFLOW_DIR names: its "input" object is the check's answer, and
input.log is the file with the failing output. Fix the code until the suite
passes, then commit. An earlier attempt may have left uncommitted changes:
inspect them and continue from them. Do not report that the suite passes.
The workflow runs it again after your turn.
```

Read the graph from `start`. The run begins at the `tests` check. An answer of `pass` goes to `@done`, which ends the run. An answer of `fail` goes to the `fix` step, which starts Claude Code in the checkout with the prompt file. When that agent's turn ends, RimZ stops the agent and follows `then` back to `tests`. The check decides whether the work is done, never the agent. The check's whole answer, `outcome` and `reason` included, becomes the next node's `input`. That is how `log` reaches the agent.

Two fields bound the loop. `max-visits = 3` refuses a fourth launch of `fix`, and the run parks to wait for you. `timeout = "1h"` ends an attempt that runs longer than an hour and follows `then` as if the turn had ended. Time the step's agent, or a subagent it launched, spends waiting on a blocking question or permission prompt does not count toward that hour. Durations take `s`, `m`, `h`, and `d`.

For distinct failures to get their own tries while the same failure stops retrying, add `max-visits-per-key = 2` to the step. A `run` script's answer can name the failure in `key`, and a condition case can fix its `key`; a shell check passes the run's key through unchanged. A key's count never resets during the run, even after the run works on other keys; without a key, all visits share one count. Keep `max-visits` to cap the total: a check naming a new key each time must not retry forever, and a per-key bound alone does not bound a cycle. Both fields must be positive, and when both are set the per-key bound must be below the total. The total cap is checked first and wins if both trip. Either cap follows `on.cap` (default `@user`), and its `input.bound` names the field that tripped, `"max-visits"` or `"max-visits-per-key"`, so a `run` or agent check can route the two apart. The reference gives the [whole cap input](../reference/cli/workflow.md#json-shapes).

Select exactly one of `run`, `shell`, `agent`, or nonempty `cases` on a check. Paths in `run` and `prompt-file` are relative to the workflow's directory and must stay inside it. `run` is a command line split into words, and no shell reads it. A pipe or a variable belongs inside the script, so start the command with `sh`, `uv run`, or another interpreter. `shell` accepts shell syntax and arbitrary paths; it is not confined to the workflow directory. `agent` takes an agent kind such as `claude` or `codex`, or the name of a [profile](./fleet.md#profiles-shape-an-agent-for-one-job) you defined.

Run, shell, and agent checks accept an optional `wake` list of signal subscriptions. Condition checks derive their subscriptions from their terms and refuse `wake`; see [waiting on the outside world](#wait-on-the-outside-world).

## Check it and start a run

Validate the definition first. The command prints every node with the edges it resolved, including the defaults the engine adds:

```console
$ rimz workflow validate ~/.rimz/workflows/fix-tests
WORKFLOW   NODE   KIND   RUNNER      BACKGROUND  EDGES
fix-tests  tests  check  cargo test  —           deadline → @user (default), fail → fix (explicit), invalid → @user (default), pass → @done (explicit), timeout → @user (default)
fix-tests  fix    step   claude      yes         cap → @user (default), capacity → @failed (default), deadline → @user (default), then → tests (explicit)
warning: fix-tests: steps.fix: auto-continue is off; a provider limit mid-attempt ends the attempt as limited
```

Every `(default)` edge to `@user` parks the run for you. The `tests` row says a command that cannot start or a log that cannot open (`invalid`) parks. The `fix` row says the cap (`cap`) parks too; a question stays live in the agent's pane. `BACKGROUND yes` means RimZ can see the agent's background work, so a turn that ends with background work still running does not end the attempt.

The warning appears while [auto-continue](./loops.md#auto-continue) is off, which is the default. It is about rate limits. A turn step whose leader or its subagent parks on a provider rate limit or spend limit ends its attempt as `limited`, and the run follows `then`. In `fix-tests` that means the check fails again and `fix` relaunches into the same limit, so a long limit can use up the three visits. A script check can wait for the reset instead; [the PR example](#wait-on-the-outside-world) links one. A staying step remains open on provider limits.

A Claude Console API usage cap (`You have reached your specified API usage limits`) is not a park, so it never ends an attempt as `limited`. The leader's card reads `failed`, but the attempt stays open until the leader's run fails or times out, which ends it as `died`, or the step's own timeout ends it as `timeout`. A headless Claude judge that hits this cap fails as `died`.

An agent step's leader is a [supervised run](./scripting.md#what-a-run-does-on-your-machine), so it has the same prerequisites. The leader's reporting hooks must be installed and trusted, its program must be on `PATH`, and its account must trust the checkout. An unset leader permission mode defaults to `auto`. Companion agents use the ordinary layout checks: an undecided folder warns, but saved-untrusted folders and untrusted hooks refuse the workflow launch. `rimz doctor` reports hooks and folder trust, and names the `rimz trust grant` command for an undecided folder.

Then start the run:

1. Open the project's room. The room is the multiplexer session RimZ manages for the project, and `rimz start` opens it and attaches to it. Run the next commands from the project's directory, in a shell inside that room or in another terminal. A shell in another project's room carries that project's root in its environment, and `workflow run` would look for the worktree there.
2. Create a RimZ worktree, so the agent's edits stay off your branch. The [worktrees guide](./worktrees.md) covers what this creates.

   ```sh
   rimz worktree new feat-x
   ```

3. Start the run in that worktree, and keep its id:

   ```sh
   RUN=$(rimz workflow run fix-tests -w feat-x | sed -n 1p)
   ```

`run` validates again, creates the run, and prints two lines: the run id (`wf_` and 32 hexadecimal digits) and the run's work directory. The first check starts in the background. If the suite already passes, the run ends at `@done` at once. Break a test first to watch the loop.

Without `-w` the run uses the checkout you are in. `-w` selects an existing RimZ worktree and never creates one. A workflow holds at most one active or parked run per checkout, so a second `run` there refuses and names the run that holds it. `--prompt 'keep the public API unchanged'` adds text to the end of every agent step's prompt for this run.

## Watch a run

```sh
rimz workflow list         # every active and parked run, with its node and what it waits on
rimz workflow show "$RUN"  # one run: position, visit counts, and its full history
```

`show` lists each check's answer and each attempt in order. An agent attempt names its handle, the commit before and after, and its cost. A command attempt shows its exit code, commits, and output log. While an agent step runs, its agent has a card in the sidebar and a pane you can watch. The [workflow reference](../reference/cli/workflow.md#inspect-runs) lists every field the two commands print.

An attempt can end as `died` when an agent fails to start or crashes, or when a command fails. `show` prints any recorded `reason:` beneath the attempt. A command's log holds its output; an agent's own UI carries errors that it did not report to RimZ.

## What a run does on your machine

`rimz workflow run` creates the run directory, `~/.rimz/ws/<workspace-dir>/owned/workflows/<run-id>/`, with four entries:

- `record.json`, the run's current position, which is the truth the engine reads.
- `journal.jsonl`, the append-only history `show` prints.
- `workflow.toml`, a sealed copy of the definition. Editing the original changes later runs, never this one. Scripts and prompt files are not copied: each check and launch reads them fresh from `~/.rimz/workflows/<name>/`.
- `work/`, the work directory, which `RIMZ_WORKFLOW_DIR` names. It holds `context.json`, `notes/`, `signs/`, shell-check logs (`<seq>.check.log`), command-step logs (`<seq>.step.log`), and judge answers (`<seq>.check.json`). Your nodes keep their scratch here; the run record and journal sit outside it.

RimZ rewrites `work/context.json` before every check and every attempt. It names the run, the node, and this visit, carries the `input`, and describes how the last attempt ended. [The context file](#the-context-file) lists its fields.

A script or shell check runs as a child process in the checkout, with no stdin, and with `RIMZ_WORKFLOW_DIR` and `RIMZ_WORKFLOW_RUN` set. RimZ reads a script's stdout as its answer; a shell command's output is only a log at `work/<seq>.check.log`. A condition check reads the same [condition keys](./loops.md#wait-for-done-and-green) as a loop, without a child process. A check's optional `timeout` spans evaluations and waits; shell checks do not inherit the loop's default timeout. Timeout, deadline, and cancellation stop the shell's whole process group.

An agent step's `agent` accepts the same layout grammar as `rimz agents`: a kind, profile, virtual cell such as `claude-auto`, or a layout such as `claude,nvim` or `yagni,reflect-notes`. It needs an unambiguous prompt leader and cannot name a team. Judges still take one profile. The layout opens in an unfocused new tab titled `run <leader kind>`. Every agent cell gets the same workflow environment variables; only the leader gets the composed prompt and owns the supervised run. Its prompt is the prompt file, then `signs/<step>.md` when that file exists, then the run's `--prompt`. The room's turn, room, and account [budgets](./budget.md) apply to every agent. Attempt cost counts the leader only; companion spend still appears on their cards and in stats.

For an agent step, `lifecycle = "turn"` is the default. When the leader's turn ends, RimZ stops every agent cell and its subagents, closes the command cells, records the commit before and after, and follows `then`. A companion's question or limit does not end the leader's attempt. A peer or team launched later by a cell keeps running until you stop it.

Use `lifecycle = "stay"` when you want to work with the agent across turns. The run reads `with you` in `workflow list` and `active (with you)` in `show`, or `asking you: …` and `active (asking you)` while a blocking question is open. Questions stay on the agent's card: answer in its pane. Turn ends, interruptions, provider limits, and provider session-end hooks do not end the attempt. Exiting the leader's provider process ends it as `ended` after registration, or `died` before registration, and follows `then`; a crash after registration also reads `ended`, so the check judges the work. Every layout cell closes. A staying step refuses `timeout` and `on.asked`; the workflow deadline, cancel, reset, and rebirth still end it as `canceled`.

Use `lifecycle = "until"` to work across turns until the check says you are done. At each leader turn end, once the leader is at rest with no pending work, blocking question, or provider limit, RimZ runs `then` once. A route back to this step or to `@wait` keeps the layout open without another visit; every other route closes it and follows that route without repeating the check. A kept `@wait` waits for your next turn end, not a timer or signal. While the check runs, `list` reads `with you; checking <check>`, with any waiting detail appended; a blocking question still takes precedence. If RimZ observes a newer turn when it rechecks a closing verdict, it discards that verdict without stopping your session. Provider exit, deadline, and cancel still end the session as for `stay`. Both `stay` and `until` are staying steps.

An agent step does not start in a checkout another agent is using. It waits until the checkout is free. With `takeover = true` it stops an idle agent there to make room, but an agent in the middle of a turn still makes it wait. Agents in other RimZ worktrees do not count.

When a loop starts the run automatically, its agent steps also wait for [capacity](./loops.md#pace-agent-starts), before stopping any occupant. A held step uses no visit and retries about once a minute. `list` and `show` print `waiting for capacity (claude) since 15:02`; a bounded wait also prints its give-up time. On an agent step, set `capacity-wait = "6h"` to give up after six hours, or leave the default `forever`. Giving up ends the run as failed unless `on.capacity` names `@done` or another node. It cannot park at `@user` or `@wait`. A window or occupancy wait restarts this clock, and the run deadline still applies. `throttle = "off"` skips capacity for that step. Runs you start with `rimz workflow run` or `rimz loop fire`, and runs detached by removing their loop task, are never limited; neither are command steps, checks, or an already-open staying session.

A judge runs headless, with no pane, and [the judge section](#let-a-model-decide) covers what it may touch.

While the room is open, it checks every active run about once a minute. With no room open for the project, only the [loop timer](./loops.md#who-keeps-time) does those checks, so keep the room open while a run works. Your `~/.rimz/workflows/` directory is never written.

## Do mechanical work without an agent

Once a check sees green CI, sending the PR to merge is a command, not a reasoning task. A command step does it without launching or paying for an agent:

```toml
[steps.merge]
run = "gh pr merge --auto"
then = "pr"
timeout = "1m"
max-visits = 3
```

Route the green outcome to `merge`, and let the `pr` check decide whether the PR merged or still needs to wait. Select exactly one of `agent`, `run`, or `shell` on a step. `run` passes words literally, without a shell; a quoted word with spaces stays one argument, and files named by slash-containing words must stay inside the workflow directory. Use `shell` instead for `sh -c` syntax and arbitrary paths. Both command kinds require an explicit `timeout`, so a hung command cannot hold the run.

The child runs in the checkout, in its own process group, with no stdin, the room's workspace environment, `RIMZ_WORKFLOW_DIR`, and `RIMZ_WORKFLOW_RUN`. Both output streams go to `work/<seq>.step.log`. It opens no pane, waits for no agent to leave the checkout, and takes no prompt. `prompt-file`, `lifecycle`, `window`, `takeover`, and `on.asked` belong only to agent steps; command steps read `context.json` instead. Signs and the run's `--prompt` compose only agent prompts.

Exit 0 records `ended`; another exit or a signal records `died`. A spawn or log-open failure also records `died`, with a reason. The step timeout stops the group and records `timeout`. These outcomes follow `then`, with the original routed input unchanged and the outcome in `prev_attempt`. A run deadline stops the group, records `canceled`, and follows `on.deadline`; `workflow cancel` stops the group and ends the run. If the host dies, the next advance stops any survivor, records `canceled`, and follows `then`, without repeating the action. A child that already exited keeps its exit outcome even if a bound became due before settlement.

Make the action safe to repeat on a later visit, just like an agent step. A cycle still needs `max-visits`, and it should go through a waiting check: a command returns at once, so an immediate route back can consume the whole visit cap in one advance.

## Wait on the outside world

Some checks can only say "not yet". A PR whose CI still runs is one. Route that outcome to `@wait`, and the run stays at the check and asks again later. This workflow keeps one worktree's PR alive until it merges:

```toml
# ~/.rimz/workflows/babysit-pr/workflow.toml
[workflow]
scope = "worktree"
start = "pr"

[checks.pr]
summary = "Wait for merge or sustained CI trouble"
else = "pending"
on = { merged = "@done", red = "sweep", pending = "@wait" }
timeout = "2d"

[[checks.pr.cases]]
when = ["pr=merged"]
outcome = "merged"

[[checks.pr.cases]]
when = ["ci=failed || pr.queue=dequeued"]
outcome = "red"
key = "ci"
for = "2m"

[steps.sweep]
summary = "Repair CI and re-arm the PR"
agent = "claude"
prompt-file = "prompts/sweep.md"
takeover = true
then = "pr"
max-visits = 5
timeout = "6h"
```

Cases are ordered. Each `when` array is ANDed, and its strings take the loop grammar (`!`, `&&`, `||`, parentheses, and branch selectors). The first true case wins; all false answers `else`. Every case outcome and `else` needs an `on` edge. Here a merged PR ends the run; a failing CI or dequeued PR held for two minutes launches `sweep`. `takeover = true` lets it stop an idle agent you left in that worktree. However the agent's turn ends, `then` sends the run back to `pr`.

A sweep that repairs a queue removal must also re-arm the PR (enable auto-merge or add it to the queue); a push alone does not clear `pr.queue=dequeued`.

A landing check can route conflicts and review state with `pr.mergeable` and `pr.review` instead of a script; the [condition keys](../reference/cli/loop.md#conditions) list their readings.

Unread is not false: an unread case before the first true case answers `unknown`, even for `!pr=open`. The default `unknown → @wait` waits for evidence; for `until`, it keeps the session until another eligible leader turn end. Override `on.unknown` to route elsewhere. Do not write `pr=unknown` in a case: unread terms answer the `unknown` outcome instead. An ordinary check needs a successful forge probe at or after entry to that check (including after resume). An `until` session's check instead accepts a successful probe at or after the session's start, including a probe from an earlier turn. Until a probe meets that floor, `show` names the unread keys. Stage and usage-window readings have no freshness floor. Keep the room open for fresh forge data: an idle repository's next probe can take up to five minutes. Two consecutive ordinary forge checks each need a post-entry probe, so put related decisions in cases of one check. Closing the room leaves its last facts that pass the relevant floor in force. An `until` check samples the latest probe at each eligible leader turn end; changes after the last end are sampled at the next one.

On an open GitHub PR, when the commits are known, bare `ci` also requires CI for the PR's head and the checkout's current HEAD to be on that PR. A local commit not yet on the PR, or a verdict for another PR head, makes `ci` unread. The check waits by default, and `show` says which commits differ. Someone else's push can count even when the checkout has not fetched it. Missing commit data skips only the comparison it prevents. Without a known PR head, the time floor alone applies, as it does for `pr`, `pr.queue`, `pr.mergeable`, `pr.review`, branch selectors such as `ci[main]`, merged or closed PRs, Gitea, and PR-less checkouts. Forge propagation lag can still affect those readings. Loop conditions have no commit fence.

A case's optional `for` is a nonzero duration in `s`, `m`, `h`, or `d`. A held case must route its outcome away from `@wait`. While that case keeps winning, the run shows `holding red 1m/2m`; the next evaluation at or after two minutes follows `on.red`. A different answer closes the hold, and a later true starts a new one. Subscribed signals and the minute tick re-sample an ordinary waiting check; a kept `until` wait samples only at the next eligible leader turn end. Changes entirely between samples go unseen. `key` fixes the case's string or integer key; without it the answer inherits the run's key.

A condition reading `window.*` requires `provider = "claude"` (an agent kind, not a profile) and accepts an optional `account = "work"`. Both are refused when inapplicable. `team.stage` values are checked against the project's effective teams plus `Done`; validation outside a workspace warns that stage names were not checked.

A waiting check without `recheck_at` runs again about once a minute. A signal matching the current check's subscription clears `recheck_at` and runs it at once. A check with no subscription gets only its recheck time and the minute tick; signals never wake a step.

A condition derives these subscriptions from all its cases, including negated terms:

| Term | Signal subscription |
| --- | --- |
| `ci` | `ci.*` for the run's checkout |
| `pr`, `pr.queue`, `pr.mergeable`, `pr.review` | `pr.*` for the run's checkout |
| `team.stage` | `team.stage` for the run's board |
| Branch-selector terms such as `ci[main]`, and `window.*` | None; recheck time and the minute tick only |

For a run, shell, or agent check, declare the signals that should re-run it:

```toml
[checks.review]
run = "checks/review.py"
on = { ready = "@done", pending = "@wait" }
timeout = "2d"
wake = ["ci.failed", { signal = "deploy.finished", match = { env = "prod" } }]
```

Each entry is a selector string or a table with `signal` and optional `match`; other table keys are refused. Selectors and match values use the [loop signal grammar](../reference/cli/loop.md#signals). `ci` and `pr` default to the run's checkout `path`, unless the entry explicitly matches `path` or `branch`. `team.stage` defaults to the run's current board, unless the entry matches `team`, `instance`, or `board`; relinking the checkout changes which board wakes it. Other families, such as `review.ready`, are room-wide unless given match filters. For example, `{ signal = "ci.failed", match = { path = "*" } }` accepts any checkout. Project-scoped runs use the project root for these defaults. Agent lifecycle selectors and team selectors other than exact `team.stage` are refused.

A check that can wait must have a `timeout`. It starts each time the run enters the check and runs across every wait there. When it passes, the run parks. Here each return from `sweep` starts a new two days.

Keep `run` for custom, many-outcome answers. A rate limit is another thing to wait for: when the last attempt ended as `limited`, `context.json` carries `prev_attempt.limited.reset_at`, and a script can answer a waiting outcome with that time as `recheck_at`. The [sample `pr.py` in the RimZ repository](../../crates/rimz/tests/fixtures/workflows/babysit-pr/checks/pr.py) does this, and waits ten minutes after an attempt that died.

## Let a model decide

Some decisions need judgment that a script cannot give: whether review comments need a code change, or whether a diff matches its issue. A check with `agent` in place of `run` is a judge:

```toml
[checks.review]
agent = "claude"
prompt-file = "checks/review.md"
window = "5h.left>=40"
on = { approve = "@done", revise = "fix" }
timeout = "1d"
```

`checks/review.md` says what to judge and what each outcome means:

```markdown
Read the review comments on this branch's PR with `gh pr view --comments`.
Answer "revise" with a one-line reason when any comment asks for a code
change that is not already on the branch. Otherwise answer "approve".
```

A judge runs headless in the checkout. Its permission mode is automatic unless its profile sets one, so it does not stop on prompts. RimZ adds instructions to its prompt: where `context.json` is, and that the answer must be one JSON object of the same shape a script prints, written to `output_path`. A judge must leave the checkout alone. Under [scoped isolation](./security.md#scoped-isolation), RimZ mounts the checkout and its Git directories read-only for the judge. Under host isolation, which is the default, only the prompt tells it so. `rimz workflow validate` refuses a judge whose agent has no headless mode.

A judge that fails (its process fails, or it writes no answer or a bad one) runs once more, with `retry` in `context.json` saying what went wrong. A second failure parks the run. An [unsupported agent version](../reference/agent-support.md) refuses before the judge runs and follows the same retry and park policy; `workflow show` includes the upgrade instruction.

`window` holds a judge until its provider's usage window has room, or an agent step until every agent cell's provider has room. `5h.left` and `7d.left` are the percent left in the 5-hour and 7-day windows, compared with a whole percent, as in [loop conditions](./loops.md#wait-for-room-in-a-usage-window). While it holds, `rimz workflow list` shows a line such as `waiting: review, window 5h.left=12<40, since 15:02, ×40`.

Put the cheap case in a script and the unclear case in a judge. A script that answers `unsure`, routed to a judge, pays for a model only when it has to.

## When a run needs you

A run parks when it would otherwise have to guess:

- a step reached `max-visits` or `max-visits-per-key`, and `on.cap` defaults or routes to `@user`,
- a check's `timeout` passed,
- a run check exited nonzero or printed a bad answer, or a shell check could not start or its log could not open,
- a judge failed twice,
- a turn step's leader or child blocked on a question or permission prompt, and `on.asked = "@user"` routes it to you,
- the run's `deadline` passed,
- a check routed an outcome to `@user`, with its `reason` as the text.

A condition's unread data normally waits rather than parking; an explicit `on.unknown = "@user"` asks you instead.

A blocking question or permission prompt from a turn step's leader or its subagent keeps the attempt live: answer in the agent's pane. `workflow list` reads `asking you: <handle> (<kind>): <text>`, and the step's `timeout` pauses from when the ask opened until RimZ sees it answered. A subagent's ask or answer is noticed within about a minute. Set `on.asked` to end the attempt on a question instead: RimZ stops the layout and follows that edge, with the question in `prev_attempt.asked` in `context.json`. If the edge is `@user`, fix what made the agent ask (its permission mode, the prompt, or a [sign](#rules-for-writing-nodes)), then resume to relaunch it.

`[workflow] deadline = "2026-11-01"` bounds the whole run. It takes a date (local midnight), a local date and time, or an RFC 3339 instant. When it passes, RimZ stops a live attempt and parks the run, unless `on.deadline` routes it somewhere else.

A parked run appears in three places: a line in the [sidebar cockpit](../interface/sidebar.md), the NEEDS YOU section of `rimz loop list` when a loop task started it, and `rimz workflow list` with its reason. The [park reasons table](../reference/cli/workflow.md#park-reasons) gives the exact word for each case.

Fix what the reason names, then send the run back to work:

```sh
rimz workflow resume "$RUN"                          # the same node, with a fresh timeout
rimz workflow resume "$RUN" --at tests               # another node
rimz workflow resume "$RUN" --extend visits=+2       # raise both visit caps for every step and key
rimz workflow resume "$RUN" --extend deadline=2026-11-08
```

A run that is still active takes `--extend` too, without `--at`. An extension is a grant on this run only, and the definition does not change. A visit grant raises both `max-visits` and `max-visits-per-key` for every step and key. `show` prints the total as `step fix: 3 visits of 3 + 2 grant`. If the step declares `max-visits-per-key`, it adds rows such as `  key "lint": 2 visits of 2 + 2 grant`, with keys rendered as JSON (`null` for unkeyed visits); otherwise it prints no key rows.

## Stop a run and clean up

```sh
rimz workflow cancel "$RUN"
```

`cancel` ends the run as `canceled` and stops whatever it is running: the check's script or shell command, the judge, a command step's process group, or every cell of an agent step's layout and its subagents. A run that already ended prints `already terminal; unchanged`. The commits and uncommitted edits in the checkout stay, and so does the run directory.

A run started by a condition loop task stays finished, even while its condition holds. To start another, run `rimz loop fire <name>` from that worktree, or start an independent run with `rimz workflow run`. Schedule, signal, and watch tasks can start another run on their next fire; disable the task with `rimz loop disable <name>` to prevent that.

When you are done with the worktree, `rimz worktree remove feat-x` removes it. It refuses while the tree has uncommitted changes, while the branch holds commits that have not landed, or while an agent or pane is still in the tree, and names the fix. A check runs in the checkout, so files your suite writes there (a `__pycache__/`, a coverage report) count as changes unless `.gitignore` covers them. `--force` removes it anyway and deletes those commits with the branch. The [cleanup section](./worktrees.md#cleanup-once-work-lands) covers landing the work first.

A run also ends on its own: as `done` at `@done`, or as `failed` at `@failed`. A finished run leaves `rimz workflow list`, and `rimz workflow show` still reads it for seven days. After that, RimZ's garbage collection removes its run directory. `rimz reset` keeps run directories, and `rimz reset --hard` removes them.

## Start runs from a loop

A loop task can start the runs for you. This one, in `~/.rimz/loop.toml`, starts a `babysit-pr` run in every RimZ worktree of one project that has an open PR:

```toml
[tasks.babysit-pr]
root          = "/home/you/code/app"
workflow      = "babysit-pr"
each-worktree = true
when          = ["pr=open"]
for           = "3m"
```

This condition task starts its workflow once per checkout. An active, parked, or finished run prevents another automatic launch there, even if the condition turns false and true again, the room restarts, or you edit the task under the same name. Use `rimz loop fire babysit-pr` from the worktree to start another run after it ends, or `rimz workflow run babysit-pr` for an independent run. A workflow with project scope covers every checkout of the task, including new ones. Schedule, signal, and watch workflow tasks instead admit a new run on each fire unless a live run holds the scope. Write this task by hand, since `rimz loop add` has no flag for it. The task's own `budget` does not apply; agent steps run under the room's budgets, and command steps use no provider. The [loop task reference](../reference/cli/workflow.md#loop-tasks) lists the fields a workflow task takes.

## One run per project

Some work is project-wide: a monthly dependency sweep, or a loop that takes the next item off a shared backlog. Set `[workflow] scope = "project"` for it. Every node then runs at the project root, and the workflow holds one active or parked run for the whole project. Starting it from a worktree still selects the root, and `-w` prints a notice that says so. A loop task with `each-worktree` starts one run and skips the other worktrees.

## Rules for writing nodes

Add an optional `summary` to a check or step to describe what it does for readers of the workflow. It must be one nonempty line, not instructions to the runner. Leaving it out produces no validation warning.

The engine stays simple because nodes follow four rules.

1. **Make every node safe to repeat.** An agent step launches the same prompt on every visit; a command step runs the same command. Both learn how far things got from the world: the repository, the PR, `$RIMZ_WORKFLOW_DIR/notes/`. A check can run twice for one decision, because the process hosting it can die after the script ran.
2. **Say what to do with a dirty tree.** A stopped attempt can leave uncommitted work behind. An agent step's prompt tells the agent to keep it, commit it, or discard it; a command must handle that state itself.
3. **Tune an agent step through its sign, never its prompt file.** A sign is a file, `$RIMZ_WORKFLOW_DIR/signs/<step>.md`, that RimZ adds after the agent step's prompt at every launch until someone deletes it. A judge can write one to steer the next attempt, and so can you before a resume. The prompt file is shared by every run of the workflow.
4. **Keep judges read-only.** A judge reads and answers. Work belongs in a step.

`notes/` is scratch for your nodes, and RimZ never reads it. A check that walks a list can record there the items it finished. Name each file after the item it records, so a repeat overwrites its own file and never adds a second one.

### The check answer

A script prints, and a judge writes, exactly one JSON object. A script must also exit 0: a nonzero exit routes to `invalid`, so a script that uses `set -e` must catch the failures it expects.

A shell check constructs its answer for you: `outcome` is `pass` or `fail`, `code` is the exit code (null after a signal), `log` is the absolute log path, and `key` is inherited when present. Only `fail` includes `reason`, either `exited N` or `killed by signal`. Output tails stay out of the history. A check timeout routes `timeout`, and the run's deadline routes `deadline`, never `fail`.

| Field | Required | Meaning |
| --- | --- | --- |
| `outcome` | yes | The edge to follow, from the check's `on` table. |
| `reason` | no | Text for the history, and the park text when the outcome routes to `@user`. |
| `recheck_at` | no | An RFC 3339 time. On `@wait`, nothing runs the check again before it, unless a subscribed signal arrives. |
| `key` | no | A string or integer naming the item or failure the run now works on, such as an issue number or `"lint"`. Later context files carry it. |

Any other field travels too: the whole object becomes the next node's `input`. An edge targets a node name, `@wait`, `@done`, `@failed`, or `@user`. A step's `then` must name a check. `rimz workflow schema` prints the JSON Schema of `workflow.toml`.

A condition answer carries `outcome`, the zero-based winning `case` (absent for `else` and `unknown`), `readings` (null for unread), and its fixed or inherited `key` when present. `unknown` also carries a `reason` naming each unread key. During a hold, `outcome` is `hold` until the case's duration completes. `timeout`, `invalid`, `cap`, `capacity`, `deadline`, `unknown`, and `hold` are reserved: cases, `else`, scripts, and judges cannot choose them; a condition's `on.unknown` may override the default, but `on.hold` is never an edge.

### The context file

| Field | Meaning |
| --- | --- |
| `run`, `node`, `seq` | The run id, the current node, and this evaluation's sequence number. |
| `prompt` | The run's `--prompt`, or null. |
| `input` | The answer that routed here, or the engine's object after a cap, timeout, or deadline. |
| `key` | The current item key, or null. |
| `visit`, `visits` | Steps only: this step's visits for the current key, and its visits in total, both including this launch. `max-visits-per-key` caps `visit`; `max-visits` caps `visits`. |
| `output_path` | Judges only: the file to write the answer to. Null for other checks and steps. |
| `retry` | Judges only: set when the judge runs again after a failed first try, with that try's outcome and reason. |
| `signs_path` | For a step, its sign file, `signs/<step>.md`. For a check, the `signs/` directory. |
| `prev_attempt` | How the last step attempt ended, or null. |
| `live_attempt` | The open `until` session, during checks too: its step, sequence, visits, launch key and time, handle, run id, claim path, starting commit, and last checked turn end. Not published for other lifecycles or staged endings. |

`prev_attempt` remains the last attempt that ended; checking an open `until` session does not replace it. During a check, the top-level fields describe that check, while `live_attempt` describes the session you are working with. A keep or discard restores the step view; a close leaves the file until the next publication.

`prev_attempt.outcome` is `ended`, `died`, `timeout`, `asked`, `limited`, or `canceled`. `asked` occurs only on a turn agent step with `on.asked` set. A staying agent step records no ask or step timeout; a terminal budget failure still reads `limited`. A command step ends as `ended`, `died`, `timeout`, or `canceled`. The object also carries the attempt's start and end times (`started_at`, `ended_at`), the leader's handle, the commit before and after (`head_start`, `head_end`), its cost, and, when they apply, the question it asked (`asked`) and the provider's reset time (`limited.reset_at`). For a command step, `code` is its exit code (null after a signal or an unobserved exit) and `log` is its output file when one exists; agent attempts have both fields null. Agent-only evidence is null for a command step. A command's `head_end` is null when another process, rather than the host that reaped the child, settles the attempt.

## See also

- [Workflow CLI](../reference/cli/workflow.md): every command and flag, the waiting line, park reasons, and the JSON shapes.
- [Loops](./loops.md): loop tasks that start runs, signals, and window conditions.
- [Scripting](./scripting.md): the supervised run each agent step's leader owns.
- [Workflow internals](../internals/harness/workflow.md): how the engine decides, for readers of the code.
