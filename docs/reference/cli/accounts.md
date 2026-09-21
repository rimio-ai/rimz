# Accounts CLI

`rimz accounts` declares, lists, and removes the named provider accounts a room can launch into. A named account is a separate provider home: RimZ launches Claude with `CLAUDE_CONFIG_DIR`, and Codex with `CODEX_HOME`, set to that home, so credentials, settings, and transcripts stay apart. The `default` account is the provider's own home, resolved the way the provider CLI resolves it, and is never declared.

The commands work inside or outside a room. `add` and `remove` write the machine `config.toml`, and `add` also writes into the account home; no command touches a running room. A room picks its accounts when it is born, with `rimz start --account` ([Accounts at start](./getting-started.md#accounts)), and changes them only through `rimz reset --account` ([Reset a wedged room](./maintenance.md#reset-a-wedged-room)). Inside a live room, `start --account` accepts the account that room already runs on and refuses any other; the reset rebuilds the room with no agents in it. The workflow is in the [accounts guide](../../guide/accounts.md).

```sh
rimz accounts add claude work                        # declare, create the home, install hooks
rimz accounts add codex personal --home ~/codex-me   # place the home yourself
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
3. Installs RimZ hooks into that home's provider config (`settings.json` for Claude, `config.toml` for Codex), the same install `rimz hooks install` runs against the provider's own home.
4. Prints where the account lives, the command that logs the provider in once under that home, and the `rimz start` flag that uses it.

```console
$ rimz accounts add claude work
✓ claude  installed 13 hooks → ~/.rimz/accounts/claude/work/settings.json  (new file)
claude account `work` lives at ~/.rimz/accounts/claude/work
  log in once   CLAUDE_CONFIG_DIR=/home/me/.rimz/accounts/claude/work claude
  use it        rimz start --account claude=work
```

RimZ writes no credentials and copies no settings or skills into the home. Run the printed login command once so the provider stores its own credentials there. For Codex the login line reads `CODEX_HOME=<home> codex`, and a new Codex account also needs its hooks trusted inside Codex (see `hooks untrusted` under [`list`](#list)).

Rerunning `add` for an account that already exists leaves the config entry alone, creates the home if it is missing, and refreshes the hooks, so a setup that stopped part way completes. `--home` on a rerun must name the same directory.

`add` refuses these cases and changes nothing:

| Case | Error |
| --- | --- |
| `NAME` is `default` | `` `default` is <kind>'s own home and needs no declaring; choose another name `` |
| `NAME` breaks the name rules | `` invalid account name `<name>`; expected 1-32 characters of ... `` |
| `--home` differs from the existing account's home | `` <kind> account `<name>` already lives at `<home>`; rerun without --home, or remove the account first `` |
| The home breaks a [home rule](#kinds-names-and-homes) | `` `accounts.<kind>.<name>.home` is `<home>`, ... `` with the rule it breaks |

## `list`

```sh
rimz accounts list [--json]
```

`list` prints one row per account of each supported kind, `default` included, and then each problem on its own line with its fix:

```console
$ rimz accounts list
KIND    NAME      HOME                          STATUS
claude  default   ~/.claude                     native
claude  work      ~/.rimz/accounts/claude/work  ready
codex   default   ~/.codex                      native
codex   personal  ~/codex-me                    hooks missing
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
| `problem` | The problem line with its fix. Present only when the account has a problem. |

`rimz doctor` checks the same accounts in its ACCOUNTS section and marks the ones the current room uses ([Diagnose with doctor](./getting-started.md#diagnose-with-doctor)).

## `remove`

```sh
rimz accounts remove <KIND> <NAME>
```

`remove` deletes the `[accounts.<KIND>.<NAME>]` entry from the machine `config.toml` (and the `[accounts.<KIND>]` table when it becomes empty) and nothing else. The home directory, its credentials, and its transcripts stay on disk, and `rimz accounts add <KIND> <NAME> --home <that home>` brings the account back with them.

```console
$ rimz accounts remove claude work
removed claude account `work`; its home ~/.rimz/accounts/claude/work and the provider files in it stay on disk, and a room still using it refuses to start until `rimz reset`
```

A room born on the removed account keeps that selection, so its next `rimz start` fails with ``unknown claude account `work`; configured: default; run `rimz accounts add claude work` ``. Either add the account again, or run `rimz reset`, which clears the room's selection; `rimz reset --account <KIND>=<NAME>` picks another in the same step.

Removing an account that is not configured prints ``no <kind> account `<name>` is configured; nothing to remove`` and exits 0. `default` cannot be removed, and asking exits 1.

Nothing else deletes an account home either. `rimz uninstall` keeps `accounts/` and `skills/` under the RimZ home whatever flags you pass, `--all` included ([Uninstall RimZ](./maintenance.md#uninstall-rimz)), so provider credentials outlive RimZ. Delete the home yourself when you want them gone.
