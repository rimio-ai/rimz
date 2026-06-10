# Configuration

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

Rimz runs with no configuration. Everything here is optional tuning.

Configuration has two tiers. The per-machine tier under `~/.config/rimz/` drives your terminal, accounts, notification routes, sidecars, and room preferences; it stays personal, uncommitted, and outside the project trust hash. The project tier at `<root>/.rimz/config.toml` declares a shared workspace shape; Rimz trust-tracks it today, and applying that shape is planned project-config behaviour.

## Get Started

```sh
rimz setup                         # detect this machine and offer a default config write
rimz setup --yes                   # non-interactive default config write; no hook or trust side effects
rimz config init --print           # print the commented field reference
rimz config init                   # write ~/.config/rimz/config.toml
```

Most users start with `rimz setup` or `rimz config init`, then edit only the few lines they need. The generated template is the exhaustive field reference: every persisted section and default scalar is shown as commented TOML. Leaving a line commented keeps following the defaults shipped by future Rimz versions; uncommenting makes it this machine's override.

## The Files

| File | Scope | What it does | Who writes it |
| --- | --- | --- | --- |
| `~/.config/rimz/config.toml` | per-machine | worktree defaults, agent layouts, room options, sidebar display, notifications, remote-control auto-launch | you, `rimz setup`, `rimz config` |
| `~/.config/rimz/resolvers.toml` | per-machine | resolver allowlist and chain order | `rimz resolver` |
| `~/.config/rimz/remote.toml` | per-machine | named SSH room aliases | `rimz remote` |
| `~/.config/rimz/projects/<id>/trust.toml` | per-machine | project executable-surface trust grant | `rimz trust` |
| `<root>/.rimz/config.toml` | committed | declared workspace shape, trust-tracked today | humans and project automation |

Per-machine settings load leniently: a missing file is the default config, and unknown keys are ignored so an older binary can tolerate a newer file. `rimz config set` is stricter than the loader and rejects unknown dotted keys before it writes.

## `config.toml` Per Machine

Eight sections make up the per-machine file:

| Section | Purpose |
| --- | --- |
| `[worktree]` | where Rimz-owned Git worktrees live and which base ref new ones branch from |
| `[agents.layouts]` | named tab layouts and per-agent launch flags for `rimz tab --layout` |
| `[remote_control]` | per-agent remote-control auto-launch opt-ins |
| `[notifications]` | best-effort desktop, bell, and command notifications |
| `[sidebar]` | sidebar width, render timing, ordering, card density, scroll, glow, and display bands |
| `[zellij]` | Rimz-owned Zellij room defaults |
| `[tmux]` | Rimz-owned tmux room defaults |
| `[resume]` | agent re-seeding policy when a room is reborn |

Every field, its default, and an inline note lives in the generated template:

```sh
rimz config init --print
```

The sections below explain the model and the knobs whose behavior is easy to misread.

### Notifications

```toml
[notifications]
triggers = ["waiting", "failed"]
desktop = "auto"
sound = "bell"
command = "ntfy publish rimz"
```

Notifications are best-effort attention delivery layered over the sidebar. `waiting`, `failed`, `paused`, and `success` transitions can notify; `running` and `idle` stay quiet. `debounce_ms` limits repeat notifications for the same agent, and `coalesce_ms` groups bursts into one banner.

`desktop = "auto"` emits terminal OSC notifications under tmux and skips them under Zellij, which drops notification OSCs today. `desktop = "osc"` forces emission for testing or future terminal paths. `sound = "bell"` writes a separate BEL byte and your local terminal decides whether that is audible.

`command` runs locally through `sh -c` with `RIMZ_NOTIFY_TITLE`, `RIMZ_NOTIFY_BODY`, `RIMZ_NOTIFY_AGENT`, and `RIMZ_NOTIFY_KIND` in its environment. Use it for machine-local routing such as ntfy, Slack, Pushover, or an OS notifier. Mechanics live in [internals/notifications.md](../internals/notifications.md).

### Multiplexer Room Options

Rimz applies room-scoped defaults when it creates or reattaches a session, so the room gets the mouse, clipboard, rich-key, and scrollback behavior agents need without editing your global Zellij or tmux files.

```toml
[zellij]
session_serialization = false
copy_clipboard = "system"

[tmux]
set_clipboard = "on"
extended_keys_format = "csi-u"
```

Zellij receives its settings as `zellij attach ... options ...` on room birth and attach, and Rimz adds locked mode so ordinary typing reaches the focused pane. tmux receives session, window, and server-scoped options as required by tmux itself; clipboard and rich-key handling are server-scoped in tmux. The backend mapping is in [internals/multiplexers.md](../internals/multiplexers.md).

### Resume On Rebirth

```toml
[resume]
on_rebirth = true
max = 8
```

When a session is reborn after a reboot, multiplexer crash, reset, or clean Rimz rebirth, Rimz re-seeds prior agents from the durable rollup. Each restored agent starts idle in its own pane, so no model work happens until you type. `on_rebirth = false` or `--no-resume` comes up empty for a fresh room, and `max` bounds how many agents one birth relaunches. Mechanics live in [internals/sidebar.md](../internals/sidebar.md#resume-on-rebirth).

### Worktrees

```toml
[worktree]
dir = "../{repo}-worktrees"
base = "fresh"
```

`rimz worktree`, `rimz tab --worktree`, and `rimz agents --worktree` use this section when creating Rimz-owned Git worktrees. Relative `dir` values resolve from the repository root, and `{repo}` expands to the root directory basename. `base = "head"` branches from local `HEAD`, `base = "fresh"` branches from `origin/HEAD`, and any other string is passed to Git as the base ref. Cleanup state lives in [internals/worktrees.md](../internals/worktrees.md).

### Agent Tab Layouts

```toml
[agents.layouts]
stacked = "claude,codex+term"

[agents.layouts.peer]
shape = "claude,codex"

[agents.layouts.peer.flags]
claude = "--permission-mode plan"
codex = "--model gpt-5-codex -c model_reasoning_effort=high"
```

Named layouts feed `rimz tab --layout <name>`. A layout is either a shape string or a table with `shape` plus `flags`. Shape strings use commas for columns, plus signs for stacked rows in a column, and registered agent kinds or `term` as cells. The built-in `peer = "claude,codex"` exists even when unset; defining `[agents.layouts.peer]` overrides the built-in for this machine, usually to attach flags. Layout names `term` and registered agent kinds are reserved for inline single-cell specs. Flags are shell-split, passed as direct argv to that agent kind, and apply to every matching cell when a shape repeats a kind. Inline CLI specs such as `rimz tab --layout "claude,codex+term"` stay shape-only.

### Sidebar Bands

```toml
[sidebar.context]
red = { percent = 95, tokens = 420000 }

[sidebar.budget]
red = 10

[sidebar.budget.pace]
yellow = 100
amber = 150
red = 200
```

The agent card context meter ramps by the worse of two axes: fill percentage and absolute tokens in the window. A large-window model can still warm by sheer token count even when its percentage looks calm.

The provider dashboard budget zones work in the opposite direction: they bound remaining budget from above. At or above `yellow` the bar stays green; below each threshold it moves through yellow, amber, and red. The template carries the shipped numbers.

Budget pace colors only the provider reset countdown. `100` is even burn, where the used share matches the elapsed share of that window; thresholds apply above each bound, moving the timer from blue through yellow, amber, and red while the bar keeps using the remaining-budget zones.

### Sidebar Rendering

```toml
[sidebar]
max_cols = 72
refresh_ms = 100
scrollbar = "auto"
glow = "auto"
card_density = "auto"
trunk = "develop"
```

`max_cols` caps the creation-time sidebar pane width so a percentage split does not swallow ultra-wide terminals. `refresh_ms` controls the renderer's animation grid, not the producer's data cadence. `scrollbar` controls only the right-margin overflow indicator.

`glow = "auto"` follows `COLORTERM` for the truecolor attention glow and transition flashes. `always` is useful when a real truecolor terminal under-advertises, such as an SSH hop that forwards `TERM` but drops `COLORTERM`; `never` keeps the plain 256-color render. `NO_COLOR` still disables color effects.

`card_density = "auto"` keeps the standard agent card: identity, description, context meter, context line, and subagents on the selected card. `expanded` shows every card's subagents. `compact` trims resting cards by status while the selected card opens to the standard card.

| status in `compact` | resting lines |
|---------------------|---------------|
| `idle` | identity |
| `running`, `waiting` | identity + description + context meter |
| `paused`, `success`, `failed` | identity + description |

`trunk` is a preferred comparison target for the worktree header's git stats. A repo where that branch does not resolve falls back to the detection ladder: `main`, then `master`, then the remote's advertised default.

### Provider Dashboard

```toml
[sidebar]
provider_tabs = "auto"
provider_list = ["codex", "all"]
max_provider_blocks = 3

[sidebar.providers.claude]
color = 173
```

The dashboard shows one block per discovered provider. `provider_tabs = "auto"` stacks one or two providers and switches to tabs at three or more. `provider_list` chooses kinds and order; `"all"` expands to every remaining discovered provider at that position. Empty discovery uses today's spend to choose up to `max_provider_blocks`, then orders the retained providers stably by kind.

`[sidebar.providers.<kind>]` overrides the built-in display name, ASCII art, or brand color for that provider. Each field is optional, so a color override can leave the shipped art intact. Account and budget sourcing is in [internals/account.md](../internals/account.md).

## Changing Values

```sh
rimz config path
rimz config get
rimz config get sidebar.max_cols
rimz config get sidebar --json
rimz config set sidebar.max_cols 80
rimz config set worktree.base fresh
rimz config set notifications.triggers '["waiting", "failed"]'
```

`rimz config get` loads the effective per-machine config over built-in defaults. `rimz config set` edits one key in `config.toml`, preserves comments through `toml_edit`, rejects unknown keys, deserializes the whole result as `MachineConfig`, then writes with Rimz's temp-file-plus-rename durability primitive.

Bare `config set` values become TOML values when they parse (`80`, `false`, arrays, inline tables); otherwise they become strings (`fresh`, `always`). For context bands, set the whole band as an inline table: `rimz config set sidebar.context.red '{ percent = 90, tokens = 400000 }'`.

## Merge Order

Later layers win:

1. built-in defaults,
2. project config (`.rimz/config.toml`),
3. per-machine config (`~/.config/rimz/config.toml`),
4. CLI flags and `RIMZ_*` environment variables.

This is the designed model. Today the per-machine layer is live, CLI/env overrides are applied by the commands that define them, and the project layer is read for the trust hash.

## Project Config

The committed `<root>/.rimz/config.toml` declares the workspace shape a team wants to share. Rimz reads it today to compute the executable-surface trust hash; launch-time application of the declared layout, agents, hooks, and env is planned project-config behaviour.

```toml
[[layout.initial_panes]]
name = "shell"
command = "$SHELL"
cwd = "$RIMZ_PROJECT_ROOT"

[[agents]]
name = "claude"
launch_command = "claude"

[[hooks]]
event = "PreToolUse"
command = "notify-send rimz"
```

Command-running fields enter the trust hash, so a clone with project config shows `untrusted` until `rimz trust grant` pins the current executable surface on this machine. The hash contract is in [internals/trust.md](../internals/trust.md); the threat model is in [security.md](../guide/security.md).

## Sidecars And Privacy

Resolver configuration lives with `rimz resolver` and the protocol details in [internals/resolvers.md](../internals/resolvers.md). Remote aliases live with `rimz remote` and are documented in [cli.md](./cli.md). Trust records live with `rimz trust` and [internals/trust.md](../internals/trust.md).

Payload-fidelity and retention controls are a planned project surface. The design and intended privacy keys live in [security.md](../guide/security.md), and the hook boundary they will govern is in [internals/hooks.md](../internals/hooks.md).
