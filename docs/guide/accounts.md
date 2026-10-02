# Provider accounts

You may pay for more than one Claude or Codex plan: a work subscription and a personal one, or a second seat you move to when the first hits its weekly limit. Both CLIs already support this. Claude reads its credentials, settings, and transcripts from `CLAUDE_CONFIG_DIR` (default `~/.claude`), and Codex from `CODEX_HOME` (default `~/.codex`), so `CLAUDE_CONFIG_DIR=~/.claude-work claude` is a second, fully separate account: its own credentials, its own limits, its own transcripts.

That works for one terminal. A fleet needs each agent, child, and restart to keep the correct home too: a session cannot resume under another account because its transcript lives in its original home. RimZ records an account on each agent and a default for future launches in the room. Declare each home once, pick the room's default, and switch it later without moving the agents already working.

```sh
rimz accounts add claude work          # declare the home, link it, install hooks
rimz start --account claude=work       # a room whose Claude agents all run under `work`
rimz reset --account claude=default    # rebuild the room on Claude's own home
```

## Declare an account

`rimz accounts add <kind> <name>` declares a named account for `claude` or `codex`:

```console
$ rimz accounts add claude work
linked to /home/me/.claude: settings.json, settings.local.json, CLAUDE.md, skills, plugins, agents, commands, output-styles
✓ claude  installed 13 hooks → ~/.rimz/accounts/claude/work/settings.json  (new file)
claude account `work` lives at ~/.rimz/accounts/claude/work
  log in once   CLAUDE_CONFIG_DIR=/home/me/.rimz/accounts/claude/work claude
  use at birth  rimz start --account claude=work
  this room     rimz accounts use claude work
  new rooms     rimz accounts use --global claude work
```

What it does on your machine:

1. Writes `[accounts.claude.work]` to the per-machine config, `~/.rimz/config.toml`.
2. Creates the home directory, `~/.rimz/accounts/<kind>/<name>`.
3. Links the account home to the provider's own home, as `default` resolves it: every top-level entry of either home except the provider's credentials. The line above is a provider home that holds nothing yet; yours also lists what your own `~/.claude` holds, such as `projects`.
4. Installs the RimZ reporting hooks through the links into the shared config, so the provider's own home gains the hooks too.
5. Prints the account home, login command, and commands to select it.

`--home <path>` adopts a directory you already keep, such as `~/.claude-work`, instead of creating one under `~/.rimz/accounts/`.

RimZ writes no credentials. Run the printed login command once so the provider stores its own credentials in the new home. Only the credentials stay per account: Claude's `.credentials.json` and `.claude.json`, and Codex's `auth.json`, `app-server-control`, `app-server-daemon`, and `packages`. Everything else is a link, so the account writes its sessions and transcripts into the provider's own home and reads the same settings, skills, and plugins. A change through any linked account applies to all of them, including permissions, `env`, and hook commands. A shared Codex account also launches with `CODEX_SQLITE_HOME` set to the provider's own home, unless you export that variable yourself, so its databases sit beside the sessions they index. Codex trusts hooks per account path, so run `/hooks` once in each account even though the config is shared.

To keep an account's sessions and transcripts in its own home, declare it with `--history standalone`. A standalone account links only its settings: Codex shares `config.toml` and `AGENTS.md`; Claude shares `settings.json`, `settings.local.json`, `CLAUDE.md`, `skills/`, `plugins/`, `agents/`, `commands/`, and `output-styles/`. `rimz accounts add claude work --history standalone` on an account you already declared removes the other links and leaves what they pointed at in the provider's own home; `--history shared` links them again.

Rerunning `add` also migrates accounts you already declared: it leaves the config entry alone, creates the home if missing, brings the links up to date, and refreshes hooks. Every agent launch on a named account brings them up to date the same way, so an entry the provider creates later is linked by the next launch. Nothing is deleted. An entry only the account has, such as the transcripts of an account you used before it was shared, moves into the provider's own home. An entry both homes have stays where the provider's own home keeps it, and the account's copy moves aside to `<account home>/.rimz-aside/<UTC timestamp>/<name>`, with each move printed; the two are not merged, except Codex hook-trust entries. Merge what you need out of `.rimz-aside` by hand. Already-correct links stay in place and the output says `already linked to <home>`. If a Claude release saves settings by replacing a link, the next `add` or launch moves the copy aside, relinks it, and says so.

Moving a directory aside under an agent that is writing into it would split its session, so RimZ refuses that one case while any other agent runs on the account, and names the fix: end those agents, or make the account standalone. An account home on another filesystem cannot have its entries moved; they stay in the account home with a warning.

The account named `default` is the provider's own home and is never declared. It is whatever the provider CLI resolves for itself: `~/.claude` or `~/.codex`, unless you export `CLAUDE_CONFIG_DIR` or `CODEX_HOME` in the shell you start the room from. When that variable points at any declared named account's home, `add` refuses before writing anything, including on a rerun inside a room. Unset it for the command, for example `env -u CODEX_HOME rimz accounts add codex work`. Do not export one that points at a named account's home in your login shell: `default` would then launch into that account too. Select the account with `rimz accounts use` instead. `rimz accounts add` refuses to declare the provider's own home, including a `--home` symlink to it, and `rimz doctor` flags an exported-home conflict.

`rimz accounts list` answers whose quota a launch from here will burn, and whether a room can use each account:

```console
$ rimz accounts list
   KIND    NAME      STATUS           DEFAULT FOR  AGENTS  HOME
   claude  default   ready            -            -       ~/.claude
●  claude  work      ready            new rooms    -       ~/.rimz/accounts/claude/work

●  codex   default   hooks untrusted  new rooms    -       ~/.codex
   codex   personal  hooks missing    -            -       ~/codex-me
RimZ hooks for codex account `default` at `/home/me/.codex` are untrusted (SessionStart, UserPromptSubmit, SubagentStart, SubagentStop, Stop, PermissionRequest, PreToolUse, PostToolUse, PreCompact, PostCompact); run /hooks inside codex and trust the RimZ hooks
RimZ hooks are missing for codex account `personal` at `/home/me/codex-me`; run `rimz accounts add codex personal`
```

`●` marks the account a launch from where you stand uses: inside a room, the room's selection; anywhere else, the account the next `rimz start` there would pick. `DEFAULT FOR` says which selection makes an account a default (`this room`, `this project`, `new rooms`), and `AGENTS` counts the agents running on it now across every live room. The [reference](../reference/cli/accounts.md#list) spells out the order the layers win in.

An account reads `ready` once its home exists and holds trusted RimZ hooks. Anything else prints its own line under the table with the fix: for a named account, `home missing` and `hooks missing` both mean `rimz accounts add` never finished, so rerun it; for `default`, run `rimz hooks install <kind>`. `hooks untrusted` is Codex asking you to approve new hooks inside Codex itself, so an account you just declared stays there until you start Codex once under its home and trust them with `/hooks`. `rimz start` still offers to install and refresh the hooks in the provider's own home ([install agent hooks](./setup.md#install-agent-hooks)), so a `default` problem never stops a room from starting.

## Start a room on an account

A room picks one account per provider when it is born:

```sh
rimz start --account claude=work --account codex=personal
```

To use the same account in every new room without repeating the flag, run `rimz accounts use --global codex personal` once. It writes the machine's `[accounts.use]` selection while keeping the account declarations and comments. Reverse it with `rimz accounts use --global codex default`. Existing rooms, running or stopped, keep their launch defaults until you [switch them inside the room](#change-a-rooms-account).

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

A supervised launch (`rimz subagents`, `rimz agents -p`) checks only the account it launches under. A broken account of another provider never stops it; that account refuses when something launches under it.

RimZ sets `CLAUDE_CONFIG_DIR` or `CODEX_HOME` on each provider process from that agent's account. Every fresh launch takes the room's current account for its provider: human launches, loop tasks, and the children and peers an agent launches alike. A restart, resume, or fork reopens a session only on the account it started on ([switching](#change-a-rooms-account)). `rimz agents explain @coder` prints the account that agent launches under, as `claude@work`. Every other provider has only `default` and launches as it always has.

## What the account changes

The sidebar's provider dashboard shows one block per provider, for the account the room launches under: the plan, the 5-hour and 7-day limit bars, and the credits all come from that account's home. A named account labels its block, as in `Codex · team-1`. After `rimz accounts use`, the block shows the new account from the next refresh on. Agents still running on the old account keep running and keep their own cards, but the dashboard does not show that account. Right after a switch to an account no room has read yet, the provider has no block until RimZ reads that account, and an account that was never logged in shows none. `rimz providers` lists every account, one table row each ([reading the numbers](./insight.md#per-account-the-provider-dashboard)).

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
rimz accounts use claude work
```

The command checks the selected home's hooks, then changes only that provider's launch default in the room record. It leaves machine and project configuration alone and reports how many running agents keep the prior account. Reverse it with `rimz accounts use claude default`. Its output confirms the switch immediately; sidebar account information follows on the next refresh. Outside a room the command refuses and names `--global`, which sets the machine selection for new rooms instead.

Every new launch uses the room's current account for its provider, including children and peers launched by existing agents and team members. Resuming, restarting, or forking a session also runs on the room's current account, so RimZ refuses to reopen a session that started on another one: the conversation lives in that account's home. The refusal names both accounts and the fix, for example `rimz accounts use claude default` to resume the session, then switch back. Recovery after a crash skips such sessions and lists each as `different account`. Keep an account declared to resume its sessions. A running [remote-control host](./remote.md#answer-asks-from-your-phone) keeps its account until its next spawn.

Inside a room that is already running, `rimz start --account` refuses and points at `rimz accounts use`. `rimz reset --account` remains available when you want to tear down and rebuild the room instead.

## Remove an account

```sh
rimz accounts remove claude work
```

This removes the `[accounts.claude.work]` declaration and clears the machine selection if it names that account. The home, its credentials, and its transcripts stay on disk; `rimz accounts add claude work --home <that home>` brings the account back. A room still selecting a removed account needs it added again or its launch default switched to another account.

If a live room selects the account as its launch default, `remove` warns and names it. Add the account back, or switch that room's default. The warning checks room defaults, not every running agent's account; an older session still needs its own account declared, and selected in the room, to resume.

`rimz uninstall` keeps account homes too: it removes RimZ's hooks from every declared account's home and leaves `~/.rimz/accounts/` in place, whichever flags you pass it.

## See also

- [Accounts CLI](../reference/cli/accounts.md): every flag of `add`, `list`, and `remove`, and the JSON shape.
- [Configuration](./configuration.md#accounts): the `[accounts]` tables in the per-machine and project config.
- [Budgets](./budget.md): daily account caps and how a park lifts.
