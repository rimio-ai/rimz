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
//! ## Staged ahead of the consumer — intentional
//!
//! The parsers and type system land *first*, fully unit-tested, as the typed
//! foundation for a forthcoming deeper transcript-history analysis (per-model
//! token rollups, cost attribution, session timelines) — rather than growing the
//! schema and the consumer in one churny change. Today only
//! [`super::spending::compute_spending`] reads the [`claude`] parser (the
//! sidebar's today / week / month spend); the [`codex`] and [`pi`] parsers are
//! complete and tested but **not yet consumed** — deliberate, not dead code:
//!
//! - [`pi`] yields a `costUSD` directly and only awaits the upcoming consumer.
//! - [`codex`] JSONL carries token counts, not `costUSD`, so turning its events
//!   into dollars additionally needs a per-model pricing table (also pending).

pub mod claude;
pub mod codex;
pub mod pi;

use std::fs;
use std::path::{Path, PathBuf};

/// Inferred provider from a JSONL file path.
pub(super) enum Provider {
    Claude,
    Pi,
    /// No recognizable path hint; treated as Claude format (covers test paths).
    Unknown,
}

pub(super) fn detect_provider(path: &Path) -> Provider {
    let s = path.to_string_lossy();
    if s.contains("/.claude/") || s.contains("/.config/claude/") {
        Provider::Claude
    } else if s.contains("/.pi/") {
        Provider::Pi
    } else {
        Provider::Unknown
    }
}

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
