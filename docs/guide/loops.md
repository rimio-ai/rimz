# Loops

> `rimz loop` is cron for your agents. A task is a name, a schedule, and one action: an agent turn in a real pane, a wake to an agent that is already running, or a shell command that guards either. It fires while the room is open. This page starts from a single scheduled turn and builds up to a fleet that works through the night. The clock, the state files, and the run log underneath are in [harness.md → Scheduled turns](../internals/harness/harness.md#scheduled-turns-loop).

## Why rimz loop

You already run things on a clock. Cron lines and systemd timers drive your maintenance, CI fires on a schedule, and lately a `while true; do claude -p "fix the next failing test"; sleep 900; done` is holding a terminal somewhere. The instinct behind that loop is right: state the goal and the stopping condition once, and let the loop iterate instead of you re-prompting by hand.

But cron and a hand-rolled `while` loop only know one move: start a fresh process and walk away. That scheduled `claude -p` is invisible, so it hangs in silence the first time it stops for a permission prompt (the headless gap [scripting](./scripting.md#why-rimz-agents--p) opens with). It fires whether or not there is anything to do, spending a whole agent turn to re-check a world that has not changed. And it cannot hand work to an agent you already have running; every run is a brand-new process starting from an empty context.

`rimz loop` is the same clock with the room behind it. A task is a name, a schedule, and an action — a fresh turn or a wake — with two shell-command gates that wrap either one:

- `--agent` starts a new agent for the turn: a fresh supervised pane that runs the prompt once and cleans up. Reach for it when the work should begin from a clean context each time.
- `--wake` hands the prompt to an agent you already have running and waiting, so the work resumes in that same conversation with all of its context, rather than in a cold new process.
- `--check` runs a shell command before the action and spends a turn only on its result, so a scheduled agent never starts just to find nothing to do.
- `--verify` runs a shell command after a spawned agent turn and re-prompts that same session until the task is actually done.

Whichever action fires, the turn is a full room citizen: a live card in the sidebar, a permission question that routes to you instead of hanging the job, and a line in the run log that `rimz loop show` summarizes and `rimz loop logs` opens in full.

One rule governs every schedule: a task repeats only when `--every` or `--cron` says so. A bare time (`--at 07:00` or `--in 30m`) fires once and then removes itself. And `rimz loop add` prints back exactly what it armed: the action, the schedule in plain words, and the concrete next fire time. You read the schedule instead of guessing at it.

The next sections take those actions one at a time — scheduling a fresh turn, waking an agent you already have, guarding a turn with a check — then put the provider's weekly surplus to work. Each opens with what the action is for, then how far it goes. The rest of the page is the machinery behind them, and how it all stacks.

## Schedule a fresh turn

`--agent` starts a new agent for the turn. On the schedule you set, it opens a fresh supervised pane, runs the prompt once, and cleans up, exactly as if you had run `rimz agents <kind> "<prompt>" -p` yourself at that moment. Reach for it whenever the work should begin from a clean context: a nightly repository sweep, a Monday dependency check, a scheduled security audit, a recurring status report. Each fire is a new agent with no memory of the last one, which is the point when the job is "do this again against today's tree."

```sh
rimz loop add deps --agent codex --worktree deps --every mon --at 09:00 \
    --prompt "Check for outdated dependencies. Open a PR for the safe minor and patch bumps, and list any majors that need review."
```

That one line stands in for a cron entry, the guard script around it, and the terminal you would have left open to watch it. It runs Codex in an isolated [worktree](./worktrees.md) every Monday at 09:00, and if a bump breaks the build the agent stops and asks instead of forcing it through: the question reaches you like any other waiting card. Every launch-shaping flag from [the agents guide](./fleet.md) rides along, so `--worktree`, `--mode`, `--effort`, `--system-prompt-file`, and `--timeout` shape a scheduled turn the same way they shape an interactive one ([full list below](#every-schedule-shape)). What the agent then does with the turn is bounded only by the prompt: a one-line check, or the whole [fleet that works nights](#a-fleet-that-works-nights).

### Prime the budget window

One scheduled turn earns its keep doing no project work at all: opening a provider's budget window before you need it.

Subscription providers meter usage in rolling windows: five hours that start with your first message and reset when they expire. Left to chance, the window lands at the worst offset from your day. Sit down at 9:00 and your first prompt opens a 9:00 to 14:00 window; on a heavy morning the budget is gone by 11:30, and you stall until 14:00, dead hours in the middle of the day.

A `<kind>-ping` virtual agent opens the window before you arrive when that adapter supplies a generic low-cost priming command:

```sh
rimz loop add morning --agent claude-ping --prompt ping --every weekday --at 07:00
```

Every weekday at 07:00 the task runs one lowest-effort turn, and the window runs 07:00 to 12:00. (Claude's ping pins Sonnet, so a flagship account does not prime at the flagship rate.) You sit down at 9:00 against an almost untouched budget, the reset lands at noon instead of mid-afternoon, and the next window carries you to 17:00. Same subscription, same limits. The resets just stop landing in the middle of your deep work.

The ping is cheap insurance, not a wasted turn. Before it fires, RimZ reads the provider's cached rate-limit state and skips when a window is already counting down. The window is account-scoped, so one ping primes every session of that provider: `codex-ping` does the same for Codex. `qwen-ping` binds the final launch to the exact Alibaba region and API-key fingerprint selected by Qwen's process environment; cold data permits the one priming turn, while missing, mismatched, or incomplete data after that keeps the gate closed before any `--check` command runs. Antigravity has no verified generic model-safe priming command, so `antigravity-ping` is rejected even though its account-owned `5h`/`7d` bars support display and surplus checks.

To keep the windows back-to-back all day, let the window set its own schedule:

```sh
rimz loop add prime --agent claude-ping --prompt ping --every reset
```

`--every reset` fires one minute after the provider's longest pacing window resets, then reads that ping's own result to time the next fire. Each window opens the moment the last one closes. Qwen uses the exact account's `7d` window for reset recurrence; an exhausted Alibaba `30d` window still blocks a fire without stretching the recurrence to a month. The shape requires a supported `<kind>-ping`.

To keep the longest window running for every supported provider without defining each task, enable the machine-level loop flag:

```toml
auto-ping = true
```

RimZ synthesizes a visible `autoping-<kind>` reset task for each ping-capable adapter in every open project room. When an authoritative provider reading says the longest window is down, the task retries within an hour; immediately before each ping, RimZ checks the provider's OAuth usage API again and skips if the window has started elsewhere. An officially cleared limit — full bars with no active reset countdown — enters the same retry path, while a logged-out provider or unknown reading stays idle.

Pause one generated provider with `rimz loop pause autoping-<kind>`, or disable all generated pings with `rimz config set loop.auto-ping false`. Generated rows have no stored task definition to remove or rename; define a task with the same name when you want an explicit replacement.

## Wake a running agent

An agent's work often ends in a wait. CI has twenty minutes left, a reviewer owes comments, a deploy is baking. The agent has nothing to do until then, and the follow-up falls to you: remember to check CI, then tell the agent to merge. You become the reminder service for your own fleet.

Where `--agent` starts a new agent, `--wake` reaches one you already have. Point it at a running, waiting session and give it a time; `--in` makes it an alarm clock:

```sh
rimz loop add check-ci --wake @planner --prompt "CI should be done; check the run and merge if green" --in 30m
```

RimZ resolves `@planner` the moment you add the task, in the current room and channel, exactly as [`rimz message`](./messaging.md) would, and pins that one session; the add output names the session it pinned. Thirty minutes later the prompt travels the same durable delivery path as any message, so it lands back in that conversation with all of its context. An idle agent takes it at once, a mid-turn agent parks it for its next turn boundary, and a session that has exited by then is skipped and the task removed.

Because `rimz loop add` is a plain command, the agent can set its own alarm. At the end of a turn ("tests pass, CI needs half an hour") it runs the `loop add` from its shell tool and goes idle; thirty minutes later it wakes itself and finishes the job. Tell the agent once that the command exists, or wrap the pattern in a short skill, and "check back later" stops being your job. Chain it and you have a self-paced loop: each wake schedules the next `--in` only while work remains, so the loop advances exactly as long as the goal is unmet, then stops on its own. These self-set alarms are one-shots. They live in state rather than your `loop.toml`, show up in `rimz loop list`, and clear themselves after firing.

For a plain reminder with no agent action, [`rimz message --schedule`](./messaging.md) is the lighter tool: one delivery on a timer, no task at all. A loop `--wake` earns its place when the wake repeats with `--every`, waits on a [`--check` guard](#guard-a-turn-with-a-check), or needs to land in the run history.

## Guard a turn with a check

Most recurring automation should stay a plain script. A cron job that runs the test suite, syncs a mirror, or checks a certificate's expiry is deterministic, instant, and free; spending an agent turn to re-check a world that has not changed is burning tokens on `true`. What a script cannot do is recover. When the command that was supposed to pass fails at 2 a.m., the script's whole error path is "page a human."

`--check` runs a script before any agent action and spends a turn only on its result, so the agent is called in only when the script says there is something to do. That makes the script the loop's body and the agent its recovery path. This is the watchdog:

```sh
# Watchdog: run the suite every 15m; Codex wakes only when it fails
rimz loop add watchdog --check "cargo test" --on fail \
    --agent codex --prompt "fix the failing test" --every 15m

# Trigger-when-green: poll CI until it passes, then hand the merge to the planner
rimz loop add ci-green --check "gh run watch --exit-status" --on success \
    --until 30m --every 2m --wake @planner --prompt "CI is green; merge"
```

The check runs first, every time, and costs nothing. Only its result spends a turn: `--on fail` (the default) wakes the agent on a non-zero exit or a timeout, `--on success` on a zero exit. When the guard fires, RimZ appends the command, its exit status, and its output tail to the prompt, so the agent wakes already reading the evidence instead of rediscovering it. That adds a rung to the escalation ladder: the script handles the routine, the agent handles the failure, and you hear about it only when the agent itself gets stuck. Its turn is supervised like any other, so a stuck fix goes `? waiting` and a [notification](./notifications.md) reaches you.

A `--check` with no agent action is still worth having. It is a scheduled command that logs `completed`, `failed`, or `timed out`, each with the exit code and output tail, into the run history, and it keeps recurring.

`--check` gates firing; `--verify` gates completion. Add `--verify "cargo test"` to a scheduled `--agent` task when the command is the definition of done: a red result returns its evidence to the same live session, up to `--max-attempts` total turns, before the fire records `verify failed`.

## Soak up the weekly surplus

The weekly subscription window expires whether you use it or not: whatever is left on the `7d` bar at the weekly reset is capacity you paid for and gave back. A recurring background task can absorb it — refactoring, filling test gaps, dependency triage are all real work with no deadline — but scheduled blindly it drains the same window your daytime sessions run on. The task should fire only when the week is ahead of pace.

`--surplus` is that condition. Before each fire, RimZ reads the provider's longest pacing window and computes forward headroom, how far ahead of the sustainable pace the window is running: `1.0x` is exactly on pace, and `1.5x` means half again as much budget remains as the clock requires. Qwen uses the exact account's `7d` Alibaba window for pacing; its `30d` window remains an exhaustion boundary only. The headroom model, a worked example, and the fail-closed rules live in [budgets → spend the provider surplus](./budget.md#spend-the-provider-surplus).

```sh
rimz loop add refactor-soak --agent claude --prompt "Refactor the next rough module and leave the branch green" \
    --every 4h --surplus 1.5x --surplus-after 3d
```

`--surplus 1.5x` opens the gate only at that headroom or above; `--surplus-after 3d` keeps the task quiet until three days of the window have elapsed, so an untouched early week is not spent before your own heavy days land (used alone, it still requires `1.0x`). The gate guards `--agent` and `--wake` actions alike and runs before any `--check` guard, so a closed gate runs nothing and costs nothing: the fire records `surplus skipped` without adding a strike, and the recurring schedule keeps polling until real surplus appears. An account without a window reading — an API key, or a window that has not started — keeps the gate closed.

## What a task does on your machine

`rimz loop add` edits one file and starts no process:

- A repeating task (`--every` or `--cron`) appends a `[tasks.<name>]` entry to `~/.config/rimz/loop.toml`: per-machine automation, like your crontab, never inherited by a cloned repository.
- A one-shot (bare `--at`, `--in`, or a `--until` deadline) persists as state instead, so an agent scheduling its own wake never touches your `loop.toml`; the entry retires itself after firing.
- `--project` writes the entry to `<root>/.rimz/config.toml`: shared automation that travels with the repo, so it has to be a repeating task (a one-shot is machine state by definition). A committed task runs commands on whoever pulls it, so it enters the project trust hash and stays inert until each user approves it ([security.md](./security.md)); in a terminal RimZ shows the surface diff and offers the grant immediately, while off a terminal it prints the review and approve commands. A trusted project task wins over a same-named machine task without double-firing.

There is no scheduler daemon; the room keeps time. While a room for the task's project is open, attached or not, that room's elected sidebar process fires due tasks on its regular tick, running each through the hidden `rimz loop run`. Close the room and the clock stops. Opening one late does not replay what was missed: a task first seen past its time waits for the next matching occurrence, so there is never a catch-up storm.

A scheduled `--agent` fire lands in the `rimzd` loop zone: the runtime column's live loop panel stays open, and transient run panes stack under it instead of splitting the sidebar or a working tab. If the panel pane was closed while the `rimzd` view remains, RimZ recreates the panel at fire time and stacks the run under it; if the whole view is gone or the split fails, it falls back to a new run tab. Manual `rimz loop fire` keeps splitting beside the caller so its foreground stream stays local.

A fire leaves two things behind: whatever the task did (one transient supervised pane for `--agent`, one delivered message for `--wake`), and one line of run history. A successful budget-window ping also records the refreshed shortest and longest window reset outcome on that line. `rimz loop show <name>` gives that history a health verdict and, for check-gated work, a separate agent-run rollup; `rimz loop logs <name>` prints the complete stored forensics. Everything reverses in one move. `rimz loop remove <name>` deletes the entry; a project removal shows the surface diff and offers the refreshed grant in a terminal, or prints the review and approve commands elsewhere. Both files are plain TOML you can read and edit by hand.

## Built-in recovery

**Repeated failures pause the task.** Three consecutive failed fires auto-pause any task indefinitely, display `paused · 3 strikes` in `rimz loop list`, and fire notification handlers with kind `loop_paused`. A completed or delivered turn still counts when its check shows the world remains broken; a healthy check or successful turn resets the counter. Inspect with `rimz loop show`, then use `rimz loop resume <name>` to clear the strikes and re-arm the schedule. Set `--max-strikes <N>` per task to change the threshold, or `--max-strikes 0` to disable auto-pause; `rimz loop fire` remains available while paused for a manual test.

Two opt-in settings keep a live agent working through the interruptions that would otherwise park it (stop it mid-task and leave it waiting for someone to resume it). Both are off by default, and each switches on with one `rimz config set`.

```sh
rimz config set resume.auto_continue true     # resume rate-limit and API-error parks
rimz config set resume.auto_redeem true       # spend useful Codex reset credits automatically
rimz config set harness.smart_compact 200k    # compact before a message once context passes 200k tokens (or "70%")
```

**Auto-continue** picks a certified parked turn back up on its own. A provider-owned per-turn failure marker proves why the turn stopped; an authoritative account window then supplies the reset clock for a rate-limit or spend-limit park, while a certified transient overload or API error uses a lengthening backoff ramp. An exhausted window, error message, or stalled pane cannot arm recovery by itself. Recovery types the nudge (`continue` by default) into the agent's live pane through the same path as a steer message, so the agent's next hook moves the row back to running. Qwen's Alibaba launch quota is checked only before a fresh managed launch and does not arm mid-run auto-continue; Qwen's native stop failures continue through the ordinary rate-limit or overload classification. Antigravity has account-owned clocks but no failure marker certified recoverable, so its error stops remain terminal and a loop fire records the ordinary failure. The backoff and retry keys are in [configuration.md → Resume](./configuration.md#resume); the decision logic is [providers.md → Auto-continue](../internals/agents/providers.md#auto-continue).

**Auto-redeem pairs with auto-continue** to carry Codex work through a spent long window: a useful or doomed reset credit restores the account capacity, then the existing certified recovery path wakes the parked turn.

**Dollar budgets bound hands-off work.** `--budget 5` caps each fired run; `--budget-per-day 20` makes the scheduler sum that task's completed run costs in the configured local day and skip a fire that cannot fund its per-run cap, recording `budget skipped`. `rimz loop list` shows each task's spend against its daily cap. For check-gated work, `rimz loop show` separates the agent attempts from cheap check passes and totals their costs across the recorded history; other tasks retain the last-run and rolling ten-run average cost. Fresh input/output tokens stay visible per run. The room-fleet and provider-account daily caps gate the same fires before launch. An exact Qwen Alibaba window exhaustion records the same `budget skipped` result before the loop's `--check` command or pane exists, and a different API-key fingerprint cannot gate that launch. The whole cap model, and why a human message can waive an interactive turn but never satisfies a loop gate, is the [budgets guide](./budget.md).

**Smart compaction** rides the same loop. Past the threshold, RimZ submits `/compact` ahead of your text, so the prompt lands against a fresh context window instead of dying mid-turn. Set a default with `harness.smart_compact`, or leave it unset and pass `--smart-compact` per message. Details in [messaging.md](./messaging.md).

Turn both on and an adapter with certified failure evidence keeps a long-running agent's footing through the five-hour wall, a flaky API, and a filling context window, with no babysitter process watching it.

## Every schedule shape

The loops above are points in a small grammar. Each task names one action: `--agent` (a kind, a profile, or a virtual cell like `codex-yolo` or `claude-ping`) for a fresh supervised pane, or `--wake @<handle>` for a running session. It carries a `--prompt` or `--prompt-file`, and it picks one firing shape. The rule from the top of the page decides whether each shape repeats: only `--every` or `--cron` makes it recur.

| Shape | Flags | Repeats? | Example |
| --- | --- | --- | --- |
| One-shot | a bare `--at HH:MM`, or `--in <delay>` | fires once, then the task removes itself | `--in 30m` |
| Interval | `--every <duration>`, measured from the last fire | yes | `--every 15m` |
| Calendar | `--every <days> --at HH:MM`, where days is `day`, `weekday`, `weekend`, a range `mon-fri`, or a list `mon,wed,fri` | on each matching day | `--every weekday --at 07:00` |
| Window-reset | `--every reset` on a `<kind>-ping` agent ([above](#prime-the-budget-window)) | after every budget-window reset | `--agent claude-ping --every reset` |
| Raw cron | `--cron`, a five-field expression | per the expression | `--cron "*/15 * * * *"` |
| Poll-until | `--every <duration>` plus `--check`, `--on`, `--until`, and an agent action ([above](#guard-a-turn-with-a-check)) | until the check trips or the deadline passes | `--check "gh run watch --exit-status" --on success --until 30m --every 2m` |

One pair is worth a second look. `--every 1d` is an interval: it fires a day after the last fire and drifts with it. `--every day --at 07:00` is the calendar's 07:00 sharp.

Calendar times, cron, `--in`, and `--until` resolve in the top-level `timezone`, falling back to the system zone when unset.

The turn itself takes the launch-shaping flags you already know from [the agents guide](./fleet.md): `--worktree` hosts the pane on an isolated branch, `--mode auto|ask|yolo` sets the permission posture ([below](#the-permission-posture-for-unattended-runs)), `--effort` and `--system-prompt-file` shape the agent, `--budget` caps one run, `--budget-per-day` gates future fires, `--surplus` and `--surplus-after` gate fires on the provider's window headroom ([above](#soak-up-the-weekly-surplus)), `--timeout` caps each wait and verify command, `--verify` with `--max-attempts` defines when the task is done, and `--max-strikes` bounds repeated failed fires. A scheduled agent turn without `--timeout` receives the machine's `loop.default-timeout`, two hours by default; manual `rimz loop fire` remains unbounded unless the task sets its own timeout. Inspect, test, and manage tasks with the rest of the surface:

```sh
rimz loop list                 # every task, grouped by project, with next-fire and last-run
rimz loop watch                # live dashboard with countdowns and running tasks
rimz loop show pr-watch        # health, next fire, agent-run rollup, and recent runs
rimz loop logs pr-watch        # full forensics for recent runs
rimz loop fire pr-watch        # fire now in the foreground for testing; the schedule stays put
rimz loop fire pr-watch --keep # leave the transient pane open to inspect
rimz loop pause pr-watch --for 2h
rimz loop resume pr-watch
rimz loop stop pr-watch        # cancel a stuck run and release its overlap lock
rimz loop remove pr-watch
```

`rimz loop fire` streams agent messages as they land, then links the completed run and its transcript.

When a fire reports `previous run still active`, inspect `rimz loop show <name>` for the active run id and holder age, then run `rimz loop stop <name>`. Stop uses the durable cancellation path first and SIGTERM only as a backstop; if the holder still owns the lock, it prints the PID and lock path for manual recovery instead of escalating to SIGKILL.

Pause holds the elder's clock without deleting the task, and `--for` resumes it automatically. The schedule continues from the resume moment instead of replaying missed fires. Pause state belongs to one machine, so pausing a project task affects only your machine; an auto-pause adds its strike reason, and `rimz loop resume` clears that counter. `rimz loop fire` still runs a paused task for testing.

Run mechanics (exit codes, output formats, `wait --stream`) are in [scripting.md](./scripting.md), and every flag is in the [loop CLI reference](../reference/cli/loop.md).

## The permission posture for unattended runs

An unattended run has to answer permission prompts without you, and two patterns compose. The posture you pick is the guardrail layer of the harness, a constraint that the room and the agent's own prompts enforce.

**Answer in the agent's own UI** to keep the full record. A [handler that acts](./notifications.md#handlers-that-act-not-just-alert) sends the answer with `rimz pane send`, leaving the prompt, the answer, and the tool run all in the agent's transcript, exactly as if you had typed it. Prefer this path when handled decisions belong on the record.

**Use the agent's bypass flag** for runs where you accept the tradeoff. `rimz agents <kind> "<prompt>" -p --yolo` passes the adapter's bypass flag (`claude --dangerously-skip-permissions`, `codex --dangerously-bypass-approvals-and-sandbox`), while `--ask` keeps the provider's prompts in place. RimZ still observes sessions, completions, and failures through lifecycle hooks; the tradeoff is that the agent skips permission events at the source, so RimZ's durable record holds what other hooks report rather than a per-decision audit trail.

Reserve the bypass flag for runs where you accept the missing per-decision trail, and keep the guardrails visible: trust grants, notification handlers, and the posture itself are product behavior, covered in [security.md](./security.md).

## A fleet that works nights

A hands-off room composes four layers, from least to most involved: [built-in recovery](#built-in-recovery) keeps one agent alive through rate limits with no schedule at all; a scheduled turn puts work on a clock; a [check guard](#guard-a-turn-with-a-check) fires that turn only when a condition trips; and a [notification handler](./notifications.md) catches what none of them can decide alone. Reach for the lowest layer that solves the problem, and stack them when the job needs it.

[Scripting](./scripting.md) turns an agent into a shell command, and a loop puts that command on a clock. Because a scheduled turn is a full room citizen, it reaches every other primitive: `-p` subagents, teams, worktrees, messages. Stacked, they turn routine work into standing tasks.

```sh
# 07:00 every day: the budget windows are running before you sit down
rimz loop add prime --agent claude-ping --prompt ping --every day --at 07:00

# every 15m: CI on the release PR fixes itself
rimz loop add ci-fix --check "gh run watch --exit-status" --on fail \
    --agent codex --prompt "CI failed on the release PR; read the failing job's logs and fix it" --every 15m

# 02:00 every night: a triage that fans out and opens PRs
rimz loop add nightly --agent claude --worktree nightly --timeout 4h --budget 5 --budget-per-day 20 --every day --at 02:00 \
    --prompt "Scan the repository for bugs and cheap improvements. For each one worth fixing, \
run a codex -p subagent in its own worktree, review its diff, and open a PR."
```

The nightly task is one scheduled turn, but its prompt hands the agent the room's own tools: it fans work out with [`-p` subagents](./scripting.md#agents-scripting-agents), isolates each fix in a [worktree](./worktrees.md), and could as easily launch a [team](./teams.md) and brief it over [messages](./messaging.md). The unit of automation stops being a prompt you type and becomes a standing cycle that checks, acts, and re-arms itself.

The rest of the harness keeps that cycle safe while you sleep: [auto-continue](#built-in-recovery) carries certified recoverable runs over rate limits, checks fire agent turns only when there is work, every fire appears in `rimz loop show` and stays inspectable through `rimz loop logs`, a question or failure trips a [notification handler](./notifications.md) that reaches your phone, and the [permission posture](#the-permission-posture-for-unattended-runs) is a per-task choice rather than a global switch. Leave the room open, detached on your workstation or on a server you reach with [`rimz remote`](./remote.md), and by morning `rimz loop list` and the PR queue show what the night produced.

## See also

- [Scripting agents](./scripting.md) — the supervised-run mechanics every scheduled `--agent` task rides on: exit codes, `--output-format`, `wait --stream`.
- [Budgets](./budget.md) — the dollar caps that bound hands-off work, and the surplus gate's headroom model.
- [Notifications](./notifications.md) — the push routes and acting handlers that catch what a loop cannot handle alone.
- [Messaging](./messaging.md) — the delivery path `--wake` uses, `--schedule` for one-off reminders, and smart compaction in full.
- [Loop CLI](../reference/cli/loop.md) — every flag on `add`, `fire`, `list`, `show`, `rename`, and `remove`.
- [Configuration](./configuration.md) — the `[resume]` and `[harness]` keys, and the `loop.toml` shape.
- [Security and trust](./security.md) — the safety posture for bypass flags and project trust.
- [harness.md → Scheduled turns](../internals/harness/harness.md#scheduled-turns-loop) — the clock, state files, and run log underneath.
