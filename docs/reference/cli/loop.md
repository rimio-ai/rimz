# Loop CLI

`rimz loop` runs work on a clock or on a signal. A task is a name, one trigger, and one action: start a fresh supervised agent (`--agent`), deliver a prompt to a live agent session (`--wait`), or run a shell command (`--check`). A check can also stand in front of an agent action as a guard. The [loops guide](../../guide/loops.md) teaches when to use each; an alarm an agent sets for itself, after a delay or a watched command, is [`rimz wait`](./wait.md).

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
| `rimz loop remove <NAME>` | Delete a task from the store that owns it. Run history stays. |
| `rimz loop rename <NAME> <NEW_NAME>` | Rename a task in its store. |
| `rimz loop enable <NAME>\|--all` | Arm tasks on this machine and clear holds and strikes. |
| `rimz loop disable <NAME>\|--all` | Hold tasks on this machine until enabled. |
| `rimz loop pause <NAME> --for <DUR>` | Hold a task for a bounded time. |
| `rimz loop fire <NAME>` | Run a task now in the foreground. |
| `rimz loop stop <NAME>` | Stop a task's active run and release its overlap lock. |
| `rimz loop list` | List tasks grouped by project, with last run and next fire. |
| `rimz loop watch` | Hold a live dashboard open with countdowns. |
| `rimz loop show <NAME>` | One task's trigger, health, spend, and recent runs. |
| `rimz loop logs <NAME>` | Full forensics for a task's recent runs. |
| `rimz loop timer install\|status\|remove` | Manage the machine-wide timer. |

Every command takes the [global flags](../cli.md#global-flags). No `loop` command prints JSON.

## Add a task

`rimz loop add <NAME>` takes a name of letters, digits, `-`, and `_`, exactly one action, and exactly one trigger. Adding a name that already exists in the same store replaces that task.

### Actions

| Action | Flags | Prompt |
| --- | --- | --- |
| Start a fresh agent | `--agent <SPEC>`: a kind, a profile, or a kind with a mode suffix such as `codex-yolo`. Each fire opens one transient supervised pane that runs the prompt once. | Required: `--prompt` or `--prompt-file`. |
| Wake a live agent | `--wait [<ADDRESS>]`: pins one live session now; see [waits and checks](#waits-and-checks). | Optional. |
| Run a command | `--check <CMD>` with no other action. Records `completed`, `failed`, or `timed out`. | None. |

`--check` combined with `--agent` or `--wait` is a guard: the command runs first, and the action runs only on the outcome `--on` names. `--agent` refuses a kind whose hooks are not installed, because a scheduled turn reports completion through them.

### Triggers

| Shape | Flags | Repeats |
| --- | --- | --- |
| One-shot | `--at HH:MM`, or `--in <DUR>` | Fires once, then the task removes itself. |
| Interval | `--every <DUR>`, measured from the last fire | Yes. |
| Calendar | `--every <DAYS> --at HH:MM` | On each matching day. |
| Raw cron | `--cron "<5 fields>"` | Per the expression. |
| Poll-until | `--every <DUR> --until <DUR> --check <CMD>` plus `--agent` or `--wait` | Until the guard fires the action or the deadline passes. |
| Signal | `--signal <NAME\|FAMILY.*>`, narrowed by `--match` | On every delivering signal, or once with `--once`; see [signals](#signals). |

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

### Flags

| Flag | Applies to | Meaning |
| --- | --- | --- |
| `--prompt <TEXT>`, `--prompt-file <PATH>` | `--agent`, `--wait` | The prompt the action delivers. The two conflict. |
| `--check <CMD>` | all | Shell command to run; alone it is the action, otherwise the guard. |
| `--on fail\|success\|any` | `--check` | Which check outcome fires the action. Default `fail`. |
| `--verify <CMD>` | `--agent` | Command that must pass after the turn; a failure re-prompts the same session. |
| `--max-attempts <N>` | `--verify` | Total turns allowed to make `--verify` pass. Default `3`, at least `1`. |
| `--max-strikes <N>` | all | Consecutive failed fires before the task disables itself. Default `3`; `0` turns the gate off. |
| `--timeout <DUR>` | all | Caps the check, the agent turn, and its verify commands; see [timeouts](#timeouts). `--wait` accepts it only with `--check`. |
| `--worktree <NAME>` | `--agent`, `--wait` | Channel or worktree that hosts the transient pane, or that resolves the `--wait` address. |
| `--mode auto\|ask\|yolo` | `--agent` | Permission posture for the turn. |
| `--effort <EFFORT>` | `--agent` | Reasoning effort passed to the agent. |
| `--system-prompt-file <PATH>` | `--agent` | Replace the agent's base system prompt with a file's contents. |
| `--budget <AMOUNT[/day]>` | `--agent` | Dollar cap for each spawned run. |
| `--budget-per-day <AMOUNT>` | `--agent` with `--budget` | Daily dollar cap for the task; see [budgets](#budgets). |
| `--surplus <RATIO>`, `--surplus-after <DUR>` | `--agent`, `--wait` | Fire only while the provider window has headroom; see [the surplus gate](#the-surplus-gate). |
| `--root <PATH>` | all | Project whose room hosts the task. Default `.`. |
| `--project` | `--agent`, check-only | Write the task to the project's `.rimz/config.toml`; see [project tasks](#project-tasks). |

`--mode`, `--effort`, `--system-prompt-file`, `--budget`, and `--budget-per-day` are refused on `--wait` and check-only tasks, and `--worktree` on check-only tasks.

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

A `--wait` action reads ``action: waits @planner — pinned to claude session `<id>` now; skipped and removed if that session exits``, a check-only task `action: runs check in <dir>`, and a surplus gate adds a `gate:` line. The last line reads `the loop timer will keep time` when the timer is active.

## Where tasks are stored

Each task lives in one of three stores, which `rimz loop list` names in its SOURCE column.

| Store | File | Holds | SOURCE |
| --- | --- | --- | --- |
| Machine | `~/.rimz/loop.toml` | Repeating `--agent` and check-only tasks added without `--project`. | `machine` |
| State | `~/.rimz/ws/<workspace-dir>/loop-instances.json` | Every `--wait` task, plus one-shots (`--at`, `--in`), `--until`, and `--once` tasks. Rows retire themselves. | `state`, or `team <instance>` for a team binding |
| Project | `<root>/.rimz/config.toml`, `[tasks.<name>]` | Tasks added with `--project`. | `project`, or `project · untrusted` / `project · stale` while trust is missing |

`loop list`, `enable --all`, and `disable --all` cover machine tasks plus the state and project tasks of the project resolved from the current directory or `--root`. Enable, disable, pause, and strike state belong to this machine: machine-wide for machine tasks, per project for state and project tasks.

### Project tasks

`--project` writes a task that ships with the repository. It needs `--every`, `--cron`, or `--signal`, and refuses `--wait`, `--until`, and `--once`. `--root` must lie inside the project of the current directory. A project task stores no `root` or `dir` and always runs at the project root.

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

A subscription observes its whole family. A task on `ci.failed` fires on `ci.failed`, records `skipped` for a sibling such as `ci.passed` without spending a turn, and ignores other families. A `'ci.*'` selector fires on every member. `--match KEY=VALUE` requires a top-level payload field to equal the value; repeat it for an all-of set. A signal that fails a match is ignored. `ci.finished` and `--match conclusion=…` are refused at add time; use `ci.passed` or `ci.failed`.

Signals come from `rimz events emit`, forge state changes on worktree branches, agent and team lifecycle transitions, team stage flips, and `rimz wait` watchers. The [signal lines table](./events.md#signal-lines) lists every name and source.

Firing runs in the emitting process, with no queue. A signal reaches only the tasks subscribed at that moment, and nothing replays it later. A disabled or paused task, or a project task whose project is untrusted, skips the signal.

`--signal --wait` with the same target session, selector, matches, and root as a live subscription prints `already subscribed as <name>` and writes nothing, even when the prompt or `--once` differ.

A signal fire's prompt opens with a headline, the task name, and the payload as one JSON line with a `signal` key added, then the task's `--prompt`:

```text
waited on ci.failed on feat-x (PR #91)
fired [ci-fix]
{"branch":"feat-x",…,"number":91,…,"signal":"ci.failed"}

read the failing job and fix it
```

The sample shortens the payload, which carries every field the signal has. The headline adds the branch and PR number for `ci` and `pr` signals, the handle for `agent`, and the instance for `team`. A manual `loop fire` of a signal task delivers `fired by hand [<task>]` with no payload line. The run record keeps the signal name and payload, truncating a payload over 4 KiB.

Project tasks may use `--signal` and `--match`, and a subscription satisfies their repeat requirement.

### Caller-scoped defaults

A `--wait` subscription fills in scope the caller leaves out. From an agent the caller supplies it; from a user shell the target does.

| Selector | Without these matches | Default |
| --- | --- | --- |
| `ci.*`, `pr.*` | `path`, `branch` | `--match path=<caller's worktree>`. From the root checkout the default is refused, because forge state is polled for worktree branches only. |
| `team.*` | `team`, `instance` | `--match instance=<caller's cohort>`, such as `forge#feat-x`. A caller outside a team is an error. |
| `agent.*` | `handle`, `session` | None: the add is refused. Name another agent with `--match handle=<other>` or `--match session=<other>`, so a target never wakes on its own lifecycle. |

From the root checkout, pass `--match branch=<name>` or `--match path=<worktree-path>`, add from a linked worktree, or watch the command instead with `rimz wait -- gh run watch --exit-status`.

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

`--verify <CMD>` runs after an `--agent` turn and re-prompts the same session with the failure until the command passes or `--max-attempts` turns are spent, then records `verify failed`. `--wait` and check-only tasks refuse it, because they have no supervised session to re-prompt. The retry loop is the one [supervised runs](./agents.md#supervised-runs--p) use.

## Budgets, gates, and strikes

Gates run before the check, so a closed gate spends nothing and opens no room. A gate skip never counts as a strike.

### Budgets

`--budget <AMOUNT>` caps each spawned run; a run that overruns it records `budget exceeded`. `--budget-per-day <AMOUNT>` sums the task's run costs in the configured local day and skips a fire when the remaining amount cannot fund the next run's `--budget`.

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

A missing, incomplete, expired, or not-started window keeps the gate closed, so an API-key account without window readings always skips. A managed Qwen `--agent` task paces against its account's 7-day window, and a Qwen `--wait` task always skips. A closed gate records `surplus skipped`, and `loop show` prints the gate and the recorded reason. The headroom model with a worked example is [budgets → the surplus gate](../../guide/budget.md#the-surplus-gate).

### Strikes

After `--max-strikes` consecutive strikes (default `3`), the task disables itself, `loop list` shows `disabled · N strikes`, and `loop_disabled` notification handlers fire. `rimz loop enable` clears the count.

| Run result | Effect on strikes |
| --- | --- |
| `failed`, `timed out`, `error`, `verify failed`, `budget exceeded` | Strike. |
| `completed`, `delivered` after a guard that fired on a failing check | Strike. |
| `completed`, `delivered` otherwise | Resets the count. |
| `skipped` after a passing check | Resets the count. |
| `skipped` after a failing check, or a sibling signal | Neutral. |
| `budget skipped`, `surplus skipped`, `overlapped`, `canceled`, `expired`, `target gone` | Neutral. |

### Timeouts

| What | Cap |
| --- | --- |
| `--check` | The task's `--timeout`, else 5 minutes. A killed check counts as a failure. |
| Scheduled `--agent` turn and its verify commands | The task's `--timeout`, else `loop.default-timeout` (default `2h`, set with `rimz config set loop.default-timeout 3h`). |
| `--agent` turn under `loop fire` | The task's `--timeout`, else unbounded. |

`--timeout` takes `s`, `m`, `h`, or `d`.

## Enable, disable, and pause

`loop enable <name>` arms a task on this machine and clears a disable, a pause, and its strike count. `loop disable <name>` holds it until the next enable. Both take `--all` instead of a name.

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

A task whose previous run is still active records `overlapped` and does not start another. `loop stop <name>` ends the active run: it marks a linked supervised run canceled, waits 5 seconds for the overlap lock, sends SIGTERM to a remaining holder, and waits again. It prints ``loop `<name>`: stopped`` (with the run id, and `· SIGTERM` when it signaled) or `no active run`, and exits `0`. When the lock is still held it exits `1` with the holder's PID and the lock path; SIGKILL is left to you.

## Read run history

Every fire appends one record to `~/.rimz/logs/loop-runs.log.jsonl`. `show` and `logs` read records for the current project, and keep reading a task's history after the task is removed.

### `loop list`

`loop list` groups tasks by project root. Each group's heading shows `room open`, `no room`, or `no room · timer`. The columns are NAME, TASK, SOURCE, SCHEDULE, LAST (age of the last run), STATUS (its result), COST, and NEXT:

| NEXT | Meaning |
| --- | --- |
| `in 12m` | The next clock fire. |
| `listening` | A signal subscription. |
| `watching` | A `rimz wait` command, PID, check, or file watch. |
| `disabled`, `disabled · N strikes`, `disabled · enable to arm` | Held until enabled; the last is a project task not yet enabled here. |
| `paused · in 2h` | Paused until then. |
| `blocked · trust` | A project task whose project is not trusted. |
| `-` | No clock has armed the task yet, or no occurrence remains. |

Footers count tasks blocked by trust and project tasks not yet enabled, with the command that fixes each.

### `loop watch`

`loop watch` holds the list open as a live dashboard, repainting countdowns and `running now` every second. It is the loop pane in the room's `rimzd` tab.

### `loop show`

`loop show <name>` opens with the task's schedule and next fire or hold, a health verdict from the latest conclusive run, and the task's facts: action, check, root (and `dir` for a linked worktree), source, effective timeout, budgets, surplus gate, spend, and `strikes N/max` once a strike is recorded. An active run adds its run id and the `loop stop` command. The sections below follow:

| Section | Contents |
| --- | --- |
| `AGENT RUNS` | Check-gated `--agent` and `--wait` tasks only: how many fires escalated to the action, their total and average cost, and the five latest attempts. |
| `RECENT RUNS` | The latest runs, identical consecutive runs collapsed into one row. `-n, --runs <N>` sets how many, default `10`. |
| `LAST RUN` | The newest run in full; an older last failure becomes a pointer to `loop logs`. |

A sample is in the [loops guide](../../guide/loops.md#what-a-fire-leaves-behind).

### `loop logs`

`loop logs <name>` prints the full record of the latest runs, oldest first, so the newest ends the output. Each block holds the status, age, mode (`scheduled` or `manual`), exit information, check output, error chains, run ids, pane output tails, cost and tokens, and transcript links when recorded. Agent messages render as [agent prose](../cli.md#agent-prose); check output and errors stay literal.

| Flag | Effect |
| --- | --- |
| `-n, --runs <N>` | How many runs to print. Default `10`. |
| `--failed` | Only `failed`, `timed out`, `budget exceeded`, `verify failed`, and `error` runs. |

## Rename and remove

`loop rename <name> <new-name>` moves the task to the new key in its store. The new name must differ and be free. The task re-arms, so an interval task next fires one interval after the rename.

`loop remove <name>` deletes the task from its store and prints ``removed loop task `<name>` ``, or ``no loop task named `<name>` `` when none exists. Run history stays readable. For a project task, both commands print the [trust result](#project-tasks).

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
- On Linux each fire runs in a transient `systemd-run --user --scope`, in its own process group, so it outlives the tick.
- A task the timer sees for the first time arms without firing, so installing the timer replays nothing.

The task model, file shapes, and scheduler are in [loops.md](../../internals/harness/loops.md).
