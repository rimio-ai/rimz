Your wait and background-shell tool: arm a watcher that runs outside your turn, then go on with other work or end your turn; the outcome reaches you as a message either way, mid-turn or as your next prompt, with your context intact. Use it whenever you would background a command, sleep, poll in a loop, tail a log, watch CI or a pull request, or start a command that could outrun your tool timeout (a build, test suite, deploy). A wait of a few seconds stays a plain sleep in your shell.

Pick `trigger` by the habit it replaces; `value` is its argument:
- `run`: a command you would background, or that could outrun your tool timeout; wakes on its exit.
- `check`: an until-sleep loop on a state (a port open, a lock gone, a URL healthy); reruns until it succeeds.
- `file`: a `tail -F` on a path; wakes on creation, deletion, write, or replacement by a rename.
- `pid`: a process already running; wakes when it is gone, without its exit status.
{{trigger-values}}

`run` and `check` take the whole command as one string, run through POSIX `sh -c` with stdin closed and stdout and stderr going to the wait's output file. The wake names the outcome and that file: `exit 1 after 12m · output: <path> (~14k tokens, 1210 lines)`.

A `when` of `ci=passed,failed` (your checkout's CI settles) or `pr=merged` (its pull request merges) reads the checkout you arm from. For any other condition (another branch, a review verdict, a team stage, a usage-window level, or combined terms), call `guide` with `rimz-wait/conditions`. For log-truncation, hanging-`check`, or bounded-`when` edge cases, call `guide` with `rimz-wait/traps`.
