# Provider accounts

You may pay for more than one Claude or Codex plan: a work subscription and a personal one, or a second seat you move to when the first hits its weekly limit. Both CLIs already support this. Claude reads its credentials, settings, and transcripts from `CLAUDE_CONFIG_DIR` (default `~/.claude`), and Codex from `CODEX_HOME` (default `~/.codex`), so `CLAUDE_CONFIG_DIR=~/.claude-work claude` is a second, fully separate account: its own credentials, its own limits, its own transcripts.

That works for one terminal. A fleet needs each agent, child, and restart to keep the correct home too: a session cannot resume under another account because its transcript lives in its original home. RimZ records an account on each agent and a default for future launches in the room. Declare each home once, pick the room's default, and switch it later without moving the agents already working.

```sh
rimz accounts add claude work          # declare the home, share settings, install hooks
rimz start --account claude=work       # a room whose Claude agents all run under `work`
rimz reset --account claude=default    # rebuild the room on Claude's own home
```

## Declare an account

`rimz accounts add <kind> <name>` declares a named account for `claude` or `codex`:

```console
$ rimz accounts add claude work
linked settings to /home/me/.claude: settings.json, settings.local.json, CLAUDE.md, skills, plugins, agents, commands, output-styles
✓ claude  installed 13 hooks → ~/.rimz/accounts/claude/work/settings.json  (new file)
claude account `work` lives at ~/.rimz/accounts/claude/work
  log in once   CLAUDE_CONFIG_DIR=/home/me/.rimz/accounts/claude/work claude
  use at birth  rimz start --account claude=work
  this room     rimz accounts use --room claude work
  new rooms     rimz accounts use claude work
```

What it does on your machine:

1. Writes `[accounts.claude.work]` to the per-machine config, `~/.rimz/config.toml`.
2. Creates the home directory, `~/.rimz/accounts/<kind>/<name>`.
3. Links settings to the provider's own home, as `default` resolves it. Codex shares `config.toml` and `AGENTS.md`; Claude shares `settings.json`, `settings.local.json`, `CLAUDE.md`, `skills/`, `plugins/`, `agents/`, `commands/`, and `output-styles/`.
4. Installs the RimZ reporting hooks through the links into the shared config, so the provider's own home gains the hooks too.
5. Prints the account home, login command, and commands to select it.

`--home <path>` adopts a directory you already keep, such as `~/.claude-work`, instead of creating one under `~/.rimz/accounts/`.

RimZ writes no credentials. Run the printed login command once so the provider stores its own credentials in the new home. Credentials, sessions, and transcripts stay per account; settings, and for Claude skills and plugins, are shared with the provider's own home, so a change in any linked account applies to all. This includes permissions, `env`, and hook commands. Codex trusts hooks per account path, so run `/hooks` once in each account even though the config is shared.

Rerunning `add` also migrates accounts you already declared: it leaves the config entry alone, creates the home if missing, links settings, and refreshes hooks. Existing files, directories, or links to another target move aside to `<name>.orig`, with each move printed; their settings are not merged, except Codex hook-trust entries. If a backup already exists, rename or remove it and rerun; no settings entries change on that refusal, though a first declaration remains in the config. Already-correct links stay in place and the output says `settings already shared`. If a Claude release saves settings by replacing a link, the next `rimz accounts add` moves the copy to `.orig`, relinks it, and says so.

The account named `default` is the provider's own home and is never declared. It is whatever the provider CLI resolves for itself: `~/.claude` or `~/.codex`, unless you export `CLAUDE_CONFIG_DIR` or `CODEX_HOME` in the shell you start the room from. When that variable points at any declared named account's home, `add` refuses before writing anything, including on a rerun inside a room. Unset it for the command, for example `env -u CODEX_HOME rimz accounts add codex work`. Do not export one that points at a named account's home in your login shell: `default` would then launch into that account too. Select the account with `rimz accounts use` instead. `rimz accounts add` refuses to declare the provider's own home, including a `--home` symlink to it, and `rimz doctor` flags an exported-home conflict.

`rimz accounts list` shows every account and whether a room can use it:

```console
$ rimz accounts list
KIND    NAME      HOME                          STATUS           NEW ROOMS
claude  default   ~/.claude                     native           yes
claude  work      ~/.rimz/accounts/claude/work  ready            -
codex   default   ~/.codex                      native           yes
codex   personal  ~/codex-me                    hooks untrusted  -
RimZ hooks for codex account `personal` at `/home/me/codex-me` are untrusted (SessionStart, UserPromptSubmit, SubagentStart, SubagentStop, Stop, PermissionRequest, PreToolUse, PostToolUse, PreCompact, PostCompact); run /hooks inside codex and trust the RimZ hooks, started as `CODEX_HOME=/home/me/codex-me codex`
```

A named account reads `ready` once its home exists and holds trusted RimZ hooks. Anything else prints its own line under the table with the fix: `home missing` and `hooks missing` both mean `rimz accounts add` never finished, so rerun it. `hooks untrusted` is Codex asking you to approve new hooks inside Codex itself, so an account you just declared stays there until you start Codex once under its home and trust them with `/hooks`. A `default` row reads `native` and is checked nowhere, because `rimz start` offers to install and refresh the hooks in the provider's own home ([install agent hooks](./setup.md#install-agent-hooks)).

## Start a room on an account

A room picks one account per provider when it is born:

```sh
rimz start --account claude=work --account codex=personal
```

To use the same account in every new room without repeating the flag, run `rimz accounts use codex personal` once. It writes the machine's `[accounts.use]` selection while keeping the account declarations and comments. Reverse it with `rimz accounts use codex default`. Running rooms keep their launch defaults until you [switch them inside the room](#change-a-rooms-account).

A room keeps its recorded launch defaults across starts. For each provider in a new room, `--account` wins over the trusted project's `[accounts]`, then the machine's `[accounts.use]`, then `default`. The project selection lives in `<repo>/.rimz/config.toml`:

```toml
[accounts]
claude = "work"
```

That selection sits in the project trust hash, so a repository you cloned cannot quietly move your agents onto another account: until you trust the project, `rimz start` refuses and prints the fix.

`rimz start` also checks every selected named account before it builds the room, and refuses rather than opening a room whose agents cannot reach their home or report from it:

```console
$ rimz start --account claude=personal
error: unknown claude account `personal`; configured: default, work; run `rimz accounts add claude personal`
$ rimz start --account claude=work
error: RimZ hooks are missing for claude account `work` at `/home/me/.rimz/accounts/claude/work`; run `rimz accounts add claude work`
```

RimZ sets `CLAUDE_CONFIG_DIR` or `CODEX_HOME` on each provider process from that agent's account. Human launches and loop tasks take the room's current default; an agent launching another agent of the same provider passes on its own account, including `default`. A launch of a different provider takes that provider's room default. Restarts, resumes, and forks keep the source agent's account. `rimz agents explain @coder` prints the account that agent launches under, as `claude@work`. Every other provider has only `default` and launches as it always has.

## What the account changes

The sidebar's provider dashboard shows a block per account in use: the plan, the 5-hour and 7-day limit bars, and the credits all come from that account's home. A named account labels its block, as in `Claude · work`. After a switch, one room can show both the new account and the old account while its agents finish. The old block drops when its last agent ends. `rimz providers` prints the same picture as text, one block per account ([reading the numbers](./insight.md#per-account-the-provider-dashboard)).

A daily cap in the per-machine config's `[accounts.budget]` table applies to each account of that provider separately:

```toml
[accounts.budget]
claude = "100/day"
```

A room on `work` parks when `work` has spent $100 today, whatever `default` has spent. The caps and what lifts a park are in [budgets](./budget.md#cap-the-room-and-the-account).

`rimz stats` and the spend totals walk every declared account home, so your spend history covers all of them whichever room you are standing in.

## Change a room's account

When you want future launches to use another account without interrupting running work, switch the default from a shell inside the room:

```sh
rimz accounts use --room claude work
```

The command checks the selected home's hooks, then changes only that provider's launch default in the room record. It leaves machine and project configuration alone and reports how many running agents keep the prior account. Reverse it with `rimz accounts use --room claude default`. Its output confirms the switch immediately; sidebar account information follows on the next refresh.

Every new launch uses the room's current account for its provider, including children and peers launched by existing agents and team members. A session always resumes under the account it started with, even after the room switches or is recovered after a crash. Keep that account declared to resume it. A running [remote-control host](./remote.md#answer-asks-from-your-phone) keeps its account until its next spawn.

Inside a room that is already running, `rimz start --account` refuses and points at `rimz accounts use --room`. `rimz reset --account` remains available when you want to tear down and rebuild the room instead.

## Remove an account

```sh
rimz accounts remove claude work
```

This removes the `[accounts.claude.work]` declaration and clears the machine selection if it names that account. The home, its credentials, and its transcripts stay on disk; `rimz accounts add claude work --home <that home>` brings the account back. A room still selecting a removed account needs it added again or its launch default switched to another account.

If a live room selects the account as its launch default, `remove` warns and names it. Add the account back, or switch that room's default. The warning checks room defaults, not every running agent's account; an older session still needs its own account declared to resume.

`rimz uninstall` keeps account homes too: it removes RimZ's hooks from every declared account's home and leaves `~/.rimz/accounts/` in place, whichever flags you pass it.

## See also

- [Accounts CLI](../reference/cli/accounts.md): every flag of `add`, `list`, and `remove`, and the JSON shape.
- [Configuration](./configuration.md#accounts): the `[accounts]` tables in the per-machine and project config.
- [Budgets](./budget.md): daily account caps and how a park lifts.
