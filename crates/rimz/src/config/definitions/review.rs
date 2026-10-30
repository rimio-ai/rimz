//! Effective launch changes between committed and candidate definition loads.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::agents::PermissionMode;
use crate::config::{Isolation, MachineConfig, PromptSource};
use crate::harness::spec::ResolvedProfile;
use crate::ids::AgentKind;
use crate::packs::ResolvedPacks;

use super::SkillCheck;
use super::inventory::PackInventory;

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveGrants {
    pub kind: AgentKind,
    pub mode: PermissionMode,
    pub isolation: Isolation,
    pub tools: Option<BTreeMap<String, BTreeSet<String>>>,
    pub allowed_tools: BTreeSet<String>,
}

#[derive(Debug)]
pub struct ProfileChange {
    pub name: String,
    pub namespace: &'static str,
    pub grants: EffectiveGrants,
    pub widened: Vec<&'static str>,
}

pub struct PackPreview {
    pub inventory: PackInventory,
    pub profiles: Vec<ProfileChange>,
}

impl EffectiveGrants {
    fn unset(machine: Isolation, kind: &AgentKind) -> Self {
        Self {
            kind: kind.clone(),
            mode: PermissionMode::Ask,
            isolation: machine,
            tools: None,
            allowed_tools: BTreeSet::new(),
        }
    }

    fn of(profile: &ResolvedProfile, machine: Isolation) -> Self {
        Self {
            kind: profile.kind.clone(),
            mode: profile.launch.mode.unwrap_or(PermissionMode::Ask),
            isolation: profile.isolation_default.unwrap_or(machine),
            tools: profile.tools.as_ref().map(|tools| {
                tools
                    .bases()
                    .iter()
                    .map(|base| {
                        let types = if base == "Agent" {
                            tools.agent_types().iter().cloned().collect()
                        } else {
                            BTreeSet::new()
                        };
                        (base.clone(), types)
                    })
                    .collect()
            }),
            allowed_tools: profile
                .allowed_tools
                .iter()
                .flatten()
                .map(ToString::to_string)
                .collect(),
        }
    }

    fn widened_from(&self, previous: &Self) -> Vec<&'static str> {
        let tools = match (&self.tools, &previous.tools) {
            (None, Some(_)) => true,
            (Some(new), Some(old)) => new.iter().any(|(base, types)| match old.get(base) {
                None => true,
                Some(old) if base == "Agent" && !old.is_empty() => {
                    types.is_empty() || !types.is_subset(old)
                }
                _ => false,
            }),
            _ => false,
        };
        [
            ("mode", self.mode.rank() > previous.mode.rank()),
            ("isolation", self.isolation < previous.isolation),
            ("tools", tools),
            (
                "allowed-tools",
                !self.allowed_tools.is_subset(&previous.allowed_tools),
            ),
        ]
        .into_iter()
        .filter_map(|(field, widened)| widened.then_some(field))
        .collect()
    }
}

pub fn compare(
    home: &Path,
    check: SkillCheck<'_>,
    machine: &MachineConfig,
    before: &ResolvedPacks,
    after: &ResolvedPacks,
) -> PackPreview {
    let load = |packs| {
        super::load_resolved(
            home,
            check,
            &machine.agents.commands,
            &MachineConfig::config_path(),
            &machine.tiers,
            machine.harness.auto_compact,
            packs,
        )
    };
    let old = profiles(&load(before), before);
    let loaded = load(after);
    let new = profiles(&loaded, after);
    let profiles = new
        .into_iter()
        .filter_map(|(name, (namespace, resolved))| {
            let previous = old.get(&name);
            if previous.is_some_and(|(scope, profile)| *scope == namespace && *profile == resolved)
            {
                return None;
            }
            let grants = EffectiveGrants::of(&resolved, machine.agents.isolation);
            let previous = previous.map_or_else(
                || EffectiveGrants::unset(machine.agents.isolation, &grants.kind),
                |(_, profile)| EffectiveGrants::of(profile, machine.agents.isolation),
            );
            let widened = grants.widened_from(&previous);
            Some(ProfileChange {
                name,
                namespace,
                grants,
                widened,
            })
        })
        .collect();
    PackPreview {
        inventory: PackInventory::from_loaded(home, after, loaded),
        profiles,
    }
}

fn profiles(
    loaded: &super::LoadedDefinitions,
    packs: &ResolvedPacks,
) -> BTreeMap<String, (&'static str, ResolvedProfile)> {
    let mut result = BTreeMap::<String, (&'static str, ResolvedProfile)>::new();
    for (namespace, profiles) in [
        ("agents", &loaded.agent_profiles),
        ("subagents", &loaded.subagent_profiles),
    ] {
        for name in profiles.0.keys() {
            let Ok(mut resolved) = crate::harness::spec::resolve_profile(name, profiles) else {
                continue;
            };
            for prompt in resolved
                .system_prompt_file
                .iter_mut()
                .chain(&mut resolved.append_system_prompt_files)
            {
                let path = match prompt {
                    PromptSource::File(path) | PromptSource::Text { origin: path, .. } => path,
                };
                if let Some(normalized) = packs.packs.iter().find_map(|pack| {
                    path.strip_prefix(&pack.root)
                        .ok()
                        .map(|relative| PathBuf::from(format!("pack:{}", pack.name)).join(relative))
                }) {
                    *path = normalized;
                }
            }
            // Kind bases are the only names shared by the materialized launch namespaces.
            result
                .entry(name.clone())
                .and_modify(|(scope, _)| *scope = "agents/subagents")
                .or_insert((namespace, resolved));
        }
    }
    result
}
