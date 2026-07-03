//! Durable Zellij presence-plugin telemetry log.
//!
//! The presence plugin runs inside the Zellij server process, so samples written
//! by its keepalive distinguish plugin WASM linear-memory growth from
//! Zellij-native RSS growth. The log is diagnostic state: append-only within a
//! size cap, never read by correctness code.

use std::path::Path;

use serde::Serialize;

use super::JsonlLog;

const PLUGIN_PRESENCE_LOG_NAME: &str = "plugin-presence.log.jsonl";
const PLUGIN_PRESENCE_LOG_MAX_BYTES: u64 = 1_048_576;
const WASM_PAGE_BYTES: u64 = 65_536;

#[derive(Serialize)]
pub struct PluginPresenceSample {
    pub at_ms: u64,
    pub session_name: Option<String>,
    pub pages: u64,
    pub bytes: u64,
    pub uptime_ms: u64,
    pub commands: u64,
    pub zellij_version: Option<String>,
}

impl PluginPresenceSample {
    pub fn new(
        at_ms: u64,
        session_name: Option<String>,
        pages: u64,
        uptime_ms: u64,
        commands: u64,
        zellij_version: Option<String>,
    ) -> Self {
        Self {
            at_ms,
            session_name,
            pages,
            bytes: pages.saturating_mul(WASM_PAGE_BYTES),
            uptime_ms,
            commands,
            zellij_version,
        }
    }
}

pub fn log(state_root: &Path) -> JsonlLog {
    JsonlLog::new(
        state_root.join(PLUGIN_PRESENCE_LOG_NAME),
        PLUGIN_PRESENCE_LOG_MAX_BYTES,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_derives_bytes_from_wasm_pages() {
        let sample = PluginPresenceSample::new(1, None, 42, 1_000, 5, None);

        assert_eq!(sample.bytes, 42 * WASM_PAGE_BYTES);
        assert_eq!(
            PluginPresenceSample::new(1, None, u64::MAX, 1_000, 5, None).bytes,
            u64::MAX
        );
    }
}
