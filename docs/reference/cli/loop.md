# Loop CLI

`rimz loop` launches something on a clock, on a signal, or while state conditions hold. A task is a name, one trigger, and one [kind](#what-a-task-launches): a shell command, a deployed workflow, an agent, or a team. The loop decides when the task fires; the kind says what the fire launches. A shell task's fire runs its command as a one-check [workflow](./workflow.md) run and lasts exactly as long as the command: exit `0` ends the fire done, any other exit or the task's `timeout` ends it failed. A workflow, agent or team task's fire runs the launch you would type yourself (`rimz workflow run`, `rimz agents`, `rimz teams launch`) with the task's target and arguments. The [loops guide](../../guide/loops.md) teaches when to use each trigger; an alarm an agent sets for itself, on a command's exit or a condition, is [`rimz shell`](./shell.md) or [`rimz watch`](./watch.md), which store their rows in the same stores as [delivery rows](#delivery-rows).

A clock task fires while a room for its project is open, from that room's sidebar, or from the optional machine-wide [timer](#timer) when no room is open. A signal task fires from whichever process emits the signal, without the timer or sidebar. [`rimz events emit`](./events.md#emit-a-signal) still requires an existing room.

```sh
rimz loop add morning --agent claude --cron '0 7 * * 1-5' -- "summarize what landed on main overnight"
rimz loop add watchdog --every 15m --timeout 20m -- cargo test
rimz loop add --team forge --when 'pr[feat-x]=merged' -- -w feat-x-followup "start the follow-up"
rimz loop list
rimz loop show watchdog
rimz loop fire watchdog
```

| Command | What it does |
| --- | --- |
| `rimz loop add [NAME] [--shell\|--workflow <NAME>\|--agent <SPEC>\|--team <TEAM[#LANE]>] ... [-- <ARGS>...]` | Add a task, or replace a same-named task (machine replacement requires the same project root). |
| `rimz loop remove <NAME>...` | Delete tasks from the stores that own them. Run history stays. |
| `rimz loop rename <NAME> <NEW_NAME>` | Rename a task in its store. |
| `rimz loop enable <NAME>...\|--all` | Arm tasks on this machine and clear holds and strikes. |
| `rimz loop disable <NAME>...\|--all` | Hold tasks on this machine until enabled. |
| `rimz loop pause <NAME> --for <DUR>` | Hold a task for a bounded time. |
| `rimz loop fire <NAME> [--signal <NAME> [--json <OBJECT>]]` | Fire a task now, outside its schedule, optionally as if a signal had triggered it. |
| `rimz loop stop <NAME>` | Stop a task's active run and release its overlap lock, including a fired one-shot's run. |
| `rimz loop list [--all] [--json]` | List the current room's tasks by lifecycle. |
| `rimz loop watch` | Hold the open room's live glance panel with countdowns and running tasks. |
| `rimz loop show <NAME>` | One task's trigger, health, and recent runs. |
| `rimz loop explain <EXPR>...` | Current condition readings and truth, without adding a task. |
| `rimz loop logs <NAME>` | Full forensics for a task's recent runs. |
| `rimz loop timer install\|status\|remove` | Manage the machine-wide timer. |

Every command takes the [global flags](../cli.md#global-flags). `loop show --json` prints structured task, launch, and run records and the running state. It requires a current task definition. Text `show` also inspects an active run or a finished one-off after its definition is gone; other removed tasks refuse with a `loop logs` hint when retained runs exist.

## Add a task

`rimz loop add [NAME] [<kind flag>] <trigger and placement flags> [-- <ARGS>...]` takes an optional name of letters, digits, `-`, and `_`, at most one [kind flag](#what-a-task-launches), exactly one trigger, and the arguments after `--`: a shell task's command, which it requires, or the arguments a launch task passes to its launch. Adding over an existing name gives the task a new identity, so its strikes, arming and history start fresh. An omitted name is generated as `loop-<petname>`, one no task in the catalog holds, and the receipt's first line prints it. A new task goes right after the store's last task. If the tasks end the file, or the store has none yet, it goes at the end of the file, below any comment lines there. Commented settings such as `# default-timeout` therefore keep their original section. Adding a name that already exists in the same store replaces that task, except that a machine task whose resolved project root differs is refused, including a hand-edited row with no root or a relative root that does not resolve to this project. Replacement keeps comment lines above the task's header in place.

### What a task launches

| Kind flag | Stored row | What a fire runs | Alive while |
| --- | --- | --- | --- |
| `--shell`, or none | `kind = "shell"`, `args = [<command>...]`, `timeout` | the command, as a one-check workflow run ([below](#the-command)) | the run is unfinished |
| `--workflow <NAME>` | `kind = "workflow"`, `target`, `args` | `rimz workflow run <NAME> <ARGS>...`, whose run is the task's own | the launch process runs, or the run is unfinished |
| `--agent <SPEC>` | `kind = "agent"`, `target`, `args` | `rimz agents <SPEC> <ARGS>...` | the launch process runs, a `-p` run it started is unfinished, or an agent it launched has not ended |
| `--team <TEAM[#LANE]>` | `kind = "team"`, `target`, `args` | `rimz teams launch <TEAM[#LANE]> <ARGS>...` | the launch process runs, or an agent it launched has not ended |

The arguments after `--` go to the launch unchanged; loop adds no launch option of its own beyond its [`-w <name>`](#placement). An agent that is idle in the room has not ended, so it is alive: under the default `skip` [overlap policy](#overlapping-triggers) a room agent or team a task launched holds the task until it is closed, and under `allow` an agent task launches beside it. A `-p` run in an agent task's arguments has no timeout unless those arguments give `--timeout`.

`loop add` checks a launch task before it stores it, and refuses, naming the task, the flag or target, and the fix, when:

- the arguments do not parse as that launch (the launch's own error is shown), read as one of the scene's other verbs, or set a global flag;
- the target does not resolve as the launch resolves it: an unknown workflow, profile or team, or a workflow that fails validation. An agent target is resolved in the lane the fire launches into, the arguments' `--channel` or, for a task that fires in its root, that directory's lane, so a bare role of the team that holds that lane in the room resolves as that team's role, except in a tail with `--subagent-profile`, which names the subagent profile. `--workflow` takes a deployed workflow's name, not a path;
- the arguments carry `--detach`, `--stdin`, `--input-format stream-json`, or `--resume` with no id;
- `--timeout` is given: the launch's own bound applies (the workflow's `deadline`, a `-p` run's `--timeout`);
- `--each-worktree` is given beside a `-w`, `--channel` or `--from-pr` in the arguments, a team target's `#lane`, or the task's own `-w`;
- the task's `-w` and a `-w` in the arguments name different worktrees, a bare `-w` in the arguments included (``-w `<name>` and the launch's worktree (a generated name) differ; keep one``); the same name in both is stored once, in the arguments;
- the task's `-w` is not a valid worktree name;
- a team target's `#lane` is empty, or comes beside a `-w` or `--channel` in the arguments, as `rimz teams launch` refuses it;
- `--each-worktree` is given beside a workflow whose `scope` is `project`;
- the arguments or the task carry `-w` for a workflow whose `scope` is `project`, which runs at the project root.

A shell command whose program is `rimz` and which reads as an agent launch, a team launch or `rimz workflow run` is refused with the kind flag to use. A command that runs `rimz` through `sh -c` or a script is a shell task.

A launch task's fire starts its launch as a separate process in the fire's checkout, with the environment a shell task's command gets minus `RIMZ_CHANNEL`, every `RIMZ_AGENT_*` variable and the room pin (`RIMZ_PROJECT_ROOT`, `RIMZ_WORKSPACE_ID`), so it lands in the task's room whatever room's pane you fire it from, no stdin, and its output in a log file. The fire waits for that process to exit, so `rimz loop fire` and `rimz loop run` return once the launch is placed. An agent fire stops waiting as soon as a `-p` run carrying its fire id is published. A room launch opens unfocused. The fire records one outcome:

| Kind | Outcome |
| --- | --- |
| workflow | the run's own outcome, recorded when it ends; a launch refused by a live run of the workflow in its checkout (another task's, or one started by hand, never a `workflow run --once` try) is `overlapped`, which is neutral and names that run, and the trigger is not held; any other launch that exits nonzero without starting a run is `error` |
| agent and team | a published `-p` run or exit `0` is `launched`, which resets strikes, and the `-p` run's own ending is never recorded; exit `125` is the neutral budget skip; any other exit is `error`, a strike, with the launch's error text on the row; a launch ended by a signal is `canceled`. A `launched` fire's receipt and its `loop logs` block (`launched: …`) name what it launched that is alive then: each agent's `@<handle>#<lane>`, or the `-p` run's id |

A workflow run's outcome, recorded after the fire, is written by the settle pass the tick runs once a minute, and before every `rimz loop run`. A fire whose `rimz loop run` died before its launch was settled is recorded by the same pass, `launched` when an agent or a `-p` run carries its fire id and `error` when nothing does.

### The command

A shell task's words after `--` are an argv, stored as `args = ["...", ...]` beside `kind = "shell"` and handed to the process word for word: nothing is re-split, expanded, or globbed, so a word holding a space, a quote, `$`, or `*` arrives as written. For a pipeline, a redirect, or a variable, write the shell yourself: `-- sh -c 'cargo test 2>&1 | tail -n 50'`. The receipt, `loop list`, and `loop show` print the argv as one shell line that reads back to the same words.

Each fire starts a workflow run of one `test` check named `command` whose script execs the argv. Its working directory is the task's project root, the checkout an `--each-worktree` fire selected, or the task's [`-w` worktree](#placement). Exit `0` ends the run `done`, which the task records as `completed`; any other exit, death by a signal, or the timeout ends it `failed`, a [strike](#strikes). The check's output stays in the run's `work/<seq>.check.log`, and `loop show` and `loop logs` print it. `rimz workflow show <task>` draws the one-node graph.

`--timeout <DUR>` caps the command: `s`, `m`, `h`, or `d`, greater than zero. Without it, add writes `loop.default-timeout` (default `2h`, set with `rimz config set loop.default-timeout 3h`) to the row, so the stored row always carries the cap its fires run under. A command still running at the cap is killed, and the fire fails.

The command runs with the room identity pin, `RIMZ_LOOP_TASK` (the task name), `RIMZ_LOOP_FIRE` (the fire's id), `RIMZ_WORKTREE_PATH` (its checkout), and, when the trigger carried evidence, `RIMZ_LOOP_TRIGGER`: one line naming what fired it, such as ``Rule `smoke` fired here. deploy.finished · env: prod`` for a signal or the readings of a condition. A clock fire and a manual `loop fire` carry no evidence, so the variable is absent, unless `loop fire --signal` hands the fire a [pretend signal](#fire-and-stop-a-run). Nested `rimz` commands stay in the task's room.

A `rimz agents`, `rimz subagents` or `rimz teams launch` in the command launches as it does from a user's shell: a launch without `-p` returns once its agent or team is placed, `--bg` and `--detach` mean what they mean there, and a `-p` run is an ordinary supervised run, with no timeout unless the launch gives `--timeout`, while the task's `timeout` still bounds the command. Each one carries the fire's id, read from `RIMZ_LOOP_FIRE`, on its agent rows, its supervised run and a worktree its `-w` creates, beside the task name and the fire's [booking chain](#where-tasks-are-stored) as provenance; it resolves no calling agent, so it has no launcher or parent and takes its lane from its directory. A launch from an agent's own pane carries no fire id, whatever its environment says, and neither does a launch whose environment removed `RIMZ_LOOP_FIRE`: that is how a command hands a launch off. [`loop show`](#loop-show) lists what carries the id while it is alive, and [`loop stop`](#fire-and-stop-a-run) stops it; a SIGTERM to a foreground `-p` run under a fire cancels the run durably, so a fire that ends early (the task's `timeout`, `--overlap replace`, `rimz workflow cancel`, `rimz loop stop`) ends its command's `-p` run too.

The trigger's evidence is appended to a launch's nonblank prompt, after a blank line. A `rimz workflow run` in the command starts its run and returns as it does from a shell; that run carries the fire's id but is not the task's own run, so it reports to nobody.

A command fire opens the task root's room before its run starts, so a command that reads room state, or launches an agent, finds one. A fire that the deadline, launch memory, or the task's run lock turns away opens nothing. A fire's run that does not end done appears in `rimz stats --assists` as a `workflow_start` event naming the task and checkout; a [quiet fire](#quiet-fires) records none.

### Placement

A task runs at the project root, wherever it is added from. `--root` names a project: a linked worktree's path, as the cwd or as `--root`, selects that worktree's project, and the receipt then adds `note: runs at the project root <root>, not <worktree>; add -w <name> to run each fire in a RimZ-owned worktree`.

`-w <name>` runs every fire in the RimZ-owned worktree of that name, for every kind, stored on the row as `worktree`. A launch task's fire passes `-w <name>` to its launch, which creates the tree from the configured base when it is missing, as it does from a shell. A shell task's fire creates a missing tree itself, recording `worktree.created`, and runs its command there; the task's conditions, launch memory and history stay keyed to the project root. A directory of that name that RimZ does not own fails the fire with the reason, recorded as a failed fire: a fire never asks. A hand-edited row whose `worktree` sits beside a `-w` in its arguments passes both, and the launch refuses the repeated flag, so each such fire records `error`.

A task that wants a clean tree for each fire makes it itself: a shell task whose command runs [`rimz worktree new`](./worktree.md#create-a-worktree) and then works in the [path](./worktree.md#print-a-worktree-path) of the tree it made, or an agent or team task with a bare `-w` in its arguments, which gives each fire's launch a fresh worktree as it does from a shell. A tree left from a task that cut one per fire before this RimZ is an ordinary RimZ-owned worktree: `rimz worktree list` lists it, and [`sweep`](./worktree.md#sweep-landed-worktrees) treats it like any other.

`--each-worktree` evaluates the trigger in each RimZ-owned checkout and fires there, and requires `--when` with `--repeat`, `--every`, or `--cron`: a signal fire reaches no checkout, and a one-shot would remove the task after its first checkout. A stored `each-worktree` row that does not repeat is refused at load with the same fix. Each checkout has its own condition and `--for` hold. The project root and unmanaged worktrees are excluded. Already-matching checkouts are eligible when the task is added. On GitHub this includes PRs already merged or closed; on Gitea the checkout's PR must have been observed open while the room ran. Room teardown and reset clear the PR cache, so Gitea loses that observation. This restriction applies to unscoped keys; branch selectors look up PRs in every state from first sight, limited to the newest 500 PRs on Gitea, and a merged PR whose head branch Gitea deleted can read unknown. Several eligible checkouts may fire on the same tick. Manual `loop fire NAME` targets the owned checkout containing your current directory and refuses elsewhere; it does not fire every checkout. Add the task with `--once-per-checkout` for a task that should succeed at most once per checkout ([launch memory](#launch-memory)).

### Triggers

| Shape | Flags | Repeats |
| --- | --- | --- |
| One-shot | `--at <TIME>`, or `--in <DUR>` | Fires once, then the task removes itself. |
| Interval | `--every <DUR>`, measured from the last fire | Yes. |
| Raw cron | `--cron "<expression>"` | Per the expression. |
| Poll-until | Stored rows only: `loop add` refuses `--until` | One run per cadence fire, until a command exits `0` or the deadline passes; a fire past the deadline records `expired`. |
| Signal | `--signal <NAME\|FAMILY.*>`, narrowed by `--match` | Once, or on every delivering signal with `--repeat`; see [signals](#signals). |
| Condition | `--when <EXPR>` with optional `--for <DUR>` | Once, or once per continuously true period with `--repeat`; a one-shot condition fire that left no run behind fires again five minutes later while the condition still reads true. |

A task fires once unless it repeats: `--every` and `--cron` repeat by nature, and a `--signal` or `--when` task repeats only with `--repeat`, stored as `repeat = true`. `--repeat` refuses with `--every` or `--cron` (`they repeat already`) and without `--signal` or `--when`.

The trigger values follow these rules:

| Value | Accepts |
| --- | --- |
| `--at` | A one-shot time: 24-hour `HH:MM`, `YYYY-MM-DD HH:MM`, or `<weekday> HH:MM` (such as `fri 09:30`). It resolves at add time to the first matching instant after now, so an `HH:MM` at or before the current minute fires tomorrow; a dated time in the past is refused. It refuses `--every`, `--cron`, `--in`, and `--signal`. |
| `--every` | An integer with `s`, `m`, `h`, or `d`, rounded up to whole minutes. `0` is refused. A daily or weekday time takes `--cron`. |
| `--cron` | Five whitespace-separated fields (minute, hour, day of month, month, day of week). Each field is `*`, a value, a range `a-b`, a step `*/n` or `a-b/n`, or a comma list of these. Months take `jan` to `dec` and days take `sun` to `sat`, case-insensitive and usable as range ends, and day `7` is Sunday as well as `0`. `@hourly`, `@daily`, `@weekly`, `@monthly`, or `@yearly` stands for the whole expression. When both day fields are other than `*`, a day either one selects fires. `L`, `W`, `?`, `#`, `@reboot`, an out-of-range or reversed value, and an expression whose day of month no selected month has (`0 0 30 2 *`) are refused, naming the token or form. |
| `--in` | A [duration](../cli.md#durations) in `s`, `m`, `h`, or `d`, greater than zero. It resolves at add time to a one-shot instant, rounded up to the next minute. |
| `--until` | Refused with `--until is no longer accepted; fire on a condition with --when`. A stored row keeps its deadline and still runs. |

`--every 1d` fires a day after the last fire and drifts with it; `--cron '0 7 * * *'` fires at 07:00. `--cron` and `--at` resolve in the top-level `timezone` setting, or the system zone when it is unset. `--in` is a delay from now and does not depend on the zone. A recurring clock task arms the first time a clock sees it and fires on the next occurrence after that, so a late room or a new timer never replays missed fires. A one-shot from `--at` or `--in` instead fires on the first clock that sees it at or past its instant, however late, and its run then reads `fired <DUR> late` in `loop show` and `loop logs` when it started two minutes or more after the instant. It lists as `once at YYYY-MM-DD HH:MM`.

### Window provider

A window term reads the windows of `--provider <kind>`. Without it, an agent task reads the kind its launch runs, and the receipt adds `windows: <kind> (from the launch), the room's account`; an agent launch that runs several kinds, or whose profile's tier can route to more than one kind, refuses, naming them and `--provider`. The account is `--account`, or else the launch's own `--account` when the provider is one of the kinds the launch runs; neither leaves the room's current account. `--account` without a provider refuses.

### Conditions

`--when 'team.stage=Done && ci=passed' --for 30m` waits for both readings to stay true for 30 minutes. Evaluation uses the clock tick, not signal history: CI can pass before or after the board reaches Done. The first tick arms without evaluating. A false tick resets the hold and an unknown tick keeps it; a fired `--repeat` task re-arms after a false tick. Holds start at the first true observation, never at a board timestamp, and reset when the room's runtime state is removed.

Expressions accept `!`, `&&`, `||`, and parentheses, in that precedence order. A term is `key=value1,value2` (either value) or `key!=value1,value2` (known and none of those values); a window key instead takes `key>=N`, `key<=N`, `key>N`, or `key<N`, with `N` a whole percent from `0` to `100`. A single value works too. Repeating `--when` ANDs the clauses. Whitespace between tokens is ignored; values use letters, digits, `_`, `.`, or `-`. Window percents are the only literals; there are no functions. A window key refuses `=` and `!=`, and for an exact percent `window.5h.left>=40 && window.5h.left<=40` reads the same.

| Key | Values | Reading |
| --- | --- | --- |
| `team.stage`, `team.stage[branch]` | `Done` and stages declared by any team in the effective configuration, case-sensitive | The `.rimz/team/blackboard.md` Stage line of the scoped checkout, or of the local checkout on the selected branch, primary or linked, owned or not. The branch's checkout comes from `git worktree list` rows cached for up to 60 seconds, so a checkout created or switched to the branch within that time can be unread. Unread when no local checkout is on the branch. |
| `ci`, `ci[branch]` | `passed`, `failed`, `pending`, `none` | The room sidebar's last-known CI result, or `none` when the last successful probe found neither PR CI nor branch CI. A live runnable loop condition or an Active workflow run with a forge-reading condition case or a default-scoped `ci`/`pr` wake keeps its scope checkout in the sidebar's probe set, with or without a pane. Removing the task releases its demand; workflow demand lasts until the run leaves Active. Without a sidebar source, including under the external timer, it is unread and the condition holds ([forge freshness](#forge-freshness)). |
| `pr`, `pr[branch]` | `open`, `merged`, `closed`, `none` | The room sidebar's last-known PR state for the checkout or selected branch. Like `ci`, live loop conditions and Active workflow runs with forge-reading cases keep their checkouts probed without panes. `none` when the last successful probe found no PR link, and unread without a sidebar source; branch CI alone does not imply a PR state. A PR in the merge queue, or removed from it, still reads `open`. |
| `pr.queue`, `pr.queue[branch]` | `queued`, `dequeued`, `armed`, `none` | The room sidebar's last-known merge-queue state for the checkout's or selected branch's open PR, GitHub only: `queued` while it is in the queue, `dequeued` once it has left the queue without merging, until it is queued again, re-armed (an auto-merge request enabled after the removal), merged, or closed. A push alone does not clear it. `armed` for an open PR in neither state that has an auto-merge request: one re-armed and waiting to enter the queue, or armed on a repository without a merge queue. `none` for an open PR with none of these, one that is merged or closed, any PR on Gitea, and when the last successful probe found no PR link; unread without a sidebar source. Pair it with `pr`: `pr=open && pr.queue=dequeued`. |
| `pr.mergeable`, `pr.mergeable[branch]` | `mergeable`, `conflicting`, `none` | GitHub's settled mergeability for the open PR's current head. Unread until GitHub settles an answer at that head, when only an answer for an older head is cached, on an open PR without open-PR facts, and without a sidebar source. `none` when the last successful probe found no PR link, or a merged or closed PR. The answer is keyed by head alone: after only the base moves, the answer settled at the same head keeps reading until GitHub settles again. GitHub only. |
| `pr.review`, `pr.review[branch]` | `draft`, `changes_requested`, `required`, `approved`, `none` | The open PR's review state, GitHub only. Draft wins over the review decision. Otherwise `changes_requested`, `required`, and `approved` reflect GitHub's review decision; a null decision reads `none`, and so does a PR the last successful probe found missing, merged, or closed. Unread when never fetched, when the review decision is unavailable on a non-draft PR, on an open PR without open-PR facts, and without a sidebar source. |
| `window.5h.left`, `window.7d.left` | A comparison with a whole percent, `0` to `100` | Percent left in the task provider's 5-hour or 7-day window: `100` minus the cached usage, projected to the tick, so a window whose reset has passed reads `100`, which lets the loop timer fire the task at the reset with no room open. Without an open room nothing refreshes the cache, so the projection misses usage since the last refresh. A limit the provider has lifted reads `100`. Unread without a cached reading of that window. The provider is [the task's provider](#window-provider), recorded as `provider` on the row. The account is the room's current one for that provider, or the task's account, whose reading is the last one stored for it: a room refreshes its current account for the provider and the accounts its agents run on, and, for Codex, each declared account. A task fires as soon as the window has the room its term asks for, which may be before the window resets. While a term that a refill can satisfy (`>` or `>=`, or `<` or `<=` under `!`) reads short, its reading shows when the window resets, when the provider reports one, in the configured timezone: `(resets YYYY-MM-DD HH:MM)` beside the reading on the `add` receipt's `now:` line, on the `loop show` and `loop explain` term lines, and in the `loop watch` state, `· resets YYYY-MM-DD HH:MM` in the WAITING FOR cell of `loop list`, and `resets YYYY-MM-DD HH:MM` in a ROUTINE row's NEXT cell when the task has no next time. A row shows it once a tick has seen the task. Where several terms show one, a row shows the latest. A met term, a term that waits for the window to be spent (`<` or `<=`), a lifted window, and a window with no reset time show none, and `--json` carries no reset. |

Only `team.stage`, `ci`, `pr`, `pr.queue`, `pr.mergeable`, and `pr.review` take an optional `[branch]` selector, such as `pr[feat/x]=merged`. A selector is nonempty and contains no whitespace, `[` or `]`; its spelling is preserved. `team.stage[branch]` reads the board of the checkout on that branch, as the table says. A forge selector reads that branch even without a checkout for it in the room. A live loop condition or an Active workflow run with forge-reading cases keeps its selected branches probed; demand ends when neither needs them. `ci[branch]` prefers CI of an open or merged PR, then the branch head's CI. Forge branch readings are unknown until the sidebar has probed them or when the scope checkout has a detached HEAD, and unread with no supported forge or under the external timer ([forge freshness](#forge-freshness)). These readings are not latched merge events. The room reads such a branch with no checkout as a branch row, which also emits `ci.*` and `pr.*` signals without a `path` ([built-in signals](#built-in-signals)).

A condition is true, false, or unknown. `=` is true when the reading is in the values and `!=` when it is not; over an unread key either term is unknown, and so is a window comparison. `!` keeps unknown unknown, `||` is true when either side is true, and `&&` is false when either side is false, so `ci=passed || pr=merged` is true on passed CI while `pr` is unread, and `!ci=failed` is unknown while CI is unread. `unknown` is not a value: `ci=unknown` refuses, since a term over an unread key is unknown by itself. Match `none` for a successful probe that found nothing. A task fires only on true; unknown keeps a running `--for` hold and the fired latch, and false resets them.

#### Forge freshness

A condition decides on forge readings only while they are fresh. A `ci`, `pr`, `pr.queue`, `pr.mergeable`, or `pr.review` key that is not fresh is unread and its terms are unknown; while the other terms do not settle the condition, it is unknown and holds: it does not fire, and a running `--for` hold keeps its clock. Every forge key is unread without a sidebar source (including under the external timer), while the scope checkout has no supported forge (it is never probed, so such a condition holds indefinitely, and a live one [needs you](#loop-list)) or a detached HEAD, while the latest probe has failed (the sidebar keeps its last readings, but a condition does not decide on them), and while the last successful probe is older than the task's last fire, or than its first sight before it has fired. A probe at the same instant counts as fresh. On an open PR whose head is known, bare `ci` is also unread when the CI verdict describes another commit than the PR head, or when the checkout's HEAD is definitely not on the PR, the same commit fence [workflow checks](../../guide/workflows.md#wait-on-the-outside-world) use; missing commit data skips only the comparison it prevents. That commit check runs only when the condition would otherwise be true. `loop show`, `loop list`, and `loop explain` apply the same rule and show each unread key's reason: `probed before the last fire or first sight`, `no sidebar source` (`readings paused (room closed)` when the room is closed), `no supported forge for this checkout`, `the checkout's HEAD is detached, so it has no branch to read`, the probe's own reason, or the commits that differ. `loop explain` reads no task, so it has no fire-stamp floor. A condition with no forge keys is unaffected. An idle repository's next probe can take up to five minutes, so a forge condition can hold that long after each fire.

A window condition is checked against the provider's cache at add, which refuses: a shell, workflow or team task without `--provider`, naming it as the fix; a kind that selects an exact managed account at launch (Qwen); a room whose account for the kind does not resolve (`rimz accounts list`); no cached reading of the window, or one whose reset has already passed (open the room's sidebar or run `rimz accounts --refresh`); and a provider without a window of that span.

A forge condition is checked against the open room's forge reading at add. When that reading already says the scope checkout has no supported forge (the scope is not in a git checkout or its repository is gone, the checkout has no `origin` remote or one RimZ cannot parse, or the `origin` host is neither `github.com` nor a host whose name contains `gitea`, `forgejo`, or `codeberg`; a supported host whose CLI, `gh` or `tea`, is missing or failing reads as a failing lookup instead), add refuses any `ci`, `pr`, `pr.queue`, `pr.mergeable`, or `pr.review` term, one joined by `||` included, with ``--when: `<key>` cannot be read in <checkout>: no supported forge for this checkout`` and what RimZ reads: a git checkout whose `origin` remote is on `github.com` (through `gh`) or on a Gitea, Forgejo, or Codeberg host (through `tea`). A checkout the sidebar has not read yet, a detached HEAD, a closed room, and an `--each-worktree` task add as before; a live condition whose checkout turns out to have no supported forge [needs you](#loop-list) instead.

Scope is the project root, wherever the task is added from; a delivery row keeps the checkout it was armed from, and a row's `dir` keeps its directory. For a branch selector, the repository is that scope checkout's origin repository. Unscoped keys keep reading the scope checkout; from the project-root checkout unscoped CI means trunk CI, so read a branch with a selector such as `ci[<branch>]`. `--when` conflicts with `--at`, `--every`, `--cron`, `--in`, `--signal`, and `--until`. `--for` requires `--when` and a positive duration in `s`, `m`, `h`, or `d`. `--project --when` is not yet supported; add without `--project`.

The receipt adds `trigger: when <expression>, for <duration>`, `scope: <checkout>`, and `now: waiting · team.stage: Review, ci: unknown` (`unknown · ...` when the known readings do not settle it, or `holding 0s/30m` when already true); a window term reads as `now: waiting · window.5h.left: 8`. Without a room, a CI or PR condition also prints `ci and pr readings come from the room's sidebar; the loop timer alone never sees them`.

### `loop explain`

`rimz loop explain <EXPR>... [--json] [--root <PATH>] [--provider <KIND>] [--account <NAME>]` evaluates the current readings without adding a task, starting probes, or writing state. Repeat positional clauses to AND them together, just like repeated `--when`. Scope is the caller's checkout, or the checkout `--root` names: a linked worktree is read as a wait armed there reads it, and the project root otherwise. The room whose sidebar source and caches it reads is the project root's. Use `--` before a positional value that is exactly a flag, including `-h` or `--help`.

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
| `--shell` | all | A shell task: the command after `--`. The default when no kind flag is given. See [what a task launches](#what-a-task-launches). |
| `--workflow <NAME>` | all | A workflow task: each fire runs `rimz workflow run <NAME>` with the arguments after `--`. |
| `--agent <SPEC>` | all | An agent task: each fire runs `rimz agents <SPEC>` with the arguments after `--`. |
| `--team <TEAM[#LANE]>` | all | A team task: each fire runs `rimz teams launch <TEAM[#LANE]>` with the arguments after `--`. |
| `--label <TEXT>` | all | Short description of 1 to 60 characters after trimming, on one line. Stored as `label`; `loop show` prints it beside the command. |
| `--timeout <DUR>` | `--shell` | Cap for the command, stored on the row; default `loop.default-timeout`, else `2h`. See [the command](#the-command). |
| `--each-worktree` | `--when` with `--repeat`, `--every`, or `--cron` | Evaluate the trigger and fire separately per RimZ-owned worktree. |
| `--when <EXPR>` | all | Repeatable state condition; see [conditions](#conditions). |
| `--for <DUR>` | `--when` | Continuous hold before firing. |
| `--match <KEY=VALUE>` | `--signal` | Require a top-level payload field to equal the value; repeatable. See [signals](#signals). |
| `--repeat` | `--signal`, `--when` | Keep the task after a fire, so it fires again. Refused with `--every` and `--cron`. |
| `--once-per-checkout` | repeating tasks | Store `once-per-checkout = true`: scheduled fires start at most one successful run per checkout; see [launch memory](#launch-memory). Refused on a one-shot, whose first fire removes the task. A re-add without it drops the key. |
| `-w`, `--worktree <NAME>` | all but `--each-worktree` | Run every fire in this RimZ-owned worktree, created when missing; see [placement](#placement). |
| `--provider <KIND>` | `--when` with a `window.*` term | The agent kind whose usage windows the condition reads, recorded as `provider`; an agent task defaults to the kind it launches ([window provider](#window-provider)). |
| `--account <NAME>` | `--provider`, or an agent task's default | A named account of that provider whose windows the condition reads, recorded as `account`; unset takes the launch's `--account` when the provider is one of its kinds, else follows the room's current account. It picks no login: the command's own launches choose theirs. |
| `--max-strikes <N>` | all | Consecutive failed fires before the task disables itself. Default `3`; `0` turns the gate off. |
| `--overlap <POLICY>` | repeating clock, signal, or condition tasks | `skip` (default), `coalesce`, `queue`, `replace`, or `allow` (agent tasks only); see [overlapping triggers](#overlapping-triggers). |
| `--overlap-limit <N>` | `--overlap queue` | Maximum waiting triggers, at least `1`, default `10`. |
| `--root <PATH>` | all | Project whose room hosts the task. Default `.`; inside an `empty-dir` agent's directory, the default is the checkout that agent was launched from. |
| `--project` | `--every`, `--cron`, or `--signal --repeat` | Write the task to the project's `.rimz/config.toml`; see [project tasks](#project-tasks). |

`--account` names an account the provider declares; add refuses a name the provider does not declare, a provider without named accounts, and an account whose home is missing or unhooked, each with the `rimz accounts` fix. The row stores the name alone. `rimz loop show` and `rimz loop list` name it beside the command, as `run cargo test · account work`.

### The receipt

`add` reads back the command or launch, where it runs, a shell task's timeout, what the launch's arguments ask for, the trigger in words, and the next fire for a clock:

```console
$ rimz loop add deps --agent claude -w deps --cron '0 9 * * mon' -- -p --timeout 1h 'Check for outdated dependencies.'
added loop task `deps`
action: launches `rimz agents claude -w deps -p --timeout 1h 'Check for outdated dependencies.'` in worktree `deps` (created at the first fire if missing)
launch: worktree `deps` · prompt found · time limit 1h
trigger: fires cron `0 9 * * mon`
next fire: 2026-11-23 09:00 (in 6d)
live while a room for /home/you/code/app is open
no room is open there; start one with `rimz start`, or use `rimz loop timer install` to fire without one
```

The `launch:` line reads the launch's arguments back: the worktree a fire runs in (`a new worktree each fire` for a bare `-w`), for an agent or team task `prompt found` or `no prompt`, and for an agent task the `-p` run's `time limit`, `none` when it has no `--timeout`. A workflow task's line names only the worktree, and a launch with nothing to name prints none. A task whose window provider or account came from its launch adds `windows: claude (from the launch), the room's account` ([window provider](#window-provider)). A repeating agent or team task that is not a `-p` run and sets neither `--overlap` nor `--once-per-checkout` adds a note; under `--once-per-checkout` the task launches once in a checkout, so no idle agent holds a later fire. An agent task's reads ``note: an agent left idle holds later fires; add `-p` for a run that ends itself, `--overlap replace` to stop the old agent at each fire, or `--overlap allow` to launch beside it``; a team task, which refuses `allow`, gets the same note without its last clause.

A one-shot's trigger line ends `; then removed`. A task added from a linked worktree adds the `note: runs at the project root` line described under [placement](#placement).

Replacing a task whose arming, pause, or strike state was set prints `arming: reset` right after the first line, and an `--project` add prints its [trust result](#project-tasks) there too. When no room is open and the timer is active, the last line reads `no room is open there; the loop timer will keep time`.

## Where tasks are stored

Each task lives in one of three stores:

| Store | File | Holds | SOURCE |
| --- | --- | --- | --- |
| Machine | `~/.rimz/loop.toml` | Tasks that repeat (`--every`, `--cron`, or `--signal` or `--when` with `--repeat`), added without `--project`. | `machine` |
| State | `~/.rimz/ws/<workspace-dir>/records/loop-instances.json` | Tasks that fire once (`--at`, `--in`, and `--signal` or `--when` without `--repeat`), stored `--until` rows, and the [delivery rows](#delivery-rows) that `rimz shell`, `rimz watch`, and team bindings arm. Rows retire themselves. | `state`, or `team <instance>` for a team binding |
| Project | `<root>/.rimz/config.toml`, `[tasks.<name>]` | Tasks added with `--project`. | `project`, or `project · untrusted` / `project · stale` while trust is missing |

Adding a state task requires an existing room: run `rimz start` in its root first, or pass `--root <room>`. Machine and project definitions can be added without a room and create no room state.

A machine task takes precedence over a same-named state task only within the same root. A machine task rooted elsewhere neither hides the room's state task nor is removed when you add that state task. `loop show`, `remove`, `rename`, `pause`, `fire`, and `stop` in that room address its state task; the machine task remains reachable from its own room or outside any room.

Adding a standing machine task also requires that an existing machine task of that name have the same resolved project root. A cross-project collision refuses without changing either task, names the stored root, and suggests another name or `rimz loop remove <NAME>` from the owning project. Re-adding from that project, or with `--root <owning-project>`, still replaces its task. A machine row with a missing, empty, or relative root is skipped at load with a warning and does not run; `~` and `~/` expand against `HOME` before the absolute-path check. Adding over it refuses when its resolved root differs too; run `rimz loop remove <NAME>` from a fresh directory outside any repository or RimZ project before reusing its name, so it cannot select a same-named room or project task instead. Rename and remove still reach a skipped row by name, and rename preserves its root as written.

A row is strict: it takes the trigger, placement, overlap, strike, and action fields `loop add` writes, and any other key refuses that row with ``loop task `<name>` is not a row this RimZ takes (<detail>); delete the task and re-add it with `rimz loop add` ``. A row that sets `wait` beside `kind` or `args`, sets neither `kind` nor `args`, is a shell row (no `kind`) with a `target` or without `timeout`, or is a launch row with no `target` or with a `timeout`, refuses with its own message and the same fix. A row an older RimZ wrote with a `command` or `fresh-worktree` key refuses as an unknown key. A refused row refuses alone: the other rows of its file load and fire, `loop add`, `remove`, and `list` keep working, and the refused row never fires: `loop list` puts it under NEEDS YOU as `invalid: <message>`. A row that does not decode reads `refused row` where its trigger would be, and `loop show` prints `invalid: <message>` in place of its trigger and `<invalid>` as its action.

`loop list`, `enable --all`, and `disable --all` cover machine tasks plus the state and project tasks of the project resolved from the current directory or `--root`. Enable, disable, pause, and strike state belong to this machine: machine-wide for machine tasks, per project for state and project tasks.

A machine or state task that an agent adds records who booked it, as `booked-by`: the calling agent, then each agent that started the one before it (its launcher, or a subagent's parent), nearest first, each as `kind` and `agent_id` (the launch id, else the session id). When an agent on that chain was started by a loop task's fire, the chain that task carried when it fired ends it, even after the task was consumed by that fire, removed, or added again under the same name; every agent the fire launches, or a shell fire's command launches, carries it, a `--detach` launch and a team's members included. The chain also ends at an agent the room's rows no longer hold, at an id it already holds, and at 16 agents. A task added from a user shell, by a caller the task's room does not know (an explicit `--root` in another room included), or with `--project` records none, and recording it never changes whether or where the add writes. Re-adding a name replaces the record, and `rimz loop show` prints the booker. `booked-by` is machine state: a project task that carries it is refused. A one-off task that carries `booked-by` and launches an agent or a team (`--agent` or `--team`) shows as a [booked launch](../../interface/sidebar.md#booked-launches) on the booker's sidebar card, or on the nearest living agent up its chain.

### Project tasks

`--project` writes a task that ships with the repository. It must repeat, so it needs `--every`, `--cron`, or `--signal` with `--repeat`, and it refuses `--until`, `--when`, and `--each-worktree`. Hand-written project tasks also reject `when` and `for`. `--root` must lie inside the project of the current directory. A project task stores no `root` or `dir` and always runs at the project root.

A project task runs unattended only when both hold:

- The project is trusted on this machine ([project trust](./hooks-trust.md#project-trust)); a project task's `command`, `timeout`, and every other field that decides what or where it runs enter the trust hash.
- The task is enabled here with `rimz loop enable <name>`. A task pulled from a repository starts disabled; `loop add --project` enables it on the machine that ran the add.

A trusted project task wins over a machine task of the same name. While the project is untrusted, the project task does not fire and the machine task keeps running.

`add --project`, and `remove` or `rename` of a project task, change the trust surface. What they print depends on trust before the write:

| Trust before | Result |
| --- | --- |
| `trusted`, or no project config | Trust is granted again for the new config. `add` prints `trust: granted — task enabled and ready to fire`; `remove` and `rename` print `trust: kept`. |
| `untrusted` or `stale`, stdin a terminal | The surface diff and `grant trust now?`. Accepting grants trust. |
| `untrusted` or `stale`, no terminal, or declined | ``trust: <state> — project tasks stay inert until you run `rimz trust grant` (review with `rimz trust`)``. |

## Signals

`--signal <SELECTOR>` subscribes a task to one signal name (`ci.failed`) or one whole family (`'ci.*'`). Without `--repeat`, the first fire that delivers spends the task; with it, the subscription listens for as long as the task exists and has no deadline. `list` and `show` render it as `on <selector> [k=v]`, with `listening` in the NEXT column.

A subscription observes its whole family. A task on `ci.failed` fires on `ci.failed`, records `skipped` for a sibling such as `ci.passed` without running its command, and ignores other families. A `'ci.*'` selector fires on every member. `--match KEY=VALUE` requires a top-level payload field to equal the value; repeat it for an all-of set. `--match KEY='*'` requires only that the payload carries the key, including a `null` value. Only the whole value `*` is a wildcard; `feat/*` is literal. Blank match values are refused. A signal that fails a match is ignored: the task records nothing. `rimz events emit` names it as excluded when its selector names the signal or its family, and not when it names a sibling (a task on `ci.failed` whose match `ci.passed` fails). `ci.finished` and `--match conclusion=…` are refused at add time; use `ci.passed` or `ci.failed`.

Signals come from `rimz events emit`, forge state changes on watched checkouts and held branches, trunk ref moves, managed worktree creation and removal, agent and team lifecycle transitions, team stage flips, workflow run moves and endings, and `rimz shell` watchers. The [signal lines table](./events.md#signal-lines) lists every name and source.

Firing runs in the emitting process. A signal reaches only the tasks subscribed at that moment; a later subscription never receives it. A busy task can retain its own trigger through its [overlap policy](#overlapping-triggers). A disabled or paused task, or a project task whose project is untrusted, skips the signal.

### Built-in signals

RimZ emits these signals from what the room already watches.

| Signal | Fires when | Payload |
| --- | --- | --- |
| `ci.passed`, `ci.failed` | The checks on a commit of a watched checkout or a held branch settle on success or failure, with or without a pull request. | `path` (none for a held branch), `branch`, `repo`. `head` (the commit the checks ran on) and `checks_url` when the commit is known. `number` and `url` when a pull request exists. |
| `pr.merged`, `pr.closed` | The pull request of a watched checkout or a held branch leaves the open state. | `path` (none for a held branch), `branch`, `repo`, `state`, plus `number`, `url`, `head`, and `checks_url` when known. |
| `pr.opened` | A successful probe finds a new open pull request after monitoring has already observed the branch successfully. | The `pr.merged` payload with `state: "open"`. |
| `pr.behind`, `pr.conflicted` | GitHub only. An open pull request falls behind its base, or has confirmed conflicts, including at the first reading of a PR on a branch the room had already observed successfully. Fires again for a changed PR head while still in that state, never for repeated readings of the same head. | The `pr.merged` payload with `state: "open"`, plus `pr_head`, `base` when known, and `behind_by` for `pr.behind`. `head` stays the local worktree HEAD. |
| `pr.queued`, `pr.dequeued` | GitHub only. An open pull request enters the merge queue, or leaves it while still open. See [`rimz events`](./events.md#reserved-families) for first readings, re-arms, and merges out of the queue. | The `pr.merged` payload with `state: "open"`, plus `pr_head` and `base` when known. `pr.queued` adds `queued_at`. `pr.dequeued` adds `dequeued_at`, `reason` when GitHub supplies one (`failed_checks`, `manual`), and `queue_checks_url` when GitHub names the queue commit. |
| `trunk.moved` | The trunk ref moves to a different commit. | `trunk`, `from`, `to`, `repo`. |
| `worktree.created`, `worktree.removed` | A managed worktree is created or removed. A reused worktree, or one rolled back after a failed creation hook, emits nothing. | `name`, `branch`, `path`, `repo`, plus `from_pr` when present. Creation adds `base` when known. Removal adds `branch_deleted`. |
| `agent.started`, `agent.idle`, `agent.waiting`, `agent.failed`, `agent.ended` | One agent's lifecycle transition. `agent.waiting` fires at the hook, also during Codex automatic review, before its 60-second attention delay. | `kind`, `session`, `status`, `errored`, plus `handle` when the card has a name. |
| `team.idle`, `team.waiting`, `team.failed`, `team.ended` | A [team](./teams.md) cohort settles: every member at rest with nothing queued and no wait armed, one member waiting on input, one member's turn failing, or the last member ending. | `team`, `instance` (`forge#feat-x`), `member`, and `members` with each handle and status. A handle can be a session id. |
| `team.stage` | A team hands its stage off, and again when a resumed owner takes its stage back. | `team`, `instance`, `to`, `by`, `board`, plus `from` on every flip that did not open the board, `owner` when the stage has one (`Done` has none), and `note` when the flip carried one ([payload](./teams.md#the-teamstage-signal)). |
| `workflow.moved`, `workflow.waiting`, `workflow.failed`, `workflow.ended` | A [workflow](./workflow.md) run moves to another node, starts to need you, fails, or ends, once per committed write ([run signals](./workflow.md#run-signals)). | `run`, `workflow`, `node`, `checkout`, plus `from` for `workflow.moved` and `outcome` (`done`, `failed`, `canceled`) for `workflow.ended` and `workflow.failed`. |
| `shell.<task-name>` | A [`rimz shell`](./shell.md) watcher reaches a check-in or its final outcome. A watcher an older RimZ started sends `wait.<task-name>`. | Empty. The outcome travels in the delivered message. |

A held branch is one the room reads with no checkout of it: a live `ci` or `pr` subscription whose `--match branch=<name>` names it literally (not `*`) holds it, and so does a live condition or Active workflow that reads `ci[<name>]` or `pr[<name>]`. A paused or disabled task holds nothing, and a team role's signal binding holds its branch the same way. Its signals carry `branch` and `repo` but no `path`, so a subscription with a `path` match never hears them, while one with no match, a `repo` match alone, or a `branch` match does. A `ci` or `pr` task added without a match keeps an empty one; only a team role's binding gets a default `path` match, its member's checkout. Its first successful reading, and any failed one, emit nothing. A change of the branch's forge reading fires once whether or not a checkout sits on the branch: a checkout speaks for it only when it emits the same change in the same pass, for the same pull request or the same CI verdict at the same commit. A checkout whose reading failed, or that reads another commit (a root on a stale `main`), leaves the signal to the branch. When the branch's commit is not known yet, as in a reading an older RimZ wrote, both may signal: a double signal is the lesser fault than a missed one. A held trunk branch emits only `ci.*`. `head` and `checks_url` come only with a pull request's checks.

A dequeued pull request never turns its own checks red, so `ci.failed` stays silent for it. Read the standing queue state with the [`pr.queue` condition key](#conditions).

### The trigger's evidence

A fire hands its command one line of evidence, `RIMZ_LOOP_TRIGGER`, and a `rimz` launch in the command appends the same line to its prompt. A workflow task's fire hands the line to its run: each command and check of the run reads it as `RIMZ_LOOP_TRIGGER`, and each agent or message step gets it at the end of its prompt or message, after any `--prompt` the task passes. The line holds the rule, then the signal's subject and fields joined by ` · `. A `smoke` task on `deploy.finished`, fired by `rimz events emit deploy.finished --json '{"env":"prod"}'`, reads:

```text
Rule `smoke` fired here. deploy.finished · env: prod
```

A condition fire lists its readings instead (``Rule `ship` fired here. team.stage: Done``). A clock fire and a manual `loop fire` have no event, so the variable is absent and a launch gets its own prompt alone; `loop fire --signal <name> [--json <object>]` gives the fire the line that signal would, whatever the task's trigger. The run record keeps the signal name and payload, truncating a payload over 4 KiB. `rimz loop logs <task>` prints the payload as one JSON line under each run's `signal:` line, and still reads the retained runs of a spent task. For a task still registered, `rimz loop show <task> --json` prints those records whole.

A built-in signal carries only what `gh`, `tea`, `git`, or `rimz` cannot answer from the branch and `#N`; the worktree path, the repository, and the PR and checks URLs stay out of the line. Each signal has its own subject:

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
| `workflow.moved` | `workflow.moved <run> · <from> -> <node>` |
| `workflow.ended` | `workflow.ended <run> · <outcome>` |
| `workflow.waiting`, `workflow.failed` | `workflow.waiting <run>` |

A segment whose field the payload lacks is left out with its separator, so a branch with no pull request reads `ci.failed on check-multi @88824c2`. A commit prints its first seven characters. `@<head>` is the commit the CI verdict is about, not an unpushed local HEAD; it is left out when the forge names no commit. Any other name in a built-in family prints its family's leading segment alone. `pr.dequeued` adds `queue checks: <url>`, since nothing else names the queue commit. A signal outside the built-in families adds one `key: value` per top-level payload field, in key order: a string bare and anything else as compact JSON. A key or a string holding a line break prints JSON-quoted.

Project tasks may use `--signal` and `--match`, and `--repeat` satisfies their repeat requirement.

## Delivery rows

A delivery row wakes a running agent instead of running a command. `rimz loop add` writes none: [`rimz shell`](./shell.md) and [`rimz watch`](./watch.md) arm them for the calling agent, and a team's [role bindings](./teams.md#signal-bindings-at-launch) arm them for its members. They live in the state store, keep the checkout they were armed from, and read like any other task in `loop list` (under WAKE-UPS), `loop show`, `loop logs`, `loop stop`, and `loop remove`. A delivery starts no workflow run: it parks a message from `@rimz` for its pinned agent, which a restart or molt keeps, and a fire that finds the agent gone records `target gone` and removes the row. When the pinned agent ends, is lost, or is stopped, its rows retire.

A signal delivery parks until the target's current turn ends, with `Type: SIGNAL`, and opens with a subject line from the table above and a verdict line naming the row:

```text
waited on ci.failed on check-multi #729 @88824c2
fired after 12m [ci-fix]
```

A signal outside the built-in families prints its `key: value` field lines under the verdict, and a condition delivery prints its readings by the same rule, with `unknown` for a reading that had no value. A manual `loop fire` of a signal delivery delivers `fired by hand [<task>]` with no field lines; with `--signal` it delivers what that signal would (`waited on <subject>`, the verdict, and its field lines), and a delay row an older RimZ armed reads `waited on <subject>` in place of `waited <delay>`. The shells and watches an agent arms for itself steer instead ([a shell's message](./shell.md#the-delivered-message), [a watch's](./watch.md#the-delivered-message)).

## Gates and strikes

A command fire passes its gates in this order before its run starts: a watch's verdict, the deadline, [launch memory](#launch-memory), the task's run lock, then the [overlap rule](#overlapping-triggers) against the task's live run. Their refusals spend nothing, and every gate before the overlap rule turns a fire away before it opens the room. A delivery passes the room's and its account's budget caps ([what a cap blocks](./budget.md#what-a-cap-blocks)) first, recorded as `budget skipped`, then the run lock, the deadline, and a watch's verdict. A gate skip never counts as a strike. `loop list` shows today's spend in LAST and `loop show` on its `spend` line when the task's runs recorded a cost.

### Strikes

After `--max-strikes` consecutive strikes (default `3`), the task disables itself, the process that recorded the last strike prints ``loop `<name>`: disabled after N consecutive failed fires; enable with `rimz loop enable <name>` ``, `loop list` shows `disabled after N strikes` under NEEDS YOU, and `loop_disabled` notification handlers fire. `rimz loop enable` clears the count.

| Run result | Effect on strikes |
| --- | --- |
| `failed` (the command exited non-zero, died by a signal, or outlived its timeout), `error` | Strike. |
| `completed`, `delivered`, an agent or team fire's `launched` (a `-p` launch included) | Resets the count. |
| a workflow fire's `launched`, which carries its run id | Neutral: the run's own outcome decides once it ends. Each run counts once, however often its outcome is reported. |
| `skipped` (a sibling signal, or a watch verdict `on` filters out) | Neutral. |
| `overlapped`, `deferred`, `canceled`, `expired`, `target gone`, `budget skipped` | Neutral. |
| `start failed`: the fire left no run behind, either the runner was never spawned, or it exited before it could record anything. An interval or cron occurrence is spent without a run; a one-shot (`--in`, `--at`) stays and is dispatched again five minutes later, as is a one-shot condition (and `rimz watch`) that still reads true | Neutral. Shown as a failure, but a fire that ran nothing never auto-disables the task. |

History an older RimZ wrote can also hold `timed out`, `verify failed`, and `budget exceeded`, which strike; `surplus skipped`, `account skipped`, `throttle skipped`, `capacity held`, `takeover blocked`, and `replaced` are neutral.

## Enable, disable, and pause

`loop enable <name>...` arms tasks on this machine and clears disables, pauses, and strike counts. `loop disable <name>...` holds them until the next enable. Both resolve every name before changing any task: an unknown name refuses the command without changing any task. Both take `--all` instead of names, covering machine tasks plus the state and project tasks of the current project. Each task prints one line: ``loop `<name>`: enabled`` (with `· next in 2h` when a clock fire is due), ``loop `<name>`: already enabled``, or ``loop `<name>`: disabled``.

`loop pause <name> --for <DUR>` holds a task for a bounded time (`s`, `m`, `h`, or `d`, greater than zero) and prints ``loop `<name>`: paused; resumes in 2h (Sat 14:00)``, the clock in the configured zone. A disabled task refuses a pause; `disable` owns the indefinite hold.

All three write machine-local state and never edit a task definition or a trust-hashed project config. Enabling, or reaching the end of a pause, starts the schedule fresh from that moment, so fires missed while held never replay.

## Fire and stop a run

`loop fire <name>` fires the task now, with the scheduled runner's gates except launch memory, and records the fire. It runs a disabled or paused task anyway, after saying so, and leaves one-shots and subscriptions in place. A false condition prints `condition not met (k of n); running anyway, task stays armed`, and an unknown one `condition unknown (k of n); running anyway, task stays armed`; expressions without a conjunction count omit `(k of n)`. This cache-only observation uses the selected checkout for fan-out tasks and never changes arming or records scheduled condition evidence. Met conditions and non-condition tasks add no notice.

`--signal <name>` fires the task as if that signal had triggered it: the fire carries the signal's evidence, the `RIMZ_LOOP_TRIGGER` line for a command and the line appended to a launch's prompt, as a real signal of that name and payload would. `--json <object>` is the signal's payload, the same top-level JSON object of at most 64 KiB [`rimz events emit --json`](./events.md) takes, not an output switch; without it the payload is empty. The name is one signal name, a reserved family such as `ci.failed` included; a selector such as `ci.*` is refused. Nothing is emitted: `rimz events list` does not show it, and no other task or workflow run subscribed to the name wakes. Any task takes it, whatever its trigger. On a task whose own signal trigger would not deliver it, the fire prints one notice and runs anyway: ``signal `<name>` is outside this task's trigger `<selector>`; running anyway, task stays armed``, or `signal payload fails this task's --match <key>=<value>; running anyway, task stays armed`, naming each `--match` field the payload fails, one it lacks included. The run's history row is a `manual` fire that keeps the signal's name and payload, which `loop logs` prints; `loop show` RECENT RUNS labels it `manual`. A bad name or payload, or `--json` without `--signal`, refuses with exit `2` before anything runs.

A fire that starts a run prints the command, `launched`, and the run's `rimz workflow show` command, then returns while the command runs on in that run; when the run already ended as a [quiet fire](#quiet-fires) and its home is gone, it points at `rimz loop show <name>` instead:

```console
$ rimz loop fire nightly
nightly — run `sh -c 'git status --short | wc -l'`
✓ launched in 211ms
  → rimz workflow show 'nightly#room'
```

For an untrusted or stale project task on a terminal, `fire` asks `grant trust and fire?` and refuses if declined; without a terminal it refuses. A scheduled fire of such a task is always refused.

| `loop fire` exit | When |
| --- | --- |
| `0` | The fire started a run or recorded its launch's outcome, a gate or overlap skipped it, or a delivery row delivered. A shell or workflow run's own outcome reaches `loop show` and `loop logs` once it ends; a `-p` run's never does. |
| `130` | Ctrl-C interrupted a watched command's check. |
| `1` | An error, or a trust refusal. |
| `2` | A usage error, such as a selector given to `--signal` or a `--json` payload that is not a JSON object. |

A task whose previous run is still active starts no other concurrently, unless it is an agent task under `--overlap allow`. By default it records `overlapped`; retaining policies record `deferred` unless the queue is full. The printed line and the row's error name what holds the task as `loop show` lists it: `agent @<handle>#<lane>` (a team's agents together), `supervised run <id>`, `workflow run <id>`, or a launch still starting by its pid. `loop stop <name>` then stops everything alive that carries one of the task's fire ids, through each launch's own stop: a workflow run (the task's own run, which ends its command, or one a command started) as `rimz workflow cancel` does, a `-p` run as `rimz agents stop <run-id>` does, and an agent as `rimz agents stop @<handle>#<lane>` does, a team being its agents. It prints ``loop `<name>`: stopped · run <id>`` for each run and ``loop `<name>`: stopped · @<handle>#<lane>`` for each agent. Alive is the durable record: a run not yet terminal, an agent whose end is not recorded (an idle agent is alive). A launch handed off without `RIMZ_LOOP_FIRE` carries no fire id and keeps running. It exits `1` naming every launch it could not stop, after trying them all.

When a runner still holds the task's run lock (a fire starting its run, or a delivery in flight), `stop` marks the run lock's holder stopped, waits up to 5 seconds for the lock, sends SIGTERM to a remaining holder, and waits again. It prints ``loop `<name>`: stopped`` (with the run id, and `· SIGTERM` when it signaled) or `no active run`, and exits `0`. When a stopped run's agent was not confirmed stopped, it prints the `stopped` line and then exits `1` with the reason and the command that finishes the stop, `rimz --root <root> agents stop <run-id>`, which names the task's room so that it runs as printed from any shell; a second `loop stop` finds no active run and leaves the agent as it is. A run launched with `--keep` is left its pane: `stop` cancels it, its agent stops, and the pane stays open. When the lock is still held it exits `1` with the holder's PID and the lock path; SIGKILL is left to you. A fired one-shot (`--at`, `--in`, or a `--signal` or `--when` task without `--repeat`) gives up its task row when its run starts; while that run is active, `stop`, `show`, and `logs` still find it by name from inside its project, and `list` and `watch` there keep a row for it.

The stopped run records one `canceled` row. If triggers were waiting, an extra stop receipt line reports their count, says the next starts now, and names `rimz loop pause <name>` to drop them.

### Launch memory

A row that sets `once-per-checkout = true`, which `loop add --once-per-checkout` writes, starts at most one successful run or launch per checkout from its scheduled fires, whatever its trigger; the field defaults to `false`. A scheduled fire records the checkout when it creates the run, the live run keeps the checkout taken, and the way the run ends decides whether the record stays:

| The run | Remembered after it ends |
| --- | --- |
| Done: a shell command exited `0`, a workflow run ended done, or an agent or team launched, a `-p` run included whatever it does after | Yes |
| Failed: a command exited non-zero, died, or timed out; a run failed; a launch ended in `error` | No |
| Canceled, or a neutral skip | No |

A checkout that is not remembered can start a run again at the task's next scheduled fire; a scheduled fire on a remembered checkout records nothing. `loop remove` clears the task's memory. A redefinition (`loop add` under the same name) cancels the task's live runs and forgets their checkouts, while a checkout whose run ended done stays remembered; `loop rename` moves the memory. A manual `loop fire` ignores the memory and replaces the checkout's entry once its own run starts.

### Overlapping triggers

Every command fire starts a workflow run, and a task holds one live run per checkout. `--overlap` chooses what a scheduled trigger does when it passes the deadline and launch memory and finds the task's run still live, or another fire of the task starting its run:

| Policy | Busy trigger |
| --- | --- |
| `skip` (unset) | Records `overlapped`, naming the live run, and is spent. |
| `coalesce` | Records `deferred` and keeps one waiting trigger, merging earlier waiting triggers into the newest. |
| `queue` | Records `deferred` and waits in firing order. At `--overlap-limit` (default `10`), records `overlapped` with `queue full (N/N), skipped`. |
| `replace` | Records `deferred`, retains the newest waiting trigger, and cancels the live run, which ends its command, or stops everything alive the blocking fire launched. The canceled run records `canceled`, which is strike-neutral. The trigger starts once nothing of the old fire is alive. |
| `allow` | Launches anyway, beside everything earlier fires launched that is still alive, and records the launch as any fire does; nothing waits and nothing is stopped. Agents pile up, one per fire, until they are closed or `loop stop` stops them all, and with a named worktree they share one checkout. A spec of two or more agents in a reused named worktree or a pull request's worktree is one cohort there instead: while an earlier fire's cohort is live, a fire hands its prompt to that cohort's leader and records `launched` without opening agents, or records `error` when the leader is not live. Another fire of the task still starting its launch (holding the run lock) records `overlapped`. |

Waiting triggers survive runner crashes. They run once the live run's report lands, with a scheduler backstop that drains them on its tick. Each reruns arming, trust, the deadline, and launch memory at its own start time. Paused or disabled tasks drop their waiting triggers without another history row; another gate's refusal consumes one trigger and records that gate's result. Removing or redefining a task clears its waiting triggers, and renaming moves them. If hand-editing its file removes a task, makes its action invalid, or leaves it without a retaining `overlap` (unset, `skip`, `allow`, or a shape that refuses the setting), the next drain drops its waiting triggers without another history row. `loop stop` does not clear waiting triggers; pause or disable the task to prevent them running.

Replace cancels the live run as `rimz workflow cancel` does, and starts the newest trigger only once that run is terminal, so a cancel that returns before the run ends never overlaps two runs. Every run a replace cancels records `canceled`, which never strikes, so triggers that keep arriving before a run finishes cannot disable the task. Use `--overlap coalesce` to let each run finish. A cancel that cannot end the run leaves the newest trigger waiting. A run that finishes on its own before the cancel reaches it keeps its own result, and the newest trigger runs after it.

Newest and oldest go by firing time, not by the order busy runners reach the task: a signal trigger fired when its signal was emitted, so every task woken by one emit shares that time, and a clock trigger fired when its runner started. Triggers that fired at the same instant keep the order they arrived in. Coalescing keeps the newest signal name and payload. If both payloads have the same signal name, carry `from` and `to`, and agree on every other key, it preserves the oldest `from`. Otherwise the newest payload stands whole. The served run records `absorbed` for the number of earlier triggers folded in (omitted when zero). Clock triggers merge only their count. Queued triggers keep their individual payloads. A run started for a waiting trigger hands its command that trigger's [evidence](#the-triggers-evidence), the same signal and payload `rimz loop logs` prints for the run.

Every explicit policy, including `skip`, is refused for tasks that fire once (including a deadline row), delivery rows, and `--each-worktree` tasks: a task that fires once retires its row, a delivery has no run to overlap, and an each-worktree task fires per checkout while its waiting triggers are kept per task. `allow` is also refused on shell and workflow tasks, which hold one unfinished run per checkout, on team tasks, whose fires launch the same team into one lane, and beside `once-per-checkout = true`, whose launch memory would hold every later fire; `loop add` refuses before it stores anything, and a hand-written or project row reads as invalid and never fires. `--overlap-limit` is refused without `queue`, or at zero. Project tasks accept the same settings. Manual `loop fire` never enqueues: it refuses a busy task with the stop hint, except under `allow`, where it launches.

## Read run history

Every attempted fire appends one record to `~/.rimz/logs/loop-runs.log.jsonl`, except a fire that starts a workflow run: that run's one record is its report, appended when the run is collected after it ends, and until then `show` and `logs` list it as a `launched` row read from the run itself. A quiet fire appends nothing. A retained trigger records `deferred`, then its served run records a separate terminal outcome. A busy or empty drainer, or a drained trigger dropped because the task is paused, disabled, missing, invalid, or no longer retaining, writes no row. `show` searches known rooms, while `logs` reads records for the current project. Both keep reading a task's history after the task is removed.

### Quiet fires

A quiet fire is a task's run that ended done: a command fire whose command exited `0`. An agent or team its command launches is the command's own launch, so it does not make the fire's run loud. Its collection forgets it: no history row, no `workflow_start` assist, and no run home, so `rimz workflow show <its id>` answers as for an unknown id. The task keeps one quiet summary instead, in `~/.rimz/loops/tasks.json`: how many quiet fires there were, how many in a row since the task's last failure, and the stored `completed` row of the last one, with the exit code and output tail of its command. A failure row resets the streak and keeps the count and the last output; the summary moves with a renamed task and goes with a removed or replaced one, while `loop enable`, which clears strikes, keeps it. A one-shot's quiet run keeps its one `completed` row and its run home, since its task row is gone by then.

### `loop list`

`loop list` shows the whole current room, including its worktrees. `--all` shows every known room, the caller's first, and unfolds older DONE entries and WAKE-UPS. Outside a project, in a directory with no recorded room, tasks, or one-off history, it shows every room, still folded unless `--all` is explicit. An explicit `--root` keeps that root selected. Known rooms come from machine tasks, stored instance tasks, and the caller's root. A project with only project-config tasks is not discovered unless it is the caller's. Other rooms' attention tasks appear on an `elsewhere:` line in the default view; healthy tasks elsewhere stay hidden.

A machine task with a missing, empty, or relative root appears only as a `warning:` on stderr, naming the task, file, problem, and fix: set an absolute `root` or run `rimz loop remove <NAME>`. It creates no task row or room group. This warning also appears with `--all` and `--json`; the list still exits 0.

Each heading names the home-relative root, whether its room is open, the task count before collapsing, the next clock fire when present, and nonzero spend today. Without a room, it explains which clocks and forge signals wait for `rimz start`, and whether the optional clock timer is active.

Empty sections are omitted:

- NEEDS YOU names missing checkouts of tasks that have not fired, strike-disabled or failing tasks, holds needing a fix, trust blocks, invalid triggers and [refused rows](#where-tasks-are-stored), tasks with a parked run, unfired one-offs waiting on a pull request closed without merging, and live condition tasks with a forge key their checkout cannot read, with commands addressing the named tasks. A task is failing when its latest acting result failed and no newer run of it is in progress: a held run lock, or an active workflow run of the task in that room created after the failed run ended, as a `loop fire` retry or a scheduled fire starts; LAST says how its row reads meanwhile. A parked run, an `each-worktree` sibling started before the failure ended, and a retry whose own failure has been reported leave the task failing. A closed pull request reads `pr[<branch>] closed without merging` and points to `loop remove`. It holds for a one-off or wake-up whose condition has a `pr[...]=merged` term outside any `!`, reading `closed`, that the condition cannot do without: `pr[a]=merged && pr[b]=merged` with `a` closed qualifies, `pr[a]=merged || pr[b]=merged` does not. It ranks after failing and before a lost watcher, and a parked run does not replace it. It clears when the reading changes (reopened, merged, or unread). Only a condition trigger reads the pull request, so a `--signal pr.merged` task stays a plain wait, and with the room closed every forge key is unread, so the cause is absent. A live condition task, one-off or repeating, whose `ci` or `pr` key is unread because its checkout has no supported forge reads `<key>: no supported forge for this checkout` and points to `loop show`. It ranks after a closed pull request and before a lost watcher, and a parked run does not replace it. It clears when the reading changes, and is absent with the room closed and for a detached HEAD, which reads unknown without needing you. A parked task reads `<n> run(s) parked: <address> <node> <reason>[: <text>] <age>` and lists the remaining parked runs below the table, each summary once. Summary ages start at the park, not node entry. Parked summaries clip their text before the age at narrow terminal widths; piped output and JSON keep the full text. A task that needs you for another reason, such as strikes, still lists its parked runs there. A missing checkout of a task that has not fired is flagged, not retired; the task returns to its normal section when the directory returns.
- HELD follows NEEDS YOU and precedes ROUTINE. It lists self-clearing held starts of the task's workflow runs, one row per start: NAME, WORKTREE when any checkout needs it, REASON, HELD age, and GIVES UP in local time only when some start is bounded. The task itself keeps its normal section. NEXT (WAITING FOR in WAITING) counts these starts as `○ N held`; a task running another checkout keeps its next fire in NEXT and reads `▸ running 3m · ○ N held` in LAST (WAITING FOR in WAITING). A hold that needs a fix instead puts the task in NEEDS YOU with its oldest needs-a-fix reason and `→ rimz doctor`.
- ROUTINE has NAME, RUNS, NEXT, ACTION, LAST. It holds recurring work, including tasks that launch once in each worktree. NEXT carries the next clock time separately from RUNS; a condition task with no next time that a spent window holds back reads `resets YYYY-MM-DD HH:MM` there. ACTION reads `run <command>`, the argv as one shell line, with `· account <name>` when the row names one.
- WAITING has NAME, WAITING FOR, THEN, with the note `one-offs, fire once`. It holds unfired one-shots and command watches without a delivery target. WAITING FOR includes disabled, paused, running, or held state when applicable, and a condition's readings, each unread forge key with its reason (`unknown · pr: no supported forge for this checkout`). Pure-AND conditions with an observed verdict append `k of n met`, plus the number not read when nonzero. A condition held back by a spent window appends `resets YYYY-MM-DD HH:MM`, the latest reset among the windows its unmet terms wait to refill ([window terms](#conditions)). Disabled, paused, unarmed, blocked, OR, and fan-out conditions have no count.
- DONE has NAME, FIRED, with the note `recent one-offs`. It merges retired one-offs and rowless active runs. Running rows stay above the five newest finished entries by default, then `(and N older · --all)` unfolds the remaining finished entries. FIRED gives a retired task's age and result, or `▸ running <duration>`. A retired name needs at least one fire and its newest kind-stamped record must be one-off. One-offs retired before that stamp was introduced stay accessible through `loop logs`, not DONE or `loop show`.
- WAKE-UPS folds to `N waits · M subscriptions`, dropping zero parts, followed by health (`all healthy` or `N need you`) and `rimz loop list --all`. With `--all`, the hint is omitted. Self waits appear first in agent cards, with launched children nested under their ancestors, and each card's handle shows the agent's name beside it when the two differ. Each wait keeps its name, state, and trigger without terminal-width clipping. Lost waits remain in their cards as well as NEEDS YOU. Subscriptions use one line per team or loop owner, with their signals, targets, and worktree count (`in 1 worktree` or `in N worktrees`). A target matching the calling agent is marked `(you)`. Cards use current addressable agent handles, matching wait receipts, and fall back to the stored handle when the target is not addressable or the room is unavailable. Ancestor chains stop at an ancestor that is not addressable.

Every table keeps the task name first. A WORKTREE column appears only when a row is checkout-bound, and the caller's checkout is marked `(here)`. Rows sort by task name except DONE, which puts running rows first and sorts each group newest first, and self waits, which put lost watches first, then watches past check-in, then longest elapsed first. A dead watcher appears in NEEDS YOU with `watcher gone · will not wake <handle>` and a `loop remove` hint; stronger attention reasons still win. Self waits allow 30 seconds after arming for the watcher to start before being classified as lost. Ended delivery targets are retired by the harness, not diagnosed by list.

Trigger text omits the checkout's matching `path` filter, includes labels and pauses, and puts `for <hold>` and `each worktree` on a continuation line. Watched commands in the lifecycle tables read `on command exit`, not their command preview; self-wait cards use the label or command subject instead. On a terminal, table triggers wrap at modifier, condition, and word boundaries down to 24 columns. Parked workflow summaries clip before the age; other table cells are not clipped. Long triggers can still overflow when the fixed columns leave fewer than 24 columns for wrapping. Piped tables keep only the fixed continuation line. Self-wait cards leave wrapping to the terminal, and show a worktree suffix only outside the reading command's checkout.

LAST in the lifecycle tables chooses running, off, latest acting result, latest heard sibling signal, watcher liveness, then `never fired`, in that order. A good acting run reads `✓ 53m ago · 143 in a row`; a failed one reads `✗ failed 2m ago · exit 101`. A run in progress without a held run lock reads `▸ running <age>`, counted from the run's creation, while it executes: an active workflow run of the task in that room created after the latest acting result (the NEEDS YOU rule above), or, before the task has any acting result (its first fire), any active workflow run of it; one whose record carries a wait is not executing, so the row keeps its failed result and any `○ N held` while the task stays out of NEEDS YOU. A held run lock decides the running text and age before either. A sibling signal after a good run does not replace it; with only sibling signals the row reads `never fired · heard ci.passed 7m ago`. Acting-run streaks, launched-worktree counts, and spend today follow inline. Quiet fires are good runs here: they extend a good streak and give the age of a task whose fires are all quiet. A `once-per-checkout` task counts the entries in its launch memory. A repo task not enabled here reads `off · repo task, enable here to run`. WAKE-UPS owner summaries have no per-row LAST cell; inspect their history with `loop show` or `loop logs`.

A run whose task row is gone keeps a DONE row until it ends, with the real name `loop stop` needs and FIRED `▸ running 3m`. After that, retained one-off history remains in DONE; other rowless runs disappear. Empty rooms say how many tasks exist elsewhere and point to `--all`; with none anywhere, the add hint remains.

`--json` prints one object with a `rooms` array for the same scope (`--all` widens it). Each room has `root`, `open`, `here`, `spend_today_usd`, `tasks`, and an unfolded `retired` array of one-offs (`name`, `at`, serialized run `result`, optional `checkout`). A consumed one-shot whose run still executes is a running task, as under its run lock, until that run ends. Tasks are never collapsed and always retain real names. Each has `name`, lifecycle `kind` (`routine`, `one_off`, `wake_up`), `section` (`needs_you`, `routine`, `waiting`, `done`, `wake_ups`), `state` (`live`, `off`, `not_enabled`, `paused`, `running`, `waits_for_room`; `waits_for_room` marks a live clock task whose room is closed while the loop timer is not active), unwrapped `trigger`, `you`, and spend. Pure-AND conditions with an observed verdict outside fan-out tasks add `condition` with `met`, `total`, and `not_read`. An unmet condition with an unread forge key adds `unread`, each such key mapped to its reason, the text WAITING FOR shows in place of `unknown`. `action` has `kind` (`shell`, `workflow`, `agent`, `team`, `wake`) and `subject` (the command's shell line, the launch's command line, or the delivery's target); it is omitted when no definition survives or the action is invalid. An action's `kind` is separate from its task's lifecycle `kind`. Other optional fields are `attention` (`checkout_gone`, `strikes`, `failing`, `pr_closed`, `no_forge`, `held`, `blocked`, `invalid`, `parked`, `lost`), `label`, `dir`, `worktree`, `owner` (`kind`: `team`, `loop`; `name`), `booked_by` (the task's [booking chain](#where-tasks-are-stored), nearest first, each with `kind` and `agent_id`), `next_at`, `running_since` (the start the running state counts from), `last` (`at`, serialized run `result`, `ok`, `streak`), `quiet` (the [quiet summary](#quiet-fires): `count`, `streak`, `last`), `heard` (`signal`, `at`), and `workflow_runs`, the task's active and parked [workflow runs](./workflow.md#json-shapes). Absent values are omitted, not null, except `capacity_wait`, which every task carries and which is `null` without a wait; timestamps use the same format as other JSON commands. Retired history is separate from `tasks`; it counts in watch's done fold without adding table rows.

Listing is read-only and never creates a room store. Malformed local task state fails the command; a malformed other room warns with its root and is skipped. A run lock that cannot be read warns on stderr with the task's name. When a room's locks directory cannot be listed, one warning names the root and directory, and its rows show their other state.

HELD is a human view of starts, not a JSON section. `capacity_wait` is the oldest active workflow run's capacity wait, with optional snake_case `hold`; unknown kinds are omitted and treated as self-clearing. `attention: "held"` means a hold needing a fix only.

### `loop watch`

`loop watch` holds the open room's tasks in a compact glance panel, repainting every second. From a worktree it still shows the whole room, never other rooms. Without an open room it refuses with `rimz start` and `rimz loop list` as alternatives; it also requires a terminal on stdin and stdout. It is the loop pane in the room's `rimzd` tab. `q` or Ctrl-C quits; `--hold` ignores both and hides the quit hint. Tasks share the room's cached forge and account-window readings within a repaint, and the next repaint reloads them. A room closing mid-watch does not stop repainting: while the loop timer is not active, its clocks read `waits for room` until it opens again. The panel prints no lock warnings.

Rows put tasks needing you first, then running tasks (those `loop list` reads `▸ running`), then clocks (due first, then earliest fire), then other tasks, with off, not-enabled, paused, waiting-for-room tasks and tasks holding only self-clearing starts last. Names sort within groups and break clock ties. The three columns are `task`, `next`, and `last run`. `next` carries the state or trigger, including `▸ running 3m`, `○ N held`, countdowns, the list's held-state words, and a waiting condition's readings with each spent window's `(resets YYYY-MM-DD HH:MM)`. It wraps between words before names or last-run text clip. The next column keeps its widest word; names keep the smaller of the widest name and a quarter of the panel width. Last-run text takes the remaining width and clips with `…`. Below those floors, any cell may clip to stay within the panel. Checkout-gone, strike-disabled, closed-pull-request, blocked, invalid, parked, and lost-watcher rows put `✗ <reason>` on a separate line under `next`, across the remaining panel width. Failing rows already explain their outcome in `last run`. A hold needing a fix names its reason in `next`, or on that separate line when `next` shows the held count or the running state.

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

A task opens with `<name> · <kind>[ · worktree <worktree>][ · <trigger>] · <state>`, with lifecycle kind `routine`, `one-off`, or `wake-up`. A condition omits the trigger from the header and adds `· k of n met` for a pure conjunction with an observed verdict. Disabled, paused, unarmed, and blocked conditions have no count. The full `when <expr>[, for <hold>]` appears once above the leaf block or fan-out table, in the same normalized form as `loop list` (parentheses only where precedence needs them). Single-checkout leaves follow in expression order. Each satisfied leaf has `✓`; an unsatisfied leaf has `✗` with a reading, followed for a `window.*` leaf a refill can satisfy by `(resets YYYY-MM-DD HH:MM)` when the provider reports one, or `?` without a reading. Missing readings always carry a reason: room closed, not checked yet, unsupported forge, failing lookup with last-attempt age, no PR or CI for the branch or checkout, no open PR, open-PR facts that do not settle the key, or `not read` for non-forge keys. A leaf under `!` keeps its prefix and negated satisfaction, so a satisfied negated leaf with no reading still shows `✓` and the absence reason. Forge conditions add `forge checked <age>` when the latest probe succeeded. All readings come from disk, never a forge poll. Command watches use `on command exit` in the header and print the full command below, wrapped at whitespace without splitting tokens. Other watches retain their trigger description. An `each-worktree` task instead appends `· each worktree` and shows a `WORKTREES` table without a single-checkout count or condition state.

`loop show <name>` answers whether the task is working, then shows its evidence, and it reads a run in progress by the rule `loop list` reads (LAST above). While a run holds the task's run lock, its header state is `▸ running 3m · pid <PID> · run <run id>` (the PID and elapsed time when the lock names its holder, the run id once the run has a record). With no lock held, an executing workflow run of the task gives `▸ running 3m`, counted from the run's creation, with no PID or run id (WORKFLOW RUNS names the run); a run whose record carries a wait keeps the timing state. A health verdict follows the headline (and watch command when present), reading the latest conclusive run: its age, its status and duration, and the streak (how many consecutive conclusive runs ended the same way, and the age of the failure or good run that bounds the streak). Quiet fires count as good runs in the verdict and its streak, and a failure newer than the last quiet fire ends their part of it. While a run is in progress, executing or waiting, with the lock or without it, the verdict says `last finished run <age>` instead of `last run <age>`, and after a failure it drops `failing`, since `loop list` keeps the task out of NEEDS YOU then: `✗ last finished run 1m ago, failed (exit 1) in 78ms`. A good result keeps `✓ healthy · last finished run <age>`. Attention, waiting-fire, and heard lines follow in that order, then a running task's `loop stop` hint. For a fired one-shot whose run is still active, the task row is already gone: `show` opens with `<name> — one-shot fired` and the same running text, then the `loop stop` command, and any recorded runs as `loop logs` prints them. A retired one-off opens with `<name> — one-shot fired · ▸ running <age>` and the `loop stop` command while a workflow run of it executes after the runner exits, else with `<name> — one-shot fired · finished`, or with its workflow run's state in place of `finished` (`active`, `parked`) while a run its fires name is still live, or with `launched`, the `loop stop` command and a LAUNCHES table like a task's own while an agent, team or `-p` run its fire launched is alive, followed by the `--runs` newest `loop logs` blocks, oldest first. Failed and canceled one-offs say `finished` too; their run blocks give the outcome.

A fire refused at the run lock and recorded as `overlapped` is spent. The run that refused it started before that fire, so the verdict says, in the warning tone, when such fires have had no run since: fires skipped while the active run holds the lock, fires skipped after the last run, or, when no run is active, fires skipped during the last run. With no active run, a warning about fires after the last run (or with no run yet) uses `a fire skipped, nothing has run since` or `fires skipped, nothing has run since` when any counted fire is already on a row; otherwise its numbered wording stays. With an active run, only rowless fires count toward `N fires skipped while the active run holds the lock` (`1 fire` in the singular), or `N fires skipped while a run is in progress` when the run in progress holds no run lock, and the clause is absent when there are none. A fire refused at another gate (`budget skipped` on a delivery row) or recorded as `deferred` ran nothing, so it does not clear the warning. Spent fires (below) and fires no later than the last quiet fire are excluded from this warning. An overlap attributed to an earlier run is not counted as skipped during the last run.

When anything waits under coalesce or replace, the header says `N fires waiting · one run follows when the active run ends`, counting the waiting trigger's `absorbed + 1` fires. A queue says `N fires queued (limit L) · each runs in turn when the active run ends`. Both use `1 fire` for a single fire. A pending replacement adds `· stopping pid N`. `deferred` refusals fold as counts into RECENT RUNS rows, without rows of their own; the `RECENT RUNS` entry below says which row takes them. `show --json` adds `backlog`: `null` for `skip` and `allow`, otherwise an object with `policy`, `limit` (queue limit, otherwise `null`), `triggers` (in firing order, each with `arrived_at`, its firing time; `signal`; `absorbed`; and `from_arrived_at`, the firing time of the kept `from`, once a merge has preserved an older one), and `stopping` (an object with `pid`, `started_at`, and `cause`, or `null` when no stop is pending). `running` describes the active run either way.

Sibling signals heard without acting are not runs. Below the verdict, `heard N other <family>.* signals since, latest <name> <age>` counts those heard after the newest acting run, or all retained history without one (omitting `since`). The line is absent when the count is zero. With no fires, a delivery task says `wakes <handle>; not fired yet`, without a manual-fire hint. Other clock-triggered tasks suggest `loop fire`; condition, signal, and watch tasks say `no runs recorded`. Watched rows label their timeout `check-in`, since it does not kill the watched command; other rows keep `timeout`.

The sections below follow in this order. The facts then read `action`, `label` (when set), `root`, `dir`, `source`, `booked by`, `timeout` (`check-in` on a watch), `will not fire` (for blocked trust), `spend`, and `strikes`. Rows without a value are omitted. `action` uses the list's vocabulary, `run <command>` or a delivery's target handle, with ` (you)` and `· account <name>` when applicable. `booked by` names the agent that booked the task: its current handle while the room's rows still resolve its id, otherwise the id.

| Section | Contents |
| --- | --- |
| `WORKTREES` | Fan-out tasks only: each owned checkout, its condition readings, and the run it launched ([below](#the-worktrees-section)). |
| `LAST RUN` | The verdict's acting run, as `loop logs` prints it ([below](#the-last-run-section)). |
| `RECENT RUNS` | The latest runs, newest first, with adjacent runs folded ([below](#the-recent-runs-section)). |
| `RUNS` | Command tasks: every workflow run the task started in its room ([below](#the-runs-section)). |
| `LAUNCHES` | What the task's fires launched that is still alive, from the fire ids they carry: columns `WHAT` (`workflow run`, `supervised run` or `agent`), `ADDRESS` (the id or `@handle#lane` its own stop takes, with a supervised run's agent name), `STATE` (the run's state, or the agent's status as the sidebar shows it), `WHERE` (its checkout or worktree) and `FIRE` (the fire id's last eight characters). Omitted when nothing is alive. |
| `SUBSCRIPTIONS` | Non-fan-out tasks only: rows owned by this loop task in its room, one per real task name, without the list's collapsing. Each row shows its checkout, target handle, signal, and last acting run with its age, using the list's LAST wording. Without an acting run it reads `never fired · heard <signal> <age>` for a heard sibling signal, otherwise `never fired`. Omitted when no rows name this task as their owner. |

#### The WORKTREES section

This section prints for fan-out tasks only. Its heading carries the owned checkout count, then the nonzero blocked, ready, holding, capacity, waiting, unknown, and launched counts, in that order. Columns are `WORKTREE`, `STATE`, each distinct condition key in expression order (`!`-prefixed when its single term sits under a negation), then `RUN` and `REASON`, each present only when a rendered row fills it.

Rows put blocked checkouts first, then ready, then holding and capacity-held, then launched runs in `active`, `parked`, or `failed` (newest first), then every waiting or unknown checkout. Waiting means the condition is false; unknown means an unread key leaves it unsettled. Within other groups, names sort ascending.

Holding rows show elapsed/hold; waiting, unknown, and ready rows show whether each leaf, negation included, is satisfied, and its reading (`?` with a cache-only reason, or `not read`, when absent). Launched rows show launch age and `<address>  <state> · <node>`, adding `· <detail>` when the shared run phrase is nonempty. A run whose record is missing shows `<run-id>  <state>`, with the state RUNS gives it (below); a project-scoped entry names the same run on every checkout. Term cells stay blank for launched rows.

Runs in any state but `active`, `parked`, and `failed` fold into a count with a status breakdown and the newest folded run's checkout, run address (id when its record is missing), and age, and the group adds `→ reasons and runs: rimz loop show <task> --all`. `--all` unfolds them after the waiting rows, newest first.

Reasons clip to the terminal width. A closed room adds the condition block's note about unavailable CI and PR readings. Readings come from disk, never a forge poll.

#### The LAST RUN section

This section shows the same acting run as the verdict. When that is a quiet fire, it prints the stored row of the last one (its time, duration, checkout, exit and output tail) and `command exited 0 · no row of its own kept`, with no `workflow show` command. Below it, or below the run LAST RUN shows, the quiet summary reads `N quiet fires · S in a row · last <age>`, with the last quiet output under `LAST QUIET FIRE` when LAST RUN shows another run. Otherwise LAST RUN is omitted for a clean `launched` result or an empty body whose header names no manual fire, signal, or late one-shot. Blank check output alone counts as empty. It never selects a refused fire.

The header carries status, age, duration, exit, `manual` for a hand fire, the signal that fired it, and how late a one-shot fired. The body retains fire-time condition readings but omits the recorded expression when it equals the current condition. Otherwise it is what `loop logs` prints for that run, except that a passing command prints its last five lines, followed by the `loop logs` command whose first block is this run. A failed command prints all of its output that the journal kept, up to the last 64 KiB. The linked log keeps the rest.

#### The RECENT RUNS section

The latest runs, newest first. Adjacent runs fold by typed mode, result, check outcome, exit, checkout, run id, target, how late a one-shot fired, and whether fires waited on them or were skipped. Reasons and output do not split a group; the newest note wins. A group adds `×N`, a TOOK duration range, and summed COST and TOKENS. If the group's runs span at least one second, NOTE starts with `over <span>`; shorter spans are omitted. Runs with collided fires stay apart from those without them; collided runs can fold together and sum their counts.

A fire refused at the run lock (`overlapped` or `deferred`) folds into the record whose `run_lock` equals its `holder`, regardless of position, checkout, or result, as `N fires waited on it` or `N fires skipped, run already active`. An `overlapped` or `deferred` record that held its own lock keeps its own row when fires match it, with its recorded status, age and note followed by those counts; it is counted once and does not also fold into another row. Without matched fires, it follows the usual fold. A fire without a holder that carries `blocking_run` folds into the record whose `run_id` is that run; with no such record it is spent once a later run of its checkout exists. A fire with neither falls back to its checkout's next run past gate refusals, and a non-refusal record for that checkout ends the refusal run. A named holder with no matching record can use that fallback only when the receiving record has no `run_lock`; a later tenure of the same checkout's lock proves it gone instead. Unstamped non-gate records bound those legacy gaps. The fires of a holder proved gone are spent. A spent fire takes no row, adds no count to any row, and takes no part in the verdict's warning. Trailing fires (after the last run, or in a checkout no later run used) without that evidence still take no row; the header states active waiting work and skipped fires with no run since. A drained follow-up folds like an ordinary run.

The table is omitted only when its sole row stands for one run, the verdict's acting run, with no folded fires. A `×N` row always keeps the table.

The heading is `RECENT RUNS (newest first)`, adding `· S of T rows` inside the parentheses only when `-n` hides rows. `-n, --runs <N>` sets how many rows, default `10`, and caps RUNS's finished rows the same way. It does not limit `show --json`, whose `runs` holds every retained fire. `MODE` appears when recorded runs differ in mode, `COST` and `TOKENS` when a row has one.

#### The RUNS section

This section prints for command tasks, after the task's facts. It lists each ledgered run, plus every run the task started in its room, finished ones included, excluding every run named by WORKTREES. Live runs come first, then finished and `gone` runs, newest first. RUN is the target `rimz workflow show` selects that run by: the address, or the id when the address would select another run, and the id when its record is missing; WORKTREES's RUN cell uses the same target. Columns are `RUN`, `CHECKOUT`, `STATE` (`active`, `parked`, `done`, `failed`, `canceled`, another fire result, or `gone`), `NODE` (where a finished run stopped), `DETAIL` (the [shared run phrase](./workflow.md#inspect-runs)), and `AGE` since a finished run ended, else since the run entered its node.

A run whose record is missing shows its fire's result as STATE (`done` for `completed`, `failed`, `canceled`, and any other result by its label, such as `verify failed`), or `gone` when the fire recorded none, with blank NODE and DETAIL, AGE since launch, and no `workflow show` command. A finished run whose record this RimZ cannot decode (an older RimZ's, or one the room's workflow pass ended as stranded) shows the state, node and ending its record names, blank DETAIL, and its id as RUN.

Adjacent finished rows fold when checkout, state, and node agree: STATE adds `×N`, DETAIL reads `over <span>` once the span reaches one second, and RUN and AGE come from the newest member; `workflow show --history` and `--json` keep each run. `-n, --runs` caps the finished rows after folding, while live rows always print; the heading adds `(S of T rows)` when rows are hidden. Under the table, `→ rimz [--root <root>] workflow show <RUN>` opens the first row that has a record. The section is omitted when there are no unmatched runs.

#### `loop show --json`

`loop show --json` keeps one entry per attempted fire, overlapped fires included, but not heard sibling signals or quiet fires: a workflow run in flight has one `launched` entry, and a kept run has its one report entry. The quiet summary is the top-level `quiet` object (`count`, `streak`, `last`, the stored row), `null` while the task has none. A retained trigger has its `deferred` entry and a later served-run entry. `absorbed` counts the queued triggers a fire took up: it is nonzero on the served run, and on a `deferred` entry whose fire took up triggers but found the run still live. Empty or busy drainers and drained triggers dropped for a paused, disabled, missing, invalid, or no longer retaining task add nothing. Human history rows also exclude siblings. `loop logs` and `loop show` use the task row's root and exclude history before a reused wait's arming. `launches` lists the `LAUNCHES` rows, each with `what`, `address`, `name` (a supervised run's agent name; omitted otherwise), `fire` (the whole fire id), `state` and `destination` (omitted when unknown); it is empty when nothing is alive. The JSON document adds top-level `kind` (`routine`, `one_off`, `wake_up`) and keeps `task`, `entry`, `launches`, `runs`, `running`, `workflow_runs` (unfolded and unlimited: each run named by the workflow launch memory once, then every other run this task started in its room, live ones first and finished ones newest first with `terminal_at`; empty for a task that starts no workflow), `booked_by` (the booking chain, as in `loop list --json`; omitted for a task with none), and `worktrees`, and `heard`: `null` without a recorded sibling signal, otherwise an object with `count`, `since` (the acting run's RFC 3339 timestamp, or `null`), `latest` (signal name), and `latest_at` (RFC 3339 timestamp). `running` is `null` while no run is executing, otherwise an object with `pid`, `started_at`, and `run_id`. Under a held lock, each is `null` when the lock names no holder or the run has no record yet. For an executing workflow run with no lock held, `pid` and `run_id` are `null` and `started_at` is the run's creation, the `running_since` `loop list --json` prints; `run_id` names only a supervised run, never a workflow run, which `workflow_runs` carries. A run waiting on its record leaves `running` `null`. A record an older RimZ wrote after a start-throttle hold carries `throttle_wait_ms`. A scheduled `fire-at` fire that started two minutes or more past its instant carries `late_ms`. A run lock that cannot be read warns on stderr and prints `null`.

`worktrees` is an unfolded array for fan-out tasks (empty for other tasks). Each row has `checkout` (path), `name`, and `state` (`ready`, `holding`, `waiting`, `unknown`, or `launched`). Condition rows include `readings` (key to value or `null`, in expression order) and `short` (keys of leaves not satisfied, negation included). Holding rows add `since`, `hold_ms`, and `held_ms`; ready rows add `since`. A true condition without a matching hold clock starts as holding with `since` set to now and `held_ms` zero when a hold is configured, otherwise ready since now. Launched rows add `since` and `run`, the same object as the matching `workflow_runs` row. `--all` changes only the human table, not JSON.

In the human table, a condition key used by multiple terms shows a present reading without a truth glyph; single-term keys keep their glyph. If the owned worktree list cannot be read, the table prints one explanatory note instead of rows. If the room's subscription catalog cannot be read, both the screen and `--json` print what they have, with every `armed` count at zero, and then exit nonzero with the error.

Run records refused at the run lock carry `holder` when the lock names one, in the shape `{"pid": 42424, "started_at": "2026-01-01T00:00:00Z"}`. Older records and records without a known holder omit it. Their stored `error` names the policy outcome without a pid or an age. A fire that held the run lock records its own tenure as `run_lock` in the same shape, including when `loop stop` writes on its behalf. Older records and fires that never held the lock omit `run_lock`; human `loop logs` output does not print it. A fire that a live workflow run kept from starting (`overlapped` or `deferred`) carries that run's id as `blocking_run`; other records omit it. Its stored `error` names the run, in one of three shapes:

- `run <id> still active — <outcome>`: the task's own run from an earlier fire is still active.
- ``run <id> of `<workflow>` was started by hand — skipped``: a run of the task's workflow started by hand in the fire's checkout is live. A `workflow run --once` try never skips a fire.
- `workflow run <id> holds the checkout — skipped`: a workflow task's launch was refused by a live run of its workflow in its checkout, which can be another task's run.

Workflow-run DETAIL in `loop show` and WORKTREES uses the same count-free status phrase as `workflow list`: a check wait reads its outcome, not `said <outcome>`, and no advance count is printed. The JSON of `loop list` and `loop show` carries the same phrase in `workflow_runs[].detail`. Loop views do not add agent or PR enrichment.

### `loop logs`

`loop logs <name>` prints full forensics in blocks, oldest first, so the newest ends the output. Each run keeps its own block. Quiet fires take one block together, at the last one's place in time: `✓ completed · <age> · N quiet fires · S in a row · <trigger>[ · exit N]`, the stored output of the last one, and `command exited 0 · no row of its own kept`. It counts as one block under `-n`, at that same time, so `-n N` shows the newest N blocks whether or not it is among them; `--failed` leaves it out. For a command task, a block that names a workflow run (`launched`, a terminal report, or `workflow skipped`, which names the run holding the scope) reads that run's record and journal when `loop logs` runs. The journal gives the run's evidence, oldest first: the command's check as `command: <outcome>[ · exit N] · <duration>`, then `output: <log>` and the output below it (the last 64 KiB of the log). The command's output prints there alone; a quiet fire, or a block whose run's journal names no readable log, prints the row's stored `output:` path and output instead; cost follows when recorded, and adjacent evaluations of one check with one outcome fold into the newest with `×N`. Then `run: <id> · <state> at <node>`, with the [shared run phrase](./workflow.md#inspect-runs) after it while the run is live, then `→ rimz [--root <root>] workflow show <RUN>`. An `overlapped` or `deferred` block names the run that blocked it the same way, as `blocked by: <id> · <state> at <node>` with its command. A finished run whose record this RimZ cannot decode reads the state its record names, ` at <node>` only when the record names one, and `→ rimz [--root <root>] workflow show <id>`. A run whose record is gone reads `run: <id> · gone` (or `blocked by: <id> · gone`) with no command. Fires refused at the run lock fold into their attributed record's block, using the same attribution as RECENT RUNS; its header ends with `N fires waited on it` and `N fires skipped, run already active` when present. Trailing and spent at-lock fires stay as refusal blocks in place, merging adjacent fires only when their typed key and holder agree. Other gate refusals fold by the RECENT RUNS key. A repeated refusal's header adds `×N` after the status and, when the span reaches one second, `over <span>` after the newest age; its body keeps the newest member's forensics. `loop logs` has no `--json`: each folded member's own record is in `loop show --json`'s `runs`, which needs a current task definition. Each ordinary block holds status, age, trigger (`manual` for a hand fire, `signal <name>` for a signal, `scheduled` otherwise), exit information, check output, error chains, run ids, and cost when recorded. A known run-lock holder gets `holder: pid N · started <D> before this fire`, or `before the newest fire` for a folded group. The interval measures the holder's start to that fire, never to now. Check output and errors stay literal. While the task is running, the last line is the running text `loop show` opens with; `--failed` leaves it out. Empty history uses the same delivery-target line or clock-only fire hint as `show`; `--failed` instead says `no failed runs recorded`. A removed task whose retained history holds only heard signals points at `--heard` instead.

| Flag | Effect |
| --- | --- |
| `-n, --runs <N>` | How many blocks to print after folding and filtering. Default `10`. |
| `--failed` | Only `failed` and `error` runs (and `timed out`, `budget exceeded`, and `verify failed` in older history), plus `launched` blocks whose run ended `failed`. |
| `--heard` | Include heard sibling signals in log order, labelled `○ heard`. |

Condition runs in `loop logs` include `when: <expression> · held <duration>` and a JSON readings line in expression order (map order if a stored expression cannot be parsed). The record keeps the canonical expression, configured hold, observed held milliseconds, and readings; manual fires do not synthesize readings. Signal runs include `signal: <name>` and, when the signal carried fields, a JSON payload line. Sibling signals appear only with `--heard`, which changes no stored record.

## Rename and remove

`loop rename <name> <new-name>` moves the task to the new key in its store and prints ``renamed loop task `<name>` to `<new-name>` ``, or ``no loop task named `<name>` `` with exit 0 when none exists. Its launch memory and live workflow runs, active or parked, follow it; finished run records keep the name they ran under. The new name must be a valid task name, differ, and be free: a taken name refuses with ``loop task `<new-name>` already exists``. A task that still remembers a launch an older RimZ recorded cannot be renamed, because that record keeps the task's name; remove and re-add it under the new name. The task re-arms, so an interval task next fires one interval after the rename.

`loop remove <name>...` deletes each task from its store and prints ``removed loop task `<name>` ``, or ``no loop task named `<name>` `` when none exists. Comment lines above the removed task's headers stay above the next header, or at the end of the file. Missing names do not stop later removals or cause a nonzero exit. Removal clears the task's launch memory and detaches its active and parked workflow runs, whose commands continue as independent runs. Run history stays readable through `rimz loop logs <name>`. For a project task, both commands print the [trust result](#project-tasks).

## Timer

`rimz loop timer install` installs one user-level timer that ticks every minute: a systemd user timer on Linux, a launchd agent on macOS. Each tick re-reads every task and fires due clock tasks only for roots without an open room; a root with a room is left to that room. One timer covers every root, so adding or editing a task needs no reinstall.

| Command | Prints |
| --- | --- |
| `timer install` | `loop timer: installed (<backend>; every 1m) → <path>` |
| `timer status` | `loop timer: active (<backend>) → <executable>` (or `inactive`), then `task roots without a room: N`; or `loop timer: not installed`. |
| `timer remove` | `loop timer: removed (<backend>)`, or `loop timer: already absent`. `rimz uninstall` removes it too. |

A timer fire follows the same rules as a room's:

- A command fire opens a room for its project before its run starts, once the deadline, launch memory, and run lock let it through. The room stays open and keeps time for that root from then on; `timer remove` does not close it.
- A delivery row still needs its pinned agent, and an untrusted project task stays blocked.
- On Linux each fire runs in a transient `systemd-run --user --scope`, in its own process group, so it outlives the tick. A tick that would take that route checks the systemd user manager is reachable first, and refuses the whole pass naming the fix when it is not: nothing fires, no stamp moves, and the tasks are still due on the next tick.
- A task the timer sees for the first time arms without firing, so installing the timer replays nothing.

The task model, file shapes, and scheduler are in [loops.md](../../internals/harness/loops.md).
