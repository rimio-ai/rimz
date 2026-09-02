# Atlas: refactor analysis

`cargo xtask atlas` produces bounded Markdown evidence for architecture review. Its four commands survey a scope, inspect a module, prove a pass, and keep target constraints from regressing.

## Vocabulary

- **Escaping (`esc`)** — an item whose effective visibility leaves the measured module boundary. It counts what outside callers can reach, not every declared `pub` item.
- **Churn%** — the share of scoped history commits that touched a module, after renames are folded to their current paths.
- **Pace** — the module's share of commits in the recent 25% window divided by its lifetime share; `1.0` is its historical rate and `1.5` or more is `hot`.
- **`max/fn`** — the greatest number of distinct target items referenced by any one production function in a caller module.
- **Family** — repeated knowledge grouped across functions: either similar call shapes or normalized guards. Each family has a stable key suitable for a verdict.
- **Verdict** — a durable, reasoned disposition of one item, pass-through, guard family, or shape family. Atlas reports stale verdict keys when their evidence disappears.
- **Dependency site** — a syntax-derived internal dependency written as either a `use` or a qualified path. Sites are deduplicated per file by resolved module and item.

## `survey` — map a scope

`survey` ranks production size, tests, `esc`, churn%, pace, complexity, and test/code ratio, then lists shape and guard families. It reads the working tree and history without building a SCIP index. `--path` defaults to `crates/rimz/src`; `--top` defaults to 20.

```sh
cargo xtask atlas survey --path crates/rimz/src/store --top 20
```

## `inspect` — learn one module

`inspect --module <module|path>` uses exact SCIP references to report callers, `max/fn`, the heaviest quoted caller, zero-production-referrer surface, repeated assembly, families, providers, target rules, and stale item/pass-through verdicts. `--from` selects a caller and `--item <module::Name>` adds item history, referrers, markers, and its verdict.

```sh
cargo xtask atlas inspect --module crates/rimz/src/store --from sidebar::enrich --item store::agent_context::write_record
```

## `diff` — prove one pass

`diff --base <ref> --path <scope>` compares an indexed base with the indexed working tree: SLOC, boundary `esc`, call-site assembly, dependency sites, changed files, parse failures, and newly unresolved definitions. `--expect` instead reads the executable pass contract below.

```sh
cargo xtask atlas diff --expect pass-contract.toml
```

## `conform` — keep the target

`conform` compares the working tree with root `refactor-target.toml`. `--ratchet` fails on excess surface, strangler counts, or unadmitted dependencies; `--tighten` only lowers measured ceilings and removes unused admissions. A missing target passes.

```sh
cargo xtask atlas conform --ratchet
```

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
| `item` | `module::path::Name`; methods use `module::path::Owner::method` |
| `pass-through` | `module::path::Name`; methods use `module::path::Owner::method` |
| `guard` | normalized guard text printed as the family key |
| `shape` | shape-family name printed as the family key |

Shape and guard verdicts suppress matching families in `survey`; `inspect` displays item verdicts and stale item/pass-through keys. `conform` preserves verdicts but does not enforce their reasons.

## Caveats

1. SCIP names a package, not a target, so same-package target attribution is unavailable.
2. `git log -S` candidates are reported, never chosen; read the candidate commits before drawing a history conclusion.
3. Macro bodies carry no dependency sites because syntax analysis does not parse their token streams.
