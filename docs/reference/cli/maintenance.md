# Maintenance CLI

This page covers the CLI commands for machine configuration, workspace identity and ledger upkeep, room recovery, runtime garbage collection, and liveness checks.

Every command on this page accepts the global `--mux <MUX>` and `--root <ROOT>` options shown in help. `--root` selects the workspace root for commands that resolve a room, and `--mux` selects the backend where a command needs multiplexer state.

## Configure the machine

```sh
rimz config init --print
rimz config init
rimz config path
rimz config get sidebar.max_cols
rimz config get sidebar --json
rimz config set sidebar.max_cols 80
rimz config set notifications.triggers '["waiting", "failed"]'
```

`rimz config` reads and edits the per-machine config at `~/.config/rimz/config.toml`. The full field model lives in [configuration.md](../configuration.md).

```sh
rimz config init [--force] [--print]
rimz config path
rimz config get [KEY] [--json]
rimz config set <KEY> <VALUE>
```

`init` writes the commented default template. `--print` sends that template to stdout instead of writing it, and `--force` replaces an existing config file.

`path` prints the resolved per-machine config path, which is useful in scripts and editors.

`get` loads the effective config over built-in defaults. With no key it prints the whole config as TOML; with a dotted key such as `sidebar.max_cols` it prints only that value; `--json` prints JSON for the whole config or selected value.

`set` edits one dotted key, preserves comments, rejects unknown keys, validates the resulting config, and writes with Rimz's temp-file-plus-rename durability primitive. Bare values become TOML values when they parse (`80`, `false`, arrays, inline tables); otherwise they become strings, so `fresh` is accepted as a string value.

Dotted keys address nested TOML tables. Examples include `worktree.base`, `sidebar.max_cols`, `notifications.enabled`, `notifications.triggers`, `tab.layouts.peer`, `tab.keywords.codex-yolo.mode`, and `sidebar.providers.codex.color`.

## Workspace ledger tools

```sh
rimz workspace resolve
rimz workspace resolve /srv/query-engine | jq -r .workspace_id
rimz workspace migrate /old/query-engine /srv/query-engine
rimz workspace rotate-events --max-bytes 64MiB --archive-older-than 30d
```

`rimz workspace` exposes identity helpers and ledger maintenance for the current room.

```sh
rimz workspace resolve [PATH]
rimz workspace migrate <OLD_ROOT> <NEW_ROOT>
rimz workspace rotate-events [--max-bytes <SIZE>] [--archive-older-than <DURATION>]
```

`resolve` prints the resolved workspace as JSON. Scripts use it to capture stable fields such as `workspace_id`, `project_root`, `root_class`, `worktree_root`, `worktree_branch`, `session_name`, and `mux_hint` before invoking other tools.

`migrate` moves a workspace ledger after a project root moves on disk. It resolves the old and new roots, moves the durable workspace directory when the workspace ID changes, and rewrites feed items, queued messages, event records, and workspace metadata to the new identity.

`rotate-events` archives the active event log when it reaches `--max-bytes`, then starts a fresh active log while preserving the agent carryover used by the sidebar and rebirth flow. Size values accept `B`, `KB`, `KiB`, `MB`, `MiB`, `GB`, and `GiB`; the default threshold is `64MiB`.

`--archive-older-than <DURATION>` prunes archived event logs older than the supplied duration. Rotation retention accepts `s`, `m`, `h`, and `d` units, such as `12h` or `30d`; omitting the flag keeps all archives.

The ledger durability and event-log rotation contract lives in [internals/sidebar/ledger.md](../../internals/sidebar/ledger.md).

## Reload, reset, and GC

```sh
rimz reload
rimz reset --yes
rimz reset --yes --no-start .
rimz reset --yes --hard /srv/query-engine
rimz gc --older-than 1h
```

These commands repair or clean an existing installation without changing configuration.

```sh
rimz reload
rimz reset [--yes] [--no-start] [--hard] [PATH]
rimz gc [--older-than <DURATION>]
```

`reload` runs from anywhere and reconciles running sidebars onto the current Rimz build. It re-execs current sidebars where possible, restarts sidebars that cannot reload in place, closes duplicates or unresponsive sidebars, repairs geometry, and leaves stopped sessions stopped.

`reset` is the explicit escape hatch for a wedged room. It resolves `PATH` as the workspace cwd, tears down the room session, purges the resurrection cache, archives current records, clears live coordination state, sweeps orphaned processes, and then rebuilds and reattaches the room by default.

`--yes` skips the confirmation prompt for scripts. Without `--yes`, `reset` requires an interactive terminal and asks before destroying the room session.

`--no-start` stops after teardown and prints the rerun hint instead of rebuilding. Use it when a script wants a clean stop boundary.

`--hard` archives the current room records without seeding prior agents on rebirth. A reset without `--hard` keeps the agent carryover so the reborn room can resume remembered agents.

`gc` removes stale runtime state older than `--older-than`, abandons pending feed items whose owner process has exited, abandons queued messages for missing agent sessions, repairs a corrupt event-log tail when needed, prunes provably dead workspace ledgers, and sweeps clean Rimz-marked worktrees whose work has landed and have no live user pane inside them.

`--older-than <DURATION>` defaults to `24h` and accepts `s`, `m`, and `h` units, such as `30s`, `5m`, or `1h`.

Sidebar reload behavior is described in [internals/sidebar/sidebar.md](../../internals/sidebar/sidebar.md), and reset and garbage-collection ledger effects are described in [internals/sidebar/ledger.md](../../internals/sidebar/ledger.md).

## Ping

```sh
rimz ping
test "$(rimz ping)" = ok
```

`rimz ping` is the machine-readable liveness check. It prints `ok` and exits successfully when the binary can start and parse its global options.
