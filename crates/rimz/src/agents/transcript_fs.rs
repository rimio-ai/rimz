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

/// The consumable JSONL suffix of `path` past byte `offset`, plus the offset
/// just past what was consumed — the incremental read every spend parser
/// shares. Consumes every newline-terminated line, and the trailing fragment
/// only when it is complete JSON: a writer appends whole lines, so a torn
/// write is a strict prefix of a JSON document and never parses — it stays
/// unconsumed for the next pass (the event log's torn-line discipline), while
/// a final line still missing only its newline is counted without waiting.
/// `None` on any IO error or when nothing consumable lies past `offset`.
pub(crate) fn read_spend_lines(path: &Path, offset: u64) -> Option<(Vec<u8>, u64)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    let complete = match buf.iter().rposition(|&b| b == b'\n') {
        Some(last_newline) => last_newline + 1,
        None => 0,
    };
    let fragment = &buf[complete..];
    let consumed = if !fragment.is_empty()
        && serde_json::from_slice::<serde::de::IgnoredAny>(fragment).is_ok()
    {
        buf.len()
    } else {
        complete
    };
    if consumed == 0 {
        return None;
    }
    buf.truncate(consumed);
    Some((buf, offset + consumed as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn read_spend_lines_consumes_lines_and_complete_json_fragments_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        // One full line, then a torn write (a strict JSON prefix).
        f.write_all(b"{\"a\":1}\n{\"b\":").unwrap();
        let (buf, next) = read_spend_lines(&path, 0).expect("the full line is consumable");
        assert_eq!(buf, b"{\"a\":1}\n");
        assert_eq!(next, 8, "the torn fragment stays unconsumed");

        // The tear heals: the rest of the line lands. No newline yet, but the
        // fragment is complete JSON, so it is counted without waiting.
        f.write_all(b"2}").unwrap();
        let (buf, next) = read_spend_lines(&path, next).expect("the healed line is consumable");
        assert_eq!(buf, b"{\"b\":2}");
        assert_eq!(next, 15);

        // Nothing new past the cursor.
        assert!(read_spend_lines(&path, next).is_none());
    }
}
