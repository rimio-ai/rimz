Arm a watcher that runs outside your turn and wakes you with a message: go on working or end your turn, and the outcome arrives mid-turn or as your next prompt. Use it where you would background a command, sleep, write an until or watch loop, tail a log, or poll CI or a pull request; a wait of a few seconds stays a plain sleep in your shell.

Pick the `trigger` by the habit it replaces; `value` is its argument:
- `run`: a command you would background or that could outrun your tool timeout, run once; wakes on its exit, either outcome unless `on` names one.
- `check`: an until-sleep loop on a state (a port open, a lock gone); reruns it every `every` until it succeeds (or fails, with `on: fail`). Use `every: 30s` against another host.
- `file`: a path, firing when it is created, written, deleted, or replaced; with `grep`, on the first new complete line holding that literal, case-sensitive text. A relative path resolves from `cwd`, not the checkout root. Remove a log before rerunning a command that truncates and rewrites it.
- `pid`: a running process, waking when it is gone, without its exit status.
- `when`: a condition, firing once when it reads true, at the arm included.
- `in`: a delay, one number and unit (`90s`, `2h`, `1d`), rounded up to a whole minute.
- `at`: the next such clock time in the configured time zone: `HH:MM`, `<weekday> HH:MM`, or `YYYY-MM-DD HH:MM`.

`run` and `check` take the whole command as one string, run through `sh -c` at the checkout root with stdin closed and stdout and stderr going to the wait's output file, whose path the wake gives.

A `when`, `in`, or `at` wake carries only what fired, so give it a `label` saying what you will do on waking. When a `run`, `file`, or `pid` watch could hang, give it a `check_in` longer than it should take: one still-running notice, never a stop. A `check` probe that hangs sends no notice, so give its command a timeout of its own. A `when` takes no `check_in`; bound it with an `in` wait beside it and cancel whichever is left.

A `when` value of `ci=passed,failed` or `pr=merged,closed` reads the checkout you arm from (`ci`: passed, failed, pending, none; `pr`: open, merged, closed, none). Before writing any other `when` expression, call `guide` with `rimz-wait/conditions` for its keys and grammar.
