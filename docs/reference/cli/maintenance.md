# Maintenance CLI

These commands inspect, upgrade, repair, clean up, and remove RimZ. Which ones write, and what they reach:

| Command | Reaches | Writes | Asks first |
| --- | --- | --- | --- |
| [`coverage`](#check-adapter-coverage) | Nothing: static adapter declarations | Nothing | No |
| [`workspace resolve`](#resolve-a-workspace) | One path | Nothing | No |
| [`workspace migrate`](#move-a-store-after-the-project-moves) | One workspace store | Moves and rewrites the store | No |
| [`workspace rotate-events`](#rotate-the-event-log) | The current workspace | Archives the event log, prunes old archives | No |
| [`update`](#update-rimz) | The installed binary | Replaces the binary, then reloads | No |
| [`reload`](#reload-running-sidebars) | Every live room on the machine | Publishes the new build to each room | No |
| [`sidebar repair`](#repair-sidebars) | Every live room on the machine | Adds, closes, or replaces sidebar panes | No |
| [`reset`](#reset-a-wedged-room) | The current workspace's room | Tears the room down, archives its records, rebuilds | Yes |
| [`gc`](#sweep-stale-state) | The machine, plus the current workspace | Removes stale runtime files, dead stores, landed worktrees | No |
| [`uninstall`](#uninstall-rimz) | The whole machine | Removes hooks, rooms, state, binaries | Yes |
| [`ping`](#check-that-the-binary-runs) | Nothing | Nothing | No |

"The current workspace" is the one the current directory resolves to, or `--root`. All of these take the [global flags](../cli.md#global-flags). Configuring the machine is [`rimz config`](./config.md), and the workflows for a stuck room, a stale pane, and full removal are in the [troubleshooting guide](../../guide/troubleshooting.md).

## Check adapter coverage

```sh
rimz coverage [--wiring] [--json]
```

`rimz coverage` prints what RimZ can observe for each agent, read from the adapters' own declarations. It needs no room and writes nothing. The default report is one grid, `WHAT EACH AGENT GIVES YOU`, with an agent per row and the six user capabilities as columns:

```console
$ rimz coverage
RimZ coverage

WHAT EACH AGENT GIVES YOU
AGENT        state  live  history  account  ask  subagents
claude       ✓      ✓     ✓        ✓        ✓    ✓
codex        ✓      ✓     ✓        ✓        ✓    ✓
amp          ✓      !     !        !        ✓    ✗
…
  legend ✓ full   ! partial   ✗ unsupported

DETAIL
CAPABILITY  DETAIL

claude
state       ✓ the card opens at session start, follows every turn, and clears when Claude exits
…
```

Under each grid, `DETAIL` repeats every cell for every agent with its reason: what a full cell covers, what a partial cell shows and where it stops, and why an unsupported cell is empty. Valid [agent plugins](../agent-plugins.md) appear after the built-in agents. What each capability and mark means for your work is in [agent support](../agent-support.md).

| Flag | Effect |
| --- | --- |
| `--wiring` | Add two mechanism grids after the capability grid, each with its own `DETAIL`: `WIRING — INTEGRATION CONCERNS` (legend `✓ wired`, `! partial`, `✗ unsupported`) and `WIRING — LIFECYCLE HOOKS` (legend `✓ native`, `! derived`, `✗ absent`). |
| `--json` | Print one JSON document instead. `capabilities` is always present; `coverage` (concerns) and `hooks_matrix` (lifecycle hooks) appear only with `--wiring`. |

Each JSON matrix is `{"agents": [...], "rows": [...]}`. A row is one capability, concern, or signal: `{"label", "cells"}`, with one cell per entry in `agents`, in the same order. A cell is `{"state", "detail"}`, where `state` is `ok`, `partial`, or `absent`.

## Resolve a workspace

```sh
rimz workspace resolve [PATH]
```

`resolve` prints, as JSON, the workspace that `PATH` (default `.`) belongs to. It writes nothing, so a script can call it to learn which room a directory maps to before running other tools.

```console
$ rimz workspace resolve
{
  "workspace_id": "ws_f89e49906df0621ad2765112",
  "project_root": "/home/me/code/query-engine",
  "cwd_project_root": "/home/me/code/query-engine",
  "root_class": "repo",
  "worktree_root": "/home/me/code/query-engine-worktrees/docs",
  "worktree_branch": "docs",
  "session_name": "rimz-query-engine-f89e49",
  "mux_hint": null
}
```

| Field | Meaning |
| --- | --- |
| `workspace_id` | The workspace's stable ID, derived from `project_root`. |
| `project_root` | The root the room belongs to. Every worktree of a repository shares its main checkout's root. |
| `cwd_project_root` | The main repository root of `PATH`'s own checkout, or the `--root` repository. `null` when neither is a Git repository. |
| `root_class` | `repo` (a Git repository), `marker` (a directory with a project marker), or `directory` (any other directory). |
| `worktree_root` | The checkout `PATH` sits in: the main checkout or a linked worktree. |
| `worktree_branch` | The branch checked out there, or `null`. |
| `session_name` | The Zellij or tmux session name the room uses. |
| `mux_hint` | Always `null` from this command. |

## Move a store after the project moves

```sh
rimz workspace migrate <OLD_ROOT> <NEW_ROOT>
```

The workspace ID is derived from the project root, so a project moved on disk resolves to a new, empty workspace. `migrate` carries the old store over: it moves the store directory to the new ID and rewrites the workspace identity in its events, queued messages, and workspace record. `OLD_ROOT` may be a path that no longer exists. Run it before starting a room in the new location.

```console
$ rimz workspace migrate ~/code/query-engine ~/src/query-engine
migrated ws_f89e49906df0621ad2765112 -> ws_0b7d3c1e5a9f2468ace13579
  old root: /home/me/code/query-engine
  new root: /home/me/src/query-engine
  messages: 12
  events:   4810
```

It refuses, and changes nothing, when:

| Condition | Error |
| --- | --- |
| `NEW_ROOT` does not exist | `new workspace root does not exist: PATH` |
| No store exists for `OLD_ROOT` | `workspace store ID not found at PATH` |
| A store already exists for `NEW_ROOT` | `destination workspace store ID already exists at PATH` |

## Rotate the event log

```sh
rimz workspace rotate-events [--max-bytes <SIZE>] [--archive-older-than <DURATION>]
```

`rotate-events` archives the current workspace's active event log once it reaches `--max-bytes`, and a fresh log starts with the next event. Rotation keeps the prior-agent carryover, so the sidebar and a later rebirth still know the room's agents. The command then deletes archives older than `--archive-older-than`. RimZ also rotates on its own when the log reaches 64 MiB, so you rarely need this by hand.

| Flag | Default | Effect |
| --- | --- | --- |
| `--max-bytes <SIZE>` | `64MiB` | Rotate only when the active log is at least this big. `0` rotates any non-empty log; an empty log never rotates. |
| `--archive-older-than <DURATION>` | `14d` | Delete archives older than this. Units: `s`, `m`, `h`, `d` ([durations](../cli.md#durations)). |

`SIZE` is an integer with an optional unit and no space: none or `B` for bytes, `K` or `KB`, `M` or `MB`, `G` or `GB` for powers of 1000, and `KiB`, `MiB`, `GiB` for powers of 1024.

The output opens with `event-log rotated` (then the bytes rotated, the archive path, and the carryover agent count) or `event-log rotation skipped` (then the current size and the threshold). Both end with the number of archives and bytes pruned. How rotation and carryover work is in [store internals](../../internals/store.md#rotation-and-carryover).

## Update RimZ

```sh
rimz update [--version <TAG>]
```

`update` upgrades RimZ through the method that installed the running binary:

| Installed with | What `update` runs |
| --- | --- |
| Homebrew (the binary lives under a `Cellar` directory) | `brew update`, then `brew upgrade rimio-ai/rimz/rimz`. |
| Cargo (the binary is in Cargo's bin directory) | `cargo install --locked rimz`. A source build (a version containing `+g`) prints a warning that the crates.io release will replace it. |
| The install script or a downloaded binary | Resolves the latest release, downloads the archive for this platform, verifies it against the release's `SHA256SUMS`, runs the extracted binary's `--version`, and atomically replaces the running binary in place. If the running version already matches, it prints `rimz VERSION is up to date.` and stops. |

When the binary changed, `update` runs the new build's [`rimz reload`](#reload-running-sidebars), so live sidebars and dashboards move to it. If that reload fails, `update` exits 1 with ``RimZ updated, but `PATH` reload exited with STATUS; rerun `rimz reload` ``.

`--version <TAG>` picks the release:

| Install | Accepted tags |
| --- | --- |
| Script or download | Any release tag, such as `v0.3.1` or an older tag to roll back, or the rolling `latest-main` build. |
| Cargo | Numbered tags only. `v0.3` becomes `0.3.0`; `latest-main` is refused with ``Cargo cannot install release tag `latest-main`; use a version tag such as `v0.3.1` ``. |
| Homebrew | None. `--version` fails with ``Homebrew cannot pin RimZ to `TAG`; reinstall the standalone build with `RIMZ_VERSION=TAG` or let `brew upgrade rimio-ai/rimz/rimz` select the release``. |

A script or downloaded install stops with a fix in these cases:

| Condition | Error |
| --- | --- |
| No prebuilt archive for this platform (prebuilts cover Linux x86_64 with glibc and macOS on Apple silicon and Intel) | `no prebuilt RimZ release exists for this target; install with: cargo install --locked rimz` |
| The binary's directory is not writable | `RimZ install directory DIR is not writable; rerun with: sudo rimz update` |
| The checksum does not match | ``checksum verification failed for `ARCHIVE`; delete any proxy cache and retry`` |

Installing and choosing a method is covered in the [installation guide](../../guide/installation.md#update).

## Reload running sidebars

```sh
rimz reload [--repair]
```

`reload` moves every running RimZ surface on the machine to the binary you invoke, without changing any pane. It runs from any directory, inside or outside a room. One run:

- publishes the invoking build as the target of every live room, so each room's sidebars restart onto it in place;
- upgrades a room's Zellij presence plugin when its build or configuration differs, and leaves a current one alone;
- restarts held `rimz stats --refresh` dashboards on the new build;
- reaps sidebar processes whose pane is gone, and sweeps leftover processes and runtime files from stopped sessions;
- restarts the shared [web](./web.md) daemon if it is running. A stopped daemon stays stopped, and a failed restart prints a warning without failing the reload.

It never starts a stopped session, creates or closes a pane, or touches an agent process. If the new build fails to start, each sidebar keeps running on its old build.

The report prints a line per non-zero count:

| Line | Meaning |
| --- | --- |
| `Reloaded N sidebars across N sessions.` | Sidebars that moved to the new build. |
| `N sidebars already on the current build.` | Nothing to do there. |
| `N sidebars still converging; their supervisors will retry from the recorded build automatically.` | These sidebars did not confirm within the report window. They keep retrying on their own; no repair is needed. |
| `N sidebars could not be build-verified.` | RimZ could not read which build these sidebars run. |
| `Upgraded N presence plugins.`, `Reconciled N presence plugins.`, `N presence plugins already current.` | The Zellij presence plugin in each room. |
| `Reloaded N stats dashboards.` | Held dashboards restarted. |
| `Reaped N orphaned sidebar processes.`, `Swept N leftover processes from stopped sessions.` | Cleanup. |
| `Restarted the shared web daemon.` | The web daemon was running and restarted. |
| `No running sidebars to reload.` | No live room, no stopped session to sweep, and no dashboard. A second line suggests `rimz start` or `rimz attach`. |

`--repair` runs [`rimz sidebar repair`](#repair-sidebars) after the reload finishes, as a separate step: a repair that cannot run does not undo the reload. How a sidebar hands over to a new build is in [sidebar internals](../../internals/sidebar/sidebar.md#build-promotion).

## Repair sidebars

```sh
rimz sidebar repair
```

`sidebar repair` fixes the sidebar panes themselves, in every live room: it adds a missing sidebar, closes duplicate or unresponsive ones, and restores a sidebar's docking and width. A replacement pane must start and report on the current build before the old one closes. It publishes no build; use [`reload`](#reload-running-sidebars) for that.

| Line | Meaning |
| --- | --- |
| `Recovered N sidebars in place.` | Sidebars added to a view that had none, or whose only sidebar was unresponsive. |
| `Closed N duplicate or unresponsive sidebars.` | Extra or dead sidebar panes removed. |
| `Repaired N sidebars geometry.` | Kept sidebars moved back to the full-height left dock. |
| `N sidebars still working but not docked.` | Running, but still outside the left dock after repair. |
| ``N sidebars could not be repaired; attach and re-run `rimz sidebar repair`.`` | The add or repair did not complete this pass. |
| `No live presence channel for N sessions; repair skipped. Reattach or restart the session.` | On Zellij, repair needs the presence plugin; these sessions were left untouched. |
| ``Deferred N sidebar repairs (no attached client); attach and re-run `rimz sidebar repair`.`` | A detached Zellij session cannot take a new pane or geometry change until a client attaches. |
| `Sidebar structure is healthy across N sessions.` | Nothing needed repair. |
| `No running sidebars to repair.` | No live room. |

The repair rules are in [sidebar internals](../../internals/sidebar/sidebar.md#structural-repair).

## Reset a wedged room

```sh
rimz reset [--yes] [--no-start] [--hard] [--account <KIND=NAME>]... [PATH]
```

`reset` tears down the room for `PATH` (default `.`) and rebuilds it empty. Use it when a room is stuck, or came back wrong after a reboot. In order, it:

1. deletes the room's multiplexer session and purges the multiplexer's resurrection cache for it;
2. signals the room's orphaned processes and removes its runtime files, per-room tmp directory, and skill copies;
3. cancels the room's active supervised runs, which report `canceled` to anyone waiting on them;
4. archives the active event log, deletes the room's diagnostic logs, and clears its recorded provider accounts;
5. starts the room again and attaches, with no prior agents recovered.

Your agents' own session files and the room's archived records stay on disk. A plain reset keeps the prior-agent carryover as history; the reborn room still starts with no agents.

| Flag | Effect |
| --- | --- |
| `--yes` | Skip the `[y/N]` prompt. Required when stdin is not a terminal. |
| `--no-start` | Stop after step 4 and print ``Room torn down. Run `rimz start` to rebuild it.`` |
| `--hard` | Also delete the prior-agent carryover, so the store keeps no record of the old room's agents beyond the archived log. The report reads `Records: prior agent rollup cleared.` |
| `--account <KIND=NAME>` | Rebuild the room under this [provider account](./accounts.md). Repeatable. Cannot be combined with `--no-start`. |

A room's accounts are fixed when it is born, so `reset --account` is how you move a running room to a different account.

Reset refuses before prompting or changing anything when `--mux` names a backend other than the one the room runs on. Without `--yes` and without a terminal it fails with `` `rimz reset` deletes the session and sweeps its processes; pass --yes to confirm without a terminal ``. Answering anything but `y` prints `Reset aborted; nothing changed.` and exits 0.

The report goes to stderr before the rebuild:

```console
Reset: session deleted, 1 cache entry removed, 2 orphan processes swept.
Tmp and skill copies: cleared.
Records: archived 48213 bytes to /home/me/.local/state/rimz/workspaces/ws_f89e49906df0621ad2765112/events.log.archive/events.0192f3a4-7c1e-7b20-9d6a-3f4e5a6b7c8d.jsonl.
Records: canceled 0 runs, removed 3 debug entries, runtime removed.
Records: prior agent rollup kept (4 agents).
```

What reset does to the store is in [store internals](../../internals/store.md#maintenance).

## Sweep stale state

```sh
rimz gc [--older-than <DURATION>] [--dry-run] [--json]
```

`gc` removes state that has outlived its use and keeps anything dirty, pending, or unproven. Part of the sweep covers the whole machine; the rest runs only in the current workspace:

| Area | Scope | What `gc` does |
| --- | --- | --- |
| `worktrees` | Current repository | Removes RimZ-owned worktrees that are clean, landed, and unoccupied, the same sweep as [`rimz worktree sweep`](./worktree.md#sweep-landed-worktrees). |
| `workspaces` | Machine | Deletes workspace stores that provably hold nothing: the project folder is gone, or a `rimz start` was abandoned before any history. A store whose record is unreadable but which holds history is kept and reported. |
| `runtime` | Machine | Removes sidebar heartbeats, sockets, and sidecar files older than `--older-than`, and stale provider probe markers. |
| `temp files` | Machine | Removes temp files (`*.tmp.<pid>.<nonce>`) older than `--older-than`, left by a process killed mid-write. |
| `messages` | Current workspace | Archives open messages whose receiver has ended, and requeues or times out messages stuck as sent. |
| `event log` | Current workspace | Cuts a corrupt tail off the event log. |
| `agent cache` | Current workspace | Prunes prior-agent carryover older than 14 days. |
| `loop schedules` | Machine | Removes loop delivery tasks whose target agent is gone and RimZ-owned instance rows that no longer compile to an action, and prunes [wait output files](./wait.md) older than 14 days that nothing claims. |

| Flag | Default | Effect |
| --- | --- | --- |
| `--older-than <DURATION>` | `24h` | The age cutoff for `runtime` and `temp files`. Units: `s`, `m`, `h` only, so write a week as `168h`. Must be greater than zero. |
| `--dry-run` | off | Report what would be removed and remove nothing. `messages`, `event log`, `agent cache`, and `loop schedules` show as skipped. |
| `--json` | off | Print the report as JSON instead. |

The four current-workspace areas show `skipped — no rimz store here` when the directory has no RimZ store, and `worktrees` shows `skipped — not inside a git repo` outside a repository.

### The report

`gc` shows progress while it runs, then prints a line per area with its verdict: `✓` healthy, `✦` acted, `⚠` warning, `✗` failed, `–` skipped. Each kept, removed, or failed worktree gets its own line under `worktrees`.

```console
$ rimz gc
gc — reclaimed 21 MB
  checked 8 areas · cutoff 24h

  ✦ worktrees       1 removed · 21 MB · 2 kept
      kept: api-redesign — in use
      kept: auth-fix — not merged yet
      removed: guides-tune  21 MB  merged, branch deleted
  ✓ workspaces      4 healthy
  ✓ runtime         5 roots scanned, all fresh
  ✓ temp files      none orphaned
  ✓ messages        queue clean
  ✓ event log       intact
  ✓ agent cache     clean
  ✓ loop schedules  none dead
```

The second line counts the areas that ran (`checked 4 of 8 areas` when the four store and schedule areas are skipped). The header adds a problem count (`· 1 problem`) when a worktree removal failed, a workspace record was unreadable, or the event log needed repair. Problems do not change the exit code: `gc` exits 0 whenever the sweep completes.

### JSON output

| Field | Content |
| --- | --- |
| `dry_run` | `true` under `--dry-run`. |
| `older_than_secs` | The cutoff in seconds. |
| `reclaimed_bytes` | Bytes removed, or that would be removed, across all areas. |
| `worktrees` | `removed` (`name`, `branch`, `path`, `bytes`, `branch_deleted`, `archive_error`), `failed` (`path`, `error`), `kept` (`name`, `path`, `reason`: `in_use`, `uncommitted_changes`, `not_merged`), and `skipped` (`null`, `not_a_repo`, `no_store`, `roster_unavailable`, `list_failed`). |
| `workspaces` | `removed` (`workspace_id`, `reason`: `project_root_gone` or `abandoned_scaffold`, `project_root`, `bytes`), `retained_unreadable` (`workspace_id`, `error`), and `kept` (a count). |
| `runtime` | `roots_scanned`, `heartbeats_removed`, `sidecars_removed`, `sockets_removed`, `probe_markers_removed`, `dirs_removed`, `bytes_removed`. |
| `temps` | `files_removed`, `bytes_removed`. |
| `messages` | `archived`, `reconciled`. |
| `carryover_pruned` | Carryover entries pruned. |
| `schedules_reaped` | Loop delivery tasks removed because their target is gone, plus RimZ-owned instance rows removed because they no longer compile to an action. |
| `wait_logs_pruned` | Wait output files pruned. |
| `repair` | `bytes_truncated` and `frames_kept`, or `null` when store maintenance was skipped. |
| `store_maintenance` | `done`, `skipped_dry_run`, or `skipped_no_store`. |

What each sweep removes on disk is in [store internals](../../internals/store.md#maintenance).

## Uninstall RimZ

```sh
rimz uninstall [--state] [--config] [--all] [--keep-binary] [--yes]
```

`uninstall` removes RimZ from the machine. It prints a preview of every root, room, hook, timer, and binary it will touch, asks `[y/N]`, and then reports each removal. Preview and report go to stderr.

| What | Default | Flag to change it |
| --- | --- | --- |
| RimZ hooks, in each provider's own home and every declared account home | Removed | |
| Running rooms, on both backends | Torn down | |
| The external [loop timer](./loop.md) | Removed | |
| Runtime and cache directories | Removed | |
| Data directory (`~/.local/share/rimz`) | Removed, except `accounts/` | |
| [Provider account](./accounts.md) homes under `accounts/` | Kept: their credentials and history belong to the provider | |
| Durable stores, spend history, and shared state (`~/.local/state/rimz`) | Kept | `--state` removes them |
| Per-machine config, themes, trust grants, notification handlers, and remote aliases (`~/.config/rimz`) | Kept | `--config` removes them |
| `rimz` binaries at the running executable, Cargo's bin directory, and `/usr/local/bin` | Removed | `--keep-binary` keeps them; `RIMZ_SYSTEM_BIN_DIR` replaces `/usr/local/bin` |
| Project `.rimz/` directories and RimZ-owned worktrees | Always kept: they can hold project config and unlanded work | |

`--all` is `--state` plus `--config`. `--yes` skips the prompt and is required when stdin is not a terminal. The paths above are the defaults; XDG base directory variables move them.

Run it from outside any RimZ room: inside one it fails with `detach and rerun from outside the RimZ room`. A Homebrew install also needs `brew uninstall rimz`. When any step fails, the rest still run, and `uninstall` exits 1 with `uninstall incomplete:` and one line per failure. A binary it had no permission to delete gets a `sudo rm PATH` line to run.

## Check that the binary runs

```sh
rimz ping
test "$(rimz ping)" = ok
```

`ping` prints `ok` and exits 0 when the binary starts and its global flags parse. It touches no room, so it is the cheapest liveness probe for a script or health check.
