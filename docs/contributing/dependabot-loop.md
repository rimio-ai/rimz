# Dependabot repair loop

This repository's dependency loop queries all open GitHub Dependabot PRs and starts Astra only when a failed update needs work. It opens one replacement PR for a batch and leaves merging to a maintainer. The replacement body includes `Closes #N` references so GitHub closes its source PRs on merge.

## Attempt checkouts

Each repository loop owns a directory under [loops/](../../loops/README.md). This task keeps its [Python coordinator](../../loops/dependabot/dependabot.py), [worker prompt](../../loops/dependabot/prompt.md), and tests together in `loops/dependabot/`. The schedule is declared directly in [.rimz/config.toml](../../.rimz/config.toml), without an installer or machine-local task definition. The scheduled command is `uv run --no-project --script loops/dependabot/dependabot.py run`, started at the project root.

The coordinator never edits files, so it runs read-only at the project root, where project check commands start; `scripts/sync-repo` keeps that primary checkout on `origin/main`. Only a selected repair attempt gets a worktree, and it is fresh for that attempt.

`run` takes the repository-common lock, checks the live agent cards for an occupied repair lane, runs `git fetch origin`, and re-plans. When the plan calls for repair, it resolves the attempt checkout in this order, printing one `attempt_checkout` JSON line (`resumed` or `created`, with `path` and `branch`):

1. A worktree already on the batch branch holds a previous attempt's unfinished work: resume it.
2. When `origin/<branch>` does not exist, publish it at the default base: `git push origin origin/<default_base>:refs/heads/<branch>`, then `git fetch origin <branch>`.
3. A local `<branch>` without a worktree whose tip is already on `origin/<branch>` is deleted. One holding commits absent from origin stops the coordinator with `push or delete it`: inspect it with `git log origin/<branch>..<branch>`, then push or delete it by hand.
4. `rimz worktree new <branch> --base origin/<branch>` cuts the checkout; RimZ derives the `deps-repair-…` directory and channel from the name.

The worker then launches into that tree with `rimz agents astra … -w <branch> --yolo -p --timeout 60m`. The worker explicitly uses unattended permissions so Codex can reach GitHub and write tool caches outside its worktree without an approval interaction; the default Astra profile is unchanged. This belongs on the nested agent launch, not the check-only task's `mode`. The repair prompt checks its own worktree and branch before writing, and reconciles the selected updates with the target base before publishing.

Because the marker's base is `origin/<branch>`, "landed" means every commit is pushed. RimZ's wrapper cleanup removes a clean, pushed tree as soon as the supervised worker exits, and `rimz gc` reclaims one the wrapper could not. After the launch returns, whatever its exit, the coordinator settles the checkout and prints one `attempt_checkout` line: `removed` when RimZ already reclaimed it, `kept` with reason `worker still live` when a repair pane is still open, and otherwise one non-forced `rimz worktree remove <branch>`, reporting `removed` or `kept` with RimZ's refusal (in use, or local changes or work not proven landed). A kept tree is resumed by the next fire. Automation never forces a removal, so unpushed work survives until a worker pushes it or a human discards it.

Worktrees on other `deps/repair-*` branches are reported as `stale_attempt_checkouts` and left alone: `rimz gc` reclaims them once their work is landed, or a human clears them. List them with `rimz worktree list`; discard a rejected batch's tree with `rimz worktree remove deps/repair-N --force`.

## Inspect, enable, and run

Run the inspection commands from any checkout of the repository that contains this implementation. Both Python scripts declare `requires-python = ">=3.14"` in their inline metadata; `uv` selects a compatible interpreter, with no third-party Python dependencies. Python 3.14's argument parser supplies typo suggestions and explicitly uncolored help for loop logs. GitHub CLI authentication, writable tool caches, the configured `astra` profile, and working Codex lifecycle hooks are required for repair launches.

```sh
uv run --script loops/dependabot/tests/test_dependabot.py
uv run --script loops/dependabot/dependabot.py plan > /tmp/rimz-dependabot-plan.json
jq . /tmp/rimz-dependabot-plan.json
```

Once this config is present in the primary project and the worker prerequisites are ready, review and grant project trust with `rimz trust grant`, then run `rimz loop enable dependabot-repair`. The scheduled `check` command is part of the trust hash, so repeat both steps after a merge that changes it. A manual `rimz loop fire dependabot-repair` runs even when disabled, so use it only when ready to launch a repair. Project tasks require both trust and machine-local enablement. RimZ resolves them once for the project root, not once per worktree; the repository-common lock and live-worker check also protect manual runs from other worktrees. Remove any old machine-local definition during migration so it cannot act as a fallback while project trust is stale.

`plan` is read-only and prints the selected action and its evidence. `run` takes a repository-common advisory lock, re-queries the forge, and launches one supervised Astra worker when the plan calls for repair. Manual and scheduled invocations use this same entry point. Do not launch a repair worker directly alongside it; direct agent launches do not acquire the coordinator lock. Query or launch failures return nonzero; no work and an already running coordinator return success without a new agent.

The project task checks every 8 hours with a two-hour command timeout. Each forge or inventory query has a two-minute timeout, and the supervised worker has a shorter 60-minute wait cap. This is a check-only loop whose command launches the selected worker, not a static `--agent` task: loop logs record the command outcome, while the agent's own supervised run records its transcript and usage. Loop-level agent budgets and verify retries do not apply to the nested worker.

A room must remain open for the project, or the machine's optional `rimz loop timer install` must keep time. The project config does not install a timer. The first manual fire exercises the same locking and selection as scheduled fires.

## Duplicate prevention and recovery

The sorted source PR numbers identify a batch: `deps/repair-309-310` and `<!-- rimz-dependabot-repair:v1 sources=309,310 -->` name the same replacement. A second body marker, `<!-- rimz-dependabot-heads:v1 sources=309@FULL_SHA,310@FULL_SHA -->`, records the reviewed source revisions. The coordinator reads PR history as well as open PRs, so a rejected replacement does not become a new batch on the next tick. Existing open replacement work takes priority over new updates. Pending or successful replacement CI spends no agent turn while source revisions remain unchanged; failed CI or changed source revisions resume that same branch and PR, in a fresh checkout or the one a previous attempt kept. Sources not yet covered wait for a later batch.

Git branches also preserve identity before PR creation: a fresh batch's branch is published on origin before its first attempt, so an interrupted first attempt is recoverable by name. An interrupted branch can resume rather than create a differently named replacement. Ambiguous or malformed repair identities stop the coordinator for inspection. The lock serializes coordinators in worktrees sharing the same Git common directory; it is not a distributed lock across independent clones. Enable the task on only one machine for this repository. Before launching, the coordinator also checks all live agent cards: a surviving pane on any repair branch defers the next worker even if its earlier supervisor died. The prompt checks for an existing PR again before creation, including recovery from an uncertain API response.

Only failures on a PR's current head qualify as repair evidence. Missing or pending checks do not count as success or as a dependency failure. CI is re-read at every fire; an agent's local green gate does not prove remote CI is green.

## Operate and stop

```sh
rimz loop show dependabot-repair
rimz loop logs dependabot-repair --failed
rimz loop disable dependabot-repair
rimz loop stop dependabot-repair
rimz loop remove dependabot-repair
```

Disabling prevents future fires; stopping requests cancellation of the running loop command. A nested worker may need to be stopped separately through the agents surface; inspect its card before removing worktrees. A worker that ends without a correctly identified replacement PR, or leaves completed CI failed, makes the command fail rather than reporting a successful repair. Pending or missing checks remain a wait, not a claim of green CI. Three consecutive failed fires disable the task. After addressing the reported failure, `rimz loop enable dependabot-repair` clears the strikes. Removing the task preserves its run history and any unmerged repair worktree.

## See also

- [Loops](../guide/loops.md) — scheduling, timekeeping, and command run history.
- [Worktrees](../guide/worktrees.md) — creation, base selection, and cleanup.
- [Rust conventions](./rust-conventions.md) — verification gates and contributor automation.
