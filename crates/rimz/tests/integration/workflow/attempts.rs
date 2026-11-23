use super::*;
use crate::common::{
    ScrubSessionEnvExt, path_with_front, trust_codex_preflight_hooks, trust_codex_project,
    write_fake_login_shell,
};
use rimz::store::run::{RunRecord, RunStatus};
use rimz::store::workflow as records;
use std::collections::BTreeMap;

struct Room {
    env: Env,
    shell: PathBuf,
    bin: PathBuf,
    owners: std::cell::RefCell<BTreeMap<String, std::process::Child>>,
}

mod budget;
mod capacity;
mod keep;
mod message;
mod rest;
mod restart;
mod subscribe;
mod takeover;
mod trust;

fn dummy_agent_process() -> std::process::Child {
    let mut owner = std::process::Command::new("sleep");
    owner.scrub_session_env().arg("30");
    owner.spawn().unwrap()
}

#[test]
fn agent_step_start_launches_with_null_context_and_counts_its_visit() {
    let room = Room::new("");
    let path = room.env.rimz_home().join("workflows/review/workflow.toml");
    fs::write(
        &path,
        fs::read_to_string(&path)
            .unwrap()
            .replace("start = 'pr'", "start = 'build'"),
    )
    .unwrap();
    let definition = success(command(&room.env, &["workflow", "show", "review"]));
    assert!(definition.contains("○ build"), "{definition}");
    assert!(
        definition
            .find("○ build")
            .zip(definition.find("◇ after"))
            .is_some_and(|(build, after)| build < after),
        "{definition}"
    );
    assert!(
        definition.contains("unreachable from build"),
        "{definition}"
    );
    let node = success(command(&room.env, &["workflow", "show", "review", "build"]));
    assert!(node.contains("build · step in review · agent"), "{node}");
    assert!(node.contains("workflow start"), "{node}");

    let (id, run) = room.launch();
    let value = show(&room.env, &id);
    let ctx = context(&value);
    assert_eq!(ctx["node"], "build");
    assert_eq!(ctx["seq"], 1);
    assert_eq!(ctx["visit"], 1);
    assert_eq!(ctx["visits"], 1);
    for field in ["input", "key", "prev_attempt"] {
        assert_eq!(ctx.get(field), Some(&Value::Null), "{ctx:#}");
    }
    assert_eq!(value["record"]["visits"]["build"], 1);
    assert_eq!(run.workflow.as_ref().unwrap().seq, 1);
    assert!(events(&value, "evaluation").is_empty(), "{value:#}");
    room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
    room.hook(&id, &run, json!({"hook_event_name":"Stop"}));
    let value = settled(&room.env, &id, "done");
    assert_eq!(events(&value, "attempt").len(), 1, "{value:#}");
    let history = success(command(&room.env, &["workflow", "show", &id, "--history"]));
    assert!(
        history
            .find("build attempt ended")
            .zip(history.find("after said done"))
            .is_some_and(|(build, after)| build < after),
        "{history}"
    );
}

/// A start the shared trust read refuses exits 1 having created nothing (no run, no supervised
/// run, no pane), and returns its stderr.
fn refused_start(room: &Room, args: &[&str]) -> String {
    let output = room.command(args).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(output.stdout.is_empty(), "{stderr}");
    assert!(
        stderr.contains("workflow start refused; nothing was created:"),
        "{stderr}"
    );
    let store = room.env.store();
    assert!(
        store.paths().workflow_runs().unwrap().is_empty(),
        "{stderr}"
    );
    assert!(
        rimz::harness::run::list(store.paths()).unwrap().is_empty(),
        "{stderr}"
    );
    let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap_or_default();
    assert!(
        !trace.contains("new-tab") && !trace.contains("new-pane"),
        "{trace}"
    );
    stderr
}

#[test]
fn missing_leader_hooks_refuse_the_start_before_anything_is_created() {
    let room = Room::with_kind("", "claude");
    crate::common::trust_claude_project(&room.env, &room.env.project_root);
    fs::remove_file(room.env.agent_config_path("claude")).unwrap();
    let stderr = refused_start(
        &room,
        &["workflow", "run", "review", "--prompt", "Review this"],
    );
    assert!(
        stderr.contains(
            "\n  - claude hooks are missing, so the supervised turn cannot report completion; run `rimz hooks install claude` (blocks build)\n"
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("rimz agents -p"), "{stderr}");
}

#[test]
fn missing_companion_hooks_refuse_the_start_before_anything_is_created() {
    let room = Room::with_kind("", "claude");
    crate::common::trust_claude_project(&room.env, &room.env.project_root);
    fs::create_dir_all(room.env.agent_config_path("codex").parent().unwrap()).unwrap();
    let path = room.env.rimz_home().join("workflows/review/workflow.toml");
    fs::write(
        &path,
        fs::read_to_string(&path)
            .unwrap()
            .replace("agent = 'claude'", "agent = 'claude,codex'"),
    )
    .unwrap();
    let stderr = refused_start(
        &room,
        &["workflow", "run", "review", "--prompt", "Review this"],
    );
    assert!(
        stderr.contains(&format!(
            "\n  - RimZ hooks are missing for codex account `default` at `{}`; run `rimz hooks install codex` (blocks build)\n",
            room.env
                .agent_config_path("codex")
                .parent()
                .unwrap()
                .display()
        )),
        "{stderr}"
    );
}

/// The start reads every judge, one on a route no run takes included, and lists a problem once
/// with every node it blocks; a try at another node reads only that node and its rest check.
#[test]
fn a_judge_on_an_untaken_route_refuses_the_start_but_not_a_try_elsewhere() {
    let room = Room::with_kind("", "claude");
    crate::common::trust_claude_project(&room.env, &room.env.project_root);
    fs::create_dir_all(room.env.agent_config_path("codex").parent().unwrap()).unwrap();
    let path = room.env.rimz_home().join("workflows/review/workflow.toml");
    fs::write(
        &path,
        fs::read_to_string(&path).unwrap().replace(
            "on.done = '@done'\n",
            "on.done = '@done'\non.audit = 'audit'\non.recheck = 'recheck'\n\
             [judge.audit]\nagent='codex'\nsummary='Audits'\nprompt-file='prompt.md'\n\
             timeout='1h'\non={pass='@done',fail='@failed'}\n\
             [judge.recheck]\nagent='codex'\nsummary='Rechecks'\nprompt-file='prompt.md'\n\
             timeout='1h'\non={pass='@done',fail='@failed'}\n",
        ),
    )
    .unwrap();
    let stderr = refused_start(
        &room,
        &["workflow", "run", "review", "--prompt", "Review this"],
    );
    let problem = "codex hooks are missing, so the supervised turn cannot report completion; \
                   run `rimz hooks install codex` (blocks audit, recheck)";
    assert_eq!(
        stderr.matches("codex hooks are missing").count(),
        1,
        "{stderr}"
    );
    assert!(stderr.contains(problem), "{stderr}");
    assert!(!stderr.contains("blocks build"), "{stderr}");
    let tried = room
        .command(&["workflow", "run", "review", "--at", "pr", "--once"])
        .output()
        .unwrap();
    assert!(
        tried.status.success(),
        "{}",
        String::from_utf8_lossy(&tried.stderr)
    );
}

/// The supervised leader refuses a folder with no trust decision at the start, as its launch
/// would; a companion launches past one with a notice
/// (`companion_trust_warns_when_undecided_and_refuses_the_start_otherwise`).
#[test]
fn an_undecided_leader_folder_refuses_the_start() {
    if crate::common::git::git_missing() {
        return;
    }
    let room = Room::with_kind("", "claude");
    crate::common::git::init_repo(&room.env.project_root);
    let stderr = refused_start(
        &room,
        &["workflow", "run", "review", "--prompt", "Review this"],
    );
    assert!(
        stderr.contains("claude has no trust decision and would stop at its trust prompt"),
        "{stderr}"
    );
    assert!(stderr.contains("(blocks build)"), "{stderr}");
}

#[test]
fn budget_park_keeps_every_workflow_lifecycle_live() {
    for lifecycle in [
        "lifecycle='turn'",
        "lifecycle='session'",
        "lifecycle='session'\nrest-check=['after']",
    ] {
        let room = Room::new(&format!("{lifecycle}\n"));
        let (id, run) = room.launch();
        room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
        let store = room.env.store();
        let (parked, wrote) = rimz::harness::run::budget_exceeded(
            store.paths(),
            store.runtime_paths(),
            &run.run_id,
            Some(1.25),
        )
        .unwrap();
        assert!(!wrote, "a budget park must not settle a {lifecycle} run");
        assert_eq!(parked.status, RunStatus::Running);
        success(
            room.command(&["workflow", "advance", &id])
                .output()
                .unwrap(),
        );
        let value = show(&room.env, &id);
        assert!(value["record"]["attempt"].is_object(), "{value:#}");
        assert!(value["record"]["prev_attempt"].is_null());
        let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
        assert!(
            !trace.contains("close-pane"),
            "a parked {lifecycle} keeps its cells: {trace}"
        );
        success(room.command(&["workflow", "cancel", &id]).output().unwrap());
    }
}

#[test]
fn budget_park_pauses_the_turn_timeout_at_the_projected_park_stamp() {
    use rimz::harness::budget::{self, BudgetLedger, BudgetParkStamp};

    let room = Room::new("timeout='1h'\n");
    let (id, run) = room.launch();
    room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
    let state = room.env.state_path_for(&room.env.project_root);
    let bound = rimz::harness::run::load(&state, &run.run_id).unwrap();
    let store = room.env.store();
    let now = jiff::Timestamp::now();
    let since = now - Duration::from_secs(5400);
    let mut ledger = BudgetLedger::new("$1".parse().unwrap());
    ledger.parked = Some(BudgetParkStamp {
        at_cost: 1.25,
        at: since,
    });
    ledger.last_interrupt_at = Some(since);
    budget::write_ledger(
        store.runtime_paths(),
        &run.kind,
        bound.agent_id.as_ref().unwrap(),
        &ledger,
    )
    .unwrap();
    {
        let guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
        let paths = state.workflow(&id.parse().unwrap());
        let mut record = records::load(&paths).unwrap();
        record.attempt.as_mut().unwrap().started_at = now - Duration::from_secs(7200);
        records::write(&paths, &record, &guard).unwrap();
    }
    success(
        room.command(&["workflow", "advance", &id])
            .output()
            .unwrap(),
    );
    let held = show(&room.env, &id);
    assert_eq!(held["record"]["state"], "active", "{held:#}");
    assert_eq!(held["record"]["attempt"]["held_since"], json!(since));
    ledger.parked = None;
    budget::write_ledger(
        store.runtime_paths(),
        &run.kind,
        bound.agent_id.as_ref().unwrap(),
        &ledger,
    )
    .unwrap();
    success(
        room.command(&["workflow", "advance", &id])
            .output()
            .unwrap(),
    );
    let resumed = show(&room.env, &id);
    assert_eq!(resumed["record"]["state"], "active", "{resumed:#}");
    assert!(resumed["record"]["attempt"]["held_since"].is_null());
    assert!(resumed["record"]["attempt"]["asked_ms"].as_u64().unwrap() >= 5_400_000);
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
}

#[test]
fn stop_includes_a_cleared_companion() {
    let room = Room::new("lifecycle='session'\n");
    room.env.install_agent_hooks("claude");
    let path = room.env.rimz_home().join("workflows/review/workflow.toml");
    fs::write(
        &path,
        fs::read_to_string(&path)
            .unwrap()
            .replace("agent = 'codex'", "agent = 'codex,claude'"),
    )
    .unwrap();
    let (id, run) = room.launch();
    let store = room.env.store();
    let snapshot = store.snapshot().unwrap();
    let stamps: Vec<_> = snapshot
        .agents
        .iter()
        .map(|agent| serde_json::to_value(agent).unwrap()["automation"].clone())
        .collect();
    assert_eq!(
        stamps,
        vec![json!({"by": "workflow", "workflow": "review", "step": "build", "depth": 1}); 2]
    );
    let companion = store
        .snapshot()
        .unwrap()
        .agents
        .iter()
        .find(|agent| agent.kind.as_str() == "claude")
        .unwrap()
        .clone();
    let workspace = room.env.resolve_workspace(&room.env.project_root);
    for (session, signal) in [
        ("companion-first", rimz::agents::LifecycleSignal::Registered),
        ("companion-first", rimz::agents::LifecycleSignal::Ended),
        (
            "companion-cleared",
            rimz::agents::LifecycleSignal::Registered,
        ),
    ] {
        let mut observation =
            rimz::agents::AgentLifecycleObservation::new(Some(session.into()), signal);
        observation.agent_name.clone_from(&companion.name);
        observation.pane_id = Some(rimz::PaneId::parse("zellij:terminal_62").unwrap());
        observation.runtime_owner = Some(rimz::pane::RuntimeOwner::new(
            rimz::pane::RuntimeOwnerKind::Agent,
            session,
            room.owner("companion"),
            rimz::proc::process_start_token(room.owner("companion")),
        ));
        store
            .append_event(&rimz::EventEnvelope::agent_lifecycle(
                workspace.workspace_id.clone(),
                &workspace.session_name,
                "claude",
                "test",
                &observation,
            ))
            .unwrap();
    }
    fs::write(room.env.project_root.join("panes.json"), r#"[{"id":62,"is_plugin":false,"tab_id":1,"title":"claude","pane_command":"claude"},{"id":99,"is_plugin":false,"tab_id":1,"title":"unrelated"}]"#).unwrap();
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
    let value = settled(&room.env, &id, "canceled");
    let stopped = value["record"]["prev_attempt"]["children"]["stopped"]
        .as_array()
        .unwrap();
    assert_eq!(
        stopped.len(),
        1,
        "the cleared companion must be stopped even without a registered leader: {value:#}"
    );
    let snapshot = store.runtime_projection(rimz::RuntimeScope::Audit).unwrap();
    let rows = &snapshot.agents;
    assert_eq!(
        rows.iter()
            .find(|agent| agent.agent_id == "companion-cleared")
            .unwrap()
            .workflow,
        run.workflow
    );
    let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
    let closed: Vec<_> = trace
        .lines()
        .filter(|line| line.contains("close-pane"))
        .filter_map(|line| line.split_whitespace().last())
        .collect();
    assert!(closed.contains(&"terminal_62"), "{trace}");
    assert!(!closed.contains(&"terminal_99"), "{trace}");
    assert!(
        rimz::harness::run::load(store.paths(), &run.run_id)
            .unwrap()
            .status
            .is_terminal()
    );
}

#[test]
fn companion_trust_warns_when_undecided_and_refuses_the_start_otherwise() {
    if crate::common::git::git_missing() {
        return;
    }
    let mut results = Vec::new();
    for trust in ["undecided", "restricted", "hooks"] {
        let room = Room::with_kind("", "claude");
        crate::common::git::init_repo(&room.env.project_root);
        success(
            room.command(&["trust", "grant", "--agents", "claude"])
                .output()
                .unwrap(),
        );
        room.env.install_agent_hooks("codex");
        crate::common::write_failing_agent_shim(&room.env, "codex", 1);
        if trust != "hooks" {
            trust_codex_preflight_hooks(&room.env);
        }
        if trust == "restricted" {
            trust_codex_project(&room.env, &room.env.project_root);
            let path = room.env.agent_config_path("codex");
            let mut config: toml::Table = fs::read_to_string(&path).unwrap().parse().unwrap();
            config["projects"][room.env.project_root.to_str().unwrap()]["trust_level"] =
                "untrusted".into();
            fs::write(path, toml::to_string(&config).unwrap()).unwrap();
        }
        let path = room.env.rimz_home().join("workflows/review/workflow.toml");
        fs::write(
            &path,
            fs::read_to_string(&path)
                .unwrap()
                .replace("agent = 'claude'", "agent = 'claude,codex'"),
        )
        .unwrap();
        let receipt = room
            .command(&["workflow", "run", "review"])
            .env("RIMZ_BIN", "/bin/true")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&receipt.stderr).into_owned();
        if trust != "undecided" {
            let refused = !receipt.status.success()
                && stderr.contains("workflow start refused; nothing was created:")
                && stderr.contains(if trust == "hooks" {
                    "untrusted RimZ hooks"
                } else {
                    "saved untrusted"
                })
                && stderr.contains("(blocks build)")
                && room.env.store().paths().workflow_runs().unwrap().is_empty();
            results.push((trust, refused, stderr));
            continue;
        }
        let id = String::from_utf8(receipt.stdout)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_owned();
        let output = room
            .command(&["workflow", "advance", &id])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let value = show(&room.env, &id);
        let correct = value["record"]["attempt"]["run"].is_object()
            && stderr.contains("has no trust decision");
        results.push((trust, correct, format!("{stderr}\n{value:#}")));
    }
    assert!(
        results.iter().all(|(_, correct, _)| *correct),
        "{results:#?}"
    );
}

#[test]
fn a_session_renders_with_you_or_asking_you_until_session_exit() {
    let room = Room::new("lifecycle='session'\n");
    let (id, run) = room.launch();
    assert_eq!(run.lifecycle, rimz::store::run::RunLifecycle::Session);
    room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
    for payload in [
        json!({"hook_event_name":"Stop"}),
        json!({"hook_event_name":"SessionEnd"}),
    ] {
        room.hook(&id, &run, payload);
        success(
            room.command(&["workflow", "advance", &id])
                .output()
                .unwrap(),
        );
        let value = show(&room.env, &id);
        assert_eq!(value["record"]["state"], "active", "{value:#}");
        assert!(value["record"]["prev_attempt"].is_null());
        assert_eq!(value["record"]["attempt"]["lifecycle"], "session");
    }
    let list: Value = serde_json::from_str(&success(
        room.command(&["workflow", "list", "--json"])
            .output()
            .unwrap(),
    ))
    .unwrap();
    let value = show(&room.env, &id);
    assert_eq!(
        list,
        json!([{
            "id":id, "address":"review#main", "workflow":"review",
            "scope":room.env.project_root,
            "state":"active", "node":"build", "since":value["record"]["entered_at"],
            "detail":"with you"
        }])
    );
    assert!(success(room.command(&["workflow", "list"]).output().unwrap()).contains("with you"));
    assert!(
        success(room.command(&["workflow", "show", &id]).output().unwrap())
            .contains("build is with you")
    );
    room.hook(
        &id,
        &run,
        serde_json::from_str(&crate::common::codex_permission_payload()).unwrap(),
    );
    success(
        room.command(&["workflow", "advance", &id])
            .output()
            .unwrap(),
    );
    assert!(show(&room.env, &id)["record"]["attempt"]["asking"].is_null());
    room.expire_permission_review(&run);
    success(
        room.command(&["workflow", "advance", &id])
            .output()
            .unwrap(),
    );
    let asking = show(&room.env, &id);
    assert_eq!(asking["record"]["state"], "active", "{asking:#}");
    assert_eq!(asking["record"]["attempt"]["asking"]["kind"], "permission");
    assert!(success(room.command(&["workflow", "list"]).output().unwrap()).contains("asking you:"));
    let ask = asking["record"]["attempt"]["asking"]["text"]
        .as_str()
        .unwrap();
    for args in [
        vec!["workflow", "show", &id],
        vec!["workflow", "show", &id, "--history"],
    ] {
        let text = success(room.command(&args).output().unwrap());
        assert!(text.contains("build is asking you"), "{text}");
        assert!(
            !text.lines().next().unwrap().contains(&format!("): {ask}")),
            "{text}"
        );
        assert_eq!(
            text.lines().filter(|line| line.trim() == ask).count(),
            1,
            "{text}"
        );
    }
    rimz::harness::run::settle_session_exit(&room.env.store(), &run.run_id, None, || None).unwrap();
    success(
        room.command(&["workflow", "advance", &id])
            .output()
            .unwrap(),
    );
    // The provider exited with its permission open, away from rest: the session died, and with
    // no failure route the run fails.
    let ended = settled(&room.env, &id, "failed");
    assert_eq!(ended["record"]["prev_attempt"]["outcome"], "died");
    assert!(ended["record"]["prev_attempt"]["asked"].is_null());
}

#[test]
fn turn_layout_pins_every_agent_and_wraps_commands() {
    let room = Room::new("");
    room.env.install_agent_hooks("claude");
    let path = room.env.rimz_home().join("workflows/review/workflow.toml");
    fs::write(
        &path,
        fs::read_to_string(&path)
            .unwrap()
            .replace("agent = 'codex'", "agent = 'codex,claude,term'"),
    )
    .unwrap();
    let id = room.start();
    let value = until(&room.env, &id, |v| {
        v["record"]["attempt"]["run"].is_object() || v["record"]["state"] != "active"
    });
    assert!(
        value["record"]["attempt"]["run"].is_object(),
        "layout must launch: {value:#}"
    );
    let state = room.env.state_path_for(&room.env.project_root);
    let runs = rimz::harness::run::list(&state).unwrap();
    assert_eq!(runs.len(), 1, "only the prompt leader owns a run");
    assert_eq!(runs[0].permission_mode, rimz::agents::PermissionMode::Auto);
    let agents = room
        .env
        .store()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    let pinned: Vec<_> = agents
        .iter()
        .filter(|row| {
            row.workflow
                .as_ref()
                .is_some_and(|pin| pin.run.as_str() == id)
        })
        .collect();
    assert_eq!(pinned.len(), 2, "{agents:#?}");
    assert!(pinned.iter().all(|row| row.parent_agent_id.is_none()));
    let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
    for expected in ["run-command", "run codex"] {
        assert!(trace.contains(expected), "missing {expected}: {trace}");
    }
    let payloads: Vec<_> = trace
        .split("\"--request-file\" ")
        .skip(1)
        .map(|rest| {
            serde_json::Deserializer::from_str(rest)
                .into_iter::<String>()
                .next()
                .unwrap()
                .unwrap()
        })
        .collect();
    assert_eq!(payloads.len(), 2);
    for (kind, payload) in ["codex", "claude"].into_iter().zip(payloads) {
        let payload = fs::read_to_string(payload).unwrap();
        let request = rimz::harness::launch::decode_exec_request(kind, None, &payload).unwrap();
        assert_eq!(request.identity.params.workflow, runs[0].workflow);
        assert!(request.identity.params.loop_task.is_none());
        if kind == "codex" {
            assert!(matches!(
                request.reminder_section,
                Some(rimz::harness::launch::ReminderSection::Workflow(_))
            ));
        } else {
            assert!(request.reminder_section.is_none());
        }
    }
}

#[test]
fn signals_leave_step_waits_and_live_attempts_untouched() {
    let room = Room::new("admission.window='5h.left>=100'\n");
    let id = room.start();
    until(&room.env, &id, |value| {
        value["record"]["wait"]["outcome"] == "window"
    });
    let state = room.env.state_path_for(&room.env.project_root);
    let paths = state.workflow(&id.parse().unwrap());
    for outcome in ["window", "takeover", "occupied"] {
        let guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
        let mut record = records::load(&paths).unwrap();
        record.wait.as_mut().unwrap().outcome = outcome.into();
        records::write(&paths, &record, &guard).unwrap();
        drop(guard);
        let before = show(&room.env, &id);
        wakes::wake(&room.env, "review.ready", json!({}));
        let after = show(&room.env, &id);
        assert_eq!(after["journal"], before["journal"], "{outcome}");
        assert_eq!(after["record"], before["record"], "{outcome}");
    }
    let room = Room::new("");
    let id = room.start();
    let _ = room.attempt(&id);
    let before = show(&room.env, &id);
    wakes::wake(&room.env, "review.ready", json!({}));
    let after = show(&room.env, &id);
    assert_eq!(after["journal"], before["journal"]);
    assert_eq!(after["record"], before["record"]);
}

#[test]
fn project_foreach_reads_root_and_tracks_one_visit_per_item() {
    let room = Room::new("");
    fs::write(room.env.project_root.join("items.txt"), "one\ntwo\n").unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["add", "items.txt"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "items",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .current_dir(&room.env.project_root)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let dir = room.env.rimz_home().join("workflows/review");
    fs::write(dir.join("workflow.toml"), "[workflow]\nscope='project'\nstart='pick'\n[script.pick]\ntimeout='365d'\nrun='sh checks/pr.sh'\non.ready='build'\non.none='@done'\n[agent.build]\nagent='codex'\nprompt-file='prompt.md'\nlifecycle='turn'\nthen='pick'\nmax-visits=2\n").unwrap();
    fs::write(dir.join("checks/pr.sh"), "test -d \"$RIMZ_WORKFLOW_DIR/notes\" && test -d \"$RIMZ_WORKFLOW_DIR/signs\" || exit 1\nwhile read -r item; do\n if ! grep -qxF \"$item\" \"$RIMZ_WORKFLOW_DIR\"/notes/*.done 2>/dev/null; then printf '{\"outcome\":\"ready\",\"key\":\"%s\"}' \"$item\"; exit; fi\ndone < items.txt\nprintf '{\"outcome\":\"none\"}'\n").unwrap();
    let id = room.start();
    for item in ["one", "two"] {
        let run = room.attempt(&id);
        let value = show(&room.env, &id);
        assert_eq!(
            value["record"]["scope"],
            json!({"kind":"project", "path":room.env.project_root})
        );
        assert_eq!(value["record"]["key"], item);
        let text = success(room.command(&["workflow", "show", &id]).output().unwrap());
        let visits = if item == "one" { 1 } else { 2 };
        assert!(
            text.contains(&format!("visits  build {visits} of 2 · key {item}: 1\n")),
            "{text}"
        );
        let work = value["dir"].as_str().unwrap();
        let seq = value["record"]["attempt"]["seq"].as_u64().unwrap();
        let note = Path::new(work).join(format!("notes/{seq}.done"));
        fs::write(&note, item).unwrap();
        fs::write(
            room.env.project_root.join("panes.json"),
            r#"[{"id":61,"is_plugin":false,"tab_id":1,"title":"codex","pane_command":"codex"}]"#,
        )
        .unwrap();
        let mut owner = dummy_agent_process();
        room.hook_with_owner(
            &id,
            &run,
            owner.id(),
            json!({"hook_event_name":"SessionStart"}),
        );
        owner.kill().unwrap();
        owner.wait().unwrap();
        fs::write(room.env.project_root.join("panes.json"), "[]").unwrap();
        room.hook_with_owner(&id, &run, owner.id(), json!({"hook_event_name":"Stop"}));
        until(&room.env, &id, |v| {
            v["record"]["attempt"]["seq"].as_u64() != Some(seq)
        });
    }
    let value = settled(&room.env, &id, "done");
    assert_eq!(value["record"]["visits"]["build"], 2, "{value:#}");
    assert_eq!(
        value["record"]["key_visits"],
        json!({"build":[{"key":"one", "visits":1}, {"key":"two", "visits":1}]}),
        "{value:#}"
    );
    let picked: Vec<_> = events(&value, "evaluation")
        .into_iter()
        .filter(|row| row["outcome"] == "ready")
        .map(|row| row["object"]["key"].clone())
        .collect();
    assert_eq!(picked, vec![json!("one"), json!("two")]);
}

#[test]
fn hook_woken_attempt_keeps_the_emitting_shell() {
    let room = Room::new("");
    let definition = room.env.rimz_home().join("workflows/review/workflow.toml");
    let spec = fs::read_to_string(&definition)
        .unwrap()
        .replace("on.done = '@done'", "on.done = 'build'");
    fs::write(definition, spec).unwrap();
    let inner = write_fake_login_shell(&room.env, "inner-workflow-shell", &[]);
    let log = room.env.home_root.join("shell-invocations.log");
    fs::write(
        &room.shell,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> {}\nexec {} \"$@\"\n",
            shlex::try_quote(log.to_str().unwrap()).unwrap(),
            shlex::try_quote(inner.to_str().unwrap()).unwrap(),
        ),
    )
    .unwrap();
    let (id, run) = room.launch();
    let invocations = fs::read_to_string(&log).unwrap();
    assert_eq!(
        invocations.matches("__RIMZ_ENV_PROBE_START__").count(),
        1,
        "the initial attempt probes through the fake shell: {invocations}"
    );
    fs::write(
        room.env.project_root.join("panes.json"),
        r#"[{"id":61,"is_plugin":false,"tab_id":1,"title":"codex","pane_command":"codex"}]"#,
    )
    .unwrap();
    let mut owner = dummy_agent_process();
    room.hook_with_owner(
        &id,
        &run,
        owner.id(),
        json!({"hook_event_name":"SessionStart"}),
    );
    owner.kill().unwrap();
    owner.wait().unwrap();
    fs::write(room.env.project_root.join("panes.json"), "[]").unwrap();
    room.hook_with_owner(&id, &run, owner.id(), json!({"hook_event_name":"Stop"}));
    let value = until(&room.env, &id, |value| {
        (value["record"]["attempt"]["run"].is_object()
            && value["record"]["attempt"]["run"]["run_id"] != run.run_id.as_str())
            || value["record"]["state"] != "active"
    });
    let invocations = fs::read_to_string(&log).unwrap();
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
    assert_eq!(
        invocations.matches("__RIMZ_ENV_PROBE_START__").count(),
        2,
        "the hook-woken attempt must also probe through the emitter's shell: {invocations}\n{value:#}"
    );
    assert!(value["record"]["attempt"]["run"].is_object(), "{value:#}");
}

/// A canceled attempt's leader gets the backstop grace: its pane leaving within the grace
/// is the stop, and nothing closes it.
#[test]
fn a_canceled_attempt_whose_leader_leaves_within_the_grace_is_never_closed() {
    let room = Room::new("");
    let (id, run) = room.launch();
    room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
    let panes = room.env.project_root.join("panes.json");
    let state = room.env.state_path_for(&room.env.project_root);
    let run_id = run.run_id.clone();
    let leaver = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while !rimz::harness::run::load(&state, &run_id)
            .unwrap()
            .status
            .is_terminal()
        {
            assert!(
                std::time::Instant::now() < deadline,
                "the run was never canceled"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // The canceled wrapper leaves well inside the grace, but after a stop that
        // skipped the grace would already have closed its pane.
        std::thread::sleep(Duration::from_millis(500));
        fs::write(&panes, "[]").unwrap();
    });
    let cancel = room.command(&["workflow", "cancel", &id]).output().unwrap();
    leaver.join().unwrap();
    success(cancel);
    let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
    assert!(!trace.contains("close-pane"), "{trace}");
    settled(&room.env, &id, "canceled");
}

/// An attempt from before a rebirth: its agent ended, and the pane id its run saved now hosts
/// another agent. Cancelling the attempt closes nothing of that agent and leaves its process.
#[test]
fn a_canceled_pre_rebirth_attempt_leaves_the_agent_now_in_its_saved_pane() {
    let room = Room::new("");
    let (id, run) = room.launch();
    let mut leader = dummy_agent_process();
    room.hook_with_owner(
        &id,
        &run,
        leader.id(),
        json!({"hook_event_name":"SessionStart"}),
    );
    let state = room.env.state_path_for(&room.env.project_root);
    let pane = rimz::ids::PaneId::from_parts(rimz::ids::MuxName::Zellij, "terminal_61");
    rimz::harness::run::record_pane(&state, &run.run_id, pane.clone()).unwrap();
    leader.kill().unwrap();
    leader.wait().unwrap();
    let workspace = room.env.resolve_workspace(&room.env.project_root);
    let lifecycle = |event: &str, observation: &rimz::agents::AgentLifecycleObservation| {
        room.env
            .store()
            .append_event(&rimz::EventEnvelope::agent_lifecycle(
                workspace.workspace_id.clone(),
                &workspace.session_name,
                run.kind.as_str(),
                event,
                observation,
            ))
            .unwrap();
    };
    lifecycle(
        "SessionEnd",
        &rimz::agents::AgentLifecycleObservation::new(
            Some("workflow-session".into()),
            rimz::agents::LifecycleSignal::Ended,
        ),
    );
    let mut other = dummy_agent_process();
    let mut observation = rimz::agents::AgentLifecycleObservation::new(
        Some("reborn-session".into()),
        rimz::agents::LifecycleSignal::Registered,
    );
    observation.agent_name = Some("reborn".to_owned());
    observation.worktree_path = Some(room.env.project_root.display().to_string());
    observation.pane_id = Some(pane);
    observation.runtime_owner = Some(rimz::store::runtime::process_owner(
        rimz::pane::RuntimeOwnerKind::Agent,
        "reborn-session",
        other.id(),
    ));
    lifecycle("SessionStart", &observation);
    let cancel = room.command(&["workflow", "cancel", &id]).output().unwrap();
    success(cancel);
    let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
    assert!(!trace.contains("close-pane"), "{trace}");
    assert!(
        other.try_wait().unwrap().is_none(),
        "the agent now in the saved pane runs on"
    );
    settled(&room.env, &id, "canceled");
    other.kill().unwrap();
    other.wait().unwrap();
}

#[test]
fn a_turn_step_that_starts_a_run_stays_at_its_step_until_the_report_lands() {
    use super::peers::{deploy, pass, reports, started};
    for detach in [false, true] {
        let room = Room::new("");
        let (id, run) = room.launch();
        room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
        let marker = deploy(&room.env, "child");
        let mut start = room.env.rimz();
        start
            .args(["workflow", "run", "child", "--prompt", "Go"])
            .env(rimz::harness::launch::ENV_AGENT_KIND, "codex")
            .env(
                rimz::harness::launch::ENV_AGENT_ID,
                run.launch_id.as_ref().unwrap().as_str(),
            );
        if detach {
            start.arg("--detach");
        }
        let child = started(start.output().unwrap());
        until(&room.env, &child, |value| {
            value["record"]["wait"].is_object()
        });
        room.hook(
            &id,
            &run,
            json!({"hook_event_name":"Stop","last_assistant_message":"started"}),
        );
        if detach {
            // A detached start holds nothing: the step ends at this turn end.
            settled(&room.env, &id, "done");
            continue;
        }
        let state = room.env.state_path_for(&room.env.project_root);
        let held = || {
            let parked = rimz::harness::run::load(&state, &run.run_id).unwrap();
            let value = show(&room.env, &id);
            assert_eq!(
                (
                    parked.parked_on,
                    &value["record"]["node"],
                    value["record"]["attempt"].is_object()
                ),
                (
                    Some(rimz::store::run::ParkCause::Wait),
                    &json!("build"),
                    true
                ),
                "the step's agent is held at its step"
            );
        };
        held();
        pass(&room.env, &child, &marker);
        let sent = reports(&room.env);
        assert_eq!(sent.len(), 1, "{sent:#?}");
        held();
        // The report lands: it leaves the queue, and the turn it opened ends.
        let workspace = room.env.resolve_workspace(&room.env.project_root);
        assert!(
            room.env
                .store()
                .cancel_message(&sent[0].message_id, &workspace.session_name, "landed")
                .unwrap()
        );
        room.hook(
            &id,
            &run,
            json!({"hook_event_name":"UserPromptSubmit","prompt":sent[0].text}),
        );
        room.hook(
            &id,
            &run,
            json!({"hook_event_name":"Stop","last_assistant_message":"read it"}),
        );
        settled(&room.env, &id, "done");
    }
}

impl Room {
    fn new(extra: &str) -> Self {
        Self::with_kind(extra, "codex")
    }

    fn with_kind(extra: &str, kind: &str) -> Self {
        let lifecycle = if toml::from_str::<toml::Table>(extra)
            .unwrap()
            .contains_key("lifecycle")
        {
            ""
        } else {
            "lifecycle='turn'\n"
        };
        let spec = format!(
            "{HEADER}[script.pr]\ntimeout='365d'\nrun = 'sh checks/pr.sh'\non.go = 'build'\n[agent.build]\nagent = '{kind}'\nprompt-file = 'prompt.md'\nthen = 'after'\nmax-visits = 2\n{lifecycle}{extra}\n[script.after]\ntimeout='365d'\nrun = 'sh checks/pr.sh'\non.done = '@done'\n"
        );
        let (env, _) = fixture(
            &spec,
            "if grep -q '^  \"node\": \"pr\"' \"$RIMZ_WORKFLOW_DIR/context.json\"; then printf '{\"outcome\":\"go\",\"key\":7}'; else printf '{\"outcome\":\"done\"}'; fi\n",
        );
        env.install_agent_hooks(kind);
        if kind == "codex" {
            trust_codex_preflight_hooks(&env);
            trust_codex_project(&env, &env.project_root);
        }
        let bin = crate::common::write_failing_agent_shim(&env, kind, 1);
        let shell = write_fake_login_shell(&env, "workflow-shell", &[]);
        let workspace = env.resolve_workspace(&env.project_root);
        crate::common::room::seed_live_zellij_room(
            &env.runtime_paths(),
            &workspace.session_name,
            Vec::new(),
        );
        fs::write(env.project_root.join("panes.json"), "[]").unwrap();
        Self {
            env,
            shell,
            bin,
            owners: Default::default(),
        }
    }

    fn command(&self, args: &[&str]) -> std::process::Command {
        let mut cmd = self.env.rimz();
        let workspace = self.env.resolve_workspace(&self.env.project_root);
        cmd.args(["--mux", "zellij"])
            .args(args)
            .env("RIMZ_TEST_SWAP_BIND_AS_WRAPPER", "1")
            .env("SHELL", &self.shell)
            .env("PATH", path_with_front(&self.bin))
            .env(
                "RIMZ_ZELLIJ_BIN",
                crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace")),
            )
            .env(
                "RIMZ_TEST_ZELLIJ_LOG",
                self.env.project_root.join("spawn.log"),
            )
            .env("RIMZ_TEST_ZELLIJ_LOG_LAYOUTS", "1")
            .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]")
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                format!("{} [Created 1s ago]\n", workspace.session_name),
            );
        let panes = self.env.project_root.join("panes.json");
        if panes.exists() {
            cmd.env("RIMZ_TEST_ZELLIJ_LIST_PANES_FILE", panes);
        }
        cmd
    }

    fn launch(&self) -> (String, RunRecord) {
        let id = self.start();
        let run = self.attempt(&id);
        (id, run)
    }

    fn start(&self) -> String {
        let receipt = success(
            self.command(&["workflow", "run", "review", "--prompt", "Review this"])
                .output()
                .unwrap(),
        );
        receipt.lines().next().unwrap().to_owned()
    }

    fn attempt(&self, id: &str) -> RunRecord {
        let value = until(&self.env, id, |v| v["record"]["attempt"]["run"].is_object());
        let run_id = value["record"]["attempt"]["run"]["run_id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        rimz::harness::run::load(&self.env.state_path_for(&self.env.project_root), &run_id).unwrap()
    }

    fn hook(&self, id: &str, run: &RunRecord, payload: Value) {
        self.hook_with_owner(id, run, self.env.agent_owner_pid(), payload);
    }

    fn owner(&self, key: &str) -> u32 {
        self.owners
            .borrow_mut()
            .entry(key.to_owned())
            .or_insert_with(|| {
                let mut command = std::process::Command::new("sleep");
                command.scrub_session_env().arg("600").spawn().unwrap()
            })
            .id()
    }

    fn kill_owner(&self, key: &str) {
        let mut owner = self.owners.borrow_mut().remove(key).unwrap();
        owner.kill().unwrap();
        owner.wait().unwrap();
    }

    fn list_pane(&self, id: u64, kind: &str) {
        let path = self.env.project_root.join("panes.json");
        let mut panes: Vec<Value> = fs::read(&path)
            .ok()
            .map(|bytes| serde_json::from_slice(&bytes).unwrap())
            .unwrap_or_default();
        if !panes.iter().any(|pane| pane["id"] == id) {
            panes.push(json!({
                "id":id,"is_plugin":false,"tab_id":1,"title":kind,"pane_command":kind
            }));
            fs::write(path, serde_json::to_vec(&panes).unwrap()).unwrap();
        }
    }

    fn expire_permission_review(&self, run: &RunRecord) {
        let snapshot = self.env.store().snapshot_cached().unwrap();
        let source = snapshot
            .agents
            .iter()
            .find(|agent| agent.name == run.agent_name)
            .unwrap();
        let ask = source.open_ask.as_ref().unwrap();
        let observation = rimz::agents::AgentLifecycleObservation::new(
            Some(source.agent_id.clone()),
            rimz::agents::LifecycleSignal::AwaitingInput {
                extra_questions: 0,
                kind: ask.kind,
                ask_id: Some(ask.id.clone()),
                detail: ask.detail.clone(),
                native_key: ask.native_key.clone(),
            },
        );
        let workspace = self.env.resolve_workspace(&self.env.project_root);
        let mut event = rimz::EventEnvelope::agent_lifecycle(
            workspace.workspace_id,
            workspace.session_name,
            "codex",
            "PermissionRequest",
            &observation,
        );
        event.timestamp = ask.since - Duration::from_secs(60);
        self.env.store().append_event(&event).unwrap();
    }

    fn hook_with_owner(&self, id: &str, run: &RunRecord, owner_pid: u32, payload: Value) {
        self.hook_with_environment(id, run, owner_pid, payload, &[]);
    }

    fn hook_with_environment(
        &self,
        id: &str,
        run: &RunRecord,
        owner_pid: u32,
        payload: Value,
        variables: &[(&str, &std::path::Path)],
    ) {
        self.hook_in_session(id, run, owner_pid, "workflow-session", payload, variables);
    }

    fn hook_in_session(
        &self,
        id: &str,
        run: &RunRecord,
        owner_pid: u32,
        session: &str,
        mut payload: Value,
        variables: &[(&str, &std::path::Path)],
    ) {
        // A real wrapper binds its own owner and pane. The shim launches no wrapper,
        // so default hooks supply a separate owner for each attempt and list its pane.
        if payload["hook_event_name"] == "SessionStart" {
            self.list_pane(61, run.kind.as_str());
        }
        let owner_pid = if owner_pid == self.env.agent_owner_pid() {
            self.owner(run.run_id.as_str())
        } else {
            owner_pid
        };
        payload["session_id"] = json!(session);
        payload["cwd"] = json!(self.env.project_root);
        let mut cmd = self.hook_command(run.kind.as_str());
        cmd.envs(variables.iter().copied());
        cmd.env("RIMZ_AGENT_PID", owner_pid.to_string())
            .env("RIMZ_RUN_ID", run.run_id.as_str())
            .env("RIMZ_AGENT_NAME", run.agent_name.as_deref().unwrap())
            .env("RIMZ_WORKFLOW_RUN", id)
            .env("ZELLIJ", "1")
            .env("ZELLIJ_PANE_ID", "61")
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_PANES",
                r#"[{"id":61,"is_plugin":false,"tab_id":1,"title":"sh"}]"#,
            );
        success(
            self.env
                .spawn_payload(cmd, &payload.to_string())
                .wait_with_output()
                .unwrap(),
        );
        self.env.drain_hooks();
    }

    fn hook_command(&self, kind: &str) -> std::process::Command {
        let mut cmd = self.command(&["hooks", "feed", "--source", kind]);
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .env("RIMZ_AGENT_PID", self.env.agent_owner_pid().to_string());
        cmd
    }
}

impl Drop for Room {
    fn drop(&mut self) {
        for child in self.owners.get_mut().values_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn terminal_write_without_a_hook_wakes_the_workflow() {
    if let Ok(id) = std::env::var("RIMZ_TEST_FAIL_WORKFLOW_ATTEMPT") {
        let fixture_home = PathBuf::from(std::env::var_os("RIMZ_HOME").unwrap());
        let state = rimz::StatePaths::for_project_root_under(
            &std::env::current_dir().unwrap(),
            &fixture_home,
        )
        .unwrap();
        rimz::harness::run::fail_if_nonterminal(
            &state,
            &rimz::RuntimePaths::for_state(&state).unwrap(),
            &id.parse().unwrap(),
            "provider exited without a hook",
        )
        .unwrap()
        .unwrap();
        return;
    }
    let room = Room::new("on.died = 'after'\n");
    let (id, run) = room.launch();
    let mut failure = room.env.rimz_at(&std::env::current_exe().unwrap());
    for (key, value) in room.command(&[]).get_envs() {
        match value {
            Some(value) => {
                failure.env(key, value);
            }
            None => {
                failure.env_remove(key);
            }
        }
    }
    success(
        failure
            .args([
                "--exact",
                "workflow::attempts::terminal_write_without_a_hook_wakes_the_workflow",
            ])
            .env("RIMZ_BIN", room.env.rimz_bin())
            .env("RIMZ_TEST_FAIL_WORKFLOW_ATTEMPT", run.run_id.as_str())
            .output()
            .unwrap(),
    );
    let ended = until(&room.env, &id, |v| v["record"]["state"] == "done");
    assert_eq!(ended["record"]["prev_attempt"]["outcome"], "died");
    let stats = assists_once(&room.env, "workflow_ends", 1);
    assert_eq!(stats["rollup"]["workflow_steps"], 1);
    assert_eq!(stats["rollup"]["workflow_routes"], 3);
    assert_eq!(stats["rollup"]["workflow_ends"], 1);
    let step = stats["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["assist"] == "workflow_step")
        .unwrap();
    assert_eq!(step["run_id"], run.run_id.as_str());
    assert_eq!(step["step"], "build");
    assert_eq!(step["visit"], 1);
}

#[test]
fn show_keeps_durable_history_when_takeover_blockers_are_unavailable() {
    let room = Room::new("admission.window='5h.left>=100'\n");
    let id = room.start();
    until(&room.env, &id, |v| {
        v["record"]["wait"]["outcome"] == "window"
    });
    let state = room.env.state_path_for(&room.env.project_root);
    let paths = state.workflow(&id.parse().unwrap());
    let guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    let mut record = records::load(&paths).unwrap();
    record.wait.as_mut().unwrap().outcome = "takeover".into();
    records::write(&paths, &record, &guard).unwrap();
    drop(guard);
    // Missing git fails owned-worktree discovery, without a pane listing.
    let text = success(
        room.command(&["workflow", "show", &id])
            .env("PATH", "/nonexistent-git-bin")
            .output()
            .unwrap(),
    );
    assert!(text.contains("blockers unavailable"), "{text}");
    assert!(text.contains("trail  "), "{text}");
    let history = success(
        room.command(&["workflow", "show", &id, "--history"])
            .env("PATH", "/nonexistent-git-bin")
            .output()
            .unwrap(),
    );
    assert!(history.contains("pr said go → build"), "{history}");
}

#[test]
fn supervised_identity_prompt_and_lifecycle_completion() {
    let room = Room::new("");
    if crate::common::git::git_missing() {
        return;
    }
    crate::common::git::init_repo(&room.env.project_root);
    let script = room.env.rimz_home().join("workflows/review/checks/pr.sh");
    let original = fs::read_to_string(&script).unwrap();
    fs::write(&script, format!("mkdir -p \"$RIMZ_WORKFLOW_DIR/signs\"\nprintf '%s' 'Use the smaller fix' > \"$RIMZ_WORKFLOW_DIR/signs/build.md\"\n{original}")).unwrap();
    let (id, run) = room.launch();
    let value = show(&room.env, &id);
    assert_eq!(run.workflow.as_ref().unwrap().run.as_str(), id);
    assert_eq!(run.workflow.as_ref().unwrap().seq, 2);
    assert_eq!(context(&value)["visit"], 1);
    assert_eq!(context(&value)["visits"], 1);
    assert_eq!(context(&value)["key"], 7);
    assert_eq!(run.prompt, "Build it\n\nUse the smaller fix\n\nReview this");
    let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
    let payload = trace
        .split("\"--request-file\" ")
        .nth(1)
        .and_then(|rest| {
            serde_json::Deserializer::from_str(rest)
                .into_iter::<String>()
                .next()?
                .ok()
        })
        .unwrap();
    let payload = fs::read_to_string(payload).unwrap();
    let request = rimz::harness::launch::decode_exec_request("codex", None, &payload).unwrap();
    assert_eq!(request.identity.params.workflow, run.workflow);
    assert!(request.identity.params.loop_task.is_none());
    assert!(
        matches!(request.reminder_section, Some(rimz::harness::launch::ReminderSection::Workflow(body)) if body.starts_with("You are one step of a RimZ workflow:"))
    );
    room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
    let snapshot = room.env.store().snapshot_cached().unwrap();
    assert_eq!(
        snapshot
            .agents
            .iter()
            .find(|agent| agent.name == run.agent_name)
            .unwrap()
            .workflow,
        run.workflow
    );
    room.hook(
        &id,
        &run,
        json!({"hook_event_name":"Stop","last_assistant_message":"built"}),
    );
    let value = settled(&room.env, &id, "done");
    assert_eq!(value["record"]["prev_attempt"]["outcome"], "ended");
    assert_eq!(
        value["record"]["prev_attempt"]["run_id"],
        run.run_id.as_str()
    );
    assert_eq!(
        value["record"]["prev_attempt"]["children"],
        json!({"stopped":[],"live":[]})
    );
    let previous = &value["record"]["prev_attempt"];
    assert!(previous["head_start"].as_str().unwrap().len() >= 40);
    assert_eq!(previous["head_start"], previous["head_end"]);
    let attempt = &events(&value, "attempt")[0];
    assert!(attempt["digests"]["prompt.md"].is_string());
    assert!(attempt["digests"]["signs/build.md"].is_string());
    assert_eq!(
        fs::read_to_string(room.env.rimz_home().join("workflows/review/prompt.md")).unwrap(),
        "Build it"
    );
    let human = success(command(&room.env, &["workflow", "show", &id, "--history"]));
    assert!(human.contains(previous["handle"].as_str().unwrap()));
    assert!(human.contains(&previous["head_start"].as_str().unwrap()[..8]));
    assert!(
        human.contains("cost") && !human.contains("stopped (0)") && !human.contains("live (0)"),
        "{human}"
    );
}

#[test]
fn queued_async_question_does_not_end_the_attempt() {
    let room = Room::new("");
    let (id, run) = room.launch();
    room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
    let bound = rimz::harness::run::load(
        &room.env.state_path_for(&room.env.project_root),
        &run.run_id,
    )
    .unwrap();
    let observation = serde_json::from_value(json!({
        "agent_id":bound.agent_id, "signal":{"signal":"tool_used","mutates":false,"edits":false},
        "ask_queue":{"queued":[{"ask_id":rimz::ids::AskId::new(),"native_key":"async","detail":"Later?",
            "question":{"question":"Later?","options":[],"multi_select":false}}]}
    })).unwrap();
    let workspace = room.env.resolve_workspace(&room.env.project_root);
    room.env
        .store()
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            workspace.workspace_id,
            workspace.session_name,
            "codex",
            "test",
            &observation,
        ))
        .unwrap();
    success(
        room.command(&["workflow", "advance", &id])
            .output()
            .unwrap(),
    );
    let current = show(&room.env, &id);
    assert_eq!(
        current["record"]["state"], "active",
        "an async question does not own input"
    );
    assert!(current["record"]["attempt"].is_object());
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
}

#[test]
fn raised_question_is_listed_without_holding_a_workflow_attempt() {
    let room = Room::new("");
    let (id, run) = room.launch();
    room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
    let store = room.env.store();
    let bound = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
    let at = jiff::Timestamp::now();
    let ask_id = rimz::ids::AskId::new();
    rimz::store::raised::raise(
        store.paths(),
        rimz::store::raised::Entry {
            kind: bound.kind,
            agent_id: bound.agent_id.unwrap(),
            value: rimz::agents::state::RaisedAsk {
                ask_id: ask_id.clone(),
                state: rimz::agents::state::RaisedAskState::Open,
                raised_at: at,
                basis_at: at,
                opened_at: Some(at),
                raised_by: None,
                text: Some("Review this?".into()),
                options: vec![],
            },
        },
    )
    .unwrap();
    let asks: Value = serde_json::from_str(&success(
        room.command(&["asks", "--all", "--json"]).output().unwrap(),
    ))
    .unwrap();
    assert!(
        asks.as_array()
            .unwrap()
            .iter()
            .any(|ask| ask["ask_id"] == ask_id.as_str()),
        "a raised question remains actionable while the workflow works: {asks}"
    );
    success(
        room.command(&["workflow", "advance", &id])
            .output()
            .unwrap(),
    );
    let current = show(&room.env, &id);
    assert_eq!(current["record"]["state"], "active", "{current:#}");
    assert!(
        current["record"]["attempt"]["asking"].is_null(),
        "{current:#}"
    );
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
}

#[test]
fn blocking_question_holds_from_its_open_stamp_with_its_question_text() {
    let room = Room::new("");
    let (id, run) = room.launch();
    room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
    room.hook(
        &id,
        &run,
        serde_json::from_str(&crate::common::codex_pre_tool_use_payload()).unwrap(),
    );
    success(
        room.command(&["workflow", "advance", &id])
            .output()
            .unwrap(),
    );
    let value = show(&room.env, &id);
    assert_eq!(value["record"]["state"], "active", "{value:#}");
    let asking = &value["record"]["attempt"]["asking"];
    let snapshot = room.env.store().snapshot_cached().unwrap();
    let source = snapshot
        .agents
        .iter()
        .find(|agent| agent.name == run.agent_name)
        .and_then(|agent| agent.open_ask.as_ref())
        .unwrap();
    assert_eq!(asking["since"], json!(source.since), "{value:#}");
    assert_eq!(asking["kind"], "question", "{value:#}");
    assert_eq!(asking["text"], "which fix shape?", "{value:#}");
    assert_eq!(asking["handle"], "@codex#main", "{value:#}");
    assert_eq!(value["record"]["attempt"]["held_since"], asking["since"]);
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
}

#[test]
fn permission_stays_live_and_explicit_question_routes_with_evidence() {
    for (extra, payload, held) in [
        (
            "",
            serde_json::from_str::<Value>(&crate::common::codex_permission_payload()).unwrap(),
            true,
        ),
        (
            "on.asked = 'after'",
            serde_json::from_str::<Value>(&crate::common::codex_pre_tool_use_payload()).unwrap(),
            false,
        ),
    ] {
        let room = Room::new(extra);
        let (id, run) = room.launch();
        room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
        room.hook(&id, &run, payload);
        if held {
            success(
                room.command(&["workflow", "advance", &id])
                    .output()
                    .unwrap(),
            );
            let current = show(&room.env, &id);
            assert!(
                current["record"]["attempt"]["asking"].is_null(),
                "{current:#}"
            );
            room.expire_permission_review(&run);
        }
        success(
            room.command(&["workflow", "advance", &id])
                .output()
                .unwrap(),
        );
        let value = if held {
            show(&room.env, &id)
        } else {
            settled(&room.env, &id, "done")
        };
        if held {
            assert_eq!(value["record"]["state"], "active", "{value:#}");
            assert!(value["record"]["prev_attempt"].is_null(), "{value:#}");
            assert!(events(&value, "attempt").is_empty(), "{value:#}");
        }
        let asked = if held {
            &value["record"]["attempt"]["asking"]
        } else {
            &value["record"]["prev_attempt"]["asked"]
        };
        assert_eq!(asked["kind"], if held { "permission" } else { "question" });
        assert!(!asked["text"].as_str().unwrap().is_empty());
        assert!(asked["handle"].as_str().unwrap().starts_with('@'));
        if held {
            assert!(asked["since"].is_string(), "{value:#}");
            let snapshot = room.env.store().snapshot_cached().unwrap();
            let source = snapshot
                .agents
                .iter()
                .find(|agent| agent.name == run.agent_name)
                .and_then(|agent| agent.open_ask.as_ref())
                .unwrap();
            assert_eq!(
                asked["since"],
                json!(source.since + std::time::Duration::from_secs(60))
            );
            let list: Value = serde_json::from_str(&success(command(
                &room.env,
                &["workflow", "list", "--json"],
            )))
            .unwrap();
            let detail = list[0]["detail"].as_str().unwrap();
            assert!(
                detail.starts_with("asking you: @") && detail.contains("(permission)"),
                "{list:#}"
            );
            assert!(!detail.contains('\n'), "{detail}");
            let human = success(command(&room.env, &["workflow", "show", &id]));
            assert!(human.contains("build is asking you"), "{human}");
            assert!(human.contains(asked["text"].as_str().unwrap()), "{human}");
            room.hook(&id, &run, json!({"hook_event_name":"Stop"}));
            let ended = settled(&room.env, &id, "done");
            assert_eq!(ended["record"]["prev_attempt"]["outcome"], "ended");
            assert!(ended["record"]["prev_attempt"]["asked"].is_null());
        }
    }
}

#[test]
fn cancel_pending_attempt_is_durable_and_idempotent() {
    let room = Room::new("");
    let (id, run) = room.launch();
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
    let value = settled(&room.env, &id, "canceled");
    memory::assert_history_copy(&room.env, &id);
    assert_eq!(value["record"]["prev_attempt"]["outcome"], "canceled");
    assert_eq!(
        rimz::harness::run::load(
            &room.env.state_path_for(&room.env.project_root),
            &run.run_id
        )
        .unwrap()
        .status,
        RunStatus::Canceled
    );
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
    assert_eq!(show(&room.env, &id)["journal"], value["journal"]);
}

#[test]
fn timeout_still_settles_but_a_failed_turn_waits_for_a_clean_end() {
    for timeout in [true, false] {
        let room = Room::new(if timeout {
            "timeout='1h'\non.timeout='after'\n"
        } else {
            ""
        });
        let (id, run) = room.launch();
        let state = room.env.state_path_for(&room.env.project_root);
        if timeout {
            room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
            let guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
            let paths = state.workflow(&id.parse().unwrap());
            let mut record = records::load(&paths).unwrap();
            record.attempt.as_mut().unwrap().started_at =
                jiff::Timestamp::now() - jiff::SignedDuration::from_hours(2);
            records::write(&paths, &record, &guard).unwrap();
        } else {
            let mut metered = rimz::harness::run::load(&state, &run.run_id).unwrap();
            metered.cost_usd = Some(0.42);
            rimz::harness::run::create(&state, &metered).unwrap();
            room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
            room.hook(
                &id,
                &run,
                json!({"hook_event_name":"Stop", "status":"failed"}),
            );
        }
        let mut advance = room.command(&["workflow", "advance", &id]);
        if timeout {
            advance.env(
                "RIMZ_TEST_ZELLIJ_LIST_PANES",
                r#"[{"id":61,"is_plugin":false,"tab_id":1,"title":"sh"}]"#,
            );
        }
        success(advance.output().unwrap());
        if !timeout {
            let value = show(&room.env, &id);
            assert_eq!(value["record"]["state"], "active", "{value:#}");
            assert!(value["record"]["attempt"].is_object());
            let open = rimz::harness::run::load(&state, &run.run_id).unwrap();
            assert_eq!(open.status, RunStatus::Running);
            assert!(open.stopped_turn.is_some());
            let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
            assert!(!trace.contains("close-pane"), "{trace}");
            room.hook(
                &id,
                &run,
                json!({"hook_event_name":"UserPromptSubmit", "prompt":"Continue"}),
            );
            room.hook(&id, &run, json!({"hook_event_name":"Stop"}));
        }
        let value = settled(&room.env, &id, "done");
        assert_eq!(
            value["record"]["prev_attempt"]["outcome"],
            if timeout { "timeout" } else { "ended" }
        );
        let ended = rimz::harness::run::load(&state, &run.run_id).unwrap();
        assert_eq!(
            ended.status,
            if timeout {
                RunStatus::Canceled
            } else {
                RunStatus::Completed
            }
        );
        if timeout {
            let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
            assert!(trace.contains("close-pane"));
        } else {
            assert_eq!(value["record"]["prev_attempt"]["cost_usd"], 0.42);
            assert!(success(command(&room.env, &["workflow", "show", &id])).contains("$0.42"));
        }
    }
    // With `on.errored`, the leader's own failed turn on an error no wait clears ends the step.
    let room = Room::with_kind("on.errored='after'\n", "claude");
    crate::common::trust_claude_project(&room.env, &room.env.project_root);
    let (id, run) = room.launch();
    room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
    room.hook(
        &id,
        &run,
        json!({"hook_event_name":"UserPromptSubmit", "prompt":"work"}),
    );
    room.hook(
        &id,
        &run,
        json!({"hook_event_name":"StopFailure", "error":"billing_error",
               "last_assistant_message":"Credit balance is too low"}),
    );
    success(
        room.command(&["workflow", "advance", &id])
            .output()
            .unwrap(),
    );
    let value = settled(&room.env, &id, "done");
    let previous = &value["record"]["prev_attempt"];
    assert_eq!(previous["outcome"], "errored", "{value:#}");
    assert_eq!(previous["routed_by"], "go", "{value:#}");
    assert_eq!(previous["errored"]["class"], "not_entitled", "{value:#}");
    let attempt = events(&value, "attempt");
    assert_eq!(attempt.len(), 1, "{attempt:#?}");
    assert_eq!(attempt[0]["outcome"], "errored", "{attempt:#?}");
    assert_eq!(attempt[0]["errored"], previous["errored"], "{attempt:#?}");
    assert!(
        events(&value, "route")
            .iter()
            .any(|route| route["outcome"] == "errored"
                && route["input"]["outcome"] == "errored"
                && route["input"]["errored"] == previous["errored"]
                && route["input"]["key"] == 7),
        "{value:#}"
    );
}

/// `submit` answers only from a judge's own row: a step's agent and a judge's own child are
/// refused with the run record untouched, while the judge itself is accepted.
#[test]
fn submit_refuses_a_steps_agent_and_a_judges_child_leaving_the_record_alone() {
    let submit = |room: &Room, row: &rimz::agents::AgentState| {
        room.command(&["workflow", "submit", "done"])
            .env(rimz::harness::launch::ENV_AGENT_KIND, row.kind.as_str())
            .env(
                rimz::harness::launch::ENV_AGENT_ID,
                row.launch_id.as_ref().unwrap_or(&row.agent_id).as_str(),
            )
            .env(
                rimz::harness::launch::ENV_AGENT_NAME,
                row.name.as_deref().unwrap(),
            )
            .output()
            .unwrap()
    };
    let row = |room: &Room, run: &RunRecord| {
        room.env
            .store()
            .snapshot()
            .unwrap()
            .agents
            .iter()
            .find(|agent| agent.name == run.agent_name && agent.ended_at.is_none())
            .cloned()
            .unwrap()
    };
    let refused = |room: &Room, id: &str, caller: &rimz::agents::AgentState, reason: &str| {
        #[cfg(target_os = "linux")]
        rest::wait_for_advances_to_exit(&room.env, id);
        let record = room
            .env
            .state_path_for(&room.env.project_root)
            .workflow(&id.parse().unwrap())
            .record;
        let before = fs::read(&record).unwrap();
        let output = submit(room, caller);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success() && stderr.contains(reason) && !stderr.contains("accepted"),
            "{stderr}"
        );
        assert_eq!(fs::read(&record).unwrap(), before, "{stderr}");
    };

    let room = Room::new("");
    let (id, run) = room.launch();
    room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
    let step = row(&room, &run);
    assert_eq!(
        step.workflow.as_ref().map(|pin| pin.role),
        Some(rimz::agents::WorkflowAttemptRole::Step)
    );
    refused(
        &room,
        &id,
        &step,
        "error: caller is a workflow step's agent, not a judge; a step ends by finishing its \
         turn\n",
    );

    let room = Room::new("");
    fs::write(
        room.env.rimz_home().join("workflows/review/workflow.toml"),
        format!(
            "{HEADER}[script.pr]\nrun = 'sh checks/pr.sh'\ntimeout = '1h'\non.go = 'build'\n\
             [judge.build]\ntimeout = '1h'\nsummary = 'Review the work'\nagent = 'codex'\n\
             prompt-file = 'prompt.md'\non.done = '@done'\n"
        ),
    )
    .unwrap();
    let (id, run) = room.launch();
    room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
    let judge = row(&room, &run);
    assert_eq!(
        judge.workflow.as_ref().map(|pin| pin.role),
        Some(rimz::agents::WorkflowAttemptRole::Judge)
    );
    // The judge launches a child through `rimz subagents`, whose pane registers by its hooks.
    let store = room.env.store();
    let mut launch = room.command(&["subagents", "codex", "look closer"]);
    launch
        .env(rimz::harness::launch::ENV_AGENT_KIND, judge.kind.as_str())
        .env(
            rimz::harness::launch::ENV_AGENT_ID,
            judge.launch_id.as_ref().unwrap_or(&judge.agent_id).as_str(),
        )
        .env(
            rimz::harness::launch::ENV_AGENT_NAME,
            judge.name.as_deref().unwrap(),
        );
    let output = crate::common::CommandTimeoutExt::bounded_output(&mut launch).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let child_run = rimz::harness::run::list(store.paths())
        .unwrap()
        .into_iter()
        .find(|record| record.subagent)
        .unwrap();
    // A real child's hooks carry its own pane and no workflow run: its launch clears that key.
    room.list_pane(62, "codex");
    let mut hook = room.hook_command("codex");
    hook.env("RIMZ_AGENT_PID", room.owner("child").to_string())
        .env("RIMZ_RUN_ID", child_run.run_id.as_str())
        .env("RIMZ_AGENT_NAME", child_run.agent_name.as_deref().unwrap())
        .env("ZELLIJ", "1")
        .env("ZELLIJ_PANE_ID", "62");
    let payload = json!({"hook_event_name":"SessionStart", "session_id":"child-session",
                         "cwd":room.env.project_root});
    success(
        room.env
            .spawn_payload(hook, &payload.to_string())
            .wait_with_output()
            .unwrap(),
    );
    room.env.drain_hooks();
    let child = row(&room, &child_run);
    assert!(child.parent_is(&judge), "{child:#?}");
    assert_eq!(
        child.workflow, None,
        "a judge's child carries no pin: {child:#?}"
    );
    refused(
        &room,
        &id,
        &child,
        "error: caller is not a workflow judge; rimz workflow submit answers a check from its \
         judge's own pane\n",
    );
    // The same command from the judge's own row is accepted.
    let output = submit(&room, &judge);
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .starts_with(&format!("accepted done for workflow run {id}")),
        "{output:?}"
    );
}

#[test]
fn closed_window_merges_without_visits_then_reopens() {
    use rimz::agents::{AgentRateLimits, RateLimitCacheEntry, RateLimitWindow, RateLimitsCache};
    let room = Room::new("admission.window='5h.left>=42'");
    let capacity = |used| {
        room.env.publish_rate_limits(&RateLimitsCache {
            entries: BTreeMap::from([(
                "codex@default".parse().unwrap(),
                RateLimitCacheEntry {
                    limits: AgentRateLimits {
                        windows: vec![RateLimitWindow {
                            used_percentage: Some(used),
                            duration_mins: Some(300),
                            resets_at: Some(
                                jiff::Timestamp::now() + jiff::SignedDuration::from_hours(2),
                            ),
                            ..Default::default()
                        }],
                    },
                    ..Default::default()
                },
            )]),
            ..Default::default()
        })
    };
    capacity(100);
    let id = room.start();
    until(&room.env, &id, |value| {
        value["record"]["wait"]["outcome"] == "window"
    });
    success(
        room.command(&["workflow", "advance", &id])
            .output()
            .unwrap(),
    );
    let value = show(&room.env, &id);
    assert_eq!(value["record"]["wait"]["count"], 2);
    assert!(value["record"]["visits"].as_object().unwrap().is_empty());
    assert!(
        rimz::harness::run::list(&room.env.state_path_for(&room.env.project_root))
            .unwrap()
            .is_empty()
    );
    assert!(success(command(&room.env, &["workflow", "show", &id])).contains("build window"));
    capacity(0);
    success(
        room.command(&["workflow", "advance", &id])
            .output()
            .unwrap(),
    );
    let _ = room.attempt(&id);
    let value = show(&room.env, &id);
    assert_eq!(events(&value, "wait").len(), 1);
    assert_eq!(events(&value, "wait")[0]["count"], 2);
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
}

#[test]
fn room_rebirth_cancels_root_attempt_and_wakes_its_died_route() {
    use crate::common::CommandTimeoutExt;
    let room = Room::new("on.died = 'after'\n");
    let (id, run) = room.launch();
    let mut owner = dummy_agent_process();
    room.hook_with_owner(
        &id,
        &run,
        owner.id(),
        json!({"hook_event_name":"SessionStart"}),
    );
    owner.kill().unwrap();
    owner.wait().unwrap();
    rimz::store::live_roster::publish(
        &room.env.state_path_for(&room.env.project_root).live_roster,
        [(run.kind.clone(), "workflow-session".into())].into(),
    )
    .unwrap();
    let presence = room.env.home_root.join("presence.wasm");
    fs::write(&presence, b"test-presence").unwrap();
    let _shim_room =
        crate::common::room::ShimRoom::watch(&room.env, &room.env.project_root.join("spawn.log"));
    let start = room
        .command(&["start", "--no-attach", "--no-resume"])
        .env_remove("RIMZ_TEST_ZELLIJ_LIST_PANES_FILE")
        .env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS", "")
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_PANES",
            crate::common::room::MATERIALIZED_ROOM_PANES,
        )
        .env("RIMZ_PRESENCE_PLUGIN", presence)
        .env("RIMZ_PETS_OFFLINE", "1")
        .env("TERM", "dumb")
        .bounded_output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&start.stderr).into_owned();
    assert!(
        !stderr.contains("did not materialize"),
        "the birth must pass the sidebar layout wait: {stderr}"
    );
    success(start);
    let value = settled(&room.env, &id, "done");
    assert_eq!(value["record"]["prev_attempt"]["outcome"], "canceled");
    assert_eq!(
        rimz::harness::run::load(
            &room.env.state_path_for(&room.env.project_root),
            &run.run_id
        )
        .unwrap()
        .status,
        RunStatus::Canceled
    );
}

mod residents;
