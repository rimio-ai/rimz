//! Machine model-tier bindings and the shared definition/launch resolver.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

use crate::agents;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum ModelTier {
    Intern,
    Junior,
    Senior,
    Principal,
}

impl ModelTier {
    pub(super) fn from_model(model: &str) -> Option<Self> {
        model.parse().ok()
    }
}

impl std::str::FromStr for ModelTier {
    type Err = TierError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "intern" => Ok(Self::Intern),
            "junior" => Ok(Self::Junior),
            "senior" => Ok(Self::Senior),
            "principal" => Ok(Self::Principal),
            _ => Err(TierError(unknown_key(value, false))),
        }
    }
}

impl std::fmt::Display for ModelTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Intern => "intern",
            Self::Junior => "junior",
            Self::Senior => "senior",
            Self::Principal => "principal",
        })
    }
}

pub(super) fn relative_effort(effort: &str) -> bool {
    effort
        .strip_prefix(['+', '-'])
        .is_some_and(|steps| !steps.is_empty() && steps.bytes().all(|byte| byte.is_ascii_digit()))
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TierProvenance {
    pub tier: ModelTier,
    /// A higher tier supplied the model.
    pub fell_back: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefinitionRenders {
    pub preference: TierPreference,
    pub renders: BTreeMap<String, super::Profile>,
    pub exclusions: BTreeMap<String, String>,
    pub effort: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TierPreference {
    pub model: Option<String>,
    pub family: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TierPick<R> {
    pub kind: String,
    pub model: String,
    pub used_tier: Option<ModelTier>,
    pub skipped: Vec<(String, R)>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TierConfig {
    pub routing: Routing,
    #[serde(flatten)]
    rows: BTreeMap<ModelTier, Vec<String>>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Routing {
    #[default]
    Fallback,
    Off,
}

impl Default for TierConfig {
    fn default() -> Self {
        Self {
            routing: Routing::Fallback,
            rows: [
                (ModelTier::Intern, vec!["luna", "haiku"]),
                (ModelTier::Junior, vec!["sol", "sonnet"]),
                (ModelTier::Senior, vec!["opus", "astra"]),
                (ModelTier::Principal, vec!["fable"]),
            ]
            .into_iter()
            .map(|(tier, models)| (tier, models.into_iter().map(str::to_owned).collect()))
            .collect(),
        }
    }
}

impl<'de> Deserialize<'de> for TierConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let rows = BTreeMap::<String, toml::Value>::deserialize(deserializer)?;
        let mut table = Self::default();
        for (key, value) in rows {
            if key == "routing" {
                table.routing = value.try_into().map_err(serde::de::Error::custom)?;
                continue;
            }
            let tier: ModelTier = key
                .parse()
                .map_err(|_| serde::de::Error::custom(unknown_key(&key, true)))?;
            if value.is_table() {
                let models = table.rows[&tier]
                    .iter()
                    .map(|model| format!("{model:?}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(serde::de::Error::custom(format!(
                    "tiers.{tier} uses the old table form; use {tier} = [{models}], preference first; effort moved to each definition's `effort:`"
                )));
            }
            let models: Vec<String> = value.try_into().map_err(serde::de::Error::custom)?;
            for model in &models {
                if !matches!(
                    agents::definition_model_kind(model),
                    Some("claude" | "codex")
                ) {
                    return Err(serde::de::Error::custom(format!(
                        "tiers.{tier} entry {model:?}; use a claude or codex alias or full model ID"
                    )));
                }
            }
            table.rows.insert(tier, models);
        }
        let mut seen = BTreeMap::new();
        for (tier, models) in &table.rows {
            for model in models {
                if let Some(previous) = seen.insert(model.as_str(), tier)
                    && previous != tier
                {
                    return Err(serde::de::Error::custom(format!(
                        "model {model:?} appears in both {previous} and {tier}; keep it in one tier"
                    )));
                }
            }
        }
        Ok(table)
    }
}

fn unknown_key(key: &str, config_key: bool) -> String {
    let names =
        &["intern", "junior", "senior", "principal", "routing"][..if config_key { 5 } else { 4 }];
    let nearest = names
        .iter()
        .min_by_key(|name| {
            let mut row: Vec<_> = (0..=name.chars().count()).collect();
            for (i, left) in key.chars().enumerate() {
                let mut diagonal = row[0];
                row[0] = i + 1;
                for (j, right) in name.chars().enumerate() {
                    let above = row[j + 1];
                    row[j + 1] = (diagonal + usize::from(left != right))
                        .min(above + 1)
                        .min(row[j] + 1);
                    diagonal = above;
                }
            }
            row[name.chars().count()]
        })
        .expect("nonempty tier key list");
    format!(
        "unknown tier key {key:?}; did you mean {nearest:?}? Valid keys: {}",
        names.join(", ")
    )
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct TierError(pub(super) String);

impl TierConfig {
    pub fn tier_for_model(&self, kind: &str, model: &str) -> Option<ModelTier> {
        self.rows.iter().find_map(|(tier, row)| {
            row.iter()
                .any(|entry| agents::definition_model_kind(entry) == Some(kind) && entry == model)
                .then_some(*tier)
        })
    }

    pub fn entries(&self, tier: ModelTier) -> impl Iterator<Item = (ModelTier, &str, &str)> {
        self.rows.range(tier..).flat_map(|(tier, row)| {
            row.iter().map(move |model| {
                // Deserialization admits only models with a known family.
                (
                    *tier,
                    agents::definition_model_kind(model).expect("validated tier model"),
                    model.as_str(),
                )
            })
        })
    }

    pub(super) fn walk<R>(
        &self,
        tier: ModelTier,
        preference: &TierPreference,
        eligible: impl Fn(&str) -> bool,
        mut unavailable: impl FnMut(&str, &str) -> Option<R>,
    ) -> Result<TierPick<R>, TierError> {
        let mut first = None;
        let mut skipped = Vec::new();
        for (candidate_tier, row) in self.rows.range(tier..) {
            let mut entries: Vec<_> = row.iter().collect();
            entries.sort_by_key(|model| {
                // Deserialization admits only models with a known family.
                let kind = agents::definition_model_kind(model).expect("validated tier model");
                let exact = *candidate_tier == tier && preference.model.as_ref() == Some(*model);
                if exact {
                    0
                } else if (*candidate_tier != tier || preference.model.is_none())
                    && preference.family.as_deref() == Some(kind)
                {
                    1
                } else {
                    2
                }
            });
            for model in entries {
                let kind = agents::definition_model_kind(model).expect("validated tier model");
                if !eligible(kind) {
                    continue;
                }
                let used_tier = (*candidate_tier != tier).then_some(*candidate_tier);
                first.get_or_insert_with(|| (kind.to_owned(), model.clone(), used_tier));
                if self.routing == Routing::Fallback
                    && let Some(reason) = unavailable(kind, model)
                {
                    skipped.push((model.clone(), reason));
                    continue;
                }
                return Ok(TierPick {
                    kind: kind.to_owned(),
                    model: model.clone(),
                    used_tier,
                    skipped,
                });
            }
        }
        if let Some((kind, model, used_tier)) = first {
            return Ok(TierPick {
                kind,
                model,
                used_tier,
                skipped: Vec::new(),
            });
        }
        Err(TierError(format!(
            "no eligible model at tier {tier} or above"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_preference_leaves_the_remaining_entries_in_list_order() {
        let table: TierConfig =
            toml::from_str("junior = []\nsenior = ['astra', 'opus', 'sonnet']").unwrap();
        let preference = TierPreference {
            model: Some("opus".to_owned()),
            family: Some("claude".to_owned()),
        };
        let pick = table
            .walk(
                ModelTier::Senior,
                &preference,
                |_| true,
                |_, model| (model == "opus").then_some("spent"),
            )
            .unwrap();
        assert_eq!(pick.model, "astra");
    }

    #[test]
    fn ordered_walk_uses_preference_eligibility_and_availability() {
        let table = TierConfig::default();
        for (tier, model, family, expected) in [
            (ModelTier::Senior, None, None, "opus"),
            (ModelTier::Senior, None, Some("codex"), "astra"),
            (ModelTier::Principal, None, Some("codex"), "fable"),
            (ModelTier::Junior, Some("sonnet"), Some("claude"), "sonnet"),
        ] {
            let preference = TierPreference {
                model: model.map(str::to_owned),
                family: family.map(str::to_owned),
            };
            let pick = table.walk(tier, &preference, |_| true, |_, _| None::<()>);
            assert!(pick.is_ok(), "walk must choose {expected}: {pick:?}");
            assert_eq!(pick.unwrap().model, expected);
        }
        let preference = TierPreference::default();
        let pick = table
            .walk(
                ModelTier::Senior,
                &preference,
                |kind| kind == "codex",
                |_, _| None::<()>,
            )
            .unwrap();
        assert_eq!(pick.model, "astra");
        assert_eq!(pick.used_tier, None);
        assert!(pick.skipped.is_empty());
        let pick = table
            .walk(
                ModelTier::Senior,
                &preference,
                |_| true,
                |_, model| (model != "fable").then_some("spent"),
            )
            .unwrap();
        assert_eq!(pick.model, "fable");
        assert_eq!(pick.used_tier, Some(ModelTier::Principal));
        assert_eq!(pick.skipped.len(), 2);
        let pick = table
            .walk(
                ModelTier::Senior,
                &preference,
                |_| true,
                |_, _| Some("spent"),
            )
            .unwrap();
        assert_eq!(pick.model, "opus");
        assert!(pick.skipped.is_empty());
        assert!(
            table
                .walk(ModelTier::Senior, &preference, |_| false, |_, _| None::<()>)
                .is_err()
        );
        let mut off = table;
        off.routing = Routing::Off;
        let pick = off
            .walk(
                ModelTier::Senior,
                &preference,
                |_| true,
                |_, _| Some("spent"),
            )
            .unwrap();
        assert_eq!(pick.model, "opus");
        assert!(pick.skipped.is_empty());
    }

    #[test]
    fn tier_list_errors_name_the_setting_and_fix() {
        for (source, expected) in [
            (
                "[senior]\nclaude = {model = 'opus'}",
                vec!["senior", "senior = [\"opus\", \"astra\"]", "effort:"],
            ),
            (
                "principle = ['fable']",
                vec![
                    "principle",
                    "principal",
                    "intern",
                    "junior",
                    "senior",
                    "routing",
                ],
            ),
            ("routing = 'smart'", vec!["fallback", "off"]),
            ("senior = ['senior']", vec!["senior", "alias", "model ID"]),
            ("senior = ['']", vec!["senior", "alias", "model ID"]),
            (
                "senior = ['unknown-model']",
                vec!["senior", "unknown-model", "alias", "model ID"],
            ),
            ("junior = ['opus']", vec!["junior", "senior"]),
        ] {
            let result = toml::from_str::<TierConfig>(source);
            assert!(result.is_err(), "must refuse {source}");
            let message = result.unwrap_err().to_string();
            for fragment in expected {
                assert!(
                    message.contains(fragment),
                    "{source}: expected {fragment:?} in {message}"
                );
            }
        }
    }

    #[test]
    fn ordered_tier_lists_replace_only_written_tiers() {
        let parsed = toml::from_str::<TierConfig>(
            "senior = ['astra', 'opus']\nprincipal = []\nrouting = 'off'",
        );
        assert!(parsed.is_ok(), "ordered tier lists must load: {parsed:?}");
        let table = toml::Value::try_from(parsed.unwrap()).unwrap();
        assert_eq!(table["senior"][0].as_str(), Some("astra"));
        assert_eq!(table["senior"][1].as_str(), Some("opus"));
        assert_eq!(table["principal"].as_array().unwrap().len(), 0);
        assert_eq!(table["junior"][0].as_str(), Some("sol"));
        assert_eq!(table["intern"][0].as_str(), Some("luna"));
        assert_eq!(table["routing"].as_str(), Some("off"));
    }

    #[test]
    fn tier_walk_replaces_rows_and_never_descends() {
        let walk = |table: &TierConfig, tier, family: &str| {
            table.walk(
                tier,
                &TierPreference {
                    model: None,
                    family: Some(family.to_owned()),
                },
                |_| true,
                |_, _| None::<()>,
            )
        };
        let table: TierConfig =
            toml::from_str("senior = ['claude-future']\nprincipal = []").unwrap();
        let preferred = walk(&table, ModelTier::Senior, "claude").unwrap();
        assert_eq!(
            (&*preferred.kind, &*preferred.model),
            ("claude", "claude-future")
        );
        assert!(preferred.used_tier.is_none());
        let fallback = walk(&table, ModelTier::Senior, "codex").unwrap();
        assert_eq!(fallback.kind, "claude");
        assert!(fallback.used_tier.is_none());
        assert_eq!(
            walk(&table, ModelTier::Junior, "codex").unwrap().model,
            "sol"
        );
        assert!(walk(&table, ModelTier::Principal, "claude").is_err());
        let table: TierConfig =
            toml::from_str("intern = []\njunior = []\nsenior = []\nprincipal = ['astra']").unwrap();
        let up = walk(&table, ModelTier::Intern, "claude").unwrap();
        assert_eq!((up.kind.as_str(), up.model.as_str()), ("codex", "astra"));
        assert!(up.used_tier.is_some());
        let empty: TierConfig =
            toml::from_str("intern = []\njunior = []\nsenior = []\nprincipal = []").unwrap();
        assert!(walk(&empty, ModelTier::Intern, "codex").is_err());
    }
}
