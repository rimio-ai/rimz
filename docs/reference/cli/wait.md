# Wait CLI

`rimz wait` is an alarm an agent sets for itself. The agent arms it and ends its turn; RimZ sends a message back into the same conversation when a delay elapses, an existing process goes away, or a watched command exits. It is the opposite direction of [`rimz agents wait`](./agents.md#list-and-manage-agents), which blocks a shell until an agent finishes.

Only an agent RimZ can identify (from its launch environment or process ancestry) can arm or cancel a wait, and every wait targets that agent's own live session. To wake another agent, attach a prompt, repeat on a schedule, or subscribe to a signal, use [`rimz loop add --wait`](./loop.md#waits-and-checks). The [loops guide](../../guide/loops.md#wake-a-running-agent) teaches the workflow.

```sh
rimz wait --in 30m                          # timer
rimz wait --pid 16776                       # an existing process
rimz wait -- gh run watch --exit-status     # a watched command
rimz wait --on fail -- cargo test           # deliver only if the command fails
rimz wait --timeout 1h -- cargo build       # check in after 1h instead of 30m
rimz wait list                              # pending waits (bare `rimz wait` does the same)
rimz wait cancel wait-bold-comet
rimz wait cancel --all
```

The [global flags](../cli.md#global-flags) apply.

## Arm a wait

Give exactly one trigger: `--in`, `--pid`, or a command after `--`. With no trigger and no other arming flag, `rimz wait` lists pending waits instead.

| Flag | Applies to | Meaning |
| --- | --- | --- |
| `--in <DURATION>` | timer | Fire once after this delay. Greater than zero and less than `24h`. |
| `--pid <PID>` | process | Fire once this existing process is gone. `1` to `2147483647`. |
| `-- <COMMAND>...` | command | Run the command in a detached watcher and fire on its exit. |
| `--on fail\|success\|any` | command only | Which final outcome delivers. Default `any`. |
| `--timeout <DURATION>` | process or command | When to send one still-running check-in. Default `30m`, greater than zero and less than `24h`. The command is never killed. |
| `--json` | all | Print the receipt as JSON. |

Durations take `s`, `m`, `h`, or `d` units (`90s`, `30m`, `1h`).

Each arm mints a name of the form `wait-<adjective>-<noun>`, unique in the workspace (a collision appends `-<N>`). The receipt names the wait, its trigger, and its target, then lists every pending row for the caller:

```console
$ rimz wait -- cargo test
armed wait-solid-pixel: watch: cargo test → @coder
NAME              STATE               TARGET  AGE  TRIGGER
wait-bold-comet   due 14:32           @coder  -    once at 14:32
wait-solid-pixel  watching pid 48213  @coder  0s   watch: cargo test
```

The trigger text in the receipt is `in <DURATION>`, `pid <PID>`, or `watch: <command>`. `--json` prints `{"name", "trigger", "target", "pending"}`, where `trigger` carries the same text and `pending` is the array [`rimz wait list --json`](#list-pending-waits) prints.

Arming is refused, exit 1, in these cases:

| Error | Cause |
| --- | --- |
| `choose exactly one wait trigger: --in, --pid, or a command after --` | No trigger with `--on` or `--timeout`, or more than one trigger. |
| `--on requires a command after --` | `--on` with `--in` or `--pid`. |
| `--timeout requires --pid or a command after --` | `--timeout` with `--in`. |
| `--in must be greater than zero`, `--timeout must be less than 24h` | A duration out of range. |
| `cannot watch PID <PID>: permission denied; choose a process owned by your user` | The caller may not signal that process. |
| `arming a wait is only available to an agent RimZ can identify; run this command from an agent pane` | Run from a user shell, or from a process RimZ cannot trace to an agent. |
| `the calling agent has not registered a real session yet` | The agent has not reported its provider session yet. |

## Triggers

### Timer: `--in`

A timer fires once. RimZ adds the delay to the current time in the configured timezone and rounds up to the next whole minute, so `rimz wait --in 30m` armed at 14:01:20 fires at 14:32. The list shows it as `once at 14:32` with the state `due 14:32`. Firing needs a clock: the room's sidebar elder, or the [loop timer](./loop.md#timer) when no room is open.

### Existing process: `--pid`

`--pid` replaces `tail --pid=<PID> -f /dev/null` and needs no GNU `tail`. The watcher runs `kill -0 <PID>` once per second and fires when the check fails. An already-absent PID fires at once.

It observes whether the PID exists, nothing more:

- The process's exit status is unavailable, so the delivered verdict is `exit 0` whenever the watch ends, and `--on` is refused.
- An unreaped zombie, or another process that reuses the PID, keeps the wait open.
- No output is captured; the output file stays empty unless the watcher itself fails.
- Canceling stops the watcher only. The process keeps running.

The receipt, the list, and the sidebar read `pid <PID>`. The delivered message names the watcher's shell loop instead: ``waited on `while kill -0 16776 2>/dev/null; do sleep 1; done` ``.

### Watched command: `--`

The command after `--` runs through `sh -c` with stdin closed, at the root of the checkout it was armed from (a linked worktree's root when armed from one). It runs in a detached watcher in its own process group, so it outlives the turn that armed it. Stdout and stderr go to the [output file](#the-output-file).

`--on` picks which final outcome delivers a message:

| `--on` | Delivers on |
| --- | --- |
| `any` (default) | every final outcome |
| `fail` | a non-zero exit, death by signal, or a lost watcher |
| `success` | exit 0 |

An outcome `--on` filters out records `skipped` in the loop history and retires the wait without a message.

### Check-ins: `--timeout`

`--timeout` sets when a still-running process or command checks in; it never kills anything. If the watch is still running at that point, RimZ sends one check-in message with the output file's current size and line count, keeps watching, and delivers the final verdict later. The check-in ignores `--on`, happens once per wait, and leaves the wait pending. The default is `30m` and does not follow `loop.default-timeout`.

## The delivered message

A wait arrives as a message from `@rimz` with `Type: WAIT` (see [the message header](./message.md#the-message-header)). It is sent as a steer: it interrupts a working agent at once instead of waiting for the turn to end. Clock and signal deliveries from `rimz loop add --wait` park until the turn ends instead. [`rimz transcript`](./transcript.md) hides wait messages from its rendered view and keeps them in `--json`.

The body never inlines command output. It names what was waited on, the verdict, and the output file with its size and line count (left out when the file is empty), and ends with the wait's name in brackets. A timer is one line:

```text
waited 30m [wait-bold-comet]
```

A final command verdict:

```text
waited on `cargo test`
exit 1 after 12m · output (48 KB, 1210 lines): /tmp/rimz-waits/wait-solid-pixel.output [wait-solid-pixel]
```

A check-in adds the two follow-up commands. `Another check-in` repeats the wait's `--timeout` value; running it arms a separate timer that neither restarts nor stops the command:

```text
waited on `cargo build`
still running after 30m · output (12 KB, 340 lines): /tmp/rimz-waits/wait-solid-pixel.output [wait-solid-pixel]

Stop it: rimz wait cancel wait-solid-pixel
Another check-in: rimz wait --in 30m
```

| Verdict | When |
| --- | --- |
| `exit <CODE> after <ELAPSED>` | The command exited, or a `--pid` watch ended (always `exit 0`). |
| `killed by signal after <ELAPSED>` | The command died from a signal. |
| `still running after <ELAPSED>` | The check-in. |
| `watcher died after <ELAPSED>; the command may still be running or may have died with it` | The watcher vanished without reporting. |

A message for an empty output file leaves the `output (...)` segment out, so a silent command ends at its verdict. A command longer than 120 characters is shortened in the middle in the message, receipt, and list; the stored command and the logs keep it whole.

### The output file

The watcher writes combined stdout and stderr to `~/.local/state/rimz/workspaces/<workspace-id>/tmp/rimz-waits/<name>.output` as they arrive, along with its own startup errors. The message shows `/tmp/rimz-waits/<name>.output` when the machine's `agents.isolation` is sandbox and the host path otherwise; an agent launched with a different per-launch isolation can see the form that does not match its own view.

Closing the room removes the file. In a long-lived room, `rimz gc` removes it once the wait is gone, no watcher runs, and the file has not been written for 14 days. The last 4 KiB of output stay in the loop history for [`rimz loop logs <name>`](./loop.md#loop-logs).

## While a wait is pending

An agent at rest with a pending one-shot wait shows the status `sleeping` (`☾` in the sidebar) instead of `idle` or `success`; the [status table](./agents.md#list) gives the precedence. Timers, PID waits, watched commands, and one-shot or deadline signal deliveries count. Standing signal subscriptions do not.

A sleeping agent still takes messages: a normal message starts a turn and leaves the wait armed. A team member with a pending wait does not emit `team.idle`. The sidebar card counts pending waits as `⧖ waits (N)` and lists each one when expanded; [the card](../../interface/sidebar.md#the-card) shows how.

## List pending waits

`rimz wait list` (alias `ls`, or bare `rimz wait`) lists pending delivery rows for the current project. From an agent it shows rows targeting that agent's session, including `rimz loop add --wait` deliveries and team subscriptions. From a user shell it shows every delivery row in the project, read-only. With nothing pending it prints `no pending waits`.

| Column | Values |
| --- | --- |
| `NAME` | The wait or task name. |
| `STATE` | `due HH:MM` (or `due now`) for a clock row; `waiting`, or `waiting · <N> left` with a deadline, for a signal row; `watching pid <PID>` or `watcher lost` for a command or PID row; `disabled` or `paused · <time>` for a held row. |
| `TARGET` | The target agent's handle. |
| `AGE` | Time since arming for signal rows with an arm stamp, time since the watcher started for command rows, otherwise `-`. |
| `TRIGGER` | `once at HH:MM` for a timer, `pid <PID>`, `watch: <command>`, a loop schedule such as `every day at 09:00`, or `on <selector> [k=v]`. A PID or command row armed from a linked worktree adds `· in <dir>`. |

`--json` prints an array of `{"name", "trigger", "dir", "target", "age", "state"}`. `dir` appears only when the row recorded a linked worktree, as a home-relative path; timers and signal rows record it too, though only commands run there.

## Cancel a wait

`rimz wait cancel <name>` cancels one pending row targeting the caller, and `rimz wait cancel --all` cancels all of them, loop and team deliveries included. Give a name or `--all`, not both. Cancel refuses a user shell (`canceling a wait requires an agent RimZ can identify; run this command from an agent pane`) and a name not in the caller's list (``no pending wait named `<name>`; see `rimz wait list` ``).

Cancel removes each row, then sends SIGTERM to its watcher's process group, which stops a watched command with it. It prints `canceled <name>, ...` and the remaining pending rows, or only `no pending waits` when `--all` found nothing. `--json` prints `{"canceled": [...], "pending": [...]}`.

## What a wait writes on your machine

A wait is one row in `~/.local/state/rimz/workspaces/<workspace-id>/loop-instances.json`; it never touches `loop.toml` or project config. A command or PID watcher runs in its own process group and holds `loop-watch-<name>.lock` in the workspace runtime directory.

The row retires when the timer fires, the command or PID watch reaches its final outcome, the wait is canceled, or the target session ends, is lost, or is stopped. A check-in does not retire it. If a watcher dies without reporting, the room's elder notices the missing lock after a 30-second grace and delivers the `watcher died` verdict. `rimz gc` removes rows left behind.

Every fire writes loop history, so `rimz loop show <name>` and `rimz loop logs <name>` keep working after the wait retires. The mechanics are in [watched commands](../../internals/harness/loops.md#watched-commands) and [waits](../../internals/harness/loops.md#waits).
