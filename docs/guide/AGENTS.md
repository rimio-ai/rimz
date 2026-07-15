# Guide writing contract

Local contract for `docs/guide/`, the user guides. Extends the root [AGENTS.md](../../AGENTS.md); it never restates parent rules. It governs new guides and every edit to an existing one.

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

Each topic has one owning guide; every other page links there with a half-line of orientation instead of restating. When a section accumulates detail another page owns, move the detail and retarget every inbound link. Guides teach the stable, dogfooded surface: mechanics live in `docs/internals/`, flag catalogs in `docs/reference/`, and early or still-shifting surfaces (agent plugins today) stay in the reference until they harden.

## Mechanics

- Guide filenames are lowercase topic words. Never name a guide `agents.md`: it collides with the `AGENTS.md` contract files on case-insensitive filesystems and with the reference and internals files of that name.
- `sh` blocks are copy-runnable; `console` blocks carry output as the command actually printed it, captured, never invented.
- Every guide ends with a "See also" list, each link carrying the reason to follow it.
- A moved or reworded heading breaks inbound anchors silently. After changing one, grep for the old anchor across the repo and run `cargo xtask docs-links`, which validates file targets and `#anchors` together.
- A new guide is not done until [docs/README.md](../README.md) and the root documentation map list it; an unlinked page is invisible.
