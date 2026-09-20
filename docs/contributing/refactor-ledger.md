# Refactor ledger

The memory between passes of the [refactor program](./refactor-program.md): what has been reviewed, what holds, and which upward edges are intended. A pass reads it first and edits it last. Commits and code are the record of what each pass changed; this file keeps only what the next pass needs. `cargo xtask atlas survey` parses the two tables under `## Module verdicts` and `## Admission intents` (`xtask/src/atlas/ledger.rs`), so their column shapes are a contract; the other sections are prose.

## Seam queue

Ordered survey findings awaiting a seam review; a queued seam is proposed before any module pass. Status is `queued`, `in flight <branch>`, or `rejected <reason>`; a landed seam is deleted from the queue, since the closed direction lives in `refactor-target.toml` layers and admissions and in the module's `AGENTS.md`.

| # | seam | evidence | status |
| --- | --- | --- | --- |

Empty. Ten seams landed in passes 1–16 (store ⇄ agents/message, adapter sibling families, mux ↔ sidebar, daemon_view ↔ mux, `wakeup` below `store`, store → harness, store edges, message → `address`, diag at L3). The cycles that remain are held by intent: `daemon_view ↔ remote_control`, `daemon_view ↔ sidebar`, `agents ↔ proc`, `config ↔ harness`, `harness ↔ message`; `sidebar_pane ↔ web`; `harness ↔ sidebar` closes when the account-cache writers lift beside `agents::account`'s reader.

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
| `agents/adapters/pi` | holds; landed pass-21c | `e237f2db5` | 30 | registry adapter is the sole escaping item; ask mapping, payloads, account probes and spend provider-local; spend parser named `spend::parse` like its siblings. |
| `agents/adapters/opencode` | holds; landed pass-21c | `e237f2db5` | 30 | registry adapter is the sole escaping item; `OpencodeHookPayload` stays `pub(super)` (caveat 13); the tolerant `parse_payload` and the `sqlite_io` error adapter hold by verdict. |
| `agents/adapters/plugin` | holds; landed pass-21c | `e237f2db5` | 30 | `agents::plugins` façade re-exports stay `pub` for the binary crate; `PluginAdapter` private, reached through `loaded()`. |
| `agents/adapters/grok` | holds; landed pass-21c | `e237f2db5` | 30 | registry adapter is the sole escaping item; hook catalog constants and context refresh private to the adapter. |
| `agents/adapters/droid` | holds; landed pass-21c | `e237f2db5` | 30 | registry adapter is the sole escaping item; interiors provider-local. |
| `agents/adapters/kiro` | holds; landed pass-21c | `e237f2db5` | 30 | registry adapter is the sole escaping item; install and session store provider-local. |
| `agents/adapters/amp` | holds; landed pass-21c | `e237f2db5` | 30 | registry adapter is the sole escaping item; session-file forwarder collapsed; `AmpHookPayload` stays `pub(super)` (caveat 13) and the tolerant `parse_payload` holds by verdict. |
| `agents/attribution` | holds | `5fe85089d` | 30 | every atlas narrowing is refuted by the binary's attribution-command tests, which build `MessageCounts`, `SubagentStat`, `Presence`, `TeamRef`, `LaneLifetime` and `LaneLifetimes::new` directly; the report model and `build` hold. |
| `agents/lifecycle` | holds; landed pass-22c | `5fe85089d` | 30 | `step` and the turn-id bookkeeping are crate-private; `LifecycleState` (a `Transition` field and `AgentState::lifecycle` return), `LifecycleSignal::tag`, `TerminalDisposition` and `LIFECYCLE_EVENT_VERSION` stay public by verdict; the signal vocabulary holds. |
| `agents/pricing` | holds; landed pass-22c | `5fe85089d` | 30 | the rate model and book lookups are private to `agents`; `PriceBook` and `cached_book*` stay public for binary, bench and integration callers, and `TokenSplit` (named by `CachedEntry::new`), `from_litellm_json` and the `source` refresh surface by verdict. |
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
| `reload` | holds; landed pass-23c | `bc333aa67` | 30 | one durable staged-build path at crate reach for `room` and renderer supervision; `StageBuildErr` stays `pub` as the public reload's error and `resolve_reexec_target` is private. |
| `remote` | holds; landed pass-21a | `d9d30b10b` | 30 | pure transitions here, drivers in `cli/remote`: rehoming the supervisor machines (`MasterState`, `RetryCause`, `OutageState`, `LinkSupervisor`) is a testability move with a flat escaping surface, not a candidate; `LinkStats`, `SessionLinkUpdate`, `AckOutcome`, `AliasErr` floored by signature; renderer/sidebar-only link helpers `pub(crate)`. |
| `remote_control` | holds; landed pass-15c | `0175c3c6b` | 30 | one enable preflight; typed snapshot, batch toggle and advisories hold. |
| `room` | holds; landed pass-15c | `0175c3c6b` | 30 | constructors, birth, ordered teardown (session kill, resurrection purge, runtime sweep, process sweep) and seven liveness policies hold. |
| `sidebar` | landed pass-4 | — | — | producer election, fusion, refresh lanes, own cadences; interiors have their own rows. |
| `sidebar/(root)` | holds; landed pass-23c | `bc333aa67` | 30 | the flat files are one data plane at sidebar or crate reach: election, unread/read marks, projections, runtime cache reads and the 60 cadences. What stays `pub` is bench-reached (`EventStore`/`append`, `fuse`/`fuse_owned`, `WorkspaceProjectionPublisher`/`publish`), integration-reached (`launch_sidebar_if_needed`, `presence_stamp_path`), or a signature floor (`OpenedUnread`, `AgentProjection`, `SidebarLaunchOutcome`); three cadences stay `pub` only for rustdoc links in held modules. The private `SidebarMux` forwarders are the launch tests' fake backend. |
| `sidebar/refresh` | holds; landed pass-23c | `bc333aa67` | 30 | pass 15b's shape holds after 38 reopening commits: one provider refresh entry, one rate-limit cache transaction, PR name/payload pairs. `project_rate_limits` holds as four delegating phases. The two `read_*_cache` wrappers pin `T` for the generic cache read and are not forwarders; `DiffStatsCache` and `ProducerRefreshState` stay `pub` for the performance tier. |
| `sidebar/consumer` | holds; landed pass-23c | `bc333aa67` | 30 | `read_published_snapshot` is a producer path at sidebar reach; `rollup_snapshot` and `read_adopting` stay `pub` for the hotpath bench. |
| `sidebar/frame` | holds; landed pass-23c | `bc333aa67` | 30 | the published `PaneFrame` wire stays `pub` field by field down to `PaneMetrics` (bench, integration crate and `daemon_view` read it) with `assemble_frame` as its entry; the assembly and rotation helpers narrowed to the sidebar. |
| `sidebar/produce` | holds; landed pass-17a | `39b72afea` | 30 | named entries kept (no fold-mode core); metrics and git one file each. |
| `sidebar/enrich` | holds; landed pass-20b | `e3785a2be` | 30 | ordered fold spine holds (fold order is an invariant); bench-reached `FoldOpts`/`enrich`/`enrich_workspace`/`WorkspaceSnapshot` keep. |
| `sidebar/observe` | holds; landed pass-20b | `e3785a2be` | 30 | exposes the five names the renderer spells; `Observer::observe_into` owns detection, send and drop accounting. |
| `sidebar/presence` | holds; landed pass-20b | `e3785a2be` | 30 | `ingest_zellij_wake` is one accept/reject transaction; CLI-reached wake and telemetry types keep `pub`. |
| `sidebar/notify` | holds; landed pass-20b | `e3785a2be` | 30 | debounce/coalesce, link-health episodes and handler delivery are distinct policies. |
| `sidebar_pane/app` | holds; landed pass-11 | `f0d27f230` | 30 | loop transitions, dispatch, folds, focus repair and input reviewed; width control and the three elder-gated workers hold. |
| `sidebar_pane/pets` | holds; landed pass-22a | `b8e4115e3` | 30 | serve-loop types narrowed to the pane; `PetView`/`PetBody`/`PetPixelView` floored by the held `UiState::pet` field; preview types stay `pub` for the CLI; load machine, track selection and voice hold. |
| `sidebar_pane/pixel` | holds; landed pass-22a | `b8e4115e3` | 30 | transport narrowed to the pane; `PLACEHOLDER`/`ROW_COLUMN_DIACRITICS` stay `pub(crate)` for ttyd and testkit; `PixelRenderCaps` and the CLI-reached encoders stay `pub`; `MeterPixels` floored by `UiState`; the resend gate (`30108e572`) and `ZellijKittySupport`'s five variants (`3718d6f34`) hold. |
| `sidebar_pane/supervise` | holds; landed pass-22a | `b8e4115e3` | 30 | reload and respawn codes pane-private; `run`, its error type, `run_worker`, `is_worker`, `instance_id` and `SELF_CLOSE_EXIT_CODE` stay `pub` for the CLI. |
| `sidebar_pane/render/chrome` | holds; landed pass-22a | `b8e4115e3` | 30 | home abbreviation private with its test; the nine bottom-chrome builders are `compose`'s vocabulary. |
| `sidebar_pane/render/labels` | holds; landed pass-22a | `b8e4115e3` | 30 | glyph and meter vocabulary already at render reach; four style helpers stay inside labels. |
| `sidebar_pane/render/theme` | holds; landed pass-22a | `b8e4115e3` | 30 | the Layer-3/4 carrier `docs/internals/theme.md` names and the color invariant exempts; file-local tones private. |
| `sidebar_pane/render/animation` | holds; landed pass-23c | `bc333aa67` | 30 | the cadence enum and its resolver are the pane's, the breath/blink math the renderer's; `Animation` and `ShimmerWave` floored by `ResolvedAnimations` and `UnreadAnim`, and three helpers stay at render reach for label and theme tests. |
| `sidebar_pane/render/(root)` | holds; landed pass-18b | `134e4e8db` | 30 | reviewed as the render bundle (root, compose, layout, sections). |
| `sidebar_pane/render/compose` | holds; landed pass-18b | `134e4e8db` | 30 | reviewed as the render bundle. |
| `sidebar_pane/render/sections` | holds; landed pass-18b | `134e4e8db` | 30 | the full-frame snapshot suite pins root → compose → chrome/sections; width budgets are distinct rules; `text_width`/`clip` are the layout vocabulary. |
| `store` | landed pass-1; pass-7; pass-8; pass-15a; pass-16; pass-17b; pass-21b | — | — | owns every record it persists, imports nothing above it; every interior has its own row below. |
| `store/snapshot` | holds; landed pass-23a | `7c2fb3b70` | 30 | reopened and re-reviewed: the fold, rebuild and carryover adapters are store-internal, the pane classifier's re-export is test-only; the view model stays crate-wide because the renderer decodes it, the pipeline and resume types by verdict. Pass 15a's projection, grouping and reducer findings still hold. |
| `store/writer` | holds; landed pass-17b | `793e3fd0a` | 30 | one log boundary with four cache policies; one publish tail; launch vocabulary, queue method pairs, reap and outcome types hold. |
| `store/event` | holds; landed pass-21b | `19c872874` | 30 | launch/attach payloads and `MessageEventMethod` stay `pub` (binary crate, integration crate, or `EventKind` signature reach); `message_event` keeps dynamic method/reason for the queue writer; legacy `message.removed` parse holds; `params_value` is test-only. |
| `store/message` | holds; landed pass-21b | `19c872874` | 30 | builders, `gate_open`, `new_for_card`, `MAX_DELIVERY_ATTEMPTS` and `sent_reconcile_deadline` stay `pub` for the integration crate; `pending`/`removed` status aliases hold for mixed-binary workspaces; header grammar and codec hold. |
| `store/gc` | holds; landed pass-21b | `19c872874` | 30 | probe-marker lifetimes stay `pub` in `store::gc` (sidebar reader, integration crate); live-room exporter check pinned by `b58b6594c`; the three `rimz gc` wrappers hold. |
| `store/event_log` | holds; landed pass-23a | `7c2fb3b70` | 30 | rotation, pruning, archive listing, repair, batch append and replace are the store's own write path; the two archive facades collapsed onto their implementations; `LogExtent`, `EventLogErr`, `append` and the always-on byte counters stay `pub` by verdict, and the incremental read stays `pub(crate)` for the message reply poll. |
| `store/sidecar` | holds; landed pass-23a | `7c2fb3b70` | 30 | the latest-wins sidecar mechanics serve store records only, so the record trait, parse cache, path helper and read/write entry points are store-internal; the file-name digest stays `pub(crate)` for the harness. |
| `store/session_death` | holds; landed pass-23a | `7c2fb3b70` | 30 | supersession, the pidless-ghost TTL, owner pid and session age are store-internal; `same_agent_instance` stays `pub(crate)` for the address resolver. |
| `store/runtime` | holds; landed pass-23a | `7c2fb3b70` | 30 | `owner_is_live` and `RuntimeProjection::from_parts` store-internal, the audit projection `pub(crate)`; `AgentLiveness` and `RuntimeProjection` stay `pub` by verdict, and the two owner constructors serve distinct launch and pane paths. |
| `store/live_roster` | holds; landed pass-23a | `7c2fb3b70` | 30 | `read` and the roster record are `pub(crate)` for the harness rebirth path and the writer reap; `publish` stays `pub` for two integration suites. |
| `store/follow` | holds | `7c2fb3b70` | 30 | nothing landed: the batch, its error and the signal payload are all floored by the `pub` follower signatures the binary's event stream calls. |
| `store/agent_context` | holds | `7c2fb3b70` | 30 | nothing to narrow: all twelve escaping items are read by the binary, the sidebar or the message layer. The rest-certificate guard is store-owned and its two binary copies are an open deferral. |
| `store/subagent_context` | holds | `7c2fb3b70` | 30 | nothing landed: the record and `read_all` are reached by the integration crate's env helper and the sidebar enrichment. |
| `store/run` | holds | `7c2fb3b70` | 30 | nothing landed: `WakeupFrame` is the pinned run-wake wire, and `mark_terminal`'s four callers pass four distinct statuses. |
| `store/active_time` | holds | `7c2fb3b70` | 30 | nothing landed: the record is floored by the `pub` `read_for_keys` and `SidebarSnapshot::with_active_time`. |
| `theme` | holds; landed pass-22b | `6e04c3c5b` | 30 | every `pub use` re-export has an outside reader; `BrandColor` floored by `resolve_provider_brand`/`ResolvedProviderIdentity` for the binary crate; OKLab blends private to theme; renderer-only helpers `pub(crate)`. |
| `trust` | holds; landed pass-22b | `6e04c3c5b` | 30 | `TrustErr`, `trust::Result`, `SurfaceSummary` and `ProjectConfig` with its field types stay `pub` by signature or integration reach; `executable_surface_hash` is the integration crate's hash probe; `_with_roots` seams private except `grant_with_roots`, which other modules' unit tests call. |
| `wakeup` | landed pass-5 | — | — | the sidebar wire at L2 below `store`. |
| `web` | holds | `bc333aa67` | 30 | nothing landed: every payload and outcome type is returned by a public entry point, and the six delegating entries are the domain facade over the ttyd and gate interiors. The same-layer cycle with `sidebar_pane` is four sites and holds by intent. |
| `workspace` | holds; landed pass-22b | `6e04c3c5b` | 30 | owns the room identity pin env keys, channel shell argv and the `workspace.json` record; `WorkspaceErr`, `record::WorkspaceRecordErr` and their `Result` aliases floored by the pub resolver and record readers; `record::write`, `pin_env` and `known_workspaces_under` stay `pub` for the integration crate; `PinScan` private. |
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
- `agents/attribution`: a `testkit`-gated fixture builder would let the binary's attribution-command tests stop naming `MessageCounts`, `SubagentStat`, `Presence`, `TeamRef` and `LaneLifetime`, so those could narrow; a deepen that waits for the module's `fix(attribution)` churn to settle.
- `agents/adapters/codex`: transcript lookup ignores `CODEX_HOME` (`codex/transcript.rs`), substring daemon classification (`codex/process.rs`), per-attempt refresh budget (`codex/app_server.rs`); reported, not fixed.
- `sidebar_pane/render/sections/provider.rs`: the tab rail measures `chars().count()`, mis-sizing a non-ASCII product name; reported, not fixed.
- `sidebar_pane/render/ui_state.rs`: `UiState::pet` and `UiState::meter_pixels` are `pub(crate)`, flooring `pets::{PetView, PetBody, PetPixelView}` and `pixel::meter::MeterPixels` at crate reach; a pass on the held render root that narrows those fields to `pub(in crate::sidebar_pane)` lets the four types follow.
- `theme::fmt::reset_secs` can go private once its boundary assertions in the sidebar renderer's `fmt` tests move into `theme/fmt.rs`'s test module; that file belongs to the sidebar_pane render pass.
- `store::agent_context::attach_rest_certificates` is copied whole into `cli/agents_cmd/exec.rs` and `cli/supervised/pane.rs` (guard and sidecar read, about fourteen lines each); the store function is `pub(crate)`, so collapsing the copies means widening it to `pub` and editing both binary call sites. Waits for a pass whose paths include them.
- `store/event_log/rotation.rs`: `prune_archive` reports an I/O failure as `EventLogErr::Atomic` while `rotate` and `newest_archives` report the same failure class as `EventLogErr::Io`. Pass 23a's façade collapse preserved the divergence deliberately, because converging it changes a user-visible error message; it waits for a pass that changes that message on purpose.
- `sidebar::timing::{EVENT_PANE_TTL, SNAPSHOT_CACHE_TTL, SESSION_REFRESH_INTERVAL}`: crate or sidebar reach by their readers, but rustdoc links in `mux` and `store::gc` public docs resolve against them and `cargo xtask doc` is a gate; unblocked by a pass on either module rewriting those links as plain backticks.
- `sidebar_pane ↔ web`: four sites — the pane's pixel probe reads web's daemon records, and web's browser bootstrap reads the pane's diacritic table. Closing either direction needs a neutral pixel-wire or daemon-probe module below both, which is a seam pass, not an interior one.
- Compiler-refused narrowings (E0446 / `private_interfaces`, atlas caveat 13) stay at their current visibility everywhere; do not re-plan them from `inspect`'s `narrow to` column without checking the signature that floors them.
