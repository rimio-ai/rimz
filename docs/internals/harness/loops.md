# Loop scheduling

> The scheduler: where task definitions live, who keeps time, what one fire does, and the unattended recovery the same elder runs alongside it. [harness.md](./harness.md) is the map for this area. For users, the guide is [loops.md](../../guide/loops.md) and the flag reference is [cli/loop.md](../../reference/cli/loop.md).

## What the scheduler does

`rimz loop` fires agent work on a clock: a fresh supervised turn, a prompt delivered to an agent that is already running, or a shell command that guards either one.

The design constraint that shapes everything is that **there is no scheduler daemon**. RimZ is a room, and the room already elects one process to do shared work: the sidebar producer, the elder ([state.md § Renderers, the producer, and consumers](../sidebar/state.md#renderers-the-producer-and-consumers)). The elder keeps time for loop tasks on its ordinary data tick. Close the room and the clock stops. That is a deliberate trade: nothing runs when you are not there, and nothing outlives the room you can see.

Three rules follow from having no daemon, and they explain most of the module.

**Arming is not firing.** A task the elder has never seen is recorded with the current time and does *not* fire. Opening a room hours late therefore never replays what was missed, so there is no catch-up storm.

**Every fire is idempotent at the tick.** The elder writes the fire timestamp before spawning the helper, so a hot sub-interval tick cannot spawn the same occurrence twice, and a per-task advisory lock stops two overlapping runs.

**Exactly one history row per fire.** However a fire ends, gated, skipped, overlapped, expired, delivered, completed, or errored, it appends one record to the run log. That log is the only durable trace of what automation did, so it must be complete.

## Module layout

| File | Owns |
| --- | --- |
| [`schedule.rs`](../../../crates/rimz/src/harness/schedule.rs) | The vocabulary: `TaskAction`, `Schedule` and its parsing, `ParsedSchedule`, due evaluation, next-occurrence calculation, `TaskTiming` display states, and the `TaskShape` compile. |
| [`schedule/catalog.rs`](../../../crates/rimz/src/harness/schedule/catalog.rs) | The task catalog: the three sources, visible and runnable precedence, synthesized auto-ping rows, source-aware mutation, and scheduled consumption. |
| [`schedule/fire.rs`](../../../crates/rimz/src/harness/schedule/fire.rs) | The elder's side: arm-on-first-sight, due planning, `loop-fire.json`, and spawning the detached `rimz loop run <name>`. |
| [`schedule/runner.rs`](../../../crates/rimz/src/harness/schedule/runner.rs) | `TaskFire`: the ordered gate ladder, the run lock, the check, prompt preparation, the prepared effect, and the one terminal history transition. |
| [`schedule/run_log.rs`](../../../crates/rimz/src/harness/schedule/run_log.rs) | `LoopRunRecord`, `LoopRunResult`, the user-global JSONL history, cost rollups, and the daily-budget gate. |
| [`schedule/pauses.rs`](../../../crates/rimz/src/harness/schedule/pauses.rs), [`strikes.rs`](../../../crates/rimz/src/harness/schedule/strikes.rs) | Machine-local overlays: the pause and its effective-last-fire rule, and consecutive failure counts. |
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

A `Spawn` task names exactly one agent cell: a built-in kind, a profile, or an adapter-supported virtual cell such as `claude-auto`, `codex-yolo`, or `claude-ping`. Teams, multi-cell layouts, and command cells are rejected at add time, because a scheduled task owns exactly one supervised pane.

`TaskShape::compile` compiles each persisted row once into an action result and a timing result **independently**. That independence is the point: a row with a valid action and a malformed schedule stays visible and manually fireable while scheduled firing skips it, instead of one bad field hiding the whole task.

## Where tasks live

Three sources back the catalog, and they are not interchangeable.

| Source | File | Nature |
| --- | --- | --- |
| `Config` | `~/.config/rimz/loop.toml` | per-machine automation, like your crontab; never inherited by a clone |
| `Project` | `<root>/.rimz/config.toml` under `[tasks.*]` | shared automation that travels with the repo, and therefore trust-gated |
| `Instance` | `~/.local/state/rimz/loop-instances.json` | RimZ-owned ephemerals: one-shots, agent self-wakes, and poll-until rows |

The instance store exists so runtime churn never edits user config. An agent scheduling its own `--in 30m` wake writes state, not your `loop.toml`, and the row retires itself after firing.

Project tasks are more constrained than machine tasks, because a committed task cannot make machine-local claims. Loading rejects `root` (a project task runs at the project root), `wake` (it cannot pin a session on someone else's machine), and `deadline` (a poll-until timestamp is machine state), and requires `every` or `cron`, because a one-shot would have to delete itself from a trust-hashed file on fire.

A project task also runs commands on whoever pulls it, so it enters the project trust hash ([trust.md](./trust.md)) and stays inert until each user grants it.

### Visible and runnable precedence

The catalog resolves two maps at once, and the split is what makes an untrusted project task honest.

- **Visible** is what `rimz loop list` shows. A project definition shadows a same-named machine or instance row *regardless of trust*, rendered as `project · untrusted` or `project · stale`, so you always see the definition that would win.
- **Runnable** is what the elder and `rimz loop run` may execute. It admits a project row only when trusted, and otherwise falls back to the same-named machine task.

So during the untrusted window you see the project task and keep running the machine one, without the two ever double-firing.

## Schedule shapes

| Shape | Entry | Due when |
| --- | --- | --- |
| One-shot | bare `at = "07:00"`, or `rimz loop add --in 30m` | its calendar time arrives; the task then removes itself |
| Interval | `every = "15m"` | measured elapsed time since the last arm or fire crosses the interval |
| Calendar | `every = "weekday"` plus `at = "07:00"` | the first tick at or after the wall-clock time on a matching day, at most once that day |
| Raw cron | `cron = "*/15 * * * *"` | an in-process five-field matcher matches the current minute, and the last fire was in an earlier minute |
| Window-reset | `every = "reset"` on a `<kind>-ping` agent | an externally resolved `ResetSignal` says so ([below](#window-priming-pings)) |
| Poll-until | `every = "2m"` with `check`, `on`, an agent action, and `deadline` | the interval elapses, until the check trips the action or the deadline passes |

The day mask accepts `day`, `weekday`, `weekend`, a range like `mon-fri`, or a list `mon,wed,fri`. Calendar times, cron, `--in`, and `--until` all evaluate in the configured `timezone`, falling back to the system zone when unset.

The arming stamp sets the edge each shape reads, which produces one behaviour worth internalizing: a calendar task first seen *after* its time today waits for the next matching day, and a cron task first seen past a matching minute waits for the next match, but a tick a few seconds late still fires a calendar task, because the comparison is "at or after".

`Schedule::next_after` is the display counterpart of `due`, and it may return a timestamp at or before now, meaning the elder should fire on its next tick. `TaskTiming::evaluate` layers the display states on top:

| State | Meaning |
| --- | --- |
| `Blocked(trust)` | a project task awaiting or stale on its grant |
| `Paused` | an active machine-local pause overlay |
| `Invalid` | the schedule half of the row failed to parse |
| `Unarmed` | no room has seen it yet |
| `Upcoming(t)` | the next occurrence, still in the future |
| `Due(t)` | the next occurrence is at or before now; the elder fires it on its next tick |
| `NoOccurrence` | parsed, armed, but the shape yields no next time (a `ResetSignal::Unknown` ping) |

## Elder firing

`fire::fire_due_tasks` runs on the elder's data tick.

1. Load the runnable catalog for the room's project root, dropping untrusted project rows.
2. Keep only tasks whose normalized `root` maps to this room's `WorkspaceId`, so each room fires only its own tasks. `rimz loop add` writes a canonical absolute root; a hand-edited `~` or relative root is expanded and canonicalized before the ownership check, display, and execution.
3. Resolve a `ResetSignal` for each window-reset ping.
4. Plan every task against `loop-fire.json`, a per-room map of task name to last-fire `Timestamp` in the workspace runtime dir.
5. Write the new state, then spawn a detached `rimz loop run <name>` with fresh null stdio for each fire.

The plan is a four-way decision per task:

| State | Action |
| --- | --- |
| no stamp | arm: record `now`, do not fire |
| stamped, pause active | hold the existing stamp unchanged |
| stamped, schedule due | fire: record `now` |
| stamped, not due | keep the stamp |

State is written before any helper spawns, which is what makes a fire at-most-once per occurrence even when ticks are hot.

Pauses overlay every source without editing durable definitions. When a timed pause expires or `loop resume` ends one, that end becomes the **effective last-fire edge**, so a resumed schedule waits for its next occurrence rather than replaying everything missed while it was held. Pause state is machine-local, so pausing a project task affects only your machine.

## One fire

`rimz loop run <name>` is hidden, and after CLI trust and action validation it hands the fire to `schedule::runner::TaskFire`. That type owns one fire from its start time through exactly one history transition; the CLI executes the returned effect and reports its typed result back.

`TaskFire::prepare` walks an ordered ladder, and the order is load-bearing: everything that can refuse cheaply refuses before anything expensive or observable happens.

| # | Gate | Records on refusal |
| --- | --- | --- |
| 1 | the task's own `--budget-per-day`, summed from this task's completed runs in the configured local day | `budget skipped` |
| 2 | the room-fleet and provider-account [scope caps](./budget.md#the-fail-fast-gate) | `budget skipped` |
| 3 | the exact managed-launch provider quota, when a binding is proven | `budget skipped` |
| 4 | `--surplus` / `--surplus-after` forward headroom on the provider's longest window | `surplus skipped` |
| 5 | the ping window gate, for a `<kind>-ping` cell | `skipped` |
| 6 | the per-task advisory run lock | `overlapped` |
| 7 | the poll-until `deadline` | `expired` |
| 8 | the `check` command and its polarity | `skipped`, or a check-only terminal result |

Only then does the action run. `TaskFirePlan` returns `Done` (a gate already produced the terminal record), `Spawn` (a prepared `SupervisedRunRequest`), or `Deliver` (a prepared target and prompt). The CLI executes it and calls `finish`, which maps the outcome to a `LoopRunResult` and appends the record. All gates apply in both scheduled and manual modes, so `rimz loop fire` tests the real policy.

A closed gate costs nothing and adds no strike; the recurring schedule keeps polling until the condition clears. The surplus gate fails closed: an account with no window reading keeps the gate shut, since spending against an unknown budget is the failure this feature exists to prevent.

The run lock is `loop-run-<name>.lock` beside `loop-fire.json`, carrying the holder's `{pid, started_at}`. The kernel releases it when the runner exits or crashes, and display probes read it without rewriting its payload. `rimz loop stop` uses the durable cancellation path first and SIGTERM only as a backstop; if the holder still owns the lock afterwards, it prints the PID and lock path rather than escalating to SIGKILL.

An **ephemeral** task (a one-shot, or any task with a `deadline`) removes its own state row *before* the supervised run or delivery. A one-shot removed pre-fire that then fails to launch is not retried. A poll-until row also removes itself when its check fires the action, and expires without delivery once its deadline passes.

### Where a scheduled run lands

A `Spawn` fire sets `loop_zone` on its request, which sends the transient pane to the `rimzd` loop zone instead of splitting beside a caller. The runtime column's loop panel stays open and run panes stack under it, so a fire never splits the sidebar or a working tab. The panel is repaired rather than assumed: elder-tick repair restores any closed managed pane, and fire-time repair recreates a missing loop panel immediately. If the whole `rimzd` view is gone or the split fails, the run falls back to a new tab. Manual `rimz loop fire` keeps splitting beside the caller, so its foreground stream stays local.

A scheduled `Spawn` also gets a timeout it never asked for. `effective_spawn_timeout` uses the task's own `--timeout` when set; otherwise a *scheduled* fire receives the machine's `loop.default-timeout`, two hours by default, while a manual `rimz loop fire` stays unbounded. Unattended work should not wedge forever; a human watching one can decide for themselves.

### Checks

`check = "<shell>"` runs through `sh -c` at the task's project root before any agent action. `on = "fail"`, the default, fires on a non-zero exit or a timeout; `on = "success"` fires on a zero exit. `timeout = "5m"` bounds it, defaulting to five minutes.

A check-only task is a scheduled command: it logs `completed`, `failed`, or `timed out` with the exit code and capped combined output, and keeps recurring unless it is ephemeral. A guarded task logs the check evidence whether it skips or fires, and when the guard fires, the command, its exit status, and the capped output are appended to the base prompt, so the agent wakes already reading the evidence.

Two patterns fall out of the guard. A **watchdog** runs a command on a schedule and wakes an agent on failure. A **trigger-when-green** polls until a command succeeds, then delivers.

`--verify` is the mirror image and applies only to `Spawn` tasks: `check` gates whether a turn starts, `verify` gates whether it counted as done, through the [same-session re-prompt loop](./scripting.md#verification-re-arms-the-same-run).

### Delivering to a live instance

`wake` pins a schedule to one exact agent session. `rimz loop add <name> --wake @<handle>` resolves the address against the live rollup **at add time**, records a `wake` sub-table of `kind`, `session`, and `handle`, and rejects `agent` and every supervised-run flag, because delivery opens no pane.

On fire, the runner resolves the recorded root, confirms the pinned root session still exists, and sends the prompt through the same path as `rimz message`, gated `done`. An idle agent takes it immediately, a running agent parks it for its next `done` boundary, and a missing session records `target gone` and removes the schedule, because that exact conversation cannot come back. `rimz gc` runs the same liveness check as a safety sweep for wake schedules whose pinned session left the rollup without the task ever firing.

Self-paced loops are ordinary one-shots: an agent schedules its next `--in` wake at the end of the current one, and the instance row is removed before delivery, so the next one exists only while work remains.

## Window-priming pings

An `agent` value ending in `<kind>-ping` is a virtual cell that starts a provider's budget window at a time you choose. It runs at the lowest effort unless configured otherwise, and Claude's ping pins Sonnet so a flagship account does not prime at the flagship rate. The task declares its prompt explicitly, usually `prompt = "ping"`.

The window is account-scoped and shared by every session of a provider kind ([providers.md § Window fusion](../agents/providers.md#window-fusion)), so one ping primes the whole account. Ping turns count in spend totals, but the session spend-window detector treats them as loop-fired automation rather than human activity.

Before spawning, the runner reads the shared rate-limit cache and skips when the relevant window is already counting down. The read is best-effort: an unknown or cold cache falls through to the ping, since missing a window start defeats the feature while an occasional extra token is cheap.

`every = "reset"` lets a ping follow the provider's longest observed window through a three-state signal:

| `ResetSignal` | Source | Next occurrence |
| --- | --- | --- |
| `At(resets_at)` | a cached reset stamp | that stamp plus a one-minute margin, so the ping lands in the new window rather than the last seconds of the old one; a passed reset stays a catch-up edge |
| `ConfirmedDown` | an authoritative not-started or known reset-less reading | retry from the last fire stamp, at most hourly |
| `Unknown` | a cold cache, a logged-out account, a lifted limit, or a best-effort-only reading | nothing is scheduled |

Immediately before a reset-shaped ping, the CLI forces one bounded provider account-usage refresh through the normal nonce-guarded claim and publication path, then re-reads the longest window and records `skipped` if it is already running. An unavailable refresh keeps the cached gate, and a successful ping's own account reading supplies the next edge.

**Auto-ping** is the zero-configuration form. With `auto-ping = true` in machine `loop.toml`, catalog loading synthesizes an `autoping-<kind>` task (`agent = "<kind>-ping"`, `prompt = "ping"`, `every = "reset"`) for every built-in adapter that exposes ping arguments, scoped to the room's project root. Explicit instance, machine, or project definitions with the same name shadow the synthesized row, and a rootless catalog read synthesizes nothing. Generated rows have no stored definition to remove or rename: pause one with `rimz loop pause autoping-<kind>`, or turn them all off with `loop.auto-ping false`.

## History, strikes, and pauses

Every fire appends a `LoopRunRecord` to the user-global `~/.local/state/rimz/loop-runs.log.jsonl`. Loop config is per-machine but the log is per-user, so history survives a task being edited or removed.

The record carries the result, mode (`scheduled` or `manual`), duration, error chain, check evidence (exit code, timeout flag, capped output), delivery target, supervised run id and transcript path, last message, cost, fresh input and output tokens, and, for a ping, the refreshed window outcome. `rimz loop show` reads it for a health verdict plus a separate agent-run rollup for check-gated work; `rimz loop logs` prints the stored forensics in full.

`LoopRunResult` has fifteen variants, and `strikes::classify` sorts them into three signals:

| Signal | Results |
| --- | --- |
| Strike | `failed`, `verify failed`, `timed out`, `error`, `budget exceeded`; also `completed` or `delivered` whose check still shows the world broken |
| Reset | `completed` or `delivered` with a passing or absent check; `skipped` from a check that passed |
| Neutral | `budget skipped`, `surplus skipped`, ping `skipped`, `overlapped`, `canceled`, `expired`, `target gone` |

That table encodes the judgement calls. A turn that completed but left its check red is a failure, because the task is not doing its job. A gate that declined to spend money is not a failure at all.

`record_transition` appends first and then updates the overlays, because the history row is durable truth while the overlays are best-effort. Consecutive strikes reaching the threshold (`--max-strikes`, default 3, `0` to disable) auto-pause the task indefinitely, display `paused · N strikes`, and fire `loop_paused` notification handlers. `rimz loop resume` clears the counter and re-arms; `rimz loop fire` still works on a paused task for testing.

Strike counts live in machine-local `loop-strikes.json`, independently of run-log rotation, and an advisory lock serializes updates from concurrent task runners.

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

Auto-ping is the deliberate exception: it enriches its existing `loop-runs.log.jsonl` row instead of duplicating a record. A completed ping compares run-scoped pre-turn capacity against a fresh post-turn cache read and stores the shortest and longest window durations and reset stamps only when the reading changed.

`rimz stats` folds both assist-log generations together with completed ping rows, scoped to the active dashboard window, and publishes one rollup: ping count and cost; delivered continues and their summed `recorded_at - parked_since` recovered time; compact commands sent; redeem attempts and `reset` outcomes; and rebirths plus restored panes. The dashboard shows those five non-zero categories, `rimz stats --assists` renders the merged newest-first event stream, and `rimz stats --json` publishes both.

**Shipping a new smart strategy is one accountable slice**: define its typed trigger, evidence, and outcome record; append it from a writer outside the sidebar import graph; fold it into the Assists stats surface; and add its variant and writer to the table above.

## See also

- [scripting.md](./scripting.md): the supervised run every `agent` task spawns.
- [messaging.md](./messaging.md): the delivery path every `wake` task uses.
- [harness.md](./harness.md): launch compilation and addressing.
- [budget.md](./budget.md): the dollar scopes the gate ladder reads, and the park the `Budget` class waits out.
- [providers.md](../agents/providers.md): account windows, capacity readings, and the pricing behind recorded cost.
- [sidebar/state.md](../sidebar/state.md#renderers-the-producer-and-consumers): how the elder is elected and what else runs on its tick.
- [cli/loop.md](../../reference/cli/loop.md): every flag on `add`, `fire`, `list`, `show`, and `remove`.
