# Shared language servers

`rimz lsp` queries shared language servers started for a checkout at agent launch. Queries never start or restart a server. For lifecycle and memory accounting, see the [internals](../../internals/lsp.md).

## Queries

```sh
rimz lsp def MuxBackend
rimz lsp refs GcReport
rimz lsp hover crates/rimz/src/lib.rs:1:1
rimz lsp impl MuxBackend
rimz lsp callers sweep
rimz lsp callees sweep
rimz lsp symbols crates/rimz/src/lib.rs
rimz lsp find MuxBackend --server rust --json
```

| Verb | Target | Result |
| --- | --- | --- |
| `def` | position or exact symbol name | definition locations |
| `refs` | position or exact symbol name | reference locations |
| `hover` | position or exact symbol name | type and documentation |
| `impl` | position or exact symbol name | implementation locations |
| `callers` | position or exact symbol name | incoming call hierarchy items |
| `callees` | position or exact symbol name | outgoing call hierarchy items |
| `symbols` | file path | document symbol outline |
| `find` | search string | workspace symbols matching the server's search |

Positions are `path:line:col`, with one-based line and column. Paths are checkout-relative or absolute. A symbol name is resolved by exact name from workspace search results; multiple matches list candidates instead of guessing. Rerun with a candidate's position. Text locations use `path:line:col`; navigation includes source lines when available, outlines indent children, and hover prints markup. Empty answers print `no results`.

All eight verbs require one target and accept:

| Flag | Effect |
| --- | --- |
| `--server <NAME>` | Select a configured server name. Otherwise a file's extension selects among checkout entries, or the sole entry is used. Ambiguous selection names this flag in the error. |
| `--json` | Print the structured LSP result instead of text; ambiguous names print the candidate array. |

The checkout comes from cwd or the global `--root`, using the most deeply enclosing registered checkout when present. Different worktrees have different servers even though they share a room. Queries wait up to 30 seconds for indexing; there is no CLI wait-duration flag.

## List and stop

```sh
rimz lsp list
rimz lsp list --json
rimz lsp stop --server rust
rimz lsp stop /path/to/checkout --server rust
rimz lsp stop --all
```

`list` covers the whole machine. Text columns are CHECKOUT, SERVER, STATE, RSS, PEAK, REQUESTS, LAST, and LEASES. RSS and PEAK are current and observed peak process-tree KiB; LAST is seconds since the last query. Stopped entries show their reason. `--json` returns registry entries, including process identities, nonce, state, estimate, timestamps, counters, and leases. Dead entries are swept before listing.

`stop [CHECKOUT]` defaults to the current checkout (or global `--root`). `--server <NAME>` selects one server when several exist. `--all` stops every machine entry and conflicts with both CHECKOUT and `--server`. Stop prints one acknowledgment line per entry; no matching entries produce no lines. It has no `--json` flag. A hand stop leaves a tombstone while agents hold leases; queries report `stopped by hand`, not a restart.

## Exit codes

| Exit | Meaning |
| --- | --- |
| 0 | Answered, including no results or ambiguous symbol candidates; successful list or stop. |
| 1 | Command failed, including server-selection or protocol errors; details on stderr. |
| 2 | Invalid command line, flag, or argument. |
| 3 | No server for the checkout, or the server stopped. The stderr line names the reason and grep fallback. |
| 4 | Still indexing after the wait bound. The stderr line gives elapsed time. |

Other failures follow the [CLI error conventions](../cli.md). `--json` does not change the stderr form of exits 3 and 4.

## Configuration

No servers are configured by default. Put server entries in `~/.rimz/config.toml` or trusted project `.rimz/config.toml`. A project entry replaces the same-named machine entry whole, rather than merging its fields. Project memory-policy keys are refused with a fix pointing to the per-machine config.

```toml
[lsp] # machine config only
reserve-percent = 10
reserve-min = "8G"
kill-floor-percent = 5

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

| `[lsp.servers.<name>]` field | Default | Meaning |
| --- | --- | --- |
| `command` | required | Nonempty executable argv, not a shell command. |
| `extensions` | required | Nonempty file extensions without dots, used for selection and saved-file watching. |
| `root-markers` | required | Nonempty checkout-relative paths; any existing marker enables this server for the checkout. |
| `init-options` | absent | TOML table of initialization options passed to the server. |
| `policy` | `"optional"` | `optional` proceeds without a server on memory refusal; `required` waits before panes open. |
| `wait-timeout` | `"10m"` | Required-server queue deadline, with duration units such as `20s` or `10m`. |
| `memory-estimate` | `"8G"` | Admission estimate before matching peak history exists. |

Server names use ASCII letters, digits, `-`, or `_`; percentages range from 0 to 100. Sizes use decimal units (`8G` is 8 GB); `8GiB` is binary. Memory admission accounts for existing servers' committed growth and cgroup headroom. Required waits print position and remaining time in the launcher's terminal, not the sidebar. A later memory-pressure stop degrades required and optional servers alike.

Project server names, commands, and initialization options join the [trust hash](../../internals/harness/trust.md). An untrusted project declaration or missing executable refuses launch, even with `optional` policy. `rimz doctor` shows servers and the last refusal or queue timeout.

Configured Claude launches deny the native `LSP` tool even when optional admission is refused, avoiding private servers outside the budget. `rimz agents validate` warns when a profile declares `LSP` and machine servers exist. OpenCode and Grok have no verified native-server suppression here. Existing agents are not reconfigured by editing this table; new launches read it.
