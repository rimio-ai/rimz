//! Bounded local requests; no query reaches stdio before readiness.

use super::{RequestPhase, Shared, registry};
use crate::lsp::{
    LspErr, Result,
    protocol::QueryRequest,
    registry::{LaunchId, Lease, State},
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
        launch_id: LaunchId,
        pid: u32,
        start_token: String,
    },
    Release {
        launch_id: LaunchId,
        pid: u32,
    },
    Stop {
        reason: String,
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
        shared.stop(&reason);
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
    loop {
        match &model.entry.state {
            State::Stopped { reason, .. } => {
                return Ok(json!({"error": {"code": -32003, "message": reason}}));
            }
            State::Ready => break,
            _ => {}
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(json!({"indexing": {"elapsed_ms": shared.elapsed()}}));
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
            shared.stop("crashed");
            return Ok(json!({"error": {"code": -32003, "message": "crashed"}}));
        }
    };
    let response = receiver.recv_timeout(Duration::from_secs(60));
    transport.cancel(id);
    if matches!(&response, Ok(Err(error)) if !matches!(error, LspErr::Server { .. })) {
        shared.stop("crashed");
    }
    let model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
    if let State::Stopped { reason, .. } = &model.entry.state {
        return Ok(json!({"error": {"code": -32003, "message": reason}}));
    }
    match response {
        Ok(result) => Ok(json!({"result": result?})),
        Err(error) => Err(LspErr::Protocol(format!("query did not answer: {error}"))),
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
            }),
            changed: Condvar::new(),
            started: Instant::now(),
        };
        let response = query(&shared, "workspace/symbol", json!({"query": "symbol"}), 1).unwrap();
        assert!(response.get("indexing").is_some());
        assert!(response.get("result").is_none());
        shared.stop("memory pressure");
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
    }
}
