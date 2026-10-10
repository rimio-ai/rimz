//! Chain resolution and profile materialization for both definition namespaces.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::agents::{self, ManualSkill, PresetField, ToolSet};
use crate::config::tiers::{DefinitionRenders, ModelTier, TierPreference, TierProvenance};
use crate::config::{Isolation, Profile, PromptSource, SkillName};

use super::frontmatter::AgentFrontmatter;
use super::{
    DefinitionCause, DefinitionErr, LoadScope, LoadedDefinitions, Namespace, SkillCatalog,
    SkillCheck, frontmatter, traits,
};

#[derive(Clone)]
struct Resolved {
    frontmatter: AgentFrontmatter,
    profile: Profile,
    preference: TierPreference,
    runtime_kind: Option<String>,
}

pub(super) fn empty_profile(kind: &str) -> Profile {
    Profile {
        agent: kind.to_owned(),
        definition_renders: None,
        model_tier: None,
        tier_stamp: None,
        isolation: None,
        skills: None,
        allowed_tools: None,
        description: None,
        subagents: None,
        model_reminder: None,
        keep_warm: None,
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
        scope: LoadScope<'a>,
        namespace: &'a str,
        [tree, foreign]: [&'a Namespace; 2],
        allowed_children: &'a BTreeSet<String>,
    ) -> Self {
        Self {
            scope,
            namespace,
            tree,
            foreign,
            allowed_children,
            resolved: BTreeMap::new(),
            trail: Vec::new(),
            errors: Vec::new(),
        }
    }
}

pub(super) struct Resolver<'a> {
    scope: LoadScope<'a>,
    namespace: &'a str,
    tree: &'a Namespace,
    foreign: &'a Namespace,
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
        if fm.model.is_some() && fm.tier.is_some() {
            return Err(DefinitionErr::new(
                path,
                "sets both `tier:` and `model:`; keep one",
            ));
        }
        if let Some(tier) = fm.model.as_deref().and_then(ModelTier::from_model) {
            return Err(DefinitionErr::new(
                path,
                format!("model is a tier; use `tier: {tier}`"),
            ));
        }
        let own_kind = fm
            .agent
            .as_deref()
            .and_then(agents::find_definition)
            .map(|definition| definition.spec().kind);
        let model_kind = fm.model.as_deref().and_then(agents::definition_model_kind);
        let mut runtime_kind = model_kind.or(own_kind).map(str::to_owned);
        if let (Some(kind), Some(implied)) = (own_kind, model_kind)
            && kind != implied
        {
            return Err(DefinitionErr::new(
                path,
                format!(
                    "runs on '{kind}' but model '{}' runs on '{implied}'; set a {kind} model or follow the {implied} base",
                    fm.model.as_deref().unwrap_or_default()
                ),
            ));
        }
        let own_runtime = fm.model.is_some() || fm.tier.is_some() || own_kind.is_some();
        let mut preference = TierPreference {
            model: fm.model.clone(),
            family: model_kind.or(own_kind).map(str::to_owned),
        };
        let parent_name = fm
            .agent
            .clone()
            .or_else(|| {
                fm.model
                    .as_deref()
                    .and_then(agents::definition_model_kind)
                    .map(str::to_owned)
            })
            .or_else(|| fm.tier.as_ref().map(|_| String::new()))
            .ok_or_else(|| {
                DefinitionErr::new(
                    path,
                    format!(
                        "has no `agent:` and {}; set `agent:`",
                        fm.model.as_ref().map_or_else(
                            || "it sets no model".to_owned(),
                            |model| format!("model '{model}' names no runtime")
                        )
                    ),
                )
            })?;
        let mut profile = if parent_name.is_empty() {
            empty_profile("")
        } else if let Some(kind) = agents::find_definition(&parent_name) {
            empty_profile(kind.spec().kind)
        } else if self.tree.definitions.contains_key(&parent_name)
            || self.tree.failed.contains(&parent_name)
        {
            let parent = self.resolve(&parent_name).ok_or_else(|| {
                DefinitionErr::new(
                    path,
                    format!("follows `{parent_name}`, which failed to load"),
                )
                .with_cause(DefinitionCause::DependsOnFailed {
                    name: parent_name.clone(),
                })
            })?;
            fm.inherit(&parent.frontmatter);
            if runtime_kind.is_none() {
                runtime_kind = parent.runtime_kind;
            }
            if !own_runtime {
                preference = parent.preference;
            }
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
        let unresolved = fm.clone();
        if let Some(shift) = fm
            .effort
            .as_deref()
            .filter(|effort| super::super::tiers::relative_effort(effort))
        {
            let source = if definition.frontmatter.effort.is_some() {
                String::new()
            } else {
                format!(" (inherited from '{}')", self.effort_source(&parent_name))
            };
            return Err(DefinitionErr::new(
                path,
                format!(
                    "effort {shift}{source} is relative; set an absolute effort or omit it for the chosen model's default"
                ),
            ));
        }
        if let Some(kind) = fm.model.as_deref().and_then(agents::definition_model_kind) {
            profile.agent = kind.to_owned();
        } else if fm.model.is_some() {
            profile.agent = runtime_kind.clone().ok_or_else(|| {
                DefinitionErr::new(
                    path,
                    "model names no runtime and the chain has no family; set `agent:`",
                )
            })?;
        }
        let tier = fm
            .tier
            .as_deref()
            .map(str::parse::<ModelTier>)
            .transpose()
            .map_err(|error| DefinitionErr::new(path, error.to_string()))?
            .or_else(|| {
                fm.model
                    .as_deref()
                    .and_then(|model| self.scope.tiers.tier_for_model(&profile.agent, model))
            });
        let mut renders = BTreeMap::new();
        let mut exclusions = BTreeMap::new();
        for (_, kind, model) in self.scope.tiers.entries(ModelTier::Intern) {
            if renders.contains_key(kind) || exclusions.contains_key(kind) {
                continue;
            }
            let mut candidate = profile.clone();
            candidate.agent = kind.to_owned();
            let mut candidate_fm = fm.clone();
            candidate_fm.model = Some(model.to_owned());
            match self.finish(name, candidate_fm, candidate) {
                Ok(rendered) => {
                    renders.insert(kind.to_owned(), rendered);
                }
                Err(error) => {
                    exclusions.insert(kind.to_owned(), error.message);
                }
            }
        }
        let mut profile = if let Some(tier) = tier {
            let pick = self
                .scope
                .tiers
                .walk(
                    tier,
                    &preference,
                    |kind| renders.contains_key(kind),
                    |_, _| None::<()>,
                )
                .map_err(|error| {
                    DefinitionErr::new(
                        path,
                        format!(
                            "{error}; {}",
                            exclusions
                                .iter()
                                .map(|(kind, reason)| format!("{kind}: {reason}"))
                                .collect::<Vec<_>>()
                                .join("; ")
                        ),
                    )
                })?;
            // The walk only selects a family whose complete render succeeded.
            let mut selected = renders[&pick.kind].clone();
            selected.model = Some(pick.model.clone());
            selected.effort = fm.effort.clone().or_else(|| {
                agents::definition_defaults(&pick.kind, Some(&pick.model))
                    .effort
                    .map(str::to_owned)
            });
            selected.model_tier = Some(TierProvenance {
                tier,
                fell_back: pick.used_tier.is_some(),
            });
            selected
        } else {
            self.finish(name, fm.clone(), profile)?
        };
        profile.definition_renders = Some(DefinitionRenders {
            preference: preference.clone(),
            renders,
            exclusions,
            effort: fm.effort,
        });
        Ok(Resolved {
            frontmatter: unresolved,
            profile,
            preference,
            runtime_kind,
        })
    }

    /// The definition along `name`'s `agent:` chain whose own frontmatter sets `effort:`.
    fn effort_source<'n>(&'n self, mut name: &'n str) -> &'n str {
        loop {
            let own = &self.tree.definitions[name].frontmatter;
            match own.agent.as_deref() {
                Some(parent)
                    if own.effort.is_none() && self.tree.definitions.contains_key(parent) =>
                {
                    name = parent;
                }
                _ => return name,
            }
        }
    }

    fn finish(
        &self,
        name: &str,
        fm: AgentFrontmatter,
        mut profile: Profile,
    ) -> Result<Profile, DefinitionErr> {
        let definition = &self.tree.definitions[name];
        let path = &definition.path;
        let kind = profile.agent.as_str();
        profile.allowed_tools = fm
            .allowed_tools
            .as_ref()
            .map(|rules| {
                let mut parsed = Vec::new();
                for rule in rules {
                    let rule = rule
                        .parse::<crate::config::ToolRule>()
                        .map_err(|error| DefinitionErr::new(path, error.to_string()))?;
                    if !parsed.contains(&rule) {
                        parsed.push(rule);
                    }
                }
                Ok::<_, DefinitionErr>(parsed)
            })
            .transpose()?;
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
                )
                .with_cause(DefinitionCause::DependsOnFailed {
                    name: failed.clone(),
                }));
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
        let skill_check = match self.scope.skills.check {
            SkillCheck::Check {
                machine_isolation, ..
            } if (!cfg!(target_os = "linux")
                || Isolation::resolve(None, fm.isolation, machine_isolation)
                    == Isolation::Host)
                && !agents::find_definition(kind).is_some_and(|definition| {
                    matches!(
                        definition.spec().host_skills,
                        agents::skills::HostSkills::Switch { .. }
                    )
                }) =>
            {
                SkillCheck::Skip
            }
            check => check,
        };
        profile.skills = skill_policy(
            path,
            kind,
            fm.skills.as_deref(),
            tools.as_ref(),
            skill_check,
            self.scope.skills,
        )?;
        let defaults = agents::definition_defaults(kind, fm.model.as_deref());
        profile.isolation = fm.isolation;
        profile.mode = fm.mode.or(defaults.mode);
        profile.model = fm.model.clone();
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
                super::super::agents::native_auto_compact(self.scope.native_auto_compact, kind)
            })
            .map(|value| auto_compact(path, &value))
            .transpose()?;
        profile.budget = fm.budget.clone();
        profile.keep_warm = fm
            .keep_warm
            .as_deref()
            .map(|value| super::team::keep_warm(path, value))
            .transpose()?;
        profile.model_reminder = fm.model_reminder;
        profile.description = Some(frontmatter::description(path, fm.description.as_deref())?);
        let craft = traits::render(
            self.scope.home,
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
            && !self.scope.bases.contains(kind)
        {
            return Err(DefinitionErr::new(
                path,
                format!(
                    "runs on {kind}, {}",
                    super::missing_kind_base(self.scope.home, kind)
                ),
            ));
        }
        Ok(profile)
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
    if crate::store::message::AutoCompact::parse_native_window(text).is_err() {
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
    catalog: &SkillCatalog<'_>,
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
    let definition = agents::find_definition(kind);
    let discovered = match check {
        SkillCheck::Check { .. } => catalog
            .enumerate(kind)
            .map_err(|error| DefinitionErr::new(path, error))?,
        SkillCheck::Skip => None,
    };
    for name in names(path, "skills", listed)? {
        let skill: SkillName = name.parse().map_err(|_| DefinitionErr::new(path, format!("lists skill {name:?}; rimz reads one bare directory name, so a `<name>:<mode>` suffix, a path, or whitespace is refused")))?;
        if let SkillCheck::Check { env, library, .. } = check {
            let roots: Vec<PathBuf> = definition
                .and_then(|definition| definition.skills_home(env))
                .into_iter()
                .chain([library.to_path_buf()])
                .collect();
            let Some(found) = discovered
                .as_ref()
                .and_then(|skills| skills.get(skill.as_str()))
            else {
                let searched: Vec<String> = roots
                    .iter()
                    .map(|root| root.join(&name).join("SKILL.md").display().to_string())
                    .collect();
                return Err(DefinitionErr::new(
                    path,
                    format!(
                        "lists skill '{name}', missing at {}",
                        searched.join(" and ")
                    ),
                )
                .with_cause(DefinitionCause::MissingSkill { skill, roots }));
            };
            let skill_path = found.source.join("SKILL.md");
            let text = std::fs::read_to_string(&skill_path).map_err(|error| {
                DefinitionErr::new(
                    path,
                    format!(
                        "lists skill '{name}', unreadable at {}: {error}",
                        skill_path.display()
                    ),
                )
            })?;
            let skill_path = skill_path.as_path();
            let directory = skill_path.parent().unwrap_or(skill_path);
            let marker = definition.map_or(ManualSkill::Unsupported, |definition| {
                definition.manual_skill()
            });
            let manual = match marker {
                ManualSkill::Unsupported => false,
                ManualSkill::Frontmatter => {
                    let (block, _) = frontmatter::split(skill_path, &text).map_err(|error| {
                        DefinitionErr::new(
                            path,
                            format!(
                                "lists skill '{name}', invalid at {}: {}",
                                error.path.display(),
                                error.message
                            ),
                        )
                    })?;
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
                                    DefinitionErr::new(
                                        path,
                                        format!(
                                            "lists skill '{name}', invalid at {}: {error}",
                                            policy_path.display()
                                        ),
                                    )
                                })?;
                            document
                                .and_then(|document| document.policy)
                                .and_then(|policy| policy.allow_implicit_invocation)
                                == Some(false)
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                        Err(error) => {
                            return Err(DefinitionErr::new(
                                path,
                                format!(
                                    "lists skill '{name}', unreadable at {}: {error}",
                                    policy_path.display()
                                ),
                            ));
                        }
                    }
                }
            };
            if manual {
                let (marker_file, marker_line) = if matches!(marker, ManualSkill::OpenAiPolicy) {
                    (
                        directory.join("agents/openai.yaml"),
                        "policy.allow_implicit_invocation: false",
                    )
                } else {
                    (skill_path.to_owned(), "disable-model-invocation: true")
                };
                return Err(DefinitionErr::new(
                    path,
                    format!(
                        "lists skill '{name}', which {} marks user-only for {kind} (`{marker_line}`); listing cannot lift that marker: drop it from `skills:` or remove the marker",
                        marker_file.display()
                    ),
                ));
            }
        }
        skills.push(skill);
    }
    Ok(Some(skills))
}
