# Packs

`rimz packs` manages machine-wide definition and skill packs linked from local checkouts. It needs no running room. Global flags and output conventions are in the [CLI reference](../cli.md); definition precedence and inheritance are in [Markdown definitions](../definitions.md#definition-tiers), and skill precedence is in [configuration](../../guide/configuration.md#skills).

```sh
rimz packs [--json]
rimz packs list [--json]
rimz packs install <PATH>
rimz packs show <PACK> [--json]
rimz packs remove <PACK>
rimz packs exclude <PACK>[:<NAME>] [--type <TYPE>]
rimz packs include <PACK>[:<NAME>] [--type <TYPE>]
```

Pack names use `[A-Za-z0-9_-]+` and are unique across installed sources. Item types are `agents`, `subagents`, `teams`, `traits`, and `skills`. Kind bases belong to `agents`, not a separate type. Qualified `<pack>:<name>` keys select items for exclusions and definition `agent:` parents; they are not launch specs.

## Install or refresh a linked source

`install` takes an existing local source directory containing [rimz-packs.toml](#manifest). The argument must be `.`, `..`, `~`, or start with `/`, `./`, `../`, or `~/`. Use `~/path` for a path under your home; quote it if you want RimZ rather than the shell to expand it. A `~user` spelling, bare name, `github:` source, or URL is not supported.

Every pack listed in the manifest is installed, regardless of `default`. The source is canonicalized and linked under the agents home's `packs/`; its contents are not copied. Definition, skill, and manifest edits are read on the next load without reinstalling. Newly added pack names require another `install` to enter the record. Refreshing a source adds and drops pack names to match the manifest and keeps exclusions for names still installed. An unchanged installation prints `Already up to date:` and succeeds.

```console
$ rimz packs install /home/me/src/rimz-crew
Installed crew from /home/me/src/rimz-crew (linked).
  Next: rimz packs show crew
```

Install refuses a missing directory, a missing or invalid manifest, a source equal to, inside, or containing the agents home, or a pack name already installed from another source. A name collision identifies its owner and the `rimz packs remove <pack>` fix. Load problems such as item conflicts are warnings after a successful install, not a rollback. Unknown manifest keys also warn.

Linked installs have no permission-summary prompt or `--yes` flag. Definitions grant the same capabilities as home definitions; later checkout edits take effect too. See [the security model](../../guide/security.md#what-rimz-changes-on-your-machine).

## List installed packs

Bare `packs` and `packs list` are equivalent. Rows follow source installation order and pack order in the record. `LOADED` counts items whose fate is `loads`, not every shipped file or generated team seat. `PROBLEMS` is the pack's problem count, or `-`; source-attributed problems follow the table. An empty installation prints the local install form. Listing succeeds even when it reports load problems.

```console
$ rimz packs
PACK  LOADED  SOURCE                           PROBLEMS
crew       7  /home/me/src/rimz-crew (linked)  -
```

`rimz packs list` and `rimz packs show` have [routine-command permissions](../agent-support.md#permission-modes). The bare `rimz packs` spelling and mutating verbs do not.

## Show a pack

`show` prints the description and linked source, then items grouped by type. It also lists exclusions for items no longer shipped. An installed but unusable pack can still be inspected; an unknown pack fails and lists installed names.

```console
$ rimz packs show crew
crew
Local review crew
Source: /home/me/src/rimz-crew (linked)

agents
  claude: loads
  coder: loads

subagents
  reviewer: loads

teams
  forge: loads

traits
  careful: loads

skills
  reflect: loads
  review: loads
```

| Fate | Meaning |
| --- | --- |
| `loads` | Selected by the home/pack chain and loaded. Skill rows describe that chain, not a particular provider's overrides. |
| `overridden by <path>` | Your home definition, configured command, or library skill claims the name. |
| `excluded` | This item has a recorded exclusion. |
| `excluded (type)` | Its whole type is excluded. |
| `conflicts with <pack> (<file>)` | Multiple non-excluded pack copies claim the name, with no home override. Neither wins by bare name. |
| `failed: <message>` | The definition could not load; the message names the cause. |

Fates use the current machine configuration and shell environment. As with config loading, skill checks are skipped inside a scoped pane; run `rimz agents validate` from a host shell to check enforced skill lists. A `loads` row does not prove provider executables, credentials, or launch preconditions.

### JSON

`packs --json` and `packs list --json` return `{packs, problems, warnings}`. Problems and warnings are arrays of `{path, message}`. Each pack has:

| Field | Value |
| --- | --- |
| `name` | Installed pack name. |
| `description` | Live manifest description, or null when the pack cannot resolve. |
| `source`, `linked`, `entry` | Canonical source path, `true` for a linked source, and its entry name under `packs/`. |
| `excluded` | `{types: [...], items: [{type, name}, ...]}`. |
| `counts` | Loaded item counts keyed by all five item types. |
| `problems` | Problems attributed to this pack. See top-level `problems` for record-wide failures. |

`packs show <pack> --json` returns that pack object, not a `{packs: [...]}` wrapper, and adds:

- `items`: `{type, name, path, fate}` rows, including overridden and excluded items. `fate.state` is `loads`, `overridden` (with `path`), `excluded`, `excluded_type`, `conflict` (with `claims: [{pack, path}, ...]` and `message`), or `failed` (with `message`).
- `unshipped_exclusions`: `{type, name}` entries still excluded but absent from the current item inventory.

## Exclude or restore items

An exclusion changes the installation record, not the checkout. `include` reverses it. Select one item with `<pack>:<name>`, or a whole type with `<pack> --type <type>`.

```sh
rimz packs exclude crew:coder --type agents
rimz packs include crew:coder --type agents
```

With no `--type`, an item name must match exactly one of the pack's five trees. Several matches fail with the explicit `--type` forms to choose from. No match fails; `include` also accepts a recorded item exclusion even when the pack no longer ships it. A bare pack name without an item or type is refused with the `rimz packs remove <pack>` form.

```console
$ rimz packs exclude crew --type teams
Excluded crew --type teams.
  teams/forge: excluded (type)
$ rimz packs include crew --type teams
Included crew --type teams.
  teams/forge: loads
```

Including one item while its type remains excluded fails with the whole-type include command. Including the type leaves any individual item exclusions in place. Repeating an already-applied change succeeds with an `already` receipt. Receipts show the selected items' new fates and any conflicts opened or resolved. Excluding a referenced base, trait, parent, child, or skill can make dependent definitions fail; another tier can supply the missing name.

## Remove a pack and clean up links

`remove` stops loading one installed pack. If it was the source's last pack, RimZ also drops the source record and entry link. The linked checkout is never deleted. Reinstalling that source installs every pack in its manifest again.

```console
$ rimz packs remove crew
Removed crew from /home/me/src/rimz-crew.
$ rimz packs
No packs installed. Install a local source: rimz packs install ./path
```

Every successful mutating verb, including an already-applied change, prunes obsolete RimZ-owned host skill links across native provider roots and declared account homes. Receipts name removed links by root. Mutations add no links; a later host launch creates them. Foreign entries are untouched. Ownership and precedence are in [configuration](../../guide/configuration.md#skills).

The record commits before cleanup. An entry-link cleanup failure still prints the removal receipt and a warning: the unrecorded entry is ignored. Incomplete host skill-link cleanup exits 1 with `pack change committed, but host skill link cleanup is incomplete`; it does not undo the pack change. `rimz uninstall` removes owned library and pack skill links but keeps `packs/` in every mode.

## Manifest

`rimz-packs.toml` lives at the source root. Each pack directory can contain `agents/`, `subagents/`, `teams/`, `traits/`, and `skills/<name>/SKILL.md`, using the same item formats as the home trees.

```toml
[[pack]]
name = "crew"
description = "Local review crew"
path = "."
default = true
requires = ["skills/essentials"]
rimz = ">=0.43"
```

| Field | Contract |
| --- | --- |
| `name` | Required string, `[A-Za-z0-9_-]+`, unique in the manifest and across installed sources. |
| `description` | Required string, nonempty after trimming. |
| `path` | Required relative path to an existing directory inside the source, after both lexical normalization and symlink resolution. Paths must be unique, including canonical aliases. `.` selects the source root. |
| `default` | Optional boolean, default false. Parsed but does not filter linked installs. |
| `requires` | Optional array of nonempty strings. Parsed only; dependencies are not installed or enforced. |
| `rimz` | Optional nonempty string. Parsed only; no version grammar or compatibility check is applied. |

At least one `[[pack]]` is required. Missing required fields, wrong types, unsafe or duplicate names, escaping or duplicate paths, and missing directories refuse installation. Unknown keys at any level warn and otherwise load. A broken live manifest withholds its source's packs; an unusable pack directory withholds that pack while other valid recorded packs can still load. A pack skill read error withholds that pack's skills, not the library or other packs. A withheld healthy skill's row names the broken skill; the pack's problem carries the read error. Excluded and overridden skill copies are not read. A failed pack item's error names the `rimz packs exclude` command that sets it aside, whether it failed on a reserved launch name, an invalid team definition, or a field its definition cannot take. Problems fail `rimz agents validate` and reach room-entry warnings and `rimz doctor`, without refusing unrelated launches. Home-only namespace reservations remain in force, including a home team `<x>-fast` when a pack supplies profile `x`; see [definition failures](../definitions.md#validation-and-failures).

## Layout and installation record

The agents home is `~/.rimz` by default. `RIMZ_HOME` moves it with the rest of RimZ; `RIMZ_AGENTS_HOME` overrides it for definitions, the skill library, and packs alone. `rimz paths` prints `agents home`; packs always live in its `packs/` child.

| Path under the agents home | Meaning |
| --- | --- |
| `packs/<entry>` | Symlink to the canonical source directory. The first 128 characters of the source basename have characters outside `[A-Za-z0-9_-]` replaced by `-`, falling back to `source` if empty; recorded entry collisions add `-2`, `-3`, and so on. |
| `packs/installed.json` | Versioned source identities, installed pack names, and exclusions. |
| `packs/installed.lock` | Serializes record read-modify-write mutations. |

The record shape for the linked source above is:

```json
{
  "version": 1,
  "sources": [
    {
      "entry": "rimz-crew",
      "source": "/home/me/src/rimz-crew",
      "linked": true,
      "ref": null,
      "commit": null,
      "packs": [
        {
          "name": "crew",
          "excluded": {"types": [], "items": []}
        }
      ]
    }
  ]
}
```

A missing record is empty. Unknown record fields are ignored; a version above 1 withholds the whole pack tier with a newer-RimZ problem. The record is the commit point: install publishes the entry before the record, and removal rewrites the record before deleting the final entry. Entries absent from the record never load and can be replaced by a later install. The durability discipline is in [store internals](../../internals/store.md#write-classes).

A linked checkout under `/tmp` or `/var/tmp` is hidden from scoped agents running `rimz` inside their mount view, because their temp unit replaces those paths. Test such a checkout from a host shell, or keep it outside those temporary trees for scoped use.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Listed or showed an installed pack, even with load problems; or committed a mutation and completed host skill-link cleanup. Already-applied changes also succeed. |
| `1` | Unknown pack, missing item, invalid source or manifest, overlapping agents home, a pack name owned by another source, an `include` of one item whose type is excluded, record/I/O failure, or incomplete post-commit host skill-link cleanup. |
| `2` | Invalid CLI syntax, pack name, item type, or non-path install argument; ambiguous item name; or an exclusion request with neither item nor type. |
