use super::*;

const SIMPLE: &str =
    "[workflow]\nscope='worktree'\nstart='pick'\n[checks.pick]\nrun='true'\non.ok='@done'\n";
const CONDITION: &str = "[workflow]\nscope='worktree'\nstart='pick'\n[checks.pick]\nelse='no'\non={yes='@done',no='@done',unknown='@user'}\ncases=[{when=['pr=merged'],outcome='yes'}]\n";
const DELAY: &str = "[workflow]\nscope='worktree'\nstart='build'\n[steps.build]\nagent='claude'\nprompt-file='prompts/build.md'\nlifecycle='until'\nthen='pick'\nmax-visits=3\n[checks.pick]\nrun='true'\ntimeout='1h'\non.ok={to='@done',delay='30m'}\non.pending='build'\n";

fn loaded(text: &str) -> Result<WorkflowSpec, SpecErr> {
    let (_home, dir) = fixture();
    fs::write(dir.join("workflow.toml"), text).unwrap();
    load(&dir)
}

#[test]
fn judge_summary_is_required_after_invalid_optional_summaries_are_ignored() {
    let (_home, dir) = fixture();
    let base = SIMPLE.replace(
        "run='true'",
        "agent='claude'\nprompt-file='prompts/build.md'",
    );
    let mut actual = Vec::new();
    for summary in [
        "",
        "summary=''",
        "summary=\"two\\nlines\"",
        "summary='Reviews correctness'",
    ] {
        fs::write(dir.join("workflow.toml"), format!("{base}{summary}\n")).unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        actual.push(report.errors.iter().any(|issue| {
            issue.path == "checks.pick.summary"
                && issue
                    .message
                    .contains("a judge check requires a one-line summary of what it reviews")
                && issue.message.contains("add summary = \"...\"")
        }));
        if summary.contains("Reviews correctness") {
            assert!(report.errors.is_empty(), "{report:?}");
        }
    }
    assert_eq!(actual, [true, true, true, false]);
    let land = "[workflow]\nscope='worktree'\nstart='collect'\n[steps.collect]\nshell='true'\ntimeout='1m'\nthen='notes'\n[checks.notes]\nrun='true'\non={unsure='notes-read',settled='@done'}\n[checks.notes-read]\nagent='claude'\nprompt-file='prompts/build.md'\nsummary='Decide whether the notes need work'\non.settled='@done'\n";
    fs::write(dir.join("workflow.toml"), land).unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    assert!(report.errors.is_empty(), "{report:?}");
}

#[test]
fn keep_warns_and_is_ignored_except_on_turn_and_until_agent_steps() {
    let mut actual = Vec::new();
    for (kind, lifecycle, keep) in [
        (
            "agent='claude'\nprompt-file='prompts/build.md'",
            "turn",
            "true",
        ),
        (
            "agent='claude'\nprompt-file='prompts/build.md'",
            "until",
            "false",
        ),
        (
            "agent='claude'\nprompt-file='prompts/build.md'",
            "stay",
            "true",
        ),
        ("run='true'", "turn", "true"),
        (
            "agent='claude'\nprompt-file='prompts/build.md'",
            "until",
            "'yes'",
        ),
    ] {
        let text = DELAY
            .replace("agent='claude'\nprompt-file='prompts/build.md'", kind)
            .replace(
                "lifecycle='until'",
                &format!("lifecycle='{lifecycle}'\nkeep={keep}"),
            );
        let spec = loaded(&text).unwrap();
        actual.push((
            spec.steps["build"].keep,
            spec.ignored
                .iter()
                .any(|issue| issue.path == "steps.build.keep"),
        ));
    }
    assert_eq!(
        actual,
        [
            (Some(true), false),
            (Some(false), false),
            (None, true),
            (None, true),
            (None, true)
        ]
    );
    for prefix in ["workflow", "checks.pick"] {
        for keep in ["true", "'yes'"] {
            let spec = loaded(
                &SIMPLE.replace(&format!("[{prefix}]"), &format!("[{prefix}]\nkeep={keep}")),
            )
            .unwrap();
            assert!(
                spec.ignored
                    .iter()
                    .any(|issue| issue.path == format!("{prefix}.keep"))
            );
        }
    }
}

#[test]
fn delayed_routes_load_and_only_delayed_edges_serialize_a_delay() {
    let result = loaded(DELAY);
    assert!(result.is_ok(), "a delayed route must load: {result:?}");
    let spec = result.unwrap();
    assert!(spec.ignored.is_empty(), "{:?}", spec.ignored);
    let edges = serde_json::to_value(spec.edges("pick")).unwrap();
    let edges = edges.as_array().unwrap();
    assert_eq!(
        edges.iter().find(|edge| edge["outcome"] == "ok").unwrap(),
        &serde_json::json!({
            "outcome":"ok", "target":"@done", "source":"explicit", "fires":"yes", "delay":"30m"
        })
    );
    assert!(
        edges
            .iter()
            .filter(|edge| edge["outcome"] != "ok")
            .all(|edge| edge.get("delay").is_none()),
        "{edges:?}"
    );
}

#[test]
fn omitted_lifecycle_keeps_delays_bans_sleep_and_explains_turn_only_fields() {
    let text = DELAY.replace("lifecycle='until'\n", "");
    let spec = loaded(&text).unwrap();
    let delay_kept = spec.checks["pick"].on["ok"].delay() == Some("30m");
    let (_home, dir) = fixture();
    fs::write(
        dir.join("workflow.toml"),
        text.replace("run='true'\ntimeout='1h'", "sleep='1m'\nthen='@done'"),
    )
    .unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    let sleep_refused_with_fix = report.errors.iter().any(|issue| {
        issue.path == "steps.build.then" && issue.message.contains("lifecycle = \"turn\"")
    });
    let spec = loaded(&text.replace(
        "max-visits=3",
        "max-visits=3\ntimeout='1m'\non.asked='pick'",
    ))
    .unwrap();
    let cleanup = ["steps.build.timeout", "steps.build.on.asked"].map(|path| {
        spec.ignored
            .iter()
            .any(|issue| issue.path == path && issue.message.contains("lifecycle = \"turn\""))
    });
    assert_eq!(
        (
            delay_kept,
            sleep_refused_with_fix,
            spec.steps["build"].timeout.is_none(),
            spec.steps["build"].on.asked.is_none(),
            cleanup,
        ),
        (true, true, true, true, [true, true]),
        "default lifecycle must use until rules: {report:?}; {:?}",
        spec.ignored
    );
}

#[test]
fn invalid_or_misplaced_delays_warn_and_keep_the_route() {
    let mut cases = vec![
        (
            DELAY.replace("lifecycle='until'", "lifecycle='turn'"),
            "@done",
        ),
        (DELAY.replace("to='@done'", "to='@wait'"), "@wait"),
        (DELAY.replace("to='@done'", "to='build'"), "build"),
    ];
    for delay in ["42", "[]", "'soon'", "'0s'"] {
        cases.push((DELAY.replace("'30m'", delay), "@done"));
    }
    for (text, target) in cases {
        let result = loaded(&text);
        assert!(
            result.is_ok(),
            "an optional delay must not refuse: {result:?}"
        );
        let spec = result.unwrap();
        assert_eq!(spec.target("pick", "ok").as_deref(), Some(target));
        assert_eq!(spec.ignored.len(), 1, "{text}: {:?}", spec.ignored);
        assert_eq!(spec.ignored[0].path, "checks.pick.on.ok.delay");
        let edges = serde_json::to_value(spec.edges("pick")).unwrap();
        assert!(
            edges
                .as_array()
                .unwrap()
                .iter()
                .all(|edge| edge.get("delay").is_none())
        );
    }
}

#[test]
fn delay_placement_reads_the_step_after_its_misplaced_lifecycle_is_ignored() {
    let text = DELAY.replace(
        "agent='claude'\nprompt-file='prompts/build.md'",
        "run='true'\ntimeout='1m'",
    );
    let spec = loaded(&text).unwrap();
    assert!(spec.steps["build"].lifecycle.is_none());
    assert!(
        spec.ignored
            .iter()
            .any(|issue| issue.path == "checks.pick.on.ok.delay"),
        "a command step cannot make a delay valid: {:?}",
        spec.ignored
    );
}

#[test]
fn route_tables_require_a_string_target_and_warn_on_unknown_keys() {
    for (route, reason) in [
        ("{}", "missing field `to`"),
        ("{delay='30m'}", "missing field `to`"),
        (
            "{to=42,delay='30m'}",
            "invalid type: integer `42`, expected a string",
        ),
    ] {
        let error = loaded(&DELAY.replace("{to='@done',delay='30m'}", route)).unwrap_err();
        let SpecErr::Toml { path, source } = error else {
            panic!("{error}")
        };
        assert_eq!(
            (path.as_str(), source.message()),
            ("checks.pick.on.ok", reason)
        );
    }
    let result = loaded(&DELAY.replace("delay='30m'", "delay='30m',future=true"));
    assert!(
        result.is_ok(),
        "unknown route fields must only warn: {result:?}"
    );
    let spec = result.unwrap();
    assert_eq!(spec.ignored.len(), 1, "{:?}", spec.ignored);
    assert_eq!(spec.ignored[0].path, "checks.pick.on.ok.future");
    let dead = CONDITION.replace("yes='@done'", "yes='@done',dead={delay='30m'}");
    let result = loaded(&dead);
    assert!(
        result.is_ok(),
        "a malformed dead route must only warn: {result:?}"
    );
    assert_eq!(result.unwrap().ignored[0].path, "checks.pick.on.dead");
}

#[test]
fn per_outcome_number_and_reset_tables_load_without_ignored_fields() {
    for cap in [
        "2",
        "{limit=2}",
        "{limit=2,reset-on='ready'}",
        "{limit=2,reset-on=['ready','done']}",
    ] {
        let text = VALID.replace(
            "max-visits = 3",
            &format!("max-visits=3\nmax-visits-per-outcome={cap}"),
        );
        let spec = loaded(&text).unwrap();
        assert!(spec.ignored.is_empty(), "{cap}: {:?}", spec.ignored);
    }
}

#[test]
fn per_outcome_inner_unknown_fields_warn_and_invalid_values_drop_the_whole_cap() {
    let path = "steps.build.max-visits-per-outcome";
    let text = VALID.replace(
        "max-visits = 3",
        "max-visits=3\nmax-visits-per-outcome={limit=2,future=true}",
    );
    let spec = loaded(&text).unwrap();
    assert_eq!(
        spec.ignored
            .iter()
            .map(|issue| issue.path.as_str())
            .collect::<Vec<_>>(),
        [format!("{path}.future")]
    );
    for cap in [
        "0",
        "-1",
        "'two'",
        "[]",
        "{}",
        "{limit=0,reset-on='ready'}",
        "{limit=2,reset-on=[]}",
        "{limit=2,reset-on=42}",
        "{limit='two'}",
        "{limit=2,reset-on=['ready',42]}",
        "{limit=3,reset-on='ready'}",
    ] {
        let spec = loaded(&VALID.replace(
            "max-visits = 3",
            &format!("max-visits=3\nmax-visits-per-outcome={cap}"),
        ))
        .unwrap();
        assert_eq!(spec.ignored.len(), 1, "{cap}: {:?}", spec.ignored);
        assert_eq!(spec.ignored[0].path, path, "{cap}");
    }
}

#[test]
fn old_per_key_caps_and_case_keys_warn_unknown_and_do_not_cap_steps() {
    let text = VALID.replace("max-visits = 3", "max-visits=3\nmax-visits-per-key=1");
    let spec = loaded(&text).unwrap();
    assert!(
        spec.ignored
            .iter()
            .any(|issue| issue.path == "steps.build.max-visits-per-key"
                && issue.message.contains("unknown field"))
    );
    let spec = loaded(&CONDITION.replace("outcome='yes'", "outcome='yes',key='ci'")).unwrap();
    assert!(
        spec.ignored
            .iter()
            .any(|issue| issue.path == "checks.pick.cases.0.key"
                && issue.message.contains("unknown field"))
    );
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
            "[workflow]\nscope='worktree'\n[checks.a\nrun='true'\n",
            "line 3, column 10",
            "[checks.a",
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
fn unknown_keys_warn_at_every_table_including_wake_bindings() {
    let text = format!(
        "future=true\n{}",
        VALID
            .replace("[workflow]", "[workflow]\nfuture=true\non.future=true")
            .replace(
                "[checks.pick]",
                "[checks.pick]\nfuture=true\nwake=[{signal='review.ready',future=true}]"
            )
            .replace(
                "[steps.build]",
                "[steps.build]\nfuture=true\non.future=true"
            )
    );
    let cases = CONDITION.replace("outcome='yes'", "outcome='yes',future=true");
    let actual = [text, cases].map(|text| {
        loaded(&text)
            .map(|spec| {
                let mut paths = spec
                    .ignored
                    .iter()
                    .map(|i| i.path.as_str())
                    .collect::<Vec<_>>();
                paths.sort_unstable();
                let paths = paths.into_iter().map(str::to_owned).collect::<Vec<_>>();
                let messages = spec.ignored.iter().all(|i| {
                    i.message.contains(&i.path)
                        && i.message.contains("ignored")
                        && i.message.contains("unknown field")
                });
                (paths, messages)
            })
            .map_err(|err| err.to_string())
    });
    assert_eq!(
        actual,
        [
            Ok((
                vec![
                    "checks.pick.future",
                    "checks.pick.wake.0.future",
                    "future",
                    "steps.build.future",
                    "steps.build.on.future",
                    "workflow.future",
                    "workflow.on.future"
                ]
                .into_iter()
                .map(str::to_owned)
                .collect(),
                true
            )),
            Ok((vec!["checks.pick.cases.0.future".into()], true)),
        ]
    );
}

#[test]
fn wrong_typed_optionals_are_absent_and_other_wakes_survive() {
    let actual = loaded(&format!(
        "{}throttle=5\ntakeover='yes'\n",
        VALID
            .replace("lifecycle = \"turn\"", "lifecycle='forever'")
            .replace("max-visits = 3", "max-visits = '3'")
            .replace(
                "[checks.pick]",
                "[checks.pick]\nwhen=7\nwake=[5,'review.ready',false,'team.stage']"
            )
    ));
    assert!(
        actual.is_ok(),
        "optional values must not prevent loading: {actual:?}"
    );
    let spec = actual.unwrap();
    let step = &spec.steps["build"];
    let mut paths = spec
        .ignored
        .iter()
        .map(|i| i.path.as_str())
        .collect::<Vec<_>>();
    paths.sort_unstable();
    assert_eq!(
        (
            step.max_visits,
            step.lifecycle,
            step.throttle,
            step.takeover,
            spec.checks["pick"].subscriptions().unwrap().len(),
            paths
        ),
        (
            None,
            None,
            None,
            None,
            2,
            vec![
                "checks.pick.wake.0",
                "checks.pick.wake.2",
                "checks.pick.when",
                "steps.build.lifecycle",
                "steps.build.max-visits",
                "steps.build.takeover",
                "steps.build.throttle"
            ]
        )
    );
    let result = loaded(&CONDITION.replace("outcome='yes'", "outcome='yes',for=5,key=true"));
    assert!(
        result.is_ok(),
        "optional case fields must be ignored: {result:?}"
    );
    let spec = result.unwrap();
    assert!(spec.checks["pick"].cases[0].hold.is_none());
    assert!(
        spec.ignored
            .iter()
            .any(|issue| issue.path == "checks.pick.cases.0.key"
                && issue.message.contains("unknown field"))
    );
    assert_eq!(spec.ignored.len(), 2);
}

#[test]
fn required_fields_refuse_at_the_exact_dotted_path() {
    let actual = [
        (SIMPLE.replace("scope='worktree'\n", ""), "workflow.scope"),
        (SIMPLE.replace("start='pick'\n", ""), "workflow.start"),
        (SIMPLE.replace("start='pick'", "start=5"), "workflow.start"),
        (
            SIMPLE.replace("scope='worktree'", "scope=5"),
            "workflow.scope",
        ),
        (
            CONDITION.replace(",outcome='yes'", ""),
            "checks.pick.cases.0.outcome",
        ),
        (
            CONDITION.replace("when=['pr=merged'],", ""),
            "checks.pick.cases.0.when",
        ),
        (
            CONDITION.replace("outcome='yes'", "outcome=5"),
            "checks.pick.cases.0.outcome",
        ),
        (
            CONDITION.replace("cases=[{when=['pr=merged'],outcome='yes'}]", "cases=[5]"),
            "checks.pick.cases.0",
        ),
        (
            format!("{SIMPLE}wake=[{{match={{path='*'}}}}]\n"),
            "checks.pick.wake.0.signal",
        ),
        (
            format!("{SIMPLE}wake=[{{signal=5}}]\n"),
            "checks.pick.wake.0.signal",
        ),
        (
            format!("{SIMPLE}wake=[{{match=5}}]\n"),
            "checks.pick.wake.0.signal",
        ),
        (
            format!("{SIMPLE}wake=[{{signal=5,match={{a=1}}}}]\n"),
            "checks.pick.wake.0.signal",
        ),
        (
            format!("{SIMPLE}wake=[5,{{match=5}}]\n"),
            "checks.pick.wake.1.signal",
        ),
        (
            format!("{SIMPLE}wake=[5,{{signal=5,match={{a=1}}}}]\n"),
            "checks.pick.wake.1.signal",
        ),
    ]
    .map(|(text, expected)| {
        let path = match loaded(&text) {
            Err(SpecErr::Toml { path, .. }) => path,
            other => format!("{other:?}"),
        };
        (path, expected.to_owned())
    });
    assert!(
        actual.iter().all(|(path, expected)| path == expected),
        "{actual:#?}"
    );
}

#[test]
fn wrong_typed_live_routes_refuse_but_dead_routes_warn() {
    let command = VALID.replace(
        "agent = \"claude\"\nprompt-file = \"prompts/build.md\"\nlifecycle = \"turn\"",
        "shell='true'\ntimeout='1m'",
    );
    let cases = [
        (
            format!("{SIMPLE}timeout='1m'\non.pending='@wait'\non.timeout=5\n"),
            "checks.pick.on.timeout",
            true,
        ),
        (
            SIMPLE.replace("on.ok='@done'", "on.ok=5"),
            "checks.pick.on.ok",
            true,
        ),
        (
            format!("{SIMPLE}on.invalid=5\n"),
            "checks.pick.on.invalid",
            true,
        ),
        (
            CONDITION.replace("yes='@done'", "yes=5"),
            "checks.pick.on.yes",
            true,
        ),
        (format!("{VALID}on.cap=5\n"), "steps.build.on.cap", true),
        (format!("{VALID}on.asked=5\n"), "steps.build.on.asked", true),
        (
            format!("{VALID}on.capacity=5\n"),
            "steps.build.on.capacity",
            true,
        ),
        (
            SIMPLE.replace("on.ok='@done'", "on=5"),
            "checks.pick.on",
            true,
        ),
        (format!("{VALID}on=5\n"), "steps.build.on", true),
        (
            SIMPLE.replace("start='pick'", "start='pick'\non=5"),
            "workflow.on",
            true,
        ),
        (
            SIMPLE.replace("start='pick'", "start='pick'\non.deadline=5"),
            "workflow.on.deadline",
            true,
        ),
        (format!("{SIMPLE}on.hold=5\n"), "checks.pick.on.hold", false),
        (
            SIMPLE.replace(
                "run='true'\non.ok='@done'",
                "shell='true'\non={pass='@done',fail='@failed',other=5}",
            ),
            "checks.pick.on.other",
            false,
        ),
        (
            CONDITION.replace("on={", "on={dead=5,"),
            "checks.pick.on.dead",
            false,
        ),
        (
            format!("{command}on.asked=5\n"),
            "steps.build.on.asked",
            false,
        ),
        (
            format!("{command}on.capacity=5\n"),
            "steps.build.on.capacity",
            false,
        ),
        (
            format!(
                "{}on.asked=5\n",
                VALID.replace("lifecycle = \"turn\"", "lifecycle='stay'")
            ),
            "steps.build.on.asked",
            false,
        ),
    ];
    let actual = cases
        .iter()
        .map(|(text, path, refuses)| {
            let (_home, dir) = fixture();
            fs::write(dir.join("workflow.toml"), text).unwrap();
            let result = load(&dir);
            let report = validate(&dir, &LoadedDefinitions::default(), None);
            let matches = match &result {
                Err(SpecErr::Toml { path: actual, .. }) => *refuses && actual == path,
                Ok(spec) => !*refuses && spec.ignored.len() == 1 && spec.ignored[0].path == *path,
                _ => false,
            };
            (
                matches && report.errors.is_empty() != *refuses,
                format!("{path}: {result:?}; {report:?}"),
            )
        })
        .collect::<Vec<_>>();
    assert!(actual.iter().all(|(matches, _)| *matches), "{actual:#?}");
}

#[test]
fn semantic_and_misplaced_values_are_cleared_before_validation() {
    let acyclic = VALID.replace("then = \"pick\"", "then='after'");
    let acyclic = format!("{acyclic}[checks.after]\nrun='true'\non.ok='@done'\n");
    let command = acyclic.replace(
        "agent = \"claude\"\nprompt-file = \"prompts/build.md\"\nlifecycle = \"turn\"",
        "shell='true'\ntimeout='1m'",
    );
    let mut cases = vec![
        (
            SIMPLE.replace("[workflow]", "[workflow]\nsummary='  '"),
            "workflow.summary",
        ),
        (
            SIMPLE.replace("[workflow]", "[workflow]\ndeadline='never'"),
            "workflow.deadline",
        ),
        (format!("{SIMPLE}summary='  '\n"), "checks.pick.summary"),
        (format!("{SIMPLE}timeout='soon'\n"), "checks.pick.timeout"),
        (
            SIMPLE.replace("on.ok='@done'", "on.ok={to='@done',delay='30m'}"),
            "checks.pick.on.ok.delay",
        ),
        (
            acyclic.replace("max-visits = 3", "summary='  '"),
            "steps.build.summary",
        ),
        (
            acyclic.replace("max-visits = 3", "timeout='soon'"),
            "steps.build.timeout",
        ),
        (
            acyclic.replace("max-visits = 3", "window='ci=passed'"),
            "steps.build.window",
        ),
        (
            acyclic.replace("max-visits = 3", "max-visits=0"),
            "steps.build.max-visits",
        ),
        (
            acyclic.replace("max-visits = 3", "max-visits=3\nmax-visits-per-outcome=0"),
            "steps.build.max-visits-per-outcome",
        ),
        (
            acyclic.replace("max-visits = 3", "max-visits=3\nmax-visits-per-outcome=3"),
            "steps.build.max-visits-per-outcome",
        ),
        (
            acyclic.replace("max-visits = 3", "max-visits=3\nmax-visits-per-outcome=4"),
            "steps.build.max-visits-per-outcome",
        ),
        (
            CONDITION.replace("outcome='yes'", "outcome='yes',for='0s'"),
            "checks.pick.cases.0.for",
        ),
        (
            CONDITION.replace("outcome='yes'", "outcome='yes',for='3ms'"),
            "checks.pick.cases.0.for",
        ),
        (
            CONDITION
                .replace("yes='@done'", "yes='@wait'")
                .replace("else='no'", "timeout='1h'\nelse='no'")
                .replace("outcome='yes'", "outcome='yes',for='1m'"),
            "checks.pick.cases.0.for",
        ),
        (
            CONDITION.replace("else='no'", "provider='missing-kind'\nelse='no'"),
            "checks.pick.provider",
        ),
        (
            CONDITION.replace("else='no'", "provider='claude'\nelse='no'"),
            "checks.pick.provider",
        ),
        (
            CONDITION.replace("else='no'", "account='work'\nelse='no'"),
            "checks.pick.account",
        ),
        (
            CONDITION.replace("else='no'", "wake=[]\nelse='no'"),
            "checks.pick.wake",
        ),
        (
            CONDITION.replace("yes='@done'", "dead='missing',yes='@done'"),
            "checks.pick.on.dead",
        ),
        (
            format!("{SIMPLE}on.hold='missing'\n"),
            "checks.pick.on.hold",
        ),
        (
            SIMPLE.replace(
                "run='true'\non.ok='@done'",
                "shell='true'\non={pass='@done',fail='@user',other='missing'}",
            ),
            "checks.pick.on.other",
        ),
    ];
    for (field, value) in [
        ("else", "'yes'"),
        ("provider", "'claude'"),
        ("account", "'work'"),
        ("prompt-file", "'missing.md'"),
        ("window", "'5h.left>=40'"),
    ] {
        cases.push((
            format!("{SIMPLE}{field}={value}\n"),
            match field {
                "else" => "checks.pick.else",
                "provider" => "checks.pick.provider",
                "account" => "checks.pick.account",
                "prompt-file" => "checks.pick.prompt-file",
                _ => "checks.pick.window",
            },
        ));
    }
    for (field, value) in [
        ("takeover", "false"),
        ("throttle", "'on'"),
        ("capacity-wait", "'1h'"),
        ("on.capacity", "'@user'"),
        ("lifecycle", "'stay'"),
        ("window", "'5h.left>=40'"),
        ("on.asked", "'@user'"),
        ("prompt-file", "'missing.md'"),
    ] {
        cases.push((
            command.replace("then='after'", &format!("{field}={value}\nthen='after'")),
            match field {
                "takeover" => "steps.build.takeover",
                "throttle" => "steps.build.throttle",
                "capacity-wait" => "steps.build.capacity-wait",
                "on.capacity" => "steps.build.on.capacity",
                "lifecycle" => "steps.build.lifecycle",
                "window" => "steps.build.window",
                "on.asked" => "steps.build.on.asked",
                _ => "steps.build.prompt-file",
            },
        ));
    }
    for lifecycle in ["stay", "until"] {
        for (field, value, path) in [
            ("timeout", "'1h'", "steps.build.timeout"),
            ("on.asked", "'@user'", "steps.build.on.asked"),
        ] {
            cases.push((
                acyclic
                    .replace("lifecycle = \"turn\"", &format!("lifecycle='{lifecycle}'"))
                    .replace("then='after'", &format!("{field}={value}\nthen='after'")),
                path,
            ));
        }
    }
    for wake in [
        "'bad selector'",
        "'ci.finished'",
        "{signal='ci.*', match={conclusion='success'}}",
        "'agent.*'",
        "'agent.idle'",
        "'team.idle'",
        "'team.*'",
        "{signal='review.ready',match={env=' '}}",
        "{signal='review.ready',match={env=42}}",
    ] {
        cases.push((format!("{SIMPLE}wake=[{wake}]\n"), "checks.pick.wake.0"));
    }
    let actual = cases
        .iter()
        .map(|(text, path)| {
            let (_home, dir) = fixture();
            fs::write(dir.join("workflow.toml"), text).unwrap();
            let result = load(&dir);
            let report = validate(&dir, &LoadedDefinitions::default(), None);
            let outcome = result.as_ref().map(|spec| {
                let value = match *path {
                    "workflow.summary" => spec.workflow.summary.is_none(),
                    "workflow.deadline" => spec.workflow.deadline.is_none(),
                    path if path.starts_with("steps.") => {
                        let step = &spec.steps["build"];
                        match path.strip_prefix("steps.build.").unwrap() {
                            "summary" => step.summary.is_none(),
                            "timeout" => step.timeout.is_none(),
                            "window" => step.window.is_none(),
                            "max-visits" => step.max_visits.is_none(),
                            "max-visits-per-outcome" => step.max_visits_per_outcome.is_none(),
                            "takeover" => step.takeover.is_none(),
                            "throttle" => step.throttle.is_none(),
                            "capacity-wait" => step.capacity_wait.is_none(),
                            "on.capacity" => step.on.capacity.is_none(),
                            "on.asked" => step.on.asked.is_none(),
                            "lifecycle" => step.lifecycle.is_none(),
                            "prompt-file" => step.prompt_file.is_none(),
                            _ => false,
                        }
                    }
                    path => {
                        let check = &spec.checks["pick"];
                        match path.strip_prefix("checks.pick.").unwrap() {
                            "summary" => check.summary.is_none(),
                            "timeout" => check.timeout.is_none(),
                            "cases.0.for" => check.cases[0].hold.is_none(),
                            "provider" => check.provider.is_none(),
                            "account" => check.account.is_none(),
                            "else" => check.else_outcome.is_none(),
                            "window" => check.window.is_none(),
                            "prompt-file" => check.prompt_file.is_none(),
                            "wake" => check.wake.is_none(),
                            "wake.0" => check.wake.as_ref().is_some_and(Vec::is_empty),
                            "on.ok.delay" => !serde_json::to_value(spec.edges("pick"))
                                .unwrap()
                                .as_array()
                                .unwrap()
                                .iter()
                                .any(|edge| edge.get("delay").is_some()),
                            field => !check.on.contains_key(field.strip_prefix("on.").unwrap()),
                        }
                    }
                };
                (
                    spec.ignored
                        .iter()
                        .map(|i| i.path.as_str())
                        .collect::<Vec<_>>()
                        == [*path],
                    value,
                    report.errors.is_empty(),
                    spec.ignored
                        .iter()
                        .all(|i| i.message.contains("ignored") && i.message.contains(&i.path)),
                )
            });
            (
                (*path).to_owned(),
                outcome.unwrap_or((false, false, false, false)),
                format!("{result:?}; {report:?}"),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual
            .iter()
            .map(|(path, outcome, _)| (path.as_str(), *outcome))
            .collect::<Vec<_>>(),
        cases
            .iter()
            .map(|(_, path)| (*path, (true, true, true, true)))
            .collect::<Vec<_>>(),
        "{actual:#?}"
    );
}

#[test]
fn cross_field_refusals_see_the_spec_after_drops() {
    let cases = [
        (
            VALID.replace("max-visits = 3", "max-visits=0"),
            "steps.build.max-visits",
            "cycle",
        ),
        (
            VALID.replace(
                "agent = \"claude\"\nprompt-file = \"prompts/build.md\"\nlifecycle = \"turn\"",
                "run='true'\ntimeout='soon'",
            ),
            "steps.build.timeout",
            "requires timeout",
        ),
        (
            VALID.replace("timeout = \"1d\"", "timeout='soon'"),
            "checks.pick.timeout",
            "a waiting check requires timeout",
        ),
        (
            CONDITION
                .replace(
                    "else='no'",
                    "provider='missing-kind'\naccount='work'\nelse='no'",
                )
                .replace("pr=merged", "window.5h.left>=40"),
            "checks.pick.provider",
            "window terms require provider",
        ),
    ];
    let actual = cases.map(|(text, path, error)| {
        let (_home, dir) = fixture();
        fs::write(dir.join("workflow.toml"), &text).unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        (
            report.warnings.iter().any(|i| i.path == path),
            report.errors.iter().any(|i| i.message.contains(error)),
            format!("{report:?}"),
        )
    });
    assert!(
        actual.iter().all(|(warning, error, _)| *warning && *error),
        "{actual:#?}"
    );
}

#[test]
fn create_and_validate_agree_on_ignored_fields_and_required_rules() {
    let mut actual = Vec::new();
    for text in [
        format!("{SIMPLE}future=true\n"),
        SIMPLE.replace("on.ok='@done'", "on.ok='missing'"),
        SIMPLE.replace("run='true'", "agent='claude'\nprompt-file=5"),
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
            &dir,
            ScopeRoots {
                checkout: dir.clone(),
                project: dir.clone(),
            },
            || Ok(None),
            None,
            false,
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
        [(true, true), (false, false), (false, false)],
        "{actual:#?}"
    );
}
