# Loop scheduling

> The scheduler: where task definitions live, who keeps time, what one fire does, how signals and wakes reach the same machinery, and the unattended recovery the same elder runs alongside it. [fleet.md](./fleet.md) is the map for this area. For users, the guide is [loops.md](../../guide/loops.md) and the flag references are [cli/loop.md](../../reference/cli/loop.md), [cli/wake.md](../../reference/cli/wake.md), and [cli/events.md](../../reference/cli/events.md).

## What the scheduler does

`rimz loop` fires agent work on a trigger: a fresh supervised turn, a prompt delivered to an agent that is already running, or a shell command that guards either one. The trigger is a clock, a signal selector, or a watched command; `rimz wake` is the agent-facing front end over the same rows.

The design constraint that shapes everything is that **there is no RimZ scheduler daemon**. RimZ is a room, and the room already elects one process to do shared work: the sidebar producer, the elder ([state.md § Renderers, the producer, and consumers](../sidebar/state.md#renderers-the-producer-and-consumers)). The elder keeps time for loop tasks on its ordinary data tick. Users who need closed-room schedules may opt into one global OS timer, which launches a one-off RimZ tick and exits; it is not a resident RimZ process and it yields every root whose room is open.

Three rules follow from that shared scheduler, and they explain most of the module.

**Arming is not firing.** A task the elder has never seen is recorded with the current time and does *not* fire. Opening a room hours late therefore never replays what was missed, so there is no catch-up storm.

**Every fire is idempotent at the tick.** The elder writes the fire timestamp before spawning the helper, so a hot sub-interval tick cannot spawn the same occurrence twice, and a per-task advisory lock stops two overlapping runs.

**An event fires in the process that produced it.** A clock needs a timekeeper, an event does not: whoever emits a signal resolves the subscribers and spawns their runs itself, so signal and watch triggers work with no room open and never touch `loop-fire.json`. The cost of having no queue is that signals are never replayed ([below](#the-signal-vocabulary)).

**Exactly one history row per fire.** However a fire ends, gated, skipped, overlapped, expired, delivered, completed, or errored, it appends one record to the run log. That log is the only durable trace of what automation did, so it must be complete.

## Module layout

| File | Owns |
| --- | --- |
| [`schedule.rs`](../../../crates/rimz/src/harness/schedule.rs) | The vocabulary: `TaskAction`, `Trigger` and its parsing, `Schedule`, `ParsedSchedule`, due evaluation, next-occurrence calculation, `TaskTiming` display states, and the `TaskShape` compile. |
| [`schedule/signal.rs`](../../../crates/rimz/src/harness/schedule/signal.rs) | The runtime signal vocabulary: `Signal`, `SignalSelector`, `WatchVerdict` and the `WatchOutcome` that carries it, the lifecycle-to-signal mapping, the `From<&Signal>` conversion into the durable payload, in-process `fire_signal`, `wake_log_path`, and `run_watcher`, the whole watched-command lifetime from the lock to the fire. |
| [`store/event.rs`](../../../crates/rimz/src/store/event.rs) | The persisted signal vocabulary the harness converts into: the `SignalName` grammar and its reserved families, `SignalSource`, and the `SignalEventPayload` that `Store::append_signal` records ([store.md](../store.md#what-is-in-it)). |
| [`schedule/signal/team.rs`](../../../crates/rimz/src/harness/schedule/signal/team.rs) | The pure cohort-edge derivation behind `team.idle`, `team.waiting`, `team.failed`, and `team.ended`. |
| [`schedule/runner/prompt.rs`](../../../crates/rimz/src/harness/schedule/runner/prompt.rs) | `compose_wake`: the armer line, the wait line, the verdict line, the evidence, and the verbatim note. |
| [`schedule/runner/status.rs`](../../../crates/rimz/src/harness/schedule/runner/status.rs) | The deadline answer: the PR-state cache lookup for a signal wake's scope, the `Answered`/`Open` split, the status label, and `rearm_command`. |
| [`cli/wake/`](../../../crates/rimz/src/cli/wake) | `rimz wake`: trigger validation, target pinning, caller-scoped match defaults, petname minting, starting the detached watcher child, the inline `--wait` join, and `list`/`cancel`. No scheduling policy. |
| [`schedule/catalog.rs`](../../../crates/rimz/src/harness/schedule/catalog.rs) | The task catalog: the three sources, visible and runnable precedence, source-aware mutation, and scheduled consumption. |
| [`schedule/fire.rs`](../../../crates/rimz/src/harness/schedule/fire.rs) | Shared elder/external firing: root ownership, arm-on-first-sight, due planning, `loop-fire.json`, and spawning the detached `rimz loop run <name>`. |
| [`cli/loop_timer.rs`](../../../crates/rimz/src/cli/loop_timer.rs) | Shared loop CLI integration for systemd user-timer and launchd-agent install, status, removal, unit rendering, and the external tick. |
| [`schedule/runner.rs`](../../../crates/rimz/src/harness/schedule/runner.rs) | `TaskFire`: the ordered gate ladder, the run lock, the check, prompt preparation, the prepared effect, and the one terminal history transition; plus `stop_task`, the stop ladder from the lock probe to the SIGTERM backstop. |
| [`schedule/run_log.rs`](../../../crates/rimz/src/harness/schedule/run_log.rs) | `LoopRunRecord`, `LoopRunResult`, the user-global JSONL history, cost rollups, and the daily-budget gate. |
| [`schedule/arming.rs`](../../../crates/rimz/src/harness/schedule/arming.rs), [`strikes.rs`](../../../crates/rimz/src/harness/schedule/strikes.rs) | Machine-local overlays: durable enablement, bounded pauses, their effective-last-fire rule, and consecutive failure counts. |
| [`schedule/instances.rs`](../../../crates/rimz/src/harness/schedule/instances.rs), [`overlay_store.rs`](../../../crates/rimz/src/harness/schedule/overlay_store.rs) | RimZ-owned ephemeral task rows, and the shared locked persistence the overlays use. |
| [`schedule/config_edit.rs`](../../../crates/rimz/src/harness/schedule/config_edit.rs) | Comment-preserving TOML editing for machine `loop.toml` and project `.rimz/config.toml`. |
| [`cli/loop_cmd/`](../../../crates/rimz/src/cli/loop_cmd) | Flag translation, terminal orchestration, executing prepared effects, and rendering. No scheduling policy. |
| [`auto_continue.rs`](../../../crates/rimz/src/harness/auto_continue.rs), [`auto_redeem.rs`](../../../crates/rimz/src/harness/auto_redeem.rs), [`assist_log.rs`](../../../crates/rimz/src/harness/assist_log.rs) | The other automation the elder runs, and its audit trail ([below](#recovery-the-elder-runs)). |

## The task

A task is a name, one action, and one firing shape. `TaskAction::from_entry` derives the action from the persisted entry, and the combinations are deliberately narrow.

| Action | Entry fields | What a fire does |
| --- | --- | --- |
| `Spawn` | `agent = "<cell>"` | opens one transient supervised pane down the [supervised-run](./scripting.md) path |
| `Deliver` | `wake = { kind, session, handle }` | sends the prompt to one pinned live session through the [message](./messaging.md) path |
| `CheckOnly` | `check` alone | runs the shell command and records its outcome |

`agent` and `wake` conflict; one of `agent`, `wake`, or `check` is required. `verify` requires `agent`, because verification needs a supervised run to re-prompt. `max-attempts` requires `verify` and must be at least 1.

A `Spawn` task names exactly one agent cell: a built-in kind, a profile, or an adapter-supported virtual cell such as `claude-auto` or `codex-yolo`. Teams, multi-cell layouts, and command cells are rejected at add time, because a scheduled task owns exactly one supervised pane.

`TaskShape::compile` compiles each persisted row once into an action result and a timing result **independently**. That independence is the point: a row with a valid action and a malformed schedule stays visible and manually fireable while scheduled firing skips it, instead of one bad field hiding the whole task.

## Where tasks live

Three sources back the catalog, and they are not interchangeable.

| Source | File | Nature |
| --- | --- | --- |
| `Config` | `~/.config/rimz/loop.toml` | per-machine automation, like your crontab; never inherited by a clone |
| `Project` | `<root>/.rimz/config.toml` under `[tasks.*]` | shared automation that travels with the repo, and therefore defaults disabled until it is both trusted and enabled on this machine |
| `Instance` | `~/.local/state/rimz/loop-instances.json` | RimZ-owned runtime rows: one-shots, poll-until rows, `once` subscriptions, and every `rimz wake` |

The instance store exists so runtime churn never edits user config. An agent scheduling its own `--in 30m` wake writes state, not your `loop.toml`, and the row retires itself after firing.

Project tasks are more constrained than machine tasks, because a committed task cannot make machine-local claims. Loading rejects `root` (a project task runs at the project root), `wake` (it cannot pin a session on someone else's machine), and `deadline` (a poll-until timestamp is machine state), and requires `every` or `cron`, because a one-shot would have to delete itself from a trust-hashed file on fire.

A project task also runs commands on whoever pulls it, so it enters the project trust hash ([trust.md](./trust.md)) and stays inert until each user grants it. Trust approves the config contents; the separate machine-local enablement record approves that task for unattended execution here. `rimz loop add --project` writes an enabled record for its author, while a cloned task has no record and therefore defaults disabled.

### Visible and runnable precedence

The catalog resolves two maps at once, and the split is what makes an untrusted project task honest.

- **Visible** is what `rimz loop list` shows. A project definition shadows a same-named machine or instance row *regardless of trust*, rendered as `project · untrusted` or `project · stale`, so you always see the definition that would win.
- **Runnable** is what the elder and `rimz loop run` may execute. It admits a project row only when trusted, and otherwise falls back to the same-named machine task.

So during the untrusted window you see the project task and keep running the machine one, without the two ever double-firing.

## Triggers

`parse_trigger` compiles the timing half of a row into one `Trigger`, and the three variants are mutually exclusive.

| Trigger | Entry | Fired by |
| --- | --- | --- |
| `Schedule` | `at`, `every`, or `cron` | the elder tick or the external timer, when `due` says so |
| `Signal` | `signal = "<selector>"`, optional `match = { k = "v" }` | the process that emits a signal in the selector's family |
| `Watch` | `watch = "<shell>"` | the detached `rimz wake watch` process that ran the command, or the elder's watch-lost rule |

A `SignalSelector` is `Exact(SignalName)` or `Family(String)`, parsed from `a.b` or `a.*` and serialized back to the same string; `*`, `a.b.*`, and `a*` are rejected, and emission still refuses wildcards outright.

The validation is where the shapes stay honest: any two trigger families together are a `TriggerConflict`; `match` and `once` without `signal` are `MatchWithoutSignal` and `OnceWithoutSignal`; `watch` with `check` is `WatchWithCheck`, because the watched command *is* the check; an unparseable selector or an empty command is `BadSignal` or `BadWatch`; `ci.finished`, or a `conclusion` match on a `ci` selector, is `ObsoleteCiSignal`, which names the outcome replacements. A `Watch` row is always one-shot, and a `Signal` row is one-shot only with `once = true`.

`Trigger::resolve` is the whole matching rule, and it has three outcomes rather than a bool. A `Signal` trigger returns `Ignore` when the families differ or any `match` key fails against the payload (comparing a JSON string to the raw value and any other JSON value to its compact encoding), `Deliver` when the selector is that exact name or its family, and `Skip` for another member of the same family that passed the matches. A `Watch` trigger resolves only the internal `wake.<task-name>` signal to `Deliver`, which is what keeps one watcher's completion from firing another wake. A `Schedule` trigger ignores everything: clocks are not signals.

`ephemeral_lifetime` decides which rows retire themselves: anything with no repeating trigger, plus any row carrying a `deadline`, `once = true`, or a `watch` command. A signal wake carries a `deadline`, so it is ephemeral like the rest, and the delivery path removes it under the instance lock rather than through the catalog ([below](#the-deadline)).

## Schedule shapes

| Shape | Entry | Due when |
| --- | --- | --- |
| One-shot | bare `at = "07:00"`, or `rimz loop add --in 30m` | its calendar time arrives; the task then removes itself |
| Interval | `every = "15m"` | measured elapsed time since the last arm or fire crosses the interval |
| Calendar | `every = "weekday"` plus `at = "07:00"` | the first tick at or after the wall-clock time on a matching day, at most once that day |
| Raw cron | `cron = "*/15 * * * *"` | an in-process five-field matcher matches the current minute, and the last fire was in an earlier minute |
| Poll-until | `every = "2m"` with `check`, `on`, an agent action, and `deadline` | the interval elapses, until the check trips the action or the deadline passes |

The day mask accepts `day`, `weekday`, `weekend`, a range like `mon-fri`, or a list `mon,wed,fri`. Calendar times, cron, `--in`, and `--until` all evaluate in the configured `timezone`, falling back to the system zone when unset.

The arming stamp sets the edge each shape reads, which produces one behaviour worth internalizing: a calendar task first seen *after* its time today waits for the next matching day, and a cron task first seen past a matching minute waits for the next match, but a tick a few seconds late still fires a calendar task, because the comparison is "at or after".

`Schedule::next_after` is the display counterpart of `due`, and it may return a timestamp at or before now, meaning the elder should fire on its next tick. `TaskTiming::evaluate` layers the display states on top:

| State | Meaning |
| --- | --- |
| `Blocked(trust)` | a project task awaiting or stale on its grant |
| `Disabled(reason)` | a machine-local manual or strike disable, or a project task not yet enabled here |
| `Paused(t)` | a bounded machine-local pause whose deadline is still in the future |
| `Invalid` | the schedule half of the row failed to parse |
| `Unarmed` | neither a room elder nor the external tick has seen it yet |
| `Upcoming(t)` | the next occurrence, still in the future |
| `Due(t)` | the next occurrence is at or before now; the elder fires it on its next tick |
| `NoOccurrence` | parsed, armed, but the shape yields no next time, such as a cron expression whose field combination never matches a real date |
| `Listening { name }` | a signal subscription, which has no next time to compute |
| `Watching { command }` | a watched command, whose watcher owns the timing |

## Elder firing

`fire::fire_due_tasks` runs on the elder's data tick.

1. Load the runnable catalog for the room's project root, dropping untrusted project rows.
2. Keep only tasks whose normalized `root` maps to this room's `WorkspaceId`, so each room fires only its own tasks. `rimz loop add` writes a canonical absolute root; a hand-edited `~` or relative root is expanded and canonicalized before the ownership check, display, and execution.
3. Plan every task against `loop-fire.json`, a per-room map of task name to last-fire `Timestamp` in the workspace runtime dir.
4. Write the new state, then spawn a detached `rimz loop run <name>` with fresh null stdio for each fire.

The plan is a decision per task, and the first row is checked before the stamp is even read:

| State | Action |
| --- | --- |
| live signal wake past its `deadline` | expire: record `now` and spawn `rimz loop run <name> --expired` |
| no stamp | arm: record `now`, do not fire |
| stamped, disabled or pause active | hold the existing stamp unchanged |
| stamped, schedule due | fire: record `now` |
| stamped, watch row with no lock holder past the grace | watch-lost: record `now` and fire the `Lost` outcome |
| stamped, not due | keep the stamp |

State is written before any helper spawns, which is what makes a fire at-most-once per occurrence even when ticks are hot.

### The external tick

`rimz loop timer install` writes one user-level systemd timer on Linux or launchd agent on macOS. Once a minute it invokes the hidden `rimz loop tick`, which enumerates roots from machine and transient task entries plus trust grants containing project tasks. Trust grants are the project-root registry because an ungranted project task cannot run. For each root the tick derives the same `WorkspaceId` and runtime paths as a room. A fresh sidebar heartbeat makes it skip that root; otherwise it prepares the runtime directories and calls the same firing planner with the root explicitly supplied, so a never-opened root does not need a workspace record before its task can arm.

The timer is only a clock host. It re-reads configuration every pass, leaves arming, trust, overlap locks, execution, and run history to the existing paths, and exits after one tick. A `Spawn` fire births a room through the supervised-run path and leaves it open, so later external ticks yield to its elder. `Deliver` still requires the pinned live session; check-only work runs bare. A room can be born between the heartbeat check and the state write, creating a one-tick race, but the shared fire stamp and per-task overlap lock remain the duplicate and concurrency defenses already used by hot elder ticks.

The machine-local `loop-arming.json` overlay holds enablement, a bounded pause deadline, and an automatic-disable strike reason without editing durable task definitions. Project keys use `<workspace_id>::<name>` and machine or instance keys use `machine::<name>`, so a same-named task in another checkout never inherits an enable. Enabling writes the anti-replay edge; when a timed pause expires, its deadline becomes the **effective last-fire edge**. Either lift makes the schedule wait for its next occurrence rather than replaying everything missed while held.

## The signal vocabulary

A signal is a name, a JSON object payload, a source, and, for watched commands, an outcome. The first three persist, and [`store/event.rs`](../../../crates/rimz/src/store/event.rs) owns them as `SignalName`, `SignalSource`, and the `SignalEventPayload` that carries all three into the log. The outcome does not persist, so the runtime `Signal` and its `WatchOutcome` stay in the harness and convert down through `From<&Signal> for SignalEventPayload`. `SignalName` pins the grammar: lowercase dot-separated segments, each starting with a lowercase letter or digit and otherwise `[a-z0-9_-]`, at most 64 bytes. The family is the first segment, and `RESERVED_FAMILIES` (`agent`, `wake`, `team`, `ci`, `pr`) is refused from `rimz events emit --source cli`, so a caller cannot forge a lifecycle transition, a forge verdict, or another wake's completion. The hidden `--source forge` is the narrow door for the room's own refresh and accepts exactly the four forge names.

| Source | Producer | Names |
| --- | --- | --- |
| `Cli` | `rimz events emit <name> --json '{…}'` | anything the grammar accepts outside the reserved families |
| `Forge` | the sidebar's PR-state refresh, which spawns `rimz events emit --source forge` on a transition ([state.md](../sidebar/state.md#push-channels)) | `ci.passed`, `ci.failed`, `pr.merged`, `pr.closed` |
| `Lifecycle` | the lifecycle hook, from the events its own store append produced | `agent.started`, `agent.idle`, `agent.waiting`, `agent.failed`, `agent.ended`, and `team.idle`, `team.waiting`, `team.failed`, `team.ended` |
| `Watch` | `rimz wake watch <name>` when its command exits, and the elder's watch-lost rule | `wake.<task-name>` |

`lifecycle_signal` maps the state machine onto the five agent names: `Registered` and `SubagentStarted` are `agent.started`, an errored `TurnEnded` is `agent.failed` and any other is `agent.idle`, `AwaitingInput` is `agent.waiting`, and `Ended`, `Lost`, and `SubagentStopped` are `agent.ended`. `Ended` and `Lost` derive their signal even though the state machine classifies a root session's end as an `Ignored` transition ([`agents/lifecycle.rs`](../../../crates/rimz/src/agents/lifecycle.rs) stamps the row in the reducer instead); every other `Ignored` transition produces no signal. The payload carries `kind`, `session`, `status`, and `errored`, plus `handle` when the card has a name and `parent` for a subagent event, which is exactly the set a subscription can filter on.

`team_lifecycle_signals` ([`schedule/signal/team.rs`](../../../crates/rimz/src/harness/schedule/signal/team.rs)) derives the cohort edges from the same event, right after the `agent.*` derivation and only when the transitioning agent has a `team`. It is pure: the hook passes the member row from the audit projection (which retains ended rows), the live cohort from `team_cohorts`, and the complete pending message queue.

| Name | Edge |
| --- | --- |
| `team.waiting` | the member entered `Waiting` from a non-waiting prior status |
| `team.failed` | the member's turn ended errored |
| `team.idle` | the live cohort is non-empty, every member is at rest (`Idle` or `Success`), and no member has a queued message; emitted only on the false-to-true edge, computed by overlaying `prior_status` on the member for the prior view |
| `team.ended` | a terminal event for the member and no other live member remains |

The payload carries `team`, `instance` (`team#channel`), `member` (the qualified handle that tripped it), and `members` with each handle and status. Membership is whatever `team_cohorts` counts as live for that `team#channel`, and a provider-native subagent's transition derives nothing. **The gap to know**: a member the reaper stops (a pane closed with no lifecycle hook, [`store/writer/reap.rs`](../../../crates/rimz/src/store/writer/reap.rs)) passes through no hook, so it derives neither `agent.ended` nor `team.ended`.

Two facts about durability are easy to get backwards. `rimz events emit`, the wake watcher, and the hook's team derivation append a `signal.emit` event through the ordinary store commit ([store.md](../store.md#what-is-in-it)), so `rimz events follow` replays them; `agent.*` signals append nothing extra, because the `agent.lifecycle` record they were derived from is already the durable trace. And the durable record is the `SignalEventPayload`, which keeps only name, payload, and source: a watched command's exit status travels in the fire's argv, not in the log.

`fire_signal` is the whole delivery mechanism, and it runs in the emitting process:

1. Load the runnable catalog for the project root, dropping untrusted project rows, and keep only tasks whose resolved root maps to this workspace.
2. `resolve` each task's trigger against the signal, dropping `Ignore` and `Schedule` rows outright.
3. Drop any task whose arming overlay is not `Live`, so a disabled or paused subscription stays quiet.
4. `Skip`: append a `SignalSkipped` run record carrying the observed signal and spawn nothing. `Deliver`: spawn a detached `rimz loop run <name> --signal-json <encoded>` and return the name for the emitter to print.

A wake row's own removal is not in that sequence. `fire_signal` touches no instance row, so a sibling leaves a wake exactly as it found it, deadline included; the spawned run removes the row itself when it prepares the delivery ([below](#the-deadline)).

There is no queue, no persistence of pending matches, and no replay: signal firing leaves `loop-fire.json` untouched, and a subscription written one second after the emit simply misses it. The elder may stamp a signal row when it first sees the catalog, but that clock-side display state is never consulted by `fire_signal`. A signal reaches only the subscriptions armed in that workspace at that instant, which is the property that lets an emitter run with no room open.

### The watch verdict

A watched command's signal carries a `WatchOutcome`: a `WatchVerdict` plus its evidence, the output tail and the path of the file holding all of it. The verdict is the output-free half, and it is one enum with one renderer.

| Variant | `label()` |
| --- | --- |
| `Exited { code: Some(0), elapsed_ms }` | `exit 0 after 4m` |
| `Exited { code: None, elapsed_ms }` | `killed by signal after 3s` |
| `TimedOut { elapsed_ms }` | `timed out after 59m` |
| `Lost { detail, elapsed_ms }` | `watcher died after 3m; the command may still be running or may have died with it` |

`WatchVerdict::label` is the only place those words are written, and `compose_wake`, `rimz wake --wait`, `rimz loop logs`, and `rimz loop show` all render through it, so one outcome cannot read three ways. `passed()` is `Exited { code: Some(0) }` and nothing else; `elapsed_ms()` is the measured run, rendered in seconds under a minute and through `theme::fmt::duration_label` above it. `to_check_outcome` folds the verdict into the polarity and strike machinery: `passed()` becomes the check's pass bit, `TimedOut` its timeout flag, and the tail its output. A `Lost` outcome's output is the log file's tail, because the cause is in the label now rather than in a synthetic evidence string.

`WatchOutcome` travels only the `rimz loop run` argv and the process memory around it, so reshaping it is not a durable-format change; the verdict becomes durable one level up, in the run record ([below](#history-strikes-and-arming)).

## Wakes

`rimz wake` writes the same rows through a narrower door. Arming validates exactly one trigger (`--in`, `--signal`, or a command after `--`), resolves the delivery target, mints a `wake-<adjective>-<noun>` name unique among existing `wake-` tasks, and writes a `Deliver` entry to the instance store. `--in` becomes a bare `at = "HH:MM"` one-shot, a command becomes a `Watch` trigger, and `--signal` becomes a `Signal` trigger with a `deadline`; `loop.toml` is never touched.

Every `rimz wake` row also carries `wake_meta` (serde `wake-meta`), which is what separates it from a `loop add --wake` row: `armed_by` (`Human`, or `Agent { handle }`), `armed_at`, and the `--in` text as `delay`. The composer reads it for the armer clause, `wake list` reads it for `AGE`, and the elder's plan reads it with the `Signal` trigger and a passed `deadline` to tell a wake to close from a loop task to leave alone.

The target is where the caller identity matters. An explicit `@handle` resolves against the live rollup and pins `{kind, session, handle}`; a provisional card is refused, because there is no session to deliver to yet. With no address, the wake goes back to the calling agent, resolved from its launch environment or process ancestry, which is why a user shell must name a target. The same identity scopes `wake list` and `wake cancel` to the caller's own wakes.

### Caller-scoped matches and guards

`default_signal_matches` resolves what the caller already knows, so the common wait is one flag. It runs for `rimz wake` and for `rimz loop add --wake` alike, over the caller's agent row when RimZ can identify one and the target's row otherwise:

- a `ci`/`pr` selector with no `path` or `branch` match takes `path = <caller worktree>`, falling back to the workspace's worktree root; when that resolves to the project root it is an error naming both fixes, because the forge poll only watches worktree branches (`needed_worktree_paths`), so the subscription could never fire.
- a `team` selector with no `team` or `instance` match takes `instance = <team>#<channel>` from the caller's own cohort, and errors when the caller is in no team.

`self_wake_guard` blocks the opposite shape: a wake armed on any `agent.*` signal must carry `--match handle=<other>` or `--match session=<other>` naming someone other than the target, so an agent's own lifecycle cannot wake it. Arm-time validation also rejects `ci.finished` and a `conclusion` match on a `ci` selector, naming `ci.passed`, `ci.failed`, and `ci.*` as the replacements.

### The deadline

A signal wake is one question, and every exit removes its row. `--timeout` (default `59m`, matching the provider prompt cache, and refused at or above 24h by the same `validate_shape` rule as `--in`) sets both the row's `timeout` and its `deadline = armed_at + timeout`. Nothing moves that deadline afterwards; three paths take the row instead, all under the instance-store lock:

| Path | Effect |
| --- | --- |
| `remove_signal_wake`, called by the runner just before it delivers | removes the row while it is still the same subscription. `false` means a concurrent runner, cancel, or re-arm owns the question now, and the fire records `canceled` with `wake already consumed, canceled, or replaced` instead of delivering twice |
| `arm_signal_wake`, called when arming finds a live row with the same target, selector, matches, and root | replaces that row in place: the name survives, and the candidate's `wake_meta`, `deadline`, `timeout`, and prompt take over |
| `claim_expired`, called by the expiry runner | removes the row and hands its entry back, but only while it is still the same subscription and still past its deadline |

`same_subscription` is that identity check: resolved root, selector, matches, delivery target, and `wake_meta.armed_at`. Signal firing also passes that arm stamp to the detached runner as `--wake-armed-at`; after loading the catalog, the runner exits without delivery or a run record when the stamp no longer matches. This fences a re-arm before child startup, while the conditional removal fences a re-arm after the child loads its row. Instance rows are published through `disk::atomic::write_temp_then_rename`, the durable path.

Expiry is planned by the elder and executed by a detached runner, which is what keeps `fire.rs:plan` read-only in the sidebar graph. `plan` marks a live instance row that has `wake_meta`, a `Signal` trigger, and a passed `deadline` as `Action::Expire` and spawns `rimz loop run <name> --expired`. `prepare_expired` claims the row, resolves `RuntimePaths` from the row's own root, and asks `status::resolve` what the room knows now:

- `Answered { label, signal }` records `Expired` with that `SignalRecord` and no message. The gate reason `answered · <label>` is presentation only and reaches nobody when the elder spawns the runner detached, so the run record's `signal` field is where the answer becomes durable: `rimz loop logs` and `rimz loop show` render it as `signal: ci.passed` on the closed row.
- `Open(view)` prepares the ordinary delivery with `Evidence::Expired { view, rearm }`, so the closing message records `Expired` with a `message_id`.

A re-arm or a `wake cancel` between the tick and the claim makes the claim a no-op, and the runner exits without a record. A re-arm leaves its replacement waiting; cancellation leaves no row.

`status::resolve` reads `pr-state.json` through `sidebar::refresh::pr::read_pr_state_cache`, the same call `idle_compact.rs` already makes, so the answer is the room's current forge truth rather than a replay of the signal trail. Only `ci` and `pr` selectors look at it; every other family is `Open(None)` and closes with no status. Scope resolution is `matches.path` into `states`, falling back to `branch_ci[path]`, or a lone `branch` match resolved to the single `PrLink` carrying it. The state matches in `status.rs` pair each label with its terminal signal name, and `Answered` is exactly the case where the state yields `Some(n)` and the selector is `Exact` on a different name. Everything else stays `Open`:

- `WorktreePrCi::Pending` and an open PR name no signal, so they answer nothing.
- A verdict the selector *would* have delivered keeps its `Open(view)` and labels itself `…; no matching transition received`. The cache stores current state, not when the state began, so it cannot say whether that verdict predates the arming; guessing here would swallow the wake.
- A scope the cache cannot pin is `Open(None)` and closes with no status at all: a `--match` key outside `path` and `branch`, a `path` whose cached branch contradicts the row's `branch`, a `branch` matching two links, or neither key to look up. A scope it can pin but has never seen is `Open` with the `no PR or CI seen on <scope>` label instead. Silence requires an understood scope, so all of these deliver.

`rearm_command` writes the same wait back as one shell-ready line through `shlex::try_join`, because selectors carry glob syntax and matches and prompt paths carry arbitrary characters. It emits every stored `--match` (the caller-scoped defaults included, since they are equivalent explicit filters), `--timeout` only when it is not `59m`, and `--prompt-file` as the row stores it, which keeps `resolve_config_path`'s rule (absolute as given, relative against `~/.config/rimz/`) pointing at the same file. It names no `@target`, since the message reaches the target that would re-arm it, and it drops an inline `--prompt`, which rides the same message as the note.

### Watched commands

A watched command runs in a detached `rimz wake watch <name>` process whose whole body is `signal::run_watcher`. `cli/wake/add.rs` starts it: it creates or truncates the wake's log file, hands the file to the child as its stderr (stdin and stdout stay null), and calls `CommandExt::process_group(0)` so the arming shell tool or turn cannot kill the watcher and its command as part of its own group. Then the CLI opens the store and hands it and the workspace over; the harness function owns the lifetime from there:

- It loads the catalog and bails with a reason when the task is gone (`no wake named {name} in the catalog`), when its resolved root is not this workspace, or when the row carries no `watch` command. Those gates return an error rather than `Ok(())` so the reason reaches `main`'s error print, whose stderr is the log file the wake's message points at; a gate that exits quietly is a watcher death nobody can explain 30 seconds later. Losing the lock race is the one silent exit, because a second watcher for the same name is by design.
- It takes an exclusive flock on `loop-watch-<name>.lock` in the workspace runtime directory and writes `{pid, started_at}` into it, so a second watcher for the same name exits instead of doubling the run. `wake list` reads that payload for the `watching pid <pid>` state and reports `watcher lost` when nobody holds the lock.
- It runs the command through the shared `run_check` at the project root under the task's `timeout`, which `rimz wake` always writes (`59m` unless `--timeout` says otherwise); the `loop.default-timeout` and two-hour fallbacks below it now only cover a row that carries no timeout at all. The echo mode is `CheckEcho::Tee`, which appends both streams to the same log file in arrival order while keeping the last `WAKE_TAIL_CAP` (4 KiB) in memory as the delivered tail.
- On exit it appends the `wake.<name>` signal and calls `fire_signal`, which spawns the run that delivers. The whole `WatchOutcome` (verdict, tail, and `output_path`) reaches that run in the fire's argv rather than the durable event ([above](#the-watch-verdict)).
- The flock is held through both the append and the fire, so the elder cannot read the row as watcher-lost while the watcher is still finishing.
- If the process dies first, the elder's plan sees a `Watch` row stamped more than 30 seconds ago with no lock holder and fires it with a `Lost` verdict, whose `elapsed_ms` counts from the arm stamp and whose evidence is whatever the log file holds, which is where a watcher that died with an error will have left it. That is the backstop that keeps a killed watcher from leaving a wake pending forever, and it needs a room or the external timer.

One helper, `signal::wake_log_path(paths, name)`, derives `<StatePaths.root>/wakes/<name>.log` for all three callers: the CLI creating it at arm time, `run_watcher` appending to it while the command runs, and `fire.rs` reading its tail for a lost watcher. The file sits in the durable state tier rather than the disposable runtime tier, so the agent can still read it after the room restarts; `prune_wake_logs`, which `rimz gc` calls, removes one only when its name has no catalog row and no running watcher and its last write is past the 14-day retention `store::event_log` already defines for archives ([store.md](../store.md#the-workspace-store)).

`--wait` keeps the caller inline instead: it polls the run log every 500 ms for a record newer than the arming stamp, attempts to cancel an open message id with the reason `joined inline`, prints the result followed by the record's own `WatchVerdict::label`, the output path, and the tail or `(no output)`, and exits `1` unless the record is `delivered` or `skipped` with a clean exit. A message that already reached the pane cannot be recalled. `rimz wake cancel` removes the row and SIGTERMs the lock holder.

## One fire

`rimz loop run <name>` is hidden, and after CLI trust and action validation it hands the fire to `schedule::runner::TaskFire`. That type owns one fire from its start time through exactly one history transition; the CLI executes the returned effect and reports its typed result back.

`TaskFire::prepare` walks an ordered ladder, and the order is load-bearing: everything that can refuse cheaply refuses before anything expensive or observable happens.

| # | Gate | Records on refusal |
| --- | --- | --- |
| 1 | the task's own `--budget-per-day`, summed from this task's completed runs in the configured local day | `budget skipped` |
| 2 | the room-fleet and provider-account [scope caps](./budget.md#the-fail-fast-gate) | `budget skipped` |
| 3 | the exact managed-launch provider quota, when a binding is proven | `budget skipped` |
| 4 | `--surplus` / `--surplus-after` forward headroom on the provider's longest window | `surplus skipped` |
| 5 | the per-task advisory run lock | `overlapped` |
| 6 | the poll-until `deadline`, which skips a signal wake because `prepare_expired` owns that deadline and a signal landing between the elder tick and the claim should still deliver | `expired` |
| 7 | the `check` command and its polarity | `skipped`, or a check-only terminal result |

Only then does the action run. `TaskFirePlan` returns `Done` (a gate already produced the terminal record), `Spawn` (a prepared `SupervisedRunRequest`), or `Deliver` (a prepared target and prompt). The CLI executes it and calls `finish`, which maps the outcome to a `LoopRunResult` and appends the record. All gates apply in both scheduled and manual modes, so `rimz loop fire` tests the real policy.

A closed gate costs nothing and adds no strike; the recurring schedule keeps polling until the condition clears. The surplus gate fails closed: an account with no window reading keeps the gate shut, since spending against an unknown budget is the failure this feature exists to prevent.

The run lock is `loop-run-<name>.lock` beside `loop-fire.json`, carrying the holder's `{pid, started_at}`. The kernel releases it when the runner exits or crashes, and display probes read it without rewriting its payload. `runner::stop_task` owns the stop ladder behind `rimz loop stop`, with the CLI passing its supervised cancellation in as a closure: probe the lock and report no active run when it is free, cancel the newest active run through the durable path, wait five seconds, then SIGTERM the holder and wait five seconds more. Only the SIGTERM branch appends a `canceled` row from the stop path itself. A holder still owning the lock afterwards is not escalated to SIGKILL; the error names its PID and the lock path instead.

An **ephemeral** task (a one-shot, or any task with a `deadline`) removes its own state row *before* the supervised run or delivery. A one-shot removed pre-fire that then fails to launch is not retried. A poll-until row also removes itself when its check fires the action, and expires without delivery once its deadline passes. A signal wake follows the same rule through its own door: `remove_signal_wake` under the instance lock rather than `consume_scheduled`, so a losing claim can stop the delivery instead of duplicating it.

### Where a scheduled run lands

A `Spawn` fire sets `loop_zone` on its request, which sends the transient pane to the `rimzd` loop zone instead of splitting beside a caller. The runtime column's loop panel stays open and run panes stack under it, so a fire never splits the sidebar or a working tab. The panel is repaired rather than assumed: elder-tick repair restores any closed managed pane, and fire-time repair recreates a missing loop panel immediately. If the whole `rimzd` view is gone or the split fails, the run falls back to a new tab. Manual `rimz loop fire` keeps splitting beside the caller, so its foreground stream stays local.

A scheduled `Spawn` also gets a timeout it never asked for. `effective_spawn_timeout` uses the task's own `--timeout` when set; otherwise a *scheduled* fire receives the machine's `loop.default-timeout`, two hours by default, while a manual `rimz loop fire` stays unbounded. Unattended work should not wedge forever; a human watching one can decide for themselves.

### Checks

`check = "<shell>"` runs through `sh -c` at the task's project root before any agent action. `on = "fail"`, the default, fires on a non-zero exit or a timeout; `on = "success"` fires on a zero exit. `timeout = "5m"` bounds it, defaulting to five minutes.

A check-only task is a scheduled command: it logs `completed`, `failed`, or `timed out` with the exit code and capped combined output, and keeps recurring unless it is ephemeral. A guarded task logs the check evidence whether it skips or fires, and when the guard fires, the command, its exit status, and the capped output are appended to the base prompt, so the agent wakes already reading the evidence.

A `Watch` row takes the same path with the command already run: the fire's signal carries the `WatchOutcome`, `prepare_check` converts it instead of executing anything, and the polarity rule then applies unchanged. A `Lost` outcome converts to a failed check, so the default `any` polarity still delivers while `--on success` skips it. A polarity skip records `skipped` and consumes the ephemeral wake without delivering a message.

### The prompt a fire delivers

`resolve_effect_prompt` composes the delivered text in one order. The base is `prompt`, or `prompt-file` read at fire time (a relative path resolves against the machine config directory, not the caller's cwd); `resolve_task_prompt` allows a wake row to have no prompt at all, while a `Spawn` row still requires one. No substitution happens anywhere: `{{key}}` is delivered as typed.

A row that delivers to a session, or fires on a signal or a watched command, then goes through `compose_wake` ([`runner/prompt.rs`](../../../crates/rimz/src/harness/schedule/runner/prompt.rs)), which writes the message the receiver reads. It is one assembler over three composers, and the receiver is an agent picking a wait back up turns later, so the order is what it waited on, how that ended, the evidence, then its own note.

| Line | Composer | Content |
| --- | --- | --- |
| armer | `armer_line(meta, task)` | `@{handle} armed this wake on you.` or `armed on you from the shell.`, and nothing at all when the armer is the delivery target or the row has no `wake_meta` |
| wait | `wait_line(task, meta, evidence)` | `` waited on `<cmd>` `` for a watch row, `waited on <signal subject>` for a signal, `waited on <selector> on <view.headline>` for a closing wake that read forge state and `waited on <selector><scope>` for one that did not, `waited <delay>` for a delay, and `scheduled wake` for a clock row that recorded none |
| verdict | `verdict_line(evidence, task, meta, now, name)` | `WatchVerdict::label` for a watch row, `fired after <elapsed since armed_at>` for a signal (`fired` with no `wake_meta`), `nothing in <timeout>; wake closed` plus ` · <view.label>` for a closing wake, `fired by hand` for a manual fire, and nothing at all for a delay, whose wait line takes the `[<name>]` suffix instead; then ` · output: <path>` when the outcome carries one, then ` [<name>]` |
| evidence | the assembler | the output tail or `(no output)` for a watch row, the payload as one compact JSON line with `signal` overwritten by the fired name for a signal, one `re-arm: <command>` line for a closing wake, nothing otherwise |
| note | the assembler | the base prompt verbatim after a blank line |

The signal subject is `signal_headline`'s wording: the name, plus ` on <branch>` and ` (PR #<n>)` for `ci`/`pr`, the `handle` for `agent`, the `instance` for `team`. A closing wake has no fired signal to read: with a `ForgeView` it takes that view's headline, which `status.rs` builds in the same `feat-x (PR #91)` shape, and without one it names the selector and the first scope `subscription_scope` finds among `branch`, `path`, `instance`, `team`, `handle`, and `session`.

Two rules hold the shape together. Every duration is elapsed rather than wall-clock, because a receiver reading the message an hour late can act on `after 12m` and cannot place `14:02`; `compose_wake` therefore takes the fire timestamp instead of a `TimeZone`, and measures from `WakeMeta.armed_at` for signals and lost watchers and from `WatchVerdict::elapsed_ms` for a command that ran. And the armer is named only when it is not the target: a wake another agent or a human armed on you is an instruction, so it leads, while your own wake says nothing about who armed it. A `loop add --wake` row has no `wake_meta` at all, so it gets the wait line and a bare `fired [<name>]`.

A guard that fired still appends its own block through `augment_prompt`, after the composed body:

```text
--- check `cargo test` exited 101 ---
<output tail>
```

`<status>` there is the exit code, or `timeout` when the command was killed at its deadline, or `signal` with no exit code at all.

Two patterns fall out of the guard. A **watchdog** runs a command on a schedule and wakes an agent on failure. A **trigger-when-green** polls until a command succeeds, then delivers.

`--verify` is the mirror image and applies only to `Spawn` tasks: `check` gates whether a turn starts, `verify` gates whether it counted as done, through the [same-session re-prompt loop](./scripting.md#verification-re-arms-the-same-run).

### Delivering to a live instance

`wake` pins a schedule to one exact agent session. `rimz loop add <name> --wake @<handle>` resolves the address against the live rollup **at add time**, records a `wake` sub-table of `kind`, `session`, and `handle`, and rejects `agent` and every supervised-run flag, because delivery opens no pane.

On fire, the runner resolves the recorded root, confirms the pinned root session still exists, and sends the prompt through the same path as `rimz message`, as a `Harness { notice: Wake }` sender carrying the `Type: WAKE` header ([messaging.md](./messaging.md#the-message-header)), gated `done` and inheriting the `[harness] smart_compact` default. An idle agent takes it immediately, a running agent parks it for its next `done` boundary, and a missing session records `target gone` and removes the schedule, because that exact conversation cannot come back. The delivery is recorded as a `Wake` transcript entry, which the rendered transcript hides and `--json` keeps ([cli/transcript.md](../../reference/cli/transcript.md)). `rimz gc` runs the same liveness check as a safety sweep for wake schedules whose pinned session left the rollup without the task ever firing.

Self-paced loops ride the same rows: an agent arms its next wake at the end of the current turn, so the wait exists only while work remains. A `--in` wake and a signal wake alike are removed before delivery, so the next wait is always one the agent armed on purpose. `rimz wake` is the front end agents use for that ([above](#wakes)); `rimz loop add --wake` is the same action with a standing trigger and no deadline.

## History, strikes, and arming

Every fire appends a `LoopRunRecord` to the user-global `~/.local/state/rimz/loop-runs.log.jsonl`. Loop config is per-machine but the log is per-user, so history survives a task being edited or removed.

The record carries the result, mode (`scheduled` or `manual`), duration, error chain, check evidence (exit code, timeout flag, capped output, and the output file's path for a watch row), the watched command's `WatchVerdict`, the triggering signal's name and payload, the durable message id of a delivery, delivery target, supervised run id and transcript path, last message, cost, and fresh input and output tokens. Append caps the stored copies: 4 KiB of check output, 2 KiB of error text and last message, and a signal payload over 4 KiB collapses to a single `_truncated` field. `rimz loop show` reads it for a health verdict plus a separate agent-run rollup for check-gated work; `rimz loop logs` prints the stored forensics in full, including the output path above the check gutter.

`LoopRunRecord.watch` and `CheckRecord.output_path` are both `#[serde(default)]`, so a record written before they existed reads back as `None` and renders the way it always did. They exist so history renders from durable data rather than reconstructing the words from a `CheckOutcome`: `prepare_check` keeps that conversion for polarity and strikes and stashes the verdict and path beside it, which also gives a watch row a real presentation duration instead of the `0ms` the trip line used to print. Every renderer that finds `watch` set uses `WatchVerdict::label` for the outcome words and `elapsed_ms()` for the duration; a non-watch row keeps the `exit <n>` / `timeout` / `signal` segments it always had.

`LoopRunResult` has fifteen variants, and `strikes::classify` sorts them into three signals. `CheckSkipped` and `SignalSkipped` both render as `skipped` and serialize distinctly (`check_skipped`, `signal_skipped`), because one is a guard that declined and the other a sibling signal the subscription observed without delivering:

| Signal | Results |
| --- | --- |
| Strike | `failed`, `verify failed`, `timed out`, `error`, `budget exceeded`; also `completed` or `delivered` whose check still shows the world broken |
| Reset | `completed` or `delivered` with a passing or absent check; `skipped` from a check that passed |
| Neutral | `budget skipped`, `surplus skipped`, `overlapped`, `canceled`, `expired`, `target gone`, `skipped` from a sibling signal |

That table encodes the judgement calls. A turn that completed but left its check red is a failure, because the task is not doing its job. A gate that declined to spend money is not a failure at all.

`record_transition` appends the history row and then updates the overlays, and it is the only writer to the log: a fire, an observed sibling signal, and `rimz loop stop` all reach it, and no CLI code appends a row. The append is best-effort: `disk::rotating` logs its own failure at debug and returns, so the `RunTransition::Recorded` it returns means the append was attempted before the overlays moved, not that the row reached the disk. Consecutive strikes reaching the threshold (`--max-strikes`, default 3, `0` to disable) auto-disable the task, display `disabled · N strikes`, and fire `loop_disabled` notification handlers. `rimz loop enable` clears the counter and re-arms; `rimz loop fire` still works on a disabled or paused task for testing.

Strike counts live in machine-local `loop-strikes.json`, independently of run-log rotation, and use the same scoped keys as arming. Each overlay has an advisory lock that serializes updates from concurrent task runners. The retired unscoped `loop-pauses.json` is ignored and removed by `rimz gc`.

## Recovery the elder runs

The same elected producer runs two more interventions on the same tick. Neither is a loop task, but both belong to the same story: the machine acting on your behalf while you are away, and being accountable for it.

**Auto-continue** ([`auto_continue.rs`](../../../crates/rimz/src/harness/auto_continue.rs)) resumes a parked agent through the message queue. It is opt-in, and it is a three-phase durable record rather than a live watcher, so the decision never depends on ephemeral per-session context surviving the wait.

| Phase | What happens |
| --- | --- |
| Arm | while an agent is parked on a certified-resumable turn error, the producer writes a `ParkRecord` capturing the park class and the agent's frozen `last_activity` |
| Fire | once the class-specific clock is due and `last_activity` has not advanced, the producer spawns the detached `rimz agents auto-continue` helper, which queues and delivers a resume-gated message |
| Clear | any activity since the park advances `last_activity` and the stale record is removed; a delivered resume message also clears it |

Three park classes carry their own clock:

| Class | Deadline | Armed from |
| --- | --- | --- |
| `Budget` | the dollar-cap park's own `resets_at`, the next local day | a RimZ [budget scope park](./budget.md#the-park) on the agent, checked first and short-circuiting the provider classification |
| `RateLimit` | the latest spent-window reset in an authoritative account capacity reading | a certified rate-limit or spend-limit turn error |
| `Overloaded` | the turn-error marker time, then a lengthening backoff ramp | a certified transient overload or API error |

`resume_park` is the pure, unit-tested arm decision for the latter two, and it is deliberately hard to satisfy: an exhausted window, an error message, or a stalled pane cannot arm recovery on its own. Only a provider-owned per-turn failure marker can, which is why an adapter with account clocks but no certified marker (Antigravity today) leaves its error stops terminal.

**Auto-redeem** ([`auto_redeem.rs`](../../../crates/rimz/src/harness/auto_redeem.rs)) pairs with it for Codex reset credits. The producer evaluates cached provider-neutral capacity and credit state and spawns a hidden helper only when a redemption is useful (`ExpiryRescue`, `BlockedGain`, `DoomedCredit`, or `ScheduledRedeem`); the scheduled reason uses a persisted burn-rate estimate and the full expiry chain to redeem early enough for each credit to catch a refill. The helper serializes account-wide attempts, refreshes both inputs, re-evaluates the same pure verdict, and performs the provider-specific consume request. A restored window then lets the existing auto-continue path wake the parked turn.

Both modules sit in the sidebar's read-only import graph, so they stay free of store-writer, run-wake, and broker imports, and pass their evidence to the detached helper through argv.

## The assist log

> **Automation is accountable.** User-benefiting automation appends a durable record of its trigger, evidence, and outcome, and surfaces in `rimz stats`; internal repairs keep durable diagnostic records.

That invariant is the assist log: `$XDG_STATE_HOME/rimz/assists.log.jsonl`, account-global, best-effort append, rotating at 4 MiB to one `assists.log.1.jsonl` predecessor. Readers fold both generations by timestamp. The intervention itself remains the operational truth when an append fails.

Four `Assist` variants live in the log today:

| Variant | Writer | Records |
| --- | --- | --- |
| `auto_redeem` | the detached redeem helper | provider, decision reason, request id, available credits, soonest expiry, the natural reset it beat, consume outcome or error, whether a reset occurred, and refreshed window stamps |
| `auto_continue` | the detached continue helper | typed provider and session ids, display handle, park class, original park timestamp, delivery verdict, and the durable message id |
| `auto_compact` | the message delivery path after a compact command lands | target session, display handle, threshold, occupied context when known, and the durable compact-command message id |
| `auto_resume` | rebirth recovery after materialization restores at least one pane | workspace, session, death cause, recovered pane count, and planned tab labels |

`rimz stats` folds both assist-log generations together, scoped to the active dashboard window, and publishes one rollup: delivered continues and their summed `recorded_at - parked_since` recovered time; compact commands sent; redeem attempts and `reset` outcomes; and rebirths plus restored panes. The dashboard shows those four non-zero categories, `rimz stats --assists` renders the merged newest-first event stream, and `rimz stats --json` publishes both.

**Shipping a new smart strategy is one accountable slice**: define its typed trigger, evidence, and outcome record; append it from a writer outside the sidebar import graph; fold it into the Assists stats surface; and add its variant and writer to the table above.

## See also

- [scripting.md](./scripting.md): the supervised run every `agent` task spawns.
- [messaging.md](./messaging.md): the delivery path every `wake` task uses.
- [fleet.md](./fleet.md): launch compilation and addressing.
- [budget.md](./budget.md): the dollar scopes the gate ladder reads, and the park the `Budget` class waits out.
- [providers.md](../agents/providers.md): account windows, capacity readings, and the pricing behind recorded cost.
- [sidebar/state.md](../sidebar/state.md#renderers-the-producer-and-consumers): how the elder is elected and what else runs on its tick.
- [cli/loop.md](../../reference/cli/loop.md): every flag on `add`, `fire`, `list`, `show`, and `remove`.
- [cli/wake.md](../../reference/cli/wake.md) and [cli/events.md](../../reference/cli/events.md): the user-facing wake triggers and the signal emit surface.
