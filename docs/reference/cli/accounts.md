# Accounts CLI

`rimz accounts` declares, selects, lists, and removes the named provider accounts a room can launch into, and redeems Claude or Codex reset credits by hand. A named account is a separate provider home: RimZ launches Claude with `CLAUDE_CONFIG_DIR`, and Codex with `CODEX_HOME`, set to that home. Credentials stay apart; `add` and every launch link the rest of a `shared` account, or the settings of a `standalone` one, to the provider's own home. The `default` account is the provider's own home, resolved the way the provider CLI resolves it, and is never declared.

The commands work inside or outside a room, except bare `use` and `use --reset`, which require running inside one. Bare `use` pins only the current room's account for that provider, leaving running agents and machine and project configuration unchanged. `--reset` removes that pin. An unpinned provider follows the trusted project's selection, then the machine's, then `default`, live on each launch ([Accounts at start](./getting-started.md#accounts)). `add`, `use --global`, and `remove` write the machine `config.toml`; `add` also writes into the account home. A machine selection reaches new rooms and existing rooms following that default, not pinned providers. Inside its own multiplexer session, `start --account` refuses and names `accounts use` instead. The workflow is in the [accounts guide](../../guide/accounts.md).

```sh
rimz accounts add claude work                        # declare, link the home, install hooks
rimz accounts add codex personal --home ~/codex-me   # place the home yourself
rimz accounts use codex personal                     # this room's account for future launches
rimz accounts use --reset codex                      # unpin; follow project and machine defaults live
rimz accounts use --global codex personal            # machine default for new and following rooms
rimz accounts list                                   # every account, its usage left, and whether a room can use it
rimz accounts list --json
rimz accounts redeem codex personal --dry-run        # preview without spending
rimz accounts remove claude work                     # forget the entry; the home stays on disk
```

## Kinds, names, and homes

Named accounts exist for `claude` and `codex` only. Any other kind is refused with `<kind> has no named accounts; accounts are supported for claude, codex`.

An account name is 1 to 32 characters of `a-z`, `0-9`, `_`, and `-`, starting with a letter or digit. `default` is reserved for the provider's own home.

Every named account has a home directory, and RimZ checks each home when it reads the machine config:

| Rule | Detail |
| --- | --- |
| Default location | Without `home`, the account lives at `~/.rimz/accounts/<KIND>/<NAME>` (under `$RIMZ_HOME` when it is set). |
| Absolute path | `home` in `config.toml` must be absolute or start with `~`. `add --home` resolves a relative path against the current directory before writing it. |
| Not the provider's own home | `~/.claude` for Claude, `~/.codex` for Codex. That directory is already the `default` account. |
| One account per home | Two accounts of the same kind cannot share a home. |
| No `,` and no trailing `projects` | Provider home lists split on commas, and Claude reads a directory named `projects` as its transcript folder. |

A `config.toml` entry that breaks a rule, or that declares `default`, makes every `rimz accounts` command exit 1 with `invalid per-machine account at <path>: ...` and the fix.

## `add`

```sh
rimz accounts add <KIND> <NAME> [--home <PATH>] [--history shared|standalone]
```

| Argument or flag | Effect |
| --- | --- |
| `KIND` | `claude` or `codex`. |
| `NAME` | The account name a room selects with `--account <KIND>=<NAME>`. |
| `--home <PATH>` | The account's provider home, such as an existing `~/.claude-work`. Without it, the home is the default location above. |
| `--history <MODE>` | `shared` or `standalone`: the account's `history` field, which names its history pool. `shared`, also what an absent field means, puts the account in one pool with `default` and every other shared account of its kind; `standalone` makes it a pool of its own. On a new account the flag writes the field, and without the flag no field is written. On an existing account the flag rewrites only that field, and without the flag the field is left alone. Any other value is refused, by the flag and by the config load, naming both. |

`add` runs these steps in order:

1. For a new account, writes `[accounts.<KIND>.<NAME>]` to the machine `config.toml`, with `home` only when `--home` is given and `history` only when `--history` is given.
2. Creates the home directory.
3. Links the account home to the provider's own home as `default` resolves it, honoring the ambient `CLAUDE_CONFIG_DIR` or `CODEX_HOME`, under one lock per provider in `~/.rimz/accounts/.locks/`. Reports linked entries, entries moved into the provider's own home, links removed, links removed from unshared names, and any moves to `.rimz-aside`. With `--history` on an existing account, the links are brought to the requested mode first and the `history` field is rewritten once that succeeds, so a refused switch leaves `config.toml` unchanged.
4. Installs RimZ hooks through the named home's config link (`settings.json` for Claude, `config.toml` for Codex), updating the shared file in the provider's own home too.
5. Prints where the account lives, the command that logs the provider in once under that home, the `rimz start` flag that uses it, and the `rimz accounts use` commands for a running room and new rooms.

A `standalone` account links the settings entries; a `shared` account links those and every other top-level entry of either home that is not unshared. A name is unshared when it is private or transient. It is private when it equals a listed name or extends it with a `.` suffix (`auth.json.bak`); `.rimz-aside` is private for both providers. It is transient, for every provider, when `.lock` or `.tmp` appears past the start of the name and either ends it or is followed by `.`: `.sqlite-maintenance.lock`, `.oauth_refresh.lock.owner`, and `session_index.jsonl.tmp` are transient, while `.tmp`, `tmp`, `thread-writer-locks`, and `.lockfile` are not and stay shared. A `shared` account always links the provider's history directories, Claude's `projects/` and Codex's `sessions/` and `archived_sessions/`, creating them in the provider's own home when neither home holds one yet.

| Provider | Settings files | Settings directories | Private names |
| --- | --- | --- | --- |
| Codex | `config.toml`, `AGENTS.md` | none | `auth.json`, `app-server-control`, `app-server-daemon`, `packages` |
| Claude | `settings.json`, `settings.local.json`, `CLAUDE.md` | `skills/`, `plugins/`, `agents/`, `commands/`, `output-styles/` | `.credentials.json`, `.claude.json`, `.oauth_refresh.lock`, `.last-update-result.json` |

Each entry takes one of four paths, and nothing but a link is ever removed:

| Account home | Provider's own home | Result |
| --- | --- | --- |
| Link to the intended target | any | Left alone. |
| Absent | any | Linked. Missing settings and history directory targets are created; other missing targets remain dangling links until written. |
| File or directory | Absent | Moved into the provider's own home, then linked. Across filesystems it stays in the account home and `add` warns. |
| File, directory, or link elsewhere | Present, or the entry is a link | Moved to `<account home>/.rimz-aside/<UTC timestamp>/<name>`, then linked, and reported as `<account> account: moved <slot> aside to <aside>; the account now reads the copy in <default-home>`. A relative link is rewritten to the absolute path it pointed at, so it still resolves from there. |

A link into the provider's own home that the account's mode no longer asks for is removed and reported as `unlinked from <default-home>`; its target stays. A link into the provider's own home at an unshared name is removed in either mode, whatever its target and without the live-agent check, since the provider must own that name in each home; its target stays, and the removal is reported as `<account> account: removed the link <slot> into <default-home>; that name is per-account and no longer shared`. A real file or directory at an unshared name, and a link that points anywhere else, are left alone. When a later entry fails after others were set aside, the error ends with the same `moved <slot> aside to <aside>` lines, since a rerun finds those links in place and has nothing left to report. Codex carries missing `[hooks.state]` keys into the shared config before moving its old `config.toml` aside; existing target keys win. Nothing else is merged.

A shared Codex account launches with `CODEX_SQLITE_HOME` set to the provider's own home, so its databases follow its sessions; a `CODEX_SQLITE_HOME` you export wins.

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

RimZ writes no credentials and never writes inside an unshared name. Unshared names remain per account; every other listed entry is shared by a link, not copied. Run the printed login command once so the provider stores its own credentials there. For Codex the login line reads `CODEX_HOME=<home> codex`; trust hooks with `/hooks` once per account path (see `hooks untrusted` under [`list`](#list)).

Rerunning `add` leaves the config entry alone unless `--history` rewrites that field, creates the home if missing, brings the links to the account's mode, and refreshes hooks. Correct links remain unchanged and print `already linked to <default-home>`. This also repairs a link replaced by a provider save, moving the replacement to `.rimz-aside`. `--home` on a rerun must name the same directory. An agent launch on a named account runs the same link step before it starts the provider, prints the removed-link, moved-aside, and left-in-place lines as warnings, and refuses on the same error.

`add` refuses these cases. The live-agent checks count agents with a pane attached unless their recorded process has died, so the unstarted seats of one team launch never block each other, and run before any entry changes, but after first-declaration config writing and home creation; that declaration remains for a rerun. The other checks, including resolved-home and exported-home checks, precede those writes.

| Case | Error |
| --- | --- |
| A directory, or a link to one elsewhere, would move aside while another agent runs on the account, or the live rooms cannot be read | `` cannot share `<entry>` of <account> with <default-home>: <N> live agent(s) on the account write to the copy in its home, which would move aside under them; end those agents, or set `history = "standalone"` under `[accounts.<kind>.<name>]` `` |
| A link to a directory would be removed, after a switch to `standalone`, while another agent runs on the account, or the live rooms cannot be read | `` cannot unlink `<entry>` of <account> from <default-home>: <N> live agent(s) on the account write through that link, which would be removed under them; end those agents, or remove `history = "standalone"` from `[accounts.<kind>.<name>]` `` |
| A directory, or a link to one elsewhere, would move aside while the account's Codex remote-control daemon holds live sessions | `` cannot share `<entry>` of <account> with <default-home>: the remote-control daemon on the account holds <N> live session(s) that write to the copy in its home, which would move aside under them; close them in the remote client and rerun once the daemon has unloaded them, or stop the daemon with `rimz config set remote_control.<kind> false`, rerun, then set it back to `true`, or set `history = "standalone"` under `[accounts.<kind>.<name>]` `` |
| A directory, or a link to one elsewhere, would move aside while the account's Codex remote-control daemon runs and does not report its sessions | `` cannot share `<entry>` of <account> with <default-home>: the remote-control daemon on the account did not report its sessions, so it may write to the copy in its home, which would move aside under it; stop it with `rimz config set remote_control.<kind> false`, rerun, then set it back to `true`, or set `history = "standalone"` under `[accounts.<kind>.<name>]` `` |
| A link to a directory would be removed, after a switch to `standalone`, while the account's Codex remote-control daemon holds live sessions | `` cannot unlink `<entry>` of <account> from <default-home>: the remote-control daemon on the account holds <N> live session(s) that write through that link, which would be removed under them; close them in the remote client and rerun once the daemon has unloaded them, or stop the daemon with `rimz config set remote_control.<kind> false`, rerun, then set it back to `true`, or remove `history = "standalone"` from `[accounts.<kind>.<name>]` `` |
| A link to a directory would be removed, after a switch to `standalone`, while the account's Codex remote-control daemon runs and does not report its sessions | `` cannot unlink `<entry>` of <account> from <default-home>: the remote-control daemon on the account did not report its sessions, so it may write through that link, which would be removed under it; stop it with `rimz config set remote_control.<kind> false`, rerun, then set it back to `true`, or remove `history = "standalone"` from `[accounts.<kind>.<name>]` `` |
| The home resolves through links to the provider's own home | `` `--home` names the provider's own home, which is the `default` account (<home>); choose another home `` |
| `NAME` is `default` | `` `default` is <kind>'s own home and needs no declaring; choose another name `` |
| `NAME` breaks the name rules | `` invalid account name `<name>`; expected 1-32 characters of ... `` |
| `--home` differs from the existing account's home | `` <kind> account `<name>` already lives at `<home>`; rerun without --home, or remove the account first `` |
| The home breaks a [home rule](#kinds-names-and-homes) | `` `accounts.<kind>.<name>.home` is `<home>`, ... `` with the rule it breaks |
| The shell exports `CLAUDE_CONFIG_DIR` or `CODEX_HOME` pointing at any declared named account of that kind (also checked on reruns) | `` `CODEX_HOME` is exported as `<home>`, the home of codex account `<exported-name>`; unset `CODEX_HOME` so `default` resolves to codex's own home, e.g. `env -u CODEX_HOME rimz accounts add codex <name>` `` |

The daemon check runs once no agent is counted, and refuses while the daemon runs under the account's home, whether or not it has a session open.

## `use`

```sh
rimz accounts use [--global] <KIND> <NAME>
rimz accounts use --reset <KIND>
rimz accounts use codex personal
rimz accounts use --global codex personal
rimz accounts use --global codex default
```

Bare `use` pins the current room's account for that provider, even if it already inherits the same name. `use <KIND> default` pins the provider's own home; it does not unpin. A named account must pass the home and hook checks before the room record changes. An unknown account names `rimz accounts add`; a named account's missing home or missing or untrusted hooks refuses with its fix rather than merely warning. `default` keeps the provider's own hook setup flow at start; every actual agent launch checks its health. The selected kind does not depend on project configuration. Output names the prior deciding layer when pinning an inherited account, and counts running root agents on the prior account; those agents keep it until they end. An invalid prior inherited selection has no prior name or layer to print. Restart, resume, and fork refuse only when the session and the current account do not share a history pool, naming the pin command that reopens it; crash recovery skips it as `different account`. A repeated pin reports that the room already launches on that account. Outside a room it refuses rather than touching the machine configuration: `` `rimz accounts use` changes the running room it is run inside; run it inside one, or pass --global to set the machine default for new rooms and rooms following it ``.

`use --reset <KIND>` removes this room's pin for that provider. Each later launch follows the trusted project's `[accounts]`, then the machine's `[accounts.use]`, then `default` live ([Accounts at start](./getting-started.md#accounts)). Output names the deciding layer as `project default`, `machine default`, or `provider default`, even when the room already follows it. It takes no account name and cannot combine with `--global`. Before removing the pin, it resolves and checks the inherited account: an unreadable machine `config.toml`, untrusted or stale project account selections, an undeclared selected account, or a named account's unusable home or hooks refuses with its fix and leaves the pin intact. Only the reset kind's outcome is checked; another kind's account error does not block it. Resolver refusals are prefixed with `cannot reset this room's account: cannot resolve this room's account: ...`.

With machine selection `codex = "work"`, a room pin to `default`, and one running Codex agent on `default`:

```console
$ rimz accounts use --reset codex
this room now follows the machine default for codex (`work`); 1 running codex agent(s) keep `default` until they end
$ rimz accounts use --reset codex
this room already follows the machine default for codex (`work`)
```

`use --global` writes `[accounts.use] <KIND> = "<NAME>"` in the machine `config.toml`, preserving comments and sibling tables, and leaves every room record alone. The name must be declared for that kind; an unknown name refuses with the configured names and `rimz accounts add <kind> <name>`. A named account's missing home or missing or untrusted hooks refuses with the preflight fix and writes nothing, just as bare `use` does. `default` skips that named-account preflight.

The machine selection reaches new rooms and existing inheriting rooms, whether running or stopped, whose provider follows the machine default. Output counts and names live following rooms, and names `rimz accounts use --reset <kind>` inside a pinned room as the way to make it follow again. Room pins and trusted project selections take precedence ([Accounts at start](./getting-started.md#accounts)); running agents keep their accounts. `use --global <kind> default` removes that provider's machine entry and says new and following rooms use the provider's own home. It accepts any provider, so it also clears a hand-set entry for one without named accounts; an empty `[accounts.use]` table is removed, and clearing an absent entry succeeds without a write.

```console
$ rimz accounts use --global claude work
new rooms and rooms following the machine default now use claude account `work`; no live room follows it for claude; a pinned room keeps its account until `rimz accounts use --reset claude` runs inside it
```

A global change can move an inheriting room outside a session's history pool; it does not block the change. A later resume names the current deciding layer and refuses with `rimz accounts use <kind> <session-account>` to pin the room and resume, then `rimz accounts use --reset <kind>` to follow that default again.

## `list`

```sh
rimz accounts list [--json]
```

`list` prints one table with a row per account of each supported kind, `default` first within its kind and an empty line between kinds, then the legend for its two markers, and each problem on its own line with its fix:

```console
$ rimz accounts list
   KIND    NAME     STATUS           5h LEFT              7d LEFT              CREDITS  AGENTS  HISTORY  HOME
○  claude  default  ready            100% · 3h31m · 0.0x  89% · 4d03h · 0.3x   -        -       -        ~/.claude
●  claude  work     ready            98% · 4h21m · 0.2x   31% · 13h51m · 0.8x  -        -       shared   ~/.rimz/accounts/claude/work

●  codex   default  hooks untrusted  ∞                    64% · 5d11h · 1.6x   -        -       -        ~/.codex
●  this room (claude)   ●  new rooms (codex)   ○  new rooms
RimZ hooks for codex account `default` at `/home/me/.codex` are untrusted (SessionStart, UserPromptSubmit, SubagentStart, SubagentStop, Stop, PermissionRequest, PreToolUse, PostToolUse, PreCompact, PostCompact); run /hooks inside codex and trust the RimZ hooks
```

`list` reads from a position: the room the command runs in, or the project at the current directory, or the one `--root` names. `●` marks the account a launch from there uses for its kind: the room's pin at that root, running or stopped, else the trusted project's `[accounts]`, then the machine's `[accounts.use]`, then `default`, resolved live. A blocked or unreadable project selection, undeclared account, or unreadable machine core leaves the affected kind without `●` and prints a warning with the fix; healthy kinds keep their markers. A kind pinned to `default` needs neither project nor machine configuration. An unreadable room record leaves every kind without `●`; machine selections can still carry `○`. `list` still exits 0.

`○` marks the machine's `[accounts.use]` selection or `default` when the machine selects nothing for the kind, on every kind where that is not the `●` account. This is the default for new rooms without a project selection or explicit pin, and for existing rooms following the machine default. When no account of a kind carries `●`, the machine-default account still carries `○`.

The legend line under the table names every layer that decides `●` among the kinds shown, in the order `this room`, `this project`, `new rooms`. With multiple deciding layers, each names its provider kinds:

| Legend | `●` is decided by |
| --- | --- |
| `●  this room   ○  new rooms` | Room pins decide all displayed `●` markers. An unpinned kind follows project and machine defaults live. |
| `●  this project   ○  new rooms` | Trusted project selections decide all displayed `●` markers. |
| `●  new rooms` | Machine selections or provider defaults decide all displayed `●` markers. An account carries `○` only where a kind has no `●`, and `   ○  new rooms` follows the legend then. |
| `●  this room (codex)   ●  new rooms (claude)   ○  new rooms` | Codex is pinned; Claude follows the machine default or its own home. |
| `○  new rooms` | Nothing: the position's selection is blocked or unreadable. |

`5h LEFT` and `7d LEFT` are the account's five-hour and seven-day usage windows, read as what is left, as [`rimz providers`](./providers.md) reads them: 100 minus the used percentage, or 100% before the window's clock starts, then the time until the window resets. A model-scoped window stays in [`rimz providers`](./providers.md). `CREDITS` is the banked reset-credit count: `-` at zero, or the faint unknown cell without a login or a reading.

| Cell | Meaning |
| --- | --- |
| `85% · 2h46m · 0.3x` | 85% left; resets in 2 hours 46 minutes; using budget at 0.3 times even pace. |
| `100% · ready` | The window's clock has not started. |
| `∞` | The limit is lifted, or the account has no subscription windows. |
| `–` | Unknown: no reading yet, or the account is logged out or not declared. |

Pace is the used share divided by the elapsed share of the window: `1.0x` lasts exactly to reset; above it, the account runs dry early at that average rate. It is rounded to one decimal, with `>10x` at a rounded pace of 10.0 or higher. A trailing `–` means less than 5% of the window has elapsed, too early to tell. Pace is absent for a lifted, unknown, not-started or exhausted window, or without a duration, reset time or positive elapsed time. Its color follows `[theme.display.budget_bar.burn_rate]`, the same bands as the sidebar reset marker.

To fill these columns `list` probes each account's login and runs the usage reads that are due, the same ones `rimz providers` runs, and re-probes an account its last probe found logged out. It also re-probes older cached records whose login state is ambiguous. A cold run therefore waits on the provider, with a spinner on a terminal; a read that fails leaves the last stored reading.

On a terminal narrower than the table, `HOME` clips with `…` and every other column stays whole; piped output and `--json` are never clipped.

`AGENTS` counts the agents on the account that the [live-agent checks of `add`](#add) count, across every live room, `-` for none. A remote-control daemon is not an agent and is not counted here. When the rooms cannot be listed, or one live room's agents cannot be read, the column reads `–` and a warning on stderr names the cause.

`HISTORY` is the account's `history` mode, `shared` or `standalone`, and `-` for `default` and for a selected account nothing declares.

`HOME` for a `default` row is the directory the provider resolves from your environment, so a `CLAUDE_CONFIG_DIR` or `CODEX_HOME` you have exported shows there, unless it names a declared account's home, as it does in a pane born on that account; then the row shows the provider's own home. `STATUS` is one of:

| Status | Meaning | Fix the problem line names |
| --- | --- | --- |
| `ready` | A room can launch into the account. | none |
| `logged out` | The home and its hooks are in order, but the provider reports no login under it. A setup problem below wins over it. | The provider's login command under the home (`CLAUDE_CONFIG_DIR=<home> claude`, `CODEX_HOME=<home> codex`, or the bare command for `default`) |
| `home missing` | The home is not a directory. | `rimz accounts add <kind> <name>`, or `rimz hooks install <kind>` for `default` |
| `hooks missing` | The home lacks RimZ hooks. | `rimz accounts add <kind> <name>`, or `rimz hooks install <kind>` for `default` |
| `hooks untrusted` | The hooks are installed but the provider has not trusted them. Codex gates new hooks behind its own trust step. | Start the provider under the home (`CODEX_HOME=<home> codex`, or plain `codex` for `default`), run `/hooks`, and trust the RimZ hooks |
| `unavailable` | A room, project, or machine selection names an account that is not declared, or a kind without named accounts. | `rimz accounts add <kind> <name>`, or the command that clears the selection |

`rimz start` refuses to launch into a named account with a setup problem status ([Accounts at start](./getting-started.md#accounts)); for `default` it walks the provider's own hook setup instead. `list` itself exits 0 whatever the statuses are.

`--json` prints an array with one object per row.

| Field | Value |
| --- | --- |
| `kind` | `claude` or `codex`. |
| `name` | The account name, `default` included. |
| `home` | Absolute path of the home, or `null` when the provider's home cannot be resolved. |
| `history` | `shared` or `standalone`; `null` for `default` and for a selected account nothing declares. |
| `machine_default` | Always present: `true` for the machine's selection for new rooms, or the `default` row when no selection is set. |
| `problem` | The problem line with its fix. Present only when the account has a problem. |
| `status` | The `STATUS` word in snake case: `ready`, `logged_out`, `home_missing`, `hooks_missing`, `hooks_untrusted`, or `unavailable`. |
| `active` | `true` on the account marked `●`. |
| `default_for` | Always present: every layer that selects the account, of `this_room` (a room pin only), `this_project`, and `new_rooms`, or `[]`. |
| `agents` | The `AGENTS` count, `0` for none, or `null` when the rooms or one live room's agents could not be read. |
| `windows` | Always present: the account's 5h and 7d windows in used terms, each with `duration_mins` (`300` or `10080`) and, when known, `used_percentage`, `resets_at`, and `lifted`, the fields [`rimz providers --json`](./providers.md) uses; `[]` when there is no reading. |
| `reset_credits` | Present when read: `count`, `soonest_expiry`, `expiries`, and `effect`, the same shape as [`rimz providers --json`](./providers.md). |
| `metered` | `true` for a subscription account, `false` for one without usage windows, `null` when unknown. |

`rimz doctor` reports an exported-home conflict as the problem on that account's row and recommends unsetting the variable and selecting the account with `rimz accounts use --global`, unless it is the current launch account for that provider at the position. `rimz doctor` lists named accounts and the current room's selections in its ACCOUNTS section with a `DEFAULT FOR` column (`this room`, `this project`, `new rooms`); its JSON account rows always include `machine_default` too ([Diagnose with doctor](./getting-started.md#diagnose-with-doctor)).

## `redeem`

```sh
rimz accounts redeem <KIND> [NAME] [--dry-run] [--yes]
```

| Argument or flag | Effect |
| --- | --- |
| `KIND` | `codex` or `claude`. |
| `NAME` | A declared account or `default`. When omitted, the account a launch here uses (room, project, then machine selection), or `default` if none is selected. |
| `--dry-run` | Read and print the preview, then exit 0 without prompting, stamping, or spending. |
| `--yes` | Spend without prompting. Required for a non-terminal stdin unless `--dry-run` is set. |

Other kinds refuse with `<kind> has no named accounts; accounts are supported for claude, codex`, exit 1.

The preview on stdout names the resolved login, banked credits and soonest expiry (countdown and wall clock), then each unscoped 5h and 7d window's used percentage and reset countdown, or `∞` for a window the provider is not enforcing, which gets no redemption line. Codex refills now and restarts each reset a full window from now. A free reset closer than `resume.auto_redeem_min_gain` produces a warning about giving it up, not a refusal. Codex's final line is auto-redeem's forecast (`off`, `armed`, or `holding`) if the longest window ran dry now, not a verdict on a live agent. Claude refills now while its reset stays at the displayed time, with no forecast: automatic Claude redemption is not supported. A Claude `hold` row explains why banked credits cannot be spent now; a cooldown deadline uses the same local wall-clock format as the reset rows.

Without `--yes`, stderr asks `redeem one credit now? [y/N]`. Declining exits 0 with `nothing spent`. A held Claude preview prints `<outcome>: <reason>` and exits 3 (`no_credit`) or 7 (`cooldown`) without prompting, stamping, or recording an attempt. Zero credits exits 3 after the preview without prompting. `--dry-run` still exits 0, including when held. With `RIMZ_OAUTH_USAGE_OFFLINE` enabled, the command refuses before a provider read and names the switch to unset.

Confirmation takes the same account-wide lock as auto-redeem, without fetching the preview again. If the shared redeem stamp changed since the preview (another redemption or an auto-redeem attempt started or finished while you decided), or it is a live helper reservation, the run refuses and asks you to rerun. Otherwise manual redemption bypasses auto-redeem's cooldown. The provider can still answer `already_redeemed` or `no_credit` if the prepared credit changed while you decided. Each attempt is recorded in `rimz stats --assists` with reason `manual`, not counted as an automation assist.

Claude preview reads usage and the account's organization profile using its existing OAuth credentials; RimZ never refreshes those credentials. Confirmation sends exactly one claim POST, never retried. A transport failure, unreadable or malformed response, or non-200 status leaves the outcome unknown: check Claude's Settings > Usage before retrying. The pending reservation prevents another attempt for ten minutes. Only a confirmed `reset` triggers a fresh usage read.

| Exit | Outcome |
| --- | --- |
| `0` | `reset`, `--dry-run`, or declined confirmation. A reset whose usage refresh fails still exits 0, warning on stderr that the sidebar will catch up on its next refresh. |
| `3` | `no_credit`, including zero credits in the preview. |
| `4` | `nothing_to_reset`; the credit stays available. |
| `5` | `already_redeemed`. |
| `6` | `unknown`. |
| `7` | `cooldown`. |
| `1` | Refused or failed; the error and fix are on stderr. |

## `remove`

```sh
rimz accounts remove <KIND> <NAME>
```

`remove` deletes the `[accounts.<KIND>.<NAME>]` entry from the machine `config.toml` (and the `[accounts.<KIND>]` table when it becomes empty). If that account is the machine selection, the same write clears its `[accounts.use]` entry; new rooms and existing rooms following that default now use `default` for that provider. A selection naming another account stays untouched. The home directory, its credentials, and its transcripts stay on disk, and `rimz accounts add <KIND> <NAME> --home <that home>` brings the account back with them.

```console
$ rimz accounts remove claude work
cleared the machine selection; new rooms and rooms following the machine default now use claude account `default`
removed claude account `work`; its home ~/.rimz/accounts/claude/work and the provider files in it stay on disk; add it back to resume its sessions, or use `rimz accounts use claude default` for future launches
```

A room pinned to the removed account keeps that pin, so its next launch of that provider fails with the account's declaration fix. Add the account again, or run `rimz accounts use <KIND> default` inside the room to pin future launches to the provider's own home. `rimz accounts use --reset <KIND>` instead unpins and follows live defaults; it refuses if the project still names the removed account. Old sessions still require their account to be declared to resume, and the room to select an account that shares its history: `default` or any shared account for a shared one, the account itself for a standalone one. `rimz reset --account <KIND>=<NAME>` remains a destructive alternative that rebuilds the room.

If a live room pins the account, removal proceeds and warns with the room's name and the add, unpin, or pin-to-`default` fixes. The warning checks pins only; it does not enumerate older account stamps on running agents.

Removing an account that is not configured prints ``no <kind> account `<name>` is configured; nothing to remove`` and exits 0. `default` cannot be removed, and asking exits 1.

Nothing else deletes an account home either. `rimz uninstall` keeps `accounts/` and `skills/` under the RimZ home whatever flags you pass, `--all` included ([Uninstall RimZ](./maintenance.md#uninstall-rimz)), so provider credentials outlive RimZ. Delete the home yourself when you want them gone.
