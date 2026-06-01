# Configuration

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

Rimz has two config tiers — one committed with the project, one private per machine. Both are optional. The smallest useful project config is a few lines.

## Minimal example

```toml
# .rimz/config.toml
[workspace]
mux = "auto"               # auto | zellij | tmux

[layout]
sidebar = true
sidebar_width = 30              # percent of terminal width

[[layout.initial_panes]]
name = "shell"
command = "$SHELL"
cwd = "$RIMZ_PROJECT_ROOT"
```

That's enough for `rimz` in this repo to open the right multiplexer, create or find the project's session, launch the native sidebar pane, and drop you in a shell when the caller is interactive.

## What to commit, what to keep local

**Commit.** `.rimz/config.toml` — the team should see the same workspace shape on every checkout.

**Don't commit.** Anything under `~/.config/rimz/` — your sound profile, agent paths, multiplexer preference, and resolver allowlist are per-machine and personal.

## Project config — `<project_root>/.rimz/config.toml`

May define:

- workspace display defaults,
- multiplexer preference (`[workspace] mux`),
- layout IR,
- agent launch commands,
- environment overrides,
- notification defaults,
- privacy defaults.

Project config is inert until the workspace is trusted. See [security.md](../guide/security.md).

## Per-machine config — `~/.config/rimz/`

Personal, never committed. Two scopes:

```text
config.toml                         machine-wide preferences (see below)
resolvers.toml                      resolver allowlist and chain order
projects/<sha256(project_root)>/    per-project, per-machine state
  trust.toml                          executable-surface grant for this project
```

`config.toml` may define:

- remote-control auto-launch, per agent (`[remote_control] claude` / `codex`),
- Rimz room multiplexer options (`[zellij]`, `[tmux]`),
- sidebar row density (`[sidebar] density`),
- local workspace display name,
- sound profile,
- hook install state,
- local agent binary paths,
- per-machine mux preference (overrides project `auto`).

### Remote control auto-launch

```toml
[remote_control]
claude = true          # off when unset
codex  = true          # off when unset
```

Each toggle is independent. The two hosts launch differently, because their lifecycles differ:

- **Claude** runs `claude remote-control --spawn worktree`: a long-lived foreground host, gated on `claude` being on PATH (best-effort — a missing `claude` is simply skipped). It runs from the project root (the main checkout), so on-demand remote sessions get isolated worktrees off the canonical repo rather than the current worktree. It is a pane in the managed `rimzd` daemon tab (see below).
- **Codex** runs `remote-control start`, which brings up the Codex app-server daemon with remote control enabled, then returns. That daemon is a **per-user singleton** (keyed by one control socket), so `rimz start` does *not* park it in a per-workspace pane: it spawns the (idempotent) start command detached, with null stdio, once — no pane, no terminal output. `remote-control start` boots and updates the app-server from the *managed standalone install* at `$CODEX_HOME/packages/standalone/current/codex` (CODEX_HOME defaults to `~/.codex`), so a distro `codex` on PATH (e.g. `/usr/bin/codex`) is a different binary and does not satisfy it. **Fail-fast:** when `codex = true` but that install is absent, `rimz start` refuses up front rather than ensuring a daemon that only errors — install it with `curl -fsSL https://chatgpt.com/codex/install.sh | sh`, then re-run (or set `codex = false`). `rimz doctor` reports the same gap and fix ahead of time.

This tier is per-machine on purpose: remote control links *your* agent accounts and accepts remote spawn commands, so a clone never inherits it and it never enters the project trust hash.

### Multiplexer room options

Rimz applies a small set of per-machine multiplexer defaults when it creates or reattaches a room, so the room has the mouse, clipboard, rich-key, and scrollback behaviour agents need without editing your global Zellij/tmux config files.

```toml
[zellij]
mouse_mode = true
mouse_click_through = true
focus_follows_mouse = true
pane_frames = false
on_force_close = "detach"              # "detach" | "quit"
scroll_buffer_size = 100000
show_startup_tips = false
show_release_notes = false
copy_clipboard = "system"              # "system" | "primary"
copy_on_select = true
support_kitty_keyboard_protocol = true
osc8_hyperlinks = true

[tmux]
mouse = true
focus_events = true
history_limit = 100000
allow_passthrough = true
set_clipboard = "on"                   # "on" | "external" | "off"
extended_keys = true
extended_keys_format = "csi-u"          # "csi-u" | "xterm"
escape_time_ms = 0
renumber_windows = true
aggressive_resize = true
pane_border_status = "off"              # "off" | "top" | "bottom"
pane_border_lines = "simple"            # "simple" | "single" | "double" | "heavy"
```

Zellij receives these as `zellij attach … options …` on both session birth and attach. `copy_clipboard = "system"` uses Zellij's OSC52 path unless your Zellij config sets `copy_command`, which deliberately replaces OSC52 with that command.

tmux splits the same idea across scopes. `mouse`, `history_limit`, and `renumber_windows` are set on the Rimz session; `allow_passthrough`, `aggressive_resize`, and pane border options are set on the Rimz session's window options; `focus_events`, `set_clipboard`, `extended_keys`, `extended_keys_format`, and `escape_time_ms` are tmux server options because tmux has no per-session equivalent for clipboard and rich-key handling.

### The `rimzd` daemon tab

`rimz start` parks its managed panes in one dedicated view — the `rimzd` tab (a Zellij tab / tmux window), born `sidebar | hosts…` and **forced to the first position**, with focus returned to your working pane so an attach never strands you on it. It hosts:

- the **Claude remote-control host** (above), when enabled; and
- the **Codex app-server broker** — `rimz codex app-server serve`, a *local read-only enrichment* host, **not** the account-linking remote-control feature. It holds one warm `codex app-server` and serves it over a per-session unix socket so Codex enrichment (`rimz codex refresh-context`) skips the cold-spawn handshake. Because it links no account and only reads (rate-limit windows, model name), it is **ungated** — it runs whenever `codex` is on PATH and simply does not appear when `codex` is absent. Enrichment prefers this broker, then the per-user remote-control daemon's control socket (`codex app-server proxy`, overridable by `RIMZ_CODEX_APP_SERVER_SOCK`; empty drops it), then a cold-spawn — so it never depends on either being up.

Neither host is a coding agent, so the sidebar filters both out of the room entirely: Claude's remote-control link surfaces as a `⇅ rc` flag on the Claude block of the [provider dashboard](#sidebar-provider-dashboard) instead, and the broker — local enrichment infrastructure — is simply never shown as an agent.

### Sidebar row density

```toml
[sidebar]
density = "compact"    # "compact" (default) | "full"
```

How much of each agent card the sidebar renders by default (unselected). `compact` shows identity, description, and the context bar; `full` adds the token line and the time/lines-worked line too. Selecting a row always reveals the full card, so density only sets the resting height — a denser default trades on-screen agent count for detail at a glance. (The pre-1.0 `"bars"` level is gone now that the budgets are account-scoped and live in the provider dashboard, not on rows; it deserializes as `"full"`.) Display-only and per-machine: it never affects ledger correctness and a clone does not inherit it.

### Sidebar attention escalation

```toml
[sidebar]
attention_redden_secs = 1800   # seconds before an unanswered ?/! reddens (default 1800 = 30 min)
```

A `waiting` `?` or `failed` `!` glyph rests bold **yellow** ("a human is needed here") and reddens to bold **red** once the row has gone unanswered past this window — so a fresh ask reads calm-urgent and a long-ignored one visibly heats up. The same threshold reddens the cockpit's `?`/`!` buckets when any of their rows is stale. Lower it for a tighter SLA, raise it for long-running unattended work. Display-only and per-machine; it tunes the colour ramp, never the ledger.

### Sidebar provider dashboard

```toml
[sidebar]
max_provider_blocks = 3        # cap the dashboard at N provider blocks (default 3)

[sidebar.providers.claude]
product_name = "Claude Code"   # header label (default per kind)
color = 173                    # 256-colour index for the brand emblem
ascii_art = """
 ▐▛███▜▌
▝▜█████▛▘
  ▘▘ ▝▝
"""
```

The pinned dashboard at the bottom of the sidebar carries one block per agent kind — active or merely logged in, so an account you are signed into still shows its budgets between turns: a brand emblem, the plan and version, aggregate spend/tokens, and the account-scoped 5-hour / 7-day budgets as draining segmented `▰`/`▱` "mana" bars (an unmetered API-key account shows an `∞` bar with no countdown). `[sidebar.providers.<kind>]` overrides the built-in style per kind — `<kind>` is the agent kind (`claude`, `codex`, `pi`, …). Each field is optional and falls back to the built-in default: `claude` clay (173), `codex` blue (32), `pi` Inflection forest green (28); an unknown kind gets a neutral grey and no emblem. `max_provider_blocks` caps how many blocks render (ordered by spend); raise it if you routinely run more than three providers. Display-only and per-machine. How each block's account, plan, and budgets are sourced and aggregated is [internals/account.md](../internals/account.md).

## Merge order

Later layers win:

1. built-in defaults,
2. project-local config (`.rimz/config.toml`),
3. per-machine config (`~/.config/rimz/config.toml`),
4. CLI flags and `RIMZ_*` environment variables.

The project layer sets the shared defaults every contributor sees. The per-machine layer mutes notifications, swaps sound profiles, overrides agent paths, or disables specific hooks without leaking those choices back into the repo. CLI flags win for one-off use.

## Layout IR

`[layout]` is multiplexer-neutral. Backend adapters compile it to Zellij or tmux command sequences at session start. v0 supports the intersection: session, views (Zellij tabs / tmux windows), panes, split direction, pane size, cwd, command, env, pane/view naming. By default the sidebar is a native pane — no plugin install — launched at session start on both backends. Zellij users can opt in to a docked plugin rail instead (`[layout.zellij]` below); the native pane stays the fallback.

```toml
[layout]
sidebar = true
sidebar_width = 30              # percent of terminal width
default_view = "main"

[[layout.initial_panes]]
name = "shell"
command = "$SHELL"
cwd = "$RIMZ_PROJECT_ROOT"

# Backend-only extras, ignored when the other backend is selected.
[layout.zellij]
sidebar_plugin = false   # opt in to the docked plugin rail instead of a native pane
# Floating/pinned panes, KDL fragments, plugin panes other than the sidebar.

[layout.tmux]
# Popup bindings, status-line snippets.
```

Backend-only entries that execute shell commands enter the trust hash.

## Privacy

```toml
[privacy]
retention_days    = 14
payload_mode      = "redacted"   # metadata | redacted | full
max_payload_bytes = 8192
```

- `metadata` — strips tool inputs, prompts, command arguments, and error text.
- `redacted` — keeps bounded payloads with built-in redaction. Default.
- `full` — keeps hook payloads as delivered. `rimz doctor` warns.
