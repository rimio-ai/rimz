//! Durable signal append through the ordinary store commit boundary.

use crate::ids::EventId;
use crate::store::event::{EventEnvelope, SIGNAL_METHOD, SignalEventPayload};

use super::Store;
use crate::store::Result;

impl Store {
    #[must_use = "durability barrier; check the result"]
    pub fn append_signal(
        &self,
        session_name: &str,
        payload: SignalEventPayload,
    ) -> Result<EventId> {
        let event = EventEnvelope::new(
            self.inner.paths.workspace_id.clone(),
            session_name,
            "rimz",
            "signal",
            SIGNAL_METHOD,
            serde_json::to_value(payload).expect("signal payload is JSON-serializable"),
        );
        let event_id = event.event_id.clone();
        self.commit(|txn| txn.append(&event))?;
        Ok(event_id)
    }
}
