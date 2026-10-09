//! Shared language-server configuration and machine registry boundaries.

use crate::common::Env;
use assert_cmd::assert::OutputAssertExt;
use rimz::config::{MachineConfig, effective};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

fn editor_broker(env: &Env, idle: &str) -> (std::process::Child, std::path::PathBuf) {
    editor_broker_modes(env, idle, &[])
}

fn editor_broker_modes(
    env: &Env,
    idle: &str,
    modes: &[&str],
) -> (std::process::Child, std::path::PathBuf) {
    let stub = crate::common::cargo_bin("lsp-server-stub", env!("CARGO_BIN_EXE_lsp-server-stub"));
    let mut command = vec![stub.to_string_lossy().into_owned()];
    command.extend(modes.iter().map(|mode| (*mode).to_owned()));
    let config = serde_json::from_value(json!({"command":command,"extensions":["rs"],"root-markers":["Cargo.toml"],"memory-estimate":"1M"})).unwrap();
    let mut machine = MachineConfig::default();
    machine.lsp.servers.insert("rust".into(), config);
    std::fs::write(
        env.rimz_home().join("config.toml"),
        toml::to_string(&std::collections::BTreeMap::from([("lsp", &machine.lsp)])).unwrap(),
    )
    .unwrap();
    let request = rimz::lsp::admission::ServeRequest {
        root: env.project_root.canonicalize().unwrap(),
        project: env.project_root.clone(),
        server: "rust".into(),
        settings_hash: "editor-test".into(),
        config: machine.lsp.servers.remove("rust").unwrap(),
        policy: rimz::config::LspConfig {
            idle_timeout: idle.into(),
            kill_floor_percent: 0,
            reserve_percent: 0,
            reserve_min: "0".into(),
            ..Default::default()
        },
        eager: false,
    };
    spawn_test_broker(env, &request)
}

fn editor_rpc(directory: &Path, value: Value) -> Value {
    let mut stream = UnixStream::connect(directory.join("sock")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    writeln!(stream, "{value}").unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

fn editor_query(directory: &Path, name: &str) -> Value {
    editor_rpc(
        directory,
        json!({"op":"query","method":"workspace/symbol","params":{"query":name},"wait_ms":4000}),
    )["result"]
        .clone()
}

#[test]
fn lsp_status_and_stop_select_one_lazy_server() {
    let env = Env::new();
    let root = env.project_root.canonicalize().unwrap();
    let mut brokers = Vec::new();
    for server in ["python", "rust"] {
        let request = rimz::lsp::admission::ServeRequest {
            root: root.clone(),
            project: env.project_root.clone(),
            server: server.into(),
            settings_hash: "selection-test".into(),
            config: serde_json::from_value(json!({
                "command": [crate::common::cargo_bin("lsp-server-stub", env!("CARGO_BIN_EXE_lsp-server-stub"))],
                "extensions": ["rs"], "root-markers": ["Cargo.toml"], "memory-estimate": "1M"
            })).unwrap(),
            policy: rimz::config::LspConfig {
                kill_floor_percent: 0,
                reserve_percent: 0,
                reserve_min: "0".into(),
                ..Default::default()
            },
            eager: false,
        };
        brokers.push(spawn_test_broker(&env, &request));
    }
    for verb in ["status", "stop"] {
        env.rimz()
            .args(["lsp", verb])
            .assert()
            .failure()
            .stderr(predicates::str::contains(
                "multiple language servers; choose --server NAME",
            ));
    }
    env.rimz()
        .args(["lsp", "status", "--server", "python"])
        .assert()
        .success()
        .stdout(predicates::str::contains("not started"));
    env.rimz()
        .args(["lsp", "status", "--server", "go"])
        .assert()
        .code(3)
        .stderr(predicates::str::contains("not running"));
    env.rimz()
        .args(["lsp", "stop", "--server", "python"])
        .assert()
        .success()
        .stdout(format!("stopped python ({})\n", root.display()));
    for (mut broker, directory) in brokers {
        assert_eq!(
            editor_rpc(
                &directory,
                json!({"op":"stop", "reason":"checkout removed"})
            )["ok"],
            true
        );
        broker.wait().unwrap();
    }
}

#[test]
fn lsp_server_arms_deliver_settings_capabilities_and_status() {
    for (kind, options, sections, answers) in [
        (
            "ty",
            json!({"diagnosticMode":"openFilesOnly"}),
            vec!["ty"],
            json!([null]),
        ),
        (
            "pyright",
            json!({"python":{"analysis":{"typeCheckingMode":"strict"}},"pyright":{"disableOrganizeImports":true}}),
            vec!["python", "pyright", "python.analysis", "absent"],
            json!([{"analysis":{"typeCheckingMode":"strict"}},{"disableOrganizeImports":true},{"typeCheckingMode":"strict"},null]),
        ),
        (
            "basedpyright",
            json!({"python":{"pythonPath":"python3"},"basedpyright":{"analysis":{"typeCheckingMode":"strict"}},"pyright":{"disableOrganizeImports":true}}),
            vec!["python", "basedpyright", "pyright"],
            json!([{"pythonPath":"python3"},{"analysis":{"typeCheckingMode":"strict"}},{"disableOrganizeImports":true}]),
        ),
        (
            "ruff",
            json!({"settings":{"lineLength":100}}),
            vec!["settings"],
            json!([{"lineLength":100}]),
        ),
        (
            "rust-analyzer",
            json!({"checkOnSave":false}),
            vec!["rust-analyzer", "rust-analyzer.checkOnSave", "absent"],
            json!([{"checkOnSave":false},false,null]),
        ),
    ] {
        let env = Env::new();
        let mut command = vec![
            crate::common::cargo_bin("lsp-server-stub", env!("CARGO_BIN_EXE_lsp-server-stub"))
                .to_string_lossy()
                .into_owned(),
        ];
        for section in sections {
            command.extend(["--configuration-section".into(), section.into()]);
        }
        let request = rimz::lsp::admission::ServeRequest {
            root: env.project_root.canonicalize().unwrap(), project: env.project_root.clone(), server: "python".into(), settings_hash: "arms-test".into(),
            config: serde_json::from_value(json!({"kind":kind,"command":command,"extensions":["py"],"root-markers":["pyproject.toml"],"init-options":options,"memory-estimate":"1M"})).unwrap(),
            policy: rimz::config::LspConfig { kill_floor_percent: 0, reserve_percent: 0, reserve_min: "0".into(), ..Default::default() }, eager: false,
        };
        let (mut broker, directory) = spawn_test_broker(&env, &request);
        let _editor = Editor::attach(&directory, "arms-test");
        let deadline = Instant::now() + Duration::from_secs(5);
        let requests = loop {
            let result = editor_query(&directory, "requests");
            if result["requests"]
                .as_array()
                .is_some_and(|frames| frames.iter().any(|f| f["id"] == "configuration"))
            {
                break result["requests"].as_array().unwrap().clone();
            }
            assert!(
                Instant::now() < deadline,
                "configuration response missing: {result}"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        let initialized = requests
            .iter()
            .find(|f| f["method"] == "initialize")
            .unwrap();
        assert_eq!(initialized["params"]["initializationOptions"], options);
        let experimental = &initialized["params"]["capabilities"]["experimental"];
        if kind == "rust-analyzer" {
            assert_eq!(
                experimental["commands"]["commands"],
                json!([
                    "rust-analyzer.runSingle",
                    "rust-analyzer.debugSingle",
                    "rust-analyzer.showReferences",
                    "rust-analyzer.gotoLocation",
                    "rust-analyzer.triggerParameterHints",
                    "rust-analyzer.rename"
                ])
            );
        } else {
            assert!(experimental.is_null(), "{kind}: {experimental}");
        }
        assert_eq!(
            requests
                .iter()
                .find(|f| f["id"] == "configuration")
                .unwrap()["result"],
            answers
        );
        let output = env
            .rimz()
            .args(["lsp", "status", "--server", "python", "--json"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap()["kind"],
            kind
        );
        assert_eq!(
            editor_rpc(&directory, json!({"op":"stop","reason":"checkout removed"}))["ok"],
            true
        );
        broker.wait().unwrap();
    }
}

#[test]
fn lsp_editor_check_on_save_tracks_first_attach_last_detach_and_restart() {
    let env = Env::new();
    let stub = crate::common::cargo_bin("lsp-server-stub", env!("CARGO_BIN_EXE_lsp-server-stub"));
    let request = rimz::lsp::admission::ServeRequest {
        root: env.project_root.canonicalize().unwrap(),
        project: env.project_root.clone(),
        server: "rust".into(),
        settings_hash: "editor-checks".into(),
        config: serde_json::from_value(json!({"kind":"rust-analyzer","command":[stub,"--reload-configuration"],"extensions":["rs"],"root-markers":["Cargo.toml"],"init-options":{"checkOnSave":false,"cargo":{"targetDir":true}},"editor-check-on-save":true,"memory-estimate":"1M"})).unwrap(),
        policy: rimz::config::LspConfig { kill_floor_percent: 0, reserve_percent: 0, reserve_min: "0".into(), ..Default::default() },
        eager: false,
    };
    let (mut broker, directory) = spawn_test_broker(&env, &request);
    let pid = std::process::id();
    assert_eq!(
        editor_rpc(
            &directory,
            json!({"op":"lease","launch_id":"agent","pid":pid,"start_token":rimz::proc::process_start_token(pid).unwrap()})
        )["ok"],
        true
    );
    let initialized_options = |requests: &Value| {
        requests["requests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["method"] == "initialize")
            .unwrap()["params"]["initializationOptions"]
            .clone()
    };
    assert_eq!(
        initialized_options(&editor_query(&directory, "requests")),
        json!({"checkOnSave":false,"cargo":{"targetDir":true}})
    );
    let wait_settings = |count: usize, enabled: bool| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let result = editor_query(&directory, "requests");
            let answers: Vec<_> = result["requests"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|f| {
                    f["id"]
                        .as_str()
                        .is_some_and(|id| id.starts_with("configuration-change:"))
                })
                .collect();
            if answers.len() == count {
                assert_eq!(
                    answers.last().unwrap()["result"],
                    json!([{"checkOnSave":enabled,"cargo":{"targetDir":true}}])
                );
                let changes: Vec<_> = result["requests"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|f| f["method"] == "workspace/didChangeConfiguration")
                    .collect();
                assert_eq!(changes.len(), count);
                assert!(
                    changes
                        .iter()
                        .all(|f| f["params"] == json!({"settings":null}))
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "settings transition missing: {result}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let a = Editor::attach(&directory, "first");
    wait_settings(1, true);
    let b = Editor::attach(&directory, "second");
    wait_settings(1, true);
    drop(a);
    let deadline = Instant::now() + Duration::from_secs(5);
    while editor_rpc(&directory, json!({"op":"status"}))["attached"]
        .as_array()
        .unwrap()
        .len()
        != 1
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    wait_settings(1, true);
    drop(b);
    wait_settings(2, false);
    let status = editor_rpc(&directory, json!({"op":"status"}));
    assert_eq!(status["editor_check_on_save"], false);
    assert_eq!(status["leases"].as_array().unwrap().len(), 1);
    let _editor = Editor::attach(&directory, "restart");
    wait_settings(3, true);
    env.rimz()
        .args(["lsp", "status", "--server", "rust", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"editor_check_on_save\": true"));
    env.rimz()
        .args(["lsp", "status", "--server", "rust"])
        .assert()
        .success()
        .stdout(predicates::str::contains("on (editor attached)"));
    assert_eq!(
        editor_rpc(&directory, json!({"op":"stop","reason":"stopped by hand"}))["ok"],
        true
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = editor_rpc(&directory, json!({"op":"status"}));
        if status["state"].get("dormant").is_some() && status["server_pid"].is_null() {
            break;
        }
        assert!(Instant::now() < deadline, "not dormant: {status}");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        initialized_options(&editor_query(&directory, "requests")),
        json!({"checkOnSave":true,"cargo":{"targetDir":true}})
    );
    assert_eq!(
        editor_rpc(&directory, json!({"op":"stop","reason":"checkout removed"}))["ok"],
        true
    );
    broker.wait().unwrap();
}

struct Editor(BufReader<UnixStream>);

impl Editor {
    fn attach(directory: &Path, name: &str) -> Self {
        Self::attach_pid(directory, name, std::process::id())
    }

    fn attach_pid(directory: &Path, name: &str, pid: u32) -> Self {
        Self::attach_capabilities(directory, name, pid, json!({}))
    }

    fn attach_capabilities(directory: &Path, name: &str, pid: u32, capabilities: Value) -> Self {
        let mut stream = UnixStream::connect(directory.join("sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        // Pipeline the first frame to prove the handshake preserves buffered bytes.
        let mut bytes = format!("{}\n", json!({"op":"attach","pid":pid,"start_token":rimz::proc::process_start_token(pid).unwrap()})).into_bytes();
        rimz::lsp::protocol::write_frame(&mut bytes, &json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"clientInfo":{"name":name},"capabilities":capabilities}})).unwrap();
        stream.write_all(&bytes).unwrap();
        let mut editor = Self(BufReader::new(stream));
        let mut line = String::new();
        editor.0.read_line(&mut line).unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["ok"], true, "attach refused: {response}");
        assert!(response["client_id"].is_u64());
        let initialized = rimz::lsp::protocol::read_frame(&mut editor.0).unwrap();
        assert_eq!(
            initialized["id"], 0,
            "nothing is broadcast before initialized: {initialized}"
        );
        assert_eq!(
            initialized["result"]["capabilities"]["textDocumentSync"]["change"],
            1
        );
        editor
    }

    fn send(&mut self, frame: Value) {
        rimz::lsp::protocol::write_frame(self.0.get_mut(), &frame).unwrap();
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc":"2.0","method":method,"params":params}));
    }

    fn until(&mut self, key: &str, value: Value) -> Value {
        loop {
            let frame = rimz::lsp::protocol::read_frame(&mut self.0).unwrap();
            if frame[key] == value {
                return frame;
            }
        }
    }

    fn ready(&mut self) {
        self.notify("initialized", json!({}));
        assert_eq!(
            self.until("method", json!("experimental/serverStatus"))["params"]["health"],
            "ok"
        );
    }

    fn open(&mut self, uri: &str, version: i64, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument":{"uri":uri,"languageId":"rust","version":version,"text":text}}),
        );
    }

    fn diagnostic(&mut self, version: i64) {
        let frame = loop {
            let frame = self.until("method", json!("textDocument/publishDiagnostics"));
            if frame["params"]["diagnostics"] != json!([]) {
                break frame;
            }
        };
        assert_eq!(frame["params"]["version"], version, "{frame}");
        assert_eq!(frame["params"]["diagnostics"].as_array().unwrap().len(), 1);
    }

    fn quiet(&mut self) {
        self.0
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(150)))
            .unwrap();
        assert!(rimz::lsp::protocol::read_frame(&mut self.0).is_err());
        self.0
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
    }
}

#[test]
fn lsp_editors_receive_negotiated_shapes_and_agents_keep_links() {
    let env = Env::new();
    let (mut broker, directory) = editor_broker_modes(&env, "10m", &["--adaptable-replies"]);
    let path = env.project_root.join("lib.rs");
    std::fs::write(&path, "fn example() {}\n").unwrap();
    let uri = url::Url::from_file_path(&path).unwrap().to_string();
    let mut a = Editor::attach(&directory, "minimal");
    let mut b = Editor::attach_capabilities(
        &directory,
        "rich",
        std::process::id(),
        json!({"textDocument":{"definition":{"linkSupport":true},"completion":{"completionItem":{"snippetSupport":true}}}}),
    );
    a.ready();
    b.ready();
    for (editor, rich) in [(&mut a, false), (&mut b, true)] {
        editor.send(json!({"id":1,"method":"textDocument/definition","params":{"textDocument":{"uri":uri},"position":{"line":0,"character":3}}}));
        let result = editor.until("id", json!(1))["result"].clone();
        assert_eq!(result[0][if rich { "targetUri" } else { "uri" }], uri);
        assert!(
            result[0]
                .get(if rich { "uri" } else { "targetUri" })
                .is_none()
        );
        editor.send(json!({"id":2,"method":"textDocument/completion","params":{"textDocument":{"uri":uri},"position":{"line":0,"character":3}}}));
        let item = editor.until("id", json!(2))["result"][0].clone();
        assert_eq!(item["insertTextFormat"], if rich { 2 } else { 1 });
        assert_eq!(
            item["insertText"],
            if rich { "foo(${1:x})$0" } else { "foo(x)" }
        );
    }
    let output = env
        .rimz()
        .args(["lsp", "def", "lib.rs:1:4", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let locations: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        locations,
        json!([{"targetUri":uri,"targetRange":{"start":{"line":0,"character":0},"end":{"line":2,"character":1}},"targetSelectionRange":{"start":{"line":0,"character":0},"end":{"line":0,"character":3}}}])
    );
    env.rimz()
        .args(["lsp", "def", "lib.rs:1:4"])
        .assert()
        .success()
        .stdout("lib.rs:1:1 (1-3)  fn example() {}\n");
    editor_rpc(&directory, json!({"op":"stop","reason":"checkout removed"}));
    broker.wait().unwrap();
}

#[test]
fn lsp_late_editor_receives_open_index_progress() {
    let env = Env::new();
    let (mut broker, directory) = editor_broker_modes(&env, "10m", &["--hold-index-progress"]);
    let mut a = Editor::attach(&directory, "first");
    // The stub's status follows its create/begin/report on the same ordered path, so A seeing it
    // proves the router recorded them; a request reply returns on a separate path and can overtake.
    a.ready();
    let mut b = Editor::attach(&directory, "late");
    b.ready();
    let create = rimz::lsp::protocol::read_frame(&mut b.0).unwrap();
    assert_eq!(create["method"], "window/workDoneProgress/create");
    assert_eq!(create["params"]["token"], "index");
    for kind in ["begin", "report"] {
        let progress = rimz::lsp::protocol::read_frame(&mut b.0).unwrap();
        assert_eq!(progress["method"], "$/progress");
        assert_eq!(progress["params"]["value"]["kind"], kind);
    }
    b.send(json!({"id":2,"method":"workspace/symbol","params":{"query":"release-index"}}));
    // The end notification and the reply reach the router on separate paths, in either order.
    let (mut ended, mut replied) = (false, false);
    while !(ended && replied) {
        let frame = rimz::lsp::protocol::read_frame(&mut b.0).unwrap();
        ended |= frame["method"] == "$/progress" && frame["params"]["value"]["kind"] == "end";
        replied |= frame["id"] == 2;
    }
    b.quiet();
    editor_rpc(&directory, json!({"op":"stop","reason":"checkout removed"}));
    broker.wait().unwrap();
}

#[test]
fn lsp_attached_editors_share_buffers_and_survive_editor_shutdown() {
    let env = Env::new();
    let (mut broker, directory) = editor_broker(&env, "10m");
    let uri = url::Url::from_file_path(env.project_root.join("lib.rs"))
        .unwrap()
        .to_string();
    std::fs::write(env.project_root.join("lib.rs"), "fn disk() {}\n").unwrap();
    let mut a = Editor::attach(&directory, "A");
    a.ready();
    let mut owner = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let extra = Editor::attach_pid(&directory, "distinct lease", owner.id());
    let mut extras = vec![extra];
    for _ in 0..6 {
        extras.push(Editor::attach(&directory, "limit"));
    }
    let refusal = editor_rpc(
        &directory,
        json!({"op":"attach","pid":std::process::id(),"start_token":rimz::proc::process_start_token(std::process::id()).unwrap()}),
    );
    assert_eq!(refusal["error"]["code"], -32003);
    assert_eq!(refusal["error"]["message"], "editor limit reached");
    let leases = editor_rpc(&directory, json!({"op":"status"}))["leases"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(leases.len(), 2);
    drop(extras);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let status = editor_rpc(&directory, json!({"op":"status"}));
        if status["attached"].as_array().unwrap().len() == 1 {
            let expected: Vec<_> = leases
                .iter()
                .filter(|lease| lease["pid"] != owner.id())
                .cloned()
                .collect();
            assert_eq!(status["leases"], json!(expected));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "distinct editor lease not released: {status}"
        );
    }
    owner.kill().unwrap();
    owner.wait().unwrap();
    a.send(json!({"jsonrpc":"2.0","id":777,"method":"textDocument/hover","params":{"hold":true}}));
    a.notify("$/cancelRequest", json!({"id":777}));
    // The reply on this connection proves the preceding cancel reached the server.
    a.send(
        json!({"jsonrpc":"2.0","id":778,"method":"workspace/symbol","params":{"query":"requests"}}),
    );
    let trace = a.until("id", json!(778))["result"].clone();
    let held = trace["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["params"]["hold"] == true)
        .unwrap();
    assert_ne!(held["id"], 777);
    assert_eq!(trace["cancellations"], json!([held["id"]]));
    // Editor readiness precedes broker indexing readiness; agent queries wait for both.
    editor_query(&directory, "requests");
    std::fs::remove_file(directory.join("entry.json")).unwrap();
    std::fs::create_dir(directory.join("entry.json")).unwrap();
    let attached = Editor::attach(&directory, "publication failure");
    let status = editor_rpc(&directory, json!({"op":"status"}));
    assert_eq!(status["state"], "ready");
    assert_eq!(status["attached"].as_array().unwrap().len(), 2);
    std::fs::remove_dir(directory.join("entry.json")).unwrap();
    drop(attached);
    a.open(&uri, 1, "fn opened() {}\n");
    a.diagnostic(1);
    assert_eq!(
        editor_query(&directory, "open")[0]["text"],
        "fn opened() {}\n"
    );
    let status = || -> Value {
        let output = env
            .rimz()
            .args(["lsp", "status", "--json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let before = status();
    assert_eq!(before["attached"][0]["pid"], std::process::id());
    assert_eq!(before["attached"][0]["open"][0]["uri"], uri);
    assert_eq!(before["attached"][0]["open"][0]["owner"], true);
    let mut c = Editor::attach(&directory, "C");
    c.ready();
    c.open(&uri, 20, "fn opened() {}\n");
    c.diagnostic(20);
    c.notify("textDocument/didClose", json!({"textDocument":{"uri":uri}}));
    drop(c);
    a.notify("textDocument/didChange", json!({"textDocument":{"uri":uri,"version":2},"contentChanges":[{"text":"fn changed() {}\n"}]}));
    a.diagnostic(2);
    assert_eq!(status()["attached"][0]["open"][0]["dirty"], true);
    env.rimz()
        .args(["lsp", "def", "opened"])
        .assert()
        .success()
        .stdout("lib.rs:1:1  (unsaved in editor)\n");
    let mut b = Editor::attach(&directory, "B");
    b.open(&uri, 5, "fn second() {}\n");
    b.quiet();
    b.ready();
    b.quiet();
    assert_eq!(
        editor_query(&directory, "open")[0]["text"],
        "fn changed() {}\n"
    );
    let holders = status()["attached"].as_array().unwrap().clone();
    assert_eq!(holders.len(), 2);
    assert!(holders.iter().all(|editor| editor["open"][0]["uri"] == uri));
    assert_eq!(holders[1]["open"][0]["owner"], false);
    editor_query(&directory, "hold-diagnostics on");
    a.notify("textDocument/didClose", json!({"textDocument":{"uri":uri}}));
    // A reply on A's stream is an ordering barrier for the ownership transfer.
    a.send(json!({"jsonrpc":"2.0","id":9,"method":"textDocument/hover","params":{}}));
    a.until("id", json!(9));
    let mut c = Editor::attach(&directory, "C2");
    c.ready();
    c.open(&uri, 30, "fn second() {}\n");
    c.send(json!({"jsonrpc":"2.0","id":9,"method":"textDocument/hover","params":{}}));
    loop {
        let frame = rimz::lsp::protocol::read_frame(&mut c.0).unwrap();
        assert_ne!(
            frame["method"], "textDocument/publishDiagnostics",
            "the transferred snapshot must not replay: {frame}"
        );
        if frame["id"] == 9 {
            break;
        }
    }
    c.quiet();
    editor_query(&directory, "hold-diagnostics off");
    c.diagnostic(30);
    c.notify("textDocument/didClose", json!({"textDocument":{"uri":uri}}));
    drop(c);
    for (editor, marker) in [(&mut a, "A reply"), (&mut b, "B reply")] {
        editor.send(json!({"jsonrpc":"2.0","id":1,"method":"textDocument/hover","params":{"marker":marker}}));
    }
    for (editor, marker) in [(&mut a, "A reply"), (&mut b, "B reply")] {
        assert_eq!(editor.until("id", json!(1))["result"]["contents"], marker);
    }
    a.send(json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":null}));
    assert!(a.until("id", json!(2))["result"].is_null());
    a.notify("exit", Value::Null);
    assert_eq!(
        editor_query(&directory, "open")[0]["text"],
        "fn second() {}\n"
    );
    assert_eq!(editor_query(&directory, "duplicates"), 0);
    assert_eq!(status()["server_pid"], before["server_pid"]);
    assert_eq!(status()["nonce"], before["nonce"]);
    drop(b);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let status = status();
        if status["attached"] == json!([]) {
            assert_eq!(
                status["leases"].as_array().unwrap().len() + 1,
                before["leases"].as_array().unwrap().len()
            );
            break;
        }
        assert!(Instant::now() < deadline, "editors not detached: {status}");
    }
    editor_rpc(&directory, json!({"op":"stop","reason":"checkout removed"}));
    assert!(broker.wait().unwrap().success());
}

#[test]
fn lsp_attach_survives_dormancy_and_terminal_stop_unblocks_readers() {
    let env = Env::new();
    let (mut broker, directory) = editor_broker(&env, "2s");
    let uri = url::Url::from_file_path(env.project_root.join("lib.rs"))
        .unwrap()
        .to_string();
    let mut editor = Editor::attach(&directory, "idle editor");
    let mut closing = Editor::attach(&directory, "closing editor");
    editor.ready();
    editor.open(&uri, 7, "fn unsaved() {}\n");
    editor.diagnostic(7);
    let before = editor_rpc(&directory, json!({"op":"status"}));
    let status = editor.until("method", json!("experimental/serverStatus"));
    assert_eq!(status["params"]["health"], "warning");
    let idle = editor_rpc(&directory, json!({"op":"status"}));
    closing.send(json!({"id":1,"method":"shutdown"}));
    assert!(closing.until("id", json!(1))["result"].is_null());
    closing.send(json!({"id":2,"method":"textDocument/hover"}));
    assert_eq!(closing.until("id", json!(2))["error"]["code"], -32600);
    let after = editor_rpc(&directory, json!({"op":"status"}));
    assert_eq!(after["request_count"], idle["request_count"]);
    assert_eq!(after["last_request_at_ms"], idle["last_request_at_ms"]);
    assert_eq!(after["state"], idle["state"]);
    closing.notify("exit", Value::Null);
    editor.send(json!({"jsonrpc":"2.0","id":1,"method":"textDocument/hover","params":{}}));
    assert_eq!(
        editor.until("id", json!(1))["result"]["contents"],
        "fixture hover"
    );
    assert_eq!(
        editor_query(&directory, "open")[0]["text"],
        "fn unsaved() {}\n"
    );
    assert_ne!(
        editor_rpc(&directory, json!({"op":"status"}))["server_pid"],
        before["server_pid"]
    );
    editor.send(json!({"id":90,"method":"textDocument/hover","params":{"hold":true}}));
    editor.send(json!({"id":91,"method":"textDocument/hover","params":{}}));
    editor.until("id", json!(91));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut frames = Vec::new();
        while let Ok(frame) = rimz::lsp::protocol::read_frame(&mut editor.0) {
            frames.push(frame);
        }
        tx.send(frames).unwrap();
    });
    editor_rpc(&directory, json!({"op":"stop","reason":"checkout removed"}));
    let frames = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("terminal stop must unblock reader");
    assert!(
        frames
            .iter()
            .any(|frame| frame["method"] == "experimental/serverStatus"
                && frame["params"]["message"] == "language server rust stopped: checkout removed"),
        "stop status lost before EOF: {frames:?}"
    );
    assert!(
        frames
            .iter()
            .any(|frame| frame["id"] == 90 && frame["error"]["code"] == -32802),
        "pending reply lost: {frames:?}"
    );
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame["method"] == "window/showMessage")
            .count(),
        1
    );
    assert!(broker.wait().unwrap().success());
}

#[test]
fn lsp_show_batches_source_and_failures() {
    let env = Env::new();
    assert!(
        std::process::Command::new("git")
            .current_dir(&env.project_root)
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(
        env.project_root.join("show.rs"),
        (1..=210)
            .map(|n| format!("  line {n}\n"))
            .collect::<String>(),
    )
    .unwrap();
    std::fs::write(env.project_root.join("notes.py"), "pass\n").unwrap();
    let (mut broker, _, _) = start_stub_broker(&env, env.project_root.clone());
    env.rimz()
        .args([
            "lsp",
            "show",
            "show.rs::Parent::child",
            "show.rs:208:4",
            "show.rs:2:4 (1-2)",
        ])
        .assert()
        .success()
        .stdout(
            [
                "show.rs:3-5\n     3\t  line 3\n     4\t  line 4\n     5\t  line 5\n",
                "show.rs:208-209\n   208\t  line 208\n   209\t  line 209\n",
                "show.rs:1-2\n     1\t  line 1\n     2\t  line 2\n",
            ]
            .join("\n"),
        );
    env.rimz()
        .args(["lsp", "show", "show.rs::Parent"])
        .assert()
        .success()
        .stdout(
            "show.rs:1-205  (outline: 205 lines; --full prints the body)\n     4\tline 4 (3-5)\n",
        );
    env.rimz()
        .args(["lsp", "show", "show.rs::Parent", "--full"])
        .assert()
        .success()
        .stdout(predicates::str::contains("   205\t  line 205\n"));
    env.rimz()
        .args([
            "lsp",
            "show",
            "show.rs:210-211",
            "gone.rs::x",
            "show.rs::child",
        ])
        .assert()
        .code(5)
        .stdout(predicates::str::contains("show.rs:210-210\n   210\t  line 210\n"))
        .stdout(predicates::str::contains("show.rs:3-5\n     3\t  line 3\n     4\t  line 4\n     5\t  line 5\n\nshow.rs:208-209\n   208\t  line 208\n   209\t  line 209\n"));
    env.rimz()
        .args([
            "lsp",
            "show",
            "show.rs::child",
            "notes.py::f",
            "o/r@v1:gone.rs::x",
        ])
        .assert()
        .code(3)
        .stdout(predicates::str::contains("external"));
    env.rimz()
        .args(["lsp", "show", "show.rs:1-2", "invalid"])
        .assert()
        .code(2)
        .stdout("");
    broker.kill().unwrap();
    broker.wait().unwrap();
}

#[test]
fn lsp_check_fixes_hints_without_rewriting_other_notes() {
    let env = Env::new();
    assert!(
        std::process::Command::new("git")
            .current_dir(&env.project_root)
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success()
    );
    for file in [
        "show.rs",
        "one/dup.rs",
        "two/dup.rs",
        "notes.py",
        "docs/manual.md",
    ] {
        let path = env.project_root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "line\n".repeat(210)).unwrap();
    }
    let notes = env.project_root.join("notes.md");
    let source = "é `show.rs::Parent::child (~90-99)`\n`show.rs::Parent::child`:90:99\n";
    let initial = format!("{source}`manual.md:~10`\n");
    std::fs::write(&notes, &initial).unwrap();
    std::os::unix::fs::symlink("notes.md", env.project_root.join("link.md")).unwrap();
    let (mut broker, _, _) = start_stub_broker(&env, env.project_root.clone());
    let plain = env
        .rimz()
        .args(["lsp", "check", "notes.md", "--json"])
        .output()
        .unwrap();
    assert_eq!(plain.status.code(), Some(7));
    assert!(
        serde_json::from_slice::<serde_json::Value>(&plain.stdout)
            .unwrap()
            .get("fixes")
            .is_none()
    );
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), initial);
    let output = env
        .rimz()
        .args(["lsp", "check", "link.md", "--fix"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        text,
        "link.md:1  fixed  show.rs::Parent::child (~90-99)  show.rs::Parent::child (~3-5)\nlink.md:2  fixed  show.rs::Parent::child:90:99  show.rs::Parent::child:4:4\nlink.md:3  fixed  manual.md:~10  docs/manual.md:~10\n3 anchors in link.md: 3 ok, 0 failed, 0 unchecked, 0 external\n"
    );
    assert!(env.project_root.join("link.md").is_symlink());
    assert_eq!(
        std::fs::read_to_string(&notes).unwrap(),
        "é `show.rs::Parent::child (~3-5)`\n`show.rs::Parent::child`:4:4\n`docs/manual.md:~10`\n"
    );
    let modified = std::fs::metadata(&notes).unwrap().modified().unwrap();
    let output = env
        .rimz()
        .args(["lsp", "check", "link.md", "--fix", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["fixes"], serde_json::json!([]));
    assert_eq!(
        std::fs::metadata(&notes).unwrap().modified().unwrap(),
        modified
    );
    std::fs::write(&notes, "`dup.rs::defined`\n").unwrap();
    let output = env
        .rimz()
        .args(["lsp", "check", "notes.md", "--fix"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "notes.md:1  fixed  dup.rs::defined  one/dup.rs::defined\n1 anchors in notes.md: 1 ok, 0 failed, 0 unchecked, 0 external\n"
    );
    assert_eq!(
        std::fs::read_to_string(&notes).unwrap(),
        "`one/dup.rs::defined`\n"
    );
    let skipped = "`show.rs:999`\n`gone.rs::x ~99`\n`dup.rs::x ~99`\n`show.rs::absent ~99`\n`show.rs::child ~4-208`\n`show.rs::child ~100`\n`o/r@v1:show.rs::Parent ~99`\n`notes.py::x ~99`\n`show.rs::Parent`\n";
    std::fs::write(&notes, format!("{source}`show.rs::child ~4`\n{skipped}")).unwrap();
    let output = env
        .rimz()
        .args(["lsp", "check", "notes.md", "--fix", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["fixes"].as_array().unwrap().len(), 3);
    assert_eq!(
        value["fixes"][2],
        serde_json::json!({"line":3,"before":"show.rs::child ~4","after":"show.rs::child ~3"})
    );
    assert_eq!(
        value["fixes"][0],
        serde_json::json!({"line":1,"before":"show.rs::Parent::child (~90-99)","after":"show.rs::Parent::child (~3-5)"})
    );
    assert!(std::fs::read_to_string(&notes).unwrap().ends_with(skipped));
    broker.kill().unwrap();
    broker.wait().unwrap();
}

#[test]
fn lsp_check_fix_hints_inserts_hints_for_single_items() {
    let env = Env::new();
    assert!(
        std::process::Command::new("git")
            .current_dir(&env.project_root)
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success()
    );
    for file in ["show.rs", "lib.rs"] {
        std::fs::write(env.project_root.join(file), "line\n".repeat(210)).unwrap();
    }
    let notes = env.project_root.join("notes.md");
    let hintless = "`show.rs::Parent::child`\n`lib.rs::Type`\n`lib.rs::saved`\n`show.rs::child`\n";
    std::fs::write(&notes, hintless).unwrap();
    std::os::unix::fs::symlink("notes.md", env.project_root.join("link.md")).unwrap();
    let (mut broker, _, _) = start_stub_broker(&env, env.project_root.clone());
    env.rimz()
        .args(["lsp", "check", "notes.md", "--hints"])
        .assert()
        .code(2)
        .stdout("");
    for args in [&["--json"][..], &["--fix", "--json"][..]] {
        let output = env
            .rimz()
            .args(["lsp", "check", "notes.md"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(value.get("ambiguous").is_none(), "{args:?}");
        assert_eq!(std::fs::read_to_string(&notes).unwrap(), hintless);
    }
    std::fs::write(
        &notes,
        format!("{hintless}`show.rs::Parent::child (~90-99)`\n"),
    )
    .unwrap();
    let output = env
        .rimz()
        .args(["lsp", "check", "link.md", "--fix", "--hints"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "link.md:1  fixed  show.rs::Parent::child  show.rs::Parent::child (3-5)\n\
         link.md:2  fixed  lib.rs::Type  lib.rs::Type (1-2)\n\
         link.md:3  fixed  lib.rs::saved  lib.rs::saved (7)\n\
         link.md:5  fixed  show.rs::Parent::child (~90-99)  show.rs::Parent::child (~3-5)\n\
         link.md:4  ambiguous-symbol  show.rs::child  method Parent::child is at 3-5; function child is at 208-209\n\
         1 anchor left without a hint: several items match\n\
         5 anchors in link.md: 5 ok, 0 failed, 0 unchecked, 0 external\n"
    );
    assert!(env.project_root.join("link.md").is_symlink());
    assert_eq!(
        std::fs::read_to_string(&notes).unwrap(),
        "`show.rs::Parent::child` (3-5)\n`lib.rs::Type` (1-2)\n`lib.rs::saved` (7)\n`show.rs::child`\n`show.rs::Parent::child (~3-5)`\n"
    );
    let modified = std::fs::metadata(&notes).unwrap().modified().unwrap();
    let output = env
        .rimz()
        .args(["lsp", "check", "link.md", "--fix", "--hints", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["fixes"], serde_json::json!([]));
    assert_eq!(value["ambiguous"].as_array().unwrap().len(), 1);
    assert_eq!(
        std::fs::metadata(&notes).unwrap().modified().unwrap(),
        modified
    );
    broker.kill().unwrap();
    broker.wait().unwrap();
}

#[test]
fn lsp_check_multiple_files() {
    let env = Env::new();
    assert!(
        std::process::Command::new("git")
            .current_dir(&env.project_root)
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success()
    );
    for (file, source) in [
        ("lib.rs", "struct Type;\n"),
        ("ok.md", "`lib.rs::Type`\n"),
        ("other.md", "`lib.rs::Type`\n"),
        ("bad.md", "`gone.rs::absent`\n"),
    ] {
        std::fs::write(env.project_root.join(file), source).unwrap();
    }
    let (mut broker, _, _) = start_stub_broker(&env, env.project_root.clone());
    let single = |file: &str| env.rimz().args(["lsp", "check", file]).output().unwrap();
    let ok = single("ok.md");
    let other = single("other.md");
    let bad = single("bad.md");
    let missing = single("missing.md");
    let missing_error = String::from_utf8(missing.stderr).unwrap();
    let missing_error = missing_error.trim().strip_prefix("error: ").unwrap();
    let error_line = format!("missing.md  error  {missing_error}\n");
    let mut failures = Vec::new();
    for (files, code, expected) in [
        (
            ["ok.md", "other.md"],
            0,
            [ok.stdout.clone(), other.stdout].concat(),
        ),
        (
            ["ok.md", "bad.md"],
            7,
            [ok.stdout.clone(), bad.stdout.clone()].concat(),
        ),
        (
            ["missing.md", "ok.md"],
            1,
            [error_line.as_bytes(), &ok.stdout].concat(),
        ),
        (
            ["missing.md", "bad.md"],
            1,
            [error_line.as_bytes(), &bad.stdout].concat(),
        ),
        (
            ["ok.md", "ok.md"],
            0,
            [ok.stdout.clone(), ok.stdout].concat(),
        ),
    ] {
        let output = env
            .rimz()
            .args(["lsp", "check"])
            .args(files)
            .output()
            .unwrap();
        if output.status.code() != Some(code)
            || output.stdout != expected
            || !output.stderr.is_empty()
        {
            failures.push(format!("{files:?}: {output:?}"));
        }
    }
    let single_json = env
        .rimz()
        .args(["lsp", "check", "ok.md", "--json"])
        .output()
        .unwrap();
    let mut checked: Value = serde_json::from_slice(&single_json.stdout).unwrap();
    checked["exit"] = json!(0);
    let output = env
        .rimz()
        .args(["lsp", "check", "ok.md", "missing.md", "--json"])
        .output()
        .unwrap();
    if output.status.code() != Some(1)
        || !output.stderr.is_empty()
        || serde_json::from_slice::<Value>(&output.stdout).ok()
            != Some(json!([
                checked, {"notes":"missing.md", "exit":1, "error":missing_error}
            ]))
    {
        failures.push(format!("json: {output:?}"));
    }
    for file in ["ok.md", "other.md"] {
        std::fs::write(env.project_root.join(file), "`lib.rs::Type` (~90-99)\n").unwrap();
    }
    let output = env
        .rimz()
        .args([
            "lsp",
            "check",
            "ok.md",
            "missing.md",
            "other.md",
            "--fix",
            "--json",
        ])
        .output()
        .unwrap();
    let fixed = serde_json::from_slice::<Value>(&output.stdout).unwrap_or(Value::Null);
    if output.status.code() != Some(1)
        || !output.stderr.is_empty()
        || fixed[0]["fixes"].as_array().map(Vec::len) != Some(1)
        || fixed[2]["fixes"].as_array().map(Vec::len) != Some(1)
        || ["ok.md", "other.md"].iter().any(|file| {
            std::fs::read_to_string(env.project_root.join(file))
                .unwrap()
                .contains("90-99")
        })
    {
        failures.push(format!("fix: {output:?}"));
    }
    broker.kill().unwrap();
    broker.wait().unwrap();
    let output = env
        .rimz()
        .args(["lsp", "check", "ok.md", "other.md"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    if output.status.code() != Some(3)
        || !output.stderr.is_empty()
        || text.lines().count() != 2
        || !text.starts_with("ok.md  error  ")
        || !text.contains("\nother.md  error  ")
    {
        failures.push(format!("unavailable: {output:?}"));
    }
    env.rimz().args(["lsp", "check"]).assert().code(2);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn lsp_check_reports_anchor_failures_and_coverage() {
    use serde_json::{Value, json};

    let env = Env::new();
    for args in [&["init", "-q", "-b", "main"][..], &["add", "."][..]] {
        assert!(
            std::process::Command::new("git")
                .current_dir(&env.project_root)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    for (file, contents) in [
        ("lib.rs", "struct Type;\nfn saved() {}\n"),
        ("src/dup.rs", ""),
        ("other/dup.rs", ""),
        ("src/unique.rs", "fn unique() {}\n"),
        ("my notes/spaced.txt", "line\n"),
        ("notes.py", ""),
        ("Cargo.toml", ""),
        (
            "notes.md",
            "`lib.rs::Type::method` (~3)\n`lib.rs::Type.field`\n`lib.rs::nosuch`\n`dup.rs::x`\n`gone.rs::x`\n`lib.rs::saved` (~40)\n`lib.rs:2`\n`notes.py::f`\n`o/r@v1:gone.rs::x`\n`unique.rs:1`\n`spaced.txt:1`\n",
        ),
        (
            "ok.md",
            "`lib.rs::Type::method` (~3)\n`lib.rs::Type.field`\n`lib.rs:2`\n`o/r@v1:gone.rs::x`\n",
        ),
    ] {
        let path = env.project_root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    let (mut broker, _, _) = start_stub_broker(&env, env.project_root.clone());
    let staged = env.project_root.join("staged.rs");
    std::fs::write(&staged, "fn f() {}\n").unwrap();
    std::fs::write(
        env.project_root.join("deleted.md"),
        "`staged.rs:1`\n`staged.rs::f`\n",
    )
    .unwrap();
    assert!(
        std::process::Command::new("git")
            .current_dir(&env.project_root)
            .args(["add", "staged.rs"])
            .status()
            .unwrap()
            .success()
    );
    std::fs::remove_file(&staged).unwrap();
    let output = env
        .rimz()
        .args(["lsp", "check", "deleted.md"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    for line in [1, 2] {
        assert!(
            text.contains(&format!("deleted.md:{line}  missing-path  ")),
            "{text}"
        );
    }
    let output = env
        .rimz()
        .args(["lsp", "check", "notes.md"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains("notes.md:10  short-path  unique.rs:1  resolves to src/unique.rs\n"),
        "{text}"
    );
    assert_eq!(text.lines().count(), 7, "{text}");
    for (line, status) in [
        (3, "missing-symbol"),
        (4, "ambiguous-path"),
        (5, "missing-path"),
        (6, "line-outside"),
        (8, "unchecked"),
        (10, "short-path"),
    ] {
        assert!(
            text.contains(&format!("notes.md:{line}  {status}  ")),
            "{text}"
        );
    }
    assert!(text.ends_with("11 anchors in notes.md: 4 ok, 5 failed, 1 unchecked, 1 external\n"));
    let output = env
        .rimz()
        .args(["lsp", "check", "notes.md", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert!(output.stderr.is_empty());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["summary"],
        json!({"anchors":11,"ok":4,"failed":5,"unchecked":1,"external":1})
    );
    let statuses: Vec<_> = value["anchors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["status"].as_str().unwrap())
        .collect();
    assert_eq!(
        statuses,
        [
            "ok",
            "ok",
            "missing-symbol",
            "ambiguous-path",
            "missing-path",
            "line-outside",
            "ok",
            "unchecked",
            "external",
            "short-path",
            "ok"
        ]
    );
    assert_eq!(value["anchors"][3]["files"].as_array().unwrap().len(), 2);
    assert_eq!(value["anchors"][9]["path"], "src/unique.rs");
    assert_eq!(value["anchors"][9]["files"], json!([]));
    assert_eq!(value["anchors"][9]["detail"], "resolves to src/unique.rs");
    env.rimz()
        .args(["lsp", "check", "ok.md"])
        .assert()
        .success()
        .stderr("")
        .stdout("4 anchors in ok.md: 3 ok, 0 failed, 0 unchecked, 1 external\n");
    env.rimz()
        .args(["lsp", "check", "missing.md"])
        .assert()
        .code(1)
        .stdout("");
    broker.kill().unwrap();
    broker.wait().unwrap();
    env.rimz()
        .args(["lsp", "check", "ok.md"])
        .assert()
        .code(3)
        .stdout("");
}

#[test]
fn lsp_attach_bridge_resolves_admits_and_versions() {
    let env = Env::new();
    let output = env
        .rimz()
        .args(["lsp", "attach", "--version"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("rimz lsp attach {}\n", rimz::build_id::VERSION)
    );
    let stub = crate::common::cargo_bin("lsp-server-stub", env!("CARGO_BIN_EXE_lsp-server-stub"));
    std::fs::write(env.project_root.join("Cargo.toml"), "").unwrap();
    std::fs::write(env.rimz_home().join("config.toml"), format!("[lsp]\nreserve-percent = 0\nreserve-min = '0'\nkill-floor-percent = 0\n[lsp.servers.rust]\ncommand = [{}]\nextensions = ['rs']\nroot-markers = ['Cargo.toml']\nmemory-estimate = '1M'\n", serde_json::to_string(&stub).unwrap())).unwrap();
    let first = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":url::Url::from_directory_path(&env.project_root).unwrap().to_string()}});
    let mut child = env
        .rimz()
        .args(["lsp", "attach", "--server", "rust", "--stdio"])
        .current_dir(&env.home_root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    rimz::lsp::protocol::write_frame(&mut input, &first).unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let answer = rimz::lsp::protocol::read_frame(&mut output).unwrap();
    assert_eq!(answer["id"], 1);
    assert_eq!(
        answer["result"]["capabilities"]["textDocumentSync"]["change"],
        1
    );
    let status = env
        .rimz()
        .args(["lsp", "status", "--json"])
        .output()
        .unwrap();
    assert!(status.status.success(), "{status:?}");
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["attached"][0]["pid"], child.id());
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");

    let mut child = env
        .rimz()
        .args(["lsp", "attach", "--server", "rust"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    rimz::lsp::protocol::write_frame(&mut input, &first).unwrap();
    assert_eq!(
        rimz::lsp::protocol::read_frame(&mut output).unwrap()["id"],
        1
    );
    for frame in [
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"shutdown"}),
    ] {
        rimz::lsp::protocol::write_frame(&mut input, &frame).unwrap();
    }
    loop {
        let frame = rimz::lsp::protocol::read_frame(&mut output).unwrap();
        if frame["id"] == 2 {
            assert_eq!(frame["result"], Value::Null);
            assert!(frame.get("error").is_none(), "{frame}");
            break;
        }
    }
    rimz::lsp::protocol::write_frame(&mut input, &json!({"jsonrpc":"2.0","method":"exit"}))
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    drop(input);

    let shim_dir = env.home_root.join("bin with ' quote");
    let shim = env
        .rimz()
        .args(["lsp", "shim", "--server", "rust", "--dir"])
        .arg(&shim_dir)
        .output()
        .unwrap();
    assert!(shim.status.success(), "{shim:?}");
    let output = std::process::Command::new(shim_dir.join("rimz-lsp-rust"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("rimz lsp attach {}\n", rimz::build_id::VERSION)
    );

    let mut child = env
        .rimz()
        .args(["lsp", "attach", "--server", "rust"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    rimz::lsp::protocol::write_frame(&mut input, &first).unwrap();
    assert_eq!(
        rimz::lsp::protocol::read_frame(&mut output).unwrap()["id"],
        1
    );
    let root = env.project_root.canonicalize().unwrap();
    let directory = env
        .runtime_root
        .join("rimz/lsp")
        .join(rimz::lsp::registry::key(&root, "rust").unwrap());
    editor_rpc(&directory, json!({"op":"stop","reason":"checkout removed"}));
    while rimz::lsp::protocol::read_frame(&mut output).is_ok() {}
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!(
            "language server rust for {} is stopped: checkout removed\n",
            root.display()
        )
    );
    drop(input);

    let required = Env::new();
    std::fs::write(required.project_root.join("Cargo.toml"), "").unwrap();
    std::fs::write(required.rimz_home().join("config.toml"), "[lsp]\nreserve-min = '1000000G'\n[lsp.servers.rust]\ncommand = ['/bin/true']\nextensions = ['rs']\nroot-markers = ['Cargo.toml']\npolicy = 'required'\nwait-timeout = '1s'\n").unwrap();
    let mut child = required
        .rimz()
        .args(["lsp", "attach", "--server", "rust"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    rimz::lsp::protocol::write_frame(
        child.stdin.as_mut().unwrap(),
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    )
    .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("waiting to start language server rust"),
        "{stderr}"
    );
    assert!(stderr.contains("position 1 in the queue"), "{stderr}");
    assert!(
        stderr.contains("required but memory stayed short for 1s"),
        "{stderr}"
    );
}

/// A broker an agent's `rimz` starts outlives that agent and serves others, so
/// its language server gets the user's temp environment, not the agent's unit.
#[test]
fn lsp_broker_started_from_an_agent_gives_its_server_the_user_tmpdir() {
    let stub = crate::common::cargo_bin("lsp-server-stub", env!("CARGO_BIN_EXE_lsp-server-stub"));
    for saved in ["user-tmp", ""] {
        let env = Env::new();
        let unit = env.home_root.join("unit");
        let user = env.home_root.join("user-tmp");
        std::fs::create_dir_all(&unit).unwrap();
        std::fs::create_dir_all(&user).unwrap();
        let (saved, expected) = if saved.is_empty() {
            (String::new(), "unset|unset|unset".to_owned())
        } else {
            let user = user.display().to_string();
            (user.clone(), format!("{user}|unset|unset"))
        };
        let marker = env.home_root.join("server-env");
        let wrapper = format!(
            "printf '%s|%s|%s' \"${{TMPDIR-unset}}\" \"${{CLAUDE_CODE_TMPDIR-unset}}\" \"${{RIMZ_USER_TMPDIR-unset}}\" > {}; exec {}",
            shlex::try_quote(marker.to_str().unwrap()).unwrap(),
            shlex::try_quote(stub.to_str().unwrap()).unwrap()
        );
        std::fs::write(env.project_root.join("Cargo.toml"), "").unwrap();
        std::fs::write(env.rimz_home().join("config.toml"), format!("[lsp]\nreserve-percent = 0\nreserve-min = '0'\nkill-floor-percent = 0\n[lsp.servers.rust]\ncommand = ['sh', '-c', {}]\nextensions = ['rs']\nroot-markers = ['Cargo.toml']\nmemory-estimate = '1M'\n", serde_json::to_string(&wrapper).unwrap())).unwrap();
        let mut child = env
            .rimz()
            .args(["lsp", "attach", "--server", "rust"])
            .env("TMPDIR", &unit)
            .env("RIMZ_USER_TMPDIR", &saved)
            .env("CLAUDE_CODE_TMPDIR", &unit)
            .env("RIMZ_TEMP_ROOT_KEYS", "CLAUDE_CODE_TMPDIR")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        rimz::lsp::protocol::write_frame(&mut input, &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":url::Url::from_directory_path(&env.project_root).unwrap().to_string()}})).unwrap();
        assert_eq!(
            rimz::lsp::protocol::read_frame(&mut output).unwrap()["id"],
            1
        );
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            expected,
            "saved {saved:?}"
        );
        drop(input);
        child.kill().unwrap();
        child.wait().unwrap();
    }
}

#[test]
fn lsp_attach_refuses_an_optional_server_that_never_starts() {
    let env = Env::new();
    std::fs::write(env.project_root.join("Cargo.toml"), "").unwrap();
    std::fs::write(env.rimz_home().join("config.toml"), "[lsp.servers.rust]\ncommand = ['/bin/true']\nextensions = ['rs']\nroot-markers = ['Cargo.toml']\n").unwrap();
    let mut child = env
        .rimz()
        .args(["lsp", "attach", "--server", "rust"])
        .env("RIMZ_BIN", "/bin/true")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    rimz::lsp::protocol::write_frame(
        child.stdin.as_mut().unwrap(),
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    )
    .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "language server rust did not start within 5s; check the server command, or remove [lsp.servers.rust]\n"
    );
}

#[test]
fn lsp_required_launch_waits_then_refuses_before_recording_a_run() {
    let env = Env::new();
    crate::common::write_kind_base(&env, "claude");
    env.install_agent_hooks("claude");
    let agent_bin = crate::common::write_failing_agent_shim(&env, "claude", 1);
    let shell = crate::common::write_fake_login_shell(&env, "lsp-test-sh", &[]);
    std::fs::write(env.rimz_home().join("theme.toml"), "[broken").unwrap();
    std::fs::write(env.project_root.join("Cargo.toml"), "").unwrap();
    std::fs::write(env.rimz_home().join("config.toml"), "[agents]\nisolation = 'host'\n[lsp]\nreserve-min = '1000000G'\n[lsp.servers.rust]\ncommand = ['/bin/true']\nextensions = ['rs']\nroot-markers = ['Cargo.toml']\npolicy = 'required'\nwait-timeout = '1s'\n").unwrap();
    let output = env
        .rimz()
        .args(["agents", "claude", "inspect", "-p"])
        .env("SHELL", shell)
        .env("PATH", crate::common::path_with_front(&agent_bin))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("waiting to start language server rust"),
        "{stderr}"
    );
    assert!(stderr.contains("position 1 in the queue"), "{stderr}");
    assert!(
        stderr.contains("required but memory stayed short for 1s"),
        "{stderr}"
    );
    assert!(stderr.contains("stop one with rimz lsp stop"), "{stderr}");
    assert!(
        rimz::harness::run::list(env.store().paths())
            .unwrap()
            .is_empty()
    );
    let output = env.rimz().args(["doctor", "--json"]).output().unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["lsp"]["ready"]["last_refusal"]["event"],
        "queue_timeout"
    );
}

#[test]
fn lsp_broker_starts_lazily_watches_saves_and_restarts() {
    use serde_json::{Value, json};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let env = Env::new();
    let (mut broker, directory, request) =
        start_stub_broker(&env, env.project_root.join("parent-project"));
    let rpc = |value: Value| -> Value {
        let mut stream = UnixStream::connect(directory.join("sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        writeln!(stream, "{value}").unwrap();
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).unwrap();
        serde_json::from_str(&response).unwrap()
    };
    let pid = std::process::id();
    let status = rpc(json!({"op": "status"}));
    assert!(status["state"].get("dormant").is_some(), "{status}");
    assert!(status["server_pid"].is_null());
    let lease = json!({"op": "lease", "pid": pid, "start_token": rimz::proc::process_start_token(pid).unwrap()});
    assert_eq!(rpc(lease.clone())["ok"], true);
    assert_eq!(rpc(lease)["ok"], true);
    assert_eq!(
        rpc(json!({"op": "status"}))["leases"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let query = |name| {
        rpc(
            json!({"op": "query", "method": "workspace/symbol", "params": {"query": name}, "wait_ms": 4000}),
        )
    };
    assert_eq!(query("symbol")["result"], json!([]));
    let status = rpc(json!({"op": "status"}));
    assert_eq!(status["state"], "ready");
    assert_eq!(status["request_count"], 1);
    let server_pid = status["server_pid"].as_u64().unwrap();
    assert_eq!(
        std::fs::read_to_string(format!("/proc/{server_pid}/oom_score_adj"))
            .unwrap()
            .trim(),
        "800"
    );
    std::fs::write(env.project_root.join("lib.rs"), "fn saved() {}\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let response = query("changes");
        if response["result"]["changes"]
            .as_array()
            .is_some_and(|changes| {
                changes
                    .iter()
                    .any(|change| change["uri"].as_str().unwrap().ends_with("/lib.rs"))
            })
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "save never reached server: {response}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    env.rimz()
        .args(["lsp", "hover", "lib.rs:1:4"])
        .assert()
        .success()
        .stdout("fixture hover\n");
    env.rimz()
        .args(["lsp", "find", "anything", "--json"])
        .assert()
        .success()
        .stdout("[]\n");
    std::fs::write(env.project_root.join("empty.rs"), "").unwrap();
    for verb in ["def", "refs", "impl", "callers", "callees", "symbols"] {
        let target = if verb == "symbols" {
            "empty.rs"
        } else {
            "lib.rs:1:4"
        };
        env.rimz()
            .args(["lsp", verb, target, "--json"])
            .assert()
            .success()
            .stdout("[]\n");
    }
    env.rimz()
        .args(["lsp", "def", "alias"])
        .assert()
        .success()
        .stdout("/fixture/definition.rs:1:1\n");
    for name in [
        "deep::pathed",
        "crate::deep::pathed",
        "deep::pathed::pathed",
        "pathed()",
        "a::twin",
    ] {
        env.rimz()
            .args(["lsp", "def", name])
            .assert()
            .success()
            .stderr("")
            .stdout(if name == "a::twin" {
                "src/a.rs:1:1\n"
            } else {
                "src/deep/pathed.rs:1:1\n"
            });
    }
    for (verb, method) in [
        ("def", "textDocument/definition"),
        ("refs", "textDocument/references"),
        ("hover", "textDocument/hover"),
        ("impl", "textDocument/implementation"),
        ("callers", "textDocument/prepareCallHierarchy"),
        ("callees", "textDocument/prepareCallHierarchy"),
    ] {
        for target in ["Type::field", "lib.rs::Type::field"] {
            env.rimz().args(["lsp", verb, target]).assert().success();
            let trace = query("requests");
            let requests = trace["result"]["requests"].as_array().unwrap();
            let navigation = &requests[requests.len() - 2];
            assert_eq!(navigation["method"], method, "{target}");
            assert_eq!(
                navigation["params"]["position"],
                json!({"line":1,"character":0})
            );
            assert_eq!(
                navigation["params"]["textDocument"]["uri"],
                format!("file://{}/lib.rs", env.project_root.display())
            );
        }
    }
    env.rimz()
        .args(["lsp", "def", "a/lib.rs::TwinType::field"])
        .assert()
        .success();
    env.rimz()
        .args(["lsp", "def", "empty.rs::Type::field"])
        .assert()
        .code(5)
        .stdout("not found: empty.rs::Type::field\n");
    for name in ["Type::nosuch", "Type::method", "Wrong::field", "field"] {
        env.rimz()
            .args(["lsp", "def", name])
            .assert()
            .code(5)
            .stdout(format!("not found: {name}\n"));
    }
    env.rimz().args(["lsp", "def", "TwinType::field"]).assert().code(6).stdout("ambiguous: 2 symbols named TwinType::field; rerun with one of these names or a position\nfield a::TwinType::field  a/lib.rs:2:1\nfield b::TwinType::field  b/lib.rs:2:1\n");
    let outline_requests = || {
        query("requests")["result"]["requests"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|request| request["method"] == "textDocument/documentSymbol")
            .count()
    };
    let outlines_before = outline_requests();
    env.rimz()
        .args(["lsp", "def", "BroadType::field"])
        .assert()
        .code(5)
        .stdout("not found: BroadType::field\n");
    assert_eq!(outline_requests(), outlines_before);
    let definition_requests = || {
        query("requests")["result"]["requests"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|request| request["method"] == "textDocument/definition")
            .count()
    };
    for (name, code, header) in [
        (
            "Wrong::Many",
            5,
            "not found: Wrong::Many; up to 45 other symbols named Many:",
        ),
        (
            "Many",
            6,
            "ambiguous: up to 45 symbols named Many; rerun with one of these names or a position",
        ),
    ] {
        let before = definition_requests();
        let output = env.rimz().args(["lsp", "def", name]).output().unwrap();
        assert_eq!(output.status.code(), Some(code));
        assert_eq!(definition_requests() - before, 30, "{name}");
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(text.lines().next().unwrap(), header);
        assert_eq!(text.lines().count(), 22);
        assert!(text.ends_with("25 more; narrow with a qualifier or use find\n"));
    }
    let before = definition_requests();
    let output = env
        .rimz()
        .args(["lsp", "def", "Wrong::Many", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));
    assert_eq!(definition_requests() - before, 30);
    let output: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output["total"], 45);
    assert_eq!(output["truncated"], true);
    assert_eq!(output["candidates"].as_array().unwrap().len(), 30);
    for name in [
        "a::TwinType::field",
        "b::TwinType::field",
        "UniqueMember::field",
    ] {
        env.rimz().args(["lsp", "def", name]).assert().success();
    }
    env.rimz().args(["lsp", "def", "wrong::pathed"]).assert().code(5).stderr("").stdout("not found: wrong::pathed; 1 other symbol named pathed:\nfunction deep::pathed::pathed  src/deep/pathed.rs:1:1\n");
    env.rimz()
        .args(["lsp", "def", "nosuch"])
        .assert()
        .code(5)
        .stderr("")
        .stdout("not found: nosuch\n");
    env.rimz().args(["lsp", "def", "twin"]).assert().code(6).stderr("").stdout("ambiguous: 2 symbols named twin; rerun with one of these names or a position\nfunction a::twin  src/a.rs:1:1\nfunction b::twin  src/b.rs:1:1\n");
    for (name, code, outcome, candidates) in [
        ("nosuch", 5, "not-found", json!([])),
        (
            "twin",
            6,
            "ambiguous",
            json!([
                {"name": "a::twin", "kind": "function", "position": "src/a.rs:1:1"},
                {"name": "b::twin", "kind": "function", "position": "src/b.rs:1:1"}
            ]),
        ),
    ] {
        let output = env
            .rimz()
            .args(["lsp", "def", name, "--json"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(code));
        assert!(output.stderr.is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap(),
            json!({"outcome": outcome, "name": name, "total": candidates.as_array().unwrap().len(), "truncated": false, "candidates": candidates})
        );
    }
    env.rimz().args(["lsp", "find", "work"]).assert().success().stdout("function w::work  src/w.rs:1:1\nfunction w::worker  src/w.rs:1:1\nfunction r::rework  src/r.rs:1:1\nfunction u::unrelated  src/u.rs:1:1\n");
    for json in [false, true] {
        let targets = ["alias", "nosuch", "twin", "alias"];
        let mut blocks = Vec::new();
        let mut values = Vec::new();
        for target in targets {
            let mut command = env.rimz();
            command.args(["lsp", "def", target]);
            if json {
                command.arg("--json");
            }
            let output = command.output().unwrap();
            let code = output.status.code().unwrap();
            if json {
                let mut value: Value = serde_json::from_slice(&output.stdout).unwrap();
                if code == 0 {
                    value = json!({"outcome": "answer", "result": value});
                }
                value["target"] = target.into();
                value["exit"] = code.into();
                values.push(value);
            } else {
                blocks.push(format!(
                    "==> {target} <==\n{}",
                    String::from_utf8(output.stdout).unwrap()
                ));
            }
        }
        let mut command = env.rimz();
        command.args(["lsp", "def"]).args(targets);
        if json {
            command.arg("--json");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(6));
        assert!(output.stderr.is_empty());
        if json {
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap(),
                json!(values)
            );
        } else {
            assert_eq!(String::from_utf8(output.stdout).unwrap(), blocks.join("\n"));
        }
    }
    env.rimz().args(["lsp", "def", "alias", "x.py:1:1"]).assert().code(3).stderr("").stdout(format!(
        "==> alias <==\n/fixture/definition.rs:1:1\n\n==> x.py:1:1 <==\nerror: no language server for {} (not running); use grep\n",
        env.project_root.display()
    ));
    env.rimz().args(["lsp", "find", "work", "--limit", "1"]).assert().success().stdout("4 symbols (showing 1)\nfunction w::work  src/w.rs:1:1\n3 more; add --limit N or --all\n");
    for verb in ["refs", "impl", "callers", "callees", "find"] {
        for flags in [
            &["--limit", "1", "--json"][..],
            &["--all", "--json"],
            &["--limit", "1", "--all"],
            &["--limit", "0"],
        ] {
            env.rimz()
                .args(["lsp", verb, "work"])
                .args(flags)
                .assert()
                .code(2);
        }
    }
    for verb in ["def", "hover", "symbols"] {
        for flags in [&["--limit", "1"][..], &["--all"]] {
            env.rimz()
                .args(["lsp", verb, "work"])
                .args(flags)
                .assert()
                .code(2);
        }
    }
    let output = env
        .rimz()
        .args(["lsp", "find", "work", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        4
    );
    env.rimz().args(["lsp", "find", "work", "--all"]).assert().success().stdout("function w::work  src/w.rs:1:1\nfunction w::worker  src/w.rs:1:1\nfunction r::rework  src/r.rs:1:1\nfunction u::unrelated  src/u.rs:1:1\n");
    for args in [
        &["lsp", "find", "work"][..],
        &["lsp", "def", "nosuch"],
        &["lsp", "def", "alias", "nosuch"],
        &["lsp", "list", "--json"],
    ] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let output = env
            .rimz()
            .args(args)
            .stdout(Stdio::from(writer))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{args:?}");
        assert!(output.stderr.is_empty(), "{args:?}: {output:?}");
    }
    env.rimz()
        .args(["lsp", "callees", "lib.rs:1:4", "--external", "--json"])
        .assert()
        .success()
        .stdout("[]\n");
    for verb in ["def", "hover", "symbols"] {
        env.rimz()
            .args(["lsp", verb, "lib.rs:1:4", "--external"])
            .assert()
            .code(2);
    }
    for verb in ["refs", "impl", "find"] {
        env.rimz()
            .args(["lsp", verb, "lib.rs:1:4", "--external"])
            .assert()
            .success()
            .stdout("no results\n");
    }
    let mut owner = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let owner_pid = owner.id();
    assert_eq!(
        rpc(
            json!({"op": "lease", "launch_id": "reaped", "pid": owner_pid, "start_token": rimz::proc::process_start_token(owner_pid).unwrap()})
        )["ok"],
        true
    );
    owner.kill().unwrap();
    owner.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(7);
    while rpc(json!({"op": "status"}))["leases"]
        .as_array()
        .unwrap()
        .len()
        != 1
    {
        assert!(Instant::now() < deadline, "dead lease was not reaped");
        std::thread::sleep(Duration::from_millis(25));
    }
    env.rimz().args(["lsp", "stop"]).assert().success();
    let wait_dormant = |reason: &str, timeout: Duration| {
        let deadline = Instant::now() + timeout;
        loop {
            let status = rpc(json!({"op": "status"}));
            if status["state"]["dormant"]["reason"] == reason && status["server_pid"].is_null() {
                return status;
            }
            assert!(Instant::now() < deadline, "not dormant: {status}");
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    wait_dormant("stopped by hand", Duration::from_secs(3));
    env.rimz()
        .args(["lsp", "find", "anything", "--json"])
        .assert()
        .success()
        .stdout("[]\n");
    let status = rpc(json!({"op": "status"}));
    assert_eq!(status["restarts"], 1);
    assert_ne!(status["server_pid"], server_pid);
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(status["server_pid"].as_i64().unwrap() as i32),
        nix::sys::signal::Signal::SIGKILL,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let status = wait_dormant("crashed", Duration::from_secs(1));
        if !status["last_crash"].is_null() || Instant::now() >= deadline {
            assert_eq!(status["last_crash"]["signal"], 9);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(query("ready")["result"], json!([]));
    let restarted = rpc(json!({"op": "status"}));
    assert_eq!(restarted["restarts"], 2);
    assert!(restarted["last_crash"].is_null());
    assert_eq!(
        rpc(json!({"op": "stop", "reason": "checkout removed"}))["ok"],
        true
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    while broker.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "terminal stop did not exit with a live lease"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(!directory.exists());

    let history = std::fs::read_to_string(env.rimz_home().join("lsp-history.jsonl")).unwrap();
    let record: Value = serde_json::from_str(history.lines().last().unwrap()).unwrap();
    assert_eq!(record["project"], json!(request.project));
    assert_eq!(record["root"], json!(request.root));
    let records: Vec<Value> = history
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0]["reason"], "stopped by hand");
    assert_eq!(records[1]["reason"], "crashed");
    assert!(records[0]["dormant_ms"].is_null());
    assert!(records[1]["dormant_ms"].is_u64());
    assert!(records[2]["dormant_ms"].is_u64());

    let mut request = request;
    request.policy.idle_timeout = "2s".into();
    let (mut broker, _) = spawn_test_broker(&env, &request);
    assert_eq!(query("ready")["result"], json!([]));
    assert_eq!(query("slow")["result"], json!([]));
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let status = rpc(json!({"op": "status"}));
        if status["state"]["dormant"]["reason"] == "idle" && status["server_pid"].is_null() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "idle server stayed running: {status}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(query("ready")["result"], json!([]));
    assert_eq!(rpc(json!({"op": "status"}))["restarts"], 1);
    assert_eq!(
        rpc(json!({"op": "stop", "reason": "checkout removed"}))["ok"],
        true
    );
    broker.wait().unwrap();
    request.policy.reserve_min = "1000000G".into();
    let (mut broker, _) = spawn_test_broker(&env, &request);
    env.rimz()
        .args(["lsp", "find", "anything"])
        .assert()
        .code(3)
        .stderr(predicates::str::contains("not started: memory short"));
    let status = rpc(json!({"op": "status"}));
    assert!(status["state"].get("dormant").is_some());
    assert!(status["server_pid"].is_null());
    let output = env.rimz().args(["doctor", "--json"]).output().unwrap();
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["lsp"]["ready"]["last_refusal"]["event"], "refused");
    assert!(report["lsp"]["ready"]["last_refusal"]["details"]["estimate_bytes"].is_u64());
    assert_eq!(
        rpc(json!({"op": "stop", "reason": "checkout removed"}))["ok"],
        true
    );
    broker.wait().unwrap();
}

#[test]
fn lsp_project_servers_are_inert_until_trusted_and_overlay_whole_entries() {
    let env = Env::new();
    env.write_config(&env.project_root, "[lsp.servers.rust]\ncommand = ['project-ra']\nextensions = ['rs']\nroot-markers = ['Cargo.toml']");
    let machine: MachineConfig = toml::from_str("[lsp.servers.rust]\ncommand = ['machine-ra']\nextensions = ['rs']\nroot-markers = ['Cargo.toml']\ninit-options = { checkOnSave = false }").unwrap();
    let config = effective::load_with_roots(&machine, &env.project_root, &env.rimz_home()).unwrap();
    assert_eq!(config.untrusted_lsp_servers, ["rust"]);
    assert_eq!(config.lsp_servers["rust"].command, ["machine-ra"]);
    env.rimz().args(["trust", "grant"]).assert().success();
    let config = effective::load_with_roots(&machine, &env.project_root, &env.rimz_home()).unwrap();
    assert!(config.untrusted_lsp_servers.is_empty());
    assert_eq!(config.lsp_servers["rust"].command, ["project-ra"]);
    assert!(config.lsp_servers["rust"].init_options.is_none());
}

fn start_stub_broker(
    env: &Env,
    project: std::path::PathBuf,
) -> (
    std::process::Child,
    std::path::PathBuf,
    rimz::lsp::admission::ServeRequest,
) {
    use rimz::lsp::{admission::ServeRequest, registry};
    use serde_json::json;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let stub = crate::common::cargo_bin("lsp-server-stub", env!("CARGO_BIN_EXE_lsp-server-stub"));
    let config: rimz::config::LspServerConfig = serde_json::from_value(
        json!({"command": [stub], "extensions": ["rs"], "root-markers": ["Cargo.toml"], "memory-estimate": "1M"}),
    )
    .unwrap();
    let mut machine = MachineConfig::default();
    machine.lsp.servers.insert("rust".into(), config.clone());
    std::fs::create_dir_all(env.rimz_home()).unwrap();
    std::fs::write(
        env.rimz_home().join("config.toml"),
        toml::to_string(&std::collections::BTreeMap::from([("lsp", &machine.lsp)])).unwrap(),
    )
    .unwrap();
    let request = ServeRequest {
        root: env.project_root.canonicalize().unwrap(),
        project,
        server: "rust".into(),
        settings_hash: rimz::lsp::history::settings_hash(&config),
        config,
        policy: rimz::config::LspConfig {
            kill_floor_percent: 0,
            reserve_percent: 0,
            reserve_min: "0".into(),
            ..Default::default()
        },
        eager: false,
    };
    let directory = env
        .runtime_root
        .join("rimz/lsp")
        .join(registry::key(&request.root, "rust").unwrap());
    let mut broker = env
        .rimz()
        .args([
            "lsp",
            "serve",
            "--request",
            &serde_json::to_string(&request).unwrap(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !directory.join("entry.json").exists() {
        assert!(
            broker.try_wait().unwrap().is_none(),
            "broker failed before publishing"
        );
        assert!(Instant::now() < deadline, "broker startup deadline");
        std::thread::sleep(Duration::from_millis(20));
    }
    (broker, directory, request)
}

#[test]
fn lsp_crash_preserves_exit_and_stderr_in_status_and_diagnostics() {
    use predicates::prelude::*;
    let env = Env::new();
    let request = rimz::lsp::admission::ServeRequest {
        root: env.project_root.canonicalize().unwrap(),
        project: env.project_root.clone(),
        server: "rust".into(),
        settings_hash: "crash-test".into(),
        config: serde_json::from_value(json!({
            "command": ["sh", "-c", "read -r header; printf 'initialize failed\\n' >&2; exit 1"],
            "extensions": ["rs"], "root-markers": ["Cargo.toml"], "memory-estimate": "1M"
        }))
        .unwrap(),
        policy: rimz::config::LspConfig {
            kill_floor_percent: 0,
            reserve_percent: 0,
            reserve_min: "0".into(),
            ..Default::default()
        },
        eager: false,
    };
    let (mut broker, directory) = spawn_test_broker(&env, &request);
    editor_query(&directory, "anything");
    let deadline = Instant::now() + Duration::from_secs(3);
    let entry = loop {
        let entry = editor_rpc(&directory, json!({"op": "status"}));
        if !entry["last_crash"].is_null() || Instant::now() >= deadline {
            break entry;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(entry["last_crash"]["exit_code"], 1);
    assert_eq!(entry["last_crash"]["stderr_tail"], "initialize failed\n");
    assert!(entry["last_crash"]["signal"].is_null());
    let output = env
        .rimz()
        .args(["lsp", "status", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["last_crash"], entry["last_crash"]);
    env.rimz()
        .args(["lsp", "status"])
        .assert()
        .success()
        .stdout(
            predicates::str::contains("last crash")
                .and(predicates::str::contains("exit code 1"))
                .and(predicates::str::contains("initialize failed")),
        );
    let log = std::fs::read_to_string(env.rimz_home().join("logs/lsp.log.jsonl")).unwrap();
    let record = log
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|record| record["event"] == "crashed")
        .unwrap();
    for field in ["at_ms", "exit_code", "signal", "stderr_tail", "error"] {
        assert_eq!(record["details"][field], entry["last_crash"][field]);
    }
    editor_rpc(
        &directory,
        json!({"op": "stop", "reason": "checkout removed"}),
    );
    assert!(broker.wait().unwrap().success());
}

pub(super) fn spawn_test_broker(
    env: &Env,
    request: &rimz::lsp::admission::ServeRequest,
) -> (std::process::Child, std::path::PathBuf) {
    use std::time::{Duration, Instant};
    let directory = env
        .runtime_root
        .join("rimz/lsp")
        .join(rimz::lsp::registry::key(&request.root, &request.server).unwrap());
    let mut broker = env
        .rimz()
        .args([
            "lsp",
            "serve",
            "--request",
            &serde_json::to_string(request).unwrap(),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !directory.join("entry.json").exists() {
        assert!(broker.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    (broker, directory)
}

#[test]
fn lsp_machine_policy_in_project_is_refused_even_before_trust() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(project.path().join(".rimz")).unwrap();
    std::fs::write(
        project.path().join(".rimz/config.toml"),
        "[lsp]\nreserve-percent = 20",
    )
    .unwrap();
    let error = effective::load_with_roots(&MachineConfig::default(), project.path(), home.path())
        .err()
        .expect("reject project policy");
    assert!(
        error.to_string().contains(
            "project config cannot set lsp.reserve-percent; move it to ~/.rimz/config.toml"
        )
    );
}

#[test]
fn lsp_sweep_removes_reused_pid_but_keeps_live_broker() {
    use rimz::lsp::registry::{self, Entry, State};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::time::{Duration, Instant};

    let runtime = tempfile::tempdir().unwrap();
    let pid = std::process::id();
    let entry = Entry {
        kind: None,
        editor_check_on_save: None,
        root: runtime.path().join("checkout"),
        project: Some(runtime.path().join("project")),
        server: "live".into(),
        nonce: "nonce".into(),
        broker_pid: pid,
        broker_start_token: rimz::proc::process_start_token(pid).unwrap(),
        server_pid: None,
        server_start_token: None,
        state: State::Dormant {
            reason: Some(rimz::lsp::registry::StopReason::MemoryPressure),
            since_ms: 100,
        },
        started_at_ms: 0,
        ready_at_ms: None,
        estimate_bytes: 8000,
        settings_hash: "hash".into(),
        request_count: 0,
        last_request_at_ms: None,
        peak_rss_kb: 5,
        restarts: 0,
        last_crash: None,
        leases: Vec::new(),
        attached: Vec::new(),
    };
    let live_dir = runtime
        .path()
        .join(registry::key(&entry.root, &entry.server).unwrap());
    rimz::disk::atomic::write_temp_then_rename_cache(&live_dir.join("entry.json"), &entry).unwrap();
    let mut dead = entry.clone();
    dead.server = "dead".into();
    dead.broker_start_token = "not-the-current-process".into();
    let dead_dir = runtime
        .path()
        .join(registry::key(&dead.root, &dead.server).unwrap());
    rimz::disk::atomic::write_temp_then_rename_cache(&dead_dir.join("entry.json"), &dead).unwrap();
    let listener = UnixListener::bind(live_dir.join("sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let started = Instant::now();
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && started.elapsed() < Duration::from_secs(5) =>
                {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("fake broker accept: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&line).unwrap()["op"],
            "hello"
        );
        stream.write_all(b"{\"nonce\":\"nonce\"}\n").unwrap();
    });
    let entries = registry::testkit::sweep(runtime.path()).unwrap();
    server.join().unwrap();
    assert_eq!(entries, std::slice::from_ref(&entry));
    assert!(live_dir.exists());
    assert!(!dead_dir.exists());
    assert_eq!(
        registry::testkit::sweep(runtime.path()).unwrap(),
        [entry],
        "an unavailable socket must not orphan a live broker"
    );
}

#[test]
fn lsp_required_queue_orders_waiters_and_reaps_dead_owners() {
    let directory = tempfile::tempdir().unwrap();
    let pid = std::process::id();
    let record = serde_json::json!({"need_bytes": 8000, "pid": pid, "start_token": rimz::proc::process_start_token(pid).unwrap()});
    let newer = directory
        .path()
        .join("00000000000000000020-00000000-0000-0000-0000-000000000020");
    let older = directory
        .path()
        .join("00000000000000000010-00000000-0000-0000-0000-000000000010");
    let dead = directory
        .path()
        .join("00000000000000000001-00000000-0000-0000-0000-000000000001");
    std::fs::write(
        directory
            .path()
            .join("00000000000000000001-00000000-0000-0000-0000-000000000001.tmp.1.nonce"),
        "partial",
    )
    .unwrap();
    for path in [&newer, &older] {
        rimz::disk::atomic::write_temp_then_rename_cache(path, &record).unwrap();
    }
    let mut dead_record = record.clone();
    dead_record["start_token"] = serde_json::json!("not-this-process");
    rimz::disk::atomic::write_temp_then_rename_cache(&dead, &dead_record).unwrap();
    assert_eq!(
        rimz::lsp::admission::testkit::queue_order(directory.path()).unwrap(),
        [older.clone(), newer.clone()]
    );
    assert!(!dead.exists());
    std::fs::remove_file(older).unwrap();
    assert_eq!(
        rimz::lsp::admission::testkit::queue_order(directory.path()).unwrap(),
        [newer]
    );
}

#[test]
fn lsp_admission_warns_for_untrusted_and_optional_configuration_once() {
    use rimz::lsp::admission::{AdmissionRequest, WaitQueue, admit_launch};

    let env = Env::new();
    std::fs::write(env.project_root.join("Cargo.toml"), "").unwrap();
    let machine: MachineConfig = toml::from_str("[lsp.servers.rust]\ncommand = ['/no-such-language-server/rust-analyzer']\nextensions = ['rs']\nroot-markers = ['Cargo.toml']").unwrap();
    let runtime = env.runtime_paths();
    let request = AdmissionRequest {
        root: &env.project_root,
        project: &env.project_root,
        servers: &machine.lsp.servers,
        untrusted_servers: &["rust".to_owned()],
        policy: &machine.lsp,
        runtime: &runtime,
    };
    let mut queue = WaitQueue::default();
    let result = admit_launch(&request, &mut queue).expect("optional configuration must warn");
    assert_eq!(result.ignored_untrusted.len(), 1);
    assert!(result.ignored_untrusted[0].contains("rust"));
    assert!(result.ignored_untrusted[0].contains("ignored until trusted; run rimz trust"));
    assert_eq!(result.startup_refused.len(), 1);
    assert!(result.startup_refused[0].contains("install it or remove [lsp.servers.rust]"));
    assert!(result.admitted.is_empty());
    assert!(result.wait_for_required.is_empty());
    let records = rimz::diag::lsp::recent();
    for message in result
        .ignored_untrusted
        .iter()
        .chain(&result.startup_refused)
    {
        assert_eq!(
            records
                .iter()
                .filter(
                    |record| record.root == env.project_root.canonicalize().unwrap()
                        && record.event == "refused"
                        && record.details["reason"] == *message
                )
                .count(),
            1
        );
    }
    let second = admit_launch(&request, &mut queue).unwrap();
    assert!(second.ignored_untrusted.is_empty());
    assert!(second.startup_refused.is_empty());
    assert_eq!(
        rimz::diag::lsp::recent()
            .iter()
            .filter(|record| record.root == env.project_root.canonicalize().unwrap())
            .count(),
        2
    );
    assert!(
        !rimz::lsp::registry::directory(&env.project_root.canonicalize().unwrap(), "rust")
            .unwrap()
            .exists()
    );
}

#[test]
fn lsp_admission_policy_errors_warn_unless_required_markers_match() {
    use rimz::lsp::admission::{AdmissionRequest, WaitQueue, admit_launch};

    let env = Env::new();
    std::fs::write(env.project_root.join("Cargo.toml"), "").unwrap();
    let mut machine: MachineConfig = toml::from_str("[lsp]\nidle-timeout = 'tomorrow'\n[lsp.servers.rust]\ncommand = ['/bin/true']\nextensions = ['rs']\nroot-markers = ['Cargo.toml']").unwrap();
    let runtime = env.runtime_paths();
    for required in [false, true] {
        machine.lsp.servers.get_mut("rust").unwrap().policy = if required {
            rimz::config::LspPolicy::Required
        } else {
            rimz::config::LspPolicy::Optional
        };
        for matched in [false, true] {
            machine.lsp.servers.get_mut("rust").unwrap().root_markers =
                vec![if matched { "Cargo.toml" } else { "absent" }.into()];
            let request = AdmissionRequest {
                root: &env.project_root,
                project: &env.project_root,
                servers: &machine.lsp.servers,
                untrusted_servers: &[],
                policy: &machine.lsp,
                runtime: &runtime,
            };
            let mut queue = WaitQueue::default();
            let result = admit_launch(&request, &mut queue);
            if required && matched {
                assert!(matches!(result, Err(rimz::lsp::LspErr::Configuration(_))));
                continue;
            }
            let result = result.expect("policy errors without matching required servers must warn");
            assert_eq!(result.startup_refused.len(), 1);
            assert!(result.startup_refused[0].contains("idle-timeout"));
            assert!(result.admitted.is_empty());
            assert!(result.wait_for_required.is_empty());
            assert!(
                admit_launch(&request, &mut queue)
                    .unwrap()
                    .startup_refused
                    .is_empty()
            );
        }
    }
}

#[test]
fn lsp_admission_optional_entry_errors_are_isolated() {
    use rimz::lsp::admission::{AdmissionRequest, WaitQueue, admit_launch};
    let env = Env::new();
    std::fs::write(env.project_root.join("Cargo.toml"), "").unwrap();
    let runtime = env.runtime_paths();
    let base = json!({"command":["/bin/true"],"extensions":["rs"],"root-markers":["Cargo.toml"]});
    for (field, value, expected) in [
        ("command", json!([]), "needs command"),
        ("extensions", json!([]), "needs command"),
        ("root-markers", json!([]), "needs command"),
        ("root-markers", json!(["../Cargo.toml"]), "must be relative"),
        ("init-options", json!(1), "must be a table"),
        ("memory-estimate", json!("bad"), "size"),
        ("wait-timeout", json!("bad"), "duration"),
    ] {
        let mut config = base.clone();
        config[field] = value;
        let mut machine = MachineConfig::default();
        machine
            .lsp
            .servers
            .insert("rust".into(), serde_json::from_value(config).unwrap());
        let request = AdmissionRequest {
            root: &env.project_root,
            project: &env.project_root,
            servers: &machine.lsp.servers,
            untrusted_servers: &[],
            policy: &machine.lsp,
            runtime: &runtime,
        };
        let result = admit_launch(&request, &mut WaitQueue::default())
            .expect("optional entry errors must warn");
        assert_eq!(result.startup_refused.len(), 1, "{field}");
        assert!(result.startup_refused[0].contains(expected), "{field}");
        assert!(result.admitted.is_empty());
    }
    let mut machine = MachineConfig::default();
    machine.lsp.servers.insert(
        "rust analyzer".into(),
        serde_json::from_value(base).unwrap(),
    );
    let request = AdmissionRequest {
        root: &env.project_root,
        project: &env.project_root,
        servers: &machine.lsp.servers,
        untrusted_servers: &[],
        policy: &machine.lsp,
        runtime: &runtime,
    };
    let result = admit_launch(&request, &mut WaitQueue::default())
        .expect("an optional entry with an invalid name must warn");
    assert_eq!(
        result.startup_refused,
        [
            "language server rust analyzer: server names must contain only letters, numbers, - or _; rename [lsp.servers.rust analyzer]"
        ]
    );
}

#[test]
fn lsp_attach_optional_configuration_error_exits_three() {
    let env = Env::new();
    std::fs::write(env.project_root.join("Cargo.toml"), "").unwrap();
    std::fs::write(env.rimz_home().join("config.toml"), "[lsp.servers.rust]\ncommand = ['/no-such-language-server/rust-analyzer']\nextensions = ['rs']\nroot-markers = ['Cargo.toml']").unwrap();
    let mut child = env
        .rimz()
        .args(["lsp", "attach", "--server", "rust", "--stdio"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    rimz::lsp::protocol::write_frame(&mut child.stdin.take().unwrap(), &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":url::Url::from_directory_path(&env.project_root).unwrap().to_string()}})).unwrap();
    child
        .wait_with_output()
        .unwrap()
        .assert()
        .code(3)
        .stderr(predicates::str::contains(
            "install it or remove [lsp.servers.rust]",
        ));
}

#[test]
fn lsp_admission_required_missing_program_refuses_before_spawn() {
    use rimz::lsp::admission::{AdmissionRequest, WaitQueue, admit_launch};
    let env = Env::new();
    std::fs::write(env.project_root.join("Cargo.toml"), "").unwrap();
    let machine: MachineConfig = toml::from_str("[lsp.servers.rust]\ncommand = ['/no-such-language-server/rust-analyzer']\nextensions = ['rs']\nroot-markers = ['Cargo.toml']\npolicy = 'required'").unwrap();
    let runtime = env.runtime_paths();
    let request = AdmissionRequest {
        root: &env.project_root,
        project: &env.project_root,
        servers: &machine.lsp.servers,
        untrusted_servers: &[],
        policy: &machine.lsp,
        runtime: &runtime,
    };
    let error = admit_launch(&request, &mut WaitQueue::default())
        .err()
        .expect("missing server must refuse");
    assert!(
        error
            .to_string()
            .contains("install it or remove [lsp.servers.rust]")
    );
}

#[test]
fn lsp_required_launch_has_one_queue_slot_for_all_its_servers() {
    use rimz::lsp::admission::testkit::{enqueue_servers, queue_order};

    let directory = tempfile::tempdir().unwrap();
    let older = enqueue_servers(directory.path(), &[8000, 4000]).unwrap();
    let older_paths = queue_order(directory.path()).unwrap();
    assert_eq!(
        older_paths.len(),
        1,
        "one queue file per launch, not per server"
    );
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&older_paths[0]).unwrap()).unwrap();
    assert_eq!(record["need_bytes"], 12000);
    let newer = enqueue_servers(directory.path(), &[2000]).unwrap();
    let paths = queue_order(directory.path()).unwrap();
    assert_eq!(paths.len(), 2);
    assert_eq!(paths[0], older_paths[0]);
    drop(older);
    assert_eq!(queue_order(directory.path()).unwrap(), [paths[1].clone()]);
    drop(newer);
    assert!(queue_order(directory.path()).unwrap().is_empty());
}
