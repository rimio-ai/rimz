//! Deterministic stdio language server for the shared broker integration tests.

use rimz::lsp::protocol::{read_frame, write_frame};
use serde_json::{Value, json};

fn main() {
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    let mut indexed = false;
    let mut changes = Value::Null;
    let mut root = String::new();
    let alias_location = |file| json!({"uri": format!("file:///fixture/{file}.rs"), "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 5}}});
    while let Ok(message) = read_frame(&mut input) {
        let method = message["method"].as_str().unwrap_or("");
        let result = match method {
            "initialize" => {
                root = message["params"]["rootUri"].as_str().unwrap().to_owned();
                json!({"capabilities": {}})
            }
            "initialized" => {
                for kind in ["begin", "end"] {
                    write_frame(&mut output, &json!({"jsonrpc": "2.0", "method": "$/progress", "params": {"token": "index", "value": {"kind": kind}}})).unwrap();
                }
                indexed = true;
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
                if message["params"]["query"] == "changes" {
                    changes.clone()
                } else if message["params"]["query"] == "alias" {
                    json!([{"name": "alias", "kind": 12, "location": alias_location("alias")}, {"name": "alias", "kind": 12, "location": alias_location("definition")}])
                } else if let Some(name @ ("pathed" | "twin" | "work")) =
                    message["params"]["query"].as_str()
                {
                    let entries = match name {
                        "pathed" => vec![("pathed", "deep/pathed")],
                        "twin" => vec![("twin", "b"), ("twin", "a")],
                        _ => vec![
                            ("unrelated", "u"),
                            ("rework", "r"),
                            ("worker", "w"),
                            ("work", "w"),
                        ],
                    };
                    json!(entries.into_iter().map(|(name, file)| json!({"name": name, "kind": 12, "location": {"uri": format!("{root}/src/{file}.rs"), "range": alias_location("alias")["range"]}})).collect::<Vec<_>>())
                } else {
                    json!([])
                }
            }
            "textDocument/hover" => json!({"contents": "fixture hover"}),
            "textDocument/definition"
                if message["params"]["textDocument"]["uri"]
                    .as_str()
                    .is_some_and(|uri| uri.starts_with("file:///fixture/")) =>
            {
                json!([alias_location("definition")])
            }
            "textDocument/definition"
                if message["params"]["textDocument"]["uri"]
                    .as_str()
                    .is_some_and(|uri| uri.starts_with(&format!("{root}/src/"))) =>
            {
                json!([{"uri": message["params"]["textDocument"]["uri"], "range": alias_location("alias")["range"]}])
            }
            "shutdown" => Value::Null,
            "exit" => break,
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
