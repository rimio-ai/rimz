# Sidebar Observer

> The node model the observer instruments lives in [state.md](./state.md); presence, ranking, and the render loop live in [sidebar.md](./sidebar.md). This doc owns the observer: what it watches and what counts as an anomaly. The durable channel its records land in — envelope, file, retention, rate limiting, inspection — lives in [diagnostics.md](./diagnostics.md).

Every sidebar renderer carries an observer that watches its own committed frame stream — the fused, gated `SidebarSnapshot` sequence the renderer actually paints — and records an evidence-rich `frame_anomaly` diagnostic when the stream misbehaves: a roster that empties and refills, a duplicated card, a phantom row, a value that bounces between two figures, a card whose pane or process is gone. The observer is a diagnostic instrument beside the render path: it reads the stream and emits records, and the rendered frame is byte-identical with the observer present or absent.

The observer exists because this bug class is transient and self-healing — a flap visible for two seconds in a live room leaves nothing to debug an hour later. Each anomaly caught live becomes a synthetic regression test over recorded signatures ([below](#from-anomaly-to-regression-test)), and a detection that proves reliable graduates into a prevention guard — renderer-side in the [commit gate](../../crates/rimz/src/sidebar_pane/app/gate.rs) or at the source in producer frame validation — detection first, prevention once trusted. The pane carry-forward guard ([sidebar.md → honest reads](./sidebar.md#honest-reads-across-a-mux-hiccup)) is such a graduation: the observer recorded the partial-read flap, the episode became a recorded-signature test, and the producer now repairs that fault before publication.

## The commit-point contract

Every mutation of the rendered snapshot flows through one fold chokepoint in the serve loop (`LoopState::fold_outcome` — both the pull path and the event-overlay path commit there), so the observer hooks exactly once. After each commit it reduces the snapshot to a compact `FrameSig` — row identities sorted by row id, card kinds, pane ids and pids, group keys, watched values, own-view and active-event context — and runs every pure detector inline, in microseconds, before the loop moves on.

- The signature is order-insensitive and carries no renderer-local presentation state (the `unread` stamp, selection, scroll), so presentation churn reads as the same frame.
- Each signature carries two scalars from the un-fused pulled snapshot (`pulled_rows` and the pulled frame stamp), so every record shows whether the producer's published truth already held the anomaly or fusion/gating introduced it — the first question of any investigation, answered inside the record.
- Detection emits small anomaly drafts over a bounded channel to one background writer thread; a full channel drops the draft and stamps the drop count on the next record that gets through. The render thread proceeds without waiting on the observer.

[`sidebar/observe`](../../crates/rimz/src/sidebar/observe.rs) owns the signature, the detectors, and the writer thread; the anomaly vocabulary is the `frame_anomaly` arm of the [diagnostic record schema](./diagnostics.md), and the windows and cadences live in [`timing.rs`](../../crates/rimz/src/sidebar/timing.rs) under the `OBSERVE_*` prefix.

## Windowed detectors

The windowed family recognizes back-and-forth motion a user would describe — rendered, gone, rendered again — with a window per detector class. Windowed detection holds during `OBSERVE_WARMUP` after the first frame-backed commit (startup and reload transients are expected, and a re-exec re-arms the grace) and while the committed fold is frameless before the first pane frame. A frameless incoming fold over a frame-backed render is gate-held, records `gate_hold`, and the observer keeps observing the committed prior frame; a committed frameless fold that still carries rows fires `FramelessRows` immediately. Windows measure receiver-clock time, like the event store's TTLs.

In tmux poll mode, a row born late in warmup can still look short-lived on the first post-warmup frame if it closes before any pane-close event reaches the renderer; that is the same accepted diagnostic ambiguity as any sub-window poll-only close.

| Detector | Window | Fires on | Stays quiet when |
| --- | --- | --- | --- |
| Roster flap | `OBSERVE_ROSTER_FLAP_WINDOW` | a populated roster empties while `own_view` still counts working siblings, then refills inside the window | active `PaneClosed` events cover every vanished row's pane (a genuinely emptied tab is [self-close](./sidebar.md#self-close) territory) |
| Row presence flap | `OBSERVE_ROW_FLAP_WINDOW` | one row (keyed by row id) disappears and returns inside the window; or a row is born and vanishes inside it (the phantom-card case, group key recorded) | a `PaneClosed` event justifies the absence, or the row's group had its idle/process tail at the visible cap at either edge — ranking churn legitimately rotates rows through `WORKTREE_ROW_CAP` |
| Value oscillation | `OBSERVE_VALUE_OSC_WINDOW` | a watched per-row value returns to its exact prior figure after differing, inside the window — status, context %, token total, todo progress, group key (the worktree↔`external` bounce), model | the field's first appearance (enrichment warm-up is `None`→value, never an oscillation) |
| Status churn | `OBSERVE_STATUS_CHURN_WINDOW` | four or more status transitions on one row inside the window — a rate verdict, deliberately wider than the oscillation window | a quick `running → idle → running` turn boundary, which is normal pace |

## Per-frame checks

The consistency family checks each committed frame alone — detect and log immediately, no window. Each check compares fields the view-model contract says must agree:

- **One pane, one row.** Two rows sharing a pane id, or two agent rows sharing one agent id, violate the presence model's binding rule directly — this is the duplicated-cards bug as a single-frame verdict.
- **Counts agree.** Each group's `status_counts` histogram re-tallies from its rows; with hidden rows the declared counts may exceed the visible tally (counts span the cap by design), and with `hidden_count` zero they match exactly.
- **The own view is coherent.** A derived `active_pane_id` names one of the view's own `working_pane_ids`.
- **Children stay nested.** A rollup child renders inside its parent's card; a child id surfacing as a top-level row, or one id rendered both nested and top-level, is a projection error.
- **Rows imply a frame.** The pane frame admits every card, so rows on a frameless fold are unreachable by contract.

## Real-world cross-checks

The writer thread re-verifies the latest roster against the world on its own cadence (`OBSERVE_CROSSCHECK_TTL`), reading only what the producer already published — the workspace `snapshot.json` pane frame through the stat-gated cache read — and `/proc`. The observer adds no mux call of its own; the producer remains the only external puller ([state.md → The Node Model](./state.md#the-node-model)).

- **Cards fit the frame.** Every roster pane id appears in the published frame, and the roster never exceeds the frame's pane count. The comparison runs only when the roster's fold stamp equals the published frame's `produced_at_ms` — a producer republish between fold and read is normal skew.
- **PIDs are alive.** A row's pane pid is checked through `/proc` with the start-time pid-reuse guard; a pid must stay dead across `OBSERVE_DEADPID_CONFIRMATIONS` consecutive passes before it logs, so a just-exited process the next frame removes leaves no record. Platforms without `/proc` skip the check.

## The records

Observer findings land in the workspace's one durable diagnostics channel — typed `frame_anomaly` records in the state-dir `diag.log.jsonl`, emitted through the shared sink ([diagnostics.md](./diagnostics.md)). The gate's own holds and the health alert edges are first-party records in the same file (`gate_hold`, `gate_release`, `health_alert`), written by the guards themselves; the observer records only what it derives — the windowed and per-frame detections above — so each fact is recorded once and a single `tail` shows the whole episode in order.

Each record carries the detector verdict and its evidence (row and pane ids, before/after values, counts, edge timestamps), the window that judged it, the frame stamp (`panes_produced_at_ms`, row/agent/process counts, and the pulled-truth scalars), the active event summary, the gate and health streaks at fold time, and the instance's elder-or-consumer role at write time; the envelope adds the workspace, session, and instance identity. Every renderer instance records its own stream, so one published-frame problem records once per instance while a node-local fusion or gating problem records on one — the distinct instance count inside an episode separates the two.

Per-kind cooldown (`OBSERVE_COOLDOWN`) keeps a persistent condition from flooding the channel: repeats inside the cooldown increment a suppressed counter that flushes on the kind's next record, so the volume stays bounded while the episode's extent stays visible. The sink's per-identity rate limit backstops it.

## From anomaly to regression test

A `frame_anomaly` record carries enough to rebuild the stream that produced it: the frame stamp and pulled-truth scalars, row and pane identities, edge timestamps, the event summary, and the judging window. A confirmed anomaly becomes a test in [`detect/tests.rs`](../../crates/rimz/src/sidebar/observe/detect/tests.rs) under `mod recorded_episodes`: a signature builder reconstructs the minimal committed sequence from the record's evidence — a warm frame, the offending frame, the restoring frame — and the assertion pins the exact recorded verdict down to its evidence values, alongside the verdicts that must stay absent. Encode the fixture from the log record rather than the frame capture: records survive rotation, captures churn through the eight-pair ring ([diagnostics.md](./diagnostics.md)). The module comment cites the source workspace and date, so a failing replay points back at its originating episode.

## Roles and cost

Every renderer runs the inline detectors on its own stream — each node fuses its own events and gates its own frames, so a flap exists only in the renderer that painted it. The elected elder's writer additionally runs the real-world cross-checks, so the room pays the `/proc` and cache reads once; the split is a single policy function on the writer thread. The cost envelope — one O(rows) signature pass per committed fold, a bounded channel, one throttled cross-check pass — is budgeted in [performance.md → The cost map](./performance.md#the-cost-map).
