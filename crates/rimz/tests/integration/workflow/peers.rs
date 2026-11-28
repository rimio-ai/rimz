//! Runs started from a run or an agent: who started them, how deep they sit, what they owe, and
//! `workflow wait` on them.

use super::*;
use std::process::{Child, Command, Output, Stdio};

use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope};
use rimz::store::message::{DeliveryGate, HarnessNotice, MessageRecord, MessageSender};
use std::io::Write as _;
use std::os::unix::net::UnixListener;

use super::commands::accept_gate;

/// Deploys workflow `name`, whose check waits until its marker file exists and then passes.
pub(super) fn deploy(env: &Env, name: &str) -> PathBuf {
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

pub(super) fn started(output: Output) -> String {
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
pub(super) fn pass(env: &Env, id: &str, marker: &Path) {
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

fn report_of(output: &Output) -> Vec<Value> {
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
    let entries = report_of(&output);
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
        text.lines().count() == 1
            && text.contains("one#main: done")
            && text.contains("· checkout "),
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
    let entries = report_of(&output);
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
    assert!(String::from_utf8_lossy(&output.stdout).contains("three#main: canceled"));
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
    assert!(stdout.contains("slow#main: still active at pr"), "{stdout}");
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
    let entry = &report_of(&output)[0];
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
    for (id, name, why) in [
        (&parked, "parked", "look at this"),
        (&asking, "asking", "May I push?"),
    ] {
        for any in [false, true] {
            let mut args = vec![quiet.as_str(), id.as_str()];
            if any {
                args.push("--any");
            }
            let output = wait(&env, &args).output().unwrap();
            assert_eq!(output.status.code(), Some(3), "{any}: {output:?}");
            let text = String::from_utf8_lossy(&output.stdout);
            assert!(
                text.contains(&format!("{name}#main: needs you: "))
                    && text.contains(why)
                    && text.contains("quiet#main: still active at pr"),
                "{text}"
            );
        }
    }
    let output = wait(&env, &[&quiet, &asking, "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    let entries = report_of(&output);
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
    let entry = &report_of(&output)[0];
    assert_eq!(
        (&entry["outcome"], &entry["state"], &entry["exit"]),
        (&json!("ended"), &json!("done"), &json!(0)),
        "{entry:#}"
    );
}

/// How an agent reads run `id`: its address where that selects it, else its id.
fn run_shown(env: &Env, id: &str) -> String {
    rimz::store::workflow::RunNames::new(&env.state_path_for(&env.project_root))
        .shown(&id.parse().unwrap())
}

pub(super) fn reports(env: &Env) -> Vec<MessageRecord> {
    let (queued, history) = env.store().list_messages_and_history().unwrap();
    queued
        .into_iter()
        .chain(history)
        .filter(|message| {
            message.sender
                == MessageSender::Harness {
                    notice: HarnessNotice::WorkflowReport,
                }
        })
        .collect()
}

fn end_agent(env: &Env, name: &str) {
    let workspace = env.resolve_workspace(&env.project_root);
    let ended = AgentLifecycleObservation::new(
        Some(format!("launch_{name}").into()),
        LifecycleSignal::Ended,
    );
    env.store()
        .append_event(&EventEnvelope::agent_lifecycle(
            workspace.workspace_id,
            &workspace.session_name,
            "claude",
            "AgentEnded",
            &ended,
        ))
        .unwrap();
}

#[test]
fn an_agents_run_reports_its_end_once_to_its_starter_and_leaves_the_live_home() {
    let env = Env::new();
    env.record(&env.project_root);
    seed_agent(&env, "planner");
    let markers: Vec<_> = ["owed", "detached", "nested"]
        .map(|name| deploy(&env, name))
        .into();
    let (owed, _) = pending(&env, "owed", &AGENT);
    let detached = started(run(&env, "detached", &["--detach"], &AGENT));
    until(&env, &detached, |value| value["record"]["wait"].is_object());
    let (nested, _) = pending(&env, "nested", &[("RIMZ_WORKFLOW_RUN", &owed)]);

    // A detached start and a run's own start owe no report.
    pass(&env, &detached, &markers[1]);
    pass(&env, &nested, &markers[2]);
    assert_eq!(reports(&env).len(), 0);

    pass(&env, &owed, &markers[0]);
    let sent = reports(&env);
    assert_eq!(sent.len(), 1, "{sent:#?}");
    let report = &sent[0];
    assert_eq!(
        (
            report.agent_id.as_str(),
            report.launch_id.as_ref().map(AgentSessionId::as_str),
            &report.gate,
        ),
        (
            "launch_planner",
            Some("launch_planner"),
            &DeliveryGate::Done
        )
    );
    let entry = &report_of(&wait(&env, &[&owed, "--json"]).output().unwrap())[0];
    for part in [
        "workflow run owed#main (owed) ended done".to_owned(),
        format!("checkout {}", entry["checkout"].as_str().unwrap()),
        format!("notes {}", entry["notes"].as_str().unwrap()),
        "rimz workflow show 'owed#main' --history".to_owned(),
    ] {
        assert!(report.text.contains(&part), "{part}: {}", report.text);
    }
    let record = &show(&env, &owed)["record"];
    assert_eq!(
        (&record["loop_report"], &record["report_message"]),
        (&json!("sent"), &json!(report.message_id)),
        "{record:#}"
    );
    assert_eq!(live_homes(&env), 0, "every ended run left the live home");
    // A finished run still names who started it.
    assert_eq!(
        (
            &show(&env, &owed)["starter"],
            &show(&env, &nested)["starter"]
        ),
        (
            &json!({"by":"agent", "kind":"claude", "launch":"launch_planner", "handle":"@planner"}),
            &json!({"by":"run", "run":owed, "node":"pr"})
        )
    );

    // Collecting again reports nothing more.
    command(&env, &["workflow", "advance", &owed]);
    assert_eq!(reports(&env).len(), 1);
}

#[test]
fn an_ended_starter_gets_no_report_and_its_run_settles() {
    let env = Env::new();
    env.record(&env.project_root);
    seed_agent(&env, "planner");
    let marker = deploy(&env, "owed");
    let (owed, _) = pending(&env, "owed", &AGENT);
    end_agent(&env, "planner");
    pass(&env, &owed, &marker);
    assert_eq!(reports(&env).len(), 0);
    assert_eq!(show(&env, &owed)["record"]["loop_report"], "sent");
    assert_eq!(live_homes(&env), 0, "the settled run left the live home");
}

#[test]
fn a_lost_collection_reports_on_the_next_pass_once() {
    use std::os::unix::net::UnixListener;
    let env = Env::new();
    env.record(&env.project_root);
    seed_agent(&env, "planner");
    let marker = deploy(&env, "owed");
    // Start with a no-op detached advance so only this test's own advances can collect `owed`.
    let owed = started(run(
        &env,
        "owed",
        &[],
        &[AGENT[0], AGENT[1], ("RIMZ_BIN", "/bin/true")],
    ));
    fs::write(&marker, "").unwrap();
    let socket = env.runtime_root.join("report.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut advance = env.rimz();
    advance
        .args(["workflow", "advance", &owed])
        .env("RIMZ_TEST_WORKFLOW_REPORT_GATE", &socket);
    let mut child = spawn(advance);
    let deadline = Instant::now() + Duration::from_secs(30);
    let held = loop {
        if let Ok((stream, _)) = listener.accept() {
            break Some(stream);
        }
        if Instant::now() > deadline || child.try_wait().unwrap().is_some() {
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let _ = child.kill();
    child.wait().unwrap();
    assert!(
        held.is_some(),
        "the collection reaches the starter's report"
    );
    assert_eq!(reports(&env).len(), 0, "the killed collection sent nothing");
    let record = &show(&env, &owed)["record"];
    assert_eq!(
        (&record["state"], &record["loop_report"]),
        (&json!("done"), &json!("owed")),
        "{record:#}"
    );
    assert_eq!(live_homes(&env), 1, "an owed run stays in the live home");

    success(command(&env, &["workflow", "advance", &owed]));
    command(&env, &["workflow", "advance", &owed]);
    let sent = reports(&env);
    assert_eq!(sent.len(), 1, "{sent:#?}");
    assert_eq!(sent[0].agent_id.as_str(), "launch_planner");
    assert_eq!(live_homes(&env), 0);
}

#[test]
fn a_stranded_run_reports_to_its_starter_as_failed() {
    let env = Env::new();
    env.record(&env.project_root);
    seed_agent(&env, "planner");
    deploy(&env, "owed");
    let (owed, _) = pending(&env, "owed", &AGENT);
    rewrite(&env, &owed, |record| {
        record["version"] = json!(rimz::store::workflow::VERSION - 1);
    });
    success(command(&env, &["workflow", "reconcile"]));
    let sent = reports(&env);
    assert_eq!(sent.len(), 1, "{sent:#?}");
    let text = &sent[0].text;
    // A stranded run is no longer on record, so its address selects nothing and the report
    // names its id, and the history command the report prints reaches it.
    assert!(
        text.contains(&format!("workflow run {owed} (owed) ended failed")),
        "{text}"
    );
    let history = text
        .lines()
        .find_map(|line| line.strip_prefix("history: rimz "))
        .unwrap_or_else(|| panic!("the report prints its history command: {text}"));
    let args = shlex::split(history).unwrap();
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    assert!(success(command(&env, &args)).contains(&owed), "{history}");
    assert_eq!(live_homes(&env), 0);
}

/// Makes the project a git checkout with one commit holding everything in it.
fn commit_project(env: &Env) -> String {
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .current_dir(&env.project_root)
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "-A"]);
    git(&["commit", "--allow-empty", "-qm", "base"]);
    git(&["rev-parse", "HEAD"])
}

/// Deploys workflow 'fan', which waits at its 'gate' check until the returned marker exists,
/// then runs the 'spread' step given as TOML, whose 'then' is the 'after' check that passes.
fn deploy_fan(env: &Env, spread: &str) -> PathBuf {
    let dir = env.rimz_home().join("workflows/fan");
    let marker = dir.join("open");
    let _ = fs::remove_file(&marker);
    fs::create_dir_all(dir.join("checks")).unwrap();
    fs::write(
        dir.join("workflow.toml"),
        format!(
            "[workflow]\nscope = 'worktree'\nstart = 'gate'\n\
             [script.gate]\nrun = 'sh checks/gate.sh'\non = {{ pending = '@wait', done = 'spread' }}\ntimeout = '1h'\n\
             [runs.spread]\n{spread}\n\
             [script.after]\nrun = 'sh checks/after.sh'\non = {{ done = '@done' }}\ntimeout = '1h'\n"
        ),
    )
    .unwrap();
    let pass = "printf '%s\\n' '{\"outcome\":\"done\"}'";
    fs::write(
        dir.join("checks/gate.sh"),
        format!(
            "if [ -e '{}' ]; then {pass}; else {PENDING}fi\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::write(dir.join("checks/after.sh"), format!("{pass}\n")).unwrap();
    fs::write(dir.join("step.md"), "Step").unwrap();
    fs::write(dir.join("entry.md"), "Entry").unwrap();
    marker
}

const TWO_CHILDREN: &str = "prompt-file = 'step.md'\nstart = [{ workflow = 'child' }, { workflow = 'child', prompt-file = 'entry.md' }]\nthen = 'after'";

/// Opens the fan's gate and advances it into its 'spread' step.
fn open_gate(env: &Env, id: &str, marker: &Path) -> Value {
    fs::write(marker, "").unwrap();
    rewrite(env, id, |record| {
        record["recheck_at"] = json!("2000-01-01T00:00:00Z");
    });
    success(command(env, &["workflow", "advance", id]));
    until(env, id, |value| value["record"]["node"] != "gate")
}

/// The runs a fan's 'spread' attempt reserved.
fn children(value: &Value) -> Vec<String> {
    value["record"]["attempt"]["runs"]["runs"]
        .as_array()
        .unwrap_or_else(|| panic!("the attempt reserves its runs: {value:#}"))
        .iter()
        .map(|run| run["id"].as_str().unwrap().to_owned())
        .collect()
}

fn head_of(path: &Path) -> String {
    let output = Command::new("git")
        .current_dir(path)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn a_runs_step_starts_each_entry_in_its_own_worktree_and_follows_then_once_both_ended() {
    let env = Env::new();
    env.record(&env.project_root);
    let child_marker = deploy(&env, "child");
    let gate = deploy_fan(&env, TWO_CHILDREN);
    let base = commit_project(&env);
    let parent = started(run(&env, "fan", &[], &[]));
    until(&env, &parent, |value| value["record"]["wait"].is_object());
    let signs = run_home(&env, &parent).work.join("signs");
    fs::create_dir_all(&signs).unwrap();
    fs::write(signs.join("spread.md"), "Sign").unwrap();
    let value = open_gate(&env, &parent, &gate);
    let ids = children(&value);
    assert_eq!(ids.len(), 2, "{value:#}");
    assert_ne!(ids[0], ids[1]);
    assert_eq!(value["record"]["attempt"]["runs"]["base"], json!(base));
    assert_eq!(value["record"]["wait"]["outcome"], "runs", "{value:#}");
    assert_eq!(value["sign"], json!(signs.join("spread.md")), "{value:#}");
    let steps = assists_once(&env, "workflow_steps", 1);
    let steps: Vec<_> = steps["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["assist"] == "workflow_step")
        .collect();
    assert_eq!(steps.len(), 1, "{steps:#?}");
    assert_eq!(steps[0]["step"], "spread", "{steps:#?}");

    let mut checkouts = Vec::new();
    for (id, prompt) in ids
        .iter()
        .zip(["Step\n\nSign\n\nGo", "Step\n\nEntry\n\nSign\n\nGo"])
    {
        let child = until(&env, id, |value| value["record"]["wait"].is_object());
        let record = &child["record"];
        assert_eq!(
            (
                &record["starter"],
                &record["depth"],
                &record["loop_report"],
                &record["prompt"]
            ),
            (
                &json!({"by":"run", "run":parent, "node":"spread"}),
                &json!(2),
                &json!("owed"),
                &json!(prompt)
            ),
            "{record:#}"
        );
        let checkout = PathBuf::from(record["scope"]["path"].as_str().unwrap());
        assert_ne!(checkout, env.project_root.canonicalize().unwrap());
        assert_eq!(head_of(&checkout), base, "{}", checkout.display());
        checkouts.push(checkout);
    }
    assert_ne!(checkouts[0], checkouts[1]);
    assert_eq!(
        rimz::worktree::discover_owned(&env.project_root)
            .unwrap()
            .len(),
        2
    );
    let announced: Vec<Value> = env
        .read_events()
        .into_iter()
        .map(|event| serde_json::from_str::<Value>(event.params.get()).unwrap())
        .filter(|event| event["name"] == "worktree.created")
        .collect();
    assert_eq!(
        announced
            .iter()
            .map(|event| {
                PathBuf::from(event["payload"]["path"].as_str().unwrap())
                    .canonicalize()
                    .unwrap()
            })
            .collect::<Vec<_>>(),
        checkouts,
        "{announced:#?}"
    );
    let shown = String::from_utf8(command(&env, &["workflow", "show", &parent]).stdout).unwrap();
    assert!(
        shown.contains("runs: 0 of 2 started runs ended")
            && shown.lines().any(|line| {
                line.trim_start().starts_with("sign ") && line.ends_with("signs/spread.md")
            }),
        "{shown}"
    );
    for (id, checkout) in ids.iter().zip(&checkouts) {
        let row = shown
            .lines()
            .find(|line| line.contains(&run_shown(&env, id)))
            .unwrap_or_else(|| panic!("{shown}"));
        let name = checkout.file_name().unwrap().to_str().unwrap();
        assert!(
            row.contains("child") && row.contains("active") && row.contains(name),
            "{shown}"
        );
    }

    // A second advance rejoins the reserved children and starts nothing, as does an extend.
    success(command(&env, &["workflow", "advance", &parent]));
    assert_eq!(live_homes(&env), 3);
    success(command(
        &env,
        &["workflow", "extend", &parent, "--deadline", "1h"],
    ));
    success(command(&env, &["workflow", "advance", &parent]));
    assert_eq!(live_homes(&env), 3);

    // One child ended: its ending is stored and the parent keeps waiting.
    fs::write(&child_marker, "").unwrap();
    rewrite(&env, &ids[0], |record| {
        record["recheck_at"] = json!("2000-01-01T00:00:00Z");
    });
    success(command(&env, &["workflow", "advance", &ids[0]]));
    let value = until(&env, &parent, |value| {
        !value["record"]["attempt"]["runs"]["runs"][0]["ended"].is_null()
    });
    assert_eq!(
        (&value["record"]["state"], &value["record"]["node"]),
        (&json!("active"), &json!("spread")),
        "{value:#}"
    );
    assert_eq!(show(&env, &ids[0])["record"]["loop_report"], "sent");

    rewrite(&env, &ids[1], |record| {
        record["recheck_at"] = json!("2000-01-01T00:00:00Z");
    });
    success(command(&env, &["workflow", "advance", &ids[1]]));
    let value = settled(&env, &parent, "done");
    let joined = value["record"]["joined"].as_array().unwrap();
    assert_eq!(joined.len(), 2, "{value:#}");
    for ((run, id), checkout) in joined.iter().zip(&ids).zip(&checkouts) {
        assert_eq!(
            (
                &run["id"],
                &run["workflow"],
                &run["ended"]["ending"]["kind"]
            ),
            (&json!(id), &json!("child"), &json!("done")),
            "{run:#}"
        );
        assert_eq!(run["ended"]["checkout"], json!(checkout), "{run:#}");
        assert!(run["ended"]["branch"].is_string(), "{run:#}");
    }
    let context = context(&value);
    assert_eq!(context["joined"], value["record"]["joined"], "{context:#}");
}

#[test]
fn a_runs_step_fails_naming_a_canceled_child_only_once_every_child_ended() {
    let env = Env::new();
    env.record(&env.project_root);
    let child_marker = deploy(&env, "child");
    let gate = deploy_fan(&env, TWO_CHILDREN);
    commit_project(&env);
    let parent = started(run(&env, "fan", &[], &[]));
    until(&env, &parent, |value| value["record"]["wait"].is_object());
    let ids = children(&open_gate(&env, &parent, &gate));

    success(command(&env, &["workflow", "cancel", &ids[0]]));
    let value = until(&env, &parent, |value| {
        !value["record"]["attempt"]["runs"]["runs"][0]["ended"].is_null()
    });
    assert_eq!(value["record"]["state"], "active", "{value:#}");
    assert_eq!(
        value["record"]["attempt"]["runs"]["runs"][0]["ended"]["ending"]["kind"],
        "canceled"
    );

    until(&env, &ids[1], |value| value["record"]["wait"].is_object());
    fs::write(&child_marker, "").unwrap();
    rewrite(&env, &ids[1], |record| {
        record["recheck_at"] = json!("2000-01-01T00:00:00Z");
    });
    success(command(&env, &["workflow", "advance", &ids[1]]));
    let value = settled(&env, &parent, "failed");
    let route = events(&value, "route")
        .into_iter()
        .find(|route| route["input"]["cause"] == "died")
        .unwrap_or_else(|| panic!("the step failed died: {value:#}"));
    assert_eq!(
        route["input"]["reason"],
        json!(format!("started run {} (child) ended canceled", ids[0])),
        "{route:#}"
    );
    assert_eq!(
        route["input"]["detail"]["runs"],
        json!([{"id":ids[0], "workflow":"child", "ending":"canceled"}]),
        "{route:#}"
    );
}

#[test]
fn a_runs_step_timeout_and_cancel_leave_its_children_running() {
    let env = Env::new();
    env.record(&env.project_root);
    deploy(&env, "child");
    let gate = deploy_fan(
        &env,
        "start = [{ workflow = 'child' }, { workflow = 'child' }]\nthen = 'after'\ntimeout = '1h'",
    );
    commit_project(&env);
    let parent = started(run(&env, "fan", &[], &[]));
    until(&env, &parent, |value| value["record"]["wait"].is_object());
    let ids = children(&open_gate(&env, &parent, &gate));
    rewrite(&env, &parent, |record| {
        record["attempt"]["started_at"] = json!("1970-01-01T00:00:00Z");
    });
    success(command(&env, &["workflow", "advance", &parent]));
    let value = settled(&env, &parent, "failed");
    assert!(
        events(&value, "route")
            .iter()
            .any(|route| route["input"]["cause"] == "timeout"),
        "{value:#}"
    );
    for id in &ids {
        assert_eq!(show(&env, id)["record"]["state"], "active");
    }

    let gate = deploy_fan(&env, TWO_CHILDREN);
    let parent = started(run(&env, "fan", &[], &[]));
    until(&env, &parent, |value| value["record"]["wait"].is_object());
    let more = children(&open_gate(&env, &parent, &gate));
    let receipt =
        String::from_utf8(command(&env, &["workflow", "cancel", &parent]).stdout).unwrap();
    for id in &more {
        assert!(receipt.contains(&run_shown(&env, id)), "{receipt}");
        assert_eq!(show(&env, id)["record"]["state"], "active");
    }
}

#[test]
fn a_runs_step_cancel_leaves_a_started_run_whose_record_cannot_be_read_out_of_its_receipt() {
    let env = Env::new();
    env.record(&env.project_root);
    deploy(&env, "child");
    let gate = deploy_fan(&env, TWO_CHILDREN);
    commit_project(&env);
    let parent = started(run(&env, "fan", &[], &[]));
    until(&env, &parent, |value| value["record"]["wait"].is_object());
    let ids = children(&open_gate(&env, &parent, &gate));
    until(&env, &ids[0], |value| value["record"]["wait"].is_object());
    // A directory at the record's path fails its read for every uid, root included.
    let record = run_home(&env, &ids[0]).record;
    fs::remove_file(&record).unwrap();
    fs::create_dir(&record).unwrap();
    let receipt = success(command(&env, &["workflow", "cancel", &parent]));
    let left = receipt
        .lines()
        .filter(|line| line.contains("left running"))
        .collect::<Vec<_>>();
    assert_eq!(
        left,
        [format!(
            "its started runs were left running: {}",
            run_shown(&env, &ids[1])
        )],
        "{receipt}"
    );
}

#[test]
fn validate_warns_about_runs_entries_a_start_would_refuse_and_a_workflow_that_starts_itself() {
    let env = Env::new();
    let whole = env.rimz_home().join("workflows/whole");
    fs::create_dir_all(whole.join("checks")).unwrap();
    fs::write(
        whole.join("workflow.toml"),
        format!("[workflow]\nscope = 'project'\nstart = 'pr'\n{WAIT}"),
    )
    .unwrap();
    fs::write(whole.join("checks/pr.sh"), PENDING).unwrap();
    deploy_fan(
        &env,
        "start = [{ workflow = 'missing' }, { workflow = 'whole' }, { workflow = 'fan' }]\nthen = 'after'",
    );
    let output = env
        .rimz()
        .args(["workflow", "validate", "fan", "--json"])
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)));
    assert_eq!(report["errors"], json!([]), "{report:#}");
    assert_eq!(
        report["warnings"],
        json!([
            {"path":"runs.spread.start.0.workflow", "message":"runs.spread.start.0.workflow: missing is not a deployed workflow; the step fails when it starts it"},
            {"path":"runs.spread.start.1.workflow", "message":"runs.spread.start.1.workflow: whole has scope = \"project\" and gets no worktree of its own; the step fails when it starts it"},
            {"path":"workflow", "message":"workflow: fan starts itself again through runs steps; each start runs one level deeper until [agents] max-workflow-depth refuses it"},
        ]),
        "{report:#}"
    );
    let table = success(command(&env, &["workflow", "validate", "fan"]));
    let row = table
        .lines()
        .find(|line| line.split_whitespace().nth(1) == Some("spread"))
        .unwrap_or_else(|| panic!("{table}"));
    assert_eq!(
        row.split_whitespace().take(4).collect::<Vec<_>>(),
        ["fan", "spread", "runs", "workflow"],
        "{table}"
    );
    // A link to the workflow starts itself through the name its target carries.
    std::os::unix::fs::symlink(
        env.rimz_home().join("workflows/fan"),
        env.rimz_home().join("workflows/fan-link"),
    )
    .unwrap();
    let output = env
        .rimz()
        .args(["workflow", "validate", "fan-link", "--json"])
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)));
    assert_eq!(
        report["warnings"][2]["message"],
        "workflow: fan-link starts itself again through runs steps; each start runs one level deeper until [agents] max-workflow-depth refuses it",
        "{report:#}"
    );
}

#[test]
fn a_runs_step_refuses_before_any_child_on_a_dirty_checkout_depth_and_unstartable_workflows() {
    let env = Env::new();
    env.record(&env.project_root);
    deploy(&env, "child");
    let project = env.rimz_home().join("workflows/whole");
    fs::create_dir_all(project.join("checks")).unwrap();
    fs::write(
        project.join("workflow.toml"),
        format!("[workflow]\nscope = 'project'\nstart = 'pr'\n{WAIT}"),
    )
    .unwrap();
    fs::write(project.join("checks/pr.sh"), PENDING).unwrap();
    commit_project(&env);
    let state = env.state_path_for(&env.project_root);
    let one = "start = [{ workflow = 'child' }]\nthen = 'after'";
    let cases = [
        (one, "dirty", "has uncommitted changes".to_owned()),
        (one, "deep", "would start at workflow depth 2".to_owned()),
        (
            "start = [{ workflow = 'child' }, { workflow = 'missing' }]\nthen = 'after'",
            "",
            "missing is not a deployed workflow".to_owned(),
        ),
        (
            "start = [{ workflow = 'whole' }]\nthen = 'after'",
            "",
            "whole has scope = \"project\" and gets no worktree of its own".to_owned(),
        ),
        (one, "unreadable", "cannot create a worktree: ".to_owned()),
    ];
    for (spread, setup, reason) in cases {
        let gate = deploy_fan(&env, spread);
        let parent = started(run(&env, "fan", &[], &[]));
        until(&env, &parent, |value| value["record"]["wait"].is_object());
        let untracked = env.project_root.join("untracked.txt");
        match setup {
            "dirty" => fs::write(&untracked, "new").unwrap(),
            "deep" => fs::write(
                env.rimz_home().join("config.toml"),
                "[agents]\nmax-workflow-depth = 1\n",
            )
            .unwrap(),
            "unreadable" => fs::write(env.rimz_home().join("config.toml"), "[agents\n").unwrap(),
            _ => {}
        }
        open_gate(&env, &parent, &gate);
        let value = settled(&env, &parent, "failed");
        let route = events(&value, "route")
            .into_iter()
            .find(|route| route["input"]["cause"] == "died")
            .unwrap_or_else(|| panic!("{setup}: the step failed died: {value:#}"));
        assert!(
            route["input"]["reason"].as_str().unwrap().contains(&reason),
            "{reason}: {route:#}"
        );
        assert!(
            rimz::store::workflow::live_runs(&state).unwrap().is_empty(),
            "{reason}"
        );
        assert!(
            rimz::worktree::discover_owned(&env.project_root)
                .unwrap()
                .is_empty(),
            "{reason}"
        );
        let _ = fs::remove_file(&untracked);
        let _ = fs::remove_file(env.rimz_home().join("config.toml"));
    }
}

/// Opens the fan's gate and spawns the advance into its 'spread' step, which holds at
/// `socket` once each child's worktree exists, before the child's run is published.
/// Sets `RIMZ_BIN=/bin/true` so each published child's detached advance is a no-op: no
/// background child advance can collect a canceled child or spawn a stray parent advance.
fn advance_held(env: &Env, id: &str, marker: &Path, socket: &Path) -> Child {
    fs::write(marker, "").unwrap();
    rewrite(env, id, |record| {
        record["recheck_at"] = json!("2000-01-01T00:00:00Z");
    });
    env.rimz()
        .args(["workflow", "advance", id])
        .env("RIMZ_TEST_WORKFLOW_RUNS_GATE", socket)
        .env("RIMZ_BIN", "/bin/true")
        .spawn()
        .unwrap()
}

/// Releases every hold the advance reaches until it exits.
fn release_until_exit(listener: &UnixListener, advance: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while advance.try_wait().unwrap().is_none() {
        if let Ok((mut stream, _)) = listener.accept() {
            stream.write_all(&[1]).unwrap();
        }
        assert!(Instant::now() < deadline, "the held advance never exited");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The fan's started runs that exist in the live home, in their reserved order.
fn live_children(env: &Env, ids: &[String]) -> Vec<String> {
    let state = env.state_path_for(&env.project_root);
    let live = rimz::store::workflow::live_runs(&state).unwrap();
    ids.iter()
        .filter(|id| live.iter().any(|record| record.id.to_string() == **id))
        .cloned()
        .collect()
}

fn owned_worktrees(env: &Env) -> usize {
    rimz::worktree::discover_owned(&env.project_root)
        .unwrap()
        .len()
}

/// Starts the fan given its 'spread' step and holds its advance at the second child's
/// worktree, the first child already started with a no-op detached advance (`RIMZ_BIN=/bin/true`
/// on `advance_held`), so only the test's own commands can collect it or start a parent advance.
/// Returns the advance, the gate listener, the held connection and the reserved ids.
fn held_at_second_child(
    env: &Env,
    spread: &str,
) -> (
    Child,
    UnixListener,
    std::os::unix::net::UnixStream,
    Vec<String>,
) {
    deploy(env, "child");
    let gate = deploy_fan(env, spread);
    commit_project(env);
    let parent = started(run(env, "fan", &[], &[]));
    until(env, &parent, |value| value["record"]["wait"].is_object());
    let socket = env.runtime_root.join("runs-gate.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let advance = advance_held(env, &parent, &gate, &socket);
    accept_gate(&listener).write_all(&[1]).unwrap();
    let held = accept_gate(&listener);
    let ids = children(&show(env, &parent));
    (
        advance,
        listener,
        held,
        vec![parent].into_iter().chain(ids).collect(),
    )
}

#[test]
fn a_cancel_while_a_runs_step_starts_its_children_stops_the_start_and_names_only_started_ones() {
    let env = Env::new();
    env.record(&env.project_root);
    let (mut advance, listener, mut held, ids) = held_at_second_child(&env, TWO_CHILDREN);
    let (parent, ids) = (&ids[0], &ids[1..]);
    let receipt = success(command(&env, &["workflow", "cancel", parent]));
    held.write_all(&[1]).unwrap();
    release_until_exit(&listener, &mut advance);
    assert_eq!(live_children(&env, ids), [ids[0].clone()]);
    assert_eq!(
        owned_worktrees(&env),
        1,
        "the unstarted child's worktree is removed"
    );
    let announced: Vec<Value> = env
        .read_events()
        .into_iter()
        .map(|event| serde_json::from_str::<Value>(event.params.get()).unwrap())
        .filter(|event| event["name"] == "worktree.created")
        .collect();
    let started = PathBuf::from(
        show(&env, &ids[0])["record"]["scope"]["path"]
            .as_str()
            .unwrap(),
    );
    assert_eq!(
        announced
            .iter()
            .map(|event| event["payload"]["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [started.file_name().unwrap().to_str().unwrap()],
        "only the started child's worktree is announced: {announced:#?}"
    );
    let left = receipt
        .lines()
        .filter(|line| line.contains("left running"))
        .collect::<Vec<_>>();
    assert_eq!(
        left,
        [format!(
            "its started runs were left running: {}",
            run_shown(&env, &ids[0])
        )],
        "{receipt}"
    );
}

#[test]
fn a_runs_step_timeout_that_passes_while_it_starts_its_children_routes_as_the_timeout() {
    let env = Env::new();
    env.record(&env.project_root);
    let spread = format!("{TWO_CHILDREN}\ntimeout = '1h'");
    let (mut advance, listener, mut held, ids) = held_at_second_child(&env, &spread);
    let (parent, ids) = (&ids[0], &ids[1..]);
    rewrite(&env, parent, |record| {
        record["attempt"]["started_at"] = json!("1970-01-01T00:00:00Z");
    });
    held.write_all(&[1]).unwrap();
    release_until_exit(&listener, &mut advance);
    let value = settled(&env, parent, "failed");
    let causes = events(&value, "route")
        .iter()
        .map(|route| route["input"]["cause"].clone())
        .filter(|cause| !cause.is_null())
        .collect::<Vec<_>>();
    assert_eq!(causes, [json!("timeout")], "{value:#}");
    assert_eq!(live_children(&env, ids), [ids[0].clone()]);
    assert_eq!(owned_worktrees(&env), 1);
}

#[test]
fn a_runs_step_whose_every_ending_is_stored_follows_then_past_its_timeout() {
    let env = Env::new();
    env.record(&env.project_root);
    deploy(&env, "child");
    let gate = deploy_fan(&env, &format!("{TWO_CHILDREN}\ntimeout = '1h'"));
    commit_project(&env);
    let parent = started(run(&env, "fan", &[], &[]));
    until(&env, &parent, |value| value["record"]["wait"].is_object());
    children(&open_gate(&env, &parent, &gate));
    rewrite(&env, &parent, |record| {
        let attempt = &mut record["attempt"];
        attempt["started_at"] = json!("1970-01-01T00:00:00Z");
        for run in attempt["runs"]["runs"].as_array_mut().unwrap() {
            run["ended"] = json!({"ending": {"kind": "done"}});
        }
    });
    success(command(&env, &["workflow", "advance", &parent]));
    settled(&env, &parent, "done");
}

/// Kills the advance held at the second child, leaving a reserved child unstarted.
fn kill_held(mut advance: Child, held: std::os::unix::net::UnixStream) {
    advance.kill().unwrap();
    advance.wait().unwrap();
    drop(held);
}

#[test]
fn a_takeover_starts_only_the_reserved_runs_never_created_and_never_one_that_ended() {
    let env = Env::new();
    env.record(&env.project_root);
    let (advance, _listener, held, ids) = held_at_second_child(&env, TWO_CHILDREN);
    let (parent, ids) = (&ids[0], &ids[1..]);
    kill_held(advance, held);
    // The first child ends and reports; its finished home is collected before the takeover.
    let canceled = env
        .rimz()
        .args(["workflow", "cancel", &ids[0]])
        .env("RIMZ_BIN", "/bin/true")
        .output()
        .unwrap();
    success(canceled);
    until(&env, parent, |value| {
        !value["record"]["attempt"]["runs"]["runs"][0]["ended"].is_null()
    });
    let state = env.state_path_for(&env.project_root);
    let finished = state.finished_workflow(&ids[0].parse().unwrap()).home;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !finished.exists() {
        assert!(Instant::now() < deadline, "{} never moved", ids[0]);
        std::thread::sleep(Duration::from_millis(10));
    }
    fs::remove_dir_all(&finished).unwrap();

    success(command(&env, &["workflow", "advance", parent]));
    assert_eq!(live_children(&env, ids), [ids[1].clone()]);
    let value = show(&env, parent);
    assert_eq!(
        (&value["record"]["state"], &value["record"]["node"]),
        (&json!("active"), &json!("spread")),
        "{value:#}"
    );
}

#[test]
fn a_takeover_fails_the_step_when_its_start_list_no_longer_matches_the_reserved_runs() {
    let env = Env::new();
    env.record(&env.project_root);
    let (advance, _listener, held, ids) = held_at_second_child(&env, TWO_CHILDREN);
    let (parent, ids) = (&ids[0], &ids[1..]);
    kill_held(advance, held);
    deploy_fan(&env, "start = [{ workflow = 'child' }]\nthen = 'after'");
    success(command(&env, &["workflow", "advance", parent]));
    let value = settled(&env, parent, "failed");
    let route = events(&value, "route")
        .into_iter()
        .find(|route| route["input"]["cause"] == "died")
        .unwrap_or_else(|| panic!("the step failed died: {value:#}"));
    assert_eq!(
        route["input"]["reason"], "spread no longer lists the runs it reserved (child, child)",
        "{route:#}"
    );
    assert_eq!(live_children(&env, ids), [ids[0].clone()]);
}

/// Copies deployed workflow `name` to `dir`.
fn copy_workflow(env: &Env, name: &str, dir: &Path) {
    let from = env.rimz_home().join("workflows").join(name);
    fs::create_dir_all(dir.join("checks")).unwrap();
    for file in ["workflow.toml", "checks/pr.sh"] {
        fs::copy(from.join(file), dir.join(file)).unwrap();
    }
}

/// Installs a linked pack `crew` whose workflow `packed` is a copy of deployed workflow
/// `name`, and returns the directory a run of it records.
fn install_packed(env: &Env, name: &str) -> PathBuf {
    let source = env.home_root.join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join("rimz-packs.toml"),
        "[[pack]]\nname = 'crew'\ndescription = 'Crew'\npath = '.'\n",
    )
    .unwrap();
    copy_workflow(env, name, &source.join("workflows/packed"));
    rimz::packs::install(&env.agents_home(), &source).unwrap();
    env.agents_home().join("packs/source/workflows/packed")
}

#[test]
fn a_runs_step_starts_pack_workflows_and_home_names_linked_from_elsewhere() {
    let env = Env::new();
    env.record(&env.project_root);
    deploy(&env, "child");
    let packed = install_packed(&env, "child");
    // A home name whose directory is a link to elsewhere is the home's workflow.
    let outside = env.home_root.join("elsewhere/linked");
    copy_workflow(&env, "child", &outside);
    let linked = env.rimz_home().join("workflows/linked");
    std::os::unix::fs::symlink(&outside, &linked).unwrap();
    let gate = deploy_fan(
        &env,
        "start = [{ workflow = 'packed' }, { workflow = 'linked' }]\nthen = 'after'",
    );
    commit_project(&env);
    let parent = started(run(&env, "fan", &[], &[]));
    until(&env, &parent, |value| value["record"]["wait"].is_object());
    let ids = children(&open_gate(&env, &parent, &gate));
    let started: Vec<_> = ids
        .iter()
        .map(|id| {
            let record =
                until(&env, id, |value| value["record"]["wait"].is_object())["record"].clone();
            (record["workflow"].clone(), record["spec_dir"].clone())
        })
        .collect();
    assert_eq!(
        started,
        [
            (json!("packed"), json!(packed)),
            (json!("linked"), json!(linked))
        ]
    );
}

#[test]
fn an_agent_starts_home_and_pack_workflows_but_never_a_loose_directory() {
    let env = Env::new();
    env.record(&env.project_root);
    seed_agent(&env, "planner");
    deploy(&env, "owed");
    deploy(&env, "plain");
    install_packed(&env, "owed");
    let loose = env.home_root.join("elsewhere/loose");
    copy_workflow(&env, "owed", &loose);
    let refusal = format!(
        "error: {}: an agent starts only a workflow in {} or an installed pack; run this \
         directory from your own shell, or move it there or install its pack",
        loose.display(),
        env.rimz_home().join("workflows").display()
    );
    let refused = |vars: &[(&str, &str)]| {
        let homes = live_homes(&env);
        let output = run(&env, loose.to_str().unwrap(), &[], vars);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(String::from_utf8_lossy(&output.stderr).trim(), refusal);
        assert_eq!(live_homes(&env), homes, "the refusal created no home");
    };
    refused(&AGENT);
    // Setting the hosted variable outside any run does not make an agent a person.
    refused(&[AGENT[0], AGENT[1], ("RIMZ_WORKFLOW_NODE", "pr")]);
    // A loop task's start takes only a deployed workflow, whoever calls.
    let homes = live_homes(&env);
    let fire = rimz::harness::workflow::TaskStart {
        task: "t".into(),
        loop_fire: rimz::ids::FireId::new(&rimz::ids::TaskId::new()),
        fire: rimz::store::workflow::RunFire::Manual {
            task: "t".into(),
            evidence: None,
            booked_by: vec![],
        },
        fire_facts: json!({}),
        manual: true,
    };
    let output = run(
        &env,
        loose.to_str().unwrap(),
        &["--loop-fire-json", &serde_json::to_string(&fire).unwrap()],
        &[],
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        format!(
            "error: {}: a loop task starts only a workflow in {} or an installed pack",
            loose.display(),
            env.rimz_home().join("workflows").display()
        )
    );
    assert_eq!(live_homes(&env), homes, "the refusal created no home");
    // An agent's start through a link it controls records the home's own path.
    deploy(&env, "aliased");
    let alias = env.home_root.join("elsewhere/alias");
    std::os::unix::fs::symlink(env.rimz_home().join("workflows/aliased"), &alias).unwrap();
    let (_, value) = pending(&env, alias.to_str().unwrap(), &AGENT);
    assert_eq!(
        value["record"]["spec_dir"],
        json!(env.rimz_home().join("workflows/aliased"))
    );
    // A directory nested in the home is the home's, as an agent's start.
    let nested = env.rimz_home().join("workflows/group/nested");
    copy_workflow(&env, "owed", &nested);
    let (_, value) = pending(&env, nested.to_str().unwrap(), &AGENT);
    assert_eq!(value["record"]["spec_dir"], json!(nested));
    let (_, value) = pending(&env, "packed", &AGENT);
    assert_eq!(
        value["record"]["starter"],
        json!({"by":"agent", "kind":"claude", "launch":"launch_planner"})
    );
    let (owed, _) = pending(&env, "owed", &AGENT);
    // A script of a run an agent started starts as that run, under the agent's rule.
    refused(&[("RIMZ_WORKFLOW_RUN", &owed), ("RIMZ_WORKFLOW_NODE", "pr")]);
    // A script of a run a person started is the person's.
    let (plain, _) = pending(&env, "plain", &[]);
    let output = run(
        &env,
        loose.to_str().unwrap(),
        &["--detach"],
        &[("RIMZ_WORKFLOW_RUN", &plain), ("RIMZ_WORKFLOW_NODE", "pr")],
    );
    let id = started(output);
    assert_eq!(
        show(&env, &id)["record"]["spec_dir"],
        json!(loose),
        "a person's run starts the loose directory"
    );
    // `show` takes the directory by path, with its runs and its node view.
    let shown = success(command(
        &env,
        &["workflow", "show", loose.to_str().unwrap()],
    ));
    assert!(shown.contains("elsewhere/loose/workflow.toml"), "{shown}");
    assert!(
        shown.contains(&format!(
            "one run per worktree · by hand: rimz workflow run {} -w '<worktree>'",
            loose.display()
        )),
        "{shown}"
    );
    assert!(shown.contains("loose · 1 active"), "{shown}");
    let node = success(command(
        &env,
        &["workflow", "show", loose.to_str().unwrap(), "pr"],
    ));
    assert!(node.contains("pr · check in loose"), "{node}");
}
