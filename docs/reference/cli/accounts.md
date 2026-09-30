# Accounts CLI

`rimz accounts` declares, selects, lists, and removes the named provider accounts a room can launch into. A named account is a separate provider home: RimZ launches Claude with `CLAUDE_CONFIG_DIR`, and Codex with `CODEX_HOME`, set to that home. Credentials, sessions, and transcripts stay apart; `add` links settings to the provider's own home. The `default` account is the provider's own home, resolved the way the provider CLI resolves it, and is never declared.

The commands work inside or outside a room. `add`, `use`, and `remove` write the machine `config.toml`, and `add` also writes into the account home; no command touches a running room. A room picks its accounts when it is born ([Accounts at start](./getting-started.md#accounts)), and changes them only through `rimz reset` ([Reset a wedged room](./maintenance.md#reset-a-wedged-room)). Run `start --account` from inside the room's own multiplexer session and it refuses without reading the value, naming `rimz reset --account` as the way to change them. From outside, a room that already recorded a selection accepts a request matching it and refuses any other. The reset that moves a room rebuilds it with no agents in it. The workflow is in the [accounts guide](../../guide/accounts.md).

```sh
rimz accounts add claude work                        # declare, share settings, install hooks
rimz accounts add codex personal --home ~/codex-me   # place the home yourself
rimz accounts use codex personal                     # machine selection for new rooms
rimz accounts list                                   # every account and whether a room can use it
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
rimz accounts add <KIND> <NAME> [--home <PATH>]
```

| Argument or flag | Effect |
| --- | --- |
| `KIND` | `claude` or `codex`. |
| `NAME` | The account name a room selects with `--account <KIND>=<NAME>`. |
| `--home <PATH>` | The account's provider home, such as an existing `~/.claude-work`. Without it, the home is the default location above. |

`add` runs these steps in order:

1. Writes `[accounts.<KIND>.<NAME>]` to the machine `config.toml`, with `home` only when `--home` is given.
2. Creates the home directory.
3. Links the entries below to the provider's own home as `default` resolves it, honoring the ambient `CLAUDE_CONFIG_DIR` or `CODEX_HOME`. Reports linked entries, existing links, and any moves to `.orig`.
4. Installs RimZ hooks through the named home's config link (`settings.json` for Claude, `config.toml` for Codex), updating the shared file in the provider's own home too.
5. Prints where the account lives, the command that logs the provider in once under that home, the `rimz start` flag that uses it, and the `rimz accounts use` command that selects it for new rooms.

| Provider | Shared files | Shared directories |
| --- | --- | --- |
| Codex | `config.toml`, `AGENTS.md` | none |
| Claude | `settings.json`, `settings.local.json`, `CLAUDE.md` | `skills/`, `plugins/`, `agents/`, `commands/`, `output-styles/` |

Missing directory targets are created; missing file targets remain dangling links until written. Existing entries other than links to the intended target move to `<name>.orig`, never get deleted, and are reported as `moved aside <slot> → <orig>`. Codex carries missing `[hooks.state]` keys into the shared config before moving its old `config.toml`; existing target keys win. Other settings are not merged.

```console
$ rimz accounts add claude work
linked settings to /home/me/.claude: settings.json, settings.local.json, CLAUDE.md, skills, plugins, agents, commands, output-styles
✓ claude  installed 13 hooks → ~/.rimz/accounts/claude/work/settings.json  (new file)
claude account `work` lives at ~/.rimz/accounts/claude/work
  log in once   CLAUDE_CONFIG_DIR=/home/me/.rimz/accounts/claude/work claude
  use it        rimz start --account claude=work
  new rooms     rimz accounts use claude work
```

RimZ writes no credentials. Credentials, sessions, and transcripts remain per account; the entries above are shared by links, not copied. Run the printed login command once so the provider stores its own credentials there. For Codex the login line reads `CODEX_HOME=<home> codex`; trust hooks with `/hooks` once per account path (see `hooks untrusted` under [`list`](#list)).

Rerunning `add` leaves the config entry alone, creates the home if missing, migrates settings to links, and refreshes hooks. Correct links remain unchanged and print `settings already shared with <default-home>`. This also repairs a settings link replaced by a provider save, moving the replacement to `.orig`. `--home` on a rerun must name the same directory.

`add` refuses these cases. The `.orig` check runs before any settings entry changes, but after first-declaration config writing and home creation; that declaration remains for a rerun. The other checks, including resolved-home and exported-home checks, precede those writes.

| Case | Error |
| --- | --- |
| A move-aside entry already has `<slot>.orig` | `` cannot move <slot> aside: <orig> exists; remove or rename <orig>, then rerun `rimz accounts add` `` |
| The home resolves through links to the provider's own home | `` `--home` names the provider's own home, which is the `default` account (<home>); choose another home `` |
| `NAME` is `default` | `` `default` is <kind>'s own home and needs no declaring; choose another name `` |
| `NAME` breaks the name rules | `` invalid account name `<name>`; expected 1-32 characters of ... `` |
| `--home` differs from the existing account's home | `` <kind> account `<name>` already lives at `<home>`; rerun without --home, or remove the account first `` |
| The home breaks a [home rule](#kinds-names-and-homes) | `` `accounts.<kind>.<name>.home` is `<home>`, ... `` with the rule it breaks |
| The shell exports `CLAUDE_CONFIG_DIR` or `CODEX_HOME` pointing at any declared named account of that kind (also checked on reruns) | `` `CODEX_HOME` is exported as `<home>`, the home of codex account `<exported-name>`; unset `CODEX_HOME` so `default` resolves to codex's own home, e.g. `env -u CODEX_HOME rimz accounts add codex <name>` `` |

## `use`

```sh
rimz accounts use <KIND> <NAME>
rimz accounts use codex personal
rimz accounts use codex default
```

`use` writes `[accounts.use] <KIND> = "<NAME>"` in the machine `config.toml`, preserving comments and sibling tables. The name must be declared for that kind; an unknown name refuses with the configured names and `rimz accounts add <kind> <name>`. A missing home or missing or untrusted hooks produces a warning but does not prevent selection: birth checks those preconditions again.

The output says that new rooms now use the selected account and running rooms keep their account until `rimz reset`. Explicit flags and trusted project selections still take precedence ([Accounts at start](./getting-started.md#accounts)). `use <kind> default` removes that provider's entry and says new rooms use the provider's own home. It accepts any provider, so it also clears a hand-set entry for one without named accounts; an empty `[accounts.use]` table is removed, and clearing an absent entry succeeds without a write.

```console
$ rimz accounts use claude work
new rooms now use claude account `work`; running rooms keep their account until `rimz reset`
```

## `list`

```sh
rimz accounts list [--json]
```

`list` prints one row per account of each supported kind, `default` included, and then each problem on its own line with its fix:

```console
$ rimz accounts list
KIND    NAME      HOME                          STATUS         NEW ROOMS
claude  default   ~/.claude                     native         yes
claude  work      ~/.rimz/accounts/claude/work  ready          -
codex   default   ~/.codex                      native         yes
codex   personal  ~/codex-me                    hooks missing  -
RimZ hooks are missing for codex account `personal` at `/home/me/codex-me`; run `rimz accounts add codex personal`
```

`HOME` for a `default` row is the directory the provider resolves from your environment, so a `CLAUDE_CONFIG_DIR` or `CODEX_HOME` you have exported shows there. `STATUS` is one of:

| Status | Meaning | Fix the problem line names |
| --- | --- | --- |
| `native` | The `default` account. RimZ runs no checks on it here; `rimz start` walks the provider's own hook setup. | none |
| `ready` | A named account a room can launch into. | none |
| `home missing` | The home is not a directory. | `rimz accounts add <kind> <name>` |
| `hooks missing` | The home lacks RimZ hooks. | `rimz accounts add <kind> <name>` |
| `hooks untrusted` | The hooks are installed but the provider has not trusted them. Codex gates new hooks behind its own trust step. | Start the provider under the home (`CODEX_HOME=<home> codex`), run `/hooks`, and trust the RimZ hooks |

`rimz start` refuses to launch into an account with any of the last three statuses ([Accounts at start](./getting-started.md#accounts)). `list` itself exits 0 whatever the statuses are.

`--json` prints an array with one object per row. It carries no status word: test for `problem`.

| Field | Value |
| --- | --- |
| `kind` | `claude` or `codex`. |
| `name` | The account name, `default` included. |
| `home` | Absolute path of the home, or `null` when the provider's home cannot be resolved. |
| `machine_default` | Always present: `true` for the machine's selection for new rooms, or the `default` row when no selection is set. |
| `problem` | The problem line with its fix. Present only when the account has a problem. |

`NEW ROOMS` marks the machine selection. A dangling machine selection adds a problem row with the commands to declare or clear it. `rimz doctor` reports an exported-home conflict as the problem on that account's row and recommends unsetting the variable and selecting the account with `rimz accounts use`, except inside a room born on that account, where RimZ exports the home itself. `rimz doctor` also marks named machine selections in its ACCOUNTS section, alongside the ones the current room uses; its JSON account rows always include `machine_default` too ([Diagnose with doctor](./getting-started.md#diagnose-with-doctor)).

## `remove`

```sh
rimz accounts remove <KIND> <NAME>
```

`remove` deletes the `[accounts.<KIND>.<NAME>]` entry from the machine `config.toml` (and the `[accounts.<KIND>]` table when it becomes empty). If that account is the machine selection, the same write clears its `[accounts.use]` entry and the output says new rooms now use `default` for that provider. A selection naming another account stays untouched. The home directory, its credentials, and its transcripts stay on disk, and `rimz accounts add <KIND> <NAME> --home <that home>` brings the account back with them.

```console
$ rimz accounts remove claude work
cleared the machine selection; new rooms now use claude account `default`
removed claude account `work`; its home ~/.rimz/accounts/claude/work and the provider files in it stay on disk, and a room still using it refuses to start until `rimz reset`
```

A room born on the removed account keeps that selection, so its next `rimz start` fails with ``unknown claude account `work`; configured: default; run `rimz accounts add claude work` ``. Either add the account again, or run `rimz reset`, which clears the room's selection; `rimz reset --account <KIND>=<NAME>` picks another in the same step.

If a live room still selects the account, removal proceeds and appends ``warning: room <session> is running on it; its <kind> dashboard stops refreshing until you add the account back or `rimz reset` that room``. With multiple rooms, the warning names each session and applies to all their provider dashboards.

Removing an account that is not configured prints ``no <kind> account `<name>` is configured; nothing to remove`` and exits 0. `default` cannot be removed, and asking exits 1.

Nothing else deletes an account home either. `rimz uninstall` keeps `accounts/` and `skills/` under the RimZ home whatever flags you pass, `--all` included ([Uninstall RimZ](./maintenance.md#uninstall-rimz)), so provider credentials outlive RimZ. Delete the home yourself when you want them gone.
