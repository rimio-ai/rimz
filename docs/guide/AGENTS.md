# Guide writing contract

## The reader

One person: a strong developer who runs coding agents daily and knows their own stack to the bone (git, cron, tmux or Zellij, `claude -p`). They trust the workflow they have, so every page must give them a reason to change it.

## How a guide teaches

- **Open from what the reader already runs.** Start a guide, and each major feature inside it, from the tool the reader uses today: the stock CLI, the cron line, the hand-rolled `while true` loop, the plain `ssh`. Credit what that tool does well, name the gap it leaves, then present the RimZ command as the same contract without the gap.
- **State the why before any flag.** Name the problem the feature solves and the moment the reader reaches for it. A feature whose why does not survive one paragraph belongs in the reference.
- **Show the mechanism.** For every command that acts on the reader's machine, state exactly what it does, as steps they could run by hand: the flags it renders, the pane it opens, the file it writes and where. `fleet.md` ("The wrapper stays thin") and `loops.md` ("What a task does on your machine") are the model. Assurance without mechanism ("it's safe", "it just works") earns nothing.
- **Teardown gets the care creation gets.** A command that changes state documents what changes, where it lands, and the one command that reverses it.
- **Detail goes last.** Field tables, provider matrices, and edge-case caveats sit at the end of the page or one link away. A provider-specific exception gets one plain sentence. Internals vocabulary (event classes, wire names, module paths) stays in `docs/internals/`.

## Who owns what

Guides teach the stable, dogfooded surface: an early or still-shifting surface (agent plugins today) stays in `docs/reference/` until it hardens. A guide owns the topic its filename names, and no guide is named `agents.md`, which collides with the `AGENTS.md` contract files on case-insensitive filesystems. These rows draw the boundaries a filename does not show.

| Owner | Owns |
| --- | --- |
| `sidebar.md` | The attention model: the trust story, the zone map, the agent lifecycle, the jump loop, and the ranking. |
| `docs/interface/sidebar.md` | Every exact drawing: glyph meanings, worktree markers, receipts, budget bars, narrow-pane fallbacks, and the key and mouse tables. |
| `insight.md` | What every token and dollar figure means, how each is calculated, and the scopes and windows. |
| `configuration.md` | Every config key and its default (the `loop.toml` keys included), choosing and binding model tiers, the `~/.rimz` layout, and rebirth resume. |
| `theme.md` | Every `[theme]` key table, only because `docs/reference/` has no theming page: create one and the slot table, the display keys, and the glyph roles move there, leaving the guide the workflow. |
| `loops.md` | Scheduled work, plus `rimz shell` and `rimz watch` for agents and the hands-off reflexes (auto-continue, auto-redeem, idle compaction, smart compaction). |
| `workflows.md` | Workflow runs, except the `context.json` field table, which lives in `docs/reference/cli/workflow.md`. |
| `troubleshooting.md` | The symptom-to-fix mapping and nothing else. An entry states the cause in a sentence or two, names the command, and links the guide that owns the model. |
| `security.md` | The threat model and nothing else. |
| `docs/reference/` | Flag catalogs, field tables, exact output shapes, and per-agent matrices. |

## One term per concept

The left column is the word to use. A synonym for one of these is a bug, and a term never shifts meaning between pages.

| Term | Means |
| --- | --- |
| account | A provider login. |
| model tier | A capability level with an ordered list of models in machine config. |
| park | An agent or run held rather than progressing: a crossed cap, a provider rate limit, spend limit, or overload, or a wake that never arrived. A message parks when it is held for the recipient's next turn boundary, the opposite of `--steer`. |
| sleeping | An agent resting on a watch or a signal, including through a child. Never "parked". |
| reporting hooks | What `rimz hooks install` writes into an agent's own config. |
| the run | What `rimz agents -p` or a scheduled task starts. |
| channel, worktree | The two ways the fleet groups. Never "lane". |
| landed | Work a base branch already contains. |
| the floor | A minimum multiplexer version. |
| truecolor | One word, matching the `COLORTERM` value. |
| cell art | The sextant render tier. |
| handle | `@name`. |
| profile, kind base | The two definition shapes: `agents/<kind>.md` or `agents/<alias>.md` is a kind base, every other file under `agents/` is a profile. |
| permission mode | What an agent may do without asking. Never "posture", which the reference reserves for launch posture. |
| cohort | One live copy of a team. |
| the per-machine config | The `~/.rimz` TOML set. |
| project config | `<repo>/.rimz/config.toml`. |
| `~/.rimz/ws/<workspace-dir>/...` | A state directory. |

## Mechanics

On top of the [documentation mechanics](../AGENTS.md#mechanics):

- Every page opens with unheaded paragraphs under the H1, never a blockquote summary.
- A bullet may lead with a bolded full sentence (`**A new turn does.**`) in a list answering "which one" or "who does it". A bold label and a colon (`**Performance:** improved`) is banned.
- A guide ends in a `## See also` list, each link carrying the reason to follow it after a colon, unless it is a terminal page like `installation.md` or `security.md`.
