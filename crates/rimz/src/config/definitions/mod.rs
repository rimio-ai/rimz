//! Machine-tier Markdown definitions, resolved into the existing profile schema.
//! Errors without a `failed` owner refuse every launch; pack-scoped diagnostics use
//! `DefinitionCause::PackProblem` to retain reporting without failing the definition set.

mod agent;
mod catalog;
pub(crate) mod frontmatter;
pub mod inventory;
mod team;
mod traits;

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::{AgentSpecSources, CommandsConfig, Profile, ProfilesConfig, PromptSource, TeamsConfig};
use crate::packs::{ClaimResolution, ItemFate, PackItem, PackName, Problem, ResolvedPacks};
use frontmatter::{AgentFrontmatter, BaseFrontmatter};

/// A user-shell launch that can reach a provider's hook-trust prompt.
pub fn hook_trust_fix(
    kind: &crate::ids::AgentKind,
    profile: Option<&str>,
    relaunch: Option<(&crate::agents::AgentState, &str)>,
) -> String {
    let team_member = |agent: &crate::agents::AgentState| {
        (agent.team.is_some() && agent.role.is_some())
            || profile.is_some_and(|profile| profile.contains('.'))
    };
    // `rimz agents resume @handle` refuses team members; restart and fork accept them.
    if let Some((agent, action)) = relaunch
        && !(action == "resume" && team_member(agent))
    {
        return format!(
            "rimz agents {action} @{}",
            agent.name.as_deref().unwrap_or(agent.agent_id.as_str())
        );
    }
    let profile = profile.and_then(standalone_profile);
    format!(
        "rimz agents {}",
        profile.as_deref().unwrap_or(kind.as_str())
    )
}

fn standalone_profile(profile: &str) -> Option<String> {
    let Some((team, role)) = profile.split_once('.') else {
        return Some(profile.to_owned());
    };
    let (_, sources) = super::MachineConfig::load_with_agent_spec_sources().ok()?;
    let path = sources.team(team)?;
    let text = std::fs::read_to_string(path).ok()?;
    let (yaml, _) = frontmatter::split(path, &text).ok()?;
    let team: frontmatter::TeamFrontmatter = frontmatter::parse(path, yaml).ok()?;
    team.roles
        .into_iter()
        .find(|binding| binding.role.as_deref().unwrap_or(&binding.agent) == role)
        .map(|binding| binding.agent)
}

/// Whether enforced skill lists resolve against the provider root under `env`, then the
/// RimZ skill chain derived from the loader's home. Enforcement uses inherited isolation,
/// then `machine_isolation`; off Linux every definition runs on the host.
/// The check reads host roots; callers inside a scoped view must skip it.
#[derive(Clone, Copy, Debug)]
pub enum SkillCheck<'a> {
    Skip,
    Check {
        env: &'a BTreeMap<String, String>,
        machine_isolation: super::Isolation,
    },
}

type SkillInventory = Rc<crate::agents::skills::SkillInventory>;
type TextRead = Result<Rc<str>, Rc<std::io::Error>>;

#[cfg(test)]
thread_local! {
    static TEXT_READS: RefCell<BTreeMap<PathBuf, usize>> = const { RefCell::new(BTreeMap::new()) };
}

#[cfg(test)]
fn count_text_read(path: &Path) {
    TEXT_READS.with(|reads| *reads.borrow_mut().entry(path.to_path_buf()).or_default() += 1);
}

struct SkillCatalog<'a> {
    check: SkillCheck<'a>,
    chain: crate::agents::skills::SkillChain,
    inventories: RefCell<BTreeMap<String, Result<SkillInventory, String>>>,
    texts: RefCell<BTreeMap<PathBuf, TextRead>>,
}

impl<'a> SkillCatalog<'a> {
    fn new(check: SkillCheck<'a>, home: &Path) -> Self {
        Self {
            check,
            chain: crate::agents::skills::SkillChain::from_home(home),
            inventories: RefCell::default(),
            texts: RefCell::default(),
        }
    }

    fn read_text(&self, path: &Path) -> TextRead {
        self.texts
            .borrow_mut()
            .entry(path.to_path_buf())
            .or_insert_with(|| {
                #[cfg(test)]
                count_text_read(path);
                std::fs::read_to_string(path).map(Rc::from).map_err(Rc::new)
            })
            .clone()
    }

    fn enumerate(&self, kind: &str) -> Result<Option<SkillInventory>, String> {
        let SkillCheck::Check { env, .. } = self.check else {
            return Ok(None);
        };
        self.inventories
            .borrow_mut()
            .entry(kind.to_owned())
            .or_insert_with(|| {
                crate::agents::skills::enumerate(
                    crate::agents::find_definition(kind)
                        .and_then(|definition| definition.skills_home(env))
                        .as_deref(),
                    Some(&self.chain),
                    |_| true,
                )
                .map(Rc::new)
                .map_err(|error| error.to_string())
            })
            .clone()
            .map(Some)
    }
}

#[derive(Debug, Default)]
pub struct LoadedDefinitions {
    pub tools: BTreeMap<String, Vec<String>>,
    pub agent_profiles: ProfilesConfig,
    pub subagent_profiles: ProfilesConfig,
    pub teams: TeamsConfig,
    pub sources: AgentSpecSources,
    pub errors: Vec<DefinitionErr>,
    pub failed: BTreeMap<String, BTreeSet<PathBuf>>,
    pub rows: Vec<DefinitionRow>,
    pub pack_items: Vec<PackItem>,
    pub warnings: Vec<crate::packs::Problem>,
    pack_problems: BTreeMap<PackName, Vec<Problem>>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{path}: {message}")]
pub struct DefinitionErr {
    pub path: PathBuf,
    pub message: String,
    pub cause: DefinitionCause,
}

/// Why a definition could not load, for surfaces that group failures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DefinitionCause {
    /// A listed skill is absent from the provider root and RimZ skill chain.
    MissingSkill {
        skill: super::SkillName,
        roots: Vec<PathBuf>,
    },
    /// A parent, kind base, allowed subagent, or team role's agent failed to load.
    DependsOnFailed { name: String },
    /// A pack-scoped diagnostic that does not refuse unrelated launches.
    PackProblem,
    /// The message describes a failure that has no more specific grouping.
    Invalid,
}

/// The tail of every refusal for a definition whose kind base is absent.
/// Nothing in the tree ships one, so naming the file is not enough: the
/// message has to say what a kind base is, where to write it, and that an
/// empty body is refused in turn.
fn missing_kind_base(home: &Path, kind: &str) -> String {
    format!(
        "whose kind base is missing — create {} with a nonempty body, the system prompt every \
         {kind} definition starts from",
        home.join("agents").join(format!("{kind}.md")).display()
    )
}

/// The tail of every refusal for a definition whose kind base exists but failed to load,
/// so the user fixes that file rather than creating it again.
fn failed_kind_base(path: &Path) -> String {
    format!(
        "whose kind base {} failed to load; fix that file",
        path.display()
    )
}

impl DefinitionErr {
    fn new(path: &Path, message: impl AsRef<str>) -> Self {
        Self {
            path: path.to_owned(),
            cause: DefinitionCause::Invalid,
            message: message
                .as_ref()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        }
    }

    fn with_cause(mut self, cause: DefinitionCause) -> Self {
        self.cause = cause;
        self
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct DefinitionRow {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_warm: Option<super::KeepWarmFields>,
    pub name: String,
    pub namespace: String,
    pub kind: String,
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<super::tiers::ModelTier>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier_fallback: Option<bool>,
    pub effort: Option<String>,
    /// Whether the definition launches in the provider's fast mode.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fast: bool,
    pub source: PathBuf,
    pub team: Option<String>,
    pub role: Option<String>,
    pub owns: Vec<String>,
    pub signals: Vec<super::TeamSignalBinding>,
}

#[derive(Clone)]
struct Definition {
    path: PathBuf,
    body: String,
    frontmatter: AgentFrontmatter,
}

#[derive(Default)]
struct Namespace {
    definitions: BTreeMap<String, Definition>,
    failed: BTreeSet<String>,
    problems: BTreeMap<String, DefinitionErr>,
    packs: BTreeSet<String>,
}

impl Namespace {
    fn lookup(&self, name: &str, source: &Path) -> Result<Option<&Definition>, DefinitionErr> {
        if let Some(error) = self.problems.get(name) {
            return Err(DefinitionErr::new(source, &error.message).with_cause(error.cause.clone()));
        }
        if let Some(definition) = self.definitions.get(name) {
            return Ok(Some(definition));
        }
        if let Some((pack, item)) = name.split_once(':') {
            let message = if self.packs.contains(pack) {
                format!(
                    "pack '{pack}' does not ship '{item}' in this namespace; run `rimz packs show {pack}`"
                )
            } else {
                format!(
                    "unknown pack '{pack}'; installed packs: {}; run `rimz packs list`",
                    self.packs.iter().cloned().collect::<Vec<_>>().join(", ")
                )
            };
            return Err(DefinitionErr::new(source, message));
        }
        Ok(None)
    }
}

/// Markdown files under `home` that are not definitions: contributor notes in
/// definition trees, and the inspection copy of the built-in consensus RimZ
/// publishes into `teams/`. Home traits alone admit contributor-note names.
fn reserved(home: &Path, path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("AGENTS.md" | "CLAUDE.md" | "README.md")
    ) || path == crate::harness::team_prompt::consensus_copy_path(home)
}

fn files(home: &Path, namespace: &str) -> Result<Vec<PathBuf>, DefinitionErr> {
    let directory = home.join(namespace);
    let directory = directory.as_path();
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(DefinitionErr::new(directory, error.to_string())),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| DefinitionErr::new(directory, error.to_string()))?;
        let path = entry.path();
        if !path.extension().is_some_and(|extension| extension == "md")
            || (namespace != "traits" && reserved(home, &path))
        {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|error| DefinitionErr::new(&path, error.to_string()))?;
        if file_type.is_file() || file_type.is_symlink() {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

pub(super) fn source_paths(agents_home: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for name in ["agents", "subagents", "teams", "traits"] {
        paths.extend(files(agents_home, name).unwrap_or_default());
        paths.push(agents_home.join(name));
    }
    paths.sort();
    paths
}

/// The inputs every resolver and seat loader in one [`load`] shares.
#[derive(Clone, Copy)]
struct LoadScope<'a> {
    home: &'a Path,
    bases: &'a BTreeSet<String>,
    failed_bases: &'a BTreeMap<String, DefinitionErr>,
    traits: &'a BTreeMap<String, ClaimResolution>,
    skills: &'a SkillCatalog<'a>,
    tiers: &'a super::tiers::TierConfig,
    native_auto_compact: Option<u64>,
}

/// Non-empty, and only ASCII alphanumerics, `_`, and `-`.
fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// `commands` are the `[agents.commands]` names a `subagents:` list may also allow.
pub fn load(
    agents_home: &Path,
    skills: SkillCheck<'_>,
    commands: &CommandsConfig,
    config_path: &Path,
    tiers: &super::tiers::TierConfig,
    native_auto_compact: Option<u64>,
) -> LoadedDefinitions {
    load_resolved(
        agents_home,
        skills,
        commands,
        config_path,
        tiers,
        native_auto_compact,
        &crate::packs::resolve(agents_home),
    )
}

fn load_resolved(
    agents_home: &Path,
    skills: SkillCheck<'_>,
    commands: &CommandsConfig,
    config_path: &Path,
    tiers: &super::tiers::TierConfig,
    native_auto_compact: Option<u64>,
    packs: &ResolvedPacks,
) -> LoadedDefinitions {
    let catalog = SkillCatalog::new(skills, agents_home);
    let mut loaded = LoadedDefinitions::default();
    loaded.warnings.clone_from(&packs.warnings);
    loaded.errors.extend(packs.problems.iter().map(|problem| {
        DefinitionErr::new(&problem.path, &problem.message).with_cause(DefinitionCause::PackProblem)
    }));
    let sources = catalog::Catalog::new(
        agents_home,
        packs,
        commands,
        config_path,
        &catalog,
        &mut loaded,
    );
    if let Ok(inventory) = catalog.chain.inventory(|_| true) {
        for problem in inventory
            .conflicts
            .values()
            .chain(inventory.pack_problems.values().flatten())
        {
            loaded.errors.push(
                DefinitionErr::new(&problem.path, &problem.message)
                    .with_cause(DefinitionCause::PackProblem),
            );
        }
        for (pack, problems) in inventory.pack_problems {
            loaded
                .pack_problems
                .entry(pack)
                .or_default()
                .extend(problems);
        }
    }
    let installed: BTreeSet<_> = packs
        .record
        .sources
        .iter()
        .flat_map(|source| &source.packs)
        .map(|pack| pack.name.to_string())
        .collect();
    let mut agents = Namespace {
        packs: installed.clone(),
        ..Namespace::default()
    };
    let mut subagents = Namespace {
        packs: installed,
        ..Namespace::default()
    };
    for (name, claim) in &sources.claims["definitions"] {
        if let ClaimResolution::Conflict { claims, message } = claim {
            let error = DefinitionErr::new(&claims[0].path, message)
                .with_cause(DefinitionCause::DependsOnFailed { name: name.clone() });
            for tree in [&mut agents, &mut subagents] {
                tree.failed.insert(name.clone());
                tree.problems.insert(name.clone(), error.clone());
            }
        }
    }
    let mut bases = BTreeSet::new();
    let mut failed_bases = BTreeMap::new();
    for (name, claim) in &sources.claims["bases"] {
        if let ClaimResolution::Conflict { claims, message } = claim {
            failed_bases.insert(
                name.clone(),
                DefinitionErr::new(&claims[0].path, message)
                    .with_cause(DefinitionCause::DependsOnFailed { name: name.clone() }),
            );
        }
    }
    let mut names: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (namespace, tree) in [("agents", &mut agents), ("subagents", &mut subagents)] {
        for item in sources
            .items
            .iter()
            .filter(|item| item.kind.as_str() == namespace)
        {
            let path = &item.path;
            let stem = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let name = &item.name;
            let kind_base = crate::agents::find_definition(&stem).filter(|_| namespace == "agents");
            if kind_base.is_some() && !item.selected() {
                continue;
            }
            let qualified = item.pack.as_ref().map(|pack| format!("{pack}:{name}"));
            if item.excluded() {
                if let Some(key) = qualified {
                    let pack = item
                        .pack
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    let target = if matches!(item.fate, Some(ItemFate::ExcludedType)) {
                        format!("{pack} --type {namespace}")
                    } else {
                        format!("{pack}:{name} --type {namespace}")
                    };
                    tree.problems.insert(
                        key,
                        DefinitionErr::new(
                            path,
                            format!("pack item is excluded; run `rimz packs include {target}`"),
                        ),
                    );
                }
                continue;
            }
            let keys: Vec<_> = item
                .selected()
                .then(|| name.clone())
                .into_iter()
                .chain(qualified)
                .collect();
            let result = (|| {
                if let Some(definition) = kind_base
                    && !definition.spec().launch.takes_system_prompt()
                {
                    return Err(DefinitionErr::new(
                        path,
                        format!(
                            "{stem} takes no system prompt, so this kind base can never reach a \
                             launch; delete {}",
                            path.display()
                        ),
                    ));
                }
                let text = catalog
                    .read_text(path)
                    .map_err(|error| DefinitionErr::new(path, error.to_string()))?;
                let (yaml, body) = frontmatter::split(path, &text)?;
                if kind_base.is_some() {
                    let fm: BaseFrontmatter = frontmatter::parse(path, yaml)?;
                    let description = frontmatter::description(path, fm.description.as_deref())?;
                    if body.trim().is_empty() {
                        return Err(DefinitionErr::new(
                            path,
                            format!(
                                "kind base has no prompt body — write the system prompt every \
                                 {stem} definition starts from below the frontmatter"
                            ),
                        ));
                    }
                    let mut profile = agent::empty_profile(&stem);
                    profile.description = Some(description);
                    profile.system_prompt_file = Some(PromptSource::Text {
                        origin: path.clone(),
                        text: body.trim().to_owned(),
                    });
                    bases.insert(stem.clone());
                    loaded.insert("agents", &stem, path, profile.clone());
                    loaded.insert("subagents", &stem, path, profile);
                    return Ok(());
                }
                if !is_safe_name(name) {
                    return Err(DefinitionErr::new(
                        path,
                        format!("has unsafe definition name {name:?}"),
                    ));
                }
                if crate::agents::find_definition(name).is_some() {
                    return Err(DefinitionErr::new(
                        path,
                        "takes the name of an agent kind; rename it",
                    ));
                }
                if item.pack.is_some() {
                    crate::harness::spec::validate_config(
                        &ProfilesConfig(BTreeMap::from([(name.clone(), agent::empty_profile(""))])),
                        &CommandsConfig::default(),
                        &super::AgentsConfig::default().teams,
                    )
                    .map_err(|error| item.error(error))?;
                }
                for key in &keys {
                    if let Some(previous) = names.get(key) {
                        tree.failed.insert(key.clone());
                        loaded
                            .failed
                            .entry(key.clone())
                            .or_default()
                            .insert(previous.clone());
                        return Err(DefinitionErr::new(
                            path,
                            format!(
                                "definition '{name}' is declared by both {} and {}",
                                previous.display(),
                                path.display()
                            ),
                        ));
                    }
                    names.insert(key.clone(), path.clone());
                }
                let fm: AgentFrontmatter = frontmatter::parse(path, yaml)?;
                frontmatter::description(path, fm.description.as_deref())?;
                if item.selected()
                    && let Some(tools) = &fm.tools
                {
                    loaded.tools.insert(name.clone(), tools.clone());
                }
                let definition = Definition {
                    path: path.clone(),
                    body: body.trim().to_owned(),
                    frontmatter: fm,
                };
                for key in &keys {
                    tree.definitions.insert(key.clone(), definition.clone());
                }
                Ok(())
            })();
            if let Err(error) = result {
                let error = if item.pack.is_some() && error.cause != DefinitionCause::PackProblem {
                    item.error(error.message)
                } else {
                    error
                };
                if kind_base.is_some() {
                    failed_bases.insert(name.clone(), error.clone());
                }
                for key in keys {
                    tree.failed.insert(key.clone());
                    if key.contains(':') {
                        tree.problems.insert(
                            key.clone(),
                            DefinitionErr::new(
                                path,
                                format!("follows `{key}`, which failed to load: {error}"),
                            )
                            .with_cause(DefinitionCause::DependsOnFailed { name: key }),
                        );
                    }
                }
                if item.selected() {
                    loaded
                        .failed
                        .entry(name.clone())
                        .or_default()
                        .insert(path.clone());
                    loaded.errors.push(error);
                }
            }
        }
    }
    // Only kind bases have been materialized yet; each family keeps its own base prompt.
    let families: BTreeSet<_> = tiers
        .entries(super::tiers::ModelTier::Intern)
        .map(|(_, kind, _)| kind)
        .collect();
    let mut renders = BTreeMap::new();
    let mut exclusions = BTreeMap::new();
    for kind in families {
        if let Some(profile) = loaded.agent_profiles.0.get(kind) {
            renders.insert(kind.to_owned(), profile.clone());
        } else if let Some(error) = failed_bases.get(kind) {
            let message = if matches!(error.cause, DefinitionCause::DependsOnFailed { .. }) {
                error.message.clone()
            } else {
                failed_kind_base(&error.path)
            };
            exclusions.insert(kind.to_owned(), message);
        } else {
            exclusions.insert(kind.to_owned(), missing_kind_base(agents_home, kind));
        }
    }
    for profiles in [&mut loaded.agent_profiles, &mut loaded.subagent_profiles] {
        for profile in profiles.0.values_mut() {
            profile.definition_renders = Some(super::tiers::DefinitionRenders {
                preference: super::tiers::TierPreference {
                    model: None,
                    family: Some(profile.agent.clone()),
                },
                renders: renders.clone(),
                exclusions: exclusions.clone(),
                effort: None,
            });
        }
    }
    // A public name belongs to exactly one tree; neither copy is loadable.
    for name in agents.definitions.keys() {
        if subagents.failed.contains(name) || subagents.definitions.contains_key(name) {
            agents.failed.insert(name.clone());
            subagents.failed.insert(name.clone());
        }
    }
    let scope = LoadScope {
        home: agents_home,
        bases: &bases,
        failed_bases: &failed_bases,
        traits: &sources.claims["traits"],
        skills: &catalog,
        tiers,
        native_auto_compact,
    };
    // Children resolve first so an agent's `subagents:` checks against what loaded.
    agent::resolve_namespace(
        agent::Resolver::new(scope, "subagents", [&subagents, &agents], &BTreeSet::new()),
        &mut loaded,
    );
    let children: BTreeSet<String> = loaded
        .subagent_profiles
        .0
        .keys()
        .chain(commands.0.keys())
        .cloned()
        .collect();
    agent::resolve_namespace(
        agent::Resolver::new(scope, "agents", [&agents, &subagents], &children),
        &mut loaded,
    );
    team::load(
        scope,
        [&agents, &subagents],
        &children,
        &sources,
        commands,
        &mut loaded,
    );
    loaded.failed.retain(|name, paths| {
        if crate::agents::find_definition(name).is_some()
            && paths.iter().all(|path| {
                sources.items.iter().any(|item| {
                    item.pack.is_some() && item.path == *path && item.namespace() != "bases"
                })
            })
        {
            for error in loaded
                .errors
                .iter_mut()
                .filter(|error| paths.contains(&error.path))
            {
                error.cause = DefinitionCause::PackProblem;
            }
            return false;
        }
        !loaded.agent_profiles.0.contains_key(name)
            && !loaded.subagent_profiles.0.contains_key(name)
            && !loaded.teams.0.contains_key(name)
    });
    let owned: BTreeSet<_> = loaded.failed.values().flatten().collect();
    for error in &mut loaded.errors {
        if !owned.contains(&error.path)
            && sources
                .items
                .iter()
                .any(|item| item.pack.is_some() && item.path == error.path)
        {
            error.cause = DefinitionCause::PackProblem;
        }
    }
    loaded.pack_items = sources.pack_items(&loaded, &catalog);
    loaded
}

impl LoadedDefinitions {
    fn insert(&mut self, namespace: &str, name: &str, source: &Path, profile: Profile) {
        self.rows.push(DefinitionRow {
            keep_warm: profile
                .keep_warm
                .as_ref()
                .and_then(|policy| policy.fields().max)
                .filter(|max| *max != super::KeepWarm::Off)
                .map(|max| super::KeepWarmFields {
                    max: Some(max),
                    while_clause: None,
                }),
            name: name.to_owned(),
            namespace: namespace.to_owned(),
            kind: profile.agent.clone(),
            model: profile.model.clone(),
            tier: profile.model_tier.as_ref().map(|tier| tier.tier),
            tier_fallback: profile.model_tier.as_ref().map(|tier| tier.fell_back),
            effort: profile.effective_effort(),
            fast: profile.fast == Some(true),
            source: source.to_owned(),
            team: None,
            role: None,
            owns: Vec::new(),
            signals: Vec::new(),
        });
        let (profiles, sources) = if namespace == "agents" {
            (&mut self.agent_profiles, &mut self.sources.agent_profiles)
        } else {
            (
                &mut self.subagent_profiles,
                &mut self.sources.subagent_profiles,
            )
        };
        profiles.0.insert(name.to_owned(), profile);
        sources.insert(name.to_owned(), source.to_owned());
    }
}
