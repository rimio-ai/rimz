use super::*;

#[test]
fn write_then_read_round_trips() {
    let (_dir, runtime) = runtime();
    let now = Timestamp::now();
    write(&runtime, "claude", "sess-1", &ctx(now)).unwrap();
    let all = read_all(&runtime);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].kind, "claude");
    assert_eq!(all[0].agent_id, "sess-1");
    assert_eq!(all[0].context.model_id.as_deref(), Some("claude-opus-4-8"));
}

#[test]
fn read_one_bypasses_the_parse_cache() {
    let (_dir, runtime) = runtime();
    let now = Timestamp::now();
    write(&runtime, "claude", "sess-1", &ctx(now)).unwrap();
    assert_eq!(
        read_all(&runtime)[0].context.model_id.as_deref(),
        Some("claude-opus-4-8")
    );

    let mut changed = ctx(now);
    changed.model_id = Some("claude-sonnet-4-5".to_owned());
    write(&runtime, "claude", "sess-1", &changed).unwrap();

    let fresh = read_one(&runtime, "claude", "sess-1").expect("fresh direct read");
    assert_eq!(fresh.context.model_id.as_deref(), Some("claude-sonnet-4-5"));
}

#[test]
fn corrupt_file_is_skipped() {
    let (_dir, runtime) = runtime();
    std::fs::write(
        runtime.agent_context_dir.join("ctx.bogus.json"),
        b"not json",
    )
    .unwrap();
    assert!(read_all(&runtime).is_empty());
}

#[test]
fn ttl_cutoff_is_boundary_exact() {
    // A missed tombstone ages out on the TTL exactly: a record *at* the
    // cutoff is still served, one second past it is gone — an off-by-one
    // in either direction fails one arm.
    let (_dir, runtime) = runtime();
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let at_cutoff = Timestamp::from_second(1_700_000_000 - CONTEXT_TTL_SECS).unwrap();
    let past_cutoff = Timestamp::from_second(1_700_000_000 - CONTEXT_TTL_SECS - 1).unwrap();
    write(&runtime, "claude", "sess-at", &ctx(at_cutoff)).unwrap();
    write(&runtime, "claude", "sess-past", &ctx(past_cutoff)).unwrap();
    let ids: Vec<_> = read_all_at(&runtime, now)
        .into_iter()
        .map(|r| r.agent_id)
        .collect();
    assert_eq!(ids, vec!["sess-at".to_owned()]);
}

#[test]
fn unchanged_stat_skips_the_reparse() {
    let (_dir, runtime) = runtime();
    let now = Timestamp::now();
    write(&runtime, "claude", "sess-1", &ctx(now)).unwrap();
    let first = read_all(&runtime);
    assert_eq!(first[0].agent_id, "sess-1");

    // Rewrite the file in place with a different identity but identical
    // length, restoring the original mtime: the stat gate cannot tell it
    // changed, so the cached parse is served — which is exactly the
    // contract (every real update is an atomic rename of a fresh temp
    // file, so a same-stat file is byte-identical in production).
    let path = runtime.agent_context_path("claude", "sess-1");
    let original = std::fs::read(&path).unwrap();
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    let swapped = String::from_utf8(original)
        .unwrap()
        .replace("sess-1", "sess-9");
    std::fs::write(&path, swapped).unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_modified(mtime).unwrap();
    drop(f);
    assert_eq!(
        read_all(&runtime)[0].agent_id,
        "sess-1",
        "same (mtime, len) serves the cached parse — one stat, no read"
    );

    // A moved mtime invalidates: the rewrite is now visible.
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_modified(mtime + std::time::Duration::from_secs(3))
        .unwrap();
    drop(f);
    assert_eq!(read_all(&runtime)[0].agent_id, "sess-9");
}

#[test]
fn remove_targets_one_session() {
    let (_dir, runtime) = runtime();
    let now = Timestamp::now();
    write(&runtime, "claude", "sess-1", &ctx(now)).unwrap();
    write(&runtime, "claude", "sess-2", &ctx(now)).unwrap();
    remove(&runtime, "claude", "sess-1").unwrap();
    let ids: Vec<_> = read_all(&runtime).into_iter().map(|r| r.agent_id).collect();
    assert_eq!(ids, vec!["sess-2".to_owned()]);
    // Removing an absent session is success.
    remove(&runtime, "claude", "sess-1").unwrap();
}
