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

Stdout is the protocol surface. The crate root of every binary enforces this with:

```rust
#![deny(clippy::print_stdout)]
```

The only legal `println!` sites are `--json` event emitters and the final user-facing message, each annotated `#[expect(clippy::print_stdout)]` with a one-line reason. The hook subcommand (`rimz hooks <agent> ...`) is a third allowed site — its stdout is the agent-native decision channel, per [agent.md → Hook stdout is the decision channel](../internals/agents/agent.md#hook-stdout-is-the-decision-channel) and the rule of the same name in [AGENTS.md](../../AGENTS.md).

Human-facing tables, key/value blocks, and listings render through the shared `cli/render` layer rather than ad-hoc `println!`. `render::out()` returns an `anstream::AutoStream` over stdout that strips ANSI when output is not a terminal or color is disabled (`NO_COLOR`/`CLICOLOR`, or `--color never`), so `--json` and snapshot output stay byte-clean. It writes through `writeln!`, not the `print!` macros, sharing the protocol-surface discipline of the `print_json` helper without a new `#[expect]` site. Colors come from `render::palette` (the same default `Semantic` palette the sidebar uses), and a state's tone is resolved once in `render::status` — keyed on the typed status enum, not a rendered string — so every command colors a given state identically. New human output uses `render`. Two surfaces stay on annotated `println!` by design: the `doctor` multi-section diagnostic (its own bespoke layout, migrated opportunistically) and terse value emitters whose stdout is a single scripting value (`queue list`'s ids, `feed`'s request ids).

All other output flows through `tracing` to stderr:

```rust
const DEFAULT_LOG_FILTER: &str = "warn";

fn stderr_env_filter() -> tracing_subscriber::EnvFilter {
    EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(DEFAULT_LOG_FILTER))
        .unwrap_or_else(|_| EnvFilter::new("warn"))
}
```

The default filter is silent at info level; `RUST_LOG` is the user's opt-in. Terminal UI binaries use `off` by default so logs do not corrupt the pane; they still honor `RUST_LOG`. Subscribers are installed once at the binary entry, never in library code. Span fields populated downstream are pre-allocated with `tracing::field::Empty`:

```rust
let span = tracing::info_span!(
    "rimz.feed.resolve",
    workspace.id = field::Empty,
    request.id = field::Empty,
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

Every identifier that travels through the schema, the ledger, or the wakeup socket is a newtype. No bare `String` or `Uuid` flowing through public APIs.

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
- Identifiers minted by Rimz (`RequestId`, `SidebarInstanceId`, and other internal correlation IDs) use **UUIDv7** for monotonic ordering — filenames named after the ID sort chronologically without an external index.
- Identifiers derived from external truth use their natural shape: `WorkspaceId` is the SHA-256 of `project_root`; `PaneId` is `"<mux>:<raw_pane_id>"` per [multiplexers.md](../internals/sidebar/multiplexers.md). These types still go through a newtype and a parser — never assembled inline.

## State machines as types

Lifecycle states are enums with predicate methods. Booleans are forbidden where a discriminated state would do.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedStatus { Pending, Resolved, TimedOut, Abandoned }

impl FeedStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Resolved | Self::TimedOut | Self::Abandoned)
    }
    pub fn allows_resolution(self) -> bool {
        matches!(self, Self::Pending)
    }
}
```

The CAS rules from [ledger.md](../internals/sidebar/ledger.md) — first valid writer wins — live at the file boundary (`ledger/feed_store.rs`), not inside the status enum. The enum carries the *vocabulary*; the boundary carries the *rule*.

`AgentStatus`, `PermissionPosture`, surfaces, resolution methods — same shape.

## Durable-state handle pattern

Actors are for genuinely long-lived processes with concurrent in-process writers (a future resolver host, a future watcher daemon). For short-lived CLI invocations, the right shape is a `Clone`able handle holding `Arc<Inner>`, where each public method takes the workspace lock for its critical section and does the file work directly. Cross-process serialization is the workspace lock's job; in-process serialization through an actor is redundant when there is one writer process per workspace at a time.

```rust
#[derive(Clone, Debug)]
pub struct Ledger {
    inner: Arc<LedgerInner>,
}

impl Ledger {
    #[must_use = "durability barrier; check the result"]
    pub fn push_feed_item(&self, item: &FeedItem, session_name: &str) -> Result<()> {
        {
            let _guard = lock::WorkspaceLock::acquire(&self.inner.paths.workspace_lock)?;
            feed_store::write(&self.inner.paths.feed_dir, item)?;
            event_log::append(&self.inner.paths.events_log, /* envelope */)?;
            snapshot::rebuild(&self.inner.paths)?;
        }
        self.wake_sidebars_best_effort(&item.workspace_id, &item.request_id);
        Ok(())
    }
}
```

Rules:

- Public methods return `Result<...>` and acquire the workspace lock for their critical section.
- Best-effort wakeups (sidebar fanout, per-request datagram) fire after the lock drops — the ledger commit always observes before notification.
- The handle is `Clone` via `Arc<Inner>`; cheap to pass into spawned tasks.
- Durability barriers carry `#[must_use = "durability barrier; check the result"]` so dropping `Err(Fsync(...))` is a compile-time warning, not a silent data loss.

When a long-lived process *does* arrive (resolver host, watcher daemon), it gets the actor shape: `tokio::mpsc` for commands, `oneshot::Sender` per durability barrier, terminal-failure cell captured behind `Arc<Mutex<Option<Arc<Err>>>>` so subsequent callers learn *why* the actor died. That process will already be async-shaped at every call site.

## Atomic writes

Two write shapes in `ledger/atomic.rs` cover every disk write in the project:

- `write_temp_then_rename(path, value)` for cold-path durable state (trust grants, workspace records, hook installs, the rotation carryover); `write_temp_then_rename_cache` — rename-atomic, no fsync — for feed files, liveness files, and rebuilt-on-next-read caches.
- `append_record_bytes(path, line) -> Result<()>` — the event-log append discipline (one `write()` per record, no fsync — appended frames become durable through the write tail's debounced group fdatasync and rotation's pre-rename sync); the frame encoding itself lives beside its decoder in `ledger/event_log.rs`.

Both helpers live next to the durability contract they enforce. No module hand-rolls its own temp-file dance, and every fsync syscall lives in `ledger/atomic.rs` (CI grep), counted through its `testkit` seam so the performance tier can assert fsync budgets from the integration binary. See [ledger.md](../internals/sidebar/ledger.md) for the frame format, torn-record recovery, and rotation rules.

## Tests

Local runner: `cargo xtask test` (wraps `cargo nextest run`; trailing args forward as nextest filters, for example `cargo xtask test auth`). nextest is the only suite runner — install it with `cargo install cargo-nextest --locked`. Doctests, which nextest does not run, go through `cargo xtask doctest`.

Core test shapes keep their own discipline:

- **Unit tests** — `#[cfg(test)] mod tests` in the module under test: inline by default; past ~500 lines the body moves to a sibling file (`#[cfg(test)] mod tests;` + `view/tests.rs`) — same module path, same private access, enforced by `cargo xtask invariants`. The move is whole-module: a module's unit tests live in one place, inline or sibling, so "where are the tests" has one answer and the size gate stays meaningful. When a sibling `tests.rs` itself grows, organize with nested modules inside it grouped by concern (a `tests/` directory module past that) — growth extends the test tree, it does not return tests to the source file. An outgrown unit-test module is also a prompt: check whether the weight belongs in a domain module or the integration tier before extracting. Pure logic only: state-machine transitions, parser shapes, schema round-trips. No filesystem, no network, no subprocess.
- **Integration tests** — one binary per crate at `crates/rimz/tests/integration/main.rs`, where each suite is a module (`mod hooks;`) and related suites group under a subdirectory module (`mod backend;` over `backend/{tmux,zellij}.rs`). The shared harness is declared once in `crates/rimz/tests/integration/common/`. Real subprocesses, real temp directories under `tempfile::TempDir`, real ledger files. Spawn `rimz` through the `Env` harness in `crates/rimz/tests/integration/common/` (an `assert_cmd` `cargo-bin` builder). The bridge and backend matrix lives in `crates/rimz/tests/integration/`.
- **Performance gates** — integration tests under `crates/rimz/tests/integration/performance/` assert deterministic work bounds, not timing. Counters live at the funnel they measure: fsyncs in `ledger::atomic::testkit`, event-log bytes in `ledger::event_log::testkit`, and hot-path subprocess attempts in `proc::testkit`. The crate-level `testkit` feature exposes only synthetic fleet builders and counter readers for tests and benches; shipped artifacts do not enable it.
- **Performance benches** — `crates/rimz/benches/` holds non-gating divan benchmarks. They measure wall-clock and allocation figures over synthetic ledgers, pane frames, and sidecars, never real agents. Run them with `cargo xtask perf`; do not put timing assertions in CI.
- **Snapshot tests** — `insta::assert_snapshot!` for every protocol stdout (CLI, hook, `--json` events) **including failure shapes**. Normalize UUIDs, timestamps, absolute paths, and other transient identifiers at the assertion boundary before snapshotting; introduce a shared helper only when multiple suites need the same normalization. Sidebar render tests draw through a `vt100::Parser`-backed ratatui backend and snapshot the resulting screen contents — never widget internals.
- **Property tests** — `proptest` for parsers (TOML override values, agent payloads, framing), serializers (round-trip schema types), and state-machine transitions (no path leaves a final state).

Snapshot churn caused by transient IDs is a test-helper bug, not a product failure — fix the normalization.

## Dependency budget — current snapshot

Current snapshot — entries move when a better-designed alternative wins on design fit, maintenance, footprint, and security. Adding, replacing, or removing a row needs a one-paragraph PR justification.

| Tier | Crates |
| --- | --- |
| **Runtime — core** | `clap`, `serde`, `serde_json`, `tokio`, `tracing`, `tracing-subscriber`, `thiserror`, `uuid`, `jiff` |
| **Runtime — utility** | `tempfile`, `fs4`, `which`, `sha2`, `hex`, `crc32fast` (event-log frame checksum: hardware CRC32 on the per-frame validate and the repair scan), `flate2` (gzip-compressed embedded pricing table; already reached through `ureq` and vet-exempted), `image-webp` (pure-Rust WebP decode for the sidebar's one-format pet sprite sheets, avoiding libwebp bindings and the full `image` crate tree), `nix` (Unix sockets, sigaction) on `cfg(unix)`, `ureq` (rustls; the runtime pricing-refresh HTTP client, also used by `xtask pricing-refresh`), `terminal_size` (the launch-path terminal probe behind the sidebar birth size; already transitive via clap's `wrap_help`) |
| **Observability (opt-in)** | `sentry` (default features off; `backtrace`, `panic`, and the `ureq` transport — `contexts` omitted so the Apple `objc2` UIKit tree never links) and `sentry-tracing` are optional behind the non-default `sentry` feature, excluded from shipped artifacts like `testkit`. With the feature enabled, off-box error reporting stays dormant until a `[sentry] dsn` resolves. The `ureq` transport reuses the rustls-backed client already pinned for pricing, so no second HTTP/TLS stack enters the tree; `sentry-tracing` bridges the existing `tracing` subscriber rather than adding a parallel reporting API. Its transitive tree (sentry-*, the `url`/`idna`/`icu` DSN-parsing subtree, `rand`) is covered by the imported audit sets in `supply-chain/config.toml`, with exemptions only for the crates those sets miss. Confined to `src/observability/` and the binary boundary in `main.rs` |
| **Binary boundary only** | `anyhow` — permitted in `crates/rimz/src/main.rs`, the private `cli/` module tree, and `xtask/` |
| **CLI presentation** | `anstyle`, `anstream`, `colorchoice`, `unicode-width` — the clap-native styling stack behind `cli/render`: `anstyle` styles, `anstream` auto-strips ANSI off-TTY and honors `NO_COLOR`/`CLICOLOR`/`CLICOLOR_FORCE`, `colorchoice` carries `--color` into anstream's global, `unicode-width` measures table columns. All four are already in the tree transitively through clap, so promoting them to direct deps adds zero footprint |
| **Sidebar runtime** | `ratatui` (via its `crossterm_0_29` feature); direct `crossterm` only when sidebar I/O actually requires it |
| **Zellij plugin (wasm-only)** | `zellij-tile` — the official plugin API, a `cfg(target_family = "wasm")` dependency of `crates/rimz-presence-zellij` alone, so no host artifact links it. Its tree is excluded from `deny.toml`'s audited targets (the wasm executes inside Zellij's plugin sandbox) and covered by `cargo vet` at `safe-to-run` |
| **Tests** | `insta`, `proptest`, `assert_cmd`, `predicates`, `vt100`, `tempfile`, `portable-pty` |
| **Performance benches** | `divan` — dev-only, small benchmark harness with built-in allocation profiling. It replaces no runtime code and links only into `[[bench]]` targets, giving reproducible cost-map numbers without criterion's larger harness footprint or bespoke allocator instrumentation |

Rules:

- Runtime deps update `deny.toml` and pass `cargo deny check`.
- Prefer std plus a small set of well-chosen crates over a transitive dependency tree.
- A new dep, or a replacement, needs a one-paragraph PR justification — what it provides, what it replaces, why we don't write the moral equivalent in twenty lines.
- `unsafe` requires a `// SAFETY:` comment naming the invariant and a code-owner review.
- An incumbent the table no longer lists is no longer accepted. New uses are caught by `cargo machete` and `cargo deny`. Crates removed in past snapshots so far: `chrono` (replaced by `jiff`), `bytes`, `tokio-util`.

## Toolchain and quality gates — current snapshot

### Toolchain

The stable channel is pinned in `rust-toolchain.toml`. No Cargo.toml carries `rust-version`. Required components: `rustfmt`, `clippy`, `llvm-tools-preview`. Required targets: `wasm32-wasip1` (the Zellij presence plugin; rustup provisions it for contributors, and `ci/Dockerfile` mirrors the list for CI).

Repo-local Cargo config stays installation-safe: `.cargo/config.toml` defines only the `xtask` alias, so source installs use each host's platform linker. CI provides `mold` on Linux through the `rimz-ci` image and runs `cargo xtask ci` through `mold -run`; contributors may opt into mold in their user Cargo config for faster local relinks. mold replaces the default bfd linker on the link-heavy integration-test binary, which relinks on every incremental change; it is a build-time tool only — no runtime or transitive footprint — and is the SOTA Unix linker.

### CI image

The `rimz-ci` image bakes Node for Actions, the Rust stable toolchain with required components and targets, cargo gate plugins, `cargo-vet`, tmux, Zellij, mold, Python, `cargo-zigbuild`, Zig, `rcodesign`, and `gh`. Tool versions live in `ci/Dockerfile`, which is the single source of truth for containerized CI and release jobs.

Refresh the image by editing `ci/Dockerfile`, then pushing that change to `main` or dispatching `.github/workflows/ci-image.yml`. The workflow builds and pushes a new immutable `rimz-ci:<tag>` image to the Gitea container registry, then repoints the repository variable `RIMZ_CI_IMAGE`; consuming workflows read only that variable.

Release packaging uses extra host tools. `cargo xtask dist` packages the non-Darwin host release binary and builds packaged macOS archives for both Apple targets through `cargo-zigbuild`, so release maintainers keep `cargo-zigbuild` and `zig` on `PATH`. Install `cargo-zigbuild` with Cargo and install Zig from the host package manager or Zig's official bundle:

```sh
cargo install --locked cargo-zigbuild
# install zig, then verify both tools
cargo zigbuild --help
zig version
```

`cargo xtask dist` then ad-hoc signs the Apple Silicon binary with `rcodesign` (the `apple-codesign` crate's CLI), so release maintainers also keep `rcodesign` on `PATH`. arm64 macOS refuses to `exec` a Mach-O that carries no code signature; zig linker-signs the arm64 build and reserves the signature room, so the explicit step rewrites it to a proper ad-hoc signature with no Apple certificate or notarization — and fails loudly if that room ever disappears. The x86_64 build needs no signature (Intel execs unsigned) and ships as built. Install `rcodesign` with Cargo or a prebuilt release binary:

```sh
cargo install --locked apple-codesign
rcodesign --version
```

`rust-toolchain.toml` provisions the Apple Rust standard libraries for rustup-managed toolchains. On Linux, the dist task supplies the SDKROOT shape current `rustc` expects while Zig supplies the Darwin linker stubs for Rimz's release binary.

### Quality gates

Every gate runs in CI with warnings treated as errors. Local equivalents are `cargo xtask <task>`; `cargo xtask ci` composes the full stack when a change calls for full validation.

- `cargo fmt --all -- --check` — formatting.
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` — lint.
- `cargo nextest run --workspace --all-features --locked` — test runner (the `test` task; the standalone fast signal); accepts nextest filters such as `cargo xtask test auth`.
- `cargo xtask doctest` — doctests.
- `cargo xtask docs-links` — every relative markdown link target and `#anchor` resolves in the working tree (offline and deterministic; external URLs are out of scope).
- `cargo deny check` — licence, advisory, and ban check.
- `cargo machete` — unused dependency check.
- `cargo vet` — supply-chain audit.
- `cargo llvm-cov nextest --workspace --all-features --locked` — coverage. This *is* the suite run inside `ci`: the tests run once, under instrumentation, instead of building and running a second uninstrumented pass. The `rimz-ci` image provides tmux and Zellij before this gate, so the same pass exercises live backend tests under nextest's live groups.
- `cargo semver-checks` — release-time API check; skipped while the workspace version is the unpublished pre-release `0.0.0`.
- `cargo xtask perf` — non-gating divan benchmarks for the measured performance model; accepts cargo bench filters such as `cargo xtask perf fleet`.

Inside `ci` the gates are ordered for speed, not listed order: the instant text gates (`fmt`, `invariants`, `docs-links`) run first and fail fast; the metadata-only audits (`deny`, `deps`, `vet`) overlap the compile gates on their own threads; the compile gates run sequentially (`build-plugin → lint → coverage → doctest → semver`) because concurrent cargo builds only serialize on the target-dir lock. `ci` prints a per-gate timing summary to stderr.

Run `cargo xtask hooks` once per clone to activate the tracked git hooks (it points `core.hooksPath` at `.githooks/`). The committed `pre-commit` shim routes git's call to `cargo xtask fmt`, so a commit that would fail the CI formatting gate is caught locally before it lands; `git commit --no-verify` bypasses it for a single commit. CI stays the authoritative gate — the hook is fast local feedback, not a substitute.

### Architectural invariants

`cargo xtask invariants` is a grep-and-shape gate over the tracked tree — a low-cost trip-wire paired with the type system and review, not a substitute for either. It guards the boundaries the compiler can't: decision-channel integrity, sidebar/ledger separation, the trust hash, pane-primitive use, the render snapshot clock, inline-test size, [UI-color provenance](../reference/theme.md#how-a-tone-resolves), and more. A new boundary lands here as an `ensure_*` check with a self-test.

### Contributor command surface

`cargo xtask <task>` is the entry point. Tasks: `build`, `build-plugin`, `install`, `hooks`, `fmt`, `lint`, `test`, `doctest`, `deps`, `deny`, `vet`, `coverage`, `semver`, `perf`, `invariants`, `docs-links`, `pricing-refresh`, `brew-formula`, `screenshot`, `ci`. New automation lands in `xtask/`; the only tracked hook script is `.githooks/pre-commit`, and it routes git's hook call back to `cargo xtask`.

## Reading order for new contributors

1. [AGENTS.md](../../AGENTS.md) — engineering principles and implementation rules.
2. This file — module shape and idioms.
3. [ARCHITECTURE.md](../../ARCHITECTURE.md) — where the modules live.
4. [ledger.md](../internals/sidebar/ledger.md) and the [quality gates](#quality-gates) — the contracts that touch every module.
