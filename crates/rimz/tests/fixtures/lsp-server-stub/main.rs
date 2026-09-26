//! Deterministic stdio language server for the shared broker integration tests.

use rimz::lsp::protocol::{read_frame, write_frame};
use serde_json::{Value, json};

fn main() {
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    let mut indexed = false;
    let mut changes = Value::Null;
    let mut initialized = false;
    let mut stopping = false;
    let mut documents: Vec<Value> = Vec::new();
    let mut duplicates = 0;
    let mut held = false;
    let mut diagnostics = Vec::new();
    let mut request_ids = Vec::new();
    let mut requests = Vec::new();
    let mut cancellations = Vec::new();
    let alias_location = |file| json!({"uri": format!("file:///fixture/{file}.rs"), "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 5}}});
    while let Ok(message) = read_frame(&mut input) {
        if let Some(id) = message.get("id") {
            assert!(
                !request_ids.contains(id),
                "server request IDs must be distinct: {id}"
            );
            request_ids.push(id.clone());
            requests.push(message.clone());
        }
        let method = message["method"].as_str().unwrap_or("");
        let result = match method {
            "$/cancelRequest" => {
                cancellations.push(message["params"]["id"].clone());
                continue;
            }
            "initialize" => {
                assert!(!initialized, "one initialize per lifetime");
                initialized = true;
                json!({"capabilities": {}})
            }
            "initialized" => {
                assert!(!indexed, "one initialized per lifetime");
                for kind in ["begin", "end"] {
                    write_frame(&mut output, &json!({"jsonrpc": "2.0", "method": "$/progress", "params": {"token": "index", "value": {"kind": kind}}})).unwrap();
                }
                indexed = true;
                write_frame(&mut output, &json!({"jsonrpc":"2.0", "method":"experimental/serverStatus", "params":{"health":"ok", "quiescent":true}})).unwrap();
                continue;
            }
            "textDocument/didOpen" | "textDocument/didChange" => {
                let document = &message["params"]["textDocument"];
                let uri = &document["uri"];
                let index = documents.iter().position(|d| d["uri"] == *uri);
                if method == "textDocument/didOpen" {
                    duplicates += usize::from(index.is_some());
                    documents.push(
                        json!({"uri":uri,"version":document["version"],"text":document["text"]}),
                    );
                } else {
                    let index = index.expect("change of open document");
                    documents[index]["version"] = document["version"].clone();
                    documents[index]["text"] =
                        message["params"]["contentChanges"][0]["text"].clone();
                }
                let frame = json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":uri,"version":document["version"],"diagnostics":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":6}},"message":"fixture diagnostic"}]}});
                if held {
                    diagnostics.push(frame);
                } else {
                    write_frame(&mut output, &frame).unwrap();
                }
                continue;
            }
            "textDocument/didClose" => {
                documents.retain(|d| d["uri"] != message["params"]["textDocument"]["uri"]);
                continue;
            }
            "workspace/didChangeWatchedFiles" => {
                changes = message["params"].clone();
                continue;
            }
            "workspace/symbol" => {
                assert!(indexed, "queries must wait for initialized");
                if message["params"]["query"] == "slow" {
                    std::thread::sleep(std::time::Duration::from_secs(8));
                }
                if message["params"]["query"] == "open" {
                    json!(documents)
                } else if message["params"]["query"] == "opened" {
                    json!(documents.iter().map(|d| json!({"name":"opened","kind":12,"location":{"uri":d["uri"],"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":6}}}})).collect::<Vec<_>>())
                } else if message["params"]["query"] == "duplicates" {
                    json!(duplicates)
                } else if message["params"]["query"] == "requests" {
                    json!({"requests":requests,"cancellations":cancellations})
                } else if message["params"]["query"] == "hold-diagnostics on" {
                    held = true;
                    Value::Null
                } else if message["params"]["query"] == "hold-diagnostics off" {
                    held = false;
                    for frame in diagnostics.drain(..) {
                        write_frame(&mut output, &frame).unwrap();
                    }
                    Value::Null
                } else if message["params"]["query"] == "changes" {
                    changes.clone()
                } else if message["params"]["query"] == "alias" {
                    json!([{"name": "alias", "kind": 12, "location": alias_location("alias")}, {"name": "alias", "kind": 12, "location": alias_location("definition")}])
                } else {
                    json!([])
                }
            }
            "textDocument/hover" => {
                if message["params"]["hold"] == true {
                    continue;
                }
                json!({"contents": message["params"]["marker"].as_str().unwrap_or("fixture hover")})
            }
            "textDocument/definition"
                if documents
                    .iter()
                    .any(|d| d["uri"] == message["params"]["textDocument"]["uri"]) =>
            {
                json!([{"uri":message["params"]["textDocument"]["uri"],"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":6}}}])
            }
            "textDocument/definition"
                if message["params"]["textDocument"]["uri"]
                    .as_str()
                    .is_some_and(|uri| uri.starts_with("file:///fixture/")) =>
            {
                json!([alias_location("definition")])
            }
            "shutdown" => {
                assert!(!stopping, "one shutdown per lifetime");
                stopping = true;
                Value::Null
            }
            "exit" => {
                assert!(stopping, "exit follows shutdown");
                break;
            }
            _ => json!([]),
        };
        if let Some(id) = message.get("id") {
            write_frame(
                &mut output,
                &json!({"jsonrpc": "2.0", "id": id, "result": result}),
            )
            .unwrap();
        }
    }
}
