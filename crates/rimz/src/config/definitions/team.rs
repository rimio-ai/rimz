//! Team rosters, stage matrices, and fully materialized seat profiles.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::agents;
use crate::config::{FlipCompact, Profile, PromptSource, RoleBinding, Team, TeamSignalBinding};
use crate::harness::team_prompt::BUILT_IN_CONSENSUS;
use crate::store::message::AutoCompact;

use super::frontmatter::{RoleFrontmatter, SignalFrontmatter, TeamFrontmatter};
use super::{
    Definition, DefinitionErr, LoadedDefinitions, Namespace, SkillLibraryCheck, agent, files,
    frontmatter,
};

struct SeatLoader<'a> {
    home: &'a Path,
    agents: &'a Namespace,
    subagents: &'a Namespace,
    bases: &'a BTreeSet<String>,
    skills: SkillLibraryCheck<'a>,
    children: &'a BTreeSet<String>,
}

pub(super) fn load(
    home: &Path,
    agents: &Namespace,
    subagents: &Namespace,
    bases: &BTreeSet<String>,
    skills: SkillLibraryCheck<'_>,
    children: &BTreeSet<String>,
    loaded: &mut LoadedDefinitions,
) {
    let seats = SeatLoader {
        home,
        agents,
        subagents,
        bases,
        skills,
        children,
    };
    let paths = match files(&home.join("teams")) {
        Ok(paths) => paths,
        Err(error) => {
            loaded.errors.push(error);
            return;
        }
    };
    let mut names = BTreeSet::new();
    for path in paths {
        let result = (|| {
            let text = std::fs::read_to_string(&path)
                .map_err(|error| DefinitionErr::new(&path, error.to_string()))?;
            let (yaml, body) = frontmatter::split(&path, &text)?;
            let fm: TeamFrontmatter = frontmatter::parse(&path, yaml)?;
            let name = fm.name.clone().unwrap_or_else(|| {
                path.file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                return Err(DefinitionErr::new(
                    &path,
                    format!("unsafe team name {name:?}"),
                ));
            }
            if !names.insert(name.clone()) {
                loaded.teams.0.remove(&name);
                loaded.sources.teams.remove(&name);
                loaded.rows.retain(|row| {
                    if row.team.as_deref() != Some(&name) {
                        return true;
                    }
                    loaded.agent_profiles.0.remove(&row.name);
                    loaded.sources.agent_profiles.remove(&row.name);
                    false
                });
                return Err(DefinitionErr::new(
                    &path,
                    format!("team '{name}' is declared twice"),
                ));
            }
            let team = roster(&path, &name, &fm, body)?;
            let mut profiles = Vec::new();
            let mut errors = Vec::new();
            for (role, binding) in fm.roles.iter().zip(&team.roles) {
                match seats.load(&path, role, &fm, loaded) {
                    Ok(profile) => profiles.push((binding, profile)),
                    Err(error) => errors.push(error),
                }
            }
            if !errors.is_empty() {
                loaded.errors.extend(errors);
                return Ok(());
            }
            for (binding, profile) in profiles {
                loaded.insert("agents", &binding.profile, &path, profile);
                if let Some(row) = loaded.rows.last_mut() {
                    row.namespace = "team".to_owned();
                    row.team = Some(name.clone());
                    row.role = Some(binding.role.clone());
                    row.owns = binding.owns.clone();
                    row.signals = binding.signals.clone();
                }
            }
            loaded.sources.teams.insert(name.clone(), path.clone());
            loaded.teams.0.insert(name, team);
            Ok(())
        })();
        if let Err(error) = result {
            loaded.errors.push(error);
        }
    }
}

fn roster(
    path: &Path,
    name: &str,
    fm: &TeamFrontmatter,
    body: &str,
) -> Result<Team, DefinitionErr> {
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
            flip_compact: Some(flip_compact(path, role.flip_compact.as_deref(), &owns)?),
            signals: signals(path, role.signals.as_deref())?,
            owns,
            mode: None,
            model: None,
            effort: None,
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
    for text in [BUILT_IN_CONSENSUS, body] {
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
    Ok(Team {
        roles,
        leader: Some(leader.to_owned()),
        layout: fm.layout.clone(),
        stages,
        scratch_files: None,
        consensus_file: None,
        append_system_prompt_files: vec![PromptSource::Text {
            origin: path.to_owned(),
            text: body.trim().to_owned(),
        }],
    })
}

impl SeatLoader<'_> {
    fn load(
        &self,
        path: &Path,
        role: &RoleFrontmatter,
        team: &TeamFrontmatter,
        loaded: &LoadedDefinitions,
    ) -> Result<Profile, DefinitionErr> {
        let definition = self.agents.definitions.get(&role.agent)
            .filter(|_| loaded.agent_profiles.0.contains_key(&role.agent))
            .ok_or_else(|| {
                DefinitionErr::new(path, format!("selects unknown or failed agent '{}'; team roles can select definitions from agents only", role.agent))
            })?;
        let original = &loaded.agent_profiles.0[&role.agent];
        let mut fm = role.overlay();
        let mut ancestor = Some(definition);
        while let Some(definition) = ancestor {
            fm.inherit(&definition.frontmatter);
            ancestor = definition
                .frontmatter
                .agent
                .as_ref()
                .and_then(|name| self.agents.definitions.get(name));
        }
        let kind = fm
            .model
            .as_deref()
            .and_then(agents::definition_model_kind)
            .unwrap_or(&original.agent);
        if !self.bases.contains(kind) {
            return Err(DefinitionErr::new(
                path,
                format!("seats {kind}, whose kind base `agents/{kind}.md` is missing"),
            ));
        }
        fm.agent = Some(kind.to_owned());
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
        if team.leader.as_deref() != Some(role.role.as_deref().unwrap_or(&role.agent))
            && let Some(tools) = &mut fm.tools
        {
            tools.retain(|tool| tool.split('(').next().unwrap_or("").trim() != "AskUserQuestion");
        }
        if let Some(skills) = &mut fm.skills
            && !skills.is_empty()
            && !skills.iter().any(|skill| skill.trim() == "reflect")
        {
            skills.push("reflect".to_owned());
        }
        // Resolve the overlaid leaf through the standalone checks/defaults, then restore its already-rendered ancestor crafts unchanged.
        let tree = Namespace {
            definitions: BTreeMap::from([(
                role.agent.clone(),
                Definition {
                    path: path.to_owned(),
                    body: definition.body.clone(),
                    frontmatter: fm,
                },
            )]),
            failed: BTreeSet::new(),
        };
        let mut seat = LoadedDefinitions::default();
        agent::resolve_namespace(
            agent::Resolver::new(
                self.home,
                "agents",
                &tree,
                self.subagents,
                self.bases,
                self.skills,
                self.children,
            ),
            &mut seat,
        );
        if let Some(error) = seat.errors.into_iter().next() {
            return Err(error);
        }
        let mut profile = seat.agent_profiles.0.remove(&role.agent).ok_or_else(|| {
            DefinitionErr::new(path, format!("seat '{}' failed to resolve", role.agent))
        })?;
        let mut crafts: Vec<_> = original
            .append_system_prompt_files
            .iter()
            .filter(|source| source.origin() != definition.path)
            .cloned()
            .collect();
        crafts.append(&mut profile.append_system_prompt_files);
        profile.append_system_prompt_files = crafts;
        Ok(profile)
    }
}

fn flip_compact(
    path: &Path,
    value: Option<&str>,
    owns: &[String],
) -> Result<FlipCompact, DefinitionErr> {
    let value = value
        .unwrap_or_else(|| {
            if owns.iter().any(|stage| stage == "Plan") {
                "120k"
            } else {
                "180k"
            }
        })
        .trim();
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
            && !matches.contains_key("handle")
            && !matches.contains_key("session")
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
