# Worktree CLI

`rimz worktree` creates, lists, enters, lands, and removes RimZ-owned worktrees: ordinary `git worktree` checkouts, each on its own branch, that RimZ marks at creation. Every verb acts only on a marked tree, so a checkout you made with `git worktree add` is never listed, merged, swept, or removed. A worktree's name is also its [channel](../../guide/messaging.md#channels), and `rimz agents --worktree` launches agents into the same trees ([Channel, worktree, and placement](./agents.md#channel-worktree-and-placement)). Why and when to isolate work this way is the [worktrees guide](../../guide/worktrees.md); the marker, the landed proof, and the protection rules are in [the worktree internals](../../internals/harness/worktrees.md).

```sh
rimz worktree new [NAME] [--base <BASE>] [--branch <BRANCH>] [--json]
rimz worktree new [NAME] --from-pr <PR> [--branch <BRANCH>] [--json]
rimz worktree list [--json]
rimz worktree cd <NAME>
rimz worktree path <NAME>
rimz worktree merge <NAME> [--yes]
rimz worktree remove <NAME> [--force]
rimz worktree sweep [--dry-run] [--json]
```

Every verb must run inside a Git repository, and elsewhere refuses with `rimz worktree requires a git repository; cd into a repo checkout`. The [global flags](../cli.md#global-flags) apply to every verb.

`list` has alias `ls`; `remove` has alias `rm`. Bare `rimz worktree` runs `list`.

## Names and paths

A name is one or more segments of ASCII letters, digits, `_`, or `-`, separated by `/`. Give it without the channel's `#`: `feat-a` names the channel `#feat-a`, and `'#feat-a'` is refused as an invalid worktree name.

| You pass | Directory and channel | Branch |
| --- | --- | --- |
| `feat-a` | `feat-a` | `feat-a` |
| `feat/login` | `feat-login` | `feat/login` |
| `feat-a --branch spike/a` | `feat-a` | `spike/a` |
| nothing | a generated adjective-noun pair, such as `swift-harbor` | the same |
| nothing, with `--from-pr 42` | The existing marked tree holding the same-repository head branch, otherwise `pr-42` | [from the pull request](#check-out-a-pull-request) |

Trees live under the `[agents.worktree] dir` template, `../{repo}-worktrees` by default, where `{repo}` is the repository's directory name and a relative path resolves from the repository root ([configuration](../../guide/configuration.md#worktrees)). `cd`, `path`, `merge`, and `remove` take the same name and resolve it the same way, so `feat/login` and `feat-login` both reach the `feat-login` tree.

For those four verbs, a missing name reports ``no worktree `NAME` in DIR; did you mean `NEAR`? `rimz worktree list` shows the N that exist``. The suggestion appears only for a near match; an empty inventory says `none exist`. An unmarked checkout still refuses as ``worktree `NAME` is not a RimZ-managed worktree at PATH``.

Explicit `--channel` lanes and worktrees share one namespace in a repository room ([`--channel`](./agents.md#channel-worktree-and-placement)).

## Create a worktree

The hook event names `worktree.created` and `worktree.removed` are also [loop signals](./events.md#reserved-families).

```console
$ rimz worktree new feat-a
created feat-a
  path   : ~/code/query-engine-worktrees/feat-a
  branch : feat-a, from main at bece28e
  seeded : 2 file(s) from .worktreeinclude
  linked : 1 dir(s) from .worktreelink
  next   : rimz agents <spec> -w feat-a    or    rimz worktree cd feat-a
```

`new` runs `git worktree add --no-track -b <branch> <path> <base>`, writes the ownership marker, and seeds the tree from the repository's `.worktreeinclude` and `.worktreelink` files ([seed the tree](../../guide/worktrees.md#seed-the-tree)). The new branch does not track its base. It then runs the optional `created` hook; success adds `  hook   : worktree.created ran` to the report, and failure fails the command ([hook lifecycle and errors](../../guide/worktrees.md#prepare-the-tree-with-a-hook)). It launches nothing into the tree. Any open messages still queued on the channel of that name are archived.

The branch receipt uses the recorded base branch and seven-character commit prefix, or just the prefix when no base branch was recorded. `new --json` prints one object with `name`, normalized absolute `path`, `branch`, `base_branch` (string or `null`), full `base_ref`, and `reused` (boolean). Warnings stay on stderr.

Seed only paths Git already ignores. A seed Git neither tracks nor ignores lands as an untracked change in every tree it reaches, which is enough for `sweep` to keep that tree with `uncommitted changes` and for `remove` to refuse it. Linked directories need no `.gitignore` entry: `new` adds each one to the exclude file `git rev-parse --git-path info/exclude` names, which for a linked worktree is usually the main checkout's `.git/info/exclude`.

`--base` picks the commit the branch starts from. Without it, `new` uses the `[agents.worktree] base` setting; unset means `head` here.

When a fresh tree's local base branch tracks an upstream and is behind it, creation succeeds with a stderr warning: `warning: base branch main is 4 commits behind origin/main as last fetched; fast-forward main first, or pass --base origin/main` (with the actual branch, upstream, and count; `1 commit` for one). The count uses local upstream refs as last fetched; creation does not fetch. Reused trees, PR checkouts, detached HEADs, bases without an upstream, and failed checks produce no warning.

| `--base` | Branch starts from |
| --- | --- |
| `head` | Your local `HEAD` in the main checkout. |
| `fresh` | `origin/HEAD`, the remote's default branch as last fetched. [`Team launches`](./teams.md#launch-a-team) fetch it first; `new` does not. |
| any other value | That Git ref, such as `main` or `v2.1.0`. |

`base` is the full commit the tree was cut from, and `base branch` the branch or ref it was cut from; `base branch` is omitted when there is none to name, such as a detached `HEAD`. `seeded` and `linked` print only when nonzero. Whether the tree's work has landed is later measured against that base branch ([choosing the comparison ref](../../internals/harness/worktrees.md#choosing-the-comparison-ref)).

| Refusal | When |
| --- | --- |
| ``worktree `NAME` already exists at PATH`` | The directory exists, marked or not. A launch with `-w NAME` reuses a marked tree instead. |
| ``channel `NAME` is held by live agents; pick another name, or stop them with `rimz agents stop` `` | Live agents launched with `--channel NAME` hold the name you passed. |
| ``invalid worktree name `NAME`; ...`` | The name breaks the [name rules](#names-and-paths). |
| `base ref cannot be empty` | `--base ''`. |

## Check out a pull request

```sh
rimz worktree new --from-pr 42
rimz worktree new review-42 --from-pr https://github.com/acme/query-engine/pull/42
```

`--from-pr` takes a pull request number or URL and creates or reuses a tree for that pull request. A URL must name the same host and repository as `origin`. Review-only and fork checkouts require a fresh tree and fetch the PR ref from `origin` (`refs/pull/<N>/head` on GitHub, Gitea, and Forgejo, `refs/merge-requests/<N>/head` on GitLab); a same-repository checkout fetches the head branch itself. `--from-pr` cannot be combined with `--base`.

What the branch tracks depends on whether RimZ can ask the forge who owns the head. It asks through `gh` when `origin` is on `github.com`, and through `tea` when the `origin` host name contains `gitea`, `forgejo`, or `codeberg`.

| Situation | Checkout | Branch | Pushes |
| --- | --- | --- | --- |
| `--branch` given, or a name containing `/` | Review-only | That branch, at the exact PR head | Not configured |
| `origin` is on another host (GitLab, GitHub Enterprise), or its CLI is not installed | Review-only | The worktree name, at the exact PR head | Not configured; the report prints a `review` line with the reason |
| The CLI reports a head branch in `origin` | Same repository | The PR's head branch | Upstream is `origin/<branch>` |
| The CLI reports a head branch in a fork | Fork | The head branch, or `<owner>/<branch>` when a local branch already has that name | To the fork; the report prints `pushes : <fork-url> refs/heads/<branch>` |

When a fork checkout's local branch name differs from the PR's head branch, the report adds a `push :` line with the exact command to push back, such as `git push <fork-url> HEAD:<branch>`.

A same-repository checkout reuses the marked worktree holding the PR's head branch, including a tree created with `-w` and no recorded PR number. Otherwise it adopts an existing local branch into a fresh tree, or creates a tracking branch when none exists. A marker recording another PR number refuses. An explicit name must match the tree holding the branch; omit the name to find that tree automatically. Reuse records the PR number when missing, preserves the tree's creation time, and prints `reused NAME` with only the path and branch lines. It does not seed the tree, rerun its created hook, or archive channel messages.

When the local tip differs, RimZ warns on stderr with the commit counts, the worktree path when present, and a reminder that the previous tip stays in the reflog. In a terminal it asks `Continue from the PR head (remote) or the local tip (local)?`. The default is `remote` when the branch is behind or its local-only commits are patch-equivalent to commits in the PR head; otherwise the default is `local`, even when the two tips merge cleanly. EOF aborts. Choosing `remote` moves the branch to the PR head, using `git reset --keep` in an existing tree so changes that would be overwritten refuse with a commit-or-stash hint. Choosing `local` keeps the tip. Both choices set the upstream to `origin/<branch>`.

Without terminal stdin, only a behind branch advances automatically, with a `fast-forwarding` warning. Rebased or locally changed tips refuse with commands to take the PR head or push the local tip, then rerun; rerunning in a terminal offers the choice instead.

| Refusal | When |
| --- | --- |
| ``could not resolve the head branch of PR #N (REASON); install/log in to gh or tea, or pass --branch <name> for a review-only checkout`` | `gh` or `tea` is installed but fails or prints output RimZ cannot read, most often because it is not logged in. There is no automatic review-only fallback in this case. |
| ``local PR branch `BRANCH` conflicts with the remote head (DETAIL); resolve the local branch or pass --branch <name> for a review-only checkout`` | The branch is held by the main checkout or an unmanaged checkout, or the explicit name differs from its marked tree's name. For a fork, both the bare and the owner-prefixed name are taken. |
| ``local PR branch `BRANCH` differs from the PR head: DIVERGENCE; ...`` | Without terminal stdin, a rebased tip or local-only patches require a decision. The message gives `git -C PATH reset --keep origin/BRANCH` for a held branch or `git branch -f BRANCH origin/BRANCH` otherwise, plus `git push origin BRANCH` as the local-tip alternative. A merely behind branch fast-forwards with a warning. |
| ``could not move local PR branch `BRANCH` to the PR head in PATH; commit or stash changes there first: ...`` | You chose the PR head for a branch a reused worktree holds, and `git reset --keep` refused because that worktree's uncommitted changes touch files the reset would change. |
| ``PR URL targets `REPO` but origin is `ORIGIN` `` | The URL names another repository. |
| ``could not fetch PR #N from REMOTE: ...`` | The fetch failed or ran past 120 seconds. Git never prompts for credentials here. |

## List worktrees

```console
$ rimz worktree list
2 worktrees in ~/code/query-engine-worktrees · 1 removable (rimz worktree sweep) · 1 in use

WORKTREE  BRANCH  STATE             HOLDERS
auth      auth    unlanded · dirty  @coder#auth
feat-a    feat-a  landed            -
```

`list` prints one row per RimZ-owned worktree of the repository, sorted by name. Bare `rimz worktree` does the same. The summary counts removable trees, occupied trees, and unlanded or unknown trees that are not occupied; zero counts are omitted. With none, it prints `no worktrees in DIR`.

| Column | Value |
| --- | --- |
| `WORKTREE` | The name, which is also the channel. |
| `BRANCH` | The branch checked out now, or `-` on a detached `HEAD`. |
| `STATE` | `landed`, `unlanded`, or `unknown`, with ` · dirty` when `git status` shows any change, untracked files included. |
| `HOLDERS` | Holders using the sweep's occupancy rules: at most three handles then `+N`, `pane` for a pane-only holder, `loop task NAME` for each holding task, then `team NAME` for each holding board, or `-`. Ended agents are omitted. |

`--json` prints an array of objects, `[]` when there are none.

| Field | Value |
| --- | --- |
| `name` | The worktree name. |
| `path` | The absolute checkout path. |
| `branch` | The branch checked out now; `null` on a detached `HEAD`. |
| `base_ref` | The full commit the tree was cut from. |
| `base_branch` | The recorded base branch, or `null` for an older marker without one. |
| `dirty` | `true` or `false`. |
| `landed` | `true`, `false`, or `null` when unknown. |
| `reasons` | All keep reason ids in precedence order: `in_use`, `uncommitted_changes`, `unlanded`, `unknown`. Empty means removable. |
| `holders` | `agents` (full handles), `panes` (the count of protected panes with no agent), `loop_tasks` (deduplicated task names), then `teams` (deduplicated team names). Both name arrays are `[]` when none. |

## Open a shell in a worktree

```sh
rimz worktree cd auth
```

`cd` requires terminal stdin and replaces the `rimz` process with your user shell, started in the tree. It first prints `entered PATH in a new shell; exit returns to CWD` on stderr. A command cannot change its parent shell's directory, so exiting that shell returns you to the directory where you ran `cd`. Without a terminal it refuses with ``rimz worktree cd needs a terminal; the tree is at PATH (`rimz worktree path NAME` prints it)``.

## Print a worktree path

```sh
rimz worktree path auth
```

`path` prints the normalized absolute checkout path and nothing else. It needs no terminal and can feed scripts or your shell's `cd`.

## Land a worktree on its base

```console
$ rimz worktree merge auth --yes
landed auth on main
  branch : auth
  head   : 5f0c2a9e1d7b4c3f8a6e2d1b0c9f8e7d6a5b4c3d
  the worktree stays at ~/code/query-engine-worktrees/auth; `rimz worktree remove auth` reclaims it
```

`merge` starts from the recorded base while it remains a live destination, otherwise the trunk. This supports `master` and stacked feature branches. A remote-tracking base such as `origin/main` maps to the local branch whose upstream is that ref, otherwise the trunk's local branch; with neither, landing refuses. The prompt and receipt name the local destination. `list` still measures against the remote-tracking base, so the tree remains `unlanded` until that local branch is pushed.

The target must be a branch checked out somewhere in this repository; a snapshot commit is not a landing target. RimZ rebases the tree's branch onto the target, then advances that branch in its own checkout with `git merge --ff-only`. It never creates a merge commit and has no `--force`. The tree stays in place afterwards; `sweep` or `remove` reclaims it.

Without `--yes`, terminal stdin prompts `land NAME on TARGET? [y/N]`. Declining prints `nothing changed.` on stderr and succeeds. Without a terminal it refuses with ``rimz worktree merge needs a terminal to confirm; pass --yes to land `NAME` on `TARGET` without asking``. `--yes` skips confirmation, not the preconditions.

Every precondition is checked before anything moves. Each refusal begins ``cannot merge worktree `NAME`: ``.

| Refusal | When |
| --- | --- |
| `it is in use by HANDLES` | A live agent or another open pane is in the tree. A stale record from a crashed agent does not count. |
| `TARGET must be checked out in a worktree` | No checkout holds the target branch. |
| `no branch landing target; land it yourself` | The comparison is not a branch (for example, a snapshot commit), no comparison ref is available, or a remote-tracking base has no local destination. |
| `the TARGET checkout has a Git operation in progress` | A rebase, merge, cherry-pick, or revert is under way there. The same refusal names `the worktree checkout`. |
| `the TARGET checkout has local changes; commit or stash them first` | The target checkout is dirty. |
| `its checkout has local changes; commit or stash them first` | The tree is dirty. |
| ``expected branch `BRANCH`, found `OTHER` `` | The tree has switched away from the branch it was created on. |
| `rebase onto TARGET failed; no commits were landed on TARGET: ...` | The rebase conflicted. RimZ aborts it, so neither checkout is left mid-rebase. |
| `TARGET changed while the worktree was rebasing; rerun the command` | The target moved, changed branch, or became dirty during the rebase. |
| `the worktree was rebased onto TARGET but now has local changes; TARGET was not updated` | The tree changed during the rebase. |
| `fast-forwarding TARGET failed: ...` | Git could not advance the target branch. |

## Remove a worktree

```console
$ rimz worktree remove auth
error: worktree `auth` is in use by @coder#auth; use --force to remove it
$ rimz worktree remove feat-a
removed feat-a
```

`remove` runs `git worktree remove` on the tree, runs the optional [`removed` hook](../../guide/worktrees.md#prepare-the-tree-with-a-hook), deletes its branch, ends the store sessions bound to it, and archives the channel's queued messages. It refuses first, in this order:

| Refusal | When |
| --- | --- |
| ``worktree `NAME` is in use by HANDLES; use --force to remove it`` | A live agent or an open pane is in the tree. `HANDLES` reads `an open pane` when no agent is recorded there. A stale record from a crashed agent does not count. |
| ``worktree `NAME` has uncommitted changes (N files); commit or stash them, or pass --force to discard them`` | The tree is dirty. |
| ``worktree `NAME` has N commits not landed on TARGET; run `rimz worktree merge NAME`, or pass --force to delete them`` | The work is unlanded on its comparison ref. |
| ``worktree `NAME` has work RimZ cannot prove landed; land it yourself, or pass --force to delete its branch`` | RimZ cannot prove whether the work landed. |

Without `--force`, the branch is deleted when Git considers it merged or RimZ proves its work landed. Otherwise the branch stays and the report adds `  branch kept: its work is not proven landed on TARGET`.

`--force` skips these checks and removes the tree with `git worktree remove --force`, then deletes the branch with `git branch -D`. When its tip could be read, the receipt prints ``branch deleted: BRANCH was at OID; restore with `git branch BRANCH OID` `` with the full commit id. When the tree was in use, `--force` first prints ``warning: worktree `NAME` is in use by HANDLES; removing it anyway`` on stderr. Holders include agent handles and any unassociated open panes.

`remove` exits 1 if ending the sessions or archiving the messages fails, even though the checkout is already gone. A failure to append the message history only warns and leaves the exit code 0 ([write classes](../../internals/store.md#write-classes)).

## Sweep landed worktrees

```console
$ rimz worktree sweep --dry-run
sweep — would remove 1 · 18 MB · 2 kept

kept, in use (1)
  auth  @coder#auth

kept, unlanded work (1)
  spike

would remove (1) from ~/code/query-engine-worktrees
  feat-a

nothing was removed; run `rimz worktree sweep` to remove the 1
```

`sweep` removes every RimZ-owned worktree that is clean, proven landed, and has no pane, agent, armed loop-task holder, or team board short of `Done`, deleting each branch the way `remove` does without `--force`, and then runs `git worktree prune`. It never forces. `--dry-run` prints the same report and removes nothing; in a repository with no RimZ store it prints `sweep — skipped · no RimZ store here` and creates none. A sweep with nothing to remove prints `sweep — nothing to remove · N kept`.

An enabled machine or instance task holds its root and its run directory until removed, disabled, or past its deadline; a paused task still holds them. `--each-worktree` holds only its root. A task that launches with `--worktree` holds only its root unless it has a check, judge, `when`, or watch that needs its run directory too. Project tasks do not hold trees. Holders print as `loop task NAME`; they also keep trees during unattended agent-exit cleanup. Explicit `remove` and `merge` ignore loop holders.

A board behind the checkout-root `.rimz/team` link holds the tree until its first `Stage:` line says `Done`. A board with no stage, or one that cannot be read for a reason other than not-found, also holds it. A missing board, a dangling link, a `.rimz` shared through `.worktreelink`, and a legacy root `blackboard.md` hold nothing. Boards in subdirectories are not checked. The holder prints as `team NAME`, or `team unknown` when the run's team cannot be read. It also protects unattended agent-exit cleanup and `gc`; explicit `remove` and `merge` ignore board holders. Release it with `rimz teams flip TEAM#LANE Done "finished"`.

Each removal runs the optional [`removed` hook](../../guide/worktrees.md#prepare-the-tree-with-a-hook); `--dry-run` runs no hooks.

| `kept` reason | Meaning |
| --- | --- |
| `in use` | An open pane or an agent is in the tree, an armed loop task needs it, or its team board is short of `Done` (including an unreadable board or one with no stage). Unlike `remove`, an agent RimZ cannot prove dead still counts. |
| `uncommitted changes` | The tree is dirty. |
| `unlanded work` | The work has not landed. |
| `unknown` | RimZ cannot prove whether the work landed. |

The report groups each kept tree under its first reason, then names additional reasons with `· also REASON`. The removed names wrap to the terminal width; the directory prints once. `--json` prints `dry_run`, `removed`, `kept`, and `failed`. Removed entries carry `name`, `branch`, `path`, `bytes`, `branch_deleted` (`null` on a dry run), and `archive_error`; kept entries carry `name`, `path`, `reasons`, and `holders` with the same ids and holder shape as `list --json`. A dry run with no store adds `skipped: "no_store"` and empty arrays.

Quitting the agent that worked in a tree reclaims nothing: its pane says the worktree was kept and names `rimz worktree sweep`, then drops back to a shell, and the tree, its branch, and its files stay. A tree goes away through `sweep`, through `remove`, or through the daily `gc` below, which runs this same sweep.

A tree that fails to remove prints `  failed: PATH — ERROR`, and a failed message archive prints `    message archive failed: ERROR` under its row. The sweep continues past both and then exits 1 with `worktree sweep completed with N problem(s)`. If the agent roster, machine loop file, workspace listing, any workspace's instance-task file, or the owned-worktree listing cannot be read, `sweep` removes nothing and exits 1, naming the failed source. Listing is best-effort: unreadable task sources contribute no loop holders to `list`, and a failed board enumeration contributes no team holders (the listing itself still requires Git's worktree list).

[`rimz gc`](./maintenance.md#sweep-stale-state) runs the same sweep as one area of its report.
