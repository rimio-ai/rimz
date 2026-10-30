use super::*;

const AT: &str = "2000-01-01T12:00:00Z";

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
fn cap_evidence_names_the_engine_route_that_entered_the_step() {
    let (env, _) = super::super::fixture(
        &format!(
            "{HEADER}[checks.pr]\nrun='sh checks/pr.sh'\non.ok='@done'\non.invalid='build'\n\
            [steps.build]\nshell='true'\ntimeout='1h'\nthen='pr'\nmax-visits=1\n"
        ),
        "exit 7\n",
    );
    let id = start(&env);
    let value = settled(&env, &id, "parked");
    assert_eq!(value["record"]["park"]["reason"], "cap", "{value:#}");
    let text = success(command(&env, &["workflow", "show", &id]));
    contains(
        &text,
        &["pr invalid · build 1", "resume 'review#project' --at pr"],
    );
}

#[test]
fn cap_explains_each_visit_and_the_refused_visit_with_copyable_options() {
    let (env, mut record) = fixture(
        "parked",
        Some("cap"),
        vec![
            evaluation("ok", json!({"outcome":"ok"})),
            attempt(1, json!("lint")),
            json!({"event":"resume", "node":"build", "visit_grant":0, "deadline":null}),
            attempt(2, json!("lint")),
            evaluation("ok", json!({"outcome":"ok"})),
            json!({"event":"route", "node":"build", "outcome":"cap",
            "input":{"bound":"max-visits", "visits":2, "max":2}}),
        ],
    );
    record.node = "build".into();
    record.visits = serde_json::from_value(json!({"build":{"total":2,"keys":[]}})).unwrap();
    save(&env, &record);
    let text = human(&env, &record, false);
    contains(
        &text,
        &[
            "max-visits",
            "2 of 2",
            "gate ok · build 1",
            "resumed · build 2",
            "build 3 refused",
            "exit 0",
            "ended",
            "1m",
            "$1.25",
        ],
    );
    commands(
        &text,
        &[
            "resume 'review#project' --extend 'visits=+1'",
            "resume 'review#project' --at gate",
            "cancel 'review#project'",
        ],
    );
}

#[test]
fn per_key_cap_filters_visits_and_uses_the_granted_limit() {
    let (env, _) = super::super::fixture(
        &format!(
            "{HEADER}[checks.pr]\nrun='sh checks/pr.sh'\non.ok='build'\n\
            [steps.build]\nshell='true'\ntimeout='1h'\nthen='pr'\nmax-visits=10\nmax-visits-per-key=1\n"
        ),
        "n=0; count=\"$RIMZ_WORKFLOW_DIR/count\"\n\
         if [ -f \"$count\" ]; then read -r n < \"$count\"; fi\n\
         printf '%s\\n' \"$((n+1))\" > \"$count\"\n\
         if [ \"$n\" = 0 ]; then key=other; else key=lint; fi\n\
         printf '{\"outcome\":\"ok\",\"key\":\"%s\"}\\n' \"$key\"\n",
    );
    let receipt = success(
        env.rimz()
            .env("RIMZ_BIN", "/bin/true")
            .args(["workflow", "run", "review"])
            .output()
            .unwrap(),
    );
    let id = receipt.lines().next().unwrap();
    success(
        env.rimz()
            .env("RIMZ_BIN", "/bin/true")
            .args(["workflow", "resume", id, "--extend", "visits=+1"])
            .output()
            .unwrap(),
    );
    success(command(&env, &["workflow", "advance", id]));
    let value = settled(&env, id, "parked");
    assert_eq!(value["record"]["visits"]["build"]["total"], 3);
    let text = success(command(&env, &["workflow", "show", id]));
    contains(
        &text,
        &[
            "max-visits-per-key",
            "key \"lint\"",
            "2 of 2",
            "build 1",
            "build 2",
            "build 3 refused",
        ],
    );
    let why = text.split_once("you can\n").unwrap().0;
    assert_eq!(
        why.matches(" · build 1 · ").count(),
        1,
        "only lint's visit 1: {text}"
    );
    assert!(text.contains("build 2 of 2 for key \"lint\""), "{text}");
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
        SPEC.replace(
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
            "invalid ×7",
            &historical_clock(AT),
            &historical_clock("2000-01-01T13:00:00Z"),
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
            "evaluation ok → build",
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
            "then @user",
            "ci: pending",
            "pr: unread",
            "said pending, ×2",
            "gate pending ×2 → @wait",
            " · now",
        ],
    );
    assert!(!text.contains("history:"), "{text}");

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
            "on={ready='build',pending='@wait'}\n[[checks.gate.cases]]\nwhen=['ci=passed']\n",
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
    let text = fs::read_to_string(&paths.spec).unwrap();
    fs::write(
        &paths.spec,
        text.replace("[checks.gate]", "[checks.gate]\ntimeout='5m'")
            .replace(
                "on={ready='build',pending='@wait'}",
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
            text.starts_with(&format!("review#project  {glyph} {word} 3m\n")),
            "{text}"
        );
        assert!(
            !text.contains("you can") && !text.contains("parked at"),
            "{text}"
        );
    }
}
