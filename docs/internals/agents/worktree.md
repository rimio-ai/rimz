# Rimz-owned worktrees

> See [DESIGN.md](../../../DESIGN.md) for the commitments this doc operationalizes. The agent harness that launches agents into worktrees and triggers their cleanup is [harness.md](./harness.md); this doc owns the worktree feature itself — creation, the ownership marker, file seeding, and the cleanup that proves work landed before reclaiming.

A worktree is one checkout of the repository on its own branch, and Rimz manages a full lifecycle for the ones it creates: spin one up for a line of work, seed it with the untracked files an agent needs, and reclaim it once its work has landed. In the agent room a worktree is a **channel** — the place a few members cooperate, the unit the sidebar groups by, and the segment an [address](./harness.md#the-address) narrows to with `#<channel>`. The feature stands on its own: `rimz worktree new`, `list`, `remove`, and `gc` work whether or not an agent ever launches into the tree.

Rimz acts only on worktrees it owns. A marker file inside the worktree's Git admin directory records that ownership, and every managed operation — cleanup, `remove`, `gc` — checks for it first, so an arbitrary user checkout is never touched.

## Create

`rimz worktree new` creates a Git worktree under the per-machine `[agents.worktree] dir` template, defaulting to a sibling `../{repo}-worktrees/<name>`, and creates a branch named `<name>` from the configured base (`head`, `fresh`, or an explicit ref) ([configuration.md](../../reference/configuration.md#worktrees)). Omitted names come from a two-word generated name; explicit names use letters, numbers, `_`, and `-`. `--base` overrides the base ref, and `--branch <NAME>` names the branch independently of the worktree name.

The marker stores the base branch name and the resolved base commit snapshot, so cleanup prefers a still-live base branch for committed-work checks and keeps the snapshot as the detached or unresolved fallback.

## Ownership marker

The checkout stays clean of Rimz metadata. Ownership lives in `rimz-worktree.json` inside the worktree's Git admin directory (`git rev-parse --git-dir` for that worktree), recording the name, branch, base branch name, base commit, repo root, worktree path, and marker version. Cleanup, `remove`, and `gc` act only when that marker is present; a missing marker reads as user-owned, even if the path matches the configured directory template.

## Seeded files and linked directories

A new worktree starts ready to run: the project's `.worktreeinclude` lists the untracked files an agent needs — `.env`, local config, caches — as glob patterns, one per line, and Rimz copies each pattern's matches from the checkout into the worktree right after `git worktree add`, preserving the path relative to the repo root. Lines use conventional shell-glob semantics (`*` within a path component, `**` across directories); blank lines and `#` comments are skipped. Matched directories copy recursively.

Seeding stays inside the project root: absolute patterns and patterns reaching out with `..` are skipped, and every file is confined by its canonical path, so a symlink a glob pattern descends into cannot pull host files into the agent-readable worktree. Seeding carries no command execution, so `.worktreeinclude` stays outside the trust hash ([trust.md](../sidebar/trust.md)).

Seeding is best-effort enrichment layered over creation: a missing `.worktreeinclude` is a silent no-op, and a pattern that matches nothing or a file that fails to copy warns on the launch path and is skipped — the worktree and its agent still launch. A reused worktree is never re-seeded. `rimz worktree new` reports the count of seeded files.

`.worktreelink` lists directories to symlink into each new worktree, one relative path per line with the same blank-line and `#` comment rules. Use it for heavy machine-local directories such as `node_modules`, `target`, or `.venv` that agents should share rather than copy. Rimz confines each source to the project root, requires it to be a directory, never clobbers an existing destination, creates an absolute symlink in the worktree, and reports the count of linked directories.

Linked directories are registered in the worktree's effective `git info/exclude` as anchored `/<path>` patterns, written with temp-file plus rename and deduped across repeated creates. For linked Git worktrees this exclude file is commonly the repo's shared common `.git/info/exclude`, so the pattern also excludes the same build directory in the main checkout and sibling worktrees; build and dependency directories are the intended use.

## Cleanup

A worktree is reclaimed once its work has landed. Cleanup runs through the on-disk `rimz worktree cleanup <path>` helper: the agent wrapper spawns it when an agent launched with `--worktree-path` exits ([harness.md → Cleanup](./harness.md#cleanup)), resolving past the kernel's trailing ` (deleted)` annotation after an atomic install so long-lived panes pick up the freshest cleanup logic; if the helper cannot be resolved or spawned, the wrapper falls back to the same cleanup implementation in process. `rimz worktree remove <name>` runs the same decision on demand.

Cleanup re-reads the marker, checks `git status --porcelain`, asks the mux for live pane cwd values, and computes the same content-landed verdict the sidebar and `rimz gc` use. The comparison ref ladder tries the marker's base branch, then `main`, `master`, `origin/HEAD`, then the creation snapshot. A base branch counts only while it is still a live destination: once it has diverged from the trunk and its own commits have landed there — a rebased or squashed fork point left dangling — the ladder falls through to the trunk, so work built on an already-merged branch is measured against where it truly landed. The verdict is conservative: a missing ref or git error is `unknown` and keeps the worktree; a pending patch keeps it; only a clean working tree with a landed verdict is removable.

The content-landed verdict first accepts a branch with no commits beyond the comparison ref, then accepts identical base/head trees. Otherwise it asks Git for branch-side non-merge commits whose patch is not present on the comparison side (`git log --right-only --cherry-pick --no-merges`) and treats any result as pending. If only merge commits remain, their tree IDs must already appear in the comparison ref's recent history, bounded to 500 commits; a missing tree is pending. That covers rebased, cherry-picked, squash/split-landed, and merge-back shapes without treating sidebar wakeups or ancestry counts as truth.

The cleanup decision is pure:

| Marker | Status | Other live user pane inside path | Decision |
| --- | --- | --- | --- |
| absent | any | any | skip |
| present | clean and content-landed | no | remove worktree and delete the branch after proving its work landed |
| present | dirty, pending, or unknown | no | prompt `keep / remove / shell` on a TTY; keep on EOF or non-TTY |
| present | any | yes | skip |

The automatic path deletes a branch only after proving its work landed on the live base: it tries `git branch -d`, escalates to `git branch -D` only after the same landed-work check succeeds, and keeps the branch otherwise. The interactive dirty `remove` choice and `rimz worktree remove --force` use Git's force removal path because the human explicitly chose destruction. Rimz sidebar panes are chrome: they inherit the tab cwd for launch, and worktree liveness reads user panes only.

## `rimz gc`

`rimz gc` sweeps clean, marked, content-landed worktrees in the current repo when no live user pane cwd sits inside them, then runs `git worktree prune`. `Fresh`-based worktrees compare against `origin/...`, so unfetched merges keep them until a fetch updates the remote-tracking base.
