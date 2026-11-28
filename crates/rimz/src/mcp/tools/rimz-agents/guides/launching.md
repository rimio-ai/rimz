# Launching an agent

## Picking a profile

Take the profile the user named. Otherwise `rimz agents profiles` lists every launchable profile with its model and purpose: pick the one whose description matches the task, or a bare model for a seat with no craft.

## Worktree conventions

Name a worktree after the task, like `fix-tokens` or `auth-retry`; its channel is `#<name>`. Passing `fresh` on a closed worktree is how a reviewer lands on work another agent left.

## The brief

State the goal, the files or entry points, the constraints, and how done is judged, including whether the agent commits or opens a pull request. Reference plans, reports, issues, PRs, and commits by path or URL, and say when such a document is only reference so the agent verifies it and decides the way forward itself.

## After the launch

Keep working or end your turn. `AGENT_REPORT` lands as a new prompt with a row per settled agent, each naming the file that holds its final message; follow-up turns report back the same way, whereas a turn the user types in the agent's pane opens no report. Use `join` only for the one answer your next step cannot start without.

A staying agent keeps its pane after reporting; a `stay: false` one closes once its report is out, and one given a rest (`stay: "10m"`) closes once it has rested that long after its first turn, unless the user types to it first. When its answer is all the user asked for, `stop` closes the pane and keeps the session on disk (`rimz agents show @<handle>` inspects it, `rimz agents resume @<handle>` brings it back). When the user will keep working with it, leave it running and tell the user the profile, handle, worktree if any, and what it was briefed to do.
