# Worktrees And Agent Tabs

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

Rimz launches agent fleets by separating three choices: **agents** choose which tools run, **tab layout** chooses the shape on screen, and **worktree** chooses where they run.

## Rimz-owned worktrees

`rimz worktree new` creates a Git worktree under the per-machine `[worktree] dir` template, defaulting to a sibling `../{repo}-worktrees/<name>`, and creates a branch from the configured base (`head`, `fresh`, or an explicit ref). Omitted names come from a two-word generated name; explicit names use letters, numbers, `_`, and `-`.

The checkout stays clean of Rimz metadata. Ownership lives in `rimz-worktree.json` inside the worktree's Git admin directory (`git rev-parse --git-dir` for that worktree). Cleanup, `remove`, and `gc` only act when that marker is present.

The marker records the name, branch, base ref, repo root, worktree path, and marker version. A missing marker reads as user-owned, even if the path matches the configured directory template.

## Cleanup

The hidden `rimz agents exec` wrapper runs the agent command in the pane and inherits the pane's TTY. When the agent exits with `--worktree-path`, it re-reads the marker, checks `git status --porcelain`, checks commits ahead of the marker base with `git rev-list --count <base>..HEAD`, and asks the mux for live pane cwd values.

The cleanup decision is pure:

| Marker | Status | Other live pane inside path | Decision |
| --- | --- | --- | --- |
| absent | any | any | skip |
| present | clean and not ahead | no | remove worktree and delete branch with `git branch -d` |
| present | dirty or ahead | no | prompt `keep / remove / shell` on a TTY; keep on EOF or non-TTY |
| present | any | yes | skip |

The automatic path never force-deletes a branch. The interactive dirty `remove` choice and `rimz worktree remove --force` use Git's force removal path because the human explicitly chose destruction.

`rimz gc` also sweeps clean, marked worktrees in the current repo when no live pane cwd sits inside them, then runs `git worktree prune`.

## Tab layout IR

`rimz tab --layout` resolves either a named `[agents.layouts]` entry or an inline DSL. Commas split columns, plus signs stack rows within a column, and each cell is a registered agent kind or `term`.

```text
claude,codex+term
```

That example creates two columns: Claude on the left, Codex stacked above a shell on the right. The built-in `dual` layout is `claude,codex`; no layout means one `term` cell.

The CLI converts cells to backend-neutral `LayoutPanes`: agent cells run `rimz agents exec <kind>` with optional `--prompt` and optional `--worktree-path`; `term` cells run the user's shell. Backends never resolve agent kinds or worktrees.

## Backend shape

tmux opens a window with `new-window -d -P`, lets the session's `after-new-window` hook dock the sidebar once, and adds the remaining layout cells with `split-window`. Columns use horizontal splits; rows use vertical splits anchored inside their column.

Zellij renders a temporary KDL layout for `new-tab --layout`: the global sidebar pane on the left, one pane per column to the right, nested horizontal splits for stacked rows, and the compact bar restored at the bottom.

Both backends receive the same `TabOptions`: session, title, cwd, focus flag, sidebar options, and the pre-built pane argv. `--no-focus` keeps the current view active where the backend can do so.
