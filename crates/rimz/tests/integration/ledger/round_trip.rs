//! Synthetic ledger round-trip checks that do not spawn `rimz`.

#[test]
fn runtime_projection_serves_lock_free_while_a_writer_holds_the_lock() {
    // Reads resume from the persisted rollup fold base, so they never take
    // the workspace lock: a projection completes — and still sees every
    // committed agent — while a writer holds the lock.
    use std::sync::mpsc;
    use std::time::Duration;

    let h = crate::common::Harness::new();

    h.ledger
        .append_event(&crate::common::lifecycle_event(
            &h,
            "rimz-test",
            "SessionStart",
            "agent-1",
        ))
        .expect("append agent");

    let _guard = rimz::ledger::lock::WorkspaceLock::acquire(h.ledger.workspace_lock_path())
        .expect("hold workspace lock");

    let ledger = h.ledger.clone();
    let (result_tx, result_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let projection = ledger.runtime_projection(rimz::RuntimeScope::Runtime);
        let _ = result_tx.send(projection.map(|p| p.agents.len()));
    });

    let agents = result_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("projection completes while the workspace lock is held")
        .expect("projection succeeds");
    assert_eq!(agents, 1, "the committed agent survives the lock-free read");
    reader.join().expect("reader thread");
}
