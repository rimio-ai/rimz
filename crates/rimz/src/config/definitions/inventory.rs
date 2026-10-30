//! Installed pack inventory and exclusion selection over the merged definition and skill chains.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::agents::skills::{SkillChain, SkillErr};
use crate::packs::{
    self, ExclusionTarget, Exclusions, ItemFate, ItemKey, ItemType, PackErr, PackItem, PackName,
    Problem,
};

use super::{CommandsConfig, SkillCheck};

pub struct PackInventory {
    pub packs: Vec<PackView>,
    pub problems: Vec<Problem>,
    pub warnings: Vec<Problem>,
}

#[derive(serde::Serialize)]
pub struct PackView {
    pub name: PackName,
    pub description: Option<String>,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub linked: bool,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    pub pinned: bool,
    pub commit: Option<String>,
    pub entry: String,
    pub excluded: Exclusions,
    pub counts: BTreeMap<ItemType, usize>,
    pub problems: Vec<Problem>,
    pub items: Vec<PackItem>,
    pub unshipped_exclusions: Vec<ItemKey>,
}

#[derive(Debug, thiserror::Error)]
pub enum ItemMatchErr {
    #[error(
        "pack {pack} has no matching item {name:?}{}",
        kind.map_or(String::new(), |kind| format!(" of type {}", kind.as_str()))
    )]
    NoMatch {
        pack: PackName,
        name: String,
        kind: Option<ItemType>,
    },
    #[error("{pack}:{name} matches several types")]
    Ambiguous {
        pack: PackName,
        name: String,
        kinds: BTreeSet<ItemType>,
    },
}

impl PackInventory {
    pub fn load(
        home: &Path,
        check: SkillCheck<'_>,
        commands: &CommandsConfig,
        config_path: &Path,
        tiers: &crate::config::tiers::TierConfig,
        native_auto_compact: Option<u64>,
    ) -> Self {
        let resolved = packs::resolve(home);
        let loaded = super::load_resolved(
            home,
            check,
            commands,
            config_path,
            tiers,
            native_auto_compact,
            &resolved,
        );
        Self::from_loaded(home, &resolved, loaded)
    }

    pub(super) fn from_loaded(
        home: &Path,
        resolved: &packs::ResolvedPacks,
        loaded: super::LoadedDefinitions,
    ) -> Self {
        let chain = SkillChain::from_resolved(home, resolved);
        let mut problems = resolved.problems.clone();
        let mut pack_problems = resolved.pack_problems.clone();
        for (name, errors) in loaded.pack_problems {
            pack_problems.entry(name).or_default().extend(errors);
        }
        let mut items = BTreeMap::<PackName, Vec<PackItem>>::new();
        for item in loaded.pack_items {
            items.entry(item.pack.clone()).or_default().push(item);
        }
        match chain.inventory(|_| true) {
            Ok(inventory) => {
                for skill in inventory.pack_skills {
                    items.entry(skill.pack.clone()).or_default().push(skill);
                }
            }
            Err(error) => {
                let problem = Problem {
                    path: match &error {
                        SkillErr::Io { path, .. } => path.clone(),
                        _ => crate::disk::paths::packs_dir(home),
                    },
                    message: error.to_string(),
                };
                problems.push(problem);
            }
        }
        let mut packs = Vec::new();
        for source in &resolved.record.sources {
            for installed in &source.packs {
                let resolved_pack = resolved
                    .packs
                    .iter()
                    .find(|pack| pack.name == installed.name);
                let mut items = items.remove(&installed.name).unwrap_or_default();
                items.sort_by(|a, b| (a.kind, &a.name, &a.path).cmp(&(b.kind, &b.name, &b.path)));
                let counts = [
                    ItemType::Agents,
                    ItemType::Subagents,
                    ItemType::Teams,
                    ItemType::Traits,
                    ItemType::Skills,
                ]
                .into_iter()
                .map(|kind| {
                    (
                        kind,
                        items
                            .iter()
                            .filter(|item| item.kind == kind && item.fate == ItemFate::Loads)
                            .count(),
                    )
                })
                .collect();
                let mut pack_problems = pack_problems.remove(&installed.name).unwrap_or_default();
                let conflicts: BTreeSet<_> = items
                    .iter()
                    .filter_map(|item| match &item.fate {
                        ItemFate::Conflict { message, .. } => Some(message.as_str()),
                        _ => None,
                    })
                    .collect();
                for item in &items {
                    if item.kind == ItemType::Skills && matches!(item.fate, ItemFate::Failed { .. })
                    {
                        continue;
                    }
                    let Some(message) = item.fate.problem() else {
                        continue;
                    };
                    if matches!(item.fate, ItemFate::Failed { .. }) && conflicts.contains(message) {
                        continue;
                    }
                    if matches!(item.fate, ItemFate::Conflict { .. })
                        && pack_problems
                            .iter()
                            .any(|problem| problem.message == message)
                    {
                        continue;
                    }
                    let problem = Problem {
                        path: if item.kind == ItemType::Skills {
                            item.path.join("SKILL.md")
                        } else {
                            item.path.clone()
                        },
                        message: message.to_owned(),
                    };
                    if !pack_problems.contains(&problem) {
                        pack_problems.push(problem);
                    }
                }
                for problem in &pack_problems {
                    let reported = problems.iter().any(|seen| {
                        seen == problem
                            || (seen.message == problem.message
                                && conflicts.contains(problem.message.as_str()))
                    });
                    if !reported {
                        problems.push(problem.clone());
                    }
                }
                let unshipped_exclusions = installed
                    .excluded
                    .items
                    .iter()
                    .filter(|key| {
                        !items
                            .iter()
                            .any(|item| item.kind == key.kind && item.name == key.name)
                    })
                    .cloned()
                    .collect();
                packs.push(PackView {
                    name: installed.name.clone(),
                    description: resolved_pack.map(|pack| pack.description.clone()),
                    source: source.source.clone(),
                    url: (!source.linked)
                        .then(|| source.url.clone().unwrap_or_else(|| source.source.clone())),
                    linked: source.linked,
                    reference: source.reference.clone(),
                    pinned: source.pinned,
                    commit: source.commit.clone(),
                    entry: source.entry.clone(),
                    excluded: installed.excluded.clone(),
                    counts,
                    problems: pack_problems,
                    items,
                    unshipped_exclusions,
                });
            }
        }
        Self {
            packs,
            problems,
            warnings: resolved.warnings.clone(),
        }
    }

    pub fn pack(&self, name: &PackName) -> packs::Result<&PackView> {
        self.packs
            .iter()
            .find(|pack| pack.name == *name)
            .ok_or_else(|| PackErr::UnknownPack {
                name: name.clone(),
                installed: self
                    .packs
                    .iter()
                    .map(|pack| pack.name.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            })
    }

    pub fn conflicts(&self) -> BTreeSet<String> {
        self.packs
            .iter()
            .flat_map(|pack| &pack.items)
            .filter_map(|item| match &item.fate {
                ItemFate::Conflict { message, .. } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }
}

impl PackView {
    pub fn item(&self, name: &str, kind: Option<ItemType>) -> Result<&PackItem, ItemMatchErr> {
        // match_item returns only an item target, including when a single type was requested.
        let ExclusionTarget::Item(key) = self.match_item(name, kind, false)? else {
            unreachable!("match_item always resolves an item, never a type")
        };
        // match_item counted this shipped item while resolving its type.
        Ok(self
            .items
            .iter()
            .find(|item| item.name == key.name && item.kind == key.kind)
            .expect("matching shipped item"))
    }

    pub fn match_item(
        &self,
        name: &str,
        kind: Option<ItemType>,
        include: bool,
    ) -> Result<ExclusionTarget, ItemMatchErr> {
        let mut kinds: BTreeSet<_> = self
            .items
            .iter()
            .filter(|item| item.name == name)
            .map(|item| item.kind)
            .collect();
        if include {
            kinds.extend(
                self.excluded
                    .items
                    .iter()
                    .filter(|item| item.name == name)
                    .map(|item| item.kind),
            );
        }
        kinds.retain(|candidate| kind.is_none_or(|kind| kind == *candidate));
        match kinds.len() {
            0 => Err(ItemMatchErr::NoMatch {
                pack: self.name.clone(),
                name: name.to_owned(),
                kind,
            }),
            1 => Ok(ExclusionTarget::Item(ItemKey {
                // The one-element set was counted above.
                kind: *kinds.first().expect("one matching type"),
                name: name.to_owned(),
            })),
            _ => Err(ItemMatchErr::Ambiguous {
                pack: self.name.clone(),
                name: name.to_owned(),
                kinds,
            }),
        }
    }
}
