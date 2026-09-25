//! One checkout's language server, shared through a nonce-checked local socket.

mod lifecycle;
mod socket;
mod transport;
mod watch;
mod watchdog;

use super::{LspErr, Result, admission::ServeRequest, history, memory, registry};
use lifecycle::{Lifecycle, Readiness};
use notify::Watcher;
use registry::{Entry, State};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};
use transport::Transport;

struct Shared {
    model: Mutex<Model>,
    changed: Condvar,
    started: Instant,
}

struct Model {
    entry: Entry,
    lifecycle: Lifecycle,
    readiness: Readiness,
    transport: Option<Arc<Transport>>,
    request_phase: RequestPhase,
}

#[derive(PartialEq, Eq)]
enum RequestPhase {
    Serving,
    Closing,
}

impl Shared {
    fn elapsed(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn stop(&self, reason: &str) {
        let mut model = self.model.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(model.entry.state, State::Stopped { .. }) {
            model.entry.state = State::Stopped {
                reason: reason.into(),
                at_ms: crate::utils::time::unix_now_ms(),
            };
        }
        self.changed.notify_all();
    }
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Run the detached broker. Initial publication deliberately does not take admission.lock.
pub fn serve(mut request: ServeRequest) -> Result<()> {
    let root = std::fs::canonicalize(&request.root)?;
    request.root = root.clone();
    let program = request
        .config
        .command
        .first()
        .ok_or_else(|| LspErr::Configuration("empty server command".into()))?;
    nix::unistd::setsid().map_err(std::io::Error::from)?;
    let pid = std::process::id();
    let entry = Entry {
        root: root.clone(),
        server: request.server.clone(),
        nonce: uuid::Uuid::now_v7().to_string(),
        broker_pid: pid,
        broker_start_token: crate::proc::process_start_token(pid)
            .ok_or_else(|| LspErr::Protocol("broker has no process start token".into()))?,
        server_pid: None,
        server_start_token: None,
        state: State::Starting,
        started_at_ms: crate::utils::time::unix_now_ms(),
        ready_at_ms: None,
        estimate_bytes: request.estimate_bytes,
        settings_hash: request.settings_hash.clone(),
        request_count: 0,
        last_request_at_ms: None,
        peak_rss_kb: 0,
        leases: Vec::new(),
    };
    let directory = registry::directory(&root, &request.server)?;
    crate::disk::paths::ensure_private_runtime_dir(&directory)?;
    let path = directory.join("sock");
    let listener = UnixListener::bind(&path)?;
    let _socket = crate::sock::SocketGuard::new(path.clone());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let shared = Arc::new(Shared {
        model: Mutex::new(Model {
            entry,
            lifecycle: Lifecycle::default(),
            readiness: Readiness::default(),
            transport: None,
            request_phase: RequestPhase::Serving,
        }),
        changed: Condvar::new(),
        started: Instant::now(),
    });
    socket::listen(listener, shared.clone());
    registry::publish(&shared.model.lock().unwrap_or_else(|e| e.into_inner()).entry)?;

    let result = (|| {
        let mut server = Server(
            Command::new(program)
                .args(&request.config.command[1..])
                .current_dir(&root)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()?,
        );
        memory::raise_oom_score(server.0.id())?;
        {
            let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
            model.entry.server_pid = Some(server.0.id());
            model.entry.server_start_token = crate::proc::process_start_token(server.0.id());
            registry::publish(&model.entry)?;
        }
        let uri = url::Url::from_directory_path(&root)
            .map_err(|()| LspErr::Protocol("invalid checkout URI".into()))?;
        let folders = json!([{"uri": uri.as_str(), "name": root.file_name().unwrap_or_default().to_string_lossy()}]);
        let options = request.config.init_options.clone().unwrap_or(Value::Null);
        let (progress_tx, progress) = mpsc::channel();
        // Piped handles were requested on this child and have not yet been taken.
        let transport = Transport::start(
            server.0.stdout.take().expect("piped stdout"),
            server.0.stdin.take().expect("piped stdin"),
            options.clone(),
            folders.clone(),
            progress_tx,
        );
        let (_, initialized) = transport.request("initialize", json!({
            "processId": pid, "rootUri": uri.as_str(), "workspaceFolders": folders,
            "initializationOptions": options,
            "capabilities": {"window": {"workDoneProgress": true}, "workspace": {"configuration": true, "workspaceFolders": true, "didChangeWatchedFiles": {"dynamicRegistration": true}}, "textDocument": {"documentSymbol": {"hierarchicalDocumentSymbolSupport": true}}}
        }))?;
        shared
            .model
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .transport = Some(transport.clone());
        let (watch_tx, events) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = watch_tx.send(event);
        })
        .map_err(|error| LspErr::Protocol(error.to_string()))?;
        watcher
            .watch(&root, notify::RecursiveMode::Recursive)
            .map_err(|error| LspErr::Protocol(error.to_string()))?;
        run(
            &shared,
            &request,
            &mut server,
            &transport,
            initialized,
            progress,
            events,
        )
    })();
    if let Err(error) = result {
        tracing::warn!(%error, "language server stopped");
        shared.stop("crashed");
    }
    {
        // A fallback kill and its history record finish before the broker adopts their tombstone.
        let _lock = registry::lock()?;
        let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = registry::read_entries()?.into_iter().find(|entry| {
            entry.nonce == model.entry.nonce && matches!(entry.state, State::Stopped { .. })
        }) {
            model.entry.state = entry.state;
        }
        record_stop(&model.entry);
        registry::publish(&model.entry)?;
        shared.changed.notify_all();
    }
    // A tombstone remains addressable while a launch still holds a lease.
    loop {
        let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
        model.lifecycle.retain(shared.elapsed(), |lease| {
            crate::proc::process_is_live(lease.pid, Some(&lease.start_token))
        });
        model.entry.leases = model.lifecycle.leases.clone();
        if model.entry.leases.is_empty() {
            model.request_phase = RequestPhase::Closing;
            break;
        }
        registry::publish(&model.entry)?;
        drop(model);
        std::thread::sleep(Duration::from_secs(5));
    }
    let _lock = registry::lock()?;
    drop(_socket);
    std::fs::remove_dir_all(directory)?;
    Ok(())
}

fn run(
    shared: &Shared,
    request: &ServeRequest,
    server: &mut Server,
    transport: &Arc<Transport>,
    initialized: mpsc::Receiver<Result<Value>>,
    progress: mpsc::Receiver<Value>,
    events: mpsc::Receiver<notify::Result<notify::Event>>,
) -> Result<()> {
    let mut initialized = Some(initialized);
    let mut housekeeping = Instant::now();
    loop {
        if let Some(receiver) = &initialized {
            match receiver.try_recv() {
                Ok(result) => {
                    result?;
                    transport.notify("initialized", json!({}))?;
                    shared
                        .model
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .readiness
                        .initialized(shared.elapsed());
                    initialized = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(LspErr::Protocol("initialize disconnected".into()));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        {
            let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
            for event in progress.try_iter() {
                model.readiness.progress(&event);
            }
            if matches!(model.entry.state, State::Stopped { .. }) {
                break;
            }
            let state = model.readiness.state(shared.elapsed());
            if state != model.entry.state {
                if state == State::Ready {
                    model
                        .entry
                        .ready_at_ms
                        .get_or_insert(crate::utils::time::unix_now_ms());
                }
                model.entry.state = state;
                registry::publish(&model.entry)?;
                shared.changed.notify_all();
            }
        }
        if initialized.is_none() {
            let batch = events
                .try_iter()
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|error| LspErr::Protocol(error.to_string()))?;
            let changes = watch::changes(&request.root, &request.config.extensions, batch);
            if changes["changes"]
                .as_array()
                .is_some_and(|changes| !changes.is_empty())
            {
                transport.notify("workspace/didChangeWatchedFiles", changes)?;
            }
        }
        if server.0.try_wait()?.is_some() {
            shared.stop("crashed");
            break;
        }
        if housekeeping.elapsed() >= Duration::from_secs(5) {
            {
                let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
                model.lifecycle.retain(shared.elapsed(), |lease| {
                    crate::proc::process_is_live(lease.pid, Some(&lease.start_token))
                });
                model.entry.leases = model.lifecycle.leases.clone();
                model.entry.peak_rss_kb = model
                    .entry
                    .peak_rss_kb
                    .max(crate::proc::tree_totals(server.0.id()).map_or(0, |totals| totals.rss_kb));
                let reason = if !request.root.exists() {
                    Some("checkout removed")
                } else {
                    model.lifecycle.expired(shared.elapsed())
                };
                if let Some(reason) = reason {
                    model.entry.state = State::Stopped {
                        reason: reason.into(),
                        at_ms: crate::utils::time::unix_now_ms(),
                    };
                    shared.changed.notify_all();
                }
                registry::publish(&model.entry)?;
            }
            if let Err(error) = watchdog::check(request.policy.kill_floor_percent) {
                tracing::warn!(%error, "language-server pressure check failed");
            }
            housekeeping = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let transport = transport.clone();
    let (finished, shutdown) = mpsc::channel();
    // A server that stops reading stdin must not prevent its owner from killing it.
    std::thread::spawn(move || {
        if let Ok((id, response)) = transport.request("shutdown", Value::Null) {
            let _ = response.recv_timeout(Duration::from_secs(1));
            transport.cancel(id);
            let _ = transport.notify("exit", Value::Null);
        }
        let _ = finished.send(());
    });
    let _ = shutdown.recv_timeout(Duration::from_secs(1));
    Ok(())
}

fn record_stop(entry: &Entry) {
    let State::Stopped { reason, at_ms } = &entry.state else {
        return;
    };
    let mut recorded = false;
    crate::disk::rotating::visit_records(
        &crate::disk::paths::lsp_history_path(),
        |record: history::Record| {
            recorded |= record.root == entry.root
                && record.server == entry.server
                && record.settings_hash == entry.settings_hash
                && record.at_ms == *at_ms;
        },
    );
    if recorded {
        return;
    }
    history::append(&history::Record {
        at_ms: *at_ms,
        root: entry.root.clone(),
        server: entry.server.clone(),
        settings_hash: entry.settings_hash.clone(),
        peak_rss_kb: entry.peak_rss_kb,
        ready_ms: entry
            .ready_at_ms
            .map(|ready| ready.saturating_sub(entry.started_at_ms)),
        reason: reason.clone(),
    });
    if matches!(reason.as_str(), "memory pressure" | "crashed") {
        crate::diag::lsp::append(&crate::diag::lsp::Record {
            at: jiff::Timestamp::now(),
            root: entry.root.clone(),
            server: entry.server.clone(),
            event: if reason == "crashed" {
                "crashed"
            } else {
                "killed"
            }
            .into(),
            details: json!({"reason": reason, "peak_rss_kb": entry.peak_rss_kb}),
        });
    }
}
