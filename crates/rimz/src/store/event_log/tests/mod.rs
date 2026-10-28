use std::path::Path;

use serde_json::json;

use super::*;
use crate::ids::WorkspaceId;
use crate::store::event::EventEnvelope;

mod frame;
mod recovery;
mod rotation;
mod roundtrip;

struct ChunkedReader<'a> {
    current: &'a [u8],
    chunks: std::vec::IntoIter<&'a [u8]>,
}

impl io::Read for ChunkedReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.current.is_empty() {
            self.current = self.chunks.next().unwrap_or_default();
        }
        io::Read::read(&mut self.current, buf)
    }
}

fn test_event(method: &str) -> EventEnvelope {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    EventEnvelope::new(
        workspace,
        "session",
        "rimz",
        "cli",
        method,
        json!({ "a": 1 }),
    )
}

fn methods(events: &[EventEnvelope]) -> Vec<&str> {
    events.iter().map(|e| e.method.as_str()).collect()
}
