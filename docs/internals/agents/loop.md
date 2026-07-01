# Loop tasks

> See [DESIGN.md](../../../DESIGN.md) for the commitments this doc operationalizes. Loop tasks ride the supervised-run path in [harness.md](./harness.md#supervised-runs), the message path in [message.md](./message.md), and the budget-window model in [provider.md](./provider.md).

Loop tasks run scheduled work from the room's elected sidebar elder. While a room for the task's project is open, the elder's data tick evaluates configured tasks and fires `rimz loop run <name>`, which resolves the recorded project `root` and then uses the configured action: `spec` spawns one transient supervised pane, `bind` delivers a prompt to one living agent instance, and `check` runs a shell command that can either stand alone or guard an agent action.

In `spec` mode, each task names exactly one agent cell: a built-in kind, a profile, or an adapter-supported virtual cell such as `claude-auto`, `codex-yolo`, or `claude-ping`. Team specs, multi-cell layouts, and command cells are rejected at add time because a scheduled task owns one supervised pane.

## Schedule forms

Rimz stores durable recurring schedule definitions in per-machine `loop.toml`. `rimz loop add` validates the task, runs hook preflight when an agent action exists, and makes it live immediately while a room for the task's project is open.

- **Calendar:** `at = "07:00"` with optional `days = "weekdays"`, `daily`, `weekends`, `mon-fri`, or `mon,wed,fri`; wall-clock evaluation uses the configured `timezone`, falling back to the system zone when unset.
- **Interval:** `every = "15m"`, `2h`, or `1d`; the elder fires at the exact interval measured from the last arm or fire.
- **Raw cron:** `cron = "*/15 * * * *"` uses the in-process five-field matcher for minute, hour, day-of-month, month, and day-of-week in the configured `timezone`.
- **One-shot:** `once = true` with a calendar or cron schedule. `rimz loop add --in 30m` resolves to an `at` time in the configured `timezone` and implies `once`.
- **Poll-until:** `every = "2m"` with `check`, `on`, an agent action, and `deadline`; `rimz loop add --until 30m` stores the resolved absolute deadline in instance state.

Calendar tasks catch up once for today's matching time when a room opens later the same day. Cron tasks fire during matching open-room minutes, so a room opened later waits for the next matching minute.

Ephemeral tasks remove their state row immediately before the supervised run or message delivery. A one-shot removed pre-fire that then fails to launch is not retried. Poll-until tasks also remove themselves when the check fires the agent action, and expire without delivery when `deadline` passes.

## Elder firing

The elder keeps a per-room `loop-fire.json` map of task name to last-fire `Timestamp` under the workspace runtime dir. First sight arms a task by recording `now` and does not fire; the next matching occurrence fires. A fire records `now` before spawning the detached helper, which guards against duplicate pane spawns on sub-interval ticks.

Each room fires only tasks whose normalized `root` maps to its `WorkspaceId`. `rimz loop add` stores a canonical absolute root, and a hand-edited root may use `~` or a relative path; the elder and runner expand and canonicalize it before workspace ownership checks, display, and execution.

The elder spawns `rimz loop run <name>` with fresh null stdio. The hidden runner resolves the task's recorded root, runs any `check` first, applies agent preflight only when the guard fires, and then launches the supervised pane or messages the pinned session.

Self-paced loops use ordinary one-shots. The agent schedules its next wake with `--in <delay>` at the end of the current wake; the instance row is removed before delivery, and the agent creates the next one only when it still has work. This keeps the pending wake visible in `rimz loop list` without editing `loop.toml`.

## Script checks

`check = "<shell>"` runs through `sh -c` at the task's project root before any agent action. `on = "fail"` wakes on non-zero exit or timeout and is the default; `on = "success"` wakes on zero exit. `timeout = "5m"` bounds the check, falling back to five minutes when unset.

A check-only task is a scheduled command with no agent action. It logs `completed`, `failed`, or `timed out` in `loop-runs.log.jsonl` and keeps recurring unless it is ephemeral. A guarded task logs `skipped` when the command exits with the non-firing polarity; when the guard fires, Rimz appends the command, exit status, and capped combined output to the base prompt before spawning or delivering.

This covers the watchdog pattern (`every = "15m"`, `check = "cargo test"`, `on = "fail"`, `spec = "codex"`) and the trigger-when-green pattern (`every = "2m"`, `check = "gh run watch --exit-status"`, `on = "success"`, `bind = ...`, `deadline = ...`). A poll-until instance stops in two cases: the first matching check result fires the agent action, or the resolved `deadline` passes and the run logs `expired`.

Script checks are per-machine user automation, like a personal crontab. `loop.toml` lives outside the repository and outside the project trust hash; a clone cannot supply a check command. Project trust continues to hash only executable fields from `.rimz/config.toml`.

## Delivering to a living instance

Bind-mode pins a schedule to one exact agent session. `rimz loop add <name> --bind @<handle> --prompt "<text>" ...` resolves the address against the live rollup immediately, records a `bind` sub-table with `kind`, `session`, and `handle`, and rejects `spec` and supervised-run flags because delivery does not launch a new pane.

On fire, `loop run` resolves the recorded `root`, checks that the root agent session still exists, and sends the prompt through the same message path as `rimz message`. An idle agent receives the text immediately; a running agent parks the message for the next `done` turn boundary; a missing session is skipped and the schedule is removed because that exact conversation cannot return.

`rimz gc` repeats the same liveness check for bind-mode tasks and reaps schedules whose pinned session has left the rollup. This is a safety sweep for tasks that did not get a successful fire after the agent exited.

## Window-priming pings

A task whose `spec` is a `<kind>-ping` virtual cell starts a provider's budget window at a time you choose. The task defaults `prompt = "ping"` and uses lowest effort unless configured otherwise; the virtual cell supplies the adapter's ping arguments.

The window is account-scoped, shared by every session of a provider kind ([provider.md](./provider.md)), so one ping per provider primes the whole account. Before spawning the turn, `loop run` reads the shared rate-limit cache and skips when the shortest window is already counting down. The read is best-effort: an unknown or cold cache falls through to the ping, since missing a window-start defeats the feature while an occasional extra token is cheap.

## Config and code

Durable loop definitions live in `~/.config/rimz/loop.toml` under `[tasks.*]`. Machine-generated ephemerals live in `~/.local/state/rimz/loop-instances.json` with the same task shape; `is_ephemeral = once || deadline.is_some()` controls add routing and removal-on-fire. Per-room elder arm/fire stamps live in runtime `loop-fire.json`, and user-global history lives in state `loop-runs.log.jsonl`.

`schedule.rs` owns pure parsing, descriptions, and due evaluation. `cli/loop_cmd.rs` owns config/state editing plus the `list` and hidden `run` surfaces, including check execution and prompt augmentation. `loop_instances.rs` owns the ephemeral state store. `loop_fire.rs` owns elder firing and the `loop-fire.json` state. `loop_run_log.rs` owns result history, including `check_skipped` and `expired`.
