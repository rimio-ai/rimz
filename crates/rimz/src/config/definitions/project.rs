//! Converts trusted project TOML profile rows into definition sources resolved through the
//! shared Markdown definition loader.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use super::frontmatter::AgentFrontmatter;
use super::{
    BaseLoad, CommandsConfig, Definition, DefinitionCause, DefinitionErr, LoadContext,
    LoadContexts, LoadScope, Namespace, Profile, ProfilesConfig, SharedLoadState, SkillCatalog,
    SkillCheck, SubagentCheck, TeamsConfig, TomlExtras, agent, check_definition_name, frontmatter,
};

/// Project TOML rows and machine context handed from `config::effective::load_with_roots`.
pub(in crate::config) struct ProjectProfilesInput<'a> {
    pub config_path: &'a Path,
    pub contexts: Option<&'a LoadContexts>,
    pub agents_home: &'a Path,
    pub tiers: &'a super::super::tiers::TierConfig,
    pub auto_compact_default: Option<super::super::AutoCompactDefault>,
    pub commands: &'a CommandsConfig,
    pub machine_subagent_profiles: &'a ProfilesConfig,
    /// The machine agents table, whose rows a project row of the same name shadows.
    pub machine_agents: &'a ProfilesConfig,
    pub agents: BTreeMap<String, (Profile, Vec<String>)>,
    pub subagents: BTreeMap<String, (Profile, Vec<String>)>,
}

/// Resolved project rows, per-name formatted failure details, and ordered diagnostics.
pub(in crate::config) struct ProjectProfilesOutput {
    pub agents: BTreeMap<String, Profile>,
    pub subagents: BTreeMap<String, Profile>,
    pub failed_agents: BTreeMap<String, String>,
    pub failed_subagents: BTreeMap<String, String>,
    /// Unshadowed machine agents rows whose `subagents:` list allows a subagent that failed to
    /// load in this project; their errors stay out of `errors`, which names the root cause.
    pub failed_machine_agents: BTreeMap<String, String>,
    /// The machine kind bases among `failed_machine_agents`, each with its detail as the error
    /// a launch moved onto its kind refuses with.
    pub failed_machine_bases: BTreeMap<String, DefinitionErr>,
    /// The other unshadowed machine agents rows as this project builds them, when a project
    /// subagent failed; see [`in_project`].
    pub machine_agents: BTreeMap<String, Profile>,
    pub errors: Vec<DefinitionErr>,
}

/// Convert one parsed project TOML [`Profile`] row into written [`AgentFrontmatter`] lines plus
/// its three TOML-only [`TomlExtras`] values.
fn profile_to_definition(profile: Profile) -> (AgentFrontmatter, TomlExtras) {
    let mut fm = AgentFrontmatter::default();
    fm.agent = Some(profile.agent);
    fm.isolation = profile.isolation;
    fm.empty_dir = profile.empty_dir;
    fm.description = profile.description;
    fm.model = profile.model;
    fm.mode = profile.mode;
    fm.effort = profile.effort;
    fm.fast = profile.fast;
    fm.code_mode = profile.code_mode;
    fm.auto_compact = profile.auto_compact;
    fm.budget = profile.budget;
    fm.keep_warm = profile.keep_warm;
    fm.idle_compact = profile.idle_compact;
    fm.smart_compact = profile.smart_compact;
    fm.model_reminder = profile.model_reminder;
    fm.subagents = profile.subagents;
    fm.skills = profile
        .skills
        .map(|list| list.into_iter().map(|s| s.to_string()).collect());
    fm.mcps = profile
        .mcps
        .map(|list| list.into_iter().map(|m| m.to_string()).collect());
    let extras = TomlExtras {
        system_prompt_file: profile.system_prompt_file,
        append_system_prompt_files: profile.append_system_prompt_files,
        args: profile.args,
    };
    (fm, extras)
}

fn check_row_preconditions(
    path: &Path,
    name: &str,
    profile: &Profile,
    unknown_keys: &[String],
    both_tables: bool,
) -> Result<(), DefinitionErr> {
    if both_tables {
        return Err(DefinitionErr::new(
            path,
            format!(
                "definition '{name}' is declared in both [profiles.{name}] and \
                 [subagents.profiles.{name}]; keep one"
            ),
        ));
    }
    if !unknown_keys.is_empty() {
        return Err(DefinitionErr::new(
            path,
            format!(
                "has unknown key(s): {}; remove or rename them",
                unknown_keys
                    .iter()
                    .map(|key| format!("`{key}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    check_definition_name(path, name, Some(&TeamsConfig::default()))?;
    if let Some(subagents) = profile.subagents.as_deref()
        && let Some(entry) = subagents
            .iter()
            .find(|entry| entry.trim_start().starts_with('+'))
    {
        return Err(DefinitionErr::new(
            path,
            format!(
                "allows subagent `{entry}`: `+` entries add to an inherited list in Markdown \
                 definitions only; list the bare name"
            ),
        ));
    }
    frontmatter::description(path, profile.description.as_deref())?;
    Ok(())
}

fn build_namespace(
    path: &Path,
    rows: BTreeMap<String, (Profile, Vec<String>)>,
    duplicates: &BTreeSet<String>,
) -> (Namespace, BTreeMap<String, DefinitionErr>) {
    let mut tree = Namespace::default();
    let mut pre_errors = BTreeMap::new();
    for (name, (profile, unknown_keys)) in rows {
        if let Err(err) = check_row_preconditions(
            path,
            &name,
            &profile,
            &unknown_keys,
            duplicates.contains(&name),
        ) {
            tree.failed.insert(name.clone());
            pre_errors.insert(name, err);
            continue;
        }
        let (frontmatter, extras) = profile_to_definition(profile);
        tree.definitions.insert(
            name,
            Definition {
                path: path.to_owned(),
                pack: None,
                body: String::new(),
                frontmatter,
                extras,
            },
        );
    }
    (tree, pre_errors)
}

fn label_error(section: &str, name: &str, mut err: DefinitionErr) -> DefinitionErr {
    err.message = format!("[{section}.{name}]: {}", err.message);
    err
}

fn format_detail(
    start_name: &str,
    first: &DefinitionErr,
    agent_errors: &BTreeMap<String, DefinitionErr>,
    subagent_errors: &BTreeMap<String, DefinitionErr>,
    machine_failure: &impl Fn(&str) -> Option<String>,
) -> String {
    let mut detail = format!("{}: {}", first.path.display(), first.message);
    let mut seen = BTreeSet::from([start_name]);
    let mut cur = first;
    while let DefinitionCause::DependsOnFailed {
        name: dep,
        subagent,
    } = &cur.cause
    {
        let next = if *subagent {
            subagent_errors.get(dep.as_str())
        } else if crate::agents::find_definition(dep).is_none() {
            // A non-subagent kind dependency names a provider kind base (project rows cannot take
            // a kind's name), so resolve it from the machine kind base rather than a rejected row.
            agent_errors
                .get(dep.as_str())
                .or_else(|| subagent_errors.get(dep.as_str()))
        } else {
            None
        };
        if let Some(next_err) = next {
            if !seen.insert(dep.as_str()) {
                break;
            }
            if !cur.message.contains(&next_err.message) {
                detail.push_str("\ncaused by: ");
                detail.push_str(&format!(
                    "{}: {}",
                    next_err.path.display(),
                    next_err.message
                ));
            }
            cur = next_err;
            continue;
        }
        if let Some(machine_detail) = machine_failure(dep)
            && !detail.contains(&machine_detail)
        {
            detail.push_str("\ncaused by: ");
            detail.push_str(&machine_detail);
        }
        break;
    }
    detail
}

/// The error a machine agents `row` meets when `subagents` checks its `subagents:` list: a
/// definition through its own list check, a kind base through the check its written list met at
/// load.
fn machine_subagents_error(
    row: &Profile,
    bases: &BTreeMap<String, AgentFrontmatter>,
    subagents: &SubagentCheck,
) -> Option<DefinitionErr> {
    if let Some(definition) = &row.definition {
        return definition.check_subagents(row, subagents).err();
    }
    let listed = bases
        .get(&row.agent)
        .filter(|_| row.kind_base)?
        .subagents
        .as_deref()?;
    // A kind base's system prompt is its own rendered body, so its origin is the base's file.
    let path = row.system_prompt_file.as_ref()?.origin();
    agent::base_subagents(path, listed, subagents).err()
}

/// A machine agents `row` as a project builds it: its definition's on-demand builds, a tier
/// walk's candidates included, check their `subagents:` lists against `context` and name the
/// detail in `failed_subagents` of a subagent they depend on that failed.
fn in_project(
    row: &Profile,
    context: &Arc<LoadContext>,
    failed_subagents: &Arc<BTreeMap<String, String>>,
) -> Profile {
    let mut row = row.clone();
    row.definition = row
        .definition
        .map(|definition| definition.in_project(context, failed_subagents));
    row
}

fn fallback_contexts(input: &ProjectProfilesInput<'_>) -> LoadContexts {
    let packs = crate::packs::ResolvedPacks::default();
    let catalog = Arc::new(SkillCatalog::new(
        SkillCheck::Skip,
        input.agents_home,
        &packs,
    ));
    let mut bases = BTreeMap::new();
    let mut failed_bases = BTreeMap::new();
    let built_in = BaseLoad {
        home: input.agents_home,
        skills: &catalog,
        tiers: input.tiers,
        auto_compact_default: input.auto_compact_default,
        traits: &BTreeMap::new(),
    }
    .built_ins(&mut bases, &mut failed_bases, None);
    let shared = Arc::new(SharedLoadState {
        home: input.agents_home.to_owned(),
        bases,
        built_in,
        failed_bases,
        skills: catalog,
        tiers: input.tiers.clone(),
        auto_compact_default: input.auto_compact_default,
    });
    let allowed: BTreeSet<String> = input
        .machine_subagent_profiles
        .0
        .keys()
        .chain(input.commands.0.keys())
        .cloned()
        .collect();
    let subagents = Arc::new(LoadContext {
        shared: Arc::clone(&shared),
        subagents: None,
    });
    let agents = Arc::new(LoadContext {
        shared,
        subagents: Some(Arc::new(SubagentCheck {
            defined: BTreeSet::new(),
            failed: BTreeSet::new(),
            problems: BTreeMap::new(),
            packs: BTreeSet::new(),
            allowed,
            machine_dir: input.agents_home.join("subagents"),
            project_config: None,
        })),
    });
    LoadContexts { agents, subagents }
}

/// Convert and resolve every project `[profiles.*]` and `[subagents.profiles.*]` row through the
/// shared Markdown definition loader without rereading disk.
pub(in crate::config) fn resolve_project_profiles(
    input: ProjectProfilesInput<'_>,
    machine_failure: impl Fn(&str) -> Option<String>,
) -> ProjectProfilesOutput {
    let fallback;
    let contexts = match input.contexts {
        Some(contexts) => contexts,
        None => {
            fallback = fallback_contexts(&input);
            &fallback
        }
    };
    let duplicates: BTreeSet<String> = input
        .agents
        .keys()
        .filter(|name| input.subagents.contains_key(*name))
        .cloned()
        .collect();
    let (agents_tree, agent_pre_errors) =
        build_namespace(input.config_path, input.agents, &duplicates);
    let (subagents_tree, subagent_pre_errors) =
        build_namespace(input.config_path, input.subagents, &duplicates);
    let traits = BTreeMap::new();

    // Resolve subagents first so direct project rows validate `subagents = [...]` against them.
    let subagents_scope = LoadScope {
        context: &contexts.subagents,
        traits: &traits,
    };
    let (resolved_subagents, subagent_resolve_errors, foreign_agent_errors) =
        agent::resolve_project_namespace(
            agent::Resolver::new(
                subagents_scope,
                "subagents",
                [&subagents_tree, &agents_tree],
            )
            .with_project(),
        );

    let mut subagent_errors: BTreeMap<String, DefinitionErr> = subagent_pre_errors
        .into_iter()
        .chain(subagent_resolve_errors)
        .map(|(name, err)| {
            let labeled = label_error("subagents.profiles", &name, err);
            (name, labeled)
        })
        .collect();

    // Extend machine SubagentCheck with project subagents (project wins on name collisions).
    let mut subagent_check = contexts
        .agents
        .subagents
        .as_deref()
        .cloned()
        .unwrap_or_default();
    subagent_check.project_config = Some(input.config_path.to_owned());
    for name in resolved_subagents.keys() {
        subagent_check.defined.insert(name.clone());
        subagent_check.allowed.insert(name.clone());
        subagent_check.failed.remove(name);
        subagent_check.problems.remove(name);
    }
    for name in subagent_errors.keys() {
        subagent_check.failed.insert(name.clone());
        subagent_check.allowed.remove(name);
        subagent_check.defined.remove(name);
        subagent_check.problems.remove(name);
    }
    let project_check = Arc::new(subagent_check);
    let agents_ctx = Arc::new(LoadContext {
        shared: Arc::clone(&contexts.agents.shared),
        subagents: Some(Arc::clone(&project_check)),
    });
    let agents_scope = LoadScope {
        context: &agents_ctx,
        traits: &traits,
    };
    let (resolved_agents, mut agent_resolve_errors, _) = agent::resolve_project_namespace(
        agent::Resolver::new(agents_scope, "agents", [&agents_tree, &subagents_tree])
            .with_project(),
    );
    // Preserve an agent row's own error found while resolving a subagent child (where `subagents:`
    // is skipped), so mutual `subagents`/`agent` edges end at the line to fix instead of cycling.
    for (name, err) in foreign_agent_errors {
        agent_resolve_errors.insert(name, err);
    }

    let agent_errors: BTreeMap<String, DefinitionErr> = agent_pre_errors
        .into_iter()
        .chain(agent_resolve_errors)
        .map(|(name, err)| {
            let labeled = label_error("profiles", &name, err);
            (name, labeled)
        })
        .collect();

    let failed_agents: BTreeMap<String, String> = agent_errors
        .iter()
        .map(|(name, err)| {
            (
                name.clone(),
                format_detail(name, err, &agent_errors, &subagent_errors, &machine_failure),
            )
        })
        .collect();
    let failed_subagents: BTreeMap<String, String> = subagent_errors
        .iter()
        .map(|(name, err)| {
            (
                name.clone(),
                format_detail(name, err, &agent_errors, &subagent_errors, &machine_failure),
            )
        })
        .collect();
    let mut failed_machine_agents = BTreeMap::new();
    let mut failed_machine_bases = BTreeMap::new();
    let mut machine_agents = BTreeMap::new();
    // Only a project subagent that failed can change a machine row's verdict.
    if !subagent_errors.is_empty() {
        let bases = &contexts.agents.shared.bases;
        let failed_children = Arc::new(failed_subagents.clone());
        let refusal = |name: &str, row: &Profile| {
            let error = machine_subagents_error(row, bases, &project_check)?;
            let detail = format_detail(
                name,
                &error,
                &agent_errors,
                &subagent_errors,
                &machine_failure,
            );
            Some((error, detail))
        };
        for (name, row) in input.machine_agents.0.iter().filter(|(name, _)| {
            !resolved_agents.contains_key(*name) && !agent_errors.contains_key(*name)
        }) {
            if let Some((error, detail)) = refusal(name, row) {
                if row.kind_base {
                    let message = detail
                        .strip_prefix(&format!("{}: ", error.path.display()))
                        .unwrap_or(&detail)
                        .to_owned();
                    failed_machine_bases.insert(name.clone(), DefinitionErr { message, ..error });
                }
                failed_machine_agents.insert(name.clone(), detail);
                continue;
            }
            let row = in_project(row, &agents_ctx, &failed_children);
            machine_agents.insert(name.clone(), row);
        }
    }

    let mut errors: Vec<DefinitionErr> = agent_errors.into_values().collect();
    errors.extend(std::mem::take(&mut subagent_errors).into_values());

    ProjectProfilesOutput {
        agents: resolved_agents,
        subagents: resolved_subagents,
        failed_agents,
        failed_subagents,
        failed_machine_agents,
        failed_machine_bases,
        machine_agents,
        errors,
    }
}
