//! An `agent` call's launch driven through its provider's hooks: a `stay: false` run that
//! settles at its turn end and reports, the answer its `schema` asks for handed in with
//! `rimz submit` from the agent's pane, and the stop at its call's `timeout`.

use super::*;
use crate::common::wait::register_calling_agent;
use rimz::store::call::{CallRecord, Seat};
use rimz::store::message::{HarnessNotice, MessageSender};

const TASK_SESSION: &str = "task-session";
const TASK_LAUNCH: &str = "task-launch";
const LAUNCHER_LAUNCH: &str = "launch-session";

/// A `stay: false` run of `@tasker`, launched by `@planner` and bound to its pane's session,
/// with the call record `call` when the call published one.
fn task_run(env: &Env, call: Option<CallRecord>) -> RunRecord {
    task_run_in(env, call, None)
}

/// [`task_run`] in Zellij terminal `pane`, owned by process `pid`, so a stop can end it.
fn task_run_in(env: &Env, call: Option<CallRecord>, owner: Option<(u64, u32)>) -> RunRecord {
    seed_run(env, call, owner, |record| record.self_cleanup = true)
}

/// The first turn of `@tasker` as a staying peer of `@planner`, its turn record open.
fn peer_turn_in(env: &Env, call: Option<CallRecord>, owner: Option<(u64, u32)>) -> RunRecord {
    seed_run(env, call, owner, |record| {
        record.peer = Some(rimz::store::run::PeerRun {
            launch_id: AgentSessionId::from(TASK_LAUNCH),
            opened_by: Vec::new(),
        });
    })
}

fn seed_run(
    env: &Env,
    call: Option<CallRecord>,
    owner: Option<(u64, u32)>,
    shape: impl FnOnce(&mut RunRecord),
) -> RunRecord {
    let store = env.store();
    let workspace = env.resolve_workspace(&env.project_root);
    let kind = AgentKind::new_unchecked("codex");
    let mut record = RunRecord::new(
        workspace.workspace_id.clone(),
        kind.clone(),
        PermissionMode::Auto,
        "audit the parser".to_owned(),
        env.project_root.clone(),
    );
    record.agent_name = Some("tasker".to_owned());
    record.agent_id = Some(AgentSessionId::from(TASK_SESSION));
    record.launch_id = Some(AgentSessionId::from(TASK_LAUNCH));
    record.reader = Some("planner".to_owned());
    shape(&mut record);
    let peer = record.peer.is_some();
    rimz::harness::run::create(store.paths(), &record).expect("create the task's run");
    store
        .append_event(&EventEnvelope::agent_launched(
            workspace.workspace_id.clone(),
            &workspace.session_name,
            &kind,
            AgentLaunchPayload {
                system_prompt_digest: None,
                agent_id: AgentSessionId::from(TASK_SESSION),
                launch_id: Some(AgentSessionId::from(TASK_LAUNCH)),
                agent_name: "tasker".to_owned(),
                agent_name_explicit: true,
                launch: LaunchParams {
                    launched_by: Some(Box::new(rimz::agents::LaunchedBy {
                        kind: AgentKind::new_unchecked("claude"),
                        agent_id: AgentSessionId::from(LAUNCHER_LAUNCH),
                    })),
                    ..LaunchParams::default()
                },
                state: AgentLaunchState::Bound,
                run_id: (!peer).then(|| record.run_id.clone()),
                pane_id: owner.map(|(pane, _)| {
                    PaneId::from_parts(MuxName::Zellij, format!("terminal_{pane}"))
                }),
                runtime_owner: owner.map(|(_, pid)| {
                    rimz::store::runtime::process_owner(
                        rimz::pane::RuntimeOwnerKind::Agent,
                        TASK_SESSION,
                        pid,
                    )
                }),
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: None,
                prompt: Some(record.prompt.clone()),
                description: None,
            },
        ))
        .expect("seed the task's agent");
    if let Some(call) = call {
        rimz::store::call::create(store.paths(), &call).expect("publish the call record");
    }
    record
}

fn call(schema: Option<serde_json::Value>, deadline: Option<(Timestamp, Duration)>) -> CallRecord {
    let created_at = deadline.map_or_else(Timestamp::now, |(created, _)| created);
    CallRecord {
        launch_id: AgentSessionId::from(TASK_LAUNCH),
        seats: vec![Seat {
            kind: AgentKind::new_unchecked("codex"),
            launch_id: AgentSessionId::from(TASK_LAUNCH),
        }],
        stay: false,
        schema: schema.and_then(|schema| schema.as_object().cloned()),
        created_at,
        deadline: deadline.map(|(created, timeout)| created + timeout),
        answer: None,
        asked: None,
        timed_out_at: None,
    }
}

/// The task's provider firing its `event` hook, `extra` merged into the payload.
fn hook(env: &Env, record: &RunRecord, event: &str, extra: serde_json::Value) {
    hook_with_owner(env, record, None, event, extra);
}

/// Like [`hook`], stamping `owner` as `RIMZ_AGENT_PID` in place of [`Env::agent_owner_pid`] so
/// the row keeps the seat's own [`provider`] process.
fn hook_as(env: &Env, record: &RunRecord, owner: u32, event: &str, extra: serde_json::Value) {
    hook_with_owner(env, record, Some(owner), event, extra);
}

fn hook_with_owner(
    env: &Env,
    record: &RunRecord,
    owner: Option<u32>,
    event: &str,
    extra: serde_json::Value,
) {
    let mut payload = json!({
        "hook_event_name": event,
        "session_id": TASK_SESSION,
        "prompt": "audit the parser",
    });
    payload
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().cloned().unwrap_or_default());
    let mut command = env.hook_command("codex");
    if let Some(pid) = owner {
        command.env("RIMZ_AGENT_PID", pid.to_string());
    }
    command.env(rimz::harness::launch::ENV_RUN_ID, record.run_id.as_str());
    let output = env
        .spawn_payload(command, &payload.to_string())
        .wait_with_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    env.drain_hooks();
}

fn turn(env: &Env, record: &RunRecord, last: &str) {
    turn_with_owner(env, record, None, last);
}

fn turn_as(env: &Env, record: &RunRecord, owner: u32, last: &str) {
    turn_with_owner(env, record, Some(owner), last);
}

fn turn_with_owner(env: &Env, record: &RunRecord, owner: Option<u32>, last: &str) {
    hook_with_owner(env, record, owner, "UserPromptSubmit", json!({}));
    hook_with_owner(
        env,
        record,
        owner,
        "Stop",
        json!({"last_assistant_message": last}),
    );
}

/// `rimz submit` run from the task's own pane.
fn submit(env: &Env, answer: &str) -> std::process::Output {
    env.rimz()
        .args(["submit", answer])
        .env(rimz::harness::launch::ENV_AGENT_KIND, "codex")
        .env(rimz::harness::launch::ENV_AGENT_ID, TASK_LAUNCH)
        .output()
        .expect("run rimz submit")
}

/// The launcher's fleet digest, as the helper a settled run spawns queues it.
fn digest(env: &Env) -> Vec<rimz::store::message::MessageRecord> {
    let request = json!({"workspace_id": env.workspace_id, "parent_agent_id": LAUNCHER_LAUNCH});
    let output = env
        .rimz()
        .args([
            "agents",
            "subagent-digest",
            "--request",
            &request.to_string(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    // A report the helper's own digest queued may already have moved to history.
    let (queued, history) = env.store().list_messages_and_history().unwrap();
    queued
        .into_iter()
        .chain(history)
        .filter(|message| {
            message.sender
                == MessageSender::Harness {
                    notice: HarnessNotice::AgentReport,
                }
        })
        .collect()
}

fn owed_notices(env: &Env) -> Vec<rimz::store::message::MessageRecord> {
    env.store()
        .list_messages()
        .unwrap()
        .into_iter()
        .filter(|message| {
            message.sender == MessageSender::System && message.text.contains("rimz submit")
        })
        .collect()
}

/// The host's `call-timeout` helper for launch `launch`, on the tracing Zellij shim listing
/// terminals `panes`, each running its provider.
fn call_timeout(env: &Env, launch: &str, panes: &[(u64, &str)]) -> std::process::Output {
    let workspace = env.resolve_workspace(&env.project_root);
    let listing = env.project_root.join("seat-panes.json");
    let rows: Vec<serde_json::Value> = panes
        .iter()
        .map(|(id, command)| json!({"id": id, "is_plugin": false, "tab_id": 1, "title": "sh", "pane_command": command}))
        .collect();
    std::fs::write(&listing, serde_json::to_vec(&rows).unwrap()).unwrap();
    let request = json!({"workspace_id": env.workspace_id, "launch_id": launch});
    env.rimz()
        .args(["agents", "call-timeout", "--request", &request.to_string()])
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env(
            "RIMZ_TEST_ZELLIJ_LOG",
            env.project_root.join("seat-trace.log"),
        )
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES_FILE", &listing)
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{} [Created 1s ago]\n", workspace.session_name),
        )
        .env("RIMZ_TEST_STOP_OWNER_EXIT_MS", "2000")
        .output()
        .unwrap()
}

/// A process standing in for a seat's provider, which a stop ends when hooks stamp its pid as
/// `RIMZ_AGENT_PID`.
fn provider() -> std::process::Child {
    use crate::common::ScrubSessionEnvExt;
    Command::new("sleep")
        .scrub_session_env()
        .arg("30")
        .spawn()
        .unwrap()
}

/// The launcher `@planner`'s `join` on `id`.
fn join(env: &Env, id: &str) -> serde_json::Value {
    let mut command = crate::common::wait::agent_wait(env);
    command.args(["mcp", "serve"]);
    let mut server = crate::mcp_server::McpServer::spawn(command);
    server.initialize();
    let (joined, is_error) = server.answer(1, "join", json!({"id": id, "timeout": "1s"}));
    assert!(!is_error, "{joined}");
    joined
}

/// Whether a row named `name` is live in a fresh projection: the cached `latest.json` keeps an
/// owner that died after its last publish until the next append, and a stop need not append.
fn live(env: &Env, name: &str) -> bool {
    env.store()
        .snapshot()
        .unwrap()
        .agents
        .iter()
        .any(|agent| agent.name.as_deref() == Some(name) && agent.ended_at.is_none())
}

fn room() -> Env {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    env
}

/// A `stay: false` run settles at its first turn end with that turn's last message, which its
/// launcher's `AGENT_REPORT` points to, so the wrapper's exit policy closes its pane.
#[test]
fn a_one_task_run_settles_at_its_turn_end_and_reports_its_last_message() {
    let env = room();
    let record = task_run(&env, None);
    turn(&env, &record, "the parser is sound");
    let settled = rimz::harness::run::load(env.store().paths(), &record.run_id).unwrap();
    assert_eq!(settled.status, RunStatus::Completed, "{settled:?}");
    assert_eq!(settled.last_message.as_deref(), Some("the parser is sound"));
    let reports = digest(&env);
    let [report] = reports.as_slice() else {
        panic!("one report: {reports:?}")
    };
    let response = rimz::harness::run::response_path(env.store().paths(), &settled).unwrap();
    assert!(
        report.text.contains(&response.display().to_string()),
        "{}",
        report.text
    );
    assert_eq!(
        std::fs::read_to_string(&response).unwrap(),
        "the parser is sound\n"
    );
    let joined = join(&env, "agent:tasker");
    assert_eq!(joined["status"], json!("done"), "{joined}");
    assert_eq!(joined["result"], json!("the parser is sound"), "{joined}");
}

/// A kept `-p --keep` run that completed leaves its agent working on later turns: a `join` on the
/// agent waits for the live turn, never answering the finished run.
#[test]
fn a_join_on_an_agent_past_its_kept_run_waits_for_its_live_turn() {
    let env = room();
    let record = seed_run(&env, None, None, |record| record.keep = true);
    turn(&env, &record, "the kept run's answer");
    let settled = rimz::harness::run::load(env.store().paths(), &record.run_id).unwrap();
    assert_eq!(settled.status, RunStatus::Completed, "{settled:?}");
    hook(
        &env,
        &record,
        "UserPromptSubmit",
        json!({"prompt": "a later task"}),
    );
    let joined = join(&env, "agent:tasker");
    assert_eq!(joined["status"], json!("running"), "{joined}");
}

/// A `schema` run whose turn ends with no answer stays open, owing it, and is told so once; a
/// misfit `rimz submit` is refused with nothing stored; the turn that hands in a fitting answer
/// settles the run with that answer as its response, in the report and in `join`'s record.
#[test]
fn a_schema_run_owes_its_answer_until_a_turn_hands_one_in() {
    let env = room();
    let schema = json!({"type": "object", "required": ["verdict"]});
    let record = task_run(&env, Some(call(Some(schema), None)));

    turn(&env, &record, "it looks fine");
    let owing = rimz::harness::run::load(env.store().paths(), &record.run_id).unwrap();
    assert!(!owing.status.is_terminal(), "{owing:?}");
    assert!(
        owing.parked_at.is_some(),
        "the turn end parks on the answer: {owing:?}"
    );
    turn(&env, &record, "still fine");
    let notices = owed_notices(&env);
    assert_eq!(notices.len(), 1, "one notice: {notices:?}");
    assert!(
        notices[0]
            .text
            .starts_with("Your launcher has no answer for this task yet"),
        "{}",
        notices[0].text
    );
    assert!(digest(&env).is_empty(), "an owing run sends no report");

    hook(&env, &record, "UserPromptSubmit", json!({}));
    let misfit = submit(&env, r#"{"note": 1}"#);
    assert_eq!(misfit.status.code(), Some(1), "{misfit:?}");
    let stderr = String::from_utf8_lossy(&misfit.stderr);
    assert!(
        stderr.contains(
            "the answer does not fit its schema: `answer` lacks the required `verdict`; \
             nothing was stored"
        ),
        "{stderr}"
    );
    let stored = rimz::store::call::load(env.store().paths(), &AgentSessionId::from(TASK_LAUNCH))
        .unwrap()
        .unwrap();
    assert!(stored.answer.is_none(), "{stored:?}");

    let fits = submit(&env, r#"{"verdict": "sound"}"#);
    assert!(fits.status.success(), "{fits:?}");
    hook(
        &env,
        &record,
        "Stop",
        json!({"last_assistant_message": "handed in"}),
    );
    let settled = rimz::harness::run::load(env.store().paths(), &record.run_id).unwrap();
    assert_eq!(settled.status, RunStatus::Completed, "{settled:?}");
    let answer = "{\n  \"verdict\": \"sound\"\n}";
    assert_eq!(settled.last_message.as_deref(), Some(answer));
    let reports = digest(&env);
    assert_eq!(reports.len(), 1, "{reports:?}");
    let response = rimz::harness::run::response_path(env.store().paths(), &settled).unwrap();
    assert_eq!(
        std::fs::read_to_string(&response).unwrap(),
        format!("{answer}\n")
    );
    let joined = join(&env, "agent:tasker");
    assert_eq!(joined["result"], json!(answer), "{joined}");
}

/// An owed-answer notice whose id was stored but never queued (a failed agent lookup, a queue
/// error, or a stop between the two writes) is queued under that id at the next turn end, once.
#[test]
fn an_owed_notice_lost_after_its_id_was_stored_is_queued_again() {
    let env = room();
    let schema = json!({"type": "object", "required": ["verdict"]});
    let record = task_run(&env, Some(call(Some(schema), None)));
    let lost = rimz::ids::MessageId::new();
    rimz::store::call::update(
        env.store().paths(),
        &AgentSessionId::from(TASK_LAUNCH),
        |call| {
            call.asked = Some(lost.clone());
            Some(())
        },
    )
    .unwrap();
    turn(&env, &record, "it looks fine");
    turn(&env, &record, "still fine");
    let notices = owed_notices(&env);
    assert_eq!(
        notices
            .iter()
            .map(|notice| &notice.message_id)
            .collect::<Vec<_>>(),
        [&lost],
        "{notices:?}"
    );
}

/// A `schema` run whose turn ends parked on a watch of its own, its answer still owed, is woken
/// by the watch, not told about the answer: the notice goes only to a park on the answer itself.
#[test]
fn a_run_parked_on_its_own_watch_gets_no_owed_notice() {
    let env = room();
    let schema = json!({"type": "object", "required": ["verdict"]});
    let record = task_run(&env, Some(call(Some(schema), None)));
    let path = crate::common::wait::loop_instances_path(&env);
    let watch = json!({"watch-own": {
        "root": env.project_root,
        "when": ["pr=merged"],
        "wait": {"kind": "codex", "session": TASK_LAUNCH, "handle": "@tasker"},
        "wait-meta": {"armed_at": Timestamp::now()},
    }});
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec(&watch).unwrap()).unwrap();
    turn(&env, &record, "waiting on the merge");
    let parked = rimz::harness::run::load(env.store().paths(), &record.run_id).unwrap();
    assert!(parked.parked_at.is_some(), "{parked:?}");
    assert!(owed_notices(&env).is_empty(), "{:?}", owed_notices(&env));
}

/// `rimz submit` from a launch given no `schema` is refused.
#[test]
fn a_submit_without_a_schema_is_refused() {
    let env = room();
    task_run(
        &env,
        Some(call(
            None,
            Some((Timestamp::now(), Duration::from_secs(600))),
        )),
    );
    let refused = submit(&env, r#"{"verdict": "sound"}"#);
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("this launch was given no `schema`, so it takes no answer"),
        "{stderr}"
    );
}

/// The call's deadline settles the open turn timed out and stops the agent: its launcher's one
/// `AGENT_REPORT` and `join` name the timeout, and the claim is taken once.
#[test]
fn the_call_timeout_stops_the_agent_and_reports_once() {
    let env = room();
    let mut process = provider();
    let created = Timestamp::now() - Duration::from_secs(31 * 60);
    let record = task_run_in(
        &env,
        Some(call(None, Some((created, Duration::from_secs(30 * 60))))),
        Some((7, process.id())),
    );
    hook_as(&env, &record, process.id(), "UserPromptSubmit", json!({}));
    let output = call_timeout(&env, TASK_LAUNCH, &[(7, "codex")]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        process.try_wait().unwrap().is_some(),
        "the stop ends the agent's own stand-in"
    );
    let settled = rimz::harness::run::load(env.store().paths(), &record.run_id).unwrap();
    assert_eq!(settled.status, RunStatus::TimedOut, "{settled:?}");
    assert_eq!(
        settled.failure_tail.as_deref(),
        Some("its 30m timeout stopped it")
    );
    assert!(
        !live(&env, "tasker"),
        "the agent is stopped: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let claimed = rimz::store::call::load(env.store().paths(), &AgentSessionId::from(TASK_LAUNCH))
        .unwrap()
        .unwrap();
    assert!(claimed.timed_out_at.is_some(), "{claimed:?}");
    let reports = digest(&env);
    let [report] = reports.as_slice() else {
        panic!("one report: {reports:?}")
    };
    assert!(report.text.contains("timed out"), "{}", report.text);
    let joined = join(&env, "agent:tasker");
    assert_eq!(joined["status"], json!("done"), "{joined}");
    assert_eq!(
        joined["detail"],
        json!("timed_out: its 30m timeout stopped it"),
        "{joined}"
    );
    let reported = rimz::harness::run::load(env.store().paths(), &record.run_id).unwrap();

    let again = call_timeout(&env, TASK_LAUNCH, &[]);
    assert!(again.status.success(), "{again:?}");
    assert_eq!(digest(&env).len(), 1, "a second pass reports nothing");
    assert_eq!(
        rimz::harness::run::load(env.store().paths(), &record.run_id).unwrap(),
        reported
    );
    let _ = process.kill();
    let _ = process.wait();
}

/// A staying agent whose deadline passes while it rests after a completed turn reads timed out
/// to `join`, `agents show`, and `agents wait`, not as that turn: the timed-out record is stamped
/// at the stop, so it sorts newest, and it is the outcome a join hands its claim.
#[test]
fn a_resting_agents_timeout_is_its_newest_run() {
    let env = room();
    let mut process = provider();
    let created = Timestamp::now() - Duration::from_secs(31 * 60);
    let mut staying = call(None, Some((created, Duration::from_secs(30 * 60))));
    staying.stay = true;
    let first = peer_turn_in(&env, Some(staying), Some((7, process.id())));
    turn_as(&env, &first, process.id(), "the first turn's answer");
    let completed = rimz::harness::run::load(env.store().paths(), &first.run_id).unwrap();
    assert_eq!(completed.status, RunStatus::Completed, "{completed:?}");

    let output = call_timeout(&env, TASK_LAUNCH, &[(7, "codex")]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        process.try_wait().unwrap().is_some(),
        "the stop ends the agent's own stand-in"
    );
    assert!(
        !live(&env, "tasker"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let runs = rimz::harness::run::list(env.store().paths()).unwrap();
    let timed_out = runs
        .iter()
        .find(|run| run.status == RunStatus::TimedOut)
        .unwrap_or_else(|| panic!("a timed-out record: {runs:?}"));
    assert!(timed_out.started_at > completed.started_at, "{runs:?}");

    let as_launcher = |args: &[&str]| {
        let output = env
            .rimz()
            .args(["agents"])
            .args(args)
            .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
            .env(rimz::harness::launch::ENV_AGENT_ID, LAUNCHER_LAUNCH)
            .output()
            .unwrap();
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    };
    let shown = as_launcher(&["show", "tasker"]);
    let run = shown
        .split_once("\nRun\n")
        .and_then(|(_, rest)| rest.split_once("\n\n"))
        .map_or("", |(run, _)| run);
    assert!(
        run.contains(timed_out.run_id.as_str()) && run.contains("timed out"),
        "{shown}"
    );
    let waited = as_launcher(&["wait", "tasker", "--timeout", "1s"]);
    assert!(waited.contains("timed out"), "{waited}");
    assert!(!waited.contains("the first turn's answer"), "{waited}");

    let joined = join(&env, "agent:tasker");
    assert_eq!(
        joined["detail"],
        json!("timed_out: its 30m timeout stopped it"),
        "{joined}"
    );
    let _ = process.kill();
    let _ = process.wait();
}

/// A `stay: false` run that completes after its deadline's claim has reported its own end: the
/// stop writes no timed-out record and sends no second report.
#[test]
fn a_one_task_run_that_completes_at_its_deadline_reports_once() {
    let env = room();
    let mut process = provider();
    let created = Timestamp::now() - Duration::from_secs(31 * 60);
    let record = task_run_in(
        &env,
        Some(call(None, Some((created, Duration::from_secs(30 * 60))))),
        Some((7, process.id())),
    );
    turn_as(&env, &record, process.id(), "done just in time");
    let output = call_timeout(&env, TASK_LAUNCH, &[(7, "codex")]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        process.try_wait().unwrap().is_some(),
        "the stop ends the agent's own stand-in"
    );
    let runs = rimz::harness::run::list(env.store().paths()).unwrap();
    assert!(
        runs.iter().all(|run| run.status == RunStatus::Completed),
        "{runs:?}"
    );
    assert_eq!(digest(&env).len(), 1, "one report");
    let _ = process.kill();
    let _ = process.wait();
}

/// A launch with no live seat left at its deadline is owed nothing: its open run stays as it
/// was and no report goes.
#[test]
fn a_launch_ended_before_its_deadline_is_sent_nothing() {
    let env = room();
    let created = Timestamp::now() - Duration::from_secs(31 * 60);
    let record = task_run(
        &env,
        Some(call(None, Some((created, Duration::from_secs(30 * 60))))),
    );
    hook(&env, &record, "UserPromptSubmit", json!({}));
    seat_ended(&env, "codex", TASK_SESSION);
    assert!(!live(&env, "tasker"));
    let output = call_timeout(&env, TASK_LAUNCH, &[]);
    assert!(output.status.success(), "{output:?}");
    let after = rimz::harness::run::load(env.store().paths(), &record.run_id).unwrap();
    assert_ne!(after.status, RunStatus::TimedOut, "{after:?}");
    assert!(digest(&env).is_empty(), "no report");
}

/// A `team` call of `@planner`'s whose launch opened `lead` and `coder` on `forge#x`, its
/// deadline an hour past, with seats `seats` (handle and Zellij terminal) each running a
/// provider stand-in, and the leader's team run.
fn team_call(env: &Env, seats: &[(&str, u64)]) -> (Vec<std::process::Child>, RunRecord) {
    let workspace = env.resolve_workspace(&env.project_root);
    let seat = |(name, pane): (&str, u64)| {
        let session = format!("{name}-session");
        let process = provider();
        let launch = LaunchParams {
            team: Some("forge".to_owned()),
            channel: Some("x".to_owned()),
            lane: Some(crate::common::wait::stamped_lane(
                env,
                &env.project_root,
                "x",
            )),
            launched_by: Some(Box::new(rimz::agents::LaunchedBy {
                kind: AgentKind::new_unchecked("claude"),
                agent_id: AgentSessionId::from(LAUNCHER_LAUNCH),
            })),
            ..LaunchParams::default()
        };
        env.store()
            .append_event(&EventEnvelope::agent_launched(
                workspace.workspace_id.clone(),
                &workspace.session_name,
                &AgentKind::new_unchecked("claude"),
                AgentLaunchPayload {
                    system_prompt_digest: None,
                    agent_id: AgentSessionId::from(session.as_str()),
                    launch_id: Some(AgentSessionId::from(format!("{name}-launch"))),
                    agent_name: name.to_owned(),
                    agent_name_explicit: true,
                    launch,
                    state: AgentLaunchState::Bound,
                    run_id: None,
                    pane_id: Some(PaneId::from_parts(
                        MuxName::Zellij,
                        format!("terminal_{pane}"),
                    )),
                    runtime_owner: Some(rimz::store::runtime::process_owner(
                        rimz::pane::RuntimeOwnerKind::Agent,
                        &session,
                        process.id(),
                    )),
                    worktree_path: Some(env.project_root.display().to_string()),
                    worktree_branch: None,
                    prompt: None,
                    description: None,
                },
            ))
            .unwrap();
        process
    };
    let processes = seats.iter().copied().map(seat).collect();
    let store = env.store();
    let mut record = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "ship the parser".to_owned(),
        env.project_root.clone(),
    );
    record.team = Some(rimz::store::run::TeamRun {
        launch_id: AgentSessionId::from("lead-launch"),
        instance: "forge#x".parse().unwrap(),
    });
    record.agent_name = Some("lead".to_owned());
    rimz::harness::run::create(store.paths(), &record).unwrap();
    let created = Timestamp::now() - Duration::from_secs(3 * 60 * 60);
    let seats = ["lead", "coder"].map(|name| Seat {
        kind: AgentKind::new_unchecked("claude"),
        launch_id: AgentSessionId::from(format!("{name}-launch")),
    });
    rimz::store::call::create(
        store.paths(),
        &CallRecord {
            launch_id: AgentSessionId::from("lead-launch"),
            seats: seats.to_vec(),
            stay: true,
            schema: None,
            created_at: created,
            deadline: Some(created + Duration::from_secs(2 * 60 * 60)),
            answer: None,
            asked: None,
            timed_out_at: None,
        },
    )
    .unwrap();

    (processes, record)
}

/// Seat `session`'s `kind` provider ending.
fn seat_ended(env: &Env, kind: &str, session: &str) {
    let workspace = env.resolve_workspace(&env.project_root);
    let ended =
        AgentLifecycleObservation::new(Some(AgentSessionId::from(session)), LifecycleSignal::Ended);
    env.store()
        .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
            session_name: &workspace.session_name,
            agent_kind: AgentKind::new_unchecked(kind),
            event_name: "SessionEnd",
            observation: &ended,
            spawned_subagents: &[],
        })
        .unwrap();
}

/// A team call's deadline stops every live seat its launch opened, with one `TEAM_REPORT` that
/// names the timeout, and leaves a later launch's seat on the same lane alone.
#[test]
fn the_call_timeout_stops_the_launchs_seats_and_reports_the_team_once() {
    let env = room();
    let (mut processes, record) = team_call(&env, &[("lead", 4), ("coder", 5), ("late", 6)]);
    let store = env.store();
    let helper = || {
        call_timeout(
            &env,
            "lead-launch",
            &[(4, "claude"), (5, "claude"), (6, "claude")],
        )
    };
    let output = helper();
    assert!(output.status.success(), "{output:?}");
    let settled = rimz::harness::run::load(store.paths(), &record.run_id).unwrap();
    assert_eq!(
        settled.status,
        RunStatus::TimedOut,
        "{}\n{:?}",
        String::from_utf8_lossy(&output.stderr),
        store.snapshot_cached().unwrap().agents
    );
    assert_eq!(
        settled.failure_tail.as_deref(),
        Some("its 2h timeout stopped it")
    );
    let reports: Vec<_> = store
        .list_messages()
        .unwrap()
        .into_iter()
        .filter(|message| matches!(message.sender, MessageSender::Harness { .. }))
        .collect();
    let [report] = reports.as_slice() else {
        panic!("one team report: {reports:?}")
    };
    assert!(
        report.text.contains("timed out: its 2h timeout stopped it"),
        "{}",
        report.text
    );
    assert!(
        !live(&env, "lead") && !live(&env, "coder"),
        "the launch's seats are stopped: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        live(&env, "late"),
        "a later launch's seat on the lane is left alone"
    );
    // With the later launch's seat gone too, the cohort's join reads the timed-out run.
    seat_ended(&env, "claude", "late-session");
    let joined = join(&env, "team:forge#x");
    assert_eq!(joined["status"], json!("done"), "{joined}");
    assert_eq!(
        joined["detail"],
        json!("timed_out: its timeout stopped the team before its board reached Done"),
        "{joined}"
    );

    let again = helper();
    assert!(again.status.success(), "{again:?}");
    assert_eq!(
        store
            .list_messages()
            .unwrap()
            .iter()
            .filter(|message| matches!(message.sender, MessageSender::Harness { .. }))
            .count(),
        1,
        "a second pass reports nothing"
    );
    for process in &mut processes {
        let _ = process.kill();
        let _ = process.wait();
    }
}

/// An `agent` call whose profile seats a team, given a `schema`: the answer its leader hands in is
/// the team run's response, though the leader's turn ends on other text.
#[test]
fn a_team_seating_calls_handed_in_answer_is_its_response() {
    let env = room();
    let (mut processes, record) = team_call(&env, &[("lead", 4), ("coder", 5)]);
    let store = env.store();
    let schema = json!({"type": "object", "required": ["verdict"]});
    rimz::store::call::update(
        store.paths(),
        &AgentSessionId::from("lead-launch"),
        |call| {
            call.schema = schema.as_object().cloned();
            Some(())
        },
    )
    .unwrap();
    let lead_hook = |event: &str, extra: serde_json::Value| {
        let mut payload = json!({"hook_event_name": event, "session_id": "lead-session", "prompt": "ship the parser"});
        payload
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().cloned().unwrap_or_default());
        let mut command = env.hook_command("claude");
        command.env("RIMZ_AGENT_PID", processes[0].id().to_string());
        command.env(rimz::harness::launch::ENV_RUN_ID, record.run_id.as_str());
        let output = env
            .spawn_payload(command, &payload.to_string())
            .wait_with_output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        env.drain_hooks();
    };
    lead_hook("UserPromptSubmit", json!({}));
    let submitted = env
        .rimz()
        .args(["submit", r#"{"verdict": "shipped"}"#])
        .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
        .env(rimz::harness::launch::ENV_AGENT_ID, "lead-launch")
        .output()
        .unwrap();
    assert!(submitted.status.success(), "{submitted:?}");
    lead_hook(
        "Stop",
        json!({"last_assistant_message": "the team is on it"}),
    );
    let run = rimz::harness::run::load(store.paths(), &record.run_id).unwrap();
    assert_eq!(
        run.last_message.as_deref(),
        Some("{\n  \"verdict\": \"shipped\"\n}"),
        "{run:?}"
    );
    for process in &mut processes {
        let _ = process.kill();
        let _ = process.wait();
    }
}

/// A `join` already waiting on a team when its call's timeout settles the team answers timed out
/// once the cohort dissolves.
#[test]
fn a_join_waiting_on_a_team_answers_its_timeout() {
    let env = room();
    let (mut processes, _) = team_call(&env, &[("lead", 4), ("coder", 5)]);
    let joined = std::thread::scope(|scope| {
        let waiting = scope.spawn(|| {
            let mut command = crate::common::wait::agent_wait(&env);
            command.args(["mcp", "serve"]);
            let mut server = crate::mcp_server::McpServer::spawn(command);
            server.initialize();
            server.answer(1, "join", json!({"id": "team:forge#x", "timeout": "30s"}))
        });
        // The join reads the cohort live before the deadline's stop ends it.
        std::thread::sleep(Duration::from_secs(2));
        let output = call_timeout(&env, "lead-launch", &[(4, "claude"), (5, "claude")]);
        assert!(output.status.success(), "{output:?}");
        // The cohort dissolves: its seats' ends are recorded here, as the stop's are in
        // `the_call_timeout_stops_the_launchs_seats_and_reports_the_team_once`.
        seat_ended(&env, "claude", "lead-session");
        seat_ended(&env, "claude", "coder-session");
        waiting.join().unwrap()
    });
    let (joined, is_error) = joined;
    assert!(!is_error, "{joined}");
    assert_eq!(joined["status"], json!("done"), "{joined}");
    assert_eq!(
        joined["detail"],
        json!("timed_out: its timeout stopped the team before its board reached Done"),
        "{joined}"
    );
    for process in &mut processes {
        let _ = process.kill();
        let _ = process.wait();
    }
}

/// A team whose leader has ended while a worker runs on still gets its timeout: the worker is
/// stopped and the launcher's one `TEAM_REPORT` names the timeout.
#[test]
fn a_team_timeout_with_its_leader_gone_still_stops_and_reports() {
    let env = room();
    let (mut processes, record) = team_call(&env, &[("lead", 4), ("coder", 5)]);
    seat_ended(&env, "claude", "lead-session");
    let output = call_timeout(&env, "lead-launch", &[(5, "claude")]);
    assert!(output.status.success(), "{output:?}");
    let store = env.store();
    let settled = rimz::harness::run::load(store.paths(), &record.run_id).unwrap();
    assert_eq!(settled.status, RunStatus::TimedOut, "{settled:?}");
    let reports: Vec<_> = store
        .list_messages()
        .unwrap()
        .into_iter()
        .filter(|message| matches!(message.sender, MessageSender::Harness { .. }))
        .collect();
    let [report] = reports.as_slice() else {
        panic!("one team report: {reports:?}")
    };
    assert!(
        report.text.contains("timed out: its 2h timeout stopped it"),
        "{}",
        report.text
    );
    assert!(
        !live(&env, "coder"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for process in &mut processes {
        let _ = process.kill();
        let _ = process.wait();
    }
}
