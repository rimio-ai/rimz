# Refactor program: improving the whole repository in passes

The repository is too large to refactor in one review, so the work runs as bounded passes, each an architecture review that ends in a landed, proven change or a recorded `holds`. This page is the operating guide for the agent running a pass. The instrument is [`cargo xtask atlas`](./atlas.md), the code shape is [rust-conventions.md](./rust-conventions.md), and the state between passes is the [ledger](./refactor-ledger.md), whose Status section says where the program stands. The code is the result; commits and PRs are the history.

An agent starting a pass has no memory of the previous one. What must survive lives in three committed places: the ledger (status, module verdicts, admission intents, open deferrals), the `[[verdict]]` rows in `refactor-target.toml` (shape and guard verdicts hide their families on the next survey; item verdicts are shown by `inspect`), and the ceilings `conform --tighten` lowers. A pass that ends without writing to the ones that apply has not ended.

## The method

Each pass asks of its scope: re-implemented from scratch, knowing what the code does for its callers, what would be written? Every gap between that target and the code is a candidate tagged with the verb that closes it: `collapse` (two implementations, one wins), `delete` (vestigial weight), `deepen` (callers assemble what the module should hide), or `rehome` (knowledge or a seam in the wrong place). Real behaviour is the requirement: a strange corner is deletable only after `git log -S` and `inspect --item` show it pins nothing.

Two tests decide most candidates. Deletion test: remove the module in your head; if the complexity vanishes it was a pass-through, if it reappears across callers it earns its keep. Call-site test: write the ideal call in a line or two; the delta to the real call site is what the interface failed to hide. A candidate must leave fewer files, flags, abstractions or options, or it is dropped. Finding nothing is a real outcome, recorded as `holds`; never inflate a ranking. In a complexity pass, a single-use extraction that deletes nothing is not a reduction: record the residual as a `[[verdict]]` item row instead.

## Three kinds of pass

**Seam passes** close a dependency direction, collapse a sibling family, or move a seam between modules. They come from `survey`'s `debt`, module-cycle and shape-family rows, run alone and sequentially, and go before any module pass on the modules they touch. The ledger's Status section queues them.

**Module passes** rethink one module at the granularity `survey` ranks (`store/snapshot`, `agents/spending`) as its callers see it. Up to three run concurrently when their scopes are disjoint ([rules](#concurrency)). Small neighbours in one layer are bundled into one pass with one contract.

**Tooling passes** add a capability the user decided to `xtask/`, such as a new atlas behaviour. The contract declares `kind = "tooling"` and takes a positive ceiling priced from the target, with the reason in the commit; `diff --expect` asks it for no narrowing, so it narrows only where the design calls for it.

## Starting a pass

Read the ledger, then `refactor-target.toml` (layers, admissions touching the scope, stranglers, `[[verdict]]` rows: a verdict is a decision, reopened only with real friction and marked as contradicting the record), then the scope's `AGENTS.md` and internals page. Then survey into a scratch directory and narrow the reports:

```sh
cargo xtask atlas survey --out /tmp/atlas/survey.md
cargo xtask atlas survey --section rank --top 400 --out /tmp/atlas/rank.md
cargo xtask atlas survey --by depth --section rank --out /tmp/atlas/depth.md
```

Read the footer first: `ledger problem` lines (an unresolved verdict SHA leaves its module unheld), `ledger restamp` mappings, and `stale verdict keys` are record defects to fix before picking. Write the mappings with `cargo xtask atlas survey --restamp` in the pass's record commit. Then `reopen` flags, unheld rows, `unreviewed` admissions, and module cycles missing from the ledger. The rank is code × churn and dominated by size, so read it beside the depth sort. Candidate signals: shallow depth with many escaping items, `pin` or `thin`, an `assemblers` row wiring several modules, churn in a few hot functions. `holds` signals: high depth, `t/c` above 1, one facade. `cli/*` modules are `bin` and are not picked on their own; their friction is fixed by a pass on the provider. A module that is days old and `hot` waits for its pace to settle. Before proposing to move code between modules, run `git log --oneline -- <source file>` for an earlier move between the same two modules. Quote any ledger row the proposal cites, found by `rg` on its exact spelling.

The pick goes to the user as confirm-or-redirect with the reason and the runners-up before any deep read; a queued seam is proposed first.

## Running a pass

**Learn.** `inspect --module <target> --brief --out` is the dossier; fan the other touched modules out to one subagent each. `--item` on every candidate before calling it deletable, `--from <caller>` for each assembler. For a data module, a caller naming five enums is use, not assembly; judge by the functions and builders it wires.

**Rethink.** Write the target concretely enough to diff against: each module, what it hides, and everything in the current code with no counterpart. Design it twice, the second time in a fresh context given only the dossiers and call sites and "the smallest interface that serves these callers"; keep the winner on depth, locality and seam placement.

**Diff.** Each candidate carries its verb, line arithmetic, files, and moving tests; one that cannot become per-file steps is not learned yet. A change to something the pass cannot edit (other repos, users' config, stored data, schemas, wire formats) is a risk finding, never planned. Intent the code and history do not reveal goes to the user as a question with a recommendation.

**Present.** Biggest win first, closing with the proposed pick (every `Strong` candidate plus the cheap `Worth exploring` ones). The plan is written after the user confirms, strikes or adds.

**Plan.** One self-standing ordered plan: title, target, contract (behaviour-preserving, net-subtractive, bugs reported not fixed), line budget, prerequisite pin tests (the first commit, green on base), tests that move, per-file steps led by their verb, verification commands. Rules that keep the coder from stopping:

- Derive the tests that move mechanically: `rg -n '<symbol>' crates/rimz/src crates/rimz/tests crates/rimz/benches` for every symbol a `[[delete]]`, narrowing or signature change names; list every hit outside the owning module.
- Quote every signature, derive and literal from its declaration line, not a subagent report; read each pin's expected value from the code path.
- Price the budget from the written target: each new signature in rustfmt form, each error boundary a shared helper crosses (two error types is a generic or a new enum), an import line per file naming old and new homes after a no-shim move, and header, imports and `mod` lines per new file. Measure the largest move on a spike. A module pass's ceiling is negative, a seam pass's flat. A narrowing-only pass may take a small positive ceiling, with the reason in the commit and an `[[esc]]` row below the measured base: `pub(crate)` is seven characters wider than `pub `, so rustfmt breaks near-limit signatures. Count those lines rather than trading a narrowing away.
- Count `esc` from `inspect --json` items plus `pub use` names. A narrowing the compiler refuses (E0446, `private_interfaces`: the item appears in a more visible signature) is not a candidate ([caveat 13](./atlas.md#caveats)), and neither is one whose readers are in `cli/**`, `tests/**` or `benches/` (atlas now floors these cross-target readers at `keep`: separate crates see only `pub`) or in another module's public rustdoc (`cargo xtask doc` is a gate).
- Write the draft contract with the merge-base SHA pinned, computed with `git merge-base HEAD origin/main` at the moment you write it, never copied from notes or an earlier read, and every edited path listed. Every `[[esc]]` path must equal, sit inside, or be the `.rs` module file of one of `paths`; to cap a parent boundary, list the parent in `paths`. Run `cargo xtask atlas diff --expect <draft>` on the clean base: every row must resolve, and the printed base counts are the ceilings' starting figures. A sandboxed spike that cannot write `.git` uses `git clone --no-hardlinks` and its own `CARGO_TARGET_DIR`.
- Read the code that enforces each row kind before locking the contract: schema `xtask/src/atlas/contract.rs`, judging `diff.rs`, ratchet `conform.rs`, SLOC and dependency sites `sources.rs` and `syntax.rs`. The [caveats](./atlas.md#caveats) index known surprises.

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

`paths` names every module the pass edits, callers included; `diff --expect` fails on a change outside them, which keeps the pass reviewable and concurrent passes safe.

**Execute.** In a worktree on a branch named for the pass: pins first, then the steps in plan order, one commit per step or bundled module. `cargo xtask check` while iterating, `cargo xtask gate` before hand-off, higher tiers per [AGENTS.md → Testing](../../AGENTS.md#testing); a pass touching `benches/` or a walker's `pub` wire also runs `cargo xtask perf -- --no-run`. `cargo xtask atlas conform --ratchet` passes at every commit; new or repointed budgets are written from the block `--ratchet` prints. Take a surface-budget change only from what `atlas conform --ratchet` fails on, and add its admission comment only when the tightened budget still sits above base. Rules the compiler and gates enforce:

- A narrowing turns rustdoc links it made private into plain backticks in the same commit (`cargo xtask doc`).
- An item whose surviving readers are all unit tests is spelled `#[cfg(test)] pub(crate)` (precedent `config.rs`, `parse_scheme_text`), since `lint` builds without `cfg(test)`.
- A module lifted below its old home is a leaf in that same commit: an upward `use` or qualified path fails the ratchet.

**Prove.** On a tree that passed `gate`, `cargo xtask atlas diff --expect <contract>` exits zero, or the drift is fixed or the contract loosened with the reason in the commit. A lint fix after `diff` reindexes, so prove again. `[[dependency]]` rows prove a closed direction; a grep is a convenience and must not count rustdoc links or inline `#[cfg(test)]` sites. Then `cargo xtask atlas conform --tighten --only <owned-path>` per owned path, restoring the comments it drops.

## Ending a pass

After the final rebase, set the contract's `base` to the branch's new merge base, then run `atlas diff --expect`; trunk commits otherwise count as changed paths.

The last commit, `docs(refactor): …`, carries the durable state, every number measured on the tree it ships:

1. A ledger verdict row for every module reviewed, at the survey's exact spelling: `holds`, or `holds; landed pass-N` when the interior was rethought; a seam pass that only reviewed edges writes bare `landed pass-N`. Stamp the post-rebase source tip (the record commit's parent). After a merge rewrites it, the survey maps it to the trunk commit that wrote the row; a SHA no trunk commit wrote to the ledger leaves the module unheld. The [resolution caveat](./atlas.md#caveats) covers older prose mentions of that SHA. The reopen count defaults to 30. The note is one clause naming what holds by verdict. Binary (`bin`) modules receive no ledger row: record them through `[[verdict]]` item rows and the Status paragraph.
2. `[[verdict]]` rows in `refactor-target.toml` for every item, family or pass-through judged and kept, with the reason; re-key rows the survey lists as stale.
3. An admission intent row for every upward edge reviewed: `keep` with the reason, or `close` naming the closing pass. A closed edge loses its admission and its row.
4. An open deferral line for a candidate judged real but not landable, with its unblocking condition; delete deferrals the pass landed or refuted.
5. The tightened `refactor-target.toml`, carrying only the pass's own rules.
6. The ledger's Status section rewritten for the tree the pass ships. Status names the merge base a pass measured against, never its own branch commits, which a rebase or squash merge rewrites.
7. Doc updates the change implies: the module's `AGENTS.md`, its internals page, the [code map](../../AGENTS.md#code-map) when a module moved, [ARCHITECTURE.md](../../ARCHITECTURE.md) when the runtime shape changed, and the prose naming any item whose visibility changed. `CHANGELOG.md` stays untouched.

A review fix after the record commit means re-measuring and amending it.

## Seam passes

- The contract declares `kind = "seam"`, lists every module on both sides in `paths`, proves each edge with a `[[dependency]]` row at its target `max-sites` (`0` for a closed direction) beside `[[rehome]]` rows for what moves, and takes a flat ceiling (`0`, or a small positive with the reason in the commit).
- It runs alone: no module pass touches its `paths` until it merges, and the ledger marks it `in flight <branch>`.
- Its target states the direction in one sentence copied into the module's `AGENTS.md` and into `refactor-target.toml` as a lowered admission, so `conform` keeps it closed.
- A same-layer seam has no admission to lower: close it by lowering one side's layer or setting an `allowed-dependencies` list.
- A sibling-family collapse names the winning member and quotes the divergences that must survive (those `git log -S` traces to a fix). Build the family dossier by running `inspect --module` on each sibling with the same `--section` set.
- Two contracts on one branch from one base each list the union of changed paths, Markdown and TOML included; each contract's proof is its own dependency, delete or esc rows.

## Concurrency

Two module passes may run at once when their contract `paths` are disjoint (callers included), they share no `crates/rimz/tests/integration/<suite>.rs`, they sit in different layers, and neither touches an in-flight seam's `paths`. Three is the practical ceiling. Take the pass number from the ledger and from open PRs (`gh pr list --search 'pass <N>'`, with `<N>` the ledger's current pass): a pass in flight holds its number before the ledger records it. Every plan names the merge order and the other passes' `paths`; before merging, a pass rebases, re-pins `base`, and reruns `diff --expect` and `gate`. The later pass resolves ledger and `refactor-target.toml` conflicts.

## Tests inside a pass

Behaviour is pinned at the interface before internals move; a module whose escaping items have no outside test site (`pins` empty, `t/c` low) pays this in full. Tests reaching past a narrowed reach move with it: rewritten when the assertion matters, deleted with the internals when not; the plan names each one. The gate runs in about three minutes; a gate near its 15-minute budget would cost more per pass than any code shape.

## Cadence

Trunk moves at tens of commits a day: keep a pass to one contract, one scope, a week at most. A pass colliding with feature work in its scope names the branch and the merge order and waits for the feature.
