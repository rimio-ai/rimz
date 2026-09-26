# Shared language servers

`rimz lsp` queries shared language servers and attaches editors to them. Agent launches and editor attachment register servers for a checkout; requests start or restart dormant servers when memory permits. For lifecycle and memory accounting, see the [internals](../../internals/lsp.md).

## Queries

```sh
rimz lsp def MuxBackend
rimz lsp refs GcReport
rimz lsp hover Store::open
rimz lsp impl MuxBackend
rimz lsp callers sweep_locked
rimz lsp callees Store::open
rimz lsp symbols crates/rimz/src/lib.rs
rimz lsp find MuxBackend --server rust --json
```

| Verb | Target | Result |
| --- | --- | --- |
| `def` | position or exact symbol name | definition locations |
| `refs` | position or exact symbol name | reference locations |
| `hover` | position or exact symbol name | type and documentation |
| `impl` | position or exact symbol name | implementation locations |
| `callers` | position or exact symbol name | incoming call hierarchy items, inside the checkout by default |
| `callees` | position or exact symbol name | outgoing call hierarchy items, inside the checkout by default |
| `symbols` | file path | document symbol outline |
| `find` | search string | workspace symbols matching the server's search |
| `check` | Markdown notes file | anchor verdicts and coverage summary |

Positions are `path:line:col`, with one-based line and column; omitting the column is an error. Paths are checkout-relative or absolute. Text locations use `path:line:col`; navigation includes source lines when available, outlines indent children, and hover prints markup. Empty answers for resolved targets print `no results`.

Symbol names strip a trailing `()`, leading `crate`, `self`, and `super` segments, and generic arguments (`<…>`) from each segment. The last `::` segment is matched exactly against workspace symbols. The remaining segments must occur in order in the candidate's checkout-relative file path and container, but may skip intermediate segments: `launch_reminders::render`, `harness::launch_reminders::render`, and `rimz::harness::launch_reminders::render` can identify the same function. File extensions, `src` components, and final `mod`, `lib`, or `main` file stems do not contribute segments. Outside-checkout candidates contribute only their container. Wrong qualifiers never fall back to a bare-name match. Inline `mod` names are not available from workspace symbols: `query::tests::foo` is not found when only the file path `query::foo` is known; the hint lists the accepted spelling.

Matches sharing one definition count as one symbol, including re-exports; distinct definitions list candidates instead of guessing. Rerun with a listed name or position; a listed name that comes back ambiguous again (the same item in several crates, several trait impls on one type) needs the position. Candidate lines in `find`, flat `symbols`, not-found, and ambiguous output use `{kind} {qualified name}  {path:line:col}`. Qualified names start after the first `src` component, include the container, and are accepted as input. Not-found lists all exact-name candidates, collapsed by definition the same way, even when the qualifier matches none. Not-found and ambiguous text lists show at most 20 candidates, followed by the remaining count; JSON and `find` are uncapped. Candidates sort by descending count of qualifier segments present in their path, then qualified name and position.

`find` sorts case-insensitively by exact name, prefix, substring, then other matches, with qualified name and position breaking ties. This order applies to text and JSON.

All eight verbs require one target. Query flags:

| Flag | Effect |
| --- | --- |
| `--server <NAME>` | Select a configured server name. Otherwise a file's extension selects among checkout entries, or the sole entry is used. Ambiguous selection names this flag in the error. |
| `--json` | Print the structured LSP result on exit 0. Exits 5 and 6 print an object with `outcome` (`not-found` or `ambiguous`), the written `name`, and `candidates`, each containing the qualified `name`, kind word `kind`, and `position` (`path:line:col`). |
| `--external` | Callers and callees only: include items outside the checkout. Default text output hides these items and ends with `<n> outside the checkout hidden; add --external to show them` when any were hidden. `--json` is always unfiltered. |

The checkout comes from cwd or the global `--root`, using the most deeply enclosing registered checkout when present. Different worktrees have different servers even though they share a room. Queries wait up to 30 seconds for indexing; there is no CLI wait-duration flag.

## Check anchors in a notes file

`rimz lsp check <FILE> [--json]` checks a note's file, symbol, and line references before hand-off. FILE is cwd-relative or absolute; anchors use the checkout enclosing cwd (or global `--root`). There is no `--server` flag.

Only inline code spans count, never fenced blocks or prose. A path must have a filename extension holding at least one letter before the first colon, so `127.0.0.1:8080` is not an anchor. Bare paths, bare symbols, and module names such as `config::Error` are ignored. Accepted anchors are `src/config.rs::Type::method`, `src/config.rs::Type.field`, and line-only forms such as `src/config.rs:42`, `src/config.rs:~42`, `src/config.rs:42:3`, or `src/config.rs:42-45`.

Symbol chains use either `::` or `.`. Generic arguments are removed; decorations end the symbol at whitespace, `(`, `{`, `[`, `,`, `~`, `#`, `!`, `=`, or `;`; trailing colons and dots are removed. Thus `config.rs::ConfigErr::Definition(DefinitionErr)` checks `ConfigErr::Definition`. A line hint inside the span after the symbol, or immediately following the span, accepts `(~42)`, `(42)`, `(~42-45)`, `(42-45)`, `~42`, `:~42`, `:42`, and `:42:3`. Symbol hints must overlap a matching outline range widened by three lines at each end. Every line of a line-only hint must exist in the file; it needs no server.

Paths resolve exactly first, then by a unique component-boundary suffix among tracked and untracked, non-ignored files present on disk; a deleted file whose removal is not yet staged is `missing-path`. Lengthen an ambiguous suffix instead of guessing. Symbols match an outline node and an ordered subsequence of its ancestors, ignoring generic arguments. A container's final whitespace-separated token also matches, so a method under `impl Type<T>` matches `Type::method`. Multiple matching nodes are accepted; a hint need only match one.

| Status | Meaning |
| --- | --- |
| `ok` | Resolved path and symbol or valid line-only anchor. |
| `missing-path` | No checkout file matches. |
| `ambiguous-path` | Several files match the suffix. |
| `missing-symbol` | No outline node matches the chain. |
| `line-outside` | The hint misses every matching range, or lies outside the file. |
| `unchecked` | No configured server covers the extension; verify it by hand. |

Text prints one line per failing or unchecked anchor: `<notes>:<line>  <status>  <anchor as written, hint included>  <detail>`. Details show candidate files, near-miss symbols and ranges, file length, or the missing server. The last line is `<N> anchors in <notes>: <ok> ok, <failed> failed, <unchecked> unchecked`. Exit 7 means at least one failure; exit 0 includes unchecked anchors and files with no anchors. An unreadable note or non-git checkout exits 1. A configured server that cannot answer aborts before printing any verdict, with the query's exit 1, 3, or 4.

JSON includes every anchor, including `ok`. The fixed top-level keys are `notes`, `checkout`, `anchors`, and `summary`. Each anchor has `line`, `text`, `status`, `path`, `symbol`, `hint`, `range`, `candidates`, `files`, and `detail`. Paths are checkout-relative or null; symbols are segment arrays or null for line-only anchors. Hints and ranges are inclusive one-based `[start, end]` pairs or null. Candidates have `name`, `kind`, and `range`; files lists ambiguous paths. Summary has `anchors`, `ok`, `failed`, and `unchecked` counts.

## Attach

```sh
rimz lsp attach --server rust
rimz lsp attach --server rust --root /path/to/checkout --stdio
rimz lsp attach --version
```

`attach [--server NAME] [--stdio] [--version]` bridges an editor's stdin and stdout to one shared server. Stdio is the only transport; `--stdio` is optional. Except for `--version`, stdout contains only Content-Length-framed LSP messages; diagnostics and admission waits go to stderr. The first input frame must be `initialize`, otherwise the command fails with `expected initialize`. `--version` prints `rimz lsp attach <version>` and exits 0 without reading input or loading configuration.

The global `--root` takes precedence over the editor's `initialize.rootUri`, then its first workspace folder, then cwd. Editor roots must be file URIs. The most deeply enclosing live registered checkout is used when present; otherwise workspace resolution supplies the worktree root. `--server NAME` selects a configured server. Without it, exactly one configured server must have a matching root marker; zero or several matches fail with an error naming `--server`.

Attachment joins an existing broker or admits a new one using the configured trust, executable, and memory checks. Creating a required server can wait up to `wait-timeout`, printing queue position and remaining time to stderr every five seconds. A stopped entry is retried for five seconds before refusal. An optional server's memory refusal happens inside the LSP session, as failed requests and an editor message, not as a launch-time exit 3.

Editor EOF exits 0. Broker refusal (including an older broker without attachment support), connection failure, or the broker closing first exits 3 with `language server <name> for <root> is <reason>` on stderr. A required-admission queue timeout also exits 3, with the queue-timeout diagnostic. Disconnecting releases the editor's lease; it does not stop a server still leased by other clients. See [editor setup](../../guide/lsp.md#use-it-from-your-editor) for supported Rust configurations.

## Shim

```sh
rimz lsp shim --server rust
rimz lsp shim --server rust --dir /path/to/bin
```

`shim --server NAME [--dir DIR]` writes executable `DIR/rimz-lsp-NAME` and prints its path. DIR defaults to `~/.local/bin` and is created if absent. The shell script invokes the resolved RimZ executable with `lsp attach --server NAME`, forwarding all arguments. It replaces an existing file only when it starts with RimZ's shell-script header and attachment-shim marker; other files are refused. It does not change editor settings or start a server. Remove the generated file to uninstall it.

## List and stop

```sh
rimz lsp list
rimz lsp list --json
rimz lsp stop --server rust
rimz lsp stop /path/to/checkout --server rust
rimz lsp stop --all
```

`list` covers the whole machine. Text columns are STATE, CHECKOUT, SERVER, RSS, PEAK, REQUESTS, LAST, RESTARTS, and LEASES; RSS through LEASES are right-aligned. CHECKOUT uses `~/` for paths under your home. Running servers sort first, then servers needing attention, then the rest; ties sort by checkout and server name. Dead entries are swept before listing.

STATE pairs a glyph with a word, preserved without color: `✓ ready`, `▸ starting`, `▸ indexing`, `· not started`, `· dormant: idle`, `✗ dormant: crashed`, `! dormant: memory pressure`, or `· stopped: released`. A never-started server shows `not started`; other dormant entries show `dormant: <reason>`, and terminal shutdown shows `stopped: <reason>`.

RSS is current process-tree memory; PEAK is the larger of the recorded peak and the live tree peak, including current tree memory, displayed in decimal byte units. The recorded peak sums the kernel's per-process high-water marks over the tree, with the five-second RSS sample as a floor. RSS and PEAK show `-` when not running. LAST is compact time since the last query (`45s ago`, `11m ago`, `1h ago`, `3d ago`), or `-` with no query. RESTARTS counts starts after a stop, excluding the first lazy start.

`--json` keeps registry order rather than the human table's sort and returns registry entries, including process identities, nonce, state, estimate, timestamps, counters, and leases; peak RSS remains in KiB as recorded, including while dormant.

`stop [CHECKOUT]` defaults to the current checkout (or global `--root`). `--server <NAME>` selects one server when several exist. `--all` stops every machine entry and conflicts with both CHECKOUT and `--server`. Dead entries are swept first; a failure stopping one entry does not prevent attempts on the others. Stop prints one acknowledgment line per entry; no matching entries produce no lines. It has no `--json` flag. A hand stop frees server memory and leaves `dormant: stopped by hand`; the next query can restart it. Stopping an already dormant entry leaves its reason unchanged.

## Status

```sh
rimz lsp status
rimz lsp status /path/to/checkout --server rust
rimz lsp status --server rust --json
```

`status [CHECKOUT]` inspects one server without starting it. CHECKOUT defaults to cwd or the global `--root`; the most deeply enclosing registered checkout is used. Dead entries are swept first. `--server <NAME>` selects a server and is required when several entries match. No matching entry exits 3.

Text shows checkout, server name, state, broker and server pids, request count, and lease count. Each attached editor shows its pid, client name (or `unnamed`), and seconds since attachment, followed by indented buffer paths relative to the checkout where possible. `(owner)` marks the holder whose buffer the server uses; `(unsaved in editor)` means changed since open or save, not a comparison with disk. A buffer already unsaved when opened is not detected, and editing back to disk text does not clear the marker.

`--json` returns the server's current registry entry, including `attached`: each editor has `pid`, optional `name`, `since_ms`, and `open`; each open buffer has `uri`, `owner`, and `dirty`. Buffer text is not included. The [editor model](../../internals/lsp.md#editors) defines ownership and marker semantics.

## Exit codes

| Exit | Meaning |
| --- | --- |
| 0 | Answered, including `no results` for a resolved symbol; successful list, status, stop, shim, or attach version probe; attach ended on editor EOF; check has no failing anchors (unchecked anchors are allowed). |
| 1 | Command failed, including server-selection or protocol errors; details on stderr. |
| 2 | Invalid command line, flag, or argument. |
| 3 | Queries/status: no server for the checkout, memory admission refused, or terminal shutdown, with a reason and grep fallback on stderr. Attach: admission timeout/refusal, broker refusal, or broker connection closing/failing, with the diagnostic described above. |
| 4 | Still indexing after the wait bound. The stderr line gives elapsed time. |
| 5 | Symbol name not found; candidates with that last segment are listed on stdout. |
| 6 | Symbol name ambiguous; candidates are listed on stdout, each with an accepted name. |
| 7 | Anchor check found failures; verdicts on stdout, nothing on stderr. |

Exits 5 and 6 write nothing to stderr. Not-found text is `not found: {written name}`, followed by `; {N} symbol(s) named {last segment}:` and candidate lines when candidates exist. Ambiguous text starts `ambiguous: {N} symbols named {written name}; rerun with one of these names or a position`.

An absent configured server reports `not running`. A broker-side memory refusal reports `no language server for <root> (not started: memory short); use grep`, leaves the broker dormant, and is retried by a later query. Diagnostics do not determine this result. Stop-reason errors use `(stopped: <reason>)`, including `idle`, `evicted`, and `team done`; an ordinary query to a dormant server requests a restart instead.

Other failures follow the [CLI error conventions](../cli.md). `--json` does not change the stderr form of exits 3 and 4.

## Configuration

No servers are configured by default. Put server entries in `~/.rimz/config.toml` or trusted project `.rimz/config.toml`. A project entry replaces the same-named machine entry whole, rather than merging its fields. Project memory-policy keys are refused with a fix pointing to the per-machine config.

```toml
[lsp] # machine config only
reserve-percent = 10
reserve-min = "8G"
kill-floor-percent = 5
idle-timeout = "10m"

[lsp.servers.rust]
command = ["rust-analyzer"]
extensions = ["rs"]
root-markers = ["Cargo.toml"]
init-options = { checkOnSave = false, hover = { dropGlue = { enable = false } }, workspace = { symbol = { search = { kind = "all_symbols", limit = 10000 } } } }
policy = "optional"
wait-timeout = "10m"
memory-estimate = "8G"
```

The Rust options disable checking that would contend with agent builds, include functions in workspace search (rust-analyzer defaults to types only), raise its 128-result search cap, and drop the `needs Drop` section from hover output. RimZ prints hover markdown as the server sends it, and rust-analyzer has no setting for its `Implements notable traits` section, so that section still appears. Without that tuning, symbol-name queries can miss functions; a position query does not need workspace search.

| Machine `[lsp]` field | Default | Meaning |
| --- | --- | --- |
| `reserve-percent` | `10` | Percentage of total memory reserved at admission. |
| `reserve-min` | `"8G"` | Minimum reserve; the larger of this and the percentage wins. |
| `kill-floor-percent` | `5` | Below this percentage of available memory relative to total, the watchdog stops shared servers. |
| `idle-timeout` | `"10m"` | Stop a ready server after this long since readiness or the last query, whichever is later, with no query in flight. Units: `s`, `m`, `h`; checked every five seconds. |

| `[lsp.servers.<name>]` field | Default | Meaning |
| --- | --- | --- |
| `command` | required | Nonempty executable argv, not a shell command. |
| `extensions` | required | Nonempty file extensions without dots, used for selection and saved-file watching. |
| `root-markers` | required | Nonempty checkout-relative paths; any existing marker enables this server for the checkout. |
| `init-options` | absent | TOML table of initialization options passed to the server. |
| `policy` | `"optional"` | `optional` starts on the first query; `required` admits and starts a new server before panes open. Both restart lazily after a stop. |
| `wait-timeout` | `"10m"` | Required-server queue deadline, with duration units such as `20s` or `10m`. |
| `memory-estimate` | `"8G"` | Admission estimate before matching peak history exists. |

Server names use ASCII letters, digits, `-`, or `_`; percentages range from 0 to 100. Sizes use decimal units (`8G` is 8 GB); `8GiB` is binary. An invalid `idle-timeout` refuses launch with the accepted units. Memory admission accounts for running servers' committed growth and cgroup headroom; dormant entries reserve nothing. Query-time admission can evict older idle servers ([admission rules](../../internals/lsp.md#admission)). Required waits print position and remaining time in the launcher's terminal, not the sidebar. Joining an existing broker, even dormant, skips eager admission; later stops and restarts treat required and optional alike.

Project server names, commands, and initialization options join the [trust hash](../../internals/harness/trust.md). An untrusted project declaration or missing executable refuses launch, even with `optional` policy. `rimz doctor` shows servers and the last refusal or queue timeout.

Configured Claude launches deny the native `LSP` tool even while the shared server is dormant or query-time admission is refused, avoiding private servers outside the budget. `rimz agents validate` warns when a profile declares `LSP` and machine servers exist. OpenCode and Grok have no verified native-server suppression here. Existing agents are not reconfigured by editing this table; new launches read it.
