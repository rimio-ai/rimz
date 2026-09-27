//! Deterministic stdio language server for the shared broker integration tests.

use rimz::lsp::protocol::{read_frame, write_frame};
use serde_json::{Value, json};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut sections = Vec::<String>::new();
    let mut adaptable = false;
    let mut hold_index = false;
    let mut reload_configuration = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--configuration-section" => sections.push(args.next().expect("section argument")),
            "--adaptable-replies" => adaptable = true,
            "--hold-index-progress" => hold_index = true,
            "--reload-configuration" => reload_configuration = true,
            _ => panic!("unknown stub argument: {arg}"),
        }
    }
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    let mut indexed = false;
    let mut capabilities = Value::Null;
    let mut changes = Value::Null;
    let mut root = String::new();
    let mut initialized = false;
    let mut stopping = false;
    let mut documents: Vec<Value> = Vec::new();
    let mut duplicates = 0;
    let mut held = false;
    let mut diagnostics = Vec::new();
    let mut request_ids = Vec::new();
    let mut requests = Vec::new();
    let mut cancellations = Vec::new();
    let mut configuration_changes = 0;
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
        if method.is_empty() {
            continue;
        }
        let result = match method {
            "workspace/didChangeConfiguration" if reload_configuration => {
                requests.push(message.clone());
                configuration_changes += 1;
                write_frame(&mut output, &json!({"jsonrpc":"2.0","id":format!("configuration-change:{configuration_changes}"),"method":"workspace/configuration","params":{"items":[{"section":"rust-analyzer"}]}})).unwrap();
                continue;
            }
            "$/cancelRequest" => {
                cancellations.push(message["params"]["id"].clone());
                continue;
            }
            "initialize" => {
                capabilities = message["params"]["capabilities"].clone();
                root = message["params"]["rootUri"].as_str().unwrap().to_owned();
                assert!(!initialized, "one initialize per lifetime");
                initialized = true;
                json!({"capabilities": {}})
            }
            "initialized" => {
                assert!(!indexed, "one initialized per lifetime");
                if !sections.is_empty() {
                    write_frame(&mut output, &json!({"jsonrpc":"2.0","id":"configuration","method":"workspace/configuration","params":{"items":sections.iter().map(|section| json!({"section":section})).collect::<Vec<_>>()}})).unwrap();
                }
                if hold_index {
                    write_frame(&mut output, &json!({"jsonrpc":"2.0","id":"index-create","method":"window/workDoneProgress/create","params":{"token":"index"}})).unwrap();
                }
                for kind in ["begin", if hold_index { "report" } else { "end" }] {
                    write_frame(&mut output, &json!({"jsonrpc": "2.0", "method": "$/progress", "params": {"token": "index", "value": {"kind": kind,"title":"Indexing","percentage":50}}})).unwrap();
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
                if message["params"]["query"] == "release-index" {
                    write_frame(&mut output, &json!({"jsonrpc":"2.0","method":"$/progress","params":{"token":"index","value":{"kind":"end"}}})).unwrap();
                }
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
                } else if let Some(name @ ("Type" | "TwinType" | "UniqueMember" | "BroadType")) =
                    message["params"]["query"].as_str()
                {
                    let files = match name {
                        "TwinType" => vec!["a/lib.rs".to_owned(), "b/lib.rs".to_owned()],
                        "UniqueMember" => vec!["lib.rs".to_owned(), "empty.rs".to_owned()],
                        "BroadType" => (0..21).map(|n| format!("broad{n}.rs")).collect(),
                        _ => vec!["lib.rs".to_owned()],
                    };
                    json!(files.into_iter().map(|file| json!({"name":name,"kind":23,"location":{"uri":format!("{}/{file}", root.trim_end_matches('/')),"range":alias_location("alias")["range"]}})).collect::<Vec<_>>())
                } else if message["params"]["query"] == "Many" {
                    json!((0..45).rev().map(|n| json!({"name":"Many","kind":12,"location":{"uri":format!("{root}/src/c{n:02}.rs"),"range":alias_location("alias")["range"]}})).collect::<Vec<_>>())
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
            "textDocument/hover" => {
                if message["params"]["hold"] == true {
                    continue;
                }
                json!({"contents": message["params"]["marker"].as_str().unwrap_or("fixture hover")})
            }
            "textDocument/documentSymbol"
                if message["params"]["textDocument"]["uri"]
                    .as_str()
                    .is_some_and(|uri| uri.ends_with("/lib.rs")) =>
            {
                let symbol = |name, kind, start, end, children| {
                    let range = json!({"start":{"line":start,"character":0},"end":{"line":end,"character":1}});
                    json!({"name":name,"kind":kind,"range":range,"selectionRange":range,"children":children})
                };
                json!([
                    symbol(
                        "Type",
                        23,
                        0,
                        1,
                        json!([symbol("field", 8, 1, 1, json!([]))])
                    ),
                    symbol(
                        "impl Type",
                        19,
                        2,
                        5,
                        json!([symbol("method", 6, 2, 5, json!([]))])
                    ),
                    symbol("saved", 12, 6, 6, json!([]))
                ])
            }
            "textDocument/completion" if adaptable => {
                let snippets = capabilities["textDocument"]["completion"]["completionItem"]["snippetSupport"]
                    == true;
                json!([{"label":"foo", "insertTextFormat":if snippets {2} else {1},"insertText":if snippets {"foo(${1:x})$0"} else {"foo(x)"}}])
            }
            "textDocument/definition" if adaptable => {
                let uri = &message["params"]["textDocument"]["uri"];
                let range =
                    json!({"start":{"line":0,"character":0},"end":{"line":0,"character":3}});
                if capabilities["textDocument"]["definition"]["linkSupport"] == true {
                    json!([{"targetUri":uri,"targetRange":range,"targetSelectionRange":range}])
                } else {
                    json!([{"uri":uri,"range":range}])
                }
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
            "textDocument/definition"
                if message["params"]["textDocument"]["uri"]
                    .as_str()
                    .is_some_and(|uri| uri.starts_with(&format!("{root}/src/"))) =>
            {
                json!([{"uri": message["params"]["textDocument"]["uri"], "range": alias_location("alias")["range"]}])
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
