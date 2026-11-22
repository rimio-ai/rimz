# Build, CI, and release

The toolchain a build needs, the compile cache, contributor installs, target pruning, the CI pipelines and their image, and release packaging. Read the section you are changing, or the toolchain and cache sections when you set up a machine. The gates themselves are in [gates.md](./gates.md).

## Toolchain

Rust 1.97.1 is pinned exactly in `rust-toolchain.toml`. The workspace is edition 2024 and declares Rust 1.89 as its installable-crate floor. rustup provisions everything below from the pin, and `ci/Dockerfile` installs the CI toolchain from that same file.

- Components: `rustfmt`, `clippy`, `llvm-tools-preview`, `rust-analyzer`.
- Targets: `wasm32-wasip1` (the Zellij presence plugin), `aarch64-apple-darwin` and `x86_64-apple-darwin` (the [release cross-builds](#release-packaging)), and `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-gnu` (CI's [cross-target type-checks](./running-gates.md#cross-target-checks)).

The pin, the vendored presence wasm, and its provenance sidecar move together: every toolchain bump runs `cargo xtask plugin-refresh` in the same commit.

Node.js 26 or newer is a development prerequisite for the gate-tier ttyd pixel-layer harness. The CI image supplies the same major version, and the integration test fails at entry with the install requirement when `node` is missing or older.

Repo-local Cargo config stays installation-safe: `.cargo/config.toml` defines only the `xtask` alias, so source installs use each host's platform linker. CI provides `mold` on Linux through the `rimz-ci` image and runs link-heavy compile/test gates through `mold -run`; contributors may opt into mold in their user Cargo config for faster relinks of the link-heavy integration-test binary.

### Compile cache

`sccache` (`cargo install sccache --locked`) is a content-addressed compile cache shared across checkouts and worktrees.

- xtask detects it on `PATH` and sets `RUSTC_WRAPPER=sccache` for the Cargo compile commands it launches. It leaves `CARGO_INCREMENTAL` to the caller's environment and Cargo config.
- To cover direct Cargo commands and the initial `cargo xtask` build, put `rustc-wrapper = "sccache"` under `[build]` in `~/.cargo/config.toml`. Leave `incremental` unset there: `incremental = false` disables incremental rebuilds of workspace crates for every command.
- `RIMZ_SCCACHE=off` routes xtask's compiles around sccache. `RIMZ_SCCACHE=on` requests cache routing and warns when `sccache` is missing.
- `sccache --show-stats` reports cache effectiveness.
- Builds in a scoped pane need sccache 0.17 or later, whose client-side mode the view pins ([toolchain homes](../internals/sandbox.md#toolchain-homes)).

Cargo compiles only workspace crates incrementally. sccache therefore keeps caching registry dependencies across worktrees, while `rimz` and the other workspace crates rebuild incrementally. sccache passes incremental invocations through uncached, which costs nothing: the `rimz` leaf crate almost never hits across branches and worktree paths. Incremental state adds roughly 5-10 GB per active worktree `target/`. CI sets `CARGO_INCREMENTAL=0` and stays non-incremental.

The byte-provenance presence-plugin build is the exception. It bypasses compiler wrappers, and it normalizes Cargo's registry-source cache keys and the standard-library source root that an installed `rust-src` displaces spans onto. Shared cache history, an exact crates.io mirror, and an optional toolchain component therefore cannot affect the vendored artifact.

Give every divergent Git worktree its own Cargo `target/` directory and let sccache reuse the compiler outputs. Cargo target directories contain branch-specific fingerprints, linked executables, and unhashed top-level binaries, so symlink-sharing `target/` through `.worktreelink` lets one worktree overwrite another worktree's runnable artifact while Cargo still considers its own fingerprint fresh. Reclaim inactive worktree targets with the worktree itself; the bounded shared sccache makes the next build warm without retaining every materialized target tree.

### Version stamp

Xtask host builds pass the semantic Git version through the build environment, so a revision, release tag, or clean/dirty transition invalidates the large `rimz` leaf crate. Every other Git-checkout build reruns the version stamp when the worktree's HEAD moves to another commit or branch, and not when git rewrites the index or runs `gc --auto`, so its `.dirty` suffix reflects the tree at the last HEAD move. The exception is a packed or unborn checked-out branch, or a reftable repository: there the stamp follows the index, so index rewrites rerun it. Packing the checked-out branch's ref reruns it once and leaves it in that state until the branch's next commit.

## Contributor installs

- `cargo xtask install-dev` is the contributor opt-in to [off-box reporting](../internals/diagnostics.md#off-box-error-reporting): it installs the optimized `profiling` host profile with `--features sentry`, line tables, frame pointers, and v0 symbol names, so dogfooding sessions stay profilable and default to the `development` Sentry environment.
- `cargo xtask install-system` builds a release binary as your user and uses sudo only to atomically install it at `/usr/local/bin/rimz` for all users, with executable permissions. `--dev` gives the same profiling/sentry build and debug-file upload as `install-dev`.
- Run either form without a `sudo` prefix. Before building, it checks sudo access by creating `/usr/local/bin` if missing, honoring the normal command-specific password policy.
- The installed file is a standalone copy: rerun the system command to update it. `install` and `install-dev` update only `~/.cargo/bin`, which takes precedence if it appears earlier on your `PATH`.
- `cargo xtask profile-build` writes the same optimized `target/profiling/rimz` without installing it.

## Target pruning

`cargo xtask prune [--dry-run]` reclaims stale output in the worktree's Cargo target directory and reports file and byte totals. It uses `CARGO_TARGET_DIR` when set (relative paths resolve from the workspace root), otherwise `target/`, including a symlinked target. Dry runs leave files and the daily stamp unchanged. A manual prune ignores and refreshes the stamp.

Successful `check`, `test`, `lint`, `gate`, `checks`, `ci`, `build`, and `test-archive` tasks attempt the same cleanup at most once per 24 hours. The stamp is refreshed before cleanup so failures do not trigger repeated attempts; errors warn without failing the task. Automatic cleanup is silent unless it removes files or encounters an error.

| Where | Removed | Kept |
| --- | --- | --- |
| `deps/`, `build/`, and `.fingerprint/` of each profile: names of the form `<stem>-<16 lowercase hex>`, with optional extensions, grouped by stem and hash | A hash older than 14 days that predates the stem's newest by more than a day. Also, whole, a hash that holds an extensionless Unix executable once it is older than one day and below the newest three such hashes | The newest hash of each stem. Variants built together, such as a library beside its build script |
| `incremental/` of each profile: direct child directories named `<stem>-<13 lowercase base36>` | A directory whose newest entry is at least 3 days old, with no superseding unit required | |
| Other target directories with `.fingerprint/` at depth one or two | The whole directory, when all its contents are older than 14 days | `debug/`, and directories without Cargo output such as `dist/`, `xtask/`, and `ci/` |

- A hash's age is its newest entry or descendant modification time, never access time.
- `.d`-only hashes that check builds leave under the same stem do not count toward the newest-three rank.
- Unrecognized names, non-directories under `incremental/`, and symlinks are left alone.
- Cleanup holds Cargo's `.cargo-lock` for each profile it touches. A busy profile, or one missing its lock file, is skipped, and its enclosing directory is not removed.

## Continuous integration

CI lives in two workflow trees: `.github/workflows/` for the GitHub origin and `.gitea/workflows/` for the Gitea mirror. Both run the same gates inside the `rimz-ci` image.

- GitHub pulls `ghcr.io/<owner>/rimz-ci:latest` with the built-in `GITHUB_TOKEN`. A manual dispatch of `ci.yml` can name another tag through `image_tag`.
- Gitea pulls the configured `RIMZ_CI_IMAGE` with its registry token.
- The workflows both trees carry (`ci.yml`, `ci-image.yml`, `coverage.yml`, `release.yml`) differ on purpose in their triggers. GitHub alone runs a merge queue, so only its `ci.yml` triggers on `merge_group`. Only GitHub's `ci-image.yml` runs on push and pull request, while Gitea's runs on dispatch or its weekly schedule ([CI image](#ci-image)). Only GitHub's `ci.yml` splits the gate tier into two runs, since its thread count is sized for GitHub's 4-vCPU runner and the mirror's runner size differs, and only it builds the gate tier once into an archive and runs it in slices across jobs. Only GitHub's `ci.yml` splits the `checks` and `tests` groups across more jobs, scopes its jobs by event ([GitHub suites](#github-suites)), and fixes the build version stamp; the Gitea mirror runs every group as one job on every event.

Both pipelines run three job groups in parallel. On GitHub, `checks` and `tests` are rollup jobs over the jobs that do the group's work, each of which compiles for itself (the `gate-tests` slices run from the archive `gate-archive` built), and a rollup passes when every job it waits on passed or was skipped for the event ([GitHub suites](#github-suites)):

- `checks`: `cargo xtask checks`, then the four [cross-target type-checks](./running-gates.md#cross-target-checks), in the order that section lists them. On GitHub the `xtask-checks` job runs `cargo xtask checks` and the `cross-targets` job runs the type-checks. The two macOS runs do not replace each other: the crate's self dev-dependency builds the test targets with `testkit` on. The Release cross-build is the only other Darwin compile. No other CI job compiles the two Linux targets, whose users take the installer's `cargo install` route. The only other aarch64 Linux compile is the arm64 leg of GitHub's Docker image build (`docker-image.yml`), which runs at release.
- `externals`: the `deny` and `vet` gates as separate steps (locally: `cargo xtask externals`). They sit apart from `checks` because deny reads the baked advisory DB offline while vet fetches crates.io directly, bypassing the runners' nexus mirror, so transient egress retries stay out of the main jobs.
- `tests`: `cargo xtask loop-tests`, then one compile of the Rust suite (`cargo xtask test --no-run`), then the `gate`, `live`, and `journey` nextest profiles from that one build, so each tier's timing reflects test execution rather than the shared compile. On GitHub the `gate-archive` job builds the test binaries into a nextest archive (`cargo xtask test-archive`) and uploads it as the `nextest-archive` artifact, each of the three `gate-tests` jobs runs one slice of the `gate` tier from it (`--archive-file`, `--partition count:<k>/3`), and the `live-tests` job runs `cargo xtask loop-tests`, then compiles and runs `live` and `journey`. On GitHub each slice of the `gate` tier is two runs of the profile: every binary but the integration binary (`-E 'not binary_id(=rimz::integration)'`) at nextest's default of one test per core, then the integration binary with `--test-threads 16`, since at one test per core its tests leave the 4-vCPU runner about half idle, waiting on child processes and deadlines. The first run's report moves to `target/nextest/gate-units/junit.xml` before the second writes its own. A gate test that fails under that overlap is isolated by a `[[profile.gate.overrides]]` entry in `.config/nextest.toml`, never retried.

Each test job runs its profile steps in one container, with profile-defined terse reporting and a three-failure cap. Default success semantics stop the job at the first failed step and leave its final failure recap at the log tail.

On GitHub each profile step passes `--deny-skips`, so a test that self-skips for a reason outside `.config/allowed-test-skips.txt` fails its tier. A capability the image lost is restored in `ci/Dockerfile`, and the allow-list holds only what the job container cannot provide. The Gitea steps report skips without failing on them.

GitHub's `main` ruleset requires `checks`, `externals`, and `tests`, which on GitHub are the two rollups and the `externals` job. On Gitea, the `tests` job is the branch-protection check.

The `live` profile runs both mux backends in one nextest process, so tmux and Zellij co-schedule, while the per-backend `[test-groups]` in `.config/nextest.toml` bound concurrency. Workflows leave nextest's global thread count uncapped for the `live` and `journey` tiers on both forges, so those test-group caps are the concurrency control there. GitHub jobs run on `ubuntu-latest` and Gitea jobs on the mirror's own runners.

The `live` and `journey` tiers retry, so a green run can hide a flaky test's failed attempts. Each tier, `gate` included, writes a JUnit report that keeps every failed attempt's output and every test's duration, and the reports are uploaded even when the tier passes: as the `nextest-junit` artifact on Gitea, and on GitHub as `nextest-junit-gate-<k>` from each `gate-tests` slice and `nextest-junit-live` from `live-tests` ([reading CI evidence](./flakes.md#reading-ci-evidence)). nextest writes each report to `target/nextest/<profile>/junit.xml` under the workspace root, not under `CARGO_TARGET_DIR`, so the upload reads from there although CI builds into `target/ci`.

Every tier runs against a checkout at the same container path the suite was compiled from, so fixtures referenced through `env!("CARGO_MANIFEST_DIR")` resolve.

Compile jobs route through `sccache`. On GitHub the backend is the Actions cache. On the Gitea mirror it comes from the runner-provided environment (S3 on the current runners).

- Both `ci.yml` workflows set `CARGO_PROFILE_DEV_DEBUG=false`, which overrides the `[profile.dev]` debuginfo setting in `Cargo.toml` for CI alone. The `test` profile inherits it, so the xtask self-build and the test build both compile without debuginfo. A CI backtrace therefore names functions without file and line, while a panic message keeps its location. Local builds keep `line-tables-only`.
- On both forges the cargo registry and index are baked into the `rimz-ci` image, and no CI job restores a registry cache. Cargo fetches only what `Cargo.lock` gained since the image was built, so a large lockfile change fetches more on every run until the next image build.
- GitHub's `gate-archive` and `live-tests` jobs set `RIMZ_BUILD_VERSION_OVERRIDE` to the crate version followed by `+ci` (`0.4.3+ci`), so a commit that leaves the crate's sources alone presents the same `rimz` inputs to sccache as the commit before it. sccache does not cache the test and binary targets (non-cacheable by crate type), so only the `rimz` library build is reused. Without it, `crates/rimz/build.rs` stamps `+g<sha>` from git, and every commit changes the `rimz` crate's inputs.
- The coverage and release workflows keep their `rust-cache` step.
- PR workflows do not cache `target/ci`. Target-dir fingerprints are not content-addressed and are a cache-poisoning risk on public PRs, while `sccache` keys compiler outputs by content and compiler identity.

A PR's `sccache` entries are written on its merge ref, which no other ref can read, so they count against the repository's Actions cache quota until evicted. `.github/workflows/cache-cleanup.yml` deletes them when the PR closes: it runs on `pull_request_target` so a fork PR's run holds a token that can delete, with `actions: write` as its only permission, no checkout, and the PR number as the only event data it reads. It deletes exactly the entries on `refs/pull/<number>/merge`; a PR with none succeeds, and a failed list or delete fails the run. That event runs the workflow from the base branch, so a change to the file takes effect for PRs closed after it merges.

The merge queue's runs write their entries on their `gh-readonly-queue/main/pr-<number>-<sha>` branches, which GitHub deletes once the group merges or leaves the queue. An hourly schedule in the same workflow lists the cache entries on `gh-readonly-queue/` refs, then the queue branches that still exist, and deletes the entries of each branch that is gone. It lists the entries first, so an entry it deletes belonged to a branch that had already been created and was gone by the second read.

### GitHub suites

GitHub's `ci.yml` starts with a `scope` job that picks how much of the suite the run takes, by event:

- A pull request runs the `pr-checks` job (`cargo xtask fmt`, then `cargo xtask invariants`) and `externals`. The `checks` and `tests` rollups pass for the jobs it skips, so lint, doc, the cross-target checks, and every test tier run first in the merge queue.
- The merge queue (`merge_group`) and a manual dispatch run every job but `pr-checks`, whose two gates `cargo xtask checks` runs.
- A push to `main` looks up a successful `merge_group` run of `ci.yml` for its commit. The queue fast-forwards `main` to the commit it tested, and both runs share the workflow concurrency group keyed on the commit, so the push run starts after the queue run ends. With such a run, the push runs `externals`, `xtask-checks`, `cross-targets`, and the build of `gate-archive` without its upload, and no test: the queue's later runs read those builds from `main`'s sccache entries, which every ref can read. Without one (a push that bypassed the queue, or a lookup that failed), it runs every job the queue runs.

A landed commit's test reports are therefore on its `merge_group` run, not its push run.

### CI image

The `rimz-ci` image bakes Node for Actions, the pinned Rust toolchain with required components and targets, the cargo gate plugins, `cargo-vet`, `sccache`, a warm RustSec advisory database, a warm cargo registry, tmux, Zellij, bubblewrap, mold, uv, Python, `cargo-zigbuild`, Zig, `rcodesign`, and `gh`. Tool versions live in `ci/Dockerfile`, the single source of truth for containerized CI and release jobs. The Rust toolchain is the exception, read from `rust-toolchain.toml`. The image keeps a copy of that file at `/` and sets no default toolchain, so rustup resolves the pin in every directory and a checkout's own file takes precedence.

The base is the slim Node image and rustup runs with the `minimal` profile, so the tools and components CI relies on are selected explicitly: an OS package a gate or test spawns is named in the Dockerfile's apt layer, and a Rust component in the pin file. Apt dependencies and the profile's own defaults still come along with them.

On GitHub, `ci-image.yml` builds `ci/Dockerfile` on `ubuntu-latest`, pushes both an immutable `rimz-ci:<tag>` and `rimz-ci:latest` to GHCR, and uses only the workflow `GITHUB_TOKEN` with `packages: write`. A weekly schedule (Mondays, 04:00 UTC) rebuilds the image so the baked RustSec advisory DB and cargo registry stay warm. Manual dispatch covers immediate refreshes and takes an optional `tag`. A push to `main` that touches `ci/Dockerfile` or `rust-toolchain.toml` rebuilds the image too. Between a channel bump or an added component or target in the pin and that rebuild, jobs fall back to rustup installing the missing toolchain or components in their first cargo step. Every build, the pull-request validation build included, writes the image's total unpacked size and a per-layer size table from `docker history` to the run summary.

To try an image from a branch before `latest` moves:

1. Dispatch `ci-image.yml` from the branch with `tag=<tag>` and `tag_only=true`. It pushes only `rimz-ci:<tag>`, and it refuses `latest` as that tag.
2. Dispatch `ci.yml` with `image_tag=<tag>`, so its jobs run in that image.

Every other event, and a dispatch that omits `image_tag`, runs in `latest`. The pull-request and merge-queue runs therefore need `latest` to carry any additional tools or system libraries the branch depends on, while rustup can supply missing pinned Rust components and targets. `container.image` reads `inputs` but not `env`, which is why the tag is a dispatch input.

When the repository opens to fork PRs, make the GHCR package public so unaffiliated forks can pull `ghcr.io/<owner>/rimz-ci:latest`.

On Gitea, `ci-image.yml` runs on manual dispatch or its weekly schedule: it refreshes the baked advisory DB and cargo registry on the current `RIMZ_CI_IMAGE` base, pushes a new immutable `rimz-ci:<tag>`, then repoints the repository variable that consuming workflows read. Dispatch with `full=true` after a toolchain change so the image rebuilds from `ci/Dockerfile` before repointing, and pass `base` to build from an image other than the current `RIMZ_CI_IMAGE`. Until that full rebuild, jobs take the same per-job rustup fallback for missing pinned Rust components and targets.

## Release packaging

`cargo xtask dist` packages the host release binary and builds macOS archives for both Apple targets through `cargo-zigbuild`, then ad-hoc signs the Apple Silicon binary with `rcodesign` (the `apple-codesign` crate's CLI). arm64 macOS refuses to `exec` a Mach-O with no code signature. zig linker-signs the arm64 build and reserves the signature room, and the explicit `rcodesign` pass rewrites it to a proper ad-hoc signature (no Apple certificate or notarization). That pass fails loudly if the room ever disappears. The x86_64 build ships as built (Intel execs unsigned binaries). `rust-toolchain.toml` provisions the Apple Rust standard libraries. On Linux the dist task supplies the SDKROOT shape current `rustc` expects and the framework text stubs current macOS dependencies link, while Zig supplies the Darwin libc stubs.

The cross-target checks and release packaging need `cargo-zigbuild` and `zig` on `PATH`. Release packaging also needs `rcodesign`:

```sh
cargo install --locked cargo-zigbuild
cargo install --locked apple-codesign   # or a prebuilt rcodesign release binary
# install zig from the host package manager or Zig's official bundle, then verify
cargo zigbuild --help
zig version
rcodesign --version
```
