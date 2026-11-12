use super::*;
use crate::harness::workflow::definition::{Kind, Target};

fn loaded(text: &str) -> Definition {
    let (_home, dir) = fixture();
    fs::write(dir.join("workflow.toml"), text).unwrap();
    load(&dir).unwrap()
}

fn delay(spec: &Definition, node: &str, outcome: &str) -> Option<String> {
    let edges = spec.resolved_edges(node).unwrap();
    let edge = edges.iter().find(|edge| edge.outcome == outcome).unwrap();
    edge.delay.as_ref().map(|delay| delay.text().to_owned())
}

#[test]
fn values_admission_refuses_load_with_their_text_and_failure() {
    let spec = loaded(
        "[workflow]\nscope='worktree'\nstart='pick'\n[checks.pick]\nelse='no'\n\
         cases=[{when=['pr=merged and'],outcome='yes'}]\non={yes='@done',no='run'}\n\
         [checks.run]\nrun=\"echo 'open\"\non.ok='@done'\n",
    );
    let Some(Kind::Condition(cases)) = spec.check("pick").unwrap().kind() else {
        panic!("pick is a condition");
    };
    assert_eq!(cases[0].when().clauses(), ["pr=merged and"]);
    assert!(cases[0].when().expression().is_err());
    let Some(Kind::Run(command)) = spec.check("run").unwrap().kind() else {
        panic!("run is a run check");
    };
    assert_eq!(command.text(), "echo 'open");
    assert_eq!(command.words(), None);

    let spec: Definition = toml::from_str(
        "[workflow]\nscope='worktree'\nstart='build'\nlimit={stagger='soon'}\n\
         [steps.build]\nagent='claude'\nprompt-file='p.md'\ntimeout='soon'\n\
         window='window.5h.bogus>=x'\nbudget='lots'\nsubscribe=['agent.stop']\nthen='pick'\n\
         [checks.pick]\nrun='true'\nwake=['agent.stop']\non.ok={to='@done',delay='later'}\n",
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
    assert_eq!(delay(&spec, "pick", "ok").as_deref(), Some("later"));
    let overrides = build
        .launch_overrides(std::path::Path::new("/definition"))
        .unwrap();
    assert!(
        overrides.budget.is_none(),
        "a budget that fails is no budget"
    );
    assert_eq!(spec.reserved_usd(), 0.0);
    assert!(build.subscriptions().is_err());
    assert!(spec.check("pick").unwrap().subscriptions().is_err());
}

#[test]
fn a_route_to_a_name_that_is_no_node_compiles_as_a_node_target() {
    let spec: Definition = toml::from_str(
        "[workflow]\nscope='worktree'\nstart='pick'\n[checks.pick]\nrun='true'\non.ok='nowhere'\n",
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
            "[workflow]\nscope='worktree'\nstart='build'\n[steps.build]\nagent='claude'\n\
             prompt-file='prompts/build.md'\nwindow='{text}'\nthen='@done'\n"
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
fn a_delay_on_a_step_with_a_misplaced_lifecycle_compiles_to_what_cleanup_left() {
    const DELAY: &str = "[workflow]\non-break='@failed'\nscope='worktree'\nstart='build'\n[steps.build]\nagent='claude'\nprompt-file='prompts/build.md'\nlifecycle='until'\nthen='pick'\nmax-visits=3\n[checks.pick]\nrun='true'\ntimeout='1h'\non.ok={to='@done',delay='30m'}\non.pending='build'\n";
    assert_eq!(delay(&loaded(DELAY), "pick", "ok").as_deref(), Some("30m"));
    let command = loaded(&DELAY.replace(
        "agent='claude'\nprompt-file='prompts/build.md'",
        "run='true'\ntimeout='1m'",
    ));
    assert_eq!(delay(&command, "pick", "ok"), None);
    assert_eq!(command.target("pick", "ok"), Some(Target::Done));
    let unset = loaded(&DELAY.replace("lifecycle='until'\n", "").replace(
        "agent='claude'\nprompt-file='prompts/build.md'",
        "run='true'\ntimeout='1m'",
    ));
    assert_eq!(
        command.step("build").unwrap().lifecycle(),
        unset.step("build").unwrap().lifecycle()
    );
}
