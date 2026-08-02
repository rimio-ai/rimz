# Atlas: refactor analysis

`cargo xtask atlas` is the instrument for architecture-refactor programs: it locates where accretion cost concentrates, measures boundary depth, and turns "is this module done?" into a machine-checked answer. This page is the operating guide; the command reference lives in [rust-conventions.md](./rust-conventions.md).

The premise: opportunity-driven refactoring never terminates. Atlas supports **target-driven** refactoring — fix a target design up front, measure each pass against it, and ratchet `refactor-target.toml` so finished boundaries stay finished. A module is done when `conform` reports it clean and a ratchet guards it; reopening a rule requires a deliberate target-file edit.

## Program shape

1. **Pass 0 — shortlist, then ask rustc.** Use `api` to find over-published and narrowly used items, but treat every suggested demotion as a hypothesis. Demote a batch and run the all-target compiler recipe below; rustc, not the name scan, decides what may stay private.
2. **Round 0 — evidence, feasibility, and target.** Run `rank`, `seams`, `api`, and `shapes` over the scope. Group deep-reading assignments by co-change cluster rather than directory. Write the from-scratch target (modules, seams, what each hides) plus ordered passes and line budgets. Before implementation, batch dependency direction, visibility, line-ledger, and behavior-ordering hypotheses into one feasibility pass. A supplied budget remains a hypothesis until that compiler pass confirms it. Seed target schema v2 with `conform --init`, then narrow allow-lists and `surface-budget` values and add stranglers for paths that must die.
3. **Each pass — one seam or one submodule.** Verticals take a submodule to its target shape; horizontals collapse knowledge repeated across files. Horizontals usually go first because they establish the shape each vertical must follow. A pass exits when its `conform` rules are clean, the `rank --since` totals are net-subtractive, and behavior holds.
4. **Ratchet.** `conform --ratchet` runs inside `gate` and `checks`; after improvement, `conform --tighten` locks the gains. Pace and totals deltas are the burn-down chart between passes.

Modules flagged `pin` get characterization tests before their pass touches them — schedulable prerequisite work visible on day one.

**Concurrency.** Passes may run in parallel worktrees when their scopes share no co-change edge in `seams`; a pass whose prerequisite has not landed waits. Each pass narrows only its own target rules. A delegated hand-off carries the target slice, owned rules with post-pass values, line budget, and verification commands; the executor verifies the assumptions against code but does not redo Round 0.

## Reading the verbs

Every verb follows `--path`, prints bounded top-N tables plus complete totals, and emits atlas JSON v2 with `--json`. Redirect JSON to `/tmp` and narrow it with `jq`. Test files are excluded from production measurements; `syn`-derived inline `#[cfg(test)] mod` regions contribute to `rank`'s test SLOC and to `api`'s separate test evidence wherever they appear in a file. Testkit support contributes to neither production nor test SLOC.

### `rank` — where should I look?

One row per scoped module: `code`, raw `pub`, escaping `esc`, `loc/esc`, `churn%`, `pace`, `cx`, `t/c`, and `flags`. Sort order is churn-weighted size — accretion cost paid daily, not raw bulk. `cx` sums severity-weighted cognitive, cyclomatic, and source-line threshold overruns.

- `esc` counts items whose effective visibility leaves the row's module boundary. Raw `pub` remains beside it so internal file plumbing cannot masquerade as external surface.
- `loc/esc` is the depth proxy: implementation hidden per escaping item. High is deep; low with a wide `esc` count is shallow.
- Flags are facts, not verdicts: `pin` (churny and under-tested), `hot` (pace at least 1.5×), `shallow` (wide, thin, low name use), and `hub` (wide, thin, high name use).
- `—` in pace means too few commits to trust. `--verbose` lists top offender functions.
- With `--since`, the overall line carries `Δcode`, `Δtests`, `Δpub`, and `Δesc` summed across the complete scope, not only the displayed top-N rows.

### `seams` — where are the seams, really?

Four sections, in rising value:

- **Import edges** and **external surface** show what each module knows from outside.
- **External providers** rank fan-in — the outside surfaces every pass will touch.
- **Co-change edges** show files that change together. Oversized commits are omitted as merge noise and low-frequency pairs are hidden.
- **Divergence** is the payload. `cochange-without-import` pairs indicate duplicated hidden knowledge; `import-without-cochange` often indicates a stable seam, occasionally a dead `use`.

Imports come from `use` items only; inline qualified paths are invisible. `--module <name>` expands import edges into distinct imported item names.

### `api` — how deep is each boundary?

Per module: `items`, `esc`, `over`, `test-only`, `unref`, `name-occ`, and `params/fn`. `esc` is effective surface escaping the table row; `over` counts assessed items published more broadly than production name evidence implies. Crate-external reach is not assessed for demotion. `name-occ` is the median production whole-word match count. The single name-caller module shortlist answers where code might belong, not whether it is safe to demote.

`--module <name>` prints each item's declared visibility, effective reach, implied reach, production/test name-match counts, and tags. Effective reach is the innermost confinement across the declaration and every enclosing `mod` link: `(extern)` means the declaration permits use outside the crate and carries an `external` tag, the empty module path means crate-wide, and a `pub fn` behind `mod detail;` cannot escape the parent merely because its own token says `pub`. `external` means not assessed for `over_published`, rather than assessed and clean. A same-file signature hop carries a function's implied reach to types named in its signature, covering cases such as an inferred return type.

The evidence remains deliberately heuristic:

- Tests are separate evidence, not absent evidence. A `test-only` item may be necessary to `cargo check --all-targets`.
- Common names in code, comments, and strings over-count. That can only overstate required reach, so it is conservative.
- Type inference, macros, and unresolved cross-file relationships can under-count. That can understate required reach, so it is unsafe.

Only rustc closes the unsafe gap. `over_published` is a demotion shortlist, never an instruction.

### `shapes` — what repeats?

Clusters large functions by Jaccard similarity over shared domain callees. A cluster is a collapse candidate and its member list is the scope of a horizontal pass. Several clusters sharing one provider often indicate one missing seam. Functions with parallel control flow but different callees do not cluster; find those through `rank --verbose`.

### `conform` — am I done?

Compares the tree with target schema v2: per-module import allow-lists, `surface-budget`, and strangler symbol counts. A surface budget counts items whose effective reach escapes the rule's own module path; `pub(super)` plumbing inside the rule stops consuming budget, while `pub(in crate::cli)` still consumes a feature-level rule budget. A re-export is surface where the re-export is declared.

`--init` seeds a truthful current-tree baseline and never overwrites; `--tighten` only lowers; `--ratchet` fails only on regression and is inert without a target. The default report folds rules exactly at budget, while `--verbose` restores them. Loosening a rule is a hand edit — that friction is intentional.

Stranglers keep stalled migrations visible: add one when old and new paths coexist, then tighten its baseline toward zero as callers move.

## From baseline to target

Suppose `conform --init --path crates/rimz/src/cli` records a legacy command with eight items escaping its own path and two providers:

```toml
version = 2

[[module]]
path = "crates/rimz/src/cli/legacy"
allowed-imports = ["agents", "store"]
surface-budget = 8
```

For a pass that moves store knowledge behind the agents seam, encode the completed shape and the old bridge's removal:

```toml
version = 2

[[module]]
path = "crates/rimz/src/cli/legacy"
allowed-imports = ["agents"]
surface-budget = 4

[[strangler]]
symbol = "LegacyStoreBridge"
path = "crates/rimz/src/cli"
baseline = 0
```

Before implementation, `conform` reports excess escaping surface, forbidden import sites, and bridge occurrences. Afterward, `conform --ratchet` proves the shape, `conform --tighten` preserves extra improvement, and `rank --since <pre-pass-ref>` proves whether the complete pass was net-subtractive.

## JSON v2 contracts

Field names are stable snake_case. All reports carry `version` and `verb`; analysis reports also carry `path` and `parse_failures`. Totals describe the complete result even when row arrays are top-N.

- `rank`: `history_commits`, `total_modules`, `total_code`, `total_tests`, `total_pub_items`, `total_escaping_items`, `total_complexity`, `rows`, and optional `delta_code`, `delta_tests`, `delta_pub`, `delta_esc`, and `offenders`. Rows expose the text columns, `tests`, `name_match_median`, and optional row deltas.
- `seams`: history bounds; totals and arrays for `import_edges`, `external_surface`, `external_providers`, `cochange_edges`, and `divergence`; optional `cochange_hub`; per-kind divergence totals; and optional `requested_module` plus untruncated `import_items`.
- `api`: `total_modules`, `modules`, `total_single_name_caller_items`, `single_name_caller_modules`, and `single_name_caller_items`. Module rows contain `items`, `escaping_items`, `over_published_items`, `test_only_items`, `unreferenced_items`, `name_match_median`, `params_median`, and optional deltas. With `--module`, `module_items` carries `name`, `kind`, `module`, `path`, `line`, `declared_visibility`, `effective_reach`, `implied_reach`, `escapes_module`, `over_published`, `test_only`, `unreferenced`, and production/test name-match counts and module lists. An `effective_reach` of `(extern)` records declared reach beyond the crate; it does not claim a downstream consumer exists.
- `shapes`: `eligible_functions`, `total_clusters`, and `clusters`; each cluster includes similarity, score, breadth, shared callees, and member locations/SLOC.
- `conform`: `target`, `rules`, `regressions`, and `parse_failures`. Module rules include unallowed imports and sites; an absent default target instead returns `configured: false`.

## Recipes

Round 0 evidence sweep:

```sh
cargo xtask atlas rank   --path crates/rimz/src/cli
cargo xtask atlas seams  --path crates/rimz/src/cli
cargo xtask atlas api    --path crates/rimz/src/cli
cargo xtask atlas shapes --path crates/rimz/src/cli
cargo xtask atlas conform --init --path crates/rimz/src/cli
```

Demote-and-check loop:

```sh
cargo xtask atlas api --path <scope> --module <m> --json > /tmp/api.json
jq -r '.module_items[] | select(.over_published and .test_name_matches == 0) | "\(.path):\(.line)\t\(.name)"' /tmp/api.json
# Demote the batch, then let the arbiter rule:
RUSTFLAGS="-D warnings" cargo check --workspace --all-targets --all-features
# Revert every item rustc names; keep the rest; repeat until green.
```

Run `test_only` candidates as a deliberate second pass: narrowing them also means updating the tests that consume them, rather than treating test evidence as permission to remove reach.

`api` does not propose `pub` → `pub(crate)`: its workspace-wide name corpus deliberately folds workspace crates into one module namespace, so it cannot prove crate-external need. To hand-build an investigation list, not a demotion batch:

```sh
jq -r '.module_items[] | select(.effective_reach == "(extern)" and ((.production_name_modules | length) <= 1)) | "\(.path):\(.line)\t\(.name)"' /tmp/api.json
```

A referrer in another workspace crate is indistinguishable from an in-crate referrer in this list. Sweep every candidate across crates with `rg`, then let rustc decide. The crate-root `conform` rule, not this hand list, guards the public boundary.

Per-pass target and line ledger:

```sh
cargo xtask atlas conform
# ... pass lands and the gate passes ...
cargo xtask atlas conform --tighten
cargo xtask atlas rank --path <scope> --since <baseline-ref>
```

The `rank --since` overall deltas are the authority for "was this pass net-subtractive?" Atlas counts SLOC and separates tests; `git diff --stat` counts physical changed lines and answers a different question.

## Judgment stays with the reader

Atlas locates; it does not decide. Keep these traps live while interpreting it:

- Name matches include comments and string literals.
- The name corpus spans the whole repository even when report rows are `--path`-scoped.
- Report row labels and name-corpus file-module buckets are different groupings.
- Atlas reads declared reach, not downstream use: `(extern)` means `pub` permits use outside the crate, while the name corpus does not inspect dependent packages.
- Crate-aware implied reach is a whole-instrument model question: it needs Cargo-target ownership from `[lib]`/`[[bin]]` roots and their `mod` trees, plus a decision about keeping the flat workspace namespace used by seams and conform. RimZ is the concrete trap: `lib.rs` never declares `mod cli;`; `main.rs` owns all of `src/cli/**` while consuming the library as an external crate.

A `shallow` flag can be a module mid-migration and a divergence pair can be legitimate product coupling. Read the code and history (`git log -S`), run the compiler feasibility pass, and let each finding earn its action — collapse, delete, deepen, or rehome — from evidence rather than a table.
