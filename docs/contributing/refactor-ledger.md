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
| 1 | `store` → `agents` and `store` → `message` upward dependencies; decide the direction and close it | `debt`: store admits agents 134 sites, message 22; cycles agents ↔ store 46/134, message ↔ store 24/22, both cross-layer | landed pass-1 |
| 2 | Adapter sibling families in `agents/adapters/*`: collapse each onto one implementation with the divergences a fix pins | `shapes`: decode-hook family 9 members / 8 siblings (`*/mod.rs`, 935 SLOC in play); `spend.rs` cache 4 siblings (354); `ask.rs` plan 3 siblings (182); `install.rs` 2 siblings (90) | landed pass-2 |
| 3 | `mux` ↔ `sidebar` and `daemon_view` ↔ `mux` cycles | cycles mux ↔ sidebar 27/42 cross-layer; daemon_view ↔ mux 10/3 same layer; mux admits sidebar 27 sites | `daemon_view` ↔ `mux` landed pass-3; `mux` ↔ `sidebar` landed pass-4 |
| 4 | Homes for `store::workspace_record` (→ `workspace`) and `store::snapshot::compose_channel` (`transcript`, `harness::target` callers); the only upward L2 → store sites after pass 1 | pass-1 re-layer admissions: `workspace` 3 sites, `transcript` 1, `pane` 1 | landed pass-3 |
| 5 | `agents` → `mux` (`NamedKey`, `CommandSpec`), `daemon_view` (markers), `diag::rotating`, `observability`: adapter-side reach into peers exposed by the re-layer | pass-1 re-layer admissions: `mux` 6 sites, `daemon_view` 3, `diag::rotating` 3; no current `observability` site | landed pass-3 |
| 6 | `wakeup` below `store`: the wire sits in the `mux` layer, so the store reaches up to send | pass-4 leftovers: `store` → `wakeup` 2 production sites (`store/writer/publish.rs:179`, `store/gc/collect.rs:12`); the wire holds the two references that keep it there, `mux::ClientPaneView` (`wakeup/events.rs:66`) and `mux::focus_anchor::FocusNonce` (`wakeup/events.rs:73`) | queued |

### Pass 2 family verdicts

| family | members reviewed | verdict and pins |
| --- | --- | --- |
| hook decode | every built-in adapter and the process-plugin adapter (`Kiro` uses the default) | holds: the generic caller already sees one `HookOutput`; event choreography is provider policy. Fix pins: `929ba4745`, `75ff1da53`, `53cc53892`, `6d79cac0c`, `55685b741`, `7c42e0184`, `f93cecb04`, `491ecd20d`, `9acad2888`, `567a18bc8`, `940758fad`, `071bf1ba3`. |
| spend cache | the ten adapter `spend.rs` parsers | holds: `agents::spending` already owns pricing, cache entries, deduplication, and aggregation; providers retain format, replay, rewind, and cost precedence. Fix pins: `b23d1898f`, `5951e11bd`, `2d2bab7c3`, `3b1754c89`, `517de4f21`, `217577bcd`, `253cac4c9`, `6e0a6f814`, `46265e8b2`. |
| lifecycle tool classification | Claude, Codex, Qwen | holds: the classifiers live once; event tables remain provider policy. The hook-decode fixes above pin the divergent arms. |
| answer plan | Claude, Codex, Pi | holds: extraction is shared; key sequences implement different provider TUIs. Fix pins: `d3d072eab`, `bff26f369`, `0908a9a52`, `512cb7390`. |
| paired settings install | Copilot, Cursor | holds: `settings_json::commit_pair` is the shared transaction; cleanup rollback differs. Fix pins: `a84ad352b`, `0c5284908`. |
| `result.kind() != ValueRefreshKind::Cached` | six test-only sites in Antigravity, Claude, Codex, Kiro | holds: counters instrument the one production `StableValueCache` policy; `23ddf5697` pins Kiro's distinct invalidation branch. |

Module-pass notes: Copilot and Cursor still hand-build parallel `report_files` lists (`copilot/install.rs`, `cursor/install.rs`); Droid and each sibling retain provider-specific session-origin stamping. The store-free `spend.rs` invariant matches by filename, so a future shared spend helper outside a `spend.rs` needs the invariant widened with it.

### Pass 4 target

The direction the pass writes into `crates/rimz/src/mux/AGENTS.md` and `crates/rimz/src/sidebar/AGENTS.md`: `mux` never imports `sidebar`. The sidebar's wire (heartbeat record, event vocabulary, datagram send) is the leaf module `wakeup`, beside `mux` in the layering and above `store`, reached alike by `mux`, `store`, `remote_control`, and `sidebar`. Every runtime file the multiplexer writes or dispatches on (focus anchor, width target, pane topology, presence-desired) is owned by `mux`. A bound on another module's behaviour lives with that module, and `sidebar::timing` keeps only the sidebar's own cadences.

Site counts on the pass-4 rows below are taken at the pass base `8a4ca7bcd`, not at the program baseline.

Ordered teardown is room-owned, not deferred. `teardown_room` and `TeardownReport` move whole to `room/teardown.rs`, keeping the four steps in their original order: kill the session, purge the resurrection cache, `sidebar::sweep_orphan_runtime`, then `mux::recovery::sweep_orphan_processes` (`crates/rimz/src/room/teardown.rs:35-41`). The alternative the pass-3 planning left open, hoisting the runtime sweep out and leaving the function in `mux`, is rejected: the hoist runs the runtime sweep after the process sweep, and the process sweep kills leaked sidebar daemons (`mux/recovery.rs:131`), which moves the heartbeat and socket mtimes `sweep_orphan_runtime` decides on (`sidebar/mod.rs:387-405`). `mux/recovery.rs` keeps the guarded process sweep alone.

## Module verdicts

One row per module reviewed, at the granularity `survey` ranks. `holds` names the SHA reviewed and the scoped-commit count that reopens it (`git log --oneline <sha>.. -- <path> | wc -l`). `landed` points at the pass row. `candidate` is a survey pick not yet reviewed, with the signal that made it one.

| module | status | sha | reopen at | note |
| --- | --- | --- | --- | --- |
| `store` | landed pass-1 | — | — | seam reviewed; module interior remains a candidate |
| `agents` | landed pass-1 | — | — | seam reviewed; module interior remains a candidate |
| `agents/adapters` | landed pass-2 | — | — | sibling seam reviewed; provider interiors remain separate module candidates |
| `daemon_view` | landed pass-3 | — | — | seam reviewed; interior remains a candidate |
| `pane` | landed pass-3 | — | — | seam reviewed; interior remains a candidate |
| `workspace` | landed pass-3 | — | — | seam reviewed; interior remains a candidate |
| `mux` | landed pass-4 | — | — | seam reviewed; it now owns the focus anchor, the room width target, and the Zellij topology/presence-desired caches; interior remains a candidate |
| `sidebar` | landed pass-4 | — | — | seam reviewed; it keeps producer election, fusion, refresh lanes, and its own cadences; interior remains a candidate |
| `wakeup` | landed pass-4 | — | — | new module: the wire lifted out of `sidebar` (heartbeat record, event vocabulary, datagram send) |
| `agents/adapters/codex` | candidate | — | — | survey rank 3; app-server, broker, rollout, and install interiors unreviewed |
| `agents/adapters/claude` | candidate | — | — | survey rank 6; remote-control and install interiors unreviewed |
| `config` | candidate | — | — | esc 182, depth 35.9, churn 8.9%; 38 admitted upward sites |
| `message` | landed pass-1 | — | — | seam reviewed; module interior remains a candidate (baseline esc 157, depth 25.2) |
| `harness/schedule` | candidate | — | — | esc 168, depth 22.2 |
| `agents/spending` | candidate | — | — | esc 118, depth 39.9 |
| `cli/remote` | candidate | — | — | pin, thin, cx 34.0, pace 1.32; pins are the prerequisite |
| `sidebar_pane/render/sections` | candidate | — | — | thin (t/c 0.06) with 3.7k code; pins are the prerequisite |

## Admission intents

One row per upward dependency edge reviewed. `keep` means the edge is the intended shape and its reason; `close` names the seam pass that closes it. An edge with no row is unreviewed. Baseline: every admission in `refactor-target.toml` is unreviewed.

| from → to | sites at baseline | intent | reason / seam |
| --- | ---: | --- | --- |
| `store` → `agents` | 134 | keep | intended direction: the store folds the agent model (pass 1) |
| `agents` → `store` | 46 | closed | pass 1 |
| `store` → `message` | 22 | closed | pass 1 |
| `config` → `store::message` | 2 | keep | `AutoCompact` is config vocabulary and persisted record data; the persisted home wins |
| `agents` → `mux` | 6 | closed | pass 3 |
| `agents` → `daemon_view` | 3 | closed | pass 3 |
| `agents` → `diag::rotating` | 3 | closed | pass 3 |
| `pane` → `store::snapshot` | 1 | closed | pass 3 |
| `transcript` → `store::snapshot` | 1 | closed | pass 3 |
| `workspace` → `store::workspace_record` | 3 | closed | pass 3 |
| `agents` → `worktree` | 7 | closed | pass 3 |
| `store` → `daemon_view` | 2 | closed | pass 3 |
| `store` → `worktree` | 4 | closed | pass 3 |
| `mux` → `daemon_view` | 3 | closed | pass 3 |
| `workspace` → `worktree` | 1 | closed | pass 3 |
| `agents` → `child_process` | 2 | closed | re-layer: `child_process` is a process-lifecycle module and sits at L2 (pass 3) |
| `message` → `child_process` | 2 | closed | re-layer: `child_process` is a process-lifecycle module and sits at L2 (pass 3) |
| `diag` → `child_process` | 2 | closed | re-layer: `child_process` is a process-lifecycle module and sits at L2 (pass 3) |
| `daemon_view` → `remote_control` | 4 | closed | re-layer: `daemon_view` sits at L6, same layer (pass 3) |
| `agents` → `harness::target` | 2 | keep | `agent_handle` is the canonical address; `agent_channel` becomes `AgentState::channel()` in a later pass |
| `agents` → `harness::budget` | 1 | close | later pass: `BudgetPark`/`BudgetScope`/`BudgetWindow` are `AgentState` record vocabulary |
| `agents` → `harness::run` | 1 | close | later pass: a `LifecycleSignal` terminal disposition in `agents::lifecycle`, mapped to `RunStatus` in harness |
| `theme` → `agents` | 5 | keep | catalog and spec vocabulary are the shared language below the store |
| `trust` → `agents` | 1 | keep | catalog and spec vocabulary are the shared language below the store |
| `proc` → `agents` | 4 | keep | catalog and spec vocabulary are the shared language below the store |
| `config` → `agents` | 14 | keep | catalog and spec vocabulary are the shared language below the store |
| `mux` → `sidebar` | 27 | closed | pass 4 |
| `mux` → `remote` | 1 | closed | pass 4: `REMOTE_LINEAGE_ENV` is process vocabulary and moves to `proc` |
| `store` → `sidebar::{heartbeat,timing,wakeup}` | 4 | closed | pass 4 |
| `remote_control` → `sidebar::wakeup` | 1 | closed | pass 4 |
| `config` → `sidebar::timing` | 3 | closed | pass 4: the refresh trio is config vocabulary and is defined in `config` |
| `store` → `wakeup` | 2 | close | seam 6: the wire moves below the store once `ClientPaneView` reaches `pane` and `FocusNonce` reaches `ids` |
| `daemon_view` → `sidebar::timing` | 1 | keep | `EVENT_PANE_TTL` is the sidebar's event-mode cadence; daemon maintenance accepts a frame under it and reaches up for the bound deliberately |
| `mux` → `harness::launch` | 5 | close | later pass: shell-program and pane-name vocabulary belongs below `mux` |

## Pass log

One row per pass, newest last. Deltas are the `diff --expect` totals at merge.

| date | pass | scope (`paths`) | base | verbs landed | Δ prod SLOC | Δ esc | Δ dep sites | deferred |
| --- | --- | --- | --- | --- | ---: | ---: | ---: | --- |
| 2026-09-03 | pass-1 seam `store` ⇄ `agents`, `store` ⇄ `message` + `disk` lift | `crates/rimz`; `xtask/src/invariants.rs`; `docs`; `refactor-target.toml`; `AGENTS.md`; `CLAUDE.md`; `ARCHITECTURE.md` | `3e01afc71` | rehome ×7, delete ×7 | −4 | +4 | +20 | Seam 4: `workspace_record`/`compose_channel` homes; seam 5: agents → peer modules; `for_terminal_status` kept. Tightened surfaces: `store` 321, `message` 90. Contract ceiling loosened −20→−1 because the estimate omitted the one-line-per-file import splits a no-shim lift imposes and the method signatures the record's merge rules gained; measured −2 at step 7, −4 after the review round. Atlas advisories: multiple destination definition sites are reported as “not defined under”; cfg-gated items count as production while files behind an equivalent cfg-gated module do not. |
| 2026-09-04 | pass-2 seam adapter sibling families | `crates/rimz/src/agents`; `docs`; `refactor-target.toml` | `93c4c48d7` | rehome ×1, deepen ×1, collapse ×1 | −45 | −2 | +1 internal (crossing +0) | Five shape families and the cached-value test guard hold for the reasons recorded above. No upward edge was reviewed. Deferred module notes: Copilot/Cursor `report_files`, Droid origin stamping, and the filename-scoped `spend.rs` invariant. Tightened `agents` surface to 970. |
| 2026-09-04 | pass-3 seam #4 + #5 + `daemon_view` ↔ `mux` | `crates/rimz`; `docs`; `refactor-target.toml`; `AGENTS.md`; `ARCHITECTURE.md` | `d8361c248` | rehome ×10, deepen ×1, re-layer ×2 | +5 | −6 | +4 internal | `AgentState::channel()`; `BudgetPark`/`BudgetScope`/`BudgetWindow` into `agents`; `LifecycleSignal` terminal-disposition split; `PaneRef` method forms; `CommandSpec` runner collapse; Codex daemon spawn-failure and timeout warning text changes as declared. |
