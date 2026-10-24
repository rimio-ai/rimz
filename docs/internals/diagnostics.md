# Diagnostics

RimZ records what goes wrong as typed evidence, so a transient fault leaves something to debug after it heals. A roster that empties and refills over two seconds, a spend figure that blinks to zero, or a card that outlives its pane is invisible an hour later unless something wrote it down at the moment it happened.

One boundary makes this safe: **correctness reads the store, CAS rules, and caches, and no correctness path reads a diagnostic record back.** Diagnostics are evidence for a human, and every write is best-effort.

Evidence has two destinations. The default is a set of durable logs on the box, always on, which most of this page describes. The second is [off-box error reporting](#off-box-error-reporting) to a Sentry project, compiled only into dev builds and dormant until a contributor opts in.

## Rules a change keeps

- A failed append logs at debug and returns. The calling path continues unchanged, and nothing here gates product behaviour.
- Only the sink touches disk. Pure projection layers return diagnostics as data, impure callers append them through [`DiagSink`](../../crates/rimz/src/diag.rs), and store reducers and the renderer gate carry no disk-write API.
- A new key or record shape on any diagnostic JSONL surface gets a new serde tag, never a distinguishing field on an existing variant. Builds of different ages share one home, and an older build skips a tag it cannot decode but ignores unknown fields, so it would read the new shape as the old one.
- A new `DiagEvent` kind gets a row in the [event taxonomy](#event-taxonomy) and in the [severity table](#severity). Both tables enumerate every kind.
- `crates/rimz/src/diag.rs` alone names the main log file and the frame-capture directory in code. `cargo xtask invariants` rejects those two literals in every other tracked file outside `docs/`, `xtask/`, and Markdown, so the primary surface stays enumerable from one file.

## Where diagnostics land

Diagnostic logs live in persistent state because an investigation starts after the pane, mux session, or machine has gone away. Workspace logs land under `~/.rimz/ws/<workspace-dir>/`, which survives reboot like the store. Runtime files under `$XDG_RUNTIME_DIR` die with the session.

| Surface | Location | Records | Owner |
| --- | --- | --- | --- |
| `audit/diag.log.jsonl` | workspace state dir | typed anomaly records, rate-limited | [this page](#the-record-envelope) |
| `audit/diag-frames/` | workspace state dir, `0700` | prior and offending pane-frame pairs | [Frame captures](#frame-captures) |
| `audit/focus.log.jsonl` | workspace state dir | focus publication edges, selection decisions, click delivery, cross-tab jump progress and push handshakes | [The focus trace](#the-focus-trace) |
| `audit/notify.log.jsonl` | workspace state dir | notification emits, bell decisions, unread transitions | [notifications.md](./sidebar/notifications.md#the-trace-log) |
| `audit/plugin-presence.log.jsonl` | workspace state dir | Zellij presence-plugin keepalive samples | [Zellij presence-plugin telemetry](#zellij-presence-plugin-telemetry) |
| `audit/binding.log.jsonl` | workspace state dir | pane-binding decisions | [sidebar.md](./sidebar/sidebar.md) |
| `audit/hibernate.log.jsonl` | workspace state dir | `wake` latency, reclaimed bytes and thaw errors, `wake_refault` samples, and `wake_lock_timeout` recovery | [hibernate.md](./harness/hibernate.md#wake-diagnostics) |
| `focus-repairs.log.jsonl` | machine `~/.rimz/logs/`, beside the assist history | automatic focus-repair evidence and outcomes | [state.md](./sidebar/state.md) |
| `hook-feed.log.jsonl` | machine `~/.rimz/logs/` | returned hook-feed errors, including errors before workspace resolution: `at`, `source`, nullable `event`, and the full `error` chain | [`diag/hook_feed.rs`](../../crates/rimz/src/diag/hook_feed.rs) |
| `lsp.log.jsonl` | machine `~/.rimz/logs/` | shared-server events: refusals, queue timeouts, kills, evictions, recycles, crashes, retirements, and unreadable registry rows | [lsp.md](./lsp.md#visibility) |
| `lanes/topology-writer-conflict.json` | workspace runtime dir | latest Zellij topology writer conflict | [multiplexers.md](./multiplexers.md#the-zellij-presence-plugin) |

Every JSONL surface appends through [`disk::rotating`](../../crates/rimz/src/disk/rotating.rs), which owns the append, the decoded visit, and the generation path. A log rotates to one kept generation (`<name>.1.jsonl`). The workspace logs rotate at the shared 1 MiB cap (`disk::retention::ROTATING_LOG_MAX_BYTES`), and the three machine logs at 4 MiB. A reader visits the retained generation, then the active file, and skips missing files and malformed lines.

`DiagSink` carries the room's identity (workspace, session name, and the sidebar instance when a renderer writes) into the main log, its frame captures, the focus trace, and the notification trace. A disabled sink keeps the same methods and does nothing, so an emitting callsite needs no `#[cfg]` or branch. The other surfaces belong to the subsystems that write them and share only the rotating helper. The rest of this page up to [the focus trace](#the-focus-trace) is the main log, `audit/diag.log.jsonl`.

## The record envelope

Every line in `audit/diag.log.jsonl` is one `DiagEnvelope` from [`record.rs`](../../crates/rimz/src/diag/record.rs):

| Field | Meaning |
| --- | --- |
| `v` | schema version, `rimz.diag.v1`. Readers drop other versions |
| `build` | build id of the writing executable |
| `workspace_id`, `session_name` | the room the writer served |
| `instance_id` | sidebar instance id, when a renderer wrote it |
| `at_ms` | Unix milliseconds |
| `severity` | `info`, `warn`, or `error`, derived from the event ([Severity](#severity)) |
| `suppressed_since_last` | records the rate limit swallowed since the last one written, omitted when zero ([Volume and retention](#volume-and-retention)) |
| `event` | the tagged event. `event.kind` names it ([Event taxonomy](#event-taxonomy)) |

The `build` id is the first six bytes of the executable's linker build identity (`.note.gnu.build-id` on ELF, `LC_UUID` on Mach-O), or of a SHA-256 hash of the image when the identity is unavailable ([`build_id.rs`](../../crates/rimz/src/build_id.rs)). Every published pane frame carries the same stamp. Records written by an old and a new build during an upgrade therefore stay distinguishable in place. `rimz doctor` warns when fresh heartbeats show more than one build writing.

Records are anomaly-first: routine fetch ticks, successful paints, and stable cache hits write nothing individually. Three families are deliberate exceptions. The fetch worker writes one aggregate `fetch_reduction_stats` record per window of at least a minute. The sidebar width traces record each accepted or rejected keypress, controller step, and terminal outcome. Hidden-view geometry repair writes one aggregate per finished sweep or jump fit, never one per window.

## Event taxonomy

The emitter is the triage pointer. Producer kinds describe pane-source truth, renderer kinds one node's holds and refreshes, supervisor kinds process lifecycle, projection kinds binding and grouping, and `frame_anomaly` the rendered symptom as the [observer](#the-frame-stream-observer) judged it. `DiagEvent` in [`record.rs`](../../crates/rimz/src/diag/record.rs) is the schema, and `DiagEvent::identity_key` is the rate-limit identity named in some rows.

| Event | Emitter | Evidence |
| --- | --- | --- |
| `frame_rejected`, `frame_shrink_verified`, `pane_count_drop`, `pane_carry_forward`, `pane_carry_refuted`, `carry_forward_expired`, `hosted_carry_dropped`, `foreign_session_pane` | `sidebar::produce::panes` | Ok-but-empty frames and missing-own-pane reads, verified shrinks, pane drops with affected views, carried and refuted panes, carry expiry, hosted-agent carry declines, foreign-session leaks ([carry kinds](#carry-kinds)) |
| `resolution_fallback` | `sidebar::produce` | A failed pane-resolution read falling back to the rollup, with its reason, when the session is live on the probed backend or its liveness is unknown; absent or exited sessions fall back quietly |
| `duplicate_pane_id` | `sidebar::frame`, `store::snapshot::view` projection | A pane id listed twice |
| `mixed_build_writers` | none | A prior frame stamped by a different build. No current code emits it. Retained logs decode it |
| `gate_hold`, `gate_release`, `fetch_failure`, `health_alert`, `link_alert`, `live_roster_held` | `sidebar_pane::app` | Renderer-side holds and releases, failed fetches, degraded refresh episodes, remote-link degraded and recovered episodes, and a recovery-roster removal withheld because the mux did not list the session, with the agents kept |
| `fetch_reduction_stats` | `sidebar_pane::app::fetch` | One fetch-worker window of at least 60 seconds ([fetch windows](#fetch-reduction-windows)) |
| `group_migration` | `sidebar_pane::app::state` | A pane whose group changed between committed snapshots, with cwd before and after |
| `renderer_panic` | `sidebar_pane::app` | Panics that would otherwise vanish with the pane |
| `sidebar_width_intent`, `sidebar_width_nudge`, `sidebar_width_settle` | `sidebar_pane::app::width_control` | Intent verdicts, controller nudges, learned feedback, and terminal outcomes for `a`/`d` width control |
| `sidebar_host_unavailable`, `supervisor_pane_gone`, `supervisor_convergence`, `supervisor_preflight_rejected`, `self_close_rejected`, `renderer_exit` | `sidebar_pane::supervise` | A failed host round, a watchdog-confirmed missing pane, a stable target about to be preflighted or a preflight that refused one, a declined self-close with the sibling count, and a confirmed self-close ([supervisor kinds](#supervisor-kinds)) |
| `work_pane_boundary_moved` | `sidebar::presence` (Zellij), `sidebar::presence::tmux` | Before and after horizontal geometry for a stable work-pane set whose view and sidebar widths did not change, drift that would otherwise leave no trace |
| `topology_writer_changed`, `topology_write_rejected` | `sidebar::presence` | Zellij topology writer generation flips and rejected stale writers, with plugin id, loaded-at generation, accepted writer, and reject count |
| `tick_budget_breach` | `sidebar::meter` | Sustained over-budget producer ticks ([tick budget breaches](#tick-budget-breaches)) |
| `frame_anomaly` | the host's `sidebar::observe` writer thread | Detector verdicts on the rendered stream, each with its detector key, evidence, and frame stamp |
| `row_conflict`, `newborn_quarantined`, `local_session_bind_rejected`, `ghost_session_bind` | `store::snapshot::view` | Duplicate agent identity suppression, a newborn known-command pane held until its cwd resolves, a contained evidence-free or stale local-session bind, and an old exact session stamp contradicted by a newer durable launch |
| `pane_cache_divergence`, `sidebar_orphan_reaped` | `reload` (`rimz reload`, `rimz sidebar repair`) | A fresh pane cache omitted a sidebar process that authoritative mux truth proved alive, and a sidebar reaped after two authoritative omissions |
| `presence_roster_stale` | `reload` (`rimz sidebar repair`) | The plugin published terminal ids already absent from the preceding live pane list: session, normalized `ghost_panes`, cache `produced_at_ms`, live `observed_at_ms`, and optional writer `plugin_id` and `loaded_at_ms`. Identity is the session and writer generation. Doctor warns that the roster is frozen and that a reattach or restart reloads it. Structural repair continues independently |
| `subagent_digest_backstopped`, `subagent_orphan_reaped`, `subagent_orphan_repair_failed` | hidden helpers started by `harness::orphan_sweep` | A fleet digest the normal wrapper path missed, queued from durable run records (identity: the parent), a child closed after its parent launch stayed ended or absent past `ORPHAN_GRACE` (10 minutes), or the repair error when that failed ([subagents.md](./harness/subagents.md#the-orphan-sweep)) |
| `subagent_zone_lookup_failed` | `cli::supervised::pane` | Placement fell back to a companion tab: `lookup` (`projection`, `caller`, `pane_listing`, or `companion_stack`), durable `run_id`, and `error`. `companion_stack` marks a stack append that failed before its spawn. A full stack moves to the next anchor without a record, and an ambiguous spawn error fails the launch instead of falling back. Identity is `(run_id, lookup)`. The launch receipt is unchanged |
| `subagent_pane_bind_timeout` | `cli::supervised::pane` | A launch or resume's 3 s bind wait timed out: `run_id`, `launch_id`, `agent_kind`, `caller` (`launch` or `resume`), `open_ms`, `waited_ms`, `polls`, and `last_read` (an `error` object, or launch-matching rows with `kind`, nullable `pane`, and nullable `ended_at`). An empty list means no matching row. Identity is `run_id` |
| `client_reaped` | `cli::room::attach_exec` | Stale mux clients killed during attach, with killed pids, client counts before and after, and whether the reap settled or timed out |
| `client_reattached` | `cli::room::attach_exec` | A re-attach attempt on Zellij before 0.46, after a client exited nonzero within 2 s while the room was still live: `attempt` (1 or 2), `exit_code`, `elapsed_ms` from spawn to exit, and `zellij_version`. Liveness is read again after the diagnostic write, and a room that ended in that interval cancels the attempt. An attempt does not prove recovery |
| `provider_startup_exit` | `cli::agents_cmd::exec` | A non-successful provider exit within 60 s of spawn that the wrapper did not cause (no wrapper stop, parent end, or signal to the wrapper), for launch, pick, resume, or fork: `agent_kind`, optional `agent_name`, `action`, exactly one of `exit_code` or `signal`, `startup_ms`, and `relaunches`. Identity is `(agent_kind, agent_name)` |
| `scoped_bind_restored` | `cli::agents_cmd::exec`, `harness::headless` | A protected-entry replacement batch bound read-only again in a running scoped view: `agent_kind`, optional `agent_name`, and `paths`. Identity is `(agent_kind, agent_name, paths)` |
| `scoped_bind_restore_failed` | `cli::agents_cmd::exec`, `harness::headless` | A failed protected-bind repair batch: the same identity and paths, plus `error` (view lookup, helper spawn or timeout, or per-entry errno). The agent keeps running |
| `model_catalog_refresh_failed` | `cli::agents_cmd::exec` | A failed model-catalog refresh, including an unusable fetched catalog: `agent_kind`, account `login`, requested `alias`, fallback `rung` (`cached_catalog` or `baked`), and the error `reason`. Identity is `(agent_kind, login)`. Written before the launch's compilation outcome is read. A fresh-cache or cooldown resolution writes nothing |
| `pane_winsize_repaired` | `cli::agents_cmd::exec` | An unsized pane tty repaired before provider startup: `pane`, `rows`, and `cols`. Identity is the pane |
| `sidebar_view_repaired` | `sidebar` (`reconcile_session`) | Landed structural repair effects for one native view: `view`, normalized `closed` pane ids in close order, and the optional verified `added` pane id. Effects before a later transaction failure are kept. Identity includes the view and both sets of pane ids |
| `hidden_views_fitted` | `sidebar_pane::app::hidden_views` | Host-owned tmux geometry repair: `trigger` (`sweep` or `jump`), `target_cols`, `target_rows`, `fitted`, `failed`, and `skipped` (the mutation guard declined). The envelope names the session. One aggregate per batched `fit_views` call: a sweep fits every lagging view in one call, a jump fits its one view. A call that fails as a whole counts every listed view as `failed` (`Summary::from_fit`). An empty aggregate writes nothing |
| `recovery_seat_refilled` | `harness::rebirth` | A parked session ended because a live replacement holds its team seat, either already present or in a confirmed replacement tab: `agent_kind`, `agent_id`. Identity is `(agent_kind, agent_id)` |
| `recovery_child_ended` | `harness::rebirth` | A non-live child ended because recovery never resumes a row with a parent: `agent_kind`, `agent_id`. Identity is `(agent_kind, agent_id)` |
| `hook_drain_lag`, `hook_apply_failed`, `hook_ingress_skipped`, `hook_ingress_truncated` | `harness::hook_drain::Drainer::drain` (lag, failed apply), `store::ingress::repair_and_read` (skipped and truncated ingress) | Sampled drain lag with pending ingress bytes, a frame the processor refused, and ingress repairs by byte range ([Hook drain evidence](#hook-drain-evidence)) |
| `workflow_retry`, `workflow_rerun` | `harness::workflow::engine::accounting::transition` | A failed check evaluation (`died`, `asked`, or `invalid`) that earned its one retry, with `run`, `node`, `seq`, `outcome`, and `reason`, and an evaluation re-run free because its hosting `advance` died, with `run`, `node`, and `seq` ([workflow engine](./harness/workflow.md#accounting)) |
| `tool_loop_escalated` | `cli::hooks::lifecycle` | A named tool's consecutive identical argument signature reaching the configured attention threshold |

### Carry kinds

The carry kinds attribute a pane-source fault precisely, and the distinction matters when reading a log. [sidebar.md](./sidebar/sidebar.md#honest-reads-across-a-mux-hiccup) describes the guards they come from.

- `pane_carry_forward` marks an omission carried after a forced direct re-pull, with a captured frame pair. `cli_confirmed: true` means the server answered: the carry is the renderer's own pane or panes in wholly omitted tabs. `cli_confirmed: false` means the direct re-pull failed and the topology cache answered. Process liveness and tab coherence protect omitted panes, and the renderer's own pane can carry by identity. A sibling missing from a tab the server lists cannot survive that read.
- `pane_carry_refuted` marks an initial carry the forced re-pull made unnecessary: the panes reappeared, or the server listed their tab without them, refuting their existence even while their roots live (the `rimz agents stop` shape). No frame pair is captured, so the log line is the evidence.
- `carry_forward_expired` marks a sibling carry dropping after `PANE_CARRY_TTL` (30 s), even if its root still lives. Whole-tab omissions and cache fallbacks share this bound. Own-pane carry never expires.
- `hosted_carry_dropped` marks a prior hosted lazy-agent stamp the producer declined to restore. Its `reason` is `probe_reports_absent` or `carry_expired` when dropping a stale stamp is correct, or `start_regressed` or `foreground_kind_mismatch`, which are evidence of contradictory identity.

### Supervisor kinds

The supervisor kinds come from `sidebar_pane::supervise`, which holds one tab's sidebar pane and attaches it to the room host ([state.md](./sidebar/state.md#host-unavailable)).

- `sidebar_host_unavailable` records one failed host round. `cause` is `start_failed`, `no_answer`, `rejected`, `reply_unreadable`, or `lost`. `reason` is the raw rejection text or the start error and log suffix, and empty for the fixed-message causes. `attached_ms` is present only for a lost accepted attachment, measured before detach cleanup. `retry_ms` is the delay about to be slept.
- `supervisor_pane_gone` carries `pane_id` when the watchdog confirms the pane is gone and supervision ends.
- `supervisor_convergence` carries `target_build` before preflight. It is not proof that exec succeeded, and a refusal follows as `supervisor_preflight_rejected` with its reason.
- `renderer_exit` carries `cause: self_close_empty_tab` for a confirmed self-close. Retained logs also hold `degraded_gave_up`, a cause no current code writes.

### Tick budget breaches

`tick_budget_breach` records sustained over-budget producer ticks: last and worst wall time, mux wait, fold bytes, spawns, the declared budgets, streak length, episode `since_ms`, and, on the recovery record, `recovered_after_ms` ([performance.md](./performance.md#the-tick-budget)). A cache-refresh record also carries `spawns_by_step`, the per-step spawn counts of the episode's worst-spawn pass, summing to its `spawns`. Zero steps are omitted, and a `fetch` record has no such field. Step names, in pass order:

`codex_daemon_reap`, `accounts`, `spending`, `rate_limits`, `sessions`, `account_usage`, `auto_continue`, `idle_compact`, `idle_stop`, `hibernate`, `cache_keepalive`, `keep_warm`, `auto_redeem`, `budget`, `run_timeout`, `orphan_sweep`, `auto_gc`, `diff_stats`, `cohort_spend`, `pipeline`, `pr_states`, `trunk`, and `other` for the rest of the metered pass (host-input fold, login resolution, seat policy load).

### Fetch reduction windows

`fetch_reduction_stats` carries `interval_ms`, `cycles`, `content_reductions`, `unchanged_reductions`, and `fold_ms` for one fetch-worker window of at least 60 seconds. `Views.reductions` counts full-content or clock-only reductions of successful updates by shared views and Scratch panes, interims included. Publications, completions, failures, hidden deferrals, and already-current co-viewers add nothing, and reductions after rollover enter the next window. `fold_ms` totals fast-fold and produce/fold time, input acquisition included and consumer projection excluded. Divide it by `cycles` for the per-cycle cost.

### Hook drain evidence

The four hook kinds record ingress tail truncation (`hook_ingress_truncated`), corrupt records (`hook_ingress_skipped`), failed application (`hook_apply_failed`), and delayed application (`hook_drain_lag`). Each drainer owns one sink, built from the workspace record's session name. An unreadable record disables diagnostics without blocking the drain. Repair and failure identities use byte offsets or ingress event ids, so distinct failures keep distinct records. The repair itself is in [store.md](./store.md#hook-ingress).

`crates/rimz/src/harness/hook_drain.rs::Drainer::drain` writes `hook_drain_lag` immediately before applying a frame whose apply start trails its ingress timestamp by more than 10 seconds. The record carries `ingress` (the frame's event id), `hook_event` (its optional event name), `lag_ms`, and `pending_bytes` (current ingress log length minus the applied cursor, this frame included). Frames at or below the threshold write nothing. The lag identity is the kind alone, so the sink admits one sample per 30 seconds per drainer and counts suppressed repeats onto the next admitted sample. Worst lag and peak pending bytes are therefore sampled maxima, not exact episode peaks.

### Severity

Severity follows the event, and for a few kinds the event's own fields. `DiagEvent::severity` holds the mapping, pinned by test in [`record/tests.rs`](../../crates/rimz/src/diag/record/tests.rs). The doctor state is how [`rimz doctor`](#reading-the-log) classifies an incident of that kind (`crates/rimz/src/cli/doctor/runtime.rs::classify_diagnostic`). The two are independent: `tool_loop_escalated` is a `warn` that Doctor expects, and `supervisor_convergence` is an `info` it asks you to investigate.

| Severity | Doctor state | Events |
| --- | --- | --- |
| `error` | investigate | `renderer_panic`, `ghost_session_bind`, `scoped_bind_restore_failed` |
| `warn` | investigate | `hook_drain_lag`, `hook_apply_failed`, `hook_ingress_skipped`, `pane_count_drop`, `carry_forward_expired`, `fetch_failure`, `row_conflict`, `duplicate_pane_id`, `foreign_session_pane`, `sidebar_host_unavailable`, `supervisor_pane_gone`, `sidebar_orphan_reaped`, `subagent_orphan_reaped`, `subagent_orphan_repair_failed`, `subagent_zone_lookup_failed`, `subagent_pane_bind_timeout`, `provider_startup_exit`, `pane_cache_divergence`, `presence_roster_stale`, `supervisor_preflight_rejected`, `self_close_rejected`, `client_reattached`, `frame_anomaly` |
| `warn` | contained | `hook_ingress_truncated`, `frame_rejected`, `pane_carry_forward`, `gate_hold`, `topology_write_rejected`, `live_roster_held` |
| `warn` | expected | `tool_loop_escalated`. Also a `pane_count_drop` that removed one view completely, with no managed panes and no mass shrink, and a `frame_anomaly` for `clicked_row_displaced` with cause `no_room_clamp` |
| `info` | investigate | `resolution_fallback`, `mixed_build_writers`, `supervisor_convergence` |
| `info` | contained | `workflow_retry`, `workflow_rerun`, `newborn_quarantined`, `local_session_bind_rejected`, `subagent_digest_backstopped` |
| `info` | recovered | `frame_shrink_verified`, `pane_carry_refuted`, `gate_release`, `topology_writer_changed`, `recovery_seat_refilled`, `recovery_child_ended`, `pane_winsize_repaired`, `sidebar_view_repaired`, `scoped_bind_restored` |
| `info` | expected | `group_migration`, `work_pane_boundary_moved`, `hidden_views_fitted`, `fetch_reduction_stats`, `sidebar_width_intent`, `sidebar_width_nudge`, `sidebar_width_settle` |
| `warn` while active, `info` with `recovered_after_ms` | investigate, then recovered | `health_alert`, `link_alert`, `tick_budget_breach` |
| `warn` unless `settled`, else `info` | investigate, or contained when settled | `client_reaped` |
| `warn` for `start_regressed` and `foreground_kind_mismatch`, `info` for `probe_reports_absent` and `carry_expired` | investigate, or expected | `hosted_carry_dropped` |
| `warn` for rung `baked`, `info` for `cached_catalog` | investigate, or contained | `model_catalog_refresh_failed`: `cached_catalog` resolved the alias, `baked` used only the compiled-in id |
| `warn` for `degraded_gave_up`, `info` for `self_close_empty_tab` | investigate, or expected | `renderer_exit` |

Filter on the kind as well as the severity: `mixed_build_writers` and `newborn_quarantined` are `info` yet often explain a `warn` beside them.

## Volume and retention

One condition can write several records, and one record can stand for many occurrences. Read counts through these rules, all held in `crates/rimz/src/diag.rs`.

- **Identity rate limit.** The sink admits one record per identity per 30-second window (`DIAG_RATE_LIMIT_WINDOW`), where the identity is the kind plus its salient evidence fields ([`DiagEvent::identity_key`](../../crates/rimz/src/diag/record.rs)). Suppressed repeats are counted onto the next record of that identity as `suppressed_since_last`, so a per-tick repeat collapses to one periodic line carrying the tally. The limiter belongs to the sink and is shared by every renderer sink the host derives from it. A successor drainer or host starts a fresh window.
- **Kind ceiling.** Each kind admits at most 120 records per window (`DIAG_KIND_CEILING`). Drops past the ceiling are added to the next admitted record's `suppressed_since_last`.
- **Kinds that skip the identity limit.** `health_alert`, `renderer_panic`, `renderer_exit`, `client_reaped`, `client_reattached`, `topology_write_rejected`, `hidden_views_fitted`, `fetch_reduction_stats`, and the three width traces go through `DiagSink::emit_unlimited`: only the kind ceiling applies, so each occurrence is its own line.
- **The observer adds no rate limit of its own.** The host's writer emits drafts through the sink. `dropped_msgs` separately counts drafts the full channel shed.
- **Audit retention.** The room's audit sweep removes the oldest files under `audit/`, logs and captures alike, while any is older than 30 days or the directory exceeds 64 MiB (`disk::retention::AUDIT_RETENTION`, `AUDIT_MAX_BYTES`). Copy `audit/diag-frames/` pairs out at the start of an investigation.

## Frame captures

Three kinds name a capture in `frames_ref` when one was written: every `pane_count_drop`, a `pane_carry_forward` with a prior frame, and a `frame_rejected` with reason `missing_own_pane` and a prior frame. Empty-frame and mux-error rejections carry none. Captures live in `audit/diag-frames/`, a private `0700` directory beside the log. Each pair is one `frame.<at_ms>.<seq>.<kind>.json` file holding `prior` and `offending`, written as a disposable cache (atomic rename, no fsync) by `DiagSink::capture_frame_pair`. No write-time limit applies: the audit sweep is the only bound. The capture is written before the sink's rate limit decides, so a suppressed record still leaves its capture behind, unnamed by any record.

Frame captures may contain command lines, cwd values, and other pane metadata. They sit behind the same local-filesystem privacy boundary as the rest of the workspace state directory.

## The frame-stream observer

The observer exists because a class of sidebar bug heals itself within seconds and leaves nothing behind. The room plane observes its shared workspace stream once, and each renderer observes its own committed roster, the fused and gated `SidebarSnapshot` sequence it paints. The host's writer records an evidence-rich `frame_anomaly` when a stream misbehaves: a roster that empties and refills, a duplicated card, a phantom row, a value that bounces between two figures, a card whose pane or process is gone. A room-common anomaly names the eldest renderer of that publication in `instance_id`, and an own-roster anomaly names its renderer.

The observer reads the stream and emits records. It never changes what the renderer commits or paints. A caught anomaly becomes a detector test ([below](#from-anomaly-to-regression-test)), and a detection that proves reliable becomes a prevention guard, either renderer-side in the [commit gate](../../crates/rimz/src/sidebar_pane/app/gate.rs) or at the source in producer frame validation.

[`sidebar/observe`](../../crates/rimz/src/sidebar/observe.rs) owns the signature (`sig.rs`), the stream detectors (`detect.rs`), and the writer thread (`writer.rs`). The anomaly vocabulary is `AnomalyKind` in the record schema, and the stream windows live in [`timing.rs`](../../crates/rimz/src/sidebar/timing.rs) under the `OBSERVE_*` prefix. The renderer's `app::loop_state` also checks painted geometry for `clicked_row_displaced`, over the `LAYOUT_FREEZE` click window.

### The commit point

Every mutation of a rendered snapshot passes through `observe_commit` in [`app::loop_state`](../../crates/rimz/src/sidebar_pane/app/loop_state.rs), reached by both pulls and event overlays. It extracts only the pane's own view, roster counts and pane membership, active events, and gate and health streaks for own-roster detection. No commit signature is extracted when the plane has no diagnostics writer.

Before local projection the fetch worker extracts one room-common `FrameSig`: sorted row identities, card kinds, pane ids and pids, groups and baseline order, watched values, aggregates, and pulled membership. Every projection shares that signature by `Arc`, and the writer runs its common detectors once. Common detection describes the workspace baseline, not each pane's gated or layout-frozen presentation. The worker's fast and produced publications have distinct baselines and therefore distinct signatures. Scoped overlay events feed a deduplicated room observation store as well as each pane's unchanged fusion store.

- The row signature leaves out renderer-local presentation state (unread stamp, selection, scroll), so presentation churn reads as unchanged content.
- The common signature keeps un-fused pulled membership, the frame stamp, and dashboard aggregates. Own records keep their projected pulled row count and the common pulled stamp beside their actual committed counts and local streaks.
- Room signatures and own drafts travel over a bounded 64-slot channel. A full channel drops the message and carries its drop count into the next passing anomaly, so neither the worker nor the renderer waits on the observer.

The windowed stream detectors stay off during `OBSERVE_WARMUP` (10 s) after the first frame-backed commit, because startup and reload transients are expected, and they stay off while the committed fold has no pane frame. The per-frame checks run from the first commit.

### The click detector

The click detector arms on a local focus input that targets a painted card, and checks after every paint, input and animation paints included. It reads the card's own first-line absolute pane row from `UiState::painted_rows`, not the target window used for anchoring. Any other input, or five seconds since the click or anchor application, ends the watch, and a later focus input replaces it. A displacement emits `from_y`, nullable `to_y`, and `dt_ms` since the click (or since the anchor was applied, for a destination landing), then re-baselines. Leaving the viewport emits `to_y: null` and disarms. A paint that follows one taken mid wheel glide re-baselines without emitting, because that motion is the user's own scroll.

A destination also seeds a watch after a landing paint that missed a shared placement, baselined on the requested row using the post-paint scroll-zone top. The same landing judge feeds the [push handshake](#push-handshakes) peer verdict and this record. An existing local watch for that card is kept, so same-tab input does not double the record. The landing miss uses `no_room_clamp` when clamped and `height_above_changed` otherwise, and the watch then continues at the painted row.

On later paints the detector attributes the first applicable cause: `no_room_clamp`, a layout freeze ending between paints (`hold_expired`), `banner_toggled`, `selection_changed`, otherwise `height_above_changed`. Banner and selection comparisons use the previous paint, not the original click. The diagnostic subject is `{row_id}:{cause}`, so an expected clamp cannot suppress or fold into a different cause on the same card. Click drafts use the committed snapshot's frame stamp and the same nonblocking `OwnAnomaly` channel and drop carry as roster drafts. Render itself reads no wall clock and performs no diagnostic I/O.

### Windowed detectors

The windowed family recognizes the back-and-forth motion a user would describe: rendered, gone, rendered again. Windows measure receiver-clock time, like the event store's TTLs.

| Detector | Window | Fires on | Stays quiet when |
| --- | --- | --- | --- |
| `roster_flap` | 10 s | A populated roster empties while own-view still counts working siblings, then refills inside the window | Active `PaneClosed` events cover every vanished row's pane (a genuinely emptied tab is [self-close](./sidebar/sidebar.md#self-close) territory) |
| `row_presence_flap`, `short_lived_row` | 7 s | One row disappears and returns inside the window, or a row is born and vanishes inside it (the phantom card, group key recorded) | A `PaneClosed` justifies the absence, or the row's pane still backs another row (the pane was rebound to a new identity) |
| `value_oscillation` | 5 s away | A watched per-row value returns to its exact prior figure after differing, however long that figure had been stable: context %, token total, group key, or model | The field's first appearance, since enrichment warm-up goes from `None` to a value, or a return after more than the window away |
| `aggregate_oscillation` | 12 s away | A dashboard figure returns to its exact prior value after differing, however long that value had been stable: cockpit or workspace spend year, or any keyed provider-panel value (below). A figure whose provider panel or window bar leaves the frame and returns counts, with `via` reading `<none>` | First appearance, or a return after more than the window away. A figure the snapshot could not supply reads `<none>` with `pulled_via` absent, so an unavailable tally stays distinct from a real `0` |
| `order_flap` | 7 s away | Rendered row order inside one group returns to its prior order after differing, however long that order had been stable, with unchanged visible membership | The visible set changed, as in a real re-rank or a cap tail rotation. Or a row whose index differs between the two orders has a different status on the frame the order returned on than on the frame it left, since ranking follows status (a short turn, an answered ask). A status change on a row that kept its place does not silence the flip. Or a return after more than the window away |
| `status_churn` | 30 s | Four or more status transitions on one row inside the window | A single `running → idle → running` turn boundary |

`row_presence_flap` stamps the frame that **caused** the anomaly, not the frame that revealed it: it fires when the row returns but carries the frame the row went missing on, so the `produced_at_ms` join reaches the producer records for the same episode. The other windowed records stamp the frame they were detected on. Doctor folds copies from several renderers or a host replacement into one incident.

The `value_oscillation`, `aggregate_oscillation`, and `order_flap` windows bound the time the value was away, and `span_ms` carries that time. Status is not a watched value, because a short turn or an answered ask is a legitimate status blink. A single status blink on one row is therefore recorded by no detector: at this layer it cannot be told from a short turn, and `status_churn` catches only repeated motion. Retained logs can hold `value_oscillation` records with `field: status` and a `span_ms` measured from the returned-to value's first appearance. An aggregate key missing from a frame is sampled as an absent figure, and its history is dropped once it has been absent for longer than the window.

Under heavy mux enumeration churn a published pane frame can briefly drop and re-list one pane. Carry-and-verify covers most of that gap while letting genuine closes through, so a residual row-presence flap stays a diagnostic record.

The provider panel is keyed per login by what it renders. A record's `aggregate` names the key:

| `aggregate` | Names | Value |
| --- | --- | --- |
| `provider_spend` | The login's trailing-year spend | Integer cents |
| `provider_spend_period` | `period`: `headline`, `week`, or `month` | Integer cents |
| `provider_mana` | One window's used % | The percent |
| `provider_mana_field` | The same window, with `field`: `resets_at` or `lifted` | The stored reset timestamp, or `true`/`false` |
| `provider_field` | `field`: `version`, `plan`, `metered`, `remote_control`, `day_budget`, `extra_credits`, `reset_credits`, or `redeem_forecast` | The value as text. A composite is one string of everything the panel renders from it, money in cents: day budget `spend/cap` with `/parked` appended while parked, extra credits `disabled`, `unlimited`, or `used/remaining/limit` with `-` for a missing figure, reset credits `count@soonest-expiry` or the count alone when no expiry is known |

An unset field is an absent value and reads `<none>`. Two panel values are left unkeyed because they move in normal operation, where a flip is not a fault: `active_sessions` and a window's `observed_at`. A period or a window field shares its identity prefix with the year or used-% key (`provider_spend:<login>:week`), and is a separate `aggregate` tag on the wire, per the [new-tag rule](#rules-a-change-keeps).

Two edge detectors run beside the windows, without a window of their own:

- `aggregate_reset` fires when a spend tally drops from a non-zero figure straight to zero, and carries the prior figure and the pulled value. It covers only the trailing-year tallies (cockpit, workspace, and each provider's `provider_spend`), whose figure never legitimately drops to zero in place. A provider mana window rolling to zero is normal, and the shorter spend periods are not checked. A transient zero that returns inside the window also records `aggregate_oscillation`.
- `aggregate_backstep` fires when a provider's `version` parses lower than the one the frame before it carried, and records the key, `from`, `to`, and the pulled figure. It catches a version alternating more slowly than the oscillation window, one record per downward step under the sink's identity limit. A version that does not parse as `major.minor[.patch]` is never judged.

Nothing else is checked for a backward step, because every other panel figure can legitimately fall. Every spend figure is a trailing window, the year included: entries older than eight days are merged per transcript, day, model, and thread, and each merged entry leaves the year whole when it passes 365 days, so a spend drop short of zero can be ageing on an account with year-old history. A window's used % is lowered by an authoritative reading, its reset time jitters between readings, and credits and the day budget reset by design.

### Per-frame checks

The consistency family checks each committed frame alone. Each check compares fields the view-model contract says must agree.

| Detector | Violation |
| --- | --- |
| `duplicate_pane_rows`, `duplicate_row_id` | Two rows share a pane id or a row id, breaking one pane, one row. This is the duplicated-card bug as a single-frame verdict |
| `status_count_mismatch` | A group's declared `status_counts` histogram differs from a re-tally of the group's rows in the signature. The check requires exact equality |
| `subagent_top_level_leak`, `subagent_double_render` | A row lists itself as its own child, or a child renders both nested and top-level or nested more than once, under one parent or several |
| `frameless_rows` | A fold with no pane frame still carries rows, which the contract makes unreachable because the pane frame admits every card |

### Real-world cross-checks

The writer thread re-verifies the latest roster against the world every `OBSERVE_CROSSCHECK_TTL` (5 s). It reads only what the producer already published (the workspace pane frame through the stat-gated cache read) and process liveness, so the producer stays the only external puller ([state.md](./sidebar/state.md#the-room-data-plane)).

| Detector | Fires when |
| --- | --- |
| `row_pane_missing_from_frame`, `cards_exceed_panes` | A roster pane id is absent from the published frame, or the roster outnumbers the frame's panes. Runs only when the roster's fold stamp equals the frame's `produced_at_ms`, since a republish between fold and read is normal skew |
| `dead_pid` | A row's pane root is gone, or its live start mismatches the published stamp, for `OBSERVE_DEADPID_CONFIRMATIONS` (2) consecutive passes. Agent rows derive the agent CLI's start under that root, as the producer does, and fall back to the root's start when derivation is unavailable. Process rows compare the root's start. Platforms without process metrics skip it |
| `agent_card_without_process` | An agent row's live pane root authoritatively hosts no process of that kind for `OBSERVE_HOSTLESS_AGENT_CONFIRMATIONS` (2) consecutive passes. Unreadable or branching process trees stay silent, and dead roots are `dead_pid`'s verdict |

### Roles and cost

Every attachment runs inline detectors on its committed stream and sends drafts to the host's one observer writer. That writer also observes the shared workspace stream and runs the cross-checks, so the room pays for process and cache reads once. It emits shared and pane-local anomalies through the same rate-limited sink, and rebinding a pane adds no limit of its own. Different renderers can contribute evidence to one episode, and `instance_id` names the renderer whose evidence was recorded. Retained records may carry a `role` field that current records omit. Gate, fetch, and health records stay per renderer.

The cost (one O(rows) signature pass per committed fold, a bounded channel, one throttled cross-check pass) is budgeted in [performance.md](./performance.md#everything-else).

### From anomaly to regression test

A `frame_anomaly` record carries enough to rebuild the stream that produced it: the frame stamp and pulled-truth scalars, row and pane identities, edge timestamps, the event summary, and the judging window.

Encode a confirmed anomaly as a test in [`observe/detect/tests.rs`](../../crates/rimz/src/sidebar/observe/detect/tests.rs). The `sig` and `row` builders there reconstruct the minimal committed sequence from the record's evidence (a warm frame, the offending frame, the restoring frame), and the assertion pins the recorded verdict and its evidence values beside the verdicts that must stay absent. `row_presence_flap_stamps_the_frame_the_row_went_missing_on` has this shape. Build the fixture from the log record rather than the frame capture, because the audit sweep can remove a capture while its record survives in the log.

## The focus trace

`audit/focus.log.jsonl` traces how focus moves through the room: what the host published, what each attachment selected, and how a cross-tab jump travelled. [`focus_trace.rs`](../../crates/rimz/src/diag/focus_trace.rs) owns the schema (`rimz.focus_trace.v1`). Records go through `DiagSink::trace_focus`, so they carry the sink's room identity in the same envelope fields as the main log (no `severity` or `suppressed_since_last`), and no rate limit applies. Neither correctness nor rendering reads this log back.

| `event.kind` | Writer | Written when |
| --- | --- | --- |
| `frame_published` | `sidebar::produce::panes`, after each pane-frame cache write | Focus, its origin, client-sample freshness, `client_sample_withheld`, own-pane membership, the roster's pane set, or the publication class changed |
| `fold_decided` | each attachment | Snapshot focus, own-view availability, the baseline, or the selected pane changed |
| `push_handshake` | the jump's source attachment | Every same-host cross-tab jump that reached push |
| `click_delivery` | the host, per attached renderer | Every host click datagram |
| `jump_progress` | the jump's source attachment | Every lease arming and pre-push checkpoint |

### Publications and folds

`frame_published` and `fold_decided` are edge-gated: identical publications and folds append nothing. The comparison lives in the sink (`focus_trace::Edges`), and a renderer sink derived with `DiagSink::for_renderer` starts its own, so folds compare per attachment and a replacement host's first publication starts a new baseline. Metrics-only writes use the topology publication class. `client_sample_withheld: true` marks a publication while the plugin withholds unsettled clients, omitted when false, and a change in that flag is a publication edge. `fold_decided` covers seeds and event-fused folds. Private CLI reads append no publication trace.

To correlate a fold with its publication, take the latest `frame_published` whose `produced_at_ms` is at or below the fold's `panes_produced_at_ms`. An unchanged newer publication needs no record of its own.

### Push handshakes

`push_handshake` records one cross-tab jump from its source, without edge gating or sampling. The source writes it after dispatch, once late reports are collected (until every peer handle drops, or 1 s after send), so neither the write nor the late collection delays the jump. The envelope names the source attachment and the write time. The event carries:

| Field | Meaning |
| --- | --- |
| `nonce`, `target` | The focus intent and its destination pane |
| `budget_ms` | Time remaining from send until click + 500 ms, floored at zero. Request work and fitting spend this one budget |
| `lead_ms` | From lease arming on the source loop, before the synchronous click paint, to the send instant, request work and any fit included |
| `fit_ms` | From push entry to the send instant, the fit included. Omitted when zero or when no fit callback ran |
| `decided_ms` | From the send instant (taken after any fit returns) to the source's wait decision |
| `dispatched_ms` | From the send instant until the mux dispatch returns. Absent on a superseded jump |
| `outcome` | The source's verdict at its wait decision (below) |
| `peers` | One entry per other attachment, in subscriber order (below) |

The source classifies `outcome` at its wait decision:

| `outcome` | Meaning |
| --- | --- |
| `painted` | A `painted` or `displaced` peer report consumed before the budget |
| `released` | Every handle dropped before the budget without a landing report |
| `timed_out` | Any decision at or after the budget, including a release or landing report at the tie |
| `superseded` | A newer click cancelled the pushed jump before dispatch. A cancellation before push writes no handshake |

Each `peers` entry carries the attachment's `instance_id` and `delivery` (`sent` or `inbox_closed`), even when delivery failed. A failed send has no stages or verdict. The stage stamps are relative to send: `received_ms` stamps arrival, `snapshot_ms` ends pending-snapshot preparation, `folded_ms` ends the intent fold, and `finished_ms` stamps the verdict. A known non-owner returns before preparation or folding, and a datagram-first nonce also skips those stages, so both omit the middle stamps. Supersession can finish a push before receipt or between preparation and folding, leaving the later stages absent. Absent optionals and zero counts are omitted.

| Peer field | Meaning |
| --- | --- |
| `verdict` | `painted`, `displaced`, `not_destination`, `exiting`, `expired`, or `superseded`. `displaced` means the placement was consumed with the owning card selected at the expected height but off its requested row. Like `painted`, it ends the source's wait. A sent peer without a verdict tore down without reporting or did not report within the horizon |
| `painted_ms` | For `painted` and `displaced`, the signed offset from send to completion of the terminal write for the frame that earned the verdict. Negative when a datagram-first frame was already painted before send. Receipt time is not paint time |
| `blocked_by` | The first gate met while parked: `paint_hold` or `confirming_empty`. Exit has its own verdict |
| `unmatched_paints` | Parked paints that failed the acknowledgement predicate |
| `misplaced_paints` | The subset of `unmatched_paints` whose selected row is already correct but whose placement is not yet consumed, whose painted height is off the push's expected rows, or whose anchor stamp is stale. A consumed placement off its row is the `displaced` verdict, not a count |

To select misses from a focus log:

```sh
jq 'select(.event.kind=="push_handshake" and (.event.outcome!="painted" or any(.event.peers[]; .verdict=="displaced")))' audit/focus.log.jsonl
```

### Click delivery and jump progress

`click_delivery` records a host click datagram for an attached renderer: the target attachment in the envelope, zero-based `column` and `row`, and `delivery` (`sent`, `inbox_full`, or `inbox_closed`). It measures delivery to the bounded input lane, which is independent of the room inbox. It is not a card hit or a processed click. A successful `sidebar click` command only enqueues its datagram, so this record tells a full or closed input inbox apart from a jump stopped later. Ordinary attach-stream input writes no such record.

`jump_progress` records cross-tab lease arming and the pre-push checkpoints, without edge gating or sampling. It carries the source attachment in the envelope, a host-local `sequence`, `target`, `stage`, and wall-clock `elapsed_ms` since lease arming. The sequence orders focus effects as source loops processed them, not raw input arrival across loops.

| `stage` | Meaning |
| --- | --- |
| `armed` | The lease armed |
| `request_waiting` | Before the serialized request gate |
| `request_started` | Inside the request gate |
| `request_superseded` | The gate refused an obsolete lease |
| `request_failed` | The request failed |
| `anchor_changed` | The stored nonce no longer matched before push. This fallback also uses the request gate for its dispatch fence |
| `push_superseded` | The obsolete jump stopped before handshake tracing |
| `input_superseded` | A same-tab focus click or key revoked the lease without arming another. The record keeps the cancelled jump's sequence, target, and source attachment |

These records stay when no `push_handshake` follows. Missing progress after a stage means work still in flight or an unavailable sink, not proof of cancellation.

## Zellij presence-plugin telemetry

`audit/plugin-presence.log.jsonl` holds the samples the presence plugin's keepalive delivers ([`plugin_presence.rs`](../../crates/rimz/src/diag/plugin_presence.rs), appended from `sidebar::presence`). Each sample carries the exact `(loaded_at_ms, plugin_id)` generation and the plugin build. The rest of its fields are listed in [multiplexers.md](./multiplexers.md#the-zellij-presence-plugin).

`rimz doctor` lists the presence-plugin panes loaded in the live session and joins each id to its newest telemetry generation. A plugin is active when it is the fresh topology cache's writer, rejected when it is the stale writer in `lanes/topology-writer-conflict.json`, and inactive otherwise. When the live listing fails, Doctor reports the probe unavailable rather than presenting retained telemetry as live. Each plugin row takes the first verdict that applies (`crates/rimz/src/cli/doctor/render.rs::presence_plugin_verdict`):

| Plugin | Verdict |
| --- | --- |
| Rejected | Counted warning: a newer plugin took over, with the ignored write count. Fix: `rimz reload` |
| Inactive | Information: loaded, not writing pane topology |
| Failure delta while its telemetry is fresh | Counted warning naming the last failure's cause |
| Build differs from the desired plugin build (outdated) | Counted warning. Fix: `rimz reload` |
| Otherwise | Healthy |

Memory growth is informational, and several loaded plugins point to `rimz reload`. Pages climbing with the Zellij server's RSS put a leak in the plugin's WASM linear memory. Flat pages under climbing RSS put the growth in Zellij-native state on the plugin command path. Samples that lack a build or split failure counts report those fields as unknown.

## Reading the log

`rimz doctor` prints the latest twelve incidents from the current workspace's log, after dropping records at or before the history watermark. The folding lives in `crates/rimz/src/cli/doctor/runtime.rs::collect_diagnostics`.

An incident folds records by session, build, and a family key. Sibling kinds share a family: `gate_hold` with `gate_release`, `pane_carry_forward` with `pane_carry_refuted`, and `frame_rejected` with `frame_shrink_verified`. Active and recovery records of one episode share a key too. A record joins the incident with its key while it arrives within 60 seconds of the incident's last record. Two kinds fold differently:

- A `frame_anomaly` with a produced-frame stamp folds only with copies whose session, build, event identity, and stamp all match, at any distance in time. These are the same anomaly seen by several renderers or a replacement host.
- `hook_drain_lag` uses a 15-minute quiet period for both grouping and recovery. Records less than 15 minutes apart join one episode, and 15 quiet minutes settle it as recovered with info impact. A later lag record starts a new investigative warning. Doctor reads the current clock to settle a quiet episode but writes no recovery record.

Each incident keeps its source severity (the highest of its records) apart from its state (`investigate`, `contained`, `recovered`, `expected`) and its impact (`alarm`, `warn`, `info`). The state comes from the newest record through the [severity table](#severity). An investigative incident has `alarm` impact at `error` severity and `warn` impact otherwise, and every other state has `info` impact. Only investigative incidents count against Doctor's health tally. The incident also carries record and distinct-observer counts, suppression totals, dropped-message counts, the occurrence range, build staleness, and evidence references. The human report gives each investigative incident a table row and folds settled states into one counted line. `--json` carries every field of every incident.

A drain-lag row summarizes the episode's worst sampled lag and peak sampled pending bytes, not its last sample. JSON adds `drain_lag: { worst_lag_ms, peak_pending_bytes }` only to lag incidents.

A `clicked_row_displaced` with `no_room_clamp` is expected because a data, configuration, or destination-geometry difference left the viewport no room to preserve the card's row. Selection-driven collapses instead hold the previous card open until they fit, so the other causes are investigative.

Doctor's message probe ([`cli/doctor/messages.rs`](../../crates/rimz/src/cli/doctor/messages.rs)) reads the message records beside this log, and calls an open record stuck once it carries `attempts` above one, an unconfirmed send, or any `last_error`. A wake the sweep deferred because its receiver has no live pane records that error in its first 30-second window ([messaging](./harness/messaging.md#scheduling-and-wakeups)). It is a doctor problem row from that window until the pane returns and the next defer clears the error. That is deliberate: an undeliverable message is what the health surface is for.

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
2. **Locate the fault in published truth or the local fold.** Every `frame_anomaly` carries the pulled snapshot's scalars beside the rendered ones. For `row_presence_flap`, read the missing-edge frame stamp and `gap_evidence.pulled_row_present` and `pulled_pane_present`: false membership puts the gap in pulled truth, true membership in the renderer's committed fold. The `instance_id` identifies the renderer whose evidence was recorded, so inspect its gate, fetch, and health records too.
3. **Attribute the cause.** Producer records in the same window name it: the [carry kinds](#carry-kinds), `frame_rejected` for held implausible reads, `pane_count_drop` for published shrinks, `gate_hold` for renderer-side holds. The frame stamp (`produced_at_ms`) joins producer and observer records across the episode. A capture's filename carries its write time instead, so reach it through the record's `frames_ref`.
4. **Diff the captures.** Each capture holds the last good frame beside the offending one. `jq '{prior: (.prior.tabs | length), offending: (.offending.tabs | length)}'` shows a whole-tab omission at a glance.
5. **Encode the episode** as a detector test ([above](#from-anomaly-to-regression-test)).

Three shapes are worth recognizing.

**A long run of `gate_hold` with rule `agent_demoted_to_process`**, where every matching `gate_release` carries `via_escape_hatch: true`, means the rollup kept presenting the same live agent pane as a bare process with unchanged or missing foreground-command evidence. A real exit whose foreground command changed commits at once. A nearby `hosted_carry_dropped` means the producer's hosted-stamp carry declined, and its `reason` names the source guard. Silence proves less: the carry may have restored the stamp, found no prior stamp to carry, or declined again inside the identity window. Read the published frame's hosted stamp before instrumenting the rollup bind guard. The reject count is evidence only: the gate holds until `ACCEPT_REGRESSION_AFTER` (1 s) has passed since the first reject, then releases through the escape hatch.

**A partial read**, where a pane source reports fourteen panes as six by omitting two whole tabs whose processes live. Both the topology cache and native Zellij listings have shown this shape. The carry-forward guard protects whole omitted tabs even after a successful server read, so a healthy log shows `pane_carry_forward` under a steady roster (`cli_confirmed: true` when the server answered, `false` on a cache fallback). It does not carry a missing sibling from a tab the server still lists. When the whole-tab guard misses, the log reads:

| Records | Reading |
| --- | --- |
| `pane_count_drop` with eight removed panes | The shrink published. The capture pair preserves both frames |
| five `row_presence_flap`, gone to back in 2.26 s, no `PaneClosed` events, `pulled_rows` back at full count | Every instance painted the flap. Pulled truth had already recovered, so the rendered gap was the published partial frame propagating |

**A tab or card that outlives its pane**, where a closed pane keeps a sidebar-only tab open, or its card listed, for up to 30 seconds. The log reads `pane_carry_forward` at the close, `carry_forward_expired` about `PANE_CARRY_TTL` (30 s) later, then `renderer_exit` with cause `self_close_empty_tab`. No `self_close_rejected` appears, because the carried sibling keeps the attachment from asking and the request never reaches the supervisor, so start from the carry records rather than looking for a rejection. Carry holds a closed pane while its root process lives, or while tab coherence vouches for it because another missing pane in the tab proved the tab alive. Measure the root process's lifetime beside the symptom (`ps -o pid,etimes,cmd -p <root_pid>` on the pane's recorded root pid). A root that ignores the hangup outlives its pane, and the late close tracks the shorter of its lifetime and the TTL.

## Inspecting live card state

Card-content questions (a wrong gauge, a missing cost, a card resting in the wrong shape) are answered from the same read path the renderer runs, before any raw file is opened. Rendered-frame anomalies (flicker, duplicate rows, missing tabs) take the [episode workflow](#investigating-an-episode) instead.

`rimz workspace resolve <path>` prints the `workspace_id` for any project path. Every worktree of a repository resolves to the repository's own workspace, so a `rimz-worktrees/<branch>` checkout maps to the main repository's directories. State lives under `~/.rimz/ws/<workspace-dir>/` and runtime files under `$XDG_RUNTIME_DIR/rimz/ws/<workspace-dir>/`, where the directory name is `<basename>-<hex>` rather than the id. `rimz paths` prints both for the current project, a layout owned by [`disk/paths.rs`](../../crates/rimz/src/disk/paths.rs).

`rimz sidebar snapshot --json --no-produce` prints the fused `SidebarSnapshot` a node renders: the event-fresh rollup folded over the published pane frame plus the per-session sidecars ([state.md](./sidebar/state.md)). Run it inside the workspace, or pass `--workspace-id` from outside. `--no-produce` keeps the read passive, with no mux or git forks, once the command knows the room's session name: from the resolved workspace inside it, an explicit session, or the workspace record from outside. With none of the three, it falls through to one producing refresh, as it does without the flag.

`rimz sidebar frame` prints the rendered frame through the same passive read when a producer frame exists, and falls back to one producing refresh otherwise. It draws at the size of the room's live sidebar pane, which each renderer stamps into its heartbeat. `--width` and `--height` override it, and without a live sidebar the width falls back to the configured policy over the calling terminal. ANSI color is stripped when stdout is piped. `--expand` expands every card and reveals every capped worktree row, so large fleets do not clip.

```sh
rimz sidebar snapshot --json --no-produce | jq '
  .agents[] | select(.worktree_branch == "truecolor") | {
    agent_id, kind, status, context_pct, total_tokens, compaction_count,
    tokens: .context.tokens, cost: .context.cost.total_cost_usd,
    observed_at: .context.observed_at }'
```

The split inside that one object is the provenance map. Bare row fields (`status`, `context_pct`, `total_tokens`, `compaction_count`) are rollup truth derived from hooks and transcript tails, while everything under `context` is the session's context sidecar, stamped `observed_at`. Statusline and app-server reports, local transcript refreshes, and sparse hook observations all merge into it (`crates/rimz/src/store/agent_context.rs`). A figure wrong in only one half names the half to debug.

Raw sidecars confirm what a producer actually wrote. Filenames are digests because session ids are free strings, so scan by record content:

```sh
cd "$(rimz paths --json | jq -r .runtime_dir)"
jq -r '[.kind, .agent_id, .context.session_name] | @tsv' live/agent_context/ctx.*.json   # find a session's record
jq . live/agent_context/ctx.<digest>.json                                                # the full record
```

The store's own view without the sidecar fold is the published checkpoint plus log tail ([store.md](./store.md#the-read-path)). Comparing it against the snapshot attributes a wrong figure to the rollup, the sidecar, or the fold. The renderer-side derivations a card dispute usually hinges on live on the view model: the gauge-source preference is [`AgentCard::context_gauge_percent`](../../crates/rimz/src/store/snapshot/row.rs), and the card shape is chosen by `CardStage` in [`agent_card/template.rs`](../../crates/rimz/src/sidebar_pane/render/sections/agent_card/template.rs).

## Off-box error reporting

Off-box error reporting sends RimZ's warnings, errors, and panics to a Sentry project, so a contributor can watch a fleet's health without tailing every box. It is best-effort enrichment and never holds a correctness path.

The code compiles only under the dev-only `sentry` cargo feature. A shipped binary omits it and ignores any `[sentry]` config. Without the feature, [`observability`](../../crates/rimz/src/observability.rs) is a no-op with the same surface, so `main.rs` and the CLI dispatch are identical in both builds. With it, [`observability/reporting.rs`](../../crates/rimz/src/observability/reporting.rs) is the live implementation.

An opted-in build reports the `warn!` and `error!` events RimZ raises, the panics it hits, and the agent conditions it observes (rate limits, spend limits, provider overload, and other turn-ending API failures) at warning level.

### Opting in

`cargo xtask install-dev` is the contributor shortcut. After installing the Cargo tools in [`scripts/install-dev-tools.sh`](../../scripts/install-dev-tools.sh), it installs the optimized `profiling` host profile with the feature on, then runs `sentry-cli debug-files upload` on the binary. A failed upload is retried up to three times, one second apart. When `sentry-cli` cannot start at all, it warns once without retrying. Either way the install succeeds.

Reporting turns on when a DSN resolves from `RIMZ_SENTRY_DSN` or the per-machine `[sentry] dsn`. The env value wins, and an empty value counts as unset. The DSN lives only in per-machine config, never in the committed `.rimz/config.toml`, so a clone or pull cannot redirect a contributor's telemetry, and it stays off the [project trust surface](./harness/trust.md#the-executable-surface).

`RIMZ_SENTRY_ENVIRONMENT` (or `[sentry] environment`) tags the deployment. Unset, it follows the build profile: a `release` build reports as `production`, and dev, profiling, and CI builds as `development`, keeping contributor noise off the production dashboard. The config shape is in [configuration.md](../guide/configuration.md#off-box-error-reporting) and the user-facing data boundary in [security.md](../guide/security.md#off-box-error-reporting).

With no DSN, no client is created and RimZ makes no network calls. A malformed DSN yields `Reporting::InvalidDsn`, logged with the fix once the subscriber is live and otherwise inert, so a telemetry typo never degrades or blocks a command.

### One init point covers every process

`main` creates the Sentry client once, before the tracing subscriber, and holds the guard for the whole process. The guard flushes pending events on drop, which covers short-lived hook subprocesses. Every RimZ subcommand runs through that `main`, including the `hooks feed` subprocess where agent conditions are observed and the `sidebar serve` loop. The wasm presence plugin is a separate binary with no HTTP stack and reports nothing.

A live workspace pin (`RIMZ_WORKSPACE_ID`) becomes a `workspace` scope tag, so one machine-wide DSN still filters per repository. Once the command parses, `set_command_scope` adds `command` and `build` tags (the same build id the diagnostics log stamps) and a structured `rimz` context with the command, the build, and, when the process serves exactly one, the agent kind and session.

The client reports under the `rimz@<build id>` release. One identity therefore tracks regressions across builds, `resolve --in-next-release` reopens on a genuinely new build, and uploaded debug files stay keyed. The profiling profile embeds DWARF line tables and frame pointers in one self-contained binary, so the uploaded debug file is the installed binary itself, whose GNU build-id begins with the release's build id.

### The tracing bridge is the capture path

The Sentry layer joins the tracing subscriber beside the stderr formatter and turns each `warn!` and `error!` into a Sentry event at the same level. The sidebar health target `rimz::sidebar::health` stays local, because the durable `health_alert` and `renderer_exit` records already carry that episode. With no DSN the layer is omitted and the subscriber is unchanged.

Breadcrumbs work by allowlist. Each deliberate seed is an `info!` on the `rimz::trail` target and rides along on the next event, so a warning arrives with the trail that led to it. An unmarked `info!` is ignored, so a stray field (a socket path, a cwd) never leaves the box as breadcrumb data. The layer carries its own `INFO` filter, so it never raises the global max-level hint above what the stderr formatter's `RUST_LOG` filter sets. Under the default filter, `debug!` and `trace!` are never constructed, and the `sidebar serve` hot loop never feeds the trail. Callsites attach a searchable `tags.operation` and pass the error as `&dyn Error`, so an event names the failed operation and carries the error's exception and stacktrace.

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

Prompts and transcripts are never forwarded. Hook payloads are forwarded in one case: an unusable subagent identity logs the raw hook payload as a field of an `error!` on `rimz::agent::lifecycle` (`crates/rimz/src/agents/identity.rs::resolve_subagent_identity`), and `before_send` does not strip event fields. The transport is the same small rustls-backed `ureq` client RimZ uses for pricing, and it swallows network failures so they never surface on a RimZ path.
