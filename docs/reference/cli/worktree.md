# Worktree CLI

`rimz worktree` creates, enters, lands, and removes the isolated Git checkouts that `rimz agents --worktree` launches agents into. Each is an ordinary `git worktree` on its own branch under a directory you configure, marked so RimZ knows it owns it — it never claims a checkout you made yourself. Removal is guarded: `remove` refuses a worktree that is dirty, unlanded, or still in use unless you pass `--force`, and `sweep` only ever removes clean, landed, RimZ-marked worktrees with no live pane inside, so unfinished work is never discarded silently. Why you isolate a layout or team this way is the [worktrees guide](../../guide/worktrees.md). For durable named lanes without a Git checkout, use [`rimz channel`](./channel.md).

```sh
rimz worktree new cli-docs --base head                  # branch cli-docs from HEAD
rimz worktree new experiment --base fresh --branch spike/experiment
rimz worktree new --from-pr 42                           # check out the PR head branch as pr-42
rimz worktree list --json
rimz worktree cd cli-docs                                # open a shell in the tree
rimz worktree merge cli-docs                             # rebase, then fast-forward main
rimz worktree sweep --dry-run                            # preview safe reclamation
rimz worktree sweep                                      # remove every safe candidate
rimz worktree remove cli-docs                            # refuses if dirty or not landed
rimz worktree remove experiment --force                  # remove anyway
```

`new` creates a marked worktree under the configured [`[agents.worktree] dir`](../../guide/configuration.md#worktrees). `--base head` branches from `HEAD`, `--base fresh` from the configured fresh base, and any other value is a Git ref. `--from-pr <number|url>` resolves the pull request and names the worktree `pr-<N>` by default (GitHub/Gitea/Forgejo use `refs/pull/<N>/head`, GitLab `refs/merge-requests/<N>/head`); a URL must match `origin`'s host and repository. With an authenticated `gh` or `tea`, same-repository heads track `origin` and fork heads push to the fork. Without a supported forge CLI, RimZ checks out the exact PR head on a review-only local branch and leaves pushes unconfigured. `--branch <name>` selects review-only behavior explicitly. RimZ stops with recovery guidance when a resolved head branch conflicts with a local branch.

`list` reads only and shows RimZ-owned worktrees as the channels they are: name, display branch, the `@kind` handles working there, a dirty marker, the landed signal, and the path.

`cd <name>` resolves only a RimZ-owned worktree and opens your user shell rooted there (replacing the `rimz` process on Unix). A child process cannot change its parent shell's directory, so exiting that shell returns you to the directory from which you invoked RimZ.

`merge <name>` accepts only the linear, clean path to `main`: the main checkout must have `main` checked out with no local changes, the named worktree must be clean and unused, its branch is rebased onto `main`, and the main checkout advances with a fast-forward-only merge. A conflicting rebase is aborted and leaves `main` untouched; any failed precondition prints an error instead of creating a merge commit or forcing through local changes. The merged worktree remains available for inspection until `sweep`, `remove`, or `rimz gc` reclaims it.

`sweep` is the worktree-only garbage collector. It removes all clean, landed worktrees that no live pane or agent occupies, keeps each unsafe tree with its reason, and reports removal or archive failures. `--dry-run` prints the same decision without deleting anything. [`rimz gc`](./maintenance.md#update-reload-reset-gc-and-uninstall) applies this same worktree policy alongside the other runtime and store maintenance areas.

`remove` refuses a dirty worktree, one whose content is not proven landed on its base, and one an agent or an open pane is still working in, naming what holds it. `--force` removes anyway, printing a warning first when the tree was in use. A stale session record from a crashed agent does not block the removal that retires it. This is the reverse of a `--worktree` launch: it deletes the checkout and prunes the branch registration after the safety checks pass.

RimZ marks only worktrees it creates, so it manages agent workspaces without claiming arbitrary checkouts. The marker, `.worktreeinclude` seeding, `.worktreelink` symlinks, and the shared `sweep` / `rimz gc` policy are in [worktrees.md](../../internals/harness/worktrees.md).
