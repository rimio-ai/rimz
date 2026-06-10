# Worktrees And Agent Tabs

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

Rimz launches agent fleets by separating three choices: **agents** choose which tools run, **tab layout** chooses the shape on screen, and **worktree** chooses where they run.

## Rimz-owned worktrees

`rimz worktree new` creates a Git worktree under the per-machine `[worktree] dir` template, defaulting to a sibling `../{repo}-worktrees/<name>`, and creates a branch named `<name>` from the configured base (`head`, `fresh`, or an explicit ref). The marker stores the base branch name and the resolved base commit snapshot, so cleanup measures committed work against the live base branch and keeps the snapshot as the detached or unresolved fallback. Omitted names come from a two-word generated name; explicit names use letters, numbers, `_`, and `-`.

The checkout stays clean of Rimz metadata. Ownership lives in `rimz-worktree.json` inside the worktree's Git admin directory (`git rev-parse --git-dir` for that worktree). Cleanup, `remove`, and `gc` only act when that marker is present.

The marker records the name, branch, base branch name, base commit, repo root, worktree path, and marker version. A missing marker reads as user-owned, even if the path matches the configured directory template.

## Cleanup

The hidden `rimz agents exec` wrapper runs the agent command in the pane and inherits the pane's TTY. When the agent exits with `--worktree-path`, it spawns the on-disk `rimz worktree cleanup <path>` helper, resolving past the kernel's trailing ` (deleted)` annotation after an atomic install, so long-lived panes pick up the freshest cleanup logic; if the helper cannot be resolved or spawned, the wrapper falls back to the same cleanup implementation in process.

Cleanup re-reads the marker, checks `git status --porcelain`, checks commits not yet landed on the live base with `git rev-list --count <base>..HEAD`, treats identical base/head trees as landed only when the branch tree differs from its fork point, applies a bounded patch-equivalence check for rebased, cherry-picked, or squash-merged work, and asks the mux for live pane cwd values. If the live base branch is unavailable, cleanup tries `main`, `master`, `origin/HEAD`, then the creation snapshot; if the unmerged count cannot be computed, cleanup treats the worktree as not clean and keeps it.

The cleanup decision is pure:

| Marker | Status | Other live user pane inside path | Decision |
| --- | --- | --- | --- |
| absent | any | any | skip |
| present | clean with no unmerged commits | no | remove worktree and delete the branch after proving its work landed |
| present | dirty or carrying unmerged commits | no | prompt `keep / remove / shell` on a TTY; keep on EOF or non-TTY |
| present | any | yes | skip |

The automatic path deletes a branch only after proving its work landed on the live base: it tries `git branch -d`, escalates to `git branch -D` only after the same landed-work check succeeds, and keeps the branch otherwise. The interactive dirty `remove` choice and `rimz worktree remove --force` use Git's force removal path because the human explicitly chose destruction.

Rimz sidebar panes are chrome: they inherit the tab cwd for launch, and worktree liveness reads user panes only.

`rimz gc` also sweeps clean, marked worktrees whose work has landed on their base in the current repo when no live user pane cwd sits inside them, then runs `git worktree prune`. `Fresh`-based worktrees compare against `origin/...`, so unfetched merges keep them until a fetch updates the remote-tracking base.

## Tab layout IR

`rimz tab --layout` resolves either a named `[agents.layouts]` entry or an inline DSL. Commas split columns, plus signs stack rows within a column, and each cell is a registered agent kind or `term`. Named layouts may be a table with `shape` plus per-agent `flags`; inline specs are shape-only.

```text
claude,codex+term
```

That example creates two columns: Claude on the left, Codex stacked above a shell on the right. The built-in `peer` layout is `claude,codex`; no layout means one `term` cell.

The CLI converts cells to backend-neutral `LayoutPanes`: agent cells run `rimz agents exec <kind>` with optional `--prompt`, optional `--worktree-path`, and `-- <flags>` when the named layout supplies launch flags for that kind; `term` cells run the user's shell. Backends never resolve agent kinds or worktrees.

## Backend shape

tmux opens a window with `new-window -d -P`, lets the session's `after-new-window` hook dock the sidebar once, and adds the remaining layout cells with `split-window`. Columns use horizontal splits; rows use vertical splits anchored inside their column.

Zellij renders a temporary KDL layout for `new-tab --layout`: the global sidebar pane on the left, one pane per column to the right, nested horizontal splits for stacked rows, and the compact bar restored at the bottom.

Both backends receive the same `TabOptions`: session, title, cwd, focus flag, sidebar options, and the pre-built pane argv. `--no-focus` keeps the current view active where the backend can do so.
