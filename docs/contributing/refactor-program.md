# Refactor program: improving the whole repository in passes

The repository is too large to refactor in one review, so the work runs as a program of bounded passes, each one an architecture review that ends in a landed, proven change or a recorded "holds". This page is the operating guide for the agent that runs a pass. It owns the sequence and the rules between passes; the instrument is [`cargo xtask atlas`](./atlas.md), the code shape is [rust-conventions.md](./rust-conventions.md), and the memory between passes is [refactor-ledger.md](./refactor-ledger.md).

An agent starting a pass has no memory of the previous one. Everything that must survive between passes lives in three committed places: the ledger (what was judged, what landed, what was deferred, and why), the `[[verdict]]` rows in `refactor-target.toml` (durable dispositions of items and families atlas suppresses on the next survey), and the ceilings `conform --tighten` lowers after each pass. A pass that ends without writing to all three that apply has not ended.

## The method

Each pass asks one question of its scope: if this were re-implemented from scratch, knowing what the code actually does for its callers, what would be written? That target is the destination; every gap between it and the current code is a candidate, tagged with the verb that closes it: `collapse` (two implementations, one wins), `delete` (vestigial weight), `deepen` (callers assemble what the module should hide), or `rehome` (knowledge or a seam in the wrong place). The code's real behaviour is the requirement: a target that reads cleaner because it dropped a load-bearing branch is a failure, and a strange corner is deletable only after `git log -S` and `inspect --item` show it pins nothing.

Two tests decide most candidates. Deletion test: remove the module in your head; if the complexity vanishes it was a pass-through, if it reappears across N callers it earns its keep. Call-site test: write the ideal call for the common case in a line or two; the delta to the real call site is what the interface failed to hide. Every candidate must pass the refactor test — fewer files, flags, abstractions, or options afterwards — or it is a sideways move and is dropped. Finding nothing is a real outcome; a module that already matches what would be built today is recorded as `holds` and the pass moves on. Never inflate a ranking to have something to show.

## Two kinds of pass

The program alternates between two shapes, and the order matters.

**Seam passes** close a dependency direction, collapse a sibling family, or move a seam between modules. They are found in `survey`'s `debt`, module-cycle, and sibling shape-family rows, not in the accretion rank. They are few, wide, and sequential: a seam pass changes the shape of every module it crosses, so it lands before any module pass on those modules, and no other pass runs concurrently on a module it touches. The ledger's seam queue holds them in order.

**Module passes** rethink one module at the granularity `survey` ranks (`store/snapshot`, `cli/hooks`, `agents/spending`): a review of that module's interface as its callers see it. They are many and narrow. Up to three run concurrently in separate worktrees when their scopes are disjoint (rules below).

Seam passes go first because they change what the module passes would see. When the seam queue is empty, module passes proceed from the rank.

## Starting a pass

Read, in this order, before running anything:

1. [refactor-ledger.md](./refactor-ledger.md) in full: the seam queue, the module verdict table, the pass log, and the admission intents. A module with a current `holds` verdict is skipped unless its churn since the verdict's SHA has crossed the threshold the row names.
2. `refactor-target.toml`: the `layers`, every `[[module]]` admission that touches the scope, every `[[strangler]]`, and every `[[verdict]]`. A `[[verdict]]` is a recorded decision; do not re-litigate it unless the friction is real enough to reopen it, and then mark the candidate as contradicting the record.
3. `git worktree list` and `git branch -a`: in-flight work names the modules a pass must not touch and the collisions a plan has to name.
4. The scope's own `AGENTS.md` contract and the internals page the [documentation map](../../AGENTS.md#documentation-map) names for it.

Then survey. Write reports to a scratch directory and narrow them; never let a whole report through stdout.

```sh
cargo xtask atlas survey --out /tmp/atlas/survey.md
cargo xtask atlas survey --by depth --section rank --out /tmp/atlas/survey-depth.md
cargo xtask atlas survey --by tc --section rank --out /tmp/atlas/survey-tc.md
```

The default rank is code × churn and is dominated by size, so read it beside the depth and test-ratio sorts and the ledger before picking. Signals that make a module a candidate: shallow `depth` with many escaping items, `pin` or `thin` flags, an `assemblers` row whose caller wires it with several other modules, and churn concentrated in a few hot functions. Signals that make it a likely `holds`: high depth, `t/c` above 1, and its escaping items reached through one facade. `cli/*` modules are `bin` with a handful of escaping items each; their friction is assembly of the modules they wire, which a pass on the provider fixes, so they are not picked on their own.

The pick goes to the user as confirm-or-redirect, with the reason for it and the two runners-up, before any deep read starts. A seam pass at the head of the queue is proposed ahead of any module pass.

## Running a pass

The pass follows the review workflow atlas documents, with these rules on top.

**Learn.** `inspect --module <target> --brief --out` is the dossier; read the verdict, the record, and the heaviest quote yourself, and fan the remaining modules the scope touches out to one surveyor subagent each with the dossier as its brief and one question to answer. `--item` on every candidate before calling it deletable; `--from <caller>` for each caller the `assemblers` table named. The assembly count folds type references with function calls, so for a data module (a schema everyone reads, such as `store::snapshot`) a heavy caller that names five enums is use, not assembly; judge assembly by the functions and builders a caller wires, not the types it matches on.

**Rethink.** Write the target: a page, concrete enough to diff against, naming each module, what it hides from its callers, and everything in the current code with no counterpart. Design it twice, the second time in a fresh context given only the dossiers and call sites with the direction "the smallest interface that serves these callers", and keep whichever wins on depth, locality of change, and seam placement.

**Diff.** Every candidate carries its verb, its line arithmetic (removed against added), the files it touches, and the tests that move. A candidate that cannot be turned into per-file steps is not learned yet. A candidate that changes something the pass cannot edit (other repos, published packages, users' config and invocations, stored data, schemas, wire formats) is reported as a risk finding, never planned. A candidate hinging on intent the code and history do not reveal goes to the user as a brief question with a recommendation.

**Present.** In the conversation, biggest win first, for an engineer who knows the repo. Close with the proposed pick: every `Strong` candidate plus the cheap `Worth exploring` ones, bundled per module. The user confirms, strikes, or adds; the plan is written only after that reply.

**Plan.** One self-standing ordered plan for a coder who has only the plan: title, the target in full, the contract (behaviour-preserving, net-subtractive, bugs found are reported rather than fixed), the line budget, prerequisites (tests to pin, landed as the first commit and green on the base), tests that move, per-file steps led by their verb, and verification commands. The line budget prices what a no-shim move adds as well as what it deletes: one line per file that imported the old and new homes together, and a header, imports, and `mod` lines per new module file; measure the largest move on a spike with `cargo xtask atlas diff --base main --path <scope>` before locking the ceiling, which the contract requires to be negative. Before locking the contract, read the code that enforces each row kind it uses rather than a model of it: the schema is `xtask/src/atlas/contract.rs`, every `diff --expect` row is judged in `xtask/src/atlas/diff.rs`, the per-commit ratchet is `xtask/src/atlas/conform.rs`, and what counts as production SLOC and as a dependency site is decided in `xtask/src/atlas/sources.rs` and `syntax.rs`. The [Caveats](./atlas.md#caveats) in atlas.md index the surprises already found; the code is the authority. Pass 1 wrote four of its nine plan defects (a zero budget for a new rule, a non-negative ceiling, a `pub use` counted as a definition site, and cfg accounting) from a model of atlas, and each cost a coder stop that ten minutes of reading would have saved. The pass contract goes in the plan verbatim as TOML, written to a scratch path outside the worktree:

```toml
version = 2
base = "main"
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

**Execute.** In a worktree on a branch named for the pass. Pin first: the prerequisite tests land as the first commit and pass on the base before any structural change. Then the per-file steps in plan order, one commit per step or per bundled module. `cargo xtask check` while iterating, `cargo xtask gate` before the hand-off, and the reach judgement in [AGENTS.md → Testing](../../AGENTS.md#testing) for whether the pass touches a surface that needs the journey, live-backend, or performance tiers. `cargo xtask atlas conform --ratchet` passes at every commit; `--tighten` only lowers, so a new rule's budget and a repointed rule's admissions are written by hand at their measured values. A commit that lifts a module below its old home leaves it a leaf in that same commit: the ratchet counts a `use` line or a qualified path into a higher layer as an upward site and fails on the first one no admission covers, so the deletion that removes the last upward reference belongs in the lift commit, not the next one.

**Prove.** `cargo xtask atlas diff --expect <contract>` exits zero or the pass has drifted; drift is fixed or the contract is loosened with the reason written in the pass log, never silently. Run it on a tree that has already passed `cargo xtask gate`: the index key hashes every Rust source, so a lint fix found after `diff` rebuilds the whole-workspace SCIP index (over a minute) and the proof runs again. The order that ends a step is `gate`, then `diff --expect` on the tree that passed, then commit. The contract's `[[dependency]]` rows are the proof that a direction is closed; a grep is a convenience, and it must not count rustdoc links, which atlas never counts as sites (a site is a `use` or a qualified path). Drop comment lines rather than delete the links to make the grep empty, which is what pass 1 did and review restored: `rg -n 'crate::<module>' <scope> --glob '!**/tests.rs' --glob '!**/tests/**' | rg -v '^[^:]+:[0-9]+:\s*//'`. Then `cargo xtask atlas conform --tighten` so the ceilings the pass earned stay earned.

## Ending a pass

A pass ends with the last commit of the branch, which records it; every number in the record is measured on the tree that commit ships:

1. A pass row in the ledger's pass log: date, scope, base SHA, the verbs landed, the deltas copied from `diff --expect` on that commit (production SLOC, `esc`, dependency sites), and every candidate deferred with the reason.
2. A module verdict row for every module the pass reviewed: `landed` with the pass row it points at, or `holds` with the SHA reviewed and the churn threshold that reopens it (default: 30 scoped commits, `git log --oneline <sha>.. -- <path> | wc -l`).
3. `[[verdict]]` rows in `refactor-target.toml` for every item, pass-through, guard family, or shape family the pass judged and kept, each with the reason. These are what stop the next survey from surfacing the same family; a pass that judges a family and writes no verdict has left the next agent to re-derive it.
4. An admission intent in the ledger for every upward dependency the pass reviewed: `keep` with the reason it is the intended shape, or `close` with the seam pass that would close it.
5. The tightened `refactor-target.toml` from `conform --tighten`.
6. Doc updates the change implies: the module's `AGENTS.md`, its internals page, the [code map](../../AGENTS.md#code-map) when a module moved, and [ARCHITECTURE.md](../../ARCHITECTURE.md) when the runtime shape changed. `CHANGELOG.md` stays untouched.

Any commit that lands after the row is written — a review fix, a loosened contract, a rebase that moves the merge base — re-measures and updates the row in the same commit. Pass 1 wrote its row at step 7 (−2), review removed two more lines, and a tenth commit with its own review round existed only to correct the number to −4.

A pass that found nothing ends the same way: a pass row saying so, a `holds` verdict per module read, and the `[[verdict]]` rows for the families it judged.

## Seam passes

A seam pass is planned like a module pass but with these differences:

- The contract's `paths` lists every module on both sides of the seam, and the proof is a `[[dependency]]` row per edge with `max-sites` at the target count (`0` for a closed direction), beside the `[[rehome]]` rows for what moves.
- It runs alone. No module pass runs on any module in its `paths` until it merges, and the ledger's seam queue marks it `in flight` with the branch name while it does.
- Its target section states the intended direction in one sentence that can be copied into the module's `AGENTS.md` and into `refactor-target.toml` as a lowered admission, so the seam stays closed by the `conform` gate rather than by memory.
- A sibling-family collapse names which member wins and quotes the divergences that must survive (the ones `git log -S` traces to a fix). Atlas has no family-level dossier; build one by running `inspect --module` on each sibling with the same `--section` set and reading them side by side.

## Module passes and concurrency

Two module passes may run concurrently when all of these hold:

- Their contract `paths` are disjoint, including the caller modules each pass edits.
- Neither touches an integration test file the other does (`crates/rimz/tests/integration/<suite>.rs` is the unit of collision).
- They sit in different layers of `refactor-target.toml`, so neither changes an interface the other consumes mid-pass.
- Neither is a seam pass, and neither touches a module in an in-flight seam pass's `paths`.

Three concurrent passes is the practical ceiling: past that the merge order dominates the work. The merge order is the assumes-X-landed order the plans name; before merging, a pass rebases onto trunk and reruns `diff --expect` and `gate`, since the contract measures against the merge base and a rebase moves it.

## Tests inside a pass

There is no separate test-suite program; two rules inside every pass do that work.

- Behaviour is pinned at the interface before internals move. The pins are the first commit, green on the base. A module whose escaping items have no test site outside the module (`pins` in `inspect` is empty and `t/c` is low) pays this prerequisite in full; one that is already covered through its interface pays nothing.
- Tests that reach past the narrowed reach (`inspect`'s `pins` table) move with the narrowing: rewritten to the new call site when the assertion still matters, deleted with the internals when it does not. The plan names every one; a test change the plan does not name holds review.

The gate's wall clock is a program-level cost: a pass iterates against `cargo xtask gate`, and a gate near its 15-minute budget costs more per pass than any code shape does. Measure it at the start of the program and, if it is slow, treat speeding it as the first pass.

## Cadence and collision

Trunk moves at tens of commits a day, so a pass that takes more than a few days is rebasing against a moving target. Keep a pass to one contract and one module (or one seam), and to a week at most. When a pass collides with in-flight feature work in its scope, the plan names the branch and the merge order, and the pass waits for the feature rather than the other way round.

## What the ledger covers that atlas does not

The ledger carries four things the tool cannot yet express. Each is a candidate to retire into atlas when the tool grows the feature; until then the ledger section is authoritative.

- **Module verdicts.** Atlas has no module-level "reviewed and holds" record, so the rank re-surfaces the same large healthy module every run. The verdict table with its SHA and churn threshold is the demotion.
- **Admission intents.** A `[[module]]` admission in `refactor-target.toml` is a ratchet, not a decision, and the survey cannot tell a reviewed admission from an unreviewed one. The intents table marks each `keep` or `close`, so the largest structural findings (the upward sites out of `store`) exist as candidates.
- **Pass records.** `diff --expect` proves a pass but writes nothing durable. The pass log is that record.
- **Family dossiers.** `inspect` reads one module; a sibling family spans several. The seam queue row for a family collapse carries the member list and the divergences found so the next pass does not re-derive them.
