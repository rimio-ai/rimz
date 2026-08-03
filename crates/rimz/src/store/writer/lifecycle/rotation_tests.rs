use std::fs::{FileTimes, OpenOptions};
use std::time::{Duration, SystemTime};

use super::tests::{observation, test_store};
use super::*;

#[test]
fn rotation_due_touches_stamp_and_fresh_stamp_debounces() {
    let (_dir, store) = test_store();
    let second = Store::open(store.paths().clone(), store.runtime_paths().clone())
        .expect("second store handle");
    let registered = observation(LifecycleSignal::Registered);
    let intent = || AgentLifecycleIntent {
        session_name: "rimz-test",
        agent_kind: AgentKind::new_unchecked("claude"),
        event_name: "SessionStart",
        observation: &registered,
        spawned_subagents: &[],
    };

    assert!(
        !second
            .append_agent_lifecycle_with_threshold(intent(), u64::MAX)
            .expect("below rotation threshold")
            .rotation_due
    );
    assert!(
        second
            .append_agent_lifecycle_with_threshold(intent(), 0)
            .expect("rotation due")
            .rotation_due
    );
    let stamp = store.paths().locks_dir.join(AUTO_ROTATE_STAMP);
    assert!(stamp.exists());
    assert!(
        !store
            .append_agent_lifecycle_with_threshold(intent(), 0)
            .expect("fresh stamp debounces")
            .rotation_due
    );
    OpenOptions::new()
        .write(true)
        .open(&stamp)
        .expect("open rotation stamp")
        .set_times(
            FileTimes::new()
                .set_modified(SystemTime::now() - AUTO_ROTATE_DEBOUNCE - Duration::from_secs(1)),
        )
        .expect("age rotation stamp");
    assert!(
        debounce::stamp_due(&stamp, AUTO_ROTATE_DEBOUNCE),
        "aged stamp must pass debounce"
    );
    assert!(
        store
            .append_agent_lifecycle_with_threshold(intent(), 0)
            .expect("aged stamp is due")
            .rotation_due
    );
    OpenOptions::new()
        .write(true)
        .open(&stamp)
        .expect("open rotation stamp")
        .set_times(FileTimes::new().set_modified(SystemTime::now() + Duration::from_secs(60)))
        .expect("future-date rotation stamp");
    assert!(
        store
            .append_agent_lifecycle_with_threshold(intent(), 0)
            .expect("future stamp is due")
            .rotation_due
    );
}
