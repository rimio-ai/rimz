# Config CLI

`rimz config` reads and edits the per-machine config set under `~/.config/rimz/` — `config.toml`, `theme.toml`, `agents.toml`, `loop.toml` — plain commented TOML you own. There is no config daemon and no bespoke format: `set` is a convenience that routes one dotted key to its owning file, validates the value, and writes it durably while preserving your comments, so you never have to remember which file holds a key. Reverse any change by re-running `set` or editing the file by hand. The field model, dotted-key catalog, and merge order are the [configuration guide](../../guide/configuration.md); `list-themes` and `list-pets` are the read-only pickers for the two config values with many choices.

## Read and edit config

```sh
rimz config init [--force] [--print]
rimz config path
rimz config get [KEY] [--json]
rimz config set <KEY> <VALUE>
```

`init` writes the commented templates under `~/.config/rimz/`; `--print` sends them to stdout instead of touching disk, and `--force` replaces an existing set (the clean-reset path, versus `rimz setup`'s merge). `path` prints the resolved `config.toml` path. `get` loads the effective config: no key prints the whole config, a dotted key prints one value, `--json` emits JSON. `set` edits one dotted key — it rejects unknown keys, validates, preserves comments, and writes atomically; a bare value becomes a TOML value when it parses and a string otherwise.

```sh
rimz config set theme "Catppuccin Mocha"     # routes theme.scheme into theme.toml
rimz config set theme.pets.enabled true       # a real bool, validated before write
rimz config set resume.auto_continue true      # routes into config.toml
```

## List themes

```sh
rimz list-themes [--json]
```

`list-themes` prints the bundled Alacritty theme names, each usable verbatim as `rimz config set theme.scheme <name>`. On a terminal it renders an aligned table: each theme's name, then grouped palette chips (background/foreground, then the six ANSI hues) under a legend header; off a terminal it prints one name per line. `--json` emits the list as an array. The palette model and custom theme files are in [theme.md](../../guide/theme.md).

## List pets

```sh
rimz list-pets [--json]
```

`list-pets` previews each bundled provider-dashboard pet and each pet installed under `~/.codex/pets/` as a medium cell-art sprite in a width-fitted grid, streaming rows as sprites load, fetching and caching the built-in sheets, and honoring `RIMZ_PETS_OFFLINE`. Installed pets are labeled by the slug you pass to `rimz config set theme.pets.pet <slug>`. Off a terminal it prints pet ids one per line, and `--json` emits the id array with installed slugs after the built-ins.

The whole config model — the two tiers, the file each key owns, and safe regeneration — is the [configuration guide](../../guide/configuration.md).
