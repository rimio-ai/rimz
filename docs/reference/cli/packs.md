# Packs

`rimz packs` manages machine-wide definition and skill packs installed from Git or linked from local checkouts. It needs no running room. Global flags and output conventions are in the [CLI reference](../cli.md); definition precedence and inheritance are in [Markdown definitions](../definitions.md#definition-tiers), and skill precedence is in [configuration](../../guide/configuration.md#skills).

```sh
rimz packs [--json]
rimz packs list [--json]
rimz packs install <SOURCE> [--yes]
rimz packs update [<PACK>] [--yes]
rimz packs show <PACK> [--json]
rimz packs remove <PACK>
rimz packs exclude <PACK>[:<NAME>] [--type <TYPE>]
rimz packs include <PACK>[:<NAME>] [--type <TYPE>]
rimz packs customize <PACK>:<NAME> [--type <TYPE>] [--copy]
rimz packs diff <PACK>:<NAME> [--type <TYPE>]
rimz packs reset <PACK>:<NAME> [--type <TYPE>] [--identical]
rimz packs reset <PACK> [--type <TYPE>] --identical
```

Pack names use `[A-Za-z0-9_-]+` and are unique across installed sources. Item types are `agents`, `subagents`, `teams`, `traits`, and `skills`. Kind bases belong to `agents`, not a separate type. Qualified `<pack>:<name>` keys select items for exclusions and definition `agent:` parents; they are not launch specs.

## Install from Git

Every source contains a [rimz-packs.toml](#manifest). These source forms are accepted:

| Source | Repository and selection |
| --- | --- |
| `crew[/<pack>][@<ref>]` | `https://github.com/rimio-ai/rimz-crew`. |
| `skills[/<pack>][@<ref>]` | `https://github.com/rimio-ai/rimz-skills`. |
| `github:<owner>/<repo>[/<pack>][@<ref>]` | A GitHub repository. |
| `<scheme>://...[@<ref>]`, `[user@]host:path[@<ref>]` | A Git URL, with no pack selector. |
| `.`, `..`, `~`, `/path`, `./path`, `../path`, `~/path` | A linked local checkout; [linked installation](#install-or-refresh-a-linked-source) differs below. |

A trailing `/`, then one trailing `.git`, are removed for source identity: `crew`, `github:rimio-ai/rimz-crew`, and `https://github.com/rimio-ai/rimz-crew.git` are the same source. Git fetches the given URL, not that identity; shorthand expands to its HTTPS URL. Reinstalling another spelling of the same source replaces its fetch URL. In a shorthand, the first `@` starts the ref; in a URL it is the first `@` after the host part, so an authentication user is not mistaken for a ref. A ref may contain `/`. URL paths cannot take a `/<pack>` selector; use a shorthand for that.

A source without a selector installs its `default = true` packs, or all packs if none is marked default. A selector installs that pack. Installing more from the same source adds to its installed pack set and retains exclusions. A pack name owned by another source is refused with `rimz packs remove <pack>` as the fix.

Without `@<ref>`, official sources follow the running binary's `v<rimz-version>` tag; third-party sources follow the remote's default branch. An explicit ref pins the source, resolving a tag before a branch of the same name, then a full 40-hex commit ID. Reinstalling a branch pin resolves its current tip. A bare reinstall unpins the source. One source has one ref and commit: selecting another pack or changing a pin moves all its installed packs together. A reinstall whose new commit omits an installed pack refuses without changes and names `rimz packs remove <pack>` as the fix before retrying. If an official version tag is missing, the error names the tag, source, and default-branch install command.

Git installs resolve `requires` transitively in the same transaction. Any installed pack of the required name satisfies a requirement, including a linked or excluded one. A requirement from an already installed source uses its installed commit without fetching when that commit ships the pack. If the pack is absent at the installed commit and the Git source is unpinned, that source moves at its following rule in the same review and commit, retaining its other packs and exclusions and staying unpinned, and the packs it then ships have their requirements resolved at the new commit. Pinned and linked sources never move for a requirement. When an installed pinned source lacks the pack, the refusal names the reinstall command, which unpins the source, or pinning a ref that ships the pack. A source the same command fetched at its own ref names no fix, since that ref ships no such pack. A new official dependency of an official parent shares the parent's tag or branch name and pin, even when the parent was joined at its installed commit. A commit ID cannot carry across repositories, so its dependency follows its own unpinned rule, as do other new dependencies. Cycles end without duplicating packs. Removal never cascades to dependencies.

### Review and confirmation

Before activating a Git install, RimZ prints each added or moved pack's source, ref, short commit, pin, and reason (named or required by another pack). A move shows the previous version too. Its items are grouped by type, with exclusions, home overrides, conflicts, and failures named. Then each added or changed launchable agent, subagent, and team-seat profile appears once, including home profiles that inherit from a pack. The summary uses two complete definition loads, including staged skills, inheritance, and provider defaults; an unwritten mode on a Claude Markdown profile is `auto`.

Each profile shows its resolved provider kind and effective mode, isolation, tools, and allowed-tools. `[widened]` marks a higher mode (`plan < ask < auto < yolo`), looser isolation (`sandbox` to `scoped` to `host`), an added tool base or `Agent` child type, or any added allowed-tools rule. A changed kind is printed but never marked, so an update whose only change is a profile's kind applies without asking. The same fields can grant different capabilities under another provider (a Codex profile with `tools: [Read]` still edits the workspace through patches), so read the printed kind. Unset mode compares as `ask`, unset isolation as the machine setting, and unset tools as all tools. An empty `Agent` type list grants all types; restriction text on other tools is not a launch grant. Allowed-tools rules compare by text, so rewording a rule can mark widening. A new profile compares against all-unset grants; a removed profile never widens. Skill frontmatter allowed-tools is not parsed or compared. A run with no changed profile says so.

The summary always prints. An unchanged install needs no confirmation; otherwise a terminal asks `Install these packs?`. Answering no succeeds with nothing activated. Without a terminal, pass `--yes`; without it the summary prints, nothing installs, and the command exits 1. `--yes` skips only the question, not the summary. These definitions grant capabilities like home definitions; the summary is not a sandbox. See [security and trust](../../guide/security.md#what-rimz-changes-on-your-machine).

Fetching, staging, and confirmation do not hold the installation-record lock. If another pack mutation changes the record before commit, this install aborts with nothing changed and asks you to run it again. Git children are bounded to 120 seconds per command, have no terminal prompts or inherited repository-local Git variables, and do not fetch submodules.

## Update Git sources

`rimz packs update` checks installed sources in record order. Each source has its own transaction, also moving an unpinned dependency when its installed commit lacks a newly required pack. That dependency shares the review and commit; its later turn is already up to date. `update <PACK>` starts with the source containing that pack, moving all of that source's installed packs together and resolving its requirements the same way. Unpinned official sources follow the running binary's `v<version>` tag; unpinned third-party sources follow their remote default branch. Updates keep the installed pack selection and exclusions rather than adding newly marked defaults. If the new commit omits an installed pack, that source's update fails without changing its entry or record; the error names `rimz packs remove <pack>` as the fix before retrying.

[`rimz update`](./maintenance.md#update-rimz) also runs this step after updating RimZ.

Each changed source prints the same pack and effective-profile review as an install. Update reviews print a reason only for a pack newly pulled in by a requirement, not a `named` label for existing packs. An update with no widened profile grants applies without asking, including without a terminal. A widening update asks `Apply this widening update?` on a terminal. Declining holds that source and succeeds. Without a terminal, a widening update is held and the command exits 1; `--yes` applies it without asking but still prints the review. A held update names its packs, widened profile fields, and the command to review again. Holds are not stored: the next run recomputes them.

Pinned sources are skipped with their ref and the install command that follows updates again. For official and `github:` sources, that command selects an installed pack to avoid adding other defaults; a URL source keeps its bare URL. Linked sources are skipped with a reminder that edits show at once. An unchanged source reports `Already up to date:`. A failed or held source does not stop later sources; failures are reported after processing the others, alongside any host skill-link cleanup failure, and any failure or non-terminal hold exits 1. Successful earlier sources are not rolled back. Staging and consent remain outside the installation-record lock, and a record change before publication refuses that source's commit and requires a fresh review.

## Install or refresh a linked source

For a linked install, use an existing local source directory. The argument must be `.`, `..`, `~`, or start with `/`, `./`, `../`, or `~/`. Use `~/path` for a path under your home; quote it if you want RimZ rather than the shell to expand it. A `~user` spelling or bare relative path is not supported.

Every pack listed in the manifest is installed, regardless of `default`. The source is canonicalized and linked under the agents home's `packs/`; its contents are not copied. Definition, skill, and manifest edits are read on the next load without reinstalling. Newly added pack names require another `install` to enter the record. Refreshing a source adds and drops pack names to match the manifest and keeps exclusions for names still installed. An unchanged installation prints `Already up to date:` and succeeds.

```console
$ rimz packs install /home/me/src/rimz-crew
Installed crew from /home/me/src/rimz-crew (linked).
  Next: rimz packs show crew
```

Install refuses a missing directory, a missing or invalid manifest, a source equal to, inside, or containing the agents home, or a pack name already installed from another source. A name collision identifies its owner and the `rimz packs remove <pack>` fix. Load problems such as item conflicts are warnings after a successful install, not a rollback. Unknown manifest keys also warn.

Linked installs do not fetch, show a permission summary, or ask for confirmation; `--yes` is accepted but unnecessary. Unmet requirements are listed with install commands, not fetched. Definitions grant the same capabilities as home definitions; later checkout edits take effect too. See [the security model](../../guide/security.md#what-rimz-changes-on-your-machine).

## List installed packs

Bare `packs` and `packs list` are equivalent. Rows follow source installation order and pack order in the record. `LOADED` counts items whose fate is `loads`, not every shipped file or generated team seat. `PROBLEMS` is the pack's problem count, or `-`; source-attributed problems follow the table. An empty installation names `rimz packs install crew`. Listing succeeds even when it reports load problems. Git sources show their shortest reinstall spelling (`crew`, `skills`, `github:o/r`, or the URL), followed by `@<ref>`, the short commit, and `pinned` when pinned. Listing does not check the network for newer commits.

```console
$ rimz packs
PACK  LOADED  SOURCE                           PROBLEMS
crew       7  /home/me/src/rimz-crew (linked)  -
```

`rimz packs list`, `rimz packs show`, and `rimz packs diff` have [routine-command permissions](../agent-support.md#permission-modes). The bare `rimz packs` spelling and mutating verbs do not.

## Show a pack

`show` prints the description and source with the same version identity as `list`, then items grouped by type. It also lists exclusions for items no longer shipped. An installed but unusable pack can still be inspected; an unknown pack fails and lists installed names.

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
| `source`, `linked`, `entry` | Canonical local path or normalized Git URL, whether it is linked, and its entry name under `packs/`. |
| `url` | Git fetch URL as given, without the ref suffix and with shorthand expanded. Omitted for linked sources. |
| `ref`, `pinned`, `commit` | Git tag, branch or commit ID; whether explicitly pinned; and the full commit ID. Linked sources have null ref/commit and false pinned. |
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

## Customize an item

`customize <pack>:<name>` writes an override under the agents home, in the shipped item's tree and with its filename. Agents and subagents inherit by default: the generated Markdown has `agent: <pack>:<name>`, the shipped description, an explicit `name` when it differs from the filename, and no body. Shipped launch settings and craft keep following the pack; fields you add replace inherited fields and your body follows its prompt. A conflicted item can be customized through its qualified parent.

`--copy` writes the shipped bytes instead of inheriting. Teams, traits, kind bases, and skills require it. A skill copies the whole directory, including ancillary files and empty directories; symlinks and other non-regular entries refuse the copy. A failed definition cannot be inherited, but `--copy` can preserve its bytes for repair. Copies stop following changes to the copied file; its parent, trait, and skill references still resolve through the merged chains.

An already-overridden item refuses, naming its winning path and the `diff` and `reset` commands. An existing destination that claims a different name also refuses without replacing it. Excluded items refuse all three override verbs and name the item or whole-type `include` command. An ambiguous name requires `--type`, as with exclusions.

The receipt names the written path, its inheritance or copy mode, `rimz packs diff`, and, for definitions, `rimz agents validate`. After printing the receipt and any host skill-link cleanup lines, with both stdin and stdout on terminals, RimZ opens `$VISUAL`, otherwise `$EDITOR`, parsed as shell words with the path appended. With no editor, or without both terminals, it prints the path to edit. An invalid editor, spawn failure, or nonzero editor exit warns; the written override remains and the command succeeds. For a skill, the edit target is its `SKILL.md`. No default editor is chosen.

```sh
rimz packs customize crew:coder --type agents
rimz packs customize crew:review --type skills --copy
rimz packs diff crew:coder --type agents
rimz agents validate
```

## Diff an override

`diff` reads without changing files or pruning links. With no override it says so and names the shipped path. When the override's own `agent:` is this item's qualified key, it prints the whole override as an inheriting layer: its fields replace shipped settings and its body follows the shipped prompt. Other overrides get a unified diff from shipped to yours for each file whose bytes differ: `---` names the shipped path and `+++` the override, with `/dev/null` for a missing side. Skills compare the union of relative file paths, including added and removed files, and identical files print nothing; a symlink appears as `symlink to <target>` without reading its target, and other non-regular entries refuse the diff. Binary differences are identified without decoding their bytes as text. When no file differs, one line says the override at its path matches the shipped item. A configured-command override names its command and `config.toml`. Differences and matching overrides both exit 0.

## Reset an override

`reset <pack>:<name>` removes the winning override only when it is directly in the item's own home tree. A cross-tree definition or configured command refuses and names the path. When a linked home tree makes that entry resolve to, or contain, the shipped item itself, reset refuses and names both paths without removing anything. A home definition symlink or skill-directory symlink loses only the link, never its target. The command asks no confirmation: inspect with `diff` first. Each receipt names the removed path and the item's resulting fate when it remains in the reloaded inventory; an item with no override gets an `already` receipt.

```sh
rimz packs reset crew:coder --type agents
```

### Remove identical home copies

`reset --identical` removes only byte-identical home overrides. A definition compares as one file; a skill must have the same relative regular-file paths and bytes, including ancillary files. Empty directories do not affect equality; inner symlinks and other non-regular entries keep the skill. Differing overrides stay, each listed with its `rimz packs diff` command. The installation record is unchanged, and repeating the command removes nothing more unless files have changed.

With a bare pack, `--identical` is required. Optional `--type` limits the pass to one tree. A pack-wide pass keeps cross-tree and configured-command claims, linked-tree entries that resolve to shipped items, and overrides with read or comparison errors, each listed as `Kept <pack>:<name> (<type>): <reason>`. It continues with the other items, deciding shared home entries once. An item-specific reset still refuses unremovable paths and fails fast on errors. If a proven-identical entry cannot be removed, host skill-link cleanup still runs and completed receipts still print before the command exits 1 with `Pack reset incomplete`. Receipts use the reloaded inventory, so removing one shared home claim can expose a pack conflict rather than a loading item.

```sh
rimz packs reset crew --identical
rimz packs reset crew --type skills --identical
rimz packs reset crew:coder --type agents --identical
```

A successful Git or linked install reports how many removable overrides are identical and names `rimz packs reset <pack> --identical`. Installation never removes those files automatically. See [moving a home crew into a pack](../../guide/configuration.md#moving-a-home-crew-into-a-pack).

## Remove a pack and clean up links

`remove` stops loading one installed pack. If it was the source's last pack, RimZ also drops the source record and entry link. It never deletes a linked checkout. An owned Git checkout survives removal until a later installation-record mutation collects it. Dependencies are not removed. Reinstalling uses the usual selection rule (all packs for linked sources; defaults or a selector for Git).

```console
$ rimz packs remove crew
Removed crew from /home/me/src/rimz-crew.
$ rimz packs
No packs installed. Install a source: rimz packs install crew
```

Every successful mutating verb (`install`, `update`, `remove`, `exclude`, `include`, `customize`, and `reset`), including an already-applied change, prunes obsolete RimZ-owned host skill links across native provider roots and declared account homes. Receipts name removed links by root. Mutations add no links; a later host launch creates them. Foreign entries are untouched. Ownership and precedence are in [configuration](../../guide/configuration.md#skills).

Installation-record mutations (`install`, `update`, `remove`, `exclude`, and `include`) also repair pack entries and collect retired checkouts. `customize` and `reset` write only home overrides; they do not change the installation record or collect checkouts.

The record commits before entry cleanup. A repair or collection failure warns but does not prevent removal of a broken source. An entry-link cleanup failure still prints the removal receipt and a warning: the unrecorded entry is ignored. Incomplete host skill-link cleanup exits 1 with `host skill link cleanup is incomplete`; it does not undo a committed pack change. `rimz uninstall` removes owned library and pack skill links but keeps `packs/` in every mode.

## Manifest

`rimz-packs.toml` lives at the source root. Each pack directory can contain `agents/`, `subagents/`, `teams/`, `traits/`, and `skills/<name>/SKILL.md`, using the same item formats as the home trees.

```toml
[[pack]]
name = "crew"
description = "Local review crew"
path = "."
default = true
requires = ["skills/essentials"]
rimz = ">=0.4"
```

| Field | Contract |
| --- | --- |
| `name` | Required string, `[A-Za-z0-9_-]+`, unique in the manifest and across installed sources. |
| `description` | Required string, nonempty after trimming. |
| `path` | Required relative path to an existing directory inside the source, after both lexical normalization and symlink resolution. Paths must be unique, including canonical aliases. `.` selects the source root. |
| `default` | Optional boolean, default false. Selects the packs installed by a bare Git source; when none is true, all are selected. Does not filter linked installs. |
| `requires` | Optional array of sibling names, `crew/<pack>`, `skills/<pack>`, or `github:<owner>/<repo>/<pack>`, without refs or URLs. Git installs resolve them transitively; linked installs only report unmet requirements. Malformed entries warn and are skipped. |
| `rimz` | Optional Cargo version-requirement string, checked against the running RimZ package version (without its `+g<rev>` build suffix). An unsupported version warns and the pack still loads; malformed or blank requirements warn and are ignored. |

Requirements accept comma-separated comparators (`>=0.4, <0.6`), caret (`^`), tilde (`~`), exact (`=`), and wildcard (`*`) ranges. A bare `0.4` means `^0.4`: `>=0.4.0, <0.5.0`. A pre-release RimZ version matches only a requirement that explicitly names a compatible pre-release. An unsupported-version warning names `rimz packs update <pack>` for unpinned Git sources, the pin and an unpinning `rimz packs install <source>` command for pinned sources, or the linked checkout path to update. It appears at room entry, in `rimz packs list`, `rimz agents validate`, and install or update review. It does not fail validation or withhold the pack's definitions or skills. Doctor shows the room's recorded warning under MULTIPLEXER; a supervised entry into an existing room keeps its previous warnings until the next normal start or attach.

At least one `[[pack]]` is required. Missing required fields, wrong types, unsafe or duplicate names, escaping or duplicate paths, and missing directories refuse installation. Unknown keys at any level warn and otherwise load. A broken live manifest withholds its source's packs; an unusable pack directory withholds that pack while other valid recorded packs can still load. A pack skill read error withholds that pack's skills, not the library or other packs. A withheld healthy skill's row names the broken skill; the pack's problem carries the read error. Excluded and overridden skill copies are not read. A failed pack item's error names the `rimz packs exclude` command that sets it aside, whether it failed on a reserved launch name, an invalid team definition, a field its definition cannot take, or a `SKILL.md` it cannot read. Problems fail `rimz agents validate` and reach room-entry warnings and `rimz doctor`, without refusing unrelated launches. Home-only namespace reservations remain in force, including a home team `<x>-fast` when a pack supplies profile `x`; see [definition failures](../definitions.md#validation-and-failures).

## Layout and installation record

The agents home is `~/.rimz` by default. `RIMZ_HOME` moves it with the rest of RimZ; `RIMZ_AGENTS_HOME` overrides it for definitions, the skill library, and packs alone. `rimz paths` prints `agents home`; packs always live in its `packs/` child.

| Path under the agents home | Meaning |
| --- | --- |
| `packs/<entry>` | Stable symlink with an absolute canonical target for a linked directory, or a relative `.checkouts/<entry>-<commit>` target for an owned Git checkout. The first 128 characters of the source basename have characters outside `[A-Za-z0-9_-]` replaced by `-`, falling back to `source` if empty; recorded entry collisions add `-2`, `-3`, and so on. |
| `packs/.checkouts/<entry>-<commit>` | RimZ-owned Git checkout, including `.git`, named by the full commit ID. |
| `packs/.checkouts/<entry>-<commit>.lock` | Coordination file for the checkout's in-flight hold; garbage collection skips held checkouts, removes each collected checkout's lock, and collects unheld orphan locks. |
| `packs/installed.json` | Version-1 source identities, refs, pins, commits, installed pack names, and exclusions. |
| `packs/installed.lock` | Serializes record read-modify-write mutations. |

Edits inside a RimZ-owned Git checkout survive an update or reinstall to the same commit, because RimZ reuses that checkout. They are lost when the pack moves to another commit and the old checkout is collected. `rimz doctor` reports these edits in PACKS, including a moved HEAD; ignored files do not warn, and linked sources are not inspected. Copy changes worth keeping into the matching `agents/`, `subagents/`, `teams/`, `traits/`, or `skills/` tree under your agents home, where they override the pack. After preserving changes, restore a modified checkout with `git -C <checkout> reset --hard <recorded-commit> && git -C <checkout> clean -fd`. For an unreadable checkout, delete its checkout directory and run the reported `rimz packs install <source>` command (including `@<ref>` for a pin); reinstalling retains recorded packs and exclusions but recreates a checkout whose readiness marker is gone. A bare URL source also selects its manifest defaults as usual; the install review shows any added packs before confirmation.

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
      "pinned": false,
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

A Git source instead records its normalized identity in `source`, its fetch URL in `url`, `linked: false`, a ref, `pinned` (default false for older records), and a full 40-hex commit. Older Git records without `url` use `source` as their fetch URL. Linked records omit `url`. The record version stays 1; older binaries ignore `pinned` and `url` and can still load or unlink the entry.

A missing record is empty. Unknown record fields are ignored; a version above 1 withholds the whole pack tier with a newer-RimZ problem. The record is the commit point: install syncs a new Git tree and publishes the entry before the record, and removal rewrites the record before unlinking the final entry. Entries absent from the record never load and can be replaced by a later install. Mutations attempt to repair Git entries to the recorded commit before collecting unreferenced, unheld checkouts; removal warns on a maintenance failure and still commits. Both an entry's old target and the recorded checkout survive that repair pass. A checkout retired by publication lives until the next installation-record mutation, preserving a running scoped agent's mounted skills through that command, however many sources it moves; after a later collection, restart that agent. The durability discipline is in [store internals](../../internals/store.md#write-classes).

A linked checkout under `/tmp` or `/var/tmp` is hidden from scoped agents running `rimz` inside their mount view, because their temp unit replaces those paths. Test such a checkout from a host shell, or keep it outside those temporary trees for scoped use.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Listed or showed an installed pack, even with load problems; ran a diff, whether different, identical, or without an override; committed a mutation and completed host skill-link cleanup; declined an install or widening update on a terminal; or skipped pinned and linked update sources. Already-applied changes also succeed. An editor failure keeps the new override and succeeds. |
| `1` | Unknown pack or selector, missing item, an excluded override target, an already-overridden customize target, an existing customize destination, inheriting a failed item, a reset outside the item's own home tree, of a configured command, or through a linked home tree to the shipped item itself, invalid source or manifest, missing Git ref/tag, fetch failure or timeout, a changed record before commit, Git install without a terminal or `--yes`, a widening update held without a terminal or `--yes`, any failed update source (the others still apply), overlapping agents home, a pack name owned by another source, an `include` of one item whose type is excluded, record/I/O failure, or incomplete post-commit host skill-link cleanup. |
| `2` | Invalid CLI syntax, pack name, item type, or source spelling; empty or ambiguous item name; a copy-only customize target without `--copy`; a bare `reset <pack>` without `--identical`; or an exclusion request with neither item nor type. |
