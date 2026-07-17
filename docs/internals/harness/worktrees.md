# RimZ-owned worktrees

> See [DESIGN.md](../../../DESIGN.md) for the commitments this doc operationalizes. [message.md § Channels](./messaging.md#channels) owns the room channel model, and [harness.md](./harness.md) launches agents into channels and triggers worktree cleanup; this doc owns the worktree itself — creation, the ownership marker, file seeding, and the cleanup that proves work landed before reclaiming.

A worktree is one checkout of the repository on its own branch, with its name and path backing a RimZ channel. RimZ runs the full lifecycle for the ones it creates: spin one up for a line of work, seed it with the untracked files an agent needs, and reclaim it once its work has landed. The feature stands on its own: `rimz worktree new`, `list`, `remove`, and `gc` work whether or not an agent ever launches into the tree.

RimZ touches only worktrees it owns. A marker file inside each one records that ownership, and every managed operation — cleanup, `remove`, `gc` — checks for it first, so a hand-made checkout is never disturbed.

## Create

`rimz worktree new [NAME]` adds a Git worktree under the `[agents.worktree] dir` template (default `../{repo}-worktrees/<name>`) on a branch named `<name>`, cut from the configured base ([configuration.md](../../guide/configuration.md#worktrees)). An omitted name is generated as two words; explicit names allow letters, numbers, `_`, and `-`; a `/` names the branch directly and maps to `-` for the worktree name, directory, and channel. `--base` overrides the base ref, and `--branch` names the branch independently of the worktree.

`--from-pr <number|url>` builds the same marked worktree from a pull-request head and defaults the worktree name to `pr-<N>`. One `git ls-remote origin` resolves the host-specific PR ref and same-repository head branch; ambiguous tips and fork heads use the matching authenticated forge CLI. Same-repository branches track `origin`, fork branches carry the fork URL as their pull/push remote, and existing local branches are adopted or fast-forwarded only when their ancestry is safe. Conflicts and unresolved fork heads fail with a review-only `--branch` escape hatch. `rimz agents <SPEC> --from-pr <PR>` implies a worktree launch, and a sibling `--worktree <NAME>` names the PR worktree.

The marker records the two inputs cleanup needs: the base **branch name** and the resolved base **commit**. Cleanup prefers a still-live base branch for the landed-work check and keeps the commit as the fallback. A PR worktree records the repository trunk as its base, so the ordinary cleanup proof reclaims it once the pull request's content lands on trunk — merge, squash, or rebase shapes alike.

## Ownership marker

The checkout stays free of RimZ metadata. Ownership lives in `rimz-worktree.json` inside the worktree's Git admin directory, recording the name, branch, base branch and commit, repo root, path, and marker version. Cleanup, `remove`, and `gc` act only when that marker is present; its absence reads as user-owned, even when the path matches the configured directory template.

## Seeded files and linked directories

A new worktree starts ready to run. Two optional, committed project files describe what to carry in beyond the tracked tree:

- **`.worktreeinclude`** copies untracked files an agent needs — `.env`, local config, caches. It lists glob patterns, one per line; RimZ copies each match from the checkout right after `git worktree add`, preserving the repo-relative path. Patterns use conventional shell-glob semantics (`*` within a path component, `**` across directories); matched directories copy recursively.
- **`.worktreelink`** symlinks directories agents should share rather than copy — heavy machine-local data whose contents are intentionally branch-independent, such as downloaded model or fixture caches. It lists relative paths, one per line. Cargo `target/` directories stay branch-local because divergent worktrees can overwrite their fingerprints and executables; contributors share compiler outputs through the [sccache setup](../../../CONTRIBUTING.md#fast-local-builds).

Both files skip blank lines and `#` comments, and both confine every source to the project root: absolute patterns and patterns reaching out with `..` are skipped, and each resolved path is checked against its canonical form, so a symlink a glob descends into cannot pull host files into the agent-readable tree. Linking additionally requires a real directory and never clobbers an existing destination, writing an absolute symlink. Each linked directory is registered in the worktree's effective `git info/exclude` as an anchored `/<path>` pattern (temp-file-plus-rename, deduped across creates); because that exclude is commonly the repo's shared one, the same branch-independent directory is also excluded in the main checkout and sibling worktrees.

Seeding is best-effort enrichment layered over creation: a missing file is a silent no-op, and a pattern that matches nothing or a copy that fails warns on the launch path and is skipped — the worktree and its agent still launch. A reused worktree is never re-seeded. Neither file runs a command, so both stay outside the trust hash ([trust.md](./trust.md)). `rimz worktree new` reports the counts it seeded and linked.

## Cleanup

A worktree is reclaimed once its work has landed — proven, never assumed. `rimz worktree remove <name>` runs the decision on demand; the agent wrapper runs the same one through the on-disk `rimz worktree cleanup <path>` helper when an agent launched with `--worktree-path` finishes a supervised run or its tab/pane closes while the room stays live ([harness.md → Cleanup](./harness.md#cleanup)), falling back to the in-process implementation if the helper cannot be resolved or spawned. A clean interactive quit drops the pane to an idle shell inside the tree and leaves reclamation to `rimz gc`. Signal-close cleanup runs outside the dying pane's process group, narrowing `rimz gc` to crash residue, dirty work, pending work, and trees still occupied by user panes.

CLI gathers pane cwd, stored agent path, process cwd, and liveness facts once. `worktree::ProtectionSet` normalizes them, excludes the caller's pane and sidebar chrome, and owns containment; a live agent contributes its stored path and process cwd, an unknown agent contributes its stored path, and a proven-dead agent contributes nothing. `ProtectionSet::assess` applies one precedence order: in use, dirty, pending or unknown landing, then removable. Marker absence remains a CLI no-op before assessment.

| Marker | Status | User pane inside | Agent session bound | Decision |
| --- | --- | --- | --- | --- |
| absent | any | any | any | skip — not RimZ-owned |
| present | clean and content-landed | no | no | remove the tree, then delete the branch |
| present | dirty, pending, or unknown | no | no | prompt `keep / remove / shell` on a TTY; keep on EOF or non-TTY |
| present | any | yes | any | skip — in use |
| present | any | any | yes | skip — in use |

**Content-landed** is the conservative core, shared by cleanup, the sidebar, and `rimz gc`. It compares the branch against a ref ladder — the marker's base branch, then `main`, `master`, `origin/HEAD`, then the creation snapshot — where a base branch counts only while it is still a live destination: once it has diverged from the trunk with its own commits already landed there, the ladder falls through to the trunk, so work built on an already-merged branch is measured against where it truly landed. The verdict accepts a branch with no commits beyond the ref, or identical base/head trees; then it accepts a branch whose three-way merge into the comparison ref reproduces that ref's tree, proving the branch adds nothing and recognizing rebase or squash landings even when concurrent edits to the same files shift patch context. Otherwise it asks Git for branch-side non-merge commits whose patch is absent on the comparison side and treats any result as pending, and requires any surviving merge commits' trees to already appear in the comparison ref's recent history. That covers rebased, cherry-picked, squash-landed, and merge-back shapes without trusting ancestry counts or sidebar wakeups. A missing ref or Git error is `unknown`, which keeps the tree.

Branch deletion follows the same proof. The automatic path tries `git branch -d`, escalates to `git branch -D` only after the landed check passes again, and otherwise keeps the branch. Force removal — `remove --force`, or the interactive `remove` on a dirty tree — is the human explicitly choosing destruction, so it uses Git's force path.

Every explicit, wrapper, reconciliation, and GC removal enters the same domain removal path. When the caller's cwd is inside the candidate checkout, that path moves to the repository root before `git worktree remove`, then applies the existing branch-deletion proof. Successful removal retires the worktree's non-live root sessions in the durable store before archiving its channel messages. Explicit removal and reconciliation surface cleanup failure, while wrapper cleanup and GC report it best-effort.

Cohort relaunch uses the same status oracle. When a named team or multi-agent inline layout launched with an explicit `-w <name>` finds closed cohort history in a clean content-landed worktree, it offers to run the ordinary marked-worktree removal path, archives the worktree channel messages as recreated, and then lets the launch path create the worktree again.

## `rimz gc`

`rimz gc` sweeps every clean, marked, content-landed worktree in the current repo that no live user pane occupies and no live-or-unknown agent session binds by recorded launch path or live process cwd, measures the checkout bytes it reclaims, then runs `git worktree prune`. The sweep requires a readable agent roster and skips worktree reclamation when it cannot get one. `gc` reclaims crash residue, trees left after clean agent quits dropped panes to shells, and trees that became safe only after later Git, pane, or agent state changed. The report names every swept tree with its branch fate, and `--dry-run` previews the sweep. A `fresh`-based worktree compares against `origin/…`, so an unfetched merge keeps the tree until a fetch updates the remote-tracking base. Named-channel records stay until `rimz channel rm`; `gc` acts on worktrees only.
