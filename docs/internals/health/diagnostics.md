# Sidebar Diagnostics

Sidebar diagnostics are typed anomaly records written to the workspace state directory so a live glitch leaves post-hoc evidence. They are diagnostic state only: no correctness path reads them, and the sidebar keeps unstructured tracing off by default.

## Location

Each workspace writes `diag.log.jsonl` under `~/.local/state/rimz/workspaces/<workspace-id>/`, rotating at 1 MiB with one kept generation at `diag.log.1.jsonl`. Anomaly frame captures live beside it in `diag-frames/`, a private `0700` ring that keeps the last eight prior/offending pane-frame pairs — one `frame.<at_ms>.<seq>.<kind>.json` per pair, so a capture joins its log record by timestamp and kind.

The log is state-dir rather than runtime-dir because its job is investigation after the pane, mux session, or machine has gone away. Runtime caches remain under `$XDG_RUNTIME_DIR`; diagnostics survive reboot like ledger records.

## Envelope

Every line is a `rimz.diag.v1` JSON object from [`schema/diag.rs`](../../../crates/rimz/src/schema/diag.rs): workspace id, session name, optional sidebar instance id, Unix milliseconds, severity, the writer's `build` id, and a tagged event.

The `build` id — also stamped on every published pane frame — is a digest prefix of the writing executable's bytes ([`build_id.rs`](../../../crates/rimz/src/build_id.rs)), so records and frames written by overlapping old/new builds during an upgrade are distinguishable in place. A producer that reads a prior frame stamped by a different build additionally records a `mixed_build_writers` event, marking the overlap window itself.

Records are anomaly-only. Routine fetch ticks, successful paints, and stable cache hits do not write records.

## Event Taxonomy

| Event | Emitter | Evidence |
| --- | --- | --- |
| `frame_rejected`, `frame_reject_escape`, `frame_shrink_verified`, `pane_count_drop`, `pane_carry_forward`, `pane_carry_refuted`, `carry_forward_expired`, `duplicate_pane_id`, `foreign_session_pane` | `sidebar::produce::panes` / `sidebar::frame` | Ok-but-empty frames, missing-own-pane reads, large pane drops, liveness-guarded carried panes, forced re-pulls that refute an initial omission, carry expiry, duplicate ids, foreign-session leaks |
| `gate_hold`, `gate_release`, `fetch_failure`, `health_alert`, `link_alert`, `producer_elected`, `producer_demoted`, `renderer_panic` | `sidebar_pane::app` | Renderer-side holds, degraded refresh episodes, remote-link degraded/recovered episodes, producer handoff, panics that would otherwise disappear with the pane |
| `row_conflict`, `newborn_quarantined`, `group_migration` | `ledger::snapshot::view` via `sidebar::enrich` and renderer state diffing | duplicate agent identity suppression, newborn known-command unknown-cwd quarantine, rows moving between groups |
| `frame_anomaly` | `sidebar::observe` writer thread | rendered-stream detector verdicts — flaps, oscillations, per-frame consistency violations, elder cross-checks — each carrying its detector key, evidence, frame stamp, and the writer's elder/consumer role ([observe.md](./observe.md)) |
| `mixed_build_writers` | `sidebar::produce::panes` | a prior published frame stamped by a different build than the producing process — the upgrade-overlap window where stale writers regress fresh state |

The emitter column is the triage pointer: producer kinds describe pane-source truth — held, verified, carried, or refuted reads ([sidebar.md → honest reads](../sidebar/sidebar.md#honest-reads-across-a-mux-hiccup)); renderer kinds describe one node's hold and refresh behaviour; projection kinds describe binding and grouping; `frame_anomaly` is the rendered symptom as the [observer](./observe.md) judged it.

The carry kinds attribute the pane-source fault precisely. `pane_carry_forward` marks a mux omission that survived a forced direct re-pull while `/proc` proved the omitted panes alive — the source under-reported and the producer carried the panes. `pane_carry_refuted` marks an initial listing the forced re-pull corrected — the first read lied and the truth healed within one produce. `carry_forward_expired` marks liveness proof running out: a carried pane drops after `PANE_CARRY_TTL` ([`timing.rs`](../../../crates/rimz/src/sidebar/timing.rs)).

Pure projection layers return diagnostics as data. Impure callers append them through `DiagSink`, keeping ledger reducers and the renderer gate free of disk-write APIs. The observer's writer thread emits through the same sink, so every anomaly source shares one envelope, one file, and one rate limiter.

## Volume and retention

One condition can write several records and one record can stand for many occurrences; read counts through these rules:

- The sink rate-limits per identity — the record's kind plus its salient evidence fields ([`identity_key`](../../../crates/rimz/src/schema/diag.rs)) — over a five-second window, so a tight loop writes once per window while distinct evidence always passes.
- The observer adds a per-kind cooldown upstream (`OBSERVE_COOLDOWN`); repeats inside it increment a suppressed counter that flushes on the kind's next record, so one `frame_anomaly` line can stand for a long episode.
- Every renderer instance records its own stream, so one published-frame problem records once per instance while a node-local problem records on one. The distinct `instance_id` count inside an episode separates the two.
- The frame-capture ring churns within hours in a busy room; copy `diag-frames/` pairs out at the start of an investigation. The log records carry enough evidence to reconstruct an episode after its captures rotate.

## Reading

`rimz doctor` prints the last twelve records for the current workspace with severity, kind, age, and a one-line summary. The file stays plain JSONL for direct inspection:

```sh
DIAG=~/.local/state/rimz/workspaces/<workspace-id>/diag.log.jsonl
tail -f "$DIAG"
jq -r '[(.at_ms|tostring), .severity, .event.kind, (.instance_id // "-")] | join(" ")' "$DIAG"  # episode timeline
jq -r '.event.kind' "$DIAG" | sort | uniq -c | sort -rn                                         # kind census
jq 'select(.at_ms > 1781070540000 and .at_ms < 1781070550000)' "$DIAG"                          # window slice
jq 'select(.event.kind == "frame_anomaly") | .event.anomaly' "$DIAG"                            # observer evidence
jq 'select(.event.kind == "gate_hold")' "$DIAG"
```

## Investigating an episode

One pass over the log answers an episode's three questions in order — what the user saw, where truth went wrong, and why.

1. **Build the timeline.** Run the timeline one-liner above (or `rimz doctor` for the tail) and cluster records by `at_ms`; an episode reads as a burst across kinds. Copy the matching `diag-frames/` pairs out now, before the ring churns.
2. **Locate the fault: published truth or local fold.** Every `frame_anomaly` carries the pulled snapshot's scalars beside the rendered ones — `pulled_rows` and the pulled frame stamp — so the record itself says whether the producer's published truth already held the anomaly or that instance's fusion/gating introduced it. The distinct `instance_id` count says the same thing from the other side.
3. **Attribute the cause.** Producer records in the same window name it: the carry kinds with the semantics above, `frame_rejected`/`frame_reject_escape` for held implausible reads, `pane_count_drop` for published shrinks, `gate_hold` for renderer-side holds. The frame stamp (`produced_at_ms`) joins producer records, observer records, and capture filenames across the episode.
4. **Diff the captures.** Each capture file holds the last good frame beside the offending one — `jq '{prior: (.prior.tabs | length), offending: (.offending.tabs | length)}'` shows a whole-tab omission at a glance.
5. **Encode the episode.** A confirmed anomaly becomes a synthetic regression test over recorded signatures — the workflow lives in [observe.md → From anomaly to regression test](./observe.md#from-anomaly-to-regression-test).

A recorded partial-read episode reads like this — a pane source reports fourteen panes as six, omitting two whole tabs while their processes live:

| Records | Reading |
| --- | --- |
| `frame_rejected: missing_own_pane` repeating, then `frame_reject_escape` | the producer held the implausible read, then the bounded escape released it |
| `pane_count_drop` with eight removed panes | the shrink published; the capture pair preserves both frames |
| 5× `row_presence_flap`, gone→back 2.26s, no `pane_closed` events, `pulled_rows` back at full count | every instance painted the flap; pulled truth had already recovered, so the rendered gap was the published partial frame propagating |

The carry-forward guard answers this shape before publication, so the same fault now records `pane_carry_forward` under a steady roster — `row_presence_flap` records beside carry records mean the guard missed. This episode persists as the recorded-episode regression test in [`observe/detect/tests.rs`](../../../crates/rimz/src/sidebar/observe/detect/tests.rs).

Frame captures may contain command lines, cwd values, and other pane metadata. They receive the same local filesystem privacy boundary as the rest of the workspace state directory.
