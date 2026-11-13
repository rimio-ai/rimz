# Set up your machine

RimZ runs with no configuration. Every preference has a default, and a room asserts the Zellij or tmux options agents need on its own, so `rimz` in a project directory is already enough. Two things it will not decide alone, because both reach past its own files: writing reporting hooks into your agents' config, and acting on your behalf while you are away. A few more are taste it cannot guess: which multiplexer you live in, whose keys move you around a room, and what your terminal can draw.

`rimz setup` asks all of it once per machine:

```sh
rimz setup
```

It reports what it found, writes the per-machine config files with their shipped defaults, then asks a numbered run of questions: reporting hooks, the multiplexer, room keys, color, icons, a pet, and what may run while you are away. Every answer has a default in brackets, and Enter keeps it. Hooks install at their own yes; every other answer waits for a review screen at the end, and nothing is written until you press Enter there. The sections below follow it question by question and end with your first room. Nothing it does is one-way: hooks come out with `rimz hooks uninstall`, every other answer is one `rimz config set` away from its opposite, and each file it writes is named as it goes.

## What setup writes

Setup opens with a short report: the multiplexer it selected and its version, the agents it found on this machine with a count of the other supported ones (`rimz doctor` lists them), and the config directory, marked `new` or `present`. On a fresh machine it then writes the config files and counts the questions ahead:

```console
rimz setup · this machine

  multiplexer  tmux 3.8
  agents       claude, codex found · 16 more supported: rimz doctor
  config       ~/.rimz/ (new)
Wrote /home/me/.rimz/config.toml
Wrote /home/me/.rimz/theme.toml
Wrote /home/me/.rimz/loop.toml
Wrote /home/me/.rimz/remote.toml
Wrote /home/me/.rimz/teams/consensus.md (read-only copy of the built-in team consensus)
Wrote /home/me/.rimz/teams/consensus/reflect.md (read-only copy of the shipped `reflect` consensus piece)

7 questions. Enter keeps the answer in [brackets].
Hooks install when you say yes; your settings are written at the review.
```

Everything it writes goes under `~/.rimz/`. Three files carry the settings you will edit:

| File | Owns |
| --- | --- |
| `~/.rimz/config.toml` | room behavior, agent launch preferences and commands, worktree defaults, attention timing, resume, compaction, notifications |
| `~/.rimz/theme.toml` | sidebar appearance: scheme, color depth, glyphs, pets |
| `~/.rimz/loop.toml` | scheduled loop tasks: the recurring turns and watchdogs you configure by hand or with `rimz loop add` |

Three generated files land beside them and need nothing from you: `~/.rimz/remote.toml`, a commented template for the [SSH aliases](./remote.md) you may add later, written only when it is missing, and `~/.rimz/teams/consensus.md` with `~/.rimz/teams/consensus/reflect.md`, read-only copies of the prompt RimZ gives a [team](./teams.md) and of the reflection piece a team can add to it, so you can read them. Nothing reads those copies back, and setup rewrites each only when the running build's text differs from it. Agent, subagent, and team definitions live under `~/.rimz/` too, as Markdown trees you write yourself; their shape is in [definitions](../reference/definitions.md).

The count covers only what this run will ask. Hooks are asked, and counted, only when an agent RimZ found needs an install or a refresh; the multiplexer only when both tmux and Zellij are installed and no `--mux`, `--tmux`, or `--zellij` flag chose one. The config files exist before the first question, so quitting leaves them holding the shipped defaults.

Rerun setup any time. When `~/.rimz/config.toml` already exists, setup merges each file against the templates of the running build without asking: your settings stay, and keys added since you last ran it appear with their defaults. It prints a line only for a file it had to create or a key it dropped as invalid, then opens straight on the review screen, where Enter writes nothing and a number changes one answer. A file RimZ cannot parse is left exactly as it is, and setup stops there and names it rather than guessing.

`rimz setup --reset` starts over. It names `config.toml`, `theme.toml`, and `loop.toml`, warns that it keeps no backup, and asks `Reset them?`, which defaults to no. A no changes nothing. A yes overwrites all three with the shipped templates, your loop tasks included, and then asks every question as on a fresh machine. Without a terminal to ask from, `--reset` refuses, exits nonzero, and changes nothing.

For a provisioning script that needs files but cannot answer questions, `rimz setup --config-only` prints the report, merges or writes the config files, reporting `Merged <file> - kept N setting(s)` for each one it merged, writes the missing remote template and the built-in consensus copies, and stops. No hooks are installed, no trust is granted, and no appearance, keys, or automation setting changes. Without `--config-only` and without a terminal to read from, setup prints the report, changes nothing, and names `rimz setup --config-only` for config. When agents lack hooks, it also names `rimz hooks install` and says how many need them.

## Install agent hooks

A stock agent CLI reports to nobody. It runs in its pane, and whether it is thinking, waiting on a permission prompt, or finished twenty minutes ago is visible only by looking at the pane. That is the gap RimZ's sidebar closes, and it closes it from the agent's own event stream rather than by scraping the screen.

A reporting hook is one line in the agent's own config file that runs `rimz hooks feed` when something happens: a session starts, a tool is called, a question blocks the turn, a turn ends. Hooks report and never answer. On a blocking prompt the hook hands the agent back its neutral no-op and the question stays in the agent's own UI, where you answer it.

Setup asks for them first, with a screen naming every file it would touch, and prints one result line per agent:

```console
1 of 7 · Reporting hooks

RimZ found 2 coding agents: claude, codex.
To show them live in the sidebar, RimZ installs or refreshes reporting hooks in each agent's config.
Codex also gets a permission rules file: routine `rimz` commands run without a prompt or sandbox in every Codex session, including outside RimZ. Turn it off with `[agents] allow-routine-rimz = false`.

  claude  13 hooks → ~/.claude/settings.json  new file
          + sets your statusline to show live context
  codex   11 hooks → ~/.codex/config.toml     new file
          config → ~/.codex/rules/rimz.rules  new file

Each hook is one `rimz hooks feed` line — it reports events, never acts or answers for you.
  undo     rimz hooks uninstall
  preview  rimz hooks install --dry-run

Install or refresh reporting hooks? [Y/n]

✓ claude  installed 13 hooks → ~/.claude/settings.json
✓ codex   installed 11 hooks → ~/.codex/config.toml
          config → ~/.codex/rules/rimz.rules
! codex ignores 11 hooks until trusted
  fix: run /hooks inside codex and trust the RimZ hooks

  undo     rimz hooks uninstall
  preview  rimz hooks install --dry-run
```

Enter or `y` installs for every agent listed, right away; `n` skips all of them and setup carries on. Only agents whose binary RimZ found appear, and an agent whose hooks and managed files are already current is not listed at all. An agent whose install fails, or whose file RimZ refuses to overwrite before anything is written (an unmarked file at a path RimZ manages whole), gets a `!` line naming the error, and setup offers and installs the rest. `All set` closes the results only when nothing is left for you to do; here Codex still needs its hooks trusted, as below. On a rerun the review's first row names the agents that need an install or a refresh, or else the agents whose hooks are installed, with `trust pending` beside each one whose hooks still wait for its own trust, as Codex's do here; the row's number opens this question.

What the write does depends on the agent. Most are a merge: RimZ adds its entries to the existing JSON, TOML, or YAML and leaves every other value, your own hooks included. Amp, Copilot, Kiro, Pi, and OMP instead get one whole file that belongs to RimZ, marked `_rimz_managed` on its first line; an unmarked file already sitting at that path is yours, and install refuses rather than overwrite it. OpenCode gets two marked files in its `plugins/rimz` directory and removes the marked file from an older RimZ install. Uninstall removes those files, preserving any other files in the directory. Muse gets a separate RimZ-owned hook file plus two settings pointing to it, with your own hooks preserved. Where the row shows a statusline note, RimZ claims the agent's statusline slot, which is how the card gets live context. A statusline you had set keeps running, wrapped inside RimZ's, and uninstall puts yours back on its own.

Codex also gets a marked `rules/rimz.rules` file for [routine coordination commands](./configuration.md#routine-rimz-commands). Those rules let matching commands run without a permission prompt or command sandbox; the dry run shows them separately from the reporting hooks. An unmarked file at that path is left alone; your own rules still apply.

MiniMax Code gets a native `rimz@local` plugin under its data directory's `plugins/rimz`, without changing your other plugins or settings; uninstall removes that directory.

`rimz hooks install` and `rimz hooks uninstall` cover the rest of the lifecycle:

```sh
rimz hooks install --dry-run    # per-agent summary plus a unified diff; writes nothing
rimz hooks install claude       # one agent kind
rimz hooks uninstall            # remove every RimZ-managed hook and restore any wrapped statusline
```

With no agent named, install covers the eighteen built-in adapters whose binaries RimZ finds here, skipping any whose install preconditions fail; naming one attempts that kind and reports any refusal. Muse needs a readable version of at least 1.4.9 even for an explicit install. Rerunning is safe either way: RimZ reclaims its own entries and writes the current set again, so a hook block you disturbed by hand comes back. Which files each agent takes, and what uninstall restores, is [what install writes](../reference/cli/hooks-trust.md#what-install-writes).

Some agents gate a new or changed hook behind their own trust prompt, Codex among them. RimZ cannot grant that on your behalf, so when it sees installed-but-untrusted hooks, including outdated trust, it prints the exact fix, and the `HOOKS` row of `rimz doctor` repeats it until you trust them. Launch Codex from your own shell with `rimz agents codex` and choose **Trust all and continue** in its new pane; unattended starts refuse and ask for that one user action ([launch checks](../reference/cli/agents.md#the-account-check-at-launch)). You can also trust through `/hooks` inside Codex. The upgrade to RimZ's frozen Codex hooks needs one final **Trust all** per account; subsequent RimZ updates keep that definition unchanged.

## Choose a multiplexer

When both tmux and Zellij are installed, setup asks which one new rooms use. A room keeps the backend it was born in, so the answer decides future births and moves no running room.

```console
2 of 7 · Multiplexer

Both tmux and Zellij are installed. A room keeps the one it was born in.

  1  tmux       ← default: the shipped default
  2  zellij

Multiplexer? [1]
```

The bracket is the multiplexer you are running setup inside, else your existing `mux.default`, else tmux, and the line beside it says which of those it is. Reopened from the review, the question keeps that line beside the default and marks a different earlier answer `← your answer`, which Enter keeps. The answer is stored as `mux.default`, and only when it differs from what the config already resolves to outside a multiplexer, where an unset key means tmux. So keeping the bracket can still write: inside Zellij with no `mux.default`, Enter writes `mux.default = "zellij"`. A `--mux`, `--tmux`, or `--zellij` flag on the command line chooses for that run, and setup skips the question. The backends and how a room is born in each are in [Zellij and tmux](./multiplexer.md).

## Room keys

A room is panes in your multiplexer, so the first question about keys is whose keys move you between them. The three options print the chords your config resolves to, so a key you customized shows as customized:

```console
3 of 7 · Room keys

Which keys move you between panes and agents?

  1  Yours         your tmux config and bindings, as they are. RimZ adds:
                     Alt+p  sidebar and back     Alt+g  fullscreen this pane
  2  RimZ          Alt+j/Alt+k walk agents · Alt+0 dashboard
                   Alt+1..9 terminals · Alt+? who needs you
                   Alt+p, then: z fullscreen
                   r resize mode, kept through moves until Esc
                   Hides the tmux status bar in rooms.
  3  RimZ + tmux   the same agent keys
                   Ctrl+b becomes RimZ's prefix, with tmux-like keys after it
                   your own prefix bindings do not fire in rooms

  Rooms run on RimZ's own tmux server; sessions outside RimZ are untouched.

  The vim keymap: rimz config set keys.keymap vim

Room keys? [1]
```

- **Yours** keeps your multiplexer's own config and bindings, which a room still reads, and adds two keys that work from any pane: one jumps to the sidebar and back, one makes the pane fullscreen. It writes `room.style = "native"`.
- **RimZ** switches the room to RimZ's keymap: chords to walk agent cards, show the dashboard, and open or show numbered terminal tabs ([room keymap](./configuration.md#room-keymap)), a prefix layer, and a resize mode that stays through moves until `Esc`. It writes `room.style = "rimz"` and `keys.keymap = "rimz"`. When `[mux] status_bar` is unset, RimZ style hides the multiplexer's status bar in rooms.
- **RimZ + tmux** (or **RimZ + Zellij**) keeps the same agent keys and makes the multiplexer's usual prefix RimZ's, with tmux-like or Zellij-like keys after it. Bindings you made under your own prefix do not fire in a room. It writes `room.style = "rimz"` and `keys.keymap = "tmux"` or `"zellij"`.

The text names the multiplexer the run chose. On tmux, rooms run on [RimZ's own tmux server](./multiplexer.md#your-tmux-rooms-run-on-their-own-server), so whichever you pick, sessions outside RimZ keep their own keys. On Zellij both RimZ options replace your own bindings inside rooms, since both are RimZ style, and going back to yours takes a detach and a reattach:

```console
3 of 7 · Room keys

Which keys move you between panes and agents?

  1  Yours         your Zellij config and bindings, as they are. RimZ adds:
                     Alt+p  sidebar and back     Alt+g  fullscreen this pane
  2  RimZ          Alt+j/Alt+k walk agents · Alt+0 dashboard
                   Alt+1..9 terminals · Alt+? who needs you
                   Alt+p, then: z fullscreen
                   r resize mode, kept through moves until Esc
                   Hides the Zellij status bar in rooms.
  3  RimZ + Zellij the same agent keys
                   Ctrl+g becomes RimZ's prefix, with Zellij-like keys after it
                   your own prefix bindings do not fire in rooms

  2 and 3 replace your own Zellij bindings in rooms; going back to yours
  needs a detach and reattach.

  The vim keymap: rimz config set keys.keymap vim

Room keys? [1] 3
```

The bundled `vim` keymap has no option of its own; set it with `rimz config set keys.keymap vim`. A stored state that none of the three options describes (`vim`, the other multiplexer's keymap) is named on a `Now:` line, the bracket reads `keep`, and Enter leaves it as it is. Every action and chord is in the [keymap reference](../reference/keymaps.md), and `rimz keys` lists what a room resolves.

## Truecolor and Nerd Font icons

The sidebar reads best with 24-bit color and Nerd Font icons, and the two are independent: a terminal can have one without the other. Terminals also lie about both, and an SSH hop or a multiplexer can strip either one. So setup draws each capability on screen and asks you what you see, which is the only reliable test.

First a color sweep, one bar of 36 blocks: answer yes if it is a single smooth gradient, no if it breaks into flat bands. Then eight sidebar icons: answer yes if you see eight distinct shapes, no if you see boxes or question marks.

An answer that changes the effective default writes `theme.mode` or `theme.glyphs.set` at the review, and only that one, so either half can fall back without touching the other. An answer matching what the defaults already resolve to writes nothing.

For terminals that qualify and fonts to install, see [installation](./installation.md#truecolor-terminal-and-a-nerd-font-optional); for what each color depth and glyph set changes on screen, see [theming](./theme.md#style-preset).

## A pet in the sidebar

A pet is a small animated companion in the sidebar's provider dashboard that reacts to what your fleet is doing. Setup renders the default pet, `rocky`, at the same tier the dashboard will use, then asks whether you want one. The preview is best-effort: if it cannot draw, the question comes anyway.

Turn one on later with the same key the answer writes:

```sh
rimz config set theme.pets.enabled true
rimz list-pets                             # preview every pet you can choose
```

The [pets guide](./pets.md) covers the rest: picking a different one, installing pets from [petdex.dev](https://petdex.dev/), the pixel and cell-art render tiers, bringing your own sprite sheet, and where the privacy boundary sits.

## While you're away

The last question controls hands-off automation. An unattended agent stops overnight for reasons that need no judgment: it hits the provider's rate limit mid-turn, or the API drops its stream, or its context sits idle long enough that the provider's warm cache expires and the next message pays to rebuild it. Each stop has a known fix, and RimZ can apply it at the moment it will work. Each fix also spends something of yours: a keystroke into your pane, a credit off your account, or part of a conversation.

So setup lists these behaviors as numbered rows with their current state and what each one costs:

| Row | What it does when on |
| --- | --- |
| auto-continue | types `continue` into a parked agent's pane after a rate limit or an API error, once the clock says the retry can succeed |
| auto-redeem | spends one of the reset credits a Codex plan grants, refilling a spent usage window on the spot, when doing so unblocks at least `resume.auto_redeem_min_gain` of blocked time. Offered only when `codex` is on the machine |
| idle compaction | has an idle agent whose context has reached its token floor (`100k` by default) summarize its conversation shortly before its provider prompt cache expires. The detail behind the summary is gone |

```console
7 of 7 · While you're away

Each of these spends something of yours and is logged (rimz stats --assists).

  1  [ ] auto-continue    types "continue" once a rate limit or API error clears
  2  [ ] auto-redeem      spends a Codex reset credit when a limit blocks 12h+
  3  [x] idle compaction  summarizes an idle 100k+ conversation; detail is gone

Numbers flip a row (for example: 1 3). Enter keeps these. [keep] 1

  1  [x] auto-continue    types "continue" once a rate limit or API error clears
  2  [ ] auto-redeem      spends a Codex reset credit when a limit blocks 12h+
  3  [x] idle compaction  summarizes an idle 100k+ conversation; detail is gone

Numbers flip a row (for example: 1 3). Enter keeps these. [keep]
```

Type the numbers of the rows to flip, separated by spaces or commas, and the rows redraw; Enter keeps them as shown. On a fresh machine idle compaction is on and the other rows are off, and a row that quotes a threshold prints the value your config holds. Without Codex the list is two rows, numbered 1 and 2. At the review, turning idle compaction off writes `off` and turning it on writes `100k`; a floor you set by hand that stays on is left as it is. Whatever you pick, every action these take appends a record you can read back with `rimz stats --assists`.

Setup removes the retired idle-compaction timer key and every `idle_compact` value that is not `off` or a token floor (the old `auto`, `always`, and `on` modes and every duration, minutes such as `25m` included), reporting each removal before refreshing defaults or reading the configuration for the questions. An existing `off` is kept. This repair also runs with `--config-only`.

The rules each one follows, down to which park resumes on which clock, are [loops → keep the fleet moving](./loops.md#keep-the-fleet-moving). The keys and their tuning are [configuration → resume](./configuration.md#resume).

## Review and write

The walk ends on a review of every answer, with the stored value beside any you changed:

```console
Review · your settings are written when you press Enter

  1  hooks        claude, codex installed · codex: trust pending
  2  multiplexer  zellij   was: tmux
  3  room keys    RimZ + Zellij, prefix Ctrl+g   was: yours (Zellij keys)
  4  color        256 colors
  5  icons        plain
  6  pet          off
  7  away         auto-continue · idle compaction on   was: idle compaction on
  Open rooms switch keys at `rimz reload`.

  Also on, not asked: alerts when an agent asks or fails
                      web access on loopback
  Any setting: rimz config set <key> <value>

Enter writes 4 changes to ~/.rimz/config.toml.
A number reopens that question · a asks them all · q quits, settings unchanged.
Review? [write]

✓ wrote 4 settings
Next:  cd into a project and run `rimz`
```

Enter writes each setting whose answer differs from your config, once, and the receipt counts them; when nothing differs, Enter writes nothing and says so. A row number reopens that question and returns to the review, `a` asks them all again, and `q` or the end of input quits with every setting unwritten (hooks you already installed stay). When the answer changes room style or keymap, the review notes that a room already open picks the new keys up at `rimz reload`. On Zellij, leaving RimZ style for your own keys is the exception: `rimz reload` drops RimZ's keys from an open room, and your own Zellij keys return after you detach and reattach. `Also on, not asked` lists the settings that are on and that setup never asks about, such as alerts when an agent asks or fails and the [web view](./web.md), and the line goes away when none is on.

The same screen is where a rerun opens:

```console
rimz setup · this machine

  multiplexer  zellij 0.45.1
  agents       claude, codex found · 16 more supported: rimz doctor
  config       ~/.rimz/ (present)
This machine is set up. Enter keeps every setting; a number changes one.

Review · your settings are written when you press Enter

  1  hooks        claude, codex installed · codex: trust pending
  2  multiplexer  zellij
  3  room keys    RimZ + Zellij, prefix Ctrl+g
  4  color        256 colors
  5  icons        plain
  6  pet          off
  7  away         auto-continue · idle compaction on

  Also on, not asked: alerts when an agent asks or fails
                      web access on loopback
  Any setting: rimz config set <key> <value>

Nothing to write: Enter leaves every setting as it is.
A number reopens that question · a asks them all · q quits, settings unchanged.
Review? [write]

✓ nothing to write; your settings are unchanged
Next:  cd into a project and run `rimz`
```

## Open your first room

Setup ends by pointing you at one. Go to a project and run `rimz` with no arguments:

```sh
cd ~/code/your-project
rimz
```

That creates or reattaches the Zellij or tmux session for this project, docks the sidebar down the left, and drops you in a shell. Launch an agent the way you always do, `claude` or `codex` straight into a pane, and its card appears in the sidebar as its first hook fires. To read the zones and the cards, see [the sidebar](./sidebar.md); to drive the fleet, see [agents](./fleet.md).

RimZ keeps one room per project while it is held. If you try to start another on a different backend, or after renaming the session without updating RimZ's record, it refuses before asking about reporting hooks: `This project's room ... is already held by another running room (another multiplexer, or a renamed session).` It leaves the running room unchanged and names these two fixes:

```sh
rimz attach                  # enter the running room
rimz reset --no-start        # close it, then run rimz start again
```

If this project's session is still open in another multiplexer, or you renamed it, close it from that multiplexer (or rename it back to the recorded name), then run the command again. Both commands above can refuse while a leftover session is listed beside the held room.

The room is held while a sidebar pane or a live supervisor lives. When starting a new sidebar, `rimz start` retains its startup hold until that room's sidebar is ready, allowing up to six seconds after the final launch before warning and continuing; a failed sidebar launch continues without that wait. Once neither a sidebar nor a supervisor lives and startup has released its hold, the room is unheld and another room can start. Attaching to the old room relaunches its sidebar only if no other room holds the workspace; otherwise, close the other room first. If the sidebar is temporarily restarting, retry the attach once it returns.

`rimz doctor` re-reads the machine at any time. A normal run writes nothing to disk and starts, stops, or moves nothing in the room, so it is the check to run when something looks wrong.

Skipping `rimz setup` entirely is also fine. The first `rimz` on a machine with no `~/.rimz/config.toml` asks at most two things, then opens the room on defaults:

- **The multiplexer**, the same question as in setup, asked only when you run it from a terminal, both tmux and Zellij are installed, no `--mux`, `--tmux`, or `--zellij` flag chose one, you are not inside a multiplexer already, and no session of this project's name is live on either. Its answer picks the backend this room is born in, and it is written to `mux.default` at once when it differs from tmux, since no review follows. `q` here quits before the room opens and changes nothing.
- **Reporting hooks**, the same screen as in setup, for the agents that need them. Every later `rimz` that starts a room not already running asks again while an agent's hooks are missing or stale.

It writes `config.toml`, `theme.toml`, `loop.toml`, and `remote.toml` with their shipped defaults, but not the team consensus copies. In place of the other questions it prints one line naming the room keys, color, and glyphs your config holds, such as `Everything else is on defaults: your Zellij keys, auto color, plain glyphs.`, with `rimz setup` beside it to change any of it. Without a terminal it asks nothing and notes that it initialized the config.

To take all of it back off the machine, `rimz uninstall --all` names every root, room, hook, and binary it is about to touch and waits for a `y` before removing them; [security and trust](./security.md#what-rimz-changes-on-your-machine) lists what it leaves behind on purpose.

## Change a setting later

Every preference key ships commented in the generated templates with a note on what it does, so the file itself is the field list:

```sh
rimz config init --print                     # the three templates, every key and comment
rimz config get                              # the whole effective config as TOML
rimz config get resume.auto_continue         # one dotted key
rimz config set theme.style modern           # edit one dotted key in the file that owns it
```

A key left commented keeps following the default the running RimZ build ships, which is what you want for anything you have no opinion about; uncommenting it pins that value as this machine's override. Read the comment before uncommenting: some lines carry an illustrative value rather than the default. The single-`#` line `# idle_compact = "100k"` shows the shipped default.

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
