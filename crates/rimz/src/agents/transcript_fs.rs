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
//! store writes, run-wake, or broker imports (CI grep).
//!
//! [`AgentAdapter::transcript_files`]: super::AgentAdapter::transcript_files

use std::fs;
use std::marker::PhantomData;
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

/// Deserialize an optional object while treating non-object JSON values as absent.
pub(crate) fn deserialize_optional_object_lossy<'de, D, T>(
    deserializer: D,
) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    struct Visitor<T>(PhantomData<T>);

    impl<'de, T: serde::Deserialize<'de>> serde::de::Visitor<'de> for Visitor<T> {
        type Value = Option<T>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an optional object")
        }

        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
            Ok(None)
        }

        fn visit_some<D: serde::Deserializer<'de>>(
            self,
            deserializer: D,
        ) -> Result<Self::Value, D::Error> {
            deserialize_optional_object_lossy(deserializer)
        }

        fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
            T::deserialize(serde::de::value::MapAccessDeserializer::new(map)).map(Some)
        }
    }

    deserializer.deserialize_any(Visitor(PhantomData))
}

/// Deserialize an optional unsigned integer from a JSON number or numeric string.
pub(crate) fn deserialize_optional_u64_lossy<'de, D>(
    deserializer: D,
) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <Option<serde_json::Value> as serde::Deserialize>::deserialize(deserializer)?;
    Ok(value.and_then(|value| match value {
        serde_json::Value::Number(value) => value.as_u64(),
        serde_json::Value::String(value) => value.trim().parse().ok(),
        _ => None,
    }))
}

/// Deserialize an optional finite float from a JSON number or numeric string.
pub(crate) fn deserialize_optional_f64_lossy<'de, D>(
    deserializer: D,
) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <Option<serde_json::Value> as serde::Deserialize>::deserialize(deserializer)?;
    Ok(value
        .and_then(|value| match value {
            serde_json::Value::Number(value) => value.as_f64(),
            serde_json::Value::String(value) => value.trim().parse().ok(),
            _ => None,
        })
        .filter(|value| value.is_finite()))
}

/// Deserialize a non-empty optional string while treating other JSON values as absent.
pub(crate) fn deserialize_optional_string_lossy<'de, D>(
    deserializer: D,
) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <Option<serde_json::Value> as serde::Deserialize>::deserialize(deserializer)?;
    Ok(value.and_then(|value| match value {
        serde_json::Value::String(value) => {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        }
        _ => None,
    }))
}

/// Read the trailing window of a transcript/rollout JSONL as lossy UTF-8, for
/// tail-scanning the most recent records newest-first. The suffix starts at a
/// record boundary. A newest record larger than the normal budget expands the
/// read far enough to return that record whole; a valid final record needs no
/// newline, while a torn final fragment stays out of the result.
pub(crate) fn read_transcript_tail(path: &Path) -> Option<String> {
    read_transcript_tail_with_status(path).map(|tail| tail.text)
}

pub(crate) struct TranscriptTail {
    pub(crate) text: String,
    pub(crate) torn_suffix: bool,
}

/// Length of the consumable JSONL prefix plus whether the unconsumed suffix
/// contains non-whitespace bytes. Newline-terminated records are complete by
/// the writer contract; a final record without a newline is complete only when
/// it parses as one JSON value.
fn complete_jsonl_prefix(bytes: &[u8]) -> (usize, bool) {
    let complete = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |newline| newline + 1);
    let fragment = &bytes[complete..];
    if !fragment.is_empty() && serde_json::from_slice::<serde::de::IgnoredAny>(fragment).is_ok() {
        return (bytes.len(), false);
    }
    (
        complete,
        fragment.iter().any(|byte| !byte.is_ascii_whitespace()),
    )
}

/// The bounded transcript tail plus whether an incomplete final record was
/// excluded. Cursor uses the extra bit to prove its transcript is resting at
/// a terminal row; other adapters retain the string-only wrapper above.
pub(crate) fn read_transcript_tail_with_status(path: &Path) -> Option<TranscriptTail> {
    use std::io::{Read, Seek, SeekFrom};

    const TAIL_BYTES: u64 = 64 * 1024;
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let normal_start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(normal_start)).ok()?;
    let mut buf = Vec::with_capacity(usize::try_from(len - normal_start).unwrap_or(0));
    file.by_ref()
        .take(len - normal_start)
        .read_to_end(&mut buf)
        .ok()?;

    let starts_at_boundary = if normal_start == 0 {
        true
    } else {
        file.seek(SeekFrom::Start(normal_start - 1)).ok()?;
        let mut previous = [0];
        file.read_exact(&mut previous).ok()?;
        previous[0] == b'\n'
    };
    let mut completeness = None;
    if !starts_at_boundary {
        let discard_partial = buf
            .iter()
            .position(|byte| *byte == b'\n')
            .and_then(|newline| {
                let remainder = &buf[newline + 1..];
                let status = complete_jsonl_prefix(remainder);
                (remainder.contains(&b'\n')
                    || (!remainder.is_empty() && status.0 == remainder.len()))
                .then_some((newline, status))
            });
        if let Some((newline, status)) = discard_partial {
            buf.drain(..=newline);
            completeness = Some(status);
        } else {
            let mut record_start = 0;
            let mut scan_end = normal_start;
            let mut chunk = Vec::with_capacity(TAIL_BYTES as usize);
            while scan_end > 0 {
                let scan_start = scan_end.saturating_sub(TAIL_BYTES);
                chunk.resize(usize::try_from(scan_end - scan_start).ok()?, 0);
                file.seek(SeekFrom::Start(scan_start)).ok()?;
                file.read_exact(&mut chunk).ok()?;
                if let Some(newline) = chunk.iter().rposition(|byte| *byte == b'\n') {
                    record_start = scan_start + newline as u64 + 1;
                    break;
                }
                scan_end = scan_start;
            }
            buf.clear();
            file.seek(SeekFrom::Start(record_start)).ok()?;
            file.take(len - record_start).read_to_end(&mut buf).ok()?;
        }
    }

    let (complete, torn_suffix) = completeness.unwrap_or_else(|| complete_jsonl_prefix(&buf));
    buf.truncate(complete);
    Some(TranscriptTail {
        text: String::from_utf8_lossy(&buf).into_owned(),
        torn_suffix,
    })
}

/// Read a torn-write-safe JSONL suffix from a transcript path, returning the
/// consumed bytes and next cursor offset. Same cursor discipline as spending,
/// exposed for `rimz agents wait --stream` without making the helper module public.
pub fn read_transcript_lines(path: &Path, offset: u64) -> Option<(Vec<u8>, u64)> {
    read_spend_lines(path, offset)
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
    let (consumed, _) = complete_jsonl_prefix(&buf);
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

    #[test]
    fn transcript_tail_starts_at_a_complete_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        let padding = "x".repeat(70 * 1024);
        fs::write(
            &path,
            format!("{{\"old\":{padding:?}}}\n{{\"new\":true}}\n"),
        )
        .unwrap();
        assert_eq!(read_transcript_tail(&path).unwrap(), "{\"new\":true}\n");
    }

    #[test]
    fn transcript_tail_expands_for_a_large_newest_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        let content = "y".repeat(70 * 1024);
        let newest = format!("{{\"content\":{content:?}}}");
        fs::write(&path, format!("{{\"old\":true}}\n{newest}")).unwrap();
        assert_eq!(read_transcript_tail(&path).unwrap(), newest);
        fs::write(&path, format!("{{\"old\":true}}\n{newest}\n")).unwrap();
        assert_eq!(read_transcript_tail(&path).unwrap(), format!("{newest}\n"));
    }

    #[test]
    fn transcript_tail_reads_a_three_chunk_newest_record_once_and_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        let content = "z".repeat(3 * 64 * 1024 + 17);
        let newest = format!("{{\"content\":{content:?}}}");
        fs::write(&path, format!("{{\"old\":true}}\n{newest}")).unwrap();

        let tail = read_transcript_tail_with_status(&path).unwrap();
        assert_eq!(tail.text, newest);
        assert!(!tail.torn_suffix);
    }

    #[test]
    fn transcript_tail_keeps_a_multichunk_record_before_a_torn_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        let content = "z".repeat(3 * 64 * 1024 + 17);
        let newest_complete = format!("{{\"content\":{content:?}}}\n");
        fs::write(
            &path,
            format!("{{\"old\":true}}\n{newest_complete}{{\"torn\":"),
        )
        .unwrap();

        let tail = read_transcript_tail_with_status(&path).unwrap();
        assert_eq!(tail.text, newest_complete);
        assert!(tail.torn_suffix);
    }

    #[test]
    fn transcript_tail_keeps_valid_no_newline_and_drops_torn_fragment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        fs::write(&path, "{\"a\":1}\n{\"b\":2}").unwrap();
        assert_eq!(read_transcript_tail(&path).unwrap(), "{\"a\":1}\n{\"b\":2}");
        fs::write(&path, "{\"a\":1}\n{\"b\":").unwrap();
        assert_eq!(read_transcript_tail(&path).unwrap(), "{\"a\":1}\n");
    }

    #[test]
    fn transcript_tail_reports_only_a_nonempty_torn_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");

        for (content, text, torn_suffix) in [
            ("{\"a\":1}\n", "{\"a\":1}\n", false),
            ("{\"a\":1}", "{\"a\":1}", false),
            ("{\"a\":1}\n{\"b\":", "{\"a\":1}\n", true),
            ("{\"a\":1}\n   ", "{\"a\":1}\n", false),
        ] {
            fs::write(&path, content).unwrap();
            let tail = read_transcript_tail_with_status(&path).unwrap();
            assert_eq!(tail.text, text, "{content:?}");
            assert_eq!(tail.torn_suffix, torn_suffix, "{content:?}");
        }
    }

    #[test]
    fn jsonl_completeness_pins_whitespace_and_incremental_fragments() {
        assert_eq!(complete_jsonl_prefix(b" \t "), (0, false));
        assert_eq!(complete_jsonl_prefix(b"{\"a\":1}"), (7, false));
        assert_eq!(complete_jsonl_prefix(b"{\"a\":"), (0, true));
        assert_eq!(complete_jsonl_prefix(b"{\"a\":1}\n{\"b\":"), (8, true));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        fs::write(&path, b" \t ").unwrap();
        let tail = read_transcript_tail_with_status(&path).unwrap();
        assert_eq!(tail.text, "");
        assert!(!tail.torn_suffix);
        assert!(read_spend_lines(&path, 0).is_none());
    }
}
