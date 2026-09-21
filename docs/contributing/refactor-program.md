# Refactor program: improving the whole repository in passes

The repository is too large to refactor in one review, so the work runs as a program of bounded passes, each an architecture review that ends in a landed, proven change or a recorded `holds`. This page is the operating guide for the agent that runs a pass. The instrument is [`cargo xtask atlas`](./atlas.md), the code shape is [rust-conventions.md](./rust-conventions.md), and the memory between passes is [refactor-ledger.md](./refactor-ledger.md).

An agent starting a pass has no memory of the previous one. Everything that must survive lives in three committed places: the ledger (module verdicts, admission intents, open deferrals), the `[[verdict]]` rows in `refactor-target.toml` (dispositions of items and families atlas suppresses on the next survey), and the ceilings `conform --tighten` lowers after each pass. Commits and code are the record of what a pass changed; a pass that ends without writing to the three places that apply has not ended.

## The method

Each pass asks one question of its scope: if this were re-implemented from scratch, knowing what the code actually does for its callers, what would be written? That target is the destination; every gap between it and the current code is a candidate, tagged with the verb that closes it: `collapse` (two implementations, one wins), `delete` (vestigial weight), `deepen` (callers assemble what the module should hide), or `rehome` (knowledge or a seam in the wrong place). The code's real behaviour is the requirement: a target that reads cleaner because it dropped a load-bearing branch is a failure, and a strange corner is deletable only after `git log -S` and `inspect --item` show it pins nothing.

Two tests decide most candidates. Deletion test: remove the module in your head; if the complexity vanishes it was a pass-through, if it reappears across N callers it earns its keep. Call-site test: write the ideal call for the common case in a line or two; the delta to the real call site is what the interface failed to hide. Every candidate must pass the refactor test — fewer files, flags, abstractions, or options afterwards — or it is a sideways move and is dropped. Finding nothing is a real outcome; a module that already matches what would be built today is recorded as `holds` and the pass moves on. Never inflate a ranking to have something to show.

## Two kinds of pass

**Seam passes** close a dependency direction, collapse a sibling family, or move a seam between modules. They are found in `survey`'s `debt`, module-cycle, and sibling shape-family rows, not in the accretion rank. They are few, wide, and sequential: a seam pass changes the shape of every module it crosses, so it lands before any module pass on those modules, and no other pass runs concurrently on a module it touches. The ledger's seam queue holds them in order.

**Module passes** rethink one module at the granularity `survey` ranks (`store/snapshot`, `cli/hooks`, `agents/spending`): a review of that module's interface as its callers see it. They are many and narrow. Up to three run concurrently in separate worktrees when their scopes are disjoint (rules below). Small neighbouring modules in one layer (sibling adapters, a store's leaf records) are bundled into one pass with one contract rather than run as separate passes.

Seam passes go first because they change what the module passes would see. When the seam queue is empty, module passes proceed from the rank.

## Starting a pass

Read, in this order, before running anything:

1. [refactor-ledger.md](./refactor-ledger.md): the seam queue, the module verdict table, the admission intents, and the open deferrals. A module with a current `holds` verdict is skipped unless its churn since the verdict's SHA has crossed the threshold the row names; `survey` reads the row and flags the rank line `held` or `reopen`, keeping a held module out of the probes. A module the survey ranks under a finer name than its verdict row (`agents/state` beside `agents`) keeps ranking until it gets its own row.
2. `refactor-target.toml`: the `layers`, every `[[module]]` admission that touches the scope, every `[[strangler]]`, and every `[[verdict]]`. A `[[verdict]]` is a recorded decision; do not re-litigate it unless the friction is real enough to reopen it, and then mark the candidate as contradicting the record.
3. The scope's own `AGENTS.md` contract and the internals page the [documentation map](../../AGENTS.md#documentation-map) names for it.

Then survey. Write reports to a scratch directory and narrow them; never let a whole report through stdout.

```sh
cargo xtask atlas survey --out /tmp/atlas/survey.md
cargo xtask atlas survey --section rank --top 90 --out /tmp/atlas/survey-rank.md
cargo xtask atlas survey --by depth --section rank --out /tmp/atlas/survey-depth.md
```

The default rank is code × churn and is dominated by size, so read it beside the depth sort and the ledger before picking; the default top 20 is mostly `held` and `bin` now, so read the top 90. Signals that make a module a candidate: shallow `depth` with many escaping items, `pin` or `thin` flags, an `assemblers` row whose caller wires it with several other modules, churn concentrated in a few hot functions. Signals that make it a likely `holds`: high depth, `t/c` above 1, escaping items reached through one facade. `cli/*` modules are `bin` with a handful of escaping items each; their friction is assembly of the modules they wire, which a pass on the provider fixes, so they are not picked on their own, though a `cli/*` module's pins can be the prerequisite of the provider's pass.

The pick goes to the user as confirm-or-redirect, with the reason for it and the runners-up, before any deep read starts. A seam pass at the head of the queue is proposed ahead of any module pass.

## Running a pass

The pass follows the review workflow atlas documents, with these rules on top.

**Learn.** `inspect --module <target> --brief --out` is the dossier; read the verdict, the record, and the heaviest quote yourself, and fan the remaining modules the scope touches out to one surveyor subagent each with the dossier as its brief and one question to answer. `--item` on every candidate before calling it deletable; `--from <caller>` for each caller the `assemblers` table named. The assembly count folds type references with function calls, so for a data module (a schema everyone reads) a heavy caller that names five enums is use, not assembly; judge assembly by the functions and builders a caller wires.

**Rethink.** Write the target: a page, concrete enough to diff against, naming each module, what it hides from its callers, and everything in the current code with no counterpart. Design it twice, the second time in a fresh context given only the dossiers and call sites with the direction "the smallest interface that serves these callers", and keep whichever wins on depth, locality of change, and seam placement.

**Diff.** Every candidate carries its verb, its line arithmetic (removed against added), the files it touches, and the tests that move. A candidate that cannot be turned into per-file steps is not learned yet. A candidate that changes something the pass cannot edit (other repos, published packages, users' config and invocations, stored data, schemas, wire formats) is reported as a risk finding, never planned. A candidate hinging on intent the code and history do not reveal goes to the user as a brief question with a recommendation.

**Present.** In the conversation, biggest win first, for an engineer who knows the repo. Close with the proposed pick: every `Strong` candidate plus the cheap `Worth exploring` ones, bundled per module. The user confirms, strikes, or adds; the plan is written only after that reply.

**Plan.** One self-standing ordered plan for a coder who has only the plan: title, the target in full, the contract (behaviour-preserving, net-subtractive, bugs found are reported rather than fixed), the line budget, prerequisites (tests to pin, landed as the first commit and green on the base), tests that move, per-file steps led by their verb, and verification commands. Rules that keep the plan from stopping the coder:

- Derive the tests-that-move table mechanically: for every symbol a `[[delete]]` row, a narrowing, or a signature change names, `rg -n '<symbol>' crates/rimz/src crates/rimz/tests crates/rimz/benches` and list every hit outside the owning module.
- Quote every signature, derive, and literal the target or a step names from its declaration line, not from a subagent report or the grammar as described; read each pin's expected value from the code path.
- Price the line budget from the target written out: each new signature in rustfmt form, the `?` error boundary each shared helper crosses (a helper over two error types is a generic or a new enum, never a free line), one line per file that imports the old and new homes together after a no-shim move, and a header, imports and `mod` lines per new module file. Measure the largest move on a spike before locking the ceiling; a module pass's ceiling is negative, a seam pass's is flat ([Seam passes](#seam-passes)). A narrowing-only module pass is the one exception, and it takes a small positive ceiling with the reason in the commit: the widened keyword is seven characters wider than `pub `, so a signature already near the line budget is broken into its parameter-per-line form by rustfmt alone, and a pass can add more formatting lines than the vestigial code it deletes. Price that expansion with the rest of the budget — count the declaration lines your narrowings touch that would cross 100 columns — rather than trading a narrowing away to buy lines back.
- Count `esc` from `inspect --json` items plus the `pub use` names, never from memory; the escaping-item delta is an explicit sum of narrowings minus additions. A narrowing the compiler will refuse (E0446, `private_interfaces`: the item is a field, parameter or return type of something more visible) is not a candidate; atlas's `narrow to` column does not see signature reach ([caveat 13](./atlas.md#caveats)).
- Write the draft contract with the merge-base SHA pinned and every edited path listed, callers and tests included, and run `cargo xtask atlas diff --expect <draft>` against the clean base before the plan ships: every row must resolve, and the printed base counts are the ceilings' starting figures. If a sandboxed spike cannot write the worktree's shared `.git`, use an independent clone (`git clone --no-hardlinks <repo> /tmp/<pass>-spike`) and a separate `CARGO_TARGET_DIR`.
- Before locking the contract, read the code that enforces each row kind it uses rather than a model of it: the schema is `xtask/src/atlas/contract.rs`, every `diff --expect` row is judged in `xtask/src/atlas/diff.rs`, the per-commit ratchet is `xtask/src/atlas/conform.rs`, and what counts as production SLOC and as a dependency site is decided in `xtask/src/atlas/sources.rs` and `syntax.rs`. The [Caveats](./atlas.md#caveats) index the surprises already found; the code is the authority.

The pass contract goes in the plan verbatim as TOML, written to a scratch path outside the worktree:

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

`paths` names every module the pass edits, callers included; `diff --expect` fails on a change outside them, which is what makes the pass reviewable and what makes concurrent passes safe.

**Execute.** In a worktree on a branch named for the pass. Pin first: the prerequisite tests land as the first commit and pass on the base before any structural change. Then the per-file steps in plan order, one commit per step or per bundled module. `cargo xtask check` while iterating, `cargo xtask gate` before the hand-off, and the reach judgement in [AGENTS.md → Testing](../../AGENTS.md#testing) for whether the pass touches a surface that needs the journey, live-backend, or performance tiers. A pass touching `crates/rimz/benches/` or a walker's `pub` wire also runs `cargo xtask perf -- --no-run` to link the benches. `cargo xtask atlas conform --ratchet` passes at every commit; `--tighten` only lowers, so a new rule's budget and a repointed rule's admissions are written from the block `--ratchet` prints at their measured values. A step that narrows a `pub` item converts every rustdoc link made private by that narrowing to plain backticks in the same commit; `cargo xtask doc` verifies the result. `check` compiles with `cfg(test)` and `lint` without, so a narrowing whose surviving readers are all unit tests passes the first and fails the second on an unused item; spell it `#[cfg(test)] pub(crate)` (precedent `config.rs`, `parse_scheme_text`) rather than widening it back. A commit that lifts a module below its old home leaves it a leaf in that same commit: the ratchet counts a `use` line or a qualified path into a higher layer as an upward site and fails on the first one no admission covers.

**Prove.** `cargo xtask atlas diff --expect <contract>` exits zero or the pass has drifted; drift is fixed or the contract is loosened with the reason written in the commit, never silently. Run it on a tree that has already passed `cargo xtask gate`: the index key hashes every Rust source, so a lint fix found after `diff` rebuilds the whole-workspace SCIP index and the proof runs again. The contract's `[[dependency]]` rows are the proof that a direction is closed; a grep is a convenience, and it must not count rustdoc links, which atlas never counts as sites (a site is a `use` or a qualified path): `rg -nH 'crate::<module>' <scope> --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/tests/**' | rg -v '^[^:]+:[0-9]+:\s*//'`. This still reports sites inside inline `#[cfg(test)]` modules, which atlas excludes, so inspect a nonzero result before believing it. Then `cargo xtask atlas conform --tighten --only <owned-path>` (repeat `--only` for each owned path) so the ceilings the pass earned stay earned without changing another pass's rules; `--tighten` re-serializes the TOML and drops its comments, so restore them.

## Ending a pass

A pass ends with the last commit of the branch, a `docs(refactor): …` commit that carries the durable state; every number it writes is measured on the tree it ships:

1. A module verdict row in the ledger for every module the pass reviewed, carrying the reviewed source SHA and the churn threshold that reopens it (default 30): `holds` when nothing landed, `holds; landed pass-N` when the pass rethought the module's interior, since only a `holds` status demotes the module in the next survey. The SHA is the post-rebase source tip, the record commit's parent (a commit cannot carry its own hash); re-stamp it when a post-review rebase rewrites the branch. A seam pass that reviewed a module's edges and left its interior a candidate writes a bare `landed pass-N`. The note names what holds by verdict, in a sentence, so the next reader does not re-derive it; deltas, contract paths and step narratives belong to the commit messages and the PR, not the ledger.
2. `[[verdict]]` rows in `refactor-target.toml` for every item, pass-through, guard family, or shape family the pass judged and kept, each with the reason. These stop the next survey from surfacing the same family; a pass that judges a family and writes no verdict has left the next agent to re-derive it. A survey footer listing `stale verdict keys` means a family's key changed under a landed pass; re-key the row.
3. An admission intent row in the ledger for every upward dependency the pass reviewed: `keep` with the reason it is the intended shape, or `close` with the pass that would close it. An edge the pass closed loses its admission and its row.
4. A line under the ledger's open deferrals for a candidate the pass judged real but could not land, with the condition that unblocks it (a gate that refuses it, a held module it must wait for, a user decision); a compiler-refused narrowing is not a deferral. A deferral the pass landed or refuted is deleted.
5. The tightened `refactor-target.toml` from `conform --tighten --only <owned-path>`; the diff carries only the pass's own rules.
6. Doc updates the change implies: the module's `AGENTS.md`, its internals page, the [code map](../../AGENTS.md#code-map) when a module moved, and [ARCHITECTURE.md](../../ARCHITECTURE.md) when the runtime shape changed. The plan's docs step lists the items whose visibility changed beside the items that moved, and the sweep covers the prose that names their reachers, not only the links to their old paths. `CHANGELOG.md` stays untouched.

If a review fix or loosened contract lands after the record commit, re-measure and amend it. A pass that found nothing ends the same way: a `holds` verdict per module read and the `[[verdict]]` rows for the families it judged.

## Seam passes

A seam pass is planned like a module pass but with these differences:

- The contract's `paths` lists every module on both sides of the seam, and the proof is a `[[dependency]]` row per edge with `max-sites` at the target count (`0` for a closed direction), beside the `[[rehome]]` rows for what moves. The contract declares `kind = "seam"` and takes a flat ceiling — `max-production-sloc-delta = 0`, or a small positive with the reason in the commit — since its value is the closed edges, not the line count; net-subtractive stays the module-pass rule.
- It runs alone. No module pass runs on any module in its `paths` until it merges, and the ledger's seam queue marks it `in flight` with the branch name while it does.
- Its target states the intended direction in one sentence that is copied into the module's `AGENTS.md` and into `refactor-target.toml` as a lowered admission, so the seam stays closed by the `conform` gate rather than by memory. When it lands, the queue row is deleted.
- A sibling-family collapse names which member wins and quotes the divergences that must survive (the ones `git log -S` traces to a fix). Atlas has no family-level dossier; build one by running `inspect --module` on each sibling with the same `--section` set and reading them side by side.

## Module passes and concurrency

Two module passes may run concurrently when all of these hold:

- Their contract `paths` are disjoint, including the caller modules each pass edits.
- Neither touches an integration test file the other does (`crates/rimz/tests/integration/<suite>.rs` is the unit of collision).
- They sit in different layers of `refactor-target.toml`, so neither changes an interface the other consumes mid-pass.
- Neither is a seam pass, and neither touches a module in an in-flight seam pass's `paths`.

Three concurrent passes is the practical ceiling: past that the merge order dominates the work. Every plan names the merge order and the other passes' `paths`; before merging, a pass rebases onto trunk and re-pins the contract's `base` to the new merge-base and reruns `diff --expect` and `gate`, since the contract measures against the merge base and a rebase moves it. The ledger and `refactor-target.toml` are edited by every pass, so the later pass resolves those conflicts on rebase.

## Tests inside a pass

There is no separate test-suite program; two rules inside every pass do that work.

- Behaviour is pinned at the interface before internals move. The pins are the first commit, green on the base. A module whose escaping items have no test site outside the module (`pins` in `inspect` is empty and `t/c` is low) pays this prerequisite in full; one that is already covered through its interface pays nothing.
- Tests that reach past the narrowed reach (`inspect`'s `pins` table) move with the narrowing: rewritten to the new call site when the assertion still matters, deleted with the internals when it does not. The plan names every one; a test change the plan does not name holds review.

The gate's wall clock is a program-level cost: a pass iterates against `cargo xtask gate` (about three minutes today), and a gate near its 15-minute budget costs more per pass than any code shape does.

## Cadence and collision

Trunk moves at tens of commits a day, so a pass that takes more than a few days is rebasing against a moving target. Keep a pass to one contract and one scope, and to a week at most. When a pass collides with in-flight feature work in its scope, the plan names the branch and the merge order, and the pass waits for the feature rather than the other way round.

## What the ledger covers that atlas does not

`survey` reads the module verdict and admission intent tables (`xtask/src/atlas/ledger.rs`), so their column shapes are a contract; the seam queue and open deferrals are prose atlas never reads.

- **Module verdicts.** Atlas has no module-level "reviewed and holds" record of its own; the verdict table's `holds` status with its SHA and churn threshold is what demotes a reviewed module (`held`/`reopen` on the rank line), and a row without them leaves the module at the top of the rank every run.
- **Admission intents.** A `[[module]]` admission in `refactor-target.toml` is a ratchet, not a decision. The intents table marks each edge `keep` or `close`; the survey lists every admitted edge with no row as `unreviewed`, and that column is the review backlog.
- **Seam queue and open deferrals.** A seam spans several modules and a deferral spans passes; neither has an atlas home, so the ledger carries them until a pass lands or refutes them.
