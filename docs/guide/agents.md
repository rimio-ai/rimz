# Agents, worktrees, and teams

`rimz agents` is the single launcher. One command opens a stock agent CLI in its own pane, arranges several agents into a layout, drops the whole layout into an isolated Git worktree, or launches a named team on a feature. Every agent it starts gets a handle you can message, a card in the sidebar, and a place in the room grouped by the worktree it lives in.

```sh
rimz agents claude                    # one agent, own pane, live card
rimz agents claude,codex -w feat-a    # two agents, side by side, isolated worktree
rimz agents forge -w feat-complex     # a named team: planner, coder, reviewer
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

## Work in an isolated worktree

`-w`/`--worktree` puts the whole layout in a Rimz-owned Git worktree: a fresh checkout on its own branch, isolated from your main tree, with its name backing the room channel. Reuse or create a named one with `-w feat-a`, or pass a bare `-w` for a generated name. Branch-style names work too — `--worktree=feat/great` creates branch `feat/great` and worktree `feat-great`.

```sh
rimz agents claude,codex -w feat-a           # two agents in one isolated worktree
rimz agents planner,coder+reviewer -w feat-b # a whole layout, isolated on its own branch
rimz agents codex --from-pr 42               # a worktree checked out from pull request 42
```

`--from-pr <number|url>` fetches a pull request head over your `origin` credentials and lands the layout in a `pr-<N>` worktree — pair it with `-w <NAME>` to choose the local name. A new worktree starts ready to run: a committed `.worktreeinclude` copies the untracked files an agent needs (`.env`, local config), and a `.worktreelink` symlinks the heavy shared directories (`node_modules`, `target`, `.venv`) so they are shared, not re-copied ([worktree.md → Seeded files](../internals/harness/worktrees.md#seeded-files-and-linked-directories)).

**The room groups panes by worktree.** Your main checkout plus two feature trees render as three groups in one room. Agents in the same worktree share files; agents in sibling worktrees each keep their own — that is the recommended pattern for several write-capable agents at once, and two write-capable agents in the same tree trigger a one-time advisory. Two `rimz agents claude "…" -w` launches race parallel attempts, each in its own fresh tree.

### Reclaim work, once it lands

Cleanup is supervised and proves work landed before removing anything. When a worktree agent finishes and its pane closes while the room stays live, Rimz runs the decision: a clean tree whose content has landed on its base branch is removed with its branch; a tree that is dirty, pending, or unproven is kept — behind a `keep / remove / shell` prompt when you are watching, kept on its own otherwise. "Landed" is measured against the trunk and recognizes merge, squash, and rebase shapes alike, so a merged feature is reclaimed and unmerged work is never lost. A clean interactive quit leaves an idle shell in the tree for `rimz gc` to sweep later.

```sh
rimz worktree list              # every Rimz-owned worktree, with its agents and dirty/landed marks
rimz worktree remove feat-a     # remove on demand; refuses a dirty or unlanded tree
rimz gc                         # sweep clean, landed, unoccupied worktrees left behind
```

`rimz worktree remove` refuses a dirty or unproven tree unless you pass `--force`. `rimz gc` sweeps only worktrees Rimz owns and no live pane or agent occupies, under the same landed proof. The full lifecycle, the ownership marker, and the landed-content check are in [worktrees.md](../internals/harness/worktrees.md#cleanup).

## Teams: models with roles

The best results come from pairing model strengths — one model plans, another codes, a third reviews — for better output at less cost. A named team makes that shape reusable. Define it in `agents.toml`: bind each role to a profile, and give the team an optional `layout` in the same grammar inline specs use. Launching the team name opens the whole team in its layout, each member answering to its role handle in the team's channel.

```sh
rimz agents forge -w feat-complex   # planner, coder, reviewer on one feature
rimz agents forge.reviewer          # re-add one role, same handle and channel
rimz agents forge -w feat-complex --resume   # reopen that exact worktree's team
```

Rimz builds itself this way. The `forge` team it uses ships under [`examples/teams/`](../../examples/README.md): a Fable planner, a GPT coder, and an Opus reviewer laid out as `planner,coder+reviewer`, each role a profile with its own model, effort, and system-prompt file. Copy it, rename the roles, and it is yours. The team config shape is in [configuration → agent profiles, commands, and teams](../reference/configuration.md#agent-profiles-commands-and-teams).

**Relaunch reconciles instead of duplicating.** Point `rimz agents <team> -w <name>` at a worktree that already holds that team, and Rimz reads the state first: a live team focuses its tab, a closed team with work in progress offers to resume it, and a clean merged tree offers to remove it and start fresh. `--resume` (alias `--continue`) forces the resume path — reopening the newest matching set of sessions, by team name and role or by cell order for an inline spec. Resume takes identity, cwd, and channel from Rimz's durable records, so it stands alone: no prompt, model, or channel flags ride with it.

```sh
rimz agents forge --resume       # reopen the newest closed forge team
rimz agents claude,codex --resume  # reopen the newest matching inline pair
rimz agents claude --resume        # resume the freshest closed Claude session
```

The room treats a team as one line of work: the sidebar keeps its members as one contiguous block with one derived state, so one member asking for you lifts the whole block ([the sidebar guide → Teams read as one](./sidebar.md#teams-read-as-one)).

## Handles, in brief

Every agent answers to a handle, and you have already used them: `@claude` names a kind, `@planner` a profile, `forge.reviewer` one role of a team. Rimz gives each bare launch a stable pet name too (`@swift-otter`), and `--name writer` pins your own (`@writer`). A `#channel` suffix scopes the handle to one channel — every worktree gets one, and named channels and teams have their own — defaulting to the one you are standing in.

```sh
rimz agents focus @claude-2#feat-a   # jump to a specific agent's pane
rimz agents show swift-otter         # its activity, context, placement, and transcript tail
```

That is enough to launch, re-add, and jump to agents. Routing text to them — parking at the turn boundary, steering the live turn, broadcasting to a channel — is the [Messaging guide](./messaging.md), which owns the full address grammar and delivery rules.

## See also

- [Messaging](./messaging.md) — reach agents by handle: park, steer, schedule, and channels.
- [The sidebar](./sidebar.md) — how the room reads the cards, worktrees, and teams you launch.
- [Scripting agents](./scripting.md) — the same launcher as a supervised, exit-coded run (`-p`).
- [Configuration → profiles and teams](../reference/configuration.md#agent-profiles-commands-and-teams) — the `agents.toml` shape behind every profile and team.
- [Agent-control reference](../reference/cli/agents.md) — the complete `rimz agents`, `worktree`, and `gc` surface.
- [Agent support](../reference/agent-support.md) — which agents Rimz drives and what each integration adds.
