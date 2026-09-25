# Shared language servers

`rimz lsp` queries shared language servers registered for a checkout at agent launch. A query starts or restarts a dormant server when memory permits. For lifecycle and memory accounting, see the [internals](../../internals/lsp.md).

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

Positions are `path:line:col`, with one-based line and column; omitting the column is an error. Paths are checkout-relative or absolute. A symbol name is resolved by exact name from workspace search results; `Type::method` matches `method` in container `Type`. Matches sharing one definition count as one symbol; distinct definitions list candidates instead of guessing. Rerun with a candidate's position. Text locations use `path:line:col`; navigation includes source lines when available, outlines indent children, and hover prints markup. Empty answers print `no results`.

All eight verbs require one target. Query flags:

| Flag | Effect |
| --- | --- |
| `--server <NAME>` | Select a configured server name. Otherwise a file's extension selects among checkout entries, or the sole entry is used. Ambiguous selection names this flag in the error. |
| `--json` | Print the structured LSP result instead of text; ambiguous names print the candidate array. |
| `--external` | Callers and callees only: include items outside the checkout. Default text output hides these items and ends with `<n> outside the checkout hidden; add --external to show them` when any were hidden. `--json` is always unfiltered. |

The checkout comes from cwd or the global `--root`, using the most deeply enclosing registered checkout when present. Different worktrees have different servers even though they share a room. Queries wait up to 30 seconds for indexing; there is no CLI wait-duration flag.

## List and stop

```sh
rimz lsp list
rimz lsp list --json
rimz lsp stop --server rust
rimz lsp stop /path/to/checkout --server rust
rimz lsp stop --all
```

`list` covers the whole machine. Text columns are CHECKOUT, SERVER, STATE, RSS, PEAK, REQUESTS, LAST, RESTARTS, and LEASES. RSS is current process-tree memory; PEAK is the larger of the recorded peak and the live tree peak, including current tree memory, displayed in decimal byte units. The recorded peak sums the kernel's per-process high-water marks over the tree, with the five-second RSS sample as a floor. LAST is seconds since the last query. RESTARTS counts starts after a stop, excluding the first lazy start. Dormant entries show `dormant` or `dormant: <reason>`; terminal shutdown shows `stopped: <reason>`. RSS and PEAK show zero for dormant and stopped entries. `--json` returns registry entries, including process identities, nonce, state, estimate, timestamps, counters, and leases; peak RSS remains in KiB as recorded, including while dormant. Dead entries are swept before listing.

`stop [CHECKOUT]` defaults to the current checkout (or global `--root`). `--server <NAME>` selects one server when several exist. `--all` stops every machine entry and conflicts with both CHECKOUT and `--server`. Dead entries are swept first; a failure stopping one entry does not prevent attempts on the others. Stop prints one acknowledgment line per entry; no matching entries produce no lines. It has no `--json` flag. A hand stop frees server memory and leaves `dormant: stopped by hand`; the next query can restart it. Stopping an already dormant entry leaves its reason unchanged.

## Exit codes

| Exit | Meaning |
| --- | --- |
| 0 | Answered, including no results or ambiguous symbol candidates; successful list or stop. |
| 1 | Command failed, including server-selection or protocol errors; details on stderr. |
| 2 | Invalid command line, flag, or argument. |
| 3 | No server for the checkout, memory admission refused, or terminal shutdown. The stderr line names the reason and grep fallback. |
| 4 | Still indexing after the wait bound. The stderr line gives elapsed time. |

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
init-options = { checkOnSave = false, workspace = { symbol = { search = { kind = "all_symbols", limit = 10000 } } } }
policy = "optional"
wait-timeout = "10m"
memory-estimate = "8G"
```

The Rust options disable checking that would contend with agent builds, include functions in workspace search (rust-analyzer defaults to types only), and raise its 128-result search cap. Without that tuning, symbol-name queries can miss functions; a position query does not need workspace search.

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
