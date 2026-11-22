//! Newline-delimited JSON-RPC 2.0 over stdio. The main thread reads requests
//! and answers each inline, a `tools/call` of `guide` included; a call of any
//! other tool runs in this process on a thread of its own, through the runner
//! `serve` was handed, so calls overlap and a later one may answer first.
//! The protocol goes out on a private copy of stdout taken at the start, and
//! descriptor 1 then points at stderr, so a stray write under a tool path
//! lands in the logs, not in the protocol.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{Value, json};

use super::catalog::{
    Call, Capability, LoadMode, SERVER_NAME, Tool, served_groups, tools_for, tools_list,
};
use super::runner::{Answer, Failure, Runner};
use super::wording::Wording;

const PROTOCOL_VERSIONS: [&str; 2] = ["2025-06-18", "2025-11-25"];
const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";

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
/// SIGTERM, carrying out each call through `runner`. `modes` gives the tools
/// of each capability it names that load mode in their listing. The wording is
/// read once, here, from the RimZ home, so a session keeps the text its server
/// started with.
pub fn serve(
    only: &[Capability],
    modes: &[(Capability, LoadMode)],
    runner: Arc<dyn Runner>,
) -> Result<(), ServeErr> {
    let out = protocol_out();
    let wording = Wording::read_home();
    for warning in wording.warnings() {
        tracing::warn!("rimz mcp serve: {warning}");
    }
    let tools: Vec<&'static Tool> = tools_for(only).collect();
    let groups = served_groups(only);
    let server = Arc::new(Server {
        served: Served {
            listing: tools_list(&tools, modes, &wording),
            instructions: wording.instructions(&groups),
            tools,
            groups,
            wording,
        },
        calls: Calls::default(),
        out: Mutex::new(out),
        runner,
    });
    #[cfg(unix)]
    {
        let server = Arc::clone(&server);
        let signals = vec![signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM];
        if let Err(err) = crate::child_process::register_signal_wake(signals, move |_| {
            server.calls.close();
            std::process::exit(0);
        }) {
            tracing::warn!(%err, "rimz mcp serve: SIGINT and SIGTERM will not end the server");
        }
    }
    let result = read_requests(&server);
    server.calls.close();
    result
}

/// The server's state, shared with the thread of each call.
struct Server {
    served: Served,
    calls: Calls,
    out: Mutex<Box<dyn Write + Send>>,
    runner: Arc<dyn Runner>,
}

/// What the server answers with, resolved once at its start.
struct Served {
    tools: Vec<&'static Tool>,
    groups: Vec<Capability>,
    listing: Value,
    instructions: String,
    wording: Wording,
}

/// The protocol's channel: a private copy of stdout, with descriptor 1 then
/// pointed at stderr; stdout itself when that cannot be done.
fn protocol_out() -> Box<dyn Write + Send> {
    #[cfg(unix)]
    match take_stdout() {
        Ok(private) => return Box::new(private),
        Err(err) => tracing::warn!(
            %err,
            "rimz mcp serve: the protocol shares stdout; a stray write can break it"
        ),
    }
    Box::new(io::stdout())
}

/// Copy stdout to a new descriptor that a child does not inherit, then point
/// descriptor 1 at stderr.
#[cfg(unix)]
fn take_stdout() -> io::Result<std::fs::File> {
    use std::os::fd::AsFd;

    let mut stdout = io::stdout().lock();
    stdout.flush()?;
    let private = stdout.as_fd().try_clone_to_owned()?;
    nix::unistd::dup2_stdout(io::stderr()).map_err(io::Error::from)?;
    Ok(std::fs::File::from(private))
}

fn read_requests(server: &Arc<Server>) -> Result<(), ServeErr> {
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
            Ok(message) => handle(message, server),
            Err(err) => {
                server.send_error(&Value::Null, PARSE_ERROR, &format!("parse error: {err}"))
            }
        }
    }
}

fn handle(message: Value, server: &Arc<Server>) {
    let Value::Object(mut message) = message else {
        return server.send_error(&Value::Null, INVALID_REQUEST, "a request is a JSON object");
    };
    let id = message.remove("id");
    let params = message.remove("params").unwrap_or(Value::Null);
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        // A response to a request this server never sends.
        return;
    };
    let Some(id) = id else {
        if method == "notifications/cancelled" {
            server.calls.cancel(&params["requestId"]);
        }
        return;
    };
    let served = &server.served;
    match method {
        "initialize" => server.send_result(&id, initialize(&params, &served.instructions)),
        "ping" => server.send_result(&id, json!({})),
        "tools/list" => server.send_result(&id, served.listing.clone()),
        "tools/call" => call(id, params, server),
        _ => server.send_error(
            &id,
            METHOD_NOT_FOUND,
            &format!("method not found: {method}"),
        ),
    }
}

fn initialize(params: &Value, instructions: &str) -> Value {
    let asked = params["protocolVersion"].as_str();
    let version = asked
        .filter(|asked| PROTOCOL_VERSIONS.contains(asked))
        .unwrap_or(LATEST_PROTOCOL_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": SERVER_NAME, "version": crate::build_id::VERSION },
        "instructions": instructions,
    })
}

fn call(id: Value, mut params: Value, server: &Arc<Server>) {
    let name = params["name"].as_str().unwrap_or_default();
    let Some(tool) = server.served.tools.iter().find(|tool| tool.name == name) else {
        return server.send_error(&id, INVALID_PARAMS, &format!("unknown tool: {name:?}"));
    };
    let arguments = match params["arguments"].take() {
        Value::Null => json!({}),
        arguments => arguments,
    };
    let call = match tool.call(arguments) {
        Ok(call) => call,
        Err(err) => {
            let failure = Failure::new(format!("invalid arguments: {err}"));
            return server.send_result(&id, tool_result(Answer::Failure(failure)));
        }
    };
    if let Call::Guide(guide_args) = call {
        return server.send_result(&id, tool_result(guide(&server.served, &guide_args.name)));
    }
    // Listed here, on the thread that reads requests, so a cancel read after
    // this call, or the server's end, always finds it.
    let Some(seq) = server.calls.begin(id.clone()) else {
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("rimz-mcp-call".to_owned())
        .spawn({
            let (id, server) = (id.clone(), Arc::clone(server));
            move || {
                let answer = run_isolated(server.runner.as_ref(), call);
                if server.calls.finish(seq) {
                    server.send_result(&id, tool_result(answer));
                }
            }
        });
    if let Err(err) = spawned
        && server.calls.finish(seq)
    {
        let failure = Failure::new(format!("could not start the call: {err}"));
        server.send_result(&id, tool_result(Answer::Failure(failure)));
    }
}

/// The answer `runner` gives `call`, or a failure when it panics, so one
/// call's panic leaves the server and every other call running.
fn run_isolated(runner: &dyn Runner, call: Call) -> Answer {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runner.run(call))).unwrap_or_else(
        |panic| {
            let reason = panic
                .downcast_ref::<&str>()
                .map(|reason| (*reason).to_owned())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "no message".to_owned());
            Answer::Failure(Failure::new(format!("the call panicked: {reason}")))
        },
    )
}

/// The answer to a `guide` call for the guide `name`.
fn guide(served: &Served, name: &str) -> Answer {
    match served.wording.guide(&served.groups, name) {
        Ok(text) => Answer::Text(text.to_owned()),
        Err(guides) => Answer::Failure(
            Failure::new(format!("no guide {name:?}"))
                .with_hint(format!("this launch has: {}", guides.join(", "))),
        ),
    }
}

/// The result a call's answer makes: data as structured content with the
/// same JSON as its text item, text as the text item, and a failure as an
/// error whose text item is its JSON, which it also carries as structured
/// content.
fn tool_result(answer: Answer) -> Value {
    let (structured, is_error) = match answer {
        Answer::Text(text) => {
            return json!({ "content": [{ "type": "text", "text": text }], "isError": false });
        }
        Answer::Data(map) => (Value::Object(map), false),
        Answer::Failure(failure) => (failure.into_value(), true),
    };
    json!({
        "content": [{ "type": "text", "text": structured.to_string() }],
        "structuredContent": structured,
        "isError": is_error,
    })
}

/// The calls in flight, each under the request id that started it, from
/// before its thread starts until it answers.
#[derive(Default)]
struct Calls {
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
    cancelled: bool,
}

impl Calls {
    fn lock(&self) -> MutexGuard<'_, CallsState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// List a call about to start; `None` once the server is ending, when it
    /// must start nothing.
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
                cancelled: false,
            },
        );
        Some(seq)
    }

    /// Unlist a call that has its answer; `false` when a cancel or the
    /// server's end withholds its reply.
    fn finish(&self, seq: u64) -> bool {
        let mut state = self.lock();
        let closed = state.closed;
        state
            .live
            .remove(&seq)
            .is_some_and(|call| !call.cancelled && !closed)
    }

    /// Withhold the reply of the call `id` names; its work runs to its end,
    /// since a thread cannot be stopped from outside.
    fn cancel(&self, id: &Value) {
        let mut state = self.lock();
        for call in state.live.values_mut().filter(|call| &call.id == id) {
            call.cancelled = true;
        }
    }

    fn close(&self) {
        self.lock().closed = true;
    }
}

impl Server {
    fn send_result(&self, id: &Value, result: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    fn send_error(&self, id: &Value, code: i64, message: &str) {
        self.send(
            &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
        );
    }

    fn send(&self, message: &Value) {
        let mut line = message.to_string();
        line.push('\n');
        let mut out = self
            .out
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Err(err) = out.write_all(line.as_bytes()).and_then(|()| out.flush()) {
            tracing::debug!(%err, "rimz mcp serve: the client stopped reading");
        }
    }
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
