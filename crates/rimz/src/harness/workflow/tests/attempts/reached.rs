use super::*;

const RAIL: &str = include_str!("../fixtures/reached.toml");

fn setup_rail(text: &str) -> (tempfile::TempDir, StatePaths, RunRecord) {
    let (dir, state, record) = setup(text);
    let spec = super::super::super::spec::parse(text).unwrap();
    let validation = super::super::super::validate::validate_spec(
        dir.path(),
        "test",
        &spec,
        &LoadedDefinitions::default(),
        Some(dir.path()),
    );
    assert!(validation.errors.is_empty(), "{:?}", validation.errors);
    (dir, state, record)
}

fn reached_nodes(state: &StatePaths, record: &RunRecord) -> Vec<String> {
    let spec = super::super::super::spec::parse(RAIL).unwrap();
    let journal = records::journal(&state.workflow(&record.id)).unwrap();
    reached(&hops(&spec, &journal, record.held.as_ref()), &journal)
        .into_keys()
        .map(str::to_owned)
        .collect()
}

#[test]
fn reached_tracks_engine_entries_and_keeps_them_after_looping_back() {
    let (_dir, state, record) = setup_rail(RAIL);
    let host = FakeHost::new();
    let launched = advance_ok(&state, &record.id, &host);
    assert_eq!(launched.node, "build");
    let middle = answer(&state, &record, &host, json!({"outcome":"next"}));
    assert_eq!(middle.node, "middle");
    assert_eq!(reached_nodes(&state, &middle), ["build", "gate", "middle"]);
    let finish = answer(&state, &record, &host, json!({"outcome":"next"}));
    assert_eq!(finish.node, "finish");
    let all = ["build", "finish", "gate", "middle", "review"];
    assert_eq!(reached_nodes(&state, &finish), all);
    let looped = answer(&state, &record, &host, json!({"outcome":"again"}));
    assert_eq!(
        (looped.node.as_str(), looped.state),
        ("build", RunState::Failed)
    );
    assert_eq!(reached_nodes(&state, &looped), all);
}

#[test]
fn reached_credits_resume_target_but_not_the_nodes_it_skipped() {
    let (_dir, state, record) =
        setup_rail(&RAIL.replace("on-break = '@failed'", "on-break = {agent='claude'}"));
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    let broken = answer(&state, &record, &host, json!({"outcome":"again"}));
    assert_eq!(broken.node, "@break");
    assert_eq!(reached_nodes(&state, &broken), ["build", "gate"]);
    resume(&state, &record.id, Some("finish"), &[]).unwrap();
    let resumed = advance_ok(&state, &record.id, &host);
    assert_eq!(resumed.node, "finish");
    assert_eq!(
        reached_nodes(&state, &resumed),
        ["@break", "build", "finish", "gate"]
    );
}
