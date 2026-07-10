# Worktrees

Agents working in parallel need isolation, and Git already ships the primitive for it: a worktree (`git worktree`) is a second checkout of your repository, in its own directory, on its own branch, sharing the same history. RimZ turns that primitive into a one-flag habit: add `-w` to any launch and the whole layout — agents, your editor, a shell — opens inside a fresh tree, seeded and ready to work.

```sh
rimz agents claude,codex -w feat-a    # a pair, isolated on their own branch
rimz agents forge -w feat-b           # a whole team, its own worktree
rimz agents codex --from-pr 42        # a tree checked out from a pull request
```

## Why worktrees

Your repository normally has one working directory with one branch checked out, and every agent you start shares it. Two agents in one checkout overwrite each other's edits, trip each other's builds, and braid two tasks into one diff — with your own uncommitted work sitting in the blast radius.

A worktree gives each line of work its own directory and branch, backed by the same repository: edits in one tree stay invisible to its siblings until you merge, builds and test runs stop colliding, and you keep as many trees side by side as you have tasks. Point one agent at a bug fix and another at a refactor and let both run. Agents that should collaborate, like a [team](./teams.md), share one tree; nothing touches your main checkout.

## What RimZ adds

`git worktree` hands you the tree and nothing else. Around it sits a repetitive lifecycle: create the tree, copy in the untracked files it needs (`.env`, local config), point your editor and a shell at the new directory, and tear tree and branch down once the work merges — once per task, several times a day when agents do the work.

Claude Code ships a taste of the fix: `claude --worktree` opens the session in a fresh tree, minimal and genuinely useful, but Claude's alone. RimZ gives every agent it drives the same flag and runs the whole lifecycle behind it:

- **The whole layout lands in the tree.** `vim,codex+term` roots your editor, the agent, and a shell in one checkout, and a [team](./teams.md) isolates as a unit — everyone in the tree works on the same files.
- **The tree opens ready to run.** Two committed manifests seed every new tree with the untracked files and shared build directories it needs ([seed the tree](#seed-the-tree)).
- **The tree is addressable.** Its name names a [channel](./messaging.md#channels), so `@coder#feat-a` reaches the coder in that tree, and the sidebar groups the tree's panes as one block.
- **Cleanup proves the work landed.** A tree is reclaimed only once its content is verifiably on its base branch; dirty or pending work is kept ([cleanup](#cleanup-once-work-lands)).

The wrapper stays thin. Each step is plain Git plus a file copy, inspectable and tunable ([under the hood](#under-the-hood)), and RimZ manages only trees it created: worktrees from your own workflow keep running beside the managed ones, untouched.

## Open a worktree

`-w` (`--worktree`) puts the whole layout in a fresh RimZ-owned worktree. Pass a name to create or reuse a specific one, or a bare `-w` for a generated two-word name. The layout grammar (`,` `+` `/`) is the same one [Agents](./agents.md#compose-a-layout) covers; `-w` only decides where it lands.

```sh
rimz agents claude,codex -w feat-a            # two agents in one isolated tree
rimz agents planner,coder+reviewer -w feat-b  # a whole layout, its own branch
rimz agents claude -w                         # bare -w: a generated name
rimz agents codex -w feat/login               # branch feat/login, tree feat-login
```

A branch-style name with a `/` names the branch directly and maps to `-` for the tree directory and channel. By default the branch is cut from your local `HEAD`, so the tree starts from the code you are looking at; `base = "fresh"` branches from the remote's default instead ([under the hood](#under-the-hood)).

`rimz worktree new <name>` creates the same tree without launching anything into it — for work of your own, or a tree you will fill with agents later.

### From a pull request

`--from-pr <number|url>` fetches a pull request's head over your `origin` credentials and lands the layout in a `pr-<N>` worktree on the pull request's own head branch. Same-repository branches track `origin`; fork branches pull from and push to the fork, so a plain `git push` updates the pull request while the worktree, channel, and tab keep the `pr-<N>` name.

```sh
rimz agents claude --from-pr 42               # review PR 42 in its own tree
rimz agents codex --from-pr 42 -w review-42   # name the tree yourself
rimz agents resume --from-pr 42               # resume that PR's local lane later
```

It resolves the host's PR ref for you: GitHub, Gitea, and Forgejo use `refs/pull/<N>/head`, GitLab uses `refs/merge-requests/<N>/head`.

### Seed the tree

A tracked checkout alone rarely runs. Two committed, optional files at the repository root tell RimZ what else every new tree carries, so an agent can start working immediately:

- **`.worktreeinclude`** lists globs for untracked files to copy in: `.env`, local config, credentials the tests need. One pattern per line.
- **`.worktreelink`** lists directories to symlink-share rather than copy: the heavy machine-local ones like `node_modules`, `target`, `.venv`. One path per line. Sharing them keeps a new tree cheap instead of duplicating gigabytes.

Because both files are committed, every teammate's worktrees seed the same way, and every create reports what it brought in:

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

Neither file runs a command; sources are confined to the project root, and a pattern that matches nothing skips with a warning while the launch continues. The exact copy, symlink, and safety rules are in [the worktree internals](../internals/harness/worktrees.md#seeded-files-and-linked-directories).

## Common workflows

**One tree per parallel line of work.** Give each independent task its own `-w` name and the tasks never step on each other; the room groups panes by the tree they live in, so your main checkout plus two feature trees read as three groups in one room. Agents that should collaborate belong in one shared tree together.

```sh
rimz agents forge -w auth       # the forge team on the auth feature
rimz agents forge -w ingest     # a second forge team, fully isolated
```

**A branch per worktree, a PR at the end.** Each tree is a real branch, so the natural unit of work is one worktree to one pull request. Launch a [team](./teams.md) into a tree, let it carry the change from plan to reviewed diff, then open a PR from the branch. Once the PR merges, [cleanup](#cleanup-once-work-lands) proves the content landed and reclaims the tree.

**Start from a PR, push back to it.** The same loop in reverse: `--from-pr` drops an agent onto an existing pull request's branch to address review comments or chase a failing check, and the fixes push straight back to the PR.

**You, your editor, and the agents in one tree.** A layout cell can be `term` for a shell or the name of your editor, so `rimz agents 'vim,claude+term' -w feat-x` opens your editor, an agent, and a shell all rooted in the same fresh tree. You edit the same files the agent does, run its build in the shared shell, and review as you go.

## Cleanup, once work lands

RimZ reclaims a tree only after proving its work landed, so a merged feature cleans itself up and unmerged work is never lost. When a worktree's agents finish and their panes close while the room stays live, RimZ checks the tree: a clean one whose content has reached its base branch is removed along with its branch; a dirty, pending, or unproven one is kept, behind a `keep / remove / shell` prompt when you are watching. "Landed" is measured against the trunk and recognizes merge, squash, and rebase alike.

`rimz worktree list` shows every RimZ-owned tree with the agents inside it, its dirty state, and the landed verdict; `remove` applies the same proof on demand:

```console
$ rimz worktree list
WORKTREE  BRANCH  AGENTS   DIRTY  MERGED  PATH
auth      auth    @coder   dirty  pending ~/code/query-engine-worktrees/auth
feat-a    feat-a  -        -      yes     ~/code/query-engine-worktrees/feat-a

$ rimz worktree remove auth
Error: worktree `auth` has local changes or work not proven landed; use --force to remove it

$ rimz worktree remove feat-a
removed feat-a
```

`--force` overrides the refusal when you have decided the work is disposable. `rimz gc` sweeps what automatic cleanup could not reach — trees left by a crash, or ones that became safe only after a later merge or fetch: it removes every clean, landed tree that no live pane or agent occupies, reports the disk it reclaimed, and previews with `--dry-run`. The full landed-content proof is in [the worktree internals](../internals/harness/worktrees.md#cleanup).

## Under the hood

`-w feat-a` runs four steps, each plain Git or a file operation you could rerun by hand:

1. **Add the tree.** `git worktree add ../<repo>-worktrees/feat-a -b feat-a <base>` — the directory template and the base ref are the two knobs below.
2. **Seed it.** Copy the `.worktreeinclude` matches, symlink the `.worktreelink` directories, and register each link in the tree's `git info/exclude`.
3. **Mark it.** Write `rimz-worktree.json` into the worktree's Git admin directory, recording the name, branch, and the base branch and commit that cleanup later measures against. The checkout itself stays free of RimZ metadata, and the marker is the ownership boundary: cleanup, `remove`, and `gc` act only on marked trees, so a checkout you made by hand is never touched.
4. **Open the layout.** Every pane starts with its working directory in the tree, on a channel named after it.

Two per-machine keys under `[agents.worktree]` in `agents.toml` tune the first step:

```sh
rimz config set agents.worktree.dir "../{repo}-worktrees"   # where sibling trees land
rimz config set agents.worktree.base fresh                  # branch from origin/HEAD, not local HEAD
```

`dir` is a directory template: a relative path resolves from the repository root, and `{repo}` expands to its basename. `base` is `head` (branch from local `HEAD`, the default), `fresh` (branch from `origin/HEAD`), or any Git ref; `rimz worktree new --base <ref> --branch <name>` overrides both for a single tree. The full field reference is [configuration → worktrees](./configuration.md#worktrees), and the complete mechanics live in [the worktree internals](../internals/harness/worktrees.md).

## See also

- [Agents](./agents.md) — launch agents by name and compose the layout that lands in the worktree.
- [Teams](./teams.md) — a named team is the common unit to isolate on its own branch.
- [Messaging](./messaging.md) — a worktree's name is its channel; reach agents there by handle.
- [Worktree CLI reference](../reference/cli/worktree.md) — the complete `rimz worktree` and `rimz gc` surface.
- [Worktree internals](../internals/harness/worktrees.md) — the ownership marker, file seeding, and the landed-content proof.
