use super::*;

#[test]
fn launch_notes_describe_each_check_kind_and_the_until_pushback() {
    let mut actual = Vec::new();
    for (definition, expected) in [
        ("run='sh check.sh'", "sh check.sh"),
        (
            "run='sh check.sh'\nsummary='all tests pass'",
            "all tests pass",
        ),
        ("shell='test -f result'", "test -f result"),
        (
            "shell='true'\nsummary='the output exists'",
            "the output exists",
        ),
        ("agent='claude'\nprompt-file='judge.md'", "agent judge"),
        (
            "agent='claude'\nprompt-file='judge.md'\nsummary='review is clean'",
            "review is clean",
        ),
        (
            "else='pending'\n[[condition.watch.cases]]\nwhen=['team.stage=Done']\noutcome='done'",
            "`done`: `team.stage=Done` → `@done`",
        ),
    ] {
        let kind = match definition.split('=').next() {
            Some("run") => "script",
            Some("shell") => "test",
            Some("agent") => "judge",
            _ => "condition",
        };
        let routes = if kind == "test" {
            "pass='@done',fail='build'"
        } else {
            "done='@done',pending='build',unknown='build'"
        };
        let text = format!(
            "[workflow]\nscope='worktree'\nstart='build'\n[agent.build]\nagent='claude'\nprompt-file='prompt.md'\nthen='watch'\nmax-visits=3\n[{kind}.watch]\non={{{routes}}}\n{definition}\n"
        );
        let (_dir, state, record) = setup(&text);
        let host = FakeHost::new();
        advance_ok(&state, &record.id, &host);
        let text = note(&host);
        let facts = if definition.starts_with("run=") {
            text.contains("sh check.sh")
                && text.contains("containing a `/`")
                && text.contains("definition folder")
                && text.contains("runs from the checkout")
                && text.contains("may run")
        } else if definition.starts_with("shell=") {
            text.contains("sh -c")
                && text.contains("runs from the checkout")
                && text.contains("$RIMZ_WORKFLOW_DEFINITION")
                && text.contains("may run")
        } else if definition.starts_with("agent=") {
            !text.contains("judge.md")
        } else {
            true
        };
        actual.push((
            text.contains(expected),
            text.contains("next prompt"),
            text.contains("push-backs count as visits"),
            facts,
        ));
    }
    assert_eq!(actual, vec![(true, true, true, true); 7]);
}

#[test]
fn sealed_judge_without_summary_uses_a_safe_note_and_lists_stay_ordered() {
    let definition = "[workflow]\nscope='worktree'\nstart='build'\n[agent.build]\nagent='claude'\nprompt-file='prompt.md'\nthen=['cheap','review']\n[script.cheap]\nrun='true'\non.pass='review'\n[judge.review]\nagent='claude'\nprompt-file='private-judge.md'\non.pass='@done'\n";
    let (_dir, state, record) = setup(definition);
    let paths = state.workflow(&record.id);
    let spec = crate::harness::workflow::load(&paths.home).unwrap();
    let text = crate::harness::workflow::brief::step(&paths, &spec, &record);
    assert!(!text.contains("private-judge.md"), "{text}");
    assert!(text.contains("agent judge"), "{text}");
    assert!(
        text.find("The check `cheap`") < text.find("The check `review`"),
        "{text}"
    );
    assert!(
        text.contains("stay open while the run moves between the listed checks")
            && text.contains("next turn end starts at the first"),
        "{text}"
    );
}
