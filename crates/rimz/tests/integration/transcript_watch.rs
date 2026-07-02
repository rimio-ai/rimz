//! The transcript-watch trigger → refresh wiring: a simulated rollout-write
//! event drives the same stat-gated refresh the producer tick uses
//! (`rimz::sidebar::refresh::refresh_codex_transcript_context`), merging fresh
//! tokens into the session's context sidecar. The OS watcher itself is not
//! driven end-to-end — platform event semantics vary — so this asserts the
//! refresh the watcher's flush invokes, against real sidecar and rollout files.

use rimz::ledger::agent_context::{self, empty_context, new_record};
use rimz::sidebar::refresh::refresh_codex_transcript_context;

use crate::common::Harness;

const SESSION_ID: &str = "sess-watch";

#[test]
fn simulated_rollout_event_merges_fresh_tokens_into_the_sidecar() {
    let harness = Harness::new();
    let runtime = &harness.runtime_paths;
    runtime.ensure_dirs().expect("runtime dirs");

    // The rollout JSONL a live Codex session appends to. The seeded sidecar
    // names it (as any prior hook push would), so the refresh stats the file
    // directly and never walks `~/.codex/sessions/`.
    let sessions = tempfile::tempdir().expect("sessions dir");
    let rollout = sessions
        .path()
        .join(format!("rollout-2026-06-07T00-00-00-{SESSION_ID}.jsonl"));
    std::fs::write(
        &rollout,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"sess-watch\"}}\n\
         {\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5\"}}\n",
    )
    .expect("seed rollout");
    let mut record = new_record(
        "codex",
        SESSION_ID,
        empty_context("codex", jiff::Timestamp::UNIX_EPOCH),
    );
    record.transcript_path = Some(rollout.to_string_lossy().into_owned());
    agent_context::write_record(runtime, &record).expect("seed sidecar");

    // Mid-turn growth: the append the fs watcher observes.
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&rollout)
        .expect("open rollout for append");
    std::io::Write::write_all(
        &mut file,
        b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\
          \"last_token_usage\":{\"input_tokens\":500,\"cached_input_tokens\":300,\
          \"output_tokens\":20,\"total_tokens\":520},\
          \"model_context_window\":1000}}}\n",
    )
    .expect("append token_count");
    drop(file);

    // The watcher's debounce flush invokes exactly this refresh per session.
    refresh_codex_transcript_context(runtime, SESSION_ID, Some("gpt-5"));

    let merged = agent_context::read_one(runtime, "codex", SESSION_ID).expect("merged sidecar");
    let tokens = merged.context.tokens.as_ref().expect("tokens merged");
    // The sidecar carries the derivation inputs (window + current usage), not a
    // baked percentage; the gauge derives 50% (500 of 1000) downstream.
    assert_eq!(tokens.context_window_size, Some(1000));
    assert_eq!(
        tokens
            .current_usage
            .as_ref()
            .and_then(|usage| usage.input_tokens),
        Some(200)
    );
    assert!(
        merged.transcript_stat.is_some(),
        "refresh persists the stat gate for the next trigger"
    );

    // A redundant trigger — the same watcher event firing again with no new
    // write — is a stat-gated no-op: the sidecar bytes do not change.
    let before = std::fs::read(runtime.agent_context_path("codex", SESSION_ID)).expect("sidecar");
    refresh_codex_transcript_context(runtime, SESSION_ID, Some("gpt-5"));
    let after = std::fs::read(runtime.agent_context_path("codex", SESSION_ID)).expect("sidecar");
    assert_eq!(before, after, "unchanged rollout tail refreshes nothing");
}
