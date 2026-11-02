use super::*;

#[test]
fn decision_is_opt_in_and_requires_the_stuck_warning_first() {
    let actual =
        [(false, 3_300), (true, 3_299), (true, 3_300), (true, 3_301)].map(|(enabled, window)| {
            let mut config = MachineConfig::default();
            config.harness.stall_nudge = enabled;
            config.agents.attention =
                toml::from_str(&format!("stalled_after_secs = {window}")).unwrap();
            decision(&config)
        });
    assert_eq!(
        actual,
        [
            Decision::Off,
            Decision::On,
            Decision::Shadowed,
            Decision::Shadowed
        ]
    );
}
