# Loop tasks

> See [DESIGN.md](../../../DESIGN.md) for the commitments this doc operationalizes. Loop tasks ride the supervised-run path in [harness.md](./harness.md#supervised-runs), the message path in [message.md](./message.md), and the budget-window model in [provider.md](./provider.md).

Loop tasks run scheduled wake-ups from the room's elected sidebar elder. While a room for the task's project is open, the elder's data tick evaluates configured tasks and fires `rimz loop run <name>`, which resolves the recorded project `root` and then uses exactly one configured mode: `spec` spawns one transient supervised pane, while `bind` delivers a prompt to one living agent instance.

In `spec` mode, each task names exactly one agent cell: a built-in kind, a profile, or an adapter-supported virtual cell such as `claude-auto`, `codex-yolo`, or `claude-ping`. Team specs, multi-cell layouts, and command cells are rejected at add time because a scheduled task owns one supervised pane.

## Schedule forms

Rimz stores schedule intent in per-machine `agents.toml`. `rimz loop add` validates the task, runs hook preflight, and makes it live immediately while a room for the task's project is open.

- **Calendar:** `at = "07:00"` with optional `days = "weekdays"`, `daily`, `weekends`, `mon-fri`, or `mon,wed,fri`.
- **Interval:** `every = "15m"`, `2h`, or `1d`; the elder fires at the exact interval measured from the last arm or fire.
- **Raw cron:** `cron = "*/15 * * * *"` uses the in-process five-field matcher for minute, hour, day-of-month, month, and day-of-week.
- **One-shot:** `once = true` with a calendar or cron schedule. `rimz loop add --in 30m` resolves to a local `at` time and implies `once`.

Calendar tasks catch up once for today's matching time when a room opens later the same day. Cron tasks fire during matching open-room minutes, so a room opened later waits for the next matching minute.

One-shot tasks remove their config row immediately before the supervised run or message delivery. The run exits the process with the agent status, so cleanup cannot happen afterward. A one-shot removed pre-fire that then fails to launch is not retried.

## Elder firing

The elder keeps a per-room `loop-fire.json` map of task name to last-fire `Timestamp` under the workspace runtime dir. First sight arms a task by recording `now` and does not fire; the next matching occurrence fires. A fire records `now` before spawning the detached helper, which guards against duplicate pane spawns on sub-interval ticks.

Each room fires only tasks whose stored absolute `root` maps to its `WorkspaceId`. The root is canonicalized at add time, so workspace ownership is a pure hash comparison and two open rooms do not fire each other's tasks.

The elder spawns `rimz loop run <name>` with fresh null stdio. The hidden runner resolves the task's recorded root, applies the same preflight as an immediate run, and then either launches the supervised pane or messages the pinned session.

Self-paced loops use ordinary one-shots. The agent schedules its next wake with `--in <delay>` at the end of the current wake; the config row is removed before delivery, and the agent creates the next one only when it still has work. This keeps the state visible in `rimz loop list`.

## Delivering to a living instance

Bind-mode pins a schedule to one exact agent session. `rimz loop add <name> --bind @<handle> --prompt "<text>" ...` resolves the address against the live rollup immediately, records `bind = { kind, session, handle }`, and rejects `spec` and supervised-run flags because delivery does not launch a new pane.

On fire, `loop run` resolves the recorded `root`, checks that the root agent session still exists, and sends the prompt through the same message path as `rimz message`. An idle agent receives the text immediately; a running agent parks the message for the next `done` turn boundary; a missing session is skipped and the schedule is removed because that exact conversation cannot return.

`rimz gc` repeats the same liveness check for bind-mode tasks and reaps schedules whose pinned session has left the rollup. This is a safety sweep for tasks that did not get a successful fire after the agent exited.

## Window-priming pings

A task whose `spec` is a `<kind>-ping` virtual cell starts a provider's budget window at a time you choose. The task defaults `prompt = "ping"` and uses lowest effort unless configured otherwise; the virtual cell supplies the adapter's ping arguments.

The window is account-scoped, shared by every session of a provider kind ([provider.md](./provider.md)), so one ping per provider primes the whole account. Before spawning the turn, `loop run` reads the shared rate-limit cache and skips when the shortest window is already counting down. The read is best-effort: an unknown or cold cache falls through to the ping, since missing a window-start defeats the feature while an occasional extra token is cheap.

## Config and code

Loop tasks live in per-machine `[agents.loop.tasks.*]`, outside the trust hash, and each entry runs the rimz-owned `loop run` rather than arbitrary shell. The config shape is in [configuration.md → Loop tasks](../../reference/configuration.md#loop-tasks); the `rimz loop add` / `remove` / `list` commands are in [agents.md → Schedule turns with loop](../../reference/cli/agents.md#schedule-turns-with-loop).

`schedule.rs` owns pure parsing, descriptions, and due evaluation. `cli/loop_cmd.rs` owns config editing plus the `list` and hidden `run` surfaces. `loop_fire.rs` owns elder firing and the `loop-fire.json` state.
