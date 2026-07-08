# Worktree CLI

`rimz worktree` creates, lists, and removes the isolated git checkouts that `rimz agents --worktree` launches agents into. For durable named lanes without a git checkout, use [`rimz channel`](./channel.md).

```sh
rimz worktree new cli-docs --base head                  # branch cli-docs from HEAD
rimz worktree new experiment --base fresh --branch spike/experiment
rimz worktree new --from-pr 42                           # branch pr-42 from the PR head
rimz worktree list --json
rimz worktree remove cli-docs                            # refuses if dirty or not landed
rimz worktree remove experiment --force                  # remove anyway
```

`new` creates a marked worktree under the configured [`[agents.worktree] dir`](../../guide/configuration.md#worktrees). `--base head` branches from `HEAD`, `--base fresh` from the configured fresh base, and any other value is a git ref. `--from-pr <number|url>` fetches the pull request head through `origin` and creates a `pr-<N>` branch unless `--branch` names it (GitHub/Gitea/Forgejo use `refs/pull/<N>/head`, GitLab `refs/merge-requests/<N>/head`).

`list` shows Rimz-owned worktrees as the channels they are: name, display branch, the `@kind` handles working there, a dirty marker, the landed signal, and the path. `remove` refuses a dirty worktree or one whose content is not proven landed on its base; `--force` removes anyway.

Rimz marks only worktrees it creates, so it manages agent workspaces without claiming arbitrary checkouts. The marker, `.worktreeinclude` seeding, `.worktreelink` symlinks, and the `rimz gc` sweep are in [worktrees.md](../../internals/harness/worktrees.md).
