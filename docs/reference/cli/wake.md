# Wake CLI

`rimz wake` arms one wakeup for one live agent session: after a delay, when a matching signal is emitted, or when a watched command exits. The wakeup delivers a message through the same durable path as [`rimz message`](./message.md), so the agent takes it at its next turn boundary with all of its context. The message explains itself: it names what fired, on what, and who armed it, and `--prompt` adds an optional note under that. Why an agent sets its own alarm instead of holding a pane open on `sleep` is the [loops guide](../../guide/loops.md#wake-a-running-agent); the scheduler underneath is [loops.md](../../internals/harness/loops.md).

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

`--in` and a watched command fire once and retire. A `--signal` wake is a standing subscription: it stays armed across fires until its quiet window runs out ([below](#a-signal-wake-is-a-standing-subscription)).

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

**`--signal <SELECTOR>`** subscribes to a signal name (`ci.failed`) or to a whole family (`ci.*`), from `rimz events emit`, from a forge transition, or from an agent or team lifecycle transition ([events reference](./events.md#emit-a-signal)). Names are lowercase dot-separated words; a selector is one exact name or one family followed by `.*`, so `*`, `a.b.*`, and `a*` are rejected. Repeat `--match KEY=VALUE` to require top-level payload fields; every pair must equal its field, comparing strings directly and other JSON values by their compact encoding. A signal wake is fired by the emitting process, so it needs no open room, and signals are never replayed: a wake armed after the emit does not see it.

`ci.finished` and its `--match conclusion=…` filter are gone, and arming either is refused with the replacement:

```console
$ rimz wake --signal ci.finished
error: schedule `wake`: ci.finished was replaced by ci.passed, ci.failed, or 'ci.*'; remove --match conclusion
```

**A command after `--`** runs it under a watcher process and wakes when it exits. `--on fail|success|any` filters the outcome, defaulting to `any`; `fail` covers a non-zero exit and a timeout, `success` a zero exit. `--timeout <DURATION>` stops watching after that long, defaulting to `59m`. The command runs through `sh -c` at the project root with stdin closed, and its combined stdout and stderr ride along as a tail (16 KiB in the delivered message, 4 KiB in the stored record). The `--on` filter applies at delivery, not at arming: an outcome the filter rejects records `skipped` in the run log and retires the one-shot wake without delivering a message.

### A signal wake is a standing subscription

A branch that fails CI at 14:10 may fail again at 14:35, after another push. So a `--signal` row is not consumed when it fires. It keeps listening, and `--timeout` (default `59m`, chosen to match the provider prompt cache) is a quiet window rather than a lifetime:

- Every observation of the subscribed family restarts the window, whether or not it delivered a message.
- When the window runs out, the row is removed. A subscription that observed nothing at all delivers one last message saying so (`<name> expired: no ci.failed on /home/you/code/app-feat-x in 59m`, naming the selector and the first scope among `branch`, `path`, `instance`, `team`, `handle`, and `session`); a subscription that observed anything retires silently and records `expired` with no message.

**Observe, then deliver or skip.** The family is the first name segment. A subscription to `ci.failed` observes every `ci.*` whose `--match` fields match: the same name delivers a wake, another member of the family records `skipped` in the run log, delivers nothing, and restarts the quiet window. A `ci.*` selector delivers every member. The same rule covers custom emits: a wake on `deploy.failed` treats `deploy.finished` as a sibling observation. A signal from another family, or one whose payload fails a `--match`, is ignored entirely and leaves the window alone.

**Arming twice is idempotent.** Arming a signal wake whose target, selector, matches, and project root equal a live row restarts that row's clock instead of minting a second one, and keeps the row's existing note and timeout (cancel and re-arm to change them):

```console
$ rimz wake --signal ci.failed
already listening: wake-still-path (59m left)
```

With `--json` the same call prints the ordinary receipt, naming the row it restarted.

**Expiry runs on the room's clock.** The elder tick (or the [loop timer](./loop.md#timer)) notices a window that has run out and hands the row to a detached runner, which claims it under the instance-store lock, re-checks the deadline, and removes it. An observation or a re-arm between the tick and the claim wins, and the subscription stays. Nothing sits in a process waiting on a signal wake.

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

The message opens with one headline: the wake's name, what fired, the subject, and who armed it and when. `armed by you` means the target armed it itself, `armed by @handle` names another agent, and `armed from the shell` means a human did.

```text
wake-still-path fired: ci.failed on feat-x (PR #91), armed by you at 14:02
{"branch":"feat-x","checks_url":"https://github.com/you/app/commit/9f2c1ab/checks","head":"9f2c1ab","number":91,"path":"/home/you/code/app-feat-x","repo":"you/app","signal":"ci.failed"}
```

Evidence follows on the next line, and its shape depends on the trigger:

| Trigger | Headline | Evidence |
| --- | --- | --- |
| `--in 30m` | `wake-still-path fired: 30m elapsed, armed by you at 14:02` | none |
| a watched command | ``wake-solid-pixel fired: `gh run watch --exit-status` exited 1 after 12m, armed by you at 14:02`` | the command's output tail |
| a signal | `wake-still-path fired: ci.failed on feat-x (PR #91), armed by you at 14:02` | the payload as one compact JSON line, carrying a `signal` field that names what fired |
| an expired subscription | `wake-still-path expired: no ci.failed on /home/you/code/app-feat-x in 59m` | none |

A watched command that timed out reads `timed out after 59m`, and a watcher that died before its command finished reads `watcher lost` with the detail as its evidence. A `rimz loop fire` of a signal- or command-triggered wake row reads `manual fire`.

`--prompt`, or the contents of `--prompt-file <PATH>`, is appended verbatim after a blank line: a note to yourself, useful when several waits are in flight and the headline alone will not say what to do next. Nothing in it is substituted or rewritten (`{{key}}` placeholders are gone; braces are delivered as typed). A relative `--prompt-file` path resolves against the machine config directory, `~/.config/rimz/`, so prefer an absolute path.

Delivery is a durable message from `@rimz` carrying a `Type: WAKE` header, gated on the receiver's next `done` boundary and inheriting the `[harness] smart_compact` default. An idle agent takes it at once, a working agent at its next boundary, and a session that has ended by then records `target gone`. The wake's run record links the message id, so `rimz message show <id>` and `rimz loop logs <name>` reach the same delivery. Delivered wakes are recorded in the transcript log but hidden from the rendered view; [`rimz transcript --json`](./transcript.md) keeps them.

## Wait for the outcome instead

`--wait` requires a watched command. It arms the wake, then blocks until the run record lands, polling every 500 ms; `--wait=<DURATION>` bounds the wait. When the message is still queued, the join cancels it with the reason `joined inline`; a message that already reached the pane cannot be recalled. After the receipt line it prints `<name>: <result>`, then ` · exit <code>` or ` · timed out`, then the command output:

```console
$ rimz wake --on fail --wait -- true
armed wake-solid-pixel: watch: true → @mint-lagoon
wake-solid-pixel: skipped · exit 0
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
wake-still-path     listening · 41m left  @mint-lagoon  18m  on ci.failed [path=/home/you/code/app-feat-x]
```

The list covers pending wakes for the current project root. Called from an agent pane it shows only the wakes aimed at that agent; from a user shell it shows every wake for the root. `TRIGGER` renders the selector and the matches RimZ defaulted in. `AGE` is how long a subscription has been armed or a watcher has been running, and `STATE` is `due <HH:MM>` for a delay, `listening · <n>m left` for a signal subscription's remaining quiet window, `watching pid <PID>` while a watcher holds its lock, and `watcher lost` when no process holds it. `--json` emits the same five fields per row.

`rimz wake cancel <name>` removes the wake and stops its watcher with SIGTERM. It refuses a name that is not a pending wake for this root, and an agent may cancel only a wake aimed at itself.

## What a wake writes on your machine

An armed wake is a loop task in machine state (`~/.local/state/rimz/loop-instances.json`), never in your `loop.toml`, and it appears in `rimz loop list` with source `state`. A signal subscription's row is rewritten durably on every observation, because its deadline and last-observation stamp move; the row is removed when it expires or is canceled. A watched command additionally starts one detached `rimz wake watch <name>` process holding `loop-watch-<name>.lock` in the workspace runtime directory; a signal subscription starts no process at all. Every fire, skip, and expiry appends one record to the loop run log, so `rimz loop show <name>` and `rimz loop logs <name>` read a wake's history like any other task until the entry retires.

If the watcher process dies before its command finishes, the room's elder notices the missing lock after a 30-second grace and fires the wake with a `watcher lost` outcome instead of leaving it pending forever. `rimz gc` reaps wake rows whose pinned session has left the agent snapshot, stopping their watchers as it goes.

The recurring counterpart is [`rimz loop add --signal`](./loop.md#signals), which subscribes a standing task to the same signals with no quiet window.
