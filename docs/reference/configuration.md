# Configuration

> See [DESIGN.md](../../DESIGN.md#commitments) for the commitments this doc operationalizes.

Rimz runs with zero configuration. Everything here is optional tuning — start the room, and you can come back to add a theme, a launch profile, or a notification route once you know what you want to change.

Configuration comes in two tiers. **Per-machine** config under `~/.config/rimz/` is yours: your terminal, accounts, notifications, theme, and launch shortcuts. It stays personal, uncommitted, and outside the project trust hash. **Project** config at `<root>/.rimz/config.toml` declares a shape a team shares through the repo; Rimz trust-tracks it so a clone can review the executable surface before it runs.

## The files

| File | Tier | What it holds |
| --- | --- | --- |
| `~/.config/rimz/config.toml` | per-machine | room behavior: accounts, notifications, remote-control launch, multiplexer defaults, resume, smart-compact, Sentry |
| `~/.config/rimz/theme.toml` | per-machine | sidebar appearance: palette, slots, glyphs, animations, provider styling ([theme.md](./theme.md)) |
| `~/.config/rimz/agents.toml` | per-machine | agent profiles, command cells, teams, worktree defaults, loop tasks, attention timing, pets |
| `~/.agents/agents/<name>/agent.toml`, `~/.agents/teams/<name>/team.toml` | per-machine | drop-in profile and team fragments merged under `agents.toml` |
| `~/.config/rimz/resolvers.toml` | per-machine | resolver allowlist and chain order (`rimz resolver`) |
| `~/.config/rimz/remote.toml` | per-machine | named SSH room aliases (`rimz remote`) |
| `~/.config/rimz/projects/<id>/trust.toml` | per-machine | project executable-surface trust grant (`rimz trust`) |
| `<root>/.rimz/config.toml` | committed | declared workspace shape, trust-tracked |
| `<root>/.worktreeinclude` | committed | globs for untracked files to seed into new worktrees |
| `<root>/.worktreelink` | committed | directories to symlink-share into new worktrees |

Per-machine settings load leniently: a missing file is the default config, unknown keys are ignored so an older binary tolerates a newer file, and a file Rimz cannot parse falls back to built-in defaults with a startup warning, so a broken config never blocks the room. `rimz config` and `rimz doctor` report the precise error and the fix.

## Get started

```sh
rimz setup                 # detect this machine and offer to keep and refresh config
rimz config init           # write config.toml, theme.toml, and agents.toml
rimz config init --print   # print the commented templates without writing
```

Most people run `rimz setup` or `rimz config init` once, then edit the few lines they care about. Setup keeps an existing config, refreshes it against the current templates, and reports any keys it skips.

**The generated template is the field reference.** Every persisted section and default scalar ships as commented TOML with an inline note, so `rimz config init --print` is the authoritative, always-current list of keys and defaults. This page explains the *model and the knobs that are easy to misread*, and leaves the full field list to the template. Leaving a line commented keeps following the defaults shipped by future Rimz versions; uncommenting makes it this machine's override.

The three per-machine files map to the in-memory config the same way: core behavior from `config.toml`, appearance from `theme.toml`, agent behavior from `agents.toml`. `rimz config set` routes a dotted key to the file that owns it.

## Read and change values

```sh
rimz config path                                   # the resolved config.toml path
rimz config get                                    # the whole effective config as TOML
rimz config get theme.display.max_cols             # one value
rimz config get sidebar --json
rimz config set theme.display.max_cols 80
rimz config set theme "TokyoNight Night"
rimz config set agents.worktree.base fresh
rimz config set notifications.triggers '["waiting", "failed"]'
```

`get` loads the effective config (your overrides over built-in defaults). `set` edits one dotted key in the owning file, preserves comments through `toml_edit`, rejects unknown keys, re-validates the whole result, then writes with Rimz's temp-file-plus-rename durability. A bare value becomes a TOML value when it parses (`80`, `false`, arrays, inline tables) and a string otherwise (`fresh`, `always`); set a whole color band as an inline table, for example `rimz config set theme.display.context_meter.red '{ percent = 90, tokens = 400000 }'`. `theme.colors.*` keys write to root `[colors.*]` in `theme.toml`, so Alacritty palettes stay paste-compatible.

## How settings combine

Later layers win:

1. built-in defaults,
2. project config (`.rimz/config.toml`),
3. per-machine config (`~/.config/rimz/{config,theme,agents}.toml`),
4. CLI flags and `RIMZ_*` environment variables.

Today the per-machine layer is live, CLI/env overrides apply where the commands define them, and the project layer is read for trust. **Launch names invert this on purpose:** trusted project `[profiles]` and `[agents.teams]` overlay machine config and win on a name collision, so a repository can pin the launch surface it hashes (see [Project config](#project-config)).

Rimz also discovers drop-in fragments under `~/.agents/agents/<name>/agent.toml` and `~/.agents/teams/<name>/team.toml`, in the same `[agents.profiles]` / `[agents.teams]` shape as `agents.toml`; an entry in `agents.toml` overrides a fragment of the same name. Set `RIMZ_AGENTS_HOME` to relocate the fragment root.

## Agent profiles, commands, and teams

This section configures what `rimz agents <spec>` can launch: reusable agent **profiles**, raw **command** panes, and named **teams**. They live in `agents.toml` under `[agents.profiles]`, `[agents.commands]`, and `[agents.teams]`.

```toml
[agents]
placement = "auto"

[agents.profiles.claude-slim]
agent = "claude"                                       # a built-in kind, or another profile
effort = "low"
system-prompt-file = "~/.config/rimz/prompts/slim.md"

[agents.profiles.planner]
agent = "claude-slim"                                  # inherit the slim profile, change the voice
system-prompt-file = "~/.config/rimz/prompts/planner.md"

[agents.profiles.codex-yolo]
agent = "codex"
mode = "yolo"
model = "gpt-5-codex"
effort = "high"

[agents.commands]
vim = "nvim -p"

[agents.teams.review]
layout = "planner+reviewer,coder+term"

[[agents.teams.review.roles]]
role = "planner"
profile = "planner"

[[agents.teams.review.roles]]
role = "coder"
profile = "codex-yolo"

[[agents.teams.review.roles]]
role = "reviewer"
profile = "planner"
mode = "plan"
```

### Profiles

A profile is a named agent preset, addressable as `@<name>` once it is running. `agent` is its base — a built-in kind (`claude`, `codex`, …) or another profile that resolves to one — and the remaining **override fields** layer on top: `mode` (`auto` | `ask` | `plan` | `yolo`), `model`, `effort`, `system-prompt-file`, `append-system-prompt-file`, and raw `args`. These same override fields recur wherever you preset an agent — profiles, team roles, and loop tasks — so the template and `rimz config get` carry the current per-field defaults.

Inheritance flattens at launch to one concrete adapter kind, and **the nearest set value wins for every field, including `args`** — a child that sets `args` replaces the base `args` rather than appending. `system-prompt-file` gives the profile its own voice; `append-system-prompt-file` keeps the adapter's base prompt and adds rules where the adapter supports it. A `~` expands to home and a relative path roots at the config file, so a prompt file points at the same file wherever the profile launches; each file must exist at launch, and a missing one fails with the path to fix. A field the resolved adapter has no flag for fails the launch and names the field to remove. Command-line `--model`, `--effort`, `--system-prompt-file`, and `--append-system-prompt-file` render after the profile and override it for that launch.

A profile may be named like a kind: `[agents.profiles.claude]` overrides the base for bare `claude`, for profiles that set `agent = "claude"`, and for virtual cells like `claude-auto` and `claude-ping`.

### Commands

`[agents.commands]` entries are bare strings, shell-split and run as raw command panes. They are launch shortcuts, not agents, so they take no profile fields and answer to no `@` handle. A command may shadow a cell word like `claude` to set a local default for that word.

### Teams

A team is an ordered `roles` list that feeds `rimz agents <name>`; each role binds a role name to a profile and may set any of the same **override fields** (replacing, like profiles). Each member answers to `@<role>` in that channel. `rimz agents <team>.<role>` launches one declared role with the same identity it has inside the full team. By default the roles open left to right as one side-by-side column per role in one tab; an optional `layout` uses the inline shape grammar (comma = column, plus = row), resolving declared role names first and then falling back to roleless cells. The built-in `peer` team is the roleless `claude,codex`.

### Inline specs and cell resolution

An inline spec like `rimz agents "claude,codex+term"` keeps the same shape grammar: commas split columns, plus signs stack rows. Each cell resolves in this order:

1. `[agents.commands]`,
2. `[agents.profiles]`,
3. built-in `term`,
4. registered agent kinds,
5. adapter-supported virtual `<kind>-<mode>` and `<kind>-ping` cells (`claude-auto`, `codex-ask`, `codex-yolo`, `claude-ping`, …).

Profiles and roles become addressable handles, so they must not shadow `@all`, agent kinds (`@claude`), kind ordinals (`@claude-2`), or the pane/channel sigils (`:`, `#`). Profile, command, and team names also reserve the `agents` subcommand verbs `list`, `ls`, `show`, `stop`, `focus`, `wait`, `term`, and `exec`. A config that still uses a removed table fails fast naming the rename rather than silently dropping it: `[tab]` (with its `[tab.keywords]`/`[tab.layouts]` children) → `placement` under `[agents]` plus `[agents.teams]`; `[agents.aliases]` → `[agents.profiles]` and `[agents.commands]`; `[agents.layouts]` → `[agents.teams]`. The room degrades to defaults with a warning while `rimz config` and `rimz doctor` print the precise rename.

### Placement

```toml
[agents]
placement = "auto"   # "auto" | "pane" | "tab"
```

`placement` sets where a launch lands when neither `--new-pane` nor `--new-tab` is passed. `auto` (the default) runs a single non-worktree agent in the current pane and opens a new tab for a worktree launch, team, or multi-cell layout; `pane` splits a new pane for a single non-worktree agent and otherwise opens a tab; `tab` always opens a tab. The CLI side of placement is in [agents.md → Worktree and placement](./cli/agents.md#worktree-and-placement).

## Worktrees

```toml
[agents.worktree]
dir = "../{repo}-worktrees"
base = "fresh"
```

`rimz worktree` and `rimz agents --worktree` create Rimz-owned Git worktrees here. A relative `dir` resolves from the repository root and `{repo}` expands to the root basename. `base = "head"` branches from local `HEAD`, `base = "fresh"` branches from `origin/HEAD`, and any other string is passed to Git as the base ref. A committed `<root>/.worktreeinclude` lists globs for untracked files to copy into each new worktree, and `<root>/.worktreelink` lists directories to symlink-share. The seeding, symlink, and cleanup mechanics are in [worktree.md](../internals/agents/worktree.md).

## Loop tasks

```toml
[agents.loop.tasks.morning]
spec = "claude-ping"     # `<kind>-ping` primes a provider window
root = "/home/you/code/app"
at = "07:00"             # 24h local wall-clock
days = "weekdays"        # daily | weekdays | weekends | mon,wed,fri

[agents.loop.tasks.pr_watch]
spec = "codex"
prompt = "check CI on the release PR"
root = "/home/you/code/app"
every = "15m"
mode = "auto"

[agents.loop.tasks.self_wake]
bind = { kind = "claude", session = "sess-abc123", handle = "@planner" }
prompt = "resume the review: inspect the latest comments and fix the next blocking item"
root = "/home/you/code/app"
at = "09:30"
```

Each task chooses either `spec` or `bind`. `spec` drives one supervised turn for a single agent spec on a calendar, interval, cron, or one-shot schedule. A `<kind>-ping` spec is the window-primer: it defaults the prompt to `ping` and skips when that provider's budget window is already counting down. Bind-mode pins delivery to one live agent session and sends the prompt through the queue path; `kind` supports hook preflight, `session` is the durable target, and `handle` is display-only. The config records the intent; `rimz loop install` applies it to this machine's OS scheduler after a consent preview. Each task carries an absolute `root` so the scheduler knows which room hosts the turn. The full model is in [loop.md](../internals/agents/loop.md), and the CLI is in [agents.md → Schedule turns with loop](./cli/agents.md#schedule-turns-with-loop).

## Behavior settings

These tune how the room behaves. Each shows the shape; the template carries every key and default.

### Notifications

```toml
[notifications]
triggers = ["waiting", "failed"]
desktop = "auto"
sound = "bell"
remind_secs = 60
command = "ntfy publish rimz"
```

Notifications are best-effort attention delivery over the sidebar inbox. `waiting`, `failed`, `paused`, and `success` rows become unread until read; `triggers` filters which newly-unread kinds raise a banner or command, while `running` and `idle` stay quiet. `desktop = "auto"` emits terminal OSC notifications under tmux and skips them under Zellij (which drops notification OSCs today); `sound = "bell"` writes a BEL byte and your terminal decides whether it is audible. `command` runs locally through `sh -c` with `RIMZ_NOTIFY_TITLE`, `RIMZ_NOTIFY_BODY`, `RIMZ_NOTIFY_AGENT`, and `RIMZ_NOTIFY_KIND` in the environment (reminders also get `RIMZ_NOTIFY_UNREAD`) — wire it to ntfy, Slack, Pushover, or an OS notifier. The full debounce/coalesce/remind model is in [notifications.md](../internals/sidebar/notifications.md).

### Remote control

```toml
[remote_control]
claude = false
codex = false
```

These opt this machine into background remote-control infrastructure shown in the `rimzd` daemon view. `claude = true` adds `claude remote-control` to the daemon column when `claude` is on PATH; `codex = true` ensures the managed standalone Codex daemon before the room opens (a `codex` CLI on PATH already adds the per-session app-server broker). An enabled host is a fail-fast precondition for `rimz start`: Claude refuses on an incompatible version or settings, Codex refuses when the managed install is missing, and `rimz doctor` prints the same refusal and fix. The mechanics are in [provider.md](../internals/agents/provider.md) and the security boundary in [security.md](../guide/security.md).

### Accounts

```toml
[accounts.usage_limit_usd]
claude = 50.0
codex = 25.0
```

`usage_limit_usd` sets display-only monthly USD ceilings per provider kind: a ceiling scales the provider dashboard's `ex`/`api` bar when the provider reports no real cap. It tunes the bar only — the provider still enforces real spend and agents keep running — and an unset provider reads uncapped with `∞`. Account enrichment is local, read-only, and best-effort — `RIMZ_OAUTH_USAGE_OFFLINE=1` disables the live fetches for one process tree without touching transcript-derived totals or credential files.

### Resume

```toml
[resume]
on_rebirth = true
max = 8
auto_continue = false
auto_continue_backoff_secs = [60, 120, 180]
auto_continue_max_retries = 10
auto_continue_text = "continue"
```

Resume covers two tenses. On a **rebirth** (reboot, multiplexer crash, or a clean Rimz rebirth of a stuck room), Rimz offers to recover prior agents from the durable rollup — the prompt defaults yes, non-interactive starts recover, and each restored agent starts idle in its worktree tab. `on_rebirth = false`, `--no-resume`, and `rimz reset` come up empty; `max` bounds how many agents one birth relaunches. While the room is **live**, `auto_continue` picks any parked turn back up by typing `auto_continue_text` through the same path as `steer`: rate-limit parks fire when the spent window resets, while overload and transient server-error parks fire on the bounded retry ramp (`auto_continue_backoff_secs`, `auto_continue_max_retries`). It is off by default. The rebirth path is in [sidebar.md](../internals/sidebar/sidebar.md#resume-on-rebirth) and the live path in [provider.md](../internals/agents/provider.md#spent-windows-and-paused-rows).

### Smart compaction

```toml
[harness]
smart_compact = "70%"
```

`smart_compact` sets the default threshold for compact-first `steer` and `queue` sends — a percentage (`"70%"`) or an occupied-token count (`"120000"`). When an agent's context window has reached the threshold, Rimz submits its `/compact` ahead of your text so the prompt lands against a fresh window. Leave it unset to keep compaction opt-in through the per-command `--smart-compact` flag, which overrides this value. The mechanics are in [harness.md](../internals/agents/harness.md#compact-before-sending).

### rtk output compression

```toml
[harness]
rtk = "auto"
```

`rtk` controls output compression for Rimz-launched agents that run `cargo xtask`; direct human `cargo xtask` runs stay on plain cargo. `auto` wraps recognized cargo subcommands (`build`, `check`, `test`, `nextest`, `clippy`) through `rtk` when the binary is on the agent's `PATH`; `on` forces the wrapper and prints one warning before plain cargo when `rtk` is missing; `off` keeps cargo unwrapped. Install `rtk` on the machine for compression to take effect.

### Off-box error reporting

```toml
[sentry]
dsn         = "https://examplePublicKey@o0.ingest.sentry.io/0"
environment = "production"
```

Set a `dsn` to report Rimz `warn!`/`error!` events and observed agent rate-limit/overload conditions to a Sentry project. With no `dsn`, reporting stays off and Rimz makes no network calls. `RIMZ_SENTRY_DSN` and `RIMZ_SENTRY_ENVIRONMENT` override the config for one invocation, and `environment` defaults by build profile (an installed release reports as `production`, a dev or CI build as `development`). The DSN lives per-machine — never in committed project config — so a clone never inherits it; events carry low-cardinality tags (workspace, command, build, fault class, and agent/session when known) with the hostname and personal data withheld. The full telemetry surface is in [security.md](../guide/security.md#off-box-error-reporting) and the mechanics in [observability.md](../internals/health/observability.md).

## Multiplexer room options

Rimz applies room-scoped multiplexer settings when it creates or reattaches a session, so the room behaves the way agents need without editing your global Zellij or tmux config. The `[zellij]` and `[tmux]` tables tune those settings; `rimz config init --print` lists every key with its default, and the per-backend mapping is in [multiplexers.md](../internals/sidebar/multiplexers.md).

The two backends differ in how a key takes effect:

- **`[zellij]`** carries a few invariants Rimz always applies — locked mode, click-through, focus-follows-mouse, no session serialization (Rimz owns rebirth), and auto-layout — plus optional keys (`pane_frames`, `copy_clipboard`, …) that apply only when you set them and otherwise fall through to your `~/.config/zellij/config.kdl`. The sidebar pane is always borderless so its hit-testing stays stable regardless of `pane_frames`.
- **`[tmux]`** applies its room invariants on every birth, each key carrying a Rimz default you can override. The pane-border keys are optional overrides; unset, they fall through to your `~/.tmux.conf` or tmux defaults just like `pane_frames`. Setting `pane_border_status` makes Rimz own `pane-border-format` too, blanking the sidebar border row and overriding any `~/.tmux.conf` format; unset, your tmux config wins and may title the sidebar. The table spans session, window, and server scope, including clipboard and rich-key handling, because tmux has no per-session form for those.

```toml
[zellij]
pane_frames = true          # an optional override; unset, your config.kdl wins

[tmux]
## pane_border_status = "top"  # optional override; unset, your ~/.tmux.conf wins
```

To configure your *own* Zellij or tmux — the theme, true color, copy-mode, and keybindings Rimz leaves to you, and your sessions outside the room — see the [Zellij](../guide/zellij.md) and [tmux](../guide/tmux.md) setup guides.

## Appearance and the sidebar

The sidebar's palette, glyphs, animations, color depth, and color stops are theme settings in `theme.toml`, documented in full in [theme.md](./theme.md). The settings below are the sidebar's *behavior* knobs — they live in `config.toml` and `agents.toml`.

### Sidebar Rendering

```toml
[sidebar]
focus_key = "Alt+p"
trunk = "develop"
spend_window = "24h"
spend_timezone = "America/New_York"

[agents.attention]
stalled_after_secs = 1800
inactive_after_secs = 3600
```

`focus_key` is the global multiplexer chord that focuses the sidebar from any pane and toggles back to your last working pane; both backends bind it at session birth, the default is `Alt+p`, and `""` or `off` registers nothing. `trunk` is a preferred comparison branch for the worktree header's git stats, falling back to `main` → `master` → the remote default when it does not resolve. `[agents.attention]` tunes attention timing: `stalled_after_secs` is when a silent running agent escalates to the actionable `!` bucket (30 minutes by default), and `inactive_after_secs` is when an idle card sinks below live work (one hour — the prompt-cache boundary, so a cold card reads as cold). The `[theme.display]` knobs that share this area — render cadence, sizing, `scrollbar`, and `card_density` — are theme settings; see [theme.md → Display](./theme.md#display).

`spend_window` sets the cockpit and provider headline row: `"24h"` keeps the trailing-24-hour default, `"today"` starts at local calendar midnight, and `"session"` starts at the latest activity burst after a five-hour idle gap. `spend_timezone` is an optional IANA zone for `"today"`; unset uses the system local zone.

### Pets

```toml
[agents.pets]
enabled = false
pet = "codex"
size = "medium"
```

An opt-in animated companion in the provider dashboard. `pet` selects a source in priority order: a built-in catalog id (`codex`, `dewey`, `fireball`, `rocky`, `seedy`, `stacky`, `bsod`, `null-signal`); an `https://` URL to your own WebP spritesheet; a path-like value (containing `/`, `.`, or a leading `~`) for a local sheet or petdex directory; or a bare slug for a petdex pet under `~/.codex/pets/<slug>/`. `size`, `glyphs`, and `voice` tune the footprint, cell-art tier, and caption line. A built-in or URL sheet is fetched once into the per-machine cache (`RIMZ_PETS_OFFLINE=1` uses the cache only); pets run no commands and stay outside the trust hash. The geometry and cache contracts are in [pets.md](../internals/sidebar/pets.md).

### Sidebar Bands

The agent-card context meter and the provider budget bar interpolate across color stops you can tune. Both are theme settings (`[theme.display.context_meter]`, `[theme.display.budget_bar]`); the model and the shipped numbers are in [theme.md → Display](./theme.md#display).

### Provider Dashboard

Which providers appear, their order, and their brand styling are theme and discovery settings (`[theme.display] provider_tabs` / `provider_list` / `max_provider_blocks`, and `[theme.providers.<kind>]`). The layout model is in [theme.md → Display](./theme.md#display) and the styling fields in [theme.md → Provider styling](./theme.md#provider-styling); account and budget sourcing is in [provider.md](../internals/agents/provider.md).

## Project config

The committed `<root>/.rimz/config.toml` declares the workspace shape a team shares. Rimz computes the executable-surface trust hash from it, and on a trusted workspace it injects each `[[agents]]` `env` table into that agent's process at launch and applies top-level `[profiles]` and `[agents.teams]` to `rimz agents` launches. Use one `agents` shape per project config — `[[agents]]` for env entries, or `[agents.teams]` for shared teams. Applying the declared layout, hooks, and agent launch command is planned project-config behaviour.

```toml
[[agents]]
name = "claude"
launch_command = "claude"
env = { CLAUDE_CODE_DISABLE_AGENT_VIEW = "1" }

[[hooks]]
event = "PreToolUse"
command = "notify-send rimz"
```

Command-running fields enter the trust hash, so a clone with project config reads `untrusted` until `rimz trust grant` pins the current surface on this machine. A trusted repo profile or team overlays machine config and wins on a name collision; a repo profile may inherit only another repo profile or a built-in kind, and a repo team role may bind only a repo profile, keeping the hashed surface closed and machine-independent. An `untrusted` or `stale` workspace refuses a launch that would consume a repo profile, team, or `[[agents]]` env, with the `rimz trust grant` fix. The hash contract and launch-time enforcement are in [trust.md](../internals/sidebar/trust.md); the threat model is in [security.md](../guide/security.md).

## Sidecars and privacy

Resolvers, remote aliases, and trust records each have their own command and reference: `rimz resolver` ([resolvers.md](../internals/agents/resolvers.md)), `rimz remote` ([getting started](./cli/getting-started.md#remote-rooms)), and `rimz trust` ([trust.md](../internals/sidebar/trust.md)).

Payload-fidelity and retention controls (`[privacy] payload_mode`) are a planned project surface. The design and intended keys are in [security.md](../guide/security.md), and the hook boundary they will govern is in [agent.md → The adapter boundary](../internals/agents/agent.md#the-adapter-boundary).
