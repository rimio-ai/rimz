# Loop scheduling

> The scheduler: where task definitions live, who keeps time, what one fire does, how signals and waits reach the same machinery, and the assist log that holds every unattended intervention to account. [fleet.md](./fleet.md) is the map for this area. For users, the guide is [loops.md](../../guide/loops.md) and the flag references are [cli/loop.md](../../reference/cli/loop.md), [cli/wait.md](../../reference/cli/wait.md), and [cli/events.md](../../reference/cli/events.md).

## What the scheduler does

`rimz loop` fires agent work on a trigger. A fire starts a fresh supervised turn, delivers a prompt to an agent that is already running, or runs a shell command that can guard either one. The trigger is a clock, a signal selector, a watch spec, or a state condition. `rimz wait` is the agent-facing front end over the same task rows, for delays and watches.

There is no RimZ scheduler daemon. The room host already owns shared data work ([state.md § The room data plane](../sidebar/state.md#the-room-data-plane)), and the host keeps time for loop tasks on its ordinary data tick. For schedules that must run with the room closed, a user can install one OS timer that launches a one-off `rimz loop tick` and exits; it yields every root whose room is open ([The external tick](#the-external-tick)).

Four rules follow from having no daemon, and they explain most of the module.

**Arming is not firing.** A task the host has never seen is stamped with the current time and does not fire. A room opened hours late never replays the occurrences it missed.

**Every fire is at-most-once per occurrence.** The host writes the fire stamp before it spawns the runner, so a hot tick cannot spawn the same occurrence twice, and a per-task advisory run lock stops two runs from overlapping. A start that fails keeps that stamp: the occurrence is spent, the next tick does not retry it, and the failure is recorded instead (below).

**An event fires in the process that produced it.** A clock needs a timekeeper and an event does not: whoever emits a signal resolves the subscribers and spawns their runs itself. Signal and watch triggers therefore work with no room open and never touch `lanes/loop-fire.json`. The cost is that nothing queues a signal, so a signal is never replayed ([The signal vocabulary](#the-signal-vocabulary)).

**Every fire appends exactly one history row.** Gated, skipped, overlapped, expired, delivered, completed, or errored, a fire records one `LoopRunRecord`. That log is the durable trace of what automation did, so it must be complete. A fire that produced no row of its own is no exception: the launch site appends a `start failed` row in-process, because no helper that never ran can report its own failure.

## Module layout

Every path below is under `crates/rimz/src/`; `schedule/` means `harness/schedule/`.

| File | Owns |
| --- | --- |
| [`harness/schedule.rs`](../../../crates/rimz/src/harness/schedule.rs) | The vocabulary: `TaskAction`, `Trigger` and its parsing, `Schedule`, `ParsedSchedule`, due evaluation, next-occurrence calculation, `TaskTiming` display states, and the `TaskShape` compile. |
| [`schedule/catalog.rs`](../../../crates/rimz/src/harness/schedule/catalog.rs) | `TaskCatalog`: the three sources, visible and runnable precedence, source-aware mutation, scheduled consumption, `LoadedTask::enable` (enablement plus strike clearing), and `clear_overlays` for replacement and removal. |
| [`schedule/instances.rs`](../../../crates/rimz/src/harness/schedule/instances.rs), [`overlay_store.rs`](../../../crates/rimz/src/harness/schedule/overlay_store.rs) | RimZ-owned instance rows with locked insert, remove, and rename, and the locked persistence the overlays share. |
| [`schedule/arming.rs`](../../../crates/rimz/src/harness/schedule/arming.rs), [`strikes.rs`](../../../crates/rimz/src/harness/schedule/strikes.rs) | Machine-local overlays: enablement, bounded pauses, effective arming and source defaults through `ArmState::resolve`, the effective-last-fire rule, and consecutive failure counts. |
| [`schedule/config_edit.rs`](../../../crates/rimz/src/harness/schedule/config_edit.rs) | Comment-preserving TOML edits to machine `loop.toml` and project `.rimz/config.toml`. |
| [`schedule/fire.rs`](../../../crates/rimz/src/harness/schedule/fire.rs) | Clock firing shared by the host and the external tick: root ownership, arm-on-first-sight, due planning, `lanes/loop-fire.json`, and how the detached `rimz loop run <name>` is hosted. |
| [`schedule/when.rs`](../../../crates/rimz/src/harness/schedule/when.rs) | Condition grammar, ordered terms/readings, room CI source, the lazy provider-window reading, predicate evaluation, hold state, and typed fire evidence. |
| [`cli/loop_timer.rs`](../../../crates/rimz/src/cli/loop_timer.rs) | The systemd user timer and launchd agent: install, status, removal, unit rendering, and the external tick. |
| [`schedule/runner.rs`](../../../crates/rimz/src/harness/schedule/runner.rs) | `TaskFire`: the gate ladder, the run lock, the check, prompt preparation, the prepared effect, and the one terminal history transition; `stop_task`, the stop ladder; `in_flight_run`, the lookup of a run in flight by task name and root; `RunLocks`, the one listing of a root's run locks behind both and behind the display state. |
| [`schedule/throttle.rs`](../../../crates/rimz/src/harness/schedule/throttle.rs) | [The start throttle](#the-start-throttle): the home-wide ticket queue, the `Turn` handle, cap counts, pressure, memory, and disk readings, the start preflight, and the held-run and readings lookups. |
| [`schedule/runner/prompt.rs`](../../../crates/rimz/src/harness/schedule/runner/prompt.rs) | `compose_wait`: the wait line, the verdict line, the evidence, and the verbatim note. `compose_launch`: the one event line a launching fire puts before its prompt. |
| [`schedule/run_log.rs`](../../../crates/rimz/src/harness/schedule/run_log.rs) | `LoopRunRecord`, `LoopRunResult` and its `spawn_exit_code` mapping through `store::run::RunStatus`, the user-global JSONL history, cost rollups, and the daily-budget gate. |
| [`schedule/signal.rs`](../../../crates/rimz/src/harness/schedule/signal.rs) | The runtime signal: `Signal`, `SignalSelector`, `WatchVerdict` and `WatchOutcome`, the lifecycle-to-signal mapping, the conversion into the durable payload, `fire_signal`, `wait_output_path`, and `run_watcher`. |
| [`schedule/signal/team.rs`](../../../crates/rimz/src/harness/schedule/signal/team.rs) | The pure cohort-edge derivation behind `team.idle`, `team.waiting`, `team.failed`, and `team.ended`. |
| [`store/event.rs`](../../../crates/rimz/src/store/event.rs) | The persisted signal: the `SignalName` grammar and its reserved families, `SignalSource`, and the `SignalEventPayload` that `Store::append_signal` records ([store.md](../store.md#what-is-in-it)). |
| [`schedule/arm.rs`](../../../crates/rimz/src/harness/schedule/arm.rs) | `arm_delivery`, the one builder for session-pinned delivery rows: caller-scoped signal defaults and guards, locked dedupe, watcher spawning, and `retire_session`. |
| [`schedule/pending.rs`](../../../crates/rimz/src/harness/schedule/pending.rs) | The read-only projection of armed one-shot deliveries into per-agent pending waits. |
| [`schedule/team.rs`](../../../crates/rimz/src/harness/schedule/team.rs) | Team and resident-loop bindings: shared standing-delivery construction and materialization at registration. |
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

A supervised `Spawn` task (without `stay`) names exactly one agent cell: a built-in kind, a profile, or an adapter-supported virtual cell such as `claude-auto` or `codex-yolo`. `rimz loop add` rejects teams, multi-cell layouts, and command cells in this mode, because a supervised task owns exactly one pane.

`TaskShape::compile` compiles each persisted row into an action result and a timing result independently. A row with a valid action and a malformed schedule stays visible and can be fired by hand, while scheduled firing skips it.

## Where tasks live

Three sources back the catalog.

| Source | File | Holds |
| --- | --- | --- |
| `Config` | `~/.rimz/loop.toml` | per-machine automation, like a crontab; never inherited by a clone |
| `Project` | `<root>/.rimz/config.toml` under `[tasks.*]` | shared automation that travels with the repository; inert until trusted and enabled on this machine |
| `Instance` | `~/.rimz/ws/<workspace-dir>/records/loop-instances.json` | RimZ-owned runtime rows: one-shots, poll-until rows, `once` subscriptions, and every session-pinned delivery, including recurring clocks and standing signals |

The instance store keeps runtime churn out of user config. An agent that arms `rimz wait --in 30m` writes an instance row, not `loop.toml`, and the row retires itself after it fires. `TaskCatalog::load(Some(root))` reads that workspace's instances and `load(None)` reads machine tasks only. Wait rows found in `loop.toml` stay `Config` rows, and machine-scope `rimz gc --all` reaps them.

Instance mutation requires a readable room record. Without one, `crates/rimz/src/harness/schedule/instances.rs::mutate` treats removal, rename, and session retirement as empty no-ops before taking a lock; inserts and delivery inserts refuse.

A project task cannot make machine-local claims, so loading rejects `root`, `dir`, `wait`, `deadline`, `watch`, `wait-meta`, `once`, `when`, `for`, `stay`, `each-worktree`, `takeover`, and `subscribe`. Conditions and resident launches are machine-local; no new fields enter the project trust hash. Project rows require `every`, `cron`, or `signal`, because a one-shot would have to delete itself from a trust-hashed file.

A project task runs commands on whoever pulls it, so it enters the project trust hash ([trust.md](./trust.md)) and needs two approvals. Trust approves the config contents; the machine-local enablement record ([History, strikes, and arming](#history-strikes-and-arming)) approves that task for unattended execution here. `rimz loop add --project` writes an enabled record for its author, and a clone has no record, so the task starts disabled.

### Visible and runnable precedence

The catalog resolves two maps, and the split keeps an untrusted project task honest.

- **Visible** is what `rimz loop list` shows. A project definition replaces a same-named instance or machine row regardless of trust, rendered under NEEDS YOU as `blocked · project untrusted` or `blocked · project stale`, so the list shows the definition that would win.
- **Runnable** is what firing reads. A trusted project row replaces a same-named base row; an untrusted one never does, so the base row keeps running. The base is instance rows overlaid by machine rows.

An untrusted project row with no same-named base row still enters the runnable map, so the refusal happens where tasks execute. `fire::runnable_tasks_for`, which both the host and `fire_signal` call, drops every untrusted project row, and the trust gate in `rimz loop run` refuses one, except that a manual fire on a terminal offers an inline grant. During the untrusted window the user sees the project task, the machine task keeps running, and the two never both fire.

## Triggers

`parse_trigger` compiles the timing half of a row into one `Trigger`.

| Trigger | Entry | Fired by |
| --- | --- | --- |
| `Schedule` | `at`, `every`, `cron`, or `fire-at` | the host tick or the external tick, when `due` says so |
| `Signal` | `signal = "<selector>"`, optional `match = { k = "v" }` | the process that emits a matching signal |
| `Watch` | `watch` as a command string or a PID, check, or file spec | the detached `rimz wait watch` process, or the host's watch-lost rule |
| `Condition` | `when = ["team.stage=Done", "ci=passed"]`, optional `for = "30m"`; `provider = "claude"` when a term reads a window | the clock planner, once per true period, or once total with `once` |

A `SignalSelector` is `Exact(SignalName)` or `Family(String)`, parsed from `a.b` or `a.*` and serialized back to the same string. `*`, `a.b.*`, and `a*` are rejected, and emission refuses wildcards outright.

Validation rejects the shapes that cannot mean anything:

| Error | Shape |
| --- | --- |
| `TriggerConflict` | two trigger families on one row |
| `MatchWithoutSignal` | `match` with no `signal` |
| `OnceWithoutSignal` | `once` with neither `signal` nor `when` |
| `ForWithoutWhen` | `for` with no `when` |
| `BlankMatch` | a match value empty after trimming; the error names the task and key |
| `WatchWithCheck` | `watch` with a separate `check` guard |
| `BadSignal`, `BadWatch` | an unparseable selector or an empty watch command, file path, or pattern |
| `BadCheckWatch` | an invalid or zero check interval, or check polarity `any` |
| `ObsoleteCiSignal` | `ci.finished`, or a `conclusion` match on a `ci` selector; the message names `ci.passed` and `ci.failed` |

A `Watch` row is always one-shot, and a `Signal` row is one-shot only with `once = true`. `ephemeral_lifetime` names the rows that retire themselves: any row with no repeating trigger, plus any row carrying a `deadline`, `once = true`, a `watch` spec, or a `fire-at` instant.

`Trigger::resolve` is the whole matching rule, with three outcomes:

| Trigger | `Deliver` | `Skip` | `Ignore` |
| --- | --- | --- | --- |
| `Signal` | every `match` key passes, and the selector is this exact name or this signal's family | every `match` key passes, but an exact selector names another member of the same family | a different family, or a failed `match` key |
| `Watch` | the internal `wait.<task-name>` signal for this row | never | everything else, so one watcher's completion never fires another wait |
| `Schedule` | never | never | always; clocks are not signals |

A `match` key compares a JSON string payload value to the raw text and any other JSON value to its compact encoding. The whole value `"*"` instead requires that the payload carries the top-level key, including when its value is `null`. Missing keys never match. Other values remain exact: `feat/*` is literal, not a glob. Thus `from = "*"` on `team.stage` excludes the first flip, whose payload omits `from`.

## Schedule shapes

| Shape | Entry | Due when |
| --- | --- | --- |
| One-shot | bare `at = "07:00"`, or `rimz loop add --in 30m` | its calendar time arrives; the task then removes itself |
| Absolute one-shot | `fire-at = "<RFC 3339>"`, written by `rimz loop add --after-reset` | the stamp is before the instant and the instant is at or before now; the task then removes itself whatever the fire's result |
| Interval | `every = "15m"` | elapsed time since the last arm or fire reaches the interval |
| Calendar | `every = "weekday"` plus `at = "07:00"` | the first tick at or after the wall-clock time on a matching day, at most once that day |
| Raw cron | `cron = "*/15 * * * *"` | the in-process five-field matcher matches the current minute, and the last fire was in an earlier minute |
| Poll-until | `every = "2m"` with `check`, `on`, an agent action, and `deadline` | the interval elapses, until the check trips the action or the deadline passes |

The day mask accepts `day`, `weekday`, `weekend`, a range such as `mon-fri`, or a list such as `mon,wed,fri`. Calendar times, cron, `--in`, and `--until` evaluate in the configured `timezone`, or the system zone when it is unset.

The arming stamp sets the edge each shape reads. A calendar task first seen after its time today waits for the next matching day, and a cron task first seen past a matching minute waits for the next match. A tick a few seconds late still fires a calendar task, because the comparison is at-or-after.

`Schedule::next_after` is the display counterpart of `due`. It can return a time at or before now, which means the host fires on its next tick. `TaskTiming::evaluate` layers the display states on top, checked in this order:

| State | Meaning |
| --- | --- |
| `Blocked(trust)` | a project task awaiting or stale on its trust grant |
| `Disabled(reason)` | a manual or strike disable, or a project task not yet enabled on this machine |
| `Paused(t)` | a bounded pause whose deadline is still ahead |
| `Invalid` | the timing half of the row failed to parse |
| `Unarmed` | neither a room host nor the external tick has stamped it |
| `Upcoming(t)` | the next occurrence is in the future |
| `Due(t)` | the next occurrence is at or before now |
| `NoOccurrence` | parsed and armed, but the shape yields no next time, such as a cron expression whose fields never match a real date |
| `Listening { name }` | a signal subscription, which has no next time |
| `Watching { spec }` | a watch spec, whose watcher owns the timing |

## Host firing

`fire::fire_due_tasks` runs on the host's data tick:

1. Load the runnable tasks for the room's project root, dropping untrusted project rows.
2. Keep only tasks whose normalized `root` maps to this room's `WorkspaceId`, so each room fires only its own tasks. `rimz loop add` writes a canonical absolute root; a hand-edited `~` or relative root is expanded and canonicalized before the ownership check, display, and execution.
3. Plan every task against `lanes/loop-fire.json`, the per-room map from task name to last-fire timestamp in the workspace runtime directory.
4. Write the new state, then spawn a detached `rimz loop run <name>` with null stdio for each fire. A spawn that fails records a `start failed` row for that task and is left off the fired list; the stamp written in this step stays as it is.

The plan decides each task from its stamp, first matching row wins:

| State | Action |
| --- | --- |
| no stamp, `fire-at` at or before now | fire when live (record now); otherwise keep no stamp |
| no stamp | arm: record now, do not fire |
| stamped, disabled or pause active | keep the stamp unchanged |
| stamped, schedule due | fire: record now |
| stamped, watch row with no lock holder after the 30-second grace | watch lost: record now and fire the `Lost` outcome |
| stamped, not due | keep the stamp |

Because state is written before any runner spawns, a fire is at-most-once per occurrence even when ticks are hot.

A `fire-at` row is the exception to arm-then-fire: it has no later occurrence, so arming it past due would strand it. It fires on its first live sight past the instant, and its due check reads the raw stamp rather than `effective_last_fire`, so a pause or disable through the instant fires at the next live tick instead of skipping it. `--after-reset` resolves the instant at add through `runner::after_reset`, which runs `window_at_add`'s refusals and then refuses a lifted window, a window with no reset, and `--surplus` on the provider's longest window; a window that has not started resolves to the add instant.

### Condition planning

Conditions use the same planner pass. `fire_tasks` evaluates readings before the pure planner; the host supplies `CiSource`, and the external tick supplies none. Scope is `TaskEntry::run_dir()`. Board stages come from `scratch::board_stage`; CI uses the cache's exact path key: an Open or Merged PR with CI wins, then branch CI, then unknown. The host keeps last-known-good CI on failed probes. A `window.<span>.left` term reads through `when::WindowReadings`, which both callers build (the CLI renderers too, room open or not): it reads the room's logins and every account's stored kind-wide windows once, on the first window term of a pass and never on a pass without one, reads each term on the row's `account` when it has one and on the kind's room default login otherwise, and projects `ProviderCapacity::window_of_span` to the pass instant, so a window whose reset has passed reads 100 under the external tick with no fresh cache. A lifted window reads 100. The kind comes from the row's `provider` field, recorded at add by `runner::window_condition_provider` (the CLI's `add`) or by `arm::build_entry` (deliveries), because the host cannot resolve an `--agent` spec per tick; `parse_trigger` refuses a window term on a row without one. The add-time resolver, `runner::window_at_add`, owns every refusal in order (no provider, managed account selection probed through `unresolved_managed_state`, unresolvable login, no reading or an expired one, no window of that span); it reads the stored window unprojected, since projection would turn a stale cache into a full window. There is no signal-driven evaluation. Grammar and unknown/negation semantics are in [Conditions](../../reference/cli/loop.md#conditions).

`when::probe_scopes` reads the runnable catalog and arming overlay to select existing directories of Live condition tasks naming a forge key (`ci`, `pr`, or `pr.queue`). Both git-fact and PR producers union these scopes with pane-derived paths, using the same path keys. A scope without a pane survives target reconciliation while its task is live; removing the task releases the probe. Activity and focus are pane-driven, but pending CI or a queued PR also selects the hot PR-refresh tier. The `pr` condition reads the link's Open, Merged, or Closed state directly; a missing link or source is unknown, with no branch-CI fallback. `pr.queue` reads the open link's merge-queue fact as `queued` or `dequeued`, and `none` for a link without one or one that is no longer open, so `pr=open` stays true for a queued or dequeued PR. `when::ForgeKey` is the one list of the keys that read the forge cache: the parser and `evaluate` use it directly; `probe_scopes` and the CLI's no-room hint ask `WhenExpr::reads_forge`.

Resident task admission stores `TaskEntry.stay`, `each-worktree`, `takeover`, and `subscribe` (the `TeamSignalBinding` table shape) in machine TOML or instance rows. Project tasks reject all four keys, including false or empty values. Add resolves the ordinary layout and its prompt leader, retaining mode and effort overrides; only subscriptions require the leader's hook preflight. Resident fires hold the run lock, re-read the durable launch ledger, and return without history on a scheduled ledger hit. The CLI opens a missing room detached and executes the ordinary layout launch in a new background tab. Its resolved cwd bypasses CLI canonicalization so marker paths retain their lexical identity. Only after launch succeeds does the runner publish the ledger and append a `launched` history row with checkout and leader. A crash between launch and ledger can duplicate once; recording first would strand a checkout without its agent.

Resident fires apply the shared fleet, account and provider budget gates to every agent cell before launching, after the locked ledger recheck. Stored deadlines use the same expiry path as supervised fires. Checks run after the deadline and before the start throttle; a shell check runs on every eligible fire, and an agent check records a verdict or fails open. Residents have no daily task-budget gate, but their history records checker spend. Add-time validation excludes `--until`, per-task budgets and surplus gates; arming and strike disabling still apply at the scheduled helper entrance.

`records/loop-launches.json` maps task names and checkout paths to launch time and leader handle. Reads are strict: a corrupt ledger must not silently forget a launch. Writes take `loop-launches.lock`, prune vanished checkouts, and use durable atomic publication. Task replacement leaves the ledger alone; explicit removal clears that task's entries. Manual fire bypasses dedupe and replaces the checkout's record. `launch_ledger` is read-only; `launch_ledger_store` owns mutations outside the sidebar graph.

`records/loop-declines.json` maps task names and checkout paths to `{at, since, reason, profile, fingerprint}`. A resident agent check's polarity miss writes one `check_skipped` row, publishes the decline under `loop-declines.lock` with the same pruning and atomic-write discipline, and appends a `CheckDecline` assist. `launch_ledger::check_fingerprint` hashes the serialized check table plus `on`; the runner records the definition it asked, so a verdict from an in-flight old definition cannot hold its replacement. A scheduled helper with a matching fingerprint and the same condition `since` (or both absent) returns without a check or history row while the optional `recheck` has not elapsed. Old records without a fingerprint do not hold. No recheck means an indefinite hold; `0` never holds. A changed check table or `on`, a newer condition clock, or manual fire re-asks. A manual fire with a valid firing verdict clears only its checkout's decline. Catalog redefinition and removal clear declines; rename moves them, while the existing refusal to rename ledgered resident leaders stays. Shell checks, non-resident checks, and checks without a verdict never write decline memory. Reads are strict and remain in `launch_ledger`; writes remain in `launch_ledger_store`.

The planner still evaluates declined checkouts so false readings retire their clocks and a later true reading starts a new `since`. After that fold, `scoped_tasks` excludes holding declines from helper launches without advancing their fire stamps. `ConditionEvidence.since` carries the folded clock into the helper and history, defaulting to absent on old records. Fan-out inspection projects a holding decline as `CheckoutState::Declined` with its time, profile, and reason.

`loop show` loads the task's own room catalog and selects rows whose `loop_task` matches its name. Non-fan-out tasks keep the `SUBSCRIPTIONS` section: each real task name keeps a row, even when `loop list` collapses those subscriptions. One room-filtered run-log fold supplies the newest acting run, otherwise the newest heard signal, using the list's LAST wording. Fan-out tasks omit the section; the `wakes` fact counts armed rows for both kinds and names declared signals even when none remain. The parent task's health verdict remains unchanged; `show --json` preserves its existing keys and adds an unfolded `worktrees` array.

The planner reads the ledger for every resident task and excludes launched checkouts before planning. For a resident condition still true without a launch record, `fire::plan` retries once 5 minutes have elapsed since that key's last fire stamp, retaining the original hold start. The same rule applies to single-checkout and fan-out tasks. No helper writes the condition cache and no stored field is added; failures still accumulate strikes, while budget skips remain strike-neutral.

With `--takeover`, the resident helper reads the alive snapshot (the extent-fresh rollup with rest certificates attached) and the project's owned worktrees, and `schedule::takeover::plan` decides over those rows alone. An occupant is the current row of a launch instance (`address::launch_occupants`, so an earlier conversation in the same pane is neither counted nor closed twice) with no end, a bound pane, not a provider subagent, whose lexically normalized `worktree_path` is the launch checkout or inside it by path component and not inside an owned worktree nested there; team, channel, and loop task play no part. The helper hands the selector the checkout and each owned worktree in two forms, the path as launched and its `canonicalize`d physical path where that resolves, because a provider hook overwrites the row's path with the physical cwd while a launch records the marker's lexical one; a row matches under either form, a nested worktree excludes under either, and all matches are checked in the one pass before any stop. The selector resolves nothing itself. A paneless row has nothing to close and is not an occupant. An occupant, or a live launched child of one wherever its cwd, blocks when `effective_status` is `running`, `waiting`, or `paused` (a provider limit or a RimZ budget park, reported alike as `is paused on a limit`), when it is awaiting input, or when it is compacting; `sleeping`, a parked clean turn, and `failed` are settled. One blocker stops nothing: the helper returns `TaskFireEffect::TakeoverBlocked` and `TaskFire::finish` records the `takeover_blocked` gate row with the checkout and a reason naming each blocker, with no ledger entry and no assist, so the condition planner's five-minute retry applies. Otherwise each occupant goes through the existing `agents_cmd::stop_resolved` tree stop with one shared tracker. Stops and subscription retirement finish before any new identity is committed; a stop with any failure fails the fire without a launch or ledger entry, and that includes an occupant's supervised run whose pane was not confirmed closed. Nothing makes the check and the pane closes atomic: the window is the time between the snapshot read and the closes, and a launch row reads `idle` until its first turn hook lands.

Resident prompt leaders carry their launching task's name on the durable agent row. Root registration looks that task up and arms its `subscribe` bindings through the same builder as team bindings, with resident provenance and standing lifetime. Each subscription stores its declaring task in `TaskEntry.loop_task` (`loop-task` on disk), separately from `team`. Names are `loop-<task>-<agent>-<binding index>`. CI and PR subscriptions default to the row's recorded checkout, including its lexical marker path. Re-registration deduplicates; removing the task before registration arms nothing. Same-session restart preserves these declared bindings even when re-registration precedes retirement; session end still removes them.

The completed fire records a `resident_launch` assist after publishing the ledger, with task, condition evidence when supplied, checkout, the handles taken over from if any, and all launched handles. An assist append failure fails the fire, which records `errored` and counts a strike, and leaves the published ledger entry in place: the ledger is the record of a launch, so the planner's retry never opens a second layout in a checkout that already has its agents. A manual fire likewise keeps its new record. The missing assist line is visible as the `errored` row. A successful assist precedes the terminal `launched` history row.

For `each-worktree`, the planner expands a task over owned markers absent from the ledger, including paused and disabled tasks so arming policy can preserve their hold and fired state like ordinary conditions. Runtime condition and fire keys encode `(task, checkout)`; ordinary task keys and edge rules are unchanged. The marker path is lexically normalized (removing `.` and `..`, without resolving symlinks) for condition evaluation, PR probes, hidden helper `--cwd`, and the ledger, matching agent rows and subscription paths. It does not pass through `TaskEntry.run_dir` (which resolves configured paths). The helper validates that checkout against normalized owned markers, retains it separately, and takes a checkout-specific run lock before its ledger recheck. Manual fire selects the deepest owned checkout containing the caller cwd and uses the same normalized marker path. Fire planning enumerates owned worktrees once when any fan-out task exists, including paused tasks; probe selection still enumerates only for a relevant live fan-out task. A ledger read failure suppresses resident fires without suppressing ordinary tasks.

`crates/rimz/src/harness/schedule/fanout.rs::inspect` reads owned markers, launch memory, the condition cache through `crates/rimz/src/harness/schedule/fire.rs::scope_key`, and the same scoped evaluator as firing. It shares fingerprint matching with the planner, retaining the configured run directory in the fingerprint while the scope key selects the checkout. A true condition with a matching clock is holding until the hold elapses, then ready; without a matching clock it is holding since now when a hold is configured, otherwise ready since now. This starts no stored clock and does not predict the scheduler's first-sight or retry tick. Inspection writes nothing and reads forge facts only from the room's cache, gated by the CLI's fresh sidebar heartbeat. The CLI joins these rows with recorded takeover refusals, effective live leader statuses, and subscriptions scoped by run directory to render `WORKTREES`. `--all` unfolds waiting-on-many and ended-leader rows; JSON always includes every owned checkout.

`lanes/loop-when.json` is a runtime-class map from task name to `{fingerprint, since, fired}`, retained only while true (or held by arming policy), rebuilt from runnable rows each pass. The fingerprint holds the canonical expression, parsed hold duration, and run directory; a missing or mismatched fingerprint starts a new true period. First sight creates only the fire stamp. A standing condition is not an ephemeral task; `once` makes it ephemeral.

| Stamp / arming | Verdict / hold state | Action |
| --- | --- | --- |
| none | any | Stamp now, no evaluation or hold state. |
| stamped, not Live | any | Carry both maps unchanged. |
| stamped, Live | false | Drop hold state, carry stamp. |
| stamped, Live | true, no state | Start `{since: now, fired: false}`; apply the next row. |
| stamped, Live | true, not fired | Fire at `now - since >= hold` (absent hold is zero), stamp now, set fired. |
| stamped, Live | true, fired, resident without a ledger entry | Retry after 5 minutes from the last fire stamp, stamp now, keep the hold start and fired bit. |
| stamped, Live | true, fired, non-resident | Carry; no repeat until a false tick. |

Write `loop-fire.json`, then `loop-when.json`, before any spawn. A fire-stamp write failure aborts the pass without spawning. A condition-state write failure suppresses only condition fires, which retry on the next tick; already-stamped clock fires and lost-watch actions still proceed. Runtime teardown resets both holds and the CI cache. The existing host/tick ownership defenses remain the synchronization boundary.

`ConditionEvidence` travels in hidden `--condition-json`, renders through `Evidence::Condition`, and is stored as the optional `LoopRunRecord.condition`: canonical `when`, optional `hold`, `held_ms`, and readings (unknown is JSON null). Manual fire uses `Evidence::Manual`. A condition delivery is `Type: WAIT` at the Done boundary, never a synthetic signal.

The delivery builder stores a single canonical joined clause and a normalized hold. Condition dedupe compares the target session, clause list, hold, resolved project root, and scope (`TaskEntry::run_dir()`); identical expressions on different worktrees remain separate subscriptions. Signal dedupe still compares only the session, selector, matches, and resolved project root. Pending one-shot waits project as `PendingWaitTrigger::Condition`; the sidebar renders their expression and hold with a static wait glyph.

### The external tick

`rimz loop timer install` writes one user-level systemd timer on Linux or a launchd agent on macOS. Once a minute it runs the hidden `rimz loop tick`, which exits after one pass. The timer is only a clock host: it re-reads configuration every pass and leaves arming, trust, overlap locks, execution, and history to the paths above.

The tick finds roots from machine and instance task entries plus the trust grants that contain project tasks; trust grants are the project-root registry, because an ungranted project task cannot run anyway. For each root it derives the same `WorkspaceId` and runtime paths a room would. A fresh sidebar heartbeat means an host owns the root, and the tick skips it. Otherwise the tick prepares the runtime directories and calls the same planner with the root supplied explicitly, so a root that has never been opened needs no workspace record for its task to arm.

The runners must outlive the tick. Under systemd the tick starts each one through `systemd-run --user --scope`, moving it out of the timer service's cgroup so the runner and any multiplexer server it births survive, and waits up to five seconds to see the child's cgroup change. That route needs the systemd user manager, which a system-level unit, a container, or a sandbox does not have, so before it walks any root the tick asks `systemctl --user show-environment` — the same bus connection `systemd-run --user` makes, so it tracks systemd's own rules instead of re-deriving them from the environment. Failing that, the whole pass refuses with the fix named, rather than falling back to a host the runners would not survive: no stamp moves and no row is written, so every due task fires on the first tick after the host is fixed. External runners also get their own process group, on launchd too. Host and signal fires keep their inherited process group, so interactive launch probes cannot stall on background-terminal job control.

An external fire opens the room it needs. A `Spawn` fire births a room through the supervised-run path. A scheduled check-only fire, including a pure shell check, ensures its root's room is open after the budget and deadline gates and before it runs the check (the run lock precedes the deadline for a recorded room, or follows birth for a never-roomed root): a fresh heartbeat skips the repair, and otherwise it uses the normal detached room entry (durable ownership, the `rimzd` dashboard, closed-agent resume off, no confirmation prompt). That birth ends no top-level agent: the ones the dead session lost stay parked for a later rebirth or explicit resume, while their non-live subagents are closed (`rimz.child-not-resumed`), because a resumed parent relaunches any children it still needs. The runner stays a per-fire supervisor outside the panes, and the room hosts any agents it launches. Both paths leave the room open, so later ticks yield to its host. A `Deliver` fire still requires its pinned live session. A manual `rimz loop fire` stays in the foreground, with no transient scope and no room birth for a check-only task.

A hand-off that fails records what happened. Because `systemd-run --scope` enrols its own pid and execs in place, a runner that loaded is indistinguishable by pid alone from one that never started: the poll is 25 ms, so a fire that ends at once — overlapped, gated, expired — migrates and exits inside a single poll window, and the tick sees the same "child gone" it sees when the scope was never created. The run log settles it. After a child-gone the tick looks for a scheduled row for that task and root stamped at or after the instant it captured before spawning: a row means the run happened and nothing is added, and no row means the run did not start and one `start failed` row is appended. One fire, one row, in both directions. The row records that observation, not a cause: no row means either that the runner was never spawned or that it exited before it could record anything. The second case is real. `run_one` (`cli/loop_cmd/run.rs`) returns without a row on four early exits — a strict `task_catalog` load rejecting an instance the tick's lenient load tolerated, a project-trust refusal, a scheduled fire finding the task no longer `Live` after a `rimz loop pause` landed mid-tick, and an action that does not parse — and each of those reads as `start failed` on the systemd tick. Having the runner record its own early exits would narrow the row to a genuine launch failure, and is a separate change. The five-second cgroup timeout stays a warning only — the child is alive there and its outcome is unknown, so claiming either result would be a guess.

A room can be born between the tick's heartbeat check and its state write. That one-tick race is covered by the same defenses as hot host ticks: the shared fire stamp and the per-task run lock.

## One fire

`rimz loop run <name>` is hidden. After CLI trust and action validation it hands the fire to `schedule::runner::TaskFire`, which owns one fire from its start time through exactly one history transition. The CLI executes the effect `TaskFire` prepares and reports the typed result back.

`TaskFire::prepare` walks an ordered ladder. Everything that can refuse cheaply refuses before anything expensive or observable happens.

| # | Gate | Records on refusal |
| --- | --- | --- |
| 1 | the task's `budget-per-day`: the cost of every run of this task and root on the configured local day, including failed ones, against the cap, reserving the per-run `budget` | `budget skipped` |
| 2 | the task's pinned `account`: declared for the resolved kind, home a directory, no stored logged-out status | `account skipped` |
| 3 | the room-fleet and provider-account [scope caps](./budget.md#the-fail-fast-gate) | `budget skipped` |
| 4 | the exact managed-launch provider quota, when a binding is proven | `budget skipped` |
| 5 | `surplus` / `surplus-after` forward headroom on the provider's longest window | `surplus skipped` |
| 6 | the per-task run lock (deferred until birth when the fire opens a never-roomed root) | `overlapped` |
| 7 | the poll-until `deadline` | `expired` |
| 8 | the `check` command and its polarity | `skipped`, or a check-only terminal result |
| 9 | [the start throttle](#the-start-throttle), only for a fire that will start an agent | `throttle skipped` |

A fire refused at any gate is spent: nothing queues or replays it, and the next attempt is the next trigger.

Gate 1 trips when the day's spend has reached the cap, or when spend plus the per-run `budget` would exceed it. A `budget-per-day` without a `budget` is an error.

Gate 2 resolves the task's login once: the row's `account`, else the room's current account for the kind. Gates 3 to 5, the hooks preflight, tier routing, and the request's `login` all use that one resolution, so a pinned task is judged and launched on its own account and never on the room's. The gate runs before the caps because they read the account. It skips rather than errors because the fix is outside the task: the status cache holds only a room's current account and the accounts its agents run on (`RoomLoginSet::in_use`), so the logged-out skip needs a positive record and an account with none proceeds to launch. A home without trusted hooks passes this gate and fails the preflight as `error`. The supervised launch resolves the pin again for the kind its own routing pass lands on, so `TaskFire::finish_error` records a pinned fire's `LoginErr` as the same `account skipped`, by downcast and with the gate's wording; every other launch error, and any error on an unpinned task, stays `error`.

A resident fire walks its own order (run lock, ledger recheck, the scope gates, deadline, check) and takes its throttle turn last, before the room-birth callback. Both ladders acquire the throttle host's interrupt listener before running a check, so a subsequent throttle hold hears interrupts from the check onward.

Then the action runs. `TaskFirePlan` returns `Done` (a gate already produced the terminal record), `Spawn` (a prepared `SupervisedRunRequest`), or `Deliver` (a prepared target and prompt). The CLI executes it and calls `finish`, which maps the outcome to a `LoopRunResult` and appends the record. Scheduled and manual fires walk the same gates, so `rimz loop fire` tests the real policy.

A closed gate costs nothing and adds no strike; a recurring task keeps polling until the condition clears. The surplus gate fails closed: an account with no window reading keeps it shut, because spending against an unknown budget is the failure the gate exists to prevent.

The run lock is `locks/loop-run-<name>.lock`, separate from `lanes/loop-fire.json`, holding the holder's `{pid, started_at}`. An `each_worktree` fire locks per launch checkout instead, as `locks/loop-run-<name>-<workspace id of the checkout>.lock`, so launches into different checkouts do not overlap. The kernel releases a lock when the runner exits or crashes, and display probes read it without rewriting it. One snapshot answers for a root: `runner::RunLocks::list` lists the root's `locks/` directory once and probes each `loop-run-*.lock` entry once, keeping every outcome per file, so one entry that cannot be probed costs only the names that claim it. A task claims its bare name and every per-checkout name whose suffix parses as a workspace id, and it is running when any file it claims is held. With several held the snapshot reports the earliest-started holder, a holderless lock last. A held file that none of the root's rows claims is a row-less run, named by the file's whole stem: a supervised one-shot holds only the bare lock, so the stem is the name `show`, `logs`, and `stop` resolve back to that file. The display state, `in_flight_run`, and `stop_task` all answer from the snapshot, and one function spells the file name for the acquiring side and the lookup alike. `runner::stop_task` is the stop ladder behind `rimz loop stop`, keyed by task name and root rather than a loaded row, with the CLI passing its supervised cancellation in as a closure:

1. Look the task's held lock up; with none held it reports no active run. Every later step waits on the lock the lookup found, so a second `loop stop` reaches a fan-out task's next checkout.
2. Cancel the newest active run through the durable path and wait five seconds.
3. Send SIGTERM to the holder and wait five seconds more. Only this step appends a `canceled` row from the stop path itself, stamped with the root the ladder was given.
4. A holder that still owns the lock is not escalated to SIGKILL; the error names its PID and the lock path.

An ephemeral task removes its own row before its supervised run starts, so a one-shot that then fails to launch is not retried. A delivery removes the row once dispatch returns, whether it succeeded or errored, so the wake's message record exists before its row disappears and turn-completion waits never see the agent rested in between ([messaging.md § Reply waits](./messaging.md#reply-waits)). A crash between dispatch and removal leaves the row to fire again. A scheduled fire of a `fire-at` row removes the row in `TaskFire::finish_record`, whatever its result, so a gate skip, an overlap, or a check skip ends it too; a manual `rimz loop run` leaves it, and a spawn that never starts records `start failed` without reaching the runner. A poll-until row also removes itself when its check fires the action, and expires without delivery once its deadline passes. A watch check-in is nonterminal and leaves the row in place; the final outcome consumes it.

While a consumed one-shot's run holds its lock, `rimz loop show`, `logs`, and `stop` follow the lock instead of the row. A name with no row falls back to the caller's project root, and `runner::in_flight_run` runs the lock lookup there, reading the newest non-terminal run record for the name only while the lock is held. The lock decides and the record enriches: the record lands after the consume, and a hard-killed runner leaves a non-terminal record behind a free lock, which is no run. `stop` runs the same ladder with that root.

A task with a row is looked up the same way from its own root, and every display says `▸ running` with the time since the holder started: `list` in LAST and `watch` in its state cell, and `show` and `logs` with the holder's PID and the run id, in the headline and as the last line. `show` adds the stop hint, and `show --json` carries the same reading under `running`. The running state overrides timing state. A consumed one-shot keeps a ROOM row with `one-shot fired` in TRIGGER and a real task name; `watch` keeps its running row with no next fire. It is enrichment on the display commands: when lookup fails, `list`, `show`, and `logs` warn on stderr and render as if no run were in flight, `watch` repaints without warnings, and `stop` fails rather than reporting no active run. An unprobeable lock file no row claims is skipped, and an unlistable locks directory costs `list` one warning per root.

The list's owned per-room model is loaded strictly for the caller and best-effort for other roots discovered from machine tasks and instance records. Each root has its own catalog precedence and root-filtered history fold; legacy history without a root remains eligible in every room. Neither discovery nor caller enrichment opens a new store. Missing checkout directories are a display classification, not a durable retirement. Text, JSON, and the compact watch dashboard read the same rows; only list text collapses owned signal subscriptions. Last acting results and newest heard siblings come from `LoopRunStats::acting` and `LoopRunStats::heard`, with no second CLI classification. Terminal wrapping is confined to TRIGGER, preserving every other cell and the real names in JSON. Watch resolves its caller workspace once, reloads the room model each frame, and suppresses the model's enrichment warnings to keep the dashboard quiet.

### The start throttle

`schedule/throttle.rs` serializes agent starts across every room of one RimZ home. `TaskFire::prepare_throttle` is the last gate of both ladders: the one-shot ladder reaches it from `prepare_effect` only when the action is `Spawn`, before `prepare_spawn` consumes an ephemeral row, and the resident ladder after `prepare_deadline`. A delivery, a check-only fire, a fire its check skipped, and a task with `throttle = "off"` never touch the queue, and with `pace = "0s"` and no cap or limit configured `admit` returns `Open` without writing anything. A verify retry is the same run and is not gated again.

The queue is a directory, `~/.rimz/loops/throttle/`, behind the lock `~/.rimz/loops/throttle.lock`. Each waiting run owns one ticket file named by its enqueue time and a UUID, so a directory listing is the queue order. A ticket records its owner (`pid` and process start token), task, root, checkout, and one of three states:

| State | Meaning | Removed when |
| --- | --- | --- |
| `waiting`, with the last blocking reason | The run has not been admitted. | Its owner is dead, or it is skipped at `max-wait`. |
| `admitted` | The run holds the turn and is launching. | Its owner is dead, or its `Turn` is dropped unreported. |
| `launched`, with the commit time, workspace, and launch reference | The launch committed; the turn is winding down. | The provider reported the agent, or `pace` passed since the commit. |

`admit` rechecks once a second, and reads the clock after taking the lock. Once a run has been held, a pass at or past `max-wait` skips it before trying to admit, so a hold that clears at the deadline, or a recheck that wakes late, never starts the run. Each pass, under the lock, sweeps the tickets that are over and then decides for its own ticket only. A ticket that cannot be parsed is removed with a warning so one damaged file cannot disable every loop through repeated error strikes. An I/O error other than not-found still fails the pass with `ThrottleError`; `throttle::held` leaves unreadable tickets out of the display. At most one ticket is `admitted` or `launched`, and while one is, every other waits. Otherwise the first waiting ticket is the candidate, except that a task held on its own cap is stepped over by tickets of other tasks (task and root both compared): once a waiting ticket records the task cap, it and every later waiting ticket of that task are stepped over, so the task steps aside as a whole. Tickets of one task never overtake each other, and nothing else reorders: a ticket of an uncapped task held by any other limit is never stepped over. A check's signal-hook handler, once dropped, leaves SIGINT ignored, so no fire relies on the inherited disposition. A fire about to run a check registers a SIGINT flag through `Host::interrupts` before the check starts and `prepare_throttle` hands it to `admit`, so a Ctrl-C after the check is heard if the fire goes on to be held; a fire that ran no check has none, and `admit` registers one once the run is held, before it announces the hold. Either way `admit` keeps the flag for the rest of the wait and checks it each pass right after taking the queue lock, ahead of the deadline and of any admission, returning `Interrupted` when it is raised. This does not cover a disabled throttle, a throttle with no configured gate, or a Ctrl-C arriving during the pass that admits the run. The runner records `canceled`, exit `130`, with the fired check, and the dropped turn removes the ticket. Only the candidate samples the caps and limits, in the pass that would admit it; a blocked verdict is reused for up to five seconds, and an admission always rests on a sample taken in that pass. Since the previous launch's row is committed before its turn is reported, the next candidate's count already includes it.

The turn handle crosses to the launch commit. `Turn::report_launch` stamps the ticket `launched`: `cli/supervised/run.rs::execute_attempt` calls it after `harness::run::create`, through `SupervisedRunRequest.throttle_turn`, and `run_one` calls `TaskFire::report_launch` with the resident leader. A second report changes nothing. Nothing in the launching process waits after that: whichever process scans the queue next releases the turn, so a helper never lingers for pacing. The provider has reported when a row carries the launch reference as `launch_id` under another `agent_id` (the hook adopted the provisional row), or when the provisional row is ended, failed, or gone. Dropping the last clone of an unreported `Turn` removes its ticket, which is how every error path between admission and commit frees the queue; a process killed outright leaves a ticket whose owner is dead, which the next pass sweeps, and `runner::stop_task` sweeps at once after a SIGTERM.

An agent is active for the caps while its row is not ended, its effective status is `running`, it is not inside the bounded compaction window (`AgentState::is_compacting` at the host's clock), and its runtime owner is not dead. `max-active` counts those across `workspace::known_workspaces()`, and inherits that census: a workspace whose `workspace.json` does not read, or that loses the census's dedupe by session name, is not counted, so the machine count can run short. Reading every state directory directly would be a new home-wide inventory, which this change does not add. `max-active-per-task` counts, in the task root's workspace, the rows stamped with the task's `loop_task` or matched by one of the task's open run records. A store that cannot be read is a hold naming the workspace, never a short count. A configured pressure limit or memory floor whose source cannot be read is an error instead: `throttle::preflight` refuses `rimz start`, and a fire records `error`. Pressure is the `some` line of `/proc/pressure/<resource>`, held on the larger of `avg10` and `avg60`; memory is `proc::memory`; disk is `statvfs` at `throttle::disk_at`: for a task with `worktree`, the target that `worktree::worktree_path` (or `worktree_parent` for a generated name) resolves from the task root, measured at its nearest existing ancestor because the worktree is not created yet; otherwise the launch checkout. `rimz loop show` reads the same path. The ticket keeps the launch checkout as its place. Nothing samples in the background.

A fire held past `max-wait` returns `Skipped` and goes through `record_gate` as `throttle skipped` with the last reason, stamped at the fire instant like every other gate skip. An admitted fire that waited stamps `throttle_wait_ms` on its terminal row. A terminal row of a spawn that fired on a check carries that check (`TaskFire::fired_check`), whether the throttle skipped it, an error ended it, or Ctrl-C canceled it. The held state is read by `throttle::held` without the lock, since a ticket is replaced by atomic rename. It lists every waiting run of the task with its owner pid and checkout: `in_flight_run` carries the list for `show`, `logs`, and `show --json`, and attaches no run record when the lock's holder is itself one of those waiting runs, since a held fire has started none, and `list` and `watch` read it for a task whose lock is held. The renderer (`render::split_held`) shows held in place of running only for the ticket whose pid is the run-lock holder's, and the task's other waiting runs as a count and first reason beside it, so a fan-out never pairs one checkout's reason with another's pid. The throttle keys are not command-executing, so they stay out of the trust hash, and a hold or skip is a history row, not an assist record.

### Where a scheduled run lands

A scheduled `Spawn` fire gets `force_new_tab` from `schedule::runner::shape_loop_owned` ([scripting.md § Runs the scheduler starts](./scripting.md#runs-the-scheduler-starts) lists the rest of the request shape). Its unfocused tab is named `loop <task>`, docks a sidebar, and keeps the firing pane's tab as its `after` anchor. A scheduled `rimz loop run` inherits the firing process's environment, including the sidebar's or emitting agent's pane id, so placement is forced rather than inferred from a missing ambient pane. The `loop ` prefix keeps a task named `rimzd` out of the daemon-view classification: tab status and the run tab's sidebar bell can surface its waiting agent. The loop panel remains a display, repaired by the host ([rimzd.md](../rimzd.md#who-repairs-and-when)), and hosts no runs. Each retry opens its own tab. A manual `rimz loop fire` of a `Spawn` task still splits beside the caller, or opens a titled tab outside a pane, so its foreground stream stays local. Resident tasks are unchanged.

Channel inheritance is the exception for loop-owned launches, including manual fires, check launches, and residents. Their channel and bare-role team inference come from the launch checkout (or an explicit launch channel), never the firer's `RIMZ_CHANNEL`: the firer observed the trigger, not a parent asking a child to join its lane. A loop-owned launch likewise resolves no agent caller from environment or process ancestry: the firer's chain depth, subagent status, and room pin cannot refuse it, and it records neither `launched_by` nor a run `reader`. The scheduled helper retains its ambient channel for delivery-target scoping; only launches exclude it.

A scheduled `Spawn` also gets a timeout it did not ask for. `effective_spawn_timeout` uses the task's own `timeout` when set; otherwise a scheduled fire takes the machine's `loop.default-timeout`, two hours by default (`SCHEDULED_RUN_DEFAULT_TIMEOUT`), and a manual fire stays unbounded. Nobody is watching an unattended run, so it must not wedge forever.

Loop-owned runs, both `Spawn` turns and agents a check launches, close their panes on every terminal status unless explicitly kept. The in-pane wrapper defers self-cleanup while the run's waiter is live, which preserves verification re-prompts and failure-tail capture; `store::run::run_waiter_is_live` probes the bound socket instead of trusting a pathname that may be stale. If the waiter dies, including when a check times out, the wrapper reclaims the pane once its record is terminal. The evidence survives the pane: the supervised `RunRecord` keeps the failure tail, transcript path, and status, and the `LoopRunRecord` keeps the fire's outcome and check evidence for `rimz loop logs`.

Loop-owned runs use the same [parked-run rule](./scripting.md#parked-runs): a clean end with an owed wait, wake message, or launched-fleet result stays `Running` and keeps its pane. A stranded park fails through the wrapper's two-look settle; the existing run deadline and terminal cleanup still apply. A park keeps the task's run active, so `newest_active_run` suppresses the next scheduled fire as `Overlapped` for as long as the wait runs: a loop whose agent arms `rimz wait --in 30m` skips the fires inside that half hour.

### Checks

`check = "<shell>"` runs through `sh -c` before any agent action, in the task's `dir`, else the linked worktree it was armed from, else the project root.

| Field | Values | Default |
| --- | --- | --- |
| `on` | `fail` fires on a non-zero exit or a timeout; `success` fires on exit 0; `any` fires on every outcome | `fail` |
| `timeout` | a duration such as `5m`; the process group is killed at the deadline | 5 minutes |

A hidden `loop check-exec` trampoline calls `setsid` before it executes the shell, which drops the caller's controlling terminal and keeps the check and any nested waiter in one killable process group. A manual interrupt is forwarded to that group, with a one-second grace before an unresponsive check is killed; the fire records `canceled` and exits 130 without evaluating polarity or launching an agent. Either stop waits at most 200 ms for the output drains, so a pipe held by an escaped process cannot block the history row.

Every check, scheduled or manual, receives `RIMZ_LOOP_TASK=<task-name>`, `RIMZ_WORKTREE_PATH` set to its execution directory, and the workspace pin (`RIMZ_WORKSPACE_ID`, `RIMZ_PROJECT_ROOT`) from `workspace::pin_env`, which binds nested RimZ commands to the task's root. Inherited `RIMZ_AGENT_*` and `RIMZ_CHANNEL` are removed, so nested commands scope to the check directory's channel rather than the firer's identity. Watched commands (`rimz wait --run`) keep their channel.

A prompted `rimz agents <cell> "<prompt>"` launched from a check becomes a loop-owned supervised run. `-p` is implied, the command blocks for the result, the final message goes to the check's captured stdout, and the exit code reflects the run status. Without an explicit worktree selection the agent lands in the check's own checkout, whereas the task's `agent` action runs at the canonical project root. The run opens an unfocused `loop <task>` tab with a docked sidebar whether the fire was scheduled or manual, with or without `--new-tab`; it inherits `loop.default-timeout` unless the launch sets a timeout, and cleans up its pane unless `--keep` is set. An unprompted launch is refused because there is no turn to complete, and `--bg` is refused because the check must await the result. The conversion does not apply inside an agent pane carrying `RIMZ_AGENT_ID`, where agent ancestry owns child launches. Room birth clears the check marker so it cannot leak into later launches from a human shell.

A check-only task logs `completed`, `failed`, or `timed out` with the exit code and capped combined output, and recurs unless it is ephemeral. A guarded task logs the check evidence whether it skips or fires.

An agent table in `check` selects a single Claude or Codex cell. `schedule::runner::agent_check` retains the complete resolved profile cell (prompt sources, skills, permission arguments, and isolation), finalizes it, builds an `ExecRequest.headless`, and uses `launch_plan::prepare_exec` and `apply` without room agents. The checker uses `LaunchLogin::RoomDefault`, not the task action's account pin. Before spawning, it builds a `FireScope` for its own kind and login and runs `TaskFire::scope_refusal`, the same scope gates as the action cells. A refusal returns the gate's result and reason through `prepare_check` without recording a check or launching the action. Folder-trust and launch configuration preconditions refuse before spawn. Checker prompt-file paths follow the action's machine/project origin rule. Schema and verdict files live in a fresh temporary directory under the launch temp unit; `launch_plan::compile` maps the host paths through `sandbox::TmpView::agent_path` before argv compilation, and the result reader retains the host paths. `proc::run_bounded_output_interruptible` runs the compiled argv with stdin closed and kills the process group at the check table's own timeout (default 5m), or when the fire's existing SIGINT flag is raised. An interrupt records `canceled`, exits 130, and launches or delivers nothing, even without a throttle. Both stops bound pipe draining to 200 ms. A timeout retains already-read bytes if an escaped child holds a pipe.

The `### Check` body replaces `### Loop`: no pane and no user, no questions, read-only instructions, the guarded task and trigger, the action and its prompt's first line, polarity, and the fixed JSON verdict contract. Read-only is an instruction, not a permission guarantee. Hooks are disabled by the adapter's headless form, so the Environment reminder retains its launch-time listing rather than relying on a prompt-submit hook. The compiled process strips `RIMZ_AGENT_*` and `RIMZ_CHANNEL` both before execution and after shell startup, while keeping the loop marker and workspace pin. Launch warnings join the check output instead of reaching a terminal.

`CheckRecord.agent` carries profile, kind, resolved model, optional `{pass, reason}` verdict, error, cost and input/output tokens. A valid verdict maps to code 0 or 1 and uses ordinary polarity; no verdict maps to no code and always fires, including under `on = success`. The error and bounded stdout/stderr tail survive in output. Fired prompts append ``--- check by `<profile>` (<kind> <model>): pass ---`` or `fail` or `no verdict`, then the reason or error, through the same augment site as shell checks. Every terminal fire retains checker usage, including declines and later gate/launch failures; supervised completions sum checker and action usage. Every agent-check polarity decline appends one `CheckDecline` assist, including ordinary tasks. Declines record one `check_skipped` row; resident agent declines use the memory above, while ordinary checks re-ask on every fire.

A terminal `Watch` outcome takes the same path with the watch already evaluated: `prepare_check` converts the signal's `WatchOutcome` instead of executing anything, and polarity applies unchanged. A command watch defaults to `on = "any"`; polled watches always use that row polarity, with check polarity applied inside the watcher. A `Lost` outcome converts to a failed check, so `any` still delivers it and `on = "success"` skips it. A polarity skip records `skipped` and consumes the ephemeral wait without delivering a message. A `Running` or `NotMet` check-in bypasses polarity and consumption.

### The prompt a fire delivers

`resolve_effect_prompt` builds the delivered text. The base is `prompt`, or `prompt-file` read at fire time, with a relative path resolved against the machine config directory. `resolve_task_prompt` lets a wait row have no prompt at all, while a `Spawn` row requires one. Nothing is substituted: `{{key}}` is delivered as typed.

A fire that launches an agent (a row with `agent` and no `watch`, resident or single run) goes through `compose_launch`. The [Loop reminder](#the-loop-reminder) already carries the standing frame, so the message is the event and the prompt:

| Evidence | Message |
| --- | --- |
| condition | ``Rule `<name>` fired here. <key>: <reading> · <key>: <reading>``, the readings in evidence order with `unknown` for one with no value, a blank line, then the prompt. The hold is not repeated: the reminder's trigger clause states it. |
| signal | ``Rule `<name>` fired here. <subject>``, the `signal_headline` subject followed by each `signal_details` entry after ` · `, a blank line, then the prompt |
| none (a clock fire, any hand fire) | the prompt alone |

Every delivery to a running agent (`--wait`, a self wait, a subscription) and every watch row goes through `compose_wait`: the wait line, the verdict, the evidence, then the loop or team prompt verbatim after a blank line. Durations are elapsed time, not wall-clock.

| Trigger | Wait and evidence lines |
| --- | --- |
| timer | `waited <delay> [<name>]` |
| watch | `WatchSpec::headline`, `WatchVerdict::label`, the name, and `output: <agent-visible path> (<FileSummary::label>)` when a path is present and the file is non-empty; only command watches say `no output` for an empty file. The tail is never inlined; a file pattern match includes its matched-line preview. |
| signal | `waited on <subject>` and `fired [<name>]`. `signal_headline` builds the subject per name from the payload, dropping a segment whose key is absent or mistyped; the [subject table](../../reference/cli/loop.md#signals) is the contract. A built-in family adds no line, except `pr.dequeued`'s `queue checks:` line; any other family adds one `key: value` line per top-level payload field. No line is the payload as JSON. |
| condition | `waited on <expr>`, `held <hold> [<name>]` or `fired [<name>]`, then one `key: value` line per reading, `unknown` for one with no value |
| manual fire | `fired by hand` |

Self waits carry no note and no armer line. A check-in appends its stop and next-alarm commands.

A guard that fired appends its own block through `augment_prompt`, after the composed body of a launch or a delivery, so the agent wakes already reading the evidence:

```text
--- check `cargo test` exited 101 ---
<output tail>
```

The status in that header is the exit code, `timeout` when the command was killed at its deadline, or `signal` when it has no exit code.

Two patterns fall out of the guard. A watchdog runs a command on a schedule and wakes an agent on failure. A trigger-when-green polls until a command succeeds, then delivers.

`verify` is the mirror image and applies only to `Spawn` tasks: `check` decides whether a turn starts, and `verify` decides whether it counted, through the [same-session re-prompt loop](./scripting.md#verification-re-arms-the-same-run).

### The Loop reminder

A fire that launches an agent also tells it what launched it, in the launch reminder's `### Loop` section ([order](./fleet.md#launch-reminders)), so the prompt file need not explain its own lifetime. `harness/schedule/runner/reminder.rs::compose` writes the body in the firing process, from the task row, the fire mode, `keep`, and the single run's resolved timeout. It travels as the non-durable `ExecRequest.loop_reminder`: a resident fire sets it through `TaskFirePlan::Resident` on the prompt leader's cell only (`plan::fresh_agent_argv` keys on the cell that carries the prompt), and a single run sets `SupervisedRunRequest.loop_reminder` after `shape_loop_owned` resolves the timeout, which `cli/supervised/run.rs::execute_attempt` copies onto every attempt. Restart, resume, rebirth, and a check-launched run never carry it, and the durable launch identity does not record it. An adapter with no append-system-text channel drops it with the rest of the reminder.

A scheduled fire opens with the origin paragraph: the rule, what it launches (`one agent in each worktree` for `each-worktree`), the trigger (the schedule's description, `on the cron schedule` with the expression for a raw cron, `on the signal` with its selector, or `when <expr> holds [there] [for <hold>]`; a watch adds nothing), the check clause for a single run (`when`, or `and` after a condition, `its check … fails` or `passes`; `on = any` reads `after its check … runs`), and the fixed-text sentence. A hand fire replaces it with `The user fired the rule … by hand.` The standing paragraph follows: lifetime (a resident stays on; a single run is fresh unless the row is ephemeral, then one turn or the verify loop, with the pane clause dropped under `keep`), who is watching, then for a resident its `subscribe` signals (`for this worktree` only when every binding defaults its match to the path) and for a single run the verify cap (`VERIFY_MAX_ATTEMPTS_DEFAULT` when unset) and the timeout. Task text renders in backticks as typed: only `<` (as `&lt;`) and control characters are escaped, so the span cannot close the reminder tag. The pinned wording lives in `runner/reminder/tests.rs`.

### Delivering to a live instance

`wait` pins a task to one exact live kind and session at add time; `--wait @me` and bare `--wait` resolve the caller through the shared CLI resolver. The runner confirms the session is alive, including `ended_at`, before it sends through the ordinary durable message path. A missing or ended session records `target gone` and removes the row.

| Intent | Header | Dispatch |
| --- | --- | --- |
| Self timer or watch (row has `wait_meta`) | `Type: WAIT` | `Steer` |
| Any `Trigger::Signal` delivery, including team bindings | `Type: SIGNAL` | `Boundary { gate: Done }` |
| Scheduled loop delivery | `Type: WAIT` | `Boundary { gate: Done }` |

The sender is `@rimz`, and delivery inherits the harness smart-compaction default. A boundary delivery reaches an idle agent immediately and parks for a working agent's next `done`; a self wait steers. Receivers acknowledge `SIGNAL` and `WAIT` as attributed harness messages, hidden in rendered transcripts and kept in `--json` ([messaging.md § The message header](./messaging.md#the-message-header)).

## History, strikes, and arming

Every fire appends a `LoopRunRecord` to the user-global `~/.rimz/logs/loop-runs.log.jsonl`. The log is per-user, so history survives a task being edited or removed. `rimz loop show`, `rimz loop logs`, and health reads filter records by the current workspace root when it is known; records with no `root` stay visible everywhere.

A record carries:

- the resolved task `root`, the result, the mode (`scheduled` or `manual`), the duration, and the error chain;
- check evidence: exit code, timeout flag, capped output, and for a watch row the output file's agent-visible `output_path`;
- the watched command's `WatchVerdict` in `watch`;
- the triggering signal's name and payload;
- the delivery's durable message id and target, or the supervised run id and transcript path;
- the last message, cost, and fresh input and output tokens;
- `throttle_wait_ms`, how long the start throttle held a fire it then admitted.

Append caps the stored copies: 4 KiB each of check output, agent verdict reason and agent error, 2 KiB each of run error text and last message, and a signal payload over 4 KiB collapses to a single `_truncated` field. The verdict reader also tails reasons to 4 KiB at a UTF-8 boundary before decline memory, assists, or fired prompts consume them. `watch`, `output_path`, `throttle_wait_ms`, and `check.agent` are `#[serde(default)]`, so old rows read them as `None`; shell rows still omit `agent` when serialized. A renderer that finds `watch` set uses `WatchVerdict::label` for the outcome words and `elapsed_ms()` for the duration; shell rows render the `exit <n>`, `timeout`, or `signal` segments. Agent checks render `check by <profile>: pass|fail|no verdict (<error>)`, reason, cost and tokens. A skipped negative agent verdict reads `check declined`; a positive verdict reads `check passed` even when skipped. A firing negative verdict reads `check failed`, and no verdict names its error rather than a signal. `rimz loop show` reads the log for a health verdict plus a separate rollup of agent runs for check-gated work, and `rimz loop logs` prints the stored forensics in full, including the output path above the check gutter.

`LoopRunResult` has twenty variants, and `strikes::classify` sorts each record into one of three outcomes. A record whose `watch` verdict is nonterminal (a `Running` check-in) is neutral before the result is read. `CheckSkipped` and `SignalSkipped` both render as `skipped` and serialize distinctly (`check_skipped`, `signal_skipped`): one is a guard that declined, the other a sibling signal the subscription observed without delivering.

| Outcome | Results |
| --- | --- |
| Strike | `failed`, `verify failed`, `timed out`, `error`, `budget exceeded`; `completed`, `delivered` or `launched` whose check did not pass |
| Reset | `completed`, `delivered` or `launched` with a passing or absent check; `skipped` from a check that passed |
| Neutral | `budget skipped`, `surplus skipped`, `account skipped`, `throttle skipped`, `takeover blocked`, `overlapped`, `canceled`, `expired`, `target gone`, `start failed`; `skipped` from a failed or absent check; `skipped` from a sibling signal; any nonterminal watch check-in |

An agent check without a verdict counts as an absent check for strikes. Provider outages and checker timeouts therefore cannot disable a successfully launched task; a valid negative verdict keeps the existing red-check strike rule.

The table encodes three judgements. A turn that completed but left its check red is a failure, because the task is not doing its job. A gate that declined to spend money is not a failure at all. And a fire that left no run behind says nothing about the task, so `start failed` is neutral even though every renderer shows it as a failure: a broken timer must not auto-disable the work it failed to run.

`record_transition` appends the history row and then updates the overlays. It and `record_stopped` are the only writers to the log: a fire and an observed sibling signal reach `record_transition`, and `rimz loop stop`, whose task row may already be consumed, reaches `record_stopped`, which appends the strike-neutral `canceled` row stamped with the stop's root and moves no overlay. No CLI code appends a row. The append is best-effort, since `disk::rotating` logs its own failure at debug and returns, so the `RunTransition::Recorded` it returns means the append was attempted before the overlays moved, not that the row reached disk.

When consecutive strikes reach the threshold (`max-strikes`, default 3, `0` disables), the task auto-disables, displays `disabled · N strikes`, and fires `loop_disabled` notification handlers. `rimz loop enable` clears the counter and re-arms. `rimz loop fire` still works on a disabled or paused task, for testing.

Two machine-local overlays under `~/.rimz/loops/` hold this state without editing task definitions, each behind an advisory lock that serializes concurrent runners:

| File | Holds |
| --- | --- |
| `loop-arming.json` | enablement, a bounded pause deadline, and an automatic-disable strike reason |
| `loop-strikes.json` | consecutive failure counts, independent of run-log rotation |

Both key a task by scope: project and instance tasks as `<workspace_id>::<name>`, machine tasks as `machine::<name>`, so a same-named task in another checkout never inherits an enable. Enabling writes the anti-replay edge, and when a timed pause expires its deadline becomes the effective last-fire edge. Either way the schedule waits for its next occurrence instead of replaying what it missed while held.

## The signal vocabulary

A signal is a name, a JSON object payload, a source, and, for watched commands, an outcome. The first three persist, and [`store/event.rs`](../../../crates/rimz/src/store/event.rs) owns them as `SignalName`, `SignalSource`, and `SignalEventPayload`. The outcome does not persist, so the runtime `Signal` and its `WatchOutcome` stay in the harness and convert down through `From<&Signal> for SignalEventPayload`.

`SignalName` is lowercase dot-separated segments, each starting with a lowercase letter or digit and otherwise `[a-z0-9_-]`, at most 64 bytes. The family is the first segment. `rimz events emit --source cli` refuses the `RESERVED_FAMILIES` (`agent`, `wait`, `team`, `ci`, `pr`, `trunk`, `worktree`). The hidden `--source forge` accepts exactly the nine forge names, and `--source git` accepts only `trunk.moved`; neither authenticates the caller.

| Source | Producer | Names |
| --- | --- | --- |
| `Cli` | `rimz events emit <name> --json '{…}'` | anything the grammar accepts outside the reserved families |
| `Forge` | the sidebar's PR-state refresh, which spawns `rimz events emit --source forge` on a transition ([state.md § Push channels](../sidebar/state.md#push-channels)) | `ci.passed`, `ci.failed`, `pr.opened`, `pr.merged`, `pr.closed`, `pr.behind`, `pr.conflicted`, `pr.queued`, `pr.dequeued` |
| `Git` | the sidebar's project-root trunk lane | `trunk.moved` |
| `Worktree` | successful managed worktree operations, appended and fired in-process | `worktree.created`, `worktree.removed` |
| `Lifecycle` | the elected hook drainer, from the lifecycle events it applies | `agent.started`, `agent.idle`, `agent.waiting`, `agent.failed`, `agent.ended`; `team.idle`, `team.waiting`, `team.failed`, `team.ended` |
| `Team` | `rimz teams flip` and the stage owner's [resume re-wake](./teams.md#resume-re-wake) | `team.stage` |
| `Watch` | `rimz wait watch <name>` at a check-in or terminal command, PID, check, or file outcome, and the host's watch-lost rule | `wait.<task-name>` |

Hook-sourced signals normally fire when `crates/rimz/src/harness/hook_drain.rs::Drainer::drain` applies the ingress frame through its child, not in the hook process. Selection uses the subscriptions armed at that apply instant. A sandbox hook whose host drainer is unreachable applies inline instead. Crash recovery replays keyed durable effects but does not fire lifecycle or team signals again: a fire lost between the lifecycle append and the applied cursor write is not recovered. `rimz events emit` still fires in its own process, with no queue or replay.

GitHub-only `pr.behind` and `pr.conflicted` fire for open PRs on entry (including first sight), or for a changed head while the state persists, without repeating for unchanged heads. Conflict keys use the head where mergeability settled. Their payload is the `pr.merged` context with `state: "open"`, plus `pr_head`, `base` when known, and `behind_by` for `pr.behind` only; `head` stays local. The [PR producer](../sidebar/state.md#push-channels) owns detection and carry-forward. Both names inherit PR-family scoping and `branch`/`path` matching; a matching `pr.merged` subscriber records `SignalSkipped` for either sibling, while a failed match records nothing.

GitHub-only `pr.queued` and `pr.dequeued` follow the same rule with a different key: the queue entry's `enqueuedAt`, or the `createdAt` of the latest removal whose reason is not `merged`. They fire on a first reading over a continuous target and on a changed timestamp, never for a changed head; a room that opens on a standing queue state has no prior target stamp, emits nothing, and reads it through `pr.queue`, and a queued PR that merges emits `pr.merged` alone. Their payload is the `pr.merged` context with `state: "open"`, `pr_head` (always the PR head), and `base` when known; `pr.queued` adds `queued_at`, and `pr.dequeued` adds `dequeued_at`, `reason` when supplied, and `queue_checks_url` when the removal names the queue's commit. `ForgeSignal::ALL` alone admits a `--source forge` name, so a variant missing from it is emitted by the poll and refused by the child.

### Trunk and worktree signals

The sidebar trunk lane observes only the room's project-root checkout, using the configured trunk, then `main`, `master`, and `origin/HEAD`. Its five-second runtime cache is published under a dedicated single-flight lock before emitting `trunk.moved` through the detached Git-source emitter. Only a changed SHA on the same resolved trunk name fires; first sight, renamed trunk selection, and unresolvable refs reset the baseline silently. The payload is `trunk`, `from`, `to`, and `repo`.

Worktree creation emits after the Created hook succeeds, never on reuse. Removal emits at retirement, including sweep, not on failed-creation rollback. Both append before firing through the shared in-process helper, with failures warning without failing the completed operation. Payloads carry `name`, `branch`, `path`, and `repo`; both include `from_pr` when present, creation adds `base` when known, and removal adds `branch_deleted`. Neither family receives an implicit subscription match; `pr.opened` retains the PR family's existing path scope.

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

`team_lifecycle_signals` derives cohort edges from the same event, right after the `agent.*` derivation and only when the transitioning agent has a `team`. It is pure. The hook calls `team_stage::react_to_lifecycle`, which passes the member row from the audit projection (which retains ended rows), the live cohort from `team_cohorts`, the complete pending message queue, and the set of sessions holding a pending one-shot wait.

| Name | Edge |
| --- | --- |
| `team.waiting` | the member entered `Waiting` from a non-waiting status |
| `team.failed` | the member's turn ended errored |
| `team.idle` | the live cohort is non-empty, every member is at rest (`Idle` or `Success`), and no live member has a queued message or a pending one-shot wait; emitted only on the false-to-true edge, where the prior view overlays `prior_status` on the member |
| `team.ended` | a terminal event for the member, with no other live member left |

The payload carries `team`, `instance` (`team#channel`), `member` (the qualified handle that tripped it), and `members` with each handle and status. Membership is whatever `team_cohorts` counts as live for that `team#channel`, and a provider-native subagent's transition derives nothing.

`team.idle` is evaluated only on lifecycle events. Removing a pending wait, which only its owner can do, emits nothing by itself; the owner's next turn boundary re-evaluates the cohort, and with no further lifecycle event the evaluation waits for the next member event.

A member the reaper stops (a pane closed with no lifecycle hook, [`store/writer/reap.rs`](../../../crates/rimz/src/store/writer/reap.rs)) passes through no hook, so it derives neither `agent.ended` nor `team.ended`. Its subscriptions are still retired, by the reconcile in [Waits](#waits) rather than by a signal: cleanup follows the durable end, and never broadens what an end emits.

`team.stage` comes from `harness::team_stage`, not the cohort derivation, and `rimz events emit` cannot produce it. Its payload carries `team`, `instance`, `from` (absent when the first flip opens the board), `to`, `owner` (absent for `Done`), `by` (a role name, `user`, or `rimz`), optional `note`, `board` (absolute path), and `at` (RFC 3339). A flip writes the board and ledger before appending the signal with `SignalSource::Team`. A re-wake appends the same signal for the current owner with `from == to` and `by = "rimz"`, without editing the board or compacting. The exec wrapper produces it on every resume of a root member (resume, restart, single-member restart, and rebirth), and root-member registration is the backstop for a start that is not a resume; [teams.md § Resume re-wake](./teams.md#resume-re-wake) owns the two triggers and their de-duplication.

The stage owner does not learn of a flip through a subscription. `team_stage` dispatches a `Type: STAGE`, `From: @rimz` message straight to the owner at the `done` boundary: prose naming the flip and a `Note:` line, or for a re-wake, prose telling the owner to continue from the board's last Progress line. When the team declares a leader and the owner is not it, the body ends with the seat's channel rule (report to the stage file, reach the user through the leader, no pane text). `Done` and a self-owned flip send no message, and an owner who is not live is woken by the re-wake when it is resumed. Explicit `team.stage` subscribers still fire independently with the loop's signal body, re-wakes included.

### What persists

`rimz events emit`, the wait watcher, stage flips and re-wakes, and the hook's team derivation append a `signal.emit` event through the ordinary store commit ([store.md](../store.md#what-is-in-it)), so `rimz events follow` replays them. `agent.*` signals append nothing extra, because the `agent.lifecycle` record they derive from is already the durable trace. The durable record is the `SignalEventPayload` (name, payload, source), so a watched command's exit status travels in the fire's argv and never reaches the log.

### Firing subscriptions

`fire_signal` delivers a signal to its subscribers, in the emitting process:

1. Load the runnable tasks for the project root, drop untrusted project rows, and keep tasks whose resolved root maps to this workspace.
2. `resolve` each task's trigger against the signal, dropping `Ignore` results.
3. Drop any task whose arming overlay is not `Live`, so a disabled or paused subscription stays quiet.
4. On `Skip`, append a `SignalSkipped` run record carrying the observed signal and spawn nothing. On `Deliver`, spawn a detached `rimz loop run <name> --signal-json <encoded>` and return the name for the emitter to print.

`fire_signal` never touches an instance row, so a sibling observation leaves the subscription armed. The runner consumes a one-shot once its delivery is dispatched, and a standing subscription stays.

Nothing queues a match. Signal firing leaves `lanes/loop-fire.json` untouched, and a subscription written one second after the emit misses it. The host may stamp a signal row when it first sees the catalog, but `fire_signal` never consults that stamp. A signal reaches only the subscriptions armed in that workspace at that instant, which is what lets an emitter run with no room open.

### The watch verdict

A watch signal carries a `WatchOutcome`: a `WatchVerdict` plus its evidence (the output tail, the agent-visible path of the output file, and its byte size, physical line count, and estimated tokens in `summary`). The verdict is one enum with one renderer; a file pattern match also carries its matched line.

| Variant | `label()` |
| --- | --- |
| `Exited { code: Some(0), elapsed_ms }` | `exit 0 after 4m` |
| `Exited { code: None, elapsed_ms }` | `killed by signal after 3s` |
| `Running { elapsed_ms }` | `still running after 30m` |
| `Met { elapsed_ms, line }` (`met`) | `met after 4m`, adding ``: `<line>` `` when a file pattern matched |
| `NotMet { elapsed_ms }` (`not_met`) | `still not met after 30m` |
| `TimedOut { elapsed_ms }` (read from old records only) | `timed out after 59m` |
| `Lost { detail, elapsed_ms }` | `watcher died after 3m; the command may still be running or may have died with it` |

`WatchVerdict::label` is the only place those words are written; `compose_wait`, `rimz loop logs`, and `rimz loop show` all render through it. `passed()` is true for `Exited { code: Some(0) }` and `Met`. `elapsed_ms()` is the measured run, rendered in seconds under a minute and through `theme::fmt::duration_label` above it. `is_terminal()` is false for `Running` and `NotMet`, the check-ins that bypass polarity, add no strike, and do not consume the row.

For terminal outcomes, `to_check_outcome` folds the verdict into the check machinery: `passed()` becomes the pass bit, `TimedOut` the timeout flag, and the tail the output. A `Lost` outcome's output is the log file's tail, since the label already states the cause.

`WatchOutcome` travels only in the `rimz loop run` argv and the process memory around it, so reshaping it is not a durable-format change. The verdict becomes durable one level up, in the run record's `watch` field.

## Watched commands

`TaskEntry.watch` is an untagged `WatchSpec`: a command string, `{pid}`, `{check, every, on}`, or `{file, grep?, mark?}`. A file mark records size, modification time, device, and inode; no mark means absent at arm time. `WatchSpec::describe` owns trigger text and `headline` owns the delivered first line.

`arm_delivery` stores the arming agent's handle as `WaitMeta.reader`, creates `out/<reader>/<name>.output` with its directory, and spawns a detached `rimz wait watch <name>` with null stdin and stdout, the output file as stderr, and `process_group(0)`, through `child_process::spawn_detached_reaped_as_agent`: the watcher runs the agent's own command, so it keeps the agent's environment, temp unit included, where every other detached child gets the user's `TMPDIR` back. A spawn failure rolls the row back.

`signal::run_watcher` takes `loop-watch-<name>.lock` in the workspace runtime directory before it reloads and checks the catalog row, which closes the race with a cancel that lands before the watcher starts. The lock holds `{pid, started_at}`, and a second watcher for the same task exits without running the command.

The command runs in the task's `dir`, else the linked worktree it was armed from, else the project root. The watcher drains stdout and stderr into the output file and keeps a 4 KiB tail. At arm time, `cli/wait/add.rs` resolves an explicit timeout first, then no timeout when keepalive is enabled and the caller has a provider TTL, otherwise 30 minutes, independent of `loop.default-timeout`. `DeliveryTrigger::Watch.timeout` is optional and only a present value is persisted. The watcher runs to exit with an optional one-time check-in; polled watch probes omit that check-in. Existing persisted timeouts stay effective. Ordinary `check` and `verify` still kill at their timeout. At the deadline an observed exit wins. Otherwise the watcher fires one `Running` outcome, with the tail and the output file measured so far, and keeps draining until the command exits. The check-in does not kill the command, filter on `on`, consume the row, or change strikes. In `compose_wait`, its verdict gains ` · still watching` before any output segment; `WatchVerdict::label` and loop logs keep their existing wording. Its body includes `Stop it: rimz wait cancel <name>` and, when a timeout is recorded, `Another check-in: rimz wait --in <timeout>`, a timer independent of the running command.

A watcher-originated fire runs `rimz loop run <name> --signal-json …` and waits for it while still holding the watcher lock, so the check-in delivery cannot overlap the terminal fire. An append or fire failure is logged and the final outcome is still attempted. Every other signal emitter spawns its runs detached.

For PID, check, and file specs, `signal::poll_watch` probes then sleeps: the check's `every` interval, or one second for PID and file. PID uses native `kill(pid, 0)` rather than a shell loop: `ESRCH` meets the condition, while success or `EPERM` keeps watching. A check probe runs to exit without a check-in, truncating and rewriting the output file before every run so evidence is the latest run's output. Its spec's `on` chooses success or failure; exit 126/127 emits `Exited` and ends the wait. Otherwise polled specs emit `Met` on completion and one `NotMet` check-in evaluated between probes; a blocking check defers that check-in.

A file watch compares existence, size, modification time, and identity (`dev`, `ino`) against its arm mark. With `grep`, its `GrepCursor` holds a byte offset and the identity it was read on, starting at the arm mark (zero and no identity if absent); it restarts at zero when the identity changes or the file is shorter than the offset (a same-inode truncate that regrows past the offset within one poll is not detected), and tests only newline-terminated lines using literal case-sensitive matching. It trims a trailing carriage return; a straddling line starts at the arm byte. The whole matched line is written to the output file; `Met.line` is capped at 4 KiB, and the label renders a one-line preview. Relative paths are resolved against the arming cwd and stored absolute. The [file reference](../../reference/cli/wait.md#file---file) owns the user-facing contract.

Cancel removes the row first, then `stop_watcher` sends SIGTERM to the lock holder's process group, stopping the watcher and its command together. Non-positive PIDs are rejected, and an absent process counts as stopped. If a watcher dies without firing, the host's watch-lost rule fires the `Lost` verdict after the 30-second grace, with the output file's tail as evidence.

`signal::wait_output_path` derives `StatePaths::out_reader_dir(wait_meta.reader)/<name>.output` (`out/_unnamed/` for a row with no reader) for arming, watching, and lost-watcher evidence. `WatchOutcome::measured` records the file's byte size, line count, and estimated tokens (`utils::tokens::estimate` over at most the first 1 MiB, scaled by length beyond it), and records the host path as `output_path` in every isolation. A failed measurement warns and records a zero summary, which renders as `· no output` for command watches and no output segment for polled watches, like an empty file. Teardown keeps the file; gc prunes it only when there is no running watcher and no write in the seven-day grace. The run record keeps the tail for `rimz loop logs`.

## Waits

A wait is a session-pinned delivery row: a `Deliver` task whose target is one live agent. Every delivery row is an instance row, and every one is built by `schedule::arm::arm_delivery`, which both `rimz wait` and `rimz loop add --wait` call.

`DeliverySpec` carries a typed trigger, name, prompt, target, and provenance. The provenance decides what the builder accepts:

| Provenance | Accepts | Name |
| --- | --- | --- |
| `SelfWait` | a delay or a watch spec; no prompt, separate check guard, surplus gate, or deadline | minted, workspace-unique `wait-<petname>` |
| `Loop` | a named clock or signal delivery | given |
| `Team(instance)` | a standing signal subscription | generated ([Team bindings](#team-bindings)) |

`SelfWait` is the only provenance that writes `wait_meta`: `armed_at` and the optional `delay`. A name that belongs to a project task, or for a team row to a machine task, is refused as configuration-owned.

`rimz wait` accepts a positive delay under 24 hours, a positive `--pid`, a polled `--check`, a `--file` with optional `--grep`, or `--run`, and resolves the live calling agent through `@me`; a user shell cannot arm or cancel. `DeliveryTrigger::Watch` carries the spec, row polarity, and check-in timeout. PID watches observe existence, not process identity or exit status, and reject `--on`.

For signal deliveries, the builder applies caller-first, target-fallback defaults and the other-agent lifecycle guard ([cli/loop.md § Caller-scoped defaults](../../reference/cli/loop.md#caller-scoped-defaults)). One locked instance mutation then compares live rows by kind and session, parsed selector, normalized matches (absent equals empty), and resolved root. A duplicate returns `AlreadySubscribed` and rewrites nothing: not its name, prompt, provenance, or overlays.

Arming and canceling print the caller's pending rows after the receipt. A list from an agent includes loop and team subscriptions for its pinned session; from a user shell it is room-wide and read-only. Cancel requires a caller and takes a name or `--all`. Command previews use `theme::fmt::command_preview`: at most 120 Unicode scalars, keeping the first 60 and last 59 around an ellipsis, with stored commands and logs unchanged.

**A session-pinned instance row never outlives its target's durable end.** Retirement follows that durable fact, not any one producer of it: the lifecycle hook, the exec wrapper's exit stamp, the store reaper, worktree removal, rebirth's unrecovered stamp, and orphan-subagent reclaim all end a session, and only the first is a hook. A new producer needs no retirement code of its own.

`schedule::arm::retire_session` removes the instance rows pinned to a kind and session, paused and disabled rows included, stops their watcher groups, and clears their arming and strike overlays. A `RetireScope` chooses which rows: `Session` takes all of them, for an identity that will not come back, and `UnrestorableOnly` leaves the team-declared rows standing, for a session that continues under the same id. It returns the number of removed rows nothing arms again — self waits and `loop add --wait` rows, never a declared binding — and that count survives a cleanup failure, because overlay cleanup warns rather than failing; an `Err` means the durable map itself could not be rewritten and nothing was removed. Retirement never signals the calling process: `stop_watcher` skips a lock whose recorded pid is its own, so a watcher that retires its own row mid-fire finishes that fire instead of killing its process group.

Lifecycle `Ended` and `Lost` call it with `Session` after the durable event and before event signals (`Lost` sets no `ended_at`, so that call stays load-bearing); an explicit agent-tree stop calls it after each successful node stop; the exec wrapper calls it for the session it stamps ended, in the same exit tail. `agents restart` calls it once the replaced pane is closed, and picks the scope from the branch it took: a fresh restart mints a new identity and retires the old one whole, while a resumed restart is the same session continuing and takes `UnrestorableOnly`, so the role's declared bindings carry over rather than depending on a re-arm nothing orders against the removal. The restart line reports the count when it is non-zero. A restart that fails before the pane closes retires nothing.

A wrapper whose session has moved on ends nothing. `resolve_own_agent_end_trace`'s argv-identity fallback applies only while the audit projection binds that session to no pane but the wrapper's own; bound elsewhere, the wrapper is a superseded one whose replacement has already attached, and it appends no `rimz.agent-ended` and retires nothing. The replacement's attach precedes its provider spawn, so any tail that reads after the replacement registered sees the new binding.

`schedule::arm::retire_ended_sessions` is the reconcile that covers every other producer, and the one place the predicate is spelled: an agent row that is not a provider subagent, matches a pinned kind and session, and carries `ended_at`. It acts on positive evidence only — a session a later lifecycle event revived has no `ended_at` and keeps its rows, a target with no agent row at all is left to gc, and a session the live roster protects for rebirth is never ended in the first place. It is pulled from two points: every hook observation whose commit appended an event, reading the audit projection after that commit, and `fire_signal_with_wait`, before it selects a single task. An observation that appended nothing — a side conversation's repeat hook, a read-only tool use — published nothing, so the session reaper never ran under it and no end appeared for the reconcile to find; it reads no projection and does no fold. The consumer is where the rule has to hold, because the sibling-skip branch returns before `delivery_target_alive` and a listener whose family mostly fires siblings would otherwise never meet a liveness gate. The workspace's instance rows are read first, so a workspace with no pinned row costs one file read and no store open.

`delivery_target_alive` rejects a session with `ended_at` set, and gc is the backstop for what the reconcile leaves: rows whose target has no agent row at all. An instance row is runnable or it is garbage: gc also reaps any `Instance` row whose action no longer compiles (a target lost to schema drift can never fire or retire), while machine and project `loop.toml` rows with an invalid action stay listed as `<invalid>` for the user to fix. Hook config, arming, reconcile, and retirement failures are logged as warnings and never fail a fire, hook, exit, or restart, and never reach hook stdout.

`pending::project_pending_waits` projects armed one-shot delivery rows onto their target agents as `pending_waits`, which the sidebar and `rimz agents list` read. Timers, PID waits, watched commands, polled checks, file watches, and one-shot signal deliveries count; standing subscriptions and recurring clocks do not. `WatchSpec` maps directly to pending `command`, `pid`, `check` (`command`), or `file` (`path`, `grep`) kinds; no command string is parsed. What a pending wait does to the displayed status is [model.md § Sleeping](../agents/model.md#sleeping), and how a card draws it is [the interface reference](../../interface/sidebar.md). A live member's pending one-shot wait also withholds `team.idle` ([Team signals](#team-signals)).

Pending waits also feed `harness::owed::owed_wake` through `TurnWaitView::load`, holding the supervised run fold open at a clean end. Reading the catalog before the queue covers the handoff from an armed row to its wake message; consuming a row alone does not imply the run completed.

### Team bindings

A team role declares standing subscriptions for its own seat. Each `[[agents.teams.<name>.roles]]` entry has a `signals` array of inline tables with `signal`, an optional string-valued `match`, and an optional `prompt`; the containing role is the receiver. `prepare_team` validates the selectors and explicit other-agent matches for `agent.*`. Each role's complete ordered binding list enters the executable trust hash, and an empty list leaves the hash unchanged, so moving bindings between roles needs a fresh grant.

A fresh `launch_layout` refuses a team binding scoped implicitly to the root checkout's CI or PR state, before any pane or worktree side effect, and names the fixes: an explicit branch or path match, `branch = "*"` for every checkout RimZ watches, or `-w <worktree>`. A launch from a linked worktree passes, a `--from-pr` launch skips the check, and resume does not apply it. `team = "*"` or `instance = "*"` disables the cohort default; on `team.*` this includes the four lifecycle signals as well as stage flips. Neither `handle = "*"` nor `session = "*"` satisfies the explicit other-agent requirement for `agent.*`.

After a committed lifecycle `Registered`, the hook calls `team_stage::react_to_lifecycle`, which reads the strict effective trusted config and calls `schedule::team::arm_member` for a root team member, never a subagent. It uses the member's adopted session identity, role, channel, and worktree, so launch, resume, restart, and role re-add share one registration path. All binding specs are built before anything persists. The rows carry `TaskEntry.team = "<team>#<channel>"`, target the member's exact kind and session, and deliver at the next `done` boundary.

Row names are `team-<team>-<channel>-<role>-<signal slug>`, lowercased, with dots and any character outside `[a-z0-9_-]` replaced by `-`. Repeated or colliding slugs within a role get a declaration-order `-<n>` suffix; a collision between lanes in one room is accepted. Repeated registration deduplicates, and an already-subscribed manual row is not relabeled as a team row. Automatic arming never replaces machine or project configuration: a generated name that collides with a configured task is reported in hook diagnostics, and the user renames that task before the member re-arms.

## Recovery the host runs

The host's tick also drives unattended recovery that is not a loop task. Each intervention is documented where its inputs live, and each records itself in the [assist log](#the-assist-log).

| Automation | What it does | Owner |
| --- | --- | --- |
| Auto-continue ([`auto_continue.rs`](../../../crates/rimz/src/harness/auto_continue.rs)) | resumes an agent parked on a certified rate limit, an overload, or a RimZ budget park, once that park's clock is due | [providers.md § Auto-continue](../agents/providers.md#auto-continue) |
| Auto-redeem ([`auto_redeem.rs`](../../../crates/rimz/src/harness/auto_redeem.rs)) | spends a Codex reset credit when it buys capacity | [providers.md § Auto-redeem](../agents/providers.md#auto-redeem) |
| Idle compaction ([`idle_compact.rs`](../../../crates/rimz/src/harness/idle_compact.rs)) | compacts an idle team member before prompt-cache expiry, or at an explicit idle threshold, while its board is not `Done` | [messaging.md § Idle compaction](./messaging.md#idle-compaction) |
| Idle stop ([`idle_stop.rs`](../../../crates/rimz/src/harness/idle_stop.rs)) | stops an agent with a pending `rimz agents stop --when-idle` request ([`store/idle_stop.rs`](../../../crates/rimz/src/store/idle_stop.rs)) once it has rested for the requested duration with nothing owed. The producer checks the rollup terms and the clock, then spawns `agents idle-stop` at most every 30 seconds per session; the helper re-runs `idle_stop::decide` on fresh reads (owed wakes, queued messages, open runs, the seat's board), applies the rest and debt terms to every live launched descendant the stop cascades to (`closing_with`; the clock is the root's alone), declines when the run the stop path would select for any of them is not terminal, re-reads the request, and stops through the ordinary stop path, whose session retirement removes the request. Rest is measured from the latest of the request, the turn end, the provider marker `settled_outcome` admits, and the session's last event, which covers a turn only a marker settled and a failed compaction; the clock keeps sub-second precision, so a stop never lands before the full duration. Provider-native subagents are not a term: a native row leaves `Running` only on its Stop hook, a superseding parent turn, or the three-hour ghost reap. A room reset removes every request inside its commit boundary. A helper that declines writes nothing, so the request stays armed | [agents.md § `stop`](../../reference/cli/agents.md#stop) |
| The budget park | interrupts an agent over a dollar cap and arms its day reset | [budget.md § The park](./budget.md#the-park) |
| Auto-gc ([`auto_gc.rs`](../../../crates/rimz/src/harness/auto_gc.rs)) | runs `rimz gc --unattended` once per 24 hours per workspace, after 5 minutes of producer uptime, paced by the durable room stamp and a 10-minute respawn throttle; the helper sweeps its room unless it wins the daily machine election, which also covers closed rooms; `gc.auto = false` skips it | [maintenance.md § Automatic sweeps](../../reference/cli/maintenance.md#automatic-sweeps), [store.md § Maintenance](../store.md#maintenance) |

Each decides on the producer tick and acts through a detached helper, which keeps store-writing code out of the sidebar's import graph ([sidebar.md](../sidebar/sidebar.md)).

## Prompt-cache keepalive

`harness/cache_keepalive.rs` runs beside idle compaction on the heavy refresh lane. A Sleeping root agent with a live pane, a completed request, and a configured provider TTL qualifies during `[TTL - 60s, TTL)`, measured from `AgentState::last_request_at`. No team or context-size gate applies; timer waits qualify too. Budget parks, compaction, and awaiting input exclude it. `harness.cache_keepalive = false` disables it.

Keep-warm widens the same reflex to an agent with nothing pending. `KeepWarmPolicy` resolves the horizon from the trusted effective definitions: a team seat reads its role binding's `keep-warm`, a solo seat its profile's, and the horizon is admitted only when the provider's TTL is known and at least `harness.keep_warm_min_ttl` (default 15 minutes; `off` admits any known TTL). A solo seat's TOML profile with no `keep-warm` of its own takes the nearest one up its `agent` base chain, and an explicit `off` stops the walk; Markdown definitions arrive with inheritance resolved. A `rimz subagents` child (`launch_depth` set) never resolves a horizon. The horizon is anchored at `AgentState::turn_ended_at`, which a ping never moves: a ping-only turn is not agent activity (see [the model](../agents/model.md#edges)), so its open and close leave status, phase, activity, turn stamps, turn ids, team signals, and the supervised run as they were, and stamp only `pinged_at`. The lifecycle hook skips its activity heartbeat, active time, and run settle on those edges too. One predicate, `cache_keepalive::holds`, says whether keep-warm is holding an agent: `now < turn_ended_at + horizon`, the cache still warm (`now < last_request_at + TTL`, so a missed window is dropped, never pinged cold), no `--when-idle` stop pending, `harness.cache_keepalive` on, the TTL still admitted under the current floor, effective status `idle`, `success`, or `sleeping` that also rests in the durable lifecycle (`AgentState::rests_in_lifecycle`: raw `idle`/`success`, or a clean end parked on background work; a `running` row only a provider settle marker rests is not held, because the event fold cannot see the sidecar and would fold its ping as work), a root agent that is not compacting, budget-parked, or awaiting input, for a team seat a live cohort (a board at `Done` ends the hold), and `harness.cache_keepalive_max` still permitting another ping (below). A hold is therefore a horizon the pings can still maintain, never a merely configured one. A held agent qualifies for a ping in the same `[TTL - 60s, TTL)` window; the Sleeping path above is unchanged and needs no horizon. Idle compaction asks the same predicate on both the producer pass and the helper recheck and skips a held agent, so the two reflexes never race on one cache window; an agent that is not held, whatever horizon it declares, compacts on its normal window.

The producer publishes the horizons it admits to `lanes/keep-warm.json` each heavy pass ([state.md](../sidebar/state.md)). Enrichment applies `holds` to each published horizon on every fold, over the agent state and harness config it already has, and stamps `AgentCard.cache`; a renderer learns the hold without resolving definitions, and a status or setting change ends it without waiting for the heavy lane.

Enrichment prepends `Subagent` waits from `FleetRuns`: each launched child or peer run that is non-terminal or still `owes_report()` contributes one wait. Open team runs owned by `harness/run/team.rs` contribute `Team` waits to their launcher, after subagents and before catalog waits, unless the cohort's board already reads `Done` (a hand-edited `Done` leaves the run open with no report coming). Both sources make a resting launcher eligible without changing the catalog-only completion readers. An open team run also holds its launcher's clean end parked through `OwedWake::Team`, with the same Done-board skip. A cohort gone before Done is settled by the digest backstop or the parked wrapper with `cohort ended before Done`, reported as a failed `TEAM_REPORT`, and the report's turn unparks the launcher; a board already at Done settles completed instead.

The producer takes a runtime advisory lock and records the anchor before spawning the hidden `agents cache-keepalive` helper. The sidebar and one-shot refresh paths therefore share one claim per session and anchor, even when they race. A failed spawn consumes that claim. The helper rechecks the anchor, eligibility, and pane on `Ctx::published_snapshot`, including pending waits and the activity heartbeat. It sends a `CACHE_KEEPALIVE` notice beginning `Cache keepalive, no action needed. Waiting on:` with one facts-only line per wait and no instructions; a keep-warm ping with no waits is the bare `Cache keepalive, no action needed.` Subagent lines carry elapsed age, last activity and any deadline, or terminal status with `reporting`; teams carry elapsed age and board stage; catalog waits carry their trigger or command and elapsed age when known. Durations are floored to whole seconds, minutes, hours, or days. Delivery uses the `Done` gate; a miss is finalized as `Errored`, never left for a later boundary. Every attempted delivery appends a `CacheKeepalive` assist, including failures; the helper rechecks the horizon with the rest of eligibility and records it as `horizon_secs` when keep-warm was holding. A ping turn advances the anchor, allowing another ping one window later, up to `harness.cache_keepalive_max` (default 6h, `off` for none).

The cap is global over any run of pings, Sleeping or keep-warm alike; a horizon never outlives it. Its clock is `AgentState::keepalive_since`, which the fold stamps at a ping-only turn's open, from the prior `keepalive_since` or else the prior `last_request_at`: the agent's last request before the run of pings. That includes a ping to a row parked on background work, since its open is the same inert edge. Any other turn start (a human prompt, an agent message, a `STAGE` or `WAIT` wake, a batch mixing a keepalive with anything else), including one that resumes a parked row, and any context reset clear it, so only an unbroken run of pings counts. The run's last ping is `pinged_at`, the one stamp a ping moves, which `last_request_at` then returns. `run_reached_cap` says the next ping would start too late when `last_request_at - keepalive_since + (TTL - 60s) >= max`, reading stamps alone so the producer and the helper's recheck agree; with no stamp, the first ping of a sleep is always allowed, so a `max` shorter than the TTL still yields one ping per sleep. `should_keepalive` refuses that ping, and `holds` returns no hold once it says so, whatever the horizon: after the final ping the card shows the TTL ramp from that ping, and idle compaction treats the agent as not held. The helper marks a ping final when `(now - since) + (TTL - 60s) >= max`, with `since` falling back to `last_request_at`, appends `Keepalive limit <max> reached: this is the last ping until your next turn.`, and records `capped` on its assist; the notice rides the ping prompt alone. The two rules use different clocks for the last ping's start: when `now` sits just under the threshold and the ping's recorded close lands just over it (seconds of send-to-submit latency), the run ends without the final line.

This is best-effort cache retention, not durable correctness. The activity heartbeat is a latency hint. A slow refresh, no sidebar host, or early provider eviction can cost one cache write, but cannot lose a wait or its eventual wake. A window missed entirely is skipped for good: `should_keepalive` keeps its `< TTL` bound, so no late ping fires and pings for that sleep end until the agent's next turn moves the anchor. With a known TTL and no `--timeout`, a wait on a command that never exits stays silent; an explicit `--timeout` restores the check-in. The shared one-minute margin covers refresh, helper spawn, and submission, not an end-of-response clock.

## The assist log

> **Automation is accountable.** User-benefiting automation appends a durable record of its trigger, evidence, and outcome, and surfaces in `rimz stats`; internal repairs keep durable diagnostic records ([diagnostics.md](../diagnostics.md)).

The assist log is that invariant's record: `~/.rimz/logs/assists.log.jsonl`, account-global, best-effort append, rotating at 4 MiB to one `assists.log.1.jsonl` predecessor. Readers fold both generations by timestamp. When an append fails, the intervention itself remains the operational truth.

The redeem record also carries user-initiated redemptions under reason `manual`, for the credit ledger; they are not counted as automation assists.

| `Assist` variant | Writer | Records |
| --- | --- | --- |
| `resident_launch` | the loop helper, after stop and launch succeed and the ledger is published | task, condition evidence, checkout, the handles taken over from if any, and launched handles; an append error fails the fire and keeps the ledger entry |
| `check_decline` | the loop helper, after any agent-check polarity decline | task, checkout, checker profile and kind, verdict reason, and optional checker cost; an append error fails the fire; resident decline memory remains published |
| `auto_redeem` | the detached redeem helper or `accounts redeem` | provider, account (`login`, absent on records older than the field), decision reason, request id, available credits, soonest expiry, the natural reset it beat, consume outcome or error, whether a reset occurred, and refreshed window stamps |
| `tier_fallback` | interactive and supervised launch, once the launch is placed; team lane restore after each tab opens, rebirth after birth succeeds for each tab confirmed open ([fleet.md](./fleet.md#every-path-that-builds-a-launch-layout) lists each path) | launched kind, agent id, optional label, profile, tier, model entry as configured (alias or full ID), and usage skips (`logged_out`, `exhausted` with optional reset, or `daily_cap` with spend and cap); one record per steered agent, none for explain or static exclusions |
| `auto_continue` | the detached continue helper | typed provider and session ids, display handle, park class, original park timestamp, delivery verdict, and the durable message id |
| `stall_notice` | the detached stall-notice helper, once the notice is queued | child provider and session, optional child handle, parent handle, silent seconds, durable message id, delivery verdict, and error when present ([backstops](./subagents.md#backstops)) |
| `auto_compact` | the message delivery path, after a compact command lands | target session, display handle, threshold, occupied context when known, and the durable compact-command message id |
| `idle_compact` | the detached idle-compaction helper | target provider and session, display handle, idle duration, resolved threshold (`idle_after_secs`, absent in old records), occupied context, durable compact-command message id, delivery verdict, and error when present |
| `cache_keepalive` | the detached cache-keepalive helper | target provider and session, display handle, idle duration, pending-wait count (including subagent and team waits), the keep-warm horizon in seconds when one was holding (`horizon_secs`, absent otherwise and in old records), message id, delivery verdict, error when present, and `capped` when the ping announced the keepalive maximum |
| `idle_stop` | the detached idle-stop helper, for every stop it attempts | target provider and session, display handle (`label`), rested seconds (`idle_secs`), requested threshold (`idle_after_secs`), requester handle when an agent asked (`requested_by`), whether the agent's own pane closed (`stopped`), and the stop's failures (`error`), which can accompany `stopped: true` when a child's pane or the retirement failed |
| `flip_compact` | `rimz teams flip` | flipper provider and session, role, previous and target stages, occupied context when known, effective threshold, the attempted compact-command message id (which may not resolve if a store error prevented publication), delivery verdict, and error when present |
| `auto_resume` | rebirth recovery, once at least one resumed tab is confirmed open | workspace, session, death cause, the pane count of the confirmed tabs, and their labels |
| `model_alias` | exec wrapper, when a catalog-derived alias target moves under the per-login lock | provider kind, login, alias, previous model, and new model; configured pins and baked fallbacks append none |
| `launch_retry` | exec wrapper, each time it answers a fresh launch's [startup death](./subagents.md#the-lifecycle-end-to-end) with another spawn | provider kind, the launch's handle (`label`), the run every attempt shares (`run_id`, absent for a launch without one), which relaunch this is (`attempt`, from 1; absent in older lines and read as 1), the exited process's `exit_code` (absent when a signal killed it), its spawn-to-exit `startup_ms`, whether the new spawn succeeded (`relaunched`), and the spawn error when it did not |
| `auto_gc` | `rimz gc --unattended`, after every attempt | workspace, `scope` (`room` or `machine`, legacy records default to `machine`), cutoff, own-room `class_bytes`, and scope-wide totals: reclaimed bytes, removed worktrees, pruned workspaces, removed runtime and temp files, archived messages, problem count, and error when the sweep failed |

`rimz stats` folds both generations, scoped to the active dashboard window, into one rollup: resident launches, check declines (`check_declines` in JSON), delivered continues and their summed recovered time (`recorded_at - parked_since`), compact commands sent, delivered keepalives, idle stops that went through, startup relaunches that spawned, redeem attempts and `reset` outcomes, rebirths with restored panes, and completed gc sweeps with their reclaimed bytes. The dashboard shows whichever categories are non-zero, `rimz stats --assists` renders the merged event stream newest first, and `rimz stats --json` publishes both.

A flip compaction reads `flip compaction` in the event stream and counts toward compact commands sent only when the command was sent; a flip completing does not count a queued command. Only a member leaving its own stage for one another role owns is eligible; `Done` has no owner and never compacts. The effective `[harness] flip_compact` or role `flip-compact` threshold is evaluated before pane availability: a flip below it, or with unknown occupancy, makes no attempt and writes no record. Every attempt records the threshold, including errors and missing-pane skips, and none of these failures fails the flip.

A new smart strategy ships as one accountable slice: define its typed trigger, evidence, and outcome record; append it from a writer outside the sidebar import graph; fold it into the stats rollup; and add its variant and writer to the table above.

## See also

- [scripting.md](./scripting.md): the supervised run every `agent` task spawns.
- [messaging.md](./messaging.md): the delivery path every `wait` task uses.
- [fleet.md](./fleet.md): launch compilation and addressing.
- [budget.md](./budget.md): the dollar scopes the gate ladder reads, and the budget park.
- [providers.md](../agents/providers.md): account windows, capacity readings, auto-continue, and auto-redeem.
- [sidebar/state.md](../sidebar/state.md#the-room-data-plane): what the shared host owns and what runs on its tick.
- [cli/loop.md](../../reference/cli/loop.md): every flag on `add`, `fire`, `list`, `show`, and `remove`.
- [cli/wait.md](../../reference/cli/wait.md) and [cli/events.md](../../reference/cli/events.md): the user-facing wait triggers and the signal emit surface.
