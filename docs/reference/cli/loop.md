# Loop CLI

`rimz loop` puts agent turns on a clock. A task is a durable schedule: `rimz loop add` writes recurring machine tasks to `loop.toml`, writes project tasks to the repo's `.rimz/config.toml`, and stores one-shots and poll-until deadlines in state. The room's sidebar elder fires tasks while a room for the task's project is open. Nothing fires when no room is open, and `loop remove` deletes the task from whichever store owns it. A task uses `--agent` to spawn one supervised transient pane, `--wake` to deliver a prompt to one live agent session through the message path, `--check` to run a scheduled command, or `--check` as a guard before an agent action. Why you schedule turns, guard them with watchdogs, and pace them against the provider's window is the [loops guide](../../guide/loops.md).

```sh
rimz loop add morning --agent claude --prompt "summarize what landed on main overnight" --every weekday --at 07:00
rimz loop add pr-watch --agent codex --prompt "check CI on the release PR" --every 15m --mode auto --root .
rimz loop add nightly --agent codex --prompt "triage issues" --every day --at 02:00 --budget 5 --budget-per-day 15
rimz loop add self-wake --wake @planner --prompt "resume the review and fix the next blocking comment" --in 30m --root .
rimz loop add watchdog --check "cargo test" --on fail --agent codex --prompt "fix the failing test" --every 15m
rimz loop add auth-fix --agent codex --prompt "fix auth" --verify "cargo xtask test auth" --max-attempts 3 --every day --at 02:00
rimz loop add ci-green --check "gh run watch --exit-status" --on success --until 30m --every 2m --wake @planner --prompt "CI is green; merge"
rimz loop add repo-audit --project --agent codex --prompt "audit the dependency lockfile for advisories" --every day --at 08:00
rimz loop fire pr-watch
rimz loop rename pr-watch ci-watch
rimz loop enable pr-watch
rimz loop disable pr-watch
rimz loop pause pr-watch --for 2h
rimz loop stop pr-watch
rimz loop list
rimz loop show pr-watch
rimz loop logs pr-watch --failed
rimz loop remove pr-watch
```

## Schedule shapes

Schedules repeat only with `--every` or `--cron`. Shapes are: one-shot (`--at 07:00` or `--in 30m`), interval (`--every 15m`), calendar (`--every weekday --at 07:00`), raw cron (`--cron`), and poll-until (`--every`, `--check`, `--on`, `--until`, plus an agent action). Calendar, cron, `--in`, and `--until` resolution use the top-level `timezone`, falling back to the system zone when unset.

`--budget <AMOUNT[/day]>` caps each spawned agent run. `--budget-per-day <AMOUNT>` requires a per-run budget, sums that task's cost-bearing run records in the configured local day, and records a skip when the remaining amount cannot fund the next run's cap. `loop list` prints today's spend in its COST column, suffixed by the daily cap when configured. For a check-gated agent or wake task, `loop show` keeps the `spend` line focused on today's daily-cap progress and puts all-record total and average cost in `AGENT RUNS`; other task shapes keep the last-run and rolling ten-run average on `spend`.

`--surplus <RATIO>` and `--surplus-after <DURATION>` gate `--agent` and `--wake` actions on the provider's longest budget window; check-only tasks reject them. Headroom is `(remaining budget share) / (remaining time share)`, so `1.0x` is the sustainable pace and `--surplus 1.5x` requires half again that headroom. The elapsed floor is checked first, and `--surplus-after 3d` by itself also implies a `1.0x` minimum. A missing, incomplete, expired, or not-started window keeps the gate closed; API-key accounts without subscription-window readings therefore always skip. Fresh managed Qwen agent actions use only their exact-account 7-day window for surplus; Qwen wakes have no fresh launch binding and fail closed, while the 30-day window remains an exhaustion gate rather than a pacing clock. Each closed fire records strike-neutral `surplus skipped`, and `loop show` prints the configured gate and its recorded reason. The headroom model with a worked example is [budgets → the surplus gate](../../guide/budget.md#the-surplus-gate).

A matching exhausted Qwen 5-hour, 7-day, or 30-day window records strike-neutral `budget skipped` before any check command or pane creation. The reason names the Alibaba region, window, usage, and reset without exposing the account fingerprint.

`--max-strikes <N>` auto-disables the task after that many consecutive failed fires. It defaults to `3`; `0` disables the strike gate. A failed, timed-out, errored, budget-exceeded, or verify-failed result counts, as does a completed or delivered action whose check still failed; a successful action or healthy check resets the counter.

`--timeout <DURATION>` caps an explicit supervised wait and its verify commands. Scheduled `--agent` runs without this task-specific value use `loop.default-timeout`, which defaults to `2h`; set it with `rimz config set loop.default-timeout 3h`. Manual `loop fire` stays unbounded when the task has no `--timeout`, while checks retain their five-minute default.

## Wakes and checks

`--wake @<handle>` resolves the address immediately and pins the exact session id; if that session is gone when the task fires, RimZ skips delivery and removes the schedule.

`--check` runs at the project root; `--on fail` wakes on non-zero exit or timeout, while `--on success` wakes on zero exit.

`--verify <CMD>` gives an `--agent` task a completion condition after its supervised turn; `--max-attempts <N>` caps total turns and defaults to `3`. Verification is unavailable for `--wake` and check-only tasks because those actions have no supervised session to re-prompt.

## Machine, project, and state tasks

`rimz loop add` writes repeating tasks to the per-machine `loop.toml` by default. RimZ-generated `--in`, bare `--at`, and `--until` tasks persist as state, not `loop.toml` config, so they clear themselves when they retire.

`--project` writes `[tasks.<name>]` to `.rimz/config.toml` instead: it omits `root` because the project root is implicit, rejects `--wake` and `--until`, requires `--every` or `--cron`, and prints the `rimz trust grant` follow-up after add, remove, or rename. Trusted project tasks win over same-named machine tasks; an untrusted project task does not fire, and during the untrusted window a same-named machine task keeps running. Project tasks ship in the repo, so they need both [project trust](./hooks-trust.md#project-trust) and a machine-local `rimz loop enable <name>` before they run unattended. `rimz loop add --project` enables the task on the authoring machine.

## Enable, disable, and pause

`loop enable <name>` arms a task locally and clears a disable, a timed pause, and its strike counter. `loop disable <name>` holds it until the next explicit enable. Both accept `--all` instead of a name to affect every machine, state, and current-project task shown by `loop list`.

`loop pause <name> --for <duration>` applies a bounded hold using the `s`, `m`, `h`, and `d` units; `--for` is required because `disable` owns the indefinite case. All three commands write per-machine state and never edit a trust-hashed project config. Enabling or reaching the end of a pause becomes the new schedule edge, so interval, calendar, and cron tasks do not replay missed fires. Reaching the strike threshold disables the task and fires `loop_disabled` notification handlers. `loop fire <name>` remains the manual testing hatch: it reports a disable or pause, then runs the task anyway.

## Fire, stop, list, show, logs, rename

`loop fire <name>` runs the task now in the foreground with the same check guard, window skip, overlap guard, and run-log record as a scheduled fire. It opens with the task's check-to-action rule, gutters the check's live output, streams the agent's replies into the same gutter, closes each stage with a glyph verdict, links successful supervised runs by run id and transcript, hints `--keep` when the transient pane closes, and keeps one-shot entries and wake schedules in place; `--keep` leaves the transient supervised pane open for inspection.

A task that is already running records `overlapped` and skips instead of stacking another run. `loop stop <name>` first marks a linked supervised run canceled, wakes its waiter, and gives it a short grace to release the overlap lock; a remaining holder receives SIGTERM and another grace, while SIGKILL stays a manual operator decision. The command exits `0` after a stop or when no run is active, and exits `1` with the holder PID and lock path when the lock remains held. `loop rename` moves the task key in its store; the task then re-arms, so an interval task next fires one interval later.

`loop list`, `loop watch`, `loop show`, and `loop logs` read only. `loop list` groups tasks by project root with room state in the section header, then shows name, task, source, schedule, last-run age, status, today's COST, and next fire; states include `disabled`, `disabled · N strikes`, `disabled · enable to arm`, and `paused · <time>`. `loop watch` holds a live dashboard open, repainting next-fire countdowns and the `running now` state every second; the `rimzd` loop panel runs it with `--hold`, which ignores `q` and Ctrl-C, and the sidebar elder restores the panel if its pane closes while the view survives. Source values are `machine`, `project`, `project · untrusted`, `project · stale`, and `state`.

`loop show <name>` opens with one task's schedule, held state or next fire, a health verdict from the latest conclusive run, task facts, effective timeout, configured budgets, spend trend, and any active pre-threshold `strikes N/max`; an active linked run includes its run id and the matching `loop stop` command. Check-gated agent and wake tasks add `AGENT RUNS`, which counts escalation attempts against all recorded fires, aggregates their valid costs across the rotation-bounded log, and lists the five latest attempts without collapsing them. `RECENT RUNS` retains the compact collapsed history, `LAST RUN` retains its full stored detail, and an older last failure becomes a one-line pointer to `loop logs`.

`loop logs <name>` prints complete forensic blocks for the latest ten records, oldest first so the newest finishes the stream. `-n, --runs <N>` changes the limit, and `--failed` keeps only failed, timed-out, budget-exceeded, verify-failed, and errored records. Each block includes the status, age, mode, exit information, check output, error chains, run ids, captured pane output tails, costs and tokens, and transcript links when recorded.

The task model and config shape are in [loops.md](../../internals/harness/loops.md).
