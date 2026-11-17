//! Focus and capture of an agent act on the pane the hosting lookup answers at the point of
//! use, never on the pane stored on the row the command resolved; a pane the user names
//! reads no lookup.

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

/// The agent moves to `NEW` after `@planner` resolved to the row whose stored pane is `OLD`
/// and before the command reads the lookup: focus and capture act on `NEW`, the pane the
/// lookup answers, while `pane send` keeps the resolved row's stored pane.
#[test]
fn focus_and_capture_act_on_the_pane_the_lookup_answers_and_send_keeps_the_stored_pane() {
    for args in [
        &["agents", "focus", "@planner"][..],
        &["pane", "focus", "@planner"],
        &["agents", "show", "@planner", "--capture"],
        &["pane", "capture", "@planner"],
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
        } else {
            assert!(output.status.success(), "{args:?}: {output:?}");
            focused(&env)
        };
        assert_eq!(acted, [NEW], "{args:?}: {output:?}\n{:#?}", trace(&env));
    }

    let env = Env::new();
    planner_with_swap(&env);
    let send = acting(&env, &[3, 4])
        .args(["pane", "send", "@planner", "hi"])
        .output()
        .unwrap();
    assert!(send.status.success(), "{send:?}");
    assert_eq!(after_arg(&env, "--pane-id"), [OLD], "{:#?}", trace(&env));
    assert_eq!(
        lookup_reads(&env),
        1,
        "only send's room membership check lists panes: {:#?}",
        trace(&env)
    );
}

/// `@planner`'s stored pane is no longer listed, so the lookup hosts it nowhere: neither
/// focused nor captured, and each command says it has no pane.
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

/// A pane the user names reads no lookup, and is focused or captured as named.
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
}
