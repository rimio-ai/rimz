# Managing a booking

`tasks` lists what you booked and `stop` removes it. Everything else runs through `rimz loop`:

```bash
rimz loop list                 # this room's tasks; --all for every room
rimz loop show <name>          # health, next fire, recent runs, what is still alive
rimz loop logs <name>          # each run's output and trigger payload
rimz loop explain '<expr>'     # a condition's current reading, without adding a task
rimz loop enable <name>        # arm, clearing a disable, pause, or strikes; --all
rimz loop disable <name>
rimz loop pause <name> --for 2h
rimz loop stop <name>          # cancel the live run, and stop every agent and workflow run its fires launched
rimz loop rename <old> <new>
```

To halt a booking now, disable it, then stop it. A strike is a fire that fails: a launch that fails, or a workflow run that does not end done. A disabled booking shows under NEEDS YOU in `rimz loop list`, and no agent is told.

Where the output of these commands and this text disagree, the output is right.

## What fires a task

- An open room (the rimz session for one project), attached or not, fires the project's time and condition tasks.
- With no room open, the external timer fires the same tasks, and a fire opens the room. `rimz loop timer status` says whether it is installed. `rimz loop timer install` adds it, a per-minute systemd or launchd timer on the user's account, and `rimz loop timer remove` uninstalls it.
- The process that emits a signal fires the signal tasks, open room or not. The room emits most signals (its CI and PR poll, its agents' lifecycle), so those stop while it is closed.

The timer reads no forge, so a `ci` or `pr` term stays unknown under it. A task with such a term, or on a signal the room emits, waits for an open room with or without the timer.

## Fire it now

```bash
rimz loop fire <name>
rimz loop show <name>
```

The fire runs the task now, without its trigger. It runs a disabled or paused task too, ignores the task's `once-per-checkout` memory, and leaves a task that fires once in place. Under `--each-worktree` it fires only the worktree your shell is in. A shell or workflow run's final outcome reaches `rimz loop show` and `rimz loop logs` when it ends.

For a signal task, `--signal <name>` gives the fire a pretend signal, so the command or prompt gets the evidence line a real one would, and `--json '<object>'` adds its payload. Nothing is emitted, so no other task or workflow wakes.

```bash
rimz loop fire ci-fix --signal ci.failed --json '{"branch":"feat-x"}'
```

## A task that stopped firing

`rimz loop show <name>` names what holds it (a launch still alive, a disable, a pause) and its last outcomes. `rimz loop timer status` says whether anything kept time with no room open.
