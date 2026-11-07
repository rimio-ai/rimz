//! Exclusive pane writes, held from the first byte through the submit key.

use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::RuntimePaths;
use crate::ids::PaneId;
use crate::pane::keys::NamedKey;
use crate::utils::time::unix_now_ms;

use super::typing::{InputSource, TypingRecord};
use super::{MuxBackend, MuxErr, Result, backend_for};

pub struct PaneWriter {
    pane: PaneId,
    session: String,
    backend: Box<dyn MuxBackend>,
    typing_runtime: Option<RuntimePaths>,
    _lock: WorkspaceLock,
}

impl PaneWriter {
    /// Wait for any in-flight write to finish, bounded by the workspace lock timeout.
    /// Every write addresses `session` (see [`MuxBackend::send_keys`]).
    /// `runtime` names its owning room, or is absent for a non-RimZ session.
    pub fn open(runtime: Option<&RuntimePaths>, pane: &PaneId, session: &str) -> Result<Self> {
        #[cfg(feature = "testkit")]
        crate::testkit::rendezvous("RIMZ_TEST_PANE_WRITE_BEFORE_LOCK");
        let lock_path = runtime.map_or_else(
            || RuntimePaths::shared().pane_write_lock(pane),
            |runtime| runtime.pane_write_lock(pane),
        );
        let lock = WorkspaceLock::acquire(&lock_path).map_err(|source| MuxErr::PaneWriteLock {
            pane: pane.clone(),
            source,
        })?;
        Ok(Self {
            pane: pane.clone(),
            session: session.to_owned(),
            backend: backend_for(pane.mux()),
            typing_runtime: runtime.filter(|_| InputSource::uses_tty()).cloned(),
            _lock: lock,
        })
    }

    pub fn type_text(&self, text: &str) -> Result<()> {
        self.write(|backend| backend.send_keys(&self.pane, &self.session, text))
    }

    pub fn paste(&self, text: &str) -> Result<()> {
        self.write(|backend| backend.paste_text(&self.pane, &self.session, text))
    }

    pub fn press(&self, key: NamedKey) -> Result<()> {
        self.write(|backend| backend.send_key(&self.pane, &self.session, key))
    }

    fn write(&self, send: impl FnOnce(&dyn MuxBackend) -> Result<()>) -> Result<()> {
        self.stamp();
        let result = send(self.backend.as_ref());
        self.stamp();
        result
    }

    fn stamp(&self) {
        let Some(runtime) = &self.typing_runtime else {
            return;
        };
        exclude_pane_input(runtime, &self.pane);
    }
}

/// Exclude terminal input caused by a room-owned pane write or popup.
pub fn exclude_pane_input(runtime: &RuntimePaths, pane: &PaneId) {
    if !InputSource::uses_tty() {
        return;
    }
    if let Err(error) = TypingRecord::update(runtime, |record| {
        record.sent(pane, unix_now_ms());
    }) {
        tracing::debug!(%error, %pane, "cannot exclude pane input for typing hold");
    }
}
