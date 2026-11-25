# Flakes

How to run the live tiers, report a failure you leave unfixed, and reproduce, force, read, and trace a flake. What a test in the integration suite needs is in [integration-tests.md](./integration-tests.md), and the runner's flags are in [running-gates.md](./running-gates.md#the-test-runner).

## Live test runs

- Run live Zellij tests through `cargo xtask test`. A bare `cargo nextest run` from a shell that exports `ZELLIJ_SOCKET_DIR` can fail them with `There is no active session!`. Prefix it with `env -u ZELLIJ_SOCKET_DIR`.
- Run `-P live` with the profile's configured retries. A test that passes on a retry is reported as flaky, with its first panic line. Only a test that fails every retry is a failure, and only a failure gets compared against trunk.
- Before you treat a trunk-comparison failure as the branch's, check that the test runs the changed code: `git diff --stat origin/main...HEAD -- <paths the test exercises>`. A fixture on `sidebar_stub_alive_for` runs no sidebar host code, for example.
- Compare at least 6 `--retries 0` runs per side. 3 runs cannot tell a 1-in-3 flake from a regression.
- CI runs tests as root (the `rimz-ci` image built from `ci/Dockerfile` sets no `USER`), so a permission fixture (`chmod`) skips itself there and proves nothing. To make an atomic cache write fail for any uid, put a directory at its destination path. The final rename then fails with `EISDIR`, root included.
- A set `CI` variable turns colour on in a spawned `rimz` even under `TERM=dumb`. A PTY test that matches plain text removes it from the child (`cmd.env_remove("CI")`). A change that adds such a test runs its module once as `CI=true cargo xtask test -E 'binary(integration) & test(/^<module>::/)'`, since the local gate runs without `CI`.

## Reporting a failure you leave unfixed

Quote a gate failure on a test you leave unfixed (a flake, another branch's failure) in your notes or the PR as four things:

1. Its name.
2. Its `panicked at <file>:<line>:<col>` line.
3. Its message, with the assertion's printed values verbatim, or the kept log's `path:line`.
4. Its run time.

Its log under `$TMPDIR` is gone before anyone investigates. The panic line alone can say which step of a multi-step test failed, and a paraphrase of the printed values drops the state that names the cause.

## Stress batches

A batch needs the user's leave ([Testing](../../AGENTS.md#testing)). Without it, [force the race](#forcing-a-race).

### Run a batch

- Stress a load-only flake with `cargo xtask stress <module>::<test>`. Do not go through nextest, whose retries and group caps hide the rate you are measuring.
- The command builds and locates the test's binary. It runs 400 copies in the test sandbox, two per CPU at a time, while one busy process per CPU holds the host. It prints the 1-minute load average before and after the batch beside `failed: K/N`.
- Each failing copy's output stays under `--out` with `summary.txt`. A copy runs with a backtrace, so a helper's panic names the call in the test.
- Give each batch its own `--out`. The command refuses a directory that already holds kept copies.
- A batch that the xtask budget stops still writes `summary.txt`, counting only the copies that completed. Widen the budget with `RIMZ_XTASK_TIMEOUT` for a long batch.
- Copies run in parallel by default, because a race against a spawned child's startup can stay at 0 in a serial loop, where a warm binary starts fast. `--jobs 1` is that serial loop, for a test whose copies would contend with each other and not with the suite.

### Check the host first

- Record the load beside the count. A batch that another worktree's stress run overlapped is not a baseline.
- Check for an overlapping run by process name, `pgrep -ax xtask`, and read the listed command lines for a `stress` argument. `pgrep -af 'xtask stress'` also matches every agent shell whose own command line quotes the phrase.
- When an overlapping run stresses the same test, find what it is fixing before you plan. Its checkout is `/proc/<pid>/cwd`. Once it has exited, its `--out` name, the open PRs, and the trunk log for the test file say.
- When the 1-minute load already exceeds `nproc` before a batch, other work is loading the host for you. Run `--hogs 0` and `--jobs` at `nproc`, and raise them only when the baseline reads 0. One default batch started at load 75 on 28 CPUs reached 169, and the user stopped all stress.
- Before a loaded batch, stress a control with the same flags: a pre-existing test that goes through the same fixture helpers as the test under proof. When the control itself fails most copies, the batch measures the host. Lower `--jobs` until the control passes before you read anything into the new test.
- When more than about one copy in ten fails in fixture setup (room birth, launch timeouts), the batch measures the host. Halve `--jobs` and `--hogs` and rerun before you count the test's own signature.
- A batch of a sub-second unit test ends before the 1-minute load average moves, so its before and after loads say nothing about the load its copies met. Raise `--copies` until the batch runs a minute or more, or record that the load was not matched and rest the fix on a forced proof.

### Get a baseline

- Count failures before and after the fix. A race that shows 1 to 3 in 400 loaded copies is typically 0 unloaded.
- A first batch started on an idle host can under-load a race, since the load is still climbing while its copies run. One flake showed 0 in 400 from load 10, and 4 in 1,200 only in a batch that started near 100. Run a second batch straight after the first before you read a zero as a baseline.
- A load-only flake first seen under ambient load (other agents' builds) may not reproduce from the batch's own hogs. Record the load at the time of the original failure. When the baseline reads 0 at a lower load, raise `--jobs` and `--hogs` until the load matches before you spend more batches. If it cannot be matched, say the fix rests on design and stop.
- When a flake was reported across several tests, give each candidate one loaded batch before you spend a second on any. A shared cause can show in only one of them.

### Read the counts

- Classify each failing copy by its panic before you count. A fix is shown by the bug's own signature going to zero across the after-batches. Copies that fail in fixture setup (room birth, launch timeouts) are listed separately and do not count against the fix.
- A new test stressed beside its control is read the same way. It passes when no copy fails an assertion the test itself makes about the behaviour under proof. Failures in shared session setup are classified by their site and compared with the control's sites across batches, never by one batch's count.
- Zero takes two batches on the final tree at the failing baseline's own flags, each with at least the baseline's copies.
- When the last residual was rarer than one per baseline batch (1 in N), size both batches with `--copies` to hold at least 4N between them as well. 4N alone can come out too small to hold the jobs busy.
- A fix that changes a helper several tests share takes a loaded batch on the final tree for every test that calls it. A sibling's batch on the base tree ran the old helper, so it is a control and no evidence for the new one.
- Judge an after-batch by whether it matches the baseline's shape (copies, `--jobs`, `--hogs`). The load it ends at misleads: a fixed test finishes sooner and ends lower.

### Base and fixed trees

`cargo xtask stress` builds and runs the test from the checkout it is started in.

1. Commit the fix before the baseline.
2. Check out the parent in a standalone clone: `git clone --no-checkout <primary> <dir> && git -C <dir> checkout --detach <parent>`.
3. Alternate baseline and fixed batches between the two checkouts (base, fixed, base, fixed), so each pair runs at the same ambient load.

A clone runs because the stress sandbox's only Git setup is a `safe.directory` entry for the checkout under its own `HOME`. Do not use a linked `git worktree`: another agent's prune can remove its registration in the primary's `.git/worktrees/` mid-batch. A fixed batch run after a long rebuild can meet a host that has gone quiet, which is why the batches alternate.

- A second standalone clone at the fix commit, for the fixed batches, leaves the checkout free for `cargo xtask gate` meanwhile.

### Keep a failing copy's state

- Add `--env RIMZ_TEST_KEEP_FAILED=1` to read a failing copy's store, event log, and diagnostics. An `Env` dropped while panicking moves its home and runtime roots to `/tmp/rimz-test-kept-*/` and prints a `rimz-test-kept-roots:` line that names them into that copy's `run-N.out`.
- The copy's event log is `rimz-home/ws/<workspace>/log/events.log.jsonl` under the kept home root. Each line carries a length and a CRC before the JSON, so read it with `cut -d' ' -f3- | jq`.
- Paths recorded inside the kept state still name the original roots. The kept directories are yours to delete.
- `cargo xtask test` strips the variable. A bare `cargo nextest run` carries it.
- To prove that a run keeps or moves an artifact, assert on the contents at the destination and make the probe test fail with an uncaught panic. A missing original path holds after removal and after a move alike, and stress keeps output only for failing copies.

### Flakes that load does not show

- The `remote_attach` web-relay tests run `--web` without a port, so every relay child on the host scans the same 100 ports (8300..=8399). A stress copy that fails with `no free local web tunnel port in 8300..8399` under `binding local web tunnel relay` is that range exhausted by more than 100 concurrent `--web` children (one default batch on a host with over 50 CPUs, or two overlapping batches on a smaller one), not a pick-then-bind race: lower `--jobs` and rerun.
- A flake on a file's `(mtime, len)` identity, such as a stat-keyed parse cache serving the prior parse after a same-length republish, runs the other way: load spreads the writes across mtime ticks and hides it. Loop it unloaded a few thousand times instead. `--hogs 0 --jobs 1 --copies 4000` is that loop.
- A flake that shows only in some full-suite runs may depend on the runner's own state and not on load. When the error names a derived identifier (a session or directory name), rerun `cargo xtask stress` with `--env RIMZ_HOME=<fill>`. The fill is a directory pre-filled with non-owned `ws/collide-<hex4>` directories that cover all 65,536 four-hex prefixes.
- For a library unit test, pass `--env RIMZ_UNIT_TEST_RIMZ_HOME=<fill>`. Library unit tests ignore `RIMZ_HOME`, so the fill would stay untouched and read as clean.
- Use a slug different from the fixture's. An unrecorded directory with the fixture's own slug is adopted and is no collision. Id-only lookups adopt only a recorded directory, so the unrecorded collision fill cannot absorb them.
- Any new entry under the fill after a run marks a runner-home write. Inspect both machine-wide additions and children of the collision directories with `find "$F" -mindepth 1 -not -path "$F/ws/collide-*" -not -path "$F/ws"; find "$F/ws" -mindepth 2`, where `F` is the fill root passed as the home.

## Forcing a race

Triage a flake by forcing the state it needs and showing the unfixed tree fail with the original signature. Insert the stalls with a script you write for the purpose. It places each stall at an exact text anchor and fails when an anchor is missing. Run the same script on the fixed tree. A saved diff misapplies once the fix moves the lines.

### What a forced proof shows

- Prove a race between two production timers with an environment override that forces the losing order, before you spend a stress batch on it. A 0 ms `RIMZ_REMOTE_REACHABLE_RETRY_MS` made the unreachable-endpoint hold test fail every run on the unfixed tree, while 400 loaded copies showed that race 0 times.
- The forced run proves the cause and must pass on the fixed tree. The loaded batches are the regression check.
- A forced run with the original signature proves one sufficient cause, not the only one. Before fixing, list every instant at which the lost event (an exit status, a plan line, a write) can be dropped with the same outcome, and force each one. The fix passes under every hold. A kill inside the plan rewrite of the `ssh-trace` shim and a master exiting during the deadline tick's `-O check` both hung `remote_attach::supervised_master_deadline_kills_and_repaces_a_hung_attempt` with the same stderr, and a fix for the first alone left the second open.
- A forced run proves the cause only when it fails with the original failure's own signature: its `panicked at` line and printed values. A 0 ms `RIMZ_TEST_DAEMON_HOST_STOP_MS` still passed `daemon_content::daemon_host_follows_room_build_after_hangup` on its unfixed tree, because the child's HUP trap ran during the `/proc` walk between the SIGHUP and the SIGKILL. A delay before the trap's `>>` redirect failed it with `NotFound` where CI had read the file empty. Only a delay between the redirect's open and its write reproduced CI's `left: ""`.
- A forced run that expects more than one test to fail passes `--no-fail-fast`. `cargo xtask test` stops at the first failure and reports the rest as not run.
- Once a forced run fails on the base and passes on the fix, a base batch only shows whether loaded stress reproduces the flake at all. Run one, started above `nproc`. When it reads 0, run only the fixed batches after it and say the fix rests on the forced proof.
- Keep that one base batch even then. When the cause was found by elimination, a base batch that reproduces the flake is the only evidence that ties it to that cause.
- A proof that temporarily edits source awaits its test run to the end before reverting the edit: `cargo xtask test` builds what is on disk when it reaches the compile, and a cold or queued build can outlast a tool timeout.
- A forced run clears a sibling test only of the order its hold reaches. Before you clear one, name the process that races it and hold where that process can still act. A 1 s sleep in the check of a run, before that run's `wait` commit, passed `workflow::peers::a_lost_collection_reports_on_the_next_pass_once`, whose stray advance collects after that commit. A poll of up to 4 s before the `collect_run` of `workflow advance` failed it with `left: 1`, `right: 0`.

### Where to hold

- For a stale-read race, where a state read precedes the event it judges, hold immediately before the event read, never after the state read. The old order's state goes stale during the hold, while the fixed order reads both event and fresh state after release. A hold after the state read can be followed by another read that ends the loop before the event is read, as `run_command`'s post-forward `try_wait` can reap before its signal read.
- A timer override forces only a loser that stays lost. When the losing side recovers, as a failed `message deliver` requeues its message, hold it inside its window with a rendezvous such as `RIMZ_TEST_DELIVERY_AFTER_CLAIM`. Settle 0 plus a sleep failed 1 of the 5 queue-before-hook tests in `message.rs` on the unfixed tree, while holding the hook's deliver after its claim failed all 5.
- To hold a hook's own `message deliver`, set the rendezvous variable on the `hooks feed` command. The hook copies every `RIMZ_*` variable into the environment of its detached children.
- A deadline that opens at a `rimz` spawn and loses to the binary's own start-up under load is forced without a source edit: put a wrapper `rimz` that sleeps before its `exec` where `common::shim::cargo_bin` looks first ([Builds and wrapped binaries](./integration-tests.md#builds-and-wrapped-binaries)). A 2.5 s sleep failed every run of four `remote_attach` deadline tests on their unfixed tree, while loaded stress had shown three of them 0 in 800 copies each. A stress zero on a start-up race is therefore no baseline.
- When a wall-clock bound is a test's only evidence of an action (a kill, a wake), park what the action cuts short well past the bound. At a bound equal to the park, a lost action misses it by the few milliseconds the test's own polling decides.
- Prove such a bound by deleting the action and watching every run fail. With its kill deleted, `remote_attach::supervised_output_relay_failure_kills_ssh_and_never_reconnects` passed 10 of 40 runs with a 3 s park as its bound, and failed all 10 with a 10 s park against a 3 s bound.
- For a boundary send, `RIMZ_TEST_FRESH_SEND_BEFORE_CLAIM` holds only the path that `dispatch_decision` leaves unparked. A send already parked behind a queued, claimed, or unacknowledged `Sent` record enqueues through `dispatch_parked` and never reaches it.
- When a fix makes the losing order impossible (an early producer moved after the late one), hold the late producer, give the early one a bounded window to show itself, and assert at the consumer's claim rendezvous. The fixed tree pays the bound once and passes, and the old tree fails at the claim (`teams_flip::resumed_lazy_leader_gets_its_resume_prompt_then_its_stage`).
- A rendezvous hold (`testkit::rendezvous`, behind hold variables such as `RIMZ_TEST_WORKFLOW_RESULT_GATE`) panics the held process when no release byte arrives within 10 s of its connect. An overrun shows as `BrokenPipe` at the test's release write.
- A deadline that proves an action frees a rendezvous-held process stays below the hold's 10 s panic. Past it, the panic frees the process and the test passes with the action deleted (`hooks::drain::hung_apply_is_killed_and_the_next_frame_is_applied` passed with a 15 s read bound and its apply deadline removed).
- Keep the test's own `rimz` calls between accept and release few. Put a read the release does not need after the action the hold waits on, so that it overlaps the action instead of lengthening the hold (`workflow::signal_during_evaluation_rechecks_the_result_without_backoff`).
- Before writing a test or a hold for a race window, list the holds that exist (`rg -n 'testkit::rendezvous\(' crates/rimz/src`).
- For a hold at that window, read the test that sets its variable (`rg -n -F '<variable>' crates/rimz/tests`) against the bug: a test can hold the window and assert only what the broken order also prints. `workflow::a_show_whose_live_home_moves_under_it_reads_the_finished_home_whole` held `show --json` across the home's move and asserted the state and `dir` alone, so it passed while the output lacked `history`.
- A flake where a seeded record expires between two passes of a test (a fork count, a warm-cache assertion) is forced by a temporary hold between the passes, never by aging the record at seeding. The first pass refreshes an aged record and the second sees it fresh, so the aged run passes on the unfixed tree.
- A hold that proves a fix sits at the same place on the base and the fixed tree, where only the fix can save the run. A hold placed after the state the fix refreshes fails both trees and proves only the signature. An 11 s hold after `performance::produce_budget::warm_fleet_produce_forks_zero_subprocesses` republished its pane frame failed the base with `SessionNotFound`, but it also aged the frame that the fix hands to the kept producer. Only the same hold before the republish, which failed the base and passed the fix, showed that the kept producer never rereads the frame.

### Probes and port races

- A probe that wraps `rimz` does nothing before the command under suspicion: capture state after that command returns or alongside it, since a few tens of milliseconds of probe work can close the race being hunted (about 380 wrapped runs of one rebirth flake showed no failure for this reason). Show the probe reproducing the failure before using it to refute a cause, and count a known-positive baseline of an event before reporting it absent.
- Force a port race, where a test picks a port that a child binds later, without root or a source edit. Narrow the kernel's ephemeral range in a private network namespace, so that the kernel's own port-0 draws land on a few ports. This opens a shell in such a namespace:

  ```sh
  unshare -rn sh -c '
    sysctl -qw net.ipv4.ip_local_port_range="40000 40255"
    ip link set lo up
    exec "$SHELL"
  '
  ```

  In that shell, run the test binary with `--exact <module>::<test>` in a loop, one process per run as nextest does.
- Size the range at about five ports per run, because connections from earlier runs keep holding their ports after they close. At 64 ports the web daemon's own port-0 draw failed with `Address already in use` after 32 runs of `web::web_stop_stops_writable_and_broadcast_daemons`. At 256 ports, 50 runs held, and the fixture's old dropped port-0 pick failed 12 of 50.

## Reading CI evidence

- A nextest `FLAKY` result's failed tries and full output are the `<flakyFailure>` elements in `live/junit.xml` of the JUnit artifact (`nextest-junit-live` on GitHub, `nextest-junit` on Gitea).
- Before reading code for a CI-only flake, read the panic body of every failed try in the failed attempt's junit (fetched as the next bullet says). A retry that fails at a different assertion than the first try often settles a theory in one step. Each try's full output is in its `<failure>` and `<rerunFailure>` bodies, while the step log keeps only a truncated tail.
- `gh run download <run> -n nextest-junit-live` returns only the latest attempt, which is usually the passing one. List a run's attempts with `gh api repos/<owner>/<repo>/actions/runs/<run>/attempts/<n>` and fetch the failed attempt's report by artifact id. Fetch a failed attempt's log with `gh run view <run> --attempt <n> --job <id> --log > <file>`; the `gh api .../logs` route refuses its terminal escape codes. Without `--attempt`, `gh` serves the latest attempt's log even for an earlier attempt's job id, so a log that shows every test passing is the retry's.
- Before you list a live-test flake as possibly your branch's, read the `<flakyFailure>` and `<failure>` bodies in `live/junit.xml` of recent merge-queue runs (`gh run list -R rimio-ai/rimz -w ci.yml -e merge_group`) for the same `panicked at` line and message: a push to `main` after a green queue run runs no test ([GitHub suites](./build-and-ci.md#github-suites)). A match in a run that does not carry the branch puts the flake outside the change. The initial native-focus timeout of `backend::zellij::launch::open_sidebar_births_native_layout_and_template` (`timed out waiting for birth work pane`, raised by `AttachedClient::wait_for_stable_focus` in `backend/zellij/support/client.rs`) matched trunk CI run 41273037773.
- When the failing assertion reads an element by position (`events[0]`, the last row), first list which element each ordering of the race puts there: a missing field on that element is often another element in its place rather than a lost write. One timeout flake's missing script digest was the next check's evaluation sitting at `events[0]`, because the timeout had routed before the timed check ever evaluated.
- A failure that is fast in CI needs fast local runs. Compare the failing CI attempt's duration with one unloaded local run, and read `uptime` first. If the local run takes several times the CI duration, or the load average is far above `nproc` before you start, the loop measures the host: instrument the test and reproduce in CI instead.
- The failing attempt's duration also bounds which path failed. Subtract the test's own waits, and rule out each candidate path whose minimum time does not fit before you design a fix. An xtask reaper test that failed at 15.013 s against its 15 s post-drop poll left about 13 ms for the drop, which ruled out every path that spends the reaper's 2 s rescan or helper bound.

## Tracing a slow or idle command

### Find where a command idles

Find where a slow command idles before you read code.

1. Run the test binary (its suite's `binary-path` in `cargo xtask test --list <fragment> --message-format json`) under `strace -f -tt -s 4096 -e trace=execve,clone,clone3 -o <file> <bin> --exact <module>::<test> --test-threads=1`. The `clone` lines show which process spawned each child.
2. Keep the `execve`, `clone`, and `exited with` lines, and read the gaps between them to locate the suspect process.
3. Add `clock_nanosleep,poll` to the trace set and group its lines by thread id. A thread that repeats one short sleep for seconds is a poll, and its run length is the wait. `poll(2)` can hold a wait such as the DSR fence without a `clock_nanosleep` line.
4. Trace every syscall of the suspect process with `-ttt -T` to separate time inside a `poll` or sleep from time outside every syscall (CPU, runqueue, or page faults). Off-syscall time is work slowed by load, and is no designed wait.
5. Add `openat` to the trace set and keep its `ENOENT` lines to see what the idle process polls for.

- A gap that matches a production constant (a 25 ms poll for 1.5 s or 3 s) points at a designed wait the fixture never satisfies. A gap in the filtered trace of step 1 can be CPU as well as a wait, which is what steps 3 and 4 settle.
- Read the trace past the failing step, through the test's next read of state the traced processes write. A value written after the event the test waited on is a second race in the same test.
- Satisfy a designed wait the way the real writer would, and do not raise a timeout: seed the sidebar heartbeat a live room carries ([how](integration-tests.md#multiplexer-doubles-and-heartbeats)), or bind the child pane the way its hook does (`common::room` holds both, for Zellij and tmux).
- Under strace, stop the traced child by its own PID (`pgrep -P <strace pid>`), never strace's. Killing strace leaves the child running, and `wait` never returns.
- A one-shot `sidebar snapshot` or `frame` walks provider transcripts only when no live sidebar serves spending for the same `RIMZ_HOME` (any room on the machine counts) and no current-version `provider-spending.json` exists. It is slow only when `cache/providers/spending.json` is cold. `Env` isolates `HOME` and hides that cost, so reproduce with a scratch `RIMZ_HOME` and the real `HOME` before you assume a designed poll.
- A dead room needs its `workspace.json` for `--no-produce` to take the consumer read.

### Other traces

- A gap inside a copy is a filesystem wait, not a hang: agent `/tmp` on the dev hosts is ZFS with block cloning on, and coreutils `cp`, `install`, and `cat` clone a just-written file (`FICLONE`, `copy_file_range`), which waits for the pool's transaction-group sync, seconds under gate load. Add `ioctl,copy_file_range` to the trace set to see it. A shim that copies a fresh fixture file uses `dd`, which reads and writes, and product code that places a file it just wrote renames it within one directory.
- For a process-tree flake, first trace one run with `strace -f -tt -s 4096 -e trace=execve,clone3,wait4` (strace cuts each string at 32 bytes by default, which hides a long argv such as the session step's provider command), then widen the suspected child's lifetime with a PATH shim. The trace names the extra child; the shim tests whether its lifetime causes the consumer's failure.
- A rebirth that plans no resume for an agent prints nothing at the default log level. A skipped candidate gets a `not resumed:` line, and an agent that never became a candidate gets none.
- Run that `rimz start` with `RUST_LOG=warn,rimz::harness::rebirth=debug,rimz::cli::room=debug` (`journey::deep`'s `REBIRTH_TRACE`) and read its stderr. It names the birth path (session already live, or inspected with the candidate count) and the roster read (absent or unreadable, or its entry count and how many have an audit row). Per agent, it names why the agent is not a candidate (`ended`, `owner live` with the pid, `no audit row`) or why a candidate got no tab.
- Read room state only after that start, never between a teardown and it. A probe there delays the rebirth by 20 to 150 ms, enough to hide a race a fast run loses.
- `records/last-death.json` lists an agent whenever the roster named it, whatever the later gates decided.

### Rank tests by time

- `cargo xtask test` prints only its summary, which times the whole test. To rank tests before tracing, run `cargo nextest run -p rimz --test integration --status-level pass -E '<filter>' > /tmp/<file>.log 2>&1`, then sort its `PASS [ Ns ]` and `LEAK [ Ns ]` lines with `grep -E '(PASS|LEAK) \[' <log> | sort -t'[' -k2 -rn`. nextest marks a passing test that leaves a child holding its stdio as `LEAK`, which is most of this suite.
- For a serial shell loop, time with zsh `$EPOCHREALTIME` after `zmodload zsh/datetime`. Without the module loaded, it expands empty and every duration reads zero. The dev hosts lack `bc`.
- To report one command's duration inside a test across repeated runs (a load proof's slowest start), wrap the command in a temporary `std::time::Instant` that prints to stderr, and run `env -u ZELLIJ_SOCKET_DIR cargo nextest run -p rimz --test integration --status-level pass --success-output final -E 'test(=<module>::<test>)'`. Do not commit the timer.
