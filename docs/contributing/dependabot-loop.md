# Dependabot repair loop

This repository's dependency loop queries GitHub Dependabot PRs and launches one `spot` team for a batch of failed updates. A fresh batch waits until the newest open Dependabot PR is at least six hours old, counted from creation only. The team opens one replacement PR; the machine-local sweep loop merges it once the board reaches `Done` with green CI, without a human merge review. The replacement body includes `Closes #N` references so GitHub closes fully covered source PRs on merge.

## Attempt checkouts

Each repository loop owns a directory under [loops/](../../loops/README.md). This task keeps its [Python coordinator](../../loops/dependabot/dependabot.py), [team prompt](../../loops/dependabot/prompt.md), and tests together in `loops/dependabot/`. The schedule is declared directly in [.rimz/config.toml](../../.rimz/config.toml), without an installer or machine-local definition for this task. The scheduled command is `uv run --no-project --script loops/dependabot/dependabot.py run`, started at the project root.

The coordinator never edits files, so it runs read-only at the project root, where project check commands start; `scripts/trunk-install` keeps that primary checkout on `origin/main` when a machine loop runs it. Only a selected repair attempt gets a worktree, and it is fresh for that attempt.

`run` takes the repository-common lock and queries the live agent cards and PRs. A repair lane is finished only when its checkout's `.rimz/team/blackboard.md` has `Stage: Done` and no open PR heads its branch. The first `Stage:` line decides; its value is trimmed and a valid terminal ` (@owner)` is dropped. A mid-stage board, an open PR (even at `Done`), or a missing or unreadable checkout/board keeps the lane occupied. A board with no `Stage:` line is also occupied. If any repair pane belongs to an unfinished lane, the fire returns idle without stopping anyone or reclaiming trees.

When all repair panes belong to finished lanes, the coordinator stops each occupant with `rimz agents stop <pane id>`, including a sweeper resting after merge or seats at a no-PR `Done`. Each successful stop prints `stopped`, `handle`, `branch`, and the reason; a failed stop fails the fire. It then runs `git worktree prune` and `git fetch --prune origin`, reclaims repair checkouts, deletes empty batch branches, and re-plans, reusing the one PR snapshot throughout. The next batch can launch in that same fire.

Reclaim tries one unforced `rimz worktree remove <branch>` per repair checkout, printing an `attempt_checkout` line with `removed` or `kept` and the reason. A refusal containing `is in use by` returns idle immediately with reason `repair lane still closing`, the branch, and RimZ's refusal in `holder`; there is no plan, publication, or launch. The process may still be exiting after its pane closed, or a human shell may hold the tree: the next fire retries, without polling or a strike. Other refusals proceed to planning and the human stop below. Automation never forces removal: unpushed or uncommitted work survives for a human.

An empty batch branch has its tip already in `origin/<default_base>`, no worktree, and no open PR head. It is deleted on origin with `git push --force-with-lease=refs/heads/<branch>:<tip> origin :refs/heads/<branch>`, or locally with `git branch -D`; each deletion prints a `pruned_batch_branch` line. A team that stops without writing therefore leaves nothing behind for the next plan. `plan` never locks, prunes, reclaims, or calls RimZ.

When the plan calls for repair, the coordinator first checks the seats' callable skills: `rimz agents explain spot.coder --json` must admit `commit` and `fix-ci`, and `spot.reviewer` must admit `pr`. A missing skill refuses the fire before publishing a branch, naming the role, skills, and fix. No callable-skills list means native discovery and is admitted. Then checkout creation proceeds in this order:

1. A tree still on the planned branch after a refusal other than `is in use by` needs human inspection. Stop with its path and branch: push the work, or discard it with `rimz worktree remove <branch> --force`. Never relaunch into a previous cohort's tree.
2. When `origin/<branch>` does not exist, publish it at the default base: `git push origin origin/<default_base>:refs/heads/<branch>`, then `git fetch origin <branch>`.
3. A local `<branch>` without a worktree whose tip is already on `origin/<branch>` is deleted. One holding commits absent from origin stops the coordinator with `push or delete it`: inspect it with `git log origin/<branch>..<branch>`, then push or delete it by hand.
4. `rimz worktree new <branch> --base origin/<branch>` cuts the checkout; RimZ derives the `deps-repair-…` directory and channel from the name.

Creation prints `attempt_checkout: created`, `branch`, `path`, and any `stale_attempt_checkouts` that reclaim kept. The coordinator then runs `rimz teams launch spot <prompt> -w <branch> --fresh --bg`, leaving focus where it is, prints a `launched: spot#deps-repair-…` receipt, and exits. It neither waits on the team nor queries GitHub after launch. A nonzero launch fails the fire and leaves the tree for the next fire to reclaim. Acceptance comes from the next fire's plan, not the receipt.

Because the checkout's base is `origin/<branch>`, "landed" means every commit is pushed. Removal retires the previous cohort; recreation starts from the published branch and PR, not its transcripts or board. Between sweep stopping the seats and placing its own pane, a fire can reclaim a clean, pushed tree; the next fire recreates it only if the PR needs repair. Sweep's merged-PR path does not need that tree.

List kept trees with `rimz worktree list`. To abandon a branch with work but no PR, stop its team, remove its tree, then run `git push origin --delete deps/repair-N`: while such a branch covers a closed source, the coordinator refuses to plan.

## Inspect, enable, and run

`cargo xtask loop-tests` runs the test command below along with every repository loop's tests; `cargo xtask gate` includes it.

Run inspection commands from any checkout containing this implementation. Both Python scripts declare `requires-python = ">=3.14"`; `uv` selects a compatible interpreter, with no third-party Python dependencies. Help is uncolored and suggests argument typos. Machine prerequisites are the `spot` team (coder leads Implement and Reflect; reviewer owns Review and Submit; coder subscribes to `ci.failed`) and the enabled `sweep` loop (`each-worktree`, `takeover`, `when = ["pr=open && team.stage=Done && !ci=pending,failed"]`, `for = "3m"`). Both are machine-local, not installed by this repository. The seats must reach GitHub and writable cargo caches under their configured isolation policy; the launch does not widen it.

```sh
uv run --script loops/dependabot/tests/test_dependabot.py
dependabot_plan=$(mktemp "${TMPDIR:-/tmp}/rimz-dependabot-plan.XXXXXX.json")
uv run --script loops/dependabot/dependabot.py plan > "$dependabot_plan"
jq . "$dependabot_plan"
rimz agents explain spot.coder --json | jq .skills.callable
rimz agents explain spot.reviewer --json | jq .skills.callable
```

Once this config is in the primary project and the prerequisites are ready, review and grant trust with `rimz trust grant`, then enable `dependabot-repair` with `rimz loop enable dependabot-repair`. The task's `check`, `timeout`, and `every` are hashed: repeat `rimz trust grant` after merge, and enable again only if disabled or struck out. Project tasks resolve once for the project root, not per checkout; the common lock and live-pane check also protect manual runs from other worktrees. Remove any old machine-local definition during migration so it cannot act as a fallback while project trust is stale.

`plan` prints the action and evidence read-only. `--quiet-period HOURS` defaults to `6` on both `plan` and `run`; `0` disables it. Every open Dependabot PR counts, even a green PR, an already covered source, or one against another base. Closed PRs and update timestamps do not count. Selection is admitted at exactly the required age, with no batch-size cap. Resume, recovery, and reconciliation of an existing batch are never gated. A deferred plan is idle with no sources, naming the newest PR and the time a new batch may start.

For a manual bypass, first inspect `uv run --script loops/dependabot/dependabot.py plan --quiet-period 0`, then run `uv run --script loops/dependabot/dependabot.py run --quiet-period 0` from the primary checkout with a room open. `rimz loop fire dependabot-repair` uses the configured quiet period, even when disabled. Do not launch a repair team directly alongside the coordinator: that bypasses its lock.

The project task checks hourly with a fifteen-minute coordinator timeout; each forge or inventory query has a two-minute timeout. The fire ends after placement, while the team may wait days for a maintainer's answer. Later fires idle on unfinished lanes and stop resting occupants of finished lanes. Query, stop, preflight, or launch failures return nonzero; no work, quiet-period deferral, a held lock, occupied repair lanes, and a still-closing tree return success without launching. This is a check-only task: loop logs record the coordinator outcome, and the team owns its board, transcripts, and usage. Loop-level agent budgets and verify retries do not apply to the team.

A room must remain open for the project, or the machine's optional `rimz loop timer install` must keep time. The project config does not install a timer. The first manual fire exercises the same locking and selection as scheduled fires.

## Duplicate prevention and recovery

The sorted source numbers identify a batch: `deps/repair-309-310` and the standalone `<!-- rimz-dependabot-repair:v1 sources=309,310 -->` body line name the same replacement. `rimz-dependabot-repair:` occurs exactly once in its body. A second standalone marker, `<!-- rimz-dependabot-heads:v1 sources=309@FULL_SHA,310@FULL_SHA -->`, records reviewed source revisions. The leader records the plan and body contract in the board's Goal for Submit, which appends the markers and fully covered sources' `Closes #N` lines after Context / Implementation / Advisories.

The coordinator reads PR history as well as open PRs, so a rejected replacement is not recreated. One existing batch takes priority over new updates. Pending, missing, or successful replacement CI spends no team launch while source revisions are unchanged; failed CI or changed source revisions repair that same branch and PR in a recreated checkout. Published branches preserve identity before PR creation, so interrupted work recovers by name. Ambiguous or malformed identities stop for inspection. The lock serializes coordinators sharing a Git common directory, not independent clones: enable this task on only one machine.

Only failures on a PR's current head qualify as repair evidence. Missing or pending checks are neither success nor a dependency failure. The team verifies locally but does not wait on remote CI: the coder's `ci.failed` subscription brings failures back while the cohort lives, and sweep waits for green.

Cargo-vet evidence falling short is one leader `AskUserQuestion` during Implement, before raising the PR, gathering every unsupported crate with its evidence and the options `trust <publisher>`, `exempt this version`, `drop this update`, or `abandon this batch`. Empty answers are not decisions. The same question rule covers refused tools or credentials and CI fixes that would change behavior outside the batch. Push finished work before waiting and cite the answer as the maintainer's decision in the PR body. Reflect cannot carry unrelated work on this automatically merged PR; follow-ups go to `.rimz/team/reflect-notes.md`.

If the selected batch no longer holds, with no open replacement the leader records the stop in Result and flips `Done`. With an open replacement, do not flip `Done`: ask whether to close it and wait. A draft is not a stop guard, because sweep's `pr=open` condition includes drafts.

## Operate and stop

```sh
rimz loop show dependabot-repair
rimz loop logs dependabot-repair --failed
rimz teams show spot#deps-repair-309-310
rimz loop disable dependabot-repair
rimz loop stop dependabot-repair
rimz teams stop spot#deps-repair-309-310
rimz loop remove dependabot-repair
```

Use the actual batch numbers in the team address. Team members start asynchronously, so inspect after placement rather than treating an immediate empty roster as failure. Disabling prevents future fires; loop stop cancels only the coordinator, not an already launched team. Stop that cohort separately before removing its checkout. Once a lane is finished (its board at `Done` and no open PR on its branch), the next fire closes every pane on that tree, a human's agent included, so park nothing in a `deps-repair-*` tree after `Done`. To abandon an open replacement, resolve whether to close it before leaving a green board at `Done`; stopping seats does not disable sweep. Three consecutive failed fires disable the task. After fixing the reported cause, `rimz loop enable dependabot-repair` clears strikes. Removing the task preserves run history and repair worktrees.

## See also

- [Loops](../guide/loops.md) — scheduling, timekeeping, and command run history.
- [Worktrees](../guide/worktrees.md) — creation, base selection, and cleanup.
- [Rust conventions](./rust-conventions.md) — verification gates and contributor automation.
