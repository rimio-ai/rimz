# Paths CLI

`rimz paths` prints where RimZ keeps every file for the room the current directory resolves to: the machine-wide home, the room's state and runtime directories, and the account-global directories beside them. It lists `home/ws/` to find the room's directory and creates nothing, so it answers on a machine that has never opened a room. The layout it reports, and how to move an older install onto it, is the [configuration guide](../../guide/configuration.md#where-rimz-keeps-its-files).

```sh
rimz paths [--json]
```

| Flag | Meaning |
| --- | --- |
| `--json` | Print one JSON object instead of the table. See [JSON](#json). |

The [global flags](../cli.md#global-flags) apply. `rimz paths` resolves the room from the directory alone, the way `rimz start` does, and ignores the room identity an agent's pane carries. `--root <PATH>` names the project instead ([which room a command reaches](../cli.md#which-room-a-command-reaches)).

The command exits `1` with the error on stderr when the directory it resolves from does not exist, or when the room's runtime directory would put a socket path over the `AF_UNIX` limit (108 bytes including the terminator, 104 on macOS). The second error names the path and the fix, `export XDG_RUNTIME_DIR=/tmp/rimz-$(id -u)`.

## The table

```console
$ rimz paths
PATH              LOCATION
home              /home/me/.rimz
config            /home/me/.rimz/config.toml
theme             /home/me/.rimz/theme.toml
loop config       /home/me/.rimz/loop.toml
remote            /home/me/.rimz/remote.toml
agents home       /home/me/.rimz
workspace id      ws_f89e49906df0621ad2765112
workspace dir     myrepo-f89e
project root      /home/me/src/myrepo
state dir         /home/me/.rimz/ws/myrepo-f89e
runtime dir       /run/user/1000/rimz/ws/myrepo-f89e
room tmp          /home/me/.rimz/ws/myrepo-f89e/tmp
shared            /home/me/.rimz/ws/myrepo-f89e/shared
out               /home/me/.rimz/ws/myrepo-f89e/out
team notes        /home/me/.rimz/ws/myrepo-f89e/team-notes
automation notes  /home/me/.rimz/ws/myrepo-f89e/automation-notes
handoffs          /home/me/.rimz/handoffs
runtime root      /run/user/1000/rimz
logs              /home/me/.rimz/logs
loops             /home/me/.rimz/loops
web               /home/me/.rimz/web
accounts          /home/me/.rimz/accounts
cache             /home/me/.rimz/cache
providers cache   /home/me/.rimz/cache/providers
builds            /home/me/.rimz/builds
```

| Row | Meaning |
| --- | --- |
| `home` | The RimZ home: `$RIMZ_HOME` when set, else `$HOME/.rimz`, else `rimz-home` under the system temp directory when `HOME` is unset. Every durable row below sits under it except `agents home` when `RIMZ_AGENTS_HOME` moves it. |
| `config`, `theme`, `loop config`, `remote` | The machine config files: `config.toml`, `theme.toml`, `loop.toml`, and `remote.toml` in `home`. |
| `agents home` | Where profiles, subagents, teams, traits, the skill library, and installed [packs](./packs.md) resolve. It equals `home` unless `RIMZ_AGENTS_HOME` overrides it. Packs live in its `packs/` child, with no separate row. |
| `workspace id` | The room's identity, `ws_<24hex>`, derived from the project root. Flags, JSON fields, and `RIMZ_WORKSPACE_ID` carry this value. |
| `workspace dir` | The directory name the room's files live under: `<slug>-<hex>`. The slug is the project root's basename with every character other than a letter, digit, or `_` turned into `-`, cut to 20 characters (`root` when nothing is left). The hex is the first 4 hex digits of the id, two more each time another directory already holds that prefix. An existing directory whose `workspace.json` names the id is reused, so the name stays fixed once created. |
| `project root` | The project the room belongs to, as the [room resolution](../cli.md#which-room-a-command-reaches) chose it. |
| `state dir`, `runtime dir` | The room's durable directory under `home/ws/` and its tmpfs directory under `runtime root/ws/`, both named by `workspace dir`. |
| `room tmp` | The room's temp root, holding one `<handle>/` directory per agent, shared with the subagents it launches, and `_unnamed/` for launches without a handle. |
| `shared` | The room's shared directory, where agents hand each other files. |
| `out` | Where RimZ writes results: subagent and peer responses and watched-command output, under `out/<reader>/` for the agent that reads them, or `out/_unnamed/` for a reader without a handle. |
| `team notes` | Archived team runs, each holding its manifest, board, and scratch files. A checkout's `.rimz/team` link points to its current run. |
| `automation notes` | Kept workflow and loop notes, with a dated folder per run. Workflow folders also keep `history.txt`. |
| `runtime root` | `$XDG_RUNTIME_DIR/rimz`, else `/tmp/rimz-<uid>/rimz`. `home/run` links here once a room is born, and a birth under another runtime root repoints it. RimZ itself never reads through the link. The tmux server RimZ rooms run on has its socket here, at `<runtime root>/tmux/server`, which is the `-S` argument for reaching a room's server by hand. |
| `handoffs` | Reserved for agent hand-off notes. RimZ names the directory and the skills that write the notes own it. |
| `accounts` | Provider homes RimZ placed for named accounts, `accounts/<kind>/<name>/`, and shared role, effort, and closed-day spend history under `.history/`. No RimZ command removes this tree. |
| `logs` | Machine-wide JSONL logs: assists, focus repairs, loop runs, and user inputs. |
| `loops` | Loop arming and strike overlays, the loop start throttle queue, and their locks. |
| `web` | `rimz web` ttyd and share records, their locks, and the Zellij web config. |
| `builds` | Staged `rimz reload` builds. |
| `cache`, `providers cache` | Pet assets, ttyd binaries and fonts, and the Zellij presence plugin under `cache/`, and provider caches (accounts, rate limits, credits, spend, pricing) under `cache/providers/`. Deleting them costs a cold provider dashboard and one full spending walk, and loses account budget choices and park state plus auto-redeem stamps and rate memos. Recorded spend history stays under `accounts/.history/`. |

## JSON

`--json` prints one object with schema `rimz.paths.v2`. Its keys are `schema`, `home`, `config`, `theme`, `loop_config`, `remote`, `agents_home`, `workspace_id`, `workspace_dir`, `project_root`, `state_dir`, `runtime_dir`, `room_tmp`, `shared`, `out`, `team_notes`, `automation_notes`, `handoffs`, `runtime_root`, `logs`, `loops`, `web`, `accounts`, `cache`, `providers_cache`, and `builds`, each a string with the row's meaning above.

```sh
cd "$(rimz paths --json | jq -r .state_dir)"
```
