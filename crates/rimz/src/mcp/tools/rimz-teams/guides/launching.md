# Launching a team

## Picking a team

Take the team the user names. Otherwise `rimz teams list` shows the configured teams. In the crew pack, `forge` (coder, scout, reviewer) is the default for a real implementation whose direction is clear; step down to `spot` (coder, reviewer) for a small fix the brief pins down, and up to `recon` (planner, scout, coder, reviewer) when the change still needs design. `mill` is a behavior-preserving pass over working code, and `trim` cuts an accreted test suite. Unsure between two, take `forge`, whose leader asks the user about an open choice.

## Worktree details

Name a worktree after the task, like `fix-tokens` or `auth-retry`. A failed fetch branches from the branch as last fetched, or from the local one, with a warning.

## The brief

The leader holds intent and asks the user about any choice the brief leaves open. State the goal, the files or entry points, the constraints, and how done is judged. Point at plans, reports, issues, PRs, and commits by path or URL instead of restating them, and say when such a document is reference rather than orders.

## After the launch

Tell the user the team, the worktree, and what it was briefed to do. `TEAM_REPORT` lands as a new prompt naming the file with the leader's final message and the board that holds the reasoning. `rimz teams show <team>#<worktree>` shows the stage and each seat's status, and the leader routes follow-up messages to other seats.

The team keeps its panes after Done. Once its answer is all the user asked for, `stop` closes every seat and leaves the checkout held, and `rimz teams resume <team>#<worktree>` brings it back. If the user will keep working with the team, leave it running.
