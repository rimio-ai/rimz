use super::*;

#[test]
fn launch_notes_describe_each_check_kind_and_the_rest_check_pushback() {
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
        let routes = match kind {
            "test" => "pass='@done',fail='build'",
            "condition" => "done='@done',pending='build',unknown='build'",
            _ => "done='@done',pending='build'",
        };
        let text = format!(
            "[workflow]\nscope='worktree'\nstart='build'\n[agent.build]\nagent='claude'\nlifecycle='session'\nrest-check=['watch']\nprompt-file='prompt.md'\nthen='watch'\nmax-visits=3\n[{kind}.watch]\ntimeout='365d'\non={{{routes}}}\n{definition}\n"
        );
        let (dir, state, record) = setup(&text);
        fs::write(dir.path().join("judge.md"), "Review it").unwrap();
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
    assert_eq!(actual, vec![(true, true, true, true); 6]);
}

#[test]
fn a_judge_note_hides_its_prompt_file_and_rest_checks_stay_ordered() {
    let definition = "[workflow]\nscope='worktree'\nstart='build'\n[agent.build]\nagent='claude'\nlifecycle='session'\nprompt-file='prompt.md'\nrest-check=['cheap','review']\nthen='cheap'\n[script.cheap]\ntimeout='365d'\nrun='true'\non.pass='review'\n[judge.review]\ntimeout='365d'\nagent='claude'\nprompt-file='private-judge.md'\nsummary='the diff is clean'\non.pass='@done'\n";
    let (_dir, state, record) = setup(definition);
    let paths = state.workflow(&record.id);
    let spec = crate::harness::workflow::load(&paths.home).unwrap();
    let text = crate::harness::workflow::brief::step(&paths, &spec, &record);
    assert!(!text.contains("private-judge.md"), "{text}");
    assert!(
        text.contains("The check `review` tests `the diff is clean`."),
        "{text}"
    );
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
