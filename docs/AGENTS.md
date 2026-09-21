# Documentation

Local contract for `docs/`, and for the [README.md](../README.md), [DESIGN.md](../DESIGN.md), and [ARCHITECTURE.md](../ARCHITECTURE.md) that head it. Extends the root [AGENTS.md](../AGENTS.md). [guide/AGENTS.md](./guide/AGENTS.md) extends this one with the rules that govern the user guides alone.

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

Each fact has one owning page; every other page links to it with a half-line of orientation. When a section accumulates detail another page owns, move the detail and retarget every inbound link. Duplication is where docs rot first: this pass found the same claim wrong in a help string, a reference page, and the README, three places that had each copied it.

## Grounding a `console` block

A `console` block carries output as the command printed it. Never invent one, and never hand-edit a captured one. Two ways to ground it:

**Capture it.** To get output under a home that reads like a reader's, run the built binary inside a nested bubblewrap, which touches nothing on the machine:

```sh
bwrap --dev-bind / / --tmpfs /home --bind /tmp/scratchpad/<dir> /home/me \
  --setenv HOME /home/me --setenv RIMZ_HOME /home/me/.rimz --chdir /tmp \
  -- /tmp/scratchpad/rimz <args>
```

`home_relative` then prints `~/...` while the copy-paste lines print `/home/me/...`. Copy the binary out of `target/debug/` first: the tmpfs hides the worktree.

**Recompute it from the renderer.** `cli/render/mod.rs` is the authority. For a `render::Table`, column width is the widest cell including the header, columns join with two spaces, right-aligned columns are the ones named by `.right(&[...])`, a section emits a leading blank line, consecutive cards are separated by a blank line, card detail indents two, and the last column is padded only when it is right-aligned. For a `render::KeyVals`, the value column sits at `indent + longest_key + 2`. For a `render::roster::Roster`, the handle column pads to `handle.len() + 1` before the two-space separator, so `@planner` under a roster holding `@reviewer` carries four spaces, not three. Recompute, never eyeball.

A block for a command you cannot run may be derived from the error type's format string plus the `error: ` prefix `write_report` adds, as long as you say in the change that it was derived rather than captured.

## Facts that differ by surface

Two figures RimZ prints in more than one place, each correct where it appears. Name the surface you mean.

- `◇` is `↘` plus `↗` in the sidebar (`SpendWindow::add`) and adds `◌` in `rimz stats` (`SpendWindow::display_tokens`).
- A crossed cap reads `fleet budget: $50.21 of $50.00/day` on an agent card (`agents/state.rs::BudgetPark::label`, two decimals always) and `$50.21 of $50/day` in the cockpit and provider headline (`theme/fmt.rs::dollars_cap`, whole-dollar cents dropped).

## Mechanics

- Prose carries no em or en dashes. Use a period, comma, colon, or parentheses. A `console` block keeps whatever the command printed.
- `sh` blocks are copy-runnable, checked against the clap definition that parses them.
- A fact from a subagent's summary is not grounded. Use a subagent to find the function; read the function before writing the sentence.
- A moved or reworded heading breaks inbound anchors silently. After changing one, grep the repo for the old anchor and run `cargo xtask docs-links`, which validates file targets and `#anchors` together. `cargo xtask lint` does not cover Markdown.
- A new page is invisible until its index lists it: [docs/README.md](./README.md) for anything a user reads, the tree's own README for `reference/` and `internals/`, and the root [documentation map](../AGENTS.md#documentation-map).
