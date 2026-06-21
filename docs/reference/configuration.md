# Configuration

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

Rimz runs with no configuration. Everything here is optional tuning.

Configuration has two tiers. The per-machine tier under `~/.config/rimz/` drives your terminal, accounts, notification routes, sidecars, and room preferences; it stays personal, uncommitted, and outside the project trust hash. The project tier at `<root>/.rimz/config.toml` declares a shared workspace shape; Rimz trust-tracks it today, and applying that shape is planned project-config behaviour.

## Get Started

```sh
rimz setup                         # detect this machine and offer to keep and refresh config
rimz setup --yes                   # non-interactive config merge/write; no hook or trust side effects
rimz config init --print           # print the commented field reference
rimz config init                   # write config.toml, theme.toml, and agents.toml
```

Most users start with `rimz setup` or `rimz config init`, then edit only the few lines they need. Setup keeps an existing config by default, refreshes it against the current templates, and reports any unknown or incompatible keys it skips. The generated template is the exhaustive field reference: every persisted section and default scalar is shown as commented TOML. Leaving a line commented keeps following the defaults shipped by future Rimz versions; uncommenting makes it this machine's override.

## The Files

| File | Scope | What it does | Who writes it |
| --- | --- | --- | --- |
| `~/.config/rimz/config.toml` | per-machine | core room behavior: accounts, notifications, remote-control auto-launch, sidebar behavior, multiplexer defaults, resume, smart-compact, Sentry | you, `rimz setup`, `rimz config` |
| `~/.config/rimz/theme.toml` | per-machine | sidebar appearance: palette, semantic slots, glyphs, animations, provider brand styling | you, `rimz setup`, `rimz config` |
| `~/.config/rimz/agents.toml` | per-machine | agent profiles, command cells, teams, worktree defaults, loop automation, attention windows, pets | you, `rimz setup`, `rimz config`, `rimz loop` |
| `~/.agents/agents/<name>/agent.toml`, `~/.agents/teams/<name>/team.toml` | per-machine | drop-in agent profile and team fragments merged under `agents.toml` | agent factory, you |
| `~/.config/rimz/resolvers.toml` | per-machine | resolver allowlist and chain order | `rimz resolver` |
| `~/.config/rimz/remote.toml` | per-machine | named SSH room aliases | `rimz remote` |
| `~/.config/rimz/projects/<id>/trust.toml` | per-machine | project executable-surface trust grant | `rimz trust` |
| `<root>/.rimz/config.toml` | committed | declared workspace shape, trust-tracked today | humans and project automation |
| `<root>/.worktreeinclude` | committed | glob patterns for untracked files to seed into new worktrees | humans |
| `<root>/.worktreelink` | committed | directory paths to symlink-share into new worktrees and exclude from git dirtiness | humans |

Per-machine settings load leniently: a missing file is the default config, unknown keys are ignored so an older binary can tolerate a newer file, and a file Rimz cannot parse degrades to built-in defaults with a warning at startup so a broken config never blocks the room. `rimz config` and `rimz doctor` report the precise error and fix, and `rimz config set` rejects unknown dotted keys before it writes.

## Per-machine config set

`rimz config init` writes three sibling files. The in-memory `MachineConfig` mirrors that layout: core behavior from `config.toml`, appearance from `theme.toml`, and agents-side behavior from `agents.toml`. Missing files load as defaults, and `rimz config set` routes a dotted key to the owning file.

`config.toml` carries the core behavior sections:

| Section | Purpose |
| --- | --- |
| `[remote_control]` | per-agent remote-control auto-launch opt-ins |
| `[accounts]` | display-only monthly ceilings |
| `[notifications]` | best-effort desktop, bell, and command notifications |
| `[sidebar]` | sidebar focus key and preferred worktree comparison trunk |
| `[zellij]` | Rimz-owned Zellij room defaults |
| `[tmux]` | Rimz-owned tmux room defaults |
| `[resume]` | agent re-seeding on rebirth, and opt-in auto-continue on rate-limit reset |
| `[harness]` | default smart-compact threshold for steer/queue |
| `[sentry]` | off-box error reporting target |

`theme.toml` carries appearance:

| Section | Purpose |
| --- | --- |
| `[theme]` | style preset, color depth, scheme, and semantic slot overrides |
| `[theme.display]` | sidebar render cadence, sizing, dashboard layout, scroll, glow, and card density |
| `[theme.display.context_meter]` | agent-card context meter color stops |
| `[theme.display.budget_bar]` | provider budget bar color zones |
| `[theme.display.budget_bar.burn_rate]` | provider reset-marker burn-rate zones |
| `[theme.animations]` | status-head frames, tones, effects, and unread pulse |
| `[theme.glyphs]` | Unicode and Nerd Font glyph examples plus set selection |
| `[theme.providers]` | provider dashboard name, emblem, and brand colors |
| `[colors.*]` | pasteable Alacritty palette lifted into `theme.colors` |

`agents.toml` carries agent-side behavior:

| Section | Purpose |
| --- | --- |
| `[agents]` | launch placement plus profiles, command cells, and named teams for `rimz agents <spec>` |
| `[agents.worktree]` | where Rimz-owned Git worktrees live and which base ref new ones branch from |
| `[agents.loop.tasks]` | scheduled supervised agent turns, applied to this machine's scheduler by `rimz loop install` |
| `[agents.attention]` | stale-running and inactive-row timing |
| `[agents.pets]` | provider-dashboard companion overlay |

Rimz also discovers drop-in fragments under `~/.agents/agents/<name>/agent.toml` and `~/.agents/teams/<name>/team.toml`. Fragments use the same `[agents.profiles]`, `[agents.commands]`, and `[agents.teams]` TOML shape as `agents.toml`; entries in `~/.config/rimz/agents.toml` override fragments with the same name. Set `RIMZ_AGENTS_HOME` to relocate the fragment root.

Every field, its default, and an inline note lives in the generated template:

```sh
rimz config init --print
```

The sections below explain the model and the knobs whose behavior is easy to misread.

### Loop tasks

```toml
[agents.loop.tasks.morning]
spec = "claude-ping"     # `<kind>-ping` primes a provider window
prompt = "ping"
root = "/home/you/code/app"
at = "07:00"             # 24h local wall-clock
days = "weekdays"        # daily | weekdays | weekends | mon-fri | mon,wed,fri

[agents.loop.tasks.pr_watch]
spec = "codex"
prompt = "check CI on the release PR"
root = "/home/you/code/app"
every = "15m"
mode = "auto"
```

Each task drives one supervised turn for a single agent spec on a calendar, interval, raw cron, or one-shot schedule. A `<kind>-ping` spec is the window-priming special case: it defaults the prompt to `ping` and skips when that provider's budget window is already counting down. The config records the intent; `rimz loop install` applies it to this machine's OS scheduler after a consent preview. The full model — the supervised-run path, installer, one-shot cleanup, and why each entry carries an absolute `root` — is [loop.md](../internals/agents/loop.md).

### Notifications

```toml
[notifications]
triggers = ["waiting", "failed"]
desktop = "auto"
sound = "bell"
remind_secs = 60
command = "ntfy publish rimz"
```

Notifications are best-effort attention delivery layered over the sidebar inbox. `waiting`, `failed`, `paused`, and `success` rows become unread until read; `notifications.triggers` filters only which newly unread kinds raise a banner/command. `running` and `idle` stay quiet. `debounce_ms` limits repeat pushes for the same agent, `coalesce_ms` groups bursts into one banner, and `remind_secs` re-rings local unread `waiting`/`failed` rows until read. Set `remind_secs = 0` to disable reminders.

`desktop = "auto"` emits terminal OSC notifications under tmux and skips them under Zellij, which drops notification OSCs today. `desktop = "osc"` forces emission for testing or future terminal paths. `sound = "bell"` writes a separate BEL byte and your local terminal decides whether that is audible.

`command` runs locally through `sh -c` with `RIMZ_NOTIFY_TITLE`, `RIMZ_NOTIFY_BODY`, `RIMZ_NOTIFY_AGENT`, and `RIMZ_NOTIFY_KIND` in its environment. Reminder commands also receive `RIMZ_NOTIFY_UNREAD`. Use it for machine-local routing such as ntfy, Slack, Pushover, or an OS notifier. Mechanics live in [internals/sidebar/notifications.md](../internals/sidebar/notifications.md).

### Remote Control

```toml
[remote_control]
claude = false
codex = false
```

These are per-machine opt-ins for background remote-control infrastructure. The `rimzd` view appears on every `rimz start` with the live stats heatmap in the middle column. `claude = true` adds `claude remote-control --spawn worktree` to the stacked daemon column when `claude` is on PATH. A `codex` CLI on PATH adds the per-session app-server broker to that column; `codex = true` also ensures the managed standalone Codex daemon (`$CODEX_HOME/packages/standalone/current/codex remote-control start`) before the room opens.

Configured hosts are fail-fast preconditions for `rimz start`. Claude refuses when its own settings or version make the host impossible: Claude Code older than 2.1.51, `disableRemoteControl: true`, `disableAgentView: true` on Claude Code 2.1.173 or newer, or API-key auth sources active on Claude Code 2.1.157 or newer. Codex refuses when the managed standalone install is missing. `rimz doctor` reports the same refusal text and fix before launch.

The sidebar's `⇅ rc` provider flag is broader than these auto-launch toggles: it also lights when a provider-owned pane-session setting enables remote control, such as Claude's `remoteControlAtStartup: true`. Reading provider settings is local enrichment and adds no project trust-hash field.

### Accounts

```toml
[accounts]
[accounts.usage_limit_usd]
claude = 50.0
codex = 25.0
```

Account enrichment is local, read-only, and best-effort. Rimz uses provider account-usage surfaces reached from local OAuth credentials or the local Codex app-server when available; `RIMZ_OAUTH_USAGE_OFFLINE=1` disables those fetches for one process tree. It does not disable transcript-derived spending totals, and it never writes provider credential files.

`[accounts.usage_limit_usd]` sets display-only monthly USD ceilings by provider kind. A ceiling scales the provider dashboard's `ex` or `api` bar when the provider does not report a real cap; it is not a provider-enforced spending limit and does not stop agents. Leaving a provider unset means the paid/API row reads uncapped or unknown with `∞`.

### Multiplexer Room Options

Rimz applies room-scoped settings when it creates or reattaches a session, so the room gets the mux behaviour agents need without editing your global Zellij or tmux files.

```toml
[zellij]
mouse_click_through = true
focus_follows_mouse = true
session_serialization = false
auto_layout = true

# Optional overrides. Left unset, your ~/.config/zellij/config.kdl or Zellij's defaults win.
pane_frames = true
copy_clipboard = "system"

[tmux]
set_clipboard = "on"
extended_keys_format = "csi-u"
```

Zellij receives settings as `zellij attach ... options ...` on room birth and attach. Rimz always applies the room invariants it owns: locked mode, click-through on supported Zellij versions, focus-follows-mouse on, session serialization off, and auto-layout on. Every other `[zellij]` key is an override: set it in `config.toml` to pass the matching Zellij `options` flag, or leave it unset to use `~/.config/zellij/config.kdl` and Zellij's defaults. Work panes keep Zellij's default frames unless you configure otherwise; the sidebar pane is explicitly borderless so its hit-testing stays stable. tmux receives session, window, and server-scoped options as required by tmux itself; clipboard and rich-key handling are server-scoped in tmux. The backend mapping is in [internals/sidebar/multiplexers.md](../internals/sidebar/multiplexers.md).

### Resume

```toml
[resume]
on_rebirth = true
max = 8
auto_continue = false
auto_continue_overloaded = false
auto_continue_overloaded_backoff_secs = [60, 120, 180]
auto_continue_overloaded_max_retries = 10
auto_continue_text = "continue"
```

Resume covers two tenses. On a **rebirth** — reboot, multiplexer crash, or clean Rimz rebirth of a stuck room — Rimz offers to recover prior agents from the durable rollup. The interactive prompt defaults yes, non-interactive starts recover, and each restored agent starts idle in its worktree's `#channel` tab, so no model work happens until you type. Closing a tab while the room survives records the end trace that keeps that agent out of future recovery. `rimz reset`, `on_rebirth = false`, and `--no-resume` come up empty for a fresh room, and `max` bounds how many agents one birth relaunches. Mechanics live in [internals/sidebar/sidebar.md](../internals/sidebar/sidebar.md#resume-on-rebirth).

While the room is **live**, `auto_continue` picks a rate-limit-parked agent's turn back up the moment its 5h/7d window resets: the producer types `auto_continue_text` into the agent's pane through the same send path `steer` uses, so the agent's next hook returns it to `running`. `auto_continue_overloaded` uses the same nudge text for an overload-parked agent on a bounded retry ramp; `auto_continue_overloaded_backoff_secs` sets the sequence, the last value repeats, and `auto_continue_overloaded_max_retries` stops attempts while leaving the row paused. Both toggles are off by default. Each resume is recorded as a text-free `agent.resumed` event. Mechanics live in [internals/agents/provider.md](../internals/agents/provider.md#spent-windows-and-paused-rows).

### Harness

```toml
[harness]
smart_compact = "70%"
```

`smart_compact` sets the default threshold for Rimz's compact-first `steer` and `queue` sends. Use a percentage string (`"70%"`) or an occupied-token count string (`"120000"`); leave it unset to keep compact-first sends opt-in through `--smart-compact`. A per-command flag overrides the config value. Mechanics live in [internals/agents/harness.md](../internals/agents/harness.md#compact-before-sending).

### Off-Box Error Reporting

```toml
[sentry]
dsn         = "https://examplePublicKey@o0.ingest.sentry.io/0"
environment = "production"
```

Set a `dsn` to report Rimz `warn!`/`error!` events and observed agent rate-limit/overload conditions to a Sentry project an operator can watch; agent-generated conditions report at warning level. With no `dsn`, reporting stays off and Rimz makes no network calls. `RIMZ_SENTRY_DSN` and `RIMZ_SENTRY_ENVIRONMENT` override the config for a single invocation. `environment` defaults by build profile when unset — an installed release reports as `production`, a dev or CI build as `development`, so contributor noise stays off the production dashboard. The DSN lives per-machine — never in the committed project config — so a clone never inherits it; events carry the `rimz@<build>` release plus `workspace`, `command`, `build`, `fault` (`agent` for an observed provider condition, `rimz` for a Rimz fault), and (when known) agent and session tags so one machine-wide project filters per repository, command, and fault class. A stable per-callsite fingerprint collapses an issue across builds, and a per-fingerprint rate limit caps any single hot path. The hostname and personal data are withheld; the full telemetry surface is in [security.md → Off-box error reporting](../guide/security.md#off-box-error-reporting). Mechanics live in [internals/health/observability.md](../internals/health/observability.md).

### Worktrees

```toml
[agents.worktree]
dir = "../{repo}-worktrees"
base = "fresh"
```

`rimz worktree` and `rimz agents --worktree` use this section when creating Rimz-owned Git worktrees. Relative `dir` values resolve from the repository root, and `{repo}` expands to the root directory basename. `base = "head"` branches from local `HEAD`, `base = "fresh"` branches from `origin/HEAD`, and any other string is passed to Git as the base ref. A committed `<root>/.worktreeinclude` lists glob patterns for untracked files to copy from the checkout into each new worktree; `<root>/.worktreelink` lists directories to symlink-share into each new worktree. Seeding, symlink registration, and cleanup state live in [internals/agents/worktree.md](../internals/agents/worktree.md).

### Agent Profiles, Commands, And Teams

```toml
[agents]
placement = "auto"

[agents.profiles.claude-slim]
agent = "claude"
effort = "low"
system-prompt-file = "~/.config/rimz/prompts/slim.md"
append-system-prompt-file = "~/.config/rimz/prompts/shared-rules.md"

[agents.profiles.planner]
agent = "claude-slim"
system-prompt-file = "~/.config/rimz/prompts/planner.md"

[agents.profiles.claude]
agent = "claude"
args = "--append-system-prompt 'Prefer concise plans.'"

[agents.commands]
vim = "nvim -p"
htop = "htop"

[agents.profiles.codex-yolo]
agent = "codex"
mode = "yolo"
model = "gpt-5-codex"
effort = "high"

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

Named teams feed `rimz agents <name>`. A team is an ordered `roles` list; each role binds a role name to a profile and may override `mode`, `model`, `effort`, `system-prompt-file`, `append-system-prompt-file`, or `args`. The default placement opens the role list left to right as one side-by-side column per role in one tab; optional `layout` uses the same comma=column / plus=row shape grammar as inline specs, resolves declared role names first, and then falls back to roleless cells such as `term`, commands, profiles, kinds, or virtual cells. Each member answers to `@<role>` in that channel. Inline specs such as `rimz agents "claude,codex+term"` keep the ad-hoc shape grammar: commas split columns, plus signs stack rows, and each cell is a command, profile, or built-in cell. Inline cells resolve in this order: `[agents.commands]`, `[agents.profiles]`, built-in `term`, registered agent kinds, and adapter-supported virtual `<kind>-<mode>` / `<kind>-ping` variants such as `claude-auto`, `codex-ask`, `codex-yolo`, and `claude-ping`. The built-in `peer` team remains `claude,codex` and is roleless. Profile, command, and team names reserve `list`, `ls`, `show`, `stop`, `focus`, `wait`, `term`, and `exec`. Commands may shadow cell words such as `claude` to set local command defaults; profiles and roles become addressable handles, so they must not shadow `@all`, agent kinds such as `@claude`, kind ordinals such as `@claude-2`, or pane/channel sigils (`:`, `#`). A profile may be named like a kind: `[agents.profiles.claude]` overrides the base for bare `claude`, profiles that say `agent = "claude"`, and virtual cells such as `claude-auto` and `claude-ping`.

`placement` sets where a launch lands when neither `--new-pane` nor `--new-tab` is passed. `"auto"` (the default) runs a single non-worktree agent in the current pane and opens a new tab for a worktree launch, team, or multi-cell layout; `"pane"` splits a new pane for a single non-worktree agent and opens a new tab for a worktree or team; `"tab"` always opens a new tab.

Commands under `[agents.commands]` are bare strings shell-split as raw command panes. Profiles under `[agents.profiles.<name>]` require `agent`, which names a built-in kind or another profile. The inheritance chain flattens at launch to one concrete adapter kind; each field takes the nearest set value, including `args`, so a child profile that sets `args` replaces the base `args` entirely. Team role overrides layer over the referenced profile the same way, with `args` replacing rather than appending. `model`, `effort`, `system-prompt-file`, and `append-system-prompt-file` render through the resolved adapter, `mode = "auto" | "ask" | "plan" | "yolo"` adds that adapter's permission argv, then `args` is shell-split and appended. `system-prompt-file` gives the profile or role its own voice; `append-system-prompt-file` keeps the adapter's base prompt and appends extra rules where the adapter supports it. `~` expands to the home directory and a relative path roots at the config file's directory, so prompt files point at the same file wherever the profile launches, and each file must exist when the profile launches — a missing one fails the launch with the path to fix. Command-line `--model`, `--effort`, `--system-prompt-file`, and `--append-system-prompt-file` render after the profile preset and override it for that launch. Unsupported typed fields fail at launch with the profile name and the field to remove. Per-machine configs with `[tab]`, `[tab.keywords]`, `[tab.layouts]`, or `[agents.aliases]` hard-error; rename them to `[agents]`, `[agents.profiles]`, `[agents.commands]`, and `[agents.teams]`.

Trusted project config may also declare top-level `[profiles]` and `[agents.teams]` in `<root>/.rimz/config.toml`. Repo profiles and teams are inert until the workspace is trusted, enter the project executable-surface hash, and win on name collision with machine config. A repo profile may inherit only another repo profile or a built-in kind, and a repo team role may bind only a repo profile, keeping the hashed surface closed and machine-independent.

### Sidebar Bands

```toml
[theme.display.context_meter]
green = { percent = 40, tokens = 100000 }
yellow = { percent = 60, tokens = 160000 }
amber = { percent = 75, tokens = 258000 }
red = { percent = 90, tokens = 420000 }

[theme.display.budget_bar]
red = 10

[theme.display.budget_bar.burn_rate]
yellow = 100
amber = 150
red = 200
```

The agent card context meter reads its health by the worse of two axes: fill percentage and absolute tokens in the window. It interpolates continuously across the theme's OKLab health scale: below `green` it stays healthy green; at `green` it starts warming toward yellow; at `yellow` it starts warming toward amber; at `amber` it starts warming toward red; at `red` it stays alarm red. A large-window model can still warm by sheer token count even when its percentage looks calm.

The provider dashboard budget bar slides the same OKLab health scale in the opposite direction: it anchors green at a brimming window, then warms continuously as it drains, reaching warn at `yellow`, amber at `amber`, and alarm red at `red` (staying red below it), with the spans between interpolated. The template carries the shipped numbers.

Budget pace colors only the provider reset marker. `100` is even burn, where the used share matches the elapsed share of that window; a sustainable pace keeps the marker at the soft tier, then past `yellow` it slides the warm tail through amber to red while the bar keeps using the remaining-budget control points.

### Sidebar Rendering

```toml
[theme.display]
max_cols = 72
refresh_ms = 100
scrollbar = "auto"
glow = "auto"
card_density = "auto"

[sidebar]
trunk = "develop"
focus_key = "Alt+p"

[agents.pets]
enabled = false
pet = "codex"
size = "medium"
glyphs = "auto"
voice = true
```

`max_cols` caps the creation-time sidebar pane width so a percentage split does not swallow ultra-wide terminals. `refresh_ms` controls the renderer's animation grid, not the producer's data cadence. `scrollbar` controls only the right-margin overflow indicator.

`focus_key` is the global multiplexer chord that focuses the sidebar from any pane, and toggles — press it again to return to your last working pane. It runs `rimz sidebar focus --toggle`, which resolves and focuses the room's sidebar pane. Both backends bind it automatically at session birth: tmux as a root-table `bind-key`, and Zellij through the presence plugin, which binds the chord at runtime once you grant it Reconfigure (the bind resets when the session ends and never touches your `config.kdl`) ([multiplexers.md → Focus key](../internals/sidebar/multiplexers.md#focus-key)). The default is `Alt+p` (`Alt` survives the terminal and Zellij's locked mode, and avoids tmux's `Ctrl+B` prefix); `Ctrl+<key>` is also accepted. Set it empty or `off` to register nothing and leave every key as it was. The sidebar's `?` overlay shows the active chord, and the in-sidebar keys (`n`/`N` to walk the inbox, `m`/`M` to mark read/unread, and the rest) are in [the interface reference](../interface/sidebar.md#jump--the-row-is-the-link).

`[theme]` picks the palette — built-in schemes, bundled Alacritty themes, color depth, and per-slot overrides — and `theme.display.glow` gates transition flashes over that base render. The full theming surface, including `[theme.animations]` status heads and `[theme.providers]` brand styling, lives in [theme.md](./theme.md).

`card_density = "auto"` keeps the standard agent card: identity, description, context meter, context line, and subagents on the selected card. `expanded` shows every card's subagents. `compact` trims resting cards by status while the selected card opens to the standard card.

| status in `compact` | resting lines |
|---------------------|---------------|
| `idle` | identity |
| `running`, `waiting` | identity + description + context meter |
| `paused`, `success`, `failed` | identity + description |

`trunk` is a preferred comparison target for the worktree header's git stats. A repo where that branch does not resolve falls back to the detection ladder: `main`, then `master`, then the remote's advertised default.

`[agents.pets] enabled = true` adds a right-side pet overlay to the provider dashboard. `pet` selects one of four sources, in this order: a built-in catalog id (`codex`, `dewey`, `fireball`, `rocky`, `seedy`, `stacky`, `bsod`, or `null-signal`) wins; an `http(s)://` selector is your own WebP spritesheet by URL; a path-like selector (one with a `/`, a `.`, or a leading `~`) is a local sheet or a petdex pet directory; and a bare slug is a petdex pet installed under `~/.codex/pets/<slug>/`. So `pet = "wall-e"` shows a petdex-installed pet, `pet = "~/pets/dragon.webp"` a local sheet, and `pet = "https://example.com/dragon.webp"` a remote one. `size = "medium"` keeps the original pet footprint; `size = "small"` fits the sprite body to the active provider block height. `glyphs` chooses `auto`, `half`, `sextant`, or `octant` cell art, and `voice` controls the canned caption line. A built-in or URL sheet is fetched over HTTPS into the per-machine cache on first use (`RIMZ_PETS_OFFLINE=1` uses the cache only); a remote URL must be `https`, while petdex and local sheets are read straight off disk with no network. A petdex pet is a directory holding a `pet.json` (whose `spritesheetPath` names the sheet) beside the WebP; any bring-your-own sheet matches the catalog geometry — a `1536×1872` WebP holding an `8×9` grid of `192×208` RGBA frames (alpha renders transparent). Pets execute no commands and stay outside the project trust hash; a configured URL widens asset egress to the host you name, while petdex and local sheets reach the network not at all. Internals, the geometry contract, and the cache contract live in [pets.md](../internals/sidebar/pets.md).

### Provider Dashboard

```toml
[theme.display]
provider_tabs = "auto"
provider_list = ["codex", "all"]
max_provider_blocks = 3
```

The dashboard shows one block per discovered provider. `provider_tabs = "auto"` stacks one or two providers and switches to tabs at three or more. `provider_list` chooses kinds and order; `"all"` expands to every remaining discovered provider at that position. Empty discovery uses today's spend to choose up to `max_provider_blocks`, then orders the retained providers stably by kind.

`[theme.providers.<kind>]` restyles a provider's display name, ASCII art, and brand color; the fields and formats live in [theme.md](./theme.md#provider-styling). Account and budget sourcing is in [internals/agents/provider.md](../internals/agents/provider.md).

## Changing Values

```sh
rimz config path
rimz config get
rimz config get theme.display.max_cols
rimz config get sidebar --json
rimz config set theme.display.max_cols 80
rimz config set theme "TokyoNight Night"
rimz config set agents.worktree.base fresh
rimz config set notifications.triggers '["waiting", "failed"]'
```

`rimz config get` loads the effective per-machine config over built-in defaults. `rimz config set` edits one key in the owning per-machine file (`config.toml`, `theme.toml`, or `agents.toml`), preserves comments through `toml_edit`, rejects unknown keys, deserializes the whole result as `MachineConfig`, then writes with Rimz's temp-file-plus-rename durability primitive. `theme.colors.*` keys write to root `[colors.*]` in `theme.toml`, so Alacritty palettes stay paste-compatible.

Bare `config set` values become TOML values when they parse (`80`, `false`, arrays, inline tables); otherwise they become strings (`fresh`, `always`). For context bands, set the whole band as an inline table: `rimz config set theme.display.context_meter.red '{ percent = 90, tokens = 400000 }'`.

## Merge Order

Later layers win:

1. built-in defaults,
2. project config (`.rimz/config.toml`),
3. per-machine config set (`~/.config/rimz/{config,theme,agents}.toml`),
4. CLI flags and `RIMZ_*` environment variables.

This is the designed model. Today the per-machine layer is live, CLI/env overrides are applied by the commands that define them, and the project layer is read for the trust hash. Project `[profiles]` and `[agents.teams]` are live when trusted and deliberately invert the general order for launch names: trusted repo profiles and teams overlay machine config so a repository can pin the launch surface it hashes.

## Project Config

The committed `<root>/.rimz/config.toml` declares the workspace shape a team wants to share. Rimz computes the executable-surface trust hash from it, and on a trusted workspace injects each `[[agents]]` `env` table into that agent's process at launch and applies top-level `[profiles]` plus `[agents.teams]` to `rimz agents` launches. Use one `agents` shape per project config: `[[agents]]` for env entries or `[agents.teams]` for shared teams. Launch-time application of the declared layout, hooks, agent `launch_command`, and top-level `[env]` is planned project-config behaviour.

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
