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
    pub fn from_model(model: &str) -> Option<Self> {
        match model {
            "intern" => Some(Self::Intern),
            "junior" => Some(Self::Junior),
            "senior" => Some(Self::Senior),
            "principal" => Some(Self::Principal),
            _ => None,
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

pub(super) fn is_relative_effort(effort: &str) -> bool {
    effort
        .strip_prefix(['+', '-'])
        .is_some_and(|steps| !steps.is_empty() && steps.bytes().all(|byte| byte.is_ascii_digit()))
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TierProvenance {
    pub tier: ModelTier,
    pub preferred_family: String,
    /// Another family or a higher tier supplied the cell.
    pub fell_back: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedTier {
    pub kind: String,
    pub model: String,
    pub effort: String,
    pub provenance: TierProvenance,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct TierConfig(BTreeMap<ModelTier, BTreeMap<Family, TierCell>>);

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
enum Family {
    Claude,
    Codex,
}

impl Family {
    fn kind(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    fn other(self) -> Self {
        match self {
            Self::Claude => Self::Codex,
            Self::Codex => Self::Claude,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TierCell {
    model: String,
    effort: Option<String>,
}

impl TierCell {
    fn effort(&self, family: Family) -> &str {
        self.effort.as_deref().unwrap_or_else(|| {
            // Both admitted families declare a default effort.
            agents::definition_defaults(family.kind(), Some(&self.model))
                .effort
                .expect("tier family has a default effort")
        })
    }
}

impl Default for TierConfig {
    fn default() -> Self {
        let mut table = Self(BTreeMap::new());
        for (tier, family, model, effort) in [
            (ModelTier::Intern, Family::Claude, "haiku", "xhigh"),
            (ModelTier::Intern, Family::Codex, "luna", "xhigh"),
            (ModelTier::Junior, Family::Claude, "sonnet", "xhigh"),
            (ModelTier::Junior, Family::Codex, "sol", "xhigh"),
            (ModelTier::Senior, Family::Claude, "opus", "xhigh"),
            (ModelTier::Senior, Family::Codex, "astra", "xhigh"),
            (ModelTier::Principal, Family::Claude, "fable", "high"),
        ] {
            table.0.entry(tier).or_default().insert(
                family,
                TierCell {
                    model: model.to_owned(),
                    effort: Some(effort.to_owned()),
                },
            );
        }
        table
    }
}

impl<'de> Deserialize<'de> for TierConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let rows = BTreeMap::<ModelTier, BTreeMap<Family, TierCell>>::deserialize(deserializer)?;
        for (tier, cells) in &rows {
            for (family, cell) in cells {
                let kind = family.kind();
                if cell.model.trim().is_empty()
                    || ModelTier::from_model(&cell.model).is_some()
                    || agents::definition_model_kind(&cell.model)
                        .is_some_and(|implied| implied != kind)
                {
                    return Err(serde::de::Error::custom(format!(
                        "tiers.{tier}.{kind}.model = {:?}; name a {kind} model alias or model ID",
                        cell.model
                    )));
                }
                if !agents::effort_ladder(kind).contains(&cell.effort(*family)) {
                    return Err(serde::de::Error::custom(format!(
                        "tiers.{tier}.{kind}.effort = {:?}; use one of {}",
                        cell.effort(*family),
                        agents::effort_ladder(kind).join(", ")
                    )));
                }
            }
        }
        let mut table = Self::default();
        table.0.extend(rows);
        Ok(table)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct TierError(String);

impl TierConfig {
    /// The tiers `family` has a model for, highest first, as written in config.
    pub fn family_tiers(&self, family: &str) -> Vec<(ModelTier, &str)> {
        self.0
            .iter()
            .rev()
            .filter_map(|(tier, row)| {
                row.iter()
                    .find(|(cell_family, _)| cell_family.kind() == family)
                    .map(|(_, cell)| (*tier, cell.model.as_str()))
            })
            .collect()
    }

    pub fn resolve(
        &self,
        tier: ModelTier,
        preferred_family: &str,
        effort: Option<&str>,
    ) -> Result<ResolvedTier, TierError> {
        let preferred = match preferred_family {
            "claude" => Family::Claude,
            "codex" => Family::Codex,
            _ => {
                return Err(TierError(format!(
                    "model tier {tier} cannot run on {preferred_family}; tiers resolve on claude or codex, so set `agent:` to one of those families"
                )));
            }
        };
        for (selected_tier, row) in self.0.range(tier..) {
            for family in [preferred, preferred.other()] {
                let Some(cell) = row.get(&family) else {
                    continue;
                };
                let kind = family.kind();
                let default = cell.effort(family);
                let effort = match effort {
                    Some(value) if is_relative_effort(value) => {
                        let shift: i64 = value.parse().map_err(|_| {
                            TierError(format!(
                                "invalid effort shift {value:?}; use +N or -N whole steps"
                            ))
                        })?;
                        let ladder = agents::effort_ladder(kind);
                        // Deserialization checks every cell against its family's ladder.
                        let index = ladder
                            .iter()
                            .position(|level| *level == default)
                            .expect("validated tier effort");
                        ladder[(index as i64)
                            .saturating_add(shift)
                            .clamp(0, ladder.len() as i64 - 1)
                            as usize]
                    }
                    Some(value) => value,
                    None => default,
                };
                return Ok(ResolvedTier {
                    kind: kind.to_owned(),
                    model: agents::expand_model_alias(kind, &cell.model),
                    effort: effort.to_owned(),
                    provenance: TierProvenance {
                        tier,
                        preferred_family: preferred_family.to_owned(),
                        fell_back: *selected_tier != tier || family != preferred,
                    },
                });
            }
        }
        Err(TierError(format!(
            "no model at tier {tier} or above for either family; set a cell under [tiers.{tier}] in config.toml"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_walk_replaces_rows_and_never_descends() {
        let table: TierConfig = toml::from_str(
            "[senior]\nclaude = { model = 'future', effort = 'medium' }\n[principal]",
        )
        .unwrap();
        let preferred = table.resolve(ModelTier::Senior, "claude", None).unwrap();
        assert_eq!(
            (&*preferred.kind, &*preferred.model, &*preferred.effort),
            ("claude", "future", "medium")
        );
        assert!(!preferred.provenance.fell_back);
        let fallback = table.resolve(ModelTier::Senior, "codex", None).unwrap();
        assert_eq!(fallback.kind, "claude");
        assert!(fallback.provenance.fell_back);
        assert_eq!(
            table
                .resolve(ModelTier::Junior, "codex", None)
                .unwrap()
                .model,
            "gpt-6-sol"
        );
        assert!(table.resolve(ModelTier::Principal, "claude", None).is_err());
        let table: TierConfig =
            toml::from_str("[intern]\n[junior]\n[senior]\n[principal]\ncodex = {model = 'astra'}")
                .unwrap();
        let up = table.resolve(ModelTier::Intern, "claude", None).unwrap();
        assert_eq!(
            (up.kind.as_str(), up.model.as_str()),
            ("codex", "gpt-6-astra")
        );
        assert!(up.provenance.fell_back);
        let empty: TierConfig =
            toml::from_str("[intern]\n[junior]\n[senior]\n[principal]").unwrap();
        assert!(empty.resolve(ModelTier::Intern, "codex", None).is_err());
    }

    #[test]
    fn tier_effort_shifts_clamp_on_each_family_ladder() {
        let table: TierConfig = toml::from_str("[senior]\nclaude = {model = 'opus', effort = 'high'}\ncodex = {model = 'astra', effort = 'high'}").unwrap();
        for family in ["claude", "codex"] {
            for (shift, expected) in [("+1", "xhigh"), ("-1", "medium"), ("absolute", "absolute")] {
                assert_eq!(
                    table
                        .resolve(ModelTier::Senior, family, Some(shift))
                        .unwrap()
                        .effort,
                    expected
                );
            }
            assert_eq!(
                table
                    .resolve(ModelTier::Senior, family, Some("-999"))
                    .unwrap()
                    .effort,
                "low"
            );
            assert_eq!(
                table
                    .resolve(ModelTier::Senior, family, Some("+999"))
                    .unwrap()
                    .effort,
                "max"
            );
        }
        assert_eq!(
            TierConfig::default()
                .resolve(ModelTier::Principal, "codex", None)
                .unwrap()
                .effort,
            "high"
        );
    }
}
