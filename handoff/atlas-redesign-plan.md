# Handoff: Atlas redesign — four verbs, a gate that sees every dependency, and an executable pass proof

Written 2026-09-01 by a refactor-review agent (Claude) after reading `docs/contributing/atlas.md`, running every verb on `crates/rimz/src`, having three explorers map `xtask/src/atlas/` at symbol level, and weighing a second from-scratch design from a separate window. Two earlier reviews of the same tool sit beside this file: `handoff/atlas-review-fable.md` and `handoff/atlas-review-sol.md`. They are references, not instructions; this plan supersedes where they differ.

## Goal and where it came from

Atlas (`cargo xtask atlas`) exists to guide refactor-review agents: where to dig, what evidence backs a finding, and whether a pass improved the tree. The owner has said the tool is not yet in use, authorized changing anything about it, and asked for one plan that makes it fit the workflow it serves. The workflow, from the review agent's own contract, is: survey a scope → learn one module (real callers, heaviest quoted call site, zero-production-referrer items, duplicated knowledge) → write a from-scratch target → per candidate, find the introducing commit and whether it names a fix → hand a coder a plan with a line budget → after the pass, prove behaviour held, the interface shrank at the call site, lines fell, nothing outside scope moved → keep the landed pass landed with a gate.

The redesign has one consumer (that agent), one output format (Markdown to stdout), and four verbs, each serving exactly one step above. Nine verbs today; four after. Every deletion below is a verb, flag, section, or file with no counterpart in that workflow.

## Current state

- Branch: `main`, clean except the untracked `handoff/` directory (this file and the two reviews). Nothing broken; `cargo xtask checks` is green on HEAD.
- Source: `xtask/src/atlas.rs` (136 lines) + `xtask/src/atlas/` (23 files, 14,620 lines including inline tests). Gate consumer: `xtask/src/gates.rs:308-310` (`gate_conform` → `atlas::conform_ratchet`) and `:430-448` (`checks`). CI reaches conform only through `cargo xtask checks` (`.github/workflows/ci.yml:84-87`, `.gitea/workflows/ci.yml:71-74`). `xtask/src/invariants.rs` has no atlas rule.
- Target file: `refactor-target.toml`, schema v4, 73 module rules, 16 `upward-imports`, 25 `allowed-imports`, 4 stranglers, zero `surface-goal`, zero `upward-debt`, and a derived 41-singleton `layers` total order. 55 commits touch it; 6 are `chore(atlas): admit|ratchet`, 47 are feature commits bumping a budget.
- Index cache: `target/atlas/index-*.scip`, two most-recent retained. Cold index ~95s on this machine; warm `survey` 15s, `inspect` 4s.

## Findings (verified on the tree, anchored)

Numbered so the passes below can cite them.

1. **The gate sees `use` only.** `conform.rs:417,531,647,755` iterate `file.imports`; `FileSyntax.imports` is populated only by `UseCollector::visit_item_use` (`syntax.rs:574-605`). 5,372 inline `crate::…` paths outside `use` lines exist in `crates/rimz/src` (`rg -n '\bcrate::' crates/rimz/src --type rust | rg -v '^\S+:\d+:\s*(pub(\(crate\))? )?use ' | wc -l`). A layer rule closes by respelling `use crate::x::Y` as `crate::x::Y` in the body. `CallCollector::visit_expr_call` (`syntax.rs:1128-1151`) already joins call-path segments into `FnBody.callees` strings, so the syn walk has an anchor; it collects no bare `ExprPath`, type path, or macro path.
2. **The `esc` ratchet is satisfied by visibility narrowing.** Since atlas landed (`8a5c7e9da`, 2026-07-30), 25 commits both remove a `pub` item and add a `pub(crate)` one; 16 subjects read "keep … private / internal / surface-neutral / within API budgets" (`8cf28a491` is one line; `d08588bc2` six). None changes anything at a call site. `esc` counts declarations, so it cannot measure depth; `diff`'s `asm Δ` (`diff.rs:738-748`, max distinct items per calling function) can, but no gate or contract uses it.
3. **`rank --since` prints `+0 +0` for every split child.** `child_rows` sets both deltas `None` (`rank.rs:642-643`); `print_row` renders `None` as zero (`:793-808`). Verified: `rank --path crates/rimz/src --since HEAD~40` shows `cli +174`, every child `+0`, while `git diff --numstat HEAD~40 -- crates/rimz/src/cli/agents_cmd/` shows `exec.rs` +134/−19.
4. **Two unlabeled history counts.** `rank` prints `pace.commits` = all 3,201 scoped non-merge commits (`rank.rs:477-480`, `history.rs:437-515`); `survey`/`seams` print 801 = `ceil(3201 × 25%)`, the pace window chosen *before* the oversized-commit filter (`history.rs:199-224,292-297`).
5. **Survey's rank is a second, shallower rank.** `survey::rank_rows` (`survey.rs:504-576`) sorts top-level rows by raw `code` (`:222-228`) and prints only `code tests pub esc cx` (`:642-646`); `rank::sort_rows` (`rank.rs:523-533`) is churn-weighted and carries churn/pace/loc/esc/t/c/flags. The doc tells the reader to read churn-weighted rows first from an artifact that has none.
6. **Detectors are noise as reported.** Single callers: alphabetical first 20 of ~1,300 (agents 213, harness 237, sidebar 187 per `detector_counts`). Vestigial: every row `101d` (predicate at `detect.rs:132-192` fires on anything blamed to one commit before the window start; 569 of agents' 973 escaping items). Pass-through: half are typed boundaries. Repeated guards: `Guard.normalized` keeps identifiers and path spellings verbatim (`syntax.rs:1107-1125`), so one `io::ErrorKind::NotFound` policy lands as five rows (35+15+13+11+7 files) split by binding name (`err`/`source`/`error`) and path alias (`std::io::`/`io::`).
7. **Shapes split one finding into five clusters.** Complete-linkage at 0.35 (`shapes.rs:409-456`) puts `decode_hook` (opencode/copilot/pi), `decode_hook` (claude/qwen), `decode_hook` (droid/amp) in three clusters, plus `lifecycle_signal` ×3, spend parsers ×4, `answer_plan` ×3. `Member.name` is stored (`:42-47`) but never joined across clusters (`:180-187`).
8. **Two `esc` meanings.** `diff --path crates/rimz/src/harness` reports 597 (sum of per-child boundaries via `escaping_items` grouping by `module_for_path`, `diff.rs:612-641`, `modules.rs:35-55`); the `harness` conform rule reports 590 (one boundary, `conform.rs:619-643,860-870`). Both labelled `esc`.
9. **A dead strangler rule.** `refactor-target.toml:352-355` sets `symbol = "store::run_store"`, but `count_in_sources` splits on every non-identifier character including `:` (`conform.rs:908-911`), so the token can never match. Baseline 0, current 0, forever.
10. **Goals and debt are unused.** `surface-goal` and `upward-debt` have zero occurrences in the live target after 55 commits. Read/write sites: `target.rs:79,81,156-184`; `conform.rs:450-451,705-717,728-737,979-995,1066-1159`; `inspect.rs:505-577,669-690`; tests at `target.rs:241-271,377-387,445-459`, `conform/tests.rs:42-43,103-104,167-183,201-202,233-234,640-641`, `inspect/tests.rs:218-219,248-249`.
11. **No verdict persistence.** A detector hit dismissed as "typed boundary, keep" is re-read by the next agent; nothing records it, so the detector queue never shrinks.
12. **`--top` does not bound output.** Split children never consume `--top` (`rank.rs:437-459`, `print_row:824-826`); `rank --top 25` prints 270 lines; the adapters brief is 1,381 lines, 350 of them the Interface listing.
13. **Real signal exists and is worth keeping.** `inspect --from sidebar::enrich --to store` quotes `enrich_core` (`crates/rimz/src/sidebar/enrich.rs:487-729`) assembling 21 store items — a `deepen` finding with the evidence attached. `api --module store` lists `store::agent_context::write_record` as `pub`, production referrers 0, test referrers 23 — a delete-or-demote candidate. `diff --base` names escaping items added by a pass. The facts model (`facts.rs`), SCIP join (`references.rs`), production/test split (`sources.rs`), effective reach (`syntax.rs:108-133`, `modules.rs:26-33`), and the ratchet in `checks` are right and stay.

## Decisions already made (by the owner, or by this review under the owner's authorization)

- Markdown to stdout is the only output. `--json` and `--md` flags go; JSON v4 is not a compatibility surface (there is no consumer).
- `surface-goal` and `upward-debt` are deleted, not populated.
- The durable target is a ratchet: layers, dependency admissions, `surface-budget`, stranglers, verdicts. It does not carry code budgets. The designer proposed a gated `code-budget` per rule; rejected because every feature PR would edit the target and reflexive bumps make the number meaningless. Production-SLOC delta belongs in the pass contract (`diff --expect`), where a reviewer states it once per pass.
- Assembly (max distinct items per calling function across a boundary) is measured by `diff` and asserted by the pass contract, never by the gate: `conform` stays index-free so `checks` stays fast.
- `conform --init` and derived layer orders go. The v5 target is hand-written once in Pass 1 with human layer groups (proposed below); `--tighten` maintains it afterward. Nothing else in the workspace needs bootstrapping (`crates/rimz-presence-zellij` has no rimz-crate deps and no rules).
- Verdicts live in `refactor-target.toml` as `[[verdict]]`, keyed `(kind, key)`, with a mandatory `reason`. `conform` parses and preserves them and never enforces them; `survey` and `inspect` subtract them and report stale keys.
- Detector hypotheses carry no verb column. The doc's line "Atlas locates; it does not decide" stays. Evidence kinds (`repeated choreography`, `repeated policy`, `zero production referrers`, `wide assembly`) map one-to-one to the review verbs; a coder reading a verb column as a verdict is the failure mode avoided.

## Target design (the from-scratch shape)

```
cargo xtask atlas survey  [--path <scope>] [--top N]
cargo xtask atlas inspect --module <module|path> [--from <module|path>] [--item <module::Item>] [--top N]
cargo xtask atlas diff    (--base <ref> --path <scope> | --expect <pass.toml>) [--top N]
cargo xtask atlas conform [--ratchet | --tighten]
```

**`survey`** — syntax + history + metrics; no SCIP. Sections: (1) *Accretion rank*: disjoint rows (split leaves replace their parent above the fixed 8,000-SLOC threshold and consume `--top`), columns `code tests esc churn% pace cx t/c flags`, flags only `pin` and `hot`, sorted by `code × churn%`; rows owned by a bin target (`cli`) marked `bin`. (2) *Duplicated knowledge*: shape families (clusters merged when they share a member function name or ≥3 domain callees) and guard families (normalized over binding names and path aliases), one row per family with file count, member count, and up to five `path:line` locations, ranked by `files × mean sloc` for shapes and by file count for guards; families with a `keep` verdict suppressed and counted in the footer. (3) Footer: totals, `history: N scoped commits (pace window M)`, parse failures, suppressed verdicts, stale verdict keys. Deleted from today's survey: co-change assignments, divergence (both kinds), external providers, exact single callers, pass-throughs, vestigial, raw repeated guards, the `--json`/`--md`/`--no-index`/`--window`/`--noise-*`/`--guard-files`/`--split-above`/`--no-split` flags (thresholds become constants).

**`inspect --module X`** — SCIP required; the module dossier. Sections: (1) *Callers by assembly* (today's brief table: caller, items, max/fn, three heaviest functions). (2) *Heaviest assembly*: the `--from` caller's heaviest function or, without `--from`, the global maximum; quoted with the 80-line cap. (3) *Zero-production surface*: escaping, resolved items with production referrers 0, test referrers listed apart; unresolved definitions listed separately, never counted as zero. (4) *Repeated assembly*: groups of ≥3 of X's items that co-occur in ≥2 calling functions across ≥2 caller modules (the "same wiring at every site" evidence for `deepen`). (5) *Duplicated knowledge* touching X (survey families filtered to X). (6) *Providers*: count per provider module, names only with `--top` room. (7) Footer: covering target rules, parse/index gaps. With `--item`: (8) *Item evidence*: declared/effective reach, production and test referrers by module:function, the introducing commit (`git log --follow -S'<name>' --format='%h %ad %s' -- <file>`, oldest hit whose patch adds the declaration line; report all candidates when ambiguous, never pick one silently), any commit-message line matching `fix|bug|incident|regression|#\d+`, the persisted verdict. The Interface listing is gone; `--item` on one name replaces auditing it.

**`diff`** — SCIP required; the pass proof. `--base <ref> --path <scope>` compares base to working tree. `--expect <pass.toml>` reads `base` and `paths` from the contract instead. Sections: (1) *Expectations* (only with `--expect`): one landed/drifted row per assertion. (2) *Totals*: production SLOC, test SLOC, `esc` ±, dependency sites ± by layer direction, files ±. (3) *Call-site interface*: per caller→provider boundary crossing `paths`, `max/fn` base→current and the heaviest site on each side. (4) *Escaping surface*: named additions and removals. (5) *Dependencies*: added/removed dependency sites (use and qualified together) by module pair. (6) *Files*: changed paths inside and outside `paths`. (7) *Incomplete evidence*: parse failures, newly unresolved definitions. The per-module table with leaf-sum `esc` is deleted (finding 8); `esc` is always the boundary of the row's path.

Pass contract (ephemeral, `/tmp/<pass>.toml`, never committed):

```toml
version = 1
base = "<sha at pass start>"
paths = ["crates/rimz/src/store"]
max-production-sloc-delta = -120     # must be negative
[[assembly]]                         # zero or more
from = "sidebar::enrich"
to = "store"
max-items = 8                        # current < base and current <= max-items
```

`--expect` fails (exit 1) when production SLOC delta exceeds the ceiling, an assembly assertion does not hold, a changed path lies outside `paths`, or evidence is incomplete. No verify commands: `cargo xtask test`/`gate` prove behaviour; a structural diff does not run arbitrary commands.

**`conform`** — syntax only, root `refactor-target.toml` only. Default report: *Violations* (first 20 sites + total) and *Headroom* (20 budgets nearest their ceiling). `--ratchet`: silent on success, exit 1 on any violation; remains the `checks`/`gate` entry through `atlas::conform_ratchet`. `--tighten`: lower each `surface-budget` and strangler `baseline` to current, drop unused admissions, preserve verdicts, atomic write. Deleted: `--init`, `--layers`, `--path`, `--file`, `--status`, `--verbose`, `--json`.

Target schema v5:

```toml
version = 5
layers = [
  ["build_id", "utils", "tui", "ids", "lane", "sock", "osc", "testkit", "update"],
  ["workspace", "transcript", "agent_activity", "pane", "proc", "store", "config", "trust", "theme"],
  ["agents", "daemon_view", "message", "mux", "remote", "observability", "diag"],
  ["harness", "forge", "worktree", "child_process", "room", "remote_control"],
  ["sidebar", "sidebar_pane", "web", "reload", "uninstall", "disk_usage", "daemon_content", "channel"],
  ["bin", "cli"],
]

[[module]]
path = "crates/rimz/src/store"
upward-dependencies = ["agents::state"]    # renamed from upward-imports; now counts qualified paths too
surface-budget = 362

[[module]]
path = "crates/rimz/src/cli/remote/supervisor.rs"
allowed-dependencies = ["cli::remote::outage_ui", "remote"]   # renamed from allowed-imports
surface-budget = 9

[[strangler]]
symbol = "invalidate_snapshot_caches"      # validation rejects `::`
path = "crates/rimz/src/store"
baseline = 3

[[verdict]]
kind = "pass-through"                      # pass-through | guard | shape | item
key = "agents::adapters::codex::account::decode_auth"
reason = "Typed boundary: serde error wrapped in the domain error type."
```

The six layer groups follow the code map in `AGENTS.md` (foundation → records → domain → orchestration → surfaces → cli). They are the review agent's proposal under the owner's "decide the final state" authorization; the owner may regroup. Peers inside a group import freely; every cross-group upward dependency that exists at migration time is admitted verbatim so `--ratchet` stays green on the migration commit. `foo.rs` and `foo/` fold into one rule `foo`.

**Dependency facts.** A new `DependencySite { from_file, line, enclosing_fn: Option<FunctionId>, target: resolved module path, item: Option<String>, spelling: Use | Qualified }` in `syntax.rs` replaces `ImportedItem` for every consumer. Collected from `use` items (as today) and from every `crate::`/`self::`/`super::`/workspace-crate-rooted path in expressions, types, trait bounds, and macro invocation paths, excluding `#[cfg(test)]` regions exactly as imports and functions are excluded today; resolved through the existing `resolve_import_path`/`resolved_internal_import`; deduplicated per (file, target, item). This closes finding 1 for `conform`, `diff`, and `inspect` at once.

**What is deleted with no counterpart:** verbs `rank`, `seams`, `api`, `shapes`, `brief`; files `brief.rs`, `api.rs`, `seams.rs`; `rank --since`; every `--json`/`--md` renderer; `--no-index` everywhere (survey and conform never index; inspect and diff always do); the vestigial detector; the single-caller detector as a list (the `single` fact survives only as a column inside `inspect --item`); co-change edges, components, and divergence; the survey/brief Interface listing; `conform --init/--status/--layers/--path/--file/--verbose`; `surface-goal`, `upward-debt`; the `brief --all --out-dir` writer; the diff module table and its leaf-sum `esc`.

## Contract for the coder

Behaviour-preserving for the gate: `cargo xtask checks` and `cargo xtask gate` stay green on every commit, and the migration commit admits rather than fixes any newly visible upward dependency. Net-subtractive: `xtask/src/atlas` production SLOC (tests apart) falls by at least 2,500 from 14,620 total; the doc `docs/contributing/atlas.md` falls below 120 lines. A bug found on the way in RimZ source (e.g. a real upward dependency the new collector exposes) is admitted and reported back, not fixed in this program. Each pass lands as its own reviewable commit set; the tree works before and after each.

## Line budget

| pass | removes | adds | net |
| --- | --- | --- | --- |
| 1 gate + v5 | `conform.rs` init/status/goal/debt (~450), `target.rs` goal/debt (~80), `inspect.rs` debt rows (~90) | `DependencySite` collection + resolution (~200), verdict model (~120) | ≈ −300 |
| 2 inspect | `brief.rs` (698), `api.rs` (769), `detect.rs` vestigial + single-caller list (~120) | inspect sections 3–8 (~500) | ≈ −1,090 |
| 3 survey | `seams.rs` (855), `rank.rs` CLI/render/`--since` (~400), `shapes.rs` CLI/render (~150), `survey.rs` noise sections + `--json` (~350) | shape/guard families (~150) | ≈ −1,600 |
| 4 diff | `--no-index` branch, module table, `--md`/`--json`, `--file` (~350) | contract parser + expectations + call-site section (~300) | ≈ −50 |
| 5 docs | `atlas.md` 219 → ≤120; `atlas.rs` help text | — | — |

Target after all passes: atlas production ≤ 11,500 SLOC. A pass that grows its net is drifting; stop and report.

## Prerequisites

- Pin before Pass 1: run `cargo xtask atlas conform` and `conform --ratchet` on HEAD and keep the output in `/tmp/atlas-before/`; run `diff --base HEAD~40 --path crates/rimz/src --no-index --md` there too. These are the before-pictures for Pass 1 and 4 verification.
- Ordering: 1 → 2 → 3 → 4 → 5. Pass 2 assumes `DependencySite` and verdicts exist (Pass 1). Pass 3 assumes `inspect` already carries the API/caller evidence (Pass 2) so `api`/`brief` deletion has landed. Pass 4 assumes the diff module table's only remaining consumer (`asm Δ`) has moved into the call-site section. Pass 5 last; each earlier pass edits only the doc lines it deletes.
- Read before touching: `xtask/src/atlas/facts.rs` (whole), `syntax.rs:22-105,574-742,1107-1151`, `modules.rs:26-55,86-143`, `references.rs:71-89,109-217`, `conform.rs:138-264,381-461,600-800,860-1005`, `target.rs:13-203`, `diff.rs:243-366,612-748,980-1060`, `inspect.rs:359-503`, `brief.rs:427-492`, `api.rs:231-345,414-467`, `shapes.rs:158-201,409-456`, `detect.rs:197-243`, `history.rs:199-297,437-515`.

## Pass 1 — Close the gate hole, migrate to v5

Files: `xtask/src/atlas/syntax.rs`, `syntax/tests.rs`, `target.rs`, `conform.rs`, `conform/tests.rs`, `inspect.rs`, `inspect/tests.rs`, `diff.rs`, `refactor-target.toml`, `docs/contributing/atlas.md` (schema section only).

1. **deepen** `syntax.rs`: add `DependencySite` and a `DependencyCollector` visitor (sibling of `UseCollector`, `:574`) implementing `visit_item_use` (fold the existing flatten logic in), `visit_expr_path`, `visit_type_path`, `visit_path` for trait bounds, and `visit_macro` (path of the invocation). Reuse `resolve_import_path` (`:718`) and `resolved_internal_import` (`:645`). Record `enclosing_fn` from the same span logic `FnCollector` uses. Skip `cfg(test)` as `UseCollector` does (`:580-582`). Replace `FileSyntax.imports: Vec<ImportedItem>` with `dependencies: Vec<DependencySite>`; delete `ImportedItem`. Tests: `dependency_sites_include_inline_crate_paths_and_exclude_tests`, `dependency_sites_dedupe_use_and_qualified_spellings_of_one_target`.
2. **rehome** `conform.rs`: the four `file.imports` loops (`:417,531,647,755`) read `file.dependencies`; a site's direction is judged identically for `Use` and `Qualified`. Violation output prints `spelling` so a reader sees which kind opened the hole. Test: `conform_rejects_upward_dependency_after_use_is_inlined`.
3. **delete** in `conform.rs`: `initialize`, `greedy_layers`, `direct_rule_path`, `--init/--layers/--path` parsing (`:148-167,354-461,519-585`); `--status` mode and `print_status` (`:212-223,1066-1159`); `--file`, `--verbose`, `--json` (`:318-342`); goal/debt evaluation (`:705-717,728-737`) and their `tighten` branches (`:979-995`). Keep `evaluate_with_facts`, `enforce`, `tighten`, the default report (now *Violations* + *Headroom*), and `ratchet`.
4. **delete** in `target.rs`: `surface_goal`, `upward_debt` fields and validation (`:79,81,156-184`); rename `upward_imports` → `upward_dependencies` and `allowed_imports` → `allowed_dependencies` (TOML keys `upward-dependencies`, `allowed-dependencies`); version accepts only `5`; add `Verdict { kind: VerdictKind, key, reason }` with `(kind,key)` uniqueness and non-empty `reason`; strangler validation rejects `symbol` containing `::` (finding 9). `write` round-trips verdicts untouched. Tests: `target_v5_rejects_goal_and_debt_fields`, `target_v5_preserves_verdicts_when_tightened`, `strangler_symbol_must_be_one_identifier`.
5. **rehome** `inspect.rs:505-577,669-690`: `RuleRow` loses `debt`; keep direction and admission.
6. **rehome** `diff.rs:653-687` (`upward_edges`) and `:752-781` (`use_edges`): read `dependencies`; the "use edges" section becomes "dependency sites" (both spellings). Full diff reshaping waits for Pass 4.
7. **rewrite** `refactor-target.toml` to v5 by hand: the six layer groups above; one rule per module folding `foo.rs`+`foo/`; drop the `store::run_store` strangler; run `cargo xtask atlas conform` and admit every reported upward site as `upward-dependencies` on its rule (expect new admissions — the qualified-path collector will surface sites the `use` graph hid; admit, do not fix); run `--tighten`; confirm `--ratchet` is silent. Commit the target rewrite separately from the code so the diff is reviewable.
8. **update** `docs/contributing/atlas.md` schema section to v5; delete the goal/debt and `--init` prose. Delete the goal/debt tests listed in finding 10.

Verify: `cargo xtask test 'atlas::syntax::tests'`, `cargo xtask test 'atlas::conform::tests'`, `cargo xtask test 'atlas::target'`, then `cargo xtask checks`; `cargo xtask atlas conform --ratchet` exits 0 and prints nothing; hand-inline one `use crate::agents::…` in `crates/rimz/src/store/` into a body path, confirm `--ratchet` now fails, revert.

## Pass 2 — Fold module learning into `inspect`

Files: `xtask/src/atlas/inspect.rs`, `inspect/tests.rs`, `references.rs`, `history.rs`, `detect.rs`, `atlas.rs`; delete `brief.rs`, `api.rs`.

1. **deepen** `inspect.rs`: `--module` becomes the primary argument; `--from` optional; `--item` optional; `--top` default 20; Markdown only (delete `--json`, the `compact_json` path `:580-597`, and `--md`). Sections in the order given in the target design.
2. **rehome** into `inspect.rs`: `brief::callers_from_edges` (`brief.rs:427-492`) → *Callers by assembly*; `api::item_occurrence` and the unref/test-only/unresolved classification (`api.rs:310-345,414-467`) → *Zero-production surface*; new *Repeated assembly* built from `references::Edge.from_fn` grouping (item-set co-occurrence across `FunctionId`s, then across caller modules); *Duplicated knowledge* from the family builders Pass 3 will move into a shared `families.rs` — until then call `shapes::cluster` and `detect::guards` directly and filter to the module.
3. **deepen** `history.rs`: `introducing_commits(file, name) -> Vec<Commit>` running `git log --follow -S'<name>' --format='%h %ct %s' -- <file>` and, for each candidate, checking that its patch adds a line declaring `name` (reuse `git show <sha> -- <file>` scan; no fuzzy pick). `inspect --item` prints all candidates when more than one qualifies. Add `fix_markers(subject_and_body) -> Vec<String>` matching `fix|bug|incident|regression|#\d+`.
4. **delete** `brief.rs` (whole: Interface listing, `--all --out-dir`, its markdown/plain renderers, tests `:648-697`), `api.rs` (whole, tests `:699-768`), and in `detect.rs` the `vestigial` (`:127-192`) and `single_callers` (`:80-125`) functions; keep `passthroughs` (as a per-item annotation in *Zero-production surface* and `--item`) and `guards`.
5. **delete** the `brief` and `api` arms in `atlas.rs:54-79` and their help lines.

Verify: `cargo xtask test 'atlas::inspect::tests'` with new tests `inspect_lists_zero_production_refs_separately_from_unresolved`, `inspect_groups_repeated_assembly_across_caller_modules`, `inspect_item_reports_every_validated_introducing_commit`, `inspect_item_surfaces_persisted_verdict`; then `cargo xtask atlas inspect --module crates/rimz/src/store --from sidebar::enrich --item store::agent_context::write_record` prints `enrich_core` quoted, `write_record` under zero-production with 23 test referrers, and its introducing commit.

## Pass 3 — Collapse `survey`; delete `rank`, `seams`, `shapes` as verbs

Files: `xtask/src/atlas/survey.rs`, `rank.rs`, `rank/tests.rs`, `shapes.rs`, `detect.rs`, `syntax.rs` (guard normalization), `history.rs`, `atlas.rs`, new `families.rs`; delete `seams.rs`.

1. **collapse** rank: `survey` consumes `rank::build_rows` + `rank::sort_rows` (make them `pub(super)`, accept already-loaded `Facts`); delete `survey::rank_rows` (`survey.rs:504-576`), `rank::Args`/`parse_args`/`print_report`/`print_row`/`--since` and the prior-facts load (`rank.rs:16-43,127-134,313-364,428-455,725-826`). Split leaves replace the parent row and consume `--top`. Delete `shallow`/`hub`/`thin` flags and `hub_refs`/`shallow_*` args; keep `pin` (churn ≥ 3%, t/c < 0.30) and `hot` (pace ≥ 1.5) as constants. Mark bin-owned rows `bin` (a row whose top module is declared only from `main.rs`; `modules.rs:117-143` + `ModIndex`).
2. **rehome** `families.rs`: `shape_families(facts)` = `shapes::cluster` output merged when two clusters share a member `name` or ≥3 domain callees; `guard_families(facts)` = `detect::guards` after the new normalization. Normalization in `syntax.rs:1107-1125`: alpha-rename every non-path identifier to `$0,$1,…` in first-appearance order; collapse a path to its last two segments (`io::ErrorKind::NotFound` ≡ `std::io::ErrorKind::NotFound`); keep exact spellings on each site so the report can list them.
3. **delete** in `survey.rs`: co-change (`cochange_clusters`, `divergence`, `providers`), single-caller/pass-through/vestigial/raw-guard sections, `--json`, `--md`, `--no-index`, `--window`, `--noise-*`, `--guard-files`, `--split-above`, `--no-split` (`:107-215,229-276,634-750`). The Facets request drops `references` and `blame`. One history count from `Facts`, labelled `history: N scoped commits (pace window M)` (finding 4).
4. **delete** `seams.rs` (whole; its `--module` callers view lives in `inspect` since Pass 2), `shapes.rs` CLI/render (`Args`, `parse_args`, `print_report`, `run`; keep `cluster`, `is_generic_callee`, similarity, and tests `:513-622`), the `rank`/`seams`/`shapes` arms in `atlas.rs`.
5. **rehome** `history.rs`: delete `cochange`, `fold_cochange`, `cochange_partners` (`:199-315`) and the `--max-commit-files` machinery; keep `Log`, pace, blame (blame is still used by nothing after vestigial went — delete `Blame` too unless `inspect --item` adopted it in Pass 2; it did not, `git log -S` replaced it).

Verify: `cargo xtask test 'atlas::rank::tests'` (`survey_ranks_by_churn_weighted_size`, `survey_rows_are_disjoint_and_top_bounded`), `cargo xtask test 'atlas::shapes::tests'` (`families_merge_clusters_sharing_a_member_name`), `cargo xtask test 'atlas::syntax::tests'` (`guards_alpha_normalize_bindings_and_path_aliases`); `cargo xtask atlas survey --path crates/rimz/src --top 20` is ≤ 80 lines, shows one `NotFound` guard family with ≥ 60 files, and one `decode_hook` shape family with 7 members.

## Pass 4 — Make the pass proof executable

Files: `xtask/src/atlas/diff.rs`, its tests (`:1335-1542`), `sources.rs`, `atlas.rs`, new `contract.rs`.

1. **deepen** `contract.rs`: parse the pass-contract TOML above; validate `version = 1`, non-empty `paths` under the root, negative `max-production-sloc-delta`, each `assembly` with resolvable `from`/`to` module selectors (reuse `inspect::ModuleSelector`).
2. **deepen** `diff.rs`: `--expect <file>` supplies `base` and `paths` (mutually exclusive with `--base`/`--path`); SCIP always required (delete the `--no-index` branch and every `Option` that existed only for it); delete `--md`, `--json`, `--file`; delete `ModuleRow`, `module_rows`, `assembly_max_delta` (`:980-1057`) and the leaf-sum `esc` path (`:612-641`) — `esc` is computed once per `paths` entry as a boundary, the same helper `conform::escaping_surface` uses (finding 8). Add *Call-site interface* (per caller→provider pair crossing `paths`: `max/fn` base→current with heaviest `FunctionId` each side, from `EdgeData::assembly` `:738-748`), *Expectations*, and *Files* listing changed paths inside and outside `paths` (from `git status --porcelain` plus `rust_files` set difference).
3. **rehome** `sources.rs`: expose the changed-path inventory for the whole worktree (tracked + untracked, any extension) so *Files* can flag out-of-scope movement.

Verify: `cargo xtask test 'atlas::diff'` with `diff_expect_requires_call_site_shrink`, `diff_expect_enforces_negative_sloc_budget`, `diff_expect_rejects_changes_outside_paths`, `diff_reports_boundary_esc_not_leaf_sums`; then write `/tmp/smoke.toml` with `base = HEAD~40`, `paths = ["crates/rimz/src/harness"]`, `max-production-sloc-delta = -1` and confirm `diff --expect /tmp/smoke.toml` exits 1 with the SLOC row drifted (harness grew +288 in that range) and reports `esc` 590 for harness, matching the conform rule.

## Pass 5 — Rewrite the doc and help to the final shape

Files: `docs/contributing/atlas.md`, `xtask/src/atlas.rs` (usage text), `AGENTS.md` (the one doc-map line, `:137`, wording only if the page title changes).

1. **rewrite** `atlas.md` to ≤ 120 lines: vocabulary (`esc`, churn%, pace, `max/fn`, family, verdict), the four commands with one example each, the pass-contract schema and the exit rule for `--expect`, target schema v5 with the verdict table, and the two caveats that survive (SCIP names a package not a target; `git log -S` candidates are reported, never chosen). Delete the nine-verb loop, the detector guide, JSON v4, the baseline-versus-target essay, the worked reading chain.
2. **rewrite** `atlas.rs` usage to the four verbs.

Verify: `cargo xtask docs` (link check) and `cargo xtask gate`.

## Verification for the whole program

```sh
cargo xtask checks                                    # after every pass
cargo xtask test 'atlas'                              # whole module, every pass
cargo xtask atlas conform --ratchet; echo $?          # 0, silent
cargo xtask atlas survey --path crates/rimz/src --top 20 | wc -l     # ≤ 80
cargo xtask atlas inspect --module crates/rimz/src/store --from sidebar::enrich --item store::agent_context::write_record
cargo xtask atlas diff --base HEAD~40 --path crates/rimz/src/harness
git ls-files xtask/src/atlas xtask/src/atlas.rs | xargs wc -l | tail -1   # ≤ ~11,500 production + tests apart
cargo xtask gate
```

## Suggested direction for the team, and what to verify first

Before Pass 1, confirm two claims this plan rests on: that `DependencyCollector` can be built on syn's `Visit` without a second parse of each file (it should share `analyze_file`'s AST, `syntax.rs:155-207`), and that the qualified-path collector on the live tree surfaces a bounded number of new upward sites (expect tens, not thousands — the 5,372 count includes same-layer and downward paths). If the second is wrong, Pass 1's target rewrite needs a coarser admission (`upward-dependencies = ["agents"]` rather than per-submodule) and the plan should say so.

The one open design question left to the planner: whether `inspect --module` without SCIP should degrade to Callers-by-`use` (cheap, weak) or refuse (current `inspect` behaviour). This plan says refuse; the gate never runs `inspect`, and the review agent always has 90 seconds.
