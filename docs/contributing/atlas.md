# Atlas: refactor analysis

`cargo xtask atlas` is the instrument for architecture-refactor programs: it locates where accretion cost concentrates, measures how deep each boundary really is, and turns "is this module done?" into a machine-checked answer. This page is the operating guide for agents running such a program; the command reference lives in [rust-conventions.md](./rust-conventions.md).

The premise: opportunity-driven refactoring never terminates. Atlas exists to run **target-driven** refactoring — a written target design fixed up front, passes measured as diffs against it, and `refactor-target.toml` ratcheting each landed pass so a finished module never needs re-review. A module is done when `conform` reports it clean and a ratchet guards it; reopening one requires a deliberate edit to the target file in its own commit.

## Program shape

1. **Round 0 — evidence and target.** Run `rank`, `seams`, `api`, and `shapes` over the scope. Write the from-scratch target (a page: modules, seams, what each hides from callers) plus the pass list with prerequisites and line budgets. Seed `refactor-target.toml` with `conform --init`, then narrow it to encode the target: shrink allow-lists, lower pub budgets, add stranglers for paths that must die.
2. **Each pass — one seam or one submodule.** Verticals take a submodule to its target shape; horizontals collapse one piece of knowledge repeated across many files (`shapes` clusters and `seams` divergence rows are the horizontal detectors). Horizontals usually go first: they fix the shape every vertical then conforms to. A pass exits when its `conform` rules are clean, net source lines fell, and behavior held.
3. **Ratchet.** `conform --ratchet` runs inside `gate`/`checks`; after improvement, `conform --tighten` locks the gains. Pace and delta columns (`--since`) are the burn-down chart between passes.

Modules flagged `pin` get characterization tests before their pass touches them — that is schedulable prerequisite work, visible from `rank` on day one.

## Reading the verbs

Every verb follows `--path`, prints top-N tables plus a totals line so truncation never hides mass, and emits versioned JSON with `--json`. Text output is bounded (~25–100 lines) and safe to print; route `--json` to a file under `/tmp` and narrow with `jq`.

### `rank` — where should I look?

One row per module: `code`, `pub`, `loc/pub`, `churn%`, `pace`, `cx`, `t/c`, `flags`. Sort order is churn-weighted size — accretion cost paid daily, not raw bulk.

- `loc/pub` is the depth proxy: implementation hidden per public item. High is deep; low with a wide `pub` count is over-publication.
- Flags are facts, not verdicts: `pin` (churny and under-tested — pin tests first), `hot` (pace ≥ 1.5×), `shallow` (wide, thin, low-use surface — deepen or fold into the caller), `hub` (wide, thin, high-use — a shared surface to treat deliberately).
- `—` in pace means too few commits to trust; the tail row aggregates everything below the fold.
- `--verbose` lists top offender functions — where several large entry points share a shape that `shapes` cannot see (see its blind spot below).

### `seams` — where are the seams, really?

Four sections, in rising value:

- **Import edges** and **external surface**: what each module must know from outside. A command module importing 100+ items from eight providers is the measurement of the missing context seam.
- **External providers**: fan-in ranking — which outside surfaces every pass will touch.
- **Co-change edges**: files that change together (window-scoped, commits touching more than `--max-commit-files` Rust sources omitted as merge noise).
- **Divergence** is the payload: `cochange-without-import` pairs are hidden knowledge duplication — two modules that must change together with no declared dependency. These are rehome candidates located for free; read the top rows before believing any target design.

Imports come from `use` items only; inline qualified paths are invisible here (they still count in `conform` stranglers and `api` occurrences).

### `api` — how deep is each boundary?

Per module: `pub fn`, `pub type`, `occ/item` (median outside-file identifier occurrences, tests excluded), `occ0` (items with zero such occurrences), `params/fn`. Then the shortlist of public items used by exactly one outside module — interface that arguably shouldn't be interface. `--module <name>` drills into one module's items with file:line.

`occ` is heuristic whole-word counting, not resolved callers: common names over-count, and `occ0` conflates dead with test-only. Treat a large `occ0` as a demote-or-delete *shortlist to read*, never a deletion list to execute.

### `shapes` — what repeats?

Clusters large functions by Jaccard similarity over shared domain callees (generic iterator/conversion/error methods excluded, three shared callees minimum). A cluster is a collapse candidate: the member list is the exact scope of a horizontal pass. Several small clusters sharing one provider's helpers usually indicate a single missing seam, not several — read them together.

Blind spot: functions with the same control flow but different callees (parallel command entry points, each orchestrating its own domain) never cluster here. Find those through `rank --verbose` cx offenders.

### `conform` — am I done?

Compares the tree against `refactor-target.toml`: per-module import allow-lists, pub budgets, and strangler symbol counts that must trend to zero. `--init` seeds a truthful baseline from the current tree and never overwrites; `--tighten` only lowers; `--ratchet` fails only on regression and is inert without a target file. Loosening any rule is a hand edit to the toml — that friction is the point.

Strangler entries make stalled migrations impossible to ignore: add one when a pass leaves old and new paths coexisting, with the old symbol's count as the baseline, and tighten it toward zero as callers migrate.

## Recipes

Round 0 evidence sweep over a scope:

```sh
cargo xtask atlas rank   --path crates/rimz/src/cli
cargo xtask atlas seams  --path crates/rimz/src/cli
cargo xtask atlas api    --path crates/rimz/src/cli
cargo xtask atlas shapes --path crates/rimz/src/cli
cargo xtask atlas conform --init --path crates/rimz/src/cli   # then narrow the toml to the target
```

Per-pass loop:

```sh
cargo xtask atlas conform                          # burn-down before planning
cargo xtask atlas api --path <scope> --module <m>  # item-level ground truth for the pass
# ... pass lands, gate passes ...
cargo xtask atlas conform --tighten                # lock the gains
cargo xtask atlas rank --path <scope> --since <baseline-ref>   # verify net-subtractive
```

## Judgment stays with the reader

Atlas locates; it does not decide. A `shallow` flag can be a module mid-migration, a divergence pair can be a legitimate product coupling, an `occ0` item can be a public contract used downstream. Before acting on any row, read the code and its history (`git log -S`), and let the finding earn its verb — collapse, delete, deepen, or rehome — from evidence, not from the table alone.
