# Atlas: refactor analysis

`cargo xtask atlas` produces bounded Markdown evidence for architecture review. Its four commands survey a scope, inspect a module, prove a pass, and keep target constraints from regressing. Atlas locates and sizes; reading decides. Every row is a measurement, and the sections below say which measurements are findings on their own and which need a reader.

## Vocabulary

- **Escaping (`esc`)** — an item whose effective visibility leaves the measured module boundary. It counts what outside callers can reach, not every declared `pub` item.
- **Churn%** and **pace** — a module's share of scoped history commits (renames folded to current paths), and its share of the recent 25% window divided by its lifetime share; pace `1.0` is its historical rate and `1.5` or more is `hot`.
- **`cx`** — severity-weighted excess over the complexity warn thresholds, summed per function; `0` means every function is under threshold.
- **`max/fn`** — the greatest number of distinct target items one production function references. Every reference to one owner type (the type, its variants, its associated functions and methods) is one item, so a builder chain reads as `MessageRecord::{new, with_channel, with_sender, +3}` and counts once.
- **Family** — repeated knowledge grouped across functions, keyed for a verdict: a shape family by its shared callee set, a guard family by its normalized guard text. Families only name crate vocabulary (std and external idiom is dropped), and only findings are shown by default: a shape family needs **siblings** or three files averaging 40 SLOC; a guard family must compose crate knowledge — two crate-defined names, a field exactly one struct declares, or a crate path such as `RunStatus::Completed`. A guard that calls one crate predicate (`$0.is_provider_subagent()`) is use, not duplication. `--all` shows what the gate dropped; the footer counts it.
- **Siblings** — member files of one shape family that occupy the same role in sibling directories (`agents/adapters/*/spend.rs`): parallel implementations of one responsibility. Families with siblings rank first, then by SLOC in play (members × mean SLOC).
- **Verdict** — a durable, reasoned disposition of one item, pass-through, guard family, or shape family. Atlas reports stale verdict keys when their evidence disappears.
- **Dependency site** — a syntax-derived internal dependency written as either a `use` or a qualified path. Sites are deduplicated per file by resolved module and item.
- **Vestigial candidate** — an escaping item at most one production site reaches, inside or outside the module, whose definition lines all blame to one commit, taken as the one that introduced it. `pins a fix` marks an introducing commit that reads as a fix: read it before deleting.

## `survey` — map a scope

`survey` reads the working tree and history without building a SCIP index. `--path` defaults to `crates/rimz/src`; `--top` to 20. Sections: `rank`, `hot`, `debt`, `shapes`, `guards`, `footer`.

- `rank` — per module: code, tests, `esc`, churn%, pace, `cx`, test/code ratio, flags. Default order is accretion (code × churn); `--by <code|esc|churn|pace|cx|tc>` re-sorts (`tc` ascending, thinnest tests first). Flags: `pin` (churn ≥ 3% with t/c < 0.3), `hot` (pace ≥ 1.5), `bin` (declared only from `main.rs`), `cx` (top decile of modules with cx > 0), `thin` (t/c < 0.3 with 200+ lines).
- `hot` — the functions to open: top `cx × file churn%` with `file:line`.
- `debt` — per target rule touching the scope: upward sites grouped by admission, unadmitted providers, and each strangler's current count against its baseline. Every admission is a `rehome` candidate someone already named.
- `shapes`, `guards` — families above the finding gate; `--all` shows the rest.

```sh
cargo xtask atlas survey --path crates/rimz/src/store --by cx --top 20
```

## `inspect` — learn one module

`inspect --module <module|path>` uses exact SCIP references. `--from` selects a caller to quote (default: heaviest), `--item <module::Name>` adds item history, referrers, markers, and its verdict, `--all` shows families below the finding gate. Sections, in order: `verdict`, `callers`, `heaviest`, `surface`, `assembly`, `calls`, `shapes`, `guards`, `providers`, `footer`, `item`.

The **verdict** is five lines: escaping items and outside sites with the head that carries 80% of them; items only the module itself reaches and how many items can narrow, tallied by target visibility; the top assembly cluster and its caller count; the heaviest caller at its folded item count; vestigial candidates and how many pin a fix. Everything after it is the evidence.

The **surface** table is the `deepen` decision in numbers, one row per escaping item sorted by outside production sites: `reach` (how far its effective visibility goes: `extern`, `crate`, or a module) and `narrow to` (the narrowest visibility that still covers every production caller: `keep`, `pub(super)`, `pub(crate)`, `pub(in crate::…)`, or `private` when only the item's own module and its descendants reach it, or nothing does). A caller in a binary-only module such as `cli` sits outside the library crate, so its items read `keep`. Test reach (sites through the escaping interface against sites past it), vestigial candidates, zero-production surface, and unresolved definitions follow.

**Repeated assembly** prints one root per cluster — the smallest item set the most callers share, with its full caller list — and nests each deeper subset as `+ <extra items>: K of M functions`. **Call shapes** add order: one function's target references in source order, folded per owner type, consecutive repeats removed; functions sharing a sequence group with `×N`. A shape earns a row when several functions share three or more items, or when one function alone references five or more. A shape proves reach, not assembly: the ideal call for the common case, written beside it, is the call-site test.

**Guard families** in `inspect` are the crate-wide families that name an item this module defines; a path segment counts only under one of the module's own modules or types. Unresolved definitions exclude `mod` declarations and `pub use` re-exports, which SCIP never defines in place; the footer counts those as unmeasured.

```sh
cargo xtask atlas inspect --module crates/rimz/src/store --from sidebar::enrich --item store::agent_context::write_record
```

## `diff` — prove one pass

`diff --base <ref> --path <scope>` compares an indexed base with the indexed working tree: SLOC, boundary `esc`, call-site assembly (only the caller→provider pairs whose `max/fn` moved, with the unchanged count), dependency sites, changed files inside and outside the scope (grouped by module or top-level directory, bounded by `--top`), parse failures, and newly unresolved definitions. `--expect` instead reads the executable pass contract below; keep that ephemeral contract outside the worktree so it is not itself an out-of-scope change. Sections: `expectations`, `totals`, `interface`, `surface`, `dependencies`, `files`, `evidence`.

```sh
cargo xtask atlas diff --expect /tmp/atlas-pass-contract.toml
```

## `conform` — keep the target

`conform` compares the working tree with root `refactor-target.toml`. `--ratchet` fails on excess surface, strangler counts, or unadmitted dependencies; `--tighten` only lowers measured ceilings and removes unused admissions. A missing target passes.

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
paths = ["crates/rimz/src/store"]
max-production-sloc-delta = -1

[[esc]]
path = "crates/rimz/src/store"
max = 110

[[delete]]
item = "store::legacy_open"

[[assembly]]
from = "sidebar::enrich"
to = "store"
max-items = 3
```

`paths` must be non-empty root-relative boundaries, and `max-production-sloc-delta` must be negative. Each optional row proves one verb: `[[esc]]` caps a boundary's escaping items (the measurement `conform`'s `surface-budget` uses; the path must lie inside `paths`), `[[delete]]` names an item in `module::Name` form that must exist at `base` and be gone now, and `[[assembly]]` names resolvable caller/provider modules whose `max/fn` must both shrink and land at or below `max-items`. With `diff --expect`, exit is zero only when production SLOC is at or below the delta ceiling, every row holds, every changed path is inside `paths`, and evidence has no parse failure or newly unresolved definition; otherwise the command reports drift and exits nonzero. Version 1 contracts (no `esc` or `delete` rows) still load.

## Review workflow

The verbs map onto an architecture review pass, and the pass contract is where the review's plan meets the coder's proof.

1. **Survey and size.** `survey` on the crate, `--section rank,hot,debt`: the rank and hotspots pick the subtree paying the most accretion cost; the debt section and the `layers` in `refactor-target.toml` are the repository's recorded decisions, read before judging.
2. **Learn what it does.** `inspect --module <target> --json --out /tmp/<target>.json`, then narrow with `jq`. The verdict says whether the interface is wide or deep; the surface table says what each item can narrow to; call shapes and repeated assembly quote the assembly callers pay; guards show the decisions callers make about the module's state. A shape proves reach, not assembly: brief a subagent with the heaviest caller and the top call shape and ask whether the caller constructs and wires the module's internals or merely names several of its items.
3. **Plan.** Write each candidate's proof as a contract row — `deepen` as an `[[esc]]` ceiling (and an `[[assembly]]` row when a named caller must get lighter), `delete` as `[[delete]]` rows, `collapse` and `rehome` as the SLOC delta and the `esc` ceiling they earn — and put the contract in the plan verbatim so the coder runs `diff --expect` and the reviewer reads drift instead of re-deriving it:

```toml
version = 2
base = "main"
paths = ["crates/rimz/src/message", "crates/rimz/src/cli/agents_cmd"]
max-production-sloc-delta = -60

[[esc]]
path = "crates/rimz/src/message"
max = 120

[[delete]]
item = "message::DEFAULT_SETTLE"
```

4. **Prove.** `diff --expect <contract>` after the pass; `conform --tighten` once it lands, so the ceilings the pass earned stay earned.

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

## Caveats

1. SCIP names a package, not a target, so same-package target attribution is unavailable.
2. `git log -S` candidates are reported, never chosen; read the candidate commits before drawing a history conclusion.
3. Macro bodies carry no dependency sites because syntax analysis does not parse their token streams.
4. The family filters match identifiers, not resolved receivers: `io::stdin().is_terminal()` reads as a crate predicate wherever the crate also defines an `is_terminal`, and a field composes only when exactly one struct declares it, so a field shared by two related structs is dropped.
5. Call shapes order references by line, and an owner type's fold sits at its first reference; two references on one line sort by name.
6. A definition rewritten wholesale in one later commit blames entirely to that commit, so a vestigial candidate's "introducing" commit is the last one to rewrite it; the summary shown is still the one to read.
7. A shape family key is its callee set, so it changes when one member gains a call; a verdict on a sibling family is best written once the shared choreography is stable.
