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
}

mod capacity;
mod until;

fn dummy_agent_process() -> std::process::Child {
    let mut owner = std::process::Command::new("sleep");
    owner.scrub_session_env().arg("30");
    owner.spawn().unwrap()
}

#[test]
fn missing_leader_hooks_refuse_before_runs_or_panes() {
    let room = Room::with_kind("", "claude");
    fs::remove_file(room.env.agent_config_path("claude")).unwrap();
    let id = room.start();
    let value = settled(&room.env, &id, "done");
    let attempts = events(&value, "attempt");
    assert_eq!(attempts.len(), 1, "{value:#}");
    assert_eq!(attempts[0]["outcome"], "died");
    assert_eq!(
        attempts[0]["reason"],
        "`rimz agents -p` requires claude hooks so the supervised turn can report completion; run `rimz hooks install claude`"
    );
    let text = success(command(&room.env, &["workflow", "show", &id, "--history"]));
    assert!(
        text.contains(&format!(
            "reason: {}",
            attempts[0]["reason"].as_str().unwrap()
        )),
        "{text}"
    );
    assert!(
        rimz::harness::run::list(&room.env.state_path_for(&room.env.project_root))
            .unwrap()
            .is_empty()
    );
    let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap_or_default();
    assert!(
        !trace.contains("new-tab") && !trace.contains("new-pane"),
        "{trace}"
    );
}

#[test]
fn missing_companion_hooks_name_the_workflow_before_runs_or_panes() {
    let room = Room::with_kind("", "claude");
    fs::create_dir_all(room.env.agent_config_path("codex").parent().unwrap()).unwrap();
    let path = room.env.rimz_home().join("workflows/review/workflow.toml");
    fs::write(
        &path,
        fs::read_to_string(&path)
            .unwrap()
            .replace("agent = 'claude'", "agent = 'claude,codex'"),
    )
    .unwrap();
    let id = room.start();
    let value = settled(&room.env, &id, "done");
    let attempts = events(&value, "attempt");
    assert_eq!(attempts.len(), 1, "{value:#}");
    assert_eq!(attempts[0]["outcome"], "died");
    assert_eq!(
        attempts[0]["reason"],
        format!(
            "workflow launch refused: RimZ hooks are missing for codex account `default` at `{}`; run `rimz hooks install codex`",
            room.env
                .agent_config_path("codex")
                .parent()
                .unwrap()
                .display()
        )
    );
    assert!(
        rimz::harness::run::list(&room.env.state_path_for(&room.env.project_root))
            .unwrap()
            .is_empty()
    );
    let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap_or_default();
    assert!(
        !trace.contains("new-tab") && !trace.contains("new-pane"),
        "{trace}"
    );
}

#[test]
fn budget_park_leaves_stay_live_but_ends_turn_as_limited() {
    for stay in [true, false] {
        let room = Room::new(if stay { "lifecycle='stay'\n" } else { "" });
        let (id, run) = room.launch();
        room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
        let state = room.env.state_path_for(&room.env.project_root);
        let (parked, wrote) =
            rimz::harness::run::budget_exceeded(&state, &run.run_id, Some(1.25)).unwrap();
        if stay {
            assert!(!wrote, "a budget park must not settle a staying run");
            assert_eq!(parked.status, RunStatus::Running);
        } else {
            assert!(wrote);
            assert_eq!(parked.status, RunStatus::BudgetExceeded);
        }
        success(
            room.command(&["workflow", "advance", &id])
                .output()
                .unwrap(),
        );
        let value = show(&room.env, &id);
        if stay {
            assert!(value["record"]["attempt"].is_object(), "{value:#}");
            assert!(value["record"]["prev_attempt"].is_null());
            let trace = fs::read_to_string(room.env.project_root.join("spawn.log")).unwrap();
            assert!(
                !trace.contains("close-pane"),
                "a parked stay keeps its cells: {trace}"
            );
            success(room.command(&["workflow", "cancel", &id]).output().unwrap());
        } else {
            let value = settled(&room.env, &id, "done");
            assert_eq!(value["record"]["prev_attempt"]["outcome"], "limited");
        }
    }
}

#[test]
fn stop_includes_a_cleared_companion() {
    let room = Room::new("lifecycle='stay'\n");
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
            room.env.agent_owner_pid(),
            rimz::proc::process_start_token(room.env.agent_owner_pid()),
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
    fs::write(room.env.project_root.join("panes.json"), r#"[{"id":62,"is_plugin":false,"tab_id":1,"title":"claude"},{"id":99,"is_plugin":false,"tab_id":1,"title":"unrelated"}]"#).unwrap();
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
    let snapshot = store.snapshot().unwrap();
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
fn companion_trust_warns_when_undecided_and_refuses_as_a_workflow() {
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
        if !receipt.status.success() {
            results.push((trust, false, stderr));
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
        let correct = if trust == "undecided" {
            value["record"]["attempt"]["run"].is_object()
                && stderr.contains("has no trust decision")
        } else {
            let attempts = events(&value, "attempt");
            attempts.iter().any(|attempt| {
                attempt["outcome"] == "died"
                    && attempt["reason"].as_str().is_some_and(|reason| {
                        reason.contains("workflow launch refused:")
                            && reason.contains(if trust == "hooks" {
                                "untrusted RimZ hooks"
                            } else {
                                "saved untrusted"
                            })
                    })
            }) && rimz::harness::run::list(&room.env.state_path_for(&room.env.project_root))
                .unwrap()
                .is_empty()
        };
        results.push((trust, correct, format!("{stderr}\n{value:#}")));
    }
    assert!(
        results.iter().all(|(_, correct, _)| *correct),
        "{results:#?}"
    );
}

#[test]
fn stay_renders_with_you_or_asking_you_until_session_exit() {
    let room = Room::new("lifecycle='stay'\n");
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
        assert_eq!(value["record"]["attempt"]["lifecycle"], "stay");
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
            "id":id, "address":"review#project", "workflow":"review",
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
    let asking = show(&room.env, &id);
    assert_eq!(asking["record"]["state"], "active", "{asking:#}");
    assert_eq!(asking["record"]["attempt"]["asking"]["kind"], "permission");
    assert!(success(room.command(&["workflow", "list"]).output().unwrap()).contains("asking you:"));
    assert!(
        success(room.command(&["workflow", "show", &id]).output().unwrap())
            .contains("build is asking you")
    );
    rimz::harness::run::settle_session_exit(&room.env.store(), &run.run_id, || None).unwrap();
    success(
        room.command(&["workflow", "advance", &id])
            .output()
            .unwrap(),
    );
    let ended = settled(&room.env, &id, "done");
    assert_eq!(ended["record"]["prev_attempt"]["outcome"], "ended");
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
    }
}

#[test]
fn signals_leave_step_waits_and_live_attempts_untouched() {
    let room = Room::new("window='5h.left>=100'\n");
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
    fs::write(dir.join("workflow.toml"), "[workflow]\nscope='project'\nstart='pick'\n[checks.pick]\nrun='sh checks/pr.sh'\non.ready='build'\non.none='@done'\n[steps.build]\nagent='codex'\nprompt-file='prompt.md'\nthen='pick'\nmax-visits=2\n").unwrap();
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
        let work = value["dir"].as_str().unwrap();
        let seq = value["record"]["attempt"]["seq"].as_u64().unwrap();
        let note = Path::new(work).join(format!("notes/{seq}.done"));
        fs::write(&note, item).unwrap();
        fs::write(
            room.env.project_root.join("panes.json"),
            r#"[{"id":61,"is_plugin":false,"tab_id":1,"title":"codex"}]"#,
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
    assert_eq!(
        value["record"]["visits"]["build"]["keys"],
        json!([{"key":"one", "visits":1}, {"key":"two", "visits":1}])
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
        r#"[{"id":61,"is_plugin":false,"tab_id":1,"title":"codex"}]"#,
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

#[test]
fn loop_project_scope_skips_before_reading_an_edited_definition() {
    let room = Room::new("");
    let dir = room.env.rimz_home().join("workflows/review");
    fs::write(
        dir.join("workflow.toml"),
        format!("[workflow]\nscope='project'\nstart='pr'\n{WAIT}"),
    )
    .unwrap();
    fs::write(dir.join("checks/pr.sh"), PENDING).unwrap();
    fs::write(
        room.env.rimz_home().join("loop.toml"),
        format!(
            "[tasks.review]\nworkflow='review'\nroot={:?}\nevery='1h'\n",
            room.env.project_root
        ),
    )
    .unwrap();
    success(room.command(&["loop", "run", "review"]).output().unwrap());
    let state = room.env.state_path_for(&room.env.project_root);
    let runs = records::list(&state).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].scope.kind, records::ScopeKind::Project);
    assert_eq!(runs[0].scope.path, room.env.project_root);
    fs::write(dir.join("workflow.toml"), "not = [toml").unwrap();
    success(room.command(&["loop", "run", "review"]).output().unwrap());
    let history: Value = serde_json::from_str(&success(
        room.command(&["loop", "show", "review", "--json"])
            .output()
            .unwrap(),
    ))
    .unwrap();
    assert!(
        history["runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["result"] == "workflow_skipped" && row["run_id"] == runs[0].id.as_str()),
        "{history}"
    );
    success(
        room.command(&["workflow", "cancel", runs[0].id.as_str()])
            .output()
            .unwrap(),
    );
}

fn condition_workflow(room: &Room, scope: &str, each: bool) {
    let dir = room.env.rimz_home().join("workflows/review");
    fs::write(
        dir.join("workflow.toml"),
        format!("[workflow]\nscope='{scope}'\nstart='pr'\n{WAIT}"),
    )
    .unwrap();
    fs::write(dir.join("checks/pr.sh"), PENDING).unwrap();
    fs::write(
        room.env.rimz_home().join("loop.toml"),
        format!(
            concat!(
                "[tasks.review]\nworkflow='review'\nroot={:?}\n",
                "when=['pr=open']\neach-worktree={}\n"
            ),
            room.env.project_root, each,
        ),
    )
    .unwrap();
}

fn workflow_launches(room: &Room) -> Value {
    let state = room.env.state_path_for(&room.env.project_root);
    let path = state.root.join("records/loop-workflow-launches.json");
    let bytes = fs::read(&path).unwrap_or_default();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

fn workflow_task_history(room: &Room, name: &str) -> Value {
    serde_json::from_str(&success(
        room.command(&["loop", "show", name, "--json"])
            .output()
            .unwrap(),
    ))
    .unwrap()
}

#[test]
fn condition_workflow_finishes_once_and_manual_fire_replaces_the_run() {
    let room = Room::new("");
    condition_workflow(&room, "worktree", false);
    let fire = || success(room.command(&["loop", "run", "review"]).output().unwrap());
    fire();
    let paths = room.env.state_path_for(&room.env.project_root);
    let first = records::list(&paths).unwrap().remove(0);
    let key = room.env.project_root.to_str().unwrap();
    assert_eq!(
        workflow_launches(&room)["tasks"]["review"][key]["run"],
        first.id.as_str()
    );
    let history = workflow_task_history(&room, "review");
    fire();
    assert_eq!(
        workflow_task_history(&room, "review")["runs"],
        history["runs"]
    );
    success(
        room.command(&["workflow", "cancel", first.id.as_str()])
            .output()
            .unwrap(),
    );
    fire();
    assert_eq!(records::list(&paths).unwrap().len(), 1);
    assert_eq!(
        workflow_task_history(&room, "review")["workflow_runs"][0]["state"],
        "canceled"
    );
    assert_eq!(
        workflow_task_history(&room, "review")["runs"],
        history["runs"]
    );
    success(room.command(&["loop", "fire", "review"]).output().unwrap());
    let second = workflow_launches(&room)["tasks"]["review"][key]["run"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(second, first.id.as_str());
    assert_eq!(
        workflow_task_history(&room, "review")["workflow_runs"][0]["id"],
        second
    );
    let list: Value = serde_json::from_str(&success(
        room.command(&["workflow", "list", "--json"])
            .output()
            .unwrap(),
    ))
    .unwrap();
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == second),
        "{list}"
    );
    success(
        room.command(&["loop", "rename", "review", "renamed"])
            .output()
            .unwrap(),
    );
    success(room.command(&["loop", "run", "renamed"]).output().unwrap());
    assert_eq!(records::list(&paths).unwrap().len(), 2);
    assert_eq!(
        workflow_launches(&room)["tasks"]["renamed"][key]["run"],
        second
    );
    assert_eq!(
        workflow_task_history(&room, "renamed")["workflow_runs"][0]["id"],
        second
    );
    let list: Value = serde_json::from_str(&success(
        room.command(&["workflow", "list", "--json"])
            .output()
            .unwrap(),
    ))
    .unwrap();
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == second),
        "{list}"
    );
    assert_eq!(
        records::list(&paths)
            .unwrap()
            .iter()
            .find(|record| record.id.as_str() == second)
            .unwrap()
            .task
            .as_deref(),
        Some("renamed")
    );
    success(
        room.command(&["workflow", "cancel", &second])
            .output()
            .unwrap(),
    );
    success(
        room.command(&["loop", "remove", "renamed"])
            .output()
            .unwrap(),
    );
    assert!(workflow_launches(&room)["tasks"].get("renamed").is_none());
    condition_workflow(&room, "worktree", false);
    fire();
    assert_eq!(records::list(&paths).unwrap().len(), 3);
}

#[test]
fn condition_workflow_readded_while_its_removed_run_lives_starts_fresh_afterward() {
    let room = Room::new("");
    condition_workflow(&room, "worktree", false);
    let fire = || success(room.command(&["loop", "run", "review"]).output().unwrap());
    fire();
    let paths = room.env.state_path_for(&room.env.project_root);
    let old = records::list(&paths).unwrap().remove(0);
    success(
        room.command(&["loop", "remove", "review"])
            .output()
            .unwrap(),
    );
    condition_workflow(&room, "worktree", false);
    fire();
    let history = workflow_task_history(&room, "review");
    assert!(
        history["runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| { row["result"] == "workflow_skipped" && row["run_id"] == old.id.as_str() }),
        "{history}"
    );
    assert!(
        workflow_launches(&room)["tasks"].get("review").is_none(),
        "a re-added task must not adopt the removed task's live run"
    );
    assert_eq!(records::list(&paths).unwrap().len(), 1);
    success(
        room.command(&["workflow", "cancel", old.id.as_str()])
            .output()
            .unwrap(),
    );
    fire();
    let runs = records::list(&paths).unwrap();
    assert_eq!(runs.len(), 2, "{runs:?}");
    let new = runs.iter().find(|run| run.id != old.id).unwrap();
    assert_eq!(new.task.as_deref(), Some("review"));
    assert_eq!(
        workflow_launches(&room)["tasks"]["review"][room.env.project_root.to_str().unwrap()]["run"],
        new.id.as_str()
    );
}

#[test]
fn condition_workflow_repairs_its_own_live_run_once() {
    let room = Room::new("");
    condition_workflow(&room, "worktree", false);
    let fire = || success(room.command(&["loop", "run", "review"]).output().unwrap());
    fire();
    let paths = room.env.state_path_for(&room.env.project_root);
    let id = records::list(&paths).unwrap().remove(0).id;
    let path = paths.root.join("records/loop-workflow-launches.json");
    if path.exists() {
        fs::remove_file(path).unwrap();
    }
    fire();
    let rows = workflow_task_history(&room, "review")["runs"]
        .as_array()
        .unwrap()
        .clone();
    let repaired = workflow_launches(&room)["tasks"]["review"]
        [room.env.project_root.to_str().unwrap()]["run"]
        == id.as_str();
    fire();
    assert!(
        repaired
            && rows
                .iter()
                .filter(|row| row["result"] == "workflow_skipped")
                .count()
                == 1
            && workflow_task_history(&room, "review")["runs"] == json!(rows),
        "repaired={repaired}, rows={rows:?}, after={}",
        workflow_task_history(&room, "review")
    );
}

#[test]
fn condition_workflow_does_not_adopt_a_foreign_run() {
    let room = Room::new("");
    condition_workflow(&room, "worktree", false);
    let foreign = success(
        room.command(&["workflow", "run", "review"])
            .env("RIMZ_BIN", "/bin/true")
            .output()
            .unwrap(),
    );
    let foreign = foreign.lines().next().unwrap();
    success(room.command(&["loop", "run", "review"]).output().unwrap());
    assert!(workflow_launches(&room)["tasks"].get("review").is_none());
    let rows = workflow_task_history(&room, "review");
    assert!(
        rows["runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["result"] == "workflow_skipped" && row["run_id"] == foreign),
        "{rows}"
    );
    success(
        room.command(&["workflow", "cancel", foreign])
            .output()
            .unwrap(),
    );
    success(room.command(&["loop", "run", "review"]).output().unwrap());
    let entry = workflow_launches(&room)["tasks"]["review"]
        [room.env.project_root.to_str().unwrap()]
    .clone();
    assert!(
        entry["run"].as_str().is_some_and(|id| id != foreign),
        "{entry}"
    );
}

#[test]
fn condition_project_workflow_covers_new_checkouts_even_after_finishing() {
    let room = Room::new("");
    if crate::common::git::git_missing() {
        return;
    }
    crate::common::git::init_repo(&room.env.project_root);
    condition_workflow(&room, "project", true);
    success(room.command(&["worktree", "new", "one"]).output().unwrap());
    let first = rimz::worktree::discover_owned(&room.env.project_root)
        .unwrap()
        .remove(0)
        .marker
        .worktree_path;
    let fire = |checkout: &Path| {
        success(
            room.command(&["loop", "run", "review", "--cwd"])
                .arg(checkout)
                .output()
                .unwrap(),
        )
    };
    fire(&first);
    let paths = room.env.state_path_for(&room.env.project_root);
    let id = records::list(&paths).unwrap().remove(0).id;
    let before = workflow_task_history(&room, "review")["runs"].clone();
    success(room.command(&["worktree", "new", "two"]).output().unwrap());
    let second = rimz::worktree::discover_owned(&room.env.project_root)
        .unwrap()
        .into_iter()
        .find(|row| row.marker.worktree_path != first)
        .unwrap()
        .marker
        .worktree_path;
    fire(&second);
    let live_unchanged = workflow_task_history(&room, "review")["runs"] == before;
    success(
        room.command(&["workflow", "cancel", id.as_str()])
            .output()
            .unwrap(),
    );
    fire(&second);
    let after = workflow_task_history(&room, "review");
    assert!(
        live_unchanged && after["runs"] == before && records::list(&paths).unwrap().len() == 1,
        "live_unchanged={live_unchanged}, {after}"
    );
    let entry = workflow_launches(&room)["tasks"]["review"]
        [room.env.project_root.to_str().unwrap()]
    .clone();
    assert_eq!(entry["scope"], "project");
    assert_eq!(entry["run"], id.as_str());
    assert_eq!(after["workflow_runs"].as_array().unwrap().len(), 1);
    for row in after["worktrees"].as_array().unwrap() {
        assert_eq!(row["state"], "launched", "{after}");
        assert_eq!(row["run"], after["workflow_runs"][0]);
    }
    condition_workflow(&room, "worktree", true);
    success(
        room.command(&["loop", "fire", "review"])
            .current_dir(&second)
            .output()
            .unwrap(),
    );
    let entries = workflow_launches(&room)["tasks"]["review"].clone();
    assert!(
        entries
            .get(room.env.project_root.to_str().unwrap())
            .is_none(),
        "{entries}"
    );
    let checkout = rimz::utils::path::normalize_path_lexical(&second);
    assert_eq!(entries[checkout.to_str().unwrap()]["scope"], "worktree");
}

#[test]
fn condition_workflow_manual_fire_from_a_checkout_replaces_only_its_entry() {
    let room = Room::new("");
    if crate::common::git::git_missing() {
        return;
    }
    crate::common::git::init_repo(&room.env.project_root);
    condition_workflow(&room, "worktree", true);
    for name in ["one", "two"] {
        success(room.command(&["worktree", "new", name]).output().unwrap());
    }
    let owned: Vec<_> = rimz::worktree::discover_owned(&room.env.project_root)
        .unwrap()
        .into_iter()
        .map(|row| rimz::utils::path::normalize_path_lexical(&row.marker.worktree_path))
        .collect();
    for checkout in &owned {
        success(
            room.command(&["loop", "run", "review", "--cwd"])
                .arg(checkout)
                .output()
                .unwrap(),
        );
    }
    let first = &owned[0];
    let key = first.to_str().unwrap();
    let other = owned[1].to_str().unwrap();
    let before = workflow_launches(&room);
    assert!(
        before["tasks"]["review"][key]["run"].is_string(),
        "{before}"
    );
    let id = before["tasks"]["review"][key]["run"].as_str().unwrap();
    success(room.command(&["workflow", "cancel", id]).output().unwrap());
    success(
        room.command(&["loop", "run", "review", "--cwd"])
            .arg(first)
            .output()
            .unwrap(),
    );
    assert_eq!(workflow_launches(&room), before);
    let shown = workflow_task_history(&room, "review");
    assert_eq!(shown["launches"], json!({}));
    assert_eq!(shown["workflow_runs"].as_array().unwrap().len(), 2);
    for row in shown["worktrees"].as_array().unwrap() {
        assert_eq!(row["state"], "launched", "{shown}");
        let run = shown["workflow_runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|run| run["id"] == row["run"]["id"])
            .unwrap();
        assert_eq!(&row["run"], run);
    }
    let text = success(room.command(&["loop", "show", "review"]).output().unwrap());
    assert!(
        text.contains("1 canceled") && text.contains("RUN"),
        "{text}"
    );
    assert!(
        !text.contains("\nRUNS\n") && !text.contains("LEADER"),
        "{text}"
    );
    let listed: Value = serde_json::from_str(&success(
        room.command(&["loop", "list", "--json"]).output().unwrap(),
    ))
    .unwrap();
    let task = listed["rooms"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|room| room["tasks"].as_array().unwrap())
        .find(|task| task["name"] == "review")
        .unwrap();
    assert_eq!(task["workflow_runs"].as_array().unwrap().len(), 1, "{task}");
    let listed = success(room.command(&["loop", "list"]).output().unwrap());
    assert!(listed.contains("2 worktrees"), "{listed}");
    success(
        room.command(&["loop", "fire", "review"])
            .current_dir(first)
            .output()
            .unwrap(),
    );
    let after = workflow_launches(&room);
    assert_ne!(after["tasks"]["review"][key]["run"], id);
    assert_eq!(
        after["tasks"]["review"][other],
        before["tasks"]["review"][other]
    );
    let shown = workflow_task_history(&room, "review");
    assert!(
        shown["worktrees"].as_array().unwrap().iter().any(|row| {
            row["checkout"] == key && row["run"]["id"] == after["tasks"]["review"][key]["run"]
        }),
        "{shown}"
    );
}

#[test]
fn condition_workflow_ledger_write_failure_keeps_the_launched_history_and_repairs() {
    let room = Room::new("");
    condition_workflow(&room, "worktree", false);
    let paths = room.env.state_path_for(&room.env.project_root);
    let lock = paths.root.join("locks/loop-workflow-launches.lock");
    fs::create_dir_all(&lock).unwrap();
    success(room.command(&["loop", "run", "review"]).output().unwrap());
    let history = workflow_task_history(&room, "review");
    let rows = history["runs"].as_array().unwrap();
    assert!(
        rows.iter().any(|row| row["result"] == "launched"
            && row["run_id"].is_string()
            && row["error"]
                .as_str()
                .is_some_and(|error| error.contains("loop-workflow-launches.lock"))),
        "{history}"
    );
    fs::remove_dir(lock).unwrap();
    success(room.command(&["loop", "run", "review"]).output().unwrap());
    assert!(
        workflow_launches(&room)["tasks"]["review"][room.env.project_root.to_str().unwrap()]["run"]
            .is_string()
    );
    assert_eq!(records::list(&paths).unwrap().len(), 1);
}

#[test]
fn loop_workflow_creation_duplicate_and_finished_scopes() {
    let room = Room::new("on.asked = '@user'");
    fs::write(
        room.env.rimz_home().join("loop.toml"),
        format!(
            "[tasks.review]\nworkflow='review'\nroot={:?}\nevery='1h'\n",
            room.env.project_root
        ),
    )
    .unwrap();
    let fire = || room.command(&["loop", "run", "review"]).output().unwrap();
    let output = fire();
    assert!(
        output.status.success(),
        "workflow task fires: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let paths = room.env.state_path_for(&room.env.project_root);
    let records = records::list(&paths).unwrap();
    assert_eq!(records.len(), 1);
    let id = records[0].id.to_string();
    assert_eq!(records[0].task.as_deref(), Some("review"));
    let manual = success(room.command(&["loop", "fire", "review"]).output().unwrap());
    assert!(
        manual.contains(&id) && !manual.contains("pane closed"),
        "manual workflow receipt: {manual}"
    );
    success(fire());
    let history = || -> Value {
        serde_json::from_str(&success(
            room.command(&["loop", "show", "review", "--json"])
                .output()
                .unwrap(),
        ))
        .unwrap()
    };
    let rows = history();
    assert!(
        rows["runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["result"] == "launched" && row["run_id"] == id)
    );
    assert!(
        rows["runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["result"] == "workflow_skipped" && row["run_id"] == id)
    );
    // A supported permission prompt parks the active run, and still blocks a duplicate.
    let run = room.attempt(&id);
    room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
    room.hook(
        &id,
        &run,
        serde_json::from_str(&crate::common::codex_permission_payload()).unwrap(),
    );
    until(&room.env, &id, |v| v["record"]["state"] == "parked");
    let list: Value = serde_json::from_str(&success(
        room.command(&["loop", "list", "--json"]).output().unwrap(),
    ))
    .unwrap();
    let task = list["rooms"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|room| room["tasks"].as_array().unwrap())
        .find(|task| task["name"] == "review")
        .unwrap();
    assert_eq!(task["section"], "needs_you", "{task}");
    assert_eq!(task["attention"], "parked");
    assert_eq!(task["workflow_runs"][0]["id"], id);
    assert_eq!(task["workflow_runs"][0]["state"], "parked");
    assert_eq!(task["workflow_runs"][0]["reason"], "asked");
    let human = success(room.command(&["loop", "list"]).output().unwrap());
    assert!(
        human.contains("NEEDS YOU") && human.contains("1 run(s) parked: review"),
        "{human}"
    );
    assert_eq!(
        human.matches("review#project build asked").count(),
        1,
        "{human}"
    );
    assert_eq!(history()["workflow_runs"][0]["id"], id);
    let show = success(room.command(&["loop", "show", "review"]).output().unwrap());
    assert!(
        show.contains("RUNS") && show.contains("review#project"),
        "{show}"
    );
    // The run holding the scope wins before a broken definition is read.
    let skipped = || {
        history()["runs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["result"] == "workflow_skipped")
            .count()
    };
    let before = skipped();
    let spec_file = room.env.rimz_home().join("workflows/review/workflow.toml");
    let spec = fs::read(&spec_file).unwrap();
    fs::write(&spec_file, "not = [toml").unwrap();
    success(fire());
    fs::write(&spec_file, spec).unwrap();
    assert_eq!(records::list(&paths).unwrap().len(), 1);
    assert_eq!(skipped(), before + 1, "{}", history());
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
    success(fire());
    assert_eq!(records::list(&paths).unwrap().len(), 2);
    assert_eq!(assists(&room.env)["rollup"]["workflow_starts"], 2);
}

#[test]
fn loop_workflow_fires_per_owned_checkout_and_rejects_node_fields() {
    let room = Room::new("");
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
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
    fs::write(
        room.env.rimz_home().join("workflows/review/workflow.toml"),
        format!("{HEADER}{WAIT}"),
    )
    .unwrap();
    fs::write(
        room.env.rimz_home().join("workflows/review/checks/pr.sh"),
        PENDING,
    )
    .unwrap();
    for lane in ["one", "two"] {
        success(room.command(&["worktree", "new", lane]).output().unwrap());
    }
    let config = room.env.rimz_home().join("loop.toml");
    let base = format!(
        "[tasks.review]\nworkflow='review'\nroot={:?}\nevery='1h'\n",
        room.env.project_root
    );
    fs::write(&config, format!("{base}each-worktree=true\n")).unwrap();
    let owned = rimz::worktree::discover_owned(&room.env.project_root).unwrap();
    assert_eq!(owned.len(), 2);
    for checkout in &owned {
        let output = room
            .command(&["loop", "run", "review", "--cwd"])
            .arg(&checkout.marker.worktree_path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "per-checkout workflow starts: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let records = records::list(&room.env.state_path_for(&room.env.project_root)).unwrap();
    assert_eq!(records.len(), 2);
    assert_ne!(records[0].scope.path, records[1].scope.path);
    for (field, hint) in [
        ("stay=true", "not a resident agent"),
        ("takeover=true", "[steps.<name>]"),
        (
            "subscribe=[{signal='ci.done'}]",
            "check names what it waits for",
        ),
        ("check='true'", "[checks.<name>]"),
    ] {
        fs::write(&config, format!("{base}{field}\n")).unwrap();
        let output = room.command(&["loop", "run", "review"]).output().unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(hint),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    for run in &records {
        success(
            room.command(&["workflow", "cancel", run.id.as_str()])
                .output()
                .unwrap(),
        );
    }
    fs::write(&config, format!("{base}each-worktree=true\n")).unwrap();
    fs::write(
        room.env.rimz_home().join("workflows/review/workflow.toml"),
        format!("[workflow]\nscope='project'\nstart='pr'\n{WAIT}"),
    )
    .unwrap();
    for checkout in &owned {
        success(
            room.command(&["loop", "run", "review", "--cwd"])
                .arg(&checkout.marker.worktree_path)
                .output()
                .unwrap(),
        );
    }
    let runs = records::list(&room.env.state_path_for(&room.env.project_root)).unwrap();
    let active: Vec<_> = runs.iter().filter(|run| !run.state.is_terminal()).collect();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].scope.kind, records::ScopeKind::Project);
    assert_eq!(active[0].scope.path, room.env.project_root);
    let history: Value = serde_json::from_str(&success(
        room.command(&["loop", "show", "review", "--json"])
            .output()
            .unwrap(),
    ))
    .unwrap();
    assert!(
        history["runs"].as_array().unwrap().iter().any(
            |row| row["result"] == "workflow_skipped" && row["run_id"] == active[0].id.as_str()
        ),
        "{history}"
    );
    success(
        room.command(&["workflow", "cancel", active[0].id.as_str()])
            .output()
            .unwrap(),
    );
}

impl Room {
    fn new(extra: &str) -> Self {
        Self::with_kind(extra, "codex")
    }

    fn with_kind(extra: &str, kind: &str) -> Self {
        let spec = format!(
            "{HEADER}[checks.pr]\nrun = 'sh checks/pr.sh'\non.go = 'build'\n[steps.build]\nagent = '{kind}'\nprompt-file = 'prompt.md'\nthen = 'after'\nmax-visits = 2\n{extra}\n[checks.after]\nrun = 'sh checks/pr.sh'\non.done = '@done'\n"
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
        Self { env, shell, bin }
    }

    fn command(&self, args: &[&str]) -> std::process::Command {
        let mut cmd = self.env.rimz();
        let workspace = self.env.resolve_workspace(&self.env.project_root);
        cmd.args(["--mux", "zellij"])
            .args(args)
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

    fn hook_with_owner(&self, id: &str, run: &RunRecord, owner_pid: u32, payload: Value) {
        self.hook_with_environment(id, run, owner_pid, payload, &[]);
    }

    fn hook_with_environment(
        &self,
        id: &str,
        run: &RunRecord,
        owner_pid: u32,
        mut payload: Value,
        variables: &[(&str, &std::path::Path)],
    ) {
        payload["session_id"] = json!("workflow-session");
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
            &id.parse().unwrap(),
            "provider exited without a hook",
        )
        .unwrap()
        .unwrap();
        return;
    }
    let room = Room::new("");
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
    let room = Room::new("window='5h.left>=100'\n");
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
    assert!(history.contains("evaluation"), "{history}");
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
    assert!(human.contains("cost") && human.contains("stopped (0)") && human.contains("live (0)"));
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
            assert_eq!(asked["since"], json!(source.since));
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
fn timeout_and_failed_run_settle_before_following_then() {
    for timeout in [true, false] {
        let room = Room::new(if timeout { "timeout='1h'" } else { "" });
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
        let value = settled(&room.env, &id, "done");
        assert_eq!(
            value["record"]["prev_attempt"]["outcome"],
            if timeout { "timeout" } else { "died" }
        );
        let ended = rimz::harness::run::load(&state, &run.run_id).unwrap();
        assert_eq!(
            ended.status,
            if timeout {
                RunStatus::Canceled
            } else {
                RunStatus::Failed
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
}

#[test]
fn closed_window_merges_without_visits_then_reopens() {
    use rimz::agents::{AgentRateLimits, RateLimitCacheEntry, RateLimitWindow, RateLimitsCache};
    let room = Room::new("window='5h.left>=42'");
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
fn room_rebirth_cancels_root_attempt_and_wakes_its_next_check() {
    use crate::common::CommandTimeoutExt;
    let room = Room::new("");
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
