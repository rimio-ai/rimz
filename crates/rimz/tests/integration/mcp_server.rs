//! `rimz mcp serve` integration tests: the stdio protocol a provider speaks to
//! a spawned server, and the `rimz` one-shots its tool calls run.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use rimz::mcp::catalog::INSTRUCTIONS;

use crate::common::Env;

const REPLY_DEADLINE: Duration = Duration::from_secs(60);

const ALL_TOOLS: [&str; 9] = [
    "lsp",
    "wait",
    "wait_list",
    "wait_cancel",
    "self_stop",
    "self_compact",
    "self_tune",
    "notes_add",
    "notes_list",
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
    use std::io::Read;

    let env = Env::new();
    let pair = native_pty_system().openpty(PtySize::default()).unwrap();
    let mut command = CommandBuilder::new(env.rimz_bin());
    env.pin_pty_command(&mut command);
    command.cwd(&env.home_root);
    command.args(["mcp", "serve"]);
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
    let always = Some(json!({"anthropic/alwaysLoad": true}));
    let deferred = Some(json!({"anthropic/alwaysLoad": false}));
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
        format!("total: ~{sum} tokens in 9 tools"),
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
    assert_eq!(result["capabilities"], json!({"tools": {}}), "{init}");
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
    server.initialize();
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

    let output = env
        .rimz()
        .args(["mcp", "serve", "--only", "rimz-loop"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
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

/// The body crosses as bytes on the one-shot's stdin, the title and an LSP
/// target that begin with `--` stay data, and the one-shot's stderr comes back
/// in the result rather than on the server's stdout.
#[test]
fn notes_add_keeps_the_body_bytes_and_the_server_keeps_parsing() {
    let env = Env::new();
    let mut server = serve(&env, &[]);
    server.initialize();
    let body = "say \"hi\" and 'bye'\n$(touch pwned) `id`\n\n\\n not a newline\n";
    let (text, is_error) = server.call(
        1,
        "notes_add",
        json!({
            "kind": "handoff",
            "title": "--force",
            "body": body,
            "meta": {"author": "mallory", "ticket": "42"},
        }),
    );
    assert!(!is_error, "{text}");
    let label = text.find("\nstderr:\n").unwrap_or_else(|| panic!("{text}"));
    let warning = text
        .find("--meta author dropped: RimZ writes that header key itself")
        .unwrap_or_else(|| panic!("the one-shot's stderr is in the result: {text}"));
    assert!(label < warning, "stderr follows its label: {text}");
    let path = std::path::PathBuf::from(text.lines().next().expect("the note's path"));
    let note = std::fs::read_to_string(&path).expect("read note");
    assert!(note.ends_with(&format!("\n---\n{body}")), "{note}");
    assert!(note.contains("title: \"--force\"\n"), "{note}");
    assert!(note.contains("ticket: \"42\"\n"), "{note}");
    assert!(!env.project_root.join("pwned").exists());
    assert_eq!(server.request(2, "ping", json!({}))["result"], json!({}));

    let (text, is_error) = server.call(3, "notes_add", json!({"kind": "log", "title": 7}));
    assert!(is_error, "{text}");
    let (text, is_error) = server.call(
        4,
        "notes_add",
        json!({"kind": "log", "title": "x", "body": "y", "cwd": "/no/such/dir"}),
    );
    assert!(is_error, "{text}");
    assert_eq!(notes_written(&env), [path]);

    let (text, is_error) = server.call(5, "lsp", json!({"verb": "find", "targets": ["--help"]}));
    assert!(!text.contains("Usage:"), "{text}");
    assert!(!text.contains("unexpected argument"), "{text}");
    assert!(is_error, "{text}");
    let label = text
        .lines()
        .position(|line| line == "stderr:")
        .unwrap_or_else(|| panic!("{text}"));
    let reason = text
        .lines()
        .position(|line| line.contains("no language server"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(label < reason, "stderr follows its label: {text}");
    assert_eq!(text.lines().last(), Some("exit code 3"), "{text}");

    let (text, is_error) = server.call(6, "wait", json!({"trigger": "pid", "value": "one"}));
    assert!(is_error, "{text}");
    let usage = text
        .find("stderr:\nerror: invalid value 'one' for '--pid <PID>'")
        .unwrap_or_else(|| panic!("the CLI's usage error under stderr: {text}"));
    assert!(!text[..usage].contains("invalid arguments"), "{text}");
    assert_eq!(text.lines().last(), Some("exit code 2"), "{text}");

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
    let (text, is_error) = server.call(
        1,
        "wait",
        json!({"trigger": "in", "value": "1h", "label": "via mcp"}),
    );
    assert!(!is_error, "{text}");
    let (text, is_error) = server.call(2, "wait_list", json!({}));
    assert!(!is_error, "{text}");
    assert!(text.contains("via mcp"), "{text}");
    let (text, is_error) = server.call(
        3,
        "notes_add",
        json!({"kind": "diary", "title": "Day", "body": "text\n"}),
    );
    assert!(!is_error, "{text}");
    let note = std::fs::read_to_string(text.lines().next().unwrap()).expect("read note");
    assert!(note.contains("author: \"planner\"\n"), "{note}");
    assert!(
        note.starts_with(&env.rimz_home().display().to_string())
            || text.starts_with(&env.rimz_home().display().to_string()),
        "{text}"
    );
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

    let out = explain("cnone");
    assert!(
        out.contains("not handed over: the mcps list names no RimZ capability"),
        "{out}"
    );
    assert!(!out.contains("CAPABILITY"), "{out}");
    assert!(
        !out.contains("## lsp") && !out.contains(INSTRUCTIONS),
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
        !out.contains("## lsp") && !out.contains(INSTRUCTIONS),
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
        !out.contains("## lsp") && !out.contains(INSTRUCTIONS),
        "{out}"
    );
}

/// On Antigravity `explain --mcps` reads the planned agent file: the inline `rimz` item, the
/// servers `inheritMcp: false` keeps, and the file's two MCP lines; a launch that writes no
/// file hands over no server and limits nothing.
#[test]
fn explain_mcps_reads_the_antigravity_agent_file() {
    let explain = |env: &Env, profile: &str| {
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
    let env = Env::new();
    for (name, mcps) in [
        ("glimit", "\nmcps: [rimz-lsp, github, notion]"),
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

    let out = explain(&env, "glimit");
    assert!(out.contains("serves rimz-lsp\n"), "{out}");
    assert!(
        out.contains("only github, notion (inheritMcp: false)"),
        "{out}"
    );
    assert_eq!(tool_load(&out, "lsp"), "default", "{out}");
    assert!(out.contains("\n  inheritMcp: false\n"), "{out}");
    assert!(
        out.contains(r#"{"name": "github"}, {"name": "notion"}]"#),
        "{out}"
    );

    let out = explain(&env, "gopen");
    assert!(
        out.contains("serves rimz-lsp, rimz-wait, rimz-control, rimz-notes"),
        "{out}"
    );
    assert!(
        out.contains("every server the account and project configure"),
        "{out}"
    );
    assert!(out.contains("\n  inheritMcp: true\n"), "{out}");

    let bare = Env::new();
    let out = explain(&bare, "antigravity");
    assert!(
        out.contains("not handed over: this launch writes no agent file"),
        "{out}"
    );
    assert!(
        out.contains("every server the account and project configure"),
        "{out}"
    );
    assert!(
        !out.contains("CAPABILITY") && !out.contains("inheritMcp"),
        "{out}"
    );
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
            !out.contains("## lsp") && !out.contains(INSTRUCTIONS),
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
    let (text, is_error) = server.call(
        1,
        "wait",
        json!({"trigger": "in", "value": "1h", "label": "cleared"}),
    );
    assert!(!is_error, "{text}");
    let (text, is_error) = server.call(2, "wait_list", json!({}));
    assert!(!is_error, "{text}");
    assert!(text.contains("cleared"), "{text}");
    let (text, is_error) = server.call(
        3,
        "notes_add",
        json!({"kind": "diary", "title": "Day", "body": "text\n"}),
    );
    assert!(!is_error, "{text}");
    let note = std::fs::read_to_string(text.lines().next().unwrap()).expect("read note");
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
    // A one-shot without a home falls back under `TMPDIR`: keep it in the fixture.
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
    let (text, is_error) = server.call(
        id,
        "notes_add",
        json!({"kind": "diary", "title": format!("Day {id}"), "body": "text\n"}),
    );
    assert!(!is_error, "{text}");
    text.lines().next().expect("the note's path").to_owned()
}

/// A one-shot sees what only the parent's environment holds (the fixture's
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

/// The watcher a `rimz wait` one-shot detached outlives the provider ending
/// the server's whole process group.
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

/// Calls run concurrently, so a later call may answer first, and a cancel
/// kills that call's one-shot and withholds its reply.
#[cfg(target_os = "linux")]
#[test]
fn calls_run_concurrently_and_a_cancel_kills_its_one_shot() {
    let env = Env::new();
    let fifo = env.home_root.join("mcp-body-fifo-5b1e");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRWXU).unwrap();
    let marker = fifo.to_str().unwrap().to_owned();
    let one_shots = || -> Vec<u32> {
        std::fs::read_dir("/proc")
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
            .filter(|pid| {
                std::fs::read(format!("/proc/{pid}/cmdline"))
                    .is_ok_and(|cmdline| String::from_utf8_lossy(&cmdline).contains(&marker))
            })
            .collect()
    };

    let mut server = serve(&env, &[]);
    server.initialize();
    server.start_call(
        1,
        "notes_add",
        json!({"kind": "log", "title": "Blocked", "file": marker}),
    );
    let deadline = Instant::now() + REPLY_DEADLINE;
    while one_shots().is_empty() {
        assert!(
            Instant::now() < deadline,
            "the blocked one-shot never started"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let (text, is_error) = server.call(2, "notes_list", json!({}));
    assert!(!is_error, "{text}");
    assert!(server.unclaimed.is_empty(), "{:?}", server.unclaimed);

    server.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": 1, "reason": "test"},
    }));
    let deadline = Instant::now() + REPLY_DEADLINE;
    while !one_shots().is_empty() {
        assert!(Instant::now() < deadline, "the cancelled one-shot lives on");
        std::thread::sleep(Duration::from_millis(20));
    }
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
