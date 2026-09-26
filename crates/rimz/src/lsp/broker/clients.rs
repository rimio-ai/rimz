//! Pure editor routing. The driver allocates server request IDs through Transport::request_with.
//! ToServer carries the originating client and its original ID; the driver retains that mapping for cancellation and replies.
//! Server requests have already been acknowledged by the transport; editor replies to fan-out requests are discarded here.

use super::super::{
    admission::Shortfall,
    query::{QueryErr, UnavailableReason},
    registry::{AttachedEditor, OpenDocument, StopReason},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ClientId(pub(super) u64);

pub(super) enum Event {
    Attach {
        id: ClientId,
        pid: u32,
        name: Option<String>,
        since_ms: u64,
    },
    Detach(ClientId),
    ClientFrame(ClientId, Value),
    ServerMessage(Value),
    ServerReply(ClientId, Value, Value),
    LifetimeStarted {
        initialize_result: Value,
    },
    LifetimeEnded(StopReason),
    Refused(Shortfall),
}

#[derive(Debug, PartialEq)]
pub(super) enum Action {
    ToServer(ClientId, Value),
    ToClient(ClientId, Value),
    Detach(ClientId),
    Publish,
    RequestStart,
}

struct Client {
    pid: u32,
    name: Option<String>,
    since_ms: u64,
    ready: bool,
    shutdown: bool,
    queued: Vec<Value>,
    pending: Vec<Value>,
}

#[derive(Clone)]
struct Holder {
    client: ClientId,
    text: String,
    version: i64,
    language_id: String,
    dirty: bool,
}

struct View {
    owner: ClientId,
    version: i64,
    incarnation: u64,
}

#[derive(Clone)]
struct Snapshot {
    frame: Value,
    text: String,
    incarnation: u64,
}

pub(super) struct Clients {
    server: String,
    clients: BTreeMap<ClientId, Client>,
    documents: BTreeMap<String, Vec<Holder>>,
    view: BTreeMap<String, View>,
    snapshots: BTreeMap<String, Snapshot>,
    shown: BTreeMap<(ClientId, String), String>,
    initialized: Option<Value>,
    active: bool,
    status: Option<Value>,
    next: u64,
}

impl Clients {
    pub(super) fn new(server: String) -> Self {
        Self {
            server,
            clients: BTreeMap::new(),
            documents: BTreeMap::new(),
            view: BTreeMap::new(),
            snapshots: BTreeMap::new(),
            shown: BTreeMap::new(),
            initialized: None,
            active: false,
            status: None,
            next: 0,
        }
    }

    pub(super) fn attached(&self) -> Vec<AttachedEditor> {
        self.clients
            .iter()
            .map(|(id, c)| AttachedEditor {
                pid: c.pid,
                name: c.name.clone(),
                since_ms: c.since_ms,
                open: self
                    .documents
                    .iter()
                    .filter_map(|(uri, holders)| {
                        holders
                            .iter()
                            .find(|h| h.client == *id)
                            .map(|h| OpenDocument {
                                uri: uri.clone(),
                                owner: holders[0].client == *id,
                                dirty: h.dirty,
                            })
                    })
                    .collect(),
            })
            .collect()
    }

    pub(super) fn event(&mut self, event: Event) -> Vec<Action> {
        let mut out = Vec::new();
        match event {
            Event::Attach {
                id,
                pid,
                name,
                since_ms,
            } => {
                self.clients.insert(
                    id,
                    Client {
                        pid,
                        name,
                        since_ms,
                        ready: false,
                        shutdown: false,
                        queued: vec![],
                        pending: vec![],
                    },
                );
                out.push(Action::Publish);
            }
            Event::Detach(id) => self.detach(id, &mut out),
            Event::ClientFrame(id, frame) => self.client_frame(id, frame, &mut out),
            Event::ServerMessage(frame) => self.server_message(frame, &mut out),
            Event::ServerReply(id, original, mut frame) => {
                if let Some(client) = self.clients.get_mut(&id)
                    && let Some(index) = client
                        .pending
                        .iter()
                        .position(|pending| *pending == original)
                {
                    client.pending.remove(index);
                    frame["id"] = original;
                    out.push(Action::ToClient(id, frame));
                }
            }
            Event::LifetimeStarted { initialize_result } => {
                self.active = true;
                self.initialized = Some(initialize_result);
                self.snapshots.clear();
                self.view.clear();
                for uri in self.documents.keys().cloned().collect::<Vec<_>>() {
                    self.sync(&uri, &mut out);
                }
                for id in self.clients.keys().copied().collect::<Vec<_>>() {
                    // This loop does not remove the clients whose keys it collected.
                    let queued = std::mem::take(
                        &mut self.clients.get_mut(&id).expect("listed client").queued,
                    );
                    for frame in queued {
                        self.client_frame(id, frame, &mut out);
                    }
                }
            }
            Event::LifetimeEnded(reason) => {
                self.active = false;
                self.snapshots.clear();
                self.view.clear();
                let message = format!("language server {} stopped: {reason}", self.server);
                self.cancel_requests(&message, 2, &mut out);
                let status = if reason.is_terminal() {
                    message
                } else {
                    format!("{message}; the next request restarts it")
                };
                self.server_message(json!({"jsonrpc": "2.0", "method": "experimental/serverStatus", "params": {"health": "warning", "quiescent": true, "message": status}}), &mut out);
            }
            Event::Refused(shortfall) => {
                let message = QueryErr::Unavailable {
                    root: shortfall.root,
                    reason: UnavailableReason::MemoryShort,
                }
                .to_string();
                self.cancel_requests(&message, 1, &mut out);
            }
        }
        out
    }

    fn cancel_requests(&mut self, message: &str, kind: u8, out: &mut Vec<Action>) {
        for (id, client) in &mut self.clients {
            if client.pending.is_empty() && client.queued.is_empty() {
                continue;
            }
            for request in client
                .pending
                .drain(..)
                .chain(client.queued.drain(..).map(|frame| frame["id"].clone()))
            {
                out.push(error(*id, request, -32802, message));
            }
            out.push(show_message(*id, kind, message));
        }
    }

    fn detach(&mut self, id: ClientId, out: &mut Vec<Action>) {
        if self.clients.remove(&id).is_none() {
            return;
        }
        for uri in self.documents.keys().cloned().collect::<Vec<_>>() {
            // sync changes only the server view, not this collected document map.
            self.documents
                .get_mut(&uri)
                .expect("listed document")
                .retain(|h| h.client != id);
            self.sync(&uri, out);
        }
        self.documents.retain(|_, holders| !holders.is_empty());
        self.shown.retain(|(client, _), _| *client != id);
        out.push(Action::Publish);
    }

    fn client_frame(&mut self, id: ClientId, frame: Value, out: &mut Vec<Action>) {
        let Some(client) = self.clients.get_mut(&id) else {
            return;
        };
        let Some(method) = frame["method"].as_str() else {
            return;
        };
        if method == "exit" {
            self.detach(id, out);
            out.push(Action::Detach(id));
            return;
        }
        let request = frame.get("id").cloned();
        if client.shutdown {
            if let Some(request) = request {
                out.push(error(id, request, -32600, "client shut down"));
            }
            return;
        }
        match method {
            "initialize" => {
                if let Some(name) = frame["params"]["clientInfo"]["name"].as_str() {
                    client.name = Some(name.to_owned());
                    out.push(Action::Publish);
                }
                if let Some(result) = &self.initialized {
                    if !self.active {
                        out.push(Action::RequestStart);
                    }
                    let mut result = result.clone();
                    let old = &result["capabilities"]["textDocumentSync"];
                    let mut sync = json!({"openClose": true, "change": 1, "save": old.get("save").cloned().unwrap_or(json!(true))});
                    for key in ["willSave", "willSaveWaitUntil"] {
                        if let Some(value) = old.get(key) {
                            sync[key] = value.clone();
                        }
                    }
                    result["capabilities"]["textDocumentSync"] = sync;
                    let folders = &mut result["capabilities"]["workspace"]["workspaceFolders"];
                    if !folders.is_object() {
                        *folders = match folders.as_bool() {
                            Some(supported) => json!({"supported": supported}),
                            None => json!({}),
                        };
                    }
                    folders["changeNotifications"] = json!(false);
                    out.push(Action::ToClient(
                        id,
                        json!({"jsonrpc": "2.0", "id": frame["id"], "result": result}),
                    ));
                } else {
                    self.enqueue(id, frame, out);
                }
            }
            "initialized" => {
                if client.ready {
                    return;
                }
                client.ready = true;
                if let Some(status) = &self.status {
                    out.push(Action::ToClient(id, status.clone()));
                }
                let uris = self
                    .documents
                    .iter()
                    .filter(|(_, holders)| holders.iter().any(|h| h.client == id))
                    .map(|(uri, _)| uri.clone())
                    .collect::<Vec<_>>();
                for uri in uris {
                    self.replay(id, &uri, out);
                }
            }
            "shutdown" => {
                client.shutdown = true;
                out.push(Action::ToClient(
                    id,
                    json!({"jsonrpc": "2.0", "id": frame["id"], "result": null}),
                ));
            }
            "textDocument/didOpen"
            | "textDocument/didChange"
            | "textDocument/didClose"
            | "textDocument/didSave" => self.document(id, frame, out),
            "$/cancelRequest" => {
                let request = &frame["params"]["id"];
                if let Some(index) = client.queued.iter().position(|v| v["id"] == *request) {
                    client.queued.remove(index);
                    out.push(error(id, request.clone(), -32800, "request cancelled"));
                } else if client.pending.contains(request) {
                    out.push(Action::ToServer(id, frame));
                }
            }
            "workspace/didChangeConfiguration"
            | "workspace/didChangeWorkspaceFolders"
            | "workspace/didChangeWatchedFiles"
            | "$/setTrace"
            | "$/logTrace" => {}
            _ => {
                if self.active {
                    if let Some(request) = request {
                        client.pending.push(request);
                    }
                    out.push(Action::ToServer(id, frame));
                } else if request.is_some() {
                    self.enqueue(id, frame, out);
                }
            }
        }
    }

    fn enqueue(&mut self, id: ClientId, frame: Value, out: &mut Vec<Action>) {
        // Called only for a frame from a currently attached client.
        let client = self.clients.get_mut(&id).expect("attached client");
        if client.queued.len() == 256 {
            out.push(error(id, frame["id"].clone(), -32803, "request queue full"));
        } else {
            client.queued.push(frame);
            out.push(Action::RequestStart);
        }
    }

    fn document(&mut self, id: ClientId, frame: Value, out: &mut Vec<Action>) {
        let Some(uri) = frame["params"]["textDocument"]["uri"].as_str() else {
            return;
        };
        let method = frame["method"].as_str().unwrap_or_default();
        let doc = &frame["params"]["textDocument"];
        if method == "textDocument/didOpen" {
            let (Some(text), Some(version), Some(language)) = (
                doc["text"].as_str(),
                doc["version"].as_i64(),
                doc["languageId"].as_str(),
            ) else {
                return;
            };
            let holders = self.documents.entry(uri.to_owned()).or_default();
            if holders.iter().any(|h| h.client == id) {
                return;
            }
            holders.push(Holder {
                client: id,
                text: text.to_owned(),
                version,
                language_id: language.to_owned(),
                dirty: false,
            });
            self.sync(uri, out);
            self.replay(id, uri, out);
            out.push(Action::Publish);
            return;
        }
        let Some(holders) = self.documents.get_mut(uri) else {
            return;
        };
        let Some(index) = holders.iter().position(|h| h.client == id) else {
            return;
        };
        match method {
            "textDocument/didClose" => {
                holders.remove(index);
                self.shown.remove(&(id, uri.to_owned()));
                self.sync(uri, out);
                self.documents.retain(|_, holders| !holders.is_empty());
                out.push(Action::Publish);
            }
            "textDocument/didChange" => {
                let Some(changes) = frame["params"]["contentChanges"].as_array() else {
                    return;
                };
                let [change] = changes.as_slice() else {
                    return;
                };
                if change.get("range").is_some() {
                    return;
                }
                let (Some(text), Some(version)) =
                    (change["text"].as_str(), doc["version"].as_i64())
                else {
                    return;
                };
                if index == 0 {
                    self.snapshots.remove(uri);
                }
                let holder = &mut holders[index];
                holder.text = text.to_owned();
                holder.version = version;
                let publish = !holder.dirty;
                holder.dirty = true;
                let key = (id, uri.to_owned());
                if self.shown.get(&key).is_some_and(|shown| shown != text) {
                    self.shown.remove(&key);
                    // The server republishes for its owner's new text; a clear would only flicker.
                    if index != 0 && self.clients[&id].ready {
                        out.push(Action::ToClient(id, json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {"uri": uri, "diagnostics": []}})));
                    }
                }
                self.sync(uri, out);
                if publish {
                    out.push(Action::Publish);
                }
            }
            "textDocument/didSave" => {
                let holder = &mut holders[index];
                if holder.dirty {
                    holder.dirty = false;
                    out.push(Action::Publish);
                }
                if self.view.get(uri).is_some_and(|v| v.owner == id) {
                    out.push(Action::ToServer(id, frame));
                }
            }
            _ => {}
        }
    }

    fn sync(&mut self, uri: &str, out: &mut Vec<Action>) {
        if !self.active {
            return;
        }
        let owner = self.documents.get(uri).and_then(|holders| holders.first());
        let view = self.view.get(uri);
        if view.is_some_and(|v| owner.is_none_or(|h| h.client != v.owner)) {
            self.snapshots.remove(uri);
            // The condition above proves a previous server view exists.
            let previous = self.view.remove(uri).expect("previous view");
            out.push(Action::ToServer(previous.owner, json!({"jsonrpc": "2.0", "method": "textDocument/didClose", "params": {"textDocument": {"uri": uri}}})));
        }
        let Some(owner) = owner else {
            return;
        };
        if let Some(view) = self.view.get_mut(uri) {
            if view.version != owner.version {
                view.version = owner.version;
                out.push(Action::ToServer(owner.client, json!({"jsonrpc": "2.0", "method": "textDocument/didChange", "params": {"textDocument": {"uri": uri, "version": owner.version}, "contentChanges": [{"text": owner.text}]}})));
            }
            return;
        }
        self.next += 1;
        self.view.insert(
            uri.to_owned(),
            View {
                owner: owner.client,
                version: owner.version,
                incarnation: self.next,
            },
        );
        out.push(Action::ToServer(owner.client, json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {"textDocument": {"uri": uri, "version": owner.version, "languageId": owner.language_id, "text": owner.text}}})));
    }

    fn server_message(&mut self, mut frame: Value, out: &mut Vec<Action>) {
        let Some(method) = frame["method"].as_str() else {
            return;
        };
        match method {
            "textDocument/publishDiagnostics" => {
                self.diagnostics(frame, out);
                return;
            }
            "experimental/serverStatus" => self.status = Some(frame.clone()),
            "$/progress" | "window/showMessage" | "window/logMessage" => {}
            "window/workDoneProgress/create"
            | "workspace/semanticTokens/refresh"
            | "workspace/codeLens/refresh"
            | "workspace/inlayHint/refresh"
            | "workspace/diagnostic/refresh" => {
                self.next += 1;
                frame["id"] = json!(format!("rimz:{}", self.next));
            }
            _ => return,
        }
        for (id, client) in &self.clients {
            if client.ready {
                out.push(Action::ToClient(*id, frame.clone()));
            }
        }
    }

    fn diagnostics(&mut self, frame: Value, out: &mut Vec<Action>) {
        // A string URI proves params is an object for every version removal below.
        let Some(uri) = frame["params"]["uri"].as_str() else {
            return;
        };
        if frame["params"]["diagnostics"]
            .as_array()
            .is_some_and(Vec::is_empty)
        {
            self.snapshots.remove(uri);
            self.shown.retain(|(_, shown_uri), _| shown_uri != uri);
            for (id, client) in &self.clients {
                if !client.ready {
                    continue;
                }
                let mut clear = frame.clone();
                if self.view.get(uri).is_none_or(|view| view.owner != *id) {
                    clear["params"]
                        .as_object_mut()
                        .expect("diagnostic params")
                        .remove("version");
                }
                out.push(Action::ToClient(*id, clear));
            }
            return;
        }
        let Some(view) = self.view.get(uri) else {
            for (id, client) in &self.clients {
                if !client.ready
                    || self
                        .documents
                        .get(uri)
                        .is_some_and(|holders| holders.iter().any(|h| h.client == *id))
                {
                    continue;
                }
                let mut frame = frame.clone();
                frame["params"]
                    .as_object_mut()
                    .expect("diagnostic params")
                    .remove("version");
                out.push(Action::ToClient(*id, frame));
            }
            return;
        };
        if frame["params"]
            .get("version")
            .is_some_and(|version| *version != json!(view.version))
        {
            if self.clients.get(&view.owner).is_some_and(|c| c.ready) {
                out.push(Action::ToClient(view.owner, frame));
            }
            return;
        }
        // A server view exists only for a held URI.
        let snapshot = Snapshot {
            frame: frame.clone(),
            text: self.documents[uri][0].text.clone(),
            incarnation: view.incarnation,
        };
        self.snapshots.insert(uri.to_owned(), snapshot.clone());
        for id in self.clients.keys().copied().collect::<Vec<_>>() {
            self.deliver(id, uri, &snapshot, out);
        }
    }

    fn replay(&mut self, id: ClientId, uri: &str, out: &mut Vec<Action>) {
        if let Some(snapshot) = self.snapshots.get(uri).cloned()
            && self
                .view
                .get(uri)
                .is_some_and(|view| view.incarnation == snapshot.incarnation)
        {
            self.deliver(id, uri, &snapshot, out);
        }
    }

    fn deliver(&mut self, id: ClientId, uri: &str, snapshot: &Snapshot, out: &mut Vec<Action>) {
        // Snapshots were admitted by diagnostics, which requires an object params with a URI.
        if !self.clients[&id].ready {
            return;
        }
        let holder = self
            .documents
            .get(uri)
            .and_then(|holders| holders.iter().find(|h| h.client == id));
        let mut frame = snapshot.frame.clone();
        if let Some(holder) = holder {
            if holder.text != snapshot.text {
                return;
            }
            if self.view.get(uri).is_none_or(|view| view.owner != id) {
                frame["params"]["version"] = json!(holder.version);
            }
            self.shown
                .insert((id, uri.to_owned()), snapshot.text.clone());
        } else {
            frame["params"]
                .as_object_mut()
                .expect("diagnostic params")
                .remove("version");
        }
        out.push(Action::ToClient(id, frame));
    }
}

fn error(client: ClientId, id: Value, code: i64, message: &str) -> Action {
    Action::ToClient(
        client,
        json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
    )
}

fn show_message(client: ClientId, kind: u8, message: &str) -> Action {
    Action::ToClient(
        client,
        json!({"jsonrpc": "2.0", "method": "window/showMessage", "params": {"type": kind, "message": message}}),
    )
}

#[cfg(test)]
mod tests;
