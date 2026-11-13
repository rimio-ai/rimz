use super::*;

const AT: &str = "2000-01-01T12:00:00Z";

const NOW_SPEC: &str = "[workflow]\nscope='worktree'\nstart='build'\n[steps.build]\nagent='claude'\nprompt-file='prompt.md'\nlifecycle='until'\nthen=['gate','later']\n[checks.gate]\nelse='pending'\non={ready='later',pending='build',unknown='build'}\ncases=[{when=['team.stage=Done'],outcome='ready',for='2m'}]\n[checks.later]\nshell='true'\non={pass='@done',fail='build'}\n";

#[test]
fn active_break_resume_receipt_counts_the_destination_step_visits() {
    let (env, mut record) = fixture("active", None, vec![]);
    record.node = "@break".into();
    record.input = Some(json!({"outcome":"died", "edge":"invalid", "from":"build"}));
    record.visits.insert("build".into(), 1);
    record.attempt = Some(
        serde_json::from_value(json!({
            "kind":"agent", "lifecycle":"stay", "seq":1, "node":"@break", "visit":1,
            "started_at":record.entered_at, "host":{"pid":0,"start":null}
        }))
        .unwrap(),
    );
    save(&env, &record);
    sealed_spec(
        &env,
        &record,
        "[workflow]\nscope='worktree'\nstart='build'\non-break={agent='claude'}\n[steps.build]\nagent='claude'\nprompt-file='prompt.md'\nlifecycle='turn'\nmax-visits=2\nthen='@done'\n",
    );
    let receipt = success(
        env.rimz()
            .args(["workflow", "resume", record.id.as_str()])
            .env("RIMZ_BIN", "/bin/true")
            .output()
            .unwrap(),
    );
    assert!(
        receipt.starts_with("resumed review#project at build · build 1 of 2 visits\n"),
        "{receipt}"
    );
}

#[test]
fn active_break_resume_receipts_its_source_instead_of_an_extension() {
    for live in [true, false] {
        for at in [None, Some("other")] {
            let (env, mut record) = fixture("active", None, vec![]);
            record.node = "@break".into();
            record.input = Some(json!({"outcome":"invalid","edge":"invalid","from":"gate"}));
            if live {
                record.attempt = Some(
                    serde_json::from_value(json!({
                        "kind":"agent", "lifecycle":"stay", "seq":1, "node":"@break",
                        "visit":1, "started_at":record.entered_at,
                        "host":{"pid":0,"start":null},
                    }))
                    .unwrap(),
                );
                record.next_seq = 2;
            }
            save(&env, &record);
            sealed_spec(
                &env,
                &record,
                "[workflow]\nscope='worktree'\nstart='gate'\non-break={agent='claude'}\n\
                 [checks.gate]\nsleep='1h'\nthen='@done'\n\
                 [checks.other]\nsleep='1h'\nthen='@done'\n",
            );
            let mut args = vec!["workflow", "resume", record.id.as_str()];
            if let Some(at) = at {
                args.extend(["--at", at]);
            }
            let target = at.unwrap_or("gate");
            let receipt = success(command(&env, &args));
            assert!(
                receipt.starts_with(&format!("resumed review#project at {target}\n")),
                "{receipt}"
            );
            let value = until(&env, record.id.as_str(), |value| {
                value["record"]["node"] == target
            });
            let history = human(&env, &record, true);
            assert!(
                history.contains(&format!("{target} resume → {target} ·")),
                "live={live}, at={at:?}: {history}"
            );
            let resumes = events(&value, "resume");
            assert_eq!(resumes.len(), 1, "{value:#}");
            assert_eq!(resumes[0]["node"], target, "{value:#}");
        }
    }
}

#[test]
fn show_now_reads_the_first_condition_on_active_steps_and_parked_runs_without_writing() {
    for (state, node) in [("active", "build"), ("parked", "build"), ("parked", "gate")] {
        let (env, mut record) = fixture(state, (state == "parked").then_some("user"), vec![]);
        record.node = node.into();
        save(&env, &record);
        sealed_spec(&env, &record, NOW_SPEC);
        conditions::board(&env, "Done");
        let paths = env
            .state_path_for(&record.project_root)
            .workflow(&record.id);
        let before = (
            fs::read(&paths.record).unwrap(),
            fs::read(&paths.journal).unwrap(),
        );
        let expected = json!({"address":record.address().to_string(), "dir":paths.work,
            "record":record,"journal":[],"warnings":[]});
        let json_before = success(command(
            &env,
            &["workflow", "show", record.id.as_str(), "--json"],
        ));
        assert_eq!(
            json_before,
            format!("{}\n", serde_json::to_string_pretty(&expected).unwrap())
        );
        let text = human(&env, &record, false);
        let line = text
            .lines()
            .find(|line| line.trim_start().starts_with("now "));
        assert!(line.is_some(), "{state} at {node}: {text}");
        let line = line.unwrap();
        contains(line, &["gate", "hold", "team.stage=Done", "2m"]);
        assert!(
            !line.contains("read "),
            "a board condition has no forge probe: {line}"
        );
        assert_eq!(
            text.lines()
                .filter(|line| line.trim_start().starts_with("now "))
                .count(),
            1
        );
        assert_eq!(
            success(command(
                &env,
                &["workflow", "show", record.id.as_str(), "--json"]
            )),
            json_before
        );
        assert_eq!(
            (
                fs::read(&paths.record).unwrap(),
                fs::read(&paths.journal).unwrap()
            ),
            before
        );
    }
}

#[test]
fn show_now_treats_pre_entry_forge_probes_as_unread_and_shows_the_probe_time() {
    let (env, mut record) = fixture("active", None, vec![]);
    record.node = "build".into();
    save(&env, &record);
    sealed_spec(
        &env,
        &record,
        &NOW_SPEC.replace("team.stage=Done", "pr=merged"),
    );
    conditions::forge(
        &env,
        record.entered_at - jiff::SignedDuration::from_secs(1),
        true,
    );
    let text = human(&env, &record, false);
    let line = text
        .lines()
        .find(|line| line.trim_start().starts_with("now "))
        .unwrap_or("");
    contains(
        line,
        &[
            "gate",
            "unknown",
            "pr=unread",
            "probed before this check was entered",
            "read ",
        ],
    );
}

#[test]
fn show_now_skips_child_checks_and_plain_sleeps_but_reads_conditional_sleeps() {
    let (env, mut record) = fixture("active", None, vec![]);
    record.node = "build".into();
    save(&env, &record);
    conditions::board(&env, "Done");
    for runner in [
        "run='true'",
        "shell='true'",
        "agent='claude'\nsummary='Review the work'\nprompt-file='judge.md'",
        "sleep='365d'\nthen='@done'",
        "sleep='365d'\nthen='@done'\nwhen=['team.stage=Done']",
    ] {
        let text = format!(
            "[workflow]\nscope='worktree'\nstart='build'\n[steps.build]\nagent='claude'\nprompt-file='prompt.md'\nthen='gate'\n[checks.gate]\n{runner}\n"
        );
        sealed_spec(&env, &record, &text);
        let output = human(&env, &record, false);
        assert_eq!(
            output
                .lines()
                .any(|line| line.trim_start().starts_with("now ")),
            runner.contains("when="),
            "{output}"
        );
    }
}

fn fixture(state: &str, reason: Option<&str>, rows: Vec<Value>) -> (Env, RunRecord) {
    let env = setup();
    let mut record = seed(&env, &env.project_root, &env.project_root, state);
    record.created_at = AT.parse().unwrap();
    record.entered_at = record.created_at;
    record.terminal_at = record.terminal_at.map(|_| record.created_at);
    if let Some(park) = &mut record.park {
        park.at = record.created_at;
        park.reason = serde_json::from_value(json!(reason.unwrap())).unwrap();
    }
    save(&env, &record);
    journal(&env, &record, rows);
    (env, record)
}

fn journal(env: &Env, record: &RunRecord, rows: Vec<Value>) {
    let paths = env
        .state_path_for(&record.project_root)
        .workflow(&record.id);
    let rows = rows
        .into_iter()
        .map(|mut row| {
            row["at"] = json!(AT);
            if row.get("node").is_none() {
                row["node"] = json!("gate");
            }
            // Verify fixtures against the durable schema before exercising presentation.
            let _: rimz::store::workflow::JournalEntry =
                serde_json::from_value(row.clone()).unwrap();
            serde_json::to_string(&row).unwrap()
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(paths.journal, rows).unwrap();
}

fn evaluation(outcome: &str, object: Value) -> Value {
    json!({"event":"evaluation", "seq":1, "outcome":outcome, "object":object,
        "digests":{}, "started_at":AT, "ended_at":AT})
}

fn attempt(visit: u64, key: Value) -> Value {
    json!({"event":"attempt", "node":"build", "seq":visit+1, "visit":visit,
        "key":key, "outcome":"ended", "started_at":AT,
        "ended_at":"2000-01-01T12:01:00Z", "code":0, "digests":{}, "cost_usd":1.25})
}

fn human(env: &Env, record: &RunRecord, history: bool) -> String {
    let mut args = vec!["workflow", "show", record.id.as_str()];
    if history {
        args.push("--history");
    }
    success(command(env, &args))
}

fn contains(text: &str, fragments: &[&str]) {
    for fragment in fragments {
        assert!(text.contains(fragment), "missing {fragment:?}:\n{text}");
    }
}

fn commands(text: &str, options: &[&str]) {
    assert!(text.contains("you can\n"), "{text}");
    for option in options {
        let command = format!("rimz workflow {option}");
        assert!(text.contains(&command), "missing {command:?}:\n{text}");
    }
}

#[test]
fn copied_commands_select_the_shown_run_across_rooms_and_reused_addresses() {
    let env = setup();
    let other = env.home_root.join("other/project");
    env.record(&other);
    let mut here = seed(&env, &env.project_root, &env.project_root, "parked");
    here.park.as_mut().unwrap().at = AT.parse().unwrap();
    save(&env, &here);
    let there = seed(&env, &other, &other, "parked");
    let list = success(command(&env, &["workflow", "list"]));
    for (hint, record) in list
        .lines()
        .filter_map(|line| line.strip_prefix("  → "))
        .zip([&here, &there])
    {
        let selected: Value = serde_json::from_str(&success(copied(&env, hint, true))).unwrap();
        assert_eq!(selected["record"]["id"], json!(record.id), "{list}");
    }
    assert_eq!(list.matches("  → ").count(), 2, "{list}");
    for text in [
        human(&env, &there, false),
        success(command(
            &env,
            &[
                "--root",
                other.to_str().unwrap(),
                "workflow",
                "show",
                "review#project",
            ],
        )),
    ] {
        let hint = text
            .lines()
            .find_map(|line| line.strip_prefix("all of it: "))
            .unwrap();
        let selected: Value = serde_json::from_str(&success(copied(&env, hint, true))).unwrap();
        assert_eq!(selected["record"]["id"], json!(there.id), "{text}");
    }
    let text = human(&env, &there, false);
    for verb in ["resume", "cancel"] {
        let option = text
            .lines()
            .find(|line| line.contains(&format!("workflow {verb}")))
            .unwrap();
        let command = option.trim().split_once("  ").unwrap().0;
        success(copied(&env, command, false));
    }
    assert_eq!(
        json_show(&env, here.id.as_str())["record"]["state"],
        "parked"
    );
    assert_eq!(
        json_show(&env, there.id.as_str())["record"]["state"],
        "canceled"
    );

    let mut older = seed(&env, &env.project_root, &env.project_root, "done");
    older.terminal_at = Some(AT.parse().unwrap());
    save(&env, &older);
    let collision = seed(
        &env,
        &env.project_root,
        &env.home_root.join("linked/project"),
        "parked",
    );
    let list = success(command(&env, &["workflow", "list"]));
    let hint = list
        .lines()
        .find_map(|line| line.strip_prefix("  → "))
        .unwrap();
    assert!(hint.contains(here.id.as_str()), "{list}");
    let selected: Value = serde_json::from_str(&success(copied(&env, hint, true))).unwrap();
    assert_eq!(selected["record"]["id"], json!(here.id), "{list}");
    for record in [&older, &here, &collision] {
        let text = human(&env, record, false);
        let hint = text
            .lines()
            .find_map(|line| line.strip_prefix("all of it: "))
            .unwrap();
        let selected: Value = serde_json::from_str(&success(copied(&env, hint, true))).unwrap();
        assert_eq!(selected["record"]["id"], json!(record.id), "{text}");
    }
}

#[test]
fn history_hint_keeps_flags_before_a_hyphen_leading_target() {
    let (env, mut record) = fixture("parked", Some("user"), vec![]);
    record.workflow = "-review".into();
    save(&env, &record);
    let text = human(&env, &record, false);
    let hint = text
        .lines()
        .find_map(|line| line.strip_prefix("all of it: "))
        .unwrap();
    let history = success(copied(&env, hint, false));
    assert!(history.starts_with("-review#project"), "{history}");
}

#[test]
fn copied_park_options_accept_a_hyphen_leading_node() {
    let results = ["review", "-review"].map(|name| {
        let (env, mut record) = fixture("parked", Some("user"), vec![]);
        record.workflow = name.into();
        save(&env, &record);
        sealed_spec(&env, &record, &SPEC.replace("build", "-build"));
        let text = human(&env, &record, false);
        let hint = text
            .lines()
            .find(|line| line.ends_with("continue at -build"))
            .unwrap()
            .trim()
            .split_once("  ")
            .unwrap()
            .0;
        let output = copied(&env, hint, false);
        (
            output.status.code(),
            text,
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    });
    assert!(
        results.iter().all(|(code, ..)| *code == Some(0)),
        "{results:#?}"
    );
}

#[test]
fn cap_after_an_engine_route_fails_the_run_and_names_both_hops() {
    let (env, _) = super::super::fixture(
        &format!(
            "{HEADER}[checks.pr]\nrun='sh checks/pr.sh'\non.ok='@done'\non.invalid='build'\n\
            [steps.build]\nshell='true'\ntimeout='1h'\nthen='pr'\nmax-visits=1\n"
        ),
        "exit 7\n",
    );
    let id = start(&env);
    let value = settled(&env, &id, "failed");
    assert_eq!(value["record"]["input"]["outcome"], "cap", "{value:#}");
    let text = success(command(&env, &["workflow", "show", &id]));
    contains(&text, &["pr invalid → build", "build cap → @failed"]);
}

#[test]
fn timeout_shows_the_flushed_wait_and_shell_log() {
    let (env, record) = fixture(
        "parked",
        Some("timeout"),
        vec![
            evaluation("fail", json!({"outcome":"fail","log":"/tmp/check.log"})),
            evaluation(
                "invalid",
                json!({"outcome":"invalid","reason":"cannot open log"}),
            ),
            json!({"event":"route", "outcome":"invalid",
                "input":{"outcome":"invalid","reason":"cannot open log"}}),
            json!({"event":"wait", "outcome":"invalid", "first":AT,
            "last":"2000-01-01T13:00:00Z", "count":7, "recheck_at":null}),
        ],
    );
    let paths = env
        .state_path_for(&record.project_root)
        .workflow(&record.id);
    fs::write(
        paths.spec,
        SPEC.replacen("run='true'", "shell='true'", 1).replace(
            "on.ok='build'",
            "on.pass='@done'\non.fail='@wait'\non.invalid='@wait'",
        ),
    )
    .unwrap();
    let text = human(&env, &record, false);
    contains(
        &text,
        &[
            "waited the full 1h",
            "gate said invalid ×7 over 1h",
            "last log /tmp/check.log",
        ],
    );
    commands(
        &text,
        &["resume 'review#project'", "cancel 'review#project'"],
    );
}

#[test]
fn invalid_shows_the_durable_reason_and_stderr() {
    let mut row = evaluation(
        "invalid",
        json!({"outcome":"invalid","reason":"checks/broken.sh exited 7"}),
    );
    row["stderr_tail"] = json!("cannot read input\nmissing token");
    let (env, mut record) = fixture("parked", Some("invalid"), vec![row]);
    record.park.as_mut().unwrap().text = Some("checks/broken.sh exited 7".into());
    save(&env, &record);
    let text = human(&env, &record, false);
    contains(
        &text,
        &[
            "checks/broken.sh exited 7",
            "  cannot read input\n  missing token",
        ],
    );
    commands(
        &text,
        &["resume 'review#project'", "cancel 'review#project'"],
    );
}

#[test]
fn user_check_offers_each_named_target_but_step_does_not() {
    let (env, mut record) = fixture("parked", Some("user"), vec![]);
    record.park.as_mut().unwrap().text = Some("Choose the next move".into());
    save(&env, &record);
    let text = human(&env, &record, false);
    contains(&text, &["gate says: Choose the next move"]);
    commands(
        &text,
        &[
            "resume 'review#project'",
            "resume 'review#project' --at build",
            "cancel 'review#project'",
        ],
    );
    record.node = "build".into();
    save(&env, &record);
    let text = human(&env, &record, false);
    commands(
        &text,
        &["resume 'review#project'", "cancel 'review#project'"],
    );
    assert!(!text.contains("--at"), "{text}");
}

#[test]
fn asked_names_the_closed_pane_and_sign_to_write_before_resuming() {
    let mut row = attempt(1, Value::Null);
    row["outcome"] = json!("asked");
    row["handle"] = json!("@fixer");
    row["asked"] = json!({"handle":"@fixer", "kind":"permission", "text":"May I push?"});
    let (env, mut record) = fixture("parked", Some("asked"), vec![row]);
    record.node = "build".into();
    save(&env, &record);
    let text = human(&env, &record, false);
    contains(
        &text,
        &[
            "@fixer (permission)",
            "May I push?",
            "turn was stopped",
            "pane closed",
            "/signs/build.md",
            "write the answer",
        ],
    );
    commands(
        &text,
        &["resume 'review#project'", "cancel 'review#project'"],
    );
}

#[test]
fn deadline_offers_plain_resume_as_well_as_rearming() {
    let (env, mut record) = fixture("parked", Some("deadline"), vec![]);
    record.deadline = Some(AT.parse().unwrap());
    record.deadline_fired = true;
    save(&env, &record);
    let text = human(&env, &record, false);
    contains(&text, &[&historical_clock(AT), "no deadline; it has fired"]);
    commands(
        &text,
        &[
            "resume 'review#project'",
            "resume 'review#project' --extend 'deadline=<when>'",
            "cancel 'review#project'",
        ],
    );
}

#[test]
fn check_failure_offers_a_cleared_retry() {
    let (env, mut record) = fixture("parked", Some("check"), vec![]);
    record.park.as_mut().unwrap().text = Some("judge exited 7".into());
    save(&env, &record);
    let text = human(&env, &record, false);
    contains(
        &text,
        &[
            "failed to evaluate twice",
            "judge exited 7",
            "retry cleared",
        ],
    );
    commands(
        &text,
        &["resume 'review#project'", "cancel 'review#project'"],
    );
}

#[test]
fn history_renders_typed_facts_without_json_and_default_trail_is_bounded() {
    let mut row = evaluation(
        "ok",
        json!({"outcome":"ok", "readings":{"ci":"passed", "pr":null},
            "reason":"all green", "log":"/tmp/check.log"}),
    );
    row["cost_usd"] = json!(0.25);
    let mut step = attempt(1, Value::Null);
    step["head_start"] = json!("123456789abcdef");
    step["head_end"] = json!("abcdef123456789");
    step["log"] = json!("/tmp/step.log");
    let mut rows = vec![row, step];
    rows.extend((0..10).map(|_| json!({"event":"resume", "visit_grant":2,"deadline":AT})));
    rows.extend([
        json!({"event":"route", "outcome":"deadline", "input":{"deadline":AT}}),
        json!({"event":"turn_end", "action":"kept",
            "turn":{"agent_id":"session","started_at":AT,"ended_at":AT},
            "check":"gate", "outcome":"ok", "target":"build"}),
        json!({"event":"wait", "outcome":"hold", "first":AT,"last":AT,"count":3,"recheck_at":null}),
        json!({"event":"park", "reason":"user", "text":"choose"}),
        json!({"event":"wake", "signal":"ci.done"}),
        json!({"event":"cancel"}),
        json!({"event":"terminal", "state":"canceled"}),
    ]);
    let (env, mut record) = fixture("canceled", None, rows);
    record.task = Some("merge".into());
    record.manual = true;
    save(&env, &record);
    let text = human(&env, &record, true);
    contains(
        &text,
        &[
            "review#project",
            "said ok → build",
            "ci=passed",
            "pr=unread",
            "all green",
            "log: /tmp/check.log",
            "$0.25",
            "visit 1",
            "exit 0 HEAD 12345678 → abcdef12",
            "log: /tmp/step.log",
            "1m",
            "resume",
            "visit grant 2",
            "deadline",
            "turn_end kept",
            "gate ok → build",
            "wait hold ×3",
            "park user",
            "choose",
            "wake ci.done",
            "cancel canceled",
            "terminal canceled",
        ],
    );
    assert!(
        !text.contains('{') && !text.contains("\"outcome\"") && !text.contains(AT),
        "{text}"
    );
    let text = human(&env, &record, false);
    contains(
        &text,
        &[
            "by loop task merge, fired by hand",
            "1 attempt, $1.50",
            "+8 earlier ·",
            "resumed → gate",
            "canceled",
            "work dir ~/",
            "all of it: rimz workflow show 'review#project' --history",
        ],
    );
    assert!(!text.contains(" · now"), "{text}");
    assert_eq!(
        json_show(&env, record.id.as_str())["address"],
        "review#project"
    );
}

#[test]
fn history_requires_one_run_and_conflicts_with_json() {
    let (env, record) = fixture("parked", Some("user"), vec![]);
    for args in [
        vec!["workflow", "show", "review", "--history"],
        vec![
            "workflow",
            "show",
            record.id.as_str(),
            "--history",
            "--json",
        ],
    ] {
        let output = command(&env, &args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    for flag in [None, Some("--history"), Some("--json")] {
        let mut args = vec!["workflow", "show"];
        args.extend(flag);
        let output = command(&env, &args);
        let error = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(1), "{error}");
        assert_eq!(
            error.trim(),
            "error: caller is not part of a workflow run; use rimz workflow show <run> or rimz workflow list"
        );
    }
}

#[test]
fn bare_show_follows_the_calling_leader_and_two_levels_of_subagents() {
    use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
    let (env, mut record) = fixture("parked", Some("user"), vec![]);
    record.notes = Some("20261107/120000Z-workflow-review-project".into());
    save(&env, &record);
    let store = env.store();
    let workspace = env.resolve_workspace(&env.project_root);
    for (id, parent, depth) in [
        ("leader", None, 0),
        ("child", Some("leader"), 1),
        ("grandchild", Some("child"), 2),
    ] {
        let mut observation =
            AgentLifecycleObservation::new(Some(id.into()), LifecycleSignal::Registered);
        observation.agent_name = Some(id.into());
        observation.pane_id = Some(format!("tmux:{}", depth + 1).parse().unwrap());
        observation.launch.parent_agent_id = parent.map(Into::into);
        observation.launch.launch_depth = Some(depth);
        if parent.is_none() {
            observation.launch.workflow = Some(
                serde_json::from_value(json!({
                    "run":record.id,"seq":1,"role":"step"
                }))
                .unwrap(),
            );
        }
        store
            .append_event(&rimz::EventEnvelope::agent_lifecycle(
                workspace.workspace_id.clone(),
                workspace.session_name.clone(),
                "claude",
                "test",
                &observation,
            ))
            .unwrap();
    }
    let notes = store.paths().notes_run(
        rimz::disk::paths::Class::AutomationNotes,
        record.notes.as_ref().unwrap(),
    );
    let roster: Vec<_> = store
        .snapshot()
        .unwrap()
        .agents
        .iter()
        .map(|agent| {
            (
                agent.agent_id.clone(),
                agent.kind.clone(),
                agent.workflow.clone(),
                agent.parent_agent_id.clone(),
                agent.launch_depth,
            )
        })
        .collect();
    assert_eq!(roster.len(), 3, "{roster:?}");
    for id in ["leader", "grandchild"] {
        for flag in [None, Some("--history"), Some("--json")] {
            let mut args = vec!["workflow", "show"];
            args.extend(flag);
            let output = env
                .rimz()
                .args(args)
                .env("RIMZ_AGENT_KIND", "claude")
                .env("RIMZ_AGENT_ID", id)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "caller={id} flag={flag:?}: {}\n{roster:?}",
                String::from_utf8_lossy(&output.stderr)
            );
            let text = success(output);
            if flag == Some("--json") {
                let value: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(value["record"]["id"], json!(record.id));
                assert_eq!(value["notes"], json!(notes));
            } else {
                assert!(
                    text.starts_with(&format!("{}\n", record.address())),
                    "{text}"
                );
                if flag.is_none() {
                    assert!(
                        text.contains(&format!("notes {}\n", notes.display())),
                        "{text}"
                    );
                }
            }
        }
    }
}

#[test]
fn waiting_check_shows_readings_timeout_target_and_folded_trail() {
    let row = evaluation(
        "pending",
        json!({"outcome":"pending", "readings":{"ci":"pending","pr":null}}),
    );
    let (env, mut record) = fixture("active", None, vec![row.clone(), row]);
    record.wait = serde_json::from_value(json!({"node":"gate", "outcome":"pending",
        "first":AT,"last":AT,"count":2,"recheck_at":"2000-01-01T12:01:00Z"}))
    .unwrap();
    save(&env, &record);
    let paths = env
        .state_path_for(&record.project_root)
        .workflow(&record.id);
    fs::write(
        &paths.spec,
        SPEC.replace("on.ok='build'", "on={ok='build',pending='@wait'}"),
    )
    .unwrap();
    let text = human(&env, &record, false);
    contains(
        &text,
        &[
            "☾ waiting at gate",
            "gives up",
            "then @failed",
            "  ci=pending  pr=unread · read ",
            ": pending",
            "gate pending ×2 → @wait",
            " · now",
        ],
    );
    assert!(!text.contains("history:"), "{text}");
    assert_eq!(text.lines().next(), Some("review#project"), "{text}");
    assert!(
        text.contains(&format!("last checked {}", historical_clock(AT)))
            && !text.contains("checked ×"),
        "{text}"
    );
    assert!(
        text.find(" · read ").unwrap() < text.find("started ").unwrap(),
        "{text}"
    );
    assert!(!text.contains("now gate"), "{text}");
    assert_eq!(
        text.lines()
            .filter(|line| line.contains("waiting at gate") && line.contains(": pending"))
            .count(),
        1,
        "{text}"
    );

    record.deadline = Some("2000-01-01T12:05:00Z".parse().unwrap());
    save(&env, &record);
    fs::write(
        &paths.spec,
        SPEC.replace("start='gate'", "start='gate'\non.deadline='@failed'")
            .replace("on.ok='build'", "on={ok='build',pending='@wait'}"),
    )
    .unwrap();
    let text = human(&env, &record, false);
    contains(
        &text,
        &[&format!(
            "gives up {}, then @failed",
            historical_clock("2000-01-01T12:05:00Z")
        )],
    );
}

#[test]
fn running_step_states_its_activity_once() {
    let (env, mut record) = fixture("active", None, vec![]);
    record.node = "build".into();
    save(&env, &record);
    let text = human(&env, &record, false);
    assert_eq!(text.matches("build running").count(), 1, "{text}");
}

#[test]
fn terminal_runs_do_not_inherit_the_checkouts_current_pr() {
    let (env, mut record) = fixture("done", None, vec![]);
    let state = env.state_path_for(&record.project_root);
    let runtime = rimz::RuntimePaths::for_state_under(&state, &env.runtime_root);
    let path = runtime.lane_path("pr-state.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut states = serde_json::Map::new();
    states.insert(
        record.scope.path.to_string_lossy().into_owned(),
        json!({"state":"open","number":200}),
    );
    fs::write(path, serde_json::to_vec(&json!({"states":states})).unwrap()).unwrap();
    let mut rendered = Vec::new();
    for state in [RunState::Done, RunState::Failed, RunState::Canceled] {
        record.state = state;
        save(&env, &record);
        for history in [false, true] {
            rendered.push(human(&env, &record, history));
        }
    }
    assert!(
        rendered.iter().all(|text| !text.contains("PR #")),
        "{rendered:#?}"
    );
}

#[test]
fn with_you_header_names_the_room_pr_and_check_without_repeating_status() {
    let (env, mut record) = fixture("active", None, vec![]);
    let state = env.state_path_for(&record.project_root);
    let runtime = rimz::RuntimePaths::for_state_under(&state, &env.runtime_root);
    let path = runtime.lane_path("pr-state.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut states = serde_json::Map::new();
    states.insert(
        record.scope.path.to_string_lossy().into_owned(),
        json!({"state":"open","number":1517}),
    );
    fs::write(path, serde_json::to_vec(&json!({"states":states})).unwrap()).unwrap();
    let actual = [false, true].map(|held| {
        record.node = if held { "gate" } else { "build" }.into();
        let attempt: rimz::store::workflow::LiveAttempt = serde_json::from_value(json!({
            "kind":"agent", "lifecycle":"until", "seq":7, "node":"build",
            "started_at":AT, "host":{"pid":1,"start":null},
            "run":{"run_id":rimz::ids::RunId::new(),"handle":"@pet"},
        }))
        .unwrap();
        record.attempt = (!held).then_some(attempt.clone());
        record.held = held
            .then(|| serde_json::from_value(json!({"attempt":attempt,"entered_at":AT})).unwrap());
        record.wait = Some(
            serde_json::from_value(json!({"node":"gate","outcome":"pending",
            "first":AT,"last":AT,"count":2,"recheck_at":null}))
            .unwrap(),
        );
        save(&env, &record);
        let text = human(&env, &record, false);
        let history = human(&env, &record, true);
        let suffix = if held {
            "checking gate: pending"
        } else {
            "gate: pending"
        };
        let header = text.lines().take(2).collect::<Vec<_>>().join("\n");
        (
            header,
            text.matches(suffix).count(),
            text.matches("build running").count(),
            history.matches("pending").count(),
        )
    });
    assert_eq!(
        actual,
        [
            (
                "review#project  PR #1517\n⢿ build is with you · @pet · gate: pending".into(),
                1,
                0,
                1
            ),
            (
                "review#project  PR #1517\n⢿ build is with you · @pet · checking gate: pending"
                    .into(),
                1,
                0,
                1
            ),
        ]
    );
    let json = json_show(&env, record.id.as_str());
    assert_eq!(json.as_object().unwrap().len(), 5, "{json:#}");
    assert!(
        json.get("pr").is_none() && json.get("agent").is_none(),
        "{json:#}"
    );
}

#[test]
fn holding_check_names_the_case_target_and_completion_clock() {
    let (env, mut record) = fixture("active", None, vec![]);
    record.wait = serde_json::from_value(json!({"node":"gate", "outcome":"hold", "case":0,
        "detail":"ready", "first":AT,"last":AT,"count":3,"recheck_at":null}))
    .unwrap();
    save(&env, &record);
    let paths = env
        .state_path_for(&record.project_root)
        .workflow(&record.id);
    fs::write(
        &paths.spec,
        concat!(
            "[workflow]\nscope='worktree'\nstart='gate'\n[checks.gate]\nelse='pending'\n",
            "on={ready='build',pending='@wait',unknown='@wait'}\n[[checks.gate.cases]]\nwhen=['ci=passed']\n",
            "outcome='ready'\nfor='10m'\n[steps.build]\nshell='true'\nthen='gate'\n",
        ),
    )
    .unwrap();
    let text = human(&env, &record, false);
    contains(
        &text,
        &[
            "☾ holding ready at gate",
            "of 10m, then build at",
            &historical_clock("2000-01-01T12:10:00Z"),
        ],
    );
    assert_eq!(text.matches("holding ready at gate").count(), 1, "{text}");
    let text = fs::read_to_string(&paths.spec).unwrap();
    fs::write(
        &paths.spec,
        text.replace("[checks.gate]", "[checks.gate]\ntimeout='5m'")
            .replace(
                "on={ready='build',pending='@wait',unknown='@wait'}",
                "on={ready='build',pending='@wait',timeout='@failed'}",
            ),
    )
    .unwrap();
    let text = human(&env, &record, false);
    contains(
        &text,
        &[&format!(
            "of 10m, then @failed at {}",
            historical_clock("2000-01-01T12:05:00Z")
        )],
    );
}

#[test]
fn terminal_headline_uses_terminal_time_and_ignores_a_stale_park() {
    let (env, mut record) = fixture("parked", Some("user"), vec![]);
    record.entered_at = jiff::Timestamp::now();
    for (state, glyph, word) in [
        (RunState::Done, "✓", "done"),
        (RunState::Failed, "!", "failed"),
        (RunState::Canceled, "○", "canceled"),
    ] {
        record.state = state;
        record.terminal_at = Some(jiff::Timestamp::now() - jiff::SignedDuration::from_mins(3));
        save(&env, &record);
        let text = human(&env, &record, false);
        assert!(
            text.starts_with(&format!("review#project\n{glyph} {word} 3m\n")),
            "{text}"
        );
        assert!(
            !text.contains("you can") && !text.contains("parked at"),
            "{text}"
        );
    }
}

const CONDITION_RAIL: &str = r#"
[workflow]
scope = 'worktree'
start = 'gate'
[checks.gate]
else = 'pending'
on = { merged = '@done', red = 'build', ready = 'build', pending = '@wait', queued = '@wait', unknown = '@wait' }
[[checks.gate.cases]]
when = ['pr=merged']
outcome = 'merged'
[[checks.gate.cases]]
when = ['ci=failed']
outcome = 'red'
[[checks.gate.cases]]
when = ['ci=passed']
outcome = 'ready'
for = '10m'
[[checks.gate.cases]]
when = ['ci=pending']
outcome = 'pending'
[[checks.gate.cases]]
when = ['pr.queue=queued']
outcome = 'queued'
[steps.build]
shell = 'true'
then = 'gate'
max-visits = 2
"#;

fn sealed_spec(env: &Env, record: &RunRecord, spec: &str) {
    let paths = env
        .state_path_for(&record.project_root)
        .workflow(&record.id);
    fs::write(paths.spec, spec).unwrap();
}

#[test]
fn active_rail_uses_the_sealed_graph_and_last_attempt_with_reentry_counts() {
    let mut step = attempt(2, Value::Null);
    step["handle"] = json!("@builder");
    step["head_start"] = json!("123456789abcdef");
    step["head_end"] = json!("abcdef123456789");
    let (env, mut record) = fixture(
        "active",
        None,
        vec![
            evaluation("ok", json!({"outcome":"ok"})),
            attempt(1, Value::Null),
            evaluation("ok", json!({"outcome":"ok"})),
            step,
        ],
    );
    record.visits = serde_json::from_value(json!({"build":2})).unwrap();
    save(&env, &record);
    sealed_spec(
        &env,
        &record,
        &SPEC.replace("shell='true'", "agent='codex'\nprompt-file='prompt.md'"),
    );
    let dir = env.rimz_home().join("workflows/review");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("workflow.toml"),
        SPEC.replace("build", "replacement"),
    )
    .unwrap();
    let text = human(&env, &record, false);
    contains(
        &text,
        &[
            "● build",
            "ended",
            "1m",
            "$1.25",
            "HEAD 12345678 → abcdef12",
            "×2",
            "visit 2 of 2",
        ],
    );
    assert!(!text.contains("replacement"), "{text}");
    assert!(!text.contains("@builder"), "{text}");
    let rail = text
        .split_once("● build")
        .unwrap()
        .1
        .split_once("trail")
        .unwrap()
        .0;
    assert!(rail.contains(&historical_clock(AT)), "{text}");
    assert!(
        text.find("● build").unwrap() < text.find("visits  ").unwrap(),
        "{text}"
    );
    let paths = env
        .state_path_for(&record.project_root)
        .workflow(&record.id);
    let rows = fs::read_to_string(&paths.journal).unwrap();
    fs::write(
        &paths.journal,
        rows.replace("abcdef123456789", "123456789abcdef"),
    )
    .unwrap();
    let text = human(&env, &record, false);
    assert!(text.contains("HEAD 12345678, no new commits"), "{text}");
    assert!(human(&env, &record, true).contains("@builder"));
}

#[test]
fn a_past_steps_visits_survive_a_key_the_run_moved_on_to() {
    let (env, _) = super::super::fixture(
        &format!(
            "{HEADER}[checks.pr]\nrun='sh checks/pr.sh'\non.ok='build'\n\
             [steps.build]\nshell='true'\ntimeout='1h'\nthen='next'\nmax-visits=2\n\
             [checks.next]\nrun='sh checks/pr.sh'\non.other='finish'\n\
             [steps.finish]\nshell='true'\ntimeout='1h'\nthen='waiting'\n\
             [checks.waiting]\nrun='sh checks/pr.sh'\non.pending='@wait'\ntimeout='1h'\n"
        ),
        "n=0; count=\"$RIMZ_WORKFLOW_DIR/count\"\n\
         if [ -f \"$count\" ]; then read -r n < \"$count\"; fi\n\
         printf '%s\\n' \"$((n+1))\" > \"$count\"\n\
         case $n in\n\
           0) printf '{\"outcome\":\"ok\",\"key\":\"lint\"}';;\n\
           1) printf '{\"outcome\":\"other\",\"key\":\"other\"}';;\n\
           *) printf '{\"outcome\":\"pending\",\"recheck_at\":\"2099-01-01T00:00:00Z\"}';;\n\
         esac\n",
    );
    let id = start(&env);
    let value = until(&env, &id, |value| {
        value["record"]["wait"]["outcome"] == "pending"
    });
    assert_eq!(value["record"]["key"], "other", "{value:#}");
    assert_eq!(value["record"]["visits"]["build"], 1, "{value:#}");
    let text = success(command(&env, &["workflow", "show", &id]));
    contains(&text, &["visits  build 1 of 2"]);
}

#[test]
fn folded_self_routes_count_every_check_entry() {
    let (env, _) = super::super::fixture(
        &format!(
            "{HEADER}[checks.pr]\nrun='sh checks/pr.sh'\n\
             on={{pass='pr',fail='@wait'}}\ntimeout='1h'\n"
        ),
        "n=0; count=\"$RIMZ_WORKFLOW_DIR/count\"\n\
         if [ -f \"$count\" ]; then read -r n < \"$count\"; fi\n\
         printf '%s\\n' \"$((n+1))\" > \"$count\"\n\
         if [ \"$n\" -lt 3 ]; then printf '{\"outcome\":\"pass\"}';\n\
         else printf '{\"outcome\":\"fail\",\"recheck_at\":\"2099-01-01T00:00:00Z\"}'; fi\n",
    );
    let receipt = success(
        env.rimz()
            .env("RIMZ_BIN", "/bin/true")
            .args(["workflow", "run", "review"])
            .output()
            .unwrap(),
    );
    let id = receipt
        .split_whitespace()
        .find(|word| word.starts_with("wf_"))
        .unwrap();
    for _ in 0..4 {
        success(command(&env, &["workflow", "advance", id]));
    }
    let text = success(command(&env, &["workflow", "show", id]));
    let rail = text
        .lines()
        .find(|line| line.starts_with("☾ pr  "))
        .unwrap();
    assert!(rail.contains("×4"), "{text}");
    assert!(text.contains("pr pass ×3 → pr"), "{text}");
}

#[test]
fn a_hold_forecasts_its_clock_only_when_it_beats_other_bounds() {
    let (env, mut record) = fixture("active", None, vec![]);
    record.wait = serde_json::from_value(json!({"node":"gate","outcome":"hold","case":2,
        "first":AT,"last":AT,"count":3,"recheck_at":null}))
    .unwrap();
    let results = ["10m", "30m", "40m"].map(|timeout| {
        save(&env, &record);
        sealed_spec(
            &env,
            &record,
            &CONDITION_RAIL
                .replace(
                    "else = 'pending'",
                    &format!("timeout = '{timeout}'\nelse = 'pending'"),
                )
                .replace("for = '10m'", "for = '30m'"),
        );
        let text = human(&env, &record, false);
        let case = text.lines().find(|line| line.contains("▸ 3")).unwrap();
        assert!(case.contains("holding 30m of 30m"), "{text}");
        assert!(
            text.contains(if timeout == "40m" {
                "then build at"
            } else {
                "then @failed at"
            }),
            "{text}"
        );
        case.contains(" at ")
    });
    assert_eq!(results, [false, false, true]);
}

#[test]
fn current_rail_glyph_matches_the_headline_in_every_live_state() {
    for mode in ["waiting", "hold", "running", "checking", "asking", "parked"] {
        let (env, mut record) = fixture(
            if mode == "parked" { "parked" } else { "active" },
            (mode == "parked").then_some("user"),
            vec![],
        );
        if matches!(mode, "waiting" | "hold") {
            record.wait = serde_json::from_value(json!({"node":"gate",
                "outcome": if mode == "hold" { "hold" } else { "pending" },
                "case":(mode == "hold").then_some(2),
                "first":AT,"last":AT,"count":1,"recheck_at":null}))
            .unwrap();
        }
        if matches!(mode, "running" | "asking") {
            record.node = "build".into();
            record.attempt = Some(
                serde_json::from_value(json!({
                    "kind":"agent", "seq":1,"node":"build","started_at":AT,
                    "host":{"pid":1,"start":null},
                    "asking":(mode == "asking").then(|| json!({"since":AT,
                        "handle":"@builder","kind":"permission","text":"May I push?"}))
                }))
                .unwrap(),
            );
        }
        save(&env, &record);
        sealed_spec(&env, &record, CONDITION_RAIL);
        let text = human(&env, &record, false);
        let glyph = text
            .lines()
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        assert!(
            text.lines()
                .skip(1)
                .any(|line| line.contains(&format!("{glyph} {}", record.node))),
            "{mode}: no matching current node in rail:\n{text}"
        );
    }
}

#[test]
fn condition_rail_marks_the_winner_and_summarizes_all_readings_once_without_color() {
    let (env, record) = fixture(
        "active",
        None,
        vec![evaluation(
            "pending",
            json!({
                "outcome":"pending", "case":3,
                "readings":{"pr":"open","ci":"pending","pr.queue":"none"}
            }),
        )],
    );
    sealed_spec(&env, &record, CONDITION_RAIL);
    let text = success(
        env.rimz()
            .env("NO_COLOR", "1")
            .args(["workflow", "show", record.id.as_str()])
            .output()
            .unwrap(),
    );
    contains(
        &text,
        &["│ ▸ 4", "pending @wait", "wakes on", "pr.*", "ci.*"],
    );
    let readings = text.lines().find(|line| line.contains(" · read "));
    assert!(readings.is_some(), "{text}");
    let readings = readings.unwrap();
    assert_eq!(readings.matches("ci=pending").count(), 1, "{text}");
    assert_eq!(readings.matches("pr=open").count(), 1, "{text}");
    assert_eq!(readings.matches("pr.queue=none").count(), 1, "{text}");
    let now = text.lines().find(|line| line.starts_with("now gate"));
    assert!(now.is_some(), "{text}");
    let now = now.unwrap();
    assert!(now.contains("unknown"), "{text}");
    assert_eq!(now.matches("ci=unread").count(), 1, "{text}");
    assert_eq!(now.matches("pr=unread").count(), 1, "{text}");
    assert_eq!(now.matches("pr.queue=unread").count(), 1, "{text}");
    assert!(
        !text.contains("ci: pending") && !text.contains("pr: open") && !text.contains('\u{1b}'),
        "{text}"
    );
    assert!(
        text.find(" · read ").unwrap() < text.find("now gate").unwrap(),
        "{text}"
    );
    assert!(
        text.find("now gate").unwrap() < text.find("3  ci=passed").unwrap(),
        "{text}"
    );
}

#[test]
fn condition_rail_marks_unread_instead_of_a_later_winner() {
    let (env, record) = fixture(
        "active",
        None,
        vec![evaluation(
            "unknown",
            json!({
                "outcome":"unknown", "readings":{"pr":"open","ci":null},
                "reason":"ci (local HEAD is not on the PR (old), retry later)"
            }),
        )],
    );
    sealed_spec(&env, &record, CONDITION_RAIL);
    let text = human(&env, &record, false);
    contains(
        &text,
        &[
            "? 2",
            "ci unread: local HEAD is not on the PR (old), retry later",
        ],
    );
    assert!(!text.contains("▸"), "{text}");
    assert_eq!(text.matches("ci unread:").count(), 1, "{text}");
}

#[test]
fn condition_rail_paints_a_held_case_even_without_a_new_evaluation() {
    let (env, mut record) = fixture("active", None, vec![]);
    record.wait = serde_json::from_value(json!({"node":"gate","outcome":"hold","case":2,
        "first":AT,"last":AT,"count":3,"recheck_at":null}))
    .unwrap();
    save(&env, &record);
    sealed_spec(&env, &record, CONDITION_RAIL);
    let text = human(&env, &record, false);
    contains(
        &text,
        &[
            "▸ 3",
            "holding 10m of 10m",
            &format!("→ build at {}", historical_clock("2000-01-01T12:10:00Z")),
        ],
    );
}

#[test]
fn parked_and_terminal_rails_are_horizontal_and_only_parked_has_a_current_node() {
    let (env, mut record) = fixture(
        "parked",
        Some("cap"),
        vec![
            evaluation("ok", json!({"outcome":"ok"})),
            attempt(1, Value::Null),
        ],
    );
    record.node = "build".into();
    record.visits = serde_json::from_value(json!({"build":1})).unwrap();
    save(&env, &record);
    let parked = human(&env, &record, false);
    contains(&parked, &["◆ gate\n╰─ ⊘ build ↑"]);
    assert!(
        parked.find("you can\n").unwrap() < parked.find("◆ gate").unwrap(),
        "{parked}"
    );
    assert!(!parked.contains("├─"), "{parked}");
    for state in [RunState::Done, RunState::Failed, RunState::Canceled] {
        record.state = state;
        record.terminal_at = Some(record.entered_at);
        save(&env, &record);
        let text = human(&env, &record, false);
        contains(&text, &["◆ gate\n╰─ ● build ↑"]);
        assert!(
            !text.contains("you can\n") && !text.contains("├─"),
            "{text}"
        );
    }
}
