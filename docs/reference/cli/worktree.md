# Worktree CLI

`rimz worktree` creates, lists, and removes the isolated Git checkouts that `rimz agents --worktree` launches agents into. Each is an ordinary `git worktree` on its own branch under a directory you configure, marked so RimZ knows it owns it — it never claims a checkout you made yourself. Removal is guarded: `remove` refuses a dirty or unlanded worktree unless you pass `--force`, and [`rimz gc`](./maintenance.md#reload-reset-gc-and-uninstall) only ever sweeps clean, landed, RimZ-marked worktrees with no live pane inside, so unfinished work is never discarded silently. Why you isolate a layout or team this way is the [worktrees guide](../../guide/worktrees.md). For durable named lanes without a Git checkout, use [`rimz channel`](./channel.md).

```sh
rimz worktree new cli-docs --base head                  # branch cli-docs from HEAD
rimz worktree new experiment --base fresh --branch spike/experiment
rimz worktree new --from-pr 42                           # check out the PR head branch as pr-42
rimz worktree list --json
rimz worktree remove cli-docs                            # refuses if dirty or not landed
rimz worktree remove experiment --force                  # remove anyway
```

`new` creates a marked worktree under the configured [`[agents.worktree] dir`](../../guide/configuration.md#worktrees). `--base head` branches from `HEAD`, `--base fresh` from the configured fresh base, and any other value is a Git ref. `--from-pr <number|url>` resolves the pull request's head branch, checks it out with upstream tracking, and names the worktree `pr-<N>` by default (GitHub/Gitea/Forgejo use `refs/pull/<N>/head`, GitLab `refs/merge-requests/<N>/head`). `--branch <name>` opts out of head-branch resolution and creates a review-only local branch at the fetched pull-request head. Rimz stops with recovery guidance when the head branch is unavailable or conflicts with a local branch.

`list` reads only and shows RimZ-owned worktrees as the channels they are: name, display branch, the `@kind` handles working there, a dirty marker, the landed signal, and the path.

`remove` refuses a dirty worktree or one whose content is not proven landed on its base; `--force` removes anyway. This is the reverse of a `--worktree` launch: it deletes the checkout and prunes the branch registration after the safety checks pass.

RimZ marks only worktrees it creates, so it manages agent workspaces without claiming arbitrary checkouts. The marker, `.worktreeinclude` seeding, `.worktreelink` symlinks, and the `rimz gc` sweep are in [worktrees.md](../../internals/harness/worktrees.md).
