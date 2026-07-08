# Troubleshooting

> When something looks off, `rimz doctor` is the first move: it inspects the machine, names the cause, and prints the fix. This page is the symptom-to-fix catalogue for the cases doctor points at, and the recovery commands when a room needs a reset.

## Start with `rimz doctor`

`rimz doctor` reports the whole room in one pass: the multiplexer backend and whether its version clears the floor, per-agent hook status, room and store health, project trust state, and the Zellij presence grant. Each problem row carries the command that resolves it, so doctor doubles as the fix list.

```sh
rimz doctor            # human report, problem rows first
rimz doctor --audit    # widen the agent section to every observed session
rimz doctor --json     # machine-readable, for scripts and issue reports
```

Run it before anything below. Most symptoms on this page surface as a named doctor row with the fix already attached.

## The room won't start

### `rimz` refuses inside an existing multiplexer

The room is its own Zellij or tmux session, so it starts from a plain terminal, outside any session you are already attached to. Run inside one, `rimz` refuses with the room's name and the same advice. Open a fresh terminal window that is not attached to Zellij or tmux, then run `rimz` there.

### Zellij or tmux is missing or too old

Rimz needs Zellij 0.44+ or tmux 3.5+ on the machine, and pixel-perfect pets add tmux 3.6+. `rimz doctor` reports the detected backend and its version against the floor; install or upgrade the multiplexer if the row flags it. Pick the backend explicitly with `--zellij` or `--tmux` (or `--mux <name>`) when both are present and doctor resolved the wrong one.

## Agents don't show up or don't report

### An agent runs but shows as a plain process row

An agent appears live only once its reporting hooks are installed. An unwired agent still shows up, as a plain process row rather than a card, which means its hooks are missing. Preview and install them:

```sh
rimz hooks install --dry-run    # per-agent summary plus a unified diff; writes nothing
rimz hooks install              # wire every detected agent (claude, codex, pi, opencode)
rimz hooks install claude       # wire one agent by name
```

The install is additive — your existing hooks stay — and `rimz doctor` reports per-agent hook status afterward. Restart the agent so it picks up the new hooks.

### Events stop landing after a hook edit

If a card goes quiet after you edited an agent's config by hand, re-run `rimz hooks install --dry-run` to see the current diff, then `rimz hooks install` to restore the Rimz-managed block. To back the change out entirely, `rimz hooks uninstall` removes exactly what Rimz added and restores your original statusline:

```sh
rimz hooks uninstall            # remove every Rimz-managed hook set, restore statusline
rimz hooks uninstall codex      # remove one agent's block
```

### Zellij pane discovery stopped

On Zellij, Rimz loads a small presence plugin so the sidebar learns pane topology; its permission grant is seeded automatically. Revoking that grant in Zellij's plugin manager makes pane discovery unavailable until you restore it, and `rimz doctor` names the fix. Re-grant the plugin's permissions in Zellij, then reload. The grant lives in Zellij's own permission store, and the plugin ships no pane content anywhere — see [security → The Zellij presence plugin](./security.md#the-zellij-presence-plugin).

## The sidebar looks wrong

### "Sidebar degraded" banner

A banner such as `Sidebar degraded for 8s: snapshot failed: store not found` means a render could not read a fresh snapshot — usually the binary moved on disk or the store (Rimz's durable state directory) vanished mid-write. The store stays authoritative, so nothing is lost: fix the underlying cause (reinstall or repoint the binary, confirm the state directory exists) and the banner clears on the next good frame. On recovery it steps down to a dim, dismissable notice (`⚠ last alert 8s ago: … · x dismiss`) so a failure that flickered past is still visible after the fact; `x` clears it and a fresh failure re-arms it.

### Colors, glyphs, or pets don't render

The `default` style uses automatic color and Unicode everywhere. The upgrades need terminal support:

```sh
rimz config set theme.style modern        # truecolor + Nerd Font icons
rimz config set theme.pets.enabled true   # an animated companion on the dashboard
```

`modern` needs a Nerd Font installed in the terminal, and pets render as crisp pixels only in Ghostty and kitty; inside tmux that also needs tmux 3.6+ with `allow-passthrough on`. Everywhere else, including Zellij, the pet falls back to cell art. Full appearance model and per-terminal notes are in [theming and pets](./theme.md).

### A pane freezes after an upgrade

When the running Rimz build drifts from an agent's tested version range, observability fidelity degrades rather than a pane silently freezing: `rimz doctor` warns, and blocking prompts still route to the agent's own UI. After upgrading the binary, reconcile the running sidebars onto the new build:

```sh
rimz reload    # re-exec sidebars onto the current build, repair geometry, close duplicates
```

`rimz reload` runs from anywhere and leaves stopped sessions stopped. Agent version drift and its exact effects are covered in [agent support](../reference/agent-support.md).

## Notifications

### Desktop notifications don't fire

Rimz raises a desktop notification by writing a terminal notification escape (OSC 777) from the sidebar; your terminal turns it into the OS banner, even over SSH. When no banner appears, check in order:

- **Zellij rooms.** Zellij currently drops notification escapes, so `desktop = "auto"` skips them there. For OS-level notifications on Zellij, wire a `[[notifications.handler]]` command (`notify-send`, `ntfy`, or anything else) — the shape is in [configuration → notifications](../reference/configuration.md#notifications).
- **tmux rooms.** Rimz turns `allow-passthrough` on in its rooms by default, which is what lets the notification bytes through tmux. A personal config that forces it off blocks them.
- **Terminal and OS.** The terminal must support notification escapes, and the OS must allow notifications from that terminal app (macOS: System Settings → Notifications).
- **Triggers.** Only the kinds in `notifications.triggers` fire — `["waiting", "failed"]` by default. Add `"success"` if you expect completion pings.

A missed notification loses nothing: the sidebar is the source of truth, so the row stays unread and ranked until you visit it, and an agent that keeps waiting earns a reminder nudge.

## Project config isn't taking effect

### `.rimz/config.toml` fields stay inert

Project config is read inertly until you trust it: an untrusted `.rimz/config.toml` contributes structural metadata only, and every command-running field (launch commands, project profiles and teams, project loop tasks, hook commands) stays disabled until you review the diff and grant trust.

```sh
rimz trust status    # show the trust state and, when stale, a field-level diff
rimz trust grant     # pin the current executable surface as trusted
```

### Trust went stale after an edit

Editing any executable-surface field re-hashes the project config, and `rimz trust status` and `rimz doctor` report `stale` on the next read — no separate sweep. The command-running fields disable themselves until you re-grant. Review the diff that `rimz trust status` prints, then `rimz trust grant` again. The full trust model, including what counts as the executable surface, is in [security and trust](./security.md#project-trust).

## Remote sessions

### The link keeps dropping

A remote room is plain SSH under a supervisor that reconnects itself when the link drops, and the sidebar footer carries a `⇄ remote` badge that reads link health at a glance. A link that will not hold usually fails the underlying SSH prerequisites: confirm you can `ssh` to the target unattended (key-based auth, a reachable host), then reconnect. Saved aliases and the reconnect model are in [remote](./remote.md).

## Reset and clean up

### Start a room clean

To skip recovering prior agents when a room is reborn, pass the global `--no-resume`; the room comes up empty instead of seeding the fleet you left.

```sh
rimz --no-resume    # come up empty: skip recovering prior agents
```

### A room is wedged — `rimz reset`

`rimz reset` is the escape hatch for a room that is stuck, or that came back wrong after a reboot. It tears the room down, purges the cached session state a rebuild would reuse, archives records, sweeps orphaned processes, then rebuilds and reattaches by default.

```sh
rimz reset              # rebuild this workspace's room from clean state
rimz reset --yes        # skip the confirmation prompt (required off a TTY)
rimz reset --no-start   # tear down only, then print the rerun hint
rimz reset --hard       # also drop the prior-agent carryover, so rebirth seeds nothing
```

A plain `reset` keeps the agent carryover for history but still starts empty; `--hard` removes it too.

### Stale worktrees and runtime state — `rimz gc`

`rimz gc` sweeps stale runtime state: orphaned atomic-write temp files, dead workspace stores, abandoned queued messages, and clean Rimz-marked worktrees whose work has already landed with no live pane inside. It keeps anything dirty, pending, or unproven, and reports a checklist of what it cleaned, kept, and why.

```sh
rimz gc                    # sweep runtime state older than 24h (the default cutoff)
rimz gc --older-than 7d    # widen the cutoff
```

### Where state lives, and full removal

Per-machine config lives under `~/.config/rimz/` (`config.toml`, `theme.toml`, `agents.toml`, `loop.toml`, `remote.toml`), and durable room state lives under `~/.local/state/rimz/`. To remove Rimz from the machine, `rimz uninstall` takes out installed hooks, running rooms, runtime state, and the binaries it finds; durable stores and per-machine config stay unless you ask for them:

```sh
rimz uninstall            # hooks, rooms, runtime state, binaries; keeps stores and config
```

`--state`, `--config`, and `--all` widen the removal to durable stores and per-machine config; the exact scope of each flag is in the [maintenance reference](../reference/cli/maintenance.md#reload-reset-gc-and-uninstall). Project-local `.rimz/` directories and Rimz-owned worktrees stay in place, because they can hold project config and unlanded work.

## Filing an issue

Capture the state Rimz sees and attach it to the report:

```sh
rimz doctor --json --output rimz-doctor.json    # full environment report as JSON
```

`rimz doctor --json` is the single best artifact — it carries backend, versions, per-agent hook status, trust state, and the room health that most reports need.

## See also

- [Set up your machine](./setup.md) — the first-pass configuration these fixes assume.
- [Security and trust](./security.md) — the trust model and the presence grant behind two of the fixes above.
- [Remote](./remote.md) — reconnect behavior and link health.
- [CLI reference → maintenance](../reference/cli/maintenance.md) — every flag for `doctor`, `reset`, `gc`, `reload`, and `uninstall`.
- [Store internals](../internals/store.md) — what `reset` and `gc` touch on disk.
