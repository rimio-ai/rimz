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
fn equivalent_uris_share_ownership_and_keep_wire_spellings() {
    let mut c = ready();
    let owner = "file:///a/b%40c/u.rs";
    let other = "file:///a/b@c/u.rs";
    for (id, uri) in [(A, owner), (B, other)] {
        let sent = server(&frame(
            &mut c,
            id,
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "languageId": "rust", "version": 1, "text": "same"}}),
        ));
        assert_eq!(sent.len(), usize::from(id == A));
        if id == A {
            assert_eq!(sent[0]["params"]["textDocument"]["uri"], owner);
        }
    }
    assert!(frame(&mut c, A, "textDocument/didOpen", json!({"textDocument": {"uri": other, "languageId": "rust", "version": 99, "text": "duplicate"}})).is_empty());
    let changed = server(&frame(
        &mut c,
        A,
        "textDocument/didChange",
        json!({"textDocument": {"uri": other, "version": 2}, "contentChanges": [{"text": "changed"}]}),
    ));
    assert_eq!(changed[0]["params"]["textDocument"]["uri"], owner);
    assert_eq!(changed[0]["params"]["contentChanges"][0]["text"], "changed");
    let saved = server(&frame(
        &mut c,
        A,
        "textDocument/didSave",
        json!({"textDocument": {"uri": other}}),
    ));
    assert_eq!(saved[0]["params"]["textDocument"]["uri"], owner);
    let attached = c.attached();
    assert_eq!(attached[0].open[0].uri, owner);
    assert_eq!(attached[1].open[0].uri, other);
    assert!(!attached[0].open[0].dirty);
    assert!(!attached[1].open[0].owner);
    let transfer = server(&frame(
        &mut c,
        A,
        "textDocument/didClose",
        json!({"textDocument": {"uri": other}}),
    ));
    assert_eq!(transfer.len(), 2);
    assert_eq!(transfer[0]["method"], "textDocument/didClose");
    assert_eq!(transfer[0]["params"]["textDocument"]["uri"], owner);
    assert_eq!(transfer[1]["method"], "textDocument/didOpen");
    assert_eq!(transfer[1]["params"]["textDocument"]["uri"], other);
}

#[test]
fn diagnostics_use_holder_spellings_on_every_delivery_path() {
    let mut c = ready();
    let owner = "file:///a/b%40c/u.rs";
    let other = "file:///a/b@c/u.rs";
    let published = "file:///a/b@c/%75.rs";
    frame(
        &mut c,
        A,
        "textDocument/didOpen",
        json!({"textDocument": {"uri": owner, "languageId": "rust", "version": 1, "text": "same"}}),
    );
    let publication = |version, empty| json!({"method": "textDocument/publishDiagnostics", "params": {"uri": published, "version": version, "diagnostics": if empty {json!([])} else {json!([{"message": "error"}])}}});
    let out = c.event(Event::ServerMessage(publication(1, false)));
    assert_eq!(recipient(&out, A)[0]["params"]["uri"], owner);
    assert_eq!(recipient(&out, C)[0]["params"]["uri"], published);
    let replay = frame(
        &mut c,
        B,
        "textDocument/didOpen",
        json!({"textDocument": {"uri": other, "languageId": "rust", "version": 8, "text": "same"}}),
    );
    assert_eq!(recipient(&replay, B)[0]["params"]["uri"], other);
    assert_eq!(recipient(&replay, B)[0]["params"]["version"], 8);
    let out = c.event(Event::ServerMessage(publication(1, false)));
    assert_eq!(recipient(&out, B)[0]["params"]["uri"], other);
    let mismatch = c.event(Event::ServerMessage(publication(99, false)));
    assert_eq!(recipient(&mismatch, A)[0]["params"]["uri"], owner);
    assert!(recipient(&mismatch, B).is_empty());
    let diverged = frame(
        &mut c,
        B,
        "textDocument/didChange",
        json!({"textDocument": {"uri": owner, "version": 9}, "contentChanges": [{"text": "different"}]}),
    );
    assert_eq!(recipient(&diverged, B)[0]["params"]["uri"], other);
    assert_eq!(
        recipient(&diverged, B)[0]["params"]["diagnostics"],
        json!([])
    );
    let clear = c.event(Event::ServerMessage(publication(1, true)));
    for (id, uri) in [(A, owner), (B, other), (C, published)] {
        assert_eq!(recipient(&clear, id)[0]["params"]["uri"], uri);
    }
    for uri in ["untitled:x", "not a uri"] {
        for id in [A, B] {
            let sent = server(&frame(
                &mut c,
                id,
                "textDocument/didOpen",
                json!({"textDocument": {"uri": uri, "languageId": "rust", "version": 1, "text": "same"}}),
            ));
            assert_eq!(sent.len(), usize::from(id == A));
        }
        let out = c.event(Event::ServerMessage(json!({"method": "textDocument/publishDiagnostics", "params": {"uri": uri, "version": 1, "diagnostics": [{"message": "error"}]}})));
        for id in [A, B] {
            assert_eq!(recipient(&out, id)[0]["params"]["uri"], uri);
        }
    }
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
    c.event(Event::ServerMessage(json!({"method":"$/progress","params":{"token":"editor-work","value":{"kind":"begin","title":"Untracked editor work"}}})));
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
fn non_rust_servers_omit_only_experimental_capabilities() {
    let mut config: crate::config::LspServerConfig = serde_json::from_value(
        json!({"command":["rust-analyzer"],"extensions":["py"],"root-markers":["pyproject.toml"]}),
    )
    .unwrap();
    let mut expected = super::super::client_capabilities(config.resolved_kind());
    expected.as_object_mut().unwrap().remove("experimental");
    for kind in [
        crate::config::LspServerKind::Pyright,
        crate::config::LspServerKind::Basedpyright,
        crate::config::LspServerKind::Ruff,
        crate::config::LspServerKind::Generic,
    ] {
        config.kind = Some(kind);
        assert_eq!(
            super::super::client_capabilities(config.resolved_kind()),
            expected
        );
    }
}

#[test]
fn capabilities_preserve_editor_actions_without_unsafe_superset_features() {
    let config: crate::config::LspServerConfig = serde_json::from_value(
        json!({"command": ["rust-analyzer"], "extensions": ["rs"], "root-markers": ["Cargo.toml"]}),
    )
    .unwrap();
    let caps = super::super::client_capabilities(config.resolved_kind());
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
    assert_eq!(caps["experimental"]["snippetTextEdit"], true);
    for absent in ["openServerLogs", "testExplorer", "localDocs"] {
        assert!(caps["experimental"].get(absent).is_none());
    }
    for method in [
        "definition",
        "typeDefinition",
        "implementation",
        "declaration",
    ] {
        assert_eq!(caps["textDocument"][method]["linkSupport"], true);
    }
    assert_eq!(
        caps["textDocument"]["completion"]["completionItem"]["snippetSupport"],
        true
    );
    assert!(caps["workspace"].get("applyEdit").is_none());
    assert!(caps["general"].get("positionEncodings").is_none());
}

fn reply(c: &mut Clients, id: ClientId, method: &str, result: Value) -> Value {
    c.event(Event::ClientFrame(
        id,
        json!({"id": 77, "method": method, "params": {}}),
    ));
    recipient(
        &c.event(Event::ServerReply(
            id,
            json!(77),
            json!({"id": 999, "result": result}),
        )),
        id,
    )[0]["result"]
        .clone()
}

#[test]
fn replies_adapt_links_per_method_and_editor() {
    let mut c = ready();
    let range = json!({"start":{"line":1,"character":2},"end":{"line":1,"character":3}});
    let link = json!({"targetUri":"file:///u", "targetRange":{}, "targetSelectionRange":range});
    for method in [
        "definition",
        "typeDefinition",
        "implementation",
        "declaration",
    ] {
        frame(
            &mut c,
            B,
            "initialize",
            json!({"capabilities":{"textDocument":{method:{"linkSupport":true}}}}),
        );
        let method = format!("textDocument/{method}");
        assert_eq!(
            reply(&mut c, A, &method, json!([link])),
            json!([{"uri":"file:///u", "range":range}])
        );
        assert_eq!(reply(&mut c, B, &method, json!([link])), json!([link]));
        assert_eq!(
            reply(&mut c, B, "textDocument/references", json!([link])),
            json!([link])
        );
    }
    // rust-analyzer's module navigation answers like a definition, keyed on its linkSupport.
    for method in ["experimental/parentModule", "experimental/childModules"] {
        frame(
            &mut c,
            B,
            "initialize",
            json!({"capabilities":{"textDocument":{"definition":{"linkSupport":true}}}}),
        );
        assert_eq!(
            reply(&mut c, A, method, json!([link])),
            json!([{"uri":"file:///u", "range":range}])
        );
        assert_eq!(
            reply(&mut c, A, method, link.clone()),
            json!({"uri":"file:///u", "range":range})
        );
        assert_eq!(reply(&mut c, A, method, Value::Null), Value::Null);
        assert_eq!(reply(&mut c, B, method, json!([link])), json!([link]));
    }
}

#[test]
fn replies_strip_snippets_in_completion_and_workspace_edits() {
    let mut c = ready();
    frame(
        &mut c,
        B,
        "initialize",
        json!({"capabilities":{"textDocument":{"completion":{"completionItem":{"snippetSupport":true}}},"experimental":{"snippetTextEdit":true}}}),
    );
    for (snippet, plain) in [
        ("foo(${1:x}, ${2:y})$0", "foo(x, y)"),
        ("$1suffix ${2:é}$0!", "suffix é!"),
        ("${1|a,b|}", "a"),
        (r"\$1", "$1"),
        ("${1:outer ${2:inner}}", "outer inner"),
        ("$NAME ${NAME:default} ${1} $9", " default  "),
        (r"${1|a\,b,c|} \} \\", "a,b } \\"),
    ] {
        let item = json!({"label":"item", "insertTextFormat":2, "insertText":snippet,"textEdit":{"newText":snippet,"range":{}}});
        let mut expected = item.clone();
        expected["insertTextFormat"] = json!(1);
        expected["insertText"] = json!(plain);
        expected["textEdit"]["newText"] = json!(plain);
        for (method, input, output) in [
            ("textDocument/completion", json!([item]), json!([expected])),
            (
                "textDocument/completion",
                json!({"isIncomplete":false,"items":[item]}),
                json!({"isIncomplete":false,"items":[expected]}),
            ),
            ("completionItem/resolve", item.clone(), expected),
        ] {
            assert_eq!(reply(&mut c, A, method, input.clone()), output);
            assert_eq!(reply(&mut c, B, method, input.clone()), input);
        }
        let edit = json!([{"edit":{"changes":{"file:///u":[{"newText":snippet,"insertTextFormat":2,"range":{}}]}}}]);
        let converted = reply(&mut c, A, "textDocument/codeAction", edit.clone());
        assert_eq!(
            converted[0]["edit"]["changes"]["file:///u"][0],
            json!({"newText":plain,"range":{}})
        );
        assert_eq!(
            reply(&mut c, B, "textDocument/codeAction", edit.clone()),
            edit
        );
    }
}

#[test]
fn requests_and_replies_map_held_uris_in_both_directions() {
    let mut c = ready();
    let owner = "file:///a/b%40c/u.rs";
    let other = "file:///a/b@c/u.rs";
    for (id, uri) in [(A, owner), (B, other)] {
        frame(
            &mut c,
            id,
            "textDocument/didOpen",
            json!({"textDocument":{"uri":uri,"languageId":"rust","version":1,"text":"same"}}),
        );
    }
    let request = json!({"id":8,"method":"textDocument/hover","params":{"textDocument":{"uri":other},"targetUri":other,"changes":{other:[]}}});
    let sent = server(&c.event(Event::ClientFrame(B, request)));
    assert_eq!(
        sent[0]["params"],
        json!({"textDocument":{"uri":owner},"targetUri":owner,"changes":{owner:[]}})
    );
    let result = reply(
        &mut c,
        B,
        "textDocument/definition",
        json!([{"uri":owner,"range":{}},{"uri":"file:///unheld","range":{}}]),
    );
    assert_eq!(
        result,
        json!([{"uri":other,"range":{}},{"uri":"file:///unheld","range":{}}])
    );
    assert_eq!(
        reply(
            &mut c,
            B,
            "workspace/executeCommand",
            json!({"changes":{owner:[]},"targetUri":owner})
        ),
        json!({"changes":{other:[]},"targetUri":other})
    );
}

#[test]
fn progress_replays_open_tokens_in_creation_order_and_resets() {
    for reset in ["end", "stop", "start", "none"] {
        let mut c = ready();
        attach(&mut c, C, false);
        let status = json!({"method":"experimental/serverStatus","params":{"health":"ok"}});
        c.event(Event::ServerMessage(status.clone()));
        for token in [json!(9), json!("index")] {
            let create =
                json!({"id":80,"method":"window/workDoneProgress/create","params":{"token":token}});
            let live = c.event(Event::ServerMessage(create));
            assert_eq!(recipient(&live, A).len(), 1);
            assert!(recipient(&live, C).is_empty());
            for (kind, percentage) in [("begin", 0), ("report", 10), ("report", 50)] {
                let progress = json!({"method":"$/progress","params":{"token":token,"value":{"kind":kind,"percentage":percentage}}});
                assert_eq!(
                    recipient(&c.event(Event::ServerMessage(progress.clone())), A),
                    vec![progress]
                );
            }
        }
        match reset {
            "end" => {
                for token in [json!(9), json!("index")] {
                    c.event(Event::ServerMessage(json!({"method":"$/progress","params":{"token":token,"value":{"kind":"end"}}})));
                }
            }
            "stop" => {
                c.event(Event::LifetimeEnded(StopReason::Idle));
            }
            // A server's first progress can reach the router before its lifetime's start event.
            "start" => {
                start(&mut c);
            }
            _ => {}
        }
        let actions = frame(&mut c, C, "initialized", json!({}));
        assert!(recipient(&actions, A).is_empty());
        let replay = recipient(&actions, C);
        let kept = matches!(reset, "none" | "start");
        assert_eq!(replay.len(), if kept { 7 } else { 1 }, "{reset}");
        if kept {
            assert_eq!(replay[0], status);
            for (offset, token) in [(1, json!(9)), (4, json!("index"))] {
                assert_eq!(replay[offset]["method"], "window/workDoneProgress/create");
                assert_eq!(replay[offset]["params"]["token"], token);
                assert_eq!(replay[offset + 1]["params"]["value"]["kind"], "begin");
                assert_eq!(replay[offset + 2]["params"]["value"]["percentage"], 50);
            }
            assert_ne!(replay[1]["id"], replay[4]["id"]);
        }
        assert!(frame(&mut c, C, "initialized", json!({})).is_empty());
    }
}
