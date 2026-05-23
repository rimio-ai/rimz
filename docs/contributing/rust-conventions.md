# Rust conventions

> See [AGENTS.md](../../AGENTS.md) for the engineering principles and implementation rules this doc operationalizes.

The shape that every module in `crates/rimz` and `crates/rimz-sidebar` follows. Pick the closest existing module before inventing; the team values consistency over novelty.

## CLI shape

The CLI is parsed with `clap` derive at the top level. Shared option groups are split into their own structs and embedded with `#[clap(flatten)]` so each subcommand inherits them without restating fields.

```rust
#[derive(Debug, clap::Parser)]
#[clap(
    author, version,
    bin_name = "rimz",
    subcommand_negates_reqs = true,
)]
struct Cli {
    #[clap(flatten)] config_overrides: CliConfigOverrides,
    #[clap(flatten)] global: GlobalFlags,
    #[clap(subcommand)] subcommand: Option<Subcommand>,
}
```

- `subcommand_negates_reqs = true` lets root-level required args become optional when a subcommand is given.
- `bin_name = "rimz"` keeps help text stable when the executable is invoked via a platform-specific path.
- Subcommands are a flat `enum Subcommand { ... }`. Use `#[clap(visible_alias = "<short>")]` for discoverable shorthand and `#[clap(hide = true)]` for hooks-only or internal subcommands.

Ad-hoc TOML overrides via `-c key=value` are parsed by wrapping the right-hand side in a sentinel assignment so the user need not quote scalars:

```rust
fn parse_toml_value(raw: &str) -> Result<toml::Value, toml::de::Error> {
    let wrapped = format!("_x_ = {raw}");
    let table: toml::Table = toml::from_str(&wrapped)?;
    table.get("_x_").cloned().ok_or_else(/* sentinel missing */)
}
```

`-c model="claude-opus-4-7"` and `-c sidebar.width=30` and `-c features=["a","b"]` all work without further escaping.

## Stdout and tracing

Stdout is the protocol surface. The crate root of every binary enforces this with:

```rust
#![deny(clippy::print_stdout)]
```

The only legal `println!` sites are `--json` event emitters and the final user-facing message, each annotated `#[expect(clippy::print_stdout)]` with a one-line reason. The hook subcommand (`rimz hooks <agent> ...`) is a third allowed site — its stdout is the agent-native decision channel, per [ledger.md](../internals/ledger.md) and the *Hook stdout is the decision channel* rule in [AGENTS.md](../../AGENTS.md).

All other output flows through `tracing` to stderr:

```rust
const DEFAULT_LOG_FILTER: &str = "warn";

fn stderr_env_filter() -> tracing_subscriber::EnvFilter {
    EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(DEFAULT_LOG_FILTER))
        .unwrap_or_else(|_| EnvFilter::new("warn"))
}
```

The default filter is silent at info level; `RUST_LOG` is the user's opt-in. Subscribers are installed once at the binary entry, never in library code. Span fields populated downstream are pre-allocated with `tracing::field::Empty`:

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

`anyhow` is allowed only at binary boundaries: `crates/rimz/src/main.rs`, the private `cli/` module tree, `crates/rimz-sidebar/src/main.rs`, and `xtask/`. Library modules return their own typed `Result`.

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
- Identifiers derived from external truth use their natural shape: `WorkspaceId` is the SHA-256 of `project_root`; `PaneId` is `"<mux>:<raw_pane_id>"` per [multiplexers.md](../internals/multiplexers.md). These types still go through a newtype and a parser — never assembled inline.

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

The CAS rules from [ledger.md](../internals/ledger.md) — first valid writer wins — live at the file boundary (`ledger/feed_store.rs`), not inside the status enum. The enum carries the *vocabulary*; the boundary carries the *rule*.

`AgentStatus`, `AgentMode`, surfaces, resolution methods — same shape.

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

Two helper functions in `ledger/atomic.rs` cover every disk write in the project:

- `write_temp_then_rename(path, bytes) -> Result<()>` — feed files, snapshot files, heartbeats.
- `append_framed_record(file, bytes) -> Result<()>` — length-prefixed framing for `events.log.jsonl`, with `fsync` per record.

Both helpers live next to the durability contract they enforce. No module hand-rolls its own temp-file dance. See [ledger.md](../internals/ledger.md) for torn-record recovery and rotation rules.

## Tests

Local default: `cargo xtask test` (wraps `cargo nextest run`). `cargo test` still works but isn't the contributor path.

Three tiers, each with a clear discipline:

- **Unit tests** — `#[cfg(test)] mod tests` inline in the module under test. Pure logic only: state-machine transitions, parser shapes, schema round-trips. No filesystem, no network, no subprocess.
- **Integration tests** — `tests/integration/*.rs` in each crate. Real subprocesses, real temp directories under `tempfile::TempDir`, real ledger files. Spawn `rimz` via `assert_cmd` resolved through a `cargo-bin`-style helper in `tests/common/`. The M0 matrix is in [testing.md](./testing.md).
- **Snapshot tests** — `insta::assert_snapshot!` for every protocol stdout (CLI, hook, `--json` events) **including failure shapes**. Use the shared redactor in `tests/common/redact.rs` to strip UUIDs, timestamps, and absolute paths before snapshotting. Sidebar render tests draw through a `vt100::Parser`-backed ratatui backend and snapshot the resulting screen contents — never widget internals.
- **Property tests** — `proptest` for parsers (TOML override values, agent payloads, framing), serializers (round-trip schema types), and state-machine transitions (no path leaves a final state).

Snapshot churn caused by transient IDs is a redactor bug, not a test failure — fix the redactor.

## Dependency budget — current snapshot

Current snapshot — entries move when a better-designed alternative wins on design fit, maintenance, footprint, and security. Adding, replacing, or removing a row needs a one-paragraph PR justification.

| Tier | Crates |
| --- | --- |
| **Runtime — core** | `clap`, `serde`, `serde_json`, `tokio`, `tracing`, `tracing-subscriber`, `thiserror`, `uuid`, `jiff` |
| **Runtime — utility** | `tempfile`, `fs4`, `which`, `sha2`, `hex`, `nix` (Unix sockets, sigaction) on `cfg(unix)` |
| **Binary boundary only** | `anyhow` — permitted in `crates/rimz/src/main.rs`, the private `cli/` module tree, `crates/rimz-sidebar/src/main.rs`, and `xtask/` |
| **Sidebar runtime** | `ratatui` (via its `crossterm_0_29` feature); direct `crossterm` only when sidebar I/O actually requires it |
| **Tests** | `insta`, `proptest`, `assert_cmd`, `predicates`, `vt100`, `tempfile`, `pretty_assertions`, `portable-pty` |

Rules:

- Runtime deps update `deny.toml` and pass `cargo deny check`.
- Prefer std plus a small set of well-chosen crates over a transitive dependency tree.
- A new dep, or a replacement, needs a one-paragraph PR justification — what it provides, what it replaces, why we don't write the moral equivalent in twenty lines.
- `unsafe` requires a `// SAFETY:` comment naming the invariant and a code-owner review.
- An incumbent the table no longer lists is no longer accepted. New uses are caught by `cargo machete` and `cargo deny`. Crates removed in past snapshots so far: `chrono` (replaced by `jiff`), `bytes`, `tokio-util`.

## Toolchain and quality gates — current snapshot

### Toolchain

The stable channel is pinned in `rust-toolchain.toml`. No Cargo.toml carries `rust-version`. Required components: `rustfmt`, `clippy`, `llvm-tools-preview`.

### Quality gates

Every gate runs in CI with warnings treated as errors. Local equivalent is `cargo xtask <task>`.

- `cargo fmt --all -- --check` — formatting.
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` — lint.
- `cargo nextest run --workspace --all-features --locked` — test runner.
- `cargo test --workspace --doc --locked` — doctests.
- `cargo deny check` — licence, advisory, and ban check.
- `cargo machete` — unused dependency check.
- `cargo vet` — supply-chain audit.
- `cargo llvm-cov` — coverage.
- `cargo semver-checks` — release-time API check.

### Contributor command surface

`cargo xtask <task>` is the entry point. Tasks: `fmt`, `lint`, `test`, `deps`, `deny`, `vet`, `coverage`, `ci`. Shell scripts are not added; new automation lands in `xtask/`.

## Reading order for new contributors

1. [AGENTS.md](../../AGENTS.md) — engineering principles and implementation rules.
2. This file — module shape and idioms.
3. [ARCHITECTURE.md](../../ARCHITECTURE.md) — where the modules live.
4. [ledger.md](../internals/ledger.md) and [testing.md](./testing.md) — the two contracts that touch every module.
