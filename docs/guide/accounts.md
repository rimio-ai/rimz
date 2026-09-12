# Provider accounts

You may pay for more than one Claude or Codex plan: a work subscription and a personal one, or a second seat you move to when the first hits its weekly limit. Both CLIs already support this. Claude reads its credentials, settings, and transcripts from `CLAUDE_CONFIG_DIR` (default `~/.claude`), and Codex from `CODEX_HOME` (default `~/.codex`), so `CLAUDE_CONFIG_DIR=~/.claude-work claude` is a second, fully separate login.

That works for one terminal. It stops working once a room runs a fleet: every agent, team member, subagent, supervised run, and restart needs the same variable, the sidebar has to read the right account's limits, and a session started under one account cannot be resumed under the other because its transcript lives in the other home. RimZ makes the account a property of the room. You declare the home once, pick it when the room is born, and everything the room launches runs under it.

```sh
rimz accounts add claude work          # declare the home, install hooks, print the login command
rimz start --account claude=work       # a room whose Claude agents all run under `work`
rimz reset --account claude=default    # rebuild the room on your own ~/.claude
```

## Declare an account

`rimz accounts add <kind> <name>` declares a named account for `claude` or `codex`:

```console
$ rimz accounts add claude work
✓ claude  installed 13 hooks → ~/.local/share/rimz/accounts/claude/work/settings.json  (new file)
claude account `work` lives at ~/.local/share/rimz/accounts/claude/work
  log in once   CLAUDE_CONFIG_DIR=/tmp/scratchpad/acct/home/.local/share/rimz/accounts/claude/work claude
  use it        rimz start --account claude=work
```

What it does on your machine:

1. Writes `[accounts.claude.work]` to `~/.config/rimz/config.toml`. `--home <path>` records a directory you choose, such as an existing `~/.claude-work`; without it the home is `~/.local/share/rimz/accounts/<kind>/<name>`.
2. Creates the home directory.
3. Installs the RimZ reporting hooks into that home's provider config, exactly as `rimz hooks install` does for `~/.claude`.

RimZ writes no credentials and copies no settings or skills. Run the printed login command once, so the provider stores its own credentials in the new home, and copy any settings you want to share. The account named `default` is always the provider's own home; you never declare it.

`rimz accounts list` shows every account and whether a room can use it:

```console
$ rimz accounts list
KIND    NAME      HOME                                      STATUS
claude  default   ~/.claude                                 native
claude  work      ~/.local/share/rimz/accounts/claude/work  ready
codex   default   ~/.codex                                  native
codex   personal  ~/codex-personal                          hooks untrusted
```

A problem prints below the table with its fix. Codex asks you to trust new hooks inside Codex itself (`/hooks`), so a new Codex account reads `hooks untrusted` until you start it once under its home and approve them.

## Start a room on an account

A room picks one account per provider when it is born:

```sh
rimz start --account claude=work --account codex=personal
```

Without `--account`, a new room uses the project's selection from `.rimz/config.toml`, and otherwise `default`:

```toml
[accounts]
claude = "work"
```

The project selection joins the project trust hash, so a cloned repository cannot move your agents onto another account until you trust it.

`rimz start` checks every selected named account before it builds the room, and refuses with the fix instead of launching agents that cannot log in or report:

```console
$ rimz start --account claude=personal
error: unknown claude account `personal`; configured: default, work; run `rimz accounts add claude personal`
$ rimz start --account claude=work
error: RimZ hooks are missing for claude account `work` at `/tmp/scratchpad/acct/home/.local/share/rimz/accounts/claude/work`; run `rimz accounts add claude work`
```

The selection is saved in the room's `workspace.json` and does not change while the room lives. RimZ sets `CLAUDE_CONFIG_DIR` or `CODEX_HOME` on every provider process the room launches: panes you open by hand with `rimz agents`, team members, subagents, supervised runs, loop tasks, restarts, and the remote-control hosts. `rimz agents explain @coder` prints the account an agent launches under. Other providers have only `default` and launch as they always have.

## What the account changes

The sidebar's provider dashboard reads the room's account: the plan, the 5-hour and 7-day limit bars, and credits come from that home, and a named account labels its block, as in `Claude · work`. Two rooms on different Claude accounts show different bars side by side.

A daily account budget, `[accounts.budget] claude = "100/day"`, applies to each Claude account separately: a room on `work` parks when `work` has spent $100 today, whatever `default` has spent. `rimz budget --account claude` inspects and adjusts the cap of the room's Claude account ([budgets](./budget.md)).

`rimz stats` and the spend totals read transcripts from every declared account home, so your spend history covers all of them.

## Change a room's account

Accounts are fixed for the life of a room, because every session it holds belongs to the account it started under. To move a room, rebuild it:

```sh
rimz reset --account claude=default
```

`rimz reset` tears the room down, forgets its account selection, and starts it again with the new one. A session always resumes under the account it was born in: rebirth skips a session from another account with a `different account` warning, and resuming or restarting one explicitly refuses with both fixes, a room started with that session's account or a `rimz reset --account` back to it. Nothing is lost; the transcript stays in the old account's home.

`rimz start --account` inside a room that is already running refuses and points at `rimz reset --account`.

## Remove an account

```sh
rimz accounts remove claude work
```

This deletes the `[accounts.claude.work]` entry and nothing else: the home, its credentials, and its transcripts stay on disk, and `rimz accounts add claude work --home <that home>` brings the account back. A room still selecting a removed account refuses to start until `rimz reset --account` picks another.

`rimz uninstall` keeps account homes too: it removes RimZ's hooks from every declared account's home, and when it clears RimZ's data directory it leaves `accounts/` in place.

## See also

- [Accounts CLI](../reference/cli/accounts.md): every flag of `add`, `list`, and `remove`, and the JSON shape.
- [Configuration](./configuration.md#accounts): the `[accounts]` tables in machine and project config.
- [Budgets](./budget.md): daily account caps and how a park lifts.
