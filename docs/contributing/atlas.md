# Atlas: refactor analysis

`cargo xtask atlas` is the instrument for target-driven architecture work. It builds one set of facts about the tree, presents those facts at several resolutions, and checks the result against `refactor-target.toml`. The operating loop is **survey → brief → target → conform**.

Opportunity-driven refactoring does not terminate. Atlas work starts by writing the intended boundary, gives each pass a bounded scope and line budget, and ends when `conform --ratchet` proves that boundary. Reopening a finished rule requires a deliberate target-file edit.

## The operating loop

1. **Survey the scope once.** Run `survey` to see ranked size and surface, seam divergence, API reference evidence, repeated shapes, and detector findings in one report. This is the shortlist, not the decision.
2. **Read one brief per candidate.** Run `brief --module <path>` for each candidate module. A brief keeps its callers, escaping items, co-change neighbors, shapes, and detector evidence together so a reviewer can distinguish a missing seam from legitimate coupling.
3. **Write the target before moving code.** State the layer order, admitted upward-import debt, exact allow-lists where a nested boundary needs one, surface budgets, stranglers, pass order, and line budget. Treat every proposed demotion or split as a hypothesis until the compiler and tests accept it.
4. **Conform and ratchet.** Implement one seam or submodule per pass. A pass exits when behavior holds, `conform --ratchet` is clean, and `rank --since <ref>` shows the intended complete-scope line delta. Run `conform --tighten` after an improvement to preserve it.

Modules flagged `pin` get characterization tests before their pass. Independent passes may use parallel worktrees only when their briefs share neither a co-change edge nor a prerequisite. A hand-off carries the relevant brief, target rules, expected post-pass budgets, and verification commands; it does not ask the executor to repeat the survey.

## One facts model

One atlas invocation loads working-tree Rust sources and syntax once into per-file and per-item facts. History, complexity, exact references, and blame are lazy facets requested by the selected verb; report rows are rollups over those facts, not separate analyses. `survey` and `brief` therefore compose the same evidence as the narrower verbs without recomputing it. Historical `--since` facts are loaded separately, while current facts always describe the working tree.

Test files and inline `#[cfg(test)] mod` regions are classified separately from production. Testkit support contributes to neither production nor test SLOC. Every table honors its scope, limits displayed rows with `--top`, and retains complete totals.

## The SCIP index

`rank`, `seams`, `api`, `survey`, and `brief` require exact references by default. Atlas asks `rust-analyzer` for a SCIP export, parses it with `scip`, and caches the result under `target/atlas/`, keyed by sorted source contents and `Cargo.lock`. A cold index on this workspace costs about 73 seconds and 6 GB; a matching cache avoids that cost. Install the pinned toolchain's component with `rustup component add rust-analyzer`, or provide `rust-analyzer` on `PATH`. A missing indexer fails at command entry with that fix.

`--no-index` is the explicit lower-cost mode. It keeps syntax-derived facts and `use` edges but omits every reference-derived column, shortlist, flag, and JSON field; it never substitutes name matching. `shapes` and `conform` never load the index, so the conform gate does not pay this cost.

SCIP definitions join to syntax items by file and identifier line, with the symbol descriptor used only to confirm the item name. An unmatched definition is reported as unresolved and excluded from reference medians; Atlas never fabricates a zero. Reference sets distinguish production from tests and count outside production modules rather than raw textual occurrences.

## Reading the reports

### `survey` — where is the program?

`survey --path <scope>` is the Round 0 report. It combines rank, seam, API, shape, and detector views from one facts load. Use `--md` for a durable review artifact or `--json` for a consumer; neither format changes the measurements. `--top` bounds displayed tables only.

Start with the largest churn-weighted rows, then follow divergence and callers before reading shapes. The report is designed to identify modules that deserve a brief, not to prescribe a refactor.

### `brief` — what is true about this module?

`brief --module <path>` is the dossier for one module at any depth. It keeps the module's surface and exact references beside its import edges, production callers, co-change neighbors, shapes, and detector findings. Use one brief per candidate and compare each to the proposed target, not to an abstract score.

### `rank` — where does accretion cost concentrate?

Rows report production and test SLOC, raw public items, effective escaping surface, `loc/esc`, rename-folded pace, churn, complexity, and flags. Sort order is churn-weighted size. `esc` measures items whose effective visibility leaves the row boundary; high `loc/esc` indicates a deeper implementation behind each escaping item.

Large directory rows split recursively by default above 8,000 production SLOC. The parent remains a subtotal; children are indented, do not consume `--top`, and never change the top-level totals. Use `--split-above <sloc>` to move the threshold or `--no-split` to retain one row per top-level module.

A wide, thin row has `esc >= 20` and `loc/esc < 120`:

- without an index it is `thin` because caller depth is unknown;
- with an index it is `shallow` when the median outside production reference count per resolved escaping item is below `--hub-refs` (default `2`);
- otherwise it is `hub`.

`pin` identifies churny, under-tested code; `hot` marks pace at least 1.5× the baseline. Flags are reading prompts, not verdicts. `--verbose` lists the functions driving complexity. With `--since`, overall deltas cover the complete scope, not only shown rows.

### `seams` — which boundaries already exist?

The import-edge table remains `use`-only because those are the edges target rules constrain. Divergence uses the union of `use` and exact reference edges, so inline qualified paths no longer hide coupling. `--module <name>` adds **callers**: production calling modules ranked by the number of distinct referenced items, with item names for the leading rows.

Co-change is computed in this order: fold renames to their HEAD paths, discard oversized commits, count file pairs, roll those pairs up to report rows (including root-dispatcher collapsing), then hide low-frequency rolled-up edges. `cochange-without-import` can expose duplicated knowledge; `import-without-cochange` can identify a stable seam or dead import. Read the item-level callers before deciding which.

### `api` — who uses the surface?

Per-module rows report items, escaping surface, reference medians, unresolved definitions, and exact reference classifications. `unref` means a resolved escaping item has no outside production referrer; `test-only` means it has test but no production referrers; `single` means exactly one outside production module refers to it. `--module <name>` lists each item with its declared and effective reach, resolution status, production and test reference modules/counts, and exact single caller when one exists.

The single-caller shortlist is a rehome candidate list, not permission to demote or move code. Macros or definitions absent from SCIP remain unresolved. The compiler remains the authority for visibility changes.

### `shapes` — what choreography repeats?

`shapes` clusters large functions by Jaccard similarity over domain callees. A cluster is a candidate for a horizontal seam; several clusters sharing a provider often indicate one missing abstraction. Parallel control flow with different callees does not cluster, so use `rank --verbose` for that case.

### Detectors — what deserves a closer read?

`survey` and `brief` add four focused shortlists:

- **Single caller:** an escaping, resolved item referenced by exactly one outside production module.
- **Pass-through:** a function whose body only forwards its parameters, in order, to one call after peeling ordinary `return`, `?`, `.await`, `Ok(...)`, or `Some(...)` wrappers. There is no score threshold.
- **Vestigial:** an escaping, resolved item's entire span blames to one commit older than the current pace-window start and has at most one outside production referrer.
- **Repeated guard:** the same normalized `if`, `while`, or match-arm guard of at least five tokens appears in at least `--guard-files 3` distinct files.

Pass-throughs suggest a seam that may not earn its layer; vestigial items suggest deletion or localization; repeated guards suggest policy knowledge that lacks a home. Confirm each in code and history.

### `conform` — does the tree match the target?

`conform` checks target schema v3: layer direction, admitted upward imports, exact allowed imports, escaping `surface-budget` values, and stranglers. It reads root `refactor-target.toml` unless `--file` selects another target. `--path` is accepted only with `--init`; normal checks use the paths encoded by the target.

`--init` writes a truthful current-tree baseline and never overwrites. It derives a low-to-high layer order from the current `use` graph unless `--layers a,b,c` supplies one. `--ratchet` fails on unadmitted upward imports and other regressions and is inert when no default target exists. `--tighten` atomically lowers budgets, removes stale admissions, and never loosens a rule.

## Target schema v3

`layers` list top-level crate modules from lower to upper. A module may import its own or a lower layer; importing a higher layer is an `upward-import`. `(crate)` may name root-declared items. A top-level name absent from `layers` is unconstrained.

Most module rules use `upward-imports` to admit known debt additively while retaining the layer rule. A hand-written nested rule may instead use `allowed-imports`, which is an exact allow-list and overrides layer checking for that rule. A rule may not contain both.

```toml
version = 3
layers = ["ids", "store", "agents", "cli"]

# Store is intentionally low. This records one known upward dependency while
# every other higher-layer import remains a regression.
[[module]]
path = "crates/rimz/src/store"
upward-imports = ["agents::state"]
surface-budget = 12

# This nested migration has a deliberately narrower, exact boundary.
[[module]]
path = "crates/rimz/src/cli/legacy"
allowed-imports = ["agents", "ids"]
surface-budget = 4

[[strangler]]
symbol = "LegacyStoreBridge"
path = "crates/rimz/src/cli"
baseline = 0
```

The first rule still permits lower-layer imports and admits only the named upward prefix. The second bypasses layer direction and permits exactly its listed prefixes. Before implementation, `conform` reports upward or unallowed import sites, excess surface, and strangler occurrences; after the pass, ratchet and tighten preserve the result.

## JSON v3

Every JSON report carries `version: 3` and `verb`; scoped analysis also carries its path and parse failures. Totals always describe the complete result even when displayed row arrays honor `--top`. `survey` and `brief` return their sections in one untruncated v3 payload. Reference-derived fields are optional and are omitted under `--no-index`, not filled with heuristic values.

Important per-verb additions are recursive `rank.rows[].children` and optional `ref_median`; `seams.callers` with calling module, distinct-item count, and item names; and `api` resolution plus production/test reference module fields and exact single-caller arrays. `conform` reports layers and `upward-import` findings. Schema v2 name-match and over-publication fields do not exist in v3.

## A worked reading chain

Suppose `survey` reports a `cochange-without-import` divergence across `claude <> codex <> kiro <> cursor`. Open a brief on their common adapter parent rather than assigning four directory-local cleanups. If its callers show the same outside consumers and `shapes` groups their `decode_hook` functions, the evidence supports one **collapse** candidate: centralize hook decoding behind a provider-neutral seam, then make each adapter translate only provider-specific input. Encode that destination in layers, admissions, and budgets before moving code; use `rank --since` and `conform --ratchet` to judge the pass.

The chain matters: divergence locates shared hidden knowledge, callers identify the boundary's consumers, shapes identify repeated choreography, and the target states the desired dependency direction. No single row establishes the refactor.

## Recipes

```sh
# One Round 0 artifact.
cargo xtask atlas survey --path crates/rimz/src --md > /tmp/atlas-survey.md

# One focused dossier per shortlisted module.
cargo xtask atlas brief --module crates/rimz/src/agents/adapters --md

# Seed once, then edit the generated baseline into the intended design.
cargo xtask atlas conform --init --path crates/rimz/src

# Per-pass proof.
cargo xtask atlas conform --ratchet
cargo xtask atlas conform --tighten
cargo xtask atlas rank --path <scope> --since <baseline-ref>
```

Atlas counts SLOC and separates tests; `git diff --stat` counts physical changed lines and answers a different question.

## Judgment stays with the reader

Atlas locates; it does not decide. Exact references remove the old whole-word heuristic, but they do not settle ownership, behavior, or whether a seam is worth its cost. A `shallow` row can be mid-migration, a divergence can be legitimate coupling, a pass-through can be a useful policy boundary, and a single caller can be intentionally isolated.

Keep the crate-ownership trap live. SCIP symbols name a package, not the Rust target that owns a module tree. In RimZ, `lib.rs` does not declare `cli`; `main.rs` owns `src/cli/**` while consuming the library as another crate. Atlas therefore cannot infer every `pub` → `pub(crate)` opportunity from references alone. Check Cargo targets and `mod` roots, search cross-crate consumers, and let rustc decide visibility.

Read code and history, run the compiler feasibility pass, and let each finding earn its action—collapse, delete, deepen, or rehome—from evidence rather than a score.
