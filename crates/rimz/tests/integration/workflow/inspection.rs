use super::*;
use rimz::store::workflow::{RunRecord, RunState};

mod definition;
mod show;

const SPEC: &str = "[workflow]\nscope='worktree'\nstart='gate'\n[checks.gate]\nshell='true'\non.ok='build'\ntimeout='1h'\n[steps.build]\nshell='true'\nthen='gate'\nmax-visits=2\n";

fn seed(env: &Env, root: &Path, checkout: &Path, state: &str) -> RunRecord {
    let id = rimz::ids::WorkflowRunId::new();
    let now = jiff::Timestamp::now();
    let record = serde_json::from_value(json!({
        "version":1, "id":id, "workflow":"review", "spec_dir":"/spec",
        "spec_digest":"digest", "project_root":root,
        "scope":{"kind":"worktree", "path":checkout},
        "created_at":now, "entered_at":now, "state":state, "node":"gate",
        "park":(state == "parked").then(|| json!({"reason":"user", "at":now})),
        "terminal_at":(state != "active" && state != "parked").then_some(now),
        "visits":{}, "visit_grant":0, "deadline_fired":false, "next_seq":1,
    }))
    .unwrap();
    save(env, &record);
    record
}

fn save(env: &Env, record: &RunRecord) {
    let paths = env
        .state_path_for(&record.project_root)
        .workflow(&record.id);
    fs::create_dir_all(&paths.home).unwrap();
    fs::write(&paths.spec, SPEC).unwrap();
    fs::write(&paths.record, serde_json::to_vec(record).unwrap()).unwrap();
}

fn setup() -> Env {
    let env = Env::new();
    env.record(&env.project_root);
    env
}

fn json_show(env: &Env, target: &str) -> Value {
    show(env, target)
}

fn historical_clock(at: &str) -> String {
    at.parse::<jiff::Timestamp>()
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::system())
        .strftime("%Y-%m-%d %H:%M")
        .to_string()
}

fn refusal(output: std::process::Output) -> String {
    let text = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(1), "{text}");
    text
}

#[test]
fn address_prefers_live_then_newest_terminal_even_after_checkout_removal() {
    let env = setup();
    let checkout = env.home_root.join("gone/feat-x");
    let mut old = seed(&env, &env.project_root, &checkout, "done");
    old.terminal_at = Some("2000-01-01T00:00:00Z".parse().unwrap());
    save(&env, &old);
    let mut live = seed(&env, &env.project_root, &checkout, "parked");
    assert_eq!(
        json_show(&env, "review#feat/x")["record"]["id"],
        json!(live.id)
    );
    live.state = RunState::Canceled;
    live.terminal_at = Some("2001-01-01T00:00:00Z".parse().unwrap());
    save(&env, &live);
    assert_eq!(
        json_show(&env, "review#feat-x")["record"]["id"],
        json!(live.id)
    );
    let text = refusal(command(&env, &["workflow", "cancel", "review#feat-x"]));
    assert!(text.contains("no review run in feat-x"), "{text}");
    assert!(text.contains("review has no live runs"), "{text}");
}

#[test]
fn address_miss_names_live_addresses_and_empty_parts_are_usage_errors() {
    let env = setup();
    seed(&env, &env.project_root, &env.project_root, "active");
    let text = refusal(command(&env, &["workflow", "show", "review#missing"]));
    assert!(
        text.contains("no review run in missing\n  live runs: review#project"),
        "{text}"
    );
    for target in ["review#", "#project"] {
        let output = command(&env, &["workflow", "show", target]);
        let text = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(2), "{text}");
        assert!(
            text.contains("workflow and lane must be nonempty"),
            "{text}"
        );
    }
}

#[test]
fn colliding_addresses_refuse_with_ids_and_checkout_paths() {
    let env = setup();
    let first = seed(&env, &env.project_root, &env.project_root, "parked");
    let second = seed(
        &env,
        &env.project_root,
        &env.home_root.join("linked/project"),
        "active",
    );
    for (verb, flags) in [
        ("show", vec![]),
        ("resume", vec!["--at", "build", "--extend", "visits=+1"]),
        ("cancel", vec![]),
    ] {
        let mut args = vec!["workflow", verb, "review#project"];
        args.extend(&flags);
        let text = refusal(command(&env, &args));
        for record in [&first, &second] {
            assert!(text.contains(record.id.as_str()), "{text}");
            assert!(text.contains(record.scope.path.to_str().unwrap()), "{text}");
            let suffix = if flags.is_empty() {
                ""
            } else {
                " --at build --extend 'visits=+1'"
            };
            assert!(
                text.contains(&format!("rimz workflow {verb} {}{suffix}", record.id)),
                "{text}",
            );
        }
    }

    let env = Env::new();
    let first_room = env.home_root.join("first/project");
    let second_room = env.home_root.join("second/project");
    env.record(&first_room);
    env.record(&second_room);
    let records = [
        seed(&env, &first_room, &first_room.join("feat-a"), "parked"),
        seed(
            &env,
            &first_room,
            &env.home_root.join("linked/feat-a"),
            "parked",
        ),
        seed(&env, &second_room, &second_room.join("feat-a"), "parked"),
    ];
    let text = refusal(command(&env, &["workflow", "show", "review#feat-a"]));
    assert!(
        text.contains("rimz --root ~/second/project workflow show 'review#feat-a'"),
        "{text}"
    );
    let mut selected = text
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("rimz "))
        .map(|line| {
            let output = copied(&env, line, true);
            if output.status.success() {
                let value: Value = serde_json::from_slice(&output.stdout).unwrap();
                value["record"]["id"].as_str().unwrap().to_owned()
            } else {
                String::from_utf8(output.stderr).unwrap()
            }
        })
        .collect::<Vec<_>>();
    selected.sort();
    let mut expected = records.map(|record| record.id.to_string());
    expected.sort();
    assert_eq!(selected, expected, "{text}");
}

#[test]
fn bare_mutations_prefer_checkout_then_only_run_then_offer_quoted_commands() {
    let env = setup();
    let first = seed(&env, &env.project_root, &env.project_root, "parked");
    let second = seed(
        &env,
        &env.project_root,
        &env.home_root.join("feat-x"),
        "parked",
    );
    let receipt = success(command(&env, &["workflow", "cancel", "review"]));
    assert!(
        receipt.starts_with("canceled review#project\n"),
        "{receipt}"
    );
    assert_eq!(
        json_show(&env, first.id.as_str())["record"]["state"],
        "canceled"
    );
    let receipt = success(command(&env, &["workflow", "cancel", "review"]));
    assert!(receipt.starts_with("canceled review#feat-x\n"), "{receipt}");
    let mut first = first;
    first.scope.path = env.home_root.join("branch-a");
    save(&env, &first);
    save(&env, &second);
    let text = refusal(command(
        &env,
        &[
            "workflow",
            "resume",
            "review",
            "--at",
            "build",
            "--extend",
            "visits=+1",
        ],
    ));
    for address in ["review#branch-a", "review#feat-x"] {
        assert!(
            text.contains(&format!(
                "rimz workflow resume '{address}' --at build --extend 'visits=+1'"
            )),
            "{text}"
        );
    }
    for mut record in [first, second] {
        record.workflow = "-review".into();
        save(&env, &record);
    }
    let text = refusal(command(
        &env,
        &[
            "workflow",
            "resume",
            "--at",
            "build",
            "--extend",
            "visits=+1",
            "--",
            "-review",
        ],
    ));
    for address in ["-review#branch-a", "-review#feat-x"] {
        assert!(
            text.contains(&format!(
                "rimz workflow resume --at build --extend 'visits=+1' -- '{address}'"
            )),
            "{text}"
        );
    }

    let other = env.home_root.join("other/project");
    env.record(&other);
    let records = [
        seed(&env, &other, &other.join("feat-a"), "parked"),
        seed(&env, &other, &other.join("feat-b"), "parked"),
        seed(
            &env,
            &env.project_root,
            &env.project_root.join("feat-a"),
            "parked",
        ),
    ];
    let mut effects = Vec::new();
    let mut expected = Vec::new();
    for verb in ["resume", "cancel"] {
        for record in &records {
            save(&env, record);
        }
        let mut args = vec![
            "--root",
            other.to_str().unwrap(),
            "workflow",
            verb,
            "review",
        ];
        if verb == "resume" {
            args.extend(["--at", "build", "--extend", "visits=+1"]);
        }
        let text = refusal(command(&env, &args));
        for record in &records[..2] {
            let address = record.address().to_string();
            let choice = text
                .lines()
                .map(str::trim)
                .find(|line| line.starts_with("rimz ") && line.contains(&address))
                .unwrap();
            for record in &records {
                save(&env, record);
            }
            success(copied(&env, choice, false));
            let states = records
                .iter()
                .map(|candidate| {
                    let value = json_show(&env, candidate.id.as_str());
                    let record = &value["record"];
                    json!([record["state"], record["node"], record["visit_grant"]])
                })
                .collect::<Vec<_>>();
            let wanted = records
                .iter()
                .map(|candidate| match (candidate.id == record.id, verb) {
                    (true, "resume") => json!(["active", "build", 1]),
                    (true, _) => json!(["canceled", "gate", 0]),
                    (false, _) => json!(["parked", "gate", 0]),
                })
                .collect::<Vec<_>>();
            effects.push((verb, choice.to_owned(), states));
            expected.push((verb, choice.to_owned(), wanted));
        }
    }
    assert_eq!(effects, expected);
}

#[test]
fn bare_show_always_lists_live_runs_and_distinguishes_known_empty_from_unknown() {
    let env = setup();
    seed(&env, &env.project_root, &env.project_root, "parked");
    seed(
        &env,
        &env.project_root,
        &env.home_root.join("feat-x"),
        "done",
    );
    let rows = json_show(&env, "review");
    assert_eq!(rows.as_array().map(Vec::len), Some(1), "{rows:#}");
    assert_eq!(rows[0]["address"], "review#project");
    let text = success(command(&env, &["workflow", "show", "review"]));
    assert!(text.contains("NEEDS YOU"), "{text}");
    assert!(!text.contains("current node"), "{text}");
    let known = env.rimz_home().join("workflows/unused");
    fs::create_dir_all(known).unwrap();
    let text = success(command(&env, &["workflow", "show", "unused"]));
    assert!(text.contains("definition unavailable"), "{text}");
    assert!(
        text.ends_with("no active or parked runs of unused\n"),
        "{text}"
    );
    let text = refusal(command(&env, &["workflow", "show", "absent"]));
    assert!(text.contains("unknown workflow absent"), "{text}");
}

#[test]
fn rooms_refuse_outside_and_prefer_the_callers_room_without_creating_state() {
    let env = setup();
    let other = env.home_root.join("other/project");
    env.record(&other);
    let here = seed(&env, &env.project_root, &env.project_root, "parked");
    let mut there = seed(&env, &other, &other, "parked");
    let outside = env.home_root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    let text = refusal(
        env.rimz()
            .current_dir(&outside)
            .args(["workflow", "show", "review#project"])
            .output()
            .unwrap(),
    );
    for root in [&env.project_root, &other] {
        let relative = root.strip_prefix(&env.home_root).unwrap();
        assert!(
            text.contains(&format!("rimz --root ~/{}", relative.display())),
            "{text}"
        );
    }
    assert!(!env.state_path_for(&outside).root.exists());
    assert_eq!(
        json_show(&env, "review#project")["record"]["id"],
        json!(here.id)
    );
    let rows = success(command(&env, &["workflow", "list"]));
    assert!(
        rows.find("~/project").unwrap() < rows.find("~/other/project").unwrap(),
        "{rows}"
    );
    assert!(
        rows.contains("rimz --root ~/other/project workflow show 'review#project'"),
        "{rows}"
    );
    there.state = RunState::Done;
    there.terminal_at = Some(jiff::Timestamp::now());
    save(&env, &there);
    for target in ["review", "review#project"] {
        let output = success(
            env.rimz()
                .current_dir(&outside)
                .args(["workflow", "show", target, "--json"])
                .output()
                .unwrap(),
        );
        let value: Value = serde_json::from_str(&output).unwrap();
        let selected = if target == "review" {
            &value[0]["id"]
        } else {
            &value["record"]["id"]
        };
        assert_eq!(selected, &json!(here.id), "{value:#}");
    }
    let mut here = here;
    here.state = RunState::Done;
    here.terminal_at = Some(jiff::Timestamp::now());
    save(&env, &here);
    let text = success(
        env.rimz()
            .current_dir(&outside)
            .args(["workflow", "show", "review"])
            .output()
            .unwrap(),
    );
    assert!(text.contains("definition unavailable"), "{text}");
    assert!(
        text.ends_with("no active or parked runs of review\n"),
        "{text}"
    );
}

fn copied(env: &Env, text: &str, json: bool) -> std::process::Output {
    let mut args = shlex::split(text).unwrap();
    assert_eq!(args.remove(0), "rimz");
    if json {
        args.retain(|arg| arg != "--history");
        args.push("--json".into());
    }
    for arg in &mut args {
        if let Some(tail) = arg.strip_prefix("~/") {
            *arg = env.home_root.join(tail).to_str().unwrap().into();
        }
    }
    env.rimz()
        .env("RIMZ_BIN", "/bin/true")
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn receipt_hints_keep_the_resumed_runs_room() {
    let env = setup();
    let other = env.home_root.join("other/project");
    env.record(&other);
    seed(&env, &env.project_root, &env.project_root, "parked");
    let record = seed(&env, &other, &other, "parked");
    let receipt = success(
        env.rimz()
            .env("RIMZ_BIN", "/bin/true")
            .args([
                "--root",
                other.to_str().unwrap(),
                "workflow",
                "resume",
                "review#project",
            ])
            .output()
            .unwrap(),
    );
    let hint = receipt
        .lines()
        .find_map(|line| line.strip_prefix("→ "))
        .unwrap();
    let selected: Value = serde_json::from_str(&success(copied(&env, hint, true))).unwrap();
    assert_eq!(selected["record"]["id"], json!(record.id), "{receipt}");
}

#[test]
fn ids_keep_working_and_terminal_cancel_names_the_state() {
    let env = setup();
    let record = seed(&env, &env.project_root, &env.project_root, "parked");
    assert_eq!(
        json_show(&env, record.id.as_str())["record"]["id"],
        json!(record.id)
    );
    let resumed = success(
        env.rimz()
            .env("RIMZ_BIN", "/bin/true")
            .args(["workflow", "resume", record.id.as_str()])
            .output()
            .unwrap(),
    );
    assert_eq!(
        resumed,
        "resumed review#project at gate\n→ rimz workflow show 'review#project'\n"
    );
    let canceled = success(command(&env, &["workflow", "cancel", record.id.as_str()]));
    assert_eq!(
        canceled,
        "canceled review#project\n→ rimz workflow show 'review#project'\n"
    );
    let unchanged = success(command(&env, &["workflow", "cancel", record.id.as_str()]));
    assert!(
        unchanged.starts_with("review#project is already canceled; unchanged\n"),
        "{unchanged}"
    );
}

#[test]
fn receipts_report_pre_spawn_visits_extensions_and_immediate_bounds() {
    let env = setup();
    let mut record = seed(&env, &env.project_root, &env.project_root, "parked");
    record.node = "build".into();
    record.visits = serde_json::from_value(json!({"build":{"total":1,"keys":[]}})).unwrap();
    save(&env, &record);
    let output = env
        .rimz()
        .env("RIMZ_BIN", "/bin/true")
        .args(["workflow", "resume", "review#project"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let text = success(output);
    assert!(
        text.starts_with("resumed review#project at build · build 1 of 2 visits\n"),
        "{text}"
    );
    assert!(!stderr.contains("warning:"), "{stderr}");
    let output = env
        .rimz()
        .env("RIMZ_BIN", "/bin/true")
        .args([
            "workflow",
            "resume",
            "review",
            "--extend",
            "visits=+1",
            "--extend",
            "deadline=2000-01-01T00:00:00Z",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let text = success(output);
    assert!(
        text.starts_with(&format!(
            "extended review#project · every capped step +1 visits · deadline {}",
            historical_clock("2000-01-01T00:00:00Z")
        )),
        "{text}"
    );
    assert!(
        stderr.contains("deadline") && stderr.contains("--extend 'deadline=<when>'"),
        "{stderr}"
    );
    record.visits.get_mut("build").unwrap().total = 2;
    save(&env, &record);
    let output = env
        .rimz()
        .env("RIMZ_BIN", "/bin/true")
        .args(["workflow", "resume", "review#project"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    success(output);
    assert!(
        stderr
            .contains("build has used 2 of 2 visits (max-visits); the run will park again at once"),
        "{stderr}"
    );
    assert!(
        stderr.contains("rimz workflow resume 'review#project' --extend 'visits=+1'"),
        "{stderr}"
    );
    save(&env, &record);
    let paths = env.state_path_for(&env.project_root).workflow(&record.id);
    fs::write(paths.spec, format!("{SPEC}on.cap='gate'\n")).unwrap();
    let output = env
        .rimz()
        .env("RIMZ_BIN", "/bin/true")
        .args(["workflow", "resume", "review#project"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    success(output);
    assert!(stderr.contains("goes to gate at once"), "{stderr}");
    assert!(!stderr.contains("park again"), "{stderr}");

    record.state = RunState::Active;
    record.park = None;
    record.attempt = Some(
        serde_json::from_value(json!({
            "kind":"script", "seq":1, "node":"build", "started_at":jiff::Timestamp::now(),
            "host":{"pid":1},
        }))
        .unwrap(),
    );
    save(&env, &record);
    let future = (jiff::Timestamp::now() + jiff::SignedDuration::from_hours(2)).to_string();
    let output = env
        .rimz()
        .env("RIMZ_BIN", "/bin/true")
        .args([
            "workflow",
            "resume",
            "review#project",
            "--extend",
            &format!("deadline={future}"),
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(success(output).starts_with("extended review#project"));
    assert!(
        !stderr.contains("has used"),
        "an admitted visit does not re-enter the step: {stderr}"
    );
}

#[test]
fn loop_json_keeps_the_park_reason_and_text_after_cancel() {
    let env = setup();
    fs::write(
        env.rimz_home().join("loop.toml"),
        format!(
            "[tasks.review-task]\nworkflow='review'\nroot={:?}\nwhen=['team.stage=Done']\n",
            env.project_root
        ),
    )
    .unwrap();
    let mut record = seed(&env, &env.project_root, &env.project_root, "parked");
    record.task = Some("review-task".into());
    record.park.as_mut().unwrap().text = Some("Choose a target".into());
    save(&env, &record);
    let state = env.state_path_for(&env.project_root);
    let launch = json!({"at":record.created_at, "run":record.id, "scope":"worktree"});
    let ledger = json!({"version":1, "tasks":{"review-task":{
        (env.project_root.to_str().unwrap()):launch
    }}});
    fs::create_dir_all(state.root.join("records")).unwrap();
    fs::write(
        state.root.join("records/loop-workflow-launches.json"),
        serde_json::to_vec(&ledger).unwrap(),
    )
    .unwrap();
    success(command(&env, &["workflow", "cancel", record.id.as_str()]));
    let value: Value = serde_json::from_str(&success(command(
        &env,
        &["loop", "show", "review-task", "--json"],
    )))
    .unwrap();
    let row = value["workflow_runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == json!(record.id))
        .unwrap();
    assert_eq!(row["reason"], "user", "{row:#}");
    assert_eq!(row["text"], "Choose a target", "{row:#}");
    assert_eq!(row["state"], "canceled", "{row:#}");
    let text = success(command(&env, &["loop", "show", "review-task"]));
    assert!(!text.contains("run(s) parked"), "{text}");
}

#[test]
fn list_is_attention_first_with_state_ages_glyphs_and_address_json() {
    use jiff::SignedDuration;
    let env = setup();
    let now = jiff::Timestamp::now();
    let mut parked = seed(
        &env,
        &env.project_root,
        &env.home_root.join("z-park"),
        "parked",
    );
    parked.entered_at = now - SignedDuration::from_secs(3_600);
    parked.park.as_mut().unwrap().at = now - SignedDuration::from_secs(180);
    save(&env, &parked);
    let mut asking = seed(
        &env,
        &env.project_root,
        &env.home_root.join("a-ask"),
        "active",
    );
    asking.attempt = Some(
        serde_json::from_value(json!({
            "kind":"agent", "lifecycle":"turn", "seq":1, "node":"build", "started_at":now,
            "host":{"pid":1}, "run":{"run_id":rimz::ids::RunId::new(), "handle":"@fox"},
            "asking":{
                "since":now - SignedDuration::from_secs(120),
                "handle":"@fox", "kind":"permission", "text":"May I\ncontinue?",
            },
        }))
        .unwrap(),
    );
    save(&env, &asking);
    let mut sleeping = seed(
        &env,
        &env.project_root,
        &env.home_root.join("b-sleep"),
        "active",
    );
    sleeping.wait = Some(
        serde_json::from_value(
            json!({"node":"gate", "outcome":"pending", "first":now, "last":now, "count":3}),
        )
        .unwrap(),
    );
    sleeping.entered_at = now - SignedDuration::from_secs(60);
    save(&env, &sleeping);
    let mut running = seed(
        &env,
        &env.project_root,
        &env.home_root.join("c-run"),
        "active",
    );
    running.entered_at = now + SignedDuration::from_secs(3_600);
    save(&env, &running);
    let text = success(command(&env, &["workflow", "list"]));
    assert!(
        text.find("NEEDS YOU").unwrap_or(usize::MAX) < text.find("ACTIVE").unwrap_or(0),
        "{text}"
    );
    assert!(
        text.find("review#z-park").unwrap() < text.find("review#a-ask").unwrap(),
        "{text}"
    );
    let theme = rimz::config::ThemeConfig::default();
    let glyph = rimz::theme::theme_glyphs(&theme);
    for (lane, role, age) in [
        ("z-park", rimz::config::GlyphRole::StatusWaiting, "3m"),
        ("a-ask", rimz::config::GlyphRole::StatusWaiting, "2m"),
        ("b-sleep", rimz::config::GlyphRole::StatusSleeping, "1m"),
        ("c-run", rimz::config::GlyphRole::StatusWorking, "0s"),
    ] {
        let line = text
            .lines()
            .find(|line| line.contains(&format!("review#{lane}")) && !line.contains('→'))
            .unwrap();
        assert!(
            line.starts_with(&format!("  {} ", glyph(role))) && line.ends_with(age),
            "{line}"
        );
    }
    assert!(
        text.contains("  → rimz workflow show 'review#z-park'"),
        "{text}"
    );
    assert!(
        text.contains("asking you: @fox (permission): May I; continue?"),
        "{text}"
    );
    let rows: Value =
        serde_json::from_str(&success(command(&env, &["workflow", "list", "--json"]))).unwrap();
    assert!(
        rows.as_array()
            .unwrap()
            .iter()
            .all(|row| row["address"].is_string()),
        "{rows:#}"
    );
    assert_eq!(
        rows.as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == json!(sleeping.id))
            .unwrap()["detail"],
        "said pending, ×3"
    );
}

#[test]
fn empty_list_gives_the_start_command() {
    let env = setup();
    assert_eq!(
        success(command(&env, &["workflow", "list"])),
        "no active or parked runs; start one with rimz workflow run <name>\n"
    );
}

#[test]
fn list_derives_bound_phrases_from_the_sealed_spec_and_ignores_stale_parks() {
    let env = setup();
    let mut records = Vec::new();
    for reason in ["cap", "timeout", "deadline"] {
        let mut record = seed(
            &env,
            &env.project_root,
            &env.home_root.join(reason),
            "parked",
        );
        record.park.as_mut().unwrap().reason = serde_json::from_value(json!(reason)).unwrap();
        record.park.as_mut().unwrap().text = Some("old durable wording".into());
        match reason {
            "cap" => {
                record.node = "build".into();
                record.visit_grant = 1;
                record.visits =
                    serde_json::from_value(json!({"build":{"total":3,"keys":[]}})).unwrap();
            }
            "deadline" => record.deadline = Some("2000-01-01T00:00:00Z".parse().unwrap()),
            _ => {}
        }
        save(&env, &record);
        records.push(record);
    }
    let mut active = seed(
        &env,
        &env.project_root,
        &env.home_root.join("active"),
        "parked",
    );
    active.state = RunState::Active;
    save(&env, &active);
    let rows: Value =
        serde_json::from_str(&success(command(&env, &["workflow", "list", "--json"]))).unwrap();
    let details: Vec<_> = records
        .iter()
        .map(|record| {
            rows.as_array()
                .unwrap()
                .iter()
                .find(|row| row["id"] == json!(record.id))
                .unwrap()["detail"]
                .as_str()
                .unwrap()
        })
        .collect();
    assert!(
        details[0] == "cap: 3 of 3 visits"
            && details[1] == "timeout: waited 1h"
            && details[2]
                == format!(
                    "deadline: passed {}",
                    historical_clock("2000-01-01T00:00:00Z")
                ),
        "{details:?}"
    );
    assert_eq!(
        rows.as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == json!(active.id))
            .unwrap()["detail"],
        "checking"
    );
}

#[test]
fn run_receipt_hint_selects_its_run_with_a_shared_address() {
    let (env, _) = fixture(&format!("{HEADER}{WAIT}"), PENDING);
    seed(
        &env,
        &env.project_root,
        &env.home_root.join("linked/project"),
        "parked",
    );
    let receipt = success(
        env.rimz()
            .env("RIMZ_BIN", "/bin/true")
            .args(["workflow", "run", "review"])
            .output()
            .unwrap(),
    );
    let id = receipt.lines().next().unwrap();
    let hint = receipt
        .lines()
        .find_map(|line| line.strip_prefix("→ "))
        .unwrap();
    assert!(hint.contains(id), "{receipt}");
    let selected: Value = serde_json::from_str(&success(copied(&env, hint, true))).unwrap();
    assert_eq!(selected["record"]["id"], id, "{receipt}");
}

#[test]
fn run_receipt_hint_keeps_the_started_runs_room() {
    let (env, _) = fixture(&format!("{HEADER}{WAIT}"), PENDING);
    let other = env.home_root.join("other/project");
    env.record(&other);
    let here = seed(&env, &env.project_root, &env.project_root, "parked");
    let receipt = success(
        env.rimz()
            .env("RIMZ_BIN", "/bin/true")
            .args([
                "--root",
                other.to_str().unwrap(),
                "workflow",
                "run",
                "review",
            ])
            .output()
            .unwrap(),
    );
    let id = receipt.lines().next().unwrap();
    let hint = receipt
        .lines()
        .find_map(|line| line.strip_prefix("→ "))
        .unwrap();
    let selected: Value = serde_json::from_str(&success(copied(&env, hint, true))).unwrap();
    assert_eq!(selected["record"]["id"], id, "{receipt}");
    assert_ne!(selected["record"]["id"], json!(here.id), "{receipt}");
}

#[test]
fn run_receipt_preserves_id_and_directory_then_adds_the_address_hint() {
    let (env, _) = fixture(&format!("{HEADER}{WAIT}"), PENDING);
    let text = success(
        env.rimz()
            .env("RIMZ_BIN", "/bin/true")
            .args(["workflow", "run", "review"])
            .output()
            .unwrap(),
    );
    let lines: Vec<_> = text.lines().collect();
    assert!(
        lines[0].parse::<rimz::ids::WorkflowRunId>().is_ok(),
        "{text}"
    );
    assert!(Path::new(lines[1]).is_dir(), "{text}");
    assert_eq!(
        lines.get(2),
        Some(&"→ rimz workflow show 'review#project'"),
        "{text}"
    );
}
