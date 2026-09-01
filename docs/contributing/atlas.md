# Atlas: refactor analysis

`cargo xtask atlas` is the instrument for target-driven architecture work. It builds one set of facts about the tree, presents those facts at several resolutions, and checks the result against `refactor-target.toml`. The operating loop is **survey → brief → target → conform**.

Opportunity-driven refactoring does not terminate. Atlas work starts by writing the intended boundary, gives each pass a bounded scope and line budget, and ends when `conform --ratchet` proves that boundary. Reopening a finished rule requires a deliberate target-file edit.

## Vocabulary

Every report uses these terms; the rest of this page assumes them.

- **Escaping (`esc`)** — an item whose *effective* visibility leaves the row's module boundary: a `pub` item re-exported from the crate root escapes, a `pub` item inside a private module does not. `pub` is what the source declares; `esc` is what a caller can reach. Every budget and detector counts `esc`, never raw `pub`.
- **Churn%** — the share of scoped history commits that touched the module, after folding renames to their HEAD paths. `rank` sorts by `code × churn%`.
- **Pace** — the module's share of commits in the recent window divided by its share of lifetime commits, so `1.0` means the module is changing at its historical rate and `hot` (≥ 1.5) means it is accelerating. The window is the most recent `--window` percent of scoped commits (default 25). Modules under `--noise-lifetime` lifetime or `--noise-window` window commits report no pace.
- **Window start** — the commit time where the pace window begins. Detectors that speak of "older than the window" mean older than this point, which on a young repository is only weeks ago.
- **Production / tests** — test files and inline `#[cfg(test)] mod` regions are classified apart from production; testkit support counts toward neither. Reference counts distinguish production referrers from test referrers, and count outside modules rather than textual occurrences.

## The operating loop

1. **Survey the scope once.** Run `survey` to see ranked size and surface, repeated shapes, seam divergence, API reference evidence, and detector shortlists in one report. Read the rank table and the shapes first; the section on [reading the detectors](#detectors--annotations-not-shortlists) explains why the shortlists are read last and never at face value. This is the reading queue, not the decision.
2. **Read one brief per candidate.** Run `brief --module <path>` for each candidate module. Its **Callers by assembly** table is the primary evidence: how many distinct items each outside module must assemble to use this one. Read that beside the providers, co-change neighbours, shapes, and conform rules before deciding whether a wide interface is a missing seam or legitimate coupling.
3. **Write the target before moving code.** State the layer order, admitted upward-import debt, exact allow-lists where a nested boundary needs one, surface budgets, stranglers, pass order, and line budget. Treat every proposed demotion or split as a hypothesis until the compiler and tests accept it. The [target section](#baseline-versus-target) says what the file can and cannot record.
4. **Conform and ratchet.** Implement one seam or submodule per pass. A pass exits when behaviour holds, `conform --ratchet` is clean, and `rank --since <ref>` shows the intended complete-scope line delta. Run `conform --tighten` after an improvement to preserve it.

Modules flagged `pin` get characterization tests before their pass. Independent passes may use parallel worktrees only when their briefs share neither a co-change edge nor a prerequisite. A hand-off carries the relevant brief, target rules, expected post-pass budgets, and verification commands; it does not ask the executor to repeat the survey.

## What Atlas cannot measure

Atlas counts and cross-references; it does not read call sites. The one question that decides most depth findings — what the heaviest caller must construct, wire, and sequence to reach the common case — starts from the assembly table and ends in the caller's source. Quote that call site in the finding; the number alone is not the evidence. Likewise, whether an old item is a paid-for fix or dead weight is answered by `git log -S'<symbol>'` and the introducing commit's subject, not by its age.

## One facts model

One atlas invocation loads working-tree Rust sources and syntax once into per-file and per-item facts. History, complexity, exact references, and blame are lazy facets requested by the selected verb; report rows are rollups over those facts, not separate analyses. `survey` and `brief` therefore compose the same evidence as the narrower verbs without recomputing it. Historical `--since` facts are loaded separately, while current facts always describe the working tree.

Every table honours its scope, limits displayed rows with `--top`, and retains complete totals. Complexity (`cx`) needs `rust-code-analysis-cli` (`cargo install rust-code-analysis-cli --locked`).

## The SCIP index

`rank`, `seams`, `api`, `survey`, and `brief` require exact references by default. Atlas asks `rust-analyzer` for a SCIP export, parses it with `scip`, and caches the result under `target/atlas/`, keyed by sorted source contents and `Cargo.lock`. A cold index on this workspace costs about 73 seconds and 6 GB; with a warm cache `survey` runs in about 15 seconds and `brief` in about 5. Install the pinned toolchain's component with `rustup component add rust-analyzer`, or provide `rust-analyzer` on `PATH`. A missing indexer fails at command entry with that fix. If `rust-analyzer` panics during export (a known upstream inlay-hint inference bug), Atlas retries once with cache priming serialized and otherwise reports both attempts; it never substitutes heuristics.

`--no-index` is the explicit lower-cost mode. It keeps syntax-derived facts and `use` edges but omits every reference-derived column, shortlist, flag, and JSON field; it never substitutes name matching. `shapes` and `conform` never load the index, so the conform gate does not pay this cost.

SCIP definitions join to syntax items by file and identifier line, with the symbol descriptor used only to confirm the item name. An unmatched definition is reported as unresolved and excluded from reference medians; Atlas never fabricates a zero. Public re-exports and module declarations commonly lack their own SCIP definitions, so inspect unresolved counts before reading a module median as complete.

## Reading the reports

### `survey` — where is the program?

`survey --path <scope>` is the first-pass report over a scope. It combines rank, shapes, seam, API, and detector views from one facts load. Use `--md` for a durable review artifact or `--json` for a consumer; neither format changes the measurements. `--top` bounds displayed top-level rows only; split children are always shown, so the markdown for the whole crate runs to several hundred lines. Redirect it to a file and read sections, as [AGENTS.md](../../AGENTS.md#implementation-rules) says for any bulk output.

Read in this order: the largest churn-weighted rank rows, then the shapes clusters, then the `cochange-without-edge` divergence rows and the callers behind them. The detector sections come last. The report identifies modules that deserve a brief; it does not prescribe a refactor.

### `brief` — what is true about this module?

`brief --module <path>` is the dossier for one module at any depth; `brief --all --out-dir <dir>` writes one per split leaf. Its sections, in reading order:

- **Callers by assembly** — each outside production module with the count and names of distinct items it references. A caller needing dozens of items to do one job is paying for a seam the module does not provide; this is the strongest depth signal Atlas produces, and the row to open in the source.
- **Providers** — what this module imports from whom, by item name. For a low layer, every provider above it is a candidate `rehome`.
- **Co-change and divergence**, **Shapes**, and the detector sections, scoped to the module.
- **Conform rules** — the target rules whose path covers this module.

The **Interface** listing at the top duplicates `api --module` and is long (several hundred lines for a large module); skip it unless you are auditing visibility item by item.

### `rank` — where does accretion cost concentrate?

Rows report production and test SLOC, raw public items, escaping surface, `loc/esc`, churn, pace, complexity, test/code ratio, and flags. Sort order is churn-weighted size. High `loc/esc` indicates a deeper implementation behind each escaping item; read it relative to the other rows of the same table rather than against a fixed number.

Large directory rows split recursively by default above 8,000 production SLOC. The parent remains a subtotal; children are indented, do not consume `--top`, and never change the top-level totals. Use `--split-above <sloc>` to move the threshold or `--no-split` to retain one row per top-level module.

A wide, thin row has `esc >= 20` and `loc/esc < 120`:

- without an index it is `thin` because caller depth is unknown;
- with an index it is `shallow` when the median outside production reference count per resolved escaping item is below `--hub-refs` (default `2`);
- otherwise it is `hub`.

`pin` identifies churny, under-tested code; `hot` marks pace at least 1.5. Flags are reading prompts, not verdicts. `--verbose` lists the functions driving complexity. With `--since <ref>`, every row gains `Δcode`/`Δpub` and the overall line covers the complete scope, not only shown rows; this is the per-pass line arithmetic.

### `shapes` — what choreography repeats?

`shapes` clusters functions of at least `--min-sloc` lines by Jaccard similarity over domain callees, after dropping generic iterator, conversion, and error-context methods. A cluster is a `collapse` candidate; several clusters sharing a provider often indicate one missing abstraction. Functions with the same name may land in two clusters when the similarity threshold splits them — read clusters sharing a name as one finding. Parallel control flow with different callees does not cluster, so use `rank --verbose` for that case. Each cluster prints as one JSON line inside the markdown fence.

### `seams` — which boundaries already exist?

The import-edge table remains `use`-only because those are the edges target rules constrain. Divergence uses the union of `use` and exact reference edges, so inline qualified paths no longer hide coupling. `--module <name>` adds **callers**: production calling modules ranked by the number of distinct referenced items, with item names for the leading rows.

Co-change is computed in this order: fold renames to their HEAD paths, discard oversized commits, count file pairs, roll those pairs up to report rows (including root-dispatcher collapsing), then hide low-frequency rolled-up edges. Two divergence kinds result:

- `cochange-without-edge` — modules that change together without importing each other. This is the interesting direction: it can expose duplicated knowledge. Open the brief on their common parent.
- `edge-without-cochange` — an import that never co-changes. Usually a stable seam, sometimes a dead import; it is good news far more often than a finding, and on a mature tree it fills most of the divergence table.

The **co-change reading assignments** line lists connected components of the co-change graph; on a tightly coupled tree it collapses to one component containing every module and carries no information.

### `api` — who uses the surface?

Per-module rows report items, escaping surface, reference medians, unresolved definitions, and exact reference classifications. `unref` means a resolved escaping item has no outside production referrer; `test-only` means it has test but no production referrers; `single` means exactly one outside production module refers to it. `--module <name>` lists each item with its declared and effective reach, resolution status, production and test reference modules/counts, and exact single caller when one exists.

The single-caller list is a rehome candidate list, not permission to demote or move code. Macros or definitions absent from SCIP remain unresolved. The compiler remains the authority for visibility changes.

### Detectors — annotations, not shortlists

`survey` and `brief` add four detector sections. Each is defined mechanically and reported without a score threshold, so the totals are large and the precision is low; treat a detector hit as an annotation on an item you are already reading, not as a queue to work through. The `--json` payload carries `detector_counts` per module so you can see how much of the surface each one flags before trusting its top rows.

- **Single caller:** an escaping, resolved item referenced by exactly one outside production module. In a layered tree one production caller is the *normal* state of a domain function, so this flags roughly a third of escaping surface. It is useful as the `single` column of `api --module` and as evidence that a group of items belongs to their one caller; it is not useful ranked.
- **Pass-through:** a function whose body only forwards its parameters, in order, to one call after peeling ordinary `return`, `?`, `.await`, `Ok(...)`, or `Some(...)` wrappers. About half the hits are typed boundaries worth keeping — a `serde_json::from_slice` wrapped in a domain error type, a serde `visit_borrowed_str` forwarding to `visit_str`. The real hits are free functions forwarding to a method on a type the caller could hold directly.
- **Vestigial:** an escaping, resolved item whose entire span blames to one commit older than the window start and that has at most one outside production referrer. Read this as **stable**, not vestigial: on a repository a few months old the window start is weeks back, and the detector flags close to half the escaping surface — `*Args` structs, `Result` type aliases, error enums, capability structs, all live. An item earns a deletion read only when its production referrer count is zero *and* `git log -S` on its name shows no fix or incident in the introducing commit.
- **Repeated guard:** the same normalized `if`, `while`, or match-arm guard of at least five tokens appears in at least `--guard-files 3` distinct files. The top rows (`err.kind() == NotFound` across dozens of files, `Instant::now() >= deadline`) are policy knowledge without a home and are usually the cheapest `rehome` in the tree.

### `conform` — does the tree match the target?

`conform` checks target schema v3: layer direction, admitted upward imports, exact allowed imports, escaping `surface-budget` values, and stranglers. It reads root `refactor-target.toml` unless `--file` selects another target. `--path` is accepted only with `--init`; normal checks use the paths encoded by the target.

`--init` writes a truthful current-tree baseline and never overwrites. It derives a low-to-high layer order from the current `use` graph unless `--layers a,b,c` supplies one. `--ratchet` fails on unadmitted upward imports and other regressions and is inert when no default target exists. `--tighten` atomically lowers budgets, removes stale admissions, and never loosens a rule. `--verbose` shows every rule instead of folding the ones exactly at budget.

## Baseline versus target

`--init` produces a **baseline**: every budget equals the current count, every existing upward import is admitted, and the layer order is whatever topological sort the `use` graph allows. A baseline is a regression guard, and that is what the gate runs: `checks` and `gate` call `conform --ratchet`, so any pass that widens a surface or adds an upward import must edit the target file in a deliberate commit. That friction is the point.

A baseline is not a design. The generated `layers` array is a total order over every top-level module, and the `upward-imports` admissions on a low layer are exactly the imports a from-scratch design would not have. Read the admissions as the debt queue: each named prefix on `store` or `config` is a `rehome` candidate, and the passes that close them are the program. The file cannot yet record a destination that differs from the present without turning the gate red, so the destination lives in the hand-off plan: name the admission or budget the pass will remove, close it in that pass, and let `--tighten` write the new truth. Removing an admission by hand before the pass is done makes `ratchet` fail on every commit in between; do not.

Writing a real target means supplying `--layers` with a handful of layers a human chose rather than accepting the derived order, and it means every later target edit is either `--tighten` output or a reviewed loosening with a reason.

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

Every JSON report carries `version: 3` and `verb`; scoped analysis also carries its path and parse failures. Totals always describe the complete result even when displayed row arrays honour `--top`. `survey` and `brief` return their sections in one untruncated v3 payload, including `detector_counts` per module. Reference-derived fields are optional and are omitted under `--no-index`, not filled with heuristic values.

Important per-verb additions are recursive `rank.rows[].children` and optional `ref_median`; `seams.callers` with calling module, distinct-item count, and item names; and `api` resolution plus production/test reference module fields and exact single-caller arrays. `conform` reports layers and `upward-import` findings. Schema v2 name-match and over-publication fields do not exist in v3.

## A worked reading chain

Suppose `shapes` clusters the `decode_hook` functions of three adapters, and a second cluster holds two more. Open a brief on their common adapter parent rather than assigning five directory-local cleanups. If its callers-by-assembly table shows the same outside consumers reaching the same items, and the divergence rows show the adapters co-changing without importing each other, the evidence supports one **collapse** candidate: centralize hook decoding behind a provider-neutral seam, then make each adapter translate only provider-specific input. Encode that destination in the hand-off as the budgets and admissions the pass will remove; use `rank --since` and `conform --ratchet` to judge the pass, and `conform --tighten` to record it.

The chain matters: shapes locate repeated choreography, assembly identifies the boundary's consumers, divergence locates shared hidden knowledge, and the target states the desired dependency direction. No single row establishes the refactor.

## Recipes

```sh
# One first-pass artifact; read it in sections.
cargo xtask atlas survey --path crates/rimz/src --md > /tmp/atlas-survey.md

# One focused dossier per shortlisted module.
cargo xtask atlas brief --module crates/rimz/src/agents/adapters --md

# Exact callers for one item, once a brief has named it.
cargo xtask atlas api --module store --top 40

# Seed once with a chosen layer order, then edit the baseline into the intended design.
cargo xtask atlas conform --init --path crates/rimz/src --layers ids,store,agents,harness,cli

# Per-pass proof.
cargo xtask atlas conform --ratchet
cargo xtask atlas conform --tighten
cargo xtask atlas rank --path <scope> --since <baseline-ref>
```

Atlas counts SLOC and separates tests; `git diff --stat` counts physical changed lines and answers a different question.

## Judgment stays with the reader

Atlas locates; it does not decide. Exact references remove the old whole-word heuristic, but they do not settle ownership, behaviour, or whether a seam is worth its cost. A `shallow` row can be mid-migration, a divergence can be legitimate coupling, a pass-through can be a useful policy boundary, a single caller can be intentionally isolated, and a vestigial hit is usually just old.

Keep the crate-ownership trap live. SCIP symbols name a package, not the Rust target that owns a module tree. In RimZ, `lib.rs` does not declare `cli`; `main.rs` owns `src/cli/**` while consuming the library as another crate. Atlas therefore cannot infer every `pub` → `pub(crate)` opportunity from references alone. Check Cargo targets and `mod` roots, search cross-crate consumers, and let rustc decide visibility.

Read code and history, run the compiler feasibility pass, and let each finding earn its action — collapse, delete, deepen, or rehome — from evidence rather than a score.
