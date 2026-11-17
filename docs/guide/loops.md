# Loops

You already run things on a clock. Cron lines and systemd timers drive your maintenance, CI fires on a schedule, and somewhere a `while true; do claude -p "fix the next failing test"; sleep 900; done` holds a terminal open. The idea behind that loop is sound: state the goal once and let the loop do the re-prompting.

Cron and a `while` loop know only a clock. They cannot fire when a pull request merges, when CI goes red, or when a team's board reaches Done, so they poll, and a polled `claude -p` spends a whole agent turn checking a world that has not changed. What they start is also invisible: the scheduled `claude -p` hangs in silence the first time it stops at a permission prompt.

`rimz loop` is the same clock with the room behind it. A room is RimZ's multiplexer session for one project, which `rimz start` opens: the panes of its agents, and the sidebar column that shows what each one is doing. A task is a name, one trigger, and one thing to launch: a shell command, a workflow, an agent, or a team. The trigger decides when the task fires, and the kind decides what the fire starts. To start an agent, name it with `--agent` and give the launch's arguments after `--`, as you would after `rimz agents claude`:

```sh
rimz loop add deps --agent claude --cron '0 9 * * mon' -- "Check for outdated dependencies. Open a PR for the safe minor and patch bumps, and list any majors that need review." -w
```

The trigger is a clock, a signal, or a condition on the room's state. This page takes each in turn, then shows how to read what a task did, how to undo it, and the reflexes that keep unattended agents running.

## Run a command on a schedule

RimZ hands the words after `--` to the process as they are, with no shell in between: a `$`, a `*`, or a quote arrives as written. For a pipeline or a variable, write the shell yourself:

```sh
rimz loop add watchdog --every 15m --timeout 20m -- sh -c 'cargo test 2>&1 | tail -n 50'
```

A task with no kind flag (or `--shell`) runs a command. Each fire runs it once and lasts as long as the command does. Exit `0` records the fire `completed`. Any other exit, or a command still running at the task's timeout, records it `failed`. The timeout is `--timeout`, else `loop.default-timeout`, else two hours, and `add` writes it into the task.

Run `add` from the project the task belongs to. The receipt reads back what you asked for:

```console
$ rimz loop add watchdog --every 15m --timeout 20m -- sh -c 'cargo test 2>&1 | tail -n 50'
added loop task `watchdog`
action: runs `sh -c 'cargo test 2>&1 | tail -n 50'` in /home/you/code/app · timeout 20m
trigger: fires every 15m
next fire: 2026-11-15 18:52 (in 14m)
live while a room for /home/you/code/app is open
no room is open there; start one with `rimz start`, or use `rimz loop timer install` to fire without one
```

The last two lines matter before your first task. A task belongs to the project you ran `add` in (`--root <path>` picks another). A clock task fires only while a room for that project is open, or while the [loop timer](#who-keeps-time) keeps time for it.

A task's command runs at the project root, even when you add it from a linked worktree. To keep its work off the tree you are editing, give the task `-w <name>`: every fire runs in the RimZ-owned worktree of that name, which the first fire creates when it is missing. For a fresh tree each fire, have the command make one with [`rimz worktree new`](./worktrees.md), or make the task an agent or team task with a bare `-w` in its arguments, [below](#start-an-agent-from-a-task). The name is optional: leave it out and the receipt prints the `loop-<petname>` name it chose.

A clock repeats only with `--every` or `--cron`. A time (`--at 07:00`, `--at 'fri 09:30'`) or a delay (`--in 30m`) fires once, and then the task removes itself. [Every schedule shape](#every-schedule-shape) lists them all.

## Start an agent from a task

`--agent <profile>`, `--team <team>` and `--workflow <name>` make the task a launch. The words after `--` are what you would type after `rimz agents claude`, `rimz teams launch recon` or `rimz workflow run fix-tests`, and each fire runs exactly that launch as its own process in the task's checkout, with no stdin and its output in a log file. The [`rimz agents`](./fleet.md) flags work as usual, among them `--effort`, a [permission mode](./fleet.md#set-a-permission-mode), `-w` and `--bg`. `loop add` checks the launch before it stores the task: an unknown profile, team or workflow, arguments the launch would refuse, or a flag a scheduled launch cannot carry (`--detach`, `--stdin`) is refused with the fix, so a typo fails now rather than at 2 a.m.

What you choose is how the fire ends:

- **An agent or team launch is recorded once it is placed.** The agent or team keeps running in its pane, unfocused, so a fire never takes the screen from you. The fire records `launched`, or `error` with the launch's own message when it failed. Use this when the agent should stay: a team that works a branch, an agent that waits for your review.
- **A `-p` launch is recorded once its run has started, the same way.** `rimz loop fire` returns then, and the fire records `launched`; how the run ends later changes neither the row nor the [strikes](#strikes), so read the run itself (`rimz agents history`) for its outcome. Use this when the turn is the job: a nightly sweep, a report, a fix. `-p` takes its own `--timeout` and `--keep`, and has no timeout unless you give one.
- **A workflow's fire is recorded with its run's outcome.** The run is the task's own: `loop show` lists it and `loop stop` cancels it.

What a fire launched stays the task's while it is alive, and an agent idle in its pane is alive. Under the default `skip` [overlap](../reference/cli/loop.md#overlapping-triggers) a later fire therefore skips until you close that agent; `--overlap replace` stops it and launches anew once it has ended, and on an agent task `--overlap allow` launches anyway and leaves it open beside the new one. A spec of two or more agents in a reused worktree is one cohort, so there an `allow` fire prompts the live cohort's leader instead of opening a second cohort. [`rimz loop show`](#see-what-a-task-did) lists each such agent, run or team member, and [`rimz loop stop`](#stop-pause-and-remove-a-task) stops them.

A shell command can still launch agents itself, from a script: those launches carry the fire's id too, and a script hands one off by removing `RIMZ_LOOP_FIRE` from its environment, after which the loop neither lists nor stops it.

A `-p` launch of an agent kind refuses in a project until that kind has a folder-trust decision there, so grant it once before the first fire:

```sh
rimz trust grant --agents claude
```

Then add the tasks:

```sh
rimz loop add sweep --agent claude --cron '0 2 * * *' -- -p --timeout 1h "Scan the repository for cheap improvements and open a PR for each one worth fixing." -w
rimz loop add review-team --team recon --cron '0 9 * * 1-5' -- "Review yesterday's merges and propose focused follow-up." -w
```

A team whose lane already holds a closed cohort needs `--continue` or `--fresh` among its arguments, as `rimz teams launch` does from a shell.

To try one before its time, run `rimz loop fire sweep` ([See what a task did](#see-what-a-task-did) covers what it prints).

A bare `-w` among the arguments gives each fire's launch a fresh worktree, as it does from a shell; the task's own `-w <name>` reuses one named tree for every fire. `loop add` refuses the two together when they name different trees. The receipt reads the launch back on a `launch:` line (the worktree, whether a prompt was found, and a `-p` run's time limit), and warns when a repeating agent or team task leaves its agent idle with no `-p`, no `--overlap` policy and no `--once-per-checkout`, since that agent holds every later fire. The second example launches `recon`, a team you have defined or installed ([Teams](./teams.md) shows both); `loop add` refuses a team it cannot load.

For an agent or team task, RimZ appends one line to the launched agent's prompt saying which task fired and why, such as ``Rule `ci-fix` fired here. ci.failed on feature #42 @88824c2``. An unattended agent then knows what happened without asking. A plain command, and each command or check of a workflow task's run, reads the same line from the `RIMZ_LOOP_TRIGGER` environment variable, `RIMZ_LOOP_TASK` holds the task's name, and `RIMZ_LOOP_FIRE` the id of this one fire. A workflow task's agent and message steps get the line at the end of their prompt or message, after any `--prompt` the task passes. A clock fire or a manual fire carries no such line, unless the manual fire names a signal with `--signal`.

A [workflow](./workflows.md) task (`--workflow <name>`) starts the deployed workflow's run as the task's own, and the fire's outcome is the run's. A `rimz workflow run` that a shell task's script runs starts an ordinary run instead, which carries the fire's id and reports to nobody.

## Who keeps time

RimZ has no scheduler daemon. The clock is the sidebar, the background process that draws the room's column. While a room for the task's project is open, attached or not, the sidebar fires due tasks on its regular tick. With no room open, clock tasks wait.

To keep time without an open room, opt in once:

```sh
rimz loop timer install
```

That installs one systemd user timer on Linux, or one launchd agent on macOS. Once a minute it runs a RimZ tick that fires due clock tasks for every project without an open room. A project with an open room keeps its own time. Know one effect before you install it: a timer fire opens a room for the task's project before its command runs, and leaves that room open. A 02:00 task therefore means a multiplexer session running on the machine by morning. `rimz loop timer status` shows whether the timer is installed, and `rimz loop timer remove` stops it.

Clock and condition tasks need a timekeeper, since both are checked on its tick. A signal task fires from the process that emits the signal, as the [next section](#fire-on-a-signal) explains.

Missed time is not made up in bulk. A new repeating task arms without firing, so adding one sets nothing off. A task that came due while nothing kept time fires once when a clock next sees it, then resumes its schedule, so a week of missed 02:00 fires arrives as one fire. A one-shot fires on the first clock that sees it at or past its time, however late, and its run says how late (`fired 2h 5m late`).

## Fire on a signal

A clock is a guess about when something will happen. A signal is the event itself, and most signals need no wiring. The sidebar already polls the forge (GitHub or Gitea) for the pull request and CI result of each [worktree](./worktrees.md) branch in the room, and the reporting hooks already know when an agent or a team changes state. Each of those changes is a signal a task can fire on by name.

| Family | Signals | Fires when |
| --- | --- | --- |
| `ci` | `ci.passed`, `ci.failed` | The checks on a branch's commit settle. |
| `pr` | `pr.opened`, `pr.merged`, `pr.closed` | A branch's pull request opens, merges, or closes. |
| `pr` (GitHub only) | `pr.behind`, `pr.conflicted`, `pr.queued`, `pr.dequeued` | The PR falls behind its base, has conflicts, or enters or leaves the merge queue. |
| `trunk` | `trunk.moved` | The trunk ref moves to another commit. |
| `worktree` | `worktree.created`, `worktree.removed` | RimZ creates or removes a managed worktree. |
| `agent` | `agent.started`, `agent.idle`, `agent.waiting`, `agent.failed`, `agent.ended` | One agent changes state. |
| `team` | `team.idle`, `team.waiting`, `team.failed`, `team.ended`, `team.stage` | A [team](./teams.md) cohort settles, or its board changes stage. |

The [loop reference](../reference/cli/loop.md#built-in-signals) gives the exact moment each one fires and the payload fields it carries.

Subscribe to one name, or to a whole family with `'ci.*'`. A task on `ci.failed` hears `ci.passed` too: it records the signal as heard, runs nothing, and stays armed, so `rimz loop show` can tell you the branch went green while the task waited for red. Narrow a subscription with `--match key=value` on a payload field. A signal that fails the match is ignored. When the task's selector names that signal or its family, `rimz events emit` names the task as excluded by its `--match`; a task on a sibling name, such as `ci.failed` hearing `ci.passed`, is not named. A `ci` or `pr` task that matches a branch by name, as below, has the sidebar read that branch from the forge even when no checkout in the room is on it, so it fires for a branch that lives only on the forge, such as a teammate's pull request.

```sh
rimz loop add ci-fix --signal ci.failed --match branch=feature --repeat --agent codex -- -w ci-fix "Fix the failing checks."
```

Without `--repeat` the task fires once and is gone. With it, the task keeps listening until you remove it. A signal reaches only the tasks subscribed at that moment, so add the task before you start the work that will emit it.

### Your own signals

Anything that can run a command can put a signal into the room:

```sh
rimz events emit deploy.finished --json '{"env":"prod","version":"1.4.2"}'
```

```console
emitted deploy.finished (evt_01a07076a71c72e09f0461533909ceb1) · fired 1 task
  smoke
```

A CI job's last step, a git hook, a deploy script, or another agent can each emit. The process that emits fires the subscribed tasks itself, so no daemon or running agent is needed. `emit` needs a room that has been started once for the project, but the room does not have to be open: a fire opens it before its command runs. The built-in `ci`, `pr`, and `trunk` signals are different, since the sidebar produces them: they arrive only while the room is open. Names are lowercase words joined by dots, and the first word is the family. The families in the table above, and `wait`, belong to RimZ, and `emit` refuses names in them. The payload is one JSON object, whose top-level fields `--match` filters on and the trigger line prints as `key: value`.

A task subscribes to your own names as it does to RimZ's. The `smoke` task that `emit` fired above was added with:

```sh
rimz loop add smoke --signal deploy.finished --match env=prod --repeat -- ./scripts/smoke-test.sh
```

When a signal arrives while the task's previous run is still going, the default drops the new trigger, so two copies never run at once. `--overlap coalesce` keeps the newest waiting trigger, `--overlap queue` keeps each one up to `--overlap-limit` (default 10), `--overlap replace` cancels the live run for the newest, and `--overlap allow` (agent tasks only) launches a new agent at every trigger and leaves the earlier ones open. [Overlapping triggers](../reference/cli/loop.md#overlapping-triggers) gives the rules.

## Wait for Done and green

A CI signal can arrive before your team finishes review, and a task that subscribes only to that signal misses the later moment when both are ready. A condition reads the current state on each tick instead, in whatever order things happen, and can require it to stay true for a while.

A task reads conditions at the project root, where an unscoped key such as `ci` reads trunk. Name the team's branch in brackets instead, here `feat-x`:

```sh
rimz loop add open-pr --when 'team.stage[feat-x]=Done && ci[feat-x]=passed' --for 30m --agent claude -- "Open the pull request for the feat-x branch."
rimz loop show open-pr
```

`team.stage[feat-x]` reads the team board in the local checkout on `feat-x`, and `ci[feat-x]` reads that branch's CI. The sidebar checks both on each tick. Once both are true, the task holds for 30 minutes, and either reading turning false restarts the hold. When the hold completes, RimZ runs the command and removes the task. Add `--repeat` to fire again each time the condition turns true after being false.

CI and PR readings come from the room's sidebar, which polls the forge, so they need an origin on GitHub or Gitea. Without an open room, or until the sidebar has polled again after a fire, those readings are unknown. An unknown condition does not fire and does not restart its hold. It waits. A poll that finds no CI reads `none`, which `ci=passed` treats as false. [Conditions](../reference/cli/loop.md#conditions) lists every key, value, and rule.

A condition can name branches that have no local checkout:

```sh
rimz loop add follow-up --when 'pr[sidebar-ordering]=merged && pr[sidebar-cards]=merged' --agent claude -- "Continue now that both prerequisites have merged."
```

Both branches belong to the origin repository of the checkout you add the task from. The sidebar keeps polling them while the task exists.

To see what a condition reads now without adding a task, run `rimz loop explain`:

```sh
rimz loop explain 'ci=passed && pr=open'
```

It prints each term's reading and the overall result, and exits `0` when the condition is true. Run from a linked worktree, it reads that worktree's board and pull request, as a wait you arm there does.

### One task for every worktree

When a team finishes the work on a branch, you may want a cleanup agent to review what landed. `--each-worktree` makes that one standing rule: RimZ reads the condition in each RimZ-owned worktree and runs the command in the one that meets it.

```sh
rimz loop add yagni --when 'pr=merged' --each-worktree --repeat --agent yagni -- "Review the merged work for unnecessary complexity and propose focused cleanup."
```

A RimZ-owned worktree is one RimZ created, with `rimz worktree new` or a launch's `-w`. Worktrees that already meet the condition when you add the task qualify too. The project root and worktrees RimZ does not own are left out. To stop after one successful run per worktree, add the task with `--once-per-checkout` ([launch memory](../reference/cli/loop.md#launch-memory)).

## Wait for room in a usage window

A long refactor that starts in a nearly spent 5-hour usage window stalls halfway. Start it when the window has room instead. `window.5h.left` and `window.7d.left` read the percent left in a provider's 5-hour or 7-day window:

```sh
rimz loop add big-refactor --when 'window.5h.left>=80' --agent claude -- "Refactor the storage layer and leave the branch green."
```

An agent task reads the windows of the kind it launches, on the account its launch names, and the receipt says so on a `windows:` line. `--provider` names the agent kind whose windows the condition reads for any other task, or overrides that default. `--account` picks one of its [named accounts](./accounts.md). Neither picks the account the command's own launches use. `add` reads the window first and refuses when it cannot, and the [conditions reference](../reference/cli/loop.md#conditions) lists those cases.

Set the threshold to the room the work needs. The task fires as soon as the window has that much room, which may be before the window resets. While the window is short, the `add` receipt, `rimz loop list`, `rimz loop watch`, `rimz loop show`, and `rimz loop explain` say when it resets, when the provider reports one. A threshold of 100 can wait a whole window: one prompt spent from the fresh window before the next tick keeps it below 100 until the next reset.

## See what a task did

Three commands answer, from the room's summary down to one run's output:

```sh
rimz loop list            # every task in this room, grouped by what needs you
rimz loop show ci-fix     # one task: is it healthy, when does it fire next, its recent runs
rimz loop logs ci-fix     # the full record of recent runs, with each command's output
```

`rimz loop list` opens with NEEDS YOU: failing tasks, tasks disabled after [strikes](#strikes), and tasks that cannot fire until you fix something, each with the command that fixes it. ROUTINE holds repeating tasks, WAITING holds one-shots that have not fired, and DONE holds the newest one-shots that have. LAST shows each task's latest result, such as `✓ 53m ago · 143 in a row` or `✗ failed 2m ago · exit 101`.

`rimz loop show <name>` opens with a verdict on the latest finished run, then that run's evidence, then the recent runs, LAUNCHES (what the task's fires launched that is still alive: each agent, `-p` run and workflow run, where it works, and its state) and the task's settings. `rimz loop logs <name>` prints one block per run, oldest first, with the trigger, the exit code, and the command's output. A signal fire's block includes the signal's payload.

A fired one-shot needs no teardown: it has removed itself, and DONE keeps its result. A repeating task whose command exits `0` is a quiet fire. It keeps no history entry of its own, so a check that passes every five minutes does not bury the fires that failed. The task counts its quiet fires and keeps the last one's output. A fire that fails keeps its full record.

To test a task before its trigger comes, fire it by hand:

```sh
rimz loop fire ci-fix
```

`fire` starts a run now and leaves the schedule and the trigger as they were. To see the line a signal would hand the task, fire it as if that signal had come; nothing is emitted, so no other task wakes:

```sh
rimz loop fire ci-fix --signal ci.failed --json '{"branch":"feature"}'
```

If the task's own trigger would not take that signal, the fire says why and runs anyway. For a condition that is not true yet, it says so and runs anyway. An agent, team, or workflow task prints its launch's [outcome](../reference/cli/loop.md#what-a-task-launches) once the launch has placed its agents or published its run. A shell task prints `launched` and the `rimz workflow show` command that follows the run, since a shell fire runs its command as a one-step workflow run. `launched` means the run started, and a run that then fails still prints it: `rimz loop show` prints the failed step's error inline.

Exact screen layouts, sections, and JSON fields are in the reference for [`loop list`](../reference/cli/loop.md#loop-list), [`loop show`](../reference/cli/loop.md#loop-show), and [`loop logs`](../reference/cli/loop.md#loop-logs).

## Strikes

Three failed fires in a row disable a task. `rimz loop list` then shows `disabled after 3 strikes` under NEEDS YOU, and [notification handlers](./notifications.md) fire with kind `loop_disabled`. A strike is a fire whose command exited non-zero or ran past its timeout. A `completed` fire resets the count, so a watchdog whose suite passes again is back to zero.

Some fires count for nothing: a canceled run, a fire turned away before it ran (by the overlap rule, for example), and a fire that could not start a run at all. A machine that cannot start runners therefore never disables your tasks.

`rimz loop enable <name>` clears the strikes and re-arms the task. `--max-strikes <N>` on `add` changes the threshold, and `--max-strikes 0` turns it off. The [strikes table](../reference/cli/loop.md#strikes) lists how each result counts.

## Stop, pause, and remove a task

Every change you make with `rimz loop` has a command that undoes it:

```sh
rimz loop stop ci-fix            # cancel the running fire and what fires launched, keep the task
rimz loop pause ci-fix --for 2h  # hold the task, then resume on its own
rimz loop disable ci-fix         # hold the task until you enable it
rimz loop enable ci-fix          # lift a pause, a disable, or a strike disable
rimz loop remove ci-fix          # delete the task, keep its history
rimz loop timer remove           # stop the machine-wide timer
```

`stop` cancels the live run, then stops everything the task's fires launched that is still alive: each agent and team member as `rimz agents stop` does, each `-p` run, and each workflow run. A launch the command handed off by removing `RIMZ_LOOP_FIRE` keeps running: stop it with `rimz agents stop`. Triggers waiting under an overlap policy survive `stop`, and the next one starts at once. Pause or disable the task to drop them.

Pause, disable, and enable belong to this machine and never edit the task's definition. When a hold lifts, the schedule starts fresh from that moment, so fires missed while held never replay.

`remove` deletes the task from the file that holds it. `rimz loop logs <name>` still reads its history. For a [project task](#what-a-task-does-on-your-machine), that file is the committed `.rimz/config.toml`, so the removal reaches everyone who pulls your commit, and RimZ prints the project's trust result. `rimz loop timer remove` stops future ticks, but rooms the timer already opened stay open. `rimz uninstall` removes the timer too.

## What a task does on your machine

`rimz loop add` writes a task definition and starts no process.

A repeating task goes into `~/.rimz/loop.toml`. Like your crontab, it belongs to this machine, and a cloned repository never brings one with it. The file is plain TOML, and `rimz loop add deps --agent claude --cron '0 9 * * mon' -- "Check for outdated dependencies." -w` lands as:

```toml
[tasks.deps]
kind = "agent"
target = "claude"
args = ["Check for outdated dependencies.", "-w"]
root = "/home/you/code/app"
cron = "0 9 * * mon"
```

A task holds the keys `rimz loop add` writes, plus a few you set by hand, such as `dir`. A task with a key RimZ does not know does not fire, and `rimz loop list` shows it under NEEDS YOU as `invalid` with the reason. Delete it and add it again with `rimz loop add`. The other tasks in the file keep firing.

A task that fires once (`--at`, `--in`, or a `--signal` or `--when` task without `--repeat`) belongs to the room. It lives in `~/.rimz/ws/<workspace-dir>/records/loop-instances.json`, beside the waits agents arm, so start the room with `rimz start` before you add one. It removes itself after its fire. [Where tasks are stored](../reference/cli/loop.md#where-tasks-are-stored) covers name clashes between the files.

`--project` writes the task into the project config, `<root>/.rimz/config.toml`, so it travels with the repository. Such a task must repeat (`--every`, `--cron`, or `--signal` with `--repeat`) and always runs at the project root. A committed task runs commands on the machine of whoever pulls it, so it runs only when both of these hold:

- **Trust decides whether the commands are acceptable.** The task's command is part of what you approve when you [trust the project](./security.md), and nothing runs until you do.
- **Enablement decides whether this task runs here.** A project task pulled from a repository starts disabled, even in a trusted project, until you run `rimz loop enable <name>`. A task you add with `--project` starts enabled on your machine.

### Where a fire runs

A fire opens the room for the task's project, then runs its command or launch at the project root, in the worktree an `--each-worktree` fire selected, in the worktree the task's `-w` names, or in a worktree its launch's `-w` opens. Every agent a fire launches into the room opens in an unfocused tab of its own, so a fire never splits the tab you are working in. When the agent needs you, its tab and its sidebar card show it, as for any agent.

### What a fire leaves behind

A shell task's fire runs its command as one [workflow run](./workflows.md#start-runs-from-a-loop), and a workflow task's fire starts the workflow's run. A launch task's fire writes its launch's output to `~/.rimz/loops/launches/<fire>.log`. A failed or canceled fire, and every one-shot fire, appends one line to `~/.rimz/logs/loop-runs.log.jsonl` when its outcome is recorded, and `rimz workflow show <run>` keeps its record. A repeating task's quiet fire leaves only the task's quiet summary in `~/.rimz/loops/tasks.json`.

## Wake a running agent

The previous sections schedule work from your shell. An agent needs its own alarm. An `until nc -z localhost 3000; do sleep 1; done` loop in its shell tool holds its turn open while it waits for a deploy, and the follow-up otherwise falls to you. `rimz wait` lets the agent arm an alarm, end its turn, and get the result back in the same conversation:

```sh
rimz wait --in 30m
rimz wait --at 14:30
rimz wait --when pr=merged
rimz wait --pid 16776
rimz wait --check 'nc -z localhost 3000'
rimz wait --file build.log --grep 'READY'
rimz wait --run 'gh run watch --exit-status'
```

An agent runs these from its own shell, and the wait wakes that agent alone. RimZ refuses a wait from a shell that is not an agent's. A watched command runs in a detached process, and the wake message gives its exit status, the elapsed time, and the path of a file holding its full output. While the agent sleeps, RimZ can send short keepalive pings so the provider's prompt cache is still warm at the wake. [Configuration](./configuration.md#idle-compaction) owns those settings.

A wait lasts until it fires, is canceled, or its agent ends. `rimz agents restart` and `rimz agents molt` keep it. `rimz wait list` shows an agent's own waits, and `rimz loop list` folds every agent's waits under WAKE-UPS. A `pr` or `ci` wait needs the room's sidebar to read the forge: with the room closed it reads `readings paused (room closed)`, and in a checkout whose `origin` is on no forge RimZ reads (GitHub through `gh`, Gitea through `tea`) the wait refuses to arm, or, when that turns out only after arming, says `no supported forge for this checkout` and counts under NEEDS YOU. The [wait reference](../reference/cli/wait.md) owns every flag and the exact message each trigger delivers.

To wake a teammate on a clock instead, make the task's command a [message](./messaging.md):

```sh
rimz loop add standup --cron '0 9 * * 1-5' -- rimz message @planner "Post today's plan to #dev"
```

A team can also wake its own members on a signal through [signal bindings](./teams.md#define-your-own-team).

## Every schedule shape

Each task names one trigger and one command after `--`.

| Shape | Flags | Repeats? | Example |
| --- | --- | --- | --- |
| One-shot | `--at` (`HH:MM`, `YYYY-MM-DD HH:MM`, or `<weekday> HH:MM`), or `--in <delay>` | fires once, then the task removes itself | `--in 30m` |
| Interval | `--every <duration>`, measured from the last fire | yes | `--every 15m` |
| Raw cron | `--cron`, a five-field expression with names such as `mon-fri` and steps, or a macro such as `@daily` | per the expression | `--cron "*/15 * * * *"` |
| Signal | `--signal <name-or-family>`, narrowed with `--match key=value` ([above](#fire-on-a-signal)) | once, or on every delivering signal with `--repeat` | `--signal ci.failed`, `--signal 'ci.*'` |
| Condition | `--when <expr>`, held with `--for <duration>` ([above](#wait-for-done-and-green)) | once, or each time it turns true again with `--repeat` | `--when 'pr[feat-x]=merged'` |

`--every 1d` is an interval: it fires a day after the last fire and drifts with it. `--cron '0 7 * * *'` fires at 07:00 each day, and `--cron '0 7 * * 1-5'` on weekdays only. `--at` times and cron use the top-level `timezone` setting, or the system zone when it is unset. Every command and flag is in the [loop CLI reference](../reference/cli/loop.md).

## Keep the fleet moving

An unattended agent stops for reasons that need no judgment from you. The provider's usage window empties mid-turn. The API sheds load and drops the stream. The context window fills one step short of the finish. A Codex account sits on reset credits that expire unused. Each stop has a known fix: wait for the reset, retry in a few minutes, compact, redeem a credit. A stock CLI leaves each of them to whoever is watching, and overnight that is nobody.

RimZ applies each fix as a configurable reflex. The room recognizes the stop from the provider's own evidence, applies the fix when it can work, and the agent carries on in the same session, as if you had typed it. Idle compaction and smart compaction are on by default. Auto-continue and auto-redeem are opt-in, except that a Codex credit about to expire is redeemed even with auto-redeem off. These reflexes type into your panes and spend on your account, so each section states the rules it follows. Every action appears in `rimz stats --assists`.

```sh
rimz config set resume.auto_continue true     # resume rate-limit and API-error parks
rimz config set resume.auto_redeem true       # spend Codex reset credits when they buy real time
rimz config set harness.idle_compact off      # opt out of idle compaction
rimz config set harness.smart_compact 200k    # wrap up a filling turn, compact before the next prompt
```

A related cleanup is not a reflex but a request: `rimz agents stop @coder --when-idle` stops an agent once it has rested three minutes with nothing owed to it ([stop reference](../reference/cli/agents.md#stop)).

### Auto-continue

A turn that dies on a rate limit or an API failure parks its agent: the card shows `⏸`, and work stops until someone types `continue`. At your desk that is one keystroke. Away from it, the fleet sits idle from 1 a.m. until you notice. Auto-continue types it for you.

It acts only on the record the agent's own CLI writes when a turn dies, naming the cause. Every automatic resume therefore traces back to the provider's account of why the agent stopped, and every other kind of stop stays parked for you. Because that record comes from each agent's adapter, the [per-agent notes](../reference/agent-support.md#notes-on-the-alpha-and-experimental-set) say where auto-continue never arms. One park needs no provider record: a [dollar-cap park](./budget.md#what-resumes-a-parked-agent), which RimZ imposed itself.

The cause sets the time of the attempt:

- A rate-limit or spend-limit park waits for the account's or model's window to reset, as a usage reading taken after the limit shows it. An agent on a model its provider refuses on that account is never continued, since the provider would refuse the turn again.
- A daily dollar-cap park waits for local midnight, and an account's monthly cap for 00:00 UTC on the 1st.
- An overload or API-error park has no reset time, so it retries three minutes after the failure, then every five minutes (`resume.auto_continue_backoff_secs`, default `[180, 300]`, the last value repeating).

Each attempt sends `continue` (`resume.auto_continue_text`) into the agent's pane, through the same path as `rimz message --steer`. After 12 attempts (`resume.auto_continue_max_retries`), just under an hour on the default timing, the card turns `failed` and comes to you like any other card that needs you. A nudge still queued when you prompt the agent yourself is withdrawn without being typed. The keys are in [configuration → Resume](./configuration.md#resume).

### Auto-redeem

A Codex plan grants reset credits. Redeem one and a spent usage window refills at once. Managed by hand, credits waste value at both ends: one you forget expires unspent, and one you hold while a spent window parks the fleet costs a night of work. Timing matters too. A credit redeemed just before the window's natural reset buys minutes, while one redeemed the moment a spent window blocks the fleet buys hours. Auto-redeem decides by three rules, and each redemption's record names the rule:

- **A credit about to expire is spent (expiry rescue).** RimZ redeems a credit within thirty minutes of its expiry, even with `auto_redeem` off, on every Codex account you declared, while any room is open. It skips an account whose window shows no usage at all, since a refill there buys nothing.
- **A spent window far from its reset is refilled now (blocked gain).** When the natural reset is at least `resume.auto_redeem_min_gain` away (12 hours by default), a credit now recovers those hours. A nearer reset makes waiting cheaper.
- **Credits that expire together are spread out (scheduled redeem).** When several credits near expiry together, RimZ measures how long your usage takes to fill a fresh window and redeems them that far apart, so each one lands on a window with room to use it.

Blocked gain and scheduled redeem act only on an account this room uses, and only when its subscription window is spent and readable. By default, `resume.spend_order = "credits_first"` uses the account's paid credit balance before banked reset credits. Set it to `resets_first` to redeem a useful reset first. Attempts per account are at least ten minutes apart, thirty after a success. A redemption refreshes the account's reading at once, and the refilled window wakes the agents [auto-continue](#auto-continue) parked on it.

The Codex block in the sidebar shows the credit count and what auto-redeem would do now, and the [sidebar reference](../interface/sidebar.md) explains each marker. Claude accounts bank limit resets of their own. For those, and for Codex with `auto_redeem` off, the sidebar runs the same rules as advice for [`rimz accounts redeem`](../reference/cli/accounts.md#redeem).

### Idle compaction

An idle agent can outlive its provider's prompt cache, and then its next prompt pays to cache the whole conversation again. Idle compaction sends the agent's own compact command shortly before the cache expires, once its context holds enough tokens to be worth it. The agent summarizes its conversation and carries on from that smaller context. Details the summary does not keep are lost.

It skips an agent whose account shows a spent cap for its model, or a spent usage window that no paid extra usage carries. If the agent starts another turn before the queued command lands, RimZ drops the command. Set `harness.idle_compact` to `off` to turn off the machine default. A profile or team role that sets its own token threshold still applies. [Configuration](./configuration.md#idle-compaction) gives the timing per provider, which agents qualify, and the thresholds. To change one agent's threshold for its current work, run [`rimz agents tune @coder idle-compact 120k`](../reference/cli/agents.md#tune), and `reset` restores it.

### Smart compaction

Every agent CLI compacts on its own when the context hits its ceiling, wherever the work stands. Driving one agent by hand, you avoid that: a task wraps up, you type `/compact` at the clean point, and the summary carries a finished state forward. In a fleet, a long turn can hit the ceiling with nobody there to choose the moment.

Smart compaction watches each agent's context against a token line and acts at two moments:

- **During a turn, RimZ asks the agent to wrap up.** It sends a steer asking the agent to finish its current step without leaving files half-edited, start nothing new, and end its turn. When the turn ends, RimZ compacts if context is still above the line, then sends `continue`. Interrupting the agent cancels the continue.
- **Before a message reaches a resting agent above the line, RimZ compacts first.** A `rimz message` or a scheduled wait then lands in the fresh context.

The line is `harness.smart_compact`, default `258k` tokens. Set a token count like `200k`, a percentage like `70%`, or `off`. A profile or team role can set its own `smart-compact`, and [`rimz agents tune @coder smart-compact 70%`](../reference/cli/agents.md#tune) overrides it for one session. While the account's usage window is spent and no paid extra usage carries it, RimZ sends no wrap-up and holds `continue` until the reset. The provider's own auto-compaction stays as the backstop. [Configuration](./configuration.md#smart-compaction) lists the adapters that skip it, and [messaging](./messaging.md#compact-before-the-message-lands) gives the delivery rules.

## Permissions for an unattended run

An unattended run has to get past permission prompts without you. Two patterns cover it, and they combine.

Answer in the agent's own UI to keep the full record. A [handler that answers](./notifications.md#handlers-that-answer) sends the answer with `rimz pane send`. The prompt, the answer, and the tool run then stay in the agent's transcript, as if you had typed it. Use this when the decisions belong on the record.

Use the agent's bypass flag when the run cannot afford to stop. `--yolo` on the task's `rimz agents` command passes the adapter's bypass flag (`claude --dangerously-skip-permissions`, `codex --dangerously-bypass-approvals-and-sandbox`), and `--ask` keeps the provider's prompts. These are the [permission modes](./fleet.md#set-a-permission-mode) every launch takes. With a bypass flag, the agent skips permission events at the source, so RimZ records no per-decision audit trail, only sessions, completions, and failures. Use it only where you accept that gap.

Trust grants, notification handlers, and the permission mode itself are covered in [security](./security.md).

## A fleet that works nights

A hands-off room stacks four layers, from least to most involved. [The reflexes](#keep-the-fleet-moving) keep one agent going through rate limits with no schedule at all. A clock task puts work on a schedule. A [signal](#fire-on-a-signal) or [condition](#wait-for-done-and-green) fires it only when something happened. A [notification handler](./notifications.md) catches what none of them can decide alone. Use the lowest layer that solves the problem.

A task's agent runs inside the room, so its prompt can use every other RimZ command:

```sh
# 02:00 every night: a triage that fans out and opens PRs
rimz loop add nightly --agent claude --cron '0 2 * * *' -- -p --timeout 4h --budget 5 \
    "Scan the repository for bugs and cheap improvements. For each one worth fixing, \
run a codex -p subagent in its own worktree, review its diff, and open a PR." -w
```

That is one scheduled turn in a fresh worktree, capped at four hours and five dollars. Its agent fans work out with [subagents](./subagents.md) and isolates each fix in a [worktree](./worktrees.md). It could as easily launch a [team](./teams.md) and brief it with [messages](./messaging.md).

Leave the room open, detached on your workstation or on a server you reach with [`rimz remote`](./remote.md). [Auto-continue](#auto-continue) carries runs over rate limits, and a question or failure trips a [notification handler](./notifications.md) that reaches your phone. In the morning, `rimz loop list` and the PR queue show what the night produced.

## See also

- [Workflows](./workflows.md): the run every fire starts, and the graph to write when one command is not enough.
- [Budgets](./budget.md): the dollar caps that bound hands-off work.
- [Notifications](./notifications.md): push routes and handlers that act on what a loop cannot handle alone.
- [Messaging](./messaging.md): the delivery path waits use, `--schedule` for one-off reminders, and smart compaction in full.
- [Loop CLI](../reference/cli/loop.md), [Wait CLI](../reference/cli/wait.md), and [Events CLI](../reference/cli/events.md): every flag, the watcher, and the signal grammar.
- [Configuration](./configuration.md): the `[resume]` and `[harness]` keys.
- [Security and trust](./security.md): bypass flags and project trust.
- [Loop internals](../internals/harness/loops.md): the clock, the state files, and the run log, for reading the code.
