# Loop scheduling

> The scheduler: where task definitions live, who keeps time, what one fire does, how signals and waits reach the same machinery, and the assist log that holds every unattended intervention to account. [fleet.md](./fleet.md) is the map for this area. For users, the guide is [loops.md](../../guide/loops.md) and the flag references are [cli/loop.md](../../reference/cli/loop.md), [cli/wait.md](../../reference/cli/wait.md), and [cli/events.md](../../reference/cli/events.md).

## What the scheduler does

`rimz loop` fires agent work on a trigger. A fire starts a fresh supervised turn, delivers a prompt to an agent that is already running, or runs a shell command that can guard either one. The trigger is a clock, a signal selector, or a watched command. `rimz wait` is the agent-facing front end over the same task rows.

There is no RimZ scheduler daemon. The room already elects one process to do shared work, the sidebar producer or elder ([state.md § Renderers, the producer, and consumers](../sidebar/state.md#renderers-the-producer-and-consumers)), and the elder keeps time for loop tasks on its ordinary data tick. For schedules that must run with the room closed, a user can install one OS timer that launches a one-off `rimz loop tick` and exits; it yields every root whose room is open ([The external tick](#the-external-tick)).

Four rules follow from having no daemon, and they explain most of the module.

**Arming is not firing.** A task the elder has never seen is stamped with the current time and does not fire. A room opened hours late never replays the occurrences it missed.

**Every fire is at-most-once per occurrence.** The elder writes the fire stamp before it spawns the runner, so a hot tick cannot spawn the same occurrence twice, and a per-task advisory run lock stops two runs from overlapping.

**An event fires in the process that produced it.** A clock needs a timekeeper and an event does not: whoever emits a signal resolves the subscribers and spawns their runs itself. Signal and watch triggers therefore work with no room open and never touch `loop-fire.json`. The cost is that nothing queues a signal, so a signal is never replayed ([The signal vocabulary](#the-signal-vocabulary)).

**Every fire appends exactly one history row.** Gated, skipped, overlapped, expired, delivered, completed, or errored, a fire records one `LoopRunRecord`. That log is the durable trace of what automation did, so it must be complete.

## Module layout

Every path below is under `crates/rimz/src/`; `schedule/` means `harness/schedule/`.

| File | Owns |
| --- | --- |
| [`harness/schedule.rs`](../../../crates/rimz/src/harness/schedule.rs) | The vocabulary: `TaskAction`, `Trigger` and its parsing, `Schedule`, `ParsedSchedule`, due evaluation, next-occurrence calculation, `TaskTiming` display states, and the `TaskShape` compile. |
| [`schedule/catalog.rs`](../../../crates/rimz/src/harness/schedule/catalog.rs) | `TaskCatalog`: the three sources, visible and runnable precedence, source-aware mutation, scheduled consumption, `LoadedTask::enable` (enablement plus strike clearing), and `clear_overlays` for replacement and removal. |
| [`schedule/instances.rs`](../../../crates/rimz/src/harness/schedule/instances.rs), [`overlay_store.rs`](../../../crates/rimz/src/harness/schedule/overlay_store.rs) | RimZ-owned instance rows with locked insert, remove, and rename, and the locked persistence the overlays share. |
| [`schedule/arming.rs`](../../../crates/rimz/src/harness/schedule/arming.rs), [`strikes.rs`](../../../crates/rimz/src/harness/schedule/strikes.rs) | Machine-local overlays: enablement, bounded pauses, effective arming and source defaults through `ArmState::resolve`, the effective-last-fire rule, and consecutive failure counts. |
| [`schedule/config_edit.rs`](../../../crates/rimz/src/harness/schedule/config_edit.rs) | Comment-preserving TOML edits to machine `loop.toml` and project `.rimz/config.toml`. |
| [`schedule/fire.rs`](../../../crates/rimz/src/harness/schedule/fire.rs) | Clock firing shared by the elder and the external tick: root ownership, arm-on-first-sight, due planning, `loop-fire.json`, and how the detached `rimz loop run <name>` is hosted. |
| [`cli/loop_timer.rs`](../../../crates/rimz/src/cli/loop_timer.rs) | The systemd user timer and launchd agent: install, status, removal, unit rendering, and the external tick. |
| [`schedule/runner.rs`](../../../crates/rimz/src/harness/schedule/runner.rs) | `TaskFire`: the gate ladder, the run lock, the check, prompt preparation, the prepared effect, and the one terminal history transition; `stop_task`, the stop ladder. |
| [`schedule/runner/prompt.rs`](../../../crates/rimz/src/harness/schedule/runner/prompt.rs) | `compose_wait`: the wait line, the verdict line, the evidence, and the verbatim note. |
| [`schedule/run_log.rs`](../../../crates/rimz/src/harness/schedule/run_log.rs) | `LoopRunRecord`, `LoopRunResult` and its `spawn_exit_code` mapping through `store::run::RunStatus`, the user-global JSONL history, cost rollups, and the daily-budget gate. |
| [`schedule/signal.rs`](../../../crates/rimz/src/harness/schedule/signal.rs) | The runtime signal: `Signal`, `SignalSelector`, `WatchVerdict` and `WatchOutcome`, the lifecycle-to-signal mapping, the conversion into the durable payload, `fire_signal`, `wait_output_path`, and `run_watcher`. |
| [`schedule/signal/team.rs`](../../../crates/rimz/src/harness/schedule/signal/team.rs) | The pure cohort-edge derivation behind `team.idle`, `team.waiting`, `team.failed`, and `team.ended`. |
| [`store/event.rs`](../../../crates/rimz/src/store/event.rs) | The persisted signal: the `SignalName` grammar and its reserved families, `SignalSource`, and the `SignalEventPayload` that `Store::append_signal` records ([store.md](../store.md#what-is-in-it)). |
| [`schedule/arm.rs`](../../../crates/rimz/src/harness/schedule/arm.rs) | `arm_delivery`, the one builder for session-pinned delivery rows: caller-scoped signal defaults and guards, locked dedupe, watcher spawning, and `retire_session`. |
| [`schedule/pending.rs`](../../../crates/rimz/src/harness/schedule/pending.rs) | The read-only projection of armed one-shot deliveries into per-agent pending waits. |
| [`schedule/team.rs`](../../../crates/rimz/src/harness/schedule/team.rs) | Team signal bindings: validation at launch, materialization at member registration. |
| [`cli/loop_cmd/`](../../../crates/rimz/src/cli/loop_cmd), [`cli/wait/`](../../../crates/rimz/src/cli/wait) | Flag translation, caller resolution, executing prepared effects, receipts, and rendering. Neither holds scheduling policy, and neither imports the other. |
| [`harness/assist_log.rs`](../../../crates/rimz/src/harness/assist_log.rs) | The [assist log](#the-assist-log). |

## The task

A task is a name, one action, and one trigger. `TaskAction::from_entry` derives the action from the persisted entry.

| Action | Entry fields | What a fire does |
| --- | --- | --- |
| `Spawn` | `agent = "<cell>"` | opens one transient supervised pane through the [supervised-run](./scripting.md) path |
| `Deliver` | `wait = { kind, session, handle }` | sends the prompt to one pinned live session through the [message](./messaging.md) path |
| `CheckOnly` | `check` alone | runs the shell command and records its outcome |

The combinations are narrow. `agent` and `wait` conflict, and one of `agent`, `wait`, or `check` is required. `verify` requires `agent`, because verification re-prompts a supervised run. `max-attempts` requires `verify` and must be at least 1.

A `Spawn` task names exactly one agent cell: a built-in kind, a profile, or an adapter-supported virtual cell such as `claude-auto` or `codex-yolo`. `rimz loop add` rejects teams, multi-cell layouts, and command cells, because a scheduled task owns exactly one supervised pane.

`TaskShape::compile` compiles each persisted row into an action result and a timing result independently. A row with a valid action and a malformed schedule stays visible and can be fired by hand, while scheduled firing skips it.

## Where tasks live

Three sources back the catalog.

| Source | File | Holds |
| --- | --- | --- |
| `Config` | `~/.config/rimz/loop.toml` | per-machine automation, like a crontab; never inherited by a clone |
| `Project` | `<root>/.rimz/config.toml` under `[tasks.*]` | shared automation that travels with the repository; inert until trusted and enabled on this machine |
| `Instance` | `~/.local/state/rimz/workspaces/<workspace-id>/loop-instances.json` | RimZ-owned runtime rows: one-shots, poll-until rows, `once` subscriptions, and every session-pinned delivery, including recurring clocks and standing signals |

The instance store keeps runtime churn out of user config. An agent that arms `rimz wait --in 30m` writes an instance row, not `loop.toml`, and the row retires itself after it fires. `TaskCatalog::load(Some(root))` reads that workspace's instances and `load(None)` reads machine tasks only. The strict loader also moves any rows left in the user-global `~/.local/state/rimz/loop-instances.json` into each row's workspace file, keeping the existing row on a name conflict, and deletes the global file; lenient reads leave it alone. Wait rows found in `loop.toml` stay `Config` rows, and `rimz gc` reaps them.

A project task cannot make machine-local claims, so loading rejects four fields: `root` and `dir` (a project task runs at the project root), `wait` (it cannot pin a session on another machine), and `deadline` (a poll-until timestamp is machine state). It also requires `every`, `cron`, or `signal`, because a one-shot would have to delete itself from a trust-hashed file.

A project task runs commands on whoever pulls it, so it enters the project trust hash ([trust.md](./trust.md)) and needs two approvals. Trust approves the config contents; the machine-local enablement record ([History, strikes, and arming](#history-strikes-and-arming)) approves that task for unattended execution here. `rimz loop add --project` writes an enabled record for its author, and a clone has no record, so the task starts disabled.

### Visible and runnable precedence

The catalog resolves two maps, and the split keeps an untrusted project task honest.

- **Visible** is what `rimz loop list` shows. A project definition replaces a same-named instance or machine row regardless of trust, rendered as `project · untrusted` or `project · stale`, so the list shows the definition that would win.
- **Runnable** is what firing reads. A trusted project row replaces a same-named base row; an untrusted one never does, so the base row keeps running. The base is instance rows overlaid by machine rows.

An untrusted project row with no same-named base row still enters the runnable map, so the refusal happens where tasks execute. `fire::runnable_tasks_for`, which both the elder and `fire_signal` call, drops every untrusted project row, and the trust gate in `rimz loop run` refuses one, except that a manual fire on a terminal offers an inline grant. During the untrusted window the user sees the project task, the machine task keeps running, and the two never both fire.

## Triggers

`parse_trigger` compiles the timing half of a row into one `Trigger`.

| Trigger | Entry | Fired by |
| --- | --- | --- |
| `Schedule` | `at`, `every`, or `cron` | the elder tick or the external tick, when `due` says so |
| `Signal` | `signal = "<selector>"`, optional `match = { k = "v" }` | the process that emits a matching signal |
| `Watch` | `watch = "<shell>"` | the detached `rimz wait watch` process that ran the command, or the elder's watch-lost rule |

A `SignalSelector` is `Exact(SignalName)` or `Family(String)`, parsed from `a.b` or `a.*` and serialized back to the same string. `*`, `a.b.*`, and `a*` are rejected, and emission refuses wildcards outright.

Validation rejects the shapes that cannot mean anything:

| Error | Shape |
| --- | --- |
| `TriggerConflict` | two trigger families on one row |
| `MatchWithoutSignal`, `OnceWithoutSignal` | `match` or `once` with no `signal` |
| `WatchWithCheck` | `watch` with `check`; the watched command is the check |
| `BadSignal`, `BadWatch` | an unparseable selector or an empty command |
| `ObsoleteCiSignal` | `ci.finished`, or a `conclusion` match on a `ci` selector; the message names `ci.passed` and `ci.failed` |

A `Watch` row is always one-shot, and a `Signal` row is one-shot only with `once = true`. `ephemeral_lifetime` names the rows that retire themselves: any row with no repeating trigger, plus any row carrying a `deadline`, `once = true`, or a `watch` command.

`Trigger::resolve` is the whole matching rule, with three outcomes:

| Trigger | `Deliver` | `Skip` | `Ignore` |
| --- | --- | --- | --- |
| `Signal` | every `match` key passes, and the selector is this exact name or this signal's family | every `match` key passes, but an exact selector names another member of the same family | a different family, or a failed `match` key |
| `Watch` | the internal `wait.<task-name>` signal for this row | never | everything else, so one watcher's completion never fires another wait |
| `Schedule` | never | never | always; clocks are not signals |

A `match` key compares a JSON string payload value to the raw text and any other JSON value to its compact encoding.

## Schedule shapes

| Shape | Entry | Due when |
| --- | --- | --- |
| One-shot | bare `at = "07:00"`, or `rimz loop add --in 30m` | its calendar time arrives; the task then removes itself |
| Interval | `every = "15m"` | elapsed time since the last arm or fire reaches the interval |
| Calendar | `every = "weekday"` plus `at = "07:00"` | the first tick at or after the wall-clock time on a matching day, at most once that day |
| Raw cron | `cron = "*/15 * * * *"` | the in-process five-field matcher matches the current minute, and the last fire was in an earlier minute |
| Poll-until | `every = "2m"` with `check`, `on`, an agent action, and `deadline` | the interval elapses, until the check trips the action or the deadline passes |

The day mask accepts `day`, `weekday`, `weekend`, a range such as `mon-fri`, or a list such as `mon,wed,fri`. Calendar times, cron, `--in`, and `--until` evaluate in the configured `timezone`, or the system zone when it is unset.

The arming stamp sets the edge each shape reads. A calendar task first seen after its time today waits for the next matching day, and a cron task first seen past a matching minute waits for the next match. A tick a few seconds late still fires a calendar task, because the comparison is at-or-after.

`Schedule::next_after` is the display counterpart of `due`. It can return a time at or before now, which means the elder fires on its next tick. `TaskTiming::evaluate` layers the display states on top, checked in this order:

| State | Meaning |
| --- | --- |
| `Blocked(trust)` | a project task awaiting or stale on its trust grant |
| `Disabled(reason)` | a manual or strike disable, or a project task not yet enabled on this machine |
| `Paused(t)` | a bounded pause whose deadline is still ahead |
| `Invalid` | the timing half of the row failed to parse |
| `Unarmed` | neither a room elder nor the external tick has stamped it |
| `Upcoming(t)` | the next occurrence is in the future |
| `Due(t)` | the next occurrence is at or before now |
| `NoOccurrence` | parsed and armed, but the shape yields no next time, such as a cron expression whose fields never match a real date |
| `Listening { name }` | a signal subscription, which has no next time |
| `Watching { command }` | a watched command, whose watcher owns the timing |

## Elder firing

`fire::fire_due_tasks` runs on the elder's data tick:

1. Load the runnable tasks for the room's project root, dropping untrusted project rows.
2. Keep only tasks whose normalized `root` maps to this room's `WorkspaceId`, so each room fires only its own tasks. `rimz loop add` writes a canonical absolute root; a hand-edited `~` or relative root is expanded and canonicalized before the ownership check, display, and execution.
3. Plan every task against `loop-fire.json`, the per-room map from task name to last-fire timestamp in the workspace runtime directory.
4. Write the new state, then spawn a detached `rimz loop run <name>` with null stdio for each fire.

The plan decides each task from its stamp, first matching row wins:

| State | Action |
| --- | --- |
| no stamp | arm: record now, do not fire |
| stamped, disabled or pause active | keep the stamp unchanged |
| stamped, schedule due | fire: record now |
| stamped, watch row with no lock holder after the 30-second grace | watch lost: record now and fire the `Lost` outcome |
| stamped, not due | keep the stamp |

Because state is written before any runner spawns, a fire is at-most-once per occurrence even when ticks are hot.

### The external tick

`rimz loop timer install` writes one user-level systemd timer on Linux or a launchd agent on macOS. Once a minute it runs the hidden `rimz loop tick`, which exits after one pass. The timer is only a clock host: it re-reads configuration every pass and leaves arming, trust, overlap locks, execution, and history to the paths above.

The tick finds roots from machine and instance task entries plus the trust grants that contain project tasks; trust grants are the project-root registry, because an ungranted project task cannot run anyway. For each root it derives the same `WorkspaceId` and runtime paths a room would. A fresh sidebar heartbeat means an elder owns the root, and the tick skips it. Otherwise the tick prepares the runtime directories and calls the same planner with the root supplied explicitly, so a root that has never been opened needs no workspace record for its task to arm.

The runners must outlive the tick. Under systemd the tick starts each one through `systemd-run --user --scope`, moving it out of the timer service's cgroup so the runner and any multiplexer server it births survive, and waits up to five seconds to see the child's cgroup change. External runners also get their own process group, on launchd too. Elder and signal fires keep their inherited process group, so interactive launch probes cannot stall on background-terminal job control.

An external fire opens the room it needs. A `Spawn` fire births a room through the supervised-run path. A scheduled check-only fire, including a pure shell check, ensures its root's room is open after the budget, run-lock, and deadline gates and before it runs the check: a fresh heartbeat skips the repair, and otherwise it uses the normal detached room entry (durable ownership, the `rimzd` loop zone, closed-agent resume off, no confirmation prompt). The runner stays a per-fire supervisor outside the panes, and the room hosts any agents it launches. Both paths leave the room open, so later ticks yield to its elder. A `Deliver` fire still requires its pinned live session. A manual `rimz loop fire` stays in the foreground, with no transient scope and no room birth for a check-only task.

A room can be born between the tick's heartbeat check and its state write. That one-tick race is covered by the same defenses as hot elder ticks: the shared fire stamp and the per-task run lock.

## One fire

`rimz loop run <name>` is hidden. After CLI trust and action validation it hands the fire to `schedule::runner::TaskFire`, which owns one fire from its start time through exactly one history transition. The CLI executes the effect `TaskFire` prepares and reports the typed result back.

`TaskFire::prepare` walks an ordered ladder. Everything that can refuse cheaply refuses before anything expensive or observable happens.

| # | Gate | Records on refusal |
| --- | --- | --- |
| 1 | the task's `budget-per-day`: the cost of every run of this task and root on the configured local day, including failed ones, against the cap, reserving the per-run `budget` | `budget skipped` |
| 2 | the room-fleet and provider-account [scope caps](./budget.md#the-fail-fast-gate) | `budget skipped` |
| 3 | the exact managed-launch provider quota, when a binding is proven | `budget skipped` |
| 4 | `surplus` / `surplus-after` forward headroom on the provider's longest window | `surplus skipped` |
| 5 | the per-task run lock | `overlapped` |
| 6 | the poll-until `deadline` | `expired` |
| 7 | the `check` command and its polarity | `skipped`, or a check-only terminal result |

Gate 1 trips when the day's spend has reached the cap, or when spend plus the per-run `budget` would exceed it. A `budget-per-day` without a `budget` is an error.

Then the action runs. `TaskFirePlan` returns `Done` (a gate already produced the terminal record), `Spawn` (a prepared `SupervisedRunRequest`), or `Deliver` (a prepared target and prompt). The CLI executes it and calls `finish`, which maps the outcome to a `LoopRunResult` and appends the record. Scheduled and manual fires walk the same gates, so `rimz loop fire` tests the real policy.

A closed gate costs nothing and adds no strike; a recurring task keeps polling until the condition clears. The surplus gate fails closed: an account with no window reading keeps it shut, because spending against an unknown budget is the failure the gate exists to prevent.

The run lock is `loop-run-<name>.lock` beside `loop-fire.json`, holding the holder's `{pid, started_at}`. The kernel releases it when the runner exits or crashes, and display probes read it without rewriting it. `runner::stop_task` is the stop ladder behind `rimz loop stop`, with the CLI passing its supervised cancellation in as a closure:

1. Probe the lock; a free lock reports no active run.
2. Cancel the newest active run through the durable path and wait five seconds.
3. Send SIGTERM to the holder and wait five seconds more. Only this step appends a `canceled` row from the stop path itself.
4. A holder that still owns the lock is not escalated to SIGKILL; the error names its PID and the lock path.

An ephemeral task removes its own row before its supervised run starts, so a one-shot that then fails to launch is not retried. A delivery removes the row once dispatch returns, whether it succeeded or errored, so the wake's message record exists before its row disappears and turn-completion waits never see the agent rested in between ([messaging.md § Reply waits](./messaging.md#reply-waits)). A crash between dispatch and removal leaves the row to fire again. A poll-until row also removes itself when its check fires the action, and expires without delivery once its deadline passes. A watch check-in is nonterminal and leaves the row in place; the final outcome consumes it.

### Where a scheduled run lands

A scheduled `Spawn` fire sets `loop_zone` on its request, which puts the transient pane in the `rimzd` loop zone ([scripting.md § Runs the scheduler starts](./scripting.md#runs-the-scheduler-starts) lists the rest of the loop-owned request shape). The runtime column's loop panel stays open and run panes stack under it, so a fire never splits the sidebar or a working tab. Elder-tick repair restores a closed managed pane, and fire-time repair recreates a missing loop panel at once ([rimzd.md](../rimzd.md#who-repairs-and-when)). If the whole `rimzd` view is gone or the split fails, the run opens a new tab. A manual `rimz loop fire` splits beside the caller, so its foreground stream stays local.

A scheduled `Spawn` also gets a timeout it did not ask for. `effective_spawn_timeout` uses the task's own `timeout` when set; otherwise a scheduled fire takes the machine's `loop.default-timeout`, two hours by default (`SCHEDULED_RUN_DEFAULT_TIMEOUT`), and a manual fire stays unbounded. Nobody is watching an unattended run, so it must not wedge forever.

Loop-owned runs, both `Spawn` turns and agents a check launches, close their panes on every terminal status unless explicitly kept. The in-pane wrapper defers self-cleanup while the run's waiter is live, which preserves verification re-prompts and failure-tail capture; `store::run::run_waiter_is_live` probes the bound socket instead of trusting a pathname that may be stale. If the waiter dies, including when a check times out, the wrapper reclaims the pane once its record is terminal. The evidence survives the pane: the supervised `RunRecord` keeps the failure tail, transcript path, and status, and the `LoopRunRecord` keeps the fire's outcome and check evidence for `rimz loop logs`.

### Checks

`check = "<shell>"` runs through `sh -c` before any agent action, in the task's `dir`, else the linked worktree it was armed from, else the project root.

| Field | Values | Default |
| --- | --- | --- |
| `on` | `fail` fires on a non-zero exit or a timeout; `success` fires on exit 0 | `fail` |
| `timeout` | a duration such as `5m`; the process group is killed at the deadline | 5 minutes |

A hidden `loop check-exec` trampoline calls `setsid` before it executes the shell, which drops the caller's controlling terminal and keeps the check and any nested waiter in one killable process group. A manual interrupt is forwarded to that group, with a one-second grace before an unresponsive check is killed; the fire records `canceled` and exits 130 without evaluating polarity or launching an agent. Either stop waits at most 200 ms for the output drains, so a pipe held by an escaped process cannot block the history row.

Every check, scheduled or manual, receives `RIMZ_LOOP_TASK=<task-name>`, `RIMZ_WORKTREE_PATH` set to its execution directory, and the workspace pin (`RIMZ_WORKSPACE_ID`, `RIMZ_PROJECT_ROOT`) from `workspace::pin_env`, which binds nested RimZ commands to the task's root.

A prompted `rimz agents <cell> "<prompt>"` launched from a check becomes a loop-owned supervised run. `-p` is implied, the command blocks for the result, the final message goes to the check's captured stdout, and the exit code reflects the run status. Without an explicit worktree selection the agent lands in the check's own checkout, whereas the task's `agent` action runs at the canonical project root. The run uses the loop zone, inherits `loop.default-timeout` unless the launch sets a timeout, and cleans up its pane unless `--keep` is set. An unprompted launch is refused because there is no turn to complete, and `--bg` is refused because the check must await the result. The conversion does not apply inside an agent pane carrying `RIMZ_AGENT_ID`, where agent ancestry owns child launches. Room birth clears the check marker so it cannot leak into later launches from a human shell.

A check-only task logs `completed`, `failed`, or `timed out` with the exit code and capped combined output, and recurs unless it is ephemeral. A guarded task logs the check evidence whether it skips or fires.

A terminal `Watch` outcome takes the same path with the command already run: `prepare_check` converts the signal's `WatchOutcome` instead of executing anything, and polarity applies unchanged. A watch defaults to `on = "any"`. A `Lost` outcome converts to a failed check, so `any` still delivers it and `on = "success"` skips it. A polarity skip records `skipped` and consumes the ephemeral wait without delivering a message. A `Running` check-in bypasses polarity and consumption.

### The prompt a fire delivers

`resolve_effect_prompt` builds the delivered text. The base is `prompt`, or `prompt-file` read at fire time, with a relative path resolved against the machine config directory. `resolve_task_prompt` lets a wait row have no prompt at all, while a `Spawn` row requires one. Nothing is substituted: `{{key}}` is delivered as typed.

A delivery, signal, or watch prompt goes through `compose_wait`: the wait line, the verdict, the evidence, then the loop or team prompt verbatim after a blank line. Durations are elapsed time, not wall-clock.

| Trigger | Wait and evidence lines |
| --- | --- |
| timer | `waited <delay> [<name>]` |
| watch | `WatchVerdict::label`, the name, and `output: <agent-visible path> (<FileSummary::label>)` when a path is present and the file is non-empty, `no output` when it is empty except for a `--pid` wait (`WaitMeta.pid`), whose empty file says nothing about the process; the tail is never inlined |
| signal | `waited on <subject>`, `fired [<name>]`, and compact JSON with the fired `signal` name; the subject adds branch and PR for forge signals, the handle for agents, or the instance for teams |
| manual fire | `fired by hand` |

Self waits carry no note and no armer line. A check-in appends its stop and next-alarm commands.

A guard that fired appends its own block through `augment_prompt`, after the composed body, so the agent wakes already reading the evidence:

```text
--- check `cargo test` exited 101 ---
<output tail>
```

The status in that header is the exit code, `timeout` when the command was killed at its deadline, or `signal` when it has no exit code.

Two patterns fall out of the guard. A watchdog runs a command on a schedule and wakes an agent on failure. A trigger-when-green polls until a command succeeds, then delivers.

`verify` is the mirror image and applies only to `Spawn` tasks: `check` decides whether a turn starts, and `verify` decides whether it counted, through the [same-session re-prompt loop](./scripting.md#verification-re-arms-the-same-run).

### Delivering to a live instance

`wait` pins a task to one exact live kind and session at add time; `--wait @me` and bare `--wait` resolve the caller through the shared CLI resolver. The runner confirms the session is alive, including `ended_at`, before it sends through the ordinary durable message path. A missing or ended session records `target gone` and removes the row.

| Intent | Header | Dispatch |
| --- | --- | --- |
| Self timer or command wait (row has `wait_meta`) | `Type: WAIT` | `Steer` |
| Any `Trigger::Signal` delivery, including team bindings | `Type: SIGNAL` | `Boundary { gate: Done }` |
| Scheduled loop delivery | `Type: WAIT` | `Boundary { gate: Done }` |

The sender is `@rimz`, and delivery inherits the harness smart-compaction default. A boundary delivery reaches an idle agent immediately and parks for a working agent's next `done`; a self wait steers. Receivers acknowledge `SIGNAL` and `WAIT` as attributed harness messages, hidden in rendered transcripts and kept in `--json` ([messaging.md § The message header](./messaging.md#the-message-header)).

## History, strikes, and arming

Every fire appends a `LoopRunRecord` to the user-global `~/.local/state/rimz/loop-runs.log.jsonl`. The log is per-user, so history survives a task being edited or removed. `rimz loop show`, `rimz loop logs`, and health reads filter records by the current workspace root when it is known; records with no `root` stay visible everywhere.

A record carries:

- the resolved task `root`, the result, the mode (`scheduled` or `manual`), the duration, and the error chain;
- check evidence: exit code, timeout flag, capped output, and for a watch row the output file's agent-visible `output_path`;
- the watched command's `WatchVerdict` in `watch`;
- the triggering signal's name and payload;
- the delivery's durable message id and target, or the supervised run id and transcript path;
- the last message, cost, and fresh input and output tokens.

Append caps the stored copies: 4 KiB of check output, 2 KiB each of error text and last message, and a signal payload over 4 KiB collapses to a single `_truncated` field. `watch` and `output_path` are `#[serde(default)]`, so a record without them reads back as `None`. A renderer that finds `watch` set uses `WatchVerdict::label` for the outcome words and `elapsed_ms()` for the duration; any other row renders the `exit <n>`, `timeout`, or `signal` segments. `rimz loop show` reads the log for a health verdict plus a separate rollup of agent runs for check-gated work, and `rimz loop logs` prints the stored forensics in full, including the output path above the check gutter.

`LoopRunResult` has fifteen variants, and `strikes::classify` sorts each record into one of three outcomes. A record whose `watch` verdict is nonterminal (a `Running` check-in) is neutral before the result is read. `CheckSkipped` and `SignalSkipped` both render as `skipped` and serialize distinctly (`check_skipped`, `signal_skipped`): one is a guard that declined, the other a sibling signal the subscription observed without delivering.

| Outcome | Results |
| --- | --- |
| Strike | `failed`, `verify failed`, `timed out`, `error`, `budget exceeded`; `completed` or `delivered` whose check did not pass |
| Reset | `completed` or `delivered` with a passing or absent check; `skipped` from a check that passed |
| Neutral | `budget skipped`, `surplus skipped`, `overlapped`, `canceled`, `expired`, `target gone`; `skipped` from a failed or absent check; `skipped` from a sibling signal; any nonterminal watch check-in |

The table encodes two judgements. A turn that completed but left its check red is a failure, because the task is not doing its job. A gate that declined to spend money is not a failure at all.

`record_transition` appends the history row and then updates the overlays. It is the only writer to the log: a fire, an observed sibling signal, and `rimz loop stop` all reach it, and no CLI code appends a row. The append is best-effort, since `disk::rotating` logs its own failure at debug and returns, so the `RunTransition::Recorded` it returns means the append was attempted before the overlays moved, not that the row reached disk.

When consecutive strikes reach the threshold (`max-strikes`, default 3, `0` disables), the task auto-disables, displays `disabled · N strikes`, and fires `loop_disabled` notification handlers. `rimz loop enable` clears the counter and re-arms. `rimz loop fire` still works on a disabled or paused task, for testing.

Two machine-local overlays hold this state without editing task definitions, each behind an advisory lock that serializes concurrent runners:

| File | Holds |
| --- | --- |
| `loop-arming.json` | enablement, a bounded pause deadline, and an automatic-disable strike reason |
| `loop-strikes.json` | consecutive failure counts, independent of run-log rotation |

Both key a task by scope: project and instance tasks as `<workspace_id>::<name>`, machine tasks as `machine::<name>`, so a same-named task in another checkout never inherits an enable. Enabling writes the anti-replay edge, and when a timed pause expires its deadline becomes the effective last-fire edge. Either way the schedule waits for its next occurrence instead of replaying what it missed while held. `rimz gc` removes the unscoped `loop-pauses.json`, which nothing reads.

## The signal vocabulary

A signal is a name, a JSON object payload, a source, and, for watched commands, an outcome. The first three persist, and [`store/event.rs`](../../../crates/rimz/src/store/event.rs) owns them as `SignalName`, `SignalSource`, and `SignalEventPayload`. The outcome does not persist, so the runtime `Signal` and its `WatchOutcome` stay in the harness and convert down through `From<&Signal> for SignalEventPayload`.

`SignalName` is lowercase dot-separated segments, each starting with a lowercase letter or digit and otherwise `[a-z0-9_-]`, at most 64 bytes. The family is the first segment. `rimz events emit --source cli` refuses the `RESERVED_FAMILIES` (`agent`, `wait`, `team`, `ci`, `pr`), so a caller cannot forge a lifecycle transition, a forge verdict, or another wait's completion. The hidden `--source forge` is the room's own door and accepts exactly the four forge names.

| Source | Producer | Names |
| --- | --- | --- |
| `Cli` | `rimz events emit <name> --json '{…}'` | anything the grammar accepts outside the reserved families |
| `Forge` | the sidebar's PR-state refresh, which spawns `rimz events emit --source forge` on a transition ([state.md § Push channels](../sidebar/state.md#push-channels)) | `ci.passed`, `ci.failed`, `pr.merged`, `pr.closed` |
| `Lifecycle` | the lifecycle hook, from the events its own store append produced | `agent.started`, `agent.idle`, `agent.waiting`, `agent.failed`, `agent.ended`; `team.idle`, `team.waiting`, `team.failed`, `team.ended` |
| `Team` | `rimz teams flip` and the stage owner's registration re-wake | `team.stage` |
| `Watch` | `rimz wait watch <name>` when its command exits, and the elder's watch-lost rule | `wait.<task-name>` |

### Agent lifecycle signals

`lifecycle_signal` maps lifecycle transitions onto the five agent names:

| Transition | Signal |
| --- | --- |
| `Registered`, `SubagentStarted` | `agent.started` |
| `TurnEnded`, errored | `agent.failed` |
| `TurnEnded`, otherwise | `agent.idle` |
| `AwaitingInput` | `agent.waiting` |
| `Ended`, `Lost`, `SubagentStopped` | `agent.ended` |

`Ended` and `Lost` derive their signal even though the state machine classifies a root session's end as an `Ignored` transition ([`agents/lifecycle.rs`](../../../crates/rimz/src/agents/lifecycle.rs) stamps the row in the reducer instead); every other `Ignored` transition produces no signal. The payload carries `kind`, `session`, `status`, and `errored`, plus `handle` when the card has a name and `parent` for a subagent event. A subscription's `match` can filter on exactly those keys.

### Team signals

`team_lifecycle_signals` derives cohort edges from the same event, right after the `agent.*` derivation and only when the transitioning agent has a `team`. It is pure. The hook passes the member row from the audit projection (which retains ended rows), the live cohort from `team_cohorts`, the complete pending message queue, and the set of sessions holding a pending one-shot wait.

| Name | Edge |
| --- | --- |
| `team.waiting` | the member entered `Waiting` from a non-waiting status |
| `team.failed` | the member's turn ended errored |
| `team.idle` | the live cohort is non-empty, every member is at rest (`Idle` or `Success`), and no live member has a queued message or a pending one-shot wait; emitted only on the false-to-true edge, where the prior view overlays `prior_status` on the member |
| `team.ended` | a terminal event for the member, with no other live member left |

The payload carries `team`, `instance` (`team#channel`), `member` (the qualified handle that tripped it), and `members` with each handle and status. Membership is whatever `team_cohorts` counts as live for that `team#channel`, and a provider-native subagent's transition derives nothing.

`team.idle` is evaluated only on lifecycle events. Removing a pending wait, which only its owner can do, emits nothing by itself; the owner's next turn boundary re-evaluates the cohort, and with no further lifecycle event the evaluation waits for the next member event.

A member the reaper stops (a pane closed with no lifecycle hook, [`store/writer/reap.rs`](../../../crates/rimz/src/store/writer/reap.rs)) passes through no hook, so it derives neither `agent.ended` nor `team.ended`.

`team.stage` comes from `harness::team_stage`, not the cohort derivation, and `rimz events emit` cannot produce it. Its payload carries `team`, `instance`, `from` (absent when the first flip opens the board), `to`, `owner` (absent for `Done`), `by` (a role name, `user`, or `rimz`), optional `note`, `board` (absolute path), and `at` (RFC 3339). A flip writes the board and ledger before appending the signal with `SignalSource::Team`. Root-member registration re-wakes the current owner with `from == to` and `by = "rimz"`, which covers resume, restart, single-member restart, and rebirth without editing the board or compacting.

The stage owner does not learn of a flip through a subscription. `team_stage` dispatches a `Type: STAGE`, `From: @rimz` message straight to the owner at the `done` boundary: prose naming the flip and a `Note:` line, or for a registration re-wake, prose telling the owner to continue from the board's last Progress line. When the team declares a leader and the owner is not it, the body ends with the seat's channel rule (report to the stage file, reach the user through the leader, no pane text). `Done` and a self-owned flip send no message, and an owner who is not live is woken when it registers. Explicit `team.stage` subscribers still fire independently with the loop's signal body, re-wakes included.

### What persists

`rimz events emit`, the wait watcher, stage flips and re-wakes, and the hook's team derivation append a `signal.emit` event through the ordinary store commit ([store.md](../store.md#what-is-in-it)), so `rimz events follow` replays them. `agent.*` signals append nothing extra, because the `agent.lifecycle` record they derive from is already the durable trace. The durable record is the `SignalEventPayload` (name, payload, source), so a watched command's exit status travels in the fire's argv and never reaches the log.

### Firing subscriptions

`fire_signal` delivers a signal to its subscribers, in the emitting process:

1. Load the runnable tasks for the project root, drop untrusted project rows, and keep tasks whose resolved root maps to this workspace.
2. `resolve` each task's trigger against the signal, dropping `Ignore` results.
3. Drop any task whose arming overlay is not `Live`, so a disabled or paused subscription stays quiet.
4. On `Skip`, append a `SignalSkipped` run record carrying the observed signal and spawn nothing. On `Deliver`, spawn a detached `rimz loop run <name> --signal-json <encoded>` and return the name for the emitter to print.

`fire_signal` never touches an instance row, so a sibling observation leaves the subscription armed. The runner consumes a one-shot once its delivery is dispatched, and a standing subscription stays.

Nothing queues a match. Signal firing leaves `loop-fire.json` untouched, and a subscription written one second after the emit misses it. The elder may stamp a signal row when it first sees the catalog, but `fire_signal` never consults that stamp. A signal reaches only the subscriptions armed in that workspace at that instant, which is what lets an emitter run with no room open.

### The watch verdict

A watched command's signal carries a `WatchOutcome`: a `WatchVerdict` plus its evidence (the output tail, the agent-visible path of the complete output file, and its byte size, physical line count, and estimated tokens in `summary`). The verdict is the output-free half, one enum with one renderer.

| Variant | `label()` |
| --- | --- |
| `Exited { code: Some(0), elapsed_ms }` | `exit 0 after 4m` |
| `Exited { code: None, elapsed_ms }` | `killed by signal after 3s` |
| `Running { elapsed_ms }` | `still running after 30m` |
| `TimedOut { elapsed_ms }` (read from old records only) | `timed out after 59m` |
| `Lost { detail, elapsed_ms }` | `watcher died after 3m; the command may still be running or may have died with it` |

`WatchVerdict::label` is the only place those words are written; `compose_wait`, `rimz loop logs`, and `rimz loop show` all render through it. `passed()` is `Exited { code: Some(0) }` and nothing else. `elapsed_ms()` is the measured run, rendered in seconds under a minute and through `theme::fmt::duration_label` above it. `is_terminal()` is false only for `Running`, the check-in that bypasses polarity, adds no strike, and does not consume the row.

For terminal outcomes, `to_check_outcome` folds the verdict into the check machinery: `passed()` becomes the pass bit, `TimedOut` the timeout flag, and the tail the output. A `Lost` outcome's output is the log file's tail, since the label already states the cause.

`WatchOutcome` travels only in the `rimz loop run` argv and the process memory around it, so reshaping it is not a durable-format change. The verdict becomes durable one level up, in the run record's `watch` field.

## Watched commands

`arm_delivery` ensures the private room tmp layout, creates `tmp/rimz-waits/<name>.output`, and spawns a detached `rimz wait watch <name>` with null stdin and stdout, the output file as stderr, and `process_group(0)`. A spawn failure rolls the row back.

`signal::run_watcher` takes `loop-watch-<name>.lock` in the workspace runtime directory before it reloads and checks the catalog row, which closes the race with a cancel that lands before the watcher starts. The lock holds `{pid, started_at}`, and a second watcher for the same task exits without running the command.

The command runs in the task's `dir`, else the linked worktree it was armed from, else the project root. The watcher drains stdout and stderr into the output file and keeps a 4 KiB tail. It uses `CheckInOnce` with its own timeout, 30 minutes by default and independent of `loop.default-timeout`; ordinary `check` and `verify` still kill at their timeout. At the deadline an observed exit wins. Otherwise the watcher fires one `Running` outcome, with the tail and the output file measured so far, and keeps draining until the command exits. The check-in does not kill the command, filter on `on`, consume the row, or change strikes. Its body includes `Stop it: rimz wait cancel <name>` and `Another check-in: rimz wait --in <timeout>`, a timer independent of the running command.

A watcher-originated fire runs `rimz loop run <name> --signal-json …` and waits for it while still holding the watcher lock, so the check-in delivery cannot overlap the terminal fire. An append or fire failure is logged and the final outcome is still attempted. Every other signal emitter spawns its runs detached.

Cancel removes the row first, then `stop_watcher` sends SIGTERM to the lock holder's process group, stopping the watcher and its command together. Non-positive PIDs are rejected, and an absent process counts as stopped. If a watcher dies without firing, the elder's watch-lost rule fires the `Lost` verdict after the 30-second grace, with the output file's tail as evidence.

`signal::wait_output_path` derives `<StatePaths.tmp_dir>/rimz-waits/<name>.output` for arming, watching, and lost-watcher evidence. `WatchOutcome::measured` records the file's byte size, line count, and estimated tokens (`utils::tokens::estimate` over at most the first 1 MiB, scaled by length beyond it), and maps the host path through `sandbox::TmpView::current` (machine policy, since no recipient is known yet) to the agent-visible `output_path`: `/tmp/rimz-waits/<name>.output` under sandbox isolation, the host path otherwise. A failed measurement warns and records a zero summary, which renders as `· no output` like an empty file. Room teardown removes the file; in a long-lived room gc prunes it only when there is no catalog row, no running watcher, and no write in the 14-day retention. The run record keeps the tail for `rimz loop logs`.

## Waits

A wait is a session-pinned delivery row: a `Deliver` task whose target is one live agent. Every delivery row is an instance row, and every one is built by `schedule::arm::arm_delivery`, which both `rimz wait` and `rimz loop add --wait` call.

`DeliverySpec` carries a typed trigger, name, prompt, target, and provenance. The provenance decides what the builder accepts:

| Provenance | Accepts | Name |
| --- | --- | --- |
| `SelfWait` | a delay, a PID, or a watched command; no prompt, check, surplus gate, or deadline | minted, workspace-unique `wait-<petname>` |
| `Loop` | a named clock or signal delivery | given |
| `Team(instance)` | a standing signal subscription | generated ([Team bindings](#team-bindings)) |

`SelfWait` is the only provenance that writes `wait_meta`: `armed_at`, and the optional `delay` and `pid`. A name that belongs to a project task, or for a team row to a machine task, is refused as configuration-owned.

`rimz wait` accepts a positive delay under 24 hours, a positive `--pid`, or a command after `--`, and resolves the live calling agent through `@me`; a user shell cannot arm or cancel. `DeliveryTrigger::Pid` lowers to the watched shell command `while kill -0 <pid> 2>/dev/null; do sleep 1; done`, reusing check-ins, cancellation, and delivery with no scheduler trigger of its own. It observes whether the PID is accessible, not process identity or exit status, and it rejects `--on`.

For signal deliveries, the builder applies caller-first, target-fallback defaults and the other-agent lifecycle guard ([cli/loop.md § Caller-scoped defaults](../../reference/cli/loop.md#caller-scoped-defaults)). One locked instance mutation then compares live rows by kind and session, parsed selector, normalized matches (absent equals empty), and resolved root. A duplicate returns `AlreadySubscribed` and rewrites nothing: not its name, prompt, provenance, or overlays.

Arming and canceling print the caller's pending rows after the receipt. A list from an agent includes loop and team subscriptions for its pinned session; from a user shell it is room-wide and read-only. Cancel requires a caller and takes a name or `--all`. Command previews use `theme::fmt::command_preview`: at most 120 Unicode scalars, keeping the first 60 and last 59 around an ellipsis, with stored commands and logs unchanged.

`schedule::arm::retire_session` removes every instance row pinned to a kind and session, paused and disabled rows included, stops their watcher groups, and clears their arming and strike overlays, attempting every cleanup and aggregating failures. Lifecycle `Ended` and `Lost` call it after the durable event and before event signals; an explicit agent-tree stop calls it after each successful node stop. `delivery_target_alive` rejects a session with `ended_at` set, and gc is the backstop. An instance row is runnable or it is garbage: gc also reaps any `Instance` row whose action no longer compiles (a target lost to schema drift can never fire or retire), while machine and project `loop.toml` rows with an invalid action stay listed as `<invalid>` for the user to fix. Hook config, arming, and retirement failures are logged as warnings and never reach hook stdout.

`pending::project_pending_waits` projects armed one-shot delivery rows onto their target agents as `pending_waits`, which the sidebar and `rimz agents` read. Timers, PID waits, watched commands, and one-shot or deadline signal deliveries count; standing subscriptions and recurring clocks do not. A watch row whose `wait_meta` has a `pid` projects as a PID wait and any other watch row as a shell watch; no command string is parsed. What a pending wait does to the displayed status is [model.md § Sleeping](../agents/model.md#sleeping), and how a card draws it is [the interface reference](../../interface/sidebar.md). A live member's pending one-shot wait also withholds `team.idle` ([Team signals](#team-signals)).

### Team bindings

A team role declares standing subscriptions for its own seat. Each `[[agents.teams.<name>.roles]]` entry has a `signals` array of inline tables with `signal`, an optional string-valued `match`, and an optional `prompt`; the containing role is the receiver. `prepare_team` validates the selectors and explicit other-agent matches for `agent.*`. Each role's complete ordered binding list enters the executable trust hash, and an empty list leaves the hash unchanged, so moving bindings between roles needs a fresh grant.

A fresh `launch_layout` refuses a team binding scoped implicitly to the root checkout's CI or PR state, before any pane or worktree side effect, and names the fixes: an explicit branch or path match, or `-w <worktree>`. A launch from a linked worktree passes, a `--from-pr` launch skips the check, and resume does not apply it.

After a committed lifecycle `Registered`, the hook reads the strict effective trusted config and calls `schedule::team::arm_member` for a root team member, never a subagent. It uses the member's adopted session identity, role, channel, and worktree, so launch, resume, restart, and role re-add share one registration path. All binding specs are built before anything persists. The rows carry `TaskEntry.team = "<team>#<channel>"`, target the member's exact kind and session, and deliver at the next `done` boundary.

Row names are `team-<team>-<channel>-<role>-<signal slug>`, lowercased, with dots and any character outside `[a-z0-9_-]` replaced by `-`. Repeated or colliding slugs within a role get a declaration-order `-<n>` suffix; a collision between lanes in one room is accepted. Repeated registration deduplicates, and an already-subscribed manual row is not relabeled as a team row. Automatic arming never replaces machine or project configuration: a generated name that collides with a configured task is reported in hook diagnostics, and the user renames that task before the member re-arms.

## Recovery the elder runs

The elder's tick also drives unattended recovery that is not a loop task. Each intervention is documented where its inputs live, and each records itself in the [assist log](#the-assist-log).

| Automation | What it does | Owner |
| --- | --- | --- |
| Auto-continue ([`auto_continue.rs`](../../../crates/rimz/src/harness/auto_continue.rs)) | resumes an agent parked on a certified rate limit, an overload, or a RimZ budget park, once that park's clock is due | [providers.md § Auto-continue](../agents/providers.md#auto-continue) |
| Auto-redeem ([`auto_redeem.rs`](../../../crates/rimz/src/harness/auto_redeem.rs)) | spends a Codex reset credit when it buys capacity | [providers.md § Auto-redeem](../agents/providers.md#auto-redeem) |
| Idle compaction ([`idle_compact.rs`](../../../crates/rimz/src/harness/idle_compact.rs)) | compacts an idle agent's context past the configured threshold | [messaging.md § Idle compaction](./messaging.md#idle-compaction) |
| The budget park | interrupts an agent over a dollar cap and arms its day reset | [budget.md § The park](./budget.md#the-park) |

Each decides on the producer tick and acts through a detached helper, which keeps store-writing code out of the sidebar's import graph ([sidebar.md](../sidebar/sidebar.md)).

## The assist log

> **Automation is accountable.** User-benefiting automation appends a durable record of its trigger, evidence, and outcome, and surfaces in `rimz stats`; internal repairs keep durable diagnostic records ([diagnostics.md](../diagnostics.md)).

The assist log is that invariant's record: `$XDG_STATE_HOME/rimz/assists.log.jsonl`, account-global, best-effort append, rotating at 4 MiB to one `assists.log.1.jsonl` predecessor. Readers fold both generations by timestamp. When an append fails, the intervention itself remains the operational truth.

| `Assist` variant | Writer | Records |
| --- | --- | --- |
| `auto_redeem` | the detached redeem helper | provider, decision reason, request id, available credits, soonest expiry, the natural reset it beat, consume outcome or error, whether a reset occurred, and refreshed window stamps |
| `auto_continue` | the detached continue helper | typed provider and session ids, display handle, park class, original park timestamp, delivery verdict, and the durable message id |
| `auto_compact` | the message delivery path, after a compact command lands | target session, display handle, threshold, occupied context when known, and the durable compact-command message id |
| `idle_compact` | the detached idle-compaction helper | target provider and session, display handle, idle duration, occupied context, durable compact-command message id, delivery verdict, and error when present |
| `flip_compact` | `rimz teams flip` | flipper provider and session, role, previous and target stages, occupied context when known, effective threshold, the attempted compact-command message id (which may not resolve if a store error prevented publication), delivery verdict, and error when present |
| `auto_resume` | rebirth recovery, after materialization restores at least one pane | workspace, session, death cause, recovered pane count, and planned tab labels |

`rimz stats` folds both generations, scoped to the active dashboard window, into one rollup: delivered continues and their summed recovered time (`recorded_at - parked_since`), compact commands sent, redeem attempts and `reset` outcomes, and rebirths with restored panes. The dashboard shows whichever of those four categories are non-zero, `rimz stats --assists` renders the merged event stream newest first, and `rimz stats --json` publishes both.

A flip compaction reads `flip compaction` in the event stream and counts toward compact commands sent only when the command was sent; a flip completing does not count a queued command. Only a member leaving its own stage for one another role owns is eligible; `Done` has no owner and never compacts. The effective `[harness] flip_compact` or role `flip-compact` threshold is evaluated before pane availability: a flip below it, or with unknown occupancy, makes no attempt and writes no record. Every attempt records the threshold, including errors and missing-pane skips, and none of these failures fails the flip.

A new smart strategy ships as one accountable slice: define its typed trigger, evidence, and outcome record; append it from a writer outside the sidebar import graph; fold it into the stats rollup; and add its variant and writer to the table above.

## See also

- [scripting.md](./scripting.md): the supervised run every `agent` task spawns.
- [messaging.md](./messaging.md): the delivery path every `wait` task uses.
- [fleet.md](./fleet.md): launch compilation and addressing.
- [budget.md](./budget.md): the dollar scopes the gate ladder reads, and the budget park.
- [providers.md](../agents/providers.md): account windows, capacity readings, auto-continue, and auto-redeem.
- [sidebar/state.md](../sidebar/state.md#renderers-the-producer-and-consumers): how the elder is elected and what else runs on its tick.
- [cli/loop.md](../../reference/cli/loop.md): every flag on `add`, `fire`, `list`, `show`, and `remove`.
- [cli/wait.md](../../reference/cli/wait.md) and [cli/events.md](../../reference/cli/events.md): the user-facing wait triggers and the signal emit surface.
