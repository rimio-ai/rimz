# Handoff: Atlas tool review and redesign

## Purpose and scope

The user asked for a brainstorm/co-design review of `cargo xtask atlas` as infrastructure for future refactoring agents: where to dig, how to form findings, and how to verify a structural improvement. This is a tool-design session, not a RimZ refactor. No source files were changed.

Read `docs/contributing/atlas.md`; implementation is under `xtask/src/atlas.rs` and `xtask/src/atlas/`; durable rules are in `refactor-target.toml`.

## Work performed

Commands exercised:

- Indexed `survey --path crates/rimz/src --md --top 20`
- Indexed standalone `rank --path crates/rimz/src --top 20`
- `brief --module crates/rimz/src/agents/adapters --md`
- `inspect --from cli::agents_cmd::check --to agents::adapters --md`
- `conform --status`
- Syntax-only historical `diff --base '8295eafbd^' --path crates/rimz/src/harness --md --no-index`
- Boundary comparison using `diff --base HEAD --path crates/rimz/src/harness --no-index`, scoped `rank`, and verbose conform status

Use `RUSTC_WRAPPER=` for Cargo in this environment because sandboxed `sccache` failed to execute.

Observed survey totals: 212,732 production SLOC, 177,304 test SLOC, 4,129 escaping items. Strong repeated-shape evidence appeared in adapter `decode_hook`, lifecycle mapping, spend parsing, and question-answer planning.

The adapter brief plus inspect produced an important negative result: although `agents/adapters` is large, its heaviest external caller assembled only three items. `cli::agents_cmd::check::run_check` was ordinary orchestration, not evidence of a missing deep Module. Atlas prevented a superficial size-based finding.

`conform --status` reported zero module goals and zero upward debt, plus four stranglers. The current target is a useful regression baseline with local ratchets, but not a broad architecture destination.

## Consolidated verdict

Atlas has the right philosophy and a strong technical foundation. Use it as an evidence collector and structural regression gate, not as an oracle or universal quality score.

Strong existing choices: effective `esc` rather than raw `pub`; exact SCIP references without heuristic fallback; production/test separation; one lazy cached Facts seam at `xtask/src/atlas/facts.rs`; quoted call-site evidence in `inspect`; named structural movement in `diff`; durable target ratcheting; versioned JSON; explicit preservation of human judgment over requirements, ownership, history, and target design.

The central weakness is packaging: Atlas gathers better evidence than it presents. Agents must assemble too many reports, filter sections known to be noisy, reconcile boundary meanings, and manually restate the intended pass before Atlas can evaluate it.

## Review of the other agent’s report

The report materially strengthens the review and should be incorporated with qualifications.

### Agree

- `survey` is too noisy as an agent-facing default.
- Survey rank is not canonical: it sorts by raw code at `xtask/src/atlas/survey.rs:222` and renders only code/tests/pub/esc/cx at `xtask/src/atlas/survey.rs:642`, while `docs/contributing/atlas.md:54` says to read churn-weighted rows. Standalone `rank` has churn, pace, loc/esc, test ratio, and flags.
- The 800-commit survey history and 3,199-commit rank history need distinct labels; the former is filtered co-change history.
- Repeated `NotFound` guards are split across aliases and binding names; group them into one symbol-aware family while retaining exact spellings/sites.
- Single callers should be grouped by defining boundary and caller boundary, not printed as 1,325 alphabetic items.
- Status should say “baseline only” when a target has zero goals and debt.
- `foo.rs` plus `foo/` should default to one conceptual Module identity, while still allowing an explicit nested boundary.
- Deletion evidence should include introducing commit subjects and fix/incident/issue markers.
- Reverse assembly/co-occurrence is valuable: show target item groups repeatedly assembled across callers.
- A machine-readable pass expectation should make `diff` self-checking.
- Document `conform --file <draft>` as a target-feasibility probe.

### Push back or qualify

- Refactor plus ratchet commit sequences prove adoption and preservation, not that every pass improved architecture or that Atlas caused it.
- Ten `store` admissions expose current cross-layer knowledge but do not alone prove `store` is conceptually a high-layer hub; durable schema knowledge may be load-bearing.
- Do not make `conform --init` refuse a truthful baseline. Keep baseline generation, label it clearly, and offer SCC/cut evidence separately. SCC condensation cannot infer semantic product layers.
- Do not use percentile-relative flags; that guarantees findings on healthy trees. Keep absolute facts and thresholds, add distribution context, and mark unassessable bin-owned rows.
- A joined queue may emit `hypothesis: collapse`, not a verdict. Choosing the boundary, semantic equivalence, and paid-fix history still needs judgment.
- Tests referencing non-escaping items are not automatically leakage; private unit tests may be legitimate. Report external/sibling boundary crossings and tests coupled to items named in the proposed pass.
- Guard normalization must retain exact predicates to avoid merging different policies.
- The other report says “four verbs” but later proposes six. Use the coherent four-workflow Interface below.

## Strong changes in order

### 1. Rehome conform onto dependency edges, not `use` spelling

`conform` checks `file.imports` at `xtask/src/atlas/conform.rs:645` and `xtask/src/atlas/conform.rs:755`. Inline qualified paths can cross a layer without the equivalent `use` violation. Collect syntactic `crate::`, `self::`, and `super::` paths in the cheap syntax pass so conform, divergence, and diff share one dependency meaning without requiring SCIP in the gate.

### 2. Collapse duplicate report semantics

Make survey consume the canonical rank query rather than maintain a shallow parallel implementation. Apply the same rule to API, seam, and shape facts: one query model, multiple renderers.

### 3. Deepen and bound the agent Interface

`--top 20` still prints every recursive child; rank produced roughly 6,600 tokens. The adapter brief exceeded 15,000 tokens even after skipping its Interface header. Bound total rows/bytes by default. Require explicit inclusion for recursive children, full Interface listings, and raw detectors. Every candidate should carry its exact next probe command.

### 4. Separate baseline, architecture target, and pass contract

Keep the truthful baseline. Loudly report when no destination exists. A real target uses human peer layers, surface goals, debt, and stranglers. The docs say the target includes pass order and line budget at `docs/contributing/atlas.md:23`, but schema v4 at `xtask/src/atlas/target.rs:69` cannot record them. Put transient scope, verb, line/file/interface expectations, prerequisites, and verification commands in a separate pass contract.

### 5. Evaluate a declared hypothesis

Do not create a composite score. Compare arbitrary base and head revisions against the pass contract and report behavior gates, expected target/dependency/assembly/file/strangler movement, line arithmetic, unexpected movement, and landed/drifted per expectation. Add `--head`; the historical comparison was contaminated by later working-tree commits.

### 6. Use one boundary identity everywhere

`diff --path crates/rimz/src/harness` reported 597 `esc` as a sum of child boundaries, while the whole-harness conform rule reported 590. Label `boundary esc` versus `sum of leaf esc`; dossiers, target rules, and evaluation must use the same boundary.

### 7. Attach detectors to candidates

Remove or rename default `vestigial`; only surface deletion hypotheses for zero production refs plus history. Keep pass-through focused, group single callers, normalize guard families, and hide whole-scope co-change components.

### 8. Add reverse assembly and targeted test coupling

Report repeated target-item co-occurrence across caller functions with counts and representative sites. Report tests crossing the reviewed boundary or touching pass-named items, not every private unit test.

## From-scratch Interface

Keep Facts and SCIP. Expose four workflows:

1. `locate` — bounded separate queues for accretion, assembly, repeated knowledge/choreography, and open target debt. Raw rank/api/seams/shapes become lenses or diagnostic JSON.
2. `inspect` — one Module or dependency edge, both caller directions, item co-occurrence, providers, heaviest call sites, relevant history, test coupling, and target rules.
3. `evaluate` — arbitrary base/head comparison against a pass contract, absorbing common `diff` plus `rank --since` work.
4. `conform` — baseline generation, target feasibility, status, ratchet, and tighten; SCC/cut costs are evidence, not asserted design.

Ideal calls:

```text
atlas locate --path crates/rimz/src --limit 8
atlas inspect --module agents/adapters
atlas evaluate --base <ref> --head <ref> --pass <pass.toml>
atlas conform --ratchet
```

Atlas may emit verb hypotheses without violating “locates; does not decide” when it includes support, contrary/unknown evidence, and the next inspection command. The hypothesis is never the gate; human-reviewed target and pass contract remain authoritative.

## Recommended use until redesign

1. Use standalone `rank --no-split` to lock a bounded scope.
2. Use shapes, grouped guards, and target debt as corroboration.
3. Run brief then inspect the highest `max/fn`; filter Interface/detector noise.
4. Read quoted source and `git log -S` before deciding a verb.
5. Test human layer drafts with `conform --file <draft>`.
6. Record baseline SHA and expected deltas in the hand-off.
7. After implementation run indexed diff, conform status, behavior gates, then tighten.

## Bottom line

The improved conclusion is: Atlas should remain a measurement instrument, but measurement should include mechanical joins and evidence-backed hypotheses. The long-term target is not a smarter score; it is a deep Interface providing a bounded reading queue, one self-contained dossier, a real architecture target, and an executable pass contract.
