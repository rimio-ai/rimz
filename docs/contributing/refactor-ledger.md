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
| 6 | `wakeup` below `store`: the wire sits in the `mux` layer, so the store reaches up to send | pass-4 leftovers: `store` → `wakeup` 2 production sites (`store/writer/publish.rs:179`, `store/gc/collect.rs:12`); the wire holds the two references that keep it there, `mux::ClientPaneView` (`wakeup/events.rs:66`) and `mux::focus_anchor::FocusNonce` (`wakeup/events.rs:73`) | landed pass-5 |
| 7 | `store` → `harness` upward dependencies (signal vocabulary, petname, prompt alignment, run schema, run wake sender): close the direction | pass-6 admission intent `store → harness::schedule::signal` close: later seam; survey at `97a947974`: 17 sites into harness (run 6, signal 5, petname 4, run_wake 1, target 1), cycle harness ↔ store 47/17 cross-layer | landed pass-7 |
| 8 | `store` → `remote::link` and `store` → `disk_usage` remaining non-diag upward dependencies and the four small upward edges: `harness` → `sidebar::refresh::pr`, `osc` → `config`/`mux`, `diag` → `sidebar::presence`, `build_id` → `proc` | survey at `6d13498b3`: store upward sites 6 = diag 4, disk_usage 1, remote::link 1; worktree → disk_usage 1 | landed pass-8 |

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

Superseded in part by the [pass 5 target](#pass-5-target): `wakeup` drops from beside `mux` above `store` to L2 below it. The rest of this direction stands.

### Pass 5 target

The direction the pass writes into `crates/rimz/src/mux/AGENTS.md` and `crates/rimz/src/store/AGENTS.md`: the wire is the leaf `wakeup` module below `store` at L2, and every sender (the store's write tail, `mux`, `remote_control`, the sidebar graph and its renderer, the CLI) reaches down to it, none reaching another module through it. The identifiers that cross the wire move to the modules that own identity: `MuxClientId` and `FocusNonce` to `ids` (`ids.rs:68`, `ids.rs:729`; the nonce stays a transparent `Uuid`, so the `FocusIntent` datagram and the `rimz.focus-anchor.v3` file are byte-identical), `ClientPaneView` to `pane` (`pane.rs:21`).

The rest of the pass sends vocabulary to the module that owns it. Shell selection (`user_shell`, `user_shell_program`, `shell_pane_name`, and the private probe helpers) is a process fact and lives in `proc` (`proc/mod.rs:85-101`); the room identity pin `ENV_CHANNEL`/`ENV_WORKTREE_PATH` and its argv rendering (`channel_shell_argv`, `channel_label_shell_argv`) join `ENV_WORKSPACE_ID`/`ENV_PROJECT_ROOT` in `workspace` (`workspace.rs:56-100`). `harness::launch` keeps agent-scope identity, argv compilation, the login-shell wrapper, and preflight. The persisted record's vocabulary is `agents`: `BudgetWindow`, `BudgetScope`, `BudgetPark` in `agents/state.rs:38-75`, `AgentState::channel()` in place of `harness::target::agent_channel` (`agents/state.rs:855`), and `LifecycleSignal::terminal_disposition` returning `TerminalDisposition` (`agents/lifecycle.rs:195-206`) in place of `harness::run::terminal_status_for_signal`. `harness` keeps the policy over that record: `BudgetSpec`/`BudgetParseError` parsing, ledgers and park constructors, `RunStatus` and the fold that maps a disposition onto it, and the verdicted `agent_handle`.

Pass 5 measures against `f43250479`. Admission rows retain their baseline counts and state changes in their reasons; the pass-log row records the measured deltas from `cargo xtask atlas diff --expect`.

### Pass 7 target

`store` owns every record it persists and reaches nothing in `harness`; `harness` is policy over store records and reaches down. `store::run` owns the run schema, codec, shared cancellation/timeout/budget terminal transition and wake sender; `store::event` owns the persisted signal vocabulary; `store::message` owns submitted-prompt alignment and the shared notice-header spelling. Handle minting and validation live in `agents::petname`. Harness retains run policy and workspace-lock wrappers, the run waiter, signal selectors and firing, and the message-header composer. Stored records, wire frames, grammar and observable behavior stay unchanged; only the impossible run-wake serialization error path is deleted.

### Pass 8 target

`store` owns every record it persists, the link-health tier included, and reaches up only into `diag`; disk mechanics, usage measurement included, sit below it in `disk`.

`LinkTier` lives beside `SidebarLinkHealth`, exposed as `store::snapshot::LinkTier`; remote keeps the classifier and thresholds over it. `disk::usage` owns the unchanged hardlink-aware byte walk and storage-root measurement. `FileIdentity` stays private: per-walk (dev, ino) dedupe, `parse_cache::FileStamp` mtime/length identity, and `temp_sweep` nlink exhaustion are three different rules, not a shared identity abstraction.

The extension puts OSC notification policy at L5 over config and mux capabilities, plugin-command failure vocabulary beside its diagnostic sample, and executable-path resolution solely in `proc`. `forge::pr_state` owns PR-state records and their reader; the sidebar retains forge probes, publication, and account-cache fusion. The generic default-on-unreadable cache read joins cache publication in `disk::atomic`. The two harness account-cache writers stay until their lift beside `agents::account`'s reader; the store/diag cycle closes from the store side, not by lowering diagnostics.

## Module verdicts

One row per module reviewed, at the granularity `survey` ranks. `holds` names the SHA reviewed and the scoped-commit count that reopens it (`git log --oneline <sha>.. -- <path> | wc -l`). `landed` points at the pass row. `candidate` is a survey pick not yet reviewed, with the signal that made it one.

| module | status | sha | reopen at | note |
| --- | --- | --- | --- | --- |
| `store` | landed pass-1; pass-7 | — | — | seam reviewed; module interior remains a candidate. Pass 7: it owns the run record (`store::run`), the signal vocabulary (`store::event`), and the submitted-prompt alignment (`store::message`); it imports no `harness` item |
| `agents` | landed pass-1; pass-5; pass-7 | — | — | seam reviewed; module interior remains a candidate. Pass 5: it owns the persisted record's budget vocabulary, `AgentState::channel()`, and `TerminalDisposition`. Pass 7: it gains `agents::petname`, the handle grammar the store mints and validates against |
| `agents/adapters` | landed pass-2 | — | — | sibling seam reviewed; provider interiors remain separate module candidates |
| `daemon_view` | landed pass-3 | — | — | seam reviewed; interior remains a candidate |
| `pane` | landed pass-3; pass-5 | — | — | seam reviewed; interior remains a candidate. Pass 5: it gains `ClientPaneView` (`pane.rs:21`) |
| `workspace` | landed pass-3; pass-5 | — | — | seam reviewed; interior remains a candidate. Pass 5: it gains the channel and worktree env keys and the channel shell argv (`workspace.rs:56-100`) |
| `mux` | landed pass-4; pass-5 | — | — | seam reviewed; it now owns the focus anchor, the room width target, and the Zellij topology/presence-desired caches; interior remains a candidate. Pass 5: it imports no `harness` item |
| `sidebar` | landed pass-4 | — | — | seam reviewed; it keeps producer election, fusion, refresh lanes, and its own cadences; interior remains a candidate |
| `wakeup` | landed pass-5 | — | — | new module in pass 4: the wire lifted out of `sidebar` (heartbeat record, event vocabulary, datagram send). Pass 5 re-layers it to L2, a leaf below `store` |
| `ids` | landed pass-5 | — | — | seam reviewed: it owns shared identifiers, gaining `MuxClientId` and `FocusNonce`; interior remains a candidate |
| `proc` | landed pass-5 | — | — | seam reviewed: it owns process and environment facts including shell selection; interior remains a candidate |
| `harness` | landed pass-5; pass-7 | — | — | seam reviewed: policy over the agent record (budget parsing, ledgers, `RunStatus`, `agent_handle`), with record vocabulary in `agents`; interior remains a candidate, and `harness/schedule` keeps its own row. Pass 7: policy over store records (run transitions and their workspace lock, the run waiter, signal selectors and firing); the run record and `RunStatus` live in `store::run`, `petname` in `agents` |
| `agents/adapters/codex` | candidate | — | — | survey rank 3; app-server, broker, rollout, and install interiors unreviewed |
| `agents/adapters/claude` | candidate | — | — | survey rank 6; remote-control and install interiors unreviewed |
| `config` | candidate | — | — | esc 182, depth 35.9, churn 8.9%; 38 admitted upward sites |
| `message` | landed pass-1 | — | — | seam reviewed; module interior remains a candidate (baseline esc 157, depth 25.2) |
| `harness/schedule` | landed pass-6 | — | — | baseline esc 168, depth 22.2; pass 6: esc 147, depth 34.4; watcher and stop lifetimes hidden in the harness |
| `agents/spending` | candidate | — | — | esc 118, depth 39.9 |
| `cli/remote` | candidate | — | — | pin, thin, cx 34.0, pace 1.32; pins are the prerequisite |
| `sidebar_pane/render/sections` | candidate | — | — | thin (t/c 0.06) with 3.7k code; pins are the prerequisite |
| `store` | landed pass-8 | — | — | owns `LinkTier` at `store::snapshot`; only diag upward admissions remain; supersedes the pass-7 seam status above |
| `remote` | landed pass-8 | — | — | keeps classifier and thresholds, imports the tier downward; module interior remains a candidate |
| `disk` | landed pass-8 | — | — | gains `disk::usage`, the unchanged byte walk and storage-root measurement |
| `worktree` | landed pass-8 | — | — | reaches `disk::usage` downward; interior remains a candidate |
| `diag` | landed pass-8 | — | — | imports the tier from `store::snapshot`; its own upward admissions remain unreviewed |
| `forge` | landed pass-8 | — | — | owns the PR-state record and reader in `forge::pr_state` |
| `osc` | landed pass-8 | — | — | re-layered to L5 as policy over config and mux capabilities |
| `build_id` | landed pass-8 | — | — | no crate imports; proc alone owns executable-path resolution |
| `harness` | landed pass-8 | — | — | reaches forge for PR state; two account-cache writer sites stay until the agents::account lift |
| `diag` | landed pass-8 extension | — | — | owns `PluginCommandFailure`; store ↔ diag::record cycle retained for the store-side close |

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
| `agents` → `harness::target` | 2 | keep | `agent_handle` is the canonical address; `agent_channel` becomes `AgentState::channel()` in a later pass. Pass 5 folded it in (`agents/state.rs:855`) and deleted `agent_channel`, leaving the one verdicted `agent_handle` site |
| `agents` → `harness::budget` | 1 | closed | later pass: `BudgetPark`/`BudgetScope`/`BudgetWindow` are `AgentState` record vocabulary. Closed pass 5: the trio is defined in `agents/state.rs:38-75` |
| `agents` → `harness::run` | 1 | closed | later pass: a `LifecycleSignal` terminal disposition in `agents::lifecycle`, mapped to `RunStatus` in harness. Closed pass 5: `TerminalDisposition` at `agents/lifecycle.rs:195` with the `RunStatus` map in the harness fold |
| `theme` → `agents` | 5 | keep | catalog and spec vocabulary are the shared language below the store |
| `trust` → `agents` | 1 | keep | catalog and spec vocabulary are the shared language below the store |
| `proc` → `agents` | 4 | keep | catalog and spec vocabulary are the shared language below the store |
| `config` → `agents` | 14 | keep | catalog and spec vocabulary are the shared language below the store |
| `mux` → `sidebar` | 27 | closed | pass 4 |
| `mux` → `remote` | 1 | closed | pass 4: `REMOTE_LINEAGE_ENV` is process vocabulary and moves to `proc` |
| `store` → `sidebar::{heartbeat,timing,wakeup}` | 4 | closed | pass 4 |
| `remote_control` → `sidebar::wakeup` | 1 | closed | pass 4 |
| `config` → `sidebar::timing` | 3 | closed | pass 4: the refresh trio is config vocabulary and is defined in `config` |
| `store` → `wakeup` | 2 | closed | seam 6: the wire moves below the store once `ClientPaneView` reaches `pane` and `FocusNonce` reaches `ids`. Closed pass 5: both types moved, `wakeup` re-layered to L2, and the two sites (`store/gc/collect.rs:12`, `store/writer/publish.rs:180`) keep their code as downward calls |
| `store` → `mux` | 1 | closed | pass 5: the site was the client-view type the wire carried; the snapshot reads `crate::pane::ClientPaneView` now (`store/snapshot/view.rs:100`) |
| `daemon_view` → `sidebar::timing` | 1 | keep | `EVENT_PANE_TTL` is the sidebar's event-mode cadence; daemon maintenance accepts a frame under it and reaches up for the bound deliberately |
| `mux` → `harness::launch` | 5 | closed | later pass: shell-program and pane-name vocabulary belongs below `mux`. Closed pass 5: shell selection to `proc`, the room pin's channel/worktree keys and channel argv to `workspace` |
| `config` → `harness::budget` | 5 | keep | `BudgetSpec` and `BudgetParseError` are parsing, which is harness policy; the window/scope/park lift takes the edge from 5 sites to 4 in pass 5 |
| `store` → `harness::schedule::signal` | 5 | closed | added by `a8e4c4afa` after pass 4 (`store/event.rs:9`, `store/follow.rs:10`, `store/writer/signal.rs:3`); pass 6 defers a split moving `SignalName`/`SignalNameErr`/`SignalSource` below store and making `append_signal` take `SignalEventPayload`. Closed pass 7: the three types are defined in `store::event` beside `SignalEventPayload`, and `append_signal` takes the payload that `harness::schedule::signal` projects through `impl From<&Signal>` |
| `config` → `harness::schedule` | — | keep | pass 6: trigger and surplus grammar are harness policy, matching the pass-5 budget-parsing precedent |
| `store` → `harness::petname` | 4 | closed | pass 7: `store` owns every record it persists and reaches nothing in `harness`; the handle grammar moves whole to `agents::petname`, whose floor is already L2 through `agents::known_kinds()` |
| `store` → `harness::target` | 1 | closed | pass 7: same direction; `align_submitted_prompt` aligns against `store::message` records and lives there with `HarnessNotice::header_type`, and the harness header composer calls the method |
| `store` → `harness::run` | 6 | closed | pass 7: same direction; the run record's schema and codec are `store::run`, and the transitions, their workspace lock and the lock-then-codec wrappers stay harness policy |
| `store` → `harness::run_wake` | 1 | closed | pass 7: same direction; the terminal wake sender, `WakeupFrame` and `run_socket_path` join the record in `store::run`, and the waiter stays in `harness::run_wake`, importing them downward |
| `store` → `diag` | — | keep | now a cycle with `diag::record → store::snapshot::LinkTier`; closes from the store side when the `store/snapshot` module pass stops constructing `DiagEvent`s in `view/live.rs` and `live/projection.rs`; a diag-below-store re-layer is rejected (pass 8) |
| `store` → `diag::record` | — | keep | now a cycle with `diag::record → store::snapshot::LinkTier`; closes from the store side when the `store/snapshot` module pass stops constructing `DiagEvent`s in `view/live.rs` and `live/projection.rs`; a diag-below-store re-layer is rejected (pass 8) |
| `store` → `remote::link` | — | closed | `LinkTier` is a value type; `remote::link` depends on `mux::CommandSpec`, so the type moves down, not the module. Closed pass 8: tier owned by `store::snapshot` |
| `store` → `disk_usage` | — | closed | `dir_size` is a disk fact; `disk_usage.rs` depends only on `disk::paths` and belongs beside `disk`. Closed pass 8: whole module moved to `disk::usage` |
| `worktree` → `disk_usage` | 1 | closed | pass 8: re-layer with the move to `disk::usage` |
| `harness` → `sidebar::refresh::pr` | 4 | closed | pass 8: the PR-state record and reader are `forge::pr_state` |
| `harness` → `sidebar::refresh` | 2 | keep | account-cache writers (`merge_account_rate_limits`, `merge_provider_realtime_usage`) embed refresh-lane fusion; close: lift the writer beside `agents::account`'s reader in a later pass |
| `osc` → `config` | 3 | closed | pass 8: re-layer, OSC is policy over config vocabulary and mux capabilities and sits at L5 |
| `osc` → `mux` | 2 | closed | pass 8: same re-layer to L5, backend facts remain in mux |
| `diag` → `sidebar::presence` | 1 | closed | pass 8: `PluginCommandFailure` is owned by `diag::plugin_presence`, which persists it |
| `build_id` → `proc` | 1 | closed | pass 8: the forwarder had no caller; proc owns the resolver |

## Pass log

One row per pass, newest last. Deltas are the `diff --expect` totals at merge.

| date | pass | scope (`paths`) | base | verbs landed | Δ prod SLOC | Δ esc | Δ dep sites | deferred |
| --- | --- | --- | --- | --- | ---: | ---: | ---: | --- |
| 2026-09-03 | pass-1 seam `store` ⇄ `agents`, `store` ⇄ `message` + `disk` lift | `crates/rimz`; `xtask/src/invariants.rs`; `docs`; `refactor-target.toml`; `AGENTS.md`; `CLAUDE.md`; `ARCHITECTURE.md` | `3e01afc71` | rehome ×7, delete ×7 | −4 | +4 | +20 | Seam 4: `workspace_record`/`compose_channel` homes; seam 5: agents → peer modules; `for_terminal_status` kept. Tightened surfaces: `store` 321, `message` 90. Contract ceiling loosened −20→−1 because the estimate omitted the one-line-per-file import splits a no-shim lift imposes and the method signatures the record's merge rules gained; measured −2 at step 7, −4 after the review round. Atlas advisories: multiple destination definition sites are reported as “not defined under”; cfg-gated items count as production while files behind an equivalent cfg-gated module do not. |
| 2026-09-04 | pass-2 seam adapter sibling families | `crates/rimz/src/agents`; `docs`; `refactor-target.toml` | `93c4c48d7` | rehome ×1, deepen ×1, collapse ×1 | −45 | −2 | +1 internal (crossing +0) | Five shape families and the cached-value test guard hold for the reasons recorded above. No upward edge was reviewed. Deferred module notes: Copilot/Cursor `report_files`, Droid origin stamping, and the filename-scoped `spend.rs` invariant. Tightened `agents` surface to 970. |
| 2026-09-04 | pass-3 seam #4 + #5 + `daemon_view` ↔ `mux` | `crates/rimz`; `docs`; `refactor-target.toml`; `AGENTS.md`; `ARCHITECTURE.md` | `d8361c248` | rehome ×10, deepen ×1, re-layer ×2 | +5 | −6 | +4 internal | `AgentState::channel()`; `BudgetPark`/`BudgetScope`/`BudgetWindow` into `agents`; `LifecycleSignal` terminal-disposition split; `PaneRef` method forms; `CommandSpec` runner collapse; Codex daemon spawn-failure and timeout warning text changes as declared. |
| 2026-09-05 | pass-4 seam #3 remaining `mux` → `sidebar` | `crates/rimz`; `xtask/src/invariants.rs`; `docs`; `refactor-target.toml`; `AGENTS.md`; `CLAUDE.md`; `ARCHITECTURE.md` | `8a4ca7bcd` | rehome (31 contract rows), deepen/collapse focus dispatch, delete duplicate clocks and unused nonce/default/error surface | −44 | +2 | −4 internal | Measured with `cargo xtask atlas diff --expect /tmp/atlas-pass-4-contract.toml`: test SLOC +62, Rust files +2; all six dependency rows at 0. Ordered teardown moved whole to `room::teardown`, not a reordered sweep hoist. Seam 6 defers wire-below-store; mux's five launch-vocabulary sites remain for a later pass. Kept separate heartbeat freshness rules and sidebar-owned `EventStore`; dropped `execute_action`'s unused nonce return; `FocusPresentation` stays public for benchmark-facing fusion. Tightened sidebar 367, mux 277, wakeup 26. |
| 2026-09-05 | pass-5 seam #6 + deferred launch/budget/lifecycle admissions | `crates/rimz/src/wakeup`; `crates/rimz/src/mux`; `crates/rimz/src/pane`; `crates/rimz/src/ids.rs`; `crates/rimz/src/proc`; `crates/rimz/src/workspace`; `crates/rimz/src/agents`; `crates/rimz/src/config`; `crates/rimz/src/store`; `crates/rimz/src/harness`; `crates/rimz/src/message`; `crates/rimz/src/room`; `crates/rimz/src/web`; `crates/rimz/src/sidebar`; `crates/rimz/src/sidebar_pane`; `crates/rimz/src/diag`; `crates/rimz/src/cli`; `crates/rimz/tests`; `docs`; `refactor-target.toml`; `AGENTS.md`; `CLAUDE.md`; `ARCHITECTURE.md` | `f43250479` | rehome ×13, delete ×2, deepen ×1, re-layer ×1 | −13 | +5 | −9 internal (crossing +0) | Measured with `cargo xtask atlas diff --expect /tmp/atlas-pass-5-contract.toml`: test SLOC +286, Rust files +0; all 13 rehomes, two deletes and nine dependency rows landed. Ceiling +16 paid for the terminal-disposition enum and split budget imports; shorter channel calls and import formatting offset that cost. Escaping surface grows +5 with the lifecycle classification and agents budget façade; five upward admissions close, and agents reaches harness only for `agent_handle`. Deferred: the room-pin map/argv duplication (`room/mod.rs:88-99` vs `workspace::channel_shell_argv`) and transparent `FocusNonce`. Tightened harness 603, mux 273, harness/launch 41, harness/run 41. |
| 2026-09-05 | pass-6 module `harness/schedule` | `crates/rimz/src/harness/schedule.rs`; `crates/rimz/src/harness/schedule`; `crates/rimz/src/cli/loop_cmd`; `crates/rimz/src/cli/wake`; `crates/rimz/src/cli/events.rs`; `crates/rimz/src/cli/hooks/lifecycle/observe.rs`; `crates/rimz/tests/integration/{loop_schedule,wake}.rs`; harness contract; loop internals; ledger; `refactor-target.toml` | `bcd1542a8` | delete unused metadata/accessors/planner/path seams; deepen stop/watcher lifetimes and schedule projection; rehome held labels, delayed time and task keys | −136 | −51 (198→147) | −10 internal; +2 crossing (+3 downward, −1 unranked) | Measured with `cargo xtask atlas diff --expect /tmp/pass-6.toml`: test SLOC +18, Rust files +0, watcher assembly 13→1; all eight deletes landed. Esc ceiling 140→149 translates the original ≥49 reduction to the gate counter: inspect excludes module declarations and deduplicates reexports (189→138), while diff counts them (198→147). Source ceiling −90 unchanged. Deferred: store→signal vocabulary seam above; `TaskFire::complete` closures and an observation module rejected as sideways. Store setup remains eager for every held lock through the stop callback; lookup errors propagate after setup. Kept inferred binary-facing result types and the ten-input fire constructor. Tightened harness 559 and runner 35; the global tighten also pruned pre-existing launch admissions and lowered launch to 40. |
| 2026-09-06 | pass-7 seam #7 `store` → `harness` | `crates/rimz`; `docs`; `refactor-target.toml`; `AGENTS.md`; `CLAUDE.md`; `ARCHITECTURE.md`; `xtask/src/invariants.rs`; `xtask/src/invariants/tests.rs` | `97a947974` | rehome ×15 (+ the terminal transition onto `RunRecord`), delete ×1, deepen ×1 | −20 | +2 | +8 internal | Measured with `cargo xtask atlas diff --expect /tmp/pass-7-contract.toml`: test SLOC +178, Rust files +0; all 15 rehomes, the delete and six zero-site dependency rows landed. Store → harness 17→0; source ceiling 0 retained (D1 spike +15, final −20). Escaping surface +2 pays for four shared items minus two deleted wrappers; internal sites +8 include the split imports. Carried `d904fa4bd` first. Read-only invariant needles follow the moved wake sender, with regression coverage. Deferred: remote link vocabulary and disk usage as later seams, and diag re-layering if the survey ranks it. Kept `harness::run::{create, load, list}` wrappers and public `WakeupFrame` for binary callers. Tightened harness 536, run 31, run_wake 9; store 336 and agents 988. |
| 2026-09-06 | pass-8 seam #8 store edges plus small upward-edge extension | `crates/rimz/src/{store,remote,diag,sidebar,sidebar_pane,disk,cli,forge,harness,proc}`; `diag.rs`; `disk_usage.rs`; `worktree.rs`; `lib.rs`; `forge.rs`; `osc.rs`; `build_id.rs`; `crates/rimz/tests/integration/wake.rs`; `docs`; `refactor-target.toml`; `AGENTS.md`; `CLAUDE.md` | `6d13498b3` | rehome ×11, delete ×2 (manual order policy, unused resolver), re-layer ×2 (disk usage, osc → L5) | −17 | +4 summed (+3 unique) | +7 internal | Measured with `cargo xtask atlas diff --expect /tmp/pass-8-contract.toml`: test SLOC +61, Rust files +1; eleven rehomes, one contract delete and seven dependency rows landed. Six zero-site rows close nine sites; harness → sidebar::refresh falls 6→2, retaining account-cache writers. Esc totals overlap forge.rs/forge: the new module declaration counts twice. The extension is production-neutral: record extraction/imports offset deletion; the combined pass retains ceiling 0 and removes 17 SLOC. Store ↔ diag::record is now a cycle; close from the store side in the store/snapshot module pass, not by lowering diag. Deferred: account-cache writers beside agents::account, distinct FileIdentity/FileStamp/temp_sweep rules, remote interior. Four pins green before moves; disk body unchanged (header corrected later). Tightened remote 177, lib 69, build_id 6, sidebar 361; store 337, diag 80, disk 123/atomic 19, forge 48. Global tighten also pruned the already-unused runner → proc allowance. |
