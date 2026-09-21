# Paths CLI

`rimz paths` prints where RimZ keeps every file for the room the current directory resolves to: the machine-wide home, the room's state and runtime directories, and the account-global directories beside them. It reads nothing but the resolver and creates nothing, so it answers on a machine that has never opened a room. The layout it reports, and how to move an older install onto it, is the [configuration guide](../../guide/configuration.md#where-rimz-keeps-its-files).

```sh
rimz paths [--json]
```

The [global flags](../cli.md#global-flags) apply; `--root` picks the project the same way it does for `rimz start`.

## The table

```console
$ rimz paths
PATH                  LOCATION
home                  /home/me/.rimz
config                /home/me/.rimz/config.toml
theme                 /home/me/.rimz/theme.toml
loop config           /home/me/.rimz/loop.toml
remote                /home/me/.rimz/remote.toml
agents home           /home/me/.rimz
workspace id          ws_f89e49906df0621ad2765112
workspace dir         myrepo-f89e
project root          /home/me/src/myrepo
state dir             /home/me/.rimz/ws/myrepo-f89e
runtime dir           /run/user/1000/rimz/ws/myrepo-f89e
room tmp              /home/me/.rimz/ws/myrepo-f89e/tmp
scratch               /home/me/.rimz/ws/myrepo-f89e/tmp/scratchpad
scratch (agent view)  /tmp/scratchpad
handoffs              /home/me/.rimz/handoffs
runtime root          /run/user/1000/rimz
logs                  /home/me/.rimz/logs
loops                 /home/me/.rimz/loops
web                   /home/me/.rimz/web
accounts              /home/me/.rimz/accounts
cache                 /home/me/.rimz/cache
providers cache       /home/me/.rimz/cache/providers
builds                /home/me/.rimz/builds
```

| Row | Meaning |
| --- | --- |
| `home` | The RimZ home: `$RIMZ_HOME` when set, else `~/.rimz`. Every durable row below sits under it. |
| `config`, `theme`, `loop config`, `remote` | The machine config files. |
| `agents home` | Where profiles, subagents, teams, traits, and the skill library resolve. It equals `home` unless `RIMZ_AGENTS_HOME` overrides it. |
| `workspace id` | The room's identity, `ws_<24hex>`, derived from the project root. Flags, JSON fields, and `RIMZ_WORKSPACE_ID` carry this value. |
| `workspace dir` | The directory name the room's files live under: the project root's basename and the first hex digits of the id. The name lengthens by two hex digits when another project already holds the shorter one, and it stays fixed once created. |
| `state dir`, `runtime dir` | The room's durable directory under `home/ws/` and its tmpfs directory under `runtime root/ws/`, both named by `workspace dir`. |
| `room tmp`, `scratch` | The room's private tmp, and the invoking agent's scratch dir inside it: `agents/<handle>` for a named agent, the shared `scratchpad` otherwise. |
| `scratch (agent view)` | That scratch dir as the invoking agent sees it: `/tmp/scratchpad` under sandbox isolation, the host path otherwise. |
| `runtime root` | `$XDG_RUNTIME_DIR/rimz`, else `/tmp/rimz-<uid>/rimz`. `~/.rimz/run` links here once a room is born; RimZ itself never reads through the link. The tmux server RimZ rooms run on has its socket here, at `<runtime root>/tmux/server`, which is the `-S` argument for reaching a room's server by hand. |
| `handoffs` | Reserved for agent hand-off notes. |
| `accounts` | Provider homes RimZ placed for named accounts, `accounts/<kind>/<name>/`: credentials and transcripts that nothing regenerates and no RimZ command removes. |
| `logs`, `loops`, `web`, `builds` | Machine-wide directories: append-only logs, loop overlays, web daemon records, and reload staging. |
| `cache`, `providers cache` | Everything RimZ can rebuild: downloaded assets and the presence plugin under `cache/`, and the provider caches (accounts, rate limits, credits, spend, pricing) under `cache/providers/`. Safe to delete; the cost is a cold provider dashboard and one full spending walk. |

## JSON

`--json` prints one object with schema `rimz.paths.v1`. Its keys are `schema`, `home`, `config`, `theme`, `loop_config`, `remote`, `agents_home`, `workspace_id`, `workspace_dir`, `project_root`, `state_dir`, `runtime_dir`, `room_tmp`, `scratch`, `scratch_agent_view`, `handoffs`, `runtime_root`, `logs`, `loops`, `web`, `accounts`, `cache`, `providers_cache`, and `builds`, each a string with the row's meaning above.

```sh
cd "$(rimz paths --json | jq -r .state_dir)"
```
