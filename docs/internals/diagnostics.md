# Diagnostics

RimZ records what goes wrong as typed evidence, so a transient fault leaves something to debug after it heals. A roster that empties and refills over two seconds, a spend figure that blinks to zero, a card that outlives its pane: each is invisible an hour later. RimZ captures the evidence at the moment it happens and keeps it where the investigation will look. Correctness reads the store, CAS rules, and caches; diagnostics are evidence for a human, and no correctness path reads them back.

Evidence has two destinations. The default is a durable per-workspace log on the box, always on. The second is off-box error reporting to a Sentry project, compiled only into dev builds and dormant until a contributor opts in. Both are best-effort enrichment layered beside the running system: neither ever holds a correctness path, and the sidebar keeps unstructured tracing off by default.

Most of this doc is the local log and the [frame-stream observer](#the-frame-stream-observer) that feeds it. [Off-box error reporting](#off-box-error-reporting) is the smaller, opt-in sibling at the end.

## Where diagnostics land

Each workspace writes its diagnostic logs under its state directory, `~/.local/state/rimz/workspaces/<workspace-id>/`. The state directory is the right home because the job is investigation after the pane, mux session, or machine has gone away: runtime caches under `$XDG_RUNTIME_DIR` die with the session, while these records survive reboot like store records.

The `diag/` module owns a family of append-only JSONL surfaces that share one rotating streaming interface, [`JsonlLog`](../../crates/rimz/src/diag/rotating.rs): every file rotates at 1 MiB to one kept generation (`<name>.1.jsonl`), appends best-effort, and visits decoded records from the retained generation through the active file without collecting them internally. A failed write logs at debug rather than surfacing on a RimZ path, while missing files and malformed read lines contribute no records. Three of these surfaces carry the workspace identity through one [`DiagSink`](../../crates/rimz/src/diag.rs); a disabled sink is a no-op with the same methods, so the emitting callsites need no `#[cfg]` or branch.

| Surface | Location | Records | Owner |
| --- | --- | --- | --- |
| `diag.log.jsonl` | state dir | typed sidebar anomalies, rate-limited | this doc |
| `diag-frames/` | state dir, `0700` | prior/offending pane-frame pairs | [Frame captures](#frame-captures) |
| `notify.log.jsonl` | state dir | notification emits, bell decisions, unread transitions | [notifications.md](./sidebar/notifications.md) |
| `plugin-presence.log.jsonl` | state dir | Zellij presence-plugin keepalive telemetry | [below](#zellij-presence-plugin-rss) |
| `binding.log.jsonl` | runtime dir | pane-binding decisions | [sidebar.md](./sidebar/sidebar.md) |
| `topology-writer-conflict.json` | runtime dir | latest Zellij topology writer conflict sidecar | [multiplexers.md](./multiplexers.md#zellij-presence-channel) |

The rest of this doc is `diag.log.jsonl` and the observer that writes most of its rich records. The notification trace and pane-binding logs are diagnostic surfaces owned by their own subsystems; they share the rotating helper and nothing else.

### Zellij presence-plugin RSS

`plugin-presence.log.jsonl` records the Zellij presence plugin's keepalive telemetry from inside the Zellij server process: the exact `(loaded_at_ms, plugin_id)` generation, plugin build, WASM pages and bytes, uptime, completed and successful `run_command` replies, stale topology-writer rejections, genuine topology-publication failures, other command failures, and Zellij version ([`plugin_presence.rs`](../../crates/rimz/src/diag/plugin_presence.rs)). Doctor lists the presence-plugin panes currently loaded in the live Zellij session, joins each id to its newest telemetry generation unless the fresh topology writer identifies the exact generation, and classifies the result as active, rejected, or inactive against the desired plugin build. It prints the current and rotated log location for full generation history, and reports live-listing failures as unavailable rather than treating retained telemetry as live truth. Memory growth and stale-writer rejections are informational, multiple loaded plugins point to `rimz reload`, and a recent genuine failure delta remains actionable. Legacy rows remain readable and their missing build plus unsplit failure count stay unknown rather than becoming an active warning. Pages climbing while the Zellij server's RSS climbs attributes the leak to the plugin's WASM linear memory; pages flat while server RSS climbs attributes the growth to Zellij-native state driven by or adjacent to the plugin command path. It is local diagnostic state, bounded by rotation, with no off-box path.

## The record envelope

Every line in `diag.log.jsonl` is a `rimz.diag.v1` JSON object from [`diag/record.rs`](../../crates/rimz/src/diag/record.rs): the workspace id, session name, optional sidebar instance id, Unix milliseconds, severity, the writer's `build` id, an optional sink suppression count, and a tagged event.

The `build` id, also stamped on every published pane frame, is a digest of the writing executable read from its linker-provided build-id note ([`build_id.rs`](../../crates/rimz/src/build_id.rs)), so records and frames written by overlapping old and new builds during an upgrade stay distinguishable in place. A producer that reads a prior frame stamped by a different build additionally records a `mixed_build_writers` event, marking the upgrade-overlap window where a stale writer can regress fresh state. `rimz doctor` separately performs a point-in-time check of fresh sidebar heartbeats plus the running binary, and warns when more than one build id is actively writing or about to write the workspace.

Most records are anomaly-only: routine fetch ticks and fetch-fold totals, successful paints, and stable cache hits write nothing. The thresholded `tick_budget_breach` remains the performance incident when fetch or cache-refresh work is persistently over budget; retained `fetch_fold_stats` records from older builds remain readable and Doctor marks them expected. Sidebar width interactions are the intentional trace exception: `sidebar_width_intent` records each accepted or rejected keypress, `sidebar_width_nudge` records each serialized controller step, and `sidebar_width_settle` records learned feedback and terminal outcomes. Pure projection layers return diagnostics as data; impure callers append them through the `DiagSink`, so store reducers and the renderer gate carry no disk-write API. The observer's writer thread emits through the same sink, so every anomaly source shares one envelope and one file.

## Event taxonomy

The emitter is the triage pointer: producer kinds describe pane-source truth, renderer kinds describe one node's hold and refresh behaviour, projection kinds describe binding and grouping, and `frame_anomaly` is the rendered symptom as the [observer](#the-frame-stream-observer) judged it.

| Event | Emitter | Evidence |
| --- | --- | --- |
| `frame_rejected`, `frame_shrink_verified`, `pane_count_drop`, `pane_carry_forward`, `pane_carry_refuted`, `carry_forward_expired`, `hosted_carry_dropped`, `duplicate_pane_id`, `foreign_session_pane` | `sidebar::produce::panes` / `sidebar::frame` | Ok-but-empty frames, missing-own-pane reads, and pane drops with affected complete/partial views, managed pane identities, and mass-shrink evidence; liveness-guarded carried panes, forced re-pulls that refute an initial omission, carry expiry, hosted-agent carry declines with reason, duplicate ids, foreign-session leaks |
| `gate_hold`, `gate_release`, `fetch_failure`, `health_alert`, `link_alert`, `producer_elected`, `producer_demoted`, `renderer_panic`, `renderer_exit` | `sidebar_pane::app` | Renderer-side holds, degraded refresh episodes, remote-link degraded/recovered episodes, producer handoff, panics that would otherwise disappear with the pane, clean self-close and give-up exits with cause |
| `sidebar_width_intent`, `sidebar_width_nudge`, `sidebar_width_settle` | `sidebar_pane::app` | Intent verdicts, serialized controller nudges, learned feedback, and terminal settle outcomes for `a`/`d` width control |
| `tick_budget_breach` | `sidebar::meter` via the fetch worker and cache refresher | sustained over-budget producer ticks: worst in-process wall time, mux wait, fold bytes, spawns, declared budgets, streak length, episode `since_ms`, and recovery |
| `renderer_signal_death`, `renderer_orphan_reaped` | `sidebar_pane::supervise` | Abnormal signal or non-panic exit from the render worker with the captured stderr tail; supervisor reap after fresh mux listings omit the owning pane, with pane id and worker pid |
| `pane_cache_divergence`, `sidebar_orphan_reaped` | `reload` / `sidebar repair` | A fresh pane cache omitted a sidebar process that authoritative mux truth proved alive; a process reap after two authoritative omissions, with pane id, pid, both observation stamps, and whether SIGKILL was required |
| `row_conflict`, `newborn_quarantined`, `group_migration` | `store::snapshot::view` via `sidebar::enrich` and renderer state diffing | duplicate agent identity suppression, newborn known-command unknown-cwd quarantine, cwd-driven pane moves across group boundaries |
| `frame_anomaly` | `sidebar::observe` writer thread | rendered-stream detector verdicts — flaps, oscillations, resets, per-frame consistency violations, and elder cross-checks — each carrying its detector key, evidence, frame stamp, and the writer's elder/consumer role |
| `mixed_build_writers` | `sidebar::produce::panes` | a prior published frame stamped by a different build than the producing process — the historical upgrade-overlap window where stale writers regressed fresh state; `rimz doctor` owns the live-concurrency warning |
| `topology_writer_changed`, `topology_write_rejected` | `sidebar::presence` | Zellij presence topology writer generation flips and rejected stale writer attempts, with plugin id, loaded-at generation, accepted writer, reject count, and the conflict sidecar for doctor; a legacy `plugin_id 0` rejection storm during build overlap is expected, correctly rejected, and needs no receiving-side code change |

The carry kinds attribute the pane-source fault precisely. `pane_carry_forward` marks a mux omission that survived a forced direct re-pull while process liveness proved the omitted panes alive: the source under-reported, the producer carried the panes, the record is `warn`, and the prior/offending frame pair is captured. `pane_carry_refuted` marks an initial listing that the forced re-pull corrected: the first read lied and the truth healed within one produce, so the record is `info` with no frame pair, because the log line is the evidence. `carry_forward_expired` marks liveness proof running out, when a carried pane drops after `PANE_CARRY_TTL` ([`timing.rs`](../../crates/rimz/src/sidebar/timing.rs)). `hosted_carry_dropped` marks a prior hosted lazy-agent stamp that the producer declined to restore, with `reason` naming a positive absence probe, a pane start regression, a foreground-kind mismatch, or TTL expiry. Doctor marks `probe_reports_absent` and `carry_expired` expected because positive absence or expiry makes dropping the stale stamp correct; `start_regressed` and `foreground_kind_mismatch` remain investigative evidence of contradictory identity. See [sidebar.md → honest reads](./sidebar/sidebar.md#honest-reads-across-a-mux-hiccup) for the guard these records come from.

Severity follows the event: an active `health_alert`, `link_alert`, or `tick_budget_breach` is `warn` and its recovery edge is `info`; `renderer_panic` and `renderer_signal_death` are `error`; `renderer_exit` is `info` for `self_close_empty_tab` and `warn` for `degraded_gave_up`; the rest are `warn` for a live fault and `info` for a verified-benign transition. The mapping is pinned by test in [`record/tests.rs`](../../crates/rimz/src/diag/record/tests.rs).

## The frame-stream observer

Every sidebar renderer carries an observer that watches its own committed frame stream, the fused and gated `SidebarSnapshot` sequence the renderer actually paints, and records an evidence-rich `frame_anomaly` when the stream misbehaves: a roster that empties and refills, a duplicated card, a phantom row, a value that bounces between two figures, a card whose pane or process is gone. The observer is a diagnostic instrument beside the render path. It reads the stream and emits records, and the rendered frame is byte-identical whether the observer is present or absent.

The observer exists because this bug class is transient and self-healing, so a flap visible for two seconds leaves nothing to debug later. Each anomaly caught live becomes a synthetic regression test over recorded signatures ([below](#from-anomaly-to-regression-test)), and a detection that proves reliable graduates into a prevention guard: renderer-side in the [commit gate](../../crates/rimz/src/sidebar_pane/app/gate.rs) or at the source in producer frame validation. Three graduations are in place. The pane carry-forward guard came from the partial-read flap: the observer recorded it, the episode became a recorded-signature test, and the producer now repairs the fault before publication ([sidebar.md → honest reads](./sidebar/sidebar.md#honest-reads-across-a-mux-hiccup)). The spend blink graduated the same way: the producer keeps the prior non-zero spend cache across an empty transcript-discovery pass, and the commit gate carries the last non-zero dashboard spend across a bounded consumer-side zero read. The shared-pane identity flap graduated at the source: the observer recorded one pane card alternating between two co-resident sessions, and the producer now pins same-pane registration ties to a stable primary.

[`sidebar/observe`](../../crates/rimz/src/sidebar/observe.rs) owns the signature, the detectors, and the writer thread. The anomaly vocabulary is the `frame_anomaly` arm of the [record schema](../../crates/rimz/src/diag/record.rs), and the windows and cadences live in [`timing.rs`](../../crates/rimz/src/sidebar/timing.rs) under the `OBSERVE_*` prefix.

### The commit-point contract

Every mutation of the rendered snapshot flows through one chokepoint in the serve loop (`observe_commit` in [`app::loop_state`](../../crates/rimz/src/sidebar_pane/app/loop_state.rs), reached by both the pull path and the event-overlay path), so the observer hooks exactly once per committed fold. After each commit it reduces the snapshot to a compact `FrameSig`: row identities sorted by row id, card kinds, pane ids and pids, group keys and rendered order, watched values, own-view and active-event context, and the gate and health streaks. It then runs every pure detector inline, in microseconds, before the loop moves on.

- The row signature carries no renderer-local presentation state (the unread stamp, selection, scroll), so presentation churn reads as the same row content; group signatures also carry rendered row order for the scoped order-flap detector.
- Each signature carries compact identities and scalars from the un-fused pulled snapshot (`pulled_rows`, pulled row and pane membership, the pulled frame stamp, and pulled dashboard aggregates), so every record shows whether the producer's published truth already held the anomaly or that instance's fusion and gating introduced it. A `row_presence_flap` preserves the missing edge's frame stamp and subject membership rather than inferring provenance from the later restoring frame.
- Detection emits small anomaly drafts over a bounded channel to one background writer thread. A full channel drops the draft and stamps the accumulated drop count on the next record that gets through, and the render thread never waits on the observer.

The windowed family holds during `OBSERVE_WARMUP` after the first frame-backed commit, because startup and reload transients are expected and a re-exec re-arms the grace, and while the committed fold is frameless before the first pane frame. A frameless incoming fold over a frame-backed render is gate-held, records `gate_hold`, and the observer keeps observing the prior frame; a committed frameless fold that still carries rows fires `frameless_rows` immediately. Windows measure receiver-clock time, like the event store's TTLs.

### Windowed detectors

The windowed family recognizes back-and-forth motion a user would describe (rendered, gone, rendered again), each with its own window constant. Under heavy-fleet mux enumeration churn a published pane frame can briefly drop and re-list one pane; carry-and-verify mitigates that gap while letting genuine closes through, so a residual row-presence flap remains a diagnostic record rather than a wrong frame.

| Detector | Window | Fires on | Stays quiet when |
| --- | --- | --- | --- |
| `roster_flap` | `OBSERVE_ROSTER_FLAP_WINDOW` | a populated roster empties while own-view still counts working siblings, then refills inside the window | active `PaneClosed` events cover every vanished row's pane (a genuinely emptied tab is [self-close](./sidebar/sidebar.md#self-close) territory) |
| `row_presence_flap`, `short_lived_row` | `OBSERVE_ROW_FLAP_WINDOW` | one row (keyed by row id) disappears and returns inside the window, or a row is born and vanishes inside it (the phantom-card case, group key recorded) | a `PaneClosed` justifies the absence, the row's group had its idle/process tail hidden at either edge (ranking churn legitimately rotates rows through the cap), or the pane was rebound to a new identity (detected by pane continuity; `group_migration` only covers cwd-changing cross-group moves) |
| `value_oscillation` | `OBSERVE_VALUE_OSC_WINDOW` | a watched per-row value returns to its exact prior figure after differing: status, context %, token total, group key (the worktree/`external` bounce), or model | the field's first appearance (enrichment warm-up is `None` to value, never an oscillation) |
| `aggregate_oscillation` | `OBSERVE_AGGREGATE_OSC_WINDOW` | a dashboard figure returns to its prior value after differing: cockpit or workspace spend year, provider spend year, or a provider mana window %, including spend to zero to back | first appearance stays quiet; the record carries `pulled_via` so a producer-published zero and a consumer-read zero split in the evidence |
| `order_flap` | `OBSERVE_ORDER_FLAP_WINDOW` | rendered row order inside one group returns to its prior order after differing, with unchanged visible membership | the visible set changed, as in a real re-rank or a cap tail rotation |
| `status_churn` | `OBSERVE_STATUS_CHURN_WINDOW` | four or more status transitions on one row inside the window, a rate verdict deliberately wider than the oscillation window | a quick `running → idle → running` turn boundary, which is normal pace |

A windowed record stamps the frame that caused the anomaly rather than the frame that revealed it. `row_presence_flap` fires when the row returns and carries the frame the row went missing on, because that missing edge is what the producer records describe and what every renderer observing the fault shares; each renderer returns on its own pull cadence, so the recovery frame differs between them. The `produced_at_ms` join [below](#investigating-an-episode) therefore reaches the producer records for the same episode, and Doctor folds the renderers' copies of one fault into one incident.

A spend tally that drops from a non-zero figure straight to zero fires `aggregate_reset` immediately on the edge rather than waiting out a window, carrying the prior figure and the pulled value. It covers only the monetary tallies (cockpit, workspace, provider spend), whose trailing-year figure never legitimately drops to zero in place; provider mana windows are excluded because their zero is a normal rate-limit roll. The reset complements the spend-blink prevention above: it catches a reset the carry did not heal, while a transient zero that returns inside the window still records `aggregate_oscillation`.

### Per-frame checks

The consistency family checks each committed frame alone, detecting and logging immediately with no window. Each compares fields the view-model contract says must agree.

- **One pane, one row.** Two rows sharing a pane id (`duplicate_pane_rows`) or two rows sharing a row id (`duplicate_row_id`) violate the binding rule directly. This is the duplicated-cards bug as a single-frame verdict.
- **Counts agree.** Each group's `status_counts` histogram re-tallies from its rows (`status_count_mismatch`). With hidden rows the declared counts may exceed the visible tally, because counts span the cap by design; with no hidden rows they match exactly.
- **Children stay nested.** A rollup child renders inside its parent's card; a child id surfacing as a top-level row (`subagent_top_level_leak`) or rendered both nested and top-level (`subagent_double_render`) is a projection error.
- **Rows imply a frame.** The pane frame admits every card, so rows on a frameless fold (`frameless_rows`) are unreachable by contract.

### Real-world cross-checks

The writer thread re-verifies the latest roster against the world on its own cadence (`OBSERVE_CROSSCHECK_TTL`), reading only what the producer already published (the workspace pane frame through the stat-gated cache read) and process liveness. The observer adds no mux call of its own; the producer remains the only external puller ([state.md → The node model](./sidebar/state.md#the-node-model)).

- **Cards fit the frame.** Every roster pane id appears in the published frame (`row_pane_missing_from_frame`), and the roster never exceeds the frame's pane count (`cards_exceed_panes`). The comparison runs only when the roster's fold stamp equals the frame's `produced_at_ms`; a producer republish between fold and read is normal skew.
- **PIDs are alive.** A row's pane pid is checked through the process backend with the start-time pid-reuse guard (`dead_pid`). A pid must stay dead across `OBSERVE_DEADPID_CONFIRMATIONS` consecutive passes before it logs, so a just-exited process the next frame removes leaves no record, and each dead-pid episode reports once. Platforms without process metrics skip the check.

### Roles and cost

Every renderer runs the inline detectors on its own stream, because each node fuses its own events and gates its own frames, so a flap exists only in the renderer that painted it. The elected elder's writer additionally runs the real-world cross-checks, so the room pays the process and cache reads once; the split is one policy function on the writer thread, and every record carries the writer's elder-or-consumer role. The cost envelope (one O(rows) signature pass per committed fold, a bounded channel, one throttled cross-check pass) is budgeted in [performance.md → The cost map](./performance.md#the-cost-map).

### From anomaly to regression test

A `frame_anomaly` record carries enough to rebuild the stream that produced it: the frame stamp and pulled-truth scalars, row and pane identities, edge timestamps, the event summary, and the judging window. A confirmed anomaly becomes a test in [`observe/detect/tests.rs`](../../crates/rimz/src/sidebar/observe/detect/tests.rs) under `mod recorded_episodes`. A signature builder reconstructs the minimal committed sequence from the record's evidence (a warm frame, the offending frame, the restoring frame), and the assertion pins the exact recorded verdict down to its evidence values, alongside the verdicts that must stay absent. Encode the fixture from the log record, not the frame capture: records survive rotation, while captures churn through the eight-pair ring. The module comment cites the source workspace and date, so a failing replay points back at its originating episode.

## Volume and retention

One condition can write several records, and one record can stand for many occurrences. Read counts through these rules.

- Anomaly emissions rate-limit per identity, the record's kind plus its salient evidence fields ([`identity_key`](../../crates/rimz/src/diag/record.rs)), over a thirty-second window, flushing the suppressed counter onto the next record of that identity, so a steady per-tick repeat collapses to one periodic line carrying the tally. Sidebar width traces bypass identity suppression so each interaction remains ordered, while the shared per-kind ceiling still bounds a pathological burst.
- The observer's writer thread emits every draft it receives and rate-limits nowhere of its own, so the sink's identity window above is the one cooldown a `frame_anomaly` passes: repeats on one row collapse into a periodic line carrying the tally, while a fault on a different row reports immediately. A separate `dropped_msgs` counter on the record reports drafts the bounded channel shed under load.
- Every renderer instance records its own stream, so one published-frame problem records once per instance while a node-local fusion or gating problem records on one. The distinct `instance_id` count inside an episode separates the two.
- The frame-capture ring churns within hours in a busy room, so copy `diag-frames/` pairs out at the start of an investigation. The log records carry enough evidence to reconstruct an episode after its captures rotate.

## Frame captures

The captures that pair with the carry, drop, and reject records live in `diag-frames/`, a private `0700` ring beside the log that keeps the last eight prior/offending pane-frame pairs. Each pair is one `frame.<at_ms>.<seq>.<kind>.json` file, written as a disposable cache (atomic rename, no fsync), so a capture joins its log record by timestamp and kind. Frame captures may contain command lines, cwd values, and other pane metadata; they receive the same local-filesystem privacy boundary as the rest of the workspace state directory.

## Reading

`rimz doctor` applies the history watermark first, then prints the latest twelve evidence incidents for the current workspace. Cross-sidebar frame anomalies collapse only when session, build, event identity, and produced-frame stamp match; other active and recovery records share a normalized identity within a 60-second episode, with a later recurrence starting a new incident. Link records normalize by their stable episode `since_ms`, so a changing tier at the Good recovery edge closes the active degraded or bad incident while a new `since_ms` starts a recurrence. Each row preserves source severity separately from `investigate`, `contained`, `recovered`, or `expected` state and `alarm`, `warn`, or `info` impact, plus record and distinct-observer counts, suppression totals, dropped-message counts, occurrence range, build staleness, and retained evidence references. Only investigative warn/alarm incidents affect Doctor's health tally; incomplete evidence stays investigative, while retained routine fetch-fold totals and positively benign hosted-carry drops are expected. The human report gives a table row to each investigative incident and folds the settled states into one counted line naming their kinds, so a settled incident stays available without competing for the reader's attention; `--json` carries every field of every incident. The file stays plain JSONL for direct inspection.

`rimz doctor --clear` writes `doctor-cleared.json` beside the diagnostic log and filters diagnostics, the last incident marker, durable message failures, and multiplexer server-log records at or before its `cleared_at` timestamp. The JSONL logs, incident archive, and event log stay untouched; deleting the watermark restores the full retained history.

```sh
DIAG=~/.local/state/rimz/workspaces/<workspace-id>/diag.log.jsonl
tail -f "$DIAG"
jq -r '[(.at_ms|tostring), .severity, .event.kind, (.instance_id // "-")] | join(" ")' "$DIAG"  # episode timeline
jq -r '.event.kind' "$DIAG" | sort | uniq -c | sort -rn                                         # kind census
jq 'select(.at_ms > 1781070540000 and .at_ms < 1781070550000)' "$DIAG"                          # window slice
jq 'select(.event.kind == "frame_anomaly") | .event.anomaly' "$DIAG"                            # observer evidence
jq 'select(.event.kind == "renderer_signal_death") | .event.stderr_excerpt' "$DIAG"             # crash tail
jq 'select(.event.kind == "renderer_exit") | .event.cause' "$DIAG"                              # clean exit cause
jq 'select(.event.kind == "gate_hold")' "$DIAG"
```

## Investigating an episode

One pass over the log answers an episode's three questions in order: what the user saw, where truth went wrong, and why.

1. **Build the timeline.** Run the timeline one-liner above (or `rimz doctor` for the tail) and cluster records by `at_ms`; an episode reads as a burst across kinds. Copy the matching `diag-frames/` pairs out now, before the ring churns.
2. **Locate the fault: published truth or local fold.** Every `frame_anomaly` carries the pulled snapshot's scalars beside the rendered ones. For `row_presence_flap`, read the missing-edge frame stamp and `pulled_row_present`/`pulled_pane_present`: false membership attributes the gap to pulled truth, while true membership attributes it to the renderer's committed fold. Older records without gap evidence retain the conservative restoring-frame summary. The distinct `instance_id` count provides a second attribution signal.
3. **Attribute the cause.** Producer records in the same window name it: the carry kinds with the semantics above, `frame_rejected` for held implausible reads, `pane_count_drop` for published shrinks, `gate_hold` for renderer-side holds. The frame stamp (`produced_at_ms`) joins producer records, observer records, and capture filenames across the episode.
4. **Diff the captures.** Each capture file holds the last good frame beside the offending one; `jq '{prior: (.prior.tabs | length), offending: (.offending.tabs | length)}'` shows a whole-tab omission at a glance.
5. **Encode the episode.** A confirmed anomaly becomes a synthetic regression test over recorded signatures ([From anomaly to regression test](#from-anomaly-to-regression-test)).

A long run of `gate_hold` for `agent_demoted_to_process` where every matching `gate_release` carries `via_escape_hatch: true` means the rollup repeatedly presented the same live agent pane as a bare process with unchanged or missing foreground-command evidence. Real in-place exits whose foreground command changed commit immediately. A nearby `hosted_carry_dropped` record means the producer's hosted-stamp carry declined and its `reason` names the source guard; no nearby record means the carry restored the stamp and the next investigation instruments the rollup bind guard instead. Reject count is evidence only: the gate holds until the wall-clock `ACCEPT_REGRESSION_AFTER` settling window ends, wakes the render loop at that deadline, and forces one non-skippable fold so the same-command exit releases without waiting for a long data tick.

A recorded partial-read episode reads like this, where a pane source reports fourteen panes as six, omitting two whole tabs while their processes live.

| Records | Reading |
| --- | --- |
| `pane_count_drop` with eight removed panes | the shrink published; the capture pair preserves both frames |
| 5× `row_presence_flap`, gone→back 2.26s, no `PaneClosed` events, `pulled_rows` back at full count | every instance painted the flap; pulled truth had already recovered, so the rendered gap was the published partial frame propagating |

The carry-forward guard answers this shape before publication, so the same fault now records `pane_carry_forward` under a steady roster; `row_presence_flap` records beside carry records mean the guard missed. This episode persists as the recorded-episode regression test in [`observe/detect/tests.rs`](../../crates/rimz/src/sidebar/observe/detect/tests.rs).

## Inspecting live card state

Card-content questions (a wrong gauge, a missing cost, a card resting in the wrong shape) are answered from the same read path the renderer runs, before any raw file is opened. Rendered-frame anomalies (flicker, duplicate rows, missing tabs) take the [episode workflow](#investigating-an-episode) instead.

`rimz workspace resolve <path>` names the workspace for any project path and prints its `workspace_id`. Every worktree of a repository resolves to the repository's own workspace, so a `rimz-worktrees/<branch>` checkout maps to the main repository's state and runtime directories. State lives under `$XDG_STATE_HOME/rimz/workspaces/<id>/` and runtime files under `$XDG_RUNTIME_DIR/rimz/<id>/`; the layout is owned by [`store/paths.rs`](../../crates/rimz/src/store/paths.rs).

`rimz sidebar snapshot --json --no-produce`, run from anywhere inside the workspace (or with `--workspace-id` from outside it), prints the fused `SidebarSnapshot` a node renders: the event-fresh rollup folded over the published pane frame plus the per-session sidecars ([state.md](./sidebar/state.md)). `--no-produce` keeps the read passive (no mux or git forks), so inspection never perturbs the room; omit it to pay one producing refresh when no fresh producer cache exists.

```sh
rimz sidebar snapshot --json --no-produce | jq '
  .agents[] | select(.worktree_branch == "truecolor") | {
    agent_id, kind, status, context_pct, total_tokens, compaction_count,
    tokens: .context.tokens, cost: .context.cost.total_cost_usd,
    observed_at: .context.observed_at }'
```

The split inside that one object is the provenance map. Bare row fields (`status`, `context_pct`, `total_tokens`, `compaction_count`) are rollup truth derived from hooks and transcript tails, while everything under `context` is the latest statusline or app-server sidecar, stamped `observed_at`. A figure wrong in only one of the two halves names the half to debug.

Raw sidecars confirm what a producer actually wrote. Filenames are digests because session ids are free strings, so scan by record content.

```sh
cd "$XDG_RUNTIME_DIR/rimz/<workspace_id>"
jq -r '[.kind, .agent_id, .context.session_name] | @tsv' agent_context/ctx.*.json   # find a session's record
jq . agent_context/ctx.<digest>.json                                                # the full record
```

The store's own view without the sidecar fold is the published checkpoint plus log tail ([store.md](./store.md#runtime-projection)); comparing it against the snapshot attributes a wrong figure to the rollup, the sidecar, or the fold. The renderer-side derivations a card dispute usually hinges on live on the view model: the gauge-source preference is [`AgentCard::context_gauge_percent`](../../crates/rimz/src/store/snapshot/row.rs) and the card-shape predicates sit in the agent-card section ([`agent_card/mod.rs`](../../crates/rimz/src/sidebar_pane/render/sections/agent_card/mod.rs)).

## Off-box error reporting

Off-box error reporting routes RimZ diagnostics to a Sentry project, so an operator watches a fleet's health without tailing every box. The reporting code compiles only under the dev-only `sentry` cargo feature; a shipped binary omits it entirely and any `[sentry]` config is inert. In an opted-in dev build it captures the warnings and errors RimZ raises, the panics it hits, the sidebar render-worker signal deaths the supervisor observes, and the agent conditions it observes (rate limits, spend limits, provider overload, and other turn-ending API failures, reported at warning level). Reporting is best-effort enrichment: it never holds a correctness path, and it is dormant until a DSN resolves.

Without the feature, [`observability`](../../crates/rimz/src/observability.rs) is a no-op with the same surface, so `main.rs` and the CLI dispatch are identical in both builds. With the feature on, [`observability/reporting.rs`](../../crates/rimz/src/observability/reporting.rs) is the live impl.

### Opting in

Reporting exists in builds compiled with `--features sentry`. `cargo xtask install-dev` is the contributor shortcut: it installs the optimized `profiling` host profile with the feature on and, when `sentry-cli` is installed and authenticated, best-effort uploads that binary's debug files. Reporting turns on when a DSN resolves from `RIMZ_SENTRY_DSN` or the per-machine `[sentry]` config; the env value wins, and an empty value counts as unset. The DSN lives per-machine, never in the committed `.rimz/config.toml`, so a clone or pull cannot redirect a contributor's telemetry, and the DSN stays off the [project trust surface](./harness/trust.md). `RIMZ_SENTRY_ENVIRONMENT` (or `[sentry] environment`) tags the deployment; unset, it defaults by build profile, so an installed release reports as `production` while dev, profiling, and CI builds report as `development`, keeping contributor noise off the production dashboard. The config shape lives in [configuration.md → Off-box error reporting](../guide/configuration.md#off-box-error-reporting); the data boundary lives in [security.md → Off-box error reporting](../guide/security.md#off-box-error-reporting).

With no DSN, no client is created and RimZ makes no network calls. A malformed DSN yields `Reporting::InvalidDsn`, logged with the fix once the subscriber is live and otherwise inert, so a telemetry typo never degrades or blocks a command.

### One init point covers every process

`main` creates the Sentry client once, before the tracing subscriber, and holds the guard for the whole process; the guard flushes pending events on drop, which covers the short-lived hook subprocesses. Every RimZ subcommand runs through that one `main`, the CLI, the `hooks feed` subprocess where agent conditions are observed, and the `sidebar serve` loop, so a single init covers them all. The wasm presence plugin is a separate binary with no HTTP stack and reports nothing.

A live workspace pin (`RIMZ_WORKSPACE_ID`) becomes a `workspace` scope tag, so one machine-wide DSN still filters per repository. Once the command parses, `set_command_scope` adds a `command` tag and a `build` tag (the same [build id](../../crates/rimz/src/build_id.rs) the diagnostics log stamps) plus a structured `rimz` context grouping the command, build, and, when a process serves exactly one, the agent kind and session, so every event names the process that raised it. The client reports under the `rimz@<build id>` release, the same executable digest, so one identity tracks regressions across builds, makes `resolve --in-next-release` reopen on a real new build, and keys any uploaded debug files. The profiling profile embeds DWARF line tables and frame pointers in one self-contained binary, so the uploaded file matches the GNU build-id that release carries.

### The tracing bridge is the capture path

RimZ already speaks diagnostics through `tracing`. The Sentry layer joins the subscriber alongside the stderr formatter and turns each `warn!`/`error!` into a Sentry event whose level mirrors the tracing level, except the sidebar health target `rimz::sidebar::health` stays local because the durable `health_alert` and `renderer_exit` records already carry that episode. Each deliberate breadcrumb seed (an `info!` on the dedicated `rimz::trail` target) becomes a breadcrumb that rides along on the next event, so a warning arrives with the trail that led to it. Gating breadcrumbs on that one target is an allowlist: an unmarked `info!` is ignored, so a stray field (a socket path, a cwd) never rides off-box as breadcrumb data. The Sentry layer carries its own `INFO` filter, which keeps the global max-level hint at `INFO`, so `debug!`/`trace!` are never constructed and the breadcrumb trail stays a cold-path concern the `sidebar serve` hot loop never feeds. Callsites attach a searchable `tags.operation` and pass the error as `&dyn Error`, so an event names the operation that failed and carries the error's exception and a stacktrace. When no DSN resolves the layer is omitted entirely, and behaviour is byte-for-byte the prior subscriber.

Agent-generated conditions ride the same path. When the hook lifecycle merges a fresh turn-error marker (`merge_turn_error_marker` in [`cli/hooks/lifecycle/transcript.rs`](../../crates/rimz/src/cli/hooks/lifecycle/transcript.rs) reports the merge changed state), it emits one `warn!` under the `rimz::agent::turn_error` target carrying the agent kind and the [`TurnErrorClass`](../../crates/rimz/src/agents/context.rs) (`PausedRateLimit`, `PausedSpendLimit`, `PausedOverloaded`, or `Failed`). Gating on the transition keeps it to one event per condition rather than one per poll, and the warning level marks it as observed, not a RimZ fault.

The sidebar crash path uses the same bridge in dev builds only. `rimz sidebar serve` supervises a re-execed render worker; a normal render panic records `renderer_panic` locally and rides Sentry's panic integration from inside the worker, while a signal or abort death makes the supervisor write `renderer_signal_death` locally and emit one `error!` under the `rimz::sidebar::crash` target with the `sidebar.render_crash` operation tag, the signal or exit code, and the worker stderr tail ([`sidebar_pane/supervise.rs`](../../crates/rimz/src/sidebar_pane/supervise.rs)). A release install compiles without the `sentry` feature, so the supervisor still writes the local diagnostic and sends nothing off-box.

`before_send` shapes every bridge event from its tracing target before it leaves the box. It tags the `rimz::agent::turn_error` target `fault=agent` and every other event `fault=rimz`, so triage filters observed provider conditions from RimZ bugs. It pins a stable fingerprint (namespace, target, `operation`, and the static message), so the unsymbolicated release stack can no longer split one callsite across issues and one resolve sticks. And it enforces a per-fingerprint budget of at most five events per minute per group, so a `warn!` on a per-frame sidebar path can never flood the channel with tens of thousands of identical events. A panic or a manual capture carries no target, so it keeps Sentry's default grouping and is never throttled.

### What stays off the wire

Personal data is off by default and the hostname is stripped in `before_send`. Events carry RimZ error text and a stacktrace, the file paths that appear in those errors, the `rimz@<build>` release, the running `command` and `build` id, the `fault` class, the agent kind, the session id and turn-error class, the `operation` that failed, and, for a failed account-usage probe, the `provider` tag and request's host authority (never its path or query). The `workspace` tag scopes them to a repository, and the curated breadcrumb seeds (the `rimz::trail` `info!` lines on the error-prone cold paths) trail an event with the steps before it. Hook payloads, prompts, and transcripts are never forwarded. A network failure is swallowed by the transport (the same small rustls-backed `ureq` client RimZ uses for pricing) and never surfaces on a RimZ path.
