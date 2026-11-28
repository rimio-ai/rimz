# Steer a workflow run

These are `rimz workflow` commands; the `workflow` tool only starts a run. To try one node, `rimz workflow run <name> --once [--at <node>] [--input '<json>']` ends the run once that node has answered and names the route it would take.

```sh
rimz workflow list                              # unfinished runs in every room: the node, what it waits on or asks
rimz workflow show <run>                        # position, readings, visit counts, newest hops, the work dir
rimz workflow show <run> --history              # every attempt and answer in order: exits, logs, reasons
rimz workflow resume <run> --at pr              # move a live run to a node with a fresh timeout; the attempt it leaves ends canceled
rimz workflow extend <run> fix --visits +2      # two more visits on step fix (no step: the one the run is at); the run stays put
rimz workflow extend <run> --deadline 4h        # replace the run's deadline, re-arming one that fired
rimz workflow wait <run>                        # block until it ends; exits 3 at once when it needs a person
rimz workflow restart <run>                     # a fresh agent for the current agent step, its visits back to 1
rimz workflow restart <run> --continue          # the same, resuming the agent's conversation
rimz workflow cancel <run>                      # end it and stop its live check, command, or agents
```

Every command here except `list`, `show`, and `wait` needs the user's approval. `resume --at` refuses a step that has spent its visits and prints the `extend` command that grants one.

- Inside a step or one of its subagents, leave out `<run>`: `show` and `--history` find that run.
- To change what a step does from its next launch on, in this run alone, append to its sign, the `sign` path `show` prints while the run is at that step. Each later agent or message launch of that step reads it; to reach the attempt already running, message its agent.
- `cancel` keeps the commits, the uncommitted edits, and the work directory. `rimz worktree remove --force` on a run's worktree cancels the run too. The runs a runs step started keep running when their parent is canceled; cancel each by the name the cancel receipt lists it under. When a loop task started the run, `rimz loop disable <task>` keeps the task from starting another.
- After seven days garbage collection may remove a finished run's work directory, so copy out anything there worth keeping.
