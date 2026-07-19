//! The transcript-watch trigger → refresh wiring: a simulated rollout-write
//! event drives the same stat-gated refresh the producer tick uses
//! (`rimz::sidebar::refresh::refresh_session_transcript_context`), merging fresh
//! tokens into the session's context sidecar. The OS watcher itself is not
//! driven end-to-end — platform event semantics vary — so this asserts the
//! refresh the watcher's flush invokes, against real sidecar and rollout files.

use rimz::agents::AgentContext;
use rimz::sidebar::refresh::refresh_session_transcript_context;
use rimz::store::agent_context::{self, new_record};

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
        AgentContext::new("codex", jiff::Timestamp::UNIX_EPOCH),
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
    refresh_session_transcript_context(runtime, "codex", SESSION_ID, Some("gpt-5"));

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
    refresh_session_transcript_context(runtime, "codex", SESSION_ID, Some("gpt-5"));
    let after = std::fs::read(runtime.agent_context_path("codex", SESSION_ID)).expect("sidecar");
    assert_eq!(before, after, "unchanged rollout tail refreshes nothing");
}

#[test]
fn cursor_transcript_event_recovers_terminal_state_without_content() {
    let harness = Harness::new();
    let runtime = &harness.runtime_paths;
    runtime.ensure_dirs().expect("runtime dirs");
    let transcripts = tempfile::tempdir().expect("transcript dir");
    let path = transcripts.path().join("cursor-session.jsonl");
    std::fs::write(
        &path,
        concat!(
            "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"first\"}]}}\n",
            "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"first answer\"}]}}\n",
            "{\"type\":\"turn_ended\",\"status\":\"success\"}\n",
        ),
    )
    .unwrap();
    let mut record = new_record(
        "cursor",
        SESSION_ID,
        AgentContext::new("cursor", jiff::Timestamp::UNIX_EPOCH),
    );
    record.transcript_path = Some(path.to_string_lossy().into_owned());
    agent_context::write_record(runtime, &record).expect("seed cursor sidecar");

    refresh_session_transcript_context(runtime, "cursor", SESSION_ID, Some("cursor/model"));
    let first = agent_context::read_one(runtime, "cursor", SESSION_ID).expect("first refresh");
    let first_stat = first.transcript_stat.expect("first stat");
    let first_complete = first.context.settle.expect("first terminal marker").at;

    std::fs::write(
        &path,
        concat!(
            "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"first\"}]}}\n",
            "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"first answer\"}]}}\n",
            "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"second\"}]}}\n",
            "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"THINKING_SENTINEL_DO_NOT_INGEST\"}]}}\n",
            "{\"type\":\"turn_ended\",\"status\":\"success\"}\n",
        ),
    )
    .unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60)),
        )
        .unwrap();
    refresh_session_transcript_context(runtime, "cursor", SESSION_ID, Some("cursor/model"));

    let merged = agent_context::read_one(runtime, "cursor", SESSION_ID).expect("merged sidecar");
    assert!(
        merged
            .context
            .settle
            .is_some_and(|settle| settle.at > first_complete)
    );
    assert_eq!(merged.context.model_id.as_deref(), Some("cursor/model"));
    assert!(merged.context.tokens.is_none());
    assert_ne!(merged.transcript_stat, Some(first_stat));
    let serialized = serde_json::to_string(&merged).unwrap();
    assert!(!serialized.contains("THINKING_SENTINEL_DO_NOT_INGEST"));

    let before = std::fs::read(runtime.agent_context_path("cursor", SESSION_ID)).unwrap();
    refresh_session_transcript_context(runtime, "cursor", SESSION_ID, Some("cursor/model"));
    let after = std::fs::read(runtime.agent_context_path("cursor", SESSION_ID)).unwrap();
    assert_eq!(before, after, "unchanged Cursor tail is stat-gated");
}
