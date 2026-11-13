use super::*;

const SIMPLE: &str = "[workflow]\non-break='@failed'\nscope='worktree'\nstart='pick'\n[script.pick]\nrun='true'\non.ok='@done'\n";
const CONDITION: &str = "[workflow]\non-break='@failed'\nscope='worktree'\nstart='pick'\n[condition.pick]\nelse='no'\non={yes='@done',no='@done',unknown='@failed'}\ncases=[{when=['pr=merged'],outcome='yes'}]\n";
const SESSION_PICK: &str = "[workflow]\non-break='@failed'\nscope='worktree'\nstart='build'\n[agent.build]\nagent='claude'\nprompt-file='prompts/build.md'\nlifecycle='session'\nrest-check=['pick']\nthen='pick'\nmax-visits=3\n[script.pick]\nrun='true'\ntimeout='1h'\non.ok='@done'\non.pending='build'\n";
/// One acyclic graph with a node of every kind, each written in full; `then='after'` on the
/// agent is where a case adds a field.
const KINDS: &str = "[workflow]\non-break='@failed'\nscope='worktree'\nstart='build'\n\
[agent.build]\nagent='claude'\nprompt-file='prompts/build.md'\nlifecycle='turn'\nthen='after'\n\
[script.after]\nrun='true'\non.ok='tell'\n\
[message.tell]\nmessage='@planner'\nthen='run'\n\
[command.run]\nshell='true'\ntimeout='1m'\nthen='test'\n\
[test.test]\nshell='true'\non.pass='judge'\non.fail='@failed'\n\
[judge.judge]\nagent='claude'\nprompt-file='prompts/build.md'\nsummary='Reviews it'\non.ok='gate'\n\
[condition.gate]\nelse='no'\non={yes='nap',no='nap',unknown='@failed'}\ncases=[{when=['pr=merged'],outcome='yes'}]\n\
[sleep.nap]\nsleep='1m'\nthen='@done'\n";

fn loaded(text: &str) -> Result<Definition, SpecErr> {
    let (_home, dir) = fixture();
    fs::write(dir.join("workflow.toml"), text).unwrap();
    load(&dir)
}

/// The path a definition refuses at and the message, as `load` reports it.
fn refusal(text: &str) -> (String, String) {
    match loaded(text) {
        Err(SpecErr::Toml { path, source }) => (path, source.message().to_owned()),
        Err(error @ (SpecErr::ReservedTarget { .. } | SpecErr::ReservedNode { .. })) => {
            (String::new(), error.to_string())
        }
        Err(error @ SpecErr::DuplicateNode { .. }) => (String::new(), error.to_string()),
        other => (format!("{other:?}"), String::new()),
    }
}

/// Every validation error, by path.
fn errors(text: &str) -> Vec<String> {
    let (_home, dir) = fixture();
    fs::write(dir.join("workflow.toml"), text).unwrap();
    validate(&dir, &LoadedDefinitions::default(), None)
        .errors
        .into_iter()
        .map(|issue| issue.path)
        .collect()
}

/// `KINDS` with `field` added to the node under `header`.
fn with(header: &str, field: &str) -> String {
    assert!(KINDS.contains(header), "{header}");
    KINDS.replace(header, &format!("{header}\n{field}"))
}

#[test]
fn every_kind_validates_as_written() {
    assert_eq!(errors(KINDS), Vec::<String>::new());
}

#[test]
fn unknown_fields_refuse_at_their_dotted_path_in_every_kind() {
    let mut cases = vec![
        (format!("future=true\n{SIMPLE}"), "future".to_owned()),
        (
            SIMPLE.replace("[workflow]", "[workflow]\nfuture=true"),
            "workflow.future".into(),
        ),
        (
            SIMPLE.replace("[workflow]", "[workflow]\non.future='@done'"),
            "workflow.on.future".into(),
        ),
        (
            SIMPLE.replace(
                "on-break='@failed'",
                "on-break={agent='claude',future=true}",
            ),
            "workflow.on-break.future".into(),
        ),
        (
            SIMPLE.replace(
                "start='pick'",
                "start='pick'\nlimit={max-parallel=1,future=1}",
            ),
            "workflow.limit.future".into(),
        ),
        (
            format!("{SIMPLE}wake=[{{signal='review.ready',future=true}}]\n"),
            "script.pick.wake.0.future".into(),
        ),
        (
            CONDITION.replace("outcome='yes'", "outcome='yes',future=true"),
            "condition.pick.cases.0.future".into(),
        ),
        (
            with("[agent.build]", "on.future='@done'"),
            "agent.build.on.future".into(),
        ),
        (
            with("[agent.build]", "admission.future=1"),
            "agent.build.admission.future".into(),
        ),
        (
            with("[agent.build]", "max-visits-per-key=1"),
            "agent.build.max-visits-per-key".into(),
        ),
        (
            with("[agent.build]", "max-visits-per-outcome={limit=2}"),
            "agent.build.max-visits-per-outcome".into(),
        ),
        (
            CONDITION.replace("outcome='yes'", "outcome='yes',key='ci'"),
            "condition.pick.cases.0.key".into(),
        ),
        (
            SIMPLE.replace("[script.pick]", "[checks.pick]"),
            "checks".into(),
        ),
    ];
    for (header, name) in [
        ("[agent.build]", "agent.build"),
        ("[command.run]", "command.run"),
        ("[message.tell]", "message.tell"),
        ("[test.test]", "test.test"),
        ("[script.after]", "script.after"),
        ("[judge.judge]", "judge.judge"),
        ("[condition.gate]", "condition.gate"),
        ("[sleep.nap]", "sleep.nap"),
    ] {
        cases.push((with(header, "future=true"), format!("{name}.future")));
    }
    let actual = cases
        .iter()
        .map(|(text, _)| refusal(text))
        .collect::<Vec<_>>();
    assert_eq!(
        actual
            .iter()
            .map(|(path, message)| (
                path.as_str(),
                message.starts_with(&format!(
                    "unknown field `{}`",
                    path.rsplit('.').next().unwrap()
                ))
            ))
            .collect::<Vec<_>>(),
        cases
            .iter()
            .map(|(_, path)| (path.as_str(), true))
            .collect::<Vec<_>>(),
        "{actual:#?}"
    );
}

#[test]
fn a_field_of_another_kind_refuses_at_its_path() {
    let cases = [
        ("[agent.build]", "run='true'", "agent.build.run"),
        (
            "[agent.build]",
            "occupancy='share'",
            "agent.build.occupancy",
        ),
        (
            "[agent.build]",
            "window='5h.left>=40'",
            "agent.build.window",
        ),
        (
            "[agent.build]",
            "capacity-wait='1h'",
            "agent.build.capacity-wait",
        ),
        (
            "[agent.build]",
            "on.undelivered='@done'",
            "agent.build.on.undelivered",
        ),
        ("[command.run]", "agent='claude'", "command.run.agent"),
        (
            "[command.run]",
            "prompt-file='prompts/build.md'",
            "command.run.prompt-file",
        ),
        (
            "[command.run]",
            "lifecycle='session'",
            "command.run.lifecycle",
        ),
        ("[command.run]", "keep=true", "command.run.keep"),
        ("[command.run]", "notes=true", "command.run.notes"),
        (
            "[command.run]",
            "admission={throttle='on'}",
            "command.run.admission",
        ),
        (
            "[command.run]",
            "on.capacity='@done'",
            "command.run.on.capacity",
        ),
        ("[command.run]", "on.asked='@done'", "command.run.on.asked"),
        (
            "[command.run]",
            "on.invalid='@done'",
            "command.run.on.invalid",
        ),
        (
            "[command.run]",
            "on.failed='@failed'",
            "command.run.on.failed",
        ),
        (
            "[command.run]",
            "subscribe=['ci.failed']",
            "command.run.subscribe",
        ),
        ("[command.run]", "session='ses-1'", "command.run.session"),
        ("[command.run]", "account='work'", "command.run.account"),
        ("[command.run]", "mode='yolo'", "command.run.mode"),
        ("[command.run]", "effort='high'", "command.run.effort"),
        ("[command.run]", "fast=true", "command.run.fast"),
        ("[command.run]", "budget='$5'", "command.run.budget"),
        ("[command.run]", "worktree='feat-x'", "command.run.worktree"),
        (
            "[command.run]",
            "system-prompt-file='prompts/build.md'",
            "command.run.system-prompt-file",
        ),
        ("[message.tell]", "timeout='1h'", "message.tell.timeout"),
        (
            "[message.tell]",
            "lifecycle='session'",
            "message.tell.lifecycle",
        ),
        ("[message.tell]", "keep=true", "message.tell.keep"),
        (
            "[message.tell]",
            "on.asked='@done'",
            "message.tell.on.asked",
        ),
        (
            "[message.tell]",
            "on.capacity='@done'",
            "message.tell.on.capacity",
        ),
        (
            "[message.tell]",
            "on.invalid='@done'",
            "message.tell.on.invalid",
        ),
        (
            "[message.tell]",
            "on.failed='@failed'",
            "message.tell.on.failed",
        ),
        (
            "[message.tell]",
            "admission={window='5h.left>=40'}",
            "message.tell.admission",
        ),
        ("[message.tell]", "account='work'", "message.tell.account"),
        ("[test.test]", "agent='claude'", "test.test.agent"),
        ("[test.test]", "on.other='@done'", "test.test.on.other"),
        ("[test.test]", "on.hold='@done'", "test.test.on.hold"),
        ("[test.test]", "then='@done'", "test.test.then"),
        ("[script.after]", "then='@done'", "script.after.then"),
        ("[script.after]", "else='ok'", "script.after.else"),
        (
            "[script.after]",
            "provider='claude'",
            "script.after.provider",
        ),
        (
            "[script.after]",
            "prompt-file='prompts/build.md'",
            "script.after.prompt-file",
        ),
        (
            "[script.after]",
            "window='5h.left>=40'",
            "script.after.window",
        ),
        ("[script.after]", "keep=true", "script.after.keep"),
        ("[script.after]", "when=['pr=merged']", "script.after.when"),
        ("[judge.judge]", "run='true'", "judge.judge.run"),
        (
            "[judge.judge]",
            "lifecycle='session'",
            "judge.judge.lifecycle",
        ),
        (
            "[judge.judge]",
            "admission={occupancy='share'}",
            "judge.judge.admission.occupancy",
        ),
        (
            "[condition.gate]",
            "wake=['review.ready']",
            "condition.gate.wake",
        ),
        ("[condition.gate]", "run='true'", "condition.gate.run"),
        ("[sleep.nap]", "timeout='1h'", "sleep.nap.timeout"),
        ("[sleep.nap]", "wake=['review.ready']", "sleep.nap.wake"),
        ("[sleep.nap]", "on.done='@done'", "sleep.nap.on.done"),
    ];
    let actual = cases
        .iter()
        .map(|(header, field, _)| refusal(&with(header, field)).0)
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        cases.iter().map(|(_, _, path)| *path).collect::<Vec<_>>()
    );
    for keep in ["true", "'yes'"] {
        assert_eq!(
            refusal(&SIMPLE.replace("[workflow]", &format!("[workflow]\nkeep={keep}"))).0,
            "workflow.keep"
        );
    }
}

#[test]
fn wrong_typed_values_refuse_at_their_path() {
    let cases = [
        (
            KINDS.replace("lifecycle='turn'", "lifecycle='forever'"),
            "agent.build.lifecycle",
        ),
        (
            with("[agent.build]", "max-visits='3'"),
            "agent.build.max-visits",
        ),
        (
            with("[agent.build]", "admission.throttle=5"),
            "agent.build.admission.throttle",
        ),
        (
            with("[agent.build]", "admission.occupancy='yes'"),
            "agent.build.admission.occupancy",
        ),
        (
            with("[agent.build]", "admission.give-up='0s'"),
            "agent.build.admission.give-up",
        ),
        (with("[agent.build]", "mode='sideways'"), "agent.build.mode"),
        (
            with("[agent.build]", "account='bad name!'"),
            "agent.build.account",
        ),
        (with("[agent.build]", "fast='yes'"), "agent.build.fast"),
        (
            with("[agent.build]", "on.invalid=5"),
            "agent.build.on.invalid",
        ),
        (
            with("[agent.build]", "on.invalid={to=5}"),
            "agent.build.on.invalid",
        ),
        (with("[agent.build]", "on.cap=5"), "agent.build.on.cap"),
        (with("[agent.build]", "on.asked=5"), "agent.build.on.asked"),
        (
            with("[agent.build]", "on.capacity=5"),
            "agent.build.on.capacity",
        ),
        (with("[agent.build]", "on=5"), "agent.build.on"),
        (
            KINDS.replace("then='test'", "then=['test']"),
            "command.run.then",
        ),
        (
            with("[script.after]", "wake=[5,'review.ready']"),
            "script.after.wake.0",
        ),
        (
            format!("{SIMPLE}timeout='1m'\non.pending='@wait'\non.timeout=5\n"),
            "script.pick.on.timeout",
        ),
        (
            SIMPLE.replace("on.ok='@done'", "on.ok=5"),
            "script.pick.on.ok",
        ),
        (format!("{SIMPLE}on.invalid=5\n"), "script.pick.on.invalid"),
        (format!("{SIMPLE}on.hold=5\n"), "script.pick.on.hold"),
        (SIMPLE.replace("on.ok='@done'", "on=5"), "script.pick.on"),
        (
            CONDITION.replace("yes='@done'", "yes=5"),
            "condition.pick.on.yes",
        ),
        (
            CONDITION.replace("on={", "on={dead=5,"),
            "condition.pick.on.dead",
        ),
        (
            CONDITION.replace("outcome='yes'", "outcome='yes',for=5"),
            "condition.pick.cases.0.for",
        ),
        (
            SIMPLE.replace("start='pick'", "start='pick'\non=5"),
            "workflow.on",
        ),
        (
            SIMPLE.replace("start='pick'", "start='pick'\non.deadline=5"),
            "workflow.on.deadline",
        ),
        (
            SIMPLE.replace(
                "on-break='@failed'",
                "on-break={agent='claude',prompt-file=42}",
            ),
            "workflow.on-break.prompt-file",
        ),
    ];
    let actual = cases
        .iter()
        .map(|(text, _)| refusal(text).0)
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        cases.iter().map(|(_, path)| *path).collect::<Vec<_>>()
    );
}

#[test]
fn invalid_optional_values_refuse_at_their_path() {
    let mut cases = vec![
        (
            SIMPLE.replace("[workflow]", "[workflow]\nsummary='  '"),
            "workflow.summary",
        ),
        (
            SIMPLE.replace("[workflow]", "[workflow]\ndeadline='never'"),
            "workflow.deadline",
        ),
        (
            SIMPLE.replace("start='pick'", "start='pick'\nlimit={}"),
            "workflow.limit",
        ),
        (
            SIMPLE.replace("start='pick'", "start='pick'\nlimit={max-parallel=0}"),
            "workflow.limit.max-parallel",
        ),
        (format!("{SIMPLE}summary='  '\n"), "script.pick.summary"),
        (format!("{SIMPLE}timeout='soon'\n"), "script.pick.timeout"),
        (format!("{SIMPLE}on.hold='@done'\n"), "script.pick.on.hold"),
        (
            format!("{SIMPLE}wake=[{{signal='review.ready',prompt='look'}}]\n"),
            "script.pick.wake.0.prompt",
        ),
        (
            with("[agent.build]", "summary=\"two\\nlines\""),
            "agent.build.summary",
        ),
        (
            with("[agent.build]", "timeout='soon'"),
            "agent.build.timeout",
        ),
        (
            with("[agent.build]", "max-visits=0"),
            "agent.build.max-visits",
        ),
        (
            with("[agent.build]", "admission.window='ci=passed'"),
            "agent.build.admission.window",
        ),
        (
            with("[agent.build]", "admission.max-parallel=0"),
            "agent.build.admission.max-parallel",
        ),
        (
            with("[agent.build]", "admission.stagger='0s'"),
            "agent.build.admission.stagger",
        ),
        (with("[agent.build]", "mode='plan'"), "agent.build.mode"),
        (with("[agent.build]", "budget='lots'"), "agent.build.budget"),
        (
            with("[agent.build]", "worktree='../out'"),
            "agent.build.worktree",
        ),
        (
            with("[agent.build]", "subscribe=['ci.failed']"),
            "agent.build.subscribe",
        ),
        (
            with("[command.run]", "max-visits=0"),
            "command.run.max-visits",
        ),
        (
            KINDS.replace("timeout='1m'\nthen='test'", "timeout='soon'\nthen='test'"),
            "command.run.timeout",
        ),
        (
            with("[message.tell]", "session=' '"),
            "message.tell.session",
        ),
        (
            with("[message.tell]", "max-visits=0"),
            "message.tell.max-visits",
        ),
        (with("[test.test]", "timeout='soon'"), "test.test.timeout"),
        (
            with("[judge.judge]", "admission.window='ci=passed'"),
            "judge.judge.admission.window",
        ),
        (
            with("[judge.judge]", "on.capacity='@done'"),
            "judge.judge.on.capacity",
        ),
        (
            CONDITION.replace("outcome='yes'", "outcome='yes',for='0s'"),
            "condition.pick.cases.0.for",
        ),
        (
            CONDITION.replace("outcome='yes'", "outcome='yes',for='3ms'"),
            "condition.pick.cases.0.for",
        ),
        (
            CONDITION
                .replace("yes='@done'", "yes='@wait'")
                .replace("else='no'", "timeout='1h'\nelse='no'")
                .replace("outcome='yes'", "outcome='yes',for='1m'"),
            "condition.pick.cases.0.for",
        ),
        (
            CONDITION
                .replace("else='no'", "provider='missing-kind'\nelse='no'")
                .replace("'pr=merged'", "'window.5h.left>=20'"),
            "condition.pick.provider",
        ),
        (
            CONDITION.replace("else='no'", "provider='claude'\nelse='no'"),
            "condition.pick.provider",
        ),
        (
            CONDITION.replace("else='no'", "account='work'\nelse='no'"),
            "condition.pick.account",
        ),
        (
            CONDITION.replace("yes='@done'", "dead='@done',yes='@done'"),
            "condition.pick.on.dead",
        ),
        (with("[sleep.nap]", "for='1m'"), "sleep.nap.for"),
        (
            with("[sleep.nap]", "when=['pr=merged']\nfor='0s'"),
            "sleep.nap.for",
        ),
        (
            with("[sleep.nap]", "when=['pr=merged']\nfor='1m'"),
            "sleep.nap.for",
        ),
    ];
    for wake in [
        "'bad selector'",
        "'ci.finished'",
        "{signal='ci.*', match={conclusion='success'}}",
        "'agent.*'",
        "'agent.idle'",
        "'team.idle'",
        "'team.*'",
        "{signal='review.ready',match={env=' '}}",
    ] {
        cases.push((format!("{SIMPLE}wake=[{wake}]\n"), "script.pick.wake.0"));
        if wake.starts_with("'team.") {
            continue;
        }
        cases.push((
            with("[agent.build]", &format!("subscribe=[{wake}]"))
                .replace("lifecycle='turn'", "lifecycle='session'"),
            "agent.build.subscribe.0",
        ));
    }
    for raw in ["366d", "8761h"] {
        for (text, path) in [
            (format!("{SIMPLE}timeout='{raw}'\n"), "script.pick.timeout"),
            (
                CONDITION.replace(
                    "when=['pr=merged']",
                    &format!("when=['pr=merged'],for='{raw}'"),
                ),
                "condition.pick.cases.0.for",
            ),
            (
                with("[agent.build]", &format!("timeout='{raw}'")),
                "agent.build.timeout",
            ),
            (
                with("[agent.build]", &format!("admission.stagger='{raw}'")),
                "agent.build.admission.stagger",
            ),
            (
                SIMPLE.replace(
                    "start='pick'",
                    &format!("start='pick'\nlimit={{max-parallel=1,stagger='{raw}'}}"),
                ),
                "workflow.limit.stagger",
            ),
        ] {
            cases.push((text, path));
        }
    }
    let actual = cases
        .iter()
        .map(|(text, _)| errors(text))
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        cases
            .iter()
            .map(|(_, path)| vec![(*path).to_owned()])
            .collect::<Vec<_>>(),
        "{:#?}",
        cases.iter().map(|(text, _)| text).collect::<Vec<_>>()
    );
    let (_home, dir) = fixture();
    fs::write(
        dir.join("workflow.toml"),
        format!("{SIMPLE}timeout='366d'\n"),
    )
    .unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    assert!(
        report.errors[0]
            .message
            .contains("duration `366d` is longer than the 365d maximum"),
        "{report:?}"
    );
}

#[test]
fn a_name_two_kinds_share_refuses() {
    let text = format!(
        "{SIMPLE}[agent.pick]\nlifecycle='turn'\nagent='claude'\nprompt-file='prompts/build.md'\nthen='@done'\n"
    );
    let (_home, dir) = fixture();
    fs::write(dir.join("workflow.toml"), text).unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    assert_eq!(
        report
            .errors
            .iter()
            .map(|issue| (issue.path.as_str(), issue.message.as_str()))
            .collect::<Vec<_>>(),
        [(
            "script.pick",
            "script.pick: example: script.pick: agent.pick has the same name; a node name is unique across every kind"
        )]
    );
}

#[test]
fn only_agents_ask_requires_each_fireable_failure_route() {
    let text = "[workflow]\nscope='worktree'\nstart='pick'\ndeadline='2099-01-01T00:00:00Z'\n[script.pick]\nrun='true'\ntimeout='1h'\non.ok='work'\n[agent.work]\nagent='claude'\nlifecycle='turn'\nprompt-file='prompts/build.md'\nthen='pick'\nmax-visits=2\n";
    let (_home, dir) = fixture();
    fs::write(dir.join("workflow.toml"), text).unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    for path in [
        "script.pick.on.timeout",
        "script.pick.on.invalid",
        "agent.work.on.invalid",
        "agent.work.on.cap",
        "workflow.on.deadline",
    ] {
        assert!(
            report.errors.iter().any(|issue| issue.path == path
                && issue.message.contains("route")
                && issue.message.contains("[workflow] on-break")),
            "{path}: {report:?}"
        );
    }
    let explicit = text
        .replace("[workflow]", "[workflow]\non.deadline='@failed'")
        .replace(
            "[script.pick]",
            "[script.pick]\non.invalid='@failed'\non.timeout='@failed'",
        )
        .replace(
            "[agent.work]",
            "[agent.work]\non.invalid='@failed'\non.cap='@failed'",
        );
    let unfireable = explicit
        .replace("deadline='2099-01-01T00:00:00Z'\n", "")
        .replace("on.deadline='@failed'\n", "")
        .replace("timeout='1h'\n", "")
        .replace("on.timeout='@failed'\n", "")
        .replace("max-visits=2\n", "")
        .replace("on.cap='@failed'\n", "")
        .replace("on.ok='work'", "on.ok='@done'");
    for text in [
        explicit,
        unfireable,
        text.replace("[workflow]", "[workflow]\non-break='@failed'"),
        text.replace("[workflow]", "[workflow]\non-break={agent='claude'}"),
    ] {
        fs::write(dir.join("workflow.toml"), &text).unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        assert!(report.errors.is_empty(), "{text}: {report:?}");
    }
}

#[test]
fn only_agents_ask_refuses_human_targets_on_every_route() {
    let command = "[command.work]\nshell='true'\ntimeout='1m'\nthen='pick'\n";
    for (path, text) in [
        (
            "script.pick.on.ok",
            SIMPLE.replace("on.ok='@done'", "on.ok='@user'"),
        ),
        (
            "script.pick.on.invalid",
            format!("{SIMPLE}on.invalid='@user'\n"),
        ),
        (
            "script.pick.on.timeout",
            format!("{SIMPLE}on.timeout='@user'\n"),
        ),
        (
            "sleep.pick.then",
            SIMPLE
                .replace("[script.pick]", "[sleep.pick]")
                .replace("run='true'\non.ok='@done'", "sleep='1m'\nthen='@user'"),
        ),
        (
            "agent.work.on.asked",
            format!(
                "{SIMPLE}[agent.work]\nlifecycle='turn'\nagent='claude'\nprompt-file='prompts/build.md'\nthen='pick'\non.asked='@user'\n"
            ),
        ),
        (
            "command.work.on.cap",
            format!("{SIMPLE}{command}on.cap='@user'\n"),
        ),
        (
            "command.work.then",
            format!("{SIMPLE}{}", command.replace("then='pick'", "then='@user'")),
        ),
        (
            "workflow.on.deadline",
            SIMPLE.replace("[workflow]", "[workflow]\non.deadline='@user'"),
        ),
        (
            "workflow.on-break",
            SIMPLE.replace("on-break='@failed'", "on-break='@user'"),
        ),
    ] {
        let (_home, dir) = fixture();
        fs::write(dir.join("workflow.toml"), text).unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        assert!(
            report.errors.iter().any(|issue| issue.path == path
                && issue.message.contains("@user is not a route target")
                && issue.message.contains("route to a node")
                && issue.message.contains("[workflow] on-break")),
            "{path}: {report:?}"
        );
    }
}

#[test]
fn an_invalid_catchall_refuses_whether_or_not_it_is_needed() {
    let (_home, dir) = fixture();
    for handler in ["42", "[]", "{}", "{agent=42}"] {
        let text = SIMPLE.replace("on-break='@failed'", &format!("on-break={handler}"));
        for text in [text.clone(), text + "on.invalid='@failed'\n"] {
            fs::write(dir.join("workflow.toml"), &text).unwrap();
            let report = validate(&dir, &LoadedDefinitions::default(), None);
            assert_eq!(
                report
                    .errors
                    .iter()
                    .map(|issue| issue.path.as_str())
                    .collect::<Vec<_>>(),
                [if handler.starts_with('{') {
                    "workflow.on-break.agent"
                } else {
                    "workflow.on-break"
                }],
                "{report:?}"
            );
        }
    }
}

#[test]
fn only_agents_ask_reserves_break_for_the_handler() {
    let base = SIMPLE.replace("on-break='@failed'", "on-break={agent='claude'}");
    for (path, text) in [
        (
            "script.pick.on.ok",
            base.replace("on.ok='@done'", "on.ok='@break'"),
        ),
        (
            "workflow.start",
            base.replace("start='pick'", "start='@break'"),
        ),
        (
            "command.@break",
            format!("{base}[command.'@break']\nshell='true'\ntimeout='1m'\nthen='pick'\n"),
        ),
        (
            "script.@break",
            format!("{base}[script.'@break']\nrun='true'\non.ok='@done'\n"),
        ),
        (
            "sleep.@break",
            format!("{base}[sleep.'@break']\nsleep='1m'\nthen='@done'\n"),
        ),
    ] {
        let (_home, dir) = fixture();
        fs::write(dir.join("workflow.toml"), text).unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        assert!(
            report.errors.iter().any(|issue| issue.path == path
                && issue.message.contains("@break")
                && issue.message.contains("on-break")),
            "{path}: {report:?}"
        );
    }
}

#[test]
fn judge_summary_is_required_and_one_line() {
    let base = SIMPLE.replace("[script.pick]", "[judge.pick]").replace(
        "run='true'",
        "agent='claude'\nprompt-file='prompts/build.md'",
    );
    let actual = [
        "",
        "summary=''",
        "summary=\"two\\nlines\"",
        "summary='Reviews correctness'",
    ]
    .map(|summary| {
        let (_home, dir) = fixture();
        fs::write(dir.join("workflow.toml"), format!("{base}{summary}\n")).unwrap();
        validate(&dir, &LoadedDefinitions::default(), None)
            .errors
            .into_iter()
            .map(|issue| {
                let required = issue
                    .message
                    .contains("a judge check requires a one-line summary of what it reviews")
                    && issue.message.contains("add summary = \"...\"");
                (issue.path, required)
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(
        actual,
        [
            vec![("judge.pick.summary".to_owned(), true)],
            vec![("judge.pick.summary".to_owned(), false)],
            vec![("judge.pick.summary".to_owned(), false)],
            vec![],
        ]
    );
}

#[test]
fn keep_timeout_and_asked_are_taken_on_both_lifecycles() {
    let lifecycles = [
        "lifecycle='turn'",
        "lifecycle='session'",
        "lifecycle='session'\nrest-check=['pick']",
    ];
    for lifecycle in lifecycles {
        for keep in ["true", "false"] {
            let text = SESSION_PICK.replace(
                "lifecycle='session'\nrest-check=['pick']",
                &format!("{lifecycle}\nkeep={keep}\ntimeout='1m'\non.asked='pick'"),
            );
            assert_eq!(errors(&text), Vec::<String>::new(), "{text}");
            let spec = loaded(&text).unwrap();
            assert_eq!(spec.authored().steps["build"].keep, Some(keep == "true"));
        }
    }
    assert_eq!(
        refusal(&SESSION_PICK.replace("then='pick'", "then='pick'\nkeep='yes'")).0,
        "agent.build.keep"
    );
}

#[test]
fn an_agent_names_turn_or_session_and_a_session_alone_takes_rest_check_and_subscribe() {
    let named = SESSION_PICK.replace("lifecycle='session'\n", "");
    for text in [
        named.clone(),
        named.replace("[agent.build]", "[agent.build]\nlifecycle='stay'"),
        named.replace("[agent.build]", "[agent.build]\nlifecycle='until'"),
    ] {
        assert_eq!(refusal(&text).0, "agent.build.lifecycle", "{text}");
    }
    assert_eq!(
        refusal(&SESSION_PICK.replace("then='pick'", "then=['pick']")).0,
        "agent.build.then"
    );
    let session = KINDS.replace("lifecycle='turn'", "lifecycle='session'");
    let actual = [
        with("[agent.build]", "rest-check=['after']"),
        with("[agent.build]", "subscribe=['review.ready']"),
        session.replace("then='after'", "then='after'\nrest-check=[]"),
        session.replace("then='after'", "then='after'\nrest-check=['run']"),
        session.replace("then='after'", "then='after'\nrest-check=['nap']"),
        session.replace("then='after'", "then='after'\nrest-check=['@done']"),
        session.replace("then='after'", "then='after'\nrest-check=['missing']"),
        session.replace(
            "then='after'",
            "then='after'\nrest-check=['run','after','nap']",
        ),
        session.replace("then='after'", "then='after'\nrest-check=['after','gate']"),
        session.replace("then='after'", "then='after'\nsubscribe=['review.ready']"),
    ]
    .map(|text| errors(&text));
    let rest = vec!["agent.build.rest-check".to_owned()];
    assert_eq!(
        actual,
        [
            rest.clone(),
            vec!["agent.build.subscribe".to_owned()],
            rest.clone(),
            rest.clone(),
            rest.clone(),
            rest.clone(),
            rest.clone(),
            [rest.clone(), rest].concat(),
            vec![],
            vec![],
        ]
    );
}

#[test]
fn a_rest_check_is_an_edge_the_cycle_bound_and_the_walk_see() {
    let text = SESSION_PICK.replace("then='pick'\nmax-visits=3", "then='@done'");
    let (_home, dir) = fixture();
    fs::write(dir.join("workflow.toml"), &text).unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    assert_eq!(
        report
            .errors
            .iter()
            .map(|issue| (issue.path.as_str(), issue.message.as_str()))
            .collect::<Vec<_>>(),
        [(
            "agent.build",
            "example: agent.build: cycle through steps has no max-visits bound"
        )],
        "{report:?}"
    );
    let bounded = text.replace("then='@done'", "then='@done'\nmax-visits=3");
    assert_eq!(errors(&bounded), Vec::<String>::new());
    assert_eq!(
        loaded(&bounded).unwrap().walk().unreachable,
        Vec::<String>::new()
    );
}

#[test]
fn route_tables_refuse_at_their_route() {
    for route in [
        "{to='@done'}",
        "{to='@done',delay='30m'}",
        "{to='@done',default=true}",
    ] {
        let error =
            loaded(&SESSION_PICK.replace("on.ok='@done'", &format!("on.ok={route}"))).unwrap_err();
        let SpecErr::Toml { path, source } = error else {
            panic!("{error}")
        };
        assert_eq!(
            (path.as_str(), source.message()),
            ("script.pick.on.ok", "invalid type: map, expected a string")
        );
    }
}

#[test]
fn syntax_errors_keep_line_column_and_excerpt() {
    for (text, location, excerpt) in [
        (
            "[workflow]\nscope='worktree'\nstart='a'\nstart='b'\n",
            "line 4, column 1",
            "start='b'",
        ),
        (
            "[workflow]\nscope='worktree'\n[script.a\nrun='true'\n",
            "line 3, column 10",
            "[script.a",
        ),
    ] {
        let error = loaded(text).unwrap_err().to_string();
        assert!(
            error.contains(location) && error.contains(excerpt) && error.contains('^'),
            "{error}"
        );
    }
}

#[test]
fn required_fields_refuse_at_the_exact_dotted_path() {
    let session = "[workflow]\non-break='@failed'\nscope='worktree'\nstart='build'\n[agent.build]\nagent='claude'\nprompt-file='prompts/build.md'\nlifecycle='session'\nthen='@done'\n";
    let actual = [
        (SIMPLE.replace("scope='worktree'\n", ""), "workflow.scope"),
        (SIMPLE.replace("start='pick'\n", ""), "workflow.start"),
        (SIMPLE.replace("start='pick'", "start=5"), "workflow.start"),
        (
            SIMPLE.replace("scope='worktree'", "scope=5"),
            "workflow.scope",
        ),
        (
            SIMPLE.replace(
                "[workflow]\non-break='@failed'\nscope='worktree'\nstart='pick'\n",
                "",
            ),
            "workflow",
        ),
        (
            with("[agent.build]", "").replace("agent='claude'\n", ""),
            "agent.build.agent",
        ),
        (
            with("[judge.judge]", "").replace("[judge.judge]\n\nagent='claude'", "[judge.judge]\n"),
            "judge.judge.agent",
        ),
        (
            with("[message.tell]", "").replace("message='@planner'\n", ""),
            "message.tell.message",
        ),
        (
            with("[sleep.nap]", "").replace("sleep='1m'\n", ""),
            "sleep.nap.sleep",
        ),
        (
            with("[condition.gate]", "")
                .replace("cases=[{when=['pr=merged'],outcome='yes'}]\n", ""),
            "condition.gate.cases",
        ),
        (
            CONDITION.replace(",outcome='yes'", ""),
            "condition.pick.cases.0.outcome",
        ),
        (
            CONDITION.replace("when=['pr=merged'],", ""),
            "condition.pick.cases.0.when",
        ),
        (
            CONDITION.replace("outcome='yes'", "outcome=5"),
            "condition.pick.cases.0.outcome",
        ),
        (
            CONDITION.replace("cases=[{when=['pr=merged'],outcome='yes'}]", "cases=[5]"),
            "condition.pick.cases.0",
        ),
        (
            format!("{SIMPLE}wake=[{{match={{path='*'}}}}]\n"),
            "script.pick.wake.0.signal",
        ),
        (
            format!("{SIMPLE}wake=[{{signal=5}}]\n"),
            "script.pick.wake.0.signal",
        ),
        (
            format!("{SIMPLE}wake=[5,{{match=5}}]\n"),
            "script.pick.wake.0",
        ),
        (
            format!("{SIMPLE}wake=['review.ready',{{match={{path='*'}}}}]\n"),
            "script.pick.wake.1.signal",
        ),
        (
            format!("{session}subscribe=[{{match={{path='*'}}}}]\n"),
            "agent.build.subscribe.0.signal",
        ),
        (
            format!("{session}subscribe=['review.ready',{{signal=5,match={{path='*'}}}}]\n"),
            "agent.build.subscribe.1.signal",
        ),
    ]
    .map(|(text, expected)| (refusal(&text).0, expected.to_owned()));
    assert!(
        actual.iter().all(|(path, expected)| path == expected),
        "{actual:#?}"
    );
}

#[test]
fn run_and_shell_each_need_exactly_one() {
    for (header, invocation) in [
        ("[command.run]", "shell='true'\n"),
        ("[test.test]", "shell='true'\n"),
        ("[script.after]", "run='true'\n"),
    ] {
        let name = header.trim_matches(['[', ']']);
        let neither = KINDS.replacen(
            &format!("{header}\n{invocation}"),
            &format!("{header}\n"),
            1,
        );
        let both = KINDS.replacen(
            &format!("{header}\n{invocation}"),
            &format!("{header}\nrun='true'\nshell='true'\n"),
            1,
        );
        for text in [neither, both] {
            assert_eq!(errors(&text), [name.to_owned()], "{text}");
        }
    }
}

#[test]
fn create_and_validate_agree_on_refusals() {
    let mut actual = Vec::new();
    for text in [
        SIMPLE.to_owned(),
        format!("{SIMPLE}future=true\n"),
        format!("{SIMPLE}timeout='soon'\n"),
        SIMPLE.replace("on.ok='@done'", "on.ok='missing'"),
        SIMPLE.replace(
            "[script.pick]\nrun='true'",
            "[judge.pick]\nagent='claude'\nprompt-file=5",
        ),
        SIMPLE.replace("on-break='@failed'\n", ""),
        SIMPLE.replace("on.ok='@done'", "on.ok='@user'"),
    ] {
        let (_home, dir) = fixture();
        fs::write(dir.join("workflow.toml"), text).unwrap();
        let state = crate::disk::paths::StatePaths::for_project_root_under(&dir, &dir).unwrap();
        let room = serde_json::from_value(serde_json::json!({
            "layout": crate::disk::paths::WORKSPACE_LAYOUT,
            "workspace_id": state.workspace_id, "project_root": dir,
            "session_name": "test", "updated_at": jiff::Timestamp::now(),
        }))
        .unwrap();
        crate::workspace::record::write(&state, &room).unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), Some(&dir));
        let result = create(
            &state,
            DefinitionSource::Deployed {
                dir: &dir,
                task: None,
                surplus: None,
                surplus_after: None,
                fire: None,
                fire_facts: None,
            },
            ScopeRoots {
                checkout: dir.clone(),
                project: dir.clone(),
            },
            || Ok(None),
            false,
            Default::default(),
        );
        actual.push((
            report.errors.is_empty(),
            result.is_ok(),
            format!("{report:?}; {result:?}"),
        ));
    }
    assert_eq!(
        actual
            .iter()
            .map(|(valid, created, _)| (*valid, *created))
            .collect::<Vec<_>>(),
        [
            (true, true),
            (false, false),
            (false, false),
            (false, false),
            (false, false),
            (false, false),
            (false, false)
        ],
        "{actual:#?}"
    );
}
