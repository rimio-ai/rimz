# Loops and schedules

> `rimz loop` is cron for your agents: a named task, a schedule, and a supervised turn that fires in a real pane while the room is open. This page goes from small single-purpose loops to a fleet that works nights. The elder clock, state files, and run log underneath are [harness.md → Scheduled turns](../internals/harness/harness.md#scheduled-turns-loop).

## Why `rimz loop`

You already automate on a clock. Crontab lines and systemd timers run your maintenance, CI runs on a cron trigger — and lately there is a `while true; do claude -p "fix the next failing test"; sleep 900; done` holding a terminal somewhere. The instinct is right: define the goal and the stopping condition, and let a loop do the iterating instead of you re-prompting by hand.

The plumbing fights back. A cron line firing `claude -p` inherits every headless problem [scripting.md](./scripting.md#why-rimz-agents--p) starts from — an invisible run that hangs silently on its first permission prompt. It also fires blind: cron cannot check whether there is anything to do, so you write wrapper scripts around it, and it knows nothing about provider budget windows, so the first turn of the day starts a 5-hour window at whatever random time the job lands. The while-loop is no better: it burns a turn every pass whether or not anything changed, dies with the terminal, and leaves no record of what pass seventeen actually did. And neither can wake an agent that is already mid-conversation — they can only start fresh processes and lose the context.

`rimz loop` is the same clock with the room behind it. A task is a name, a schedule, and one action — open a supervised agent turn, deliver a prompt to a running agent, or run a shell command that guards either — and it fires while the room is open. The turn lands in a real pane with a live sidebar card, a question routes to you instead of hanging, and every fire is recorded for `rimz loop show`.

What that buys day to day is small and immediate: a budget window that starts before you do, an agent that sets its own alarm instead of making you the reminder service, a test suite that wakes a fixer only when it goes red. The next three sections are those loops, each a one-liner; the rest of the page is the machinery behind them and how far it stacks.

Zoomed out, a hands-off room is four layers you compose, from least to most involved:

1. **Built-in recovery** ([below](#built-in-recovery)) keeps a single agent alive through rate limits and transient errors with no schedule at all.
2. **Scheduled turns** fire supervised work on a clock — the loops below, and [every shape](#every-schedule-shape) behind them.
3. **Watchdogs** run a command first and wake an agent only on its result.
4. **Notification handlers** run your own command the moment an agent needs eyes — their own page: [notifications.md](./notifications.md).

Reach for the lowest layer that solves the problem. A long-running agent often needs only layer 1; a nightly job needs layer 2; a self-healing pipeline composes all four.

## Start the budget window before you do

Subscription providers meter usage in rolling windows: five hours that start with your first message and reset when they expire. Left alone, the window schedule follows your work schedule at the worst possible offset. Sit down at 9:00 and your first prompt opens a 9:00–14:00 window; on a heavy morning the budget is gone by 11:30, and you stall until 14:00 — dead hours in the middle of your day.

A ping task starts the window before you do:

```sh
rimz loop add morning --spec claude-ping --prompt ping --at 07:00 --days weekdays
```

At 07:00 the task runs one lowest-effort turn (Claude's ping pins Sonnet, so a flagship account does not prime at the flagship rate), and the window runs 07:00–12:00. You sit down at 9:00 with an almost untouched budget, the reset lands at noon instead of mid-afternoon, and the second window carries you to 17:00. Same subscription, same limits — the resets just stop landing in the middle of your deep work.

The ping is cheap insurance, not a wasted turn: before firing, Rimz reads the provider's cached rate-limit state and skips when a window is already counting down. The window is account-scoped, so one ping per provider primes every session of that kind — `codex-ping` does the same for Codex. And `--at-reset` replaces the fixed time with the window's own rhythm: it fires one minute after the provider's longest observed window resets, then uses the ping's own reading to schedule the next.

```sh
rimz loop add prime --spec claude-ping --prompt ping --at-reset   # keep the windows running back-to-back
```

## The agent's alarm clock

An agent's work often ends with a wait. CI has twenty minutes left, a reviewer was asked for comments, a deploy is baking. The agent has nothing to do until then — but the follow-up lands on you: remember to check CI, then tell the agent to merge. You become the reminder service for your own fleet.

`--bind` with `--in` is the alarm clock:

```sh
rimz loop add check-ci --bind @planner --prompt "CI should be done; check the run and merge if green" --in 30m
```

The one-shot delivers the prompt to that exact agent session through the same durable path as [`rimz message`](./messaging.md): `--bind` resolves the address when the task is added and pins the session, so the wake reaches the same conversation with all its context. An idle agent receives it immediately, a mid-turn agent parks it for its next turn boundary, and a session that has since exited is skipped and the task removed.

Because it is a plain command, the agent can set the alarm itself. At the end of a turn — "tests pass, CI needs half an hour" — it runs the `loop add` from its own shell tool and goes idle; thirty minutes later it wakes itself and finishes the job. Tell your agent once that the command exists, or package the pattern as a few-line skill, and "check back later" stops being your job. Chained, this is a self-paced loop: each wake schedules the next `--in` only while there is still work, so the loop advances exactly as long as the goal is unmet and then stops by itself. Self-set alarms persist as state — they never edit your `loop.toml` — show up in `rimz loop list`, and vanish on delivery.

## Watchdogs: a script for the routine, an agent for the recovery

Most recurring automation should stay a script. A cron job that runs the test suite, syncs a mirror, or checks certificate expiry is deterministic, instant, and free; spending an agent turn to re-check a world that has not changed is burning tokens on `true`. What a script cannot do is recover. When the command that is supposed to pass fails at 2 a.m., the script's whole error path is "page a human."

`--check` keeps the script as the loop's body and adds an agent as its recovery path:

```sh
# Watchdog: run the suite every 15m; Codex wakes only when it fails
rimz loop add watchdog --check "cargo test" --on fail \
    --spec codex --prompt "fix the failing test" --every 15m

# Trigger-when-green: poll CI until it passes, then hand the merge to the planner
rimz loop add ci-green --check "gh run watch --exit-status" --on success \
    --until 30m --every 2m --bind @planner --prompt "CI is green; merge"
```

The check runs first, every time, and costs nothing. Only its result spends a turn: `--on fail` (the default) wakes the agent on a non-zero exit or a timeout, `--on success` on a zero exit. When the guard fires, Rimz appends the command, its exit status, and its output tail to the prompt, so the agent wakes already reading the evidence instead of rediscovering it. The escalation ladder gets a new rung: the script handles the routine, the agent handles the failure, and you hear about it only when the agent gets stuck — its turn is supervised like any other, so a stuck fix goes `? waiting` and a [notification](./notifications.md) reaches you.

A `--check` with no agent action is still useful: a scheduled command that logs `completed`, `failed`, or `timed out` with the exit code and output tail into the run history, and keeps recurring.

## What a task does on your machine

`rimz loop add` edits one file and starts no process:

- By default it appends a `[tasks.<name>]` entry to `~/.config/rimz/loop.toml` — per-machine automation, like your crontab, never inherited by a cloned repository.
- `--project` writes the entry to `<root>/.rimz/config.toml` instead: shared automation that travels with the repo. A committed task runs commands on whoever pulls it, so it enters the project trust hash and stays inert until each user reviews the diff and runs `rimz trust grant` ([security.md](./security.md)); a trusted project task wins over a same-named machine task without double-firing.
- One-shots (`--in`, `--once`, `--until`) persist as state rather than config, so an agent scheduling its own wake never edits your `loop.toml`.

There is no daemon; the room keeps time. While a room for the task's project is open — attached or not — the room's elected sidebar process fires due tasks on its regular tick, running each through the hidden `rimz loop run`. Close the room and the clock stops. Opening one late does not replay what was missed: a task first seen past its time waits for the next matching occurrence, so there is never a catch-up storm.

A fire leaves two things behind: whatever the task did — one transient supervised pane for `--spec`, one delivered message for `--bind` — and one appended line of run history that `rimz loop show <name>` reads back. Everything reverses in one move: `rimz loop remove <name>` deletes the entry (a project removal prints the `rimz trust grant` follow-up, since the trusted file changed), and both files are plain TOML you can read and edit by hand.

## Built-in recovery

Two settings keep a live agent working through the interruptions that would otherwise park it — leave it stopped mid-task, waiting for someone to resume it — until morning. Both are off by default and switch on with one `rimz config set`.

```sh
rimz config set resume.auto_continue true     # resume rate-limit and API-error parks
rimz config set harness.smart_compact "70%"   # compact before a message once context passes 70%
```

**Auto-continue** picks a parked turn back up on its own. A rate-limit or spend-limit park resumes the moment the provider's budget window resets, and a transient overload or API error retries on a lengthening backoff ramp — the first retry a few minutes after the failure, then spaced further out, giving up after a bounded number of attempts. Recovery types the nudge (`continue` by default) into the agent's live pane through the same path as a steer message, so the agent's next hook moves the row back to running. The backoff and retry keys are in [configuration.md → Resume](./configuration.md#resume); the decision logic is [provider.md → Auto-continue](../internals/agents/providers.md#auto-continue).

**Smart compaction** rides the same loop: past the threshold, Rimz submits `/compact` ahead of your text so the prompt lands against a fresh context window instead of dying mid-turn. Set a default with `harness.smart_compact`, or leave it unset and pass `--smart-compact` per message. Details in [messaging.md](./messaging.md).

Turn both on and a long-running agent keeps its footing — through the 5-hour wall, through a flaky API, through a filling context window — with no babysitter process watching it.

## Every schedule shape

The loops above are points in a small grammar. Each task names one action — `--spec` (a kind, a profile, or a virtual cell like `codex-yolo` or `claude-ping`) for a fresh supervised pane, or `--bind @<handle>` for a running session — carries a `--prompt` or `--prompt-file`, and picks one firing shape:

| Shape | Flags | Example |
| --- | --- | --- |
| Calendar | `--at`, with an optional `--days` mask (`daily`, `weekdays`, `weekends`, a range `mon-fri`, or a list `mon,wed,fri`) | `--at 07:00 --days weekdays` |
| Interval | `--every`, measured from the last fire | `--every 15m` |
| Raw cron | `--cron`, a five-field expression | `--cron "*/15 * * * *"` |
| One-shot | `--once` on a calendar or cron schedule, or `--in <delay>` | `--in 30m` |
| Window-reset | `--at-reset` on a `<kind>-ping` spec, tied to the provider's budget-window reset ([above](#start-the-budget-window-before-you-do)) | `--spec claude-ping --at-reset` |
| Poll-until | `--every` plus `--check`, `--on`, `--until`, and an agent action ([above](#watchdogs-a-script-for-the-routine-an-agent-for-the-recovery)) | `--check "gh run watch --exit-status" --on success --until 30m --every 2m` |

Calendar, cron, `--in`, and `--until` resolve in the top-level `timezone`, falling back to the system zone when unset.

The turn itself takes the launch-shaping flags you already know from [agents.md](./agents.md): `--worktree` hosts the pane on an isolated branch, `--mode auto|ask|yolo` sets the permission posture ([below](#the-permission-posture-for-unattended-runs)), `--effort` and `--system-prompt-file` shape the agent, and `--timeout` caps the wall clock. Inspect, test, and manage tasks with the rest of the surface:

```sh
rimz loop list                 # every task, grouped by project, with next-fire and last-run
rimz loop show pr-watch        # one task's schedule, next fire, and recent run forensics
rimz loop fire pr-watch        # fire now in the foreground for testing; the schedule stays put
rimz loop fire pr-watch --keep # leave the transient pane open to inspect
rimz loop remove pr-watch
```

Run mechanics — exit codes, output formats, `wait --stream` — are [scripting.md](./scripting.md); every flag is in the [loop CLI reference](../reference/cli/loop.md).

## The permission posture for unattended runs

An unattended run has to answer permission prompts without you, and two patterns compose. The posture you pick is the guardrail layer of the harness — a constraint the room and the agent's own prompts enforce.

**Answer in the agent's own UI** to keep the full record. A [handler that acts](./notifications.md#handlers-that-act-not-just-alert) sends the answer with `rimz pane send`, leaving the prompt, the answer, and the tool run all in the agent's transcript, exactly as if you had typed it. Prefer this path when handled decisions belong on the record.

**Use the agent's bypass flag** for runs where you accept the tradeoff. `rimz agents <kind> "<prompt>" -p --yolo` passes the adapter's bypass flag (`claude --dangerously-skip-permissions`, `codex --dangerously-bypass-approvals-and-sandbox`), while `--ask` keeps the provider's prompts in place. Rimz still observes sessions, completions, and failures through lifecycle hooks; the tradeoff is that the agent skips permission events at the source, so Rimz's durable record holds what other hooks report rather than a per-decision audit trail.

Reserve the bypass flag for runs where you accept the missing per-decision trail, and keep the guardrails visible — trust grants, notification handlers, and the posture itself are product behavior, covered in [security.md](./security.md).

## A fleet that works nights

[Scripting](./scripting.md) turns an agent into a shell command; a loop puts that command on a clock; and because a scheduled turn is a full room citizen, it reaches every other primitive — `-p` subagents, teams, worktrees, messages. Stacked, the layers turn routine work into standing tasks:

```sh
# 07:00 — the budget windows are running before you sit down
rimz loop add prime --spec claude-ping --prompt ping --at 07:00 --days daily

# every 15m — CI on the release PR fixes itself
rimz loop add ci-fix --check "gh run watch --exit-status" --on fail \
    --spec codex --prompt "CI failed on the release PR; read the failing job's logs and fix it" --every 15m

# 02:00 — a nightly triage that fans out and opens PRs
rimz loop add nightly --spec claude --worktree nightly --timeout 4h --at 02:00 --days daily \
    --prompt "Scan the repository for bugs and cheap improvements. For each one worth fixing, \
run a codex -p subagent in its own worktree, review its diff, and open a PR."
```

The nightly task is one scheduled turn, but its prompt hands the agent the room's own tools: it fans work out with [`-p` subagents](./scripting.md#agents-scripting-agents), isolates each fix in a [worktree](./worktrees.md), and could as easily launch a [team](./teams.md) and brief it over [messages](./messaging.md). The unit of automation stops being a prompt you type and becomes a standing cycle that checks, acts, and re-arms itself.

The rest of the harness keeps that cycle safe while you sleep: [auto-continue](#built-in-recovery) carries runs over rate limits, checks fire agent turns only when there is work, every fire lands in `rimz loop show`, a question or failure trips a [notification handler](./notifications.md) that reaches your phone, and the [permission posture](#the-permission-posture-for-unattended-runs) is a per-task choice, not a global switch. Leave the room open, detached on your workstation or on a server you reach with [`rimz remote`](./remote.md), and by morning `rimz loop list` and the PR queue show what the night produced.

## See also

- [Scripting agents](./scripting.md) — the supervised-run mechanics every scheduled `--spec` task rides on: exit codes, `--output-format`, `wait --stream`.
- [Notifications](./notifications.md) — the push routes and acting handlers that catch what a loop cannot handle alone.
- [Messaging](./messaging.md) — the delivery path `--bind` uses, and smart compaction in full.
- [Loop CLI](../reference/cli/loop.md) — every flag on `add`, `fire`, `list`, `show`, `rename`, and `remove`.
- [Configuration](./configuration.md) — the `[resume]` and `[harness]` keys, and the `loop.toml` shape.
- [Security and trust](./security.md) — the safety posture for bypass flags and project trust.
- [harness.md → Scheduled turns](../internals/harness/harness.md#scheduled-turns-loop) — the elder clock, state files, and run log underneath.
