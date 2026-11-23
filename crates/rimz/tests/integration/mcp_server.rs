//! `rimz mcp serve` integration tests: the stdio protocol a provider speaks to
//! a spawned server, and the tool calls it runs in its own process.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use rimz::mcp::catalog::Capability;
use rimz::mcp::wording::Wording;

use crate::common::Env;

const REPLY_DEADLINE: Duration = Duration::from_secs(60);

const ALL_TOOLS: [&str; 12] = [
    "lsp",
    "wait",
    "subagent",
    "tasks",
    "join",
    "stop",
    "self_stop",
    "self_compact",
    "self_tune",
    "self_molt",
    "notes",
    "guide",
];

/// The LOAD cell of `tool`'s row in the Tools table of `explain --mcps`,
/// read only between the table's header and its total line, since the
/// tool text printed after it names every tool too.
fn tool_load(out: &str, tool: &str) -> String {
    out.lines()
        .skip_while(|line| !line.trim_start().starts_with("CAPABILITY"))
        .take_while(|line| !line.trim_start().starts_with("total:"))
        .find(|line| line.split_whitespace().nth(1) == Some(tool))
        .and_then(|line| line.split_whitespace().last())
        .unwrap_or_else(|| panic!("no {tool} row in {out}"))
        .to_owned()
}

/// A spawned server, leading its own process group as a provider starts one.
struct McpServer {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    seen: Vec<String>,
    unclaimed: Vec<Value>,
}

impl McpServer {
    fn spawn(mut command: Command) -> Self {
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn rimz mcp serve");
        let stdout = child.stdout.take().expect("server stdout");
        let (tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            stdin: child.stdin.take(),
            child,
            lines,
            seen: Vec::new(),
            unclaimed: Vec::new(),
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn send_raw(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        stdin.write_all(bytes).expect("write request");
        stdin.flush().expect("flush request");
    }

    fn send(&mut self, message: &Value) {
        let mut line = serde_json::to_vec(message).unwrap();
        line.push(b'\n');
        self.send_raw(&line);
    }

    fn next_line(&mut self, deadline: Instant) -> Option<Value> {
        let wait = deadline.saturating_duration_since(Instant::now());
        match self.lines.recv_timeout(wait) {
            Ok(line) => {
                let value = serde_json::from_str(&line)
                    .unwrap_or_else(|err| panic!("stdout line is not JSON ({err}): {line:?}"));
                self.seen.push(line);
                Some(value)
            }
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => None,
        }
    }

    /// The reply to `id`, holding any other reply for a later claim.
    fn reply(&mut self, id: &Value) -> Value {
        if let Some(index) = self.unclaimed.iter().position(|reply| &reply["id"] == id) {
            return self.unclaimed.remove(index);
        }
        let deadline = Instant::now() + REPLY_DEADLINE;
        loop {
            let reply = self
                .next_line(deadline)
                .unwrap_or_else(|| panic!("no reply to {id}; saw {:?}", self.seen));
            if &reply["id"] == id {
                return reply;
            }
            self.unclaimed.push(reply);
        }
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.reply(&json!(id))
    }

    fn initialize(&mut self) -> Value {
        let reply = self.request(
            0,
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"},
            }),
        );
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        reply
    }

    fn start_call(&mut self, id: i64, tool: &str, arguments: Value) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": tool, "arguments": arguments},
        }));
    }

    /// A tool call's result: its text and whether it is an error.
    fn call(&mut self, id: i64, tool: &str, arguments: Value) -> (String, bool) {
        self.start_call(id, tool, arguments);
        tool_result(&self.reply(&json!(id)))
    }

    /// A tool call's answer, read as `tool_answer` reads it.
    fn answer(&mut self, id: i64, tool: &str, arguments: Value) -> (Value, bool) {
        self.start_call(id, tool, arguments);
        tool_answer(tool, &self.reply(&json!(id)))
    }

    /// Wait for the `notifications/tools/list_changed` a replaced server sends
    /// first, which `reply` may already have held while it read for a reply.
    #[cfg(target_os = "linux")]
    fn list_changed(&mut self) {
        let changed = json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"});
        if let Some(index) = self.unclaimed.iter().position(|line| line == &changed) {
            self.unclaimed.remove(index);
            return;
        }
        let line = self.next_line(Instant::now() + REPLY_DEADLINE);
        assert_eq!(line, Some(changed), "saw {:?}", self.seen);
    }

    fn assert_silent_for(&mut self, wait: Duration) {
        if let Some(line) = self.next_line(Instant::now() + wait) {
            panic!("unexpected output: {line}");
        }
    }

    fn wait_exit(&mut self) -> ExitStatus {
        let deadline = Instant::now() + REPLY_DEADLINE;
        loop {
            if let Some(status) = self.child.try_wait().expect("poll server") {
                return status;
            }
            assert!(Instant::now() < deadline, "server did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Close stdin, wait for the exit, and return the status with every line
    /// the server wrote.
    fn close(mut self) -> (ExitStatus, Vec<String>) {
        drop(self.stdin.take());
        let status = self.wait_exit();
        while self
            .next_line(Instant::now() + Duration::from_secs(5))
            .is_some()
        {}
        (status, std::mem::take(&mut self.seen))
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn tool_result(reply: &Value) -> (String, bool) {
    let result = reply
        .get("result")
        .unwrap_or_else(|| panic!("a tool result: {reply}"));
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("text content: {reply}"))
        .to_owned();
    (text, result["isError"] == json!(true))
}

/// A tool call's answer and whether it is a failure: the structured value of a
/// data answer or a failure, which its text item repeats as JSON, or a text
/// answer's text as a string. A data answer fits the output schema its tool
/// lists, and a failure names its error.
fn tool_answer(tool: &str, reply: &Value) -> (Value, bool) {
    let (text, is_error) = tool_result(reply);
    let schema = rimz::mcp::catalog::TOOLS
        .iter()
        .find(|entry| entry.name == tool)
        .and_then(rimz::mcp::catalog::Tool::output_schema);
    let Some(structured) = reply["result"].get("structuredContent") else {
        assert!(!is_error, "a failure carries structured content: {reply}");
        assert!(schema.is_none(), "{tool} lists an output schema: {reply}");
        return (Value::String(text), false);
    };
    let parsed: Value = serde_json::from_str(&text)
        .unwrap_or_else(|err| panic!("the text item is the structured JSON ({err}): {reply}"));
    assert_eq!(&parsed, structured, "{reply}");
    if is_error {
        assert!(structured["error"].is_string(), "{reply}");
        return (parsed, true);
    }
    let schema =
        schema.unwrap_or_else(|| panic!("{tool} answers data without an output schema: {reply}"));
    assert_fits(&schema, &parsed, tool);
    for key in parsed
        .as_object()
        .into_iter()
        .flat_map(serde_json::Map::keys)
    {
        assert!(
            schema["properties"].get(key).is_some(),
            "{tool}'s output schema lacks {key}: {reply}"
        );
    }
    (parsed, false)
}

/// `value` fits `schema` in the keywords the catalog's output schemas use:
/// `type`, `required`, `properties`, and `items`.
fn assert_fits(schema: &Value, value: &Value, at: &str) {
    let kind = match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    let fits = |expected: &Value| expected == kind || (expected == "number" && kind == "integer");
    match &schema["type"] {
        Value::Null => {}
        Value::Array(kinds) => assert!(kinds.iter().any(fits), "{at}: {value} is not {schema}"),
        expected => assert!(fits(expected), "{at}: {value} is not {expected}"),
    }
    for key in schema["required"].as_array().into_iter().flatten() {
        let key = key.as_str().unwrap();
        assert!(value.get(key).is_some(), "{at}: {value} lacks {key}");
    }
    if let (Some(properties), Some(object)) = (schema["properties"].as_object(), value.as_object())
    {
        for (key, property) in properties {
            if let Some(field) = object.get(key) {
                assert_fits(property, field, &format!("{at}.{key}"));
            }
        }
    }
    if let Some(items) = value.as_array().filter(|_| schema.get("items").is_some()) {
        for item in items {
            assert_fits(&schema["items"], item, &format!("{at}[]"));
        }
    }
}

fn serve(env: &Env, args: &[&str]) -> McpServer {
    let mut command = env.rimz();
    command.args(["mcp", "serve"]).args(args);
    McpServer::spawn(command)
}

/// The server as the calling agent of `common::wait` starts it.
fn serve_as_agent(env: &Env) -> McpServer {
    let mut command = crate::common::wait::agent_wait(env);
    command.args(["mcp", "serve"]);
    McpServer::spawn(command)
}

/// The server as `serve_as_agent` starts it, on the tracing Zellij shim listing Zellij
/// terminals `panes`, each running `claude`; its `close-pane` drops the closed one.
fn serve_live(env: &Env, panes: &[u64]) -> McpServer {
    McpServer::spawn(live_server(env, panes))
}

/// The command of [`serve_live`]'s server, for a test that adds to its environment.
fn live_server(env: &Env, panes: &[u64]) -> Command {
    let session = env.resolve_workspace(&env.project_root).session_name;
    let listing = env.project_root.join("stop-panes.json");
    let rows: Vec<Value> = panes
        .iter()
        .map(|id| json!({"id": id, "is_plugin": false, "tab_id": 1, "title": "sh", "pane_command": "claude"}))
        .collect();
    std::fs::write(&listing, serde_json::to_vec(&rows).unwrap()).unwrap();
    let mut command = crate::common::wait::agent_wait(env);
    command
        .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
        .env(
            "RIMZ_TEST_ZELLIJ_LOG",
            env.project_root.join("stop-trace.log"),
        )
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES_FILE", listing)
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{session} [Created 1s ago]\n"),
        )
        .env("RIMZ_TEST_STOP_OWNER_EXIT_MS", "2000")
        .args(["mcp", "serve"]);
    command
}

/// Registers `name` (session `<name>-session`, launch `launch-<name>`) as `launch` says, in
/// Zellij terminal `pane` and owned by a process of its own that a stop can end, and returns
/// that process.
fn live_seat(env: &Env, name: &str, pane: u64, launch: rimz::agents::LaunchParams) -> Child {
    use crate::common::ScrubSessionEnvExt;
    use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};

    let session = format!("{name}-session");
    let process = Command::new("sleep")
        .scrub_session_env()
        .arg("30")
        .spawn()
        .unwrap();
    let workspace = env.resolve_workspace(&env.project_root);
    let mut observation =
        AgentLifecycleObservation::new(Some(session.as_str().into()), LifecycleSignal::Registered);
    observation.agent_name = Some(name.to_owned());
    observation.launch_id = Some(format!("launch-{name}").into());
    observation.worktree_path = Some(env.project_root.display().to_string());
    observation.pane_id = Some(rimz::ids::PaneId::from_parts(
        rimz::ids::MuxName::Zellij,
        format!("terminal_{pane}"),
    ));
    observation.runtime_owner = Some(rimz::store::runtime::process_owner(
        rimz::pane::RuntimeOwnerKind::Agent,
        &session,
        process.id(),
    ));
    observation.launch = rimz::agents::LaunchParams {
        lane: Some(match launch.channel.as_deref() {
            Some(channel) => crate::common::wait::stamped_lane(env, &env.project_root, channel),
            None => rimz::worktree::own_lane(&env.project_root, &env.project_root),
        }),
        ..launch
    };
    env.store()
        .append_event(&rimz::store::event::EventEnvelope::agent_lifecycle(
            workspace.workspace_id,
            workspace.session_name,
            "claude",
            "SessionStart",
            &observation,
        ))
        .unwrap();
    process
}

/// Records the provider's end of session `session`.
fn end_session(env: &Env, session: &str) {
    session_event(
        env,
        session,
        rimz::agents::LifecycleSignal::Ended,
        "SessionEnd",
    );
}

/// Appends `session`'s lifecycle `signal` as its provider's `event` hook would.
fn session_event(env: &Env, session: &str, signal: rimz::agents::LifecycleSignal, event: &str) {
    let observation = rimz::agents::AgentLifecycleObservation::new(Some(session.into()), signal);
    let workspace = env.resolve_workspace(&env.project_root);
    env.store()
        .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
            session_name: &workspace.session_name,
            agent_kind: rimz::ids::AgentKind::new_unchecked("claude"),
            event_name: event,
            observation: &observation,
            spawned_subagents: &[],
        })
        .unwrap();
}

fn launched_by(launch: &str) -> rimz::agents::LaunchParams {
    rimz::agents::LaunchParams {
        launched_by: Some(Box::new(rimz::agents::LaunchedBy {
            kind: rimz::ids::AgentKind::new_unchecked("claude"),
            agent_id: rimz::ids::AgentSessionId::from(launch),
        })),
        ..rimz::agents::LaunchParams::default()
    }
}

fn child_of_caller() -> rimz::agents::LaunchParams {
    rimz::agents::LaunchParams {
        parent_agent_id: Some(rimz::ids::AgentSessionId::from("launch-session")),
        parent_agent_kind: Some(rimz::ids::AgentKind::new_unchecked("claude")),
        launch_depth: Some(1),
        ..rimz::agents::LaunchParams::default()
    }
}

/// A `subagent` call launches the child in the server's own process, in the caller's room and
/// recorded as the caller's child, from a server whose process cwd is outside the room; `tasks`
/// then lists it under the id the launch answered. A subagent's call answers the library's
/// refusal and launches nothing.
#[cfg(unix)]
#[test]
fn a_subagent_call_launches_a_child_of_the_caller_from_outside_the_room() {
    use crate::common::room::{MATERIALIZED_ROOM_PANES, bind_child_panes, seed_live_zellij_room};
    use rimz::agents::LaunchParams;
    use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId};
    use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope};

    let env = Env::new();
    env.install_agent_hooks("codex");
    crate::common::trust_codex_preflight_hooks(&env);
    crate::common::trust_codex_project(&env, &env.project_root);
    env.install_agent_hooks("qwen");
    let agent_bin = crate::common::write_failing_agent_shim(&env, "codex", 1);
    crate::common::write_failing_agent_shim(&env, "qwen", 1);
    let shell = crate::common::write_fake_login_shell(&env, "rimz-test-sh", &[]);
    let workspace = env.resolve_workspace(&env.project_root);
    let store = env.store();
    store.record_workspace(&workspace).expect("record room");
    let seed = |launch: &str, name: &str, pane: &str, launch_params: LaunchParams| {
        store
            .append_event(&EventEnvelope::agent_launched(
                workspace.workspace_id.clone(),
                &workspace.session_name,
                &AgentKind::new_unchecked("claude"),
                AgentLaunchPayload {
                    system_prompt_digest: None,
                    agent_id: AgentSessionId::from(format!("{name}-session")),
                    launch_id: Some(AgentSessionId::from(launch)),
                    agent_name: name.to_owned(),
                    agent_name_explicit: true,
                    launch: launch_params,
                    state: AgentLaunchState::Bound,
                    run_id: None,
                    pane_id: Some(PaneId::from_parts(MuxName::Zellij, pane)),
                    runtime_owner: None,
                    worktree_path: Some(env.project_root.display().to_string()),
                    worktree_branch: None,
                    prompt: None,
                    description: None,
                },
            ))
            .expect("seed an agent");
    };
    seed(
        "parent-launch",
        "parent",
        "terminal_2",
        LaunchParams::default(),
    );
    seed(
        "child-launch",
        "child",
        "terminal_1",
        LaunchParams {
            parent_agent_id: Some(AgentSessionId::from("elsewhere-launch")),
            parent_agent_kind: Some(AgentKind::new_unchecked("claude")),
            launch_depth: Some(1),
            ..LaunchParams::default()
        },
    );
    seed_live_zellij_room(
        store.runtime_paths(),
        &workspace.session_name,
        serde_json::from_str(MATERIALIZED_ROOM_PANES).unwrap(),
    );
    let presence = env.project_root.join("presence.wasm");
    std::fs::write(&presence, b"test-presence").expect("write presence fixture");
    let trace = env.project_root.join("launch-trace.log");
    let outside = tempfile::tempdir().expect("a directory outside the room");
    let serve = |launch: &str| {
        let mut command = env.rimz();
        command
            .current_dir(outside.path())
            .args(["mcp", "serve", "--launch-env"])
            .env("RIMZ_MCP_SERVE", "rimz-subagents")
            .env("RIMZ_MCP_CWD", &env.project_root)
            .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
            .env(rimz::harness::launch::ENV_AGENT_ID, launch)
            .env("SHELL", &shell)
            .env("PATH", crate::common::path_with_front(&agent_bin))
            .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
            .env("RIMZ_TEST_ZELLIJ_LOG", &trace)
            .env("RIMZ_PRESENCE_PLUGIN", &presence)
            .env("ZELLIJ_PANE_ID", "2")
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                format!("{} [Created 1s ago]\n", workspace.session_name),
            )
            .env("RIMZ_TEST_ZELLIJ_LIST_PANES", MATERIALIZED_ROOM_PANES);
        let mut server = McpServer::spawn(command);
        server.initialize();
        server
    };
    let call = json!({"profile": "codex", "prompt": "audit the parser"});

    let mut refused = serve("child-launch");
    let (answer, is_error) = refused.answer(1, "subagent", call.clone());
    assert!(is_error, "{answer}");
    assert_eq!(
        answer,
        json!({"error": "launch refused: subagents cannot launch agents or subagents. Do the work \
            yourself and report the result to your caller; do not retry this command."})
    );
    assert!(
        rimz::harness::run::list(store.paths())
            .expect("list runs")
            .is_empty()
    );

    let mut server = serve("parent-launch");
    let launched = std::sync::atomic::AtomicBool::new(false);
    let (answer, is_error) = std::thread::scope(|scope| {
        scope.spawn(|| bind_child_panes(&store, &trace, &workspace.session_name, &launched));
        let answered = server.answer(1, "subagent", call);
        launched.store(true, std::sync::atomic::Ordering::Relaxed);
        answered
    });
    assert!(!is_error, "{answer}");
    let runs = rimz::harness::run::list(store.paths()).expect("list runs");
    let [run] = runs.as_slice() else {
        panic!("one run: {runs:?}")
    };
    let name = run.agent_name.clone().expect("a minted petname");
    let id = format!("subagent:{name}");
    assert_eq!(answer["id"], json!(id), "{answer}");
    assert_eq!(answer["status"], json!("running"), "{answer}");
    assert!(answer.get("warnings").is_none(), "{answer}");
    assert!(run.subagent, "{run:?}");
    assert_eq!(
        std::fs::canonicalize(&run.worktree_path).unwrap(),
        std::fs::canonicalize(&env.project_root).unwrap()
    );
    let agents = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("launch history")
        .agents;
    let child = agents
        .iter()
        .find(|agent| agent.name.as_deref() == Some(name.as_str()))
        .expect("the child's launch row");
    assert_eq!(
        child.parent_agent_id,
        Some(AgentSessionId::from("parent-launch")),
        "{child:?}"
    );
    let (listed, is_error) = server.answer(2, "tasks", json!({}));
    assert!(!is_error, "{listed}");
    assert_eq!(task_ids(&listed), [id.as_str()], "{listed}");

    let launched = std::sync::atomic::AtomicBool::new(false);
    let (answer, is_error) = std::thread::scope(|scope| {
        scope.spawn(|| bind_child_panes(&store, &trace, &workspace.session_name, &launched));
        let answered = server.answer(
            3,
            "subagent",
            json!({"profile": "qwen", "prompt": "audit the lexer", "effort": "high"}),
        );
        launched.store(true, std::sync::atomic::Ordering::Relaxed);
        answered
    });
    assert!(!is_error, "{answer}");
    assert_eq!(
        answer["warnings"],
        json!(["warning: `qwen` effort high is unsupported on qwen; launching without it"]),
        "{answer}"
    );

    // A timeout past the last representable instant fails after the plan's notice.
    let (answer, is_error) = server.answer(
        4,
        "subagent",
        json!({
            "profile": "qwen",
            "prompt": "audit the lexer",
            "effort": "high",
            "timeout": "100000000h",
        }),
    );
    assert!(is_error, "{answer}");
    assert!(
        answer["error"]
            .as_str()
            .is_some_and(|error| error.starts_with("computing subagent kill deadline")),
        "{answer}"
    );
    assert_eq!(
        answer["warnings"],
        json!(["warning: `qwen` effort high is unsupported on qwen; launching without it"]),
        "{answer}"
    );
    assert_eq!(
        rimz::harness::run::list(store.paths())
            .expect("list runs")
            .len(),
        2,
        "the failed launch starts no run"
    );
}

/// The id of each row a `tasks` answer lists.
fn task_ids(listed: &Value) -> Vec<&str> {
    listed["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect()
}

/// Writes subagent `name`'s running supervised run and returns its id.
fn seed_subagent_run(env: &Env, name: &str) -> rimz::ids::RunId {
    let mut run = rimz::store::run::RunRecord::new(
        env.workspace_id.clone(),
        rimz::ids::AgentKind::new_unchecked("claude"),
        rimz::agents::PermissionMode::Auto,
        "task".to_owned(),
        env.project_root.clone(),
    );
    run.agent_id = Some(format!("{name}-session").into());
    run.agent_name = Some(name.to_owned());
    run.status = rimz::store::run::RunStatus::Running;
    run.subagent = true;
    rimz::harness::run::create(env.store().paths(), &run).unwrap();
    run.run_id
}

fn assert_protocol_lines(lines: &[String]) {
    assert!(!lines.is_empty());
    for line in lines {
        let value: Value = serde_json::from_str(line).expect("protocol line");
        assert_eq!(value["jsonrpc"], "2.0", "{line}");
    }
}

fn notes_written(env: &Env) -> Vec<std::path::PathBuf> {
    fn walk(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.map(Result::unwrap) {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(&env.rimz_home().join("notes"), &mut found);
    found
}

#[test]
fn serve_in_a_terminal_prints_a_note_and_exits() {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    let env = Env::new();
    // The note wins over `--launch-env`'s own early exit too.
    for extra in [&[][..], &["--launch-env"]] {
        let pair = native_pty_system().openpty(PtySize::default()).unwrap();
        let mut command = CommandBuilder::new(env.rimz_bin());
        env.pin_pty_command(&mut command);
        command.cwd(&env.home_root);
        command.args(["mcp", "serve"]);
        command.args(extra);
        assert_terminal_note(pair, command);
    }
}

fn assert_terminal_note(pair: portable_pty::PtyPair, command: portable_pty::CommandBuilder) {
    use std::io::Read;

    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let output = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = reader.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            panic!("rimz mcp serve kept reading a terminal");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "{status:?}");
    let output = output.join().unwrap();
    assert!(
        output.contains("Claude, Codex, and Antigravity launches"),
        "{output}"
    );
    assert!(
        output.contains("rimz agents explain <profile> --mcps"),
        "{output}"
    );
    assert!(output.contains("rimz mcp tools"), "{output}");
    assert!(!output.contains("jsonrpc"), "{output}");
}

/// Under `--launch-env` the server serves what the launch's variables name, with their marks,
/// and runs each call in the launch's directory, not its own.
#[test]
fn launch_env_serves_the_launchs_selection_in_its_directory() {
    let env = Env::new();
    let launched = env.home_root.join("launched-here");
    std::fs::create_dir_all(&launched).unwrap();
    let launched = launched.canonicalize().unwrap();
    let mut command = env.rimz();
    command
        .args(["mcp", "serve", "--launch-env"])
        .env("RIMZ_MCP_SERVE", "rimz-wait:always,rimz-lsp")
        .env("RIMZ_MCP_CWD", &launched);
    let mut server = McpServer::spawn(command);
    server.initialize();
    let tools = server.request(1, "tools/list", json!({}));
    let always = Some(json!({"anthropic/alwaysLoad": true, "antigravity/alwaysLoad": true}));
    assert_eq!(
        tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| (tool["name"].clone(), tool.get("_meta").cloned()))
            .collect::<Vec<_>>(),
        [
            (json!("lsp"), None),
            (json!("wait"), always),
            (json!("tasks"), None),
            (json!("join"), None),
            (json!("stop"), None),
            (json!("guide"), None),
        ]
    );
    let (text, is_error) = server.call(2, "tasks", json!({}));
    assert!(is_error, "{text}");
    assert!(
        text.contains(&format!("no room at {}", launched.display())),
        "{text}"
    );
    server.close();
}

/// Under `--launch-env` with nothing named the server exits 0 before it reads a request,
/// leaving one stderr line for agy's `/mcp` row; a value that does not parse is an error
/// naming its entry, and the flag refuses the argv selection flags.
#[test]
fn launch_env_with_nothing_served_exits_at_once() {
    let env = Env::new();
    for value in [None, Some("")] {
        let mut command = env.rimz();
        command.args(["mcp", "serve", "--launch-env"]);
        if let Some(value) = value {
            command.env("RIMZ_MCP_SERVE", value);
        }
        let mut server = McpServer::spawn(command);
        assert_eq!(server.wait_exit().code(), Some(0), "{value:?}");

        let mut command = env.rimz();
        command.args(["mcp", "serve", "--launch-env"]);
        if let Some(value) = value {
            command.env("RIMZ_MCP_SERVE", value);
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "rimz mcp serve: RimZ serves no tools in this session, as expected in an Antigravity \
             session RimZ did not launch or a launch whose profile serves none\n"
        );
    }

    let output = env
        .rimz()
        .args(["mcp", "serve", "--launch-env"])
        .env("RIMZ_MCP_SERVE", "rimz-wait,github")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "RIMZ_MCP_SERVE entry `github` is not a RimZ capability (rimz-lsp, rimz-wait, \
             rimz-subagents, rimz-control, rimz-notes) with an optional `:always` or `:deferred`, named once"
        ),
        "{stderr}"
    );
    assert!(output.stdout.is_empty(), "{output:?}");

    for flag in ["--only", "--always", "--deferred"] {
        let output = env
            .rimz()
            .args(["mcp", "serve", "--launch-env", flag, "rimz-lsp"])
            .env("RIMZ_MCP_SERVE", "rimz-lsp")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{flag}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("cannot be used with"),
            "{flag}: {output:?}"
        );
    }
}

#[test]
fn marked_capabilities_carry_their_load_mode_in_the_listing() {
    let env = Env::new();
    let meta = |args: &[&str]| {
        let mut server = serve(&env, args);
        server.initialize();
        let tools = server.request(1, "tools/list", json!({}));
        server.close();
        tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| (tool["name"].clone(), tool.get("_meta").cloned()))
            .collect::<Vec<_>>()
    };
    assert!(meta(&[]).iter().all(|(_, meta)| meta.is_none()));
    let always = Some(json!({"anthropic/alwaysLoad": true, "antigravity/alwaysLoad": true}));
    let deferred = Some(json!({"anthropic/alwaysLoad": false, "antigravity/alwaysLoad": false}));
    let only = [
        "--only",
        "rimz-lsp",
        "--only",
        "rimz-wait",
        "--only",
        "rimz-notes",
    ];
    let marks = ["--always", "rimz-lsp", "--deferred", "rimz-wait"];
    assert_eq!(
        meta(&[&only[..], &marks].concat()),
        [
            (json!("lsp"), always),
            (json!("wait"), deferred),
            (json!("tasks"), None),
            (json!("join"), None),
            (json!("stop"), None),
            (json!("notes"), None),
            (json!("guide"), None),
        ]
    );

    let output = env
        .rimz()
        .args([
            "mcp",
            "serve",
            "--always",
            "rimz-lsp",
            "--deferred",
            "rimz-lsp",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("rimz-lsp is marked both --always and --deferred"),
        "{output:?}"
    );
}

#[test]
fn tools_sizes_each_tool_and_json_is_the_servers_listing() {
    let env = Env::new();
    let run = |args: &[&str]| {
        let output = env
            .rimz()
            .args(["mcp", "tools"])
            .args(args)
            .output()
            .unwrap();
        let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
        (output.status, text(&output.stdout), text(&output.stderr))
    };

    let (status, stdout, stderr) = run(&["--json", "--only", "rimz-wait"]);
    assert!(status.success(), "{stderr}");
    let printed: Value = serde_json::from_str(&stdout).expect("tools json");
    let mut server = serve(&env, &["--only", "rimz-wait"]);
    server.initialize();
    let listing = server.request(2, "tools/list", json!({}));
    assert_eq!(printed, listing["result"]);
    server.close();

    let (status, stdout, stderr) = run(&[]);
    assert!(status.success(), "{stderr}");
    let mut sum = 0;
    for name in ALL_TOOLS {
        let line = stdout
            .lines()
            .find(|line| line.split_whitespace().nth(1) == Some(name))
            .unwrap_or_else(|| panic!("{name}: {stdout}"));
        let size = line.split_whitespace().last().unwrap();
        sum += size.strip_prefix('~').unwrap().parse::<u64>().unwrap();
    }
    let total = stdout.lines().last().unwrap();
    assert_eq!(
        total,
        format!("total: ~{sum} tokens in {} tools", ALL_TOOLS.len()),
        "{stdout}"
    );

    let (status, _, stderr) = run(&["--only", "rimz-message"]);
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains(
            "expected one of rimz-lsp, rimz-wait, rimz-subagents, rimz-control, rimz-notes"
        ),
        "{stderr}"
    );
}

#[test]
fn handshake_lists_the_tools_and_answers_only_known_methods() {
    let env = Env::new();
    let mut server = serve(&env, &[]);
    let discover = server.request(1, "server/discover", json!({}));
    assert_eq!(discover["error"]["code"], -32601, "{discover}");

    let init = server.initialize();
    let result = &init["result"];
    assert_eq!(result["protocolVersion"], "2025-06-18", "{init}");
    assert_eq!(
        result["capabilities"],
        json!({"tools": {"listChanged": true}}),
        "{init}"
    );
    assert_eq!(result["serverInfo"]["name"], "rimz", "{init}");
    let instructions = result["instructions"].as_str().expect("instructions");
    let first = instructions.lines().next().expect("a first line");
    assert!(
        !first.is_empty() && first.chars().count() < 250,
        "{first:?}"
    );
    server.assert_silent_for(Duration::from_millis(300));

    let older = server.request(
        2,
        "initialize",
        json!({"protocolVersion": "2024-11-05", "capabilities": {}}),
    );
    assert_eq!(older["result"]["protocolVersion"], "2025-11-25", "{older}");

    assert_eq!(server.request(3, "ping", json!({}))["result"], json!({}));
    let tools = server.request(4, "tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect();
    assert_eq!(names, ALL_TOOLS);
    for tool in tools["result"]["tools"].as_array().unwrap() {
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
        assert!(
            tool["description"]
                .as_str()
                .is_some_and(|text| !text.is_empty())
        );
    }

    let unknown = server.request(5, "resources/list", json!({}));
    assert_eq!(unknown["error"]["code"], -32601, "{unknown}");

    server.send_raw(b"{not json\n");
    let parse = server.reply(&Value::Null);
    assert_eq!(parse["error"]["code"], -32700, "{parse}");
    server.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": 999},
    }));
    assert_eq!(server.request(6, "ping", json!({}))["result"], json!({}));

    let (status, lines) = server.close();
    assert_eq!(status.code(), Some(0), "{status:?}");
    assert_protocol_lines(&lines);
}

#[test]
fn only_serves_the_named_capability() {
    let env = Env::new();
    let mut server = serve(&env, &["--only", "rimz-lsp"]);
    let init = server.initialize();
    assert_eq!(
        init["result"]["instructions"],
        Wording::built_in().instructions(&[Capability::Lsp]),
        "{init}"
    );
    let tools = server.request(1, "tools/list", json!({}));
    assert_eq!(
        tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].clone())
            .collect::<Vec<_>>(),
        [json!("lsp")]
    );
    server.start_call(2, "wait", json!({"trigger": "in", "value": "5m"}));
    let wait = server.reply(&json!(2));
    assert!(wait["error"]["code"].is_i64(), "{wait}");
    server.start_call(3, "no_such_tool", json!({}));
    assert!(server.reply(&json!(3))["error"]["code"].is_i64());
    server.start_call(4, "guide", json!({"name": "rimz-wait/conditions"}));
    let guide = server.reply(&json!(4));
    assert_eq!(guide["error"]["code"], -32602, "{guide}");

    let output = env
        .rimz()
        .args(["mcp", "serve", "--only", "rimz-loop"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}

/// `guide` is served beside a group that has a guide, answers a served name
/// with the guide's text, and any other name with the guides the launch has.
#[test]
fn guide_answers_a_served_guide_and_lists_the_served_guides_for_any_other() {
    let env = Env::new();
    let mut server = serve(&env, &["--only", "rimz-wait"]);
    server.initialize();
    let tools = server.request(1, "tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["wait", "tasks", "join", "stop", "guide"]);

    let (text, is_error) = server.call(2, "guide", json!({"name": "rimz-wait/conditions"}));
    assert!(!is_error, "{text}");
    assert_eq!(
        Ok(text.as_str()),
        Wording::built_in().guide(&[Capability::Wait], "rimz-wait/conditions")
    );
    for (id, name) in [(3, "rimz-wait/none"), (4, "rimz-lsp/conditions")] {
        let (failure, is_error) = server.answer(id, "guide", json!({"name": name}));
        assert!(is_error, "{failure}");
        assert_eq!(
            failure,
            json!({
                "error": format!("no guide {name:?}"),
                "hint": "this launch has: rimz-wait/conditions",
            }),
        );
    }
    let (failure, is_error) = server.answer(5, "guide", json!({}));
    assert!(
        is_error
            && failure["error"]
                .as_str()
                .unwrap()
                .starts_with("invalid arguments: "),
        "{failure}"
    );
    let (status, lines) = server.close();
    assert_eq!(status.code(), Some(0), "{status:?}");
    assert_protocol_lines(&lines);
}

/// A replacement file under the home's `mcps/` reaches `tools/list`,
/// `initialize`, `rimz mcp tools`, and `rimz agents explain --mcps`, which
/// marks it with its path.
#[test]
fn a_replacement_reaches_every_reader_of_the_wording() {
    let env = Env::new();
    let mcps = env.rimz_home().join("mcps/rimz-wait");
    std::fs::create_dir_all(&mcps).unwrap();
    let list = format!(
        "Wait outside your turn, replaced. {}",
        "Each wait, its name. ".repeat(40)
    );
    std::fs::write(mcps.join("wait.md"), format!("{list}\n")).unwrap();
    std::fs::write(mcps.join("instructions.md"), "Use `wait`, replaced.\n").unwrap();

    let mut server = serve(&env, &["--only", "rimz-wait"]);
    let init = server.initialize();
    let instructions = init["result"]["instructions"].as_str().unwrap().to_owned();
    assert_eq!(
        instructions.lines().nth(1),
        Some("Use `wait`, replaced."),
        "{instructions}"
    );
    let listing = server.request(1, "tools/list", json!({}));
    let described = listing["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "wait")
        .map(|tool| tool["description"].clone());
    assert_eq!(described, Some(json!(list.trim())));
    server.close();

    let output = env
        .rimz()
        .args(["mcp", "tools", "--json", "--only", "rimz-wait"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let printed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(printed, listing["result"]);
    let output = env.rimz().args(["mcp", "tools"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let tokens: u64 = stdout
        .lines()
        .find(|line| line.split_whitespace().nth(1) == Some("wait"))
        .and_then(|line| line.split_whitespace().last())
        .and_then(|cell| cell.strip_prefix('~')?.parse().ok())
        .unwrap_or_else(|| panic!("no wait row: {stdout}"));
    assert!(tokens > 150, "{stdout}");
    assert!(output.stderr.is_empty(), "{output:?}");

    crate::common::write_definition(
        &env,
        "agents",
        "cwait",
        "description: cwait profile\nagent: claude\ntools: [Bash]\nmcps: [rimz-wait]",
        "",
    );
    let bin = crate::common::write_failing_agent_shim(&env, "claude", 0);
    let shell = crate::common::write_fake_login_shell(&env, "mcps-wording-sh", &[]);
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = env
        .rimz()
        .args(["agents", "explain", "cwait", "--mcps"])
        .env("SHELL", shell)
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let out = String::from_utf8(output.stdout).unwrap();
    assert!(out.contains(&instructions), "{out}");
    assert!(out.contains(list.trim()), "{out}");
    assert!(
        out.contains(&format!(
            " · text {})\n",
            mcps.join("instructions.md").display()
        )),
        "{out}"
    );
    assert!(
        out.contains(&format!(
            "· rimz-wait · load default · text {}\n",
            mcps.join("wait.md").display()
        )),
        "{out}"
    );
    assert!(out.contains("## wait\n~"), "{out}");
    assert_eq!(out.matches(" · text ").count(), 2, "{out}");
}

/// A launch whose home holds a rejected or misplaced wording file carries a
/// warning for each on its plan, which `rimz agents explain` prints; a clean
/// home carries none.
#[test]
fn a_bad_wording_file_warns_on_the_launch_plan() {
    let env = Env::new();
    crate::common::write_definition(
        &env,
        "agents",
        "cwait",
        "description: cwait profile\nagent: claude\ntools: [Bash]\nmcps: [rimz-wait]",
        "",
    );
    let explain = || {
        let bin = crate::common::write_failing_agent_shim(&env, "claude", 0);
        let shell = crate::common::write_fake_login_shell(&env, "mcps-wording-sh", &[]);
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = env
            .rimz()
            .args(["agents", "explain", "cwait"])
            .env("SHELL", shell)
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stderr).unwrap()
    };
    let mcps = env.rimz_home().join("mcps");
    assert!(!explain().contains("mcps"), "{}", explain());

    std::fs::create_dir_all(mcps.join("rimz-wait")).unwrap();
    std::fs::create_dir_all(mcps.join("rimz-lsp")).unwrap();
    std::fs::write(mcps.join("rimz-wait/instructions.md"), " \n").unwrap();
    std::fs::write(mcps.join("rimz-lsp/wait.md"), "Misplaced.\n").unwrap();
    std::fs::write(mcps.join("rimz-lsp/lsp.md"), "\n").unwrap();
    let stderr = explain();
    let rejected = format!(
        "rimz: {} is not used, so the built-in text is served: it is empty\n",
        mcps.join("rimz-wait/instructions.md").display()
    );
    let misplaced = format!(
        "rimz: {} is ignored: RimZ has no wording file rimz-lsp/wait.md; it belongs at {}\n",
        mcps.join("rimz-lsp/wait.md").display(),
        mcps.join("rimz-wait/wait.md").display()
    );
    assert!(stderr.contains(&rejected), "{stderr}");
    assert!(stderr.contains(&misplaced), "{stderr}");
    assert!(
        !stderr.contains("rimz-lsp/lsp.md"),
        "a group the launch does not serve: {stderr}"
    );

    let output = env.rimz().args(["mcp", "tools"]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("rimz-lsp/lsp.md is not used"), "{stderr}");
}

#[cfg(unix)]
#[test]
fn signals_end_the_server_cleanly() {
    let env = Env::new();
    for signal in [
        nix::sys::signal::Signal::SIGINT,
        nix::sys::signal::Signal::SIGTERM,
    ] {
        let mut server = serve(&env, &[]);
        server.initialize();
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(server.pid() as i32), signal).unwrap();
        assert_eq!(server.wait_exit().code(), Some(0), "{signal}");
    }
}

/// The body, a title and an LSP target that begin with `--`, and the meta
/// keys stay data; a warning the command would print is a field of the answer,
/// and a call that cannot do its job answers a failure naming why.
#[test]
fn notes_keeps_the_body_bytes_and_the_server_keeps_parsing() {
    let env = Env::new();
    let mut server = serve(&env, &[]);
    server.initialize();
    let body = "say \"hi\" and 'bye'\n$(touch pwned) `id`\n\n\\n not a newline\n";
    let (added, is_error) = server.answer(
        1,
        "notes",
        json!({
            "kind": "handoff",
            "title": "--force",
            "body": body,
            "meta": {"author": "mallory", "ticket": "42"},
        }),
    );
    assert!(!is_error, "{added}");
    assert_eq!(added["dropped_meta"], json!(["author"]), "{added}");
    let path = std::path::PathBuf::from(added["path"].as_str().expect("the note's path"));
    let note = std::fs::read_to_string(&path).expect("read note");
    assert!(note.ends_with(&format!("\n---\n{body}")), "{note}");
    assert!(note.contains("title: \"--force\"\n"), "{note}");
    assert!(note.contains("ticket: \"42\"\n"), "{note}");
    assert!(!env.project_root.join("pwned").exists());
    assert_eq!(server.request(2, "ping", json!({}))["result"], json!({}));

    let (failure, is_error) = server.answer(3, "notes", json!({"kind": "log", "title": 7}));
    assert!(is_error, "{failure}");
    assert!(
        failure["error"]
            .as_str()
            .unwrap()
            .starts_with("invalid arguments: "),
        "{failure}"
    );
    let (failure, is_error) = server.answer(
        4,
        "lsp",
        json!({"verb": "find", "targets": ["x"], "cwd": "/no/such/dir"}),
    );
    assert!(is_error, "{failure}");
    assert_eq!(notes_written(&env), [path]);

    let (failure, is_error) =
        server.answer(5, "lsp", json!({"verb": "find", "targets": ["--help"]}));
    assert!(is_error, "{failure}");
    let error = failure["error"].as_str().unwrap();
    assert!(error.contains("no language server"), "{failure}");
    assert!(!error.contains("Usage:"), "{failure}");

    let (failure, is_error) = server.answer(6, "wait", json!({"trigger": "pid", "value": "one"}));
    assert!(is_error, "{failure}");
    assert_eq!(
        failure["error"],
        json!("invalid pid `one`: a process id from 1 to 2147483647"),
        "{failure}"
    );

    let (_, lines) = server.close();
    assert_protocol_lines(&lines);
}

/// A call as an agent in a test room resolves the agent's home, room and
/// caller: the wait it arms is its own, and the note it writes names it.
#[test]
fn a_call_as_an_agent_resolves_its_home_room_and_caller() {
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent(&env);
    let mut server = serve_as_agent(&env);
    server.initialize();
    let (start, is_error) = server.answer(
        1,
        "wait",
        json!({"trigger": "in", "value": "1h", "label": "via mcp"}),
    );
    assert!(!is_error, "{start}");
    let (listed, is_error) = server.answer(2, "tasks", json!({}));
    assert!(!is_error, "{listed}");
    let rows = listed["tasks"].as_array().expect("the task rows");
    let [row] = rows.as_slice() else {
        panic!("one task: {listed}");
    };
    assert_eq!(row["about"], json!("via mcp"), "{listed}");
    let id = row["id"].as_str().expect("the wait's id");
    assert!(id.starts_with("wait:"), "{listed}");
    assert_eq!(
        start,
        json!({
            "id": id,
            "status": "running",
            "stop": format!("stop({{id: \"{id}\"}})"),
            "report": "a message to you when the wait fires",
        })
    );
    let (added, is_error) = server.answer(
        3,
        "notes",
        json!({"kind": "diary", "title": "Day", "body": "text\n"}),
    );
    assert!(!is_error, "{added}");
    let path = added["path"].as_str().unwrap();
    let note = std::fs::read_to_string(path).expect("read note");
    assert!(note.contains("author: \"planner\"\n"), "{note}");
    assert!(
        path.starts_with(&env.rimz_home().display().to_string()),
        "{path}"
    );
}

/// A `wait` label is one line of 1 to 60 characters, trimmed, as `rimz wait
/// --label` takes it: any other label fails the call and arms nothing.
#[test]
fn a_wait_label_is_one_trimmed_line_as_the_command_takes_it() {
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent(&env);
    let mut server = serve_as_agent(&env);
    server.initialize();
    let long = "x".repeat(61);
    for (id, label) in [(1, "first\nsecond"), (2, long.as_str()), (3, "   ")] {
        let (failure, is_error) = server.answer(
            id,
            "wait",
            json!({"trigger": "in", "value": "1h", "label": label}),
        );
        assert!(is_error, "{failure}");
        assert_eq!(
            failure["error"],
            json!(format!(
                "invalid label {label:?}: label must be one line of 1 to 60 characters"
            )),
            "{failure}"
        );
    }
    let (listed, _) = server.answer(4, "tasks", json!({}));
    assert_eq!(listed["tasks"], json!([]), "a refused label arms nothing");
    let (start, is_error) = server.answer(
        5,
        "wait",
        json!({"trigger": "in", "value": "1h", "label": "  fix it  "}),
    );
    assert!(!is_error, "{start}");
    let (listed, _) = server.answer(6, "tasks", json!({}));
    assert_eq!(listed["tasks"][0]["about"], json!("fix it"), "{listed}");
}

/// A booked `self_stop` answers what holds the stop, as the command's receipt
/// prints it; the control tools answer data for the inputs the command's
/// flags took (`off`, a tuned value), and `stop` ends the wait by its id.
#[test]
fn self_stop_answers_what_holds_it_and_control_calls_answer_data() {
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent(&env);
    let mut server = serve_as_agent(&env);
    server.initialize();
    let (start, is_error) = server.answer(1, "wait", json!({"trigger": "in", "value": "2h"}));
    assert!(!is_error, "{start}");
    let id = start["id"].as_str().unwrap();
    let name = id.trim_start_matches("wait:");
    let (booked, is_error) = server.answer(2, "self_stop", json!({}));
    assert!(!is_error, "{booked}");
    assert_eq!(booked["booking"], json!("booked"), "{booked}");
    assert_eq!(booked["after_secs"], json!(180), "{booked}");
    let holding = booked["started"]["holding"]
        .as_array()
        .unwrap_or_else(|| panic!("what holds the stop: {booked}"));
    assert!(
        holding
            .iter()
            .any(|row| row["name"].as_str().is_some_and(|row| row.contains(name))),
        "the wait {name} holds the stop: {booked}"
    );
    let (withdrawn, is_error) = server.answer(3, "self_stop", json!({"when_idle": "off"}));
    assert!(!is_error, "{withdrawn}");
    assert_eq!(withdrawn, json!({"booking": "withdrawn"}));
    let (tuned, is_error) = server.answer(
        4,
        "self_tune",
        json!({"setting": "keep-warm", "value": "2h"}),
    );
    assert!(!is_error, "{tuned}");
    let (stopped, is_error) = server.answer(5, "stop", json!({"id": id}));
    assert!(!is_error, "{stopped}");
    assert_eq!(stopped, json!({"id": id, "status": "stopped"}));
}

#[cfg(unix)]
fn serve_for_molt(env: &Env, trace: &std::path::Path) -> McpServer {
    use crate::common::{path_with_front, write_env_dump_shim, write_fake_login_shell};
    use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};

    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    crate::common::trust_claude_project(env, &env.project_root);
    crate::common::write_definition(env, "agents", "claude", "description: Base", "Base.");
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[agents]\nisolation = 'host'\n",
    )
    .unwrap();

    crate::common::wait::register_calling_agent(env);
    let workspace = env.resolve_workspace(&env.project_root);
    let mut observation = AgentLifecycleObservation::new(
        Some("provider-session".into()),
        LifecycleSignal::Registered,
    );
    observation.agent_name = Some("planner".to_owned());
    observation.pane_id = Some("zellij:terminal_1".parse().unwrap());
    env.store()
        .append_event(&rimz::store::event::EventEnvelope::agent_lifecycle(
            workspace.workspace_id,
            &workspace.session_name,
            "claude",
            "SessionStart",
            &observation,
        ))
        .unwrap();
    crate::common::room::seed_live_zellij_room(
        &env.runtime_paths(),
        &workspace.session_name,
        Vec::new(),
    );

    let shim = write_env_dump_shim(env, "claude");
    let shell = write_fake_login_shell(env, "molt-shell", &[]);
    let mut command = live_server(env, &[1]);
    command
        .env("ZELLIJ_PANE_ID", "1")
        .env("ZELLIJ_SESSION_NAME", workspace.session_name)
        .env("RIMZ_TEST_ZELLIJ_LOG", trace)
        .env("RIMZ_TEST_SWAP_BIND_AS_WRAPPER", "1")
        .env("PATH", path_with_front(&shim))
        .env("SHELL", shell);
    let mut server = McpServer::spawn(command);
    server.initialize();
    server
}

#[cfg(unix)]
#[test]
fn self_molt_binds_a_successor_and_keeps_the_server_answering() {
    let env = Env::new();
    let trace = env.home_root.join("molt-mux.log");
    let mut server = serve_for_molt(&env, &trace);
    let prompt = "Read the note at /x. go on";
    let (molted, is_error) = server.answer(1, "self_molt", json!({"prompt": prompt}));
    assert!(!is_error, "{molted}");
    assert_eq!(molted["agent"], "@planner#main", "{molted}");
    assert_eq!(molted["successor_kind"], "claude", "{molted}");
    assert_eq!(molted["closes_after_secs"], 0, "{molted}");
    assert_eq!(server.request(2, "ping", json!({}))["result"], json!({}));

    let store = env.store();
    let rows = store.audit_rows().unwrap();
    let successor = rows
        .iter()
        .find(|row| row.launch_id.as_deref() == Some("launch-session") && row.epoch == 1)
        .unwrap_or_else(|| panic!("no successor at epoch 1 under the caller's launch: {rows:?}"));
    assert!(
        successor
            .first_prompt
            .as_deref()
            .is_some_and(|first| first.starts_with(prompt)),
        "successor's first prompt: {:?}",
        successor.first_prompt
    );
    let trace = std::fs::read_to_string(trace).unwrap();
    assert!(trace.contains("\taction\tnew-pane\t"), "{trace}");
    assert!(!trace.contains("\taction\tclose-pane\t"), "{trace}");
    let bookings = rimz::store::idle_stop::read(store.paths());
    let [booking] = bookings.as_slice() else {
        panic!("expected one booking for the replaced session: {bookings:?}");
    };
    assert_eq!(booking.kind.as_str(), "claude");
    assert_eq!(booking.agent_id.as_str(), "provider-session");
    assert_eq!(booking.stop.after_secs, 0);
}

#[cfg(unix)]
#[test]
fn self_molt_refuses_an_invalid_when_idle_before_doing_any_work() {
    let env = Env::new();
    let trace = env.home_root.join("molt-mux.log");
    let mut server = serve_for_molt(&env, &trace);
    let events_before = env.read_events().len();
    let (failure, is_error) =
        server.answer(1, "self_molt", json!({"prompt": "x", "when_idle": "soon"}));
    assert!(is_error, "{failure}");
    assert_eq!(
        failure,
        json!({"error": "invalid when_idle `soon`: unknown duration unit `soon`; use s/m/h/d"})
    );
    assert!(
        !trace.exists(),
        "invalid when_idle touched the multiplexer: {:?}",
        std::fs::read_to_string(&trace)
    );
    let events = env.read_events();
    assert_eq!(events.len(), events_before, "{events:?}");
    let bookings = rimz::store::idle_stop::read(env.store().paths());
    assert!(bookings.is_empty(), "{bookings:?}");
}

#[test]
fn self_molt_refuses_a_caller_outside_an_agent() {
    let env = Env::new();
    env.record(&env.project_root);
    let mut server = serve(&env, &[]);
    server.initialize();
    let (failure, is_error) = server.answer(1, "self_molt", json!({"prompt": "x"}));
    assert!(is_error, "{failure}");
    assert_eq!(
        failure,
        json!({"error": "molt runs only inside an agent: only the session holding the context can \
                         write the note. Type /rimz-molt in the agent's pane, or send it with \
                         `rimz message @<agent> '/rimz-molt'`"})
    );
}

/// A tool that reads `/dev/stdin` reads nothing of the client's requests: the
/// call answers, refusing the empty body it read, and the next request on the
/// pipe is answered.
#[test]
fn a_tool_reading_stdin_leaves_the_protocol_alone() {
    let env = Env::new();
    let mut server = serve(&env, &[]);
    server.initialize();
    server.start_call(
        1,
        "notes",
        json!({"kind": "stdin", "title": "From stdin", "file": "/dev/stdin"}),
    );
    server.send(&json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}));
    let (failure, is_error) = tool_answer("notes", &server.reply(&json!(1)));
    assert!(is_error, "{failure}");
    assert_eq!(
        failure["error"],
        json!("the note body is empty; write the note on stdin or name it with --file"),
        "{failure}"
    );
    assert_eq!(server.reply(&json!(2))["result"], json!({}));
}

/// `lsp show` on one target that shows no code fails the call, as `query` and
/// `check` do; several targets stay text.
#[test]
fn lsp_show_on_one_target_that_fails_is_a_failure() {
    let env = Env::new();
    std::fs::create_dir_all(env.project_root.join("src")).unwrap();
    std::fs::write(env.project_root.join("src/x.rs"), "fn x() {}\n").unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&env.project_root)
            .status()
            .unwrap()
            .success()
    );
    let mut server = serve(&env, &[]);
    server.initialize();
    let cwd = &env.project_root;
    let (failure, is_error) = server.answer(
        1,
        "lsp",
        json!({"verb": "show", "targets": ["src/x.rs::Missing"], "cwd": cwd}),
    );
    assert!(is_error, "{failure}");
    assert!(
        failure["error"]
            .as_str()
            .unwrap()
            .starts_with("src/x.rs::Missing  "),
        "{failure}"
    );
    let (missing, is_error) = server.answer(
        2,
        "lsp",
        json!({"verb": "show", "targets": ["src/none.rs:1"], "cwd": cwd}),
    );
    assert!(is_error, "{missing}");
    assert!(
        missing["error"]
            .as_str()
            .unwrap()
            .starts_with("src/none.rs:1  missing-path  "),
        "{missing}"
    );
    let (several, is_error) = server.answer(
        3,
        "lsp",
        json!({"verb": "show", "targets": ["src/x.rs::Missing", "src/x.rs:1"], "cwd": cwd}),
    );
    assert!(!is_error, "{several}");
    assert!(several.as_str().unwrap().contains("fn x() {}"), "{several}");
}

/// `rimz agents explain <profile> --mcps` shows what each launch hands over:
/// RimZ's server and the load mode of each tool, the user's servers it keeps,
/// each mark it did not apply, and the provider's own arguments.
#[test]
fn explain_mcps_shows_what_each_launch_hands_over() {
    let env = Env::new();
    let codex_config = env.home_root.join(".codex/config.toml");
    std::fs::create_dir_all(codex_config.parent().unwrap()).unwrap();
    std::fs::write(
        &codex_config,
        "[mcp_servers.github]\ncommand = 'gh'\n[mcp_servers.notion]\ncommand = 'notion'\n",
    )
    .unwrap();
    for skill in ["rimz-subagents", "rimz-lsp"] {
        let dir = env.home_root.join(".claude/skills").join(skill);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {skill}\ndescription: {skill}\n---\n{skill}\n"),
        )
        .unwrap();
    }
    for (name, kind, mcps) in [
        (
            "cmark",
            "claude",
            "['*', 'rimz:deferred', 'rimz-lsp:always', 'github:always']",
        ),
        ("cflip", "claude", "['rimz:always', 'rimz-lsp:deferred']"),
        ("cnone", "claude", "[github]"),
        (
            "xmark",
            "codex",
            "['rimz:deferred', 'rimz-lsp:always', github]",
        ),
        ("pmark", "pi", "[rimz-lsp]"),
        (
            "cmute",
            "claude",
            "['*']\nskills: [rimz-subagents, rimz-lsp]\nsubagents: []",
        ),
    ] {
        let tools = match (kind, name) {
            ("pi", _) => "",
            (_, "cmute") => "\ntools: [Bash, Skill]",
            _ => "\ntools: [Bash]",
        };
        crate::common::write_definition(
            &env,
            "agents",
            name,
            &format!("description: {name} profile\nagent: {kind}{tools}\nmcps: {mcps}"),
            "",
        );
    }
    let config = env.rimz_home().join("config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let explain_with = |profile: &str, mcps: bool| {
        let mut bins = Vec::new();
        for kind in ["claude", "codex", "pi"] {
            bins.push(crate::common::write_failing_agent_shim(&env, kind, 0));
        }
        let shell = crate::common::write_fake_login_shell(&env, "mcps-explain-sh", &[]);
        let path = format!(
            "{}:{}",
            bins[0].display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = env
            .rimz()
            .args(["agents", "explain", profile])
            .args(mcps.then_some("--mcps"))
            .env("SHELL", shell)
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let explain = |profile: &str| explain_with(profile, true);

    let out = explain("cmark");
    assert!(!out.contains("Plan"), "{out}");
    assert!(
        out.contains("serves rimz-lsp, rimz-wait, rimz-subagents, rimz-control, rimz-notes"),
        "{out}"
    );
    assert_eq!(tool_load(&out, "lsp"), "always", "{out}");
    assert_eq!(tool_load(&out, "wait"), "default", "{out}");
    assert_eq!(tool_load(&out, "guide"), "default", "{out}");
    assert!(
        out.contains("every server the account and project configure"),
        "{out}"
    );
    assert!(
        out.contains(
            "the mcps entry `github:always` is not applied: Claude takes a load mode only for \
             RimZ's server and its capabilities"
        ),
        "{out}"
    );
    assert!(out.contains("--mcp-config"), "{out}");
    assert!(out.contains(r#""--always","rimz-lsp""#), "{out}");
    assert!(out.contains("claude mcp list"), "{out}");
    assert!(out.contains("/context"), "{out}");
    let mut server = serve(&env, &[]);
    let instructions = server.initialize()["result"]["instructions"].clone();
    let instructions = instructions.as_str().expect("instructions");
    assert!(out.contains(instructions), "{out}");
    let served = server.request(1, "tools/list", json!({}));
    for tool in served["result"]["tools"].as_array().expect("tools") {
        let name = tool["name"].as_str().expect("tool name");
        let schema = serde_json::to_string_pretty(&tool["inputSchema"]).unwrap();
        assert!(out.contains(&format!("\n## {name}\n")), "{out}");
        assert!(out.contains(tool["description"].as_str().unwrap()), "{out}");
        assert!(out.contains(&format!("```json\n{schema}\n```")), "{out}");
    }
    server.close();
    assert!(
        out.contains("rimz cannot see the tools of your own MCP servers"),
        "{out}"
    );

    for skill in [
        "rimz-lsp",
        "rimz-wait",
        "rimz-subagents",
        "rimz-control",
        "rimz-notes",
    ] {
        assert!(
            out.contains(&format!(
                "\n  {skill}: the launch serves its MCP tools, so the model is not offered the skill\n"
            )),
            "{out}"
        );
    }

    let out = explain("cflip");
    assert_eq!(tool_load(&out, "lsp"), "deferred", "{out}");
    assert_eq!(tool_load(&out, "wait"), "always", "{out}");
    assert_eq!(tool_load(&out, "notes"), "always", "{out}");
    assert_eq!(tool_load(&out, "guide"), "always", "{out}");
    let guide_line = out
        .split("\n## guide\n")
        .nth(1)
        .and_then(|rest| rest.lines().next())
        .unwrap_or_else(|| panic!("{out}"));
    assert!(guide_line.ends_with("load always"), "{guide_line}");

    let out = explain("cnone");
    assert!(
        out.contains("not handed over: the mcps list names no RimZ capability"),
        "{out}"
    );
    assert!(!out.contains("CAPABILITY"), "{out}");
    assert!(
        !out.contains("## lsp") && !out.contains("Server instructions"),
        "{out}"
    );
    assert!(out.contains("only github (allowedMcpServers)"), "{out}");
    assert!(!out.contains("Hidden skills"), "{out}");

    let out = explain("xmark");
    assert!(
        out.contains("serves rimz-lsp, rimz-wait, rimz-subagents, rimz-control, rimz-notes"),
        "{out}"
    );
    assert_eq!(tool_load(&out, "lsp"), "deferred", "{out}");
    assert_eq!(tool_load(&out, "notes"), "deferred", "{out}");
    assert!(out.contains("switched off: notion"), "{out}");
    assert!(
        out.contains(
            "the mcps entry `rimz-lsp:always` is not applied: Codex takes a load mode per \
             server, so rimz-lsp loads as the rimz server does"
        ),
        "{out}"
    );
    assert!(out.contains("mcp_servers.rimz.omit_tools_from=[]"), "{out}");
    assert!(out.contains("CODEX_ROLLOUT_TRACE_ROOT"), "{out}");

    let out = explain("pmark");
    assert!(
        out.contains("pi takes no MCP servers from a launch; it loads its own as configured"),
        "{out}"
    );
    assert!(!out.contains("CAPABILITY"), "{out}");
    assert!(
        !out.contains("## lsp") && !out.contains("Server instructions"),
        "{out}"
    );

    // A profile that allows no subagents is served no subagent tool and offered no skill.
    let out = explain("cmute");
    assert!(
        out.contains("serves rimz-lsp, rimz-wait, rimz-control, rimz-notes\n"),
        "{out}"
    );
    assert!(!out.contains("rimz-subagents"), "{out}");
    let out = explain_with("cmute", false);
    let callable = out
        .lines()
        .find(|line| line.starts_with("  callable: "))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(!callable.contains("rimz-subagents"), "{out}");

    std::fs::write(&config, "[agents]\nrimz-mcp-server = false\n").unwrap();
    let out = explain("cmark");
    assert!(
        out.contains("off: [agents] rimz-mcp-server = false"),
        "{out}"
    );
    assert!(!out.contains("CAPABILITY"), "{out}");
    assert!(!out.contains("Hidden skills"), "{out}");
    assert!(
        !out.contains("## lsp") && !out.contains("Server instructions"),
        "{out}"
    );
}

/// On Antigravity `explain --mcps` reads the planned agent file for the user's servers (the
/// ones `inheritMcp: false` keeps, and the file's two MCP lines) and the launch env for RimZ's
/// plugin server; a launch that writes no file still serves and limits nothing.
#[test]
fn explain_mcps_reads_the_antigravity_agent_file() {
    let explain = |env: &Env, target: &str, extra: &[&str]| {
        let plugin = env.home_root.join(".gemini/config/plugins/rimz");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(
            plugin.join("plugin.json"),
            r#"{"name":"rimz","author":"RimZ"}"#,
        )
        .unwrap();
        let bin = crate::common::write_failing_agent_shim(env, "agy", 0);
        let shell = crate::common::write_fake_login_shell(env, "mcps-explain-sh", &[]);
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = env
            .rimz()
            .args(["agents", "explain", target, "--mcps"])
            .args(extra)
            .env("SHELL", shell)
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let env = Env::new();
    for (name, mcps) in [
        ("glimit", "\nmcps: [rimz-lsp:always, github:always, notion]"),
        ("gopen", ""),
    ] {
        crate::common::write_definition(
            &env,
            "agents",
            name,
            &format!("description: {name} profile\nagent: antigravity\ntools: [Bash]{mcps}"),
            "",
        );
    }

    let out = explain(&env, "glimit", &[]);
    assert!(out.contains("serves rimz-lsp as `rimz_tools`\n"), "{out}");
    assert!(
        out.contains("\nLaunch environment\n  RIMZ_MCP_SERVE=rimz-lsp:always\n  RIMZ_MCP_CWD="),
        "{out}"
    );
    assert!(
        out.contains("only github, notion (inheritMcp: false)"),
        "{out}"
    );
    assert_eq!(tool_load(&out, "lsp"), "always", "{out}");
    assert!(
        out.contains("\nNot applied\n  the mcps entry `github:always` is not applied: Antigravity takes a load mode only for RimZ's capabilities\n"),
        "{out}"
    );
    assert!(out.contains("\n  inheritMcp: false\n"), "{out}");
    assert!(
        out.contains(r#"{"name": "github"}, {"name": "notion"}]"#),
        "{out}"
    );

    let out = explain(&env, "gopen", &[]);
    assert!(
        out.contains(
            "serves rimz-lsp, rimz-wait, rimz-subagents, rimz-control, rimz-notes as `rimz_tools`\n"
        ),
        "{out}"
    );
    assert!(
        out.contains("every server the account and project configure"),
        "{out}"
    );
    assert!(!out.contains("inheritMcp"), "{out}");

    // A launch that writes no agent file serves the same tools from the plugin's server.
    let bare = Env::new();
    let out = explain(&bare, "antigravity", &[]);
    assert!(
        out.contains(
            "serves rimz-lsp, rimz-wait, rimz-subagents, rimz-control, rimz-notes as `rimz_tools`\n"
        ),
        "{out}"
    );
    assert!(
        out.contains("every server the account and project configure"),
        "{out}"
    );
    assert!(!out.contains("inheritMcp"), "{out}");

    // A raw `allowedMcpServers` setting is Claude's, never an Antigravity limit.
    let out = explain(
        &env,
        "gopen",
        &["--", "--settings", r#"{"allowedMcpServers":[]}"#],
    );
    assert!(
        out.contains("every server the account and project configure")
            && !out.contains("only none"),
        "{out}"
    );

    // A resumed conversation keeps the user servers it started with, whatever its profile lists
    // now; RimZ's tools follow the profile, since the launch env names them on every launch.
    env.record(&env.project_root);
    let transcript = env.home_root.join("planner.jsonl");
    std::fs::write(&transcript, "{}\n").unwrap();
    let launch = rimz::agents::LaunchParams {
        profile: Some("glimit".to_owned()),
        ..Default::default()
    };
    crate::common::wait::register_seat(&env, "antigravity", launch, "terminal_1", &transcript);
    let out = explain(&env, "@planner", &[]);
    let kept = "kept from the conversation's start: a resume writes no agent file";
    assert_eq!(out.matches(kept).count(), 1, "{out}");
    assert!(out.contains("serves rimz-lsp as `rimz_tools`\n"), "{out}");
    assert_eq!(tool_load(&out, "lsp"), "always", "{out}");

    // A resume that serves nothing reports why, as a fresh launch does: the plugin's server
    // follows the launch env, not the conversation's start.
    let none = Env::new();
    crate::common::write_definition(
        &none,
        "agents",
        "gnone",
        "description: gnone profile\nagent: antigravity\ntools: [Bash]\nmcps: []",
        "",
    );
    none.record(&none.project_root);
    let transcript = none.home_root.join("planner.jsonl");
    std::fs::write(&transcript, "{}\n").unwrap();
    let launch = rimz::agents::LaunchParams {
        profile: Some("gnone".to_owned()),
        ..Default::default()
    };
    crate::common::wait::register_seat(&none, "antigravity", launch, "terminal_1", &transcript);
    let out = explain(&none, "@planner", &[]);
    assert!(
        out.contains("not handed over: the mcps list names no RimZ capability\n"),
        "{out}"
    );
    assert_eq!(out.matches(kept).count(), 1, "{out}");
}

/// A profile's own Codex overrides sit in the argv beside RimZ's, and
/// `explain --mcps` reads what Codex applies: the last `omit_tools_from` for
/// `rimz`, in either `-c` or `--config=` spelling, only the entries a
/// `mcp_servers=` table switches off, and the `rimz` server off when an override
/// sets its `enabled` to false, the dotted key or the table.
#[test]
fn explain_mcps_reads_a_profiles_own_codex_overrides_as_codex_applies_them() {
    let env = Env::new();
    env.write_config(
        &env.project_root,
        "[profiles.xlast]\nagent = 'codex'\nmcps = ['*', 'rimz:always']\n\
         args = \"-c 'mcp_servers.rimz.omit_tools_from=[]'\"\n\
         [profiles.xconf]\nagent = 'codex'\nmcps = ['*']\n\
         args = \"'--config=mcp_servers.rimz.omit_tools_from=[\\\"deferred\\\"]'\"\n\
         [profiles.xtable]\nagent = 'codex'\nmcps = ['*']\n\
         args = \"-c 'mcp_servers={notion={enabled=false},github={command=\\\"gh\\\"}}'\"\n\
         [profiles.xoff]\nagent = 'codex'\nmcps = ['*']\n\
         args = \"-c mcp_servers.rimz.enabled=false\"\n\
         [profiles.xtableoff]\nagent = 'codex'\nmcps = ['*']\n\
         args = \"-c 'mcp_servers={rimz={enabled=false}}'\"\n",
    );
    let granted = env.rimz().args(["trust", "grant"]).output().unwrap();
    assert!(
        granted.status.success(),
        "{}",
        String::from_utf8_lossy(&granted.stderr)
    );
    let explain = |profile: &str| {
        let bin = crate::common::write_failing_agent_shim(&env, "codex", 0);
        let shell = crate::common::write_fake_login_shell(&env, "mcps-overrides-sh", &[]);
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = env
            .rimz()
            .args(["agents", "explain", profile, "--mcps"])
            .env("SHELL", shell)
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };

    let out = explain("xlast");
    let user = out
        .find("mcp_servers.rimz.omit_tools_from=[]")
        .unwrap_or_else(|| panic!("{out}"));
    let rimz = out
        .find(r#"mcp_servers.rimz.omit_tools_from=["deferred"]"#)
        .unwrap_or_else(|| panic!("{out}"));
    assert!(user < rimz, "RimZ's override follows the profile's: {out}");
    assert_eq!(tool_load(&out, "lsp"), "always", "{out}");
    assert_eq!(tool_load(&out, "guide"), "always", "{out}");

    let out = explain("xconf");
    assert_eq!(tool_load(&out, "lsp"), "always", "{out}");
    assert!(
        out.contains(r#"--config mcp_servers.rimz.omit_tools_from=["deferred"]"#),
        "{out}"
    );

    let out = explain("xtable");
    assert!(out.contains("switched off: notion\n"), "{out}");

    for (profile, switch) in [
        ("xoff", "-c mcp_servers.rimz.enabled=false"),
        ("xtableoff", "-c mcp_servers={rimz={enabled=false}}"),
    ] {
        let out = explain(profile);
        assert!(
            out.contains(&format!("rimz:         off: {switch}\n")),
            "{out}"
        );
        assert!(!out.contains("CAPABILITY"), "{out}");
        assert!(
            !out.contains("## lsp") && !out.contains("Server instructions"),
            "{out}"
        );
        assert!(out.contains("mcp_servers.rimz.command="), "{out}");
        assert!(!out.contains("Hidden skills"), "{out}");
        let json = env
            .rimz()
            .args(["agents", "explain", profile, "--json"])
            .env(
                "SHELL",
                crate::common::write_fake_login_shell(&env, "mcps-overrides-json-sh", &[]),
            )
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    crate::common::write_failing_agent_shim(&env, "codex", 0).display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .output()
            .unwrap();
        let report: serde_json::Value = serde_json::from_slice(&json.stdout)
            .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&json.stderr)));
        assert_eq!(
            report["skills"]["hidden"],
            serde_json::json!([]),
            "{profile}"
        );
    }
}

/// Codex hands the server only the variables its entry's `env_vars` names, so
/// the same calls pass under an environment cleared to the names a Codex
/// launch's rendered entry lists.
#[test]
fn a_call_under_the_codex_entrys_env_vars_resolves_the_same_agent() {
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent(&env);
    let output = crate::common::wait::agent_wait(&env)
        .args(["agents", "explain", "codex", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let argv: Vec<&str> = report["provider_argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap())
        .collect();
    let listed = argv
        .iter()
        .find_map(|arg| arg.strip_prefix("mcp_servers.rimz.env_vars="))
        .unwrap_or_else(|| panic!("no env_vars entry in {argv:?}"));
    let names: Vec<String> =
        toml::from_str::<toml::Table>(&format!("names = {listed}")).unwrap()["names"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap().to_owned())
            .collect();

    let mut command = crate::common::wait::agent_wait(&env);
    command.args(["mcp", "serve"]);
    let mut spawned: std::collections::BTreeMap<_, _> = std::env::vars_os().collect();
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => spawned.insert(key.to_owned(), value.to_owned()),
            None => spawned.remove(key),
        };
    }
    let kept: Vec<_> = spawned
        .into_iter()
        .filter(|(key, _)| names.iter().any(|name| key == name.as_str()))
        .collect();
    command.env_clear().envs(kept);
    let mut server = McpServer::spawn(command);
    server.initialize();
    let (start, is_error) = server.answer(
        1,
        "wait",
        json!({"trigger": "in", "value": "1h", "label": "cleared"}),
    );
    assert!(!is_error, "{start}");
    let (listed, is_error) = server.answer(2, "tasks", json!({}));
    assert!(!is_error, "{listed}");
    assert!(listed.to_string().contains("cleared"), "{listed}");
    let (added, is_error) = server.answer(
        3,
        "notes",
        json!({"kind": "diary", "title": "Day", "body": "text\n"}),
    );
    assert!(!is_error, "{added}");
    let note = std::fs::read_to_string(added["path"].as_str().unwrap()).expect("read note");
    assert!(note.contains("author: \"planner\"\n"), "{note}");
}

/// `--parent-env` under a shell that stays the server's parent, as Codex
/// does: the shell holds the fixture's environment, while the server starts
/// from `env -i` with only `own` set.
#[cfg(unix)]
fn serve_under_parent(env: &Env, own: &[(&str, &std::path::Path)]) -> McpServer {
    let rimz = env.rimz();
    let mut shell = Command::new("/bin/sh");
    for (key, value) in rimz.get_envs() {
        match value {
            Some(value) => shell.env(key, value),
            None => shell.env_remove(key),
        };
    }
    // A server without a home falls back under `TMPDIR`: keep it in the fixture.
    let tmp = env.home_root.join("own-tmp");
    let own: Vec<String> = [("TMPDIR", tmp.as_path())]
        .iter()
        .chain(own)
        .map(|(key, value)| format!("{key}={}", value.display()))
        .collect();
    // The trailing `exit` keeps the shell from exec'ing into `env`.
    shell
        .arg("-c")
        .arg(r#"env -i XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" "$@"; exit $?"#)
        .arg("sh")
        .args(own)
        .arg(rimz.get_program())
        .args(["mcp", "serve", "--parent-env"]);
    McpServer::spawn(shell)
}

#[cfg(unix)]
fn note_home(server: &mut McpServer, id: i64) -> String {
    let (added, is_error) = server.answer(
        id,
        "notes",
        json!({"kind": "diary", "title": format!("Day {id}"), "body": "text\n"}),
    );
    assert!(!is_error, "{added}");
    added["path"].as_str().expect("the note's path").to_owned()
}

/// A call sees what only the parent's environment holds (the fixture's
/// `RIMZ_HOME`), while a name the server's own environment holds keeps the
/// server's value.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn parent_env_fills_the_names_the_servers_own_environment_lacks() {
    let env = Env::new();
    let mut server = serve_under_parent(&env, &[]);
    server.initialize();
    let path = note_home(&mut server, 1);
    assert!(
        path.starts_with(&env.rimz_home().display().to_string()),
        "{path}"
    );
    drop(server);

    let own_home = env.home_root.join("own-rimz-home");
    let mut server = serve_under_parent(&env, &[("RIMZ_HOME", &own_home)]);
    server.initialize();
    let path = note_home(&mut server, 1);
    assert!(path.starts_with(&own_home.display().to_string()), "{path}");
}

/// A server started, as Codex starts it, without the fixture's `RIMZ_HOME`
/// or `HOME` reads its wording from the home its parent names, as its
/// calls resolve it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn parent_env_names_the_home_the_wording_is_read_from() {
    let env = Env::new();
    let mcps = env.rimz_home().join("mcps/rimz-wait");
    std::fs::create_dir_all(&mcps).unwrap();
    std::fs::write(
        mcps.join("wait.md"),
        "Wait outside your turn, from the parent's home.\n",
    )
    .unwrap();
    let mut server = serve_under_parent(&env, &[]);
    server.initialize();
    let listing = server.request(1, "tools/list", json!({}));
    let described = listing["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "wait")
        .map(|tool| tool["description"].clone());
    assert_eq!(
        described,
        Some(json!("Wait outside your turn, from the parent's home."))
    );
    let path = note_home(&mut server, 2);
    assert!(
        path.starts_with(&env.rimz_home().display().to_string()),
        "{path}"
    );
}

/// The watcher a wait detached outlives the provider ending the server's
/// whole process group.
#[cfg(unix)]
#[test]
fn a_wait_armed_through_the_server_fires_after_its_group_is_terminated() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    crate::common::wait::register_calling_agent(&env);
    let mut server = serve_as_agent(&env);
    server.initialize();
    let (text, is_error) = server.call(
        1,
        "wait",
        json!({"trigger": "run", "value": "sleep 1; printf fired"}),
    );
    assert!(!is_error, "{text}");
    nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(server.pid() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    assert_eq!(server.wait_exit().code(), Some(0));

    let runs = env.rimz_home().join("logs/loop-runs.log.jsonl");
    let deadline = Instant::now() + REPLY_DEADLINE;
    let record = loop {
        let records: Vec<rimz::harness::schedule::run_log::LoopRunRecord> =
            std::fs::read_to_string(&runs)
                .unwrap_or_default()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
        if let Some(record) = records.into_iter().next() {
            break record;
        }
        assert!(Instant::now() < deadline, "the wait never fired");
        std::thread::sleep(Duration::from_millis(50));
    };
    let check = record.check.expect("a watch record");
    assert_eq!(check.code, Some(0));
    assert_eq!(
        std::fs::read_to_string(check.output_path.expect("output file")).unwrap(),
        "fired"
    );
}

/// A git checkout under the fixture holding a FIFO, `held.md`, that blocks a
/// reader until `HeldFifo::release`.
#[cfg(target_os = "linux")]
fn checkout_with_fifo(env: &Env, name: &str) -> std::path::PathBuf {
    let dir = env.home_root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .status()
        .unwrap();
    assert!(status.success());
    nix::unistd::mkfifo(&dir.join("held.md"), nix::sys::stat::Mode::S_IRWXU).unwrap();
    dir
}

/// The write end of a checkout's FIFO, opened once a reader blocks in it.
#[cfg(target_os = "linux")]
struct HeldFifo(std::fs::File);

#[cfg(target_os = "linux")]
impl HeldFifo {
    /// Open the FIFO's write end as soon as a reader holds its read end, which
    /// leaves that reader blocked until `release`.
    fn when_read(checkout: &std::path::Path) -> Self {
        use std::os::unix::fs::OpenOptionsExt;
        let deadline = Instant::now() + REPLY_DEADLINE;
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(nix::libc::O_NONBLOCK)
                .open(checkout.join("held.md"))
            {
                Ok(file) => return Self(file),
                Err(err) if err.raw_os_error() == Some(nix::libc::ENXIO) => {
                    assert!(Instant::now() < deadline, "no call read the FIFO");
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(err) => panic!("open the FIFO: {err}"),
            }
        }
    }

    fn release(mut self, text: &str) {
        self.0.write_all(text.as_bytes()).unwrap();
    }
}

/// Every child process of `pid`, from each of its threads.
#[cfg(target_os = "linux")]
fn children(pid: u32) -> Vec<String> {
    std::fs::read_dir(format!("/proc/{pid}/task"))
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|task| std::fs::read_to_string(task.path().join("children")).ok())
        .flat_map(|children| {
            children
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Two overlapping calls with their own `cwd` each act on their own checkout,
/// a later call answers while an earlier one still runs, and neither starts a
/// process: the first blocks reading a FIFO in its checkout while the second
/// checks a note in another.
#[cfg(target_os = "linux")]
#[test]
fn overlapping_calls_act_on_their_own_checkouts_inside_the_server() {
    let env = Env::new();
    let first = checkout_with_fifo(&env, "first-5b1e");
    let second = env.home_root.join("second-5b1e");
    std::fs::create_dir_all(&second).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&second)
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(second.join("free.md"), "see `src/gone.rs::Gone`\n").unwrap();
    let mut server = serve(&env, &[]);
    server.initialize();
    server.start_call(
        1,
        "lsp",
        json!({"verb": "check", "targets": ["held.md"], "cwd": first}),
    );
    let held = HeldFifo::when_read(&first);
    let (text, is_error) = server.answer(
        2,
        "lsp",
        json!({"verb": "check", "targets": ["free.md"], "cwd": second}),
    );
    assert!(!is_error, "{text}");
    let text = text.as_str().unwrap();
    assert!(
        text.contains("free.md:1  missing-path  src/gone.rs::Gone"),
        "{text}"
    );
    assert!(server.unclaimed.is_empty(), "{:?}", server.unclaimed);
    assert_eq!(children(server.pid()), Vec::<String>::new());

    held.release("see `src/held.rs::Held`\n");
    let (text, is_error) = tool_answer("lsp", &server.reply(&json!(1)));
    assert!(!is_error, "{text}");
    assert!(
        text.as_str()
            .unwrap()
            .contains("held.md:1  missing-path  src/held.rs::Held"),
        "{text}"
    );
}

/// A cancelled call gets no reply, even once its work ends, and the server
/// keeps answering.
#[cfg(target_os = "linux")]
#[test]
fn a_cancelled_call_gets_no_reply_and_the_server_keeps_answering() {
    let env = Env::new();
    let checkout = checkout_with_fifo(&env, "cancelled-5b1e");
    let mut server = serve(&env, &[]);
    server.initialize();
    server.start_call(
        1,
        "lsp",
        json!({"verb": "check", "targets": ["held.md"], "cwd": checkout}),
    );
    let held = HeldFifo::when_read(&checkout);
    server.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": 1, "reason": "test"},
    }));
    assert_eq!(server.request(2, "ping", json!({}))["result"], json!({}));
    held.release("no anchors\n");
    server.assert_silent_for(Duration::from_secs(2));
    assert_eq!(server.request(3, "ping", json!({}))["result"], json!({}));
    let (status, lines) = server.close();
    assert_eq!(status.code(), Some(0));
    assert!(
        lines.iter().all(|line| !line.contains("\"id\":1,")),
        "a cancelled call is not answered: {lines:?}"
    );
}

/// The hint of a wait, schedule, or team RimZ holds no record of.
const GONE: &str = "nothing of this name runs now: it ended, or never existed, and RimZ keeps no \
     record that tells which";

/// Seed a live workflow run in the fixture's room, started by the agent whose launch is
/// `starter`, with `extra` merged into its record; returns its id.
fn seed_started_run(env: &Env, starter: &str, extra: Value) -> String {
    let paths = env.state_path_for(&env.project_root);
    let guard = rimz::disk::lock::WorkspaceLock::acquire(&paths.workspace_lock).unwrap();
    let now = jiff::Timestamp::now();
    let id = rimz::ids::WorkflowRunId::new();
    let mut record = json!({
        "version": rimz::store::workflow::VERSION, "id": id, "workflow": "review",
        "spec_dir": "/definitions/review",
        "scope": {"kind": "worktree", "path": env.project_root},
        "project_root": env.project_root, "created_at": now, "entered_at": now,
        "state": "active", "node": "agent", "visits": {}, "deadline_fired": false,
        "next_seq": 1, "starter": {"by": "agent", "kind": "claude", "launch": starter},
    });
    if let (Some(record), Value::Object(extra)) = (record.as_object_mut(), extra) {
        record.extend(extra);
    }
    let record: rimz::store::workflow::RunRecord = serde_json::from_value(record).unwrap();
    rimz::store::workflow::initialize(&paths, &record, &guard).unwrap();
    id.to_string()
}

/// The record of run `id` and whether its home is the finished one.
fn run_record(env: &Env, id: &str) -> (Value, bool) {
    let state = env.state_path_for(&env.project_root);
    let found = rimz::store::workflow::find(&state, &id.parse().unwrap())
        .unwrap()
        .unwrap_or_else(|| panic!("run {id} has no home"));
    let record = serde_json::from_slice(&std::fs::read(&found.paths.record).unwrap()).unwrap();
    (record, found.finished)
}

/// A wait the agent armed is a row of `tasks` by its id; `join` answers that its outcome
/// comes by message, `stop` ends it, and a second stop answers `ended` as a value. An id
/// of another shape, or of nothing RimZ holds, is refused with its fix.
#[test]
fn tasks_join_and_stop_act_on_a_wait_by_its_id() {
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent(&env);
    let mut server = serve_as_agent(&env);
    server.initialize();
    let (start, is_error) = server.answer(
        1,
        "wait",
        json!({"trigger": "in", "value": "1h", "label": "nightly check"}),
    );
    assert!(!is_error, "{start}");
    let id = start["id"].as_str().unwrap().to_owned();
    let (listed, is_error) = server.answer(2, "tasks", json!({"kind": "wait"}));
    assert!(!is_error, "{listed}");
    let [row] = listed["tasks"].as_array().unwrap().as_slice() else {
        panic!("one task: {listed}");
    };
    assert_eq!(
        (
            &row["id"],
            &row["kind"],
            &row["status"],
            &row["on_your_stop"],
            &row["about"]
        ),
        (
            &json!(id),
            &json!("wait"),
            &json!("running"),
            &json!("holds"),
            &json!("nightly check")
        ),
        "{listed}"
    );
    let (listed, _) = server.answer(3, "tasks", json!({"kind": "agent"}));
    assert_eq!(listed, json!({"tasks": []}));

    let (joined, is_error) = server.answer(4, "join", json!({"id": id}));
    assert!(!is_error, "{joined}");
    assert_eq!(
        joined,
        json!({
            "id": id,
            "status": "running",
            "report": "its outcome comes to you as a message; join does not wait for it",
        })
    );
    let (stopped, is_error) = server.answer(5, "stop", json!({"id": id}));
    assert!(!is_error, "{stopped}");
    assert_eq!(stopped, json!({"id": id, "status": "stopped"}));
    let (listed, _) = server.answer(6, "tasks", json!({}));
    assert_eq!(listed, json!({"tasks": []}), "the stopped wait is gone");
    let (again, is_error) = server.answer(7, "stop", json!({"id": id}));
    assert!(!is_error, "{again}");
    assert_eq!(again, json!({"id": id, "status": "ended", "hint": GONE}));
    let (joined, _) = server.answer(8, "join", json!({"id": id}));
    assert_eq!(joined, json!({"id": id, "status": "ended", "hint": GONE}));

    let (failure, is_error) = server.answer(9, "stop", json!({"id": "toolu_01ABC"}));
    assert!(is_error, "{failure}");
    assert_eq!(
        failure,
        json!({
            "error": "`toolu_01ABC` is no id of a thing RimZ started",
            "hint": "an id is a kind (subagent, agent, team, workflow, schedule, or wait), a \
                     colon, and a name, as `tasks` lists it; end a task or subagent your \
                     provider started with your provider's own tool",
        })
    );
    let (failure, is_error) = server.answer(11, "stop", json!({"id": "wait:../x"}));
    assert!(is_error, "{failure}");
    assert_eq!(
        failure,
        json!({
            "error": "invalid wait name `../x`: schedule name `../x` must be non-empty and use \
                      only letters, digits, `-`, or `_`",
            "hint": "`tasks` lists what you started, by id",
        }),
        "an invalid name is refused, not read as gone"
    );
    let (failure, is_error) = server.answer(10, "join", json!({"id": "subagent:nobody"}));
    assert!(is_error, "{failure}");
    assert_eq!(
        failure,
        json!({
            "error": "no subagent named `nobody`",
            "hint": "`tasks` lists what you started, by id",
        })
    );
}

/// A wait the caller armed for itself is its to stop; a subscription that wakes the caller but
/// that its team armed is not, and stays booked.
#[test]
fn stop_refuses_a_subscription_another_armed_for_the_caller() {
    use crate::common::wait::{seed_signal_wait, wait_instances};

    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent(&env);
    seed_signal_wait(&env, "own-ping", "own-ping", false);
    seed_signal_wait(&env, "team-ping", "team-ping", false);
    let mut tasks = wait_instances(&env);
    tasks.0.get_mut("team-ping").unwrap().team = Some("forge#main".parse().unwrap());
    std::fs::write(
        crate::common::wait::loop_instances_path(&env),
        serde_json::to_vec(&tasks).unwrap(),
    )
    .unwrap();
    let mut server = serve_as_agent(&env);
    server.initialize();

    let (listed, is_error) = server.answer(3, "tasks", json!({"kind": "wait"}));
    assert!(!is_error, "{listed}");
    let ids: Vec<&Value> = listed["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| &row["id"])
        .collect();
    assert_eq!(ids, [&json!("wait:own-ping")], "{listed}");
    let (failure, is_error) = server.answer(1, "stop", json!({"id": "wait:team-ping"}));
    assert!(is_error, "{failure}");
    assert_eq!(
        failure,
        json!({
            "error": "wait:team-ping was started by another agent",
            "hint": "stop and join act on what you started, or what an agent you started \
                     started; `tasks` lists it",
        })
    );
    assert!(wait_instances(&env).0.contains_key("team-ping"));
    let (stopped, is_error) = server.answer(2, "stop", json!({"id": "wait:own-ping"}));
    assert!(!is_error, "{stopped}");
    assert_eq!(stopped, json!({"id": "wait:own-ping", "status": "stopped"}));
    assert!(!wait_instances(&env).0.contains_key("own-ping"));
}

/// Live children the caller launched stop through the tree stop of `rimz subagents stop` and
/// `rimz agents stop`, which cancels the subagent's run and ends each one's process, and a
/// second stop answers `ended`; a live agent another agent launched is refused and runs on.
#[test]
fn stop_ends_a_live_child_and_refuses_another_agents_live_one() {
    use rimz::store::run::RunStatus;

    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent(&env);
    let mut kid = live_seat(&env, "otter", 3, child_of_caller());
    let mut peer = live_seat(&env, "scout", 4, launched_by("launch-session"));
    let mut far = live_seat(&env, "rival", 5, launched_by("launch-other"));
    let run = seed_subagent_run(&env, "otter");
    let mut server = serve_live(&env, &[3, 4, 5]);
    server.initialize();

    let (listed, is_error) = server.answer(10, "tasks", json!({}));
    assert!(!is_error, "{listed}");
    let ids = task_ids(&listed);
    for id in ["subagent:otter", "agent:scout"] {
        assert!(ids.contains(&id), "{id} is listed: {listed}");
    }
    assert!(!ids.contains(&"agent:rival"), "{listed}");
    for (seq, id, process) in [
        (1, "subagent:otter", &mut kid),
        (3, "agent:scout", &mut peer),
    ] {
        let (stopped, is_error) = server.answer(seq, "stop", json!({"id": id}));
        assert!(!is_error, "{stopped}");
        assert_eq!(stopped, json!({"id": id, "status": "stopped"}));
        assert!(
            process.try_wait().unwrap().is_some(),
            "{id}'s process is gone"
        );
        // The stop records a subagent's end itself; an agent's comes from its provider's
        // SessionEnd, which the stand-in process never sends.
        if id == "agent:scout" {
            end_session(&env, "scout-session");
        }
        let (again, is_error) = server.answer(seq + 1, "stop", json!({"id": id}));
        assert!(!is_error, "{again}");
        assert_eq!(again, json!({"id": id, "status": "ended"}));
    }
    assert_eq!(
        rimz::harness::run::load(env.store().paths(), &run)
            .unwrap()
            .status,
        RunStatus::Canceled
    );
    let (failure, is_error) = server.answer(5, "stop", json!({"id": "agent:rival"}));
    assert!(is_error, "{failure}");
    assert_eq!(
        failure,
        json!({
            "error": "agent:rival was started by another agent",
            "hint": "stop and join act on what you started, or what an agent you started \
                     started; `tasks` lists it",
        })
    );
    assert!(
        far.try_wait().unwrap().is_none(),
        "the refused agent runs on"
    );
    far.kill().unwrap();
    far.wait().unwrap();
}

/// `join` of a live subagent answers `running` at a short deadline while its run goes, and
/// `done` with the run's last message once the run completes.
#[test]
fn join_answers_running_at_its_deadline_then_a_finished_subagents_result() {
    use rimz::store::run::RunStatus;

    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent(&env);
    let mut kid = live_seat(&env, "otter", 3, child_of_caller());
    let run = seed_subagent_run(&env, "otter");
    let mut server = serve_live(&env, &[3]);
    server.initialize();

    let (listed, is_error) = server.answer(10, "tasks", json!({"kind": "subagent"}));
    assert!(!is_error, "{listed}");
    assert_eq!(task_ids(&listed), ["subagent:otter"], "{listed}");
    let began = Instant::now();
    let (joined, is_error) =
        server.answer(1, "join", json!({"id": "subagent:otter", "timeout": "1s"}));
    assert!(!is_error, "{joined}");
    assert_eq!(joined, json!({"id": "subagent:otter", "status": "running"}));
    assert!(
        began.elapsed() >= Duration::from_secs(1),
        "{:?}",
        began.elapsed()
    );
    let paths = env.store().paths().clone();
    let mut record = rimz::harness::run::load(&paths, &run).unwrap();
    record.status = RunStatus::Completed;
    record.last_message = Some("all green".to_owned());
    rimz::harness::run::create(&paths, &record).unwrap();
    let (joined, is_error) =
        server.answer(2, "join", json!({"id": "subagent:otter", "timeout": "10s"}));
    assert!(!is_error, "{joined}");
    assert_eq!(
        joined,
        json!({"id": "subagent:otter", "status": "done", "result": "all green"})
    );
    kid.kill().unwrap();
    kid.wait().unwrap();
}

/// A join cancelled once its subagent's run has settled leaves the run unclaimed, for the
/// digest to deliver; an answered join claims it.
#[test]
fn a_join_cancelled_as_its_run_settles_leaves_the_run_unclaimed() {
    use rimz::store::run::RunStatus;

    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent(&env);
    let mut kid = live_seat(&env, "otter", 3, child_of_caller());
    let run = seed_subagent_run(&env, "otter");
    let paths = env.store().paths().clone();
    let mut record = rimz::harness::run::load(&paths, &run).unwrap();
    record.status = RunStatus::Completed;
    record.last_message = Some("all green".to_owned());
    rimz::harness::run::create(&paths, &record).unwrap();
    let joined_at = || rimz::harness::run::load(&paths, &run).unwrap().joined_at;
    // A join claims a run only inside its caller's turn.
    session_event(
        &env,
        "provider-session",
        rimz::agents::LifecycleSignal::TurnStarted { turn_id: None },
        "UserPromptSubmit",
    );
    let gate = crate::common::DeliveryRendezvous::new(&env, "join-claim");
    let mut command = live_server(&env, &[3]);
    command.env("RIMZ_TEST_MCP_JOIN_CLAIM_GATE", &gate.path);
    let mut server = McpServer::spawn(command);
    server.initialize();

    let id = "subagent:otter";
    server.start_call(1, "join", json!({"id": id, "timeout": "10s"}));
    let mut held = gate.arrive();
    server.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": 1, "reason": "test"},
    }));
    // The reader handles lines in order, so the ping's reply shows it has taken the cancel.
    assert_eq!(server.request(2, "ping", json!({}))["result"], json!({}));
    held.write_all(&[0]).unwrap();
    let mut decided = gate.arrive();
    assert_eq!(joined_at(), None, "a cancelled join claims nothing");
    decided.write_all(&[0]).unwrap();

    server.start_call(3, "join", json!({"id": id, "timeout": "10s"}));
    gate.arrive().write_all(&[0]).unwrap();
    let mut decided = gate.arrive();
    assert!(joined_at().is_some(), "an answered join claims the run");
    decided.write_all(&[0]).unwrap();
    let (joined, is_error) = tool_answer("join", &server.reply(&json!(3)));
    assert!(!is_error, "{joined}");
    assert_eq!(
        joined,
        json!({"id": id, "status": "done", "result": "all green"})
    );
    let (_, lines) = server.close();
    assert!(
        lines.iter().all(|line| !line.contains("\"id\":1,")),
        "a cancelled join is not answered: {lines:?}"
    );
    kid.kill().unwrap();
    kid.wait().unwrap();
}

/// `stop` of a team the caller launched ends every seat, and a second stop finds no cohort.
#[test]
fn stop_ends_every_seat_of_a_team_the_caller_launched() {
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent(&env);
    let seat = |role: &str| rimz::agents::LaunchParams {
        team: Some("forge".into()),
        role: Some(role.to_owned()),
        channel: Some("feature-team".to_owned()),
        ..launched_by("launch-session")
    };
    let mut seats = [
        live_seat(&env, "coder", 3, seat("coder")),
        live_seat(&env, "reviewer", 4, seat("reviewer")),
    ];
    let mut rival = live_seat(
        &env,
        "rival",
        5,
        rimz::agents::LaunchParams {
            team: Some("forge".into()),
            role: Some("coder".to_owned()),
            channel: Some("rival-team".to_owned()),
            ..launched_by("launch-other")
        },
    );
    let mut server = serve_live(&env, &[3, 4, 5]);
    server.initialize();

    let id = "team:forge#feature-team";
    let (listed, is_error) = server.answer(10, "tasks", json!({"kind": "team"}));
    assert!(!is_error, "{listed}");
    assert_eq!(task_ids(&listed), [id], "{listed}");
    let (joined, is_error) = server.answer(11, "join", json!({"id": id, "timeout": "1s"}));
    assert!(!is_error, "{joined}");
    assert_eq!(joined, json!({"id": id, "status": "running"}));
    let board = env.project_root.join(".rimz/team/blackboard.md");
    std::fs::create_dir_all(board.parent().unwrap()).unwrap();
    std::fs::write(&board, "# Work\nStage: Done\n\n## Result\nshipped\n").unwrap();
    let (joined, is_error) = server.answer(12, "join", json!({"id": id, "timeout": "10s"}));
    assert!(!is_error, "{joined}");
    assert_eq!(
        joined,
        json!({"id": id, "status": "done", "result": "shipped"})
    );
    let (failure, is_error) = server.answer(13, "stop", json!({"id": "team:forge#rival-team"}));
    assert!(is_error, "{failure}");
    assert_eq!(
        failure,
        json!({
            "error": "team:forge#rival-team was started by another agent",
            "hint": "stop and join act on what you started, or what an agent you started \
                     started; `tasks` lists it",
        })
    );
    assert!(
        rival.try_wait().unwrap().is_none(),
        "the refused team runs on"
    );
    rival.kill().unwrap();
    rival.wait().unwrap();

    let (stopped, is_error) = server.answer(1, "stop", json!({"id": id}));
    assert!(!is_error, "{stopped}");
    assert_eq!(stopped, json!({"id": id, "status": "stopped"}));
    for seat in &mut seats {
        assert!(
            seat.try_wait().unwrap().is_some(),
            "every seat's process is gone"
        );
    }
    let (again, is_error) = server.answer(2, "stop", json!({"id": id}));
    assert!(!is_error, "{again}");
    assert_eq!(again, json!({"id": id, "status": "ended", "hint": GONE}));
}

/// An agent and a subagent the caller launched that have ended stop as `ended`, a value;
/// one another agent launched is refused, and left as it was.
#[test]
fn stop_answers_an_ended_child_as_a_value_and_refuses_another_agents() {
    use rimz::agents::{AgentLifecycleObservation, LaunchParams, LaunchedBy, LifecycleSignal};
    use rimz::ids::{AgentKind, AgentSessionId};
    use rimz::store::writer::AgentLifecycleIntent;

    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent(&env);
    let claude = AgentKind::new_unchecked("claude");
    let by = |launch: &str| {
        Some(Box::new(LaunchedBy {
            kind: claude.clone(),
            agent_id: AgentSessionId::from(launch),
        }))
    };
    let peer = |launch: &str| LaunchParams {
        launched_by: by(launch),
        ..LaunchParams::default()
    };
    let child = LaunchParams {
        parent_agent_id: Some(AgentSessionId::from("launch-session")),
        launch_depth: Some(1),
        ..LaunchParams::default()
    };
    let register = crate::common::wait::register_agent;
    register(
        &env,
        "peer-session",
        "launch-peer",
        "ended-peer",
        peer("launch-session"),
    );
    register(&env, "kid-session", "launch-kid", "ended-kid", child);
    register(
        &env,
        "other-session",
        "launch-other",
        "other",
        LaunchParams::default(),
    );
    register(
        &env,
        "far-session",
        "launch-far",
        "far-peer",
        peer("launch-other"),
    );
    let workspace = env.resolve_workspace(&env.project_root);
    for session in ["peer-session", "kid-session"] {
        let observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from(session)),
            LifecycleSignal::Ended,
        );
        env.store()
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: &workspace.session_name,
                agent_kind: claude.clone(),
                event_name: "SessionEnd",
                observation: &observation,
                spawned_subagents: &[],
            })
            .unwrap();
    }
    let mut server = serve_as_agent(&env);
    server.initialize();
    for (seq, id) in [(1, "agent:ended-peer"), (2, "subagent:ended-kid")] {
        let (stopped, is_error) = server.answer(seq, "stop", json!({"id": id}));
        assert!(!is_error, "{stopped}");
        assert_eq!(stopped, json!({"id": id, "status": "ended"}));
    }
    let (listed, is_error) = server.answer(3, "tasks", json!({}));
    assert!(!is_error, "{listed}");
    assert!(!task_ids(&listed).contains(&"agent:far-peer"), "{listed}");
    for (seq, tool) in [(4, "stop"), (5, "join")] {
        let (failure, is_error) = server.answer(seq, tool, json!({"id": "agent:far-peer"}));
        assert!(is_error, "{failure}");
        assert_eq!(
            failure,
            json!({
                "error": "agent:far-peer was started by another agent",
                "hint": "stop and join act on what you started, or what an agent you started \
                         started; `tasks` lists it",
            })
        );
    }
}

/// A workflow run the agent started: `join` answers `running` at its deadline, `stop`
/// cancels it, and until the room's reconcile moves its home, `join` and a second `stop`
/// read the cancelled run in the live home as ended. After the reconcile, its record is
/// the one `rimz workflow cancel` leaves, in the finished home.
#[test]
fn a_stopped_workflow_run_reads_as_ended_and_settles_as_a_cli_cancel_does() {
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent(&env);
    let run = seed_started_run(&env, "launch-session", json!({}));
    let by_cli = seed_started_run(&env, "launch-session", json!({}));
    let foreign = seed_started_run(&env, "launch-other", json!({}));
    let id = format!("workflow:{run}");
    let mut server = serve_as_agent(&env);
    server.initialize();
    let (listed, is_error) = server.answer(1, "tasks", json!({"kind": "workflow"}));
    assert!(!is_error, "{listed}");
    let row = listed["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == json!(id))
        .unwrap_or_else(|| panic!("{id} is listed: {listed}"));
    assert_eq!(row["status"], json!("running"), "{listed}");

    let began = Instant::now();
    let (joined, is_error) = server.answer(2, "join", json!({"id": id, "timeout": "1s"}));
    assert!(!is_error, "{joined}");
    assert_eq!(joined, json!({"id": id, "status": "running"}));
    assert!(
        began.elapsed() < Duration::from_secs(10),
        "{:?}",
        began.elapsed()
    );

    let (stopped, is_error) = server.answer(3, "stop", json!({"id": id}));
    assert!(!is_error, "{stopped}");
    assert_eq!(
        stopped,
        json!({"id": id, "status": "stopped", "still_running": []})
    );
    let (record, finished) = run_record(&env, &run);
    assert!(!finished, "the stop leaves the home for the reconcile");
    assert_eq!(record["state"], json!("canceled"), "{record:#}");
    let (joined, is_error) = server.answer(4, "join", json!({"id": id}));
    assert!(!is_error, "{joined}");
    let detail = format!("canceled · checkout {}", env.project_root.display());
    assert_eq!(
        joined,
        json!({"id": id, "status": "done", "detail": detail})
    );
    let (again, _) = server.answer(5, "stop", json!({"id": id}));
    assert_eq!(again, json!({"id": id, "status": "ended"}));

    let (failure, is_error) =
        server.answer(6, "stop", json!({"id": format!("workflow:{foreign}")}));
    assert!(is_error, "{failure}");
    assert_eq!(
        failure["error"],
        json!(format!("workflow:{foreign} was started by another agent"))
    );
    assert_eq!(run_record(&env, &foreign).0["state"], json!("active"));
    server.close();

    let output = env
        .rimz()
        .args(["workflow", "cancel", &by_cli])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let output = env.rimz().args(["workflow", "reconcile"]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let (record, finished) = run_record(&env, &run);
    let (cli, cli_finished) = run_record(&env, &by_cli);
    assert!(finished && cli_finished, "both homes are finished");
    let settled = |record: &Value| (record["state"].clone(), record["loop_report"].clone());
    assert_eq!(settled(&record), settled(&cli), "{record:#}\n{cli:#}");
}

/// A schedule the agent booked: `join` answers that its outcome comes by message, and
/// `stop` removes the booking while the run it already started runs on, named.
#[test]
fn stopping_a_schedule_removes_its_booking_and_leaves_its_run_running() {
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent(&env);
    let output = crate::common::wait::agent_wait(&env)
        .args(["loop", "add", "nightly", "--every", "1h", "--", "true"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let records: rimz::harness::schedule::task_record::TaskRecords =
        serde_json::from_slice(&std::fs::read(env.rimz_home().join("loops/tasks.json")).unwrap())
            .unwrap();
    let task = records
        .named_in(&env.project_root, "nightly")
        .expect("the booking's record")
        .0
        .clone();
    let run = seed_started_run(
        &env,
        "launch-unrelated",
        json!({"task": "nightly", "loop_fire": rimz::ids::FireId::new(&task)}),
    );
    let mut server = serve_as_agent(&env);
    server.initialize();
    let (listed, _) = server.answer(1, "tasks", json!({"kind": "schedule"}));
    assert_eq!(
        listed["tasks"][0]["id"],
        json!("schedule:nightly"),
        "{listed}"
    );
    let (joined, _) = server.answer(2, "join", json!({"id": "schedule:nightly"}));
    assert_eq!(
        joined,
        json!({
            "id": "schedule:nightly",
            "status": "running",
            "report": "its outcome comes to you as a message; join does not wait for it",
        })
    );
    let (stopped, is_error) = server.answer(3, "stop", json!({"id": "schedule:nightly"}));
    assert!(!is_error, "{stopped}");
    assert_eq!(
        stopped,
        json!({
            "id": "schedule:nightly",
            "status": "stopped",
            "still_running": [format!("workflow:{run}")],
        })
    );
    assert_eq!(run_record(&env, &run).0["state"], json!("active"));
    // The stop's write of `loop.toml` drops the server's memo of it, so the next read is fresh.
    let (listed, _) = server.answer(4, "tasks", json!({"kind": "schedule"}));
    assert_eq!(listed, json!({"tasks": []}));
    let (again, _) = server.answer(5, "stop", json!({"id": "schedule:nightly"}));
    assert_eq!(
        again,
        json!({"id": "schedule:nightly", "status": "ended", "hint": GONE})
    );

    // A switched-off schedule promises no message, and a stop that leaves nothing running
    // says so with an empty list.
    for args in [
        &["loop", "add", "weekly", "--every", "7d", "--", "true"][..],
        &["loop", "disable", "weekly"],
    ] {
        let output = crate::common::wait::agent_wait(&env)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    let (joined, _) = server.answer(6, "join", json!({"id": "schedule:weekly"}));
    assert_eq!(
        joined,
        json!({"id": "schedule:weekly", "status": "disabled"})
    );
    let (stopped, _) = server.answer(7, "stop", json!({"id": "schedule:weekly"}));
    assert_eq!(
        stopped,
        json!({"id": "schedule:weekly", "status": "stopped", "still_running": []})
    );

    // A schedule booked from outside the caller's tree is another's.
    let output = env
        .rimz()
        .args(["loop", "add", "hourly", "--every", "1h", "--", "true"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let (failure, is_error) = server.answer(8, "stop", json!({"id": "schedule:hourly"}));
    assert!(is_error, "{failure}");
    assert_eq!(
        failure,
        json!({
            "error": "schedule:hourly was started by another agent",
            "hint": "stop and join act on what you started, or what an agent you started \
                     started; `tasks` lists it",
        })
    );
}

/// An idle server holds the parked wrapper's private-memory budget.
#[cfg(target_os = "linux")]
#[test]
fn an_idle_server_stays_inside_its_memory_budget() {
    let env = Env::new();
    let mut server = serve(&env, &[]);
    server.initialize();
    server.request(1, "tools/list", json!({}));
    std::thread::sleep(Duration::from_millis(500));
    crate::common::assert_wrapper_memory_budget(
        "idle mcp server",
        server.pid(),
        crate::common::WRAPPER_BUDGET_MIB * 1024,
    );
}

/// The server as an agent's pane in the fixture's room starts it, through
/// `bin`, with the room pin set and the room record polled every 50 ms.
#[cfg(target_os = "linux")]
fn serve_in_room_at(env: &Env, bin: &std::path::Path) -> McpServer {
    let mut command = crate::common::wait::agent_wait_at(env, bin);
    command
        .args(["mcp", "serve"])
        .env("RIMZ_WORKSPACE_ID", env.workspace_id.as_str())
        .env("RIMZ_PROJECT_ROOT", &env.project_root)
        .env("RIMZ_TEST_SIDEBAR_RECORD_POLL_MS", "50");
    McpServer::spawn(command)
}

/// A server follows a change of its room's build in place, keeping its pid and
/// its pipes: the new image first announces a changed tool list, then answers,
/// and a wait watcher the old image armed is reaped by the new one when it ends.
#[cfg(target_os = "linux")]
#[test]
fn a_server_follows_its_rooms_build_in_place_and_announces_its_tools() {
    use crate::common::builds::{RoomBuilds, executable};

    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let builds = RoomBuilds::new(&env);
    env.install_agent_hooks("claude");
    crate::common::wait::register_calling_agent(&env);
    let mut server = serve_in_room_at(&env, &builds.first);
    server.initialize();
    let (start, is_error) = server.answer(1, "wait", json!({"trigger": "run", "value": "sleep 3"}));
    assert!(!is_error, "{start}");
    // The call's thread may still be handing its children to the main thread.
    let deadline = Instant::now() + Duration::from_secs(5);
    while children(server.pid()).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let watchers = children(server.pid());
    assert_eq!(
        watchers.len(),
        1,
        "the wait's watcher is the server's child"
    );

    builds.record(&env, &builds.first);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(executable(server.pid()), builds.first);
    builds.record(&env, &builds.second);
    let deadline = Instant::now() + REPLY_DEADLINE;
    while executable(server.pid()) != builds.second && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(
        executable(server.pid()),
        builds.second,
        "the server did not follow"
    );
    server.list_changed();
    assert_eq!(server.request(2, "ping", json!({}))["result"], json!({}));

    let deadline = Instant::now() + REPLY_DEADLINE;
    while !children(server.pid()).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        children(server.pid()),
        Vec::<String>::new(),
        "the old image's watcher {watchers:?} was left unreaped"
    );
    let (status, lines) = server.close();
    assert_eq!(status.code(), Some(0), "{status:?}");
    assert_protocol_lines(&lines);
}

/// A server never replaces itself while a call is in flight, and a request
/// written right behind the last reply is answered, by whichever image reads it.
#[cfg(target_os = "linux")]
#[test]
fn a_server_swaps_builds_only_when_quiet_and_loses_no_request() {
    use crate::common::builds::{RoomBuilds, executable};

    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let builds = RoomBuilds::new(&env);
    let checkout = checkout_with_fifo(&env, "follow-5b1e");
    let mut server = serve_in_room_at(&env, &builds.first);
    server.initialize();
    server.start_call(
        1,
        "lsp",
        json!({"verb": "check", "targets": ["held.md"], "cwd": checkout}),
    );
    let held = HeldFifo::when_read(&checkout);
    builds.record(&env, &builds.second);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        executable(server.pid()),
        builds.first,
        "the server swapped with a call in flight"
    );

    held.release("no anchors\n");
    let (text, is_error) = tool_answer("lsp", &server.reply(&json!(1)));
    assert!(!is_error, "{text}");
    assert_eq!(server.request(2, "ping", json!({}))["result"], json!({}));
    server.list_changed();
    assert_eq!(executable(server.pid()), builds.second);
    assert_eq!(server.request(3, "ping", json!({}))["result"], json!({}));
}

/// A cancelled `join` stops waiting within a poll: its call leaves the server's calls in
/// flight, so the build the room moved to is taken at once, long before the join's deadline.
#[cfg(target_os = "linux")]
#[test]
fn a_cancelled_join_frees_its_call_within_a_poll() {
    use crate::common::builds::{RoomBuilds, executable};

    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let builds = RoomBuilds::new(&env);
    crate::common::wait::register_calling_agent(&env);
    let run = seed_started_run(&env, "launch-session", json!({}));
    let mut server = serve_in_room_at(&env, &builds.first);
    server.initialize();
    server.start_call(
        1,
        "join",
        json!({"id": format!("workflow:{run}"), "timeout": "9m"}),
    );
    std::thread::sleep(Duration::from_millis(200));
    builds.record(&env, &builds.second);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        executable(server.pid()),
        builds.first,
        "the server swapped with a join in flight"
    );
    server.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": 1, "reason": "test"},
    }));
    let cancelled = Instant::now();
    while executable(server.pid()) != builds.second && cancelled.elapsed() < REPLY_DEADLINE {
        std::thread::sleep(Duration::from_millis(25));
    }
    // The join reads its cancel each half-second poll and the quiet server swaps within its
    // 50 ms idle poll; the rest of the bound is slack for the exec on a loaded host.
    assert!(
        cancelled.elapsed() < Duration::from_millis(1500),
        "the cancelled join held its call for {:?}",
        cancelled.elapsed()
    );
    server.list_changed();
    assert_eq!(server.request(2, "ping", json!({}))["result"], json!({}));
    let (_, lines) = server.close();
    assert!(
        lines.iter().all(|line| !line.contains("\"id\":1,")),
        "a cancelled join is not answered: {lines:?}"
    );
}
