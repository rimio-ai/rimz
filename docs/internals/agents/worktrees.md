# Worktrees And Agent Layouts

> See [DESIGN.md](../../../DESIGN.md) for the commitments this doc operationalizes.

Rimz launches agent fleets by separating three choices: **agents** choose which tools run, **layout** chooses the shape on screen, and **worktree** chooses where they run.

## Rimz-owned worktrees

`rimz worktree new` creates a Git worktree under the per-machine `[worktree] dir` template, defaulting to a sibling `../{repo}-worktrees/<name>`, and creates a branch named `<name>` from the configured base (`head`, `fresh`, or an explicit ref). The marker stores the base branch name and the resolved base commit snapshot, so cleanup measures committed work against the live base branch and keeps the snapshot as the detached or unresolved fallback. Omitted names come from a two-word generated name; explicit names use letters, numbers, `_`, and `-`.

The checkout stays clean of Rimz metadata. Ownership lives in `rimz-worktree.json` inside the worktree's Git admin directory (`git rev-parse --git-dir` for that worktree). Cleanup, `remove`, and `gc` only act when that marker is present.

The marker records the name, branch, base branch name, base commit, repo root, worktree path, and marker version. A missing marker reads as user-owned, even if the path matches the configured directory template.

### Seeded files

A new worktree starts ready to run: the project's `.worktreeinclude` lists the untracked files an agent needs — `.env`, local config, caches — as glob patterns, one per line, and Rimz copies each pattern's matches from the checkout into the worktree right after `git worktree add`, preserving the path relative to the repo root. Lines use conventional shell-glob semantics (`*` within a path component, `**` across directories); blank lines and `#` comments are skipped. Matched directories copy recursively.

Seeding stays inside the project root: absolute patterns and patterns reaching out with `..` are skipped, and every file is confined by its canonical path, so a symlink a glob pattern descends into cannot pull host files into the agent-readable worktree. Seeding carries no command execution, so `.worktreeinclude` stays outside the trust hash.

Seeding is best-effort enrichment layered over creation: a missing `.worktreeinclude` is a silent no-op, and a pattern that matches nothing or a file that fails to copy warns on the launch path and is skipped — the worktree and its agent still launch. A reused worktree is never re-seeded. `rimz worktree new` reports the count of seeded files.

## Cleanup

The hidden `rimz agents exec` wrapper runs the agent command in the pane and inherits the pane's TTY. It launches the agent through the user's default shell startup path when that shell and `/usr/bin/env` are available, re-applying Rimz launch env after shell rc/profile files, and falls back to direct exec for unsupported or missing shells. When the agent exits with `--worktree-path`, it spawns the on-disk `rimz worktree cleanup <path>` helper, resolving past the kernel's trailing ` (deleted)` annotation after an atomic install, so long-lived panes pick up the freshest cleanup logic; if the helper cannot be resolved or spawned, the wrapper falls back to the same cleanup implementation in process.

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

## Agent layout IR

`rimz agents <spec>` resolves either a named `[agents.layouts]` entry or an inline DSL. Commas split columns, plus signs stack rows within a column, and each cell is an alias or built-in cell: built-in `term`, an agent kind, an adapter-supported virtual `<kind>-<mode>` variant such as `claude-auto` or `codex-yolo`, or a `[agents.aliases]` entry.

```text
claude,codex+term
vim,htop+zsh
claude-auto,codex-yolo
```

The first example creates two columns: Claude on the left, Codex stacked above a shell on the right. The second creates raw command panes from user aliases. The third opens agent cells with adapter-owned permission posture args. The built-in `peer` layout is `claude,codex`; bare `rimz agents` lists cards, while the hidden layout default remains one `term` cell for internal callers.

The CLI converts cells to backend-neutral `LayoutPanes`: agent cells run `rimz agents exec <kind>` with optional `--prompt`, optional `--worktree-path`, and `-- <args>` from their alias; command cells run their raw argv, with empty argv reserved for the user's shell. Backends never resolve agent kinds or worktrees.

## Backend shape

tmux opens a window with `new-window -d -P`, lets the session's `after-new-window` hook dock the sidebar once, and adds the remaining layout cells with `split-window`. Columns use horizontal splits; rows use vertical splits anchored inside their column.

Zellij renders a temporary KDL layout for `new-tab --layout`: the global sidebar pane on the left, one pane per column to the right, nested horizontal splits for stacked rows, and the compact bar restored at the bottom.

Both backends receive the same `TabOptions`: session, title, cwd, focus flag, sidebar options, and the pre-built pane argv. `--no-focus` keeps the current view active where the backend can do so.
