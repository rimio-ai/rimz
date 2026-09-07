//! Exclusive pane writes, held from the first byte through the submit key.

use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::RuntimePaths;
use crate::ids::PaneId;
use crate::pane::keys::NamedKey;

use super::{MuxBackend, MuxErr, Result, backend_for};

pub struct PaneWriter {
    pane: PaneId,
    backend: Box<dyn MuxBackend>,
    _lock: WorkspaceLock,
}

impl PaneWriter {
    /// Wait for any in-flight write to finish, bounded by the workspace lock timeout.
    pub fn open(runtime: &RuntimePaths, pane: &PaneId) -> Result<Self> {
        #[cfg(feature = "testkit")]
        crate::testkit::rendezvous("RIMZ_TEST_PANE_WRITE_BEFORE_LOCK");
        let lock = WorkspaceLock::acquire(&runtime.pane_write_lock(pane)).map_err(|source| {
            MuxErr::PaneWriteLock {
                pane: pane.clone(),
                source,
            }
        })?;
        Ok(Self {
            pane: pane.clone(),
            backend: backend_for(pane.mux()),
            _lock: lock,
        })
    }

    pub fn type_text(&self, text: &str) -> Result<()> {
        self.backend.send_keys(&self.pane, text)
    }

    pub fn paste(&self, text: &str) -> Result<()> {
        self.backend.paste_text(&self.pane, text)
    }

    pub fn press(&self, key: NamedKey) -> Result<()> {
        self.backend.send_key(&self.pane, key)
    }
}
