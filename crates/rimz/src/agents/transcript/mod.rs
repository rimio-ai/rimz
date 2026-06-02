//! Per-provider full-history cost/usage parsing from agent transcripts.
//!
//! Read-only and sidebar-safe: each submodule ([`claude`], [`codex`], [`pi`])
//! owns the typed deserialization, path discovery, and JSONL parser that turns a
//! provider's full session history into cost/token records. The consumer is
//! [`super::spending`]. Shared filesystem utilities live here.
//!
//! This is the *full-history* read — distinct from the bounded-tail context
//! gauge each adapter reads in its `observe_lifecycle` ([`super::claude`],
//! [`super::codex`]): that scans only the trailing window for the live row's
//! `context_pct`/`total_tokens`; this walks the whole log for spend.
//!
//! ## Cost vs. tokens
//!
//! [`super::spending::compute_spending`] consumes all three parsers. [`claude`]
//! and [`pi`] log `costUSD` directly, so their entries carry a cost as parsed.
//! [`codex`] logs only token counts: `spending` multiplies each
//! [`codex::CodexTokenEvent`] through the [`pricing`](super::pricing) table to a
//! USD cost. The parsers here stay pure and network-free — pricing lives in the
//! consumer, so the read-only transcript tree never reaches the network.

pub mod claude;
pub mod codex;
pub mod pi;

use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"))
}

pub(super) fn expand_tilde(raw: &str) -> PathBuf {
    if raw == "~" {
        return home_dir();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(raw)
}

pub(super) fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
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

pub(super) fn bytes_contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.len() > haystack.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}
