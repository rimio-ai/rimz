# Shell CLI

`rimz shell` runs a command for an agent outside its turn. The agent starts the command and ends its turn or goes on with other work, and RimZ sends a message back into the same conversation when the command exits, naming its outcome and its output file. Only an agent RimZ can identify (from its launch environment or process ancestry) can start or cancel a shell, and every shell wakes that agent's own live session. [`rimz watch`](./watch.md) is its counterpart for a state outside the shell, such as CI or a pull request.

Three other commands sit near this one. [`rimz agents wait`](./agents.md#list-and-manage-agents) blocks a shell until an agent finishes. A team wakes its members on a signal through [role bindings](./teams.md#signal-bindings-at-launch). A [loop task](./loop.md#add-a-task) runs a command, such as `rimz message`, on a clock, a signal, or a condition. The [loops guide](../../guide/loops.md#wake-a-running-agent) teaches when to use each.

```sh
rimz shell -- cargo test                         # run a command, wake on its exit
rimz shell --on fail -- cargo test               # deliver only if the command fails
rimz shell --check-in 1h -- cargo build          # one check-in after 1h
rimz shell --label soak -- sleep 600             # a label in lists and the message
rimz shell list                                  # your shells, or the whole room's from a user shell
rimz shell cancel shell-chief-mesa
rimz shell cancel --all
```

The [global flags](../cli.md#global-flags) apply.

## Start a shell

Everything after `--` is the command. With no command and no option but `--json`, `rimz shell` lists your shells, and `rimz shell --json` is `rimz shell list --json`.

| Flag | Meaning |
| --- | --- |
| `--on fail\|success\|any` | Which outcome delivers a message. Default `any` ([which outcome delivers](#which-outcome-delivers---on)). |
| `--label <TEXT>` | Short description: one line, 1 to 60 characters after trimming. In lists it replaces the command, and the message adds it after the command as ` · <label>`. |
| `--check-in <DURATION>` | When to send one check-in. Greater than zero and less than `24h`. The default is in [check-ins](#check-ins---check-in). |
| `--json` | Print the receipt as JSON. |

A duration is one whole number and one unit, `s`, `m`, `h`, or `d` ([durations](../cli.md#durations)). A compound value such as `1h30m` is an argument error.

Each start mints a name of the form `shell-<adjective>-<noun>`, unique in the workspace, with a numeric suffix in a crowded namespace. The receipt names the shell, `shell: <command>`, and the target, then prints your [list](#list-your-shells):

```console
$ rimz shell --label soak -- sleep 600
armed shell-chief-mesa: shell: sleep 600 → @coder#probe
@coder#probe (@smart-beacon) · 1 shell
  shell-chief-mesa  starting  soak
```

A new shell usually lists as `starting`, because its detached watcher has not yet taken its lock. The receipt ends with `(check-in <DURATION>)` when the shell records a check-in, and names a `--on` other than `any` the same way: `(on fail)`, or `(on fail, check-in 1m)` with both. The one above has none, because cache keepalive covers that agent ([check-ins](#check-ins---check-in)):

```console
$ rimz shell --check-in 1m -- cargo build
armed shell-smart-drift: shell: cargo build → @coder#probe (check-in 1m)
@coder#probe (@smart-beacon) · 2 shells
  shell-chief-mesa   watching 11s  soak
  shell-smart-drift  starting      cargo build
```

`--json` prints `{"name", "trigger", "target", "pending"}`. `trigger` is the receipt's trigger text. `pending` is the array [`rimz shell list --json`](#list-your-shells) prints.

### The command

One word after `--` is the command as written (`rimz shell -- 'make && make test'`). Several words are that argv: each is quoted for POSIX `sh` and they are joined with spaces, so `rimz shell -- sh -c 'sleep 3; exit 2'` runs `sh -c 'sleep 3; exit 2'`, and the receipt, lists, and message show that quoted form. The command runs once through `sh -c` with stdin closed, at the root of the checkout it was started from (a linked worktree's root when started from one). It runs under a detached watcher in the watcher's own process group, so it outlives the turn that started it. It keeps the arming agent's environment, so `$TMPDIR` is the agent's temp directory. Stdout and stderr go to the [output file](#the-output-file).

Quote a command that holds `&&`, `|`, `;`, or a redirect as one word, so the caller's shell does not run that part itself. It runs through POSIX `sh`, which may not be bash. A command that cannot start (exit 126 or 127) delivers its exit like any other, with the shell's error in the output file.

Any other wait is written as the command:

```sh
rimz shell -- 'until curl -sf localhost:8080/health; do sleep 2; done'   # a state
rimz shell -- 'tail -n0 -F app.log | grep -m1 -F READY'                 # a line in a log
rimz shell -- 'while kill -0 4242; do sleep 1; done'                    # a process
rimz shell -- 'sleep 1200'                                              # a delay
```

A shell reads as work on the agent's card, so a delay written this way shows `running`, not `sleeping`, and does not survive a reboot.

### Which outcome delivers: `--on`

| `--on` | Delivers on |
| --- | --- |
| `any` (default) | every final outcome |
| `fail` | a non-zero exit, death by signal, or a lost watcher |
| `success` | exit 0 |

An outcome that `--on` filters out is recorded as `skipped` in the loop history and retires the shell without a message.

### Check-ins: `--check-in`

A check-in is one message sent while the command still runs. It kills nothing. RimZ sends it once per shell, with the output file's summary when the file is not empty, keeps watching, and delivers the final outcome later. A check-in ignores `--on` and reads `still running after <ELAPSED>`.

An explicit `--check-in` always applies. Without one, RimZ decides at start time, from what it knows of the calling agent:

- When [cache keepalive](../../guide/configuration.md#idle-compaction) pings that agent, the shell gets no check-in. Keepalive pings take its place.
- Otherwise the check-in is `30m`, independent of `loop.default-timeout`.

A configuration change does not alter check-ins already recorded. Keepalive pings stop at `harness.cache_keepalive_max`, and that does not restore a check-in, so a command that never exits, started without `--check-in` under keepalive, goes silent after the last ping. Pass `--check-in` to get one in that case.

### Refusals

Each of these prints `error: ` and the message, exits 1, and starts nothing. An invalid `--label` or `--on` value, or a duration that does not parse, is an argument error instead (exit 2).

| Error | Cause |
| --- | --- |
| `a shell needs a command` | Options without a command, or an empty or whitespace-only command. |
| `--check-in must be greater than zero` | Zero for `--check-in`. |
| `--check-in must be less than 24h` | `24h` or more for `--check-in`. |
| `arming a shell is only available to an agent RimZ can identify; run this command from an agent pane` | Run from a user shell, or from a process RimZ cannot trace to an agent. |
| `nothing was armed: RimZ could not find the calling agent's live session from this process` | The caller's identity is set but no live session is visible, for example when a command sandbox hides the agent's process. The error names the kind and the launch id, or the pane for a legacy caller. |
| `RimZ identified the calling agent but its live session is unavailable` | The calling agent's session has ended. |
| `the calling agent has not registered a real session yet` | The agent has not reported its provider session yet. |

### Inside a provider's command sandbox

A provider's command sandbox can hide the calling agent's process, which refuses the start before any write, or make the RimZ home read-only, which adds `nothing was armed: this command cannot write the RimZ home` to the original cause. `shell list` and `shell cancel` refuse the same two ways, as do the same commands of [`rimz watch`](./watch.md). The remedy is the same for all of them:

- Run `rimz shell` (or `rimz watch`) as its own command, without a chain, pipe, redirect, or `VAR=` prefix. Its [routine permission rule](../agent-support.md#permission-modes) then runs it outside the provider's command sandbox.
- With [`allow-routine-rimz`](../../guide/configuration.md#routine-rimz-commands) off, approve the sandbox's escalation instead.
- For Codex, if its routine rules in `rules/rimz.rules` predate this RimZ, run `rimz hooks install codex` from a user shell and restart the agent.

## List your shells

`rimz shell list` (alias `ls`, or bare `rimz shell`) shows your own shells from an agent pane. Flags: `--json`. It lists shells only; [`rimz watch list`](./watch.md#list-your-watches) lists watches.

```console
$ rimz shell list
@coder#probe (@smart-beacon) · 1 shell
  shell-chief-mesa  watching 3s  soak
```

The header is `<handle> · N shells` (`1 shell`), or `<handle> · no shells`. Each shell is one line with its name, state, and subject, never clipped to the terminal width. Lost shells come first, then shells past their check-in, then the longest elapsed, with names breaking ties. The subject is the command, or the label when one is set. A worktree suffix (`· in <worktree>`) appears only when the row belongs to a checkout other than the one the command runs in.

| Row | Displayed state |
| --- | --- |
| Live watcher | `watching 12m`, adding `· check-in in 38m` when a check-in is ahead, or `· past check-in at 30m` once it has passed. |
| Watcher not started | `starting`, for up to 30 seconds after the start. |
| Missing watcher after that grace | `lost · armed 41m ago`. The age is from the start, not from when the watcher vanished. |
| Paused | `paused · resumes in 2h`, after [`rimz loop pause`](./loop.md#enable-disable-and-pause) on the shell's name. |
| Disabled | `disabled`, after [`rimz loop disable`](./loop.md#enable-disable-and-pause) on the shell's name. |

From a user shell, the view is read-only and lists every shell in the room under the agent it wakes:

```console
$ rimz shell list
~/room · whole room · 1 shell

  @coder#probe (@smart-beacon)
    shell-chief-mesa  watching 4s  soak
```

The header is `<home-relative project root> · whole room · N shells` (`1 shell`), or `no shells` with no body. Cards use the same current addressable handles as receipts. A target that is no longer addressable, including a session shadowed by the pane's current owner, falls back to the recorded target handle.

`--json` prints your shells, or from a user shell the room view's rows. Each row's keys, in order:

| Key | Value |
| --- | --- |
| `name` | The shell's name. |
| `type` | `shell`. |
| `state` | `watching`, `starting`, `lost`, `disabled`, or `paused`. |
| `target` | The recorded target handle. |
| `trigger` | The label, else the command, with a ` · in <dir>` suffix, and a paused row's ` · resumes <when>` suffix. |
| `label` | Present only when set. |
| `dir` | Present only when recorded: the home-relative checkout path. |
| `elapsed`, `elapsed_s` | Age since the start, as a label and in seconds. |
| `check_in`, `check_in_s` | The check-in, as a label and in seconds. `null` when none is recorded. |
| `watcher_pid` | Present only while a live watcher holds the row's lock. |
| `owner` | `self`. |
| `parent` | The target's launched parent's full handle over the current addressable roster, or `null` for a root agent, or a target or parent that is not addressable. |

The start and cancel receipts' `pending` arrays are this same list.

When the caller's identity is set but no live session is visible, `list` refuses with exit 1 and `RimZ could not find the calling agent's live session from this process`, naming the kind and the launch id, or the legacy pane. It does not fall back to the whole-room view. An identified agent whose session has ended gets `RimZ identified the calling agent but its live session is unavailable`. When `list` cannot probe a watcher's lock because the RimZ home is read-only, it refuses with exit 1 and the original cause, and pending shells stay unchanged. Each of these three refusals gives the [sandbox remedy](#inside-a-providers-command-sandbox).

## Cancel a shell

`rimz shell cancel <name>` cancels one of your pending shells. `rimz shell cancel --all` cancels every shell you started. Give a name or `--all`: both, or neither, is an argument error (exit 2). Flags: `--all`, `--json`.

```console
$ rimz shell cancel shell-chief-mesa
canceled shell-chief-mesa
@coder#probe (@smart-beacon) · 1 shell
  shell-smart-drift  watching 0s · check-in in 1m  cargo build
```

Cancel removes each selected row, writes a `canceled` record to its [loop history](./loop.md), then sends SIGTERM to its watcher's process group, which stops the command with it. If RimZ cannot write the shell's home-wide state, cancel refuses before it removes the row or stops the watcher. It prints `canceled <name>, ...` and then your list. `--all` that finds no shell you started leaves out the `canceled` line and exits 0. `--json` prints `{"canceled": [...], "pending": [...]}`.

| Error (exit 1) | Cause |
| --- | --- |
| `canceling a shell requires an agent RimZ can identify; run this command from an agent pane` | Run from a user shell. |
| ``no pending shell named `<name>`; see `rimz shell list` `` | The name is not one of your shells. A watch's name is refused here too: cancel it with [`rimz watch cancel`](./watch.md#cancel-a-watch). |
| `nothing was canceled: RimZ could not find the calling agent's live session from this process` | The caller's identity is set but no live session is visible. The error names the kind and the launch id, or the legacy pane, and gives the [sandbox remedy](#inside-a-providers-command-sandbox). |
| `RimZ identified the calling agent but its live session is unavailable` | The calling agent's session has ended. |
| `nothing was canceled: this command cannot write the RimZ home` | The RimZ home is read-only. The original cause and the sandbox remedy follow. |

## The delivered message

A shell's outcome arrives as a message from `@rimz` with `Type: WAIT` ([the message header](./message.md#the-message-header)), sent as a steer: it is typed into the live turn at once. When the agent has its own question or permission prompt open, the wake parks instead and lands at the next turn boundary after the prompt is answered. A check-in is delivered the same way. When the shell delivers its final outcome, or a `join` takes it, RimZ cancels a check-in still parked. A final outcome that `--on` filters out delivers nothing, so a parked check-in still lands then. [`rimz transcript`](./transcript.md) hides wait messages from its rendered view and keeps them in `--json`.

The body names the command, the verdict, and any non-empty output file with its estimated tokens and line count, and puts the shell's name in brackets at the end of the verdict line. A label follows the command as ` · <label>`. Command output is never inlined:

```text
waited on `cargo build`
exit 1 after 0s · output: /tmp/rimz-sandbox-MVVdEJ/home/.rimz/ws/room-d7eb/out/smart-beacon/shell-smart-drift.output (<1k tokens, 2 lines) [shell-smart-drift]
```

A check-in puts `· still watching` right after the verdict, and ends with the command that stops the shell:

```text
waited on `cargo build --release > build.log 2>&1; sleep 200`
still running after 1m · still watching · no output [shell-stout-hinge]

Stop it: rimz shell cancel shell-stout-hinge
```

| Verdict | When |
| --- | --- |
| `exit <CODE> after <ELAPSED>` | The command exited. |
| `killed by signal after <ELAPSED>` | The command died from a signal. |
| `still running after <ELAPSED>` | A check-in. |
| `watcher died after <ELAPSED>; the command may still be running or may have died with it` | The watcher vanished without reporting. |

A shell with an empty output file reads `· no output` after its verdict and names no file.

The token count is an estimate from OpenAI's public `o200k_base` tokenizer, shown as `<1k`, `~1.2k`, `~22k`, or `~1.2M`. Past the first 1 MiB, the count is scaled from that sample.

A command longer than 120 characters is shortened in the middle in the message, the receipt, and the list. A trailing shell output redirect, with its path and any `2>&1`, stays whole, even when the preview then exceeds 120 characters. A quoted or escaped `>` is not a redirect. When the redirect leaves no room for a command tail, the command prefix ends at the ellipsis. The stored command and the logs keep the whole command.

### A joined shell

An agent that calls the MCP `join` tool on `shell:<name>` blocks until the command exits and gets the outcome from `join` instead ([the MCP server](./mcp.md#calls)): its verdict, the output file, and the file's size. That outcome then comes as no message: `join` claims it on the shell's row until its own deadline, and a final outcome that finds a live claim is recorded as `joined` in the loop history and sends nothing. A `join` that times out or is interrupted while the command still runs drops its claim, so the message comes as usual; once a final outcome has taken the claim, `join` waits for that outcome instead. An outcome delivered before the claim landed is answered by `join` too, with a hint that it also came as a message. Only the agent the shell's message goes to claims: a `join` by an agent that started that agent blocks and answers the same outcome, and the message still goes to the shell's agent. A `join` on a shell that has already exited answers its last outcome from the loop history. A `join` on a shell that is canceled answers `ended` with a hint that it was canceled, at once, even while it holds the claim.

### The output file

The watcher writes combined stdout and stderr to `~/.rimz/ws/<workspace-dir>/out/<reader>/<name>.output` as they arrive, and nothing else. Its own errors and log lines go to the room's `~/.rimz/ws/<workspace-dir>/log/wait-watch.log`, which is emptied into `wait-watch.1.log` beside it at 32 MiB, and a warning repeated from one place in the code is written once a minute by each watcher, with a `suppressed=<n>` count. `<reader>` is the arming agent's name: its `--name`, or the petname RimZ minted. A team member's role handle is not its name, so a shell that `@coder` in a team starts writes under its petname (`out/smart-beacon/` above). The message shows this host path at every isolation level.

Closing the room keeps the file. `rimz gc` removes it once no watcher runs and the file has not been written for 7 days, and so removes a `wait-<adjective>-<noun>.output` an older RimZ left. `rimz reset --hard` removes it at once, and so does a new agent that later takes the same name. The last 4 KiB of output stay in the loop history for [`rimz loop logs <name>`](./loop.md#loop-logs).

## While a shell runs

An agent at rest with a running shell shows `running` where it would show `idle` or `success`, as it does for a listed background shell or script of its provider, and its sidebar card draws the shell with the same `shell` tag and heading as those ([the card](../../interface/sidebar.md#the-card)). A pending [watch](./watch.md#while-a-watch-is-pending) shows `sleeping` instead. The [status table](./agents.md#list) gives the precedence.

In a supervised run, starting a shell and ending the turn does not finish the run: it stays `running` and keeps its pane until the work ends, and the wake starts the next turn in that same run. If the wake is lost and nothing else remains to wake the parked run, the run fails with exit `1` and a reason beginning `parked on a wake that never arrived`. The run's timeout still applies ([supervised runs](./agents.md#supervised-runs--p)).

An agent with a running shell still takes messages: a normal message starts a turn and leaves the shell running. A team member with a pending shell does not emit `team.idle`. The sidebar card counts pending shells and watches as `⧖ waits (N)`.

## What a shell writes on your machine

A shell is one row in `~/.rimz/ws/<workspace-dir>/records/loop-instances.json`. It never touches `loop.toml` or project config. Its optional `label` stores the trimmed description, and its `watch` is the command string. Every watcher runs in its own process group and holds `locks/loop-watch-<name>.lock` in the room state directory.

The row retires when the command reaches its final outcome, the shell is canceled, or the agent ends, is lost, or is stopped. A restart or molt of the agent keeps the row, and the wake reaches the session that holds the agent when it fires. A check-in does not retire it. If a watcher dies without reporting, the room's sidebar host notices the missing lock after a 30-second grace and delivers the `watcher died` verdict. `rimz gc` removes rows left behind.

A row an older RimZ stored for a process, a polled check, or a file watch is retired the next time RimZ retires the rows of ended sessions, which it does before a signal fires, on an agent's hook, and when an agent's wrapper exits: RimZ stops its watcher, removes the row, and writes a `canceled` loop history record whose error reads `retired: RimZ no longer watches a pid, a check, or a file; run the wait as a command with \`rimz shell\``. The other rows of the file load as before.

Every fire writes loop history, which `rimz loop logs <name>` keeps readable after the shell retires. Once its run ends, `rimz loop show <name>` refuses and points to those logs, with `--root <root>` when the retained runs are in another known room. `logs` keeps its own room selection, so pass that root there too. The mechanics are in [watched commands](../../internals/harness/loops.md#watched-commands) and [waits](../../internals/harness/loops.md#waits).

## Exit codes

| Code | When |
| --- | --- |
| `0` | A shell was started, a list printed, or a cancel finished, including `cancel --all` with nothing to cancel. |
| `1` | A refusal: a [start refusal](#refusals), a [list refusal](#list-your-shells), or a [cancel refusal](#cancel-a-shell). Nothing was started or canceled. |
| `2` | An argument error: an invalid value, a duration that does not parse (a compound one included), or `cancel` with both a name and `--all`, or neither. |
