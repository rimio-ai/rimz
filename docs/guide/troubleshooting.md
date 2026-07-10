# Troubleshooting

When something looks off, `rimz doctor` is the first move, this page is the symptom-to-fix catalogue for the rows doctor points at.

## Start with `rimz doctor`

`rimz doctor` reports the whole room in one pass: the multiplexer backend and whether its version clears the floor, per-agent hook status, workspace and store health, project trust state, terminal color depth, scheduled loop tasks, and the Zellij presence grant. It resolves the workspace, probes each of those surfaces, and prints a verdict. It writes nothing, so you can run it as often as you like.

```sh
rimz doctor
```

The report reads top to bottom, one verdict per row (trimmed here to its shape):

```
rimz doctor
  version: 0.1.0+g90deb6d64e14
  user:    marvin (uid 1000)
  binary:  /home/marvin/.cargo/bin/rimz

WORKSPACE
  id:              ws_f89e49906df0621ad2765112
  project root:    /home/marvin/workspace/project-rimz/rimz
  session:         rimz-rimz-f89e49
  sock headroom:   ✓ OK (79/108 bytes for …)

MULTIPLEXER
  backend:        zellij
  version:        zellij 0.44.3
  zellij floor:   ✓ OK (>= 0.44.0 required)
  session health: ✓ ok
  presence:       ✓ event mode (poked 12s ago)
  …

TERMINAL
  depth:   ✓ truecolor (mode truecolor)

HOOKS
   AGENT     STATUS     FIX
✓  claude    installed  -
✓  codex     installed  -
✓  pi        installed  -
✓  opencode  installed  -

TRUST
  trust: ✓ trusted (granted 2026-07-07T13:59:21.705032339Z)

AGENTS
  17 live: 3 running, 1 waiting, 7 idle, 6 success

MESSAGES
  0 open — `rimz message list`
   ID                    STATUS   TARGET                 AGE      PROBLEM
✗  msg_06fk51tqadqpvd28  errored  @coder#bandwidth-tune  18m ago  receiver not found
✗  msg_06fk51isbpsgl81l  errored  @coder                 20m ago  receiver not found

DIAGNOSTICS
  12 recent records (…/diag.log.jsonl)
  …

✗ 2 problems, ⚠ 9 warnings
```

Run it before anything below. Most symptoms on this page show up as a named doctor row with the fix already attached, so the report doubles as the fix list.

### Reading the report

Each row carries a glyph that sets its tone:

- `✓` clear, `●` informational, `·` neutral background fact.
- `⚠` a warning: working, but worth a look.
- `✗` a problem: something is broken and a `FIX` column or inline hint says what to run.

The last line tallies the run (`✗ 2 problems, ⚠ 9 warnings`, or `✓ no problems found`). Work top down and clear the `✗` rows first. The rest of this page expands the common ones.

The tail sections (`AGENTS`, `MESSAGES`, `DIAGNOSTICS`) report live state rather than machine setup: the agents doctor observed, messages that failed to land, and RimZ's own record of transient rendering hiccups. They are mostly context for a bug report, not a to-do list.

## The room won't start

### RimZ reports an existing room instead of opening one

A RimZ room is its own Zellij or tmux session, so it cannot nest inside a session you are already attached to. Run bare `rimz` (or `rimz start`) from inside one, and instead of nesting it names the room and tells you how to reach it:

```
You're already inside a zellij session, which can't host a nested room.
This directory's room is `rimz-rimz-f89e49`. Detach to (re)launch it, or run `rimz` from outside the session.
```

Open a fresh terminal window that is not attached to Zellij or tmux, then run `rimz` there.

### Zellij or tmux is missing or too old

RimZ needs Zellij 0.44+ or tmux 3.5+ on the machine, and pixel-perfect pets add tmux 3.6+. The `MULTIPLEXER` section reports the detected backend and its version against the floor (`zellij floor: ✓ OK (>= 0.44.0 required)`, or `TOO OLD`). Install or upgrade the multiplexer if the row flags it. When both are installed and doctor resolved the one you did not want, pick a backend explicitly with `--zellij`, `--tmux`, or `--mux <name>`.

## An agent isn't showing as a card

### It shows as a plain process row

An agent earns a live card only once its reporting hooks are installed. The hooks are a handful of lines RimZ adds to the agent's own config file; through them the agent reports its turns, prompts, and blocking questions to the sidebar. Without those lines the agent still runs fine, but the sidebar has nothing to read, so it renders a plain process row instead of a card. Preview the lines, then install them:

```sh
rimz hooks install --dry-run    # per-agent summary plus a unified diff; writes nothing
rimz hooks install              # wire every detected agent (claude, codex, pi, opencode)
rimz hooks install claude       # wire one agent by name
```

The install is additive, so your existing hooks stay, and `rimz doctor` reports per-agent hook status afterward. Restart the agent so it picks up the new hooks. To back the change out, `rimz hooks uninstall [AGENT]` removes exactly what RimZ added and restores any statusline it wrapped.

### A card went quiet after a config edit

If a card stops updating after you hand-edited an agent's config, the RimZ-managed hook block was likely disturbed. Re-run `rimz hooks install --dry-run` to see the current diff, then `rimz hooks install` to restore the block. Some agents gate hooks behind their own trust prompt; when one reports installed-but-untrusted hooks, `rimz doctor` prints the exact fix in the `HOOKS` row.

### Zellij pane discovery stopped

On Zellij, RimZ loads a small presence plugin so the sidebar learns which panes exist and where. Its permission grant is seeded for you on the first attach. Revoking that grant in Zellij's plugin manager stops pane discovery until you restore it, and `rimz doctor` names the fix in the `presence` row. Re-grant the plugin's permissions in Zellij, then run `rimz reload`. The grant lives in Zellij's own permission store, and the plugin ships no pane content anywhere; the full picture is in [security and trust](./security.md#the-zellij-presence-plugin).

## The sidebar looks wrong

### The "Sidebar degraded" banner

A banner such as `⚠ Sidebar degraded for 8s: snapshot failed` means a render could not read a fresh snapshot, usually because the binary moved on disk or the durable state directory vanished mid-write. Nothing is lost: the durable store stays the source of truth, so fixing the underlying cause (reinstall or repoint the binary, confirm the state directory exists) clears the banner on the next good frame. Once it recovers, the banner steps down to a dim, dismissable notice (`⚠ last alert 8s ago: … · x dismiss`) so a failure that flickered past is still visible after the fact. Press `x` to clear it; a fresh failure re-arms it.

### Colors, glyphs, or pets don't render

Three layers decide what renders, and the upgrades need terminal support:

```sh
rimz config set theme.style modern        # truecolor + Nerd Font icons
rimz config set theme.pets.enabled true   # an animated companion on the dashboard
```

`modern` needs a Nerd Font installed in the terminal, and pets render as crisp pixels only in Ghostty and kitty; inside tmux that also needs tmux 3.6+ with `allow-passthrough on`. Everywhere else, including Zellij, the pet falls back to cell art. The full appearance model is in [theming](./theme.md); the per-terminal pet notes are in the [pets guide](./pets.md).

### A pane looks stale after upgrading RimZ

When the running RimZ build drifts from an agent's tested version range, reporting fidelity degrades rather than a pane freezing outright: `rimz doctor` warns, and blocking prompts still route to the agent's own UI. After upgrading the binary, reconcile the running sidebars onto the new build:

```sh
rimz reload    # re-exec sidebars onto the current build, repair geometry, close duplicates
```

`rimz doctor` also warns when more than one RimZ build is writing the workspace at the same time; `rimz reload` is the fix for that mixed-build state too.

`rimz reload` runs from anywhere and leaves stopped sessions stopped. Agent version drift and its exact effects are in [agent support](../reference/agent-support.md).

## Notifications don't fire

RimZ raises a desktop notification by writing a terminal notification escape (OSC 777) from the sidebar; your terminal turns it into the OS banner, even over SSH. When no banner appears, check in order:

- **Zellij rooms.** Zellij currently drops notification escapes, so `desktop = "auto"` skips them there. For OS-level notifications on Zellij, wire a `[[notifications.handler]]` command (`notify-send`, `ntfy`, or anything else); the shape is in [the configuration guide](./configuration.md#notifications).
- **tmux rooms.** RimZ turns `allow-passthrough` on in its rooms by default, which is what lets the notification bytes through tmux. A personal config that forces it off blocks them.
- **Terminal and OS.** The terminal must support notification escapes, and the OS must allow notifications from that terminal app (on macOS, System Settings, then Notifications).
- **Triggers.** Only the statuses in `notifications.triggers` fire, and the default is `["waiting", "failed"]`. Add `"success"` if you expect completion pings.

A missed notification loses nothing: the sidebar is the source of truth, so the row stays unread and ranked until you visit it, and an agent that keeps waiting earns a reminder nudge.

## Project config isn't taking effect

A cloned repository can ship a `.rimz/config.toml` that names agents, profiles, teams, loop tasks, hooks, and environment variables, any of which can run a command. So RimZ keeps that file inert until you trust the workspace: on an untrusted clone it reads only structural metadata, and every command-running field stays disabled.

```sh
rimz trust status    # show the trust state and, when stale, a field-level diff
rimz trust grant     # pin the current executable surface as trusted
```

Editing any command-running field re-hashes the config, so `rimz trust status` and `rimz doctor` report `stale` on the next read (there is no background sweep) and the command-running fields disable themselves until you re-grant. Review the diff that `rimz trust status` prints, then run `rimz trust grant` again. The full model, including exactly which fields count, is in [security and trust](./security.md#project-trust).

## The remote link keeps dropping

A remote room is plain SSH under a supervisor that reconnects itself when the link drops, and the sidebar footer carries a `⇄ remote` badge that reads link health at a glance. A link that will not hold usually fails the underlying SSH prerequisites. Confirm you can `ssh` to the target unattended (key-based auth, a reachable host), then reconnect. Saved aliases and the reconnect model are in [remote](./remote.md).

## Reset and clean up

These commands touch RimZ's own state, never your project files. Each one says exactly what it removes and what it keeps.

### Start a room clean

To skip recovering prior agents when a room is reborn, pass the global `--no-resume`. The room comes up empty instead of seeding the fleet you left; your durable records stay untouched, so a later start without the flag can still bring the agents back.

```sh
rimz --no-resume    # come up empty: skip recovering prior agents
```

### Rebuild a wedged room with `rimz reset`

`rimz reset` is the escape hatch for a room that is stuck, or that came back wrong after a reboot. Every step is one you could run by hand: it tears the session down, purges the cached session state a rebuild would otherwise reuse, archives the room's records, sweeps orphaned processes, then rebuilds and reattaches. It touches only this workspace's room.

```sh
rimz reset              # rebuild this workspace's room from clean state
rimz reset --yes        # skip the confirmation prompt (required off a TTY)
rimz reset --no-start   # tear down only, then print the rerun hint
rimz reset --hard       # also drop the prior-agent carryover, so rebirth seeds nothing
```

A plain reset keeps the prior-agent carryover for history but still starts the room empty; `--hard` removes that carryover too.

### Sweep stale state with `rimz gc`

`rimz gc` sweeps runtime state that has outlived its use: orphaned atomic-write temp files, dead workspace stores, abandoned queued messages, and clean RimZ-marked worktrees whose work has already landed with no live pane inside. It keeps anything dirty, pending, or unproven, and it always prints a checklist of what it cleaned, what it kept, and why. Run it with `--dry-run` first to see the plan without removing anything:

```sh
rimz gc --dry-run          # preview reclaimable state, remove nothing
rimz gc                    # sweep runtime state older than 24h (the default cutoff)
rimz gc --older-than 7d    # widen the cutoff
```

```
gc — would reclaim 62 MB (dry run)
  checked 4 of 8 areas · cutoff 24h

  ✦ worktrees       would remove 3 · 62 MB · 4 kept
      kept: 3 in use, 1 not merged yet
      would remove: guides-tune  21 MB  merged
      …
  ✦ workspaces      would prune 31 · 9.2 KB
      would prune: ws_8282bdd4e723c6a86e061fa8 — abandoned setup, never used (280 B)
      …
  ✦ runtime         would remove 178 stale files · 109 KB — 5 roots scanned
  ✓ temp files      none orphaned
  – messages        skipped (dry run)
  – event log       skipped (dry run)
  – agent cache     skipped (dry run)
  – loop schedules  skipped (dry run)
```

A kept worktree carries its reason (`in use`, `not merged yet`), so nothing with unlanded work is ever swept.

### Where state lives, and full removal

Per-machine config lives under `~/.config/rimz/` (`config.toml`, `theme.toml`, `agents.toml`, `loop.toml`, `remote.toml`), and durable room state lives under `~/.local/state/rimz/`. Both are plain files you can read and edit. To remove RimZ from the machine, `rimz uninstall` takes out installed hooks, running rooms, runtime state, and the binaries it finds; durable stores and per-machine config stay unless you ask for them:

```sh
rimz uninstall            # hooks, rooms, runtime state, binaries; keeps stores and config
```

`--state`, `--config`, and `--all` widen the removal to durable stores and per-machine config; the exact scope of each flag is in the [maintenance reference](../reference/cli/maintenance.md#reload-reset-gc-and-uninstall). Project-local `.rimz/` directories and RimZ-owned worktrees stay in place, because they can hold project config and unlanded work.

## Filing an issue

Capture the state RimZ sees and attach it to the report:

```sh
rimz doctor --json --output rimz-doctor.json    # full environment report as JSON
```

`rimz doctor --json` is the single best artifact: it carries the backend, versions, per-agent hook status, trust state, and the room health that most reports need, with no prompts, transcripts, or file contents.

## See also

- [Set up your machine](./setup.md) for the first-pass configuration these fixes assume.
- [Security and trust](./security.md) for the trust model and the presence grant behind two of the fixes above.
- [Remote](./remote.md) for reconnect behavior and link health.
- [CLI reference: maintenance](../reference/cli/maintenance.md) for every flag on `doctor`, `reset`, `gc`, `reload`, and `uninstall`.
