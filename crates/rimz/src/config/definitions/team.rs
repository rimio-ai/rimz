//! Team rosters, stage matrices, and fully materialized seat profiles.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::agents;
use crate::config::engine;
use crate::config::{FlipCompact, Profile, PromptSource, RoleBinding, Team, TeamSignalBinding};
use crate::harness::team_prompt::BUILT_IN_CONSENSUS;
use crate::store::message::AutoCompact;

use super::frontmatter::{RoleFrontmatter, SignalFrontmatter, TeamFrontmatter};
use super::{
    Definition, DefinitionCause, DefinitionErr, LoadScope, LoadedDefinitions, Namespace, agent,
    catalog, frontmatter, traits,
};

struct SeatLoader<'a> {
    scope: LoadScope<'a>,
    agents: &'a Namespace,
    subagents: &'a Namespace,
}

pub(super) fn load(
    scope: LoadScope<'_>,
    [agents, subagents]: [&Namespace; 2],
    sources: &catalog::Catalog,
    commands: &super::CommandsConfig,
    loaded: &mut LoadedDefinitions,
) {
    let seats = SeatLoader {
        scope,
        agents,
        subagents,
    };
    let mut names = BTreeMap::new();
    for item in sources
        .items
        .iter()
        .filter(|item| item.kind == crate::packs::ItemType::Teams && item.selected())
    {
        let path = item.path.clone();
        let mut name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let mut role_names = Vec::new();
        let result = (|| {
            let text = scope
                .context
                .shared
                .skills
                .read_text(&path)
                .map_err(|error| DefinitionErr::new(&path, error.to_string()))?;
            let (yaml, body) = frontmatter::split(&path, &text)?;
            let fm: TeamFrontmatter = frontmatter::parse(&path, yaml)?;
            name = fm.name.clone().unwrap_or_else(|| name.clone());
            if !super::is_safe_name(&name) {
                return Err(DefinitionErr::new(
                    &path,
                    format!("unsafe team name {name:?}"),
                ));
            }
            let roster = roster(scope, &path, &name, &fm, body);
            if let Ok((team, _)) = &roster {
                role_names.extend(team.roles.iter().map(|binding| binding.profile.clone()));
            }
            if let Some(previous) = names.insert(name.clone(), path.clone()) {
                loaded
                    .failed
                    .entry(name.clone())
                    .or_default()
                    .insert(previous);
                loaded.teams.0.remove(&name);
                loaded.sources.teams.remove(&name);
                loaded.rows.retain(|row| {
                    if row.team.as_deref() != Some(&name) {
                        return true;
                    }
                    loaded.agent_profiles.0.remove(&row.name);
                    loaded.sources.agent_profiles.remove(&row.name);
                    loaded
                        .failed
                        .entry(row.name.clone())
                        .or_default()
                        .extend([row.source.clone(), path.clone()]);
                    false
                });
                return Err(DefinitionErr::new(
                    &path,
                    format!("team '{name}' is declared twice"),
                ));
            }
            let (team, reflects) = roster?;
            let mut profiles = Vec::new();
            let mut errors = Vec::new();
            for (role, binding) in fm.roles.iter().zip(&team.roles) {
                match seats.load(&path, &name, role, &fm, reflects, loaded) {
                    Ok(profile) => profiles.push((binding, profile)),
                    Err(error) => errors.push(error),
                }
            }
            if !errors.is_empty() {
                loaded.errors.extend(errors);
                return Ok(());
            }
            if item.pack.is_some() {
                check_team_name(&name, loaded, commands).map_err(|error| item.error(error))?;
            }
            let origin = super::DefinitionOrigin::new(item.pack.as_ref(), None);
            for (binding, profile) in profiles {
                loaded.insert("agents", &binding.profile, &path, &origin, profile);
                if let Some(row) = loaded.rows.last_mut() {
                    row.keep_warm = team.keep_warm(Some(&binding.role));
                    row.namespace = "team".to_owned();
                    row.team = Some(name.clone());
                    row.role = Some(binding.role.clone());
                    row.owns = binding.owns.clone();
                    row.signals = binding.signals.clone();
                }
            }
            loaded.sources.teams.insert(name.clone(), path.clone());
            loaded.teams.0.insert(name.clone(), team);
            Ok(())
        })();
        if let Err(error) = result {
            loaded.errors.push(
                if item.pack.is_some() && error.cause != DefinitionCause::PackProblem {
                    item.error(error.message)
                } else {
                    error
                },
            );
        }
        if !loaded.teams.0.contains_key(&name) {
            // A name either table reserves stays its cell's, so the failed team never owns it.
            if check_team_name(&name, loaded, commands).is_ok() {
                loaded.failed_teams.insert(name.clone());
            }
            for failed in std::iter::once(&name).chain(&role_names) {
                loaded
                    .failed
                    .entry(failed.clone())
                    .or_default()
                    .insert(path.clone());
            }
        }
    }
    check_teams(sources, agents, commands, loaded);
}

/// The name checks the set-wide namespace checks make of a team, against the agents and the
/// subagents table. A seat's `<team>.<role>` key never matches a team name, which holds no `.`.
fn check_team_name(
    name: &str,
    loaded: &LoadedDefinitions,
    commands: &super::CommandsConfig,
) -> Result<(), crate::harness::spec::LayoutErr> {
    for profiles in [&loaded.agent_profiles, &loaded.subagent_profiles] {
        crate::harness::spec::validate_team_name(name, profiles, commands)?;
    }
    Ok(())
}

/// Check each loaded team past its name against the final agents table, which holds every
/// team's seats, so a layout cell may name any team's seat. A team that fails fails on its own
/// name and seats at its file, as one that does not load, and the check repeats until a pass
/// fails no team, so a cell on a seat of a team that failed fails its team too. A team whose
/// name either table reserves ([`check_team_name`]) is left to the set-wide namespace checks.
fn check_teams(
    sources: &catalog::Catalog,
    agents: &Namespace,
    commands: &super::CommandsConfig,
    loaded: &mut LoadedDefinitions,
) {
    loop {
        let mut table = loaded.agent_profiles.clone();
        // Each failed profile name, an `agents/` definition or a `<team>.<role>` seat, stands in
        // as its own detail, so the error a cell on it raises names it. A kind's word stays the
        // kind, and a failed team's name is no profile, so a cell reads the same beside it.
        table.set_failed_rows(
            loaded
                .failed
                .keys()
                .filter(|name| {
                    !loaded.agent_profiles.0.contains_key(*name)
                        && crate::agents::find_runtime(name).is_none()
                        && (agents.definitions.contains_key(*name)
                            || agents.failed.contains(*name)
                            || name.contains('.'))
                })
                .map(|name| (name.clone(), name.clone()))
                .collect(),
        );
        let failures: Vec<_> = loaded
            .teams
            .0
            .iter()
            .filter(|(name, _)| check_team_name(name, loaded, commands).is_ok())
            .filter_map(|(name, team)| {
                let error =
                    crate::harness::spec::validate_team_roster(name, team, &table, commands)
                        .err()?;
                Some((name.clone(), error))
            })
            .collect();
        if failures.is_empty() {
            return;
        }
        for (name, error) in failures {
            let (Some(team), Some(path)) = (
                loaded.teams.0.remove(&name),
                loaded.sources.teams.remove(&name),
            ) else {
                continue;
            };
            let error = match error {
                crate::harness::spec::LayoutErr::FailedProfile(failed) => DefinitionErr::new(
                    &path,
                    format!("team '{name}' layout names failed definition '{failed}'"),
                )
                .with_cause(DefinitionCause::DependsOnFailed {
                    name: failed,
                    subagent: false,
                }),
                error => DefinitionErr::new(&path, error.to_string()),
            };
            loaded.errors.push(
                match sources
                    .items
                    .iter()
                    .find(|item| item.pack.is_some() && item.path == path)
                {
                    Some(item) => item.error(error.message),
                    None => error,
                },
            );
            loaded.rows.retain(|row| {
                if row.team.as_deref() != Some(&name) {
                    return true;
                }
                loaded.agent_profiles.0.remove(&row.name);
                loaded.sources.agent_profiles.remove(&row.name);
                false
            });
            loaded.failed_teams.insert(name.clone());
            let seats = team.roles.iter().map(|binding| &binding.profile);
            for failed in std::iter::once(&name).chain(seats) {
                loaded
                    .failed
                    .entry(failed.clone())
                    .or_default()
                    .insert(path.clone());
            }
        }
    }
}

/// The team, and whether it lists the shipped `reflect` piece.
fn roster(
    scope: LoadScope<'_>,
    path: &Path,
    name: &str,
    fm: &TeamFrontmatter,
    body: &str,
) -> Result<(Team, bool), DefinitionErr> {
    let fail = |message: String| DefinitionErr::new(path, format!("team '{name}' {message}"));
    if body.trim().is_empty() {
        return Err(fail("has no body; its pipeline is the body".to_owned()));
    }
    if fm.roles.is_empty() {
        return Err(fail("lists no roles".to_owned()));
    }
    let stages = fm.stages.clone().unwrap_or_default();
    if stages.is_empty() {
        return Err(fail("declares no `stages:`; rimz gives an unstaged team neither the consensus nor its pipeline".to_owned()));
    }
    let leader = fm
        .leader
        .as_deref()
        .filter(|leader| !leader.is_empty())
        .ok_or_else(|| fail("sets `stages:` but no `leader:`".to_owned()))?;
    let mut handles = BTreeSet::new();
    let mut owners = BTreeMap::new();
    let mut roles = Vec::new();
    for role in &fm.roles {
        let handle = role.role.as_deref().unwrap_or(&role.agent);
        if !handles.insert(handle) {
            return Err(fail(format!("declares role handle '{handle}' twice")));
        }
        let owns = role.owns.clone().unwrap_or_default();
        for stage in &owns {
            if !stages.contains(stage) {
                return Err(fail(format!(
                    "role '{handle}' owns unknown stage '{stage}'"
                )));
            }
            if let Some(previous) = owners.insert(stage.clone(), handle) {
                return Err(fail(format!(
                    "stage '{stage}' owned by both '{previous}' and '{handle}'"
                )));
            }
        }
        roles.push(RoleBinding {
            role: handle.to_owned(),
            profile: format!("{name}.{handle}"),
            flip_compact: role
                .flip_compact
                .as_deref()
                .map(|value| flip_compact(path, value))
                .transpose()?,
            // A Markdown role's value rides its overlay into the seat's
            // `<team>.<role>` profile, inheriting like `auto-compact`.
            idle_compact: None,
            smart_compact: None,
            keep_warm: role.keep_warm.clone(),
            signals: signals(path, role.signals.as_deref())?,
            owns,
            mode: None,
            model: None,
            effort: None,
            // The overlay carries it too; the binding keeps the role's own value, so it
            // stays above an `--agent` override as a TOML role's does.
            fast: role.fast,
            budget: None,
            auto_compact: None,
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            args: None,
        });
    }
    if !handles.contains(leader) {
        return Err(fail(format!(
            "names leader '{leader}', not a declared role"
        )));
    }
    for stage in &stages {
        if !owners.contains_key(stage) {
            return Err(fail(format!("stage '{stage}' has no owner")));
        }
    }
    if let Some(owner) = owners.get("Implement")
        && owners.get("Review") == Some(owner)
    {
        return Err(fail(format!(
            "seats Implement and Review on '{owner}'; the producer of a stage never referees it"
        )));
    }
    let consensus = fm.consensus.clone().unwrap_or_default();
    let pieces = traits::consensus(
        &scope.context.shared.home,
        scope.traits,
        name,
        &consensus,
        &stages,
        |path| {
            scope
                .context
                .shared
                .skills
                .read_text(path)
                .map(|text| text.to_string())
                .map_err(|error| error.to_string())
        },
    )
    .map_err(|message| DefinitionErr::new(path, message))?;
    let texts = pieces.sources.iter().filter_map(|source| match source {
        PromptSource::Text { text, .. } => Some(text.as_str()),
        PromptSource::File(_) => None,
    });
    for text in std::iter::once(BUILT_IN_CONSENSUS)
        .chain(texts)
        .chain([body])
    {
        for tail in text.split('@').skip(1) {
            if !tail.starts_with(|c: char| c.is_ascii_alphabetic()) {
                continue;
            }
            let end = tail
                .find(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
                .unwrap_or(tail.len());
            let handle = &tail[..end];
            if !handles.contains(handle)
                && !matches!(handle, "all" | "rimz")
                && agents::find_definition(handle).is_none()
            {
                return Err(fail(format!("references undeclared handle '@{handle}'")));
            }
        }
    }
    let mut files = pieces.sources;
    files.push(PromptSource::Text {
        origin: path.to_owned(),
        text: body.trim().to_owned(),
    });
    let team = Team {
        keep_warm: fm.keep_warm.clone(),
        roles,
        leader: Some(leader.to_owned()),
        layout: fm.layout.clone(),
        stages,
        scratch_files: None,
        consensus_file: None,
        consensus,
        append_system_prompt_files: files,
    };
    Ok((team, pieces.shipped_reflect))
}

impl SeatLoader<'_> {
    /// `definition`'s frontmatter with its `agent:` chain inherited as the definition resolves
    /// alone: each layer inherits its parent's already-inherited frontmatter, so a layer that
    /// writes one of `tools`/`capabilities` hides the other key above it from every `+` list.
    fn chain_frontmatter(
        &self,
        definition: &Definition,
        path: &Path,
    ) -> Result<frontmatter::AgentFrontmatter, DefinitionErr> {
        let mut fm = definition.frontmatter.clone();
        if let Some(parent) = definition
            .frontmatter
            .agent
            .as_ref()
            .map(|name| self.agents.lookup(name, path))
            .transpose()?
            .flatten()
        {
            fm.inherit(&self.chain_frontmatter(parent, path)?);
        }
        Ok(fm)
    }

    fn load(
        &self,
        path: &Path,
        name: &str,
        role: &RoleFrontmatter,
        team: &TeamFrontmatter,
        reflects: bool,
        loaded: &LoadedDefinitions,
    ) -> Result<Profile, DefinitionErr> {
        let definition = self.agents.lookup(&role.agent, path)?
            .filter(|_| loaded.agent_profiles.0.contains_key(&role.agent))
            .ok_or_else(|| {
                let handle = role.role.as_deref().unwrap_or(&role.agent);
                if self.agents.definitions.contains_key(&role.agent) || self.agents.failed.contains(&role.agent) {
                    return DefinitionErr::new(path, format!("team '{name}' role '{handle}' selects failed agent '{}'", role.agent))
                        .with_cause(DefinitionCause::DependsOnFailed {
                            name: role.agent.clone(),
                            subagent: false,
                        });
                }
                DefinitionErr::new(path, format!("team '{name}' role '{handle}' selects unknown agent '{}'; team roles can select definitions from agents only", role.agent))
            })?;
        let original = &loaded.agent_profiles.0[&role.agent];
        let mut fm = role.overlay();
        let role_overlay = fm.clone();
        fm.check_plus().map_err(|message| {
            let handle = role.role.as_deref().unwrap_or(&role.agent);
            DefinitionErr::new(path, format!("team '{name}' role '{handle}' {message}"))
        })?;
        fm.inherit(&self.chain_frontmatter(definition, path)?);
        // A model the role writes routes by its id; only the chain's own stays.
        let kind = engine::seat_kind(&original.agent, fm.model.as_deref(), role.model.is_some());
        fm.agent = if role.tier.is_some() && !agents::runs_other_providers_models(kind) {
            None
        } else if role.model.is_some() {
            Some(kind.to_owned())
        } else if original.kind_base {
            Some(original.agent.clone())
        } else if let Some(source) = &original.definition {
            source
                .preference()
                .family
                .clone()
                .or_else(|| Some(kind.to_owned()))
        } else {
            Some(kind.to_owned())
        };
        if let Some(variant) = fm
            .agent
            .as_deref()
            .and_then(|agent| engine::variant(agent, &original.agent, original.variant))
        {
            fm.agent = Some(variant.name.to_owned());
        }
        fm.description = definition.frontmatter.description.clone();
        let mut traits = Vec::new();
        for name in definition
            .frontmatter
            .traits
            .iter()
            .flatten()
            .chain(role.traits.iter().flatten())
            .chain(team.traits.iter().flatten())
        {
            if !traits.contains(name) {
                traits.push(name.clone());
            }
        }
        fm.traits = Some(traits);
        let handle = role.role.as_deref().unwrap_or(&role.agent);
        let seat_info = agent::SeatInfo {
            edits: agent::SeatEdits {
                leader: team.leader.as_deref() == Some(handle),
                reflects,
            },
            team: name.to_owned(),
            role: handle.to_owned(),
            agent: role.agent.clone(),
            profile_name: format!("{name}.{handle}"),
        };
        let ancestor_crafts: Vec<_> = original
            .append_system_prompt_files
            .iter()
            .filter(|source| source.origin() != definition.path)
            .cloned()
            .collect();
        let ancestor_chain = original
            .definition
            .as_ref()
            .map(|source| source.chain().to_vec())
            .unwrap_or_else(|| vec![role.agent.clone()]);
        let mut seat_writers = original
            .definition
            .as_ref()
            .map(|source| source.writers().clone())
            .unwrap_or_default();
        seat_writers.overlay_from(&role_overlay, &seat_info.profile_name);
        // Resolve the overlaid leaf through the standalone checks/defaults with its already-rendered
        // ancestor crafts in place, so the load-time seat and on-demand builds share one path.
        let tree = Namespace {
            definitions: BTreeMap::from([(
                role.agent.clone(),
                Definition {
                    path: path.to_owned(),
                    pack: None,
                    body: definition.body.clone(),
                    frontmatter: fm,
                    extras: super::TomlExtras::default(),
                },
            )]),
            failed: BTreeSet::new(),
            ..Namespace::default()
        };
        let mut seat = LoadedDefinitions::default();
        agent::resolve_namespace(
            agent::Resolver::new(self.scope, "agents", [&tree, self.subagents]).with_seat(
                seat_info,
                ancestor_chain,
                ancestor_crafts,
                seat_writers,
            ),
            &mut seat,
        );
        if let Some(error) = seat.errors.into_iter().next() {
            return Err(error);
        }
        seat.agent_profiles.0.remove(&role.agent).ok_or_else(|| {
            DefinitionErr::new(path, format!("seat '{}' failed to resolve", role.agent))
        })
    }
}

fn flip_compact(path: &Path, value: &str) -> Result<FlipCompact, DefinitionErr> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("off") {
        return Ok(FlipCompact::Off);
    }
    let digits = value.trim_end_matches(['k', 'K', 'm', 'M', '%']);
    if digits.is_empty()
        || value.len() - digits.len() > 1
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(DefinitionErr::new(
            path,
            format!(
                "sets `flip-compact: {value}`; rimz takes a token count such as '120k', a percentage such as '70%', or 'off'"
            ),
        ));
    }
    AutoCompact::parse(value)
        .map(FlipCompact::Threshold)
        .map_err(|error| DefinitionErr::new(path, error))
}

fn signals(
    path: &Path,
    entries: Option<&[SignalFrontmatter]>,
) -> Result<Vec<TeamSignalBinding>, DefinitionErr> {
    let Some(entries) = entries else {
        return Ok(Vec::new());
    };
    if entries.is_empty() {
        return Err(DefinitionErr::new(
            path,
            "sets `signals:` to something other than a non-empty list",
        ));
    }
    let mut bindings = Vec::new();
    for entry in entries {
        let (signal, matches, prompt) = match entry {
            SignalFrontmatter::Selector(signal) => (signal, BTreeMap::new(), None),
            SignalFrontmatter::Binding(binding) => (
                &binding.signal,
                binding.matches.clone().unwrap_or_default(),
                binding.prompt.as_deref(),
            ),
        };
        let valid = signal.split_once('.').is_some_and(|(family, event)| {
            let word = |text: &str| {
                !text.is_empty()
                    && text.bytes().all(|byte| {
                        byte.is_ascii_lowercase()
                            || byte.is_ascii_digit()
                            || matches!(byte, b'_' | b'-')
                    })
            };
            family.starts_with(|c: char| c.is_ascii_lowercase())
                && word(family)
                && (event == "*" || word(event))
        });
        if !valid {
            return Err(DefinitionErr::new(
                path,
                format!(
                    "signal '{signal}': rimz reads one event name or one family, spelled 'ci.failed' or 'ci.*'"
                ),
            ));
        }
        if matches.values().any(|value| value.trim().is_empty()) {
            return Err(DefinitionErr::new(
                path,
                "signal `match:` must be a map of payload fields to non-empty string values",
            ));
        }
        if signal.starts_with("agent.")
            && !["handle", "session"].iter().any(|key| {
                matches
                    .get(*key)
                    .is_some_and(|value| !crate::harness::signal::is_match_wildcard(value))
            })
        {
            return Err(DefinitionErr::new(
                path,
                format!("binds '{signal}' with no handle or session match"),
            ));
        }
        if prompt.is_some_and(|prompt| prompt.trim().is_empty()) {
            return Err(DefinitionErr::new(
                path,
                format!("sets an empty `prompt:` on signal '{signal}'"),
            ));
        }
        bindings.push(TeamSignalBinding {
            signal: signal.clone(),
            matches,
            prompt: prompt.map(|prompt| prompt.trim().to_owned()),
        });
    }
    Ok(bindings)
}
