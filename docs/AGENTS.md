# Documentation

Local contract for `docs/`, and for the [README.md](../README.md), [DESIGN.md](../DESIGN.md), and [ARCHITECTURE.md](../ARCHITECTURE.md) that head it. Extends the root [AGENTS.md](../AGENTS.md). [guide/AGENTS.md](./guide/AGENTS.md) extends this one for the user guides.

## Who each tree is for

Write for the reader of the tree you are in. A page that serves two of these serves neither.

| Tree | Reader | What the page owes them |
| --- | --- | --- |
| `guide/` | A developer who found RimZ an hour ago and will not read source to fill a gap. | The daily path, taught front to back, every feature carrying its why. |
| `interface/` | The same reader, at the screen. | Every drawing exactly as it renders. |
| `reference/` | Someone already running RimZ who needs one flag, field, or exit code. | Complete facts in a fixed order. Nothing taught. |
| `internals/` | Someone who reads the code. | The invariants, traps, and reasons the code cannot carry, anchored by path and symbol. |
| `externals/` | The same reader, at an upstream surface. | Protocol facts pinned to an upstream source URL. |
| `contributing/` | Someone about to change RimZ. | The workflow, the gates, and what a change must keep true. |

## One home per fact

Each fact has one owning page; every other page links to it with a half-line of orientation. When a section accumulates detail another page owns, move the detail and retarget every inbound link.

## Mechanics

- Each paragraph, bullet, and table row is one line, never hard-wrapped.
- Prose carries no em or en dashes. Use a period, comma, colon, or parentheses. A `console` block keeps whatever the command printed.
- A doc states what the product does now. Words relative to a change ("unchanged", "now", "no longer") belong in the PR, not the page.
- A `console` block carries output as the command printed it: never invented, never hand-edited.
- An `sh` block is copy-runnable. Every flag an `sh` block or a prose sentence tells the reader to pass is checked against the clap definition of the command it names.
- A fact from a subagent's summary is not grounded. Use a subagent to find the function; read the function before writing the sentence.
- Before adding a sentence to a paragraph, read the whole paragraph against the code it describes, and fix any statement there that the change makes wrong or that is wrong already.
- A moved or reworded heading breaks inbound anchors silently. After changing one, grep the repo for the old anchor and run `cargo xtask docs-links`, which validates file targets and `#anchors` in every tracked Markdown file. `cargo xtask lint` does not cover Markdown.
- A new page is invisible until its index lists it: [docs/README.md](./README.md) for anything a user reads, the tree's own README for `reference/`, `internals/`, and `externals/`, and the [documentation map](#documentation-map) below. A page inside a subsystem folder (an `internals/agents/` adapter page) is listed in its tree's README, not in that map, which names only the folder.
- A change that deletes or moves a page retargets each [CHANGELOG.md](../CHANGELOG.md) link to it (`rg -n -F '<page>.md' CHANGELOG.md`), the one edit a pull request makes there.

## Before a doc change

Before writing one of these, read the [change checklists](./contributing/change-checklists.md) section named beside it and do what it asks. No gate catches a skipped one.

- A `console` block written, or one cell of an existing block changed: [Console blocks](./contributing/change-checklists.md#console-blocks).
- A sentence that restates a code condition, lists the commands that trigger a behaviour, describes a path through several functions, groups several mechanisms under one property, states a process command line, gives a figure RimZ prints on more than one surface, cites a measurement or the cause of a measured change, or is rewritten to widen or narrow a list; a recipe spread over several `sh` blocks; or a procedure written or rewritten: [Doc sentences](./contributing/change-checklists.md#doc-sentences).
- A page that gains a new kind of content, a behaviour whose trigger moves, or a scan over Markdown: [Stale wording sweep](./contributing/change-checklists.md#stale-wording-sweep).

## Documentation map

**Root**: [README.md](../README.md) (product entry), [docs/README.md](./README.md) (user documentation index), [DESIGN.md](../DESIGN.md), [ARCHITECTURE.md](../ARCHITECTURE.md), [CHANGELOG.md](../CHANGELOG.md) (one section per git tag, generated from merged history ahead of the tag).

**Guide**: `docs/guide/` teaches the shipped product to people who never open the source; [docs/README.md](./README.md) indexes the pages. Ship a user-visible behaviour change into the guide page that promises it, and get your own working knowledge from `docs/internals/` instead.

**Interface**: [sidebar.md](./interface/sidebar.md), the sidebar on screen: every drawing, rendered frames, and the glyph legend.

**Reference**: `docs/reference/` answers a specific flag or field; [its README](./reference/README.md) indexes the pages by topic, and [cli.md](./reference/cli.md) is the command map over a page per scene under `docs/reference/cli/`.

**Internals**: `docs/internals/` documents each subsystem for people who read the code; [its README](./internals/README.md) indexes the pages by area and by source tree. The three multi-doc subsystems (`agents/`, `harness/`, `sidebar/`) keep a folder; every other subsystem is one flat file.

**Externals**: `docs/externals/`, upstream protocol references pinned to source URLs, indexed by [its README](./externals/README.md): one page per agent adapter under `agent-adapter/` and per mux backend under `mux-adapter/`.

**Contributing**: `docs/contributing/`
- [rust-conventions.md](./contributing/rust-conventions.md): Rust shape, test taxonomy, toolchain, quality gates, CI, and release packaging.
- [change-checklists.md](./contributing/change-checklists.md): what a change of a given kind lists, greps, and pins before hand-off, doc sentences and console blocks included.
- [running-gates.md](./contributing/running-gates.md): the gates and the test runner in practice.
- [probes.md](./contributing/probes.md): hand-built probes and live checks.
- [integration-tests.md](./contributing/integration-tests.md): what a test in the integration suite needs by kind, and how a flake there is stressed, forced, and traced.
- [cli-scenes.md](./contributing/cli-scenes.md): the scene grammar `rimz agents` and `rimz teams` share, and the rules every human history list keeps.
- [agent-adapters.md](./contributing/agent-adapters.md): the built-in adapter integration playbook, and what a change to a shipped adapter also runs.
- [model-upgrades.md](./contributing/model-upgrades.md): what a new provider model needs.
- [dependabot-loop.md](./contributing/dependabot-loop.md): the worktree-isolated dependency repair loop.
- [atlas.md](./contributing/atlas.md): operating `cargo xtask atlas`, and the `refactor-target.toml` schema.
- [refactor-program.md](./contributing/refactor-program.md): the whole-repository refactor as rounds of passes; [refactor-ledger.md](./contributing/refactor-ledger.md) is its memory between passes and its current status.
- [sidebar-screenshots.md](./contributing/sidebar-screenshots.md): scenario playback and PNG capture for sidebar frames.
- [sidebar-live-check.md](./contributing/sidebar-live-check.md): live sidebar checks in disposable tmux and Zellij rooms, with frame and kitty-graphics captures.
