# Atlas review — hand-off

Co-design note from a refactor-review agent (Claude) after reading `docs/contributing/atlas.md`, running every verb on `crates/rimz/src`, and weighing a second review from GPT 5.6 Sol. Everything below that is stated as fact was verified against the tree on 2026-09-01; claims I could not verify are marked.

## Verdict

Keep Atlas. Its foundation is right and worth more than its output: `esc` over `pub`, exact SCIP references that fail loudly rather than fall back to grep, production/test separation, one facts model with lazy facets, and `conform --ratchet` in the gate so a landed pass stays landed. The Aug 2–5 program (`refactor(store|mux|sidebar|message)` followed by `chore(atlas): ratchet …`) shows the loop works end to end.

Two things stop it from guiding a refactoring agent today:

1. **The gate is porous.** `conform` and `diff` count only `use` imports. The crate has 5,118 inline `crate::…::` paths outside `use` lines (`sidebar/enrich.rs`: 44 inline against 23 `use`). A layer rule that ignores `crate::store::agent_context::read_all(...)` inside a body does not mean what the target file says it means, and a pass can "close" an upward import by rewriting the `use` as an inline path.
2. **It measures and never joins.** The survey prints nine sections, four of which the doc tells the reader to discount, and the only verb-level conclusion (collapse / delete / deepen / rehome) is assembled in the reader's head by walking shapes → brief → inspect → divergence by hand. The target file, which is supposed to give direction, is still the `--init` baseline: 41 layers in a derived total order, 0 goals, 0 debt, `store` admitting ten upward imports.

## What today's run showed

Warm-cache cost: `survey` 16s, `rank` 14s, `brief` 5s, `inspect` 3.5s, `api` 3.5s, `diff --base HEAD~40` 93s. Acceptable.

Useful results:

- `inspect --from sidebar::enrich --to store`: `enrich_core` assembles 21 distinct store items across 243 lines, source quoted. This is the call-site test mechanized and the single most valuable verb.
- `shapes`: `decode_hook` × 5 adapters (scores 492 / 198 / 121), `lifecycle_signal` × 3, spend parsers × 4, `question_answer_plan` × 3. A real `collapse` candidate for a provider-neutral hook decoder.
- `api --module store`: `test-only` (40 in agents, 18 in harness) and `single` columns — precise, mechanical, and the right evidence for "public for tests only" leaks.
- `diff`: escaping items ±, use/reference edges ±, `asm Δ`, files ± — the shape of proof a plan needs.

Noise in the same run:

- Survey rank drops churn%, pace, loc/esc, t/c, flags (`survey.rs:222` sorts by raw `code`); the doc says to read churn-weighted rows first, which the survey artifact cannot support.
- Survey header says `history: 800 commits`, `rank` says `3199` — the 800 is the co-change denominator after dropping oversized commits, unlabeled.
- `shallow` on 12 of the top 25 rank rows; `cli` ranked deepest (`esc 26, loc/esc 2020`) because `main.rs` owns it and its `pub` never escapes — the crate-ownership trap the doc warns about, presented as a result.
- Co-change reading assignments: one component containing every module.
- Divergence: 1 `cochange-without-edge` row, 19 "good news" rows.
- Vestigial: 20 rows, all `(101d)`, all live.
- Exact single callers: the alphabetically first 20 of 1,325.
- Repeated guards: the top five rows are one guard spelled five ways (`err.kind()==std::io::ErrorKind::NotFound` 35 files, `…io::ErrorKind…` 15, `source.kind()…` 13, `error.kind()…` 11, `source.kind()==std::…` 7). The tool's own best finding — one NotFound policy without a home across 81 files — is split by path alias and binding name.
- `diff --path crates/rimz/src/harness` reports esc 597 (sum of child boundaries); the `harness` conform rule reports 590 (whole-module boundary). Both labelled `esc`.
- `refactor-target.toml` carries `config.rs` + `config`, `diag.rs` + `diag`, `message.rs` + `message`, `pane.rs` + `pane`, `worktree.rs` + `worktree` as separate rules — Rust file layout leaking into the design file.

## Position on the GPT 5.6 Sol review

Agreed and adopted:

- **Gate qualified paths, not `use` spelling** (its #1). I missed this; it is the top item because it decides whether the target means anything. Do it in the syntax pass, no SCIP in the gate.
- **Survey rank duplicates `rank` with a shallower sort** (its #2). Verified. Collapse.
- **Baseline-only must be loud** (its #4) and **one canonical boundary count** (its #6). Verified.
- **Evaluate a declared pass, never a quality score** (its #5). I had rated this Speculative; the user's stated goal is "evaluate if the result improved", so it is in scope. Adopted, but folded into `diff` (below), not a new verb.
- **Vestigial out of the default report; pass-through and single-caller as annotations; hide the whole-scope co-change component.** Same conclusion from both reviews.
- **The doc promises the target states pass order and line budget; schema v4 cannot express them** (`target.rs:69`). Resolve by putting those in the pass contract file, not the target.

Pushed back:

- Its reading of `agents/adapters` — "heaviest outside caller assembles 3 items, so no missing deep module, Atlas prevented a superficial finding" — is half right. Low `max/fn` clears `deepen`; it does not clear the module. The accretion is *inside* adapters (`decode_hook` × 5), a `collapse`, and a caller-assembly lens will never see it. This is the concrete case for joining lenses instead of reporting them side by side.
- `locate` returning "several small reading queues" keeps the tool one step short. The queues are evidence kinds; the value is the join across kinds with a verb hypothesis attached. A hypothesis with its evidence rows is still locating, not deciding. (Decision for the owner: see the last section.)
- A separate `evaluate` verb is a sideways move. `diff` already computes everything; give it `--head` and `--expect`.

Things the other review did not cover that I keep: SCC/cycle detection for layer proposals (Atlas has none: `rg -i 'cycle|tarjan|condens' xtask/src/atlas` → nothing), item-level introducing-commit history, reverse-direction assembly, and tests-past-interface.

## Changes, ranked

### 1. Gate qualified paths — Strong

Files: `xtask/src/atlas/syntax.rs` (collect), `conform.rs:645` and `:755` (check), `diff.rs` (upward-import and edge counts).

Collect `crate::`, `self::`, `super::` path expressions in the syntax pass alongside `ImportedItem`; resolve through the same `resolved_internal_import`. `conform` counts both; `diff`'s "upward imports" line counts both; the `seams` import table gains a `qualified` column or folds them in. Expect the first run to surface new unadmitted upward sites; admit them as the new baseline in one commit so the ratchet stays green, then mark the ones the program means to close as debt.

Payoff: the target file becomes enforceable; a pass cannot launder an upward dependency by changing its spelling.

### 2. A joined `findings` view — Strong

Files: new `xtask/src/atlas/findings.rs`; `survey.rs` shrinks to rank + findings.

Join, per top-level module and per split leaf: shapes clusters (≥2 files), `Callers by assembly` rows above a relative threshold, `cochange-without-edge` pairs, single-caller items grouped by (defining module, caller module) pair with count ≥3, `test-only` items, repeated guards after normalization, and open debt/strangler counts. Emit ≤10 rows, each: verb hypothesis (`collapse` / `delete` / `deepen` / `rehome`), the evidence rows that produced it, and the exact next command (`inspect --from … --to …`, `api --module …`). Example of the row this run would produce:

```
collapse  agents/adapters/*::decode_hook   shapes 492/198/121 (5 files) · co-change 3 adapters, no edge · next: brief --module crates/rimz/src/agents/adapters
rehome    io::ErrorKind::NotFound guard    81 files after normalization · next: rg -n 'ErrorKind::NotFound' crates/rimz/src
rehome    store::agent_context → cli::hooks::lifecycle   7 single-caller items · next: api --module store::agent_context
deepen    store ← sidebar::enrich          max/fn 21 in enrich_core · next: inspect --from sidebar::enrich --to store
```

Raw detector sections move behind `--all-detectors`.

### 3. `diff --head <ref> --expect <pass.toml>` — Strong

Files: `xtask/src/atlas/diff.rs`; new pass-contract schema next to `target.rs`.

`--head` compares two revisions so a historical pass can be judged without working-tree contamination. `--expect` reads the pass contract the plan already states in prose:

```toml
verb = "deepen"
scope = "crates/rimz/src/store"
code-delta-max = -300          # production SLOC, must fall at least this much
esc-delta-max = -20
close-debt = ["agents::state"]
close-edges = ["sidebar::enrich -> store::agent_context"]
stranglers-to-zero = ["invalidate_snapshot_caches"]
verify = ["cargo xtask gate", "cargo xtask test --name store::"]
```

The report has four verdict lines: behaviour (verify commands' exit), declared movement (each expectation landed / not), arithmetic (Δcode, Δtests, Δesc, edges, files), and unexpected movement (escaping items or edges changed outside `scope`). No aggregate score. "Did quality improve" becomes "behaviour held, the declared hypothesis landed, the pass was net-subtractive, nothing else moved".

### 4. Turn the target into a design aid — Strong

Files: `conform.rs` (`--init`, `--status`), `target.rs`, `refactor-target.toml`, `docs/contributing/atlas.md`.

- `--init` refuses to emit a total order. It condenses the `use`+qualified graph into SCCs, proposes ≤6 layers, and prints for each candidate cut how many upward sites it would admit, so a human picks with numbers in front of them. SCCs are reported as the places a clean cut is impossible until code moves.
- `--status` with zero goals and zero debt prints `baseline only — ratchet active, no destination declared`.
- One rule per module: `foo.rs` and `foo/` fold into `foo`.
- One boundary definition: rules and dossiers report *boundary esc*; any aggregate is labelled `leaf-sum esc`.
- Document `conform --file <draft.toml>` as the feasibility check for a from-scratch layering (design twice, score each draft, keep the one with fewer admitted sites).
- Remove "pass order and line budget" from what the target promises; the pass contract in #3 carries them.

### 5. One rank — Worth exploring

Files: `survey.rs:222`, `rank.rs`.

Survey consumes `rank`'s report model and sort. Flags become relative (top quartile of the table) rather than absolute (`esc >= 20 && loc/esc < 120`). Modules owned by a bin target are marked `(bin)` and excluded from `loc/esc` ranking. Label the two commit counts (`history` vs `co-change history`).

### 6. Detector precision — Worth exploring

Files: `detect.rs`.

- Guards: normalize path aliases (`std::io::ErrorKind` ≡ `io::ErrorKind`) and binding names before hashing.
- Single caller: report only grouped by (defining module → caller module) with counts; the per-item list stays in `api --module`.
- Vestigial: rename to `stable`; surface only zero-production-referrer items, and print the introducing commit subject (blame is already computed) with a `fix|incident|#NNN` marker so a paid-for fix is never proposed for deletion.
- Pass-through: module view only.
- Co-change component: hide when it covers >80% of the scope.

### 7. Module view content — Worth exploring

Files: `brief.rs`, `inspect.rs`.

- `inspect --to X` (no `--from`): groups of X's items that co-occur inside calling functions across N caller modules — the "same assembly repeated at every site" evidence for `deepen`, which `shapes` only catches when whole functions are similar.
- Tests past the interface: tests referencing non-escaping items of the module (SCIP has this). That count is what a `deepen` pass will break and why the module is the wrong shape; today the brief's test line is `23330 test SLOC · 75 files · 64 inline regions`.
- Per-function churn for `pin` decisions.
- Interface listing moves behind `--interface` or to the end.

## From-scratch shape

Both reviews converge. Keep the facts model and the SCIP index as they are. Four operations:

1. `findings --path` — the joined, ranked, ≤10-row queue with verb hypotheses and next commands.
2. `module <path>` (today's `brief` + `inspect`, both directions) — callers by assembly, heaviest site quoted, providers, tests past the interface, covering rules, relevant shapes; nothing else by default.
3. `diff --base --head [--expect]` — the per-pass proof.
4. `conform` — init with a layer proposal, status that names baseline-only, ratchet, tighten.

`rank`, `api`, `seams`, `shapes` remain as lenses that `findings` and `module` consume; they stay runnable but stop being the workflow. `docs/contributing/atlas.md` shrinks to vocabulary + loop; the paragraphs explaining which sections to ignore disappear because the sections do.

## How a refactoring agent should use Atlas until then

1. `rank --path <scope> --no-split` (not the survey's rank) to pick a bounded target; read `cli` as unmeasured.
2. `shapes --path <scope>` for collapse candidates; `api --path <scope>` for `test-only` and `single` counts.
3. `brief --module <target>` → `Callers by assembly` → `inspect --from <caller> --to <target>` on the highest `max/fn`; read the quoted source.
4. Before proposing deletion: `git log -S'<symbol>' --oneline -- <path>` and read the introducing commit.
5. Draft the layering as a `conform --file draft.toml` and score it; write the pass contract as prose in the plan (verb, scope, Δcode, Δesc, debt to close, verification commands) until #3 lands.
6. After implementation: `diff --base <ref> --path <scope> --md`, then `conform --ratchet`, behaviour gates, `conform --tighten`. Until #1 lands, also `rg -n 'crate::<higher-layer>::' <scope>` by hand, because the gate does not see inline paths.

## One decision for the owner

The doc draws a line: "Atlas locates; it does not decide." #2 attaches a verb hypothesis to each finding. My position is that a hypothesis with its evidence rows and the command that would confirm or refute it is still locating — but it is the line the doc drew, and moving it is the owner's call. If the line stays, #2 still lands as evidence-kind queues with next commands and no verb column; the join is where the value is either way.
