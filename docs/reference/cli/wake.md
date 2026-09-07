# Wake CLI

`rimz wake` is a self-only alarm: wake the calling agent after a delay or when a watched command exits, without holding its turn open. It pins the caller's live session from its launch environment or process ancestry; arming and canceling require an identifiable agent, not a user shell. To target another agent, attach a note, or subscribe to a signal, use [`rimz loop add --wake`](./loop.md#wakes-and-checks). The [loops guide](../../guide/loops.md#wake-a-running-agent) covers the workflow.

```sh
rimz wake --in 30m
rimz wake -- gh run watch --exit-status
rimz wake --on fail -- cargo test
rimz wake --timeout 1h -- cargo build
rimz wake list
rimz wake list --json
rimz wake cancel wake-bold-comet
rimz wake cancel --all
```

Exactly one trigger is required when arming: `--in` or a command after `--`. Bare `rimz wake` lists pending deliveries. Each arm mints a workspace-unique `wake-<adjective>-<noun>` name and prints a receipt followed by the caller's pending rows; `--json` includes `pending` alongside `name`, `trigger`, and `target`.

## Triggers

**`--in <DURATION>`** fires once after a positive delay shorter than 24 hours. The delay resolves in the configured timezone and rounds up to the next scheduler minute. It needs the room's elder tick or the [loop timer](./loop.md#timer).

**A command after `--`** runs through `sh -c` at the project root with stdin closed, in a detached watcher that outlives the arming turn. `--on fail|success|any` filters its final outcome, defaulting to `any`: `fail` covers a non-zero exit or a lost watcher, and `success` a zero exit. A filtered outcome records `skipped` and retires the row without a final message.

**`--timeout <DURATION>` is a check-in, not a kill deadline.** It defaults to `30m`, independent of `loop.default-timeout`, and must be positive and shorter than 24 hours. If the command is still running then, RimZ sends one notice with the current output tail, leaves the command and row running, and later delivers the exit verdict. The check-in is never filtered by `--on` and does not repeat automatically. Both `--on` and `--timeout` require a command.

## The delivered message

Self wakes are durable messages from `@rimz` with `Type: WAKE`, dispatched as steer: they interrupt a working agent rather than waiting for its next `done` boundary. Scheduled and signal [loop deliveries](./loop.md#signals) instead park at that boundary. Delivered wakes are hidden from the rendered transcript and retained by [`rimz transcript --json`](./transcript.md).

The body names the wait, its elapsed outcome, the output path and task name, then the last 4 KiB of combined output or `(no output)`. A timer reads `waited 30m [<name>]`. A command check-in has this shape:

```text
waited on `cargo build`
still running after 30m · output: …/wakes/wake-solid-pixel.log [wake-solid-pixel]
<output tail>

Stop it: rimz wake cancel wake-solid-pixel
Another check-in: rimz wake --in 30m
```

The follow-up timer is a separate alarm; it does not restart or stop the watched command. Final verdicts include `exit 0 after 4m`, `exit 1 after 12m`, `killed by signal after 3s`, and `watcher died after 3m; the command may still be running or may have died with it`. Commands longer than 120 characters are middle-truncated in receipts, wait lines, and lists only; stored commands and logs stay complete.

### The output file

Combined stdout and stderr go to `~/.local/state/rimz/workspaces/<workspace-id>/wakes/<name>.log` as they arrive. The watcher uses that file for its own stderr too, so an early startup failure leaves evidence there. The file outlives the room; `rimz gc` removes it only once its row is gone, no watcher is running, and its last write is over 14 days old.

## List and cancel

`rimz wake list` shows pending instance delivery rows in this workspace, including loop and team subscriptions. An identified agent sees only rows targeting its session; a user shell can read every delivery row in the room. The list includes name, state, target, age, and trigger, including signal matches. Disabled or paused rows show their held state rather than waiting or due. An active command row reports `watching pid <PID>` or `watcher lost`.

`rimz wake cancel <name>` cancels a pending row targeting the caller; `rimz wake cancel --all` cancels every such row, including loop and team deliveries. A name and `--all` are mutually exclusive. Cancellation removes rows first and sends SIGTERM to each watcher's process group, stopping its command too. Every cancel prints the canceled names followed by the remaining pending rows, including an explicit empty state. JSON returns `{"canceled":[…],"pending":[…]}`.

## What a wake writes on your machine

Every wake is an instance row in `~/.local/state/rimz/workspaces/<workspace-id>/loop-instances.json`, never `loop.toml`. The watcher runs in its own process group and holds `loop-watch-<name>.lock` in workspace runtime storage. The row retires after the timer or final command outcome, cancellation, or session retirement; a check-in alone does not consume it.

Each fire records durable loop history, so `rimz loop show <name>` and `rimz loop logs <name>` remain useful after retirement. If the watcher dies, the elder detects its missing lock after a 30-second grace and delivers the lost-watcher verdict. Session end, loss, and explicit stop retire pinned deliveries and stop watchers; `rimz gc` is the backstop.
