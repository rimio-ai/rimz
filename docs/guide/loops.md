# Loops

You already run things on a clock. Cron lines and systemd timers drive your maintenance, CI fires on a schedule, and lately a `while true; do claude -p "fix the next failing test"; sleep 900; done` is holding a terminal somewhere. The instinct behind that loop is right: state the goal once and let the loop iterate instead of you re-prompting by hand.

But cron and a hand-rolled `while` loop only know a clock. They cannot fire when a pull request merges, when CI goes red, or when a team's board reaches Done, so they poll, and a polled `claude -p` spends a whole agent turn re-checking a world that has not changed. What they start is invisible too: the scheduled `claude -p` hangs in silence the first time it stops for a permission prompt.

`rimz loop` is the same clock with the room behind it. A task is a name, one trigger, and one command, written after `--` exactly as you would type it in a shell. The loop decides *when* the command runs; the command says *what* happens. To start an agent, the command is the `rimz agents` launch you already use:

```sh
rimz loop add deps --every mon --at 09:00 --fresh-worktree deps -- rimz agents claude --detach "Check for outdated dependencies. Open a PR for the safe minor and patch bumps, and list any majors that need review."
```

The trigger is a clock, a signal, or a condition on the room's state. A clock repeats only when `--every` or `--cron` says so; a bare time (`--at 07:00`, `--at 'fri 09:30'`, `--at '2026-12-24 18:00'`, or `--in 30m`) fires once and the task removes itself. A signal or condition task fires once too, unless `--repeat` keeps it listening.

## Run a command on a schedule

The words after `--` are the command, handed to the process word for word, as cron hands its line to a shell, except that no shell reads them: a `$`, a `*`, or a quote arrives as written. When you want a pipeline or a variable, write the shell yourself:

```sh
rimz loop add watchdog --every 15m --timeout 20m -- sh -c 'cargo test 2>&1 | tail -n 50'
```

Each fire runs the command once and lasts exactly as long as it does. Exit `0` records the fire `completed`; any other exit, or a command still running at the task's `--timeout`, records it `failed`. The timeout is written to the task when you add it: `--timeout`, else `loop.default-timeout`, else two hours. The command's output stays with the fire, and `rimz loop show` and `rimz loop logs` print it. Run the add from the project the task belongs to:

```console
$ rimz loop add deps --every mon --at 09:00 --fresh-worktree deps -- rimz agents claude --detach 'Check for outdated dependencies.'
added loop task `deps`
action: runs `rimz agents claude --detach 'Check for outdated dependencies.'` in a fresh worktree `deps-<time>` per fire, branched from trunk · timeout 2h
trigger: fires every Mon at 09:00
next fire: 2026-11-16 09:00 (in 2d)
live while a room for /home/you/code/app is open
no room is open there; start one with `rimz start`, or use `rimz loop timer install` to fire without one
```

The receipt reads back the command, where it runs, its timeout, the schedule in plain words, and, for a clock, the concrete next fire. Two facts in it matter before your first task. A task is bound to the project you ran the command in (`--root <path>` picks another), runs at that project's root even when you add it from a linked worktree unless `--fresh-worktree` gives it a fresh worktree per fire, and fires only while a room for that project is open, or while the [loop timer](#who-keeps-time) keeps time for it. Without a room, `rimz loop list` tells you whether the timer is active: while it is, clock rows read as they do in an open room, and while it is not, each says `waits for room` beside its trigger. The name is optional: leave it out and the receipt prints the generated `loop-<petname>` it chose.

## Start an agent from a task

A command that launches an agent is the launch you would type, so every flag you use with [`rimz agents`](./fleet.md) shapes it the same way: a [profile](./fleet.md#profiles-shape-an-agent-for-one-job), `--effort`, a [permission mode](./fleet.md#set-a-permission-mode), `--worktree`. The one choice the loop adds is how long the fire owns the agent:

- **A prompted launch without `--detach` is a run the fire owns.** It behaves as `-p` does: the command waits for the agent's turn, captures its answer in the fire's output, and exits with the run's exit code, so the fire is `completed` when the turn succeeds and `failed` when it fails. The pane opens in an unfocused tab beside the room's loop dashboard and closes when the turn ends, unless `--keep` holds it open. The fire owns the agent until it ends: when the fire ends first, by the task's `timeout`, a replacing trigger, `rimz loop stop` or `rimz workflow cancel`, or because you redefined or removed the task, its agent is stopped with it. `--keep` holds the pane and not the turn: a kept agent is stopped too, and its pane stays open for you to read until you press Ctrl-C in it. Reach for it when the turn is the job: a nightly sweep, a status report, a fix that should count as a strike when it fails.
- **`--detach` hands the agent off.** The launch returns at once, the fire ends with it, and the agent keeps running on its own, as it would from your shell. Reach for it when the agent should stay: a team that works the branch, an agent that waits for your review.

```sh
rimz loop add sweep --every day --at 02:00 --fresh-worktree sweep -- rimz agents claude --timeout 1h "Scan the repository for cheap improvements and open a PR for each one worth fixing."
rimz loop add review-merge --when 'pr[topic]=merged' -- rimz teams launch recon -w review-merge --detach "Review the merged work and propose focused follow-up."
```

Either way, the launched agent's prompt ends with a line saying which task fired it and why, such as ``Rule `smoke` fired here. deploy.finished · env: prod``, so an unattended agent knows what happened without asking. A plain command reads the same line from `RIMZ_LOOP_TRIGGER`, and `RIMZ_LOOP_TASK` names the task. A prompted launch with no `--detach` must have a prompt and refuses `--bg`, since the fire has to wait for it to end. It is one agent: a team of more than one role is launched with `--detach`, and the launch says so when it is missing.

A detached agent carries the task's name and the chain of agents that booked it as provenance, and `rimz loop stop` leaves it running. A [workflow](./workflows.md) is started the same way, with `rimz workflow run` as the command; it returns once the run starts, and the run goes on as its own.

## Who keeps time

There is no RimZ scheduler daemon. The clock is the sidebar, the background process that draws the room's column: while a room for the task's project is open, attached or not, it fires due tasks on its regular tick, each through a detached `rimz loop run <name>`. With no room open, clock tasks wait. To keep time without an open room, opt in once:

```sh
rimz loop timer install
```

That installs one systemd user timer on Linux or launchd agent on macOS. Once a minute it runs a one-off RimZ tick, re-reads every task, and fires only projects without an open room; an open room still wins for its own tasks. On Linux each fire launches through `systemd-run --user --scope`, in its own process group, so the work outlives the tick that started it. A tick that cannot reach your systemd user session refuses rather than firing, and tells you how to fix it; nothing is lost, and the due tasks fire on the next tick after that.

Know what a timer fire does before you install it. A fire opens a room for the task's project before its command runs, so a command that launches an agent finds one. It leaves the room open, so a 02:00 task means a multiplexer session running on the machine by morning, and that room takes over later occurrences. `rimz loop timer status` shows whether the timer is installed; `rimz loop timer remove` stops future ticks but does not close rooms already opened.

Only clocks need a timekeeper. A signal task and a `rimz wait` watcher are fired by the process that emits the signal or runs the command, whether or not a room is open.

Missed time is never made up in bulk. A task a clock has not seen before is armed rather than fired, so adding one sets nothing off; a task that was already armed and came due while nothing kept time fires once when a clock next sees it, then resumes its schedule. A week of missed 02:00 occurrences does not arrive as a week of fires. A one-shot is the exception: its time is fixed when you add it, so it fires on the first clock that sees it at or past that time, however late, and its run says how late (`fired 2h 5m late`).

## Wake a running agent

An `until nc -z localhost 3000; do sleep 1; done` loop waits for a service, and `tail -F build.log | grep -m1 READY` waits for a log line. Both observe the right thing, but run in the agent's shell tool and hold its turn open while nothing needs its attention. A deploy is baking, the agent has nothing to do until it is ready, and the follow-up otherwise falls to you.

A wait inverts that. The agent arms its own alarm, ends its turn, and receives the result in the same conversation.

```sh
rimz wait --in 30m
rimz wait --at 14:30
rimz wait --after-reset 5h
rimz wait --when pr=merged
rimz wait --pid 16776
rimz wait --check 'nc -z localhost 3000'
rimz wait --file build.log --grep 'READY'
rimz wait --run 'gh run watch --exit-status'
rimz wait --on fail --run 'cargo test'
```

A timer can be any length (`rimz wait --in 2d`). `--at` fires at a clock time, `--after-reset` when your provider's usage window next resets, and `--when` once a state expression such as `pr=merged` or `ci=passed` reads true; the scheduler fires these as it fires a timer, and `pr` and `ci` need a running room, whose sidebar reads them. The window reading is the room's current account for your provider, which can differ from the account you run on. A process, command, check, or file runs in a detached watcher with stdin closed, at the root of the checkout the wait was armed from, linked worktrees included; a polled `--check` or a `--file` watch samples once a second. When the trigger fires, the wake message names the command, its exit status, and the elapsed time, then points at a file holding the complete output rather than inlining it: `~/.rimz/ws/<workspace-dir>/out/<you>/<name>.output`, the same host path in a [scoped pane](./security.md#scoped-isolation) and outside one. `rimz gc` removes it 7 days after its last write once no watcher runs, and `rimz loop logs <name>` keeps the last 4 KiB in durable history.

A long command can check in rather than disappearing. Set `--check-in` to receive one still-running notice with the output file's line count and estimated tokens so far; the command keeps running and its exit verdict follows later. Without an explicit check-in, agents with cache keepalive enabled and a resolved lifetime above the ping margin and at least the keep-warm floor receive keepalive pings instead. Other agents still check in after 30 minutes. `rimz wait list` shows your own waits, with team subscriptions on an `also woken by` line; from a user shell it shows the whole room. `rimz wait cancel <name>` drops any one row aimed at you, while `rimz wait cancel --all` drops only waits you armed with `rimz wait` and stops their watched commands, leaving subscriptions intact. The [wait reference](../reference/cli/wait.md) owns every flag and the exact message each trigger delivers.

Long waits can also outlive the provider's prompt cache, making the eventual wake pay for a fresh cache write. While the agent rests with a wait armed, including a parent displayed as working on its subagents or a team it launched, RimZ sends a facts-only keepalive shortly before the agent's resolved cache expiry, when its lifetime clears the ping margin and floor: “Cache keepalive, no action needed. Waiting on:” followed by a line per wait. Each completed ping turn starts another cache window, until the next ping would start at or beyond `harness.cache_keepalive_max` (default 6h) after the agent's last request of its own: that last ping ends with `Keepalive limit 6h reached: this is the last ping until your next turn.`, and the agent's next turn of any other kind restarts the clock. On Claude's 1h cache the default allows six pings; Codex's API-key or unknown-sign-in 30m cache allows twelve. The first sleeping ping is uncapped, so Codex's subscription 12h default gets one final ping at about 11h59m, even with the default 6h maximum. An idle agent with nothing pending gets the same ping, with the bare first sentence, while a [keep-warm horizon](./configuration.md#keep-warm) set globally or on its profile, team, or role is still running and its optional stage condition permits it, under the same maximum; that ping leaves the agent's status and quiet time alone. A ping held back because someone was typing in the pane is sent once more 30 seconds before the cache expires, past the typing hold and ahead of any message the same typing holds in the agent's queue, so a draft left open does not cost the cache; no ping is ever written onto a cache that has already expired: a ping still queued at the cache's expiry, or after the agent takes another prompt, is withdrawn untyped. No ping goes to an agent whose account's usage window is spent ([idle compaction](./configuration.md#idle-compaction) lists which windows count). The maximum also bounds how far off a [subagent's limit reset](./subagents.md#deadlines-stopping-and-cleanup) may be for its run to be held to it. This is best-effort and needs a running sidebar. The [configuration guide](./configuration.md) covers lifetime observations, settings, defaults and the fallback and the switch to disable it.

Only an agent RimZ can identify may arm or cancel its own waits, and a self wait steers that agent the moment it fires unless the agent has a native prompt open. In that case, the wake parks until the next turn boundary after the prompt is answered. A check-in parks the same way and is canceled when the wait delivers its final outcome. A team wakes its own members on a signal through [signal bindings](./teams.md#define-your-own-team): forge sends failed CI directly to its coder, whoever pushed, and the delivery lands at the coder's next turn boundary. To hand a teammate a reminder on a clock, make the command a [message](./messaging.md):

```sh
rimz loop add standup --every weekday --at 09:00 -- rimz message @planner "Post today's plan to #dev"
```

Every wait and binding pins one exact session and never outlives it: a stop, a lost pane, a closed agent, or a reaped one retires that session's deliveries, and a chain of self waits ends when the agent no longer arms another.

`rimz agents restart` is the case worth knowing. A restart that launches fresh leaves the old session behind and retires its rows whole, while a restart that resumes the same conversation keeps that session, and a team's declared bindings with it. Either way a wait the agent armed for itself is gone, and the restart line says how many it dropped.

## Signals: the room's event bus

A wait on a timer is a guess about when something will happen. A signal is the thing itself. Most of them need no wiring: RimZ emits from what the room already watches. The sidebar polls the forge for every [worktree](./worktrees.md) branch's pull request and CI verdict (the `#91` badge and the check glyph in each group header), and the reporting hooks already know when an agent or a team changes state. Each of those transitions is a signal a task can fire on by name.

| Signal | Fires when | Payload carries |
| --- | --- | --- |
| `ci.passed`, `ci.failed` | the checks on a commit on a watched checkout settle on success or failure, with or without a pull request | `path`, `branch`, `repo`, plus `head` (the commit those checks ran on) and `checks_url` when the commit is known, and `number` and `url` when a pull request exists |
| `pr.merged`, `pr.closed` | the pull request on one of the room's watched checkouts leaves the open state | `path`, `branch`, `repo`, `state`, plus `number`, `url`, `head`, and `checks_url` when known |
| `pr.opened` | a successful probe finds a new open PR after monitoring has already observed the branch successfully | the PR payload above with `state: "open"` |
| `trunk.moved` | the same trunk ref moves to a different commit | `trunk`, `from`, `to`, `repo` |
| `worktree.created`, `worktree.removed` | a managed worktree is successfully created or removed, not reused or rolled back after a failed creation hook | `name`, `branch`, `path`, `repo`, plus `from_pr` when present; creation adds `base` when known, removal adds `branch_deleted` |
| `pr.behind`, `pr.conflicted` (GitHub only) | an open PR first falls behind its base or has confirmed conflicts, including when first seen; fires again on a changed PR head while still in that state, not on repeated readings of the same head | the PR payload above with `state: "open"`, plus `pr_head`, `base` when known, and `behind_by` for `pr.behind`; `head` remains the local worktree HEAD |
| `pr.queued`, `pr.dequeued` (GitHub only) | an open PR enters the merge queue, or leaves it while still open (failed queue checks, a manual removal), including the first reading on a worktree the room was already watching; a room that opens on a PR already queued or dequeued gets no signal; fires again for a later entry or removal; a removal already re-armed at first reading is silent, but withdrawing a re-arm announces the original removal again; a merge out of the queue fires `pr.merged` alone | the PR payload above with `state: "open"`, plus `pr_head` and `base` when known; `pr.queued` adds `queued_at`, and `pr.dequeued` adds `dequeued_at`, GitHub's `reason` when supplied (`failed_checks`, `manual`), and `queue_checks_url` when GitHub names the queue commit |
| `agent.started`, `agent.idle`, `agent.waiting`, `agent.failed`, `agent.ended` | one agent's own lifecycle transitions; `agent.waiting` fires at the hook even during Codex automatic review, before its 60-second attention delay | `kind`, `session`, `status`, `errored`, plus `handle` when the card has a name |
| `team.idle`, `team.waiting`, `team.failed`, `team.ended` | a [team](./teams.md) cohort settles: every member at rest with nothing queued and no wait armed, one member waiting on input, one member's turn failing, or the last member ending; `team.waiting` also fires at the hook during Codex automatic review | `team`, `instance` (`forge#feat-x`), `member`, and `members` with each handle and status; a handle can be a session id |
| `team.stage` | a team [hands a stage off](./teams.md#hand-off-with-one-command), and again when a resumed owner takes its stage back | `team`, `instance`, `to`, `owner`, `by`, `note`, and `board`, plus `from` on every flip that did not open the board |

Those six families (`ci`, `pr`, `trunk`, `worktree`, `agent`, and `team`), plus `wait` for a watched command's own completion, belong to RimZ. `rimz events emit` refuses every name inside them, so your own signals can never collide with RimZ's.

Use `pr.behind` to react when the base moves ahead of your PR, and `pr.conflicted` when a push needs conflict resolution. GitHub may take time to calculate conflicts after a push; RimZ waits for a settled answer before treating that push as a new conflict. Both signals accept `--match branch=feature` or `--match path=/your/worktree`, just like `pr.merged`.

Use `pr.dequeued` to learn that the merge queue rejected your PR. The queue tests your change on a commit of its own, so a failure there never turns the PR's own checks red and `ci.failed` stays silent; `queue_checks_url`, when supplied, opens that queue commit's checks. A dequeued PR stays dequeued until it is queued again, re-armed (an auto-merge request enabled after the removal), merged, or closed, so an agent that was away can still read it with [`--when pr.queue=dequeued`](../reference/cli/loop.md#conditions). A push alone does not clear it. While re-armed and waiting to enter the queue, it reads `pr.queue=armed`. A newly opened room receives no signal for a removal that predates its first poll.

Subscribe to one name or to a whole family: `--signal ci.failed` for the red verdict, `--signal 'ci.*'` for any verdict. Either subscription observes every name in the family, and what it does with the ones it did not ask for is the useful part. A subscription to `ci.failed` sees the green build too, records it as a skip without running its command, and stays quiet; a branch that goes green never starts the fix that only red asked for. Narrow a subscription with `--match key=value` on a payload field, such as `--match branch=feature`; a signal that fails a match is ignored outright.

```sh
rimz loop add ci-fix --signal ci.failed --match branch=feature --repeat -- rimz agents codex -w ci-fix "Fix the failing checks."
```

Without `--repeat` the task fires once and is spent; with it, it keeps listening until you remove it, and a subscription has no deadline. The fire hands the command the signal's one-line subject, such as `ci.failed on check-multi #729 @88824c2`, through `RIMZ_LOOP_TRIGGER` and at the end of a launched agent's prompt; the [loop reference](../reference/cli/loop.md#the-triggers-evidence) lists the subject each signal prints and how to read the retained payload.

### Your own signals

Anything that can run a command can put an event into the room:

```sh
rimz events emit deploy.finished --json '{"env":"prod","version":"1.4.2"}'
```

```console
emitted deploy.finished (evt_01a07076a71c72e09f0461533909ceb1) · fired 1 task
  smoke
```

A CI job's last step, a git hook, a deploy script, a cron line, another agent: each one emits, and every task subscribed to that family fires. Names are lowercase dot-separated words outside the seven reserved families, and the payload is one JSON object whose top-level fields `--match` filters on and the fired command reads, one `key: value` each on its trigger line.

Your own families observe and skip like the built-in ones, so it pays to name them by outcome. A task added with `rimz loop add deploy-red --signal deploy.failed -- ./scripts/rollback.sh` fires on `deploy.failed`, stays armed and quiet through `deploy.finished`, and ignores anything outside the `deploy` family.

The emitting process fires the subscribers itself, so this works with no daemon or running agent, but needs an existing room. A signal reaches only the tasks subscribed at that instant, which is why you add the task before starting the work that will emit it. A busy repeating task can keep its own trigger with an overlap policy; later subscribers still miss the signal.

If a task's run is still live when the next signal arrives, the default is to skip that trigger rather than run two copies; the skip names the live run. Use `--overlap coalesce` when only the newest state matters, or `--overlap queue --overlap-limit 10` when each trigger needs its own run. RimZ keeps those triggers on disk and starts them one at a time once the live run has ended, rerunning the task's gates. `rimz loop show <name>` shows what waits and how many fires waited on each run. Pause or disable prevents waiting work from running; removing or redefining the task clears it. These policies apply to every repeating clock, signal, or condition task, not to one-shots. The [reference](../reference/cli/loop.md#overlapping-triggers) gives the merge rule and all policy values.

If a newer trigger makes the current work obsolete, use `--overlap replace`. RimZ keeps the newest trigger, cancels the live run as `rimz workflow cancel` does, and starts the newest trigger only once that run has ended, so two runs never overlap. A canceled run records `canceled`, which never counts as a strike, so triggers that keep arriving cannot disable the task. A run that finishes on its own before the cancel reaches it keeps its own result. `rimz loop stop` also keeps waiting triggers and reports when the next starts; use `rimz loop pause <name>` to drop them instead.

A fire owns only what its command waits for. A `--detach` launch, or a workflow the command started, outlives the fire, so the next trigger does not see it as an overlap: a repeating task that starts a workflow already running on its checkout fails that fire.

## Wait for Done and green

A CI event can arrive before your team finishes review. Subscribing only to that event misses the later moment when both are ready. A condition reads their current state in either order and can require it to stay true before opening a PR.

A task reads conditions at the project root, so name the team's branch, here `feat-x`, with its room open:

```sh
rimz loop add open-pr --when 'team.stage[feat-x]=Done && ci[feat-x]=passed' --for 30m -- rimz agents claude "Open the pull request for the feat-x branch."
rimz loop show open-pr
```

`team.stage[feat-x]` reads the board of the local checkout on `feat-x`, and `ci[feat-x]` that branch's CI. The sidebar checks both each tick: `waiting` becomes `holding`, and either reading turning false restarts the 30 minutes. Once ready, RimZ runs the command, whose agent opens a pane with the prompt, and removes the one-shot task; `rimz loop list` and `rimz loop watch` keep showing it as `▸ running` until that run ends. The room's sidebar keeps probing the branch while the task is live, even with no checkout for it; removing the task releases that probe. Without a room sidebar, or until the sidebar probes the forge again after a fire, CI is unread and the condition is unknown: the task holds, and a running 30 minutes keeps counting rather than restarting ([forge freshness](../reference/cli/loop.md#forge-freshness)). A probe that finds no CI reads `none`, which this condition treats as false. Cancel before it fires with `rimz loop remove open-pr`; [conditions](../reference/cli/loop.md#conditions) lists the expression rules and unknown readings. Add `--repeat` for a task that should fire again each time the condition turns true after being false.

When your next step depends on several branches landing, name them directly instead of keeping their checkouts open:

```sh
rimz loop add follow-up --when 'pr[sidebar-ordering]=merged && pr[sidebar-cards]=merged' -- rimz agents claude --detach "Continue now that both prerequisites have merged."
```

Both branches belong to the origin repository of the checkout where you add the task. The room sidebar keeps those branch readings live without local checkouts; removing `follow-up` releases them.

Before adding a task, use `rimz loop explain 'ci=passed && pr=open'` to inspect its current readings and truth without creating it. A script can add `--json` to read the same facts and their probe timestamp without parsing the display; [loop explain](../reference/cli/loop.md#loop-explain) lists the fields and exit codes. For selected branches, add the task first so the sidebar starts probing them.

### One task for every worktree

When a team finishes a lane, you may still want someone on its PR after merge: a cleanup agent that reviews what landed. Opening that pane by hand for every lane repeats the same handoff. `--each-worktree` makes it one standing rule: the condition is read in each RimZ-owned worktree, and the command runs in the one that meets it.

```sh
rimz loop add yagni --when 'pr=merged' --each-worktree --repeat -- rimz agents yagni --detach "Review the merged work for unnecessary complexity and propose focused cleanup."
```

Keep a room running for CI and PR conditions to be observed. Lanes already meeting a rule qualify too. GitHub sees PRs already merged or closed; Gitea needs the room to have observed the checkout's PR while it was open, and room teardown or reset forgets that observation. The project root and worktrees RimZ does not own are not included. Set `once-per-checkout = true` on the task in `loop.toml` when a checkout should run the command at most once successfully ([launch memory](../reference/cli/loop.md#launch-memory)). From an owned worktree, `rimz loop fire yagni` fires there again.

## Wait for room in a usage window

Some work only pays off once the provider's usage window has room: a long refactor that starts in a nearly spent 5-hour window stalls halfway, and what you want is to start it when the window has refilled.

`window.5h.left` and `window.7d.left` are condition keys for that. Each reads the percent left in the `--provider` kind's 5-hour or 7-day window, from the provider usage cache the room's sidebar keeps, and compares it with a whole percent:

```sh
rimz loop add big-refactor --when 'window.5h.left>=60' --provider claude -- rimz agents claude "Refactor the storage layer and leave the branch green."
```

Each tick projects the cached reading to now, so a window whose reset has passed reads `100` before a new reading lands; that is what lets the loop timer fire the task at the reset with no room open. Without a room nothing refreshes the cache, so the reading is only that projection: it rises at the reset but misses usage since the last refresh. `--for 30m` requires the window to stay at that level for half an hour, so it measures a window that stays quiet and full: any use that drops it below the bound restarts the hold.

`--provider` names the kind whose windows the condition reads, and `--account` one of its [named accounts](./accounts.md); neither picks the login the command's own launches run on. Adding reads the current window first and refuses what it cannot read: Qwen, whose account is chosen per launch, and a missing or already-reset reading, which `rimz accounts --refresh` or an open sidebar replaces. A window that has not started still reports about 1% used, so it reads `99` and `>=100` rarely holds; ask for `>=95` instead. Window conditions are not available to `--project` tasks. The expression rules are under [conditions](../reference/cli/loop.md#conditions).

When the moment you want is the reset itself, `--after-reset 5h` (or `7d`) fires the task once when that window next resets, then removes it. An agent that only wants to wake itself, with no prompt, can arm `rimz wait --after-reset 5h` instead ([wake a running agent](#wake-a-running-agent)):

```sh
rimz loop add after-limit --after-reset 5h --provider claude -- rimz agents claude "The 5h window has reset; pick the refactor back up."
```

Add records the reset instant on the task, and the receipt shows the window's current percent left and when it resets. A window that has not started has no reset to wait for yet, so the task fires on the next tick. If the reset passes while no clock is running, or while the task is paused, it fires as soon as one sees it. It gets one attempt: whether the fire runs, is skipped by a gate, or fails, the task is gone afterwards. A fire that never got as far as recording anything (the runner could not start, or was killed) leaves the task in place, and RimZ dispatches it again five minutes later. Add refuses what a window condition refuses, plus a window the provider is not enforcing. The rules are under [after a reset](../reference/cli/loop.md#after-a-reset).

## Strikes

Repeated failures disable the task. Three consecutive failed fires auto-disable it, show `disabled after 3 strikes` under NEEDS YOU in `rimz loop list`, and fire [notification handlers](./notifications.md) with kind `loop_disabled`. A strike is a fire whose command exited non-zero or outlived its timeout; a fire that ended `completed` resets the count, so a watchdog whose suite passed again is back to zero. A canceled run, a fire that a gate turned away, and a fire that left no run behind at all are neutral, so a machine that cannot start runners never disables your tasks. Each run counts once, however often its outcome reaches the loop. `rimz loop enable <name>` clears the strikes and re-arms the schedule, `--max-strikes <N>` changes the threshold per task and `--max-strikes 0` removes it, and `rimz loop fire` still runs a disabled task for a manual test. Which run results strike, reset, and count for nothing is a table in the [loop reference](../reference/cli/loop.md#strikes).

What a launched agent spends is bounded where it launches: `--budget` on the `rimz agents` command, and the room's and accounts' caps in the [budgets guide](./budget.md).

## What a task does on your machine

`rimz loop add` writes a task definition and starts no process.

A repeating task writes a `[tasks.<name>]` entry to `~/.rimz/loop.toml`: per-machine automation, like your crontab, never inherited by a cloned repository. The file is plain TOML you can edit by hand, and the `deps` task above lands as:

```toml
[tasks.deps]
command = ["rimz", "agents", "claude", "--detach", "Check for outdated dependencies."]
root = "/home/you/code/app"
fresh-worktree = "deps"
timeout = "2h"
at = "09:00"
every = "mon"
```

A task that fires once (`--at`, `--in`, `--after-reset`, or a `--signal` or `--when` task without `--repeat`) lives in the room instead, in `~/.rimz/ws/<workspace-dir>/records/loop-instances.json`, beside the waits agents arm. Start a room with `rimz start` before adding one ([where tasks are stored](../reference/cli/loop.md#where-tasks-are-stored)); a repeating machine task needs no room at add time. A one-shot retires after its fire, and while its run is active it stays in `rimz loop list` and `rimz loop watch` as a `▸ running` row, so `rimz loop stop <name>` has a name to take. Agents' waits and team subscriptions last until they retire or their agent ends; a restart or molt keeps them. `rimz loop list` folds agents' waits under WAKE-UPS; `--all` unfolds them in agent cards, followed by team subscriptions.

A task holds only the keys `rimz loop add` writes. A row with any other key, hand-written or left by an older RimZ, does not fire: `rimz loop list` shows it under NEEDS YOU as `invalid` with the reason, and the fix is to delete it and add it again with `rimz loop add`. The other tasks in the file keep firing.

Room-local tasks can reuse a machine task's name when that machine task belongs to another project. Adding the room-local task leaves the other project's definition intact; [name precedence](../reference/cli/loop.md#where-tasks-are-stored) keeps commands in the room pointed at its local task. A repeating machine task cannot take a name another project's machine task holds: choose another name, or remove the existing task from its owning project first.

`--project` writes the entry to `<root>/.rimz/config.toml`: shared automation that travels with the repo, so it has to repeat (`--every`, `--cron`, or `--signal` with `--repeat`) and always runs its command at the project root. A committed task runs commands on whoever pulls it, so its command enters the [project trust hash](./security.md) and stays inert until each user approves it. Trust and enablement answer different questions: trust says the project config contains commands you accept as yours to run, and `rimz loop enable <name>` says this particular task may run unattended on this machine. A project task pulled from a repo starts disabled even after trust is granted; a task you create with `rimz loop add --project` starts enabled on your machine. A trusted project task wins over a same-named machine task without double-firing.

### Where a fire runs

A fire opens the room for the task's project, then runs its command at the project root, in the checkout an `--each-worktree` fire selected, or in a fresh worktree. Every agent its command launches without `--detach` opens in an unfocused tab beside the room's loop dashboard, whether a clock fired it or you did with `rimz loop fire`, so it never splits the tab you are working in. When the agent needs you, its tab shows the waiting glyph and its sidebar can ring the bell.

A `--fresh-worktree` task gets a new tree for every fire, the same `git worktree add` from trunk that `rimz worktree new` runs, named `<prefix>-<yyyymmdd-hhmmss>` in UTC. A fire cuts its tree only when it will start a run. When the run ends, the usual [worktree cleanup](./worktrees.md) removes the tree if it holds no changes and no commits that have not landed, and no agent pane is open in it; a tree with work, or with a `--detach` agent still working there, stays for you to merge or remove. `rimz loop show <name>` lists those under `KEPT WORKTREES`, and `rimz loop logs <name>` names each fire's tree on its `worktree:` line. While a fire runs, `rimz worktree sweep` and `rimz gc` leave its tree alone.

### What a fire leaves behind

A fire leaves one workflow run that runs the command. The run outlives the `rimz loop fire` that started it: `fire` prints `launched` with the run's `rimz workflow show` command and returns. A fire whose command failed, timed out, or was canceled, and any fire of a one-shot task, leaves one line of run history in `~/.rimz/logs/loop-runs.log.jsonl` with its outcome (`completed`, `failed`, or `canceled`) once the command ends, and its record stays readable in `rimz workflow show <run>`. A repeating task's fire whose command exits `0` is a [quiet fire](../reference/cli/loop.md#quiet-fires): it leaves no history line and no record, and the task keeps one quiet summary instead, counting its quiet fires and holding the last one's output, so a check that passes every five minutes does not bury the fires that failed. `rimz loop show <name>` gives that history a health verdict; `rimz loop logs <name>` prints the complete stored forensics, including the command's output, and `rimz workflow show <run>` opens one run ([starting runs from a loop](./workflows.md#start-runs-from-a-loop)).

The `loop show` screen answers first and shows its evidence after. The verdict line reads the latest conclusive run: how long ago it ran, how it ended, and, once there is a streak, how many runs in a row ended that way and when the streak began. While the task has a run in progress, it says `last finished run`, and after a failure it drops `failing`, since `rimz loop list` keeps such a task out of NEEDS YOU. `LAST RUN` is that run's evidence. An empty `LAST RUN` is omitted unless it names a manual fire or signal. `RECENT RUNS` appears newest first unless it would only repeat a single run from the verdict with no folded fire counts. Repeated runs share one row with a duration range and, when they cover at least a second, a time span, and keep the table even when that is its only row. The task's facts close the screen, where `strikes` counts consecutive failures against the threshold. `manual` marks a run you fired by hand; a clock's fire carries no mark.

A subscription can hear another signal in its family without acting on it. Those signals do not replace `LAST RUN` or inflate the run counts: a muted `heard` line counts them since the last acting run and names the latest. `rimz loop logs <name> --heard` includes that listening history when you need it. A fire refused because a run held the lock folds as a count into that holder's recorded run: how many fires waited on it or were skipped. If a later lock tenure proves the holder gone without a record, a separate `○ no record` row keeps its pid and fire counts; those fires do not count against the later run or its verdict. Without that evidence, trailing fires still take no recent row, and waiting work stays in the header. Other gate refusals still appear in recent runs. `rimz loop logs <name>` keeps each run's full evidence in its own block, with the same attributed fire counts. Repeated refusals fold into blocks with their newest evidence; no-record groups get blocks too, and trailing fires stay as refusal blocks in place. `-n` counts blocks after filtering. Heard signals keep one block each under `--heard` and change no fire count. With no fires, a wake-up names the agent it will wake and says `not fired yet`, without a manual-fire hint. Only other clock-triggered tasks suggest a manual fire; conditions, signals, and watches do not.

To test a command before its condition becomes true, use `rimz loop fire <name>`. It warns that the condition is not met, including the clause count when available, then runs anyway without disarming the task. A condition that is already met adds no warning.

Teardown is one command. `rimz loop remove <name>` deletes the entry from whichever file owns it; the run history stays readable through `rimz loop logs <name>`. `rimz loop timer remove` takes the timer out, and `rimz uninstall` removes it too.

## Every schedule shape

Each task names one trigger and one command after `--`. A clock recurs only with `--every` or `--cron`; a signal or condition task fires once unless `--repeat` keeps it.

| Shape | Flags | Repeats? | Example |
| --- | --- | --- | --- |
| One-shot | a bare `--at` (`HH:MM`, `YYYY-MM-DD HH:MM`, or `<weekday> HH:MM`), or `--in <delay>` | fires once, then the task removes itself | `--in 30m` |
| Interval | `--every <duration>`, measured from the last fire | yes | `--every 15m` |
| Calendar | `--every <days> --at HH:MM`, where days is `day`, `weekday`, `weekend`, a range `mon-fri`, or a list `mon,wed,fri` | on each matching day | `--every weekday --at 07:00` |
| Raw cron | `--cron`, a five-field expression with names such as `mon-fri` and steps, or a macro such as `@daily` | per the expression | `--cron "*/15 * * * *"` |
| Signal | `--signal <name-or-family>`, narrowed with `--match key=value` ([above](#signals-the-rooms-event-bus)) | once, or on every delivering emit with `--repeat` | `--signal ci.failed`, `--signal 'ci.*'` |
| Condition | `--when <expr>`, held with `--for <duration>` ([above](#wait-for-done-and-green)) | once, or each time it turns true again with `--repeat` | `--when 'pr[feat-x]=merged'` |
| After a reset | `--after-reset 5h` or `7d` with `--provider` ([above](#wait-for-room-in-a-usage-window)) | fires once, then the task removes itself | `--after-reset 5h --provider claude` |

One pair is worth a second look. `--every 1d` is an interval: it fires a day after the last fire and drifts with it. `--every day --at 07:00` is the calendar's 07:00 sharp. Calendar times, cron, and `--in` resolve in the top-level `timezone`, falling back to the system zone when unset.

Inspect, test, and manage tasks with the rest of the surface:

You can inspect the worktree name printed by the list with `rimz loop show <worktree>`. If several tasks belong to it, RimZ lists their real names and inspection commands instead of choosing. `rimz loop show <team>` (or `<team>#<channel>`) lists the real subscription names hidden by the folded WAKE-UPS line. Exact task names work from outside the room too; the result names the `--root` other commands need. Use the real task name for changes such as `remove`, `pause`, and `fire`.

```sh
rimz loop list                 # this room's tasks, grouped by lifecycle
rimz loop list --all           # every known room, with older one-offs and wake-ups unfolded
rimz loop list --json          # real task names and structured, uncollapsed rows
rimz loop watch                # this room's live dashboard, including every worktree
rimz loop show pr-watch        # health, next fire, and recent runs
rimz loop logs pr-watch        # full forensics for recent runs
rimz loop fire pr-watch        # start a run now for testing; the schedule stays put
rimz loop enable pr-watch      # arm locally and clear any pause or strike disable
rimz loop disable pr-watch     # hold until the next explicit enable
rimz loop pause pr-watch --for 2h
rimz loop stop pr-watch        # cancel the task's live runs
rimz loop rename pr-watch ci-watch
rimz loop remove pr-watch
```

`rimz loop fire` runs the scheduled gates except launch memory (overlap still applies), starts the run, and returns with the `rimz workflow show` command that follows it; the command then runs inside that run. When a fire reports `previous run still active`, `rimz loop show <name>` and `rimz loop list` show the task as `▸ running` with how long it has run, and `rimz loop stop <name>` cancels each live run the task started, as `rimz workflow cancel` does, and stops each agent the command launched without `--detach`.

The list starts with NEEDS YOU: failing tasks (a retry in progress, by hand or on schedule, takes a task out until the retry's own result lands; while the retry executes, `rimz loop list` and `rimz loop watch` read the task `▸ running` from the retry's start), holds needing a fix, trust blocks, invalid tasks and triggers, dead watchers, checkouts whose directories are gone before their task fired, and one-offs waiting on a pull request that was closed without merging. Each reason points to the command that addresses it. ROUTINE holds recurring work. WAITING holds one-offs that have not fired yet, with a clause count for AND conditions. DONE shows the five newest one-off launches and outcomes, including a fired one-shot while it is still running. WAKE-UPS counts self waits and subscriptions separately, then shows their health. Use `--all` to unfold older DONE entries and wake-ups, and inspect other rooms. Self waits appear in agent cards, with children under their ancestors; subscriptions follow by team. Tables keep names first and add WORKTREE only when needed; your checkout is marked `(here)` and your wake targets `(you)`. A missing checkout is only flagged, not removed, so restoring its directory restores its row. Other rooms' attention tasks appear on an `elsewhere:` line. JSON always keeps real task names and every retired one-off's latest outcome.

LAST answers whether a task acted, not whether it merely heard an unrelated signal. A good run stays `✓` after sibling signals; a subscription that has only heard siblings says `never fired · heard ci.passed 7m ago`. Expanded self-wait cards show the wait's subject (a watch's label in place of its command, any other trigger followed by its label), elapsed watching time, and time left to check-in. Lost waits lead their cards and also appear in NEEDS YOU because they can no longer wake their targets; a newly armed watch has 30 seconds to start before it is called lost. Spend appears beside lifecycle-table results and in the room heading, rather than in a separate mostly-empty column.

Enablement and pauses belong to one machine and never edit the task definition. `disable` holds a task indefinitely, `pause --for <duration>` is a bounded hold that lifts itself, and `enable` clears either hold and any strike counter. `--all` applies enable or disable to all machine tasks plus this project's instance and project tasks, independently of the list's room scope. A lifted hold starts the schedule fresh from that moment, so fires missed while held never replay.

Every flag is in the [loop CLI reference](../reference/cli/loop.md); the run every fire starts is a [workflow run](./workflows.md).


## Keep the fleet moving

An unattended agent stops for reasons that need no judgment from you. The provider's usage window empties mid-turn. The API sheds load and drops the stream. The context window fills one step short of the finish. A spent Codex account sits on reset credits that expire unused. Each stop has a known fix: wait for the reset, retry in a few minutes, compact, redeem a credit. A stock CLI leaves every one of them to whoever is watching, and overnight that is nobody.

RimZ ships each of those fixes as a configurable reflex. The room recognizes the stop from the provider's own evidence, applies the fix at the moment it can work, and the agent carries on in the same session, as if you had typed the resume yourself. Idle compaction and smart compaction are on by default; auto-continue and auto-redeem are opt-in, with one exception stated under [auto-redeem](#auto-redeem): a Codex credit about to expire is redeemed whether or not you opted in. Because these reflexes let RimZ type into your panes and spend on your account, each section below states the rules it follows.

```sh
rimz config set resume.auto_continue true     # resume rate-limit and API-error parks
rimz config set resume.auto_redeem true       # spend Codex reset credits when they buy real time
rimz config set harness.idle_compact off      # opt out of idle compaction
rimz config set harness.smart_compact 200k    # wrap up a filling turn; compact before the next prompt
```

### Auto-continue

A turn that dies on a rate limit or an API failure parks its agent: the card shows `⏸`, and the work stops until someone types `continue`. At your desk that is one keystroke. Away from it, the fleet sits idle from 1 a.m. until you notice. Auto-continue types that keystroke for you, and its policy is three decisions: what counts as evidence, which clock schedules the resume, and when to stop trying.

The evidence is the structured per-turn failure record the agent's own CLI writes when a turn dies, naming the cause. That record is the one provider input the decision reads, so every automatic resume traces back to the provider's own account of why the agent stopped, and every other kind of stop stays parked for your judgment. Because the record comes from each agent's adapter, auto-continue is a per-agent capability, and the [per-agent notes](../reference/agent-support.md#notes-on-the-alpha-and-experimental-set) say where it never arms. The one park that needs no provider record is the one RimZ imposed itself, a [dollar-cap park](./budget.md#what-resumes-a-parked-agent), because RimZ already knows why it stopped.

The cause picks the clock:

- A rate-limit or spend-limit park waits for the latest applicable account or model window to reset, including a Fable model limit. If the provider still reports a limit afterward, nudges stay at least two minutes apart; those limit replies do not restart the attempt cap.
- A daily dollar-cap park reopens at local midnight; an account's monthly park at 00:00 UTC on the next 1st. Continue waits for the last binding cap's reset.
- A transient overload or API-error park has no reset clock, so it retries on a lengthening ramp: the first attempt three minutes after the failure, then every five (`resume.auto_continue_backoff_secs`, default `[180, 300]`, last value repeating). Overload replies do not restart the attempt cap either.

Each attempt is a keystroke you could have typed: when the clock fires and the agent is still parked, RimZ sends the configured nudge (`continue` by default, `resume.auto_continue_text`) into the agent's pane through the same path as `rimz message --steer`. All causes share one attempt cap (`resume.auto_continue_max_retries`, default 12, just under an hour on the default ramp); when it runs out, the card goes `failed` and routes to you like any other actionable card. Every attempt appends the park time, delivery verdict, and message id to the assist log that `rimz stats --assists` prints. A nudge still waiting when you prompt the agent yourself, or when its park closes, is withdrawn without being typed (`canceled` in `rimz message show`) and does not count toward the cap.

The keys are in [configuration → Resume](./configuration.md#resume).

### Auto-redeem

A Codex plan grants reset credits: redeem one and a spent usage window refills on the spot, and its natural reset starts over a full window from then. Managed by hand they waste value at both ends. A credit you forget expires unspent; a credit you hold while a spent window parks the fleet is a night of work standing still. And the timing is a real judgment call: redeemed just before the window's natural reset, a credit buys minutes; redeemed the moment a spent window blocks a night of work, it buys hours. Auto-redeem makes that call by three rules, and each redemption's record (`rimz stats --assists`) names the rule that fired:

- Expiry rescue. A credit within thirty minutes of expiring is spent rather than lost. This rule always runs, even with `auto_redeem` off: the capacity is already paid for. It is also the one rule that reaches past this room: while a room is open, every Codex account you declared is watched, including one no agent here runs on. On such an account RimZ reads usage at most once every three hours and skips the rescue when the window shows no usage at all, since a refill there would buy nothing.
- Blocked gain. A window is spent and its natural reset is at least `resume.auto_redeem_min_gain` away (twelve hours by default). A credit now recovers those hours, so it redeems immediately; a nearer reset means waiting is cheaper than spending.
- Scheduled redeem. Several credits approach expiry together and a spent window resets sooner than `resume.auto_redeem_min_gain`. RimZ measures from your recent usage how long a fresh window takes to fill and spaces the redemptions that far apart, working back from each credit's expiry, so each one lands on a window with room to absorb it.

Blocked gain and scheduled redeem fire when an in-use Codex account has spent its subscription window and a reset is the next source of capacity. They do not wait for an agent to park. By default, `resume.spend_order = "credits_first"` uses available paid credit before banked resets; choose `resets_first` to spend a useful reset before paid credit. Neither rule fires on an account this room is not using.

The reflex fails closed and paces itself. Every rule starts from a credit in hand; blocked gain and scheduled redeem also require a spent window with a readable reset time. Each login's attempts are throttled across every room on the machine: ten minutes between attempts, thirty after a success. Logins sharing a provider account serialize their refresh and redemption, so a second helper sees the first refill before deciding. A successful redemption refreshes the account reading at once, and because the refilled window is exactly the reset [auto-continue](#auto-continue) waits for, it wakes the turns parked on it.

With `auto_redeem` on, the Codex block in the sidebar shows what auto-redeem would do if the longest usage window ran dry right now. A green `⟳ 2` means a credit would be spent, or no natural reset is known to wait for. A muted `⟳ 2` means the fleet would use paid credit or wait for the free reset instead. When that reset is closer than `resume.auto_redeem_min_gain` and the nearest credit outlives it by a day, it stays muted until the reset. Where redeeming is up to you (every Claude account, and Codex with `auto_redeem` off, though expiry rescue still spends a credit in its last thirty minutes), the block shows `▬ 2` and runs the same rules as advice: green when the longest window is nearly spent and a credit will be worth redeeming once it runs out, green and bold when redeeming one now with [`rimz accounts redeem`](../reference/cli/accounts.md#redeem) is worth it, which includes the credit a redemption would spend being within a day of expiring. `resume.auto_redeem_min_gain` tunes that advice too. The bold glyph breathes while an agent on that account is stopped on a limit. The time until the nearest expiry follows the count, as in `⟳ 2 · 3h`.

### Idle compaction

An idle agent can outlive its provider's warm prompt cache, so its next prompt pays to cache the whole accumulated conversation again. Idle compaction submits the agent's native compact command with its seat's brief (solo or team), with no new prompt behind it, once its context has reached its token floor, unless its account shows a spent cap for its model or a spent usage window that no usable paid extra usage carries. The agent summarizes its conversation and carries on from that smaller context; details not preserved in the summary are lost.

The [configuration rules](./configuration.md#idle-compaction) define the provider timing, eligible agents, and the per-profile and per-role floors. Each attempt leaves a durable command for boundary delivery and an assist record visible in `rimz stats --assists`, including the idle threshold it fired at. If the agent takes another turn after that idle decision while the command is still queued, RimZ drops the command instead of compacting after the new turn; a turn resumed from background work counts when it ends. `rimz message show` keeps the reason. Set `harness.idle_compact` to `off` to disable the machine default; a profile or role that sets its own floor still applies.

If only one agent needs a different floor for its current work, [`rimz agents tune @coder idle-compact 120k`](../reference/cli/agents.md#tune) overrides its configured chain; `reset` restores it.

### Stop when idle

An agent that has finished still holds its pane, its process, and a place in the roster. `rimz agents stop @coder --when-idle` hands the cleanup to the room: the agent stops once it has rested three minutes with nothing owed to it, and an agent can ask for its own exit with `@me` as its last act. A pending wait, an unreported subagent, a soft-compaction continuation, a queued message, an open run, or a team board short of `Done` all hold the stop, as does a launched child that is still working or owed something. If you return and start a turn by typing into the pane or sending `rimz message` from your shell without `--no-from`, your prompt withdraws the stop for good. RimZ deliveries only restart the rest clock. Pass a duration to change the rest, or `off` to withdraw without a prompt; book again to restore a withdrawn stop. The [stop reference](../reference/cli/agents.md#stop) has the exact rule; each stop and human-prompt withdrawal shows in `rimz stats --assists`.

### Smart compaction

Every agent CLI already compacts. `/compact` is the manual command: summarize the conversation and carry on against a fresh window. Auto-compaction is its fallback, firing when the context hits the ceiling, wherever the work happens to stand. Driving one agent by hand, you preempt the fallback without thinking about it: a task wraps up, you type `/compact` at the clean boundary, and the summary hands a finished state to whatever comes next.

In a fleet, a long turn can hit that ceiling with nobody there to ask for a clean handover. Smart compaction watches the context against your seat's threshold and acts at two moments:

- During a turn, RimZ sends a steer asking the agent to finish its current step without leaving files half-edited, start nothing new, and end its turn. This is not an interrupt. Once the turn ends cleanly, RimZ puts continue behind any messages already queued. If context is still above the line, it submits the compact command before continue; if a native compaction already lowered context, it continues without another command. Continue follows even when no compaction was needed. Interrupting the agent or clearing its session cancels this continuation.
- Before a `rimz message` or scheduled wait reaches a resting agent above the same line, RimZ submits the compact command first, then delivers the text against the fresh window. A steer or interrupt to a busy agent never types a compact command into its live turn.

While the agent's account shows a spent cap for its model, or a spent 5-hour or weekly usage window that no usable paid extra usage carries, RimZ sends no wrap-up and holds continue until the window resets, and a message that would compact first is delivered without the compaction. With usable paid extra usage carrying the spent window, both steps go out as usual.

Solo agents, team members, supervised children, and watched panes participate when their adapter supports the trigger. Agents awaiting input are left alone. The provider's own auto-compaction remains the backstop: a slow observation or a long wrap-up can still let it fire first. Recent counted pane input cancels the wrap-up rather than submitting a draft, including after the viewer leaves on Linux. A draft left untouched longer than the [typing hold](./messaging.md#park-steer-interrupt-or-schedule) can still be submitted, just as with an ordinary steer. The [configuration guide](./configuration.md#smart-compaction) lists skipped adapters and the continue-text switch.

Both moments resolve a session override first, then the seat's role or profile-chain `smart-compact`, falling back to `harness.smart_compact`, which defaults to `258k`. Set another occupied-token count like `200k` or a percentage like `70%` to change it, or `off` to stop inheritance. `--smart-compact` overrides the line per message only. [`harness.compact_instruction`](./configuration.md#smart-compaction) changes the summary brief. Each delivered wrap-up counts under `Soft compact:` in `rimz stats --assists`; any compact command that follows counts separately under `Auto-compact:`. The threshold grammar and delivery mechanics are in [messaging → compact before the message lands](./messaging.md#compact-before-the-message-lands).

To give one session more room or ask it to compact sooner, [`rimz agents tune @coder smart-compact 70%`](../reference/cli/agents.md#tune) replaces its configured chain; `reset` restores it. A message's explicit `--smart-compact` wins over that override.

## Permissions for an unattended run

An unattended run has to answer permission prompts without you. Two patterns cover it, and they compose.

Answer in the agent's own UI to keep the full record. A [handler that answers](./notifications.md#handlers-that-answer) sends the answer with `rimz pane send`, leaving the prompt, the answer, and the tool run all in the agent's transcript, exactly as if you had typed it. Prefer this path when handled decisions belong on the record.

Use the agent's bypass flag when the run cannot afford to stop. `--yolo` on the task's `rimz agents` command passes the adapter's bypass flag to the launch (`claude --dangerously-skip-permissions`, `codex --dangerously-bypass-approvals-and-sandbox`), while `--ask` keeps the provider's prompts in place; these are the [permission modes](./fleet.md#set-a-permission-mode) every launch takes. RimZ still observes sessions, completions, and failures through the reporting hooks, but the agent skips permission events at the source, so the durable record has no per-decision audit trail, only what the other hooks report. Reserve the flag for runs where you accept that missing trail.

The guardrails around either choice stay visible: trust grants, notification handlers, and the permission mode itself are product behavior, covered in [security](./security.md).

## A fleet that works nights

A hands-off room composes four layers, from least to most involved: [the recovery reflexes](#keep-the-fleet-moving) keep one agent alive through rate limits with no schedule at all; a scheduled task puts work on a clock; a [signal](#signals-the-rooms-event-bus) or [condition](#wait-for-done-and-green) fires it only when something happened; and a [notification handler](./notifications.md) catches what none of them can decide alone. Reach for the lowest layer that solves the problem, and stack them when the job needs it.

Because a task's agent runs inside the room, its prompt can use every other RimZ command:

```sh
# 02:00 every night: a triage that fans out and opens PRs
rimz loop add nightly --every day --at 02:00 --fresh-worktree nightly --timeout 4h -- \
    rimz agents claude --timeout 4h --budget 5 "Scan the repository for bugs and cheap improvements. For each one worth fixing, \
run a codex -p subagent in its own worktree, review its diff, and open a PR."
```

That is one scheduled turn, capped at four hours and five dollars, and its prompt hands the agent the room's own tools: it fans work out with [subagents](./subagents.md), isolates each fix in a [worktree](./worktrees.md), and could as easily launch a [team](./teams.md) and brief it over [messages](./messaging.md).

Leave the room open, detached on your workstation or on a server you reach with [`rimz remote`](./remote.md), and the night runs on the pieces above: [auto-continue](#auto-continue) carries recoverable runs over rate limits, a question or failure trips a [notification handler](./notifications.md) that reaches your phone, and the [permission choice](#permissions-for-an-unattended-run) stays per task, on its launch. By morning `rimz loop list` and the PR queue show what the night produced.

## See also

- [Workflows](./workflows.md): the run every fire starts, and the graph to write when one command is not enough.
- [Budgets](./budget.md): the dollar caps that bound hands-off work.
- [Notifications](./notifications.md): the push routes and acting handlers that catch what a loop cannot handle alone.
- [Messaging](./messaging.md): the delivery path waits use, `--schedule` for one-off reminders, and smart compaction in full.
- [Loop CLI](../reference/cli/loop.md), [Wait CLI](../reference/cli/wait.md), and [Events CLI](../reference/cli/events.md): every flag, the watcher, and the signal grammar.
- [Configuration](./configuration.md): the `[resume]` and `[harness]` keys, and the `loop.toml` shape.
- [Security and trust](./security.md): the safety story for bypass flags and project trust.
- [Loop internals](../internals/harness/loops.md): the clock, the state files, and the run log underneath, for reading the code.
