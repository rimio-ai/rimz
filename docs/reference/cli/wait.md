# Wait CLI

`rimz wait` is an alarm an agent sets for itself. The agent arms it and ends its turn; RimZ sends a message back into the same conversation when a delay elapses, an existing process goes away, a watched command exits, a polled check meets its condition, or a file changes or gains a matching line. It is the opposite direction of [`rimz agents wait`](./agents.md#list-and-manage-agents), which blocks a shell until an agent finishes.

Only an agent RimZ can identify (from its launch environment or process ancestry) can arm or cancel a wait, and every wait targets that agent's own live session. To wake another agent, attach a prompt, repeat on a schedule, or subscribe to a signal, use [`rimz loop add --wait`](./loop.md#waits-and-checks). The [loops guide](../../guide/loops.md#wake-a-running-agent) teaches the workflow.

```sh
rimz wait --in 30m                          # timer
rimz wait --pid 16776                       # an existing process
rimz wait --check 'nc -z localhost 3000'    # poll until the port opens
rimz wait --file build.log                  # any file change
rimz wait --file build.log --grep 'READY'   # a new matching line
rimz wait -- gh run watch --exit-status     # a watched command
rimz wait --on fail -- cargo test           # deliver only if the command fails
rimz wait --timeout 1h -- cargo build       # check in after 1h instead of 30m
rimz wait list                              # pending waits (bare `rimz wait` does the same)
rimz wait cancel wait-bold-comet
rimz wait cancel --all
```

The [global flags](../cli.md#global-flags) apply.

## Arm a wait

Give exactly one trigger: `--in`, `--pid`, `--check`, `--file`, or a command after `--`. With no trigger and no other arming flag, `rimz wait` lists pending waits instead.

| Flag | Applies to | Meaning |
| --- | --- | --- |
| `--in <DURATION>` | timer | Fire once after this delay. Greater than zero and less than `24h`. |
| `--pid <PID>` | process | Fire once this existing process is gone. `1` to `2147483647`. |
| `-- <COMMAND>...` | command | Run the command in a detached watcher and fire on its exit. |
| `--check <CMD>` | check | Poll a shell command until it succeeds (or fails with `--on fail`). |
| `--every <DURATION>` | check only | Sleep between runs. Default `1s`, greater than zero and less than `24h`. |
| `--file <PATH>` | file | Wait for a change in existence, size, or modification time. |
| `--grep <PATTERN>` | file only | Wait for a new newline-terminated line containing this literal, case-sensitive pattern. |
| `--on fail\|success\|any` | check or command | Default `success` for a check, `any` for a command. A check refuses `any`. |
| `--timeout <DURATION>` | every trigger except timer | When to send one check-in. Default `30m`, greater than zero and less than `24h`. The watch continues; commands are never killed. |
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

The trigger text in the receipt is `in <DURATION>`, `pid <PID>`, `watch: <command>`, `check: <command>` (with ` every <DURATION>` for a non-default interval and ` on fail` for failure polarity), or `file: <absolute path>` (with ` grep: <pattern>` when supplied). `--json` prints `{"name", "trigger", "target", "pending"}`, where `trigger` carries the same text and `pending` is the array [`rimz wait list --json`](#list-pending-waits) prints.

Arming is refused, exit 1, in these cases:

| Error | Cause |
| --- | --- |
| `choose exactly one wait trigger: --in, --pid, --check, --file, or a command after --` | Arming flags without a trigger, or more than one trigger. |
| `--check needs a command` | An empty or whitespace-only check. |
| `--file needs a path` | An empty file path. |
| `--grep requires --file` | A pattern without a file trigger. |
| `--grep needs a pattern` | An empty pattern. |
| `--on requires --check or a command after --` | `--on` with a timer, PID, or file. |
| `--on any has no meaning with --check; it polls until the command succeeds (default) or fails` | A check with `--on any`. |
| `--every requires --check` | An interval on another trigger. |
| `--timeout requires --pid, --check, --file, or a command after --` | `--timeout` with `--in`. |
| `<flag> must be greater than zero` | Zero for `--in`, `--timeout`, or `--every`. |
| `<flag> must be less than 24h` | `24h` or more for `--in`, `--timeout`, or `--every`. |
| `--file <path> is a directory; watch a file` | The path names a directory. |
| `cannot watch PID <PID>: permission denied; choose a process owned by your user` | The caller may not signal that process. |
| `arming a wait is only available to an agent RimZ can identify; run this command from an agent pane` | Run from a user shell, or from a process RimZ cannot trace to an agent. |
| `the calling agent has not registered a real session yet` | The agent has not reported its provider session yet. |

## Triggers

### Timer: `--in`

A timer fires once. RimZ adds the delay to the current time in the configured timezone and rounds up to the next whole minute, so `rimz wait --in 30m` armed at 14:01:20 fires at 14:32. The list shows it as `once at 14:32` with the state `due 14:32`. Firing needs a clock: the room's sidebar elder, or the [loop timer](./loop.md#timer) when no room is open.

### Existing process: `--pid`

`--pid` replaces `tail --pid=<PID> -f /dev/null` and needs no GNU `tail`. The watcher probes the PID natively once per second and fires when it no longer exists. An already-absent PID fires at once.

It observes whether the PID exists, nothing more:

- The process's exit status is unavailable; the verdict is `met after <ELAPSED>`, and `--on` is refused. A check-in reads `still not met after <ELAPSED>`.
- An unreaped zombie, or another process that reuses the PID, keeps the wait open.
- No output is captured; the output file stays empty unless the watcher itself fails.
- Canceling stops the watcher only. The process keeps running.

The receipt, the list, and the sidebar read `pid <PID>`. The delivered message starts with `waited on pid <PID>`.

### Polled check: `--check`

`rimz wait --check 'nc -z localhost 3000'` runs the command through `sh -c` at the checkout root until it exits successfully. `--on fail` instead waits for a non-zero exit or death by signal. `--on any` is refused; use a command after `--` for a single run.

The first run starts immediately. `--every` is the sleep between completed runs, default `1s`, not a fixed start-to-start interval. Each run truncates and rewrites the output file, so it holds the latest run's stdout and stderr. Exit 126 or 127 ends the wait and delivers `exit <CODE> after <ELAPSED>` rather than retrying a command that cannot run.

Completion reads `met after <ELAPSED>` beneath ``waited on check `<command>` ``. The single `--timeout` check-in reads `still not met after <ELAPSED>` and is evaluated between runs; a blocking run defers it and is never interrupted for it.

### File: `--file`

`rimz wait --file build.log` polls once per second and fires when existence, size, modification time, or the file itself (device and inode) differs from the mark recorded when armed. Creation, deletion, growth, truncation, same-size rewrites with a changed modification time, and a replacement at the path (an atomic rename or a rotation) count. Relative paths resolve against the arming shell's current directory and are stored absolute. An absent file is accepted; a directory is refused.

With `--grep 'READY'`, the watcher reads from the file's size at the arm point, or byte zero if it was absent. Existing lines do not count. Matching is literal and case-sensitive, on newline-terminated lines with a trailing carriage return trimmed. A line straddling the arm point counts only from the arm byte; an unterminated last line waits for its newline. If a different file appears at the path (a rename rotation or an atomic rewrite) or the file shrinks below the cursor, reading restarts at byte zero. One case goes unseen: the same file truncated in place and regrown past the old cursor within one poll, as `cmd > build.log` rerun can do. Lines before the old cursor are skipped. For a rerun that rewrites its log, remove the file before starting the command, or wait on `--check` instead.

The message starts with `waited on file <path>`, adding ``for `<pattern>` `` with `--grep`. Completion reads `met after <ELAPSED>`; a match adds a one-line preview as ``met after <ELAPSED>: `<line>` `` and writes the whole matched line to the output file. The line stored in the verdict is capped at 4 KiB. A check-in reads `still not met after <ELAPSED>`.

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

`--timeout` sets when a pending process, command, check, or file watch checks in; it never kills anything. RimZ sends one check-in message, including the output file's summary when non-empty, keeps watching, and delivers the final verdict later. The check-in ignores `--on`, happens once per wait, and leaves the wait pending. Polled watches evaluate it between probes. The default is `30m` and does not follow `loop.default-timeout`.

## The delivered message

A wait arrives as a message from `@rimz` with `Type: WAIT` (see [the message header](./message.md#the-message-header)). It is sent as a steer: it interrupts a working agent at once instead of waiting for the turn to end. Clock and signal deliveries from `rimz loop add --wait` park until the turn ends instead. [`rimz transcript`](./transcript.md) hides wait messages from its rendered view and keeps them in `--json`.

The body never inlines command output; a file pattern match inlines its matched-line preview. It names what was waited on, the verdict, and any non-empty output file with its estimated tokens and line count, and ends with the wait's name in brackets. Only a command watch says `no output` when the file is empty. A timer is one line:

```text
waited 30m [wait-bold-comet]
```

A final command verdict:

```text
waited on `cargo test`
exit 1 after 12m · output: /tmp/rimz-waits/wait-solid-pixel.output (~14k tokens, 1210 lines) [wait-solid-pixel]
```

A check-in adds the two follow-up commands. `Another check-in` repeats the wait's `--timeout` value; running it arms a separate timer that neither restarts nor stops the command:

```text
waited on `cargo build`
still running after 30m · output: /tmp/rimz-waits/wait-solid-pixel.output (~3.4k tokens, 340 lines) [wait-solid-pixel]

Stop it: rimz wait cancel wait-solid-pixel
Another check-in: rimz wait --in 30m
```

| Verdict | When |
| --- | --- |
| `exit <CODE> after <ELAPSED>` | The command exited, or a check exited 126 or 127. |
| `killed by signal after <ELAPSED>` | The command died from a signal. |
| `still running after <ELAPSED>` | A command check-in. |
| `met after <ELAPSED>` | A PID, check, or file condition was met; a file pattern match adds the matched-line preview. |
| `still not met after <ELAPSED>` | A PID, check, or file check-in. |
| `watcher died after <ELAPSED>; the command may still be running or may have died with it` | The watcher vanished without reporting. |

A silent command reads `· no output` after its verdict and names no file. A `--pid` wait captures no output of its process, so it shows no segment at all unless the watcher itself wrote an error. The token count is an estimate from OpenAI's public `o200k_base` tokenizer (`<1k`, `~1.2k`, `~22k`, `~1.2M`); Claude's tokenizer is not published and typically counts somewhat higher. Past the first 1 MiB the count is scaled from that sample. A command longer than 120 characters is shortened in the middle in the message, receipt, and list; the stored command and the logs keep it whole.

### The output file

The watcher writes combined stdout and stderr to `~/.rimz/ws/<workspace-dir>/tmp/rimz-waits/<name>.output` as they arrive, along with its own startup errors. A check truncates it before each run; a file pattern watch writes its matched line. The message shows `/tmp/rimz-waits/<name>.output` when the machine's `agents.isolation` is sandbox and the host path otherwise; an agent launched with a different per-launch isolation can see the form that does not match its own view.

Closing the room removes the file. In a long-lived room, `rimz gc` removes it once the wait is gone, no watcher runs, and the file has not been written for 14 days. The last 4 KiB of output stay in the loop history for [`rimz loop logs <name>`](./loop.md#loop-logs).

## While a wait is pending

An agent at rest with a pending one-shot wait shows the status `sleeping` (`☾` in the sidebar) instead of `idle` or `success`; the [status table](./agents.md#list) gives the precedence. Timers, PID waits, watched commands, polled checks, file watches, and one-shot or deadline signal deliveries count. Standing signal subscriptions do not.

In a supervised run, arming a wait and ending the turn does not finish the run: it stays `running` and keeps its pane until the work ends. The wake starts the next turn in that same run. If the wake is lost and nothing else remains to wake the parked run, it fails with exit `1` and a reason beginning `parked on a wake that never arrived`. The run's timeout still applies ([supervised runs](./agents.md#supervised-runs--p)).

A sleeping agent still takes messages: a normal message starts a turn and leaves the wait armed. A team member with a pending wait does not emit `team.idle`. The sidebar card counts pending waits as `⧖ waits (N)` and lists each one when expanded; [the card](../../interface/sidebar.md#the-card) shows how.

## List pending waits

`rimz wait list` (alias `ls`, or bare `rimz wait`) lists pending delivery rows for the current project. From an agent it shows rows targeting that agent's session, including `rimz loop add --wait` deliveries and team subscriptions. From a user shell it shows every delivery row in the project, read-only. With nothing pending it prints `no pending waits`.

| Column | Values |
| --- | --- |
| `NAME` | The wait or task name. |
| `STATE` | `due HH:MM` (or `due now`) for a clock row; `waiting`, or `waiting · <N> left` with a deadline, for a signal row; `watching pid <watcher PID>` or `watcher lost` for a command, PID, check, or file row; `disabled` or `paused · <time>` for a held row. |
| `TARGET` | The target agent's handle. |
| `AGE` | Time since arming for signal rows with an arm stamp, time since the watcher started for watch rows, otherwise `-`. |
| `TRIGGER` | `once at HH:MM` for a timer, `pid <PID>`, `watch: <command>`, `check: <command>[ every <DURATION>][ on fail]`, `file: <path>[ grep: <pattern>]`, a loop schedule such as `every day at 09:00`, or `on <selector> [k=v]`. A watch row armed from a linked worktree adds `· in <dir>`. |

`--json` prints an array of `{"name", "trigger", "dir", "target", "age", "state"}`. `dir` appears only when the row recorded a linked worktree, as a home-relative path; timers and signal rows record it too, though only commands and checks run there.

## Cancel a wait

`rimz wait cancel <name>` cancels one pending row targeting the caller, and `rimz wait cancel --all` cancels all of them, loop and team deliveries included. Give a name or `--all`, not both. Cancel refuses a user shell (`canceling a wait requires an agent RimZ can identify; run this command from an agent pane`) and a name not in the caller's list (``no pending wait named `<name>`; see `rimz wait list` ``).

Cancel removes each row, then sends SIGTERM to its watcher's process group, which stops a watched command with it. It prints `canceled <name>, ...` and the remaining pending rows, or only `no pending waits` when `--all` found nothing. `--json` prints `{"canceled": [...], "pending": [...]}`.

## What a wait writes on your machine

A wait is one row in `~/.rimz/ws/<workspace-dir>/loop-instances.json`; it never touches `loop.toml` or project config. Its `watch` is a command string, `{pid}`, `{check, every, on}`, or `{file, grep?, mark?}`. The file mark records size, modification time, device, and inode; an absent mark means the file was absent at arm time. Every watcher runs in its own process group and holds `loop-watch-<name>.lock` in the workspace runtime directory.

The row retires when the timer fires, the watch reaches its final outcome, the wait is canceled, or the target session ends, is lost, or is stopped. A check-in does not retire it. If a watcher dies without reporting, the room's elder notices the missing lock after a 30-second grace and delivers the `watcher died` verdict. `rimz gc` removes rows left behind.

Every fire writes loop history, so `rimz loop show <name>` and `rimz loop logs <name>` keep working after the wait retires. The mechanics are in [watched commands](../../internals/harness/loops.md#watched-commands) and [waits](../../internals/harness/loops.md#waits).

## Recipes

```sh
rimz wait --check 'test -e path'                          # file exists
rimz wait --check '! test -e path'                        # file gone
rimz wait --check 'nc -z localhost 3000'                  # port open
rimz wait --check 'curl -fsS http://localhost:3000/health' # URL healthy
rimz wait --check '! test -e .git/index.lock'             # lock file gone
rimz wait --check 'curl -fsS https://example.com/health' --every 30s
rimz wait --pid 16776                                    # process gone
rimz loop add merged --signal pr.merged --wait --once     # PR merged
```

Use `--every 30s` for remote checks. CI and PR state stay on [signals](./loop.md#waits-and-checks), rather than polling forge commands.
