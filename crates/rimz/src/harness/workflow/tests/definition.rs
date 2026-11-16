use super::*;
use crate::harness::workflow::definition::{Invocation, Kind, Target};

fn loaded(text: &str) -> Definition {
    let (_home, dir) = fixture();
    fs::write(dir.join("workflow.toml"), text).unwrap();
    load(&dir).unwrap()
}

#[test]
fn values_admission_refuses_load_with_their_text_and_failure() {
    let spec = loaded(
        "[workflow]\nscope='worktree'\nstart='pick'\n[condition.pick]\ntimeout='365d'\nelse='no'\n\
         cases=[{when=['pr=merged and'],outcome='yes'}]\non={yes='@done',no='run'}\n\
         [script.run]\ntimeout='365d'\nrun=\"echo 'open\"\non.ok='@done'\n",
    );
    let Some(Kind::Condition(cases)) = spec.check("pick").unwrap().kind() else {
        panic!("pick is a condition");
    };
    assert_eq!(cases[0].when().clauses(), ["pr=merged and"]);
    assert!(cases[0].when().expression().is_err());
    let Some(Kind::Script(Invocation::Run(command))) = spec.check("run").unwrap().kind() else {
        panic!("run is a run check");
    };
    assert_eq!(command.text(), "echo 'open");
    assert_eq!(command.words(), None);

    let spec: Definition = toml::from_str(
        "[workflow]\nscope='worktree'\nstart='build'\nlimit={stagger='soon'}\n\
         [agent.build]\nlifecycle='turn'\nagent='claude'\nprompt-file='p.md'\ntimeout='soon'\n\
         admission.window='window.5h.bogus>=x'\nbudget='lots'\nsubscribe=['agent.stop']\nthen='pick'\n\
         [script.pick]\ntimeout='365d'\nrun='true'\nwake=['agent.stop']\non.ok='@done'\n",
    )
    .unwrap();
    let build = spec.step("build").unwrap();
    for (text, failed) in [
        (
            build.timeout().unwrap().text(),
            build.timeout().unwrap().value().is_err(),
        ),
        (
            build.window().unwrap().text(),
            build.window().unwrap().value().is_err(),
        ),
        (
            build.budget().unwrap().text(),
            build.budget().unwrap().value().is_err(),
        ),
        (
            spec.limit().unwrap().stagger.as_ref().unwrap().text(),
            spec.limit()
                .unwrap()
                .stagger
                .as_ref()
                .unwrap()
                .value()
                .is_err(),
        ),
    ] {
        assert!(failed, "{text} must keep its failure");
    }
    assert_eq!(build.timeout().unwrap().text(), "soon");
    assert_eq!(build.window().unwrap().text(), "window.5h.bogus>=x");
    assert_eq!(build.budget().unwrap().text(), "lots");
    assert_eq!(
        spec.limit().unwrap().stagger.as_ref().unwrap().text(),
        "soon"
    );
    let overrides = build
        .launch_overrides(std::path::Path::new("/definition"))
        .unwrap();
    assert!(
        overrides.budget.is_none(),
        "a budget that fails is no budget"
    );
    assert!(build.subscriptions().is_err());
    assert!(spec.check("pick").unwrap().subscriptions().is_err());
}

#[test]
fn a_route_to_a_name_that_is_no_node_compiles_as_a_node_target() {
    let spec: Definition = toml::from_str(
        "[workflow]\nscope='worktree'\nstart='pick'\n[script.pick]\ntimeout='365d'\nrun='true'\non.ok='nowhere'\n",
    )
    .unwrap();
    assert_eq!(
        spec.target("pick", "ok"),
        Some(Target::Node("nowhere".into()))
    );
    assert!(spec.node("nowhere").is_none());
}

#[test]
fn the_bare_window_shorthand_compiles_like_the_dotted_term() {
    let window = |text: &str| {
        let spec = loaded(&format!(
            "[workflow]\nscope='worktree'\nstart='build'\n[agent.build]\nlifecycle='turn'\nagent='claude'\n\
             prompt-file='prompts/build.md'\nadmission.window='{text}'\nthen='@done'\n"
        ));
        spec.step("build")
            .unwrap()
            .window()
            .unwrap()
            .value()
            .unwrap()
            .clone()
    };
    assert_eq!(window("5h.left>=20"), window("window.5h.left>=20"));
}

#[test]
fn a_lifecycle_on_a_command_refuses() {
    let (_home, dir) = fixture();
    fs::write(
        dir.join("workflow.toml"),
        "[workflow]\nscope='worktree'\nstart='build'\n[command.build]\nrun='true'\ntimeout='1m'\nlifecycle='session'\nthen='pick'\nmax-visits=3\n[script.pick]\nrun='true'\ntimeout='1h'\non.ok='@done'\non.pending='build'\n",
    )
    .unwrap();
    assert!(
        matches!(load(&dir), Err(SpecErr::Toml { path, .. }) if path == "command.build.lifecycle")
    );
}

#[test]
fn every_time_spelling_resolves_in_the_given_zone() {
    // Friday 2026-11-13, 15:30 at +05:30.
    let now: jiff::Timestamp = "2026-11-13T10:00:00Z".parse().unwrap();
    let zone = jiff::tz::TimeZone::fixed(jiff::tz::Offset::from_seconds(19_800).unwrap());
    let cases = [
        ("8h", "2026-11-13T18:00:00Z"),
        ("90s", "2026-11-13T10:01:30Z"),
        ("15:30", "2026-11-14T10:00:00Z"),
        ("16:00", "2026-11-13T10:30:00Z"),
        ("fri 15:30", "2026-11-20T10:00:00Z"),
        ("Monday 09:00", "2026-11-16T03:30:00Z"),
        ("2026-11-13 09:00", "2026-11-13T03:30:00Z"),
        ("2026-11-20T08:15", "2026-11-20T02:45:00Z"),
        ("2026-11-20T08:15:30", "2026-11-20T02:45:30Z"),
        ("2026-11-21", "2026-11-20T18:30:00Z"),
        ("2026-11-13T21:00:00Z", "2026-11-13T21:00:00Z"),
        ("2026-11-13T21:00:00+01:00", "2026-11-13T20:00:00Z"),
    ];
    let actual = cases
        .iter()
        .map(|(raw, _)| {
            let when = parse_when(raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
            (*raw, when.resolve(now, &zone).unwrap().to_string())
        })
        .collect::<Vec<_>>();
    let expected = cases
        .iter()
        .map(|(raw, at)| (*raw, (*at).to_owned()))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
    for raw in ["never", "25:00", "fri  09:00", "366d", "2026-13-01"] {
        assert_eq!(
            parse_when(raw),
            Err(format!(
                "{raw:?} is not a workflow time; give a duration (8h), HH:MM, <weekday> HH:MM, \
                 a date (2026-11-14), a date-time (2026-11-14 09:00), or a timestamp \
                 (2026-11-13T21:00:00Z)"
            ))
        );
    }
}
