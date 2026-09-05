# Loops

> `rimz loop` is cron for your agents. A task is a name, a trigger, and one action: start a fresh agent turn, wake an agent that is already running, or run a shell command that guards either. The clock, the state files, and the run log underneath are in [loops.md](../internals/harness/loops.md).

## Why rimz loop

You already run things on a clock. Cron lines and systemd timers drive your maintenance, CI fires on a schedule, and lately a `while true; do claude -p "fix the next failing test"; sleep 900; done` is holding a terminal somewhere. The instinct behind that loop is right: state the goal once and let the loop iterate instead of you re-prompting by hand.

But cron and a hand-rolled `while` loop only know one move: start a fresh process and walk away. That scheduled `claude -p` is invisible, so it hangs in silence the first time it stops for a permission prompt. It fires whether or not there is anything to do, spending a whole agent turn to re-check a world that has not changed. And it cannot hand work to an agent you already have running; every run starts from an empty context.

`rimz loop` is the same clock with the room behind it. A task has one action and up to two shell-command gates:

- `--agent <kind>` starts a fresh supervised agent that runs the prompt once and cleans up.
- `--wake @handle` delivers the prompt to an agent you already have running, so the work resumes in that conversation with all of its context.
- `--check <cmd>` runs before the action and spends a turn only on its result, so a scheduled agent never starts just to find nothing to do.
- `--verify <cmd>` runs after an `--agent` turn and re-prompts the same session until the command passes.

Whichever action fires, the turn is a full room citizen: a live card in the sidebar, a permission question that routes to you instead of hanging the job, and a line in the run log that `rimz loop show` summarizes and `rimz loop logs` prints in full.

One rule governs every clock: a task repeats only when `--every` or `--cron` says so. A bare time (`--at 07:00` or `--in 30m`) fires once and then removes itself.

## Schedule a fresh turn

`--agent` starts a new agent for the turn. On the schedule you set, it opens a fresh supervised pane, runs the prompt once, and cleans up, the same run `rimz agents <kind> "<prompt>" -p` makes when you type it yourself. Reach for it whenever the work should begin from a clean context: a nightly repository sweep, a Monday dependency check, a scheduled security audit, a recurring status report. Each fire is a new agent with no memory of the last one, which is the point when the job is "do this again against today's tree."

```sh
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

The receipt names the action, the schedule in plain words, and the concrete next fire. The last two lines are the one thing about the model to know before your first task: the open room keeps time, and with no room open nothing fires until you start one or install the timer ([who keeps time](#who-keeps-time)).

That one line stands in for a cron entry, the guard script around it, and the terminal you would have left open to watch it. It runs Claude in an isolated [worktree](./worktrees.md) every Monday at 09:00, and if a bump breaks the build the agent stops and asks instead of forcing it through: the question reaches you like any other waiting card. The launch-shaping flags from [the agents guide](./fleet.md) ride along, so `--worktree`, `--mode`, `--effort`, `--system-prompt-file`, and `--timeout` shape a scheduled turn the same way they shape an interactive one ([full list below](#every-schedule-shape)). What the agent does with the turn is bounded only by the prompt: a one-line check, or the whole [fleet that works nights](#a-fleet-that-works-nights).

A scheduled turn reports its completion through the agent's hooks, so `rimz loop add --agent` refuses until those hooks are installed:

```console
$ rimz loop add deps --agent codex --every mon --at 09:00 --prompt "…"
error: codex hooks are not installed, so a scheduled turn cannot report completion
  install them with `rimz hooks install codex`
```

Codex additionally has to trust the installed hooks once (`/hooks` inside Codex), and the same error names that step when it is the one missing. [Set up your machine](./setup.md) covers both.

## Wake a running agent

An agent's work often ends in a wait. CI has twenty minutes left, a reviewer owes comments, a deploy is baking. The agent has nothing to do until then, and the follow-up falls to you: remember to check CI, then tell the agent to merge. The two habits an agent falls into on its own are worse: a `sleep 900` in its shell tool, which holds the turn open for fifteen minutes of nothing, or a poll loop like `while ! gh run watch --exit-status; do sleep 30; done`, which keeps the pane busy until the command finishes. Both spend a live agent on watching a clock.

A wake inverts that. The agent (or you) arms a condition, the agent ends its turn and goes idle, and when the condition trips a message lands in that same conversation. The agent picks up where it left off, with all of its context. An idle agent takes the message at once; a mid-turn agent takes it at its next turn boundary.

There are two commands, and the choice is about who arms the wake and how often it fires.

`rimz wake` is the one-shot form an agent uses on itself. It fires once and retires, and with no address it comes back to the caller. Three triggers cover the waits an agent meets:

```sh
rimz wake --in 30m --prompt "CI should be done; check the run and merge if green"
rimz wake -- gh run watch --exit-status
rimz wake --signal deploy.finished --prompt "the deploy landed; run the smoke tests"
```

- `--in 30m` is the alarm clock. The room's clock fires it, so the room has to be open (or the [loop timer](#who-keeps-time) installed), and the delay has to be under 24 hours.
- A command after `--` is the poll loop without the pane. RimZ runs it in a detached watcher process that outlives the turn, and when it exits the agent is woken with the command, its exit status, and its output tail already in the prompt. `--on fail` or `--on success` wakes only for that outcome; `--timeout 30m` gives up on a command that never ends.
- `--signal deploy.finished` waits on a named event instead of a command. The [next section](#signals-the-rooms-event-bus) is where signals come from.

Pass `@handle` to wake someone else. `rimz wake list` shows what is pending, with the trigger, the target, and whether a watcher is still running; `rimz wake cancel <name>` calls it off. A chain of wakes is a self-paced loop: each turn arms the next only while work remains, so the loop stops when the goal is met and never when a counter runs out. When the agent has nothing else to do until a command finishes, `rimz wake --wait -- cargo build` blocks and prints the outcome inline instead; reach for that in a script.

`rimz loop add --wake` is the standing form. Point it at a running agent and give it any trigger a task takes:

```sh
rimz loop add check-ci --wake @planner --prompt "CI should be done; check the run and merge if green" --in 30m
rimz loop add merged --signal pr.merged --once --wake @planner --prompt "PR #{{number}} merged; start the follow-up"
```

RimZ resolves `@planner` the moment you add the task, exactly as [`rimz message`](./messaging.md) would, and pins that one session; the receipt names it. Thirty minutes later the prompt travels the same durable delivery path as any message. A session that has exited by then is skipped and the task removed, because that conversation cannot come back. A loop wake is the right tool when the wake should repeat with `--every`, keep listening on a [signal](#signals-the-rooms-event-bus), or wait behind a [`--check` guard](#guard-a-turn-with-a-check). For a plain reminder with no condition at all, [`rimz message --schedule`](./messaging.md) is lighter still: one delivery on a timer, no task.

## Signals: the room's event bus

A wake on a timer is a guess about when something will happen. A signal is the thing itself. `rimz events emit` puts a named event into the room, and every wake and loop task subscribed to that name fires:

```sh
rimz events emit deploy.finished --json '{"env":"prod","version":"1.4.2"}'
```

```console
emitted deploy.finished (evt_01a07076a71c72e09f0461533909ceb1) · fired 1 tasks
  smoke
```

Anything that can run a command can emit: a CI job's last step, a git hook, a deploy script, a cron line, another agent. The emitting process fires the subscribers itself, so this works with no daemon and no open room. Nothing is queued or replayed either: a signal reaches the wakes armed at that instant, which is why an agent arms its wake before starting the work that will emit it.

RimZ also emits signals from what the room already watches, so the most common waits need no wiring at all:

| Signal | Fires when | Payload carries |
| --- | --- | --- |
| `pr.merged`, `pr.closed` | a pull request the room tracks for a worktree leaves the open state | `path`, `branch`, `repo`, `number`, `url`, `head`, `state` |
| `ci.finished` | the checks for a tracked worktree settle on success or failure, with or without a pull request | the same fields plus `conclusion` |
| `agent.started`, `agent.idle`, `agent.waiting`, `agent.failed`, `agent.ended` | an agent's own lifecycle transitions | `kind`, `session`, `handle`, `status`, `errored` |

`--match KEY=VALUE` filters on those fields, and `{{key}}` in the prompt is replaced by the value, so the woken agent reads the specifics instead of going to look them up:

```sh
rimz wake --signal ci.finished --match conclusion=failure --prompt "CI failed on {{branch}}; read the failing job and fix it"
```

Waking on an `agent.*` signal must name someone else (`--match handle=@coder` or `--match session=<id>`); RimZ refuses the arrangement where an agent's own idle transition wakes it into another turn. For a subscription that keeps listening instead of retiring after one fire, `rimz loop add <name> --signal ci.finished --agent codex --prompt "…"` is the standing form; add `--once` when the task should retire like a wake.

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

The check runs first, every time, at the project root, and costs nothing. Only its result spends a turn: `--on fail` (the default) wakes the agent on a non-zero exit or a timeout, `--on success` on a zero exit. When the guard fires, RimZ appends the command, its exit status, and its output tail to the prompt, so the agent wakes already reading the evidence. That adds a rung to the escalation ladder: the script handles the routine, the agent handles the failure, and you hear about it only when the agent itself gets stuck. Its turn is supervised like any other, so a stuck fix goes `? waiting` and a [notification](./notifications.md) reaches you. `--until 30m` is the poll-until deadline: the task retires when the check trips or the deadline passes, whichever comes first.

A `--check` with no agent action is still worth having. It is a scheduled command that logs `completed`, `failed`, or `timed out`, each with the exit code and output tail, into the run history, and it keeps recurring. Checks have a five-minute timeout by default.

`--check` gates firing; `--verify` gates completion. Add `--verify "cargo test"` to a scheduled `--agent` task when the command is the definition of done: a red result returns its evidence to the same live session, up to `--max-attempts` total turns (default 3), before the fire records `verify failed`. It behaves exactly as it does on a hand-run `-p`, described under [verify and retry](./scripting.md#verify-and-retry).

## Gate a task on surplus

Recurring background work is the natural fill for a loop: refactor the next rough module, close test gaps, triage dependencies. None of it is urgent, which is exactly why it must never crowd out the work that is. A background task draws on the same subscription window as your own sessions, and a schedule cannot tell a light week from a heavy one: fired blindly, the 03:00 task spends the budget your Tuesday afternoon needed.

`--surplus` puts that judgment in the schedule. Before each fire, RimZ reads the provider's pacing window and computes forward headroom, how far ahead of the sustainable pace the window is running: `1.0x` is exactly on pace, and `1.5x` means half again as much budget remains as the clock requires.

```sh
rimz loop add refactor --agent claude --prompt "Refactor the next rough module and leave the branch green" \
    --every 4h --surplus 1.5x --surplus-after 3d
```

`--surplus 1.5x` opens the gate only at that headroom or above. `--surplus-after 3d` keeps the task quiet until three days of the window have elapsed, so an untouched early week is not spent before your own heavy days land (used alone, it still requires `1.0x`). The gate guards `--agent` and `--wake` actions alike and runs before any `--check`, so a closed gate runs nothing and costs nothing: the fire records `surplus skipped` without adding a strike, and the schedule keeps polling until real slack appears. An account without a window reading (an API key, or a window that has not started) keeps the gate closed. Which window counts per provider, the headroom model with a worked example, and the fail-closed rules are in [budgets → the surplus gate](./budget.md#the-surplus-gate).

## Budgets and strikes

Two brakes bound a task that runs without you.

Dollar budgets cap what a task spends. `--budget 5` caps each fired run. `--budget-per-day 20` (which requires `--budget`) sums that task's run costs in the configured local day and skips a fire that cannot fund its per-run cap, recording `budget skipped`. `rimz loop list` shows each task's spend against its daily cap, and `rimz loop show` breaks the costs down per run. The room-fleet and provider-account daily caps gate the same fires before launch, and a spent provider quota records the same `budget skipped` before the task's check or pane exists. The whole cap model, and why a human message can waive an interactive turn but never satisfies a loop gate, is the [budgets guide](./budget.md).

Repeated failures disable the task. Three consecutive failed fires auto-disable it, show `disabled · 3 strikes` in `rimz loop list`, and fire [notification handlers](./notifications.md) with kind `loop_disabled`. A turn that completed but left its check red counts as a failure, because the task is not doing its job; a skipped gate (`budget skipped`, `surplus skipped`, an overlapping run) is neutral; a healthy check or a successful turn resets the counter. `rimz loop show` prints the running count, and `rimz loop enable <name>` clears the strikes and re-arms the schedule. `--max-strikes <N>` changes the threshold per task, and `--max-strikes 0` disables the gate. `rimz loop fire` still runs a disabled task for a manual test.

## What a task does on your machine

`rimz loop add` edits one file and starts no process.

A repeating task (`--every`, `--cron`, or `--signal`) appends a `[tasks.<name>]` entry to `~/.config/rimz/loop.toml`: per-machine automation, like your crontab, never inherited by a cloned repository. The file is plain TOML you can edit by hand, and the `deps` task above lands as:

```toml
[tasks.deps]
agent = "claude"
prompt = "Check for outdated dependencies. Open a PR for the safe minor and patch bumps, and list any majors that need review."
root = "/home/you/code/app"
worktree = "deps"
at = "09:00"
every = "mon"
```

A one-shot (a bare `--at`, `--in`, a `--until` deadline, a `--once` subscription, or any `rimz wake`) is stored as state in `~/.local/state/rimz/loop-instances.json` instead, so an agent scheduling its own wake never touches your `loop.toml`; the entry retires itself after firing. `rimz loop list` shows the source of each task as `machine` or `state`.

`--project` writes the entry to `<root>/.rimz/config.toml`: shared automation that travels with the repo, so it has to be a repeating task, and it cannot use `--wake` (a session pinned on your machine means nothing on someone else's). A committed task runs commands on whoever pulls it, so it enters the [project trust hash](./security.md) and stays inert until each user approves it. Trust and enablement answer different questions: trust says the project config contains commands you accept as yours to run, and `rimz loop enable <name>` says this particular task may run unattended on this machine. A project task pulled from a repo starts disabled even after trust is granted; a task you create with `rimz loop add --project` starts enabled on your machine. A trusted project task wins over a same-named machine task without double-firing.

### Who keeps time

There is no RimZ scheduler daemon. While a room for the task's project is open, attached or not, that room's sidebar process fires due tasks on its regular tick, each through a detached `rimz loop run <name>`. With no room open, clock tasks wait. To keep time without an open room, opt in once:

```sh
rimz loop timer install
```

That installs one systemd user timer on Linux or launchd agent on macOS. Once a minute it runs a one-off RimZ tick, re-reads every task, and fires only projects without an open room; an open room still wins for its own tasks. An `--agent` fire from the timer starts the room through the normal supervised path and leaves it open, a check-only task needs no room, and a `--wake` still needs its pinned session alive. `rimz loop timer status` shows whether it is installed; `rimz loop timer remove` reverses it.

Only clocks need a timekeeper. A signal task and a `rimz wake` watcher are fired by the process that emits the signal or runs the command, whether or not a room is open.

With or without the timer, nothing is replayed. A task is armed the first time a clock sees it and fires on its next occurrence after that, so opening a room hours late never sets off a catch-up storm.

### Where a scheduled turn runs

A scheduled `--agent` fire opens its pane in the room's background `rimzd` tab, stacked under the live loop dashboard that lives there, so it never splits the tab you are working in. If that dashboard pane was closed, RimZ recreates it at fire time; if the whole `rimzd` tab is gone, the run falls back to a new tab. A manual `rimz loop fire` opens the pane beside your shell instead, so its output stays in front of you.

Every scheduled `--agent` turn gets a timeout: the task's own `--timeout`, or the machine's `loop.default-timeout` (two hours by default). A manual `rimz loop fire` runs unbounded unless the task sets its own.

### What a fire leaves behind

A fire leaves two things: whatever the task did (one transient supervised pane for `--agent`, one delivered message for `--wake`), and one line of run history in `~/.local/state/rimz/loop-runs.log.jsonl`. `rimz loop show <name>` gives that history a health verdict; `rimz loop logs <name>` prints the complete stored forensics.

```console
$ rimz loop show cert
cert — every 1h
  ✗ failing · failed (exit 1) ×1 since 0s ago
  task:    check
  check:   false
  root:    /home/you/code/app · no room
  source:  machine — ~/.config/rimz/loop.toml
  strikes: 1/3

RECENT RUNS (1 recorded)
  WHEN    MODE    STATUS             TOOK  COST
  0s ago  manual  ✗ failed (exit 1)  24ms     -
```

Teardown is one command. `rimz loop remove <name>` deletes the entry from whichever file owns it; the run history stays. `rimz loop timer remove` takes the timer out, and `rimz uninstall` removes it too.

## Every schedule shape

Each task names one action (`--agent`, `--wake`, or a bare `--check`), carries a `--prompt` or `--prompt-file`, and picks one firing shape. Only `--every` or `--cron` makes it recur.

| Shape | Flags | Repeats? | Example |
| --- | --- | --- | --- |
| One-shot | a bare `--at HH:MM`, or `--in <delay>` | fires once, then the task removes itself | `--in 30m` |
| Interval | `--every <duration>`, measured from the last fire | yes | `--every 15m` |
| Calendar | `--every <days> --at HH:MM`, where days is `day`, `weekday`, `weekend`, a range `mon-fri`, or a list `mon,wed,fri` | on each matching day | `--every weekday --at 07:00` |
| Raw cron | `--cron`, a five-field expression | per the expression | `--cron "*/15 * * * *"` |
| Poll-until | `--every <duration>` plus `--check`, `--on`, `--until`, and an agent action ([above](#guard-a-turn-with-a-check)) | until the check trips or the deadline passes | `--check "gh run watch --exit-status" --on success --until 30m --every 2m` |
| Signal | `--signal <name>`, narrowed with `--match key=value` ([above](#signals-the-rooms-event-bus)) | on every matching emit, or once with `--once` | `--signal ci.finished --match conclusion=failure` |

One pair is worth a second look. `--every 1d` is an interval: it fires a day after the last fire and drifts with it. `--every day --at 07:00` is the calendar's 07:00 sharp. Calendar times, cron, `--in`, and `--until` resolve in the top-level `timezone`, falling back to the system zone when unset.

An `--agent` task takes the launch flags from [the agents guide](./fleet.md): `--worktree` hosts the pane on an isolated branch, `--mode auto|ask|yolo` sets the [permission posture](#the-permission-posture-for-unattended-runs), `--effort` and `--system-prompt-file` shape the agent, and `--timeout` caps the turn and its verify commands. The `--agent` value is a kind, a profile, or a virtual cell like `codex-yolo`.

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

Enablement and pauses belong to one machine and never edit the task definition. `disable` holds a task indefinitely, `pause --for <duration>` is a bounded hold that lifts itself, and `enable` clears either hold and any strike counter. `--all` applies enable or disable to every task in `rimz loop list`. Each lift becomes the new schedule edge, so missed fires never replay.

Every flag is in the [loop CLI reference](../reference/cli/loop.md); the run mechanics an `--agent` task rides on (exit codes, output formats, `wait --stream`) are in [scripting](./scripting.md).

## Keep the fleet moving

An unattended agent stops for reasons that need no judgment from you. The provider's five-hour budget window empties mid-turn. The API sheds load and drops the stream. The context window fills one step short of the finish. A spent Codex account sits on reset credits that expire unused. Each stop has a known fix: wait for the reset, retry in a few minutes, compact, redeem a credit. A stock CLI leaves every one of them to whoever is watching, and overnight that is nobody.

RimZ ships those reflexes. Switched on, the room recognizes each stop from the provider's own evidence, applies the fix at the moment it can work, and the agent carries on in the same session, as if you had typed the resume yourself. Every setting is off by default and switches on with one `rimz config set`. Because switching one on lets RimZ type into your panes and spend on your account, each section below states the exact rules the reflex follows.

```sh
rimz config set resume.auto_continue true     # resume rate-limit and API-error parks
rimz config set resume.auto_redeem true       # spend Codex reset credits when they buy real time
rimz config set harness.idle_compact auto     # compact warm idle contexts while work may return
rimz config set harness.smart_compact 200k    # compact before a message once context passes 200k tokens (or "70%")
```

### Auto-continue

A turn that dies on a rate limit or an API failure parks its agent: the card shows `⏸`, and the work stops until someone types `continue`. At your desk that is one keystroke. Away from it, the fleet sits idle from 1 a.m. until you notice. Auto-continue types that keystroke for you, by three rules: what counts as evidence, which clock schedules the resume, and when to stop trying.

The evidence is the structured per-turn failure record the agent's own CLI writes when a turn dies, naming the cause. That record is the one input the decision reads, so every automatic resume traces back to the provider's own account of why the agent stopped, and every other kind of stop stays parked for your judgment. Because the record comes from each agent's adapter, auto-continue is a per-agent capability; [agent support](../reference/agent-support.md#notes-on-the-alpha-and-experimental-set) states it per agent.

The cause picks the clock:

- A rate-limit or spend-limit park has a known end, the account window's reset. The resume fires then, the first moment it can succeed.
- A transient overload or API-error park has no reset clock, so it retries on a lengthening ramp: the first attempt three minutes after the failure, then every five (`resume.auto_continue_backoff_secs`, default `[180, 300]`, last value repeating).

Each attempt is a keystroke you could have typed: when the clock fires and the agent is still parked, RimZ sends the configured nudge (`continue` by default, `resume.auto_continue_text`) into the agent's pane through the same path as `rimz message --steer`. All causes share one attempt cap (`resume.auto_continue_max_retries`, default 12, just under an hour on the default ramp); when it runs out, the card goes `failed` and routes to you like any other actionable card. Every attempt appends the park time, delivery verdict, and message id to the assist log that `rimz stats --assists` prints.

The keys are in [configuration → Resume](./configuration.md#resume); the arm, fire, and exhaust state machine is [providers.md → Auto-continue](../internals/agents/providers.md#auto-continue).

### Auto-redeem

A Codex plan grants reset credits: redeem one and a spent usage window refills on the spot. Managed by hand they waste value at both ends. A credit you forget expires unspent; a credit you hold while a spent window parks the fleet is a night of work standing still. And the timing is a real judgment call: redeemed just before the window's natural reset, a credit buys minutes; redeemed the moment a spent window blocks a night of work, it buys hours. Auto-redeem makes that call by four rules, and each redemption's record (`rimz stats --assists`) names the rule that fired:

- Expiry rescue. A credit within thirty minutes of expiring is spent rather than lost. This rule runs even with `auto_redeem` off: the capacity is already paid for.
- Blocked gain. A window is spent and its natural reset is at least `resume.auto_redeem_min_gain` away (twelve hours by default). A credit now recovers those hours, so it redeems immediately; a nearer reset means waiting is cheaper than spending.
- Doomed credit. The natural reset is near, but the credit would keep less than twenty-four hours of useful life after it. Waiting for the free reset would strand the credit, so it goes first.
- Scheduled redeem. Several credits approach expiry together. RimZ measures from your recent usage how long a fresh window takes to fill and spaces the redemptions that far apart, working back from each credit's expiry, so each one lands on a window with room to absorb it.

The reflex fails closed and paces itself. Every rule starts from a credit in hand, blocked gain and doomed credit also require a readable reset time, and attempts are throttled account-wide across every room on the machine: ten minutes between attempts, thirty after a success. A successful redemption refreshes the account reading at once, and because a refilled window is certified recovered capacity, [auto-continue](#auto-continue) wakes the parked turns on it. The verdict and pacing model in full are [providers.md → Auto-redeem](../internals/agents/providers.md#auto-redeem).

### Idle compaction

An idle agent can outlive its provider's warm prompt cache, so the next message pays to cache the whole accumulated conversation again. `harness.idle_compact = "auto"` submits the agent's own compact command after 59 minutes of inactivity while a same-channel teammate is still working or the worktree's pull request remains open; `"always"` applies the reflex to every eligible idle agent. `harness.idle_compact_after` changes the threshold, with a duration such as `"45m"` or `"2h"`.

The reflex applies only to top-level agents whose adapter exposes a compact command and whose occupied context is at least 50,000 tokens. Working, waiting, parked, and already-compacting agents stay untouched, and delivery waits for an idle turn boundary. Each idle stretch compacts at most once.

### Smart compaction

Every agent CLI already compacts. `/compact` is the manual command: summarize the conversation and carry on against a fresh window. Auto-compaction is its fallback, firing when the context hits the ceiling, wherever the work happens to stand. Driving one agent by hand, you preempt the fallback without thinking about it: a task wraps up, you type `/compact` at the clean boundary, and the summary hands a finished state to whatever comes next.

In a fleet, most prompts arrive with no human there to make that call. A reviewer sends comments back to a coder sitting at 90% context; the coder takes the message, edits two files, hits the ceiling, and the automatic summary captures a half-changed tree mid-fix. Smart compaction restores the by-hand habit at the same spot. When a `rimz message` or a scheduled wake is about to land and the receiver's context has passed your threshold, RimZ submits the agent's compact command first, then delivers the text against the fresh window. A message boundary is the strongest checkpoint available: the previous task has ended and the next has not begun, so the summary is a handover and not a snapshot of work in flight.

Set the default once with `harness.smart_compact`, an occupied-token count like `200k` or a percentage like `70%`, and every message send and scheduled wake inherits it; or leave it unset and pass `--smart-compact` per message. [`harness.compact_instruction`](./configuration.md#smart-compaction) changes the summary brief. The threshold grammar and delivery mechanics are in [messaging → land against a fresh window](./messaging.md#land-against-a-fresh-window).

## The permission posture for unattended runs

An unattended run has to answer permission prompts without you. Two patterns cover it, and they compose.

Answer in the agent's own UI to keep the full record. A [handler that acts](./notifications.md#handlers-that-act-not-just-alert) sends the answer with `rimz pane send`, leaving the prompt, the answer, and the tool run all in the agent's transcript, exactly as if you had typed it. Prefer this path when handled decisions belong on the record.

Use the agent's bypass flag when the run cannot afford to stop. `--mode yolo` on the task passes the adapter's bypass flag to the scheduled turn (`claude --dangerously-skip-permissions`, `codex --dangerously-bypass-approvals-and-sandbox`), while `--mode ask` keeps the provider's prompts in place; the modes are the same ones [`rimz agents`](./fleet.md#set-a-permission-mode) takes. RimZ still observes sessions, completions, and failures through lifecycle hooks, but the agent skips permission events at the source, so the durable record holds what other hooks report and no per-decision audit trail. Reserve the flag for runs where you accept that missing trail.

The guardrails around either posture stay visible: trust grants, notification handlers, and the posture itself are product behavior, covered in [security](./security.md).

## A fleet that works nights

A hands-off room composes four layers, from least to most involved: [the recovery reflexes](#keep-the-fleet-moving) keep one agent alive through rate limits with no schedule at all; a scheduled turn puts work on a clock; a [check guard](#guard-a-turn-with-a-check) fires that turn only when a condition trips; and a [notification handler](./notifications.md) catches what none of them can decide alone. Reach for the lowest layer that solves the problem, and stack them when the job needs it.

[Scripting](./scripting.md) turns an agent into a shell command, and a loop puts that command on a clock. Because a scheduled turn is a full room citizen, it reaches every other primitive: `-p` subagents, teams, worktrees, messages.

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
