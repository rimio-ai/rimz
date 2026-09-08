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
| [`schedule/runner/prompt.rs`](../../../crates/rimz/src/harness/schedule/runner/prompt.rs) | `compose_wake`: the wait line, the verdict line, the evidence, and the verbatim note. |
| [`cli/wake/`](../../../crates/rimz/src/cli/wake) | Self-only trigger parsing, caller resolution, receipts, and pending list/cancel presentation. No scheduling policy. |
| [`schedule/arm.rs`](../../../crates/rimz/src/harness/schedule/arm.rs) | Shared delivery builder, caller-scoped signal defaults and guards, locked dedupe, watcher spawning, and session retirement. |
| [`schedule/team.rs`](../../../crates/rimz/src/harness/schedule/team.rs) | Team binding validation at launch and materialization at member registration. |
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
| `Instance` | `~/.local/state/rimz/workspaces/<workspace-id>/loop-instances.json` | RimZ-owned runtime rows: one-shots, poll-until rows, `once` subscriptions, and every session-pinned delivery, including recurring clocks and standing signals |

The instance store exists so runtime churn never edits user config. The strict catalog loader migrates the legacy global instance file once by each row's resolved root, keeping an existing workspace name on conflict and removing the global file afterwards; lenient reads never migrate. Old wake rows in `loop.toml` remain `Config` and gc reaps them rather than migrating them. `TaskCatalog::load(Some(root))` reads that workspace's instances; `load(None)` reads machine tasks only. An agent scheduling its own `--in 30m` wake writes state, not your `loop.toml`, and the row retires itself after firing.

Project tasks are more constrained than machine tasks, because a committed task cannot make machine-local claims. Loading rejects `root` (a project task runs at the project root), `wake` (it cannot pin a session on someone else's machine), and `deadline` (a poll-until timestamp is machine state), and requires `every`, `cron`, or `signal`, because a one-shot would have to delete itself from a trust-hashed file on fire.

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

`ephemeral_lifetime` decides which rows retire themselves: anything with no repeating trigger, plus any row carrying a `deadline`, `once = true`, or a `watch` command.

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
| no stamp | arm: record `now`, do not fire |
| stamped, disabled or pause active | hold the existing stamp unchanged |
| stamped, schedule due | fire: record `now` |
| stamped, watch row with no lock holder past the grace | watch-lost: record `now` and fire the `Lost` outcome |
| stamped, not due | keep the stamp |

State is written before any helper spawns, which is what makes a fire at-most-once per occurrence even when ticks are hot.

### The external tick

`rimz loop timer install` writes one user-level systemd timer on Linux or launchd agent on macOS. Once a minute it invokes the hidden `rimz loop tick`, which enumerates roots from machine and transient task entries plus trust grants containing project tasks. Trust grants are the project-root registry because an ungranted project task cannot run. For each root the tick derives the same `WorkspaceId` and runtime paths as a room. A fresh sidebar heartbeat makes it skip that root; otherwise it prepares the runtime directories and calls the same firing planner with the root explicitly supplied, so a never-opened root does not need a workspace record before its task can arm.

The timer is only a clock host. It re-reads configuration every pass, leaves arming, trust, overlap locks, execution, and run history to the existing paths, and exits after one tick. A `Spawn` fire births a room through the supervised-run path and leaves it open, so later external ticks yield to its elder. `Deliver` still requires the pinned live session; check-only work runs bare. A room can be born between the heartbeat check and the state write, creating a one-tick race, but the shared fire stamp and per-task overlap lock remain the duplicate and concurrency defenses already used by hot elder ticks.

The machine-local `loop-arming.json` overlay holds enablement, a bounded pause deadline, and an automatic-disable strike reason without editing durable task definitions. Project and instance keys use `<workspace_id>::<name>` and machine keys use `machine::<name>`, so a same-named task in another checkout never inherits an enable. Enabling writes the anti-replay edge; when a timed pause expires, its deadline becomes the **effective last-fire edge**. Either lift makes the schedule wait for its next occurrence rather than replaying everything missed while held.

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
| `team.idle` | the live cohort is non-empty, every member is at rest (`Idle` or `Success`), and no live member has a queued message or pending one-shot wake; emitted only on the false-to-true edge, computed by overlaying `prior_status` on the member for the prior view |
| `team.ended` | a terminal event for the member and no other live member remains |

The payload carries `team`, `instance` (`team#channel`), `member` (the qualified handle that tripped it), and `members` with each handle and status. Membership is whatever `team_cohorts` counts as live for that `team#channel`, and a provider-native subagent's transition derives nothing. **The gap to know**: a member the reaper stops (a pane closed with no lifecycle hook, [`store/writer/reap.rs`](../../../crates/rimz/src/store/writer/reap.rs)) passes through no hook, so it derives neither `agent.ended` nor `team.ended`.

Two facts about durability are easy to get backwards. `rimz events emit`, the wake watcher, and the hook's team derivation append a `signal.emit` event through the ordinary store commit ([store.md](../store.md#what-is-in-it)), so `rimz events follow` replays them; `agent.*` signals append nothing extra, because the `agent.lifecycle` record they were derived from is already the durable trace. And the durable record is the `SignalEventPayload`, which keeps only name, payload, and source: a watched command's exit status travels in the fire's argv, not in the log.

`fire_signal` is the whole delivery mechanism, and it runs in the emitting process:

1. Load the runnable catalog for the project root, dropping untrusted project rows, and keep only tasks whose resolved root maps to this workspace.
2. `resolve` each task's trigger against the signal, dropping `Ignore` and `Schedule` rows outright.
3. Drop any task whose arming overlay is not `Live`, so a disabled or paused subscription stays quiet.
4. `Skip`: append a `SignalSkipped` run record carrying the observed signal and spawn nothing. `Deliver`: spawn a detached `rimz loop run <name> --signal-json <encoded>` and return the name for the emitter to print.

A row's consumption is not in that sequence. `fire_signal` touches no instance row; sibling observations leave subscriptions armed. The runner consumes a one-shot before delivery; standing subscriptions remain.

There is no queue, no persistence of pending matches, and no replay: signal firing leaves `loop-fire.json` untouched, and a subscription written one second after the emit simply misses it. The elder may stamp a signal row when it first sees the catalog, but that clock-side display state is never consulted by `fire_signal`. A signal reaches only the subscriptions armed in that workspace at that instant, which is the property that lets an emitter run with no room open.

### The watch verdict

A watched command's signal carries a `WatchOutcome`: a `WatchVerdict` plus its evidence, the output tail and the path of the file holding all of it. The verdict is the output-free half, and it is one enum with one renderer.

| Variant | `label()` |
| --- | --- |
| `Exited { code: Some(0), elapsed_ms }` | `exit 0 after 4m` |
| `Exited { code: None, elapsed_ms }` | `killed by signal after 3s` |
| `Running { elapsed_ms }` | `still running after 30m` |
| `TimedOut { elapsed_ms }` (legacy records only) | `timed out after 59m` |
| `Lost { detail, elapsed_ms }` | `watcher died after 3m; the command may still be running or may have died with it` |

`WatchVerdict::label` is the only place those words are written, and `compose_wake`, `rimz loop logs`, and `rimz loop show` all render through it, so one outcome cannot read three ways. `passed()` is `Exited { code: Some(0) }` and nothing else; `elapsed_ms()` is the measured run, rendered in seconds under a minute and through `theme::fmt::duration_label` above it. `is_terminal()` distinguishes the one nonterminal `Running` check-in, which bypasses polarity, is strike-neutral, and does not consume the row. For terminal outcomes, `to_check_outcome` folds the verdict into the polarity and strike machinery: `passed()` becomes the check's pass bit, `TimedOut` its timeout flag, and the tail its output. A `Lost` outcome's output is the log file's tail, because the cause is in the label now rather than in a synthetic evidence string.

`WatchOutcome` travels only the `rimz loop run` argv and the process memory around it, so reshaping it is not a durable-format change; the verdict becomes durable one level up, in the run record ([below](#history-strikes-and-arming)).

## Wakes

An armed instance-sourced one-shot delivery row projects its resting target (`idle` or `success`) to `sleeping` in the sidebar and `rimz agents`; `rimz teams` gains a `sleeping` rung below `working` and above `done`. Projection matches the workspace root and target kind/session and covers timers, watched commands, and one-shot or deadline signal deliveries, not standing subscriptions or recurring clocks. Working, waiting, failed, paused, and live-child delegating states take precedence. The standard agent card carries `⧖ waits (N)` for the pending one-shot count regardless of status, after the subagent stats or expanded child entries. Any live member's pending one-shot wake withholds `team.idle`, regardless of its displayed status. Reevaluation remains lifecycle-only: cancellation is self-only and the caller's following turn boundary reevaluates the cohort; row removal itself emits no immediate idle signal. If no lifecycle event follows a removal, evaluation waits for the next member lifecycle event. `agent.idle` remains the ordinary turn-boundary signal.

`rimz wake` accepts only a positive delay shorter than 24 hours or a command after `--`, and resolves the live calling agent through `@me`. A user shell cannot arm or cancel. Both this CLI and `loop add --wake` call `schedule::arm::arm_delivery`; neither command imports the other.

`DeliverySpec` carries typed trigger, name, prompt, target, and provenance. The private builder assembles every delivery row. `SelfWake` accepts only delay/watch with no prompt or auxiliary gates and is the only provenance that writes `wake_meta`: `armed_at` and optional `delay`. It mints a workspace-unique `wake-<petname>`; `Loop` accepts named clock and signal deliveries; `Team(instance)` accepts standing signals. Every wake target is an instance row.

For signal deliveries, the builder applies caller-first, target-fallback defaults and the other-agent lifecycle guard ([caller-scoped defaults](../../reference/cli/loop.md#caller-scoped-defaults)). One locked instance mutation compares live rows by kind/session, parsed selector, normalized matches (absent and empty equal), and resolved root before replacing a name. A duplicate returns `AlreadySubscribed` without rewriting its name, prompt, provenance, or overlays.

Arming and canceling print the caller's pending rows after the receipt. Lists include loop and team subscriptions for the pinned session; from a user shell listing is room-wide and read-only. Cancel requires a caller and accepts a name or `--all`. Command previews use `theme::fmt::command_preview`: at most 120 Unicode scalars, preserving the first 60 and last 59 around an ellipsis. Stored commands and logs are unchanged.

### Team bindings

`[[agents.teams.<name>.signals]]` declares `signal`, `role`, optional string-valued `match`, and optional `prompt`. `prepare_team` validates declared roles, selectors, and explicit other-agent matches for `agent.*`. The complete ordered binding list enters the executable trust hash; an empty list preserves existing hashes. Fresh `launch_layout` refuses implicit root-checkout CI/PR scope before pane or worktree side effects, naming an explicit branch/path match or `-w <worktree>` as fixes. Launching from a linked worktree passes; this refusal is not added to resume.

After a committed lifecycle `Registered`, the hook reads strict effective trusted config and calls `schedule::team::arm_member` for a root team member, never a subagent. It uses the member's adopted real session identity, role, channel, and worktree, so launch, resume, restart, and role re-add share one registration seam. All binding specs are built before persistence. Rows carry `TaskEntry.team = "<team>#<channel>"` and target the member's exact kind/session; they deliver at the next `done` boundary, not as steer.

Names are `team-<team>-<channel>-<role>-<signal slug>`, lowercased with dots and characters outside `[a-z0-9_-]` replaced by `-`; a role's repeated or colliding signal slugs get a declaration-order `-<n>` suffix. A slug collision between lanes in one room is accepted. Repeated registration deduplicates; an already-subscribed manual row is not relabeled as a team row. Signals are never replayed.

Automatic team arming never replaces machine or project configuration. A generated name that conflicts with a configured task is reported in hook diagnostics; rename that task before rearming the member.

`schedule::retire_session` removes all instance rows pinned to a kind/session, including paused and disabled rows, stops their watcher groups, and clears arming/strike overlays, attempting all cleanup and aggregating failures. Lifecycle `Ended`/`Lost` calls it after the durable event and before event signals; explicit agent-tree stop calls it after each successful node stop. `delivery_target_alive` rejects `ended_at.is_some()`, and gc remains the backstop. Hook config, arming, and retirement failures are warning-logged, never printed to hook stdout.

## Watched commands

The shared delivery builder creates `wakes/<name>.log` and spawns detached `rimz wake watch <name>` with null stdin/stdout, the log as stderr, and `process_group(0)`. Spawn failure rolls back the row. `signal::run_watcher` acquires its workspace runtime lock before reloading and checking the catalog row, closing cancel-before-start races. The lock carries `{pid, started_at}`; a second watcher exits without running the command.

The command runs at the project root, draining both streams into the durable log and retaining a 4 KiB tail. The watcher uses `CheckInOnce`, with an explicit timeout defaulting to 30 minutes independently of `loop.default-timeout`. Ordinary `--check` and `--verify` retain kill-at-timeout behavior. At the deadline an observed exit wins; otherwise one `Running` outcome snapshots the tail and fires without killing the command, filtering on `--on`, consuming the row, or changing strikes. Draining continues until the final exit.

Watcher-originated fires spawn `rimz loop run <name> --signal-json …` and wait for completion while holding the watcher lock, so the interim delivery cannot make the terminal fire overlap. Append/fire failures are logged and the final outcome is still attempted. Other signal emitters remain detached. The check-in body includes `Stop it: rimz wake cancel <name>` and `Another check-in: rimz wake --in <timeout>`; that timer is independent of the running command.

Cancel removes rows first, then `stop_watcher` sends SIGTERM to the lock holder's process group, stopping the watcher and command together. Non-positive PIDs are rejected; an absent process is already stopped. A lost watcher still produces the elder's `Lost` verdict after a 30-second grace, using the log tail as evidence.

`signal::wake_log_path` derives `<StatePaths.root>/wakes/<name>.log` for arming, watching, and lost-watcher evidence. The file outlives room runtime; gc prunes it only with no catalog row, no running watcher, and a last write older than the 14-day retention.

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
| 6 | the poll-until `deadline` | `expired` |
| 7 | the `check` command and its polarity | `skipped`, or a check-only terminal result |

Only then does the action run. `TaskFirePlan` returns `Done` (a gate already produced the terminal record), `Spawn` (a prepared `SupervisedRunRequest`), or `Deliver` (a prepared target and prompt). The CLI executes it and calls `finish`, which maps the outcome to a `LoopRunResult` and appends the record. All gates apply in both scheduled and manual modes, so `rimz loop fire` tests the real policy.

A closed gate costs nothing and adds no strike; the recurring schedule keeps polling until the condition clears. The surplus gate fails closed: an account with no window reading keeps the gate shut, since spending against an unknown budget is the failure this feature exists to prevent.

The run lock is `loop-run-<name>.lock` beside `loop-fire.json`, carrying the holder's `{pid, started_at}`. The kernel releases it when the runner exits or crashes, and display probes read it without rewriting its payload. `runner::stop_task` owns the stop ladder behind `rimz loop stop`, with the CLI passing its supervised cancellation in as a closure: probe the lock and report no active run when it is free, cancel the newest active run through the durable path, wait five seconds, then SIGTERM the holder and wait five seconds more. Only the SIGTERM branch appends a `canceled` row from the stop path itself. A holder still owning the lock afterwards is not escalated to SIGKILL; the error names its PID and the lock path instead.

An **ephemeral** task (a one-shot, or any task with a `deadline`) removes its own state row *before* the supervised run or delivery. A one-shot removed pre-fire that then fails to launch is not retried. A poll-until row also removes itself when its check fires the action, and expires without delivery once its deadline passes. A watch check-in is nonterminal and leaves the row intact; the final outcome consumes it.

### Where a scheduled run lands

A `Spawn` fire sets `loop_zone` on its request, which sends the transient pane to the `rimzd` loop zone instead of splitting beside a caller. The runtime column's loop panel stays open and run panes stack under it, so a fire never splits the sidebar or a working tab. The panel is repaired rather than assumed: elder-tick repair restores any closed managed pane, and fire-time repair recreates a missing loop panel immediately. If the whole `rimzd` view is gone or the split fails, the run falls back to a new tab. Manual `rimz loop fire` keeps splitting beside the caller, so its foreground stream stays local.

A scheduled `Spawn` also gets a timeout it never asked for. `effective_spawn_timeout` uses the task's own `--timeout` when set; otherwise a *scheduled* fire receives the machine's `loop.default-timeout`, two hours by default, while a manual `rimz loop fire` stays unbounded. Unattended work should not wedge forever; a human watching one can decide for themselves.

### Checks

`check = "<shell>"` runs through `sh -c` at the task's project root before any agent action. `on = "fail"`, the default, fires on a non-zero exit or a timeout; `on = "success"` fires on a zero exit. `timeout = "5m"` bounds it, defaulting to five minutes.

A check-only task is a scheduled command: it logs `completed`, `failed`, or `timed out` with the exit code and capped combined output, and keeps recurring unless it is ephemeral. A guarded task logs the check evidence whether it skips or fires, and when the guard fires, the command, its exit status, and the capped output are appended to the base prompt, so the agent wakes already reading the evidence.

A terminal `Watch` outcome takes the same path with the command already run: the fire's signal carries the `WatchOutcome`, `prepare_check` converts it instead of executing anything, and the polarity rule then applies unchanged. `Running` bypasses polarity and consumption. A `Lost` outcome converts to a failed check, so the default `any` polarity still delivers while `--on success` skips it. A polarity skip records `skipped` and consumes the ephemeral wake without delivering a message.

### The prompt a fire delivers

`resolve_effect_prompt` composes the delivered text in one order. The base is `prompt`, or `prompt-file` read at fire time (a relative path resolves against the machine config directory, not the caller's cwd); `resolve_task_prompt` allows a wake row to have no prompt at all, while a `Spawn` row still requires one. No substitution happens anywhere: `{{key}}` is delivered as typed.

A delivery, signal, or watch prompt goes through `compose_wake` in `runner/prompt.rs`: wait line, verdict, evidence, then an optional loop/team prompt verbatim after a blank line. Self wakes have no note or armer line. Durations are elapsed, not wall-clock. A watch uses `WatchVerdict::label`, its output path and name, and the capped tail or `(no output)`; a timer reads `waited <delay> [<name>]`. A signal reads `waited on <subject>`, `fired [<name>]`, and compact JSON with the fired `signal` name. The subject adds branch/PR for forge signals, handle for agents, or instance for teams. A manual fire reads `fired by hand`. Check-ins append their stop and next-alarm commands.

A guard that fired still appends its own block through `augment_prompt`, after the composed body:

```text
--- check `cargo test` exited 101 ---
<output tail>
```

`<status>` there is the exit code, or `timeout` when the command was killed at its deadline, or `signal` with no exit code at all.

Two patterns fall out of the guard. A **watchdog** runs a command on a schedule and wakes an agent on failure. A **trigger-when-green** polls until a command succeeds, then delivers.

`--verify` is the mirror image and applies only to `Spawn` tasks: `check` gates whether a turn starts, `verify` gates whether it counted as done, through the [same-session re-prompt loop](./scripting.md#verification-re-arms-the-same-run).

### Delivering to a live instance

`wake` pins a task to one exact live kind/session at add time; `--wake @me` and bare `--wake` resolve the caller through the shared CLI resolver. The runner confirms the session is alive, including `ended_at`, before sending through the ordinary durable message path.

| Intent | Header | Dispatch |
| --- | --- | --- |
| Self timer or command wake (`wake_meta`) | `Type: WAKE` | `Steer` |
| Any `Trigger::Signal` delivery, including team bindings | `Type: SIGNAL` | `Boundary { gate: Done }` |
| Scheduled loop delivery | `Type: WAKE` | `Boundary { gate: Done }` |

The sender is `@rimz` and delivery inherits the harness smart-compaction default. A boundary delivery reaches an idle agent immediately and parks for a working agent's next `done`; a self wake steers. A missing or ended session records `target gone` and removes the row. SIGNAL and WAKE are acknowledged as attributed harness messages, hidden in rendered transcripts, and kept in `--json` ([messaging.md](./messaging.md#the-message-header)).

## History, strikes, and arming

Every fire appends a `LoopRunRecord` to the user-global `~/.local/state/rimz/loop-runs.log.jsonl`. The log is per-user, so history survives a task being edited or removed. Show, logs, and health reads filter records by the current workspace root when known; legacy records with no `root` remain visible.

The record carries the resolved task `root`, result, mode (`scheduled` or `manual`), duration, error chain, check evidence (exit code, timeout flag, capped output, and the output file's path for a watch row), the watched command's `WatchVerdict`, the triggering signal's name and payload, the durable message id of a delivery, delivery target, supervised run id and transcript path, last message, cost, and fresh input and output tokens. Append caps the stored copies: 4 KiB of check output, 2 KiB of error text and last message, and a signal payload over 4 KiB collapses to a single `_truncated` field. `rimz loop show` reads it for a health verdict plus a separate agent-run rollup for check-gated work; `rimz loop logs` prints the stored forensics in full, including the output path above the check gutter.

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

Five `Assist` variants live in the log today:

| Variant | Writer | Records |
| --- | --- | --- |
| `auto_redeem` | the detached redeem helper | provider, decision reason, request id, available credits, soonest expiry, the natural reset it beat, consume outcome or error, whether a reset occurred, and refreshed window stamps |
| `auto_continue` | the detached continue helper | typed provider and session ids, display handle, park class, original park timestamp, delivery verdict, and the durable message id |
| `auto_compact` | the message delivery path after a compact command lands | target session, display handle, threshold, occupied context when known, and the durable compact-command message id |
| `idle_compact` | the detached idle-compaction helper | target provider and session, display handle, idle duration, occupied context, durable compact-command message id, delivery verdict, and error when present |
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
