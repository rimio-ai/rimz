//! `agents restart` and `agents molt` replace the pane the hosting lookup answers for the
//! agent, read before restart's preflights and again after them, and close that pane only
//! once a read after the replacement bound no longer finds the agent there.

use std::io::Write as _;
use std::process::Stdio;
use std::time::Duration;

use super::*;
use crate::common::DeliveryRendezvous;

/// The zellij panes of `ids` as the room's listing, in JSON.
fn listing(ids: &[u64]) -> String {
    let panes = ids
        .iter()
        .map(|id| serde_json::json!({"id": id, "is_plugin": false, "tab_id": 1, "title": "claude", "pane_command": "claude"}))
        .collect::<Vec<_>>();
    serde_json::Value::Array(panes).to_string()
}

fn mux_log(env: &Env) -> String {
    std::fs::read_to_string(env.home_root.join("mux.log")).unwrap_or_default()
}

fn replacement_recorded(env: &Env) -> bool {
    env.store()
        .audit_rows()
        .unwrap()
        .iter()
        .any(|row| row.launch_id.as_deref() == Some("launch-session") && row.epoch == 1)
}

/// The listing no longer shows `@planner`'s stored pane, so the lookup hosts it nowhere: the
/// restart refuses and opens nothing, before the folder-trust preflight could act on the
/// folder the fixture leaves undecided: no trust notice and no grant.
#[test]
fn restart_of_an_agent_hosted_nowhere_refuses_and_opens_nothing() {
    let (env, mut command) = owed_planner_restart(false, &listing(&[2]));
    let claude_config = env.home_root.join(".claude.json");
    std::fs::remove_file(&claude_config).unwrap();
    let output = command.bounded_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        stderr.contains("error: agent has no bound pane; nothing to restart"),
        "{stderr}"
    );
    assert!(!stderr.contains("has no trust decision for"), "{stderr}");
    assert!(!claude_config.exists(), "a folder-trust grant was written");
    assert!(!mux_log(&env).contains("new-pane"), "{}", mux_log(&env));
    assert!(!replacement_recorded(&env));
}

/// The agent moves to another pane while the restart runs its preflights: the read after
/// them finds it elsewhere, so the restart refuses and opens nothing.
#[test]
fn restart_refuses_when_the_agent_moves_during_its_checks() {
    let (env, mut command) = owed_planner_restart(false, &listing(&[1, 2]));
    let hold = DeliveryRendezvous::new(&env, "before-read");
    let restart = command
        .env("RIMZ_TEST_AGENT_PANE_BEFORE_READ", &hold.path)
        .env("RIMZ_TEST_SWAP_BIND_AS_WRAPPER", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The first read places the agent before the preflights; the second follows them.
    if let Some(mut first) = hold.arrive_timeout(Duration::from_secs(10)) {
        first.write_all(&[0]).unwrap();
        if let Some(mut second) = hold.arrive_timeout(Duration::from_secs(10)) {
            let mut observation = AgentLifecycleObservation::new(
                Some("provider-session".into()),
                LifecycleSignal::Registered,
            );
            observation.agent_name = Some("planner".to_owned());
            observation.launch_id = Some("launch-session".into());
            observation.pane_id = Some(rimz::ids::PaneId::parse("zellij:terminal_2").unwrap());
            let workspace = env.resolve_workspace(&env.project_root);
            env.store()
                .append_event(&EventEnvelope::agent_lifecycle(
                    workspace.workspace_id,
                    &workspace.session_name,
                    "claude",
                    "SessionStart",
                    &observation,
                ))
                .unwrap();
            second.write_all(&[0]).unwrap();
        }
    }
    let output = restart.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        stderr.contains(
            "error: the agent moved from pane zellij:terminal_1 to pane zellij:terminal_2 \
             during the restart's checks; nothing was restarted"
        ),
        "{stderr}"
    );
    assert!(!mux_log(&env).contains("new-pane"), "{}", mux_log(&env));
    assert!(!replacement_recorded(&env));
}

/// The pane a restart replaced is closed only once the lookup no longer finds the agent in
/// it: a replacement bound in that same pane leaves it open with a warning, and a resumed one
/// bound in a listed pane of its own closes the replaced pane alone.
#[test]
fn restart_closes_the_replaced_pane_only_once_the_agent_left_it() {
    let (env, mut command) = owed_planner_restart(false, &listing(&[1]));
    let output = command
        .env("RIMZ_TEST_SWAP_BIND_AS_WRAPPER", "old")
        .env("RIMZ_TEST_SWAP_BIND_WAIT_MS", "300")
        .bounded_output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains(
            "warning: the agent is still in pane zellij:terminal_1 after its replacement \
             bound; the pane was not closed"
        ),
        "{stderr}"
    );
    assert!(!mux_log(&env).contains("close-pane"), "{}", mux_log(&env));

    let (env, mut command) = owed_planner_restart(true, &listing(&[1, 9999]));
    let output = command
        .env("RIMZ_TEST_SWAP_BIND_AS_WRAPPER", "1")
        .bounded_output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let closed = mux_log(&env)
        .lines()
        .filter(|line| line.contains("close-pane"))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(closed.len(), 1, "{closed:?}");
    assert!(closed[0].contains("terminal_1"), "{closed:?}");
}

/// A molt whose caller the lookup hosts nowhere, its stored pane no longer listed, refuses
/// before it reserves the swap and opens nothing.
#[test]
fn molt_of_a_caller_hosted_nowhere_refuses_before_reserving_the_swap() {
    let (env, output) = molt_over(&[2], false, LaunchParams::default(), &[], |_| {});
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        stderr.contains("error: molt needs the caller's own pane; nothing was changed"),
        "{stderr}"
    );
    assert!(!mux_log(&env).contains("new-pane"), "{}", mux_log(&env));
    assert!(!replacement_recorded(&env));
}

/// A molt whose caller resolves to a session of its launch other than the one the lookup
/// holds in the launch's pane refuses before it reserves the swap and opens nothing.
#[test]
fn molt_of_a_caller_whose_launch_another_session_holds_refuses() {
    let (env, output) = molt_over(&[1, 2], true, LaunchParams::default(), &[], |_| {});
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        stderr.contains(
            "error: another session of the agent, revived-session, holds its pane \
             zellij:terminal_2; nothing was changed"
        ),
        "{stderr}"
    );
    let log = mux_log(&env);
    assert!(!log.contains("new-pane"), "{log}");
    assert!(!log.contains("close-pane"), "{log}");
    assert!(!replacement_recorded(&env));
}

/// `@planner`'s own `agents molt` over a room listing the zellij panes `ids`, with a revived
/// second session of its launch in `terminal_2` when `revived`, `@planner` launched with
/// `launch`, `extra` after its note, and `prepare` run on the room before it.
fn molt_over(
    ids: &[u64],
    revived: bool,
    launch: LaunchParams,
    extra: &[&str],
    prepare: impl FnOnce(&Env),
) -> (Env, std::process::Output) {
    let env = Env::new();
    env.install_agent_hooks("claude");
    env.record(&env.project_root);
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[agents]\nisolation = 'host'\n",
    )
    .unwrap();
    crate::common::trust_claude_project(&env, &env.project_root);
    register_paned_planner(&env, launch, None);
    if revived {
        register_revived_planner(&env, "terminal_2");
    }
    prepare(&env);
    let workspace = env.resolve_workspace(&env.project_root);
    let shim = write_env_dump_shim(&env, "claude");
    let shell = write_fake_login_shell(&env, "molt-shell", &[]);
    let panes = listing(ids);
    crate::common::room::seed_live_zellij_room(
        &env.runtime_paths(),
        &workspace.session_name,
        serde_json::from_str(&panes).unwrap(),
    );
    let mut molt = crate::common::wait::agent_wait(&env);
    if revived {
        // Without its launch id the caller resolves by its pane, `terminal_1`, to the
        // session there, which the launch's lookup does not hold.
        molt.env_remove("RIMZ_AGENT_ID");
    }
    let output = molt
        .args(["--mux", "zellij", "agents", "molt", "continue the work"])
        .args(extra)
        .env("RIMZ_TEST_SWAP_BIND_AS_WRAPPER", "1")
        .env("PATH", path_with_front(&shim))
        .env("SHELL", &shell)
        .env("ZELLIJ_PANE_ID", "1")
        .env("ZELLIJ_SESSION_NAME", &workspace.session_name)
        .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("mux.log"))
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES", &panes)
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{} [Created 1s ago]\n", workspace.session_name),
        )
        .bounded_output()
        .unwrap();
    (env, output)
}

/// Registers a second live session of `@planner`'s launch, at its epoch, in `zellij:<pane>`:
/// the state a revival leaves, where the later registration is the launch's occupant.
fn register_revived_planner(env: &Env, pane: &str) {
    crate::common::wait::register_agent(
        env,
        "revived-session",
        "launch-session",
        "planner",
        LaunchParams::default(),
    );
    let mut observation =
        AgentLifecycleObservation::new(Some("revived-session".into()), LifecycleSignal::Registered);
    observation.agent_name = Some("planner".to_owned());
    observation.launch_id = Some("launch-session".into());
    observation.pane_id = Some(rimz::ids::PaneId::parse(&format!("zellij:{pane}")).unwrap());
    let workspace = env.resolve_workspace(&env.project_root);
    env.store()
        .append_event(&EventEnvelope::agent_lifecycle(
            workspace.workspace_id,
            &workspace.session_name,
            "claude",
            "SessionStart",
            &observation,
        ))
        .unwrap();
}

/// Two live sessions of one launch in two listed panes: a restart of the session the lookup
/// does not hold in the launch's pane refuses, and neither splits beside nor closes the
/// pane of the session it does hold.
#[test]
fn restart_of_a_session_whose_launch_another_session_holds_refuses() {
    let (env, mut command) = owed_planner_restart_of("@provider-session", false, &listing(&[1, 2]));
    register_revived_planner(&env, "terminal_2");
    let output = command
        .env("RIMZ_TEST_SWAP_BIND_AS_WRAPPER", "1")
        .bounded_output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        stderr.contains(
            "error: another session of the agent, revived-session, holds its pane \
             zellij:terminal_2; nothing was restarted"
        ),
        "{stderr}"
    );
    let log = mux_log(&env);
    assert!(!log.contains("new-pane"), "{log}");
    assert!(!log.contains("close-pane"), "{log}");
    assert!(!replacement_recorded(&env));
}

/// The refusal a fresh Antigravity launch with no system prompt gives a limiting `mcps` list,
/// which only the agent file the prompt rides could apply.
const AGY_MCPS_REFUSAL: &str = "error: antigravity applies a `mcps` list only through the agent \
     file its system prompt rides, and this launch has no system prompt; add the kind base \
     `agents/antigravity.md` or another prompt source, or remove `mcps`";

/// Installs Antigravity's hooks and writes the Amp profile `seat` with `mcps: []`, which
/// carries no prompt onto Antigravity, and Antigravity's kind base, a prompt source there, when
/// `prompted`.
fn write_seat(env: &Env, prompted: bool) {
    env.install_agent_hooks("antigravity");
    crate::common::write_definition(
        env,
        "agents",
        "seat",
        "description: Seat.\nagent: amp\nmcps: []",
        "",
    );
    if prompted {
        crate::common::write_kind_base(env, "antigravity");
    }
}

fn seat_launch() -> LaunchParams {
    LaunchParams {
        profile: Some("seat".to_owned()),
        ..LaunchParams::default()
    }
}

/// A molt of the `seat` agent onto Antigravity, whose `mcps: []` then needs an agent file, is
/// refused before it reserves the swap or opens a pane; with the kind base as its prompt it is
/// not refused on that rule.
#[test]
fn molt_onto_antigravity_without_a_prompt_refuses_a_limiting_mcps_list() {
    let molt = |prompted: bool| {
        molt_over(
            &[1],
            false,
            seat_launch(),
            &["--agent", "antigravity"],
            |env| {
                write_seat(env, prompted);
            },
        )
    };
    let (env, output) = molt(false);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{output:?}");
    assert!(stderr.contains(AGY_MCPS_REFUSAL), "{stderr}");
    let log = mux_log(&env);
    assert!(!log.contains("new-pane"), "{log}");
    assert!(!replacement_recorded(&env));

    let (env, output) = molt(true);
    assert!(output.status.success(), "{output:?}");
    assert!(replacement_recorded(&env));
}

/// `agents restart @planner --no-continue` of an Antigravity seat molted from the profile
/// `seat`, its conversation present when `resumed`, in a room listing its pane alone.
fn agy_seat_restart(env: &Env, resumed: bool) -> std::process::Output {
    env.record(&env.project_root);
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[agents]\nisolation = 'host'\n",
    )
    .unwrap();
    let transcript = env.home_root.join("planner.jsonl");
    if resumed {
        std::fs::write(&transcript, "{}\n").unwrap();
    }
    let launch = LaunchParams {
        record: Some(Box::new(rimz::agents::LaunchRecord {
            agent: Some("antigravity".into()),
            ..Default::default()
        })),
        ..seat_launch()
    };
    crate::common::wait::register_seat(env, "antigravity", launch, "terminal_1", &transcript);
    let workspace = env.resolve_workspace(&env.project_root);
    let panes = listing(&[1]).replace("claude", "agy");
    crate::common::room::seed_live_zellij_room(
        &env.runtime_paths(),
        &workspace.session_name,
        serde_json::from_str(&panes).unwrap(),
    );
    let shim = write_env_dump_shim(env, "agy");
    let shell = write_fake_login_shell(env, "swap-shell", &[]);
    env.rimz()
        .args([
            "--mux",
            "zellij",
            "agents",
            "restart",
            "@planner",
            "--no-continue",
        ])
        .env("RIMZ_TEST_SWAP_BIND_AS_WRAPPER", "1")
        .env("SHELL", &shell)
        .env("PATH", path_with_front(&shim))
        .env("ZELLIJ_SESSION_NAME", &workspace.session_name)
        .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("mux.log"))
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES", &panes)
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{} [Created 1s ago]\n", workspace.session_name),
        )
        .bounded_output()
        .unwrap()
}

/// A restart of an Antigravity seat with no prompt and a limiting `mcps` list whose conversation
/// is gone launches fresh, so it is refused before it reserves the swap or opens a pane; the same
/// seat whose conversation resumes keeps the servers it started with and is not refused.
#[test]
fn fresh_restart_of_antigravity_without_a_prompt_refuses_a_limiting_mcps_list() {
    let env = Env::new();
    write_seat(&env, false);
    let output = agy_seat_restart(&env, false);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{output:?}");
    assert!(stderr.contains(AGY_MCPS_REFUSAL), "{stderr}");
    let log = mux_log(&env);
    assert!(!log.contains("new-pane"), "{log}");
    assert!(!replacement_recorded(&env));

    let env = Env::new();
    write_seat(&env, false);
    let output = agy_seat_restart(&env, true);
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("(resumed session provider-session)"),
        "{output:?}"
    );
}
