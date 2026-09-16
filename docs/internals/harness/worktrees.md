# RimZ-owned worktrees

> The life of a RimZ-owned Git worktree: the ownership marker, creation and seeding, the proof that a branch's work landed, and every path that removes a tree. The domain code is [`worktree.rs`](../../../crates/rimz/src/worktree.rs) and its four private submodules; [fleet.md](./fleet.md) maps the harness around it. Users read [cli/worktree.md](../../reference/cli/worktree.md) for the commands and the [worktrees guide](../../guide/worktrees.md) for the workflow.

A managed worktree is one Git checkout of the repository on its own branch whose whole life RimZ runs: it creates the tree for a line of work, seeds it with the untracked files an agent needs, and reclaims it once the work has landed. The tree's name is also a [channel](./messaging.md#channels), so `@coder#feat-a` addresses the agent working inside it.

The subsystem stands on its own. `rimz worktree new`, `list`, `merge`, `remove`, `sweep`, and `rimz gc` work whether or not an agent ever launches into a tree, and the launch path calls the same domain entry points when one does. What agents do inside a tree belongs to [fleet.md](./fleet.md).

## The one rule: RimZ touches only what it marked

Every managed operation starts by reading `rimz-worktree.json`. A checkout without that marker belongs to the user: `remove`, `merge`, `sweep`, `gc`, and wrapper cleanup all skip or refuse it, even when its path matches the configured directory template exactly. That one check is what lets RimZ delete directories and branches without endangering a checkout someone made by hand.

The marker lives in the worktree's Git admin directory (`.git/worktrees/<name>/rimz-worktree.json`), so the checkout carries no RimZ metadata and needs no `.gitignore` entry. `WorktreeMarker` holds these fields:

| Field | Meaning |
| --- | --- |
| `version` | Schema version, `MARKER_VERSION` (4). |
| `name` | Worktree directory and channel name, in its dashed spelling. |
| `branch` | The branch checked out in the tree. |
| `base_branch` | The branch the tree was cut from, when one can be named. First choice for the [comparison ref](#choosing-the-comparison-ref). |
| `base_ref` | The full commit id of the base at creation. Last-resort comparison ref. |
| `from_pr` | Pull-request number for a `--from-pr` tree. |
| `repo_root`, `worktree_path` | The repository the tree was created from and where the tree lives. |
| `created_at` | Creation time. It bounds the lane's [attribution lifetime](../agents/attribution.md#selecting-records). |

Creation writes the marker once, and reusing a tree keeps the marker it finds, so `created_at` is the birth of the tree's current incarnation: a tree removed and recreated under the same name gets a fresh one.

`base_branch` and `from_pr` are `#[serde(default)]` options, so markers written before those fields existed still deserialize and their trees still clean up; two tests in [`worktree/tests.rs`](../../../crates/rimz/src/worktree/tests.rs) pin that. A new marker field needs the same treatment.

## Module map

| Path | Owns |
| --- | --- |
| [`worktree.rs`](../../../crates/rimz/src/worktree.rs) | The marker, creation, launch checkout resolution, dirty and landed status, the protection set and removal assessment, merge, sweep, discovery, and the `git` helpers. |
| [`worktree/pr.rs`](../../../crates/rimz/src/worktree/pr.rs) | `--from-pr`: strategy selection, head resolution through the forge CLI, fork push wiring, PR ref fetching. |
| [`worktree/include.rs`](../../../crates/rimz/src/worktree/include.rs) | `.worktreeinclude` copying and its containment rules. |
| [`worktree/link.rs`](../../../crates/rimz/src/worktree/link.rs) | `.worktreelink` directory symlinks. |
| [`worktree/exclude.rs`](../../../crates/rimz/src/worktree/exclude.rs) | `info/exclude` registration for linked directories and team scratch patterns. |
| [`cli/worktree.rs`](../../../crates/rimz/src/cli/worktree.rs) | The `rimz worktree` commands and the hidden `cleanup` helper. |
| [`cli/worktree_protection.rs`](../../../crates/rimz/src/cli/worktree_protection.rs) | Gathering pane and agent facts for each removal caller. |
| [`cli/mod.rs`](../../../crates/rimz/src/cli/mod.rs) | `resolve_launch_checkout` (the unmarked-checkout confirmation) and `confirm_cross_repo_worktree`. |
| [`cli/gc.rs`](../../../crates/rimz/src/cli/gc.rs) | The worktree area of the aggregate `rimz gc` report. |
| [`cli/agents_cmd/exec.rs`](../../../crates/rimz/src/cli/agents_cmd/exec.rs) | The exec wrapper that enters a tree and triggers cleanup when the agent exits. |
| [`cli/agents_cmd/reconcile.rs`](../../../crates/rimz/src/cli/agents_cmd/reconcile.rs) | Cohort relaunch into a tree that already exists. |

The domain module runs Git through `crate::proc::git_command` with `LC_ALL=C` and returns typed `WorktreeErr` values. Prompts, confirmations, and reports live in the CLI layer; a `WorktreeErr` message reaches the user as written, so each carries its own fix (`use --force to remove it`, `pass --branch <name> for a review-only checkout`).

## Creating a worktree

`create` and `create_from_pr` walk the same five steps.

1. **Resolve the name.** A requested name is validated per `/`-separated segment (ASCII alphanumerics, `_`, `-`). A `/` names the branch directly while the directory and channel take the dashed spelling: `feat/login` gives branch `feat/login` in directory `feat-login`. `--branch` overrides the derived branch without changing the directory. An omitted name becomes an adjective-noun pair derived from a UUIDv7; each retry mixes the attempt number into that seed and adds it as a suffix, for up to 64 attempts until the directory is unused. A PR tree's omitted name is `pr-<N>`.
2. **Resolve the base.** [`WorktreeConfig`](../../../crates/rimz/src/config/worktree.rs) carries the per-machine `dir` template (default `../{repo}-worktrees`, where `{repo}` is the repository basename and a relative template resolves from the repository root) and `base`. `head` (the default) branches from `HEAD`, `fresh` from `origin/HEAD`, and any other value is a literal ref. The base resolves to a commit for `base_ref`; `base_branch` records the current branch for `head`, the `origin/HEAD` target for `fresh`, and the branch a literal ref names. A PR tree ignores `base` and records the trunk ([from a pull request](#from-a-pull-request)).
3. **Add the tree.** `git worktree add` runs in one of three shapes: `-b <branch> <path> <base>` for a new branch, `--track -b <branch> <path> origin/<branch>` for a same-repository PR head, and `add <path> <branch>` for an existing local branch already fast-forwarded to the PR head.
4. **Write the marker.** A temp-file-plus-rename into the Git admin directory. Ownership begins here, so a failure at this step leaves an ordinary unmanaged Git worktree.
5. **Seed the tree.** `.worktreeinclude` copies, then `.worktreelink` symlinks, both best-effort ([seeding](#seeding-worktreeinclude-and-worktreelink)).

A launch that names an existing marked tree reuses it; `rimz worktree new` refuses with `Exists`. A reused tree is never re-seeded, and `CreatedWorktree` reports zero included and linked counts so the CLI prints no seeding lines. `rimz worktree new` also refuses a name a named channel already holds (`channel::ensure_worktree_name_available`) and, after creation, archives any messages left in that channel with the reason `channel recreated`.

### Which repository a launch uses

A launch creates its tree under the repository the command starts in, and `rimz worktree` commands act on the room's repository. `resolve_launch_checkout` takes its repository from `ResolvedWorkspace::launch_repo_root`: the freshly probed main-repository root of the starting directory, or the room root when the start is outside Git. It returns the cwd every pane in the layout gets and the worktree name that becomes the channel, and it fails with `LaunchWorktreeRequiresRepo` (or `LaunchPrRequiresRepo`) when neither path is a Git repository. `rimz worktree new`, `list`, `merge`, `remove`, and `sweep` use the room's pinned `project_root` instead.

The two roots can differ, and the launch asks before creating anything across them. `confirm_cross_repo_worktree` prints both paths and warns that the room will not list, remove, or garbage-collect a tree created under the other repository. A terminal gets a default-no confirmation; non-terminal stdin refuses with a `--root <current-git-root>` hint. Cohort relaunch reconciliation resolves its tree path from the same launch root.

A launch that names an existing checkout without a marker can still enter it, without adopting it. The domain returns `Unmarked`, and the CLI's `resolve_launch_checkout` hands the name to `resolve_unmanaged_launch_checkout`, which accepts only a linked worktree of the launch repository: same Git common directory, the checkout's own top level, and not the main checkout itself. The user then confirms entry at a terminal (default no); non-terminal stdin refuses. An accepted checkout gets its cwd and channel with no marker and no seeding, so removal and cleanup never touch it. Path checks canonicalize for Git identity but keep the configured path spelling in launch records, reconciliation, and resume filtering. `--from-pr` launches never take this path, because reusing a PR tree requires the marker's `from_pr`.

### From a pull request

`--from-pr <number|url>` produces the same marked tree from a pull-request head. A URL must name the same host and repository as `origin` before any network call, and reusing a named PR tree requires its marker's `from_pr` to equal the requested number.

Head resolution picks one of four strategies:

| Condition | Strategy | Branch | Pushes |
| --- | --- | --- | --- |
| `--branch` given, or the name was branch-style | Review-only | The requested branch, at the exact PR head commit | Not configured |
| `origin` has no supported forge CLI, or `gh`/`tea` is not installed | Review-only | Derived from the worktree name | Not configured; the reason is reported |
| The forge CLI reports a same-repository head | Same-repository | The PR head branch | Upstream is `origin/<branch>`, the PR's own branch |
| The forge CLI reports a fork head | Fork | The head branch, or `<owner>/<branch>` when that name is taken | `branch.<b>.remote` is the fork URL, `branch.<b>.merge` the head ref |

Review-only and fork checkouts fetch the forge's PR ref (`Forge::pr_refspec`) into a per-process temporary ref, `refs/rimz/pr/<N>-<pid>-<nonce>`, resolve its object id, and delete the ref through a `Drop` guard. Shared `FETCH_HEAD` never becomes checkout authority, so concurrent PR checkouts cannot cross-contaminate. A same-repository checkout fetches `refs/heads/<branch>` into `origin/<branch>` instead.

The forge module owns the head query: `ForgeCli::pr_head_args` builds the `gh pr view` or `tea api` call and `decode_pr_head` parses it. The same-repository decision uses the CLI's cross-repository verdict when it has one and otherwise compares the head repository with the `origin` slug, case-insensitively.

A same-repository head adopts an existing local branch only when that branch is not checked out elsewhere and its tip equals or is an ancestor of the remote head, in which case it fast-forwards and gains the upstream. An ahead or diverged branch refuses with the reason.

Network calls are bounded: `PR_HEAD_COMMAND_TIMEOUT` (10 seconds) for the forge CLI query and `PR_FETCH_TIMEOUT` (120 seconds) for a fetch, which runs with `GIT_TERMINAL_PROMPT=0` so a credential prompt fails instead of hanging a launch.

A PR tree's marker records the repository trunk as `base_branch`, so the ordinary landed proof reclaims it once the pull request's content reaches the trunk, whether it merged, squashed, or rebased.

### Seeding: `.worktreeinclude` and `.worktreelink`

`git worktree add` checks out tracked files and nothing else, so the `.env` and local config an agent needs to run never follow it. Two optional files at the repository root fill that gap, and both are read from the main checkout at creation:

- `.worktreeinclude` lists glob patterns, one per line. Each match is copied into the new tree at the same repo-relative path. `*` stays within a path component, `**` crosses directories, a leading `*` matches dotfiles, and a matched directory copies recursively.
- `.worktreelink` lists relative directory paths, one per line. Each is symlinked into the new tree with an absolute link target and registered in the tree's effective `info/exclude` as an anchored `/<path>` pattern. It is for heavy machine-local data whose contents do not depend on the branch, such as model or fixture caches.

Both files skip blank lines and `#` comments and confine every source to the project root. Absolute entries and entries containing `..` are skipped with a warning. Include matches that are themselves symlinks are skipped, and every copied file is re-checked against the canonicalized root, because `glob`'s `**` descends through symlinked directories and a committed symlink could otherwise pull host files into a tree an agent reads. A link source must canonicalize to a real directory inside the root, and an existing destination is never replaced.

Seeding is best-effort. A missing file is a silent no-op; a pattern that matches nothing, a failed copy, or an unlinkable directory warns through `tracing` and is skipped, and the tree and its agents still launch. Neither file can run a command, which is why both stay outside the trust hash ([trust.md](./trust.md)).

Git usually resolves a linked worktree's effective `info/exclude` to the repository's shared `.git/info/exclude`, so a linked directory's pattern also hides that path in the main checkout and in sibling worktrees.

### Team scratch patterns

A team's effective scratch patterns describe the ephemeral records its members keep: `scratch-files` replaces the list (`[]` for none); when unset, a staged team (nonempty `stages` or any role's `owns`) gets `["/blackboard.md", "/*-notes.md"]`, and an unstaged team gets none. `worktree::exclude_team_scratch` registers these gitignore patterns in the checkout's effective `info/exclude` before panes start. Fresh launch, explicit cohort resume, and place-first team restore all call it. Registration appends only missing lines, publishes the file atomically, and warns without blocking the launch on a Git or filesystem failure. It shares the `info/exclude` caveat above: the patterns hide matching untracked names in the main checkout and every sibling worktree, and RimZ never removes the lines, because another team or checkout may still rely on them.

The patterns matter to reclamation. Excluded files do not show in `git status --porcelain`, so a landed tree holding only declared scratch files is clean, and wrapper cleanup and `rimz gc` reclaim it along with those files. That is the purpose of declaring them ephemeral. What the team does with the files, including the `blackboard.md` stage board, is [teams.md](./teams.md).

## Status: dirty and landed

`status(path, marker)` answers the two questions every removal decision needs.

A tree is dirty when `git status --porcelain` prints anything, untracked files included.

A tree is landed when the branch's content already exists on the ref it should return to. Landing is proven, never assumed: `content_landed` answers `Landed`, `Pending`, or `Unknown`, and anything short of `Landed` keeps the tree. `worktree list` shows the verdict in its `MERGED` column (`yes`, `pending`, `?`) and as `landed: true | false | null` in JSON.

### Choosing the comparison ref

`comparison_ref` picks what to prove against, taking the first that applies:

1. The marker's `base_branch`, while it resolves and has not been superseded.
2. The repository trunk (`trunk_ref`): the first of `main`, `master`, or the `origin/HEAD` target that resolves.
3. The marker's `base_ref`, when it still resolves to that exact commit.

A base branch is superseded once it is not an ancestor of the trunk and is itself content-landed on the trunk. This serves stacked work: a feature cut from another feature branch is measured against that branch while it is a live destination, and against the trunk once that branch has merged. A base branch that is an ancestor of the trunk stays the comparison.

No usable comparison ref means `Unknown`.

### The content-landed ladder

`content_landed(cwd, comparison, head)` tries cheap proofs before expensive ones. A failed `rev-list` or `log` call returns `Unknown`, so a broken repository keeps its trees.

| Rung | Check | Verdict |
| --- | --- | --- |
| 1 | `rev-list --count <comparison>..<head>` is zero | Landed: nothing was committed past the comparison. |
| 2 | Both refs have the same tree object | Landed: identical content, whatever the history. |
| 3 | `merge-tree --write-tree <comparison> <head>` produces the comparison's own tree | Landed: the branch adds nothing, even when patch context drifted. |
| 4 | `log --right-only --cherry-pick --no-merges <comparison>...<head>` lists commits | Landed if the head's tree appears in `log <head>..<comparison>`, otherwise Pending. |
| 5 | Rung 4 lists nothing and the head side has no merge commits | Landed. |
| 6 | Rung 4 lists nothing and every head-side merge commit's tree appears in the comparison's last `LANDED_BASE_SCAN_CAP` (500) commits | Landed: a merge-back landing. Otherwise Pending. |

Rung 4 carries the squash case. Patch residue alone does not mean pending: a squash landing followed by more trunk commits leaves residue while the work is in. Finding the branch tip's exact tree in the comparison's exclusive history proves the destination once held the branch's complete final state. That scan is uncapped, because a landing can be arbitrarily far back.

Together the rungs cover rebased, cherry-picked, squash-landed, and merge-back shapes without trusting ancestry alone or any sidebar state.

### What the sidebar asks

The sidebar's git-stats refresh (`sidebar/refresh/git_stats.rs`) uses the same proof with different inputs, so its worktree header can disagree with `worktree list`.

- Its landed marker is measured against the sidebar's own trunk, never the marker's `base_branch`: zero commits ahead is landed, and a clean tree with commits ahead goes through `content_landed`. That trunk tries the per-machine `[sidebar] trunk` setting before `main`, `master`, and the `origin/HEAD` target ([sidebar.md → Worktree groups](../sidebar/sidebar.md#worktree-groups)).
- Its `did_work` marker answers whether the tree has done any work of its own. HEAD equal to the marker's `base_ref` is no work; otherwise `on_trunk_first_parent` checks whether HEAD sits on the trunk's first-parent chain (scan capped at `LANDED_BASE_SCAN_CAP`), where a tree that only tracked the trunk sits and a tree carrying its own commits does not.

The sidebar reads markers through `read_marker_from_checkout_metadata`, which follows the checkout's `.git` file directly and forks no Git process.

## Landing on main

`rimz worktree merge <name>` is the one path that moves a branch onto the trunk, and it is deliberately narrower than the landed proof. `merge_to_main` requires a marked tree whose checked-out branch matches the marker, `main` checked out in the room's main checkout, both checkouts clean, no rebase, merge, cherry-pick, or revert in progress in either, and no other pane or live agent inside the tree (explicit-removal protection facts, below).

It rebases the worktree branch onto `main` with `rebase.updateRefs=false`, aborting a failed rebase so neither checkout is left mid-operation. It then rechecks that the worktree is clean and that `main` has neither moved nor become dirty, and advances `main` with `merge --ff-only` to the rebased head. It never creates a merge commit and never forces. The tree stays marked afterwards, and `sweep` or `gc` reclaims it once the proof passes.

## Removing a worktree

### Protection facts

Git state alone cannot make removal safe: a tree can be clean and landed while someone still has a shell open in it. `ProtectionSet` holds the normalized paths the live room occupies, and a candidate checkout is protected when any of those paths equals it or lies inside it.

The CLI gathers the facts (pane cwds from the mux, agent rows from the store) and `protection_set_from_runtime` folds them:

- A pane contributes its cwd, unless it is sidebar chrome or the caller's own pane.
- A live agent contributes its recorded `worktree_path` and its process cwd.
- An agent of unknown liveness contributes its recorded path under `Occupancy::Unproven` only.
- A dead agent, or any agent bound to the caller's own pane, contributes nothing.

Paths are normalized lexically (`.` and `..` folded) before comparison, so `/repo/../repo-worktrees/demo` matches `/repo-worktrees/demo`.

`Occupancy` and the own-pane exemption are where the callers differ, and `cli/worktree_protection.rs` fixes them per caller:

| Caller | Occupancy | Own pane exempt | Missing facts |
| --- | --- | --- | --- |
| `worktree remove`, `worktree merge` (`for_explicit_removal`) | `ProvenLive` | Yes | Best-effort: an unreadable roster or mux protects nothing. |
| Wrapper cleanup (`for_wrapper_cleanup`) | `Unproven` | Yes | Best-effort. |
| `worktree sweep`, `rimz gc` (`for_automatic_gc`) | `Unproven` | No | The roster is required; failure skips the sweep. |

Unattended reclamation uses `Unproven`, so an agent RimZ cannot prove dead still holds its tree. A person naming one tree has already decided, so `ProvenLive` lets only a running process or an open pane hold it, and a stale session record left by a crash never blocks the command that would retire it.

### The assessment

`ProtectionSet::assess` returns one `RemovalAssessment` in a fixed precedence: `InUse`, then `Dirty`, then `NotLanded`, then `Removable`. A dirty tree someone is standing in reports as in use. Callers never reorder these checks; they only choose what to do with each verdict.

| Verdict | `worktree remove` | Wrapper cleanup | `sweep` and `gc` | Cohort relaunch |
| --- | --- | --- | --- | --- |
| `Removable` | Remove the tree and delete the branch | Remove, note it on stderr | Remove, count the bytes | Offer `remove / fresh / cancel`, default cancel |
| `Dirty` | Refuse; `--force` removes | Prompt `keep / remove / shell` at a terminal, keep otherwise | Keep: "uncommitted changes" | Offer `resume / fresh / cancel`, default resume |
| `NotLanded` | Refuse; `--force` removes | Same as `Dirty` | Keep: "not merged yet" | Same as `Dirty` |
| `InUse` | Refuse, naming the holder; `--force` warns and removes | Skip silently | Keep: "in use" | Not reached (see below) |

The domain owns every refusal: `worktree::remove` returns `WorktreeErr::InUse` or `WorktreeErr::Dirty` (one error for dirty and not landed), and `--force` skips the assessment entirely. `cli/worktree.rs` only names the holder, by matching `InUse` and asking `agents_in_worktree` which agents are bound to the checkout; with none, the holder is "an open pane".

Cohort relaunch assesses against an empty protection set, because it has already established that the cohort's panes are closed and a live cohort is focused instead. The offers, `--fresh`, and what each choice does are [fleet.md § Cohort relaunch reconciliation](./fleet.md#cohort-relaunch-reconciliation); only the `remove` choice enters the removal path below.

### Branch deletion and retirement

Removal is `git worktree remove` followed by branch deletion, both run from the repository root. `remove_marked_worktree` first moves the process out of the checkout when its cwd is inside it.

Branch deletion re-runs the landed proof instead of trusting Git's merge check. It tries `git branch -d`; a branch already gone counts as deleted; a "not merged" refusal escalates to `-D` only when `content_landed` passes against the marker's comparison ref, and otherwise returns `BranchDeletion::KeptUnmerged` so the CLI can say the branch survived. A forced removal skips the proof and deletes with `-D` directly, so an unlanded branch goes with it and no `KeptUnmerged` notice appears. Two paths force: `worktree remove --force` and the `remove` answer to the wrapper's dirty prompt.

After Git removal succeeds, `retire_removal` runs two durable effects: it ends the store sessions bound to that path or branch, and archives the worktree channel's messages. Both run even when the first fails, and both results return in a `#[must_use]` `RemovalRetirement`, because the Git removal is already irreversible and one failure must not hide the other. `worktree remove` and cohort reconciliation fail the command on either error; wrapper cleanup logs both at debug level; `sweep` and `gc` warn on session retirement and report archival failures per tree.

Attribution resolves lane lifetimes from the checkout at read time (`worktree::lane_lifetimes`), so once the directory is gone the tree's sessions drop out of attribution, `teams show` cost, and the sidebar's cohort receipt. The records themselves are untouched.

## Who triggers removal

Four callers reach the removal path.

**`rimz worktree remove <name>`** is the explicit one: it resolves the marked tree under the configured directory, assesses it with explicit-removal facts, and removes it.

**`rimz worktree sweep` and `rimz gc`** call `sweep_owned`, the one sweep implementation. It discovers every marked tree in the room's repository from `git worktree list --porcelain`, assesses each, removes the `Removable` ones, retires their sessions and messages, and runs `git worktree prune` when it removed anything. A per-tree failure is recorded and the sweep continues. `--dry-run` reports the same accounting, including bytes, without acting. Both commands print every marked tree: removed trees with their branch fate, kept trees with their reason, failures with their error. `gc` skips its worktree area with a reason when the directory is not a repository, no store exists, the agent roster cannot be read, or listing fails. Named channel records outlive `gc`; only `rimz channel rm` removes them.

**The exec wrapper** reclaims a tree an agent was launched into with `--worktree-path`. Before launching, `enter_worktree` refuses to start the agent if the marker has vanished since the launch decision, instead of silently running it in the project root. After the agent exits, the shape of the exit decides; [fleet.md § Reclaiming a pane](./fleet.md#reclaiming-a-pane) owns the deliberate-close probe and the end trace.

| Exit shape | Wrapper behavior |
| --- | --- |
| Non-abrupt exit, not a supervised run | Print the relaunch hint and exec a shell in the tree. No cleanup runs; `sweep` or `gc` reclaims the tree later. |
| Non-abrupt exit of a supervised `-p` run | Run cleanup in the foreground; the dirty prompt is allowed. |
| Signal (SIGHUP, SIGTERM) or tab or pane close, while the mux session still accepts closes | Spawn cleanup detached with `--non-interactive`, null stdio, and its own process group, so it outlives the closing pane. |
| Abrupt exit with the mux session gone, wedged, or resurrected | Skip cleanup. Recovery comes from the sidebar producer's live roster. |

Cleanup runs `rimz worktree cleanup <path>` through `reload::current_reexec_target`, the binary now on disk at the wrapper's executable path, which is the replacement when an install swapped the binary while the agent ran. When no re-exec target resolves, or a foreground spawn fails, it runs in-process instead; a failed detached spawn is reported on stderr. The helper reads the marker (no marker is a silent no-op), reads Git status, and, in non-interactive mode, waits `CLEANUP_SIGNAL_ROSTER_GRACE` (300 ms) so the store roster settles before it gathers protection facts. Those facts, session retirement, and message archival use the room named by `--root`, else the verified room pin in the environment, else the marker's `repo_root`; Git removal always runs against the marker's `repo_root`.

**Cohort relaunch** handles a team or multi-agent layout launched with `-w <name>` into a tree that already exists, as described under [the assessment](#the-assessment).

## Invariants worth preserving

- Read the marker before acting. An unmarked checkout is the user's.
- Keep RimZ metadata out of the working tree; the marker belongs in the Git admin directory.
- Prove a landing. Ancestry counts, branch names, and sidebar state are hints, and every uncertain answer keeps the tree.
- Keep refusals in the domain. A command may name the holder; it may not decide removal safety itself.
- Keep seeding best-effort and non-executing. A launch never fails because a glob missed, and neither seed file may gain the power to run a command without entering the trust hash.
- Add marker fields as `#[serde(default)]` options, so an older tree still cleans up after an upgrade.
- Keep the sidebar's marker read on `read_marker_from_checkout_metadata`, off `git rev-parse`.
