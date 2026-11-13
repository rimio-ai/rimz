//! The agent selector on the action commands: `--filter`, addresses on stdin
//! through a positional `-`, and `--all` as consent for more than one match.
//! For each command, what `agents list --filter Q -q` prints, piped to
//! `<command> - --all`, acts on the agents `<command> --filter Q --all` acts on.

use std::io::Write as _;
use std::process::{Child, Command, Output, Stdio};

use rimz::agents::{AgentLifecycleObservation, LaunchParams, LifecycleSignal};
use rimz::ids::{AgentKind, MuxName, PaneId, ViewKind};
use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope};

use crate::common::{
    DeliveryRendezvous, Env, path_with_front, write_env_dump_shim, write_fake_login_shell,
    zellij_trace_shim,
};

/// Every row the fixture holds: two top-level agents and one launched child.
const EVERY: &str = "parent=*,none";

/// A room with two top-level agents, `parent` and `peer`, and `kin`, a child
/// `parent` launched, stamped with the parent's launch id as
/// `harness::ancestry::resolve_launch_ancestry` stamps one. Each sits in its
/// own Zellij pane of the published frame.
fn seed_room() -> Env {
    seed(false)
}

/// [`seed_room`], with each agent owned by a process of its own that a stop can end, so
/// the stop can confirm the agent is gone.
fn seed_stoppable_room() -> Env {
    seed(true)
}

/// A process standing in for one agent, its launch identity scrubbed so nothing reads it
/// as an agent of its own; reaped once it exits.
fn owner_process(id: &str) -> rimz::pane::RuntimeOwner {
    let mut command = Command::new("sleep");
    for key in [
        rimz::harness::launch::ENV_AGENT_NAME,
        rimz::harness::launch::ENV_AGENT_PROFILE,
        rimz::harness::launch::ENV_AGENT_ROLE,
        rimz::harness::launch::ENV_TEAM,
        rimz::harness::launch::ENV_LAUNCH_GROUP,
        rimz::harness::launch::ENV_LAUNCH_ORDINAL,
        rimz::workspace::ENV_CHANNEL,
        rimz::harness::launch::ENV_AGENT_MODEL,
        rimz::harness::launch::ENV_AGENT_EFFORT,
    ] {
        command.env(key, "");
    }
    let mut child = command.arg("30").spawn().expect("spawn an agent owner");
    let owner =
        rimz::store::runtime::process_owner(rimz::pane::RuntimeOwnerKind::Agent, id, child.id());
    std::thread::spawn(move || child.wait());
    owner
}

fn seed(owned: bool) -> Env {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let workspace = env.resolve_workspace(&env.project_root);
    env.store()
        .append_event(&EventEnvelope::agent_launched(
            env.workspace_id.clone(),
            &workspace.session_name,
            &AgentKind::new_unchecked("claude"),
            AgentLaunchPayload {
                system_prompt_digest: None,
                agent_id: "parent".into(),
                launch_id: Some("launch-parent".into()),
                agent_name: "seat-parent".into(),
                agent_name_explicit: true,
                launch: LaunchParams::default(),
                state: AgentLaunchState::Bound,
                run_id: None,
                pane_id: Some(pane_id("terminal_1")),
                runtime_owner: None,
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: None,
                prompt: None,
                description: None,
            },
        ))
        .expect("launch parent");
    let first = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(10);
    for (order, (id, name, pane, parent)) in [
        ("parent", "seat-parent", "terminal_1", None),
        ("peer", "seat-peer", "terminal_2", None),
        ("kin", "seat-kin", "terminal_3", Some("launch-parent")),
    ]
    .into_iter()
    .enumerate()
    {
        let mut observation =
            AgentLifecycleObservation::new(Some(id.into()), LifecycleSignal::Registered);
        observation.agent_name = Some(name.to_owned());
        observation.worktree_path = Some(env.project_root.display().to_string());
        observation.pane_id = Some(pane_id(pane));
        observation.runtime_owner = owned.then(|| owner_process(id));
        if let Some(parent) = parent {
            observation.launch.parent_agent_id = Some(parent.into());
            observation.launch.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
            observation.launch.launch_depth = Some(1);
        }
        let mut registered = EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            &workspace.session_name,
            "claude",
            "SessionStart",
            &observation,
        );
        registered.timestamp = first + jiff::SignedDuration::from_secs(order as i64);
        env.store().append_event(&registered).expect("register");
    }
    let runtime = env.runtime_paths();
    runtime.ensure_dirs().expect("runtime dirs");
    let panes = ["terminal_1", "terminal_2", "terminal_3"]
        .into_iter()
        .map(|raw| frame_pane(&workspace.session_name, raw, &env.project_root))
        .collect();
    let frame = rimz::sidebar::frame::assemble_frame(
        panes,
        rimz::utils::time::unix_now_ms(),
        &workspace.session_name,
    );
    rimz::disk::atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &frame)
        .expect("publish pane frame");
    env
}

/// The fixture's panes as the deterministic pane list a fresh pane read takes
/// in place of the mux.
fn pane_fixture(env: &Env) -> std::path::PathBuf {
    let session = env.resolve_workspace(&env.project_root).session_name;
    let panes = ["terminal_1", "terminal_2", "terminal_3"]
        .into_iter()
        .map(|raw| frame_pane(&session, raw, &env.project_root))
        .collect::<Vec<_>>();
    env.write_pane_fixture(&panes)
}

fn pane_id(raw: &str) -> PaneId {
    PaneId::from_parts(MuxName::Zellij, raw)
}

fn frame_pane(session: &str, raw: &str, cwd: &std::path::Path) -> rimz::pane::PaneRef {
    rimz::pane::PaneRef {
        pane_id: pane_id(raw),
        session_name: session.to_owned(),
        view_id: Some("1".to_owned()),
        view_kind: Some(ViewKind::Tab),
        view_name: Some("room".to_owned()),
        title: None,
        is_floating: false,
        command: Some("claude".to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some(cwd.display().to_string()),
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        hosted_agent_lineage: Vec::new(),
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

/// `rimz <args>` against the shimmed Zellij session, with `stdin` piped in.
fn rimz(env: &Env, args: &[&str], stdin: &str) -> Output {
    spawn(env, args, stdin, None)
        .wait_with_output()
        .expect("rimz output")
}

/// `rimz <args>` started as `rimz` does, held, when `hold` is set, between
/// selecting its targets and acting on them until the test releases it.
fn spawn(env: &Env, args: &[&str], stdin: &str, hold: Option<&DeliveryRendezvous>) -> Child {
    spawn_with(env, args, stdin, |command| {
        if let Some(hold) = hold {
            command.env("RIMZ_TEST_SELECT_AFTER_SELECTION", &hold.path);
        }
    })
}

/// [`spawn`], with `configure` setting anything more on the command.
fn spawn_with(
    env: &Env,
    args: &[&str],
    stdin: &str,
    configure: impl FnOnce(&mut Command),
) -> Child {
    let session = env.resolve_workspace(&env.project_root).session_name;
    let mut command: Command = env.rimz();
    command
        .args(["--mux", "zellij"])
        .args(args)
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", env.project_root.join("trace.log"))
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{session} [Created 1s ago]\n"),
        )
        .env("RIMZ_TEST_PANE_LIST", pane_fixture(env))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure(&mut command);
    let mut child = command.spawn().expect("spawn rimz");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    child
}

/// Append one lifecycle signal for the fixture agent `id`, named `name` in
/// pane `pane`, as its hook would; with `child`, as a child `parent` launched.
fn observe(env: &Env, id: &str, name: &str, pane: &str, child: bool, signal: LifecycleSignal) {
    let session = env.resolve_workspace(&env.project_root).session_name;
    let mut observation = AgentLifecycleObservation::new(Some(id.into()), signal);
    observation.agent_name = Some(name.to_owned());
    observation.worktree_path = Some(env.project_root.display().to_string());
    observation.pane_id = Some(pane_id(pane));
    if child {
        observation.launch.parent_agent_id = Some("launch-parent".into());
        observation.launch.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
        observation.launch.launch_depth = Some(1);
    }
    let event = EventEnvelope::agent_lifecycle(
        env.workspace_id.clone(),
        &session,
        "claude",
        "test-observe",
        &observation,
    );
    env.store().append_event(&event).expect("lifecycle");
}

/// Hold `args` after it selected its targets, let `change` move the room,
/// then release it and return what it printed.
fn act_after(env: &Env, args: &[&str], change: impl FnOnce()) -> Output {
    act_after_with(env, args, |_| {}, change)
}

/// [`act_after`], with `configure` setting anything more on the command.
fn act_after_with(
    env: &Env,
    args: &[&str],
    configure: impl FnOnce(&mut Command),
    change: impl FnOnce(),
) -> Output {
    let hold = DeliveryRendezvous::new(env, "select");
    let child = spawn_with(env, args, "", |command| {
        command.env("RIMZ_TEST_SELECT_AFTER_SELECTION", &hold.path);
        configure(command);
    });
    let mut held = hold.arrive();
    change();
    held.write_all(&[0]).expect("release the held selection");
    child.wait_with_output().expect("rimz output")
}

/// `seat-kin` ends, and `parent` launches another child under its name in the
/// same pane. A root's name stays with its row, ended or not, while a child
/// name can pass to a later child, so only a child's address can move.
fn replace_kin(env: &Env) {
    observe(
        env,
        "kin",
        "seat-kin",
        "terminal_3",
        true,
        LifecycleSignal::Ended,
    );
    observe(
        env,
        "kin-2",
        "seat-kin",
        "terminal_3",
        true,
        LifecycleSignal::Registered,
    );
}

fn text(output: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn sorted_lines(bytes: &[u8]) -> Vec<String> {
    let mut lines = String::from_utf8_lossy(bytes)
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    lines.sort_unstable();
    lines
}

/// What `agents list --filter <query> -q` prints.
fn quiet_list(env: &Env, query: &str) -> String {
    let out = rimz(env, &["agents", "list", "--filter", query, "-q"], "");
    assert!(out.status.success(), "{}", text(&out));
    String::from_utf8(out.stdout).expect("utf8")
}

/// Two matches without `--all` exit 2, list both, and print the same command
/// with `--all`; the trace log shows that no pane was touched.
fn assert_consent(env: &Env, args: &[&str], query: &str, rest: &[&str]) {
    let mut argv = args.to_vec();
    argv.extend(["--filter", query]);
    argv.extend(rest);
    let out = rimz(env, &argv, "");
    let all = text(&out);
    assert_eq!(out.status.code(), Some(2), "{all}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let verb = args[args.len() - 1];
    assert!(
        stderr.contains(&format!(
            "agents selected; {verb} acts on more than one only with --all:"
        )),
        "{all}"
    );
    for address in quiet_list(env, query).lines() {
        assert!(
            stderr.lines().any(|line| line.trim() == address),
            "{address}: {all}"
        );
    }
    assert!(stderr.contains(" --all"), "{all}");
    let trace = std::fs::read_to_string(env.project_root.join("trace.log")).unwrap_or_default();
    assert!(
        !trace.contains("close-pane") && !trace.contains("write-chars"),
        "{trace}"
    );
}

/// The fixture panes a stop closed, by their `terminal_N` number: `parent`
/// is 1, `peer` 2, and `kin` 3.
fn closed(env: &Env) -> Vec<String> {
    let trace = std::fs::read_to_string(env.project_root.join("trace.log")).unwrap_or_default();
    let mut panes = trace
        .lines()
        .filter(|line| line.contains("close-pane"))
        .filter_map(|line| line.split(['_', ' ', '\t']).next_back().map(str::to_owned))
        .collect::<Vec<_>>();
    panes.sort_unstable();
    panes
}

#[test]
fn stop_selects_by_filter_and_stdin_and_asks_consent_for_more_than_one() {
    let env = seed_stoppable_room();
    assert_consent(&env, &["agents", "stop"], EVERY, &[]);
    assert!(closed(&env).is_empty());

    let one = rimz(&env, &["agents", "stop", "--filter", "parent=*"], "");
    assert!(one.status.success(), "{}", text(&one));
    assert_eq!(sorted_lines(&one.stdout), ["stopped @seat-kin"]);
    assert_eq!(closed(&env), ["3"]);

    let filtered = seed_stoppable_room();
    let piped = seed_stoppable_room();
    let query = "name=seat-parent,seat-peer";
    let by_filter = rimz(
        &filtered,
        &["agents", "stop", "--filter", query, "--all"],
        "",
    );
    assert!(by_filter.status.success(), "{}", text(&by_filter));
    let by_stdin = rimz(
        &piped,
        &["agents", "stop", "-", "--all"],
        &quiet_list(&piped, query),
    );
    assert!(by_stdin.status.success(), "{}", text(&by_stdin));
    assert_eq!(closed(&filtered), ["1", "2", "3"]);
    assert_eq!(closed(&piped), closed(&filtered));
    assert_eq!(
        sorted_lines(&by_stdin.stdout),
        sorted_lines(&by_filter.stdout)
    );

    let none = rimz(&env, &["agents", "stop", "--filter", "status=waiting"], "");
    assert!(none.status.success(), "{}", text(&none));
    assert!(
        String::from_utf8_lossy(&none.stderr).contains("matches the filter"),
        "{}",
        text(&none)
    );
}

#[test]
fn mark_selects_by_filter_and_stdin_with_one_receipt_per_agent() {
    let env = seed_room();
    assert_consent(&env, &["agents", "mark"], EVERY, &["unread"]);
    let marked = |env: &Env| {
        env.read_events()
            .iter()
            .filter(|event| event.method == "card.marked")
            .count()
    };
    // A launched child has no card to mark, so the round trip reads the roots.
    let roots = "name=seat-parent,seat-peer";
    let refused = rimz(&env, &["agents", "mark", "--filter", roots, "unread"], "");
    assert_eq!(refused.status.code(), Some(2), "{}", text(&refused));
    assert_eq!(marked(&env), 0);

    let by_filter = rimz(
        &env,
        &[
            "agents", "mark", "--filter", roots, "--all", "alert", "look",
        ],
        "",
    );
    assert!(by_filter.status.success(), "{}", text(&by_filter));
    assert_eq!(
        sorted_lines(&by_filter.stdout).len(),
        2,
        "{}",
        text(&by_filter)
    );
    assert_eq!(marked(&env), 2);
    let by_stdin = rimz(
        &env,
        &["agents", "mark", "-", "--all", "alert", "look"],
        &quiet_list(&env, roots),
    );
    assert!(by_stdin.status.success(), "{}", text(&by_stdin));
    assert_eq!(
        sorted_lines(&by_stdin.stdout),
        sorted_lines(&by_filter.stdout)
    );
    assert_eq!(marked(&env), 4);

    // One line fails: every line still gets its receipt, the others are
    // marked, and the command exits 1.
    let lines = format!("{}@nobody-here\n", quiet_list(&env, "name=seat-peer"));
    let mixed = rimz(&env, &["agents", "mark", "-", "--all", "unread"], &lines);
    assert_eq!(mixed.status.code(), Some(1), "{}", text(&mixed));
    let stdout = String::from_utf8_lossy(&mixed.stdout);
    assert!(
        stdout.contains("marked unread @seat-peer"),
        "{}",
        text(&mixed)
    );
    assert!(stdout.contains("error @nobody-here: "), "{}", text(&mixed));
    assert_eq!(marked(&env), 5);
}

#[test]
fn refresh_selects_by_filter_and_stdin_without_asking_consent() {
    let env = seed_room();
    let by_filter = rimz(&env, &["agents", "refresh", "--filter", EVERY], "");
    let lines = sorted_lines(&by_filter.stdout);
    assert_eq!(lines.len(), 3, "{}", text(&by_filter));
    for handle in ["@seat-parent", "@seat-peer", "@seat-kin"] {
        assert!(
            lines.iter().any(|line| line.contains(handle)),
            "{handle}: {}",
            text(&by_filter)
        );
    }
    let by_stdin = rimz(
        &env,
        &["agents", "refresh", "-", "--all"],
        &quiet_list(&env, EVERY),
    );
    assert_eq!(by_stdin.status.code(), by_filter.status.code());
    assert_eq!(sorted_lines(&by_stdin.stdout), lines);
    let named = rimz(&env, &["agents", "refresh", "@seat-peer", "--all"], "");
    assert_eq!(named.status.code(), Some(2), "{}", text(&named));
    assert!(named.stdout.is_empty(), "{}", text(&named));
    let room = rimz(
        &env,
        &["agents", "refresh", "--all", "--filter", "name=seat-peer"],
        "",
    );
    assert_eq!(sorted_lines(&room.stdout).len(), 1, "{}", text(&room));
}

/// Each root finishes a turn; `wait` on the filter and on the piped list
/// settles the same agents with the same results.
#[test]
fn wait_selects_by_filter_and_stdin_alike() {
    let env = seed_room();
    for (id, name, pane) in [
        ("parent", "seat-parent", "terminal_1"),
        ("peer", "seat-peer", "terminal_2"),
    ] {
        observe(
            &env,
            id,
            name,
            pane,
            false,
            LifecycleSignal::TurnStarted { turn_id: None },
        );
        let ended = LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        };
        observe(&env, id, name, pane, false, ended);
    }
    let roots = "name=seat-parent,seat-peer";
    let json = |out: &Output| -> serde_json::Value {
        assert!(out.status.success(), "{}", text(out));
        serde_json::from_slice(&out.stdout).unwrap_or_else(|err| panic!("{err}: {}", text(out)))
    };
    let wait = ["agents", "wait", "--all", "--json", "--timeout", "10s"];
    let by_filter = json(&rimz(&env, &[&wait[..], &["--filter", roots]].concat(), ""));
    let by_stdin = json(&rimz(
        &env,
        &[&wait[..], &["-"]].concat(),
        &quiet_list(&env, roots),
    ));
    let settled = by_filter
        .as_object()
        .map(|map| map.keys().cloned().collect::<Vec<_>>());
    assert_eq!(
        settled,
        Some(vec!["@seat-parent".to_owned(), "@seat-peer".to_owned()]),
        "{by_filter}"
    );
    assert_eq!(by_stdin, by_filter);
}

/// Set `command` up to relaunch a fixture agent: host isolation, a trusted
/// project, a fake provider and login shell, and the fixture's panes listed.
fn relaunchable(env: &Env, command: &mut Command) {
    const LIST_PANES: &str = r#"[{"id":1,"is_plugin":false,"tab_id":1,"title":"claude"},{"id":2,"is_plugin":false,"tab_id":1,"title":"claude"},{"id":3,"is_plugin":false,"tab_id":1,"title":"claude"}]"#;
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[agents]\nisolation = 'host'\n",
    )
    .expect("host isolation");
    crate::common::trust_claude_project(env, &env.project_root);
    let provider = write_env_dump_shim(env, "claude");
    let shell = write_fake_login_shell(env, "restart-shell", &[]);
    let session = env.resolve_workspace(&env.project_root).session_name;
    command
        .env("SHELL", &shell)
        .env("PATH", path_with_front(&provider))
        .env("ZELLIJ_SESSION_NAME", &session)
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES", LIST_PANES);
}

/// How many panes a restart opened.
fn new_panes(env: &Env) -> usize {
    std::fs::read_to_string(env.project_root.join("trace.log"))
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains("new-pane"))
        .count()
}

/// A restart relaunches each selected agent in a new pane, under a fake
/// provider and login shell, the same agents for the filter and the piped list.
#[test]
fn restart_selects_by_filter_and_stdin_alike() {
    let roots = "name=seat-parent,seat-peer";
    let restart = |env: &Env, args: &[&str], stdin: &str| {
        spawn_with(env, args, stdin, |command| relaunchable(env, command))
            .wait_with_output()
            .expect("rimz output")
    };
    let filtered = seed_room();
    let by_filter = restart(
        &filtered,
        &["agents", "restart", "--filter", roots, "--all"],
        "",
    );
    assert!(by_filter.status.success(), "{}", text(&by_filter));
    let piped = seed_room();
    let list = quiet_list(&piped, roots);
    let by_stdin = restart(&piped, &["agents", "restart", "-", "--all"], &list);
    assert!(by_stdin.status.success(), "{}", text(&by_stdin));
    assert_eq!(new_panes(&filtered), 2, "{}", text(&by_filter));
    assert_eq!(new_panes(&piped), new_panes(&filtered));
    // A fixture agent has no recorded conversation, so each restart starts
    // fresh under a newly minted handle: compare the receipts without it. The
    // selected address starts each line, since the new handle names neither.
    let receipts = |out: &Output| {
        sorted_lines(&out.stdout)
            .into_iter()
            .map(|line| line.split(" as @").next().unwrap_or_default().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        receipts(&by_filter),
        [
            "@seat-parent: restarted fresh",
            "@seat-peer: restarted fresh"
        ]
    );
    assert_eq!(receipts(&by_stdin), receipts(&by_filter));
}

#[test]
fn restart_and_wait_ask_consent_for_more_than_one_match() {
    let env = seed_room();
    assert_consent(&env, &["agents", "restart"], EVERY, &[]);
    assert_consent(&env, &["agents", "wait"], EVERY, &[]);
    let none = rimz(
        &env,
        &["agents", "restart", "--filter", "status=waiting"],
        "",
    );
    assert!(none.status.success(), "{}", text(&none));
    let mixed = rimz(
        &env,
        &["agents", "restart", "@seat-peer", "--filter", EVERY],
        "",
    );
    assert_eq!(mixed.status.code(), Some(2), "{}", text(&mixed));
}

#[test]
fn message_selects_by_filter_and_stdin_as_one_send() {
    let env = seed_room();
    assert_consent(&env, &["message"], EVERY, &[]);
    let recipients = |env: &Env| {
        let (live, history) = env.store().list_messages_and_history().expect("messages");
        let mut ids = live
            .into_iter()
            .chain(history)
            .map(|record| record.agent_id.to_string())
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids
    };
    assert!(recipients(&env).is_empty());
    let filtered = seed_room();
    let by_filter = rimz(
        &filtered,
        &["message", "--filter", EVERY, "--all", "hello"],
        "",
    );
    assert!(by_filter.status.success(), "{}", text(&by_filter));
    let piped = seed_room();
    let by_stdin = rimz(
        &piped,
        &["message", "-", "--all", "hello"],
        &quiet_list(&piped, EVERY),
    );
    assert!(by_stdin.status.success(), "{}", text(&by_stdin));
    assert_eq!(recipients(&filtered), ["kin", "parent", "peer"]);
    assert_eq!(recipients(&piped), recipients(&filtered));
    for record in filtered.store().list_messages().expect("messages") {
        assert!(!record.text.contains("@all"), "{}", record.text);
    }

    let twice = rimz(&env, &["message", "-", "--stdin"], "@seat-peer\n");
    assert_eq!(twice.status.code(), Some(2), "{}", text(&twice));
    assert!(recipients(&env).is_empty());
}

/// The act-time rule at message's decision point: an address that now names
/// another agent and one whose agent has ended each land in the send's misses,
/// the rest are sent to, and the agent that took the address gets nothing.
#[test]
fn message_skips_addresses_that_moved_or_ended_by_act_time() {
    let env = seed_room();
    let out = act_after(
        &env,
        &["message", "--filter", EVERY, "--all", "hello"],
        || {
            replace_kin(&env);
            observe(
                &env,
                "peer",
                "seat-peer",
                "terminal_2",
                false,
                LifecycleSignal::Ended,
            );
        },
    );
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines = stdout.lines().collect::<Vec<_>>();
    assert!(
        lines.contains(&"error @seat-kin: now names another agent than the one selected"),
        "{}",
        text(&out)
    );
    // An ended root's handle no longer resolves at all, so its miss is the
    // resolver's own refusal.
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("error @seat-peer: no agent matches target `@seat-peer`")),
        "{}",
        text(&out)
    );
    assert_eq!(lines.len(), 3, "{}", text(&out));
    let (live, history) = env.store().list_messages_and_history().expect("messages");
    let recipients = live
        .into_iter()
        .chain(history)
        .map(|record| record.agent_id.to_string())
        .collect::<Vec<_>>();
    assert_eq!(recipients, ["parent"]);
}

/// The act-time rule at the destructive commands' decision points: `seat-kin`
/// moves to another child while the command is held, so its line fails and
/// the newcomer is never acted on, while `seat-peer` still is.
#[test]
fn stop_restart_and_refresh_skip_an_address_that_moved_by_act_time() {
    let pair = "name=seat-kin,seat-peer parent=*,none";
    // The ended `kin` still holds its pane, so a resolver that reads pane
    // bindings sees two agents at the address rather than one that moved.
    let assert_moved = |out: &Output| {
        assert_eq!(out.status.code(), Some(1), "{}", text(out));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout
                .lines()
                .any(|line| line.starts_with("error @seat-kin: ")),
            "{}",
            text(out)
        );
        assert_eq!(stdout.lines().count(), 2, "{}", text(out));
    };

    let env = seed_room();
    let out = act_after(&env, &["agents", "stop", "--filter", pair, "--all"], || {
        replace_kin(&env)
    });
    assert_moved(&out);
    assert_eq!(closed(&env), ["2"], "{}", text(&out));

    let env = seed_room();
    let out = act_after(&env, &["agents", "refresh", "--filter", pair], || {
        replace_kin(&env)
    });
    assert_moved(&out);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("@seat-peer"),
        "{}",
        text(&out)
    );

    let env = seed_room();
    let out = act_after_with(
        &env,
        &["agents", "restart", "--filter", pair, "--all"],
        |command| relaunchable(&env, command),
        || replace_kin(&env),
    );
    assert_moved(&out);
    assert_eq!(new_panes(&env), 1, "{}", text(&out));
    assert!(
        closed(&env).iter().all(|pane| pane != "3"),
        "{}",
        text(&out)
    );
}

/// Mark's act-time read: a root that ends while the command is held gets its
/// error line, and the other root is still marked.
#[test]
fn mark_skips_an_agent_that_ended_by_act_time() {
    let env = seed_room();
    let out = act_after(
        &env,
        &[
            "agents",
            "mark",
            "--filter",
            "name=seat-parent,seat-peer",
            "--all",
            "unread",
        ],
        || {
            observe(
                &env,
                "peer",
                "seat-peer",
                "terminal_2",
                false,
                LifecycleSignal::Ended,
            )
        },
    );
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("error @seat-peer: ")),
        "{}",
        text(&out)
    );
    assert!(
        stdout.contains("marked unread @seat-parent"),
        "{}",
        text(&out)
    );
    let marked = env
        .read_events()
        .iter()
        .filter(|event| event.method == "card.marked")
        .count();
    assert_eq!(marked, 1, "{}", text(&out));
}

/// A run line whose stop fails is that line's failed receipt: the agent after
/// it is still stopped, and the command exits 1.
#[test]
fn stop_carries_on_past_a_run_whose_stop_fails() {
    use rimz::store::run::RunRecord;
    let env = seed_stoppable_room();
    let mut run = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        rimz::agents::PermissionMode::Auto,
        "task".to_owned(),
        env.project_root.clone(),
    );
    run.pane_id = Some(pane_id("terminal_9"));
    rimz::harness::run::create(env.store().paths(), &run).expect("create run");
    // The run's pane stays listed and refuses to close; every other close
    // goes through the trace shim.
    let zellij = env.project_root.join("zellij-close-9");
    std::fs::write(
        &zellij,
        format!(
            "#!/bin/sh\ncase \" $* \" in\n  *\" close-pane --pane-id terminal_9 \"*) echo refused >&2; exit 1 ;;\nesac\nexec '{}' \"$@\"\n",
            zellij_trace_shim().display()
        ),
    )
    .expect("write zellij wrapper");
    let mut mode = std::fs::metadata(&zellij).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    std::fs::set_permissions(&zellij, mode).unwrap();
    let lines = format!("{}\n@seat-peer\n", run.run_id);
    let out = spawn_with(&env, &["agents", "stop", "-", "--all"], &lines, |command| {
        command.env("RIMZ_ZELLIJ_BIN", &zellij).env(
            "RIMZ_TEST_ZELLIJ_LIST_PANES",
            r#"[{"id":9,"is_plugin":false,"tab_id":1,"title":"sh"}]"#,
        );
    })
    .wait_with_output()
    .expect("rimz output");
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with(&format!("error {}: ", run.run_id))),
        "{}",
        text(&out)
    );
    assert!(stdout.contains("stopped @seat-peer"), "{}", text(&out));
    assert!(closed(&env).contains(&"2".to_owned()), "{}", text(&out));
}

/// Two identical stdin lines select one agent, which needs no `--all`.
#[test]
fn identical_stdin_lines_select_one_agent() {
    let env = seed_stoppable_room();
    let out = rimz(&env, &["agents", "stop", "-"], "@seat-peer\n@seat-peer\n");
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(sorted_lines(&out.stdout), ["stopped @seat-peer"]);
    assert_eq!(closed(&env), ["2"]);
}

/// `wait` is a join: a selected address that resolves to nothing refuses the
/// whole wait before any waiting, as a typed reference does.
#[test]
fn wait_refuses_a_selection_with_an_address_that_names_nothing() {
    let env = seed_room();
    let out = rimz(
        &env,
        &["agents", "wait", "-", "--all", "--timeout", "10s"],
        "@missing-here\n@seat-peer\n",
    );
    assert!(!out.status.success(), "{}", text(&out));
    assert!(out.stdout.is_empty(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("@missing-here"),
        "{}",
        text(&out)
    );
}

/// `--filter help` prints the field table outside any room, and mark needs
/// no other argument for it; under `--filter`, a missing mark is named.
#[test]
fn filter_help_prints_the_table_outside_a_room() {
    let env = Env::new();
    for args in [
        &["agents", "stop", "--filter", "help"][..],
        &["agents", "restart", "--filter", "help"],
        &["agents", "mark", "--filter", "help"],
    ] {
        let out = env.rimz().args(args).output().expect("rimz");
        assert!(out.status.success(), "{args:?}: {}", text(&out));
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("FIELD"),
            "{args:?}: {}",
            text(&out)
        );
    }
    let out = env
        .rimz()
        .args(["agents", "mark", "--filter", "name=x"])
        .output()
        .expect("rimz");
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("<MARK>"),
        "{}",
        text(&out)
    );
}

/// Under `--filter`, text after `--` is sent as written, a leading hyphen or
/// a subcommand's name included; a subcommand beside a selection refuses.
#[test]
fn message_filter_text_after_the_terminator_is_sent_as_written() {
    let env = seed_room();
    for body in ["-x", "list"] {
        let out = rimz(
            &env,
            &["message", "--filter", "name=seat-peer", "--", body],
            "",
        );
        assert!(out.status.success(), "{body}: {}", text(&out));
    }
    let texts = {
        let (live, history) = env.store().list_messages_and_history().expect("messages");
        live.into_iter().chain(history)
    }
    .map(|record| record.text)
    .collect::<Vec<_>>();
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(texts.iter().any(|text| text.ends_with("-x")), "{texts:?}");
    assert!(texts.iter().any(|text| text.ends_with("list")), "{texts:?}");

    let out = rimz(&env, &["message", "--filter", "name=seat-peer", "list"], "");
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(out.stdout.is_empty(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("-- <TEXT>"),
        "{}",
        text(&out)
    );
}

/// A selected steer prints the miss line even when its only outcome is a
/// skip the reporter refuses on.
#[test]
fn message_steer_prints_misses_beside_a_skip() {
    let env = seed_room();
    observe(
        &env,
        "peer",
        "seat-peer",
        "terminal_2",
        false,
        LifecycleSignal::AwaitingInput {
            extra_questions: 0,
            kind: rimz::agents::AskKind::Permission,
            ask_id: None,
            detail: None,
            native_key: None,
        },
    );
    let out = rimz(
        &env,
        &["message", "-", "--all", "--steer", "hi"],
        "@seat-peer\n@nobody-here\n",
    );
    assert!(!out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|line| line.starts_with("error @nobody-here: ")),
        "{}",
        text(&out)
    );
}

/// A stdin `@me` line sends to the calling agent.
#[test]
fn message_stdin_me_names_the_caller() {
    let env = seed_room();
    let out = spawn_with(&env, &["message", "-", "hi"], "@me\n", |command| {
        command
            .env("RIMZ_AGENT_KIND", "claude")
            .env("RIMZ_AGENT_ID", "peer");
    })
    .wait_with_output()
    .expect("rimz output");
    assert!(out.status.success(), "{}", text(&out));
    let recipients = {
        let (live, history) = env.store().list_messages_and_history().expect("messages");
        live.into_iter().chain(history)
    }
    .map(|record| record.agent_id.to_string())
    .collect::<Vec<_>>();
    assert_eq!(recipients, ["peer"]);
}

/// A stdin `@me` line from a caller RimZ cannot identify fails alone: every
/// other line still gets the message, and a lone one sends nothing.
#[test]
fn message_stdin_me_from_no_agent_misses_alone() {
    let env = seed_room();
    let recipients = || {
        let (live, history) = env.store().list_messages_and_history().expect("messages");
        live.into_iter()
            .chain(history)
            .map(|record| record.agent_id.to_string())
            .collect::<Vec<_>>()
    };
    let me_line =
        "error @me: @me requires an agent RimZ can identify; run this command from an agent pane";
    for (stdin, sent) in [("@me\n@seat-peer\n", ["peer"]), ("@me\n", ["peer"])] {
        let out = rimz(&env, &["message", "-", "--all", "hi"], stdin);
        assert_eq!(out.status.code(), Some(1), "{stdin:?}: {}", text(&out));
        assert!(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|line| line == me_line),
            "{stdin:?}: {}",
            text(&out)
        );
        assert_eq!(recipients(), sent, "{stdin:?}: {}", text(&out));
    }
}
