//! One checkout's language server, shared through a nonce-checked local socket.

mod lifecycle;
mod socket;
mod transport;
mod watch;
pub(super) mod watchdog;

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
    in_flight: std::sync::atomic::AtomicUsize,
}

struct Model {
    entry: Entry,
    lifecycle: Lifecycle,
    readiness: Readiness,
    transport: Option<Arc<Transport>>,
    request_phase: RequestPhase,
    start_requested: bool,
    refusal_epoch: u64,
    stop_epoch: u64,
    refusal: Option<super::admission::Shortfall>,
    lifetime_peak_kb: u64,
    dormant_ms: Option<u64>,
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
        model.stop(reason);
        self.changed.notify_all();
    }
}

impl Model {
    fn stop(&mut self, reason: StopReason) {
        if matches!(self.entry.state, State::Stopped { .. })
            || (!reason.is_terminal() && matches!(self.entry.state, State::Dormant { .. }))
        {
            return;
        }
        let now = crate::utils::time::unix_now_ms();
        self.stop_epoch += 1;
        self.entry.state = if reason.is_terminal() {
            State::Stopped { reason, at_ms: now }
        } else {
            State::Dormant {
                since_ms: now,
                reason: Some(reason),
            }
        };
        self.entry.server_pid = None;
        self.entry.server_start_token = None;
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
    if request.config.command.is_empty() {
        return Err(LspErr::Configuration("empty server command".into()));
    }
    super::admission::idle_timeout(&request.policy)?;
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
        state: if request.eager {
            State::Starting
        } else {
            State::Dormant {
                since_ms: crate::utils::time::unix_now_ms(),
                reason: None,
            }
        },
        started_at_ms: crate::utils::time::unix_now_ms(),
        ready_at_ms: None,
        estimate_bytes: estimate(&request)?,
        settings_hash: request.settings_hash.clone(),
        request_count: 0,
        last_request_at_ms: None,
        peak_rss_kb: 0,
        restarts: 0,
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
            start_requested: false,
            refusal_epoch: 0,
            stop_epoch: 0,
            refusal: None,
            lifetime_peak_kb: 0,
            dormant_ms: None,
        }),
        changed: Condvar::new(),
        started: Instant::now(),
        in_flight: std::sync::atomic::AtomicUsize::new(0),
    });
    socket::listen(listener, shared.clone());
    registry::publish(&shared.model.lock().unwrap_or_else(|e| e.into_inner()).entry)?;

    let mut eager = request.eager;
    let mut last_housekeeping = Instant::now();
    loop {
        if last_housekeeping.elapsed() >= Duration::from_secs(5) {
            housekeeping(&shared, &request)?;
            last_housekeeping = Instant::now();
        }
        {
            let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
            if matches!(model.entry.state, State::Stopped { .. }) {
                model.request_phase = RequestPhase::Closing;
                shared.changed.notify_all();
                break;
            }
            if !eager && !model.start_requested {
                drop(model);
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            model.start_requested = false;
        }
        let admitted = prepare_start(&shared, &request, eager)?;
        eager = false;
        if !admitted {
            continue;
        }
        let result = lifetime(&shared, &request);
        if matches!(result, Ok(false)) {
            continue;
        }
        if let Err(error) = &result {
            tracing::warn!(%error, "language server stopped");
            shared.stop(StopReason::Crashed);
        }
        let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
        model.transport = None;
        model.entry.server_pid = None;
        model.entry.server_start_token = None;
        record_stop(
            &model.entry,
            model.lifetime_peak_kb,
            model.dormant_ms,
            result.as_ref().err().map(ToString::to_string).as_deref(),
        );
        registry::publish(&model.entry)?;
        shared.changed.notify_all();
    }
    let _lock = registry::lock()?;
    drop(_socket);
    std::fs::remove_dir_all(directory)?;
    Ok(())
}

fn estimate(request: &ServeRequest) -> Result<u64> {
    Ok(history::estimate(
        &request.project,
        &request.server,
        &request.settings_hash,
        crate::utils::size::parse_byte_size(&request.config.memory_estimate)
            .map_err(LspErr::Configuration)?,
    ))
}

fn prepare_start(shared: &Shared, request: &ServeRequest, eager: bool) -> Result<bool> {
    let _lock = if eager { None } else { Some(registry::lock()?) };
    let estimate = estimate(request)?;
    if !eager && let Some(shortfall) = super::admission::admit_query(request, estimate)? {
        let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
        model.start_requested = false;
        model.refusal_epoch += 1;
        model.refusal = Some(shortfall);
        shared.changed.notify_all();
        return Ok(false);
    }
    let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
    if matches!(model.entry.state, State::Stopped { .. }) {
        return Ok(false);
    }
    let now = crate::utils::time::unix_now_ms();
    model.dormant_ms = match model.entry.state {
        State::Dormant {
            since_ms,
            reason: Some(_),
        } => {
            model.entry.restarts += 1;
            Some(now.saturating_sub(since_ms))
        }
        _ => None,
    };
    model.start_requested = false;
    model.entry.state = State::Starting;
    model.entry.started_at_ms = now;
    model.entry.ready_at_ms = None;
    model.entry.estimate_bytes = estimate;
    model.lifetime_peak_kb = 0;
    model.readiness = Readiness::default();
    registry::publish(&model.entry)?;
    shared.changed.notify_all();
    Ok(true)
}

/// `Ok(false)`: a stop landed before the spawn, so no lifetime ran.
fn lifetime(shared: &Shared, request: &ServeRequest) -> Result<bool> {
    let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
    if model.entry.state != State::Starting {
        return Ok(false);
    }
    let mut server = Server(
        Command::new(&request.config.command[0])
            .args(&request.config.command[1..])
            .current_dir(&request.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()?,
    );
    model.entry.server_pid = Some(server.0.id());
    model.entry.server_start_token = crate::proc::process_start_token(server.0.id());
    model.lifetime_peak_kb = memory::tree_peak_kb(server.0.id());
    let published = registry::publish(&model.entry);
    drop(model);
    published?;
    memory::raise_oom_score(server.0.id())?;
    let uri = url::Url::from_directory_path(&request.root)
        .map_err(|()| LspErr::Protocol("invalid checkout URI".into()))?;
    let folders = json!([{"uri": uri.as_str(), "name": request.root.file_name().unwrap_or_default().to_string_lossy()}]);
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
            "processId": std::process::id(), "rootUri": uri.as_str(), "workspaceFolders": folders,
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
    watch::register(&mut watcher, &request.root, &|path, error| {
        watch_error(request, path, error)
    })?;
    let result = run(
        shared,
        request,
        &mut server,
        &transport,
        initialized,
        progress,
        (&mut watcher, events),
    );
    let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
    model.lifetime_peak_kb = model
        .lifetime_peak_kb
        .max(memory::tree_peak_kb(server.0.id()));
    model.entry.peak_rss_kb = model.entry.peak_rss_kb.max(model.lifetime_peak_kb);
    result.map(|()| true)
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
    let mut last_housekeeping = Instant::now();
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
                model.readiness.progress(&event, shared.elapsed());
            }
            if matches!(
                model.entry.state,
                State::Stopped { .. } | State::Dormant { .. }
            ) {
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
        if last_housekeeping.elapsed() >= Duration::from_secs(5) {
            housekeeping(shared, request)?;
            last_housekeeping = Instant::now();
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

fn housekeeping(shared: &Shared, request: &ServeRequest) -> Result<()> {
    let idle_timeout = super::admission::idle_timeout(&request.policy)?.as_millis() as u64;
    {
        let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
        model.lifecycle.retain(shared.elapsed(), |lease| {
            crate::proc::process_is_live(lease.pid, Some(&lease.start_token))
        });
        model.entry.leases = model.lifecycle.leases.clone();
        model.lifetime_peak_kb = model
            .lifetime_peak_kb
            .max(model.entry.server_pid.map_or(0, memory::tree_peak_kb));
        model.entry.peak_rss_kb = model.entry.peak_rss_kb.max(model.lifetime_peak_kb);
        let reason = if !request.root.exists() {
            Some(StopReason::CheckoutRemoved)
        } else if let Some(reason) = model.lifecycle.expired(shared.elapsed()) {
            Some(reason)
        } else if model.entry.state == State::Ready
            && lifecycle::idle_expired(
                crate::utils::time::unix_now_ms(),
                model.entry.ready_at_ms,
                model.entry.last_request_at_ms,
                shared.in_flight.load(std::sync::atomic::Ordering::SeqCst),
                idle_timeout,
            )
        {
            Some(StopReason::Idle)
        } else {
            None
        };
        if let Some(reason) = reason {
            model.stop(reason);
            shared.changed.notify_all();
        }
        registry::publish(&model.entry)?;
    }
    if let Err(error) = watchdog::check(request.policy.kill_floor_percent) {
        tracing::warn!(%error, "language-server pressure check failed");
    }
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

fn record_stop(entry: &Entry, peak_rss_kb: u64, dormant_ms: Option<u64>, error: Option<&str>) {
    let (reason, at_ms) = match entry.state {
        State::Stopped { reason, at_ms } => (reason, at_ms),
        State::Dormant {
            reason: Some(reason),
            since_ms,
        } => (reason, since_ms),
        _ => return,
    };
    history::append(&history::Record {
        at_ms,
        root: entry.root.clone(),
        project: entry.project.clone(),
        server: entry.server.clone(),
        settings_hash: entry.settings_hash.clone(),
        peak_rss_kb,
        ready_ms: entry
            .ready_at_ms
            .map(|ready| ready.saturating_sub(entry.started_at_ms)),
        dormant_ms,
        reason,
    });
    if reason == StopReason::Crashed {
        crate::diag::lsp::append(&crate::diag::lsp::Record {
            at: jiff::Timestamp::now(),
            root: entry.root.clone(),
            server: entry.server.clone(),
            event: "crashed".into(),
            details: json!({"reason": reason, "peak_rss_kb": peak_rss_kb, "error": error}),
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
