# Config CLI

`rimz config` reads and edits the per-machine config: three commented TOML files under the RimZ home, `~/.rimz/`, that you own and can edit by hand. `rimz list-themes` and `rimz list-pets` print the choices for the two settings with many values, `theme.scheme` and `theme.pets.pet`. What each key means, the merge order, and the project config tier are in the [configuration guide](../../guide/configuration.md).

```sh
rimz config init [--force] [--print]
rimz config path
rimz config get [KEY] [--json]
rimz config set <KEY> <VALUE>
rimz list-themes [--json]
rimz list-pets [--json]
```

A refusal or error exits 1, following the CLI-wide [exit codes](../cli.md#exit-codes); the [global flags](../cli.md#global-flags) are defined there too.

## The config files

The directory is `$RIMZ_HOME`, or `~/.rimz/` when `RIMZ_HOME` is unset; [`rimz paths`](./paths.md) prints it with every other location. A dotted key's first segment decides which file `rimz config set` writes.

| File | Keys routed to it | What it holds |
| --- | --- | --- |
| `config.toml` | every key not listed below (`agents.*` preferences and commands, `resume.*`, `harness.*`, `sidebar.*`, `remote_control.*`, `timezone`, ...) | room behaviour: accounts, notifications, remote control, multiplexer options, resume, compaction |
| `theme.toml` | `theme.*` | sidebar appearance and pets |
| `loop.toml` | `loop.*` | loop defaults and task definitions |

Two key families land under a different table than their dotted name suggests. `theme.colors.*` lands in the root `[colors.*]` tables of `theme.toml`, so an Alacritty palette pasted there keeps working. `loop.*` drops the `loop.` prefix inside `loop.toml`, so `loop.default-timeout` is the file's top-level `default-timeout`.

The same directory holds `remote.toml` (SSH room aliases, managed by [`rimz remote`](./remote.md)) and `agents/`, `subagents/`, `teams/`, `traits/`, and `skills/` ([Markdown definitions](../definitions.md) and the shared [skill library](../../guide/configuration.md#skills)). `rimz config get` includes loaded definitions; `set` does not edit their source files.

## Write the templates

```sh
rimz config init           # write all three files
rimz config init --print   # print the templates, write nothing
```

`init` writes the shipped template for each of the three files and prints `wrote <path>` for each. Every template line is commented, so a fresh file follows the defaults; `rimz config init --print` is the complete, current list of keys and defaults.

| Flag | Effect |
| --- | --- |
| (none) | Refuses when any of the three files exists: `` <path> already exists; pass --force to replace the per-machine config set ``. Nothing is written. |
| `--force` | Overwrites all three files with fresh templates. Every value and comment you added is lost. |
| `--print` | Prints the three templates to stdout, each under a `# === <file> ===` header, and touches no file. `--force` has no effect alongside it. |

`init --force` is the clean reset. To refresh existing files against newer templates and keep your values, run [`rimz setup`](./getting-started.md#set-up-the-machine), which merges instead.

## Print the config path

`rimz config path` prints the resolved path of `config.toml`, whether or not the file exists. The other three files sit beside it.

```console
$ rimz config path
/home/me/.rimz/config.toml
```

## Read a value

`rimz config get` loads the effective config (the three files layered over built-in defaults, plus profiles and teams from `~/.rimz/`) and prints it. It reads strictly: a file with a TOML or validation error fails the command with that error instead of falling back to defaults.

| Form | Prints |
| --- | --- |
| `rimz config get` | The whole effective config as TOML. |
| `rimz config get <KEY>` | A scalar or array bare on one line; a table as TOML. |
| `--json` | The same selection as pretty-printed JSON. |

```console
$ rimz config get sidebar.focus_key
Alt+p
$ rimz config get theme.scheme
error: config key `theme.scheme` is unset
```

A key RimZ knows but that has no stored value and no printed default fails with `` config key `KEY` is unset `` and exit 1. Several optional keys whose default is resolved at runtime answer this way until you set them, among them `theme.scheme`, `theme.mode`, and `loop.default-timeout`; the template shows their defaults. A key RimZ does not know fails with `` unknown config key `KEY` ``, and a key with an empty segment (`theme..scheme`) fails with `config keys use non-empty dotted segments`.

## Set a value

`rimz config set <KEY> <VALUE>` changes one key in its owning file and prints `set <KEY>`. It runs these steps, and stops without writing at the first one that fails:

1. Parses the value (rules below) and rejects a key RimZ does not know, so a typo never lands in the file.
2. Reads the owning file, or starts from its template when the file does not exist.
3. Checks the value against the key's type and format.
4. Sets the key in place: it uncomments the file's commented line for the key when there is one and otherwise adds the key, keeping every other line and comment as it was.
5. Validates the whole resulting file.
6. Writes the file atomically (a temporary file renamed over the original).

```sh
rimz config set theme "Catppuccin Mocha"                # theme.scheme in theme.toml
rimz config set theme.pets.enabled true                 # a TOML boolean
rimz config set notifications.triggers '["waiting", "failed"]'
rimz config set theme.display.context_meter.red '{ percent = 90, tokens = 400000 }'
```

To undo a change, set the old value again, or delete the line from the file to return to the default.

### How the value is parsed

A value that parses as a TOML value is written as that value (`80`, `true`, `["a", "b"]`, an inline table); anything else is written as a string (`fresh`, `Catppuccin Mocha`). Quote a string that would otherwise parse as another type, such as `'"true"'`.

These keys always take a string, so `200k`, `50/day`, or a theme named `0x96f` needs no extra quoting:

| Keys | Value |
| --- | --- |
| `theme.scheme`, and the shorthand `theme` | a bundled theme name or a path to an Alacritty TOML file |
| `theme.glyphs.set` and the shorthand `theme.glyphs`; `theme.glyphs.<set>.<namespace>.<role>` | a glyph set name or glyph string |
| `harness.smart_compact` | a token count (`200k`) or a percentage (`70%`) |
| `harness.compact_instruction` | any string, including `""` |
| `harness.idle_compact` | `off`, `auto`, or `always` |
| `harness.idle_compact_after` | a duration such as `59m` or `2h` |
| `harness.budget`, `accounts.budget.<kind>` | an amount ending in `/day` |
| `harness.turn_budget` | a dollar amount such as `3` or `$2.50` |
| `resume.auto_redeem_min_gain` | a duration |
| `gc.older_than` | a duration such as `8h` or `3d` |

The two shorthands write the full key: `rimz config set theme <name>` sets `theme.scheme`, and `rimz config set theme.glyphs <set>` sets `theme.glyphs.set`.

### What set refuses

| Attempt | Error |
| --- | --- |
| Profile or team definition keys | Refused with the Markdown source path; edit that file and run `rimz agents validate`. |
| A key RimZ does not know | `` unknown config key `KEY` `` |
| A whole table, such as `theme.display` or `loop.tasks.<name>` | `` unknown config key `KEY` ``. Set one field inside it, or edit the table in the file. |
| `notifications.handler` and its fields | `` config key `KEY` is an array of tables; edit <path to config.toml> `` |
| A value of the wrong type or format | `` invalid value VALUE for `KEY`: ... ``, or a key-specific message such as `harness.idle_compact must be one of off, auto, or always` |
| Any key, when the owning file already has a TOML or validation error | `` cannot set `KEY`: the existing config is invalid `` (or `cannot edit <path> — the file has a TOML error`). Fix the file first. |

### Keys that do more than write

Three keys check or change the machine beyond the file.

`agents.isolation sandbox` probes bubblewrap before writing. On Linux it looks for `bwrap` on `PATH` and runs a trivial command inside a bubblewrap mount view; on any other OS, with `bwrap` missing, or when the probe fails (commonly unprivileged user namespaces disabled by sysctl or AppArmor), the command prints the reason and leaves `config.toml` unchanged. `host`, the default, runs no probe. The setting applies to later launches, restarts included; running panes keep the isolation they started with. The per-launch `--isolation` flag on [`rimz agents`](./agents.md), [`rimz teams`](./teams.md), [`rimz subagents`](./subagents.md), and `rimz agents fork` takes precedence, and what isolation changes is in [security](../../guide/security.md#sandbox-isolation).

`remote_control.claude` and `remote_control.codex` switch the provider's remote-control host on the running machine. Setting either to `true` checks the host's preconditions first and refuses with the fix when they fail. After the write, `claude` adds or closes the `claude remote-control` pane in every running room's `rimzd` view, and `codex` starts or stops the shared Codex remote-control daemon. A hand edit reaches Claude hosts on the `rimzd` view's next repair pass, but for Codex it changes only what later room starts do, so use `set` to start or stop a running daemon. What each host runs is in the [remote guide](../../guide/remote.md#answer-asks-from-your-phone).

## List themes

`rimz list-themes` prints the bundled Alacritty theme names, sorted. Each name works as the value of `rimz config set theme.scheme <name>`; quote a name with spaces.

| Output | Shape |
| --- | --- |
| On a terminal | An aligned table: each name, then color chips for background and foreground and for the six ANSI hues (red, green, yellow, blue, magenta, cyan), under a legend row that labels them `bg fg` and `r g y b m c`. |
| Piped | One name per line. |
| `--json` | A JSON array of names. |

```console
$ rimz list-themes | head -3
0x96f
12-bit Rainbow
3024 Day
```

Custom scheme files and color slots are in the [theming guide](../../guide/theme.md#color-scheme).

## List pets

`rimz list-pets` lists every pet id you can give `theme.pets.pet`: the built-in pets first, then each pet installed under `~/.codex/pets/` (a directory holding `pet.json`), sorted by directory name. An installed pet's id is that directory name.

A built-in id wins a collision: install a petdex pet named `rocky` and `theme.pets.pet = "rocky"` still selects the built-in, so reach the installed one by its directory path instead. RimZ resolves the setting in order: a built-in id, then an `http://` or `https://` selector as a remote sprite sheet, then a selector containing `/` or `.` or starting with `~` as a local path, and anything else as an installed pet's directory name.

| Output | Shape |
| --- | --- |
| On a terminal | A preview grid sized to the terminal width, each pet's idle sprite over its id. Sprites draw as pixels when `theme.pets.glyphs` and the terminal allow kitty graphics, and as cell art otherwise. |
| Piped | One id per line. Loads no sprite. |
| `--json` | A JSON array of ids, built-ins first. Loads no sprite. |

On a machine with no installed pets:

```console
$ rimz list-pets --json
[
  "codex",
  "dewey",
  "fireball",
  "rocky",
  "seedy",
  "stacky",
  "bsod",
  "null-signal"
]
```

The terminal preview fetches each built-in sprite sheet over HTTPS into `~/.rimz/cache/pets/v1/assets/` on first use and reads installed pets from disk. With `RIMZ_PETS_OFFLINE` set to any value it reads the cache only. A pet that cannot load leaves an empty slot, and the grid ends with `(some pets unavailable - check network, or RIMZ_PETS_OFFLINE serves cache only)`; the command still exits 0. Render tiers, custom sheets, and petdex installs are in the [pets guide](../../guide/pets.md).
