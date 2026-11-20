# Running gates and tests

How the gates and the test runner behave in practice: what each command shows and hides, how to pick one test, what the surface ratchet and a rebase need, which tests outside the change can pin it, and how to compare timings. [gates.md](./gates.md) is the reference for the gate stack and the test profiles, and the everyday commands are in the root [AGENTS.md](../../AGENTS.md#testing).

## Compile and lint signals

- Compile tests with `cargo xtask check`. A bare `cargo check --tests` compiles the same shape, because the crate's self dev-dependency turns `testkit` on for every build that carries dev-dependencies. Editors and agent harnesses therefore report the gate's diagnostics.
- `cargo xtask check` and `cargo xtask lint` build with `testkit` on (lint runs only the all-features clippy, `lint_all_features` in `xtask/src/gates.rs`), so production code that calls a `cfg(any(test, feature = "testkit"))` item passes both. Before hand-off, run `cargo clippy -p rimz --bin rimz --locked -- -D warnings`, the install-host shape the gate runs (`INSTALL_HOST_LINT_ARGS` in `xtask/src/gates.rs`).
- `cargo xtask check` prints no compiler warnings. After an edit that can leave test code with unused bindings (a removed parameter, a dropped field), run `cargo xtask lint` before the first test run, or list the warnings with `cargo build --tests -p rimz --message-format short`.
- A remove-and-compile census (delete an item, count the compile errors) measures the library alone. The unit-test target is the library built under `cfg(test)`, and the crate's self dev-dependency links the library into it, so neither `--tests` nor `--keep-going` type-checks `#[cfg(test)]` code while the library fails. Count test-side reach after the production errors are fixed, or report it as unmeasured.
- Never gate a command on a piped check (`cargo xtask check | tail && git commit`). The pipe exits with the status of `tail`, so a failed check still commits.

## The test runner

`cargo xtask test` wraps `cargo nextest run --workspace --all-features --locked`. Trailing arguments forward to nextest as filters and profiles (`cargo xtask test auth`, `cargo xtask test -P live`). nextest is the only suite runner. Install it with `cargo install cargo-nextest --locked`.

- Drive the suite through `cargo xtask test`. xtask supplies the disposable host-state roots ([test environment](./gates.md#test-environment)) and the suite's feature set.
- nextest runs each test in its own process, and the suite relies on that. A unit-test binary run directly runs every selected test in one libtest process, where tests that share process-wide state fail each other. A direct run (a `TZ` reproduction, say) names one test with `--exact` per call.
- `cargo xtask test` prints only its summary line. `--no-capture` hands the terminal to nextest (`requests_no_capture` in `xtask/src/gates.rs`), which runs the tests one at a time and prints each test's own output and one `PASS [   0.287s]` line per test. Redirect it to keep the stream: `cargo xtask test --no-capture <filter> >/tmp/run.log 2>&1`.
- A failing captured run, the gate's test step included, saves its complete output to a uniquely named `.output` file under the temporary directory (`$TMPDIR`, with the usual fallback). The excerpt and any skip reports end with `full test output: <path> (<N> lines, <K> KB)` above `NEXT:`. A passing run writes no file, including one refused only by `--deny-skips`, and `--no-capture` saves nothing.
- Long phases show an animated status line on a terminal. For agents and redirected output they print a newline heartbeat after 15 seconds and every 30 seconds after that. `RIMZ_NO_PROGRESS=1` silences both.
- The `rimz` CLI spinner has no heartbeat: it draws only when stderr is a terminal outside an agent. An agent therefore runs a long `rimz` command under `rimz wait --run`. `xtask/src/spinner.rs` is a separate copy of `crates/rimz/src/cli/spinner.rs`, so that xtask stays independent of the runtime crate.
- An agent tool sandbox can deny Unix socket binds. `PermissionDenied` from `UnixListener::bind` in `agents::spending::service` tests is one known symptom. When a failure occurs only under that sandbox, rerun the affected tests unsandboxed (`cargo xtask test 'agents::spending::service'`). xtask still supplies its disposable roots.

### Self-skips

A test that returns early because the host lacks a capability records the skip through the integration harness's skip helper. nextest counts such a test as passed.

- Every xtask test run (`gate`, `test`, `--name`, `--no-capture`) prints a self-skip report after nextest's result: the count of distinct tests that self-skipped, then one line per reason with its count and the first test names. A clean summary with a report beneath it did not exercise those tests. A run with no skip prints no report.
- `cargo xtask test --deny-skips` works on every run shape (plain, `--name`, `--no-capture`). It fails the run when a recorded skip reason starts with no entry of `.config/allowed-test-skips.txt`, even when nextest passed. It lists the offending tests by reason, then the fix.
- Each reason a test records is judged on its own, so an allowed skip recorded later (a partial skip, a retry) does not cover an unlisted one.
- xtask consumes the flag, and it never reaches nextest. A missing allow-list is an error that names the path.

### What a test binary is

- Every test binary is built `--all-features`, so each `rimz` process a test spawns is a `sentry` build. It runs `observability::reporting::init` before dispatch, which reads the per-machine core config file. A test that pins what a process loads, prints at startup, or holds in memory describes that build.
- To check a build without `sentry` by hand, build with `cargo build -p rimz --bin rimz --features testkit`. That binary keeps the unoptimized dev profile. Measure a CPU or time figure that a doc cites on `cargo build --release -p rimz --bin rimz --features testkit`, held in a sandbox room with `RIMZ_BIN=target/release/rimz cargo xtask sandbox room`.
- With none of the `RIMZ_UNIT_TEST_*` keys set, a library unit test gets per-process temp roots, and nothing removes them. A bare run can leave a runtime root at `/tmp/rimz-unit-<16 hex>` and a home at `$TMPDIR/rimz-unit-test-home-<pid>-<uuid>` for each test process that wrote. Remove them after the run.
- A re-exec test passes a root under its unit-test key.
- Under `cargo xtask stress` every copy of a library test shares the sandbox's unit-test home. `--env RIMZ_UNIT_TEST_RIMZ_HOME=<dir>` picks which directory they share (one you keep or pre-fill). Library unit tests ignore `--env RIMZ_HOME`.

## Selecting tests

| Goal | Command |
| --- | --- |
| Find a test's exact name | `cargo xtask test --list <fragment>` |
| Run tests by exact name | `cargo xtask test --name <test> [--name <test>...]` |
| Run one module of the library | `cargo xtask test 'sidebar_pane::app::width_control'` |
| Run one integration module | `cargo xtask test -E 'binary(integration) & test(/^message::/)'` |
| Run one xtask module | `cargo xtask test -E 'package(xtask) & test(/^sandbox::/)'` |
| Run one live module | `cargo xtask test -P live -E 'binary(integration) & test(/^backend::tmux::team_stage::/)'` |
| See every new test fail | add `--no-fail-fast` |
| Rerun the gate's test step alone, when it times out even under a raised `RIMZ_XTASK_TIMEOUT` | `cargo xtask test --profile gate --no-fail-fast` |

### Names

- `--name` takes the nextest test name without the binary: an integration test is `lsp::some_test`, never `integration::lsp::some_test`. A leaf name or a full `module::path` name both work, and the match is exact and segment-anchored.
- Repeat `--name` to batch several tests through one nextest run. Do not launch parallel Cargo processes. xtask lists the selection first, runs every matched test, and fails after the run if any requested name was unmatched.
- `--list` prints each binary's id (`rimz::integration`) followed by its exact names. The binary's path comes from `cargo xtask test --list <fragment> --message-format json`, or from the `binary:` line of a `cargo xtask stress` `summary.txt`.
- `--list` builds the test binaries first. Run a cold one under `rimz wait --run`, never as a foreground call: a cold build on a loaded host outlasts the 10-minute tool cap.
- During a read-only sweep, run `rimz lsp hover` on the test function. The module path it prints, minus the crate name (`rimz::` or `integration::`), plus `::<fn>`, is the exact name.
- A file mounted with `#[path = "..."] mod name;` runs under the mounting module's path plus `::name`, never under its file name. A sibling `#[path = "foo_tests.rs"] mod tests;` runs as `loop_cmd::list::tests`. The integration file `loop_list.rs` runs as `loop_schedule::list_tests`, so `test(loop_list::)` matches nothing.

### Filters

- A filter and `--name` intersect. When a run that passes both reports "0 tests run", the intersection is empty: run the two as separate calls.
- A raw nextest filter that matches nothing fails and names the `--list` command as its next action.
- Integration tests live in their own binary, so a module filter names it with `binary(integration)`. A bare `'integration::message'` matches nothing.
- Anchor the module as `test(/^<module>::/)`, since `test()` matches a substring: `test(wait::)` also runs `teams_wait::`, and `test(stats::)` also runs `performance::sidebar_diff_stats::`.
- xtask's own unit tests run in binary `xtask::bin/xtask`, and the `rimz` library has modules of the same names (`sandbox::`). A filter for them names the package with `package(xtask)`.
- The default nextest profile stops at the first failure, and a test's profile comes from the filters in `.config/nextest.toml` ([test profiles](./gates.md#test-profiles)).

### Durations

- Locally, `--no-capture` prints each test's duration ([the test runner](#the-test-runner)).
- CI's `gate` profile prints only failures and retries and keeps no JUnit report (`[profile.gate]` in `.config/nextest.toml`). Only `live` and `journey` write `junit.xml`. Read a gate test's duration on trunk from the Coverage workflow's log (`gh run view <id> -R rimio-ai/rimz --log`).

## Tests outside the change

A test in a module you never edited can pin the old behaviour, and only the full gate then finds it. The [root rule](../../AGENTS.md#before-a-change) is to grep for what you changed and run each module that names it. These cases need more:

- A refusal, error, or warning whose wording or trigger changed: grep the old text as a fixed string across `crates/rimz/src` and `crates/rimz/tests`. For a refusal, also grep the error variant's typed fields (`rg -n 'field: "<name>"'`), which unit tests assert in place of the text. Reread a new or widened message against each case it covers, including what any command it tells the user to run does in that case.
- A field added to a record that tests compare as whole JSON rows: update each golden in place, so that its only diff is the new key. Never drop a key from a comparison to make it pass.
- A field added to a `RunRecord`: two records have that name. The workflow run in `store/workflow.rs` has a JSON-shape snapshot that pins every key. The supervised run in `store/run.rs` has per-field round trips only, so a field added there brings its own round-trip test.
- A field with accessors: a function that returns a field counts as the field. List the accessors with `rimz lsp refs` and run every module that names one. `AgentState::resolved_context_window` returns an adapter's default, so removing Codex's default failed a test that named only the accessor.
- A guard deleted because its state is no longer written: list every writer of that state, bypass flags such as `--skip-capacity` included, and keep the guard for each case a writer can still produce.
- A filter deleted from a lookup that returns its first match: list each case where several candidates pass what remains, and pin which one wins. The filter may have been the only thing choosing.
- A new or extended probe or poll path: test its failure paths as well as success. A first attempt that fails with no prior state backs off and does not rerun on every pass. One target's error does not fail or silence its group. A test over a stubbed command asserts the call count per endpoint.
- A reader bounded by a window relative to now: make the fixed timestamps of its tests (`rg -n '"20[0-9]{2}-[0-9]{2}-[0-9]{2}T'`) now-relative, or give the reader an explicit floor. A fixture inside the window today falls out of it later.
- A read pointed at rooms beyond the caller's: trace each diagnostic record it can emit to `classify_diagnostic` in `crates/rimz/src/cli/doctor/runtime.rs`. A record classed `Investigate` files a health incident against the other room's `rimz doctor`, written again on every run.

## The surface ratchet

`cargo xtask atlas conform --ratchet` is the surface-budget and dependency check. The pre-commit hook runs it, and `check` and `lint` do not see it ([what it is](./gates.md#the-conform-ratchet)). [atlas.md](./atlas.md#conform--keep-the-target) is the reference for `conform` and the `refactor-target.toml` schema.

### Admit a new item

A new `pub` or `pub(crate)` item, module, or cross-module import can fail the ratchet until `refactor-target.toml` admits it.

- For an item, add a `[[verdict]]`. A `pub` method on an existing type needs no verdict but still counts toward its module's surface.
- When the module's surface is at the cap, raise its `surface-budget`: paste the rule the ratchet prints, and add a `#` line above the budget that names the item, why it escapes, and the raise (`(+1)`). Keep every earlier line.
- For a new import, name the imported module in the importing module's `allowed-dependencies`, with a comment that says why the edge is deliberate.
- Count every item the change adds in its `(+N)`, including those that fit under the cap at the branch point. Trunk can use that slack before the rebase, and the [rebase sum](#rebasing) then comes out short.
- A `pub(super)` item whose parent module sits inside the same `[[module]]` boundary (`harness::spec` inside `crates/rimz/src/harness`) escapes nothing and needs no admission. Share a helper at that visibility in preference to copying it.
- A file with its own `[[module]]` entry (`crates/rimz/src/harness/plan.rs`) is a boundary of its own, so its `pub(super)` items count toward that entry's `surface-budget`.

### Find the entries

`refactor-target.toml` runs to over 13,000 lines. Never read it whole.

- Find a module's entries with `rg -n '<module path>' refactor-target.toml` and read around each hit.
- Search both key forms. `[[verdict]]` keys are Rust paths (`agents::spending::publish::ProviderSpendingCache`). `[[module]]` budgets are keyed by file or directory path (`crates/rimz/src/agents/spending`).
- Search each enclosing directory path too. A new `pub` item can count toward an enclosing directory's `[[module]]` budget: `WindowStarts` in `agents::spending` raised `crates/rimz/src/agents` by one as well.
- A new `pub` item in a `pub` module that the boundary's root module also re-exports counts twice, once at its definition and once for its `pub use` leaf. `ResetGrant` in `agents::credits` and its leaf in `crates/rimz/src/agents/mod.rs` raised `crates/rimz/src/agents` by two.

### Name what a raise covers

- `cargo xtask atlas diff --base "$(git merge-base HEAD origin/main)" --path <boundary>` lists each added surface item. Take the names for a `(+N)` line from it.
- A `conform --ratchet` failure with the budget one lower proves the sum. It does not prove the names.
- `atlas diff` has run for over 20 minutes on a shared host, so run it under `rimz wait --run`. When it cannot finish, count each boundary's new `pub` and `pub(crate)` items and `pub use` leaves by hand, and prove the sum the same way.

### Keep the comment lines true

- A change that removes or renames an item a `#` line names rewrites that line to name what remains, with no tag. `(+N)` marks a raise and nothing else.
- A line reworded while every item it names remains keeps its `(+N)` tag.
- A shrunk surface may keep its budget, which is a ceiling.

### Before a design crosses modules

A plan that routes a call into another module first reads three things:

- The calling module's `allowed-dependencies` (or `upward-dependencies`) entry. A module missing from the list reshapes the design, or needs an admission that the plan names.
- The `layers` list at the top of `refactor-target.toml`. A call may only reach a module in an earlier layer unless `upward-dependencies` admits it.
- Every module contract in an `AGENTS.md` that lists what the calling module reaches (``rg -n '`address` reaches' crates --glob AGENTS.md``). Atlas checks only the modules that carry a list.

### Untracked files

The ratchet indexes the worktree's untracked `.rs` files too, whether a module declares them or not. An untracked file that held one `pub fn`, never declared as a module, failed it on its module's budget and on the enclosing `crates/rimz/src/harness` budget alike. When you split a change into commits, hold back a later commit's new file before you commit an earlier one. Move it out of the tree, or stash it under a unique message (`git stash push -u -m <tag>`) and restore it by its SHA.

## Rebasing

A rebase runs no hook, and the gate builds only the head. Each case below names what to run by hand.

- Every rebase: run `cargo xtask atlas conform --ratchet` and `cargo xtask invariants` afterwards.
- A conflict resolved in a Rust file: check formatting at every commit whose conflict you resolved. `git rebase --exec 'cargo fmt --all --check' "$(git merge-base HEAD origin/main)"` checks each commit in place. A resolution left unformatted passes `check`, and the next commit that formats the file carries the churn in its diff. Name the merge base, since `origin/main` would replay the branch onto a trunk that moved. The command checks out each commit, so never run it alongside a build or gate in the same checkout.
- Trunk calls an item the branch changed, or changes an item the branch calls (a shared test fixture included): run `cargo xtask check` at each branch commit before any test run: `git rebase --exec 'cargo xtask check' "$(git merge-base HEAD origin/main)"`. An earlier commit can stop compiling against the new trunk without any conflict.
- The same check from a check-only subagent: brief it with a detached [scratch worktree](#builds-and-scratch-worktrees) per commit (`git worktree add --lock --detach "$TMPDIR/<dir>" <commit>`, then `cargo xtask check` inside it), since such an agent declines `git rebase --exec`, which moves the checkout.
- The branch removes or renames a text format that tests write as strings (a workflow definition field, a record key): grep the rebased tree for each old spelling before any gate run. `git diff --name-only <old merge base> origin/main` lists the files trunk changed. Trunk's new tests carry the old spelling without a conflict or a compile error, and fail only when run.
- Trunk adds a flag, state, or refusal to a command the branch changed (`git log --oneline <old merge base>..origin/main -- <the command's files>`): reread each message the branch adds to that command (a receipt line, a note, a refusal) against it, conflicted or not. Trunk's `--once-per-checkout` arrived without a conflict and left a branch's `loop add` note false.
- Trunk added a reader over an item the branch deletes or reshapes: this is a design question for the branch, asked before the rebase. One such rebase stopped on compile errors in a file the branch never touched.

### Conflicts on a surface budget

- Set the value to trunk's value plus the branch's own `(+N)` raises, with trunk's `#` lines kept before the branch's. This holds for a conflict on the value and for one on the `#` lines alone, because git merges two equal raises into one without a conflict on the value.
- When a later commit of the branch rewords a `#` line that an earlier one added, the resolution at that commit keeps the new wording alone. Keeping both sides leaves the old line in, and its `(+N)` is counted twice.
- `cargo xtask atlas conform --ratchet` fails a sum that is too low. A sum that is too high passes, so check it against the `(+N)` lines.

### Version bumps

A branch that bumps `SNAPSHOT_VERSION` or `ROLLUP_CACHE_VERSION` re-checks each against trunk after a rebase, even a clean one. When trunk bumped a version to the value the branch chose, git merges the two bumps into one without a conflict, and each regression test's literal pre-bump stamp still reads as older and passes.

- Where trunk moved a version since the branch's old base, bump again past it and move the pre-bump stamps to trunk's value.
- A conflict on the `*_contract_is_version_<N>` pin in `crates/rimz/src/store/snapshot/assemble/tests.rs` keeps a single pin at the final value.

## Builds and scratch worktrees

- This worktree's built binary is `$(cargo metadata --format-version 1 --no-deps | jq -r .target_directory)/debug/rimz`. The target directory may be shared with other worktrees, each under its own hash directory, so resolve it this way and never pick the newest.
- Run a throwaway probe test in a detached scratch worktree: `git worktree add --lock --detach "$TMPDIR/<dir>" HEAD`. Set `CARGO_TARGET_DIR` to your worktree's own target directory (the `cargo metadata` lookup above), so the build reuses that cache. Remove the scratch worktree afterwards with `git worktree remove --force --force "$TMPDIR/<dir>"`.
- The lock keeps another agent's `git worktree prune`, which cannot see a sandbox's `$TMPDIR`, from deleting the admin directory mid-run. A single `--force` refuses to remove a locked worktree.
- A `cargo xtask` run from a scratch worktree on a shared target directory replaces `target/debug/xtask` with that tree's build. Cargo does not rebuild it for the checkout that owns the target directory, so that checkout's next `cargo xtask` and its pre-commit hook run the scratch tree's gates. Afterwards, run `touch xtask/src/main.rs` in that checkout.

## Background runs

Run a gate or test as `setsid --wait <command>` when it starts outside the terminal's foreground process group while it still holds that terminal. A command that a `rimz wait --run` watcher or a trailing `&` starts from an agent pane is such a run.

Every test that reaches the interactive shell probe of a launch (`bash_interactive_shell_argv` in `crates/rimz/src/harness/launch.rs`) starts a `bash -i` that opens the controlling terminal. From a background group, bash stops itself and the test with `SIGTTIN`. A gate run that way timed out 30 launch and sandbox tests (`did not finish within 30s; killed`, or a nextest timeout near 370 s) that pass in seconds under `setsid --wait`.

## Codex agent shells

- To reproduce a failure that a Codex agent reports from a `rimz` command, run `codex sandbox -c 'default_permissions=":workspace"' -- <command>` from the checkout, outside a Codex pane. It applies Codex's own bubblewrap and seccomp to that command. The bare `codex sandbox linux` spelling fails with `execvp linux`.
- A routine `rimz` command that a Codex agent runs bare matches its `prefix_rule` and runs unsandboxed, so reproduce the wrapped form the agent ran.
- A gate run from a Codex agent's shell hangs the launch-path tests (`agent_launch::`, `sandbox::`, `harness::launch_plan::tests`, `harness::launch::tests`) on every rerun. Some fail at their 30-second child deadlines and the rest at nextest's 370-second timeout. All pass in seconds from a Claude agent's shell. The cause is not diagnosed.
- Before you read such failures as the change's, rerun those tests by exact name (`cargo xtask test --no-fail-fast --name <test> ...`) from a Claude agent's shell or a user shell. Brief a gate subagent on Codex to list them and report the gate as incomplete.

## Cross-target checks

`gate` compiles the host target only. Run CI's four cross-target type-checks when a change touches `nix`, `libc`, or platform-gated code, or adds a call to a `#[cfg(target_os = ...)]` item (a test helper included):

```sh
# Linux-only items in the shipped library and binary, on macOS
RUSTFLAGS='-D warnings' cargo-zigbuild check -p rimz --target aarch64-apple-darwin --locked
# the same in the test targets, which build with testkit on
RUSTFLAGS='-D warnings' cargo-zigbuild check -p rimz --target aarch64-apple-darwin --locked --tests
# glibc-only items, on musl
RUSTFLAGS='-D warnings' cargo-zigbuild check -p rimz --target x86_64-unknown-linux-musl --locked
# architecture-specific types, on aarch64 Linux
RUSTFLAGS='-D warnings' cargo-zigbuild check -p rimz --target aarch64-unknown-linux-gnu --locked
```

They need `cargo-zigbuild` and `zig` on `PATH` ([setup](./build-and-ci.md#release-packaging)).

## Timing comparisons

A single perf run on a shared host is no evidence of a regression or of its absence. Every comparison here runs the merge base and the head back to back, in the order base, head, head, base.

### Which bench a change times

All of these run as `cargo xtask perf --bench hotpath -- <bench>`, before hand-off.

| Change | Benches |
| --- | --- |
| The snapshot fold or its reducer | The touched fold benches: `rollup_fold_warm`, `rollup_fold_unchanged`, `carryover_fold_cold`, `carryover_fold_cold_trimmed`, `carryover_fold_rebirth_warm` |
| `sidebar::enrich::enrich_core` or a step it calls | `enrich_cached` |
| The room spend slice (`aggregate_scoped_rollups` in `crates/rimz/src/agents/spending/aggregate.rs` and what it calls) | `spending_live_scale_additional_workspace_scope`, `spending_live_scale_additional_workspace_partial_scope` |
| The walker memo build (`SpendingWalker::ensure_memo`) | `spending_walk_cold`, `spending_live_scale_cold_hydrate` |

The benches are `#[divan::bench]` functions in `crates/rimz/benches/hotpath.rs`. The walk benches pass `scope: None` and never reach the room spend slice. The warm fixtures build the walker memo during setup, which is why the memo build is timed on the cold benches.

### Reading a difference

When one round shows a difference on a host whose load average is above its core count, run three more rounds on the same copied binaries before you read it. One round showed `rollup_fold_unchanged` 1.9x slower, and the next three showed nothing.

### Comparing two trees

A base and head comparison on one target directory rebuilds RimZ at each switch of tree, over five minutes a time.

1. Build each tree once with `cargo xtask perf --bench hotpath --no-run`.
2. Copy the binary that its `Executable` line names.
3. Run the copies (`<copy> --bench <bench>`) in the order base, head, head, base.

This holds for a bench that stays in process. A bench that spawns `rimz` (`hook_stop_budget`) finds the CLI two directories above its own path. A copy outside `target/release/deps/` panics starting the sandbox reaper, and a copy inside it runs whichever tree built last. Run that bench through `cargo xtask perf` on each tree.

### Live probes

- A live timing probe judged against a target (a tmux or Zellij room, a sidebar latency) runs its controls (the stock mode, the off mode) in the same session, in the same order, before you call a miss. A miss whose controls moved with it is host load.
- A live base-versus-head comparison runs both trees from checkouts whose paths have the same length. A pane's shell prompt carries the cwd, and its wrap can change what a capture shows.
