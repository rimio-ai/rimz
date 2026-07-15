//! Opt-in, secret-free account-refresh timing trace.

use std::ffi::OsStr;
use std::path::PathBuf;

use serde::Serialize;

use crate::RuntimePaths;

const TRACE_MAX_BYTES: u64 = 1_048_576;

#[derive(Serialize)]
struct TraceRecord<'a> {
    at_ms: u64,
    #[serde(flatten)]
    event: TraceEvent<'a>,
}

#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(super) enum TraceEvent<'a> {
    ProviderProbe {
        kind: &'a str,
        outcome: &'a str,
        account_ms: u64,
        version_ms: u64,
        total_ms: u64,
    },
    ProbeBatch {
        due_count: usize,
        worker_count: usize,
        total_ms: u64,
        success_count: usize,
        unavailable_count: usize,
    },
    Contention {
        outcome: &'a str,
        wait_ms: u64,
    },
    Claim {
        kind: &'a str,
        outcome: &'a str,
        elapsed_ms: u64,
    },
    HelperSpawn {
        kind: &'a str,
        outcome: &'a str,
        elapsed_ms: u64,
    },
    UsageHelper {
        kind: &'a str,
        outcome: &'a str,
        realtime_ms: u64,
        direct_ms: u64,
        cache_publication_ms: u64,
        total_ms: u64,
    },
}

pub(super) fn record<'a>(runtime: &RuntimePaths, event: impl FnOnce() -> TraceEvent<'a>) {
    let Some(path) = trace_path(runtime) else {
        return;
    };
    crate::diag::JsonlLog::new(path, TRACE_MAX_BYTES).append(&TraceRecord {
        at_ms: super::super::timing::unix_now_ms(),
        event: event(),
    });
}

fn trace_path(runtime: &RuntimePaths) -> Option<PathBuf> {
    trace_path_from(
        std::env::var_os("RIMZ_ACCOUNT_REFRESH_TRACE").as_deref(),
        runtime,
    )
}

fn trace_path_from(raw: Option<&OsStr>, runtime: &RuntimePaths) -> Option<PathBuf> {
    let raw = raw?.to_string_lossy();
    let raw = raw.trim();
    if raw.is_empty() || raw == "1" || raw == "true" {
        Some(
            runtime
                .persistent_shared_root
                .join("account_refresh_trace.jsonl"),
        )
    } else {
        Some(PathBuf::from(raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;

    #[test]
    fn trace_toggle_uses_shared_cache_directory_and_path_override() {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();

        assert_eq!(trace_path_from(None, &runtime), None);
        assert_eq!(
            trace_path_from(Some(OsStr::new("true")), &runtime),
            Some(
                runtime
                    .persistent_shared_root
                    .join("account_refresh_trace.jsonl")
            )
        );
        assert_eq!(
            trace_path_from(Some(OsStr::new("/tmp/account.jsonl")), &runtime),
            Some(PathBuf::from("/tmp/account.jsonl"))
        );
    }
}
