# Wait CLI

`rimz wait` is a self-only alarm: wake the calling agent after a delay, when an existing process disappears, or when a watched command exits, without holding its turn open. It pins the caller's live session from its launch environment or process ancestry; arming and canceling require an identifiable agent, not a user shell. To target another agent, attach a note, or subscribe to a signal, use [`rimz loop add --wait`](./loop.md#waits-and-checks). The [loops guide](../../guide/loops.md#wake-a-running-agent) covers the workflow.

```sh
rimz wait --in 30m
rimz wait --pid 16776
rimz wait -- gh run watch --exit-status
rimz wait --on fail -- cargo test
rimz wait --timeout 1h -- cargo build
rimz wait list
rimz wait list --json
rimz wait cancel wait-bold-comet
rimz wait cancel --all
```

Exactly one trigger is required when arming: `--in`, `--pid`, or a command after `--`. Bare `rimz wait` lists pending deliveries. Each arm mints a workspace-unique `wait-<adjective>-<noun>` name and prints a receipt followed by the caller's pending rows; `--json` includes `pending` alongside `name`, `trigger`, and `target`.

## Triggers

Once the caller rests, an armed one-shot wait makes its status `sleeping` rather than `idle` or `success`. The standard agent card counts pending one-shot waits on its `⧉ subagents (N) · ⧖ waits (N)` line while any is armed, even when the agent is working, and lists each wait beneath when the card is expanded; the subagents half is absent until the session has spawned a child. The sidebar shows a static cool-toned `☾` and names the wait, for example `wait timer 30m · in 12m` or `wait shell cargo test`; `--pid` reads `wait pid <PID>`. Running, waiting, failed, paused, and delegation to live children take precedence. A normal message can start another turn immediately without canceling the wait. The same projection covers one-shot loop deliveries, but standing subscriptions listed by `wait list` do not make an agent sleep. A pending one-shot wait also withholds `team.idle`; cancellation is reevaluated at the caller's following lifecycle boundary, not by emitting an idle signal when the row is removed.

Below 46 columns the card's waits count shortens to `⧖ N`, sharing its line with `⧉ N` when subagents are present. At 46 columns and wider the full labels return. Clicking the shared count line toggles the entries without focusing the pane. Expanded wait entries name the kind first (`timer`, `pid`, `shell`, or `signal`) and keep their elapsed clocks muted even for long waits.

**`--in <DURATION>`** fires once after a positive delay shorter than 24 hours. The delay resolves in the configured timezone and rounds up to the next scheduler minute. It needs the room's elder tick or the [loop timer](./loop.md#timer).

**`--pid <PID>`** waits for an existing process without needing `tail --pid=<PID> -f /dev/null`. The receipt starts `armed <name>: pid <PID>`; the pending list and sidebar wait entry also read `pid <PID>`. It watches a positive PID using a portable `kill -0` check once per second, so no GNU `tail` is required. Permission to observe the PID is checked before arming; an inaccessible PID is refused, and an already-absent PID completes immediately. This observes PID presence, not the original process identity or its exit status: an unreaped zombie or a reused PID can keep the wait open. `exit 0` means the wait finished, not that the process succeeded; `--on` is therefore command-only. No process output is captured, and canceling this wait stops only the watcher, not the existing process.

**A command after `--`** runs through `sh -c` at the root of the checkout it was armed from (including linked worktrees) with stdin closed, in a detached watcher that outlives the arming turn. `--on fail|success|any` filters its final outcome, defaulting to `any`: `fail` covers a non-zero exit or a lost watcher, and `success` a zero exit. A filtered outcome records `skipped` and retires the row without a final message.

**`--timeout <DURATION>` is a check-in, not a kill deadline.** It defaults to `30m`, independent of `loop.default-timeout`, and must be positive and shorter than 24 hours. If the command or PID wait is still running then, RimZ sends one notice with the current output file's size and line count, leaves the watcher and row running, and later delivers the exit verdict. The check-in is never filtered by `--on` and does not repeat automatically. `--timeout` requires `--pid` or a command.

## The delivered message

Self waits are durable messages from `@rimz` with `Type: WAIT`, dispatched as steer: they interrupt a working agent rather than waiting for its next `done` boundary. Scheduled and signal [loop deliveries](./loop.md#signals) instead park at that boundary. Delivered waits are hidden from the rendered transcript and retained by [`rimz transcript --json`](./transcript.md).

The body names the wait, its elapsed outcome and task name, and the combined output file's path, byte size, and line count. It never inlines command output; even an empty file is listed as `0 B, 0 lines`. A timer reads `waited 30m [<name>]`. A command check-in has this shape:

```text
waited on `cargo build`
still running after 30m · output (12 KB, 340 lines): /tmp/rimz-waits/wait-solid-pixel.output [wait-solid-pixel]

Stop it: rimz wait cancel wait-solid-pixel
Another check-in: rimz wait --in 30m
```

The follow-up timer is a separate alarm; it does not restart or stop the watched command. Final verdicts include `exit 0 after 4m`, `exit 1 after 12m`, `killed by signal after 3s`, and `watcher died after 3m; the command may still be running or may have died with it`. Commands longer than 120 characters are middle-truncated in receipts, wait lines, and lists only; stored commands and logs stay complete.

### The output file

Combined stdout and stderr go to `~/.local/state/rimz/workspaces/<workspace-id>/tmp/rimz-waits/<name>.output` as they arrive. Under sandbox isolation the message shows `/tmp/rimz-waits/<name>.output`; in host mode it shows the host path. The watcher uses that file for its own stderr too, so an early startup failure leaves evidence there. Room teardown removes the file; in a long-lived room, `rimz gc` removes it once its row is gone, no watcher is running, and its last write is over 14 days old. The durable run record retains the last 4 KiB for `rimz loop logs` after the file is gone.

## List and cancel

`rimz wait list` shows pending instance delivery rows in this workspace, including loop and team subscriptions. An identified agent sees only rows targeting its session; a user shell can read every delivery row in the room. The list includes name, state, target, age, and trigger, including signal matches; only watched commands armed from a linked worktree show `· in <dir>` in the trigger. JSON includes the optional home-relative `dir` when set on any delivery row: it records the arming worktree's root, even for timers and signal subscriptions that run no command. Only checks and watched commands execute there. Disabled or paused rows show their held state rather than waiting or due. An active command row reports `watching pid <PID>` or `watcher lost`.

`rimz wait cancel <name>` cancels a pending row targeting the caller; `rimz wait cancel --all` cancels every such row, including loop and team deliveries. A name and `--all` are mutually exclusive. Cancellation removes rows first and sends SIGTERM to each watcher's process group, stopping its command too. Every cancel prints the canceled names followed by the remaining pending rows, including an explicit empty state. JSON returns `{"canceled":[…],"pending":[…]}`.

## What a wait writes on your machine

Every wait is an instance row in `~/.local/state/rimz/workspaces/<workspace-id>/loop-instances.json`, never `loop.toml`. The watcher runs in its own process group and holds `loop-watch-<name>.lock` in workspace runtime storage. The row retires after the timer or final command outcome, cancellation, or session retirement; a check-in alone does not consume it.

Each fire records durable loop history, so `rimz loop show <name>` and `rimz loop logs <name>` remain useful after retirement. If the watcher dies, the elder detects its missing lock after a 30-second grace and delivers the lost-watcher verdict. Session end, loss, and explicit stop retire pinned deliveries and stop watchers; `rimz gc` is the backstop.
