//! Editor stdio bridge to the checkout's shared language server.

use super::{LspErr, Result, admission, protocol, registry};
use crate::config::LspServerConfig;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

pub struct Target {
    root: PathBuf,
    project: PathBuf,
    server: String,
    servers: BTreeMap<String, LspServerConfig>,
    policy: crate::config::LspConfig,
    runtime: crate::RuntimePaths,
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "language server {} for {}",
            self.server,
            self.root.display()
        )
    }
}

impl Target {
    pub fn resolve(root: Option<&Path>, first: &Value, server: Option<&str>) -> Result<Self> {
        if first["method"] != "initialize" {
            return Err(LspErr::Protocol("expected initialize".into()));
        }
        let cwd = editor_root(root, first, &std::env::current_dir()?)?.canonicalize()?;
        let entries = registry::sweep()?;
        let checkout = registry::enclosing_checkout(&cwd, &entries);
        let workspace =
            crate::workspace::WorkspaceResolver::resolve(checkout.unwrap_or(&cwd), None)
                .map_err(|error| LspErr::Configuration(error.to_string()))?;
        let root = checkout.unwrap_or(&workspace.worktree_root).to_owned();
        let machine = crate::config::MachineConfig::load()
            .map_err(|error| LspErr::Configuration(error.to_string()))?;
        let mut effective = crate::config::effective::load(&machine, workspace.launch_repo_root())
            .map_err(|error| LspErr::Configuration(error.to_string()))?;
        if let Some(server) = effective.untrusted_lsp_servers.first() {
            return Err(LspErr::Configuration(format!(
                "project config declares language server {server} but the project is not trusted; run rimz trust"
            )));
        }
        let server = select_server(&root, &effective.lsp_servers, server)?;
        effective.lsp_servers.retain(|name, _| name == &server);
        Ok(Self {
            root,
            project: workspace.launch_repo_root().to_owned(),
            server,
            servers: effective.lsp_servers,
            policy: machine.lsp,
            runtime: crate::RuntimePaths::for_project_root(&workspace.project_root)?,
        })
    }

    /// One admission pass; the handler prints returned waits before polling again.
    pub fn admit(&self, queue: &mut admission::WaitQueue) -> Result<admission::Admitted> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let entries = registry::sweep()?;
            let entry = entries
                .into_iter()
                .find(|entry| entry.root == self.root && entry.server == self.server);
            match entry {
                Some(registry::Entry {
                    state: registry::State::Stopped { reason, .. },
                    ..
                }) => {
                    if Instant::now() >= deadline {
                        return Ok(admission::Admitted {
                            startup_refused: vec![format!("{self} is stopped: {reason}")],
                            ..Default::default()
                        });
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Some(entry) => {
                    return Ok(admission::Admitted {
                        admitted: vec![entry],
                        ..Default::default()
                    });
                }
                None => {
                    return admission::admit_launch(
                        &admission::AdmissionRequest {
                            root: &self.root,
                            project: &self.project,
                            servers: &self.servers,
                            untrusted_servers: &[],
                            policy: &self.policy,
                            runtime: &self.runtime,
                        },
                        queue,
                    );
                }
            }
        }
    }
}

pub enum Outcome {
    EditorClosed,
    EditorFailed(String),
    BrokerClosed(String),
    Refused(String),
}

/// The losing pump may still be blocked on editor I/O; the CLI exits on return.
pub fn bridge(
    entry: &registry::Entry,
    input: impl Read + Send + 'static,
    output: impl Write + Send + 'static,
    first: &Value,
) -> Result<Outcome> {
    let socket = registry::directory(&entry.root, &entry.server)?.join("sock");
    let stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        Err(error) => return Ok(Outcome::BrokerClosed(error.to_string())),
    };
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let pid = std::process::id();
    let token = crate::proc::process_start_token(pid)
        .ok_or_else(|| LspErr::Protocol("cannot identify bridge process".into()))?;
    if let Err(error) = writeln!(
        &stream,
        "{}",
        serde_json::json!({"op":"attach","pid":pid,"start_token":token})
    ) {
        return Ok(Outcome::BrokerClosed(error.to_string()));
    }
    let mut line = String::new();
    if let Err(error) = reader.by_ref().take(1024 * 1024).read_line(&mut line) {
        return Ok(Outcome::BrokerClosed(error.to_string()));
    }
    if !line.ends_with('\n') {
        return Ok(Outcome::BrokerClosed("closed during attach".into()));
    }
    let response: Value = serde_json::from_str(&line)?;
    if let Some(error) = response.get("error") {
        return Ok(Outcome::Refused(
            error["message"]
                .as_str()
                .unwrap_or("attach refused")
                .to_owned(),
        ));
    }
    if response["ok"] != true || response["nonce"] != serde_json::to_value(&entry.nonce)? {
        return Ok(Outcome::Refused("invalid attach acknowledgement".into()));
    }
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(None)?;
    if let Err(error) = protocol::write_frame(&mut &stream, first) {
        return Ok(Outcome::BrokerClosed(error.to_string()));
    }
    let (send, receive) = mpsc::channel();
    let exit_forwarded = Arc::new(Mutex::new(false));
    let editor_exit = exit_forwarded.clone();
    let mut writer = stream.try_clone()?;
    let editor_done = send.clone();
    std::thread::Builder::new()
        .name("lsp-editor-input".into())
        .spawn(move || {
            let mut input = BufReader::new(input);
            let outcome = loop {
                let frame = match input.fill_buf() {
                    Ok([]) => break Outcome::EditorClosed,
                    Ok(_) => match protocol::read_frame(&mut input) {
                        Ok(frame) => frame,
                        Err(error) => break Outcome::EditorFailed(error.to_string()),
                    },
                    Err(error) => break Outcome::EditorFailed(error.to_string()),
                };
                let exit = frame["method"] == "exit";
                // Serialize the completed exit write with the output pump's close decision.
                let mut forwarded = editor_exit.lock().unwrap_or_else(|e| e.into_inner());
                if protocol::write_frame(&mut writer, &frame).is_err() {
                    // The broker closed the socket, so the output pump reaches EOF
                    // too; its outcome carries the stop reason, which this one lacks.
                    return;
                }
                if exit {
                    *forwarded = true;
                    break Outcome::EditorClosed;
                }
            };
            let _ = editor_done.send(outcome);
        })?;
    std::thread::Builder::new()
        .name("lsp-editor-output".into())
        .spawn(move || {
            let mut output = output;
            let closed = relay(&mut reader, &mut output);
            let outcome = if *exit_forwarded.lock().unwrap_or_else(|e| e.into_inner()) {
                Outcome::EditorClosed
            } else {
                closed
            };
            let _ = send.send(outcome);
        })?;
    let outcome = receive
        .recv()
        .map_err(|_| LspErr::Protocol("bridge pumps stopped".into()))?;
    let _ = stream.shutdown(Shutdown::Both);
    Ok(outcome)
}

/// Copies broker frames to the editor until one side fails. A terminal stop
/// sends its reason as the connection's last frame, before the broker closes it.
fn relay(reader: &mut impl BufRead, output: &mut impl Write) -> Outcome {
    let mut stopped = None;
    loop {
        let frame = match reader.fill_buf() {
            Ok([]) => {
                return Outcome::BrokerClosed(
                    stopped.map_or_else(|| "closed".into(), |reason| format!("stopped: {reason}")),
                );
            }
            Ok(_) => match protocol::read_frame(reader) {
                Ok(frame) => frame,
                Err(error) => return Outcome::BrokerClosed(error.to_string()),
            },
            Err(error) => return Outcome::BrokerClosed(error.to_string()),
        };
        if frame["method"] == protocol::STOPPED {
            stopped =
                serde_json::from_value::<registry::StopReason>(frame["params"]["reason"].clone())
                    .ok();
            continue;
        }
        if let Err(error) = protocol::write_frame(output, &frame) {
            return Outcome::EditorFailed(error.to_string());
        }
    }
}

fn editor_root(root: Option<&Path>, first: &Value, cwd: &Path) -> Result<PathBuf> {
    if let Some(root) = root {
        return Ok(root.to_owned());
    }
    let uri = first["params"]["rootUri"]
        .as_str()
        .or_else(|| first["params"]["workspaceFolders"][0]["uri"].as_str());
    let Some(uri) = uri else {
        return Ok(cwd.to_owned());
    };
    url::Url::parse(uri)
        .ok()
        .and_then(|uri| uri.to_file_path().ok())
        .ok_or_else(|| LspErr::Configuration("initialize root must be a file URI".into()))
}

fn select_server(
    root: &Path,
    servers: &BTreeMap<String, LspServerConfig>,
    name: Option<&str>,
) -> Result<String> {
    if let Some(name) = name {
        if servers.contains_key(name) {
            return Ok(name.to_owned());
        }
        return Err(LspErr::Configuration(format!(
            "unknown language server {name}; choose --server NAME from the configured servers"
        )));
    }
    let mut matched = servers.iter().filter(|(_, config)| {
        config
            .root_markers
            .iter()
            .any(|marker| root.join(marker).exists())
    });
    if let Some((name, _)) = matched.next()
        && matched.next().is_none()
    {
        return Ok(name.clone());
    }
    Err(LspErr::Configuration(
        "expected one matching language server; choose --server NAME".into(),
    ))
}

pub fn install_shim(server: &str, directory: &Path) -> Result<PathBuf> {
    registry::key(directory, server)?;
    let path = directory.join(format!("rimz-lsp-{server}"));
    match std::fs::read_to_string(&path) {
        Ok(text) if text.starts_with("#!/bin/sh\n# rimz lsp attach shim\n") => {}
        Ok(_) => {
            return Err(LspErr::Configuration(format!(
                "refusing to overwrite {}: not a RimZ shim",
                path.display()
            )));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let exe = crate::proc::rimz_exe();
    let exe = exe
        .to_str()
        .ok_or_else(|| LspErr::Configuration("RimZ executable path is not UTF-8".into()))?;
    let quoted = shlex::try_quote(exe).map_err(|error| LspErr::Configuration(error.to_string()))?;
    let script = format!(
        "#!/bin/sh\n# rimz lsp attach shim\nexec {quoted} lsp attach --server {server} \"$@\"\n"
    );
    std::fs::create_dir_all(directory)?;
    crate::disk::atomic::write_executable_bytes_atomically(&path, script.as_bytes())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn root_precedence() {
        let cwd = Path::new("/cwd");
        let first = json!({"params":{"rootUri":"file:///root%20uri","workspaceFolders":[{"uri":"file:///folder"}]}});
        assert_eq!(
            editor_root(Some(Path::new("/explicit")), &first, cwd).unwrap(),
            Path::new("/explicit")
        );
        assert_eq!(
            editor_root(None, &first, cwd).unwrap(),
            Path::new("/root uri")
        );
        assert_eq!(
            editor_root(
                None,
                &json!({"params":{"rootUri":null,"workspaceFolders":[{"uri":"file:///folder"}]}}),
                cwd
            )
            .unwrap(),
            Path::new("/folder")
        );
        assert_eq!(editor_root(None, &json!({"params":{}}), cwd).unwrap(), cwd);
        assert!(
            editor_root(
                None,
                &json!({"params":{"rootUri":"https://example.com"}}),
                cwd
            )
            .is_err()
        );
    }

    #[test]
    fn relay_reports_the_stop_reason_the_broker_sent_last() {
        let reply = json!({"jsonrpc":"2.0","id":1,"result":null});
        let mut broker = Vec::new();
        protocol::write_frame(&mut broker, &reply).unwrap();
        let mut output = Vec::new();
        let closed = relay(&mut broker.clone().as_slice(), &mut output);
        assert!(matches!(closed, Outcome::BrokerClosed(reason) if reason == "closed"));
        assert_eq!(output, broker);

        protocol::write_frame(&mut broker, &json!({"jsonrpc":"2.0","method":protocol::STOPPED,"params":{"reason":"checkout removed"}})).unwrap();
        let mut output = Vec::new();
        let closed = relay(&mut broker.as_slice(), &mut output);
        assert!(
            matches!(&closed, Outcome::BrokerClosed(reason) if reason == "stopped: checkout removed")
        );
        let mut forwarded = output.as_slice();
        assert_eq!(protocol::read_frame(&mut forwarded).unwrap(), reply);
        assert!(forwarded.is_empty(), "the stop frame stays with attach");
    }

    #[test]
    fn server_selection_requires_one_matching_configuration() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Cargo.toml"), "").unwrap();
        let config: LspServerConfig = serde_json::from_value(
            json!({"command":["rust-analyzer"],"extensions":["rs"],"root-markers":["Cargo.toml"]}),
        )
        .unwrap();
        let mut servers = BTreeMap::from([("rust".into(), config.clone())]);
        assert_eq!(select_server(root.path(), &servers, None).unwrap(), "rust");
        servers.insert("another".into(), config);
        assert!(
            select_server(root.path(), &servers, None)
                .unwrap_err()
                .to_string()
                .contains("--server")
        );
        assert_eq!(
            select_server(root.path(), &servers, Some("rust")).unwrap(),
            "rust"
        );
        assert!(select_server(root.path(), &servers, Some("missing")).is_err());
        std::fs::remove_file(root.path().join("Cargo.toml")).unwrap();
        assert!(
            select_server(root.path(), &servers, None)
                .unwrap_err()
                .to_string()
                .contains("--server")
        );
    }

    #[test]
    fn shim_refuses_foreign_files_and_replaces_its_own() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("rimz-lsp-rust");
        std::fs::write(&path, "#!/bin/sh\necho unrelated\n").unwrap();
        assert!(install_shim("rust", root.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "#!/bin/sh\necho unrelated\n"
        );
        std::fs::remove_file(&path).unwrap();
        assert_eq!(install_shim("rust", root.path()).unwrap(), path);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("#!/bin/sh\n"));
        assert!(text.contains("lsp attach --server"));
        assert_eq!(install_shim("rust", root.path()).unwrap(), path);
        assert!(install_shim("../escape", root.path()).is_err());
    }
}
