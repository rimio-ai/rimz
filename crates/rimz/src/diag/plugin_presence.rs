//! Durable Zellij presence-plugin telemetry log.
//!
//! The presence plugin runs inside the Zellij server process, so samples written
//! by its keepalive distinguish plugin WASM linear-memory growth from
//! Zellij-native RSS growth. The log is diagnostic state: append-only within a
//! size cap, never read by correctness code.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::JsonlLog;

const PLUGIN_PRESENCE_LOG_NAME: &str = "plugin-presence.log.jsonl";
const PLUGIN_PRESENCE_LOG_MAX_BYTES: u64 = 1_048_576;
pub const WASM_PAGE_BYTES: u64 = 65_536;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PluginPresenceSample {
    pub at_ms: u64,
    pub session_name: Option<String>,
    #[serde(default)]
    pub plugin_id: Option<u32>,
    #[serde(default)]
    pub loaded_at_ms: u64,
    pub pages: u64,
    pub bytes: u64,
    pub uptime_ms: u64,
    pub commands: u64,
    #[serde(default)]
    pub commands_succeeded: Option<u64>,
    /// Legacy cumulative failures. Split counters are authoritative when set.
    #[serde(default)]
    pub commands_failed: u64,
    #[serde(default)]
    pub stale_writer_rejections: Option<u64>,
    #[serde(default)]
    pub topology_failures: Option<u64>,
    #[serde(default)]
    pub other_failures: Option<u64>,
    #[serde(default)]
    pub zellij_version: Option<String>,
}

pub fn log(state_root: &Path) -> JsonlLog {
    JsonlLog::new(
        state_root.join(PLUGIN_PRESENCE_LOG_NAME),
        PLUGIN_PRESENCE_LOG_MAX_BYTES,
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginPresenceSpan {
    pub plugin_id: u32,
    pub loaded_at_ms: u64,
    pub sample_count: usize,
    pub first_at_ms: u64,
    pub last_at_ms: u64,
    pub zellij_version: Option<String>,
    pub page_growth: i64,
    pub byte_growth: i64,
    pub commands_completed_delta: u64,
    pub commands_succeeded_delta: Option<u64>,
    pub stale_writer_rejections_delta: Option<u64>,
    pub topology_failures_delta: Option<u64>,
    pub other_failures_delta: Option<u64>,
}

pub fn generation_span(
    state_root: &Path,
    session_name: &str,
    plugin_id: u32,
    loaded_at_ms: u64,
) -> Option<PluginPresenceSpan> {
    let current = log(state_root).path().to_owned();
    let rotated = current.with_file_name("plugin-presence.log.1.jsonl");
    let mut samples = [rotated, current]
        .into_iter()
        .flat_map(read_samples)
        .filter(|sample| sample.session_name.as_deref() == Some(session_name))
        .filter(|sample| sample.plugin_id == Some(plugin_id) && sample.loaded_at_ms == loaded_at_ms)
        .collect::<Vec<_>>();
    samples.sort_by_key(|sample| sample.at_ms);
    let first = samples.first()?;
    let last = samples.last()?;
    Some(PluginPresenceSpan {
        plugin_id,
        loaded_at_ms,
        sample_count: samples.len(),
        first_at_ms: first.at_ms,
        last_at_ms: last.at_ms,
        zellij_version: last.zellij_version.clone(),
        page_growth: signed_delta(last.pages, first.pages),
        byte_growth: signed_delta(last.bytes, first.bytes),
        commands_completed_delta: last.commands.saturating_sub(first.commands),
        commands_succeeded_delta: option_delta(last.commands_succeeded, first.commands_succeeded),
        stale_writer_rejections_delta: option_delta(
            last.stale_writer_rejections,
            first.stale_writer_rejections,
        ),
        topology_failures_delta: option_delta(last.topology_failures, first.topology_failures),
        other_failures_delta: option_delta(last.other_failures, first.other_failures),
    })
}

fn read_samples(path: std::path::PathBuf) -> Vec<PluginPresenceSample> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect()
}

fn signed_delta(last: u64, first: u64) -> i64 {
    let delta = i128::from(last) - i128::from(first);
    delta.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

fn option_delta(last: Option<u64>, first: Option<u64>) -> Option<u64> {
    Some(last?.saturating_sub(first?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_span_reads_rotated_and_current_and_ignores_malformed_tail() {
        let dir = tempfile::tempdir().unwrap();
        let current = log(dir.path()).path().to_owned();
        let rotated = current.with_file_name("plugin-presence.log.1.jsonl");
        let sample = |at_ms, pages, failures| {
            serde_json::to_string(&PluginPresenceSample {
                at_ms,
                session_name: Some("room".to_owned()),
                plugin_id: Some(7),
                loaded_at_ms: 100,
                pages,
                bytes: pages.saturating_mul(WASM_PAGE_BYTES),
                uptime_ms: at_ms,
                commands: at_ms / 10,
                commands_succeeded: Some(at_ms / 10 - failures),
                commands_failed: failures,
                stale_writer_rejections: Some(1),
                topology_failures: Some(failures),
                other_failures: Some(0),
                zellij_version: Some("0.44.3".to_owned()),
            })
            .unwrap()
        };
        std::fs::write(&rotated, format!("{}\n", sample(100, 2, 0))).unwrap();
        std::fs::write(&current, format!("{}\nmalformed", sample(200, 5, 2))).unwrap();

        let span = generation_span(dir.path(), "room", 7, 100).unwrap();
        assert_eq!(span.sample_count, 2);
        assert_eq!(span.page_growth, 3);
        assert_eq!(span.topology_failures_delta, Some(2));
        assert!(generation_span(dir.path(), "other", 7, 100).is_none());
    }

    #[test]
    fn legacy_samples_remain_readable_but_cannot_join_without_generation() {
        let sample: PluginPresenceSample = serde_json::from_str(
            r#"{"at_ms":1,"session_name":"room","pages":2,"bytes":131072,"uptime_ms":1,"commands":3,"commands_failed":1}"#,
        )
        .unwrap();
        assert_eq!(sample.plugin_id, None);
        assert_eq!(sample.topology_failures, None);
    }
}
