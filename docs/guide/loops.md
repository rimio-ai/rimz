# Loops

> `rimz loop` is cron for your agents. A task is a name, a trigger, and one action: start a fresh agent turn, wake an agent that is already running, or run a shell command. The clock, the state files, and the run log underneath are in [loops.md](../internals/harness/loops.md).

## Why rimz loop

You already run things on a clock. Cron lines and systemd timers drive your maintenance, CI fires on a schedule, and lately a `while true; do claude -p "fix the next failing test"; sleep 900; done` is holding a terminal somewhere. The instinct behind that loop is right: state the goal once and let the loop iterate instead of you re-prompting by hand.

But cron and a hand-rolled `while` loop only know one move: start a fresh process and walk away. That scheduled `claude -p` is invisible, so it hangs in silence the first time it stops for a permission prompt. It fires whether or not there is anything to do, spending a whole agent turn to re-check a world that has not changed. And it cannot hand work to an agent you already have running; every run starts from an empty context.

`rimz loop` is the same clock with the room behind it. A task names one of three actions:

- `--agent <kind>` starts a fresh supervised agent that runs the prompt once and cleans up.
- `--wake @handle` delivers the prompt to an agent you already have running, so the work resumes in that conversation with all of its context.
- `--check <cmd>` runs a shell command. Alone it is the whole task, a scheduled command with a run history. In front of `--agent` or `--wake` it is a gate: the command runs first, and a turn is spent only on its result, so a scheduled agent never starts just to find nothing to do.

`--verify <cmd>` is the gate on the other side of an `--agent` turn: it runs afterwards and re-prompts the same session until the command passes.

Whichever action fires, the turn runs in the room like any agent you started by hand: a live card in the sidebar, a permission question that routes to you instead of hanging the job, and a line in the run log that `rimz loop show` summarizes and `rimz loop logs` prints in full.

The trigger is a clock or a signal. A clock repeats only when `--every` or `--cron` says so; a bare time (`--at 07:00` or `--in 30m`) fires once and the task removes itself. A signal subscription keeps listening until `--once` retires it.

## Schedule a fresh turn

`--agent` starts a new agent for the turn. On the schedule you set, it opens a fresh supervised pane, runs the prompt once, and cleans up, the same run `rimz agents <kind> "<prompt>" -p` makes when you type it yourself. Reach for it whenever the work should begin from a clean context: a nightly repository sweep, a Monday dependency check, a scheduled security audit, a recurring status report. Each fire is a new agent with no memory of the last one, which is the point when the job is "do this again against today's tree."

One prerequisite: a scheduled turn reports its completion through the agent's hooks, so `rimz loop add --agent` refuses an agent whose hooks are not installed, and names the fix:

```console
$ rimz loop add deps --agent codex --every mon --at 09:00 --prompt "…"
error: codex hooks are not installed, so a scheduled turn cannot report completion
  install them with `rimz hooks install codex`
```

`rimz hooks install claude` is enough for Claude. Codex also has to trust the installed hooks once, from inside Codex (`/hooks`), and the same error names that step when it is the one missing; there is no non-interactive way to grant it. [Set up your machine](./setup.md) covers hooks for every agent. The examples on this page use Claude and Codex interchangeably; swap in the kind you have set up.

Install the hooks, then run the add from the project the task belongs to:

```sh
rimz hooks install claude
cd ~/code/app
rimz loop add deps --agent claude --worktree deps --every mon --at 09:00 \
    --prompt "Check for outdated dependencies. Open a PR for the safe minor and patch bumps, and list any majors that need review."
```

```console
added loop task `deps`
action: launches a fresh claude pane in /home/you/code/app
trigger: fires every Mon at 09:00
next fire: 2026-09-07 09:00 (in 2d)
live while a room for /home/you/code/app is open
no room is open there; start one with `rimz start`, or use `rimz loop timer install` to fire without one
```

The receipt reads back the action, the schedule in plain words, and, for a clock, the concrete next fire. Two facts in it matter before your first task. A task is bound to the project you ran the command in (`--root <path>` picks another), and it fires only while a room for that project is open: with no room, nothing fires until you start one or install the timer ([who keeps time](#who-keeps-time)). Until then `rimz loop list` shows the task with `-` in its NEXT column, because no clock has picked it up yet.

That one command stands in for a cron entry, the guard script around it, and the terminal you would have left open to watch it. It runs Claude in an isolated [worktree](./worktrees.md) every Monday at 09:00, and if a bump breaks the build the agent stops and asks instead of forcing it through: the question reaches you like any other waiting card. A scheduled turn takes the launch flags you know from [the agents guide](./fleet.md): `--worktree`, `--mode`, `--effort`, `--system-prompt-file`, and `--timeout` shape it the same way they shape an interactive one. What the agent does with the turn is bounded only by the prompt: a one-line check, or the whole [fleet that works nights](#a-fleet-that-works-nights).

## Wake a running agent

An agent's work often ends in a wait. CI has twenty minutes left, a reviewer owes comments, a deploy is baking. The agent has nothing to do until then, and the follow-up falls to you: remember to check CI, then tell the agent to merge. The two habits an agent falls into on its own are worse: a `sleep 900` in its shell tool, which holds the turn open for fifteen minutes of nothing, or a poll loop like `while ! gh run watch --exit-status; do sleep 30; done`, which keeps the pane busy until the command finishes. Both spend a live agent on watching a clock.

A wake inverts that. The agent (or you) arms a condition, the agent ends its turn and goes idle, and when the condition trips a message lands in that same conversation. The agent picks up where it left off, with all of its context. An idle agent takes the message at once; a mid-turn agent takes it at its next turn boundary.

Two commands arm a wake, and the choice is who arms it. `rimz wake` is what an agent runs for itself, from its own pane, mid-task. `rimz loop add --wake` is the standing task you set from your shell. Either one needs a live agent to wake, so both need the room open. For a reminder with no condition behind it, [`rimz message --schedule`](./messaging.md) is lighter than both: one delivery on a timer, no task.

Run from an agent's pane with no address, `rimz wake` wakes the caller, and three lines cover the waits an agent meets:

```sh
rimz wake --in 30m                         # wake me in half an hour
rimz wake -- gh run watch --exit-status    # wake me when the command exits
rimz wake --signal ci.failed               # wake me if CI fails on my branch
```

- `--in 30m` is the alarm clock. The room's clock fires it, and the delay has to be under 24 hours.
- A command after `--` is the poll loop without the pane. RimZ runs it in a detached watcher process that outlives the turn, and when it exits the agent is woken with the command, its exit status, and its output tail. It wakes on any exit by default; `--on fail` or `--on success` wakes only for that outcome, and `--timeout 30m` gives up on a command that never ends.
- `--signal ci.failed` waits on an event instead of a command, and needs no arguments beyond the name: RimZ already knows which worktree the calling agent is working in and scopes the wait to that branch's checks. The [next section](#signals-the-rooms-event-bus) is where signals come from.

None of those lines carries a prompt, because the wake explains itself when it lands. It reads back what the agent waited on, how it ended, how long it took, and the event's own payload, so the agent picks up the outcome it was waiting for:

```text
waited on ci.failed on feat-x (PR #91)
fired after 18m [wake-still-path]
{"branch":"feat-x","checks_url":"https://github.com/you/app/commit/9f2c1ab/checks","head":"9f2c1ab","number":91,"path":"/home/you/code/app-feat-x","repo":"you/app","signal":"ci.failed"}
```

A watched command reads the same way, with its own verdict in place of the fire: `exit 1 after 12m`, `timed out after 59m`, or a `watcher died after 3m` that also says the command's own fate is unknown. Its whole combined output goes to `~/.local/state/rimz/workspaces/<workspace-id>/wakes/<name>.log`, which the verdict line names, and the message carries the last 4 KiB of it, so a chatty build leaves the agent with the tail in front of it and the rest one read away ([wake reference](../reference/cli/wake.md#the-output-file)).

`--prompt` is still there as an optional note, delivered verbatim under the evidence. Use it when the verdict will not be enough on its own: a reminder of what you meant to do next, or a label that tells two waits apart. `rimz wake --in 30m --prompt "if the deploy is still baking, extend the freeze"` is the shape to reach for.

From your own shell there is no caller to wake, so the address is required: `rimz wake @planner --in 30m`. An agent passes `@handle` the same way to wake a teammate. `rimz wake list` shows what is pending, with the trigger, the target, and how much of a subscription's window is left; `rimz wake cancel <name>` calls it off. A chain of wakes is a self-paced loop: each turn ends by arming the next wake only while work remains, so the loop stops when the goal is met. When the agent has nothing else to do until a command finishes, `rimz wake --wait -- cargo build` blocks and prints the outcome inline instead; reach for that in a script.

`rimz loop add --wake` takes any trigger a task takes, so it is the form for a wake that repeats or waits behind a guard:

```sh
rimz loop add check-ci --wake @planner --prompt "CI should be done; check the run and merge if green" --in 30m
rimz loop add standup --wake @planner --prompt "post yesterday's progress and today's plan to #dev" --every weekday --at 09:00
```

RimZ resolves `@planner` the moment you add the task, exactly as [`rimz message`](./messaging.md) would, and pins that one session; the receipt names it. When the trigger fires, the prompt travels the same durable delivery path as any message. A session that has exited by then is skipped and the task removed, because that conversation cannot come back. The [signal](#signals-the-rooms-event-bus) and [check-guarded](#guard-a-turn-with-a-check) forms below take `--wake` the same way.

## Signals: the room's event bus

A wake on a timer is a guess about when something will happen. A signal is the thing itself. Most of them need no wiring: RimZ emits from what the room already watches. The sidebar polls the forge for every [worktree](./worktrees.md) branch's pull request and CI verdict (the `#91` badge and the check glyph in each group header), and the lifecycle hooks already know when an agent or a team changes state. Each of those transitions is a signal you can wait on by name.

| Signal | Fires when | Payload carries |
| --- | --- | --- |
| `ci.passed`, `ci.failed` | the checks on a worktree branch settle on success or failure, with or without a pull request | `path`, `branch`, `repo`, plus `head` and `checks_url` when the commit is known, and `number` and `url` when a pull request exists |
| `pr.merged`, `pr.closed` | the pull request on one of the room's worktree branches leaves the open state | `path`, `branch`, `repo`, `state`, plus `number`, `url`, `head`, and `checks_url` when known |
| `agent.started`, `agent.idle`, `agent.waiting`, `agent.failed`, `agent.ended` | one agent's own lifecycle transitions | `kind`, `session`, `status`, `errored`, plus `handle` when the card has a name |
| `team.idle`, `team.waiting`, `team.failed`, `team.ended` | a [team](./teams.md) cohort settles: every member at rest with no queued messages, one member waiting on input, one member's turn failing, or the last member ending | `team`, `instance` (`forge#feat-x`), `member`, and `members` with each handle and status |

Those four families, plus `wake` for a watched command's own completion, are RimZ's own, and `rimz events emit` refuses all five, so nothing can forge a CI verdict or a teammate's lifecycle.

A subscription names one signal or one whole family: `--signal ci.failed` for the red verdict, `--signal 'ci.*'` for any verdict. Either way it watches the whole family, and what it does with a sibling is the useful part. A wake on `ci.failed` sees the green build too, records it as a skip, and stays quiet; the branch that goes green never wakes an agent that only asked for red. A signal that fails a `--match`, from another branch or another cohort, is ignored outright.

Family-watching is what makes a wake survive the fix. A `rimz wake --signal ci.failed` stays armed after it fires: it is a standing subscription with a 59-minute quiet window, and every verdict on the watched branch restarts that window, so the failure at 14:10, the push at 14:20, and the second failure at 14:35 all reach the agent on one arming. When an hour passes with nothing at all, the subscription retires, and the agent hears about that too:

```text
waited on ci.failed on /home/you/code/app-feat-x
nothing in 59m; subscription closed [wake-still-path]
```

A subscription that did see something retires in silence, because the agent has already been told what happened. `--timeout 4h` widens the window for a slower wait, and re-running the same `rimz wake` command restarts the clock rather than arming a second subscription (`already listening: wake-still-path (59m left)`).

Scoping is automatic wherever RimZ already knows the answer. A `ci.*` or `pr.*` wake armed from an agent working in a worktree is scoped to that worktree, and a `team.*` wake to the caller's own cohort, so `rimz wake --signal ci.failed` is the whole CI wait. On the root checkout it refuses instead of waiting forever, because the forge poll only watches worktree branches, and it names the two ways forward: `--match branch=<name>`, or `rimz wake -- gh run watch --exit-status`. An `agent.*` wake has the opposite rule: it must name someone else (`--match handle=@coder` or `--match session=<id>`), because an agent's own idle transition waking it into another turn is a loop.

For a subscription that outlives the agent that armed it, `rimz loop add ci-fix --signal ci.failed --agent codex --prompt "…"` is the standing form: it keeps listening for as long as the task exists, with no quiet window, and `--once` retires it after its first real fire.

### Your own signals

Anything that can run a command can put an event into the room:

```sh
rimz events emit deploy.finished --json '{"env":"prod","version":"1.4.2"}'
```

```console
emitted deploy.finished (evt_01a07076a71c72e09f0461533909ceb1) · fired 1 tasks
  smoke
```

A CI job's last step, a git hook, a deploy script, a cron line, another agent: each one emits, and every wake and task subscribed to that family fires. Names are lowercase dot-separated words outside the five reserved families, and the payload is one JSON object whose top-level fields `--match` filters on and the woken agent reads.

Your own families observe and skip like the built-in ones, so it pays to name them by outcome. An agent waiting with `rimz wake --signal deploy.failed` is woken by `deploy.failed`, kept alive but quiet by `deploy.finished`, and left alone by anything outside the `deploy` family.

The emitting process fires the subscribers itself, so this works with no daemon and no open room. Nothing is queued or replayed either: a signal reaches the subscriptions armed at that instant, which is why an agent arms its wake before starting the work that will emit it.

## Guard a turn with a check

Most recurring automation should stay a plain script. A cron job that runs the test suite, syncs a mirror, or checks a certificate's expiry is deterministic, instant, and free; spending an agent turn to re-check a world that has not changed is burning tokens on `true`. What a script cannot do is recover. When the command that was supposed to pass fails at 2 a.m., the script's whole error path is "page a human."

`--check` runs a script before any agent action and spends a turn only on its result. That makes the script the loop's body and the agent its recovery path:

```sh
# Watchdog: run the suite every 15m; Codex wakes only when it fails
rimz loop add watchdog --check "cargo test" --on fail \
    --agent codex --prompt "fix the failing test" --every 15m

# Trigger-when-green: poll CI until it passes, then hand the merge to the planner
rimz loop add ci-green --check "gh run watch --exit-status" --on success \
    --until 30m --every 2m --wake @planner --prompt "CI is green; merge"
```

The check runs at the project root before any agent action, every fire, and costs nothing. Only its result spends a turn: `--on fail` (the default) wakes the agent on a non-zero exit or a timeout, `--on success` on a zero exit. When the guard fires, RimZ appends the command, its exit status, and its output tail to the prompt, so the agent wakes already reading the evidence. That adds a rung to the escalation ladder: the script handles the routine, the agent handles the failure, and you hear about it only when the agent itself gets stuck. Its turn is supervised like any other, so a stuck fix goes `? waiting` and a [notification](./notifications.md) reaches you. `--until 30m` is the poll-until deadline: the task retires when the check trips or the deadline passes, whichever comes first. A one-shot guarded by a check retires only when the guard fires; a skipped check leaves a bare `--at` task armed for the same time next day.

A check is killed after five minutes unless the task's `--timeout` says otherwise (the same flag caps the agent turn), and a killed check counts as a failure. `gh run watch` on a long pipeline is the case to watch: give the task a `--timeout` longer than the pipeline, or poll on `--every` with a command that returns at once.

A `--check` with no agent action is still worth having. It is a scheduled command that logs `completed`, `failed`, or `timed out`, each with the exit code and output tail, into the run history, and it keeps recurring. It needs no prompt.

`--check` gates firing; `--verify` gates completion. Add `--verify "cargo test"` to a scheduled `--agent` task when the command is the definition of done: a red result returns its evidence to the same live session, up to `--max-attempts` total turns (default 3), before the fire records `verify failed`. It behaves exactly as it does on a hand-run `-p`, described under [verify and retry](./scripting.md#verify-and-retry).

## Gate a task on surplus

Recurring background work is the natural fill for a loop: refactor the next rough module, close test gaps, triage dependencies. None of it is urgent, which is exactly why it must never crowd out the work that is. A background task draws on the same subscription window as your own sessions, and a schedule cannot tell a light week from a heavy one: fired blindly, the 03:00 task spends the budget your Tuesday afternoon needed.

`--surplus` puts that judgment in the schedule. Before each fire, RimZ reads the provider's usage window (the weekly bar on today's Claude and Codex plans) and divides the share of budget left by the share of time left. That ratio is the headroom: `1.0x` means the current pace lands exactly on the reset, and `1.5x` means half again as much budget remains as the remaining time needs.

```sh
rimz loop add refactor --agent claude --prompt "Refactor the next rough module and leave the branch green" \
    --every 4h --surplus 1.5x --surplus-after 3d
```

`--surplus 1.5x` opens the gate only at that headroom or above. `--surplus-after 3d` keeps the task quiet until three days of the window have elapsed, so an untouched early week is not spent before your own heavy days land (used alone, it still requires `1.0x`). The gate guards `--agent` and `--wake` actions alike and runs before any `--check`, so a closed gate runs nothing and costs nothing: the fire records `surplus skipped` without adding a strike, and the schedule keeps polling until real slack appears. An account without a window reading (an API key, or a window that has not started) keeps the gate closed. Which window counts per provider, the headroom model with a worked example, and the fail-closed rules are in [budgets → the surplus gate](./budget.md#the-surplus-gate).

## Budgets and strikes

Two brakes bound a task that runs without you.

Dollar budgets cap what a task spends. `--budget 5` caps each fired run. `--budget-per-day 20` (which requires `--budget`) sums that task's run costs in the configured local day and skips a fire that cannot fund its per-run cap, recording `budget skipped`. `rimz loop list` shows each task's spend against its daily cap, and `rimz loop show` breaks the costs down per run. A room-wide or per-account daily cap from the [budgets guide](./budget.md) gates the same fires the same way, before the task's check or pane exists.

Repeated failures disable the task. Three consecutive failed fires auto-disable it, show `disabled · 3 strikes` in `rimz loop list`, and fire [notification handlers](./notifications.md) with kind `loop_disabled`. A failed, timed-out, or verify-failed turn is a strike. So is a turn fired by a failing check, even when the turn completed: the check on record is the one that ran before the turn, and the counter resets only when a later fire's check passes. A watchdog that finds the world broken three fires in a row is therefore disabled, whatever the agent did in between. A fire that a gate turned away (`budget skipped`, `surplus skipped`, or the previous run still active) is neutral. `rimz loop show` prints the running count, and `rimz loop enable <name>` clears the strikes and re-arms the schedule. `--max-strikes <N>` changes the threshold per task, and `--max-strikes 0` disables the gate. `rimz loop fire` still runs a disabled task for a manual test.

## What a task does on your machine

`rimz loop add` writes a task definition and starts no process.

A standing task appends a `[tasks.<name>]` entry to `~/.config/rimz/loop.toml`: per-machine automation, like your crontab, never inherited by a cloned repository. The file is plain TOML you can edit by hand, and the `deps` task above lands as:

```toml
[tasks.deps]
agent = "claude"
prompt = "Check for outdated dependencies. Open a PR for the safe minor and patch bumps, and list any majors that need review."
root = "/home/you/code/app"
worktree = "deps"
at = "09:00"
every = "mon"
```

A task that retires itself (a bare `--at`, `--in`, anything with a `--until` deadline, a `--once` subscription, or any `rimz wake`) is stored as state in `~/.local/state/rimz/loop-instances.json` instead, so an agent scheduling its own wake never touches your `loop.toml`. Most of those entries disappear when they fire; a `rimz wake --signal` row instead stays until its quiet window runs out, and its stored deadline moves each time it sees a signal. `rimz loop list` shows the source of each task as `machine` or `state`.

`--project` writes the entry to `<root>/.rimz/config.toml`: shared automation that travels with the repo, so it has to be a standing task (`--every`, `--cron`, or `--signal`), and it cannot use `--wake` (a session pinned on your machine means nothing on someone else's). A committed task runs commands on whoever pulls it, so it enters the [project trust hash](./security.md) and stays inert until each user approves it. Trust and enablement answer different questions: trust says the project config contains commands you accept as yours to run, and `rimz loop enable <name>` says this particular task may run unattended on this machine. A project task pulled from a repo starts disabled even after trust is granted; a task you create with `rimz loop add --project` starts enabled on your machine. A trusted project task wins over a same-named machine task without double-firing.

### Who keeps time

There is no RimZ scheduler daemon. While a room for the task's project is open, attached or not, that room's sidebar process fires due tasks on its regular tick, each through a detached `rimz loop run <name>`. With no room open, clock tasks wait. To keep time without an open room, opt in once:

```sh
rimz loop timer install
```

That installs one systemd user timer on Linux or launchd agent on macOS. Once a minute it runs a one-off RimZ tick, re-reads every task, and fires only projects without an open room; an open room still wins for its own tasks. Know what a timer fire does before you install it. An `--agent` fire starts a room for that project, runs the turn in it, and leaves the room open, so a 02:00 task means a multiplexer session running on the machine by morning. A check-only task runs with no room. A `--wake` task cannot be rescued by the timer at all: its target lives in the room, so a closed room means the session is gone and the task is removed. `rimz loop timer status` shows whether it is installed; `rimz loop timer remove` reverses it.

Only clocks need a timekeeper. A signal task and a `rimz wake` watcher are fired by the process that emits the signal or runs the command, whether or not a room is open. The one clock a signal subscription still needs is its own expiry: a `rimz wake --signal` window that runs out is noticed by the room's elder or the timer, like any other deadline.

With or without the timer, nothing is replayed. A task is armed the first time a clock sees it and fires on its next occurrence after that, so opening a room hours late never sets off a catch-up storm.

### Where a scheduled turn runs

A scheduled `--agent` fire opens its pane in the room's `rimzd` tab, the background tab that holds the live stats pane and a `rimz loop watch` dashboard, stacked under that dashboard, so it never splits the tab you are working in. If that dashboard pane was closed, RimZ recreates it at fire time; if the whole `rimzd` tab is gone, the run falls back to a new tab. A manual `rimz loop fire` opens the pane beside your shell instead, so its output stays in front of you.

Every scheduled `--agent` turn gets a timeout: the task's own `--timeout`, or the machine's `loop.default-timeout` (two hours by default). A manual `rimz loop fire` runs unbounded unless the task sets its own.

### What a fire leaves behind

A fire leaves two things: whatever the task did (one transient supervised pane for `--agent`, one delivered message for `--wake`), and one line of run history in `~/.local/state/rimz/loop-runs.log.jsonl`. `rimz loop show <name>` gives that history a health verdict; `rimz loop logs <name>` prints the complete stored forensics. Take a check-only task that runs `cargo test` hourly, fired once by hand in a repository with no `Cargo.toml`:

```console
$ rimz loop fire suite
suite — check
  check: cargo test
  │ error: could not find `Cargo.toml` in `/home/you/code/app` or any parent directory
✗ check failed (exit 101) in 24ms
$ rimz loop show suite
suite — every 1h
  ✗ failing · failed (exit 101) ×1 since 0s ago
  task:    check
  check:   cargo test
  root:    /home/you/code/app · no room
  source:  machine — ~/.config/rimz/loop.toml
  strikes: 1/3
…
LAST RUN — ✗ failed (exit 101) · 0s ago · manual
  │ error: could not find `Cargo.toml` in `/home/you/code/app` or any parent directory
```

The verdict line reads the latest conclusive run, `strikes` counts consecutive failures against the threshold, and `manual` marks a run you fired by hand; a clock's fire reads `scheduled`.

Teardown is one command. `rimz loop remove <name>` deletes the entry from whichever file owns it; the run history stays. `rimz loop timer remove` takes the timer out, and `rimz uninstall` removes it too.

## Every schedule shape

Each task names one action (`--agent`, `--wake`, or a bare `--check`), carries a `--prompt` or `--prompt-file` unless it is check-only or a wake, and picks one firing shape. A clock recurs only with `--every` or `--cron`; a signal subscription keeps listening unless `--once` retires it.

| Shape | Flags | Repeats? | Example |
| --- | --- | --- | --- |
| One-shot | a bare `--at HH:MM`, or `--in <delay>` | fires once, then the task removes itself | `--in 30m` |
| Interval | `--every <duration>`, measured from the last fire | yes | `--every 15m` |
| Calendar | `--every <days> --at HH:MM`, where days is `day`, `weekday`, `weekend`, a range `mon-fri`, or a list `mon,wed,fri` | on each matching day | `--every weekday --at 07:00` |
| Raw cron | `--cron`, a five-field expression | per the expression | `--cron "*/15 * * * *"` |
| Poll-until | `--every <duration>` plus `--check`, `--on`, `--until`, and an agent action ([above](#guard-a-turn-with-a-check)) | until the check trips or the deadline passes | `--check "gh run watch --exit-status" --on success --until 30m --every 2m` |
| Signal | `--signal <name-or-family>`, narrowed with `--match key=value` ([above](#signals-the-rooms-event-bus)) | on every delivering emit, or once with `--once` | `--signal ci.failed`, `--signal 'ci.*'` |

One pair is worth a second look. `--every 1d` is an interval: it fires a day after the last fire and drifts with it. `--every day --at 07:00` is the calendar's 07:00 sharp. Calendar times, cron, `--in`, and `--until` resolve in the top-level `timezone`, falling back to the system zone when unset.

An `--agent` task takes the launch flags from [the agents guide](./fleet.md): `--worktree` hosts the pane on an isolated branch, `--mode auto|ask|yolo` sets the [permission posture](#the-permission-posture-for-unattended-runs), `--effort` and `--system-prompt-file` shape the agent, and `--timeout` caps the turn and its verify commands. The `--agent` value is a kind, a [profile](./fleet.md#profiles-shape-an-agent-for-one-job), or a kind with its permission-mode suffix, like `codex-yolo`.

Inspect, test, and manage tasks with the rest of the surface:

```sh
rimz loop list                 # every task, grouped by project, with next-fire and last-run
rimz loop watch                # live dashboard with countdowns and running tasks
rimz loop show pr-watch        # health, next fire, agent-run rollup, and recent runs
rimz loop logs pr-watch        # full forensics for recent runs
rimz loop fire pr-watch        # fire now in the foreground for testing; the schedule stays put
rimz loop fire pr-watch --keep # leave the transient pane open to inspect
rimz loop enable pr-watch      # arm locally and clear any pause or strike disable
rimz loop disable pr-watch     # hold until the next explicit enable
rimz loop pause pr-watch --for 2h
rimz loop stop pr-watch        # cancel a stuck run and release its overlap lock
rimz loop rename pr-watch ci-watch
rimz loop remove pr-watch
```

`rimz loop fire` runs every gate a scheduled fire would (budget, surplus, overlap, check), streams the agent's messages as they land, then links the completed run and its transcript. When a fire reports `previous run still active`, `rimz loop show <name>` names the active run and `rimz loop stop <name>` cancels it; stop asks the run to cancel first and sends SIGTERM only as a backstop, and if the run still holds its lock it prints the PID and lock path instead of escalating to SIGKILL.

Enablement and pauses belong to one machine and never edit the task definition. `disable` holds a task indefinitely, `pause --for <duration>` is a bounded hold that lifts itself, and `enable` clears either hold and any strike counter. `--all` applies enable or disable to every task in `rimz loop list`. A lifted hold starts the schedule fresh from that moment, so fires missed while held never replay.

Every flag is in the [loop CLI reference](../reference/cli/loop.md); the run mechanics an `--agent` task rides on (exit codes, output formats, `wait --stream`) are in [scripting](./scripting.md).

## Keep the fleet moving

An unattended agent stops for reasons that need no judgment from you. The provider's five-hour budget window empties mid-turn. The API sheds load and drops the stream. The context window fills one step short of the finish. A spent Codex account sits on reset credits that expire unused. Each stop has a known fix: wait for the reset, retry in a few minutes, compact, redeem a credit. A stock CLI leaves every one of them to whoever is watching, and overnight that is nobody.

RimZ ships those reflexes. Switched on, the room recognizes each stop from the provider's own evidence, applies the fix at the moment it can work, and the agent carries on in the same session, as if you had typed the resume yourself. Every setting is off by default and switches on with one `rimz config set`, with one exception stated under [auto-redeem](#auto-redeem): a Codex credit about to expire is redeemed whether or not you opted in. Because switching a reflex on lets RimZ type into your panes and spend on your account, each section below states the exact rules it follows.

```sh
rimz config set resume.auto_continue true     # resume rate-limit and API-error parks
rimz config set resume.auto_redeem true       # spend Codex reset credits when they buy real time
rimz config set harness.idle_compact auto     # compact warm idle contexts while work may return
rimz config set harness.smart_compact 200k    # compact before a message once context passes 200k tokens (or "70%")
```

### Auto-continue

A turn that dies on a rate limit or an API failure parks its agent: the card shows `⏸`, and the work stops until someone types `continue`. At your desk that is one keystroke. Away from it, the fleet sits idle from 1 a.m. until you notice. Auto-continue types that keystroke for you, and its policy is three decisions: what counts as evidence, which clock schedules the resume, and when to stop trying.

The evidence is the structured per-turn failure record the agent's own CLI writes when a turn dies, naming the cause. That record is the one provider input the decision reads, so every automatic resume traces back to the provider's own account of why the agent stopped, and every other kind of stop stays parked for your judgment. Because the record comes from each agent's adapter, auto-continue is a per-agent capability; [agent support](../reference/agent-support.md#notes-on-the-alpha-and-experimental-set) states it per agent. The one park that needs no provider record is the one RimZ imposed itself, a [dollar-cap park](./budget.md#what-resumes-a-parked-agent), because RimZ already knows why it stopped.

The cause picks the clock:

- A rate-limit or spend-limit park has a known end, the account window's reset. The resume fires then, the first moment it can succeed.
- A daily dollar-cap park resumes at the next local midnight, when the cap reopens.
- A transient overload or API-error park has no reset clock, so it retries on a lengthening ramp: the first attempt three minutes after the failure, then every five (`resume.auto_continue_backoff_secs`, default `[180, 300]`, last value repeating).

Each attempt is a keystroke you could have typed: when the clock fires and the agent is still parked, RimZ sends the configured nudge (`continue` by default, `resume.auto_continue_text`) into the agent's pane through the same path as `rimz message --steer`. All causes share one attempt cap (`resume.auto_continue_max_retries`, default 12, just under an hour on the default ramp); when it runs out, the card goes `failed` and routes to you like any other actionable card. Every attempt appends the park time, delivery verdict, and message id to the assist log that `rimz stats --assists` prints.

The keys are in [configuration → Resume](./configuration.md#resume); the arm, fire, and exhaust state machine is [providers.md → Auto-continue](../internals/agents/providers.md#auto-continue).

### Auto-redeem

A Codex plan grants reset credits: redeem one and a spent usage window refills on the spot. Managed by hand they waste value at both ends. A credit you forget expires unspent; a credit you hold while a spent window parks the fleet is a night of work standing still. And the timing is a real judgment call: redeemed just before the window's natural reset, a credit buys minutes; redeemed the moment a spent window blocks a night of work, it buys hours. Auto-redeem makes that call by four rules, and each redemption's record (`rimz stats --assists`) names the rule that fired:

- Expiry rescue. A credit within thirty minutes of expiring is spent rather than lost. This rule runs even with `auto_redeem` off: the capacity is already paid for.
- Blocked gain. A window is spent and its natural reset is at least `resume.auto_redeem_min_gain` away (twelve hours by default). A credit now recovers those hours, so it redeems immediately; a nearer reset means waiting is cheaper than spending.
- Doomed credit. A window is spent, and the credit would expire less than twenty-four hours after that window's natural reset. Waiting for the free reset would strand the credit, so it goes first, however far off the reset is.
- Scheduled redeem. Several credits approach expiry together. RimZ measures from your recent usage how long a fresh window takes to fill and spaces the redemptions that far apart, working back from each credit's expiry, so each one lands on a window with room to absorb it.

The reflex fails closed and paces itself. Every rule starts from a credit in hand, blocked gain and doomed credit also require a readable reset time, and attempts are throttled account-wide across every room on the machine: ten minutes between attempts, thirty after a success. A successful redemption refreshes the account reading at once, and because the refilled window is exactly the reset [auto-continue](#auto-continue) waits for, it wakes the turns parked on it. The verdict and pacing model in full are [providers.md → Auto-redeem](../internals/agents/providers.md#auto-redeem).

### Idle compaction

An idle agent can outlive its provider's warm prompt cache, so the next message pays to cache the whole accumulated conversation again. `harness.idle_compact = "auto"` submits the agent's own compact command after 59 minutes of inactivity while a same-channel teammate is still working or the worktree's pull request remains open; `"always"` applies the reflex to every eligible idle agent. `harness.idle_compact_after` changes the threshold, with a duration such as `"45m"` or `"2h"`.

The reflex applies only to top-level agents whose adapter exposes a compact command and whose occupied context is at least 50,000 tokens. Working, waiting, parked, and already-compacting agents stay untouched, and delivery waits for an idle turn boundary. Each idle stretch compacts at most once.

### Smart compaction

Every agent CLI already compacts. `/compact` is the manual command: summarize the conversation and carry on against a fresh window. Auto-compaction is its fallback, firing when the context hits the ceiling, wherever the work happens to stand. Driving one agent by hand, you preempt the fallback without thinking about it: a task wraps up, you type `/compact` at the clean boundary, and the summary hands a finished state to whatever comes next.

In a fleet, most prompts arrive with no human there to make that call. A reviewer sends comments back to a coder sitting at 90% context; the coder takes the message, edits two files, hits the ceiling, and the automatic summary captures a half-changed tree mid-fix. Smart compaction restores the by-hand habit at the same spot. When a `rimz message` or a scheduled wake is about to land and the receiver's context has passed your threshold, RimZ submits the agent's compact command first, then delivers the text against the fresh window. A message boundary is the strongest checkpoint available: the previous task has ended and the next has not begun, so the summary is a handover.

Set the default once with `harness.smart_compact`, an occupied-token count like `200k` or a percentage like `70%`, and every message send and scheduled wake inherits it; or leave it unset and pass `--smart-compact` per message. [`harness.compact_instruction`](./configuration.md#smart-compaction) changes the summary brief. The threshold grammar and delivery mechanics are in [messaging → land against a fresh window](./messaging.md#land-against-a-fresh-window).

## The permission posture for unattended runs

An unattended run has to answer permission prompts without you. Two patterns cover it, and they compose.

Answer in the agent's own UI to keep the full record. A [handler that acts](./notifications.md#handlers-that-act-not-just-alert) sends the answer with `rimz pane send`, leaving the prompt, the answer, and the tool run all in the agent's transcript, exactly as if you had typed it. Prefer this path when handled decisions belong on the record.

Use the agent's bypass flag when the run cannot afford to stop. `--mode yolo` on the task passes the adapter's bypass flag to the scheduled turn (`claude --dangerously-skip-permissions`, `codex --dangerously-bypass-approvals-and-sandbox`), while `--mode ask` keeps the provider's prompts in place; the modes are the same ones [`rimz agents`](./fleet.md#set-a-permission-mode) takes. RimZ still observes sessions, completions, and failures through lifecycle hooks, but the agent skips permission events at the source, so the durable record has no per-decision audit trail, only what the other hooks report. Reserve the flag for runs where you accept that missing trail.

The guardrails around either posture stay visible: trust grants, notification handlers, and the posture itself are product behavior, covered in [security](./security.md).

## A fleet that works nights

A hands-off room composes four layers, from least to most involved: [the recovery reflexes](#keep-the-fleet-moving) keep one agent alive through rate limits with no schedule at all; a scheduled turn puts work on a clock; a [check guard](#guard-a-turn-with-a-check) fires that turn only when a condition trips; and a [notification handler](./notifications.md) catches what none of them can decide alone. Reach for the lowest layer that solves the problem, and stack them when the job needs it.

[Scripting](./scripting.md) turns an agent into a shell command, and a loop puts that command on a clock. Because a scheduled turn runs inside the room, its prompt can use every other RimZ command: `-p` subagents, teams, worktrees, messages.

```sh
# every 15m: CI on the release PR fixes itself
rimz loop add ci-fix --check "gh run watch --exit-status" --on fail \
    --agent codex --prompt "CI failed on the release PR; read the failing job's logs and fix it" --every 15m

# 02:00 every night: a triage that fans out and opens PRs
rimz loop add nightly --agent claude --worktree nightly --timeout 4h --budget 5 --budget-per-day 20 --every day --at 02:00 \
    --prompt "Scan the repository for bugs and cheap improvements. For each one worth fixing, \
run a codex -p subagent in its own worktree, review its diff, and open a PR."
```

The nightly task is one scheduled turn, but its prompt hands the agent the room's own tools: it fans work out with [`-p` subagents](./scripting.md#agents-scripting-agents), isolates each fix in a [worktree](./worktrees.md), and could as easily launch a [team](./teams.md) and brief it over [messages](./messaging.md).

Leave the room open, detached on your workstation or on a server you reach with [`rimz remote`](./remote.md), and the night runs on the pieces above: [auto-continue](#auto-continue) carries recoverable runs over rate limits, a question or failure trips a [notification handler](./notifications.md) that reaches your phone, and the [permission posture](#the-permission-posture-for-unattended-runs) stays a per-task choice. By morning `rimz loop list` and the PR queue show what the night produced.

## See also

- [Scripting agents](./scripting.md): the supervised-run mechanics every scheduled `--agent` task rides on, including exit codes, `--output-format`, and `wait --stream`.
- [Budgets](./budget.md): the dollar caps that bound hands-off work, and the surplus gate's headroom model.
- [Notifications](./notifications.md): the push routes and acting handlers that catch what a loop cannot handle alone.
- [Messaging](./messaging.md): the delivery path `--wake` uses, `--schedule` for one-off reminders, and smart compaction in full.
- [Loop CLI](../reference/cli/loop.md), [Wake CLI](../reference/cli/wake.md), and [Events CLI](../reference/cli/events.md): every flag, the watcher and `--wait`, and the signal grammar.
- [Configuration](./configuration.md): the `[resume]` and `[harness]` keys, and the `loop.toml` shape.
- [Security and trust](./security.md): the safety posture for bypass flags and project trust.
- [loops.md](../internals/harness/loops.md): the clock, state files, and run log underneath.
