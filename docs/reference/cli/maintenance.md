# Maintenance CLI

These commands inspect, upgrade, repair, clean up, and remove RimZ. Which ones write, and what they reach:

| Command | Reaches | Writes | Asks first |
| --- | --- | --- | --- |
| [`coverage`](#check-adapter-coverage) | Nothing: static adapter declarations | Nothing | No |
| [`workspace resolve`](#resolve-a-workspace) | One path | Nothing | No |
| [`workspace migrate`](#move-a-store-after-the-project-moves) | One workspace store | Moves and rewrites the store | No |
| [`workspace rotate-events`](#rotate-the-event-log) | The current workspace | Archives the event log, prunes old archives | No |
| [`update`](#update-rimz) | The installed binary and unpinned pack sources | Replaces the binary, reloads, then updates unpinned packs | Widening pack updates, on a terminal |
| [`reload`](#reload-running-sidebars) | The whole machine | Publishes build targets and retires outdated language-server brokers | No |
| [`sidebar repair`](#repair-sidebars) | Every live room on the machine | Adds, closes, or replaces sidebar panes, and reopens a closed `rimzd` tab | No |
| [`reset`](#reset-a-wedged-room) | The current workspace's room | Tears the room down, archives its records, rebuilds | Yes |
| [`gc`](#sweep-stale-state) | The current room; `--all` adds the machine | Removes stale state and landed worktrees; `--all` also prunes dead stores | No |
| [`uninstall`](#uninstall-rimz) | The whole machine | Removes hooks, rooms, runtime and cache, binaries; `--state` and `--config` add durable state and config | Yes |
| [`ping`](#check-that-the-binary-runs) | Nothing | Nothing | No |

"The current workspace" is the one the current directory resolves to, or `--root`; `gc` and `workspace rotate-events` also follow the running room's identity ([room resolution](../cli.md#which-room-a-command-reaches)). All of these take the [global flags](../cli.md#global-flags). Configuring the machine is [`rimz config`](./config.md), and the workflows for a stuck room, a stale pane, and full removal are in the [troubleshooting guide](../../guide/troubleshooting.md).

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
amp          ✓      ✗     ✗        !        !    ✗
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

`resolve` prints, as JSON, the workspace that `PATH` (default `.`) belongs to. It writes nothing and needs no room, so a script can call it to learn which room a directory maps to. `--root` overrides the resolved project root. A `PATH` that does not exist fails with `the path does not exist`.

```console
$ rimz workspace resolve
{
  "workspace_id": "ws_f89e49906df0621ad2765112",
  "project_root": "/home/me/code/query-engine",
  "cwd_project_root": "/home/me/code/query-engine",
  "root_class": "repo",
  "worktree_root": "/home/me/code/query-engine-worktrees/docs",
  "worktree_branch": "docs",
  "session_name": "query-engine-f89e",
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

The workspace ID is derived from the project root, so a project moved on disk resolves to a new, empty workspace. `migrate` carries the old store over: it moves the store directory to the new ID and rewrites the workspace identity in its events, queued messages, and workspace record. `OLD_ROOT` may be a path that no longer exists. When both roots resolve to the same ID, the store stays in place and only the identity is rewritten. Run it before starting a room in the new location.

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
| `NEW_ROOT` does not exist | `resolving new root PATH`, then `could not resolve workspace from PATH: the path does not exist; check the path or create the directory first` |
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

The current workspace must have a store: without one the command fails with ``no room at PATH: run `rimz start` there, or pass --root <room>``.

The output opens with `event-log rotated` or `event-log rotation skipped`, followed by indented `key: value` lines. Both start with `workspace` (the workspace ID). A rotation then prints `bytes rotated`, `archive` (the archive path), and `carryover` (`N agent(s)`). A skip prints `current bytes` and `threshold`. Both end with `pruned` (`N archive(s)`) and `bytes pruned`. How rotation and carryover work is in [store internals](../../internals/store.md#rotation-and-carryover).

## Update RimZ

```sh
rimz update [--version <TAG>]
```

`update` upgrades RimZ through the method that installed the running binary:

| Installed with | What `update` runs |
| --- | --- |
| Homebrew (the binary lives under a `Cellar` directory) | `brew update`, then `brew upgrade rimio-ai/rimz/rimz`. |
| Cargo (the binary is in Cargo's bin directory) | `cargo install --locked rimz`. A source build (a version containing `+g`) prints a warning that the crates.io release will replace it. |
| The install script or a downloaded binary | Resolves the latest release, downloads the archive for this platform, verifies it against the release's `SHA256SUMS`, runs the extracted binary's `--version`, and atomically replaces the running binary in place, printing `Updated rimz OLD → NEW at PATH`. A numbered tag equal to the running version prints `rimz VERSION is up to date.` before any download. A source build (`+g` in its version) and `latest-main` always download; a staged build identical to the running one prints `rimz VERSION is already up to date.` and replaces nothing. Either way `update` continues to packs. |

When the binary changed, `update` runs the new build's [`rimz reload`](#reload-running-sidebars), so live sidebars and dashboards move to it and outdated language-server brokers retire without interrupting their servers. A reload that fails to start or exits nonzero does not stop pack updates. After packs, `update` exits 1 with ``RimZ updated, but `PATH reload` exited with STATUS; see its fixes above`` (or the error that prevented starting reload).

After the binary step and any reload attempt, the active binary runs [`rimz packs update`](./packs.md#update-git-sources), even when RimZ was already current. Unpinned official packs follow that binary's `v<version>` tag; unpinned third-party sources follow their remote default branch. Pinned and linked sources do not move. With no recorded sources, the pack step prints nothing. If the agents home's `packs/` directory belongs to another user, the step is skipped with a notice to run `rimz packs update` as its owner, without changing the exit code; this keeps `sudo rimz update` from leaving root-owned checkouts in a user's home. An unreadable installation record is reported by the pack step rather than silently skipped.

Pack reviews and permission holds are the same as for `rimz packs update`: non-widening updates apply without asking, and widening updates ask on a terminal or are held with exit 1 without one. `rimz update` has no `--yes`; use `rimz packs update --yes` to approve a held update explicitly. A failed or non-terminal-held pack step makes `update` exit 1, saying RimZ itself is updated or current but pack updates did not complete and naming `rimz packs update` as the retry. If reload also failed, both errors are reported. The binary is not rolled back, nor are successful earlier pack updates. A failure in the binary update itself still stops before packs.

An official pack without the binary's `v<version>` tag, possible on `latest-main`, fails the pack step with the missing-tag error. Rolling back with `--version` to a binary that predates `packs` likewise fails that step with the older binary's usage error.

`--version <TAG>` picks the release:

| Install | Accepted tags |
| --- | --- |
| Script or download | Any release tag, such as `v0.3.1` or an older tag to roll back, or the rolling `latest-main` build. A tag that is not a valid release tag fails with ``release tag `TAG` is invalid; use a tag such as `v0.3.1` or `latest-main` ``. |
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

`reload` promotes the invoking binary to live rooms and dashboards, and retires outdated language-server brokers against the first `rimz` on PATH, without changing any pane. It runs from any directory, inside or outside a room. One run:

- publishes the invoking build as the target of every live room, so each room's sidebars restart onto it in place;
- stops each live room's running hook drainer after its current frame, then nudges a replacement on the recorded build; a room with no running drainer starts none;
- upgrades a room's Zellij presence plugin when its build or configuration differs, and leaves a current one alone;
- re-applies `[tmux]` room options, `[mux] status_bar` and the whole room keymap to each live tmux room, removing the previous marked bindings and reconciling the copy-mode key style, reporting any failed apply while continuing with other rooms;
- moves the `rimzd` loop panel, Codex broker, content supervisors and their configured commands onto the room's new build in place, and restarts the Claude remote-control host (an open phone session drops);
- restarts standalone `rimz stats --refresh` dashboards and stats children of older, non-following content supervisors;
- retires outdated language-server brokers without interrupting running servers or attached editors;
- reaps sidebar processes whose pane is gone, and sweeps leftover processes and runtime files from stopped sessions;
- restarts the shared [web](./web.md) daemon if it is running. A stopped daemon stays stopped; a failed restart appears in the report's closing block.

It never starts a stopped session, creates or closes a pane, or touches an agent wrapper, loop run, or `rimz wait`. If the new build fails to start, the sidebar and following rimzd processes keep running on their old build. A content slot restarts its configured command, even when that command is not RimZ.

The rimzd processes follow a verified build change in the room record, normally within about one second; an unchanged build restarts nothing. Panes launched before this support do not follow: after upgrading, close the `rimzd` tab and run `rimz sidebar repair` once to recreate them.

A drainer that cannot stop within the bounded wait keeps its old build until it goes idle or a later reload succeeds. Reload reports it and continues with the other surfaces and rooms. Pending hooks stay queued across a restart.

A broker on the PATH build is left alone, including a staged copy of that build. An outdated broker retires now if dormant, with no start pending and no editor attached; otherwise it leaves after its server next stops and no editor remains. Its leases stay for the next query to start a fresh broker, without relaunching agents. An outdated broker already exiting counts as retired; one that exits during the check is left out of the report. Older brokers that cannot receive the retirement mark are retired only when their server and editors are gone; reload reports the rest and asks for another reload once they are idle. If the installed build cannot be read from PATH, reload reports that brokers were not checked. No running or indexing language server is restarted by reload.

A room whose sidebar pane was closed has nothing to reload: its line says `no sidebar reporting in`, and the closing block offers `fix: rimz reload --repair`. The same phrase covers a renderer using a protocol this build cannot signal, which repair replaces rather than mounts. It is still a live room, never the empty state.

The headline names the invoking build and counts confirmed rooms, not individual processes. Each live room has one row: project path, mux, session and sidebar count. Rows needing you come first, then settling rooms, then healthy rooms; paths break ties in byte order. Narrow terminals drop the session column before the sidebar count and shorten paths from the left.

| Line | Meaning |
| --- | --- |
| `✓ N rooms on rimz VERSION` | Every room confirmed its build and nothing needs you. A healthy room row ends at `N sidebars`; successful drainer and plugin upgrades stay quiet. |
| `… C of N rooms on rimz VERSION · S settling` | These rooms have not confirmed yet. Their rows say `C of N sidebars · S settling, retries on its own`, including sidebars whose build could not be verified. |
| `! C of N rooms on rimz VERSION · K need you` | The closing block has K entries, rooms plus machine issues. If some rooms are settling, `· S settling` precedes the need-you count. |
| `Also: …` | Nonzero dashboard reloads, broker retirements, brokers marked to retire when their servers stop, orphaned sidebar processes reaped, leftovers swept from stopped rooms, and a web daemon restart. |
| `Claude remote control restarts in SESSION; an open phone session drops.` | An enabled Claude host was live before recording a different build. The restart follows asynchronously; unknown or stopped hosts are not listed. |
| `No running sidebars to reload.` | No live room. A second line suggests `rimz start` or `rimz attach`; machine results and fixes still follow. |

The closing block starts with `! K need you`. Each `! SESSION: problem` (or `! machine: problem`) has its own `fix:` line:

| Problem | Fix |
| --- | --- |
| `no sidebar reporting in` | `rimz reload --repair` |
| `hook drainer still on the old build` | `rimz reload once its agents are idle` |
| `new build not recorded: ERROR` | `rimz doctor --root PATH` |
| `tmux room options not applied: ERROR` | `rimz reload` |
| `presence plugin not upgraded: ERROR` | `rimz doctor --root PATH` (check plugin availability before retrying reload) |
| `sidebar could not be repaired`, `repair waits for an attached client` | `rimz attach --root PATH`, then `rimz reload --repair` |
| `sidebar working but not docked`, `rimzd tab could not be reopened` | `rimz reload --repair` |
| `another repair holds the room` | `rimz reload --repair once it finishes` |
| `presence plugin silent` | `rimz attach --root PATH` |
| `N language-server brokers too old to retire` | `rimz reload once idle` |
| `no readable rimz on PATH; brokers not checked` | `put rimz on PATH, then rimz reload` |
| `web daemon restart failed: ERROR` | `rimz web restart` |

Zellij settings that cannot apply live appear in muted `Applies when the room next starts:` or `Applies when you next attach:` blocks, comparing your effective machine config with what RimZ last asserted. Each room gets a targeted reset or attach command. Reset closes the room's agents. Rooms started before settings tracking instead get `N rooms started before RimZ tracked room settings; changes there show after their next start.` No setting is inferred for those rooms. tmux has no pending settings; before tmux 3.7, a `history_limit` change still affects only new panes, not existing panes' scrollback. These notices do not count as problems; see [multiplexer settings](../../guide/multiplexer.md#change-what-the-room-asserts) for the classes.

Exit status is 1 exactly when the closing block has an entry, after the whole report is printed: `reload left 1 item that needs you; see the fixes above`, or `reload left K items that need you; see the fixes above` for several items. Settling and pending settings alone exit 0. `--repair` runs structural repair and reopens wholly closed `rimzd` tabs before the report, so rows show the repair result, including recovered sidebars. A completed repair replaces the missing-sidebar warning; incomplete repairs name their own problem. Repair never undoes the published build. Standalone [`rimz sidebar repair`](#repair-sidebars) retains its own report and exit rules. How a sidebar hands over to a new build is in [sidebar internals](../../internals/sidebar/sidebar.md#build-promotion).

## Repair sidebars

```sh
rimz sidebar repair
```

`sidebar repair` fixes the sidebar panes themselves, in every live room: it adds a missing sidebar, closes duplicate or unresponsive ones, and restores a sidebar's docking and width. A replacement pane must start and report on the current build before the old one closes. Each view with a successfully added or closed pane leaves a [`sidebar_view_repaired` diagnostic](../../internals/diagnostics.md#event-taxonomy) naming those panes; `rimz doctor` shows it as recovered information. After sidebar repair it also reopens a wholly closed `rimzd` tab, including in rooms opened by supervised runs. A tab still present is left to automatic daemon-pane repair. It publishes no build; use [`reload`](#reload-running-sidebars) for that.

The report prints one line per outcome that occurred, in this order:

| Line | Meaning |
| --- | --- |
| ``Presence plugin silent or stale in N sessions; repair ran from Zellij's live pane list. Run `rimz doctor`; reattach or restart the session to reload the plugin.`` | Plugin health is diagnostic; it does not prevent structural repair. |
| `Recovered N sidebars in place.` | Sidebars added to a view that had none, or whose only sidebar was unresponsive. |
| `Closed N duplicate or unresponsive sidebars.` | Extra or dead sidebar panes removed. |
| `Repaired N sidebars geometry.` | Kept sidebars moved back to the full-height left dock. |
| `Reopened the rimzd tab in N sessions.` | Wholly closed daemon tabs reopened from their room's recorded build. |
| ``Deferred reopening the rimzd tab in N sessions (no attached client); attach and re-run `rimz sidebar repair`.`` | A detached Zellij session needs a client before its daemon tab can be reopened; no tab or sidebar was created. |
| ``Could not reopen the rimzd tab in N sessions; re-run `rimz sidebar repair`.`` | Reopening the daemon tab failed this pass. |
| `Reaped N orphaned sidebar processes.` | Sidebar processes whose pane is gone were stopped. |
| `N sidebars still working but not docked.` | Running, but still outside the left dock after repair. |
| ``Deferred N sidebar repairs (no attached client); attach and re-run `rimz sidebar repair`.`` | A detached Zellij session cannot take a new pane or geometry change until a client attaches. |
| ``N sidebars could not be repaired; attach and re-run `rimz sidebar repair`.`` | The add or repair did not complete this pass. |
| ``Repair busy in N sessions (another repair holds the lock); re-run `rimz sidebar repair` when it finishes.`` | Another pass held the per-session lock beyond the sixty-second wait. A busy session skips its daemon-tab reopen. |
| `Sidebar structure is healthy across N sessions.` | Nothing above occurred. |
| `No running sidebars to repair.` | No live room. This line is printed alone. |

After the full report, the command exits 1 with `Sidebar repair is incomplete; see the report above.` if a sidebar repair or daemon-tab reopen failed or was deferred, a sidebar remained misdocked, or a session was busy. Otherwise it exits 0. A silent or stale plugin alone does not fail this standalone command; `rimz reload --repair` also names it in its closing block. The repair rules are in [sidebar internals](../../internals/sidebar/sidebar.md#structural-repair).

## Reset a wedged room

```sh
rimz reset [--yes] [--no-start] [--hard] [--account <KIND@NAME>]... [PATH]
```

`reset` tears down the room for `PATH` (default `.`) and rebuilds it empty. Use it when a room is stuck, or came back wrong after a reboot. In order, it:

1. deletes the room's multiplexer session and purges the multiplexer's resurrection cache for it;
2. signals the room's orphaned processes and clears the runtime `sock/`, `live/`, and `lanes/` classes, keeping lock inodes;
3. cancels the room's active supervised runs, which report `canceled` to anyone waiting on them, and drops every pending [`stop --when-idle`](./agents.md#stop) request, raised ask, card alert, human-turn stamp, hibernation record, and queued hook frame;
4. archives the active event log, clears the room's `cache/` (including skill copies in `cache/skills/` and system SSH-config copies in `cache/ssh-config/`), and clears its recorded provider accounts and run-only remote-control waivers;
5. starts the room again and attaches, with no prior agents recovered.

| Flag | Effect |
| --- | --- |
| `--yes` | Skip the `[y/N]` prompt. Required when stdin is not a terminal. |
| `--no-start` | Stop after step 4 and print ``Room torn down. Run `rimz start` to rebuild it.`` |
| `--hard` | Also delete prior-agent carryover, the active event log, `audit/` (transcripts, messages, diagnostics), `owned/` except `owned/runs/`, `tmp/`, `shared/`, `team-notes/`, `automation-notes/`, and `out/`. |
| `--account <KIND@NAME>` | Rebuild the room under this [provider account](./accounts.md), with no agents. Repeatable. Cannot be combined with `--no-start`. |

`rimz accounts use` moves a running room's launch default in place; `reset --account` rebuilds the room under a different account instead.

### What survives

Your agents' own session files stay on disk, and so do the archived event logs. A plain reset also keeps transcripts, message history, run records, agent scratch under `owned/`, agent temp files, the room's `shared/`, `team-notes/`, `automation-notes/`, and `out/` directories, and the prior-agent carryover as history. Both resets keep the standing fleet budget choices, canceled run records, and state `locks/`. Waiters can read a canceled run's record after reset; terminal run records are reclaimed after the owned GC grace.

The reborn room starts with no agents either way. A plain reset run with no terminal (`--yes` from a script) ends none of the top-level agents: the ones the old room lost stay parked, and `rimz agents resume <lane>` brings a lane back. Their subagents are closed (`rimz.child-not-resumed`), because a resumed parent relaunches any children it still needs. With `--hard`, the carryover is deleted: those agents are no longer offered for recovery, even if their identities remain in the pending record.

A process an agent left running in the background can still be inside the agent's scoped view, using the `/tmp` that `--hard` deletes. After step 2, `reset --hard` ends every such process. If one survives, reset stops before step 3, lists each remaining pid and command line, and ends with ``Stop them, then run `rimz reset --hard` again.`` Processes outside a scoped view are left alone, and a plain reset ends none ([temp units](../../internals/sandbox.md#temp-units)).

A room written by an older RimZ is removed whole, with its runtime tree, and no history is carried over.

### Refusals

Reset refuses before prompting or changing anything when:

- `--mux` names a backend other than the one the room runs on;
- the room is held: a listed session without its own live sidebar, or an older-layout room, while another process holds the workspace (close that leftover session from its multiplexer instead);
- without `--no-start`, the rebuilt room's multiplexer sockets would not fit the path budget;
- stdin is not a terminal and `--yes` is absent: `` `rimz reset` deletes the session and sweeps its processes; pass --yes to confirm without a terminal ``.

Answering anything but `y` or `yes` (any case) at the prompt prints `Reset aborted; nothing changed.` and exits 0.

### The reset report

The report goes to stderr before the rebuild, one line per fact:

| Line | Meaning |
| --- | --- |
| `Reset: session deleted, N cache entries removed, N orphan processes swept.` | Steps 1 and 2. `session not deleted` replaces `session deleted` when the multiplexer refused. |
| `Reset: could not delete the session: ERROR` | Only after `session not deleted`. The teardown still runs its remaining steps. |
| `Scoped views: ended N processes still inside the room's scoped views.` | Only under `--hard`, when it ended any. |
| `Room tmp: kept; skill copies: cleared.` | `cleared` replaces `kept` under `--hard`. |
| `Records: archived N bytes to PATH.` | The active event log was archived. `Records: no active event log archived (N bytes).` when it was empty. |
| `Records: canceled N runs, removed N debug entries, runtime removed.` | `runtime already clean` when there was no runtime state left to remove. |
| `Records: prior agent rollup kept (N agents).` | `Records: prior agent rollup cleared.` under `--hard`. |

What reset does to the store is in [store internals](../../internals/store.md#maintenance).

## Sweep stale state

```sh
rimz gc [--older-than <DURATION>] [--dry-run] [--json] [--all]
```

`gc` removes state that has outlived its use and keeps anything dirty, unlanded, or unknown. By default it sweeps only the current room: the running room's identity when the command runs inside one, otherwise the room the current directory resolves to, and `--root` overrides both. `--all` adds every room and shared machine state; store maintenance and worktrees stay current-room only. When no room resolves, `gc` fails with `resolving the room for gc; run inside a room or pass --root`.

| Flag | Default | Effect |
| --- | --- | --- |
| `--older-than <DURATION>` | the `gc.older_than` setting, `7d` | The age cutoff for `runtime` and `temp files`. Units: `s`, `m`, `h`, `d` ([durations](../cli.md#durations)). Must be greater than zero. |
| `--dry-run` | off | Report what would be removed and remove nothing. `messages`, `event log`, `agent cache`, and `loop schedules` show as skipped. |
| `--json` | off | Print the report as JSON instead. |
| `--all` | off | Sweep every room and shared machine state. |

The sweep runs eight areas, in the order the report prints them. Before them, the report lists the lifetime classes swept in each selected room.

| Area | Room scope (default) | Machine scope (`--all`) |
| --- | --- | --- |
| Room class blocks (not an area) | The current room's lifetime classes. | Every remaining room, after dead workspace pruning. |
| `worktrees` | Removes current-repository RimZ-owned worktrees that are clean, landed, and have no pane, agent, armed loop-task holder, team board short of `Done`, or unfinished workflow run, as in [`rimz worktree sweep`](./worktree.md#sweep-landed-worktrees). | Same. |
| `workspaces` | Skipped; run with `--all`. | Deletes rooms written by an older RimZ, stores whose project folder is gone, and setups abandoned before any history, along with their runtime trees. Sweeps empty working directories of rooms removed now or earlier, keeping agent directories within the seven-day grace. Unreadable records with history are kept and reported. A dead workspace that cannot be removed is reported; any surviving workspace directory is retried by the next sweep. |
| `runtime` | The current room's runtime root only; no shared provider probe markers. Live files use `--older-than`, sockets a connect probe, locks a try-lock. Also the machine's account budget ledgers that no history pool reads, counted as `account budget ledgers`. | Every runtime root, stale shared provider probe markers, and the same account budget ledgers. |
| `temp files` | Removes orphan atomic-write temps (`*.tmp.<pid>.<nonce>`) older than `--older-than` under the room's state and runtime roots. | Sweeps both whole roots. |
| `messages` | Archives open messages whose receiver has ended, and requeues or times out messages stuck as sent. | Same current-room maintenance. |
| `event log` | Cuts a corrupt tail off the event log. | Same current-room maintenance. |
| `agent cache` | Prunes prior-agent carryover older than 14 days. | Same current-room maintenance. |
| `loop schedules` | Reaps this room's dead or invalid instance rows, prunes its orphan schedule overlays, and removes unclaimed [wait outputs](./wait.md) older than 7 days. Machine `loop.toml` is untouched. | Reaps machine delivery tasks and every root's instance rows; sweeps all rooms' wait outputs. Overlay cleanup still uses the current root's known scopes. |

With either scope, a current room written by an older RimZ refuses the command with a layout error; run `rimz start` or `rimz reset` there to replace it with a fresh room.

GC never creates a store. Without one, `messages`, `event log`, `agent cache`, and repository `worktrees` show `skipped — no rimz store here`; outside a repository, `worktrees` shows `skipped — not inside a git repo`. An absent room state root produces no room class block.

The `worktrees` area is the one that reaches past RimZ's own files: removing a tree deletes that checkout and its files, and the branch behind it. A dirty, held, or unmerged tree is kept and reported, never forced. Armed loop tasks hold the paths described under [`worktree sweep`](./worktree.md#sweep-landed-worktrees), even while paused, and a team board short of `Done` or an unfinished workflow run holds its checkout too. When `git branch -d` refuses a branch as unmerged, RimZ compares its content against the base and deletes it with `git branch -D` once that comparison says the work already landed. A branch it cannot prove landed is kept. If any task source, workflow record or workflows directory, or the owned-worktree listing cannot be read, the whole worktree area is skipped and names the failure; other areas still run.

### The report

`gc` shows progress while it runs, then prints a two-line header. The first line is `gc — reclaimed BYTES`, `gc — maintenance complete` (no bytes, but loop rows or wait outputs removed), `gc — no bytes reclaimed` (nothing removed, with problems), or `gc — all clean, nothing to reclaim`. A dry run reads `gc — would reclaim BYTES (dry run)` or `gc — nothing to reclaim (dry run)`. A problem count follows (`· 1 problem`) when a worktree removal failed, a dead workspace could not be removed, a workspace record was unreadable, or the event log needed repair.

The second line is `checked N of 8 areas · cutoff DURATION`, ending in `· room <workspace-dir>` or `· machine` (`checked 8 areas` when none was skipped). A room sweep normally reads `checked 7 of 8 areas`, since `workspaces` is skipped with `skipped — machine area, run with --all`. A dry run also skips `messages`, `event log`, `agent cache`, and `loop schedules`.

Next comes one block per selected room: the room name, then one `class · N files · BYTES` row per lifetime class, plus `empty` and `transcript` rows, sorted by name. The `audit`, `transcript`, `team-notes`, and `automation-notes` rows append `· BYTES kept`: file bytes remaining after the sweep, or that would remain under `--dry-run`. The `transcript` row combines the conversation transcript and final message text. A dry run shows a lock class as `class · N files would check`.

Last come the eight area rows, each with its verdict: `✓` healthy, `✦` acted, `⚠` warning, `✗` failed, `–` skipped. Each kept, removed, or failed worktree gets its own line under `worktrees`. Kept lines name every reason (`in use`, `uncommitted changes`, `unlanded work`, `unknown`) and the holders when present, including `loop task NAME`, `team NAME`, and `workflow run ADDRESS`. Removed lines say `landed`, with the branch-deletion outcome. An unreadable workflow record or workflows directory in any room skips the worktree area with `workflow runs unavailable: <error>`, naming the path.

Problems do not change the exit code: `gc` exits 0 whenever the sweep completes.

### Retention

Owned state has a seven-day grace. Agent temp units use `gc.tmp_grace` (default `1d`) instead. A temp unit stays while any agent sharing it is live or has no recorded end. It then expires only when both its latest end and the mtime of its directory or any direct child are past the grace. Rowless units use mtime alone, and `tmp/_unnamed/` stays with the room.

Diagnostic audit files expire after 30 days, or oldest-first above 64 MiB. Neither rule applies to `audit/transcript/` and `audit/messages/`. Those histories follow `gc.transcript_retention`: `forever` by default, or a non-zero duration (`s`, `m`, `h`, `d`) that expires weekly files by their last modification. Hard reset and `uninstall --state` or `--all` still remove both.

Team archives and automation notes follow their own `gc.team_notes_retention` and `gc.automation_notes_retention` keys, both `forever` by default. A duration expires whole closed runs by their newest recursive entry modification, without following symlinks. An open team board or an unreadable manifest protects the run. A missing board counts as closed, and an unreadable or stageless board as open. Empty date directories go once their runs are removed. Before removing an old terminal workflow home, GC fills in a missing `history.txt` in its kept notes folder. A copy failure keeps the home for the next sweep, and `--dry-run` writes no copy. Notes already removed by their retention setting or by you are not recreated.

The `empty` class keeps an agent's empty working directory while any retained session records it, then removes it once its directory mtime is older than seven days. The room-level directory stays while its store exists. Once the store is gone, a machine sweep removes agent directories past the same grace, and the room-level directory when empty. The `workspaces` area reports both the files removed and the agent directories still within the grace. Unknown entries and symlink targets stay untouched.

`--older-than` changes none of these policies.

### JSON output

Fields appear in this order:

| Field | Content |
| --- | --- |
| `scope` | `room` or `machine`. |
| `dry_run` | `true` under `--dry-run`. |
| `older_than_secs` | The cutoff in seconds. |
| `reclaimed_bytes` | Bytes removed, or that would be removed, across all areas. |
| `worktrees` | `removed` (`name`, `branch`, `path`, `bytes`, `branch_deleted`, `archive_error`), `failed` (`path`, `error`), `kept` (`name`, `path`, `reasons`, `holders`), and `skipped`. `reasons` is an array of `in_use`, `uncommitted_changes`, `unlanded`, `unknown`. `holders` has `agents` (array), `panes` (a count), `loop_tasks` (task names), `teams` (team names), and `workflow_runs` (`{id, address}` objects, deduplicated by id); each array is `[]` when empty. `skipped` is `null`, `not_a_repo`, `no_store`, `roster_unavailable`, `loop_tasks_unavailable`, `workflow_runs_unavailable`, or `list_failed`. The domain records match `worktree sweep --json`. |
| `workspaces` | `skipped` (`room_scope` for a room sweep, otherwise `null`), `removed` (`workspace_id`, `dir_name`, `reason`, `project_root`, `bytes`), `failed` (`dir_name`, `error`), `retained_unreadable` (`workspace_id`, `error`), `kept` (a count), and `empty_dirs` (`dir_name`, `files_removed`, `bytes`, `agent_dirs_kept`). `reason` is `project_root_gone`, `abandoned_scaffold`, or `incompatible_layout` (a room written by an older RimZ). `workspace_id` and `project_root` are `null` for a directory that never recorded its workspace. `failed` is `[]` when no removals failed, including dry runs and room scope. `empty_dirs` lists every orphan tree visited, even when only recent agent directories remain, and is `[]` at room scope. Its bytes count toward `reclaimed_bytes`. |
| `temps` | `files_removed`, `bytes_removed`. |
| `runtime` | `roots_scanned`, `heartbeats_removed`, `sidecars_removed`, `sockets_removed`, `probe_markers_removed`, `dirs_removed`, `bytes_removed`. |
| `rooms` | Per-room `name` and `classes` (`class`, `files_removed`, `bytes_removed`, `locks_would_check`). Only `audit`, `transcript`, `team-notes`, and `automation-notes` add `bytes_retained`, file bytes remaining after the sweep (or projected under `--dry-run`). Room scope lists only the current room, or none when its state root is absent. |
| `messages` | `archived`, `reconciled`. |
| `carryover_pruned` | Carryover entries pruned. |
| `schedules_reaped` | Loop delivery tasks removed because their target is gone, plus RimZ-owned instance rows removed because they no longer compile to an action. |
| `wait_logs_pruned` | Wait output files pruned. |
| `budget_ledgers_removed` | Account budget ledgers no history pool reads, removed or, under `--dry-run`, that would be removed; their bytes count in `reclaimed_bytes`. |
| `repair` | `bytes_truncated` and `frames_kept`, or `null` when store maintenance was skipped. |
| `store_maintenance` | `done`, `skipped_dry_run`, or `skipped_no_store`. |

What each sweep removes on disk is in [store internals](../../internals/store.md#maintenance).

### Automatic sweeps

Every open room runs `gc` on its own once a day, with the `gc.older_than` cutoff. The first sweep waits until the room has been up for 5 minutes, so a room coming back from a reboot finishes restoring its panes before the worktree area judges them. The last sweep time is kept per workspace, so a reboot does not reset the day. Set `gc.auto = false` ([configuration](../../guide/configuration.md#garbage-collection)) to sweep only by hand.

One of those helpers is elected for a machine sweep when the last automatic machine attempt is at least 24 hours old, or none is recorded. It runs the same scope as `--all`, including its own room, and the other helpers sweep only their rooms. Failed attempts are stamped too, so a failing sweep retries a day later. Apart from free-space eviction below, closed rooms wait for that elected sweep or a manual `--all`, and start no helpers themselves.

While a machine config file fails to load ([`rimz config get`](./config.md#read-a-value) names the error), automatic sweeps pause and do not fall back to the defaults. The one exception is an unreadable `loop.toml` while the core config stays readable: the sweep runs and skips only the worktree area. Loop arming overlays are kept while task sources cannot be read.

Each run is recorded in [`rimz stats --assists`](./stats.md#the-assist-timeline). Repeated sweeps of the same workspace and scope fold into a `♻` line with summed reclaimed bytes, removal counts, and problem counts, and `--all` includes failed runs. An assist record carries `scope` (`room` or `machine`) and `class_bytes`, the reclaimed bytes by class for the helper's own room only, even in a machine sweep. Its other totals cover the run's full scope.

### Free-space eviction

Each open room also checks free disk space on its heavy refresh. Below the floor, `max(gc.tmp_floor_percent% of capacity, gc.tmp_floor_min)` (by default the larger of 10% and 10 GiB), it starts a machine-wide temp-unit eviction helper, at most once per ten minutes per producer. One machine lock elects the helper. The helper removes ended and rowless units from all known rooms, closed rooms included, oldest first, until measured or estimated free space reaches the floor. Ended units are ordered by their newest owner's end, rowless ones by mtime.

The estimate starts with free space before eviction and adds each removed tree's allocated bytes. It bounds each pass when the filesystem reclaims space late, as when snapshots retain the removed blocks. The floor measures the filesystem that holds the workspace directory; separately mounted room temp directories are not supported.

Eviction ignores `gc.tmp_grace`, but never takes a unit from a live or not-yet-ended owner, and never removes `tmp/_unnamed/`, owned state, or shared files. Each attempted removal appends a `tmp_evict` assist, failures included, and one failed removal or assist append does not stop the other candidates. If measuring space fails just after an eviction, the helper warns and the record keeps the last successful free-space reading; the next removal still needs a fresh reading. `gc.auto = false` and a machine config that fails to load both pause the helper. Setting both floor settings to zero disables only this eviction.

`rimz doctor` previews the machine-wide free space and temp-unit inventory in `TEMP UNITS`, and suggests `rimz gc --tmp-floor`, a hidden flag that runs only this pass by hand. It conflicts with `--all`, `--dry-run`, and `--older-than`. A plain `gc --dry-run` previews only age-based collection.

| Outcome | stdout | stderr | Exit |
| --- | --- | --- | --- |
| Pass completed | One `HANDLE: BYTES (WORKSPACE_ID)` line per attempted eviction (` failed: ERROR` appended on failure), then `free BYTES of BYTES, floor BYTES` | | 0 |
| Pass completed, `--json` | `free_bytes`, `total_bytes`, `floor_bytes`, and `evictions` (the assist records) | | 0 |
| `gc.auto = false` | Empty, also with `--json` | `pressure eviction is paused by gc.auto = false; rimz gc --all removes temp units past gc.tmp_grace; rimz config set gc.auto true resumes pressure eviction.` | 0 |
| Another pass holds the lock | Empty, also with `--json` | `another free-space pass is already running` | 0 |
| Machine config fails to load | | The load error, as `rimz config get` names it | 1 |

With `gc.auto = false`, doctor names the pause instead of offering the command. Eviction counts reclaimed bytes as allocated blocks (`st_blocks × 512`), counting hardlinked files once per tree. Ordinary GC and doctor's `--audit` inventory report apparent sizes, and a default doctor run counts units without measuring them. `free_bytes_after` in each eviction record and the closing free-space figure are filesystem measurements, not the estimate, and may still be below the floor when the pass stops.

## Uninstall RimZ

```sh
rimz uninstall [--state] [--config] [--all] [--keep-binary] [--yes]
```

`uninstall` removes RimZ from the machine. It prints a preview of every root, room, hook, skill-link root, timer, and binary it will touch, plus the account homes it keeps, asks `[y/N]`, and then reports each removal. Preview and report go to stderr.

| What | Default | Flag to change it |
| --- | --- | --- |
| RimZ hooks, in each provider's own home and every declared account home | Removed | |
| RimZ skill links into the library or packs in each provider's skill root, including declared account homes | Removed, including dangling owned links; your own entries are kept | |
| Running rooms, on both backends | Torn down | |
| The external [loop timer](./loop.md) | Removed | |
| The runtime tree and `~/.rimz/cache/`, including the provider caches under `cache/providers/` | Removed | |
| [Provider account](./accounts.md) homes under `~/.rimz/accounts/` | Kept: their credentials and history belong to the provider; the preview names each existing declared home and the `rm -rf` command to clear it by hand | |
| RimZ role, effort, and closed-day spend history under `accounts/.history/` | Always kept: other homes sharing the accounts folder use these durable ledgers | |
| Durable room stores, logs, and registries (`ws/`, `logs/`, `loops/`, `web/`, `builds/` under `~/.rimz`) | Kept | `--state` removes them |
| Per-machine config, themes, trust grants, plugins, and remote aliases (`config.toml`, `theme.toml`, `loop.toml`, `remote.toml`, `cursor-statusline.json`, `trust/`, `agents.d/` under `~/.rimz`) | Kept | `--config` removes them |
| Agent library (`agents/`, `subagents/`, `teams/`, `traits/`, `skills/`, `profiles/`), installed `packs/`, `handoffs/`, and `notes/` | Always kept, including linked pack checkouts | |
| `rimz` binaries at the running executable, Cargo's bin directory, and `/usr/local/bin` | Removed | `--keep-binary` keeps them; `RIMZ_SYSTEM_BIN_DIR` replaces `/usr/local/bin` |
| Project `.rimz/` directories and RimZ-owned worktrees | Always kept: they can hold project config and unlanded work | |

`--all` is `--state` plus `--config`. `--yes` skips the prompt. Without it and without a terminal, `uninstall` fails with `` `rimz uninstall` removes hooks, rooms, runtime/cache/data, and the binary; pass --yes to confirm without a terminal ``. Answering anything but `y` or `yes` (any case) prints `Uninstall aborted; nothing changed.` and exits 0. The paths above are the defaults; `RIMZ_HOME` moves them. Anything else under the home, such as `cache/` and the `run` link, is removed.

Run it from outside any RimZ room: inside one it fails with `detach and rerun from outside the RimZ room`. A Homebrew install also needs `brew uninstall rimz`. A successful run ends with `Uninstall complete.` When any step fails, the rest still run, and `uninstall` exits 1 with `uninstall incomplete:` and one `- FAILURE` line per failure. Each binary it had no permission to delete then gets a `sudo rm PATH` line under `Protected binary paths may need sudo:`.

## Check that the binary runs

```sh
rimz ping
test "$(rimz ping)" = ok
```

`ping` prints `ok` and exits 0 when the binary starts and its global flags parse. It touches no room, so it is the cheapest liveness probe for a script or health check.
