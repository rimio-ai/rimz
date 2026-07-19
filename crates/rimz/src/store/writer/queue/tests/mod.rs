use tempfile::tempdir;

use super::*;
use crate::agents::AgentLifecycleObservation;
use crate::agents::lifecycle::LifecycleSignal;
use crate::agents::{AgentState, AgentStatus};
use crate::ids::{AgentKind, AgentSessionId, MuxName, PaneId, WorkspaceId};
use crate::message::{AfterCondition, AutoCompact, DeliveryGate, WhenCondition};
use crate::store::event_log;
use crate::{RuntimePaths, StatePaths};

/// A durable store over a disposable root, plus the queue-shaped readers every
/// test in this module needs. Derefs to `Store` so call sites read as the
/// public API they exercise.
struct Queue {
    _dir: tempfile::TempDir,
    store: Store,
    workspace_id: WorkspaceId,
}

impl std::ops::Deref for Queue {
    type Target = Store;

    fn deref(&self) -> &Store {
        &self.store
    }
}

impl Queue {
    fn new() -> Self {
        let dir = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let state = StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap();
        let runtime =
            RuntimePaths::under(workspace_id.clone(), &dir.path().join("runtime")).unwrap();
        Self {
            store: Store::open(state, runtime).unwrap(),
            workspace_id,
            _dir: dir,
        }
    }

    /// Build a record under a deterministic id. Not queued.
    fn record(&self, id: u64) -> MessageRecord {
        let mut record = MessageRecord::new(
            self.workspace_id.clone(),
            &agent(),
            "next".to_owned(),
            true,
            DeliveryGate::Done,
        );
        record.message_id = message_id(id);
        record
    }

    /// Build under a deterministic id, queue it, and hand the record back.
    fn queue(&self, id: u64) -> MessageRecord {
        self.queue_with(id, |_| {})
    }

    fn queue_with(&self, id: u64, edit: impl FnOnce(&mut MessageRecord)) -> MessageRecord {
        let mut record = self.record(id);
        edit(&mut record);
        self.store.queue_message(&record, "session").unwrap();
        record
    }

    /// Build under a deterministic id bound to a pane, and record it as sent.
    fn sent(&self, id: u64) -> MessageRecord {
        self.sent_with(id, |_| {})
    }

    fn sent_with(&self, id: u64, edit: impl FnOnce(&mut MessageRecord)) -> MessageRecord {
        let mut record = self
            .record(id)
            .with_pane_id(PaneId::from_parts(MuxName::Tmux, "%1"));
        edit(&mut record);
        self.store
            .record_sent_message(&record, "session")
            .unwrap()
            .expect("sent")
    }

    fn live(&self) -> Vec<MessageRecord> {
        self.store.list_messages().unwrap()
    }

    fn history(&self) -> Vec<MessageRecord> {
        self.store.list_message_history().unwrap()
    }

    fn by_id(&self, message_id: &MessageId) -> MessageRecord {
        self.live()
            .into_iter()
            .find(|message| message.message_id == *message_id)
            .expect("message")
    }

    fn events(&self) -> Vec<EventEnvelope> {
        event_log::read_all(&self.store.inner.paths.events_log).unwrap()
    }

    /// Every event method, in log order.
    fn methods(&self) -> Vec<String> {
        self.events()
            .into_iter()
            .map(|event| event.method)
            .collect()
    }

    fn count(&self, method: &str) -> usize {
        self.events()
            .iter()
            .filter(|event| event.method == method)
            .count()
    }

    /// `params["reason"]` of the first event carrying this method.
    fn reason(&self, method: &str) -> String {
        self.events()
            .iter()
            .find(|event| event.method == method)
            .unwrap_or_else(|| panic!("{method} event missing"))
            .params_value()["reason"]
            .as_str()
            .expect("reason string")
            .to_owned()
    }
}

fn message_id(value: u64) -> MessageId {
    MessageId::parse(&format!("msg_{value:016}")).unwrap()
}

fn sweep(
    message_id: &MessageId,
    after_indices: &[usize],
    when_indices: &[usize],
    retry_after: Option<Timestamp>,
    archive_reason: Option<&str>,
) -> DeliverySweepUpdate {
    DeliverySweepUpdate {
        message_id: message_id.clone(),
        after_indices: after_indices.to_vec(),
        when_indices: when_indices.to_vec(),
        retry_after,
        archive_reason: archive_reason.map(ToOwned::to_owned),
    }
}

fn agent() -> AgentState {
    crate::testkit::agent_state("claude", "sess-1", Timestamp::now())
}

trait SingularQueueTestExt {
    fn claim_message_for_delivery(
        &self,
        message_id: &MessageId,
        now: Timestamp,
    ) -> Result<Option<MessageRecord>>;
    fn record_sent_message(
        &self,
        message: &MessageRecord,
        session_name: &str,
    ) -> Result<Option<MessageRecord>>;
    fn record_message_delivery_failure(
        &self,
        message_id: &MessageId,
        error: &str,
        session_name: &str,
    ) -> Result<DeliveryFailureResult>;
}

impl SingularQueueTestExt for Store {
    fn claim_message_for_delivery(
        &self,
        message_id: &MessageId,
        now: Timestamp,
    ) -> Result<Option<MessageRecord>> {
        Ok(self
            .claim_delivery_batch(message_id, AgentStatus::Idle, now)?
            .and_then(|claimed| claimed.into_iter().next()))
    }

    fn record_sent_message(
        &self,
        message: &MessageRecord,
        session_name: &str,
    ) -> Result<Option<MessageRecord>> {
        Ok(self
            .record_sent_batch(std::slice::from_ref(message), session_name)?
            .into_iter()
            .next())
    }

    fn record_message_delivery_failure(
        &self,
        message_id: &MessageId,
        error: &str,
        session_name: &str,
    ) -> Result<DeliveryFailureResult> {
        self.record_message_delivery_failures(
            std::slice::from_ref(message_id),
            None,
            DeliveryFailureDisposition::Retry,
            error,
            session_name,
        )
    }
}

mod claim;
mod deliver;
mod lifecycle;
