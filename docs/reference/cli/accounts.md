# Accounts CLI

`rimz accounts` compares provider accounts, plans, usage left, and launch health; it is the same as `rimz accounts list` and exits 0. `show` expands account details. The other subcommands select, declare, redeem reset credits for, and remove named accounts. A named account is a separate provider home: RimZ launches Claude with `CLAUDE_CONFIG_DIR`, Codex with `CODEX_HOME`, and Pi with `PI_CODING_AGENT_DIR`, set to that home. Credentials stay apart; `add` and every launch link the rest of a `shared` account, or the settings of a `standalone` one, to the provider's own home. The `default` account is the provider's own home, resolved the way the provider CLI resolves it, and is never declared.

The commands work inside or outside a room, except bare `use` and `use --reset`, which require running inside one. Bare `use` pins only the current room's account for that provider, leaving running agents and machine and project configuration unchanged. `--reset` removes that pin. An unpinned provider follows the trusted project's selection, then the machine's, then `default`, live on each launch ([Accounts at start](./getting-started.md#accounts)). `add`, `use --global`, and `remove` write the machine `config.toml`; `add` also writes into the account home. A machine selection reaches new rooms and existing rooms following that default, not pinned providers. Inside its own multiplexer session, `start --account` refuses and names `accounts use` instead. The workflow is in the [accounts guide](../../guide/accounts.md).

```sh
rimz accounts add claude work                        # declare, link the home, install hooks
rimz accounts add codex personal --home ~/codex-me   # place the home yourself
rimz accounts use codex personal                     # this room's account for future launches
rimz accounts use --reset codex                      # unpin; follow project and machine defaults live
rimz accounts use --global codex personal            # machine default for new and following rooms
rimz accounts list                                   # every account, its usage left, and whether a room can use it
rimz accounts list --json
rimz accounts show codex                             # one provider's account details
rimz accounts redeem codex personal --dry-run        # preview without spending
rimz accounts remove claude work                     # forget the entry; the home stays on disk
```

## Kinds, names, and homes

Named accounts exist for `claude`, `codex`, and `pi` only. `add`, `use`, and `remove` refuse kinds without named accounts with `<kind> has no named accounts; accounts are supported for claude, codex, pi`; `list` and `show` report every known kind. `use --global <kind> default` can also clear a hand-set selection for any kind.

An account name is 1 to 32 characters of `a-z`, `0-9`, `_`, and `-`, starting with a letter or digit. `default` is reserved for the provider's own home.

An optional `label` on `[accounts.<kind>.<name>]` replaces the name only in the dashboard title: `Codex · work` instead of `Codex · team-1`. Labels may repeat; selections, records, and commands still use the name. A label must be a non-empty string at most 32 columns wide, with no control characters or leading or trailing whitespace. Wide characters count as two columns; a label made only of zero-width characters is invalid. `default` is never labelled.

An invalid string label warns at room start and in `accounts list` and `show`; the dashboard uses the account name instead, without changing the account's usability. Set a valid label with `rimz accounts add <kind> <name> --label <TEXT>`, or remove it with `--label ''`. A non-string value is a TOML type error, like a non-string `history`.

Single-token account selections use `KIND@NAME`. `start --account` and `reset --account` also accept `KIND=NAME` as a compatibility alias. Mutation verbs keep their separate `KIND NAME` arguments; `show` takes `KIND[@NAME]`. `loop add --account` takes a bare name because the task supplies its provider.

Every named account has a home directory, and RimZ checks each home when it reads the machine config:

| Rule | Detail |
| --- | --- |
| Default location | Without `home`, the account lives at `~/.rimz/accounts/<KIND>/<NAME>` (under `$RIMZ_HOME` when it is set). |
| Absolute path | `home` in `config.toml` must be absolute or start with `~`. `add --home` resolves a relative path against the current directory before writing it. |
| Not the provider's own home | `~/.claude` for Claude, `~/.codex` for Codex, `~/.pi/agent` for Pi. That directory is already the `default` account. |
| One account per home | Two accounts of the same kind cannot share a home. |
| No `,` and no trailing `projects` | Provider home lists split on commas, and Claude reads a directory named `projects` as its transcript folder. |

A `config.toml` entry that breaks a home rule, or that declares `default`, makes mutations refuse with `invalid per-machine account at <path>: ...` and the fix. Listing and `show` warn, retain healthy kinds, and report the failed kind instead of failing the command.

## `add`

```sh
rimz accounts add <KIND> <NAME> [--home <PATH>] [--history shared|standalone] [--label <TEXT>]
```

| Argument or flag | Effect |
| --- | --- |
| `KIND` | `claude`, `codex`, or `pi`. |
| `NAME` | The account name a room selects with `--account <KIND>@<NAME>`. |
| `--home <PATH>` | The account's provider home, such as an existing `~/.claude-work`. Without it, the home is the default location above. |
| `--label <TEXT>` | Sets the display label; `--label ''` removes the field. On a rerun, changes only this field (and `history` when `--history` is also given); without the flag, leaves it alone. An invalid label is refused before any config write or home creation. |
| `--history <MODE>` | `shared` or `standalone`: the account's `history` field, which names its history pool. `shared`, also what an absent field means, puts the account in one pool with `default` and every other shared account of its kind; `standalone` makes it a pool of its own. On a new account the flag writes the field, and without the flag no field is written. On an existing account the flag rewrites only that field, and without the flag the field is left alone. Any other value is refused, by the flag and by the config load, naming both. |

`add` runs these steps in order:

1. For a new account, writes `[accounts.<KIND>.<NAME>]` to the machine `config.toml`, with `home` only when `--home` is given, `history` only when `--history` is given, and `label` only when a non-empty `--label` is given. For an existing account, checks any `--history` or `--label` edit against the current `config.toml` without writing it. If that check fails, the home and `config.toml` stay unchanged.
2. Creates the home directory.
3. Links the account home to the provider's own home as `default` resolves it, honoring the ambient `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, or `PI_CODING_AGENT_DIR`, under one lock per provider in `~/.rimz/accounts/.locks/`. Reports linked entries, entries moved into the provider's own home, links removed, links removed from unshared names, and any moves to `.rimz-aside`. On an existing account, writes any checked `--history` and `--label` updates after the links reach the requested mode, rereading `config.toml` for the edit. A refused rerun leaves `config.toml` unchanged.
4. Installs RimZ hooks through the named home's config link (`settings.json` for Claude, `config.toml` for Codex) or Pi's `extensions/` directory link, updating the provider's own home too.
5. Prints where the account lives, the command that logs the provider in once under that home, the `rimz start` flag that uses it, and the `rimz accounts use` commands for a running room and new rooms.

Links are relative when the account home's real location is under the real ambient `HOME` and the provider home's configured path is under the configured `HOME`. They climb from the account home's real location, then follow the provider home's configured path below `HOME`, even if that provider home is a symlink. Otherwise links are absolute, including when `HOME` is unset, empty, or cannot be resolved. This keeps links under home portable across dotfile sync and a moved home.

A `standalone` account links the settings entries; a `shared` account links those and every other top-level entry of either home that is not unshared. A name is unshared when it is private or transient. It is private when it equals a listed name or extends it with a `.` suffix (`auth.json.bak`); `.rimz-aside` is private for every provider. It is transient, for every provider, when `.lock` or `.tmp` appears past the start of the name and either ends it or is followed by `.`: `.sqlite-maintenance.lock`, `.oauth_refresh.lock.owner`, and `session_index.jsonl.tmp` are transient, while `.tmp`, `tmp`, `thread-writer-locks`, and `.lockfile` are not and stay shared. A `shared` account always links the provider's history directories, Claude's `projects/`, Codex's `sessions/` and `archived_sessions/`, and Pi's `sessions/`, creating them in the provider's own home when neither home holds one yet.

| Provider | Settings files | Settings directories | Private names |
| --- | --- | --- | --- |
| Codex | `config.toml`, `AGENTS.md` | `rules/` | `auth.json`, `app-server-control`, `app-server-daemon`, `packages`, `models_cache.json` |
| Claude | `settings.json`, `settings.local.json`, `CLAUDE.md` | `skills/`, `plugins/`, `agents/`, `commands/`, `output-styles/` | `.credentials.json`, `.claude.json`, `.oauth_refresh.lock`, `.last-update-result.json` |
| Pi | `settings.json`, `models.json`, `mcp.json`, `keybindings.json`, `trust.json`, `AGENTS.md`, `CLAUDE.md`, `SYSTEM.md`, `APPEND_SYSTEM.md` | `extensions/`, `skills/`, `prompts/`, `themes/`, `npm/`, `git/` | `auth.json`, `models-store.json`, `mcp-auth.json`, `oauth.json`, `mcp.log`, `crashes.json`, `pi-debug.log`, `pi-tui-debug.log`, `pi-tui-crash.log` |

Each entry takes one of five paths, and nothing but a link is ever removed:

| Account home | Provider's own home | Result |
| --- | --- | --- |
| Link with the text RimZ would write | any | Left alone as current, even if its target is missing. |
| Link with other text to the same existing file or directory | Present | Atomically rewritten in place, without a live-agent or daemon check or a `.rimz-aside` entry, and reported as linked. |
| Absent | any | Linked. Missing settings and history directory targets are created; other missing targets remain dangling links until written. |
| File or directory | Absent | Moved into the provider's own home, then linked. Across filesystems it stays in the account home and `add` warns. |
| File, directory, or any other link | Present, or the entry is a link | Moved to `<account home>/.rimz-aside/<UTC timestamp>/<name>`, then linked, and reported as `<account> account: moved <slot> aside to <aside>; the account now reads the copy in <default-home>`. A relative link is rewritten against the account home's real location, preserving where it resolves. A dangling link with other text takes this path too. |

A link into the provider's own home that the account's mode no longer asks for is removed and reported as `unlinked from <default-home>`; its target stays. A link into the provider's own home at an unshared name is removed in either mode, whatever its target and without the live-agent check, since the provider must own that name in each home; its target stays, and the removal is reported as `<account> account: removed the link <slot> into <default-home>; that name is per-account and no longer shared`. A real file or directory at an unshared name, and a link that points anywhere else, are left alone. When a later entry fails after others were set aside, the error ends with the same `moved <slot> aside to <aside>` lines, since a rerun finds those links in place and has nothing left to report. Codex carries missing `[hooks.state]` keys into the shared config before moving its old `config.toml` aside; existing target keys win. A carried trust key counts only while RimZ's hook remains at the same group and handler slot; otherwise trust it again through `/hooks` in Codex. Nothing else is merged.

A shared Codex account launches with `CODEX_SQLITE_HOME` set to the provider's own home, so its databases follow its sessions; a `CODEX_SQLITE_HOME` you export wins.

```console
$ rimz accounts add claude work
linked to /home/me/.claude: settings.json, settings.local.json, CLAUDE.md, skills, plugins, agents, commands, output-styles, projects
✓ claude  installed 13 hooks → ~/.rimz/accounts/claude/work/settings.json  (new file)
claude account `work` lives at ~/.rimz/accounts/claude/work
  log in once   CLAUDE_CONFIG_DIR=/home/me/.rimz/accounts/claude/work claude
  use at birth  rimz start --account claude@work
  this room     rimz accounts use claude work
  new rooms     rimz accounts use --global claude work
```

RimZ writes no credentials and never writes inside an unshared name. Unshared names remain per account; every other listed entry is shared by a link, not copied. Run the printed login command once so the provider stores its own credentials there. For Codex the login line reads `CODEX_HOME=<home> codex`; trust hooks once per account path, through its startup review or `/hooks` (see `hooks untrusted` under [`list`](#list)).

Rerunning `add` leaves the config entry alone unless `--history` or `--label` updates its field, creates the home if missing, brings the links to the account's mode, and refreshes hooks. An update rewrites an account declared as an inline table as its own `[accounts.<KIND>.<NAME>]` table, keeping its comments. Links whose text matches remain unchanged and print `already linked to <default-home>`. This also repairs a link replaced by a provider save, moving the replacement to `.rimz-aside`. `--home` on a rerun must name the same directory. An agent launch on a named account runs the same link step before it starts the provider, prints the removed-link, moved-aside, and left-in-place lines as warnings, and refuses on the same error.

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
| The shell exports `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, or `PI_CODING_AGENT_DIR` pointing at any declared named account of that kind (also checked on reruns) | `` `CODEX_HOME` is exported as `<home>`, the home of codex account `<exported-name>`; unset `CODEX_HOME` before adding or updating accounts, e.g. `env -u CODEX_HOME rimz accounts add codex <name>` `` |

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
this room now follows the machine default for codex (`codex@work`); new launches only; 1 running codex agent(s) stay on codex@default
$ rimz accounts use --reset codex
this room already follows the machine default for codex (`codex@work`)
```

`use --global` writes `[accounts.use] <KIND> = "<NAME>"` in the machine `config.toml`, preserving comments and sibling tables, and leaves every room record alone. The name must be declared for that kind; an unknown name refuses with the configured names and `rimz accounts add <kind> <name>`. A named account's missing home or missing or untrusted hooks refuses with the preflight fix and writes nothing, just as bare `use` does. `default` skips that named-account preflight.

The machine selection reaches new rooms and existing inheriting rooms, whether running or stopped, whose provider follows the machine default. The receipt says `new launches only` and counts running agents on the prior account in live following rooms, excluding pinned and project-selected rooms. If those rooms cannot be inventoried, the count is unknown rather than zero. Output also counts and names live following rooms, and names `rimz accounts use --reset <kind>` inside a pinned room as the way to make it follow again. Room pins and trusted project selections take precedence ([Accounts at start](./getting-started.md#accounts)); running agents keep their accounts. `use --global <kind> default` removes that provider's machine entry and says new and following rooms use the provider's own home. It accepts any provider, so it also clears a hand-set entry for one without named accounts; an empty `[accounts.use]` table is removed, and clearing an absent entry succeeds without a write.

```console
$ rimz accounts use --global claude work
new rooms and rooms following the machine default now use claude account `claude@work`; new launches only; 0 running claude agent(s) stay on claude@default; no live room follows it for claude; a pinned room keeps its account until `rimz accounts use --reset claude` runs inside it
```

A global change can move an inheriting room outside a session's history pool; it does not block the change. A later resume names the current deciding layer and refuses with `rimz accounts use <kind> <session-account>` to pin the room and resume, then `rimz accounts use --reset <kind>` to follow that default again.

## `list`

```sh
rimz accounts [--json] [--refresh] [--all]
rimz accounts list [--json] [--refresh] [--all]
```

Both spellings print the same overview. Providers with multiple accounts or a quota reading get their own tables, with `default` first and named accounts by name. A reading is a window, paid usage, or reset credits on an account with a login; stale cached facts on a logged-out account do not qualify. Healthy single-account providers without readings share `Other providers`. Unchecked logins and account repairs follow below. Spend history alone does not earn a table. Provider tables and `Other providers` put kinds named in the sidebar's [`provider_list`](../../guide/theme.md#display) first in listed order. `"all"` and unknown entries are ignored, duplicates use their first position, and a strict list does not hide unlisted kinds. The rest follow cached 30-day session counts across all accounts, descending; missing counts are zero and ties use registry order (Claude before Codex). Multiple-account providers keep their tables even when not installed.

| Flag | Effect |
| --- | --- |
| `--all` | Include unhealthy and uninstalled single-account providers without readings in `Other providers`. Does not change JSON rows. |
| `--refresh` | Bypass account and usage TTLs ([freshness](#how-fresh-the-figures-are)). |
| `--json` | Print the [merged account array](#json-output), including kinds hidden from the text overview. |

The heading names the provider and CLI version: the active account's version, else the default account's, else the first known. With spending recorded and every account in the default history pool, it adds `$X spent in 7d` and, for multiple members, `(shared by default, a, b)`. With any standalone account, it instead adds `shared pool $X spent in 7d`, without the member list. A tally that rounds to `$0.00` and its member list are omitted; spend-column cells are unchanged.

### Provider tables

| Column | Shows |
| --- | --- |
| (marker) | Only for providers with multiple accounts: `●` on the account a launch from here uses; `○` on the machine default when it is not the `●` row. |
| `NAME` | Bare account name; `default` is muted. |
| `LABEL` | Only when some account in this provider's table has a valid label; rows without one have an empty cell. `NAME` remains the identity. |
| `PLAN` | Plan label or a faint `–` when unknown, overridden by the first applicable state below. |
| One `<window> LEFT` column | Every window any row reports, including model sub-caps and named quotas such as Copilot's `AI credits`. Reads what's left, time to reset, then pace. Percentages and countdowns align within the column. |
| `EXTRA LEFT` | Only when some row reports paid usage: `off` when disabled, `∞` for unlimited credit, dollars left when known, else `$X used`, else `–`. An API account with a counting monthly cap shows its spend since the UTC month began as `$X used`; the sidebar's `api` bar shows the dollars left, and the detail view prints spend of cap. ` · paying` follows when paid credit is the next spend step. |
| `RESET CREDITS` | Only when some row reports reset credits: count plus the nearest expiry (`2 · exp 9d14h`), `2 · due` after expiry, a bare count without an expiry, `-` at zero, or `–` when unknown. |
| `AGENTS` | Live cross-room agent count, `-` for none, or `–` when rooms or a live room's agents could not be read. Remote-control daemons are not agents. |
| `SPEND 7d` | Only when some account is standalone: its own trailing-week spend, `-` when unrecorded, or `shared` for default-pool members. |
| `READING` | Only when some row lacks an expected usage reading: `never read` or `no reading since <date>`. Claude, Codex and Kimi expect their declared windows unless unmetered. Other accounts must be known-metered and support direct usage reads; Pi and OpenCode also need a backing login with readable quotas. OpenCode OpenAI OAuth qualifies; Pi OpenAI OAuth does not. An observed window also establishes the expectation, except on unmetered accounts. |

`PLAN` applies this precedence: `not installed`; a setup problem (`home missing`, `hooks missing`, `hooks untrusted`, `hooks disabled`, `unavailable`); `lapsed`; `no login check` for providers without a login check; `logged out`; `not probed` or `probe failed`; then the plan label. Setup problems use warning tones, lapses the failure tone, and login states their provider-status tones. A setup problem takes precedence over a lapse in both `PLAN` and its repair line.

| Window cell | Meaning |
| --- | --- |
| `85% · 2h46m · 0.3x` | 85% left; resets in 2 hours 46 minutes; using budget at 0.3 times even pace. |
| `100% · ready` | The window's clock has not started. |
| `N%` | No reset time is known. |
| `no limit` | The window is lifted, or the account has no subscription windows. |
| `–` | Lapsed, logged out, no probe record, or no reading. Paid usage and reset credits also read unknown without a login. |

Pace is the used share divided by the elapsed share of the window: `1.0x` lasts exactly to reset; above it, the account runs dry early at that average rate. It is rounded to one decimal, with `>10x` at a rounded pace of 10.0 or higher. A trailing `–` means less than 5% of the window has elapsed, too early to tell. A fixed period also reads `–` while its reset lies at or beyond its nominal length, such as the first day of a 31-day billing month. Pace is absent for a lifted, unknown, not-started or exhausted window, or without a duration or reset time; a sliding window also needs positive elapsed time. Its color follows `[theme.display.budget_bar.burn_rate]`, the same bands as the sidebar reset marker. The definition is also in `accounts --help` and `accounts list --help`; no pace legend prints beneath the tables. Tables are not clipped to the terminal width.

### Position and footer

The position is the room the command runs in, else the project at the current directory, or the one `--root` names. For providers with multiple accounts, `●` marks the account a launch from that position uses, inside or outside a room: its room pin, else the trusted project's selection, else the machine's `[accounts.use]`, else `default`, resolved live. Pins decide per provider. A blocked or unreadable selection leaves affected kinds without `●` and warns with the fix; healthy kinds keep theirs. A pin to `default` needs neither project nor machine configuration. An unreadable room record leaves all kinds without `●`.

`○` marks the machine's selection, or `default` when it selects nothing, only when that account is not active. It is the default for new rooms without a project selection or explicit pin, and existing rooms following the machine default.

After the own tables comes the marker legend, using only providers with multiple accounts:

- One deciding layer: `● next launch from here (this room)`, `(this project)`, or `(machine default)`.
- Mixed layers: `● next launch from here: codex pinned by this room, claude by machine default`. Only present groups appear, in room, project, machine order, with kinds joined by `, `.
- `   ○ new rooms` follows only when a displayed row carries `○`. With no `●`, it stands alone as `○ new rooms`. With neither marker, no legend prints.

`Other providers` follows the legend. Its columns are `PROVIDER` (product name), `LOGIN` (plan label, or `logged in`), optional `READING` under the rule above, `AGENTS`, optional `SPEND 7d` when some tally does not round to `$0.00`, then `VERSION` (`v…`, or `–` when unknown). With `--all`, unhealthy and uninstalled single-account providers without readings join this table; their `LOGIN` cells use the same state precedence as `PLAN`.

Without `--all`, installed single-account providers without readings whose login is unchecked appear in a faint `Not checked, launches proceed:` line. It sits directly below `Other providers`, or on its own when that table is empty. Groups appear in this order: each distinct `login read failed: <cause>` (or `login read failed` without a cause), then `not probed yet`, then `no login check`. Kind ids are alphabetical within each group. `--all` omits this line because its table includes those states.

`Needs you` ends the overview, listing every installed account with a problem, whether or not it has its own table. Its aligned columns are the kind id (`kind@name` for a named account), state, and short repair. Setup problems come first, then lapses, then logouts, keeping provider and account order within each class. Missing homes and hooks name `rimz accounts add <kind> <name>` or `rimz hooks install <kind>` for `default`; untrusted or disabled hooks name the action, including the named account's launch environment when needed. Configuration errors retain their full sentence. A lapse reads `no <Product> access`; restore access, then run `rimz accounts show <kind> --refresh`. A logout reads `log in once: <command>`. Uninstalled accounts have no repair line. Use `show` for the long problem, paths, and hook names.

Sections have one blank line between them and no trailing blank line, except the unchecked line directly below `Other providers`. Empty sections are omitted. If nothing qualifies, the overview points to `rimz accounts --all` to list every supported provider.

`rimz start` warns about configured account problems and opens the room; an explicit `--account` still refuses ([Accounts at start](./getting-started.md#accounts)). Agent launches refuse an unhealthy account rather than switching accounts, except that watched Codex and Hermes launches proceed to their hook-trust prompts with a notice ([launch checks](./agents.md#the-account-check-at-launch)). The account row still says `hooks untrusted` until you consent. Listing exits 0 despite account problems. `rimz doctor` additionally reports exported-home conflicts; a named account missing hooks is `hooks missing`, with `rimz accounts add <kind> <name>` as the repair.

## `show`

```sh
rimz accounts show <KIND>[@<NAME>] [--json] [--refresh]
```

`show codex` prints every Codex account as a block; `show claude@work` selects just that account. Any known provider kind is accepted, even without named accounts or cached facts. An unknown kind exits 1 with ``unknown provider kind `<kind>`; known kinds: …``; an undeclared name exits 1 with ``no <kind> account `<name>`; configured: default, …``.

Blocks use the table's state precedence in their titles, alongside the plan and account's default layers. Markers and the footer legend appear only for providers with multiple accounts, counted before the optional name filter. A single selected named account retains its canonical `kind@name` in the title and its marker when that provider has other accounts. Long problem lines retain paths, hook names, and reasons, unlike the overview's short repairs.

| Row | Shows |
| --- | --- |
| `label` | Display label, only when valid and set; the header keeps the account's real identity. |
| `probed` | Age of the last login probe, or `never`. |
| `home` | Home as `~/…`, or `–`. For `default`, the provider's environment-resolved home, unless it names a declared account's home; then the native provider home. |
| `version` | CLI version as `v…`, or `–`. Always printed. |
| `account` | Non-secret provider account id, when known. |
| `provider`, `scope` | Backing provider and scoped provider/variant, when applicable. |
| One row per window | Percentage left, reset countdown, pace when available, and observation age when known; `no limit` for a lifted window. |
| `usage` | `no limit` for an unmetered account, or its missing-reading reason under the per-account [`READING` rule](#provider-tables). Accounts without readable quotas omit a missing-reading reason. |
| `extra` | `off`, `unlimited`, or known dollars used, left, and limit; ` · paying` when paid credit is next. Without a reported limit, the ceiling is `[accounts.usage_limit_usd]`. |
| `reset credits` | Positive banked count and up to three known expiries as `YYYY-MM-DD HH:MM:SS ±HH:MM` in machine timezone, then `in <interval>` or `due`. |
| `spend` | History pool's trailing 7- and 30-day spend on default and standalone accounts; shared named members omit it. Default without published spend reads `–`. |
| `budget` | `$spent of $cap/day` or `$spent of $cap/month`, with `· parked` when the cap has parked agents. |
| `agents` | Cross-room count, `0` for none, or `–` when inventory is unavailable. |

Windows, extra usage, and reset credits belong to the credential account; spend and dollar caps belong to its history pool. On an API-billed account with a counting monthly cap, `extra` shows this UTC month's spend against that cap instead of the trailing-month display ceiling. To spend a banked reset, use [`redeem`](#redeem). Dollar amounts use the money tone; percentages and pace use the sidebar's budget and burn-rate tones.

## How fresh the figures are

Listing and `show` probe every catalog login and run due usage reads for logged-in accounts, publishing into the sidebar's caches. `show` limits usage reads to its provider kind, not its individual name; login probes still cover all kinds. They touch no agent or room. A cold run can wait on providers, with a spinner on a terminal.

| Reading | Plain run | With `--refresh` |
| --- | --- | --- |
| Login, plan, account id, version | Probe after 10 minutes; a failed probe retries after 10 seconds and retains its last good facts. A provisional Pi or OpenCode answer also retries after that floor; an undecided login reports unknown plan and metering, and no credits. A new Pi session or Pi login change follows within seconds, behind the same floor from the previous probe. A settled answer can still take up to 10 minutes to follow a switch inside a running session, a resume, or concurrent Pi sessions on different providers. For kinds supporting named accounts (Claude, Codex, and Pi), stored logged-out or ambiguous legacy records are probed again on every run. | Probe every catalog login, regardless of the selected kind or name. |
| Claude live plan | Read at the next account probe for a never-read or changed account, a changed auth-status plan, a new machine-wide cockpit session, an early usage reset, or a profile answer older than 24 hours. Attempts are at least 10 minutes apart. After a sign-in whose auth-status plan differs from the answer's saved-plan basis, auth status shows until a new read answers. A failed read keeps the last answer without changing login state or usage. | Read for every probed Claude subscription login with a known identity, except offline or behind an account producer that outlives the wait bound. |
| Windows, paid usage, reset credits | Read for logged-in accounts after 5 minutes, or 1 hour after missing/rejected credentials or subscription access. Providers without a usage API show agent-reported windows. | Read now for logged-in accounts of the selected kinds. Successful usage clears a recorded lapse. |
| Spend | Last published `provider-spending.json`. | Same; this command does not compute spend. |

Rooms and concurrent account queries share one probe and one usage read per account at a time. A forced query waits behind a live account producer, then performs its own probes and profile reads. Only a producer that outlives the batch-derived wait bound makes it serve the current cache; unforced queries keep their short wait. `RIMZ_OAUTH_USAGE_OFFLINE=1` skips usage and profile API reads, even with `--refresh`; values `0`, `false`, and empty leave reads enabled. A changed credential account identity drops the former account's windows, paid usage, and reset credits before reading again ([internals](../../internals/agents/providers.md#the-credits-cache-and-usage-identity)). Before anything publishes spend, it is `–` in text and `null` in JSON; `rimz stats` or a room's sidebar publishes it.

## JSON output

`accounts --json`, `accounts list --json`, and `accounts show … --json` print the same top-level array contract. Listing includes every known kind, including those hidden from the text overview, plus undeclared selected accounts and failed configuration rows. `show` filters by kind and optional name. Rows follow the [overview's provider order](#list), with `default` first and named accounts by name; they are not split into the text overview's sections. Credentials and account fingerprints never appear.

Every key below is present, with `null` when unknown, except `problem`, `probe_error`, `extra_credits`, and `reset_credits`, which are omitted when absent.

| Field | Meaning |
| --- | --- |
| `kind`, `account` | Provider kind and canonical `kind@name`. All registered kinds are included, not only Claude, Codex, and Pi. |
| `home`, `history` | Absolute home; `shared` or `standalone` history (`null` for default or undeclared accounts). |
| `label` | Valid display label, or `null` for default, undeclared accounts, or unset or invalid labels. Always present. |
| `machine_default` | Whether this is the machine's selection, or `default` when none is set. |
| `problem` | Problem and fix, only when present. |
| `status` | Launch health: `ready`, `login_unknown`, `logged_out`, `home_missing`, `hooks_missing`, `hooks_untrusted`, `hooks_disabled`, or `unavailable`. A lapse changes displayed `PLAN`, not this value. |
| `login` | Recorded auth state: `logged_in`, `logged_out`, `unavailable`, or `null` without a probe record. Providers without a login check also record `logged_out`; that is not a logout finding. |
| `entitlement` | `"ok"`, or `{"lapsed":{"since_ms":N}}` with first-seen Unix milliseconds. Independent of launch health and login; suppresses quota readings. Adds a lapse problem only without a setup problem (`status` `ready` or `login_unknown`). |
| `active` | A launch from this position uses the account, including outside a room. |
| `default_for` | Array containing any of `this_room` (a pin only), `this_project`, `new_rooms`, or empty. |
| `agents` | Cross-room count, `0` for none, `null` when inventory fails. |
| `pool` | History pool as `kind@name`; deduplicate spend by this key. |
| `product_name` | Dashboard title: the theme-resolved product for `default`, else `<Product> · <label>` when valid, or `<Product> · <name>`. |
| `probed_at`, `probe_error` | Last probe's RFC 3339 timestamp; failure cause only after a failed probe. |
| `plan`, `plan_label` | Plan tier and display label, both `null` while lapsed. Claude uses a recognised live profile answer bound to the current login and its unchanged saved plan: `pro` / `Claude Pro`, `max_5x` / `Claude Max 5x`, or `max_20x` / `Claude Max 20x`. Otherwise it falls back to auth status, refining `max` from credential metadata; these are not untouched `subscriptionType` values. Codex retains raw `prolite`, `pro`, and `promax`, labeled `ChatGPT Pro 100`, `ChatGPT Pro 200`, and `ChatGPT Pro 500`. |
| `account_id`, `sub_provider` | Non-secret provider account id and backing provider name. |
| `account_scope` | `{"kind":"kind_wide"}` or `{"kind":"sub_provider","provider":…,"variant":…}`. |
| `version`, `metered` | CLI version; whether the account has subscription windows (`null` when unknown). |
| `windows` | Every reported window, including named quotas and model sub-caps; `[]` without readings. Fields below. |
| `extra_credits` | `"disabled"` or `{"known":{…}}` with known `used_usd`, `remaining_usd`, `limit_usd`, or `unlimited:true`. |
| `reset_credits` | `{"count":N,"effect":…}`, plus `soonest_expiry` and sorted `expiries` when known. Effect is `restarts_window` for Codex or `keeps_schedule` for Claude. |
| `spend_step` | `subscription` (capacity available or unknown), `paid_credit` (usable paid credit next), `reset_credit` (auto-redeem would spend a reset), or `spent` (neither). `null` without a reading. |
| `spending` | History pool's spend projection, or `null` before publication. |
| `day_budget` | `{"cap_usd":…,"spend_usd":…,"parked":…}`; spend since local midnight and daily cap state; `null` without a day cap. |
| `month_budget` | The same shape for a counting monthly cap, with spend since 00:00 UTC on the 1st; `null` otherwise, including subscription-exempt pools. |

Window fields are omitted when unknown:

| Window field | Meaning |
| --- | --- |
| `scope` | `{"id":…,"label":…}` for a named quota or model sub-cap; absent for a plain time window. |
| `used_percentage` | Percent used, 0 to 100. |
| `amount` | Optional `{"used":N,"limit":N}` in the provider's integer units, with a positive limit; present only with known usage. Units agree across an account's unscoped windows. Usage may exceed the limit; its percentage floors and caps at 100. |
| `resets_at` | RFC 3339 reset time. |
| `duration_mins` | Nominal window duration, such as `300` (5h) or `10080` (7d); also the identity for an unscoped time window. |
| `observed_at` | Reading timestamp. |
| `source` | `"authoritative"` for provider API readings; absent for agent-reported readings. |
| `lifted` | `true` when the provider stopped enforcing this window; absent otherwise. |
| `fixed_period` | `true` when the clock runs on a fixed schedule regardless of usage; absent otherwise. Its countdown runs even at zero usage. |

`spending.today` is spend since local midnight in the configured `timezone`, or `null` when publication was for another local day. `headline` follows the sidebar setting; sibling `window` names `session`, `24h`, or `today`. `week`, `month`, and `year` cover trailing 7, 30, and 365 days. Each holds `usd`, `tokens`, `input`, `output`, `cache_write`, `cache_read`, and `sessions`, plus `tool_calls` and a `tools` name-to-count map when recorded.

Compared with the previous `accounts list --json`, rows now cover every kind, `windows` includes all scopes, and provider plan, identity, pool, freshness, spending, and day/month budget fields are added. `status` remains launch health and `login` remains auth state.

The hidden, deprecated `rimz providers [KIND]` alias remains for one release and writes a warning to stderr. Its human stdout matches `accounts` or `accounts show KIND`; its `--json` preserves the [legacy report](./providers.md#json-output), including auth `status` and `active:false` outside a room.

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

Pi refuses with `reset-credit request failed: pi does not support reset-credit redemption`, exit 1. Kinds without named accounts refuse with `<kind> has no named accounts; accounts are supported for claude, codex, pi`, exit 1.

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

A room pinned to the removed account keeps that pin, so its next launch of that provider fails with the account's declaration fix. Add the account again, or run `rimz accounts use <KIND> default` inside the room to pin future launches to the provider's own home. `rimz accounts use --reset <KIND>` instead unpins and follows live defaults; it refuses if the project still names the removed account. Old sessions still require their account to be declared to resume, and the room to select an account that shares its history: `default` or any shared account for a shared one, the account itself for a standalone one. `rimz reset --account <KIND>@<NAME>` remains a destructive alternative that rebuilds the room.

If a live room pins the account, removal proceeds and warns with the room's name and the add, unpin, or pin-to-`default` fixes. The warning checks pins only; it does not enumerate older account stamps on running agents.

Removing an account that is not configured prints ``no <kind> account `<name>` is configured; nothing to remove`` and exits 0. `default` cannot be removed, and asking exits 1.

Nothing else deletes an account home either. `rimz uninstall` keeps `accounts/` and `skills/` under the RimZ home whatever flags you pass, `--all` included ([Uninstall RimZ](./maintenance.md#uninstall-rimz)), so provider credentials outlive RimZ. Delete the home yourself when you want them gone.
