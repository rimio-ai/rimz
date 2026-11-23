# Launching an agent

## Picking a profile

Take the profile the user named. Otherwise `rimz agents profiles` lists every launchable profile with its model and purpose: pick the one whose description matches the task. When no specialist fits, name a bare model from that list for a seat with no craft.

## Where the agent works

A change to the repo runs in its own `worktree` unless the user says otherwise. A review, a plan, or a question runs where the work it reads lives, usually this checkout. Name a worktree in kebab-case with at most two words from the task, like `fix-tokens` or `auth-retry`; its channel is `#<name>`. A new worktree branches from `agents.worktree.base` when set, else from the `HEAD` of your repository's main checkout, so whatever the agent must see has to be on that base first. A `team.role` branches as its team does. Naming a closed worktree is refused; `fresh` starts a new session there on purpose, which is how a reviewer lands on work another agent left.

`from_pr` takes the place of `worktree` when the work is bound to an open pull request. `cwd` takes it for another existing directory. When the target channel already holds an agent with the profile's name, you included, `name` gives the new one a distinct handle.

## The brief

The `prompt` is the agent's first turn: the goal, the files or entry points, the constraints, and how done is judged, including whether it commits or opens a pull request. Reference plans, reports, issues, PRs, and commits by path or URL, and say when such a document is only reference, so the agent verifies it and decides the way forward itself.

## After the launch

Keep working or end your turn. Once none of the agents you launched is mid-turn, one `AGENT_REPORT` lands as a new prompt, a row per agent that settled, each naming the file that holds the agent's final message. Read it and act on it. `join` is for the one answer your next step cannot start without. When the answer's `report` says no report comes, `join` is the only way to the agent's answers, follow-ups included.

A follow-up goes as a message to `@<handle>` when the agent shares this checkout's channel, and `@<handle>#<worktree>` when it runs in a worktree. The turn it opens reports back the same way. A turn the user types in the agent's pane opens no report.

The agent keeps its pane after reporting. When its answer is all the user asked for, `stop` closes the pane and keeps the session on disk, and `rimz agents resume @<handle>` brings it back. When the user will keep working with it, leave it running and tell the user the profile, the handle, the worktree if any, and what it was briefed to do.
