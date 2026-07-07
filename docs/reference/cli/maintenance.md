# Maintenance CLI

These commands configure the machine, keep a room's identity and ledger healthy, recover a wedged room, sweep runtime state, and answer liveness probes. Every command here also accepts the global `--mux` and `--root` overrides.

## Configure the machine

```sh
rimz config init [--force] [--print]
rimz config path
rimz config get [KEY] [--json]
rimz config set <KEY> <VALUE>
```

`rimz config` reads and edits the per-machine config set at `~/.config/rimz/` (`config.toml`, `theme.toml`, `agents.toml`, `loop.toml`). `init` writes the commented templates — `--print` sends them to stdout instead, `--force` replaces an existing set. `path` prints the resolved `config.toml` path. `get` loads the effective config (no key prints the whole config, a dotted key prints one value, `--json` emits JSON). `set` edits one dotted key, preserves comments, rejects unknown keys, validates, and writes durably; a bare value becomes a TOML value when it parses and a string otherwise.

The full field model, dotted-key examples, and merge order are in [configuration.md](../configuration.md).

## Adapter coverage

```sh
rimz coverage [--json]
```

`rimz coverage` prints two static matrices from the built-in adapter descriptors — integration-concern coverage and lifecycle-hook coverage — with agents as rows and concern or signal labels as columns. The matrices use `✓` for native support, `◐` for partial or derived, and `✗` for absent, followed by a per-agent `DETAIL` breakdown that names the backing hook, event, or derivation next to each glyph for every cell. `--json` emits one document with `coverage` and `hooks_matrix` for scripting.

## List themes

```sh
rimz list-themes [--json]
```

`list-themes` prints the bundled Alacritty theme names, one per line, each usable verbatim as `rimz config set theme.scheme <name>`; on a terminal it renders an aligned table: each theme's name, then grouped palette chips (background/foreground, then the six ANSI hues) under a legend header. `--json` emits the list as an array. The palette model and custom theme files are in [theme.md](../theme.md).

## List pets

```sh
rimz list-pets [--json]
```

`list-pets` previews each bundled provider-dashboard pet and each pet installed under `~/.codex/pets/` as a medium cell-art sprite in a width-fitted grid on a terminal, streaming rows as sprites load, fetching and caching the built-in sheets, and honoring `RIMZ_PETS_OFFLINE`. Installed pets are labeled by selectable slug. Off a terminal it prints pet ids one per line, and `--json` emits the id array with installed slugs after the built-ins.

## Workspace ledger tools

```sh
rimz workspace resolve [PATH]
rimz workspace migrate <OLD_ROOT> <NEW_ROOT>
rimz workspace rotate-events [--max-bytes <SIZE>] [--archive-older-than <DURATION>]
```

`resolve` prints the resolved workspace as JSON — scripts use it to capture stable fields (`workspace_id`, `project_root`, `root_class`, `worktree_root`, `worktree_branch`, `session_name`, `mux_hint`) before invoking other tools. `migrate` moves a workspace ledger after its project root moves on disk, rewriting queued messages, events, and metadata to the new identity. `rotate-events` archives the active event log when it reaches `--max-bytes` (default `64MiB`) and starts a fresh log while preserving the agent carryover the sidebar and rebirth flow need; `--archive-older-than` prunes older archives and defaults to `14d`. The durability and rotation contract is in [ledger.md](../../internals/sidebar/ledger.md).

## Reload, reset, GC, and uninstall

```sh
rimz reload
rimz reset [--yes] [--no-start] [--hard] [PATH]
rimz gc [--older-than <DURATION>] [--dry-run] [--json]
rimz uninstall [--state] [--config] [--all] [--keep-binary] [--yes]
```

These repair or clean an installation without changing configuration.

- **`reload`** runs from anywhere and reconciles running sidebars onto the current Rimz build: it re-execs sidebars where possible, restarts those that cannot reload in place, closes duplicates and unresponsive ones, repairs geometry, restarts `rimz stats --refresh` dashboards, and leaves stopped sessions stopped.
- **`reset`** is the escape hatch for a wedged room. It resolves `PATH` as the cwd, tears down the session, purges the resurrection cache, archives records, clears coordination state, sweeps orphaned processes, then rebuilds and reattaches by default. `--yes` skips the prompt (required off a TTY), `--no-start` stops after teardown and prints the rerun hint, and `--hard` also removes the agent carryover (a plain reset keeps it for history but still starts empty).
- **`gc`** removes stale runtime state older than `--older-than` (default `24h`), sweeps orphaned atomic-write temp files (`*.tmp.<pid>.<nonce>`) across the state and runtime trees, removes stale provider probe-throttle markers (`*-probe.*`) from the runtime shared dir, applies the shorter Codex TTL to per-session app-server throttle stamps, abandons queued messages for missing sessions, repairs a corrupt event-log tail, prunes provably dead workspace ledgers, and sweeps clean Rimz-marked worktrees whose work has landed with no live pane inside. It prints live progress, reports reclaimed disk grouped by category, and names each swept worktree and pruned workspace. `--dry-run` previews the same report without removing anything and skips ledger maintenance; `--json` emits the report as JSON on stdout.
- **`uninstall`** removes Rimz from the machine: installed agent hooks, running rooms, runtime state, cache, data artifacts, and the `rimz` binaries it finds at the current executable, Cargo's bin dir, and `/usr/local/bin` (override the system bin probe with `RIMZ_SYSTEM_BIN_DIR`). Durable ledgers and spend history stay unless `--state` is passed; per-machine config, themes, trust grants, notification handlers, and remote aliases stay unless `--config` is passed; `--all` passes both. `--keep-binary` leaves binaries in place. `--yes` skips the prompt and is required off a TTY. Project-local `.rimz/` dirs and Rimz-owned worktrees stay in place because they can hold project config and unlanded work.

Sidebar reload behavior is in [sidebar.md](../../internals/sidebar/sidebar.md); reset and GC ledger effects are in [ledger.md](../../internals/sidebar/ledger.md).

## Ping

```sh
rimz ping
test "$(rimz ping)" = ok
```

`rimz ping` is the machine-readable liveness check: it prints `ok` and exits successfully when the binary can start and parse its global options.
