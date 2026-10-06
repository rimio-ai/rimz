# Loop CLI

`rimz loop` runs work on a clock, on a signal, or while state conditions hold. A task is a name, one trigger, and one action: start a fresh supervised agent (`--agent`), deliver a prompt to a live agent session (`--wait`), or run a shell command (`--check`). A check can also stand in front of an agent action as a guard. The [loops guide](../../guide/loops.md) teaches when to use each; an alarm an agent sets for itself, after a delay or a watched command, is [`rimz wait`](./wait.md).

A clock task fires while a room for its project is open, from that room's sidebar, or from the optional machine-wide [timer](#timer) when no room is open. A signal task fires from whichever process emits the signal, so it needs no room.

```sh
rimz loop add morning --agent claude --prompt "summarize what landed on main overnight" --every weekday --at 07:00
rimz loop add watchdog --check "cargo test" --on fail --agent codex --prompt "fix the failing test" --every 15m
rimz loop add ci-green --check "gh run watch --exit-status" --on success --until 30m --every 2m --wait @planner --prompt "CI is green; merge"
rimz loop add merged --signal pr.merged --once --wait @planner --prompt "start the follow-up"
rimz loop list
rimz loop show watchdog
rimz loop fire watchdog
```

| Command | What it does |
| --- | --- |
| `rimz loop add <NAME>` | Add a task, or replace the task of that name. |
| `rimz loop remove <NAME>...` | Delete tasks from the stores that own them. Run history stays. |
| `rimz loop rename <NAME> <NEW_NAME>` | Rename a task in its store. |
| `rimz loop enable <NAME>...\|--all` | Arm tasks on this machine and clear holds and strikes. |
| `rimz loop disable <NAME>...\|--all` | Hold tasks on this machine until enabled. |
| `rimz loop pause <NAME> --for <DUR>` | Hold a task for a bounded time. |
| `rimz loop fire <NAME>` | Run a task now in the foreground. |
| `rimz loop stop <NAME>` | Stop a task's active run and release its overlap lock, including a fired one-shot's run. |
| `rimz loop list [--all] [--json]` | List the current room's tasks, grouped by attention and checkout. |
| `rimz loop watch` | Hold a live dashboard open with countdowns and running tasks. |
| `rimz loop show <NAME>` | One task's trigger, health, spend, and recent runs. |
| `rimz loop logs <NAME>` | Full forensics for a task's recent runs. |
| `rimz loop timer install\|status\|remove` | Manage the machine-wide timer. |

Every command takes the [global flags](../cli.md#global-flags). `loop show --json` prints structured task, launch, and run records and the running state. It fails if the task no longer exists; without `--json`, `show` can still display its retained history.

## Add a task

`rimz loop add <NAME>` takes a name of letters, digits, `-`, and `_`, exactly one action, and exactly one trigger. Adding a name that already exists in the same store replaces that task.

### Actions

| Action | Flags | Prompt |
| --- | --- | --- |
| Start a fresh agent | `--agent <SPEC>`: a kind, a profile, or a kind with a mode suffix such as `codex-yolo`. Each fire opens one transient supervised pane that runs the prompt once. | Required: `--prompt` or `--prompt-file`. |
| Wake a live agent | `--wait [<ADDRESS>]`: pins one live session now; see [waits and checks](#waits-and-checks). | Optional. |
| Run a command | `--check <CMD>` with no other action. Records `completed`, `failed`, or `timed out`. | None. |

`--check` combined with `--agent` or `--wait` is a guard: the command runs first, and the action runs only on the outcome `--on` names. A supervised `--agent` task refuses a kind whose hooks are not installed, because a scheduled turn reports completion through them.

`--agent <SPEC> --stay` opens ordinary agents in a new background tab on the checkout's channel, leaves their panes open, and returns without supervising a turn. It accepts the same layout grammar as `rimz agents`, requires a prompt leader and a prompt, and accepts `--effort`. With `--stay`, `--mode ask` and `--mode yolo` override the layout; `--mode auto` leaves each profile its own mode. A missing room is opened detached. Scheduled fires launch only once per task and checkout; changing the task under the same name preserves that memory. `loop fire NAME` launches again, and `loop remove NAME` clears the launch records. `loop show NAME` lists launched checkouts, times, and leader handles; `--json` includes those records and run history. Logs identify each launch's checkout and leader.

A resident condition task retries a failed or skipped fire that leaves no launch record after 5 minutes from that checkout's last fire, while the task is enabled, unpaused, and the condition remains true. A recorded launch suppresses further scheduled fires. Failures remain subject to the task's strike limit; budget skips and a blocked takeover add no strike. Strikes are per task, so one checkout that keeps failing disables the task for every checkout.

`--each-worktree` requires both `--stay` and `--when`. Repeatable `--subscribe <SIGNAL>` and `--takeover` require `--stay`; subscriptions also require the prompt leader's installed, trusted registration hooks. CI and PR subscriptions need `--each-worktree` or `--root <worktree>`; team subscriptions need a team layout. Subscriptions cannot use one-shot triggers (`--in`, `--at` without `--every`, or `--after-reset`), because the task disappears before registration. Agent-family subscriptions need a separately scoped `loop add --wait --signal` task. `--stay` refuses `--account`, `--check`, `--verify`, `--max-attempts`, `--budget`, `--budget-per-day`, `--surplus`, `--surplus-after`, `--timeout`, `--system-prompt-file`, `--worktree`, `--once`, `--wait`, and `--project`.

At registration, the resident prompt leader arms each `--subscribe` as a standing subscription named `loop-<task>-<agent>-<n>`, with an ordinal chosen to avoid name collisions. CI and PR signals match that agent's checkout, not other lanes. Re-registering does not duplicate subscriptions, and ending the session retires them. Removing the launch task before the leader registers arms nothing. After `rimz loop remove NAME`, a live leader keeps the subscriptions it already armed until its session ends; remove one sooner by its own name, `rimz loop remove loop-<task>-<agent>-<n>`.

`--takeover` makes the launched layout the checkout's one owner: every agent already in the launch checkout is stopped before the resident layout opens, and their subscriptions and waits are retired with them. An agent counts when it is live in a pane and its recorded directory is the checkout or anywhere under it (by the path the task launches into, or the physical path where a symlink leads to the checkout), whoever started it: a team member, another loop task's resident, or a solo agent. Agents in another checkout, including a RimZ-owned worktree nested inside this one, are left alone, as are command cells and your own shell. Subagents an agent launched are stopped with it.

Nothing is stopped mid-turn. If any of those agents, or a subagent one of them launched, is working, waiting on you, paused on a limit (a provider limit or a RimZ budget), or compacting, the fire stops nobody, launches nothing, and records `takeover blocked` with the checkout and each busy agent. A resting agent is stopped whatever it has armed, a failed turn included. A `--when` task tries again 5 minutes later, as for any fire that leaves no launch record; an `--every` or `--at` task waits for its next occurrence. The check reads one snapshot and then closes the panes, so an agent that starts a turn in that instant is still stopped. An empty checkout launches as usual, and an agent that cannot be stopped fails the fire without launching or recording that checkout as launched.

A team layout cannot replace a different team that still holds the checkout. This check runs after the takeover stops the old agents, so the fire can leave them stopped, launch nothing, and record an error with a strike. The hold remains while an old member is still recorded as live or the old team's board is not at Done. Bring that board to Done and let the stopped members end before retrying, or use a non-team layout.

Each successful resident launch also appears in `rimz stats --assists`, with its task, checkout, launched handles, condition evidence when present, and the handles it took over from if any (`stopped` in JSON). `rimz stats --json` includes these events and the `resident_launches` count.

With `--each-worktree`, each RimZ-owned checkout has its own condition and `--for` hold, even without a resident pane. The project root and unmanaged worktrees are excluded. Already-matching checkouts are eligible when the task is added. On GitHub this includes PRs already merged or closed; on Gitea the checkout's PR must have been observed open while the room ran. Room teardown and reset clear the PR cache, so Gitea loses that observation. Launched checkouts stay excluded across condition changes, task replacement, and room teardown. Several eligible checkouts may launch on the same tick. Manual `loop fire NAME` targets the owned checkout containing your current directory and refuses elsewhere; it does not fire every checkout.

### Triggers

| Shape | Flags | Repeats |
| --- | --- | --- |
| One-shot | `--at HH:MM`, or `--in <DUR>` | Fires once, then the task removes itself. |
| After a window reset | `--after-reset 5h\|7d` | Fires once at the next reset of the task provider's window, then the task removes itself; see [after a reset](#after-a-reset). |
| Interval | `--every <DUR>`, measured from the last fire | Yes. |
| Calendar | `--every <DAYS> --at HH:MM` | On each matching day. |
| Raw cron | `--cron "<5 fields>"` | Per the expression. |
| Poll-until | `--every <DUR> --until <DUR> --check <CMD>` plus `--agent` or `--wait` | Until the guard fires the action or the deadline passes. |
| Signal | `--signal <NAME\|FAMILY.*>`, narrowed by `--match` | On every delivering signal, or once with `--once`; see [signals](#signals). |
| Condition | `--when <EXPR>` with optional `--for <DUR>` | Once per continuously true period, except resident retries; `--once` retires after the first fire. |

The trigger values follow these rules:

| Value | Accepts |
| --- | --- |
| `--at` | 24-hour `HH:MM`. |
| `--every` interval | An integer with `s`, `m`, `h`, or `d`, rounded up to whole minutes. `0` is refused, and an interval cannot take `--at`. |
| `--every` days | `day`, `weekday`, `weekend`, a list such as `mon,wed,fri`, or a range such as `mon-fri`. Requires `--at`. |
| `--cron` | Five whitespace-separated fields. |
| `--in` | A [duration](../cli.md#durations) in `s`, `m`, `h`, or `d`, greater than zero and less than `24h`. It resolves at add time to a one-shot `--at`, rounded up to the next minute. |
| `--until` | A duration in `s`, `m`, `h`, or `d`, resolved at add time to a deadline. Requires `--check`, `--every`, and `--agent` or `--wait`; refused with `--in`. |

`--every 1d` fires a day after the last fire and drifts with it; `--every day --at 07:00` fires at 07:00. Calendar times, `--in`, and `--until` resolve in the top-level `timezone` setting, or the system zone when it is unset. A clock task arms the first time a clock sees it and fires on the next occurrence after that, so a late room or a new timer never replays missed fires.

### After a reset

`--after-reset <5h|7d>` fires once at the next reset of that window of the task's provider, the `--agent` or `--wait` kind. Add reads the window from the provider's cache and writes its reset instant to the row as `fire-at`; a window that has not started fires on the next scheduler tick instead. An instant that passed before any clock saw the task, or while it was paused, fires on the first tick it is live. The task gets one attempt: a fire, a gate skip, an overlap, a check skip, or an error all remove it, while `rimz loop run` by hand leaves it.

Add refuses what a [window condition](#conditions) refuses, and also a window the provider is not enforcing (use the other span), a window with no reset time, and `--surplus` or `--surplus-after` on the provider's longest window, whose gate is always closed at its reset. `--after-reset` requires `--agent` or `--wait`, conflicts with `--at`, `--every`, `--cron`, `--in`, `--signal`, `--when`, and `--until`, and is refused with `--project`. Listings describe the task as `once at <YYYY-MM-DD HH:MM>` in the configured zone and show that instant as its next fire.

### Conditions

`--when 'team.stage=Done && ci=passed' --for 30m` waits for both readings to stay true for 30 minutes. Evaluation uses the clock tick, not signal history: CI can pass before or after the board reaches Done. The first tick arms without evaluating. A false tick resets the hold; a fired standing task without `--stay` re-arms after a false tick. Holds start at the first true observation, never at a board timestamp, and reset when the room's runtime state is removed.

Expressions accept `!`, `&&`, `||`, and parentheses, in that precedence order. A term is `key=value` or `key=value1,value2` (either value); a window key instead takes `key>=N`, `key<=N`, `key>N`, or `key<N`, with `N` a whole percent from `0` to `100`. Repeating `--when` ANDs the clauses. Whitespace between tokens is ignored; values use letters, digits, `_`, `.`, or `-`. Window percents are the only numbers, and there are no other literals and no functions. Use `!ci=failed`, not `ci!=failed`; a window key refuses `=` and `!=`, and for an exact percent `window.5h.left>=40 && window.5h.left<=40` reads the same.

| Key | Values | Reading |
| --- | --- | --- |
| `team.stage` | `Done` and stages declared by any team in the effective configuration, case-sensitive | The scoped checkout's `blackboard.md` Stage line. |
| `ci` | `passed`, `failed`, `pending` | The room sidebar's last-known CI result. A live runnable condition keeps its checkout in the sidebar's probe set, with or without a pane; removing the task releases it. Without a fresh sidebar it is unknown, including under the external timer. |
| `pr` | `open`, `merged`, `closed` | The room sidebar's last-known PR state for the checkout. Like `ci`, a live condition keeps its checkout probed without a pane. Unknown with no PR link or no sidebar source; branch CI alone does not imply a PR state. A PR in the merge queue, or removed from it, still reads `open`. |
| `pr.queue` | `queued`, `dequeued`, `none` | The room sidebar's last-known merge-queue state for the checkout's open PR, GitHub only: `queued` while it is in the queue, `dequeued` once it has left the queue without merging, until it is queued again, merged, or closed. `none` for a PR that was never queued and for one that is merged or closed. Unknown with no PR link or no sidebar source. Pair it with `pr`: `pr=open && pr.queue=dequeued`. |
| `window.5h.left`, `window.7d.left` | A comparison with a whole percent, `0` to `100` | Percent left in the task provider's 5-hour or 7-day window: `100` minus the cached usage, projected to the tick, so a window whose reset has passed reads `100`. A limit the provider has lifted reads `100`. Unknown without a cached reading of that window. The provider is the kind `--agent` or `--wait` resolves to at add, recorded as `provider` on the row. The account is the room's current one for that provider, or the task's `--account`, whose reading is the last one stored for it: a room refreshes only its current account for the provider and the accounts its agents run on. |

An unknown reading makes its term false, but `!` is boolean NOT: `!ci=failed` is true when CI is unknown. Use `ci=passed,pending` for known and not failed. A failed CI probe leaves the sidebar's last-known reading in place.

A window condition is checked against the provider's cache at add, which refuses: a check-only task, which has no provider (add `--agent` or `--wait`); a kind that selects an exact managed account at launch (Qwen); a room whose account for the kind does not resolve (`rimz accounts list`); no cached reading of the window, or one whose reset has already passed (open the room's sidebar or run `rimz providers --refresh`); and a provider without a window of that span.

Scope is the checkout where the task was added, or `--root <worktree path>`. From the project-root checkout CI means trunk CI. `--worktree` still chooses the agent pane or wait target, not condition scope. `--when` conflicts with `--at`, `--every`, `--cron`, `--in`, `--signal`, and `--until`. `--for` requires `--when` and a positive duration in `s`, `m`, `h`, or `d`. `--project --when` is not yet supported; add without `--project`.

The receipt adds `trigger: when <expression>, for <duration>`, `scope: <checkout>`, and `now: waiting · team.stage: Review, ci: unknown` (or `holding 0s/30m` when already true); a window term reads as `now: waiting · window.5h.left: 8`. Without a room, a CI or PR condition also prints `ci and pr readings come from the room's sidebar; the loop timer alone never sees them`.

`--wait --when` pins the live session like other waits. A live subscription with the same target session, canonical expression, hold, project root, and scoped checkout prints `already subscribed as <name>` without rewriting it, even if its prompt, label, or `--once` differs. Identical conditions on different worktrees remain separate waits.

### Flags

| Flag | Applies to | Meaning |
| --- | --- | --- |
| `--label <TEXT>` | all | Short description of 1 to 60 characters after trimming, on one line. Stored as `label`; `loop show` prints it beside the command. |
| `--stay` | `--agent` | Open a resident layout in a background tab, once per checkout. |
| `--each-worktree` | `--stay --when` | Evaluate and launch separately per RimZ-owned worktree. |
| `--subscribe <SIGNAL>` | `--stay` | Arm a standing subscription for the prompt leader at registration; repeatable. |
| `--takeover` | `--stay` | Stop the checkout's idle agents before launching; a busy one defers the fire, a failed stop aborts it. |
| `--prompt <TEXT>`, `--prompt-file <PATH>` | `--agent`, `--wait` | The prompt the action delivers. The two conflict. |
| `--check <CMD>` | all | Shell command to run; alone it is the action, otherwise the guard. |
| `--when <EXPR>` | all | Repeatable state condition; see [conditions](#conditions). |
| `--for <DUR>` | `--when` | Continuous hold before firing. |
| `--once` | `--signal`, `--when` | Retire after the first delivering fire. |
| `--on fail\|success\|any` | `--check` | Which check outcome fires the action. Default `fail`. |
| `--verify <CMD>` | `--agent` | Command that must pass after the turn; a failure re-prompts the same session. |
| `--max-attempts <N>` | `--verify` | Total turns allowed to make `--verify` pass. Default `3`, at least `1`. |
| `--max-strikes <N>` | all | Consecutive failed fires before the task disables itself. Default `3`; `0` turns the gate off. |
| `--timeout <DUR>` | all | Caps the check, the agent turn, and its verify commands; see [timeouts](#timeouts). `--wait` accepts it only with `--check`. |
| `--worktree <NAME>` | `--agent`, `--wait` | Channel or worktree that hosts the transient pane, or that resolves the `--wait` address. |
| `--mode auto\|ask\|yolo` | `--agent` | Permission posture for the turn. |
| `--effort <EFFORT>` | `--agent` | Reasoning effort passed to the agent. |
| `--account <NAME>` | `--agent` | Named account of the agent's provider that every fire runs on; see [the account pin](#the-account-pin). |
| `--system-prompt-file <PATH>` | `--agent` | Replace the agent's base system prompt with a file's contents. |
| `--budget <AMOUNT[/day]>` | `--agent` | Dollar cap for each spawned run. |
| `--budget-per-day <AMOUNT>` | `--agent` with `--budget` | Daily dollar cap for the task; see [budgets](#budgets). |
| `--surplus <RATIO>`, `--surplus-after <DUR>` | `--agent`, `--wait` | Fire only while the provider window has headroom; see [the surplus gate](#the-surplus-gate). |
| `--throttle on\|off` | `--agent` | `off` exempts the task from [the start throttle](#the-start-throttle). Default `on`. |
| `--root <PATH>` | all | Project whose room hosts the task. Default `.`. |
| `--project` | `--agent`, check-only | Write the task to the project's `.rimz/config.toml`; see [project tasks](#project-tasks). |

`--mode`, `--effort`, `--account`, `--system-prompt-file`, `--budget`, and `--budget-per-day` are refused on `--wait` and check-only tasks, and `--worktree` on check-only tasks.

### The account pin

Without `--account`, each fire takes the room's current account for the agent's provider, so `rimz accounts use` moves the task with the room. `--account <NAME>` holds the task on one [named account](./accounts.md) whatever the room selects: the fire launches on it, and its hooks preflight, daily caps, surplus gate, and window triggers read it. `--account default` pins the provider's own home.

The flag takes a bare name, where `rimz start --account` takes `KIND=NAME`: a task has one agent, so the provider is the one its `--agent` spec resolves to. Add refuses a name that provider does not declare, a provider without named accounts, and an account whose home is missing or unhooked, each with the `rimz accounts` fix. The row stores the name alone as `account`. A tier-routed spec that lands on another provider at fire time resolves the name against that provider, and runs on that provider's account of the same name when it declares one.

A pinned fire never falls back to the room's account. When the account cannot run, the fire records `account skipped` with the reason and its fix, launches nothing, and adds no strike:

| The pinned account | Recorded fix |
| --- | --- |
| is no longer declared for the provider | `rimz accounts add <kind> <name>` |
| belongs to a provider without named accounts | remove `account` from the task |
| has a home that is not a directory | `rimz accounts add <kind> <name>` |
| has a stored logged-out status | the provider's login command, with the account's home set |

The launch resolves the name once more, for the provider its routing lands on; a name that provider does not declare, or a provider without named accounts, records the same skip there. The logged-out skip needs a stored status, which a room writes only for its current account and the accounts its agents run on. A `default` pin has no account home to miss, so a missing provider home fails as it does on an unpinned task, as an `error`. An account with no stored status launches, and a login the provider then rejects fails that run. Hooks missing or untrusted in the account's home stay an `error`, as on any account, so they count a strike.

The receipt's action line names the pin, as ``launches a fresh claude pane on account `work` in <root>``; `rimz loop show` and `rimz loop list` name it beside the agent, as `claude · account work`.

### The receipt

`add` reads back the action, the trigger in words, and the next fire for a clock:

```console
$ rimz loop add deps --agent claude --worktree deps --every mon --at 09:00 --prompt "Check for outdated dependencies."
added loop task `deps`
action: launches a fresh claude pane in /home/you/code/app
trigger: fires every Mon at 09:00
next fire: 2026-09-07 09:00 (in 2d)
live while a room for /home/you/code/app is open
no room is open there; start one with `rimz start`, or use `rimz loop timer install` to fire without one
```

An `--after-reset` task replaces the trigger and next-fire lines:

```console
trigger: fires once after the next claude 5h reset
window: claude 5h · 70% left · resets 2026-09-05 14:00
next fire: 2026-09-05 14:00 (in 2h)
```

A window that has not started reads `window: claude 5h · not started · 100% left` and `next fire: the next scheduler tick, since the window has not started`.

A `--wait` action reads ``action: waits @planner — pinned to claude session `<id>` now; skipped and removed if that session exits``, a check-only task `action: runs check in <dir>`, and a surplus gate adds a `gate:` line. The last line reads `the loop timer will keep time` when the timer is active.

## Where tasks are stored

Each task lives in one of three stores, which `rimz loop list` names in its SOURCE column.

| Store | File | Holds | SOURCE |
| --- | --- | --- | --- |
| Machine | `~/.rimz/loop.toml` | Repeating `--agent` and check-only tasks added without `--project`. | `machine` |
| State | `~/.rimz/ws/<workspace-dir>/records/loop-instances.json` | Every `--wait` task, plus one-shots (`--at`, `--in`, `--after-reset`), `--until`, and `--once` tasks. Rows retire themselves. | `state`, or `team <instance>` for a team binding |
| Project | `<root>/.rimz/config.toml`, `[tasks.<name>]` | Tasks added with `--project`. | `project`, or `project · untrusted` / `project · stale` while trust is missing |

`loop list`, `enable --all`, and `disable --all` cover machine tasks plus the state and project tasks of the project resolved from the current directory or `--root`. Enable, disable, pause, and strike state belong to this machine: machine-wide for machine tasks, per project for state and project tasks.

### Project tasks

`--project` writes a task that ships with the repository. It needs `--every`, `--cron`, or `--signal`, and refuses `--wait`, `--until`, `--once`, `--when`, and `--after-reset`. Hand-written project tasks also reject `when` and `for`. `--root` must lie inside the project of the current directory. A project task stores no `root` or `dir` and always runs at the project root.

A project task runs unattended only when both hold:

- The project is trusted on this machine ([project trust](./hooks-trust.md#project-trust)); project tasks enter the trust hash.
- The task is enabled here with `rimz loop enable <name>`. A task pulled from a repository starts disabled; `loop add --project` enables it on the machine that ran the add.

A trusted project task wins over a machine task of the same name. While the project is untrusted, the project task does not fire and the machine task keeps running.

`add --project`, and `remove` or `rename` of a project task, change the trust surface. What they print depends on trust before the write:

| Trust before | Result |
| --- | --- |
| `trusted`, or no project config | Trust is granted again for the new config. `add` prints `trust: granted — task enabled and ready to fire`; `remove` and `rename` print `trust: kept`. |
| `untrusted` or `stale`, stdin a terminal | The surface diff and `grant trust now?`. Accepting grants trust. |
| `untrusted` or `stale`, no terminal, or declined | ``trust: <state> — project tasks stay inert until you run `rimz trust grant` (review with `rimz trust`)``. |

## Signals

`--signal <SELECTOR>` subscribes a task to one signal name (`ci.failed`) or one whole family (`'ci.*'`). The subscription listens for as long as the task exists and has no deadline; `--once` retires it after the first fire that delivers. `list` and `show` render it as `on <selector> [k=v]`, with `listening` in the NEXT column.

A subscription observes its whole family. A task on `ci.failed` fires on `ci.failed`, records `skipped` for a sibling such as `ci.passed` without spending a turn, and ignores other families. A `'ci.*'` selector fires on every member. `--match KEY=VALUE` requires a top-level payload field to equal the value; repeat it for an all-of set. `--match KEY='*'` requires only that the payload carries the key, including a `null` value. Only the whole value `*` is a wildcard; `feat/*` is literal. Blank match values are refused. A signal that fails a match is ignored. `ci.finished` and `--match conclusion=…` are refused at add time; use `ci.passed` or `ci.failed`.

Signals come from `rimz events emit`, forge state changes on watched checkouts, trunk ref moves, managed worktree creation and removal, agent and team lifecycle transitions, team stage flips, and `rimz wait` watchers. The [signal lines table](./events.md#signal-lines) lists every name and source.

Firing runs in the emitting process, with no queue. A signal reaches only the tasks subscribed at that moment, and nothing replays it later. A disabled or paused task, or a project task whose project is untrusted, skips the signal.

`--signal --wait` with the same target session, selector, matches, and root as a live subscription prints `already subscribed as <name>` and writes nothing, even when the prompt, label, or `--once` differ.

A signal delivered to a running agent (`--wait`) opens with a subject line and a verdict line carrying the task name, then the task's `--prompt`:

```text
waited on ci.failed on check-multi #729 @88824c2
fired after 12m [ci-fix]

read the failing job and fix it
```

A built-in signal carries only what `gh`, `tea`, `git`, or `rimz` cannot answer from the branch and `#N`; the worktree path, the repository, and the PR and checks URLs stay out of the message. Each signal has its own subject:

| Signal | Subject |
| --- | --- |
| `ci.*` | `ci.failed on <branch> #<number> @<head>` |
| `pr.opened`, `pr.merged`, `pr.closed`, `pr.queued` | `pr.merged on <branch> #<number>` |
| `pr.behind` | `pr.behind on <branch> #<number> · <behind_by> behind <base>` |
| `pr.conflicted` | `pr.conflicted on <branch> #<number> · with <base>` |
| `pr.dequeued` | `pr.dequeued on <branch> #<number> · <reason>` |
| `trunk.moved` | `trunk.moved on <trunk> <from>..<to>` |
| `worktree.created` | `worktree.created <name> from <base>` |
| `worktree.removed` | `worktree.removed <name>`, plus ` · branch deleted` when the branch went with it |
| `agent.*` | `agent.idle <handle>`, or the session id for a card with no name |
| `team.idle`, `team.waiting`, `team.ended` | `team.idle <instance>` |
| `team.failed` | `team.failed <instance> · <member>` |
| `team.stage` | `team.stage <instance> · <from> -> <to>` |

A segment whose field the payload lacks is left out with its separator, so a branch with no pull request reads `ci.failed on check-multi @88824c2`. A commit prints its first seven characters. `@<head>` is the worktree's HEAD when the verdict was read, which is the commit the verdict is about unless the branch has unpushed commits. Any other name in a built-in family prints its family's leading segment alone. `pr.dequeued` adds one line, `queue checks: <url>`, since nothing else names the queue commit.

A signal outside the built-in families prints one `key: value` line per top-level payload field, in key order, under the verdict: a string bare and anything else as compact JSON. A key or a string holding a line break prints JSON-quoted, so one field is always one line. `rimz events emit deploy.finished --json '{"env":"prod","detail":{"a":1}}'` delivers:

```text
waited on deploy.finished
fired [smoke]
detail: {"a":1}
env: prod
```

A condition delivery prints its readings by the same rule, `team.stage: Done`, with `unknown` for a reading that had no value. A manual `loop fire` of a signal `--wait` task delivers `fired by hand [<task>]` with no field lines.

A fire that launches an agent (`--agent`) sends a shorter message, since the launched agent's reminder already says which rule started it and why. The event is one line holding the same subject and fields, joined by ` · `, then the prompt:

```text
Rule `smoke` fired here. deploy.finished · detail: {"a":1} · env: prod

run the smoke suite against the new deploy
```

A condition launch lists its readings on that line (``Rule `ship` fired here. team.stage: Done``). A clock fire and a manual `loop fire` have no event, so the agent gets the prompt alone. The run record keeps the signal name and payload, truncating a payload over 4 KiB. `rimz loop logs <task>` prints the payload as one JSON line under each run's `signal:` line, and still reads the retained runs of a fired `--once` task. For a task still registered, `rimz loop show <task> --json` prints those records whole.

Project tasks may use `--signal` and `--match`, and a subscription satisfies their repeat requirement.

### Caller-scoped defaults

A `--wait` subscription fills in scope the caller leaves out. From an agent the caller supplies it; from a user shell the target does.

| Selector | Without these matches | Default |
| --- | --- | --- |
| `ci.*`, `pr.*` | `path`, `branch` | `--match path=<caller's worktree>`. From the root checkout an explicit scope is required; `--match branch='*'` accepts every watched checkout. |
| `team.*` | `team`, `instance` | `--match instance=<caller's cohort>`, such as `forge#feat-x`. A caller outside a team is an error. |
| `agent.*` | `handle`, `session` | None: the add is refused. Name another agent with `--match handle=<other>` or `--match session=<other>`, so a target never wakes on its own lifecycle. A wildcard does not satisfy this requirement. |

From the root checkout, pass `--match branch=<name>`, `--match path=<worktree-path>`, or `--match branch='*'`, add from a linked worktree, or watch the command instead with `rimz wait --run 'gh run watch --exit-status'`. `*` on `branch` means every checkout RimZ is polling (a checkout with a live pane or agent), not every branch on the forge. `--match team='*'` or `--match instance='*'` disables the cohort default for team signals.

```sh
rimz loop add ci-red --signal ci.failed --wait
rimz loop add merged --signal pr.merged --once --wait @me
```

## Waits and checks

`--wait @<handle>` resolves the address at add time and pins that exact session id. `--wait @me` and a bare `--wait` pin the calling agent, identified from its launch environment or process ancestry; a user shell must name a live target. A session that has not registered yet is refused. When the pinned session ends, is lost, or is stopped, its tasks retire; a fire that finds it gone records `target gone` and removes the task.

A `--wait` delivery arrives from `@rimz` and parks until the target's current turn ends. A signal fire carries `Type: SIGNAL`; a clock fire carries `Type: WAIT` and opens `scheduled wait` then `fired [<task>]`. The [message header](./message.md#the-message-header) lists every type. Waits armed by `rimz wait` interrupt instead ([the delivered message](./wait.md#the-delivered-message)).

`--check` runs through `sh` at the root of the checkout the task was added from, a linked worktree included; project tasks run at the project root. The outcome decides the action:

| `--on` | Fires the action on |
| --- | --- |
| `fail` (default) | A non-zero exit, death by signal, or a timeout. |
| `success` | Exit `0`. |
| `any` | Every outcome. |

When the guard fires, the prompt gains ``--- check `<cmd>` exited <code> ---`` and the check's output. A check that does not fire the action records `skipped`.

A scheduled check-only task opens its root's room before it runs the check, so a check that reads room state finds one. A check guarding an `--agent` or `--wait` action runs with no room open; the action opens it if the guard fires.

`--verify <CMD>` runs after an `--agent` turn and re-prompts the same session with the failure until the command passes or `--max-attempts` turns are spent, then records `verify failed`. `--wait` and check-only tasks refuse it, because they have no supervised session to re-prompt. The retry loop is the one [supervised runs](./agents.md#supervised-runs--p) use.

## Budgets, gates, and strikes

Gates run before the check, so a closed gate spends nothing and opens no room. A gate skip never counts as a strike.

### Budgets

`--budget <AMOUNT>` caps each spawned run; a run that overruns it records `budget exceeded`. `--budget-per-day <AMOUNT>` sums the task's run costs in the configured local day and skips a fire when the remaining amount cannot fund the next run's `--budget`. It requires `--budget` for that reason: the gate reserves one run at the per-run cap before it lets a fire through.

A fire records `budget skipped` from any of three sources:

| Source | Applies to |
| --- | --- |
| The task's own `--budget-per-day` | `--agent` |
| A room or account daily cap ([what a cap blocks](./budget.md#what-a-cap-blocks)) | `--agent`, `--wait` |
| An exhausted Qwen 5-hour, 7-day, or 30-day quota window on the task's account | `--agent` |

The room, account, and quota caps are checked again when the agent launches and between verify attempts. A Qwen skip names the Alibaba region, window, usage, and reset.

`loop list` shows today's spend in its COST column, as `$1.20/$15` when a daily cap is set. `loop show` prints the spend on its `spend` line.

### The surplus gate

`--surplus <RATIO>` fires an `--agent` or `--wait` action only while the provider's longest budget window has that much headroom: the share of budget left divided by the share of time left. `1.0x` is the sustainable pace, and `--surplus 1.5x` needs half again as much. `--surplus-after <DUR>` (`m`, `h`, or `d`) first requires that much of the window to have elapsed; alone it implies `--surplus 1.0x`.

A missing, incomplete, expired, or not-started window keeps the gate closed (which is why `--after-reset` on the longest window refuses a surplus gate), so an API-key account without window readings always skips. A managed Qwen `--agent` task paces against its account's 7-day window, and a Qwen `--wait` task always skips. A closed gate records `surplus skipped`, and `loop show` prints the gate and the recorded reason. The headroom model with a worked example is [budgets → the surplus gate](../../guide/budget.md#the-surplus-gate).

### The start throttle

Every fire that starts an agent, resident or one-shot, takes a turn in one queue shared by every room of this RimZ home, so a burst of due tasks starts its agents one after another. It is the last gate: it runs after the check and the deadline, and before the room is born or a one-shot's row is consumed. A `--wait` delivery, a check-only task, a fire its check skipped, and a task with `--throttle off` never enter the queue.

The `[throttle]` table of `loop.toml` (`loop.throttle.*` to `rimz config set`) sets what a turn waits for:

| Key | Default | A start waits while |
| --- | --- | --- |
| `pace` | `10s` | The previous start has not yet reported in. The turn passes on when the provider reports the new agent, or after `pace`, whichever is first. `0s` turns pacing off. |
| `max-wait` | `30m` | Never longer than this: a fire still held then records `throttle skipped`. Greater than zero. |
| `max-active` | unset | That many agents are mid-turn across every workspace of this home. |
| `max-active-per-task` | unset | That many agents this task started are mid-turn in its project. |
| `cpu-pressure`, `io-pressure`, `memory-pressure` | unset | Either the 10-second or the 60-second pressure-stall average of that resource is at or above this percent (`1` to `100`). |
| `min-memory` | unset | Available memory is below this size (`8GB`). |
| `min-disk` | unset | Free disk where the run will write is below this size: for a `--worktree` task, the filesystem its new worktree goes on (`[agents.worktree] dir`); otherwise the launch checkout. |

`pace` and `max-wait` take `s`, `m`, or `h`. With `pace = "0s"` and no other key set the queue is off. An agent counts as active while its turn is running: one compacting its context, waiting on a prompt, resting, or ended does not. Starts are admitted in the order they queued, with one exception: once a waiting run is held by its task's own `max-active-per-task`, that run and every later waiting run of the task let runs of other tasks pass, and each keeps its place among the runs of its own task. Nothing else reorders: a run of another task held by any other limit, or a start that holds the turn, is never passed.

A held fire holds its run lock, so the task shows as held rather than running and a later fire of it records `overlapped`. `loop fire` prints each new reason as `held: <reason>`, and Ctrl-C ends the wait: the fire records `canceled` with exit code `130`, keeps the check it ran, and starts nothing. A fire admitted after a hold records how long it waited, shown as `held 3m` in its `NOTE`. `loop stop` ends a held run like any other and leaves one `canceled` record.

The pressure and memory readings are Linux's (`/proc/pressure`, `/proc/meminfo`). A configured limit this machine cannot read is refused rather than ignored: `rimz start` fails naming the key and the fix, and a fire records `error`. A workspace whose agents cannot be counted holds a capped start, with the workspace named in the reason. An unreadable queue file errors the fire; a malformed ticket is removed with a warning so other tasks can continue.

What pressure means as a number depends on the machine, and in a container `/proc/pressure` may describe the host rather than the container. Before choosing a limit, watch the `THROTTLE` readings in `rimz loop show <task>` while the machine works as usual, and set the limit above them.

### Strikes

After `--max-strikes` consecutive strikes (default `3`), the task disables itself, `loop list` shows `disabled · N strikes`, and `loop_disabled` notification handlers fire. `rimz loop enable` clears the count.

| Run result | Effect on strikes |
| --- | --- |
| `failed`, `timed out`, `error`, `verify failed`, `budget exceeded` | Strike. |
| `completed`, `delivered` after a guard that fired on a failing check | Strike. |
| `completed`, `delivered` otherwise | Resets the count. |
| `skipped` after a passing check | Resets the count. |
| `skipped` after a failing check, or a sibling signal | Neutral. |
| `budget skipped`, `surplus skipped`, `account skipped`, `throttle skipped`, `takeover blocked`, `overlapped`, `canceled`, `expired`, `target gone` | Neutral. |
| `start failed` — the fire left no run behind, so the occurrence is spent without one: either the runner was never spawned, or it exited before it could record anything | Neutral. Shown as a failure, but a fire that ran nothing never auto-disables the task. |

### Timeouts

| What | Cap |
| --- | --- |
| `--check` | The task's `--timeout`, else 5 minutes. A killed check counts as a failure. |
| Scheduled `--agent` turn and its verify commands | The task's `--timeout`, else `loop.default-timeout` (default `2h`, set with `rimz config set loop.default-timeout 3h`). |
| `--agent` turn under `loop fire` | The task's `--timeout`, else unbounded. |

`--timeout` takes `s`, `m`, `h`, or `d`, and must be greater than zero. `loop.default-timeout` is refused the same way.

## Enable, disable, and pause

`loop enable <name>...` arms tasks on this machine and clears disables, pauses, and strike counts. `loop disable <name>...` holds them until the next enable. Both resolve every name before changing any task: an unknown name refuses the command without changing any task. Both take `--all` instead of names, covering machine tasks plus the state and project tasks of the current project.

`loop pause <name> --for <DUR>` holds a task for a bounded time (`s`, `m`, `h`, or `d`, greater than zero) and prints when it resumes. A disabled task refuses a pause; `disable` owns the indefinite hold.

All three write machine-local state and never edit a task definition or a trust-hashed project config. Enabling, or reaching the end of a pause, starts the schedule fresh from that moment, so fires missed while held never replay.

## Fire and stop a run

`loop fire <name>` runs the task now in the foreground, with the same gates, check, overlap guard, and run record as a scheduled fire. It runs a disabled or paused task anyway, after saying so, and leaves one-shots and subscriptions in place. It prints the check's output and the agent's replies as they arrive, then a verdict per stage and, for a supervised run, its run id and transcript. The pane opens beside your shell and closes when the run ends; `--keep` leaves it open.

For an untrusted or stale project task on a terminal, `fire` asks `grant trust and fire?` and refuses if declined; without a terminal it refuses. A scheduled fire of such a task is always refused.

| `loop fire` exit | When |
| --- | --- |
| The run's status: `0` completed, `1` failed, `123` verify failed, `124` timed out, `125` budget exceeded, `130` canceled | An `--agent` action ran. |
| `130` | Ctrl-C interrupted the check; the action does not run. |
| `0` | A gate or check skipped the action, a `--wait` delivered, or a check-only task finished, whatever its check returned. |
| `1` | An error, or a trust refusal. |

A task whose previous run is still active records `overlapped` and does not start another. `loop stop <name>` ends the active run: it marks a linked supervised run canceled, waits 5 seconds for the overlap lock, sends SIGTERM to a remaining holder, and waits again. It prints ``loop `<name>`: stopped`` (with the run id, and `· SIGTERM` when it signaled) or `no active run`, and exits `0`. When the run stopped but its pane was not confirmed closed, it prints the `stopped` line and then exits `1` naming the pane and the command that closes it, `rimz agents stop <run-id>`; a second `loop stop` finds no active run and leaves the pane open. When the lock is still held it exits `1` with the holder's PID and the lock path; SIGKILL is left to you. A fired one-shot (`--at`, `--in`, `--after-reset`) gives up its task row when its run starts; while that run is active, `stop`, `show`, and `logs` still find it by name from inside its project, and `list` and `watch` there keep a row for it.

## Read run history

Every fire appends one record to `~/.rimz/logs/loop-runs.log.jsonl`. `show` and `logs` read records for the current project, and keep reading a task's history after the task is removed.

### `loop list`

`loop list` shows the whole current room, including its worktrees. `--all` shows every known room, the caller's first; without a resolved room it behaves as `--all`. Known rooms come from machine tasks, stored instance tasks, and the caller's root. A project with only project-config tasks is not discovered unless it is the caller's. Other rooms' attention tasks appear on an `elsewhere:` line in the default view; healthy tasks elsewhere stay hidden.

Each heading names the home-relative root, whether its room is open, the task count before collapsing, the next clock fire when present, and nonzero spend today. Without a room, it explains which clocks and forge signals wait for `rimz start`, and whether the optional clock timer is active.

Empty sections are omitted:

- NEEDS YOU names missing checkouts, strike-disabled or failing tasks, throttle holds, trust blocks, and invalid triggers, with commands addressing the named tasks. A missing checkout is flagged, not retired; the task returns to its normal section when the directory returns.
- ROOM has NAME, TRIGGER, ACTION, LAST. Running rows come first. Actions read `run check`, `start <profile>`, or `check, then start <profile>`, with an account pin when set.
- WORKTREES has WORKTREE, TRIGGER, WAKES, LAST, NAME. Deliveries and directory-bound tasks group by checkout, with the caller's checkout first and marked `(here)`. A target matching the calling agent is marked `(you)`. Owned signal subscriptions with the same target, owner, and non-path filters collapse; NAME reads `↳ team <team>` or `↳ <loop task>`. User-named rows without an owner never collapse.

TRIGGER omits the checkout's matching `path` filter, includes labels and pauses, and puts `for <hold>` and `each worktree` on a continuation line. Watched commands read `on command exit`, not their command preview. On a terminal, the trigger wraps at modifier, condition, and word boundaries down to 24 columns; no cell is clipped. If the other columns plus that floor exceed the terminal width, the table overflows. Piped output keeps only the fixed continuation line.

LAST chooses running, off, latest acting result, latest heard sibling signal, watcher liveness, then `never fired`, in that order. A good acting run reads `✓ 53m ago · 143 in a row`; a failed one reads `✗ failed 2m ago · exit 101`. A sibling signal after a good run does not replace it; with only sibling signals the row reads `heard ci.passed 7m ago`. A watch reads `watching 12m` or `lost`; a lost watcher is not NEEDS YOU. Delivery streaks, launched-worktree counts, and spend today follow inline. A daily cap is shown even at zero spend. A repo task not enabled here reads `off · repo task, enable here to run`.

A run whose task row is gone keeps a ROOM row until it ends, with the real name `loop stop` needs, trigger `one-shot fired`, and LAST `▸ running 3m`. Empty rooms say how many tasks exist elsewhere and point to `--all`; with none anywhere, the add hint remains.

`--json` prints one object with a `rooms` array for the same scope (`--all` widens it). Each room has `root`, `open`, `here`, `spend_today_usd`, and `tasks`. Tasks are never collapsed and always retain real names. Each has `name`, `section` (`needs_you`, `room`, `worktrees`), `state` (`live`, `off`, `not_enabled`, `paused`, `running`, `waits_for_room`), unwrapped `trigger`, `you`, and spend. `action` has `kind` (`check`, `start`, `wake`) and `subject`; it is omitted when no definition survives or the action is invalid. Other optional fields are `attention` (`checkout_gone`, `strikes`, `failing`, `held`, `blocked`, `invalid`), `label`, `dir`, `worktree`, `owner` (`kind`: `team`, `loop`; `name`), `next_at`, `running_since`, `last` (`at`, serialized run `result`, `ok`, `streak`), `heard` (`signal`, `at`), and `budget_per_day_usd`. Absent values are omitted, not null; timestamps use the same format as other JSON commands.

Listing is read-only and never creates a room store. Malformed local task state fails the command; a malformed other room warns with its root and is skipped. A run lock that cannot be read warns on stderr with the task's name. When a room's locks directory cannot be listed, one warning names the root and directory, and its rows show their other state.

### `loop watch`

`loop watch` holds this room's tasks open as a compact live dashboard, repainting countdowns and each running task's `▸ running 3m` every second. From a worktree it still shows the whole room; outside any project it shows every known room. Its last-run columns use the same acting results as `list` and `show`, so sibling signals do not replace a good or failed run; with no acting run they read `heard <signal>`. Off, paused, and waiting-for-room tasks keep the list's state words. The band counts running, failed, held, and ok tasks; clocks waiting for a room are excluded from `next:`. A run whose task row is gone is a running row there too, counted in the band's totals and never its `next:`; the dashboard prints no lock warnings. It is the loop pane in the room's `rimzd` tab.

### `loop show`

A condition adds a `condition: <STATE text>` block immediately below the trigger. Each leaf term appears in expression order, followed by `✓` or `✗` for that term's truth and its current reading (`unknown` if absent). Negation applies to the expression, not the leaf's glyph.

`loop show <name>` answers whether the task is working, then shows its evidence. It opens with the task's schedule and next fire or hold, or, while a run holds the task's run lock, `▸ running 3m · pid <PID> · run <run id>` in their place (the PID and elapsed time when the lock names its holder, the run id once the run has a record), then a health verdict from the latest conclusive run: its age, its status and duration, and the streak (how many consecutive conclusive runs ended the same way, and the age of the failure or good run that bounds the streak). A run waiting in [the start throttle](#the-start-throttle) opens with `held: <reason>, 3m · pid <PID>` instead, or `held: waiting for its turn, 3m` before its first reading. Other runs of the task waiting in other processes, as a fan-out's checkouts do, follow as `· 2 held: <first reason>`, so a running checkout keeps its own state; `loop list` and `loop watch` show the same. A running task adds the `loop stop` command under the verdict. For a fired one-shot whose run is still active, the task row is already gone: `show` opens with `<name> — one-shot fired` and the same running text, then the agent, the `loop stop` command, and any recorded runs as `loop logs` prints them.

A fire refused because the previous run was still active is spent: nothing queues or replays it. The run that refused it started before that fire, so the verdict says, in the warning tone, when such fires have had no run since: fires skipped while the active run holds the lock, fires skipped after the last run, or, when no run is active, fires skipped during the last run. A fire refused at another gate (`budget skipped`, `account skipped`, `surplus skipped`, `throttle skipped`, `signal skipped`) ran nothing, so it does not clear the warning.

The sections below follow in this order, and the task's facts close the screen: action and check (one `task` row for a check-only task), `verify`, root (and `dir` for a linked worktree), source, effective timeout, budgets, surplus gate, `throttle off` for an exempt task, spend, and `strikes N/max` once a strike is recorded. For an `--agent` task that is not exempt, a `THROTTLE` section ends the screen with what the machine reads now, each beside its limit when one is set: the three pressure averages, available memory, free disk where the run will write (as `min-disk` reads it), the active agent count, and, with `max-active-per-task` set, the task's own active agent count.

| Section | Contents |
| --- | --- |
| `LAST RUN` | The newest run that is not an overlapped fire. The header carries status, age, duration, exit, `manual` for a hand fire, and the signal that fired it. The body is what `loop logs` prints for that run, except that a passing check prints its last five lines, followed by the `loop logs` command whose first block is this run. A failed check prints every line. While the task is failing and a newer run that is not the failure takes this section, a `last failure` pointer to `loop logs --failed` follows; a healthy task prints none. |
| `AGENT RUNS` | Check-gated `--agent` and `--wait` tasks only: how many fires escalated to the action, the total and average cost across every escalated run the log still holds that recorded one, and the five latest attempts, newest first. |
| `RECENT RUNS` | The latest runs, newest first, identical consecutive runs collapsed into one row. An `overlapped` fire takes no row once a later run is recorded: it joins that run's `NOTE` as a count (`2 fires skipped, run already active`), and the runs on either side of it collapse. Overlapped fires with no later run keep one row at the top. The heading gives the runs these rows stand for out of the runs recorded. `-n, --runs <N>` sets how many rows, default `10`. `MODE` appears when the rows differ in mode, `COST` and `TOKENS` when a row has one. |
| `SUBSCRIPTIONS` | Rows owned by this loop task in its room, one per real task name, without the list's collapsing. Each row shows its checkout, target handle, signal, and last acting run with its age, using the list's LAST wording. Without an acting run it reads `heard <signal> <age>` for a skipped signal, otherwise `never fired`. Omitted when no rows name this task as their owner. |

`loop logs` and `loop show --json` keep one entry per fire, overlapped fires included. The JSON document's keys are `task`, `entry`, `launches`, `runs`, `running`, `held`, and `waiting`. `running` is `null` while no run holds the lock, otherwise an object with `pid`, `started_at`, and `run_id`, each `null` when the lock names no holder or the run has no record yet. `held` is `null` unless that run is waiting in the start throttle, and then an object with `pid`, `checkout`, `since`, `reason` (`null` before the first reading), and `position`, its 1-based place in the queue. `waiting` lists the task's other runs waiting in the throttle, each an object of the same shape. A record admitted after a hold carries `throttle_wait_ms`. A run lock that cannot be read warns on stderr and prints `null`.

A sample is in the [loops guide](../../guide/loops.md#what-a-fire-leaves-behind).

### `loop logs`

`loop logs <name>` prints the full record of the latest runs, oldest first, so the newest ends the output. Each block holds the status, age, mode (`scheduled` or `manual`), exit information, check output, error chains, run ids, pane output tails, cost and tokens, and transcript links when recorded. Agent messages render as [agent prose](../cli.md#agent-prose); check output and errors stay literal. While the task is running, the last line is the running text `loop show` opens with; `--failed` leaves it out.

| Flag | Effect |
| --- | --- |
| `-n, --runs <N>` | How many runs to print. Default `10`. |
| `--failed` | Only `failed`, `timed out`, `budget exceeded`, `verify failed`, and `error` runs. |

## Rename and remove

Condition runs in `loop logs` include `when: <expression> · held <duration>` and a JSON readings line. The record keeps the canonical expression, configured hold, observed held milliseconds, and readings; manual fires do not synthesize readings. Signal runs include `signal: <name>` and, when the signal carried fields, a JSON payload line.

`loop rename <name> <new-name>` moves the task to the new key in its store. The new name must differ and be free. A task with recorded resident launches cannot be renamed because its resident leaders retain the task name. Edit it with `loop add` under the same name, or remove and re-add under a new name (which relaunches its checkouts). A resident task without recorded launches can be renamed. The task re-arms, so an interval task next fires one interval after the rename.

`loop remove <name>...` deletes each task from its store and prints ``removed loop task `<name>` ``, or ``no loop task named `<name>` `` when none exists. Missing names do not stop later removals or cause a nonzero exit. Run history stays readable. For a project task, both commands print the [trust result](#project-tasks).

## Timer

`rimz loop timer install` installs one user-level timer that ticks every minute: a systemd user timer on Linux, a launchd agent on macOS. Each tick re-reads every task and fires due clock tasks only for roots without an open room; a root with a room is left to that room. One timer covers every root, so adding or editing a task needs no reinstall.

| Command | Prints |
| --- | --- |
| `timer install` | `loop timer: installed (<backend>; every 1m) → <path>` |
| `timer status` | `loop timer: active` or `inactive`, the backend and executable, and `task roots without a room: N`; or `loop timer: not installed`. |
| `timer remove` | `loop timer: removed (<backend>)`, or `loop timer: already absent`. `rimz uninstall` removes it too. |

A timer fire follows the same rules as a room's:

- An `--agent` fire starts a room for its project and runs the turn there. A scheduled check-only fire opens the room before the check, once the budget, overlap, and deadline gates pass. The room stays open and keeps time for that root from then on; `timer remove` does not close it.
- A `--wait` task still needs its pinned session, and an untrusted project task stays blocked.
- On Linux each fire runs in a transient `systemd-run --user --scope`, in its own process group, so it outlives the tick. A tick that would take that route checks the systemd user manager is reachable first, and refuses the whole pass naming the fix when it is not: nothing fires, no stamp moves, and the tasks are still due on the next tick.
- A task the timer sees for the first time arms without firing, so installing the timer replays nothing.

The task model, file shapes, and scheduler are in [loops.md](../../internals/harness/loops.md).
