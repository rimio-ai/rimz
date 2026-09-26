//! The single editor-traffic writer; socket pumps never touch the server transport.

use super::{
    RequestPhase, Shared, Transport,
    clients::{Action, ClientId, Clients, Event},
};
use crate::lsp::{
    LspErr, Result,
    admission::Shortfall,
    protocol,
    registry::{self, Lease, State, StopReason},
};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, mpsc};

pub(super) enum RouterEvent {
    Attach {
        stream: UnixStream,
        pid: u32,
        start_token: String,
        reply: mpsc::Sender<Result<ClientId>>,
    },
    Client(Event),
    Message(u64, Value),
    Reply(u64, ClientId, Value, Result<Value>),
    Starting(u64),
    Started(Arc<Transport>, Value, mpsc::Sender<()>),
    Ended(StopReason),
    Refused(Shortfall),
    Close,
}

struct Connection {
    stream: UnixStream,
    outbound: mpsc::SyncSender<Value>,
    writer: std::thread::JoinHandle<()>,
    pid: u32,
}

pub(super) fn run(shared: Arc<Shared>, events: mpsc::Receiver<RouterEvent>) {
    let server = shared
        .model
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry
        .server
        .clone();
    let mut router = Router {
        clients: Clients::new(server),
        connections: BTreeMap::new(),
        transport: None,
        pending: Vec::new(),
        epoch: 0,
        next_client: 0,
    };
    for event in events {
        if matches!(event, RouterEvent::Close) {
            for connection in std::mem::take(&mut router.connections).into_values() {
                if connection
                    .stream
                    .set_write_timeout(Some(std::time::Duration::from_secs(1)))
                    .is_err()
                {
                    let _ = connection.stream.shutdown(Shutdown::Both);
                }
                drop(connection.outbound);
                let _ = connection.writer.join();
                let _ = connection.stream.shutdown(Shutdown::Both);
            }
            break;
        }
        if let Err(error) = router.event(&shared, event) {
            tracing::warn!(%error, "editor router failed");
            shared.stop(StopReason::Crashed);
        }
    }
}

struct Router {
    clients: Clients,
    connections: BTreeMap<ClientId, Connection>,
    transport: Option<Arc<Transport>>,
    pending: Vec<(ClientId, Value, u64)>,
    epoch: u64,
    next_client: u64,
}

impl Router {
    fn event(&mut self, shared: &Shared, event: RouterEvent) -> Result<()> {
        let event = match event {
            RouterEvent::Attach {
                stream,
                pid,
                start_token,
                reply,
            } => {
                let _ = reply.send(self.attach(shared, stream, pid, start_token));
                return Ok(());
            }
            RouterEvent::Client(Event::Detach(id)) => {
                self.detach(shared, id);
                Event::Detach(id)
            }
            RouterEvent::Client(Event::ClientFrame(id, frame)) => {
                if !self.connections.contains_key(&id) {
                    return Ok(());
                }
                let actions = self.clients.event(Event::ClientFrame(id, frame));
                if actions.iter().any(|action| {
                    matches!(action, Action::RequestStart)
                        || matches!(action, Action::ToServer(_, frame) if frame.get("id").is_some())
                }) {
                    let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
                    model.entry.request_count = model.entry.request_count.saturating_add(1);
                    model.entry.last_request_at_ms = Some(crate::utils::time::unix_now_ms());
                }
                return self.actions(shared, actions);
            }
            RouterEvent::Client(event) => event,
            RouterEvent::Message(epoch, frame) => {
                if epoch != self.epoch {
                    return Ok(());
                }
                if frame["method"] == "$/progress" {
                    let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
                    model.readiness.progress(&frame["params"], shared.elapsed());
                    shared.changed.notify_all();
                }
                Event::ServerMessage(frame)
            }
            RouterEvent::Reply(epoch, id, original, frame) => {
                if epoch != self.epoch {
                    return Ok(());
                }
                let Ok(frame) = frame else {
                    return Ok(());
                };
                self.pending
                    .retain(|(client, request, _)| *client != id || *request != original);
                Event::ServerReply(id, original, frame)
            }
            RouterEvent::Started(transport, initialize_result, replayed) => {
                self.transport = Some(transport);
                let actions = self
                    .clients
                    .event(Event::LifetimeStarted { initialize_result });
                self.actions(shared, actions)?;
                let _ = replayed.send(());
                return Ok(());
            }
            RouterEvent::Starting(epoch) => {
                self.epoch = epoch;
                return Ok(());
            }
            RouterEvent::Ended(reason) => {
                self.epoch += 1;
                self.transport = None;
                self.pending.clear();
                Event::LifetimeEnded(reason)
            }
            RouterEvent::Refused(shortfall) => Event::Refused(shortfall),
            RouterEvent::Close => return Ok(()),
        };
        let actions = self.clients.event(event);
        self.actions(shared, actions)
    }

    fn attach(
        &mut self,
        shared: &Shared,
        stream: UnixStream,
        pid: u32,
        start_token: String,
    ) -> Result<ClientId> {
        let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
        let refusal = if model.request_phase == RequestPhase::Closing {
            Some("broker is shutting down".to_owned())
        } else if let State::Stopped { reason, .. } = model.entry.state {
            Some(reason.to_string())
        } else if !crate::proc::process_is_live(pid, Some(&start_token)) {
            Some("lease process is not live".into())
        } else if self.connections.len() >= 8 {
            Some("editor limit reached".into())
        } else {
            None
        };
        if let Some(message) = refusal {
            return Err(LspErr::Server {
                code: -32003,
                message,
            });
        }
        let mut writer = stream.try_clone()?;
        let id = ClientId(self.next_client + 1);
        let (outbound, frames) = mpsc::sync_channel(1024);
        let sender = shared.router.clone();
        let writer = std::thread::Builder::new().spawn(move || {
            for frame in frames {
                if protocol::write_frame(&mut writer, &frame).is_err() {
                    break;
                }
            }
            let _ = sender.send(RouterEvent::Client(Event::Detach(id)));
        })?;
        let since_ms = crate::utils::time::unix_now_ms();
        model.lifecycle.register(Lease {
            launch_id: None,
            pid,
            start_token,
            since_ms,
        });
        model.entry.leases = model.lifecycle.leases.clone();
        drop(model);
        self.next_client += 1;
        self.connections.insert(
            id,
            Connection {
                stream,
                outbound,
                writer,
                pid,
            },
        );
        let actions = self.clients.event(Event::Attach {
            id,
            pid,
            name: None,
            since_ms,
        });
        self.actions(shared, actions)?;
        Ok(id)
    }

    fn detach(&mut self, shared: &Shared, id: ClientId) {
        if let Some(connection) = self.connections.remove(&id) {
            let _ = connection.stream.shutdown(Shutdown::Both);
            if !self
                .connections
                .values()
                .any(|other| other.pid == connection.pid)
            {
                let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
                model.lifecycle.retain(shared.elapsed(), |lease| {
                    lease.launch_id.is_some() || lease.pid != connection.pid
                });
                model.entry.leases = model.lifecycle.leases.clone();
            }
        }
        self.pending.retain(|(client, _, request)| {
            if *client != id {
                return true;
            }
            if let Some(transport) = &self.transport {
                transport.cancel(*request);
            }
            false
        });
    }

    fn actions(&mut self, shared: &Shared, actions: Vec<Action>) -> Result<()> {
        let mut actions = VecDeque::from(actions);
        while let Some(action) = actions.pop_front() {
            match action {
                Action::ToServer(client, frame) => self.forward(shared, client, frame)?,
                Action::ToClient(id, frame) => {
                    if self
                        .connections
                        .get(&id)
                        .is_some_and(|c| c.outbound.try_send(frame).is_err())
                    {
                        self.detach(shared, id);
                        actions.extend(self.clients.event(Event::Detach(id)));
                    }
                }
                Action::Detach(id) => {
                    self.detach(shared, id);
                    actions.extend(self.clients.event(Event::Detach(id)));
                    actions.push_back(Action::Publish);
                }
                Action::Publish => {
                    let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
                    model.entry.attached = self.clients.attached();
                    if let Err(error) = registry::publish(&model.entry) {
                        tracing::warn!(%error, "cannot publish attached editors");
                    }
                }
                Action::RequestStart => {
                    let mut model = shared.model.lock().unwrap_or_else(|e| e.into_inner());
                    if matches!(model.entry.state, State::Dormant { .. }) {
                        model.start_requested = true;
                        shared.changed.notify_all();
                    }
                }
            }
        }
        Ok(())
    }

    fn forward(&mut self, shared: &Shared, client: ClientId, frame: Value) -> Result<()> {
        let Some(transport) = &self.transport else {
            return Ok(());
        };
        let Some(method) = frame["method"].as_str() else {
            return Ok(());
        };
        if let Some(original) = frame.get("id") {
            let original = original.clone();
            let reply_id = original.clone();
            let sender = shared.router.clone();
            let epoch = self.epoch;
            let id = transport.request_with(
                method,
                frame["params"].clone(),
                Box::new(move |reply| {
                    let _ = sender.send(RouterEvent::Reply(epoch, client, reply_id, reply));
                }),
            )?;
            self.pending.push((client, original, id));
        } else if method == "$/cancelRequest" {
            if let Some((_, _, id)) = self
                .pending
                .iter()
                .find(|(id, original, _)| *id == client && *original == frame["params"]["id"])
            {
                transport.notify(method, serde_json::json!({"id":id}))?;
            }
        } else {
            transport.notify(method, frame["params"].clone())?;
        }
        Ok(())
    }
}
