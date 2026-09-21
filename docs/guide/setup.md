# Set up your machine

RimZ runs with no configuration. Every preference has a default, and a room asserts the Zellij or tmux options agents need on its own, so `rimz` in a project directory is already enough. Two things it will not decide alone, because both reach past its own files: writing reporting hooks into your agents' config, and acting on your behalf while you are away.

`rimz setup` is that conversation, once per machine:

```sh
rimz setup
```

It reads the machine, writes the per-machine config, then asks about hooks, color, glyphs, a pet, and hands-off automation, in that order. The sections below follow it question by question and end with your first room. Nothing it does is one-way: hooks come out with `rimz hooks uninstall`, every other answer is one `rimz config set` away from its opposite, and each file it writes is named as it goes.

## What setup writes

Setup opens with a report of what it found: the multiplexer it selected and its version, the project root you ran it in and whether RimZ [trusts](./security.md#project-trust) it, the path to the core config file and whether that file already exists, then one row per agent adapter giving where RimZ found the binary and whether its hooks are installed.

Everything it writes goes under `~/.rimz/`, and it writes only what is missing. Three files carry the settings you will edit:

| File | Owns |
| --- | --- |
| `~/.rimz/config.toml` | room behavior, agent launch preferences and commands, worktree defaults, attention timing, resume, compaction, notifications |
| `~/.rimz/theme.toml` | sidebar appearance: scheme, color depth, glyphs, pets |
| `~/.rimz/loop.toml` | scheduled loop tasks: the recurring turns and watchdogs you configure by hand or with `rimz loop add` |

Two generated files land beside them and need nothing from you: `~/.rimz/remote.toml`, a commented template for the [SSH aliases](./remote.md) you may add later, and `~/.rimz/teams/consensus.md`, a read-only copy of the prompt RimZ gives a [team](./teams.md) so you can read it. Nothing reads that copy back, and each setup run rewrites it from the running build. Agent, subagent, and team definitions live under `~/.rimz/` too, as Markdown trees you write yourself; their shape is in [definitions](../reference/definitions.md).

Rerun setup any time. When a config file already exists it asks `Keep your current config?`, which defaults to yes: your settings stay and the file is merged against the templates of the running build, so keys added since you last ran it appear with their defaults. Answering no overwrites the files with fresh templates. A file RimZ cannot parse is left exactly as it is, and setup stops there and names it rather than guessing.

`rimz setup --yes` takes the non-interactive path: it prints the report, merges or writes the config files, and stops. No hooks are installed, no trust is granted, and no appearance or automation setting changes, which is what you want from a provisioning script. Without `--yes` and without a terminal to read from, setup prints the report, changes nothing, and tells you which of the two to do.

## Install agent hooks

A stock agent CLI reports to nobody. It runs in its pane, and whether it is thinking, waiting on a permission prompt, or finished twenty minutes ago is visible only by looking at the pane. That is the gap RimZ's sidebar closes, and it closes it from the agent's own event stream rather than by scraping the screen.

A reporting hook is one line in the agent's own config file that runs `rimz hooks feed` when something happens: a session starts, a tool is called, a question blocks the turn, a turn ends. Hooks report and never answer. On a blocking prompt the hook hands the agent back its neutral no-op and the question stays in the agent's own UI, where you answer it.

Setup asks for them first, with a screen naming every file it would touch:

```console
rimz · first-run setup
────────────────────────────────────────────────

RimZ found 2 coding agents: claude, codex.
To show them live in the sidebar, RimZ installs or refreshes reporting hooks in each agent's config.

  claude  13 hooks → ~/.claude/settings.json  updates existing config
          + sets your statusline to show live context
  codex   11 hooks → ~/.codex/config.toml     updates existing config

Each hook is one `rimz hooks feed` line — it reports events, never acts or answers for you.
  undo     rimz hooks uninstall
  preview  rimz hooks install --dry-run

Install or refresh reporting hooks? [Y/n]
```

Enter or `y` installs for every agent listed; `n` skips all of them and setup carries on. Only agents whose binary RimZ found appear, and an agent whose hooks are already current is not listed at all.

What the write does depends on the agent. Most are a merge: RimZ adds its entries to the existing JSON or TOML and leaves every other value, your own hooks included. Amp, Copilot, Kiro, OpenCode, and Pi instead get one whole file that belongs to RimZ, marked `_rimz_managed` on its first line; an unmarked file already sitting at that path is yours, and install refuses rather than overwrite it. Where the row shows a statusline note, RimZ claims the agent's statusline slot, which is how the card gets live context. A statusline you had set keeps running, wrapped inside RimZ's, and uninstall puts yours back on its own.

`rimz hooks install` and `rimz hooks uninstall` cover the rest of the lifecycle:

```sh
rimz hooks install --dry-run    # per-agent summary plus a unified diff; writes nothing
rimz hooks install claude       # one agent kind
rimz hooks uninstall            # remove every RimZ-managed hook and restore any wrapped statusline
```

With no agent named, install covers every one of the thirteen built-in adapters whose binary RimZ finds here; naming one installs that kind whether or not its binary is present. Rerunning is safe either way: RimZ reclaims its own entries and writes the current set again, so a hook block you disturbed by hand comes back. Which files each agent takes, and what uninstall restores, is [what install writes](../reference/cli/hooks-trust.md#what-install-writes).

Some agents gate a newly installed hook behind their own trust prompt, Codex among them. RimZ cannot grant that on your behalf, so when it sees installed-but-untrusted hooks it prints the exact fix, and the `HOOKS` row of `rimz doctor` repeats it until you run `/hooks` inside Codex and trust them.

## Truecolor and Nerd Font icons

The sidebar reads best with 24-bit color and Nerd Font icons, and the two are independent: a terminal can have one without the other. Terminals also lie about both, and an SSH hop or a multiplexer can strip either one. So setup draws each capability on screen and asks you what you see, which is the only reliable test.

First a color sweep, one bar of 36 blocks: answer yes if it is a single smooth gradient, no if it breaks into flat bands. Then eight sidebar icons: answer yes if you see eight distinct shapes, no if you see boxes or question marks.

An answer that changes the effective default writes `theme.mode` or `theme.glyphs.set`, and only that one, so either half can fall back without touching the other. An answer matching what the defaults already resolve to writes nothing.

For terminals that qualify and fonts to install, see [installation](./installation.md#truecolor-terminal-and-a-nerd-font-optional); for what each color depth and glyph set changes on screen, see [theming](./theme.md#style-preset).

## A pet in the sidebar

A pet is a small animated companion in the sidebar's provider dashboard that reacts to what your fleet is doing. Setup renders the default pet, `rocky`, at the same tier the dashboard will use, then asks whether you want one. The preview is best-effort: if it cannot draw, the question comes anyway.

Turn one on later with the same key the answer writes:

```sh
rimz config set theme.pets.enabled true
rimz list-pets                             # preview every pet you can choose
```

The [pets guide](./pets.md) covers the rest: picking a different one, installing pets from [petdex.dev](https://petdex.dev/), the pixel and cell-art render tiers, bringing your own sprite sheet, and where the privacy boundary sits.

## Hands-off automation

The last question is the one that matters most, because it is the only one that lets RimZ act without you. An unattended agent stops overnight for reasons that need no judgment: it hits the provider's rate limit mid-turn, or the API drops its stream, or its context sits idle long enough that the provider's warm cache expires and the next message pays to rebuild it. Each stop has a known fix, and RimZ can apply it at the moment it will work. Each fix also spends something of yours: a keystroke into your pane, a credit off your account, or part of a conversation.

So setup lists three behaviors as rows with their current state and what each one costs, and asks once:

| Row | What it does when on |
| --- | --- |
| auto-continue | types `continue` into a parked agent's pane after a rate limit or an API error, once the clock says the retry can succeed |
| auto-redeem | spends one of the reset credits a Codex plan grants, refilling a spent usage window on the spot, when doing so unblocks hours of work. Offered only when `codex` is on the machine |
| idle compaction | has a long-idle agent summarize its conversation and carry on from the summary, while other agents in its channel are still running. The detail behind the summary is gone |

`y` turns every listed row on, `n` turns every row off, and `choose` walks them one at a time. Enter keeps each row as it is, so on a fresh machine all three stay off. Whatever you pick, every action these take appends a record you can read back with `rimz stats --assists`.

The rules each one follows, down to which park resumes on which clock, are [loops → keep the fleet moving](./loops.md#keep-the-fleet-moving). The keys and their tuning are [configuration → resume](./configuration.md#resume).

## Open your first room

Setup ends by telling you to start one. Go to a project and run `rimz` with no arguments:

```sh
cd ~/code/your-project
rimz
```

That creates or reattaches the Zellij or tmux session for this project, docks the sidebar down the left, and drops you in a shell. Launch an agent the way you always do, `claude` or `codex` straight into a pane, and its card appears in the sidebar as its first hook fires. To read the zones and the cards, see [the sidebar](./sidebar.md); to drive the fleet, see [agents](./fleet.md).

`rimz doctor` re-reads the machine at any time. A normal run writes nothing to disk and starts, stops, or moves nothing in the room, so it is the check to run when something looks wrong. Skipping `rimz setup` entirely is also fine: the first `rimz` in a project asks the same hook, color, glyph, pet, and automation questions when it finds no config, and writes the same files.

To take all of it back off the machine, `rimz uninstall --all` names every root, room, hook, and binary it is about to touch and waits for a `y` before removing them; [security and trust](./security.md#what-rimz-changes-on-your-machine) lists what it leaves behind on purpose.

## Change a setting later

Every preference key ships commented in the generated templates with a note on what it does, so the file itself is the field list:

```sh
rimz config init --print                     # the three templates, every key and comment
rimz config get                              # the whole effective config as TOML
rimz config get resume.auto_continue         # one dotted key
rimz config set theme.style modern           # edit one dotted key in the file that owns it
```

A key left commented keeps following the default the running RimZ build ships, which is what you want for anything you have no opinion about; uncommenting it pins that value as this machine's override. Read the comment before uncommenting: some lines carry an illustrative value rather than the default, `idle_compact = "auto"` among them, where the default is `off`.

`rimz config set` routes a dotted key to whichever of the three files owns it, validates the value, and writes durably in place, keeping your comments and key order. The whole model, tier by tier and section by section, is the [configuration guide](./configuration.md).

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

Completion covers the static command and flag surface, and adds live room data as you type: `@handles` and pane targets, queued `msg_` ids, loop task names, agent and subagent profiles, team names, worktrees, channels, sessions, remote aliases, and config keys. Source the registration at shell startup rather than caching its output to a file, so it keeps up when RimZ upgrades.

## See also

- [The sidebar](./sidebar.md): what the cards, zones, and process rows you just switched on actually show.
- [Agents](./fleet.md): run the stock CLIs in the room, shape one with a profile, and drive the running fleet.
- [Zellij and tmux](./multiplexer.md): what the room asserts in your multiplexer, and a baseline worth adopting for the sessions you run outside it.
- [Configuration](./configuration.md): every key, the file that owns it, and how the tiers combine.
- [Loops](./loops.md): the hands-off reflexes in full, plus putting agent turns on a clock.
- [Security and trust](./security.md): everything RimZ changes on your machine, and the command that undoes each one.
- [Hooks and trust CLI](../reference/cli/hooks-trust.md): `rimz hooks install` and `uninstall` flag by flag, with the per-agent file table.
- [Troubleshooting](./troubleshooting.md): reading `rimz doctor`, and the fixes when an agent does not report.
