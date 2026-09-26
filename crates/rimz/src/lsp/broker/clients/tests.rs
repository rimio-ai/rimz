use super::*;
use serde_json::json;

const A: ClientId = ClientId(1);
const B: ClientId = ClientId(2);
const C: ClientId = ClientId(3);

fn attach(c: &mut Clients, id: ClientId, ready: bool) {
    c.event(Event::Attach {
        id,
        pid: id.0 as u32,
        name: None,
        since_ms: 42,
    });
    if ready {
        frame(c, id, "initialized", json!({}));
    }
}
fn start(c: &mut Clients) -> Vec<Action> {
    c.event(Event::LifetimeStarted { initialize_result: json!({"capabilities": {"textDocumentSync": {"change": 2, "save": {"includeText": true}, "willSave": true}, "workspace": {"workspaceFolders": {"changeNotifications": true}}}, "serverInfo": {"name": "stub"}}) })
}
fn frame(c: &mut Clients, id: ClientId, method: &str, params: Value) -> Vec<Action> {
    c.event(Event::ClientFrame(
        id,
        json!({"jsonrpc": "2.0", "method": method, "params": params}),
    ))
}
fn open(c: &mut Clients, id: ClientId, text: &str, version: i64) -> Vec<Action> {
    frame(
        c,
        id,
        "textDocument/didOpen",
        json!({"textDocument": {"uri": "file:///u", "languageId": "rust", "version": version, "text": text}}),
    )
}
fn change(c: &mut Clients, id: ClientId, text: &str, version: i64) -> Vec<Action> {
    frame(
        c,
        id,
        "textDocument/didChange",
        json!({"textDocument": {"uri": "file:///u", "version": version}, "contentChanges": [{"text": text}]}),
    )
}
fn close(c: &mut Clients, id: ClientId) -> Vec<Action> {
    frame(
        c,
        id,
        "textDocument/didClose",
        json!({"textDocument": {"uri": "file:///u"}}),
    )
}
fn request(c: &mut Clients, id: ClientId, n: i64) -> Vec<Action> {
    c.event(Event::ClientFrame(
        id,
        json!({"jsonrpc": "2.0", "id": n, "method": "textDocument/hover", "params": {}}),
    ))
}
fn server(actions: &[Action]) -> Vec<Value> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::ToServer(_, v) => Some(v.clone()),
            _ => None,
        })
        .collect()
}
fn recipient(actions: &[Action], id: ClientId) -> Vec<Value> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::ToClient(c, v) if *c == id => Some(v.clone()),
            _ => None,
        })
        .collect()
}
fn ready() -> Clients {
    let mut c = Clients::new("rust".into());
    for id in [A, B, C] {
        attach(&mut c, id, true);
    }
    start(&mut c);
    c
}
fn diagnostics(c: &mut Clients, version: i64, empty: bool) -> Vec<Action> {
    c.event(Event::ServerMessage(json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {"uri": "file:///u", "version": version, "diagnostics": if empty {json!([])} else {json!([{"message": "error"}])}}})))
}

#[test]
fn replies_and_cancellation_keep_client_identity() {
    let mut c = ready();
    for id in [A, B] {
        let out = request(&mut c, id, 1);
        assert!(matches!(&out[0], Action::ToServer(client, v) if *client == id && v["id"] == 1));
    }
    let cancel = frame(&mut c, B, "$/cancelRequest", json!({"id": 1}));
    assert!(matches!(&cancel[0], Action::ToServer(id, _) if *id == B));
    let error = json!({"code": -32602, "message": "bad", "data": {"x": 1}});
    let out = c.event(Event::ServerReply(
        B,
        json!(1),
        json!({"id": 99, "error": error}),
    ));
    assert_eq!(recipient(&out, B), [json!({"id": 1, "error": error})]);
    assert!(recipient(&out, A).is_empty());
    assert_eq!(
        recipient(
            &c.event(Event::ServerReply(
                A,
                json!(1),
                json!({"id": 98, "result": 42})
            )),
            A
        )[0]["result"],
        42
    );
}

#[test]
fn ownership_transfers_in_open_order() {
    let mut c = ready();
    assert_eq!(
        server(&open(&mut c, A, "a", 1))[0]["method"],
        "textDocument/didOpen"
    );
    assert!(server(&open(&mut c, B, "b", 8)).is_empty());
    assert!(server(&change(&mut c, B, "bb", 9)).is_empty());
    let changed = server(&change(&mut c, A, "aa", 2));
    assert_eq!(changed.len(), 1);
    assert_eq!(
        changed[0]["params"]["contentChanges"],
        json!([{"text": "aa"}])
    );
    let transfer = server(&close(&mut c, A));
    assert_eq!(transfer.len(), 2);
    assert_eq!(transfer[0]["method"], "textDocument/didClose");
    assert_eq!(transfer[1]["params"]["textDocument"]["text"], "bb");
    assert_eq!(transfer[1]["params"]["textDocument"]["version"], 9);
    assert_eq!(
        server(&close(&mut c, B))[0]["method"],
        "textDocument/didClose"
    );
}

#[test]
fn detach_releases_documents_and_discards_replies() {
    let mut c = ready();
    open(&mut c, A, "a", 1);
    open(&mut c, B, "b", 2);
    request(&mut c, A, 1);
    let out = c.event(Event::Detach(A));
    assert_eq!(server(&out).len(), 2);
    assert_eq!(c.attached().len(), 2);
    assert!(
        c.event(Event::ServerReply(A, json!(1), json!({"result": 1})))
            .is_empty()
    );
}

#[test]
fn lifetime_replays_latest_text_before_queued_requests() {
    let mut c = Clients::new("rust".into());
    attach(&mut c, A, true);
    assert!(server(&open(&mut c, A, "old", 1)).is_empty());
    assert!(server(&change(&mut c, A, "new", 2)).is_empty());
    assert!(request(&mut c, A, 1).contains(&Action::RequestStart));
    let out = server(&start(&mut c));
    assert_eq!(out.len(), 2);
    assert_eq!(out[0]["params"]["textDocument"]["text"], "new");
    assert_eq!(out[1]["method"], "textDocument/hover");
}

#[test]
fn lifetime_end_and_refusal_cancel_requests_once() {
    let mut c = ready();
    request(&mut c, A, 1);
    request(&mut c, A, 2);
    let out = recipient(&c.event(Event::LifetimeEnded(StopReason::Idle)), A);
    assert_eq!(
        out.iter().filter(|v| v["error"]["code"] == -32802).count(),
        2
    );
    assert_eq!(
        out.iter()
            .filter(|v| v["method"] == "window/showMessage")
            .count(),
        1
    );
    assert!(out.iter().any(|v| v["params"]["health"] == "warning"));
    request(&mut c, A, 3);
    let shortfall = Shortfall {
        root: "/checkout".into(),
        server: "rust".into(),
        estimate_bytes: 1,
        available_bytes: 0,
        committed_bytes: 0,
        reserve_bytes: 0,
        holders: vec![],
    };
    let out = recipient(&c.event(Event::Refused(shortfall)), A);
    assert_eq!(
        out.iter().filter(|v| v["error"]["code"] == -32802).count(),
        1
    );
    assert!(out.iter().any(|v| {
        v["params"]["message"]
            .as_str()
            .is_some_and(|s| s.contains("memory short"))
    }));
}

#[test]
fn failed_start_answers_queued_requests_before_editor_readiness() {
    let mut c = Clients::new("rust".into());
    attach(&mut c, A, false);
    c.event(Event::ClientFrame(
        A,
        json!({"id": 1, "method": "initialize"}),
    ));
    request(&mut c, A, 2);
    let actions = c.event(Event::LifetimeEnded(StopReason::Crashed));
    let out = recipient(&actions, A);
    for id in [1, 2] {
        assert!(
            out.iter()
                .any(|v| v["id"] == id && v["error"]["code"] == -32802),
            "{out:?}"
        );
    }
    assert_eq!(
        out.iter()
            .filter(|v| v["method"] == "window/showMessage" && v["params"]["type"] == 2)
            .count(),
        1
    );
    assert!(
        out.iter()
            .any(|v| v["params"]["message"] == "language server rust stopped: crashed")
    );
    assert!(!actions.contains(&Action::RequestStart));
    assert!(recipient(&start(&mut c), A).is_empty());
}

#[test]
fn idle_stop_without_pending_requests_has_no_warning_popup() {
    let mut c = ready();
    let out = recipient(&c.event(Event::LifetimeEnded(StopReason::Idle)), A);
    assert!(!out.iter().any(|v| v["method"] == "window/showMessage"));
    assert!(
        out.iter()
            .any(|v| v["method"] == "experimental/serverStatus")
    );
    let out = recipient(&c.event(Event::LifetimeEnded(StopReason::Crashed)), A);
    assert!(!out.iter().any(|v| v["method"] == "window/showMessage"));
}

#[test]
fn refusal_answers_requests_before_editor_readiness() {
    let mut c = Clients::new("rust".into());
    attach(&mut c, A, false);
    attach(&mut c, B, true);
    c.event(Event::ClientFrame(
        A,
        json!({"id": 1, "method": "initialize"}),
    ));
    let actions = c.event(Event::Refused(Shortfall {
        root: "/checkout".into(),
        server: "rust".into(),
        estimate_bytes: 1,
        available_bytes: 0,
        committed_bytes: 0,
        reserve_bytes: 0,
        holders: vec![],
    }));
    let out = recipient(&actions, A);
    assert_eq!(
        out.iter()
            .filter(|v| v["id"] == 1 && v["error"]["code"] == -32802)
            .count(),
        1
    );
    assert_eq!(
        out.iter()
            .filter(|v| v["method"] == "window/showMessage" && v["params"]["type"] == 1)
            .count(),
        1
    );
    assert!(
        recipient(&actions, B)
            .iter()
            .all(|v| v["method"] != "window/showMessage")
    );
    assert!(!actions.contains(&Action::RequestStart));
    assert!(recipient(&start(&mut c), A).is_empty());
}

#[test]
fn initialize_normalizes_boolean_workspace_folders() {
    let mut c = Clients::new("rust".into());
    attach(&mut c, A, false);
    c.event(Event::LifetimeStarted {
        initialize_result: json!({"capabilities":{"workspace":{"workspaceFolders":true}}}),
    });
    let out = recipient(
        &c.event(Event::ClientFrame(A, json!({"id":1,"method":"initialize"}))),
        A,
    );
    assert_eq!(
        out[0]["result"]["capabilities"]["workspace"]["workspaceFolders"],
        json!({"supported":true,"changeNotifications":false})
    );
}

#[test]
fn readiness_gates_fanout_and_replays_latest_status_once() {
    let mut c = Clients::new("rust".into());
    attach(&mut c, A, true);
    attach(&mut c, B, false);
    for method in [
        "$/progress",
        "workspace/semanticTokens/refresh",
        "workspace/diagnostic/refresh",
    ] {
        let mut message = json!({"method": method, "params": {}});
        if method != "$/progress" {
            message["id"] = json!(42);
        }
        let out = c.event(Event::ServerMessage(message));
        assert_eq!(recipient(&out, A).len(), 1);
        assert!(recipient(&out, B).is_empty());
        assert!(
            c.event(Event::ClientFrame(A, json!({"id": 42, "result": null})))
                .is_empty()
        );
    }
    for health in ["warning", "ok"] {
        c.event(Event::ServerMessage(
            json!({"method": "experimental/serverStatus", "params": {"health": health}}),
        ));
    }
    let out = recipient(&frame(&mut c, B, "initialized", json!({})), B);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["params"]["health"], "ok");
    assert!(frame(&mut c, B, "initialized", json!({})).is_empty());
}

#[test]
fn diagnostics_follow_snapshot_text_and_recipient_versions() {
    let mut c = ready();
    open(&mut c, A, "a", 1);
    open(&mut c, B, "a", 8);
    let out = diagnostics(&mut c, 1, false);
    assert_eq!(recipient(&out, A)[0]["params"]["version"], 1);
    assert_eq!(recipient(&out, B)[0]["params"]["version"], 8);
    assert!(recipient(&out, C)[0]["params"].get("version").is_none());
    let clear = recipient(&change(&mut c, B, "b", 9), B);
    assert_eq!(clear[0]["params"]["diagnostics"], json!([]));
    assert!(recipient(&change(&mut c, B, "bb", 10), B).is_empty());
    assert!(recipient(&diagnostics(&mut c, 1, false), B).is_empty());
    assert_eq!(
        recipient(&open(&mut c, C, "a", 20), C)[0]["params"]["version"],
        20
    );
    assert!(recipient(&change(&mut c, A, "a2", 2), A).is_empty());
}

#[test]
fn diagnostics_invalidate_on_transfer_change_lifetimes_and_last_close() {
    for case in 0..5 {
        let mut c = ready();
        open(&mut c, A, "a", 1);
        assert_eq!(recipient(&diagnostics(&mut c, 1, false), A).len(), 1);
        match case {
            0 => {
                open(&mut c, B, "b", 2);
                close(&mut c, A);
            }
            1 => {
                change(&mut c, A, "b", 2);
            }
            2 => {
                c.event(Event::LifetimeEnded(StopReason::Idle));
            }
            3 => {
                start(&mut c);
            }
            _ => {
                close(&mut c, A);
                open(&mut c, A, "a", 1);
            }
        }
        assert!(
            recipient(&open(&mut c, C, if case == 0 { "b" } else { "a" }, 8), C).is_empty(),
            "case {case}"
        );
    }
}

#[test]
fn lagging_diagnostics_are_owner_only_and_empty_publications_clear_everyone() {
    let mut c = ready();
    open(&mut c, A, "a", 2);
    open(&mut c, B, "a", 8);
    let old = diagnostics(&mut c, 1, false);
    assert_eq!(recipient(&old, A).len(), 1);
    assert!(recipient(&old, B).is_empty());
    assert!(recipient(&open(&mut c, C, "a", 9), C).is_empty());
    diagnostics(&mut c, 2, false);
    let empty = diagnostics(&mut c, 2, true);
    for id in [A, B, C] {
        assert_eq!(recipient(&empty, id)[0]["params"]["diagnostics"], json!([]));
    }
    close(&mut c, C);
    assert!(recipient(&open(&mut c, C, "a", 10), C).is_empty());
}

#[test]
fn dirty_is_per_holder_and_only_flips_publish() {
    let mut c = ready();
    open(&mut c, A, "a", 1);
    open(&mut c, B, "b", 1);
    assert!(change(&mut c, B, "bb", 2).contains(&Action::Publish));
    assert!(!change(&mut c, B, "bbb", 3).contains(&Action::Publish));
    close(&mut c, A);
    let attached = c.attached();
    let b = attached.iter().find(|e| e.pid == 2).unwrap();
    assert!(b.open[0].dirty && b.open[0].owner);
    assert_eq!(b.since_ms, 42);
    let saved = frame(
        &mut c,
        B,
        "textDocument/didSave",
        json!({"textDocument": {"uri": "file:///u"}}),
    );
    assert!(saved.contains(&Action::Publish));
    assert_eq!(server(&saved).len(), 1);
    assert!(!c.attached().iter().find(|e| e.pid == 2).unwrap().open[0].dirty);
}

#[test]
fn initialize_is_cached_shutdown_and_exit_are_local() {
    let mut c = Clients::new("rust".into());
    attach(&mut c, A, false);
    let init =
        json!({"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "editor"}}});
    assert!(
        c.event(Event::ClientFrame(A, init.clone()))
            .contains(&Action::RequestStart)
    );
    let out = recipient(&start(&mut c), A);
    assert_eq!(
        out[0]["result"]["capabilities"]["textDocumentSync"]["change"],
        1
    );
    assert_eq!(
        out[0]["result"]["capabilities"]["workspace"]["workspaceFolders"]["changeNotifications"],
        false
    );
    assert_eq!(c.attached()[0].name.as_deref(), Some("editor"));
    c.event(Event::LifetimeEnded(StopReason::Idle));
    attach(&mut c, B, false);
    assert_eq!(recipient(&c.event(Event::ClientFrame(B, init)), B).len(), 1);
    assert!(server(&frame(&mut c, A, "initialized", json!({}))).is_empty());
    let out = c.event(Event::ClientFrame(
        A,
        json!({"id": 2, "method": "shutdown"}),
    ));
    assert_eq!(recipient(&out, A)[0]["result"], Value::Null);
    assert!(!out.contains(&Action::RequestStart));
    assert!(!request(&mut c, A, 4).contains(&Action::RequestStart));
    assert_eq!(
        recipient(&request(&mut c, A, 3), A)[0]["error"]["code"],
        -32600
    );
    assert!(frame(&mut c, A, "exit", Value::Null).contains(&Action::Detach(A)));
}

#[test]
fn pending_queues_are_bounded_and_queued_cancellation_is_local() {
    let mut c = Clients::new("rust".into());
    attach(&mut c, A, false);
    for n in 0..256 {
        assert!(recipient(&request(&mut c, A, n), A).is_empty());
    }
    assert_eq!(
        recipient(&request(&mut c, A, 256), A)[0]["error"]["code"],
        -32803
    );
    assert_eq!(
        recipient(&frame(&mut c, A, "$/cancelRequest", json!({"id": 3})), A)[0]["error"]["code"],
        -32800
    );
    assert_eq!(server(&start(&mut c)).len(), 255);
}

#[test]
fn capabilities_preserve_editor_actions_without_unsafe_superset_features() {
    let config = serde_json::from_value(
        json!({"command": ["rust-analyzer"], "extensions": ["rs"], "root-markers": ["Cargo.toml"]}),
    )
    .unwrap();
    let caps = super::super::client_capabilities(&config);
    assert_eq!(
        caps["experimental"]["commands"]["commands"],
        json!([
            "rust-analyzer.runSingle",
            "rust-analyzer.debugSingle",
            "rust-analyzer.showReferences",
            "rust-analyzer.gotoLocation",
            "rust-analyzer.triggerParameterHints",
            "rust-analyzer.rename"
        ])
    );
    assert_eq!(caps["experimental"]["hoverActions"], true);
    assert_eq!(caps["experimental"]["codeActionGroup"], true);
    for absent in ["snippetTextEdit", "openServerLogs", "testExplorer"] {
        assert!(caps["experimental"].get(absent).is_none());
    }
    assert!(!caps.to_string().contains("linkSupport"));
    assert!(caps["workspace"].get("applyEdit").is_none());
    assert!(caps["general"].get("positionEncodings").is_none());
}
