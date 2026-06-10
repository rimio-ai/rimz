# Sidebar Diagnostics

Sidebar diagnostics are typed anomaly records written to the workspace state directory so a live glitch leaves post-hoc evidence. They are diagnostic state only: no correctness path reads them, and the sidebar keeps unstructured tracing off by default.

## Location

Each workspace writes `diag.log.jsonl` under `~/.local/state/rimz/workspaces/<workspace-id>/`, with one rotated generation at `diag.log.1.jsonl`. Anomaly frame captures live beside it in `diag-frames/`, a private `0700` ring that keeps the last eight prior/offending pane-frame pairs with unique timestamped names.

The log is state-dir rather than runtime-dir because its job is investigation after the pane, mux session, or machine has gone away. Runtime caches remain under `$XDG_RUNTIME_DIR`; diagnostics survive reboot like ledger records.

## Envelope

Every line is a `rimz.diag.v1` JSON object from [`schema/diag.rs`](../../crates/rimz/src/schema/diag.rs): workspace id, session name, optional sidebar instance id, Unix milliseconds, severity, the writer's `build` id, and a tagged event.

The `build` id — also stamped on every published pane frame — is a digest prefix of the writing executable's bytes ([`build_id.rs`](../../crates/rimz/src/build_id.rs)), so records and frames written by overlapping old/new builds during an upgrade are distinguishable in place. A producer that reads a prior frame stamped by a different build additionally records a `mixed_build_writers` event, marking the overlap window itself.

Records are anomaly-only. Routine fetch ticks, successful paints, and stable cache hits do not write records.

## Event Taxonomy

| Event | Emitter | Evidence |
| --- | --- | --- |
| `frame_rejected`, `frame_reject_escape`, `frame_shrink_verified`, `pane_count_drop`, `pane_carry_forward`, `pane_carry_refuted`, `carry_forward_expired`, `duplicate_pane_id`, `foreign_session_pane` | `sidebar::produce::panes` / `sidebar::frame` | Ok-but-empty frames, missing-own-pane reads, large pane drops, liveness-guarded carried panes, forced re-pulls that refute an initial omission, carry expiry, duplicate ids, foreign-session leaks |
| `gate_hold`, `gate_release`, `fetch_failure`, `health_alert`, `producer_elected`, `producer_demoted`, `renderer_panic` | `sidebar_pane::app` | Renderer-side holds, degraded refresh episodes, producer handoff, panics that would otherwise disappear with the pane |
| `row_conflict`, `newborn_quarantined`, `group_migration` | `ledger::snapshot::view` via `sidebar::enrich` and renderer state diffing | duplicate agent identity suppression, newborn known-command unknown-cwd quarantine, rows moving between groups |
| `frame_anomaly` | `sidebar::observe` writer thread | rendered-stream detector verdicts — flaps, oscillations, per-frame consistency violations, elder cross-checks — each carrying its detector key, evidence, frame stamp, and the writer's elder/consumer role ([observe.md](./observe.md)) |
| `mixed_build_writers` | `sidebar::produce::panes` | a prior published frame stamped by a different build than the producing process — the upgrade-overlap window where stale writers regress fresh state |

Pure projection layers return diagnostics as data. Impure callers append them through `DiagSink`, keeping ledger reducers and the renderer gate free of disk-write APIs. The observer's writer thread emits through the same sink, so every anomaly source shares one envelope, one file, and one rate limiter.

## Reading

`rimz doctor` prints the last twelve records for the current workspace with severity, kind, age, and a one-line summary. The file stays plain JSONL for direct inspection:

```sh
tail -f ~/.local/state/rimz/workspaces/<workspace-id>/diag.log.jsonl
jq 'select(.event.kind == "gate_hold")' ~/.local/state/rimz/workspaces/<workspace-id>/diag.log.jsonl
jq 'select(.event.kind == "frame_anomaly") | .event.anomaly' ~/.local/state/rimz/workspaces/<workspace-id>/diag.log.jsonl
jq -r '.severity + " " + .event.kind' ~/.local/state/rimz/workspaces/<workspace-id>/diag.log.jsonl
```

Frame captures may contain command lines, cwd values, and other pane metadata. They receive the same local filesystem privacy boundary as the rest of the workspace state directory.
