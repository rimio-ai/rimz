# Wait CLI

`rimz wait` is an alarm an agent sets for itself. The agent arms it and ends its turn. RimZ sends a message back into the same conversation when a delay elapses, an existing process goes away, a watched command exits, a polled check meets its condition, or a file changes or gains a matching line. [`rimz agents wait`](./agents.md#list-and-manage-agents) works in the other direction: it blocks a shell until an agent finishes.

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
rimz wait list                               # your waits, or the whole room from a user shell
rimz wait cancel wait-bold-comet
rimz wait cancel --all
```

The [global flags](../cli.md#global-flags) apply.

## Arm a wait

Give exactly one trigger: `--in`, `--pid`, `--check`, `--file`, or `--run`. With no arming flag at all, `rimz wait` lists pending waits, and `rimz wait --json` is `rimz wait list --json`.

| Flag | Applies to | Meaning |
| --- | --- | --- |
| `--label <TEXT>` | all triggers | Short description: one line, 1 to 60 characters after trimming. It replaces the trigger subject in lists. |
| `--in <DURATION>` | timer | Fire once after this delay. Greater than zero. |
| `--pid <PID>` | process | Fire once this existing process is gone. `1` to `2147483647`. |
| `--run <COMMAND>` | command | Run the command once in a detached watcher and fire on its exit. |
| `--check <COMMAND>` | check | Poll a shell command until it succeeds, or fails with `--on fail`. |
| `--every <DURATION>` | `--check` only | Sleep between runs. Default `1s`, greater than zero and less than `24h`. |
| `--file <PATH>` | file | Wait for a change in existence, size, modification time, or file identity. |
| `--grep <PATTERN>` | `--file` only | Wait for a new newline-terminated line containing this literal, case-sensitive pattern. |
| `--on fail\|success\|any` | `--check` or `--run` | Which outcome delivers. Default `success` for a check, `any` for a command. A check refuses `any`. |
| `--check-in <DURATION>` | `--pid`, `--check`, `--file`, `--run` | When to send one check-in. Greater than zero and less than `24h`. The default is in [check-ins](#check-ins---check-in). |
| `--json` | all | Print the receipt as JSON. |

A duration is a whole number followed by one unit, `s`, `m`, `h`, or `d` (`90s`, `30m`, `1h`). A combined form such as `1h30m` is refused during argument parsing, exit 2.

Each arm mints a name of the form `wait-<adjective>-<noun>`, unique in the workspace. In a crowded namespace RimZ appends a numeric suffix. The receipt names the wait, its trigger, and its target, then shows the caller's self waits, with other subscriptions summarized separately. A new watch usually shows `starting`, because its detached watcher has not yet taken its lock:

```console
$ rimz wait --run 'cargo test'
armed wait-solid-pixel: watch: cargo test → @coder (check-in 30m)
@coder · 2 waits
  wait-bold-comet   pending · fires in 28m  once at 2026-11-08 14:32
  wait-solid-pixel  starting                cargo test
```

The trigger text in the receipt is one of these:

- `in <DURATION>`
- `pid <PID>`
- `watch: <command>`
- `check: <command>`, with ` every <DURATION>` for a non-default interval and ` on fail` for failure polarity
- `file: <absolute path>`, with ` grep: <pattern>` when a pattern is given

A watch receipt ends with `(check-in <DURATION>)` when the wait records a check-in. A timer has no suffix, and neither has a watch armed without a check-in under cache keepalive.

`--json` prints `{"name", "trigger", "target", "pending"}`. `trigger` is the receipt's trigger text. `pending` is the same array that [`rimz wait list --json`](#list-pending-waits) prints, subscriptions included, with their own types, bare trigger subjects, `owner`, and `parent`.

Arming is refused with exit 1 in these cases. An invalid `--label`, `--pid`, `--on` value, or duration exits 2 during argument parsing.

| Error | Cause |
| --- | --- |
| `` `--timeout` is now `--check-in`: it sends one still-running notice and never stops the command `` | `--timeout` passed to `rimz wait`. Other commands' `--timeout` flags are unaffected. |
| `choose exactly one wait trigger: --in, --pid, --check, --file, or --run` | Arming flags without a trigger, or more than one trigger. |
| `--file needs a path` | An empty file path. |
| `--grep requires --file` | A pattern without a file trigger. |
| `--grep needs a pattern` | An empty pattern. |
| `--check needs a command` | An empty or whitespace-only check. |
| `--run needs a command` | An empty or whitespace-only watched command. |
| `--on requires --check or --run` | `--on` with a timer, PID, or file. |
| `--on any has no meaning with --check; it polls until the command succeeds (default) or fails` | A check with `--on any`. |
| `--every requires --check` | An interval on another trigger. |
| `--check-in requires --pid, --check, --file, or --run` | `--check-in` with `--in`. |
| `<flag> must be greater than zero` | Zero for `--in`, `--check-in`, or `--every`. |
| `<flag> must be less than 24h` | `24h` or more for `--check-in` or `--every`. |
| `arming a wait is only available to an agent RimZ can identify; run this command from an agent pane` | Run from a user shell, or from a process RimZ cannot trace to an agent. |
| `nothing was armed: RimZ could not find the calling agent's live session from this process` | The caller's identity is set but no live session is visible, for example when a command sandbox hides the agent's process. The error names the kind and the launch id, or the pane for a legacy caller. |
| `RimZ identified the calling agent but its live session is unavailable` | The calling agent's session has ended. |
| `the calling agent has not registered a real session yet` | The agent has not reported its provider session yet. |
| `cannot watch PID <PID>: permission denied; choose a process owned by your user` | The caller may not signal that process. |
| `--file <path> is a directory; watch a file` | The path names a directory. |
| `reading --file <path>` | The path exists but cannot be read for its size and modification time, for example a permission error. The cause follows. |

A failed arm leaves no wait row. In a provider's command sandbox, an arm can refuse before any write when it cannot see the calling agent's process. When the RimZ home is read-only instead, the error keeps its original cause and adds `nothing was armed: this command cannot write the RimZ home`. Both refusals give the same remedy:

1. Run `rimz wait` as its own command, without a chain, pipe, redirect, or `VAR=` prefix. Its [routine permission rule](../agent-support.md#permission-modes) then runs it outside the provider's command sandbox.
2. With [`allow-routine-rimz`](../../guide/configuration.md#routine-rimz-commands) off, approve the sandbox's escalation instead.
3. For Codex, if its routine rules in `rules/rimz.rules` predate the `rimz wait` rule, run `rimz hooks install codex` from a user shell and restart the agent.

## Triggers

### Timer: `--in`

A timer fires once. RimZ adds the delay to the current time and rounds up to the next whole minute, so `rimz wait --in 30m` armed at 14:01:20 fires at 14:32. The list shows the trigger as `once at 2026-11-08 14:32` (date and time in the configured timezone) and the state as `pending · fires in 28m`. Past its minute and not yet fired, the state is `pending`. A timer that no clock saw at its minute fires on the first clock that sees it, however late. Firing needs a clock: the room's sidebar host, or the [loop timer](./loop.md#timer) when no room is open.

### Existing process: `--pid`

`--pid` does what `tail --pid=<PID> -f /dev/null` does, without GNU `tail`. The watcher checks the PID once per second and fires when no process has it. An already-absent PID fires at once.

The watcher sees only whether the PID exists:

- The process's exit status is unavailable. The verdict is `met after <ELAPSED>`, and `--on` is refused. A check-in reads `still not met after <ELAPSED>`.
- An unreaped zombie, or another process that reuses the PID, keeps the wait open.
- No output is captured. The output file stays empty unless the watcher itself fails.
- Canceling stops the watcher only. The process keeps running.

The receipt, the list, and the sidebar read `pid <PID>`. The delivered message starts with `waited on pid <PID>`.

### Polled check: `--check`

`rimz wait --check 'nc -z localhost 3000'` runs the command through `sh -c` with stdin closed, at the root of the checkout it was armed from, until it exits 0. With `--on fail` it waits for a non-zero exit or death by signal. `--on any` is refused: use `--run` for a single run.

The first run starts immediately. `--every` is the sleep between completed runs (default `1s`), so a slow run lengthens the cycle. Each run truncates and rewrites the output file, which holds the latest run's stdout and stderr. Exit 126 or 127 means the command cannot run: the wait ends and delivers `exit <CODE> after <ELAPSED>`.

The message starts with ``waited on check `<command>` ``. Completion reads `met after <ELAPSED>`. The check-in reads `still not met after <ELAPSED>` and is evaluated between runs, so a blocking run delays it and is never interrupted for it.

### File: `--file`

`rimz wait --file build.log` checks the file once per second. It fires when existence, size, modification time, or the file itself (device and inode) differs from the mark recorded at arm time. Creation, deletion, growth, truncation, a same-size rewrite with a new modification time, and a replacement at the path (an atomic rename or a rotation) all count. A relative path resolves against the arming shell's current directory and is stored absolute. An absent file is accepted. A directory is refused.

With `--grep 'READY'`, the watcher reads from the file's size at arm time, or from byte zero if the file was absent. Lines already in the file do not count. Matching is literal and case-sensitive, on newline-terminated lines, with a trailing carriage return trimmed from the reported line. A line that straddles the arm point is read only from the arm byte. An unterminated last line waits for its newline. When a different file appears at the path (a rename rotation or an atomic rewrite), or the file shrinks below the read position, reading restarts at byte zero.

One case goes unseen: the same file truncated in place and regrown past the old read position within one second, as a rerun of `cmd > build.log` can do. The lines before the old position are skipped. For a rerun that rewrites its log, remove the file before starting the command, or wait on `--check` instead.

The message starts with `waited on file <path>`, and adds ``for `<pattern>` `` with `--grep`. Completion reads `met after <ELAPSED>`. A match adds a one-line preview, ``met after <ELAPSED>: `<line>` ``, and writes the whole matched line to the output file. The line stored in the verdict is capped at 4 KiB. A check-in reads `still not met after <ELAPSED>`.

### Watched command: `--run`

The `--run` command string runs once through `sh -c` with stdin closed, at the root of the checkout it was armed from (a linked worktree's root when armed from one). It runs under a detached watcher in the watcher's own process group, so it outlives the turn that armed it. It keeps the arming agent's environment, so `$TMPDIR` is the agent's temp directory. Stdout and stderr go to the [output file](#the-output-file).

Quote the whole string so the caller's shell does not split it at `&&`, `|`, or `;`. It runs through POSIX `sh`, which may not be bash.

`--on` picks which final outcome delivers a message:

| `--on` | Delivers on |
| --- | --- |
| `any` (default) | every final outcome |
| `fail` | a non-zero exit, death by signal, or a lost watcher |
| `success` | exit 0 |

An outcome that `--on` filters out is recorded as `skipped` in the loop history and retires the wait without a message.

### Check-ins: `--check-in`

A check-in is one message sent while a process, command, check, or file watch is still pending. It kills nothing. RimZ sends it once per wait, includes the output file's summary when the file is not empty, keeps watching, and delivers the final verdict later. A check-in ignores `--on` and leaves the wait pending. Polled watches evaluate it between probes.

An explicit `--check-in` always applies. Without one, RimZ reads the caller's published agent row, or its resolution row when none is published, at arm time:

- With `harness.cache_keepalive = true` and a resolved prompt-cache lifetime that RimZ pings (longer than `harness.prompt_cache_margin`, and at least `harness.keep_warm_min_ttl` unless that floor is `off`), the wait gets no check-in. Keepalive pings take its place.
- Otherwise the check-in is `30m`, independent of `loop.default-timeout`.

A configuration change does not alter check-ins already armed. Reaching `harness.cache_keepalive_max` does not restore one, so a watch on a command that never exits, armed without `--check-in` under keepalive, stays silent after the last ping. Pass `--check-in` to get a check-in in that case.

While an agent rests with waits pending, even when its card displays working, RimZ sends a facts-only prompt-cache keepalive shortly before the cache expires. One ping lists every pending wait, subagent, and launched team, with elapsed times when known. The first ping is never capped. Later pings stop once the next one would start more than `harness.cache_keepalive_max` (default `6h`, `off` for no limit) after the agent's own last request. The agent's next turn of any other kind restarts that clock. The last ping adds `Keepalive limit 6h reached: this is the last ping until your next turn.` A ping missed in its window (no running sidebar, a suspended machine) is skipped, and no more come until the agent's next turn. Delivery is best-effort and never replaces a wait's final wake. The [configuration guide](../../guide/configuration.md#idle-compaction) owns the switch, the lifetimes, the margin, and the floor. The keys are listed in [`rimz config`](./config.md).

## The delivered message

A wait arrives as a message from `@rimz` with `Type: WAIT` (see [the message header](./message.md#the-message-header)). It is sent as a steer: it is typed into the live turn at once, unless the agent has a native prompt open. With a prompt open, the wake parks and lands at the next turn boundary after the prompt is answered. A check-in parks the same way. When the wait delivers its final outcome, RimZ cancels a check-in still parked. A final outcome that `--on` filters out delivers nothing, so a parked check-in still lands. Clock and signal deliveries from `rimz loop add --wait` park until the turn ends. [`rimz transcript`](./transcript.md) hides wait messages from its rendered view and keeps them in `--json`.

The body names what was waited on, the verdict, and any non-empty output file with its estimated tokens and line count, and ends with the wait's name in brackets. It never inlines command output. A file pattern match inlines its matched-line preview. A timer is one line:

```text
waited 30m [wait-bold-comet]
```

A final command verdict:

```text
waited on `cargo test`
exit 1 after 12m · output: /home/me/.rimz/ws/myrepo-f89e/out/coder/wait-solid-pixel.output (~14k tokens, 1210 lines) [wait-solid-pixel]
```

A check-in puts `· still watching` right after the verdict, and ends with two commands. `Another check-in` repeats the wait's recorded `--check-in` value. Running it arms a separate timer, which neither restarts nor stops the command:

```text
waited on `cargo build`
still running after 30m · still watching · output: /home/me/.rimz/ws/myrepo-f89e/out/coder/wait-solid-pixel.output (~3.4k tokens, 340 lines) [wait-solid-pixel]

Stop it: rimz wait cancel wait-solid-pixel
Another check-in: rimz wait --in 30m
```

| Verdict | When |
| --- | --- |
| `exit <CODE> after <ELAPSED>` | The command exited, or a check exited 126 or 127. |
| `killed by signal after <ELAPSED>` | The command died from a signal. |
| `still running after <ELAPSED>` | A command check-in. |
| `met after <ELAPSED>` | A PID, check, or file condition was met. A file pattern match adds the matched-line preview. |
| `still not met after <ELAPSED>` | A PID, check, or file check-in. |
| `watcher died after <ELAPSED>; the command may still be running or may have died with it` | The watcher vanished without reporting. |

A command watch with an empty output file reads `· no output` after its verdict and names no file. A PID, check, or file watch with an empty file shows no output segment. A `--pid` wait captures no output of its process, so its file is empty unless the watcher itself wrote an error.

The token count is an estimate from OpenAI's public `o200k_base` tokenizer, shown as `<1k`, `~1.2k`, `~22k`, or `~1.2M`. Claude's tokenizer is not published and typically counts somewhat higher. Past the first 1 MiB, the count is scaled from that sample.

A command longer than 120 characters is shortened in the middle in the message, receipt, and list. A trailing shell output redirect, with its path and any `2>&1`, stays whole, even when the preview then exceeds 120 characters. A quoted or escaped `>` is not a redirect. When the redirect leaves no room for a command tail, the command prefix ends at the ellipsis. The stored command and the logs keep the whole command.

### The output file

The watcher writes combined stdout and stderr to `~/.rimz/ws/<workspace-dir>/out/<reader>/<name>.output` as they arrive, along with its own startup errors. `<reader>` is the name of the agent that armed the wait (a petname for a launched child). A check truncates the file before each run. A file pattern watch writes its matched line. The message shows this host path in every isolation level.

Closing the room keeps the file. `rimz gc` removes it once no watcher runs and the file has not been written for 7 days. `rimz reset --hard` removes it at once, and so does a new agent that later takes the same handle. The last 4 KiB of output stay in the loop history for [`rimz loop logs <name>`](./loop.md#loop-logs).

## While a wait is pending

An agent at rest on a timer, signal, condition, or file watch shows `sleeping` (`☾` in the sidebar) where it would show `idle` or `success`. A PID wait, watched command, polled check, unfinished team, or working child shows `running` instead, as does a listed background shell. Child waits follow the target's work through further waits. A settled child's report contributes neither state. Standing signal subscriptions do not count. The [status table](./agents.md#list) gives the precedence.

In a supervised run, arming a wait and ending the turn does not finish the run: it stays `running` and keeps its pane until the work ends. The wake starts the next turn in that same run. If the wake is lost and nothing else remains to wake the parked run, the run fails with exit `1` and a reason beginning `parked on a wake that never arrived`. The run's timeout still applies ([supervised runs](./agents.md#supervised-runs--p)).

A sleeping agent still takes messages: a normal message starts a turn and leaves the wait armed. A team member with a pending wait does not emit `team.idle`. The sidebar card counts pending waits as `⧖ waits (N)` and lists each one when expanded ([the card](../../interface/sidebar.md#the-card)).

## List pending waits

`rimz wait list` (alias `ls`, or bare `rimz wait`) shows your own waits from an agent pane. Flags: `--json`.

The header is `<handle> · N waits`, or `<handle> · no waits`. Each self wait is one line with its name, state, and trigger, never clipped to the terminal width. Lost watches come first, then watches past their check-in, then the longest elapsed, with names breaking ties. A label replaces the trigger subject. Otherwise the subject is the PID, command, check, or file, or `once at YYYY-MM-DD HH:MM` for a timer. A worktree suffix (`· in <worktree>`) appears only when the row belongs to a checkout other than the one the command runs in.

| Row | Displayed state |
| --- | --- |
| Live watcher | `watching 12m`, adding `· check-in in 38m` when a check-in is ahead, or `· past check-in at 30m` once it has passed. |
| Watcher not started | `starting`, for up to 30 seconds after arming. |
| Missing watcher after that grace | `lost · armed 41m ago`. The age is from arming, not from when the watcher vanished. |
| Timer | `pending · fires in 28m`, or `pending` once past its minute. |
| Paused | `paused · resumes in 2h`. |
| Disabled | `disabled`. |

Loop and team subscriptions that target your session are not wait lines. They appear on one line, `also woken by <triggers>[ in <worktree>] (loop <name>)` or `(team <name>)`, one group per owner, joined by ` · `. That line follows your waits, or joins the header when you have none. A plain `loop add --wait` row is a loop subscription named after itself.

From a user shell, the text view is read-only and starts with `<home-relative project root> · whole room · N waits · M subscriptions`, omitting a zero count. With no rows the header ends in `no waits` and no body follows. Otherwise a blank line separates the header from the same room body as [`loop list --all`'s WAKE-UPS](./loop.md#loop-list): self-wait cards, with children nested under their ancestors and named by petname, followed by team and loop owner summaries. Cards use the same current addressable handles as wait receipts. A target that is no longer addressable, including a session shadowed by the pane's current owner, falls back to the recorded target handle. Hand-written configuration deliveries appear in the room view too.

`--json` prints every instance delivery that targets the caller's session, loop and team subscriptions included. From a user shell it prints the room view's rows. Each row's keys, in order:

| Key | Value |
| --- | --- |
| `name` | The wait or subscription name. |
| `type` | `timer`, `watch`, `check`, `pid`, `file`, `signal`, `condition`, or `schedule`. |
| `state` | `pending`, `waiting`, `watching`, `starting`, `lost`, `disabled`, or `paused`. |
| `target` | The recorded target handle. |
| `trigger` | The trigger text. A watch keeps its ` · in <dir>` suffix, and a paused row its ` · resumes <when>` suffix. |
| `label` | Present only when set. |
| `dir` | Present only when recorded: the home-relative checkout path, for every type. |
| `elapsed`, `elapsed_s` | Age since arming, as a label and in seconds. `null` when not applicable. |
| `check_in`, `check_in_s` | The check-in, as a label and in seconds. For a timer, the requested delay. `null` when not applicable. |
| `watcher_pid` | Present only while a live watcher holds the row's lock. |
| `owner` | `self`, `loop:<task>`, or `team:<team instance>`. |
| `parent` | The target's launched parent's full handle over the current addressable roster, or `null` for a root agent, or a target or parent that is not addressable. |

The arm and cancel receipts' `pending` arrays are this same list.

When the caller's identity is set but no live session is visible, `list` refuses with exit 1 and `RimZ could not find the calling agent's live session from this process`. It does not fall back to the whole-room view. The error names the kind and the launch id, or the legacy pane, and gives the same [sandbox remedy as arming](#arm-a-wait). An identified agent whose session has ended gets `RimZ identified the calling agent but its live session is unavailable`.

When `list` cannot probe a watcher's lock because the RimZ home is read-only, it refuses with exit 1, keeping the original cause and giving the same remedy. Pending waits stay unchanged.

## Cancel a wait

`rimz wait cancel <name>` cancels one pending wait or subscription that targets the caller. `rimz wait cancel --all` cancels only the waits you armed with `rimz wait`, and leaves loop and team subscriptions in place. Give a name or `--all`: giving both, or neither, is an argument error, exit 2. Flags: `--all`, `--json`.

| Error (exit 1) | Cause |
| --- | --- |
| `canceling a wait requires an agent RimZ can identify; run this command from an agent pane` | Run from a user shell. |
| ``no pending wait named `<name>`; see `rimz wait list` `` | The name is not in the caller's list. |
| `nothing was canceled: RimZ could not find the calling agent's live session from this process` | The caller's identity is set but no live session is visible. The error names the kind and the launch id, or the legacy pane, and gives the [sandbox remedy](#arm-a-wait). |
| `RimZ identified the calling agent but its live session is unavailable` | The calling agent's session has ended. |
| `nothing was canceled: this command cannot write the RimZ home` | The RimZ home is read-only. The original cause and the sandbox remedy follow. |

Cancel removes each selected row, then sends SIGTERM to its watcher's process group, which stops a watched command with it. If RimZ cannot write the wait's home-wide state, cancel refuses before it removes the row or stops the watcher. It prints `canceled <name>, ...` followed by your self view. The `canceled` line is left out when `--all` found no self waits. `--json` prints `{"canceled": [...], "pending": [...]}`, where `pending` holds every remaining instance delivery, subscriptions included.

## What a wait writes on your machine

A wait is one row in `~/.rimz/ws/<workspace-dir>/records/loop-instances.json`. It never touches `loop.toml` or project config. Its optional `label` stores the trimmed description. Its `watch` is a command string, `{pid}`, `{check, every, on}`, or `{file, grep?, mark?}`. The file mark records size, modification time, device, and inode. An absent mark means the file was absent at arm time. Every watcher runs in its own process group and holds `locks/loop-watch-<name>.lock` in the room state directory.

The row retires when the timer fires, the watch reaches its final outcome, the wait is canceled, or the target session ends, is lost, or is stopped. A check-in does not retire it. If a watcher dies without reporting, the room's host notices the missing lock after a 30-second grace and delivers the `watcher died` verdict. `rimz gc` removes rows left behind.

Every fire writes loop history, which `rimz loop logs <name>` keeps readable after the wait retires. Once its run ends, `rimz loop show <name>` refuses and points to those logs, with `--root <root>` when the retained runs are in another known room. `logs` keeps its own room selection, so pass that root there too. The mechanics are in [watched commands](../../internals/harness/loops.md#watched-commands) and [waits](../../internals/harness/loops.md#waits).

## Recipes

```sh
rimz wait --check 'test -e path'                           # file exists
rimz wait --check '! test -e path'                         # file gone
rimz wait --check 'nc -z localhost 3000'                   # port open
rimz wait --check 'curl -fsS http://localhost:3000/health' # URL healthy
rimz wait --check '! test -e .git/index.lock'              # lock file gone
rimz wait --check 'curl -fsS https://example.com/health' --every 30s
rimz wait --pid 16776                                      # process gone
rimz loop add merged --signal pr.merged --wait --once      # PR merged
```

Use `--every 30s` for remote checks. For CI and PR state, subscribe to [signals](./loop.md#waits-and-checks) instead of polling forge commands.
