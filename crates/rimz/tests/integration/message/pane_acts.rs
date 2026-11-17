//! Focus, capture, and `pane send` of an agent act on the pane the hosting lookup answers at
//! the point of use, never on the pane stored on the row the command resolved; a pane the
//! user names reads no lookup.

use super::*;

const OLD: &str = "terminal_3";
const NEW: &str = "terminal_4";

/// `@planner`, registered in pane `OLD` under launch `launch-planner`, and an unbound swap
/// reservation of it.
fn planner_with_swap(env: &Env) -> rimz::store::writer::AgentLaunchIdentity {
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let process = dummy_agent_process();
    let owner = agent_owner("sess-old", &process);
    reap_later(process);
    append_lifecycle(
        env,
        "claude",
        "SessionStart",
        "sess-old",
        LifecycleSignal::Registered,
        |observation| {
            observation.agent_name = Some("planner".to_owned());
            observation.launch_id = Some("launch-planner".into());
            observation.pane_id = Some(PaneId::from_parts(MuxName::Zellij, OLD));
            observation.runtime_owner = Some(owner);
        },
    );
    let store = env.store();
    let workspace = env.resolve_workspace(&env.project_root);
    let source = store
        .audit_rows()
        .unwrap()
        .iter()
        .find(|row| row.agent_id == "sess-old")
        .cloned()
        .unwrap();
    store
        .begin_agent_swap(
            &source,
            &rimz::store::writer::AgentLaunchRequest {
                kind: AgentKind::new_unchecked("claude"),
                agent_id: "launch_swap".into(),
                name: rimz::store::writer::AgentLaunchName::Explicit("planner".into()),
                login: rimz::store::writer::LaunchLogin::RoomDefault,
                launch: LaunchParams::default(),
                run_id: None,
                prompt: None,
            },
            rimz::store::writer::AgentLaunchScope {
                session_name: workspace.session_name.clone(),
                cwd: env.project_root.clone(),
                branch: None,
                description: None,
            },
        )
        .unwrap()
        .single_identity()
        .unwrap()
        .clone()
}

/// Bind the swap reservation to `pane`, as its wrapper would: the agent is in `pane` now.
fn bind_swap(env: &Env, reservation: &rimz::store::writer::AgentLaunchIdentity, pane: &str) {
    let workspace = env.resolve_workspace(&env.project_root);
    env.store()
        .bind_agent_launch(
            reservation,
            &workspace.session_name,
            &env.project_root,
            &PaneId::from_parts(MuxName::Zellij, pane),
            None,
        )
        .unwrap();
}

/// A traced `rimz --mux zellij` whose pane listing is the zellij panes `ids`.
fn acting(env: &Env, ids: &[u64]) -> std::process::Command {
    let panes = list_stop_panes(env, ids);
    stop_command(env, &panes, 50)
}

fn trace(env: &Env) -> Vec<String> {
    trace_lines(&env.project_root.join("stop-trace.log"))
}

/// The panes the traced commands focused, in order.
fn focused(env: &Env) -> Vec<String> {
    after_arg(env, "focus-pane-id")
}

/// The distinct panes the traced commands captured; a capture retries an empty dump.
fn captured(env: &Env) -> Vec<String> {
    let mut panes = trace(env)
        .iter()
        .filter(|line| line.contains("\tdump-screen\t"))
        .filter_map(|line| line.split('\t').skip_while(|arg| *arg != "-p").nth(1))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    panes.sort();
    panes.dedup();
    panes
}

fn after_arg(env: &Env, arg: &str) -> Vec<String> {
    trace(env)
        .iter()
        .filter_map(|line| line.split('\t').skip_while(|word| *word != arg).nth(1))
        .map(str::to_owned)
        .collect()
}

/// The lookup's reads: each lists the room's panes.
fn lookup_reads(env: &Env) -> usize {
    trace(env)
        .iter()
        .filter(|line| line.contains("\tlist-panes"))
        .count()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// `@planner`, registered in zellij `pane` with an owner of its own.
fn register_planner(env: &Env, pane: &str) {
    let process = dummy_agent_process();
    let owner = agent_owner("sess-a", &process);
    reap_later(process);
    append_lifecycle(
        env,
        "claude",
        "SessionStart",
        "sess-a",
        LifecycleSignal::Registered,
        |observation| {
            observation.agent_name = Some("planner".to_owned());
            observation.pane_id = Some(PaneId::from_parts(MuxName::Zellij, pane));
            observation.runtime_owner = Some(owner);
        },
    );
}

/// The panes the traced commands typed into.
fn typed(env: &Env) -> Vec<String> {
    after_arg(env, "--pane-id")
}

/// The agent moves to `NEW` after `@planner` resolved to the row whose stored pane is `OLD`
/// and before the command reads the lookup: focus, capture, and send act on `NEW`, the pane
/// the lookup answers.
#[test]
fn focus_capture_and_send_act_on_the_pane_the_lookup_answers() {
    for args in [
        &["agents", "focus", "@planner"][..],
        &["pane", "focus", "@planner"],
        &["agents", "show", "@planner", "--capture"],
        &["pane", "capture", "@planner"],
        &["pane", "send", "@planner", "hi"],
    ] {
        let env = Env::new();
        let reservation = planner_with_swap(&env);
        let hold = DeliveryRendezvous::new(&env, "before-read");
        // The shim prints no screen, so a capture fails after its dump.
        let command = acting(&env, &[3, 4])
            .env("RIMZ_TEST_AGENT_PANE_BEFORE_READ", &hold.path)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(mut held) = hold.arrive_timeout(Duration::from_secs(10)) {
            bind_swap(&env, &reservation, NEW);
            held.write_all(&[0]).unwrap();
        }
        let output = command.wait_with_output().unwrap();
        let acted = if args.contains(&"--capture") || args[1] == "capture" {
            captured(&env)
        } else if args[1] == "send" {
            assert!(output.status.success(), "{args:?}: {output:?}");
            typed(&env)
        } else {
            assert!(output.status.success(), "{args:?}: {output:?}");
            focused(&env)
        };
        assert_eq!(acted, [NEW], "{args:?}: {output:?}\n{:#?}", trace(&env));
    }
}

/// The agent moves to `NEW` after `pane send @planner` locked `OLD` and before its second
/// read: nothing is typed into either pane, and the send fails naming the move.
#[test]
fn send_refuses_when_the_agent_moves_while_its_pane_is_locked() {
    let env = Env::new();
    let reservation = planner_with_swap(&env);
    let hold = DeliveryRendezvous::new(&env, "after-lock");
    let send = acting(&env, &[3, 4])
        .env("RIMZ_TEST_AGENT_PANE_AFTER_WAKE", &hold.path)
        .args(["pane", "send", "@planner", "--enter", "hi"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(mut held) = hold.arrive_timeout(Duration::from_secs(10)) {
        bind_swap(&env, &reservation, NEW);
        held.write_all(&[0]).unwrap();
    }
    let output = send.wait_with_output().unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(
        stderr(&output).contains(&format!(
            "error: agent planner: the agent left pane zellij:{OLD} while it was woken; nothing \
             was sent"
        )),
        "{}",
        stderr(&output)
    );
    assert_eq!(typed(&env), Vec::<String>::new(), "{:#?}", trace(&env));
}

/// `@planner`'s stored pane is no longer listed, so the lookup hosts it nowhere: neither
/// focused, captured, nor typed into, and each command says it has no pane.
#[test]
fn focus_and_capture_refuse_when_the_lookup_hosts_the_agent_nowhere() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_planner(&env, OLD);

    for args in [
        &["agents", "focus", "@planner"][..],
        &["pane", "focus", "@planner"],
        &["agents", "show", "@planner", "--capture"],
        &["pane", "capture", "@planner"],
        &["pane", "send", "@planner", "hi"],
    ] {
        let output = acting(&env, &[4]).args(args).output().unwrap();
        assert!(!output.status.success(), "{args:?}: {output:?}");
        assert!(
            stderr(&output).contains("agent planner has no bound pane"),
            "{args:?}: {}",
            stderr(&output)
        );
    }
    assert_eq!(focused(&env), Vec::<String>::new(), "{:#?}", trace(&env));
    assert_eq!(captured(&env), Vec::<String>::new(), "{:#?}", trace(&env));
    assert_eq!(typed(&env), Vec::<String>::new(), "{:#?}", trace(&env));
}

/// `pane focus @planner --session-name <other>` names a session other than the one the
/// lookup answers for the agent's pane: it refuses and focuses nothing.
#[test]
fn agent_focus_refuses_a_session_name_the_lookup_did_not_answer() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_planner(&env, OLD);
    let session = env.resolve_workspace(&env.project_root).session_name;

    let output = acting(&env, &[3])
        .args(["pane", "focus", "@planner", "--session-name", "elsewhere"])
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(
        stderr(&output).contains(&format!(
            "error: agent planner is in pane zellij:{OLD} of session {session}, not session \
             elsewhere; nothing was focused"
        )),
        "{}",
        stderr(&output)
    );
    assert_eq!(focused(&env), Vec::<String>::new(), "{:#?}", trace(&env));
}

/// An agent that ended is captured by no one, though its stored pane is still listed.
#[test]
fn show_capture_of_an_ended_agent_refuses_while_its_stored_pane_is_listed() {
    let env = Env::new();
    planner_with_swap(&env);
    append_lifecycle(
        &env,
        "claude",
        "SessionEnd",
        "sess-old",
        LifecycleSignal::Ended,
        |_| {},
    );
    let output = acting(&env, &[3])
        .args(["agents", "show", "@sess-old", "--capture"])
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(
        stderr(&output).contains("agent planner has no bound pane"),
        "{}",
        stderr(&output)
    );
    assert_eq!(captured(&env), Vec::<String>::new(), "{:#?}", trace(&env));
}

/// An agent that moves to another pane between the read that found it and the read after
/// its pane was woken is not focused.
#[test]
fn focus_refuses_when_the_agent_moves_while_its_pane_is_woken() {
    for verb in ["agents", "pane"] {
        let env = Env::new();
        let reservation = planner_with_swap(&env);
        let hold = DeliveryRendezvous::new(&env, "after-wake");
        let focus = acting(&env, &[3, 4])
            .env("RIMZ_TEST_AGENT_PANE_AFTER_WAKE", &hold.path)
            .args([verb, "focus", "@planner"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(mut held) = hold.arrive_timeout(Duration::from_secs(10)) {
            bind_swap(&env, &reservation, NEW);
            held.write_all(&[0]).unwrap();
        }
        let output = focus.wait_with_output().unwrap();
        assert_eq!(focused(&env), Vec::<String>::new(), "{verb}: {output:?}");
        assert!(!output.status.success(), "{verb}: {output:?}");
        assert!(
            stderr(&output).contains(&format!(
                "agent planner: the agent left pane zellij:{OLD} while it was woken; nothing \
                 was focused"
            )),
            "{verb}: {}",
            stderr(&output)
        );
    }
}

/// A pane the user names reads no lookup, and is focused, captured, or typed into as named.
#[test]
fn a_pane_the_user_names_reads_no_lookup() {
    let env = Env::new();
    planner_with_swap(&env);
    let pane = format!("zellij:{OLD}");
    let focus = acting(&env, &[3, 4])
        .args(["pane", "focus", &pane])
        .output()
        .unwrap();
    assert!(focus.status.success(), "{focus:?}");
    assert_eq!(focused(&env), [OLD]);
    assert_eq!(lookup_reads(&env), 0, "{:#?}", trace(&env));
    let _ = acting(&env, &[3, 4])
        .args(["pane", "capture", &pane])
        .output()
        .unwrap();
    assert_eq!(captured(&env), [OLD]);
    assert_eq!(
        lookup_reads(&env),
        1,
        "only capture's room membership check lists panes: {:#?}",
        trace(&env)
    );
    let send = acting(&env, &[3, 4])
        .args(["pane", "send", &pane, "hi"])
        .output()
        .unwrap();
    assert!(send.status.success(), "{send:?}");
    assert_eq!(typed(&env), [OLD]);
    assert_eq!(
        lookup_reads(&env),
        2,
        "only send's room membership check lists panes: {:#?}",
        trace(&env)
    );
}

/// `pane send` with nothing to send refuses before it reads the lookup or names a pane.
#[test]
fn an_empty_send_refuses_before_any_lookup_read() {
    let env = Env::new();
    planner_with_swap(&env);
    for target in ["@planner".to_owned(), format!("zellij:{OLD}")] {
        let output = acting(&env, &[3])
            .args(["pane", "send", &target])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{target}: {output:?}");
        assert!(
            stderr(&output).contains("error: expected text, --key, or --enter"),
            "{target}: {}",
            stderr(&output)
        );
    }
    assert_eq!(lookup_reads(&env), 0, "{:#?}", trace(&env));
    assert_eq!(typed(&env), Vec::<String>::new(), "{:#?}", trace(&env));
}

/// An open Claude question on `sess-old`, the session `planner_with_swap` registers.
fn ask_planner(env: &Env) {
    let transcript = env.home_root.join("planner-question.jsonl");
    std::fs::write(&transcript, "").unwrap();
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "sess-old",
        "transcript_path": transcript,
        "tool_name": "AskUserQuestion",
        "tool_input": {
            "questions": [{
                "question": "Choose deployment path?",
                "options": [{ "label": "safe" }, { "label": "fast" }],
                "multiSelect": false
            }]
        }
    });
    let hook = env.run_hook("claude", &payload.to_string());
    assert!(hook.status.success(), "{hook:?}");
}

/// `rimz answer @planner safe`, held at the rendezvous `hold` names while its action runs.
fn answer(env: &Env, ids: &[u64], hold: Option<(&str, &dyn Fn())>) -> std::process::Output {
    let mut command = acting(env, ids);
    command
        .args(["answer", "@planner", "safe", "--no-wait"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let Some((variable, during)) = hold else {
        return command.output().unwrap();
    };
    let rendezvous = DeliveryRendezvous::new(env, "answer-hold");
    let child = command.env(variable, &rendezvous.path).spawn().unwrap();
    if let Some(mut held) = rendezvous.arrive_timeout(Duration::from_secs(10)) {
        during();
        held.write_all(&[0]).unwrap();
    }
    child.wait_with_output().unwrap()
}

/// The handle `answer` names `@planner` by: its card's, which the swap reservation's
/// `planner` makes ambiguous.
const ANSWERED: &str = "@claude#main";

/// `rimz answer` types into the pane the lookup answers once its write lock is held, and
/// exits 2 with no key sent when the agent moved during the lock or no listed pane hosts it.
#[test]
fn answer_types_only_into_the_pane_the_lookup_answers_after_the_lock() {
    let env = Env::new();
    planner_with_swap(&env);
    ask_planner(&env);
    let output = answer(&env, &[3, 4], None);
    assert!(output.status.success(), "{output:?}");
    assert!(!typed(&env).is_empty(), "{:#?}", trace(&env));
    assert!(
        typed(&env).iter().all(|pane| pane == OLD),
        "{:#?}",
        trace(&env)
    );

    let env = Env::new();
    planner_with_swap(&env);
    ask_planner(&env);
    let output = answer(&env, &[4], None);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        stderr(&output).contains(&format!("error: {ANSWERED} has no live bound pane")),
        "{}",
        stderr(&output)
    );
    assert_eq!(typed(&env), Vec::<String>::new(), "{:#?}", trace(&env));

    let env = Env::new();
    let reservation = planner_with_swap(&env);
    ask_planner(&env);
    let output = answer(
        &env,
        &[3, 4],
        Some(("RIMZ_TEST_AGENT_PANE_AFTER_WAKE", &|| {
            bind_swap(&env, &reservation, NEW)
        })),
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        stderr(&output).contains(&format!(
            "error: {ANSWERED}: the agent left pane zellij:{OLD} while it was woken; no key \
             was sent"
        )),
        "{}",
        stderr(&output)
    );
    assert_eq!(typed(&env), Vec::<String>::new(), "{:#?}", trace(&env));
}

/// A running supervised run of `sess-old`, the session the budget park settles.
fn planner_run(env: &Env) -> rimz::store::run::RunRecord {
    let workspace = env.resolve_workspace(&env.project_root);
    let mut run = rimz::store::run::RunRecord::new(
        workspace.workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        rimz::agents::PermissionMode::Auto,
        "task".into(),
        env.project_root.clone(),
    );
    run.agent_id = Some("sess-old".into());
    run.status = rimz::store::run::RunStatus::Running;
    rimz::harness::run::create(env.store().paths(), &run).unwrap();
    run
}

fn run_status(env: &Env, run: &rimz::store::run::RunRecord) -> rimz::store::run::RunStatus {
    rimz::harness::run::list(env.store().paths())
        .unwrap()
        .into_iter()
        .find(|record| record.run_id == run.run_id)
        .unwrap()
        .status
}

/// The budget park helper for `sess-old` in `OLD`, held at `hold` when it names one.
fn budget_park(env: &Env, ids: &[u64], hold: Option<&dyn Fn()>) -> std::process::Output {
    let request = rimz::harness::budget::BudgetParkRequest {
        workspace_id: env.workspace_id.clone(),
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "sess-old".into(),
        pane_id: PaneId::from_parts(MuxName::Zellij, OLD),
        at_cost: Some(1.0),
    };
    let mut command = acting(env, ids);
    command
        .args(rimz::child_process::agent_helper_argv(
            "budget-park",
            &request,
        ))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let Some(during) = hold else {
        return command.output().unwrap();
    };
    let rendezvous = DeliveryRendezvous::new(env, "park-hold");
    let child = command
        .env("RIMZ_TEST_AGENT_PANE_AFTER_WAKE", &rendezvous.path)
        .spawn()
        .unwrap();
    if let Some(mut held) = rendezvous.arrive_timeout(Duration::from_secs(10)) {
        during();
        held.write_all(&[0]).unwrap();
    }
    child.wait_with_output().unwrap()
}

/// The budget park presses Escape in the pane the lookup answers and settles the session's
/// run; when the agent moved during the lock, another session holds its pane, or no listed
/// pane hosts it, it presses nothing and settles nothing.
#[test]
fn budget_park_presses_and_settles_only_in_the_pane_the_lookup_answers() {
    use rimz::store::run::RunStatus;

    let env = Env::new();
    planner_with_swap(&env);
    let run = planner_run(&env);
    let output = budget_park(&env, &[3, 4], None);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(typed(&env), [OLD], "{:#?}", trace(&env));
    assert_ne!(run_status(&env, &run), RunStatus::Running);

    for case in ["nowhere", "moved", "other session"] {
        let env = Env::new();
        let reservation = planner_with_swap(&env);
        let run = planner_run(&env);
        let bind = || bind_swap(&env, &reservation, NEW);
        let output = match case {
            "nowhere" => budget_park(&env, &[4], None),
            "moved" => budget_park(&env, &[3, 4], Some(&bind)),
            _ => {
                bind();
                budget_park(&env, &[3, 4], None)
            }
        };
        assert!(!output.status.success(), "{case}: {output:?}");
        if case != "moved" {
            assert!(
                stderr(&output).contains("budget target pane is no longer bound to the agent"),
                "{case}: {}",
                stderr(&output)
            );
        }
        assert_eq!(
            typed(&env),
            Vec::<String>::new(),
            "{case}: {:#?}",
            trace(&env)
        );
        assert_eq!(run_status(&env, &run), RunStatus::Running, "{case}");
    }
}

/// Session `session` of `@planner`'s launch registers in zellij `pane`, with an owner of its
/// own: a provider that cleared its conversation, or a session that moved panes.
fn register_in(env: &Env, session: &str, pane: &str) {
    let process = dummy_agent_process();
    let owner = agent_owner(session, &process);
    reap_later(process);
    append_lifecycle(
        env,
        "claude",
        "SessionStart",
        session,
        LifecycleSignal::Registered,
        |observation| {
            observation.agent_name = Some("planner".to_owned());
            observation.launch_id = Some("launch-planner".into());
            observation.pane_id = Some(PaneId::from_parts(MuxName::Zellij, pane));
            observation.runtime_owner = Some(owner);
        },
    );
}

/// The one message record the test queued.
fn only_message(env: &Env) -> MessageRecord {
    let messages = env.store().list_messages().unwrap();
    assert_eq!(messages.len(), 1, "{messages:?}");
    messages.into_iter().next().unwrap()
}

/// Accepts the first lookup read at `hold`, runs `during`, and releases it, then releases
/// any later read until none arrives within a second: each read connects anew.
fn serve_reads(hold: &DeliveryRendezvous, during: impl FnOnce()) -> usize {
    let Some(mut held) = hold.arrive_timeout(Duration::from_secs(10)) else {
        return 0;
    };
    during();
    held.write_all(&[0]).unwrap();
    let mut reads = 1;
    while let Some(mut held) = hold.arrive_timeout(Duration::from_secs(1)) {
        held.write_all(&[0]).unwrap();
        reads += 1;
    }
    reads
}

/// The agent leaves the claim's pane while a fresh send waits under its pane lock, its row
/// still storing that pane: nothing is typed and the claim goes back to `Queued` with no
/// attempt counted. Once its binding and the lookup agree on the pane it is in, a delivery
/// types the message there.
#[test]
fn delivery_under_the_pane_lock_releases_a_claim_the_lookup_does_not_answer() {
    let env = Env::new();
    planner_with_swap(&env);
    let hold = DeliveryRendezvous::new(&env, "before-write");
    let send = acting(&env, &[3, 4])
        .env("RIMZ_TEST_MESSAGE_BEFORE_WRITE", &hold.path)
        .args(["message", "@planner", "--", "after the move"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(mut held) = hold.arrive_timeout(Duration::from_secs(10)) {
        list_stop_panes(&env, &[4]);
        held.write_all(&[0]).unwrap();
    }
    let output = send.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(typed(&env), Vec::<String>::new(), "{:#?}", trace(&env));
    let released = only_message(&env);
    assert_eq!(released.status, MessageStatus::Queued, "{released:?}");
    assert_eq!(released.attempts, 0, "{released:?}");
    assert_eq!(released.last_sent_at, None, "{released:?}");

    register_in(&env, "sess-old", NEW);
    let deliver = acting(&env, &[4])
        .args([
            "message",
            "deliver",
            "--message-id",
            released.message_id.as_str(),
        ])
        .output()
        .unwrap();
    assert!(deliver.status.success(), "{deliver:?}");
    let delivered = message_by_id(&env, &released.message_id);
    assert_ne!(delivered.status, MessageStatus::Queued, "{delivered:?}");
    assert!(!typed(&env).is_empty(), "{:#?}", trace(&env));
    assert!(
        typed(&env).iter().all(|pane| pane == NEW),
        "{:#?}",
        trace(&env)
    );
}

/// The agent moves after a fresh send resolved its card to the row storing `OLD` and before
/// the send reads the lookup: the send parks naming the pane the agent is in, with no claim,
/// no attempt, and nothing typed.
#[test]
fn a_fresh_send_parks_when_the_lookup_answers_another_pane() {
    let env = Env::new();
    let reservation = planner_with_swap(&env);
    let hold = DeliveryRendezvous::new(&env, "before-read");
    let send = acting(&env, &[3, 4])
        .env("RIMZ_TEST_AGENT_PANE_BEFORE_READ", &hold.path)
        .args(["message", "@planner", "--", "where are you"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let reads = serve_reads(&hold, || bind_swap(&env, &reservation, NEW));
    let output = send.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(reads, 1, "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(&format!(
            "receiver in pane zellij:{NEW}, not its bound pane zellij:{OLD}"
        )),
        "{output:?}"
    );
    assert_eq!(typed(&env), Vec::<String>::new(), "{:#?}", trace(&env));
    let parked = only_message(&env);
    assert_eq!(parked.status, MessageStatus::Queued, "{parked:?}");
    assert_eq!(parked.attempts, 0, "{parked:?}");
}

/// A pane listing that cannot be read parks a fresh send and keeps it waiting: each sweep
/// records the same blocker, counts no attempt, and types nothing.
#[test]
fn an_unreadable_listing_keeps_a_message_waiting_unchanged_across_sweeps() {
    let env = Env::new();
    planner_with_swap(&env);
    let unreadable = |env: &Env| {
        let command = acting(env, &[3]);
        std::fs::write(env.project_root.join("stop-panes.json"), "not a listing").unwrap();
        command
    };
    let output = unreadable(&env)
        .args(["message", "@planner", "--", "can you hear me"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("receiver pane unreadable: "),
        "{output:?}"
    );
    let mut seen = Vec::new();
    for _ in 0..2 {
        let sweep = unreadable(&env)
            .args(["message", "sweep"])
            .output()
            .unwrap();
        assert!(sweep.status.success(), "{sweep:?}");
        let waiting = only_message(&env);
        assert_eq!(waiting.status, MessageStatus::Queued, "{waiting:?}");
        assert_eq!(waiting.attempts, 0, "{waiting:?}");
        assert!(
            waiting
                .last_error
                .as_deref()
                .is_some_and(|error| error.starts_with("stuck: receiver pane unreadable: ")),
            "{waiting:?}"
        );
        seen.push((waiting.status, waiting.attempts, waiting.last_error));
    }
    assert_eq!(seen[0], seen[1]);
    assert_eq!(typed(&env), Vec::<String>::new(), "{:#?}", trace(&env));
}

/// A receiver whose owner process is gone is hosted nowhere though its pane is listed: the
/// send waits as no live pane and types nothing.
#[test]
fn a_receiver_whose_owner_died_waits_as_no_live_pane() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let mut process = dummy_agent_process();
    let owner = agent_owner("sess-a", &process);
    append_lifecycle(
        &env,
        "claude",
        "SessionStart",
        "sess-a",
        LifecycleSignal::Registered,
        |observation| {
            observation.agent_name = Some("planner".to_owned());
            observation.pane_id = Some(PaneId::from_parts(MuxName::Zellij, OLD));
            observation.runtime_owner = Some(owner);
        },
    );
    process.kill().unwrap();
    process.wait().unwrap();
    let output = acting(&env, &[3])
        .args(["message", "@planner", "--", "anyone home"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("no live pane"),
        "{output:?}"
    );
    assert_eq!(typed(&env), Vec::<String>::new(), "{:#?}", trace(&env));
    let waiting = only_message(&env);
    assert_eq!(waiting.status, MessageStatus::Queued, "{waiting:?}");
    assert_eq!(waiting.attempts, 0, "{waiting:?}");
}

/// A restart's session replaces the receiver in another pane during the interrupt delay:
/// the paste stays sent, the key is withheld, and the send says so apart from an
/// unconfirmed stop.
#[test]
fn an_interrupt_key_is_withheld_when_the_session_is_replaced_during_the_delay() {
    let env = Env::new();
    let reservation = planner_with_swap(&env);
    append_lifecycle(
        &env,
        "claude",
        "UserPromptSubmit",
        "sess-old",
        LifecycleSignal::TurnStarted { turn_id: None },
        |_| {},
    );
    let hold = DeliveryRendezvous::new(&env, "before-key");
    let send = acting(&env, &[3, 4])
        .env("RIMZ_TEST_MESSAGE_BEFORE_INTERRUPT_KEY", &hold.path)
        .env("RIMZ_MESSAGE_INTERRUPT_DELAY_MS", "0")
        .args(["message", "--interrupt", "@planner", "--", "stop there"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(mut held) = hold.arrive_timeout(Duration::from_secs(10)) {
        bind_swap(&env, &reservation, NEW);
        held.write_all(&[0]).unwrap();
    }
    let output = send.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let lines = trace(&env);
    assert!(
        lines
            .iter()
            .any(|line| is_paste(line, &user_message("stop there"))),
        "{lines:#?}"
    );
    assert!(!lines.iter().any(|line| is_escape_key(line)), "{lines:#?}");
    assert!(
        stderr(&output).contains(
            "the message was sent, but its interrupt key was not: the receiver left its pane \
             during the interrupt delay"
        ),
        "{}",
        stderr(&output)
    );
}

/// A pane no registered session holds (an agent RimZ found running, unbound) asks no lookup:
/// the steer types into it as today.
#[test]
fn a_sessionless_pane_is_delivered_with_no_lookup_read() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("codex");
    let panes = env.write_pane_fixture(&[agent_pane(&env, "codex")]);
    let hold = DeliveryRendezvous::new(&env, "before-read");
    let trace = env.project_root.join("sessionless-trace.log");
    // A read would hold the send on a rendezvous nobody serves, which fails it.
    let output = traced_rimz(&env, &trace)
        .env("RIMZ_TEST_PANE_LIST", &panes)
        .env("RIMZ_TEST_AGENT_PANE_BEFORE_READ", &hold.path)
        .args(["message", "--steer", "@codex", "--", "continue"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(hold.arrive_timeout(Duration::from_millis(1)).is_none());
    assert_text_then_enter(&trace, &user_message("continue"));
}

/// An input prompt raised while a steer's under-lock lookup lists the panes is seen by the
/// write decision, which reads the row after that lookup: nothing is pasted.
#[test]
fn a_steer_skips_an_input_prompt_raised_during_its_under_lock_lookup() {
    let env = Env::new();
    planner_with_swap(&env);
    let hold = DeliveryRendezvous::new(&env, "under-lock-read");
    let send = acting(&env, &[3, 4])
        .env("RIMZ_TEST_AGENT_PANE_BEFORE_READ", &hold.path)
        .args(["message", "--steer", "@planner", "--", "steer me"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The first read comes before the claim, the second under the pane lock.
    let mut first = hold.arrive_timeout(Duration::from_secs(10)).unwrap();
    first.write_all(&[0]).unwrap();
    let mut under_lock = hold.arrive_timeout(Duration::from_secs(10)).unwrap();
    push_pending_agent_ask(&env, "sess-old");
    under_lock.write_all(&[0]).unwrap();
    while let Some(mut later) = hold.arrive_timeout(Duration::from_secs(1)) {
        later.write_all(&[0]).unwrap();
    }
    let output = send.wait_with_output().unwrap();
    assert_eq!(typed(&env), Vec::<String>::new(), "{:#?}", trace(&env));
    assert!(!output.status.success(), "{output:?}");
    assert!(
        stderr(&output).contains("waiting on your input"),
        "{output:?}"
    );
}

/// `message steer <id>` on a record the lookup holds waiting names the lookup's cause, not a
/// delivery attempt in progress.
#[test]
fn a_steer_of_a_message_waiting_on_an_unreadable_listing_names_the_listing() {
    let env = Env::new();
    planner_with_swap(&env);
    let unreadable = |env: &Env| {
        let command = acting(env, &[3]);
        std::fs::write(env.project_root.join("stop-panes.json"), "not a listing").unwrap();
        command
    };
    let output = unreadable(&env)
        .args(["message", "@planner", "--", "can you hear me"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let waiting = only_message(&env);
    let steer = unreadable(&env)
        .args(["message", "steer", waiting.message_id.as_str()])
        .output()
        .unwrap();
    assert!(!steer.status.success(), "{steer:?}");
    assert!(
        stderr(&steer).contains("error: stuck: receiver pane unreadable: "),
        "{}",
        stderr(&steer)
    );
    assert!(
        !stderr(&steer).contains("recent delivery attempt"),
        "{}",
        stderr(&steer)
    );
    assert_eq!(typed(&env), Vec::<String>::new(), "{:#?}", trace(&env));
}

/// The launch's other session holds the pane the lookup answers, not the session asking:
/// `answer` exits 2 and types into no pane.
#[test]
fn answer_refuses_when_another_session_of_the_launch_holds_the_pane() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    // The rival registered first and runs a turn, so it is the launch's occupant even while
    // the later session asks (Claude keeps a launch's first registered open turn).
    rival(&env, NEW);
    launched_in(&env, "sess-old", OLD);
    ask_planner(&env);
    let asking = env.store().snapshot_cached().unwrap();
    let (ask, _) = asking
        .agents
        .iter()
        .find(|row| row.agent_id == "sess-old")
        .and_then(|row| row.actionable_asks().next())
        .unwrap();
    let output = acting(&env, &[3, 4])
        .args(["answer", ask.id.as_str(), "safe", "--no-wait"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        stderr(&output).contains(&format!(
            "holds its pane zellij:{NEW}, not the session asking `{}`; no key was sent",
            ask.id
        )),
        "{}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains(", sess-new, holds"),
        "{}",
        stderr(&output)
    );
    assert_eq!(typed(&env), Vec::<String>::new(), "{:#?}", trace(&env));
}

/// Session `session` of launch `launch-planner`, registered and bound in zellij `pane` with an
/// owner of its own: two such sessions are one agent live in two panes, as after an exact-id
/// resume of a cleared conversation.
fn launched_in(env: &Env, session: &str, pane: &str) {
    register_in(env, session, pane);
    let workspace = env.resolve_workspace(&env.project_root);
    let row = env
        .store()
        .audit_rows()
        .unwrap()
        .iter()
        .find(|row| row.agent_id == session)
        .cloned()
        .unwrap();
    env.store()
        .append_event(&EventEnvelope::agent_launched(
            workspace.workspace_id,
            workspace.session_name,
            &AgentKind::new_unchecked("claude"),
            AgentLaunchPayload {
                agent_id: session.into(),
                launch_id: Some("launch-planner".into()),
                system_prompt_digest: None,
                agent_name: "planner".to_owned(),
                agent_name_explicit: true,
                launch: LaunchParams::default(),
                state: AgentLaunchState::Bound,
                run_id: None,
                pane_id: Some(PaneId::from_parts(MuxName::Zellij, pane)),
                runtime_owner: row.runtime_owner,
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: None,
                prompt: None,
                description: None,
            },
        ))
        .unwrap();
}

/// Session `sess-new` of `@planner`'s launch, live in zellij `pane` with a turn open.
fn rival(env: &Env, pane: &str) {
    launched_in(env, "sess-new", pane);
    append_lifecycle(
        env,
        "claude",
        "UserPromptSubmit",
        "sess-new",
        LifecycleSignal::TurnStarted { turn_id: None },
        |_| {},
    );
}

/// `@planner` as session `sess-old` of launch `launch-planner`, registered in `OLD`.
fn planner(env: &Env) {
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    launched_in(env, "sess-old", OLD);
}

/// A session that registers in the receiver's pane during the interrupt delay holds it now:
/// the paste stays sent and the key is withheld.
#[test]
fn an_interrupt_key_is_withheld_when_another_session_holds_the_pane_after_the_delay() {
    let env = Env::new();
    planner(&env);
    append_lifecycle(
        &env,
        "claude",
        "UserPromptSubmit",
        "sess-old",
        LifecycleSignal::TurnStarted { turn_id: None },
        |_| {},
    );
    let hold = DeliveryRendezvous::new(&env, "before-key");
    let send = acting(&env, &[3, 4])
        .env("RIMZ_TEST_MESSAGE_BEFORE_INTERRUPT_KEY", &hold.path)
        .env("RIMZ_MESSAGE_INTERRUPT_DELAY_MS", "0")
        .args(["message", "--interrupt", "@planner", "--", "stop there"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(mut held) = hold.arrive_timeout(Duration::from_secs(10)) {
        // The turn ends and a new session of the launch opens one in the same pane.
        append_lifecycle(
            &env,
            "claude",
            "Stop",
            "sess-old",
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
                turn_id: None,
            },
            |_| {},
        );
        rival(&env, OLD);
        held.write_all(&[0]).unwrap();
    }
    let output = send.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let lines = trace(&env);
    assert!(
        lines
            .iter()
            .any(|line| is_paste(line, &user_message("stop there"))),
        "{lines:#?}"
    );
    assert!(!lines.iter().any(|line| is_escape_key(line)), "{lines:#?}");
    assert!(
        stderr(&output).contains(
            "the message was sent, but its interrupt key was not: another session of the \
             receiver holds its pane now"
        ),
        "{}",
        stderr(&output)
    );
}

/// `sess-old` at rest with 50k tokens in its window, due for an idle compaction one second
/// after its turn ended: a 10 s cache lifetime with a 9 s margin and a 1k floor.
fn idle_planner(env: &Env) {
    planner(env);
    for (key, value) in [
        ("harness.prompt_cache_margin", "9s"),
        ("harness.prompt_cache_ttl.claude", "10s"),
        ("harness.idle_compact", "1k"),
    ] {
        run_success(env.rimz().args(["config", "set", key, value]), key);
    }
    let mut context = rimz::agents::AgentContext::new("claude", jiff::Timestamp::now());
    context.tokens = Some(rimz::agents::AgentTokenUsage {
        current_context_tokens: Some(50_000),
        ..Default::default()
    });
    rimz::store::agent_context::write_record(
        &env.runtime_paths(),
        &rimz::agents::context::record::AgentContextRecord::new("claude", "sess-old", context),
    )
    .unwrap();
    for (event, signal) in [
        (
            "UserPromptSubmit",
            LifecycleSignal::TurnStarted { turn_id: None },
        ),
        (
            "Stop",
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
                turn_id: None,
            },
        ),
    ] {
        append_lifecycle(env, "claude", event, "sess-old", signal, |_| {});
    }
    std::thread::sleep(Duration::from_millis(1_500));
}

/// The idle-compaction helper for `sess-old` in `OLD`, held after its wake when `hold` names
/// what to do there.
fn idle_compact(env: &Env, hold: Option<&dyn Fn()>) -> std::process::Output {
    let request = rimz::harness::idle_compact::IdleCompactRequest {
        workspace_id: env.workspace_id.clone(),
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "sess-old".into(),
        pane_id: PaneId::from_parts(MuxName::Zellij, OLD),
        command: rimz::agents::compact_command(
            env.store()
                .snapshot_cached()
                .unwrap()
                .agents
                .iter()
                .find(|row| row.agent_id == "sess-old")
                .unwrap(),
            &rimz::config::HarnessConfig::default(),
        )
        .unwrap(),
        occupied_tokens: 50_000,
        label: "@planner".to_owned(),
    };
    let mut command = acting(env, &[3, 4]);
    command
        .args(rimz::child_process::agent_helper_argv(
            "idle-compact",
            &request,
        ))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let Some(during) = hold else {
        return command.output().unwrap();
    };
    let rendezvous = DeliveryRendezvous::new(env, "idle-hold");
    let child = command
        .env("RIMZ_TEST_AGENT_PANE_AFTER_WAKE", &rendezvous.path)
        .spawn()
        .unwrap();
    if let Some(mut held) = rendezvous.arrive_timeout(Duration::from_secs(10)) {
        during();
        held.write_all(&[0]).unwrap();
    }
    child.wait_with_output().unwrap()
}

fn compact_records(env: &Env) -> Vec<MessageRecord> {
    let mut messages = env.store().list_messages().unwrap();
    messages.extend(env.store().list_message_history().unwrap());
    messages.retain(|message| message.body == MessageBody::Command);
    messages
}

/// The idle-compaction helper queues its compact command only for the session in the pane the
/// lookup answers: an agent that moved during the wake, or a pane another session of its
/// launch holds, gets no compact record.
#[test]
fn idle_compaction_queues_nothing_when_the_lookup_does_not_answer_its_session() {
    let env = Env::new();
    idle_planner(&env);
    let output = idle_compact(&env, None);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(compact_records(&env).len(), 1, "{output:?}");

    for case in ["moved", "other session"] {
        let env = Env::new();
        idle_planner(&env);
        let bind = || rival(&env, NEW);
        let output = if case == "moved" {
            idle_compact(&env, Some(&bind))
        } else {
            bind();
            idle_compact(&env, None)
        };
        assert!(!output.status.success(), "{case}: {output:?}");
        assert!(
            stderr(&output).contains("idle-compaction target pane is no longer bound to the agent"),
            "{case}: {}",
            stderr(&output)
        );
        assert_eq!(compact_records(&env), Vec::new(), "{case}");
        assert_eq!(
            typed(&env),
            Vec::<String>::new(),
            "{case}: {:#?}",
            trace(&env)
        );
    }
}

/// A pane that cannot be woken keeps `answer`'s exit 1, with no key sent.
#[test]
fn answer_exits_1_when_the_owners_pane_cannot_be_woken() {
    let env = Env::new();
    planner_with_swap(&env);
    ask_planner(&env);
    // A directory where the hibernation lock goes fails the wake's lock for any uid.
    std::fs::create_dir_all(env.store().runtime_paths().locks_dir.join("hibernate.lock")).unwrap();
    let output = answer(&env, &[3, 4], None);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        stderr(&output).contains(&format!("error: waking pane zellij:{OLD}: ")),
        "{}",
        stderr(&output)
    );
    assert_eq!(typed(&env), Vec::<String>::new(), "{:#?}", trace(&env));
}
