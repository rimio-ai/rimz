# Diagnostics

RimZ records what goes wrong as typed evidence, so a transient fault leaves something to debug after it heals. A roster that empties and refills over two seconds, a spend figure that blinks to zero, or a card that outlives its pane is invisible an hour later unless something wrote it down at the moment it happened.

One boundary makes this safe: **correctness reads the store, CAS rules, and caches, and no correctness path reads a diagnostic record back.** Diagnostics are evidence for a human, and every write is best-effort.

Evidence has two destinations. The default is a durable log on the box, always on, which most of this page describes along with the [frame-stream observer](#the-frame-stream-observer) that feeds it. The second is [off-box error reporting](#off-box-error-reporting) to a Sentry project, compiled only into dev builds and dormant until a contributor opts in.

## Where diagnostics land

Diagnostic logs live in persistent state because an investigation starts after the pane, mux session, or machine has gone away. Workspace logs land under `~/.rimz/ws/<workspace-dir>/`, which survives reboot like the store; runtime files under `$XDG_RUNTIME_DIR` die with the session.

| Surface | Location | Records | Owner |
| --- | --- | --- | --- |
| `diag.log.jsonl` | workspace state dir | typed anomaly records, rate-limited | this page |
| `diag-frames/` | workspace state dir, `0700` | prior and offending pane-frame pairs | [Frame captures](#frame-captures) |
| `notify.log.jsonl` | workspace state dir | notification emits, bell decisions, unread transitions | [notifications.md](./sidebar/notifications.md#the-trace-log) |
| `plugin-presence.log.jsonl` | workspace state dir | Zellij presence-plugin keepalive samples | [below](#zellij-presence-plugin-telemetry) |
| `focus-repairs.log.jsonl` | account-global `~/.rimz/logs/`, beside the assist history | automatic focus-repair evidence and outcomes | [state.md](./sidebar/state.md) |
| `binding.log.jsonl` | workspace runtime dir | pane-binding decisions | [sidebar.md](./sidebar/sidebar.md) |
| `topology-writer-conflict.json` | workspace runtime dir | latest Zellij topology writer conflict | [multiplexers.md](./multiplexers.md#the-zellij-presence-plugin) |

The `diag/` module owns the JSONL surfaces, and all of them share the append, decoded-visit, and generation-path helper in [`disk::rotating`](../../crates/rimz/src/disk/rotating.rs). A log rotates to one kept generation (`<name>.1.jsonl`): workspace logs at 1 MiB, the focus-repair log at the assist history's 4 MiB. A failed append logs at debug and the calling path continues. A reader visits the retained generation, then the active file, and skips missing files and malformed lines.

The first three surfaces carry workspace identity through one [`DiagSink`](../../crates/rimz/src/diag.rs). A disabled sink keeps the same methods and does nothing, so emitting callsites need no `#[cfg]` or branch. The notification trace and the binding log belong to their own subsystems and share only the rotating helper; the rest of this page is `diag.log.jsonl` and its captures.

### Zellij presence-plugin telemetry

`plugin-presence.log.jsonl` holds the samples the presence plugin's keepalive delivers ([`plugin_presence.rs`](../../crates/rimz/src/diag/plugin_presence.rs), appended from `sidebar::presence`). Each sample carries the exact `(loaded_at_ms, plugin_id)` generation and plugin build; the rest of its fields are listed in [multiplexers.md](./multiplexers.md#the-zellij-presence-plugin).

`rimz doctor` lists the presence-plugin panes loaded in the live session and joins each id to its newest telemetry generation. A plugin is active when it is the fresh topology cache's writer, rejected when it is the stale writer in `topology-writer-conflict.json`, and inactive otherwise; a build that differs from the desired plugin build is marked outdated. When the live listing fails, Doctor reports the probe unavailable rather than presenting retained telemetry as live.

Memory growth and stale-writer rejections are informational, several loaded plugins point to `rimz reload`, and a recent genuine failure delta is actionable. Pages climbing with the Zellij server's RSS put a leak in the plugin's WASM linear memory; flat pages under climbing RSS put the growth in Zellij-native state on the plugin command path. Samples that lack a build or split failure counts report those fields as unknown.

## The record envelope

Every line in `diag.log.jsonl` is one `DiagEnvelope` from [`record.rs`](../../crates/rimz/src/diag/record.rs):

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

The `build` id is a digest of the executable read from its linker-provided build-id note ([`build_id.rs`](../../crates/rimz/src/build_id.rs)), and every published pane frame carries the same stamp. Records written by an old and a new build during an upgrade therefore stay distinguishable in place. A producer that reads a prior frame stamped by a different build records `mixed_build_writers`, marking the overlap window where a stale writer can regress fresh state; `rimz doctor` separately warns when fresh heartbeats show more than one build writing.

Records are anomaly-only: routine fetch ticks, successful paints, and stable cache hits write nothing. Two kinds are deliberate traces. The sidebar width records trace each accepted or rejected keypress, controller step, and terminal outcome, and `work_pane_boundary_moved` preserves geometry drift that would otherwise leave no trace.

Pure projection layers return diagnostics as data, and impure callers append them through the `DiagSink`. Store reducers and the renderer gate carry no disk-write API, and the observer's writer thread emits through the same sink, so every source shares one envelope and one file.

## Event taxonomy

The emitter is the triage pointer. Producer kinds describe pane-source truth, renderer kinds one node's holds and refreshes, supervisor kinds process lifecycle, projection kinds binding and grouping, and `frame_anomaly` the rendered symptom as the [observer](#the-frame-stream-observer) judged it.

| Event | Emitter | Evidence |
| --- | --- | --- |
| `frame_rejected`, `frame_shrink_verified`, `pane_count_drop`, `pane_carry_forward`, `pane_carry_refuted`, `carry_forward_expired`, `hosted_carry_dropped`, `foreign_session_pane` | `sidebar::produce::panes` | Ok-but-empty frames and missing-own-pane reads, verified shrinks, pane drops with affected views, carried and refuted panes, carry expiry, hosted-agent carry declines, foreign-session leaks |
| `resolution_fallback` | `sidebar::produce` | A pane-resolution snapshot falling back to the rollup, with its reason |
| `duplicate_pane_id` | `sidebar::frame`, `store::snapshot::view` projection | A pane id listed twice |
| `mixed_build_writers` | `sidebar::produce::panes` | A prior published frame stamped by a different build than the producing process |
| `gate_hold`, `gate_release`, `fetch_failure`, `health_alert`, `link_alert`, `producer_elected`, `producer_demoted` | `sidebar_pane::app` | Renderer-side holds and releases, failed fetches, degraded refresh episodes, remote-link degraded and recovered episodes, producer handoff |
| `group_migration` | `sidebar_pane::app::state` (elder only) | A pane whose group changed between committed snapshots, with cwd before and after |
| `renderer_panic`, `renderer_exit` | `sidebar_pane::app`; `renderer_exit` also `sidebar_pane::supervise` | Panics that would otherwise vanish with the pane; self-close and give-up exits with their cause |
| `sidebar_width_intent`, `sidebar_width_nudge`, `sidebar_width_settle` | `sidebar_pane::app::width_control` | Intent verdicts, controller nudges, learned feedback, and terminal outcomes for `a`/`d` width control |
| `renderer_signal_death`, `renderer_orphan_reaped`, `supervisor_convergence`, `supervisor_preflight_rejected`, `self_close_rejected` | `sidebar_pane::supervise` | A signal or non-panic worker exit with its stderr tail; a worker reaped after fresh mux listings omit its pane; a re-exec onto a target build, or a preflight that refused one with its reason; a declined self-close with the sibling count |
| `work_pane_boundary_moved` | `sidebar::presence` (Zellij), `sidebar::presence::tmux` | Before and after horizontal geometry for a stable work-pane set whose view and sidebar widths did not change |
| `topology_writer_changed`, `topology_write_rejected` | `sidebar::presence` | Zellij topology writer generation flips and rejected stale writers, with plugin id, loaded-at generation, accepted writer, and reject count |
| `tick_budget_breach` | `sidebar::meter` | Sustained over-budget producer ticks: last and worst wall time, mux wait, fold bytes, spawns, declared budgets, streak length, episode `since_ms`, and recovery ([performance.md](./performance.md#the-tick-budget)) |
| `frame_anomaly` | `sidebar::observe` writer thread | Detector verdicts on the rendered stream, each with its detector key, evidence, frame stamp, and the writer's role |
| `row_conflict`, `newborn_quarantined`, `local_session_bind_rejected`, `ghost_session_bind` | `store::snapshot::view` | Duplicate agent identity suppression, a newborn known-command pane held until its cwd resolves, a contained evidence-free or stale local-session bind, and an old exact session stamp contradicted by a newer durable launch |
| `pane_cache_divergence`, `sidebar_orphan_reaped` | `reload` (`rimz reload`, `rimz sidebar repair`) | A fresh pane cache omitted a sidebar process that authoritative mux truth proved alive; a sidebar reaped after two authoritative omissions |
| `subagent_digest_backstopped`, `subagent_orphan_reaped`, `subagent_orphan_repair_failed` | hidden subagent helpers started by `harness::orphan_sweep` | A fleet digest the normal wrapper path missed, queued from durable run records; a child closed after its parent launch stayed ended or absent past `ORPHAN_GRACE`, or the repair error when that failed ([subagents.md](./harness/subagents.md#backstops)) |
| `client_reaped` | `cli::room::attach_exec` | Stale mux clients killed during attach, with killed pids, client counts before and after, and whether the reap settled or timed out |
| `tool_loop_escalated` | `cli::hooks::lifecycle` | A named tool's consecutive identical argument signature reaching the configured attention threshold |

The schema also keeps `fetch_fold_stats`, which no current code path emits; Doctor still reads retained records of it as expected.

The carry kinds attribute a pane-source fault precisely, and the distinction matters when reading a log:

- `pane_carry_forward` marks a mux omission that survived a forced direct re-pull while process liveness proved the omitted panes alive. The source under-reported, the producer carried the panes, and the frame pair is captured.
- `pane_carry_refuted` marks an initial listing that the forced re-pull corrected. Truth healed within one produce, so no frame pair is captured and the log line is the evidence.
- `carry_forward_expired` marks a carried pane dropping when its liveness proof runs out after `PANE_CARRY_TTL` (30s).
- `hosted_carry_dropped` marks a prior hosted lazy-agent stamp the producer declined to restore. Its `reason` is `probe_reports_absent` or `carry_expired`, where dropping a stale stamp is correct and Doctor marks the record expected, or `start_regressed` or `foreground_kind_mismatch`, which stay investigative evidence of contradictory identity.

[sidebar.md](./sidebar/sidebar.md#honest-reads-across-a-mux-hiccup) describes the guards these records come from.

### Severity

Severity follows the event, and for a few kinds the event's own fields. `DiagEvent::severity` holds the mapping, pinned by test in [`record/tests.rs`](../../crates/rimz/src/diag/record/tests.rs).

| Severity | Events |
| --- | --- |
| `error` | `renderer_panic`, `renderer_signal_death`, `ghost_session_bind` |
| `warn` | `frame_rejected`, `pane_count_drop`, `pane_carry_forward`, `carry_forward_expired`, `duplicate_pane_id`, `foreign_session_pane`, `row_conflict`, `gate_hold`, `fetch_failure`, `frame_anomaly`, `tool_loop_escalated`, `topology_write_rejected`, `renderer_orphan_reaped`, `sidebar_orphan_reaped`, `subagent_orphan_reaped`, `subagent_orphan_repair_failed`, `pane_cache_divergence`, `supervisor_preflight_rejected`, `self_close_rejected` |
| `warn` while active, `info` on recovery | `health_alert`, `link_alert`, `tick_budget_breach` (recovery sets `recovered_after_ms`) |
| depends on a field | `client_reaped`: `warn` unless `settled`. `hosted_carry_dropped`: `warn` for `start_regressed` and `foreground_kind_mismatch`. `renderer_exit`: `warn` for `degraded_gave_up`. Each is `info` otherwise |
| `info` | `frame_shrink_verified`, `resolution_fallback`, `pane_carry_refuted`, `gate_release`, `producer_elected`, `producer_demoted`, `local_session_bind_rejected`, `group_migration`, `newborn_quarantined`, `mixed_build_writers`, `topology_writer_changed`, `supervisor_convergence`, `subagent_digest_backstopped`, `work_pane_boundary_moved`, the three width traces, `fetch_fold_stats` |

Filter on the kind as well as the severity: `mixed_build_writers` and `newborn_quarantined` are `info` yet often explain a `warn` beside them.

## The frame-stream observer

Every sidebar renderer carries an observer that watches its own committed frame stream, the fused and gated `SidebarSnapshot` sequence the renderer paints, and records an evidence-rich `frame_anomaly` when the stream misbehaves: a roster that empties and refills, a duplicated card, a phantom row, a value that bounces between two figures, a card whose pane or process is gone.

The observer reads the stream and emits records; it never changes what the renderer commits or paints. It exists because this class of bug heals itself within seconds and leaves nothing behind. A caught anomaly becomes a detector test ([below](#from-anomaly-to-regression-test)), and a detection that proves reliable becomes a prevention guard, either renderer-side in the [commit gate](../../crates/rimz/src/sidebar_pane/app/gate.rs) or at the source in producer frame validation.

[`sidebar/observe`](../../crates/rimz/src/sidebar/observe.rs) owns the signature (`sig.rs`), the detectors (`detect.rs`), and the writer thread (`writer.rs`). The anomaly vocabulary is `AnomalyKind` in the record schema, and the windows live in [`timing.rs`](../../crates/rimz/src/sidebar/timing.rs) under the `OBSERVE_*` prefix.

### The commit point

Every mutation of the rendered snapshot passes through one chokepoint, `observe_commit` in [`app::loop_state`](../../crates/rimz/src/sidebar_pane/app/loop_state.rs), reached by both the pull path and the event-overlay path, so the observer runs exactly once per committed fold.

After each commit it reduces the snapshot to a compact `FrameSig` and runs every pure detector inline, in microseconds, before the loop moves on. The signature holds row identities sorted by row id, card kinds, pane ids and pids, group keys and rendered order, watched values, own-view and active-event context, and the gate and health streaks.

- The row signature leaves out renderer-local presentation state (unread stamp, selection, scroll), so presentation churn reads as unchanged content.
- The signature also carries scalars from the un-fused pulled snapshot: `pulled_rows`, pulled row and pane membership, the pulled frame stamp, and pulled dashboard aggregates. Every record therefore shows whether the producer's published truth already held the anomaly or this instance's fusion and gating introduced it.
- Detectors send drafts over a bounded 64-slot channel to the writer thread. A full channel drops the draft and stamps the drop count as `dropped_msgs` on the next draft that gets through, so the render thread never waits on the observer.

The windowed detectors stay off during `OBSERVE_WARMUP` (10s) after the first frame-backed commit, because startup and reload transients are expected, and they stay off while the committed fold has no pane frame. The per-frame checks run from the first commit.

### Windowed detectors

The windowed family recognizes the back-and-forth motion a user would describe: rendered, gone, rendered again. Windows measure receiver-clock time, like the event store's TTLs.

| Detector | Window | Fires on | Stays quiet when |
| --- | --- | --- | --- |
| `roster_flap` | 10s | A populated roster empties while own-view still counts working siblings, then refills inside the window | Active `PaneClosed` events cover every vanished row's pane (a genuinely emptied tab is [self-close](./sidebar/sidebar.md#self-close) territory) |
| `row_presence_flap`, `short_lived_row` | 7s | One row disappears and returns inside the window, or a row is born and vanishes inside it (the phantom card, group key recorded) | A `PaneClosed` justifies the absence, the row's group had its idle tail hidden at either edge (ranking churn rotates rows through the cap), or the pane was rebound to a new identity |
| `value_oscillation` | 5s | A watched per-row value returns to its exact prior figure after differing: status, context %, token total, group key, or model | The field's first appearance, since enrichment warm-up goes from `None` to a value |
| `aggregate_oscillation` | 12s | A dashboard figure returns to its prior value after differing: cockpit or workspace spend year, provider spend year, or a provider mana window % | First appearance; a figure the snapshot could not supply reads `<none>` with `pulled_via` absent, so an unavailable tally stays distinct from a real `0` |
| `order_flap` | 7s | Rendered row order inside one group returns to its prior order after differing, with unchanged visible membership | The visible set changed, as in a real re-rank or a cap tail rotation |
| `status_churn` | 30s | Four or more status transitions on one row inside the window | A single `running → idle → running` turn boundary |

Under heavy mux enumeration churn a published pane frame can briefly drop and re-list one pane. Carry-and-verify covers most of that gap while letting genuine closes through, so a residual row-presence flap stays a diagnostic record.

A windowed record stamps the frame that **caused** the anomaly, not the frame that revealed it. `row_presence_flap` fires when the row returns but carries the frame the row went missing on, because every renderer observing the fault shares that frame while each returns on its own pull cadence. The `produced_at_ms` join therefore reaches the producer records for the same episode, and Doctor folds the renderers' copies of one fault into one incident.

`aggregate_reset` fires on the edge, without a window, when a spend tally drops from a non-zero figure straight to zero, and carries the prior figure and the pulled value. It covers only the monetary tallies, whose trailing-year figure never legitimately drops to zero in place; a provider mana window rolling to zero is normal. A transient zero that returns inside the window also records `aggregate_oscillation`.

### Per-frame checks

The consistency family checks each committed frame alone. Each check compares fields the view-model contract says must agree.

| Detector | Violation |
| --- | --- |
| `duplicate_pane_rows`, `duplicate_row_id` | Two rows share a pane id or a row id, breaking one pane, one row. This is the duplicated-card bug as a single-frame verdict |
| `status_count_mismatch` | A group's `status_counts` histogram disagrees with a re-tally of its rows. With hidden rows the declared counts may exceed the visible tally, because counts span the cap; with none they match exactly |
| `subagent_top_level_leak`, `subagent_double_render` | A rollup child surfaces as a top-level row, or renders both nested and top-level |
| `frameless_rows` | A fold with no pane frame still carries rows, which the contract makes unreachable because the pane frame admits every card |

### Real-world cross-checks

The writer thread re-verifies the latest roster against the world every `OBSERVE_CROSSCHECK_TTL` (5s). It reads only what the producer already published (the workspace pane frame through the stat-gated cache read) and process liveness, so the producer stays the only external puller ([state.md](./sidebar/state.md#renderers-the-producer-and-consumers)).

| Detector | Fires when |
| --- | --- |
| `row_pane_missing_from_frame`, `cards_exceed_panes` | A roster pane id is absent from the published frame, or the roster outnumbers the frame's panes. Runs only when the roster's fold stamp equals the frame's `produced_at_ms`, since a republish between fold and read is normal skew |
| `dead_pid` | A row's pane pid, checked through the process backend with the start-time pid-reuse guard, stays dead for `OBSERVE_DEADPID_CONFIRMATIONS` (2) consecutive passes. Platforms without process metrics skip it |
| `agent_card_without_process` | An agent row's live pane root authoritatively hosts no process of that kind for `OBSERVE_HOSTLESS_AGENT_CONFIRMATIONS` (2) consecutive passes. Unreadable or branching process trees stay silent, and dead roots are `dead_pid`'s verdict |

### Roles and cost

Every renderer runs the inline detectors on its own stream, because each node fuses its own events and gates its own frames, so a flap can exist only in the renderer that painted it. Only the elected elder's writer runs the cross-checks, so the room pays for the process and cache reads once. `crosscheck_enabled` on the writer thread makes that split, and every record carries the writer's `role` (`elder` or `consumer`).

The cost (one O(rows) signature pass per committed fold, a bounded channel, one throttled cross-check pass) is budgeted in [performance.md](./performance.md#everything-else).

### From anomaly to regression test

A `frame_anomaly` record carries enough to rebuild the stream that produced it: the frame stamp and pulled-truth scalars, row and pane identities, edge timestamps, the event summary, and the judging window.

Encode a confirmed anomaly as a test in [`observe/detect/tests.rs`](../../crates/rimz/src/sidebar/observe/detect/tests.rs). The `sig` and `row` builders there reconstruct the minimal committed sequence from the record's evidence (a warm frame, the offending frame, the restoring frame), and the assertion pins the recorded verdict and its evidence values alongside the verdicts that must stay absent; `row_presence_flap_stamps_the_frame_the_row_went_missing_on` has this shape. Build the fixture from the log record rather than the frame capture, because records outlive the eight-pair capture ring.

## Volume and retention

One condition can write several records, and one record can stand for many occurrences. Read counts through these rules.

- **Identity rate limit.** The sink admits one record per identity per 30-second window, where the identity is the kind plus its salient evidence fields ([`identity_key`](../../crates/rimz/src/diag/record.rs)). Suppressed repeats are counted onto the next record of that identity as `suppressed_since_last`, so a per-tick repeat collapses to one periodic line carrying the tally.
- **Kind ceiling.** Each kind admits at most 120 records per window. Drops past the ceiling are added to the next admitted record's `suppressed_since_last`.
- **Kinds that skip the identity limit.** `health_alert`, `renderer_panic`, `renderer_exit`, `producer_elected`, `producer_demoted`, `client_reaped`, `topology_write_rejected`, and the three width traces go through `emit_unlimited`: only the kind ceiling applies, so each occurrence is its own line.
- **The observer adds no limit of its own.** Its writer thread emits every draft it receives through the sink; `dropped_msgs` separately counts drafts the full channel shed.
- **One fault, many instances.** Every renderer records its own stream, so a published-frame problem records once per renderer while a node-local fusion or gating problem records on one. The count of distinct `instance_id` values inside an episode separates the two.
- **Captures churn faster than records.** The capture ring can turn over within hours in a busy room, so copy `diag-frames/` pairs out at the start of an investigation.

## Frame captures

`frame_rejected`, `pane_count_drop`, and `pane_carry_forward` records name their capture in `frames_ref`. Captures live in `diag-frames/`, a private `0700` ring beside the log that keeps the last eight prior/offending pairs. Each pair is one `frame.<at_ms>.<seq>.<kind>.json` file holding `prior` and `offending`, written as a disposable cache (atomic rename, no fsync).

Frame captures may contain command lines, cwd values, and other pane metadata. They sit behind the same local-filesystem privacy boundary as the rest of the workspace state directory.

## Reading the log

`rimz doctor` prints the latest twelve incidents from the current workspace's log, after dropping records at or before the history watermark. An incident folds records by identity. Cross-sidebar frame anomalies collapse only when session, build, event identity, and produced-frame stamp all match; other records, active and recovery edges alike, share a normalized identity and join an incident while they arrive within 60 seconds of its last record, so a fault that keeps repeating stays one incident and a recurrence after a quiet minute starts a new one.

Each incident keeps the source severity apart from its state (`investigate`, `contained`, `recovered`, `expected`) and impact (`alarm`, `warn`, `info`), plus record and distinct-observer counts, suppression totals, dropped-message counts, occurrence range, build staleness, and evidence references. Only investigative `warn` and `alarm` incidents count against Doctor's health tally. Incomplete evidence stays investigative, while `fetch_fold_stats` records and benign `hosted_carry_dropped` reasons are expected. The human report gives each investigative incident a table row and folds settled states into one counted line; `--json` carries every field of every incident.

`rimz doctor --clear` writes the watermark `doctor-cleared.json` beside the log. Doctor then hides diagnostics, the last incident marker, durable message failures, and multiplexer server-log records at or before its `cleared_at` timestamp. The logs, incident archive, and event log are untouched, so deleting the watermark restores the full retained history.

The log is plain JSONL. A kind census is the fastest orientation on an unfamiliar one (sample output):

```console
$ DIAG="$(rimz paths --json | jq -r .state_dir)"/diag.log.jsonl
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
jq 'select(.event.kind == "renderer_signal_death") | .event.stderr_excerpt' "$DIAG"             # crash tail
jq 'select(.event.kind == "renderer_exit") | .event.cause' "$DIAG"                              # exit cause
```

## Investigating an episode

One pass over the log answers an episode's three questions in order: what the user saw, where truth went wrong, and why.

1. **Build the timeline.** Run the timeline one-liner above, or `rimz doctor` for the recent incidents, and cluster records by `at_ms`. An episode reads as a burst across kinds. Copy the matching `diag-frames/` pairs out now, before the ring turns over.
2. **Locate the fault in published truth or the local fold.** Every `frame_anomaly` carries the pulled snapshot's scalars beside the rendered ones. For `row_presence_flap`, read the missing-edge frame stamp and `gap_evidence.pulled_row_present` and `pulled_pane_present`: false membership puts the gap in pulled truth, true membership in the renderer's committed fold. The distinct `instance_id` count is a second signal.
3. **Attribute the cause.** Producer records in the same window name it: the carry kinds as described above, `frame_rejected` for held implausible reads, `pane_count_drop` for published shrinks, `gate_hold` for renderer-side holds. The frame stamp (`produced_at_ms`) joins producer records, observer records, and capture filenames across the episode.
4. **Diff the captures.** Each capture holds the last good frame beside the offending one; `jq '{prior: (.prior.tabs | length), offending: (.offending.tabs | length)}'` shows a whole-tab omission at a glance.
5. **Encode the episode** as a detector test ([above](#from-anomaly-to-regression-test)).

Two shapes are worth recognizing.

**A long run of `gate_hold` with rule `agent_demoted_to_process`**, where every matching `gate_release` carries `via_escape_hatch: true`, means the rollup kept presenting the same live agent pane as a bare process with unchanged or missing foreground-command evidence. A real exit whose foreground command changed commits at once. A nearby `hosted_carry_dropped` means the producer's hosted-stamp carry declined, and its `reason` names the source guard; with no such record the carry restored the stamp, and the next step is instrumenting the rollup bind guard. The reject count is evidence only: the gate holds until `ACCEPT_REGRESSION_AFTER` (1s) has passed since the first reject, then releases through the escape hatch.

**A partial read**, where a pane source reports fourteen panes as six by omitting two whole tabs whose processes live. The carry-forward guard answers this before publication, so a healthy log shows `pane_carry_forward` under a steady roster. When the guard misses, the log reads:

| Records | Reading |
| --- | --- |
| `pane_count_drop` with eight removed panes | The shrink published; the capture pair preserves both frames |
| five `row_presence_flap`, gone to back in 2.26s, no `PaneClosed` events, `pulled_rows` back at full count | Every instance painted the flap; pulled truth had already recovered, so the rendered gap was the published partial frame propagating |

## Inspecting live card state

Card-content questions (a wrong gauge, a missing cost, a card resting in the wrong shape) are answered from the same read path the renderer runs, before any raw file is opened. Rendered-frame anomalies (flicker, duplicate rows, missing tabs) take the [episode workflow](#investigating-an-episode) instead.

`rimz workspace resolve <path>` prints the `workspace_id` for any project path. Every worktree of a repository resolves to the repository's own workspace, so a `rimz-worktrees/<branch>` checkout maps to the main repository's directories. State lives under `~/.rimz/ws/<workspace-dir>/` and runtime files under `$XDG_RUNTIME_DIR/rimz/ws/<workspace-dir>/`, where the directory name is `<basename>-<hex>` rather than the id; `rimz paths` prints both for the current project, a layout owned by [`disk/paths.rs`](../../crates/rimz/src/disk/paths.rs).

`rimz sidebar snapshot --json --no-produce` prints the fused `SidebarSnapshot` a node renders: the event-fresh rollup folded over the published pane frame plus the per-session sidecars ([state.md](./sidebar/state.md)). Run it inside the workspace, or pass `--workspace-id` from outside. `--no-produce` keeps the read passive, with no mux or git forks, so inspection never perturbs the room; without it the command may pay one producing refresh.

`rimz sidebar frame` prints the rendered frame through the same passive read when a producer frame exists, and falls back to one producing refresh otherwise. ANSI color is stripped when stdout is piped. `--expand` expands every card and reveals every capped worktree row, so large fleets do not clip.

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

An opted-in build reports the `warn!` and `error!` events RimZ raises, the panics it hits, sidebar render-worker signal deaths the supervisor observes, and the agent conditions it observes (rate limits, spend limits, provider overload, and other turn-ending API failures) at warning level.

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

The sidebar crash path uses the bridge too. A render panic records `renderer_panic` locally and reaches Sentry through its panic integration from inside the worker. A signal or abort death makes the supervisor write `renderer_signal_death` locally and emit one `error!` on `rimz::sidebar::crash` with the `sidebar.render_crash` operation tag, the signal or exit code, and the worker stderr tail ([`supervise.rs`](../../crates/rimz/src/sidebar_pane/supervise.rs)). Without the feature the supervisor still writes the local record and sends nothing.

`before_send` shapes every bridge event from its tracing target before it leaves the box:

- It tags `rimz::agent::turn_error` events `fault=agent` and every other event `fault=rimz`, so triage separates observed provider conditions from RimZ bugs.
- It pins a stable fingerprint (namespace, target, `operation`, and the static message), so an unsymbolicated release stack cannot split one callsite across issues and one resolve sticks.
- It allows five events per minute per fingerprint, so a `warn!` on a per-frame sidebar path cannot flood the project.

A panic or manual capture carries no target, so it keeps Sentry's default grouping and is never throttled.

### What stays off the wire

Sentry's default personal-data collection is off, and `before_send` strips the hostname.

Events carry RimZ error text and a stacktrace, the file paths that appear in those errors, the `rimz@<build>` release, the `command` and `build` tags, the `fault` class, the agent kind, the session id and turn-error class, the failed `operation`, and, for a failed account-usage probe, the `provider` tag and the request's host authority (never its path or query). The `workspace` tag scopes them to a repository, and the `rimz::trail` breadcrumbs trail an event with the steps before it.

Hook payloads, prompts, and transcripts are never forwarded. The transport is the same small rustls-backed `ureq` client RimZ uses for pricing, and it swallows network failures so they never surface on a RimZ path.
