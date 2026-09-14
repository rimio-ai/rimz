# Refactor ledger

The memory between passes of the [refactor program](./refactor-program.md): what has been reviewed, what holds, and which upward edges are intended. A pass reads it first and edits it last. Commits and code are the record of what each pass changed; this file keeps only what the next pass needs. `cargo xtask atlas survey` parses the two tables under `## Module verdicts` and `## Admission intents` (`xtask/src/atlas/ledger.rs`), so their column shapes are a contract; the other sections are prose.

## Seam queue

Ordered survey findings awaiting a seam review; a queued seam is proposed before any module pass. Status is `queued`, `in flight <branch>`, or `rejected <reason>`; a landed seam is deleted from the queue, since the closed direction lives in `refactor-target.toml` layers and admissions and in the module's `AGENTS.md`.

| # | seam | evidence | status |
| --- | --- | --- | --- |

Empty. Ten seams landed in passes 1–16 (store ⇄ agents/message, adapter sibling families, mux ↔ sidebar, daemon_view ↔ mux, `wakeup` below `store`, store → harness, store edges, message → `address`, diag at L3). The cycles that remain are held by intent: `daemon_view ↔ remote_control`, `daemon_view ↔ sidebar`, `agents ↔ proc`, `config ↔ harness`, `harness ↔ message`; `harness ↔ sidebar` closes when the account-cache writers lift beside `agents::account`'s reader.

## Module verdicts

One row per module at the granularity `survey` ranks (`store/snapshot`, `agents/(root)`, `mux/tmux`). `holds` names the SHA reviewed and the scoped-commit count that reopens it (`git rev-list --count <sha>..HEAD -- <path>`); `survey` reads those two cells and flags the module `held` until the count is reached, then `reopen`. A module pass that landed writes `holds; landed pass-N` with the record commit's parent as the SHA, since only a `holds` status demotes the module; a bare `landed` marks a seam pass that reviewed the module's edges and left its interior a candidate. A module reviewed as part of a parent's pass gets its own row at the parent's SHA. The note names what holds by verdict so the next reader does not re-litigate it.

| module | status | sha | reopen at | note |
| --- | --- | --- | --- | --- |
| `address` | holds; landed pass-14 | `0a9330ef1` | 30 | grammar, renderer, pane binding and launch lineage at L4; one channel reconciler; message-only items narrowed. |
| `agents` | holds; landed pass-18a | `a3d0b8851` | 30 | root exports only outside-spelled names; hook representation private to `hook_types`; test helpers gated; the six never-overridden install defaults stay trait defaults; schema types hold. Adapters, spending and petname keep their own rows. |
| `agents/(root)` | holds; landed pass-18a | `a3d0b8851` | 30 | reviewed with the `agents` row. |
| `agents/state` | holds; landed pass-18a | `a3d0b8851` | 30 | reviewed with the `agents` row; owns `BudgetWindow`/`BudgetScope`/`BudgetPark` and `AgentState::channel()`. |
| `agents/context` | holds; landed pass-18a | `a3d0b8851` | 30 | reviewed with the `agents` row. |
| `agents/definition` | holds; landed pass-18a | `a3d0b8851` | 30 | reviewed with the `agents` row. |
| `agents/adapters` | landed pass-2 | — | — | sibling families hold: hook decode, spend cache, lifecycle classification, answer plan, paired settings install are provider policy over shared helpers; interiors are per-adapter rows. |
| `agents/adapters/claude` | holds; landed pass-9 | `7af6d9f74` | 30 | neutral control outcomes, shared priced-record gate; remote-control trio, typed Stop validation and discovery stay separate. |
| `agents/adapters/codex` | holds; landed pass-9 | `7af6d9f74` | 30 | one framed transport and handshake for enrichment and broker; rollout presence semantics and pane-confirmation seam hold. |
| `agents/adapters/kimi` | holds; landed pass-19c | `11e3d305a` | 30 | registry adapter is the sole escaping item; interiors provider-local. |
| `agents/adapters/qwen` | holds; landed pass-19c | `11e3d305a` | 30 | registry adapter is the sole escaping item; regional quota, statusline, rewind spend and managed install provider-local. |
| `agents/adapters/copilot` | holds; landed pass-20c | `e26f986a1` | 30 | registry adapter is the sole escaping item; multi-file report rows through `adapters/install_report.rs`; `CopilotHookPayload`/`NormalizedToolCall(s)` stay `pub(super)` (caveat 13). |
| `agents/adapters/cursor` | holds; landed pass-20c | `e26f986a1` | 30 | registry adapter is the sole escaping item; statusline serde forwarders collapsed onto the generic lossy helper; `RETAINED_RENDERING_KEYS` behaviour pinned by `0c5284908`. |
| `agents/adapters/antigravity` | holds; landed pass-20c | `e26f986a1` | 30 | registry adapter is the sole escaping item; reports through the shared builder. |
| `agents/spending` | holds; landed pass-13 | `3c7dcbb18` | 30 | one writer per cache, version predicates, shared readers, one engine election path; parser wire, walker, fold, discovery and cache records hold. |
| `config` | holds; landed pass-12 | `9ea1ad057` | 30 | effective loader takes the machine snapshot; registry constructors, defaults and validators private; the fragment fold, editor, section records and presentation wire hold. |
| `daemon_view` | holds; landed pass-15c | `0175c3c6b` | 30 | loop-panel acquisition is one operation; spec, identity, repair machine and elder tracker hold; both cycles hold by intent. |
| `diag` | holds; landed pass-16 | `6fccc2b04` | 30 | evidence vocabulary and append mechanics at L3 below store; one sink admission point; shared rotated read; wire and per-surface logs hold. |
| `disk` | holds; landed pass-19b | `ecb57065f` | 30 | file mechanics over `ids` and `sock`; durability classes, filenames, locking, and the three distinct file identities (walk dedupe, parse stamp, temp-sweep nlink) hold. |
| `harness` | landed pass-5; pass-7; pass-8; pass-14; pass-20a | — | — | policy over agent and store records, reaching down; interiors have their own rows below. Two account-cache writer sites stay until the `agents::account` lift. |
| `harness/schedule` | holds; landed pass-14 | `bee9e413d` | 30 | one arming rule, compiled armed receipt, exit codes from `RunStatus`; `RunLockInfo`, `SignalSelector`, `LoopRunRecord::new` public by verdict. |
| `harness/resume` | holds; landed pass-19a | `4aed3e793` | 30 | posture composes `plan::ResumeLaunchPosture`; recovery interior `pub(super)`; `plan_resume`/`ResumeContext` stay `pub` for the integration crate. |
| `harness/plan` | holds; landed pass-19a | `4aed3e793` | 30 | owns the resume-argv DTOs; three phase-wide validation loops held by `92b6fbeab`; `launch_identity_requests` keeps nine positional args. |
| `harness/launch` | holds; landed pass-20a | `5350d68e3` | 30 | one private `LAUNCH_FIELDS` key table encodes and decodes the pane identity env; relaunch requests over `ExecRequest::bare_launch`; `ENV_*` keys the integration crate sets stay `pub`. |
| `harness/spec` | holds; landed pass-20a | `5350d68e3` | 30 | config-reached validators `pub(crate)`; `LayoutErr`, `parse_layout_spec` and the `Column`/`RawLayout`/`RawColumn` family stay `pub`. |
| `harness/budget` | holds; landed pass-20a | `5350d68e3` | 30 | `evaluate`/`BudgetVerdict` private; ledger and scope-state types stay `pub` as signature types. |
| `harness/run` | holds; landed pass-20a | `5350d68e3` | 30 | `RunCancellation::new` beside `Default` holds (integration callers); `RunWakeErr` and `socket_path` stay `pub`. |
| `ids` | landed pass-5; pass-16 | — | — | shared identifiers (`MuxClientId`, `FocusNonce`, `LinkTier`); interior a candidate. |
| `message` | holds; landed pass-13 | `efe38f50c` | 30 | one System queue shape with auto-continue through `nudge_now`; settle owned by `message deliver`; dispatch wire, ordered check family and reply machine hold. |
| `mux` | holds; landed pass-18c | `c037d47f8` | 30 | planner verdicts private; `TmuxBackend` by `Default`; `SplitPaneOptions::from_command` owns the pane-command projection; `MuxErr` and `PaneListOptions` fully live; records the integration crate, bench or CLI reach hold. |
| `mux/(root)` | holds; landed pass-18c | `c037d47f8` | 30 | reviewed with the `mux` row. |
| `mux/tmux` | holds; landed pass-18c | `c037d47f8` | 30 | reviewed with the `mux` row. |
| `mux/zellij` | holds; landed pass-10 | `173682d90` | 30 | presence lifecycle deepened; topology schema is the public wire; socket, pane_pid, parse and reap interiors hold. |
| `pane` | landed pass-3; pass-5 | — | — | owns `ClientPaneView`; interior a candidate. |
| `proc` | holds; landed pass-19b | `ecb57065f` | 30 | process/platform facts and shell selection; platform seams, bounded execution, spawn accounting and pane-probe abstention hold. |
| `remote` | holds; landed pass-21a | `d9d30b10b` | 30 | pure transitions here, drivers in `cli/remote`: rehoming the supervisor machines (`MasterState`, `RetryCause`, `OutageState`, `LinkSupervisor`) is a testability move with a flat escaping surface, not a candidate; `LinkStats`, `SessionLinkUpdate`, `AckOutcome`, `AliasErr` floored by signature; renderer/sidebar-only link helpers `pub(crate)`. |
| `remote_control` | holds; landed pass-15c | `0175c3c6b` | 30 | one enable preflight; typed snapshot, batch toggle and advisories hold. |
| `room` | holds; landed pass-15c | `0175c3c6b` | 30 | constructors, birth, ordered teardown (session kill, resurrection purge, runtime sweep, process sweep) and seven liveness policies hold. |
| `sidebar` | landed pass-4 | — | — | producer election, fusion, refresh lanes, own cadences; interiors have their own rows. |
| `sidebar/refresh` | holds; landed pass-15b | `fbbe60d34` | 30 | one provider refresh entry and rate-limit transaction; PR computation emits name/payload pairs; lane internals narrowed. |
| `sidebar/produce` | holds; landed pass-17a | `39b72afea` | 30 | named entries kept (no fold-mode core); metrics and git one file each. |
| `sidebar/enrich` | holds; landed pass-20b | `e3785a2be` | 30 | ordered fold spine holds (fold order is an invariant); bench-reached `FoldOpts`/`enrich`/`enrich_workspace`/`WorkspaceSnapshot` keep. |
| `sidebar/observe` | holds; landed pass-20b | `e3785a2be` | 30 | exposes the five names the renderer spells; `Observer::observe_into` owns detection, send and drop accounting. |
| `sidebar/presence` | holds; landed pass-20b | `e3785a2be` | 30 | `ingest_zellij_wake` is one accept/reject transaction; CLI-reached wake and telemetry types keep `pub`. |
| `sidebar/notify` | holds; landed pass-20b | `e3785a2be` | 30 | debounce/coalesce, link-health episodes and handler delivery are distinct policies. |
| `sidebar_pane/app` | holds; landed pass-11 | `f0d27f230` | 30 | loop transitions, dispatch, folds, focus repair and input reviewed; width control and the three elder-gated workers hold. |
| `sidebar_pane/render/(root)` | holds; landed pass-18b | `134e4e8db` | 30 | reviewed as the render bundle (root, compose, layout, sections). |
| `sidebar_pane/render/compose` | holds; landed pass-18b | `134e4e8db` | 30 | reviewed as the render bundle. |
| `sidebar_pane/render/sections` | holds; landed pass-18b | `134e4e8db` | 30 | the full-frame snapshot suite pins root → compose → chrome/sections; width budgets are distinct rules; `text_width`/`clip` are the layout vocabulary. |
| `store` | landed pass-1; pass-7; pass-8; pass-15a; pass-16; pass-17b; pass-21b | — | — | owns every record it persists, imports nothing above it; `snapshot`, `writer`, `event`, `message` and `gc` have their own rows; `event_log`, `agent_context`, `runtime` and `sidecar` interiors are candidates. |
| `store/snapshot` | holds; landed pass-15a | `922b15292` | 30 | snapshot-owned live projection and grouping; one private reducer owner; wire, fold, binding and assemblers hold. |
| `store/writer` | holds; landed pass-17b | `793e3fd0a` | 30 | one log boundary with four cache policies; one publish tail; launch vocabulary, queue method pairs, reap and outcome types hold. |
| `store/event` | holds; landed pass-21b | `19c872874` | 30 | launch/attach payloads and `MessageEventMethod` stay `pub` (binary crate, integration crate, or `EventKind` signature reach); `message_event` keeps dynamic method/reason for the queue writer; legacy `message.removed` parse holds; `params_value` is test-only. |
| `store/message` | holds; landed pass-21b | `19c872874` | 30 | builders, `gate_open`, `new_for_card`, `MAX_DELIVERY_ATTEMPTS` and `sent_reconcile_deadline` stay `pub` for the integration crate; `pending`/`removed` status aliases hold for mixed-binary workspaces; header grammar and codec hold. |
| `store/gc` | holds; landed pass-21b | `19c872874` | 30 | probe-marker lifetimes stay `pub` in `store::gc` (sidebar reader, integration crate); live-room exporter check pinned by `b58b6594c`; the three `rimz gc` wrappers hold. |
| `wakeup` | landed pass-5 | — | — | the sidebar wire at L2 below `store`. |
| `workspace` | landed pass-3; pass-5 | — | — | owns the room identity pin env keys and channel shell argv; interior a candidate. |
| `worktree` | landed pass-8 | — | — | interior a candidate. |
| `forge` | landed pass-8 | — | — | owns the PR-state record in `forge::pr_state`. |
| `osc` | landed pass-8 | — | — | L5 policy over config and mux capabilities. |
| `build_id` | landed pass-8 | — | — | no crate imports. |

## Admission intents

One row per upward dependency edge that is the intended shape, spelled `` `from` → `to` `` with an optional `to::{a,b}` group and the reason. `survey` lists every admitted edge with no row as `unreviewed`; that column is the review backlog. An edge a pass closes loses its admission in `refactor-target.toml` and its row here; a `close` intent names the pass that will close it.

| from → to | sites | intent | reason / seam |
| --- | ---: | --- | --- |
| `store` → `agents` | 134 | keep | the store folds the agent model |
| `config` → `agents` | 17 | keep | catalog and spec vocabulary are the shared language below the store |
| `theme` → `agents` | 5 | keep | same |
| `trust` → `agents` | 1 | keep | same |
| `proc` → `agents` | 4 | keep | same; pane-probe classification needs the catalog, moving provider policy into proc duplicates it |
| `config` → `store::message` | 3 | keep | `AutoCompact` is config vocabulary and persisted record data; the persisted home wins |
| `config` → `harness::spec` | 9 | keep | the loader holds the ordered fragment set; harness owns layout validation and resolution |
| `config` → `harness::budget` | 4 | keep | `BudgetSpec`/`BudgetParseError` parsing is harness policy |
| `config` → `harness::schedule` | 4 | keep | trigger and surplus grammar are harness policy |
| `agents` → `address` | 1 | keep | `agent_handle` is the canonical routable address; its renderer needs target resolution and launch occupancy |
| `daemon_view` → `sidebar::timing` | 1 | keep | `EVENT_PANE_TTL` is the sidebar's event-mode cadence, reached up deliberately |
| `daemon_view` → `daemon_content` | 1 | keep | `resolve_content` keeps layout slot cardinality and the supervisor's pane policy one decision |
| `daemon_view` → `sidebar::frame` | 1 | keep | the elder reads the published `PaneFrame` as disposable topology truth and falls back to repair |
| `daemon_view` → `sidebar::cache` | 2 | keep | freshness verdict and zero-child cache read; a stale frame never suppresses repair |
| `room` → `sidebar` | 3 | keep | birth purges rebirth heartbeats after proving the session absent; teardown orders the orphan sweep between session death and the process sweep |
| `room` → `sidebar::body_filter` | 1 | keep | pristine birth resets presentation state (`0e3b80c55`) |
| `room` → `reload` | 1 | keep | ownership stages through reload's one durable-build path (`f7e1b4153`, `28c3552ba`) |
| `harness` → `sidebar::refresh` | 2 | close | account-cache writers (`merge_account_rate_limits`, `merge_provider_realtime_usage`) embed refresh-lane fusion; a later pass lifts the writer beside `agents::account`'s reader |
| `message` → `harness::ancestry` | 2 | keep | sender exclusion from `@all` needs durable launch identity after resolution |
| `message` → `sidebar::produce` | 4 | keep | dispatch chooses a rollup-only or fresh frame+rollup fold after inspecting pending records |
| `message` → `harness::auto_continue` | 1 | keep | `ResumeUnrecovered` re-verifies a `Resume` park at delivery time |
| `message` → `harness::run` | 1 | keep | `report::digest_fully_joined` is the subagent-digest join guard |
| `message` → `harness::assist_log` | 3 | keep | the auto-compact assist is observable only where the synthesized command reaches `Sent` |
| `message` → `harness::schedule::pending` | 1 | keep | `TurnWaitView::load` attaches the scheduler-owned turn-completion waits to the reply view (`4f3adcd90`) |

## Open deferrals

Candidates a pass judged real but could not land, each with the condition that unblocks it. A future pass on the module weighs them before its own survey; everything else a pass deferred is re-derived by `atlas inspect` on the current code.

- `harness` → `agents::account`: lift the two account-cache writers beside the reader (closes `harness ↔ sidebar`).
- `agents::transcript_fs`: one lossy-object serde helper for Copilot `spend.rs` and Cursor's `deserialize_optional_object_lossy`; waits for a pass on the held `agents` root.
- `harness/launch`: the four relaunch sites repeat the three posture prompt fields; a posture-aware seam that `launch` may not import.
- `harness/auto_continue`: `ResumeConfig::auto_continue_backoff(retries)` absorbing the ramp interpretation (`ce0c00897` pins the empty-ramp 300 s fallback).
- `harness/schedule/config_edit.rs`: the `parse_text` seam takes a filename plus an unused `agents_home`.
- `message`: `ReplyWait::run` three methods → one plus `ReplyEvent` (timing pinned by `27077a848`/`cfe1240a3`); `compact_idle` absorbing idle preflight needs `send_compact` to return the id.
- `store/message` ↔ `address`: `address::message_header` respells the `Type:`/`From:`/`Content:` literals `store::message` parses; a store-owned `compose_header` measured line-neutral (pass 21b), so it waits for a header grammar change that edits both sides.
- `room/mod.rs:88-99` repeats `workspace::channel_shell_argv`'s room-pin map.
- `proc::in_pane_agent_start` is uncalled; its eager `then_some(starts[0])` panics on an empty match. Deletion trips `dead_code`; reported, not fixed.
- `agents/adapters/codex`: transcript lookup ignores `CODEX_HOME` (`codex/transcript.rs`), substring daemon classification (`codex/process.rs`), per-attempt refresh budget (`codex/app_server.rs`); reported, not fixed.
- `sidebar_pane/render/sections/provider.rs`: the tab rail measures `chars().count()`, mis-sizing a non-ASCII product name; reported, not fixed.
- Compiler-refused narrowings (E0446 / `private_interfaces`, atlas caveat 13) stay at their current visibility everywhere; do not re-plan them from `inspect`'s `narrow to` column without checking the signature that floors them.
