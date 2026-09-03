# Refactor ledger

The memory between passes of the [refactor program](./refactor-program.md). An agent starting a pass reads this file first and appends to it last. Rows are never rewritten except to change a status; a superseded row keeps its text and gains a pointer to what superseded it. Every number here is reproducible by the command beside it, and the baseline is refreshed only by a pass row that names the new SHA.

## Program baseline

Taken at `73199f48c` (2026-09-03) with `cargo xtask atlas survey --out /tmp/atlas/survey.md`.

| measure | value |
| --- | ---: |
| production SLOC (`crates/rimz/src`) | 213086 |
| test SLOC in source | 177938 |
| escaping items | 5386 |
| `cx` (over-threshold excess, summed) | 472.6 |
| scoped commits in history | 3219 |
| admitted upward sites out of `store` | 185 |
| module cycles | 31 |

## Seam queue

Ordered. A seam pass at the head is proposed before any module pass. Status is `queued`, `in flight <branch>`, `landed <pass>`, or `rejected <reason>`. These rows are survey findings awaiting a review, not decisions; the review may reject one.

| # | seam | evidence at baseline | status |
| --- | --- | --- | --- |
| 1 | `store` → `agents` and `store` → `message` upward dependencies; decide the direction and close it | `debt`: store admits agents 134 sites, message 22; cycles agents ↔ store 46/134, message ↔ store 24/22, both cross-layer | queued |
| 2 | Adapter sibling families in `agents/adapters/*`: collapse each onto one implementation with the divergences a fix pins | `shapes`: decode-hook family 9 members / 8 siblings (`*/mod.rs`, 935 SLOC in play); `spend.rs` cache 4 siblings (354); `ask.rs` plan 3 siblings (182); `install.rs` 2 siblings (90) | queued |
| 3 | `mux` ↔ `sidebar` and `daemon_view` ↔ `mux` cycles | cycles mux ↔ sidebar 27/42 cross-layer; daemon_view ↔ mux 10/3 same layer; mux admits sidebar 27 sites | queued |

## Module verdicts

One row per module reviewed, at the granularity `survey` ranks. `holds` names the SHA reviewed and the scoped-commit count that reopens it (`git log --oneline <sha>.. -- <path> | wc -l`). `landed` points at the pass row. `candidate` is a survey pick not yet reviewed, with the signal that made it one.

| module | status | sha | reopen at | note |
| --- | --- | --- | --- | --- |
| `config` | candidate | — | — | esc 182, depth 35.9, churn 8.9%; 38 admitted upward sites |
| `message` | candidate | — | — | esc 157, depth 25.2; 76 items can narrow; constant params on `queue_synthetic`, `MessageRecord::new`, `gate_open`; heaviest caller `run_idle_compact` wires 7 items |
| `harness/schedule` | candidate | — | — | esc 168, depth 22.2 |
| `agents/spending` | candidate | — | — | esc 118, depth 39.9 |
| `cli/remote` | candidate | — | — | pin, thin, cx 34.0, pace 1.32; pins are the prerequisite |
| `sidebar_pane/render/sections` | candidate | — | — | thin (t/c 0.06) with 3.7k code; pins are the prerequisite |

## Admission intents

One row per upward dependency edge reviewed. `keep` means the edge is the intended shape and its reason; `close` names the seam pass that closes it. An edge with no row is unreviewed. Baseline: every admission in `refactor-target.toml` is unreviewed.

| from → to | sites at baseline | intent | reason / seam |
| --- | ---: | --- | --- |

## Pass log

One row per pass, newest last. Deltas are the `diff --expect` totals at merge.

| date | pass | scope (`paths`) | base | verbs landed | Δ prod SLOC | Δ esc | Δ dep sites | deferred |
| --- | --- | --- | --- | --- | ---: | ---: | ---: | --- |
