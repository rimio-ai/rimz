# Refactor ledger

The memory between passes of the [refactor program](./refactor-program.md): where the program stands, what has been reviewed, and which upward edges are intended. A pass reads it first and edits it last. `cargo xtask atlas survey` parses the tables under `## Module verdicts` and `## Admission intents` (`xtask/src/atlas/ledger.rs`), so their column shapes are a contract; the other sections are prose. Keep every note to one clause: the code is the result, and commits and PRs are the history.

## Status

Pass 28d reviewed `store/snapshot` complexity on `c34ca45c2` (base `1eaa698e9`): lifecycle effects fold onto the carried row; the local-bind cascade and the lifecycle fold's residual cx are inherent pinned rules. Pass 28g, before it, recorded the lifecycle reaction in `harness/team_stage` on `6b02f6006`, rebased onto trunk `36dd87064`: `cli/hooks` cx 32.2 versus 58.0 at the round base (`27c50ce2d`); `harness/team_stage` cx 12.5 versus 0.5, missing the planned ceiling of 5, with `react_to_lifecycle` (cx 12.0) under an item verdict for its pinned per-event order. Binary modules receive no ledger row. Rewrite this section whenever a pass ends.

- **Seam queue: empty.** Eleven seams landed in passes 1 to 25. A seam a survey surfaces is added here as `queued` and proposed before any module pass; a landed seam leaves the list, its direction living in `refactor-target.toml` and the module's `AGENTS.md`.
- **Cycles held by intent:** `daemon_view ↔ remote_control`, `daemon_view ↔ sidebar`, `agents ↔ proc`, `pane ↔ proc`, `config ↔ harness`, `config ↔ trust` (trust hashes the command-executing fields; effective config reads trust), `harness ↔ message`, `sidebar_pane ↔ web` (see deferrals). Also listed by the survey and each backed by `keep` admissions: `agents ↔ config`, `agents ↔ theme`, `address ↔ agents`, `config ↔ store`, `config ↔ theme`, and the crate-root re-export cycles.
- **Reopened** (churn past the row's count): none; pass 27a closed `agents/adapters/claude`, pass 27b `store/writer`, pass 26b `sidebar_pane/app` and `sidebar_pane/render/sections`.
- **Never reviewed:** none; pass 26a reviewed `lsp` and `config/lsp`, pass 26c the leaves.
- **Unreviewed admissions:** none; pass 26c closed `config` → `harness::idle_compact`.
- **Row SHAs:** the survey reports a row whose SHA `HEAD` does not reach as a ledger problem and leaves its module unheld; pass 28a re-stamped 29 rebased-away SHAs, passes 14 to 27, to their trunk commits. A PR that writes rows names them so the merge re-stamps them to the landed commit; pass 28g's `harness/team_stage` row and pass 28d's `store/snapshot` row carry branch SHAs.
- **Unjudged families:** none; pass 28a judged the copilot/cursor install shape and the `RunStatus::Completed`, `wait_for_required`, `Isolation::Host` and `degraded` guards.

## Module verdicts

One row per module at the granularity `survey` ranks. `holds` carries the reviewed SHA (the record commit's parent) and the scoped-commit count that reopens it; `survey` flags the module `held` until the count is reached, then `reopen`. A landed module pass writes `holds; landed pass-N`; a bare `landed` marks a module whose edges a seam pass reviewed. A module reviewed under a parent's pass gets its own row at the parent's SHA.

| module | status | sha | reopen at | note |
| --- | --- | --- | --- | --- |
| `address` | holds; landed pass-14 | `b269429c2` | 30 | grammar, renderer, pane binding and launch lineage at L4; one channel reconciler. |
| `agents` | holds; landed pass-18a | `045e858e8` | 30 | root exports only outside-spelled names; hook representation private; install defaults stay trait defaults. |
| `agents/(root)` | holds; landed pass-18a | `045e858e8` | 30 | with `agents`. |
| `agents/state` | holds; landed pass-18a | `045e858e8` | 30 | with `agents`; owns the budget window/scope/park types. |
| `agents/context` | holds; landed pass-18a | `045e858e8` | 30 | with `agents`. |
| `agents/definition` | holds; landed pass-18a | `045e858e8` | 30 | with `agents`. |
| `agents/adapters` | landed pass-2 | — | — | sibling families are provider policy over shared helpers; interiors are per-adapter rows. |
| `agents/adapters/(root)` | holds | `4a82d6b28` | 30 | module declarations and the registry; nothing escapes `agents`. |
| `agents/adapters/install_report` | holds | `4a82d6b28` | 30 | `report_files` is `pub(super)` for the sibling installers. |
| `agents/adapters/claude` | holds; landed pass-9; pass-27a | `ba3ca4dfa` | 30 | one ask decision and payload parser, proc-owned ancestry, three distinct home rules, child-cost fold, remote trio, and `deny_native_tool` (multi-value `--disallowedTools`, either denial order, 696a4d504). |
| `agents/adapters/codex` | holds; landed pass-9 | `7af6d9f74` | 30 | one framed transport and handshake; rollout presence and pane-confirmation seam hold. |
| `agents/adapters/kimi` | holds; landed pass-19c | `d3a951a73` | 30 | registry adapter is the sole escaping item. |
| `agents/adapters/qwen` | holds; landed pass-19c | `d3a951a73` | 30 | registry adapter is the sole escaping item. |
| `agents/adapters/copilot` | holds; landed pass-20c | `e55c49fd4` | 30 | registry adapter is the sole escaping item; payload types floored (caveat 13). |
| `agents/adapters/cursor` | holds; landed pass-25 | `4a82d6b28` | 30 | registry adapter only; its statusline lossy reader swallows inner errors, unlike `transcript_fs`, so they stay apart. |
| `agents/adapters/antigravity` | holds; landed pass-20c | `e55c49fd4` | 30 | registry adapter is the sole escaping item. |
| `agents/adapters/pi` | holds; landed pass-21c | `dde310cbf` | 30 | registry adapter is the sole escaping item. |
| `agents/adapters/opencode` | holds; landed pass-21c | `dde310cbf` | 30 | registry adapter only; tolerant `parse_payload` and `sqlite_io` adapter hold. |
| `agents/adapters/plugin` | holds; landed pass-21c | `dde310cbf` | 30 | `agents::plugins` re-exports stay `pub` for the binary; `PluginAdapter` private. |
| `agents/adapters/grok` | holds; landed pass-21c | `dde310cbf` | 30 | registry adapter is the sole escaping item. |
| `agents/adapters/droid` | holds; landed pass-21c | `dde310cbf` | 30 | registry adapter is the sole escaping item. |
| `agents/adapters/kiro` | holds; landed pass-21c | `dde310cbf` | 30 | registry adapter is the sole escaping item. |
| `agents/adapters/amp` | holds; landed pass-21c | `dde310cbf` | 30 | registry adapter only; tolerant `parse_payload` holds. |
| `agents/attribution` | holds | `ca23feb3b` | 30 | binary attribution tests build the report types directly (see deferrals). |
| `agents/lifecycle` | holds; landed pass-22c | `ca23feb3b` | 30 | `step` crate-private; signal vocabulary and state types public by signature. |
| `agents/pricing` | holds; landed pass-22c | `ca23feb3b` | 30 | rate model private; `PriceBook` and cached-book entries public for binary, bench and integration. |
| `agents/spending` | holds; landed pass-13 | `8e9477b57` | 30 | one writer per cache, one engine election; wire, walker, fold and cache records hold. |
| `agents/capabilities` | holds | `6f49ccef0` | 30 | trait methods have no own visibility and `AgentDefinition` hands out `dyn AgentIntegration`: all ten traits stay `pub`. |
| `agents/hook_types` | holds; landed pass-24b | `6f49ccef0` | 30 | hook representation at `pub(super)`; `HookRouting` and two binary-called `HookOutput` readers stay `pub`. |
| `agents/account` | holds; landed pass-24b | `6f49ccef0` | 30 | cache schema and reset-credit types stay `pub` for the integration crate. |
| `agents/credits` | holds; landed pass-24b | `6f49ccef0` | 30 | OAuth transport `pub(super)`; probe and snapshot types floored by signature. |
| `agents/managed_json_hooks` | holds; landed pass-24b | `6f49ccef0` | 30 | JSON hook merge backend reachable only from `agents`. |
| `agents/managed_statusline` | holds; landed pass-24b | `6f49ccef0` | 30 | escapes nothing. |
| `agents/settings_json` | holds; landed pass-24b | `6f49ccef0` | 30 | strict JSON primitives `pub(super)`; install path stays strict, tolerant read is `jsonc`. |
| `agents/managed_source` | holds | `6f49ccef0` | 30 | inherent methods are the adapters' seam; forwarders pinned by `0022abeca`, `aa72ece02`. |
| `agents/question` | holds; landed pass-24b | `6f49ccef0` | 30 | `NormalizedQuestion` floored by `decode` (caveat 13). |
| `agents/transcript_fs` | holds; landed pass-25 | `4a82d6b28` | 30 | tail readers at `agents` reach; its lossy visitor propagates inner errors, unlike Cursor's. |
| `agents/registry` | holds; landed pass-24b | `6f49ccef0` | 30 | catalog lookups stay `pub` for the binary. |
| `agents/runtime_control` | holds; landed pass-24b | `6f49ccef0` | 30 | forwarders are the `agents` boundary; issue and error types floored by signature. |
| `agents/login` | holds; landed pass-24b | `6f49ccef0` | 30 | everything but `LoginCatalog::names` floored by a public signature or binary test. |
| `agents/delegated_account` | holds; landed pass-24b | `6f49ccef0` | 30 | delegated OAuth usage is an `agents` interior. |
| `agents/identity` | holds; landed pass-24b | `6f49ccef0` | 30 | resolvers `pub(super)`. |
| `agents/locate` | holds; landed pass-24b | `6f49ccef0` | 30 | install-file discovery no longer escapes. |
| `agents/payload` | holds; landed pass-24b | `6f49ccef0` | 30 | `sanitize_user_prompt` and `non_empty_trimmed` keep crate reach for `store/message` tests. |
| `agents/tools` | holds; landed pass-24b | `6f49ccef0` | 30 | `definition_model_kind` is the one escaping name, for the integration crate. |
| `agents/session` | holds; landed pass-24b | `6f49ccef0` | 30 | `DaemonSessionEvidence` floored by `SessionCapability`. |
| `agents/observation` | holds; landed pass-24b | `6f49ccef0` | 30 | observation field and signature types stay `pub`. |
| `agents/petname` | holds; landed pass-24b | `6f49ccef0` | 30 | reserved words and `valid_agent_name` stay `pub` for the binary. |
| `agents/local_session_cache` | holds; landed pass-24b | `6f49ccef0` | 30 | refresh types floored at `pub(super)` (caveat 13). |
| `agents/jsonc` | holds; landed pass-24b | `6f49ccef0` | 30 | the only JSONC entry point. |
| `agents/version` | holds; landed pass-24b | `6f49ccef0` | 30 | escapes nothing. |
| `agents/model_display` | holds; landed pass-24b | `6f49ccef0` | 30 | one crate-reach selector. |
| `agents/background_shell` | holds; landed pass-24b | `6f49ccef0` | 30 | report fold at crate reach. |
| `agents/transcript` | holds | `6f49ccef0` | 30 | root-re-exported types imported by adapters; `TranscriptPosition::new` pinned by `0022abeca`. |
| `agents/turns` | holds | `6f49ccef0` | 30 | the binary's history command reads `TurnOutcome`. |
| `agents/skill_links` | holds | `6f49ccef0` | 30 | plan/apply types public; host-mode symlink ownership unchanged. |
| `agents/skills` | holds; landed pass-26c | `38b487c52` | 30 | key constructor and reader `pub(super)`; `SkillDir`, `ProviderSkillKey` and `HostSkillArgErr` floored by signature. |
| `agents/emblems` | holds | `6f49ccef0` | 30 | `EmblemTint` floored by `Emblem`. |
| `agents/open_ask` | holds | `6f49ccef0` | 30 | the record the binary and sidebar read. |
| `agents/plugins` | holds; landed pass-21c | `dde310cbf` | 30 | with `agents/adapters/plugin`. |
| `agents/conformance` | holds | `6f49ccef0` | 30 | `#[cfg(test)]`, no production surface. |
| `agents/testkit` | holds | `6f49ccef0` | 30 | `#[cfg(test)]`, no production surface. |
| `agent_activity` | holds | `2a9ec66e5` | 30 | readers named by the integration hooks suite. |
| `child_process` | holds; landed pass-24a | `2a9ec66e5` | 30 | `agent_helper_argv` stays `pub` for the integration suite. |
| `config` | holds; landed pass-24a | `2a9ec66e5` | 30 | the façade alone decides reach; child declarations keep `pub` as "offered to the façade". |
| `config/(root)` | holds; landed pass-24a | `2a9ec66e5` | 30 | with `config`; owns façade, loaders, notices and file registry. |
| `config/accounts` | holds; landed pass-24a | `2a9ec66e5` | 30 | named-account and budget records. |
| `config/agents` | holds; landed pass-24a | `2a9ec66e5` | 30 | profile, team and subagent records. |
| `config/animation` | holds | `2a9ec66e5` | 30 | `MachineConfig` field type; `is_unset` is a live serde predicate. |
| `config/attention` | holds | `2a9ec66e5` | 30 | `MachineConfig` field type. |
| `config/color` | holds; landed pass-24a | `2a9ec66e5` | 30 | `Semantic::DEFAULT` test-gated; production reads the embedded catalog. |
| `config/daemon` | holds | `2a9ec66e5` | 30 | `MachineConfig` field type. |
| `config/definitions` | holds; landed pass-24a | `2a9ec66e5` | 30 | loader already `pub(super)`; constructor deepen deferred. |
| `config/diagnosis` | holds; landed pass-24a | `2a9ec66e5` | 30 | two test-gated accessors pin `1862840`. |
| `config/display` | holds; landed pass-24a | `2a9ec66e5` | 30 | section record; `is_unset` is a live serde predicate. |
| `config/edit` | holds; landed pass-24a | `2a9ec66e5` | 30 | comment-preserving writer and template merge hold. |
| `config/effective` | holds; landed pass-24a | `2a9ec66e5` | 30 | project-task parse and error types floored by `load`. |
| `config/gc` | holds | `2a9ec66e5` | 30 | `MachineConfig` field type. |
| `config/glyphs` | holds; landed pass-24a | `2a9ec66e5` | 30 | `GlyphRole` names stay `pub` for `theme/glyphs.rs`. |
| `config/harness` | holds; landed pass-26c | `38b487c52` | 30 | `DayCap`/`TurnCap` verdicts hold; owns the prompt-cache margin its TTL parse validates against. |
| `config/loop_` | holds; landed pass-24a | `2a9ec66e5` | 30 | `Tasks` and `LoopConfig` stay `pub` for the integration crate. |
| `config/lsp` | holds; landed pass-26a | `1ec086301` | 30 | one root-marker predicate on the server config; façade controls reach. |
| `config/mux` | holds | `2a9ec66e5` | 30 | backend option records test-gated at the façade. |
| `config/notifications` | holds; landed pass-24a | `2a9ec66e5` | 30 | `NotificationKind` re-exported by `sidebar/notify.rs`. |
| `config/pets` | holds; landed pass-24a | `2a9ec66e5` | 30 | `is_default` is a live serde predicate. |
| `config/remote_control` | holds | `2a9ec66e5` | 30 | `MachineConfig` field type. |
| `config/resume` | holds; landed pass-25 | `4a82d6b28` | 30 | config reads its own backoff ramp; duration-parser verdict holds. |
| `config/scheme` | holds | `2a9ec66e5` | 30 | scheme lookup and validation hold. |
| `config/sentry` | holds | `2a9ec66e5` | 30 | `MachineConfig` field type. |
| `config/sidebar` | holds; landed pass-24a | `2a9ec66e5` | 30 | section record. |
| `config/skills` | holds; landed pass-24a | `2a9ec66e5` | 30 | skill-list validator at crate reach for sandbox and agents. |
| `config/theme` | holds | `2a9ec66e5` | 30 | `MachineConfig` field type; predicates are live serde predicates. |
| `config/web` | holds | `2a9ec66e5` | 30 | `WebPrefs` left the façade. |
| `config/worktree` | holds; landed pass-24a | `2a9ec66e5` | 30 | base parse errors left the façade. |
| `daemon_view` | holds; landed pass-15c | `944c8120e` | 30 | loop-panel acquisition is one operation; both cycles held by intent. |
| `diag` | holds; landed pass-16 | `b90a229cb` | 30 | evidence vocabulary and append mechanics at L3 below store; one sink admission point. |
| `disk` | holds; landed pass-27d | `7742c8a3e` | 30 | durability classes, filenames and lock identity hold; the class partition lives only in `Class::STATE`/`RUNTIME`. |
| `harness` | landed pass-5; pass-7; pass-8; pass-14; pass-20a; pass-23b; pass-25 | — | — | policy reaching down; never reaches `sidebar`: usage refresh returns, its CLI entry publishes. |
| `harness/schedule` | holds; landed pass-14; pass-23b | `25faa1bf6` | 30 | one arming rule; catalog, arm, team and signal error types are signature floors. |
| `harness/resume` | holds; landed pass-19a | `bdaedf434` | 30 | posture composes `plan::ResumeLaunchPosture`; recovery interior `pub(super)`. |
| `harness/plan` | holds; landed pass-19a | `bdaedf434` | 30 | owns resume-argv DTOs; validation loops held by `92b6fbeab`. |
| `harness/launch` | holds; landed pass-20a | `4d7889261` | 30 | one private `LAUNCH_FIELDS` key table encodes and decodes the pane identity env. |
| `harness/spec` | holds; landed pass-20a | `4d7889261` | 30 | layout parse family stays `pub`. |
| `harness/budget` | holds; landed pass-20a | `4d7889261` | 30 | evaluation private; ledger types are signature types. |
| `harness/run` | holds; landed pass-20a | `4d7889261` | 30 | `RunWakeErr` and `socket_path` stay `pub`. |
| `harness/rebirth` | holds; landed pass-23b | `25faa1bf6` | 30 | `materialize` returns the plan; types floored by `room::RoomContext::inspect_rebirth`. |
| `harness/auto_continue` | holds; landed pass-25 | `4a82d6b28` | 30 | `message` admission on `ResumeUnrecovered` held by intent. |
| `harness/auto_redeem` | holds; landed pass-25 | `4a82d6b28` | 30 | `AutoRedeemErr`/`Redeemed` are the CLI entry's error and return. |
| `harness/prompt_compose` | holds; landed pass-23b | `25faa1bf6` | 30 | prompt types stay `pub` via `LaunchPlan.prompt`. |
| `harness/scratch` | holds; landed pass-23b | `25faa1bf6` | 30 | scan types floored by the teams CLI. |
| `harness/subagent_policy` | holds; landed pass-23b | `25faa1bf6` | 30 | `catalog` is the launch-time entry. |
| `harness/team_prompt` | holds; landed pass-23b | `25faa1bf6` | 30 | consensus text and copy path stay `pub` for `config`. |
| `harness/orphan_sweep` | holds; landed pass-23b | `25faa1bf6` | 30 | types floored by `resolve`. |
| `harness/run_timeout` | holds; landed pass-23b | `25faa1bf6` | 30 | request type stays `pub` for the integration crate. |
| `harness/launch_reminders` | holds; landed pass-23b | `25faa1bf6` | 30 | `wrap` held by a `harness/launch` test. |
| `harness/launch_context` | holds; landed pass-23b | `25faa1bf6` | 30 | `TeamLaunchContext` floored by signatures. |
| `harness/assist_log` | holds; landed pass-23b | `25faa1bf6` | 30 | `log_path` read by the binary's stats test. |
| `harness/owed` | holds; landed pass-23b | `25faa1bf6` | 30 | binary test names `OwedWake` variants. |
| `harness/team_stage` | holds; landed pass-28g | `6b02f6006` | 30 | owns the committed lifecycle reaction and private registration re-wake. |
| `harness/ancestry` | holds; landed pass-23b | `25faa1bf6` | 30 | error floored by the resolver. |
| `harness/parent_watch` | holds; landed pass-23b | `25faa1bf6` | 30 | reached from the exec command. |
| `harness/run_wake` | holds; landed pass-23b | `25faa1bf6` | 30 | verdicted under `harness/run`. |
| `harness/auto_gc` | holds; landed pass-23b | `25faa1bf6` | 30 | reached from the binary. |
| `harness/fleet` | holds; landed pass-23b | `25faa1bf6` | 30 | reached from the binary. |
| `harness/idle_compact` | holds; landed pass-26c | `38b487c52` | 30 | reached from the binary; reads the cache margin from `config`. |
| `harness/board` | holds | `38b487c52` | 30 | `BoardErr`, `BoardSection` and `RecordReceipt` floored by `record`, which the teams CLI calls. |
| `harness/cache_keepalive` | holds | `38b487c52` | 30 | request type stays `pub` for the integration crate. |
| `harness/deadline` | holds; landed pass-26c | `38b487c52` | 30 | rung selection `pub(super)` for `run`; stop channel private. |
| `harness/launch_env` | holds | `38b487c52` | 30 | already `pub(super)`; `LaunchEnv` floored by the launch reminders render. |
| `harness/launch_plan` | holds; landed pass-24c | `e083557ba` | 30 | error and warning types floor `compile`/`apply`. |
| `harness/(root)` | holds; landed pass-24c | `e083557ba` | 30 | declarations and re-exports. |
| `ids` | holds; landed pass-5; pass-16; pass-24c | `e083557ba` | 30 | parse errors are `FromStr::Err`; conversion impls are trait boundaries, not forwarders. |
| `lsp` | holds; landed pass-26a | `1ec086301` | 30 | one sweep, memory measure, parsed target and lease list; domain presentation and broker RAII hold. |
| `message` | holds; landed pass-27c | `844951543` | 30 | timing-only DispatchMode; one interrupt-key and delivery-kind owner; reply machine holds. |
| `mux` | holds; landed pass-18c | `9ba3585b9` | 30 | planner verdicts private; `SplitPaneOptions::from_command` owns the pane-command projection. |
| `mux/(root)` | holds; landed pass-18c | `9ba3585b9` | 30 | with `mux`. |
| `mux/tmux` | holds; landed pass-18c | `9ba3585b9` | 30 | with `mux`. |
| `mux/zellij` | holds; landed pass-10 | `173682d90` | 30 | presence lifecycle deepened; topology schema is the public wire. |
| `mux/capabilities` | holds; landed pass-24c | `e083557ba` | 30 | only `drops_desktop_osc` public. |
| `mux/width` | holds; landed pass-24c | `e083557ba` | 30 | width types named by integration and `cli`. |
| `mux/focus_key` | holds; landed pass-24c | `e083557ba` | 30 | floored by `MuxBackend::register_room_key`. |
| `mux/focus_anchor` | holds; landed pass-24c | `e083557ba` | 30 | pass-18c verdicts hold. |
| `mux/reconcile` | holds; landed pass-24c | `e083557ba` | 30 | liveness and recovery driven by the integration crate. |
| `mux/recovery` | holds; landed pass-24c | `e083557ba` | 30 | at the reach its readers need. |
| `mux/command` | holds; landed pass-24c | `e083557ba` | 30 | `CommandSpec` verbs public for integration and `cli`. |
| `mux/selection` | holds; landed pass-24c | `e083557ba` | 30 | backend choice is the CLI's. |
| `mux/domain` | holds; landed pass-24c | `e083557ba` | 30 | driven by the integration proc suite. |
| `mux/companion_layout` | holds; landed pass-24c | `e083557ba` | 30 | the layout's vocabulary. |
| `mux/binaries` | holds; landed pass-24c | `e083557ba` | 30 | read by `cli/doctor`. |
| `mux/width_target` | holds; landed pass-24c | `e083557ba` | 30 | the room's width intent. |
| `mux/tab_name` | holds; landed pass-24c | `e083557ba` | 30 | already crate-internal. |
| `mux/mount_proof` | holds; landed pass-24c | `e083557ba` | 30 | the backends' mount proof. |
| `mux/pane_writer` | holds; landed pass-24c | `e083557ba` | 30 | the per-pane write lock `pane send` runs through. |
| `pane` | holds; landed pass-24a | `2a9ec66e5` | 30 | owns `ClientPaneView`; `proc` cycle by intent. |
| `proc` | holds; landed pass-25 | `4a82d6b28` | 30 | platform seams, bounded execution and pane-probe abstention hold. |
| `reload` | holds; landed pass-23c | `1830bd0c6` | 30 | one durable staged-build path. |
| `remote` | holds; landed pass-21a | `e204ceebd` | 30 | pure transitions here, drivers in `cli/remote`. |
| `remote_control` | holds; landed pass-15c | `944c8120e` | 30 | one enable preflight; typed snapshot and batch toggle. |
| `room` | holds; landed pass-15c | `944c8120e` | 30 | birth, ordered teardown and seven liveness policies hold. |
| `sandbox` | holds; landed pass-23b | `25faa1bf6` | 30 | the integration sandbox suite drives plan/apply/argv; `prepare` kept for it. |
| `sidebar` | landed pass-4 | — | — | election, fusion, refresh lanes, own cadences; interiors have rows. |
| `sidebar/(root)` | holds; landed pass-23c | `1830bd0c6` | 30 | one data plane; `pub` items are binary, bench or integration reached, or signature floors. |
| `sidebar/refresh` | holds; landed pass-25 | `4a82d6b28` | 30 | one refresh entry, one rate-limit transaction; one `pub` account-cache publish entry. |
| `sidebar/consumer` | holds; landed pass-23c | `1830bd0c6` | 30 | two readers `pub` for the hotpath bench. |
| `sidebar/frame` | holds; landed pass-23c | `1830bd0c6` | 30 | `PaneFrame` wire public field by field. |
| `sidebar/produce` | holds; landed pass-17a | `78fa580ea` | 30 | named entries, no fold-mode core. |
| `sidebar/enrich` | holds; landed pass-20b | `b96a0f293` | 30 | ordered fold spine (fold order is an invariant). |
| `sidebar/observe` | holds; landed pass-20b | `b96a0f293` | 30 | `observe_into` owns detection, send and drop accounting. |
| `sidebar/presence` | holds; landed pass-20b | `b96a0f293` | 30 | `ingest_zellij_wake` is one transaction. |
| `sidebar/notify` | holds; landed pass-20b | `b96a0f293` | 30 | debounce, link-health episodes and delivery are distinct policies. |
| `sidebar/timing` | holds; landed pass-23c; pass-24c | `e083557ba` | 30 | three cadences narrowed; the rest hold with `sidebar/(root)`. |
| `sidebar/cache` | holds; landed pass-23c | `1830bd0c6` | 30 | with `sidebar/(root)`. |
| `sidebar/unread` | holds; landed pass-23c | `1830bd0c6` | 30 | with `sidebar/(root)`. |
| `sidebar/read_marks` | holds; landed pass-23c | `1830bd0c6` | 30 | with `sidebar/(root)`. |
| `sidebar/fuse` | holds; landed pass-23c | `1830bd0c6` | 30 | with `sidebar/(root)`. |
| `sidebar/meter` | holds; landed pass-23c | `1830bd0c6` | 30 | with `sidebar/(root)`. |
| `sidebar/event_store` | holds; landed pass-23c | `1830bd0c6` | 30 | with `sidebar/(root)`. |
| `sidebar/body_filter` | holds; landed pass-23c | `1830bd0c6` | 30 | with `sidebar/(root)`. |
| `sidebar/agent_projection` | holds; landed pass-23c | `1830bd0c6` | 30 | with `sidebar/(root)`. |
| `sidebar/workspace_projection` | holds; landed pass-23c | `1830bd0c6` | 30 | with `sidebar/(root)`. |
| `sidebar_pane/app` | holds; landed pass-26b | `be91f3434` | 30 | loop clock mechanics consume one render animation answer. |
| `sidebar_pane/pets` | holds; landed pass-22a | `1b6ece679` | 30 | preview types `pub` for the CLI. |
| `sidebar_pane/pixel` | holds; landed pass-22a | `1b6ece679` | 30 | resend gate (`30108e572`) and Kitty support variants (`3718d6f34`) hold. |
| `sidebar_pane/supervise` | holds; landed pass-22a | `1b6ece679` | 30 | worker entry points `pub` for the CLI. |
| `sidebar_pane/render/chrome` | holds; landed pass-22a | `1b6ece679` | 30 | bottom-chrome builders are `compose`'s vocabulary. |
| `sidebar_pane/render/labels` | holds; landed pass-22a | `1b6ece679` | 30 | glyph and meter vocabulary at render reach. |
| `sidebar_pane/render/theme` | holds; landed pass-22a | `1b6ece679` | 30 | the Layer-3/4 carrier the color invariant exempts. |
| `sidebar_pane/render/animation` | holds; landed pass-26b | `be91f3434` | 30 | cadence decision is now render's, contradicting pass 23c. |
| `sidebar_pane/render/(root)` | holds; landed pass-26b | `be91f3434` | 30 | one live draw entry and pane-reach selectors. |
| `sidebar_pane/render/compose` | holds; landed pass-18b | `045f8bfe6` | 30 | render bundle; frame types at pane reach. |
| `sidebar_pane/render/sections` | holds; landed pass-26b | `be91f3434` | 30 | distinct width-budget rules with section-private gutter and layout imports. |
| `sidebar_pane/(root)` | holds; landed pass-24c | `e083557ba` | 30 | declarations and re-exports. |
| `sidebar_pane/view` | holds; landed pass-24c | `e083557ba` | 30 | the pane's body projection; row cap read by the CLI fixture. |
| `sidebar_pane/render/ui_state` | holds; landed pass-26b | `be91f3434` | 30 | pane-reach state owns the active roster projection. |
| `sidebar_pane/render/interaction` | holds; landed pass-24c | `e083557ba` | 30 | hit map pane-internal. |
| `sidebar_pane/render/odometer` | holds; landed pass-24c | `e083557ba` | 30 | `Roll` floored by `TallyAnim`. |
| `sidebar_pane/render/scrollbar` | holds; landed pass-24c | `e083557ba` | 30 | pane-internal. |
| `sidebar_pane/render/fmt` | holds; landed pass-24c | `e083557ba` | 30 | label formatters are the render vocabulary. |
| `sidebar_pane/render/layout` | holds; landed pass-24c | `e083557ba` | 30 | width vocabulary at render reach. |
| `sidebar_pane/render/ansi` | holds; landed pass-24c | `e083557ba` | 30 | the one ANSI writer. |
| `store` | landed pass-1; pass-7; pass-8; pass-15a; pass-16; pass-17b; pass-21b; pass-27b | — | — | owns every record it persists, imports nothing above it. |
| `store/(root)` | holds; landed pass-24c | `e083557ba` | 30 | `snapshot` `pub` for the integration crate. |
| `store/snapshot` | holds; landed pass-28d | `c34ca45c2` | 30 | view model crate-wide because the renderer decodes it; local-bind cascade and lifecycle fold inherent. |
| `store/writer` | holds; landed pass-27b | `52b2358b0` | 30 | one log boundary; one queue terminal step; one lifecycle staging path. |
| `store/event` | holds; landed pass-21b | `5e6580fb0` | 30 | legacy `message.removed` parse holds. |
| `store/message` | holds; landed pass-21b | `5e6580fb0` | 30 | status aliases hold for mixed-binary workspaces; header grammar and codec hold. |
| `store/gc` | holds; landed pass-21b | `5e6580fb0` | 30 | exporter check pinned by `b58b6594c`. |
| `store/event_log` | holds; landed pass-25 | `4a82d6b28` | 30 | the store's own write path; incremental read `pub(crate)` for the reply poll. |
| `store/sidecar` | holds; landed pass-23a | `250f10820` | 30 | store-internal; digest `pub(crate)` for the harness. |
| `store/session_death` | holds; landed pass-23a | `250f10820` | 30 | `same_agent_instance` `pub(crate)` for the address resolver. |
| `store/runtime` | holds; landed pass-23a | `250f10820` | 30 | two owner constructors serve distinct paths. |
| `store/live_roster` | holds; landed pass-23a | `250f10820` | 30 | `publish` `pub` for two integration suites. |
| `store/follow` | holds | `250f10820` | 30 | floored by the follower signatures. |
| `store/agent_context` | holds; landed pass-25 | `4a82d6b28` | 30 | rest-certificate guard is store-owned and `pub`. |
| `store/subagent_context` | holds | `250f10820` | 30 | reached by integration and sidebar enrichment. |
| `store/run` | holds | `250f10820` | 30 | `WakeupFrame` is the pinned run-wake wire. |
| `store/active_time` | holds | `250f10820` | 30 | floored by `read_for_keys`. |
| `theme` | holds; landed pass-22b | `339310452` | 30 | every re-export has an outside reader; OKLab blends private. |
| `transcript` | holds; landed pass-24a | `2a9ec66e5` | 30 | `TranscriptLogErr` carried by the public signatures. |
| `trust` | holds; landed pass-22b | `339310452` | 30 | `_with_roots` seams private except `grant_with_roots`. |
| `wakeup` | holds; landed pass-24a | `2a9ec66e5` | 30 | the sidebar wire at L2 below `store`. |
| `web` | holds | `1830bd0c6` | 30 | domain façade over ttyd and gate interiors; `sidebar_pane` cycle by intent. |
| `workspace` | holds; landed pass-22b | `339310452` | 30 | owns the room identity pin keys and `workspace.json`. |
| `worktree` | holds; landed pass-8; pass-23b | `25faa1bf6` | 30 | landed proof at crate reach; request types named by `cli/worktree.rs`. |
| `forge` | holds; landed pass-8; pass-24c | `e083557ba` | 30 | URL forwarders deleted; `PrLink` floored by `RefreshedLanes::pr_states`. |
| `osc` | holds; landed pass-8; pass-24c | `e083557ba` | 30 | one escape writer. |
| `build_id` | holds; landed pass-8; pass-24c | `e083557ba` | 30 | `current_if_ready` deferred. |
| `(root)` | holds; landed pass-24c | `e083557ba` | 30 | `lib.rs` declarations and public re-exports. |
| `channel` | holds; landed pass-24c | `e083557ba` | 30 | record types named by `cli/channel.rs`. |
| `daemon_content` | holds; landed pass-24c | `e083557ba` | 30 | `resolve_content` at crate reach for the elder. |
| `lane` | holds; landed pass-24c | `e083557ba` | 30 | counters are the observability lane's vocabulary. |
| `observability` | holds; landed pass-24c | `e083557ba` | 30 | entry points reached from `main.rs`. |
| `sock` | holds; landed pass-24c | `e083557ba` | 30 | `SocketPathTooLong` is a `disk::paths` error variant. |
| `tui` | holds; landed pass-24c | `e083557ba` | 30 | `TuiLogWriter` built in `main.rs`. |
| `uninstall` | holds; landed pass-24c | `e083557ba` | 30 | machine-wide entry points `pub` for the CLI. |
| `update` | holds; landed pass-24c | `e083557ba` | 30 | error and release types floor the CLI's entry points. |
| `utils` | holds; landed pass-24c | `e083557ba` | 30 | `tokens::estimate` is the integration crate's probe. |

## Admission intents

One row per intended upward edge, `` `from` → `to` `` with an optional `to::{a,b}` group. `survey` lists an admitted edge with no row as `unreviewed`. An edge a pass closes loses its admission and its row; a `close` intent names the pass that will close it.

| from → to | sites | intent | reason / seam |
| --- | ---: | --- | --- |
| `store` → `agents` | 134 | keep | the store folds the agent model |
| `config` → `agents` | 17 | keep | catalog and spec vocabulary are the shared language below the store |
| `theme` → `agents` | 5 | keep | same |
| `trust` → `agents` | 1 | keep | same |
| `proc` → `agents` | 4 | keep | same; pane-probe classification needs the catalog |
| `config` → `store::message` | 3 | keep | `AutoCompact` is persisted record data; the persisted home wins |
| `config` → `harness::spec` | 9 | keep | harness owns layout validation and resolution |
| `config` → `harness::budget` | 4 | keep | budget parsing is harness policy |
| `config` → `harness::schedule` | 4 | keep | trigger and surplus grammar are harness policy |
| `config` → `harness::team_prompt` | 2 | keep | the harness owns the built-in consensus text and its copy path |
| `agents` → `address` | 1 | keep | `agent_handle` is the canonical routable address |
| `daemon_view` → `sidebar::timing` | 1 | keep | `EVENT_PANE_TTL` is the sidebar's event-mode cadence |
| `daemon_view` → `daemon_content` | 1 | keep | slot cardinality and pane policy stay one decision |
| `daemon_view` → `sidebar::frame` | 1 | keep | the elder reads the published `PaneFrame` as disposable topology truth |
| `daemon_view` → `sidebar::cache` | 2 | keep | a stale frame never suppresses repair |
| `room` → `sidebar` | 3 | keep | birth purges rebirth heartbeats; teardown orders the orphan sweep |
| `room` → `sidebar::body_filter` | 1 | keep | pristine birth resets presentation state (`0e3b80c55`) |
| `room` → `reload` | 1 | keep | ownership stages through reload's durable-build path (`f7e1b4153`) |
| `message` → `harness::ancestry` | 2 | keep | `@all` sender exclusion needs durable launch identity |
| `message` → `sidebar::produce` | 4 | keep | dispatch picks a rollup-only or full fold after inspecting pending records |
| `message` → `harness::auto_continue` | 1 | keep | `ResumeUnrecovered` re-verifies a `Resume` park at delivery |
| `message` → `harness::run` | 1 | keep | the subagent-digest join guard |
| `message` → `harness::assist_log` | 3 | keep | the auto-compact assist is observable only where the command reaches `Sent` |
| `message` → `harness::schedule::pending` | 1 | keep | the reply view attaches scheduler-owned turn waits (`4f3adcd90`) |

## Open deferrals

Candidates a pass judged real but could not land, each with what unblocks it.

- `cli/hooks` → `harness/run`: supervised spend settlement still assembles context cost, transcript token totals and spend recording; waits for pass 28f's run ownership.
- `cli/hooks` → `store/snapshot`: repeated agent lookup by `(kind, agent_id)` waits for pass 28d's snapshot ownership.
- `harness/team_stage` ↔ `cli/teams`: trusted team loading remains duplicated; sharing it needs a typed harness error and a pass owning the CLI flip caller.
- `agents`: `_rimz_managed` spelled in `managed_source`, `managed_json_hooks`, `managed_statusline`; one owner measured line-neutral. Waits for a marker change or a relayer of the managed trio.
- `agents/adapters`: `attach_hook_context` is identical in claude, codex and qwen (droid and grok omit only `hookEventName`) while `Capabilities::hook_context` restates it; declaring the reply shape in the spec would delete the impls and the agreement test (about −37). Waits for a seam pass over `agents/definition` and the adapters.
- `agents/adapters/codex`: `cap_turn_error_label` and `TURN_ERROR_LABEL_MAX` copy Claude's `statusline` pair word for word; a shared helper beside `TurnErrorClass::classify_label` in `agents/context` lands with a pass owning codex or `agents/context`.
- `config/definitions`: one load context for the seven-argument `Resolver::new` and the `SeatLoader` repack, plus one safe-name predicate (about −20 SLOC). Waits for the module's pace to drop below hot.
- `store/writer` ↔ `harness/rebirth`: `record_agents_ended` repeats reap's `append_ended_sessions`; batching them changes partial-failure shape. Waits for a rebirth pass that owns both.
- `harness/launch`: three relaunch sites in `cli/agents_cmd/{fork,restart}.rs` repeat posture prompt fields; needs a posture-aware seam `launch` may not import.
- `harness/schedule/runner.rs`: `run_command`/`prepare_check` carry the cx; `fire_due_tasks` and `parse_signal_selector` are the seams. Waits for pace to settle.
- `message`: `compact_idle` absorbing idle preflight needs `CompactErr` to separate a pre-queue refusal-check failure from a publication failure (dropping the preflight today changes assist records on a raced refusal and on a store read failure).
- `store/message` ↔ `address`: header literals spelled on both sides; a store-owned composer measured line-neutral. Waits for a header grammar change.
- `agents/attribution`: a `testkit` fixture builder would let five report types narrow. Waits for its `fix(attribution)` churn to settle.
- `disk`: the `StatePaths`/`RuntimePaths` constructor family (5 + 8, ~460 test sites on `under`/`under_named`) → `for_project_root(root, home)`, `for_workspace(id, home)`, `RuntimePaths::for_state(state, runtime_root)`. Waits for a round with no concurrent passes.
- `disk::parse_cache`: fold the `(mtime,len)` key onto the full `FileStamp`. Waits for a `store/snapshot` or `disk` pass that takes cache identity in scope.
- `ids::ViewId::as_str`: no production reader, `dead_code` blocks narrowing, tests hold it. Wait for a pass on `sidebar/produce`.
- `build_id::current_if_ready`: its only reader is behind a non-default feature. Waits for atlas to index feature-gated items.
- `sidebar_pane ↔ web`: four sites; closing either side needs a neutral pixel-wire module below both (a seam pass).
- Reported, not fixed: Codex transcript lookup ignores `CODEX_HOME`, substring daemon classification, per-attempt refresh budget; the provider tab rail measures `chars().count()`.
- Compiler-refused narrowings (atlas caveat 13) are never deferrals; do not re-plan them from `inspect`'s `narrow to` column.
