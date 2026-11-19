//! Newline-delimited JSON-RPC 2.0 over stdio. The main thread reads requests
//! and answers all but `tools/call` inline; each call runs its one-shot on a
//! thread of its own, so calls overlap and a later one may answer first.
//! Stdout carries protocol lines only; logs go to stderr.

use std::collections::HashMap;
use std::io::{self, BufRead, Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{Value, json};

use super::catalog::{Capability, Invocation, SERVER_NAME, Tool, tools_for, tools_list};

const PROTOCOL_VERSIONS: [&str; 2] = ["2025-06-18", "2025-11-25"];
const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";

const INSTRUCTIONS: &str = "Each tool runs one `rimz` command as this agent, in its room, and \
                            returns that command's output; a command that fails returns an error \
                            result carrying its exit code.";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

#[derive(Debug, thiserror::Error)]
pub enum ServeErr {
    #[error("reading the client's requests: {0}")]
    Read(#[source] std::io::Error),
}

/// Serve `only`'s tools (every tool when empty) until stdin ends, SIGINT, or
/// SIGTERM, then kill the one-shots still running and return. With
/// `parent_env`, each one-shot also gets every variable of the parent
/// process's environment that the server's own lacks.
pub fn serve(only: &[Capability], parent_env: bool) -> Result<(), ServeErr> {
    let tools: Vec<&'static Tool> = tools_for(only).collect();
    let inherited = if parent_env {
        #[cfg(unix)]
        let parent = crate::proc::environ(std::os::unix::process::parent_id());
        #[cfg(not(unix))]
        let parent = None;
        inherited_from(parent).unwrap_or_else(|| {
            tracing::warn!(
                "rimz mcp serve: the parent's environment is unreadable; one-shots run with \
                 the server's own"
            );
            Vec::new()
        })
    } else {
        Vec::new()
    };
    let calls = Arc::new(Calls {
        inherited,
        state: Mutex::default(),
    });
    #[cfg(unix)]
    {
        let calls = Arc::clone(&calls);
        let signals = vec![signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM];
        if let Err(err) = crate::child_process::register_signal_wake(signals, move |_| {
            calls.close();
            std::process::exit(0);
        }) {
            tracing::warn!(%err, "rimz mcp serve: SIGINT and SIGTERM will not stop one-shots");
        }
    }
    let result = read_requests(&tools, &calls);
    calls.close();
    result
}

/// The parent's variables the server's own environment lacks; `None` when
/// the parent's environment is unreadable.
fn inherited_from(parent: Option<Vec<(String, String)>>) -> Option<Vec<(String, String)>> {
    Some(
        parent?
            .into_iter()
            .filter(|(name, _)| std::env::var_os(name).is_none())
            .collect(),
    )
}

fn read_requests(tools: &[&'static Tool], calls: &Arc<Calls>) -> Result<(), ServeErr> {
    let mut stdin = io::stdin().lock();
    let mut line = Vec::new();
    loop {
        line.clear();
        if stdin.read_until(b'\n', &mut line).map_err(ServeErr::Read)? == 0 {
            return Ok(());
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match serde_json::from_slice::<Value>(&line) {
            Ok(message) => handle(message, tools, calls),
            Err(err) => send_error(&Value::Null, PARSE_ERROR, &format!("parse error: {err}")),
        }
    }
}

fn handle(message: Value, tools: &[&'static Tool], calls: &Arc<Calls>) {
    let Value::Object(mut message) = message else {
        return send_error(&Value::Null, INVALID_REQUEST, "a request is a JSON object");
    };
    let id = message.remove("id");
    let params = message.remove("params").unwrap_or(Value::Null);
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        // A response to a request this server never sends.
        return;
    };
    let Some(id) = id else {
        if method == "notifications/cancelled" {
            calls.cancel(&params["requestId"]);
        }
        return;
    };
    match method {
        "initialize" => send_result(&id, initialize(&params)),
        "ping" => send_result(&id, json!({})),
        "tools/list" => send_result(&id, tools_list(tools)),
        "tools/call" => call(id, params, tools, calls),
        _ => send_error(
            &id,
            METHOD_NOT_FOUND,
            &format!("method not found: {method}"),
        ),
    }
}

fn initialize(params: &Value) -> Value {
    let asked = params["protocolVersion"].as_str();
    let version = asked
        .filter(|asked| PROTOCOL_VERSIONS.contains(asked))
        .unwrap_or(LATEST_PROTOCOL_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": SERVER_NAME, "version": crate::build_id::VERSION },
        "instructions": INSTRUCTIONS,
    })
}

fn call(id: Value, mut params: Value, tools: &[&'static Tool], calls: &Arc<Calls>) {
    let name = params["name"].as_str().unwrap_or_default();
    let Some(tool) = tools.iter().find(|tool| tool.name == name) else {
        return send_error(&id, INVALID_PARAMS, &format!("unknown tool: {name:?}"));
    };
    let arguments = match params["arguments"].take() {
        Value::Null => json!({}),
        arguments => arguments,
    };
    let invocation = match tool.invocation(arguments) {
        Ok(invocation) => invocation,
        Err(err) => {
            return send_result(&id, tool_result(&format!("invalid arguments: {err}"), true));
        }
    };
    // Listed here, on the thread that reads requests, so a cancel read after
    // this call, or the server's end, always finds it.
    let Some(seq) = calls.begin(id.clone()) else {
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("rimz-mcp-call".to_owned())
        .spawn({
            let (id, calls) = (id.clone(), Arc::clone(calls));
            move || run_one_shot(&id, seq, invocation, &calls)
        });
    if let Err(err) = spawned
        && calls.abandon(seq)
    {
        send_result(
            &id,
            tool_result(&format!("could not start the call: {err}"), true),
        );
    }
}

/// Run the call's one-shot and answer with its output, unless a cancel or
/// the server's end killed it first.
fn run_one_shot(id: &Value, seq: u64, invocation: Invocation, calls: &Calls) {
    let mut command = Command::new(crate::proc::rimz_exe());
    command
        .arg("--color=never")
        .args(&invocation.args)
        .envs(calls.inherited.iter().map(|(name, value)| (name, value)))
        .stdin(if invocation.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = &invocation.cwd {
        command.current_dir(cwd);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            if calls.abandon(seq) {
                send_result(id, tool_result(&format!("could not run rimz: {err}"), true));
            }
            return;
        }
    };
    let body = child.stdin.take().zip(invocation.stdin);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    if !calls.attach(seq, child) {
        return;
    }
    if let Some((mut pipe, body)) = body {
        // A one-shot that exits without reading its stdin closes the pipe;
        // its exit status is what the result reports.
        std::thread::spawn(move || drop(pipe.write_all(&body)));
    }
    let stderr = std::thread::spawn(move || read_all(stderr));
    let mut text = read_all(stdout);
    text.push_str(&stderr.join().unwrap_or_default());
    let Some(status) = calls.finish(seq) else {
        return;
    };
    match status {
        Ok(status) if status.success() => send_result(id, tool_result(&text, false)),
        Ok(status) => {
            append_line(&mut text, &exit_line(status));
            send_result(id, tool_result(&text, true));
        }
        Err(err) => {
            append_line(&mut text, &format!("waiting for rimz: {err}"));
            send_result(id, tool_result(&text, true));
        }
    }
}

fn read_all(pipe: Option<impl Read>) -> String {
    let mut bytes = Vec::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.read_to_end(&mut bytes);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn exit_line(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => format!("no exit code: {status}"),
    }
}

fn append_line(text: &mut String, line: &str) {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(line);
}

fn tool_result(text: &str, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

/// The calls in flight, each under the request id that started it, from
/// before its one-shot spawns until it is reaped. A listed child is never
/// waited on, so its pid cannot be reused while a cancel or the server's end
/// may still kill it.
struct Calls {
    /// The parent's variables a one-shot adds under `--parent-env`.
    inherited: Vec<(String, String)>,
    state: Mutex<CallsState>,
}

#[derive(Default)]
struct CallsState {
    closed: bool,
    next: u64,
    live: HashMap<u64, LiveCall>,
}

struct LiveCall {
    id: Value,
    /// `None` until the one-shot has spawned.
    child: Option<Child>,
    cancelled: bool,
}

impl Calls {
    fn lock(&self) -> MutexGuard<'_, CallsState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// List a call about to spawn; `None` once the server is ending, when it
    /// must spawn nothing.
    fn begin(&self, id: Value) -> Option<u64> {
        let mut state = self.lock();
        if state.closed {
            return None;
        }
        state.next += 1;
        let seq = state.next;
        state.live.insert(
            seq,
            LiveCall {
                id,
                child: None,
                cancelled: false,
            },
        );
        Some(seq)
    }

    /// Hand a listed call its spawned one-shot; `false`, with the one-shot
    /// killed and reaped, when a cancel or the server's end came first.
    fn attach(&self, seq: u64, mut child: Child) -> bool {
        let mut state = self.lock();
        let Some(call) = state.live.get_mut(&seq).filter(|call| !call.cancelled) else {
            state.live.remove(&seq);
            drop(state);
            let _ = child.kill();
            let _ = child.wait();
            return false;
        };
        call.child = Some(child);
        true
    }

    /// Unlist a call whose spawn failed; `false` when a cancel or the
    /// server's end withholds its reply.
    fn abandon(&self, seq: u64) -> bool {
        self.lock()
            .live
            .remove(&seq)
            .is_some_and(|call| !call.cancelled)
    }

    /// Reap a call's one-shot; `None` when a cancel or the server's end
    /// killed it, which withholds its reply.
    fn finish(&self, seq: u64) -> Option<io::Result<ExitStatus>> {
        let call = self.lock().live.remove(&seq)?;
        let status = call.child?.wait();
        (!call.cancelled).then_some(status)
    }

    fn cancel(&self, id: &Value) {
        let mut state = self.lock();
        for call in state.live.values_mut().filter(|call| &call.id == id) {
            call.kill();
        }
    }

    fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        state.live.values_mut().for_each(LiveCall::kill);
    }
}

impl LiveCall {
    fn kill(&mut self) {
        self.cancelled = true;
        if let Some(child) = &mut self.child {
            let _ = child.kill();
        }
    }
}

fn send_result(id: &Value, result: Value) {
    send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
}

fn send_error(id: &Value, code: i64, message: &str) {
    send(&json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }));
}

fn send(message: &Value) {
    let mut line = message.to_string();
    line.push('\n');
    let mut stdout = io::stdout().lock();
    if let Err(err) = stdout
        .write_all(line.as_bytes())
        .and_then(|()| stdout.flush())
    {
        tracing::debug!(%err, "rimz mcp serve: the client stopped reading");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parent_fills_only_names_the_server_lacks_and_unreadable_falls_back() {
        assert_eq!(inherited_from(None), None);
        let parent = vec![
            ("PATH".to_owned(), "/parent/bin".to_owned()),
            ("RIMZ_MCP_PARENT_ONLY_7c2e".to_owned(), "v".to_owned()),
        ];
        assert_eq!(
            inherited_from(Some(parent)),
            Some(vec![(
                "RIMZ_MCP_PARENT_ONLY_7c2e".to_owned(),
                "v".to_owned()
            )])
        );
    }
}
