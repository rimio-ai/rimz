# Team memory and stages

> How a team's shared files work: the declared scratch files, the `blackboard.md` stage board, and `rimz teams flip`. The code is [`harness/scratch.rs`](../../../crates/rimz/src/harness/scratch.rs) (the scan and the board parser) and [`harness/team_stage.rs`](../../../crates/rimz/src/harness/team_stage.rs) (flips and re-wakes). Launching a team is [fleet.md](./fleet.md); the `team.stage` signal and the stage-owner message are [loops.md § Team signals](./loops.md#team-signals). Users read the [teams guide](../../guide/teams.md) and [cli/teams.md](../../reference/cli/teams.md).

A team cooperates through files in its checkout. The members write the content; RimZ finds the files, reads the board's current stage, and performs the one board edit it owns, the stage flip. Neither the board nor member status is lifecycle authority: stages are advisory progress for the agents and the reports.

## Scratch files

A team's `scratch-files` entries are gitignore patterns for the ephemeral records its members keep. Before panes start, the launch registers them in the checkout's `info/exclude`, which keeps them out of `git status` and lets a landed tree holding only those files be reclaimed ([worktrees.md § Team scratch patterns](./worktrees.md#team-scratch-patterns)).

`scratch::scan(root, patterns)` resolves the patterns against the checkout root for two consumers, the member launch reminder and `rimz teams show`, so both see the same files. It strips a leading `/` before rooting each glob, matches files only, and returns absolute paths sorted and deduplicated across patterns. Each file carries its line count (zero when unreadable) and its modification time when metadata allows. An invalid glob or a failed traversal sets `probe_failed` instead of failing the scan.

The reminder presents relative paths and line counts as a launch-time snapshot. `teams show` scans again on every call and reports line counts and modification ages; its human output shows paths relative to the worktree, and its JSON keeps them absolute.

## The board and its stage

`scratch::board_stage` reads the first line starting `Stage:` in `<worktree>/blackboard.md`, whether or not `scratch-files` declares the board. Only a terminal ` (@owner)` suffix splits off as the owner, stored without `@`; other parenthesized text stays in the stage name, so `Stage: Implement (delta) (@coder)` is stage `Implement (delta)` owned by `coder`. A missing or unreadable board has no stage.

`teams show` combines the team's declared `stages` (its `pipeline` line, with the implicit `Done` last) with the board's current stage. The stage's age is the `at` of the newest `team.stage` event from `stage_flips` whose instance is the cohort and whose destination is the board's stage; a hand-edited board or a flip rotated out of the active event log leaves the age unknown, and the stage shows without one. Each armed signal subscription's fire state is the newest loop run-log record for its task name. Its PR and CI line comes from the published sidebar projection (the snapshot's `worktree_groups`), with no forge call of its own, so an absent PR line is not proof that no PR exists.

## Flipping a stage

`team_stage::flip` is the only RimZ code that writes the board. `rimz teams flip <stage> "<note>"` calls it, from a member (`Flipper::Member`) or the user (`Flipper::User`).

1. **Resolve the owner.** The team must have at least one role that owns a stage (`NoOwners`), the destination must be a declared stage or `Done` (`UnknownStage`), and a stage other than `Done` must have an owning role (`UnownedStage`).
2. **Lock and read.** The flip takes the per-worktree board lock (`RuntimePaths::board_lock`) and reads the board; a missing board reads as empty, and the current stage is the `from` of the flip.
3. **Check a hand-off.** A hand-off is a member leaving a stage its role owns for one it does not own, `Done` included. Before any write, a hand-off requires `git status --porcelain=v1 --untracked-files=all` over the worktree, excluding the root `blackboard.md`, to be empty, and refuses with the dirty paths otherwise. Other memory files count as dirty; only the board is exempt, and ignored files (such as registered scratch patterns) never appear. A worktree outside Git has nothing to commit and passes.
4. **Rewrite the board.** The first `Stage:` line becomes `Stage: <to> (@<owner>)` (no owner for `Done`); without one, the line goes after a leading `# ` heading or at the top. The note is appended as a ledger line, `- <local time> @<by>: <from> -> <to> — <note>` or `opened <to>` for a board with no stage, at the end of the `## Progress` section (an existing `## Progress log` section is accepted), which is created when absent. The board is published atomically.
5. **Open the stage.** The flip appends the `team.stage` signal, fires its subscriptions, and delivers a `STAGE` message to the owner at the `done` boundary unless the stage is `Done` or the flipper owns it ([loops.md § Team signals](./loops.md#team-signals)). A failure after the board write returns `PartialFlip`, naming what completed and telling the user to re-run the flip.
6. **Compact the flipper.** On a hand-off to a stage another role owns, when the role's `flip_compact` threshold (falling back to `[harness] flip_compact`) is reached, the flipper's pane gets its compaction command. This is best-effort enrichment recorded in the assist log; it never fails the flip. A flip to `Done` never compacts, because no member works after it.

## Registration re-wake

`team_stage::rewake` runs when a root member registers, which covers resume, restart, and rebirth. If the board's current stage is owned by that member's role and is not `Done`, it opens the same stage again with `by = "rimz"` and `from == to`, so the owner is told to continue from the board's last Progress line. A re-wake reads the board under the lock but never edits it, never checks the worktree, and never compacts. `stage_flips`, the history `rimz transcript` renders, skips re-wakes.
