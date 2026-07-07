# Configuration

> See [DESIGN.md](../../DESIGN.md#invariants) for the invariants this doc operationalizes.

Rimz runs with zero configuration. Everything here is optional tuning — start the room, and you can come back to add a theme, a launch profile, or a notification route once you know what you want to change.

Configuration comes in two tiers. **Per-machine** config under `~/.config/rimz/` is yours: your terminal, accounts, notifications, theme, and launch shortcuts. It stays personal, uncommitted, and outside the project trust hash. **Project** config at `<root>/.rimz/config.toml` declares a shape a team shares through the repo; Rimz trust-tracks it so a clone can review the executable surface before it runs.

## The files

| File | Tier | What it holds |
| --- | --- | --- |
| `~/.config/rimz/config.toml` | per-machine | room behavior: accounts, notifications, remote-control launch, multiplexer defaults, resume, smart-compact, optional Sentry |
| `~/.config/rimz/theme.toml` | per-machine | sidebar appearance: palette, slots, glyphs, animations, provider styling, pets ([theme.md](../guide/theme.md)) |
| `~/.config/rimz/agents.toml` | per-machine | agent profiles, command cells, teams, worktree defaults, attention timing |
| `~/.config/rimz/loop.toml` | per-machine | durable recurring loop task definitions and scheduled command checks |
| `~/.agents/agents/<name>/agent.toml`, `~/.agents/teams/<name>/team.toml` | per-machine | drop-in profile and team fragments merged under `agents.toml` |
| `~/.config/rimz/remote.toml` | per-machine | named SSH room aliases (`rimz remote`) |
| `~/.config/rimz/projects/<id>/trust.toml` | per-machine | project executable-surface trust grant (`rimz trust`) |
| `<root>/.rimz/config.toml` | committed | declared workspace shape and shared loop tasks, trust-tracked |
| `<root>/.worktreeinclude` | committed | globs for untracked files to seed into new worktrees |
| `<root>/.worktreelink` | committed | directories to symlink-share into new worktrees |

Per-machine settings load leniently: a missing file is the default config, unknown keys are ignored so an older binary tolerates a newer file, and a file Rimz cannot parse falls back to built-in defaults with a startup warning, so a broken config never blocks the room. `rimz config` and `rimz doctor` report the precise error and the fix.

## Get started

```sh
rimz                       # first start writes missing config, asks setup questions, opens the room
rimz setup                 # detect this machine and write or refresh config
rimz config init           # write config.toml, theme.toml, agents.toml, and loop.toml
rimz config init --print   # print the commented templates without writing
```

Most people run `rimz` inside a project or `rimz setup` once, then edit the few lines they care about. First start on an interactive terminal writes missing per-machine config, offers hook install, asks the live glyph probe, and asks whether to enable a pet; non-interactive first start writes the same defaults without prompting. Interactive setup repeats those questions after the config refresh step. `rimz setup --yes` writes or merges files without hook, trust, or appearance changes.

**The generated template is the field reference.** Every persisted section and default scalar ships as commented TOML with an inline note, so `rimz config init --print` is the authoritative, always-current list of keys and defaults. This page explains the *model and the knobs that are easy to misread*, and leaves the full field list to the template. Leaving a line commented keeps following the defaults shipped by future Rimz versions; uncommenting makes it this machine's override.

The per-machine files map to the in-memory config the same way: core behavior from `config.toml`, appearance from `theme.toml`, agent behavior from `agents.toml`, and durable loop definitions from `loop.toml`. `rimz config set` routes a dotted key to the file that owns it.

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
3. per-machine config (`~/.config/rimz/{config,theme,agents,loop}.toml`),
4. CLI flags and `RIMZ_*` environment variables.

Today the per-machine layer is live, CLI/env overrides apply where the commands define them, and the project layer is read for trust. **Launch names and loop task names invert this on purpose once trusted:** trusted project `[profiles]`, `[agents.teams]`, and `[tasks]` overlay machine config and win on a name collision, so a repository can pin the executable surface it hashes (see [Project config](#project-config)).

Rimz also discovers drop-in fragments under `~/.agents/agents/<name>/agent.toml` and `~/.agents/teams/<name>/team.toml`, in the same `[agents.profiles]` / `[agents.teams]` shape as `agents.toml`; an entry in `agents.toml` overrides a fragment of the same name. Validation runs on the merged view, so `agents.toml` teams can reference profiles defined in fragments. Set `RIMZ_AGENTS_HOME` to relocate the fragment root.

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

### Profiles

A profile is a named agent preset, addressable as `@<name>` once it is running. `agent` is its base — a built-in kind (`claude`, `codex`, …) or another profile that resolves to one — and the remaining **override fields** layer on top: `mode` (`auto` | `ask` | `plan` | `yolo`), `model`, `effort`, `system-prompt-file`, `append-system-prompt-file`, and raw `args`. These same override fields recur wherever you preset an agent — profiles, team roles, and loop tasks — so the template and `rimz config get` carry the current per-field defaults.

Inheritance flattens at launch to one concrete adapter kind, and **the nearest set value wins for every field, including `args`** — a child that sets `args` replaces the base `args` rather than appending. `system-prompt-file` gives the profile its own voice; `append-system-prompt-file` keeps the adapter's base prompt and adds rules where the adapter supports it. A `~` expands to home and a relative path roots at the config file, so a prompt file points at the same file wherever the profile launches; each file must exist at launch, and a missing one fails with the path to fix. A field the resolved adapter has no flag for fails the launch and names the field to remove. Command-line `--model`, `--effort`, `--system-prompt-file`, and `--append-system-prompt-file` render after the profile and override it for that launch.

A profile may be named like a kind: `[agents.profiles.claude]` overrides the base for bare `claude`, for profiles that set `agent = "claude"`, and for virtual cells like `claude-auto` and `claude-ping`.

### Commands

`[agents.commands]` entries are bare strings, shell-split and run as raw command panes. They are launch shortcuts, not agents, so they take no profile fields and answer to no `@` handle. A command may shadow a cell word like `claude` to set a local default for that word.

### Teams

A team is an ordered `roles` list that feeds `rimz agents <name>`; each role binds a role name to a profile and may set any of the same **override fields** (replacing, like profiles). Each member answers to `@<role>` in that channel. `rimz agents <team>.<role>` launches one declared role with the same identity it has inside the full team. By default multi-role teams open left to right as one side-by-side column per role in one tab; a one-role team follows the single-cell placement policy. An optional `layout` uses the inline shape grammar (comma = column, plus = tiled row, slash = Zellij stacked row with tmux tiling), resolving declared role names first and then falling back to roleless cells. The built-in `peer` team is the roleless `claude,codex`.

### Inline specs and cell resolution

An inline spec like `rimz agents "claude,codex+term"` keeps the same shape grammar: commas split columns, plus signs tile rows, and slashes stack rows as a Zellij stack while tmux tiles them. Each cell resolves in this order:

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

`placement` sets where a launch lands when neither `--new-pane` nor `--new-tab` is passed. `auto` (the default) runs a one-cell non-worktree launch in the current pane and opens a new tab for a worktree, named-channel, or multi-cell launch; `pane` splits a new pane for a one-cell non-worktree launch and otherwise opens a tab; `tab` always opens a tab. The CLI side of placement is in [agents.md → Channel, worktree, and placement](./cli/agents.md#channel-worktree-and-placement).

## Worktrees

```toml
[agents.worktree]
dir = "../{repo}-worktrees"
base = "fresh"
```

`rimz worktree` and `rimz agents --worktree` create Rimz-owned Git worktrees here. A relative `dir` resolves from the repository root and `{repo}` expands to the root basename. `base = "head"` branches from local `HEAD`, `base = "fresh"` branches from `origin/HEAD`, and any other string is passed to Git as the base ref. A committed `<root>/.worktreeinclude` lists globs for untracked files to copy into each new worktree, and `<root>/.worktreelink` lists directories to symlink-share. The seeding, symlink, and cleanup mechanics are in [worktree.md](../internals/harness/worktree.md).

## Loop tasks

```toml
[tasks.morning]
spec = "claude-ping"     # `<kind>-ping` primes a provider window
prompt = "ping"
root = "/home/you/code/app"
at = "07:00"             # 24h time in the configured timezone
days = "weekdays"        # daily | weekdays | weekends | mon,wed,fri

[tasks.pr_watch]
spec = "codex"
prompt = "check CI on the release PR"
root = "/home/you/code/app"
every = "15m"
mode = "auto"
check = "cargo test"
on = "fail"              # fail | success

[tasks.ci_green]
prompt = "CI is green; merge the PR"
root = "/home/you/code/app"
every = "2m"
check = "gh run watch --exit-status"
on = "success"
deadline = "2026-07-01T12:00:00Z"

[tasks.ci_green.bind]
kind = "claude"
session = "sess-abc123"
handle = "@planner"

[tasks.self_wake]
prompt = "resume the review: inspect the latest comments and fix the next blocking item"
root = "/home/you/code/app"
at = "09:30"

[tasks.self_wake.bind]
kind = "claude"
session = "sess-abc123"
handle = "@planner"
```

Loop tasks live in `~/.config/rimz/loop.toml` under `[tasks.<name>]`. Shared project tasks use the same `[tasks.<name>]` shape in `<root>/.rimz/config.toml`, are trust-hashed, and stay inert until `rimz trust grant`. Each task chooses `spec`, `bind`, `check`, or `check` plus one agent action. `spec` drives one supervised turn for a single agent spec on a calendar, interval, cron, or one-shot schedule. Bind-mode pins delivery to one live agent session and sends the prompt through the message path; `kind` supports hook preflight, `session` is the durable target, and `handle` is display-only. `check` runs a shell command at the task root before the agent action; `on = "fail"` wakes on non-zero exit or timeout, and `on = "success"` wakes on zero exit. Check output is appended to the agent prompt when the guard fires. `deadline` is normally written by `rimz loop add --until 30m` into the instance state store for poll-until tasks, not hand-authored in `loop.toml`.

Calendar and cron wall-clock fields resolve in the top-level `timezone`, falling back to the system zone when unset. A `<kind>-ping` spec is the window-primer: it skips when that provider's budget window is already counting down, and it takes a short prompt like any spawn task. Machine tasks carry a `root`; `rimz loop add` writes an absolute path, and hand-edited `~` or relative roots are normalized before room matching, firing, and display. Project tasks run at the project root implicitly, resolve `prompt-file` and `system-prompt-file` relative to `.rimz/`, and reject `root`, `bind`, `deadline`, and one-shots because those are machine-local state or would rewrite committed config on fire. Trusted project tasks win over same-named machine tasks and state instances; untrusted or stale project tasks stay visible but inert, so a same-named machine task keeps running until grant. `rimz loop add --project` writes `.rimz/config.toml`, and removing or renaming a project-owned task edits that file and prints the `rimz trust grant` follow-up. Rimz-generated one-shots, self-wakes, and poll-until instances live in `~/.local/state/rimz/loop-instances.json` rather than this file. The full model is in [harness.md → Scheduled turns](../internals/harness/harness.md#scheduled-turns-loop), and the CLI is in [agents.md → Schedule turns with loop](./cli/agents.md#schedule-turns-with-loop).

## Behavior settings

These tune how the room behaves. Each shows the shape; the template carries every key and default.

### Notifications

```toml
[notifications]
triggers = ["waiting", "failed"]
desktop = "auto"
sound = "bell"
remind_secs = 60
title = "Rimz: {{agent}} {{kind}}"
body = "{{task}}"
command = "ntfy publish rimz"

[[notifications.handler]]
name = "waiting-ntfy"
command = "ntfy publish --title {{title}} rimz {{body}}"
when = { kind = ["waiting"], worktree = ["feat/*"], handle = ["@planner"] }
```

Notifications are best-effort attention delivery over the sidebar inbox. `waiting`, `failed`, `paused`, and `success` rows become unread until read; `triggers` filters which newly-unread kinds raise a banner or handler command, while `running` and `idle` stay quiet. `desktop = "auto"` emits terminal OSC notifications under tmux and skips them under Zellij (which drops notification OSCs today); `sound = "bell"` writes a BEL byte and your terminal decides whether it is audible.

`title` and `body` are optional templates for agent-status and coalesced desktop/banner text. Templates substitute `{{kind}}`, `{{agent}}`, `{{handle}}`, `{{status}}`, `{{worktree}}`, `{{task}}`, `{{count}}`, and `{{unread}}`; `agent` and `handle` are the agent handles or roles joined for multi-agent notifications, and values unavailable for a notification kind render empty. Reminder and remote-link notifications keep their built-in text.

Each `[[notifications.handler]]` runs locally through `sh -c` when all present `when` clauses match. `kind` names notification kinds (`waiting`, `failed`, `paused`, `success`, `coalesced`, `reminder`, `link_lost`, `link_restored`), `worktree` glob-matches an agent branch/path, and `handle` glob-matches the agent handle or role; a leading `@` in a handle pattern is accepted as the usual address sigil. `command` templates may also use `{{title}}` and `{{body}}`, which are the rendered banner strings. Each substituted command value is shell-quoted as one token, so write `ntfy publish --title {{title}} rimz {{body}}`, not `--title "{{title}}"`. The legacy `command = "..."` key is shorthand for one unconditional handler, and every handler still receives `RIMZ_NOTIFY_TITLE`, `RIMZ_NOTIFY_BODY`, `RIMZ_NOTIFY_AGENT`, and `RIMZ_NOTIFY_KIND` in the environment; reminders also get `RIMZ_NOTIFY_UNREAD`. The full debounce/coalesce/remind model is in [notifications.md](../internals/sidebar/notifications.md).

### Remote control

```toml
[remote_control]
claude = false
codex = false
```

These opt this machine into background remote-control infrastructure shown in the `rimzd` daemon view. `claude = true` adds `claude remote-control` to the daemon column when `claude` is on PATH; `codex = true` ensures the managed standalone Codex daemon before the room opens (a `codex` CLI on PATH already adds the per-session app-server broker). `rimz start` refuses when an installed enabled host has a fixable misconfiguration, such as incompatible Claude version or settings. An enabled host whose agent is not installed is skipped so the room still starts; `rimz doctor` reports that advisory with the install fix. The mechanics are in [provider.md](../internals/agents/provider.md) and the security boundary in [security.md](../guide/security.md).

### Web access

```toml
[web]
enabled = true

[web.zellij]
base_url = "https://devbox.example/zellij"
auto_start = true
font = "JetBrainsMono Nerd Font Mono"
style_client = true
```

`[web] enabled` defaults to true and gates `rimz web open` plus `rimz remote connect --web`. Rimz always seeds Zellij's permission cache for its own presence plugin's pane-topology permissions; when web is enabled it also seeds the web-sharing permission so runtime browser sharing works without a one-time prompt. When disabled, web commands fail before room changes and tell you to change the config on the machine serving the room. `[web.zellij]` tunes browser access through Zellij's web server. `base_url` is the URL prefix Rimz prints for `rimz web open` and `rimz web url`, useful when a reverse proxy serves Zellij under a public host or path. `auto_start` lets `rimz web open` run `zellij web --start --daemonize` when the server is offline; set it to `false` when another supervisor owns the server. `style_client` lets Rimz write a generated Zellij `web_client` block on server start so the browser terminal uses `[theme]`; `font` sets that browser terminal font. These keys are per-machine and outside the project trust hash because they run no command and often name private hostnames or tunnels. Command details are in [web.md](./cli/web.md), and remote browser tunnels are in [remote.md](../internals/reach/remote.md#web-access).

### Daemon view

```toml
[daemon]
[[daemon.pane]]
command = "stats"

[[daemon.pane]]
command = "btop"
cwd = "/var/log"
```

`[daemon]` configures the `rimzd` daemon view's middle column, beside the sidebar and any managed hosts. Unset or empty keeps the built-in held live stats pane (`rimz stats --refresh --hold`). Listing `[[daemon.pane]]` entries replaces that default, so include `command = "stats"` when you want live stats plus extra panes. The reserved command token `"stats"` expands to the built-in stats argv; any other `command` is split into argv and run directly without a shell. `cwd` is optional: absent runs from the worktree root, absolute paths are used as-is, and relative paths are joined onto the worktree root. A running room reloads command and cwd edits when `config.toml` is saved; adding or removing `[[daemon.pane]]` entries changes the pane count and takes effect on room restart. A pane with an empty or unparseable command is skipped; if every configured pane is skipped, Rimz falls back to the built-in stats pane.

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
max = 128
auto_continue = false
auto_continue_backoff_secs = [180, 300]
auto_continue_max_retries = 13
auto_continue_text = "continue"
```

Resume covers two tenses. On a **rebirth after reboot or mux crash** (the machine rebooted since the room was last alive, or wrappers recorded positive lost-agent markers when the mux died), Rimz offers to recover prior agents from the durable rollup — the prompt defaults yes, non-interactive starts recover, and each restored agent starts idle in its worktree tab. Empty named channels still reopen on same-boot rebirths. `on_rebirth = false`, `--no-resume`, and `rimz reset` come up without agents; `max` bounds how many agents one birth relaunches and defaults to 128. While the room is **live**, `auto_continue` picks any parked turn back up by typing `auto_continue_text` through the same path as `message --steer`: rate-limit and spend-limit parks fire from the fused account budget's spent-window reset, while overload and transient API-error parks (stalled streams, timeouts, and connection drops) fire on the bounded retry ramp (`auto_continue_backoff_secs`, `auto_continue_max_retries`). Rate-limit, spend-limit, and overload records all stop after `auto_continue_max_retries`. The default backoff sends the first overload/transient retry 3 minutes after the marker, then every 5 minutes until about 63 minutes, then leaves the row parked. It is off by default. The rebirth path is in [sidebar.md](../internals/sidebar/sidebar.md#resume-on-rebirth) and the live path in [provider.md → Auto-continue](../internals/agents/provider.md#auto-continue).

### Smart compaction

```toml
[harness]
smart_compact = "70%"
```

`smart_compact` sets the default threshold for compact-first `message` sends — a percentage (`"70%"`) or an occupied-token count (`"120000"`). When an agent's context window has reached the threshold, Rimz submits its `/compact` ahead of your text so the prompt lands against a fresh window. Leave it unset to keep compaction opt-in through the per-command `--smart-compact` flag, which overrides this value. The mechanics are in [message.md](../internals/harness/message.md#smart-compaction).

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

This section applies only to Rimz builds compiled with the non-default `sentry` feature, and it is intentionally omitted from the generated per-machine config template. Set a `dsn` to report Rimz `warn!`/`error!` events and observed agent rate-limit/overload conditions to a Sentry project. With no `dsn`, reporting stays off and Rimz makes no network calls; without the feature, the block is inert. `RIMZ_SENTRY_DSN` and `RIMZ_SENTRY_ENVIRONMENT` override the config for one invocation, and `environment` defaults by build profile (an installed release reports as `production`, a dev or CI build as `development`). The DSN lives per-machine — never in committed project config — so a clone never inherits it; events carry low-cardinality tags (workspace, command, build, fault class, and agent/session when known) with the hostname and personal data withheld. The full telemetry surface is in [security.md](../guide/security.md#off-box-error-reporting) and the mechanics in [diagnostics.md](../internals/health/diagnostics.md#off-box-error-reporting).

## Multiplexer room options

Rimz applies room-scoped multiplexer settings when it creates or reattaches a session, so the room behaves the way agents need without editing your global Zellij or tmux config. The `[zellij]` and `[tmux]` tables tune those settings; `rimz config init --print` lists every key with its default, and the per-backend mapping is in [multiplexers.md](../internals/mux/multiplexers.md).

The `[mux]` table selects the default backend after the `--mux <name>` selection, its `--zellij`/`--tmux` shorthands, and active Zellij/tmux environment checks. Leave `default` unset to choose tmux when both backends are installed, or set it to `"zellij"` or `"tmux"` to require that backend. A configured backend that is not installed makes `rimz start` refuse with a fix message.

The two backends differ in how a key takes effect:

- **`[zellij]`** carries a few invariants Rimz always applies — locked mode, click-through with focus-follows-mouse off, no session serialization (Rimz owns rebirth), disabled session metadata, and native focused-pane splitting — plus optional keys (`pane_frames`, `copy_clipboard`, …) that apply only when you set them and otherwise fall through to your `~/.config/zellij/config.kdl`. The sidebar pane is always borderless so its hit-testing stays stable regardless of `pane_frames`.
- **`[tmux]`** applies its room invariants on every birth, each key carrying a Rimz default you can override. The pane-border keys are optional overrides; unset, they fall through to your `~/.tmux.conf` or tmux defaults just like `pane_frames`. Setting `pane_border_status` makes Rimz own `pane-border-format` too, blanking the sidebar border row and overriding any `~/.tmux.conf` format; unset, your tmux config wins and may title the sidebar. The table spans session, window, and server scope, including clipboard and rich-key handling, because tmux has no per-session form for those.

```toml
[mux]
default = "tmux"

[zellij]
pane_frames = true          # an optional override; unset, your config.kdl wins

[tmux]
## pane_border_status = "top"  # optional override; unset, your ~/.tmux.conf wins
```

To configure your *own* Zellij or tmux — the theme, true color, copy-mode, and keybindings Rimz leaves to you, and your sessions outside the room — see the [Zellij](../guide/setup.md#zellij) and [tmux](../guide/setup.md#tmux) baselines in the setup guide.

## Appearance and the sidebar

The sidebar's palette, glyphs, animations, color depth, color stops, and pets are theme settings in `theme.toml`, documented in full in [theme.md](../guide/theme.md). The settings below cover sidebar behavior plus the pet display selector.

### Sidebar Rendering

```toml
timezone = "America/New_York"

[sidebar]
focus_key = "Alt+p"
afk_after_secs = 900
trunk = "develop"
spend_window = "session"

[sidebar.keys]
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

`timezone` is an optional IANA zone for displayed transcript times, wall-clock scheduling, and the `"today"` spend cutoff; unset or unknown uses the system local zone. `focus_key` is the global multiplexer chord that focuses the sidebar from any pane and toggles back to your last working pane; both backends bind it at session birth, the default is `Alt+p`, and `""` or `off` registers nothing. `afk_after_secs` sets the input-idle window before the footer shows `zᶻ idle` on tmux, adding `· Nm` after the first minute; Zellij reports attach state only, so it shows `zᶻ away` on full detach regardless of this value. The default is 900 seconds (15 minutes). `trunk` is a preferred comparison branch for the worktree header's git stats, falling back to `main` → `master` → the remote default when it does not resolve. `[agents.attention]` tunes attention timing: `stalled_after_secs` is when a silent running agent escalates to the actionable `!` bucket (30 minutes by default), `inactive_after_secs` is when a card leaves hot work (one hour — the prompt-cache boundary, so a cold card reads as cold), and `archive_after_secs` is when a card parks below hot and warm work (24 hours by default). Set `archive_after_secs` greater than `inactive_after_secs`; lower values are lifted to the first second after the inactive window. The `[theme.display]` knobs that share this area — render cadence, sizing, `scrollbar`, and `card_density` — are theme settings; see [theme.md → Display](../guide/theme.md#display).

`spend_window` sets the cockpit and provider headline row: `"session"` starts at the latest human activity burst after a five-hour idle gap and is the default, loop-fired turns still count inside the resulting window but do not start or bridge it, `"24h"` keeps a trailing-24-hour window, and `"today"` starts at calendar midnight in `timezone`.

`[sidebar.keys]` rebinds movement keys only: `up`, `down`, `top`, `bottom`, `worktree_up`, `worktree_down`, `page_up`, `page_down`, `screen_top`, and `screen_bottom`. Each value is a space-separated list of alternate chords; the first chord is shown in the `?` help overlay. Chords use optional `ctrl`/`control`/`c` and `alt`/`meta`/`m` modifiers with `+` or `-`, case-sensitive single characters (`H` differs from `h`), or named keys: `up`, `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown`, `enter`, and `space`. Defaults keep Vim movement plus arrow and page keys: `k/up`, `j/down`, `g/G`, `K/J`, `ctrl+b/pageup`, `ctrl+f/pagedown`, and `H/L`. tmux's default prefix consumes `Ctrl+b` before the sidebar sees it, so `PageUp` is the portable default page-up key there.

### Pets

```toml
[theme.pets]
enabled = false
pet = "rocky"
glyphs = "auto"
voice = true
```

An opt-in animated companion in the provider dashboard. The first-run and setup pet question writes `enabled = true` for the default `rocky` pet. Full setup is in [theme.md → Pets](../guide/theme.md#pets); render mechanics, cache layout, and sheet geometry are in [pets.md](../internals/sidebar/pets.md).

### Sidebar Bands

The agent-card context meter and the provider budget bar interpolate across color stops you can tune. Both are theme settings (`[theme.display.context_meter]`, `[theme.display.budget_bar]`); the model and the shipped numbers are in [theme.md → Display](../guide/theme.md#display).

### Provider Dashboard

Which providers appear, their order, and their brand styling are theme and discovery settings (`[theme.display] provider_tabs` / `provider_list` / `max_provider_blocks`, and `[theme.providers.<kind>]`). The layout model is in [theme.md → Display](../guide/theme.md#display) and the styling fields in [theme.md → Provider styling](../guide/theme.md#provider-styling); account and budget sourcing is in [provider.md](../internals/agents/provider.md).

## Project config

The committed `<root>/.rimz/config.toml` declares the workspace shape a team shares. Rimz computes the executable-surface trust hash from it, and on a trusted workspace it injects each `[[agents]]` `env` table into that agent's process at launch, applies top-level `[profiles]` and `[agents.teams]` to `rimz agents` launches, and loads `[tasks]` for `rimz loop`. Use one `agents` shape per project config — `[[agents]]` for env entries, or `[agents.teams]` for shared teams. Applying the declared hooks and agent launch command is planned project-config behaviour. Room layout is per-machine config: a project config carrying a `[layout]` table is refused with the fix to move it to `$XDG_CONFIG_HOME/rimz/config.toml`. Rimz's own [`.rimz/config.toml`](../../.rimz/config.toml) is a living project-task example; its repository sync task assumes push rights on the remote.

```toml
[[agents]]
name = "claude"
launch_command = "claude"
env = { CLAUDE_CODE_DISABLE_AGENT_VIEW = "1" }

[[hooks]]
event = "PreToolUse"
command = "notify-send rimz"

[tasks.morning-codex-ping]
spec = "codex-ping"
prompt = "ping"
at = "08:00"
days = "daily"
```

Command-running fields enter the trust hash, so a clone with project config reads `untrusted` until `rimz trust grant` pins the current surface on this machine. A trusted repo profile, team, or task overlays machine config and wins on a name collision; a repo profile may inherit only another repo profile or a built-in kind, and a repo team role may bind only a repo profile, keeping the hashed surface closed and machine-independent. An `untrusted` or `stale` workspace refuses a launch or project-only task run that would consume project config, with the `rimz trust grant` fix; a same-named machine task continues to run, `rimz loop list` and `rimz loop show` still display project tasks with their trust state, and a `stale` report shows a field-level diff of what changed since the grant, so the re-grant is informed. The hash contract, stored surface, and launch-time enforcement are in [trust.md](../internals/harness/trust.md); the threat model is in [security.md](../guide/security.md).

## Sidecars and privacy

Notification handlers, remote aliases, and trust records each have their own reference: [notifications.md](../internals/sidebar/notifications.md), `rimz remote` ([getting started](./cli/getting-started.md#remote-rooms)), and `rimz trust` ([trust.md](../internals/harness/trust.md)).

Payload-fidelity and retention controls (`[privacy] payload_mode`) are a planned project surface. The design and intended keys are in [security.md](../guide/security.md), and the hook boundary they will govern is in [agent.md → The adapter boundary](../internals/agents/agent.md#the-adapter-boundary).
