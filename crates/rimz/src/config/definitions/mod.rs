//! Machine-tier Markdown definitions, resolved into the existing profile schema.

mod agent;
mod frontmatter;
mod team;
mod traits;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::{AgentSpecSources, CommandsConfig, Profile, ProfilesConfig, PromptSource, TeamsConfig};
use frontmatter::{AgentFrontmatter, BaseFrontmatter};

#[derive(Clone, Copy, Debug)]
pub enum SkillLibraryCheck<'a> {
    Skip,
    Check(&'a Path),
}

#[derive(Debug, Default)]
pub struct LoadedDefinitions {
    pub agent_profiles: ProfilesConfig,
    pub subagent_profiles: ProfilesConfig,
    pub teams: TeamsConfig,
    pub sources: AgentSpecSources,
    pub errors: Vec<DefinitionErr>,
    pub rows: Vec<DefinitionRow>,
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("{path}: {message}")]
pub struct DefinitionErr {
    pub path: PathBuf,
    pub message: String,
}

impl DefinitionErr {
    fn new(path: &Path, message: impl AsRef<str>) -> Self {
        Self {
            path: path.to_owned(),
            message: message
                .as_ref()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct DefinitionRow {
    pub name: String,
    pub namespace: String,
    pub kind: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub source: PathBuf,
    pub team: Option<String>,
    pub role: Option<String>,
    pub owns: Vec<String>,
    pub signals: Vec<super::TeamSignalBinding>,
}

struct Definition {
    path: PathBuf,
    body: String,
    frontmatter: AgentFrontmatter,
}

#[derive(Default)]
struct Namespace {
    definitions: BTreeMap<String, Definition>,
    failed: BTreeSet<String>,
}

fn files(directory: &Path) -> Result<Vec<PathBuf>, DefinitionErr> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(DefinitionErr::new(directory, error.to_string())),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|error| DefinitionErr::new(directory, error.to_string()))?
            .path();
        if path.is_file()
            && path.extension().is_some_and(|extension| extension == "md")
            && !matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("AGENTS.md" | "CLAUDE.md" | "README.md")
            )
        {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

pub fn source_paths(agents_home: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for name in ["agents", "subagents", "teams", "traits"] {
        let directory = agents_home.join(name);
        paths.extend(files(&directory).unwrap_or_default());
        paths.push(directory);
    }
    paths.sort();
    paths
}

/// `commands` are the `[agents.commands]` names a `subagents:` list may also allow.
pub fn load(
    agents_home: &Path,
    skills: SkillLibraryCheck<'_>,
    commands: &CommandsConfig,
) -> LoadedDefinitions {
    let mut loaded = LoadedDefinitions::default();
    let mut agents = Namespace::default();
    let mut subagents = Namespace::default();
    let mut bases = BTreeSet::new();
    let mut names: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (namespace, tree) in [("agents", &mut agents), ("subagents", &mut subagents)] {
        let paths = match files(&agents_home.join(namespace)) {
            Ok(paths) => paths,
            Err(error) => {
                loaded.errors.push(error);
                continue;
            }
        };
        for path in paths {
            let stem = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let mut name = stem.clone();
            let result = (|| {
                let text = std::fs::read_to_string(&path)
                    .map_err(|error| DefinitionErr::new(&path, error.to_string()))?;
                let (yaml, body) = frontmatter::split(&path, &text)?;
                if namespace == "agents" && crate::agents::find_definition(&stem).is_some() {
                    let fm: BaseFrontmatter = frontmatter::parse(&path, yaml)?;
                    let description = frontmatter::description(&path, fm.description.as_deref())?;
                    if body.trim().is_empty() {
                        return Err(DefinitionErr::new(&path, "kind base has no prompt body"));
                    }
                    let mut profile = agent::empty_profile(&stem);
                    profile.description = Some(description);
                    profile.system_prompt_file = Some(PromptSource::Text {
                        origin: path.clone(),
                        text: body.trim().to_owned(),
                    });
                    bases.insert(stem.clone());
                    loaded.insert("agents", &stem, &path, profile.clone());
                    loaded.insert("subagents", &stem, &path, profile);
                    return Ok(());
                }
                #[derive(serde::Deserialize)]
                struct Name {
                    name: Option<String>,
                }
                if let Ok(fm) = serde_saphyr::from_str::<Name>(yaml) {
                    name = fm.name.unwrap_or_else(|| stem.clone());
                }
                if name.is_empty()
                    || !name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                {
                    return Err(DefinitionErr::new(
                        &path,
                        format!("has unsafe definition name {name:?}"),
                    ));
                }
                if crate::agents::find_definition(&name).is_some() {
                    return Err(DefinitionErr::new(
                        &path,
                        "takes the name of an agent kind; rename it",
                    ));
                }
                if let Some(previous) = names.get(&name) {
                    tree.failed.insert(name.clone());
                    return Err(DefinitionErr::new(
                        &path,
                        format!(
                            "definition '{name}' is declared by both {} and {}",
                            previous.display(),
                            path.display()
                        ),
                    ));
                }
                names.insert(name.clone(), path.clone());
                let fm: AgentFrontmatter = frontmatter::parse(&path, yaml)?;
                frontmatter::description(&path, fm.description.as_deref())?;
                tree.definitions.insert(
                    name.clone(),
                    Definition {
                        path: path.clone(),
                        body: body.trim().to_owned(),
                        frontmatter: fm,
                    },
                );
                Ok(())
            })();
            if let Err(error) = result {
                tree.failed.insert(name);
                loaded.errors.push(error);
            }
        }
    }
    // A public name belongs to exactly one tree; neither copy is loadable.
    for name in agents.definitions.keys() {
        if subagents.failed.contains(name) || subagents.definitions.contains_key(name) {
            agents.failed.insert(name.clone());
            subagents.failed.insert(name.clone());
        }
    }
    // Children resolve first so an agent's `subagents:` checks against what loaded.
    agent::resolve_namespace(
        agent::Resolver::new(
            agents_home,
            "subagents",
            &subagents,
            &agents,
            &bases,
            skills,
            &BTreeSet::new(),
        ),
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
        agent::Resolver::new(
            agents_home,
            "agents",
            &agents,
            &subagents,
            &bases,
            skills,
            &children,
        ),
        &mut loaded,
    );
    team::load(
        agents_home,
        &agents,
        &subagents,
        &bases,
        skills,
        &children,
        &mut loaded,
    );
    loaded
}

impl LoadedDefinitions {
    fn insert(&mut self, namespace: &str, name: &str, source: &Path, profile: Profile) {
        self.rows.push(DefinitionRow {
            name: name.to_owned(),
            namespace: namespace.to_owned(),
            kind: profile.agent.clone(),
            model: profile.model.clone(),
            effort: profile.effort.clone(),
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
