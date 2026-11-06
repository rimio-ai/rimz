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
        ("agent='claude'\nprompt-file='judge.md'", "judge.md"),
        (
            "agent='claude'\nprompt-file='judge.md'\nsummary='review is clean'",
            "review is clean",
        ),
        (
            "else='pending'\n[[checks.watch.cases]]\nwhen=['team.stage=Done']\noutcome='done'",
            "`done`: `team.stage=Done` → `@done`",
        ),
    ] {
        let text = format!(
            "[workflow]\nscope='worktree'\nstart='build'\n[steps.build]\nagent='claude'\nprompt-file='prompt.md'\nthen='watch'\nmax-visits=3\n[checks.watch]\non={{done='@done',pending='build',pass='@done',fail='build'}}\n{definition}\n"
        );
        let (_dir, state, record) = setup(&text);
        let host = FakeHost::new();
        advance_ok(&state, &record.id, &host);
        let text = note(&host);
        actual.push((
            text.contains(expected),
            text.contains("next prompt"),
            text.contains("push-backs count as visits"),
        ));
    }
    assert_eq!(actual, vec![(true, true, true); 7]);
}
