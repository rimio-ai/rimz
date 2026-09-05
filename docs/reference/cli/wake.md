# Wake CLI

`rimz wake` arms one wakeup for one live agent session: after a delay, when a named signal is emitted, or when a watched command exits. The wakeup delivers a prompt through the same durable path as [`rimz message`](./message.md), so the agent takes it at its next turn boundary with all of its context. Each wake fires once and then retires itself. Why an agent sets its own alarm instead of holding a pane open on `sleep` is the [loops guide](../../guide/loops.md#wake-a-running-agent); the scheduler underneath is [loops.md](../../internals/harness/loops.md).

```sh
rimz wake --in 30m --prompt "CI should be done; check the run and merge if green"
rimz wake @planner --in 2h --prompt "the migration window is open"
rimz wake --signal ci.finished --match conclusion=failure --prompt "CI failed on {{branch}}; fix it"
rimz wake -- gh run watch --exit-status      # wake me when the command exits
rimz wake --on fail -- cargo test            # wake me only if it fails
rimz wake --timeout 30m -- ./deploy.sh       # stop watching after 30m
rimz wake --wait -- cargo build              # block here and print the outcome
rimz wake list
rimz wake list --json
rimz wake cancel wake-bold-comet
```

Exactly one trigger per wake: `--in`, `--signal`, or a command after `--`. Naming two, or none, is an error. Bare `rimz wake` with no arguments lists pending wakeups.

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

**`--signal <NAME>`** fires when a matching signal is emitted, from `rimz events emit`, from a forge transition, from an agent lifecycle transition, or from a watched command ([events reference](./events.md#emit-a-signal)). Names are lowercase dot-separated words. Repeat `--match KEY=VALUE` to require top-level payload fields; every pair must equal its field, comparing strings directly and other JSON values by their compact encoding. A signal wake is fired by the emitting process, so it needs no open room, and signals are never replayed: a wake armed after the emit does not see it.

Waking on an `agent.*` lifecycle signal requires naming a different agent than the one being woken, otherwise the target's own transition would wake it:

```console
$ rimz wake --signal agent.idle
error: wake on an agent.* signal requires --match handle=<other> or --match session=<other> to avoid waking the target from its own lifecycle signal
```

**A command after `--`** runs it under a watcher process and wakes when it exits. `--on fail|success|any` filters the outcome, defaulting to `any`; `fail` covers a non-zero exit and a timeout, `success` a zero exit. `--timeout <DURATION>` stops watching after that long, otherwise the watcher uses `loop.default-timeout` (`2h` by default). The command runs through `sh -c` at the project root with stdin closed, and its combined stdout and stderr ride along as a tail (16 KiB in the delivered prompt, 4 KiB in the stored record). The `--on` filter applies at delivery, not at arming: an outcome the filter rejects records `skipped` in the run log and retires the one-shot wake without delivering a message.

## The delivered message

The delivered text is `--prompt`, or the contents of `--prompt-file <PATH>` (a relative path resolves against the machine config directory, `~/.config/rimz/`, so prefer an absolute path). With neither, the wake delivers `The wake condition you were waiting for completed.`

Evidence rides with the prompt. A watched command appends its command line, exit status, and output tail:

```text
--- watch `cargo test` exited 101 ---
<output tail>
```

A signal wake appends the signal name and its pretty-printed payload under `--- signal ci.finished ---`, and `{{key}}` placeholders in the prompt are substituted with that payload's top-level values before delivery.

Delivery is a durable message from `@rimz` carrying a `Type: WAKE` header, gated on the receiver's next `done` boundary and inheriting the `[harness] smart_compact` default. An idle agent takes it at once, a working agent at its next boundary, and a session that has ended by then records `target gone`. The wake's run record links the message id, so `rimz message show <id>` and `rimz loop logs <name>` reach the same delivery.

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
NAME                TRIGGER                        TARGET        AGE  STATE
wake-mint-isle      once at 12:44                  @mint-lagoon  -    due 12:44
wake-patient-block  watch: sleep 30                @mint-lagoon  28s  watching pid 1871141
wake-stout-meter    on agent.idle [handle=@other]  @mint-lagoon  -    listening
```

The list covers pending wakes for the current project root. Called from an agent pane it shows only the wakes aimed at that agent; from a user shell it shows every wake for the root. `AGE` is how long a watcher has been running, and `STATE` is `due <HH:MM>` for a delay, `listening` for a signal, `watching pid <PID>` while a watcher holds its lock, and `watcher lost` when no process holds it. `--json` emits the same five fields per row.

`rimz wake cancel <name>` removes the wake and stops its watcher with SIGTERM. It refuses a name that is not a pending wake for this root, and an agent may cancel only a wake aimed at itself.

## What a wake writes on your machine

An armed wake is a one-shot loop task in machine state (`~/.local/state/rimz/loop-instances.json`), never in your `loop.toml`, and it appears in `rimz loop list` with source `state`. A watched command additionally starts one detached `rimz wake watch <name>` process holding `loop-watch-<name>.lock` in the workspace runtime directory. Firing appends one record to the loop run log, so `rimz loop show <name>` and `rimz loop logs <name>` read a wake's history like any other task until the entry retires.

If the watcher process dies before its command finishes, the room's elder notices the missing lock after a 30-second grace and fires the wake with a `watcher lost` outcome instead of leaving it pending forever. `rimz gc` reaps wake rows whose pinned session has left the agent snapshot, stopping their watchers as it goes.

The recurring counterpart is [`rimz loop add --signal`](./loop.md#signals), which subscribes a task to the same signals without retiring after one fire.
