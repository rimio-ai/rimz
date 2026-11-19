use super::attempts::{advance_ok, setup};
use super::*;
use crate::disk::lock::WorkspaceLock;
use crate::store::workflow::RunState;

/// A check routing to a second check, which ends the run failed.
const TWO: &str = "[workflow]\nscope='worktree'\nstart='a'\n[script.a]\ntimeout='1h'\nrun='sh check.sh'\non.ok='b'\n[script.b]\ntimeout='1h'\nrun='sh check.sh'\non.ok='@failed'\n";
/// A check that waits on its recheck time.
const PENDING: &str = "[workflow]\nscope='worktree'\nstart='pr'\n[script.pr]\ntimeout='1h'\nrun='sh check.sh'\non.pending='@wait'\n";

/// Each committed edge as `(node before, state before, node after, state after)`.
fn edges(host: &FakeHost) -> Vec<(String, RunState, String, RunState)> {
    host.committed
        .borrow()
        .iter()
        .map(|(before, after)| {
            (
                before.node.clone(),
                before.state,
                after.node.clone(),
                after.state,
            )
        })
        .collect()
}

#[test]
fn every_committed_edge_reaches_the_host_after_the_lock_is_released() {
    let (dir, state, record) = setup(TWO);
    fs::write(
        dir.path().join("check.sh"),
        r#"printf '%s' '{"outcome":"ok"}'"#,
    )
    .unwrap();
    let host = FakeHost::new();
    let lock = state.workspace_lock.clone();
    let held = Rc::new(Cell::new(0));
    let seen = Rc::clone(&held);
    *host.on_committed.borrow_mut() = Some(Box::new(move || {
        if WorkspaceLock::try_acquire(&lock).unwrap().is_none() {
            seen.set(seen.get() + 1);
        }
    }));
    advance(&state, &record.id, &host).unwrap();
    let edges = edges(&host);
    assert_eq!(
        edges
            .iter()
            .map(|(from, before, _, after)| (from.as_str(), *before, *after))
            .collect::<Vec<_>>(),
        [
            ("a", RunState::Active, RunState::Active),
            ("b", RunState::Active, RunState::Failed),
        ],
        "{edges:?}"
    );
    assert_eq!(edges[0].2, "b");
    assert_eq!(
        held.get(),
        0,
        "the host hears a write only once the lock is free"
    );
}

#[test]
fn a_write_with_no_edge_hands_nothing_and_a_cancel_hands_its_ending() {
    let (dir, state, record) = setup(PENDING);
    fs::write(
        dir.path().join("check.sh"),
        r#"printf '%s' '{"outcome":"pending","recheck_at":"2099-01-01T00:00:00Z"}'"#,
    )
    .unwrap();
    let host = FakeHost::new();
    let waiting = advance_ok(&state, &record.id, &host);
    assert!(waiting.wait.is_some(), "{waiting:#?}");
    assert!(edges(&host).is_empty(), "{:?}", edges(&host));
    cancel(&state, &record.id, &host, None).unwrap();
    assert_eq!(
        edges(&host),
        [(
            "pr".to_owned(),
            RunState::Active,
            "pr".to_owned(),
            RunState::Canceled
        )]
    );
}
