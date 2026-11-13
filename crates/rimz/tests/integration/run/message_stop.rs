//! A join an agent runs stops when a person or an agent queues a prompt for
//! that agent: exit 75, one stderr line, and nothing awaited is marked.

use super::{create_running_named_run, register_running_wait_agent, write_run_status};
use crate::common::Env;
use crate::common::wait::{agent_wait, register_agent, register_calling_agent};
use jiff::Timestamp;
use rimz::agents::{
    AgentLifecycleObservation, AgentState, AgentStatus, LaunchParams, LifecycleSignal,
};
use rimz::ids::{AgentKind, AgentSessionId, MessageId};
use rimz::store::event::EventEnvelope;
use rimz::store::message::{
    AfterCondition, DeliveryGate, HarnessNotice, MessageBody, MessageRecord, MessageSender,
    MessageStatus, WhenCondition,
};
use rimz::store::run::RunStatus;

const FROM_USER: &str = "rimz: stopped waiting: 1 message from user arrives when you end your \
                         turn; what you waited on is still running\n";

fn card(env: &Env, name: &str) -> AgentState {
    env.store()
        .snapshot_cached()
        .expect("read snapshot")
        .agents
        .iter()
        .find(|agent| agent.name.as_deref() == Some(name))
        .cloned()
        .unwrap_or_else(|| panic!("no card named {name}"))
}

fn prompt_for(env: &Env, name: &str, text: &str) -> MessageRecord {
    MessageRecord::new(
        env.workspace_id.clone(),
        &card(env, name),
        text.to_owned(),
        DeliveryGate::Done,
    )
}

fn queue(env: &Env, message: &MessageRecord) {
    env.store()
        .queue_message(message, "rimz-test")
        .expect("queue message");
}

fn live(env: &Env, id: &MessageId) -> MessageRecord {
    env.store()
        .list_messages()
        .expect("list messages")
        .into_iter()
        .find(|message| &message.message_id == id)
        .unwrap_or_else(|| panic!("{id} left the live queue"))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn assert_running(env: &Env, run_id: &rimz::RunId) {
    let record = rimz::harness::run::load(env.store().paths(), run_id).expect("load run");
    assert_eq!(record.status, RunStatus::Running);
    assert_eq!(record.joined_at, None, "a message stop joins nothing");
}

fn end_session(env: &Env, kind: &str, session_id: &str) {
    let workspace = env.resolve_workspace(&env.project_root);
    env.store()
        .append_event(&EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            workspace.session_name,
            kind,
            "SessionEnd",
            &AgentLifecycleObservation::new(
                Some(AgentSessionId::from(session_id)),
                LifecycleSignal::Ended,
            ),
        ))
        .expect("end session");
}

#[test]
fn an_agents_join_stops_on_a_prompt_for_its_caller_and_announces_it_once() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let otter = create_running_named_run(&env, &env.store(), "swift-otter");
    let message = prompt_for(&env, "planner", "the secret follow-up");
    queue(&env, &message);

    let out = agent_wait(&env)
        .args(["agents", "wait", "swift-otter", "--timeout", "30s"])
        .output()
        .expect("run agents wait");

    assert_eq!(out.status.code(), Some(75), "{out:?}");
    assert_eq!(text(&out.stdout), "");
    assert_eq!(
        text(&out.stderr),
        format!("--- swift-otter (running) ---\n{FROM_USER}")
    );
    let stamped = live(&env, &message.message_id);
    assert_eq!(stamped.status, MessageStatus::Queued);
    let announced_at = stamped.announced_at.expect("the stop stamps announced_at");
    assert_eq!(stamped.updated_at, message.updated_at);
    assert_running(&env, &otter.run_id);

    let again = agent_wait(&env)
        .args(["agents", "wait", "swift-otter", "--timeout", "0s"])
        .output()
        .expect("rerun agents wait");
    assert_eq!(again.status.code(), Some(124), "{again:?}");
    assert_eq!(text(&again.stderr), "--- swift-otter (timed out) ---\n");
    assert_eq!(
        live(&env, &message.message_id).announced_at,
        Some(announced_at)
    );
}

#[test]
fn a_prompt_queued_mid_wait_stops_the_join_well_inside_its_timeout() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let otter = create_running_named_run(&env, &env.store(), "swift-otter");
    let started = std::time::Instant::now();
    let wait = agent_wait(&env)
        .args(["agents", "wait", "swift-otter", "--timeout", "60s"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn agents wait");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let message = prompt_for(&env, "planner", "arrives mid-wait");
    queue(&env, &message);

    let out = wait.wait_with_output().expect("agents wait exits");
    assert_eq!(out.status.code(), Some(75), "{out:?}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        text(&out.stderr),
        format!("--- swift-otter (running) ---\n{FROM_USER}")
    );
    assert!(live(&env, &message.message_id).announced_at.is_some());
    assert_running(&env, &otter.run_id);
}

#[test]
fn json_joins_report_unfinished_targets_running_with_exit_75() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let store = env.store();
    let mut otter = create_running_named_run(&env, &store, "swift-otter");
    let fox = create_running_named_run(&env, &store, "quiet-fox");
    write_run_status(&store, &mut otter, RunStatus::Completed);
    queue(&env, &prompt_for(&env, "planner", "first"));

    let out = agent_wait(&env)
        .args(["agents", "wait", "swift-otter", "quiet-fox", "--json"])
        .args(["--timeout", "30s"])
        .output()
        .expect("run plural JSON join");
    assert_eq!(out.status.code(), Some(75), "{out:?}");
    let map: serde_json::Value = serde_json::from_slice(&out.stdout).expect("result map");
    assert_eq!(map["swift-otter"]["status"], "completed");
    assert_eq!(map["swift-otter"]["exit"], 0);
    assert_eq!(map["quiet-fox"]["status"], "running");
    assert_eq!(map["quiet-fox"]["exit"], 75);
    assert_eq!(text(&out.stderr), FROM_USER);
    assert_running(&env, &fox.run_id);

    // A deadline and a message in the same poll: the message wins.
    queue(&env, &prompt_for(&env, "planner", "second"));
    let out = agent_wait(&env)
        .args(["agents", "wait", "quiet-fox", "--json", "--timeout", "0s"])
        .output()
        .expect("run single JSON join");
    assert_eq!(out.status.code(), Some(75), "{out:?}");
    let map: serde_json::Value = serde_json::from_slice(&out.stdout).expect("result map");
    assert_eq!(map["quiet-fox"]["status"], "running");
    assert_eq!(map["quiet-fox"]["exit"], 75);
    assert_running(&env, &fox.run_id);
}

#[test]
fn a_result_settled_in_the_poll_of_a_message_exits_by_its_own_rule() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let store = env.store();
    let mut otter = create_running_named_run(&env, &store, "swift-otter");
    otter.last_message = Some("otter answer\n".to_owned());
    write_run_status(&store, &mut otter, RunStatus::Completed);
    let message = prompt_for(&env, "planner", "while you wait");
    queue(&env, &message);

    let out = agent_wait(&env)
        .args(["agents", "wait", "swift-otter", "--timeout", "30s"])
        .output()
        .expect("run agents wait");

    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(text(&out.stdout), "otter answer\n");
    assert_eq!(live(&env, &message.message_id).announced_at, None);
}

#[test]
fn a_person_or_an_ended_caller_never_stops_or_stamps() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    create_running_named_run(&env, &env.store(), "swift-otter");
    let message = prompt_for(&env, "planner", "for the planner");
    queue(&env, &message);

    let person = env
        .rimz()
        .args(["agents", "wait", "swift-otter", "--timeout", "0s"])
        .output()
        .expect("run a person's wait");
    assert_eq!(person.status.code(), Some(124), "{person:?}");
    assert_eq!(live(&env, &message.message_id).announced_at, None);

    end_session(&env, "claude", "provider-session");
    let ended = agent_wait(&env)
        .args(["agents", "wait", "swift-otter", "--timeout", "0s"])
        .output()
        .expect("run an ended caller's wait");
    assert_eq!(ended.status.code(), Some(124), "{ended:?}");
    assert_eq!(live(&env, &message.message_id).announced_at, None);
}

#[test]
fn records_that_open_no_conversation_turn_never_stop_a_wait() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    register_agent(
        &env,
        "other-session",
        "other-launch",
        "other",
        LaunchParams::default(),
    );
    create_running_named_run(&env, &env.store(), "swift-otter");
    let store = env.store();
    let planner = card(&env, "planner");
    let unlatched = AfterCondition {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: AgentSessionId::from("other-session"),
        agent_name: Some("other".to_owned()),
        address: "@other".to_owned(),
        met_at: None,
    };
    let cases = [
        (
            "harness wake",
            prompt_for(&env, "planner", "wake").with_sender(MessageSender::Harness {
                notice: HarnessNotice::Wait,
            }),
        ),
        (
            "stage notice",
            prompt_for(&env, "planner", "stage").with_sender(MessageSender::Harness {
                notice: HarnessNotice::Stage,
            }),
        ),
        (
            "system",
            prompt_for(&env, "planner", "no from").with_sender(MessageSender::System),
        ),
        (
            "command",
            prompt_for(&env, "planner", "/compact").with_body(MessageBody::Command),
        ),
        (
            "scheduled",
            prompt_for(&env, "planner", "later")
                .with_not_before(Some(Timestamp::now() + jiff::SignedDuration::from_hours(1))),
        ),
        (
            "unlatched after",
            prompt_for(&env, "planner", "after other").with_after(vec![unlatched]),
        ),
        (
            "unlatched when",
            prompt_for(&env, "planner", "when other idles").with_when(vec![WhenCondition {
                kind: AgentKind::new_unchecked("claude"),
                agent_id: AgentSessionId::from("other-session"),
                agent_name: Some("other".to_owned()),
                address: "@other".to_owned(),
                status: AgentStatus::Idle,
                dwell_secs: 0,
                met_at: None,
            }]),
        ),
        ("other card", prompt_for(&env, "other", "not yours")),
    ];
    for (_, message) in &cases {
        queue(&env, message);
    }
    let steered = prompt_for(&env, "planner", "steered");
    queue(&env, &steered);
    store
        .claim_message_for_steer(&steered.message_id, Timestamp::now())
        .expect("claim steer")
        .expect("steer claimed");
    let canceled = prompt_for(&env, "planner", "withdrawn");
    queue(&env, &canceled);
    assert!(
        store
            .cancel_message(&canceled.message_id, "rimz-test", "test")
            .expect("cancel")
    );
    assert_eq!(planner.ended_at, None);

    let out = agent_wait(&env)
        .args(["agents", "wait", "swift-otter", "--timeout", "0s"])
        .output()
        .expect("run agents wait");
    assert_eq!(out.status.code(), Some(124), "{out:?}");
    for (label, message) in &cases {
        assert_eq!(
            live(&env, &message.message_id).announced_at,
            None,
            "{label} was announced"
        );
    }
    assert_eq!(live(&env, &steered.message_id).announced_at, None);
}

#[test]
fn two_agents_waiting_on_each_other_both_stop_on_each_others_message() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    register_agent(
        &env,
        "reviewer-session",
        "reviewer-launch",
        "reviewer",
        LaunchParams::default(),
    );
    let sender = |name: &str, session: &str| MessageSender::Agent {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: Some(AgentSessionId::from(session)),
        name: Some(name.to_owned()),
        profile: None,
        role: None,
        channel: None,
    };
    queue(
        &env,
        &prompt_for(&env, "planner", "from reviewer")
            .with_sender(sender("reviewer", "reviewer-session")),
    );
    queue(
        &env,
        &prompt_for(&env, "reviewer", "from planner")
            .with_sender(sender("planner", "provider-session")),
    );

    let planner = agent_wait(&env)
        .args(["agents", "wait", "@reviewer", "--timeout", "30s"])
        .output()
        .expect("planner waits on reviewer");
    let reviewer = env
        .rimz()
        .args(["agents", "wait", "@planner", "--timeout", "30s"])
        .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
        .env(rimz::harness::launch::ENV_AGENT_ID, "reviewer-launch")
        .env(rimz::harness::launch::ENV_AGENT_NAME, "reviewer")
        .output()
        .expect("reviewer waits on planner");

    for (out, from) in [(&planner, "@reviewer"), (&reviewer, "@planner")] {
        assert_eq!(out.status.code(), Some(75), "{out:?}");
        let stderr = text(&out.stderr);
        assert!(
            stderr.ends_with(&format!(
                "rimz: stopped waiting: 1 message from {from} arrives when you end your \
                 turn; what you waited on is still running\n"
            )),
            "{stderr}"
        );
        assert!(!stderr.contains("from planner") && !stderr.contains("from reviewer"));
    }
}

#[test]
fn a_run_stream_stops_on_a_message_without_an_end_or_a_timeout_line() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let otter = create_running_named_run(&env, &env.store(), "swift-otter");

    queue(&env, &prompt_for(&env, "planner", "stream text"));
    let out = agent_wait(&env)
        .args(["agents", "wait", otter.run_id.as_str(), "--stream"])
        .args(["--timeout", "30s"])
        .output()
        .expect("run text stream");
    assert_eq!(out.status.code(), Some(75), "{out:?}");
    let stderr = text(&out.stderr);
    assert!(stderr.ends_with(FROM_USER), "{stderr}");
    assert!(!stderr.contains("timed out"), "{stderr}");

    queue(&env, &prompt_for(&env, "planner", "ndjson text"));
    let out = agent_wait(&env)
        .args([
            "agents",
            "wait",
            otter.run_id.as_str(),
            "--stream",
            "--json",
        ])
        .args(["--timeout", "30s"])
        .output()
        .expect("run NDJSON stream");
    assert_eq!(out.status.code(), Some(75), "{out:?}");
    assert!(!text(&out.stdout).contains("\"end\""), "{out:?}");
    assert!(text(&out.stderr).ends_with(FROM_USER), "{out:?}");
    assert_running(&env, &otter.run_id);
}

#[test]
fn an_interactive_agent_stream_stops_on_a_message() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    register_running_wait_agent(&env, &env.store(), "swift-otter", "sess-otter");
    queue(&env, &prompt_for(&env, "planner", "stream text"));

    let out = agent_wait(&env)
        .args([
            "agents",
            "wait",
            "swift-otter",
            "--stream",
            "--timeout",
            "30s",
        ])
        .output()
        .expect("run interactive stream");
    assert_eq!(out.status.code(), Some(75), "{out:?}");
    let stderr = text(&out.stderr);
    assert!(stderr.ends_with(FROM_USER), "{stderr}");
    assert!(!stderr.contains("timed out"), "{stderr}");
}

#[test]
fn a_subagents_join_stops_on_a_prompt_for_the_parent() {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let (mut child, parent_kind, parent_launch) = super::create_finished_subagent(&env, &store);
    child.completed_at = None;
    write_run_status(&store, &mut child, RunStatus::Running);
    let message = prompt_for(&env, "parent", "parent follow-up");
    queue(&env, &message);

    let out = env
        .rimz()
        .args(["subagents", "wait", "quiet-fox", "--timeout", "30s"])
        .env(rimz::harness::launch::ENV_AGENT_KIND, parent_kind.as_str())
        .env(rimz::harness::launch::ENV_AGENT_ID, parent_launch.as_str())
        .output()
        .expect("run subagents wait");

    assert_eq!(out.status.code(), Some(75), "{out:?}");
    assert!(text(&out.stderr).ends_with(FROM_USER), "{out:?}");
    assert!(live(&env, &message.message_id).announced_at.is_some());
    assert_running(&env, &child.run_id);
}
