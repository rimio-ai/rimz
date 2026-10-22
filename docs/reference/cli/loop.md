# Loop CLI

`rimz loop` runs work on a clock, on a signal, or while state conditions hold. A task is a name, one trigger, and one action: start a fresh supervised agent (`--agent`), deliver a prompt to a live agent session (`--wait`), or run a shell command (`--check`). A shell check and model judges can guard an agent action, in that order. The [loops guide](../../guide/loops.md) teaches when to use each; an alarm an agent sets for itself, after a delay or a watched command, is [`rimz wait`](./wait.md).

A clock task fires while a room for its project is open, from that room's sidebar, or from the optional machine-wide [timer](#timer) when no room is open. A signal task fires from whichever process emits the signal, without the timer or sidebar. [`rimz events emit`](./events.md#emit-a-signal) still requires an existing room.

```sh
rimz loop add morning --agent claude --prompt "summarize what landed on main overnight" --every weekday --at 07:00
rimz loop add watchdog --every 15m --check "cargo test" --check-timeout 10m --judge haiku --judge-prompt "Are these failures flakes or already fixed on main?" --on fail --agent codex --prompt "fix the failing test"
rimz loop add ci-green --check "gh run watch --exit-status" --on success --until 30m --every 2m --wait @planner --prompt "CI is green; merge"
rimz loop add merged --signal pr.merged --once --wait @planner --prompt "start the follow-up"
rimz loop list
rimz loop show watchdog
rimz loop fire watchdog
```

| Command | What it does |
| --- | --- |
| `rimz loop add <NAME>` | Add a task, or replace a same-named task (machine replacement requires the same project root). |
| `rimz loop remove <NAME>...` | Delete tasks from the stores that own them. Run history stays. |
| `rimz loop rename <NAME> <NEW_NAME>` | Rename a task in its store. |
| `rimz loop enable <NAME>...\|--all` | Arm tasks on this machine and clear holds and strikes. |
| `rimz loop disable <NAME>...\|--all` | Hold tasks on this machine until enabled. |
| `rimz loop pause <NAME> --for <DUR>` | Hold a task for a bounded time. |
| `rimz loop fire <NAME>` | Run a task now in the foreground. |
| `rimz loop stop <NAME>` | Stop a task's active run and release its overlap lock, including a fired one-shot's run. |
| `rimz loop list [--all] [--json]` | List the current room's tasks by lifecycle. |
| `rimz loop watch` | Hold the open room's live glance panel with countdowns and running tasks. |
| `rimz loop show <NAME>` | One task's trigger, health, spend, and recent runs. |
| `rimz loop explain <EXPR>...` | Current condition readings and truth, without adding a task. |
| `rimz loop logs <NAME>` | Full forensics for a task's recent runs. |
| `rimz loop timer install\|status\|remove` | Manage the machine-wide timer. |

Every command takes the [global flags](../cli.md#global-flags). `loop show --json` prints structured task, launch, and run records and the running state. It fails if the task no longer exists; without `--json`, `show` can still display its retained history.

## Add a task

`rimz loop add <NAME>` takes a name of letters, digits, `-`, and `_`, exactly one action, and exactly one trigger. Adding a name that already exists in the same store replaces that task, except that a machine task rooted in another project, or a hand-edited machine row with no root, is refused.

### Actions

| Action | Flags | Prompt |
| --- | --- | --- |
| Start a fresh agent | `--agent <SPEC>`: a kind, a profile, or a kind with a mode suffix such as `codex-yolo`. Each fire opens one transient supervised pane that runs the prompt once. | Required: `--prompt` or `--prompt-file`. |
| Wake a live agent | `--wait [<ADDRESS>]`: pins one live session now; see [waits and checks](#waits-and-checks). | Optional. |
| Run a command | `--check <CMD>` with no other action. Records `completed`, `failed`, or `timed out`. | None. |

A fourth action, starting a [workflow](./workflow.md) run, has no `loop add` flag: write `workflow = "<name>"` in `loop.toml` by hand ([workflow tasks](./workflow.md#loop-tasks)).

`--check` combined with `--agent` or `--wait` is a guard: the command runs first, then any `--judge`, and the action runs only when every answered stage lands on the outcome `--on` names. A supervised `--agent` task refuses a kind whose hooks are not installed, because a scheduled turn reports completion through them. A fresh agent or `--judge` whose directory is the home directory fails its fire, for every kind: nobody answers an unattended folder-trust prompt there; re-add the task from a project directory.

`--agent <SPEC> --stay` opens ordinary agents in a new background tab on the checkout's channel, leaves their panes open, and returns without supervising a turn. It accepts the same layout grammar as `rimz agents`, requires a prompt leader and a prompt, and accepts `--effort` and `--fast` or `--no-fast`. For both supervised and resident tasks, add refuses `--effort` or `--fast` before saving if any agent in the spec cannot take it, with the same message as a direct launch. With `--stay`, `--mode ask` and `--mode yolo` override the layout; `--mode auto` leaves each profile its own mode. A missing room is opened detached. Scheduled fires launch only once per task and scope checkout; changing the task under the same name preserves that memory. `loop fire NAME` launches again, and `loop remove NAME` clears the launch records. `loop show NAME` lists launched checkouts, times, and leader handles; `--json` includes those records and run history. Logs identify each launch's checkout and leader.

A resident condition task retries a failed or skipped fire that leaves no launch record after 5 minutes from that checkout's last fire, while the task is enabled, unpaused, and the condition remains true. A recorded launch suppresses further scheduled fires. Failures remain subject to the task's strike limit; budget skips and a blocked takeover add no strike. Strikes are per task, so one checkout that keeps failing disables the task for every checkout.

`--each-worktree` requires both `--stay` and `--when`. Repeatable `--subscribe <SIGNAL>` and `--takeover` require `--stay`; subscriptions also require the prompt leader's installed, trusted registration hooks. CI and PR subscriptions need `--each-worktree`, `--root <worktree>`, or `--worktree <name>`; team subscriptions need a team layout. Subscriptions cannot use one-shot triggers (`--in`, `--at` without `--every`, or `--after-reset`), because the task disappears before registration. Agent-family subscriptions need a separately scoped `loop add --wait --signal` task. `--stay` refuses `--account`, `--verify`, `--max-attempts`, `--budget`, `--budget-per-day`, `--surplus`, `--surplus-after`, `--timeout`, `--system-prompt-file`, `--once`, `--wait`, and `--project`.

`--stay --worktree <name>` creates or reuses the named RimZ-owned worktree when the task fires, through the same resolver as `rimz teams <team> -w <name>`. A new team worktree starts from fetched trunk; a non-team layout uses the configured base. An existing worktree is reused as is, never reseeded. Conditions, guards, scope budget gates, run history and launch memory stay at the task's scope. Deleting the destination does not clear that memory: use `loop fire NAME` to launch again or `loop remove NAME` to clear it. This combination refuses `--each-worktree` and conditions reading unscoped `ci`, `pr`, `pr.queue`, or `team.stage`, including nested expressions. Use branch-qualified conditions such as `pr[topic]=merged`, window conditions, or a clock trigger instead. A live cohort in the destination refuses with a strike without delivering the prompt; stop it or use `--takeover`. A closed cohort without takeover refuses with the ordinary resume-or-fresh guidance.

At registration, the resident prompt leader arms each `--subscribe` as a standing subscription named `loop-<task>-<agent>-<n>`, with an ordinal chosen to avoid name collisions. CI and PR signals match that agent's checkout, not other lanes. Re-registering does not duplicate subscriptions, and ending the session retires them. Removing the launch task before the leader registers arms nothing. After `rimz loop remove NAME`, a live leader keeps the subscriptions it already armed until its session ends; remove one sooner by its own name, `rimz loop remove loop-<task>-<agent>-<n>`.

With `--worktree`, takeover targets that destination, not the scope, and launches a stopped same-layout cohort fresh. A missing destination stops nobody. `--takeover` makes the launched layout the checkout's one owner: every agent already in the launch checkout is stopped before the resident layout opens, and their subscriptions and waits are retired with them. An agent counts when it is live in a pane and its recorded directory is the checkout or anywhere under it (by the path the task launches into, or the physical path where a symlink leads to the checkout), whoever started it: a team member, another loop task's resident, or a solo agent. Agents in another checkout, including a RimZ-owned worktree nested inside this one, are left alone, as are command cells and your own shell. Subagents an agent launched are stopped with it.

Nothing is stopped mid-turn. If any of those agents, or a subagent one of them launched, is working, waiting on you, paused on a limit (a provider limit or a RimZ budget), or compacting, the fire stops nobody, launches nothing, and records `takeover blocked` with the checkout and each busy agent. A resting agent is stopped whatever it has armed, a failed turn included. A `--when` task tries again 5 minutes later, as for any fire that leaves no launch record; an `--every` or `--at` task waits for its next occurrence. The check reads one snapshot and then closes the panes, so an agent that starts a turn in that instant is still stopped. An empty checkout launches as usual, and an agent that cannot be stopped fails the fire without launching or recording that checkout as launched.

A team layout cannot replace a different team that still holds the checkout. This check runs after the takeover stops the old agents, so the fire can leave them stopped, launch nothing, and record an error with a strike. The hold remains while an old member is still recorded as live or the old team's board is not at Done. Bring that board to Done and let the stopped members end before retrying, or use a non-team layout.

Each successful resident launch also appears in `rimz stats --assists`, with its task, checkout, launched handles, condition evidence when present, and the handles it took over from if any (`stopped` in JSON). `rimz stats --assists --json` includes these events; `rimz stats --json` includes the `resident_launches` count without events.

With `--each-worktree`, each RimZ-owned checkout has its own condition and `--for` hold, even without a resident pane. The project root and unmanaged worktrees are excluded. Already-matching checkouts are eligible when the task is added. On GitHub this includes PRs already merged or closed; on Gitea the checkout's PR must have been observed open while the room ran. Room teardown and reset clear the PR cache, so Gitea loses that observation. This restriction applies to unscoped keys; branch selectors look up PRs in every state from first sight, limited to the newest 500 PRs on Gitea, and a merged PR whose head branch Gitea deleted can read unknown. Launched checkouts stay excluded across condition changes, task replacement, and room teardown. Several eligible checkouts may launch on the same tick. Manual `loop fire NAME` targets the owned checkout containing your current directory and refuses elsewhere; it does not fire every checkout.

### Triggers

| Shape | Flags | Repeats |
| --- | --- | --- |
| One-shot | `--at HH:MM`, or `--in <DUR>` | Fires once, then the task removes itself. |
| After a window reset | `--after-reset 5h\|7d` | Fires once at the next reset of the task provider's window, then the task removes itself; see [after a reset](#after-a-reset). |
| Interval | `--every <DUR>`, measured from the last fire | Yes. |
| Calendar | `--every <DAYS> --at HH:MM` | On each matching day. |
| Raw cron | `--cron "<5 fields>"` | Per the expression. |
| Poll-until | `--every <DUR> --until <DUR>` with `--check` or `--judge`, plus `--agent` or `--wait` | Until the guard fires the action or the deadline passes. |
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
| `--until` | A duration in `s`, `m`, `h`, or `d`, resolved at add time to a deadline. Requires `--check` or `--judge`, `--every`, and `--agent` or `--wait`; refused with `--in` or `--stay`. |

`--every 1d` fires a day after the last fire and drifts with it; `--every day --at 07:00` fires at 07:00. Calendar times, `--in`, and `--until` resolve in the top-level `timezone` setting, or the system zone when it is unset. A clock task arms the first time a clock sees it and fires on the next occurrence after that, so a late room or a new timer never replays missed fires.

### After a reset

`--after-reset <5h|7d>` fires once at the next reset of that window of the task's provider, the `--agent` or `--wait` kind. Add reads the window from the provider's cache and writes its reset instant to the row as `fire-at`; a window that has not started fires on the next scheduler tick instead. An instant that passed before any clock saw the task, or while it was paused, fires on the first tick it is live. The task gets one attempt: a fire, a gate skip, an overlap, a check skip, or an error all remove it, while `rimz loop run` by hand leaves it.

Add refuses what a [window condition](#conditions) refuses, and also a window the provider is not enforcing (use the other span), a window with no reset time, and `--surplus` or `--surplus-after` on the provider's longest window, whose gate is always closed at its reset. `--after-reset` requires `--agent` or `--wait`, conflicts with `--at`, `--every`, `--cron`, `--in`, `--signal`, `--when`, and `--until`, and is refused with `--project`. Listings describe the task as `once at <YYYY-MM-DD HH:MM>` in the configured zone and show that instant as its next fire.

### Conditions

`--when 'team.stage=Done && ci=passed' --for 30m` waits for both readings to stay true for 30 minutes. Evaluation uses the clock tick, not signal history: CI can pass before or after the board reaches Done. The first tick arms without evaluating. A false tick resets the hold; a fired standing task without `--stay` re-arms after a false tick. Holds start at the first true observation, never at a board timestamp, and reset when the room's runtime state is removed.

Expressions accept `!`, `&&`, `||`, and parentheses, in that precedence order. A term is `key=value1,value2` (either value) or `key!=value1,value2` (known and none of those values); a window key instead takes `key>=N`, `key<=N`, `key>N`, or `key<N`, with `N` a whole percent from `0` to `100`. A single value works too. Repeating `--when` ANDs the clauses. Whitespace between tokens is ignored; values use letters, digits, `_`, `.`, or `-`. `unknown` and window percents are the only literals; there are no functions. A window key refuses `=` and `!=`, and for an exact percent `window.5h.left>=40 && window.5h.left<=40` reads the same.

| Key | Values | Reading |
| --- | --- | --- |
| `team.stage` | `unknown`, `Done` and stages declared by any team in the effective configuration, case-sensitive | The scoped checkout's `.rimz/team/blackboard.md` Stage line. |
| `ci`, `ci[branch]` | `unknown`, `passed`, `failed`, `pending` | The room sidebar's last-known CI result. A live runnable condition keeps its checkout in the sidebar's probe set, with or without a pane; removing the task releases it. Without a sidebar source it is unknown, including under the external timer. |
| `pr`, `pr[branch]` | `unknown`, `open`, `merged`, `closed` | The room sidebar's last-known PR state for the checkout or selected branch. Like `ci`, a live condition keeps its checkout probed without a pane. Unknown with no PR link or no sidebar source; branch CI alone does not imply a PR state. A PR in the merge queue, or removed from it, still reads `open`. |
| `pr.queue`, `pr.queue[branch]` | `unknown`, `queued`, `dequeued`, `none` | The room sidebar's last-known merge-queue state for the checkout's or selected branch's open PR, GitHub only: `queued` while it is in the queue, `dequeued` once it has left the queue without merging, until it is queued again, merged, or closed. `none` for a PR that was never queued and for one that is merged or closed. Unknown with no PR link or no sidebar source. Pair it with `pr`: `pr=open && pr.queue=dequeued`. |
| `window.5h.left`, `window.7d.left` | A comparison with a whole percent, `0` to `100` | Percent left in the task provider's 5-hour or 7-day window: `100` minus the cached usage, projected to the tick, so a window whose reset has passed reads `100`. A limit the provider has lifted reads `100`. Unknown without a cached reading of that window. The provider is the kind `--agent` or `--wait` resolves to at add, recorded as `provider` on the row. The account is the room's current one for that provider, or the task's `--account`, whose reading is the last one stored for it: a room refreshes only its current account for the provider and the accounts its agents run on. |

Only `ci`, `pr`, and `pr.queue` take an optional `[branch]` selector, such as `pr[feat/x]=merged`. A selector is nonempty and contains no whitespace, `[` or `]`; its spelling is preserved. It reads that branch even without a checkout for it in the room. A live condition keeps its selected branches probed; removing the task releases them. `ci[branch]` prefers CI of an open or merged PR, then the branch head's CI. Branch readings are unknown until the sidebar has probed them, with no supported origin, when the scope checkout has a detached HEAD, or under the external timer. These readings emit no signals. They are last-known readings, carried forward on a failed probe, not latched merge events.

Every membership key accepts `unknown`: `ci=unknown` matches a missing reading, and `ci=passed,unknown` matches either passed or missing. `=` matches when the reading is in the values, treating a missing reading as `unknown`. `!=` matches only when the reading is known and not in the values: `ci!=failed` is false when CI is unknown, and `ci!=unknown` means known. `!` remains boolean NOT, so `!ci=failed` is true when CI is unknown. Unknown window readings make comparisons false. A failed CI probe leaves the sidebar's last-known reading in place.

A window condition is checked against the provider's cache at add, which refuses: a check-only task, which has no provider (add `--agent` or `--wait`); a kind that selects an exact managed account at launch (Qwen); a room whose account for the kind does not resolve (`rimz accounts list`); no cached reading of the window, or one whose reset has already passed (open the room's sidebar or run `rimz accounts --refresh`); and a provider without a window of that span.

Scope is the checkout where the task was added, or `--root <worktree path>`. For a branch selector, the repository is that scope checkout's origin repository. Unscoped keys keep reading the scope checkout; from the project-root checkout unscoped CI means trunk CI. `--worktree` chooses the launch destination or wait target, not condition scope. With `--stay`, scope-reading conditions are refused alongside `--worktree` as described under [Actions](#actions). `--when` conflicts with `--at`, `--every`, `--cron`, `--in`, `--signal`, and `--until`. `--for` requires `--when` and a positive duration in `s`, `m`, `h`, or `d`. `--project --when` is not yet supported; add without `--project`.

The receipt adds `trigger: when <expression>, for <duration>`, `scope: <checkout>`, and `now: waiting · team.stage: Review, ci: unknown` (or `holding 0s/30m` when already true); a window term reads as `now: waiting · window.5h.left: 8`. Without a room, a CI or PR condition also prints `ci and pr readings come from the room's sidebar; the loop timer alone never sees them`.

`--wait --when` pins the live session like other waits. A live subscription with the same target session, canonical expression, hold, project root, and scoped checkout prints `already subscribed as <name>` without rewriting it, even if its prompt, label, or `--once` differs. Identical conditions on different worktrees remain separate waits.

### `loop explain`

`rimz loop explain <EXPR>... [--root <PATH>] [--provider <KIND>] [--account <NAME>]` evaluates the current readings without adding a task, starting probes, or writing state. Repeat positional clauses to AND them together, just like repeated `--when`. Scope resolves like `loop add`: the caller's checkout, overridden by `--root`. Use `--` before a positional value that is exactly a flag, including `-h` or `--help`.

Output shape:

```text
when: <canonical expression>
scope: <checkout>
  [!]<term>   ✓|✗|? <reading or reason>
now: true|false
```

Each leaf term gets the same line as in `loop show`, in expression order. Exit 0 means true; exit 1 means false; exit 2 means a refused expression. Refused expressions use the same clause, position, and fix diagnostics as `loop add --when`; mistyped keys, forge values, and configured stages within two edits get a `did you mean` suggestion.

Forge terms read the existing room cache only: explain does not keep a selected branch probed. Outside a room they read unknown and print the sidebar hint. Window terms use `--provider` and optionally `--account`, or the room's default account for that provider. Without `--provider`, window readings are unknown and a hint names the missing flag.

### Flags

| Flag | Applies to | Meaning |
| --- | --- | --- |
| `--label <TEXT>` | all | Short description of 1 to 60 characters after trimming, on one line. Stored as `label`; `loop show` prints it beside the command. |
| `--stay` | `--agent` | Open a resident layout in a background tab, once per scope checkout. |
| `--each-worktree` | `--stay --when` | Evaluate and launch separately per RimZ-owned worktree; refused with `--worktree`. |
| `--subscribe <SIGNAL>` | `--stay` | Arm a standing subscription for the prompt leader at registration; repeatable. |
| `--takeover` | `--stay` | Stop the checkout's idle agents before launching; a busy one defers the fire, a failed stop aborts it. |
| `--prompt <TEXT>`, `--prompt-file <PATH>` | `--agent`, `--wait` | The prompt the action delivers. The two conflict. |
| `--check <CMD>` | all | Shell command to run; alone it is the action, otherwise the guard. |
| `--check-timeout <DUR>` | `--check` | Cap for the shell check; default `--timeout`, else `5m`; a killed check fails. |
| `--judge <PROFILE>` | `--agent`, `--wait` | Profile for a headless judge, resolving to one Claude or Codex agent. May follow `--check`; requires `--on`. |
| `--judge-prompt <TEXT>`, `--judge-prompt-file <PATH>` | `--judge` | Judge question; exactly one is required. |
| `--judge-recheck <DUR>` | `--judge --stay` | Re-ask after a decline; no default. Refused on non-resident tasks. |
| `--judge-timeout <DUR>` | `--judge` | Cap for the judge; default `5m`, independent of `--timeout`. |
| `--when <EXPR>` | all | Repeatable state condition; see [conditions](#conditions). |
| `--for <DUR>` | `--when` | Continuous hold before firing. |
| `--once` | `--signal`, `--when` | Retire after the first delivering fire. |
| `--on fail\|success\|any` | `--check`, `--judge` | Outcome every stage must land on for the action to run; required with `--judge`, otherwise default `fail`. |
| `--verify <CMD>` | `--agent` | Command that must pass after the turn; a failure re-prompts the same session. |
| `--max-attempts <N>` | `--verify` | Total turns allowed to make `--verify` pass. Default `3`, at least `1`. |
| `--max-strikes <N>` | all | Consecutive failed fires before the task disables itself. Default `3`; `0` turns the gate off. |
| `--timeout <DUR>` | all except `--stay` | Cap for the agent turn, each verify command, and the shell check when it has no `--check-timeout`; see [timeouts](#timeouts). `--wait` accepts it only with `--check`. |
| `--worktree <NAME>` | `--agent`, `--wait` | Channel or worktree for a transient pane or `--wait` address; with `--stay`, create or reuse this named worktree at fire time. |
| `--mode auto\|ask\|yolo` | `--agent` | Permission posture for the turn. |
| `--effort <EFFORT>` | `--agent` | Reasoning effort passed to the agent. Add refuses before saving if any agent in the spec cannot take it, with the direct launch's message. |
| `--fast`, `--no-fast` | `--agent` | Launch in the provider's [fast mode](../agent-support.md#fast-mode), or request none even where the profile sets `fast`. Add refuses `--fast` before saving if any agent in the spec lacks a fast-mode switch, with the direct launch's message; `--no-fast` is accepted on every kind. Stored as the task's `fast = true` or `false`. |
| `--account <NAME>` | `--agent` | Named account of the agent's provider that every fire runs on; see [the account pin](#the-account-pin). |
| `--system-prompt-file <PATH>` | `--agent` | Replace the agent's base system prompt with a file's contents. |
| `--budget <AMOUNT[/day]>` | `--agent` | Dollar cap for each spawned run. |
| `--budget-per-day <AMOUNT>` | `--agent` with `--budget` | Daily dollar cap for the task; see [budgets](#budgets). |
| `--surplus <RATIO>`, `--surplus-after <DUR>` | `--agent`, `--wait` | Fire only while the provider window has headroom; see [the surplus gate](#the-surplus-gate). |
| `--throttle on\|off` | `--agent` | `off` exempts the task from [the start throttle](#the-start-throttle). Default `on`. |
| `--overlap <POLICY>` | repeating clock/signal check-only or nonresident `--agent` | `skip` (default), `coalesce`, `queue`, or `replace`; see [overlapping triggers](#overlapping-triggers). |
| `--overlap-limit <N>` | `--overlap queue` | Maximum waiting triggers, at least `1`, default `10`. |
| `--root <PATH>` | all | Project whose room hosts the task. Default `.`; inside an `empty-dir` agent's directory, the default is the checkout that agent was launched from. |
| `--project` | `--agent`, check-only | Write the task to the project's `.rimz/config.toml`; see [project tasks](#project-tasks). |

`--mode`, `--effort`, `--fast`, `--no-fast`, `--account`, `--system-prompt-file`, `--budget`, and `--budget-per-day` are refused on `--wait` and check-only tasks, and `--worktree` on check-only tasks.

### The account pin

Without `--account`, each fire takes the room's current account for the agent's provider: its pin, else the trusted project default, else the live machine default, else `default`. Pinning, unpinning, or changing a default the room follows moves the task with the room. `--account <NAME>` holds the task on one [named account](./accounts.md) whatever the room selects: the fire launches on it, and its hooks preflight, daily caps, surplus gate, and window triggers read it. `--account default` pins the provider's own home.

The flag takes a bare name, where `rimz start --account` takes `KIND@NAME`: a task has one agent, so the provider is the one its `--agent` spec resolves to. Add refuses a name that provider does not declare, a provider without named accounts, and an account whose home is missing or unhooked, each with the `rimz accounts` fix. The row stores the name alone as `account`. A pinned task uses the provider and model resolved from its definition, without availability-based tier fallback. Unpinned tasks retain availability-based routing.

A pinned fire never falls back to the room's account. When the account cannot run, the fire records `account skipped` with the reason and its fix, launches nothing, and adds no strike:

| The pinned account | Recorded fix |
| --- | --- |
| is no longer declared for the provider | `rimz accounts add <kind> <name>` |
| belongs to a provider without named accounts | remove `account` from the task |
| has a home that is not a directory | `rimz accounts add <kind> <name>` |
| has a stored logged-out status | the provider's login command, with the account's home set |

The launch resolves the name once more for the declared provider. A name that provider does not declare, or a provider without named accounts, records the same skip there. The logged-out skip needs a stored status, which a room writes only for its current account and the accounts its agents run on. A `default` pin has no account home to miss, so a missing provider home fails as it does on an unpinned task, as an `error`. An account with no stored status launches, and a login the provider then rejects fails that run. Hooks missing or untrusted in the account's home stay an `error`, as on any account, so they count a strike.

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

For `--agent recon --stay --worktree review-merge`, the action instead reads ``leaves resident layout recon in worktree `review-merge` (created from trunk for a team, the configured base otherwise, or reused when the task fires)``. A separate `scope` line says that condition, guards and launch memory stay at the scope path. With `--takeover`, the before-launch line names ``worktree `review-merge` `` rather than the scope checkout.

An `--after-reset` task replaces the trigger and next-fire lines:

```console
trigger: fires once after the next claude 5h reset
window: claude 5h · 70% left · resets 2026-09-05 14:00
next fire: 2026-09-05 14:00 (in 2h)
```

A window that has not started reads `window: claude 5h · not started · 100% left` and `next fire: the next scheduler tick, since the window has not started`.

A judge or a shell check before an action adds one `guard:` line. The watchdog above reads `guard: runs cargo test (cap 10m), then asks haiku (cap 5m); acts when both fail, a judge with no verdict counting as a fail`.

A `--wait` action reads ``action: waits @planner — pinned to claude session `<id>` now; skipped and removed if that session exits``, a check-only task `action: runs check in <dir>`, and a surplus gate adds a `gate:` line. The last line reads `the loop timer will keep time` when the timer is active.

## Where tasks are stored

Each task lives in one of three stores:

| Store | File | Holds | SOURCE |
| --- | --- | --- | --- |
| Machine | `~/.rimz/loop.toml` | Repeating `--agent` and check-only tasks added without `--project`. | `machine` |
| State | `~/.rimz/ws/<workspace-dir>/records/loop-instances.json` | Every `--wait` task, plus one-shots (`--at`, `--in`, `--after-reset`), `--until`, and `--once` tasks. Rows retire themselves. | `state`, or `team <instance>` for a team binding |
| Project | `<root>/.rimz/config.toml`, `[tasks.<name>]` | Tasks added with `--project`. | `project`, or `project · untrusted` / `project · stale` while trust is missing |

Adding a state task requires an existing room: run `rimz start` in its root first, or pass `--root <room>`. Machine and project definitions can be added without a room and create no room state.

A machine task takes precedence over a same-named state task only within the same root. A machine task rooted elsewhere neither hides the room's state task nor is removed when you add that state task. `loop show`, `remove`, `rename`, `pause`, `run`, and `stop` in that room address its state task; the machine task remains reachable from its own room or outside any room.

Adding a standing machine task also requires that an existing machine task of that name have the same resolved project root. A cross-project collision refuses without changing either task, names the stored root, and suggests another name or `rimz loop remove <NAME>` from the owning project. Re-adding from that project, or with `--root <owning-project>`, still replaces its task. A machine row with no root refuses too; run `rimz loop remove <NAME>` from a fresh directory outside any repository or RimZ project before reusing its name, so it cannot select a same-named room or project task instead.

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

Firing runs in the emitting process. A signal reaches only the tasks subscribed at that moment; a later subscription never receives it. A busy task can retain its own trigger through its [overlap policy](#overlapping-triggers). A disabled or paused task, or a project task whose project is untrusted, skips the signal.

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

`--check` runs through plain `sh` at the root of the checkout the task was added from, a linked worktree included; project tasks run at the project root. It runs before the judges, which run in file order, then the action. The first answered stage that misses `--on` stops the chain:

| `--on` | Fires the action on |
| --- | --- |
| `fail` (default without a judge) | Each shell result is non-zero, death by signal, or a timeout; each judge verdict is negative. |
| `success` | The shell exits `0`; each judge verdict is positive. |
| `any` | Every outcome. |

When the guard fires, the prompt gains ``--- check `<cmd>` exited <code> ---`` and the check's output. A check that does not fire the action records `skipped`.

Plain `sh` reports a pipeline's last command: `cargo test | tail` can pass when the tests failed. Pipe nothing; RimZ caps the captured output tail itself.

A scheduled check-only task opens its root's room before it runs the check, so a check that reads room state finds one. A check guarding an `--agent` or `--wait` action runs with no room open; the action opens it if the guard fires.

`--verify <CMD>` runs after an `--agent` turn and re-prompts the same session with the failure until the command passes or `--max-attempts` turns are spent, then records `verify failed`. `--wait` and check-only tasks refuse it, because they have no supervised session to re-prompt. The retry loop is the one [supervised runs](./agents.md#supervised-runs--p) use.

`--judge` requires `--agent` or `--wait`, explicit `--on`, and exactly one of `--judge-prompt` and `--judge-prompt-file`. The prompt flags, `--judge-recheck`, and `--judge-timeout` require `--judge`; `--judge-timeout` without it names `--check-timeout` as the shell cap. `--judge-recheck` also requires `--stay`. Layouts with multiple cells and kinds other than Claude or Codex are refused at add. Judges are stored as a [judge table or ordered array](../../guide/configuration.md#looptoml-scheduled-turns); an old agent table under `check` still reads as the first judge. A judge cannot be a check-only action. `--check-timeout` requires `--check`, and `--on` requires `--check` or `--judge`.

Both stages guard `--stay` launches. A resident judge's polarity miss is remembered per task and checkout, including its tier and the whole guard definition. With a standing hold, the shell still runs on every eligible fire; if it reaches the judges, none is re-asked, and the held tier is recorded as `reused` with zero spend. Without a shell, scheduled helpers are suppressed entirely. A shell decline, a changed guard or `--on`, a new true condition edge, task redefinition, or the declining tier's `--judge-recheck` expires the hold. No recheck means an indefinite hold; `0` re-asks every fire. Manual `loop fire` re-asks; a valid firing verdict clears that checkout's decline. Removal clears it, and rename moves it. Ordinary non-resident judges re-ask every eligible fire. A missing verdict continues to the next judge or action without recording a decline.

The judge runs headlessly in the task's checkout, with the profile's isolation and the room-default account. It has no pane or user to question, and is instructed to change nothing. This is an instruction, not an enforced read-only permission mode. Its final answer is one JSON object with `pass` and `reason`: `pass: true` counts as success, `pass: false` as failure. A missing or invalid verdict, provider failure, or timeout continues the chain, with the error in `loop logs`. Strikes use the last stage with a verdict, so an outage alone cannot disable a successfully launched task. A missing profile, unsupported kind, or unreadable prompt file instead records `error` and stops the fire; completed stages and their spend survive.

`--judge-timeout` caps only the judge, default `5m`; the task's `--timeout` never caps a judge. Relative judge prompt files resolve beside `loop.toml` for machine and instance tasks, or beside `.rimz/config.toml` for project tasks. Each judge's prompt receives the shell output and every earlier judge's heading and reason or error. The action receives all completed evidence, including ``--- judge `<profile>` (<kind> <model>): pass ---`` (or `fail` or `no verdict`). `loop logs` shows every stage's duration, verdict, reason, cost, tokens, and reuse status. Recorded cost and tokens sum every judge and the action.

Ctrl-C before, during, or between stages cancels the guard and the fire, exits `130`, and neither launches the action nor sends a delivery. Completed evidence remains. This also applies when the start throttle is off or has no limits.

Every fresh judge decline appears in `rimz stats --assists` and increments `judge_declines` in `rimz stats --json`, including ordinary tasks. Reused decisions do not append assists. Decline memory remains resident-only.

## Budgets, gates, and strikes

The action's budget, account, and deadline gates run before the guard, so their refusals spend nothing and open no room. Each judge also passes scope gates for its own kind and room-default account before its process starts; a refusal stops the fire without launching the action, retaining earlier stages and spend. The start throttle runs after the guard. A gate skip never counts as a strike.

### Budgets

`--budget <AMOUNT>` caps each spawned run; a run that overruns it records `budget exceeded`. `--budget-per-day <AMOUNT>` sums the task's run costs in the configured local day and skips a fire when the remaining amount cannot fund the next run's `--budget`. It requires `--budget` for that reason: the gate reserves one run at the per-run cap before it lets a fire through.

A fire records `budget skipped` from any of three sources:

| Source | Applies to |
| --- | --- |
| The task's own `--budget-per-day` | `--agent` |
| A room or account daily cap ([what a cap blocks](./budget.md#what-a-cap-blocks)) | `--agent`, `--wait` |
| An exhausted Qwen 5-hour, 7-day, or 30-day quota window on the task's account | `--agent` |

The room, account, and quota caps are checked again when the agent launches and between verify attempts. A Qwen skip names the Alibaba region, window, usage, and reset.

`loop list` includes today's spend in LAST, as `$1.20 / $15 today` when a daily cap is set. `loop show` prints the spend on its `spend` line.

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

A held fire holds its run lock, so the task shows as held rather than running and a later scheduled fire follows the task's overlap policy. A coalescing fire absorbs waiting triggers again after throttle admission, without starting a second run. `loop fire` prints each new reason as `held: <reason>`, and Ctrl-C ends the wait: the fire records `canceled` with exit code `130`, keeps the check it ran, and starts nothing. A fire admitted after a hold records how long it waited, shown as `queued 3m` in its `NOTE`. `loop stop` ends a held run like any other and leaves one `canceled` record.

The pressure and memory readings are Linux's (`/proc/pressure`, `/proc/meminfo`). A configured limit this machine cannot read warns at `rimz start`, naming the key and the fix; the room opens, but a fire records `error` rather than ignoring the limit. A workspace whose agents cannot be counted holds a capped start, with the workspace named in the reason. An unreadable queue file errors the fire; a malformed ticket is removed with a warning so other tasks can continue.

What pressure means as a number depends on the machine, and in a container `/proc/pressure` may describe the host rather than the container. Before choosing a limit, watch the `load` or `THROTTLE` readings in `rimz loop show <task>` while the machine works as usual, and set the limit above them.

### Strikes

After `--max-strikes` consecutive strikes (default `3`), the task disables itself, `loop list` shows `disabled after N strikes` under NEEDS YOU, and `loop_disabled` notification handlers fire. `rimz loop enable` clears the count.

| Run result | Effect on strikes |
| --- | --- |
| `failed`, `timed out`, `error`, `verify failed`, `budget exceeded`, `replaced` | Strike. |
| `completed`, `delivered`, `launched` whose last answered guard stage failed | Strike. |
| `completed`, `delivered`, `launched` otherwise | Resets the count. |
| `skipped` whose last answered guard stage passed | Resets the count. |
| `skipped` whose last answered guard stage failed or is absent, or a sibling signal | Neutral. |
| `skipped` by a workflow fire whose checkout already has an active or parked run (`workflow_skipped` in JSON) | Neutral. |
| `budget skipped`, `surplus skipped`, `account skipped`, `throttle skipped`, `takeover blocked`, `overlapped`, `deferred`, `canceled`, `expired`, `target gone` | Neutral. |
| `start failed` — the fire left no run behind, so the occurrence is spent without one: either the runner was never spawned, or it exited before it could record anything | Neutral. Shown as a failure, but a fire that ran nothing never auto-disables the task. |

### Timeouts

| What | Cap |
| --- | --- |
| `--check` | `--check-timeout`, else the task's `--timeout`, else 5 minutes. A killed check counts as a failure. |
| Each judge | Its own `--judge-timeout` or table `timeout`, else 5 minutes, independent of `--timeout`. No verdict continues the chain. |
| Scheduled `--agent` turn and its verify commands | The task's `--timeout`, else `loop.default-timeout` (default `2h`, set with `rimz config set loop.default-timeout 3h`). |
| `--agent` turn under `loop fire` | The task's `--timeout`, else unbounded. |

`--timeout`, `--check-timeout`, and `--judge-timeout` take `s`, `m`, `h`, or `d`, and must be greater than zero. `loop.default-timeout` is refused the same way. Resident `--stay` tasks accept `--check-timeout` and `--judge-timeout`, not `--timeout`.

## Enable, disable, and pause

`loop enable <name>...` arms tasks on this machine and clears disables, pauses, and strike counts. `loop disable <name>...` holds them until the next enable. Both resolve every name before changing any task: an unknown name refuses the command without changing any task. Both take `--all` instead of names, covering machine tasks plus the state and project tasks of the current project.

`loop pause <name> --for <DUR>` holds a task for a bounded time (`s`, `m`, `h`, or `d`, greater than zero) and prints when it resumes. A disabled task refuses a pause; `disable` owns the indefinite hold.

All three write machine-local state and never edit a task definition or a trust-hashed project config. Enabling, or reaching the end of a pause, starts the schedule fresh from that moment, so fires missed while held never replay.

## Fire and stop a run

`loop fire <name>` runs the task now in the foreground, with the scheduled runner's check, overlap guard, resource gates, and run record. It runs a disabled or paused task anyway, after saying so, and leaves one-shots and subscriptions in place. An unmet condition prints `condition not met (k of n); running anyway, task stays armed`; expressions without a conjunction count omit `(k of n)`. This cache-only observation uses the selected checkout for fan-out tasks and never changes arming or records scheduled condition evidence. Met conditions and non-condition tasks add no notice. It prints the check's output and the agent's replies as they arrive, then a verdict per stage and, for a supervised run, its run id and transcript. The pane opens beside your shell and closes when the run ends; `--keep` leaves it open.

For an untrusted or stale project task on a terminal, `fire` asks `grant trust and fire?` and refuses if declined; without a terminal it refuses. A scheduled fire of such a task is always refused.

| `loop fire` exit | When |
| --- | --- |
| The run's status: `0` completed, `1` failed, `123` verify failed, `124` timed out, `125` budget exceeded, `130` canceled | An `--agent` action ran. |
| `130` | Ctrl-C interrupted the check; the action does not run. |
| `0` | A gate or check skipped the action, a `--wait` delivered, or a check-only task finished, whatever its check returned. |
| `1` | An error, or a trust refusal. |

A task whose previous run is still active never starts another concurrently. By default it records `overlapped`; retaining policies record `deferred` unless the queue is full. `loop stop <name>` ends the active run: it marks a linked supervised run canceled, waits 5 seconds for the overlap lock, sends SIGTERM to a remaining holder, and waits again. It prints ``loop `<name>`: stopped`` (with the run id, and `· SIGTERM` when it signaled) or `no active run`, and exits `0`. When the run stopped but its pane was not confirmed closed, it prints the `stopped` line and then exits `1` naming the pane and the command that closes it, `rimz agents stop <run-id>`; a second `loop stop` finds no active run and leaves the pane open. When the lock is still held it exits `1` with the holder's PID and the lock path; SIGKILL is left to you. A fired one-shot (`--at`, `--in`, `--after-reset`) gives up its task row when its run starts; while that run is active, `stop`, `show`, and `logs` still find it by name from inside its project, and `list` and `watch` there keep a row for it.

During a check, the runner forwards SIGTERM to the check's process group and keeps the lock until the check exits, bounded only by its own timeout. The stopped run records one `canceled` row. If triggers were waiting, an extra stop receipt line reports their count, says the next starts now, and names `rimz loop pause <name>` to drop them.

### Overlapping triggers

Only one run of a task can hold its run lock. `--overlap` chooses what a scheduled clock or signal trigger does when it passes the scope gates and finds that lock held:

| Policy | Busy trigger |
| --- | --- |
| `skip` (unset) | Records `overlapped` and is spent. |
| `coalesce` | Records `deferred` and keeps one waiting trigger, merging earlier waiting triggers into the newest. |
| `queue` | Records `deferred` and waits in firing order. At `--overlap-limit` (default `10`), records `overlapped` with `queue full (N/N), skipped`. |
| `replace` | Records `deferred`, retains the newest waiting trigger, and stops the active run. The old run records `replaced`, a strike. |

Waiting triggers survive runner crashes. They run after the active run releases its lock, with a scheduler backstop for a runner that died. Each reruns arming, trust, scope caps, deadline, check, and throttle at its own start time. Paused or disabled tasks drop their waiting triggers without another history row; another gate's refusal consumes one trigger and records that gate's result. Removing or redefining a task clears its waiting triggers, and renaming moves them. If hand-editing its file removes a task, makes its action invalid, or leaves it without a retaining `overlap` (unset, `skip`, or a shape that refuses the setting), the next drain drops its waiting triggers without another history row. `loop stop` does not clear waiting triggers; pause or disable the task to prevent them running.

Replace uses the same cancellation and SIGTERM ladder as `loop stop`, without escalating to SIGKILL. A stop that cannot end the holder leaves the newest trigger waiting. Further arrivals merge into that slot without another stop of the same holder. A runner still held by the start throttle is not stopped: it absorbs the newest trigger after admission. An agent run parked on an owed wait (its turn ended cleanly while it still owes a wait, a wake message, or a launched fleet's result) is stopped and records `replaced`; three consecutive replacements disable the task at the default strike threshold. A budget park ends its run as `budget exceeded`, so the next trigger starts fresh. A run that finishes on its own before the stop reaches it keeps its own result and records no strike, and the newest trigger runs after it.

Newest and oldest go by firing time, not by the order busy runners reach the task: a signal trigger fired when its signal was emitted, so every task woken by one emit shares that time, and a clock trigger fired when its runner started. Triggers that fired at the same instant keep the order they arrived in. Coalescing keeps the newest signal name and payload. If both payloads have the same signal name, carry `from` and `to`, and agree on every other key, it preserves the oldest `from`. Otherwise the newest payload stands whole. The served run records `absorbed` for the number of earlier triggers folded in (omitted when zero). Clock triggers merge only their count. Queued triggers keep their individual payloads.

Every explicit policy, including `skip`, is refused for `--stay`, `--when`, one-shots (including `--once` or a deadline), `--wait`, and workflows. Residents launch once per checkout, conditions are not clock or signal triggers, one-shots retire their task row, deliveries have no run to overlap, and workflows own admission. `--overlap-limit` is refused without `queue`, or at zero. Project tasks accept the same settings. Manual `loop fire` never enqueues: it still refuses a busy task with the stop hint.

## Read run history

Every attempted fire appends one record to `~/.rimz/logs/loop-runs.log.jsonl`. A retained trigger records `deferred`, then its served run records a separate terminal outcome. A busy or empty drainer, or a drained trigger dropped because the task is paused, disabled, missing, invalid, or no longer retaining, writes no row. `show` searches known rooms, while `logs` reads records for the current project. Both keep reading a task's history after the task is removed.

### `loop list`

`loop list` shows the whole current room, including its worktrees. `--all` shows every known room, the caller's first, and unfolds older DONE entries and WAKE-UPS. Outside a project, in a directory with no recorded room, tasks, or one-off history, it shows every room, still folded unless `--all` is explicit. An explicit `--root` keeps that root selected. Known rooms come from machine tasks, stored instance tasks, and the caller's root. A project with only project-config tasks is not discovered unless it is the caller's. Other rooms' attention tasks appear on an `elsewhere:` line in the default view; healthy tasks elsewhere stay hidden.

Each heading names the home-relative root, whether its room is open, the task count before collapsing, the next clock fire when present, and nonzero spend today. Without a room, it explains which clocks and forge signals wait for `rimz start`, and whether the optional clock timer is active.

Empty sections are omitted:

- NEEDS YOU names missing checkouts, strike-disabled or failing tasks, throttle holds, trust blocks, invalid triggers, and workflow tasks with a parked run, with commands addressing the named tasks. A parked task reads `<n> run(s) parked: <workflow> <node> <reason>[: <text>] <age> <checkout>` and lists each parked run on its own line below the table, unless the task's one parked run is already its reason. A task that needs you for another reason, such as strikes, still lists its parked runs there. A missing checkout is flagged, not retired; the task returns to its normal section when the directory returns.
- ROUTINE has NAME, RUNS, NEXT, ACTION, LAST. It holds recurring work, including tasks that launch once in each worktree. NEXT carries the next clock time separately from RUNS. Actions read `run check`, `start <profile>`, or `check, then start <profile>`, with an account pin when set.
- WAITING has NAME, WAITING FOR, THEN, with the note `one-offs, fire once`. It holds unfired one-shots, command watches without a delivery target, and single-checkout resident launches. WAITING FOR includes disabled, paused, running, or held state when applicable. Pure-AND conditions with an observed verdict append `k of n met`, plus the number not read when nonzero. Disabled, paused, unarmed, blocked, OR, and fan-out conditions have no count.
- DONE has NAME, FIRED, with the note `recent one-offs`. It merges recorded single-checkout resident launches, retired one-offs, and rowless active runs. Running rows stay above the five newest finished entries by default, then `(and N older · --all)` unfolds the remaining finished entries. FIRED gives launch age, subject, and leader; a retired task's age and result; or `▸ running <duration>`. Only history stamped as one-off is included, so one-offs retired before that stamp was introduced stay accessible through `loop show` and `loop logs`, not DONE.
- WAKE-UPS folds to a count of tasks that wake agents, their health, and `rimz loop list --all`. With `--all`, the unfold hint is omitted, and owned subscriptions use one line per team or loop owner, with their signals, targets, and worktree count (`in 1 worktree` or `in N worktrees`). Unowned wake-ups keep their real names. A target matching the calling agent is marked `(you)`.

Every table keeps the task name first. A WORKTREE column appears only when a row is checkout-bound, and the caller's checkout is marked `(here)`. A resident task with `--worktree` names its launch destination, not the checkout its guards read; `loop show <destination>` resolves that name too. Rows sort by task name except DONE, which puts running rows first and sorts each group newest first. A dead watcher moves to NEEDS YOU with `watcher gone · will not wake <handle>` and a `loop remove` hint; stronger attention reasons still win. Ended delivery targets are retired by the harness, not diagnosed by list.

Trigger text omits the checkout's matching `path` filter, includes labels and pauses, and puts `for <hold>` and `each worktree` on a continuation line. Watched commands read `on command exit`, not their command preview. On a terminal, the trigger wraps at modifier, condition, and word boundaries down to 24 columns; no cell is clipped. If the other columns plus that floor exceed the terminal width, the table overflows. Piped output keeps only the fixed continuation line.

LAST chooses running, off, latest acting result, latest heard sibling signal, watcher liveness, then `never fired`, in that order. A good acting run reads `✓ 53m ago · 143 in a row`; a failed one reads `✗ failed 2m ago · exit 101`. A sibling signal after a good run does not replace it; with only sibling signals the row reads `never fired · heard ci.passed 7m ago`. An expanded live watch reads `watching 12m`. Delivery streaks, launched-worktree counts, and spend today follow inline. A daily cap is shown even at zero spend. A repo task not enabled here reads `off · repo task, enable here to run`.

A run whose task row is gone keeps a DONE row until it ends, with the real name `loop stop` needs and FIRED `▸ running 3m`. After that, retained one-off history remains in DONE; other rowless runs disappear. Empty rooms say how many tasks exist elsewhere and point to `--all`; with none anywhere, the add hint remains.

`--json` prints one object with a `rooms` array for the same scope (`--all` widens it). Each room has `root`, `open`, `here`, `spend_today_usd`, `tasks`, and an unfolded `retired` array of one-offs (`name`, `at`, serialized run `result`, optional `checkout`). Tasks are never collapsed and always retain real names. Each has `name`, lifecycle `kind` (`routine`, `one_off`, `wake_up`), `section` (`needs_you`, `routine`, `waiting`, `done`, `wake_ups`), `state` (`live`, `off`, `not_enabled`, `paused`, `running`, `waits_for_room`), unwrapped `trigger`, `you`, and spend. Pure-AND conditions with an observed verdict outside fan-out tasks add `condition` with `met`, `total`, and `not_read`. `action` has `kind` (`check`, `start`, `wake`, `workflow`) and `subject`; it is omitted when no definition survives or the action is invalid. An action's `kind` is separate from its task's lifecycle `kind`; a workflow action's `subject` is the workflow name. Other optional fields are `attention` (`checkout_gone`, `strikes`, `failing`, `held`, `blocked`, `invalid`, `parked`, `lost`), `label`, `dir`, `worktree`, `owner` (`kind`: `team`, `loop`; `name`), `next_at`, `running_since`, `last` (`at`, serialized run `result`, `ok`, `streak`), `heard` (`signal`, `at`), `budget_per_day_usd`, and `workflow_runs`, the task's active and parked [workflow runs](./workflow.md#json-shapes). Absent values are omitted, not null; timestamps use the same format as other JSON commands. Retired history is separate from `tasks`; it counts in watch's done fold without adding table rows.

Listing is read-only and never creates a room store. Malformed local task state fails the command; a malformed other room warns with its root and is skipped. A run lock that cannot be read warns on stderr with the task's name. When a room's locks directory cannot be listed, one warning names the root and directory, and its rows show their other state.

### `loop watch`

`loop watch` holds the open room's tasks in a compact glance panel, repainting every second. From a worktree it still shows the whole room, never other rooms. Without an open room it refuses with `rimz start` and `rimz loop list` as alternatives; it also requires a terminal on stdin and stdout. It is the loop pane in the room's `rimzd` tab. `q` or Ctrl-C quits; `--hold` ignores both and hides the quit hint. A room closing mid-watch does not stop repainting: its clocks read `waits for room` until it opens again. The panel prints no lock warnings.

Rows put tasks needing you first, then running tasks, then clocks (due first, then earliest fire), then other tasks, with off, not-enabled, paused, and waiting-for-room tasks last. Names sort within groups and break clock ties. The three columns are `task`, `next`, and `last run`. `next` carries the state or trigger, including `▸ running 3m`, `held: <reason>, 3m`, countdowns, and the list's held-state words. It wraps between words before names or last-run text clip. The next column keeps its widest word; names keep the smaller of the widest name and a quarter of the panel width. Last-run text takes the remaining width and clips with `…`. Below those floors, any cell may clip to stay within the panel. Checkout-gone, strike-disabled, blocked, invalid, parked, and lost-watcher rows put `✗ <reason>` on a separate line under `next`, across the remaining panel width. Failing rows already explain their outcome in `last run`; throttle-held rows explain it in `next`.

`last run` is history only: `✓ <age>`, `✗ failed <age>` with an exit code when known, `heard <signal> <age>` with no acting run, or `never fired`. A first running task without history reads `—`. Sibling signals do not replace a good or failed acting result. Streaks, delivery counts, worktree counts, and spend stay in `loop list`. A consumed one-shot with an active run keeps a named running row and supplies no next fire.

Finished one-offs fold below the table as `done`, counting retained retired history too, with the newest task's name and age. Healthy nonrunning wake-ups always fold as `wake-ups`, with a count and `rimz loop list --all` hint; wake-ups needing you or running retain rows instead. Folds alone print no table header. An empty room reads `` no loop tasks; add one with `rimz loop add …` ``.

The band starts with `loop`, then nonzero `✗ N need you` (`✗ 1 needs you`), `▸ N running`, `next: <task> in <duration>`, `○ N held`, and `q quit`, in that priority order. A throttle-held run counts as needing you, not running. Folded tasks and held or blocked clocks never supply `next:`. Narrow panels drop the band's trailing segments first. Short panels cut whole rows from the end with `+N more`, keeping the header and folds when space permits; at two lines or fewer only the band prints.

### `loop show`

`loop show <word>` resolves an exact task name in the caller's room first, then the other rooms known to `loop list`. A name present in several other rooms prints one `rimz --root <root> loop show <name>` command per room and refuses to choose. Retired names with retained history come next (text only; JSON still requires a task row), then worktree names among routine and one-off tasks, then subscription owners: a team name, `team#channel`, or loop-task name. A unique worktree match starts with `worktree <worktree> → <task>`; several matches print the real task names, triggers, inspection commands, and wake-up count. An owner shows `SUBSCRIPTIONS` with every real name (text only; use `loop list --json` for structured subscription rows). A task found outside the caller's room names its room and the `--root` needed by other commands. Mutating commands and `loop logs` still require exact task names and keep their existing room selection.

A task opens with `<name> · <kind>[ · worktree <worktree>][ · <trigger>] · <state>`, with lifecycle kind `routine`, `one-off`, or `wake-up`. A condition omits the trigger from the header and adds `· k of n met` for a pure conjunction with an observed verdict. Disabled, paused, unarmed, and blocked conditions have no count. The full `when <expr>[, for <hold>]` appears once above the leaf block or fan-out table, in the same normalized form as `loop list` (parentheses only where precedence needs them). Single-checkout leaves follow in expression order. Each satisfied leaf has `✓`; an unsatisfied leaf has `✗` with a reading, or `?` without one. Missing readings always carry a reason: room closed, not checked yet, unsupported forge, failing lookup with last-attempt age, no PR or CI for the branch or checkout, or `not read` for non-forge keys. A leaf under `!` keeps its prefix and negated satisfaction, so a satisfied negated leaf with no reading still shows `✓` and the absence reason. Forge conditions add `forge checked <age>` when the latest probe succeeded. All readings come from disk, never a forge poll. Command watches use `on command exit` in the header and print the full command below, wrapped at whitespace without splitting tokens. Other watches retain their trigger description. An `each-worktree` task instead appends `· each worktree` and shows a `WORKTREES` table without a single-checkout count or condition state.

`loop show <name>` answers whether the task is working, then shows its evidence. While a run holds the task's run lock, its header state is `▸ running 3m · pid <PID> · run <run id>` (the PID and elapsed time when the lock names its holder, the run id once the run has a record). A health verdict follows from the latest conclusive run: its age, its status and duration, and the streak (how many consecutive conclusive runs ended the same way, and the age of the failure or good run that bounds the streak). A run waiting in [the start throttle](#the-start-throttle) has state `held: <reason>, 3m · pid <PID>` instead, or `held: waiting for its turn, 3m` before its first reading. Other runs of the task waiting in other processes, as a fan-out's checkouts do, follow as `· 2 held: <first reason>`, so a running checkout keeps its own state; `loop list` and `loop watch` show the same. A running task adds the `loop stop` command under the verdict. For a fired one-shot whose run is still active, the task row is already gone: `show` opens with `<name> — one-shot fired` and the same running text, then the agent, the `loop stop` command, and any recorded runs as `loop logs` prints them.

Attention lines under the verdict name checkouts ready but blocked by takeover for at least 30 minutes, then failed live launch leaders, without changing the rule's health verdict. For fan-out tasks, only owned checkouts ready now (condition true and hold elapsed) can be blocked; losing ownership or readiness clears the warning. A block's age starts at the oldest refusal in that checkout's uninterrupted refusal run; a newer non-refusal or a recorded launch clears its warning. Ended leaders earn no warning.

A fire recorded as `overlapped` is spent. The run that refused it started before that fire, so the verdict says, in the warning tone, when such fires have had no run since: fires skipped while the active run holds the lock, fires skipped after the last run, or, when no run is active, fires skipped during the last run. A fire refused at another gate (`budget skipped`, `account skipped`, `surplus skipped`, `throttle skipped`) or recorded as `deferred` ran nothing, so it does not clear the warning.

When anything waits under a non-skip overlap policy, a line below the header says `1 trigger waiting · runs when the active run ends`, adding `· absorbed N` when nonzero, or `queue 4/10 · next runs when the active run ends` for a queue. A pending replacement adds `· stopping pid N`. `deferred` rows remain visible in recent history and are not folded into skipped overlaps. `show --json` adds `backlog`: `null` for `skip`, otherwise an object with `policy`, `limit` (queue limit, otherwise `null`), `triggers` (in firing order, each with `arrived_at`, its firing time; `signal`; `absorbed`; and `from_arrived_at`, the firing time of the kept `from`, once a merge has preserved an older one), and `stopping` (an object with `pid`, `started_at`, and `cause`, or `null` when no stop is pending). This does not change `running`, `held`, or `waiting`, which still describe active runs and throttle tickets.

Sibling signals heard without acting are not runs. Below the verdict, `heard N other <family>.* signals since, latest <name> <age>` counts those heard after the newest acting run, or all retained history without one (omitting `since`). The line is absent when the count is zero. With no fires, a delivery task says `wakes <handle>; not fired yet`, without a manual-fire hint. Other clock-triggered tasks suggest `loop fire`; condition, signal, and watch tasks say `no runs recorded`. Watched rows label their timeout `check-in`, since it does not kill the watched command; other nonresident rows keep `timeout`.

The sections below follow in this order. The facts then read `action`, `check` (when present and not check-only), `wakes`, `label` (when set), `verify`, `root`, `dir`, `source`, `timeout`, `will not fire` (for blocked trust), `throttle`, `budget`, `surplus`, `spend`, and `strikes`. Rows without a value are omitted. `action` uses the list's vocabulary, such as `start sweeper` or `check, then start sweeper`, with ` (you)` and the account pin when applicable, then `· pane stays open` and `· stops idle agents in the checkout first` when configured. A check-only task reads `action: run check · <command>`. Resident tasks never print `timeout`.

When signals are declared, `wakes` reads `leader on <signals> · N armed`, counting the room's subscriptions owned by this task. With zero armed subscriptions and no live ledgered leader it ends `· 0 armed, no live leader`; a live leader with none armed reads just `· 0 armed`. Launch memory is not a live roster.

Without a configured limit or a run waiting in the start throttle, there are no throttle or load facts, even for an explicit per-task switch. Pacing and the maximum wait are not limits. With a limit or a held run, an `--agent` task's `THROTTLE` section ends the screen with current readings beside configured limits: `cpu pressure`, `io pressure`, and `memory pressure` (avg10 only), available memory, free disk where the run will write, the active agent count, and, with `max-active-per-task` set, the task's own active agent count. An exempt task prints `throttle: off (starts without taking a turn)` instead of the section when limits are configured. Non-spawning tasks never show the readings section.

| Section | Contents |
| --- | --- |
| `WORKTREES` | Fan-out tasks only: owned checkout count and nonzero blocked, ready, holding, waiting, declined, and launched counts, in that order. Columns are `WORKTREE`, `STATE`, each distinct condition key in expression order (`!`-prefixed when its single term sits under a negation), then `LEADER` and `REASON`, each present only when a rendered row fills it. Rows put blocked checkouts first, then ready, holding, launched with running, waiting-on-you, or failed leaders (newest first), then every waiting checkout. Within other groups, names sort ascending. Blocked means ready now (condition true and hold elapsed), unlaunched, with an uninterrupted takeover refusal run. Blocked rows show the uninterrupted refusal age and name the newest recorded blockers under `REASON`; holding rows show elapsed/hold; waiting and ready rows show whether each leaf, negation included, is satisfied, and its reading (`?` with a cache-only reason, or `not read`, when absent). Launched rows show launch age and ledgered handle with effective status under `LEADER`, adding `· N armed` only when the task declares subscriptions. Term cells stay blank for blocked, declined, and launched rows. Declines fold into a count with judge profiles, the soonest recheck (or re-judging when the guard changes), and the newest decline's checkout, age, and first reason line. Other launches fold into a count with a status breakdown and the newest folded launch's checkout, handle, and age. A folded group adds `→ reasons and leaders: rimz loop show <task> --all`. `--all` unfolds both groups after the waiting rows: declined checkouts by name, then other launches newest first, with ended leaders marked `@handle ended`. Declined rows show the decline age and `judge <profile>: <first reason line>` under `REASON`. Reasons and decline summaries clip to the terminal width. A closed room adds the condition block's note about unavailable CI and PR readings. Readings come from disk, never a forge poll. |
| `LAUNCHES` | Non-fan-out resident launch memory, when nonempty: bare destination names (scope checkout names when no destination is recorded), ledgered leader handles, and relative launch ages, newest first. |
| `LAST RUN` | The same acting run as the verdict, omitted for a clean `launched` result. Newer refusals and guard declines that ran nothing remain in `RECENT RUNS`, not here. The header carries status, age, duration, exit, `manual` for a hand fire, and the signal that fired it. The body retains fire-time condition readings but omits the recorded expression when it equals the current condition. Otherwise it is what `loop logs` prints for that run, except that a passing check prints its last five lines, followed by the `loop logs` command whose first block is this run. A failed check prints every line. |
| `AGENT RUNS` | Check-gated `--agent` and `--wait` tasks only, never for a resident task: how many fires escalated to the action, the total and average cost across every escalated run the log still holds that recorded one, and the five latest attempts, newest first. |
| `RECENT RUNS` | The latest runs, newest first, identical consecutive runs collapsed into one row. Omitted when the only collapsed row is already LAST RUN and carries no skipped overlaps; the repeat count appears once on the verdict. Each uninterrupted run of `takeover blocked` refusals for a checkout folds into one row at its newest refusal's position, with `×N` for that run's refusal count and a note naming the checkout, the newest recorded blocker reason, and `since <age>` for the oldest refusal in that run. A non-refusal record for the same checkout ends the refusal run; older runs keep separate rows and ages. An `overlapped` fire takes no row once a later run is recorded: it joins that run's `NOTE` as a count (`2 fires skipped, run already active`), and the runs on either side of it collapse. Overlapped fires with no later run keep one row at the top. The heading gives the runs these rows stand for out of the runs recorded. `-n, --runs <N>` sets how many rows, default `10`. `MODE` appears when the rows differ in mode, `COST` and `TOKENS` when a row has one. |
| `RUNS` | Workflow tasks only, after the facts: one row per active or parked run the task started, with `RUN`, `CHECKOUT`, `STATE`, `NODE`, `DETAIL` (the [waiting line](./workflow.md#the-waiting-line) or the park reason), and `AGE` since the run entered its node. Omitted when there are none. |
| `SUBSCRIPTIONS` | Non-fan-out tasks only: rows owned by this loop task in its room, one per real task name, without the list's collapsing. Each row shows its checkout, target handle, signal, and last acting run with its age, using the list's LAST wording. Without an acting run it reads `never fired · heard <signal> <age>` for a heard sibling signal, otherwise `never fired`. Omitted when no rows name this task as their owner. |

`loop logs` and `loop show --json` keep one entry per attempted fire, overlapped fires and guard declines included, but not heard sibling signals. A retained trigger has its `deferred` entry and a later served-run entry; only the served run carries a nonzero `absorbed` count. Empty or busy drainers and drained triggers dropped for a paused, disabled, missing, invalid, or no longer retaining task add nothing. `RECENT RUNS` totals and the `AGENT RUNS` denominator also exclude siblings. Both commands use the task row's root and exclude history before a reused wait's arming. Each `launches` record is keyed by scope checkout and carries `at`, `leader`, and, for a named resident worktree, `destination` (the launched checkout path). Older records omit `destination`. The JSON document adds top-level `kind` (`routine`, `one_off`, `wake_up`) and keeps `task`, `entry`, `launches`, `runs`, `running`, `held`, `waiting`, `workflow_runs` (the same rows as `loop list`, empty for a task that starts no workflow), and `worktrees`, and `heard`: `null` without a recorded sibling signal, otherwise an object with `count`, `since` (the acting run's RFC 3339 timestamp, or `null`), `latest` (signal name), and `latest_at` (RFC 3339 timestamp). `running` is `null` while no run holds the lock, otherwise an object with `pid`, `started_at`, and `run_id`, each `null` when the lock names no holder or the run has no record yet. `held` is `null` unless that run is waiting in the start throttle, and then an object with `pid`, `checkout`, `since`, `reason` (`null` before the first reading), and `position`, its 1-based place in the queue. `waiting` lists the task's other runs waiting in the throttle, each an object of the same shape. A record admitted after a hold carries `throttle_wait_ms`. A run lock that cannot be read warns on stderr and prints `null`.

`worktrees` is an unfolded array for fan-out tasks (empty for other tasks). Each row has `checkout` (path), `name`, and `state` (`blocked`, `ready`, `holding`, `declined`, `waiting`, or `launched`). Condition rows include `readings` (key to value or `null`, in expression order) and `short` (keys of leaves not satisfied, negation included). Holding rows add `since`, `hold_ms`, and `held_ms`; ready rows add `since`. A true condition without a matching hold clock starts as holding with `since` set to now and `held_ms` zero when a hold is configured, otherwise ready since now. Blocked rows add `since` and `blocked` (`count`, `since`, `reason`), whose count and age span only the current uninterrupted refusal run. Declined rows add `since` (decline time), `reason`, judge `profile`, and `recheck_at` (RFC 3339) when that judge sets a recheck; `recheck_at` is omitted otherwise. Launched rows add `since` (launch time), `leader`, `leader_status` (`null` when ended), and `armed`. `--all` changes only the human table, not JSON.

In the human table, a condition key used by multiple terms shows a present reading without a truth glyph; single-term keys keep their glyph. If the owned worktree list cannot be read, the table prints one explanatory note instead of rows. If the room's subscription catalog cannot be read, both the screen and `--json` print what they have, with every `armed` count at zero, and then exit nonzero with the error.

A sample is in the [loops guide](../../guide/loops.md#what-a-fire-leaves-behind).

### `loop logs`

`loop logs <name>` prints the full record of the latest fires, oldest first, so the newest ends the output. Each block holds the status, age, trigger (`manual` for a hand fire, `signal <name>` for a signal, `scheduled` otherwise), exit information, check output, error chains, run ids, pane output tails, cost and tokens, and transcript links when recorded. Agent messages render as [agent prose](../cli.md#agent-prose); check output and errors stay literal. While the task is running, the last line is the running text `loop show` opens with; `--failed` leaves it out. Empty history uses the same delivery-target line or clock-only fire hint as `show`; `--failed` instead says `no failed runs recorded`. A removed task whose retained history holds only heard signals points at `--heard` instead.

| Flag | Effect |
| --- | --- |
| `-n, --runs <N>` | How many blocks to print after filtering. Default `10`. |
| `--failed` | Only `failed`, `timed out`, `budget exceeded`, `verify failed`, and `error` runs. |
| `--heard` | Include heard sibling signals in log order, labelled `○ heard`. |

## Rename and remove

Condition runs in `loop logs` include `when: <expression> · held <duration>` and a JSON readings line in expression order (map order if a stored expression cannot be parsed). The record keeps the canonical expression, configured hold, observed held milliseconds, and readings; manual fires do not synthesize readings. Signal runs include `signal: <name>` and, when the signal carried fields, a JSON payload line. Sibling signals appear only with `--heard`; their stored records are unchanged.

`loop rename <name> <new-name>` moves the task to the new key in its store. Its live workflow runs, active or parked, follow it; finished runs keep the name they ran under. The new name must differ and be free. A task with recorded resident launches cannot be renamed because its resident leaders retain the task name. Edit it with `loop add` under the same name, or remove and re-add under a new name (which relaunches its checkouts). A resident task without recorded launches can be renamed. The task re-arms, so an interval task next fires one interval after the rename.

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
