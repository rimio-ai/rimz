//! Embedded provider-emblem catalog and shared fallback resolution.

use std::collections::BTreeMap;
use std::sync::OnceLock;

const CATALOG_SOURCE: &str = include_str!("emblems.toml");

fn catalog() -> &'static BTreeMap<String, Vec<String>> {
    static CATALOG: OnceLock<BTreeMap<String, Vec<String>>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let raw: BTreeMap<String, String> =
            toml::from_str(CATALOG_SOURCE).expect("embedded emblem catalog must be valid TOML");
        raw.into_iter()
            .map(|(kind, art)| {
                let lines = art
                    .trim_matches('\n')
                    .lines()
                    .map(ToOwned::to_owned)
                    .collect();
                (kind, lines)
            })
            .collect()
    })
}

/// Return the curated emblem for `kind`, or the shared fallback.
pub fn emblem_lines(kind: &str) -> Vec<String> {
    catalog().get(kind).cloned().unwrap_or_else(fallback_emblem)
}

/// Return the shared emblem used for kinds without curated art.
pub fn fallback_emblem() -> Vec<String> {
    // The checked-in catalog always carries the shared fallback entry.
    catalog()
        .get("fallback")
        .expect("embedded emblem catalog must contain `fallback`")
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CURATED_KINDS: [&str; 4] = ["claude", "codex", "pi", "opencode"];

    #[test]
    fn fallback_is_present_and_non_empty() {
        assert!(!fallback_emblem().is_empty());
    }

    #[test]
    fn curated_emblems_are_present_and_distinct_from_fallback() {
        let fallback = fallback_emblem();
        for kind in CURATED_KINDS {
            let art = emblem_lines(kind);
            assert!(!art.is_empty(), "{kind} emblem must not be empty");
            assert_ne!(art, fallback, "{kind} must have curated art");
        }
    }

    #[test]
    fn unknown_kind_uses_fallback() {
        assert_eq!(emblem_lines("does-not-exist"), fallback_emblem());
    }
}
