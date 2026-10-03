# Refactor program: improving the whole repository in passes

The repository is too large to refactor in one review, so the work runs as bounded passes, each an architecture review that ends in a landed, proven change or a recorded `holds`. Passes run in rounds: a picker chooses a round's passes with the user and hands each to a team, and the team runs its pass end to end and opens a PR without further questions. This page is the manual for both roles. The instrument is [`cargo xtask atlas`](./atlas.md), the code shape is [rust-conventions.md](./rust-conventions.md), and the state between passes is the [ledger](./refactor-ledger.md). The code is the result; commits and PRs are the history.

Nobody carries memory from one pass to the next. What must survive lives in three committed places: the ledger (status, module verdicts, admission intents, open deferrals), the `[[verdict]]` rows in `refactor-target.toml` (shape and guard verdicts hide their families on the next survey; item verdicts are shown by `inspect`), and the ceilings `conform --tighten` lowers. A pass that ends without writing to the ones that apply has not ended.

## The method

Each pass asks of its scope: re-implemented from scratch, knowing what the code does for its callers, what would be written? Every gap between that target and the code is a candidate tagged with the verb that closes it: `collapse` (two implementations, one wins), `delete` (vestigial weight), `deepen` (callers assemble what the module should hide), or `rehome` (knowledge or a seam in the wrong place). Real behaviour is the requirement: a strange corner is deletable only after `git log -S` and `inspect --item` show it pins nothing.

Two tests decide most candidates. Deletion test: remove the module in your head; if the complexity vanishes it was a pass-through, if it reappears across callers it earns its keep. Call-site test: write the ideal call in a line or two; the delta to the real call site is what the interface failed to hide. A candidate must leave fewer files, flags, abstractions or options, or it is dropped; a [rehome out of an edge layer](#edge-layers) is the one exception, since the entry stays and its owner may be new. Finding nothing is a real outcome, recorded as `holds`; never inflate a ranking.

Every pass is behaviour-preserving: wire formats and stored data keep their shape, and a bug found on the way is reported in the PR, not fixed.

## Pass kinds

| kind | picked from | contract `kind` and SLOC ceiling | proof rows | success |
| --- | --- | --- | --- | --- |
| seam | `debt`, module-cycle and shape-family rows; the ledger's seam queue; an edge layer's workflow hotspots | `seam`, flat | `[[dependency]]`, `[[rehome]]` | the direction is closed and `conform` keeps it closed, or the logic sits with its owner, unit-tested there |
| module | `reopen` and unheld rank rows, ripe deferrals | `module`, negative | `[[esc]]`, `[[assembly]]`, `[[delete]]` | fewer escaping items, lighter callers, less code |
| complexity | a user decision naming the hotspots | `module`, negative | `[[cx]]` beside the module rows | lower cx on the named functions, or an item verdict recording it as inherent |
| tooling | a user decision | `tooling`, positive, priced from the target | optional | the capability works and its docs say how to use it |

A flat ceiling is `0`, or a small positive with the reason in the commit. A module pass that only narrows may take a small positive ceiling with an `[[esc]]` row below the measured base, since `pub(crate)` lengthens signatures ([caveat 18](./atlas.md#caveats)). Small neighbours in one layer bundle into one pass with one contract. The schema of every row is in [atlas.md](./atlas.md#pass-contract-v2).

### Seam passes

- The contract lists every module on both sides in `paths` and proves each edge with a `[[dependency]]` row at its target `max-sites` (`0` for a closed direction) beside `[[rehome]]` rows for what moves.
- Its target states the direction in one sentence, copied into the module's `AGENTS.md` and into `refactor-target.toml` as a lowered admission.
- A same-layer seam has no admission to lower: close it by lowering one side's layer or setting an `allowed-dependencies` list.
- A sibling-family collapse names the winning member and quotes the divergences that must survive (those `git log -S` traces to a fix). Build the family dossier by running `inspect --module` on each sibling with the same `--section` set.
- Two contracts on one branch from one base each list the union of changed paths, Markdown and TOML included; each contract's proof is its own rows.

### Complexity passes

A complexity pass reopens modules that hold a verdict because the user chose to lower their cx, not because of friction. It may contradict earlier `[[verdict]]` rows and ledger notes with evidence, and says so in the PR and the new rows. `git log -S`/`-L` each branch before folding it and pin its corner case at the interface first. A single-use extraction that deletes nothing is not a reduction, since any split lowers cx ([caveat 17](./atlas.md#caveats)): price a fold from the raw `cyc`/`cog`/`sloc` columns of `survey --section hot`, and record a residual that is a protocol state machine or a table of fix-pinned cases as a `[[verdict]]` item row.

### Edge layers

An edge layer holds only what its `AGENTS.md` admits, and a seam pass rehomes the rest to its owner. `cli` is the one edge layer today: it holds argv parsing, the process edge (tty, signals, exit codes, stdout discipline) and presentation, under the [CLI layer contract](../../crates/rimz/src/cli/AGENTS.md).

- A workflow qualifies to move when several commands use it, it holds decisions worth unit tests, or it carries a product invariant. A straight-line sequence of library calls stays in the edge. Glue with no owner gets a new library module, never a module inside the edge.
- Prove a function that leaves whole with an item `[[rehome]]`, and decisions that leave an entry that stays with a [logic `[[rehome]]`](./atlas.md#logic-rehome) from the edge module to the owner. For edge-to-edge edges that close, add `[[dependency]]` rows with leaf `from` modules, and tighten the edge's surface budgets the pass lowers.
- Give each workflow a small library interface: one typed request in and one typed outcome out, capped by an `[[esc]]` row on the owner, with an `[[assembly]]` row where the edge function wired several modules. Unit-test the moved logic in the owner without spawning a process.
- Rendering stays in the edge; where a renderer decides things, split it into a view model and a renderer that only draws.
- A pass that edits the cross-command list in the CLI layer contract maps every `cli`-to-`cli` import `atlas conform` reports to a named entry before committing.

## Running a round (the picker)

1. Fetch trunk and confirm the previous round's `docs(refactor)` record commits landed (`git log --oneline -8 -- docs/contributing/refactor-ledger.md`).
2. Survey into a directory every team can read:

   ```sh
   cargo xtask atlas survey --out /var/tmp/refactor-round/survey.md
   cargo xtask atlas survey --section rank --top 400 --out /var/tmp/refactor-round/rank.md
   cargo xtask atlas survey --by depth --section rank --out /var/tmp/refactor-round/depth.md
   cargo xtask atlas survey --section hot --top 40 --path crates/rimz/src/cli --out /var/tmp/refactor-round/hot-cli.md
   ```

3. Read the footer first. `ledger problem` lines (an unresolved verdict SHA leaves its module unheld), `ledger restamp` mappings and `stale verdict keys` are record defects; a round opens by fixing them (`cargo xtask atlas survey --restamp` writes the mappings).
4. Pick in this order: record defects, a queued seam, `reopen` flags and unheld rows (`rg -v 'held|bin'` over the rank), ripe open deferrals folded into the pass that owns their path, then the kinds the user chose for the round. The rank is code × churn and dominated by size, so read it beside the depth sort. Candidate signals: shallow depth with many escaping items, `pin` or `thin`, an `assemblers` row wiring several modules, cx in a few functions. `holds` signals: high depth, `t/c` above 1, one facade. `hot` is information, not a gate: a module under heavy feature work is still picked, and its conflicts are resolved at merge. `cli/*` modules are `bin` and are picked only for a rehome out of the [edge layer](#edge-layers); for every other kind, their friction is fixed by a pass on the provider.
5. Gather evidence per pick: `inspect --module <full path> --brief --out …`, `--item` per moving item, `--from` per assembler. Before proposing a move, `git log --oneline -- <source file>` for an earlier move between the same two modules. Quote any ledger row a pick cites, found by `rg` on its exact spelling. The design is the team's; the picker gathers facts and suggests.
6. Take the pick, the evidence and the open decisions to the user in one question, recommended shape first.
7. Write one note per pass: goal and the user's decisions, base SHA, findings anchored to path and symbol, suggested contract shape, the other passes' `paths` in the round, and the [Ending a pass](#ending-a-pass) checklist. For a complexity pass, list each named hotspot's callers with the arguments that gate its branches. Number passes from the ledger and from open PRs (`gh pr list --search 'pass <N>'`): a pass in flight holds its number before the ledger records it.
8. Launch one team per pass: `spot` (coder, reviewer) when the note states the edit, `recon` (scout, planner, coder, reviewer) otherwise.

## Running a pass (the team)

Read the note, the ledger, `refactor-target.toml` (layers, admissions touching the scope, stranglers, `[[verdict]]` rows: a verdict is a decision, reopened only with real friction or by the round's kind, and marked as contradicting the record), then the scope's `AGENTS.md` and internals page. The team does not ask the user: every judgment call goes in the PR under Decisions, and intent the code and history do not reveal is decided with the recommendation written down.

**Learn.** `inspect --module <target> --brief --out` is the dossier; fan the other touched modules out to one subagent each. `--item` on every candidate before calling it deletable, `--from <caller>` for each assembler. For a data module, a caller naming five enums is use, not assembly; judge by the functions and builders it wires.

**Rethink.** Write the target concretely enough to diff against: each module, what it hides, and everything in the current code with no counterpart. Design it twice, the second time in a fresh context given only the dossiers and call sites and "the smallest interface that serves these callers"; keep the winner on depth, locality and seam placement. Each candidate carries its verb, line arithmetic, files and moving tests; one that cannot become per-file steps is not learned yet. A change to something the pass cannot edit (other repos, users' config, stored data, schemas, wire formats) is a risk finding, never planned.

**Plan.** One self-standing ordered plan: title, target, contract, line budget, prerequisite pin tests (the first commit, green on base), tests that move, per-file steps led by their verb, verification commands.

- Derive the tests that move mechanically: `rg -n '<symbol>' crates/rimz/src crates/rimz/tests crates/rimz/benches` for every symbol a `[[delete]]`, narrowing or signature change names; list every hit outside the owning module.
- Quote every signature, derive and literal from its declaration line, not a subagent report; read each pin's expected value from the code path.
- Price the budget from the written target: each new signature in rustfmt form, each error boundary a shared helper crosses (two error types is a generic or a new enum), an import line per file naming old and new homes after a no-shim move, and header, imports and `mod` lines per new file. Measure the largest move on a spike; a sandboxed spike that cannot write `.git` uses `git clone --no-hardlinks` and its own `CARGO_TARGET_DIR`.
- A narrowing is a candidate only when the compiler accepts it and no reader outside the library target needs it ([caveats 13 and 19](./atlas.md#caveats)). Count `esc` from `inspect --json` items plus `pub use` names.
- Before deleting a legacy deserialization shape, list every durable host of the type with `rimz lsp refs <Type> --all` and name, per host, the version check or boundary that keeps a pre-migration record out; a host with neither blocks the deletion.
- Write the contract with `base` computed by `git merge-base HEAD origin/main` at the moment you write it, never copied from notes, and every edited path in `paths`. Run `cargo xtask atlas diff --expect <draft>` on the clean base: every row must resolve, and the printed base counts are the ceilings' starting figures. An `[[esc]]` ceiling is the base count minus the items the pass removes or narrows plus the items it adds; a new public method counts. Draft `max-production-sloc-delta` as a direction (zero or a small allowance) and fix the number only after the first caller migration compiles and `atlas diff --expect` has measured it: a struct-literal request type costs one line per field per call site under rustfmt. Read the code that enforces each row kind (`xtask/src/atlas/contract.rs`, `diff.rs`, `conform.rs`) and the [caveats](./atlas.md#caveats) before locking it.

The contract goes in the plan verbatim, at a scratch path outside the worktree:

```toml
version = 2
base = "<merge-base SHA>"
paths = ["crates/rimz/src/message", "crates/rimz/src/cli/agents_cmd"]
max-production-sloc-delta = -60

[[esc]]
path = "crates/rimz/src/message"
max = 120

[[assembly]]
from = "cli::agents_cmd::idle_compact"
to = "message"
max-items = 3
```

`paths` names every module the pass edits, callers included; `diff --expect` fails on a change outside them, which keeps the pass reviewable.

**Execute.** In a worktree on a branch named for the pass: pins first, then the steps in plan order, one commit per step or bundled module. `cargo xtask check` while iterating, `cargo xtask gate` before hand-off, higher tiers per [AGENTS.md → Testing](../../AGENTS.md#testing); a pass touching `benches/` or a walker's `pub` wire also runs `cargo xtask perf -- --no-run`. `cargo xtask atlas conform --ratchet` passes at every commit; take a surface-budget change only from the block `--ratchet` prints when it fails, and add its admission comment only when the tightened budget still sits above base. Rules the compiler and gates enforce:

- A narrowing turns rustdoc links it made private into plain backticks in the same commit (`cargo xtask doc`).
- An item whose surviving readers are all unit tests is spelled `#[cfg(test)] pub(crate)` (precedent `config.rs`, `parse_scheme_text`), since `lint` builds without `cfg(test)`.
- A module lifted below its old home is a leaf in that same commit: an upward `use` or qualified path fails the ratchet.

**Tests.** Behaviour is pinned at the interface before internals move; a module whose escaping items have no outside test site (`pins` empty, `t/c` low) pays this in full. Tests reaching past a narrowed reach move with it: rewritten when the assertion matters, deleted with the internals when not; the plan names each one. A pin is proven before the steps build on it: break the pinned behaviour with a one-line edit, watch the pin fail, restore. The PR lists each mutation with the pin it failed.

**Prove.** On a tree that passed `gate`, `cargo xtask atlas diff --expect <contract>` exits zero, or the drift is fixed or the contract loosened with the reason in the commit. A lint fix after `diff` reindexes, so prove again. `[[dependency]]` rows prove a closed direction; a grep is a convenience and must not count rustdoc links or inline `#[cfg(test)]` sites. Then `cargo xtask atlas conform --tighten --only <owned-path>` per owned path.

## Ending a pass

After the final rebase, set the contract's `base` to the branch's new merge base and run `atlas diff --expect` again; trunk commits otherwise count as changed paths.

The last commit, `docs(refactor): record pass N`, carries the durable state, every number measured on the tree it ships:

1. A ledger verdict row for every library module reviewed, at the survey's exact spelling: `holds`, or `holds; landed pass-N` when the interior was rethought; a seam pass that only reviewed edges writes bare `landed pass-N`. Stamp the post-rebase source tip (the record commit's parent); after a merge rewrites it, `survey --restamp` maps it to the trunk commit that wrote the row. The reopen count defaults to 30. The note is one clause naming what holds. Binary (`bin`) modules receive no ledger row: record them through `[[verdict]]` item rows.
2. `[[verdict]]` rows in `refactor-target.toml` for every item, family or pass-through judged and kept, with the reason; re-key rows the survey lists as stale. A key is stale only on the tree that records it: run `cargo xtask atlas survey` on the tip before deleting or re-keying a row, since a family can cross the gate again under the same key while the pass is in flight.
3. An admission intent row for every upward edge reviewed: `keep` with the reason, or `close` naming the closing pass. A closed edge loses its admission and its row.
4. An open deferral line for a refactor candidate judged real but not landable, with the concrete blocker; delete deferrals the pass landed or refuted. A bug goes in the PR, not the ledger.
5. The tightened `refactor-target.toml`, carrying only the pass's own rules.
6. The ledger's Status bullets rewritten for the tree the pass ships. Status holds state, not history: the pass's figures (cx and SLOC before and after, budgets moved, what was judged) go in the PR description and the record commit's message, never in the ledger. Status cites no SHA except the survey base line, since a prose SHA misleads row resolution ([caveat 16](./atlas.md#caveats)).
7. Doc updates the change implies: the module's `AGENTS.md`, its internals page, the [code map](../../AGENTS.md#code-map) when a module moved, [ARCHITECTURE.md](../../ARCHITECTURE.md) when the runtime shape changed, and the prose naming any item whose visibility changed. `CHANGELOG.md` stays untouched.

A review fix after the record commit means re-measuring and amending it.

## Merging

Passes of one round run at once, whatever their layers, and feature work keeps landing beside them. A pass keeps to one contract and one scope, and stays out of an unmerged seam pass's `paths` when it can. Each plan names the other passes' `paths` and a suggested merge order. The later pass to merge rebases, re-pins `base`, reruns `diff --expect` and `gate`, resolves conflicts in the ledger and `refactor-target.toml` (then runs `conform --ratchet`: a textual merge of two budgets does not prove the merged surface fits), and runs `survey --restamp` for rows that earlier merges rewrote.
