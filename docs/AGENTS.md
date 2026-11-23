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
- Prose carries no em or en dashes. Use a period, comma, colon, or parentheses.
- A doc states what the product does now. Words relative to a change ("unchanged", "now", "no longer") belong in the PR, not the page.
- A moved or reworded heading breaks inbound anchors silently: grep the repo for the old anchor and run `cargo xtask docs-links`, since `cargo xtask lint` does not cover Markdown.
- A new page is invisible until its index lists it: [docs/README.md](./README.md) for anything a user reads, the tree's own README for `reference/`, `internals/`, and `externals/`, and the [documentation map](#documentation-map) below, which names a subsystem folder but not the pages inside it.
- `AGENTS.md` and `CLAUDE.md` are one file per directory, joined by symlink. Edit `AGENTS.md`, and give a new one its `CLAUDE.md` link.
- A change that deletes or moves a page retargets each [CHANGELOG.md](../CHANGELOG.md) link to it (`rg -n -F '<page>.md' CHANGELOG.md`), the one edit a pull request makes there.

## Sentences about the code

- A sentence that restates the code is checked against each source it covers: a condition against its source line with every `&&` and `||` kept, the commands that trigger a behaviour against `rimz lsp callers` followed up to every CLI `run`, a path through several functions against each function `rimz lsp callees` lists from the entry, and a set it quantifies (`every`, `only`, `most`) member by member.
- Before adding a sentence to a paragraph, read the whole paragraph against the code it describes, and fix any statement there that the change makes wrong or that is wrong already.
- A fact from a subagent's summary is not grounded: read the function before writing the sentence.
- An `sh` block is copy-runnable, and every flag a block or a sentence tells the reader to pass is checked against the clap definition of the command it names. A procedure written or rewritten runs once end to end, on the setup it names, before hand-off. A recipe spread over several `sh` blocks names each file by the path an earlier block created.
- A `console` block carries output as the command printed it, never invented or hand-edited, captured as [Console blocks](./contributing/probes.md#console-blocks) describes.

## Sweeps

How to find the sentences a change made false.

- Grep also the verbs a removed mechanism is described by.
- A set that gains or loses a member is found by two existing members joined by `.*` in both orders, by the old count beside the set's noun, and, where a table documents it, by one member as a cell (``rg -n '^\| `lifted` \|'``).
- A renumbered list moves every citation by number (`check 13`). A page that gains a new kind of content updates its one-line summary on each page that links to it.
- A renamed identifier's stem is also grepped over test function names without `-w` (`rg -n 'fn <stem>_'`), since `rg -w` cannot match `fn agent_breakdown_folds_tail`.
- A changed argv is grepped with any separator between its tokens (`'attach.{1,4}--create'`): a test joins them with a tab or lists them as array elements.
- A change that adds a second input to an outcome an existing key decided greps the old key beside the outcome's verbs in both orders (`rg -n --no-ignore-dot -e 'mcps.{0,60}(choos|serv|decid|keep|require)' -e '(choos|serv|decid|keep|require).{0,60}mcps' docs`), not only the phrases the plan lists.
- Every `rg` names its path (`rg PATTERN .`): in an agent shell stdin is not a terminal, so a pathless `rg` reads stdin and hangs.
- A tree-wide sweep passes `--no-ignore-dot`, since the root `.ignore` skips `docs/externals/`, `examples/`, `supply-chain/`, and `CHANGELOG.md`, and `--hidden` or `.github` when workflows are in scope.
- Scan Markdown with `rg -o '.{0,80}PATTERN.{0,80}'`, never `cut -c`: a paragraph is one line.

## Documentation map

**Root**: [README.md](../README.md) (product entry), [docs/README.md](./README.md) (user documentation index), [DESIGN.md](../DESIGN.md), [ARCHITECTURE.md](../ARCHITECTURE.md), [CHANGELOG.md](../CHANGELOG.md).

**Guide**: `docs/guide/`, indexed by [docs/README.md](./README.md). Ship a user-visible behaviour change into the guide page that promises it, and get your own working knowledge from `docs/internals/` instead.

**Interface**: [sidebar.md](./interface/sidebar.md), the sidebar on screen: every drawing, rendered frames, and the glyph legend.

**Reference**: `docs/reference/`. [Its README](./reference/README.md) indexes the pages by topic, and [cli.md](./reference/cli.md) is the command map over a page per scene under `docs/reference/cli/`.

**Internals**: `docs/internals/`. [Its README](./internals/README.md) indexes the pages by area and by source tree. The three multi-doc subsystems (`agents/`, `harness/`, `sidebar/`) keep a folder; every other subsystem is one flat file.

**Externals**: `docs/externals/`, indexed by [its README](./externals/README.md): one page per agent adapter under `agent-adapter/` and per mux backend under `mux-adapter/`.

**Contributing**: `docs/contributing/`
- [rust-conventions.md](./contributing/rust-conventions.md): Rust shape, timestamps and clocks, the shape of each kind of test, and the dependency budget.
- [gates.md](./contributing/gates.md): the gate stack, run budgets, the pre-commit hook, test profiles and the test environment, architectural invariants, and the compatibility surface.
- [build-and-ci.md](./contributing/build-and-ci.md): the toolchain, compile cache, contributor installs, target pruning, CI and its image, and release packaging.
- [running-gates.md](./contributing/running-gates.md): the gates, the test runner, and timing comparisons in practice.
- [probes.md](./contributing/probes.md): hand-built probes, live checks, and `console` block captures.
- [integration-tests.md](./contributing/integration-tests.md): what a test in the integration suite needs, by kind.
- [flakes.md](./contributing/flakes.md): live test runs, reporting a failure, and how a flake is stressed, forced, read from CI, and traced.
- [cli-scenes.md](./contributing/cli-scenes.md): the scene grammar `rimz agents` and `rimz teams` share, the rules every human history list keeps, and what a change to rendered output checks.
- [agent-adapters.md](./contributing/agent-adapters.md): the built-in adapter integration playbook, and what a change to a shipped adapter also runs.
- [model-upgrades.md](./contributing/model-upgrades.md): what a new provider model needs.
- [dependabot-loop.md](./contributing/dependabot-loop.md): the worktree-isolated dependency repair loop.
- [atlas.md](./contributing/atlas.md): operating `cargo xtask atlas`, and the `refactor-target.toml` schema.
- [refactor-program.md](./contributing/refactor-program.md): the whole-repository refactor as rounds of passes; [refactor-ledger.md](./contributing/refactor-ledger.md) is its memory between passes and its current status.
- [sidebar-screenshots.md](./contributing/sidebar-screenshots.md): scenario playback and PNG capture for sidebar frames.
- [sandbox-rooms.md](./contributing/sandbox-rooms.md): holding a disposable room, its card, running commands as an agent, and host rooms for real provider turns.
- [sidebar-live-check.md](./contributing/sidebar-live-check.md): the live sidebar check before hand-off, card states, and kitty-graphics captures, with the scripts in `scripts/live-check/` it runs.
