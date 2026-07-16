# Maintenance CLI

These commands inspect a room's identity and store, repair a wedged room, sweep runtime state, and answer liveness probes. The read-only ones (`coverage`, `workspace resolve`, `ping`) change nothing; the rest state exactly what they remove and what they keep, and none touch your durable stores or config unless a flag says so. Every command here also accepts the global `--mux` and `--root` overrides. Configuring the machine is [`rimz config`](./config.md).

## Adapter coverage

```sh
rimz coverage [--json]
```

`rimz coverage` prints two static matrices from the built-in adapter descriptors, integration-concern coverage and lifecycle-hook coverage, with agents as rows and concern or signal labels as columns. The matrices use `✓` for native support, `◐` for partial or derived, and `✗` for absent, followed by a per-agent `DETAIL` breakdown that names the backing hook, event, or derivation next to each glyph for every cell. It reads only. `--json` emits one document with `coverage` and `hooks_matrix` for scripting.

## Workspace store tools

```sh
rimz workspace resolve [PATH]
rimz workspace migrate <OLD_ROOT> <NEW_ROOT>
rimz workspace rotate-events [--max-bytes <SIZE>] [--archive-older-than <DURATION>]
```

`resolve` prints the resolved workspace as JSON and writes nothing; scripts use it to capture stable fields (`workspace_id`, `project_root`, `root_class`, `worktree_root`, `worktree_branch`, `session_name`, `mux_hint`) before invoking other tools. `migrate` moves a workspace store after its project root moves on disk, rewriting queued messages, events, and metadata to the new identity. `rotate-events` archives the active event log when it reaches `--max-bytes` (default `64MiB`) and starts a fresh log while preserving the agent carryover the sidebar and rebirth flow need; `--archive-older-than` prunes older archives and defaults to `14d`. The durability and rotation contract is in [store.md](../../internals/store.md).

## Update, reload, reset, GC, and uninstall

These repair or clean an installation without changing your configuration.

```sh
rimz update [--version <TAG>]
rimz reload
rimz reset [--yes] [--no-start] [--hard] [PATH]
rimz gc [--older-than <DURATION>] [--dry-run] [--json]
rimz uninstall [--state] [--config] [--all] [--keep-binary] [--yes]
```

`update` follows the current installation method: Homebrew delegates to `brew upgrade rimz`, Cargo delegates to `cargo install --locked rimz`, and script or manual prebuilt installs download, checksum, smoke-test, and atomically replace the current binary. `--version` selects a numbered release tag for Cargo or any release tag, including `latest-main`, for a standalone install; Homebrew keeps formula version selection. A changed binary automatically launches the new build's `reload` command. Unsupported prebuilt targets report the exact `cargo install --locked rimz` fallback, and a protected standalone destination reports the `sudo rimz update` retry.

`reload` runs from anywhere and reconciles running sidebars onto the current RimZ build: it re-execs sidebars where possible, restarts those that cannot reload in place, closes duplicates and unresponsive ones, repairs geometry, restarts `rimz stats --refresh` dashboards, and leaves stopped sessions stopped. It touches sidebar processes only, not agents. Sidebar reload behavior is in [sidebar.md](../../internals/sidebar/sidebar.md).

`reset` is the escape hatch for a wedged room. It resolves `PATH` as the cwd, tears down the session, purges the resurrection cache, archives records, clears coordination state, sweeps orphaned processes, then rebuilds and reattaches by default. Durable history is archived, not deleted, so a reset room comes up empty but its records survive. `--yes` skips the prompt (required off a TTY), `--no-start` stops after teardown and prints the rerun hint, and `--hard` also removes the agent carryover (a plain reset keeps it for history but still starts empty).

`gc` removes stale runtime state older than `--older-than` (default `24h`) — runtime liveness hints, not durable records. One run:

- sweeps orphaned atomic-write temp files (`*.tmp.<pid>.<nonce>`) across the state and runtime trees,
- removes stale provider probe-throttle markers (`*-probe.*`) from the runtime shared dir, applying the shorter Codex TTL to per-session app-server throttle stamps,
- abandons queued messages for missing sessions,
- repairs a corrupt event-log tail,
- prunes provably dead workspace stores,
- and sweeps clean RimZ-marked worktrees whose work has landed with no live pane inside.

It prints live progress, then always reports an eight-area checklist: worktrees, workspaces, runtime hints, temp files, messages, event log, agent cache, and loop schedules. The checklist names what was cleaned, what is healthy, what was skipped, and why kept worktrees or workspaces were preserved; the cutoff echoes in compact human units such as `24h`. `--dry-run` previews reclaimable work without removing anything and marks store-maintenance rows as skipped. `--json` emits the same report on stdout, including kept worktrees with reason tokens and `store_maintenance` as `done`, `skipped_dry_run`, or `skipped_no_store`. Reset and GC store effects are in [store.md](../../internals/store.md).

`uninstall` removes RimZ from the machine. By default it removes installed agent hooks, running rooms, runtime state, cache, data artifacts, and the `rimz` binaries it finds at the current executable, Cargo's bin dir, and `/usr/local/bin` (override the system bin probe with `RIMZ_SYSTEM_BIN_DIR`). What it keeps unless you ask: durable stores and spend history stay unless `--state` is passed; per-machine config, themes, trust grants, notification handlers, and remote aliases stay unless `--config` is passed; `--all` passes both. `--keep-binary` leaves binaries in place. `--yes` skips the prompt and is required off a TTY. Project-local `.rimz/` dirs and RimZ-owned worktrees always stay in place, because they can hold project config and unlanded work.

## Ping

```sh
rimz ping
test "$(rimz ping)" = ok
```

`rimz ping` is the machine-readable liveness check: it prints `ok` and exits successfully when the binary can start and parse its global options.
