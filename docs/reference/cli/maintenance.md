# Maintenance CLI

These commands configure the machine, keep a room's identity and ledger healthy, recover a wedged room, sweep runtime state, and answer liveness probes. Every command here also accepts the global `--mux` and `--root` overrides.

## Configure the machine

```sh
rimz config init [--force] [--print]
rimz config path
rimz config get [KEY] [--json]
rimz config set <KEY> <VALUE>
```

`rimz config` reads and edits the per-machine config set at `~/.config/rimz/` (`config.toml`, `theme.toml`, `agents.toml`). `init` writes the commented templates — `--print` sends them to stdout instead, `--force` replaces an existing set. `path` prints the resolved `config.toml` path. `get` loads the effective config (no key prints the whole config, a dotted key prints one value, `--json` emits JSON). `set` edits one dotted key, preserves comments, rejects unknown keys, validates, and writes durably; a bare value becomes a TOML value when it parses and a string otherwise.

The full field model, dotted-key examples, and merge order are in [configuration.md](../configuration.md).

## Adapter coverage

```sh
rimz coverage [--json]
```

`rimz coverage` prints two static matrices from the built-in adapter descriptors — integration-concern coverage and lifecycle-hook coverage — using `✓` for native support, `◐` for partial or derived, and `✗` for absent. Each matrix includes a `GAPS` table listing every non-OK cell with its concern, agent, and detail. `--json` emits one document with `coverage` and `hooks_matrix` for scripting.

## List themes

```sh
rimz list-themes [--json]
```

`list-themes` prints the bundled Alacritty theme names, one per line, each usable verbatim as `rimz config set theme.scheme <name>`; `--json` emits the list as an array. The palette model and custom theme files are in [theme.md](../theme.md).

## Workspace ledger tools

```sh
rimz workspace resolve [PATH]
rimz workspace migrate <OLD_ROOT> <NEW_ROOT>
rimz workspace rotate-events [--max-bytes <SIZE>] [--archive-older-than <DURATION>]
```

`resolve` prints the resolved workspace as JSON — scripts use it to capture stable fields (`workspace_id`, `project_root`, `root_class`, `worktree_root`, `worktree_branch`, `session_name`, `mux_hint`) before invoking other tools. `migrate` moves a workspace ledger after its project root moves on disk, rewriting feed items, queued messages, events, and metadata to the new identity. `rotate-events` archives the active event log when it reaches `--max-bytes` (default `64MiB`) and starts a fresh log while preserving the agent carryover the sidebar and rebirth flow need; `--archive-older-than` prunes older archives. The durability and rotation contract is in [ledger.md](../../internals/sidebar/ledger.md).

## Reload, reset, and GC

```sh
rimz reload
rimz reset [--yes] [--no-start] [--hard] [PATH]
rimz gc [--older-than <DURATION>]
```

These repair or clean an installation without changing configuration.

- **`reload`** runs from anywhere and reconciles running sidebars onto the current Rimz build: it re-execs sidebars where possible, restarts those that cannot reload in place, closes duplicates and unresponsive ones, repairs geometry, and leaves stopped sessions stopped.
- **`reset`** is the escape hatch for a wedged room. It resolves `PATH` as the cwd, tears down the session, purges the resurrection cache, archives records, clears coordination state, sweeps orphaned processes, then rebuilds and reattaches by default. `--yes` skips the prompt (required off a TTY), `--no-start` stops after teardown and prints the rerun hint, and `--hard` also removes the agent carryover (a plain reset keeps it for history but still starts empty).
- **`gc`** removes stale runtime state older than `--older-than` (default `24h`), abandons pending feed items whose owner has exited, abandons queued messages for missing sessions, repairs a corrupt event-log tail, prunes provably dead workspace ledgers, and sweeps clean Rimz-marked worktrees whose work has landed with no live pane inside. It prints live progress and reports reclaimed disk grouped by category.

Sidebar reload behavior is in [sidebar.md](../../internals/sidebar/sidebar.md); reset and GC ledger effects are in [ledger.md](../../internals/sidebar/ledger.md).

## Ping

```sh
rimz ping
test "$(rimz ping)" = ok
```

`rimz ping` is the machine-readable liveness check: it prints `ok` and exits successfully when the binary can start and parse its global options.
