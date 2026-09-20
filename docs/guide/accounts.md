# Provider accounts

You may pay for more than one Claude or Codex plan: a work subscription and a personal one, or a second seat you move to when the first hits its weekly limit. Both CLIs already support this. Claude reads its credentials, settings, and transcripts from `CLAUDE_CONFIG_DIR` (default `~/.claude`), and Codex from `CODEX_HOME` (default `~/.codex`), so `CLAUDE_CONFIG_DIR=~/.claude-work claude` is a second, fully separate account: its own credentials, its own limits, its own transcripts.

That works for one terminal. It stops working once a room runs a fleet: every agent, team member, subagent, loop task, and restart needs the same variable set the same way. The sidebar has to read the right account's limits, and a session started under one account cannot be resumed under the other, because its transcript lives in the other home. RimZ makes the account a property of the room. You declare the home once, pick it when the room is born, and everything the room launches runs under it.

```sh
rimz accounts add claude work          # declare the home, install hooks, print the login command
rimz start --account claude=work       # a room whose Claude agents all run under `work`
rimz reset --account claude=default    # rebuild the room on Claude's own home
```

## Declare an account

`rimz accounts add <kind> <name>` declares a named account for `claude` or `codex`:

```console
$ rimz accounts add claude work
✓ claude  installed 13 hooks → ~/.rimz/accounts/claude/work/settings.json  (new file)
claude account `work` lives at ~/.rimz/accounts/claude/work
  log in once   CLAUDE_CONFIG_DIR=/home/me/.rimz/accounts/claude/work claude
  use it        rimz start --account claude=work
```

What it does on your machine:

1. Writes `[accounts.claude.work]` to the per-machine config, `~/.rimz/config.toml`.
2. Creates the home directory, `~/.rimz/accounts/<kind>/<name>`.
3. Installs the RimZ reporting hooks into that home's provider config, exactly as `rimz hooks install` does for the provider's own home.

`--home <path>` adopts a directory you already keep, such as `~/.claude-work`, instead of creating one under `~/.rimz/accounts/`.

RimZ writes no credentials and copies no settings. Run the printed login command once, so the provider stores its own credentials in the new home, then copy over any settings you want the two accounts to share. Your [skill library](./configuration.md#skills) needs no copying: an agent launched in a named Claude home reaches it the same way an agent in `~/.claude` does.

Rerunning `add` for an account you already declared is how you finish a setup that stopped part way. It leaves the config entry alone, creates the home if it is missing, and reinstalls the hooks.

The account named `default` is the provider's own home and is never declared. It is whatever the provider CLI resolves for itself: `~/.claude` or `~/.codex`, unless you export `CLAUDE_CONFIG_DIR` or `CODEX_HOME` in the shell you start the room from.

`rimz accounts list` shows every account and whether a room can use it:

```console
$ rimz accounts list
KIND    NAME      HOME                          STATUS
claude  default   ~/.claude                     native
claude  work      ~/.rimz/accounts/claude/work  ready
codex   default   ~/.codex                      native
codex   personal  ~/codex-personal              hooks untrusted
RimZ hooks for codex account `personal` at `/home/me/codex-personal` are untrusted (SessionStart, UserPromptSubmit, SubagentStart, SubagentStop, Stop, PermissionRequest, PreToolUse, PostToolUse, PreCompact, PostCompact); run /hooks inside codex and trust the RimZ hooks, started as `CODEX_HOME=/home/me/codex-personal codex`
```

A named account reads `ready` once its home exists and holds trusted RimZ hooks. Anything else prints its own line under the table with the fix: `home missing` and `hooks missing` both mean `rimz accounts add` never finished, so rerun it. `hooks untrusted` is Codex asking you to approve new hooks inside Codex itself, so an account you just declared stays there until you start Codex once under its home and trust them with `/hooks`. A `default` row reads `native` and is checked nowhere, because `rimz start` offers to install and refresh the hooks in the provider's own home ([install agent hooks](./setup.md#install-agent-hooks)).

## Start a room on an account

A room picks one account per provider when it is born:

```sh
rimz start --account claude=work --account codex=personal
```

Without `--account`, a new room uses the selection in the project config, `<repo>/.rimz/config.toml`, and otherwise `default`:

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

The selection is then frozen for the life of the room, and RimZ sets `CLAUDE_CONFIG_DIR` or `CODEX_HOME` on every provider process the room launches. Every launch path is covered: panes you open by hand with `rimz agents`, team members, subagents, loop tasks, scripted runs, restarts, and the [remote-control hosts](./remote.md#answer-asks-from-your-phone). `rimz agents explain @coder` prints the account that agent launches under, as `claude@work`. Every other provider has only `default` and launches as it always has.

## What the account changes

The sidebar's provider dashboard reads the room's account: the plan, the 5-hour and 7-day limit bars, and the credits all come from that home. A named account labels its block, as in `Claude · work`, so two rooms on different Claude accounts show different bars side by side. `rimz providers` prints the same picture as text, one block per account ([reading the numbers](./insight.md#per-account-the-provider-dashboard)).

A daily cap in the per-machine config's `[accounts.budget]` table applies to each account of that provider separately:

```toml
[accounts.budget]
claude = "100/day"
```

A room on `work` parks when `work` has spent $100 today, whatever `default` has spent. The caps and what lifts a park are in [budgets](./budget.md#cap-the-room-and-the-account).

`rimz stats` and the spend totals walk every declared account home, so your spend history covers all of them whichever room you are standing in.

## Change a room's account

Accounts are fixed for the life of a room, because every session it holds belongs to the account it started under. To move a room, rebuild it:

```sh
rimz reset --account claude=default
```

`rimz reset` tears the room down, forgets its account selection, and starts it again on the new one. The rebuilt room comes up empty: a reset never brings back the agents it tore down.

A session always resumes under the account it was born in, so a room on a new account leaves the old room's sessions behind. When RimZ rebuilds a room after a reboot or a crash, it skips each such session and prints `different account` beside it. Asking for one by hand, with `rimz agents resume` or `rimz agents restart`, refuses instead, and names the two ways back: start a room under that session's account, or `rimz reset --account` this one back to it. Nothing is lost either way, and the transcript stays in the old account's home.

Inside a room that is already running, `rimz start --account` accepts the account the room is already on and refuses any other, pointing at `rimz reset --account`.

## Remove an account

```sh
rimz accounts remove claude work
```

This deletes the `[accounts.claude.work]` entry and nothing else: the home, its credentials, and its transcripts stay on disk, and `rimz accounts add claude work --home <that home>` brings the account back. A room still selecting a removed account refuses to start until you add the account again or run `rimz reset`, which clears the selection (`rimz reset --account` picks another in the same step).

`rimz uninstall` keeps account homes too: it removes RimZ's hooks from every declared account's home and leaves `~/.rimz/accounts/` in place, whichever flags you pass it.

## See also

- [Accounts CLI](../reference/cli/accounts.md): every flag of `add`, `list`, and `remove`, and the JSON shape.
- [Configuration](./configuration.md#accounts): the `[accounts]` tables in the per-machine and project config.
- [Budgets](./budget.md): daily account caps and how a park lifts.
