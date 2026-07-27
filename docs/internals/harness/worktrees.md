# RimZ-owned worktrees

> The full life of a RimZ-owned Git worktree: the ownership marker, creation and seeding, the proof that a branch's work landed, and every path that removes a tree. The code is `crates/rimz/src/worktree.rs` plus its three private submodules; [harness.md](./harness.md) is the map for this area. For users, the commands are [cli/worktree.md](../../reference/cli/worktree.md) and the guide is [worktrees.md](../../guide/worktrees.md).

A worktree is one Git checkout of the repository on its own branch, and RimZ runs its whole life: create it for a line of work, seed it with the untracked files an agent needs, and reclaim it once its work has landed on the base branch. The tree's name is also a [channel](./messaging.md#channels), so `@coder#feat-a` addresses the agent working inside it.

The subsystem stands on its own. `rimz worktree new`, `list`, `remove`, and the `rimz gc` sweep work whether or not an agent ever launches into a tree, and the harness calls the same entry points when it does ([harness.md](./harness.md#reclaiming-a-pane)). What agents do inside a tree belongs to [harness.md](./harness.md); the lane the tree backs belongs to [messaging.md](./messaging.md#channels).

## The one rule: RimZ touches only what it marked

Every managed operation starts by reading `rimz-worktree.json`. No marker means the checkout belongs to the user, and cleanup, `remove`, and `gc` all skip it, even when its path matches the configured directory template exactly. That single check is what lets RimZ delete directories and branches without ever endangering a checkout someone made by hand.

The marker lives in the worktree's Git admin directory (`.git/worktrees/<name>/rimz-worktree.json`), not in the checkout, so the working tree stays free of RimZ metadata and no `.gitignore` entry is needed.

| Field | Purpose |
| --- | --- |
| `version` | Marker schema version, currently `4`. |
| `name` | Worktree and channel name. Branch-style requests arrive here already dashed. |
| `branch` | The branch checked out in the tree. |
| `base_branch` | Branch name the tree was cut from. The first rung of the landed-comparison ladder. |
| `base_ref` | Resolved base commit at creation. The last-resort comparison when no branch survives. |
| `from_pr` | Pull-request number for a `--from-pr` tree. |
| `repo_root`, `worktree_path` | Where the tree came from and where it lives. |
| `created_at` | Creation timestamp. |

`base_branch` arrived in version 3 and `from_pr` in version 4, both as `#[serde(default)]` options, so markers written by older builds still deserialize and their trees still clean up. Two tests in [`worktree/tests.rs`](../../../crates/rimz/src/worktree/tests.rs) pin that compatibility. Any field added later needs the same treatment.

## Module map

| Path | Owns |
| --- | --- |
| [`worktree.rs`](../../../crates/rimz/src/worktree.rs) | The marker, creation, the dirty and landed status, the removal policy, discovery, and the shared `git` helpers. |
| [`worktree/pr.rs`](../../../crates/rimz/src/worktree/pr.rs) | `--from-pr`: strategy selection, forge-CLI head resolution, fork remote wiring, PR ref fetching. |
| [`worktree/include.rs`](../../../crates/rimz/src/worktree/include.rs) | `.worktreeinclude` file copying and its containment rules. |
| [`worktree/link.rs`](../../../crates/rimz/src/worktree/link.rs) | `.worktreelink` directory symlinks. |
| [`worktree/exclude.rs`](../../../crates/rimz/src/worktree/exclude.rs) | Shared `info/exclude` registration for linked directories and team scratch files. |
| [`cli/worktree.rs`](../../../crates/rimz/src/cli/worktree.rs) | `rimz worktree new`, `list`, `remove`, and the hidden `cleanup` helper. |
| [`cli/worktree_protection.rs`](../../../crates/rimz/src/cli/worktree_protection.rs) | Runtime pane and agent fact gathering for explicit removal, wrapper cleanup, and automatic gc. |
| [`cli/gc.rs`](../../../crates/rimz/src/cli/gc.rs) | The `rimz gc` worktree sweep and its report. |
| [`cli/agents_cmd/exec.rs`](../../../crates/rimz/src/cli/agents_cmd/exec.rs) | The exec wrapper that triggers cleanup when an agent pane ends. |
| [`cli/agents_cmd/reconcile.rs`](../../../crates/rimz/src/cli/agents_cmd/reconcile.rs) | Cohort relaunch into a worktree that already exists. |

The domain module runs Git through `crate::proc::git_command` with `LC_ALL=C` and returns typed `WorktreeErr` values; every user-visible string and prompt lives in the CLI layer.

## Creating a worktree

`create` and `create_from_pr` walk the same five steps.

1. **Resolve the name.** An omitted name becomes an adjective-noun pair seeded from a UUIDv7, retried with a numeric suffix until an unused directory appears. A requested name is validated per segment (ASCII alphanumerics, `_`, `-`), and a `/` in it names the branch directly while the directory and channel take the dashed spelling: `feat/login` gives branch `feat/login` in directory `feat-login`. `--branch` overrides the derived branch without touching the directory name.
2. **Resolve the base.** [`WorktreeConfig`](../../../crates/rimz/src/config/worktree.rs) carries the per-machine `dir` template (default `../{repo}-worktrees`, `{repo}` expanding to the repository basename) and `base`. `head` branches from local `HEAD`, `fresh` from `origin/HEAD`, and anything else is a literal ref. Both the resolved commit and the branch name Git reports for that choice go into the marker.
3. **Add the tree.** `git worktree add` in one of three shapes: `-b <branch> <base>` for a fresh branch, `--track -b <branch> origin/<branch>` for a same-repository PR head, and a bare `add <path> <branch>` when a local branch already exists and was fast-forwarded to the PR head.
4. **Write the marker.** Temp-file-plus-rename into the Git admin directory. Ownership begins here, so a failure at this step leaves an ordinary unmanaged Git worktree rather than a half-managed one.
5. **Seed the tree.** `.worktreeinclude` copies, then `.worktreelink` symlinks, both best-effort.

A launch that names its worktree reuses an existing one; `rimz worktree new` refuses instead. A reused tree is never re-seeded, and `CreatedWorktree` reports zero included and linked counts so the CLI stays quiet about work it did not do.

`resolve_launch_checkout` is the seam the agent launcher enters. It returns the cwd every pane in the layout gets, plus the worktree name that becomes the channel, and it fails fast with `LaunchWorktreeRequiresRepo` when the room is not a Git repository.

### From a pull request

`--from-pr <number|url>` produces the same marked tree from a pull request head. A URL must resolve to the same host and repository as `origin` before any network call.

Head resolution picks one of three strategies:

| Condition | Strategy | Branch | Pushes |
| --- | --- | --- | --- |
| `--branch` given, or the name was branch-style | Review-only | The requested branch, at the exact PR head commit | Unconfigured |
| `origin` has no supported forge CLI, or `gh`/`tea` is not installed | Review-only | Derived from the worktree name | Unconfigured, with the reason reported |
| The forge CLI reports a same-repository head | Same-repository | The PR head branch, tracking `origin/<branch>` | A plain `git push` updates the PR |
| The forge CLI reports a fork head | Fork | The head branch, or `<owner>/<branch>` when that name is taken | `branch.<b>.remote` set to the fork URL, `branch.<b>.merge` to the head ref |

Review-only and fork checkouts fetch the host's PR ref (`refs/pull/<N>/head` on GitHub, Gitea, and Forgejo; `refs/merge-requests/<N>/head` on GitLab) into a per-process temporary ref, `refs/rimz/pr/<N>-<pid>-<nonce>`, resolve its object ID, and delete the ref through a `Drop` guard. Shared `FETCH_HEAD` state never becomes checkout authority, so concurrent PR checkouts cannot cross-contaminate.

An existing local branch is adopted only when its tip is an ancestor of the remote head, in which case it fast-forwards; a diverged or ahead branch refuses with the reason. Reusing a named PR worktree requires the marker's `from_pr` to equal the requested number.

Network calls are bounded: 10 seconds for the forge CLI head query, 120 seconds for a fetch, with `GIT_TERMINAL_PROMPT=0` so a credential prompt fails instead of hanging a launch.

The marker of a PR tree records the repository trunk as its base, so the ordinary cleanup proof reclaims it once the pull request's content lands on trunk, in any merge, squash, or rebase shape.

### Seeding: `.worktreeinclude` and `.worktreelink`

`git worktree add` checks out tracked files at the base ref and nothing else, so the `.env` and local config an agent needs to actually run never follow it. Two optional committed files at the repository root close that gap:

- **`.worktreeinclude`** lists glob patterns, one per line. Each match is copied from the main checkout into the new tree at the same repo-relative path. `*` stays within a path component, `**` crosses directories, dotfiles match a leading `*`, and a matched directory copies recursively.
- **`.worktreelink`** lists relative directory paths, one per line. Each is symlinked from the main checkout into the new tree and registered in the tree's effective `info/exclude` as an anchored `/<path>` pattern. This is for heavy machine-local data whose contents are branch-independent, such as model or fixture caches.

Both skip blank lines and `#` comments, and both confine every source to the project root. Absolute patterns and patterns containing `..` are skipped. Top-level symlink matches are skipped. Every copied file is re-checked against the canonicalized project root, because `glob`'s `**` descends through symlinked directories: a committed symlink pointing out of the repository would otherwise pull host files into a tree an agent can read. Linking additionally requires a real directory, refuses to clobber an existing destination, and writes an absolute symlink.

Seeding is best-effort enrichment layered on top of creation. A missing file is a silent no-op; a pattern matching nothing, a failed copy, or an unlinkable directory warns through `tracing` and is skipped, and the tree and its agents still launch. Neither file can run a command, which is why both stay outside the trust hash ([trust.md](./trust.md)).

Because the `info/exclude` a linked directory is registered in is commonly the repository's shared one, the same branch-independent directory ends up excluded in the main checkout and in sibling worktrees too.

### Team scratch files

A team definition's `scratch-files` entries are verbatim gitignore patterns for ephemeral cooperation records. Fresh launch, explicit cohort resume (including the relaunch reconciliation path), and place-first team restore all register the patterns in the checkout's effective `info/exclude` before panes start. Registration is idempotent, uses an atomic whole-file replacement, and stays best-effort: a Git or filesystem failure warns but does not block the team.

Git commonly resolves a linked worktree's effective exclude file to the repository's shared `.git/info/exclude`. Team scratch patterns therefore hide matching untracked names in the main checkout and every sibling worktree too. Removing the appended lines reverses the registration; RimZ does not remove them automatically because another team or checkout may still depend on them.

A content-landed worktree containing only declared scratch files is clean and therefore eligible for unprompted wrapper cleanup and `rimz gc`; removing the worktree removes those scratch files with it. That lifecycle consequence is the purpose of declaring the records ephemeral.

## Status: dirty and landed

`status(path, marker)` answers the two questions every removal decision needs.

**Dirty** is `git status --porcelain` returning anything at all, untracked files included.

**Landed** asks whether the branch's content already exists on the branch it should return to. It is proven, never assumed, and the same `content_landed` function serves cleanup, `rimz gc`, the `worktree list` table, and the sidebar's group header.

### Choosing the comparison ref

Before proving anything, `comparison_ref` picks what to prove against:

1. The marker's `base_branch`, while it still resolves **and** has not been superseded.
2. The repository trunk: the first of `main`, `master`, or the `origin/HEAD` default branch that resolves.
3. The marker's `base_ref`, when it is a raw commit id that still resolves.

A base branch is superseded once it has diverged from the trunk and its own commits are themselves content-landed there. Long-lived stacked work is the case that motivates this: a feature cut from another feature branch is measured against that branch while the branch is a live destination, and against the trunk once the branch itself has merged. An ancestor base branch stays the comparison, since nothing has moved.

No usable comparison ref yields `Unknown`, which keeps the tree.

### The content-landed ladder

`content_landed(cwd, comparison, head)` walks cheap proofs before expensive ones and returns `Landed`, `Pending`, or `Unknown`. Any Git failure short-circuits to `Unknown`, so a broken repository keeps its trees.

| Rung | Check | Verdict |
| --- | --- | --- |
| 1 | `rev-list --count <comparison>..<head>` is zero | Landed: nothing was committed past the comparison |
| 2 | Both refs resolve to the same tree object | Landed: content identical, whatever the history shape |
| 3 | `merge-tree --write-tree` of head into comparison reproduces comparison's own tree | Landed: the branch adds nothing, even when patch context drifted |
| 4 | `log --right-only --cherry-pick --no-merges` reports patch residue | Decided by the branch-tip proof below |
| 5 | No residue and no surviving merge commits | Landed |
| 6 | No residue, and every surviving merge commit's tree appears in the comparison's recent history | Landed: a merge-back landing. Otherwise pending |

Rung 4 is the interesting one. Patch residue alone does not mean pending: a squash landing followed by more commits on the trunk leaves residue while the work is genuinely in. So residue is resolved by scanning the comparison's exclusive history (`<head>..<comparison>`) for the branch tip's exact tree. Finding it proves the destination once held the branch's complete final state, and its absence is what finally reports pending. That scan runs uncapped, because a landing can be arbitrarily far back; the rung 6 scan of the comparison's own history is capped at `LANDED_BASE_SCAN_CAP` (500 commits).

Together these rungs cover rebased, cherry-picked, squash-landed, and merge-back shapes without ever trusting ancestry counts or sidebar state.

### `on_trunk_first_parent`, a different question

The sidebar asks something the removal policy never does: has this tree done any work of its own yet? `on_trunk_first_parent` answers it by checking whether HEAD sits on the trunk's own first-parent chain, which a tree that only tracked the trunk does and a tree carrying its own commits does not. It feeds the `did_work` field in the sidebar's git-stats cache and nothing else.

The sidebar also resolves its own trunk, trying the per-machine `[sidebar] trunk` setting before `main`, `master`, and the remote default. On a repository that sets one, the sidebar's landed glyph and the `MERGED` column of `rimz worktree list` are answering against different refs and can disagree ([sidebar/state.md](../sidebar/state.md)).

## Removing a worktree

### Protection facts

Git safety is not enough on its own: a tree can be clean and landed while a human still has a shell open in it. `ProtectionSet` folds the room's live state into the answer.

The CLI gathers the facts once (pane cwds from the mux, agent rows and liveness from the store) and `ProtectionSet::from_facts` normalizes them:

- Panes contribute their cwd, except the sidebar's own chrome and the caller's own pane.
- A **live** agent contributes both its recorded launch path and its real process cwd.
- An **unknown** agent contributes its recorded launch path under the `Unproven` occupancy policy only.
- A **dead** agent contributes nothing.

A candidate checkout is protected when any of those normalized paths lies inside it. Paths are folded lexically (`.` and `..` removed) before comparison, so `/repo/../repo-worktrees/demo` and `/repo-worktrees/demo` match.

`Occupancy` is what separates the two audiences for that fold. Automatic reclamation runs with nobody watching, so it uses `Unproven`: an agent RimZ cannot prove dead still holds its tree. An explicit `rimz worktree remove` uses `ProvenLive`, where only a running process or an open pane holds it, so a stale session record left by a crash never blocks the very command that would retire it.

### The assessment

`ProtectionSet::assess` returns one verdict in a fixed precedence: **InUse**, then **Dirty**, then **NotLanded**, then **Removable**. Safety first, so a dirty tree someone is standing in reports as in use rather than dirty. Callers never re-order these checks; they only choose what to do with each verdict.

| Verdict | `worktree remove` | Wrapper cleanup | `rimz gc` | Cohort relaunch |
| --- | --- | --- | --- | --- |
| Removable | Remove and delete the branch | Remove, log to stderr | Remove, count the bytes | Offer to remove, then relaunch fresh |
| Dirty | Refuse; `--force` overrides | Prompt `keep / remove / shell` on a TTY, keep otherwise | Keep, reported as "uncommitted changes" | Offer a worktree-scoped resume |
| NotLanded | Refuse; `--force` overrides | Same prompt | Keep, reported as "not merged yet" | Offer a worktree-scoped resume |
| InUse | Refuse, naming who holds it; `--force` warns and proceeds | Skip silently | Keep, reported as "in use" | Focus the live pane instead |

The domain owns every one of those refusals. `cli/worktree.rs` only adds the handles to the message, by matching the typed `WorktreeErr::InUse` and asking `agents_in_worktree` who is bound to the checkout; when no agent matches, the holder is a bare pane and the message says so.

Cohort relaunch is the one caller that assesses against an empty protection set, because it has already established that the cohort's own panes are closed, and Git state alone decides between recreating the tree and resuming into it.

### Branch deletion and retirement

Removal is `git worktree remove` followed by branch deletion, both from the repository root (the domain path steps out of the checkout first when the caller's cwd is inside it).

Branch deletion re-runs the same proof rather than trusting Git's merge check. `git branch -d` first; a branch already gone counts as deleted; a "not fully merged" refusal escalates to `-D` only when `content_landed` passes again, and otherwise returns `KeptUnmerged` so the CLI can tell the user their branch survived.

After Git removal succeeds, `retire_removal` runs two independent durable effects: ending the store sessions bound to that path or branch, and archiving the worktree channel's messages. They run independently and both results come back in a `#[must_use]` struct, because Git removal is already irreversible by that point and one failure must not swallow the other. Explicit removal and cohort reconciliation surface these failures; wrapper cleanup and `gc` log them and move on.

## Who triggers removal

Four callers enter the same domain path.

**`rimz worktree remove <name>`** is the explicit one, described above. It answers to the live room the same way the automatic paths do, refusing while an agent process or an open pane is inside the tree and naming what it found.

**The exec wrapper** reclaims a tree an agent owned. The shape of the agent's exit decides what happens:

| Exit shape | Wrapper behavior |
| --- | --- |
| Clean interactive quit | Record the end trace, print the relaunch hint, exec a shell inside the tree. `rimz gc` reclaims it later. |
| Supervised `-p` run completes | Run cleanup in the foreground; an interactive prompt is allowed. |
| Tab or pane close, or SIGHUP/SIGTERM, while the mux session still accepts closes | Spawn cleanup detached with `--non-interactive`, null stdio, and its own process group, so it survives the disappearing pane. |
| Abrupt exit with the mux session gone, wedged, or resurrected | Skip cleanup entirely. Recovery comes from the sidebar producer's live roster ([harness.md](./harness.md#reclaiming-a-pane)). |

Cleanup prefers the on-disk `rimz worktree cleanup <path>` binary over its own in-process implementation, and falls back in-process when the binary cannot be resolved or spawned. The non-interactive path sleeps briefly first so the store roster settles before protection facts are read. On the way in, the wrapper refuses to launch at all if the marker vanished between the launch decision and the exec, rather than silently running the agent in the project root.

**`rimz gc`** sweeps every managed tree in the repository that assesses Removable, measures the bytes reclaimed, and runs `git worktree prune` afterwards. It requires a readable agent roster and skips the whole worktree area without one, reporting why, alongside its other skip reasons (not a repository, no store, listing failed). Every managed tree appears in the report: removed trees with their branch fate, retained trees with the reason that kept them. `--dry-run` produces the same accounting without acting. Named channel records outlive `gc`; only `rimz channel rm` removes those.

**Cohort relaunch** handles `rimz agents <team> -w <name>` against a tree that already exists. `inspect_cohort_relaunch` classifies the prior cohort as absent, present, or closed; a closed cohort in a Removable tree offers to remove and recreate, and a closed cohort in a kept tree offers a resume instead ([harness.md](./harness.md#cohort-relaunch-reconciliation)).

## Invariants worth preserving

- Read the marker before acting. An unmarked checkout is a user's checkout.
- Keep RimZ metadata out of the working tree. The marker belongs in the Git admin directory.
- Prove a landing. Ancestry counts, branch names, and sidebar state are hints, never verdicts, and every uncertain answer keeps the tree.
- Keep the refusals in the domain. A command may enrich the message with names it has; it may not decide removal safety for itself.
- Keep seeding best-effort and non-executing. A launch never fails because a glob missed, and neither seed file may gain the power to run a command without entering the trust hash.
- Keep new marker fields optional with `#[serde(default)]`, so an older tree still cleans up after an upgrade.
- Sidebar code reads markers through `read_marker_from_checkout_metadata`, which follows the `.git` file itself and forks no Git process. Keep the projection path off `git rev-parse`.
