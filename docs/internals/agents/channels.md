# Agent channels

> See [DESIGN.md](../../../DESIGN.md) for the commitments this doc operationalizes. [harness.md](./harness.md) launches agents into channels and drives them by address; [worktree.md](./worktree.md) owns the Git worktree backing. This doc owns the channel model: labels, backings, durable named lanes, and recovery.

A channel is a cooperation lane inside one room. It is the identity the sidebar groups by, the suffix an address uses as `#channel`, and the tab name Rimz recovers on rebirth.

## Backings and labels

Four backings can produce a channel:

- **Named channel** — a durable bare name created by `rimz channel new design` or first use through `--channel design`; it carries `RIMZ_CHANNEL=design`.
- **Worktree channel** — a Rimz-owned Git worktree; the branch is the preferred label and the worktree name/path stay addressable aliases.
- **Team channel** — an in-place named team under one directory, labelled `<dir>/<team>` and carried by `RIMZ_TEAM`.
- **Directory channel** — the directory basename used when a live agent has no named, worktree, or team identity.

Label precedence is explicit named channel, then worktree branch, then `<dir>/<team>`, then directory basename. This single rule feeds target resolution, rendered handles, sidebar grouping, `agents list`, pane overlays, and recovery.

Sidebar pods keep identity and kind separate: a named-channel pod stores `label = design` and renders the channel hash glyph plus that bare name; a worktree pod stores the branch label and renders the branch or merge glyph; a non-repo room root stores the directory basename and renders no glyph.

Git isolation follows the agent's own resolved worktree, not the room tree. Hooks run `git rev-parse --show-toplevel` from the agent cwd at any depth, and a git-backed row contributes that toplevel as its grouping root. Directory rooms do not scan child repos; non-git agents at the room root or in non-git subdirs fold into the room's root pod, while a nested checkout that an agent is actually working in earns its own worktree pod.

## Named-channel registry

Named channels live in `channels.json` beside `workspace.json` in the workspace ledger. The record stores the bare name and creation time; writes hold the workspace lock and use temp-file-plus-rename.

The registry stores only named channels. Worktree channels use their `rimz-worktree.json` marker as durable truth, while team and directory channels derive from live launch identity. `rimz channel list` unions the registry, Rimz-owned worktrees, and live channels from the snapshot.

The sidebar remains presence-driven: a group appears when a pane is running in that channel. An empty named channel persists in `channels.json`, appears in `rimz channel list`, and reopens as an empty `#channel` tab on room rebirth. Named-channel records stay until `rimz channel rm`; `rimz gc` acts on worktrees only.

## Launch and address

`rimz agents <SPEC> --channel design` registers the channel if needed, stamps `RIMZ_CHANNEL`, opens a `#design` tab, and writes the channel into the launch event so the rollup survives hook timing and recovery. `rimz steer @planner#design --create -- "draft"` follows the same path for create-on-miss.

`--worktree` and `--channel` are separate launch intents. A worktree launch creates or reuses Git backing; a named-channel launch stays in the room root and records only the bare lane. Inline `#design` and `--channel design` reconcile through the same target parser, so mismatched channel names fail before delivery.

Commands run inside a named-channel tab inherit `RIMZ_CHANNEL`, so `@claude` scopes to that lane by default. Human shells in a bare directory room have no current channel and reach the whole room unless an address or flag supplies one.
