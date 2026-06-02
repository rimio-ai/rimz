//! Per-provider typed parsers for the agent transcript-history read-path.
//!
//! Each submodule owns the typed deserialization structs, path discovery, and
//! JSONL parser for one agent provider ([`claude`], [`codex`], [`pi`]).  Shared
//! filesystem utilities live here.
//!
//! ## Staged ahead of the consumer — intentional
//!
//! This layer is the typed foundation for a forthcoming **deeper analysis of
//! the agent transcript-history files** (per-model token rollups, cost
//! attribution, session timelines).  The parsers and type system land *first*,
//! fully unit-tested, so that analysis logic can build on a stable, reviewed
//! base — rather than growing the schema and the consumer in one churny change.
//!
//! Today exactly one consumer is wired: [`super::spending::compute_spending`]
//! reads the [`claude`] parser to produce the sidebar's today / week / month
//! spend.  The [`codex`] and [`pi`] parsers are complete and tested but **not
//! yet consumed in the live path** — that is deliberate, not dead code:
//!
//! - [`pi`] yields a `costUSD` directly and only awaits the upcoming consumer.
//! - [`codex`] JSONL carries token counts, not `costUSD`, so turning its events
//!   into dollars additionally needs a per-model pricing table (also pending).
//!
//! ## Not the hook-decision adapter
//!
//! Distinct from the [`AgentIntegration`](crate::agents::AgentIntegration)
//! "adapter" (the hook/decision boundary described in `agents/CLAUDE.md`): that
//! normalizes the *decision channel*; this normalizes the *transcript/usage
//! read-path*.  Same provider, different surface.

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
