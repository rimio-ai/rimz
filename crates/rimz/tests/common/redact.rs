//! Shared snapshot redactor.
//!
//! `docs/contributing/testing.md` mandates one shared place for stripping the
//! transient fields that appear in snapshot output. Everything that would
//! cause churn — UUIDs, RFC3339 timestamps, absolute paths under a
//! `TempDir`, and Rimz's typed ID prefixes — collapses to a stable token.
//!
//! Callers run output through [`redact`] (or [`redact_under`] when they have
//! a specific `TempDir` to scrub) before passing it to `insta::assert_*`.

#![allow(dead_code)]

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static regex compiles")
}

fn uuid_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        re(r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b")
    })
}

fn uuid_simple_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| re(r"\b[0-9a-fA-F]{32}\b"))
}

fn rfc3339_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| re(r"\b\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})\b"))
}

fn prefixed_id_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| re(r"\b(req|evt|ws|sb)_[0-9a-fA-F]{12,32}\b"))
}

/// Strip every transient marker described in the module docs.
pub fn redact(input: &str) -> String {
    let s = prefixed_id_re().replace_all(input, "${1}_<id>");
    let s = uuid_re().replace_all(&s, "<uuid>");
    let s = uuid_simple_re().replace_all(&s, "<uuid>");
    let s = rfc3339_re().replace_all(&s, "<ts>");
    s.into_owned()
}

/// Same as [`redact`] but additionally collapses every occurrence of the
/// given temp-dir path (and its canonicalised form, if different) into the
/// literal `<tempdir>`.
pub fn redact_under(tempdir: &Path, input: &str) -> String {
    let mut out = redact(input);
    let raw = tempdir.to_string_lossy().to_string();
    if !raw.is_empty() {
        out = out.replace(&raw, "<tempdir>");
    }
    if let Ok(canonical) = tempdir.canonicalize() {
        let canon = canonical.to_string_lossy().to_string();
        if !canon.is_empty() && canon != raw {
            out = out.replace(&canon, "<tempdir>");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_uuid_timestamp_and_typed_ids() {
        let input = "req_0123456789abcdef0123456789abcdef at 2026-05-21T10:00:00Z \
             id 11111111-2222-3333-4444-555555555555 sb_aaaaaaaaaaaaaaaa";
        let red = redact(input);
        assert!(red.contains("req_<id>"));
        assert!(red.contains("<ts>"));
        assert!(red.contains("<uuid>"));
        assert!(red.contains("sb_<id>"));
    }

    #[test]
    fn redacts_tempdir_paths() {
        let red = redact_under(
            Path::new("/tmp/rimz-abc"),
            "wrote /tmp/rimz-abc/feed/x.json",
        );
        assert_eq!(red, "wrote <tempdir>/feed/x.json");
    }
}
