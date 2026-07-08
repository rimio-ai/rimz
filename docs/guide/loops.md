# Loops and hands-off operation

Loops keep the fleet moving while you sleep: you define the goal and the stopping condition, and agents act, observe, and recover without you at the keyboard.

```sh
rimz loop add morning --spec claude-ping --prompt ping --at 07:00 --days weekdays   # start the provider's 5h budget window every weekday
rimz loop add watchdog --check "cargo test" --on fail \
    --spec codex --prompt "fix the failing test" --every 15m                        # watch the suite, wake an agent on failure
```

A hands-off loop is four layers you compose, from least to most involved:

1. **Built-in recovery** keeps a single agent alive through rate limits and transient errors with no schedule at all.
2. **Scheduled turns** (`rimz loop`) fire supervised work on a clock.
3. **Watchdogs** run a command first and wake an agent only on its result.
4. **Notification handlers** run your own command the moment a row needs eyes, so routine prompts clear themselves.

Reach for the lowest layer that solves the problem. A long-running agent often needs only layer 1; a nightly job needs layer 2; a self-healing pipeline composes all four.

## Built-in recovery

Two settings keep a live agent working through the interruptions that would otherwise park it — leave it stopped mid-task, waiting for someone to resume it — until morning. Both are off by default and switch on with one `rimz config set`.

```sh
rimz config set resume.auto_continue true     # resume rate-limit and API-error parks
rimz config set harness.smart_compact "70%"   # compact before a message once context passes 70%
```

**Auto-continue** picks a parked turn back up on its own. A rate-limit or spend-limit park resumes the moment the provider's budget window resets, and a transient overload or API error retries on a lengthening backoff ramp — the first retry a few minutes after the failure, then spaced further out, giving up after a bounded number of attempts. Recovery types the nudge (`continue` by default) into the agent's live pane through the same path as a steer message, so the agent's next hook moves the row back to running. The backoff and retry keys are in [configuration.md → Resume](../reference/configuration.md#resume); the decision logic is [provider.md → Auto-continue](../internals/agents/provider.md#auto-continue).

**Smart compaction** rides the same loop: past the threshold, Rimz submits `/compact` ahead of your text so the prompt lands against a fresh context window instead of dying mid-turn. Set a default with `harness.smart_compact`, or leave it unset and pass `--smart-compact` per message. Details in [messaging.md](./messaging.md).

Turn both on and a long-running agent keeps its footing — through the 5-hour wall, through a flaky API, through a filling context window — with no babysitter process watching it.

## Scheduled turns

`rimz loop` fires a supervised agent turn on a clock. Each task names one agent cell with `--spec` (a kind, a profile, or a virtual cell like `claude-ping`), carries a `--prompt`, and picks one firing shape. The room keeps time while it is open, so tasks fire whenever that project's room is alive.

```sh
rimz loop add morning --spec claude-ping --prompt ping --at 07:00 --days weekdays   # prime the 5h window
rimz loop add nudge --bind @planner --prompt "resume the review" --in 30m           # one-shot wake
rimz loop add pr-watch --spec codex --prompt "check CI on the release PR" --every 15m
```

A task fires in one of six shapes:

| Shape | Flags | Example |
| --- | --- | --- |
| Calendar | `--at`, with an optional `--days` mask (`daily`, `weekdays`, `weekends`, a range `mon-fri`, or a list `mon,wed,fri`) | `--at 07:00 --days weekdays` |
| Interval | `--every`, measured from the last fire | `--every 15m` |
| Raw cron | `--cron`, a five-field expression | `--cron "*/15 * * * *"` |
| One-shot | `--once` on a calendar or cron schedule, or `--in <delay>` | `--in 30m` |
| Window-reset | `--at-reset` on a `<kind>-ping` spec, tied to the provider's budget-window reset ([below](#prime-a-budget-window-on-your-clock)) | `--spec claude-ping --at-reset` |
| Poll-until | `--every` plus `--check`, `--on`, `--until`, and an agent action ([below](#watchdogs-check-first-wake-on-the-result)) | `--check "gh run watch --exit-status" --on success --until 30m --every 2m` |

Calendar, cron, `--in`, and `--until` resolve in the top-level `timezone`, falling back to the system zone when unset. Inspect, test, and manage tasks with the rest of the surface:

```sh
rimz loop list                 # every task, grouped by project, with next-fire and last-run
rimz loop show pr-watch        # one task's schedule, next fire, and recent run forensics
rimz loop fire pr-watch        # fire now in the foreground for testing; the schedule stays put
rimz loop fire pr-watch --keep # leave the transient pane open to inspect
rimz loop remove pr-watch
```

A scheduled `--spec` task runs down the same supervised path as a scripted `-p` run — one transient pane opens, the turn runs under a timeout, and the outcome is recorded. Run mechanics (exit codes, streaming, `wait`) live in [scripting.md](./scripting.md).

### Watchdogs: check first, wake on the result

Add `--check "<command>"` and the task runs that command before any agent action, waking the agent only on the result. `--on fail` (the default) wakes on a non-zero exit or a timeout; `--on success` wakes on a zero exit. When the guard fires, Rimz appends the command, its exit status, and its output tail to the prompt, so the agent wakes already knowing what happened.

```sh
# Watchdog: run the suite every 15m, wake Codex to fix it on failure
rimz loop add watchdog --check "cargo test" --on fail \
    --spec codex --prompt "fix the failing test" --every 15m

# Trigger-when-green: poll CI until it passes, then hand the merge to the planner
rimz loop add ci-green --check "gh run watch --exit-status" --on success \
    --until 30m --every 2m --bind @planner --prompt "CI is green; merge"
```

A failing test suite becomes a fix prompt instead of a red morning. A `--check` with no agent action is a scheduled command in its own right — it logs `completed`, `failed`, or `timed out` with the exit code and output tail, and keeps recurring.

### Deliver to a live agent with `--bind`

`--spec` opens a fresh pane; `--bind @<handle>` instead delivers the prompt to one agent session that is already running, through the same durable message path as `rimz message`. It resolves the address at add time and pins that exact session, so the wake reaches the same conversation. An idle agent receives it immediately; a running one parks it for the next turn boundary; a session that has since exited is skipped and the schedule removed. This is the shape a self-paced agent uses to wake itself: at the end of a turn it schedules its own next `--in <delay>` one-shot, so the loop advances only while there is still work.

### Prime a budget window on your clock

A `<kind>-ping` spec starts a provider's budget window at a time you choose. It runs a lowest-effort turn (Claude's ping pins Sonnet so a flagship account does not prime at the flagship rate), and skips when the provider's window is already counting down — so a ping is cheap insurance, not a wasted turn.

```sh
rimz loop add morning --spec claude-ping --prompt ping --at 07:00 --days weekdays
rimz loop add prime --spec claude-ping --prompt ping --at-reset   # follow the window's own reset
```

The window is account-scoped, so one ping per provider primes every session of that kind. `--at-reset` chains the ping to the provider's longest observed window: it fires one minute after that window resets, then uses the ping's own reading to schedule the next. Prime each provider in the morning and the fleet has a full budget window waiting when you — or a scheduled job — start work.

### Personal versus shared schedules

Recurring definitions live in per-machine `~/.config/rimz/loop.toml` — personal automation, like a crontab, never inherited by a cloned repository. `rimz loop add --project` instead writes the task into `<root>/.rimz/config.toml` as shared automation that travels with the repo. A project task enters the project trust hash and stays inert until you review the diff and run `rimz trust grant` — the same guardrail every command-running project field passes through ([security.md](./security.md), [troubleshooting.md](./troubleshooting.md#project-config-isnt-taking-effect)). A trusted project task wins over a same-named machine task without double-firing.

## Notification handlers

A notification handler runs your own command the moment a row needs eyes. The room fires it on its attention cues — an agent going `waiting`, a `failed` turn, a park — with the triggering agent, its kind, its pane id, and the workspace root in the command's environment (`RIMZ_NOTIFY_AGENT`, `RIMZ_NOTIFY_KIND`, `RIMZ_NOTIFY_PANE`, `RIMZ_NOTIFY_ROOT`). Handlers live in per-machine `config.toml`, outside project trust, because they often carry host-specific push credentials.

```toml
[[notifications.handler]]
when = { kind = ["waiting"] }
command = "python3 ~/bin/waiting_handler.py"
```

The built-in triggers are `waiting` and `failed`; a `when` clause narrows a handler by `kind`, `worktree` glob, or `handle` glob, and an empty `when` matches every notification. The simplest handler is a push relay — `ntfy publish`, a Slack webhook, an OS notifier — so an agent going waiting reaches your phone. The complete configuration surface, template variables, and channel behavior are in [notifications.md](../internals/sidebar/notifications.md).

### Handlers that act, not just alert

A handler fires with the pane and root in hand, and everything it might do next is a public Rimz command. That makes a handler a place to clear the routine prompt you have already approved eight times today, composed from the room's own primitives:

- `rimz pane capture @<handle>` reads what the agent is asking.
- `rimz pane send @<handle>` types the answer into the agent's own UI.
- `rimz message @<other>` hands the situation to a different agent.
- `rimz agents <kind> -p` runs a one-shot supervised turn to decide.

A handler can match the prompt against patterns your script owns and answer only the shapes it recognizes — a bounded-pattern approver, a one-shot agent delegate, or a standing in-room guardian you steer with `rimz message --steer @guardian`. Anything the handler leaves alone stays `? waiting` in the sidebar and still routes to you. Attention bandwidth then scales with what you automate rather than with the agent count. Because pane text is agent output and can contain anything, treat it as untrusted: match known shapes, do nothing on the unknown ([security.md](./security.md)).

## The permission posture for unattended runs

An unattended run has to answer permission prompts without you, and two patterns compose. The posture you pick is the guardrail layer of the harness — a constraint the room and the agent's own prompts enforce.

**Answer in the agent's own UI** to keep the full record. A handler that sends the answer with `rimz pane send` leaves the prompt, the answer, and the tool run all in the agent's transcript, exactly as if you had typed it. Prefer this path when handled decisions belong on the record.

**Use the agent's bypass flag** for runs where you accept the tradeoff. `rimz agents <kind> "<prompt>" -p --yolo` passes the adapter's bypass flag (`claude --dangerously-skip-permissions`, `codex --dangerously-bypass-approvals-and-sandbox`), while `--ask` keeps the provider's prompts in place. Rimz still observes sessions, completions, and failures through lifecycle hooks; the tradeoff is that the agent skips permission events at the source, so Rimz's durable record holds what other hooks report rather than a per-decision audit trail.

Reserve the bypass flag for runs where you accept the missing per-decision trail, and keep the guardrails visible — trust grants, notification handlers, and the posture itself are product behavior, covered in [security.md](./security.md).

## See also

- [Scripting agents](./scripting.md) — the supervised-run mechanics every scheduled `--spec` task rides on: exit codes, `--output-format`, `wait --stream`.
- [Messaging](./messaging.md) — the delivery path `--bind` uses, and smart compaction in full.
- [Configuration](../reference/configuration.md) — the `[resume]`, `[harness]`, and `[notifications]` keys, and the `loop.toml` shape.
- [Security and trust](./security.md) — the safety posture for handlers, bypass flags, and project trust.
- [harness.md → Scheduled turns](../internals/harness/harness.md#scheduled-turns-loop) and [notifications.md](../internals/sidebar/notifications.md) — the internals behind the clock and the wakeups.
