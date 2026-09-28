use super::unscheduled_clock;
use rimz::config::TaskEntry;
use rimz::harness::schedule::{arming::Arming, catalog::TaskSource, parse_trigger};

#[test]
fn only_live_clock_tasks_without_a_scheduler_are_unscheduled() {
    let now = "2026-01-01T00:00:00Z".parse().unwrap();
    for (trigger, enabled, pause_until, room, timer, expected) in [
        ("every = '1m'", true, None, false, false, true),
        ("every = '1m'", true, None, true, false, false),
        ("every = '1m'", true, None, false, true, false),
        ("every = '1m'", false, None, false, false, false),
        (
            "every = '1m'",
            true,
            Some("2026-01-02T00:00:00Z"),
            false,
            false,
            false,
        ),
        (
            "every = '1m'",
            true,
            Some("2025-12-31T00:00:00Z"),
            false,
            false,
            true,
        ),
        ("signal = 'ci.failed'", true, None, false, false, false),
        ("watch = 'true'", true, None, false, false, false),
        ("every = 'invalid'", true, None, false, false, false),
    ] {
        let entry: TaskEntry = toml::from_str(&format!(
            "agent = 'claude'\nroot = '/tmp/project'\n{trigger}"
        ))
        .unwrap();
        let parsed = parse_trigger("morning", &entry);
        let record = Arming {
            enabled,
            at: None,
            pause_until: pause_until.map(|s| s.parse().unwrap()),
            strikes: None,
        };
        assert_eq!(
            unscheduled_clock(&parsed, Some(&record), TaskSource::Config, room, timer, now),
            expected,
            "{trigger}, {record:?}, room={room}, timer={timer}"
        );
    }
}
