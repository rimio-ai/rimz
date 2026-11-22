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

const ALL_TOOLS: [&str; 10] = [
    "lsp",
    "wait",
    "wait_list",
    "wait_cancel",
    "self_stop",
    "self_compact",
    "self_tune",
    "notes_add",
    "notes_list",
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
            (json!("wait"), always.clone()),
            (json!("wait_list"), always.clone()),
            (json!("wait_cancel"), always),
            (json!("guide"), None),
        ]
    );
    let (text, is_error) = server.call(2, "wait_list", json!({}));
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
             rimz-control, rimz-notes) with an optional `:always` or `:deferred`, named once"
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
            (json!("wait"), deferred.clone()),
            (json!("wait_list"), deferred.clone()),
            (json!("wait_cancel"), deferred),
            (json!("notes_add"), None),
            (json!("notes_list"), None),
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
        format!("total: ~{sum} tokens in 10 tools"),
        "{stdout}"
    );

    let (status, _, stderr) = run(&["--only", "rimz-message"]);
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("expected one of rimz-lsp, rimz-wait, rimz-control, rimz-notes"),
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
    assert_eq!(names, ["wait", "wait_list", "wait_cancel", "guide"]);

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
        "List your waits, replaced. {}",
        "Each wait, its name. ".repeat(40)
    );
    std::fs::write(mcps.join("wait_list.md"), format!("{list}\n")).unwrap();
    std::fs::write(mcps.join("instructions.md"), "Use `wait`, replaced.\n").unwrap();

    let mut server = serve(&env, &["--only", "rimz-wait"]);
    let init = server.initialize();
    let instructions = init["result"]["instructions"].as_str().unwrap().to_owned();
    assert!(
        instructions.starts_with("Use `wait`, replaced.\n"),
        "{instructions}"
    );
    let listing = server.request(1, "tools/list", json!({}));
    let described = listing["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "wait_list")
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
        .find(|line| line.split_whitespace().nth(1) == Some("wait_list"))
        .and_then(|line| line.split_whitespace().last())
        .and_then(|cell| cell.strip_prefix('~')?.parse().ok())
        .unwrap_or_else(|| panic!("no wait_list row: {stdout}"));
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
            mcps.join("wait_list.md").display()
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
    std::fs::write(mcps.join("rimz-wait/wait_list.md"), " \n").unwrap();
    std::fs::write(mcps.join("rimz-lsp/wait.md"), "Misplaced.\n").unwrap();
    std::fs::write(mcps.join("rimz-lsp/lsp.md"), "\n").unwrap();
    let stderr = explain();
    let rejected = format!(
        "rimz: {} is not used, so the built-in text is served: it is empty\n",
        mcps.join("rimz-wait/wait_list.md").display()
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
fn notes_add_keeps_the_body_bytes_and_the_server_keeps_parsing() {
    let env = Env::new();
    let mut server = serve(&env, &[]);
    server.initialize();
    let body = "say \"hi\" and 'bye'\n$(touch pwned) `id`\n\n\\n not a newline\n";
    let (added, is_error) = server.answer(
        1,
        "notes_add",
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

    let (failure, is_error) = server.answer(3, "notes_add", json!({"kind": "log", "title": 7}));
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
    let (waits, is_error) = server.answer(2, "wait_list", json!({}));
    assert!(!is_error, "{waits}");
    let rows = waits["waits"].as_array().expect("the wait rows");
    let [row] = rows.as_slice() else {
        panic!("one wait: {waits}");
    };
    assert!(row.to_string().contains("via mcp"), "{waits}");
    let name = row["name"].as_str().expect("the wait's name");
    assert_eq!(
        start,
        json!({
            "id": format!("wait:{name}"),
            "status": "running",
            "stop": format!("wait_cancel({{name: \"{name}\"}})"),
            "report": "a message to you when the wait fires",
        })
    );
    let (added, is_error) = server.answer(
        3,
        "notes_add",
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
    let (waits, _) = server.answer(4, "wait_list", json!({}));
    assert_eq!(waits["waits"], json!([]), "a refused label arms nothing");
    let (start, is_error) = server.answer(
        5,
        "wait",
        json!({"trigger": "in", "value": "1h", "label": "  fix it  "}),
    );
    assert!(!is_error, "{start}");
    let (waits, _) = server.answer(6, "wait_list", json!({}));
    assert_eq!(waits["waits"][0]["label"], json!("fix it"), "{waits}");
}

/// A booked `self_stop` answers what holds the stop, as the command's receipt
/// prints it; the control and wait tools answer data for the inputs the
/// command's flags took (`off`, a tuned value, `all`).
#[test]
fn self_stop_answers_what_holds_it_and_control_calls_answer_data() {
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::wait::register_calling_agent(&env);
    let mut server = serve_as_agent(&env);
    server.initialize();
    let (start, is_error) = server.answer(1, "wait", json!({"trigger": "in", "value": "2h"}));
    assert!(!is_error, "{start}");
    let name = start["id"].as_str().unwrap().trim_start_matches("wait:");
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
    let (canceled, is_error) = server.answer(5, "wait_cancel", json!({"all": true}));
    assert!(!is_error, "{canceled}");
    assert_eq!(canceled["canceled"], json!([name]), "{canceled}");
    let (listed, is_error) = server.answer(6, "notes_list", json!({}));
    assert!(!is_error, "{listed}");
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
        "notes_add",
        json!({"kind": "stdin", "title": "From stdin", "file": "/dev/stdin"}),
    );
    server.send(&json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}));
    let (failure, is_error) = tool_answer("notes_add", &server.reply(&json!(1)));
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
    ] {
        let tools = if kind == "pi" { "" } else { "\ntools: [Bash]" };
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
    let explain = |profile: &str| {
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

    let out = explain("cmark");
    assert!(!out.contains("Plan"), "{out}");
    assert!(
        out.contains("serves rimz-lsp, rimz-wait, rimz-control, rimz-notes"),
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

    for skill in ["rimz-lsp", "rimz-wait", "rimz-control", "rimz-notes"] {
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
    assert_eq!(tool_load(&out, "notes_list"), "always", "{out}");
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
        out.contains("serves rimz-lsp, rimz-wait, rimz-control, rimz-notes"),
        "{out}"
    );
    assert_eq!(tool_load(&out, "lsp"), "deferred", "{out}");
    assert_eq!(tool_load(&out, "notes_add"), "deferred", "{out}");
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
        out.contains("serves rimz-lsp, rimz-wait, rimz-control, rimz-notes as `rimz_tools`\n"),
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
        out.contains("serves rimz-lsp, rimz-wait, rimz-control, rimz-notes as `rimz_tools`\n"),
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
    let (waits, is_error) = server.answer(2, "wait_list", json!({}));
    assert!(!is_error, "{waits}");
    assert!(waits.to_string().contains("cleared"), "{waits}");
    let (added, is_error) = server.answer(
        3,
        "notes_add",
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
        "notes_add",
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
        mcps.join("wait_list.md"),
        "List your waits, from the parent's home.\n",
    )
    .unwrap();
    let mut server = serve_under_parent(&env, &[]);
    server.initialize();
    let listing = server.request(1, "tools/list", json!({}));
    let described = listing["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "wait_list")
        .map(|tool| tool["description"].clone());
    assert_eq!(
        described,
        Some(json!("List your waits, from the parent's home."))
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

/// An idle server holds the parked wrapper's private-memory budget.
#[cfg(target_os = "linux")]
#[test]
fn an_idle_server_stays_inside_its_memory_budget() {
    let env = Env::new();
    let mut server = serve(&env, &[]);
    server.initialize();
    server.request(1, "tools/list", json!({}));
    std::thread::sleep(Duration::from_millis(500));
    crate::common::assert_wrapper_memory_budget("idle mcp server", server.pid(), 8 * 1024);
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
