# Loop CLI

`rimz loop` runs work on a clock, on a signal, or while state conditions hold. A task is a name, one trigger, and one action: deliver a prompt to a live agent session (`--wait`), or start a [workflow](./workflow.md) run. A `--workflow` task names a deployed workflow; every other task compiles its row to an inline graph that `rimz workflow show <task>` draws: one agent turn (`--agent`), one shell check (`--check` alone), or an agent turn behind a shell check and model judges, in that order. `rimz loop eject` turns that graph into a deployed workflow. The [loops guide](../../guide/loops.md) teaches when to use each; an alarm an agent sets for itself, after a delay or a watched command, is [`rimz wait`](./wait.md).

A clock task fires while a room for its project is open, from that room's sidebar, or from the optional machine-wide [timer](#timer) when no room is open. A signal task fires from whichever process emits the signal, without the timer or sidebar. [`rimz events emit`](./events.md#emit-a-signal) still requires an existing room.

```sh
rimz loop add morning --agent claude --prompt "summarize what landed on main overnight" --every weekday --at 07:00
rimz loop add watchdog --every 15m --check "cargo test" --check-timeout 10m --judge haiku --judge-prompt "Are these failures flakes or already fixed on main?" --on fail --agent codex --prompt "fix the failing test"
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
| `rimz loop eject <NAME> [<WORKFLOW>]` | Publish a task's compiled graph as a workflow and point the task at it. |
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

Every command takes the [global flags](../cli.md#global-flags). `loop show --json` prints structured task, launch, and run records and the running state. It requires a current task definition. Text `show` also inspects an active run or a finished one-off after its definition is gone; other removed tasks refuse with a `loop logs` hint when retained runs exist.

## Add a task

`rimz loop add <NAME>` takes a name of letters, digits, `-`, and `_`, exactly one action, and exactly one trigger. A new task goes right after the store's last task. If the tasks end the file, or the store has none yet, it goes at the end of the file, below any comment lines there. Commented settings such as `# default-timeout` therefore keep their original section. Adding a name that already exists in the same store replaces that task, except that a machine task whose resolved project root differs is refused, including a hand-edited row with no root or a relative root that does not resolve to this project. Replacement keeps comment lines above the task's header in place; comments above its old child headers stay below the new task, above the next header or at the end of the file.

### Actions

| Action | Flags | Prompt |
| --- | --- | --- |
| Start a fresh agent | `--agent <SPEC>`: a kind, a profile, or a kind with a mode suffix such as `codex-yolo`. Each fire starts a workflow run whose one agent step runs the prompt once in a pane that closes. | Required: `--prompt` or `--prompt-file`. |
| Wake a live agent | `--wait [<ADDRESS>]`: pins one live session now; see [waits and checks](#waits-and-checks). | Optional. |
| Run a command | `--check <CMD>` with no other action. Each fire starts a workflow run of one shell check, which ends `completed` on exit `0` and `failed` otherwise. | None. |
| Start a workflow run | `--workflow <NAME>`: a definition under `~/.rimz/workflows/`. Each fire starts one [workflow](./workflow.md) run ([workflow tasks](./workflow.md#loop-tasks)). | Optional: becomes the run's prompt. |

`--workflow` validates the definition the way `rimz workflow run` does, refusing an undeployed or invalid one with the same errors and printing its warnings, and the name must use only letters, digits, `-`, and `_`. It accepts the triggers with `--match`, `--for`, and `--once`, `--each-worktree`, `--prompt` or `--prompt-file`, `--label`, `--root`, `--project`, `--max-strikes`, `--overlap`, `--overlap-limit`, `--budget-per-day`, `--surplus`, and `--surplus-after`. `--budget-per-day` gates each run on the day's reported run costs plus what the task's live runs reserve (the sum of their agent steps' `budget`), and refuses a run whose own steps' `budget` would cross it; a graph without step budgets is admitted until the day's cost reaches the cap. `--surplus` checks the account of every agent step and agent check in the graph. Every other flag is refused, with the place that owns it in a workflow: `--stay`, `--takeover`, and `--subscribe`, because agent steps launch and stop agents and a check names the signals it waits for; `--check`, `--check-timeout`, the `--judge` flags, `--on`, `--verify`, and `--max-attempts`, because checks are the workflow's `[test.<name>]`, `[script.<name>]`, `[judge.<name>]`, and `[condition.<name>]` nodes; `--throttle` and `--capacity-wait`, which are `admission.throttle` and `admission.give-up` of an `[agent.<name>]` step, and `--timeout`, a field of each node that takes one; `--mode`, `--effort`, `--fast`, `--no-fast`, `--account`, and `--system-prompt-file`, which come from a step's `agent` profile; `--budget`, because each agent step sets its own `budget`; `--worktree`, because a run scopes to the task's checkout (use `--root <worktree>` or `--each-worktree`); `--after-reset` and `window.*` conditions, because a workflow task has no provider window; and, with `--each-worktree`, `--signal`, `--in`, `--at` without `--every`, and `--once`, because fan-out needs `--when`, `--every`, or `--cron`.

A run with an agent step or a delivery keeps its workflow notes folder, `automation-notes/<yyyymmdd>/<HHMMSS>Z-workflow-<task>-<checkout>/`, and the agent's brief names `agent-notes.md` there, for memory across restarts and subagents and a record of changes outside the repository. Set task `notes = false` in `loop.toml` to omit the instruction. A run of checks alone keeps no folder. Retention follows `gc.automation_notes_retention` (default forever), as for every [workflow run](./workflow.md). Run forensics stay in `rimz loop logs` and `rimz workflow show`.

`--check` combined with `--agent` is a guard: the command runs first, then any `--judge`, and the action runs only when every answered stage lands on the outcome `--on` names. `--check` and `--judge` refuse `--wait`; a stored guarded wait still loads and runs, as a workflow run whose guard ends in a message step. A supervised `--agent` task refuses a kind whose hooks are not installed, because a scheduled turn reports completion through them. A fresh agent or `--judge` whose directory is the home directory fails its fire, for every kind: nobody answers an unattended folder-trust prompt there; re-add the task from a project directory.

A fresh-agent or check-only task runs at the project root, wherever it is added from. `--root` names a project: a linked worktree's path, as the cwd or as `--root`, selects that worktree's project, and the receipt then adds `note: runs at the project root <root>, not <worktree>; pass --worktree <prefix> for a fresh worktree per fire`. With `--worktree <prefix>`, the task is stored with `fresh-worktree = "<prefix>"`, and each fire gets a fresh RimZ-owned worktree named `<prefix>-<yyyymmdd-hhmmss>` (the fire's start in UTC), branched from trunk: the guard, its judges, the turn and `--verify` run there, while conditions, gates, launch memory and run history stay at the project root. A name a tree or branch already holds takes `-2`, `-3`, and so on, and the branch takes the tree's name. When trunk cannot be fetched, the tree branches from trunk as last fetched (or the local branch) and `loop logs` prints the reason as a `warning:` line. A fire cuts its tree last, once its gates, the run lock and the task's overlap policy let it start a run, so a fire that overlaps a live run or that a gate stops cuts none, and a tree whose run is refused at start is force-removed as the fire ends. The run then owns the tree, and its report settles it. A run that never reached its agent (a declined guard, a check-only task) has its tree and branch force-removed, untracked files included, with `worktree.removed` announced. A run that reached its agent, whether the launch opened, failed or was refused, runs the [cleanup](./worktree.md#sweep-landed-worktrees), which removes a clean, landed tree; a tree holding work or an open pane stays until you merge or remove it, listed under `KEPT WORKTREES` in [`loop show`](#loop-show). Waits keep the checkout they were armed from ([waits and checks](#waits-and-checks)). A `--workflow` task without `--each-worktree` keeps the checkout it was added from, or the linked worktree its `--root` names, since its runs and its workflow ledger belong to that checkout.

Rows written by earlier versions keep their meaning: `dir` on an agent, check-only or `--stay` row is still that task's guard, condition and history scope, and `worktree` on a non-`--stay` agent row still reuses or creates that one named worktree from `HEAD` for every fire. New adds write neither; `fresh-worktree` replaces both. Each catalog load refuses `fresh-worktree` beside `stay`, `each-worktree`, `wait`, `workflow`, `worktree` or `dir`, naming the field and its fix.

`--agent <SPEC> --stay` compiles to a run whose one `session` step, with no rest check and `then = "@done"`, opens ordinary agents in a new background tab on the checkout's channel and leaves their panes open; the run ends when the prompt leader's session does. It accepts the same layout grammar as `rimz agents`, requires a prompt leader and a prompt, and accepts `--effort` and `--fast` or `--no-fast`. For both supervised and resident tasks, add saves unsupported `--effort` and `--fast` with the direct launch's warning; those agents run without the setting. The task keeps the requested value, and each fire warns again. OpenCode effort saves silently because add does not resolve the profile's final model; fire checks its live variants or warns and launches without effort when no model is set ([details](../../internals/agents/adapter_opencode.md#model-selection)). With `--stay`, `--mode ask` and `--mode yolo` override the layout; `--mode auto` leaves each profile its own mode. On resident and supervised tasks alike, an invalid nonblank saved `mode`, such as a hand-edited `plan` or typo, errors each fire with ``unknown loop mode `<value>`; use auto, ask, or yolo`` and counts as a strike. A missing room is opened detached. Scheduled fires launch only once per task and scope checkout; changing the task under the same name preserves that memory. `loop fire NAME` launches again, and `loop remove NAME` clears the launch records. `loop show NAME` lists launched checkouts, times, and leader handles; `--json` includes those records and run history. Logs identify each launch's checkout and leader.

A resident condition task retries a failed or skipped fire that leaves no launch record after 5 minutes from that checkout's last fire, while the task is enabled, unpaused, and the condition remains true. A recorded launch suppresses further scheduled fires. Failures remain subject to the task's strike limit; budget skips and a blocked takeover add no strike. Strikes are per task, so one checkout that keeps failing disables the task for every checkout.

`--each-worktree` requires either `--stay` and `--when`, or `--workflow` with `--when`, `--every`, or `--cron`: a signal fire reaches no checkout, and a one-shot or `--once` would remove the task after its first checkout. Repeatable `--subscribe <SIGNAL>` and `--takeover` require `--stay`; subscriptions also require the prompt leader's installed, trusted registration hooks. CI and PR subscriptions need `--each-worktree` or `--worktree <name>`, wherever the task is added from; team subscriptions need a team layout. Subscriptions cannot use one-shot triggers (`--in`, `--at` without `--every`, or `--after-reset`), because the task disappears before registration. Agent-family subscriptions need a separately scoped `loop add --wait --signal` task. `--stay` refuses `--account`, `--verify`, `--max-attempts`, `--budget`, `--budget-per-day`, `--surplus`, `--surplus-after`, `--timeout`, `--system-prompt-file`, `--once`, `--wait`, and `--project`.

`--stay --worktree <name>` creates or reuses the named RimZ-owned worktree when the task fires, through the same resolver as `rimz teams <team> -w <name>`. A new team worktree starts from fetched trunk; a non-team layout uses the configured base. An existing worktree is reused as is, never reseeded. Conditions, guards, scope budget gates, run history and launch memory stay at the project root. Without `--worktree` or `--each-worktree`, the resident seats at the project root, wherever it is added from. Deleting the destination does not clear that memory: use `loop fire NAME` to launch again or `loop remove NAME` to clear it. Redefining the task with `loop add NAME` keeps it, except that a run which has not opened its layout yet is canceled and its checkout forgotten. This combination refuses `--each-worktree` and conditions reading unscoped `ci`, `pr`, `pr.queue`, `pr.mergeable`, `pr.review`, or `team.stage`, including nested expressions. Use branch-qualified conditions such as `pr[topic]=merged`, window conditions, or a clock trigger instead. A live cohort in the destination refuses with a strike without delivering the prompt; stop it or use `--takeover`. A closed cohort without takeover refuses with the ordinary resume-or-fresh guidance.

The session step forwards each `--subscribe` signal to the prompt leader as a `Type: SIGNAL` message while its session lives, as a workflow step's [`subscribe`](./workflow.md) does. CI and PR signals match that agent's checkout, not other lanes, and team signals match the cohort launched there. Forwarding ends with the leader's session, and `rimz loop remove NAME` leaves a live run and its forwarding in place.

With `--worktree`, takeover targets that destination, not the scope, and launches a stopped same-layout cohort fresh. A missing destination stops nobody. Without `--takeover`, the layout opens beside whoever is in the checkout. `--takeover` makes the launched layout the checkout's one owner: every agent already in the launch checkout is stopped before the layout opens, and their subscriptions and waits are retired with them. An agent counts when it is live in a pane and its recorded directory is the checkout or anywhere under it (by the path the task launches into, or the physical path where a symlink leads to the checkout), whoever started it: a team member, another loop task's resident, or a solo agent. Agents in another checkout, including a RimZ-owned worktree nested inside this one, are left alone, as are command cells and your own shell. Subagents an agent launched are stopped with it.

Nothing is stopped mid-turn. If any of those agents, or a subagent one of them launched, is working, waiting on you, paused on a recoverable provider limit or a RimZ budget, or compacting, the run's step stops nobody and waits until the checkout is free, as a workflow step with `admission.occupancy = "takeover"` does. A resting agent is stopped whatever it has armed, a failed turn included. A provider pause the card shows as `failed` is stopped too: one auto-continue has run out of retries on, or a rate or spend limit whose account has no exhausted window left after a reset. An agent whose close failed still counts as stopped once its session has ended and its process is gone. Any other agent that cannot be stopped makes the step wait `retry` and try the takeover once more at the run's next advance; a second failure fails the step's launch, and the run ends `failed`.

A team layout cannot replace a different team that still holds the checkout. This check runs after the takeover stops the old agents, so the step can leave them stopped, launch nothing, and end the run `failed`, a strike. The hold remains while an old member is still recorded as live or the old team's board is not at Done. Bring that board to Done and let the stopped members end before retrying, or use a non-team layout.

Each run a task starts appears in `rimz stats --assists` as a `workflow_start` event naming the task and checkout. Older `resident_launch` events still appear there, and `rimz stats --json` still counts them as `resident_launches`.

With `--each-worktree`, each RimZ-owned checkout has its own condition and `--for` hold, even without a resident pane. The project root and unmanaged worktrees are excluded. Already-matching checkouts are eligible when the task is added. On GitHub this includes PRs already merged or closed; on Gitea the checkout's PR must have been observed open while the room ran. Room teardown and reset clear the PR cache, so Gitea loses that observation. This restriction applies to unscoped keys; branch selectors look up PRs in every state from first sight, limited to the newest 500 PRs on Gitea, and a merged PR whose head branch Gitea deleted can read unknown. Launched checkouts stay excluded across condition changes, task replacement, and room teardown. Several eligible checkouts may launch on the same tick. Manual `loop fire NAME` targets the owned checkout containing your current directory and refuses elsewhere; it does not fire every checkout.

### Triggers

| Shape | Flags | Repeats |
| --- | --- | --- |
| One-shot | `--at <TIME>`, or `--in <DUR>` | Fires once, then the task removes itself. |
| After a window reset | `--after-reset 5h\|7d` | Fires once at the next reset of the task provider's window, then the task removes itself; see [after a reset](#after-a-reset). |
| Interval | `--every <DUR>`, measured from the last fire | Yes. |
| Calendar | `--every <DAYS> --at HH:MM` | On each matching day. |
| Raw cron | `--cron "<expression>"` | Per the expression. |
| Poll-until | Stored rows only: `loop add` refuses `--until` | One run per cadence fire, until a run reaches its action or the deadline passes; a fire past the deadline records `expired`. |
| Signal | `--signal <NAME\|FAMILY.*>`, narrowed by `--match` | On every delivering signal, or once with `--once`; see [signals](#signals). |
| Condition | `--when <EXPR>` with optional `--for <DUR>` | Once per continuously true period, except resident retries; `--once` retires after the first fire, and a `--once` fire that left no run behind fires again five minutes later while the condition still reads true. |

The trigger values follow these rules:

| Value | Accepts |
| --- | --- |
| `--at` | Alone, a one-shot time: 24-hour `HH:MM`, `YYYY-MM-DD HH:MM`, or `<weekday> HH:MM` (such as `fri 09:30`). It resolves at add time to the first matching instant after now, so an `HH:MM` at or before the current minute fires tomorrow; a dated time in the past is refused. With `--every`, 24-hour `HH:MM` only. |
| `--every` interval | An integer with `s`, `m`, `h`, or `d`, rounded up to whole minutes. `0` is refused, and an interval cannot take `--at`. |
| `--every` days | `day` (or `daily`), `weekday` (or `weekdays`), `weekend` (or `weekends`), a list such as `mon,wed,fri`, or a range such as `mon-fri`. Requires `--at`. |
| `--cron` | Five whitespace-separated fields (minute, hour, day of month, month, day of week). Each field is `*`, a value, a range `a-b`, a step `*/n` or `a-b/n`, or a comma list of these. Months take `jan` to `dec` and days take `sun` to `sat`, case-insensitive and usable as range ends, and day `7` is Sunday as well as `0`. `@hourly`, `@daily`, `@weekly`, `@monthly`, or `@yearly` stands for the whole expression. When both day fields are other than `*`, a day either one selects fires. `L`, `W`, `?`, `#`, `@reboot`, an out-of-range or reversed value, and an expression whose day of month no selected month has (`0 0 30 2 *`) are refused, naming the token or form. |
| `--in` | A [duration](../cli.md#durations) in `s`, `m`, `h`, or `d`, greater than zero. It resolves at add time to a one-shot instant, rounded up to the next minute. |
| `--until` | Refused with `--until is no longer accepted; fire on a condition with --when, or `rimz loop eject` the task and wait in a workflow check`. A stored row keeps its deadline and still runs. |

`--every 1d` fires a day after the last fire and drifts with it; `--every day --at 07:00` fires at 07:00. Calendar times, `--at`, `--in`, and `--until` resolve in the top-level `timezone` setting, or the system zone when it is unset. A recurring clock task arms the first time a clock sees it and fires on the next occurrence after that, so a late room or a new timer never replays missed fires. A one-shot from `--at` or `--in` instead fires on the first clock that sees it at or past its instant, however late, and its run then reads `fired <DUR> late` in `loop show` and `loop logs` when it started two minutes or more after the instant. It lists as `once at YYYY-MM-DD HH:MM`.

### After a reset

`--after-reset <5h|7d>` fires once at the next reset of that window of the task's provider, the `--agent` or `--wait` kind. Add reads the window from the provider's cache and writes its reset instant to the row as `fire-at`; a window that has not started fires on the next scheduler tick instead. An instant that passed before any clock saw the task, or while it was paused, fires on the first tick it is live. The task gets one recorded attempt: a fire, a gate skip, an overlap, a check skip, or an error all remove it, while `rimz loop fire` leaves it. A fire whose runner never started, or exited before it recorded anything, leaves the task in place and is dispatched again once five minutes pass with no runner holding its lock.

Add refuses what a [window condition](#conditions) refuses, and also a window the provider is not enforcing (use the other span), a window with no reset time, and `--surplus` or `--surplus-after` on the provider's longest window, whose gate is always closed at its reset. `--after-reset` requires `--agent` or `--wait`, conflicts with `--at`, `--every`, `--cron`, `--in`, `--signal`, `--when`, and `--until`, and is refused with `--project`. Listings describe the task as `once at <YYYY-MM-DD HH:MM>` in the configured zone and show that instant as its next fire.

### Conditions

`--when 'team.stage=Done && ci=passed' --for 30m` waits for both readings to stay true for 30 minutes. Evaluation uses the clock tick, not signal history: CI can pass before or after the board reaches Done. The first tick arms without evaluating. A false tick resets the hold and an unknown tick keeps it; a fired standing task without `--stay` re-arms after a false tick. Holds start at the first true observation, never at a board timestamp, and reset when the room's runtime state is removed.

Expressions accept `!`, `&&`, `||`, and parentheses, in that precedence order. A term is `key=value1,value2` (either value) or `key!=value1,value2` (known and none of those values); a window key instead takes `key>=N`, `key<=N`, `key>N`, or `key<N`, with `N` a whole percent from `0` to `100`. A single value works too. Repeating `--when` ANDs the clauses. Whitespace between tokens is ignored; values use letters, digits, `_`, `.`, or `-`. Window percents are the only literals; there are no functions. A window key refuses `=` and `!=`, and for an exact percent `window.5h.left>=40 && window.5h.left<=40` reads the same.

| Key | Values | Reading |
| --- | --- | --- |
| `team.stage`, `team.stage[branch]` | `Done` and stages declared by any team in the effective configuration, case-sensitive | The `.rimz/team/blackboard.md` Stage line of the scoped checkout, or of the local checkout on the selected branch, primary or linked, owned or not. The branch's checkout comes from `git worktree list` rows cached for up to 60 seconds, so a checkout created or switched to the branch within that time can be unread. Unread when no local checkout is on the branch. |
| `ci`, `ci[branch]` | `passed`, `failed`, `pending`, `none` | The room sidebar's last-known CI result, or `none` when the last successful probe found neither PR CI nor branch CI. A live runnable loop condition or an Active workflow run with a forge-reading condition case or a default-scoped `ci`/`pr` wake keeps its scope checkout in the sidebar's probe set, with or without a pane. Removing the task releases its demand; workflow demand lasts until the run leaves Active. Without a sidebar source, including under the external timer, it is unread and the condition holds ([forge freshness](#forge-freshness)). |
| `pr`, `pr[branch]` | `open`, `merged`, `closed`, `none` | The room sidebar's last-known PR state for the checkout or selected branch. Like `ci`, live loop conditions and Active workflow runs with forge-reading cases keep their checkouts probed without panes. `none` when the last successful probe found no PR link, and unread without a sidebar source; branch CI alone does not imply a PR state. A PR in the merge queue, or removed from it, still reads `open`. |
| `pr.queue`, `pr.queue[branch]` | `queued`, `dequeued`, `armed`, `none` | The room sidebar's last-known merge-queue state for the checkout's or selected branch's open PR, GitHub only: `queued` while it is in the queue, `dequeued` once it has left the queue without merging, until it is queued again, re-armed (an auto-merge request enabled after the removal), merged, or closed. A push alone does not clear it. `armed` for an open PR in neither state that has an auto-merge request: one re-armed and waiting to enter the queue, or armed on a repository without a merge queue. `none` for an open PR with none of these, one that is merged or closed, any PR on Gitea, and when the last successful probe found no PR link; unread without a sidebar source. Pair it with `pr`: `pr=open && pr.queue=dequeued`. |
| `pr.mergeable`, `pr.mergeable[branch]` | `mergeable`, `conflicting`, `none` | GitHub's settled mergeability for the open PR's current head. Unread until GitHub settles an answer at that head, when only an answer for an older head is cached, on an open PR without open-PR facts, and without a sidebar source. `none` when the last successful probe found no PR link, or a merged or closed PR. The answer is keyed by head alone: after only the base moves, the answer settled at the same head keeps reading until GitHub settles again. GitHub only. |
| `pr.review`, `pr.review[branch]` | `draft`, `changes_requested`, `required`, `approved`, `none` | The open PR's review state, GitHub only. Draft wins over the review decision. Otherwise `changes_requested`, `required`, and `approved` reflect GitHub's review decision; a null decision reads `none`, and so does a PR the last successful probe found missing, merged, or closed. Unread when never fetched, when the review decision is unavailable on a non-draft PR, on an open PR without open-PR facts, and without a sidebar source. |
| `window.5h.left`, `window.7d.left` | A comparison with a whole percent, `0` to `100` | Percent left in the task provider's 5-hour or 7-day window: `100` minus the cached usage, projected to the tick, so a window whose reset has passed reads `100`. A limit the provider has lifted reads `100`. Unread without a cached reading of that window. The provider is the kind `--agent` or `--wait` resolves to at add, recorded as `provider` on the row. The account is the room's current one for that provider, or the task's `--account`, whose reading is the last one stored for it: a room refreshes only its current account for the provider and the accounts its agents run on. |

Only `team.stage`, `ci`, `pr`, `pr.queue`, `pr.mergeable`, and `pr.review` take an optional `[branch]` selector, such as `pr[feat/x]=merged`. A selector is nonempty and contains no whitespace, `[` or `]`; its spelling is preserved. `team.stage[branch]` reads the board of the checkout on that branch, as the table says. A forge selector reads that branch even without a checkout for it in the room. A live loop condition or an Active workflow run with forge-reading cases keeps its selected branches probed; demand ends when neither needs them. `ci[branch]` prefers CI of an open or merged PR, then the branch head's CI. Forge branch readings are unknown until the sidebar has probed them or when the scope checkout has a detached HEAD, and unread with no supported forge or under the external timer ([forge freshness](#forge-freshness)). These readings emit no signals and are not latched merge events.

A condition is true, false, or unknown. `=` is true when the reading is in the values and `!=` when it is not; over an unread key either term is unknown, and so is a window comparison. `!` keeps unknown unknown, `||` is true when either side is true, and `&&` is false when either side is false, so `ci=passed || pr=merged` is true on passed CI while `pr` is unread, and `!ci=failed` is unknown while CI is unread. `unknown` is not a value: `ci=unknown` refuses, since a term over an unread key is unknown by itself. Match `none` for a successful probe that found nothing. A task fires only on true; unknown keeps a running `--for` hold and the fired latch, and false resets them.

#### Forge freshness

A condition decides on forge readings only while they are fresh. A `ci`, `pr`, `pr.queue`, `pr.mergeable`, or `pr.review` key that is not fresh is unread and its terms are unknown; while the other terms do not settle the condition, it is unknown and holds: it does not fire, and a running `--for` hold, a held judge decline, and a capacity wait keep their clock. Every forge key is unread without a sidebar source (including under the external timer), while the scope checkout has no supported forge (it is never probed, so such a condition holds indefinitely), while the latest probe has failed (the sidebar keeps its last readings, but a condition does not decide on them), and while the last successful probe is older than the task's last fire, or than its first sight before it has fired. A probe at the same instant counts as fresh. On an open PR whose head is known, bare `ci` is also unread when the CI verdict describes another commit than the PR head, or when the checkout's HEAD is definitely not on the PR, the same commit fence [workflow checks](../../guide/workflows.md#wait-on-the-outside-world) use; missing commit data skips only the comparison it prevents. That commit check runs only when the condition would otherwise be true. `loop show`, `loop list`, and `loop explain` apply the same rule and show each unread key's reason: `probed before the last fire or first sight`, `no sidebar source` (`readings paused (room closed)` when the room is closed), `no supported forge for this checkout`, the probe's own reason, or the commits that differ. `loop explain` reads no task, so it has no fire-stamp floor. A condition with no forge keys is unaffected. An idle repository's next probe can take up to five minutes, so a forge condition can hold that long after each fire.

A window condition is checked against the provider's cache at add, which refuses: a check-only task, which has no provider (add `--agent` or `--wait`); a kind that selects an exact managed account at launch (Qwen); a room whose account for the kind does not resolve (`rimz accounts list`); no cached reading of the window, or one whose reset has already passed (open the room's sidebar or run `rimz accounts --refresh`); and a provider without a window of that span.

Scope is the project root, wherever the task is added from; a `--wait` keeps the checkout it was armed from, or the linked worktree its `--root` names, a `--workflow` task without `--each-worktree` keeps the checkout it was added from the same way, and a legacy row's `dir` keeps its directory. For a branch selector, the repository is that scope checkout's origin repository. Unscoped keys keep reading the scope checkout; from the project-root checkout unscoped CI means trunk CI, so read a branch with a selector such as `ci[<branch>]`. `--worktree` chooses the launch destination or wait target, not condition scope. With `--stay`, scope-reading conditions are refused alongside `--worktree` as described under [Actions](#actions). `--when` conflicts with `--at`, `--every`, `--cron`, `--in`, `--signal`, and `--until`. `--for` requires `--when` and a positive duration in `s`, `m`, `h`, or `d`. `--project --when` is not yet supported; add without `--project`.

The receipt adds `trigger: when <expression>, for <duration>`, `scope: <checkout>`, and `now: waiting · team.stage: Review, ci: unknown` (`unknown · ...` when the known readings do not settle it, or `holding 0s/30m` when already true); a window term reads as `now: waiting · window.5h.left: 8`. Without a room, a CI or PR condition also prints `ci and pr readings come from the room's sidebar; the loop timer alone never sees them`.

`--wait --when` pins the live session like other waits. A live subscription with the same target session, canonical expression, hold, project root, and scoped checkout prints `already subscribed as <name>` without rewriting it, even if its prompt, label, or `--once` differs. Identical conditions on different worktrees remain separate waits.

### `loop explain`

`rimz loop explain <EXPR>... [--json] [--root <PATH>] [--provider <KIND>] [--account <NAME>]` evaluates the current readings without adding a task, starting probes, or writing state. Repeat positional clauses to AND them together, just like repeated `--when`. Scope is the project root of the caller's checkout, or of `--root`, as a launching task reads it; a wait armed from a linked worktree reads that worktree instead. Use `--` before a positional value that is exactly a flag, including `-h` or `--help`.

Output shape:

```text
when: <canonical expression>
scope: <checkout>
  [!]<term>   ✓|✗|? <reading or reason>
now: true|false|unknown
```

Each leaf term gets the same line as in `loop show`, in expression order. Exit 0 means true; exit 1 means false or unknown; exit 2 means a refused expression. Refused expressions use the same clause, position, and fix diagnostics as `loop add --when`; mistyped keys, forge values, and configured stages within two edits get a `did you mean` suggestion.

Forge terms read the existing room cache only: explain does not keep a selected branch probed. Outside a room they are unread, with the reason `readings paused (room closed)`, and print the sidebar hint. Window terms use `--provider` and optionally `--account`, or the room's default account for that provider. An unknown `--provider` kind refuses with exit 1. Without `--provider`, window readings are unknown and the text output ends with `window readings need --provider <KIND>; without it they are unknown`.

`--json` prints one object instead of text and omits the human hints. For `ci=passed` outside a room:

```json
{
  "when": "ci=passed",
  "scope": "<checkout>",
  "room_open": false,
  "now": null,
  "checked_at": null,
  "terms": [
    {
      "term": "ci=passed",
      "key": "ci",
      "negated": false,
      "reading": null,
      "met": null,
      "unread": "readings paused (room closed)"
    }
  ]
}
```

`when` is the canonical expression; `scope` is its checkout path. `terms` has one entry per leaf in expression order, including repeated keys. `term` excludes surrounding negation; `negated` is true for an odd number of surrounding `!`. `reading` is a string or null when unread; `unread` is the text explanation for an unread term, otherwise null. `met` is the leaf's truth after negation, null for an unread term. `now` is the condition's truth (true, false, or null for unknown), over the same readings a task decides on, with [unread](#forge-freshness) forge terms unknown.

`checked_at` is the latest probe's RFC 3339 time only if it succeeded, the expression reads forge terms, and the room is open; otherwise it is null, and every forge term is unread. Exit codes stay 0 for true, 1 for false or unknown (with JSON still printed), and 2 for a refused expression (no JSON).

### Flags

| Flag | Applies to | Meaning |
| --- | --- | --- |
| `--label <TEXT>` | all | Short description of 1 to 60 characters after trimming, on one line. Stored as `label`; `loop show` prints it beside the command. |
| `--stay` | `--agent` | Open a staying layout in a background tab, once per scope checkout. |
| `--each-worktree` | `--stay --when`; `--workflow` with `--when`, `--every`, or `--cron` | Evaluate and launch separately per RimZ-owned worktree; refused with `--worktree`. |
| `--subscribe <SIGNAL>` | `--stay` | Forward this signal to the prompt leader while its session lives; repeatable. |
| `--takeover` | `--stay` | Stop the checkout's idle agents before launching; the run waits while one is busy. An agent whose close failed still counts as stopped once its session has ended and its process is gone; any other failed stop is retried once, and a second failure fails the run. |
| `--prompt <TEXT>`, `--prompt-file <PATH>` | `--agent`, `--wait`, `--workflow` | The prompt the action delivers, or the workflow run's prompt. The two conflict. |
| `--check <CMD>` | `--agent`, or alone | Shell command to run; alone it is the action, otherwise the guard. Refused with `--wait` and `--workflow`. |
| `--check-timeout <DUR>` | `--check` | Cap for the shell check; default `--timeout`, else `5m`; a killed check fails. |
| `--judge <PROFILE>` | `--agent` | Profile for a headless judge, resolving to one Claude or Codex agent. May follow `--check`; requires `--on`. |
| `--judge-prompt <TEXT>`, `--judge-prompt-file <PATH>` | `--judge` | Judge question; exactly one is required. |
| `--judge-recheck <DUR>` | `--judge --stay` | Re-ask after a decline; no default. Refused without `--stay`. |
| `--judge-timeout <DUR>` | `--judge` | Cap for the judge, one deadline over both attempts; default `5m`, independent of `--timeout`. |
| `--when <EXPR>` | all | Repeatable state condition; see [conditions](#conditions). |
| `--for <DUR>` | `--when` | Continuous hold before firing. |
| `--match <KEY=VALUE>` | `--signal` | Require a top-level payload field to equal the value; repeatable. See [signals](#signals). |
| `--once` | `--signal`, `--when` | Retire after the first delivering fire. |
| `--on fail\|success\|any` | `--check`, `--judge` | Outcome every stage must land on for the action to run; required with `--judge`, otherwise default `fail`. |
| `--verify <CMD>` | `--agent` without `--stay` | Command that must pass after the turn; a failure re-prompts the same session. |
| `--max-attempts <N>` | `--verify` | Total turns allowed to make `--verify` pass. Default `3`, at least `1`. |
| `--max-strikes <N>` | all | Consecutive failed fires before the task disables itself. Default `3`; `0` turns the gate off. |
| `--timeout <DUR>` | `--agent` without `--stay`, check-only | Cap for the agent turn, each verify command, and the shell check when it has no `--check-timeout`; see [timeouts](#timeouts). |
| `--worktree <NAME>` | `--agent`, `--check`, `--wait` | Fresh worktree prefix for each fire of an `--agent` or check-only task; with `--stay`, create or reuse this named worktree at fire time; with `--wait`, the channel of the address. |
| `--mode auto\|ask\|yolo` | `--agent` | Permission posture for the turn. |
| `--effort <EFFORT>` | `--agent` | Reasoning effort passed to the agent. Add saves with the direct launch's warning if any agent in the spec lacks an effort flag; those agents launch without effort. Stored as the task's requested effort. |
| `--fast`, `--no-fast` | `--agent` | Launch in the provider's [fast mode](../agent-support.md#fast-mode), or request none even where the profile sets `fast`. Add saves with a warning if any agent lacks a fast-mode switch; those agents launch without fast. `--no-fast` is accepted on every kind. Stored as the task's requested `fast = true` or `false`. |
| `--account <NAME>` | `--agent` without `--stay` | Named account of the agent's provider that every fire runs on; see [the account pin](#the-account-pin). |
| `--system-prompt-file <PATH>` | `--agent` without `--stay` | Replace the agent's base system prompt with a file's contents. |
| `--budget <AMOUNT[/day]>` | `--agent` without `--stay` | Dollar cap for each spawned run, such as `5`, `$5`, or `5/day`, in the form [`rimz agents --budget`](./agents.md#shared-launch-params) takes. |
| `--budget-per-day <AMOUNT>` | `--agent` with `--budget`, or `--workflow` | Daily dollar cap for the task; see [budgets](#budgets). |
| `--surplus <RATIO>`, `--surplus-after <DUR>` | `--agent` without `--stay`, `--wait`, `--workflow` | Fire only while the provider window has headroom; see [the surplus gate](#the-surplus-gate). A ratio greater than zero with an optional `x` (`1.5x`), and a duration in `m`, `h`, or `d`. |
| `--throttle on\|off` | `--agent` | `off` skips machine stagger, resource limits, and the capacity check, not caps. Default `on`. See [the start throttle](#the-start-throttle). |
| `--capacity-wait DURATION\|forever` | `--agent` | Positive `s`/`m`/`h`/`d` duration or `forever`; defaults follow [the trigger](#the-start-throttle). |
| `--overlap <POLICY>` | repeating clock, signal, or condition tasks, `--stay` and `--workflow` included | `skip` (default), `coalesce`, `queue`, or `replace`; see [overlapping triggers](#overlapping-triggers). |
| `--overlap-limit <N>` | `--overlap queue` | Maximum waiting triggers, at least `1`, default `10`. |
| `--root <PATH>` | all | Project whose room hosts the task. Default `.`; inside an `empty-dir` agent's directory, the default is the checkout that agent was launched from. |
| `--project` | `--agent`, `--workflow`, check-only | Write the task to the project's `.rimz/config.toml`; see [project tasks](#project-tasks). |

`--mode`, `--effort`, `--fast`, `--no-fast`, `--account`, `--system-prompt-file`, `--budget`, and `--budget-per-day` are refused on `--wait` and check-only tasks.

### The account pin

Without `--account`, each fire takes the room's current account for the agent's provider: its pin, else the trusted project default, else the live machine default, else `default`. Pinning, unpinning, or changing a default the room follows moves the task with the room. `--account <NAME>` holds the task on one [named account](./accounts.md) whatever the room selects: the fire launches on it, and its hooks preflight, dollar caps, surplus gate, and window triggers read it. `--account default` pins the provider's own home.

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
action: launches a fresh claude pane in a fresh worktree `deps-<time>` per fire, branched from trunk
trigger: fires every Mon at 09:00
next fire: 2026-09-07 09:00 (in 2d)
live while a room for /home/you/code/app is open
no room is open there; start one with `rimz start`, or use `rimz loop timer install` to fire without one
```

For `--agent recon --stay --worktree review-merge`, the action instead reads ``leaves staying layout recon in worktree `review-merge` (created from trunk for a team, the configured base otherwise, or reused when the task fires)``. A separate `scope` line says that condition, guards and launch memory stay at the project root. An `--each-worktree` resident reads `action: leaves staying layout <agent> in each matching owned worktree`. With `--takeover`, a line `before launch: stops idle agents in the checkout first (waits while any is busy)` follows, naming ``worktree `review-merge` `` in place of `the checkout` when the task has `--worktree`. Each `--subscribe` adds `subscription: <signal> for the prompt leader`.

An `--after-reset` task replaces the trigger and next-fire lines:

```console
trigger: fires once after the next claude 5h reset
window: claude 5h · 70% left · resets 2026-09-05 14:00
next fire: 2026-09-05 14:00 (in 2h)
```

A window that has not started reads `window: claude 5h · not started · 100% left` and `next fire: the next scheduler tick, since the window has not started`.

A judge or a shell check before an action adds one `guard:` line. The watchdog above reads `guard: runs cargo test (cap 10m), then asks haiku (cap 5m); acts when both fail, a judge with no answer after one retry counting as a fail`.

A `--wait` action reads ``action: waits @planner — pinned to claude session `<id>` now; skipped and removed if that session exits``, a check-only task `action: runs check in <root>` (or ``in a fresh worktree `<prefix>-<time>` per fire, branched from trunk`` with `--worktree`), and a workflow task `action: starts workflow <name>`. A surplus gate adds `gate: [after <DUR> into the <kind> longest window · ]surplus ≥ <ratio>x`, the ratio to one decimal. A launching or check-only task added from a linked worktree without `--worktree` adds the `note: runs at the project root` line described under [actions](#actions).

Replacing a task whose arming, pause, or strike state was set prints `arming: reset` right after the first line, and an `--project` add prints its [trust result](#project-tasks) there too. When no room is open and the timer is active, the last line reads `no room is open there; the loop timer will keep time`. A `--wait` receipt prints a no-room line only for a condition task, as ``no room is open there; start one with `rimz start` ``.

## Where tasks are stored

Each task lives in one of three stores:

| Store | File | Holds | SOURCE |
| --- | --- | --- | --- |
| Machine | `~/.rimz/loop.toml` | `--agent`, `--workflow`, and check-only tasks with a repeating trigger (`--every`, `--cron`, `--signal`, or `--when`, without `--once`), added without `--project`. | `machine` |
| State | `~/.rimz/ws/<workspace-dir>/records/loop-instances.json` | Every `--wait` task, plus one-shots (`--at`, `--in`, `--after-reset`), `--once` tasks, and stored `--until` rows. Rows retire themselves. | `state`, or `team <instance>` for a team binding |
| Project | `<root>/.rimz/config.toml`, `[tasks.<name>]` | Tasks added with `--project`. | `project`, or `project · untrusted` / `project · stale` while trust is missing |

Adding a state task requires an existing room: run `rimz start` in its root first, or pass `--root <room>`. Machine and project definitions can be added without a room and create no room state.

A machine task takes precedence over a same-named state task only within the same root. A machine task rooted elsewhere neither hides the room's state task nor is removed when you add that state task. `loop show`, `remove`, `rename`, `pause`, `fire`, and `stop` in that room address its state task; the machine task remains reachable from its own room or outside any room.

Adding a standing machine task also requires that an existing machine task of that name have the same resolved project root. A cross-project collision refuses without changing either task, names the stored root, and suggests another name or `rimz loop remove <NAME>` from the owning project. Re-adding from that project, or with `--root <owning-project>`, still replaces its task. A machine row with a missing, empty, or relative root is skipped at load with a warning and does not run; `~` and `~/` expand against `HOME` before the absolute-path check. Adding over it refuses when its resolved root differs too; run `rimz loop remove <NAME>` from a fresh directory outside any repository or RimZ project before reusing its name, so it cannot select a same-named room or project task instead. Rename and remove still reach a skipped row by name, and rename preserves its root as written.

`loop list`, `enable --all`, and `disable --all` cover machine tasks plus the state and project tasks of the project resolved from the current directory or `--root`. Enable, disable, pause, and strike state belong to this machine: machine-wide for machine tasks, per project for state and project tasks.

A machine or state task that an agent adds records who booked it, as `booked-by`: the calling agent, then each agent that started the one before it (its launcher, or a subagent's parent), nearest first, each as `kind` and `agent_id` (the launch id, else the session id). When an agent on that chain was started by a loop task's fire, the chain that task carried when it fired ends it, even after the task was consumed by that fire, removed, or added again under the same name; every agent the fire started carries it, a team's members included. The chain also ends at an agent the room's rows no longer hold, at an id it already holds, and at 16 agents. A task added from a user shell, by a caller the task's room does not know (an explicit `--root` in another room included), with `--wait`, or with `--project` records none, and recording it never changes whether or where the add writes. Re-adding a name replaces the record, and `rimz loop show` prints the booker. `booked-by` is machine state: a project task that carries it is refused.

### Project tasks

`--project` writes a task that ships with the repository. It needs `--every`, `--cron`, or `--signal`, and refuses `--wait`, `--until`, `--once`, `--when`, and `--after-reset`. Hand-written project tasks also reject `when` and `for`. `--root` must lie inside the project of the current directory. A project task stores no `root` or `dir` and always runs at the project root.

A project task runs unattended only when both hold:

- The project is trusted on this machine ([project trust](./hooks-trust.md#project-trust)); project tasks enter the trust hash.
- The task is enabled here with `rimz loop enable <name>`. A task pulled from a repository starts disabled; `loop add --project` enables it on the machine that ran the add.

A trusted project task wins over a machine task of the same name. While the project is untrusted, the project task does not fire and the machine task keeps running.

`add --project`, and `remove`, `rename`, or `eject` of a project task, change the trust surface. What they print depends on trust before the write:

| Trust before | Result |
| --- | --- |
| `trusted`, or no project config | Trust is granted again for the new config. `add` prints `trust: granted — task enabled and ready to fire`; `remove`, `rename`, and `eject` print `trust: kept`. |
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

A segment whose field the payload lacks is left out with its separator, so a branch with no pull request reads `ci.failed on check-multi @88824c2`. A commit prints its first seven characters. `@<head>` is the commit the CI verdict is about, not an unpushed local HEAD; it is left out when the forge names no commit. Any other name in a built-in family prints its family's leading segment alone. `pr.dequeued` adds one line, `queue checks: <url>`, since nothing else names the queue commit.

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

`--wait @<handle>` resolves the address at add time over the same live roster that `rimz wait` uses and pins that exact session id. A named address cannot target a session shadowed by its pane's current owner, such as the old session left by a Claude `/clear`. The task keeps the handle a `rimz wait` receipt gives that agent. `--wait @me` and a bare `--wait` pin the calling agent, identified from its launch environment or process ancestry; a user shell must name a live target. A session that has not registered yet is refused. When the pinned session ends, is lost, or is stopped, its tasks retire; a fire that finds it gone records `target gone` and removes the task.

A `--wait` delivery arrives from `@rimz` and parks until the target's current turn ends. A signal fire carries `Type: SIGNAL`; a clock fire carries `Type: WAIT` and opens `scheduled wait` then `fired [<task>]`. The [message header](./message.md#the-message-header) lists every type. Waits armed by `rimz wait` interrupt instead ([the delivered message](./wait.md#the-delivered-message)).

`--check` runs through plain `sh` at the project root, or in the fire's fresh worktree for a `--worktree` task, and a legacy row's `dir` keeps its directory. `loop add` refuses `--check` and `--judge` beside `--wait`, and refuses `--until`: fire on a condition with `--when`, deliver from a workflow message step, or [`rimz loop eject`](#eject-a-task) the task and wait in a workflow check. A guarded `--wait` or an `--until` row stored by an earlier version still loads and runs; a stored guarded wait's check runs at the root of the checkout it was armed from, a linked worktree included. It runs before the judges, which run in file order, then the action. The first answered stage that misses `--on` stops the chain:

| `--on` | Fires the action on |
| --- | --- |
| `fail` (default without a judge) | Each shell result is non-zero, death by signal, or a timeout; each judge verdict is negative. |
| `success` | The shell exits `0`; each judge verdict is positive. |
| `any` | Every outcome. |

When the guard fires, the agent's launch brief names the check and its answer, and the check's output stays in the run's `work/<seq>.check.log`. A check that does not fire the action ends the run `completed`. A shell check that times out ends the run `failed`.

Plain `sh` reports a pipeline's last command: `cargo test | tail` can pass when the tests failed. Pipe nothing; RimZ caps the captured output tail itself.

An `--agent` task, every fire included, and a scheduled check-only task open the root's room before the run starts, so a check that reads room state finds one. A stored guarded `--wait` needs the room its target lives in.

`--verify <CMD>` runs after an `--agent` turn and re-prompts the same session with the failure until the command passes or `--max-attempts` turns are spent, then ends the run `failed`; a verify command that times out ends it `failed` too. `--wait` and check-only tasks refuse it, because they have no session to re-prompt. It compiles to a workflow `session` step with `rest-check = ["verify"]` and `then = "verify"`, and its shell check.

`--judge` requires `--agent`, explicit `--on`, and exactly one of `--judge-prompt` and `--judge-prompt-file`. The prompt flags, `--judge-recheck`, and `--judge-timeout` require `--judge`; `--judge-timeout` without it names `--check-timeout` as the shell cap. `--judge-recheck` also requires `--stay`. Layouts with multiple cells and kinds other than Claude or Codex are refused at add. Judges are stored as a [judge table or ordered array](../../guide/configuration.md#looptoml-scheduled-turns); a judge table under `check`, the form an older RimZ wrote, refuses: delete the task from the file and re-add it with `rimz loop add`. A judge cannot be a check-only action. `--check-timeout` requires `--check`, and `--on` requires `--check` or `--judge`.

Both stages guard `--stay` launches. A `--stay` task does not end its run on a decline; it waits by the bound of the stage that declined, 5 minutes after a shell check and the judge's own `--judge-recheck` after a judge, and then runs its guard again. With `--when`, the run also starts over once the condition turns false and true again, and a judge with no recheck waits for that alone. Without `--when`, a judge with no recheck holds the run untimed: later fires on that checkout ask no judge until a manual `loop fire`, a redefinition, or `loop remove` ends the hold, and the held run does not run the shell again. A recheck of `0`, with or without `--when`, ends the run, and the next fire asks again. The live run is the hold, so no further run starts for that checkout meanwhile; a run that fails, or ends on its own without launching, leaves no launch record ([launch memory](#launch-memory)). Any other task's decline ends its run as `completed`. Manual `loop fire` starts a fresh run; on a checkout whose run holds a judge decline with no recheck, it cancels that run first.

Each judge is a [workflow judge](./workflow.md): it runs headlessly in the run's checkout, with the profile's isolation. It has no pane or user to question, and is instructed to change nothing. This is an instruction, not an enforced read-only permission mode. Its final answer is one JSON object with `outcome` and `reason`: `outcome` is `pass` or `fail`, and `--on` maps onto it (`fail` fires on `fail`, `success` on `pass`, `any` on both). A missing or invalid answer, or a failed process, runs the judge once more. A judge with no answer after that, or one that times out, continues the chain to the next judge or the action, a route `rimz workflow show <task>` labels as a default. Guard verdicts never strike: only the run's own result does. An unreadable judge prompt file fails the fire with `error` before its run starts.

`--judge-timeout` caps only the judge, default `5m`; the task's `--timeout` never caps a judge. Relative judge prompt files resolve beside `loop.toml` for machine and instance tasks, or beside `.rimz/config.toml` for project tasks, and are read when the fire starts its run. Each judge's launch note points it at the run's history, which holds the shell check's log and every earlier judge's answer. `rimz workflow show <run> --history` and `loop logs` show every stage's verdict, reason, and cost.

Every stage runs inside the fire's workflow run, after `loop run` or `loop fire` has returned. `rimz loop stop` cancels the run; completed evidence remains in its journal.

Every judge decline in a run a task started appears in `rimz stats --assists` and increments `judge_declines` in `rimz stats --json`, including ordinary tasks.

## Budgets, gates, and strikes

A fire passes its gates in this order before its run starts: scope and arming (account, fleet and provider budget caps), the deadline and launch memory, the task's run lock, the task's daily budget and the surplus gate, then the overlap rule. Their refusals spend nothing and open no room. Each agent step passes the room, account and quota caps again when it launches, and each judge the fleet and account daily caps, the account's park, its bound provider quota, and the task's surplus gate as the row had it when the run started; a judge they refuse never spawns, and the run records `budget skipped`. A gate skip never counts as a strike.

### Budgets

`--budget <AMOUNT>` caps each spawned run; a run that overruns it records `budget exceeded`. `--budget-per-day <AMOUNT>` sums the task's run costs in the configured local day and skips a fire when the remaining amount cannot fund the next run's `--budget`. It requires `--budget` for that reason: the gate reserves one run at the per-run cap before it lets a fire through, and counts the same reservation for each of the task's live runs. A `--workflow` task's gate reserves the next run's agent-step budgets instead, and counts what the task's live runs reserve; see [actions](#actions).

A fire records `budget skipped` from any of three sources:

| Source | Applies to |
| --- | --- |
| The task's own `--budget-per-day` | `--agent`, `--workflow` |
| A room's daily cap or account's day or month cap ([what a cap blocks](./budget.md#what-a-cap-blocks)) | `--agent`, `--wait` |
| An exhausted Qwen 5-hour, 7-day, or 30-day quota window on the task's account | `--agent` |

The room, account, and quota caps are checked again when the agent launches and between verify attempts. A Qwen skip names the Alibaba region, window, usage, and reset.

`loop list` includes today's spend in LAST, as `$1.20 / $15 today` when a daily cap is set. `loop show` prints the spend on its `spend` line.

### The surplus gate

`--surplus <RATIO>` fires an `--agent` or `--wait` action only while the provider's longest budget window has that much headroom: the share of budget left divided by the share of time left. `1.0x` is the sustainable pace, and `--surplus 1.5x` needs half again as much. `--surplus-after <DUR>` (`m`, `h`, or `d`) first requires that much of the window to have elapsed; alone it implies `--surplus 1.0x`.

A missing, incomplete, expired, or not-started window keeps the gate closed (which is why `--after-reset` on the longest window refuses a surplus gate), so an API-key account without window readings always skips. A managed Qwen `--agent` task paces against its account's 7-day window, and a Qwen `--wait` task always skips. A closed gate records `surplus skipped`, and `loop show` prints the gate and the recorded reason. A run the fire started keeps the gate it started with: its judges read the thresholds the row had then, so editing or removing the row changes the next fire, not a run in flight. The headroom model with a worked example is [budgets → the surplus gate](../../guide/budget.md#the-surplus-gate).

### The start throttle

Automated agent starts take turns through one capacity door shared by every room of this RimZ home, so a burst of due tasks starts its agents one after another. A loop fire takes no turn itself: each agent step of its workflow run takes its turn when it launches, under the step's [admission rules](./workflow.md#run-a-workflow). The task's `throttle` and `capacity-wait` keys become its agent step's `admission.throttle` and `admission.give-up`. A `--wait` delivery and a check-only task never wait in the machine queue. An agent task with `--throttle off` skips the machine turn, stagger, pressure, memory, disk, and capacity check, but still waits for configured caps.

The machine-only `[throttle]` table in `~/.rimz/config.toml` (`throttle.*` to `rimz config set`) sets what a start waits for. A project `[throttle]` warns and is ignored:

| Key | Default | A start waits while |
| --- | --- | --- |
| `max-parallel-agents` | unset | The incoming group would exceed this positive cap on all open agents across every known workspace of this home. |
| `max-parallel-automation` | unset | The incoming group would exceed this positive cap on open agents started by loop tasks or workflows, including their RimZ-launched children and peers. |
| `stagger` | unset | This positive minimum gap since the last machine-gated launch has not elapsed. `off` starts neither wait for nor restart it; a `--skip-capacity` start passes it and restarts it. |
| `check` | unset | This shell command answers no capacity (exit `75`) or errors. Exit `0` admits; every other exit, timeout, signal, or failure to start holds. |
| `check-timeout` | `30s` | Maximum time for one capacity check; a timeout is an error, not permission to start. |
| `cpu-pressure`, `io-pressure`, `memory-pressure` | unset | Either the 10-second or the 60-second pressure-stall average of that resource is at or above this percent (`1` to `100`). |
| `min-memory` | unset | Available memory is below this size (`8GB`). |
| `min-disk` | unset | Free disk where the run will write is below this size: for a `--worktree` task, the filesystem its new worktree goes on (`[agents.worktree] dir`); otherwise the launch checkout. |

`stagger` and `check-timeout` take positive `s`, `m`, or `h` durations. There is no default stagger. An open agent counts regardless of turn state, including waiting, idle, paused, hibernated, compacting, or an errored last turn; ended or dead agents, failed launches, and provider-built-in subagents do not. Agents you start yourself count toward `max-parallel-agents` only, unless started by a loop or workflow or from an automated agent's pane. Loop and workflow starts wait whatever started them; a [`--skip-capacity`](./workflow.md#run-a-workflow) run launches past a full cap and still counts as automated. An open agent of a finished workflow run keeps its slot. Every cap includes other admitted-but-unlaunched starts, and the whole incoming group must fit. An automated start larger than a cap fails with the key and the fix instead of waiting forever. Cap hold reasons name in-flight agents separately (`1 more starting`) and multi-agent demand (`this start opens 2`).

Workflow and step limits apply first, then the machine caps. Machine-gated starts enter the machine FIFO before checking the caps, then check stagger, pressure, memory, disk, and the capacity script. A waiting capacity-check hold lets starts of disjoint provider kinds pass. No other machine limit reorders starts, and a start holding a shared in-flight scope is never passed. An `off` start takes no machine FIFO place and may take a freed cap slot ahead of a queued start. Without caps or workflow/step limits it takes no ticket. Machine-gated starts always take a turn, even with no `[throttle]` configured.

Cap holds read `12 of 128 agents open (@a, +11)` or `32 of 32 automated agents open (@a, +31)`. Machine stagger reads `next start in 12s`. An unreadable agent store reads `cannot count open agents: <detail>`. Handle lists contain at most two names followed by `+N` for any remaining count.

A start queued behind a failing check, disk-floor breach, or unreadable agent count carries the first blocking waiting start's needs-a-fix cause and reason, even when other barriers or a held turn are ahead. The blocker may belong to another room. Other provider-only queues carry their provider hold reason; remaining queues show `queued: N starts ahead`. Inheriting a needs-a-fix cause does not let a start bypass a held turn or mixed queue barriers.

The task key `capacity-wait` (`rimz loop add --capacity-wait`) becomes the agent step's `admission.give-up`: a positive duration in `s`, `m`, `h`, or `d`, or `forever`. Condition, signal, watch, trigger-less, and absolute one-shot agent tasks default to forever. Repeating clocks default to giving up one second before the fire's next occurrence, an instant fixed when the fire starts its run, so the guards that run before the step do not move it. A task's deadline still applies. A step that gives up ends its run `failed`, which the task records as `throttle skipped`, never a strike. Workflow tasks refuse both `throttle` and `capacity-wait`; configure their agent steps instead.

The capacity check runs through `sh -c` in the task's run directory (`dir` or root), or the selected checkout for an `each-worktree` fire, after the queue and host limits clear, holding the turn but not the queue lock. For a task with `worktree` or `fresh-worktree`, the check runs in the task's run directory, not in that worktree. `RIMZ_WORKTREE_PATH` names the check's directory. It receives the room identity pin and `RIMZ_START_KIND`, `RIMZ_START_ACCOUNT` (empty when unresolved), `RIMZ_START_AGENTS`, and `RIMZ_START_TASK`. For a workflow run no loop task started, such as one from `rimz workflow run`, `RIMZ_START_TASK` and `RIMZ_LOOP_TASK` are empty and the check runs in the run's checkout. A layout runs one check per distinct provider-kind/account group, with that group's agent count; every group must answer capacity. The remaining wait budget caps each check, and reaching that bound gives up without caching an error. Process-group members holding the output pipes after the check exits are killed at its timeout. No-capacity and error answers are reused for 60 seconds per kind/account, except that a blocking fire reloading a changed `check` or `check-timeout` checks fresh when it next reaches that gate; capacity is checked fresh for every admission. Hold reasons name the kind and account; errors include the exit status or timeout and the first stderr line. While a check is configured, `rimz doctor` reports each group's latest error with a fix naming `[throttle] check`. The check is machine policy, not a project trust grant.


`loop list` puts each self-clearing held start in HELD and counts them in the task's NEXT; a hold needing a fix instead puts the task in NEEDS YOU. `loop show` shows each checkout's wait as `held <age>[, gives up <clock>] · <reason>`. Give-up clocks follow the [workflow clock](./workflow.md#the-waiting-line), including the day when it is not today. Their JSON has `capacity_wait: {kinds, since, gives_up_at, reason, hold?}`, with `since` the first capacity hold and `gives_up_at: null` for forever; list retains the oldest wait among the task's active workflow runs. `show` puts it on each worktree row, or at the top level for a single-checkout task. An absent wait is `null`. Blocking queue tickets keep the existing `held` JSON shape, with optional `hold` added.

A fire never waits in the throttle: every fire but an unguarded `--wait` starts a workflow run, and each agent step takes its start turn when it launches, as a [`rimz workflow run`](./workflow.md) step does. A `rimz loop fire` run waits at each hold as a scheduled one does, and the fire's receipt adds the held line `rimz workflow run` prints. A fire admitted after a hold records how long it waited, shown as `queued 3m` in its `NOTE`. `loop stop` ends a held run like any other and leaves one `canceled` record.

The pressure and memory readings are Linux's. Pressure reads `/sys/fs/cgroup/<resource>.pressure` at the cgroup namespace root first, falling back per resource to `/proc/pressure/<resource>` when the cgroup file cannot be read or parsed; memory reads `/proc/meminfo`. A configured limit this machine cannot read warns at `rimz start`, naming the key and the fix; the room opens, but a machine-gated fire records `error` rather than ignoring the limit. For pressure the fix names `throttle.cpu-pressure` (or the other resource), removing the key from config.toml or enabling PSI with `psi=1`; memory names `throttle.min-memory` and removing it from config.toml. A workspace whose agents cannot be counted holds a capped start, with the workspace named in the reason. An unreadable queue file errors the fire; a malformed ticket is removed with a warning so other tasks can continue.

What pressure means as a number depends on the machine. The cgroup source describes the machine/container's namespace, not just the launching process's scope; where that source is unavailable, a container's `/proc/pressure` fallback may describe its host instead. Before choosing a limit, watch the `load` or `THROTTLE` readings in `rimz loop show <task>` while the machine works as usual, and set the limit above them.

### Strikes

After `--max-strikes` consecutive strikes (default `3`), the task disables itself, the process that recorded the last strike prints ``loop `<name>`: disabled after N consecutive failed fires; enable with `rimz loop enable <name>` ``, `loop list` shows `disabled after N strikes` under NEEDS YOU, and `loop_disabled` notification handlers fire. `rimz loop enable` clears the count.

| Run result | Effect on strikes |
| --- | --- |
| `failed`, `timed out`, `error`, `verify failed`, `budget exceeded` | Strike. |
| `completed`, `delivered` | Resets the count. |
| `launched` without a run id, in older history | Resets the count. |
| `launched` with a run id | Neutral: the run's own outcome decides once it ends. |
| A workflow run's outcome, `completed`, `failed`, or `canceled` | As the same result above; `canceled` is neutral. Each run counts once, however often its outcome is reported. |
| `skipped`, whatever its guard answered, or a sibling signal | Neutral. |
| `overlapped` by a fire whose scope already has an active or parked run, or `skipped` in older history (`workflow_skipped` in JSON); condition tasks with a recorded launch are silent instead | Neutral. |
| `budget skipped`, `surplus skipped`, `account skipped`, `throttle skipped`, `capacity held`, `takeover blocked`, `overlapped`, `deferred`, `canceled`, `replaced`, `expired`, `target gone` | Neutral. |
| `start failed`: the fire left no run behind, either the runner was never spawned, or it exited before it could record anything. An interval, calendar, or cron occurrence is spent without a run; a one-shot (`--in`, `--at`, `--after-reset`) stays and is dispatched again five minutes later, as is a `--once` condition (and `rimz wait --when`) that still reads true | Neutral. Shown as a failure, but a fire that ran nothing never auto-disables the task. |

### Timeouts

| What | Cap |
| --- | --- |
| `--check` | `--check-timeout`, else the task's `--timeout`, else 5 minutes. A killed check counts as a failure. |
| Each judge | Its own `--judge-timeout` or table `timeout`, else 5 minutes, independent of `--timeout`: one deadline that also covers the retry, and a timeout is not retried. No answer continues the chain. |
| `--agent` turn and each verify command, scheduled or under `loop fire` | The task's `--timeout`, else `loop.default-timeout` (default `2h`, set with `rimz config set loop.default-timeout 3h`) for the turn and 5 minutes for a verify command. |
| `--stay` launch | None. |

`--timeout`, `--check-timeout`, and `--judge-timeout` take `s`, `m`, `h`, or `d`, and must be greater than zero. `loop.default-timeout` is refused the same way. Resident `--stay` tasks accept `--check-timeout` and `--judge-timeout`, not `--timeout`.

## Enable, disable, and pause

`loop enable <name>...` arms tasks on this machine and clears disables, pauses, and strike counts. `loop disable <name>...` holds them until the next enable. Both resolve every name before changing any task: an unknown name refuses the command without changing any task. Both take `--all` instead of names, covering machine tasks plus the state and project tasks of the current project. Each task prints one line: ``loop `<name>`: enabled`` (with `· next in 2h` when a clock fire is due), ``loop `<name>`: already enabled``, or ``loop `<name>`: disabled``.

`loop pause <name> --for <DUR>` holds a task for a bounded time (`s`, `m`, `h`, or `d`, greater than zero) and prints ``loop `<name>`: paused; resumes in 2h (Sat 14:00)``, the clock in the configured zone. A disabled task refuses a pause; `disable` owns the indefinite hold.

All three write machine-local state and never edit a task definition or a trust-hashed project config. Enabling, or reaching the end of a pause, starts the schedule fresh from that moment, so fires missed while held never replay.

## Fire and stop a run

`loop fire <name>` fires the task now, with the scheduled runner's gates except launch memory, and records the fire. It runs a disabled or paused task anyway, after saying so, and leaves one-shots and subscriptions in place. A false condition prints `condition not met (k of n); running anyway, task stays armed`, and an unknown one `condition unknown (k of n); running anyway, task stays armed`; expressions without a conjunction count omit `(k of n)`. This cache-only observation uses the selected checkout for fan-out tasks and never changes arming or records scheduled condition evidence. Met conditions and non-condition tasks add no notice. A fire that starts a run prints `launched` and the run's `rimz workflow show` command, then returns; for a fresh-agent task whose run has not ended, it adds `the agent's pane closes after its turn; fire with --keep to leave it open`; the check, judges, and agent run inside that run, whose agent opens in a background tab as a scheduled fire's does. When the run's first advance holds it, the receipt ends with an indented held line, such as `  build held 0s · claude: no capacity (your check)`. `--keep` sets the agent step's `keep` on every agent task, a `stay` task's session included, so the step's agents stay open outside the run when its attempt ends. A task that names a `workflow` refuses `--keep` with ``workflow task `<name>` cannot use --keep; set `keep = true` on the workflow's `[agent.<name>]` ``, since its definition decides what each step keeps.

For an untrusted or stale project task on a terminal, `fire` asks `grant trust and fire?` and refuses if declined; without a terminal it refuses. A scheduled fire of such a task is always refused.

| `loop fire` exit | When |
| --- | --- |
| `0` | The fire started a run, a gate or overlap skipped it, or a `--wait` delivered. The run's own outcome reaches `loop show` and `loop logs` once it ends. |
| `130` | Ctrl-C interrupted a watched command's check. |
| `1` | An error, or a trust refusal. |

A task whose previous run is still active never starts another concurrently. By default it records `overlapped`; retaining policies record `deferred` unless the queue is full. `loop stop <name>` cancels each live workflow run the task started, as `rimz workflow cancel` does, printing ``loop `<name>`: stopped · run <id>`` for each, and exits `1` naming every run it could not cancel, after trying them all. For a run still on the older run lock, it marks a linked supervised run canceled, waits up to 5 seconds for the overlap lock, sends SIGTERM to a remaining holder, and waits again. A check-only run has no supervised agent to cancel, so it skips the initial wait and receives SIGTERM immediately; the post-SIGTERM wait is unchanged. It prints ``loop `<name>`: stopped`` (with the run id, and `· SIGTERM` when it signaled) or `no active run`, and exits `0`. When the run stopped but its pane was not confirmed closed, it prints the `stopped` line and then exits `1` naming the pane and the command that closes it, `rimz agents stop <run-id>`; a second `loop stop` finds no active run and leaves the pane open. When the lock is still held it exits `1` with the holder's PID and the lock path; SIGKILL is left to you. A fired one-shot (`--at`, `--in`, `--after-reset`) gives up its task row when its run starts; while that run is active, `stop`, `show`, and `logs` still find it by name from inside its project, and `list` and `watch` there keep a row for it.

During a check, the runner forwards SIGTERM to the check's process group and keeps the lock until the check exits, bounded only by its own timeout. The stopped run records one `canceled` row. If triggers were waiting, an extra stop receipt line reports their count, says the next starts now, and names `rimz loop pause <name>` to drop them.

### Launch memory

A remembering task starts at most one run per checkout from its scheduled fires: a `stay` task, a `workflow` task with `when`, and any row that sets `once-per-checkout = true`, whatever its trigger; `once-per-checkout = false` turns the memory off. A scheduled fire records the checkout when it creates the run, the live run keeps the checkout taken, and the way the run ends decides whether the record stays:

| The run | Remembered after it ends |
| --- | --- |
| Acted: reached an agent or message step | Yes |
| Canceled, acted or not | Yes |
| Failed | No |
| Ended on its own without acting | No |

A checkout that is not remembered can start a run again at the task's next scheduled fire. `loop remove` clears the task's memory. A redefinition (`loop add` under the same name) cancels the task's runs that have not acted and forgets their checkouts, while a checkout whose run acted stays remembered; `loop rename` moves the memory. A manual `loop fire` ignores the memory and replaces the checkout's entry once its own run starts.

### Overlapping triggers

Every fire but an unguarded `--wait` starts a workflow run, and a task holds one live run per checkout. `--overlap` chooses what a scheduled trigger does when it passes the scope gates and finds the task's run still live, or another fire of the task admitting:

| Policy | Busy trigger |
| --- | --- |
| `skip` (unset) | Records `overlapped`, naming the live run, and is spent. |
| `coalesce` | Records `deferred` and keeps one waiting trigger, merging earlier waiting triggers into the newest. |
| `queue` | Records `deferred` and waits in firing order. At `--overlap-limit` (default `10`), records `overlapped` with `queue full (N/N), skipped`. |
| `replace` | Records `deferred`, retains the newest waiting trigger, and cancels the live run. The canceled run records `canceled`, which is strike-neutral. |

Waiting triggers survive runner crashes. They run once the live run's report lands, with a scheduler backstop that drains them on its tick. Each reruns arming, trust, scope caps, deadline, check, and throttle at its own start time. Paused or disabled tasks drop their waiting triggers without another history row; another gate's refusal consumes one trigger and records that gate's result. Removing or redefining a task clears its waiting triggers, and renaming moves them. If hand-editing its file removes a task, makes its action invalid, or leaves it without a retaining `overlap` (unset, `skip`, or a shape that refuses the setting), the next drain drops its waiting triggers without another history row. `loop stop` does not clear waiting triggers; pause or disable the task to prevent them running.

Replace cancels the live run as `rimz workflow cancel` does, and starts the newest trigger only once that run is terminal, so a cancel that returns before the run ends never overlaps two runs. Every run a replace cancels records `canceled`, which never strikes, so triggers that keep arriving before a run finishes cannot disable the task. Use `--overlap coalesce` to let each run finish. A cancel that cannot end the run leaves the newest trigger waiting. A run that finishes on its own before the cancel reaches it keeps its own result, and the newest trigger runs after it.

Newest and oldest go by firing time, not by the order busy runners reach the task: a signal trigger fired when its signal was emitted, so every task woken by one emit shares that time, and a clock trigger fired when its runner started. Triggers that fired at the same instant keep the order they arrived in. Coalescing keeps the newest signal name and payload. If both payloads have the same signal name, carry `from` and `to`, and agree on every other key, it preserves the oldest `from`. Otherwise the newest payload stands whole. The served run records `absorbed` for the number of earlier triggers folded in (omitted when zero). Clock triggers merge only their count. Queued triggers keep their individual payloads. A run started for a waiting trigger opens its agent with that trigger's event line, the same signal and payload `rimz loop logs` prints for the run.

Every explicit policy, including `skip`, is refused for one-shots (including `--once` or a deadline), `--wait`, and `--each-worktree` tasks: one-shots retire their task row, deliveries have no run to overlap, and an each-worktree task fires per checkout while its waiting triggers are kept per task. `--overlap-limit` is refused without `queue`, or at zero. Project tasks accept the same settings. Manual `loop fire` never enqueues: it still refuses a busy task with the stop hint, except that it cancels a run holding a judge decline with no recheck and starts its own.

## Read run history

Every attempted fire appends one record to `~/.rimz/logs/loop-runs.log.jsonl`. A retained trigger records `deferred`, then its served run records a separate terminal outcome. A busy or empty drainer, or a drained trigger dropped because the task is paused, disabled, missing, invalid, or no longer retaining, writes no row. `show` searches known rooms, while `logs` reads records for the current project. Both keep reading a task's history after the task is removed.

### `loop list`

`loop list` shows the whole current room, including its worktrees. `--all` shows every known room, the caller's first, and unfolds older DONE entries and WAKE-UPS. Outside a project, in a directory with no recorded room, tasks, or one-off history, it shows every room, still folded unless `--all` is explicit. An explicit `--root` keeps that root selected. Known rooms come from machine tasks, stored instance tasks, and the caller's root. A project with only project-config tasks is not discovered unless it is the caller's. Other rooms' attention tasks appear on an `elsewhere:` line in the default view; healthy tasks elsewhere stay hidden.

A machine task with a missing, empty, or relative root appears only as a `warning:` on stderr, naming the task, file, problem, and fix: set an absolute `root` or run `rimz loop remove <NAME>`. It creates no task row or room group. This warning also appears with `--all` and `--json`; the list still exits 0.

Each heading names the home-relative root, whether its room is open, the task count before collapsing, the next clock fire when present, and nonzero spend today. Without a room, it explains which clocks and forge signals wait for `rimz start`, and whether the optional clock timer is active.

Empty sections are omitted:

- NEEDS YOU names missing checkouts of tasks that have not fired, strike-disabled or failing tasks, holds needing a fix, trust blocks, invalid triggers, workflow tasks with a parked run, and unfired one-offs waiting on a pull request closed without merging, with commands addressing the named tasks. A task is failing when its latest acting result failed and no newer run of it is in progress: a held run lock, or an active workflow run of the task in that room created after the failed run ended, as a `loop fire` retry or a scheduled fire starts; LAST says how its row reads meanwhile. A parked run, an `each-worktree` sibling started before the failure ended, and a retry whose own failure has been reported leave the task failing. A closed pull request reads `pr[<branch>] closed without merging` and points to `loop remove`. It holds for a one-off or wake-up whose condition has a `pr[...]=merged` term outside any `!`, reading `closed`, that the condition cannot do without: `pr[a]=merged && pr[b]=merged` with `a` closed qualifies, `pr[a]=merged || pr[b]=merged` does not. It ranks after failing and before a lost watcher, and a parked run does not replace it. It clears when the reading changes (reopened, merged, or unread). Only a condition trigger reads the pull request, so a `--signal pr.merged` task stays a plain wait, and with the room closed every forge key is unread, so the cause is absent. A parked task reads `<n> run(s) parked: <address> <node> <reason>[: <text>] <age>` and lists the remaining parked runs below the table, each summary once. Summary ages start at the park, not node entry. Parked summaries clip their text before the age at narrow terminal widths; piped output and JSON keep the full text. A task that needs you for another reason, such as strikes, still lists its parked runs there. A missing checkout of a task that has not fired is flagged, not retired; the task returns to its normal section when the directory returns. A staying one-off whose launch memory records its fire reads DONE whether or not its checkout still exists.
- HELD follows NEEDS YOU and precedes ROUTINE. It lists self-clearing held starts from condition records, waiting tickets, and workflow steps, one row per start: NAME, WORKTREE when any checkout needs it, REASON, HELD age, and GIVES UP in local time only when some start is bounded. The task itself keeps its normal section. NEXT (WAITING FOR in WAITING) counts these starts as `○ N held`; a task running another checkout keeps its next fire in NEXT and reads `▸ running 3m · ○ N held` in LAST (WAITING FOR in WAITING). A failing capacity check, disk floor, or unreadable agent count instead puts the task in NEEDS YOU with its oldest needs-a-fix reason and `→ rimz doctor`.
- ROUTINE has NAME, RUNS, NEXT, ACTION, LAST. It holds recurring work, including tasks that launch once in each worktree. NEXT carries the next clock time separately from RUNS. Actions read `run check`, `start <profile>`, or `check, then start <profile>`, with an account pin when set.
- WAITING has NAME, WAITING FOR, THEN, with the note `one-offs, fire once`. It holds unfired one-shots, command watches without a delivery target, and single-checkout resident launches. WAITING FOR includes disabled, paused, running, or held state when applicable. Pure-AND conditions with an observed verdict append `k of n met`, plus the number not read when nonzero. Disabled, paused, unarmed, blocked, OR, and fan-out conditions have no count.
- DONE has NAME, FIRED, with the note `recent one-offs`. It merges recorded single-checkout resident launches (a gone checkout included), retired one-offs, and rowless active runs. Running rows stay above the five newest finished entries by default, then `(and N older · --all)` unfolds the remaining finished entries. FIRED gives launch age, subject, and leader; a retired task's age and result; or `▸ running <duration>`. A retired name needs at least one fire and its newest kind-stamped record must be one-off. One-offs retired before that stamp was introduced stay accessible through `loop logs`, not DONE or `loop show`.
- WAKE-UPS folds to `N waits · M subscriptions`, dropping zero parts, followed by health (`all healthy` or `N need you`) and `rimz loop list --all`. With `--all`, the hint is omitted. Self waits appear first in agent cards, with launched children nested under their ancestors and their petnames shown beside their handles. Each wait keeps its name, state, and trigger without terminal-width clipping. Lost waits remain in their cards as well as NEEDS YOU. Subscriptions use one line per team or loop owner, with their signals, targets, and worktree count (`in 1 worktree` or `in N worktrees`). A plain `loop add --wait` row is a loop owner named after itself, not a self wait. A target matching the calling agent is marked `(you)`. Cards use current addressable agent handles, matching wait receipts, and fall back to the stored handle when the target is not addressable or the room is unavailable. Ancestor chains stop at an ancestor that is not addressable.

Every table keeps the task name first. A WORKTREE column appears only when a row is checkout-bound, and the caller's checkout is marked `(here)`. A resident task with `--worktree` names its launch destination, not the checkout its guards read; `loop show <destination>` resolves that name too. Rows sort by task name except DONE, which puts running rows first and sorts each group newest first, and self waits, which put lost watches first, then watches past check-in, then longest elapsed first. A dead watcher appears in NEEDS YOU with `watcher gone · will not wake <handle>` and a `loop remove` hint; stronger attention reasons still win. Self waits allow 30 seconds after arming for the watcher to start before being classified as lost. Ended delivery targets are retired by the harness, not diagnosed by list.

Trigger text omits the checkout's matching `path` filter, includes labels and pauses, and puts `for <hold>` and `each worktree` on a continuation line. Watched commands in the lifecycle tables read `on command exit`, not their command preview; self-wait cards use the label or command subject instead. On a terminal, table triggers wrap at modifier, condition, and word boundaries down to 24 columns. Parked workflow summaries clip before the age; other table cells are not clipped. Long triggers can still overflow when the fixed columns leave fewer than 24 columns for wrapping. Piped tables keep only the fixed continuation line. Self-wait cards leave wrapping to the terminal, and show a worktree suffix only outside the reading command's checkout.

LAST in the lifecycle tables chooses running, off, latest acting result, latest heard sibling signal, watcher liveness, then `never fired`, in that order. A good acting run reads `✓ 53m ago · 143 in a row`; a failed one reads `✗ failed 2m ago · exit 101`. A run in progress without a held run lock reads `▸ running <age>`, counted from the run's creation, while it executes: an active workflow run of the task in that room created after the latest acting result (the NEEDS YOU rule above), or, before the task has any acting result (its first fire), any active workflow run of it; one whose record carries a wait (a capacity wait, or a deduping guard's decline held inside the run) is not executing, so the row keeps its failed result and any `○ N held` while the task stays out of NEEDS YOU. A held run lock decides the running text and age before either. A sibling signal after a good run does not replace it; with only sibling signals the row reads `never fired · heard ci.passed 7m ago`. Acting-run streaks, launched-worktree counts, and spend today follow inline. A `stay` task or a condition workflow task counts the entries in its launch memory, including finished runs and resident launches remembered before every fire became a workflow run. A daily cap is shown even at zero spend. A repo task not enabled here reads `off · repo task, enable here to run`. WAKE-UPS owner summaries have no per-row LAST cell; inspect their history with `loop show` or `loop logs`.

A run whose task row is gone keeps a DONE row until it ends, with the real name `loop stop` needs and FIRED `▸ running 3m`. After that, retained one-off history remains in DONE; other rowless runs disappear. Empty rooms say how many tasks exist elsewhere and point to `--all`; with none anywhere, the add hint remains.

`--json` prints one object with a `rooms` array for the same scope (`--all` widens it). Each room has `root`, `open`, `here`, `spend_today_usd`, `tasks`, and an unfolded `retired` array of one-offs (`name`, `at`, serialized run `result`, optional `checkout`). A consumed one-shot whose run still executes is a running task, as under its run lock, until that run ends. Tasks are never collapsed and always retain real names. Each has `name`, lifecycle `kind` (`routine`, `one_off`, `wake_up`), `section` (`needs_you`, `routine`, `waiting`, `done`, `wake_ups`), `state` (`live`, `off`, `not_enabled`, `paused`, `running`, `waits_for_room`; `waits_for_room` marks a live clock task whose room is closed while the loop timer is not active), unwrapped `trigger`, `you`, and spend. Pure-AND conditions with an observed verdict outside fan-out tasks add `condition` with `met`, `total`, and `not_read`. `action` has `kind` (`check`, `start`, `wake`, `workflow`) and `subject`; it is omitted when no definition survives or the action is invalid. An action's `kind` is separate from its task's lifecycle `kind`; a workflow action's `subject` is the workflow name. Other optional fields are `attention` (`checkout_gone`, `strikes`, `failing`, `pr_closed`, `held`, `blocked`, `invalid`, `parked`, `lost`), `label`, `dir`, `worktree`, `fresh_worktree` (the per-fire prefix), `owner` (`kind`: `team`, `loop`; `name`), `booked_by` (the task's [booking chain](#where-tasks-are-stored), nearest first, each with `kind` and `agent_id`), `next_at`, `running_since` (the start the running state counts from), `last` (`at`, serialized run `result`, `ok`, `streak`), `heard` (`signal`, `at`), `budget_per_day_usd`, and `workflow_runs`, the task's active and parked [workflow runs](./workflow.md#json-shapes). Absent values are omitted, not null, except `capacity_wait`, which every task carries and which is `null` without a wait; timestamps use the same format as other JSON commands. Retired history is separate from `tasks`; it counts in watch's done fold without adding table rows.

Listing is read-only and never creates a room store. Malformed local task state fails the command; a malformed other room warns with its root and is skipped. A run lock that cannot be read warns on stderr with the task's name. When a room's locks directory cannot be listed, one warning names the root and directory, and its rows show their other state.

HELD is a human view of starts, not a JSON section. `capacity_wait` remains the oldest active workflow run's capacity wait, now with optional snake_case `hold`; unknown kinds are omitted and treated as self-clearing. `attention: "held"` means a hold needing a fix only. `loop show --json` also adds optional `hold` to each `held` or `waiting` ticket object.

### `loop watch`

`loop watch` holds the open room's tasks in a compact glance panel, repainting every second. From a worktree it still shows the whole room, never other rooms. Without an open room it refuses with `rimz start` and `rimz loop list` as alternatives; it also requires a terminal on stdin and stdout. It is the loop pane in the room's `rimzd` tab. `q` or Ctrl-C quits; `--hold` ignores both and hides the quit hint. Tasks share the room's cached forge and account-window readings within a repaint, and the next repaint reloads them. A room closing mid-watch does not stop repainting: while the loop timer is not active, its clocks read `waits for room` until it opens again. The panel prints no lock warnings.

Rows put tasks needing you first, then running tasks (those `loop list` reads `▸ running`), then clocks (due first, then earliest fire), then other tasks, with off, not-enabled, paused, waiting-for-room tasks and tasks holding only self-clearing starts last. Names sort within groups and break clock ties. The three columns are `task`, `next`, and `last run`. `next` carries the state or trigger, including `▸ running 3m`, `○ N held`, countdowns, and the list's held-state words. It wraps between words before names or last-run text clip. The next column keeps its widest word; names keep the smaller of the widest name and a quarter of the panel width. Last-run text takes the remaining width and clips with `…`. Below those floors, any cell may clip to stay within the panel. Checkout-gone, strike-disabled, closed-pull-request, blocked, invalid, parked, and lost-watcher rows put `✗ <reason>` on a separate line under `next`, across the remaining panel width. Failing rows already explain their outcome in `last run`. A hold needing a fix names its reason in `next`, or on that separate line when `next` shows the held count or the running state.

`last run` is history only: `✓ <age>`, `✗ failed <age>` with an exit code when known, `heard <signal> <age>` with no acting run, or `never fired`. A first running task without history reads `—`. Sibling signals do not replace a good or failed acting result. Streaks, delivery counts, worktree counts, and spend stay in `loop list`. A consumed one-shot with an active run keeps a named running row and supplies no next fire.

Finished one-offs fold below the table as `done`, counting retained retired history too, with the newest task's name and age. Healthy nonrunning wake-ups always fold as `wake-ups`, with `N waits · M subscriptions` (zero parts omitted) and a `rimz loop list --all` hint; wake-ups needing you or running retain rows instead and do not count in that fold. Folds alone print no table header. An empty room reads `` no loop tasks; add one with `rimz loop add …` ``.

The band starts with `loop`, then nonzero `✗ N need you` (`✗ 1 needs you`), `▸ N running`, `next: <task> in <duration>`, `○ N held`, and `q quit`, in that priority order. A task with a hold needing a fix counts as needing you. A task holding only self-clearing starts counts as held, not running; the same held count also includes off, not-enabled, paused, and waiting-for-room tasks. Folded tasks and held or blocked clocks never supply `next:`. Narrow panels drop the band's trailing segments first. Short panels cut whole rows from the end with `+N more`, keeping the header and folds when space permits; at two lines or fewer only the band prints.

### `loop show`

`loop show <word>` resolves an exact task name in the caller's room first, then the other rooms known to `loop list`. A name present in several other rooms prints one `rimz --root <root> loop show <name>` command per room and refuses to choose. Active runs without a definition and retired one-offs come next (text only; JSON still requires a task row), then worktree names among routine and one-off tasks, then subscription owners: a team name, `team#channel`, or loop-task name. Retired one-offs use the same rule as DONE: at least one fire, with the newest kind-stamped record being one-off. Other retained history does not shadow a worktree or owner. A unique worktree match starts with `worktree <worktree> → <task>`; several matches print the real task names, triggers, inspection commands, and wake-up count. An owner shows `SUBSCRIPTIONS` with every real name (text only; use `loop list --json` for structured subscription rows). A task found outside the caller's room names its room and the `--root` needed by other commands. Mutating commands and `loop logs` still require exact task names and keep their existing room selection.

A machine task skipped for a missing, empty, or relative root refuses with exit 1 and its root warning, in text and JSON alike, unless a same-named loaded task resolves first. Set an absolute `root` or remove the skipped task with `rimz loop remove <NAME>`.

A missing task refuses with exit 1 and empty stdout, in text and JSON alike. When retained fires exist in a known room, stderr points to their logs:

```text
error: no loop task named `trunk-install`; see `rimz loop list`
  2,268 retained runs, last 1d ago: `rimz loop logs trunk-install`
```

The count and age describe fires, not heard signals. The caller's room wins when it has fires; otherwise each known room with fires gets a hint using `rimz --root <root> loop logs <name>`. With no fires in any scanned room, only the first line appears. JSON also uses this refusal for retired one-offs and active rowless runs.

A task opens with `<name> · <kind>[ · worktree <worktree>][ · <trigger>] · <state>`, with lifecycle kind `routine`, `one-off`, or `wake-up`. A condition omits the trigger from the header and adds `· k of n met` for a pure conjunction with an observed verdict. Disabled, paused, unarmed, and blocked conditions have no count. The full `when <expr>[, for <hold>]` appears once above the leaf block or fan-out table, in the same normalized form as `loop list` (parentheses only where precedence needs them). Single-checkout leaves follow in expression order. Each satisfied leaf has `✓`; an unsatisfied leaf has `✗` with a reading, or `?` without one. Missing readings always carry a reason: room closed, not checked yet, unsupported forge, failing lookup with last-attempt age, no PR or CI for the branch or checkout, no open PR, open-PR facts that do not settle the key, or `not read` for non-forge keys. A leaf under `!` keeps its prefix and negated satisfaction, so a satisfied negated leaf with no reading still shows `✓` and the absence reason. Forge conditions add `forge checked <age>` when the latest probe succeeded. All readings come from disk, never a forge poll. Command watches use `on command exit` in the header and print the full command below, wrapped at whitespace without splitting tokens. Other watches retain their trigger description. An `each-worktree` task instead appends `· each worktree` and shows a `WORKTREES` table without a single-checkout count or condition state.

`loop show <name>` answers whether the task is working, then shows its evidence, and it reads a run in progress by the rule `loop list` reads (LAST above). While a run holds the task's run lock, its header state is `▸ running 3m · pid <PID> · run <run id>` (the PID and elapsed time when the lock names its holder, the run id once the run has a record). With no lock held, an executing workflow run of the task gives `▸ running 3m`, counted from the run's creation, with no PID or run id (WORKFLOW RUNS names the run); a run whose record carries a wait keeps the timing state. A health verdict follows the headline (and watch command when present), reading the latest conclusive run: its age, its status and duration, and the streak (how many consecutive conclusive runs ended the same way, and the age of the failure or good run that bounds the streak). A run waiting in [the start throttle](#the-start-throttle) has state `held 4m[, gives up 21:02] · <reason> · pid <PID>` instead; before its first reading it omits the reason. Other runs of the task waiting in other processes, as a fan-out's checkouts do, follow as `· 2 held: <first reason>`, so a running checkout keeps its own state; `loop list` and `loop watch` instead count self-clearing starts as `○ N held` beside any genuinely running checkout. While a run is in progress, executing or waiting, with the lock or without it, the verdict says `last finished run <age>` instead of `last run <age>`, and after a failure it drops `failing`, since `loop list` keeps the task out of NEEDS YOU then: `✗ last finished run 1m ago, failed (exit 1) in 78ms`. A good result keeps `✓ healthy · last finished run <age>`. Attention, capacity, waiting-fire, and heard lines follow in that order, then a running task's `loop stop` hint. For a fired one-shot whose run is still active, the task row is already gone: `show` opens with `<name> — one-shot fired` and the same running text, then the agent, the `loop stop` command, and any recorded runs as `loop logs` prints them. A retired one-off opens with `<name> — one-shot fired · ▸ running <age>` and the `loop stop` command while a workflow run of it executes after the runner exits, else with `<name> — one-shot fired · finished`, or with its workflow run's state in place of `finished` (`active`, `parked`) while a run its fires name is still live, followed by the `--runs` newest `loop logs` blocks, oldest first. Failed and canceled one-offs say `finished` too; their run blocks give the outcome.

Attention lines under the verdict name checkouts ready but blocked by takeover for at least 30 minutes, then failed live launch leaders, without changing the rule's health verdict. For fan-out tasks, only owned checkouts ready now (condition true and hold elapsed) can be blocked; losing ownership or readiness clears the warning. A block's age starts at the oldest refusal in that checkout's uninterrupted refusal run; a newer non-refusal or a recorded launch clears its warning. Ended leaders earn no warning.

A fire refused at the run lock and recorded as `overlapped` is spent. The run that refused it started before that fire, so the verdict says, in the warning tone, when such fires have had no run since: fires skipped while the active run holds the lock, fires skipped after the last run, or, when no run is active, fires skipped during the last run. With no active run, a warning about fires after the last run (or with no run yet) uses `a fire skipped, nothing has run since` or `fires skipped, nothing has run since` when any counted fire is already on a row; otherwise its numbered wording stays. With an active run, only rowless fires count toward `N fires skipped while the active run holds the lock` (`1 fire` in the singular), or `N fires skipped while a run is in progress` when the run in progress holds no run lock, and the clause is absent when there are none. A fire refused at another gate (`budget skipped`, `account skipped`, `surplus skipped`, `throttle skipped`, `capacity held`) or recorded as `deferred` ran nothing, so it does not clear the warning. Fires on a `○ no record` row (below) are excluded from this warning. An overlap attributed to an earlier run is not counted as skipped during the last run.

When anything waits under coalesce or replace, the header says `N fires waiting · one run follows when the active run ends`, counting the waiting trigger's `absorbed + 1` fires. A queue says `N fires queued (limit L) · each runs in turn when the active run ends`. Both use `1 fire` for a single fire. A pending replacement adds `· stopping pid N`. `deferred` refusals fold as counts into RECENT RUNS rows, without rows of their own; the `RECENT RUNS` entry below says which row takes them. `show --json` adds `backlog`: `null` for `skip`, otherwise an object with `policy`, `limit` (queue limit, otherwise `null`), `triggers` (in firing order, each with `arrived_at`, its firing time; `signal`; `absorbed`; and `from_arrived_at`, the firing time of the kept `from`, once a merge has preserved an older one), and `stopping` (an object with `pid`, `started_at`, and `cause`, or `null` when no stop is pending). This does not change `running`, `held`, or `waiting`, which still describe active runs and throttle tickets.

Sibling signals heard without acting are not runs. Below the verdict, `heard N other <family>.* signals since, latest <name> <age>` counts those heard after the newest acting run, or all retained history without one (omitting `since`). The line is absent when the count is zero. With no fires, a delivery task says `wakes <handle>; not fired yet`, without a manual-fire hint. Other clock-triggered tasks suggest `loop fire`; condition, signal, and watch tasks say `no runs recorded`. Watched rows label their timeout `check-in`, since it does not kill the watched command; other nonresident rows keep `timeout`.

The sections below follow in this order. The facts then read `action`, `check` (when present and not check-only), `wakes`, `label` (when set), `verify`, `root`, `dir`, `source`, `booked by`, `timeout`, `will not fire` (for blocked trust), `throttle`, `budget`, `surplus`, `spend`, and `strikes`. Rows without a value are omitted. `action` uses the list's vocabulary, such as `start sweeper` or `check, then start sweeper`, with ` (you)` and the account pin when applicable, then `· pane stays open` and `· stops idle agents in the checkout first` when configured. A check-only task reads `action: run check · <command>`. `booked by` names the agent that booked the task: its current handle while the room's rows still resolve its id, otherwise the id. Resident tasks never print `timeout`.

When signals are declared, `wakes` reads `leader on <signals> · N armed`, counting the room's subscriptions owned by this task. With zero armed subscriptions and no live ledgered leader it ends `· 0 armed, no live leader`; a live leader with none armed reads just `· 0 armed`. Launch memory is not a live roster.

Without a configured throttle setting or a run waiting in the start throttle, there are no throttle or load facts, even for an explicit per-task switch. With a setting or a held run, an `--agent` task's `THROTTLE` section ends the screen with current readings beside configured limits: `cpu pressure`, `io pressure`, and `memory pressure` (avg10 only), available memory, free disk where the run will write, `open agents`, and `automated agents`. The two counts read `N (limit max M)` when their cap is set, otherwise just `N`. An explicit `off` task still shows the readings, with `throttle: off (skips machine stagger, resource limits and capacity check, not caps)` in its facts. Non-spawning tasks never show the readings section.

| Section | Contents |
| --- | --- |
| `WORKTREES` | Fan-out tasks only: owned checkout count and nonzero blocked, ready, holding, capacity, waiting, unknown, and launched counts, in that order. Columns are `WORKTREE`, `STATE`, each distinct condition key in expression order (`!`-prefixed when its single term sits under a negation), then `LEADER` (`RUN` for workflow tasks) and `REASON`, each present only when a rendered row fills it. Rows put blocked checkouts first, then ready, holding, launched with running, waiting-on-you, or failed leaders (newest first), then every waiting or unknown checkout. Waiting means the condition is false; unknown means an unread key leaves it unsettled. Workflow runs in `active`, `parked`, or `failed` take that same launched rank. Within other groups, names sort ascending. Blocked means ready now (condition true and hold elapsed), unlaunched, with an uninterrupted takeover refusal run. Blocked rows show the uninterrupted refusal age and name the newest recorded blockers under `REASON`; holding rows show elapsed/hold; waiting, unknown, and ready rows show whether each leaf, negation included, is satisfied, and its reading (`?` with a cache-only reason, or `not read`, when absent). Resident launches show launch age and ledgered handle with effective status under `LEADER`, adding `· N armed` only when the task declares subscriptions. Workflow launches show launch age and `<address>  <state> · <node>`, adding `· <detail>` when the shared run phrase is nonempty. Missing run records show `<run-id>  gone`; a project-scoped entry names the same run on every checkout. Term cells stay blank for blocked and launched rows. A checkout whose run holds on a judge decline is a launched workflow row. Launches fold into a count with a status breakdown and the newest folded launch's checkout, handle or run address (id when gone), and age. Workflow states `done`, `canceled`, and `gone` fold this way. A folded group adds `→ reasons and leaders: rimz loop show <task> --all` (`reasons and runs` for workflows). `--all` unfolds them after the waiting rows, newest first, with ended leaders marked `@handle ended`. Reasons clip to the terminal width. A closed room adds the condition block's note about unavailable CI and PR readings. Readings come from disk, never a forge poll. |
| `LAUNCHES` | Non-fan-out resident launch memory, when nonempty (the launches remembered before every fire became a workflow run): bare destination names (scope checkout names when no destination is recorded), ledgered leader handles, and relative launch ages, newest first. |
| `LAST RUN` | The same acting run as the verdict, omitted for a clean `launched` result or an empty body whose header names no manual fire, signal, or late one-shot. Blank check output alone counts as empty. It never selects a refused fire or a guard decline that ran nothing. The header carries status, age, duration, exit, `manual` for a hand fire, the signal that fired it, and how late a one-shot fired. The body retains fire-time condition readings but omits the recorded expression when it equals the current condition. Otherwise it is what `loop logs` prints for that run, except that a passing check prints its last five lines, followed by the `loop logs` command whose first block is this run. A failed check prints every line. |
| `AGENT RUNS` | Check-gated `--agent` and `--wait` tasks only, never for a resident task: how many fires escalated to the action, the total and average cost across every escalated run the log still holds that recorded one, and the five newest folded rows. The heading says `A of F fires`, or `none in F fires`, counting action attempts over all retained fires. Rows fold when adjacent within AGENT RUNS, on the same typed key as RECENT RUNS: distinct spawned run ids stay separate, while deliveries and `target gone` can fold. A check skip between equal deliveries does not split their AGENT RUNS row; RECENT RUNS still shows that skip. STATUS adds `×N`, TOOK gives a duration range, COST sums spend, and NOTE starts with `over <span>` when the span reaches one second and ends with the attributed waited and skipped fire counts. |
| `RECENT RUNS` | The latest runs, newest first. Adjacent runs fold by typed mode, result, check or judge outcome, exit, checkout, run id, target, how late a one-shot fired, and whether fires waited on them or were skipped. Reasons and output do not split a group; the newest note wins. A group adds `×N`, a TOOK duration range, and summed COST and TOKENS. If the group's runs span at least one second, NOTE starts with `over <span>`; shorter spans are omitted. Runs with collided fires stay apart from those without them; collided runs can fold together and sum their counts. A fire refused at the run lock (`overlapped` or `deferred`) folds into the record whose `run_lock` equals its `holder`, regardless of position, checkout, or result, as `N fires waited on it` or `N fires skipped, run already active`. An `overlapped` or `deferred` record that held its own lock keeps its own row when fires match it, with its recorded status, age and note followed by those counts; it is counted once and does not also fold into another row. Without matched fires, it follows the usual fold. Fires without a holder fall back to their checkout's next run past gate refusals. A named holder with no matching record can use that fallback only when the receiving record has no `run_lock`; a later tenure of the same checkout's lock proves it gone instead. Unstamped non-gate records bound those legacy gaps. A holder proved gone gets one `○ no record` row at its newest refused fire's position. Trailing fires (after the last run, or in a checkout no later run used) without that evidence still take no row; the header states active waiting work and skipped fires with no run since. A drained follow-up folds like an ordinary run. The table is omitted only when its sole row stands for one run, the verdict's acting run, with no folded fires. A `×N` row always keeps the table. Each uninterrupted run of `takeover blocked` refusals for a checkout still folds at its newest refusal's position, with `×N`, the checkout, the newest blocker reason, and `since <age>` for its oldest refusal. A non-refusal record for that checkout ends the refusal run. The heading is `RECENT RUNS (newest first)`, adding `· S of T rows` inside the parentheses only when `-n` hides rows. `-n, --runs <N>` sets how many rows, default `10`, and caps RUNS's finished rows the same way. It does not limit `show --json`, whose `runs` holds every retained fire. `MODE` appears when recorded runs differ in mode, `COST` and `TOKENS` when a row has one. |
| `RUNS` | Workflow tasks only, after the facts: each ledgered run, plus every run the task started in its room, finished ones included, excluding every run named by WORKTREES. Live runs come first, then finished and `gone` runs, newest first. RUN is the target `rimz workflow show` selects that run by: the address, or the id when the address would select another run, and the id when gone; WORKTREES's RUN cell uses the same target. Columns are `RUN`, `CHECKOUT`, `STATE` (`active`, `parked`, `done`, `failed`, `canceled`, or `gone`), `NODE` (where a finished run stopped), `DETAIL` (the [shared run phrase](./workflow.md#inspect-runs)), and `AGE` since a finished run ended, else since the run entered its node. A missing record has state `gone`, blank NODE and DETAIL, and AGE since launch. A finished run whose record this RimZ cannot decode (an older RimZ's, or one the room's workflow pass ended as stranded) shows the state, node and ending its record names, blank DETAIL, and its id as RUN. Adjacent finished rows fold when checkout, state, and node agree: STATE adds `×N`, DETAIL reads `over <span>` once the span reaches one second, and RUN and AGE come from the newest member; `workflow show --history` and `--json` keep each run. `-n, --runs` caps the finished rows after folding, while live rows always print; the heading adds `(S of T rows)` when rows are hidden. Under the table, `→ rimz [--root <root>] workflow show <RUN>` opens the first row that has a record. Omitted when there are no unmatched runs. |
| `KEPT WORKTREES` | `--worktree` tasks: the per-fire trees still on disk whose marker names this task, newest first, with columns `WORKTREE`, `BRANCH` (the branch checked out now) and `AGE` (since creation). Omitted when there are none, and for a task without a `fresh-worktree` prefix: a tree a task kept before its prefix was removed is listed by `rimz worktree list`. |
| `SUBSCRIPTIONS` | Non-fan-out tasks only: rows owned by this loop task in its room, one per real task name, without the list's collapsing. Each row shows its checkout, target handle, signal, and last acting run with its age, using the list's LAST wording. Without an acting run it reads `never fired · heard <signal> <age>` for a heard sibling signal, otherwise `never fired`. Omitted when no rows name this task as their owner. |

`loop show --json` keeps one entry per attempted fire, overlapped fires and guard declines included, but not heard sibling signals. A retained trigger has its `deferred` entry and a later served-run entry; only the served run carries a nonzero `absorbed` count. Empty or busy drainers and drained triggers dropped for a paused, disabled, missing, invalid, or no longer retaining task add nothing. Human history rows and the `AGENT RUNS` denominator also exclude siblings. `loop logs` and `loop show` use the task row's root and exclude history before a reused wait's arming. `launches` holds resident records only and is `{}` for a workflow task. Each record is keyed by scope checkout and carries `at`, `leader`, and, for a named resident worktree, `destination` (the launched checkout path). Older records omit `destination`. The JSON document adds top-level `kind` (`routine`, `one_off`, `wake_up`) and keeps `task`, `entry`, `launches`, `runs`, `running`, `held`, `waiting`, `workflow_runs` (unfolded and unlimited: each run named by the workflow launch memory once, then every other run this task started in its room, live ones first and finished ones newest first with `terminal_at`; empty for a task that starts no workflow), `booked_by` (the booking chain, as in `loop list --json`; omitted for a task with none), `kept_worktrees` (the `KEPT WORKTREES` rows, each with `name`, `branch`, `path` and `created_at`; empty when there are none), and `worktrees`, and `heard`: `null` without a recorded sibling signal, otherwise an object with `count`, `since` (the acting run's RFC 3339 timestamp, or `null`), `latest` (signal name), and `latest_at` (RFC 3339 timestamp). `running` is `null` while no run is executing, otherwise an object with `pid`, `started_at`, and `run_id`. Under a held lock, each is `null` when the lock names no holder or the run has no record yet. For an executing workflow run with no lock held, `pid` and `run_id` are `null` and `started_at` is the run's creation, the `running_since` `loop list --json` prints; `run_id` names only a supervised run, never a workflow run, which `workflow_runs` carries. A run waiting on its record leaves `running` `null`. `held` is `null` unless that run is waiting in the start throttle, and then an object with `pid`, `checkout`, `since`, `reason` (`null` before the first reading), `hold` (the hold's kind, such as `disk` or `queue`, omitted before the first reading), and `position`, its 1-based place in the queue. `waiting` lists the task's other runs waiting in the throttle, each an object of the same shape. A record admitted after a hold carries `throttle_wait_ms`. A scheduled `fire-at` fire that started two minutes or more past its instant carries `late_ms`. A run lock that cannot be read warns on stderr and prints `null`.

`worktrees` is an unfolded array for fan-out tasks (empty for other tasks). Each row has `checkout` (path), `name`, and `state` (`blocked`, `ready`, `holding`, `capacity`, `waiting`, `unknown`, or `launched`). Condition rows include `readings` (key to value or `null`, in expression order) and `short` (keys of leaves not satisfied, negation included). Holding rows add `since`, `hold_ms`, and `held_ms`; ready rows add `since`. A true condition without a matching hold clock starts as holding with `since` set to now and `held_ms` zero when a hold is configured, otherwise ready since now. Blocked rows add `since` and `blocked` (`count`, `since`, `reason`), whose count and age span only the current uninterrupted refusal run. Resident launched rows add `since` (launch time), `leader`, `leader_status` (`null` when ended), and `armed`. Workflow launched rows instead add `since` and `run`, the same object as the matching `workflow_runs` row, without leader or arming keys. `--all` changes only the human table, not JSON.

In the human table, a condition key used by multiple terms shows a present reading without a truth glyph; single-term keys keep their glyph. If the owned worktree list cannot be read, the table prints one explanatory note instead of rows. If the room's subscription catalog cannot be read, both the screen and `--json` print what they have, with every `armed` count at zero, and then exit nonzero with the error.

Run records refused at the run lock carry `holder` when the lock names one, in the shape `{"pid": 42424, "started_at": "2026-01-01T00:00:00Z"}`. Older records and records without a known holder omit it. Their stored `error` names the policy outcome without a pid or an age. A fire that held the run lock records its own tenure as `run_lock` in the same shape, including when `loop stop` writes on its behalf. Older records and fires that never held the lock omit `run_lock`; human `loop logs` output does not print it. A fire that found the task's workflow run still active (`overlapped` or `deferred`) carries that run's id as `blocking_run`, and its stored `error` reads `run <id> still active — <outcome>`; other records omit it.

A `○ no record` row uses the warning tone, never merges, and never adds `×N`. Its WHEN is the newest refused fire's age; NOTE has `pid <pid>`, the checkout prefix when present, and the usual waiting/skipped counts. TOOK, COST, TOKENS, and MODE are `-` when their columns appear, and this row alone never introduces MODE. It counts as one row toward the heading's row limit; the missing run itself adds no record. These rows are human-only: `--json` still has one entry per real fire and no synthetic missing-run entry.

A sample is in the [loops guide](../../guide/loops.md#what-a-fire-leaves-behind).

Workflow-run DETAIL in `loop show` and WORKTREES uses the same count-free status phrase as `workflow list`: a check wait reads its outcome, not `said <outcome>`, and no advance count is printed. `loop list --json` and `loop show --json` keep their shapes; only `workflow_runs[].detail` text changes. Loop views do not add agent or PR enrichment.

### `loop logs`

`loop logs <name>` prints full forensics in blocks, oldest first, so the newest ends the output. Each run, including a guard decline, keeps its own block. For a task whose fires start workflow runs (every task but an unguarded delivery), a block that names a workflow run (`launched`, a terminal report, or `workflow skipped`, which names the run holding the scope) reads that run's record and journal when `loop logs` runs. The journal gives the run's evidence, oldest first: each shell check as `<node>: <outcome>[ · exit N]` with its output below (the last 64 KiB of its log), each judge as `judge <agent>: <outcome> — <first reason line>`, and each step attempt as `<node> attempt <visit>: <outcome>`; cost and duration follow when recorded, and adjacent evaluations of one check with one outcome fold into the newest with `×N`. Then `run: <id> · <state> at <node>`, with the [shared run phrase](./workflow.md#inspect-runs) after it while the run is live, then `→ rimz [--root <root>] workflow show <RUN>`. An `overlapped` or `deferred` block names the run that blocked it the same way, as `blocked by: <id> · <state> at <node>` with its command. A finished run whose record this RimZ cannot decode reads the state its record names, ` at <node>` only when the record names one, and `→ rimz [--root <root>] workflow show <id>`. A run whose record is gone reads `run: <id> · gone` (or `blocked by: <id> · gone`) with no command. Fires refused at the run lock fold into their attributed record's block, using the same attribution as RECENT RUNS; its header ends with `N fires waited on it` and `N fires skipped, run already active` when present. A holder proved gone without a record gets one warning-toned `○ no record · <newest fire age>[ · <checkout>] · <counts>` block at its newest refused fire's position, with that fire's forensics below. Trailing at-lock fires stay as refusal blocks in place, merging adjacent fires only when their typed key and holder agree. Other gate refusals fold by the RECENT RUNS key, and `takeover blocked` folds per checkout at its newest member's position. A repeated refusal's header adds `×N` after the status and, when the span reaches one second, `over <span>` after the newest age; its body keeps the newest member's forensics. `loop logs` has no `--json`: each folded member's own record is in `loop show --json`'s `runs`, which needs a current task definition. Each ordinary block holds status, age, trigger (`manual` for a hand fire, `signal <name>` for a signal, `scheduled` otherwise), exit information, check output, error chains, run ids, pane output tails, cost and tokens, and transcript links when recorded. A `--worktree` fire adds `worktree: <name>`, its per-fire tree, and a `warning:` line when that tree fell back from fetching trunk; the run record keeps it as `worktree` with `path` and, on a fallback, `base_fallback`. A known run-lock holder gets `holder: pid N · started <D> before this fire`, or `before the newest fire` for a folded group. The interval measures the holder's start to that fire, never to now. Agent messages render as [agent prose](../cli.md#agent-prose); check output and errors stay literal. While the task is running, the last line is the running text `loop show` opens with; `--failed` leaves it out. Empty history uses the same delivery-target line or clock-only fire hint as `show`; `--failed` instead says `no failed runs recorded`. A removed task whose retained history holds only heard signals points at `--heard` instead.

| Flag | Effect |
| --- | --- |
| `-n, --runs <N>` | How many blocks to print after folding and filtering. Default `10`. |
| `--failed` | Only `failed`, `timed out`, `budget exceeded`, `verify failed`, and `error` runs, plus `launched` workflow blocks whose run ended `failed`. |
| `--heard` | Include heard sibling signals in log order, labelled `○ heard`. |

Condition runs in `loop logs` include `when: <expression> · held <duration>` and a JSON readings line in expression order (map order if a stored expression cannot be parsed). The record keeps the canonical expression, configured hold, observed held milliseconds, and readings; manual fires do not synthesize readings. Signal runs include `signal: <name>` and, when the signal carried fields, a JSON payload line. Sibling signals appear only with `--heard`; their stored records are unchanged.

## Rename and remove

`loop rename <name> <new-name>` moves the task to the new key in its store and prints ``renamed loop task `<name>` to `<new-name>` ``, or ``no loop task named `<name>` `` with exit 0 when none exists. Its workflow launch entries and live workflow runs, active or parked, follow it; finished run records keep the name they ran under. The new name must be a valid task name, differ, and be free: a taken name refuses with ``loop task `<new-name>` already exists``. A `stay` task's launch memory follows it the same way. A task that still remembers a launch from before every fire became a workflow run cannot be renamed, because that launch's leader retains the task name. Edit it with `loop add` under the same name, or remove and re-add under a new name (which relaunches its checkouts). A resident task without such launches can be renamed. The task re-arms, so an interval task next fires one interval after the rename.

`loop remove <name>...` deletes each task from its store and prints ``removed loop task `<name>` ``, or ``no loop task named `<name>` `` when none exists. Comment lines above the removed task's headers stay above the next header, or at the end of the file. Missing names do not stop later removals or cause a nonzero exit. Removal clears the task's resident or workflow launch entries and detaches its active and parked workflow runs, which continue as independent runs. Re-adding a condition workflow task skips that independent run without adopting it, then permits a fresh launch after it ends. Replacing a task under the same name keeps its launch entries. Run history stays readable through `rimz loop logs <name>`. For a project task, both commands print the [trust result](#project-tasks).

## Eject a task

`rimz loop eject <name> [<workflow>]` turns a task's compiled graph, the one [`rimz workflow show <name>`](./workflow.md#loop-tasks) draws, into a deployed workflow at `~/.rimz/workflows/<workflow>/`, named after the task unless you give a name. The directory holds `workflow.toml` and the files the graph reads, each copied as the task's fire would read it now: the prompt or prompt file as `prompts/prompt.md`, each judge's prompt as `prompts/judge-<n>.md`, and the system-prompt file as `prompts/system.md`. An agent step without a `timeout` gets `loop.toml`'s `default-timeout` (2h when unset) written into the file. The command then rewrites the task's row in its own store (`loop.toml`, or the project's `.rimz/config.toml`) to `workflow = "<workflow>"`. The row keeps its trigger and timing (`every`, `at`, `cron`, `signal`, `match`, `when`, `for`, `watch`, `once`, `deadline`, `provider`), `root` and `dir`, `label`, `each-worktree`, `overlap` and `overlap-limit`, `budget-per-day`, `surplus` and `surplus-after`, `max-strikes`, and `booked-by`, and eject writes `once-per-checkout` with the task's own value, so a task that launched at most once per checkout still does and one that did not still does not; every other field now lives in the workflow. Arming, strikes, launch memory and run history stay with the task. Live runs keep the graph they were sealed with, and still hold the task's scope, so the next fire after an eject follows the task's overlap policy against them. From then on the workflow directory is what the task runs: edit it, and `rimz workflow validate <workflow>` checks it.

It prints ``ejected loop task `<name>` to workflow `<workflow>` at <dir>`` and a line naming the row's new `workflow` value, then for a project task the [trust result](#project-tasks). It refuses, changing nothing:

| Task or target | Refusal |
| --- | --- |
| The workflow directory exists with other contents | ``workflow `<workflow>` already exists at <dir>``, with the command to pick another name. |
| A room-instance row: a `--wait` delivery or a one-shot | It is bound to the room's session and cannot outlive it. |
| A `workflow =` task | It already runs that workflow; edit the workflow instead. |
| A `fresh-worktree` task | A workflow task's row refuses `fresh-worktree`, since a workflow's steps place their own agents; remove it or keep the task inline. |
| A project task while the project is untrusted | Its commands would leave project trust; trust the project first. |
| A graph that does not validate | Every validation error, as `workflow validate` prints them. |

The directory appears whole: it is written beside the workflows under `~/.rimz/workflows/.eject/`, validated there, and renamed into place. A rerun after a crash between publishing the directory and rewriting the row finds the same contents and finishes the rewrite.

A task that fires on a signal, a condition, or a watch keeps its trigger's evidence: the fire hands it to the run as the run prompt, which each agent step's prompt ends with, where the inline graph put it before the prompt.

## Timer

`rimz loop timer install` installs one user-level timer that ticks every minute: a systemd user timer on Linux, a launchd agent on macOS. Each tick re-reads every task and fires due clock tasks only for roots without an open room; a root with a room is left to that room. One timer covers every root, so adding or editing a task needs no reinstall.

| Command | Prints |
| --- | --- |
| `timer install` | `loop timer: installed (<backend>; every 1m) → <path>` |
| `timer status` | `loop timer: active (<backend>) → <executable>` (or `inactive`), then `task roots without a room: N`; or `loop timer: not installed`. |
| `timer remove` | `loop timer: removed (<backend>)`, or `loop timer: already absent`. `rimz uninstall` removes it too. |

A timer fire follows the same rules as a room's:

- An `--agent` fire starts a room for its project and runs the turn there. A scheduled check-only fire opens the room before the check, once the budget, overlap, and deadline gates pass. The room stays open and keeps time for that root from then on; `timer remove` does not close it.
- A `--wait` task still needs its pinned session, and an untrusted project task stays blocked.
- On Linux each fire runs in a transient `systemd-run --user --scope`, in its own process group, so it outlives the tick. A tick that would take that route checks the systemd user manager is reachable first, and refuses the whole pass naming the fix when it is not: nothing fires, no stamp moves, and the tasks are still due on the next tick.
- A task the timer sees for the first time arms without firing, so installing the timer replays nothing.

The task model, file shapes, and scheduler are in [loops.md](../../internals/harness/loops.md).
