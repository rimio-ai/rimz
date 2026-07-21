# Set up your machine

RimZ runs with zero configuration, and one setup pass makes it a comfortable daily driver — on a laptop or on the remote server you SSH into. Picking up where [installation](./installation.md) ends, you'll:

- initialize the per-machine config,
- install or refresh the agent hooks that let the sidebar see your agents,
- check truecolor and Nerd Font support independently, and preview a pet,
- switch on the hands-off behaviors that keep agents working while you're away,
- and give your own Zellij or tmux a baseline worth keeping.

The fast path is three commands and a room. The rest of the page is what each step does and the settings worth choosing while you are here.

```sh
rimz setup            # detect the machine, write config, choose hooks and appearance
rimz hooks install    # wire every detected agent's hooks into RimZ
rimz doctor           # confirm the machine is ready
cd ~/code/your-project && rimz
```

## Initialize the config

`rimz setup` prints a first-run report — the selected multiplexer, workspace root, trust state, config path, detected agent binaries, and hook install status — and writes any missing per-machine config under `~/.config/rimz/`. On an interactive terminal it also:

- offers to keep an existing config and refresh it against the current templates,
- offers one summarized install or refresh for every detected agent with missing hooks or a stale RimZ-owned whole-file integration,
- shows separate live truecolor and Nerd Font probes,
- previews the configured sidebar pet before asking whether to enable it, and
- offers to enable auto-continue and automatic Codex reset-credit redemption together.

The hook summary names every affected file and points to `rimz hooks install --dry-run` for the exact unified diff before you consent. The first `rimz` run on a terminal asks the same hook, color, glyph, pet, and automation questions when it creates the config. `rimz setup --yes` takes the non-interactive path — merge existing files, write missing ones, no hook installs, upgrades, trust grants, appearance changes, or automation opt-ins — which suits a server provisioning script.

Four files carry the settings this guide touches:

| File | Owns |
| --- | --- |
| `~/.config/rimz/config.toml` | room behavior: resume, auto-continue, smart compaction, notifications, multiplexer room overrides |
| `~/.config/rimz/theme.toml` | sidebar appearance: scheme, color depth, glyphs, pets |
| `~/.config/rimz/agents.toml` | agent profiles, teams, worktree defaults, attention timing |
| `~/.config/rimz/loop.toml` | scheduled loop tasks: recurring turns, watchdogs, self-wakes |

Every key ships commented with its default and an inline note, so the generated template is the field reference:

```sh
rimz config init --print                     # every key, its default, and what it does
rimz config get                              # the whole effective config as TOML
rimz config set theme "Catppuccin Mocha"     # edit one dotted key in the owning file
```

A commented line keeps following the defaults shipped by future RimZ versions; uncommenting makes it this machine's override. `rimz config set` routes a dotted key to the file that owns it, validates the value, and writes durably. The config model — tiers, merge order, and every behavior section including notifications — is in the [configuration guide](./configuration.md).

## Shell completion

Source RimZ's completion registration from your shell startup file:

```sh
# ~/.bashrc
source <(COMPLETE=bash rimz)

# ~/.zshrc
source <(COMPLETE=zsh rimz)

# ~/.config/fish/config.fish
COMPLETE=fish rimz | source
```

Completion covers the static command and flag surface plus current room data such as live `@handles`, queued `msg_` ids, loop tasks, launch specs, worktrees, channels, sessions, remote aliases, and config keys. Source the registration at shell startup instead of caching its output so it stays compatible when RimZ upgrades.

## Install agent hooks

Hooks are how a running agent reports to the room: turn starts and ends, permission prompts, and blocking questions reach the sidebar through hook events. Install them into every detected agent's per-user config:

```sh
rimz hooks install --dry-run    # per-agent summary plus a unified diff; writes nothing
rimz hooks install              # every detected installable agent (claude, codex, amp, copilot, kimi, pi, opencode, antigravity, cursor, droid, qwen, grok)
rimz hooks install claude       # one agent kind
```

Structured installs preserve existing user hooks, and whole-file Pi and OpenCode integrations replace only a file carrying RimZ's first-line `_rimz_managed` ownership marker; an unmarked file remains user-owned and installation refuses to overwrite it. Each report names every file it edits and the undo (`rimz hooks uninstall [AGENT]`). For agents with a statusline, RimZ wraps the command so the sidebar reads live context, and restores yours on uninstall. Cursor shows and commits its hook file and CLI statusline config as one rollback-safe two-file operation. The first `rimz` run and interactive `rimz setup` compare marked whole-file integrations with the source embedded in the running RimZ build, include stale sources in the same summarized consent prompt as missing hooks, and point to `rimz hooks install --dry-run` for exact diffs. Some agents gate hooks behind their own trust prompt; when one reports installed-but-untrusted hooks, `rimz doctor` prints the exact fix. Command detail is in [the hooks CLI](../reference/cli/hooks-trust.md#agent-hooks).

Grok installs one passive global JSON file at `${GROK_HOME:-~/.grok}/hooks/rimz.json`. It preserves unrelated Grok hooks and leaves the blocking `PreToolUse` decision channel to the native TUI.

Newly-born rooms also give direct Copilot launches a private metadata-only OTel file under that workspace's runtime directory, with message-content capture disabled. A room that was already alive when RimZ gained this support keeps its original environment; rebirth it after install or upgrade before expecting a plain `copilot` typed in the work shell to show model and token composition.

Kiro CLI 2.12.1 v3 does not execute its documented standalone hook configs, so RimZ leaves Kiro out of hook installation. Its validated stock local session store supplies transcript and live display without a setup step.

## True color

The sidebar and agent TUIs render best at 24-bit color, and three layers decide whether you get it:

- **Your terminal.** Pick one that advertises truecolor — Ghostty, WezTerm, Kitty, and Alacritty all do. This is the whole story for local terminal-attached Zellij, which inherits color support from the terminal it runs in.
- **RimZ.** `[theme] mode = "auto"` (the default) emits truecolor whenever `COLORTERM` or the `$TERM` terminfo advertises it. Inside a RimZ tmux room, RimZ stamps `COLORTERM=truecolor` at birth when the launching terminal advertises it, so `auto` resolves to truecolor despite tmux's `tmux-256color` default; `rimz remote` carries the same stamp over SSH when the local terminal advertises it, and `rimz web` stamps browser-born rooms because xterm.js renders 24-bit color. Pin `mode = "truecolor"` for rooms born before this support or for other stripping hops.
- **Your own tmux sessions.** tmux needs `default-terminal` and the RGB overrides in [the tmux essentials below](#tmux) for its own color handling outside RimZ rooms.

With a Nerd Font in the terminal, one line upgrades the glyphs too:

```toml
[theme]
style = "modern"       # truecolor + Nerd Font icons; "default" = auto color + Unicode
# mode = "truecolor"   # force RGB when auto-detection is defeated
```

Schemes, palette slots, and the full display model are in [theming](./theme.md).

First-run setup puts each capability beside its own question: a color sweep checks truecolor, then sampled sidebar icons check the Nerd Font. The detected terminal capability supplies the truecolor default; answers that change the effective defaults write `theme.mode` and `theme.glyphs.set` independently, so either capability can fall back without disabling the other.

## Pets

Pets add a small animated companion to the sidebar's provider dashboard, following the fleet's state. Setup renders the configured pet at the same pixel or cell-art tier the dashboard will use, then asks whether to enable the default `rocky`; when the best-effort preview is unavailable, the question remains. One command turns a pet on later:

```sh
rimz config set theme.pets.enabled true
rimz list-pets                             # preview every built-in as cell art
```

Picking a different pet, [petdex.dev](https://petdex.dev/) installs, crisp pixels vs cell art, bring-your-own sprite sheets, and the privacy boundary are in the [pets guide](./pets.md).

## Keep the fleet moving

RimZ routes attention by default and leaves every decision to you. Four opt-in behaviors keep agents productive through reboots, rate limits, and full context windows — the difference between a fleet that waits for you and one that only needs you for real decisions. All four live in per-machine config.

### Resume agents after a reboot

```toml
[resume]
on_rebirth = true    # already the default
max = 128            # cap on agents one rebirth relaunches
```

When RimZ rebuilds a room after a reboot or a multiplexer crash — a *rebirth* — it offers to bring back the prior agents from its durable records, each restored agent starting idle in its worktree tab. This is on by default; the knobs bound it, and `rimz start --no-resume` or `on_rebirth = false` gives a clean empty room instead. The mechanics are in [sidebar internals → Resume on rebirth](../internals/sidebar/sidebar.md#resume-on-rebirth).

### Auto-continue interrupted turns

```toml
[resume]
auto_continue = true                       # off by default
# auto_continue_backoff_secs = [180, 300]  # first retry after 3m, then every 5m
# auto_continue_max_retries = 12           # stop after ~58 minutes of retries
# auto_continue_text = "continue"          # the nudge typed into the parked pane
```

A turn that dies mid-flight — a rate limit, a spend limit, a provider overload, or a transient API error such as a stalled stream, timeout, or dropped connection — *parks* its agent: the agent sits waiting for a nudge to continue. `auto_continue` picks those turns back up by typing `continue` into the pane through the same audited path as `rimz message`. Rate-limit and spend-limit parks resume when the provider's budget window resets; overload and transient-error parks retry on the backoff ramp until the retry cap. The model is in [provider internals → Auto-continue](../internals/agents/providers.md#auto-continue).

### Compact before the prompt lands

```toml
[harness]
smart_compact = "200k"   # occupied-token count; a percentage of the window such as "70%" works too
```

`smart_compact` makes `rimz message` compact-first: when the target agent's context window has reached the threshold, RimZ submits the agent's `/compact` ahead of your text so the prompt lands against a fresh window instead of dying at the context ceiling. Unset, compaction stays opt-in per send through `rimz message --smart-compact`. The mechanics are in [message internals → Smart compaction](../internals/harness/messaging.md#smart-compaction).

### Put a turn on a schedule

Scheduled work lives in its own file, `~/.config/rimz/loop.toml`, one `[tasks.<name>]` entry per job:

```toml
[tasks.nightly-audit]
agent = "codex"
prompt = "Audit the dependency lockfile for advisories and open an issue for anything actionable"
root = "/home/you/code/app"      # absolute project root whose room hosts the turn
at = "00:30"                     # 24h time in the configured timezone
every = "day"
```

`rimz loop add` writes the same entries, so you never have to hand-edit unless you want to. Two things about the model are worth knowing before you write your first task: a task fires only while a room for `root` is open, since the room's sidebar keeps the clock and there is no separate daemon, and a task that repeats needs `every` or `cron` — a bare `at` fires once and retires itself.

The same table carries watchdogs and self-wakes, where an agent turn runs on an interval behind a shell check such as `cargo test` or `gh run watch`. What you would schedule and why is [loops](./loops.md); the field-by-field shape is [configuration → Loop tasks](./configuration.md#loop-tasks), and every flag is in [the loop CLI](../reference/cli/loop.md).

## Configure your multiplexer

RimZ sets the room's behavior for you: on every session birth and reattach it applies the options agents need — locked mode and single-click sidebar jumps on Zellij; mouse, focus events, OSC passthrough, CSI-u soft-newline keys, and clipboard on tmux; 100k-line scrollback on both — so a freshly installed multiplexer works without touching its config. Your own `~/.config/zellij/config.kdl` or `~/.tmux.conf` owns everything RimZ leaves alone — the theme, true color, copy-mode, the status bar, and your keybindings — inside the room and in every session you run outside RimZ.

The fastest path to a good config of your own is copying the shipped baselines under [examples/](../../examples/README.md) — Zellij as a complete starting file, tmux as sourceable modules:

```sh
# From the rimz checkout
cp examples/zellij/config.kdl ~/.config/zellij/config.kdl        # Zellij: the whole baseline, one file
printf 'source-file %s\n' "$PWD"/examples/tmux/{agents,quality-of-life,zellij-keys,theme-tokyonight}.conf >> ~/.tmux.conf
```

The essentials below are the settings that matter most for agent sessions. The full walkthrough — recommended quality-of-life options, one `Alt`-chord keybinding set that drives both multiplexers, a themed status bar, and the room overrides in RimZ config — is [Zellij and tmux baselines](./multiplexer.md).

### Zellij

The file is `~/.config/zellij/config.kdl`; `zellij setup --check` validates your edits. These settings make a coding-agent session behave correctly — long output stays readable, the keyboard reaches the agent, and copied text lands where you expect:

```kdl
default_mode "locked"                  // hand typing straight to the agent; Ctrl+g enters Zellij
scroll_buffer_size 100000              // keep long agent output scrollable
mouse_mode true                        // scroll, select, and resize with the mouse
copy_on_select true                    // selecting text copies it
copy_clipboard "system"                // yank into the OS clipboard
support_kitty_keyboard_protocol true   // Shift+Enter and friends reach TUI agents
```

`default_mode "locked"` is the one that matters most: locked mode passes ordinary keystrokes to the focused pane, so an agent's TUI gets your input until you deliberately press `Ctrl+g` for a Zellij mode. RimZ already opens its room in locked mode; setting it here keeps your own sessions consistent and your muscle memory intact. Zellij inherits true color from the terminal it runs in, so there is no color flag to set.

### tmux

The file is `~/.tmux.conf` (or `~/.config/tmux/tmux.conf`); reload it with `tmux source-file ~/.tmux.conf`. Two groups: **true color** sets your terminal type and RGB passthrough (tmux needs this for its own color handling even though RimZ stamps its rooms), and the rest are behaviors RimZ applies inside its room — set them here so your own sessions behave the same.

```tmux
# True color + italics: advertise a color terminal, pass RGB and styles through.
set -g default-terminal "tmux-256color"
set -ga terminal-overrides ",*256col*:RGB,alacritty:RGB,wezterm:RGB"
set -ga terminal-features ",*:RGB,*:usstyle,*:clipboard"

# Behaviors a modern TUI agent relies on.
set -g  mouse on                 # scroll, select panes, resize
set -g  history-limit 100000     # long Claude/Codex output stays in scrollback
set -sg escape-time 0            # no ESC lag in helix, nvim, fzf, agent TUIs
set -ga terminal-features ",*:sync"   # atomic redraws for tmux sessions outside RimZ
set -g  focus-events on          # editors and agents see focus changes
set -g  allow-passthrough on     # let desktop notifications pass through tmux
set -s  extended-keys on             # distinguish modified Enter from Enter
set -s  extended-keys-format csi-u   # forward Shift+Enter / Alt+Enter as CSI-u
set -ga terminal-features "*:extkeys" # ask the outer terminal to send them
set -s  user-keys[240] "\e[27u"        # name Ghostty's modifier-less CSI-u Esc
bind-key -n User240 send-keys Escape   # normalize it back to plain Esc
bind-key -n S-Enter send-keys Escape "[13;2u"
bind-key -n M-Enter send-keys Escape "[13;3u"
set -g  set-clipboard on         # yank into the host clipboard over OSC52
```

Three of these earn a note; the block's comments carry the rest. `escape-time 0` removes the lag that otherwise makes `Esc` feel sticky in any full-screen TUI. The extended-keys trio plus the modified-Enter bindings let an agent's composer receive Shift+Enter and Alt+Enter as soft newlines while plain Enter still submits — on tmux 3.5.x this trades clean multiline paste while extended keys are active (use Ctrl+J or tmux 3.6+ for both); the `user-keys` pair keeps plain Esc clean on terminals such as Ghostty that answer the extended-keys request with modifier-less CSI-u. `allow-passthrough on` lets the desktop-notification bytes RimZ emits reach your terminal.

Copy-mode, stable window names, titled pane borders, smart splits, Zellij-parity keys, and the themed status bar continue in [Zellij and tmux baselines](./multiplexer.md#tmux).
