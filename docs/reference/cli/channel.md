# Channel CLI

`rimz channel` creates, lists, and removes named channels: durable lanes such as `#design`, `#ops`, or `#release` that group and address agents without a Git checkout. A named channel is one record in the room store plus a `#NAME` tab. It creates no branch and touches no files, so creating and removing one is cheap. Reach for [`rimz worktree`](./worktree.md) instead when the agents need their own working tree. How channels scope addresses and render in the sidebar is the [messaging guide](../../guide/messaging.md#channels).

```sh
rimz channel new <NAME>
rimz channel list [--json]
rimz channel rm <NAME>
```

The [global flags](../cli.md#global-flags) apply to every verb.

## Channel names

A name is one or more ASCII letters, digits, `_`, or `-`. Output and addresses spell it `#design`, but every command argument takes the bare name: `rimz channel new '#design'` and `rimz agents claude --channel '#design'` are refused with `invalid channel name`.

Named channels and RimZ-owned worktrees share one namespace in a repository room, so a name belongs to one or the other. Creating a named channel over a worktree's name is refused with ``channel `NAME` is backed by a worktree``, and creating a worktree over a named channel's name is refused with ``channel `NAME` is a named channel``.

## Create a channel

```sh
rimz channel new design
```

`new` writes the record to the store of the room resolved from the current directory, prints `created design`, and opens a focused `#design` tab whose shell has `RIMZ_CHANNEL=design` set, so a bare address such as `@codex` typed there resolves within `#design`. It works in repository, marker, and directory rooms.

The tab opens only when the room's multiplexer session is running. Outside a running room, `new` still records the channel and skips the tab without a message. Running `new` for a channel that already exists keeps the original record, prints `created design` again, exits 0, and opens another tab.

## List channels

```sh
rimz channel list
```

```console
CHANNEL             BACKING    AGENTS
#fable-pause        worktree   @reviewer @coder @planner
#provider-accounts  worktree   @fixer
#rimz               directory  @fable @opus @brave-comet @lucid-atlas @tidy-spire
```

`list` prints one row per channel, sorted by name: every named-channel record, every RimZ-owned worktree in a repository room, and every lane where a live agent runs. `AGENTS` lists the live agents' handles, or `-` for an empty channel. A room with no channels prints the header alone.

`BACKING` says where the channel comes from:

| Value | Channel |
| --- | --- |
| `named` | A record from `rimz channel new` or a `--channel` launch. A live lane whose agents were launched with an explicit channel also reads `named` without a record, including a team launched in place (`#rimz/forge`); `rm` has no record to remove for such a lane. |
| `worktree` | A RimZ-owned worktree, named for its branch. It lists while empty; remove it with [`rimz worktree remove`](./worktree.md). |
| `directory` | Agents launched with no explicit channel, grouped by their working directory's name, such as `#rimz` for the room root. The row disappears when the last agent ends. |

`--json` prints the same rows as an array of objects, `[]` when there are none:

| Field | Value |
| --- | --- |
| `channel` | The name without `#`. |
| `backing` | `named`, `worktree`, or `directory`. |
| `agents` | Live handles, such as `"@coder"`; empty when none. |

## Remove a channel

```sh
rimz channel rm design
```

`rm` (alias `remove`) deletes the named-channel record and prints `removed design`. It closes no tab and stops no agent: agents still running in the lane stay there, and `list` keeps showing the lane while they run.

| Refusal | When |
| --- | --- |
| ``channel `NAME` is backed by a worktree; use `rimz worktree remove NAME` `` | No record has that name, and a RimZ-owned worktree does. `rimz worktree remove` runs the Git cleanup and landed-work checks. |
| ``no named channel `NAME` `` | No record has that name. |
| ``invalid channel name `NAME`; use ASCII letters, numbers, `_`, or `-` `` | The name breaks the [name rules](#channel-names). |

## Launch or send into a channel

Three commands launch agents into a named channel and create its record when it is missing:

| Command | Effect |
| --- | --- |
| [`rimz agents <SPEC> --channel <NAME>`](./agents.md#channel-worktree-and-placement) | Launches in the room root, in a `#NAME` tab. Needs a running room and an agent spec. |
| [`rimz teams <TEAM> --channel <NAME>`](./teams.md#launch-a-team) | Launches the team into the lane in the room root. |
| [`rimz message --channel <NAME> --create`](./message.md) | Launches a missing kind or profile into the lane with the text as its first prompt. Without `--create`, `--channel` only narrows which agents match, the same as an inline `@codex#NAME`. |

```sh
rimz agents claude --channel design "Draft the API shape."
rimz message @planner#design --create "Plan the rollout."
```

From a pane in a team's lane, a bare role name launches that team's role into the lane: in a `forge` cohort's lane, `rimz agents reviewer` means `rimz agents forge.reviewer`. The spec rules, including when a bare role is refused as ambiguous, are on [`rimz agents`](./agents.md#launch-a-layout).
