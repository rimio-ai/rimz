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

const ALL_TOOLS: [&str; 21] = [
    "lsp",
    "shell",
    "watch",
    "subagent",
    "agent",
    "team",
    "workflow",
    "schedule",
    "message",
    "ask",
    "tasks",
    "join",
    "stop",
    "self_stop",
    "self_compact",
    "self_tune",
    "self_molt",
    "notes",
    "blackboard_flip",
    "blackboard_record",
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
        .map(|line| {
            line.split_whitespace()
                .skip(3)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_else(|| panic!("no {tool} row in {out}"))
}

/// A spawned server, leading its own process group as a provider starts one.
pub(crate) struct McpServer {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    seen: Vec<String>,
    unclaimed: Vec<Value>,
}

impl McpServer {
    pub(crate) fn spawn(mut command: Command) -> Self {
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

    pub(crate) fn initialize(&mut self) -> Value {
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
    pub(super) fn call(&mut self, id: i64, tool: &str, arguments: Value) -> (String, bool) {
        self.start_call(id, tool, arguments);
        tool_result(&self.reply(&json!(id)))
    }

    /// A tool call's answer, read as `tool_answer` reads it.
    pub(crate) fn answer(&mut self, id: i64, tool: &str, arguments: Value) -> (Value, bool) {
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

pub(super) fn serve(env: &Env, args: &[&str]) -> McpServer {
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

/// A live room with a git checkout, a `worker` profile, and a `solo` team, whose servers run
/// from a directory outside it as the agent `parent-launch` or the subagent `child-launch`.
#[cfg(unix)]
struct LayoutRoom {
    env: Env,
    store: rimz::Store,
    session: String,
    agent_bin: std::path::PathBuf,
    shell: std::path::PathBuf,
    presence: std::path::PathBuf,
    trace: std::path::PathBuf,
    outside: tempfile::TempDir,
}

#[cfg(unix)]
impl LayoutRoom {
    fn new() -> Self {
        use crate::common::git::git;
        use crate::common::room::{MATERIALIZED_ROOM_PANES, seed_live_zellij_room};
        use rimz::agents::LaunchParams;
        use rimz::ids::{AgentKind, AgentSessionId};

        let env = Env::new();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "rimz@example.test"],
            &["config", "user.name", "RimZ Test"],
            &["commit", "-q", "--allow-empty", "-m", "init"],
        ] {
            git(&env.project_root, args);
        }
        env.install_agent_hooks("claude");
        crate::common::trust_claude_project(&env, &env.project_root);
        std::fs::write(
            env.rimz_home().join("config.toml"),
            "[agents]\nisolation = 'host'\n",
        )
        .unwrap();
        crate::common::write_definition(
            &env,
            "agents",
            "worker",
            "description: Worker\nagent: claude\ntools: []",
            "",
        );
        crate::common::write_definition(
            &env,
            "teams",
            "solo",
            "layout: lead\nleader: lead\nstages: [Build]\nroles:\n  - {role: lead, agent: worker, owns: [Build]}",
            "Complete the work.",
        );
        let agent_bin = crate::common::write_failing_agent_shim(&env, "claude", 1);
        let shell = crate::common::write_fake_login_shell(&env, "rimz-test-sh", &[]);
        let workspace = env.resolve_workspace(&env.project_root);
        let store = env.store();
        store.record_workspace(&workspace).expect("record room");
        for (launch, name, pane, launch_params) in [
            (
                "parent-launch",
                "parent",
                "terminal_2",
                LaunchParams::default(),
            ),
            (
                "child-launch",
                "child",
                "terminal_1",
                LaunchParams {
                    parent_agent_id: Some(AgentSessionId::from("elsewhere-launch")),
                    parent_agent_kind: Some(AgentKind::new_unchecked("claude")),
                    launch_depth: Some(1),
                    ..LaunchParams::default()
                },
            ),
        ] {
            seed_agent(
                &store,
                &workspace,
                launch,
                name,
                pane,
                launch_params,
                &env.project_root,
            );
        }
        seed_live_zellij_room(
            store.runtime_paths(),
            &workspace.session_name,
            serde_json::from_str(MATERIALIZED_ROOM_PANES).unwrap(),
        );
        let presence = env.home_root.join("presence.wasm");
        std::fs::write(&presence, b"test-presence").expect("write presence fixture");
        let trace = env.home_root.join("launch-trace.log");
        Self {
            session: workspace.session_name,
            store,
            agent_bin,
            shell,
            presence,
            trace,
            outside: tempfile::tempdir().expect("a directory outside the room"),
            env,
        }
    }

    fn serve(&self, launch: &str) -> McpServer {
        use crate::common::room::MATERIALIZED_ROOM_PANES;

        let mut command = self.env.rimz();
        command
            .current_dir(self.outside.path())
            .args(["mcp", "serve", "--launch-env"])
            .env("RIMZ_MCP_SERVE", "rimz-agents,rimz-teams")
            .env("RIMZ_MCP_CWD", &self.env.project_root)
            .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
            .env(rimz::harness::launch::ENV_AGENT_ID, launch)
            .env("SHELL", &self.shell)
            .env("PATH", crate::common::path_with_front(&self.agent_bin))
            .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
            .env("RIMZ_TEST_ZELLIJ_LOG", &self.trace)
            .env("RIMZ_TEST_ZELLIJ_VERSION", "0.45.1")
            .env("RIMZ_PRESENCE_PLUGIN", &self.presence)
            .env("ZELLIJ_PANE_ID", "2")
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                format!("{} [Created 1s ago]\n", self.session),
            )
            .env("RIMZ_TEST_ZELLIJ_LIST_PANES", MATERIALIZED_ROOM_PANES);
        let mut server = McpServer::spawn(command);
        server.initialize();
        server
    }

    fn agents(&self) -> Vec<rimz::agents::AgentState> {
        self.store
            .runtime_projection(rimz::RuntimeScope::Audit)
            .expect("launch history")
            .agents
    }

    /// Registers launched `seat` as live in zellij pane `pane`, its provider process the
    /// returned sleep, as its provider's session start would.
    fn register(&self, seat: &rimz::agents::AgentState, pane: &str) -> std::process::Child {
        use crate::common::ScrubSessionEnvExt;

        let process = Command::new("sleep")
            .scrub_session_env()
            .arg("30")
            .spawn()
            .unwrap();
        let session = format!("{}-session", seat.name.as_deref().expect("a seat name"));
        let mut observation = rimz::agents::AgentLifecycleObservation::new(
            Some(session.as_str().into()),
            rimz::agents::LifecycleSignal::Registered,
        );
        observation.agent_name.clone_from(&seat.name);
        observation.launch_id = Some(seat.agent_id.clone());
        observation.worktree_path.clone_from(&seat.worktree_path);
        observation.pane_id = Some(rimz::ids::PaneId::from_parts(
            rimz::ids::MuxName::Zellij,
            pane,
        ));
        observation.runtime_owner = Some(rimz::store::runtime::process_owner(
            rimz::pane::RuntimeOwnerKind::Agent,
            &session,
            process.id(),
        ));
        observation.launch = rimz::agents::LaunchParams {
            team: seat.team.clone(),
            role: seat.role.clone(),
            lane: seat.lane.clone(),
            ..launched_by("parent-launch")
        };
        let workspace = self.env.resolve_workspace(&self.env.project_root);
        self.store
            .append_event(&rimz::store::event::EventEnvelope::agent_lifecycle(
                workspace.workspace_id,
                workspace.session_name,
                "claude",
                "SessionStart",
                &observation,
            ))
            .expect("the seat goes live");
        process
    }
}

/// An `agent` call opens a persistent peer started by the caller in a new tab, from a server
/// whose process directory is outside the room, and the answered id works in `tasks`, `join`,
/// and `stop`. Every refusal answers before anything launches.
#[cfg(unix)]
#[test]
fn an_agent_call_opens_a_peer_of_the_caller_from_outside_the_room() {
    let room = LayoutRoom::new();
    let rows = room.agents().len();
    let call = json!({"profile": "worker", "prompt": "audit the parser", "description": "audit"});

    let (answer, is_error) = room.serve("child-launch").answer(1, "agent", call.clone());
    assert!(is_error, "{answer}");
    assert_eq!(
        answer,
        json!({"error": "launch refused: subagents cannot launch agents or subagents. Do the work \
            yourself and report the result to your caller; do not retry this command."})
    );
    let (answer, is_error) = room
        .serve("unknown-launch")
        .answer(1, "agent", call.clone());
    assert!(is_error, "{answer}");
    assert_eq!(
        answer["error"],
        json!(
            "launch refused: RimZ could not resolve the calling agent's durable launch identity, \
             so it cannot safely verify the configured chain limit. Launching another agent from \
             here is not permitted; do not retry this command."
        ),
    );
    let mut server = room.serve("parent-launch");
    for (id, profile, error) in [
        (2, "solo", "`solo` is a team; launch it with `team`"),
        (
            3,
            "worker,worker",
            "`worker,worker` is a layout of several seats; `agent` launches one",
        ),
    ] {
        let (answer, is_error) =
            server.answer(id, "agent", json!({"profile": profile, "prompt": "p"}));
        assert!(is_error, "{answer}");
        assert_eq!(answer["error"], json!(error), "{answer}");
    }
    assert_eq!(room.agents().len(), rows, "a refusal launches nothing");

    let (answer, is_error) = server.answer(4, "agent", call);
    assert!(!is_error, "{answer}");
    let agents = room.agents();
    let peer = agents
        .iter()
        .find(|agent| agent.profile.as_deref() == Some("worker"))
        .expect("the peer's launch row");
    let name = peer.name.clone().expect("a handle");
    let id = format!("agent:{name}");
    assert_eq!(answer["id"], json!(id), "{answer}");
    assert_eq!(answer["status"], json!("running"), "{answer}");
    assert!(
        answer["report"].as_str().unwrap().contains("AGENT_REPORT"),
        "{answer}"
    );
    assert_eq!(
        peer.launcher().map(|(_, id)| id.to_string()),
        Some("parent-launch".to_owned()),
    );
    assert_eq!(
        std::fs::canonicalize(peer.worktree_path.as_deref().unwrap()).unwrap(),
        std::fs::canonicalize(&room.env.project_root).unwrap()
    );
    let trace = std::fs::read_to_string(&room.trace).unwrap();
    assert!(trace.contains("new-tab"), "{trace}");

    let (listed, is_error) = server.answer(5, "tasks", json!({"kind": "agent"}));
    assert!(!is_error, "{listed}");
    assert_eq!(task_ids(&listed), [id.as_str()], "{listed}");
    let (joined, is_error) = server.answer(6, "join", json!({"id": id, "timeout": "1s"}));
    assert!(!is_error, "{joined}");
    assert_eq!(joined["status"], json!("running"), "{joined}");
    let (stopped, is_error) = server.answer(7, "stop", json!({"id": id}));
    assert!(!is_error, "{stopped}");
    assert_eq!(stopped["id"], json!(id), "{stopped}");
}

/// An `agent` call with `stay: false`, a `schema`, and a `timeout` opens one task that closes
/// once its turn ends, and records the call its answer and deadline are read from.
#[cfg(unix)]
#[test]
fn an_agent_call_for_one_task_records_its_schema_and_deadline() {
    let room = LayoutRoom::new();
    let mut server = room.serve("parent-launch");
    let schema = json!({"type": "object", "required": ["verdict"]});
    let call = json!({
        "profile": "worker",
        "prompt": "audit the parser",
        "name": "tasker",
        "worktree": "audit",
        "stay": false,
        "schema": schema,
        "timeout": "30m",
    });
    let (refused, is_error) = server.answer(
        1,
        "agent",
        json!({"profile": "worker,worker", "prompt": "p", "stay": false}),
    );
    assert!(is_error, "{refused}");
    assert_eq!(
        refused["error"],
        json!("`worker,worker` is a layout of several seats; `agent` launches one"),
        "{refused}"
    );
    let (answer, is_error) = server.answer(2, "agent", call);
    assert!(!is_error, "{answer}");
    assert_eq!(answer["status"], json!("running"), "{answer}");
    assert_eq!(
        answer["report"],
        json!("an AGENT_REPORT message to you once its turn ends, then it closes"),
        "{answer}"
    );
    assert_eq!(answer["id"], json!("agent:tasker"), "{answer}");
    let agents = room.agents();
    let worker = agents
        .iter()
        .find(|agent| agent.profile.as_deref() == Some("worker"))
        .expect("the worker's launch row");
    assert_eq!(worker.name.as_deref(), Some("tasker"));
    let checkout = worker.worktree_path.as_deref().expect("a checkout");
    assert!(
        checkout.ends_with("/audit") && checkout != room.env.project_root.display().to_string(),
        "`worktree` places it in its own checkout, as on the peer path: {checkout}"
    );
    let trace = std::fs::read_to_string(&room.trace).unwrap();
    let tabs: Vec<&str> = trace
        .lines()
        .filter(|line| line.contains("new-tab"))
        .collect();
    assert!(
        tabs.iter().any(|line| line.contains("#audit")),
        "a background tab titled by its lane: {tabs:?}"
    );
    assert!(!trace.contains("run claude"), "{tabs:?}");
    let launch_id = worker.launch_id.clone().expect("a launch id");
    let record = rimz::store::call::load(room.store.paths(), &launch_id)
        .unwrap()
        .expect("the call record");
    assert!(!record.stay, "{record:?}");
    assert_eq!(
        record.schema.clone().map(Value::Object),
        Some(schema),
        "{record:?}"
    );
    assert_eq!(
        record
            .deadline
            .map(|deadline| deadline.duration_since(record.created_at)),
        Some(jiff::SignedDuration::from_mins(30)),
    );
    assert!(record.owes_answer());
}

/// A `team` call opens the team's cohort in the named worktree, through a real launch: the
/// answered id is the seats' team instance, and `tasks`, `join`, and `stop` act on it. A second
/// call onto the live cohort says its brief went to the leader, so it is not sent again.
#[cfg(unix)]
#[test]
fn a_team_call_opens_its_cohort_in_the_named_worktree() {
    let room = LayoutRoom::new();
    let rows = room.agents().len();
    let mut server = room.serve("parent-launch");
    for (id, team, error) in [
        (
            1,
            "worker",
            "unknown team `worker`; configured teams: peer, solo",
        ),
        (
            2,
            "solo.lead",
            "`solo.lead` names one role; launch it with `agent`",
        ),
    ] {
        let (answer, is_error) = server.answer(
            id,
            "team",
            json!({"team": team, "prompt": "p", "worktree": "demo"}),
        );
        assert!(is_error, "{answer}");
        assert_eq!(answer["error"], json!(error), "{answer}");
    }
    assert_eq!(room.agents().len(), rows, "a refusal launches nothing");

    let call = json!({"team": "solo", "prompt": "build it", "worktree": "demo"});
    let (answer, is_error) = server.answer(3, "team", call.clone());
    assert!(!is_error, "{answer}");
    let agents = room.agents();
    let seat = agents
        .iter()
        .find(|agent| agent.team.as_deref() == Some("solo"))
        .expect("the seat's launch row");
    let id = format!("team:{}", seat.team_instance().expect("a team instance"));
    assert_eq!(id, "team:solo#demo");
    assert_eq!(answer["id"], json!(id), "{answer}");
    assert!(
        answer["report"].as_str().unwrap().contains("TEAM_REPORT"),
        "{answer}"
    );
    assert!(room.env.home_root.join("project-worktrees/demo").exists());

    let (listed, is_error) = server.answer(4, "tasks", json!({"kind": "team"}));
    assert!(!is_error, "{listed}");
    assert_eq!(task_ids(&listed), [id.as_str()], "{listed}");
    let (joined, is_error) = server.answer(5, "join", json!({"id": id, "timeout": "1s"}));
    assert!(!is_error, "{joined}");
    assert_eq!(joined["status"], json!("running"), "{joined}");

    let mut process = room.register(seat, "terminal_3");
    let (again, is_error) = server.answer(6, "team", call);
    process.kill().unwrap();
    process.wait().unwrap();
    assert!(is_error, "{again}");
    assert_eq!(
        again["error"],
        json!(
            "the cohort already live in this worktree took your brief: it went to its leader, and nothing new started"
        ),
        "{again}"
    );
    assert_eq!(
        again["hint"],
        json!("do not send the brief again; its leader answers in its own turn"),
    );
    let queued = room.store.list_messages().expect("the message queue");
    assert!(
        queued.iter().any(|message| message.text == "build it"),
        "{queued:?}"
    );

    let (stopped, is_error) = server.answer(7, "stop", json!({"id": id}));
    assert!(!is_error, "{stopped}");
    assert_eq!(stopped["id"], json!(id), "{stopped}");
}

/// A worktree whose cohort has closed refuses a plain launch, and the hint names what the called
/// tool offers: `agent` has `fresh`, which then launches, and `team` does not.
///
/// The seat never goes live, so the refusal comes from `worktree_entry::check`; a `from_pr` call
/// skips that check and reaches the closed-cohort branch only for a seat that went live and ended,
/// as `a_closed_pull_request_worktree_refusal_hints_a_retry_the_tool_accepts` shows.
#[cfg(unix)]
#[test]
fn a_closed_worktree_refusal_hints_in_the_called_tools_terms() {
    let room = LayoutRoom::new();
    let mut server = room.serve("parent-launch");
    let call = json!({"profile": "worker", "prompt": "p", "worktree": "closed-agent"});
    let (opened, is_error) = server.answer(1, "agent", call.clone());
    assert!(!is_error, "{opened}");
    let (refused, is_error) = server.answer(2, "agent", call.clone());
    assert!(is_error, "{refused}");
    assert_eq!(
        refused["hint"],
        json!("set `fresh` to start a new session in this worktree"),
        "{refused}"
    );
    let mut fresh = call;
    fresh["fresh"] = json!(true);
    let (relaunched, is_error) = server.answer(3, "agent", fresh);
    assert!(!is_error, "{relaunched}");

    let call = json!({"team": "solo", "prompt": "p", "worktree": "closed-team"});
    let (opened, is_error) = server.answer(4, "team", call.clone());
    assert!(!is_error, "{opened}");
    let (refused, is_error) = server.answer(5, "team", call);
    assert!(is_error, "{refused}");
    assert_eq!(
        refused["hint"],
        json!("name another `worktree`"),
        "{refused}"
    );
}

/// A pull request's worktree whose cohort has closed refuses a team seat or team launched with
/// `from_pr`, and the hint names a retry the called tool accepts: `agent` cannot take `fresh`
/// with `from_pr`, so its hint names the worktree, which then launches; `team` has no `fresh`.
#[cfg(unix)]
#[test]
fn a_closed_pull_request_worktree_refusal_hints_a_retry_the_tool_accepts() {
    use crate::common::git::{configure_github_origin_rewrite, publish_pr_ref};

    let room = LayoutRoom::new();
    publish_pr_ref(&room.env, "refs/pull/1/head");
    configure_github_origin_rewrite(&room.env);
    crate::common::write_path_shim(
        &room.agent_bin,
        "gh",
        &format!("printf '%s\\n' '{}'", crate::common::gh_same_repo_head()),
    );
    let mut server = room.serve("parent-launch");
    let call = json!({"profile": "solo.lead", "prompt": "p", "from_pr": "1"});
    let (opened, is_error) = server.answer(1, "agent", call.clone());
    assert!(!is_error, "{opened}");
    let agents = room.agents();
    let seat = agents
        .iter()
        .find(|agent| agent.team.as_deref() == Some("solo"))
        .expect("the seat's launch row");
    let mut process = room.register(seat, "terminal_3");
    process.kill().unwrap();
    process.wait().unwrap();
    let (refused, is_error) = server.answer(2, "agent", call);
    assert!(is_error, "{refused}");
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .starts_with("worktree `pr-1` "),
        "{refused}"
    );
    assert_eq!(
        refused["hint"],
        json!(
            "to start a new session in this worktree, call again with `worktree` \"pr-1\" and `fresh` in place of `from_pr`"
        ),
        "{refused}"
    );
    let retry = json!({"profile": "solo.lead", "prompt": "p", "worktree": "pr-1", "fresh": true});
    let (relaunched, is_error) = server.answer(3, "agent", retry);
    assert!(!is_error, "{relaunched}");

    let (refused, is_error) = server.answer(
        4,
        "team",
        json!({"team": "solo", "prompt": "p", "from_pr": "1"}),
    );
    assert!(is_error, "{refused}");
    assert_eq!(
        refused["hint"],
        json!(
            "`team` cannot start a new session in this worktree; name another `worktree` in place of `from_pr` to launch outside the pull request"
        ),
        "{refused}"
    );
}

/// A caller whose recorded checkout is in another repository gets the tools' cross-repository
/// refusal from `agent` and `team`, worded for a tool caller, and no worktree appears in the
/// room's repository.
#[cfg(unix)]
#[test]
fn a_caller_in_another_repository_gets_the_cross_repository_refusal() {
    use crate::common::git::git;

    let room = LayoutRoom::new();
    let other = room.env.home_root.join("other-repo");
    std::fs::create_dir_all(&other).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "rimz@example.test"],
        &["config", "user.name", "RimZ Test"],
        &["commit", "-q", "--allow-empty", "-m", "init"],
    ] {
        git(&other, args);
    }
    let other = std::fs::canonicalize(&other).unwrap();
    crate::common::trust_claude_project(&room.env, &other);
    let workspace = room.env.resolve_workspace(&room.env.project_root);
    seed_agent(
        &room.store,
        &workspace,
        "stranger-launch",
        "stranger",
        "terminal_2",
        rimz::agents::LaunchParams::default(),
        &other,
    );
    let mut server = room.serve("stranger-launch");
    let room_root = std::fs::canonicalize(&room.env.project_root).unwrap();
    let refusal = format!(
        "this agent's checkout is in another repository than room root {}; `worktree` works \
         only from the room's checkout",
        room_root.display()
    );
    for (id, tool, call) in [
        (
            1,
            "agent",
            json!({"profile": "worker", "prompt": "p", "worktree": "elsewhere-agent"}),
        ),
        (
            2,
            "team",
            json!({"team": "solo", "prompt": "p", "worktree": "elsewhere-team"}),
        ),
    ] {
        let name = call["worktree"].as_str().unwrap().to_owned();
        let (answer, is_error) = server.answer(id, tool, call);
        assert!(is_error, "{answer}");
        assert_eq!(answer["error"], json!(refusal), "{answer}");
        assert!(
            !room
                .env
                .home_root
                .join("project-worktrees")
                .join(&name)
                .exists(),
            "{tool} made a worktree in the room's repository"
        );
    }
}

/// A seat of a team launched through `agent`, by a literal `<team>.<role>` or by a bare role the
/// caller's lane qualifies, answers its `agent:` id with the way to its answer: no AGENT_REPORT
/// comes, `join` on the id answers, and `tasks` lists the seat under its team.
#[cfg(unix)]
#[test]
fn an_agent_call_that_seats_a_team_says_how_its_answer_comes() {
    let room = LayoutRoom::new();
    crate::common::write_definition(
        &room.env,
        "teams",
        "duo",
        "layout: lead,helper\nleader: lead\nstages: [Build]\nroles:\n  - {role: lead, agent: \
         worker, owns: [Build]}\n  - {role: helper, agent: worker}",
        "Complete the work.",
    );
    let seat_answer = |answer: &Value, role: &str| {
        let agents = room.agents();
        let seat = agents
            .iter()
            .find(|agent| {
                agent.team.as_deref() == Some("duo") && agent.role.as_deref() == Some(role)
            })
            .unwrap_or_else(|| panic!("the {role} seat's launch row: {answer}"))
            .clone();
        let team = format!("team:{}", seat.team_instance().expect("a team instance"));
        assert_eq!(
            answer["id"],
            json!(format!("agent:{}", seat.name.as_deref().unwrap())),
            "{answer}"
        );
        assert_eq!(
            answer["report"],
            json!(format!(
                "no AGENT_REPORT comes, since this agent is a seat of team `duo`: `join` on the \
                 id is the way to its answer, and `tasks` lists it under `{team}`"
            )),
            "{answer}"
        );
        (seat, team)
    };

    let mut server = room.serve("parent-launch");
    let (answer, is_error) = server.answer(
        1,
        "agent",
        json!({"profile": "duo.lead", "prompt": "lead it", "worktree": "duo-lane"}),
    );
    assert!(!is_error, "{answer}");
    let (lead, team) = seat_answer(&answer, "lead");
    assert_eq!(team, "team:duo#duo-lane");
    let (listed, is_error) = server.answer(2, "tasks", json!({}));
    assert!(!is_error, "{listed}");
    assert_eq!(task_ids(&listed), [team.as_str()], "{listed}");

    let mut live = room.register(&lead, "terminal_3");
    let mut seat = room.serve(lead.agent_id.as_str());
    let (answer, is_error) =
        seat.answer(1, "agent", json!({"profile": "helper", "prompt": "help"}));
    assert!(!is_error, "{answer}");
    let (_, helper_team) = seat_answer(&answer, "helper");
    assert_eq!(helper_team, team);
    let (listed, is_error) = seat.answer(2, "tasks", json!({}));
    assert!(!is_error, "{listed}");
    assert_eq!(task_ids(&listed), [team.as_str()], "{listed}");
    live.kill().unwrap();
    live.wait().unwrap();
}

/// A bad `name` is refused before anything opens: no worktree appears.
#[cfg(unix)]
#[test]
fn an_agent_call_with_a_bad_name_is_refused_before_a_worktree_exists() {
    let room = LayoutRoom::new();
    let (answer, is_error) = room.serve("parent-launch").answer(
        1,
        "agent",
        json!({"profile": "worker", "prompt": "p", "name": "bad name", "worktree": "named"}),
    );
    assert!(is_error, "{answer}");
    assert_eq!(
        answer["error"],
        json!("invalid agent name `bad name`; use ASCII letters, numbers, and `-`"),
        "{answer}"
    );
    assert!(!room.env.home_root.join("project-worktrees/named").exists());
}

/// Records agent `name` of launch `launch` as bound to `pane` in its checkout `worktree`.
#[cfg(unix)]
fn seed_agent(
    store: &rimz::Store,
    workspace: &rimz::ResolvedWorkspace,
    launch: &str,
    name: &str,
    pane: &str,
    launch_params: rimz::agents::LaunchParams,
    worktree: &std::path::Path,
) {
    use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId};
    use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope};

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
                worktree_path: Some(worktree.display().to_string()),
                worktree_branch: None,
                prompt: None,
                description: None,
            },
        ))
        .expect("seed an agent");
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
        output.contains("Claude, Codex, Antigravity, OpenCode, and Pi launches"),
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
        .env("RIMZ_MCP_SERVE", "rimz-watch:always,rimz-lsp")
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
            (json!("watch"), always),
            (json!("tasks"), None),
            (json!("join"), None),
            (json!("stop"), None),
            (json!("guide"), None),
        ]
    );
    let (text, is_error) = server.call(2, "tasks", json!({}));
    assert!(is_error, "{text}");
    assert!(
        text.contains(&format!(
            "no room at {}: run `rimz start` there\"",
            launched.display()
        )),
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
        .env("RIMZ_MCP_SERVE", "rimz-watch,github")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "RIMZ_MCP_SERVE entry `github` is not a RimZ capability (rimz-lsp, rimz-shell, \
             rimz-watch, rimz-subagents, rimz-agents, rimz-teams, rimz-workflow, rimz-loop, \
             rimz-message, rimz-ask, rimz-control, rimz-notes, rimz-blackboard) with an optional \
             `:always` or `:deferred`, named once"
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
        "rimz-watch",
        "--only",
        "rimz-notes",
    ];
    let marks = ["--always", "rimz-lsp", "--deferred", "rimz-watch"];
    assert_eq!(
        meta(&[&only[..], &marks].concat()),
        [
            (json!("lsp"), always),
            (json!("watch"), deferred),
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

    let (status, stdout, stderr) = run(&["--json", "--only", "rimz-watch"]);
    assert!(status.success(), "{stderr}");
    let printed: Value = serde_json::from_str(&stdout).expect("tools json");
    let mut server = serve(&env, &["--only", "rimz-watch"]);
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

    let (status, _, stderr) = run(&["--only", "rimz-chat"]);
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains(
            "expected one of rimz-lsp, rimz-shell, rimz-watch, rimz-subagents, rimz-agents, rimz-teams, rimz-workflow, rimz-loop, rimz-message, rimz-ask, rimz-control, rimz-notes, rimz-blackboard"
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
    server.start_call(2, "watch", json!({"when": "pr=merged"}));
    let wait = server.reply(&json!(2));
    assert!(wait["error"]["code"].is_i64(), "{wait}");
    server.start_call(3, "no_such_tool", json!({}));
    assert!(server.reply(&json!(3))["error"]["code"].is_i64());
    server.start_call(4, "guide", json!({"name": "rimz-watch/conditions"}));
    let guide = server.reply(&json!(4));
    assert_eq!(guide["error"]["code"], -32602, "{guide}");

    let output = env
        .rimz()
        .args(["mcp", "serve", "--only", "rimz-nope"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}

/// `guide` is served beside a group that has a guide, answers a served name
/// with the guide's text, and any other name with the guides the launch has.
#[test]
fn guide_answers_a_served_guide_and_lists_the_served_guides_for_any_other() {
    let env = Env::new();
    let mut server = serve(&env, &["--only", "rimz-watch"]);
    server.initialize();
    let tools = server.request(1, "tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["watch", "tasks", "join", "stop", "guide"]);

    let (text, is_error) = server.call(2, "guide", json!({"name": "rimz-watch/conditions"}));
    assert!(!is_error, "{text}");
    assert_eq!(
        Ok(text.as_str()),
        Wording::built_in().guide(&[Capability::Watch], "rimz-watch/conditions")
    );
    for (id, name) in [(3, "rimz-watch/none"), (4, "rimz-lsp/conditions")] {
        let (failure, is_error) = server.answer(id, "guide", json!({"name": name}));
        assert!(is_error, "{failure}");
        assert_eq!(
            failure,
            json!({
                "error": format!("no guide {name:?}"),
                "hint": "this launch has: rimz-watch/conditions",
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
    let mcps = env.rimz_home().join("mcps/rimz-watch");
    std::fs::create_dir_all(&mcps).unwrap();
    let list = format!(
        "Wait outside your turn, replaced. {}",
        "Each wait, its name. ".repeat(40)
    );
    std::fs::write(mcps.join("watch.md"), format!("{list}\n")).unwrap();
    std::fs::write(mcps.join("instructions.md"), "Use `watch`, replaced.\n").unwrap();

    let mut server = serve(&env, &["--only", "rimz-watch"]);
    let init = server.initialize();
    let instructions = init["result"]["instructions"].as_str().unwrap().to_owned();
    assert_eq!(
        instructions.lines().nth(1),
        Some("Use `watch`, replaced."),
        "{instructions}"
    );
    let listing = server.request(1, "tools/list", json!({}));
    let described = listing["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "watch")
        .map(|tool| tool["description"].clone());
    assert_eq!(described, Some(json!(list.trim())));
    server.close();

    let output = env
        .rimz()
        .args(["mcp", "tools", "--json", "--only", "rimz-watch"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let printed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(printed, listing["result"]);
    let output = env.rimz().args(["mcp", "tools"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let tokens: u64 = stdout
        .lines()
        .find(|line| line.split_whitespace().nth(1) == Some("watch"))
        .and_then(|line| line.split_whitespace().last())
        .and_then(|cell| cell.strip_prefix('~')?.parse().ok())
        .unwrap_or_else(|| panic!("no watch row: {stdout}"));
    assert!(tokens > 150, "{stdout}");
    assert!(output.stderr.is_empty(), "{output:?}");

    crate::common::write_definition(
        &env,
        "agents",
        "cwait",
        "description: cwait profile\nagent: claude\ntools: [Bash]\nmcps: [rimz-watch]",
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
            "· rimz-watch · load default · text {}\n",
            mcps.join("watch.md").display()
        )),
        "{out}"
    );
    assert!(out.contains("## watch\n~"), "{out}");
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
        "description: cwait profile\nagent: claude\ntools: [Bash]\nmcps: [rimz-watch]",
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

    std::fs::create_dir_all(mcps.join("rimz-watch")).unwrap();
    std::fs::create_dir_all(mcps.join("rimz-lsp")).unwrap();
    std::fs::write(mcps.join("rimz-watch/instructions.md"), " \n").unwrap();
    std::fs::write(mcps.join("rimz-lsp/watch.md"), "Misplaced.\n").unwrap();
    std::fs::write(mcps.join("rimz-lsp/lsp.md"), "\n").unwrap();
    let stderr = explain();
    let rejected = format!(
        "rimz: {} is not used, so the built-in text is served: it is empty\n",
        mcps.join("rimz-watch/instructions.md").display()
    );
    let misplaced = format!(
        "rimz: {} is ignored: RimZ has no wording file rimz-lsp/watch.md; it belongs at {}\n",
        mcps.join("rimz-lsp/watch.md").display(),
        mcps.join("rimz-watch/watch.md").display()
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

    let (failure, is_error) = server.answer(6, "shell", json!({"command": "  "}));
    assert!(is_error, "{failure}");
    assert_eq!(
        failure["error"],
        json!("invalid arguments: `command` is blank"),
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
    let (start, is_error) =
        server.answer(1, "watch", json!({"when": "pr=merged", "label": "via mcp"}));
    assert!(!is_error, "{start}");
    let (listed, is_error) = server.answer(2, "tasks", json!({}));
    assert!(!is_error, "{listed}");
    let rows = listed["tasks"].as_array().expect("the task rows");
    let [row] = rows.as_slice() else {
        panic!("one task: {listed}");
    };
    assert_eq!(row["about"], json!("via mcp"), "{listed}");
    let id = row["id"].as_str().expect("the wait's id");
    assert!(id.starts_with("watch:"), "{listed}");
    assert_eq!(
        start,
        json!({
            "id": id,
            "status": "running",
            "stop": format!("stop({{id: \"{id}\"}})"),
            "report": "a message to you when the condition reads true",
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

/// A `watch` label is one line of 1 to 60 characters, trimmed, as `rimz watch
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
        let (failure, is_error) =
            server.answer(id, "watch", json!({"when": "pr=merged", "label": label}));
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
        "watch",
        json!({"when": "pr=merged", "label": "  fix it  "}),
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
    let (start, is_error) = server.answer(1, "watch", json!({"when": "pr=merged"}));
    assert!(!is_error, "{start}");
    let id = start["id"].as_str().unwrap();
    let name = id.trim_start_matches("watch:");
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

/// `ask` arms a raised ask on its caller with the brief, an errored end drops it, a second call
/// replaces the first, and a clean end opens the last one with the final reply as its text; an
/// empty brief arms nothing.
#[test]
fn ask_arms_with_the_brief_and_a_clean_end_opens_it_with_the_final_reply() {
    use rimz::agents::state::RaisedAskState;
    use rimz::store::raised;

    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent(&env);
    let feed = |event: &str, extra: Value| {
        let mut payload = json!({"hook_event_name": event, "session_id": "provider-session"});
        payload
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let output = env.run_hook("claude", &payload.to_string());
        assert!(output.status.success(), "{output:?}");
    };
    let mut server = serve_as_agent(&env);
    server.initialize();
    let (refused, is_error) = server.answer(1, "ask", json!({"brief": "  "}));
    assert!(is_error, "{refused}");
    assert_eq!(
        refused["error"],
        json!("invalid arguments: `brief` is empty"),
        "{refused}"
    );
    assert!(raised::read(env.store().paths()).asks.is_empty());
    let (dropped, is_error) = server.answer(2, "ask", json!({"brief": "Ship now?"}));
    assert!(!is_error, "{dropped}");
    feed("StopFailure", json!({"error": "failed"}));
    assert!(
        raised::read(env.store().paths()).asks.is_empty(),
        "an errored end drops the armed ask"
    );
    let (first, is_error) = server.answer(3, "ask", json!({"brief": "Ship now?"}));
    assert!(!is_error, "{first}");
    let (second, is_error) =
        server.answer(4, "ask", json!({"brief": "Ship now or after the review?"}));
    assert!(!is_error, "{second}");
    assert_eq!(second["status"], json!("armed"), "{second}");
    assert_ne!(first["id"], second["id"]);
    let asks = raised::read(env.store().paths()).asks;
    assert_eq!(asks.len(), 1, "the second call replaces the first");
    assert_eq!(json!(asks[0].value.ask_id.as_str()), second["id"]);
    assert_eq!(asks[0].value.state, RaisedAskState::Armed);
    assert_eq!(asks[0].value.raised_by.as_deref(), Some("@planner#main"));
    feed(
        "Stop",
        json!({"last_assistant_message": "The review found two gaps. Ship now or after it?"}),
    );
    let snapshot = env.store().snapshot_cached().unwrap();
    let ask = snapshot.agents[0]
        .raised_ask
        .as_ref()
        .expect("a clean end opens the armed ask");
    assert_eq!(json!(ask.ask_id.as_str()), second["id"]);
    assert_eq!(ask.state, RaisedAskState::Open);
    assert_eq!(
        ask.text.as_deref(),
        Some("The review found two gaps. Ship now or after it?")
    );
    assert_eq!(
        ask.detail().as_deref(),
        Some("Ship now or after the review?")
    );
    let raised: Vec<_> = env
        .read_events()
        .into_iter()
        .filter(|event| event.method == "ask.raised")
        .collect();
    assert_eq!(raised.len(), 3, "{raised:?}");
}

/// Even when `rimz-ask` is served explicitly, a launched subagent calling `ask` is refused with
/// the same error as `rimz asks raise` and writes neither a raised ask nor an audit event.
#[test]
fn ask_refuses_a_launched_subagent() {
    use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
    use rimz::store::event::EventEnvelope;
    use rimz::store::raised;

    let env = Env::new();
    env.record(&env.project_root);
    let workspace = env.resolve_workspace(&env.project_root);
    let mut observation =
        AgentLifecycleObservation::new(Some("child-session".into()), LifecycleSignal::Registered);
    observation.agent_name = Some("helper".into());
    observation.launch.parent_agent_id = Some("parent-session".into());
    observation.launch.launch_depth = Some(1);
    observation.worktree_path = Some(env.project_root.display().to_string());
    env.store()
        .append_event(&EventEnvelope::agent_lifecycle(
            workspace.workspace_id,
            workspace.session_name,
            "claude",
            "SessionStart",
            &observation,
        ))
        .unwrap();

    let mut command = env.rimz();
    command
        .env("RIMZ_AGENT_KIND", "claude")
        .env("RIMZ_AGENT_ID", "child-session")
        .env("RIMZ_AGENT_NAME", "helper")
        .args(["mcp", "serve", "--only", "rimz-ask"]);
    let mut server = McpServer::spawn(command);
    server.initialize();
    let (refused, is_error) = server.answer(1, "ask", json!({"brief": "Ship now?"}));
    assert!(is_error, "{refused}");
    assert_eq!(
        refused["error"],
        json!(
            "a subagent cannot ask the user: make the question your result and end your turn, and \
             the agent that launched you answers it"
        ),
        "{refused}"
    );
    assert!(raised::read(env.store().paths()).asks.is_empty());
    assert!(
        !env.read_events()
            .iter()
            .any(|event| event.method == "ask.raised"),
        "a refused ask writes nothing"
    );
}

#[test]
fn self_compact_owes_a_continue_by_default_and_honors_no_continue() {
    use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
    use rimz::store::message::{MessageBody, SoftCompactStep};

    for no_continue in [false, true] {
        let env = Env::new();
        env.record(&env.project_root);
        env.install_agent_hooks("claude");
        crate::common::wait::register_calling_agent(&env);
        let workspace = env.resolve_workspace(&env.project_root);
        let mut obs = AgentLifecycleObservation::new(
            Some("provider-session".into()),
            LifecycleSignal::TurnStarted { turn_id: None },
        );
        obs.pane_id = Some("zellij:terminal_1".parse().unwrap());
        env.store()
            .append_event(&rimz::store::event::EventEnvelope::agent_lifecycle(
                workspace.workspace_id,
                &workspace.session_name,
                "claude",
                "UserPromptSubmit",
                &obs,
            ))
            .unwrap();
        let pane = rimz::pane::PaneRef {
            pane_id: rimz::ids::PaneId::from_parts(rimz::ids::MuxName::Zellij, "terminal_1"),
            session_name: workspace.session_name.clone(),
            view_id: Some("tab_1".to_owned()),
            view_kind: Some(rimz::ids::ViewKind::Tab),
            view_name: Some("project".to_owned()),
            title: None,
            is_floating: false,
            command: Some("claude".to_owned()),
            foreground_cmdline: None,
            spawn_command: None,
            cwd: Some(env.project_root.display().to_string()),
            pane_pid: None,
            pane_process_start: None,
            hosted_agent_kind: None,
            hosted_agent_process_start: None,
            resumed_session_id: None,
            elevated_agent: None,
            first_seen_at_ms: None,
        };
        let panes = env.write_pane_fixture(std::slice::from_ref(&pane));
        let mut command = crate::common::wait::agent_wait(&env);
        command
            .env("RIMZ_TEST_PANE_LIST", &panes)
            .args(["mcp", "serve"]);
        let mut server = McpServer::spawn(command);
        server.initialize();
        let args = if no_continue {
            json!({"no_continue": true})
        } else {
            json!({})
        };
        let (receipt, is_error) = server.answer(1, "self_compact", args);
        assert!(!is_error, "no_continue={no_continue}: {receipt}");
        assert_eq!(receipt["status"], json!("queued"), "{receipt}");
        assert_eq!(receipt["continues"], json!(!no_continue), "{receipt}");
        let messages = env.store().list_messages().unwrap();
        let expected_len = if no_continue { 1 } else { 2 };
        assert_eq!(
            messages.len(),
            expected_len,
            "no_continue={no_continue}: {messages:?}"
        );
        assert!(messages.iter().any(|m| m.body == MessageBody::Command));
        assert_eq!(
            messages
                .iter()
                .any(|m| m.soft_compact == Some(SoftCompactStep::Continue)),
            !no_continue
        );
    }
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
/// each mark it did not apply beside the load it concerns (never on stderr),
/// and the provider's own arguments.
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
            "['rimz:deferred', 'rimz-lsp:always', github, 'absent:always']",
        ),
        (
            "ccode",
            "claude",
            "['*', 'rimz:deferred', 'rimz-lsp:always', 'github:always']\ncode-mode: on",
        ),
        (
            "cflipcode",
            "claude",
            "['rimz:always', 'rimz-lsp:deferred']\ncode-mode: on",
        ),
        (
            "pmark",
            "pi",
            "['*', 'rimz:deferred', 'rimz-lsp:always', 'github:always']",
        ),
        ("poff", "pi", "[rimz-lsp, rimz-watch]\ncode-mode: off"),
        (
            "pgrant",
            "pi",
            "['*']\ncapabilities: [rimz-lsp]\ncode-mode: off",
        ),
        (
            "omark",
            "opencode",
            "['rimz:always', 'rimz-lsp:deferred', github]",
        ),
        (
            "ooff",
            "opencode",
            "['rimz-lsp:deferred', 'rimz-watch', 'github:always']\ncode-mode: off",
        ),
        (
            "cmute",
            "claude",
            "['*']\nskills: [rimz-subagents, rimz-lsp]\nsubagents: []",
        ),
    ] {
        let tools = match (kind, name) {
            ("pi" | "opencode", _) => "",
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
    crate::common::write_definition(
        &env,
        "teams",
        "crew",
        "leader: lead\nstages: [Plan]\nroles:\n  - {role: lead, agent: cmark, owns: [Plan]}\n  - \
         {role: code, agent: cflipcode}",
        "Complete the work.",
    );
    let config = env.rimz_home().join("config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let explain_with = |profile: &str, flag: Option<&str>| {
        let mut bins = Vec::new();
        for kind in ["claude", "codex", "pi", "opencode"] {
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
            .args(flag)
            .env("SHELL", shell)
            .env("PATH", path)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        assert!(!stderr.contains(" entry `"), "{stderr}");
        String::from_utf8(output.stdout).unwrap()
    };
    let explain = |profile: &str| explain_with(profile, Some("--mcps"));
    let claude_user = "the mcps entry `github:always` is not applied: Claude takes a load mode \
                       only for RimZ's server and its capabilities";

    // A role seat of a staged team is served the board tools, loaded up front beside its
    // `rimz:deferred`; the same profile alone is served neither.
    let seat = explain("crew.lead");
    assert_eq!(tool_load(&seat, "blackboard_flip"), "always", "{seat}");
    assert_eq!(tool_load(&seat, "blackboard_record"), "always", "{seat}");
    let out = explain("cmark");
    assert!(!out.contains("Plan"), "{out}");
    assert!(
        !out.contains("rimz-blackboard") && !out.contains("blackboard_flip"),
        "{out}"
    );
    assert!(
        out.contains(
            "serves rimz-lsp, rimz-shell, rimz-watch, rimz-subagents, rimz-agents, rimz-teams, rimz-workflow, rimz-loop, rimz-message, rimz-ask, rimz-control, rimz-notes"
        ),
        "{out}"
    );
    assert_eq!(tool_load(&out, "lsp"), "always", "{out}");
    assert_eq!(tool_load(&out, "watch"), "default", "{out}");
    assert_eq!(tool_load(&out, "guide"), "default", "{out}");
    assert!(
        out.contains(&format!(
            "every server the account and project configure; {claude_user}\n"
        )),
        "{out}"
    );
    assert!(!out.contains("\nNot applied\n"), "{out}");
    assert!(out.contains("--mcp-config"), "{out}");
    assert!(out.contains(r#""--always","rimz-lsp""#), "{out}");
    assert!(out.contains("claude mcp list"), "{out}");
    assert!(out.contains("/context"), "{out}");
    // A solo profile is served every group a list may name, which leaves out the board.
    let listed = rimz::mcp::catalog::serve_args(&Capability::LISTED).split_off(2);
    let listed: Vec<&str> = listed.iter().map(String::as_str).collect();
    let mut server = serve(&env, &listed);
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
        "rimz-watch",
        "rimz-subagents",
        "rimz-agents",
        "rimz-teams",
        "rimz-workflow",
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
    assert_eq!(tool_load(&out, "watch"), "always", "{out}");
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
        out.contains(
            "serves rimz-lsp, rimz-shell, rimz-watch, rimz-subagents, rimz-agents, rimz-teams, rimz-workflow, rimz-loop, rimz-message, rimz-ask, rimz-control, rimz-notes"
        ),
        "{out}"
    );
    assert_eq!(
        tool_load(&out, "lsp"),
        "deferred; the mcps entry `rimz-lsp:always` is not applied: Codex takes a load mode \
         per server, so rimz-lsp loads as the rimz server does",
        "{out}"
    );
    assert_eq!(tool_load(&out, "notes"), "deferred", "{out}");
    assert!(
        out.contains(
            "switched off: notion; the mcps entry `absent:always` is not applied: the account's \
             Codex config defines no MCP server `absent`\n"
        ),
        "{out}"
    );
    assert!(out.contains("mcp_servers.rimz.omit_tools_from=[]"), "{out}");
    assert!(out.contains("CODEX_ROLLOUT_TRACE_ROOT"), "{out}");

    // Claude's code mode offers the model only REPL and Poll, so no mark of RimZ's applies,
    // while the argv still carries them.
    let out = explain("ccode");
    let coded = "is not applied: Claude's code mode offers the model only REPL and Poll, so no \
                 MCP tool loads up front";
    assert_eq!(
        tool_load(&out, "lsp"),
        format!("always; the mcps entry `rimz-lsp:always` {coded}"),
        "{out}"
    );
    assert_eq!(tool_load(&out, "shell"), "default", "{out}");
    assert!(
        out.contains(&format!(
            "rimz-control, rimz-notes; the mcps entry `rimz:deferred` {coded}\n"
        )),
        "{out}"
    );
    assert!(
        out.contains(&format!(
            "every server the account and project configure; {claude_user}\n"
        )),
        "{out}"
    );
    assert!(out.contains(r#""--always","rimz-lsp""#), "{out}");
    // Nor does a staged seat's up-front board load, or a mark on `rimz`, which shows beside each
    // load it sets.
    let out = explain("crew.code");
    for tool in ["blackboard_flip", "blackboard_record"] {
        assert_eq!(
            tool_load(&out, tool),
            format!(
                "always; the `always` load a staged team's role seat gives `rimz-blackboard` \
                 {coded}"
            ),
            "{out}"
        );
    }
    for tool in ["shell", "join", "guide"] {
        assert_eq!(
            tool_load(&out, tool),
            format!("always; the mcps entry `rimz:always` {coded}"),
            "{out}"
        );
    }
    assert_eq!(
        tool_load(&out, "lsp"),
        format!("deferred; the mcps entry `rimz-lsp:deferred` {coded}"),
        "{out}"
    );

    // Pi's installed extension registers the server from the launch env, each tool at the
    // exposure Pi gives it; Pi cannot limit or mark the user's own servers.
    let out = explain("pmark");
    assert!(
        out.contains(
            "serves rimz-lsp, rimz-shell, rimz-watch, rimz-subagents, rimz-agents, rimz-teams, rimz-workflow, rimz-loop, rimz-message, rimz-ask, rimz-control, rimz-notes\n"
        ),
        "{out}"
    );
    assert_eq!(tool_load(&out, "lsp"), "direct", "{out}");
    assert_eq!(tool_load(&out, "watch"), "deferred", "{out}");
    assert_eq!(tool_load(&out, "guide"), "deferred", "{out}");
    assert!(
        out.contains("\n  /mcp in the Pi session: the rimz server and each tool's exposure\n"),
        "{out}"
    );
    assert!(
        out.contains(
            "every server the account and project configure; the mcps entry `github:always` is \
             not applied: Pi takes a load mode only for RimZ's server and its capabilities\n"
        ),
        "{out}"
    );
    assert!(
        out.contains("\n  RIMZ_MCP_SERVE=rimz-lsp:always,rimz-shell:deferred,"),
        "{out}"
    );
    assert!(out.contains("\n  RIMZ_PI_MCP_EXPOSURE={"), "{out}");
    assert!(out.contains("\n## lsp\n"), "{out}");
    let out = explain("poff");
    assert!(out.contains("serves rimz-lsp, rimz-watch\n"), "{out}");
    assert_eq!(tool_load(&out, "lsp"), "deferred", "{out}");
    assert_eq!(tool_load(&out, "tasks"), "deferred", "{out}");
    assert!(
        out.contains(r#"RIMZ_PI_MCP_EXPOSURE={"exposure":"deferred"}"#),
        "{out}"
    );
    // A capability limit that grants no Pi tool still reaches the RimZ tools it grants.
    let out = explain("pgrant");
    assert!(out.contains("serves rimz-lsp\n"), "{out}");
    let report: Value = serde_json::from_str(&explain_with("pgrant", Some("--json"))).unwrap();
    let argv: Vec<&str> = report["provider_argv"]
        .as_array()
        .unwrap_or_else(|| panic!("{report}"))
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(!argv.contains(&"--no-tools"), "{argv:?}");
    assert!(
        argv.windows(2).any(|pair| pair
            == [
                "--tools",
                "mcp__*,list_mcp_resources,list_mcp_resource_templates,read_mcp_resource,tool_search"
            ]),
        "{argv:?}"
    );

    // An installed extension that differs from the shipped one serves nothing, and the row says
    // why and how to fix it.
    let installed = env.home_root.join(".pi/agent/extensions/rimz.ts");
    std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
    std::fs::write(&installed, "// RimZ Pi extension — _rimz_managed\nold\n").unwrap();
    let out = explain("pmark");
    assert!(
        out.contains(&format!(
            "not handed over: the installed Pi extension {} differs from the one this rimz \
             ships, so this launch reaches RimZ through its skills: run `rimz hooks install pi`",
            installed.display()
        )),
        "{out}"
    );
    std::fs::remove_file(&installed).unwrap();

    // OpenCode carries the server in its composed inline config: `rimz:always` makes the record
    // native, a capability's own `:deferred` puts its tools back behind `execute` through the
    // plugin, and the list keeps the user's servers through policies.
    let out = explain("omark");
    assert!(
        out.contains(
            "serves rimz-lsp, rimz-shell, rimz-watch, rimz-subagents, rimz-agents, rimz-teams, rimz-workflow, rimz-loop, rimz-message, rimz-ask, rimz-control, rimz-notes\n"
        ),
        "{out}"
    );
    assert_eq!(tool_load(&out, "lsp"), "code mode", "{out}");
    assert_eq!(tool_load(&out, "watch"), "native", "{out}");
    assert_eq!(tool_load(&out, "guide"), "native", "{out}");
    assert!(out.contains("only github (policies)\n"), "{out}");
    assert!(
        out.contains("\nInline config (OPENCODE_CONFIG_CONTENT)\n"),
        "{out}"
    );
    assert!(out.contains(r#""codemode":false"#), "{out}");
    assert!(
        out.contains(r#"RIMZ_LAUNCH_MCP_TOOLS={"lsp":"code"}"#),
        "{out}"
    );
    assert!(!out.contains("Hidden skills"), "{out}");
    // The Served text gives each tool the load the Tools table shows.
    let served = |tool: &str, capability: &str, load: &str| {
        out.lines()
            .skip_while(|line| *line != format!("## {tool}"))
            .nth(1)
            .is_some_and(|line| line.ends_with(&format!(" · {capability} · load {load}")))
    };
    assert!(served("lsp", "rimz-lsp", "code mode"), "{out}");
    assert!(served("watch", "rimz-watch", "native"), "{out}");
    assert!(served("guide", "-", "native"), "{out}");
    // An inherited inline value prints none of its own members, and its policies decide which of
    // the user's servers stay, in either spelling.
    let inherited = serde_json::json!({
        "mcp": {"servers": {"docs": {"type": "remote", "url": "https://docs.example", "headers": {"Authorization": "skip-secret"}}}},
        "policies": [
            {"action": "mcp.use", "resource": "github", "effect": "deny"},
        ],
    });
    let shell = crate::common::write_fake_login_shell(&env, "mcps-explain-sh", &[]);
    let output = env
        .rimz()
        .args(["agents", "explain", "omark", "--mcps"])
        .env("SHELL", shell)
        .env("OPENCODE_CONFIG_CONTENT", inherited.to_string())
        .output()
        .unwrap();
    let out = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success(), "{out}");
    assert!(
        out.contains(
            "  your servers: only none (policies)
"
        ),
        "{out}"
    );
    assert!(
        !out.contains("skip-secret") && !out.contains("mcp.use"),
        "{out}"
    );
    let out = explain("ooff");
    assert_eq!(
        tool_load(&out, "lsp"),
        "native; the mcps entry `rimz-lsp:deferred` is not applied: code-mode off sends every \
         RimZ tool native, and OpenCode has no tool search outside `execute`",
        "{out}"
    );
    assert_eq!(tool_load(&out, "watch"), "native", "{out}");
    assert!(
        out.contains(
            "only github (policies); the mcps entry `github:always` is not applied: OpenCode \
             takes a load mode only for RimZ's server and its capabilities\n"
        ),
        "{out}"
    );

    // A profile that allows no subagents is served no subagent tool and offered no skill, and
    // keeps the agent and team tools, which start no child of it.
    let out = explain("cmute");
    assert!(
        out.contains(
            "serves rimz-lsp, rimz-shell, rimz-watch, rimz-agents, rimz-teams, rimz-workflow, rimz-loop, rimz-message, rimz-ask, rimz-control, rimz-notes\n"
        ),
        "{out}"
    );
    assert!(!out.contains("rimz-subagents"), "{out}");
    let out = explain_with("cmute", None);
    let callable = out
        .lines()
        .find(|line| line.starts_with("  callable: "))
        .unwrap_or_else(|| panic!("{out}"));
    assert!(!callable.contains("rimz-subagents"), "{out}");

    std::fs::write(&config, "[agents]\nrimz-mcp-server = false\n").unwrap();
    let out = explain("cmark");
    let off = "is not applied: RimZ's MCP server is off for this launch";
    assert!(
        out.contains(&format!(
            "off: [agents] rimz-mcp-server = false; the mcps entry `rimz:deferred` {off}; the \
             mcps entry `rimz-lsp:always` {off}\n"
        )),
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        assert!(!stderr.contains(" entry `"), "{stderr}");
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
        out.contains("only github, notion (inheritMcp: false); the mcps entry `github:always` is not applied: Antigravity takes a load mode only for RimZ's capabilities\n"),
        "{out}"
    );
    assert_eq!(tool_load(&out, "lsp"), "always", "{out}");
    assert!(!out.contains("\nNot applied\n"), "{out}");
    assert!(out.contains("\n  inheritMcp: false\n"), "{out}");
    assert!(
        out.contains(r#"{"name": "github"}, {"name": "notion"}]"#),
        "{out}"
    );

    let out = explain(&env, "gopen", &[]);
    assert!(
        out.contains(
            "serves rimz-lsp, rimz-shell, rimz-watch, rimz-subagents, rimz-agents, rimz-teams, rimz-workflow, rimz-loop, rimz-message, rimz-ask, rimz-control, rimz-notes as `rimz_tools`\n"
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
            "serves rimz-lsp, rimz-shell, rimz-watch, rimz-subagents, rimz-agents, rimz-teams, rimz-workflow, rimz-loop, rimz-message, rimz-ask, rimz-control, rimz-notes as `rimz_tools`\n"
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
    assert!(!out.contains("is not applied"), "{out}");
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
    crate::common::write_definition(
        &env,
        "agents",
        "codex",
        "description: Codex base\ntools: [Read]",
        "Codex base.",
    );
    env.write_config(
        &env.project_root,
        "[profiles.xlast]\ndescription = 'xlast'\nagent = 'codex'\nmcps = ['*', 'rimz:always']\n\
         args = \"-c 'mcp_servers.rimz.omit_tools_from=[]'\"\n\
         [profiles.xconf]\ndescription = 'xconf'\nagent = 'codex'\nmcps = ['*']\n\
         args = \"'--config=mcp_servers.rimz.omit_tools_from=[\\\"deferred\\\"]'\"\n\
         [profiles.xtable]\ndescription = 'xtable'\nagent = 'codex'\nmcps = ['*']\n\
         args = \"-c 'mcp_servers={notion={enabled=false},github={command=\\\"gh\\\"}}'\"\n\
         [profiles.xoff]\ndescription = 'xoff'\nagent = 'codex'\nmcps = ['*']\n\
         args = \"-c mcp_servers.rimz.enabled=false\"\n\
         [profiles.xtableoff]\ndescription = 'xtableoff'\nagent = 'codex'\nmcps = ['*']\n\
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
    let (start, is_error) =
        server.answer(1, "watch", json!({"when": "pr=merged", "label": "cleared"}));
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
    let mcps = env.rimz_home().join("mcps/rimz-watch");
    std::fs::create_dir_all(&mcps).unwrap();
    std::fs::write(
        mcps.join("watch.md"),
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
        .find(|tool| tool["name"] == "watch")
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
    let (text, is_error) = server.call(1, "shell", json!({"command": "sleep 1; printf fired"}));
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
        "watch",
        json!({"when": "pr=merged", "label": "nightly check"}),
    );
    assert!(!is_error, "{start}");
    let id = start["id"].as_str().unwrap().to_owned();
    let (listed, is_error) = server.answer(2, "tasks", json!({"kind": "watch"}));
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
            &json!("watch"),
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
            "hint": "an id is a kind (subagent, agent, team, workflow, schedule, shell, or \
                     watch), a colon, and a name, as `tasks` lists it; end a task or subagent your \
                     provider started with your provider's own tool",
        })
    );
    let (failure, is_error) = server.answer(11, "stop", json!({"id": "watch:../x"}));
    assert!(is_error, "{failure}");
    assert_eq!(
        failure,
        json!({
            "error": "invalid watch name `../x`: schedule name `../x` must be non-empty and use \
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

/// `join` on a shell claims its outcome: it blocks until the command exits and answers the exit,
/// the output file and its size, and no message brings the outcome a second time.
#[test]
fn join_on_a_shell_answers_its_outcome_and_no_message_comes() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    crate::common::wait::register_calling_agent(&env);
    let mut server = serve_as_agent(&env);
    server.initialize();
    let release = env.home_root.join("release-join");
    let command = format!(
        "while [ ! -e {} ]; do sleep 0.05; done; printf joined-marker; exit 3",
        release.display()
    );
    let (start, is_error) = server.answer(1, "shell", json!({"command": command}));
    assert!(!is_error, "{start}");
    let id = start["id"].as_str().unwrap().to_owned();
    let name = id.strip_prefix("shell:").unwrap().to_owned();
    server.start_call(2, "join", json!({"id": id, "timeout": "60s"}));
    let deadline = Instant::now() + REPLY_DEADLINE;
    while crate::common::wait::wait_instances(&env).0[&name]
        .wait_meta
        .as_ref()
        .and_then(|meta| meta.joined_until)
        .is_none()
    {
        assert!(
            Instant::now() < deadline,
            "the join never claimed the shell"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    std::fs::write(&release, "").unwrap();
    let (joined, is_error) = tool_answer("join", &server.reply(&json!(2)));
    assert!(!is_error, "{joined}");
    let output = joined["result"].as_str().expect("the output file");
    assert_eq!(std::fs::read_to_string(output).unwrap(), "joined-marker");
    let detail = joined["detail"].as_str().unwrap();
    assert!(
        detail.starts_with("exit 3 after ") && detail.ends_with(" · <1k tokens, 1 line"),
        "{joined}"
    );
    assert_eq!(
        joined,
        json!({"id": id, "status": "done", "result": output, "detail": detail}),
        "{joined}"
    );
    assert!(
        !crate::common::wait::wait_instances(&env)
            .0
            .contains_key(&name)
    );
    let (mut messages, history) = env.store().list_messages_and_history().unwrap();
    messages.extend(history);
    assert!(messages.is_empty(), "the outcome came twice: {messages:?}");
    let (again, _) = server.answer(3, "join", json!({"id": id}));
    assert_eq!(
        again, joined,
        "a shell that has exited answers its outcome again"
    );
    let unknown = "shell:shell-never-armed";
    let (never, _) = server.answer(4, "join", json!({"id": unknown}));
    assert_eq!(
        never,
        json!({"id": unknown, "status": "ended", "hint": GONE})
    );
}

/// Every process `pids` names (as a command wrote them, space separated) is gone or a zombie
/// within the reply deadline.
#[cfg(target_os = "linux")]
fn assert_processes_gone(pids: &std::path::Path) {
    let deadline = Instant::now() + REPLY_DEADLINE;
    for pid in std::fs::read_to_string(pids).unwrap().split_whitespace() {
        let stat = format!("/proc/{pid}/stat");
        let running = || {
            std::fs::read_to_string(&stat).is_ok_and(|stat| {
                !stat
                    .rsplit_once(") ")
                    .is_some_and(|(_, rest)| rest.starts_with('Z'))
            })
        };
        while running() {
            assert!(
                Instant::now() < deadline,
                "process {pid} outlived its shell"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

/// Wait until wait row `name` is gone from the room's instances.
fn wait_for_row_gone(env: &Env, name: &str) {
    let deadline = Instant::now() + REPLY_DEADLINE;
    while crate::common::wait::wait_instances(env)
        .0
        .contains_key(name)
    {
        assert!(Instant::now() < deadline, "wait {name} kept its row");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Every message the room holds, pending or settled.
fn all_messages(env: &Env) -> Vec<rimz::store::message::MessageRecord> {
    let (mut messages, history) = env.store().list_messages_and_history().unwrap();
    messages.extend(history);
    messages
}

/// A shell given `timeout` is killed once it passes, with every process of its command, its
/// background children included: its wake reads timed out, unless `on: success` holds the wake
/// back, and `join` answers that verdict afterward.
#[cfg(target_os = "linux")]
#[test]
fn a_shell_past_its_timeout_is_killed_whole_and_reads_timed_out() {
    for on in [None, Some("success")] {
        let env = Env::new();
        env.record(&env.project_root);
        env.install_agent_hooks("claude");
        crate::common::wait::register_calling_agent(&env);
        let mut server = serve_as_agent(&env);
        server.initialize();
        let pids = env.home_root.join("timed-out.pids");
        let command = format!(
            "sleep 300 & printf '%s %s' $$ $! > {}; wait",
            pids.display()
        );
        let mut arguments = json!({"command": command, "timeout": "1s"});
        if let Some(on) = on {
            arguments["on"] = json!(on);
        }
        let (start, is_error) = server.answer(1, "shell", arguments);
        assert!(!is_error, "{start}");
        let id = start["id"].as_str().unwrap().to_owned();
        let name = id.strip_prefix("shell:").unwrap().to_owned();
        wait_for_row_gone(&env, &name);
        assert_processes_gone(&pids);
        // The fire consumes the row before it appends the history `join` answers from.
        let deadline = Instant::now() + REPLY_DEADLINE;
        let joined = loop {
            let (joined, is_error) = server.answer(2, "join", json!({"id": id}));
            assert!(!is_error, "{joined}");
            if joined["status"] != json!("ended") || Instant::now() >= deadline {
                break joined;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(joined["status"], json!("done"), "{joined}");
        let messages = all_messages(&env);
        let detail = joined["detail"].as_str().unwrap();
        assert!(detail.starts_with("timed out after "), "{joined}");
        if on.is_some() {
            assert!(messages.is_empty(), "on: success sent a wake: {messages:?}");
            continue;
        }
        let [wake] = messages.as_slice() else {
            panic!("one wake: {messages:?}");
        };
        let verdict = wake.text.lines().nth(1).unwrap_or_default();
        assert!(
            verdict.starts_with("timed out after ") && verdict.ends_with(&format!(" [{name}]")),
            "{}",
            wake.text
        );
    }
}

/// A shell with a timeout still sends its one still-running notice, and `stop` ends its command
/// whole with no wake of its outcome.
#[cfg(target_os = "linux")]
#[test]
fn check_in_and_stop_work_on_a_shell_with_a_timeout() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    crate::common::wait::register_calling_agent(&env);
    let mut server = serve_as_agent(&env);
    server.initialize();
    let pids = env.home_root.join("stopped.pids");
    let command = format!(
        "sleep 300 & printf '%s %s' $$ $! > {}; wait",
        pids.display()
    );
    let (start, is_error) = server.answer(
        1,
        "shell",
        json!({"command": command, "check_in": "1s", "timeout": "1h"}),
    );
    assert!(!is_error, "{start}");
    let id = start["id"].as_str().unwrap().to_owned();
    let name = id.strip_prefix("shell:").unwrap().to_owned();
    let deadline = Instant::now() + REPLY_DEADLINE;
    while all_messages(&env).is_empty() {
        assert!(Instant::now() < deadline, "no check-in came");
        std::thread::sleep(Duration::from_millis(50));
    }
    let (stopped, is_error) = server.answer(2, "stop", json!({"id": id}));
    assert!(!is_error, "{stopped}");
    assert_eq!(stopped, json!({"id": id, "status": "stopped"}));
    wait_for_row_gone(&env, &name);
    assert_processes_gone(&pids);
    let messages = all_messages(&env);
    let [checkin] = messages.as_slice() else {
        panic!("only the check-in: {messages:?}");
    };
    assert!(
        checkin.text.contains("still running after "),
        "{}",
        checkin.text
    );
}

/// A shell given `input` runs a command that reads `{ "input": … }` from
/// `$RIMZ_WORKFLOW_DIR/context.json`, in a directory beside its output file; a shell without
/// one sees `RIMZ_WORKFLOW_DIR` as the server's environment has it.
#[test]
fn a_shells_input_reaches_its_command_through_the_workflow_dir() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    crate::common::wait::register_calling_agent(&env);
    let mut command = crate::common::wait::agent_wait(&env);
    command
        .args(["mcp", "serve"])
        .env("RIMZ_WORKFLOW_DIR", "/outer-step");
    let mut server = McpServer::spawn(command);
    server.initialize();
    let input = json!({"brief": "--force", "items": [1, "two"]});
    let mut outputs = Vec::new();
    for (id, arguments) in [
        (
            1,
            json!({"command": "cat \"$RIMZ_WORKFLOW_DIR/context.json\"", "input": input}),
        ),
        (3, json!({"command": "printf %s \"$RIMZ_WORKFLOW_DIR\""})),
    ] {
        let (start, is_error) = server.answer(id, "shell", arguments);
        assert!(!is_error, "{start}");
        let (joined, is_error) =
            server.answer(id + 1, "join", json!({"id": start["id"], "timeout": "60s"}));
        assert!(!is_error, "{joined}");
        let output = std::path::PathBuf::from(joined["result"].as_str().expect("the output"));
        outputs.push((std::fs::read_to_string(&output).unwrap(), output));
    }
    let (context, output) = &outputs[0];
    let context: Value = serde_json::from_str(context).expect("context.json");
    assert_eq!(context, json!({"input": input}));
    assert!(output.with_extension("").join("context.json").is_file());
    assert_eq!(outputs[1].0, "/outer-step");
}

/// A watch given `timeout` that still reads unmet when it passes wakes its agent once, naming
/// the timeout, and its row is gone.
#[test]
fn a_watch_past_its_timeout_wakes_once_naming_it() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    crate::common::wait::register_calling_agent(&env);
    crate::common::wait::publish_claude_5h(&env, 90);
    let mut server = serve_as_agent(&env);
    server.initialize();
    let (start, is_error) = server.answer(
        1,
        "watch",
        json!({"when": "window.5h.left >= 40", "timeout": "1s"}),
    );
    assert!(!is_error, "{start}");
    let name = start["id"]
        .as_str()
        .unwrap()
        .strip_prefix("watch:")
        .unwrap()
        .to_owned();
    let tick = || {
        let output = crate::common::wait::agent_wait(&env)
            .args(["loop", "tick"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    tick();
    std::thread::sleep(Duration::from_millis(1100));
    tick();
    wait_for_row_gone(&env, &name);
    tick();
    let messages = all_messages(&env);
    let [wake] = messages.as_slice() else {
        panic!("one wake: {messages:?}");
    };
    let lines: Vec<_> = wake.text.lines().collect();
    assert_eq!(lines[0], "waited on window.5h.left>=40", "{}", wake.text);
    assert!(
        lines[1].starts_with("timed out after ") && lines[1].ends_with(&format!(" [{name}]")),
        "{}",
        wake.text
    );
}

/// A shell canceled while a join holds its claim ends that join at once as `ended`, saying it
/// was canceled, and `rimz loop logs` shows the cancel.
#[test]
fn a_join_on_a_shell_canceled_under_it_ends_at_once() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    crate::common::wait::register_calling_agent(&env);
    let mut server = serve_as_agent(&env);
    server.initialize();
    let (start, is_error) = server.answer(1, "shell", json!({"command": "sleep 300"}));
    assert!(!is_error, "{start}");
    let id = start["id"].as_str().unwrap().to_owned();
    let name = id.strip_prefix("shell:").unwrap().to_owned();
    server.start_call(2, "join", json!({"id": id, "timeout": "50s"}));
    let deadline = Instant::now() + REPLY_DEADLINE;
    while crate::common::wait::wait_instances(&env).0[&name]
        .wait_meta
        .as_ref()
        .and_then(|meta| meta.joined_until)
        .is_none()
    {
        assert!(
            Instant::now() < deadline,
            "the join never claimed the shell"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let canceled_at = Instant::now();
    let (stopped, is_error) = server.answer(3, "stop", json!({"id": id}));
    assert!(!is_error, "{stopped}");
    let (joined, is_error) = tool_answer("join", &server.reply(&json!(2)));
    assert!(!is_error, "{joined}");
    assert_eq!(
        joined,
        json!({"id": id, "status": "ended", "hint": "it was canceled before its command exited"}),
    );
    assert!(
        canceled_at.elapsed() < Duration::from_secs(30),
        "the join waited out its deadline: {:?}",
        canceled_at.elapsed()
    );
    let logs = crate::common::wait::wait_ok(&env, &["loop", "logs", &name]);
    assert!(logs.contains("canceled"), "{logs}");
}

/// A join by an owner the shell's outcome does not go to (here the parent of the child that
/// armed it) claims nothing: it answers the outcome from history and the child gets its message.
#[test]
fn a_join_on_a_childs_shell_answers_and_leaves_the_childs_message() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    crate::common::wait::register_calling_agent(&env);
    crate::common::wait::register_agent(
        &env,
        "kid-session",
        "launch-kid",
        "kid",
        child_of_caller(),
    );
    let mut server = serve_as_agent(&env);
    server.initialize();
    let release = env.home_root.join("release-kid");
    let command = format!(
        "while [ ! -e {} ]; do sleep 0.05; done; exit 3",
        release.display()
    );
    let (start, is_error) = server.answer(1, "shell", json!({"command": command}));
    assert!(!is_error, "{start}");
    let id = start["id"].as_str().unwrap().to_owned();
    let name = id.strip_prefix("shell:").unwrap().to_owned();
    // The shell reads as the child's: its outcome goes to the child.
    crate::common::wait::rewrite_loop_instances(&env, |rows: &mut Value| {
        rows[&name]["wait"] = json!({"kind": "claude", "session": "launch-kid", "handle": "@kid"});
    });
    server.start_call(2, "join", json!({"id": id, "timeout": "60s"}));
    std::thread::sleep(Duration::from_millis(500));
    let claim = crate::common::wait::wait_instances(&env).0[&name]
        .wait_meta
        .as_ref()
        .and_then(|meta| meta.joined_until);
    std::fs::write(&release, "").unwrap();
    let (joined, is_error) = tool_answer("join", &server.reply(&json!(2)));
    assert!(!is_error, "{joined}");
    assert_eq!(claim, None, "the parent's join claimed the child's shell");
    let detail = joined["detail"].as_str().unwrap_or_default();
    assert!(
        joined["status"] == "done" && joined.get("hint").is_none() && detail.starts_with("exit 3"),
        "{joined}"
    );
    let deadline = Instant::now() + REPLY_DEADLINE;
    let messages = loop {
        let (mut messages, history) = env.store().list_messages_and_history().unwrap();
        messages.extend(history);
        if !messages.is_empty() || Instant::now() >= deadline {
            break messages;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let recipients: Vec<_> = messages
        .iter()
        .map(|message| message.agent_id.as_str())
        .collect();
    assert_eq!(recipients, ["kid-session"], "{messages:?}");
}

/// A launched child whose run is terminal reads as `done` in `tasks` while its pane lingers,
/// and a child whose run is still active reads as `running` (R7).
#[test]
fn tasks_reads_a_lingering_terminal_subagent_as_done() {
    use rimz::agents::LaunchParams;
    use rimz::ids::{AgentKind, AgentSessionId};
    use rimz::store::run::{RunRecord, RunStatus};

    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent(&env);
    let claude = AgentKind::new_unchecked("claude");
    let child_params = LaunchParams {
        parent_agent_id: Some(AgentSessionId::from("launch-session")),
        parent_agent_kind: Some(claude.clone()),
        launch_depth: Some(1),
        ..LaunchParams::default()
    };
    crate::common::wait::register_agent(
        &env,
        "lingering-session",
        "launch-lingering",
        "lingering-kid",
        child_params.clone(),
    );
    crate::common::wait::register_agent(
        &env,
        "active-session",
        "launch-active",
        "active-kid",
        child_params,
    );
    let workspace = env.resolve_workspace(&env.project_root);
    let paths = env.store().paths().clone();
    let mut done_run = RunRecord::new(
        workspace.workspace_id.clone(),
        claude.clone(),
        rimz::agents::PermissionMode::Auto,
        "done work".into(),
        env.project_root.clone(),
    );
    done_run.subagent = true;
    done_run.launch_id = Some(AgentSessionId::from("launch-lingering"));
    done_run.agent_id = Some(AgentSessionId::from("lingering-session"));
    done_run.agent_name = Some("lingering-kid".into());
    done_run.status = RunStatus::Completed;
    done_run.joined_at = Some(jiff::Timestamp::now());
    rimz::harness::run::create(&paths, &done_run).unwrap();

    let mut active_run = RunRecord::new(
        workspace.workspace_id,
        claude,
        rimz::agents::PermissionMode::Auto,
        "active work".into(),
        env.project_root.clone(),
    );
    active_run.subagent = true;
    active_run.launch_id = Some(AgentSessionId::from("launch-active"));
    active_run.agent_id = Some(AgentSessionId::from("active-session"));
    active_run.agent_name = Some("active-kid".into());
    active_run.status = RunStatus::Running;
    rimz::harness::run::create(&paths, &active_run).unwrap();

    let mut server = serve_as_agent(&env);
    server.initialize();
    let (listed, is_error) = server.answer(1, "tasks", json!({"kind": "subagent"}));
    assert!(!is_error, "{listed}");
    let rows = listed["tasks"].as_array().unwrap();
    let status_of = |id: &str| {
        rows.iter()
            .find(|row| row["id"].as_str() == Some(id))
            .and_then(|row| row["status"].as_str())
            .unwrap_or_else(|| panic!("missing {id} in {listed}"))
    };
    assert_eq!(status_of("subagent:lingering-kid"), "done", "{listed}");
    assert_eq!(status_of("subagent:active-kid"), "running", "{listed}");
}

/// A wait the caller armed for itself is its to stop; a subscription that wakes the caller but
/// that its team armed is not, and stays booked.
#[test]
fn stop_refuses_a_subscription_another_armed_for_the_caller() {
    use crate::common::wait::{rewrite_loop_instances, seed_signal_wait, wait_instances};

    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent(&env);
    seed_signal_wait(&env, "own-ping", "own-ping", false);
    seed_signal_wait(&env, "team-ping", "team-ping", false);
    rewrite_loop_instances(&env, |tasks: &mut rimz::config::Tasks| {
        tasks.0.get_mut("team-ping").unwrap().team = Some("forge#main".parse().unwrap());
    });
    let mut server = serve_as_agent(&env);
    server.initialize();

    let (listed, is_error) = server.answer(3, "tasks", json!({"kind": "watch"}));
    assert!(!is_error, "{listed}");
    let ids: Vec<&Value> = listed["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| &row["id"])
        .collect();
    assert_eq!(ids, [&json!("watch:own-ping")], "{listed}");
    let (failure, is_error) = server.answer(1, "stop", json!({"id": "watch:team-ping"}));
    assert!(is_error, "{failure}");
    assert_eq!(
        failure,
        json!({
            "error": "watch:team-ping was started by another agent",
            "hint": "stop and join act on what you started, or what an agent you started \
                     started; `tasks` lists it",
        })
    );
    assert!(wait_instances(&env).0.contains_key("team-ping"));
    let (stopped, is_error) = server.answer(2, "stop", json!({"id": "watch:own-ping"}));
    assert!(!is_error, "{stopped}");
    assert_eq!(
        stopped,
        json!({"id": "watch:own-ping", "status": "stopped"})
    );
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

/// `message` parks a message for a live agent as `rimz message` does, its record marked for a
/// reply when asked; an address that names no agent fails with the live agents to pick from and
/// records the bounce the command records; and a message to the caller's ended child goes to its
/// resume, which refuses a child with no recorded conversation as the command refuses it.
#[test]
fn message_parks_for_a_live_agent_and_reaches_an_ended_childs_resume() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    crate::common::wait::register_calling_agent(&env);
    let mut otter = live_seat(&env, "otter", 7, rimz::agents::LaunchParams::default());
    let other_channel = rimz::agents::LaunchParams {
        channel: Some("rival-team".to_owned()),
        ..rimz::agents::LaunchParams::default()
    };
    let mut far_seat = live_seat(&env, "far-seat", 8, other_channel);
    let kid = rimz::agents::LaunchParams {
        isolation: Some(rimz::config::Isolation::Host),
        ..child_of_caller()
    };
    crate::common::wait::register_agent(&env, "kid-session", "launch-kid", "kid", kid);
    end_session(&env, "kid-session");
    let mut server = serve_live(&env, &[]);
    server.initialize();
    let (parked, is_error) = server.answer(
        1,
        "message",
        json!({"to": "@otter", "text": "--steer plan.md is ready", "reply_wanted": true}),
    );
    assert!(!is_error, "{parked}");
    let rows = parked["messages"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{parked}");
    assert_eq!(rows[0]["status"], json!("queued"), "{parked}");
    let pending = env.store().list_pending_messages().unwrap();
    let [record] = pending.as_slice() else {
        panic!("one queued record: {pending:?}");
    };
    assert_eq!(rows[0]["message_id"], json!(record.message_id.as_str()));
    assert!(record.reply_wanted, "{record:?}");
    let bounces = || {
        env.read_events()
            .into_iter()
            .filter(|event| event.method == "message.errored")
            .count()
    };
    let (unknown, is_error) =
        server.answer(2, "message", json!({"to": "@nobody-here", "text": "hi"}));
    assert!(is_error, "{unknown}");
    let error = unknown["error"].as_str().unwrap();
    assert!(
        error.starts_with("no agent matches target `@nobody-here`"),
        "{unknown}"
    );
    assert!(
        unknown["hint"]
            .as_str()
            .is_some_and(|hint| hint.starts_with("live agents: ")
                && hint.contains("@otter")
                && !hint.contains("far-seat")),
        "{unknown}"
    );
    assert_eq!(bounces(), 1);
    let (ended, is_error) = server.answer(3, "message", json!({"to": "@kid", "text": "follow up"}));
    assert!(is_error, "{ended}");
    let error = ended["error"].as_str().unwrap();
    assert!(
        error.starts_with("cannot resume @kid") && error.contains("no recorded conversation"),
        "{ended}"
    );
    assert_eq!(env.store().list_pending_messages().unwrap().len(), 1);
    for seat in [&mut otter, &mut far_seat] {
        seat.kill().ok();
        seat.wait().ok();
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

/// A definition whose one script holds the run live until it is canceled.
const HELD_WORKFLOW: &str = "[workflow]\nscope = 'worktree'\nstart = 'hold'\n\
    [script.hold]\nrun = 'sleep 300'\ntimeout = '1h'\non = { done = '@done' }\n";

fn deploy_workflow(env: &Env, name: &str, spec: &str) {
    let dir = env.rimz_home().join("workflows").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("workflow.toml"), spec).unwrap();
    std::fs::write(dir.join("prompt.md"), "Build it").unwrap();
}

fn live_run_ids(env: &Env) -> Vec<String> {
    rimz::store::workflow::live_runs(&env.state_path_for(&env.project_root))
        .unwrap()
        .into_iter()
        .map(|record| record.id.to_string())
        .collect()
}

/// A run the tool starts is its caller's from the tool's answer on: `tasks` lists it, `join`
/// reads it running, and the returned stop call cancels it. The record names the caller.
#[test]
fn a_workflow_the_tool_starts_is_listed_joined_and_stopped_by_its_caller() {
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent(&env);
    deploy_workflow(&env, "held", HELD_WORKFLOW);
    let mut server = serve_as_agent(&env);
    server.initialize();
    let (start, is_error) = server.answer(
        1,
        "workflow",
        json!({"name": "held", "prompt": "keep the API", "deadline": "8h"}),
    );
    assert!(!is_error, "{start}");
    let [run] = live_run_ids(&env).try_into().expect("one live run");
    let id = format!("workflow:{run}");
    assert_eq!(
        start,
        json!({
            "id": id,
            "status": "running",
            "stop": format!("stop({{id: \"{id}\"}})"),
            "report": "a WORKFLOW_REPORT message to you at your next turn boundary after the \
                       run ends",
        })
    );
    let (record, _) = run_record(&env, &run);
    assert_eq!(
        record["starter"],
        json!({"by": "agent", "kind": "claude", "launch": "launch-session"}),
        "{record:#}"
    );
    assert_eq!(record["loop_report"], json!("owed"), "{record:#}");
    assert_eq!(record["prompt"], json!("keep the API"), "{record:#}");
    assert!(record["deadline"].is_string(), "{record:#}");

    let (listed, is_error) = server.answer(2, "tasks", json!({"kind": "workflow"}));
    assert!(!is_error, "{listed}");
    assert_eq!(task_ids(&listed), [id.as_str()], "{listed}");
    let (joined, is_error) = server.answer(3, "join", json!({"id": id, "timeout": "1s"}));
    assert!(!is_error, "{joined}");
    assert_eq!(joined, json!({"id": id, "status": "running"}));
    let (stopped, is_error) = server.answer(4, "stop", json!({"id": id}));
    assert!(!is_error, "{stopped}");
    assert_eq!(stopped["status"], json!("stopped"), "{stopped}");
    assert_eq!(run_record(&env, &run).0["state"], json!("canceled"));
    server.close();
}

/// The run's input reaches its record whole, and its `key` becomes the run's.
#[test]
fn a_workflow_the_tool_starts_reads_its_input_back_from_the_run() {
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent(&env);
    deploy_workflow(&env, "held", HELD_WORKFLOW);
    let mut server = serve_as_agent(&env);
    server.initialize();
    let input = json!({"outcome": "green", "key": "pr-7", "detail": {"checks": 3}});
    let (start, is_error) = server.answer(
        1,
        "workflow",
        json!({"name": "held", "input": input.clone()}),
    );
    assert!(!is_error, "{start}");
    let [run] = live_run_ids(&env).try_into().expect("one live run");
    let (record, _) = run_record(&env, &run);
    assert_eq!(record["input"], input, "{record:#}");
    assert_eq!(record["key"], json!("pr-7"), "{record:#}");
    let (stopped, _) = server.answer(2, "stop", json!({"id": format!("workflow:{run}")}));
    assert_eq!(stopped["status"], json!("stopped"), "{stopped}");
    server.close();
}

/// `detach` reaches the run's record, and the answer promises no report.
#[test]
fn a_detached_workflow_answers_that_no_report_comes() {
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent(&env);
    deploy_workflow(&env, "held", HELD_WORKFLOW);
    let mut server = serve_as_agent(&env);
    server.initialize();
    let (start, is_error) = server.answer(1, "workflow", json!({"name": "held", "detach": true}));
    assert!(!is_error, "{start}");
    assert_eq!(
        start["report"],
        json!("no message when the run ends; `join` waits on it"),
        "{start}"
    );
    let [run] = live_run_ids(&env).try_into().expect("one live run");
    assert_eq!(run_record(&env, &run).0["loop_report"], json!("none"));
    let (stopped, _) = server.answer(2, "stop", json!({"id": format!("workflow:{run}")}));
    assert_eq!(stopped["status"], json!("stopped"), "{stopped}");
    server.close();
}

/// A validation warning the command prints to stderr reaches the caller in `warnings`.
#[test]
fn a_workflow_definition_warning_reaches_the_answer() {
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent(&env);
    deploy_workflow(
        &env,
        "fan",
        "[workflow]\nscope = 'worktree'\nstart = 'spread'\n\
         [runs.spread]\nstart = [{ workflow = 'ghost' }]\ntimeout = '1m'\nthen = '@done'\n",
    );
    let mut server = serve_as_agent(&env);
    server.initialize();
    let (start, is_error) = server.answer(1, "workflow", json!({"name": "fan"}));
    assert!(!is_error, "{start}");
    assert_eq!(
        start["warnings"],
        json!([
            "warning: runs.spread.start.0.workflow: ghost is not a deployed workflow; the step \
             fails when it starts it"
        ]),
        "{start}"
    );
    server.close();
}

/// Each refusal of `rimz workflow run` reaches the caller as the tool's error, and nothing
/// starts.
#[test]
fn the_workflow_tool_refuses_what_the_command_refuses() {
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent(&env);
    deploy_workflow(&env, "held", HELD_WORKFLOW);
    deploy_workflow(
        &env,
        "whole",
        "[workflow]\nscope = 'project'\nstart = 'hold'\n\
         [script.hold]\nrun = 'true'\ntimeout = '1m'\non = { done = '@done' }\n",
    );
    deploy_workflow(
        &env,
        "agentic",
        "[workflow]\nscope = 'worktree'\nstart = 'build'\n\
         [agent.build]\nagent = 'claude'\nprompt-file = 'prompt.md'\nthen = '@done'\n\
         lifecycle = 'turn'\n",
    );
    let mut server = serve_as_agent(&env);
    server.initialize();
    let home = env.rimz_home().join("workflows");
    let (failure, is_error) = server.answer(1, "workflow", json!({"name": "ghost"}));
    assert!(is_error, "{failure}");
    assert_eq!(
        failure["error"],
        json!(format!(
            "ghost: no workflow of that name in {} or an installed pack",
            home.display()
        ))
    );
    let (failure, is_error) = server.answer(2, "workflow", json!({"name": "./held"}));
    assert!(is_error, "{failure}");
    assert_eq!(
        failure["error"],
        json!("invalid arguments: `name` is a deployed workflow's name, not a path")
    );
    let (failure, is_error) = server.answer(
        3,
        "workflow",
        json!({"name": "held", "deadline": "whenever"}),
    );
    assert!(is_error, "{failure}");
    assert!(
        failure["error"]
            .as_str()
            .unwrap()
            .starts_with("invalid deadline `whenever`: "),
        "{failure}"
    );
    let (failure, is_error) =
        server.answer(4, "workflow", json!({"name": "whole", "worktree": "feat"}));
    assert!(is_error, "{failure}");
    let error = failure["error"].as_str().unwrap();
    assert!(
        error.starts_with("whole has scope = project, so it runs at ")
            && error.ends_with("; start it without `worktree`"),
        "{failure}"
    );
    let (failure, is_error) =
        server.answer(9, "workflow", json!({"name": "held", "worktree": "feat"}));
    assert!(is_error, "{failure}");
    assert_eq!(
        failure["error"],
        json!("`worktree` requires a git repository-backed room")
    );
    for (id, input, reason) in [
        (
            10,
            json!({"key": "pr-7"}),
            "expected an object with a string `outcome`",
        ),
        (
            11,
            json!({"outcome": 7}),
            "expected an object with a string `outcome`",
        ),
        (
            12,
            json!({"outcome": "go", "key": true}),
            "expected a `key` that is a string or an integer",
        ),
    ] {
        let (failure, is_error) =
            server.answer(id, "workflow", json!({"name": "held", "input": input}));
        assert!(is_error, "{failure}");
        assert_eq!(failure["error"], json!(format!("invalid input: {reason}")));
    }
    let (failure, is_error) = server.answer(13, "workflow", json!({"name": "held", "input": []}));
    assert!(is_error, "{failure}");
    assert!(
        failure["error"]
            .as_str()
            .unwrap()
            .starts_with("invalid arguments: "),
        "{failure}"
    );
    let (failure, is_error) = server.answer(5, "workflow", json!({"name": "agentic"}));
    assert!(is_error, "{failure}");
    let error = failure["error"].as_str().unwrap();
    assert!(
        error.starts_with("workflow start refused; nothing was created:\n- ")
            && error.contains("(blocks build)"),
        "{failure}"
    );
    assert_eq!(live_run_ids(&env), Vec::<String>::new());

    let (started, is_error) = server.answer(6, "workflow", json!({"name": "held"}));
    assert!(!is_error, "{started}");
    let [run] = live_run_ids(&env).try_into().expect("one live run");
    let (failure, is_error) = server.answer(7, "workflow", json!({"name": "held"}));
    assert!(is_error, "{failure}");
    assert!(
        failure["error"].as_str().unwrap().contains(&run),
        "the overlap names the live run: {failure}"
    );
    assert_eq!(live_run_ids(&env), vec![run.clone()]);
    let (stopped, _) = server.answer(8, "stop", json!({"id": format!("workflow:{run}")}));
    assert_eq!(stopped["status"], json!("stopped"), "{stopped}");
    server.close();
}

/// A caller RimZ cannot identify starts nothing, and neither does one inside an inline run,
/// whose start would record no starter.
#[test]
fn the_workflow_tool_starts_nothing_for_a_caller_it_could_not_own_it_for() {
    let env = Env::new();
    env.record(&env.project_root);
    deploy_workflow(&env, "held", HELD_WORKFLOW);
    let mut command = env.rimz();
    command.args(["mcp", "serve"]);
    let mut stranger = McpServer::spawn(command);
    stranger.initialize();
    let (failure, is_error) = stranger.answer(1, "workflow", json!({"name": "held"}));
    assert!(is_error, "{failure}");
    assert_eq!(
        failure["error"],
        json!("starting a workflow is only available to an agent RimZ can identify"),
        "{failure}"
    );
    assert_eq!(live_run_ids(&env), Vec::<String>::new());
    stranger.close();

    crate::common::wait::register_calling_agent(&env);
    let inline = seed_started_run(&env, "launch-session", json!({"origin": "inline"}));
    let mut command = crate::common::wait::agent_wait(&env);
    command
        .args(["mcp", "serve"])
        .env("RIMZ_WORKFLOW_RUN", &inline);
    let mut inside = McpServer::spawn(command);
    inside.initialize();
    let (failure, is_error) = inside.answer(1, "workflow", json!({"name": "held"}));
    assert!(is_error, "{failure}");
    assert_eq!(
        failure["error"],
        json!("a workflow started here would record no agent as its starter, so nothing started"),
        "{failure}"
    );
    assert_eq!(live_run_ids(&env), [inline]);
    inside.close();
}

/// A schedule the agent booked: `join` answers that no message comes when it fires, and
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
            "report": "no message comes when it fires; join does not wait for it",
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
    let checkout = checkout_with_fifo(&env, "follow-7c2d");
    let command = format!("cat {}", checkout.join("held.md").display());
    let (start, is_error) = server.answer(1, "shell", json!({"command": command}));
    assert!(!is_error, "{start}");
    let held = HeldFifo::when_read(&checkout);
    // A read that overlaps the call thread's exit can miss the child as it moves to the main
    // thread, so the read that found it is the one kept.
    let deadline = Instant::now() + Duration::from_secs(5);
    let watchers = loop {
        let found = children(server.pid());
        if !found.is_empty() || Instant::now() >= deadline {
            break found;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
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
    assert_eq!(
        children(server.pid()),
        watchers,
        "the watcher did not outlive the swap"
    );

    held.release("");
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

/// The `worker` profile, the `forge` team, and the deployed `held` workflow a schedule can name.
fn schedule_targets(env: &Env) {
    crate::common::write_definition(env, "agents", "claude", "description: Base", "Follow.");
    crate::common::write_definition(
        env,
        "agents",
        "worker",
        "description: Worker\nagent: claude\ntools: []",
        "",
    );
    crate::common::write_definition(
        env,
        "teams",
        "forge",
        "leader: coder\nstages: [Build]\nroles:\n  - {role: coder, agent: worker, owns: [Build]}",
        "Complete the work.",
    );
    deploy_workflow(env, "held", HELD_WORKFLOW);
}

fn schedule_room() -> Env {
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent(&env);
    schedule_targets(&env);
    env
}

fn loop_rows(env: &Env) -> toml::Table {
    let Ok(text) = std::fs::read_to_string(env.rimz_home().join("loop.toml")) else {
        return toml::Table::new();
    };
    let table = toml::from_str::<toml::Table>(&text).unwrap();
    table
        .get("tasks")
        .and_then(toml::Value::as_table)
        .cloned()
        .unwrap_or_default()
}

fn loop_instances(env: &Env) -> rimz::config::Tasks {
    let path = env
        .state_path_for(&env.project_root)
        .root
        .join("records/loop-instances.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return rimz::config::Tasks::default();
    };
    serde_json::from_str(&text).expect("loop instances")
}

/// A booking the tool makes is the caller's from the start: `tasks` lists it, and `stop` on the
/// id it answered removes it. The answer promises no message, since a fire sends the booker none.
#[test]
fn a_schedule_the_tool_books_is_listed_and_stopped_by_its_caller() {
    let env = schedule_room();
    let mut server = serve_as_agent(&env);
    server.initialize();
    let (booked, is_error) = server.answer(
        1,
        "schedule",
        json!({
            "name": "nightly",
            "trigger": "every",
            "value": "1h",
            "tool": "agent",
            "arguments": {"profile": "worker", "prompt": "check the build"},
            "overlap": "replace",
        }),
    );
    assert!(!is_error, "{booked}");
    assert_eq!(booked["id"], json!("schedule:nightly"), "{booked}");
    assert_eq!(booked["stop"], json!("stop({id: \"schedule:nightly\"})"));
    let report = booked["report"].as_str().unwrap();
    assert!(!report.contains("message to you"), "{report}");
    let (listed, _) = server.answer(2, "tasks", json!({"kind": "schedule"}));
    assert_eq!(task_ids(&listed), ["schedule:nightly"], "{listed}");
    let (stopped, is_error) = server.answer(3, "stop", json!({"id": "schedule:nightly"}));
    assert!(!is_error, "{stopped}");
    assert_eq!(stopped["status"], json!("stopped"), "{stopped}");
    assert!(!loop_rows(&env).contains_key("nightly"));
    let (listed, _) = server.answer(4, "tasks", json!({"kind": "schedule"}));
    assert_eq!(listed, json!({"tasks": []}));

    let (once, is_error) = server.answer(
        5,
        "schedule",
        json!({
            "name": "later",
            "trigger": "in",
            "value": "30m",
            "worktree": "later",
            "tool": "team",
            "arguments": {"team": "forge", "prompt": "ship it"},
        }),
    );
    assert!(!is_error, "{once}");
    let (listed, _) = server.answer(6, "tasks", json!({"kind": "schedule"}));
    assert_eq!(task_ids(&listed), ["schedule:later"], "{listed}");
    let (stopped, _) = server.answer(7, "stop", json!({"id": "schedule:later"}));
    assert_eq!(stopped["status"], json!("stopped"), "{stopped}");
    server.close();
}

/// A machine team whose layout cell resolves nowhere is refused as a schedule's team target with
/// its file's error, books nothing, and leaves a healthy agent and team bookable.
#[test]
fn a_schedule_refuses_a_machine_team_whose_layout_cell_does_not_resolve() {
    let env = schedule_room();
    let crew = crate::common::write_definition(
        &env,
        "teams",
        "crew",
        "leader: coder\nstages: [Build]\nlayout: \"coder,ghost\"\n\
         roles:\n  - {role: coder, agent: worker, owns: [Build]}",
        "Complete the work.",
    );
    let mut server = serve_as_agent(&env);
    server.initialize();
    let book = |tool: &str, arguments: serde_json::Value| {
        let mut call = json!({
            "name": "nightly",
            "trigger": "every",
            "value": "1h",
            "tool": tool,
            "arguments": arguments,
            "overlap": "replace",
        });
        if tool == "team" {
            call["worktree"] = json!("nightly");
        }
        call
    };
    let (refused, is_error) = server.answer(
        1,
        "schedule",
        book("team", json!({"team": "crew", "prompt": "ship it"})),
    );
    assert!(is_error, "{refused}");
    assert_eq!(
        refused,
        json!({"error": format!(
            "{}: team `crew` layout token `ghost` is not a declared role or valid roleless cell",
            crew.display()
        )})
    );
    assert!(loop_rows(&env).is_empty());
    for (id, tool, arguments) in [
        (2, "agent", json!({"profile": "worker", "prompt": "check"})),
        (4, "team", json!({"team": "forge", "prompt": "ship it"})),
    ] {
        let (booked, is_error) = server.answer(id, "schedule", book(tool, arguments));
        assert!(!is_error, "{booked}");
        let (stopped, _) = server.answer(id + 1, "stop", json!({"id": "schedule:nightly"}));
        assert_eq!(stopped["status"], json!("stopped"), "{stopped}");
    }
    server.close();
}

/// While a definition-set failure stands, the schedule tool's team target refuses every team with
/// the set's error and books nothing, matching the agent target and launches.
#[test]
fn a_schedule_team_refuses_while_definition_set_has_failed() {
    let env = schedule_room();
    crate::common::write_definition(
        &env,
        "teams",
        "crew",
        "leader: coder\nstages: [Build]\nroles:\n  - {role: coder, agent: worker, owns: [Build]}",
        "Complete the work.",
    );
    crate::common::write_definition(
        &env,
        "subagents",
        "crew",
        "description: Colliding child\nagent: claude\ntools: [Bash]",
        "Help.",
    );
    let mut server = serve_as_agent(&env);
    server.initialize();
    let expected = json!({"error": format!(
        "{}: team name `crew` is reserved for an inline profile/command cell; choose another team name",
        env.agents_home().display()
    )});
    for (id, team) in [(1, "crew"), (2, "forge")] {
        let (refused, is_error) = server.answer(
            id,
            "schedule",
            json!({
                "name": "nightly",
                "trigger": "every",
                "value": "1h",
                "worktree": "nightly",
                "tool": "team",
                "arguments": {"team": team, "prompt": "ship it"},
                "overlap": "replace",
            }),
        );
        assert!(is_error, "{refused}");
        assert_eq!(refused, expected);
    }
    assert!(loop_rows(&env).is_empty());
    server.close();
}

/// What the tool stores for each target kind passes the check `rimz loop add` makes of a fire's
/// launch, so a booking it accepts fires.
#[test]
fn each_target_a_schedule_books_passes_the_commands_launch_check() {
    let env = schedule_room();
    let mut server = serve_as_agent(&env);
    server.initialize();
    for (id, name, trigger, value, tool, args, worktree) in [
        (
            1,
            "to-agent",
            "cron",
            "0 9 * * mon",
            "agent",
            json!({"profile": " worker ", "prompt": "- Review the changes"}),
            None,
        ),
        (
            2,
            "to-builder",
            "cron",
            "0 9 * * mon",
            "agent",
            json!({"profile": "worker", "prompt": "- Review the changes"}),
            Some("build"),
        ),
        (
            3,
            "to-builder-fresh",
            "cron",
            "0 9 * * mon",
            "agent",
            json!({"profile": "worker", "prompt": "go", "fresh": true}),
            Some("build-fresh"),
        ),
        (
            4,
            "to-crew",
            "cron",
            "0 9 * * mon",
            "team",
            json!({"team": " forge ", "prompt": "- ship it", "description": "- daily crew"}),
            Some("crew"),
        ),
        (
            5,
            "to-pr-team",
            "in",
            "30m",
            "team",
            json!({"team": "forge", "from_pr": "42", "prompt": "ship pr"}),
            None,
        ),
        (
            6,
            "to-pr-agent",
            "cron",
            "0 9 * * mon",
            "agent",
            json!({"profile": "worker", "from_pr": "42", "prompt": "watch pr"}),
            None,
        ),
        (
            7,
            "to-pr-seat",
            "in",
            "30m",
            "agent",
            json!({"profile": "forge.coder", "from_pr": "42", "prompt": "ship pr seat"}),
            None,
        ),
        (
            8,
            "to-workflow",
            "cron",
            "0 9 * * mon",
            "workflow",
            json!({
                "name": "held",
                "prompt": "--look at the flaky test",
                "deadline": "8h",
                "give_up": "2h",
                "skip_capacity": true,
            }),
            None,
        ),
    ] {
        let mut arguments = json!({
            "name": name,
            "trigger": trigger,
            "value": value,
            "tool": tool,
            "arguments": args,
        });
        if let Some(worktree) = worktree {
            arguments["worktree"] = json!(worktree);
        }
        let (booked, is_error) = server.answer(id, "schedule", arguments);
        assert!(!is_error, "{name}: {booked}");
    }
    server.close();
    let rows = loop_rows(&env);
    let instances = loop_instances(&env).0;
    // Targets are trimmed when stored.
    assert_eq!(rows["to-agent"]["target"].as_str(), Some("worker"));
    assert_eq!(rows["to-crew"]["target"].as_str(), Some("forge"));
    // An agent or team booked in a worktree launches fresh there (once even when `fresh: true`),
    // so a fire meeting the closed worktree an earlier fire left is not refused; one booked
    // without a worktree carries no `--fresh`.
    for (name, args) in [
        ("to-agent", vec!["- Review the changes"]),
        ("to-builder", vec!["--fresh", "- Review the changes"]),
        ("to-builder-fresh", vec!["--fresh", "go"]),
        (
            "to-crew",
            vec!["--fresh", "--description=- daily crew", "- ship it"],
        ),
        ("to-pr-agent", vec!["--from-pr=42", "watch pr"]),
    ] {
        assert_eq!(
            rows[name]["args"],
            toml::Value::from(args),
            "{name}: {:?}",
            rows[name]
        );
    }
    let pr_team = &instances["to-pr-team"];
    assert_eq!(pr_team.target.as_deref(), Some("forge"));
    assert_eq!(pr_team.args, vec!["--from-pr=42", "ship pr"]);
    assert_eq!(pr_team.worktree, None);
    let pr_seat = &instances["to-pr-seat"];
    assert_eq!(pr_seat.target.as_deref(), Some("forge.coder"));
    assert_eq!(pr_seat.args, vec!["--from-pr=42", "ship pr seat"]);
    assert_eq!(pr_seat.worktree, None);
    for (name, flag, row) in [
        ("to-pr-team", "--team", pr_team),
        ("to-pr-seat", "--agent", pr_seat),
    ] {
        let check = env
            .rimz()
            .args([
                "loop",
                "add",
                &format!("check-{name}"),
                flag,
                row.target.as_deref().unwrap(),
                "--every",
                "1h",
                "--",
            ])
            .args(&row.args)
            .output()
            .unwrap();
        assert!(check.status.success(), "{name} {:?}: {check:?}", row.args);
    }
    for (name, flag) in [
        ("to-agent", "--agent"),
        ("to-builder", "--agent"),
        ("to-builder-fresh", "--agent"),
        ("to-crew", "--team"),
        ("to-pr-agent", "--agent"),
        ("to-workflow", "--workflow"),
    ] {
        let row = rows[name].as_table().unwrap();
        let tail: Vec<&str> = row
            .get("args")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .map(|word| word.as_str().unwrap())
            .collect();
        let check = format!("check-{name}");
        let worktree = row
            .get("worktree")
            .map(|name| ["-w", name.as_str().unwrap()]);
        let output = env
            .rimz()
            .args(["loop", "add", &check, flag, row["target"].as_str().unwrap()])
            .args(worktree.iter().flatten())
            .args(["--every", "1h", "--"])
            .args(&tail)
            .output()
            .unwrap();
        assert!(output.status.success(), "{name} {tail:?}: {output:?}");
    }
}

/// A booking carrying `cwd`, `name`, `agent`, and `description` resolves relative `cwd` at the
/// call, routes `agent` into a usage-window condition's provider lookup, omits the root-run
/// warning when a place is set, and stores a row `rimz loop add` accepts.
#[test]
fn a_schedule_carrying_cwd_name_agent_and_description_stores_a_row_the_command_accepts() {
    let env = schedule_room();
    crate::common::write_definition(&env, "agents", "codex", "description: Codex base", "Base.");
    crate::common::write_definition(
        &env,
        "agents",
        "codex-worker",
        "description: Codex worker\nagent: codex\ntools: []",
        "",
    );
    env.install_agent_hooks("claude");
    crate::common::wait::publish_claude_5h(&env, 10);
    let sub = env.project_root.join("nested-dir");
    std::fs::create_dir_all(&sub).unwrap();
    let expected_cwd = std::fs::canonicalize(&sub).unwrap();

    let mut server = serve_as_agent(&env);
    server.initialize();
    let (booked, is_error) = server.answer(
        1,
        "schedule",
        json!({
            "name": "full-agent",
            "trigger": "when",
            "value": "window.5h.left>=40",
            "tool": "agent",
            "arguments": {
                "profile": "codex-worker",
                "prompt": "check the queue",
                "cwd": "nested-dir",
                "name": "helper",
                "agent": "claude",
                "description": "- nightly check",
            },
        }),
    );
    assert!(!is_error, "{booked}");
    // A repeating booking without `overlap` names `arguments.prompt` in its idle note.
    let (repeating, is_error) = server.answer(
        2,
        "schedule",
        json!({
            "name": "idle-note",
            "trigger": "every",
            "value": "1h",
            "worktree": "crew",
            "tool": "team",
            "arguments": {"team": "forge", "prompt": "keep shipping"},
        }),
    );
    assert!(!is_error, "{repeating}");
    let warnings = repeating["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|line| line.as_str().unwrap_or("").contains("`arguments.prompt`")),
        "{repeating}"
    );
    server.close();

    let instances = loop_instances(&env).0;
    let full = &instances["full-agent"];
    assert_eq!(
        full.provider.as_ref().map(|kind| kind.as_str()),
        Some("claude"),
        "{full:?}"
    );
    let tail: Vec<&str> = full.args.iter().map(String::as_str).collect();
    let expected_cwd_flag = format!("--cwd={}", expected_cwd.display());
    assert!(
        tail.contains(&expected_cwd_flag.as_str()),
        "expected {expected_cwd_flag} in {tail:?}"
    );
    assert!(tail.contains(&"--name=helper"), "{tail:?}");
    assert!(tail.contains(&"--agent=claude"), "{tail:?}");
    assert!(tail.contains(&"--description=- nightly check"), "{tail:?}");

    let output = env
        .rimz()
        .args([
            "loop",
            "add",
            "check-full",
            "--agent",
            "codex-worker",
            "--every",
            "1h",
            "--",
        ])
        .args(&tail)
        .output()
        .unwrap();
    assert!(output.status.success(), "{tail:?}: {output:?}");

    // From a linked worktree, the "runs at the project root" warning appears only when the
    // booking carries no place at all (`worktree`, `from_pr`, or `cwd`).
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "rimz@example.test"],
        &["config", "user.name", "RimZ Test"],
        &["commit", "-q", "--allow-empty", "-m", "init"],
    ] {
        crate::common::git::git(&env.project_root, args);
    }
    let made = env
        .rimz()
        .args(["worktree", "new", "side"])
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
    let side = env.home_root.join("project-worktrees/side");
    let mut command = crate::common::wait::agent_wait(&env);
    command.current_dir(&side).args(["mcp", "serve"]);
    let mut in_wt = McpServer::spawn(command);
    in_wt.initialize();
    let (placed, is_error) = in_wt.answer(
        1,
        "schedule",
        json!({
            "name": "placed-cwd",
            "trigger": "in",
            "value": "10m",
            "tool": "agent",
            "arguments": {"profile": "worker", "prompt": "go", "cwd": "."},
        }),
    );
    assert!(!is_error, "{placed}");
    assert!(
        placed.get("warnings").is_none(),
        "placed booking should not warn about project root: {placed}"
    );
    let (placed_pr, is_error) = in_wt.answer(
        2,
        "schedule",
        json!({
            "name": "placed-pr",
            "trigger": "in",
            "value": "10m",
            "tool": "agent",
            "arguments": {"profile": "worker", "prompt": "go", "from_pr": "42"},
        }),
    );
    assert!(!is_error, "{placed_pr}");
    assert!(
        placed_pr.get("warnings").is_none(),
        "from_pr booking should not warn about project root: {placed_pr}"
    );
    let (unplaced, is_error) = in_wt.answer(
        3,
        "schedule",
        json!({
            "name": "unplaced",
            "trigger": "in",
            "value": "10m",
            "tool": "agent",
            "arguments": {"profile": "worker", "prompt": "go"},
        }),
    );
    assert!(!is_error, "{unplaced}");
    let root_warnings = unplaced["warnings"].as_array().unwrap();
    assert!(
        root_warnings.iter().any(|line| line
            .as_str()
            .unwrap_or("")
            .contains("runs at the project root")),
        "{unplaced}"
    );
    in_wt.close();
}

/// A repeating `when` booking with `for` and a repeating `signal` booking with `match` store
/// rows whose trigger and launch, read back as `rimz loop add` flags, pass the command's own
/// checks.
#[test]
fn a_when_and_a_signal_booking_store_rows_the_command_accepts() {
    let env = schedule_room();
    let mut server = serve_as_agent(&env);
    server.initialize();
    for (id, name, mut call) in [
        (
            1,
            "on-green",
            json!({"trigger": "when", "value": "ci=passed", "for": "10m"}),
        ),
        (
            2,
            "on-red",
            json!({"trigger": "signal", "value": "ci.failed", "match": {"branch": "feature"}}),
        ),
    ] {
        let map = call.as_object_mut().unwrap();
        map.insert("name".to_owned(), json!(name));
        map.insert("repeat".to_owned(), json!(true));
        map.insert("overlap".to_owned(), json!("replace"));
        map.insert("tool".to_owned(), json!("agent"));
        map.insert(
            "arguments".to_owned(),
            json!({"profile": "worker", "prompt": "fix it"}),
        );
        let (booked, is_error) = server.answer(id, "schedule", call);
        assert!(!is_error, "{booked}");
    }
    server.close();
    let rows = loop_rows(&env);
    let text = |row: &toml::Table, key: &str| row[key].as_str().unwrap().to_owned();
    let on_green = rows["on-green"].as_table().unwrap();
    assert_eq!(
        on_green["when"],
        toml::Value::from(vec!["ci=passed"]),
        "{on_green:?}"
    );
    assert_eq!(text(on_green, "for"), "10m", "{on_green:?}");
    let on_red = rows["on-red"].as_table().unwrap();
    assert_eq!(text(on_red, "signal"), "ci.failed", "{on_red:?}");
    let matched: Vec<String> = on_red["match"]
        .as_table()
        .unwrap()
        .iter()
        .map(|(key, value)| format!("{key}={}", value.as_str().unwrap()))
        .collect();
    assert_eq!(matched, ["branch=feature"], "{on_red:?}");
    let green_trigger = vec![
        "--when".to_owned(),
        on_green["when"][0].as_str().unwrap().to_owned(),
        "--for".to_owned(),
        text(on_green, "for"),
    ];
    let mut red_trigger = vec!["--signal".to_owned(), text(on_red, "signal")];
    red_trigger.extend(
        matched
            .iter()
            .flat_map(|pair| ["--match".to_owned(), pair.clone()]),
    );
    for (name, row, trigger) in [
        ("on-green", on_green, green_trigger),
        ("on-red", on_red, red_trigger),
    ] {
        assert_eq!(row["repeat"], toml::Value::from(true), "{row:?}");
        let tail: Vec<&str> = row["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|word| word.as_str().unwrap())
            .collect();
        let output = env
            .rimz()
            .args(["loop", "add", &format!("check-{name}"), "--agent"])
            .arg(row["target"].as_str().unwrap())
            .args(&trigger)
            .args(["--repeat", "--overlap", &text(row, "overlap"), "--"])
            .args(&tail)
            .output()
            .unwrap();
        assert!(output.status.success(), "{trigger:?} {tail:?}: {output:?}");
    }
}

/// Each refusal of `rimz loop add` and of a fire reaches the caller in the tool's own terms,
/// and nothing is booked.
#[test]
fn the_schedule_tool_refuses_what_the_command_refuses() {
    let env = schedule_room();
    deploy_workflow(
        &env,
        "whole",
        "[workflow]\nscope = 'project'\nstart = 'hold'\n\
         [test.hold]\ntimeout = '5m'\nshell = 'true'\non = { pass = '@done', fail = '@failed' }\n",
    );
    let mut server = serve_as_agent(&env);
    server.initialize();
    let agent = json!({"profile": "worker", "prompt": "go"});
    let held = |extra: Value| {
        let mut workflow = json!({"name": "held"});
        workflow
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        workflow
    };
    let cases = [
        (
            json!({"name": "bad", "trigger": "every", "value": "1h", "repeat": true, "tool": "agent", "arguments": agent}),
            "invalid arguments: `repeat` takes a `signal` or `when` trigger".to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "in", "value": "1h", "overlap": "queue", "tool": "agent", "arguments": agent}),
            "invalid arguments: `overlap` takes a repeating trigger".to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "every", "value": "1h", "match": {"branch": "x"}, "tool": "agent", "arguments": agent}),
            "invalid arguments: `match` takes a `signal` trigger".to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "cron", "value": "@daily", "for": "1m", "tool": "agent", "arguments": agent}),
            "invalid arguments: `for` takes a `when` trigger".to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "in", "value": "0s", "tool": "agent", "arguments": agent}),
            "invalid arguments: `value`: must be greater than zero".to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "in", "value": "9999999999d", "tool": "agent", "arguments": agent}),
            "invalid in `9999999999d`: lands past the latest time RimZ can schedule".to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "signal", "value": "ci.finished", "tool": "agent", "arguments": agent}),
            "schedule `bad`: ci.finished was replaced by ci.passed, ci.failed, or 'ci.*'; leave \
             `conclusion` out of `match`"
                .to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "signal", "value": "ci.*", "match": {"conclusion": "failure"},
                   "tool": "agent", "arguments": agent}),
            "schedule `bad`: ci.finished was replaced by ci.passed, ci.failed, or 'ci.*'; leave \
             `conclusion` out of `match`"
                .to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "at", "value": "soon", "tool": "agent", "arguments": agent}),
            "invalid arguments: `value`: `soon` is not `HH:MM`, `<weekday> HH:MM`, or `YYYY-MM-DD \
             HH:MM`"
                .to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "every", "value": "1h", "tool": "agent",
                   "arguments": {"profile": "worker", "prompt": "-w now"}}),
            "invalid arguments: `arguments.prompt` starts with `-`, which a fire's launch reads as a flag"
                .to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "every", "value": "1h", "tool": "workflow",
                   "arguments": held(json!({"input": {"outcome": "go"}}))}),
            "invalid arguments: `arguments.input`: a fire starts a run with no input".to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "every", "value": "1h", "tool": "workflow",
                   "arguments": held(json!({"detach": true}))}),
            "invalid arguments: `arguments.detach`: a fire's run owes its task a report".to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "every", "value": "1h", "tool": "workflow",
                   "arguments": held(json!({"worktree": "feat"}))}),
            "invalid arguments: `arguments.worktree`: pass top-level `worktree` instead".to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "every", "value": "1h", "tool": "agent",
                   "arguments": {"profile": "worker", "prompt": "go", "worktree": "feat"}}),
            "invalid arguments: `arguments.worktree`: pass top-level `worktree` instead".to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "in", "value": "5m", "tool": "team",
                   "arguments": {"team": "forge", "prompt": "go"}}),
            "invalid arguments: `arguments` (with top-level `worktree`): give exactly one of \
             `worktree` and `from_pr`"
                .to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "every", "value": "1h", "tool": "team",
                   "arguments": {"team": "forge", "prompt": "go", "from_pr": "42"}}),
            "invalid arguments: `arguments.from_pr`: a repeating team booking cannot use `from_pr`; \
             set top-level `worktree` or use a one-shot trigger"
                .to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "every", "value": "1h", "tool": "agent",
                   "arguments": {"profile": "forge.coder", "prompt": "go", "from_pr": "42"}}),
            "`arguments.from_pr`: a repeating booking of a team seat cannot use `from_pr`; set \
             top-level `worktree` or use a one-shot trigger"
                .to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "signal", "value": "ci.failed", "repeat": true,
                   "tool": "agent",
                   "arguments": {"profile": "forge.coder", "prompt": "go", "from_pr": "42"}}),
            "`arguments.from_pr`: a repeating booking of a team seat cannot use `from_pr`; set \
             top-level `worktree` or use a one-shot trigger"
                .to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "every", "value": "1h", "worktree": "crew", "tool": "team",
                   "arguments": {"team": "forge#crew", "prompt": "go"}}),
            "invalid arguments: `arguments.team` cannot include `#lane`; pass top-level `worktree` instead"
                .to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "every", "value": "1h", "worktree": "crew", "tool": "team",
                   "arguments": {"team": "ghost", "prompt": "go"}}),
            "unknown team `ghost`".to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "every", "value": "1h", "tool": "agent",
                   "arguments": {"profile": "forge", "prompt": "go"}}),
            "`arguments.profile` `forge` is a team; book it with `tool` `team`".to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "every", "value": "1h", "worktree": "feat",
                   "tool": "workflow", "arguments": {"name": "whole"}}),
            "workflow `whole` has scope = project, so it runs at the project root; remove `worktree`"
                .to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "every", "value": "1h", "tool": "agent",
                   "arguments": {"profile": "claude,true", "prompt": "go"}}),
            "`arguments.profile` `claude,true` names more than one cell; name one agent profile"
                .to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "every", "value": "1h", "tool": "agent",
                   "arguments": {"profile": "true", "prompt": "go"}}),
            "`arguments.profile` `true` names a command, not an agent; name an agent profile"
                .to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "every", "value": "1h", "worktree": "-probe",
                   "tool": "workflow", "arguments": held(json!({}))}),
            "invalid arguments: `worktree` starts with `-`, which a fire's launch reads as a flag"
                .to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "every", "value": "1h", "tool": "workflow",
                   "arguments": {"name": "-nightly"}}),
            "invalid arguments: `arguments.name` starts with `-`, which a fire's launch reads as a flag"
                .to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "every", "value": "1h", "worktree": "crew", "tool": "team",
                   "arguments": {"team": "-nightly", "prompt": "ship it"}}),
            "invalid arguments: `arguments.team` starts with `-`, which a fire's launch reads as \
             a flag"
                .to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "every", "value": "1h", "tool": "agent",
                   "arguments": {"profile": "worker", "prompt": "go\u{0}"}}),
            "invalid arguments: `arguments.prompt` holds a NUL character, which a fire's launch \
             cannot pass"
                .to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "every", "value": "1h", "worktree": "crew", "tool": "team",
                   "arguments": {"team": "forge", "prompt": "go\u{0}"}}),
            "invalid arguments: `arguments.prompt` holds a NUL character, which a fire's launch \
             cannot pass"
                .to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "every", "value": "1h", "tool": "workflow",
                   "arguments": held(json!({"prompt": "go\u{0}"}))}),
            "invalid arguments: `arguments.prompt` holds a NUL character, which a fire's \
             launch cannot pass"
                .to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "signal", "value": "ci.failed", "match": {"": "x"},
                   "tool": "agent", "arguments": agent}),
            "invalid arguments: a `match` key is empty".to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "every", "value": "1h", "tool": "agent",
                   "arguments": {"profile": "logs", "prompt": "go"}}),
            "invalid arguments: `arguments.profile` `logs` is a `rimz agents` verb, which a \
             fire's launch reads in place of a profile"
                .to_owned(),
        ),
        (
            json!({"name": "bad", "trigger": "every", "value": "1h", "tool": "agent",
                   "arguments": {"profile": "worker", "prompt": "go", "name": "bad name"}}),
            "`arguments.name`: invalid agent name `bad name`; use ASCII letters, numbers, and `-`"
                .to_owned(),
        ),
    ];
    let mut mismatches = Vec::new();
    for (id, (arguments, expected)) in (1..).zip(cases) {
        let (failure, is_error) = server.answer(id, "schedule", arguments.clone());
        if !is_error {
            mismatches.push(format!("case {id} accepted: {failure}"));
        } else if failure["error"] != json!(expected) {
            mismatches.push(format!(
                "case {id}: got {:?}, wanted {expected:?}",
                failure["error"]
            ));
        }
    }
    assert_eq!(mismatches, Vec::<String>::new());

    let (failure, is_error) = server.answer(
        98,
        "schedule",
        json!({"name": "bad", "trigger": "every", "value": "1h", "tool": "agent",
               "arguments": {"profile": "worker", "prompt": "go", "cwd": "no-such-dir"}}),
    );
    assert!(is_error, "{failure}");
    assert!(
        failure["error"]
            .as_str()
            .unwrap()
            .starts_with("no directory "),
        "{failure}"
    );
    let (failure, is_error) = server.answer(
        99,
        "schedule",
        json!({"name": "bad", "trigger": "every", "value": "1h", "tool": "agent",
               "arguments": {"profile": "worker", "prompt": "go", "from_pr": "not-a-pr"}}),
    );
    assert!(is_error, "{failure}");
    assert!(
        failure["error"]
            .as_str()
            .unwrap()
            .starts_with("`arguments.from_pr`: "),
        "{failure}"
    );
    let (failure, is_error) = server.answer(
        100,
        "schedule",
        json!({"name": "bad", "trigger": "every", "value": "1h", "tool": "workflow",
               "arguments": {"name": "ghost"}}),
    );
    assert!(is_error, "{failure}");
    assert!(
        failure["error"]
            .as_str()
            .unwrap()
            .starts_with("ghost: no workflow of that name in "),
        "{failure}"
    );
    // A team's fire reads the machine config strictly, so a config it cannot load is refused
    // at booking rather than at every fire.
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[agents
",
    )
    .unwrap();
    let (failure, is_error) = server.answer(
        101,
        "schedule",
        json!({"name": "bad", "trigger": "every", "value": "1h", "worktree": "crew", "tool": "team",
               "arguments": {"team": "forge", "prompt": "go"}}),
    );
    assert!(is_error, "{failure}");
    assert!(
        failure["error"]
            .as_str()
            .unwrap()
            .starts_with("the machine config does not load, so a team's fire cannot launch: "),
        "{failure}"
    );
    assert!(loop_rows(&env).is_empty(), "{:?}", loop_rows(&env));
    assert!(
        loop_instances(&env).0.is_empty(),
        "{:?}",
        loop_instances(&env).0
    );
    server.close();
}

/// Padded path components (`" . "`, `" .. "`) are not trimmed into local workflow directories:
/// the checked string is looked up as a deployed name, refused when absent, and nothing is booked.
#[test]
fn a_schedule_refuses_padded_path_workflow_names() {
    let env = schedule_room();
    std::fs::write(env.project_root.join("workflow.toml"), HELD_WORKFLOW).unwrap();
    std::fs::write(
        env.project_root.parent().unwrap().join("workflow.toml"),
        HELD_WORKFLOW,
    )
    .unwrap();
    let mut server = serve_as_agent(&env);
    server.initialize();
    for (id, padded) in [(1, " . "), (2, " .. ")] {
        let (failure, is_error) = server.answer(
            id,
            "schedule",
            json!({
                "name": "bad-wf",
                "trigger": "every",
                "value": "1h",
                "tool": "workflow",
                "arguments": {"name": padded},
            }),
        );
        assert!(is_error, "{padded:?} was accepted: {failure}");
        assert!(
            failure["error"]
                .as_str()
                .unwrap()
                .contains("no workflow of that name in "),
            "{padded:?}: {failure}"
        );
    }
    assert!(loop_rows(&env).is_empty(), "{:?}", loop_rows(&env));
    assert!(
        loop_instances(&env).0.is_empty(),
        "{:?}",
        loop_instances(&env).0
    );
    server.close();
}

/// A scoped caller's `arguments.cwd` under `/tmp` is stored as its host temp unit, so a fire
/// (which runs without the caller's mount view) runs in the directory the caller meant.
#[cfg(target_os = "linux")]
#[test]
fn a_scoped_callers_schedule_cwd_stores_the_host_temp_unit() {
    let env = schedule_room();
    let unit = env.store().paths().temp_unit_dir(Some("planner"));
    std::fs::create_dir_all(&unit).unwrap();
    let expected_host = std::fs::canonicalize(&unit).unwrap();
    let mut command = crate::common::wait::agent_wait(&env);
    command
        .env("RIMZ_ISOLATION", "scoped")
        .args(["mcp", "serve"]);
    let mut server = McpServer::spawn(command);
    server.initialize();
    let (booked, is_error) = server.answer(
        1,
        "schedule",
        json!({
            "name": "scoped-cwd",
            "trigger": "in",
            "value": "10m",
            "tool": "agent",
            "arguments": {"profile": "worker", "prompt": "go", "cwd": "/tmp"},
        }),
    );
    assert!(!is_error, "{booked}");
    server.close();
    let instances = loop_instances(&env).0;
    let row = &instances["scoped-cwd"];
    let expected_flag = format!("--cwd={}", expected_host.display());
    assert!(
        row.args.contains(&expected_flag),
        "expected {expected_flag} in {:?}",
        row.args
    );
}

/// A name the caller booked is replaced; one another starter holds is refused and left as it
/// was.
#[test]
fn a_schedule_replaces_only_a_name_its_caller_booked() {
    let env = schedule_room();
    let output = env
        .rimz()
        .args(["loop", "add", "taken", "--every", "2h", "--", "true"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let mut server = serve_as_agent(&env);
    server.initialize();
    let book = |name: &str, every: &str| {
        json!({
            "name": name,
            "trigger": "every",
            "value": every,
            "tool": "agent",
            "arguments": {"profile": "worker", "prompt": "go"},
        })
    };
    let (failure, is_error) = server.answer(1, "schedule", book("taken", "1h"));
    assert!(is_error, "{failure}");
    assert_eq!(
        failure["error"],
        json!("a schedule named `taken` exists that you did not book; choose another name")
    );
    assert_eq!(loop_rows(&env)["taken"]["every"].as_str(), Some("2h"));
    let (_, is_error) = server.answer(2, "schedule", book("mine", "1h"));
    assert!(!is_error);
    let (again, is_error) = server.answer(3, "schedule", book("mine", "3h"));
    assert!(!is_error, "{again}");
    assert_eq!(loop_rows(&env)["mine"]["every"].as_str(), Some("3h"));
    // A molt to another provider keeps the launch id, so the booking stays the caller's.
    let path = env.rimz_home().join("loop.toml");
    let mut table =
        toml::from_str::<toml::Table>(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let link = &mut table["tasks"]["mine"]["booked-by"][0];
    assert_eq!(
        link["agent_id"].as_str(),
        Some("launch-session"),
        "{link:?}"
    );
    link["kind"] = toml::Value::from("codex");
    std::fs::write(&path, toml::to_string(&table).unwrap()).unwrap();
    let (molted, is_error) = server.answer(4, "schedule", book("mine", "4h"));
    assert!(!is_error, "{molted}");
    assert_eq!(loop_rows(&env)["mine"]["every"].as_str(), Some("4h"));
    server.close();
}

/// What a booking of `tool` + `arguments` in a worktree stores, launched as its fire launches it,
/// meets the worktree an earlier fire left closed and launches fresh there.
#[cfg(unix)]
fn booking_fire_launches_fresh_into_a_closed_worktree(
    kind: rimz::config::LaunchKind,
    tool: &str,
    arguments: Value,
) {
    let env = crate::agent_launch::team_worktree_launch_env();
    crate::common::wait::register_calling_agent(&env);
    let made = env
        .rimz()
        .args(["worktree", "new", "demo"])
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
    crate::common::trust_claude_project(&env, &env.home_root.join("project-worktrees/demo"));
    let mut server = serve_as_agent(&env);
    server.initialize();
    let (booked, is_error) = server.answer(
        1,
        "schedule",
        json!({"name": "crew", "trigger": "every", "value": "1h", "worktree": "demo",
               "overlap": "replace", "tool": tool, "arguments": arguments}),
    );
    assert!(!is_error, "{booked}");
    server.close();
    let row = loop_rows(&env)["crew"].as_table().unwrap().clone();
    let text = |value: &toml::Value| value.as_str().unwrap().to_owned();
    let argv = rimz::harness::schedule::TaskLaunch {
        kind,
        target: text(&row["target"]),
        worktree: row.get("worktree").map(text),
        args: row["args"].as_array().unwrap().iter().map(text).collect(),
    }
    .argv();
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let output = crate::agent_launch::launch_team_worktree(&env, &argv);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{argv:?}: {stderr}");
    assert!(!stderr.contains("work in progress"), "{stderr}");
}

#[cfg(unix)]
#[test]
fn a_team_booking_fire_launches_fresh_into_the_worktree_an_earlier_fire_left() {
    booking_fire_launches_fresh_into_a_closed_worktree(
        rimz::config::LaunchKind::Team,
        "team",
        json!({"team": "solo", "prompt": "ship it"}),
    );
}

#[cfg(unix)]
#[test]
fn an_agent_booking_fire_launches_fresh_into_the_worktree_an_earlier_fire_left() {
    booking_fire_launches_fresh_into_a_closed_worktree(
        rimz::config::LaunchKind::Agent,
        "agent",
        json!({"profile": "worker", "prompt": "ship it"}),
    );
}

/// A caller at the chain limit books nothing: a fire resolves no caller, so its launch would
/// pass the limit a direct launch refuses.
#[test]
fn the_schedule_tool_refuses_a_caller_at_the_chain_limit() {
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent_with_launch(
        &env,
        rimz::agents::LaunchParams {
            launch_depth: Some(3),
            ..rimz::agents::LaunchParams::default()
        },
    );
    schedule_targets(&env);
    let mut server = serve_as_agent(&env);
    server.initialize();
    let (failure, is_error) = server.answer(
        1,
        "schedule",
        json!({"name": "nightly", "trigger": "every", "value": "1h",
               "tool": "agent", "arguments": {"profile": "worker", "prompt": "go"}}),
    );
    assert!(is_error, "{failure}");
    assert_eq!(
        failure["error"],
        json!(
            "booking refused: this agent is 3 launches deep in an agent chain, and a fire's \
             launch would exceed this workspace's maximum chain length of 3; do not retry this \
             booking"
        )
    );
    assert!(loop_rows(&env).is_empty());
    server.close();
}

/// A caller RimZ cannot identify books nothing: no one could list or stop the booking.
#[test]
fn the_schedule_tool_books_nothing_for_a_caller_it_cannot_identify() {
    let env = Env::new();
    env.record(&env.project_root);
    schedule_targets(&env);
    let mut command = env.rimz();
    command.args(["mcp", "serve"]);
    let mut stranger = McpServer::spawn(command);
    stranger.initialize();
    let (failure, is_error) = stranger.answer(
        1,
        "schedule",
        json!({"name": "nightly", "trigger": "every", "value": "1h",
               "tool": "agent", "arguments": {"profile": "worker", "prompt": "go"}}),
    );
    assert!(is_error, "{failure}");
    assert_eq!(
        failure["error"],
        json!("booking a schedule is only available to an agent RimZ can identify")
    );
    assert!(loop_rows(&env).is_empty());
    stranger.close();
}
