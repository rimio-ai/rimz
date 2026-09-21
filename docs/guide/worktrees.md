# Worktrees

Agents working in parallel need isolation, and Git already ships the primitive: a worktree (`git worktree`) is a second checkout of your repository, in its own directory, on its own branch, sharing the same history. RimZ puts that primitive behind one flag. Add `-w` to any launch and the whole layout (agents, your editor, a shell) opens inside a fresh tree, seeded and ready to work.

```sh
rimz agents claude,codex -w feat-a    # a pair, isolated on their own branch
rimz teams forge -w feat-b            # a whole team, its own worktree
rimz agents codex --from-pr 42        # a tree checked out from a pull request
```

## Why a tree per task

Your repository normally has one working directory with one branch checked out, and every agent you start shares it. Two agents in one checkout overwrite each other's edits, trip each other's builds, and braid two tasks into one diff, with your own uncommitted work sitting in the blast radius.

A worktree gives each line of work its own directory and branch, backed by the same repository. Edits in one tree stay invisible to its siblings until you merge, and you keep as many trees side by side as you have tasks. Point one agent at a bug fix and another at a refactor and let both run. Agents that should collaborate, like a [team](./teams.md), share one tree; nothing touches your main checkout.

Because each tree is a real branch, the natural unit of work is one worktree to one pull request. Launch a team into a tree, let it carry the change from plan to reviewed diff, open a PR from the branch, and reclaim the tree once the PR merges.

## What RimZ adds to `git worktree`

`git worktree` hands you the tree and nothing else. Around it sits a lifecycle you run once per task, several times a day once agents do the work: create the tree, copy in the untracked files it needs (`.env`, local config), point your editor and a shell at the new directory, and tear tree and branch down once the work merges.

Claude Code solves part of this. `claude --worktree` opens the session in a fresh tree, minimal and genuinely useful, but Claude's alone. RimZ gives every agent it drives the same flag and runs the whole lifecycle behind it:

- **The whole layout lands in the tree.** `vim,codex+term` roots your editor, the agent, and a shell in one checkout, and a [team](./teams.md) isolates as a unit. Everyone in the tree works on the same files, so you edit what the agent edits and run its build in the shared shell.
- **The tree opens ready to run.** Two committed manifests seed every new tree with the local files and shared directories a fresh checkout does not carry ([seed the tree](#seed-the-tree)).
- **The tree is addressable.** Its name is also a [channel](./messaging.md#channels) name, so `@coder#feat-a` reaches the coder in that tree, and the sidebar groups the tree's panes as one block.
- **Cleanup proves the work landed.** RimZ reclaims a tree only once its content is verifiably on its base branch, and only a tree it created itself; worktrees from your own workflow keep running beside RimZ's, untouched ([cleanup](#cleanup-once-work-lands)).

The wrapper stays thin: every step is plain Git or a file copy you could run by hand ([what `-w` does on your machine](#what--w-does-on-your-machine)).

## Open a worktree

`-w` (`--worktree`) puts the whole layout in a fresh RimZ-owned worktree. Pass a name to create or reuse a specific one, or a bare `-w` for a generated name like `brisk-harbor`. The layout grammar (`,` `+` `/`) is the same one [Agents](./fleet.md#compose-a-layout) covers; `-w` only decides where it lands.

```sh
rimz agents claude,codex -w feat-a            # two agents in one isolated tree
rimz agents planner,coder+reviewer -w feat-b  # a whole layout, its own branch
rimz agents claude -w                         # bare -w: a generated name
rimz agents codex -w feat/login               # branch feat/login, tree feat-login
```

Give the name without the channel's `#`: `-w feat-a` creates the tree `feat-a` and names its channel `#feat-a`. A `#` on the name never helps. Unquoted, your shell eats `#feat-a` as a comment, so RimZ sees a bare `-w` and generates a name; quoted, `-w '#feat-a'` is rejected as an invalid worktree name. If a launch lands in a generated tree you did not ask for, look for a `#` on the name you typed.

A `/` in the name goes into the branch and becomes `-` everywhere else: `-w feat/login` puts branch `feat/login` in a tree called `feat-login`, on channel `#feat-login`.

`rimz worktree new <name>` creates an empty tree, launching nothing into it, for work of your own or a tree you will fill with agents later. [`rimz worktree`](../reference/cli/worktree.md) is the whole verb surface: `new`, `list`, `cd`, `merge`, `remove`, and `sweep`.

Already made the tree with `git worktree add`? If it is in the configured worktree directory and belongs to this repository, `rimz agents <spec> -w <name>` and `rimz teams <team> -w <name>` ask whether to enter it. Answer `y` to launch there; Enter or `n` prints `Launch aborted; nothing changed.` RimZ keeps your files and branch as they are, does not mark or seed the tree, and leaves cleanup to you. Confirmation needs a terminal; an unattended launch refuses instead of guessing.

### Start from a pull request

`--from-pr <number|url>` fetches a pull request's head over your `origin` credentials and lands the layout in a `pr-<N>` worktree. Reach for it to address review comments or chase a failing check: the agent works the PR's own branch, and on a forge RimZ can ask, its fixes push straight back to the PR.

```sh
rimz agents claude --from-pr 42               # review PR 42 in its own tree
rimz agents codex --from-pr 42 -w review-42   # name the tree yourself
rimz agents resume --from-pr 42               # come back to that PR's tree later
```

The fetch works on any host. RimZ knows each one's PR ref, `refs/pull/<N>/head` on GitHub, Gitea, and Forgejo and `refs/merge-requests/<N>/head` on GitLab, and a PR URL must name the same host and repository as your `origin`.

Whether the tree can push back is the part that varies, because it depends on an authenticated forge CLI that can say who owns the PR's head branch. RimZ asks `gh` when `origin` is on `github.com`, and `tea` when the `origin` hostname contains `gitea`, `forgejo`, or `codeberg`. The CLI reports whether the head branch lives in the origin repository or in a fork, and RimZ configures pushes to whichever it is, so your fixes go straight back to the PR.

Everywhere else you get a review-only checkout: the exact PR head on a local branch, with no push destination configured, and a `review :` line on the create report saying why. That covers GitLab, a self-hosted forge whose hostname carries none of those names, and a host whose CLI is simply not installed. A CLI that is installed but cannot answer, usually because you are not logged in, refuses the checkout instead of quietly downgrading it; log in, or pass `--branch <name>` to ask for review-only on purpose. The full table of checkout shapes and refusals is in [pull request checkouts](../reference/cli/worktree.md#check-out-a-pull-request).

### Seed the tree

A tracked checkout alone rarely runs. `git worktree add` checks out tracked files at the base ref and nothing else, so the `.env` an agent needs and the fixture cache its tests read never follow it. Two committed, optional files at the repository root tell RimZ what else every new tree carries:

- **`.worktreeinclude`** lists globs for files to copy in: `.env`, local config, credentials the tests need. `*` stays inside a path component and `**` crosses directories, so `.env*` matches the root only and `**/*.key` recurses.
- **`.worktreelink`** lists directories to symlink-share rather than copy: heavy machine-local data whose contents are intentionally branch-independent, such as downloaded model or fixture caches. Sharing them keeps a new tree cheap instead of duplicating gigabytes. Leave build output directories out of it. A shared one is written by every tree at once, so branches overwrite each other's artifacts; share the compiler's work through a content-addressed cache instead, the way RimZ's own contributors use `sccache` and keep `target/` local ([contributor cache setup](../../CONTRIBUTING.md#fast-local-builds)).

Both files take one entry per line and ignore blank lines and `#` comments. Because both are committed, every teammate's worktrees seed the same way, and every create reports what it brought in:

```console
$ rimz worktree new feat-a
created feat-a
  path   : ~/code/query-engine-worktrees/feat-a
  branch : feat-a
  base branch: main
  base   : c175274596fd0148c7ad1b8376d4f69f55160b9a
  seeded : 2 file(s) from .worktreeinclude
  linked : 1 dir(s) from .worktreelink
```

Cover every `.worktreeinclude` path in `.gitignore`. A copied file that Git can see makes the tree permanently dirty, and a dirty tree is one RimZ refuses to reclaim. Linked directories need no such care: RimZ writes each one into the new tree's `.git/info/exclude` as it links it.

Neither file runs a command. Sources are confined to the project root, absolute patterns and patterns reaching out with `..` are skipped, and a pattern that matches nothing prints a warning and is skipped while the launch continues. The exact copy, symlink, and safety rules are in [the worktree internals](../internals/harness/worktrees.md#seeding-worktreeinclude-and-worktreelink).

## What `-w` does on your machine

`-w feat-a` runs four steps, each plain Git or a file operation you could rerun by hand:

1. **Add the tree.** `git worktree add -b feat-a ../<repo>-worktrees/feat-a <base>`. The directory template and the base ref are the two knobs below.
2. **Mark it.** Write `rimz-worktree.json` into the tree's Git admin directory, at `.git/worktrees/feat-a/rimz-worktree.json` under the main repository, recording the name, branch, and the base branch and commit that cleanup later measures against. That file is the whole of RimZ's claim on the tree: the checkout itself stays free of RimZ metadata, and every verb that can delete something reads the marker first and does nothing without it.
3. **Seed it.** Copy the `.worktreeinclude` matches, symlink the `.worktreelink` directories, and register each link in the tree's `.git/info/exclude`.
4. **Open the layout.** Every pane starts with its working directory in the tree, on a channel named after it.

Two per-machine keys under `[agents.worktree]` in `config.toml` tune the first step:

```sh
rimz config set agents.worktree.dir "../{repo}-worktrees"   # where sibling trees land
rimz config set agents.worktree.base fresh                  # branch from origin/HEAD, not local HEAD
```

`base` is `head` (the default), `fresh`, or any Git ref you name. Both keys read the main repository even when you launch from one of its linked worktrees, so `head` means the branch checked out in your main clone, not the one under your cursor, and `dir` puts the new tree beside that clone. `rimz worktree new --base <ref> --branch <name>` overrides the base and the branch name for one tree; the directory always comes from `dir`. Both fields are written up in [configuration → worktrees](./configuration.md#worktrees).

If the repository you are standing in differs from the active room's, RimZ prints both paths and asks before creating anything, and warns that this room will not list, remove, or garbage-collect a tree made under the other root. A non-interactive caller states the choice with `--root <git-root>` instead of answering.

## Cleanup, once work lands

RimZ reclaims a tree only after proving its work landed, so a merged feature cleans itself up and unmerged work is never lost. "Landed" is measured against the base branch the marker recorded, falling back to the trunk once that base has itself merged away, and it recognizes merge, squash, and rebase alike.

What happens when the last agent in a tree exits depends on how it exits, and the three cases differ enough to be worth knowing:

- **The pane closes, or you kill it.** RimZ checks the tree. A clean one whose work has landed goes, branch and all, and says so on stderr: `rimz: removed clean worktree <path>`. A dirty or unproven one raises a `Choose (keep/remove/shell) [keep]:` prompt if you are there to answer it, and is kept if you are not.
- **An unattended run finishes.** Same check, same outcomes. This is the path a [scripted `-p` run](./scripting.md) or a [scheduled loop turn](./loops.md) takes, and it is why a fleet that works while you sleep does not leave trees behind.
- **You quit the agent normally.** Nothing is reclaimed. The pane prints ``rimz: worktree <path> kept; `rimz worktree sweep` reclaims it once its work lands``, then drops to a shell inside the tree so you can commit, push, or look around.

`rimz worktree list` is the status read. Two of its columns decide everything: a tree that is clean under `DIRTY` and `yes` under `MERGED` is one RimZ can reclaim, and anything else is the reason it cannot.

```console
$ rimz worktree list
WORKTREE  BRANCH  AGENTS  DIRTY  MERGED   PATH
auth      auth    @coder  dirty  pending  ~/code/query-engine-worktrees/auth
feat-a    feat-a  -       -      yes      ~/code/query-engine-worktrees/feat-a
```

`rimz worktree merge auth` takes the deliberately narrow landing path: with no agent or pane left in the worktree and both checkouts clean, the branch rebases onto `main` and the main checkout advances by fast-forward only. A conflict aborts the rebase and leaves `main` unchanged rather than creating a merge commit. Your main checkout must have a branch named `main` out, so a repository whose trunk is `master` lands its branches with Git or a pull request instead.

```console
$ rimz worktree merge auth
merged auth into main
  branch : auth
  head   : f0c10e649367adc7a6a82bbbe07549e5ea1f805b
```

Merging leaves the tree in place. `rimz worktree sweep` is the routine that takes it away: it removes every marked tree that is clean, proven landed, and unoccupied, reports what it reclaimed and why it kept the rest, and previews with `--dry-run`. `rimz gc` runs the same sweep alongside the other maintenance areas. A sweep never forces, so a tree you are still working in survives it.

```console
$ rimz worktree sweep
sweep — removed 1 · 203 B · 1 kept
  removed: auth — /home/you/code/query-engine-worktrees/auth
  kept: feat-a — uncommitted changes
```

`rimz worktree remove <name>` reclaims one tree on demand, and refuses twice before it does:

```console
$ rimz worktree remove auth
error: worktree `auth` is in use by @coder; use --force to remove it

$ rimz worktree remove feat-a
removed feat-a
```

The first refusal is for an agent or an open pane still working in the tree, naming what holds it. A stale record left by a crashed agent does not count, so a crash never leaves you unable to reclaim the tree. Once the tree is empty, a dirty or unlanded one refuses again with ``worktree `auth` has local changes or work not proven landed; use --force to remove it``, one message for both conditions; `rimz worktree list` says which.

Removal deletes the branch with the tree when Git considers it merged or RimZ proves its work landed. Otherwise the branch stays and the report adds `branch kept: work not proven merged into its base`, so the commits are still reachable by name. `--force` skips both refusals and deletes the branch regardless, taking any unlanded commits on it with no further warning; when a live agent is in the tree it warns first, then removes anyway.

`rimz worktree cd auth` opens your user shell rooted in that checkout, for the times you want to look before you decide. Exit the shell to return to the directory where you started.

The full landed-content proof is in [the worktree internals](../internals/harness/worktrees.md#the-content-landed-ladder), and every refusal each verb can print is in [the worktree CLI reference](../reference/cli/worktree.md).

## See also

- [Agents](./fleet.md): launch agents by name and compose the layout that lands in the worktree.
- [Teams](./teams.md): a named team is the common unit to isolate on its own branch.
- [Messaging](./messaging.md): a worktree's name is its channel; reach agents there by handle.
- [Worktree CLI reference](../reference/cli/worktree.md): the complete `rimz worktree` and `rimz gc` surface.
- [Worktree internals](../internals/harness/worktrees.md): the ownership marker, file seeding, and the landed-content proof.
