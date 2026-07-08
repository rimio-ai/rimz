# Channel CLI

`rimz channel` manages durable named cooperation lanes. Use it when a room needs a `#design`, `#ops`, or `#release` lane without creating a Git worktree.

```sh
rimz channel new <NAME>
rimz channel list [--json]
rimz channel rm <NAME>
```

```sh
rimz channel new design
rimz channel list
rimz channel rm design
rimz agents claude --channel design "Draft the API shape."
rimz message @planner#design --create "Plan the rollout."
rimz message @codex --channel design "Review after your current turn."
```

Names are bare ASCII letters, numbers, `_`, or `-`. They render and address as `#name`; do not include `#` in the command argument.

## Commands

`new <NAME>` validates and records the channel in the current room store, then best-effort opens a `#NAME` tab with `RIMZ_CHANNEL` stamped into the shell. The command works in repo, marker, and directory rooms.

`list` prints named channels, Rimz-owned worktree channels, and live derived channels in one table. The `BACKING` column is `named`, `worktree`, or `directory`; explicitly stamped named lanes stay `named`, and in-place team lanes appear as live directory lanes such as `#project/pcr`. The `AGENTS` column lists live handles. `--json` emits the same entries as objects with `channel`, `backing`, and `agents`.

`rm <NAME>` (alias `remove`) removes a named-channel record. If the same name is backed by a worktree, use `rimz worktree remove <NAME>` so Git cleanup and landed-work checks run.

## Launching into a channel

`rimz agents <SPEC> --channel <NAME>` registers the named channel when missing, launches in the room root, and opens a `#NAME` tab. [`rimz message`](./agents.md#message-an-agent) accepts `--channel <NAME>` as the flag form of an inline `#NAME` address in every send mode; `--create` uses it to launch a missing kind or profile into that lane.

`--channel` is distinct from `--worktree`: named channels are durable lane records, while worktrees are Git checkouts with cleanup policy. In-place team and directory lanes stay derived from live agents' stamped launch identity.
