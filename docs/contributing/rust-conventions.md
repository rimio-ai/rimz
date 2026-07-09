# Rust conventions

> See [AGENTS.md](../../AGENTS.md) for the engineering principles and implementation rules this doc operationalizes.

The shape that every module in `crates/rimz` follows. Pick the closest existing module before inventing; the team values consistency over novelty.

## CLI shape

The CLI is parsed with `clap` derive at the top level. Shared option groups are split into their own structs and embedded with `#[clap(flatten)]` so each subcommand inherits them without restating fields.

```rust
#[derive(Debug, clap::Parser)]
#[command(
    author, version,
    bin_name = "rimz",
    about = "One room per project for agents, scripts, and humans.",
)]
struct Cli {
    #[clap(flatten)] global: GlobalFlags,
    #[clap(flatten)] attach: AttachFlags,
    #[command(subcommand)] subcommand: Option<Subcmd>,
}
```

- The bare `rimz` default action targets `.`. Path-taking launch stays on `rimz start [PATH]` so mistyped subcommands reach clap as unknown commands.
- `bin_name = "rimz"` keeps help text stable when the executable is invoked via a platform-specific path.
- Subcommands are a flat `enum Subcmd { ... }`. Use `#[clap(visible_alias = "<short>")]` for discoverable shorthand and `#[clap(hide = true)]` for hooks-only or internal subcommands.

## Stdout and tracing

Stdout is the protocol surface. The lint wall lives once in the workspace `Cargo.toml`: `[workspace.lints.clippy]` denies `print_stdout` and `print_stderr` in every crate, and `[workspace.lints.rust]` forbids `unsafe_code` outright — the workspace has no unsafe escape hatch. Both binary crate roots restate the stdout rule as `#![deny(clippy::print_stdout)]`.

A `println!` site is legal only when annotated `#[expect(clippy::print_stdout, reason = "...")]` and its stdout is one of:

- a `--json` event stream or the final user-facing message of a command;
- a single scripting value (`message` ids, launched agent names);
- the agent-native decision channel — `rimz hooks <agent> ...` stdout is parsed by the agent, per [agent.md → Hook stdout is the decision channel](../internals/agents/model.md#hook-stdout-is-the-decision-channel);
- the `doctor` multi-section diagnostic, a bespoke layout migrated to `render` opportunistically.

Human-facing tables, key/value blocks, and listings render through the shared `cli/render` layer, never ad-hoc `println!`. `render::out()` returns an `anstream::AutoStream` over stdout that strips ANSI when output is piped or color is disabled (`NO_COLOR`/`CLICOLOR`, or `--color never`), so `--json` and snapshot output stay byte-clean; it writes through `writeln!`, keeping the `print_stdout` lint on guard without a new `#[expect]` site. Colors come from `render::palette` (the same default `Semantic` tones the sidebar uses), and a state's tone resolves once in `render::status` — keyed on the typed status enum, not a rendered string — so every command colors a given state identically. New human output uses `render`.

All other output flows through `tracing` to stderr:

```rust
const DEFAULT_LOG_FILTER: &str = "warn";

fn stderr_env_filter() -> tracing_subscriber::EnvFilter {
    EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(DEFAULT_LOG_FILTER))
        .unwrap_or_else(|_| EnvFilter::new("warn"))
}
```

The default filter prints warnings and errors only; `RUST_LOG` is the user's opt-in for more. Terminal UI binaries default to `off` so logs do not corrupt the pane; they still honor `RUST_LOG`. Subscribers are installed once at the binary entry, never in library code. Span fields populated downstream are pre-allocated with `tracing::field::Empty`:

```rust
let span = tracing::info_span!(
    "rimz.message.deliver",
    workspace.id = field::Empty,
    message.id = field::Empty,
);
```

## Error types

One `thiserror` enum per module, named after the failure mode it represents, not with a generic `*Error` suffix:

```rust
#[derive(Debug, thiserror::Error)]
pub enum EventLogErr {
    #[error("torn record at offset {offset}: {reason}")]
    TornRecord { offset: u64, reason: String },
    #[error("frame length mismatch: claimed {claimed}, available {available}")]
    FrameLength { claimed: u64, available: u64 },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, EventLogErr>;
```

The `Result<T>` alias lives next to the enum it shadows. Predicates like `is_recoverable()` or `is_retryable()` go on the enum, not at call sites. `#[from]` is used for boring system-error conversions; structured context is captured as struct fields on the variant, never lost.

`anyhow` is allowed only at binary boundaries: `crates/rimz/src/main.rs`, the private `cli/` module tree, and `xtask/`. Library modules return their own typed `Result`.

## Identifier newtypes

Every identifier that travels through the schema, the store, or the wakeup socket is a newtype. No bare `String` or `Uuid` flowing through public APIs.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId(Uuid);

impl RequestId {
    pub fn new() -> Self { Self(Uuid::now_v7()) }
}

impl std::fmt::Display for RequestId { /* ... */ }
impl std::str::FromStr for RequestId { /* ... */ }
impl Serialize for RequestId { /* writes as a string */ }
impl<'de> Deserialize<'de> for RequestId { /* parses from a string */ }
```

Conventions:

- Inner value is **never** `pub`. Use `pub(crate)` only if the same crate needs the unwrapped form for an FFI seam.
- Identifiers minted by Rimz (`RunId`, `EventId`, `SidebarInstanceId`, and other internal correlation IDs) use **UUIDv7** for monotonic ordering — filenames named after the ID sort chronologically without an external index.
- Identifiers derived from external truth use their natural shape: `WorkspaceId` is the SHA-256 of `project_root`; `PaneId` is `"<mux>:<raw_pane_id>"` per [multiplexers.md](../internals/multiplexers.md). These types still go through a newtype and a parser — never assembled inline.

## State machines as types

Lifecycle states are enums with predicate methods. Booleans are forbidden where a discriminated state would do.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus { Queued, Claimed, Sent, Delivered, TimedOut, /* … */ }

impl MessageStatus {
    pub const fn is_open(self) -> bool {
        matches!(self, Self::Queued | Self::Claimed)
    }
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Claimed | Self::Sent)
    }
}
```

The lifecycle rules from [messaging.md](../internals/harness/messaging.md) — record first, claim before send, terminal is final — live at the store boundary (`store/message_store.rs`, `store/writer/queue.rs`), not inside the status enum. The enum carries the *vocabulary*; the boundary carries the *rule*.

`AgentStatus`, `PermissionPosture`, gates, delivery outcomes — same shape.

## Durable-state handle pattern

Actors are for genuinely long-lived processes with concurrent in-process writers (a future watcher daemon, a future scheduler host). For short-lived CLI invocations, the right shape is a `Clone`able handle holding `Arc<Inner>`, where each public method takes the workspace lock for its critical section and does the file work directly. Cross-process serialization is the workspace lock's job; in-process serialization through an actor is redundant when there is one writer process per workspace at a time.

```rust
#[derive(Clone, Debug)]
pub struct Store {
    inner: Arc<StoreInner>,
}

impl Store {
    #[must_use = "durability barrier; check the result"]
    pub fn queue_message(&self, message: &MessageRecord, session_name: &str) -> Result<()> {
        {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            message_store::write(&self.inner.paths.messages_dir, message)?;
            event_log::append(&self.inner.paths.events_log, /* envelope */)?;
            snapshot::rebuild(&self.inner.paths)?;
        }
        self.wake_sidebars_best_effort();
        Ok(())
    }
}
```

Rules:

- Public methods return `Result<...>` and acquire the workspace lock for their critical section.
- Best-effort wakeups (sidebar fanout, the run-socket datagram) fire after the lock drops — the store commit always observes before notification.
- The handle is `Clone` via `Arc<Inner>`; cheap to pass into spawned tasks.
- Durability barriers carry `#[must_use = "durability barrier; check the result"]` so dropping `Err(Fsync(...))` is a compile-time warning, not a silent data loss.

When a long-lived process *does* arrive (watcher daemon, scheduler host), it gets the actor shape: `tokio::mpsc` for commands, `oneshot::Sender` per durability barrier, terminal-failure cell captured behind `Arc<Mutex<Option<Arc<Err>>>>` so subsequent callers learn *why* the actor died. That process will already be async-shaped at every call site.

## Atomic writes

Two write shapes in `store/atomic.rs` cover every disk write in the project:

- `write_temp_then_rename(path, value)` for cold-path durable state (trust grants, workspace records, hook installs, the rotation carryover); `write_temp_then_rename_cache` — rename-atomic, no fsync — for liveness files and rebuilt-on-next-read caches.
- `append_record_bytes(path, line) -> Result<()>` — the event-log append discipline (one `write()` per record, no fsync — appended frames become durable through the write tail's debounced group fdatasync and rotation's pre-rename sync); the frame encoding itself lives beside its decoder in `store/event_log.rs`.

Both helpers live next to the durability contract they enforce. No module hand-rolls its own temp-file dance, and every fsync syscall lives in `store/atomic.rs` (CI grep), counted through its `testkit` seam so the performance tier can assert fsync budgets from the integration binary. See [store.md](../internals/store.md) for the frame format, torn-record recovery, and rotation rules.

## Tests

`cargo xtask test` wraps `cargo nextest run --workspace --all-features --locked`; trailing args forward as nextest filters and profiles (`cargo xtask test auth`, `cargo xtask test -P live`). nextest is the only suite runner — never run bare `cargo test`; install it with `cargo install cargo-nextest --locked`. Three profiles in `.config/nextest.toml` partition the suite: `gate` (deterministic non-live tests, what `cargo xtask gate` runs), `live` (mux backend tests and deep mux smokes), and `journey` (non-deep rendered journeys).

Core test shapes keep their own discipline:

- **Unit tests** — `#[cfg(test)] mod tests` in the module under test: inline by default; past 500 lines the body moves to a sibling file (`#[cfg(test)] mod tests;` + `view/tests.rs`) — same module path, same private access, enforced by `cargo xtask invariants`. The move is whole-module: a module's unit tests live in one place, inline or sibling, so "where are the tests" has one answer and the size gate stays meaningful. When a sibling `tests.rs` itself grows, organize with nested modules inside it grouped by concern (a `tests/` directory module past that) — growth extends the test tree, it does not return tests to the source file. An outgrown unit-test module is also a prompt: check whether the weight belongs in a domain module or the integration tier before extracting. Pure logic only: state-machine transitions, parser shapes, schema round-trips. No filesystem, no network, no subprocess.
- **Integration tests** — one binary per crate at `crates/rimz/tests/integration/main.rs`, where each suite is a module (`mod hooks;`) and related suites group under a subdirectory module (`mod backend;` over `backend/{tmux,zellij}.rs`). Real subprocesses, real temp directories under `tempfile::TempDir`, real store files. Spawn `rimz` through the `Env` harness in `crates/rimz/tests/integration/common/` (an `assert_cmd` `cargo-bin` builder); suite layout and local rules live in the [suite contract](../../crates/rimz/tests/integration/AGENTS.md).
- **Performance gates** — integration tests under `crates/rimz/tests/integration/performance/` assert deterministic work bounds, not timing. Counters live at the funnel they measure: fsyncs in `store::atomic::testkit`, event-log bytes in `store::event_log::testkit`, and hot-path subprocess attempts in `proc::testkit`. The crate-level `testkit` feature exposes only synthetic fleet builders and counter readers for tests and benches; shipped artifacts do not enable it.
- **Performance benches** — `crates/rimz/benches/` holds non-gating divan benchmarks. They measure wall-clock and allocation figures over synthetic stores, pane frames, and sidecars, never real agents. Run them with `cargo xtask perf`; do not put timing assertions in CI.
- **Snapshot tests** — `insta::assert_snapshot!` for every protocol stdout (CLI, hook, `--json` events) **including failure shapes**. Normalize UUIDs, timestamps, absolute paths, and other transient identifiers at the assertion boundary before snapshotting; introduce a shared helper only when multiple suites need the same normalization. Sidebar render tests draw through a `vt100::Parser`-backed ratatui backend and snapshot the resulting screen contents — never widget internals.
- **Property tests** — `proptest` for parsers (TOML override values, agent payloads, framing), serializers (round-trip schema types), and state-machine transitions (no path leaves a final state).

Snapshot churn caused by transient IDs is a test-helper bug, not a product failure — fix the normalization.

## Dependency budget

The direct-dependency snapshot. Entries move when a better-designed alternative wins on design fit, maintenance, footprint, and security. The full justification for each entry — what it provides, what it replaces, why Rimz does not write the moral equivalent in twenty lines — lives as a comment beside it in the workspace [Cargo.toml](../../Cargo.toml); this table is the policy summary.

| Tier | Crates |
| --- | --- |
| **Runtime — core** | `clap`, `serde`, `serde_json`, `toml`, `tokio`, `tracing`, `tracing-subscriber`, `thiserror`, `uuid`, `jiff` |
| **Runtime — utility** | `tempfile`, `which`, `sha2`, `hex`, `foldhash` (spend-aggregation maps), `crc32fast` (event-log frame checksum), `flate2` (gzipped embedded pricing table), `image-webp` (pure-Rust decode for WebP pet sprite sheets), `png` (pure-Rust decode for PNG pet sprite sheets, including petdex installs), `rusqlite` (bundled; read-only parse of OpenCode's SQLite store), `toml_edit` (format-preserving `rimz config set`), `kdl` (structured merge of Zellij's `permissions.kdl` cache), `shlex` (launch-flag strings to argv without a shell), `glob` (`.worktreeinclude` pattern expansion), `similar` (hook-install consent diffs), `notify` (filesystem-watch latency hint over the tick backstop), `terminal_size` (sidebar birth-size probe), `ureq` (rustls; the pricing-refresh HTTP client, shared with `xtask pricing-refresh`), `tungstenite` (RFC6455 client over the Codex daemon's UDS control socket), and on `cfg(unix)`: `nix` (sockets, signals, process and user queries) and `signal-hook` (signal-to-handler plumbing) |
| **Observability (opt-in)** | `sentry` (default features off; `backtrace`, `panic`, and the `ureq` transport — `contexts` omitted so the Apple `objc2` UIKit tree never links) and `sentry-tracing`, optional behind the non-default `sentry` feature so shipped artifacts exclude the whole tree. The `ureq` transport reuses the rustls client already pinned for pricing; `sentry-tracing` bridges the existing `tracing` subscriber rather than adding a parallel reporting API. Confined to `src/observability/` and the binary boundary in `main.rs` |
| **Binary boundary only** | `anyhow` — permitted in `crates/rimz/src/main.rs`, the private `cli/` module tree, and `xtask/` |
| **CLI presentation** | `anstyle`, `anstream`, `colorchoice`, `unicode-width` — the clap-native styling stack behind `cli/render`; all four are already in the tree transitively through clap, so promoting them to direct deps adds zero footprint |
| **Sidebar runtime** | `ratatui` (via its `crossterm_0_29` feature); direct `crossterm` only when sidebar I/O actually requires it |
| **Zellij plugin (wasm-only)** | `zellij-tile` — the official plugin API, a `cfg(target_family = "wasm")` dependency of `crates/rimz-presence-zellij` alone, so no host artifact links it. Its tree is excluded from `deny.toml`'s audited targets (the wasm executes inside Zellij's plugin sandbox) and covered by `cargo vet` at `safe-to-run` |
| **Tests** | `insta`, `proptest`, `assert_cmd`, `predicates`, `vt100`, `tempfile`, `portable-pty` |
| **Performance benches** | `divan` — dev-only, small benchmark harness with built-in allocation profiling; links only into `[[bench]]` targets |

Rules:

- Adding, replacing, or removing a row needs a one-paragraph PR justification and a matching comment in the workspace `Cargo.toml`.
- Runtime deps update `deny.toml` and pass `cargo deny check`.
- Prefer std plus a small set of well-chosen crates over a transitive dependency tree.
- An incumbent the table no longer lists is no longer accepted. New uses are caught by `cargo machete` and `cargo deny`. Crates removed in past snapshots so far: `chrono` (replaced by `jiff`), `bytes`, `tokio-util`, `fs4` (replaced by std `File` locking in Rust 1.89).

## Toolchain

The stable channel is pinned in `rust-toolchain.toml`; the workspace is edition 2024 and no `Cargo.toml` carries `rust-version`. Required components: `rustfmt`, `clippy`, `llvm-tools-preview`. Required targets: `wasm32-wasip1` (the Zellij presence plugin) plus `aarch64-apple-darwin` and `x86_64-apple-darwin` (the [release cross-builds](#release-packaging)). rustup provisions all of them from the pin; `ci/Dockerfile` mirrors the list for CI.

Repo-local Cargo config stays installation-safe: `.cargo/config.toml` defines only the `xtask` alias, so source installs use each host's platform linker. CI provides `mold` on Linux through the `rimz-ci` image and runs link-heavy compile/test gates through `mold -run`; contributors may opt into mold in their user Cargo config for faster relinks of the link-heavy integration-test binary.

Install `sccache` for a local compile cache: xtask detects it on `PATH` and sets `RUSTC_WRAPPER=sccache` plus `CARGO_INCREMENTAL=0` for cargo compile commands. `RIMZ_SCCACHE=off` keeps incremental enabled for heavy single-crate iteration; `RIMZ_SCCACHE=on` requests cache routing and warns when `sccache` is missing.

Run `cargo xtask hooks` once per clone to activate the tracked git hooks (it points `core.hooksPath` at `.githooks/`). The committed `pre-commit` shim routes git's call to `cargo xtask fmt`, so a commit that would fail the CI formatting gate is caught before it lands; `git commit --no-verify` bypasses it for a single commit. CI stays the authoritative gate — the hook is fast local feedback, not a substitute.

## Contributor command surface

`cargo xtask <task>` is the entry point for contributor automation; new automation lands in `xtask/`, and the only tracked hook script is `.githooks/pre-commit`, which routes back to it. Tasks: `build`, `build-plugin`, `plugin-refresh`, `install`, `install-dev`, `stage-install`, `dist`, `brew-formula`, `profile-build`, `hooks`, `fmt`, `lint`, `test`, `test-archive`, `deps`, `deny`, `vet`, `semver`, `externals`, `coverage`, `perf`, `complexity`, `invariants`, `docs-links`, `gate`, `checks`, `ci`, `pricing-refresh`, `theme-refresh`, `screenshot`.

Three deserve a note:

- `cargo xtask complexity [N]` ranks tracked `.rs` files by cyclomatic/cognitive complexity via `rust-code-analysis-cli` (`cargo install rust-code-analysis-cli --locked`); a local report, not part of any gate.
- `cargo xtask install-dev` is the contributor opt-in to [off-box reporting](../internals/diagnostics.md#off-box-error-reporting): it installs the optimized `profiling` host profile with `--features sentry`, line tables, frame pointers, and v0 symbol names, so dogfooding sessions stay profilable and default to the `development` Sentry environment.
- `cargo xtask profile-build` writes the same optimized `target/profiling/rimz` without installing it.

## Quality gates

Every PR gate runs in CI with warnings treated as errors; each has a local `cargo xtask` equivalent. Four composites cover the everyday flows:

- `cargo xtask gate` — the pre-PR default: `cargo fmt --all` in fix mode, then invariants, docs-links, lint, and `cargo nextest run --profile gate --workspace --all-features --locked`. It captures each step's output, prints one compact success line per step, and fails fast with a trimmed excerpt plus a `NEXT:` hint.
- `cargo xtask checks` — the registry-free non-test gates, ordered for speed: the instant text gates (`fmt` in check mode, `invariants`, `docs-links`) run first and fail fast; `deps` overlaps the compile gates on its own thread; the compile gates run sequentially (`build-plugin`, then `lint`) because concurrent cargo builds serialize on the target-dir lock. Prints a per-gate timing summary to stderr.
- `cargo xtask externals` — the gates that talk to the crates.io registry: `deny`, `vet`, `semver`. All three run so a single pass reports every signal.
- `cargo xtask ci` — `checks` plus plain `cargo nextest run --workspace --all-features --locked`; the local full stack when a change calls for full validation.

Escalate past `gate` when the change touches the matching surface: `cargo xtask test -P live` for live-backend and deep-mux-smoke coverage, `cargo xtask test -P journey` for rendered journeys, `cargo xtask externals` when dependencies or the public API change, and `cargo xtask ci` for both checks and the full suite.

The individual gates:

- `cargo fmt --all -- --check` — formatting (the `fmt` task).
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` — lint.
- `cargo nextest run --workspace --all-features --locked` — the `test` task; accepts nextest filters and profiles as trailing args.
- `cargo nextest archive --workspace --all-features --locked --archive-file <path>` — the `test-archive` task; compiles and packages the workspace test binaries for portable execution.
- `cargo xtask docs-links` — every relative markdown link target and `#anchor` resolves in the working tree (offline and deterministic; external URLs are out of scope).
- `cargo xtask invariants` — the [architectural invariants](#architectural-invariants).
- `cargo deny check -D warnings` — license, advisory, ban, and yanked-crate check. In CI it runs offline (`RIMZ_DENY_OFFLINE=1` adds `--disable-fetch`) against the image's baked advisory DB and a local crates.io index prepared at the canonical cache path.
- `cargo machete` — unused-dependency check (the `deps` task).
- `cargo vet --locked` — supply-chain audit; fetches the crates.io index directly, bypassing the runners' nexus mirror.
- `cargo semver-checks` — API check against the published baseline; skips only while the workspace version is `0.0.0` or while crates.io has no published `rimz` baseline to compare against.
- `cargo xtask coverage` — instrumented coverage (`cargo llvm-cov nextest --workspace --all-features --locked`), run off the PR hot path by the nightly/dispatch `coverage.yml` workflow, which uploads `target/ci/coverage/lcov.info`. The CI image provides tmux and Zellij, so the same pass exercises the live-backend tests.

### Architectural invariants

`cargo xtask invariants` is a grep-and-shape gate over the tracked tree — shallow string matches, so treat it as a low-cost trip-wire paired with the type system and review, not a substitute for either. It guards boundaries the compiler can't see: hook-stdout decision-channel integrity, sidebar/store import separation, spend-parser and diagnostic-write confinement, fsync calls staying in `store/atomic.rs`, pane-primitive use, the render snapshot clock, [UI-color provenance](../guide/theme.md#color-slots) and glyph provenance, vendored presence-plugin freshness, and the inline-test size gate. The authoritative set is the `ensure_*` list in [xtask/src/invariants.rs](../../xtask/src/invariants.rs); a new boundary lands there as an `ensure_*` check with a self-test.

## Continuous integration

CI lives in two workflow trees: `.gitea/workflows/` for the Gitea origin and `.github/workflows/` for the GitHub mirror. Both run the same gates inside the `rimz-ci` image; GitHub pulls `ghcr.io/<owner>/rimz-ci:latest` with the built-in `GITHUB_TOKEN`, while Gitea pulls the configured `RIMZ_CI_IMAGE` with its registry token. Both pipelines run three job groups in parallel:

- `checks` — `cargo xtask checks`.
- `externals` — the `deny`, `vet`, and `semver` gates as separate steps (locally: `cargo xtask externals`). They sit apart from `checks` because deny reads the baked advisory DB offline while vet and semver fetch crates.io directly, bypassing the runners' nexus mirror, so transient egress retries stay out of the main jobs.
- `tests` — compile the suite once, then run the `gate`, `live`, and `journey` nextest profiles from that one build so each tier's timing reflects test execution, not the shared compile.

The `tests` job compiles the suite (`cargo xtask test --no-run`) and runs the three profile steps in the same container, each guarded by `if: ${{ !cancelled() }}` plus a compile-success check so one failing tier still reports the others. The `tests` job is the branch-protection check on both forges.

The `live` profile runs both mux backends in one nextest process so tmux and Zellij co-schedule, while the per-backend `[test-groups]` in `.config/nextest.toml` bound concurrency. Runners are 64-core, so workflows leave nextest's global thread count uncapped. Every tier runs against a checkout at the same container path the suite was compiled from, so fixtures referenced through `env!("CARGO_MANIFEST_DIR")` resolve.

Compile jobs route through `sccache`. On Gitea PR CI the backend comes from the runner-provided environment (S3 on the current runners), and the cargo registry/index is baked into the `rimz-ci` image with a per-run `cargo fetch --locked` for lockfile drift. On the GitHub mirror, `sccache` uses the Actions cache and `rust-cache` still warms the registry because those hosted runners have no S3 backend or baked-registry guarantee. PR workflows do not cache `target/ci`: target-dir fingerprints are not content-addressed and are a cache-poisoning risk on public PRs, while `sccache` keys compiler outputs by content and compiler identity.

### CI image

The `rimz-ci` image bakes Node for Actions, the pinned Rust toolchain with required components and targets, the cargo gate plugins, `cargo-vet`, `sccache`, a warm RustSec advisory database, a warm cargo registry, tmux, Zellij, mold, Python, `cargo-zigbuild`, Zig, `rcodesign`, and `gh`. Tool versions live in `ci/Dockerfile`, the single source of truth for containerized CI and release jobs.

On GitHub, `ci-image.yml` builds `ci/Dockerfile` on `ubuntu-latest`, pushes both an immutable `rimz-ci:<tag>` and `rimz-ci:latest` to GHCR, and uses only the workflow `GITHUB_TOKEN` with `packages: write`. First bootstrap is a manual dispatch after the workflow lands on GitHub; when the repository opens to fork PRs, make the GHCR package public so unaffiliated forks can pull `ghcr.io/<owner>/rimz-ci:latest`.

On Gitea, a weekly schedule dispatches the default `ci-image.yml` refresh automatically: it refreshes the baked RustSec advisory DB and cargo registry on the current `RIMZ_CI_IMAGE` base, pushes a new immutable `rimz-ci:<tag>`, then repoints the repository variable that consuming workflows read. Manual dispatch remains the path for immediate advisory/registry refreshes and toolchain changes; for toolchain changes, edit `ci/Dockerfile` first, then dispatch with `full=true` so the image rebuilds from the Dockerfile before repointing.

## Release packaging

`cargo xtask dist` packages the host release binary and builds macOS archives for both Apple targets through `cargo-zigbuild`, then ad-hoc signs the Apple Silicon binary with `rcodesign` (the `apple-codesign` crate's CLI). arm64 macOS refuses to `exec` a Mach-O with no code signature; zig linker-signs the arm64 build and reserves the signature room, and the explicit `rcodesign` pass rewrites it to a proper ad-hoc signature — no Apple certificate or notarization — failing loudly if that room ever disappears. The x86_64 build ships as built (Intel execs unsigned binaries). `rust-toolchain.toml` provisions the Apple Rust standard libraries; on Linux the dist task supplies the SDKROOT shape current `rustc` expects and the framework text stubs current macOS dependencies link, while Zig supplies the Darwin libc stubs.

Release maintainers keep three extra tools on `PATH`:

```sh
cargo install --locked cargo-zigbuild
cargo install --locked apple-codesign   # or a prebuilt rcodesign release binary
# install zig from the host package manager or Zig's official bundle, then verify
cargo zigbuild --help
zig version
rcodesign --version
```

## Reading order for new contributors

1. [AGENTS.md](../../AGENTS.md) — engineering principles and implementation rules.
2. This file — module shape and idioms.
3. [ARCHITECTURE.md](../../ARCHITECTURE.md) — where the modules live.
4. [store.md](../internals/store.md) and the [quality gates](#quality-gates) — the contracts that touch every module.
