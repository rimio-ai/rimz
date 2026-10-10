# Provider accounts

You may pay for more than one Claude or Codex plan: a work subscription and a personal one, or a second seat you move to when the first hits its weekly limit. Both CLIs already support this. Claude reads its credentials, settings, and transcripts from `CLAUDE_CONFIG_DIR` (default `~/.claude`), and Codex from `CODEX_HOME` (default `~/.codex`), so `CLAUDE_CONFIG_DIR=~/.claude-work claude` is a second, fully separate account: its own credentials, its own limits, its own transcripts.

That works for one terminal. A fleet needs each agent, child, and restart to keep the correct home too: a session can resume only under an account that reads the same history, which a standalone account keeps in its own home. RimZ records an account on each agent. For future launches, a room either pins an account or follows the project and machine defaults live. Declare each home once, then choose whether this room should stay on one account or follow your defaults, without moving the agents already working.

```sh
rimz accounts add claude work          # declare the home, link it, install hooks
rimz start --account claude=work       # a room whose Claude agents all run under `work`
rimz reset --account claude=default    # rebuild the room on Claude's own home
```

## Redeem a reset credit by hand

Claude limit resets and Codex reset credits can refill your allowance before its free reset. To choose the moment yourself, preview the account first:

```sh
rimz accounts redeem codex --dry-run
rimz accounts redeem codex work
rimz accounts redeem claude --dry-run
rimz accounts redeem claude work
```

Without a name, RimZ uses the account a launch here would use. It shows that login, the credit count and expiry, the usage windows, and what spending one credit changes, then asks before spending. A Codex redemption restarts the window's clock: just before a free reset, you give that reset up for little extra capacity. The preview warns when it is near, but leaves the choice to you. Claude keeps its usual reset time; if its banked credits are not usable yet, the preview explains the hold and spends nothing. Claude redemption is manual only. `--yes` skips confirmation for scripts; [the reference](../reference/cli/accounts.md#redeem) lists the outcome exit codes and how to check an uncertain claim. Spending a credit cannot be undone.

## Declare an account

`rimz accounts add <kind> <name>` declares a named account for `claude` or `codex`:

```console
$ rimz accounts add claude work
linked to /home/me/.claude: settings.json, settings.local.json, CLAUDE.md, skills, plugins, agents, commands, output-styles, projects
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
3. Links the account home to the provider's own home, as `default` resolves it: every top-level entry of either home except the provider's credentials, its per-install state, and its lock and temp files. The line above is a provider home that holds nothing yet; yours also lists any other shared entries your own `~/.claude` holds.
4. Installs the RimZ reporting hooks through the links into the shared config, so the provider's own home gains the hooks too.
5. Prints the account home, login command, and commands to select it.

`--home <path>` adopts a directory you already keep, such as `~/.claude-work`, instead of creating one under `~/.rimz/accounts/`.

RimZ writes no credentials. Run the printed login command once so the provider stores its own credentials in the new home. The credentials and per-install state stay per account: Claude's `.credentials.json`, `.claude.json`, `.oauth_refresh.lock`, and `.last-update-result.json`, and Codex's `auth.json`, `app-server-control`, `app-server-daemon`, `packages`, and `models_cache.json`. So do each provider's lock and temp files, names such as `.sqlite-maintenance.lock` or `settings.json.tmp.123`; if an account still holds a link at one of those names, the next `add` or launch removes the link, leaves its target, and says so. Everything else is a link, so the account writes its sessions and transcripts into the provider's own home and reads the same settings, skills, and plugins. A change through any linked account applies to all of them, including permissions, `env`, and hook commands. A shared Codex account also launches with `CODEX_SQLITE_HOME` set to the provider's own home, unless you export that variable yourself, so its databases sit beside the sessions they index. Codex trusts hooks per account path, so run `/hooks` once in each account even though the config is shared. Claude keeps per-project trust and its MCP server list in `.claude.json`, so those stay with each account too.

To keep an account's sessions and transcripts in its own home, declare it with `--history standalone`. A standalone account links only its settings: Codex shares `config.toml` and `AGENTS.md`; Claude shares `settings.json`, `settings.local.json`, `CLAUDE.md`, `skills/`, `plugins/`, `agents/`, `commands/`, and `output-styles/`. `rimz accounts add claude work --history standalone` on an account you already declared removes the other links and leaves what they pointed at in the provider's own home; `--history shared` links them again.

Rerunning `add` also migrates accounts you already declared: it leaves the config entry alone, creates the home if missing, brings the links up to date, and refreshes hooks. Every agent launch on a named account brings them up to date the same way, so an entry the provider creates later is linked by the next launch. Nothing is deleted. An entry only the account has, such as the transcripts of an account you used before it was shared, moves into the provider's own home. An entry both homes have stays where the provider's own home keeps it, and the account's copy moves aside to `<account home>/.rimz-aside/<UTC timestamp>/<name>`, with each move printed; the two are not merged, except Codex hook-trust entries. Merge what you need out of `.rimz-aside` by hand: sessions in there cannot be resumed and are left out of `rimz stats`, the spend totals, and today's budget count until you move them. Already-correct links stay in place and the output says `already linked to <home>`. If a Claude release saves settings by replacing a link, the next `add` or launch moves the copy aside, relinks it, and says so. A session whose directory was set aside is no longer where the provider looks for it, so its first relaunch fails once at the provider; the launch warning names where the directory went, and the relaunch after that starts fresh.

Moving a directory out from under an agent that is writing into it would split its session, so RimZ refuses to set a directory aside, or to remove the link to one after a switch to standalone, while any other agent runs on the account. The refusal names the fix: end those agents, or set the account's `history` back. A Codex remote-control daemon on the account blocks the same way only while it holds a conversation; an idle daemon does not, and keeps running. Close the conversations in the remote client and rerun about a minute later, once the daemon has unloaded them (Codex before 0.154.0 keeps a closed conversation loaded for thirty minutes), or turn the daemon off with `rimz config set remote_control.codex false`, rerun, then turn it back on. A daemon that does not report its conversations blocks too, and the toggle is its fix. A daemon that was running when a switch went through may keep writing history the old mode's way until it restarts, so after changing `history` on such an account, turn remote control off and back on the same way. A switch made with `rimz accounts add --history` that is refused leaves the account's mode as it was. An account home on another filesystem cannot have its entries moved; they stay in the account home with a warning. Its history then stays local too: no other account can resume it, and it is left out of the pool's spend totals and daily budget.

The account named `default` is the provider's own home and is never declared. It is whatever the provider CLI resolves for itself: `~/.claude` or `~/.codex`, unless you export `CLAUDE_CONFIG_DIR` or `CODEX_HOME` in the shell you start the room from. When that variable points at any declared named account's home, `add` refuses before writing anything, including on a rerun inside a room. Unset it for the command, for example `env -u CODEX_HOME rimz accounts add codex work`. Do not export one that points at a named account's home in your login shell: `default` would then launch into that account too. Select the account with `rimz accounts use` instead. `rimz accounts add` refuses to declare the provider's own home, including a `--home` symlink to it, and `rimz doctor` flags an exported-home conflict.

`rimz accounts list` answers whose quota a launch from here will burn, how much of it each account has left, and whether a room can use each account:

```console
$ rimz accounts list
   KIND    NAME     STATUS           5h LEFT              7d LEFT              CREDITS  AGENTS  HISTORY  HOME
○  claude  default  ready            100% · 3h31m · 0.0x  89% · 4d03h · 0.3x   -        -       -        ~/.claude
●  claude  work     ready            98% · 4h21m · 0.2x   31% · 13h51m · 0.8x  -        -       shared   ~/.rimz/accounts/claude/work

●  codex   default  hooks untrusted  ∞                    64% · 5d11h · 1.6x   -        -       -        ~/.codex
●  this room (claude)   ●  new rooms (codex)   ○  new rooms
RimZ hooks for codex account `default` at `/home/me/.codex` are untrusted (SessionStart, UserPromptSubmit, SubagentStart, SubagentStop, Stop, PermissionRequest, PreToolUse, PostToolUse, PreCompact, PostCompact); run /hooks inside codex and trust the RimZ hooks
```

`●` marks the account a launch from where you stand uses: a room's pin, else the trusted project's selection, else the machine's, else the provider's own home. `○` marks the machine default when that is a different account. The line under the table names every deciding layer (`this room`, `this project`, `new rooms`), with provider kinds when they differ. `5h LEFT` and `7d LEFT` show how much of each usage window the account has left and how long until it resets, so you can pick the account to switch to. `AGENTS` counts the agents running on the account now across every live room. The [reference](../reference/cli/accounts.md#list) spells out the order the layers win in.

Pace is the used share divided by the elapsed share of the window: `1.0x` lasts exactly to reset; above it, the account runs dry early at that average rate. It is rounded to one decimal, with `>10x` at a rounded pace of 10.0 or higher. A trailing `–` means less than 5% of the window has elapsed, too early to tell. Pace is absent for a lifted, unknown, not-started or exhausted window, or without a duration, reset time or positive elapsed time. Its color follows `[theme.display.budget_bar.burn_rate]`, the same bands as the sidebar reset marker.

An account reads `ready` once its home exists, holds trusted RimZ hooks, and has no known login problem; if the provider cannot report its login state, `ready` does not prove it is logged in. Anything else prints its own line under the table with the fix: `logged out` means the provider has no login under that home, and the line carries the login command; for a named account, `home missing` and `hooks missing` both mean `rimz accounts add` never finished, so rerun it; for `default`, run `rimz hooks install <kind>`. `hooks untrusted` is Codex asking you to approve new hooks inside Codex itself, so an account you just declared stays there until you start Codex once under its home and trust them with `/hooks`. `rimz start` still offers to install and refresh the hooks in the provider's own home ([install agent hooks](./setup.md#install-agent-hooks)), so a `default` problem never stops a room from starting.

## Start a room on an account

To keep a room on one account per provider across restarts and changes to your defaults, pin it at start:

```sh
rimz start --account claude=work --account codex=personal
```

To set the machine default without repeating the flag, run `rimz accounts use --global codex personal` once. It checks the account's home and hooks, then writes the machine's `[accounts.use]` selection while keeping the account declarations and comments. New rooms and existing rooms following that default use it on their next launch; pinned rooms keep their account, and running agents stay where they are. Reverse it with `rimz accounts use --global codex default`.

A room keeps its pins across starts. Without a pin, each launch follows the trusted project's `[accounts]`, then the machine's `[accounts.use]`, then `default`. Without `--account`, start writes no pin. Older rooms keep recorded named accounts as pins; their recorded `default` accounts follow live defaults unless you explicitly pin `default` now. On re-entry from outside the room, `--account` can pin a provider that was following defaults, or repeat its existing pin; a different pin refuses and names `rimz accounts use` as the fix. The project selection lives in `<repo>/.rimz/config.toml`:

```toml
[accounts]
claude = "work"
```

That selection sits in the project trust hash, so a repository you cloned cannot quietly move your agents onto another account. An untrusted or stale selection refuses a launch for a provider following defaults and prints the fix; a pinned provider ignores it. An unreadable machine `config.toml` also refuses a provider following defaults or pinned to a named account. A provider pinned to `default` needs neither the project selection nor the machine file. These refusals belong to that provider only; a healthy provider can still launch. Start checks all providers before opening the room.

`rimz start` also checks every selected named account before it builds the room, and refuses rather than opening a room whose agents cannot reach their home or report from it:

```console
$ rimz start --account claude=personal
error: unknown claude account `personal`; configured: default, work; run `rimz accounts add claude personal`
$ rimz start --account claude=work
error: RimZ hooks are missing for claude account `work` at `/home/me/.rimz/accounts/claude/work`; run `rimz accounts add claude work`
```

Every launch checks the account it launches under, `default` included: a fresh agent or team, a restart, a resume, a fork, and a supervised launch (`rimz subagents`, `rimz agents -p`) refuse, naming the fix, when that account's home or hooks are not usable. A broken account of another provider never stops a launch; that account refuses when something launches under it. `rimz start` alone stays lenient on `default`, so a room still opens and the refusal waits for the first launch under it.

RimZ sets `CLAUDE_CONFIG_DIR` or `CODEX_HOME` on each provider process from that agent's account. A fresh launch takes the room's current account for its provider: human launches, loop tasks, and the peers an agent launches alike. There are two exceptions. A `rimz subagents` child runs on its parent's account when both are the same provider, and on the room's current account for its own provider otherwise. A loop task added with `--account` runs every fire on that account ([loops](./loops.md#budgets-and-strikes)). A restart, resume, or fork reopens a session on the room's current account, as long as that account shares the session's history ([switching](#change-a-rooms-account)). `rimz agents explain @coder` prints the account that agent launches under, as `claude@work`. Every other provider has only `default` and launches as it always has.

## What the account changes

The sidebar's provider dashboard shows one block per provider, for the account the room launches under: the plan, the 5-hour and 7-day limit bars, and the credits all come from that account's home. A named account labels its block, as in `Codex · team-1`. After pinning, unpinning, or changing a default the room follows, the block shows the new account from the next refresh on. Agents still running on the old account keep running and keep their own cards, but the dashboard does not show that account. Right after a switch to an account no room has read yet, the provider has no block until RimZ reads that account, and an account that was never logged in shows none. `rimz providers` lists every account, one table row each ([reading the numbers](./insight.md#per-account-the-provider-dashboard)).

An account can stay logged in while its subscription no longer allows access. The [dashboard](./sidebar.md#the-provider-dashboard) and `rimz providers` show `lapsed`; `rimz accounts list` shows the same status with `plan lapsed: no Claude access` for Claude. The problem names the account's product, so a delegated Pi login reads `no Pi access`. This is separate from a logged-out account or missing hooks, and does not change launch setup checks.

A daily cap in the per-machine config's `[accounts.budget]` table follows the history: `default` and the shared accounts of a provider spend against one cap, and each standalone account has its own:

```toml
[accounts.budget]
claude = "100/day"
```

With `work` shared, a room on `work` parks when `default` and the shared accounts have spent $100 between them today. Declare `work` with `history = "standalone"` and it parks on its own $100, whatever `default` has spent. A raise or clear made while `work` was standalone stays with that account and does not follow it into the shared cap: once `work` is shared again, the next `rimz gc` drops it. The caps and what lifts a park are in [budgets](./budget.md#cap-the-room-and-the-account).

`rimz stats` and the spend totals read the provider's own home and every standalone account home, so your spend history covers all of them whichever room you are standing in. `default` and the shared accounts of a provider are one history, so they are one spend figure: a shared account's dashboard block shows that shared spend beside the account's own plan, limits, and credits.

## Change a room's account

When you want future launches to stay on one account without interrupting running work, pin it from a shell inside the room:

```sh
rimz accounts use claude work
```

The command checks the selected home's hooks, then pins only that provider's account in the room record, even when the room already follows that account. It leaves machine and project configuration alone and reports how many running agents keep the prior account. Reverse it with `rimz accounts use --reset claude` to remove the pin and follow the project and machine defaults live again. Reset checks the inherited account before removing the pin, so a refusal leaves it intact. `rimz accounts use claude default` instead pins the provider's own home; `default` is an account, not a reset. Its output confirms the switch immediately; sidebar account information follows on the next refresh. Outside a room the command refuses and names `--global`, which changes the machine default instead.

Every new launch uses the room's current account for its provider, including peers launched by existing agents and team members. A `rimz subagents` child of the same provider as its parent stays on its parent's account, so the work an agent delegates does not move when the room switches. A loop task pinned with `--account` stays on its account too, and skips its fire when that account cannot run. Resuming, restarting, or forking a session also runs on the room's current account. `default` and every shared account of a provider read one history, so a session moves between them: switch the room to another shared account, resume, and the conversation continues there, with the agent recorded on the account it now runs on. A standalone account keeps its history in its own home, so RimZ refuses to reopen a session across that boundary, in either direction. The refusal names both accounts and the fix, for example `rimz accounts use claude default` to resume the session, then switch back. Recovery after a crash skips such sessions and lists each as `different account`. Keep an account declared to resume its sessions. A running [remote-control host](./remote.md#answer-asks-from-your-phone) keeps its account until its next spawn.

Inside a room that is already running, `rimz start --account` refuses and points at `rimz accounts use`. `rimz reset --account` remains available when you want to tear down and rebuild the room instead.

A changed project or machine default can move an unpinned room outside an old session's history pool. That does not block the default change; the later resume refuses. Pin the room to the session's account with `rimz accounts use`, resume, then use `rimz accounts use --reset` to follow live defaults again.

## Remove an account

```sh
rimz accounts remove claude work
```

This removes the `[accounts.claude.work]` declaration and clears the machine selection if it names that account. The home, its credentials, and its transcripts stay on disk; `rimz accounts add claude work --home <that home>` brings the account back. A room pinned to a removed account needs it added again, unpinned with `rimz accounts use --reset claude`, or pinned to another account. Rooms following the machine default move with the cleared entry; a project that still names the removed account needs its selection changed too.

If a live room pins the account, `remove` warns and names it. Add the account back, unpin it, or pin another account in that room. The warning checks room pins, not every running agent's account; an older session still needs its account declared and a room account sharing its history to resume.

`rimz uninstall` keeps account homes too: it removes RimZ's hooks from every declared account's home and leaves `~/.rimz/accounts/` in place, whichever flags you pass it.

## See also

- [Accounts CLI](../reference/cli/accounts.md): every flag of `add`, `list`, and `remove`, and the JSON shape.
- [Configuration](./configuration.md#accounts): the `[accounts]` tables in the per-machine and project config.
- [Budgets](./budget.md): daily account caps and how a park lifts.
