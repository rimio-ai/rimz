//! Warm-read bound on the consumer enrichment sidecars.
//!
//! The sidebar's consumer read folds every live session's `agent_context`
//! and `agent_activity` sidecar on each wakeup. The contract
//! (docs/internals/performance.md): an unchanged room costs one stat per
//! file — the `(mtime, len)` parse caches serve every record without a read
//! or a parse, so a fleet of tens of agents re-reads in microseconds.
//!
//! Proven structurally, not by wall clock: every sidecar is rewritten in
//! place with *different* content but identical `(mtime, len)`. A warm pass
//! that re-parsed would see the swapped bytes; serving the originals proves
//! zero re-parses across the whole fleet. A moved mtime then proves the gate
//! does not pass vacuously — that one file re-parses and the swap shows.

use std::path::Path;

use jiff::Timestamp;
use rimz::agent_activity;
use rimz::agents::AgentContext;
use rimz::ids::WorkspaceId;
use rimz::store::RuntimePaths;
use rimz::store::agent_context;

const FLEET: usize = 20;

fn context_at(now: Timestamp) -> AgentContext {
    serde_json::from_value(serde_json::json!({
        "source": "claude",
        "model_id": "claude-opus-4-8",
        "observed_at": now.to_string(),
    }))
    .expect("minimal context parses — every other field is optional")
}

/// Rewrite `path` in place, swapping `from` → `to` (same byte length), then
/// restore the original mtime so the `(mtime, len)` stat gate cannot tell.
fn swap_in_place(path: &Path, from: &str, to: &str) {
    assert_eq!(
        from.len(),
        to.len(),
        "the swap must preserve the file length"
    );
    let mtime = std::fs::metadata(path).unwrap().modified().unwrap();
    let body = String::from_utf8(std::fs::read(path).unwrap()).unwrap();
    let swapped = body.replace(from, to);
    assert_ne!(body, swapped, "the marker must occur in {path:?}");
    std::fs::write(path, swapped).unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(mtime).unwrap();
}

#[test]
fn warm_sidecar_read_is_one_stat_per_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/perf-enrich"));
    let runtime = RuntimePaths::under(workspace, dir.path()).expect("runtime paths");
    runtime.ensure_dirs().expect("runtime dirs");

    // Seed a fleet: one context sidecar and one activity touch per session.
    let now = Timestamp::now();
    let ids: Vec<String> = (0..FLEET).map(|i| format!("sess-{i:02}")).collect();
    for id in &ids {
        agent_context::write(&runtime, "claude", id, &context_at(now)).expect("write context");
        agent_activity::touch(&runtime, "claude", id).expect("touch activity");
    }
    let keys: Vec<(&str, &str)> = ids.iter().map(|id| ("claude", id.as_str())).collect();

    // Cold pass primes this thread's parse caches.
    assert_eq!(agent_context::read_all(&runtime).len(), FLEET);
    assert_eq!(
        agent_activity::read_for_keys(&runtime, keys.iter().copied()).len(),
        FLEET
    );

    // Poison every sidecar under an unchanged stat. (`sess-` → `boom-` keeps
    // the byte length; a re-parse would surface the poisoned ids.)
    for dir in [&runtime.agent_context_dir, &runtime.agent_activity_dir] {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            swap_in_place(&entry.path(), "sess-", "boom-");
        }
    }

    // Warm pass: every record must come from the cache — zero re-parses
    // across the fleet, or a poisoned id leaks through.
    let contexts = agent_context::read_all(&runtime);
    assert_eq!(contexts.len(), FLEET);
    assert!(
        contexts
            .iter()
            .all(|r| r.agent_id.as_str().starts_with("sess-")),
        "an unchanged (mtime, len) sidecar must serve the cached parse"
    );
    let touches = agent_activity::read_for_keys(&runtime, keys.iter().copied());
    assert_eq!(touches.len(), FLEET);
    assert!(
        touches
            .iter()
            .all(|t| t.agent_id.as_str().starts_with("sess-")),
        "an unchanged (mtime, len) touch must serve the cached parse"
    );

    // Not vacuous: move one context sidecar's mtime and the gate re-parses
    // exactly that file — the poison shows.
    let one = std::fs::read_dir(&runtime.agent_context_dir)
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let f = std::fs::OpenOptions::new().write(true).open(&one).unwrap();
    let mtime = std::fs::metadata(&one).unwrap().modified().unwrap();
    f.set_modified(mtime + std::time::Duration::from_secs(3))
        .unwrap();
    drop(f);
    let poisoned = agent_context::read_all(&runtime)
        .into_iter()
        .filter(|r| r.agent_id.as_str().starts_with("boom-"))
        .count();
    assert_eq!(poisoned, 1, "a moved stat re-parses exactly that file");
}
