# Wake CLI

`rimz wake` arms one wakeup for one live agent session: after a delay, when a matching signal is emitted, or when a watched command exits. The wakeup delivers a message through the same durable path as [`rimz message`](./message.md), so the agent takes it at its next turn boundary with all of its context. The message reads as the result of the wait: what was waited on, how it ended, how long it took, and where the full output is; `--prompt` adds an optional note under that. Why an agent sets its own alarm instead of holding a pane open on `sleep` is the [loops guide](../../guide/loops.md#wake-a-running-agent); the scheduler underneath is [loops.md](../../internals/harness/loops.md).

```sh
rimz wake --in 30m                           # wake me once, 30 minutes from now
rimz wake -- gh run watch --exit-status      # wake me when the command exits
rimz wake --signal ci.failed                 # wake me when CI fails on my branch
rimz wake @planner --in 2h --prompt "the migration window is open"
rimz wake --signal 'ci.*' --timeout 4h       # any CI verdict, watched for four hours
rimz wake --on fail -- cargo test            # wake me only if it fails
rimz wake --wait -- cargo build              # block here and print the outcome
rimz wake list
rimz wake list --json
rimz wake cancel wake-bold-comet
```

Exactly one trigger per wake: `--in`, `--signal`, or a command after `--`. Naming two, or none, is an error. Bare `rimz wake` with no arguments lists pending wakeups.

Every wake fires once and retires. `--in` and a watched command have one outcome each; a `--signal` wake ends on the signal it asked for or on its `--timeout` deadline, whichever comes first ([below](#a-signal-wake-is-one-question)).

## Who gets woken

`@TARGET` names the agent to wake, resolved against the live rollup at arm time exactly as [`rimz message`](./message.md) resolves it, and pinned to that one session id. An address with no live match, or a card that has not registered a real session yet, is an error rather than a queued wake.

Omit the target from an agent's own pane and the wake goes back to the calling agent: RimZ identifies the caller from its launch environment or process ancestry and pins its session. From a user shell there is no caller to pin, so the address is required:

```console
$ rimz wake --in 30m
error: arming a wake without an explicit @target is only available to an agent RimZ can identify; from a user shell, pass the live agent address
```

Each wake is named `wake-<adjective>-<noun>` when it is armed, and the receipt names it:

```console
$ rimz wake --in 30m
armed wake-still-path: in 30m → @mint-lagoon
```

`--json` prints that receipt as `{"name":…,"trigger":…,"target":…}` instead.

## Triggers

**`--in <DURATION>`** fires once after the delay. The delay resolves against the configured `timezone` and rounds up to the next scheduler minute; values must be greater than zero and less than 24 hours. A delay wake fires on the room's sidebar elder tick, so the room must be open, or the machine must run the [loop timer](./loop.md#timer).

**`--signal <SELECTOR>`** subscribes to a signal name (`ci.failed`) or to a whole family (`ci.*`), from `rimz events emit`, from a forge transition, or from an agent or team lifecycle transition ([events reference](./events.md#emit-a-signal)). Names are lowercase dot-separated words; a selector is one exact name or one family followed by `.*`, so `*`, `a.b.*`, and `a*` are rejected. Repeat `--match KEY=VALUE` to require top-level payload fields; every pair must equal its field, comparing strings directly and other JSON values by their compact encoding. `--timeout <DURATION>` sets how long the wake waits, defaulting to `59m` and taking the same bounds as `--in` ([below](#a-signal-wake-is-one-question)). A signal wake is fired by the emitting process, so it needs no open room, and signals are never replayed: a wake armed after the emit does not see it.

`ci.finished` and its `--match conclusion=…` filter are gone, and arming either is refused with the replacement:

```console
$ rimz wake --signal ci.finished
error: schedule `wake`: ci.finished was replaced by ci.passed, ci.failed, or 'ci.*'; remove --match conclusion
```

**A command after `--`** runs it under a watcher process and wakes when it exits. `--on fail|success|any` filters the outcome, defaulting to `any`; `fail` covers a non-zero exit and a timeout, `success` a zero exit. `--timeout <DURATION>` stops watching after that long, defaulting to `59m` and taking the same bounds as `--in`. The command runs through `sh -c` at the project root with stdin closed. Its combined stdout and stderr are written to a per-wake log file as they arrive ([below](#the-output-file)), and the last 4 KiB rides along as a tail in the delivered message and in the stored run record. The `--on` filter applies at delivery, not at arming: an outcome the filter rejects records `skipped` in the run log and retires the one-shot wake without delivering a message.

### A signal wake is one question

`rimz wake --signal ci.failed` asks one thing: wake me if CI fails, and let me rest if it passes or is still running. It ends on the signal it asked for or on its deadline, and either ending removes the row.

**The signal arrives.** The wake is delivered and the row is gone, so a second `ci.failed` twenty minutes later reaches nothing armed. A `ci.*` selector ends the same way, on the first verdict of the family.

**A sibling arrives, and nothing happens.** `ci.passed` under a `ci.failed` selector delivers nothing and moves no deadline: the wake stays armed for the answer it asked for. The observation records `skipped` in the run log, where `rimz loop logs <name>` shows it. The same holds for your own emits, so a wake on `deploy.failed` is left armed by `deploy.finished`. A signal from another family, or one whose payload fails a `--match`, is ignored entirely.

**The deadline passes.** `--timeout` (default `59m`, chosen to match the provider prompt cache) is the wake's lifetime, and must be greater than zero and less than 24 hours. At the deadline RimZ reads the room's current PR and CI state for the row's scope, and what it finds decides between two closings.

A `ci` or `pr` wake whose scope already carries a terminal verdict the selector was *not* asking for is answered: the row is removed with no message, and the run record is an `expired` carrying that verdict's name, so `rimz loop logs <name>` prints `signal: ci.passed` on the closed row. You asked to be woken on red, CI is green, and nothing wakes you. Silence needs the scope to be certain, so a row whose `--match` carries a key the cache cannot check (`head`, `repo`, `number`), whose `path` and `branch` disagree, or whose `branch` alone matches two cached worktrees, is never closed silently.

Every other deadline delivers one closing message with the status and the command that resumes the wait:

```text
waited on ci.failed on feat-x (PR #91)
nothing in 59m; wake closed · ci pending on feat-x (PR #91) [wake-still-path]
re-arm: rimz wake --signal ci.failed --match 'path=/home/you/code/app-feat-x'
```

The status after the `·` is what the room's state said at that moment, named on the scope's branch and pull-request number: `ci pending`, `ci passing`, or `ci failing` for a `ci.*` wake, `pr open`, `pr closed`, or `pr merged` for a `pr.*` one, and `no CI seen`, `no PR seen`, or `no PR or CI seen` when the room holds nothing for that half of the scope. A verdict the selector *would* have delivered reads `ci failing on feat-x (PR #91); no matching transition received`: the cache holds the current state and not when it began, so RimZ cannot tell whether that verdict landed before you armed the wake, and it delivers rather than close silently. A wake on any other family (`agent.*`, `team.*`, or your own emits) has no such state to read, so its verdict line carries no status and its wait line names the selector and the first scope among `branch`, `path`, `instance`, `team`, `handle`, and `session`.

The `re-arm:` line is this same wait as a shell-ready command: every stored `--match`, including the ones RimZ defaulted in, a `--timeout` that is not the default, and `--prompt-file` exactly as the original wake stored it, so it resolves to the same file. It names no `@target`, because it is delivered to the target, who re-arms on itself. An inline `--prompt` is not reproduced, since that note is already at the bottom of the same message. Both closings record `expired` in the run log, one with the delivered message id and the other with the answering signal name, so `rimz loop logs <name>` tells them apart afterwards.

**Arming twice replaces the row.** Arming a signal wake whose target, selector, matches, and project root equal a live row replaces that row in place: same name, new deadline, timeout, and note. The receipt is the ordinary `armed <name>: …` line.

**The deadline runs on the room's clock.** The elder tick (or the [loop timer](./loop.md#timer)) notices a deadline that has passed and hands the row to a detached runner, which claims it under the instance-store lock and re-checks the deadline before closing it. Closing therefore needs an open room or the timer, the way `--in` does, even though a fire needs neither; the status it reports comes from the cache the room's sidebar writes as it polls the forge, so a scope no room has polled reads `no PR or CI seen`. A re-arm between the tick and the claim leaves the replacement waiting; `wake cancel` leaves no row. Nothing sits in a process waiting on a signal wake.

### Caller-scoped defaults

A `ci.*` or `pr.*` wake with no `path` or `branch` match is scoped to the caller's own work: RimZ adds `--match path=<caller's worktree>`, so `rimz wake --signal ci.failed` from an agent pane in a worktree waits on that worktree's branch. From a user shell with `@target`, the target's worktree scopes it the same way.

RimZ polls the forge for worktree branches only, so the same wake armed from the root checkout would never fire, and is refused instead:

```console
$ rimz wake --signal ci.failed
error: CI on the root checkout is not watched: RimZ polls the forge for worktree branches. Pass --match branch=<name>, or watch it with: rimz wake -- gh run watch --exit-status
```

A `team.*` wake with no `team` or `instance` match defaults to the caller's own cohort (`--match instance=forge#feat-x`), and is an error when the caller is not in a team. An `agent.*` wake keeps the opposite rule: it must name someone else, otherwise the target's own transition would wake it.

```console
$ rimz wake --signal agent.idle
error: wake on an agent.* signal requires --match handle=<other> or --match session=<other> to avoid waking the target from its own lifecycle signal
```

## The delivered message

The message reads back the wait it came from, in one order: the wait line names what was waited on, the verdict line says how it ended, then the evidence, then your note after a blank line.

```text
waited on `gh run watch --exit-status`
exit 1 after 12m · output: …/wakes/wake-solid-pixel.log [wake-solid-pixel]
<the last 4 KiB of the command's combined output>

<note>
```

Times are elapsed, never wall-clock: an agent reading the message turns later can use `after 12m` and cannot place `14:02`. The bracketed name closes the verdict line (the wait line for a delay), and it is the name `rimz loop logs` and `rimz wake cancel` take.

| Trigger | Wait line | Verdict line | Evidence |
| --- | --- | --- | --- |
| `--in 30m` | `waited 30m [wake-still-path]` | none: the wait line already says it | none |
| a watched command | ``waited on `cargo test` `` | `exit 0 after 4m`, `exit 1 after 12m`, `killed by signal after 3s`, `timed out after 59m`, or `watcher died after 3m; the command may still be running or may have died with it`, each closed by ` · output: <path> [<name>]` | the last 4 KiB of the command's combined output, or `(no output)` |
| a signal | `waited on ci.failed on feat-x (PR #91)` | `fired after 18m [wake-still-path]` | the payload as one compact JSON line, carrying a `signal` field that names what fired |
| a wake its deadline closed | `waited on ci.failed on feat-x (PR #91)`, or `waited on ci.failed on /home/you/code/app-feat-x` when RimZ read no forge state for the scope | `nothing in 59m; wake closed`, then ` · <status>` when it read one, then ` [wake-still-path]` | one `re-arm: <command>` line |

```text
waited on ci.failed on feat-x (PR #91)
fired after 18m [wake-still-path]
{"branch":"feat-x","checks_url":"https://github.com/you/app/commit/9f2c1ab/checks","head":"9f2c1ab","number":91,"path":"/home/you/code/app-feat-x","repo":"you/app","signal":"ci.failed"}
```

A `rimz loop fire` of a signal- or command-triggered wake row reads `fired by hand`. A [`rimz loop add --wake`](./loop.md) row carries no arming stamp, so its verdict line is `fired` with no elapsed.

A wake you armed on yourself says nothing about who armed it. When someone else armed it on you, the message is an instruction rather than your own late result, so it opens with one more line: `@planner armed this wake on you.`, or `armed on you from the shell.` when a human did.

```text
@planner armed this wake on you.
waited on ci.passed on feat-x (PR #91)
fired after 41m [wake-calm-river]
{"branch":"feat-x",…,"signal":"ci.passed"}

the migration window is open
```

`--prompt`, or the contents of `--prompt-file <PATH>`, is appended verbatim after a blank line: a note to yourself, useful when several waits are in flight and the verdict alone will not say what to do next. Nothing in it is substituted or rewritten (`{{key}}` placeholders are gone; braces are delivered as typed). A relative `--prompt-file` path resolves against the machine config directory, `~/.config/rimz/`, so prefer an absolute path.

Delivery is a durable message from `@rimz` carrying a `Type: WAKE` header, gated on the receiver's next `done` boundary and inheriting the `[harness] smart_compact` default. An idle agent takes it at once, a working agent at its next boundary, and a session that has ended by then records `target gone`. The wake's run record links the message id, so `rimz message show <id>` and `rimz loop logs <name>` reach the same delivery. Delivered wakes are recorded in the transcript log but hidden from the rendered view; [`rimz transcript --json`](./transcript.md) keeps them.

### The output file

A watched command writes its whole combined output to `~/.local/state/rimz/workspaces/<workspace-id>/wakes/<name>.log`, and the message carries only the last 4 KiB of it, so whatever the tail cut off is one file read away.

`rimz wake` creates the file when it arms the wake and hands it to the watcher as the watcher's own error stream, so a watcher that fails before the command ever runs leaves its error there instead of dying silently. Stdout and stderr land in the file in arrival order. It lives in state, not in the room's runtime directory, so it outlives the room and the machine's uptime. `rimz gc` removes it once the wake itself is gone, no watcher is running under that name, and its last write is more than 14 days old.

## Wait for the outcome instead

`--wait` requires a watched command. It arms the wake, then blocks until the run record lands, polling every 500 ms; `--wait=<DURATION>` bounds the wait. When the message is still queued, the join cancels it with the reason `joined inline`; a message that already reached the pane cannot be recalled. After the receipt line it prints `<name>: <result>` and the same verdict the message would have carried, then the output file's path, then the tail or `(no output)`:

```console
$ rimz wake --on fail --wait -- true
armed wake-solid-pixel: watch: true → @mint-lagoon
wake-solid-pixel: skipped · exit 0 after 0s
output: …/wakes/wake-solid-pixel.log
(no output)
```

The exit code is `0` only when the wake was delivered (or filtered out by `--on`) and the command exited `0` without timing out; every other outcome, including `target gone` and a non-zero command exit, is `1`. It does not reproduce the command's own exit code. A deadline that passes exits `1` and leaves the wake armed:

```console
$ rimz wake --wait=2s -- sleep 30
armed wake-patient-block: watch: sleep 30 → @mint-lagoon
error: timed out waiting for wake-patient-block; the wake remains armed
```

`--wait --json` prints the run record instead of the receipt and the outcome lines.

## List and cancel

```console
$ rimz wake list
NAME                STATE                 TARGET        AGE  TRIGGER
wake-mint-isle      due 12:44             @mint-lagoon  -    once at 12:44
wake-patient-block  watching pid 1871141  @mint-lagoon  28s  watch: sleep 30
wake-still-path     waiting · 41m left    @mint-lagoon  18m  on ci.failed [path=/home/you/code/app-feat-x]
```

The list covers pending wakes for the current project root. Called from an agent pane it shows only the wakes aimed at that agent; from a user shell it shows every wake for the root. `TRIGGER` renders the selector and the matches RimZ defaulted in. `AGE` is how long a signal wake has been armed or a watcher has been running, and `STATE` is `due <HH:MM>` for a delay, `waiting · <n>m left` for a signal wake's remaining time, `watching pid <PID>` while a watcher holds its lock, and `watcher lost` when no process holds it. `--json` emits the same five fields per row.

`rimz wake cancel <name>` removes the wake and stops its watcher with SIGTERM. It refuses a name that is not a pending wake for this root, and an agent may cancel only a wake aimed at itself.

## What a wake writes on your machine

An armed wake is a loop task in machine state (`~/.local/state/rimz/loop-instances.json`), never in your `loop.toml`, and it appears in `rimz loop list` with source `state`. That command labels signal rows `listening`; `rimz wake list` calls them `waiting`. A signal wake's row is removed on every exit: the delivery it was waiting for, the deadline that closes it, a `target gone` verdict, and `rimz wake cancel`. A watched command additionally writes `wakes/<name>.log` in the workspace state directory ([above](#the-output-file)) and starts one detached `rimz wake watch <name>` process, in its own process group so that the shell or turn that armed the wake cannot take the watcher down with it, holding `loop-watch-<name>.lock` in the workspace runtime directory; a signal wake starts no process at all. Every fire, skip, and expiry appends one record to the loop run log, so `rimz loop show <name>` and `rimz loop logs <name>` read a wake's history even after its entry retires; `show` then displays history without an active schedule.

If the watcher process dies before its command finishes, the room's elder notices the missing lock after a 30-second grace and fires the wake with a `watcher died after <elapsed>` verdict instead of leaving it pending forever; the watched command's own fate is unknown at that point, and the log file is where any error the watcher printed will be. `rimz gc` reaps wake rows whose pinned session has left the agent snapshot, stopping their watchers as it goes.

The standing counterpart is [`rimz loop add --signal`](./loop.md#signals), which subscribes a task to the same signals for as long as the task exists, with no deadline.
