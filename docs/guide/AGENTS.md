# Guide writing contract

Local contract for `docs/guide/`, the user guides. Extends [docs/AGENTS.md](../AGENTS.md), which carries the rules every documentation page follows: the reader of each tree, grounding a `console` block, and the mechanics. This file governs new guides and every edit to an existing one.

## The reader

Write for one person: a strong developer who found RimZ an hour ago. They run coding agents daily, know their own stack to the bone (git, cron, tmux or Zellij, `claude -p`), and know nothing about RimZ. They have workflows and tooling they trust, so every page must give them a reason to change anything, and they will not read source code to fill a gap a sentence left.

## Open from what the reader already runs

Start a guide, and each major feature inside it, from the tool the reader uses today: the stock CLI, the cron line, the hand-rolled `while true` loop, the plain `ssh`. Credit what that tool does well, name the gap it leaves, then present the RimZ command as the same contract without the gap. A delta from a known tool teaches faster than a feature list from zero; `scripting.md` opens from `claude -p` and `loops.md` from cron, and new guides keep that pattern.

## Every feature carries its why

Before any flag, state the problem the feature exists to solve and the moment the reader reaches for it. A feature whose why does not survive one paragraph belongs in the reference, not in a guide.

## Show the mechanism

RimZ wraps primitives thin, and a pro adopts only what they can predict. For every command that acts on the reader's machine, state exactly what it does, as steps they could run by hand: the flags it renders, the pane it opens, the file it writes and where. `fleet.md` ("The wrapper stays thin") and `loops.md` ("What a task does on your machine") are the model. Assurance without mechanism ("it's safe", "it just works") earns nothing.

The same transparency is the safety story: a command that changes state documents what changes, where it lands, and the one command that reverses it. Creation and teardown get equal care; the reader's fear lives at the end of the lifecycle, not the start.

## Main workflow first, detail last

A guide reads front to back as the daily path. Field tables, provider matrices, and edge-case caveats sit at the end of the page or one link away; the reader digs for a field when they need it, never on the way in. Provider-specific exceptions get one plain sentence; internals vocabulary (event classes, wire names, module paths) stays in `docs/internals/`.

## One home per fact

Guides teach the stable, dogfooded surface, and the table below says which guide owns what. Mechanics live in `docs/internals/`, flag catalogs in `docs/reference/`, and early or still-shifting surfaces (agent plugins today) stay in the reference until they harden.

## Who owns what

Each page owns the topics in its row. A fact from another row gets a link, never a second copy.

| Owner | Owns |
| --- | --- |
| `installation.md` | Getting RimZ and a multiplexer onto the machine, the version floors, and choosing a truecolor terminal and a Nerd Font. |
| `setup.md` | One run of `rimz setup`, question by question. |
| `sidebar.md` | The attention model: the trust story, the zone map, the agent lifecycle, the jump loop, and the ranking. |
| `docs/interface/sidebar.md` | Every exact drawing: glyph meanings, worktree markers, receipts, budget bars, narrow-pane fallbacks, and the key and mouse tables. |
| `insight.md` | What every token and dollar figure means, how each is calculated, and the scopes and windows. |
| `configuration.md` | Every config key and its default, the `~/.rimz` layout, and rebirth resume. |
| `theme.md` | Every `[theme]` key table. |
| `loops.md` | `loop.toml` and the four hands-off reflexes: auto-continue, auto-redeem, idle compaction, smart compaction. |
| `multiplexer.md` | Configuring Zellij and tmux, and what a room asserts on the session. |
| `troubleshooting.md` | The symptom-to-fix mapping and nothing else. |
| `security.md` | The threat model and nothing else. |
| `docs/reference/` | Flag catalogs, field tables, exact output shapes, and per-agent matrices. |

Two rows need a note. `theme.md` holds key tables only because `docs/reference/` has no theming page; create one and the slot table, the display keys, and the glyph roles move there, leaving the guide the workflow. A `troubleshooting.md` entry states the cause in a sentence or two, names the command, and links the guide that owns the model, so the model itself never migrates into the catalogue.

## One term per concept

The left column is the word to use. A synonym for one of these is a bug, and a term never shifts meaning between pages.

| Term | Means |
| --- | --- |
| account | A provider login. |
| park | An agent or run held rather than progressing: a crossed cap, a provider rate limit, spend limit, or overload, or a wake that never arrived. The sidebar's `⏸` covers the limit cases only. A message parks when it is held for the recipient's next turn boundary, the opposite of `--steer`. Sleeping is a separate state: an agent resting with a wait armed. |
| reporting hooks | What `rimz hooks install` writes into an agent's own config. |
| the run | What `rimz agents -p` or a scheduled task starts. |
| channel, worktree | The two ways the fleet groups. Never "lane". |
| landed | Work a base branch already contains. |
| the floor | A minimum multiplexer version. |
| truecolor | One word, matching the `COLORTERM` value. |
| cell art | The sextant render tier. |
| handle | `@name`. |
| profile, kind base | The two definition shapes: `agents/<kind>.md` is a kind base, every other file under `agents/` is a profile. |
| permission mode | What an agent may do without asking. Never "posture", which the reference reserves for launch posture. |
| cohort | One live copy of a team. |
| the per-machine config | The `~/.rimz` TOML set. |
| project config | `<repo>/.rimz/config.toml`. |

A state directory is written `~/.rimz/ws/<workspace-dir>/...`.

## Mechanics

On top of the [documentation mechanics](../AGENTS.md#mechanics):

- Every page opens with unheaded paragraphs under the H1, never a blockquote summary.
- A bullet list answering "which one" or "who does it" may lead each bullet with a bolded full sentence (`**A new turn does.**`). A bold label and a colon (`**Performance:** improved`) stays banned.
- Guide filenames are lowercase topic words. Never name a guide `agents.md`: it collides with the `AGENTS.md` contract files on case-insensitive filesystems and with the reference and internals files of that name.
- A guide that leaves the reader with an obvious next step ends in a `## See also` list, each link carrying the reason to follow it after a colon. Most do; a terminal page like `installation.md` or `security.md` reasonably stops instead.
