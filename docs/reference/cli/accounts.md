# Accounts CLI

`rimz accounts` declares the named provider accounts a room can launch into. A named account is a separate provider home: RimZ launches Claude with `CLAUDE_CONFIG_DIR` and Codex with `CODEX_HOME` pointing at it, so credentials, settings, and transcripts stay apart. The `default` account is the provider's own home, resolved the way the provider CLI resolves it; it is never declared. The commands work inside or outside a room and write only the machine config file and the account home. Choosing an account for a room is `rimz start --account` ([Getting started](./getting-started.md)); the workflow is in the [accounts guide](../../guide/accounts.md).

```sh
rimz accounts add claude work                        # declare, create the home, install hooks
rimz accounts add codex personal --home ~/codex-me   # place the home yourself
rimz accounts list                                   # every account and whether a room can use it
rimz accounts list --json
rimz accounts remove claude work                     # forget the entry; the home stays on disk
```

Named accounts are supported for `claude` and `codex`. Names follow `[a-z0-9][a-z0-9_-]*`, at most 32 characters, and `default` is reserved.

## `add`

```sh
rimz accounts add <KIND> <NAME> [--home <PATH>]
```

| Argument or flag | Effect |
| --- | --- |
| `KIND` | `claude` or `codex` |
| `NAME` | The account name rooms select with `--account <KIND>=<NAME>` |
| `--home` | The provider home for this account; a relative path resolves against the current directory. Without it, a new account lives at `$XDG_DATA_HOME/rimz/accounts/<KIND>/<NAME>` |

`add` does three things, in order: it writes `[accounts.<KIND>.<NAME>]` (with `home` when `--home` is given) to the machine `config.toml`, creates the home directory, and installs RimZ hooks into that home's provider config exactly as `rimz hooks install <KIND>` does for the default home. It then prints the command that logs the provider in once under that home, such as `CLAUDE_CONFIG_DIR=<home> claude`. RimZ writes no credentials and copies no settings or skills into the home.

Rerunning `add` for an existing account keeps its home and refreshes its hooks, so a setup that stopped part way completes. `--home` naming a different directory than the existing entry is refused. A home that is the provider's own home, or that another account of the same kind already uses, is refused.

## `list`

```sh
rimz accounts list [--json]
```

One row per account of each supported kind, `default` included: kind, name, home, and status. `native` marks a `default` account, `ready` a named account a room can launch into, and `home missing`, `hooks missing`, or `hooks untrusted` a named account `rimz start` would refuse; each problem prints below the table with its fix. `--json` emits an array of `{kind, name, home, problem?}`. `rimz doctor` repeats these verdicts in its ACCOUNTS section and marks the accounts the current room launches under; a broken account that room uses, or a selection naming an account no longer declared, counts as a problem.

## `remove`

```sh
rimz accounts remove <KIND> <NAME>
```

`remove` deletes the `[accounts.<KIND>.<NAME>]` entry and nothing else: the home directory, its credentials, and its transcripts stay on disk, and adding the account again with the same home picks them back up. A room whose frozen selection names a removed account refuses to start until `rimz reset --account <KIND>=<NAME>` picks another. Removing an account that is not configured succeeds with a note; `default` cannot be removed.
