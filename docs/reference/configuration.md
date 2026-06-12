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
| `~/.config/rimz/config.toml` | per-machine | worktree defaults, tab keywords and layouts, room options, sidebar display, notifications, remote-control auto-launch | you, `rimz setup`, `rimz config` |
| `~/.config/rimz/resolvers.toml` | per-machine | resolver allowlist and chain order | `rimz resolver` |
| `~/.config/rimz/remote.toml` | per-machine | named SSH room aliases | `rimz remote` |
| `~/.config/rimz/projects/<id>/trust.toml` | per-machine | project executable-surface trust grant | `rimz trust` |
| `<root>/.rimz/config.toml` | committed | declared workspace shape, trust-tracked today | humans and project automation |

Per-machine settings load leniently: a missing file is the default config, and unknown keys are ignored so an older binary can tolerate a newer file. `rimz config set` is stricter than the loader and rejects unknown dotted keys before it writes.

## `config.toml` Per Machine

Nine sections make up the per-machine file:

| Section | Purpose |
| --- | --- |
| `[worktree]` | where Rimz-owned Git worktrees live and which base ref new ones branch from |
| `[tab]` | tab keywords and named tab layouts for `rimz tab --layout` |
| `[remote_control]` | per-agent remote-control auto-launch opt-ins |
| `[accounts]` | provider account-usage enrichment and display-only monthly ceilings |
| `[notifications]` | best-effort desktop, bell, and command notifications |
| `[sidebar]` | sidebar width, render timing, ordering, card density, scroll, theme and glow, and display bands |
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

`command` runs locally through `sh -c` with `RIMZ_NOTIFY_TITLE`, `RIMZ_NOTIFY_BODY`, `RIMZ_NOTIFY_AGENT`, and `RIMZ_NOTIFY_KIND` in its environment. Use it for machine-local routing such as ntfy, Slack, Pushover, or an OS notifier. Mechanics live in [internals/sidebar/notifications.md](../internals/sidebar/notifications.md).

### Remote Control

```toml
[remote_control]
claude = false
codex = false
```

These are per-machine opt-ins for background remote-control infrastructure. `claude = true` launches `claude remote-control --spawn worktree` in the `rimzd` view when `claude` is on PATH. `codex = true` ensures the managed standalone Codex daemon (`$CODEX_HOME/packages/standalone/current/codex remote-control start`) before the room opens.

Configured hosts are fail-fast preconditions for `rimz start`. Claude refuses when its own settings or version make the host impossible: Claude Code older than 2.1.51, `disableRemoteControl: true`, `disableAgentView: true` on Claude Code 2.1.173 or newer, or API-key auth sources active on Claude Code 2.1.157 or newer. Codex refuses when the managed standalone install is missing. `rimz doctor` reports the same refusal text and fix before launch.

The sidebar's `⇅ rc` provider flag is broader than these auto-launch toggles: it also lights when a provider-owned pane-session setting enables remote control, such as Claude's `remoteControlAtStartup: true`. Reading provider settings is local enrichment and adds no project trust-hash field.

### Accounts

```toml
[accounts]
oauth_usage = true

[accounts.usage_limit_usd]
claude = 50.0
codex = 25.0
```

Account enrichment is local and best-effort. `oauth_usage = true` lets Rimz use provider account-usage surfaces reached from local OAuth credentials or the local Codex app-server; turning it off suppresses those provider-reported paid-usage queries and caches. It does not disable transcript-derived spending totals, and it never writes provider credential files. `RIMZ_OAUTH_USAGE_OFFLINE=1` disables the same fetches for one process tree.

`[accounts.usage_limit_usd]` sets display-only monthly USD ceilings by provider kind. A ceiling scales the provider dashboard's `ex` or `api` bar when the provider does not report a real cap; it is not a provider-enforced spending limit and does not stop agents. Leaving a provider unset means the paid/API row reads uncapped or unknown with `∞`.

### Multiplexer Room Options

Rimz applies room-scoped defaults when it creates or reattaches a session, so the room gets the mouse, clipboard, rich-key, and scrollback behavior agents need without editing your global Zellij or tmux files.

```toml
[zellij]
session_serialization = false
auto_layout = true
copy_clipboard = "system"

[tmux]
set_clipboard = "on"
extended_keys_format = "csi-u"
```

Zellij receives its settings as `zellij attach ... options ...` on room birth and attach, and Rimz adds locked mode so ordinary typing reaches the focused pane. tmux receives session, window, and server-scoped options as required by tmux itself; clipboard and rich-key handling are server-scoped in tmux. The backend mapping is in [internals/sidebar/multiplexers.md](../internals/sidebar/multiplexers.md).

### Resume On Rebirth

```toml
[resume]
on_rebirth = true
max = 8
```

When a session is reborn after a reboot, multiplexer crash, reset, or clean Rimz rebirth, Rimz re-seeds prior agents from the durable rollup. Each restored agent starts idle in its own pane, so no model work happens until you type. `on_rebirth = false` or `--no-resume` comes up empty for a fresh room, and `max` bounds how many agents one birth relaunches. Mechanics live in [internals/sidebar/sidebar.md](../internals/sidebar/sidebar.md#resume-on-rebirth).

### Worktrees

```toml
[worktree]
dir = "../{repo}-worktrees"
base = "fresh"
```

`rimz worktree`, `rimz tab --worktree`, and `rimz agents --worktree` use this section when creating Rimz-owned Git worktrees. Relative `dir` values resolve from the repository root, and `{repo}` expands to the root directory basename. `base = "head"` branches from local `HEAD`, `base = "fresh"` branches from `origin/HEAD`, and any other string is passed to Git as the base ref. Cleanup state lives in [internals/agents/worktrees.md](../internals/agents/worktrees.md).

### Tab Keywords And Layouts

```toml
[tab.keywords]
vim = "nvim -p"
htop = "htop"

[tab.keywords.claude-plan]
agent = "claude"
args = "--permission-mode plan"

[tab.keywords.codex-yolo]
agent = "codex"
mode = "yolo"
args = "--model gpt-5-codex -c model_reasoning_effort=high"

[tab.layouts]
review = "claude-plan,codex-yolo+vim"
debug = "pi,htop+term"
```

Named layouts feed `rimz tab --layout <name>`, and inline specs such as `rimz tab --layout "claude,codex+term"` use the same cell resolver. A layout is a shape string: commas split columns, plus signs stack rows in a column, and each cell is a keyword. Keywords resolve in this order: user entries in `[tab.keywords]`, built-in `term`, registered agent kinds, and adapter-supported virtual `<kind>-<mode>` agent variants such as `claude-auto`, `codex-ask`, or `codex-yolo`. Non-`ask` virtual modes exist only when that adapter contributes permission argv for the posture. The built-in `peer = "claude,codex"` exists even when unset, and `[tab.layouts.peer]` overrides it for this machine. Layout names that collide with a keyword, `term`, an agent kind, or a virtual mode name are reserved for inline single-cell specs.

A bare keyword string is shell-split as a raw command pane. A keyword table with `agent = "<kind>"` opens an agent cell; `mode = "auto" | "ask" | "yolo"` adds that adapter's permission argv first, then `args` is shell-split and appended. Launch posture and model flags live on keywords, so a named layout composes variants like `claude-plan` or `codex-yolo` directly.

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

Budget pace colors only the provider reset marker. `100` is even burn, where the used share matches the elapsed share of that window; thresholds apply above each bound, moving the marker from the countdown's soft tier through yellow, amber, and red while the bar keeps using the remaining-budget zones.

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

`[sidebar.theme]` picks the palette — built-in schemes, Ghostty theme adoption, color depth, and per-slot overrides — and `glow` gates transition flashes over that base render. The full theming surface, including `[sidebar.animations]` status heads and `[sidebar.providers]` brand styling, lives in [theme.md](./theme.md).

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
```

The dashboard shows one block per discovered provider. `provider_tabs = "auto"` stacks one or two providers and switches to tabs at three or more. `provider_list` chooses kinds and order; `"all"` expands to every remaining discovered provider at that position. Empty discovery uses today's spend to choose up to `max_provider_blocks`, then orders the retained providers stably by kind.

`[sidebar.providers.<kind>]` restyles a provider's display name, ASCII art, and brand color; the fields and formats live in [theme.md](./theme.md#provider-styling). Account and budget sourcing is in [internals/agents/provider.md](../internals/agents/provider.md).

## Changing Values

```sh
rimz config path
rimz config get
rimz config get sidebar.max_cols
rimz config get sidebar --json
rimz config set sidebar.max_cols 80
rimz config set sidebar.theme.scheme slate
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

The committed `<root>/.rimz/config.toml` declares the workspace shape a team wants to share. Rimz computes the executable-surface trust hash from it, and on a trusted workspace injects each `[[agents]]` `env` table into that agent's process at launch; launch-time application of the declared layout, hooks, agent `launch_command`, and top-level `[env]` is planned project-config behaviour.

```toml
[[layout.initial_panes]]
name = "shell"
command = "$SHELL"
cwd = "$RIMZ_PROJECT_ROOT"

[[agents]]
name = "claude"
launch_command = "claude"
env = { CLAUDE_CODE_DISABLE_AGENT_VIEW = "1" }

[[hooks]]
event = "PreToolUse"
command = "notify-send rimz"
```

Command-running fields enter the trust hash, so a clone with project config shows `untrusted` until `rimz trust grant` pins the current executable surface on this machine. Agents launch through your default shell so terminal env applies, then trusted project `[[agents]]` env and adapter pins outrank shell rc/profile values. An `untrusted` or `stale` workspace with `[[agents]]` `env` configured refuses the agent launch with the `rimz trust grant` fix. The hash contract and launch-time enforcement are in [internals/sidebar/trust.md](../internals/sidebar/trust.md); the threat model is in [security.md](../guide/security.md).

## Sidecars And Privacy

Resolver configuration lives with `rimz resolver` and the protocol details in [internals/agents/resolvers.md](../internals/agents/resolvers.md). Remote aliases live with `rimz remote` and are documented in [cli.md](./cli.md). Trust records live with `rimz trust` and [internals/sidebar/trust.md](../internals/sidebar/trust.md).

Payload-fidelity and retention controls are a planned project surface. The design and intended privacy keys live in [security.md](../guide/security.md), and the hook boundary they will govern is in [internals/agents/agent.md → The adapter boundary](../internals/agents/agent.md#the-adapter-boundary).
