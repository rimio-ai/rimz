//! Durable Zellij presence-plugin telemetry log.
//!
//! The presence plugin runs inside the Zellij server process, so samples written
//! by its keepalive distinguish plugin WASM linear-memory growth from
//! Zellij-native RSS growth. The log is diagnostic state: append-only within a
//! size cap, never read by correctness code.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::sidebar::presence::PluginCommandFailure;

const PLUGIN_PRESENCE_LOG_NAME: &str = "plugin-presence.log.jsonl";
const PLUGIN_PRESENCE_LOG_MAX_BYTES: u64 = 1_048_576;
pub(crate) const WASM_PAGE_BYTES: u64 = 65_536;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct PluginPresenceSample {
    pub at_ms: u64,
    pub session_name: Option<String>,
    #[serde(default)]
    pub plugin_id: Option<u32>,
    #[serde(default)]
    pub build: Option<String>,
    #[serde(default)]
    pub loaded_at_ms: u64,
    pub pages: u64,
    pub bytes: u64,
    pub uptime_ms: u64,
    pub commands: u64,
    #[serde(default)]
    pub commands_succeeded: Option<u64>,
    #[serde(default)]
    pub stale_writer_rejections: Option<u64>,
    #[serde(default)]
    pub topology_failures: Option<u64>,
    #[serde(default)]
    pub other_failures: Option<u64>,
    #[serde(default)]
    pub zellij_version: Option<String>,
    #[serde(default)]
    pub last_failure: Option<PluginCommandFailure>,
}

fn log_path(state_root: &Path) -> PathBuf {
    state_root.join(PLUGIN_PRESENCE_LOG_NAME)
}

pub(crate) fn append(state_root: &Path, sample: &PluginPresenceSample) {
    super::rotating::append(&log_path(state_root), PLUGIN_PRESENCE_LOG_MAX_BYTES, sample);
}

/// Existing current and rotated telemetry logs, in display order.
pub fn history_paths(state_root: &Path) -> Vec<PathBuf> {
    let current = log_path(state_root);
    let rotated = super::rotating::rotated_path(&current);
    [current, rotated]
        .into_iter()
        .filter(|path| path.is_file())
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginPresenceSpan {
    pub plugin_id: u32,
    pub loaded_at_ms: u64,
    pub build: Option<String>,
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
    pub last_failure: Option<PluginCommandFailure>,
}

pub fn recent_generations(state_root: &Path, session_name: &str) -> Vec<PluginPresenceSpan> {
    let mut grouped = BTreeMap::<(u32, u64), Vec<PluginPresenceSample>>::new();
    for sample in recent_samples(state_root, session_name) {
        let Some(plugin_id) = sample.plugin_id else {
            continue;
        };
        grouped
            .entry((plugin_id, sample.loaded_at_ms))
            .or_default()
            .push(sample);
    }
    grouped
        .into_iter()
        .filter_map(|((plugin_id, loaded_at_ms), samples)| {
            span_from_samples(plugin_id, loaded_at_ms, samples)
        })
        .collect()
}

fn recent_samples(state_root: &Path, session_name: &str) -> Vec<PluginPresenceSample> {
    let mut samples = Vec::new();
    super::rotating::visit_records(&log_path(state_root), |sample: PluginPresenceSample| {
        if sample.session_name.as_deref() == Some(session_name) {
            samples.push(sample);
        }
    });
    samples
}

fn span_from_samples(
    plugin_id: u32,
    loaded_at_ms: u64,
    mut samples: Vec<PluginPresenceSample>,
) -> Option<PluginPresenceSpan> {
    samples.sort_by_key(|sample| sample.at_ms);
    let first = samples.first()?;
    let last = samples.last()?;
    Some(PluginPresenceSpan {
        plugin_id,
        loaded_at_ms,
        build: last.build.clone(),
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
        // The cause has to explain the counters beside it, so it must come from
        // the span those counters measure: a failure predating the span belongs
        // to a window this one already recovered from. The plugin retains its
        // last failure across recoveries, so the newest sample usually carries
        // it; scanning back covers samples written before the plugin retained.
        // An unstamped cause comes from a plugin older than the stamp and stays
        // eligible, since dropping it would lose the only account there is.
        last_failure: samples
            .iter()
            .rev()
            .filter_map(|sample| sample.last_failure.clone())
            .find(|failure| failure.at_ms.is_none_or(|at_ms| at_ms >= first.at_ms)),
    })
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

    fn generation_span(
        state_root: &Path,
        session_name: &str,
        plugin_id: u32,
        loaded_at_ms: u64,
    ) -> Option<PluginPresenceSpan> {
        recent_generations(state_root, session_name)
            .into_iter()
            .find(|span| span.plugin_id == plugin_id && span.loaded_at_ms == loaded_at_ms)
    }

    #[test]
    fn generation_span_reads_rotated_and_current_and_ignores_malformed_tail() {
        let dir = tempfile::tempdir().unwrap();
        let current = log_path(dir.path());
        let rotated = super::super::rotating::rotated_path(&current);
        let sample = |at_ms, pages, failures| {
            serde_json::to_string(&PluginPresenceSample {
                at_ms,
                session_name: Some("room".to_owned()),
                plugin_id: Some(7),
                build: Some("wasm-build".to_owned()),
                loaded_at_ms: 100,
                pages,
                bytes: pages.saturating_mul(WASM_PAGE_BYTES),
                uptime_ms: at_ms,
                commands: at_ms / 10,
                commands_succeeded: Some(at_ms / 10 - failures),
                stale_writer_rejections: Some(1),
                topology_failures: Some(failures),
                other_failures: Some(0),
                zellij_version: Some("0.44.3".to_owned()),
                last_failure: None,
            })
            .unwrap()
        };
        std::fs::write(&rotated, format!("{}\n", sample(100, 2, 0))).unwrap();
        std::fs::write(&current, format!("{}\nmalformed", sample(200, 5, 2))).unwrap();

        assert_eq!(
            history_paths(dir.path()),
            vec![current.clone(), rotated.clone()]
        );

        let span = generation_span(dir.path(), "room", 7, 100).unwrap();
        assert_eq!(span.sample_count, 2);
        assert_eq!(span.build.as_deref(), Some("wasm-build"));
        assert_eq!(span.page_growth, 3);
        assert_eq!(span.topology_failures_delta, Some(2));
        assert!(generation_span(dir.path(), "other", 7, 100).is_none());
    }

    /// The cause has to describe the same window as the counters printed beside
    /// it. The plugin retains its last failure across recoveries, so a span that
    /// recovered long ago still carries one, and printing it would answer a
    /// question about this hour with an explanation from a previous one.
    #[test]
    fn generation_span_takes_its_cause_from_the_window_its_counters_measure() {
        let dir = tempfile::tempdir().unwrap();
        let current = log_path(dir.path());
        let sample = |at_ms: u64, failure: Option<PluginCommandFailure>| {
            serde_json::to_string(&PluginPresenceSample {
                at_ms,
                session_name: Some("room".to_owned()),
                plugin_id: Some(7),
                build: Some("wasm-build".to_owned()),
                loaded_at_ms: 100,
                pages: 2,
                bytes: 2 * WASM_PAGE_BYTES,
                uptime_ms: at_ms,
                commands: at_ms,
                commands_succeeded: Some(at_ms),
                stale_writer_rejections: Some(0),
                topology_failures: Some(0),
                other_failures: Some(0),
                zellij_version: Some("0.44.3".to_owned()),
                last_failure: failure,
            })
            .unwrap()
        };
        let failure = |at_ms: Option<u64>| {
            Some(PluginCommandFailure {
                exit_code: Some(2),
                detail: format!("failed at {at_ms:?}"),
                at_ms,
            })
        };

        // The span runs 1000..1200; the retained cause predates it.
        std::fs::write(
            &current,
            format!(
                "{}\n{}\n",
                sample(1_000, failure(Some(400))),
                sample(1_200, failure(Some(400))),
            ),
        )
        .unwrap();
        assert_eq!(
            generation_span(dir.path(), "room", 7, 100)
                .unwrap()
                .last_failure,
            None,
            "a cause older than the span explains a window that already closed",
        );

        // A cause raised inside the span is the one the counters are about.
        std::fs::write(
            &current,
            format!(
                "{}\n{}\n",
                sample(1_000, failure(Some(400))),
                sample(1_200, failure(Some(1_100))),
            ),
        )
        .unwrap();
        assert_eq!(
            generation_span(dir.path(), "room", 7, 100)
                .unwrap()
                .last_failure
                .map(|failure| failure.detail),
            Some("failed at Some(1100)".to_owned()),
        );

        // A plugin older than the stamp reports no time, and its one account of
        // the failure stays usable rather than being dated to the epoch.
        std::fs::write(
            &current,
            format!(
                "{}\n{}\n",
                sample(1_000, None),
                sample(1_200, failure(None))
            ),
        )
        .unwrap();
        assert_eq!(
            generation_span(dir.path(), "room", 7, 100)
                .unwrap()
                .last_failure
                .map(|failure| failure.detail),
            Some("failed at None".to_owned()),
        );
    }

    #[test]
    fn legacy_samples_remain_readable_but_cannot_join_without_generation() {
        let sample: PluginPresenceSample = serde_json::from_str(
            r#"{"at_ms":1,"session_name":"room","pages":2,"bytes":131072,"uptime_ms":1,"commands":3,"commands_failed":1}"#,
        )
        .unwrap();
        assert_eq!(sample.plugin_id, None);
        assert_eq!(sample.build, None);
        assert_eq!(sample.topology_failures, None);
    }

    #[test]
    fn recent_generations_groups_mixed_writers_and_accepts_buildless_samples() {
        let dir = tempfile::tempdir().unwrap();
        let current = log_path(dir.path());
        std::fs::write(
            current,
            concat!(
                "{\"at_ms\":100,\"session_name\":\"room\",\"plugin_id\":1,\"loaded_at_ms\":10,\"pages\":2,\"bytes\":131072,\"uptime_ms\":1,\"commands\":1,\"commands_failed\":0}\n",
                "{\"at_ms\":200,\"session_name\":\"room\",\"plugin_id\":2,\"build\":\"new-build\",\"loaded_at_ms\":20,\"pages\":3,\"bytes\":196608,\"uptime_ms\":1,\"commands\":2,\"commands_failed\":0}\n",
                "{\"at_ms\":300,\"session_name\":\"room\",\"plugin_id\":1,\"loaded_at_ms\":10,\"pages\":4,\"bytes\":262144,\"uptime_ms\":2,\"commands\":3,\"commands_failed\":0}\n",
            ),
        )
        .unwrap();

        let spans = recent_generations(dir.path(), "room");

        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].plugin_id, 1);
        assert_eq!(spans[0].sample_count, 2);
        assert_eq!(spans[0].build, None);
        assert_eq!(spans[1].plugin_id, 2);
        assert_eq!(spans[1].build.as_deref(), Some("new-build"));
    }
}
