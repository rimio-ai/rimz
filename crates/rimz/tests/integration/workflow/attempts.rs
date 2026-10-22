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

fn dummy_agent_process() -> std::process::Child {
    let mut owner = std::process::Command::new("sleep");
    owner.scrub_session_env().arg("30");
    owner.spawn().unwrap()
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

#[test]
fn loop_workflow_creation_duplicate_and_finished_scopes() {
    let room = Room::new("");
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
    assert_eq!(human.matches("review build asked").count(), 1, "{human}");
    assert_eq!(history()["workflow_runs"][0]["id"], id);
    let show = success(room.command(&["loop", "show", "review"]).output().unwrap());
    assert!(show.contains("RUNS") && show.contains(&id), "{show}");
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

    fn hook_with_owner(&self, id: &str, run: &RunRecord, owner_pid: u32, mut payload: Value) {
        payload["session_id"] = json!("workflow-session");
        payload["cwd"] = json!(self.env.project_root);
        let mut cmd = self.hook_command(run.kind.as_str());
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
    assert!(text.contains("history:"), "{text}");
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
        .split("\"--request\" ")
        .nth(1)
        .and_then(|rest| {
            serde_json::Deserializer::from_str(rest)
                .into_iter::<String>()
                .next()?
                .ok()
        })
        .unwrap();
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
    let human = success(command(&room.env, &["workflow", "show", &id]));
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
fn permission_parks_and_question_routes_with_evidence() {
    for (extra, payload, parked) in [
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
        let value = settled(&room.env, &id, if parked { "parked" } else { "done" });
        let asked = &value["record"]["prev_attempt"]["asked"];
        assert_eq!(
            asked["kind"],
            if parked { "permission" } else { "question" }
        );
        assert!(!asked["text"].as_str().unwrap().is_empty());
        assert!(asked["handle"].as_str().unwrap().starts_with('@'));
        if parked {
            assert_eq!(value["record"]["park"]["reason"], "asked");
            let human = success(command(&room.env, &["workflow", "show", &id]));
            assert!(human.contains(asked["text"].as_str().unwrap()));
            assert!(human.contains("permission"));
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
    assert!(
        success(command(&room.env, &["workflow", "show", &id])).contains("waiting: build, window")
    );
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
    room.hook(&id, &run, json!({"hook_event_name":"SessionStart"}));
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
        .env("RIMZ_TEST_ZELLIJ_HEALTH_PROBE_MS", "250")
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
