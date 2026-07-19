//! `codex app-server`-shaped JSON-RPC stub for the hooks integration tests.
//!
//! Reads newline-delimited JSON-RPC requests on stdin and replies with canned
//! results so the Codex context refresh (`rimz agents refresh-context`) can be
//! exercised without the real `codex` binary. Tests point `RIMZ_CODEX_BIN` at
//! this binary; the client spawns it as `<bin> app-server` (argv ignored).
//! Notifications (no `id`) get no reply; the process exits on stdin EOF.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        // Only requests (with an `id`) get a reply; `initialized` is a
        // notification and carries none.
        let Some(id) = value.get("id") else { continue };
        let method = value.get("method").and_then(Value::as_str).unwrap_or("");
        let frame = json!({ "id": id, "result": response_for(method) });
        // Exit promptly when the client closes the pipe rather than spinning on
        // stdin until EOF with every write silently dropped.
        if writeln!(stdout, "{frame}").is_err() || stdout.flush().is_err() {
            break;
        }
    }
}

fn response_for(method: &str) -> Value {
    match method {
        "initialize" => json!({
            "userAgent": "rimz/9.9.9 (Test 1.0; x86_64)",
            "codexHome": "/tmp/.codex",
            "platformFamily": "unix",
            "platformOs": "linux"
        }),
        "account/rateLimits/read" => json!({
            "rateLimits": {
                "limitId": "codex",
                "primary": { "usedPercent": 42, "windowDurationMins": 300, "resetsAt": 1_790_000_000_i64 },
                "secondary": { "usedPercent": 7, "windowDurationMins": 10080, "resetsAt": 1_790_500_000_i64 },
                "credits": { "balance": 18.5 },
                "planType": "team"
            }
        }),
        "model/list" => json!({
            "data": [
                { "id": "gpt-5.5-codex", "model": "gpt-5.5-codex", "displayName": "GPT-5.5 Codex",
                  "defaultReasoningEffort": "high", "isDefault": true, "description": "",
                  "hidden": false, "supportedReasoningEfforts": [] }
            ]
        }),
        _ => json!({}),
    }
}
