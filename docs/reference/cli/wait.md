# Wait CLI

`rimz wait` is an alarm an agent sets for itself. The agent arms a wait and ends its turn, and RimZ sends a message back into the same conversation when the trigger fires. Only an agent RimZ can identify (from its launch environment or process ancestry) can arm or cancel a wait, and every wait wakes that agent's own live session.

Three other commands sit near this one. [`rimz agents wait`](./agents.md#list-and-manage-agents) blocks a shell until an agent finishes. A team wakes its members on a signal through [role bindings](./teams.md#signal-bindings-at-launch). A [loop task](./loop.md#add-a-task) runs a command, such as `rimz message`, on a clock, a signal, or a condition. The [loops guide](../../guide/loops.md#wake-a-running-agent) teaches when to use each.

```sh
rimz wait --in 30m                           # a delay
rimz wait --at 14:30                         # a clock time
rimz wait --when pr=merged                   # a state expression
rimz wait --pid 16776                        # an existing process
rimz wait --check 'nc -z localhost 3000'     # poll until the port opens
rimz wait --file build.log                   # any change to a file
rimz wait --file build.log --grep 'READY'    # a new matching line
rimz wait --run 'gh run watch --exit-status' # a watched command
rimz wait --on fail --run 'cargo test'       # deliver only if the command fails
rimz wait --check-in 1h --run 'cargo build'  # one check-in after 1h
rimz wait list                               # your waits, or the whole room from a user shell
rimz wait cancel wait-bold-comet
rimz wait cancel --all
```

The [global flags](../cli.md#global-flags) apply.

## Arm a wait

Give exactly one trigger: `--in`, `--at`, `--when`, `--pid`, `--check`, `--file`, or `--run`. The last four start a watch, a detached watcher process that checks a process, a command, or a file. With no arming flag at all, `rimz wait` lists pending waits, and `rimz wait --json` is `rimz wait list --json`.

| Flag | Applies to | Meaning |
| --- | --- | --- |
| `--label <TEXT>` | all triggers | Short description: one line, 1 to 60 characters after trimming. In lists it replaces a watch's subject, and follows any other subject as ` · <label>`. |
| `--in <DURATION>` | timer | Fire once after this delay, rounded up to the next whole minute. Greater than zero. |
| `--at <TIME>` | clock | Fire once at this time: `HH:MM` (today, or tomorrow once past), `<weekday> HH:MM`, or `YYYY-MM-DD HH:MM`. |
| `--when <EXPR>` | condition | Fire once when the state expression reads true. Repeat the flag to AND clauses. |
| `--pid <PID>` | process | Fire once this existing process is gone. `1` to `2147483647`. |
| `--run <COMMAND>` | command | Run the command once in a detached watcher and fire on its exit. |
| `--check <COMMAND>` | check | Poll a shell command until it succeeds, or until it fails with `--on fail`. |
| `--every <DURATION>` | `--check` only | Sleep between runs. Default `1s`, greater than zero and less than `24h`. |
| `--file <PATH>` | file | Fire on a change in existence, size, modification time, or file identity. |
| `--grep <PATTERN>` | `--file` only | Fire on a new newline-terminated line containing this literal, case-sensitive pattern. |
| `--on fail\|success\|any` | `--check` or `--run` | Which outcome delivers. Default `success` for a check, `any` for a command. A check refuses `any`. |
| `--check-in <DURATION>` | `--pid`, `--check`, `--file`, or `--run` | When to send one check-in. Greater than zero and less than `24h`. The default is in [check-ins](#check-ins---check-in). |
| `--json` | all | Print the receipt as JSON. |

A duration is one whole number and one unit, `s`, `m`, `h`, or `d` ([durations](../cli.md#durations)). A compound value such as `1h30m` is an argument error.

Each arm mints a name of the form `wait-<adjective>-<noun>`, unique in the workspace, with a numeric suffix in a crowded namespace. The receipt names the wait, its trigger, and the target, then prints your [list](#list-pending-waits):

```console
$ rimz wait --in 30m
armed wait-simple-route: in 30m → @coder#probe
@coder#probe · 1 wait
  wait-simple-route  pending · fires in 30m  once at 2026-11-16 00:19
$ rimz wait --run 'cargo test'
armed wait-choice-cipher: watch: cargo test → @coder#probe
@coder#probe · 2 waits
  wait-choice-cipher  starting                cargo test
  wait-simple-route   pending · fires in 30m  once at 2026-11-16 00:19
```

A new watch usually lists as `starting`, because its detached watcher has not yet taken its lock. The trigger text in the receipt is one of these:

- `in <DURATION>`
- `once at <YYYY-MM-DD HH:MM>` for `--at`
- `when <expression>`, with repeated clauses joined by ` && `
- `pid <PID>`
- `watch: <command>`
- `check: <command>`, with ` every <DURATION>` for a non-default interval and ` on fail` for failure polarity
- `file: <absolute path>`, with ` grep: <pattern>` when a pattern is given

A watch receipt ends with `(check-in <DURATION>)` when the wait records a check-in. The `cargo test` watch above has none, because cache keepalive covers that agent ([check-ins](#check-ins---check-in)). A `--when` receipt adds a `now:` line, described under [state expression](#state-expression---when). A `--when` that repeats one of your pending waits arms nothing: it prints `already waiting <name>: when <expression> → <handle>`, then your list ([`--when`](#state-expression---when)).

`--json` prints `{"name", "trigger", "target", "pending"}`, adding `"existing": true` when a `--when` named an existing wait. `trigger` is the receipt's trigger text. `pending` is the array [`rimz wait list --json`](#list-pending-waits) prints.

### Refusals

Each of these prints `error: ` and the message, exits 1, and arms nothing. An invalid `--label`, `--pid`, or `--on` value, or a duration that does not parse, is an argument error instead (exit 2).

| Error | Cause |
| --- | --- |
| `` `--timeout` is now `--check-in`: it sends one still-running notice and never stops the command `` | `--timeout` passed to `rimz wait`. |
| `choose exactly one wait trigger: --in, --at, --when, --pid, --check, --file, or --run` | Arming flags without a trigger, or more than one trigger. |
| `--file needs a path` | An empty `--file`. |
| `--grep requires --file` | A pattern without a file trigger. |
| `--grep needs a pattern` | An empty pattern. |
| `--check needs a command` | An empty or whitespace-only check. |
| `--run needs a command` | An empty or whitespace-only watched command. |
| `--on requires --check or --run` | `--on` with a timer, clock, condition, PID, or file. |
| `--on any has no meaning with --check; it polls until the command succeeds (default) or fails` | A check with `--on any`. |
| `--every requires --check` | An interval on another trigger. |
| `--check-in requires --pid, --check, --file, or --run` | `--check-in` with `--in`, `--at`, or `--when`. |
| ``--at `<value>` is not a time; use `HH:MM`, `YYYY-MM-DD HH:MM`, or `<weekday> HH:MM` `` | An `--at` value in none of the three forms. |
| ``--at `<value>` is in the past; choose a later time`` | A dated `--at` that has passed. |
| `<kind> chooses its account per launch, so the room has no <kind> account whose windows a wait can read; window.* terms do not support <kind>` | A `window.*` term in `--when`, from a provider kind that RimZ launches on an exact managed account. |
| ``cannot resolve the room's <kind> account; run `rimz accounts list` `` | A `window.*` term, when the room has no account for your provider kind. |
| ``no current <kind> <span> window reading; open the room's sidebar or run `rimz accounts --refresh` `` | A `window.*` term, when the room's account has no reading, or its reading of that window has a reset that has already passed. |
| `<kind> has no <span> window` | A `window.*` term, when the account's reading has no window of that span. |
| `--when: clause <N>, position <M>: <fix>` | A `--when` expression that does not parse, or names a team stage no team defines. |
| ``--when: `<key>` cannot be read in <checkout>: no supported forge for this checkout; RimZ reads a git checkout whose `origin` remote is on github.com (through `gh`) or on a host whose name contains gitea, forgejo or codeberg (through `tea`), so wait on something else here`` | A `pr` or `ci` term, when the open room's last reading of your checkout found no supported forge ([state expression](#state-expression---when)). |
| `<flag> must be greater than zero` | Zero for `--in`, `--check-in`, or `--every`. |
| `<flag> must be less than 24h` | `24h` or more for `--check-in` or `--every`. |
| `arming a wait is only available to an agent RimZ can identify; run this command from an agent pane` | Run from a user shell, or from a process RimZ cannot trace to an agent. |
| `nothing was armed: RimZ could not find the calling agent's live session from this process` | The caller's identity is set but no live session is visible, for example when a command sandbox hides the agent's process. The error names the kind and the launch id, or the pane for a legacy caller. |
| `RimZ identified the calling agent but its live session is unavailable` | The calling agent's session has ended. |
| `the calling agent has not registered a real session yet` | The agent has not reported its provider session yet. |
| `cannot watch PID <PID>: permission denied; choose a process owned by your user` | The caller may not signal that process. |
| `--file <path> is a directory; watch a file` | The path names a directory. |
| `reading --file <path>` | The path exists but its size and modification time cannot be read, for example for a permission error. The cause follows. |

### Inside a provider's command sandbox

A provider's command sandbox can hide the calling agent's process, which refuses the arm before any write, or make the RimZ home read-only, which adds `nothing was armed: this command cannot write the RimZ home` to the original cause. `wait list` and `wait cancel` refuse the same two ways. The remedy is the same for all of them:

- Run `rimz wait` as its own command, without a chain, pipe, redirect, or `VAR=` prefix. Its [routine permission rule](../agent-support.md#permission-modes) then runs it outside the provider's command sandbox.
- With [`allow-routine-rimz`](../../guide/configuration.md#routine-rimz-commands) off, approve the sandbox's escalation instead.
- For Codex, if its routine rules in `rules/rimz.rules` predate the `rimz wait` rule, run `rimz hooks install codex` from a user shell and restart the agent.

## Triggers

### Timer: `--in`

A timer fires once. RimZ adds the delay to the current time and rounds up to the next whole minute, so `rimz wait --in 30m` armed at 14:01:20 fires at 14:32. The list shows the trigger as `once at 2026-11-08 14:32` (date and time in the configured timezone) and the state as `pending · fires in 28m`. Past its minute and not yet fired, the state is `pending`. A timer that no clock saw at its minute fires on the first clock that sees it, however late. Firing needs a clock, the scheduler that fires loop tasks and waits: the room's sidebar host while the room is open, or the [loop timer](./loop.md#timer) when no room is open. The rest of this page calls each pass of it a tick.

### Clock time: `--at`

`rimz wait --at 14:30` fires once at that minute in the configured timezone. A bare `HH:MM` means today, or tomorrow once that minute has passed. `<weekday> HH:MM` means the next such day, today included while the time is ahead. `YYYY-MM-DD HH:MM` must lie ahead. The receipt and the list show `once at 2026-11-08 14:30`, and the state is `waiting`. Like a timer, it needs a clock, and a clock that missed the minute fires it on its first tick.

### State expression: `--when`

`rimz wait --when pr=merged` fires once when the expression reads true. It takes the [condition grammar of `rimz loop add --when`](./loop.md#conditions) (`pr`, `ci`, `team.stage`, `window.*`, and the rest) without a `--for` hold. Repeated `--when` flags join with `&&`. The scope is the checkout you arm from.

The scheduler records the condition's first reading and fires on its next tick when the condition still holds, so a `window.*` or `team.stage` condition true at arm time fires on the second tick. A `pr` or `ci` key also waits for a sidebar probe at or after that first reading, so a pull request merged before the arm fires only after the sidebar probes it again ([forge freshness](./loop.md#forge-freshness)). A `window.*` term reads the room's current account for your provider kind, which can differ from the account your own session runs on. It is refused when that window cannot be read or its reading's reset has passed. To wake when a spent window has room again, wait on `--when 'window.5h.left>=50'` (or `window.7d.left`) with the room your work needs: it fires as soon as the window has that much room, which may be before the window resets. While the window is short of that room, the receipt's `now:` reading shows when it resets, when the provider reports one, in the configured timezone: `window.5h.left: 8 (resets 2026-11-17 13:52)`.

The receipt's `now:` line gives the condition's state, `waiting` while it reads false, `unknown` while an unread key leaves it undecided, or `due` when it already holds, then each key's reading, or for an unread `pr` or `ci` key the reason it is unread:

```console
$ rimz wait --when pr=merged
armed wait-chief-arc: when pr=merged → @coder#probe
  now: waiting · pr: open
@coder#probe · 3 waits
  wait-chief-arc      waiting                 when pr=merged
  wait-choice-cipher  watching 0s             cargo test
  wait-simple-route   pending · fires in 30m  once at 2026-11-16 00:19
```

`pr` and `ci` are read by the room's sidebar. With the room closed they read as unread, the wait stays armed until a sidebar reads them true, and the `now:` line says so:

```text
  now: unknown · pr: readings paused (room closed)
```

While the room is open, the reason names why a key stays unread: `not checked yet`, `no supported forge for this checkout`, `the checkout's HEAD is detached, so it has no branch to read`, a failing lookup, or no pull request yet. `rimz wait list` shows the same reasons on a waiting condition row, and `rimz loop show <wait-name>` prints a reason for every missing reading.

When the room's last reading already says your checkout has no supported forge (the scope is not in a git checkout or its repository is gone, the checkout has no `origin` remote or one RimZ cannot parse, or the `origin` host is neither `github.com` nor a host whose name contains `gitea`, `forgejo`, or `codeberg`; a supported host whose CLI, `gh` or `tea`, is missing or failing reads as a failing lookup instead), `--when` refuses to arm a condition with any `pr` or `ci` term, a term joined by `||` included, since that term can never be read. The error names the key, the checkout, and what RimZ reads: a git checkout whose `origin` remote is on `github.com` (through `gh`) or on a Gitea, Forgejo, or Codeberg host (through `tea`). A checkout the sidebar has not read yet, a detached HEAD, and a closed room arm as before. A wait whose checkout turns out to have no supported forge after it is armed stays armed and needs you in [`rimz loop list`](./loop.md#loop-list).

Bare `pr` reads the pull request of your checkout's branch. In a RimZ worktree it ignores a merged or closed pull request opened more than five minutes before the worktree was created, so a branch name reused from older work does not read as `merged`. The pull request the worktree was created from, and one whose creation time the forge does not give, always count. `pr[<branch>]` has no such guard: it accepts whatever pull request the forge returns for that branch name.

A `--when` whose expression and checkout match a pending wait of your own session arms nothing and names that wait.

### Existing process: `--pid`

`--pid` does what `tail --pid=<PID> -f /dev/null` does, without GNU `tail`. The watcher checks the PID once per second and fires when no process has it. An already-absent PID fires at once. The watcher sees only whether the PID exists:

- The process's exit status is unavailable. The verdict is `met after <ELAPSED>`, and `--on` is refused.
- An unreaped zombie, or another process that reuses the PID, keeps the wait open.
- No output is captured, so the output file stays empty.
- Canceling stops the watcher only. The process keeps running.

The receipt reads `pid <PID>`, and the list shows the bare PID. The delivered message starts with `waited on pid <PID>`.

### Polled check: `--check`

`rimz wait --check 'nc -z localhost 3000'` runs the command through `sh -c` with stdin closed, at the root of the checkout it was armed from, until it exits 0. With `--on fail` it waits for a non-zero exit or death by signal. `--on any` is refused: use `--run` for a single run.

The first run starts at once. `--every` is the sleep between completed runs (default `1s`), so a slow run lengthens the cycle. Each run truncates and rewrites the output file, which holds the latest run's stdout and stderr. Exit 126 or 127 means the command cannot run: the wait ends and delivers `exit <CODE> after <ELAPSED>`.

The message starts with ``waited on check `<command>` ``, and completion reads `met after <ELAPSED>`. A check-in is evaluated between runs, so a blocking run delays it and is never interrupted for it.

### File: `--file`

`rimz wait --file build.log` checks the file once per second. It fires when existence, size, modification time, or the file itself (device and inode) differs from the mark recorded at arm time. Creation, deletion, growth, truncation, a same-size rewrite with a new modification time, and a replacement at the path (an atomic rename or a rotation) all count. A relative path resolves against the arming shell's current directory and is stored absolute. An absent file is accepted. A directory is refused.

With `--grep 'READY'`, the watcher reads from the file's size at arm time, or from byte zero if the file was absent, so lines already in the file do not count. Matching is literal and case-sensitive, on newline-terminated lines, with a trailing carriage return trimmed from the reported line. A line that straddles the arm point is read only from the arm byte, and an unterminated last line waits for its newline. When a different file appears at the path (a rename rotation or an atomic rewrite), or the file shrinks below the read position, reading restarts at byte zero.

With `--grep`, one case goes unseen: the same file truncated in place and regrown past the old read position within one second, as a rerun of `cmd > build.log` can do. The lines before the old position are skipped. For a rerun that rewrites its log, remove the file before starting the command, or wait on `--check` instead.

The message starts with `waited on file <path>`, adding ``for `<pattern>` `` with `--grep`. Completion reads `met after <ELAPSED>`. A match adds a one-line preview, ``met after <ELAPSED>: `<line>` ``, and writes the whole matched line to the output file. The line stored in the verdict is capped at 4 KiB.

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

A check-in is one message sent while a process, command, check, or file watch is still pending. It kills nothing. RimZ sends it once per wait, with the output file's summary when the file is not empty, keeps watching, and delivers the final verdict later. A check-in ignores `--on` and leaves the wait pending. It reads `still running after <ELAPSED>` for a command and `still not met after <ELAPSED>` for a PID, check, or file.

An explicit `--check-in` always applies. Without one, RimZ decides at arm time, from what it knows of the calling agent:

- When [cache keepalive](../../guide/configuration.md#idle-compaction) pings that agent, the wait gets no check-in. Keepalive pings take its place.
- Otherwise the check-in is `30m`, independent of `loop.default-timeout`.

A configuration change does not alter check-ins already armed. Keepalive pings stop at `harness.cache_keepalive_max`, and that does not restore a check-in, so a watch on a command that never exits, armed without `--check-in` under keepalive, goes silent after the last ping. Pass `--check-in` to get one in that case.

## List pending waits

`rimz wait list` (alias `ls`, or bare `rimz wait`) shows your own waits from an agent pane. Flags: `--json`.

```console
$ rimz wait list
@coder#probe · 3 waits
  wait-chief-arc      waiting                 when pr=merged
  wait-choice-cipher  watching 0s             cargo test
  wait-simple-route   pending · fires in 30m  once at 2026-11-16 00:19
```

The header is `<handle> · N waits`, or `<handle> · no waits`. Each wait you armed is one line with its name, state, and subject, never clipped to the terminal width. Lost watches come first, then watches past their check-in, then the longest elapsed, with names breaking ties. The subject is the PID, command, check, or file, `once at YYYY-MM-DD HH:MM` for a timer or clock time, or `when <expression>` for a condition. A label replaces a watch's subject and follows any other subject as ` · <label>`. A worktree suffix (`· in <worktree>`) appears only when the row belongs to a checkout other than the one the command runs in.

| Row | Displayed state |
| --- | --- |
| Live watcher | `watching 12m`, adding `· check-in in 38m` when a check-in is ahead, or `· past check-in at 30m` once it has passed. |
| Watcher not started | `starting`, for up to 30 seconds after arming. |
| Missing watcher after that grace | `lost · armed 41m ago`. The age is from arming, not from when the watcher vanished. |
| Timer | `pending · fires in 28m`, or `pending` once past its minute. |
| Clock time or condition | `waiting`. A condition with an unread `pr` or `ci` key adds each key's reason: `waiting · pr: no supported forge for this checkout`. |
| Paused | `paused · resumes in 2h`, after [`rimz loop pause`](./loop.md#enable-disable-and-pause) on the wait's name. |
| Disabled | `disabled`, after [`rimz loop disable`](./loop.md#enable-disable-and-pause) on the wait's name. |

Loop and team subscriptions that target your session are not wait lines. They appear on one line, `also woken by <triggers>[ in <worktree>] (loop <name>)` or `(team <name>)`, one group per owner, joined by ` · `. That line follows your waits, or joins the header when you have none. A delivery written by hand in configuration shows as a loop subscription under its own name.

From a user shell, the view is read-only and starts with `<home-relative project root> · whole room · N waits · M subscriptions`, omitting a zero count. With no rows the header ends in `no waits` (`~/room · whole room · no waits`) and no body follows. Otherwise a blank line follows the header, then the same room body as [`loop list --all`'s WAKE-UPS](./loop.md#loop-list): self-wait cards, with children nested under their ancestors and each handle paired with a differing name, followed by team and loop owner summaries. Cards use the same current addressable handles as wait receipts. A target that is no longer addressable, including a session shadowed by the pane's current owner, falls back to the recorded target handle. Hand-written configuration deliveries appear in the room view too.

`--json` prints every delivery that targets the caller's session, loop and team subscriptions included, or from a user shell the room view's rows. Each row's keys, in order:

| Key | Value |
| --- | --- |
| `name` | The wait or subscription name. |
| `type` | `timer` (`--in`), `schedule` (`--at`), `condition` (`--when`), `pid`, `check`, `file`, `watch` (`--run`), or `signal` (a subscription). |
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
| `unread` | A waiting condition's unread `pr` and `ci` keys, each mapped to its reason. Present only when some key is unread. |

The arm and cancel receipts' `pending` arrays are this same list.

When the caller's identity is set but no live session is visible, `list` refuses with exit 1 and `RimZ could not find the calling agent's live session from this process`, naming the kind and the launch id, or the legacy pane. It does not fall back to the whole-room view. An identified agent whose session has ended gets `RimZ identified the calling agent but its live session is unavailable`. When `list` cannot probe a watcher's lock because the RimZ home is read-only, it refuses with exit 1 and the original cause, and pending waits stay unchanged. Each of these three refusals gives the [sandbox remedy](#inside-a-providers-command-sandbox).

## Cancel a wait

`rimz wait cancel <name>` cancels one pending wait or subscription that targets the caller. `rimz wait cancel --all` cancels only the waits you armed with `rimz wait`, and leaves loop and team subscriptions in place. Give a name or `--all`: both, or neither, is an argument error (exit 2) whose usage line reads `rimz wait cancel <NAME|--all>`. Flags: `--all`, `--json`.

By the time of this cancel, the `cargo test` watch above had finished and retired:

```console
$ rimz wait cancel wait-chief-arc
canceled wait-chief-arc
@coder#probe · 1 wait
  wait-simple-route  pending · fires in 30m  once at 2026-11-16 00:19
```

Cancel removes each selected row, then sends SIGTERM to its watcher's process group, which stops a watched command with it. If RimZ cannot write the wait's home-wide state, cancel refuses before it removes the row or stops the watcher. It prints `canceled <name>, ...` and then your list. `--all` that finds no wait you armed leaves out the `canceled` line and exits 0. `--json` prints `{"canceled": [...], "pending": [...]}`, where `pending` holds every remaining delivery, subscriptions included.

| Error (exit 1) | Cause |
| --- | --- |
| `canceling a wait requires an agent RimZ can identify; run this command from an agent pane` | Run from a user shell. |
| ``no pending wait named `<name>`; see `rimz wait list` `` | The name is not in the caller's list. |
| `nothing was canceled: RimZ could not find the calling agent's live session from this process` | The caller's identity is set but no live session is visible. The error names the kind and the launch id, or the legacy pane, and gives the [sandbox remedy](#inside-a-providers-command-sandbox). |
| `RimZ identified the calling agent but its live session is unavailable` | The calling agent's session has ended. |
| `nothing was canceled: this command cannot write the RimZ home` | The RimZ home is read-only. The original cause and the sandbox remedy follow. |

## The delivered message

A wait arrives as a message from `@rimz` with `Type: WAIT` ([the message header](./message.md#the-message-header)). Every wait armed with `rimz wait` is sent as a steer, whatever its trigger: it is typed into the live turn at once. When the agent has its own question or permission prompt open, the wake parks instead and lands at the next turn boundary after the prompt is answered. A check-in is delivered the same way.

When the wait delivers its final outcome, RimZ cancels a check-in still parked. A final outcome that `--on` filters out delivers nothing, so a parked check-in still lands then. [`rimz transcript`](./transcript.md) hides wait messages from its rendered view and keeps them in `--json`.

The body names what was waited on, the verdict, and any non-empty output file with its estimated tokens and line count, and puts the wait's name in brackets at the end of the verdict line. A label follows the first line's subject as ` · <label>`. Command output is never inlined. A file pattern match inlines its matched-line preview. A timer is one line, `waited <DURATION>`, and a clock time is one line, `waited until <YYYY-MM-DD HH:MM>`:

```text
waited 30m [wait-bold-comet]
waited until 2026-11-08 14:30 · standup [wait-calm-river]
```

A condition names its expression, then `fired after <ELAPSED>`, then each key's reading on its own line:

```text
waited on window.5h.left>=40
fired after 2h [wait-tidy-otter]
window.5h.left: 90
```

A final command verdict:

```text
waited on `cargo test`
exit 1 after 12m · output: /home/me/.rimz/ws/myrepo-f89e/out/coder/wait-solid-pixel.output (~14k tokens, 1210 lines) [wait-solid-pixel]
```

A check-in puts `· still watching` right after the verdict, and ends with two commands. `Another check-in` repeats the wait's recorded `--check-in` value. Running it arms a separate timer, which neither restarts nor stops the command:

```text
waited on `cargo build`
still running after 30m · still watching · output: /home/me/.rimz/ws/myrepo-f89e/out/coder/wait-keen-lathe.output (~3.4k tokens, 340 lines) [wait-keen-lathe]

Stop it: rimz wait cancel wait-keen-lathe
Another check-in: rimz wait --in 30m
```

| Verdict | When |
| --- | --- |
| `exit <CODE> after <ELAPSED>` | The command exited, or a check exited 126 or 127. |
| `killed by signal after <ELAPSED>` | The command died from a signal. |
| `still running after <ELAPSED>` | A command check-in. |
| `fired after <ELAPSED>` | A `--when` expression read true. |
| `met after <ELAPSED>` | A PID, check, or file condition was met. A file pattern match adds the matched-line preview. |
| `still not met after <ELAPSED>` | A PID, check, or file check-in. |
| `watcher died after <ELAPSED>; the command may still be running or may have died with it` | The watcher vanished without reporting. |

A command watch with an empty output file reads `· no output` after its verdict and names no file. A PID, check, or file watch with an empty file shows no output segment.

The token count is an estimate from OpenAI's public `o200k_base` tokenizer, shown as `<1k`, `~1.2k`, `~22k`, or `~1.2M`. Past the first 1 MiB, the count is scaled from that sample.

A command longer than 120 characters is shortened in the middle in the message, the receipt, and the list. A trailing shell output redirect, with its path and any `2>&1`, stays whole, even when the preview then exceeds 120 characters. A quoted or escaped `>` is not a redirect. When the redirect leaves no room for a command tail, the command prefix ends at the ellipsis. The stored command and the logs keep the whole command.

### The output file

The watcher writes combined stdout and stderr to `~/.rimz/ws/<workspace-dir>/out/<reader>/<name>.output` as they arrive, and nothing else. Its own errors and log lines go to the room's `~/.rimz/ws/<workspace-dir>/log/wait-watch.log`, which is emptied into `wait-watch.1.log` beside it at 32 MiB, and a warning repeated from one place in the code is written once a minute by each watcher, with a `suppressed=<n>` count. `<reader>` is the arming agent's name: its `--name`, or the petname RimZ minted. A team member's role handle is not its name, so a wait that `@coder` in a team arms writes under its petname (`out/happy-marker/`). A check truncates the file before each run, and a file pattern watch writes its matched line. The message shows this host path at every isolation level.

Closing the room keeps the file. `rimz gc` removes it once no watcher runs and the file has not been written for 7 days. `rimz reset --hard` removes it at once, and so does a new agent that later takes the same name. The last 4 KiB of output stay in the loop history for [`rimz loop logs <name>`](./loop.md#loop-logs).

## While a wait is pending

An agent at rest with a pending timer, clock time, condition, file watch, or one-shot signal delivery shows `sleeping` (`☾` in the sidebar) where it would show `idle` or `success`. A PID wait, watched command, polled check, unfinished team, or working child shows `running` instead, as does a listed background shell or script. Child waits follow the target's work through further waits. A settled child's report contributes neither state. A standing (repeating) signal subscription does not count. The [status table](./agents.md#list) gives the precedence.

In a supervised run, arming a wait and ending the turn does not finish the run: it stays `running` and keeps its pane until the work ends, and the wake starts the next turn in that same run. If the wake is lost and nothing else remains to wake the parked run, the run fails with exit `1` and a reason beginning `parked on a wake that never arrived`. A run held only for a person's or an agent's message that is then withdrawn completes instead. The run's timeout still applies ([supervised runs](./agents.md#supervised-runs--p)).

A sleeping agent still takes messages: a normal message starts a turn and leaves the wait armed. A team member with a pending wait does not emit `team.idle`. The sidebar card counts pending waits as `⧖ waits (N)` and lists each one when expanded ([the card](../../interface/sidebar.md#the-card)).

## What a wait writes on your machine

A wait is one row in `~/.rimz/ws/<workspace-dir>/records/loop-instances.json`. It never touches `loop.toml` or project config. Its optional `label` stores the trimmed description. Its `watch` is a command string, `{pid}`, `{check, every, on}`, or `{file, grep?, mark?}`. The file mark records size, modification time, device, and inode, and an absent mark means the file was absent at arm time. Every watcher runs in its own process group and holds `locks/loop-watch-<name>.lock` in the room state directory.

The row retires when a timer, clock time, or condition fires, the watch reaches its final outcome, the wait is canceled, or the agent ends, is lost, or is stopped. A restart or molt of the agent keeps the row, and the wake reaches the session that holds the agent when it fires. A check-in does not retire it. If a watcher dies without reporting, the room's sidebar host notices the missing lock after a 30-second grace and delivers the `watcher died` verdict. `rimz gc` removes rows left behind.

Every fire writes loop history, which `rimz loop logs <name>` keeps readable after the wait retires. Once its run ends, `rimz loop show <name>` refuses and points to those logs, with `--root <root>` when the retained runs are in another known room. `logs` keeps its own room selection, so pass that root there too. The mechanics are in [watched commands](../../internals/harness/loops.md#watched-commands) and [waits](../../internals/harness/loops.md#waits).

## Exit codes

| Code | When |
| --- | --- |
| `0` | A wait was armed or an existing `--when` wait named, a list printed, or a cancel finished, including `cancel --all` with nothing to cancel. |
| `1` | A refusal: an [arm refusal](#refusals), a [list refusal](#list-pending-waits), or a [cancel refusal](#cancel-a-wait). Nothing was armed or canceled. |
| `2` | An argument error: an invalid value, a duration that does not parse (a compound one included), or `cancel` with both a name and `--all`, or neither. |

## Recipes

```sh
rimz wait --check 'test -e path'                           # file exists
rimz wait --check '! test -e path'                         # file gone
rimz wait --check 'curl -fsS http://localhost:3000/health' # URL healthy
rimz wait --check '! test -e .git/index.lock'              # lock file gone
rimz wait --check 'curl -fsS https://example.com/health' --every 30s
```

For CI and pull request state, wait on `--when` or fire a [loop task on a signal](./loop.md#signals) instead of polling forge commands.
