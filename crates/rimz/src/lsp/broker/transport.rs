//! Multiplexed JSON-RPC requests over the server's single stdio connection.

use super::super::{LspErr, Result, protocol};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

type Reply = Box<dyn FnOnce(Result<Value>) + Send>;

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
        sink: mpsc::Sender<Value>,
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
            let result = background.read(&mut reader, &options, &folders, &sink);
            let message = result
                .err()
                .map_or_else(|| "server closed".into(), |e| e.to_string());
            background.closed.store(true, Ordering::Release);
            let pending =
                std::mem::take(&mut *background.pending.lock().unwrap_or_else(|e| e.into_inner()));
            for reply in pending.into_values() {
                reply(Err(LspErr::Protocol(message.clone())));
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
        let (sender, receiver) = mpsc::channel();
        let id = self.request_with(
            method,
            params,
            Box::new(move |frame| {
                let result = frame.and_then(|message| {
                    if let Some(error) = message.get("error") {
                        Err(LspErr::Server {
                            code: error["code"].as_i64().unwrap_or(-32603),
                            message: error["message"]
                                .as_str()
                                .unwrap_or("invalid server error")
                                .to_owned(),
                        })
                    } else {
                        Ok(message["result"].clone())
                    }
                });
                let _ = sender.send(result);
            }),
        )?;
        Ok((id, receiver))
    }

    pub(super) fn request_with(&self, method: &str, params: Value, callback: Reply) -> Result<u64> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, callback);
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
        Ok(id)
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
        sink: &mpsc::Sender<Value>,
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
                    if matches!(
                        method,
                        "window/workDoneProgress/create"
                            | "workspace/semanticTokens/refresh"
                            | "workspace/codeLens/refresh"
                            | "workspace/inlayHint/refresh"
                            | "workspace/diagnostic/refresh"
                    ) {
                        let _ = sink.send(message);
                    }
                } else {
                    let _ = sink.send(message);
                }
                continue;
            }
            if let Some(id) = message.get("id").and_then(Value::as_u64) {
                let reply = self
                    .pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                if let Some(reply) = reply {
                    reply(Ok(message));
                }
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
        "client/registerCapability"
        | "client/unregisterCapability"
        | "window/workDoneProgress/create"
        | "workspace/semanticTokens/refresh"
        | "workspace/codeLens/refresh"
        | "workspace/inlayHint/refresh"
        | "workspace/diagnostic/refresh" => Value::Null,
        _ => return Err(()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_replies_preserve_errors_and_report_closure() {
        struct Reader(mpsc::Receiver<u8>);
        impl Read for Reader {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                let Some(first) = bytes.first_mut() else {
                    return Ok(0);
                };
                match self.0.recv() {
                    Ok(byte) => {
                        *first = byte;
                        Ok(1)
                    }
                    Err(_) => Ok(0),
                }
            }
        }
        let (server, reader) = mpsc::channel();
        let (sink, _) = mpsc::channel();
        let transport = Transport::start(
            Reader(reader),
            Vec::<u8>::new(),
            Value::Null,
            Value::Null,
            sink,
        );
        let (sender, replies) = mpsc::channel();
        let first = sender.clone();
        let id = transport
            .request_with(
                "first",
                Value::Null,
                Box::new(move |r| {
                    first.send(r).unwrap();
                }),
            )
            .unwrap();
        let other = transport
            .request_with(
                "second",
                Value::Null,
                Box::new(move |r| {
                    sender.send(r).unwrap();
                }),
            )
            .unwrap();
        assert_ne!(id, other);
        let frame = json!({"id": id, "error": {"code": -32602, "message": "bad position", "data": {"detail": 42}}});
        let mut bytes = Vec::new();
        protocol::write_frame(&mut bytes, &frame).unwrap();
        for byte in bytes {
            server.send(byte).unwrap();
        }
        assert_eq!(replies.recv().unwrap().unwrap(), frame);
        drop(server);
        assert!(matches!(replies.recv().unwrap(), Err(LspErr::Protocol(_))));
    }

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
            "client/unregisterCapability",
            "window/workDoneProgress/create",
            "workspace/semanticTokens/refresh",
            "workspace/codeLens/refresh",
            "workspace/inlayHint/refresh",
            "workspace/diagnostic/refresh",
        ] {
            assert_eq!(
                server_response(method, &Value::Null, &options, &folders),
                Ok(Value::Null)
            );
        }
        assert!(server_response("workspace/applyEdit", &Value::Null, &options, &folders).is_err());
    }

    #[test]
    fn server_sink_receives_notifications_and_acknowledged_refreshes() {
        let transport = Transport {
            writer: Mutex::new(Box::new(Vec::<u8>::new())),
            pending: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
            closed: AtomicBool::new(false),
        };
        let frames = [
            json!({"method": "$/progress", "params": {"token": 1}}),
            json!({"method": "textDocument/publishDiagnostics", "params": {"uri": "file:///u", "diagnostics": []}}),
            json!({"id": 1, "method": "workspace/diagnostic/refresh"}),
            json!({"id": 2, "method": "window/workDoneProgress/create"}),
        ];
        let mut bytes = Vec::new();
        for frame in &frames {
            protocol::write_frame(&mut bytes, frame).unwrap();
        }
        protocol::write_frame(
            &mut bytes,
            &json!({"id": 3, "method": "workspace/workspaceFolders"}),
        )
        .unwrap();
        protocol::write_frame(&mut bytes, &json!({"id": 4, "method": "unknown"})).unwrap();
        let (sender, sink) = mpsc::channel();
        assert!(
            transport
                .read(&mut bytes.as_slice(), &Value::Null, &Value::Null, &sender)
                .is_err()
        );
        assert_eq!(sink.try_iter().collect::<Vec<_>>(), frames);
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
