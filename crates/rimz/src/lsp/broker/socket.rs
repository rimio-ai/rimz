//! Bounded local requests; no query reaches stdio before readiness.

use super::{RequestPhase, Shared, registry};
use crate::lsp::{
    LspErr, Result,
    protocol::QueryRequest,
    registry::{LaunchId, Lease, State, StopReason},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Operation {
    Hello,
    Status,
    Lease {
        launch_id: Option<LaunchId>,
        pid: u32,
        start_token: String,
    },
    Release {
        launch_id: Option<LaunchId>,
        pid: u32,
    },
    Stop {
        reason: StopReason,
    },
    Query {
        method: String,
        params: Value,
        wait_ms: u64,
    },
}

pub(super) fn listen(listener: UnixListener, shared: Arc<Shared>) {
    std::thread::spawn(move || {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let shared = shared.clone();
                    std::thread::spawn(move || {
                        if let Err(error) = handle(stream, &shared) {
                            tracing::debug!(%error, "LSP socket request failed");
                        }
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::Interrupted | std::io::ErrorKind::ConnectionAborted
                    ) =>
                {
                    continue;
                }
                Err(error) if matches!(error.raw_os_error(), Some(code) if code == nix::libc::EMFILE || code == nix::libc::ENFILE) =>
                {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(_) => break,
            }
        }
    });
}

fn handle(mut stream: UnixStream, shared: &Shared) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut line = String::new();
    BufReader::new(&stream)
        .take(1024 * 1024)
        .read_line(&mut line)?;
    let response = if !line.ends_with('\n') {
        Err(LspErr::Protocol("incomplete request".into()))
    } else {
        serde_json::from_str::<Operation>(&line)
            .map_err(LspErr::from)
            .and_then(|operation| respond(operation, shared))
    };
    let mut response = response.unwrap_or_else(|error| match error {
        LspErr::Server { code, message } => json!({"error": {"code": code, "message": message}}),
        error => json!({"error": {"code": -32603, "message": error.to_string()}}),
    });
    response["nonce"] = json!(
        shared
            .model
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry
            .nonce
    );
    serde_json::to_writer(&mut stream, &response)?;
    stream.write_all(b"\n")?;
    Ok(())
}

fn respond(operation: Operation, shared: &Shared) -> Result<Value> {
    if let Operation::Query {
        method,
        params,
        wait_ms,
    } = operation
    {
        {
            let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
            if model.request_phase == RequestPhase::Serving {
                model.entry.request_count = model.entry.request_count.saturating_add(1);
                model.entry.last_request_at_ms = Some(crate::utils::time::unix_now_ms());
                registry::publish(&model.entry)?;
            }
        }
        return query(shared, &method, params, wait_ms);
    }
    if let Operation::Stop { reason } = operation {
        shared.stop(reason);
        let model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
        if model.request_phase == RequestPhase::Serving {
            registry::publish(&model.entry)?;
        }
        return Ok(json!({"ok": true}));
    }
    let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
    if model.request_phase == RequestPhase::Closing
        && !matches!(operation, Operation::Hello | Operation::Status)
    {
        return Err(LspErr::Protocol("broker is shutting down".into()));
    }
    match operation {
        Operation::Hello => {
            return Ok(json!({"state": model.entry.state, "elapsed_ms": shared.elapsed()}));
        }
        Operation::Status => return Ok(serde_json::to_value(&model.entry)?),
        Operation::Lease {
            launch_id,
            pid,
            start_token,
        } => {
            if !crate::proc::process_is_live(pid, Some(&start_token)) {
                return Err(LspErr::Protocol("lease process is not live".into()));
            }
            model.lifecycle.register(Lease {
                launch_id,
                pid,
                start_token,
                since_ms: crate::utils::time::unix_now_ms(),
            });
        }
        Operation::Release { launch_id, pid } => {
            model.lifecycle.retain(shared.elapsed(), |lease| {
                lease.launch_id != launch_id || lease.pid != pid
            })
        }
        Operation::Stop { .. } | Operation::Query { .. } => {
            unreachable!("handled before acquiring model")
        }
    }
    model.entry.leases = model.lifecycle.leases.clone();
    registry::publish(&model.entry)?;
    Ok(json!({"ok": true}))
}

fn query(shared: &Shared, method: &str, params: Value, wait_ms: u64) -> Result<Value> {
    let _: QueryRequest = serde_json::from_value(json!({"method": method, "params": params}))?;
    let deadline = Instant::now() + Duration::from_millis(wait_ms.min(30_000));
    let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
    let refusal_epoch = model.refusal_epoch;
    let stop_epoch = model.stop_epoch;
    shared
        .in_flight
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let _in_flight = QueryFlight(&shared.in_flight);
    if matches!(model.entry.state, State::Dormant { .. }) {
        model.start_requested = true;
        shared.changed.notify_all();
    }
    loop {
        match &model.entry.state {
            State::Stopped { reason, .. } => {
                return Ok(json!({"error": {"code": -32003, "message": reason}}));
            }
            State::Dormant {
                reason: Some(reason),
                ..
            } if model.stop_epoch > stop_epoch => {
                return Ok(json!({"error": {"code": -32003, "message": reason}}));
            }
            State::Ready => break,
            _ => {}
        }
        if model.refusal_epoch > refusal_epoch {
            return Ok(json!({"refused": model.refusal}));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(
                json!({"indexing": {"elapsed_ms": crate::utils::time::unix_now_ms().saturating_sub(model.entry.started_at_ms)}}),
            );
        }
        model = shared
            .changed
            .wait_timeout(model, remaining)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
    let transport = model
        .transport
        .clone()
        .ok_or_else(|| LspErr::Protocol("ready server has no transport".into()))?;
    drop(model);
    let (id, receiver) = match transport.request(method, params) {
        Ok(request) => request,
        Err(error) => {
            tracing::debug!(%error, "language-server transport closed");
            transport_failed(shared, &transport);
            return Ok(json!({"error": {"code": -32003, "message": StopReason::Crashed}}));
        }
    };
    let response = receiver.recv_timeout(Duration::from_secs(60));
    transport.cancel(id);
    if matches!(&response, Ok(Err(error)) if !matches!(error, LspErr::Server { .. })) {
        transport_failed(shared, &transport);
    }
    let model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
    if let State::Stopped { reason, .. }
    | State::Dormant {
        reason: Some(reason),
        ..
    } = &model.entry.state
    {
        return Ok(json!({"error": {"code": -32003, "message": reason}}));
    }
    match response {
        Ok(result) => Ok(json!({"result": result?})),
        Err(error) => Err(LspErr::Protocol(format!("query did not answer: {error}"))),
    }
}

struct QueryFlight<'a>(&'a std::sync::atomic::AtomicUsize);

impl Drop for QueryFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

fn transport_failed(shared: &Shared, transport: &Arc<super::Transport>) {
    let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
    if model
        .transport
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, transport))
    {
        model.stop(StopReason::Crashed);
        shared.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::broker::{Lifecycle, Model, Readiness};
    use crate::lsp::registry::Entry;
    use std::sync::{Condvar, Mutex};

    #[test]
    fn query_at_consumer_never_returns_empty_results_while_indexing() {
        let entry: Entry = serde_json::from_value(json!({"root": "/checkout", "server": "rust", "nonce": "n", "broker_pid": 1, "broker_start_token": "t", "server_pid": null, "server_start_token": null, "state": "indexing", "started_at_ms": 0, "ready_at_ms": null, "estimate_bytes": 0, "settings_hash": "s", "request_count": 0, "last_request_at_ms": null, "peak_rss_kb": 0, "leases": []})).unwrap();
        let shared = Shared {
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
        };
        let response = query(&shared, "workspace/symbol", json!({"query": "symbol"}), 1).unwrap();
        assert!(response.get("indexing").is_some());
        assert!(response.get("result").is_none());
        shared.stop(StopReason::CheckoutRemoved);
        assert_eq!(
            query(
                &shared,
                "workspace/symbol",
                json!({"query": "symbol"}),
                30_000
            )
            .unwrap()["error"]["code"],
            -32003
        );
        shared.model.lock().unwrap().entry.state = State::Dormant {
            since_ms: 0,
            reason: None,
        };
        let response = query(&shared, "workspace/symbol", json!({"query": "symbol"}), 0).unwrap();
        assert!(response.get("indexing").is_some());
        assert!(
            shared.model.lock().unwrap().start_requested,
            "dormant query must ask main thread to start"
        );
        let shared = Arc::new(shared);
        let waiting = shared.clone();
        let query_thread = std::thread::spawn(move || {
            query(
                &waiting,
                "workspace/symbol",
                json!({"query": "symbol"}),
                1000,
            )
            .unwrap()
        });
        let deadline = Instant::now() + Duration::from_secs(1);
        while shared.in_flight.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        {
            let mut model = shared.model.lock().unwrap();
            model.refusal_epoch += 1;
            model.refusal = Some(crate::lsp::admission::Shortfall {
                root: "/checkout".into(),
                server: "rust".into(),
                estimate_bytes: 100,
                available_bytes: 0,
                committed_bytes: 0,
                reserve_bytes: 0,
                holders: vec![],
            });
            shared.changed.notify_all();
        }
        assert_eq!(
            query_thread.join().unwrap()["refused"]["estimate_bytes"],
            100
        );
        assert_eq!(
            shared.in_flight.load(std::sync::atomic::Ordering::SeqCst),
            0
        );

        {
            let mut model = shared.model.lock().unwrap();
            model.start_requested = false;
            model.entry.state = State::Dormant {
                since_ms: 0,
                reason: Some(StopReason::Idle),
            };
        }
        let waiting = shared.clone();
        let query_thread = std::thread::spawn(move || {
            query(
                &waiting,
                "workspace/symbol",
                json!({"query": "symbol"}),
                5_000,
            )
            .unwrap()
        });
        let deadline = Instant::now() + Duration::from_secs(1);
        while !shared.model.lock().unwrap().start_requested {
            assert!(
                Instant::now() < deadline,
                "dormant entry with a reason must still wake"
            );
            std::thread::yield_now();
        }
        let asked = Instant::now();
        {
            let mut model = shared.model.lock().unwrap();
            model.entry.state = State::Starting;
            model.stop(StopReason::Crashed);
            shared.changed.notify_all();
        }
        let response = query_thread.join().unwrap();
        assert_eq!(response["error"]["code"], -32003, "{response}");
        assert_eq!(response["error"]["message"], "crashed");
        assert!(asked.elapsed() < Duration::from_secs(2));
    }
}
