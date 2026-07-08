# Teams

A team pairs model strengths on one feature: one model plans, another codes, a third reviews, for better output at less cost. A named team makes that shape reusable. Define the roles once in `agents.toml`, then launch the whole set with one name, each member answering to its own role handle in a shared channel.

```sh
rimz agents forge -w feat-complex   # planner, coder, reviewer on one feature
rimz agents forge.reviewer          # re-add one role, same handle and channel
rimz agents forge --resume          # reopen the newest closed forge team
```

## Define a team

Define a team in `agents.toml`: bind each role to a profile, and give the team an optional `layout` in the same grammar inline specs use (commas split columns, plus signs tile rows, slashes stack them). Launching the team name opens every member in that layout, and each answers to its role handle: `forge.reviewer` across the workspace, or `@reviewer` inside the team's channel.

Rimz builds itself this way. The `forge` team it uses ships under [`examples/teams/`](../../examples/README.md): a Fable planner, a GPT coder, and an Opus reviewer laid out as `planner,coder+reviewer`, each role a profile with its own model, effort, and system-prompt file. Copy it, rename the roles, and it is yours. The team config shape is in [configuration → agent profiles, commands, and teams](../reference/configuration.md#agent-profiles-commands-and-teams).

```sh
rimz agents forge -w feat-complex   # open the whole team in its layout, one isolated worktree
rimz agents forge.reviewer          # re-add a single role to the running team
```

## Relaunch reconciles instead of duplicating

Point `rimz agents <team> -w <name>` at a worktree that already holds that team, and Rimz reads the state first: a live team focuses its tab, a closed team with work in progress offers to resume it, and a clean merged tree offers to remove it and start fresh.

`--resume` (alias `--continue`) forces the resume path, reopening the newest matching set of sessions: by team name and role for a team, or by cell order for an inline spec. Resume takes identity, working directory, and channel from Rimz's durable records, so it stands alone: no prompt, model, or channel flags ride with it.

```sh
rimz agents forge --resume         # reopen the newest closed forge team
rimz agents claude,codex --resume  # reopen the newest matching inline pair
rimz agents claude --resume        # resume the freshest closed Claude session
```

## One team, one line of work

The room treats a team as a single line of work: the sidebar keeps its members as one contiguous block with one derived state, so one member asking for you lifts the whole block ([the sidebar guide → Teams read as one](./sidebar.md#teams-read-as-one)).

## See also

- [Agents & worktrees](./agents.md) — launch agents by name, compose layouts, and isolate work in a Rimz-owned worktree.
- [Messaging](./messaging.md) — reach a role by handle: park, steer, schedule, and channels.
- [Configuration → profiles and teams](../reference/configuration.md#agent-profiles-commands-and-teams) — the `agents.toml` shape behind every profile and team.
- [Agent-control reference](../reference/cli/agents.md) — the complete `rimz agents`, `worktree`, and `gc` surface.
