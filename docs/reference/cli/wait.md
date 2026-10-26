# Wait CLI

`rimz wait` is an alarm an agent sets for itself. The agent arms it and ends its turn; RimZ sends a message back into the same conversation when a delay elapses, an existing process goes away, a watched command exits, a polled check meets its condition, or a file changes or gains a matching line. It is the opposite direction of [`rimz agents wait`](./agents.md#list-and-manage-agents), which blocks a shell until an agent finishes.

Only an agent RimZ can identify (from its launch environment or process ancestry) can arm or cancel a wait, and every wait targets that agent's own live session. To wake another agent, attach a prompt, repeat on a schedule, or subscribe to a signal, use [`rimz loop add --wait`](./loop.md#waits-and-checks). The [loops guide](../../guide/loops.md#wake-a-running-agent) teaches the workflow.

```sh
rimz wait --in 30m                           # timer
rimz wait --pid 16776                        # an existing process
rimz wait --check 'nc -z localhost 3000'     # poll until the port opens
rimz wait --file build.log                   # any file change
rimz wait --file build.log --grep 'READY'    # a new matching line
rimz wait --run 'gh run watch --exit-status' # a watched command
rimz wait --on fail --run 'cargo test'       # deliver only if the command fails
rimz wait --check-in 1h --run 'cargo build'  # request a check-in after 1h
rimz wait list                               # your waits; whole room from a user shell
rimz wait cancel wait-bold-comet
rimz wait cancel --all
```

The [global flags](../cli.md#global-flags) apply.

## Arm a wait

Give exactly one trigger: `--in`, `--pid`, `--check`, `--file`, or `--run`. With no trigger and no other arming flag, `rimz wait` lists pending waits instead.

| Flag | Applies to | Meaning |
| --- | --- | --- |
| `--label <TEXT>` | all triggers | Short description of 1 to 60 characters after trimming, on one line. |
| `--in <DURATION>` | timer | Fire once after this delay. Greater than zero and less than `24h`. |
| `--pid <PID>` | process | Fire once this existing process is gone. `1` to `2147483647`. |
| `--run <COMMAND>` | command | Run the command in a detached watcher and fire on its exit. |
| `--check <CMD>` | check | Poll a shell command until it succeeds (or fails with `--on fail`). |
| `--every <DURATION>` | check only | Sleep between runs. Default `1s`, greater than zero and less than `24h`. |
| `--file <PATH>` | file | Wait for a change in existence, size, or modification time. |
| `--grep <PATTERN>` | file only | Wait for a new newline-terminated line containing this literal, case-sensitive pattern. |
| `--on fail\|success\|any` | check or command | Default `success` for a check, `any` for a command. A check refuses `any`. |
| `--check-in <DURATION>` | every trigger except timer | When to send one check-in. No default check-in with cache keepalive enabled and a known provider TTL; otherwise `30m`. Explicit durations must be greater than zero and less than `24h`. The watch continues; commands are never killed. |
| `--json` | all | Print the receipt as JSON. |

Durations take `s`, `m`, `h`, or `d` units (`90s`, `30m`, `1h`).

While an agent rests with waits pending, even when its card displays working, RimZ sends a facts-only prompt-cache keepalive shortly before its provider TTL expires. One ping lists every pending wait, subagent, and launched team, including elapsed times when known. Pings recur after each completed ping turn, without instructions, while each would start less than `harness.cache_keepalive_max` (default `6h`, `off` for no limit) after the agent's last request of its own. The last one adds `Keepalive limit 6h reached: this is the last ping until your next turn.`, and none follow until the agent's next turn of any other kind, which restarts the clock. The [configuration guide](../../guide/configuration.md) sets the switch and provider TTLs; delivery is best-effort and does not replace a wait's final wake. A ping missed in its window (no running sidebar, a suspended machine) is skipped, and no further pings come until the agent's next turn. With a known TTL and no `--check-in`, a wait on a command that never exits therefore stays silent; pass `--check-in` to get a check-in.

Each arm mints a name of the form `wait-<adjective>-<noun>`, unique in the workspace (a collision appends `-<N>`). The receipt names the wait, its trigger, and its target, then shows the caller's self waits, with other subscriptions summarized separately:

```console
$ rimz wait --run 'cargo test'
armed wait-solid-pixel: watch: cargo test → @coder (check-in 30m)
@coder · 2 waits
  wait-bold-comet   pending · fires in 28m         once at 14:32
  wait-solid-pixel  watching 0s · check-in in 30m  cargo test
```

The trigger text in the receipt is `in <DURATION>`, `pid <PID>`, `watch: <command>`, `check: <command>` (with ` every <DURATION>` for a non-default interval and ` on fail` for failure polarity), or `file: <absolute path>` (with ` grep: <pattern>` when supplied). Watch receipt headers end with `(check-in <DURATION>)` when a check-in limit is recorded; timers, and watches armed without one under cache keepalive, have no suffix. `--json` prints `{"name", "trigger", "target", "pending"}`, where `trigger` carries the prefixed header text and `pending` is exactly the array [`rimz wait list --json`](#list-pending-waits) prints, including subscriptions, with separate types, bare trigger subjects, `owner`, and `parent`.

Arming is refused, exit 1, in these cases (an invalid `--label` exits 2 during argument parsing):

| Error | Cause |
| --- | --- |
| `` `--timeout` is now `--check-in`: it sends one still-running notice and never stops the command `` | `--timeout` passed to `rimz wait`; the flag was renamed. |
| `choose exactly one wait trigger: --in, --pid, --check, --file, or --run` | Arming flags without a trigger, or more than one trigger. |
| `--check needs a command` | An empty or whitespace-only check. |
| `--run needs a command` | An empty or whitespace-only watched command. |
| `label must be one line of 1–60 characters` | An empty label after trimming, more than 60 characters, or a line break. |
| `--file needs a path` | An empty file path. |
| `--grep requires --file` | A pattern without a file trigger. |
| `--grep needs a pattern` | An empty pattern. |
| `--on requires --check or --run` | `--on` with a timer, PID, or file. |
| `--on any has no meaning with --check; it polls until the command succeeds (default) or fails` | A check with `--on any`. |
| `--every requires --check` | An interval on another trigger. |
| `--check-in requires --pid, --check, --file, or --run` | `--check-in` with `--in`. |
| `<flag> must be greater than zero` | Zero for `--in`, `--check-in`, or `--every`. |
| `<flag> must be less than 24h` | `24h` or more for `--in`, `--check-in`, or `--every`. |
| `--file <path> is a directory; watch a file` | The path names a directory. |
| `cannot watch PID <PID>: permission denied; choose a process owned by your user` | The caller may not signal that process. |
| `arming a wait is only available to an agent RimZ can identify; run this command from an agent pane` | Run from a user shell, or from a process RimZ cannot trace to an agent. |
| `the calling agent has not registered a real session yet` | The agent has not reported its provider session yet. |

## Triggers

### Timer: `--in`

A timer fires once. RimZ adds the delay to the current time in the configured timezone and rounds up to the next whole minute, so `rimz wait --in 30m` armed at 14:01:20 fires at 14:32. The list shows it as `once at 14:32` with the state `due 14:32`. Firing needs a clock: the room's sidebar host, or the [loop timer](./loop.md#timer) when no room is open.

### Existing process: `--pid`

`--pid` replaces `tail --pid=<PID> -f /dev/null` and needs no GNU `tail`. The watcher probes the PID natively once per second and fires when it no longer exists. An already-absent PID fires at once.

It observes whether the PID exists, nothing more:

- The process's exit status is unavailable; the verdict is `met after <ELAPSED>`, and `--on` is refused. A check-in reads `still not met after <ELAPSED>`.
- An unreaped zombie, or another process that reuses the PID, keeps the wait open.
- No output is captured; the output file stays empty unless the watcher itself fails.
- Canceling stops the watcher only. The process keeps running.

The receipt, the list, and the sidebar read `pid <PID>`. The delivered message starts with `waited on pid <PID>`.

### Polled check: `--check`

`rimz wait --check 'nc -z localhost 3000'` runs the command through `sh -c` at the checkout root until it exits successfully. `--on fail` instead waits for a non-zero exit or death by signal. `--on any` is refused; use `--run` for a single run.

The first run starts immediately. `--every` is the sleep between completed runs, default `1s`, not a fixed start-to-start interval. Each run truncates and rewrites the output file, so it holds the latest run's stdout and stderr. Exit 126 or 127 ends the wait and delivers `exit <CODE> after <ELAPSED>` rather than retrying a command that cannot run.

Completion reads `met after <ELAPSED>` beneath ``waited on check `<command>` ``. The single check-in reads `still not met after <ELAPSED>` and is evaluated between runs; a blocking run defers it and is never interrupted for it.

### File: `--file`

`rimz wait --file build.log` polls once per second and fires when existence, size, modification time, or the file itself (device and inode) differs from the mark recorded when armed. Creation, deletion, growth, truncation, same-size rewrites with a changed modification time, and a replacement at the path (an atomic rename or a rotation) count. Relative paths resolve against the arming shell's current directory and are stored absolute. An absent file is accepted; a directory is refused.

With `--grep 'READY'`, the watcher reads from the file's size at the arm point, or byte zero if it was absent. Existing lines do not count. Matching is literal and case-sensitive, on newline-terminated lines with a trailing carriage return trimmed. A line straddling the arm point counts only from the arm byte; an unterminated last line waits for its newline. If a different file appears at the path (a rename rotation or an atomic rewrite) or the file shrinks below the cursor, reading restarts at byte zero. One case goes unseen: the same file truncated in place and regrown past the old cursor within one poll, as `cmd > build.log` rerun can do. Lines before the old cursor are skipped. For a rerun that rewrites its log, remove the file before starting the command, or wait on `--check` instead.

The message starts with `waited on file <path>`, adding ``for `<pattern>` `` with `--grep`. Completion reads `met after <ELAPSED>`; a match adds a one-line preview as ``met after <ELAPSED>: `<line>` `` and writes the whole matched line to the output file. The line stored in the verdict is capped at 4 KiB. A check-in reads `still not met after <ELAPSED>`.

### Watched command: `--run`

The `--run` command string runs through `sh -c` with stdin closed, at the root of the checkout it was armed from (a linked worktree's root when armed from one). It runs in a detached watcher in its own process group, so it outlives the turn that armed it. It keeps the arming agent's environment, so `$TMPDIR` is the agent's temp directory. Stdout and stderr go to the [output file](#the-output-file).

Quote the whole string so the caller's shell does not split it at `&&`, `|`, or `;`; it runs through POSIX `sh`, not bash.

`--on` picks which final outcome delivers a message:

| `--on` | Delivers on |
| --- | --- |
| `any` (default) | every final outcome |
| `fail` | a non-zero exit, death by signal, or a lost watcher |
| `success` | exit 0 |

An outcome `--on` filters out records `skipped` in the loop history and retires the wait without a message.

### Check-ins: `--check-in`

`--timeout` is no longer accepted by `rimz wait`; use `--check-in` instead. It sends one still-running notice and never stops the command. Other commands' `--timeout` flags are unchanged.

`--check-in` sets when a pending process, command, check, or file watch checks in; it never kills anything. RimZ sends one check-in message, including the output file's summary when non-empty, keeps watching, and delivers the final verdict later. The check-in ignores `--on`, happens once per wait, and leaves the wait pending. Polled watches evaluate it between probes. At arm time, a known provider TTL with `harness.cache_keepalive = true` suppresses the default check-in because keepalive pings replace it. An explicit `--check-in` always wins. With no TTL or with keepalive disabled, the default remains `30m`, independent of `loop.default-timeout`. Changing the configuration does not change check-ins already armed, and reaching `harness.cache_keepalive_max` does not restore one: a watch armed without `--check-in` under keepalive stays silent after the last ping.

## The delivered message

A wait arrives as a message from `@rimz` with `Type: WAIT` (see [the message header](./message.md#the-message-header)). It is sent as a steer: it writes into the live turn now instead of waiting for the turn to end, unless the agent has a native prompt open. With a prompt open, the wake parks and lands at the next turn boundary after the prompt is answered. A check-in parks the same way and is canceled when the wait delivers its final outcome; a finish that `--on` filters out delivers nothing, so a parked check-in still lands. Clock and signal deliveries from `rimz loop add --wait` park until the turn ends instead. [`rimz transcript`](./transcript.md) hides wait messages from its rendered view and keeps them in `--json`.

The body never inlines command output; a file pattern match inlines its matched-line preview. It names what was waited on, the verdict, and any non-empty output file with its estimated tokens and line count, and ends with the wait's name in brackets. Only a command watch says `no output` when the file is empty. A timer is one line:

```text
waited 30m [wait-bold-comet]
```

A final command verdict:

```text
waited on `cargo test`
exit 1 after 12m · output: /home/me/.rimz/ws/myrepo-f89e/out/planner/wait-solid-pixel.output (~14k tokens, 1210 lines) [wait-solid-pixel]
```

A check-in adds `· still watching` immediately after the verdict and the follow-up commands. `Another check-in` repeats the wait's recorded `--check-in` value; running it arms a separate timer that neither restarts nor stops the command:

```text
waited on `cargo build`
still running after 30m · still watching · output: /home/me/.rimz/ws/myrepo-f89e/out/planner/wait-solid-pixel.output (~3.4k tokens, 340 lines) [wait-solid-pixel]

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

A silent command reads `· no output` after its verdict and names no file. A `--pid` wait captures no output of its process, so it shows no segment at all unless the watcher itself wrote an error. The token count is an estimate from OpenAI's public `o200k_base` tokenizer (`<1k`, `~1.2k`, `~22k`, `~1.2M`); Claude's tokenizer is not published and typically counts somewhat higher. Past the first 1 MiB the count is scaled from that sample. A command longer than 120 characters is shortened in the middle in the message, receipt, and list. A trailing shell output redirect, including its path and `2>&1`, stays whole even when that makes the preview longer than 120 characters; quoted or escaped `>` is not a redirect. If the redirect leaves no room for a command tail, the command prefix ends at the ellipsis. The stored command and the logs keep it whole.

### The output file

The watcher writes combined stdout and stderr to `~/.rimz/ws/<workspace-dir>/out/<reader>/<name>.output` as they arrive, along with its own startup errors, where `<reader>` is the name of the agent that armed the wait (a petname for a launched child). A check truncates it before each run; a file pattern watch writes its matched line. The message shows that host path in every isolation.

Closing the room keeps the file. `rimz gc` removes it once no watcher runs and the file has not been written for 7 days; `rimz reset --hard` removes it at once. The last 4 KiB of output stay in the loop history for [`rimz loop logs <name>`](./loop.md#loop-logs).

## While a wait is pending

An agent at rest on a timer, signal, condition, or file watch shows `sleeping` (`☾` in the sidebar) instead of `idle` or `success`. A PID wait, watched command, polled check, unfinished team, or working child instead shows `running`, as does a listed background shell. Child waits follow the target's work through further waits; a settled child's report contributes neither state. The [status table](./agents.md#list) gives the precedence. Standing signal subscriptions do not count.

In a supervised run, arming a wait and ending the turn does not finish the run: it stays `running` and keeps its pane until the work ends. The wake starts the next turn in that same run. If the wake is lost and nothing else remains to wake the parked run, it fails with exit `1` and a reason beginning `parked on a wake that never arrived`. The run's timeout still applies ([supervised runs](./agents.md#supervised-runs--p)).

A sleeping agent still takes messages: a normal message starts a turn and leaves the wait armed. A team member with a pending wait does not emit `team.idle`. The sidebar card counts pending waits as `⧖ waits (N)` and lists each one when expanded; [the card](../../interface/sidebar.md#the-card) shows how.

## List pending waits

`rimz wait list` (alias `ls`, or bare `rimz wait`) shows your own waits from an agent pane. Its header is `<handle> · N waits`, or `<handle> · no waits`. Each self wait has its name, state, and trigger, without terminal-width clipping. Lost watches come first, then watches past check-in, then longest elapsed first, with names breaking ties. A label replaces the watch's subject; otherwise the trigger is the PID, command, check, or file subject, or `once at HH:MM` for a timer. A worktree suffix (`· in <worktree>`) appears only when the row belongs to a different checkout from the reading command.

| Row | Displayed state |
| --- | --- |
| Live watcher | `watching 12m`, adding `· check-in in 38m` when a limit is ahead, or `· past check-in at 30m` once it has passed. |
| Watcher not started | `starting` through 30 seconds after arming. |
| Missing watcher after grace | `lost · armed 41m ago`; this is the arm age, not when the watcher vanished. |
| Timer | `pending · fires in 28m`. Timer firing rounds up to the minute. |
| Paused | `paused · resumes in 2h`. |
| Disabled | `disabled`. |

Loop and team subscriptions targeting your session are not wait lines. They appear on `also woken by <triggers>[ in <worktree>] (loop <name> or team <name>)`, grouped by owner. That line follows your waits, or joins the header when you have none. A plain `loop add --wait` row is a loop subscription named after itself.

From a user shell, text is read-only and starts with `<home-relative project root> · whole room · N waits · M subscriptions`, omitting zero count parts. With no rows it ends in `no waits` and prints no body. Otherwise a blank line separates the header from the same room body as [`loop list --all`'s WAKE-UPS](./loop.md#loop-list): self-wait cards, children nested under their ancestors with their petnames, followed by team and loop owner summaries. Cards use the same current addressable handles as wait receipts; a target that is no longer addressable, including a session shadowed by the pane's current owner, falls back to the recorded target handle. Hand-written configuration deliveries appear in the room view too.

`--json` keeps every instance delivery targeting the caller's session, including loop and team subscriptions, or from a user shell the room view's rows. Keys remain in order: `name`, `type`, `state`, `target`, `trigger`, optional `label`, optional `dir`, `elapsed`, `elapsed_s`, `check_in`, `check_in_s`, optional `watcher_pid`, then always `owner` and `parent`. `type` is `timer`, `watch`, `check`, `pid`, `file`, `signal`, `condition`, or `schedule`; `state` is `pending`, `waiting`, `watching`, `starting`, `lost`, `disabled`, or `paused`. Elapsed and check-in values are labels plus integer seconds, always present and `null` when not applicable; a timer's check-in value is its requested delay. `watcher_pid` appears only while a live watcher holds the row's lock. There is no `age` field. `dir` remains the recorded home-relative path, including for timers and signals. Watch JSON triggers retain the directory suffix; paused JSON triggers retain the resume suffix.

`owner` is `self`, `loop:<task>`, or `team:<team instance>`. `parent` is the target's launched parent's full handle over the current addressable roster, or `null` for a root agent or a target or parent that is not addressable. The arm and cancel receipts' `pending` arrays equal this JSON list.

## Cancel a wait

`rimz wait cancel <name>` cancels any one pending wait or subscription targeting the caller. `rimz wait cancel --all` cancels only waits you armed with `rimz wait`, leaving loop and team subscriptions intact. Give a name or `--all`, not both. Cancel refuses a user shell (`canceling a wait requires an agent RimZ can identify; run this command from an agent pane`) and a name not in the caller's list (``no pending wait named `<name>`; see `rimz wait list` ``).

Cancel removes each selected row, then sends SIGTERM to its watcher's process group, which stops a watched command with it. It prints `canceled <name>, ...` followed by your self view. The canceled line is omitted when `--all` found no self waits. `--json` prints `{"canceled": [...], "pending": [...]}` with all remaining instance deliveries, including subscriptions.

## What a wait writes on your machine

A wait is one row in `~/.rimz/ws/<workspace-dir>/records/loop-instances.json`; it never touches `loop.toml` or project config. Its optional `label` stores the trimmed description. Its `watch` is a command string, `{pid}`, `{check, every, on}`, or `{file, grep?, mark?}`. The file mark records size, modification time, device, and inode; an absent mark means the file was absent at arm time. Every watcher runs in its own process group and holds `locks/loop-watch-<name>.lock` in the room state directory.

The row retires when the timer fires, the watch reaches its final outcome, the wait is canceled, or the target session ends, is lost, or is stopped. A check-in does not retire it. If a watcher dies without reporting, the room's host notices the missing lock after a 30-second grace and delivers the `watcher died` verdict. `rimz gc` removes rows left behind.

Every fire writes loop history, which `rimz loop logs <name>` keeps readable after the wait retires. Once its run ends, `rimz loop show <name>` refuses and points to those logs, using `--root <root>` when the retained runs are in another known room. `logs` keeps its existing room selection, so pass that root there. The mechanics are in [watched commands](../../internals/harness/loops.md#watched-commands) and [waits](../../internals/harness/loops.md#waits).

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
