//! Multiplexed JSON-RPC requests over the server's single stdio connection.

use super::super::{LspErr, Result, protocol};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

type Reply = mpsc::Sender<Result<Value>>;

pub(super) struct Transport {
    writer: Mutex<Box<dyn Write + Send>>,
    pending: Mutex<BTreeMap<u64, Reply>>,
    next: AtomicU64,
    closed: AtomicBool,
}

impl Transport {
    pub(super) fn start(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        options: Value,
        folders: Value,
        progress: mpsc::Sender<Value>,
    ) -> Arc<Self> {
        let transport = Arc::new(Self {
            writer: Mutex::new(Box::new(writer)),
            pending: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
            closed: AtomicBool::new(false),
        });
        let background = transport.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(reader);
            let result = background.read(&mut reader, &options, &folders, &progress);
            let message = result
                .err()
                .map_or_else(|| "server closed".into(), |e| e.to_string());
            background.closed.store(true, Ordering::Release);
            let pending =
                std::mem::take(&mut *background.pending.lock().unwrap_or_else(|e| e.into_inner()));
            for reply in pending.into_values() {
                let _ = reply.send(Err(LspErr::Protocol(message.clone())));
            }
        });
        transport
    }

    fn send(&self, message: &Value) -> Result<()> {
        protocol::write_frame(
            &mut *self.writer.lock().unwrap_or_else(|e| e.into_inner()),
            message,
        )
    }

    pub(super) fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    pub(super) fn request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<(u64, mpsc::Receiver<Result<Value>>)> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, sender);
        if self.closed.load(Ordering::Acquire) {
            self.cancel(id);
            return Err(LspErr::Protocol("server transport closed".into()));
        }
        if let Err(error) =
            self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
        {
            self.cancel(id);
            return Err(error);
        }
        Ok((id, receiver))
    }

    pub(super) fn cancel(&self, id: u64) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
    }

    fn read(
        &self,
        reader: &mut impl std::io::BufRead,
        options: &Value,
        folders: &Value,
        progress: &mpsc::Sender<Value>,
    ) -> Result<()> {
        loop {
            let message = protocol::read_frame(reader)?;
            if let Some(method) = message.get("method").and_then(Value::as_str) {
                if let Some(id) = message.get("id") {
                    let response = server_response(method, &message["params"], options, folders);
                    let mut reply = json!({"jsonrpc": "2.0", "id": id});
                    match response {
                        Ok(result) => reply["result"] = result,
                        Err(()) => {
                            reply["error"] =
                                json!({"code": -32601, "message": "method not supported"})
                        }
                    }
                    self.send(&reply)?;
                } else if method == "$/progress" {
                    let _ = progress.send(message["params"].clone());
                }
                continue;
            }
            if let Some(id) = message.get("id").and_then(Value::as_u64)
                && let Some(reply) = self
                    .pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id)
            {
                let result = if let Some(error) = message.get("error") {
                    Err(LspErr::Server {
                        code: error["code"].as_i64().unwrap_or(-32603),
                        message: error["message"]
                            .as_str()
                            .unwrap_or("invalid server error")
                            .to_owned(),
                    })
                } else {
                    Ok(message["result"].clone())
                };
                let _ = reply.send(result);
            }
        }
    }
}

fn server_response(
    method: &str,
    params: &Value,
    options: &Value,
    folders: &Value,
) -> std::result::Result<Value, ()> {
    Ok(match method {
        "workspace/configuration" => Value::Array(
            params["items"]
                .as_array()
                .ok_or(())?
                .iter()
                .map(|item| {
                    let Some(section) = item["section"].as_str() else {
                        return options.clone();
                    };
                    // Initialization options are already scoped to the language server.
                    let section = section.strip_prefix("rust-analyzer.").unwrap_or(section);
                    if section == "rust-analyzer" {
                        return options.clone();
                    }
                    section
                        .split('.')
                        .fold(options, |value, key| &value[key])
                        .clone()
                })
                .collect(),
        ),
        "workspace/workspaceFolders" => folders.clone(),
        "client/registerCapability" | "window/workDoneProgress/create" => Value::Null,
        _ => return Err(()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_requests_receive_configuration_folders_and_acknowledgements() {
        let options = json!({"checkOnSave": false});
        let folders = json!([{"uri": "file:///checkout", "name": "checkout"}]);
        assert_eq!(
            server_response(
                "workspace/configuration",
                &json!({"items": [{"section": "rust-analyzer"}, {"section": "rust-analyzer.checkOnSave"}, {"section": "absent"}]}),
                &options,
                &folders
            ),
            Ok(json!([options, false, null]))
        );
        assert_eq!(
            server_response(
                "workspace/workspaceFolders",
                &Value::Null,
                &options,
                &folders
            ),
            Ok(folders.clone())
        );
        for method in [
            "client/registerCapability",
            "window/workDoneProgress/create",
        ] {
            assert_eq!(
                server_response(method, &Value::Null, &options, &folders),
                Ok(Value::Null)
            );
        }
        assert!(server_response("workspace/applyEdit", &Value::Null, &options, &folders).is_err());
    }

    #[test]
    fn responses_match_ids_not_arrival_order() {
        let transport = Transport {
            writer: Mutex::new(Box::new(Vec::<u8>::new())),
            pending: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
            closed: AtomicBool::new(false),
        };
        let (_, first) = transport.request("first", Value::Null).unwrap();
        let (_, second) = transport.request("second", Value::Null).unwrap();
        let (_, failed) = transport.request("failed", Value::Null).unwrap();
        let mut bytes = Vec::new();
        for (id, result) in [(2, "second"), (1, "first")] {
            protocol::write_frame(&mut bytes, &json!({"id": id, "result": result})).unwrap();
        }
        protocol::write_frame(
            &mut bytes,
            &json!({"id": 3, "error": {"code": -32602, "message": "bad position"}}),
        )
        .unwrap();
        let (sender, _) = mpsc::channel();
        assert!(
            transport
                .read(&mut bytes.as_slice(), &Value::Null, &Value::Null, &sender)
                .is_err()
        );
        assert_eq!(first.recv().unwrap().unwrap(), "first");
        assert_eq!(second.recv().unwrap().unwrap(), "second");
        assert!(matches!(
            failed.recv().unwrap(),
            Err(LspErr::Server { code: -32602, .. })
        ));
        transport.closed.store(true, Ordering::Release);
        assert!(transport.request("after close", Value::Null).is_err());
    }
}
