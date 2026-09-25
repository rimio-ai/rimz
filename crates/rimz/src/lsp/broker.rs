//! One checkout's language server, shared through a nonce-checked local socket.

mod lifecycle;
mod socket;
mod transport;
mod watch;
mod watchdog;

use super::{LspErr, Result, admission::ServeRequest, history, memory, registry};
use lifecycle::{Lifecycle, Readiness};
use registry::{Entry, State, StopReason};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
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

    fn stop(&self, reason: StopReason) {
        let mut model = self.model.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(model.entry.state, State::Stopped { .. }) {
            model.entry.state = State::Stopped {
                reason,
                at_ms: crate::utils::time::unix_now_ms(),
            };
        }
        self.changed.notify_all();
    }
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(self.0.id() as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
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
        project: Some(request.project.clone()),
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
                .process_group(0)
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
        watch::register(&mut watcher, &root, &|path, error| {
            watch_error(&request, path, error)
        })?;
        run(
            &shared,
            &request,
            &mut server,
            &transport,
            initialized,
            progress,
            (&mut watcher, events),
        )
    })();
    if let Err(error) = &result {
        tracing::warn!(%error, "language server stopped");
        shared.stop(StopReason::Crashed);
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
        record_stop(
            &model.entry,
            result.as_ref().err().map(ToString::to_string).as_deref(),
        );
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
    watch: (
        &mut impl notify::Watcher,
        mpsc::Receiver<notify::Result<notify::Event>>,
    ),
) -> Result<()> {
    let (watcher, events) = watch;
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
            let report = |path: &std::path::Path, error: &str| watch_error(request, path, error);
            let batch = events
                .try_iter()
                .filter_map(|event| match event {
                    Ok(event) => Some(event),
                    Err(error) => {
                        report(&request.root, &error.to_string());
                        None
                    }
                })
                .collect();
            let batch = watch::register_created(watcher, &request.root, batch, &report)?;
            let changes = watch::changes(
                &request.root,
                &request.config.extensions,
                &request.config.root_markers,
                batch,
            );
            if changes["changes"]
                .as_array()
                .is_some_and(|changes| !changes.is_empty())
            {
                transport.notify("workspace/didChangeWatchedFiles", changes)?;
            }
        }
        if server.0.try_wait()?.is_some() {
            shared.stop(StopReason::Crashed);
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
                    .max(memory::tree_peak_kb(server.0.id()));
                let reason = if !request.root.exists() {
                    Some(StopReason::CheckoutRemoved)
                } else {
                    model.lifecycle.expired(shared.elapsed())
                };
                if let Some(reason) = reason {
                    model.entry.state = State::Stopped {
                        reason,
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

fn watch_error(request: &ServeRequest, path: &std::path::Path, error: &str) {
    crate::diag::lsp::append(&crate::diag::lsp::Record {
        at: jiff::Timestamp::now(),
        root: request.root.clone(),
        server: request.server.clone(),
        event: "watch_error".into(),
        details: json!({"path": path, "error": error}),
    });
}

fn record_stop(entry: &Entry, error: Option<&str>) {
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
        project: entry.project.clone(),
        server: entry.server.clone(),
        settings_hash: entry.settings_hash.clone(),
        peak_rss_kb: entry.peak_rss_kb,
        ready_ms: entry
            .ready_at_ms
            .map(|ready| ready.saturating_sub(entry.started_at_ms)),
        reason: *reason,
    });
    if *reason == StopReason::Crashed {
        crate::diag::lsp::append(&crate::diag::lsp::Record {
            at: jiff::Timestamp::now(),
            root: entry.root.clone(),
            server: entry.server.clone(),
            event: "crashed".into(),
            details: json!({"reason": reason, "peak_rss_kb": entry.peak_rss_kb, "error": error}),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    #[test]
    fn server_drop_kills_its_descendants() {
        let mut server = Server(
            Command::new("sh")
                .args(["-c", "sleep 60 & echo $!; wait"])
                .stdout(Stdio::piped())
                .process_group(0)
                .spawn()
                .unwrap(),
        );
        let pid = server.0.id();
        let mut line = String::new();
        BufReader::new(server.0.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let child: u32 = line.trim().parse().unwrap();
        let token = crate::proc::process_start_token(child).unwrap();
        drop(server);
        for _ in 0..100 {
            if !crate::proc::process_is_live(child, Some(&token)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!crate::proc::process_is_live(pid, None));
        assert!(!crate::proc::process_is_live(child, Some(&token)));
    }
}
