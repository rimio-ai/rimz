use serde_json::{Map, json};

use super::*;
use crate::mcp::catalog::{StopArgs, TasksArgs};

#[test]
fn data_and_failures_repeat_their_json_as_text_and_text_stays_text() {
    let mut data = Map::new();
    data.insert("path".to_owned(), json!("/notes/a.md"));
    let result = tool_result(Answer::Data(data));
    assert_eq!(result["structuredContent"], json!({"path": "/notes/a.md"}));
    assert_eq!(result["isError"], json!(false));
    let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, result["structuredContent"]);

    let result = tool_result(Answer::Failure(
        Failure::new("no room here").with_hint("start one with rimz"),
    ));
    let expected = json!({"error": "no room here", "hint": "start one with rimz"});
    assert_eq!(result["isError"], json!(true));
    assert_eq!(result["structuredContent"], expected);
    let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, expected);
    let result = tool_result(Answer::Failure(Failure::new("bare")));
    assert_eq!(result["structuredContent"], json!({"error": "bare"}));

    let result = tool_result(Answer::Text("0 anchors\n".to_owned()));
    assert_eq!(
        result,
        json!({"content": [{"type": "text", "text": "0 anchors\n"}], "isError": false})
    );
}

/// Panics on a `tasks` call and answers every other call.
struct Flaky;

impl Runner for Flaky {
    fn run(&self, call: Call, _cancelled: &dyn Fn() -> bool) -> Answer {
        if let Call::Tasks(_) = call {
            panic!("boom");
        }
        Answer::data(&json!({"notes": []}))
    }
}

#[test]
fn a_call_that_panics_answers_a_failure_and_the_next_call_succeeds() {
    let runner = Flaky;
    assert_eq!(
        run_isolated(&runner, Call::Tasks(TasksArgs { kind: None }), &|| false),
        Answer::Failure(Failure::new("the call panicked: boom"))
    );
    let mut notes = Map::new();
    notes.insert("notes".to_owned(), json!([]));
    assert_eq!(
        run_isolated(
            &runner,
            Call::Stop(StopArgs {
                id: "shell:nightly".to_owned()
            }),
            &|| false
        ),
        Answer::Data(notes)
    );
}

/// Once the server holds the protocol, a write to descriptor 1 reaches
/// stderr and only protocol lines reach the client.
#[cfg(unix)]
#[test]
fn a_stray_stdout_write_lands_on_stderr_not_the_protocol() {
    use std::io::Read;

    let (client, client_end) = nix::unistd::pipe().unwrap();
    let (logs, logs_end) = nix::unistd::pipe().unwrap();
    nix::unistd::dup2_stdout(&client_end).unwrap();
    nix::unistd::dup2_stderr(&logs_end).unwrap();
    drop((client_end, logs_end));

    let mut protocol = take_stdout().unwrap();
    writeln!(io::stdout(), "stray").unwrap();
    io::stdout().flush().unwrap();
    protocol.write_all(b"{\"jsonrpc\":\"2.0\"}\n").unwrap();
    drop(protocol);
    let null = std::fs::File::open("/dev/null").unwrap();
    nix::unistd::dup2_stdout(&null).unwrap();
    nix::unistd::dup2_stderr(&null).unwrap();

    let mut sent = String::new();
    std::fs::File::from(client)
        .read_to_string(&mut sent)
        .unwrap();
    let mut logged = String::new();
    std::fs::File::from(logs)
        .read_to_string(&mut logged)
        .unwrap();
    assert_eq!(sent, "{\"jsonrpc\":\"2.0\"}\n");
    assert_eq!(logged, "stray\n");
}
