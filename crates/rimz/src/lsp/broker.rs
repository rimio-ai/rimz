//! One checkout's language server, shared through a nonce-checked local socket.

mod adaptation;
mod clients;
mod lifecycle;
pub mod probe;
mod router;
mod socket;
mod transport;
mod watch;
pub(super) mod watchdog;

use super::server::{self, Server};
use super::{LspErr, Result, admission::ServeRequest, history, memory, registry};
use crate::config::LspServerKind;
use lifecycle::{Lifecycle, Readiness};
use registry::{Entry, State, StopReason};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};
use transport::Transport;

fn client_capabilities(kind: LspServerKind) -> Value {
    let text_document = json!({
        "synchronization": {"didSave": true, "willSave": true, "willSaveWaitUntil": true},
        "publishDiagnostics": {"relatedInformation": true, "versionSupport": true, "tagSupport": {"valueSet": [1, 2]}, "codeDescriptionSupport": true, "dataSupport": true},
        "completion": {"completionItem": {"snippetSupport": true, "documentationFormat": ["markdown", "plaintext"], "resolveSupport": {"properties": ["documentation", "detail", "additionalTextEdits"]}}},
        "hover": {"contentFormat": ["markdown", "plaintext"]},
        "signatureHelp": {"signatureInformation": {"documentationFormat": ["markdown", "plaintext"], "parameterInformation": {"labelOffsetSupport": true}}},
        "references": {}, "documentHighlight": {},
        "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
        "codeAction": {"codeActionLiteralSupport": {"codeActionKind": {"valueSet": ["", "quickfix", "refactor", "refactor.extract", "refactor.inline", "refactor.rewrite", "source", "source.organizeImports"]}}, "resolveSupport": {"properties": ["edit"]}, "dataSupport": true},
        "codeLens": {}, "formatting": {}, "rangeFormatting": {}, "onTypeFormatting": {},
        "rename": {"prepareSupport": true}, "foldingRange": {}, "selectionRange": {},
        "semanticTokens": {
            "requests": {"range": true, "full": {"delta": true}}, "formats": ["relative"],
            "tokenTypes": ["namespace", "type", "class", "enum", "interface", "struct", "typeParameter", "parameter", "variable", "property", "enumMember", "event", "function", "method", "macro", "keyword", "modifier", "comment", "string", "number", "regexp", "operator", "decorator"],
            "tokenModifiers": ["declaration", "definition", "readonly", "static", "deprecated", "abstract", "async", "modification", "documentation", "defaultLibrary"]
        },
        "inlayHint": {"resolveSupport": {"properties": ["tooltip", "textEdits", "label.tooltip", "label.location", "label.command"]}},
        "callHierarchy": {}, "typeHierarchy": {}, "definition": {"linkSupport": true}, "typeDefinition": {"linkSupport": true}, "implementation": {"linkSupport": true}, "declaration": {"linkSupport": true}
    });
    let mut capabilities = json!({
        "textDocument": text_document,
        "workspace": {
            "configuration": true, "workspaceFolders": true,
            "didChangeWatchedFiles": {"dynamicRegistration": true},
            "semanticTokens": {"refreshSupport": true}, "codeLens": {"refreshSupport": true}, "inlayHint": {"refreshSupport": true},
            "workspaceEdit": {"documentChanges": true, "resourceOperations": ["create", "rename", "delete"]}, "symbol": {}
        },
        "window": {"workDoneProgress": true, "showMessage": {}},
        "experimental": {
            "serverStatusNotification": true, "hoverActions": true, "codeActionGroup": true, "snippetTextEdit": true,
            "commands": {"commands": ["rust-analyzer.runSingle", "rust-analyzer.debugSingle", "rust-analyzer.showReferences", "rust-analyzer.gotoLocation", "rust-analyzer.triggerParameterHints", "rust-analyzer.rename"]}
        }
    });
    if kind != LspServerKind::RustAnalyzer {
        // The capability object above is always an object.
        capabilities
            .as_object_mut()
            .expect("capability object")
            .remove("experimental");
    }
    capabilities
}

struct Settings {
    kind: LspServerKind,
    options: Value,
    editor_check_on_save: Option<bool>,
}

impl Settings {
    fn options(&self) -> Value {
        let mut options = self.options.clone();
        if self.editor_check_on_save == Some(true) {
            options["checkOnSave"] = json!(true);
        }
        options
    }
}

struct Shared {
    settings: Arc<Mutex<Settings>>,
    router: mpsc::Sender<router::RouterEvent>,
    model: Mutex<Model>,
    changed: Condvar,
    started: Instant,
    in_flight: std::sync::atomic::AtomicUsize,
    requests: socket::Requests,
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
        // A hand stop acknowledges an alarm once serve has recorded the
        // lifetime's end (it clears the transport under the lock it reads the
        // reason with): only the reason changes, and nothing waiting on a stop
        // is woken. During teardown it is a no-op, as for any dormant entry.
        if let State::Dormant {
            reason: Some(dormant @ (StopReason::Crashed | StopReason::MemoryPressure)),
            ..
        } = &mut self.entry.state
            && reason == StopReason::StoppedByHand
            && self.transport.is_none()
        {
            *dormant = reason;
            return;
        }
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
    let kind = request.config.resolved_kind();
    let entry = Entry {
        kind: Some(kind),
        editor_check_on_save: request.config.editor_check_on_save.then_some(false),
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
        last_crash: None,
        leases: Vec::new(),
        attached: Vec::new(),
    };
    let directory = registry::directory(&root, &request.server)?;
    crate::disk::paths::ensure_private_runtime_dir(&directory)?;
    let path = directory.join("sock");
    let listener = UnixListener::bind(&path)?;
    let _socket = crate::sock::SocketGuard::new(path.clone());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let (router_tx, router_rx) = mpsc::channel();
    let shared = Arc::new(Shared {
        settings: Arc::new(Mutex::new(Settings {
            kind,
            options: request.config.init_options.clone().unwrap_or(Value::Null),
            editor_check_on_save: request.config.editor_check_on_save.then_some(false),
        })),
        router: router_tx,
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
        requests: socket::Requests::default(),
    });
    let routing = shared.clone();
    let router = std::thread::spawn(move || router::run(routing, router_rx));
    socket::listen(listener, shared.clone());
    registry::publish(&shared.model.lock().unwrap_or_else(|e| e.into_inner()).entry)?;

    let mut eager = request.eager;
    let mut lifetime_epoch = 0;
    let mut last_housekeeping = Instant::now();
    let reason = loop {
        if last_housekeeping.elapsed() >= Duration::from_secs(5) {
            housekeeping(&shared, &request)?;
            last_housekeeping = Instant::now();
        }
        {
            let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
            if let State::Stopped { reason, .. } = model.entry.state {
                // A stop that landed between lifetimes has not been published yet.
                model.request_phase = RequestPhase::Closing;
                if let Err(error) = registry::publish(&model.entry) {
                    tracing::warn!(%error, "cannot publish the terminal stop");
                }
                shared.changed.notify_all();
                break reason;
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
        lifetime_epoch += 1;
        let _ = shared
            .router
            .send(router::RouterEvent::Starting(lifetime_epoch));
        let result = lifetime(&shared, &request, lifetime_epoch);
        if let Err(error) = &result {
            tracing::warn!(%error, "language server stopped");
            shared.stop(StopReason::Crashed);
        }
        let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
        model.transport = None;
        model.entry.server_pid = None;
        model.entry.server_start_token = None;
        let reason = match model.entry.state {
            State::Stopped { reason, .. }
            | State::Dormant {
                reason: Some(reason),
                ..
            } => reason,
            _ => StopReason::Crashed,
        };
        let _ = shared.router.send(router::RouterEvent::Ended(reason));
        if matches!(result, Ok(false)) {
            continue;
        }
        if reason == StopReason::Crashed && model.entry.last_crash.is_none() {
            model.entry.last_crash = Some(registry::CrashCause {
                at_ms: crate::utils::time::unix_now_ms(),
                exit_code: None,
                signal: None,
                stderr_tail: String::new(),
                error: result.as_ref().err().map(ToString::to_string),
            });
        }
        record_stop(&model.entry, model.lifetime_peak_kb, model.dormant_ms);
        registry::publish(&model.entry)?;
        shared.changed.notify_all();
    };
    let _ = shared.router.send(router::RouterEvent::Close(reason));
    let _ = router.join();
    // Before the registry lock: the main thread holds no lock while it waits on a socket.
    let abandoned = shared.requests.drain(Duration::from_secs(5));
    if abandoned > 0 {
        tracing::warn!(abandoned, "broker exits with requests unanswered");
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
        model.refusal = Some(shortfall.clone());
        let _ = shared.router.send(router::RouterEvent::Refused(shortfall));
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
    model.entry.last_crash = None;
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
fn lifetime(shared: &Shared, request: &ServeRequest, epoch: u64) -> Result<bool> {
    let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
    if model.entry.state != State::Starting {
        return Ok(false);
    }
    let mut server = server::spawn(&request.root, &request.config)?;
    model.entry.server_pid = Some(server.child.id());
    model.entry.server_start_token = crate::proc::process_start_token(server.child.id());
    model.lifetime_peak_kb = memory::tree_peak_kb(server.child.id());
    let published = registry::publish(&model.entry);
    drop(model);
    let result = published.and_then(|()| initialize_and_run(shared, request, epoch, &mut server));
    if result.is_err() {
        shared.stop(StopReason::Crashed);
    }
    let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
    model.lifetime_peak_kb = model
        .lifetime_peak_kb
        .max(memory::tree_peak_kb(server.child.id()));
    model.entry.peak_rss_kb = model.entry.peak_rss_kb.max(model.lifetime_peak_kb);
    if matches!(
        model.entry.state,
        State::Dormant {
            reason: Some(StopReason::Crashed),
            ..
        }
    ) {
        model.entry.last_crash = Some(server.crash(result.as_ref().err().map(ToString::to_string)));
    }
    result.map(|()| true)
}

fn initialize_params(
    root: &std::path::Path,
    config: &crate::config::LspServerConfig,
) -> Result<Value> {
    let uri = url::Url::from_directory_path(root)
        .map_err(|()| LspErr::Protocol("invalid checkout URI".into()))?;
    Ok(json!({
        "processId": std::process::id(), "rootUri": uri.as_str(),
        "workspaceFolders": [{"uri": uri.as_str(), "name": root.file_name().unwrap_or_default().to_string_lossy()}],
        "initializationOptions": config.init_options.clone().unwrap_or(Value::Null),
        "capabilities": client_capabilities(config.resolved_kind())
    }))
}

fn initialize_and_run(
    shared: &Shared,
    request: &ServeRequest,
    epoch: u64,
    server: &mut Server,
) -> Result<()> {
    memory::raise_oom_score(server.child.id())?;
    let mut params = initialize_params(&request.root, &request.config)?;
    let (messages_tx, messages) = mpsc::channel();
    let sender = shared.router.clone();
    std::thread::spawn(move || {
        for frame in messages {
            if sender
                .send(router::RouterEvent::Message(epoch, frame))
                .is_err()
            {
                break;
            }
        }
    });
    // Piped handles were requested on this child and have not yet been taken.
    let transport = Transport::start(
        server.child.stdout.take().expect("piped stdout"),
        server.child.stdin.take().expect("piped stdin"),
        shared.settings.clone(),
        params["workspaceFolders"].clone(),
        messages_tx,
    );
    // Initialize from the broker's settings, the same state the transport answers configuration from.
    let settings = shared.settings.lock().unwrap_or_else(|e| e.into_inner());
    params["initializationOptions"] = settings.options();
    params["capabilities"] = client_capabilities(settings.kind);
    let initial_check_on_save = settings.editor_check_on_save;
    drop(settings);
    let (_, initialized) = transport.request("initialize", params)?;
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
    run(
        shared,
        request,
        server,
        &transport,
        (initialized, initial_check_on_save),
        (&mut watcher, events),
    )
}

fn run(
    shared: &Shared,
    request: &ServeRequest,
    server: &mut Server,
    transport: &Arc<Transport>,
    initialized: (mpsc::Receiver<Result<Value>>, Option<bool>),
    watch: (
        &mut impl notify::Watcher,
        mpsc::Receiver<notify::Result<notify::Event>>,
    ),
) -> Result<()> {
    let (watcher, events) = watch;
    let (initialized, initial_check_on_save) = initialized;
    let mut initialized = Some(initialized);
    let mut replaying = None;
    let mut last_housekeeping = Instant::now();
    loop {
        if let Some(receiver) = &initialized {
            match receiver.try_recv() {
                Ok(result) => {
                    let initialize_result = result?;
                    transport.notify("initialized", json!({}))?;
                    let (replayed, replay) = mpsc::channel();
                    let _ = shared.router.send(router::RouterEvent::Started(
                        transport.clone(),
                        initialize_result,
                        replayed,
                        initial_check_on_save,
                    ));
                    replaying = Some(replay);
                    initialized = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(LspErr::Protocol("initialize disconnected".into()));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(receiver) = &replaying {
            match receiver.try_recv() {
                Ok(()) => {
                    shared
                        .model
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .readiness
                        .initialized(shared.elapsed());
                    replaying = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(LspErr::Protocol("editor replay failed".into()));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        {
            let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
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
                shared
                    .settings
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .kind,
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
        if server.child.try_wait()?.is_some() {
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
        let model = &mut *model;
        model
            .lifecycle
            .retain(&mut model.entry.leases, shared.elapsed(), |lease| {
                crate::proc::process_is_live(lease.pid, Some(&lease.start_token))
            });
        model.lifetime_peak_kb = model
            .lifetime_peak_kb
            .max(model.entry.server_pid.map_or(0, memory::tree_peak_kb));
        model.entry.peak_rss_kb = model.entry.peak_rss_kb.max(model.lifetime_peak_kb);
        let reason = if !request.root.exists() {
            Some(StopReason::CheckoutRemoved)
        } else if let Some(reason) = model
            .lifecycle
            .expired(&model.entry.leases, shared.elapsed())
        {
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

fn record_stop(entry: &Entry, peak_rss_kb: u64, dormant_ms: Option<u64>) {
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
        let mut details = json!(entry.last_crash);
        details["reason"] = json!(reason);
        details["peak_rss_kb"] = json!(peak_rss_kb);
        crate::diag::lsp::append(&crate::diag::lsp::Record {
            at: jiff::Timestamp::now(),
            root: entry.root.clone(),
            server: entry.server.clone(),
            event: "crashed".into(),
            details,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    #[test]
    fn server_drop_kills_its_descendants() {
        let config = serde_json::from_value(json!({"command": ["sh", "-c", "sleep 60 & echo $!; wait"], "extensions": ["rs"], "root-markers": ["Cargo.toml"]})).unwrap();
        let mut server = server::spawn(std::path::Path::new("/"), &config).unwrap();
        let pid = server.child.id();
        let mut line = String::new();
        BufReader::new(server.child.stdout.take().unwrap())
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

    fn dormant_model(reason: Option<StopReason>) -> Model {
        let entry: Entry = serde_json::from_value(serde_json::json!({"root": "/checkout", "server": "rust", "nonce": "n", "broker_pid": 1, "broker_start_token": "t", "server_pid": null, "server_start_token": null, "state": {"dormant": {"since_ms": 42, "reason": reason}}, "started_at_ms": 0, "ready_at_ms": null, "estimate_bytes": 0, "settings_hash": "s", "request_count": 0, "last_request_at_ms": null, "peak_rss_kb": 0, "leases": []})).unwrap();
        Model {
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
        }
    }

    #[test]
    fn hand_stop_acknowledges_a_dormant_crash_or_memory_pressure() {
        for (dormant, stop, expected) in [
            (
                Some(StopReason::Crashed),
                StopReason::StoppedByHand,
                Some(StopReason::StoppedByHand),
            ),
            (
                Some(StopReason::MemoryPressure),
                StopReason::StoppedByHand,
                Some(StopReason::StoppedByHand),
            ),
            (
                Some(StopReason::Crashed),
                StopReason::Idle,
                Some(StopReason::Crashed),
            ),
            (
                Some(StopReason::Idle),
                StopReason::StoppedByHand,
                Some(StopReason::Idle),
            ),
            (
                Some(StopReason::Evicted),
                StopReason::StoppedByHand,
                Some(StopReason::Evicted),
            ),
            (None, StopReason::StoppedByHand, None),
        ] {
            let mut model = dormant_model(dormant);
            model.stop(stop);
            assert_eq!(
                model.entry.state,
                State::Dormant {
                    since_ms: 42,
                    reason: expected
                },
                "{stop} on dormant {dormant:?}"
            );
            assert_eq!(model.stop_epoch, 0, "a dormant entry has nothing to stop");
        }
        let mut tearing_down = dormant_model(Some(StopReason::Crashed));
        tearing_down.transport = Some(Transport::start(
            std::io::empty(),
            Vec::<u8>::new(),
            Arc::new(Mutex::new(Settings {
                kind: LspServerKind::RustAnalyzer,
                options: Value::Null,
                editor_check_on_save: None,
            })),
            Value::Null,
            mpsc::channel().0,
        ));
        tearing_down.stop(StopReason::StoppedByHand);
        assert_eq!(
            tearing_down.entry.state,
            State::Dormant {
                since_ms: 42,
                reason: Some(StopReason::Crashed)
            },
            "serve has yet to record the crash while the lifetime tears down"
        );
    }
}
