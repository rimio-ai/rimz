Your wait and background-shell tool: arm a watcher that runs outside your turn, then go on with other work or end your turn; the outcome reaches you as a message either way, mid-turn or as your next prompt, with your context intact. Use it at the moment you would run a command in the background, put a sleep in a command, write an until or watch loop, tail a log, poll CI or a pull request, or start a command that could outrun your tool timeout (a build, a test suite, a deploy). A wait of a few seconds stays a plain sleep in your shell.

Pick the `trigger` by the habit it replaces; `value` is its argument:
- `run`: a command you would background, or that could outrun your tool timeout; wakes on its exit, either outcome unless `on` names one (`on: fail` for a server you leave running).
- `check`: an until-sleep loop on a state (a port open, a lock gone, a URL healthy); reruns the command every `every` (`1s`; use `30s` for anything that polls another host) until it succeeds, or until it fails with `on: fail`.
- `file`: a `tail -F | grep` on an event; fires on any change to the path (creation, deletion, write, replacement by a rename), and with `grep` on the first new complete line holding that literal, case-sensitive text. A relative path resolves from `cwd`, not the checkout root. A rerun that truncates and regrows the file within a second hides the lines before the old end, so remove a log before starting a command that rewrites it.
- `pid`: a process already running, waking when it is gone, without its exit status.
- `when`: a condition, firing once when it reads true, already true at the arm included.
- `in`: a sleep, one number and unit (`90s`, `20m`, `2h`, `1d`), rounded up to the next whole minute.
- `at`: a clock time in the configured time zone (the `timezone` setting), the next one ahead: `HH:MM`, `<weekday> HH:MM`, or `YYYY-MM-DD HH:MM`.

`run` and `check` take the whole command as one string, even a two-word one, run through POSIX `sh -c` at the root of the checkout, with stdin closed and stdout and stderr going to the wait's output file. The wake names the outcome and that file: `exit 1 after 12m · output: <path> (~14k tokens, 1210 lines)`.

A `when`, `in`, or `at` wake carries only what fired, so give it a `label` (1 to 60 characters) saying what you will do on waking. When a `run`, `check`, `file`, or `pid` watch could hang, give it a `check_in` longer than the watch should take: one still-running notice whatever `on` says, never a stop. A `check` command that hangs delays its notice, so give it a timeout of its own. A `when` takes no `check_in`: to bound a condition that may never come true, arm an `in` wait beside it and cancel whichever is left.

A `when` of `ci=passed,failed` (your checkout's CI settles) or `pr=merged` (its pull request merges) reads the checkout you arm from. Before you write any other expression (another branch, a review verdict, a team stage, a usage-window level, or terms combined), call `guide` with `rimz-wait/conditions`.
