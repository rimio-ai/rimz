# RimZ-owned worktrees

> The life of a RimZ-owned Git worktree: the ownership marker, creation and seeding, the proof that a branch's work landed, and every path that removes a tree. The domain code is [`worktree.rs`](../../../crates/rimz/src/worktree.rs) and its six private submodules. [fleet.md](./fleet.md) maps the harness around it. Users read [cli/worktree.md](../../reference/cli/worktree.md) for the commands and the [worktrees guide](../../guide/worktrees.md) for the workflow.

A managed worktree is one Git checkout of the repository, on its own branch, whose whole life RimZ runs. RimZ creates the tree for a line of work, seeds it with the untracked files an agent needs, and reclaims it once the work has landed. The tree's name is also a [channel](./messaging.md#channels), so `@coder#feat-a` addresses the agent working inside it.

The subsystem stands on its own. `rimz worktree new`, `list`, `merge`, `remove`, `sweep`, and `rimz gc` work whether or not an agent ever launches into a tree, and an agent launch calls the same domain entry points when it does. What agents do inside a tree belongs to [fleet.md](./fleet.md).

## The one rule: RimZ touches only what it marked

Every managed operation starts by reading `rimz-worktree.json`. A checkout without that marker belongs to the user: `remove`, `merge`, `sweep`, `gc`, and wrapper cleanup all skip or refuse it, even when its path matches the configured directory template exactly. That one check is what lets RimZ delete directories and branches without endangering a checkout someone made by hand.

The marker lives in the worktree's Git admin directory (`.git/worktrees/<name>/rimz-worktree.json`), so the checkout carries no RimZ metadata and needs no `.gitignore` entry. `crates/rimz/src/worktree.rs::WorktreeMarker` holds these fields:

| Field | Meaning |
| --- | --- |
| `version` | Schema version, `MARKER_VERSION` (4). |
| `name` | Worktree directory and channel name, in its dashed spelling. |
| `branch` | The branch checked out in the tree. |
| `base_branch` | The branch the tree was cut from, when one can be named. First choice for the [comparison ref](#choosing-the-comparison-ref). |
| `base_ref` | The full commit id of the base at creation. Last-resort comparison ref. |
| `from_pr` | Pull-request number for a `--from-pr` tree. |
| `repo_root`, `worktree_path` | The repository the tree was created from, and where the tree lives. |
| `created_at` | Creation time. It bounds the lane's [attribution lifetime](../agents/attribution.md#selecting-records). |

Creation writes the marker through `write_marker`, a temp file renamed into place. One later write exists: reusing a same-repository PR head's tree fills a missing `from_pr` through the same writer and keeps every other field, `version` and `created_at` included. So `created_at` is the birth of the tree's current incarnation, and a tree removed and recreated under the same name gets a fresh one. The filled PR number makes the tree discoverable by PR resume and by the sidebar's PR matching.

`base_branch` and `from_pr` are `#[serde(default)]` options, so a marker that lacks them still deserializes and its tree still cleans up. Two tests in [`worktree/tests.rs`](../../../crates/rimz/src/worktree/tests.rs) pin that. A new marker field needs the same treatment.

### Two spellings of a tree's path

A tree's path has two spellings, and code that matches an agent or a cohort to a tree must know which one it holds. The marker's `worktree_path` is the configured directory joined verbatim. Discovery (`discover_owned`) keeps the path `git worktree list --porcelain` reports, which is the resolved path when the `[agents.worktree] dir` template passes through a symlink.

| Spelling | Carried by |
| --- | --- |
| Marker `worktree_path` | The launch cwd (`LaunchCheckout::cwd`), so every agent's recorded worktree path, launch records, cohort reconciliation, `rimz worktree remove` and `merge` (which build the path from the template) |
| Git's reported path | `worktree list`, `sweep`, `gc`, lane resume (`crates/rimz/src/cli/agents_cmd/resume.rs::local_worktrees`) |

Path matches are lexical, after `normalize_path_lexical`, so under a symlinked template the two spellings never compare equal. Only the Git identity check for an [unmarked checkout](#which-repository-a-launch-uses) canonicalizes.

## Module map

| Path | Owns |
| --- | --- |
| [`worktree.rs`](../../../crates/rimz/src/worktree.rs) | The marker, creation, launch checkout resolution, dirty and landed status, the protection set and removal assessment, landing, sweep, discovery, and the `git` helpers. |
| [`worktree/landed_memo.rs`](../../../crates/rimz/src/worktree/landed_memo.rs) | Shared commit-pair proofs and their best-effort cache file. |
| [`worktree/pr.rs`](../../../crates/rimz/src/worktree/pr.rs) | `--from-pr`: strategy selection, head resolution through the forge CLI, fork push wiring, PR ref fetching, tip divergence. |
| [`worktree/include.rs`](../../../crates/rimz/src/worktree/include.rs) | `.worktreeinclude` copying and its containment rules. |
| [`worktree/link.rs`](../../../crates/rimz/src/worktree/link.rs) | `.worktreelink` directory symlinks. |
| [`worktree/hooks.rs`](../../../crates/rimz/src/worktree/hooks.rs) | Machine-local lifecycle command execution and bounded failure tails. |
| [`worktree/exclude.rs`](../../../crates/rimz/src/worktree/exclude.rs) | `info/exclude` registration for linked directories and team scratch patterns. |
| [`cli/worktree.rs`](../../../crates/rimz/src/cli/worktree.rs) | The `rimz worktree` commands and the hidden `cleanup` helper. |
| [`cli/worktree_protection.rs`](../../../crates/rimz/src/cli/worktree_protection.rs) | Gathering pane, agent, loop-task, and team-board facts for each removal caller. |
| [`cli/mod.rs`](../../../crates/rimz/src/cli/mod.rs) | The CLI's `resolve_launch_checkout` and `settle_launch_checkout` (unmarked-checkout confirmation, PR retry, the `worktree.created` signal), `resolve_pr_branch_choice`, and `confirm_cross_repo_worktree`. |
| [`cli/gc.rs`](../../../crates/rimz/src/cli/gc.rs) | The worktree area of the aggregate `rimz gc` report. |
| [`cli/agents_cmd/exec.rs`](../../../crates/rimz/src/cli/agents_cmd/exec.rs) | The exec wrapper that enters a tree and triggers cleanup when the agent exits. |
| [`launcher/reconcile.rs`](../../../crates/rimz/src/launcher/reconcile.rs) | Cohort relaunch into a tree that already exists. |

The domain module runs Git through `crate::proc::git_command` with `LC_ALL=C` and returns typed `WorktreeErr` values. Prompts, confirmations, and reports live in the CLI layer. A `WorktreeErr` message reaches the user as written, so each refusal carries its fix in its own text (`use --force to remove it`, `pass --branch <name> for a review-only checkout`).

## Creating a worktree

A fresh tree from `create` or `create_from_pr` goes through six steps. Reuse of an existing marked tree returns before step 2 ([reuse](#reusing-a-tree)).

1. Resolve the name. `parse_requested_name` validates each `/`-separated segment (ASCII alphanumerics, `_`, `-`). A `/` names the branch directly while the directory and channel take the dashed spelling: `feat/login` gives branch `feat/login` in directory `feat-login`. `--branch` overrides the derived branch without changing the directory. An omitted name becomes an adjective-noun pair seeded from a UUIDv7 (`available_auto_name`). Each retry mixes the attempt number into the seed and appends it as a suffix, for up to 64 attempts, until the directory is unused. A PR tree's omitted name is `pr-<N>`.
2. Resolve the base ([choosing the base](#choosing-the-base)). A PR tree skips this policy and records the trunk ([from a pull request](#from-a-pull-request)).
3. Add the tree. `add_worktree` runs `git worktree add` in one of three shapes: `--no-track -b <branch> <path> <base>` for a new branch, `--track -b <branch> <path> origin/<branch>` for a same-repository PR head, and `add <path> <branch>` for an existing local branch.
4. Write the marker. Ownership begins here, so a failure at this step leaves an ordinary unmanaged Git worktree.
5. Seed the tree: `.worktreeinclude` copies, then `.worktreelink` symlinks, both best-effort ([seeding](#seeding-worktreeinclude-and-worktreelink)).
6. Run the `created` hook. `finish_worktree` waits for the configured command, and a failure rolls the tree back ([lifecycle hooks](#lifecycle-hooks)).

Both creation entry points, and both PR strategies that create a tree, converge on `add_worktree` and `finish_worktree`.

### Choosing the base

[`WorktreeConfig`](../../../crates/rimz/src/config/worktree.rs) carries the per-machine `dir` template and an optional `base`. The `dir` default is `../{repo}-worktrees`, where `{repo}` is the repository basename and a relative template resolves from the repository root. `rimz worktree new --base` overrides the configured `base`. When neither is set, the caller's `crates/rimz/src/worktree.rs::LaunchBase` picks the default:

| `LaunchBase` | Chosen by | Unset `base` means |
| --- | --- | --- |
| `Configured` | `rimz worktree new`, and agent or supervised launches that name no team | `head` |
| `Team` | Agent and supervised launches that name a team | `fresh` |

Each base value resolves to a commit for `base_ref` and records a `base_branch`:

| Base | Cuts from | `base_branch` records |
| --- | --- | --- |
| `head` | Local `HEAD` | The current branch (none on a detached `HEAD`) |
| `fresh` | The remote default branch, `origin/HEAD` | The `origin/HEAD` target, such as `origin/main` |
| Any other value | That literal ref | The local branch the ref names, otherwise the value as written |

Only a `Team` tree with a `fresh` base, set or defaulted, fetches before cutting (`fetch_team_trunk`). A `Configured` tree with `fresh` cuts from the local `origin/HEAD` as last fetched. The fetch runs these steps:

1. Read the cached `refs/remotes/origin/HEAD` target. When it is absent, discover the default branch with `git ls-remote --symref origin HEAD`.
2. Fetch `+refs/heads/<default>:refs/remotes/origin/<default>` with `--no-tags --no-recurse-submodules`. The fully qualified refspec avoids collisions with local branch names, while markers and warnings keep the short `origin/<default>`.
3. When step 1 discovered the branch, point `refs/remotes/origin/HEAD` at it. Unlike `git remote set-head --auto`, this works before the remote-tracking ref exists.

Discovery and fetch each run through `git_network_output`: `proc::run_bounded_output` with `TRUNK_FETCH_TIMEOUT` (60 seconds) and `GIT_TERMINAL_PROMPT=0`. A failure returns a `crates/rimz/src/worktree.rs::TrunkFetchFailure` that the CLI prints as a warning, and the tree cuts from the last-fetched `origin/<default>` when it resolves, otherwise from local `HEAD`. A repository with no `origin` skips the fetch: a defaulted `fresh` falls back to `head`, while an explicit `fresh` keeps its missing-ref error.

Before adding a fresh non-PR tree, `create` computes an optional `crates/rimz/src/worktree.rs::StaleBase`: how far `base_branch` trails its upstream, from local refs and without fetching. A tree cut from the remote trunk skips the check, and a fallback to `HEAD` keeps it. The observation travels through `CreatedWorktree` and `LaunchCheckout`, never the marker, and `crates/rimz/src/cli/worktree.rs::new_worktree` and `crates/rimz/src/launcher/checkout.rs::settle_launch_checkout` report it through the CLI on stderr after creation. The check is enrichment: any Git or parse failure leaves it silent.

### Reusing a tree

A launch that names an existing marked tree reuses it, subject to the CLI's [named-worktree entry and cohort checks](../../reference/cli/agents.md#relaunch-into-a-named-worktree), and `CreatedWorktree.reused` marks that. Plain `rimz worktree new` refuses an existing directory with `Exists`. `new --from-pr` can reuse a same-repository branch holder ([from a pull request](#from-a-pull-request)). A reused tree is never re-seeded, never runs the `created` hook, reports zero included and linked counts, and emits no `worktree.created` signal. `report_created` prints `reused NAME` with the path, branch, and next-step lines only.

`rimz worktree new` refuses a name that live agents hold as an explicit lane (`channel::admit_worktree_name`). A launch applies the same admission when the tree does not exist yet (`admit_launch_worktree_name` in `cli/mod.rs`). After a fresh creation, `new` archives any messages left in that channel with the reason `channel recreated`.

`crates/rimz/src/launcher/worktree_entry.rs::check` is the entry check. It reads the marker and returns for a missing, unmarked, or occupied tree. It then prints the `--fresh` notice, and only after that inspects Git, so an occupied tree or a `--fresh` launch pays no Git status. It runs after cross-repository consent and before checkout resolution, from layout `launcher::execute` and supervised `prepare_supervised`. Single agents enter it directly, and cohorts enter only from `Absent`. Harness-owned runs, PR checkouts, explicit resumes, and generated names skip it. The launcher owns entry checks and protected removal, and the CLI presents the closed-cohort offers described [below](#removing-a-worktree).

The facts behind the check come from the harness. `crates/rimz/src/harness/worktree_entry.rs::occupied` applies `schedule::takeover::read` to `cli::alive_snapshot` rows. `crates/rimz/src/harness/worktree_entry.rs::read` takes a marked tree and owns the landed memo load and save, Git inspection with `WorktreeState::removable`, and the newest Audit activity at or under the tree, ended agents included. Both lexical and physical checkout paths match.

### Which repository a launch uses

A launch creates its tree under the repository the command starts in. `resolve_launch_checkout` takes its repository from `ResolvedWorkspace::launch_repo_root`: the main-repository root of the starting directory, or the room root when the start is outside Git. It returns the cwd every pane in the layout gets and the worktree name that becomes the channel. It fails with `LaunchWorktreeRequiresRepo` (or `LaunchPrRequiresRepo`) when neither path is a Git repository.

The `rimz worktree` commands choose their repository differently. `crates/rimz/src/cli/worktree.rs::run` resolves the workspace through `WorkspaceResolver::resolve` from the current directory or `--root`, and that resolver ignores the pane's room pin. Run from inside another repository, `rimz worktree list`, `merge`, `remove`, and `sweep` act on that repository. `rimz gc` resolves through `resolve_participant`, so it acts on the room's pinned project root.

A launch asks before it creates a tree under a repository other than the room's. `confirm_cross_repo_worktree` prints both roots and warns that the room will not list, remove, or garbage-collect that tree. A terminal gets a default-no confirmation, and non-terminal stdin refuses with a `--root <current-git-root>` hint. Cohort relaunch reconciliation resolves its tree path from the same launch root.

A launch that names an existing checkout without a marker can still enter it, without adopting it. The domain returns `Unmarked`, and `settle_launch_checkout` passes the name to `resolve_unmanaged_launch_checkout`. That accepts only a linked worktree of the launch repository: the same Git common directory, the checkout's own top level, and not the main checkout. The user then confirms entry at a terminal (default no), and non-terminal stdin refuses. An accepted checkout gets its cwd and channel with no marker and no seeding, so removal and cleanup never touch it. `--from-pr` launches never take this path: a branch holder needs a marker, though its `from_pr` may be absent and filled on reuse.

### From a pull request

`--from-pr <number|url>` creates or reuses a marked tree for a pull request. `crates/rimz/src/worktree/pr.rs::create_from_pr` validates a URL's host and repository against `origin` before any network call. A named tree whose marker already records the requested PR returns without fetching, and a marker recording another PR refuses with `PrWorktreeMismatch`. Otherwise review-only exits come before the forge head lookup, which selects same-repository or fork handling.

Head resolution picks one of four strategies:

| Condition | Strategy | Branch | Pushes |
| --- | --- | --- | --- |
| `--branch` given, or the name was branch-style | Review-only | The requested branch, at the exact PR head commit | Not configured |
| `origin` has no supported forge CLI, or `gh`/`tea` is not installed | Review-only | Derived from the worktree name | Not configured, and the reason is reported |
| The forge CLI reports a same-repository head | Same-repository | The PR head branch | Upstream is `origin/<branch>`, the PR's own branch |
| The forge CLI reports a fork head | Fork | The head branch, or `<owner>/<branch>` when that name is taken | `branch.<b>.remote` is the fork URL, `branch.<b>.merge` the head ref |

The forge module owns the head query: `ForgeCli::pr_head_args` builds the `gh pr view` or `tea api` call, and `decode_pr_head` parses it. The same-repository decision uses the CLI's cross-repository verdict when it has one, and otherwise compares the head repository with the `origin` slug, case-insensitively.

Review-only and fork checkouts fetch the forge's PR ref (`Forge::pr_refspec`) into a per-process temporary ref, `refs/rimz/pr/<N>-<pid>-<nonce>`, resolve its object id, and delete the ref through a `Drop` guard. Shared `FETCH_HEAD` never decides the checkout, so concurrent PR checkouts cannot cross-contaminate. A same-repository checkout fetches `refs/heads/<branch>` into `origin/<branch>` instead. Every network call is bounded: `PR_HEAD_COMMAND_TIMEOUT` (10 seconds) for the forge CLI query and `PR_FETCH_TIMEOUT` (120 seconds) for a fetch, which runs with `GIT_TERMINAL_PROMPT=0` so a credential prompt fails instead of hanging a launch.

On the fork strategy, `crates/rimz/src/worktree/pr.rs::fork_checkout` calls `add_pr_worktree` before it writes the branch's remote and merge configuration. The `created` hook therefore runs before that push configuration exists.

#### The same-repository branch holder

After fetching the head branch, `crates/rimz/src/worktree/pr.rs::same_repo_checkout` finds the checkout holding it through Git's worktree list, including trees outside the configured directory. The main checkout and an unmarked holder refuse with `PrBranchConflict`, and a marker recording another PR refuses with `PrWorktreeMismatch`. An explicit worktree name that differs from the holder's marker name refuses with a hint to omit `-w` or name the holder. When the local tip equals the PR head, no choice is needed: RimZ reuses the marked holder, adopts an unheld local branch into a fresh tree, or creates a tracking branch when none exists.

When the tips differ, `crates/rimz/src/worktree/pr.rs::classify_divergence` counts local-only and remote-only commits and returns one of three shapes:

| Shape | Condition |
| --- | --- |
| `Behind` | The local tip is an ancestor of the PR head. |
| `Rebased` | `git log --cherry-pick --right-only --no-merges remote...local` lists nothing: no local-only non-merge patch remains. A branch ahead only by merge commits lands here. |
| `Diverged` | Local patches remain. `conflicts` records whether `git merge-tree --write-tree` fails. A branch ahead by non-merge commits is `Diverged`, never a fast-forward candidate. |

Without a choice, the domain returns `PrBranchDiverged` before it moves the local ref or sets upstream. `crates/rimz/src/cli/mod.rs::resolve_pr_branch_choice` owns the warning and the choice for both launch and `worktree new`. At a terminal it defaults to remote for `Behind` and `Rebased` and local for `Diverged`, and EOF aborts. Non-terminal stdin advances only `Behind`, with a warning, and otherwise returns the domain error with its alignment commands. The CLI then retries the domain call with a `PrBranchChoice`, which repeats the head query and fetch:

- `Remote` runs `reset --keep origin/<branch>` in a holder, refusing changes that would be overwritten with a commit-or-stash hint, or `branch -f <branch> origin/<branch>` when no tree holds the branch. The prior tip stays in the reflog.
- `Local` leaves the tip untouched.

Both choices set upstream to `origin/<branch>` before reuse or creation. Prompt details and refusal wording belong to the [CLI reference](../../reference/cli/worktree.md#check-out-a-pull-request).

Reuse fills only a missing `from_pr` and keeps the holder's existing `base_branch`, even a feature-branch base. Fork and review-only strategies always need a fresh tree.

#### PR checkout in a team launch

A named team, or a layout with at least two agent cells, settles a PR checkout in four stages, ordered by `crates/rimz/src/launcher/mod.rs::execute`: lookup, cohort reconciliation, tip choice, then checkout settlement. The lookup is a `create_from_pr` call with no tip choice. Its invariant is that it never moves an existing local branch tip, though a fresh path may create a branch. It may fetch remote refs, set upstream for an equal tip, or fill a missing marker PR number. Reconciliation keys on the `worktree_path` in the holder's marker, which may lie outside the configured worktree directory. A live cohort, a cancellation, or an unattended closed cohort returns before tip choice and before any launch row. A resume settles the chosen tip first, and removing the tree discards the pending checkout and resolves it anew. For PR selection, single-agent specs outside a named team, and supervised launches, take the ordinary checkout path.

#### The PR tree's base

A freshly created PR tree records the repository trunk as `base_branch` (`pr_marker_provenance`, through `trunk_ref`), so the ordinary landed proof reclaims it once the pull request's content reaches the trunk, whether the PR merged, squashed, or rebased.

### Seeding: `.worktreeinclude` and `.worktreelink`

`git worktree add` checks out tracked files and nothing else, so the `.env` and local config an agent needs never follow it. Two optional files at the repository root fill that gap. Both are read from the main checkout at creation:

- `.worktreeinclude` lists glob patterns, one per line. Each match is copied into the new tree at the same repo-relative path. `*` stays within a path component, `**` crosses directories, a leading `*` matches dotfiles, and a matched directory copies recursively.
- `.worktreelink` lists relative directory paths, one per line. Each is symlinked into the new tree with an absolute link target and registered in the tree's effective `info/exclude` as an anchored `/<path>` pattern. It is for heavy machine-local data whose contents do not depend on the branch, such as model or fixture caches.

Both files skip blank lines and `#` comments, and both confine every source to the project root. Absolute entries and entries containing `..` are skipped with a warning. An include match that is itself a symlink is skipped, and every copied file is re-checked against the canonicalized root, because `glob`'s `**` descends through symlinked directories and a committed symlink could otherwise pull host files into a tree an agent reads. A link source must canonicalize to a real directory inside the root, and an existing destination is never replaced.

Seeding is best-effort. A missing file is a silent no-op. A pattern that matches nothing, a failed copy, or an unlinkable directory warns through `tracing` and is skipped, and the tree and its agents still launch. Neither file can run a command, which is why both stay outside the trust hash ([trust.md](./trust.md)).

Git usually resolves a linked worktree's effective `info/exclude` to the repository's shared `.git/info/exclude`. A linked directory's pattern therefore also hides that path in the main checkout and in sibling worktrees.

### Team scratch patterns

A staged team registers Git exclude patterns for its scratch files before its panes start. Fresh launch, explicit cohort resume, and place-first team restore all register through the same harness entry. Registration always adds `/.rimz/team`, with no trailing slash because it names a symlink. When the launch directory is below the Git worktree root, it also adds `/<path from the root>/.rimz/team`, escaping gitignore metacharacters in that prefix. The team's `scratch-files` adds extra patterns, and is empty when unset or `[]`.

`worktree::exclude_team_scratch` appends only missing lines, publishes the effective `info/exclude` atomically, and warns without blocking on a Git or filesystem failure. The patterns hide matching untracked names in the main checkout and every sibling worktree. RimZ never removes a line it registered.

Excluded files do not show in `git status --porcelain`, so a landed tree holding only those entries is clean. Removal deletes checkout-local extras and the archive link, but copies or deletes no archive content: the board and stage notes already live in room state. What the team keeps there and how registration chooses a run are owned by [teams.md](./teams.md).

### Lifecycle hooks

[`WorktreeHooks`](../../../crates/rimz/src/config/worktree.rs) carries two optional commands from machine config, `created` and `removed`. Project config neither overrides them nor includes them in its executable-surface hash, and the seed files never execute anything. Validation refuses a blank command both at config load and at each lifecycle entry point, since launch config loading is lenient. The [guide](../../guide/worktrees.md#prepare-the-tree-with-a-hook) owns the command and environment contract.

`hooks::run_hook` has exactly two fire sites:

| Event | Fire site | When | Runs in |
| --- | --- | --- | --- |
| `created` | `finish_worktree` | After the marker, includes, and links | The new tree |
| `removed` | `remove_checkout` | After a successful `git worktree remove`, before branch deletion | The repository root |

`run_hook` returns at once when no command is configured, without spawning. Otherwise it runs `sh -c` with null stdin through `proc::run_bounded_output`: captured stdout and stderr, a separate process group, and a ten-minute deadline (`hooks::TIMEOUT`) that includes draining output after the direct child exits. A timeout kills the group. The runner sets the five contract environment variables from the marker and leaves the rest inherited. A failure tail keeps the last 4096 bytes and 20 lines of each stream, and successful output is discarded.

A failed `created` hook rolls the tree back with a forced removal. A tree that created its branch goes through `remove_marked_worktree(force = true)`, which also deletes the branch. A tree that checked out an existing local branch goes through `remove_checkout` alone, which keeps the branch. Either way the rollback fires `removed` to undo partial setup, and the call returns `WorktreeErr::CreatedHook`. That error carries the hook failure and the rollback outcome (`tree removed`, or `rollback failed: <error>`), followed by the config key to fix.

So a fresh tree with a configured `created` hook stays marked only when the hook succeeded, with two exceptions. A failed rollback can leave a marked tree, such as a locked one, and `crates/rimz/src/worktree/tests.rs::failed_created_hook_reports_rollback_failure` pins that case. SIGINT or SIGTERM interrupting RimZ during the hook leaves a marked tree with no rollback. The marker stores no hook state, and a later named launch can reuse such a tree after confirmation or `--fresh` without rerunning the hook, so remove it before retrying. A tree that existed before a hook was configured is never initialized retroactively.

A failed `removed` hook only emits `tracing::warn!` and does not change the removal result. Detached wrapper cleanup has null stdio and garbage collection can run unattended, so this teardown is best-effort and nobody may see the warning.

The hook events share their names with two [loop signals](../../reference/cli/events.md#reserved-families), which fire from other places:

- `worktree.created` comes from `crates/rimz/src/launcher/checkout.rs::emit_worktree_created`, called by `new_worktree` and `settle_launch_checkout` after a fresh creation, so after a successful hook. A launch skips the signal, with a debug log, when the store does not open.
- `worktree.removed` comes from `crates/rimz/src/worktree.rs::retire_removal`, so every caller that retires a removed tree emits it ([branch deletion and retirement](#branch-deletion-and-retirement)). A hook rollback calls no retirement and emits no signal.

### Who triggers creation

| Caller | Entry | Notes |
| --- | --- | --- |
| `rimz worktree new` | `crates/rimz/src/cli/worktree.rs::new_worktree` calls `create` or `create_from_pr` | No launch. The PR path can reuse a branch holder and retry after a tip choice. |
| Agent layouts and teams | `crates/rimz/src/launcher/mod.rs::execute`, through the launcher's `checkout::resolve_launch_checkout` to the domain resolver | Cohort reconciliation can remove a landed tree before this path recreates it. |
| Supervised `-p` runs, scheduled loop launches included | `cli/supervised/run.rs`, through the same resolver | See below. |

A `created` hook failure on a supervised run comes before the launch batch and the run record, so no pane or launch row exists. The loop caller routes that error through `crates/rimz/src/cli/loop_cmd/run.rs::record_task_error` to `TaskFire::finish_error`, which records `LoopRunResult::Errored` with the error text.

## Status: dirty and landed

`status(path, marker, memo)` answers the two questions every removal decision needs.

A tree is dirty when `git status --porcelain` prints anything, untracked files included. `WorktreeStatus::changed_files` counts those lines, and `dirty()` tests whether the count is nonzero. `ahead` is the ancestry commit count when available, and `target` records the selected comparison ref.

A tree is landed when the branch's content already exists on the ref it should return to. Landing is proven, never assumed: `content_landed` answers `Landed`, `Unlanded`, or `Unknown`, and anything short of `Landed` keeps the tree. `worktree list` shows the verdict in its `STATE` column (`landed`, `unlanded`, `unknown`) and as `landed: true | false | null` in JSON.

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
| 4 | `log --right-only --cherry-pick --no-merges <comparison>...<head>` lists commits | Landed if the head's tree appears in `log <head>..<comparison>`, otherwise Unlanded. |
| 5 | Rung 4 lists nothing and the head side has no merge commits | Landed. |
| 6 | Rung 4 lists nothing and every head-side merge commit's tree appears in the comparison's last `LANDED_BASE_SCAN_CAP` (500) commits | Landed: a merge-back landing. Otherwise Unlanded. |

Rung 4 carries the squash case. Patch residue alone does not mean unlanded: a squash landing followed by more trunk commits leaves residue while the work is in. Finding the branch tip's exact tree in the comparison's exclusive history proves the destination once held the branch's complete final state. That scan is uncapped, because a landing can be arbitrarily far back.

Together the rungs cover rebased, cherry-picked, squash-landed, and merge-back shapes without trusting ancestry alone or any sidebar state.

### The landed memo

`LandedMemo::proof` resolves the comparison and head refs to commit oids together before proving anything, so moved refs choose a new pair. Shallow boundaries and replace refs can change a proof without changing either oid, so `LandedMemo::load` disables cache reads and writes when the repository is shallow or has any `refs/replace/` ref. Proofs are still computed then. `Unknown` is never cached. The same memo serves the base-branch supersession proof and the branch-deletion proof.

The cache is `<git common dir>/rimz-landed-memo.json`, outside the checkout and shared by the repository's linked trees: `{ "version": 1, "entries": { "COMPARISON_OID:HEAD_OID": { "landed": true, "ahead": 0, "seen_at_ms": 0 } } }`. A missing, unreadable, or unsupported-version file reads as empty. `seen_at_ms` records when the proof was computed: hits do not refresh it, and entries older than 30 days are ignored and recomputed. `save` merges with the file on disk, keeping the newer entry per key, drops expired entries, and publishes through the atomic cache writer. The memo is an acceleration, never a durable record.

### Inspecting every tree

`inspect` pairs Git status with the protection set's holders in a `WorktreeState`. `inspect_owned` loads and saves one memo and runs `inspect` over the discovered trees through `utils::parallel::bounded_map` with eight workers, returning them in input order. A tree whose inspection fails gets an unknown status, so it is kept. Listing and sweeping share this projection.

The loop scheduler enumerates trees for each-worktree tasks through `discover_owned_cached` (called from `harness/schedule/fire.rs` and `harness/schedule/when.rs`). It caches the `git worktree list` rows per repository for 60 seconds, invalidated when the common directory's `worktrees/` entry changes, and reads every marker fresh.

### What the sidebar asks

The sidebar's git-stats refresh (`sidebar/refresh/git_stats.rs`) uses the same proof with different inputs, so its worktree header can disagree with `worktree list`.

- Its landed marker is measured against the sidebar's own trunk, never the marker's `base_branch`. Zero commits ahead is landed, and a clean tree with commits ahead goes through `content_landed`. That trunk tries the per-machine `[sidebar] trunk` setting before `main`, `master`, and the `origin/HEAD` target ([sidebar.md → Worktree groups](../sidebar/sidebar.md#worktree-groups)).
- Its `did_work` marker answers whether the tree has done any work of its own. HEAD equal to the marker's `base_ref` is no work. Otherwise `on_trunk_first_parent` checks whether HEAD sits on the trunk's first-parent chain (scan capped at `LANDED_BASE_SCAN_CAP`). A tree that only tracked the trunk sits there, and a tree carrying its own commits does not.

The sidebar reads markers through `read_marker_from_checkout_metadata`, which follows the checkout's `.git` file directly and forks no Git process.

## Landing on the comparison branch

`rimz worktree merge` lands a tree's branch on its comparison branch in two calls: `landing_plan` picks the destination, and `land` moves it.

`crates/rimz/src/worktree.rs::landing_plan` resolves the marked tree and starts from `comparison_ref`. A local branch is the target as it is. A remote-tracking comparison maps to the local branch whose upstream is that ref (`git for-each-ref --format='%(refname:short) %(upstream:short)' refs/heads`), otherwise to the trunk's local branch. With neither, it refuses with `NoLandingTarget`. `LandingPlan::target` names the local branch in prompts, receipts, and errors. Status comparison stays remote-tracking, so `list` still reads `unlanded` until the local destination is pushed.

The target branch must be checked out somewhere in `git worktree list --porcelain`, or the plan refuses with `TargetNotCheckedOut`. The CLI confirms the named destination before landing, or accepts `--yes`. Non-terminal stdin without `--yes` refuses.

`crates/rimz/src/worktree.rs::land` checks, in order: no pane or live agent inside the source tree (explicit-removal [protection facts](#protection-facts), refusing with `MergeInUse`), the target still checked out at the planned checkout, no rebase, merge, cherry-pick, or revert in progress in either checkout, both checkouts clean, and the marked branch still checked out in the tree. Then it runs these steps:

1. Rebase the tree onto the target with `rebase.updateRefs=false`. On failure, run `rebase --abort` (best-effort, its own error discarded) and return `RebaseFailed`.
2. Recheck that the tree is clean, and that the target checkout has not moved, switched branch, or become dirty (`TargetChanged`).
3. Advance the target with `merge --ff-only` to the rebased head.

`land` never creates a merge commit and never forces. The tree stays marked afterwards, and `sweep` or `gc` reclaims it once the proof passes.

## Removing a worktree

### Protection facts

Git state alone cannot make removal safe: a tree can be clean and landed while someone still has a shell open in it, a loop task needs it later, or a team board is short of `Done`. `ProtectionSet` holds normalized occupied or pinned paths. A candidate checkout is protected when any of those paths equals it or lies inside it.

The CLI gathers pane cwds from the mux and agent rows from the store, and `protection_set_from_runtime` folds them. Task pins enter through `ProtectionSet::insert_loop_task` and team pins through `ProtectionSet::insert_team`:

- A pane contributes its cwd, unless it is sidebar chrome or the caller's own pane.
- A live agent contributes its recorded worktree path and its process cwd.
- An agent of unknown liveness contributes its recorded path, under `Occupancy::Unproven` only.
- A dead agent, or any agent bound to the caller's own pane, contributes nothing.
- An armed machine or instance loop task contributes its root and usually its run directory. `crates/rimz/src/harness/schedule/catalog.rs::worktree_pins` is the shared rule: live and paused tasks pin until their deadline (equality is expired), and disabled and project-source tasks do not. Fan-out tasks pin only their root. A spawning `--worktree` task also pins only its root, unless a check, judge, condition, or watch needs its run directory. Invalid actions or triggers do not release pins.
- A team board short of `Done` contributes its checkout root. `crates/rimz/src/harness/scratch.rs::holding_team` reads the board once through the root's `.rimz/team` link, following foreign links too. The first `Stage:` line decides. No stage, or a read error other than not-found, holds the tree. `Done`, a missing board, and a dangling link hold nothing. The label comes from the linked `run.json`, falling back to `unknown`. A board anywhere else in the checkout is not read.
- A `.rimz` that is itself a symlink (shared through `.worktreelink`) holds nothing, since the board behind it belongs to the checkout it shares. `crates/rimz/src/harness/team_notes.rs::register` enforces the same rule when it refuses to register a team there.

`crates/rimz/src/harness/schedule/catalog.rs::load_worktree_pins` reads machine `loop.toml` and every workspace's instance file strictly, without loading project catalogs, and applies the catalog's machine-over-instance precedence within each root. A missing instance file is empty. Any unreadable listing or source fails the whole protection read, since a room's tasks can pin another room's tree. Arming is read leniently: a missing overlay defaults machine and instance tasks to live, the protective direction.

Paths are normalized lexically (`.` and `..` folded) before comparison, so `/repo/../repo-worktrees/demo` matches `/repo-worktrees/demo`. A symlinked spelling does not match its resolved path ([two spellings](#two-spellings-of-a-trees-path)).

Callers differ in occupancy, the own-pane exemption, and which facts they require. `cli/worktree_protection.rs` fixes them per caller:

| Caller | Occupancy | Own pane exempt | Loop tasks | Team boards | Missing facts |
| --- | --- | --- | --- | --- | --- |
| `worktree remove`, `worktree merge`, a launch's `remove` answer (`for_explicit_removal`, over the room of each root it is given: on a cross-repository launch, the launch's room and the tree owner's) | `ProvenLive` | Yes | Ignored | Ignored | Best-effort: an unreadable roster or mux protects nothing. |
| Wrapper cleanup, `worktree cleanup PATH`, supervised retry (`for_wrapper_cleanup`) | `Unproven` | Yes | Required | Read at PATH | Roster and mux best-effort. A task-source failure keeps the tree. No tree enumeration. |
| `worktree list` (`for_listing`) | `Unproven` | No | Best-effort | Read at every owned tree | An unreadable task source contributes no loop holders. A failed owned-tree listing contributes no team holders. |
| `worktree sweep`, `rimz gc` (`for_automatic_gc`) | `Unproven` | No | Required | Read at every owned tree | A roster, task-source, or owned-tree listing failure fails the gather. `gc` skips its worktree area, and `worktree sweep` exits 1 under its protection-facts context. |
| Cohort relaunch (`ProtectionSet::default()`) | n/a | n/a | Not gathered | Not gathered | No facts gathered. |

Listing and automatic sweeping enumerate owned checkouts with `worktree::discover_owned` to gather their board pins, and the domain inspection then enumerates again. A board read failure protects only that tree. The domain takes pins only and never reads boards. Explicit removal, merge, and relaunch leave boards to the user's decision.

Unattended reclamation uses `Unproven`, so an agent RimZ cannot prove dead still holds its tree. A person naming one tree has already decided, so `ProvenLive` lets only a running process or an open pane hold it, and a stale session record left by a crash never blocks the command that would retire it.

### The assessment

`WorktreeState::keep_reasons` returns every `KeepReason` in fixed precedence: `InUse`, `Dirty`, then `Unlanded` or `Unknown`. Empty means removable. Every removal caller uses this classifier with its own facts.

`ProtectionSet::holders` names agent handles (using the full roster as peers), counts protected panes with no agent, and lists deduplicated loop-task and team names. Loop and board holders are `InUse`, rendered as `loop task NAME` then `team NAME`. JSON adds the `teams` array after `loop_tasks`, with `[]` when none. One holder reached through several paths counts once.

| Verdict | `worktree remove` | Wrapper cleanup | `sweep` and `gc` | Cohort relaunch |
| --- | --- | --- | --- | --- |
| Removable | Remove the tree and delete the branch | Remove, note it on stderr | Remove, count the bytes | Offer `remove / fresh / cancel`, default cancel |
| `Dirty` | Refuse. `--force` removes | Prompt `keep / remove / shell` at a terminal, keep otherwise | Keep: "uncommitted changes" | Offer `resume / fresh / cancel`, default resume |
| `Unlanded` | Refuse with commit count, target, and merge hint. `--force` removes | Same as `Dirty` | Keep: "unlanded work" | Same as `Dirty` |
| `Unknown` | Refuse with the unproven-work fix. `--force` removes | Same as `Dirty` | Keep: "unknown" | Same as `Dirty` |
| `InUse` | Refuse, naming the holder. `--force` warns and removes | Skip silently | Keep: "in use" | Not reached |

The domain owns every refusal. `worktree::remove` returns `WorktreeErr::InUse`, `Dirty`, `Unlanded`, or `Unproven` for the first reason, and `--force` skips the refusal. An unlanded tree with no ahead count or target refuses as `Unproven`. The CLI renders holders from the same protection set. `sweep` groups kept trees by their first reason and shows the others, and `gc` lists every reason and holder. Both serialize the domain sweep records, with a `reasons` array per tree.

Sweep reads one protection snapshot before it removes any candidate. A task booked or a board reopened during the pass can still lose its tree, the same window an arriving agent has. Wrapper cleanup reads the board after its 300 ms roster grace.

Cohort relaunch chooses between its offers by Git state, because it has already established that the cohort's panes are closed. A live cohort instead receives its prompt, or is focused without one. The `remove` answer then gathers the same explicit-removal protections as `rimz worktree remove`, after the prompt, so another live agent or an open pane refuses removal with `in use` and `nothing removed`. A successful removal retires sessions and archives channel messages before the tree is recreated. The offers, `--fresh`, and what each choice does are [fleet.md § Cohort relaunch reconciliation](./fleet.md#cohort-relaunch-reconciliation). Only the `remove` choice enters the removal path below.

### Branch deletion and retirement

`remove_marked_worktree` is the one removal sequence, run from the repository root:

1. Read the comparison ref, for the receipt's target.
2. `remove_checkout`: move the process out of the checkout when its cwd is inside it, run `git worktree remove` (with `--force` when forced), tell the language-server registry the checkout is gone (`lsp::registry::stop_checkout`, debug-logged on failure), and fire the [`removed` hook](#lifecycle-hooks).
3. Read the branch tip into `RemovalOutcome::branch_tip`.
4. Delete the branch.

Branch deletion first tries `git branch -d`, and a success is final. A branch already gone counts as deleted. When Git refuses with "not merged", `delete_branch` runs the memo-aware landed proof against the marker's comparison ref. It escalates to `-D` when the proof passes, and otherwise returns `BranchDeletion::KeptUnmerged` so the CLI can say the branch survived. A forced removal skips the proof and deletes with `-D` directly, and `worktree remove --force` prints the saved tip with a restore command. Three paths force: `worktree remove --force`, the `remove` answer to the wrapper's dirty prompt, and a `created` hook rollback.

After the domain call, the caller runs `retire_removal`, which has three effects. It ends the store sessions bound to that path or branch, archives the worktree channel's messages, and emits the `worktree.removed` signal. Archival runs even when session retirement fails, and both results return in a `#[must_use]` `RemovalRetirement`: the Git removal is already irreversible, and one failure must not hide the other. Callers handle the results differently:

| Caller | On a retirement error |
| --- | --- |
| `worktree remove`, cohort reconciliation | Fail the command |
| Wrapper cleanup | Debug-log both, and debug-log a store that does not open |
| `sweep`, `gc` | Warn on session retirement, report an archival failure per tree |

Attribution resolves lane lifetimes from the checkout at read time (`worktree::lane_lifetimes`). Once the directory is gone, the tree's sessions drop out of attribution, `teams show` cost, and the sidebar's cohort receipt. The records themselves are untouched.

## Who triggers removal

Five callers reach the removal sequence, besides the `created` hook rollback in `finish_worktree`:

| Caller | Entry | What it does |
| --- | --- | --- |
| `rimz worktree remove <name>` | `crates/rimz/src/cli/worktree.rs::remove_worktree` | Builds the path from the configured directory, assesses it with explicit-removal facts, and removes it through `worktree::remove`. |
| `rimz worktree sweep`, `rimz gc` | `crates/rimz/src/worktree.rs::sweep_owned` | The one sweep implementation (below). |
| The exec wrapper | `cli/agents_cmd/exec.rs` | Reclaims a tree an agent was launched into with `--worktree-path` (below). |
| Cohort relaunch | `crates/rimz/src/launcher/worktree_entry.rs::remove` | A team or multi-agent layout launched with `-w <name>` into an existing tree ([the assessment](#the-assessment)). |
| Supervised retry cleanup | `cli/supervised/run.rs` | Defers wrapper cleanup while retries remain, then calls `crates/rimz/src/cli/worktree.rs::cleanup_worktree` after the run settles. That helper loads machine config and reaches `remove_marked_worktree` through `remove_for_cleanup`, so retry teardown runs the same hooks as wrapper cleanup. |

`sweep_owned` discovers every marked tree in the repository from `git worktree list --porcelain`, assesses each, removes the removable ones, retires their sessions and messages, and runs `git worktree prune` when it removed anything. A per-tree failure is recorded and the sweep continues. `--dry-run` reports the same accounting, bytes included, without acting. `sweep` flows removed names in one block, and `gc` prints each tree with every reason and its holders or branch fate. `gc` skips its worktree area with a reason when the directory is not a repository, no store exists, the agent roster cannot be read, a loop-task source cannot be read (`loop_tasks_unavailable`, naming the source in text), or listing fails.

### The exec wrapper

Before launching, `enter_worktree` refuses to start the agent if the marker has vanished since the launch decision, instead of silently running it in the project root. After the agent exits, the shape of the exit decides what happens. [fleet.md § Reclaiming a pane](./fleet.md#reclaiming-a-pane) owns the deliberate-close probe and the end trace.

| Exit shape | Wrapper behavior |
| --- | --- |
| Non-abrupt exit, not a supervised run | Print the relaunch hint and exec a shell in the tree. No cleanup runs, and `sweep` or `gc` reclaims the tree later. |
| Non-abrupt exit of a supervised `-p` run | Run cleanup in the foreground. The dirty prompt is allowed. |
| Signal (SIGHUP, SIGTERM) or tab or pane close, while the mux session still accepts closes | Spawn cleanup detached with `--non-interactive`, null stdio, and its own process group, so it outlives the closing pane. |
| Abrupt exit with the mux session gone, wedged, or resurrected, or with the agent parked for recovery | Skip cleanup. Recovery comes from the sidebar producer's live roster. |

Cleanup runs `rimz worktree cleanup <path>` through `reload::current_reexec_target`: the binary on disk at the wrapper's executable path, which is the replacement when an install swapped the binary while the agent ran. When no re-exec target resolves, or a foreground spawn fails, cleanup runs in-process instead. A failed detached spawn is reported on stderr.

`crates/rimz/src/cli/worktree.rs::cleanup_worktree` runs these steps:

1. Read the marker. No marker is a silent no-op.
2. In non-interactive mode, wait `CLEANUP_SIGNAL_ROSTER_GRACE` (300 ms) so the store roster settles.
3. Resolve the room: `--root`, else the verified room pin in the environment, else the marker's `repo_root`.
4. Gather `for_wrapper_cleanup` protection facts. A loop-task source failure keeps the tree silently.
5. Inspect the tree and act on its first keep reason, as the [assessment](#the-assessment) table gives.

Protection facts, session retirement, and message archival use the room from step 3. Git removal always runs against the marker's `repo_root`.

## Invariants worth preserving

- Read the marker before acting. An unmarked checkout is the user's.
- Keep RimZ metadata out of the working tree. The marker belongs in the Git admin directory.
- Prove a landing. Ancestry counts, branch names, and sidebar state are hints, and every uncertain answer keeps the tree.
- Keep refusals in the domain. A command may name the holder, and it may not decide removal safety itself.
- Keep seeding best-effort and non-executing. A launch never fails because a glob missed, and neither seed file may gain the power to run a command without entering the trust hash.
- Hooks are machine config only. A fresh tree with a configured `created` hook stays marked only when the hook succeeded, except after a failed rollback or an interrupted hook ([lifecycle hooks](#lifecycle-hooks)), and reuse never reruns the hook.
- Add marker fields as `#[serde(default)]` options, so an older tree still cleans up after an upgrade.
- Keep the sidebar's marker read on `read_marker_from_checkout_metadata`, off `git rev-parse`.
