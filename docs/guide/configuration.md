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
rimz config set harness.idle_compact off       # disable idle compaction on this machine
rimz config set notifications.triggers '["waiting", "failed"]'   # which cards raise a banner
rimz config set sidebar.focus_key "Alt+p"      # the chord that jumps to the sidebar from any pane
rimz config set sidebar.zoom_key "Alt+g"       # smart fullscreen zoom that skips sidebar chrome
rimz config set timezone "America/New_York"    # transcript times, scheduling, and the "today" cutoff
```

### What `rimz config set` does to your disk

`set` edits one real file, and knowing which file and how is what makes it safe to run.

It takes a dotted key and routes it to the file that owns it: `theme.*` to `theme.toml`, `loop.*` to `loop.toml`, everything else to `config.toml`. A key RimZ does not know is refused, so a typo never reaches the file. The edit itself is in place, through `toml_edit`, so your comments and surrounding formatting survive. RimZ then re-validates the whole result and writes it as a temp file renamed over the original, so a crash mid-write never leaves half a file.

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

Room files that only matter while the machine is up (sockets, locks, wakeup pipes) sit on tmpfs under `$XDG_RUNTIME_DIR/rimz/ws/<name>/`, or `/tmp/rimz-<uid>/rimz/ws/<name>/` without a runtime dir, and `~/.rimz/run` links there once a room is born. Panes, scoped views, and hooks all inherit the `RIMZ_HOME` the room was born with.

A room's directory name is the project's basename plus the first hex digits of its workspace id, such as `ws/myrepo-3f2a/`, and the durable tree under the home and the runtime tree on tmpfs use that same name. When two projects share a basename and those digits, the newer one gets a longer name. `rimz paths` prints every location for the project you run it in, including its `team-notes/` archive, and `rimz paths --json` gives scripts the same thing ([reference](../reference/cli/paths.md)).

### How the layers combine

RimZ reads configuration in four layers, and a later layer wins:

1. built-in defaults,
2. project config (`<repo>/.rimz/config.toml`),
3. per-machine config (`~/.rimz/`),
4. CLI flags and `RIMZ_*` environment variables.

The per-machine layer applies to everything; CLI flags and `RIMZ_*` variables override where each command defines them. The project layer is narrower than a general override: RimZ reads it for the trust hash, for the `[[agents]]` environment it injects, and for the four name tables and `env-reminder` preference below.

One case inverts the order on purpose: a trusted project's launch names and loop-task names (`[profiles]`, `[subagents.profiles]`, `[agents.teams]`, `[tasks]`) overlay your machine config and win a name collision, so a repository can pin the exact executable surface it hashes ([Project config](#project-config)).

The project layer also overrides one `[agents]` scalar: [`env-reminder`](#environment-at-launch). Other `[agents]` preferences remain machine-local.

### When a file is broken

Per-machine TOML loads leniently: a missing file uses defaults, an unknown key warns, and a malformed file falls back to defaults with a warning at startup. Markdown definitions are strict about keys and types instead. A broken definition refuses only the launches that select it, every other profile keeps working, and read-only views still show the definitions that loaded. `rimz doctor` reports both kinds of failure and points at `rimz agents validate`, which names the file and the error; fixing the named source restores the launch.

## Generate and refresh the files

You never have to write these files from scratch. RimZ ships a commented template for each one, and generating them is safe to repeat.

```sh
rimz                       # first start writes whichever of the three files is missing, then opens the room
rimz setup                 # detect this machine and write or refresh config
rimz config init           # write all three, refusing if any exists unless you pass --force
rimz config init --print   # print the commented templates without writing anything
```

Most people run `rimz` inside a project once, or `rimz setup` once, then edit the few lines they care about. A first start on an interactive terminal writes the per-machine config, offers hook install, runs the live glyph probe, and asks whether to enable a pet; a non-interactive first start writes the same defaults without prompting. It writes each file independently: delete `config.toml` and keep your `theme.toml`, and the next start writes the one that is missing and leaves the one you kept exactly as it is.

**Rerunning setup keeps your values.** On a machine that already has config, `rimz setup` asks `Keep your current config?`, and saying yes merges. `rimz setup --config-only` merges without asking, installs no hooks, and grants no trust. A non-interactive `rimz setup` without `--config-only` changes nothing at all.

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
| `enabled` | `true` | `false` stops banners, bells, nudges, and all handlers and leaves the unread cards alone. |
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
spend_order = "credits_first"
auto_redeem_min_gain = "12h"
```

Resume covers two moments: bringing the room back after the machine or the multiplexer went down, and picking a turn back up after a provider parked it. The behavior model is [loops → keep the fleet moving](./loops.md#keep-the-fleet-moving); these are its keys.

**A rebirth after a reboot or multiplexer crash.** RimZ offers to recover the prior top-level agents from the durable record: the prompt defaults to yes, a non-interactive start recovers without asking, and each restored agent comes up idle in its worktree tab. If a tab fails to open, RimZ names it and offers its agents again at the next rebirth; a running room only tells you they stay parked. A top-level agent that cannot be resumed is dropped only when you say so at a second prompt, which defaults to no; until then it remains parked. Its prior subagents are closed automatically, because a resumed parent relaunches any children it still needs. A missing worktree is listed as `worktree gone` and kept until you choose to drop the agent, so a volume not yet mounted after a reboot does not lose its agents. `on_rebirth = false`, `--no-resume`, and a plain `rimz reset` come up with no agents: at a terminal that is your decision and the prior agents are recorded as ended, while a start with no terminal leaves the top-level agents parked. `max` (default 128) caps how many loose agents one birth relaunches; team panes are planned before that cap applies, so a large team can exceed it on its own. When a team seat has been refilled, RimZ retires its old parked session instead of offering it again.

Subagent children are never restored. A child's open run cut off by the crash reports as canceled to its parent, which can relaunch the work if it is still needed.

A running room never asks about parked agents, whether you use Zellij or tmux. Use `rimz agents resume '#channel'` or `rimz agents resume <worktree>` to bring a group back. An attended `rimz start --no-resume` ends all parked candidates instead, without a prompt. The host reports the ended count; if there were none, start or attach prints `rimz: --no-resume changed nothing: the room was already running with no parked agents`. Remote connects with `--no-resume` report the same outcome.

**A park while the room is live.** `auto_continue` is off by default. Turned on, it picks a parked turn back up by typing `auto_continue_text` down the same path as `message --steer`:

- A rate-limit or spend-limit park fires when the account's budget window resets.
- An overload or transient API-error park (a stalled stream, a timeout, a dropped connection) fires on the retry ramp. `auto_continue_backoff_secs = [180, 300]` sends the first retry three minutes after the failure, then one every five minutes.
- Every park type stops after `auto_continue_max_retries` (default 12, about an hour on the default ramp) and stays parked for you.

**A Codex reset credit about to go to waste.** A Codex plan grants credits that refill a spent usage window on the spot, and they expire. `auto_redeem = true` lets RimZ spend one for you when an in-use account's subscription window is spent and a useful reset is the next source of capacity, by [three rules](./loops.md#auto-redeem) that the assist record names afterwards. It does not wait for an agent to park. One of those rules, rescuing a credit within thirty minutes of its expiry, runs whether or not you turn the key on, because the capacity is already paid for. That rescue covers every Codex account you declared, not only the ones this room uses.

`spend_order` chooses which paid-for capacity comes next after the subscription: `credits_first` (default) uses available paid credit before a banked reset; `resets_first` prefers a reset when the redemption rules allow it, then paid credit. Expiry rescue runs under either order. Claude can use paid extra usage, but only Codex supports automatic resets.

`auto_redeem_min_gain` is the threshold the main rule uses: RimZ redeems into a spent window when its natural reset is at least this far away, since that is how much blocked time the credit recovers. It takes `s`, `m`, `h`, or `d` and defaults to `12h`. Raise it to redeem only when a window would otherwise block most of a day.

`rimz setup` offers `auto_continue`, `auto_redeem` (when Codex is installed), and `idle_compact` together as one consent question; idle compaction starts at `100k`, and answering no sets it to `off` ([set up your machine](./setup.md)).

### Dollar budgets

```toml
[harness]
budget = "50/day"
turn_budget = "3"

[accounts.budget]
claude = "100/day"
codex = "100/day"
```

`harness.turn_budget` caps every agent turn in the room. It takes a plain dollar amount such as `"3"` or `"$2.50"`, and each new prompt starts a fresh turn baseline. `harness.budget` caps a room's whole fleet for one day. An `[accounts.budget]` entry accepts `/day` or `/month` and caps `default` and its shared accounts together, with a separate cap of the same size for each standalone account, shared by every room signed in to them. A named account's own `budget` overrides the kind entry and requires `history = "standalone"`; shared spend cannot be capped separately.

A daily cap runs from local midnight in your [`timezone`](#sidebar-rendering) and must carry the `/day` suffix; a bare amount is refused with the form to use. A monthly account cap runs from 00:00 UTC on the 1st and counts API-key billing only: a pool containing any subscription account is never parked by it, while a cold account cache does not disable it. An account entry also needs a provider that publishes a complete dollar history, which [five of them do](./budget.md#cap-the-room-and-the-account) today. A subscription quota bar or a partial estimate does not qualify, so `rimz config set`, strict config loading, and launches of an ineligible kind such as Antigravity refuse and name the key to remove. Room start warns instead. `rimz budget --account KIND` checks only the targeted kind; fleet reports omit ineligible kinds and warn on stderr about each key to remove without blocking adjustments. Cursor still takes per-agent and room caps, just not account caps. None of these keys runs a command, so none of them enters the project trust hash.

The cap keys supply standing defaults. `rimz budget 50/day` can also arm a room cap without touching config, and `rimz budget --account claude 100/day` or `300/month` can arm an account pool. `rimz budget off` lifts the room cap; add `--account claude` to lift that pool's cap. The command shows `harness.turn_budget` read-only, so change that standing per-turn cap with `rimz config set harness.turn_budget 3` or delete the key. The cap model (the [five scopes](./budget.md#one-model-five-scopes), what a park does, and what resumes it) is the [budgets guide](./budget.md).

### Native auto-compaction

Claude and Codex compact automatically when a conversation reaches their native ceiling. `auto_compact` sets the window RimZ passes to profiles that have no `auto-compact` of their own:

```toml
[harness]
auto_compact = "272k"
```

The built-in default is `272k`. The value takes a token count from `100k` through `1M` inclusive (`"272k"`, `"272000"`, or `"1m"`), never a percentage. Set it to `"off"` to pass no window and leave the provider's default alone. It applies to Markdown definitions and trusted project TOML profiles on Claude and Codex; other providers and bare-kind launches such as `rimz agents claude` are unchanged. Switching a profile onto a provider without a window flag silently drops the injected default. A profile's own `auto-compact`, inherited values, and explicit team role overrides still win, and warn when the final provider cannot express them. Claude caps the native window at the model's context window; Codex clamps it to 90% ([provider support](../reference/agent-support.md#auto-compaction-window)). On any model whose window is at or below `258k` (a 200k Claude model, or Codex GPT-5.5 after its 90% clamp), the native ceiling sits below the built-in smart threshold, so smart compaction never fires there unless you lower it.

### Smart compaction

```toml
[harness]
smart_compact = "200k"
compact_instruction = "preserve the open questions and exact decisions"
```

`smart_compact` sets the default threshold for compact-first `rimz message` sends and scheduled loop waits, as an occupied-token count (`"200k"` or `"120000"`) or a percentage of the window (`"70%"`). Once an agent's context has reached the threshold, RimZ submits its compact command ahead of the text so the prompt lands against a fresh window. Left unset, it defaults to `258k`; `"off"` disables it. `rimz message --smart-compact` overrides the machine value for that send, including when the machine setting is `"off"`.

`compact_instruction` is the brief that rides the compact command, for smart, idle, and hand-off compaction alike; [`rimz agents compact`](../reference/cli/agents.md#compact) uses it too unless you pass an instruction on the command line. Unset, RimZ picks a built-in brief by the agent's seat: a solo agent is asked to preserve decisions, problems, alternatives, exact details, current state, and next steps, while a [team](./teams.md) member's brief points at the board, the stage files, and git by path and section and keeps only what its own window knows plus what is in flight. Set the key to another string to replace both briefs, or to `""` to send the bare command. Only adapters whose native command accepts trailing text receive it: Claude, Grok, and Droid do; every other adapter gets its bare command.

### Idle compaction

```toml
[harness]
idle_compact = "100k"
```

An idle agent may get its next prompt after its provider's warm prompt cache has expired, and that prompt then pays to cache the whole conversation again. Idle compaction shrinks the conversation first, while the cache is still warm. `idle_compact` is a token floor: once an agent has sat idle for its provider's prompt-cache lifetime less `prompt_cache_margin`, RimZ compacts it if its occupied context has reached the floor. It defaults to `"100k"`; it takes a token count in digits or `k` (`"50k"`, `"250000"`, `"1000k"`) or `"off"`; an `m` suffix is refused, since an old minute value such as `"25m"` would otherwise read as millions. A profile's or team role's [`idle-compact`](../reference/definitions.md#idle-compaction) sets its own floor, so a cheap reviewer can compact at `50k` while a long-running planner stays at `250k` or `off`.

| Provider | Default prompt-cache lifetime | Compacts after idle (default margin) |
| --- | --- | --- |
| Claude, every sign-in | 60 minutes | 59 minutes |
| Codex, subscription | 12 hours | 11 hours 59 minutes |
| Codex, API key or unknown sign-in | 30 minutes | 29 minutes |
| Other providers, via fallback | 60 minutes | 59 minutes |

Each agent's newest observed cache write wins over settings, even `off`: a five-minute write uses five minutes, a one-hour write uses one hour, and a mixed write uses five minutes because its shorter-lived part expires first. Claude observes main-chain requests and Pi observes its active branch; read-only requests do not change the observation. A write with no retention split falls through to settings. Before an observation, the first match wins: the provider's sign-in setting, its plain setting, its built-in sign-in default, then `fallback` (default `1h`). Unknown sign-in skips split settings; Codex uses its shorter default until its account is classified.

Configure lifetimes for your setup, or use `off` to leave an unresolved agent's clock muted and disable its idle compaction and pings:

```toml
[harness.prompt_cache_ttl]
claude = "5m"
codex = { subscription = "12h", api-key = "30m" }
fallback = "1h"
```

Keys name built-in or installed plugin provider kinds, not models or named logins; `fallback` is reserved and cannot name a plugin. Any kind accepts a plain duration or `off`, or a table with optional `subscription` and `api-key` keys. Those keys classify billing, not named accounts (which may also be named `subscription` or `api-key`). `fallback` takes only a plain duration or `off`. Unknown kinds, other sub-keys, and invalid durations are refused. `harness.prompt_cache_margin` (default `"1m"`) is how long before a prompt cache expires both cache pings and idle compaction fire; raise it if pings land too late on a slow machine. At load it must be shorter than every lifetime resolved from settings, defaults, and the fallback, and than `cache_keepalive_max`; that check does not judge an agent's own cache writes. An observed lifetime at or below the margin still shows its card clock but gets no idle compaction or pings.

`harness.cache_keepalive` defaults to `true` and controls waiting-agent and [keep-warm](#keep-warm) cache pings independently of idle compaction. `harness.cache_keepalive_max` (default `"6h"`) stops subsequent pings that would start that long after the agent's last request of its own; the first sleeping ping is always uncapped. The last ping says so, and the agent's next turn of any other kind restarts the clock. The maximum accepts `off`, which keeps pinging for as long as the agent sleeps, or a duration longer than `prompt_cache_margin`.

Solo agents and team members both qualify; `rimz subagents` children and provider subagents never do. A team member compacts on the same schedule whatever its board says, including after `Done`. The agent needs a bound pane and must be idle rather than working, sleeping with a pending wait, awaiting input, budget-parked, or already compacting. The reflex sends the native command with the seat's [brief](#smart-compaction) at a turn boundary and fires at most once per idle stretch. Once an agent has compacted (by this reflex, by hand, or on its own while resting), neither idle compaction nor keep-warm touches it again until a real prompt arrives. Idle time starts at the last tool event or, without one, the turn start, capped at the completed turn's end. Restarting an idle agent does not restart this clock. An agent found past its cache lifetime is left alone rather than compacted cold. Timing is best-effort: missing hooks, a slow sidebar pass, or early provider eviction can cost a cache write, never correctness. A seat that [keep-warm](#keep-warm) is holding is not compacted until its horizon passes or its stage condition stops holding. See [loops → idle compaction](./loops.md#idle-compaction) for the action and its record.

### Keep-warm

An agent you expect to come back to can keep its prompt cache warm while it sits idle. Set `keep-warm` on a profile, team, or team role to a maximum such as `2h`: after each real turn ends, RimZ pings the agent shortly before every cache expiry until that time passes, so your next prompt inside it reads the context from cache. It is off unless set.

In a trusted project's `.rimz/config.toml`:

```toml
[profiles.coder]
agent = "claude"
keep-warm = "2h"
```

In machine configuration, choose the shortest cache lifetime to hold:

```toml
[harness]
keep_warm_min_ttl = "15m"
```

A team can add `while` using the [loop `--when` condition grammar](../reference/cli/loop.md#conditions), restricted to `team.stage`, for example `keep-warm = { max = "6h", while = "team.stage!=Done" }`. The same block works in Markdown YAML. Each role inherits `max` and `while` separately from the team; a role's `off` cancels that field. A duration is shorthand for `max` alone, so it still inherits the team's `while`; shorthand `off` cancels both. A plain duration no longer stops at `Done`. The condition is reread, not latched: reopening the board can restore a hold if the cache is still warm. Before the first stage, or when the board is absent or unreadable, the condition permits the hold. A `while` without `max` uses `harness.cache_keepalive_max`; with that cap `off`, it holds nothing and validate asks you to set `max`.

A team seat ignores the profile behind it. A solo agent follows its profile's `max` (shorthand or block), inherited from the nearest `agent` base; an explicit `off` wins. `while` is refused on solo profiles and unstaged teams because they have no board, and other condition keys or undeclared stages are refused at load. See the full [`keep-warm` rules](../reference/definitions.md#keep-warm). A `rimz subagents` child is never held. A ping turn does not restart the horizon, and it is not activity: the card keeps its status, unread mark, and quiet time, team signals and idle stops ignore it, and only the cache clock moves.

Each ping costs a cache read of the whole context, so a short cache lifetime makes a long horizon expensive. `keep_warm_min_ttl` is the shortest resolved prompt-cache lifetime admitted for any ping, sleeping or keep-warm, `15m` by default: a five-minute lifetime gets no pings and no hold. Set it to `off` to admit shorter lifetimes; they must still exceed the margin. Idle compaction has no lifetime floor. An unresolved lifetime with `fallback = "off"` gets no hold. `harness.cache_keepalive = false` turns every ping off, keep-warm included, and `harness.cache_keepalive_max` (default `6h`) caps keep-warm like any other ping run: a horizon longer than the maximum ends at it, the card shows the heating clock from the last ping, and `rimz agents validate` names the cap. A horizon counts only while the pings can maintain it: with pings off, a cache that has already gone cold (a missed window is not pinged back), an idle stop pending, a lifetime under the floor or at or below the margin, a budget-parked agent, or one waiting on your answer, the card shows the ordinary heating clock and idle compaction runs on its normal schedule. `rimz agents validate` judges settings and defaults only, naming each machine Markdown definition or team role whose `keep-warm` cannot take effect and the setting to change; an agent's own cache writes can change its lifetime. Project TOML definitions are not checked by that command. The pings need a running sidebar, appear in [`rimz stats`](../reference/cli/stats.md) as `holding warm`, and are described in [loops](./loops.md#wake-a-running-agent).

### Hand-off compaction

```toml
[harness]
flip_compact = "180k"
```

`flip_compact` compacts a [team](./teams.md) member at the stage flip that hands its own stage to another role, once its context is at least this full. It takes the same thresholds as `smart_compact`, or `"off"` to disable hand-off compaction. Unset uses the built-in role defaults: `120k` for a role owning `Plan`, `180k` otherwise. A role's own `flip-compact` field overrides the machine setting, including `"off"`, and the built-in defaults.

### Garbage collection

```toml
[gc]
auto = true
older_than = "7d"
transcript_retention = "forever"
team_notes_retention = "forever"
tmp_grace = "1d"
tmp_floor_percent = 10
tmp_floor_min = "10GiB"
```

Stale runtime files, orphaned temp files, dead workspace stores, and landed worktrees pile up as rooms come and go. With `auto` on, the default, every open room runs [`rimz gc`](../reference/cli/maintenance.md#sweep-stale-state) once a day, starting five minutes after the room comes up. A manual `rimz gc` sweeps only the current room; `--all` adds every room and shared machine state. One daily helper is elected to run that machine scope, so closed rooms also get cleaned without every open room repeating the machine sweep. Both scopes keep anything dirty, unmerged, held by an agent, pane, armed loop task, or team board short of `Done`, or unproven. Each sweep shows up in `rimz stats --assists`.

Conversation transcripts and final message text stay forever by default, separately from diagnostic logs. Set `gc.transcript_retention` to a positive duration such as `30d` or `12h` to expire their weekly files by the time of the last append. A file can keep its earliest entries up to seven days longer than the setting. `rimz gc` reports both histories together as `transcript`, including their kept bytes; `--older-than` does not override this setting. This is a per-machine setting, not project config. Whole-room deletion (including machine GC of a deleted project), hard reset, and `uninstall --state` or `--all` still remove the history. Files already deleted by an older sweep cannot be recovered by changing retention.

Team boards and notes have their own archive under `~/.rimz/ws/<workspace-dir>/team-notes/`. They also stay forever by default. Set `gc.team_notes_retention` to `forever` or a positive duration with the same grammar to remove whole runs after their newest file or directory modification expires. One fresh entry keeps the whole run; removing a checkout does not remove its notes. `rimz gc` reports kept bytes in `team-notes`, independently of `--older-than`. Soft reset keeps these archives; hard reset, whole-room deletion, and `uninstall --state` or `--all` remove them.

`older_than` is the age past which runtime and atomic-write temp files go, and it is also what `rimz gc --older-than` defaults to. Agent scratch has its own `tmp_grace`: a unit stays while any agent sharing it is live or has not ended, then goes after both its latest end and its latest directory or direct-child modification are past the grace. Both durations take `s`, `m`, `h`, or `d` and must be greater than zero. `tmp_floor_percent` (0 through 100) and `tmp_floor_min` reserve the larger of a capacity percentage and a byte size; `GiB` is binary, `G` is decimal. When space falls below that floor, open rooms trigger [oldest-first pressure eviction](../reference/cli/maintenance.md#automatic-sweeps) of ended agents' scratch across all rooms, without waiting for the grace. Live agents' scratch stays. `rimz doctor` shows the free space and reclaimable units. Setting both floor settings to zero disables pressure eviction. `rimz config set gc.auto false` turns off both automatic deletion paths and leaves ordinary `rimz gc` for you to run by hand.

### Sidebar rendering

Three tables shape the sidebar: the room-level keys, its movement chords, and the timings behind card ranking. What the sidebar shows is the [sidebar guide](./sidebar.md) and the [interface reference](../interface/sidebar.md); these are the settings behind it. Its colors, glyphs, width, render cadence, card density, and stale-card threshold live in `theme.toml` instead, under [theme → display](./theme.md#display).

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

`afk_after_secs` defaults to 900 seconds. On both backends it bounds how long a departed client's sidebar view is retained for reattach. On tmux it also sets the input-idle window before the footer shows `zᶻ idle`, adding `· Nm` after the first minute. Zellij reports attach state only, so its footer shows `zᶻ away` only on a full detach. The [sidebar guide](./sidebar.md#how-the-column-is-ordered) explains how views follow clients. `trunk` names the branch the worktree header compares against, falling back to `main`, then `master`, then the remote's default when it does not resolve.

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

`[sidebar.keys]` rebinds the twelve movement and width keys, shown above with their defaults. Changes apply to each sidebar with its next refreshed snapshot, without a restart, and the `?` help overlay shows the same bindings. Each value is a space-separated list of alternate chords, and the first one is what the overlay shows. A chord takes optional `ctrl`/`control`/`c` and `alt`/`meta`/`m` modifiers joined by `+` or `-`, then either a case-sensitive single character (`H` differs from `h`) or a named key: `up`, `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown`, `enter`, `esc`, or `space`.

Two things to know before you rebind. Outside search typing, configured chords resolve before the sidebar's fixed action keys, so a rebind can shadow a filter (`A` for all, `s` for success and done) on purpose or by accident. While typing a search, plain characters enter the query instead, and every `Ctrl` or `Alt` chord other than `Ctrl+n` and `Ctrl+p` does nothing, bound or not. And tmux's default prefix swallows `Ctrl+b` before the sidebar ever sees it, which is why `pageup` rides alongside it in the default.

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
| `stalled_after_secs` | `1800` | When a silent running agent, including native and launched children, escalates to the actionable `!` bucket. The same window covers a launched child given a task whose first turn has not started, and an unfinished team stage whose members are all idle or done, measured since the latest member activity or stage start. A native child silent for three hours leaves its card, so a window of `10800` or more never flags one. |
| `tool_repeat_warn_after` | `3` | Identical tool calls in a row before `⟲` appears on the selected or held card's stats line, or under `expanded` density. |
| `tool_repeat_attention_after` | `20` | Identical tool calls in a row before that agent escalates to `!`. |
| `inactive_after_secs` | `3600` | How long calm and parked cards stay hot after their last activity. Asks and failures hold their place while the agent is alive. |
| `archive_after_secs` | `86400` | When calm and parked cards sink below hot and warm work. Asks and failures do not archive. |

Keep `archive_after_secs` above `inactive_after_secs`: a lower value is lifted to the first second after the inactive window.

### Multiplexer room options

RimZ applies room-scoped multiplexer settings when it creates or reattaches a session, so the room behaves the way agents need without your editing the global Zellij or tmux config. These tables are the only ones this page does not list in full: `rimz config init --print` gives every key with its default, and the per-backend mapping is in [multiplexers.md](../internals/multiplexers.md).

```toml
[mux]
default = "tmux"

[zellij]
pane_frames = true            # an optional override; unset, your config.kdl wins

[tmux]
pane_border_status = "top"    # optional; unset: inherit, except stack-window titles
```

`[mux] default` picks the backend, consulted after an explicit `--mux <name>`, its `--zellij` and `--tmux` shorthands, and the check for a Zellij or tmux you are already inside. Left unset, RimZ takes tmux when both are installed. If the configured backend is not installed, RimZ skips the setting and auto-detects an installed backend; room start warns. An already-open room keeps its backend until it is reset.

The two backends differ in what an unset key means.

`[zellij]` asserts four keys in every room, whatever you set them to. Their defaults are what make the room work, and changing one asserts your value instead.

| Key | Default | Why RimZ sets it |
| --- | --- | --- |
| `mouse_click_through` | `true` | One click on a card jumps to that agent. |
| `focus_follows_mouse` | `false` | Keeps that click reaching the renderer. |
| `session_serialization` | `false` | RimZ owns rebirth; a resurrected Zellij room comes back with dead panes. |
| `disable_session_metadata` | `true` | Stops Zellij's per-second metadata and `ps` loop. |

Every other key, `pane_frames` and `copy_clipboard` among them, is asserted only when you set it and otherwise falls through to your `~/.config/zellij/config.kdl` at room birth. Six behaviours are not keys at all: the room starts in locked mode, a directionless new pane splits the focused pane rather than the largest one, swap layouts are off, resizing stacks panes rather than shrinking them, every view carries RimZ's own layout with its one-row compact bar in place of Zellij's built-in tab and status bars, and the sidebar pane is always borderless so its hit-testing stays stable whatever `pane_frames` says.

A session value here overrides `config.kdl` absolutely. Most changes apply on the next `rimz start` or reattach; start-only settings and removal of optional keys wait for a new room. Client settings apply per attach, but `mouse_mode = true` cannot override `mouse_mode false` in `config.kdl`. See [change what the room asserts](./multiplexer.md#change-what-the-room-asserts) for the start-only keys and how to apply changes.

`[tmux]` carries a RimZ default for every key except the two pane-border keys, so other unset keys mean RimZ's value rather than yours. Changed window settings reach every open window on the next open or reattach, and future windows match. Removing a pane-border key restores inheritance from `~/.tmux.conf` or tmux's defaults on that next apply, except in a window holding a stack. With `pane_border_status` unset, a stack window gets top borders naming its members and blanking every other pane, including the sidebar; closing the last member returns control to your config. Setting `pane_border_status` to `"top"` or `"bottom"` also hands RimZ `pane-border-format`, which titles work panes and blanks the sidebar's border row. An explicit `"off"` disables borders, including stack borders, and inherits your format. The table spans session, window, and server scope, clipboard and rich-key handling included, because tmux has no per-session form for those.

Three tmux behaviours are not keys either: window titles (`set-titles` with the `set-titles-string` that carries a pane's title to the tab), and the server `terminal-features` entries for synchronized output and for hyperlinks. Two more are governed by `extended_keys` rather than standing alone: the `extkeys` terminal feature, which RimZ unsets when you turn the key off, and the three root key bindings for Shift+Enter, Alt+Enter, and RimZ's own `User240` signal, which it stops writing.

To configure your own Zellij or tmux, the theme, truecolor, copy-mode, and keybindings RimZ leaves to you and the sessions you run outside the room, see the [Zellij](./multiplexer.md#zellij) and [tmux](./multiplexer.md#tmux) baselines.

### Accounts

```toml
[accounts.claude.work]
home = "/home/you/.claude-work"

[accounts.codex.personal]
history = "standalone"

[accounts.use]
codex = "personal"

[accounts.budget]
claude = "100/day"

[accounts.usage_limit_usd]
claude = 50.0
codex = 25.0
```

`[accounts.<kind>.<name>]` declares a named Claude, Codex, or Pi account: a separate provider home that a room launches that provider's agents into. `home` is optional, and an empty table places the home under `~/.rimz/accounts/<kind>/<name>`. `history` is `shared` unless you write `standalone`: a shared account keeps its credentials and per-account state in its home and links everything else, sessions and transcripts included, to the provider's own home, while a standalone account links its settings and configured packages. An optional `label` changes only the dashboard title's account suffix, never the account's name or selection; labels may repeat ([label rules](../reference/cli/accounts.md#kinds-names-and-homes)). `default` is reserved for the provider's own home and is never declared or labelled. `rimz accounts add` writes these entries for you, and [provider accounts](./accounts.md) walks the whole flow.

Two limits apply to the path. Two accounts of the same provider cannot share a home, and a `home` cannot contain `,` or end in a directory named `projects`, because provider home lists split on commas and Claude reads `projects` as its transcript folder.

`[accounts.use]` selects the machine default for new rooms and existing rooms following it. Each key names a provider and each value names a declared account or `default`, the provider's own home. Room pins and trusted project selections take precedence. It coexists with the account declarations; a provider name directly under `[accounts]` is not a selection in the machine file. Names are checked when a room resolves that provider's account, not when the file loads, so you can still repair a missing declaration. Prefer `rimz accounts use --global`, which checks a named account's home and hooks before changing the default.

A project supplies live defaults in `.rimz/config.toml` with `[accounts]` entries such as `claude = "work"`. That selection joins the project trust hash, so an untrusted or stale one refuses launches for providers following defaults until you grant it. `rimz start --account` or `rimz accounts use` pins a provider instead; `rimz accounts use --reset` removes that pin. The [accounts guide](./accounts.md#start-a-room-on-an-account) explains precedence and per-provider refusals.

`budget` is the enforced day or month cap from [dollar budgets](#dollar-budgets), and an entry for a provider that cannot supply real account spend refuses that provider's launches rather than quietly doing nothing. Room start warns instead. `usage_limit_usd` is a different key with no such gate: it is display-only, so Cursor is valid here. Its ceiling scales the provider dashboard's `ex`/`api` bar when the provider reports neither a real cap of its own nor unlimited credit, while the provider keeps enforcing actual spend and the agents keep running. A counting `/month` budget replaces that ceiling on an API row, using UTC-calendar-month spend instead of trailing-month spend; a `/day` cap leaves it unchanged. The account figures RimZ shows are read locally, read-only, and best-effort; `RIMZ_OAUTH_USAGE_OFFLINE=1` turns off the live fetches for one process tree without touching transcript-derived totals or any credential file.

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

`enabled` defaults to true and gates `rimz web open`, `share`, `start`, `restart`, and `rimz remote connect --web`. While enabled, a normal `rimz start` makes a best-effort start of the shared daemon that every Zellij and tmux room on the machine uses; a missing or pre-1.7.5 ttyd, or an occupied port, warns without blocking the room. With `enabled = false`, those gated commands fail before any room or daemon changes and tell you to change the config on the machine serving the room. An already-running daemon stays up until `rimz web stop`; `status`, `stop`, and all `token` operations, including revocation, remain usable while disabled. Install ttyd 1.7.5 or newer with `brew install ttyd` or an apt source carrying a current package.

`interface` is the public bind address for both daemons. `port` (8200) is the writable sign-in gate, and `share_port` (8201) is the read-only broadcast daemon, which takes no login at all. `base_url` is the URL prefix RimZ prints for `rimz web open` and `rimz web url`, and `share_base_url` the one it prints for `rimz web share`.

Three fields apply to the writable daemon alone. Its gate normally checks [named sign-in tokens](./web.md#serve-a-local-room). A non-empty `auth_header` instead hands sign-in to a proxy-injected header. `auth_users` narrows that header to an exact-match list of your identity provider's canonical usernames; both the configured entries and the incoming header value are trimmed before comparison, and the match is case-sensitive. It requires `auth_header`. `trusted_proxies` takes bare IPs or IPv4 and IPv6 CIDRs allowed to reach the gate besides loopback. An empty list accepts any peer in token mode but only loopback in trusted-header mode.

`style_client` passes the active theme and the `font` family to both browser terminals. Turning it off does not take you back to ttyd's stock page: RimZ still generates the browser page, which is what maps a `?room=` link to the right room. The built-in `JetBrainsMono Nerd Font Mono` and `CaskaydiaCove Nerd Font Mono` families download and cache verified regular and bold faces on their own; `font_source` instead names one local font file or HTTPS font URL, and an unrecognized `font` with no source passes through for the browser to resolve.

No field here runs a command, so the section stays outside the project trust hash. The commands are in the [web guide](./web.md) and the [web reference](../reference/cli/web.md).

### Remote control

```toml
[remote_control]
claude = false
codex = false
```

These two switches opt the machine into the host each provider's official mobile app talks to: with a host up, a remotely reachable agent's ask pushes to your phone and you answer it in that app ([remote → answer asks from your phone](./remote.md#answer-asks-from-your-phone)). Both default to false. Claude's host runs in the `rimzd` daemon view; Codex's toggle keeps its daemon up for sessions started from the phone or outside RimZ. Codex agents RimZ launches run embedded (`--no-daemon`) and do not appear in the app.

Set either one with `rimz config set`, not by editing the file, because `set` also applies the change to the rooms already running. `rimz config set remote_control.claude true` adds `claude remote-control` to every running room's `rimzd` view at once when `claude` is on `PATH`, and `false` closes those host panes; a whole view you closed on purpose stays closed until the next room start. `rimz config set remote_control.codex true` starts the Codex daemon at once and `false` stops it. Either way the running sidebars wake so the provider dashboard's flag follows the file.

A hand edit does not reach a running room the same way. A Claude host catches up on the daemon view's next repair pass, within about thirty seconds. A Codex edit only changes what the next room start decides, so a daemon already running keeps running.

A `codex` CLI on `PATH` adds its per-session app-server broker whatever this toggle says. When an enabled host is installed but misconfigured, such as an incompatible Claude version, room start warns and runs that host off for this run without asking or editing the file. The dashboard shows a muted `rc off`, and `rimz doctor` keeps the warning and fixes visible. When the host's account is unavailable, the remote-control warning points at the account warning instead of repeating it. Re-entering a live room warns without changing its applied policy. An enabled host whose agent is not installed at all is skipped, and `rimz doctor` reports the install fix. The live Claude enable command still refuses a blocked host. The security boundary is in [security.md](./security.md).

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

## Model tiers

A concrete model names what to run; a model tier names the capability the work needs. Set an ordered list once in `~/.rimz/config.toml`, then change that list instead of editing every tiered definition. A concrete `model:` runs exactly as named, whether listed in the table or not; only `tier:` permits routing. See [choosing a model tier](#choosing-a-model-tier) for the four levels and family guidance.

These are the defaults; the generated template leaves them commented:

```toml
[tiers]
routing = "fallback"
principal = ["fable"]
senior = ["opus", "astra"]
junior = ["sol", "sonnet"]
intern = ["luna", "haiku"]
```

List order is preference. A written tier replaces that tier's whole list; `[]` empties it, and unwritten tiers retain their defaults. Entries must be recognized Claude or Codex aliases or full model IDs. Each entry may belong to only one tier; an alias and a full model ID are distinct entries. Aliases resolve at launch through [model alias pins and the account catalog](#model-aliases). Old per-family tables are refused with the replacement list syntax; effort belongs in definitions, not the table.

In an agent or subagent Markdown definition, replace `model:` with `tier:`. An optional `agent:` prefers a family rather than pinning it:

```yaml
agent: codex
tier: senior
effort: high
```

Tier resolution tries the preferred family's entries first, then the remaining entries, then higher tiers with the preferred family first. It never moves down. With no preference, `tier: senior` selects Opus; the example above selects Astra. `agent: codex` with `tier: principal` selects Fable. Families missing a kind base or unable to support the definition's tools or skills are excluded; `rimz agents validate` warns with the reason. An exclusion ahead of the chosen entry is recorded as an `unbuildable` pass with its full build error; later exclusions stay static validation information. No eligible model at or above the requested tier is an error. A concrete definition instead fails with its path, build error and fix; a listed model also gets a `tier:` hint for allowing a move.

At launch, `routing = "fallback"` skips a tier entry only when its cached account is logged out, a matching usage window is exhausted with nothing left to spend, or its applicable account cap is reached. A spent subscription window does not skip an account that still has usable paid credit or a banked Codex reset that [auto-redeem](./loops.md#auto-redeem) would spend; a spent per-model cap skips that model regardless. Missing readings do not block a model. A concrete `model:` never falls back for usage: it launches on its own family under the normal launch gates, even on a spent account. If every eligible tier entry is unavailable, RimZ retries the first buildable choice under the normal launch gates, retaining only build passes ahead of it. Set `routing = "off"` to ignore usage, not buildability. Profile listings stay usage-free; `rimz agents explain <profile>` previews the current launch choice without launching, and `rimz stats` counts launches moved by build or usage fallback.

Set an absolute `effort:` or omit it for the chosen model's default. Relative efforts such as `+1` and `-1` are refused. `tier:` and `model:` are mutually exclusive and inherit as one setting: a child setting either replaces both inherited values. The [definitions reference](../reference/definitions.md#chains-and-defaults) specifies inheritance and team roles.

Reorder a tier with `rimz config set tiers.junior '["sonnet", "sol"]'`. Inspect the result with `rimz agents profiles` or `rimz agents explain <profile>`; an upward move is marked `fallback` in listings. Override one launch with `--tier senior`, including on a concrete definition, or run an exact model with `--model opus`; the two flags conflict. A concrete CLI model normally uses its family's Markdown render and refuses if that family cannot build the definition; a conflicting `--agent` family is an error. Pi and OpenCode definitions keep their engine under `--model`; `--agent pi` or `--agent opencode` also runs the requested model without requiring its native family's render. Use `--agent` to change that engine; `--tier` still walks the table's families. The table is machine-only; project TOML profiles and roles must name concrete models and do not route through the table.

### Choosing a model tier

| Model tier | Use it for |
| --- | --- |
| `principal` | Designs without precedent, or supplying structure the request left out. |
| `senior` | Most real work: planning, orchestration, review, implementation, and debugging. |
| `junior` | Clear, bounded work that a more capable parent checks. |
| `intern` | Sweeps over a large corpus that return a target, map, or excerpt. The parent owns every conclusion. |

Prefer Claude for people and loose intent, and Codex for literal briefs. Put a reviewer on a different family from the coder whose work it reads. A Claude planner can weigh its design against a Codex designer's proposal before settling the plan.

## Model aliases

Like `codex --model`, a provider model ID selects one specific model. RimZ's Codex aliases (`sol`, `astra`, `luna`, and `terra`) instead follow their model family on fresh launches. When you want a stable choice across those launches, pin the alias in your per-machine `~/.rimz/config.toml`:

```toml
[models.codex]
sol = "gpt-6.1-sol"
```

Or write the same setting with:

```sh
rimz config set models.codex.sol gpt-6.1-sol
```

The pin wins over the account's model catalog and is never automatically followed to a newer model. Remove the `sol` entry from `[models.codex]` to follow the family again. A RimZ-recorded launch resolves its requested alias again on restart, resume, or fork, so a changed pin applies. Legacy sessions without a launch record may keep their observed model; use `--model` to override it. Pi and OpenCode instead keep their native session's model on plain resume or restart; Pi also does so on fork.

Each `[models.<kind>]` table maps any alias name to a non-empty provider model ID. Other providers work too, for example `[models.claude]` with `opus = "chosen-model-id"`. A custom alias in a Markdown definition needs an explicit `agent:` when RimZ cannot infer the provider from a built-in alias. This table belongs only in machine config; project `.rimz/config.toml` refuses `[models]` and asks you to move it.

## Agent definitions and launch preferences

Reusable agents and teams are Markdown files with YAML frontmatter, one per definition. The `[agents]` tables in `config.toml` hold this machine's launch preferences alongside them: isolation, placement, command shortcuts, worktree defaults, and the two launch limits.

### Agent isolation

A stock agent CLI uses the host's temporary directory and sees every skill installed for that provider. To give a room's agents one shared temporary directory, and to choose which user-level skills a profile may invoke on its own, turn on the Linux bubblewrap mount view:

```sh
rimz config set agents.isolation scoped
```

That probes bubblewrap and then writes `isolation = "scoped"` under `[agents]` in `config.toml`. The default is `host`, and `rimz config set agents.isolation host` restores it without disturbing any profile `skills` list. For a profile that needs host access, such as a system administrator using `sudo`, set `isolation: host` in its Markdown definition. A recorded `--isolation` flag wins over the profile default, which wins over machine policy. Restart re-reads the profile; panes already open keep what they started with. Off Linux there is no scoped view, so a `config.toml` or profile shared from a Linux machine that asks for `scoped` runs on the host instead, with a warning naming the setting. An explicit `--isolation scoped` still refuses.

`rimz config set agents.isolation sandbox` instead adds each provider's own sandbox flag to later launches, without a mount view: `sandbox.enabled: true` for Claude Code, and `--sandbox workspace-write` for Codex when the launch selects no sandbox mode of its own. A provider without a sandbox switch warns and launches without one. Saving the setting checks nothing. The [security guide](./security.md#provider-sandbox-isolation) lists the flags and links each provider's documentation.

To try one launch another way, pass `--isolation host`, `--isolation scoped`, or `--isolation sandbox` to `rimz agents`, `rimz teams`, or `rimz subagents`. That agent keeps the choice through restart and resume. Children inherit a recorded override and never run looser than their parent's effective level in the order host < scoped < sandbox: a looser explicit override refuses, and a looser default clamps with a note. Codex keeps its own sandbox and network choices at every level, apart from the `sandbox` level's default above. RimZ adds only room-directory writes where the selected permissions support them. Auto escalates other access to Guardian, Ask to you ([permission modes](../reference/agent-support.md#permission-modes)).

Every launched agent carries three variables, so a script or skill inside the pane can read them instead of guessing: `RIMZ_ISOLATION` is `host`, `scoped`, or `sandbox`, `TMPDIR` names the agent's temp directory, which its subagents share (`/tmp` in the scoped view, a host path otherwise; Claude's sandboxed commands use Claude's own temp directory), and `RIMZ_SHARED` names the directory the whole room shares for files agents hand each other (the same host path at every level). A process that outlives the agent's command, such as a language server, a notification handler, or a room started from the pane, gets your own `TMPDIR` back; a `rimz wait --run` command is the agent's own work and keeps the agent's. What the scoped view does and does not hide is [security → scoped isolation](./security.md#scoped-isolation).

### Agent hibernation

Many idle agents can fill a small machine's RAM. On Linux, RimZ hibernates eligible idle agents by default: it freezes the provider's systemd user scope and asks the kernel to reclaim its memory to swap. To change the idle grace, edit `~/.rimz/config.toml`:

```toml
[agents.hibernate]
after = "30m"
deep_after = "120m"
```

The defaults are `enabled = true`, `after = "30m"`, and `deep_after = "120m"`. Both durations accept CLI duration syntax up to 24 hours; `deep_after` must be at least `after`. After the idle grace, RimZ freezes the scope and asks the kernel to reclaim half its anonymous memory. Every 30 minutes it lowers the resident target, reaching zero at `deep_after`; a final shorter interval is allowed. Equal durations request full reclaim at the freeze. Reclaim is best-effort, so the kernel may leave more memory resident than the target.

When hibernation is available, newly launched providers receive scopes at every isolation level. Already-open providers without a scope do not hibernate. Set `enabled = false` to stop new hibernations and deepening. Navigating to a hibernating pane or clicking its card wakes it after about a second, or sooner when you type or click in it; passing through does not wake it. A look without a new turn lets it freeze again about 10 seconds after you leave; messages, answers, and commands naming a pane wake it at once and restart the full `after` grace. `rimz agents show` reports the current step and cumulative reclaimed memory. If waking fails, it also reports the error until a later successful wake.

Linux needs systemd's user manager, delegated memory controls, and swap visible in `/proc/meminfo`. In LXC, give the container a swap allowance too: an unlimited visible cgroup setting alone does not prove swap is available. Where hibernation cannot work, including off Linux, RimZ silently skips it; `rimz doctor` says why and lists fixes. An explicit `enabled = true` adds one warning at `rimz start`, but never prevents startup for a missing capability. An invalid duration disables hibernation instead of using a default schedule. After fixing the schedule or capability, run `rimz start` again to activate hibernation. See [the sidebar lifecycle](./sidebar.md#the-agent-lifecycle) for eligibility and waking.

### Agent profiles, commands, and teams

Four directories under `~/.rimz/` hold the definitions: `agents/` for the profiles you launch directly, `subagents/` for supervised children, `teams/` for rosters and their pipelines, and `traits/` for prompt fragments several definitions share. One file is one definition, named after it, and deleting the file removes the definition; agents already running keep the launch they started with.

For Markdown definitions, every profile and team seat on a provider that takes a system prompt needs a [kind base](../reference/definitions.md#kind-bases) in `agents/`, named after the kind, such as `agents/claude.md`. That includes Claude, Codex, OpenCode, Pi, OMP, Qwen, and MiniMax, whose prompt appends to its own. A provider without a system prompt takes neither a base nor a craft and cannot fill a team seat; a bodyless standalone profile needs no base. A failed base also fails the Markdown profiles that depend on it. A trusted project profile in `.rimz/config.toml` that inherits a failed base still loads, but refuses fresh launch, restart, and fork with the base's error; one named after the kind replaces the base and keeps launching. [`rimz agents validate`](../reference/cli/agents.md) checks the Markdown set and names what is wrong, and the [definition reference](../reference/definitions.md) owns every frontmatter key, inheritance rule, and error.

#### Profiles

A profile is a named agent preset, loaded from `agents/<name>.md` for one you launch directly or `subagents/<name>.md` for a supervised child. Its `agent:` chain stays inside its own tree, and a field the child sets explicitly replaces the inherited one. Parent craft bodies come before the child's; `description` and `traits` are local to each file, and `model-reminder` is inherited. Where profiles live and how you use one are the [agents guide](./fleet.md#profiles-shape-an-agent-for-one-job); every field, default, model alias, and composition rule is the [definition reference](../reference/definitions.md#chains-and-defaults).

`subagents: [explorer, designer]` on a direct definition limits which children RimZ will delegate to; drop the native `Agent` tool at the same time. Omitting the field leaves delegation unrestricted, `[]` permits none, and a child definition cannot set it at all. The launch reminder names the available children when the [skill policy](#skills) permits `rimz-subagents`. When that catalog has a launchable entry, RimZ switches off the provider's native delegation tool where supported, even if `tools:` lists `Agent`. An explicit skills list without `rimz-subagents`, or a catalog with no launchable children, keeps native delegation; a kind base counts as a launchable child. The reminder also names the selected model for solo agents and subagents by default; `model-reminder: false` removes only that model line. Team members never show model names, regardless of this setting.

`auto-compact` sets the provider's own native compaction window, a token count from `100k` through `1M`, never a percentage. Profiles without a value inherit the machine [`auto_compact`](#native-auto-compaction) default on the kinds that support it ([provider support](../reference/agent-support.md#auto-compaction-window)). A role's `flip-compact` is a different field, covered under [hand-off compaction](#hand-off-compaction).

`fast: true` launches the profile in the provider's fast mode, which serves turns sooner at a higher price; reach for it on a profile whose turns you sit and wait on. Claude, Codex, Amp, and Pi support it (Pi only on supported OpenAI models); on any other kind the launch warns, names the alternative such as a fast model ID on Cursor, and runs without it. Leaving `fast` off adds nothing to the launch, so a fast mode you set inside the provider still applies. The exact switch each provider receives is in [provider support](../reference/agent-support.md#fast-mode).

On the command line, `--model`, `--effort`, `--budget`, and `--system-prompt-file` override the resolved profile, and repeated `--append-system-prompt-file` values replace its fragment list for that launch while leaving the team layer intact. `--agent <PROFILE|KIND>` swaps the engine and keeps the seat identity; the carry-over rules are in the [launch reference](../reference/cli/agents.md#shared-launch-params). Markdown definitions accept no raw `args` and no prompt-path keys: tools generate the arguments, and bodies supply the prompt.

#### Skills

Two separate things share the word. `~/.rimz/skills/` is a library you fill, shared across providers; a profile's `skills:` list decides which skills that agent may invoke on its own.

**The library.** Put a directory containing `SKILL.md` in `~/.rimz/skills/` and every provider can reach it without a copy. Each host launch links `<root>/<name>` to the library entry inside that provider's skill root: Claude, Qwen, Kiro, and Hermes read their config home's `skills/`, other built-ins read `~/.agents/skills`, and plugins declare no root at all. A named Claude account home gets its links on its first host launch. If `RIMZ_AGENTS_HOME` already puts the library at a provider's own skill root (`RIMZ_AGENTS_HOME=~/.agents`, say), that provider reads it natively and RimZ writes no links.

Your own directories and links are never replaced, and a name collision is reported at launch. Delete a library skill and its link goes at the next host launch for that root; `rimz uninstall` removes RimZ's links from every provider and declared account home and keeps the library itself. A scoped launch merges the same library through the skill view instead of linking, with the provider's own entry winning a name collision.

**The list.** In scoped mode, `skills: [merge, review]` in a Markdown profile keeps those two model-callable and turns every other skill RimZ can prepare into one you invoke by name. `skills: []` makes all of them user-invoked; it does not hide the view. Entries are bare directory names, never paths. A child list replaces its parent's rather than appending, omitting the field inherits, and once a parent sets a list no child can go back to unconfigured behaviour. With no list anywhere in the chain, invocation behaviour is the provider's own, except that a [launched subagent](./subagents.md#children-cannot-delegate) withholds `rimz-subagents` wherever skill restrictions are supported.

The tools and skills lists together decide which RimZ skills the [launch reminder](../internals/harness/fleet.md#launch-reminders) names, in every isolation level. On Claude, Codex, or Pi, an explicit `tools:` list without `Skill` makes no skill callable, regardless of the skills list. Otherwise, no skills list makes every skill callable for the reminder; a list makes only its exact names callable, without checking installation. The Subagents catalog requires callable `rimz-subagents`, and the Environment language-server bullet requires callable `rimz-lsp`. Every child instead gets a fixed assignment-and-report paragraph with no skill names. A Skills paragraph tells the agent to invoke a skill whose description matches the work when tools permit skills and the skills list is either absent or names any `rimz-*` skill; `skills: []` and lists containing only other skills omit it. Without a callable replacement skill, RimZ leaves native delegation and LSP tools as the tools list renders them; children still have native delegation disabled.

Three rules decide whether a list loads. It requires the `Skill` tool, except on a Pi definition that leaves out `tools`. Every listed name must resolve to an installed skill, searched in the provider's skill directory first and the RimZ library second; a name in neither fails and names both paths. And listing a skill cannot lift a restriction its author set, so a skill already marked user-only is refused with two fixes: drop it from `skills:`, or remove the marker.

On Claude, a list also lets the agent invoke those skills without a permission prompt, in host and scoped mode, even when `allow-routine-rimz` is off. Names containing `*` receive no permission rule. Claude's managed policy and explicit ask/deny rules still apply.

When one role needs recurring permission, such as a fixer running Git commands, put `allowed-tools: ["Bash(git *)"]` in its Markdown definition or team role. This pre-approves calls on Claude; `tools` still decides which tools exist. See the [standing-grant implications and removal steps](./security.md#what-can-run-a-command) and the [definition syntax](../reference/definitions.md#tools-to-provider-arguments).

On the host, Claude keeps unlisted skills user-only through `--settings skillOverrides`; Codex hides them through `-c skills.config`, including from explicit `$skill` invocation. Other providers warn and run unrestricted. Neither switch has a wildcard, so skills installed after launch stay callable until restart. Your Codex `config.toml` skill restrictions remain in effect. In scoped mode, Antigravity, Amp, OpenCode, Kiro, Grok, OMP, and plugins refuse a list outright; remove the field to launch them. Changing a trusted project's skill list means granting trust again. The provider-by-provider table is [agent support](../reference/agent-support.md), and the exact merge rules and limits are in [sandbox internals](../internals/sandbox.md#host-mode).

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

A staged team's seats coordinate through the board and stage notes in `.rimz/team/`, a link to an archived run in room state. Every launch or resume excludes the link from Git. `scratch-files` declares extra checkout-local excludes only; it does not change the memory listing or archive. The [teams guide](./teams.md#hand-off-with-one-command) owns the run lifecycle.

#### Inline specs and cell resolution

An inline spec such as `rimz agents "claude,codex+term"` names the cells to launch and how to arrange them: commas split columns, plus signs tile rows, and slashes stack rows with one expanded member on either backend. Each cell resolves in this order, first match winning:

1. `[agents.commands]`,
2. direct profiles from `agents/`, including a trusted project's overrides,
3. the built-in `term`,
4. a registered agent kind,
5. an adapter-supported `<kind>-<mode>` cell (`claude-auto`, `codex-ask`, `codex-yolo`, and so on; the [agents reference](../reference/cli/agents.md#permission-mode-cells) lists which kinds have which),
6. a `<base>-fast` cell over a profile, kind, or `<kind>-<mode>` cell, launched in the provider's fast mode ([fast cells](../reference/cli/agents.md#fast-cells)); a base whose kind has no fast switch warns and launches without it, never reaching step 7,
7. an executable of that name on `PATH`.

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

An agent that runs `rimz agents` or `rimz teams` launches independent top-level peers, not children. `max-chain-length` caps how many of those launches can chain from one human-started root, and defaults to three. A launch past the limit fails before it creates a pane, a worktree, or a provisional agent, and tells the calling agent not to retry. [`rimz agents fork`](../reference/cli/agents.md#fork) is not a launch: the fork takes the chain position of the agent it copies, so a fork of an agent at the limit cannot launch peers either, and a fork is never refused by the limit. `rimz agents molt` replaces the calling agent at the same generation, so repeated molts do not consume the limit.

#### Startup relaunches

```toml
[agents]
startup-relaunches = 3
startup-relaunch-wait = "3s"
```

An agent CLI sometimes dies while it starts, on a provider error that is gone a few seconds later. When a freshly launched agent's CLI exits with an error before its session opens, RimZ says so in the pane, waits `startup-relaunch-wait`, and launches it again in the same pane under the same name, up to `startup-relaunches` times. Ctrl-C during the wait cancels the relaunch. A configuration refusal (exit 78) is never automatically relaunched; fix the reported cause first. RimZ never reads the error text, so other repeatable CLI failures are launched four times before the failure is reported. Resumed sessions, forks, and provider session pickers are never automatically relaunched; [picker exits](../reference/cli/agents.md#resume-a-provider-session) have their own hint rule. `startup-relaunches = 0` turns only the relaunch off; the wait takes a whole number of `s`, `m`, `h`, or `d` from `"0s"` (relaunch at once) through `"24h"`. While relaunches are on, a wait outside that range stops fresh launches through `rimz agents`, `rimz teams`, and `rimz subagents` before any pane opens, with the key and the fix. Each relaunch is a `Launch retry` in [`rimz stats`](./insight.md).

#### Environment at launch

Agents receive their launch cwd and the kind of shell their commands run under (`zsh`, `bash`) by default, so they can start with their environment in view. A worktree launch also learns its base branch, when recorded, and where the primary checkout is. Current git state is left out of the launch text: it can change between launch and the agent's first turn. To omit these optional environment bullets for your machine, set this in `~/.rimz/config.toml`:

```toml
[agents]
env-reminder = false
```

Claude and Codex agents get the facts that change instead with the prompt. The first prompt of each conversation, and each stage notice a team member receives, carries the current branch, the short status, and the last five commits, and for a team member the team's memory files as they are at that moment. The text you typed is recorded unchanged. This travels through RimZ's hooks, so a Claude or Codex agent whose account has no RimZ hooks installed, or whose Codex hooks are not yet trusted, is treated like the other agents: they keep the memory-file list in their launch text and read git themselves. To put the list back at launch for Claude and Codex as well and send nothing with the prompt:

```toml
[agents]
runtime-env = false
```

`runtime-env` is a machine setting; a project's config cannot change it. `env-reminder` is the one a project can: a trusted room's `.rimz/config.toml` can set `env-reminder` to override the machine value in either direction. With `env-reminder` off, team members still receive cwd and their memory-file listing, and a shared language server still appears when the [skill policy](#skills) makes `rimz-lsp` callable. `rimz agents explain <spec> --prompt` shows the reminder before launching. See [launch reminders](../internals/harness/fleet.md#launch-reminders) for the full reminder.

#### Agent shell

Agents launch through your login shell, so its startup files set their PATH and environment. The commands a model writes usually assume bash, though: under zsh, an unquoted `$files` stays one word and an unset list behaves differently, so runs fail or get redone. To run every agent under one shell while your own shell panes keep yours, set this in `~/.rimz/config.toml`:

```toml
[agents]
shell = "bash"
```

Each launch then runs its wrapper under that shell and sets `SHELL` and `CLAUDE_CODE_SHELL` to the resolved path, so a provider that picks its tool shell from either uses it too. Claude Code honours them only for bash or zsh: under fish its tools fall back to zsh or bash ([Claude's tool shell](../externals/agent-adapter/claude-reference.md#tool-shell)). Use the name or full path of an installed bash, zsh, fish, or sh-family shell. A bare name is looked up once on the starting process's PATH when a new room session is born, and the room saves the hit without following symlinks. When a scheduled loop run starts the room, it instead clears any saved lookup so each launch uses its own PATH; `rimz loop fire` still saves it. Live re-entry and reload keep the room's choice. A full path pins that shell, and changing to a full path reaches the next launch without a restart. If the room has no saved lookup for the current name, or the saved path is no longer a file, each launch looks the name up on its own PATH instead. Relative paths containing a slash, missing binaries, `false` or `nologin`, and csh and tcsh refuse every launch with the fix rather than quietly falling back to your shell. Room start warns and records a configuration problem instead of refusing the room; a failed lookup saves nothing. Remove the line to go back to your login shell.

Bash runs as an interactive non-login shell (`bash -i -c`), so it reads `~/.bashrc`, not `~/.bash_profile`: put the PATH entries that find your provider CLIs in `~/.bashrc`. A supervised run (`rimz agents -p`) looks the provider up under the configured shell before it starts, so an entry only `~/.bash_profile` sets fails there, naming the provider.

This key is machine-only: project config cannot override it, so a repository cannot choose which shell runs its agents. `rimz agents explain claude --prompt` shows the wrapper, the two keys, and the `- shell:` line before launching.

#### Routine RimZ commands

Claude and Codex can otherwise pause for permission just to coordinate with another agent. RimZ defaults `allow-routine-rimz` to `true`. For Claude, it adds curated command permissions and the launch's scratch/shared directories through `--settings`, plus auto-mode context describing those commands and directories. Your Claude settings are preserved; RimZ does not edit `~/.claude/settings.json`. For Codex, `rimz hooks install` writes the same command prefixes into `$CODEX_HOME/rules/rimz.rules` (`~/.codex/rules/rimz.rules` by default); matching commands run unsandboxed without Guardian review. The file is shared by named accounts, including standalone accounts. See [permission modes](./fleet.md#set-a-permission-mode) for what this covers.

The switch also covers your installed `rimz-*` skills in Claude's skill directory or the RimZ library. Only exact directory names with a `SKILL.md` receive rules, never a wildcard grant. Turning it off does not remove the permissions from a profile's own `skills:` list.

To opt out, set this in `~/.rimz/config.toml`:

```toml
[agents]
allow-routine-rimz = false
```

This key is machine-only: project config cannot override it. It applies to every RimZ-launched Claude agent, including subagents, restarts, resumes, and forks, under host or scoped isolation and in every permission mode. For Codex, switching it off removes the managed rules file at the next `rimz start` consent or `rimz hooks install`; `rimz hooks uninstall` removes it too. Your own unmarked file is left alone during preview and install, never replaced or removed; your own rules still apply. Other providers are unchanged. `rimz agents explain claude` shows the launch settings without starting an agent; `rimz hooks install --dry-run codex` previews Codex's file changes.

#### Subagent launches

```toml
[agents.subagents]
timeout = "30m"
warn = ["6m", "3m"]
grace = "3m"
```

This table holds the defaults for [`rimz subagents`](../reference/cli/subagents.md), the agent-only doorway and the one launch path that creates a parented child. To give a child time to report instead of losing its work, supported providers receive warnings before the 30-minute work deadline. RimZ asks the child to stop at that deadline and allows three more minutes for reporting, then stops it with its last available assistant text as a partial response. Each launch can override the three values; the [deadline reference](../reference/cli/subagents.md#configure-the-deadline-ladder) covers delivery support and how to disable warnings or grace. A subagent cannot launch agents or subagents of its own.

A child's own preset is Markdown, in `subagents/<name>.md`. This table holds only the doorway defaults.

### Worktrees

```toml
[agents.worktree]
dir = "../{repo}-worktrees"
# base = "head"
```

`rimz worktree` and `rimz agents --worktree` create their Git worktrees under `dir`. A relative path resolves from the main checkout's root, and `{repo}` expands to that root's basename.

`base` is the ref a new branch starts from. Unset, [team launches](../reference/cli/teams.md#launch-a-team) branch from the remote default branch fetched at launch, while `rimz worktree new` and non-team agent launches use the main checkout's `HEAD`. Explicit `head` selects that local `HEAD` for every launch without fetching. `fresh` selects `origin/HEAD`: team launches fetch it first, other commands use the last-fetched tip. Any other string is handed to Git as a ref without fetching. These settings read the main repository, even when you launch from one of its linked worktrees, so `head` means the main checkout's branch, not the branch under your cursor.

For machine-specific setup that needs a command rather than a file copy, two optional keys live under `[agents.worktree.hooks]`:

| Key | Default | Value |
| --- | --- | --- |
| `created` | unset | A `sh -c` command to prepare each fresh tree after seeding. |
| `removed` | unset | A `sh -c` command to clean up after each tree is removed. |

Empty or whitespace-only commands are refused; remove a key to disable it. Both are settable and gettable with `rimz config set|get agents.worktree.hooks.created` or `agents.worktree.hooks.removed`. The [hook workflow](./worktrees.md#prepare-the-tree-with-a-hook) covers ordering, failures, and the script environment.

Seeding files into a new worktree and sharing directories by symlink are repo-level concerns that ride on committed files; they are in the [worktrees guide](./worktrees.md).

### Team signal bindings

A role's `signals` array declares which events wake it. The workflow is [teams → send events to the responsible role](./teams.md#send-events-to-the-responsible-role); this is the field. Inside a role mapping in `teams/forge.md`:

```yaml
signals:
  - signal: ci.failed
    match: {branch: feat-x}
    prompt: Read the failed job and repair it.
```

Each entry is either a bare selector string or a mapping that must carry a `signal` selector, and the role containing it is the receiver. A selector is an exact name or a family such as `ci.*`. Optional `match` is a map of string values that must all equal a top-level field of the event payload, except that `"*"` accepts any value when the field is present. Optional `prompt` is appended verbatim after the event evidence RimZ delivers. An `agent.*` binding additionally requires `match.handle` or `match.session` naming another agent, not a wildcard. A binding that does not check out fails team preparation and stays visible in `rimz teams show`.

Scope comes from where the member is working. CI and PR bindings default to that member's worktree, and team signals default to its cohort. Launching from a fresh root checkout with implicit CI or PR scope is refused before anything happens, because nothing would narrow the match: launch with `-w <worktree>`, work from a linked worktree, or give an explicit branch or path in `match`. To give one role project-wide coverage, use `match: {branch: "*"}` for every checkout RimZ watches, or `match: {team: "*"}` for every cohort. Quote the wildcard in YAML. The [loop reference](../reference/cli/loop.md#signals) owns the matching rules. `rimz teams show` keeps the declarations and the live registrations in separate lists, and a registration disappears when the session ends, is lost, or is stopped.

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

`default-timeout` bounds a scheduled run whose task omits `timeout`. It takes a positive `s`, `m`, `h`, or `d` duration, defaults to `2h`, and `rimz config set loop.default-timeout 3h` writes it; `0s` is refused rather than accepted as a deadline no turn can meet. A task's own `timeout` wins, and a manual `rimz loop fire` without one stays unbounded.

Every task you write here does one thing: `agent` runs one agent cell on a calendar, interval, cron, or one-shot schedule.

`check` guards that run. It is a shell command RimZ runs first: `on = "fail"` proceeds on a non-zero exit or a timeout, `on = "success"` on a zero exit. `check-timeout` caps it, else `timeout`, else five minutes. The output flows to any judges and then the agent's prompt when it fires. It runs in the task's `dir` when one is set, and at `root` otherwise; `rimz loop add` fills `dir` with the worktree you ran it from.

When the guard also needs judgment, add `judge` beside `check`. The profile must resolve to one Claude or Codex agent, with exactly one of `prompt` and `prompt-file`, an action, and explicit `on` on the task:

```toml
[tasks.pr_watch.judge]
agent = "haiku"
prompt = "Does this failure need a code change?"
timeout = "5m"           # optional; the judge's own cap, independent of the task timeout
# prompt-file = "prompts/judge.md"  # instead of prompt
# recheck = "6h"         # optional; resident tasks only, no default
```

Judges run without a pane and return a verdict and reason before the action. For several, use `[[tasks.pr_watch.judge]]` tables in the order they should run. The shell runs first, then each judge sees all earlier evidence; the first answered stage that misses `on` stops the chain. A judge without a verdict continues it. An old agent table under `check` still reads as the first judge. The [loop guide](./loops.md#guard-a-turn-with-a-check) explains judgment guards, and the [loop reference](../reference/cli/loop.md#waits-and-checks) lists the corresponding flags and refusals.

The rest tune what one fire is allowed to do:

- `overlap` chooses what a busy repeating clock or signal task keeps: `skip` (default), `coalesce`, `queue`, or `replace` (stops the active run for the newest trigger). `overlap-limit` bounds a queue, defaults to `10`, and must be at least `1`. Both are refused for resident layouts, conditions, one-shots, deliveries, and workflows. See [overlapping triggers](../reference/cli/loop.md#overlapping-triggers).
- `verify` runs a shell command after an agent turn and re-prompts that same session when it fails. `max-attempts` caps the agent turns one fire may take, and defaults to `3`.
- `max-strikes` disables the task after that many consecutive failed or no-progress fires. It defaults to `3`, and `0` turns the strike gate off. `rimz loop enable` clears the counter, which is machine-local.
- `account` holds an `agent` task on one named [provider account](./accounts.md) instead of the room's current one, and `"default"` pins the provider's own home. It is refused beside `wait` and on a check-only task. In a project task it stays out of the trust hash, since it names an account and runs no command.
- `budget` caps each run in dollars. `budget-per-day` requires `budget` and skips a fire when today's recorded spend for the task, plus the next run's cap, would pass the daily amount.
- `surplus` holds a task back unless the provider's usage window has room to spare: `"1.5x"` fires only while the budget left in the window outpaces the time left in it by half again. `surplus-after` adds an elapsed floor, and alone it still demands the break-even `1.0x`. Both fail closed when RimZ has no usable window reading, so a plain API key never fires a gated task. The model, worked through with numbers, is [budgets → the surplus gate](./budget.md#the-surplus-gate).

A task that starts an agent also takes a turn in the start throttle, tuned by the file's top-level `[throttle]` table: `pace` (default `"10s"`, `"0s"` off) and `max-wait` (default `"30m"`), the caps `max-active` and `max-active-per-task`, the pressure limits `cpu-pressure`, `io-pressure`, and `memory-pressure` in percent, and the floors `min-memory` and `min-disk`. `rimz config set loop.throttle.pace 30s` writes one. A task's own `throttle = "off"` exempts it. The keys are explained in [pace agent starts](./loops.md#pace-agent-starts).

A task can start a [workflow](./workflows.md) run instead of an agent. `workflow` names a definition under `~/.rimz/workflows/`, `prompt` or `prompt-file` becomes the run's prompt, and `each-worktree = true` starts one run per worktree. `rimz loop add` has no flag for it, so this is the one action you only write by hand:

```toml
[tasks.babysit-pr]
workflow = "babysit-pr"
root = "/home/you/code/app"
each-worktree = true
when = ["pr=open"]
```

A workflow task refuses `stay`, `takeover`, `subscribe`, `check`, and `judge`, because the workflow's own steps and checks own them, and it takes no start-throttle turn. The [workflow reference](../reference/cli/workflow.md#loop-tasks) lists every field.

Four things are worth knowing before you edit the file by hand:

- `at` and `cron` resolve in the top-level [`timezone`](#sidebar-rendering), falling back to the system zone when it is unset.
- A machine task carries a `root`. `rimz loop add` writes an absolute path, and a hand-written `~` or relative root is normalized before RimZ matches it to a room, fires it, or displays it.
- A project task runs at the project root implicitly. It resolves `prompt-file` and `system-prompt-file` relative to `.rimz/`, rejects `root`, `dir`, `wait`, and `deadline`, and requires `every`, `cron`, or `signal`, because a one-shot is machine state and does not belong in a committed file.
- A trusted project task outranks a machine task of the same name, but starts disabled until you enable it locally. An untrusted or stale project task stays visible and inert, so the machine task keeps running until you run `rimz trust grant`. `rimz loop add --project` writes `.rimz/config.toml` and enables the task for whoever added it; removing or renaming a project task means editing that file.

Two more task fields exist that you never write here. A `wait` task delivers to one live agent session rather than launching a cell: `rimz loop add --wait @handle` writes it into the workspace's instance state, and bare `--wait` or `--wait @me` targets the caller. `deadline` is what `rimz loop add --until 30m` writes for a task that polls until a time. Both appear in `rimz loop show` and never in `loop.toml`.

That instance state is one of two files outside `loop.toml`. Every session delivery, generated one-shot, and poll-until instance lives in `~/.rimz/ws/<workspace-dir>/records/loop-instances.json`; machine-local enablement and bounded pauses live in `~/.rimz/loops/loop-arming.json`.

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

`<repo>/.rimz/config.toml` is committed, and it declares the workspace shape a team shares. On a trusted workspace RimZ injects each `[[agents]]` `env` table into that agent's process at launch, applies top-level `[profiles]` and `[agents.teams]` to `rimz agents`, applies `[subagents.profiles]` to `rimz subagents`, overlays `[agents] env-reminder`, and loads `[tasks]` for `rimz loop`. Pick one `agents` shape per file: `[[agents]]` for env entries, or the `[agents]` table for `env-reminder` and shared `[agents.teams]`.

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

Two fields enter the trust hash but do nothing yet: `launch_command` on an `[[agents]]` entry, and a `[[hooks]]` table. RimZ neither runs the command nor installs the hook today. One table is refused outright: a `[layout]` table fails the load with an error naming what replaced it, `placement` under `[agents]` for where a launch lands and a team definition for a whole room shape. Per-machine config has no `[layout]` either, so the table goes rather than moves. RimZ's own [`.rimz/config.toml`](../../.rimz/config.toml) is a working example of project tasks; its repository sync task assumes push rights on the remote.

Because the file can name commands to run, every command-running field enters the trust hash, and a clone reads `untrusted` until `rimz trust grant` pins the current surface on this machine. An `untrusted` or `stale` workspace refuses any launch or project-only task run that would consume project config, and says `rimz trust grant`. A machine task of the same name keeps running meanwhile, `rimz loop list` and `rimz loop show` still show project tasks with their trust state, and a `stale` report prints a field-level diff of what changed since the grant, so you re-grant knowing what you are re-granting.

A trusted repo profile, team, or task overlays your machine config and wins a name collision. To keep that surface closed and machine-independent, a repo profile may inherit only another repo profile or a built-in kind, and a repo team role may bind only a repo profile. The threat model is [security and trust](./security.md), and the hash contract and launch-time enforcement are in [trust.md](../internals/harness/trust.md).

## Moving from the XDG roots

Earlier releases split RimZ across four XDG directories. RimZ reads only `~/.rimz/` now and does not move the old roots for you. `rimz doctor` lists all four old roots when present: config, state, data, and cache. Its fix says to move the contents of the first three into the home and delete the obsolete cache. `rimz start` and `rimz attach` warn and open the room when a config, state, or data root exists but `~/.rimz/config.toml` does not; they write no template and run no first-run setup. Account-following launches refuse until the move is completed, rather than silently selecting the default account; an explicit default pin can still launch. A leftover cache alone blocks neither command. Setting `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_DATA_HOME`, or `XDG_CACHE_HOME` no longer moves any RimZ file; `RIMZ_HOME` does.

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
