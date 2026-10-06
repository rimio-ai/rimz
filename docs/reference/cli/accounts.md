# Accounts CLI

`rimz accounts` declares, selects, lists, and removes the named provider accounts a room can launch into. A named account is a separate provider home: RimZ launches Claude with `CLAUDE_CONFIG_DIR`, and Codex with `CODEX_HOME`, set to that home. Credentials stay apart; `add` and every launch link the rest of a `shared` account, or the settings of a `standalone` one, to the provider's own home. The `default` account is the provider's own home, resolved the way the provider CLI resolves it, and is never declared.

The commands work inside or outside a room, except bare `use`, which requires running inside one. Bare `use` writes only the current room's launch default for that provider, leaving running agents and machine and project configuration unchanged. `add`, `use --global`, and `remove` write the machine `config.toml`; `add` also writes into the account home. A room picks its defaults at birth and keeps them across restarts ([Accounts at start](./getting-started.md#accounts)), so the machine selection reaches only rooms born after it. Inside its own multiplexer session, `start --account` refuses and names `accounts use` instead. The workflow is in the [accounts guide](../../guide/accounts.md).

```sh
rimz accounts add claude work                        # declare, link the home, install hooks
rimz accounts add codex personal --home ~/codex-me   # place the home yourself
rimz accounts use codex personal                     # this room's account for future launches
rimz accounts use --global codex personal            # machine selection for new rooms
rimz accounts list                                   # every account, its usage left, and whether a room can use it
rimz accounts list --json
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
linked to /home/me/.claude: settings.json, settings.local.json, CLAUDE.md, skills, plugins, agents, commands, output-styles
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
rimz accounts use codex personal
rimz accounts use --global codex personal
rimz accounts use --global codex default
```

Bare `use` changes the current room's launch default for that provider. The selection must pass the home and hook checks before the room record changes. An unknown account names `rimz accounts add`; a missing home or missing or untrusted hooks refuses with its fix rather than merely warning. The command does not read project configuration. It prints the new default and the count of running root agents on the prior account; those agents keep it until they end. Restart, resume, and fork of a session from the prior account refuse with the `rimz accounts use` command that reopens it; crash recovery skips it as `different account`. A repeated selection reports that the room already launches on that account. Outside a room it refuses rather than touching the machine configuration: `` `rimz accounts use` changes the running room it is run inside; run it inside one, or pass --global to set the machine default for new rooms ``.

`use --global` writes `[accounts.use] <KIND> = "<NAME>"` in the machine `config.toml`, preserving comments and sibling tables, and leaves every room record alone. The name must be declared for that kind; an unknown name refuses with the configured names and `rimz accounts add <kind> <name>`. A missing home or missing or untrusted hooks produces a warning but does not prevent selection: birth checks those preconditions again.

The machine selection reaches only rooms born after it: an existing room, running or stopped, keeps its recorded account, and the output names `rimz accounts use <kind> <name>` inside the room as the way to move it. Explicit birth flags and trusted project selections still take precedence ([Accounts at start](./getting-started.md#accounts)). `use --global <kind> default` removes that provider's machine entry and says new rooms use the provider's own home. It accepts any provider, so it also clears a hand-set entry for one without named accounts; an empty `[accounts.use]` table is removed, and clearing an absent entry succeeds without a write.

```console
$ rimz accounts use --global claude work
new rooms now use claude account `work`; existing rooms, running or stopped, keep their recorded account until `rimz accounts use claude work` runs inside each
```

## `list`

```sh
rimz accounts list [--json]
```

`list` prints one table with a row per account of each supported kind, `default` first within its kind and an empty line between kinds, then the legend for its two markers, and each problem on its own line with its fix:

```console
$ rimz accounts list
   KIND    NAME     STATUS           5h LEFT       7d LEFT       AGENTS  HISTORY  HOME
○  claude  default  ready            100% · 3h31m  89% · 4d03h   -       -        ~/.claude
●  claude  work     ready            98% · 4h21m   31% · 13h51m  -       shared   ~/.rimz/accounts/claude/work

●  codex   default  hooks untrusted  ∞             64% · 5d11h   -       -        ~/.codex
●  this room   ○  new rooms
RimZ hooks for codex account `default` at `/home/me/.codex` are untrusted (SessionStart, UserPromptSubmit, SubagentStart, SubagentStop, Stop, PermissionRequest, PreToolUse, PostToolUse, PreCompact, PostCompact); run /hooks inside codex and trust the RimZ hooks
```

`list` reads from a position: the room the command runs in, or the project at the current directory, or the one `--root` names. `●` marks the account a launch from there uses for its kind: the selection the room recorded at that root, running or stopped; without one, the trusted project's `[accounts]`, then the machine's `[accounts.use]`, then `default`, the order `rimz start` follows. When no room is recorded there and the project's account selection waits on `rimz trust grant`, no kind carries `●` and a warning on stderr names the fix, since `rimz start` would refuse. When the deciding selection names an account that is not declared, that kind carries no `●`; the account's `unavailable` row and problem line say why. When the position's selection cannot be read at all (a project `.rimz/config.toml` with a TOML error, say), a warning on stderr carries the error, no kind carries `●`, and the list marks only the machine's selections with `○`; `list` still exits 0.

`○` marks the account new rooms start on, the machine's `[accounts.use]` selection or `default` when the machine selects nothing for the kind, on every kind where that is not the `●` account. A kind without `○` starts new rooms on its `●` account. When no account of a kind carries `●`, the new-rooms account still carries `○`.

The legend line under the table names the layer that decides `●`, the highest among the kinds shown:

| Legend | `●` is decided by |
| --- | --- |
| `●  this room   ○  new rooms` | The selection recorded by the room at the position. A kind the record leaves out is `default`. |
| `●  this project   ○  new rooms` | The trusted project's selection. A kind the project leaves out follows the machine's. |
| `●  new rooms` | The machine's selection. An account carries `○` only where a kind has no `●`, and `   ○  new rooms` follows the legend then. |
| `○  new rooms` | Nothing: the position's selection is blocked or unreadable. |

`5h LEFT` and `7d LEFT` are the account's five-hour and seven-day usage windows, read as what is left, as [`rimz providers`](./providers.md) reads them: 100 minus the used percentage, or 100% before the window's clock starts, then the time until the window resets. A model-scoped window stays in [`rimz providers`](./providers.md).

| Cell | Meaning |
| --- | --- |
| `85% · 2h46m` | 85% left; the window resets in 2 hours 46 minutes. |
| `100% · ready` | The window's clock has not started. |
| `∞` | The limit is lifted, or the account has no subscription windows. |
| `–` | Unknown: no reading yet, or the account is logged out or not declared. |

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
| `default_for` | Always present: every layer that selects the account, of `this_room`, `this_project`, and `new_rooms`, or `[]`. |
| `agents` | The `AGENTS` count, `0` for none, or `null` when the rooms or one live room's agents could not be read. |
| `windows` | Always present: the account's 5h and 7d windows in used terms, each with `duration_mins` (`300` or `10080`) and, when known, `used_percentage`, `resets_at`, and `lifted`, the fields [`rimz providers --json`](./providers.md) uses; `[]` when there is no reading. |
| `metered` | `true` for a subscription account, `false` for one without usage windows, `null` when unknown. |

`rimz doctor` reports an exported-home conflict as the problem on that account's row and recommends unsetting the variable and selecting the account with `rimz accounts use --global`, except inside a room born on that account, where RimZ exports the home itself. `rimz doctor` lists named accounts and the current room's selections in its ACCOUNTS section with a `DEFAULT FOR` column (`this room`, `this project`, `new rooms`); its JSON account rows always include `machine_default` too ([Diagnose with doctor](./getting-started.md#diagnose-with-doctor)).

## `remove`

```sh
rimz accounts remove <KIND> <NAME>
```

`remove` deletes the `[accounts.<KIND>.<NAME>]` entry from the machine `config.toml` (and the `[accounts.<KIND>]` table when it becomes empty). If that account is the machine selection, the same write clears its `[accounts.use]` entry and the output says new rooms now use `default` for that provider. A selection naming another account stays untouched. The home directory, its credentials, and its transcripts stay on disk, and `rimz accounts add <KIND> <NAME> --home <that home>` brings the account back with them.

```console
$ rimz accounts remove claude work
cleared the machine selection; new rooms now use claude account `default`
removed claude account `work`; its home ~/.rimz/accounts/claude/work and the provider files in it stay on disk; add it back to resume its sessions, or use `rimz accounts use claude default` for future launches
```

A room selecting the removed account keeps that default, so its next launch fails with the account's declaration fix. Add the account again, or run `rimz accounts use <KIND> default` inside the room for future launches. Old sessions still require their account to be declared to resume, and the room to select an account that shares its history: `default` or any shared account for a shared one, the account itself for a standalone one. `rimz reset --account <KIND>=<NAME>` remains a destructive alternative that rebuilds the room.

If a live room still selects the account as its launch default, removal proceeds and warns with the room's name and the add-or-switch fixes. The warning reads room records only; it does not enumerate older account stamps on running agents.

Removing an account that is not configured prints ``no <kind> account `<name>` is configured; nothing to remove`` and exits 0. `default` cannot be removed, and asking exits 1.

Nothing else deletes an account home either. `rimz uninstall` keeps `accounts/` and `skills/` under the RimZ home whatever flags you pass, `--all` included ([Uninstall RimZ](./maintenance.md#uninstall-rimz)), so provider credentials outlive RimZ. Delete the home yourself when you want them gone.
