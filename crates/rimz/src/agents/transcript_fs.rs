//! Shared filesystem utilities for the per-adapter full-history spend parsers.
//!
//! Each adapter's `spend.rs` owns its typed deserialization, path discovery,
//! and JSONL parser ([`AgentAdapter::transcript_files`] /
//! [`AgentAdapter::parse_spend`](super::AgentAdapter::parse_spend)); the
//! consumer is [`super::spending`]. The walk helpers they share live here.
//!
//! This is the *full-history* read — distinct from the bounded-tail context
//! gauge each adapter reads in its `observe_lifecycle`: that scans only the
//! trailing window for the live row's `context_pct`/`total_tokens`; this walks
//! the whole log for spend. Spend parsers are read-only and sidebar-safe — no
//! ledger writes, bridge, or broker imports (CI grep).
//!
//! [`AgentAdapter::transcript_files`]: super::AgentAdapter::transcript_files

use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"))
}

pub(crate) fn expand_tilde(raw: &str) -> PathBuf {
    if raw == "~" {
        return home_dir();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(raw)
}

pub(crate) fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for entry in rd.filter_map(|e| e.ok()) {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        } else if ft.is_dir() {
            collect_jsonl(&path, out);
        }
    }
}

pub(crate) fn bytes_contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.len() > haystack.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}
