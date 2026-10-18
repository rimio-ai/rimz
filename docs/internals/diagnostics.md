# Diagnostics

RimZ records what goes wrong as typed evidence, so a transient fault leaves something to debug after it heals. A roster that empties and refills over two seconds, a spend figure that blinks to zero, or a card that outlives its pane is invisible an hour later unless something wrote it down at the moment it happened.

One boundary makes this safe: **correctness reads the store, CAS rules, and caches, and no correctness path reads a diagnostic record back.** Diagnostics are evidence for a human, and every write is best-effort.

Evidence has two destinations. The default is a durable log on the box, always on, which most of this page describes along with the [frame-stream observer](#the-frame-stream-observer) that feeds it. The second is [off-box error reporting](#off-box-error-reporting) to a Sentry project, compiled only into dev builds and dormant until a contributor opts in.

## Where diagnostics land

Diagnostic logs live in persistent state because an investigation starts after the pane, mux session, or machine has gone away. Workspace logs land under `~/.rimz/ws/<workspace-dir>/`, which survives reboot like the store; runtime files under `$XDG_RUNTIME_DIR` die with the session.

| Surface | Location | Records | Owner |
| --- | --- | --- | --- |
| `audit/diag.log.jsonl` | workspace state dir | typed anomaly records, rate-limited | this page |
| `audit/diag-frames/` | workspace state dir, `0700` | prior and offending pane-frame pairs | [Frame captures](#frame-captures) |
| `audit/notify.log.jsonl` | workspace state dir | notification emits, bell decisions, unread transitions | [notifications.md](./sidebar/notifications.md#the-trace-log) |
| `audit/focus.log.jsonl` | workspace state dir | edge-triggered frame publications and selection decisions, plus every cross-tab push handshake | this page |
| `audit/plugin-presence.log.jsonl` | workspace state dir | Zellij presence-plugin keepalive samples | [below](#zellij-presence-plugin-telemetry) |
| `focus-repairs.log.jsonl` | account-global `~/.rimz/logs/`, beside the assist history | automatic focus-repair evidence and outcomes | [state.md](./sidebar/state.md) |
| `audit/binding.log.jsonl` | workspace state dir | pane-binding decisions | [sidebar.md](./sidebar/sidebar.md) |
| `hook-feed.log.jsonl` | machine `~/.rimz/logs/` | returned hook-feed errors, including before workspace resolution | `at`, `source`, nullable `event`, full `error` chain; 4 MiB rotation |
| `audit/hibernate.log.jsonl` | workspace state dir | `wake` latency, reclaimed bytes and thaw errors, `wake_refault` samples, and `wake_lock_timeout` recovery | [hibernate.md](./harness/hibernate.md#wake-and-recovery) |
| `lsp.log.jsonl` | machine `~/.rimz/logs/` | shared-server refusals, queue timeouts, watch errors, kills, evictions, crashes, and unreadable registry rows | [lsp.md](./lsp.md#visibility) |
| `lanes/topology-writer-conflict.json` | workspace runtime dir | latest Zellij topology writer conflict | [multiplexers.md](./multiplexers.md#the-zellij-presence-plugin) |

The `diag/` module owns the JSONL surfaces, and all of them share the append, decoded-visit, and generation-path helper in [`disk::rotating`](../../crates/rimz/src/disk/rotating.rs). A log rotates to one kept generation (`<name>.1.jsonl`): the workspace logs at the shared 1-MiB cap (`disk::retention::ROTATING_LOG_MAX_BYTES`), the focus-repair log at the assist history's 4 MiB. A failed append logs at debug and the calling path continues. A reader visits the retained generation, then the active file, and skips missing files and malformed lines.

The LSP log rotates at 4 MiB and carries timestamp, checkout root, server name, event, and structured details. `rimz doctor` reads its latest refusal or queue timeout for human inspection. The `evicted` event carries free bytes before and after, victim RSS and peak RSS, and `for: {root, server}` naming the query's requested server. Query memory-refusal errors come directly from the broker, not diagnostic inference. The separate `~/.rimz/lsp-history.jsonl` is an admission input owned by `lsp/history.rs`, not diagnostics; it carries idle stops and restart gaps without separate diagnostic events.

The first three surfaces carry workspace identity through one [`DiagSink`](../../crates/rimz/src/diag.rs). A disabled sink keeps the same methods and does nothing, so emitting callsites need no `#[cfg]` or branch. The notification trace and the binding log belong to their own subsystems and share only the rotating helper; the rest of this page is `audit/diag.log.jsonl` and its captures.

The focus trace ([`focus_trace.rs`](../../crates/rimz/src/diag/focus_trace.rs), schema `rimz.focus_trace.v1`) uses the notification envelope and sink identity, without rate limiting. `frame_published` records changes in focus, its origin, client-sample freshness, own-pane membership, roster, or publication class; metrics-only writes use the topology class. It also records `client_sample_withheld: true` while the plugin withholds unsettled clients, omitted when false, and a change in that flag is a publication edge. `fold_decided` records changes in snapshot focus, own-view availability, baseline, or selected pane separately for each attachment, including seeds and event-fused folds. Identical publications and folds append nothing; comparison is per host process, so a replacement host's first publication starts a new trace baseline. Private CLI reads append no publication trace. Correlate a fold with the latest producer record whose `produced_at_ms` is at or below its `panes_produced_at_ms`; unchanged newer publications need no duplicate record. Neither correctness nor rendering reads this log back.

Each `push_handshake` records a same-host cross-tab jump, without edge gating or sampling. Its envelope identifies the source attachment and the write time. The event carries the intent `nonce`, `target`, `budget_ms` (100, or 300 after fitting a stale tmux view), `decided_ms`, `dispatched_ms`, and optional `fit_ms`. When a fit callback runs, `fit_ms` measures push entry through the send instant, including the fit; it is omitted when zero or no callback ran. Every other event millisecond value is elapsed from the source's send instant, taken after any fit returns; `dispatched_ms` ends when the mux dispatch returns. The source classifies `outcome` at its wait decision: `painted` for a `painted` or `displaced` report consumed before the budget, `released` if all handles drop before it without a landing report, and `timed_out` for any decision at or after the budget, including a release or landing report at the tie. Recording follows dispatch, after collecting late reports until all handles drop or 1 s from send, so neither logging nor late collection delays the jump.

`peers` preserves subscriber order and includes each other attachment's `instance_id` and `delivery` (`sent`, `inbox_full`, or `inbox_closed`), even if delivery failed. A failed send has no stages or verdict. Destination `received_ms` stamps arrival, `snapshot_ms` ends pending-snapshot preparation, `folded_ms` ends the intent fold, and `finished_ms` stamps its verdict. A known non-owner returns before preparation or folding, and a datagram-first nonce also skips those stages; both omit the middle stamps. Every other path carries both stamps. Verdicts are `painted`, `displaced`, `not_destination`, `exiting`, `expired`, and `superseded`. `displaced` means the placement was consumed with the owning card selected at the expected height but missed its requested row; like `painted`, it ends the source's wait. A sent peer without a verdict either tore down without reporting or did not report within the horizon. `blocked_by` retains the first `paint_hold` or `confirming_empty` gate encountered while parked; exit has its own verdict. `unmatched_paints` counts parked paints that fail the acknowledgement predicate, with `misplaced_paints` counting the subset whose selected row is already correct but whose placement or other predicate detail is not. Absent optionals and zero counts are omitted. To select misses from a focus log:

```sh
jq 'select(.event.kind=="push_handshake" and (.event.outcome!="painted" or any(.event.peers[]; .verdict=="displaced")))' audit/focus.log.jsonl
```

### Hook drain evidence

`crates/rimz/src/diag/record.rs::DiagEvent` records ingress tail truncation (`hook_ingress_truncated`), corrupt records (`hook_ingress_skipped`), failed application (`hook_apply_failed`), and delayed application (`hook_drain_lag`) in the main diagnostic log. All four have Warn source severity and the standard workspace/session envelope. Each drainer owns one sink, built from the workspace record's session name; an unreadable record disables diagnostics without blocking the drain. Repair and failure identities use byte offsets or ingress event IDs, so distinct failures retain distinct records. Doctor treats truncation as contained and skipped records or failed application as investigative warnings.

`crates/rimz/src/harness/hook_drain.rs::Drainer::drain` emits `hook_drain_lag` immediately before applying a frame when apply start minus its ingress timestamp exceeds 10 seconds. The record carries `ingress` (the frame's event ID), `hook_event` (its optional event name), `lag_ms`, and `pending_bytes` (current ingress log length minus the applied cursor, including this frame). Frames at or below the threshold emit no lag record. One room-wide lag identity admits one sample per 30 seconds per drainer; suppressed repeats are counted on the next admitted sample. Worst lag and peak pending bytes are therefore sampled maxima, not exact episode peaks. This is diagnostic evidence, not an assist record or a delivery decision input.

The retired `audit/hook-drain.log.jsonl` files are neither written nor read; the ordinary 30-day/64-MiB audit sweep removes them.

### Zellij presence-plugin telemetry

`audit/plugin-presence.log.jsonl` holds the samples the presence plugin's keepalive delivers ([`plugin_presence.rs`](../../crates/rimz/src/diag/plugin_presence.rs), appended from `sidebar::presence`). Each sample carries the exact `(loaded_at_ms, plugin_id)` generation and plugin build; the rest of its fields are listed in [multiplexers.md](./multiplexers.md#the-zellij-presence-plugin).

`rimz doctor` lists the presence-plugin panes loaded in the live session and joins each id to its newest telemetry generation. A plugin is active when it is the fresh topology cache's writer, rejected when it is the stale writer in `lanes/topology-writer-conflict.json`, and inactive otherwise; a build that differs from the desired plugin build is marked outdated. When the live listing fails, Doctor reports the probe unavailable rather than presenting retained telemetry as live.

Memory growth and stale-writer rejections are informational, several loaded plugins point to `rimz reload`, and a recent genuine failure delta is actionable. Pages climbing with the Zellij server's RSS put a leak in the plugin's WASM linear memory; flat pages under climbing RSS put the growth in Zellij-native state on the plugin command path. Samples that lack a build or split failure counts report those fields as unknown.

## The record envelope

Every line in `audit/diag.log.jsonl` is one `DiagEnvelope` from [`record.rs`](../../crates/rimz/src/diag/record.rs):

| Field | Meaning |
| --- | --- |
| `v` | schema version, `rimz.diag.v1`; readers drop other versions |
| `build` | build id of the writing executable |
| `workspace_id`, `session_name` | the room the writer served |
| `instance_id` | sidebar instance id, when a renderer wrote it |
| `at_ms` | Unix milliseconds |
| `severity` | `info`, `warn`, or `error`, derived from the event ([Severity](#severity)) |
| `suppressed_since_last` | records the rate limit swallowed since the last one written, omitted when zero ([Volume and retention](#volume-and-retention)) |
| `event` | the tagged event; `event.kind` names it ([Event taxonomy](#event-taxonomy)) |

The `build` id is a digest of the executable read from its linker-provided build-id note ([`build_id.rs`](../../crates/rimz/src/build_id.rs)), and every published pane frame carries the same stamp. Records written by an old and a new build during an upgrade therefore stay distinguishable in place. Legacy `mixed_build_writers` records mark overlap between the former competing publishers; the host-only producer no longer emits them. `rimz doctor` still warns when fresh heartbeats show more than one build writing.

Records are anomaly-first: routine fetch ticks, successful paints, and stable cache hits write nothing individually. The fetch worker emits one aggregate delivery record per minute. The sidebar width records trace each accepted or rejected keypress, controller step, and terminal outcome, and `work_pane_boundary_moved` preserves geometry drift that would otherwise leave no trace. Hidden-view geometry repair writes one aggregate per finished sweep or jump fit, never one per window.

Pure projection layers return diagnostics as data, and impure callers append them through the `DiagSink`. Store reducers and the renderer gate carry no disk-write API, and the observer's writer thread emits through the same sink, so every source shares one envelope and one file.

## Event taxonomy

The emitter is the triage pointer. Producer kinds describe pane-source truth, renderer kinds one node's holds and refreshes, supervisor kinds process lifecycle, projection kinds binding and grouping, and `frame_anomaly` the rendered symptom as the [observer](#the-frame-stream-observer) judged it.

| Event | Emitter | Evidence |
| --- | --- | --- |
| `frame_rejected`, `frame_shrink_verified`, `pane_count_drop`, `pane_carry_forward`, `pane_carry_refuted`, `carry_forward_expired`, `hosted_carry_dropped`, `foreign_session_pane` | `sidebar::produce::panes` | Ok-but-empty frames and missing-own-pane reads, verified shrinks, pane drops with affected views, carried and refuted panes, carry expiry, hosted-agent carry declines, foreign-session leaks |
| `resolution_fallback` | `sidebar::produce` | A pane-resolution snapshot falling back to the rollup, with its reason |
| `duplicate_pane_id` | `sidebar::frame`, `store::snapshot::view` projection | A pane id listed twice |
| `mixed_build_writers` | Legacy pane producers | A prior frame stamped by a different build; retained in historical logs, no longer emitted |
| `gate_hold`, `gate_release`, `fetch_failure`, `health_alert`, `link_alert`, `live_roster_held` | `sidebar_pane::app` | Renderer-side holds and releases, failed fetches, degraded refresh episodes, remote-link degraded and recovered episodes, a recovery-roster removal withheld because the mux did not list the session, with the agents kept |
| `group_migration` | `sidebar_pane::app::state` | A pane whose group changed between committed snapshots, with cwd before and after |
| `renderer_panic`, `renderer_exit` | `sidebar_pane::app`; `renderer_exit` also `sidebar_pane::supervise` | Panics that would otherwise vanish with the pane; self-close and give-up exits with their cause |
| `sidebar_width_intent`, `sidebar_width_nudge`, `sidebar_width_settle` | `sidebar_pane::app::width_control` | Intent verdicts, controller nudges, learned feedback, and terminal outcomes for `a`/`d` width control |
| `sidebar_host_unavailable`, `supervisor_pane_gone`, `supervisor_convergence`, `supervisor_preflight_rejected`, `self_close_rejected`, `renderer_exit` | `sidebar_pane::supervise` | A failed host round with its cause and retry delay; a watchdog-confirmed missing pane; a stable target about to be preflighted, or a preflight that refused one with its reason; a declined self-close with the sibling count; a confirmed self-close |
| `work_pane_boundary_moved` | `sidebar::presence` (Zellij), `sidebar::presence::tmux` | Before and after horizontal geometry for a stable work-pane set whose view and sidebar widths did not change |
| `topology_writer_changed`, `topology_write_rejected` | `sidebar::presence` | Zellij topology writer generation flips and rejected stale writers, with plugin id, loaded-at generation, accepted writer, and reject count |
| `tick_budget_breach` | `sidebar::meter` | Sustained over-budget producer ticks: last and worst wall time, mux wait, fold bytes, spawns, declared budgets, streak length, episode `since_ms`, and recovery ([performance.md](./performance.md#the-tick-budget)) |
| `frame_anomaly` | The host's `sidebar::observe` writer thread | Detector verdicts on the rendered stream, each with its detector key, evidence, and frame stamp |
| `row_conflict`, `newborn_quarantined`, `local_session_bind_rejected`, `ghost_session_bind` | `store::snapshot::view` | Duplicate agent identity suppression, a newborn known-command pane held until its cwd resolves, a contained evidence-free or stale local-session bind, and an old exact session stamp contradicted by a newer durable launch |
| `pane_cache_divergence`, `sidebar_orphan_reaped` | `reload` (`rimz reload`, `rimz sidebar repair`) | A fresh pane cache omitted a sidebar process that authoritative mux truth proved alive; a sidebar reaped after two authoritative omissions |
| `subagent_digest_backstopped`, `subagent_orphan_reaped`, `subagent_orphan_repair_failed` | hidden subagent helpers started by `harness::orphan_sweep` | A fleet digest the normal wrapper path missed, queued from durable run records; a child closed after its parent launch stayed ended or absent past `ORPHAN_GRACE`, or the repair error when that failed ([subagents.md](./harness/subagents.md#backstops)) |
| `subagent_zone_lookup_failed` | `cli::supervised::pane` | Placement fell back to a companion tab: `lookup` (`projection`, `caller`, or `pane_listing`), durable `run_id`, and `error`; identity is `(run_id, lookup)`. The launch receipt is unchanged |
| `client_reaped` | `cli::room::attach_exec` | Stale mux clients killed during attach, with killed pids, client counts before and after, and whether the reap settled or timed out |
| `provider_startup_exit` | `cli::agents_cmd::exec` | A non-successful provider exit within 60s of spawn that the wrapper did not cause (no wrapper stop, parent end, or signal to the wrapper), for launch, pick, resume, or fork: `agent_kind`, optional `agent_name`, `action`, exactly one of `exit_code` or `signal`, `startup_ms`, and `relaunches`; rate-limit identity is `(agent_kind, agent_name)` |
| `model_catalog_refresh_failed` | `cli::agents_cmd::exec` | An actual failed model-catalog refresh, including an unusable fetched catalog: `agent_kind`, account `login`, requested `alias`, fallback `rung` (`cached_catalog` or `baked`), and real error `reason`; rate-limit identity is `(agent_kind, login)`. Emitted before the launch's compilation outcome is consulted; fresh-cache and cooldown resolutions emit nothing |
| `pane_winsize_repaired` | `cli::agents_cmd::exec` | An unsized pane tty repaired before provider startup: `pane`, `rows`, and `cols`; rate-limit identity is the pane |
| `hidden_views_fitted` | `sidebar_pane::app::hidden_views` | Host-owned tmux geometry repair: `trigger` (`sweep` or `jump`), `target_cols`, `target_rows`, `fitted`, `failed`, and `skipped` (the mutation guard declined); the envelope names the session. One aggregate per finished sweep (including a restarted partial sweep) or jump fit, admitted under the per-kind ceiling rather than the identity cooldown. An empty aggregate writes nothing |
| `recovery_seat_refilled` | `harness::rebirth` | A parked session ended because a live replacement holds its team seat, either already present or in a confirmed replacement tab: `agent_kind`, `agent_id`; rate-limit identity is `(agent_kind, agent_id)` |
| `recovery_child_ended` | `harness::rebirth` | A non-live child ended because recovery never resumes a row with a parent: `agent_kind`, `agent_id`; rate-limit identity is `(agent_kind, agent_id)` |
| `hook_drain_lag`, `hook_apply_failed`, `hook_ingress_skipped`, `hook_ingress_truncated` | `harness::hook_drain::Drainer::drain` (lag, failed apply), `store::ingress::repair_and_read` (skipped and truncated ingress) | Sampled drain lag with pending ingress bytes, a frame the processor refused, and ingress repairs by byte range ([Hook drain evidence](#hook-drain-evidence)) |
| `workflow_retry`, `workflow_rerun` | `harness::workflow::engine::accounting::transition` | A failed check evaluation (`died`, `asked`, or `invalid`) that earned its one retry, with `run`, `node`, `seq`, `outcome`, and `reason`; an evaluation re-run free because its hosting `advance` died, with `run`, `node`, and `seq` ([workflow engine](./harness/workflow.md#accounting)) |
| `tool_loop_escalated` | `cli::hooks::lifecycle` | A named tool's consecutive identical argument signature reaching the configured attention threshold |

`fetch_delivery_stats` records one completed fetch-worker window: `interval_ms`, `cycles`, `content_deliveries`, `unchanged_deliveries`, and `fold_ms`. Deliveries count at consumer application, including interim deliveries, not at worker send. For non-eldest attachments their sum counts consumer-built projections; hidden deferrals, including equal content, add nothing. Unconsumed mailboxes add nothing, and consumption racing rollover enters the next window. `fold_ms` is the total fold time over the window, not a per-cycle figure: divide it by `cycles` for the per-cycle cost. It covers the fast fold and produce/fold calls, including their input acquisition, but not consumer projection. Doctor classifies these records as expected information. This replaces the unused `fetch_fold_stats` vocabulary with a new tag.

The host-only supervisor no longer records child-renderer signal deaths or child-renderer orphan reaps. Its replacement kinds have new tags rather than repurposing the old shapes (`crates/rimz/src/diag/record.rs::DiagEvent`):

- `sidebar_host_unavailable` is `warn`: `cause` is `start_failed`, `no_answer`, `rejected`, `reply_unreadable`, or `lost`; `reason` is the raw rejection text, the start error/log suffix, or empty for the fixed-message causes. `attached_ms` is present only for a lost accepted attachment, measured before detach cleanup; `retry_ms` is the delay about to be slept. Each failed round emits through the identity-rate-limited sink ([notice and retry](./sidebar/state.md#host-unavailable)).
- `supervisor_pane_gone` is `warn` and carries `pane_id` when the watchdog confirms the pane is gone and supervision ends.
- `supervisor_convergence` carries `target_build` before preflight, not proof that exec succeeded; a refusal follows as `supervisor_preflight_rejected`.

The carry kinds attribute a pane-source fault precisely, and the distinction matters when reading a log:

- `pane_carry_forward` marks an omission carried after a forced direct re-pull, with a captured frame pair. `cli_confirmed: true` means the server answered: the carry is the renderer's own pane or panes in wholly omitted tabs. `cli_confirmed: false` means the direct re-pull failed and the topology cache answered. Process liveness and tab coherence protect omitted panes; the renderer's own pane can carry by identity. A sibling missing from a tab the server lists cannot survive that read.
- `pane_carry_refuted` marks an initial carry the forced re-pull made unnecessary: the panes reappeared, or the server listed their tab without them, refuting their existence even while their roots live (the `rimz agents stop` shape). No frame pair is captured; the log line is the evidence.
- `carry_forward_expired` marks a sibling carry dropping after `PANE_CARRY_TTL` (30s), even if its root still lives. Whole-tab omissions and cache fallbacks share this bound; own-pane carry never expires.
- `hosted_carry_dropped` marks a prior hosted lazy-agent stamp the producer declined to restore. Its `reason` is `probe_reports_absent` or `carry_expired`, where dropping a stale stamp is correct and Doctor marks the record expected, or `start_regressed` or `foreground_kind_mismatch`, which stay investigative evidence of contradictory identity.

[sidebar.md](./sidebar/sidebar.md#honest-reads-across-a-mux-hiccup) describes the guards these records come from.

### Severity

Severity follows the event, and for a few kinds the event's own fields. `DiagEvent::severity` holds the mapping, pinned by test in [`record/tests.rs`](../../crates/rimz/src/diag/record/tests.rs).

| Severity | Events |
| --- | --- |
| `error` | `renderer_panic`, `ghost_session_bind` |
| `warn` | `frame_rejected`, `pane_count_drop`, `pane_carry_forward`, `carry_forward_expired`, `duplicate_pane_id`, `foreign_session_pane`, `row_conflict`, `live_roster_held`, `gate_hold`, `fetch_failure`, `frame_anomaly`, `tool_loop_escalated`, `topology_write_rejected`, `sidebar_host_unavailable`, `supervisor_pane_gone`, `sidebar_orphan_reaped`, `subagent_orphan_reaped`, `subagent_orphan_repair_failed`, `subagent_zone_lookup_failed`, `pane_cache_divergence`, `supervisor_preflight_rejected`, `self_close_rejected`, `provider_startup_exit`, `hook_drain_lag`, `hook_apply_failed`, `hook_ingress_skipped`, `hook_ingress_truncated` |
| `warn` while active, `info` on recovery | `health_alert`, `link_alert`, `tick_budget_breach` (recovery sets `recovered_after_ms`) |
| `info` on cached resolution, `warn` on baked fallback | `model_catalog_refresh_failed`: `cached_catalog` resolved the alias; `baked` used only the compiled-in id |
| depends on a field | `client_reaped`: `warn` unless `settled`. `hosted_carry_dropped`: `warn` for `start_regressed` and `foreground_kind_mismatch`. `renderer_exit`: `warn` for `degraded_gave_up`. Each is `info` otherwise |
| `info` | `frame_shrink_verified`, `resolution_fallback`, `pane_carry_refuted`, `gate_release`, `local_session_bind_rejected`, `group_migration`, `newborn_quarantined`, `mixed_build_writers`, `topology_writer_changed`, `supervisor_convergence`, `subagent_digest_backstopped`, `recovery_seat_refilled`, `recovery_child_ended`, `work_pane_boundary_moved`, `pane_winsize_repaired`, `hidden_views_fitted`, the three width traces, `fetch_delivery_stats` |

Filter on the kind as well as the severity: `mixed_build_writers` and `newborn_quarantined` are `info` yet often explain a `warn` beside them.

## The frame-stream observer

The room plane observes its shared workspace stream once, and each renderer observes its own committed roster, the fused and gated `SidebarSnapshot` sequence it paints. The host's writer records an evidence-rich `frame_anomaly` when a stream misbehaves: a roster that empties and refills, a duplicated card, a phantom row, a value that bounces between two figures, a card whose pane or process is gone. A room-common anomaly names the eldest renderer of that publication in `instance_id`; an own-roster anomaly names its renderer. Rebinding shares the sink's existing rate limit, not a new limit per pane.

The observer reads the stream and emits records; it never changes what the renderer commits or paints. It exists because this class of bug heals itself within seconds and leaves nothing behind. A caught anomaly becomes a detector test ([below](#from-anomaly-to-regression-test)), and a detection that proves reliable becomes a prevention guard, either renderer-side in the [commit gate](../../crates/rimz/src/sidebar_pane/app/gate.rs) or at the source in producer frame validation.

[`sidebar/observe`](../../crates/rimz/src/sidebar/observe.rs) owns the signature (`sig.rs`), the stream detectors (`detect.rs`), and the writer thread (`writer.rs`). The anomaly vocabulary is `AnomalyKind` in the record schema, and the stream windows live in [`timing.rs`](../../crates/rimz/src/sidebar/timing.rs) under the `OBSERVE_*` prefix. The renderer's `app::loop_state` also checks painted geometry for `clicked_row_displaced`, over the `REORDER_HOLD` click window.

### The commit point

Every mutation of a rendered snapshot passes through `observe_commit` in [`app::loop_state`](../../crates/rimz/src/sidebar_pane/app/loop_state.rs), reached by both pulls and event overlays. It extracts only the pane's own view, roster counts and pane membership, active events, and gate/health streaks for own-roster detection. No commit signature is extracted when the plane has no diagnostics writer.

Before local projection the fetch worker extracts one room-common `FrameSig`: sorted row identities, card kinds, pane ids and pids, groups and baseline order, watched values, aggregates, and pulled membership. Every projection shares that signature by `Arc`; the writer runs its common detectors once. Common detection describes the workspace baseline, not each pane's gated or order-held presentation. The worker's fast and produced publications have distinct baselines and therefore distinct signatures. Scoped overlay events feed a deduplicated room observation store as well as each pane's unchanged fusion store.

- The row signature leaves out renderer-local presentation state (unread stamp, selection, scroll), so presentation churn reads as unchanged content.
- The common signature retains un-fused pulled membership, frame stamp, and dashboard aggregates. Own records retain their projected pulled row count and the common pulled stamp alongside their actual committed counts and local streaks.
- Room signatures and own drafts travel over a bounded 64-slot channel. A full channel drops the message and carries its drop count into the next passing anomaly, so neither the worker nor the renderer waits on the observer.

The windowed stream detectors stay off during `OBSERVE_WARMUP` (10s) after the first frame-backed commit, because startup and reload transients are expected, and they stay off while the committed fold has no pane frame. The per-frame checks run from the first commit. The click detector instead arms on a local focus input targeting a painted card and checks after every paint, including input and animation paints. It also seeds in the destination after a landing paint that missed a shared placement, baselined on the requested row using the post-paint scroll-zone top. The same landing judge feeds the peer verdict and this record. An existing local watch for that card is kept, so same-tab input does not double the record. It reads the card's own first-line absolute pane row from `UiState::painted_rows`, not the target window used for anchoring. Any other input or five seconds since the click or anchor application ends the watch; a later focus input replaces it. A displacement emits `from_y`, nullable `to_y`, and `dt_ms` since the click (or since the anchor was applied for a destination landing), then re-baselines; leaving the viewport emits `to_y: null` and disarms. A paint that follows one taken mid wheel glide re-baselines without emitting, because that motion is the user's own scroll.

A destination's landing miss uses `no_room_clamp` when clamped, otherwise `height_above_changed`; the watch then continues at the painted row. On later paints the click detector attributes the first applicable cause: `no_room_clamp`, an order hold ending between paints (`hold_expired`), `banner_toggled`, `selection_changed`, otherwise `height_above_changed`. Banner and selection comparisons use the previous paint, not the original click. The diagnostic subject is `{row_id}:{cause}`, so an expected clamp cannot suppress or fold into a different cause on the same card. Click drafts use the committed snapshot's frame stamp and the same nonblocking `OwnAnomaly` channel and drop carry as roster drafts; render itself reads no wall clock and performs no diagnostic I/O.

### Windowed detectors

The windowed family recognizes the back-and-forth motion a user would describe: rendered, gone, rendered again. Windows measure receiver-clock time, like the event store's TTLs.

| Detector | Window | Fires on | Stays quiet when |
| --- | --- | --- | --- |
| `roster_flap` | 10s | A populated roster empties while own-view still counts working siblings, then refills inside the window | Active `PaneClosed` events cover every vanished row's pane (a genuinely emptied tab is [self-close](./sidebar/sidebar.md#self-close) territory) |
| `row_presence_flap`, `short_lived_row` | 7s | One row disappears and returns inside the window, or a row is born and vanishes inside it (the phantom card, group key recorded) | A `PaneClosed` justifies the absence, the row's group had its idle tail hidden at either edge (ranking churn rotates rows through the cap), or the pane was rebound to a new identity |
| `value_oscillation` | 5s away | A watched per-row value returns to its exact prior figure after differing, however long that figure had been stable: context %, token total, group key, or model | The field's first appearance, since enrichment warm-up goes from `None` to a value, or a return after more than the window away |
| `aggregate_oscillation` | 12s away | A dashboard figure returns to its exact prior value after differing, however long that value had been stable: cockpit or workspace spend year, or any keyed provider-panel value (below). A figure whose provider panel or window bar leaves the frame and returns counts, with `via` reading `<none>` | First appearance, or a return after more than the window away; a figure the snapshot could not supply reads `<none>` with `pulled_via` absent, so an unavailable tally stays distinct from a real `0` |
| `order_flap` | 7s away | Rendered row order inside one group returns to its prior order after differing, however long that order had been stable, with unchanged visible membership | The visible set changed, as in a real re-rank or a cap tail rotation; a row whose index differs between the two orders has a different ranking status on the frame the order returned on than on the frame it left, since ranking follows status (a short turn, an answered ask), though a status change on a row that kept its place does not silence the flip; or a return after more than the window away |
| `status_churn` | 30s | Four or more status transitions on one row inside the window | A single `running → idle → running` turn boundary |

Under heavy mux enumeration churn a published pane frame can briefly drop and re-list one pane. Carry-and-verify covers most of that gap while letting genuine closes through, so a residual row-presence flap stays a diagnostic record.

A windowed record stamps the frame that **caused** the anomaly, not the frame that revealed it. `row_presence_flap` fires when the row returns but carries the frame the row went missing on. The `produced_at_ms` join therefore reaches the producer records for the same episode. Doctor still folds copies from retained multi-renderer logs or a host replacement into one incident.

The `value_oscillation`, `aggregate_oscillation`, and `order_flap` windows bound the time the value was away, and `span_ms` carries that time. A record written before this measure carries the time since the returned-to value first appeared, and that older detector missed a blink on any value that had been stable for longer than the window. An older `value_oscillation` record may also carry `field: status`; status left that detector because a short turn or an answered ask is a legitimate status blink. A single status blink on one row is therefore recorded by no detector: at this layer it cannot be told from a short turn, and `status_churn` catches only repeated motion. An aggregate key missing from a frame is sampled as an absent figure, and its history is dropped once it has been absent for longer than the window.

The provider panel is keyed per login by what it renders. A record's `aggregate` names the key:

| `aggregate` | Names | Value |
| --- | --- | --- |
| `provider_spend` | The login's trailing-year spend | Integer cents |
| `provider_spend_period` | `period`: `headline`, `week`, or `month` | Integer cents |
| `provider_mana` | One window's used % | The percent |
| `provider_mana_field` | The same window, with `field`: `resets_at` or `lifted` | The stored reset timestamp, or `true`/`false` |
| `provider_field` | `field`: `version`, `plan`, `metered`, `remote_control`, `day_budget`, `extra_credits`, `reset_credits`, or `redeem_forecast` | The value as text. A composite is one string of everything the panel renders from it, money in cents: day budget `spend/cap` with `/parked` appended while parked, extra credits `disabled` or `used/remaining/limit` with `-` for a missing figure, reset credits `count@soonest-expiry` |

An unset field is an absent value and reads `<none>`. Two panel values are left unkeyed because they move in normal operation, where a flip is not a fault: `active_sessions` and a window's `observed_at`. A period or a window field shares its identity prefix with the year or used-% key (`provider_spend:<login>:week`), and is a separate `aggregate` tag on the wire: a build from before these keys skips a tag it does not know, where it would read an unknown field on `provider_spend` or `provider_mana` as that older key.

`aggregate_reset` fires on the edge, without a window, when a spend tally drops from a non-zero figure straight to zero, and carries the prior figure and the pulled value. It covers only the trailing-year tallies (cockpit, workspace, and each provider's `provider_spend`), whose figure never legitimately drops to zero in place; a provider mana window rolling to zero is normal, and the shorter spend periods are not checked. A transient zero that returns inside the window also records `aggregate_oscillation`.

`aggregate_backstep` fires on the edge, without a window, when a provider's `version` parses lower than the one the frame before it carried, and records the key, `from`, `to`, and the pulled figure. It catches a version alternating more slowly than the oscillation window, one record per downward step under the sink's identity limit. A version that does not parse as `major.minor[.patch]` is never judged. Nothing else is checked for a backward step. Every spend figure is a trailing window, the year included: entries older than eight days are merged per transcript, day, model, and thread, and each merged entry leaves the year whole when it passes 365 days, so a spend drop short of zero can be ageing on an account with year-old history. A window's used % is lowered by an authoritative reading, its reset time jitters between readings, and credits and the day budget reset by design.

### Per-frame checks

The consistency family checks each committed frame alone. Each check compares fields the view-model contract says must agree.

| Detector | Violation |
| --- | --- |
| `duplicate_pane_rows`, `duplicate_row_id` | Two rows share a pane id or a row id, breaking one pane, one row. This is the duplicated-card bug as a single-frame verdict |
| `status_count_mismatch` | A group's `status_counts` histogram disagrees with a re-tally of its rows. With hidden rows the declared counts may exceed the visible tally, because counts span the cap; with none they match exactly |
| `subagent_top_level_leak`, `subagent_double_render` | A rollup child surfaces as a top-level row, or renders both nested and top-level |
| `frameless_rows` | A fold with no pane frame still carries rows, which the contract makes unreachable because the pane frame admits every card |

### Real-world cross-checks

The writer thread re-verifies the latest roster against the world every `OBSERVE_CROSSCHECK_TTL` (5s). It reads only what the producer already published (the workspace pane frame through the stat-gated cache read) and process liveness, so the producer stays the only external puller ([state.md](./sidebar/state.md#the-room-data-plane)).

| Detector | Fires when |
| --- | --- |
| `row_pane_missing_from_frame`, `cards_exceed_panes` | A roster pane id is absent from the published frame, or the roster outnumbers the frame's panes. Runs only when the roster's fold stamp equals the frame's `produced_at_ms`, since a republish between fold and read is normal skew |
| `dead_pid` | A row's pane root is gone or its live start mismatches the published stamp for `OBSERVE_DEADPID_CONFIRMATIONS` (2) consecutive passes. Agent rows derive the agent CLI's start under that root, as the producer does, falling back to the root's start when derivation is unavailable; process rows compare the root's start. Platforms without process metrics skip it |
| `agent_card_without_process` | An agent row's live pane root authoritatively hosts no process of that kind for `OBSERVE_HOSTLESS_AGENT_CONFIRMATIONS` (2) consecutive passes. Unreadable or branching process trees stay silent, and dead roots are `dead_pid`'s verdict |

### Roles and cost

Every attachment runs inline detectors on its committed stream and sends drafts to the host's one observer writer. That writer also observes the shared workspace stream and runs cross-checks, so the room pays for process and cache reads once. It emits both shared and pane-local anomalies through the same rate-limited sink. New records omit `role`; retained records may still carry it, and `instance_id` names the renderer whose evidence was recorded. Gate, fetch, and health records remain pane-local.

The cost (one O(rows) signature pass per committed fold, a bounded channel, one throttled cross-check pass) is budgeted in [performance.md](./performance.md#everything-else).

### From anomaly to regression test

A `frame_anomaly` record carries enough to rebuild the stream that produced it: the frame stamp and pulled-truth scalars, row and pane identities, edge timestamps, the event summary, and the judging window.

Encode a confirmed anomaly as a test in [`observe/detect/tests.rs`](../../crates/rimz/src/sidebar/observe/detect/tests.rs). The `sig` and `row` builders there reconstruct the minimal committed sequence from the record's evidence (a warm frame, the offending frame, the restoring frame), and the assertion pins the recorded verdict and its evidence values alongside the verdicts that must stay absent; `row_presence_flap_stamps_the_frame_the_row_went_missing_on` has this shape. Build the fixture from the log record rather than the frame capture, because records outlive the eight-pair capture ring.

## Volume and retention

One condition can write several records, and one record can stand for many occurrences. Read counts through these rules.

- **Identity rate limit.** The sink admits one record per identity per 30-second window, where the identity is the kind plus its salient evidence fields ([`identity_key`](../../crates/rimz/src/diag/record.rs)); drain lag uses the kind alone across the room. Suppressed repeats are counted onto the next record of that identity as `suppressed_since_last`, so a per-tick repeat collapses to one periodic line carrying the tally. The limiter belongs to the sink: a successor drainer starts a fresh window.
- **Kind ceiling.** Each kind admits at most 120 records per window. Drops past the ceiling are added to the next admitted record's `suppressed_since_last`.
- **Kinds that skip the identity limit.** `health_alert`, `renderer_panic`, `renderer_exit`, `client_reaped`, `topology_write_rejected`, and the three width traces go through `emit_unlimited`: only the kind ceiling applies, so each occurrence is its own line.
- **The observer adds no rate limit of its own.** The host's writer emits drafts through the sink; `dropped_msgs` separately counts drafts the full channel shed.
- **One observer writer per room.** The host appends shared and pane-local `frame_anomaly` records. Different renderers can contribute evidence to one episode; `gate_hold`, `gate_release`, `fetch_failure`, and `health_alert` remain per renderer.
- **Audit retention.** The room's 30-day/64-MiB audit sweep can remove captures, so copy `audit/diag-frames/` pairs out at the start of an investigation.

## Frame captures

`frame_rejected`, `pane_count_drop`, and `pane_carry_forward` records name their capture in `frames_ref`. Captures live in `audit/diag-frames/`, a private `0700` directory beside the log, bounded by the audit sweep rather than a write-time ring. Each pair is one `frame.<at_ms>.<seq>.<kind>.json` file holding `prior` and `offending`, written as a disposable cache (atomic rename, no fsync).

Frame captures may contain command lines, cwd values, and other pane metadata. They sit behind the same local-filesystem privacy boundary as the rest of the workspace state directory.

## Reading the log

`rimz doctor` prints the latest twelve incidents from the current workspace's log, after dropping records at or before the history watermark. An incident folds records by identity. Cross-sidebar frame anomalies collapse only when session, build, event identity, and produced-frame stamp all match; other records, active and recovery edges alike, share a normalized identity and normally join an incident while they arrive within 60 seconds of its last record. Drain lag instead uses a 15-minute quiet period for both grouping and recovery: records less than 15 minutes apart join one episode, and 15 quiet minutes settle it as recovered with Info impact. A later lag record starts a new investigative warning. Sessions and builds stay distinct.

Each incident keeps the source severity apart from its state (`investigate`, `contained`, `recovered`, `expected`) and impact (`alarm`, `warn`, `info`), plus record and distinct-observer counts, suppression totals, dropped-message counts, occurrence range, build staleness, and evidence references. Only investigative `warn` and `alarm` incidents count against Doctor's health tally. Incomplete evidence stays investigative, while `fetch_delivery_stats` records and benign `hosted_carry_dropped` reasons are expected. A `clicked_row_displaced` with `no_room_clamp` is expected with info impact: a data, configuration, or destination-geometry difference left the viewport no room to preserve the card's row. Selection-driven collapses instead hold the previous card open until they fit. Its other causes are investigative with warn impact. A `model_catalog_refresh_failed` on `cached_catalog` is contained with info impact because the alias resolved; its `baked` rung is investigative with warn impact. The human report gives each investigative incident a table row and folds settled states into one counted line; `--json` carries every field of every incident.

A drain-lag row summarizes the episode's worst sampled lag and peak sampled pending bytes, not its last sample. JSON adds `drain_lag: { worst_lag_ms, peak_pending_bytes }` only to lag incidents. Doctor reads the current clock to settle quiet episodes but writes no recovery record.

Doctor's message probe ([`cli/doctor/messages.rs`](../../crates/rimz/src/cli/doctor/messages.rs)) calls an open record stuck once it carries `attempts` above one, an unconfirmed send, or any `last_error`. A wake the sweep deferred because its receiver has no live pane records that error in its first 30-second window ([messaging](./harness/messaging.md#scheduling-and-wakeups)), so it is a doctor problem row from that first window until the pane returns and the next defer clears the sentence. That is deliberate: an undeliverable message is exactly what the health surface is for, and the row disappears on its own once the receiver is bindable again.

`rimz doctor --clear` writes the watermark `cache/doctor-cleared.json`. Doctor then hides diagnostics, the last incident marker, durable message failures, and multiplexer server-log records at or before its `cleared_at` timestamp. The logs, incident archive, and event log are untouched, so deleting the watermark restores the full retained history.

The log is plain JSONL. A kind census is the fastest orientation on an unfamiliar one (sample output):

```console
$ DIAG="$(rimz paths --json | jq -r .state_dir)"/audit/diag.log.jsonl
$ jq -r '.event.kind' "$DIAG" | sort | uniq -c | sort -rn | head
    311 sidebar_width_settle
    197 link_alert
    190 frame_anomaly
    102 client_reaped
     96 topology_writer_changed
     94 supervisor_convergence
     83 frame_rejected
     69 fetch_failure
     53 sidebar_width_nudge
     52 pane_cache_divergence
```

```sh
jq -r '[(.at_ms|tostring), .severity, .event.kind, (.instance_id // "-")] | join(" ")' "$DIAG"  # episode timeline
jq 'select(.at_ms > 1781070540000 and .at_ms < 1781070550000)' "$DIAG"                          # window slice
jq 'select(.event.kind == "frame_anomaly") | .event.anomaly' "$DIAG"                            # observer evidence
jq 'select(.event.kind == "sidebar_host_unavailable") | .event' "$DIAG"                        # host retry
jq 'select(.event.kind == "renderer_exit") | .event.cause' "$DIAG"                              # exit cause
```

## Investigating an episode

One pass over the log answers an episode's three questions in order: what the user saw, where truth went wrong, and why.

1. **Build the timeline.** Run the timeline one-liner above, or `rimz doctor` for the recent incidents, and cluster records by `at_ms`. An episode reads as a burst across kinds. Copy the matching `audit/diag-frames/` pairs out before the audit sweep reclaims them.
2. **Locate the fault in published truth or the local fold.** Every `frame_anomaly` carries the pulled snapshot's scalars beside the rendered ones. For `row_presence_flap`, read the missing-edge frame stamp and `gap_evidence.pulled_row_present` and `pulled_pane_present`: false membership puts the gap in pulled truth, true membership in the renderer's committed fold. The `instance_id` identifies the renderer whose evidence was recorded; inspect its gate, fetch, and health records too.
3. **Attribute the cause.** Producer records in the same window name it: the carry kinds as described above, `frame_rejected` for held implausible reads, `pane_count_drop` for published shrinks, `gate_hold` for renderer-side holds. The frame stamp (`produced_at_ms`) joins producer records, observer records, and capture filenames across the episode.
4. **Diff the captures.** Each capture holds the last good frame beside the offending one; `jq '{prior: (.prior.tabs | length), offending: (.offending.tabs | length)}'` shows a whole-tab omission at a glance.
5. **Encode the episode** as a detector test ([above](#from-anomaly-to-regression-test)).

Three shapes are worth recognizing.

**A long run of `gate_hold` with rule `agent_demoted_to_process`**, where every matching `gate_release` carries `via_escape_hatch: true`, means the rollup kept presenting the same live agent pane as a bare process with unchanged or missing foreground-command evidence. A real exit whose foreground command changed commits at once. A nearby `hosted_carry_dropped` means the producer's hosted-stamp carry declined, and its `reason` names the source guard; with no such record the carry restored the stamp, and the next step is instrumenting the rollup bind guard. The reject count is evidence only: the gate holds until `ACCEPT_REGRESSION_AFTER` (1s) has passed since the first reject, then releases through the escape hatch.

**A partial read**, where a pane source reports fourteen panes as six by omitting two whole tabs whose processes live. Both the topology cache and native Zellij listings have shown this shape. The carry-forward guard protects whole omitted tabs even after a successful server read, so a healthy log shows `pane_carry_forward` under a steady roster (`cli_confirmed: true` when the server answered, `false` on a cache fallback). It does not carry a missing sibling from a tab the server still lists. When the whole-tab guard misses, the log reads:

| Records | Reading |
| --- | --- |
| `pane_count_drop` with eight removed panes | The shrink published; the capture pair preserves both frames |
| five `row_presence_flap`, gone to back in 2.26s, no `PaneClosed` events, `pulled_rows` back at full count | Every instance painted the flap; pulled truth had already recovered, so the rendered gap was the published partial frame propagating |

**A tab or card that outlives its pane**, where a closed pane keeps a sidebar-only tab open, or its card listed, for up to 30 seconds. The log reads `pane_carry_forward` at the close, `carry_forward_expired` about `PANE_CARRY_TTL` (30s) later, then `renderer_exit` with cause `self_close_empty_tab`. No `self_close_rejected` appears, because the carried sibling keeps the attachment from asking and the request never reaches the supervisor, so start from the carry records rather than looking for a rejection. Carry holds a closed pane only while its root process lives, so measure that process's lifetime alongside the symptom (`ps -o pid,etimes,cmd -p <root_pid>` on the pane's recorded root pid): a root that ignores the hangup outlives its pane, and the late close tracks the shorter of its lifetime and the TTL.

## Inspecting live card state

Card-content questions (a wrong gauge, a missing cost, a card resting in the wrong shape) are answered from the same read path the renderer runs, before any raw file is opened. Rendered-frame anomalies (flicker, duplicate rows, missing tabs) take the [episode workflow](#investigating-an-episode) instead.

`rimz workspace resolve <path>` prints the `workspace_id` for any project path. Every worktree of a repository resolves to the repository's own workspace, so a `rimz-worktrees/<branch>` checkout maps to the main repository's directories. State lives under `~/.rimz/ws/<workspace-dir>/` and runtime files under `$XDG_RUNTIME_DIR/rimz/ws/<workspace-dir>/`, where the directory name is `<basename>-<hex>` rather than the id; `rimz paths` prints both for the current project, a layout owned by [`disk/paths.rs`](../../crates/rimz/src/disk/paths.rs).

`rimz sidebar snapshot --json --no-produce` prints the fused `SidebarSnapshot` a node renders: the event-fresh rollup folded over the published pane frame plus the per-session sidecars ([state.md](./sidebar/state.md)). Run it inside the workspace, or pass `--workspace-id` from outside. `--no-produce` keeps the read passive, with no mux or git forks, so inspection never perturbs the room; without it the command may pay one producing refresh.

`rimz sidebar frame` prints the rendered frame through the same passive read when a producer frame exists, and falls back to one producing refresh otherwise. It draws at the size of the room's live sidebar pane, which each renderer stamps into its heartbeat; `--width` and `--height` override it, and without a live sidebar the width falls back to the configured policy over the calling terminal. ANSI color is stripped when stdout is piped. `--expand` expands every card and reveals every capped worktree row, so large fleets do not clip.

```sh
rimz sidebar snapshot --json --no-produce | jq '
  .agents[] | select(.worktree_branch == "truecolor") | {
    agent_id, kind, status, context_pct, total_tokens, compaction_count,
    tokens: .context.tokens, cost: .context.cost.total_cost_usd,
    observed_at: .context.observed_at }'
```

The split inside that one object is the provenance map. Bare row fields (`status`, `context_pct`, `total_tokens`, `compaction_count`) are rollup truth derived from hooks and transcript tails, while everything under `context` is the latest statusline or app-server sidecar, stamped `observed_at`. A figure wrong in only one half names the half to debug.

Raw sidecars confirm what a producer actually wrote. Filenames are digests because session ids are free strings, so scan by record content:

```sh
cd "$(rimz paths --json | jq -r .runtime_dir)"
jq -r '[.kind, .agent_id, .context.session_name] | @tsv' agent_context/ctx.*.json   # find a session's record
jq . agent_context/ctx.<digest>.json                                                # the full record
```

The store's own view without the sidecar fold is the published checkpoint plus log tail ([store.md](./store.md#the-read-path)); comparing it against the snapshot attributes a wrong figure to the rollup, the sidecar, or the fold. The renderer-side derivations a card dispute usually hinges on live on the view model: the gauge-source preference is [`AgentCard::context_gauge_percent`](../../crates/rimz/src/store/snapshot/row.rs), and the card-shape predicates sit in [`agent_card/mod.rs`](../../crates/rimz/src/sidebar_pane/render/sections/agent_card/mod.rs).

## Off-box error reporting

Off-box error reporting sends RimZ's warnings, errors, and panics to a Sentry project, so a contributor can watch a fleet's health without tailing every box. It is best-effort enrichment and never holds a correctness path.

The code compiles only under the dev-only `sentry` cargo feature. A shipped binary omits it and ignores any `[sentry]` config. Without the feature, [`observability`](../../crates/rimz/src/observability.rs) is a no-op with the same surface, so `main.rs` and the CLI dispatch are identical in both builds; with it, [`observability/reporting.rs`](../../crates/rimz/src/observability/reporting.rs) is the live implementation.

An opted-in build reports the `warn!` and `error!` events RimZ raises, the panics it hits, and the agent conditions it observes (rate limits, spend limits, provider overload, and other turn-ending API failures) at warning level.

### Opting in

`cargo xtask install-dev` is the contributor shortcut. After installing the Cargo tools in [`scripts/install-dev-tools.sh`](../../scripts/install-dev-tools.sh), it installs the optimized `profiling` host profile with the feature on, then runs `sentry-cli debug-files upload` on the binary. A failed upload is retried up to three times, one second apart; when `sentry-cli` cannot start at all it warns once without retrying. Either way the install succeeds.

Reporting turns on when a DSN resolves from `RIMZ_SENTRY_DSN` or the per-machine `[sentry] dsn`; the env value wins, and an empty value counts as unset. The DSN lives only in per-machine config, never in the committed `.rimz/config.toml`, so a clone or pull cannot redirect a contributor's telemetry, and it stays off the [project trust surface](./harness/trust.md#the-executable-surface).

`RIMZ_SENTRY_ENVIRONMENT` (or `[sentry] environment`) tags the deployment. Unset, it follows the build profile: a `release` build reports as `production`, and dev, profiling, and CI builds as `development`, keeping contributor noise off the production dashboard. The config shape is in [configuration.md](../guide/configuration.md#off-box-error-reporting) and the user-facing data boundary in [security.md](../guide/security.md#off-box-error-reporting).

With no DSN, no client is created and RimZ makes no network calls. A malformed DSN yields `Reporting::InvalidDsn`, logged with the fix once the subscriber is live and otherwise inert, so a telemetry typo never degrades or blocks a command.

### One init point covers every process

`main` creates the Sentry client once, before the tracing subscriber, and holds the guard for the whole process. The guard flushes pending events on drop, which covers short-lived hook subprocesses. Every RimZ subcommand runs through that `main`, including the `hooks feed` subprocess where agent conditions are observed and the `sidebar serve` loop. The wasm presence plugin is a separate binary with no HTTP stack and reports nothing.

A live workspace pin (`RIMZ_WORKSPACE_ID`) becomes a `workspace` scope tag, so one machine-wide DSN still filters per repository. Once the command parses, `set_command_scope` adds `command` and `build` tags (the same build id the diagnostics log stamps) and a structured `rimz` context with the command, the build, and, when the process serves exactly one, the agent kind and session.

The client reports under the `rimz@<build id>` release. One identity therefore tracks regressions across builds, `resolve --in-next-release` reopens on a genuinely new build, and uploaded debug files stay keyed. The profiling profile embeds DWARF line tables and frame pointers in one self-contained binary, so the uploaded file matches the GNU build-id that release carries.

### The tracing bridge is the capture path

The Sentry layer joins the tracing subscriber beside the stderr formatter and turns each `warn!` and `error!` into a Sentry event at the same level. The sidebar health target `rimz::sidebar::health` stays local, because the durable `health_alert` and `renderer_exit` records already carry that episode. With no DSN the layer is omitted and the subscriber is unchanged.

Breadcrumbs work by allowlist. Each deliberate seed is an `info!` on the `rimz::trail` target and rides along on the next event, so a warning arrives with the trail that led to it. An unmarked `info!` is ignored, so a stray field (a socket path, a cwd) never leaves the box as breadcrumb data. The layer carries its own `INFO` filter, which keeps the global max-level hint at `INFO`: `debug!` and `trace!` are never constructed, and the `sidebar serve` hot loop never feeds the trail. Callsites attach a searchable `tags.operation` and pass the error as `&dyn Error`, so an event names the failed operation and carries the error's exception and stacktrace.

Agent-generated conditions ride the same path. When `merge_turn_error_marker` in [`transcript.rs`](../../crates/rimz/src/cli/hooks/lifecycle/transcript.rs) reports that a fresh turn-error marker changed state, the hook lifecycle emits one `warn!` on `rimz::agent::turn_error` with the agent kind and the [`TurnErrorClass`](../../crates/rimz/src/agents/context.rs). Gating on the transition keeps it to one event per condition rather than one per poll.

The host's render panic hook records `renderer_panic` locally and reaches Sentry through its panic integration. A lost attachment is not proof of a host-process crash: its supervisor writes the local `sidebar_host_unavailable` record with cause `lost`, shows the retry notice, and sends no separate Sentry crash event ([`supervise.rs`](../../crates/rimz/src/sidebar_pane/supervise.rs)).

`before_send` shapes every bridge event from its tracing target before it leaves the box:

- It tags `rimz::agent::turn_error` events `fault=agent` and every other event `fault=rimz`, so triage separates observed provider conditions from RimZ bugs.
- It pins a stable fingerprint (namespace, target, `operation`, and the static message), so an unsymbolicated release stack cannot split one callsite across issues and one resolve sticks.
- It allows five events per minute per fingerprint, so a `warn!` on a per-frame sidebar path cannot flood the project.

A panic or manual capture carries no target, so it keeps Sentry's default grouping and is never throttled.

### What stays off the wire

Sentry's default personal-data collection is off, and `before_send` strips the hostname.

Events carry RimZ error text and a stacktrace, the file paths that appear in those errors, the `rimz@<build>` release, the `command` and `build` tags, the `fault` class, the agent kind, the session id and turn-error class, the failed `operation`, and, for a failed account-usage probe, the `provider` tag and the request's host authority (never its path or query). The `workspace` tag scopes them to a repository, and the `rimz::trail` breadcrumbs trail an event with the steps before it.

Hook payloads, prompts, and transcripts are never forwarded. The transport is the same small rustls-backed `ureq` client RimZ uses for pricing, and it swallows network failures so they never surface on a RimZ path.
