# Rust conventions

The shape every module in `crates/rimz` follows: the CLI, output, errors, identifiers, state, durable writes, long-lived processes, clocks, tests, and the dependency budget. Pick the closest existing module before inventing one: the team values consistency over novelty. Read the root [AGENTS.md](../../AGENTS.md) first, and [ARCHITECTURE.md](../../ARCHITECTURE.md) for where the modules live. The gates that enforce these conventions are in [gates.md](./gates.md), and the toolchain, CI, and release packaging are in [build-and-ci.md](./build-and-ci.md).

A path joined onto a room's state or runtime root is built only in `disk/paths.rs`, through the lifetime-class path API. Outside tests, `cargo xtask invariants` rejects a `.root.join(`, `.runtime_root.join(`, or `.rimz_root.join(` anywhere else. A module may join a file name onto a class directory that API returned. Provider trees, host skill directories, and sandbox mount views are not room roots.

## CLI shape

The CLI is parsed with `clap` derive. Shared option groups are their own structs, embedded with `#[clap(flatten)]` so each subcommand inherits them without restating fields.

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

- The [CLI layer contract](../../crates/rimz/src/cli/AGENTS.md) draws the boundary between handlers and library-owned workflows.
- The bare `rimz` default action targets `.`. Path-taking launch stays on `rimz start [PATH]`, so a mistyped subcommand reaches clap as an unknown command.
- `bin_name = "rimz"` keeps help text stable when the executable is invoked through a platform-specific path.
- Subcommands are a flat `enum Subcmd { ... }`, with `#[clap(visible_alias = "<short>")]` for shorthand and `#[clap(hide = true)]` for hooks-only or internal subcommands.
- A clap `value_parser` refusal quotes the whole rejected value and prints only the error's own text, not its source. Parse a value that can be large or whose cause matters (a JSON payload, a file's text) in the command, and refuse with `cli::usage::UsageError` so the exit code stays 2.
- A free-text positional (a note, text, prompt, instruction, or message body) sets `allow_hyphen_values = true`, and its reference page names `--` as the escape for a value that is exactly one of the command's flags, `-h`/`--help` included. Agent and subagent launches reserve `--` for provider passthrough (`#[arg(last = true)]`), so a print run reads such a prompt from `rimz agents -p --stdin` and a subagent reads it from `--prompt-file`.
- A doc comment on an `Args` struct prints in the `--help` of every subcommand that flattens it, since clap derive applies it to the command it is flattened into. When those verbs take different input, set the help per verb and assert it per verb.
- A mode that refuses flags another mode accepts first lists every clap constraint on those flags (`rg -n 'requires|conflicts_with|required_unless' <the Args file>`). Clap checks `requires` and `conflicts_with` before the mode's own check runs, so the user gets exit 2 and a fix that is wrong for the mode.
- A command that starts refusing or asking where it proceeded, or a flag renamed or removed, is read against its scripted callers in `loops/`, `xtask/src`, `scripts/`, `.github`, and the integration tests: they run with no terminal and take the refusal path, while a test that pins only the argv still passes.

## Stdout and tracing

Stdout is the protocol surface. The workspace `Cargo.toml` holds the lint wall once: `[workspace.lints.clippy]` denies `print_stdout` and `print_stderr` in every crate, and `[workspace.lints.rust]` denies `unsafe_code`. Both binary crate roots restate the stdout rule as `#![deny(clippy::print_stdout)]`. Two unsafe sites are admitted, each under `#[expect(unsafe_code, reason = "...")]`: the exported jemalloc configuration in `crates/rimz/src/main.rs`, and the counting allocator in `crates/rimz/benches/fleet.rs`, which only the fleet bench links and which gives the scale gate its deterministic allocation counts. A new unsafe site is a design decision.

A `println!` site is legal only under `#[expect(clippy::print_stdout, reason = "...")]`, and only when its stdout is one of:

- a `--json` event stream or the final user-facing message of a command;
- a single scripting value (`message` ids, launched agent names);
- the agent-native decision channel: `rimz hooks <agent> ...` stdout is parsed by the agent ([Hook stdout is the decision channel](../internals/agents/adapter.md#hook-stdout-is-the-decision-channel));
- the `doctor` multi-section diagnostic, a bespoke layout that moves to `render` opportunistically.

Human-facing tables, key/value blocks, and listings render through the shared `cli/render` layer. `render::out()` returns an `anstream::AutoStream` over stdout that strips ANSI when output is piped or color is disabled (`NO_COLOR`/`CLICOLOR`, or `--color never`), so `--json` and snapshot output stay byte-clean. It writes through `writeln!`, so it needs no `#[expect]` site.

Colors come from `render::palette`, which lazily resolves the machine's scheme, slot overrides, depth, and provider styles through the shared theme core. State tones resolve once in `render::status`, keyed on typed status values rather than rendered strings. Provider identity, state, hierarchy, and quantities follow the [interface language](../internals/theme.md#interface-language). Raw protocol surfaces stay theme-independent.

All other output flows through `tracing` to stderr:

```rust
const DEFAULT_LOG_FILTER: &str = "warn";

fn stderr_env_filter() -> tracing_subscriber::EnvFilter {
    EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(DEFAULT_LOG_FILTER))
        .unwrap_or_else(|_| EnvFilter::new("warn"))
}
```

The default filter prints warnings and errors; `RUST_LOG` opts into more. Terminal UI binaries default to `off` so logs do not corrupt the pane, and still honor `RUST_LOG`. Subscribers are installed once at the binary entry, never in library code. Span fields populated downstream are pre-allocated with `tracing::field::Empty`:

```rust
let span = tracing::info_span!(
    "rimz.message.deliver",
    workspace.id = field::Empty,
    message.id = field::Empty,
);
```

## Error types

One `thiserror` enum per module, named after the failure mode it represents rather than with a generic `*Error` suffix:

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

The `Result<T>` alias lives next to the enum it shadows. Predicates like `is_recoverable()` or `is_retryable()` go on the enum, not at call sites. `#[from]` covers plain system-error conversions; structured context is kept as fields on the variant.

`anyhow` is allowed only at the binary boundary: `crates/rimz/src/main.rs`, the private `cli/` module tree, and `xtask/`. Library modules return their own typed `Result`.

Keep a field required when only tests need to omit it: give the tests a constructor helper instead of adding a production default, which would hide a missing value behind shipped defaults.

## Identifier newtypes

Every identifier that travels through the schema, the store, or the wakeup socket is a newtype, so no bare `String` or `Uuid` flows through a public API.

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

- The inner value is never `pub`; `pub(crate)` only when the same crate needs the unwrapped form for an FFI seam.
- Identifiers RimZ mints (`RunId`, `EventId`, `SidebarInstanceId`, and other internal correlation IDs) are UUIDv7, so files named after an ID sort chronologically without an index.
- Identifiers derived from external truth keep their natural shape (`WorkspaceId` is the SHA-256 of `project_root`; `PaneId` is `"<mux>:<raw_pane_id>"` per [multiplexers.md](../internals/multiplexers.md)) and still go through a newtype and a parser, never assembled inline.

## State machines as types

Lifecycle states are enums with predicate methods, never booleans where a discriminated state would do.

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

The enum carries the vocabulary and the store boundary carries the rule: the lifecycle rules in [messaging.md](../internals/harness/messaging.md) (record first, claim before send, terminal is final) live in `store/message_store.rs` and `store/writer/queue.rs`, not in the status enum. `AgentStatus`, `PermissionPosture`, gates, and delivery outcomes take the same shape.

## Durable-state handle pattern

A short-lived CLI invocation reaches durable state through a `Clone`able handle holding `Arc<Inner>`, whose public methods each take the workspace lock for their critical section and do the file work directly. The workspace lock serializes across processes, and with one writer process per workspace at a time an in-process actor would add nothing.

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

- Public methods return `Result<...>` and take the workspace lock for their critical section.
- Best-effort wakeups (sidebar fanout, the run-socket datagram) fire after the lock drops, so the store commit lands before any notification.
- The handle is `Clone` through `Arc<Inner>`, cheap to pass into spawned tasks.
- Durability barriers carry `#[must_use = "durability barrier; check the result"]` so a dropped `Err(Fsync(...))` is a compile-time warning rather than silent data loss.

The actor shape is for a long-lived process with concurrent in-process writers, which RimZ does not have (a watcher daemon or a scheduler host would be one): `tokio::mpsc` for commands, a `oneshot::Sender` per durability barrier, and a terminal-failure cell behind `Arc<Mutex<Option<Arc<Err>>>>` so later callers learn why the actor died.

## Long-lived processes

A process that outlives one command (the sidebar host, `stats --refresh`, the Codex broker, `lsp serve`, the exec wrapper, supervisors, and followers) keeps its thread count and its memory bounded:

- It reuses one named worker and does not call `thread::spawn` in a loop or per event. The LSP broker's listener (`lsp/broker/socket.rs`) is an exception: it gives each accepted connection a thread.
- One shape of fan-out is admitted: work split per pass across a constant worker cap, run under `thread::scope`, and joined before the pass returns. No thread outlives the pass, and the count never grows with the input. `bounded_map` in `utils/parallel.rs` is that helper for independent fan-out. A persistent pool would need `'static` jobs and would clone what the scoped form borrows.
- Every collection the process holds across iterations carries a bound or an eviction rule stated beside it.
- A deadline built from a configured or user-supplied duration uses `Instant::checked_add` and falls back to waiting without a deadline. `Instant + Duration` panics on overflow, which kills the worker.
- Where a measurement counts work by thread name, shared workers start from a thread named for them. On Linux a thread spawned without `thread::Builder::name` takes its creator's name (`/proc/<pid>/task/<tid>/comm`). The sidebar host's room lanes start from `rimz-plane` for this reason, and never from the first `rimz-attach` thread.

A reader that checks a cached value against a generation the writer stores after publishing the value reloads only when the stored generation is ahead of its cache (`>`), never when the two differ: between the writer's two stores the cache can already hold the newer value, and an inequality check then reloads it on every call. The sidebar host's per-pane views-record cache (`ViewAccess::record` in `sidebar_pane/app/loop_state/shared.rs`) is such a reader. A test of one leaves the value a generation ahead of the stored generation and counts the reloads.

Reuse workers and bound retained state rather than relying on allocator tuning. On Linux x86-64 and aarch64, glibc and musl builds alike select jemalloc for Rust and C allocations in `crates/rimz/src/main.rs`, with immediate dirty-page decay (`dirty_decay_ms:0`), extents kept off transparent huge pages (`thp:never`), and defaults otherwise. Other targets and a [heap-profiling build](../internals/profiling.md#heap-profiling-build) keep their platform allocator. Base metadata can still use huge pages under THP `always`, an accepted cost the [allocator measurements](../internals/performance.md#anti-patterns) record.

## Timestamps and clocks

These rules hold for any change that stamps, compares, or waits on a time. The root rule, [prove timing at the consumer](../../AGENTS.md#before-a-change), comes first.

- A stamp another rule compares with is taken at the instant that rule means, and keyed on the state that rule reads: a stamp set on a raw status misses the rows a consumer partitions by their displayed status (a clean turn end parked on background work is raw running and displays as success).
- A floor that must expire with an existing cadence takes that cadence's own stamp, passed in, not a fresh `now()` after the pass's work, or it outlasts the cadence and skips the next due pass.
- A freshness stamp checked against later triggers is taken before the request it covers, or a trigger that lands during the request is absorbed.
- A fix that changes which clock stamps a value audits every comparison against that value for its behaviour at ties, since a stamp that used to trail another may now equal it.
- A move onto an injected clock keeps every read that followed a lock, I/O, or spawn after that work, and converts between clocks at the present, never at construction.
- A state a polling consumer reads stays published for at least one of that consumer's read intervals (its backstop tick plus one pass); a design that withdraws such a state names that interval beside the rule that withdraws it.
- A state that changes with time alone reaches a reader sleeping on a stored retry time only if that time is computed from the change's instant.
- A change that moves a record's deadline lists every consumer that enforces it (`rimz lsp refs` on the field, plus any caller that keeps its own clock from a `--timeout` flag) and every input the new value is computed from: a message unpark once rearmed a run's `deadline_at` while the foreground waiter still timed the run out by its own clock.
- A cached answer reused while a key is unchanged keys its reuse on every input the answer reads, configuration included.

## Atomic writes

Write shapes in `disk/atomic.rs` cover every disk write in the project:

- `write_temp_then_rename(path, value)` for cold-path durable state (trust grants, workspace records, hook installs, the rotation carryover); `write_temp_then_rename_cache` (rename-atomic, no fsync) for liveness files and rebuilt-on-next-read caches.
- `publish_staged_no_replace(staged, destination)` for already-synced files or directories. It refuses any existing destination entry and syncs the destination's parent after publication.
- `publish_staged_replace(staged, destination)` for a complete staged file. It syncs the file, replaces the destination by rename, then syncs the parent; immutable price-table readers keep their old inode until they reopen.
- `append_record_bytes(path, line) -> Result<()>` for the event log: one `write()` per record and no fsync, since appended frames become durable through the write tail's debounced group fdatasync and rotation's pre-rename sync. The frame encoding lives beside its decoder in `store/event_log/frame.rs`.

These primitives live next to the durability contract they enforce. No module hand-rolls its own temp-file dance, and every fsync syscall lives in `disk/atomic.rs` (`cargo xtask invariants`), counted through its `testkit` seam so the performance tier can assert fsync budgets from the integration binary. See [store.md](../internals/store.md) for the frame format, torn-record recovery, and rotation rules.

## Tests

`cargo xtask test` runs the suite through nextest ([the test runner](./running-gates.md#the-test-runner)). The profile a test runs in and the environment it gets are in [gates.md](./gates.md#test-profiles).

Each kind of test has its own rules.

### Unit tests

- A unit test lives in `#[cfg(test)] mod tests` in the module under test and covers pure logic: state-machine transitions, parser shapes, schema round-trips.
- By default it uses no filesystem, network, or subprocess. A tree's `AGENTS.md` names an exception: the sidebar's launch tests use isolated tempdirs for heartbeat files.
- The module is inline by default. Past 500 lines the whole module moves to a sibling file (`#[cfg(test)] mod tests;` plus `view/tests.rs`) with the same module path and private access. `cargo xtask invariants` enforces the cap.
- Make the move in the same change when an edit leaves the inline module within about 20 lines of the cap. A module's unit tests live in one place, inline or sibling.
- A sibling `tests.rs` that grows splits into nested modules by concern, then into a `tests/` directory module. It never returns tests to the source file.
- An outgrown unit-test module is a prompt to check whether the weight belongs in a domain module or the integration tier.

### Integration tests

- The crate has one integration binary at `crates/rimz/tests/integration/main.rs`. Each suite is a module (`mod hooks;`), and related suites group under a subdirectory module (`mod backend;` over `backend/tmux/` and `backend/zellij/`).
- The tests use real subprocesses, real temp directories under `tempfile::TempDir`, and real store files.
- Spawn `rimz` through the `Env` harness in `crates/rimz/tests/integration/common/`, which builds a `std::process::Command` on the built binary.
- Suite layout and local rules are in the [suite contract](../../crates/rimz/tests/integration/AGENTS.md).

### Performance gates

- Integration tests under `crates/rimz/tests/integration/performance/` assert deterministic work bounds, not timing.
- Counters live at the funnel they measure: fsyncs in `disk::atomic::testkit`, event-log bytes in `store::event_log::testkit`, and hot-path subprocess attempts in `proc::testkit`.
- The crate-level `testkit` feature exposes test seams to tests and benches: synthetic fleet builders, counter readers, and rendezvous points for subprocess tests. Shipped artifacts do not enable it.
- Nothing has to pass `--features testkit` by hand. `crates/rimz/Cargo.toml` carries a self dev-dependency (`rimz = { path = ".", features = ["testkit"] }`), so any build that pulls dev-dependencies resolves the feature on: `cargo check --tests`, `cargo nextest`, `cargo bench`, and the crate graph rust-analyzer builds from `cargo metadata`. `cargo build`, the shipped binary, and the install-host lint passes never see it.

### Performance benches

- `crates/rimz/benches/` holds divan benchmarks, including explicit regression ceilings that fail `cargo xtask perf`.
- They measure wall-clock and allocation figures over synthetic stores, pane frames, and sidecars, never real agents. `hook_stop_budget` is the exception when `RIMZ_HOOK_BUDGET_HOME` names a home for it to copy.
- CI does not run the benches.

### Snapshot tests

- Use `insta::assert_snapshot!` for every protocol stdout (CLI, hook, `--json` events), failure shapes included.
- For a clap command or flag change, include `cargo xtask test --name cli_surface_is_stable` in the focused pass. A new or renamed `rimz agents` subcommand, hidden ones included, also joins the non-launch verbs of `crates/rimz/src/pane.rs::command_is_launch_chrome`; run `cargo xtask test -E 'test(/cli_surface_is_stable|launch_chrome/)'`.
- For a changed one-line doc on a top-level command (its `Subcmd` variant in `crates/rimz/src/cli/mod.rs`), include `cargo xtask test --name top_level_command_lines_fit_eighty_column_help`: it caps each line of the grouped help at 80 columns.
- Normalize UUIDs, timestamps, absolute paths, and other transient identifiers at the assertion boundary before the snapshot. Snapshot churn from a transient ID is a bug in the test helper, fixed in its normalization.
- A shared helper is for normalization that several suites need.
- Sidebar render tests draw through a `vt100::Parser`-backed ratatui backend and snapshot the screen contents, never widget internals.
- `INSTA_UPDATE=new` writes an `assertion_line:` header into each `.snap.new`. Accept with `cargo insta accept`, or strip the header before you commit a hand-moved `.snap.new`.

### Property tests

Use `proptest` for parsers (TOML override values, agent payloads, framing), serializers (round-trip schema types), and state-machine transitions (no path leaves a final state).

### Writing a test

- A provider stub that stays alive answers informational probes such as `--version` at once; a stub that sleeps through one spends the launch's startup deadline before the provider is spawned, and the failure appears only under full-suite load.
- A unit test does not wait out a production command deadline (`LIST_SESSIONS_TIMEOUT`, `COMMAND_TIMEOUT` in `crates/rimz/src/mux/command.rs`): when the path behind a deadline matters, the test reaches it through a shorter bound it passes in, or through a cheaper failure such as a missing binary.
- Do not branch production code on `cfg!(test)` to pick a dependency. Inject the dependency (a `Host` trait) and let tests pass the fake.
- When a change adds a verdict, marker, or timer to an existing state machine, enumerate how the new input interacts with each existing one and test the plausible simultaneous cases, at minimum a structural and a geometry change arriving together, and a pending classification against every backstop and retry path. Single-input tests pass on a machine whose bugs all live in the combined states.
- A test of a polling consumer holds each state the consumer must observe open for at least three poll intervals: a state that opens and closes between two polls is never seen, and the test then passes on the broken build.
- The negative control for a regression test is the fix commit's files checked out from its parent, run several times when the test is timing-based, never a hand-made mutant of the fixed code.
- A test of a bounded retry or poll includes a case that succeeds only after several retries and a case asserting that giving up waits at least the bound. Besides the parent-tree negative control, run these cases once against the loop cut down to a single retry: one of them must fail, or the tests do not pin the loop over the simpler design.
- A test of a timing floor or rate limit queues the inputs it means to merge before the worker can wake (sent from inside the cycle, or held behind a barrier) instead of racing a sleep against the floor; it asserts a delay's lower bound against the floor and its upper bound against the floor plus generous slack.
- A table test that must be shown red first collects every case's result and asserts the whole vector in one `assert_eq!`, so one failing run shows each case.
- A test that asserts on a value it captured from a run (an error string, a child's output, a pid parsed from either) prints that value in the assertion's message: a flake under load fails once, in a log nobody can rerun, and a bare `assert!(error.contains(..))` or `.expect(..)` leaves its cause to inference. The [integration suite contract](../../crates/rimz/tests/integration/AGENTS.md) applies the same rule to event counts.
- A test or bench that scans `/proc/<pid>/task` skips a task that exited mid-scan on ESRCH as well as on `NotFound`: a task gone after `read_dir` lists it reads as ENOENT, but one gone after its file opens fails the read with ESRCH, which matches no stable `io::ErrorKind`, so the check reads `raw_os_error()`.
- A test helper that asserts for its callers is `#[track_caller]` and asserts in its own body, never inside a closure or a thread it runs: closures do not inherit `#[track_caller]`, so a panic there names the helper's line instead of the caller's step.

## Dependency budget

The direct dependencies by tier. An entry moves when a better-designed alternative wins on design fit, maintenance, footprint, and security. Each entry's justification (what it provides, what it replaces, why RimZ does not write the equivalent in twenty lines) is a comment beside it in the workspace [Cargo.toml](../../Cargo.toml).

| Tier | Crates |
| --- | --- |
| **Runtime: core** | `clap`, `serde`, `serde_json`, `toml`, `tokio`, `tracing`, `tracing-subscriber`, `tracing-log` (the original place of a `log` crate record for repeat suppression; already in the tree through `tracing-subscriber`), `thiserror`, `uuid`, `jiff` |
| **Field paths** | `serde_ignored` (unknown-field paths for the config and pack manifest loaders), `serde_path_to_error` (the dotted path of the field a workflow definition refuses at, without parsing error prose) |
| **Runtime: utility** | `tempfile`, `which`, `sha2`, `md-5` (Cursor's provider-defined workspace bucket), `hex`, `prost` (Cursor's version-pinned pending-call protobuf field), `foldhash` (spend-aggregation maps), `crc32fast` (event-log frame checksum), `flate2` (gzip streams for embedded pricing and self-update archives), `tar` (single-entry release archive extraction; xattrs disabled), `image-webp` (pure-Rust decode for WebP pet sprite sheets), `png` (pure-Rust decode for PNG pet sprite sheets, including petdex installs), `rusqlite` (bundled; read-only parse of OpenCode and Cursor SQLite stores, plus the shared immutable price table), `toml_edit` (format-preserving `rimz config set`), `kdl` (structured merge of Zellij's `permissions.kdl` cache), `shlex` (launch-flag strings to argv without a shell), `semver` (Cargo version requirements for pack compatibility), `glob` (`.worktreeinclude` pattern expansion), `similar` (hook-install consent diffs), `notify` (filesystem-watch latency hint over the tick backstop), `terminal_size` (sidebar birth-size probe), `ureq` (rustls; the pricing-refresh and self-update HTTP client, shared with `xtask pricing-refresh`), `tungstenite` (RFC6455 client over the Codex daemon's UDS control socket), `tiktoken-rs` (embedded `o200k_base` BPE behind the `*.output` token estimate), and on `cfg(unix)`: `nix` (sockets, signals, process and user queries) and `signal-hook` (signal-to-handler plumbing); on Linux x86-64 and aarch64 (glibc and musl): `tikv-jemallocator` (the global allocator, overriding C `malloc`) |
| **Observability (opt-in)** | `sentry` (default features off; `backtrace`, `panic`, and the `ureq` transport, with `contexts` omitted so the Apple `objc2` UIKit tree never links) and `sentry-tracing`, optional behind the non-default `sentry` feature so shipped artifacts exclude the whole tree. The `ureq` transport reuses the rustls client already pinned for pricing; `sentry-tracing` bridges the existing `tracing` subscriber rather than adding a parallel reporting API. Confined to `src/observability/` and the binary boundary in `main.rs` |
| **Contributor analysis** | `scip` plus its `protobuf` decoder: xtask-only parsing of rust-analyzer's exact-reference index for Atlas; neither links into RimZ runtime artifacts |
| **Binary boundary only** | `anyhow` ([Error types](#error-types)) |
| **CLI presentation** | `anstyle`, `anstream`, `colorchoice`, `unicode-width`: the clap-native styling stack behind `cli/render`; all four are already in the tree transitively through clap, so promoting them to direct deps adds zero footprint. `pulldown-cmark` provides the markdown event stream behind `cli/render`'s prose renderer |
| **Sidebar runtime** | `ratatui` (via its `crossterm_0_29` feature); direct `crossterm` only when sidebar I/O actually requires it |
| **Zellij plugin (wasm-only)** | `zellij-tile` (the official plugin API) and its same-version `zellij-utils` dependency for `MouseEventType`, which tile does not re-export. Both are `cfg(target_family = "wasm")` dependencies of `crates/rimz-presence-zellij` alone, so no host artifact links them. Their tree is excluded from `deny.toml`'s audited targets (the wasm executes inside Zellij's plugin sandbox) and covered by `cargo vet` at `safe-to-run` |
| **Tests** | `insta`, `proptest`, `assert_cmd`, `predicates`, `vt100`, `tempfile`, `portable-pty` |
| **Performance benches** | `divan`: dev-only, small benchmark harness with built-in allocation profiling; links only into `[[bench]]` targets |

- Adding, replacing, or removing a row needs a one-paragraph PR justification and a matching comment in the workspace `Cargo.toml`.
- Runtime deps update `deny.toml` and pass `cargo deny check`.
- Prefer std plus a small set of well-chosen crates over a transitive dependency tree.
- A crate the table does not list is not accepted, and `cargo machete` and `cargo deny` catch a new use. Removed crates stay out: `chrono` (replaced by `jiff`), `bytes`, `tokio-util`, and `fs4` (replaced by std `File` locking in Rust 1.89).
