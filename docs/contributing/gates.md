# Gates and test profiles

The gate stack as reference: the composite tasks, each gate they run, the run budgets, the pre-commit hook, the test profiles, the environment a test run gets, the architectural invariants, and the compatibility surface. The everyday commands are in the root [AGENTS.md](../../AGENTS.md#testing). What the commands print and hide, and how to pick one test, is in [running-gates.md](./running-gates.md). The CI jobs that run these gates are in [build-and-ci.md](./build-and-ci.md#continuous-integration).

`cargo xtask <task>` is the entry point for contributor automation, and new automation lands in `xtask/`. `cargo xtask help` lists every task, and `cargo xtask help <task>` prints the command a task runs. CI runs every PR gate under `RUSTFLAGS=-D warnings`, except the plugin build, which sets its own flags. Each gate has a local `cargo xtask` equivalent except the four [cross-target checks](./running-gates.md#cross-target-checks), which call `cargo-zigbuild` directly.

## Composites

Four composites cover the everyday flows.

| Task | Steps, in order | Use it |
| --- | --- | --- |
| `gate` | `cargo fmt --all` (fix mode), `invariants`, the conform ratchet, `docs-links`, `loop-tests`, all-feature and install-host `lint`, `doc`, `cargo nextest run --profile gate --workspace --all-features --locked` | Before a PR or hand-off |
| `checks` | `fmt` (check mode), `invariants`, the conform ratchet, `docs-links`, `deps`, `build-plugin`, `plugin-provenance`, all-feature and install-host `lint`, `doc` | The non-test gates, without the two that talk to the crates.io registry |
| `externals` | `deny`, `vet`: the gates that talk to the crates.io registry | When dependencies change |
| `ci` | `checks`, `loop-tests`, `cargo nextest run --workspace --all-features --locked` | When a change calls for full validation |

A new gate task is documented in two places: the row of each composite that runs it, and [individual gates](#individual-gates).

### `gate`

- It prints one compact line per passing step and fails fast with a trimmed excerpt and a `NEXT:` hint. `check`, the clippy passes of `lint`, and a captured `test` run report a failure in the same form.
- A test step that passed only on retry adds nextest's `FLAKY` line for each flaky test.
- `--check` verifies formatting with `cargo fmt --all -- --check` and leaves the tree unmodified.
- `--keep-going` runs every step and ends with a `gate: fail (<steps>)` line that names each failed step. The lint step runs all three clippy passes under cargo's own `--keep-going`, so one crate's compile failure does not hide another's. The test step adds nextest's `--no-fail-fast`.
- A step that cannot run at all (a spawn error, a budget overrun) ends the gate at once, with or without `--keep-going`.
- The two flags combine in either order.

### `checks`

- The steps are ordered for speed. The instant text gates (`fmt`, `invariants`, the conform ratchet, `docs-links`) run first and fail fast.
- `deps` runs on its own thread beside the compile gates.
- The compile gates (`build-plugin`, its `plugin-provenance` byte comparison, `lint`, `doc`) run one after another, because concurrent cargo builds serialize on the target-dir lock.
- Outside CI, `plugin-provenance` skips the byte comparison when the local rustc differs from the one the vendored plugin records.
- It prints a per-gate timing summary to stderr.

`cargo xtask check` (singular) is a different task: the fast `cargo check --workspace --all-targets --all-features --locked` compile pass, for early iteration.

### `externals`

Both gates run, so one pass reports every signal. A vet exemption in `supply-chain/config.toml` names one exact version. A lockfile bump of an exempted crate therefore fails `vet` as an unvetted dependency until its `[[exemptions.<crate>]]` version moves to the new locked one.

### Past `gate`

Escalate when the change touches the matching surface:

- `cargo xtask test -P live` for the live-backend tests and deep mux smokes.
- `cargo xtask test -P journey` for rendered journeys.
- `cargo xtask externals` when dependencies change.
- `cargo xtask perf` for the benches and their regression ceilings.
- `cargo xtask ci` for the checks and the full suite.

After a change to the presence plugin, run `cargo xtask plugin-refresh` before a live Zellij verification. The `backend::zellij::presence::` tests that load `presence_wasm_artifact` read the vendored `crates/rimz/presence/rimz-presence-zellij.wasm`. `cargo xtask build-plugin` compiles the plugin and leaves that file as it is.

## Run budgets

Two wall clocks bound a run, so a wedged compile, test process, or multiplexer costs a bounded wait.

Every task carries a budget armed at startup ([xtask/src/deadline.rs](../../xtask/src/deadline.rs)). xtask checks it while it waits on a child it started through its bounded runner. `semver` waits on `cargo semver-checks` outside that runner, so no budget ends it.

| Budget | Tasks |
| --- | --- |
| 45 minutes | `ci`, `checks`, `lint`, `test`, `stress`, `test-archive`, `coverage`, `perf`, `semver`, `dist`, `install`, `install-dev`, `install-system`, `stage-install`, `profile-build` |
| None | `sandbox`, `screenshot` (the operator drives them) |
| 15 minutes | Every other task, `gate` included |

On overrun the running child gets `SIGTERM`, then `SIGKILL` five seconds later, and the task fails naming the command and its elapsed time. A captured step also prints the trimmed output it collected. `RIMZ_XTASK_TIMEOUT` sets the budget for one run: `45m`, `900s`, a bare `900` for seconds, or `off` to lift it. Use it when a cold tree needs a full compile inside the gate.

One test is bounded by `slow-timeout` in [.config/nextest.toml](../../.config/nextest.toml), which warns every two minutes and terminates the test at six. A hung test then fails by name in the run summary. In the `gate` and `live` profiles it counts toward the three-failure stop like any other failure. The suite's slowest legitimate waits are the live spawn and lock deadlines, at 150 s and 120 s.

## Individual gates

| Task | Command | Checks |
| --- | --- | --- |
| `fmt` | `cargo fmt --all -- --check` | Formatting |
| `lint` | `cargo fmt --all -- --check`, then `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | Formatting and clippy, with `testkit` on |
| `check` | `cargo check --workspace --all-targets --all-features --locked` | That the workspace compiles |
| `test` | `cargo nextest run --workspace --all-features --locked` | The suite ([the test runner](./running-gates.md#the-test-runner)) |
| `test-archive` | `cargo nextest archive --workspace --all-features --locked --archive-file <path>` | Packages the workspace test binaries for portable execution. Reserve it, with partitioned execution, for large CI suites |
| `docs-links` | (xtask) | Every relative inline link in a tracked Markdown file resolves to a file in the working tree, and its `#anchor` to a heading. Offline and deterministic. Reference-style links and external URLs are out of scope |
| `loop-tests` | `uv run --script <file>` for each `loops/*/tests/test_*.py`, from the repository root | The repository loop tests. A failing file, no matching file, or a missing `uv` fails it |
| `doc` | `cargo doc --no-deps --workspace --all-features --locked` with `RUSTDOCFLAGS="-D warnings"` | Rustdoc warnings, broken and private intra-doc links included |
| `invariants` | (xtask) | The [architectural invariants](#architectural-invariants) |
| `deps` | `cargo machete` | Unused dependencies |
| `deny` | `cargo deny check -D warnings` | Licenses, advisories, bans, yanked crates |
| `vet` | `cargo vet --locked` | The supply-chain audit. It fetches the crates.io index directly, bypassing the runners' nexus mirror |
| `semver` | `cargo semver-checks` | Rust API drift against the published baseline. Advisory |
| `coverage` | `cargo llvm-cov clean --workspace`, then `cargo llvm-cov nextest --lcov --output-path target/ci/coverage/lcov.info --workspace --all-features --locked` | Instrumented coverage |

### Formatting

`rustfmt.toml` sets no `wrap_comments`, so wrap comments by hand ([AGENTS.md](../../AGENTS.md#engineering-principles)). Wrap by hand also a macro call rustfmt cannot parse (tracing's `%field` and `?field` sigils, an `if` inside `json!`), to the width of the code around it.

rustfmt can leave a whole expression as written when it cannot fit some part of it within `max_width`. This is often a closure passed as an argument, and `cargo fmt --check` and `cargo xtask lint` still pass. When the layout of new code looks hand-made, indent one statement in a scratch copy and run rustfmt. If the indent stays, split the body into named functions until rustfmt restores it.

### Lint

`gate` and `checks` add two clippy passes that lint the `rimz` host without `testkit`, one per install feature shape: default for `cargo xtask install`, and `sentry` for `cargo xtask install-dev`. The host passes expose dead code that test-only references can mask. Each re-checks the whole `rimz` crate, so the per-commit `lint` leaves them to the pre-PR gate.

### Loop tests

`loop-tests` needs `uv` ([install uv](https://docs.astral.sh/uv/getting-started/installation/)) and runs in `gate` and `ci`. Each repository loop task owns a directory under [loops/](../../loops/README.md), which holds the layout contract, and its schedule is declared in `.rimz/config.toml`. The [Dependabot loop runbook](./dependabot-loop.md) owns the loop that launches a worktree-isolated `spot` team for failed Dependabot updates.

### Rustdoc

The `doc` task replaces inherited rustdoc flags, `CARGO_ENCODED_RUSTDOCFLAGS` included, so they cannot weaken the gate. A link to a `#[doc(hidden)]` item emits neither a page nor a warning: use plain backticks for it, and inspect the rendered docs when you change a reference to one.

### Deny in CI

In CI `deny` runs offline: `RIMZ_DENY_OFFLINE=1` adds the global `--offline` option, and the check reads the image's baked advisory DB and a local crates.io index prepared at the canonical cache path.

### Semver checks

`cargo semver-checks` is in no gate and no CI job, and it never blocks a change. RimZ ships as a binary. The `rimz` lib target exists so the binary, tests, and benches can link the domain modules, and its Rust API carries no compatibility promise ([lib.rs](../../crates/rimz/src/lib.rs) states this). The check fires on internal refactors (a renamed error-enum field reads as a major break) and does not see the [compatibility surface](#the-compatibility-surface). Run it when a release note wants the API delta. It skips while the workspace version is `0.0.0` or while crates.io has no published `rimz` baseline.

### Coverage

The nightly and dispatch `coverage.yml` workflow runs `coverage` off the PR path and uploads `target/ci/coverage/lcov.info`. The CI image provides tmux and Zellij, so the same pass exercises the live-backend tests.

## The pre-commit hook

Run `cargo xtask hooks` once per clone. It points `core.hooksPath` at `.githooks/`, whose `pre-commit` shim is the only tracked hook script. The shim builds xtask, then runs four checks:

1. `cargo xtask fmt --staged`
2. `cargo xtask invariants`
3. `cargo xtask atlas conform --ratchet`
4. `cargo xtask docs-links`

The shim builds xtask first because the `cargo xtask` alias runs cargo quiet. A failed build prints cargo's output and exit status, and no check runs.

The formatting step judges only the staged index copies of Rust files, without following child modules. On refusal it formats the working copies of the refused files, unstaged hunks included, and aborts without re-staging. Review the diff, re-stage, and commit again. A working copy that is already formatted needs only re-staging. A missing or unparsable one is left alone, with the reason reported.

Plain `cargo xtask fmt`, `lint`, `checks`, and `gate` keep their whole-tree formatting scope. The slow gates (`check`, `lint`, `test`) stay out of the hook. `git commit --no-verify` bypasses it for one commit. CI is the authoritative gate, and the hook is fast local feedback.

## Test profiles

Three named profiles in `.config/nextest.toml` partition the suite. A run with no `-P` (`cargo xtask test`, `cargo xtask ci`) uses nextest's default profile, which stops at the first failure.

| Profile | Holds | Run by |
| --- | --- | --- |
| `gate` | Deterministic non-live tests | `cargo xtask gate` |
| `live` | The mux backend-parity suites, the real-browser web suite, deep mux smokes | `cargo xtask test -P live` |
| `journey` | Non-deep rendered journeys | `cargo xtask test -P journey` |

The name filters in `.config/nextest.toml` set a test's profile, and neither its directory nor a self-skip does. A test outside `backend::` and `journey::` that drives tmux and self-skips without it (`which::which("tmux")`) runs in `gate`. To find a test's profile, match the exact name from `cargo xtask test --list <fragment>` against those filters. The shape each kind of test takes is in [rust-conventions.md](./rust-conventions.md#tests).

## Test environment

Each xtask invocation builds a sandbox for the suite before it starts a test process:

- It replaces HOME and every XDG root with disposable directories, and gives tmux and Zellij private server namespaces.
- It mirrors its RimZ and XDG roots into `RIMZ_UNIT_TEST_RIMZ_HOME`, `RIMZ_UNIT_TEST_XDG_CONFIG_HOME`, `RIMZ_UNIT_TEST_XDG_DATA_HOME`, `RIMZ_UNIT_TEST_XDG_CACHE_HOME`, `RIMZ_UNIT_TEST_XDG_STATE_HOME`, and `RIMZ_UNIT_TEST_XDG_RUNTIME_DIR`. Library unit tests read only these keys and ignore the production root keys. With none set, a unit test uses per-process temp roots.
- It points `RIMZ_CODEX_BIN` at a path it never creates, so a test process and every session it births in-process read codex as not installed.
- It exports `RIMZ_PRICING_OFFLINE=1`, which keeps in-process spending walks off live pricing lists. A test that needs prices seeds its own fixture cache.
- It points `RIMZ_TEST_PRESSURE_ROOT` at a missing directory, so no test reads the host's PSI. A pressure test supplies its own fixture root.
- It drops `NO_COLOR` and the ambient room session keys (`RIMZ_*` except build and test inputs, `TMUX*`, `ZELLIJ*`, and provider homes), and keeps the keys the sandbox sets.
- It removes git's repository-local variables (those `git rev-parse --local-env-vars` lists) from test processes and loop tests. This keeps fixture git inside its fixtures under `git rebase --exec` in a linked worktree.
- It pins `CARGO_HOME` and `RUSTUP_HOME` to their host values before it replaces HOME, or to the host HOME's `.cargo` and `.rustup` when they are unset, so cargo and rustup keep the host toolchain.

A bare `cargo nextest run` skips this layer, and still inherits git's local variables.

Inside the sandbox every `common::Env` and live `ZellijNamespace` owns narrower per-test roots and an independent stdin-keepalive reaper. On orderly drop and on abrupt death of the test owner alike, the reaper stops the private mux endpoints, terminates the marker-carrying descendants, and removes the roots. That fixture layer also works under bare nextest, and the xtask layer is the invocation-wide backstop.

## Architectural invariants

`cargo xtask invariants` is a grep-and-shape gate over the tracked tree. Its matches are shallow strings, so it is a cheap trip-wire beside the type system and review, never a substitute for either. It guards boundaries the compiler can't see: hook-stdout decision-channel integrity, sidebar/store import separation, spend-parser and diagnostic-write confinement, fsync calls staying in `disk/atomic.rs`, pane-primitive use, the render snapshot clock, the sidebar image-permit mint, [UI-color provenance](../guide/theme.md#color-slots), CLI color-carrier provenance, single-home provider-brand resolution, glyph provenance, vendored presence-plugin freshness, the inline-test size gate, and the deep-journey backend prefix that schedules a test in its live-mux group. The authoritative set is the `ensure_*` list in [xtask/src/invariants.rs](../../xtask/src/invariants.rs). A new boundary lands there as an `ensure_*` check with a self-test.

A boundary on who may construct a value (a permit, a grant, a proof that a check ran) is held by the compiler before any grep. Give the type a file to itself and keep its field private: a module and every module under it can write a private-field literal, so a type declared in a `mod.rs` can be forged from each child. The `ensure_*` check is then needed only for what the compiler cannot see, the call sites of the named constructor. `ImagePermit` in [pixel/permit.rs](../../crates/rimz/src/sidebar_pane/pixel/permit.rs) is the shape, with `ensure_image_permit_minted_only_by_host` holding its one mint.

## The conform ratchet

`cargo xtask atlas conform --ratchet` is a gate of its own, outside `invariants` ([atlas.md](./atlas.md#conform--keep-the-target) is its reference). It reads target schema v5 from the root `refactor-target.toml` and fails when dependency sites, escaping surface, or strangler counts exceed the checked-in constraints. It also fails on a target file it cannot parse and on a configured path that does not exist. It is inert when that file is absent. `--tighten` only lowers baselines and removes unused dependency admissions, and raising one is a deliberate edit of the target file.

Neither `check` nor `lint` sees it, so a change that passes both can still fail `cargo xtask gate` at its `conform` step. Run it as part of the per-commit checks whenever a change adds an item visible outside its module, a module, or an import of one module from another. `pub(crate)` counts the same as `pub`, so narrowing visibility does not clear the gate, though it is still the right visibility to pick. How to admit a new item is in [the surface ratchet](./running-gates.md#the-surface-ratchet).

## The compatibility surface

RimZ ships as a binary, so the surface that carries a compatibility promise is the one a user or a script touches: commands and flags, `--json` output, exit codes, config keys, and persisted formats. [CHANGELOG.md](../../CHANGELOG.md) states the terms: on the 0.x line these can change between releases, and a release's "Changed" section announces a break. The `rimz` crate's Rust API is not part of that promise (`cargo semver-checks` under [individual gates](#individual-gates)).

Three checks hold that surface, all in the normal test run:

- Flags: [crates/rimz/src/cli/surface_tests.rs](../../crates/rimz/src/cli/surface_tests.rs) snapshots the built clap tree: every command with its aliases, and every flag with its long aliases, arity, defaults, accepted value names, and global/hidden status. It holds no help prose, so wording churn stays out of the diff. A failure is the review prompt. Adding a command or an optional flag is additive. Renaming or removing a flag, narrowing a value set, or changing a default breaks callers and earns a CHANGELOG entry. Accept a reviewed diff with `cargo insta accept`.
- Commands: `visible_commands_are_grouped_exactly_once` in [crates/rimz/src/cli/help.rs](../../crates/rimz/src/cli/help.rs) fails when a visible subcommand appears or disappears without moving its help group.
- Persisted and wire formats: versioned formats carry their own version constants (`rimz.event.v2`, `rimz.plugin.v6`, `rimz.web.v2`, `SNAPSHOT_VERSION`, and peers), independent of the crate version. A reader refuses a record of another version and does not migrate it. A new shared machine-wide cache that rejects other versions puts its format version in its file name (`rate_limits.v<N>.json`), so builds on different versions keep separate files. `spending.json`, `provider-spending.json`, and `model_catalog.<login>.json` are the exceptions: they reject other versions under fixed names, so builds on different versions contend over one file there. The unversioned `credits.json` cache accepts additive, defaulted optional fields. The doctor integration suite pins the event-log and sidebar-plugin versions as literals, so a bump there fails a test that names the old value.

Shared files that builds of different ages rewrite whole (read, change one entry, write all), such as provider caches under `~/.rimz/cache/providers/`, lose the fields an older build does not know on every write by that build, not only at upgrade. A new optional field's absence must preserve the behavior the file had before the field existed.
