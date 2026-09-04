# Atlas: refactor analysis

`cargo xtask atlas` produces bounded Markdown evidence for architecture review. Its four commands survey a scope, inspect a module, prove a pass, and keep target constraints from regressing. Atlas locates and sizes; reading decides. Every section below is tagged **finding** (a row is a candidate on its own) or **evidence** (a row needs a reader), and the [vocabulary](#vocabulary) at the end defines the terms the output does not explain.

## Review workflow

The verbs map onto an architecture review pass, and the pass contract is where the review's plan meets the coder's proof.

1. **Survey and size.** `survey` on the crate. The probes join every signal per module and name the next command; take the pick to the user as confirm-or-redirect. `assemblers` names the functions that wire several modules together, the caller-side candidates the per-module probes cannot see. The admitted-dependency table and the `layers` in `refactor-target.toml` are the repository's recorded ratchets; a `[[verdict]]` is where a recorded decision lives, so read those before judging.
2. **Learn what it does.** `inspect --module <target> --brief --out /tmp/<target>.md` is the dossier a subagent brief carries; `--json --out /tmp/<target>.json` then `jq` narrows the full report. Read it in section order: the verdict for wide or deep, the record for what the module claims to be, then the heaviest quote for the call-site test. A shape proves reach, not assembly: the quote (whole under 80 lines) shows whether the caller constructs and wires the module's internals or merely names several of its items; brief a subagent only when it elides the part that decides. `--item <module::Name>` on each candidate before calling it deletable: its introducing commits and fix markers are the history check. `--from <caller>` re-quotes for a caller the `assemblers` table named.
3. **Plan.** Write each candidate's proof as a contract row — `deepen` as an `[[assembly]]` row on the caller that must get lighter (the call-site proof; an `[[esc]]` ceiling beside it only when names leave the surface, since narrowing visibility alone satisfies `esc` without changing any call), `delete` as `[[delete]]` rows, `rehome` as `[[rehome]]` for the item and `[[dependency]]` for the seam it closes, `collapse` as `[[delete]]` rows on the siblings that die plus the SLOC delta — and put the contract in the plan verbatim so the coder runs `diff --expect` and the reviewer reads drift instead of re-deriving it:

```toml
version = 2
base = "main"
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

4. **Prove.** `diff --expect <contract>` after the pass; `conform --tighten` once it lands, so the ceilings the pass earned stay earned.

## `survey` — map a scope

`survey` reads the working tree and history without building a SCIP index. `--path` defaults to `crates/rimz/src`; `--top` to 20. Sections: `probes`, `rank`, `hot`, `assemblers`, `debt`, `shapes`, `guards`, `footer`.

- `probes` — **finding.** One line per module for the top five rank rows in the order in force: rank figures and flags, the hot functions inside it, shape and guard families with members in it (`→ collapse?` when a family has siblings), admitted upward sites, and the exact `inspect` command to run next. It joins the other sections; it adds no score and no verb the sections do not already carry.
- `rank` — **evidence.** Per module: code, tests, `esc`, `depth` (code per escaping item), churn%, pace, `cx`, test/code ratio, flags. Default order is accretion (code × churn); `--by <code|esc|churn|pace|cx|tc|depth>` re-sorts (`tc` and `depth` ascending: thinnest tests and shallowest modules first). Flags: `pin` (churn ≥ 3% with t/c < 0.3), `hot` (pace ≥ 1.5), `bin` (declared only from `main.rs`), `cx` (top decile of modules with cx > 0), `thin` (t/c < 0.3 with 200+ lines).
- `hot` — **finding.** The functions to open: top `cx × file churn%` with `file:line`.
- `assemblers` — **finding.** The caller-side `deepen` candidates: production functions that call into three or more of the scope's modules, with the distinct callees per module (`wires`). Callees resolve by syntax through the file's imports, so a method call or an unimported name is not counted and the count is a floor; `inspect --module <provider> --from <caller module>` measures the same function exactly in its `heaviest` table.
- `debt` — **evidence.** Admitted upward dependencies: per target rule touching the scope, upward sites grouped by admission, unadmitted providers, and each strangler's current count against its baseline. Admissions are ratchets `conform` keeps, not decisions; nothing here says an admission is meant to close. The section ends with module cycles: top-level module pairs that import each other, with sites each way, bounded by `--top`. A pair in one target layer reads `same layer` and sorts first, since layering cannot express it; a `cross-layer` pair already shows in the debt rows above.
- `shapes`, `guards` — **finding.** Families above the finding gate; `--all` shows the rest and the footer counts what was dropped and why.

```sh
cargo xtask atlas survey --path crates/rimz/src/store --by depth --top 20
```

## `inspect` — learn one module

`inspect --module <module|path>` uses exact SCIP references. `--from` selects a caller module to quote (default: the heaviest), `--item` adds item history, referrers, markers, and its verdict, `--all` shows families below the finding gate, `--top` defaults to 20, and `--brief` is the subagent-brief preset: `verdict`, `record`, `heaviest`, `surface`, `flags`, `passthroughs`, `pins` (and `item` when `--item` is given) at `--top 10`; `--brief` and `--section` are mutually exclusive. `--item` takes `Name` for an item anywhere in the module or `module::Name` for `Name` in that module or beneath it (`message::queue_synthetic` finds `message::deliver::queue_synthetic`; a definition in the named module itself wins), and a `pub use` resolves to the definition it re-exports. Sections, in order: `verdict`, `record`, `item` (when given), `callers`, `heaviest`, `surface`, `pins`, `passthroughs`, `assembly`, `calls`, `flags`, `shapes`, `guards`, `providers`, `footer`.

- `verdict` — **finding.** Seven lines: escaping items (and how many escape through `pub use`) and outside sites with the head that carries 80% of them; items only the module itself reaches and how many items can narrow, tallied by target visibility; the top assembly cluster and its caller count; the heaviest caller at its folded item count and what else it wires; items without production sites and how many pin a fix; one-caller flags and constant parameters; pass-throughs and the items whose tests sit past their narrowed reach. Everything after it is the evidence.
- `record` — **evidence.** The module's root file, the nearest `AGENTS.md` above it, and the first paragraph of its `//!` header: what the repository already says the module is for, so a dossier pasted into a brief carries the record beside the numbers.
- `callers`, `heaviest` — **evidence.** Callers by assembly, then the heaviest production functions of the selected caller with `items` (folded), `sites`, `also wires` (distinct items per other provider module the same function references), and a quote of the heaviest: the whole function when it fits in 80 lines, otherwise its signature and every reference site with a line of context, gaps under ten lines kept and longer ones elided. The ideal call for the common case, written beside the quote, is the call-site test.
- `surface` — **evidence.** The `deepen` decision in numbers, one row per escaping item sorted by outside production sites: `sloc` (lines the definition spans, for the plan's line arithmetic), `reach` (how far its effective visibility goes: `extern`, `crate`, or a module) and `narrow to` (the narrowest visibility that still covers every production caller: `keep`, `pub(super)`, `pub(crate)`, `pub(in crate::…)`, or `private` when only the item's own module and its descendants reach it, or nothing does). A caller in a binary-only module such as `cli` sits outside the library crate, so its items read `keep`. A `pub use` is measured at the definition it re-exports and keyed by the exporting module, so a root that re-exports private submodules reads at its real surface; a glob expands to every definition behind it, a second `pub use` of a measured definition counts as an alias, and a re-export of something defined outside the boundary is unmeasured. **Vestigial candidates** (a finding) follow: escaping items with no production site, each with its test referrers and, when its definition blames to one commit, that commit and date; `pins a fix` marks one that reads as a fix, so read it before deleting. Unresolved definitions close the section.
- `pins` — **finding.** Items whose tests reach them from outside the visibility `narrow to` names: a test in another crate (`tests/`) loses every narrowing, a test in another module loses `private`, `pub(super)`, and `pub(in …)`. Each row counts the test sites lost and names the test functions (`path:line` alone for an import line or a test crate the syntax pass does not parse). This is the plan's "tests that move" list: rewrite each to the new call site or delete it with the internals it exercised.
- `passthroughs` — **finding.** Functions in the module whose body forwards to one callee, escaping ones first: the deletion test's candidates. Inline the private ones; for an escaping one, ask whether the seam earns its keep across its callers.
- `assembly`, `calls` — **evidence.** Repeated assembly prints one root per cluster — the smallest item set the most callers share, with its full caller list — and nests each deeper subset as `+ <extra items>: K of M functions`. Call shapes add order: one function's target references in source order, folded per owner type, consecutive repeats removed; functions sharing a sequence group with `×N`. A shape earns a row when several functions share three or more items, or when one function alone references five or more. Type aliases are names, not behaviour, and never count as assembly items.
- `flags` — **finding.** Parameters of escaping functions with a flag-like type (`bool`, `Option`, a crate enum), grouped by the literal value production callers pass. `one-caller`: one value has exactly one caller while others have more — a branch in shared code serving one site. `constant`: every caller passes the same value — a parameter to delete. Sites whose call the join cannot locate are counted as skipped.
- `shapes`, `guards` — **finding.** The crate-wide families that name an item this module defines; a qualified name counts only under one of the module's own modules or types.
- `providers`, `footer` — **evidence.** What the module depends on, the target rules that cover it, parse failures, and unresolved definitions (`mod` declarations and re-exports of items defined outside the boundary are counted as unmeasured).

```sh
cargo xtask atlas inspect --module crates/rimz/src/store --from sidebar::enrich --item store::agent_context::write_record
cargo xtask atlas inspect --module message --brief --out /tmp/atlas-message.md
```

## `diff` — prove one pass

`diff --base <ref> --path <scope>` compares an indexed base with the indexed working tree. The base is the merge base of `<ref>` and `HEAD`, so `main` means where the pass forked rather than wherever trunk has moved since; an ancestor SHA resolves to itself. It reports SLOC, boundary `esc`, call-site assembly (only the caller→provider pairs whose `max/fn` moved, with the unchanged count), dependency sites, changed files inside and outside the scope (grouped by module or top-level directory, bounded by `--top`), parse failures, and newly unresolved definitions. Dependency sites split into those crossing the scope boundary, counted per layer direction and listed in full, and internal sites between the scope's own modules, counted on one row and listed only under `--section internal`, so a file split is not reported as a seam moving. `--expect` instead reads the executable pass contract below; keep that ephemeral contract outside the worktree so it is not itself an out-of-scope change. Sections: `expectations`, `totals`, `interface`, `surface`, `dependencies`, `internal`, `files`, `evidence`.

```sh
cargo xtask atlas diff --expect /tmp/atlas-pass-contract.toml
```

## `conform` — keep the target

`conform` compares the working tree with root `refactor-target.toml`. `--ratchet` fails on excess surface, strangler counts, or unadmitted dependencies; `--tighten` only lowers measured ceilings and removes unused admissions. A ratchet failure names the measure that regressed (`surface`, `strangler`, or the admissions) and prints, per rule, the `[[module]]` or `[[strangler]]` block at its measured values, ready to paste over the rule or, for a module with no rule yet, to add; since `--tighten` never raises, that block is how a new or repointed rule is written. A missing target passes.

```sh
cargo xtask atlas conform --ratchet
```

## Output flags

Every report verb (`survey`, `inspect`, `diff`) takes the same three flags. `--json` emits the full report as JSON (`--top` bounds Markdown only); `--out <file>` writes it there instead of stdout; `--section <a,b>` keeps only the named sections in either form. An agent reading a dossier should write it with `--out` and narrow it with `--section` or `jq` rather than let the whole report through stdout.

```sh
cargo xtask atlas inspect --module crates/rimz/src/store --json --section verdict,surface --out /tmp/store.json
```

## Index cache

`inspect` and `diff` read a rust-analyzer SCIP index of the whole workspace, cached under `target/atlas/index-<key>.scip` where the key hashes every Rust source and `Cargo.lock`. The first run after any source change generates it, which takes over a minute and says so on stderr; later runs on the same tree reuse it. `diff` indexes `--base` in a temporary checkout under the same cache, so its first run generates twice. The two newest indexes are kept. `survey` and `conform` never build one.

## Pass contract v2

```toml
version = 2
base = "main"
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

`paths` must be non-empty root-relative boundaries. `kind` is `module` (the default) or `seam`: a module contract's `max-production-sloc-delta` must be negative; a seam contract carries at least one `[[dependency]]` or `[[rehome]]` row and takes a flat ceiling, `0` by convention or a small positive with the reason in the pass row. Each optional row proves one verb: `[[esc]]` caps a boundary's escaping items (the measurement `conform`'s `surface-budget` uses; the path must lie inside `paths`); `[[delete]]` names an item in `module::Name` form (`Name` in `module` or beneath it, as `inspect --item` resolves it) that must exist once at `base` and be gone now; `[[rehome]]` names such an item that must be gone from its base module and defined exactly once under `to` (a drift row lists every site when there are more; a `pub use` re-export is one, caveat 11); `[[dependency]]` caps the syntax dependency sites from one module to another, whatever the layer direction, and prints the base count beside it; `[[assembly]]` names resolvable caller/provider modules whose `max/fn` must both shrink and land at or below `max-items`. With `diff --expect`, exit is zero only when production SLOC is at or below the delta ceiling, every row holds, every changed path is inside `paths`, and evidence has no parse failure or newly unresolved definition; otherwise the command reports drift and exits nonzero. Version 1 contracts (no `esc`, `delete`, `rehome`, or `dependency` rows) still load.

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

Every `[[verdict]]` needs a non-empty reason and a unique `(kind, key)`. Key forms are:

| kind | key |
| --- | --- |
| `item` | `module::path::Name` |
| `pass-through` | `module::path::Name` |
| `guard` | normalized guard text printed as the family key |
| `shape` | shared callee set printed as the family key |

Method keys are name-only within their module. If several public items share that name, `inspect` reports the ambiguity with each definition and known owner rather than selecting one. Shape and guard verdicts suppress matching families in `survey`; `inspect` displays item verdicts and stale item/pass-through keys. `conform` preserves verdicts but does not enforce their reasons.

## Vocabulary

- **Escaping (`esc`)** — an item whose effective visibility leaves the measured module boundary. It counts what outside callers can reach, not every declared `pub` item.
- **Depth** — code SLOC per escaping item: how much a module hides behind each name it exposes.
- **Churn%** and **pace** — a module's share of scoped history commits (renames folded to current paths), and its share of the recent 25% window divided by its lifetime share; pace `1.0` is its historical rate and `1.5` or more is `hot`.
- **`cx`** — severity-weighted excess over the complexity warn thresholds, summed per function; `0` means every function is under threshold.
- **`max/fn`** — the greatest number of distinct target items one production function references. Every reference to one owner type (the type, its variants, its associated functions and methods) is one item, so a builder chain reads as `MessageRecord::{new, with_channel, with_sender, +3}` and counts once.
- **Family** — repeated knowledge grouped across functions, keyed for a verdict: a shape family by its shared callee set, a guard family by its normalized guard text. Families only name crate vocabulary (std and external idiom is dropped), and only findings are shown by default. A shape family needs **siblings** or three files averaging 40 SLOC, and a non-sibling family whose crate callees all belong to one module is use of that module's API, not duplication. A guard family must compose crate knowledge — two crate-defined names, a field exactly one struct declares, or a crate path such as `RunStatus::Completed`; a guard that calls one crate predicate or matches one callee's result (`Err(_) = atomic::write(...)`) is use. `--all` shows what the gate dropped; the footer counts it.
- **Siblings** — member files of one shape family that occupy the same role in sibling directories (`agents/adapters/*/spend.rs`): parallel implementations of one responsibility. Families with siblings rank first, then by SLOC in play (members × mean SLOC). Siblings exempt a family from the one-module gate only when they are most of its members.
- **Verdict** — a durable, reasoned disposition of one item, pass-through, guard family, or shape family. Atlas reports stale verdict keys when their evidence disappears.
- **Dependency site** — a syntax-derived internal dependency written as either a `use` or a qualified path. Sites are deduplicated per file by resolved module and item.
- **Production SLOC** — code lines outside every `cfg` gated on `test` or the `testkit` feature, whether the gate sits on the item or on the `mod` line that reaches its file; `test`-gated lines are test SLOC and `testkit`-only lines count in neither column. Every other syntax measure (escaping items, dependency sites, functions) applies the same gate.

## Caveats

1. SCIP names a package, not a target, so same-package target attribution is unavailable.
2. `git log -S` candidates are reported, never chosen; read the candidate commits before drawing a history conclusion.
3. Macro bodies carry no dependency sites because syntax analysis does not parse their token streams.
4. The family filters match identifiers, not resolved receivers: `io::stdin().is_terminal()` reads as a crate predicate wherever the crate also defines an `is_terminal`; a qualified callee is crate-defined when any of its segments is; a field composes only when exactly one struct declares it, so a field shared by two related structs is dropped; the one-module gate resolves callees by name, so a name defined in several modules widens the candidate set.
5. Call shapes order references by line, and an owner type's fold sits at its first reference; two references on one line sort by name.
6. A definition rewritten wholesale in one later commit blames entirely to that commit, so a vestigial candidate's "introducing" commit is the last one to rewrite it; the summary shown is still the one to read.
7. A shape family key is its callee set, so it changes when one member gains a call; a verdict on a sibling family is best written once the shared choreography is stable.
8. The flag join matches a reference to the call on the same line by callee name; a line holding two calls of one callee takes the first, and a reference with no call on its line is counted as skipped, never guessed.
9. A `pub use` path resolves by syntax: an explicit `crate`/`self`/`super` path once, a bare path first as a child of the declaring module and then from the crate root. A re-export chain deeper than eight hops, or one that passes through a module the crate does not define, reads as unresolved or foreign rather than guessed.
10. `assemblers` resolves a callee through the file's `use` lines and explicit `crate`/`self`/`super` paths; a method call, a `Self::` call, or a name the file does not import is not attributed, so the count is a floor.
11. `[[rehome]]` counts every `pub` item with the name under `to`, a `pub use` re-export included, so the destination declares the item once and re-exports it nowhere.
