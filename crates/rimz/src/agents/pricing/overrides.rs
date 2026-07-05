use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

const FAST_MULTIPLIER_OVERRIDES_JSON: &str = include_str!("fast-multiplier-overrides.json");

#[derive(Debug, Default, Deserialize)]
struct FastMultiplierOverrides {
    exact: HashMap<String, f64>,
    normalized_prefix: HashMap<String, f64>,
}

pub(super) fn multiplier_for(model: &str) -> Option<f64> {
    static OVERRIDES: OnceLock<FastMultiplierOverrides> = OnceLock::new();
    let overrides = OVERRIDES.get_or_init(|| {
        serde_json::from_str(FAST_MULTIPLIER_OVERRIDES_JSON)
            .expect("parse embedded fast-multiplier-overrides.json")
    });
    overrides.multiplier_for(model)
}

impl FastMultiplierOverrides {
    fn multiplier_for(&self, model: &str) -> Option<f64> {
        if let Some(multiplier) = self.exact.get(model) {
            return Some(*multiplier);
        }
        let normalized = model.replace(['.', '@'], "-");
        normalized.split(['/', ':']).find_map(|part| {
            self.normalized_prefix
                .iter()
                .find_map(|(base, multiplier)| {
                    matches_model_suffix(part, base).then_some(*multiplier)
                })
        })
    }
}

fn matches_model_suffix(part: &str, base: &str) -> bool {
    let Some(index) = part.rfind(base) else {
        return false;
    };
    let suffix = &part[index..];
    suffix == base || suffix.as_bytes().get(base.len()) == Some(&b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiplier_matches_exact_and_normalized_suffixes() {
        assert_eq!(multiplier_for("gpt-5.5"), Some(2.5));
        assert_eq!(
            multiplier_for("openrouter/anthropic/claude-opus-4.7"),
            Some(6.0)
        );
        assert_eq!(multiplier_for("claude-opus-4-70"), None);
    }
}
