# Loop CLI

`rimz loop` puts agent turns on a clock. A task is a durable schedule: `rimz loop add` writes recurring machine tasks to `loop.toml`, writes project tasks to the repo's `.rimz/config.toml`, and stores one-shots and poll-until deadlines in state. The room's sidebar elder fires tasks while a room for the task's project is open. Nothing fires when no room is open, and `loop remove` deletes the task from whichever store owns it. A task uses `--agent` to spawn one supervised transient pane, `--wake` to deliver a prompt to one live agent session through the message path, `--check` to run a scheduled command, or `--check` as a guard before an agent action. Why you schedule turns, prime budget windows, and guard with watchdogs is the [loops guide](../../guide/loops.md).

```sh
rimz loop add morning --agent claude-ping --prompt ping --every weekday --at 07:00
rimz loop add weekly-prime --agent claude-ping --prompt ping --every reset
rimz loop add pr-watch --agent codex --prompt "check CI on the release PR" --every 15m --mode auto --root .
rimz loop add nightly --agent codex --prompt "triage issues" --every day --at 02:00 --budget 5 --budget-per-day 15
rimz loop add self-wake --wake @planner --prompt "resume the review and fix the next blocking comment" --in 30m --root .
rimz loop add watchdog --check "cargo test" --on fail --agent codex --prompt "fix the failing test" --every 15m
rimz loop add auth-fix --agent codex --prompt "fix auth" --verify "cargo xtask test auth" --max-attempts 3 --every day --at 02:00
rimz loop add ci-green --check "gh run watch --exit-status" --on success --until 30m --every 2m --wake @planner --prompt "CI is green; merge"
rimz loop add repo-prime --project --agent codex-ping --prompt ping --every day --at 08:00
rimz loop fire pr-watch
rimz loop rename pr-watch ci-watch
rimz loop pause pr-watch --for 2h
rimz loop resume pr-watch
rimz loop list
rimz loop show pr-watch
rimz loop remove pr-watch
```

## Schedule shapes

Schedules repeat only with `--every` or `--cron`. Shapes are: one-shot (`--at 07:00` or `--in 30m`), interval (`--every 15m`), calendar (`--every weekday --at 07:00`), raw cron (`--cron`), window-reset (`--every reset` on a `<kind>-ping` agent), and poll-until (`--every`, `--check`, `--on`, `--until`, plus an agent action). Calendar, cron, `--in`, and `--until` resolution use the top-level `timezone`, falling back to the system zone when unset.

A `<kind>-ping` agent is the window-primer: the run skips when the provider's window is already counting down. `--every reset` fires that ping one minute after the provider's longest observed budget window resets, then uses the ping turn's own cache refresh as the next occurrence.

`--budget <AMOUNT[/day]>` caps each spawned agent run. `--budget-per-day <AMOUNT>` requires a per-run budget, sums that task's cost-bearing run records in the configured local day, and records a skip when the remaining amount cannot fund the next run's cap. `loop list` prints today's spend in its COST column, suffixed by the daily cap when configured; `loop show` prints the configured budgets and a `spend` line with today's spend, the last run cost, and the rolling average over up to ten costed runs.

`--surplus <RATIO>` and `--surplus-after <DURATION>` gate `--agent` and `--wake` actions on the provider's longest budget window; check-only tasks reject them. Headroom is `(remaining budget share) / (remaining time share)`, so `1.0x` is the sustainable pace and `--surplus 1.5x` requires half again that headroom. The elapsed floor is checked first, and `--surplus-after 3d` by itself also implies a `1.0x` minimum. A missing, incomplete, expired, or not-started window keeps the gate closed; API-key accounts without subscription-window readings therefore always skip. Each closed fire records strike-neutral `surplus skipped`, and `loop show` prints the configured gate and its recorded reason.

`--max-strikes <N>` auto-pauses the task after that many consecutive failed fires. It defaults to `3`; `0` disables auto-pause. A failed, timed-out, errored, budget-exceeded, or verify-failed result counts, as does a completed or delivered action whose check still failed; a successful action or healthy check resets the counter.

## Wakes and checks

`--wake @<handle>` resolves the address immediately and pins the exact session id; if that session is gone when the task fires, RimZ skips delivery and removes the schedule.

`--check` runs at the project root; `--on fail` wakes on non-zero exit or timeout, while `--on success` wakes on zero exit.

`--verify <CMD>` gives an `--agent` task a completion condition after its supervised turn; `--max-attempts <N>` caps total turns and defaults to `3`. Verification is unavailable for `--wake` and check-only tasks because those actions have no supervised session to re-prompt.

## Machine, project, and state tasks

`rimz loop add` writes repeating tasks to the per-machine `loop.toml` by default. RimZ-generated `--in`, bare `--at`, and `--until` tasks persist as state, not `loop.toml` config, so they clear themselves when they retire.

`--project` writes `[tasks.<name>]` to `.rimz/config.toml` instead: it omits `root` because the project root is implicit, rejects `--wake` and `--until`, requires `--every` or `--cron`, and prints the `rimz trust grant` follow-up after add, remove, or rename. Trusted project tasks win over same-named machine tasks; an untrusted project task does not fire, and during the untrusted window a same-named machine task keeps running. Project tasks ship in the repo, so they run only on a machine that has [granted trust](./hooks-trust.md#project-trust).

## Pause and resume

`loop pause <name>` holds a task until `loop resume <name>` lifts the pause. `--for <duration>` uses the `s`, `m`, `h`, and `d` duration units and resumes automatically; a pause without `--for` is indefinite. Resumed schedules continue from the resume moment, so interval, calendar, and cron tasks do not replay fires missed during the pause.

Pause is per-machine state. Pausing a project task affects only this machine and does not edit the trust-hashed project config. Reaching the strike threshold writes the same machine pause state with a strike reason and fires `loop_paused` notification handlers; `loop resume` clears the counter. `loop fire <name>` remains the manual testing hatch: it reports the pause, then runs the task anyway.

## Fire, list, show, rename

`loop fire <name>` runs the task now in the foreground with the same check guard, window skip, overlap guard, and run-log record as a scheduled fire. It opens with the task's check-to-action rule, gutters the check's live output, streams the agent's replies into the same gutter, closes each stage with a glyph verdict, links successful supervised runs by run id and transcript, hints `--keep` when the transient pane closes, and keeps one-shot entries and wake schedules in place; `--keep` leaves the transient supervised pane open for inspection.

A task that is already running records `overlapped` and skips instead of stacking another run. `loop rename` moves the task key in its store; the task then re-arms, so an interval task next fires one interval later.

`loop list`, `loop watch`, and `loop show` read only. `loop list` groups tasks by project root with room state in the section header, then shows name, task, source, schedule, last-run age, status, today's COST, and next fire; timed pauses show their automatic resume time and strike pauses show `paused · N strikes`. `loop watch` holds a live dashboard open, repainting next-fire countdowns and the `running now` state every second; the `rimzd` loop panel runs it with `--hold`. Source values are `machine`, `project`, `project · untrusted`, `project · stale`, and `state`. `loop show <name>` opens with one task's schedule, pause state or next fire, task, check, root, source with the defining file path, configured budgets, spend trend, and any active pre-threshold `strikes N/max`, then prints recent runs with per-run costs and fresh input/output tokens plus stored details such as check output, error chains, run ids, captured pane output tails, and transcript links.

The task model and config shape are in [harness.md → Scheduled turns](../../internals/harness/harness.md#scheduled-turns-loop).
