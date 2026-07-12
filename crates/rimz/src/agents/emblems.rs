//! Embedded provider-emblem catalog and shared fallback resolution.

use std::collections::BTreeMap;
use std::sync::OnceLock;

const CATALOG_SOURCE: &str = include_str!("emblems.toml");

/// The parsed catalog: curated per-kind art keyed by agent kind, plus the
/// shared `fallback` pool that covers kinds without their own emblem.
struct Catalog {
    curated: BTreeMap<String, Vec<String>>,
    fallback: Vec<Vec<String>>,
}

fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let raw: BTreeMap<String, toml::Value> =
            toml::from_str(CATALOG_SOURCE).expect("embedded emblem catalog must be valid TOML");
        let mut curated = BTreeMap::new();
        let mut fallback = Vec::new();
        for (key, value) in raw {
            if key == "fallback" {
                fallback = value
                    .as_array()
                    .expect("`fallback` must be an array of emblems")
                    .iter()
                    .map(|entry| {
                        emblem_from(entry.as_str().expect("fallback emblem must be a string"))
                    })
                    .collect();
            } else {
                let art = value.as_str().expect("curated emblem must be a string");
                curated.insert(key, emblem_from(art));
            }
        }
        assert!(
            !fallback.is_empty(),
            "embedded emblem catalog must carry a non-empty `fallback` pool"
        );
        Catalog { curated, fallback }
    })
}

/// Split a stored emblem block into its rendered lines.
fn emblem_from(art: &str) -> Vec<String> {
    art.trim_matches('\n')
        .lines()
        .map(ToOwned::to_owned)
        .collect()
}

/// Return the curated emblem for `kind`, or the shared fallback.
pub fn emblem_lines(kind: &str) -> Vec<String> {
    catalog()
        .curated
        .get(kind)
        .cloned()
        .unwrap_or_else(fallback_emblem)
}

/// Return the shared emblem used for kinds without curated art.
pub fn fallback_emblem() -> Vec<String> {
    // `catalog()` guarantees the fallback pool is non-empty.
    catalog().fallback[0].clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::registry::ADAPTERS;

    #[test]
    fn fallback_is_present_and_non_empty() {
        assert!(!fallback_emblem().is_empty());
    }

    #[test]
    fn every_provider_has_curated_art_distinct_from_fallback() {
        let fallback = fallback_emblem();
        for adapter in ADAPTERS {
            let kind = adapter.descriptor().kind;
            let art = catalog().curated.get(kind);
            assert!(art.is_some(), "{kind} must have a curated emblem");
            let art = art.expect("checked above");
            assert!(!art.is_empty(), "{kind} emblem must not be empty");
            assert_ne!(art, &fallback, "{kind} must have art of its own");
        }
    }

    #[test]
    fn unknown_kind_uses_fallback() {
        assert_eq!(emblem_lines("does-not-exist"), fallback_emblem());
    }
}
