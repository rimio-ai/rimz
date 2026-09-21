# Configuration

RimZ starts with no configuration: run it in a project and you have a working room. Everything you change afterwards is a file you own. Preferences are TOML under `~/.rimz/`, and reusable agents and teams are Markdown with YAML frontmatter. There is no config daemon holding your settings, no separate UI, and nothing to learn beyond TOML.

Change one line when you have a reason: pin a theme, save a launch profile, route a notification to your phone. This page opens with the settings most people touch and what changing one does to your disk, then maps where configuration lives and how the layers combine, then gives a section per file so you can jump to the one you are editing. On a fresh machine, [set up your machine](./setup.md) walks the first pass; this page is what it links into.

## Common changes

Most people touch a handful of settings and leave the rest on their defaults. Each line below is the whole change: `rimz config set` writes the value into the file that owns it, in place, so you never open an editor.

```sh
rimz config set theme "Catppuccin Mocha"       # sidebar palette; `rimz list-themes` shows the choices
rimz config set theme.style modern             # truecolor plus Nerd Font glyphs
rimz config set theme.pets.enabled true        # an animated companion on the provider dashboard
rimz config set resume.auto_continue true      # resume rate-limit and API-error parks on their own
rimz config set harness.smart_compact 200k     # compact before a message once context passes 200k tokens
rimz config set harness.idle_compact auto      # compact warm idle contexts while work may return
rimz config set notifications.triggers '["waiting", "failed"]'   # which cards raise a banner
rimz config set sidebar.focus_key "Alt+p"      # the chord that jumps to the sidebar from any pane
rimz config set sidebar.zoom_key "Alt+g"       # smart fullscreen zoom that skips sidebar chrome
rimz config set timezone "America/New_York"    # transcript times, scheduling, and the "today" cutoff
```

### What `rimz config set` does to your disk

`set` edits one real file, and knowing which file and how is what makes it safe to run.

It takes a dotted key and routes it to the file that owns it: `theme.*` to `theme.toml`, `loop.*` to `loop.toml`, everything else to `config.toml`. A key RimZ does not know is refused, so a typo never reaches the file. The edit itself is in place, through `toml_edit`, so your comments and formatting survive. RimZ then re-validates the whole result and writes it as a temp file renamed over the original, so a crash mid-write never leaves half a file.

What lands is byte-for-byte the file you would have edited by hand, which is what makes undoing ordinary: run `set` again with the old value, or delete the line to fall back to the default.

A bare value becomes a TOML value when it parses (`80`, `false`, an array, an inline table) and a string otherwise (`fresh`, `always`). Set a whole color band as an inline table: `rimz config set theme.display.context_meter.red '{ percent = 90, tokens = 400000 }'`. Keys under `theme.colors.*` write to the root `[colors.*]` table in `theme.toml`, so an Alacritty palette pasted there stays paste-compatible. The step list, the value grammar, and every refusal are in the [config reference](../reference/cli/config.md#set-a-value).

### Read a value back

```sh
rimz config path                          # the resolved config.toml path
rimz config get                           # the whole effective config as TOML
rimz config get theme.display.max_cols    # one value
rimz config get sidebar --json
```

`get` prints the effective config: your overrides layered over the built-in defaults. It answers "what is RimZ actually using", not "what did I write", so a key you never set prints the default it follows. A few optional keys whose default is resolved at runtime, `theme.scheme` among them, answer `config key ... is unset` instead ([reference](../reference/cli/config.md#read-a-value)).

## Where your configuration lives

Configuration comes from two places, and each answers a different question.

Your personal settings live in one directory, the RimZ home `~/.rimz/`: your terminal, accounts, notifications, theme, and launch shortcuts. This tier is yours, uncommitted and outside the project trust hash, so nothing you set here follows a repository to someone else's machine.

A repository can also carry one shared file, `<repo>/.rimz/config.toml`: the shape a team agrees on through the repo, such as the agents a clone should launch and the loop tasks it should run. Because that file can name commands to execute, RimZ trust-tracks it, and a fresh clone reads `untrusted` until you review the executable surface and grant it. The full model is [Project config](#project-config).

### The files in your home directory

You rarely open these by hand: `rimz config set` writes to them for you, and `rimz config init` creates them. The directory splits by concern so every command has one place to write and you always know which file owns a setting.

| File | What it holds |
| --- | --- |
| `config.toml` | room behavior, accounts, notifications, agent launch preferences and commands, worktree defaults, attention timing, resume, compaction |
| `theme.toml` | sidebar appearance: palette, slots, glyphs, animations, provider styling, pets ([theme.md](./theme.md)) |
| `loop.toml` | recurring loop task definitions and scheduled command checks |

Two more files in the directory are managed for you, and you reach both through their commands rather than by hand: `remote.toml` holds the named SSH room aliases [`rimz remote`](../reference/cli/remote.md#saved-aliases) writes; `trust/` holds the per-project grants [`rimz trust`](../internals/harness/trust.md) records.

Alongside them, `~/.rimz/` holds `agents.d/` for plugins and `agents/`, `subagents/`, `teams/`, and `traits/` for [Markdown definitions](../reference/definitions.md), plus the shared `skills/` library. You edit those Markdown files directly; `rimz config set` refuses profile and team definition keys.

Two environment variables move the directory. `RIMZ_HOME` moves the whole home. `RIMZ_AGENTS_HOME` moves only the definition trees and the skill library, and it wins over `RIMZ_HOME` for them. `rimz doctor` lists it under HOME with the path it resolved to.

### Where RimZ keeps its files

Everything RimZ keeps for itself across a reboot lives under the home: the config files at the top, `ws/<name>/` for each project's room state, `accounts/<kind>/<name>/` for the provider homes RimZ placed, and machine-wide `logs/`, `loops/`, `web/`, `builds/`, and `cache/`. What RimZ writes outside the home it writes into other programs' config: the reporting hooks `rimz hooks install` adds to each agent CLI, and the [skill links](#skills) a host launch makes in a provider's own skill directory.

Two of those are worth knowing by name. `cache/` holds only what RimZ can rebuild: downloaded assets, the presence plugin, and the provider caches under `cache/providers/`. Deleting it costs a cold provider dashboard and one full spending walk, nothing more. `accounts/` holds provider account homes with their credentials and transcripts, which nothing regenerates and no RimZ command removes.

Room files that only matter while the machine is up (sockets, locks, wakeup pipes) sit on tmpfs under `$XDG_RUNTIME_DIR/rimz/ws/<name>/`, or `/tmp/rimz-<uid>/rimz/ws/<name>/` without a runtime dir, and `~/.rimz/run` links there once a room is born. Panes, sandboxes, and hooks all inherit the `RIMZ_HOME` the room was born with.

A room's directory name is the project's basename plus the first hex digits of its workspace id, such as `ws/myrepo-3f2a/`, and the durable tree under the home and the runtime tree on tmpfs use that same name. When two projects share a basename and those digits, the newer one gets a longer name. `rimz paths` prints every location for the project you run it in, and `rimz paths --json` gives scripts the same thing ([reference](../reference/cli/paths.md)).

### How the layers combine

RimZ reads configuration in four layers, and a later layer wins:

1. built-in defaults,
2. project config (`<repo>/.rimz/config.toml`),
3. per-machine config (`~/.rimz/`),
4. CLI flags and `RIMZ_*` environment variables.

The per-machine layer applies to everything; CLI flags and `RIMZ_*` variables override where each command defines them. The project layer is narrower than a general override: RimZ reads it for the trust hash, for the `[[agents]]` environment it injects, and for the four name tables below.

One case inverts the order on purpose: a trusted project's launch names and loop-task names (`[profiles]`, `[subagents.profiles]`, `[agents.teams]`, `[tasks]`) overlay your machine config and win a name collision, so a repository can pin the exact executable surface it hashes ([Project config](#project-config)).

### When a file is broken

Per-machine TOML loads leniently: a missing file uses defaults, an unknown key warns, and a malformed file falls back to defaults with a warning at startup. Markdown definitions are strict about keys and types instead. A broken definition refuses only the launches that select it, every other profile keeps working, and read-only views still show the definitions that loaded. `rimz doctor` reports both kinds of failure and points at `rimz agents validate`, which names the file and the error; fixing the named source restores the launch.

## Generate and refresh the files

You never have to write these files from scratch. RimZ ships a commented template for each one, and generating them is safe to repeat.

```sh
rimz                       # first start writes the three files when none exists yet, then opens the room
rimz setup                 # detect this machine and write or refresh config
rimz config init           # write config.toml, theme.toml, and loop.toml
rimz config init --print   # print the commented templates without writing anything
```

Most people run `rimz` inside a project once, or `rimz setup` once, then edit the few lines they care about. A first start on an interactive terminal writes the per-machine config, offers hook install, runs the live glyph probe, and asks whether to enable a pet; a non-interactive first start writes the same defaults without prompting. That write is all or nothing: if even one of the three files is already on disk, first start writes none of them, and `rimz setup` is what fills the gaps.

**Rerunning setup keeps your values.** On a machine that already has config, `rimz setup` asks `Keep your current config?`, and saying yes merges. `rimz setup --yes` merges without asking, and a non-interactive `rimz setup` without `--yes` changes nothing at all.

A file that will not parse is the case to know first: setup leaves it byte-for-byte untouched, names the offending line and key when it can, and asks you to fix it before rerunning. Nothing is lost while the file is broken.

The merge itself starts from the shipped template and carries each value you set explicitly onto that template's own documented line, uncommented in place, so the refreshed file keeps the template's structure instead of collecting overrides at the top. It names every key it drops: an unknown key, and a recognized key whose value no longer validates.

Two things do not survive that rebuild. Comments you wrote yourself are gone, because the file is rebuilt from the template and only the template's own comments come back. And a value whose template line sits inside a commented-out table (`[agents]`, `[agents.worktree]`, `[agents.attention]`, `[agents.subagents]`) returns in a table the merge appends rather than on that line, so it keeps working but moves.

Markdown definitions are never touched. The one file setup overwrites outright is `teams/consensus.md`, the [read-only copy of the built-in team consensus](../reference/definitions.md#the-built-in-consensus-copy), which is generated output RimZ never reads back.

Three paths replace a file wholesale with fresh templates: answering no to `Keep your current config?`, `rimz config init --force`, and `rimz config init` on a machine with none of the three files. Plain `rimz config init` where one exists refuses and tells you to pass `--force`.

**The template is close to a field reference.** Every persisted `config.toml` and `theme.toml` section ships as commented TOML with an inline note, so `rimz config init --print` is the fastest current list of keys and defaults. The loop template is the exception: it shows the common task fields and leaves out `verify`, `max-attempts`, and `max-strikes`, which [loop tasks](#loop-tasks) covers below. A line left commented keeps following the default future RimZ versions ship; uncommenting it makes that value this machine's override.

## config.toml: room behavior

`config.toml` holds how the room behaves. Each section below shows its shape and names every key it owns with that key's default, except the multiplexer tables, which are long enough that the section points at the template instead. The `[agents]` tables live under [agent definitions](#agent-definitions-and-launch-preferences), one section down.

### Notifications

Notifications deliver attention off-screen, best-effort, alongside the sidebar's unread cards. The [notifications guide](./notifications.md) covers what fires before you change anything; this section is the keys.

```toml
[notifications]
enabled = true
triggers = ["waiting", "failed"]
desktop = "auto"
sound = "bell"
suppress_focused = true
debounce_ms = 5000
coalesce_ms = 1000
remind_secs = 60
title = "RimZ: {{agent}} {{kind}}"
body = "{{task}}"
command = "ntfy publish rimz"

[[notifications.handler]]
name = "waiting-ntfy"
command = "ntfy publish --title {{title}} rimz {{body}}"
when = { kind = ["waiting"], worktree = ["feat/*"], handle = ["@planner"] }
```

| Key | Default | What it does |
| --- | --- | --- |
| `enabled` | `true` | `false` stops banners, bells, nudges, and handlers and leaves the unread cards alone. One thing outlives it: a loop task that disables itself still fires `loop_disabled` handlers. |
| `triggers` | `["waiting", "failed"]` | Which newly unread kinds raise a banner or a handler. `paused` and `success` also become unread cards; `running` and `idle` stay quiet. |
| `desktop` | `"auto"` | `auto` emits terminal OSC notifications under tmux and skips them under Zellij, which drops notification OSCs. `osc` always emits, `off` never does. |
| `sound` | `"bell"` | `bell` writes a BEL byte and your terminal decides whether it is audible; `off` writes nothing. |
| `suppress_focused` | `true` | Skips the push for a pane you are already looking at. |
| `debounce_ms` | `5000` | The floor between two pushes for one agent. |
| `coalesce_ms` | `1000` | The window that batches several agents into one notification; `0` pushes as soon as the sidebar sees the change. |
| `remind_secs` | `60` | Seconds between reminders about cards still unread; `0` stops them. |
| `title`, `body` | unset | Optional banner templates for agent and coalesced notifications. Reminders and remote-link alerts keep their built-in text. |
| `command` | unset | Shorthand for one unconditional handler. |
| `[[notifications.handler]]` | none | A command RimZ runs through `sh -c` when every `when` clause present matches. |

`title`, `body`, and a handler `command` substitute `{{kind}}`, `{{agent}}`, `{{handle}}`, `{{status}}`, `{{worktree}}`, `{{task}}`, `{{count}}`, `{{unread}}`, `{{pane}}`, and `{{root}}`; a handler command may also use `{{title}}` and `{{body}}`, the rendered banner strings. `agent` and `handle` join the handles or roles when one notification covers several agents, and a variable a notification kind does not carry renders empty.

Each substituted value in a command is shell-quoted as one token, so write `ntfy publish --title {{title}} rimz {{body}}`, never `--title "{{title}}"`. Handlers also receive the same event as `RIMZ_NOTIFY_*` environment variables, listed beside the templates in [what the handler receives](./notifications.md#what-the-handler-receives).

A handler's `when` clauses filter it. `kind` names notification kinds (`waiting`, `failed`, `paused`, `success`, `coalesced`, `reminder`, `loop_disabled`, `link_lost`, `link_restored`), `worktree` glob-matches an agent's branch or path, and `handle` glob-matches its handle or role, with a leading `@` accepted in the pattern.

### Resume

```toml
[resume]
on_rebirth = true
max = 128
auto_continue = false
auto_continue_backoff_secs = [180, 300]
auto_continue_max_retries = 12
auto_continue_text = "continue"
auto_redeem = false
auto_redeem_min_gain = "12h"
```

Resume covers two moments: bringing the room back after the machine or the multiplexer went down, and picking a turn back up after a provider parked it. The behavior model is [loops → keep the fleet moving](./loops.md#keep-the-fleet-moving); these are its keys.

**A rebirth after a reboot or multiplexer crash.** RimZ offers to recover the prior agents from the durable record: the prompt defaults to yes, a non-interactive start recovers without asking, and each restored agent comes up idle in its worktree tab. Empty named channels reopen too on a rebirth within the same boot. `on_rebirth = false`, `--no-resume`, and `rimz reset` come up with no agents. `max` (default 128) caps how many loose agents one birth relaunches; team panes are planned before that cap applies, so a large team can exceed it on its own.

**A park while the room is live.** `auto_continue` is off by default. Turned on, it picks a parked turn back up by typing `auto_continue_text` down the same path as `message --steer`:

- A rate-limit or spend-limit park fires when the account's budget window resets.
- An overload or transient API-error park (a stalled stream, a timeout, a dropped connection) fires on the retry ramp. `auto_continue_backoff_secs = [180, 300]` sends the first retry three minutes after the failure, then one every five minutes.
- Every park type stops after `auto_continue_max_retries` (default 12, about an hour on the default ramp) and stays parked for you.

**A Codex reset credit about to go to waste.** A Codex plan grants credits that refill a spent usage window on the spot, and they expire. `auto_redeem = true` lets RimZ spend one for you when spending it buys real time, by [four rules](./loops.md#auto-redeem) that the assist record names afterwards. One of those rules, rescuing a credit within thirty minutes of its expiry, runs whether or not you turn the key on, because the capacity is already paid for.

`auto_redeem_min_gain` is the threshold the main rule uses: RimZ redeems into a spent window when its natural reset is at least this far away, since that is how much blocked time the credit recovers. It takes `s`, `m`, `h`, or `d` and defaults to `12h`. Raise it to redeem only when a window would otherwise block most of a day.

`rimz setup` offers `auto_continue`, `auto_redeem` (when Codex is installed), and `idle_compact` together as one consent question ([set up your machine](./setup.md)).

### Dollar budgets

```toml
[harness]
budget = "50/day"
turn_budget = "3"

[accounts.budget]
claude = "100/day"
codex = "100/day"
```

`harness.turn_budget` caps every agent turn in the room. It takes a plain dollar amount such as `"3"` or `"$2.50"`, and each new prompt starts a fresh turn baseline. `harness.budget` caps a room's whole fleet for one day, and an `[accounts.budget]` entry caps one provider account for one day, shared by every room signed in to it.

A daily cap runs from local midnight in your [`timezone`](#sidebar-rendering) and must carry the `/day` suffix; a bare amount is refused with the form to use. An account entry also needs a provider that publishes a complete dollar history, which [five of them do](./budget.md#cap-the-room-and-the-account) today. A subscription quota bar or a partial estimate does not qualify, so `rimz config set`, strict config loading, room start, and the `rimz budget` account commands all refuse an ineligible kind such as Antigravity and name the key to remove. Cursor still takes per-agent and room caps, just not an account-day cap. None of these keys runs a command, so none of them enters the project trust hash.

The daily keys are the on-switch. `rimz budget` inspects and adjusts a cap that is already armed without touching the file, and refuses to arm one the file never set; it shows `harness.turn_budget` read-only, so change that standing per-turn cap with `rimz config set harness.turn_budget 3` or delete the key. The cap model (the [five scopes](./budget.md#one-model-five-scopes), what a park does, and what resumes it) is the [budgets guide](./budget.md).

### Smart compaction

```toml
[harness]
smart_compact = "200k"
compact_instruction = "preserve the open questions and exact decisions"
```

`smart_compact` sets the default threshold for compact-first `rimz message` sends and scheduled loop waits, as an occupied-token count (`"200k"` or `"120000"`) or a percentage of the window (`"70%"`). Once an agent's context has reached the threshold, RimZ submits its compact command ahead of the text so the prompt lands against a fresh window. Left unset, compaction stays opt-in through `rimz message --smart-compact`, which overrides this value when both are present.

`compact_instruction` is the brief that rides the compact command, for smart, idle, and hand-off compaction alike; [`rimz agents compact`](../reference/cli/agents.md#compact) uses it too unless you pass an instruction on the command line. Unset, RimZ picks a built-in brief by the agent's seat: a solo agent is asked to preserve decisions, problems, alternatives, exact details, current state, and next steps, while a [team](./teams.md) member's brief points at the board, the stage files, and git by path and section and keeps only what its own window knows plus what is in flight. Set the key to another string to replace both briefs, or to `""` to send the bare command. Only adapters whose native command accepts trailing text receive it: Claude, Grok, and Droid do; every other adapter gets its bare command.

### Idle compaction

```toml
[harness]
idle_compact = "auto"
idle_compact_after = "59m"
```

`idle_compact` is `off` by default, and `rimz setup` offers `auto` as one of its hands-off rows. `auto` compacts an eligible idle agent only while another agent in the same channel is running, on the theory that the work is coming back; an open worktree pull request does not count as that signal. `always` drops the requirement. `idle_compact_after` takes `s`, `m`, `h`, or `d` and defaults to `59m`. The reflex needs at least 50,000 occupied context tokens, sends the adapter's native compact command with the same `compact_instruction`, and fires at most once per idle stretch. The behavior model is [loops → idle compaction](./loops.md#idle-compaction).

### Hand-off compaction

```toml
[harness]
flip_compact = "180k"
```

`flip_compact` compacts a [team](./teams.md) member at the stage flip that hands its own stage to another role, once its context is at least this full. It takes the same thresholds as `smart_compact` and is unset by default, which means no flip compacts. A role's own `flip-compact` field overrides it, and Markdown roles set that field themselves: `120k` for the owner of `Plan` and `180k` for every other owner, or `off` to disable it for that role.

### Garbage collection

```toml
[gc]
auto = true
older_than = "7d"
```

Stale runtime files, orphaned temp files, dead workspace stores, and landed worktrees pile up as rooms come and go. With `auto` on, the default, every open room runs [`rimz gc`](../reference/cli/maintenance.md#sweep-stale-state) once a day, starting five minutes after the room comes up. It removes exactly what a manual run removes, and keeps anything dirty, unmerged, occupied by an agent, or unproven. Each sweep shows up in `rimz stats --assists`.

`older_than` is the age past which runtime and temp files go, and it is also what `rimz gc --older-than` defaults to. It takes `s`, `m`, `h`, or `d`. `rimz config set gc.auto false` turns the daily sweep off and leaves `rimz gc` for you to run by hand.

### Sidebar rendering

Three tables shape the sidebar: the room-level keys, its movement chords, and the timings behind card ranking. What the sidebar shows is the [sidebar guide](./sidebar.md) and the [interface reference](../interface/sidebar.md); these are the settings behind it. Its colors, glyphs, width, render cadence, and card density live in `theme.toml` instead, under [theme → display](./theme.md#display).

```toml
timezone = "America/New_York"

[sidebar]
focus_key = "Alt+p"
zoom_key = "Alt+g"
afk_after_secs = 900
trunk = "develop"
spend_window = "session"
```

`timezone` is an IANA zone for displayed transcript times, wall-clock scheduling, and the `"today"` spend cutoff. Unset or unknown, RimZ uses the system local zone. It is a top-level key, not part of `[sidebar]`.

`focus_key` is the multiplexer chord that focuses the sidebar from any pane and toggles back to the pane you left. Both backends bind it at session birth, the default is `Alt+p`, and `""` or `off` registers nothing. `zoom_key` toggles fullscreen for the focused work pane; with the sidebar focused it picks a working sibling in that view first, so the sidebar is never zoomed. Its default is `Alt+g`, and `""` or `off` disables it.

`afk_after_secs` is the input-idle window before the footer shows `zᶻ idle` on tmux, adding `· Nm` after the first minute; the default is 900 seconds. Zellij reports attach state only, so it shows `zᶻ away` on a full detach whatever this value says. `trunk` names the branch the worktree header compares against, falling back to `main`, then `master`, then the remote's default when it does not resolve.

`spend_window` decides how far back the token and dollar figures in the sidebar count. `"session"`, the default, counts the current stretch of work: it opens when you prompt any agent after five idle hours and stays open while the work continues. `"24h"` counts a trailing twenty-four hours, and `"today"` counts from calendar midnight in `timezone`. [Per room: the cockpit](./insight.md#per-room-the-cockpit) works each choice through.

```toml
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
```

`[sidebar.keys]` rebinds the twelve movement and width keys, shown above with their defaults. Each value is a space-separated list of alternate chords, and the first one is what the `?` help overlay shows. A chord takes optional `ctrl`/`control`/`c` and `alt`/`meta`/`m` modifiers joined by `+` or `-`, then either a case-sensitive single character (`H` differs from `h`) or a named key: `up`, `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown`, `enter`, or `space`.

Two things to know before you rebind. Configured chords resolve before the sidebar's fixed action keys, so a rebind can shadow a filter (`A` for all, `s` for success and done) on purpose or by accident. And tmux's default prefix swallows `Ctrl+b` before the sidebar ever sees it, which is why `pageup` rides alongside it in the default.

```toml
[agents.attention]
active_grace_secs = 180
stalled_after_secs = 1800
tool_repeat_warn_after = 3
tool_repeat_attention_after = 20
inactive_after_secs = 3600
archive_after_secs = 86400
```

`[agents.attention]` moves the boundaries the [ranking](./sidebar.md) draws between cards.

| Key | Default | Boundary it moves |
| --- | --- | --- |
| `active_grace_secs` | `180` | How much silence inside an open working span still counts as active time. |
| `stalled_after_secs` | `1800` | When a silent running agent escalates to the actionable `!` bucket. |
| `tool_repeat_warn_after` | `3` | Identical tool calls in a row before the card gains `⟲`. |
| `tool_repeat_attention_after` | `20` | Identical tool calls in a row before that agent escalates to `!`. |
| `inactive_after_secs` | `3600` | How long a card stays in hot work after its last activity before the ranking treats it as cold. |
| `archive_after_secs` | `86400` | When a card parks below hot and warm work. |

Keep `archive_after_secs` above `inactive_after_secs`: a lower value is lifted to the first second after the inactive window.

### Multiplexer room options

RimZ applies room-scoped multiplexer settings when it creates or reattaches a session, so the room behaves the way agents need without your editing the global Zellij or tmux config. These tables are the only ones this page does not list in full: `rimz config init --print` gives every key with its default, and the per-backend mapping is in [multiplexers.md](../internals/multiplexers.md).

```toml
[mux]
default = "tmux"

[zellij]
pane_frames = true            # an optional override; unset, your config.kdl wins

[tmux]
pane_border_status = "top"    # an optional override; unset, your ~/.tmux.conf wins
```

`[mux] default` picks the backend, consulted after an explicit `--mux <name>`, its `--zellij` and `--tmux` shorthands, and the check for a Zellij or tmux you are already inside. Left unset, RimZ takes tmux when both are installed. Set to `"zellij"` or `"tmux"` it requires that backend, and `rimz start` refuses with a fix message when that backend is not installed.

The two backends differ in what an unset key means.

`[zellij]` passes four keys on every birth and attach, whatever you set them to. Their defaults are what make the room work, and changing one sends your value instead.

| Key | Default | Why RimZ sets it |
| --- | --- | --- |
| `mouse_click_through` | `true` | One click on a card jumps to that agent. |
| `focus_follows_mouse` | `false` | Keeps that click reaching the renderer. |
| `session_serialization` | `false` | RimZ owns rebirth; a resurrected Zellij room comes back with dead panes. |
| `disable_session_metadata` | `true` | Stops Zellij's per-second metadata and `ps` loop. |

Every other key, `pane_frames` and `copy_clipboard` among them, is passed only when you set it and otherwise falls through to your `~/.config/zellij/config.kdl`. Four behaviours are not keys at all: the room starts in locked mode, a directionless new pane splits the focused pane rather than the largest one, swap layouts are off, and the sidebar pane is always borderless so its hit-testing stays stable whatever `pane_frames` says.

A boolean you set here is a request, not a guarantee. Zellij XORs a boolean command-line option against the same key in your `config.kdl`, so `pane_frames = true` on top of a `config.kdl` that already says `pane_frames true` turns frames off. Set a Zellij boolean in one file, not both; RimZ works around the XOR for the two mouse keys its sidebar depends on.

`[tmux]` carries a RimZ default for every key and applies all of them on every birth, so an unset key means RimZ's value rather than yours. The two pane-border keys are the exception, falling through to your `~/.tmux.conf` or tmux's defaults when unset. Setting `pane_border_status` also hands RimZ `pane-border-format`, which blanks the sidebar's border row and overrides any format in `~/.tmux.conf`; leave it unset and your tmux config wins, which may put a title on the sidebar. The table spans session, window, and server scope, clipboard and rich-key handling included, because tmux has no per-session form for those.

To configure your own Zellij or tmux, the theme, truecolor, copy-mode, and keybindings RimZ leaves to you and the sessions you run outside the room, see the [Zellij](./multiplexer.md#zellij) and [tmux](./multiplexer.md#tmux) baselines.

### Accounts

```toml
[accounts.claude.work]
home = "/home/you/.claude-work"

[accounts.codex.personal]

[accounts.budget]
claude = "100/day"

[accounts.usage_limit_usd]
claude = 50.0
codex = 25.0
```

`[accounts.<kind>.<name>]` declares a named Claude or Codex account: a separate provider home that a room launches that provider's agents into. `home` is optional, and an empty table places the home under `~/.rimz/accounts/<kind>/<name>`. `default` is reserved for the provider's own home and is never declared. `rimz accounts add` writes these entries for you, and [provider accounts](./accounts.md) walks the whole flow.

Two limits apply to the path. Two accounts of the same provider cannot share a home, and a `home` cannot contain `,` or end in a directory named `projects`, because provider home lists split on commas and Claude reads `projects` as its transcript folder.

A project picks its room's accounts in `.rimz/config.toml` with `[accounts]` entries such as `claude = "work"`. That selection joins the project trust hash, so an untrusted one refuses `rimz start` until you grant it, and `rimz start --account` overrides it for one room.

`budget` is the enforced daily cap from [dollar budgets](#dollar-budgets), and an entry for a provider that cannot supply real account spend refuses room birth rather than quietly doing nothing. `usage_limit_usd` is a different key with no such gate: it is display-only, so Cursor is valid here. Its monthly ceiling scales the provider dashboard's `ex`/`api` bar when the provider reports no real cap of its own, while the provider keeps enforcing actual spend and the agents keep running. The account figures RimZ shows are read locally, read-only, and best-effort; `RIMZ_OAUTH_USAGE_OFFLINE=1` turns off the live fetches for one process tree without touching transcript-derived totals or any credential file.

### Web access

```toml
[web]
enabled = true
port = 8200
share_port = 8201
interface = "127.0.0.1"
base_url = "https://devbox.example/rimz"
share_base_url = "https://watch.example/rimz"
# auth_header = "X-Authentik-Username"
# auth_users = ["alice"]
# trusted_proxies = ["172.18.0.0/16"]
font = "JetBrainsMono Nerd Font Mono"
# font_source = "/path/to/font.woff2"
style_client = true
```

`enabled` defaults to true and gates `rimz web open`, `rimz web share`, and `rimz remote connect --web`. A normal `rimz start` makes a best-effort start of the shared daemon that every Zellij and tmux room on the machine uses; a missing or pre-1.7.5 ttyd, or an occupied port, warns without blocking the room. With `enabled = false`, the web commands fail before any room changes and tell you to change the config on the machine serving the room. Install ttyd 1.7.5 or newer with `brew install ttyd` or an apt source carrying a current package.

`interface` is the bind address for both daemons. `port` (8200) is the shared daemon, and `share_port` (8201) is the read-only broadcast daemon, which takes no login at all. `base_url` is the URL prefix RimZ prints for `rimz web open` and `rimz web url`, and `share_base_url` the one it prints for `rimz web share`.

Three fields apply to the shared daemon alone. A non-empty `auth_header` hands the public access decision to a proxy-injected header at RimZ's gate, while ttyd keeps its machine-wide Basic Auth behind it. `auth_users` narrows that header to an exact-match list of your identity provider's canonical usernames; both the configured entries and the incoming header value are trimmed before comparison, and the match is case-sensitive. It requires `auth_header`. `trusted_proxies` takes bare IPs or IPv4 and IPv6 CIDRs allowed to reach that gate, and loopback is always allowed.

`style_client` passes the active theme and the `font` family to both browser terminals. Turning it off does not take you back to ttyd's stock page: RimZ still generates the browser page, which is what maps a `?room=` link to the right room. The built-in `JetBrainsMono Nerd Font Mono` and `CaskaydiaCove Nerd Font Mono` families download and cache verified regular and bold faces on their own; `font_source` instead names one local font file or HTTPS font URL, and an unrecognized `font` with no source passes through for the browser to resolve.

No field here runs a command, so the section stays outside the project trust hash. The commands are in the [web guide](./web.md) and the [web reference](../reference/cli/web.md).

### Remote control

```toml
[remote_control]
claude = false
codex = false
```

These two switches opt the machine into the host each provider's official mobile app talks to: with a host up, an agent's ask pushes to your phone and you answer it in that app ([remote → answer asks from your phone](./remote.md#answer-asks-from-your-phone)). Both default to false, and the host runs in the `rimzd` daemon view.

Set either one with `rimz config set`, not by editing the file, because `set` also applies the change to the rooms already running. `rimz config set remote_control.claude true` adds `claude remote-control` to every running room's `rimzd` view at once when `claude` is on `PATH`, and `false` closes those host panes; a whole view you closed on purpose stays closed until the next room start. `rimz config set remote_control.codex true` starts the Codex daemon at once and `false` stops it. Either way the running sidebars wake so the provider dashboard's flag follows the file.

A hand edit does not reach a running room the same way. A Claude host catches up on the daemon view's next repair pass, within about thirty seconds. A Codex edit only changes what the next room start decides, so a daemon already running keeps running.

A `codex` CLI on `PATH` adds its per-session app-server broker whatever this toggle says. `rimz start` and the live Claude enable path both refuse when an enabled host is installed but misconfigured in a way you can fix, such as an incompatible Claude version. An enabled host whose agent is not installed at all is skipped so the room still starts, and `rimz doctor` reports it with the install fix. The security boundary is in [security.md](./security.md).

### Daemon view

```toml
[daemon]
[[daemon.pane]]
command = "stats"

[[daemon.pane]]
command = "btop"
cwd = "/var/log"
```

`[daemon]` fills the middle column of the `rimzd` view, beside the sidebar and any provider hosts. Left unset or empty it holds one live stats pane, `rimz stats --refresh --hold`. A list of `[[daemon.pane]]` entries replaces that default outright, so include `command = "stats"` when you want live stats plus something else.

The reserved token `"stats"` expands to that built-in command. Any other `command` is split into words and run directly, with no shell, so pipes and globs do not work. `cwd` is optional: absent runs from the worktree root, an absolute path is used as written, and a relative path is joined onto the worktree root.

Edits reach a running room unevenly. A changed `command` or `cwd` reloads when you save `config.toml`. A newly added entry gets its pane within about thirty seconds, or at the next `rimz start`. A removed entry's pane stays open, showing live stats, until the room restarts. A pane whose command is empty or unparseable is skipped, and if every configured pane is skipped, RimZ falls back to the built-in stats pane.

### Off-box error reporting

```toml
[sentry]
dsn         = "https://examplePublicKey@o0.ingest.sentry.io/0"
environment = "production"
```

This section works only in a RimZ built with the non-default `sentry` feature, which is why the generated template leaves it out. Set a `dsn` and RimZ reports its own warnings and errors, plus the agent rate-limit and overload conditions it observes, to that Sentry project. With no `dsn` reporting stays off and RimZ makes no network calls, and in a build without the feature the block does nothing at all.

`RIMZ_SENTRY_DSN` and `RIMZ_SENTRY_ENVIRONMENT` override the file for one invocation. `environment` defaults by build profile: an installed release reports as `production`, a dev or CI build as `development`. The DSN is per-machine and never goes in committed project config, so a clone never inherits it, and each event carries only low-cardinality tags (workspace, command, build, fault class, and the agent or session when RimZ knows it), with the hostname and personal data withheld. What leaves the box is [security → off-box error reporting](./security.md#off-box-error-reporting).

## Agent definitions and launch preferences

Reusable agents and teams are Markdown files with YAML frontmatter, one per definition. The `[agents]` tables in `config.toml` hold this machine's launch preferences alongside them: isolation, placement, command shortcuts, worktree defaults, and the two launch limits.

### Agent isolation

A stock agent CLI uses the host's temporary directory and sees every skill installed for that provider. To give a room's agents one shared temporary directory, and to choose which user-level skills a profile may invoke on its own, turn on the Linux bubblewrap mount view:

```sh
rimz config set agents.isolation sandbox
```

That probes bubblewrap and then writes `isolation = "sandbox"` under `[agents]` in `config.toml`. The default is `host`, and `rimz config set agents.isolation host` restores it without disturbing any profile `skills` list. The setting is machine-wide policy, not something a profile overrides. New launches follow it, restarted agents included; panes already open keep what they started with.

To try one launch the other way, pass `--isolation host` or `--isolation sandbox` to `rimz agents`, `rimz teams`, or `rimz subagents`. That agent keeps the choice through restart and resume, and its subagents inherit it. Inside the mount view a Codex agent runs with its own command sandbox off (`--sandbox danger-full-access`); host mode leaves it on.

Every launched agent carries three variables, so a script or skill inside the pane can read them instead of guessing: `RIMZ_ISOLATION` is `sandbox` or `host`, `RIMZ_SCRATCH` names that agent's own scratch directory (`/tmp/scratchpad` in the sandbox, a host path otherwise), and `RIMZ_SHARED` names the directory the whole room shares for files agents hand each other (`/tmp/shared` in the sandbox, a host path otherwise). What the sandbox does and does not hide is [security → sandbox isolation](./security.md#sandbox-isolation).

### Agent profiles, commands, and teams

Four directories under `~/.rimz/` hold the definitions: `agents/` for the profiles you launch directly, `subagents/` for supervised children, `teams/` for rosters and their pipelines, and `traits/` for prompt fragments several definitions share. One file is one definition, named after it, and deleting the file removes the definition; agents already running keep the launch they started with.

Each provider needs a [kind base](../reference/definitions.md#kind-bases) in `agents/`, named after the kind, such as `agents/claude.md`. Three things require one: a profile on a provider that takes a system prompt (Claude, Codex, Pi, and Qwen), a profile carrying a craft body, and every team seat. [`rimz agents validate`](../reference/cli/agents.md) checks the whole set and names what is wrong, and the [definition reference](../reference/definitions.md) owns every frontmatter key, inheritance rule, and error.

#### Profiles

A profile is a named agent preset, loaded from `agents/<name>.md` for one you launch directly or `subagents/<name>.md` for a supervised child. Its `agent:` chain stays inside its own tree, and a field the child sets explicitly replaces the inherited one. Parent craft bodies come before the child's; `description` and `traits` are local to each file, and `model-reminder` is inherited. Where profiles live and how you use one are the [agents guide](./fleet.md#profiles-shape-an-agent-for-one-job); every field, default, model alias, and composition rule is the [definition reference](../reference/definitions.md#chains-and-defaults).

`subagents: [explorer, designer]` on a direct definition limits which children RimZ will delegate to; drop the native `Agent` tool at the same time. Omitting the field leaves delegation unrestricted, `[]` permits none, and a child definition cannot set it at all. The launch reminder names the available children and, by default, the selected model; `model-reminder: false` removes only that model line.

`auto-compact` sets the provider's own native compaction window, a token count from `100k` through `1M`, never a percentage. Markdown definitions default it to `258k` on the kinds that support it ([provider support](../reference/agent-support.md#auto-compaction-window)). A role's `flip-compact` is a different field, covered under [hand-off compaction](#hand-off-compaction).

On the command line, `--model`, `--effort`, `--budget`, and `--system-prompt-file` override the resolved profile, and repeated `--append-system-prompt-file` values replace its fragment list for that launch while leaving the team layer intact. `--agent <PROFILE|KIND>` swaps the engine and keeps the seat identity; the carry-over rules are in the [launch reference](../reference/cli/agents.md#shared-launch-params). Markdown definitions accept no raw `args` and no prompt-path keys: tools generate the arguments, and bodies supply the prompt.

#### Skills

Two separate things share the word. `~/.rimz/skills/` is a library you fill, shared across providers; a profile's `skills:` list decides which skills that agent may invoke on its own.

**The library.** Put a directory containing `SKILL.md` in `~/.rimz/skills/` and every provider can reach it without a copy. Each host launch links `<root>/<name>` to the library entry inside that provider's skill root: Claude, Qwen, and Kiro read their config home's `skills/`, other built-ins read `~/.agents/skills`, and plugins declare no root at all. A named Claude account home gets its links on its first host launch. If `RIMZ_AGENTS_HOME` already puts the library at a provider's own skill root (`RIMZ_AGENTS_HOME=~/.agents`, say), that provider reads it natively and RimZ writes no links.

Your own directories and links are never replaced, and a name collision is reported at launch. Delete a library skill and its link goes at the next host launch for that root; `rimz uninstall` removes RimZ's links from every provider and declared account home and keeps the library itself. A sandbox launch merges the same library through the skill view instead of linking, with the provider's own entry winning a name collision.

**The list.** In sandbox mode, `skills: [merge, review]` in a Markdown profile keeps those two model-callable and turns every other skill RimZ can prepare into one you invoke by name. `skills: []` makes all of them user-invoked; it does not hide the view. Entries are bare directory names, never paths. A child list replaces its parent's rather than appending, omitting the field inherits, and once a parent sets a list no child can go back to unconfigured behaviour. With no list anywhere in the chain, invocation behaviour is the provider's own.

Three rules decide whether a list loads. It requires the `Skill` tool, except on Pi. Every listed name must resolve to an installed skill, searched in the provider's skill directory first and the RimZ library second; a name in neither fails and names both paths. And listing a skill cannot lift a restriction its author set, so a skill already marked user-only is refused with two fixes: drop it from `skills:`, or remove the marker.

Host mode ignores every `skills` list, `[]` included, and linked skills use the provider's native discovery and invocation. In sandbox mode, Antigravity, Amp, OpenCode, Kiro, Grok, and plugins refuse a list outright; remove the field to launch them. Changing a trusted project's skill list means granting trust again. The provider-by-provider table is [agent support](../reference/agent-support.md), and the mount order and markers are in [sandbox internals](../internals/sandbox.md#profile-skill-views).

An unlisted skill RimZ cannot prepare for the user-only view, because its source is unreadable or its metadata will not rewrite, is left out of that agent's launch and the pane says so at startup. The installed skill is untouched and every other skill stays available. The rewriter takes block mappings and block sequences with no anchors, aliases, or tags; inline lists and mappings work as values when they open and close on one line, quoted values stay on one line, and a multiline description needs a block scalar. Listing the skill binds it exactly as installed and skips the rewrite altogether ([the startup warning](./troubleshooting.md#an-agent-pane-says-starting-without-skill-)).

#### Commands

```toml
[agents.commands]
vim = "nvim -p"
```

An `[agents.commands]` entry is a bare string, split into words and run as a plain command pane. These are launch shortcuts, not agents: they take no profile field and answer to no `@` handle. Because they resolve first, an entry can deliberately shadow a binary on `PATH` or even a kind name like `claude`, which is how you set a local default for the word.

#### Teams

A team lives in `teams/<name>.md`. Its frontmatter declares the roles, the stages the work passes through, the leader, and an optional layout; its body is the pipeline every seat shares. Each role picks a direct definition with `agent:` and becomes the profile `<team>.<role>`. Every stage you declare names exactly one role that owns it, and the final `Done` stage is implicit, never declared. A seat's prompt composes in one order: the kind base, then each profile in the role's `agent:` chain, then the role's own craft, then the built-in consensus, then the team body.

Launch every seat with `rimz teams <name>`, or one seat with `rimz agents <team>.<role>`. The optional layout uses the same grammar as an [inline spec](#inline-specs-and-cell-resolution) and must place every role once. The built-in roleless `peer` team stays available unless a definition of that name replaces it. Designing a team is the [teams guide](./teams.md#define-your-own-team); every key and rule is the [definition reference](../reference/definitions.md#teams-and-seats).

A team's seats coordinate through files in the repository: a shared board and per-seat notes. Markdown teams default those patterns to `/blackboard.md` and `/*-notes.md`, and every launch or resume registers them in the repository's Git exclude file so the board never reaches a commit.

#### Inline specs and cell resolution

An inline spec such as `rimz agents "claude,codex+term"` names the cells to launch and how to arrange them: commas split columns, plus signs tile rows, and slashes stack rows, as a Zellij stack or tiled on tmux. Each cell resolves in this order, first match winning:

1. `[agents.commands]`,
2. direct profiles from `agents/`, including a trusted project's overrides,
3. the built-in `term`,
4. a registered agent kind,
5. an adapter-supported `<kind>-<mode>` cell (`claude-auto`, `codex-ask`, `codex-yolo`, and so on; the [agents reference](../reference/cli/agents.md#permission-mode-cells) lists which kinds have which),
6. an executable of that name on `PATH`.

`rimz subagents` uses the same order with one change: step 2 reads child profiles from `subagents/`. A profile that exists only in the other namespace fails the launch and names the namespace to move or copy it to.

Profiles and roles become addressable handles, so validation refuses a name that collides with a reserved command or another address, along with a filename, frontmatter, or chain that does not check out ([validation failures](../reference/definitions.md#validation-and-failures)).

#### Placement

```toml
[agents]
placement = "auto"   # "auto" | "pane" | "tab"
```

`placement` decides where a launch lands when you pass neither `--new-pane` nor `--new-tab`. `auto`, the default, runs a one-cell launch with no worktree in the current pane, and opens a new tab for a worktree, a named channel, or more than one cell. `pane` splits a new pane for that same one-cell case and opens a tab otherwise. `tab` always opens a tab. The flags are in [agents → channel, worktree, and placement](../reference/cli/agents.md#channel-worktree-and-placement).

#### Agent launch chains

```toml
[agents]
max-chain-length = 3
```

An agent that runs `rimz agents` or `rimz teams` launches independent top-level peers, not children. `max-chain-length` caps how many of those launches can chain from one human-started root, and defaults to three. A launch past the limit fails before it creates a pane, a worktree, or a provisional agent, and tells the calling agent not to retry.

#### Subagent launches

```toml
[agents.subagents]
timeout = "30m"
```

This table holds the defaults for [`rimz subagents`](../reference/cli/subagents.md), the agent-only doorway and the one launch path that creates a parented child. `timeout` is each child's wall-clock limit and defaults to 30 minutes; RimZ enforces it even when nothing is waiting on the result. A per-launch `--timeout` overrides it. A subagent cannot launch agents or subagents of its own.

A child's own preset is Markdown, in `subagents/<name>.md`. This table holds only the doorway defaults.

### Worktrees

```toml
[agents.worktree]
dir = "../{repo}-worktrees"
base = "head"
```

`rimz worktree` and `rimz agents --worktree` create their Git worktrees under `dir`. A relative path resolves from the main checkout's root, and `{repo}` expands to that root's basename.

`base` is the ref a new branch starts from. `head` (the default) branches from the main checkout's `HEAD`, `fresh` branches from `origin/HEAD`, and any other string is handed to Git as a ref. Both defaults read the main checkout, even when you run the command from one of its linked worktrees, so a tree created from inside another tree still branches off the main checkout's `HEAD` rather than that tree's.

Seeding files into a new worktree and sharing directories by symlink are repo-level concerns that ride on committed files; they are in the [worktrees guide](./worktrees.md).

### Team signal bindings

A role's `signals` array declares which events wake it. The workflow is [teams → send events to the responsible role](./teams.md#send-events-to-the-responsible-role); this is the field. Inside a role mapping in `teams/forge.md`:

```yaml
signals:
  - signal: ci.failed
    match: {branch: feat-x}
    prompt: Read the failed job and repair it.
```

Each entry is either a bare selector string or a mapping that must carry a `signal` selector, and the role containing it is the receiver. A selector is an exact name or a family such as `ci.*`. Optional `match` is a map of string values that must all equal a top-level field of the event payload. Optional `prompt` is appended verbatim after the event evidence RimZ delivers. An `agent.*` binding additionally requires `match.handle` or `match.session` naming another agent. A binding that does not check out fails team preparation and stays visible in `rimz teams show`.

Scope comes from where the member is working. CI and PR bindings default to that member's worktree, and team signals default to its cohort. Launching from a fresh root checkout with implicit CI or PR scope is refused before anything happens, because nothing would narrow the match: launch with `-w <worktree>`, work from a linked worktree, or give an explicit branch or path in `match`. `rimz teams show` keeps the declarations and the live registrations in separate lists, and a registration disappears when the session ends, is lost, or is stopped.

Each role's ordered binding list, selectors, matches, and prompts together, enters the trust hash, so changing one in a project config needs a fresh `rimz trust grant`.

## loop.toml: scheduled turns

### Loop tasks

```toml
default-timeout = "2h"   # scheduled agent turns without a task timeout

[tasks.morning]
agent = "claude"
prompt = "summarize what landed on main since yesterday"
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
```

Loop tasks live in `~/.rimz/loop.toml` under `[tasks.<name>]`. Shared project tasks take the same shape in `<repo>/.rimz/config.toml`, enter the trust hash, and need both `rimz trust grant` and a machine-local `rimz loop enable <name>` before they run unattended. The scheduling model (the shapes, the watchdogs, the self-waits) is the [loops guide](./loops.md); this section is the field shape, and [`rimz loop`](../reference/cli/loop.md) is the command that writes most of it for you.

`default-timeout` bounds a scheduled run whose task omits `timeout`. It takes `s`, `m`, `h`, and `d` durations, defaults to `2h`, and `rimz config set loop.default-timeout 3h` writes it. A task's own `timeout` wins, and a manual `rimz loop fire` without one stays unbounded.

Every task you write here does one thing: `agent` runs one agent cell on a calendar, interval, cron, or one-shot schedule.

`check` guards that run. It is a shell command RimZ runs first, and its exit code decides whether the agent fires at all: `on = "fail"` proceeds on a non-zero exit or a timeout, `on = "success"` on a zero exit. The check's output is appended to the agent's prompt when it fires. It runs in the task's `dir` when one is set, and at `root` otherwise; `rimz loop add` fills `dir` with the worktree you ran it from.

The rest tune what one fire is allowed to do:

- `verify` runs a shell command after an agent turn and re-prompts that same session when it fails. `max-attempts` caps the agent turns one fire may take, and defaults to `3`.
- `max-strikes` disables the task after that many consecutive failed or no-progress fires. It defaults to `3`, and `0` turns the strike gate off. `rimz loop enable` clears the counter, which is machine-local.
- `budget` caps each run in dollars. `budget-per-day` requires `budget` and skips a fire when today's recorded spend for the task, plus the next run's cap, would pass the daily amount.
- `surplus` holds a task back unless the provider's usage window has room to spare: `"1.5x"` fires only while the budget left in the window outpaces the time left in it by half again. `surplus-after` adds an elapsed floor, and alone it still demands the break-even `1.0x`. Both fail closed when RimZ has no usable window reading, so a plain API key never fires a gated task. The model, worked through with numbers, is [budgets → the surplus gate](./budget.md#the-surplus-gate).

Four things are worth knowing before you edit the file by hand:

- `at` and `cron` resolve in the top-level [`timezone`](#sidebar-rendering), falling back to the system zone when it is unset.
- A machine task carries a `root`. `rimz loop add` writes an absolute path, and a hand-written `~` or relative root is normalized before RimZ matches it to a room, fires it, or displays it.
- A project task runs at the project root implicitly. It resolves `prompt-file` and `system-prompt-file` relative to `.rimz/`, rejects `root`, `dir`, `wait`, and `deadline`, and requires `every`, `cron`, or `signal`, because a one-shot is machine state and does not belong in a committed file.
- A trusted project task outranks a machine task of the same name, but starts disabled until you enable it locally. An untrusted or stale project task stays visible and inert, so the machine task keeps running until you run `rimz trust grant`. `rimz loop add --project` writes `.rimz/config.toml` and enables the task for whoever added it; removing or renaming a project task means editing that file.

Two more task fields exist that you never write here. A `wait` task delivers to one live agent session rather than launching a cell: `rimz loop add --wait @handle` writes it into the workspace's instance state, and bare `--wait` or `--wait @me` targets the caller. `deadline` is what `rimz loop add --until 30m` writes for a task that polls until a time. Both appear in `rimz loop show` and never in `loop.toml`.

That instance state is one of two files outside `loop.toml`. Every session delivery, generated one-shot, and poll-until instance lives in `~/.rimz/ws/<workspace-dir>/loop-instances.json`; machine-local enablement and bounded pauses live in `~/.rimz/loops/loop-arming.json`.

## theme.toml: appearance and pets

The sidebar's palette, glyphs, animations, color depth, color stops, and pets all live in `theme.toml`, and the [theming guide](./theme.md) documents them in full. Three settings are worth naming here because they change what the sidebar does rather than how it looks.

### Pets

```toml
[theme.pets]
enabled = false
pet = "rocky"
glyphs = "auto"
# cell_aspect = 2.5
voice = true
```

An opt-in animated companion in the provider dashboard. The pet question in `rimz setup` and on first start writes `enabled = true` with the default `rocky`. Picking one and feeding it a sprite sheet is the [pets guide](./pets.md).

| Key | What it does |
| --- | --- |
| `enabled` | Turns the dashboard pet on. |
| `pet` | Selects a built-in, HTTPS, local-sheet, or petdex pet. |
| `glyphs` | Chooses `auto`, `pixel`, or `sextant` rendering. `[theme.display] pixel = "off"` overrides a pixel choice. |
| `cell_aspect` | Overrides the terminal's cell height-to-width ratio for sextant correction. Set it for a tall or short font under Zellij: `rimz config set theme.pets.cell_aspect 2.5`. |
| `voice` | Toggles the canned captions a pet shows when its action changes. |

### Sidebar bands

The agent card's context meter and the provider budget bar interpolate across color stops you can tune, in `[theme.display.context_meter]` and `[theme.display.budget_bar]`. `[theme.display] pixel` is the master kitty-graphics switch for the context meter and for pets, `auto` or `off`. The model, the terminal requirements, the fallbacks, and the shipped numbers are in [theme → display](./theme.md#display).

### Provider dashboard

Which providers appear on the dashboard, in what order, and with what brand styling comes from `[theme.display] provider_tabs`, `provider_list`, and `max_provider_blocks`, plus `[theme.providers.<kind>]`. The layout is in [theme → display](./theme.md#display) and the styling fields in [theme → provider styling](./theme.md#provider-styling).

## Project config

`<repo>/.rimz/config.toml` is committed, and it declares the workspace shape a team shares. On a trusted workspace RimZ injects each `[[agents]]` `env` table into that agent's process at launch, applies top-level `[profiles]` and `[agents.teams]` to `rimz agents`, applies `[subagents.profiles]` to `rimz subagents`, and loads `[tasks]` for `rimz loop`. Pick one `agents` shape per file: `[[agents]]` for env entries, or `[agents.teams]` for shared teams.

```toml
[[agents]]
name = "claude"
env = { CLAUDE_CODE_DISABLE_AGENT_VIEW = "1" }

[accounts]
claude = "work"

[tasks.morning-triage]
agent = "codex"
prompt = "triage yesterday's failing CI runs and open one issue per distinct cause"
at = "08:00"
every = "day"
```

Two fields enter the trust hash but do nothing yet: `launch_command` on an `[[agents]]` entry, and a `[[hooks]]` table. RimZ neither runs the command nor installs the hook today. One table is refused outright: a `[layout]` table fails the load with an error telling you to move it to `$RIMZ_HOME/config.toml`. Ignore the fix and delete the table; per-machine config has no `[layout]` either. RimZ's own [`.rimz/config.toml`](../../.rimz/config.toml) is a working example of project tasks; its repository sync task assumes push rights on the remote.

Because the file can name commands to run, every command-running field enters the trust hash, and a clone reads `untrusted` until `rimz trust grant` pins the current surface on this machine. An `untrusted` or `stale` workspace refuses any launch or project-only task run that would consume project config, and says `rimz trust grant`. A machine task of the same name keeps running meanwhile, `rimz loop list` and `rimz loop show` still show project tasks with their trust state, and a `stale` report prints a field-level diff of what changed since the grant, so you re-grant knowing what you are re-granting.

A trusted repo profile, team, or task overlays your machine config and wins a name collision. To keep that surface closed and machine-independent, a repo profile may inherit only another repo profile or a built-in kind, and a repo team role may bind only a repo profile. The threat model is [security and trust](./security.md), and the hash contract and launch-time enforcement are in [trust.md](../internals/harness/trust.md).

## Moving from the XDG roots

Earlier releases split RimZ across four XDG directories. RimZ reads only `~/.rimz/` now and does not move the old roots for you. `rimz doctor` lists the config, state, and data roots when they are still present, and `rimz start` and `rimz attach` refuse to open a room on a machine that has them but no `~/.rimz/config.toml`, naming this section. Setting `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_DATA_HOME`, or `XDG_CACHE_HOME` no longer moves any RimZ file; `RIMZ_HOME` does.

| Old location | New location |
| --- | --- |
| `~/.config/rimz/config.toml`, `theme.toml`, `loop.toml`, `remote.toml` | `~/.rimz/` |
| `~/.config/rimz/agents/`, `subagents/`, `teams/`, `traits/`, `skills/`, `agents.d/` | `~/.rimz/` |
| `~/.config/rimz/projects/` | `~/.rimz/trust/` |
| `~/.local/state/rimz/workspaces/ws_<24hex>/` | `~/.rimz/ws/<basename>-<hex>/` |
| `$XDG_RUNTIME_DIR/rimz/ws_<24hex>/` | `$XDG_RUNTIME_DIR/rimz/ws/<basename>-<hex>/` |
| `~/.local/state/rimz/shared/` | `~/.rimz/cache/providers/` |
| `~/.local/state/rimz/*.log.jsonl` | `~/.rimz/logs/` |
| `~/.local/state/rimz/loop-arming.json`, `loop-strikes.json` | `~/.rimz/loops/` |
| `~/.local/state/rimz/web-*.json` and the Zellij web config | `~/.rimz/web/` |
| `~/.local/state/rimz/builds/` | `~/.rimz/builds/` |
| `~/.local/share/rimz/accounts/` | `~/.rimz/accounts/` |
| `~/.cache/rimz/` | `~/.rimz/cache/` |

Stop every running room first, because a live room keeps writing to the old roots. Then move the machine config and the named provider accounts, which are the two things you would miss:

```sh
mkdir -p ~/.rimz
mv ~/.config/rimz/* ~/.rimz/
mv ~/.local/share/rimz/accounts ~/.rimz/accounts
```

A room's state is keyed by directory name now, so it carries over only if you move it to the name RimZ expects. To keep one, run `rimz paths` in that project and move its old directory into place:

```sh
cd ~/src/myrepo
id=$(rimz paths --json | jq -r .workspace_id)
mkdir -p ~/.rimz/ws
mv ~/.local/state/rimz/workspaces/"$id" "$(rimz paths --json | jq -r .state_dir)"
```

A project you skip starts with fresh room state at its next `rimz start`. Remove the old roots once you are done, so `rimz doctor` stops listing them:

```sh
rm -rf ~/.config/rimz ~/.local/state/rimz ~/.local/share/rimz ~/.cache/rimz
```

## See also

- [Set up your machine](./setup.md): the guided first pass, which writes most of these files for you.
- [Theming](./theme.md): everything in `theme.toml`, from the palette to the color stops.
- [Loops and schedules](./loops.md): what a `loop.toml` task actually does when it fires.
- [Security and trust](./security.md): the two places config can run a command, and what leaves the box.
- [Zellij and tmux](./multiplexer.md): configuring your own multiplexer, outside the room RimZ manages.
- [Config CLI reference](../reference/cli/config.md): every `rimz config` form, the value grammar, and what `set` refuses.
- [Definition reference](../reference/definitions.md): every frontmatter key for profiles, subagents, teams, and traits.
