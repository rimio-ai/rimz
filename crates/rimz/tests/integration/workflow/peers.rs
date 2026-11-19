//! Runs started from a run or an agent: who started them, how deep they sit, what they owe, and
//! `workflow wait` on them.

use super::*;
use std::process::{Child, Command, Output, Stdio};

use rimz::ids::{AgentKind, AgentSessionId};
use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope};

/// Deploys workflow `name`, whose check waits until its marker file exists and then passes.
fn deploy(env: &Env, name: &str) -> PathBuf {
    let dir = env.rimz_home().join("workflows").join(name);
    let marker = dir.join("passed");
    fs::create_dir_all(dir.join("checks")).unwrap();
    fs::write(dir.join("workflow.toml"), format!("{HEADER}{WAIT}")).unwrap();
    let pass = "printf '%s\\n' '{\"outcome\":\"done\"}'";
    fs::write(
        dir.join("checks/pr.sh"),
        format!(
            "if [ -e '{}' ]; then {pass}; else {PENDING}fi\n",
            marker.display()
        ),
    )
    .unwrap();
    marker
}

fn run(env: &Env, name: &str, extra: &[&str], vars: &[(&str, &str)]) -> Output {
    let mut command = env.rimz();
    command
        .args(["workflow", "run", name, "--prompt", "Go"])
        .args(extra);
    for (key, value) in vars {
        command.env(key, value);
    }
    command.output().unwrap()
}

fn started(output: Output) -> String {
    success(output)
        .split_whitespace()
        .find(|word| word.starts_with("wf_"))
        .expect("run receipt must include its id")
        .into()
}

/// Starts `name` and waits until its check is pending.
fn pending(env: &Env, name: &str, vars: &[(&str, &str)]) -> (String, Value) {
    let id = started(run(env, name, &[], vars));
    let value = until(env, &id, |value| value["record"]["wait"].is_object());
    (id, value)
}

fn rewrite(env: &Env, id: &str, edit: impl FnOnce(&mut Value)) {
    let state =
        rimz::StatePaths::for_project_root_under(&env.project_root, &env.rimz_home()).unwrap();
    let _guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    let paths = state.workflow(&id.parse().unwrap());
    let mut record: Value = serde_json::from_slice(&fs::read(&paths.record).unwrap()).unwrap();
    edit(&mut record);
    rimz::disk::atomic::write_temp_then_rename(&paths.record, &record).unwrap();
}

/// Lets a pending run's check pass and advances it to its end.
fn pass(env: &Env, id: &str, marker: &Path) {
    fs::write(marker, "").unwrap();
    rewrite(env, id, |record| {
        record["recheck_at"] = json!("2000-01-01T00:00:00Z");
    });
    success(command(env, &["workflow", "advance", id]));
    settled(env, id, "done");
}

fn seed_agent(env: &Env, name: &str) {
    seed_agent_as(env, name, None);
}

/// Seeds agent `name` as launched by `automation`: a kept step agent keeps its run's stamp.
fn seed_agent_as(env: &Env, name: &str, automation: Option<rimz::agents::Automation>) {
    let workspace = env.resolve_workspace(&env.project_root);
    env.store()
        .append_event(&EventEnvelope::agent_launched(
            workspace.workspace_id,
            &workspace.session_name,
            &AgentKind::new_unchecked("claude"),
            AgentLaunchPayload {
                system_prompt_digest: None,
                agent_id: AgentSessionId::from(format!("launch_{name}")),
                launch_id: Some(AgentSessionId::from(format!("launch_{name}"))),
                agent_name: name.to_owned(),
                agent_name_explicit: true,
                launch: rimz::agents::LaunchParams {
                    automation,
                    lane: Some(crate::common::wait::stamped_lane(
                        env,
                        &env.project_root,
                        "main",
                    )),
                    ..Default::default()
                },
                state: AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: None,
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: Some("main".to_owned()),
                prompt: None,
                description: None,
            },
        ))
        .unwrap();
}

const AGENT: [(&str, &str); 2] = [
    (rimz::harness::launch::ENV_AGENT_KIND, "claude"),
    (rimz::harness::launch::ENV_AGENT_ID, "launch_planner"),
];

fn listed(env: &Env, id: &str) -> Value {
    let list: Value =
        serde_json::from_str(&success(command(env, &["workflow", "list", "--json"]))).unwrap();
    list.as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("{id} is listed: {list:#}"))
}

fn live_homes(env: &Env) -> usize {
    let state = env.state_path_for(&env.project_root);
    fs::read_dir(
        state
            .workflow(&rimz::ids::WorkflowRunId::new())
            .home
            .parent()
            .unwrap(),
    )
    .map(|entries| entries.count())
    .unwrap_or(0)
}

#[test]
fn a_run_started_from_a_run_sits_one_deeper_and_one_past_the_limit_refuses() {
    let env = Env::new();
    env.record(&env.project_root);
    for name in ["outer", "inner", "deepest"] {
        deploy(&env, name);
    }
    let (outer, _) = pending(&env, "outer", &[]);
    fs::write(
        env.rimz_home().join("config.toml"),
        "[agents]\nmax-workflow-depth = 2\n",
    )
    .unwrap();
    let (inner, value) = pending(&env, "inner", &[("RIMZ_WORKFLOW_RUN", &outer)]);
    let record = &value["record"];
    assert_eq!(
        (&record["starter"], &record["depth"], &record["loop_report"]),
        (
            &json!({"by":"run", "run":outer, "node":"pr"}),
            &json!(2),
            &json!("none")
        ),
        "{record:#}"
    );
    assert_eq!(
        value["starter"],
        json!({"by":"run", "run":outer, "node":"pr"})
    );
    assert_eq!(listed(&env, &inner)["starter"], value["starter"]);
    let text = success(command(&env, &["workflow", "show", &inner]));
    assert!(text.contains(&format!("run {outer} at pr")), "{text}");

    let homes = live_homes(&env);
    let refused = run(&env, "deepest", &[], &[("RIMZ_WORKFLOW_RUN", &inner)]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success()
            && stderr.contains("would start at workflow depth 3")
            && stderr.contains("max-workflow-depth` of 2")
            && stderr.contains("nothing was created"),
        "{stderr}"
    );
    assert_eq!(live_homes(&env), homes, "the refusal created no home");
}

#[test]
fn an_agents_run_owes_it_a_report_unless_detached_and_names_it() {
    let env = Env::new();
    env.record(&env.project_root);
    seed_agent(&env, "planner");
    for name in ["owed", "detached", "plain"] {
        deploy(&env, name);
    }
    let (owed, value) = pending(&env, "owed", &AGENT);
    let agent = json!({"by":"agent", "kind":"claude", "launch":"launch_planner"});
    assert_eq!(
        (
            &value["record"]["starter"],
            &value["record"]["depth"],
            &value["record"]["loop_report"]
        ),
        (&agent, &json!(1), &json!("owed")),
        "{:#}",
        value["record"]
    );
    let mut named = agent.clone();
    named["handle"] = json!("@planner");
    assert_eq!(value["starter"], named);
    assert_eq!(listed(&env, &owed)["starter"], named);
    let text = success(command(&env, &["workflow", "show", &owed]));
    assert!(text.contains("@planner"), "{text}");

    let detached = started(run(&env, "detached", &["--detach"], &AGENT));
    let value = until(&env, &detached, |value| value["record"]["wait"].is_object());
    assert_eq!(
        (&value["record"]["starter"], &value["record"]["loop_report"]),
        (&agent, &json!("none"))
    );
    // By hand: no starter, nothing owed, and --detach is accepted all the same.
    let plain = started(run(&env, "plain", &["--detach"], &[]));
    let value = until(&env, &plain, |value| value["record"]["wait"].is_object());
    assert_eq!(
        (
            &value["record"]["starter"],
            &value["record"]["depth"],
            &value["record"]["loop_report"]
        ),
        (&Value::Null, &json!(1), &json!("none"))
    );
}

#[test]
fn a_kept_step_agent_starts_runs_as_itself_while_its_run_lives_and_after_it_ends() {
    let env = Env::new();
    env.record(&env.project_root);
    // The step's agent outlived its step: its pin is gone, its automation stamp and variables
    // still name the run.
    seed_agent_as(
        &env,
        "planner",
        Some(rimz::agents::Automation::Workflow {
            workflow: "outer".into(),
            step: "pr".into(),
            depth: Some(1),
        }),
    );
    let marker = deploy(&env, "outer");
    for name in ["beside", "after"] {
        deploy(&env, name);
    }
    let (outer, _) = pending(&env, "outer", &[]);
    let vars = [AGENT[0], AGENT[1], ("RIMZ_WORKFLOW_RUN", outer.as_str())];
    let agent = json!({"by":"agent", "kind":"claude", "launch":"launch_planner"});
    let (_, beside) = pending(&env, "beside", &vars);
    pass(&env, &outer, &marker);
    let (_, after) = pending(&env, "after", &vars);
    for value in [beside, after] {
        let record = &value["record"];
        assert_eq!(
            (&record["starter"], &record["depth"], &record["loop_report"]),
            (&agent, &json!(2), &json!("owed")),
            "{record:#}"
        );
    }
}

#[test]
fn a_check_carrying_a_kept_agents_variables_starts_its_run_as_the_run() {
    let env = Env::new();
    env.record(&env.project_root);
    seed_agent(&env, "planner");
    deploy(&env, "inner");
    let dir = env.rimz_home().join("workflows").join("spawner");
    fs::create_dir_all(dir.join("checks")).unwrap();
    fs::write(dir.join("workflow.toml"), format!("{HEADER}{WAIT}")).unwrap();
    let receipt = dir.join("inner.receipt");
    fs::write(
        dir.join("checks/pr.sh"),
        format!(
            "[ -e '{receipt}' ] || '{rimz}' workflow run inner --prompt Go > '{receipt}'\nprintf '%s\\n' '{{\"outcome\":\"done\"}}'\n",
            receipt = receipt.display(),
            rimz = env.rimz().get_program().to_string_lossy(),
        ),
    )
    .unwrap();
    // The advance and its check inherit the caller's agent variables.
    let spawner = started(run(&env, "spawner", &[], &AGENT));
    settled(&env, &spawner, "done");
    let inner = fs::read_to_string(&receipt)
        .unwrap()
        .split_whitespace()
        .find(|word| word.starts_with("wf_"))
        .expect("the check started inner")
        .to_owned();
    let value = until(&env, &inner, |value| value["record"]["wait"].is_object());
    let record = &value["record"];
    assert_eq!(
        (&record["starter"], &record["depth"], &record["loop_report"]),
        (
            &json!({"by":"run", "run":spawner, "node":"pr"}),
            &json!(2),
            &json!("none")
        ),
        "{record:#}"
    );
}

fn wait(env: &Env, args: &[&str]) -> Command {
    let mut command = env.rimz();
    command.args(["workflow", "wait"]).args(args);
    command
}

fn spawn(mut command: Command) -> Child {
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn report(output: &Output) -> Vec<Value> {
    serde_json::from_slice::<Value>(&output.stdout)
        .unwrap_or_else(|error| {
            panic!(
                "{error}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        })
        .as_array()
        .unwrap()
        .clone()
}

#[test]
fn wait_returns_when_its_runs_end_with_their_endings_and_exit() {
    let env = Env::new();
    env.record(&env.project_root);
    let markers: Vec<_> = ["one", "two", "three", "four", "five"]
        .map(|name| deploy(&env, name))
        .into();
    let (one, _) = pending(&env, "one", &[]);
    let (two, _) = pending(&env, "two", &[]);

    // All: the wait outlasts the first ending and returns at the second.
    let child = spawn(wait(&env, &[&one, &two, "--json", "--timeout", "60s"]));
    pass(&env, &one, &markers[0]);
    pass(&env, &two, &markers[1]);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let entries = report(&output);
    assert_eq!(entries.len(), 2);
    for (entry, id) in entries.iter().zip([&one, &two]) {
        assert_eq!(
            (
                &entry["id"],
                &entry["outcome"],
                &entry["state"],
                &entry["exit"]
            ),
            (&json!(id), &json!("ended"), &json!("done"), &json!(0)),
            "{entry:#}"
        );
        assert!(
            entry["checkout"].is_string() && entry["notes"].is_string(),
            "{entry:#}"
        );
    }

    // Any: the first to end decides, and only it is reported.
    let (three, _) = pending(&env, "three", &[]);
    let output = wait(&env, &[&one, &three, "--any"]).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.lines().count() == 1 && text.contains(&one) && text.contains("· checkout "),
        "{text}"
    );

    // Any, with two runs ended before one look: the earlier terminal stamp decides, not argv.
    let (four, _) = pending(&env, "four", &[]);
    let (five, _) = pending(&env, "five", &[]);
    pass(&env, &five, &markers[4]);
    pass(&env, &four, &markers[3]);
    let output = wait(&env, &[&four, &five, "--any", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let entries = report(&output);
    assert_eq!(
        entries.iter().map(|entry| &entry["id"]).collect::<Vec<_>>(),
        [&json!(five)],
        "{entries:#?}"
    );

    // A canceled run exits 1.
    let child = spawn(wait(&env, &[&three, "--timeout", "60s"]));
    success(command(&env, &["workflow", "cancel", &three]));
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains(&three));
}

#[test]
fn wait_times_out_refuses_unknown_runs_and_stops_for_a_prompt() {
    let env = Env::new();
    env.record(&env.project_root);
    deploy(&env, "slow");
    let (slow, _) = pending(&env, "slow", &[]);

    let output = wait(&env, &[&slow, "--timeout", "1s"]).output().unwrap();
    assert_eq!(output.status.code(), Some(124), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains(&format!("{slow}): still active at pr")),
        "{stdout}"
    );
    assert!(
        stderr.contains("timed out after 1s; the runs still going are left running"),
        "{stderr}"
    );

    let unknown = rimz::ids::WorkflowRunId::new().to_string();
    let output = wait(&env, &[&slow, &unknown]).output().unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains(&format!("unknown workflow run {unknown}")),
        "{output:?}"
    );

    seed_agent(&env, "planner");
    let planner = env
        .store()
        .snapshot_cached()
        .unwrap()
        .agents
        .iter()
        .find(|agent| agent.name.as_deref() == Some("planner"))
        .cloned()
        .expect("planner card");
    let message = rimz::store::message::MessageRecord::new(
        env.workspace_id.clone(),
        &planner,
        "follow-up".to_owned(),
        rimz::store::message::DeliveryGate::Done,
    );
    env.store().queue_message(&message, "rimz-test").unwrap();
    let mut command = wait(&env, &[&slow, "--timeout", "30s", "--json"]);
    for (key, value) in AGENT {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(75), "{output:?}");
    let entry = &report(&output)[0];
    assert_eq!(
        (&entry["outcome"], &entry["exit"]),
        (&json!("running"), &json!(75)),
        "a run still going carries the stop's own exit"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("rimz: stopped waiting: 1 message"),
        "{output:?}"
    );
    assert_eq!(
        show(&env, &slow)["record"]["state"],
        "active",
        "the run is left running"
    );
}

#[test]
fn wait_returns_at_once_when_a_run_needs_a_person() {
    let env = Env::new();
    env.record(&env.project_root);
    for name in ["parked", "asking", "quiet"] {
        deploy(&env, name);
    }
    let (parked, _) = pending(&env, "parked", &[]);
    let (asking, _) = pending(&env, "asking", &[]);
    let (quiet, _) = pending(&env, "quiet", &[]);
    let now = jiff::Timestamp::now();
    rewrite(&env, &parked, |record| {
        record["state"] = json!("parked");
        record["park"] = json!({"reason":"user", "text":"look at this", "at":now});
    });
    rewrite(&env, &asking, |record| {
        record["attempt"] = json!({
            "kind":"agent", "lifecycle":"turn", "seq":1, "node":"pr", "started_at":now,
            "host":{"pid":1}, "run":{"run_id":rimz::ids::RunId::new(), "handle":"@fox"},
            "asking":{"since":now, "handle":"@fox", "kind":"permission", "text":"May I push?"},
        });
    });
    for (id, why) in [(&parked, "look at this"), (&asking, "May I push?")] {
        for any in [false, true] {
            let mut args = vec![quiet.as_str(), id.as_str()];
            if any {
                args.push("--any");
            }
            let output = wait(&env, &args).output().unwrap();
            assert_eq!(output.status.code(), Some(3), "{any}: {output:?}");
            let text = String::from_utf8_lossy(&output.stdout);
            assert!(
                text.contains(&format!("{id}): needs you: "))
                    && text.contains(why)
                    && text.contains(&format!("{quiet}): still active at pr")),
                "{text}"
            );
        }
    }
    let output = wait(&env, &[&quiet, &asking, "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    let entries = report(&output);
    assert_eq!(
        entries
            .iter()
            .map(|entry| (&entry["outcome"], &entry["exit"]))
            .collect::<Vec<_>>(),
        [
            (&json!("running"), &json!(3)),
            (&json!("needs_you"), &json!(3))
        ],
        "{entries:#?}"
    );
}

#[test]
fn wait_reads_a_home_deleted_under_it_as_a_quiet_ending() {
    let env = Env::new();
    env.record(&env.project_root);
    deploy(&env, "forgotten");
    let (forgotten, _) = pending(&env, "forgotten", &[]);
    let child = spawn(wait(&env, &[&forgotten, "--json", "--timeout", "60s"]));
    // The wait's first look finds the home; the collector of a quiet task run then deletes it.
    std::thread::sleep(Duration::from_secs(2));
    let home = env
        .state_path_for(&env.project_root)
        .workflow(&forgotten.parse().unwrap())
        .home;
    fs::remove_dir_all(&home).unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let entry = &report(&output)[0];
    assert_eq!(
        (&entry["outcome"], &entry["state"], &entry["exit"]),
        (&json!("ended"), &json!("done"), &json!(0)),
        "{entry:#}"
    );
}
