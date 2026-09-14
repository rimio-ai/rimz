# Worktree CLI

`rimz worktree` creates, lists, enters, lands, and removes RimZ-owned worktrees: ordinary `git worktree` checkouts, each on its own branch, that RimZ marks at creation. Every verb acts only on a marked tree, so a checkout you made with `git worktree add` is never listed, merged, swept, or removed. A worktree's name is also its [channel](./channel.md), and `rimz agents --worktree` launches agents into the same trees ([Channel, worktree, and placement](./agents.md#channel-worktree-and-placement)). Why and when to isolate work this way is the [worktrees guide](../../guide/worktrees.md); the marker, the landed proof, and the protection rules are in [the worktree internals](../../internals/harness/worktrees.md).

```sh
rimz worktree new [NAME] [--base <BASE>] [--branch <BRANCH>]
rimz worktree new [NAME] --from-pr <PR> [--branch <BRANCH>]
rimz worktree list [--json]
rimz worktree cd <NAME>
rimz worktree merge <NAME>
rimz worktree remove <NAME> [--force]
rimz worktree sweep [--dry-run]
```

Every verb must run inside a Git repository, and elsewhere refuses with `rimz worktree requires a git repository; cd into a repo checkout`. The [global flags](../cli.md#global-flags) apply to every verb.

## Names and paths

A name is one or more segments of ASCII letters, digits, `_`, or `-`, separated by `/`. Give it without the channel's `#`: `feat-a` names the channel `#feat-a`, and `'#feat-a'` is refused as an invalid worktree name.

| You pass | Directory and channel | Branch |
| --- | --- | --- |
| `feat-a` | `feat-a` | `feat-a` |
| `feat/login` | `feat-login` | `feat/login` |
| `feat-a --branch spike/a` | `feat-a` | `spike/a` |
| nothing | a generated adjective-noun pair, such as `swift-harbor` | the same |
| nothing, with `--from-pr 42` | `pr-42` | [from the pull request](#check-out-a-pull-request) |

Trees live under the `[agents.worktree] dir` template, `../{repo}-worktrees` by default, where `{repo}` is the repository's directory name and a relative path resolves from the repository root ([configuration](../../guide/configuration.md#worktrees)). `cd`, `merge`, and `remove` take the same name and resolve it the same way, so `feat/login` and `feat-login` both reach the `feat-login` tree.

Named channels and worktrees share one namespace in a repository room ([channel names](./channel.md#channel-names)).

## Create a worktree

```console
$ rimz worktree new feat-a
created feat-a
  path   : ~/code/query-engine-worktrees/feat-a
  branch : feat-a
  base branch: main
  base   : bece28e8adc3d54ed07a03764b25452117cdce6d
  seeded : 2 file(s) from .worktreeinclude
  linked : 1 dir(s) from .worktreelink
```

`new` runs `git worktree add -b <branch> <path> <base>`, writes the ownership marker, and seeds the tree from the repository's `.worktreeinclude` and `.worktreelink` files ([seed the tree](../../guide/worktrees.md#seed-the-tree)). It launches nothing into the tree. Any open messages still queued on the channel of that name are archived.

`--base` picks the commit the branch starts from. Without it, `new` uses the `[agents.worktree] base` setting, which defaults to `head`.

| `--base` | Branch starts from |
| --- | --- |
| `head` | Your local `HEAD` in the main checkout. |
| `fresh` | `origin/HEAD`, the remote's default branch as last fetched. |
| any other value | That Git ref, such as `main` or `v2.1.0`. |

`base` is the full commit the tree was cut from, and `base branch` the branch or ref it was cut from; `base branch` is omitted when there is none to name, such as a detached `HEAD`. `seeded` and `linked` print only when nonzero. Whether the tree's work has landed is later measured against that base branch ([choosing the comparison ref](../../internals/harness/worktrees.md#choosing-the-comparison-ref)).

| Refusal | When |
| --- | --- |
| ``worktree `NAME` already exists at PATH`` | The directory exists, marked or not. A launch with `-w NAME` reuses a marked tree instead. |
| ``channel `NAME` is a named channel; use `rimz channel new` or pick another name`` | A named channel holds the name you passed. |
| ``invalid worktree name `NAME`; ...`` | The name breaks the [name rules](#names-and-paths). |
| `base ref cannot be empty` | `--base ''`. |

## Check out a pull request

```sh
rimz worktree new --from-pr 42
rimz worktree new review-42 --from-pr https://github.com/acme/query-engine/pull/42
```

`--from-pr` takes a pull request number or URL and creates the tree at that pull request's head. A URL must name the same host and repository as `origin`. Review-only and fork checkouts fetch the PR ref from `origin` (`refs/pull/<N>/head` on GitHub, Gitea, and Forgejo, `refs/merge-requests/<N>/head` on GitLab); a same-repository checkout fetches the head branch itself. `--from-pr` cannot be combined with `--base`.

What the branch tracks depends on whether RimZ can ask the forge who owns the head. It asks through `gh` when `origin` is on `github.com`, and through `tea` when the `origin` host name contains `gitea`, `forgejo`, or `codeberg`.

| Situation | Checkout | Branch | Pushes |
| --- | --- | --- | --- |
| `--branch` given, or a name containing `/` | Review-only | That branch, at the exact PR head | Not configured |
| `origin` is on another host (GitLab, GitHub Enterprise), or its CLI is not installed | Review-only | The worktree name, at the exact PR head | Not configured; the report prints a `review` line with the reason |
| The CLI reports a head branch in `origin` | Same repository | The PR's head branch | Upstream is `origin/<branch>` |
| The CLI reports a head branch in a fork | Fork | The head branch, or `<owner>/<branch>` when a local branch already has that name | To the fork; the report prints `pushes : <fork-url> refs/heads/<branch>` |

When a fork checkout's local branch name differs from the PR's head branch, the report adds a `push :` line with the exact command to push back, such as `git push <fork-url> HEAD:<branch>`.

A same-repository checkout reuses an existing local branch of that name when the branch is at or behind the PR head, fast-forwarding it first.

| Refusal | When |
| --- | --- |
| ``could not resolve the head branch of PR #N (REASON); install/log in to gh or tea, or pass --branch <name> for a review-only checkout`` | `gh` or `tea` is installed but fails or prints output RimZ cannot read, most often because it is not logged in. There is no automatic review-only fallback in this case. |
| ``local PR branch `BRANCH` conflicts with the remote head (DETAIL); resolve the local branch or pass --branch <name> for a review-only checkout`` | The local branch is checked out elsewhere, is ahead of the PR head, or has diverged from it. For a fork, both the bare and the owner-prefixed name are taken. |
| ``PR URL targets `REPO` but origin is `ORIGIN` `` | The URL names another repository. |
| ``could not fetch PR #N from REMOTE: ...`` | The fetch failed or ran past 120 seconds. Git never prompts for credentials here. |

## List worktrees

```console
$ rimz worktree list
WORKTREE  BRANCH  AGENTS   DIRTY  MERGED  PATH
auth      auth    @coder   dirty  pending ~/code/query-engine-worktrees/auth
feat-a    feat-a  -        -      yes     ~/code/query-engine-worktrees/feat-a
```

`list` prints one row per RimZ-owned worktree of the repository, sorted by name.

| Column | Value |
| --- | --- |
| `WORKTREE` | The name, which is also the channel. |
| `BRANCH` | The branch checked out now, or `-` on a detached `HEAD`. |
| `AGENTS` | Handles of the agents the room records in the tree, or `-`. |
| `DIRTY` | `dirty` when `git status` shows any change, untracked files included. |
| `MERGED` | `yes` when the work is proven landed, `pending` when it is not, `?` when RimZ cannot tell. |
| `PATH` | The checkout, relative to your home directory where possible. |

`--json` prints an array of objects, `[]` when there are none. It carries no agent handles.

| Field | Value |
| --- | --- |
| `name` | The worktree name. |
| `path` | The absolute checkout path. |
| `branch` | The branch checked out now; `null` on a detached `HEAD`. |
| `base_ref` | The full commit the tree was cut from. |
| `dirty` | `true` or `false`. |
| `landed` | `true`, `false`, or `null` when unknown; the `MERGED` column as a value. |

## Open a shell in a worktree

```sh
rimz worktree cd auth
```

`cd` replaces the `rimz` process with your user shell, started in the tree. A command cannot change its parent shell's directory, so exiting that shell returns you to the directory where you ran `cd`. It refuses a name with no checkout (``worktree `NAME` does not exist at PATH``) and a checkout RimZ did not create (``worktree `NAME` is not a RimZ-managed worktree at PATH``).

## Land a worktree on main

```console
$ rimz worktree merge auth
merged auth into main
  branch : auth
  head   : 5f0c2a9e1d7b4c3f8a6e2d1b0c9f8e7d6a5b4c3d
```

`merge` rebases the tree's branch onto `main`, then advances `main` in the main checkout with `git merge --ff-only`. It never creates a merge commit and has no `--force`. The branch must be named `main` exactly: a repository whose trunk is `master` cannot use `merge`. The tree stays in place afterwards; `sweep` or `remove` reclaims it.

Every precondition is checked before anything moves. Each refusal begins ``cannot merge worktree `NAME`: ``.

| Refusal | When |
| --- | --- |
| `it is in use by HANDLES` | A live agent or another open pane is in the tree. A stale record from a crashed agent does not count. |
| `main must be checked out at PATH` | The main checkout is on another branch. |
| `the main checkout has a Git operation in progress` | A rebase, merge, cherry-pick, or revert is under way there. The same refusal names `the worktree checkout`. |
| `the main checkout has local changes; commit or stash them first` | The main checkout is dirty. |
| `its checkout has local changes; commit or stash them first` | The tree is dirty. |
| ``expected branch `BRANCH`, found `OTHER` `` | The tree has switched away from the branch it was created on. |
| `rebase onto main failed; no commits were merged into main: ...` | The rebase conflicted. RimZ aborts it, so neither checkout is left mid-rebase. |
| `main changed while the worktree was rebasing; rerun the command` | Someone committed to `main` during the rebase. |
| `the worktree was rebased onto main but now has local changes; main was not updated` | The tree changed during the rebase. |

## Remove a worktree

```console
$ rimz worktree remove auth
error: worktree `auth` is in use by @coder; use --force to remove it
$ rimz worktree remove feat-a
removed feat-a
```

`remove` runs `git worktree remove` on the tree, deletes its branch, ends the store sessions bound to it, and archives the channel's queued messages. It refuses first, in this order:

| Refusal | When |
| --- | --- |
| ``worktree `NAME` is in use by HANDLES; use --force to remove it`` | A live agent or an open pane is in the tree. `HANDLES` reads `an open pane` when no agent is recorded there. A stale record from a crashed agent does not count. |
| ``worktree `NAME` has local changes or work not proven landed; use --force to remove it`` | The tree is dirty, or its work is not proven landed on its base. One message covers both; `rimz worktree list` shows which. |

Without `--force`, the branch is deleted when Git considers it merged or RimZ proves its work landed. Otherwise the branch stays and the report adds `  branch kept: work not proven merged into its base`.

`--force` skips these checks and removes the tree with `git worktree remove --force`, then deletes the branch with `git branch -D`. Unlanded commits on that branch are deleted with it, and no notice says so. When the tree was in use, `--force` first prints ``warning: worktree `NAME` is in use by HANDLES; removing it anyway`` on stderr.

`remove` exits 1 if ending the sessions or archiving the messages fails, even though the checkout is already gone. A failure to append the message history only warns and leaves the exit code 0 ([write classes](../../internals/store.md#write-classes)).

## Sweep landed worktrees

```console
$ rimz worktree sweep --dry-run
sweep — would remove 1 · 18 MB · 2 kept
  would remove: feat-a — /home/me/code/query-engine-worktrees/feat-a
  kept: auth — in use
  kept: spike — not merged yet
```

`sweep` removes every RimZ-owned worktree that is clean, proven landed, and unoccupied, deleting each branch the way `remove` does without `--force`, and then runs `git worktree prune`. It never forces. `--dry-run` prints the same report and removes nothing; in a repository with no RimZ store it prints `sweep — skipped · no RimZ store here` and creates none. A sweep with nothing to remove prints `sweep — nothing to remove · N kept`.

| `kept` reason | Meaning |
| --- | --- |
| `in use` | An open pane or an agent is in the tree. Unlike `remove`, an agent RimZ cannot prove dead still counts. |
| `uncommitted changes` | The tree is dirty. |
| `not merged yet` | The work is pending or unknown. |

A tree that fails to remove prints `  failed: PATH — ERROR`, and a failed message archive prints `    message archive failed: ERROR` under its row. The sweep continues past both and then exits 1 with `worktree sweep completed with N problem(s)`. If the agent roster cannot be read, `sweep` removes nothing and exits 1.

[`rimz gc`](./maintenance.md#sweep-stale-state) runs the same sweep as one area of its report.
