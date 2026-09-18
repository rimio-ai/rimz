//! Chain resolution and profile materialization for both definition namespaces.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::agents::{self, ManualSkill, PresetField, ToolSet};
use crate::config::{Profile, PromptSource, SkillName};

use super::frontmatter::AgentFrontmatter;
use super::{DefinitionErr, LoadedDefinitions, Namespace, SkillCheck, frontmatter, traits};

#[derive(Clone)]
struct Resolved {
    frontmatter: AgentFrontmatter,
    profile: Profile,
}

pub(super) fn empty_profile(kind: &str) -> Profile {
    Profile {
        agent: kind.to_owned(),
        skills: None,
        description: None,
        subagents: None,
        model_reminder: None,
        mode: None,
        model: None,
        effort: None,
        budget: None,
        auto_compact: None,
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        args: None,
    }
}

/// Resolves every definition of one tree into `loaded`.
pub(super) fn resolve_namespace(mut resolver: Resolver<'_>, loaded: &mut LoadedDefinitions) {
    for name in resolver.tree.definitions.keys() {
        if resolver.resolve(name).is_none() {
            loaded
                .failed
                .entry(name.clone())
                .or_default()
                .insert(resolver.tree.definitions[name].path.clone());
        }
    }
    loaded.errors.extend(resolver.errors);
    for (name, resolved) in resolver.resolved {
        if let Some(resolved) = resolved {
            loaded.insert(
                resolver.namespace,
                &name,
                &resolver.tree.definitions[&name].path,
                resolved.profile,
            );
        }
    }
}

impl<'a> Resolver<'a> {
    pub(super) fn new(
        home: &'a Path,
        namespace: &'a str,
        tree: &'a Namespace,
        foreign: &'a Namespace,
        bases: &'a BTreeSet<String>,
        skills: SkillCheck<'a>,
        allowed_children: &'a BTreeSet<String>,
    ) -> Self {
        Self {
            home,
            namespace,
            tree,
            foreign,
            bases,
            skills,
            allowed_children,
            resolved: BTreeMap::new(),
            trail: Vec::new(),
            errors: Vec::new(),
        }
    }
}

pub(super) struct Resolver<'a> {
    home: &'a Path,
    namespace: &'a str,
    tree: &'a Namespace,
    foreign: &'a Namespace,
    bases: &'a BTreeSet<String>,
    skills: SkillCheck<'a>,
    allowed_children: &'a BTreeSet<String>,
    resolved: BTreeMap<String, Option<Resolved>>,
    trail: Vec<String>,
    errors: Vec<DefinitionErr>,
}

impl Resolver<'_> {
    fn resolve(&mut self, name: &str) -> Option<Resolved> {
        if self.tree.failed.contains(name) {
            return None;
        }
        if let Some(resolved) = self.resolved.get(name) {
            return resolved.clone();
        }
        if self.trail.iter().any(|entry| entry == name) {
            let mut trail = self.trail.clone();
            trail.push(name.to_owned());
            self.errors.push(DefinitionErr::new(
                &self.tree.definitions[name].path,
                format!("follows itself through `agent:` ({})", trail.join(" -> ")),
            ));
            return None;
        }
        self.trail.push(name.to_owned());
        let result = self.materialize(name);
        self.trail.pop();
        let resolved = match result {
            Ok(resolved) => Some(resolved),
            Err(error) => {
                if !self
                    .errors
                    .iter()
                    .any(|previous| previous.path == error.path)
                {
                    self.errors.push(error);
                }
                None
            }
        };
        self.resolved.insert(name.to_owned(), resolved.clone());
        resolved
    }

    fn materialize(&mut self, name: &str) -> Result<Resolved, DefinitionErr> {
        let definition = &self.tree.definitions[name];
        let path = &definition.path;
        let mut fm = definition.frontmatter.clone();
        let parent_name = fm
            .agent
            .clone()
            .or_else(|| {
                fm.model
                    .as_deref()
                    .and_then(agents::definition_model_kind)
                    .map(str::to_owned)
            })
            .ok_or_else(|| {
                DefinitionErr::new(
                    path,
                    format!(
                        "has no `agent:` and {}; name one",
                        fm.model.as_ref().map_or_else(
                            || "it sets no model".to_owned(),
                            |model| format!("model '{model}' names no runtime")
                        )
                    ),
                )
            })?;
        let mut profile = if let Some(kind) = agents::find_definition(&parent_name) {
            empty_profile(kind.spec().kind)
        } else if self.tree.definitions.contains_key(&parent_name)
            || self.tree.failed.contains(&parent_name)
        {
            let parent = self.resolve(&parent_name).ok_or_else(|| {
                DefinitionErr::new(
                    path,
                    format!("follows `{parent_name}`, which failed to load"),
                )
            })?;
            fm.inherit(&parent.frontmatter);
            let mut profile = empty_profile(&parent.profile.agent);
            profile.append_system_prompt_files = parent.profile.append_system_prompt_files;
            profile
        } else {
            let hint = if self.foreign.definitions.contains_key(&parent_name)
                || self.foreign.failed.contains(&parent_name)
            {
                format!("; rimz resolves `agent:` within {}", self.namespace)
            } else {
                String::new()
            };
            return Err(DefinitionErr::new(
                path,
                format!("follows unknown profile '{parent_name}'{hint}"),
            ));
        };
        let kind = profile.agent.as_str();
        if let Some(definition) = agents::find_definition(kind) {
            for (field, name, value) in [
                (PresetField::Model, "model", &fm.model),
                (PresetField::Effort, "effort", &fm.effort),
                (PresetField::AutoCompact, "auto-compact", &fm.auto_compact),
            ] {
                if value.is_some() && definition.spec().launch.preset_arg_matcher(field).is_none() {
                    return Err(DefinitionErr::new(
                        path,
                        agents::PresetErr::UnsupportedField {
                            agent: definition.spec().kind,
                            field: name,
                        }
                        .to_string(),
                    ));
                }
            }
        }
        if let Some(model) = fm.model.as_deref()
            && let Some(implied) = agents::definition_model_kind(model)
            && implied != kind
        {
            return Err(DefinitionErr::new(
                path,
                format!(
                    "runs on '{kind}' but model '{model}' runs on '{implied}'; set a {kind} model or follow the {implied} base"
                ),
            ));
        }
        if fm.tools.is_none() && agents::tools_required(kind) {
            return Err(DefinitionErr::new(
                path,
                format!("lists no `tools`, which {kind} needs"),
            ));
        }
        let tools = fm
            .tools
            .as_deref()
            .map(ToolSet::parse)
            .transpose()
            .map_err(|error| DefinitionErr::new(path, error.to_string()))?;
        let argv = agents::render_tool_args(kind, tools.as_ref())
            .map_err(|error| DefinitionErr::new(path, error.to_string()))?;
        if !argv.is_empty() {
            profile.args = Some(
                shlex::try_join(argv.iter().map(String::as_str))
                    .map_err(|error| DefinitionErr::new(path, error.to_string()))?,
            );
        }
        if let Some(listed) = fm.subagents.as_ref() {
            if self.namespace == "subagents" {
                return Err(DefinitionErr::new(
                    path,
                    "sets `subagents:`, which only a direct profile takes; a child cannot launch again",
                ));
            }
            if tools.as_ref().is_some_and(|tools| tools.has("Agent")) {
                return Err(DefinitionErr::new(
                    path,
                    "sets `subagents:` and lists the Agent tool; the rimz doorway replaces the native one, so drop whichever this seat does not use",
                ));
            }
            let names = names(path, "subagents", listed)?;
            if let Some(failed) = names.iter().find(|name| {
                (self.foreign.definitions.contains_key(*name)
                    || self.foreign.failed.contains(*name))
                    && !self.allowed_children.contains(*name)
            }) {
                return Err(DefinitionErr::new(
                    path,
                    format!("allows subagent '{failed}', which failed to load"),
                ));
            }
            let unknown: Vec<_> = names
                .iter()
                .filter(|name| {
                    name.as_str() != "general"
                        && agents::find_definition(name).is_none()
                        && !self.allowed_children.contains(*name)
                })
                .collect();
            if !unknown.is_empty() {
                return Err(DefinitionErr::new(
                    path,
                    format!("allows unknown subagent profile(s) {unknown:?}"),
                ));
            }
            profile.subagents = Some(names);
        }
        profile.skills = skill_policy(
            path,
            kind,
            fm.skills.as_deref(),
            tools.as_ref(),
            self.skills,
        )?;
        let defaults = agents::definition_defaults(kind, fm.model.as_deref());
        profile.mode = fm.mode.or(defaults.mode);
        profile.model = fm
            .model
            .as_ref()
            .map(|model| agents::expand_model_alias(kind, model));
        profile.effort = fm.effort.clone().or_else(|| {
            agents::find_definition(kind)
                .and_then(|definition| {
                    definition
                        .spec()
                        .launch
                        .preset_arg_matcher(PresetField::Effort)
                })
                .and(defaults.effort)
                .map(str::to_owned)
        });
        profile.auto_compact = fm
            .auto_compact
            .clone()
            .or_else(|| {
                agents::find_definition(kind)
                    .and_then(|definition| {
                        definition
                            .spec()
                            .launch
                            .preset_arg_matcher(PresetField::AutoCompact)
                    })
                    .map(|_| "258k".to_owned())
            })
            .map(|value| auto_compact(path, &value))
            .transpose()?;
        profile.budget = fm.budget.clone();
        profile.model_reminder = fm.model_reminder;
        profile.description = Some(frontmatter::description(path, fm.description.as_deref())?);
        let craft = traits::render(
            self.home,
            path,
            &definition.body,
            fm.traits.as_deref().unwrap_or_default(),
        )?;
        if !craft.is_empty() {
            profile.append_system_prompt_files.push(PromptSource::Text {
                origin: path.clone(),
                text: craft,
            });
        }
        let replaces_system_prompt = agents::find_definition(kind).is_some_and(|definition| {
            definition
                .spec()
                .launch
                .preset_arg_matcher(PresetField::SystemPromptFile)
                .is_some()
        });
        // A kind that takes a system prompt always runs on its base; any other kind
        // needs one only to carry a craft, which its launch then refuses.
        if (replaces_system_prompt || !profile.append_system_prompt_files.is_empty())
            && !self.bases.contains(kind)
        {
            return Err(DefinitionErr::new(
                path,
                format!("runs on {kind}, whose kind base `agents/{kind}.md` is missing"),
            ));
        }
        Ok(Resolved {
            frontmatter: fm,
            profile,
        })
    }
}

fn names(path: &Path, key: &str, listed: &[String]) -> Result<Vec<String>, DefinitionErr> {
    let mut names = Vec::new();
    for name in listed {
        let name = name.trim();
        if name.is_empty() {
            return Err(DefinitionErr::new(
                path,
                format!("sets `{key}:` to something other than a list of names"),
            ));
        }
        if !names.iter().any(|existing| existing == name) {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

pub(super) fn auto_compact(path: &Path, value: &str) -> Result<String, DefinitionErr> {
    let text = value.trim();
    let (digits, scale) = match text.as_bytes().last() {
        Some(b'k' | b'K') => (&text[..text.len() - 1], 1_000_u64),
        Some(b'm' | b'M') => (&text[..text.len() - 1], 1_000_000_u64),
        _ => (text, 1),
    };
    let count = if digits.bytes().all(|byte| byte.is_ascii_digit()) {
        digits
            .parse::<u64>()
            .ok()
            .and_then(|count| count.checked_mul(scale))
    } else {
        None
    };
    if !count.is_some_and(|count| (100_000..=1_000_000).contains(&count)) {
        return Err(DefinitionErr::new(
            path,
            format!(
                "sets `auto-compact: {value}`; rimz takes a token count from 100k through 1M, spelled '200k', '200000', or '1m'"
            ),
        ));
    }
    Ok(text.to_owned())
}

/// Under [`SkillCheck::Check`], each listed skill resolves as the sandbox view finds it: the kind's provider skill root, then the library. The first root holding `SKILL.md` wins and carries the marker check; only an absent file falls through, so an unreadable copy fails rather than letting a shadowed library copy speak for it.
pub(super) fn skill_policy(
    path: &Path,
    kind: &str,
    listed: Option<&[String]>,
    tools: Option<&ToolSet>,
    check: SkillCheck<'_>,
) -> Result<Option<Vec<SkillName>>, DefinitionErr> {
    let Some(listed) = listed else {
        return Ok(None);
    };
    if agents::tools_required(kind) && !tools.is_some_and(|tools| tools.has("Skill")) {
        return Err(DefinitionErr::new(
            path,
            "sets `skills:` and lists no Skill tool; the list governs which skills that tool reaches on its own",
        ));
    }
    let mut skills = Vec::new();
    for name in names(path, "skills", listed)? {
        let skill: SkillName = name.parse().map_err(|_| DefinitionErr::new(path, format!("lists skill {name:?}; rimz reads one bare directory name, so a `<name>:<mode>` suffix, a path, or whitespace is refused")))?;
        if let SkillCheck::Check { env, library } = check {
            let definition = agents::find_definition(kind);
            let candidates: Vec<PathBuf> = definition
                .and_then(|definition| definition.skills_home(env))
                .into_iter()
                .chain([library.to_path_buf()])
                .map(|root| root.join(&name).join("SKILL.md"))
                .collect();
            let mut found = None;
            for candidate in &candidates {
                match std::fs::read_to_string(candidate) {
                    Ok(text) => {
                        found = Some((candidate, text));
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(DefinitionErr::new(
                            path,
                            format!(
                                "lists skill '{name}', unreadable at {}: {error}",
                                candidate.display()
                            ),
                        ));
                    }
                }
            }
            let Some((skill_path, text)) = found else {
                let searched: Vec<String> = candidates
                    .iter()
                    .map(|candidate| candidate.display().to_string())
                    .collect();
                return Err(DefinitionErr::new(
                    path,
                    format!(
                        "lists skill '{name}', missing at {}",
                        searched.join(" and ")
                    ),
                ));
            };
            let directory = skill_path.parent().unwrap_or(skill_path);
            let marker = definition.map_or(ManualSkill::Unsupported, |definition| {
                definition.manual_skill()
            });
            let manual = match marker {
                ManualSkill::Unsupported => false,
                ManualSkill::Frontmatter => {
                    let (block, _) = frontmatter::split(skill_path, &text)?;
                    block.lines().any(|line| {
                        line.strip_prefix("disable-model-invocation:")
                            .is_some_and(|value| value.trim() == "true")
                    })
                }
                ManualSkill::OpenAiPolicy => {
                    let policy_path = directory.join("agents/openai.yaml");
                    match std::fs::read_to_string(&policy_path) {
                        Ok(text) => {
                            #[derive(serde::Deserialize)]
                            struct Document {
                                policy: Option<Policy>,
                            }
                            #[derive(serde::Deserialize)]
                            struct Policy {
                                allow_implicit_invocation: Option<bool>,
                            }
                            let document: Option<Document> = serde_saphyr::from_str(&text)
                                .map_err(|error| {
                                    DefinitionErr::new(&policy_path, error.to_string())
                                })?;
                            document
                                .and_then(|document| document.policy)
                                .and_then(|policy| policy.allow_implicit_invocation)
                                == Some(false)
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                        Err(error) => {
                            return Err(DefinitionErr::new(&policy_path, error.to_string()));
                        }
                    }
                }
            };
            if manual {
                return Err(DefinitionErr::new(
                    path,
                    format!(
                        "lists skill '{name}', which is marked user-only for {kind}; listing it changes nothing"
                    ),
                ));
            }
        }
        skills.push(skill);
    }
    Ok(Some(skills))
}
