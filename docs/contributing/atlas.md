# Atlas: refactor analysis

`cargo xtask atlas` produces Markdown evidence for architecture review. Its five commands survey a scope, inspect a module, list raw index occurrences, prove a pass, and keep target constraints from regressing. Atlas locates and sizes; reading decides. Every section below is tagged **finding** (a row is a candidate on its own) or **evidence** (a row needs a reader), and the [vocabulary](#vocabulary) at the end defines the terms the output does not explain.

## Review workflow

The verbs map onto an architecture review pass, and the pass contract is where the review's plan meets the coder's proof.

1. **Survey and size.** `survey` on the crate. The probes join every signal per module and name the next command; take the pick to the user as confirm-or-redirect. `assemblers` names the functions that wire several modules together, the caller-side candidates the per-module probes cannot see. The admitted-dependency table and the `layers` in `refactor-target.toml` are the repository's recorded ratchets; a `[[verdict]]` is where a recorded decision lives, so read those before judging.
2. **Learn what it does.** `inspect --module <target> --brief --out /tmp/<target>.md` is the dossier a subagent brief carries; `--json --out /tmp/<target>.json` then `jq` narrows the full report. Read it in section order: the verdict for wide or deep, the record for what the module claims to be, then the heaviest quote for the call-site test. A shape proves reach, not assembly: the quote (whole under 80 lines) shows whether the caller constructs and wires the module's internals or merely names several of its items; brief a subagent only when it elides the part that decides. `--item <module::Name>` on each candidate before calling it deletable: its introducing commits and fix markers are the history check. `--from <caller>` re-quotes for a caller the `assemblers` table named.
3. **Plan.** Write each candidate's proof as a contract row — `deepen` as an `[[assembly]]` row on the caller that must get lighter (the call-site proof; an `[[esc]]` ceiling beside it only when names leave the surface, since narrowing visibility alone satisfies `esc` without changing any call), `delete` as `[[delete]]` rows, `rehome` as `[[rehome]]` for the item or the logic that moves and `[[dependency]]` for the seam it closes, `collapse` as `[[delete]]` rows on the siblings that die plus the SLOC delta — and put the contract in the plan verbatim so the coder runs `diff --expect` and the reviewer reads drift instead of re-deriving it:

```toml
version = 2
# base: pin the merge-base SHA; changed-path checking diffs against the ref as written
base = "<merge-base SHA>"
paths = ["crates/rimz/src/message", "crates/rimz/src/cli/agents_cmd"]
max-production-sloc-delta = -60

[[esc]]
path = "crates/rimz/src/message"
max = 120

[[delete]]
item = "message::queue_synthetic"

[[dependency]]
from = "message"
to = "store"
max-sites = 12
```

When logic leaves a function that stays (a CLI entry whose workflow moves to its library owner), write the logic form of `[[rehome]]` with `from`, `to` and `min-decisions` instead of an item row ([logic rehome](#logic-rehome)).

4. **Prove.** `diff --expect <contract>` after the pass; `conform --tighten --only <owned-path>` once it lands (repeat `--only` for each owned path), so the ceilings the pass earned stay earned without changing another pass's rules.

## `survey` — map a scope

`survey` reads the working tree and history without building a SCIP index. `--path` defaults to `crates/rimz/src`; `--top` to 20. A narrower `--path` ranks its own rows under scope-relative names (`remote` for `cli/remote`), counts churn against the scope's history, and still joins ledger rows by their crate-relative spelling and judges verdict keys against the whole tree, so a scoped run agrees with the crate-wide one on what is held and what is stale. Sections: `probes`, `rank`, `hot`, `assemblers`, `debt`, `shapes`, `guards`, `footer`.

- `probes` — **finding.** One line per module for the top five rank rows in the order in force, skipping rows flagged `held`: rank figures and flags, the hot functions inside it, shape and guard families with members in it (`→ collapse?` when a family has siblings), admitted upward sites with how many are unreviewed, and the exact `inspect` command to run next. It joins the other sections; it adds no score and no verb the sections do not already carry.
- `rank` — **evidence.** Per module: code, tests, `esc`, `depth` (code per escaping item), churn%, pace, `cx`, test/code ratio, flags. Default order is accretion (code × churn); `--by <code|esc|churn|pace|cx|tc|depth>` re-sorts (`tc` and `depth` ascending: thinnest tests and shallowest modules first). Flags: `pin` (churn ≥ 3% with t/c < 0.3), `hot` (pace ≥ 1.5), `bin` (binary-target code: its crate's `src/main.rs`, `src/bin/`, or a top-level module that crate's `main.rs` declares and its `lib.rs` does not), `cx` (top decile of modules with cx > 0), `thin` (t/c < 0.3 with 200+ lines), and from the ledger's module verdicts, `held` (a `holds` row whose module has fewer scoped commits since its SHA than its reopen count) or `reopen` (the count is reached). A held module keeps its row and its position; it only stops taking a probe slot.
- `hot` — **finding.** The functions to open: top `cx × file churn%` with `file:line`, each row showing its raw cyclomatic (`cyc`), cognitive (`cog`), and SLOC (`sloc`) counts beside `cx`; the JSON carries them as `cyclomatic`, `cognitive`, `sloc`. `cyc` counts one per closure or nested function inside the function, so replacing a `match` with `map_err(|e| ..)?` raises it.
- `assemblers` — **finding.** The caller-side `deepen` candidates: production functions in the scope that call into three or more modules, with the distinct callees per module (`wires`). A module inside the scope reads by its crate-relative row (`cli/room`), one outside it by its top-level module (`room`), so `survey --path crates/rimz/src/cli` lists the CLI functions that wire the library. Callees resolve by syntax through the file's imports and crate-rooted paths, so a method call or an unimported name is not counted and the count is a floor; `inspect --module <provider> --from <caller module>` measures the same function exactly in its `heaviest` table.
- `debt` — **evidence.** Admitted upward dependencies: per target rule touching the scope, upward sites split into reviewed (a `docs/contributing/refactor-ledger.md` admission-intent row covers the edge, shown as the ledger spells it with the row's intent), unreviewed (admitted by the target, no ledger row: the review backlog), and unadmitted providers, then each strangler's current count against its baseline. Admissions are ratchets `conform` keeps, not decisions; the ledger row is the decision, and an edge whose row says `closed` yet still has sites is a regression to read. Sites are counted per `[[module]]` rule, so a file rule and the directory rule enclosing it both count the same site. The section ends with module cycles: top-level module pairs that import each other, with sites each way, bounded by `--top`. A pair in one target layer reads `same layer` and sorts first, since layering cannot express it; a `cross-layer` pair already shows in the debt rows above. `(crate root re-exports)` names the crate root: the other side imports a `lib.rs` re-export such as `crate::Store`, which syntax resolves to the root rather than to the defining module.
- `shapes`, `guards` — **finding.** Families above the finding gate; `--all` shows the rest and the footer counts what was dropped and why. The footer's `stale verdict keys` line lists every `[[verdict]]` whose evidence is gone anywhere in the tree, labelled `shape:`, `guard:`, `item:` or `pass-through:`, and `ambiguous verdict keys` every key that resolves to several definitions (a shape key under which one view forms several families counts too); both are record defects to fix in `refactor-target.toml`. Item and pass-through keys resolve as every [item key](#vocabulary) does, and a pass-through key must resolve to a function that still forwards. The footer also reports the ledger's admission intents and holds, or that no ledger exists, in which case every admission reads as unreviewed. At load time, across the whole ledger regardless of `--path`, a SHA that `HEAD` does not reach resolves to the oldest reachable commit that changed its occurrence count in the ledger file. This works after rebase or squash merges even without the old object; churn counts from that commit, not its parent. Each mapping appears as `ledger restamp:`. `cargo xtask atlas survey --restamp` writes only those rows' SHA cells, preserving other bytes and refusing stale cells. Malformed rows and SHAs no reachable commit wrote remain `ledger problem:` lines and leave their modules unheld.

```sh
cargo xtask atlas survey --path crates/rimz/src/store --by depth --top 20
```

## `inspect` — learn one module

`inspect --module <module|path>` uses exact SCIP references. A `.rs` path measures that one file; name the module (`harness::schedule`) to measure the directory beneath it as well (`crates/rimz/src/harness/schedule.rs` read 34 items where `harness::schedule` read 168). `--from` selects a caller module to quote (default: the heaviest), `--item` adds item history, referrers (production, testkit, and test apart), markers, and its verdict, `--all` shows families below the finding gate, `--top` defaults to 20, and `--brief` is the subagent-brief preset: `verdict`, `record`, `heaviest`, `surface`, `flags`, `passthroughs`, `pins` (and `item` when `--item` is given) at `--top 10`, with every `surface` row that can narrow listed past that cap so the verdict's narrow count reads off the table; `--brief` and `--section` are mutually exclusive. `--item` takes an [item key](#vocabulary); a private function or item reads as `private` with its defining module as its reach. A qualified `--item` implies `--module <module>` when that flag is absent. Sections, in order: `verdict`, `record`, `item` (when given), `callers`, `heaviest`, `surface`, `pins`, `passthroughs`, `assembly`, `calls`, `flags`, `shapes`, `guards`, `providers`, `footer`.

- `verdict` — **finding.** Seven lines: escaping items (how many escape through `pub use`, and the raw declaration count `survey`, `diff`, and `conform` report as `esc`, which keeps `mod` lines and counts each `pub use` before this dossier folds re-exports onto their definitions) and outside sites with the head that carries 80% of them; items only the module itself reaches and how many items can narrow, tallied by target visibility; the top assembly cluster and its caller count; the heaviest caller at its folded item count and what else it wires; items without production or testkit sites and how many pin a fix; one-caller flags and constant parameters; pass-throughs and the items whose tests sit past their narrowed reach. Everything after it is the evidence.
- `record` — **evidence.** The module's root file, the nearest `AGENTS.md` above it, and the first paragraph of its `//!` header: what the repository already says the module is for, so a dossier pasted into a brief carries the record beside the numbers.
- `callers`, `heaviest` — **evidence.** Callers by assembly, then the heaviest production functions of the selected caller (a caller module that encloses the inspected one counts only its functions outside it, as `[[assembly]]` rows do) with `items` (folded), `sites`, `also wires` (distinct items per other provider module the same function references), and a quote of the heaviest: the whole function when it fits in 80 lines, otherwise its signature and every reference site with a line of context, gaps under ten lines kept and longer ones elided. The ideal call for the common case, written beside the quote, is the call-site test.
- `surface` — **evidence.** The `deepen` decision in numbers, one row per escaping item sorted by outside production sites: `sloc` (lines the definition spans, for the plan's line arithmetic), `reach` (how far its effective visibility goes: `extern`, `crate`, or a module) and `narrow to` (the narrowest visibility that still covers every production and testkit caller: `keep`, `pub(super)`, `pub(crate)`, `pub(in crate::…)`, or `private` when only the item's own module and its descendants reach it, or nothing does). The `testkit` column counts support sites separately from `internal` production sites and `tests`. A type named on the RHS of a public type alias cannot narrow below that alias's effective reach. A reader in another compilation target, whether production, testkit, or test, floors the item at `keep`. A `pub use` is measured at the definition it re-exports and keyed by the exporting module, so a root that re-exports private submodules reads at its real surface; a glob expands to every definition behind it, a second `pub use` of a measured definition counts as an alias, and a re-export of something defined outside the boundary is unmeasured. **Vestigial candidates** (a finding) follow: escaping items with no production or testkit site, each with its test referrers and, when its definition blames to one commit, that commit and date; `pins a fix` marks one that reads as a fix, so read it before deleting. Unresolved definitions close the section.
- `pins` — **finding.** Items whose in-crate tests reach them from outside the visibility `narrow to` names: a test in another module loses `private`, `pub(super)`, and `pub(in …)`. Each row counts the test sites lost and names the test functions (`path:line` alone for an import line or an unparsed in-crate test file). This is the plan's "tests that move" list: rewrite each to the new call site or delete it with the internals it exercised.
- `passthroughs` — **finding.** Functions in the module whose body forwards to one callee, escaping ones first: the deletion test's candidates. Inline the private ones; for an escaping one, ask whether the seam earns its keep across its callers.
- `assembly`, `calls` — **evidence.** Repeated assembly prints one root per cluster — the smallest item set the most callers share, with its full caller list — and nests each deeper subset as `+ <extra items>: K of M functions`. Call shapes add order: one function's target references in source order, folded per owner type, consecutive repeats removed; functions sharing a sequence group with `×N`. A shape earns a row when several functions share three or more items, or when one function alone references five or more. Type aliases are names, not behaviour, and never count as assembly items.
- `flags` — **finding.** Parameters of escaping functions with a flag-like type (`bool`, `Option`, a crate enum), grouped by the literal value production and testkit callers pass. `one-caller`: one value has exactly one caller while others have more — a branch in shared code serving one site. `constant`: every caller passes the same value — a parameter to delete. Sites whose call the join cannot locate are counted as skipped.
- `shapes`, `guards` — **finding.** The crate-wide families that name an item this module defines; a qualified name counts only under one of the module's own modules or types.
- `providers`, `footer` — **evidence.** What the module depends on, the target rules that cover it, parse failures, and unresolved definitions (`mod` declarations and re-exports of items defined outside the boundary are counted as unmeasured).

```sh
cargo xtask atlas inspect --module crates/rimz/src/store --from sidebar::enrich --item store::message::MessageRecord
cargo xtask atlas inspect --module message --brief --out /tmp/atlas-message.md
```

## `diff` — prove one pass

`diff --base <ref> --path <scope>` compares an indexed base with the indexed working tree. The base is the merge base of `<ref>` and `HEAD`, so `main` means where the pass forked rather than wherever trunk has moved since; an ancestor SHA resolves to itself. It reports SLOC, boundary `esc`, call-site assembly (only the caller→provider pairs whose `max/fn` moved, with the unchanged count), dependency sites, changed files inside and outside the scope (grouped by module or top-level directory, bounded by `--top`), parse failures, and newly unresolved definitions. Dependency sites split into those crossing the scope boundary, counted per layer direction and listed in full, and internal sites between the scope's own modules, counted on one row and listed only under `--section internal`, so a file split is not reported as a seam moving. `--expect` instead reads the executable pass contract below; keep that ephemeral contract outside the worktree so it is not itself an out-of-scope change. It loads complexity metrics only when the contract carries `[[cx]]` rows, measuring both revisions from their recorded source contents in temporary roots (survey's `target/atlas-complexity` stays untouched); without those rows it neither runs nor requires `rust-code-analysis-cli`. Sections: `expectations`, `totals`, `interface`, `surface`, `dependencies`, `internal`, `files`, `evidence`.

```sh
cargo xtask atlas diff --expect /tmp/atlas-pass-contract.toml
```

## `conform` — keep the target

`conform` compares the working tree with root `refactor-target.toml`. `--ratchet` fails on excess surface, strangler counts, or unadmitted dependencies; `--tighten` only lowers measured ceilings and removes unused admissions. A passing ratchet prints nothing, so it does not show how much room a module has left; the count it compares against `surface-budget` is the "N declarations as … count esc" figure on the first [`verdict`](#inspect--learn-one-module) line of `inspect --module <module> --brief`, which is also that module's `esc` on a crate-wide `survey` rank row. The `esc` on a scoped survey's `overall` line sums its submodule rows, each measured at its own boundary, so it runs above the module's figure. Tightening edits the target in place, preserving comments and leaving untouched rules byte-identical. `--only <path>` (repeatable, requires `--tighten`) selects the `[[module]]` and `[[strangler]]` rules at that exact root-relative `path`, not descendant rules, so a pass can tighten only the rules it owns. An unknown path fails before measurement. A ratchet failure names the measure that regressed (`surface`, `strangler`, or the admissions) and prints, per rule, the `[[module]]` or `[[strangler]]` block at its measured values, ready to paste over the rule or, for a module with no rule yet, to add; since `--tighten` never raises, that block is how a new or repointed rule is written. A missing target passes; `--only` with no target fails.

Compare a module's measured escaping-item count with its `surface-budget` in `refactor-target.toml`: the budget is a ceiling, not the current surface, and only a count above it fails the surface ratchet.

```sh
cargo xtask atlas conform --ratchet
cargo xtask atlas conform --tighten --only crates/rimz/src/store
```

## `index`

**Evidence.** `cargo xtask atlas index [--doc <path>] [--symbol <name>]` lists raw SCIP occurrences before atlas filters references. Compare it with `inspect`: a site listed here but absent there was filtered by atlas; a site absent from both is an index miss. At least one selector is required. `--doc` is root-relative (`./` is normalized); `--symbol` takes one bare item name, such as `open`, using the same descriptor-tail match as atlas's item join: the name must be a whole descriptor, so `ensure` does not select `should_ensure`. Use the full SCIP symbol in the output to distinguish same-named items.

| Selectors | Selected symbols | Listed occurrences |
| --- | --- | --- |
| `--symbol N` | Every non-local symbol matching N | Every occurrence of those symbols, index-wide |
| `--doc D --symbol N` | Matching symbols with a definition in D | Every occurrence of those symbols, index-wide |
| `--doc D` | Every non-local symbol occurring in D | Non-local occurrences in D only |

The header names the index, selectors, and whether a requested document exists in the index. An absent document or empty match is a successful answer. `symbols` lists full symbols, available display names, SCIP kinds and enclosing symbols, all definition sites, and listed occurrence counts. `occurrences` lists normalized 1-based sites, roles, symbols, and enclosing functions, including documents outside atlas's source walk. Both sections are sorted and untruncated. Use `--out`, `--section`, or JSON with `jq` for large queries.

Local symbols are omitted; a document-only query counts D's locals under `unlisted.locals`, and a `--symbol` query, which can never select one, reports 0. Selected occurrences without a line are counted under `unlisted.no_line`. Roles decode all seven SCIP bits; rust-analyzer currently sets only `definition`, with plain references carrying no bits (`[]` in JSON, `reference` in Markdown). Enclosing functions come from atlas's source syntax, not SCIP's unreliable enclosing ranges; non-production sources and files that fail to parse have no enclosing function.

The verb is read-only apart from the [index cache](#index-cache) and a requested `--out`: a miss builds the current tree's index, while a hit refreshes its timestamp and participates in keeping the two newest indexes. It does not accept a foreign index or base revision.

## Output flags

Every report verb (`survey`, `inspect`, `index`, `diff`) takes the same three flags. `--json` emits the full report as JSON (`--top` bounds Markdown only where supported); `--out <file>` writes it there instead of stdout; `--section <a,b>` keeps only the named sections in either form. An agent reading a dossier should write it with `--out` and narrow it with `--section` or `jq` rather than let the whole report through stdout.

```sh
cargo xtask atlas inspect --module crates/rimz/src/store --json --section verdict,surface --out /tmp/store.json
```

## Index cache

`inspect`, `index`, and `diff` read a rust-analyzer SCIP index of the whole workspace, cached under `target/atlas/index-<key>.scip` where the key hashes every Rust source and `Cargo.lock`. The first run after any source change generates it, which takes over a minute and says so on stderr; later runs on the same tree reuse it. `diff` indexes `--base` in a temporary checkout under the same cache, so its first run generates twice. The two newest indexes are kept. `survey` and `conform` never build one.

## Pass contract v2

```toml
version = 2
# base: pin the merge-base SHA; changed-path checking diffs against the ref as written
base = "<merge-base SHA>"
kind = "seam"
paths = ["crates/rimz/src/store", "crates/rimz/src/agents"]
max-production-sloc-delta = 0

[[esc]]
path = "crates/rimz/src/store"
max = 110

[[delete]]
item = "store::legacy_open"

[[rehome]]
item = "store::AgentContext"
to = "agents"

[[dependency]]
from = "store"
to = "agents"
max-sites = 0

[[assembly]]
from = "sidebar::enrich"
to = "store"
max-items = 3
```

`paths` must be non-empty root-relative boundaries. A directory path (`crates/rimz/src/harness/resume`) covers the directory and its sibling `resume.rs`; a file path covers only that file, not the module's `resume/tests/`. `kind` is `module` (the default), `seam` or `tooling`: a module contract's `max-production-sloc-delta` is negative by the program's rule, positive only for a narrowing-only pass with the reason in the commit, and `diff --expect` makes a positive module ceiling carry at least one `[[esc]]` row whose `max` is below that path's measured base count, so the pass pays for its growth with a narrowing; a seam contract carries at least one `[[dependency]]` or `[[rehome]]` row and takes a flat ceiling, `0` by convention or a small positive with the reason in the pass row. A tooling contract prices its ceiling from its target and carries no narrowing obligation; its optional rows still hold. Each optional row proves one verb: `[[esc]]` caps a boundary's escaping items (the measurement `conform`'s `surface-budget` uses; the path must lie inside `paths`), and a `pub` or `pub(crate)` module declaration is itself an escaping item, so a pass that adds a submodule pays one `esc` for the declaration on top of every item inside it: read the number from `conform --ratchet` on the built tree rather than counting by hand; `[[delete]]` names an [item key](#vocabulary) that must exist once at `base` and be gone now; `[[rehome]]` in its item form names such an item that must be gone from its base module and defined exactly once under `to` (a drift row lists every site when there are more; a `pub use` re-export is one, caveat 11), and in its [logic form](#logic-rehome) proves that decisions left a module that stays; `[[dependency]]` caps the syntax dependency sites from one module to another, whatever the layer direction, and prints the base count beside it; `[[assembly]]` names resolvable caller/provider modules whose `max/fn` must both shrink and land at or below `max-items`. With `diff --expect`, exit is zero only when production SLOC is at or below the delta ceiling, every row holds, every changed path is inside `paths`, and evidence has no parse failure or newly unresolved definition; otherwise the command reports drift and exits nonzero. Version 1 contracts (no `esc`, `delete`, `rehome`, or `dependency` rows) still load.

Set each cap from a measurement. Before setting one, run `cargo xtask atlas diff --expect <draft>` on the clean base with every row present: the report prints the base value of each `esc`, `dependency`, `assembly`, and `cx` row, so the caps come from those numbers and the draft needs no second indexed run.

### Logic rehome

```toml
kind = "seam"

[[rehome]]
from = "cli::agents_cmd::exec"
to = "harness"
min-decisions = 40
```

A `[[rehome]]` row sets exactly one form: `item` and `to`, or `from`, `to` and `min-decisions`. The logic form is for logic that leaves a module which stays, such as a CLI entry whose workflow moves to its library owner. `from` and `to` are module paths, neither inside the other, each with production files inside the contract's `paths`; `to` may be new at current, and a side with no functions measures `0`. The row measures [decisions](#vocabulary) on both revisions and lands when `from` falls by at least `min-decisions` and `to` rises by no more than `from` fell: the logic moved, rather than splitting in place or growing branches on the way. The detail prints both counts, and a drift names the condition that failed. Like `[[cx]]`, the row needs `rust-code-analysis-cli`.

The row proves where decisions went, not that the destination is a good interface: cap the destination with an `[[esc]]` row and prove the edge call got lighter with an `[[assembly]]` row. A `to` that only holds the moved body behind a request type is a pass-through by the deletion test, whatever the row says.

### Complexity rows

```toml
[[cx]]
item = "cli::lsp::run"
max = 5.0

[[cx]]
path = "crates/rimz/src/cli"
max = 12.0
```

Each `[[cx]]` row sets exactly one of `item` or `path`. An item is an exact production function key, `module::name` or `module::Owner::name`, including private functions. `Owner` is the impl self type's last segment, including trait impls. Ambiguity on either revision is a load error listing the sites; a key absent at both revisions is also rejected. A side where the function is absent measures zero and prints `absent`. A resolved function with no matching metric drifts as `no metric`, including a nested function absorbed into its parent's metric. The key's module includes enclosing inline `mod` blocks, as for every [item key](#vocabulary): for example, `config::harness::prompt_cache_ttl_serde::deserialize`.

A path sums function scores in that file or directory, including a directory's sibling `.rs` module file. Paths and item definitions on each side must lie within the contract's `paths`. Scores and maxima are rounded to one decimal before comparison. A row lands when current complexity is at most `max`; a drift prints the excess. An item row may cap a new destination, so shrinking is not required of every row.

A `cx` cap cannot tell a move from a split ([caveat 17](#caveats)); a move is proved by a [logic rehome](#logic-rehome).

## Target schema v5

```toml
version = 5
layers = [["store", "theme"], ["agents", "message"], ["sidebar", "cli"]]

[[module]]
path = "crates/rimz/src/store"
upward-dependencies = ["message"]
surface-budget = 120

[[strangler]]
symbol = "legacy_open"
path = "crates/rimz/src/store"
baseline = 2

[[verdict]]
kind = "shape"
key = "decode_request"
reason = "Provider formats intentionally share this choreography."
```

`layers` is an ordered list of module groups from lower to higher. A `[[module]]` path sets its escaping-surface ceiling and may set either `upward-dependencies` for exceptions to layer direction or `allowed-dependencies` as an exact boundary allow-list; a directory rule also covers its sibling `.rs` file. A `[[strangler]]` counts one Rust identifier under its path.

After a rebase that conflicts in `refactor-target.toml`, run `cargo xtask atlas conform --ratchet` before any hand-off: a textual merge of two budgets does not prove the merged surface fits.

Every `[[verdict]]` needs a non-empty reason and a unique `(kind, key)`. Key forms are:

| kind | key |
| --- | --- |
| `item` | `module::path::Name` |
| `pass-through` | `module::path::Name` |
| `guard` | normalized guard text printed as the family key |
| `shape` | shared callee set printed as the family key |

An item verdict's key is an [item key](#vocabulary), so it may name a private function or type (`cli::remote::supervisor::wait_for_master`) and a method either by name or with its owner; if several candidates share it, `inspect` reports the ambiguity with each definition and known owner rather than selecting one. Shape and guard verdicts suppress matching families in `survey`; `inspect` displays item verdicts, and both verbs list stale and ambiguous keys. `conform` preserves verdicts but does not enforce their reasons. Item verdicts do not hide survey hotspots.

## Vocabulary

- **Escaping (`esc`)** — an item whose effective visibility leaves the measured module boundary. It counts what outside callers can reach, not every declared `pub` item. A `pub` inherent method is its own item, so a rehomed type carries one `esc` per public method beside the type itself (pass 15c planned 51→50 and measured 48). `pub(crate)` reaches the crate root, so narrowing `pub` to `pub(crate)` never lowers `esc`; only a deletion, a dropped re-export, or a `private`/`pub(super)` spelling inside the boundary does.
- **Depth** — code SLOC per escaping item: how much a module hides behind each name it exposes.
- **Churn%** and **pace** — a module's share of scoped history commits (renames folded to current paths), and its share of the recent 25% window divided by its lifetime share; pace `1.0` is its historical rate and `1.5` or more is `hot`.
- **`cx`** — severity-weighted excess over the complexity warn thresholds, summed per function; `0` means every function is under threshold. Per function it is a multiplier times the weighted relative excess `(cog/15 − 1) + 0.5·(cyc/10 − 1) + 0.25·(sloc/60 − 1)`, each term floored at 0 and the `cyc` term counted only when cog > 15; the multiplier is ×4 when cog > 50 or (cog > 15 and cyc > 25), ×2 when cog > 25, (cog > 15 and cyc > 15), or sloc > 100, and 0 otherwise, every comparison strict (`xtask/src/atlas/metrics.rs`). Splitting a function lowers `cx` whether or not the design improves, since each part measures its excess from the thresholds afresh, so a `cx` drop from a split alone proves nothing: price a fold from the raw `cyc`/`cog`/`sloc` columns. To prove that logic moved rather than split, count [decisions](#logic-rehome) instead.
- **Decisions** — Σ(`cyc` − 1) over the production functions of a module and its submodules. Each function carries a base of 1, so the sum counts branch points (`if`, loops, match arms, `?`, `&&`, `||`) plus one per closure or nested function, and extracting code into another function or method leaves it unchanged. It is the measure of a [logic rehome](#logic-rehome).
- **`max/fn`** — the greatest number of distinct target items one production function references. Every reference to one owner type (the type, its variants, its associated functions and methods) is one item, so a builder chain reads as `MessageRecord::{new, with_channel, with_sender, +3}` and counts once.
- **Family** — repeated knowledge grouped across functions, keyed for a verdict: a shape family by its shared callee set, a guard family by its normalized guard text. Families only name crate vocabulary (std and external idiom is dropped), and only findings are shown by default. A shape family needs **siblings** or three files averaging 40 SLOC, and a non-sibling family whose crate callees all belong to one module is use of that module's API, not duplication. A guard family must compose crate knowledge — two crate-defined names, a field exactly one struct declares, or a crate path such as `RunStatus::Completed`; a guard that calls one crate predicate or matches one callee's result (`Err(_) = atomic::write(...)`) is use. `--all` shows what the gate dropped; the footer counts it.
- **Siblings** — member files of one shape family that occupy the same role in sibling directories (`agents/adapters/*/spend.rs`): parallel implementations of one responsibility. Families with siblings rank first, then by SLOC in play (members × mean SLOC). Siblings exempt a family from the one-module gate only when they are most of its members.
- **Item key** — how `inspect --item`, item `[[verdict]]` keys, `[[delete]]` and item `[[rehome]]` name one definition (`modules::definitions_for_key`). It is `module::Name` (or a bare `Name` beside `inspect --module`) or `module::Owner::name`, searched in the named module first and then beneath it (`message::queue_synthetic` finds `message::deliver::queue_synthetic`). A module includes enclosing inline `mod` blocks. Three tiers resolve in order: visible items (a `pub use` resolves to the definition it re-exports), production functions, then private non-function items (struct, enum, union, trait, const, static, type alias). Within a tier, resolution follows Rust paths: `module::name` names a free item (a module, type, function or re-export outside any impl or trait) before a method, and reaches a method by name only when no free item matches; `module::Owner::name` names an inherent method of `Owner` before a trait-impl method for it. Definitions of one name under pairwise different `#[cfg]` predicates (a feature on/off pair, one per `target_os`) are alternatives and resolve to the first in file order. An item declared inside a macro body, or compiled only for tests or `testkit`, is invisible to every tier. `[[cx]]` keys are stricter: an exact production function key.
- **Verdict** — a durable, reasoned disposition of one item, pass-through, guard family, or shape family. Atlas reports a key as stale when its evidence disappears and as ambiguous when it resolves to several definitions.
- **Dependency site** — a syntax-derived internal dependency written as either a `use` or a qualified path. Sites are deduplicated per file by resolved module and item.
- **Production SLOC** — code lines outside every `cfg` gated on `test` or the `testkit` feature, whether the gate sits on the item or on the `mod` line that reaches its file; `test`-gated lines are test SLOC and `testkit`-only lines count in neither column. Every other syntax measure (escaping items, dependency sites, functions) applies the same gate.

## Caveats

1. SCIP names a package, not a target. Visibility floors infer targets from layout: separate crate roots, `src/main.rs`, `src/bin/`, and top-level modules declared by that crate's `main.rs` but not its `lib.rs`. Manifest `path` overrides moving library or binary roots off the default layout are not honoured.
2. For `inspect` history, `git log -S` candidates are reported, never chosen; read the candidate commits before drawing a history conclusion.
3. Macro bodies carry no dependency sites because syntax analysis does not parse their token streams.
4. The family filters match identifiers, not resolved receivers: `io::stdin().is_terminal()` reads as a crate predicate wherever the crate also defines an `is_terminal`; a qualified callee is crate-defined when any of its segments is; a field composes only when exactly one struct declares it, so a field shared by two related structs is dropped; the one-module gate resolves callees by name, so a name defined in several modules widens the candidate set.
5. Call shapes order references by line, and an owner type's fold sits at its first reference; two references on one line sort by name.
6. A definition rewritten wholesale in one later commit blames entirely to that commit, so a vestigial candidate's "introducing" commit is the last one to rewrite it; the summary shown is still the one to read.
7. A shape family key is its callee set, so it changes when one member gains a call; a verdict on a sibling family is best written once the shared choreography is stable.
8. The flag join matches a reference to the call on the same line by callee name; a line holding two calls of one callee takes the first, and a reference with no call on its line is counted as skipped, never guessed.
9. A `pub use` path resolves by syntax: an explicit `crate`/`self`/`super` path once, a bare path first as a child of the declaring module and then from the crate root. A re-export chain deeper than eight hops, or one that passes through a module the crate does not define, reads as unresolved or foreign rather than guessed.
10. `assemblers` resolves a callee through the file's `use` lines and explicit `crate`/`self`/`super` or crate-name (`rimz::`) paths; a method call, a `Self::` call, or a name the file does not import is not attributed, so the count is a floor.
11. An item `[[rehome]]` counts every visible item and production function with the name under `to` (for a method, only methods of the same owner), a `pub use` re-export included, so the destination declares the item once and re-exports it nowhere.
12. A `[[delete]]` or item `[[rehome]]` key resolves as an [item key](#vocabulary). An item declared inside a macro body or compiled only for tests or `testkit` fails as "not defined at base" and gets no row: prove its deletion by the SLOC ceiling and by source search, for example `rg -n '<symbol>' crates/rimz/src` returning no matches. A method declared in a `trait` block carries no owner in the index, so `module::Trait::name` fails the same way: write the name-only `module::name` key; a method in a trait impl is keyable as `module::Owner::name`. A name-only method key that shares its name with another method in the module is ambiguous; write the owner-qualified key. Check a claim that the index lacks something against `xtask/src/atlas/syntax.rs::FileSyntax`, which holds per file `pub_items` (escaping items), `fns` (every production function, private included, with its module, owner and line span), `private_items` (private non-function items), `calls`, and `dependencies`.
13. Public type aliases floor their RHS types at the alias's effective reach, one step only: alias chains are not followed transitively. Direct public field and signature constraints remain outside `narrow to` and `pins`. A type exposed by a public field or `pub` signature cannot narrow below its owner under `private_interfaces` (`-D warnings`); public associated types such as `FromStr::Err` must also remain visible. Read `surface` for functions, constants and methods, and treat the remaining types as `keep` until a spike proves otherwise. A re-export reached only by tests also reads as narrowable when the right move is deleting the alias; check both before writing a narrowing step.
14. A method named only inside a serde `skip_serializing_if = "Type::method"` string is invisible to the index: it reads as vestigial and draws a `private` proposal while being live. Grep the string before narrowing or deleting anything that looks unreferenced — the `is_unset`/`is_empty`/`is_default` predicates are the usual family.
15. `diff` measures every Rust file under `paths`, `xtask/` included.
16. Ledger SHA resolution assumes no older trunk commit quoted the same SHA before the reviewed pass landed. An earlier prose mention would map to that commit and over-count churn, reopening early; inspect the reported `ledger restamp:` mapping before writing it. A committed typo resolves too, since git cannot distinguish it from a rewritten SHA whose object is absent.
17. A function's `cx` score is not a smooth count of excess: the multiplier in its [definition](#vocabulary) is 0 below the high thresholds, so a function drops to exactly 0 once it clears them, and a pass either brings it under that jump or only shrinks it. Survey's raw per-function metrics are in `target/atlas-complexity/**/*.json`; `diff --expect` measures in temporary roots and leaves those files unchanged.
18. Production SLOC counts lines after rustfmt, and `pub(crate)` is seven characters wider than `pub `, so a narrowing can push a near-limit signature onto more lines. A narrowing-only contract prices those lines into a small positive ceiling rather than trading the narrowing away.
19. `inspect`'s `narrow to` floors readers in `cli/**`, `tests/**` and `benches/` at `keep`, since those are separate crates that see only `pub`. It does not see a name used in another module's public rustdoc; `cargo xtask doc` is a gate, so check `rg` for the name in doc comments before narrowing.
