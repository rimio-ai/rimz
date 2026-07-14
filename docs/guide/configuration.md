# Configuration

RimZ runs with zero configuration: start it and you have a working room, nothing to write first. Everything you can tune from there is plain TOML in files you own. There is no config daemon holding your settings, no separate UI, and no bespoke language between you and a value: you already edit dotfiles and keep them under version control, and RimZ asks nothing new of that habit.

Change something when you have a reason to, one line at a time: pin a theme, save a launch profile, route a notification. This page is the whole model. It opens with the settings most people touch and exactly what changing one does to your disk, then maps where configuration lives and how the layers combine, then gives a section per file so you can jump to the one you are editing. For the guided first pass on a new machine, read [set up your machine](./setup.md) first; this page is the reference it links into.

> The invariants behind this model are in [DESIGN.md](../../DESIGN.md#invariants).

## Common changes

Most people touch a handful of settings and leave the rest on their defaults. Each command below is the whole change: `rimz config set` writes the value into the file that owns it, in place, so you never open an editor.

```sh
rimz config set theme "Catppuccin Mocha"       # sidebar palette; `rimz list-themes` shows the choices
rimz config set theme.style modern             # truecolor plus Nerd Font glyphs
rimz config set theme.pets.enabled true        # an animated companion on the provider dashboard
rimz config set resume.auto_continue true      # resume rate-limit and API-error parks on their own
rimz config set harness.smart_compact "70%"    # compact before a message once context passes 70%
rimz config set notifications.triggers '["waiting", "failed"]'   # which rows raise a banner
rimz config set sidebar.focus_key "Alt+p"      # the chord that jumps to the sidebar from any pane
rimz config set timezone "America/New_York"    # transcript times, scheduling, and the "today" cutoff
```

### What `rimz config set` does to your disk

`set` edits a real file, and it is worth knowing exactly which one and how, because that is what makes it safe to run.

It takes a dotted key, routes it to the file that owns the key (`theme.*` to `theme.toml`, `agents.*` to `agents.toml`, `loop.*` to `loop.toml`, everything else to `config.toml`), and edits that file in place. It parses the existing TOML with `toml_edit`, so your comments and formatting survive untouched. It rejects an unknown key rather than writing a typo, re-validates the whole resulting file, then writes it with a temp-file-plus-rename so a crash mid-write never leaves a half-written file. The result is byte-for-byte the file you would have edited by hand, which is what makes undoing a change ordinary: re-run `set` with the old value, or open the file and delete the line to fall back to the default.

A bare value becomes a TOML value when it parses (`80`, `false`, an array, an inline table) and a string otherwise (`fresh`, `always`). Set a whole color band as an inline table, for example `rimz config set theme.display.context_meter.red '{ percent = 90, tokens = 400000 }'`. Keys under `theme.colors.*` write to the root `[colors.*]` table in `theme.toml`, so an Alacritty palette pasted there stays paste-compatible.

### Read a value back

```sh
rimz config path                          # the resolved config.toml path
rimz config get                           # the whole effective config as TOML
rimz config get theme.display.max_cols    # one value
rimz config get sidebar --json
```

`get` shows the effective config: your overrides layered over the built-in defaults. It answers "what is RimZ actually using", not "what did I write", so a key you never set still prints the default it is following.

## Where your configuration lives

Configuration comes from two places, and each answers a different question.

Your personal settings live in one directory, `~/.config/rimz/`: your terminal, accounts, notifications, theme, and launch shortcuts. This tier is yours, uncommitted and outside the project trust hash, so nothing you set here follows a repository to someone else's machine.

A repository can also carry one shared file, `<repo>/.rimz/config.toml`: the shape a team agrees on through the repo, such as the agents a clone should launch and the loop tasks it should run. Because that file can name commands to execute, RimZ trust-tracks it, and a fresh clone reads `untrusted` until you review the executable surface and grant it. The full model is [Project config](#project-config).

### The files in your home directory

You rarely open these by hand: `rimz config set` writes to them for you, and `rimz config init` creates them. The directory is split into a few files by concern so every command has one clear place to write and you always know which file owns a setting.

| File | What it holds |
| --- | --- |
| `config.toml` | room behavior: accounts, notifications, remote control, multiplexer defaults, resume, smart compaction |
| `theme.toml` | sidebar appearance: palette, slots, glyphs, animations, provider styling, pets ([theme.md](./theme.md)) |
| `agents.toml` | agent profiles, command cells, teams, worktree defaults, attention timing |
| `loop.toml` | recurring loop task definitions and scheduled command checks |

Two more things share the directory but are managed for you: `remote.toml` (named SSH room aliases, written by `rimz remote`) and a handful of machine-managed sidecars (trust grants, notification state), which you reach through their own commands rather than by hand ([Sidecars and privacy](#sidecars-and-privacy)).

`agents.d/<kind>/agent.toml` is the machine-tier library for [third-party agent plugins](../reference/agent-plugins.md). Each bundle may also carry a shim, setup guide, and executable probes. `rimz agents register <kind>` creates the directory; `rimz agents register --check` validates every bundle.

### How the layers combine

RimZ reads configuration in four layers, and a later layer wins:

1. built-in defaults,
2. project config (`<repo>/.rimz/config.toml`),
3. per-machine config (`~/.config/rimz/`),
4. CLI flags and `RIMZ_*` environment variables.

Today the per-machine layer is live, CLI and env overrides apply where each command defines them, and the project layer is read for trust. One case inverts the order on purpose: a trusted project's launch names and loop-task names (`[profiles]`, `[agents.teams]`, `[tasks]`) overlay your machine config and win a name collision, so a repository can pin the exact executable surface it hashes (see [Project config](#project-config)).

### A broken file never blocks the room

Per-machine settings load leniently. A missing file is the default config, unknown keys are ignored so an older binary tolerates a newer file, and a file RimZ cannot parse falls back to built-in defaults with a startup warning. The room still opens, and `rimz config` and `rimz doctor` report the precise error and the fix.

Agent plugin manifests are the deliberate exception because they declare executable launch and probe commands. A malformed `agents.d/*/agent.toml` makes `rimz start` refuse before room side effects; read-only commands skip it, and `rimz doctor` reports its path and validation error.

## Generate and refresh the files

You never have to write these files from scratch. RimZ ships a commented template for each one, and generating them is safe to repeat.

```sh
rimz                       # first start writes any missing config, then opens the room
rimz setup                 # detect this machine and write or refresh config
rimz config init           # write config.toml, theme.toml, agents.toml, and loop.toml
rimz config init --print   # print the commented templates without writing anything
```

Most people run `rimz` inside a project once, or `rimz setup` once, then edit the few lines they care about. First start on an interactive terminal writes any missing per-machine config, offers hook install, runs the live glyph probe, and asks whether to enable a pet; a non-interactive first start writes the same defaults without prompting.

**Rerunning is safe: your values are kept.** `rimz setup` and `rimz setup --yes` merge. They write the files that are missing and leave the ones you already tuned alone, skipping any key an older file no longer understands with a warning. When an existing file is unparseable, setup leaves it byte-for-byte untouched, prints the parse error, and asks you to fix it before rerunning. `rimz config init` is stricter: it refuses to touch an existing file and tells you to pass `--force`, and `--force` is the deliberate clean reset that overwrites with fresh templates. So the routine refresh (`rimz setup`) never overwrites your work, and the destructive path (`init --force`) is the one you have to ask for by name.

**The template is the field reference.** Every persisted section and default ships as commented TOML with an inline note, so `rimz config init --print` is the authoritative, always-current list of keys and defaults. This page explains the model and the knobs that are easy to misread, and leaves the exhaustive field list to the template. A line left commented keeps following the default that future RimZ versions ship; uncommenting it makes that value this machine's override.

## config.toml: room behavior

The core file holds how the room behaves. Each subsection below shows the shape; `rimz config init --print` carries every key and default.

### Notifications

```toml
[notifications]
triggers = ["waiting", "failed"]
desktop = "auto"
sound = "bell"
remind_secs = 60
title = "RimZ: {{agent}} {{kind}}"
body = "{{task}}"
command = "ntfy publish rimz"

[[notifications.handler]]
name = "waiting-ntfy"
command = "ntfy publish --title {{title}} rimz {{body}}"
when = { kind = ["waiting"], worktree = ["feat/*"], handle = ["@planner"] }
```

Notifications deliver attention off-screen, best-effort, over the sidebar inbox. `waiting`, `failed`, `paused`, and `success` rows become unread until read; `triggers` filters which newly-unread kinds raise a banner or handler command, while `running` and `idle` stay quiet. `desktop = "auto"` emits terminal OSC notifications under tmux and skips them under Zellij, which drops notification OSCs today; `sound = "bell"` writes a BEL byte and your terminal decides whether it is audible.

`title` and `body` are optional templates for agent-status and coalesced desktop or banner text. Templates substitute `{{kind}}`, `{{agent}}`, `{{handle}}`, `{{status}}`, `{{worktree}}`, `{{task}}`, `{{count}}`, and `{{unread}}`; `agent` and `handle` are the agent handles or roles joined for multi-agent notifications, and a value unavailable for a notification kind renders empty. Reminder and remote-link notifications keep their built-in text.

Each `[[notifications.handler]]` runs locally through `sh -c` when all present `when` clauses match. `kind` names notification kinds (`waiting`, `failed`, `paused`, `success`, `coalesced`, `reminder`, `loop_paused`, `link_lost`, `link_restored`), `worktree` glob-matches an agent branch or path, and `handle` glob-matches the agent handle or role (a leading `@` in the pattern is accepted as the usual address sigil). A `command` template may also use `{{title}}` and `{{body}}`, the rendered banner strings. Each substituted command value is shell-quoted as one token, so write `ntfy publish --title {{title}} rimz {{body}}`, not `--title "{{title}}"`. The legacy `command = "..."` key is shorthand for one unconditional handler, and every handler receives `RIMZ_NOTIFY_TITLE`, `RIMZ_NOTIFY_BODY`, `RIMZ_NOTIFY_AGENT`, and `RIMZ_NOTIFY_KIND` in the environment; reminders also get `RIMZ_NOTIFY_UNREAD`. The debounce, coalesce, and remind model is in [notifications.md](../internals/sidebar/notifications.md), and the guide is [notifications.md](./notifications.md).

### Resume

```toml
[resume]
on_rebirth = true
max = 128
auto_continue = false
auto_continue_backoff_secs = [180, 300]
auto_continue_max_retries = 12
auto_continue_text = "continue"
```

Resume covers two moments; the behavior model is [loops.md → Built-in recovery](./loops.md#built-in-recovery), and these are its keys.

On a **rebirth after a reboot or multiplexer crash**, RimZ offers to recover prior agents from the durable record: the prompt defaults yes, non-interactive starts recover, and each restored agent starts idle in its worktree tab (empty named channels also reopen on same-boot rebirths). `on_rebirth = false`, `--no-resume`, and `rimz reset` come up without agents; `max` bounds how many agents one birth relaunches (default 128).

While the room is **live**, `auto_continue` (off by default) picks a parked turn back up by typing `auto_continue_text` through the same path as `message --steer`:

- Rate-limit and spend-limit parks fire when the account's budget window resets.
- Overload and transient API-error parks (stalled streams, timeouts, connection drops) fire on the retry ramp: `auto_continue_backoff_secs = [180, 300]` sends the first retry three minutes after the failure, then every five minutes.
- Every park type stops retrying after `auto_continue_max_retries` (default 12, about an hour on the default ramp), leaving the row parked for you.

The rebirth path is in [sidebar.md](../internals/sidebar/sidebar.md#resume-on-rebirth) and the live path in [provider.md → Auto-continue](../internals/agents/providers.md#auto-continue).

### Daily dollar budgets

```toml
timezone = "America/New_York"

[harness]
budget = "50/day"

[accounts.budget]
claude = "100/day"
codex = "100/day"
```

`harness.budget` turns on a cap for each room's whole fleet, while `[accounts.budget]` turns on a cap for one provider login across every room on the machine. Account keys are accepted only for adapters that expose durable account-spend history; unknown kinds and identity-only or live-price-only providers such as Cursor are rejected by `config set`, strict loading, and room start. Cursor remains eligible for per-agent/session and room caps. Both daily keys read spend since local midnight in `timezone` and require the `/day` suffix; a bare amount is rejected with the form to use. These keys run no command and stay outside the project trust hash.

These keys are the on-switch; `rimz budget` inspects and adjusts an armed cap at runtime without touching them, and refuses to arm a cap they never set. The full cap model — the per-agent and loop-task scopes, what a park does, and what resumes it — is the [budgets guide](./budget.md).

### Smart compaction

```toml
[harness]
smart_compact = "70%"
```

`smart_compact` sets the default threshold for compact-first `message` sends, a percentage (`"70%"`) or an occupied-token count (`"120000"` or `"180k"`). When an agent's context window has reached the threshold, RimZ submits its `/compact` ahead of your text so the prompt lands against a fresh window. Leave it unset to keep compaction opt-in through the per-command `--smart-compact` flag, which overrides this value. The mechanics are in [messaging.md](../internals/harness/messaging.md#smart-compaction).

### rtk output compression

```toml
[harness]
rtk = "auto"
```

`rtk` controls output compression for RimZ-launched agents that run `cargo xtask`; a direct human `cargo xtask` run stays on plain cargo. `auto` wraps recognized cargo subcommands (`build`, `check`, `test`, `nextest`, `clippy`) through `rtk` when the binary is on the agent's `PATH`; `on` forces the wrapper and prints one warning before falling back to plain cargo when `rtk` is missing; `off` keeps cargo unwrapped. Install `rtk` on the machine for compression to take effect.

### Remote control

```toml
[remote_control]
claude = false
codex = false
```

Remote control is the bridge the providers' official mobile apps drive: with a host up, an agent's ask pushes to your phone and you answer it in the official app ([agents.md → Answer asks from your phone](./agents.md#answer-asks-from-your-phone)). These opt this machine into the background remote-control infrastructure shown in the `rimzd` daemon view. `rimz config set remote_control.claude true` adds `claude remote-control` to every running room's live `rimzd` view immediately when `claude` is on `PATH`, and `false` closes those managed host panes; a deliberately closed whole view stays closed until the next room start. `rimz config set remote_control.codex true` starts the managed standalone Codex daemon immediately, and `false` stops it. Each change wakes the running sidebars so the provider-dashboard flag follows the saved value. Direct file edits converge Claude hosts on the daemon-view repair pass. Direct Codex edits change future auto-start decisions; use `rimz config set` to transition an already-running daemon. A `codex` CLI on `PATH` already adds the per-session app-server broker independently of this toggle. `rimz start` and the live Claude enable path refuse when an installed, enabled host has a fixable misconfiguration such as an incompatible Claude version or settings. An enabled host whose agent is not installed is skipped so the room still starts, and `rimz doctor` reports that advisory with the install fix. The mechanics are in [providers.md](../internals/agents/providers.md), and the security boundary in [security.md](./security.md).

### Web access

```toml
[web]
enabled = true

[web.zellij]
base_url = "https://devbox.example/zellij"
auto_start = true
font = "JetBrainsMono Nerd Font Mono"
style_client = true

[web.tmux]
base_url = "https://devbox.example/tmux"
auto_start = true
```

`[web] enabled` defaults to true and gates `rimz web open` plus `rimz remote connect --web`. RimZ always seeds Zellij's permission cache for its own presence plugin's pane-topology permissions; when web is enabled it also seeds the web-sharing permission so runtime browser sharing works without a one-time prompt. When disabled, web commands fail before any room change and tell you to change the config on the machine serving the room.

`[web.zellij]` tunes browser access through Zellij's web server. `base_url` is the URL prefix RimZ prints for `rimz web open` and `rimz web url`, useful when a reverse proxy serves Zellij under a public host or path. `auto_start` lets `rimz web open` run `zellij web --start --daemonize` when the server is offline; set it to `false` when another supervisor owns the server. `style_client` lets RimZ write a generated Zellij `web_client` block on server start so the browser terminal uses `[theme]`; `font` sets that browser terminal font. These keys are per-machine and outside the project trust hash because they run no command and often name private hostnames or tunnels. Command detail is in [web.md](../reference/cli/web.md), and remote browser tunnels in [remote.md](../internals/remote.md#web-access).

`[web.tmux]` tunes browser access through ttyd. `base_url` is the reverse-proxy prefix for per-room ttyd routes, and `auto_start` lets `rimz web open` start the room's loopback ttyd process. The ttyd engine is selected by the room's tmux backend; install it with `brew install ttyd` or `apt install ttyd`. These keys execute nothing and stay outside the trust hash.

### Daemon view

```toml
[daemon]
[[daemon.pane]]
command = "stats"

[[daemon.pane]]
command = "btop"
cwd = "/var/log"
```

`[daemon]` configures the middle column of the `rimzd` daemon view, beside the sidebar and any managed hosts. Unset or empty keeps the built-in held-live stats pane (`rimz stats --refresh --hold`). Listing `[[daemon.pane]]` entries replaces that default, so include `command = "stats"` when you want live stats plus extra panes. The reserved command token `"stats"` expands to the built-in stats argv; any other `command` is split into argv and run directly without a shell. `cwd` is optional: absent runs from the worktree root, an absolute path is used as-is, and a relative path is joined onto the worktree root. A running room reloads command and cwd edits when `config.toml` is saved; adding or removing `[[daemon.pane]]` entries changes the pane count and takes effect on room restart. A pane with an empty or unparseable command is skipped, and if every configured pane is skipped, RimZ falls back to the built-in stats pane.

### Accounts

```toml
[accounts.budget]
claude = "100/day"

[accounts.usage_limit_usd]
claude = 50.0
codex = 25.0
```

`budget` sets the enforced local-day dollar cap described in [Daily dollar budgets](#daily-dollar-budgets) for descriptor-gated providers with durable spend; an unsupported entry refuses room birth instead of disappearing into lenient defaults. `usage_limit_usd` is separate, display-only, and has no account-spend eligibility gate, so Cursor remains valid there: its monthly ceiling scales the provider dashboard's `ex`/`api` bar when the provider reports no real cap, while the provider still enforces real spend and agents keep running. Account enrichment is local, read-only, and best-effort; `RIMZ_OAUTH_USAGE_OFFLINE=1` disables the live fetches for one process tree without touching transcript-derived totals or credential files.

### Off-box error reporting

```toml
[sentry]
dsn         = "https://examplePublicKey@o0.ingest.sentry.io/0"
environment = "production"
```

This section applies only to RimZ builds compiled with the non-default `sentry` feature, and it is deliberately omitted from the generated per-machine template. Set a `dsn` to report RimZ `warn!` and `error!` events and observed agent rate-limit or overload conditions to a Sentry project. With no `dsn`, reporting stays off and RimZ makes no network calls; without the feature, the block is inert. `RIMZ_SENTRY_DSN` and `RIMZ_SENTRY_ENVIRONMENT` override the config for one invocation, and `environment` defaults by build profile (an installed release reports as `production`, a dev or CI build as `development`). The DSN lives per-machine, never in committed project config, so a clone never inherits it; events carry low-cardinality tags (workspace, command, build, fault class, and agent or session when known) with the hostname and personal data withheld. The full telemetry surface is in [security.md](./security.md#off-box-error-reporting) and the mechanics in [diagnostics.md](../internals/diagnostics.md#off-box-error-reporting).

### Multiplexer room options

RimZ applies room-scoped multiplexer settings when it creates or reattaches a session, so the room behaves the way agents need without editing your global Zellij or tmux config. The `[zellij]` and `[tmux]` tables tune those settings; `rimz config init --print` lists every key with its default, and the per-backend mapping is in [multiplexers.md](../internals/multiplexers.md).

The `[mux]` table selects the default backend after the `--mux <name>` selection, its `--zellij`/`--tmux` shorthands, and active Zellij/tmux environment checks. Leave `default` unset to choose tmux when both backends are installed, or set it to `"zellij"` or `"tmux"` to require that backend. A configured backend that is not installed makes `rimz start` refuse with a fix message.

The two backends differ in how a key takes effect:

- **`[zellij]`** carries a few invariants RimZ always applies (locked mode, click-through with focus-follows-mouse off, no session serialization since RimZ owns rebirth, disabled session metadata, and native focused-pane splitting) plus optional keys (`pane_frames`, `copy_clipboard`, …) that apply only when you set them and otherwise fall through to your `~/.config/zellij/config.kdl`. The sidebar pane is always borderless so its hit-testing stays stable regardless of `pane_frames`.
- **`[tmux]`** applies its room invariants on every birth, each key carrying a RimZ default you can override. The pane-border keys are optional overrides; unset, they fall through to your `~/.tmux.conf` or tmux defaults just like `pane_frames`. Setting `pane_border_status` makes RimZ own `pane-border-format` too, blanking the sidebar border row and overriding any `~/.tmux.conf` format; unset, your tmux config wins and may title the sidebar. The table spans session, window, and server scope, including clipboard and rich-key handling, because tmux has no per-session form for those.

```toml
[mux]
default = "tmux"

[zellij]
pane_frames = true          # an optional override; unset, your config.kdl wins

[tmux]
## pane_border_status = "top"  # optional override; unset, your ~/.tmux.conf wins
```

To configure your *own* Zellij or tmux (the theme, true color, copy-mode, and keybindings RimZ leaves to you, and your sessions outside the room) see the [Zellij](./multiplexer.md#zellij) and [tmux](./multiplexer.md#tmux) baselines.

### Sidebar rendering

```toml
timezone = "America/New_York"

[sidebar]
focus_key = "Alt+p"
afk_after_secs = 900
trunk = "develop"
spend_window = "session"

[sidebar.keys]
narrower = "a"
wider = "d"
up = "k up"
down = "j down"
top = "g"
bottom = "G"
worktree_up = "K"
worktree_down = "J"
page_up = "ctrl+b pageup"
page_down = "ctrl+f pagedown"
screen_top = "H"
screen_bottom = "L"

[agents.attention]
stalled_after_secs = 1800
inactive_after_secs = 3600
archive_after_secs = 86400
```

`timezone` is an optional IANA zone for displayed transcript times, wall-clock scheduling, and the `"today"` spend cutoff; unset or unknown uses the system local zone. `focus_key` is the global multiplexer chord that focuses the sidebar from any pane and toggles back to your last working pane; both backends bind it at session birth, the default is `Alt+p`, and `""` or `off` registers nothing. `afk_after_secs` sets the input-idle window before the footer shows `zᶻ idle` on tmux, adding `· Nm` after the first minute; Zellij reports attach state only, so it shows `zᶻ away` on full detach regardless of this value, and the default is 900 seconds (15 minutes). `trunk` is a preferred comparison branch for the worktree header's git stats, falling back to `main` → `master` → the remote default when it does not resolve.

`[agents.attention]` tunes attention timing: `stalled_after_secs` is when a silent running agent escalates to the actionable `!` bucket (30 minutes by default), `inactive_after_secs` is when a card leaves hot work (one hour, the prompt-cache boundary, so a cold card reads as cold), and `archive_after_secs` is when a card parks below hot and warm work (24 hours by default). Set `archive_after_secs` greater than `inactive_after_secs`; a lower value is lifted to the first second after the inactive window. The `[theme.display]` knobs that share this area (render cadence, sizing, `scrollbar`, and `card_density`) are theme settings; see [theme.md → Display](./theme.md#display).

`spend_window` sets the cockpit and provider headline row: `"session"` starts at the latest human activity burst after a five-hour idle gap and is the default (loop-fired turns count inside the resulting window but do not start or bridge it), `"24h"` keeps a trailing-24-hour window, and `"today"` starts at calendar midnight in `timezone`.

`[sidebar.keys]` rebinds movement and width keys: `narrower`, `wider`, `up`, `down`, `top`, `bottom`, `worktree_up`, `worktree_down`, `page_up`, `page_down`, `screen_top`, and `screen_bottom`. Each value is a space-separated list of alternate chords, and the first chord shows in the `?` help overlay. Chords use optional `ctrl`/`control`/`c` and `alt`/`meta`/`m` modifiers with `+` or `-`, case-sensitive single characters (`H` differs from `h`), or named keys: `up`, `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown`, `enter`, and `space`. Defaults use `a`/`d` to step the pane narrower/wider and keep Vim movement plus arrow and page keys: `k/up`, `j/down`, `g/G`, `K/J`, `ctrl+b/pageup`, `ctrl+f/pagedown`, and `H/L`. The sidebar uses an explicit `[theme.display].width_percent`, otherwise 30% on views wider than 240 columns and 25% at or below, capped at `max_cols`. A width selection persists for the room across launch, reload, and session rebirth until its runtime state is reset, and this explicit selection outranks the configured or width-keyed percentage and `max_cols`. Configured chords resolve before fixed action keys, so a rebind may intentionally shadow a filter or action. Fixed filters use `A` for all and `s` for success/done. tmux's default prefix consumes `Ctrl+b` before the sidebar sees it, so `PageUp` is the portable default page-up key there.

## agents.toml: profiles, teams, and worktrees

This file configures what `rimz agents <spec>` can launch and how worktrees are cut.

### Agent profiles, commands, and teams

Reusable agent **profiles**, raw **command** panes, and named **teams** live in `agents.toml` under `[agents.profiles]`, `[agents.commands]`, and `[agents.teams]`.

```toml
[agents]
placement = "auto"

[agents.profiles.claude-slim]
agent = "claude"                                       # a built-in kind, or another profile
effort = "low"
budget = "5"
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
leader = "planner"
layout = "planner/reviewer,coder+term"

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

#### Profiles

A profile is a named agent preset, addressable as `@<name>` once it is running. `agent` is its base, a built-in kind (`claude`, `codex`, …) or another profile that resolves to one, and the remaining **override fields** layer on top: `mode` (`auto` | `ask` | `plan` | `yolo`), `model`, `effort`, `budget`, `system-prompt-file`, `append-system-prompt-file`, and raw `args`. `budget = "5"` caps the session and `budget = "20/day"` resets at the configured local day boundary. These same override fields recur wherever you preset an agent (profiles, team roles, and loop tasks), so the template and `rimz config get` carry the current per-field defaults. A provider flag repeated in raw `args` is reconciled at launch: the typed field or launch flag wins and RimZ warns when its value differs, while a model set only in `args` becomes the launch model and suppresses the adapter default.

Inheritance flattens at launch to one concrete adapter kind, and **the nearest set value wins for every field, including `args`**: a child that sets `args` replaces the base `args` rather than appending. `system-prompt-file` gives the profile its own voice; `append-system-prompt-file` keeps the adapter's base prompt and adds rules where the adapter supports it. A `~` expands to home and a relative path roots at the config file, so a prompt file points at the same file wherever the profile launches; each file must exist at launch, and a missing one fails with the path to fix. A field the resolved adapter has no flag for fails the launch and names the field to remove. Command-line `--model`, `--effort`, `--budget`, `--system-prompt-file`, and `--append-system-prompt-file` render after the profile and override it for that launch.

A profile may be named like a kind: `[agents.profiles.claude]` overrides the base for bare `claude`, for profiles that set `agent = "claude"`, and for virtual cells like `claude-auto` and `claude-ping`.

#### Commands

`[agents.commands]` entries are bare strings, shell-split and run as raw command panes. They are launch shortcuts, not agents, so they take no profile fields and answer to no `@` handle. A command may shadow a cell word like `claude` to set a local default for that word.

#### Teams

A team is an ordered `roles` list that feeds `rimz agents <name>`; each role binds a role name to a configured profile or registered agent kind and may set any of the same **override fields** (replacing, like profiles). A same-named machine profile overrides the kind's implicit base, so `profile = "claude"` uses `[agents.profiles.claude]` when present and bare Claude otherwise. Each member answers to `@<role>` in that channel. `leader` names the role that receives a trailing launch prompt and defaults to the first declared role. `rimz agents <team>.<role>` launches one declared role with the same identity it has inside the full team. By default a multi-role team opens left to right as one side-by-side column per role in one tab; a one-role team follows the single-cell placement policy. An optional `layout` uses the inline shape grammar (comma = column, plus = tiled row, slash = Zellij stacked row with tmux tiling), resolving declared role names first and then falling back to roleless cells. The built-in `peer` team is the roleless `claude,codex`. Building a team from scratch is walked in [teams.md](./teams.md).

#### Inline specs and cell resolution

An inline spec like `rimz agents "claude,codex+term"` keeps the same shape grammar: commas split columns, plus signs tile rows, and slashes stack rows as a Zellij stack while tmux tiles them. Each cell resolves in this order:

1. `[agents.commands]`,
2. `[agents.profiles]`,
3. built-in `term`,
4. registered agent kinds,
5. adapter-supported virtual `<kind>-<mode>` and `<kind>-ping` cells (`claude-auto`, `codex-ask`, `codex-yolo`, `claude-ping`, …).

Profiles and roles become addressable handles, so they must not shadow `@all`, agent kinds (`@claude`), kind ordinals (`@claude-2`), or the pane and channel sigils (`:`, `#`). Profile, command, and team names also reserve the `agents` subcommand verbs `list`, `ls`, `show`, `stop`, `focus`, `wait`, `term`, and `exec`. A config that still uses a removed table fails fast naming the rename rather than silently dropping it: `[tab]` (with its `[tab.keywords]`/`[tab.layouts]` children) → `placement` under `[agents]` plus `[agents.teams]`; `[agents.aliases]` → `[agents.profiles]` and `[agents.commands]`; `[agents.layouts]` → `[agents.teams]`. The room degrades to defaults with a warning while `rimz config` and `rimz doctor` print the precise rename.

#### Placement

```toml
[agents]
placement = "auto"   # "auto" | "pane" | "tab"
```

`placement` sets where a launch lands when neither `--new-pane` nor `--new-tab` is passed. `auto` (the default) runs a one-cell non-worktree launch in the current pane and opens a new tab for a worktree, named-channel, or multi-cell launch; `pane` splits a new pane for a one-cell non-worktree launch and otherwise opens a tab; `tab` always opens a tab. The CLI side of placement is in [agents.md → Channel, worktree, and placement](../reference/cli/agents.md#channel-worktree-and-placement).

### Worktrees

```toml
[agents.worktree]
dir = "../{repo}-worktrees"
base = "fresh"
```

`rimz worktree` and `rimz agents --worktree` create RimZ-owned Git worktrees here. A relative `dir` resolves from the repository root and `{repo}` expands to the root basename. `base = "head"` branches from local `HEAD`, `base = "fresh"` branches from `origin/HEAD`, and any other string is passed to Git as the base ref. Seeding untracked files into a new worktree and symlink-sharing directories are committed, repo-level concerns covered in the [worktrees guide](./worktrees.md); the seeding, symlink, and cleanup mechanics are in [worktrees.md](../internals/harness/worktrees.md).

## loop.toml: scheduled turns

### Loop tasks

```toml
default-timeout = "2h"   # scheduled agent turns without a task timeout

[tasks.morning]
agent = "claude-ping"     # `<kind>-ping` primes a provider window
prompt = "ping"
root = "/home/you/code/app"
at = "07:00"             # 24h time in the configured timezone
every = "weekday"        # day | weekday | weekend | mon,wed,fri

[tasks.pr_watch]
agent = "codex"
prompt = "check CI on the release PR"
root = "/home/you/code/app"
every = "15m"
mode = "auto"
check = "cargo test"
on = "fail"              # fail | success
verify = "cargo xtask gate"
max-attempts = 3
max-strikes = 3
surplus = "1.5x"
surplus-after = "3d"

[tasks.ci_green]
prompt = "CI is green; merge the PR"
root = "/home/you/code/app"
every = "2m"
check = "gh run watch --exit-status"
on = "success"
deadline = "2026-07-01T12:00:00Z"

[tasks.ci_green.wake]
kind = "claude"
session = "sess-abc123"
handle = "@planner"

[tasks.self_wake]
prompt = "resume the review: inspect the latest comments and fix the next blocking item"
root = "/home/you/code/app"
at = "09:30"

[tasks.self_wake.wake]
kind = "claude"
session = "sess-abc123"
handle = "@planner"
```

Loop tasks live in `~/.config/rimz/loop.toml` under `[tasks.<name>]`; shared project tasks use the same shape in `<repo>/.rimz/config.toml`, are trust-hashed, and stay inert until `rimz trust grant`. The scheduling model (shapes, watchdogs, pings, self-wakes) is [loops.md](./loops.md); this section is the field shape.

`default-timeout` bounds scheduled supervised turns whose task omits `timeout`; it accepts positive `s`, `m`, `h`, and `d` durations and defaults to `2h`. Set it with `rimz config set loop.default-timeout 3h`. Task-specific `timeout` wins, and a manual `rimz loop fire` without one remains unbounded.

Each task chooses `agent`, `wake`, `check`, or `check` plus one agent action:

- `agent` drives one supervised turn for a single agent cell on a calendar, interval, cron, or one-shot schedule. A `<kind>-ping` agent is the window-primer: it skips when that provider's budget window is already counting down, and takes a short prompt like any spawn task.
- `[tasks.<name>.wake]` pins delivery to one live agent session through the message path: `kind` supports hook preflight, `session` is the durable target, and `handle` is display-only.
- `check` runs a shell command at the task root before the agent action; `on = "fail"` wakes on non-zero exit or timeout, `on = "success"` on zero exit. Check output is appended to the agent prompt when the guard fires.
- `verify` runs a shell command after a spawned agent turn and re-prompts that same supervised session on failure; `max-attempts` is the total agent-turn cap and defaults to `3`.
- `max-strikes` auto-pauses the task after that many consecutive failed or no-progress fires, defaults to `3`, and accepts `0` to disable; `rimz loop resume` clears the machine-local counter.
- `deadline` is normally written by `rimz loop add --until 30m` into the instance state store for poll-until tasks, not hand-authored in `loop.toml`.
- `budget` caps each spawned supervised run; `budget-per-day` requires it and skips a fire when today's recorded task spend, plus the next run's cap, would exceed the daily amount.
- `surplus` requires forward headroom in the provider's longest running budget window; `surplus-after` adds an elapsed floor and implies at least `1.0x` headroom when used alone. Both fields apply to `agent` and `wake` actions and fail closed without a complete window reading; the headroom model is [budgets → spend the provider surplus](./budget.md#spend-the-provider-surplus).

Field notes:

- Calendar and cron wall-clock fields resolve in the top-level `timezone`, falling back to the system zone when unset.
- Machine tasks carry a `root`: `rimz loop add` writes an absolute path, and a hand-edited `~` or relative root is normalized before room matching, firing, and display.
- Project tasks run at the project root implicitly, resolve `prompt-file` and `system-prompt-file` relative to `.rimz/`, reject `root`, `wake`, and `deadline`, and require `every` or `cron` because one-shots are machine state.
- Trusted project tasks win over same-named machine tasks and state instances; an untrusted or stale project task stays visible but inert, so a same-named machine task keeps running until grant. `rimz loop add --project` writes `.rimz/config.toml`, and removing or renaming a project-owned task edits that file and prints the `rimz trust grant` follow-up.
- RimZ-generated one-shots, self-wakes, and poll-until instances live in `~/.local/state/rimz/loop-instances.json` rather than this file.

The full model is in [harness.md → Scheduled turns](../internals/harness/harness.md#scheduled-turns-loop), and the CLI is in [loop.md](../reference/cli/loop.md).

## theme.toml: appearance and pets

The sidebar's palette, glyphs, animations, color depth, color stops, and pets live in `theme.toml`, documented in full in [theme.md](./theme.md). The pointers below cover the settings that touch sidebar behavior and the pet selector.

### Pets

```toml
[theme.pets]
enabled = false
pet = "rocky"
glyphs = "auto"
# cell_aspect = 2.5
voice = true
```

An opt-in animated companion in the provider dashboard. The first-run and setup pet question writes `enabled = true` for the default `rocky` pet. Full setup is in the [pets guide](./pets.md); render mechanics, cache layout, and sheet geometry are in [pets.md](../internals/sidebar/pets.md).

| key | does |
| --- | --- |
| `enabled` | turns the dashboard pet on |
| `pet` | selects a built-in, HTTPS, local-sheet, or petdex pet |
| `glyphs` | chooses `auto`, `pixel`, or `sextant` rendering; `[theme.display] pixel = "off"` overrides pixel choices |
| `cell_aspect` | overrides terminal cell height/width for sextant correction; set it for tall or short fonts under Zellij, for example with `rimz config set theme.pets.cell_aspect 2.5` |
| `voice` | toggles canned captions on pet-action changes |

### Sidebar bands

The agent-card context meter and the provider budget bar interpolate across color stops you can tune. Both are theme settings (`[theme.display.context_meter]`, `[theme.display.budget_bar]`); `[theme.display] pixel = "auto" | "off"` is the master kitty-graphics switch for the context meter and pets. The model, requirements, fallbacks, and shipped numbers are in [theme.md → Display](./theme.md#display).

### Provider dashboard

Which providers appear, their order, and their brand styling are theme and discovery settings (`[theme.display] provider_tabs` / `provider_list` / `max_provider_blocks`, and `[theme.providers.<kind>]`). The layout model is in [theme.md → Display](./theme.md#display) and the styling fields in [theme.md → Provider styling](./theme.md#provider-styling); account and budget sourcing is in [providers.md](../internals/agents/providers.md).

## Project config

The committed `<repo>/.rimz/config.toml` declares the workspace shape a team shares. RimZ computes the executable-surface trust hash from it, and on a trusted workspace it injects each `[[agents]]` `env` table into that agent's process at launch, applies top-level `[profiles]` and `[agents.teams]` to `rimz agents` launches, and loads `[tasks]` for `rimz loop`. Use one `agents` shape per project config: `[[agents]]` for env entries, or `[agents.teams]` for shared teams. Applying the declared hooks and agent launch command is planned project-config behavior. Room layout is per-machine config: a project config carrying a `[layout]` table is refused with the fix to move it to `$XDG_CONFIG_HOME/rimz/config.toml`. RimZ's own [`.rimz/config.toml`](../../.rimz/config.toml) is a living project-task example; its repository sync task assumes push rights on the remote.

```toml
[[agents]]
name = "claude"
launch_command = "claude"
env = { CLAUDE_CODE_DISABLE_AGENT_VIEW = "1" }

[[hooks]]
event = "PreToolUse"
command = "notify-send rimz"

[tasks.morning-codex-ping]
agent = "codex-ping"
prompt = "ping"
at = "08:00"
every = "day"
```

Command-running fields enter the trust hash, so a clone with project config reads `untrusted` until `rimz trust grant` pins the current surface on this machine. A trusted repo profile, team, or task overlays machine config and wins on a name collision; a repo profile may inherit only another repo profile or a built-in kind, and a repo team role may bind only a repo profile, keeping the hashed surface closed and machine-independent. An `untrusted` or `stale` workspace refuses a launch or project-only task run that would consume project config, with the `rimz trust grant` fix; a same-named machine task continues to run, `rimz loop list` and `rimz loop show` still display project tasks with their trust state, and a `stale` report shows a field-level diff of what changed since the grant, so the re-grant is informed. The hash contract, stored surface, and launch-time enforcement are in [trust.md](../internals/harness/trust.md); the threat model is in [security.md](./security.md).

## Sidecars and privacy

Notification handlers, remote aliases, and trust records each have their own reference: [notifications.md](../internals/sidebar/notifications.md), `rimz remote` ([remote CLI](../reference/cli/remote.md#remote-rooms)), and `rimz trust` ([trust.md](../internals/harness/trust.md)).

Payload-fidelity and retention controls (`[privacy] payload_mode`) are a planned project surface. The design and intended keys are in [security.md](./security.md), and the hook boundary they will govern is in [agent.md → The adapter boundary](../internals/agents/model.md#the-adapter-boundary).
