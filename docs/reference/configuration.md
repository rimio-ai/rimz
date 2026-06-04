# Configuration

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

Rimz runs with no configuration. Everything here is optional tuning.

Two per-machine files configure how Rimz drives *your* box: `~/.config/rimz/config.toml` (room, sidebar, remote-control) and `~/.config/rimz/resolvers.toml` (the resolver chain). A project may also commit a `.rimz/config.toml`; today Rimz reads that file only to compute the project's trust hash — the workspace shape it declares is on the roadmap, and the last section explains exactly what is and isn't live.

## What configures Rimz today

| File | Scope | What it does today |
| --- | --- | --- |
| `~/.config/rimz/config.toml` | per-machine | room options, sidebar look, remote-control auto-launch |
| `~/.config/rimz/resolvers.toml` | per-machine | the resolver allowlist and chain order |
| `~/.config/rimz/projects/<id>/trust.toml` | per-machine | the project's trust grant — written by `rimz trust grant`, not by hand |
| `<root>/.rimz/config.toml` | committed | the declared workspace shape; trust-tracked, [not yet applied](#project-config) |

The per-machine tier is personal and never committed — a clone never inherits it, and none of it enters the trust hash. Settings load best-effort: a missing file is the default, and unknown keys are ignored so a newer file never breaks an older binary.

## Per-machine config — `~/.config/rimz/config.toml`

Five sections, each optional: `[remote_control]`, `[sidebar]`, `[zellij]`, `[tmux]`, `[resume]`.

### Remote control auto-launch

```toml
[remote_control]
claude = true          # off when unset
codex  = true          # off when unset
```

Each toggle is independent and off by default — Rimz never links an account or starts a remote-control host without opt-in. This tier is per-machine on purpose: remote control links *your* agent accounts and accepts remote spawn commands.

`claude = true` runs `claude remote-control` in the managed daemon tab when `claude` is on PATH (best-effort — a missing `claude` is skipped). `codex = true` ensures the per-user Codex remote-control daemon once per start. The Codex daemon boots from the managed standalone install at `$CODEX_HOME` (default `~/.codex`), not a distro `codex` on PATH; when `codex = true` and that install is absent, `rimz start` refuses up front with the fix, and `rimz doctor` reports it ahead of time. How each host links its account is in [internals/account.md](../internals/account.md); how enrichment connects is in [internals/transcript.md](../internals/transcript.md).

`rimz start` parks both hosts in one dedicated `rimzd` tab, focus returned to your working pane. Neither is a coding agent, so the sidebar filters both out of the room — Claude's link surfaces as a `⇅ rc` flag on its provider block instead ([interface/sidebar.md](../interface/sidebar.md)).

### Multiplexer room options

Rimz applies a small set of room defaults when it creates or reattaches a session, so the room has the mouse, clipboard, rich-key, and scrollback behaviour agents need without editing your global Zellij or tmux config.

```toml
[zellij]
mouse_mode = true
mouse_click_through = true
focus_follows_mouse = false
pane_frames = false
on_force_close = "detach"              # "detach" | "quit"
scroll_buffer_size = 100000
show_startup_tips = false
show_release_notes = false
copy_clipboard = "system"              # "system" | "primary"
copy_on_select = true
support_kitty_keyboard_protocol = true
osc8_hyperlinks = true
session_serialization = false          # Rimz owns rebirth; a resurrected room comes back suspended

[tmux]
mouse = true
focus_events = true
history_limit = 100000
allow_passthrough = true
set_clipboard = "on"                   # "on" | "external" | "off"
extended_keys = true
extended_keys_format = "csi-u"         # "csi-u" | "xterm"
escape_time_ms = 0
renumber_windows = true
aggressive_resize = true
pane_border_status = "off"             # "off" | "top" | "bottom"
pane_border_lines = "simple"           # "simple" | "single" | "double" | "heavy"
```

Zellij receives these as `zellij attach … options …` on session birth and attach, so they never touch `~/.config/zellij/config.kdl`. tmux applies them across the right scopes — session, window, and the few that are server-global (clipboard and rich-key handling have no per-session equivalent). The backend-by-backend mapping is in [internals/multiplexers.md](../internals/multiplexers.md).

### Resume on rebirth

```toml
[resume]
on_rebirth = true   # re-seed prior agents when a session is reborn (default true)
max = 8             # cap auto-resumed agents per birth (default 8); overflow is reported
```

When a session is reborn — reboot, multiplexer crash, or a Rimz-initiated rebirth of a stuck room — Rimz re-seeds the prior agents from the durable rollup, each restored idle in its own pane (`claude --resume`, `codex resume`), so the room comes up where you left off. `on_rebirth = false` (or `--no-resume` per invocation) comes up empty for a deliberately fresh start; `max` bounds how many agents one birth relaunches so a long-lived workspace never fork-bombs a fleet of processes. Mechanics in [internals/sidebar.md](../internals/sidebar.md#resume-on-rebirth).

### Sidebar appearance

Per-machine, display-only tuning of how the sidebar paints. None of it affects ledger correctness.

#### Attention escalation

```toml
[sidebar]
attention_redden_secs = 1800   # seconds before an unanswered ?/! reddens (default 1800 = 30 min)
```

A `waiting` `?` or `failed` `!` glyph rests bold yellow and reddens once the row has gone unanswered past this window. Lower it for a tighter SLA, raise it for long unattended work.

#### Pane width

```toml
[sidebar]
max_cols = 72     # column cap on the 30% sidebar split (default 72)
```

Every sidebar pane targets 30% of the view at the `max_cols` cap, on both backends — on an ultra-wide terminal 30% alone is a hundred-column sidebar. The launch path reads your terminal's width once and bakes the decision into the session's pane templates: capped at `max_cols` when 30% would exceed it, plain 30% otherwise. Every pane — session birth, every new tab or window, recovery — lands at its size the instant it exists; resize a pane afterwards and your width sticks, and the sidebar always renders at the pane's full width. (A launch outside a terminal falls back to the 30% split; how each backend spells the cap is in [internals/multiplexers.md](../internals/multiplexers.md).)

#### Provider dashboard

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

The dashboard pinned at the bottom of the sidebar carries one block per agent kind. `[sidebar.providers.<kind>]` overrides the built-in style — `<kind>` is `claude`, `codex`, `pi`, …; each field is optional and falls back to the shipped default, so you can recolour without restating the art. `max_provider_blocks` caps how many blocks render, ordered by spend. How each block's account, plan, and usage budgets are sourced is in [internals/account.md](../internals/account.md).

## Resolver allowlist — `~/.config/rimz/resolvers.toml`

The per-machine chain of resolvers allowed to answer ahead of you. `rimz resolver add` writes this file; you can also hand-edit it.

```toml
[[resolver]]
id = "opus-policy"
order = 10
budget_seconds = 30
binary = "/home/me/bin/opus-resolver"   # optional; pins the heartbeat's executable path
display_name = "Opus policy"            # optional

[[resolver]]
id = "slack-on-call"
order = 20
budget_seconds = 300
```

`order` sets the chain position (low → high); `budget_seconds` is the time each link holds a request before the chain advances. A resolver engages the bridge only when it is on this list *and* heartbeating freshly. The protocol, the heartbeat, and the chain are in [internals/resolvers.md](../internals/resolvers.md); the trust boundary is in [security.md](../guide/security.md).

## Project trust record — `~/.config/rimz/projects/<id>/trust.toml`

Written by `rimz trust grant`, not by hand. It pins the project's executable-surface hash so an edit to a command-running config field auto-revokes the grant. The four states and the hash contract are in [internals/trust.md](../internals/trust.md); the threat model is in [security.md](../guide/security.md).

## Merge order

Later layers win:

1. built-in defaults,
2. project config (`.rimz/config.toml`),
3. per-machine config (`~/.config/rimz/config.toml`),
4. CLI flags and `RIMZ_*` environment variables.

This is the designed model. Today only the per-machine layer and the trust hash are live; the project layer is parsed for the trust hash and otherwise not yet applied (next section).

## Project config

The committed `<root>/.rimz/config.toml`.

> **Declared and trust-tracked today; not yet applied.** Rimz reads this file only to compute the project's trust hash (see [internals/trust.md](../internals/trust.md)). Applying the workspace shape it declares — opening the layout, launching agents, running hooks and env, firing notifications — is on the [roadmap](../contributing/roadmap.md). Every section below carries this caveat.

Commit `.rimz/config.toml` so a team shares one declared workspace shape; per-machine settings stay in `~/.config/rimz/`. The command-running fields below enter the trust hash, so a clone shows `untrusted` until someone runs `rimz trust grant` — that gate works today even though the shape is not yet applied.

```toml
[[layout.initial_panes]]
name = "shell"
command = "$SHELL"
cwd = "$RIMZ_PROJECT_ROOT"
env = { EDITOR = "vim" }

[layout.tmux]
status_left = "session"
status_right = "time"
popup_command = "fzf-projects"

[layout.zellij]
plugin_command = "/opt/sidebar.wasm"

[[agents]]
name = "claude"
launch_command = "claude"
env = { CLAUDE_HOME = "/opt/claude" }

[[hooks]]
event = "PreToolUse"
command = "notify-send rimz"

[env]
RUST_LOG = "debug"

[notifications]
command = "notify-send rimz"
```

- `[layout]` — `initial_panes` (each with `name`/`command`/`cwd`/`env`) plus the backend-only `[layout.tmux]` and `[layout.zellij]` fragments.
- `[[agents]]` — `name` (a known agent), optional `launch_command`, and `env`.
- `[[hooks]]` — `event` → `command` pairs. This is a *declared, trust-hashed* project hook, distinct from `rimz hooks install`, which wires an agent's own native hooks and works today (see [cli.md](./cli.md) and [internals/hooks.md](../internals/hooks.md)).
- `[env]` — workspace-wide environment variables.
- `[notifications]` — a notification helper `command`.

Which of these fields the trust hash covers is in [internals/trust.md](../internals/trust.md).

## Privacy

Payload-fidelity and retention controls are a planned project surface — Rimz does not yet read a `[privacy]` section or redact hook payloads. The design and the intended `payload_mode` / `retention_days` keys live in [security.md](../guide/security.md); the gate point, once live, is the agent hook path in [internals/hooks.md](../internals/hooks.md).
