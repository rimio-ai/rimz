# Dependabot repair loop

This repository's dependency loop queries all open GitHub Dependabot PRs and starts Astra only when a failed update needs work. It opens one replacement PR for a batch and leaves merging to a maintainer. The replacement body includes `Closes #N` references so GitHub closes its source PRs on merge.

## Two levels of worktree isolation

Keep the coordinator in a dedicated `dependabot-loop` linked worktree. Each repair runs in its own named worktree through `rimz agents astra … -w deps/repair-309-310 -p`. The coordinator refuses to run from the primary checkout or another branch. The repair prompt checks its own worktree and branch before writing. Worktree creation follows the configured RimZ base policy; the worker must reconcile its selected updates with the target base before publishing.

Each repository loop owns a directory under [loops/](../../loops/README.md). This task keeps its [Python coordinator](../../loops/dependabot/dependabot.py), [worker prompt](../../loops/dependabot/prompt.md), and tests together in `loops/dependabot/`. The schedule is declared directly in [.rimz/config.toml](../../.rimz/config.toml), without an installer or machine-local task definition. Project check commands start at the project root: the read-only `dispatch` action finds the `dependabot-loop` branch in Git's NUL-delimited worktree inventory, changes into that worktree, and replaces itself with the coordinator through `uv`. PR queries, locking, and worker launches happen only after that handoff. Missing control worktrees or loop code fail with setup instructions instead of running repairs in the primary checkout. The worktree path is discovered, not hard-coded.

## Inspect, enable, and run

Run the inspection commands from the dedicated control worktree that contains this implementation. Create that worktree with `rimz worktree new dependabot-loop` if it does not exist, and ensure it contains this loop code before enabling the task. Both Python scripts declare `requires-python = ">=3.14"` in their inline metadata; `uv` selects a compatible interpreter, with no third-party Python dependencies. Python 3.14's argument parser supplies typo suggestions and explicitly uncolored help for loop logs. GitHub CLI authentication, writable tool caches, the configured `astra` profile, and working Codex lifecycle hooks are required for repair launches.

```sh
uv run --script loops/dependabot/tests/test_dependabot.py
uv run --script loops/dependabot/dependabot.py plan > /tmp/rimz-dependabot-plan.json
jq . /tmp/rimz-dependabot-plan.json
```

Once this config is present in the primary project and the worker prerequisites are ready, review and grant project trust with `rimz trust grant`, then run `rimz loop enable dependabot-repair`. A manual `rimz loop fire dependabot-repair` runs even when disabled, so use it only when ready to launch a repair. Project tasks require both trust and machine-local enablement. RimZ resolves them once for the project root, not once per worktree; the repository-common lock and live-worker check also protect manual dispatches from other worktrees. Remove any old machine-local definition during migration so it cannot act as a fallback while project trust is stale.

`plan` is read-only and prints the selected action and its evidence. `run` takes a repository-common advisory lock, re-queries the forge, and launches one supervised Astra worker when the plan calls for repair. Manual and scheduled invocations use this same entry point. Do not launch a repair worker directly alongside it; direct agent launches do not acquire the coordinator lock. Query or launch failures return nonzero; no work and an already running coordinator return success without a new agent.

The project task checks every 8 hours with a two-hour command timeout. Each forge or inventory query has a two-minute timeout, and the supervised worker has a shorter 60-minute wait cap. This is a check-only loop whose command launches the selected worker, not a static `--agent` task: loop logs record the command outcome, while the agent's own supervised run records its transcript and usage. Loop-level agent budgets and verify retries do not apply to the nested worker.

A room must remain open for the control worktree's project, or the machine's optional `rimz loop timer install` must keep time. The project config does not install a timer. The first manual fire exercises the same locking and selection as scheduled fires.

## Duplicate prevention and recovery

The sorted source PR numbers identify a batch: `deps/repair-309-310` and `<!-- rimz-dependabot-repair:v1 sources=309,310 -->` name the same replacement. A second body marker, `<!-- rimz-dependabot-heads:v1 sources=309@FULL_SHA,310@FULL_SHA -->`, records the reviewed source revisions. The coordinator reads PR history as well as open PRs, so a rejected replacement does not become a new batch on the next tick. Existing open replacement work takes priority over new updates. Pending or successful replacement CI spends no agent turn while source revisions remain unchanged; failed CI or changed source revisions resume that same worktree and PR. Sources not yet covered wait for a later batch.

Git branches also preserve identity before PR creation. An interrupted branch can resume rather than create a differently named replacement. Ambiguous or malformed repair identities stop the coordinator for inspection. The lock serializes coordinators in worktrees sharing the same Git common directory; it is not a distributed lock across independent clones. Enable the task on only one machine for this repository. Before launching, the coordinator also checks all live agent cards: a surviving pane on any repair branch defers the next worker even if its earlier supervisor died. The prompt checks for an existing PR again before creation, including recovery from an uncertain API response.

Only failures on a PR's current head qualify as repair evidence. Missing or pending checks do not count as success or as a dependency failure. CI is re-read at every fire; an agent's local green gate does not prove remote CI is green.

## Operate and stop

```sh
rimz loop show dependabot-repair
rimz loop logs dependabot-repair --failed
rimz loop disable dependabot-repair
rimz loop stop dependabot-repair
rimz loop remove dependabot-repair
```

Disabling prevents future fires; stopping requests cancellation of the running loop command. A nested worker may need to be stopped separately through the agents surface; inspect its card before removing worktrees. A worker that ends without a correctly identified replacement PR, or leaves completed CI failed, makes the command fail rather than reporting a successful repair. Pending or missing checks remain a wait, not a claim of green CI. Three consecutive failed fires disable the task. After addressing the reported failure, `rimz loop enable dependabot-repair` clears the strikes. Removing the task preserves its run history and any unmerged repair worktree. Keep the control worktree while the task refers to it; update or remove the task before removing that worktree.

## See also

- [Loops](../guide/loops.md) — scheduling, timekeeping, and command run history.
- [Worktrees](../guide/worktrees.md) — creation, base selection, and cleanup.
- [Rust conventions](./rust-conventions.md) — verification gates and contributor automation.
