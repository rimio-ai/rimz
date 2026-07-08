# Agents

`rimz agents` is the single launcher: one command opens a stock agent CLI in its own pane, or arranges several agents into a layout. Every agent it starts gets a handle you can message, a card in the sidebar, and a place in the room. To run a layout in isolation, drop it into a [worktree](./worktrees.md); to pair models by role on one feature, launch a [team](./teams.md).

```sh
rimz agents claude            # one agent, own pane, live card
rimz agents claude,codex      # two agents, side by side in one room
rimz agents claude-plan       # a permission mode as a suffix
```

## Launch an agent by name

A bare kind opens that agent's stock CLI in its own pane, reporting to the sidebar from its first line.

```sh
rimz agents claude          # the stock Claude CLI, its own pane, live card
rimz agents codex           # same for Codex
rimz agents peer            # the built-in team: claude,codex side by side
```

**A suffix sets the permission mode**, rendered into each provider's own flags:

| Suffix | What it does |
| --- | --- |
| `-auto` | the provider's auto-accept mode for routine actions |
| `-ask` | keep the provider's native permission prompts |
| `-plan` | start in plan mode |
| `-yolo` | pass the provider's bypass flag, skipping its prompts |
| `-ping` | open at lowest effort, to keep the provider's budget window warm |

The built-in set is `claude-{auto,ask,plan,yolo,ping}`, `codex-{auto,ask,plan,yolo,ping}`, and `pi-{ask,plan}`; a mode a given provider has no equivalent for keeps that provider's default behavior. On the command line the same choice is `--ask` or `--yolo`. The exact flag each mode becomes, per provider, is in [agent support](../reference/agent-support.md).

```sh
rimz agents codex-yolo      # launch straight through provider prompts
rimz agents claude-plan     # start in plan mode
rimz agents claude --yolo   # the same mode as a flag
```

**A profile is your named preset.** Define one in `agents.toml` with its own model, reasoning effort, system prompt, and launch args, then launch it by name. A profile launches under its own handle, so `rimz agents planner` appears as `@planner`. The shared launch flags `--model`, `--effort`, `--system-prompt-file`, and `--append-system-prompt-file` override a profile for one launch. Effort levels are provider-specific: Claude `low|medium|high|xhigh|max`, Codex `minimal|low|medium|high|xhigh`, Pi `off|minimal|low|medium|high|xhigh`. The profile shape lives in [configuration → agent profiles, commands, and teams](../reference/configuration.md#agent-profiles-commands-and-teams).

```sh
rimz agents planner                                  # your profile: model, effort, prompt, args
rimz agents claude --model opus --effort xhigh       # override for one launch
```

## Compose a layout

One spec describes the shape of a whole room. **Commas split columns, plus signs tile rows, slashes stack rows** (a Zellij stack; tmux tiles them). Each cell is an agent kind, a `<kind>-<mode>` cell, a profile, a configured command, or `term` for a plain shell. An optional trailing prompt broadcasts to every agent cell in the layout.

```sh
rimz agents claude,codex                     # two agents, side by side
rimz agents claude,codex+term                # Claude | Codex tiled over a shell
rimz agents claude/codex/term                # one stack of three rows
rimz agents 'vim,codex+term'                 # your editor beside an agent stacked over a shell
rimz agents claude,codex "Draft the API shape."   # one prompt to both agents
```

Quote a spec whenever it contains a `+`, a space, or anything your shell would otherwise expand. Profiles and kinds compose the same way, so `rimz agents planner,coder+reviewer` lays out three of your presets. The full grammar and how cells compile to panes is [harness.md → The layout IR](../internals/harness/harness.md#the-layout-ir).

Add `-w` and the whole layout lands in an isolated Git worktree, the pattern for running several agents in parallel without stepping on each other: see [Worktrees](./worktrees.md).

## Handles, in brief

Every agent answers to a handle, and you have already used them: `@claude` names a kind, `@planner` a profile, `forge.reviewer` one role of a team. Rimz gives each bare launch a stable pet name too (`@swift-otter`), and `--name writer` pins your own (`@writer`). A `#channel` suffix scopes the handle to one channel — every worktree gets one, and named channels and teams have their own — defaulting to the one you are standing in.

```sh
rimz agents focus @claude-2#feat-a   # jump to a specific agent's pane
rimz agents show swift-otter         # its activity, context, placement, and transcript tail
```

That is enough to launch, re-add, and jump to agents. Routing text to them — parking at the turn boundary, steering the live turn, broadcasting to a channel — is the [Messaging guide](./messaging.md), which owns the full address grammar and delivery rules.

## See also

- [Worktrees](./worktrees.md) — isolate a layout on its own branch so several agents run in parallel.
- [Teams](./teams.md) — pair models by role and launch, reopen, and resume the whole set as one unit.
- [Messaging](./messaging.md) — reach agents by handle: park, steer, schedule, and channels.
- [The sidebar](./sidebar.md) — how the room reads the cards, worktrees, and teams you launch.
- [Scripting agents](./scripting.md) — the same launcher as a supervised, exit-coded run (`-p`).
- [Configuration → profiles and teams](../reference/configuration.md#agent-profiles-commands-and-teams) — the `agents.toml` shape behind every profile and team.
- [Agent-control reference](../reference/cli/agents.md) — the complete `rimz agents`, `worktree`, and `gc` surface.
- [Agent support](../reference/agent-support.md) — which agents Rimz drives and what each integration adds.
