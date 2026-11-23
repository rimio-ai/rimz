# Launching a team

## Picking a team

Take the team the user names. Otherwise `rimz teams list` shows the configured teams. In the crew pack, `forge` (coder, scout, reviewer) is the default for a real implementation whose direction is clear; step down to `spot` (coder, reviewer) for a small fix the brief pins down, and up to `recon` (planner, scout, coder, reviewer) when the change still needs design. `mill` is a behavior-preserving pass over working code, and `trim` cuts an accreted test suite. Unsure between two, take `forge`, whose leader asks the user about an open choice.

## The worktree

Every team runs in its own worktree. Name it with `worktree`, in kebab-case with at most two words from the task, like `fix-tokens` or `auth-retry`. A new worktree branches from `agents.worktree.base` when set, else from origin's default branch, fetched at launch, so whatever the team must see has to be there first. A repository with no origin branches from `HEAD`, and a failed fetch branches from the branch as last fetched, or from the local one, with a warning. Work bound to an open pull request takes `from_pr` in its place, which creates or reuses a worktree from that PR.

## The brief

The `prompt` goes to the leader, which holds intent and asks the user about any choice the brief leaves open. State the goal, the files or entry points, the constraints, and how done is judged. Point at plans, reports, issues, PRs, and commits by path or URL instead of restating them, and say when such a document is reference rather than orders.

## After the launch

Tell the user the team, the worktree, and what it was briefed to do, and end your turn: a run usually takes hours. Each time its board reaches Done, one `TEAM_REPORT` lands as a new prompt, naming the file with the leader's final message and the board that holds the reasoning. When the answer's `report` says no report comes, `join` on the id blocks until the board reads Done and answers its Result, or answers `running` at its `timeout` (4 minutes by default, 9 at most), and you call it again.

Reach a live team with a message to `@<role>#<worktree>`, to the leader unless you mean one seat, since the leader routes to the others. `rimz teams show <team>#<worktree>` shows the stage and each seat's status.

The team keeps its panes after Done. Once its answer is all the user asked for, `stop` closes every seat and leaves the checkout held, and `rimz teams resume <team>#<worktree>` brings it back. If the user will keep working with the team, leave it running.
