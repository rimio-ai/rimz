# Atlas: refactor analysis

`cargo xtask atlas` produces bounded Markdown evidence for architecture review. Its four commands survey a scope, inspect a module, prove a pass, and keep target constraints from regressing.

## Vocabulary

- **Escaping (`esc`)** — an item whose effective visibility leaves the measured module boundary. It counts what outside callers can reach, not every declared `pub` item.
- **Churn%** — the share of scoped history commits that touched a module, after renames are folded to their current paths.
- **Pace** — the module's share of commits in the recent 25% window divided by its lifetime share; `1.0` is its historical rate and `1.5` or more is `hot`.
- **`max/fn`** — the greatest number of distinct target items referenced by any one production function in a caller module.
- **Family** — repeated knowledge grouped across functions: either similar call shapes or normalized guards. Each family has a stable key suitable for a verdict: a shape family is keyed by its shared callee set (`decode_catalog_hook+set_ask+with_worktree`), a guard family by its normalized guard text. A family is knowledge only when its key names something the crate defines: shape keys naming only std or external vocabulary (`Vec::new`, `Line::from`, `min`) and guard keys whose named identifiers are all std idiom (`is_empty`, `ErrorKind::NotFound`, `insert`) are dropped, and the footer counts both.
- **Siblings** — member files of one shape family that occupy the same role in sibling directories (`agents/adapters/*/spend.rs`): parallel implementations of one responsibility. Families with siblings rank first.
- **Debt** — the upward dependencies a target rule admits, counted at their sites, beside any it has not admitted, and each strangler's current count against its baseline. Every admission is a `rehome` candidate someone already named.
- **Verdict** — a durable, reasoned disposition of one item, pass-through, guard family, or shape family. Atlas reports stale verdict keys when their evidence disappears.
- **Dependency site** — a syntax-derived internal dependency written as either a `use` or a qualified path. Sites are deduplicated per file by resolved module and item.
- **Call shape** — one caller function's target references in source order with consecutive repeats removed; functions sharing a sequence group into one shape with its `×N`. A shape earns a row when several functions share three or more items, or when one function alone references five or more. The ideal call for the common case, written beside the shape, is the call-site test.
- **Test reach** — where test code reaches a module: through its escaping interface, or past it at boundary-visible items the interface does not expose (public items of a private submodule, `pub(super)` helpers). Private items are not indexed and stay unmeasured.
- **Vestigial candidate** — an escaping item at most one production site reaches, inside or outside the module, whose definition lines all blame to one commit, taken as the one that introduced it. A candidate whose introducing commit reads as a fix is marked `pins a fix`: read that commit before deleting.

## `survey` — map a scope

`survey` ranks production size, tests, `esc`, churn%, pace, complexity, and test/code ratio, lists recorded debt under the target rules touching the scope, then lists shape and guard families. It reads the working tree and history without building a SCIP index. `--path` defaults to `crates/rimz/src`; `--top` defaults to 20. Sections: `rank`, `debt`, `shapes`, `guards`, `footer`.

```sh
cargo xtask atlas survey --path crates/rimz/src/store --top 20
```

## `inspect` — learn one module

`inspect --module <module|path>` uses exact SCIP references to report callers, `max/fn`, the heaviest caller quoted at its target sites, the escaping surface measured from outside, repeated assembly, call shapes, families, providers, target rules, and stale item/pass-through verdicts. `--from` selects a caller and `--item <module::Name>` adds item history, referrers, markers, and its verdict. Sections: `callers`, `heaviest`, `surface`, `assembly`, `calls`, `shapes`, `guards`, `providers`, `footer`, `item`.

The surface section is the `deepen` decision in numbers. Its table lists every escaping item with outside production sites, distinct caller files, internal sites, test sites, and caller modules, sorted by outside sites; the summary line names how many items carry 80% of outside sites, how many have exactly one outside site, and how many only the module itself reaches (escaping wider than any caller needs). Test reach, vestigial candidates, zero-production surface, and unresolved definitions follow it.

Repeated assembly prints one root per cluster — the smallest item set the most callers share, with its full caller list — and nests each deeper subset under it as `+ <extra items>: K of M functions`. Call shapes complement it with order: `×3 (4 items): MessageRecord → new → with_channel → deliver_one` is three callers performing the same choreography. Guard families in `inspect` are the ones that name an item this module defines, wherever their sites are; `survey` lists families by where they sit. Unresolved definitions exclude `mod` declarations and `pub use` re-exports, which SCIP never defines in place; the footer counts those separately as unmeasured.

```sh
cargo xtask atlas inspect --module crates/rimz/src/store --from sidebar::enrich --item store::agent_context::write_record
```

## `diff` — prove one pass

`diff --base <ref> --path <scope>` compares an indexed base with the indexed working tree: SLOC, boundary `esc`, call-site assembly (only the caller→provider pairs whose `max/fn` moved, with the unchanged count), dependency sites, changed files (outside-scope paths as a count under `--base`, listed under `--expect`), parse failures, and newly unresolved definitions. `--expect` instead reads the executable pass contract below; keep that ephemeral contract outside the worktree so it is not itself an out-of-scope change. Sections: `expectations`, `totals`, `interface`, `surface`, `dependencies`, `files`, `evidence`.

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
cargo xtask atlas inspect --module crates/rimz/src/store --json --section assembly,surface --out /tmp/store.json
```

## Index cache

`inspect` and `diff` read a rust-analyzer SCIP index of the whole workspace, cached under `target/atlas/index-<key>.scip` where the key hashes every Rust source and `Cargo.lock`. The first run after any source change generates it, which takes over a minute and says so on stderr; later runs on the same tree reuse it. `diff` indexes `--base` in a temporary checkout under the same cache, so its first run generates twice. The two newest indexes are kept. `survey` and `conform` never build one.

## Pass contract v1

```toml
version = 1
base = "main"
paths = ["crates/rimz/src/store"]
max-production-sloc-delta = -1

[[assembly]]
from = "sidebar::enrich"
to = "store"
max-items = 3
```

`paths` must be non-empty root-relative boundaries, and `max-production-sloc-delta` must be negative. Each optional assembly row names resolvable caller/provider modules. With `diff --expect`, exit is zero only when production SLOC is at or below the delta ceiling, every assembly value both shrinks and is at or below `max-items`, every changed path is inside `paths`, and evidence has no parse failure or newly unresolved definition; otherwise the command reports drift and exits nonzero.

## Review workflow

The verbs map onto an architecture review pass, and the pass contract is where the review's plan meets the coder's proof.

1. **Survey and size.** `survey` on the crate, `--section rank,debt`: the rank picks the subtree paying the most accretion cost; the debt section and the `layers` in `refactor-target.toml` are the repository's recorded decisions, read before judging.
2. **Learn what it does.** `inspect --module <target> --section surface,calls,assembly,guards --json --out /tmp/<target>.json`, then narrow with `jq`. The surface table decides whether the interface is wide or deep; call shapes and repeated assembly quote the assembly callers pay; guards show the decisions callers make about the module's state. A shape proves reach, not assembly: brief a subagent with the heaviest caller and the top call shape and ask whether the caller constructs and wires the module's internals or merely names several of its items.
3. **Plan.** A `deepen` names the caller→provider pairs it shrinks and the `max-items` each lands at; a `delete` or `collapse` names its SLOC delta. Write those as the pass contract, and put the contract in the plan verbatim so the coder runs `diff --expect` and the reviewer reads drift instead of re-deriving it:

```toml
version = 1
base = "main"
paths = ["crates/rimz/src/message", "crates/rimz/src/cli/agents_cmd"]
max-production-sloc-delta = -60

[[assembly]]
from = "cli::agents_cmd::idle_compact"
to = "message"
max-items = 4
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
4. The family vocabulary filter matches identifiers, not resolved receivers: `io::stdin().is_terminal()` survives as a guard family wherever the crate also defines an `is_terminal`.
5. Call shapes order references by line; two references on one line sort by name.
6. A definition rewritten wholesale in one later commit blames entirely to that commit, so a vestigial candidate's "introducing" commit is the last one to rewrite it; the summary shown is still the one to read.
